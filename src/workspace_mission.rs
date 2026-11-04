//! Native Mission inspector over the shared host controller.
//! A renderer edits plans and reads receipts; it never asserts delivery success.
use crate::{native_shell::InspectorTab, Frost, Message};
use iced::widget::{button, checkbox, column, container, row, scrollable, text, text_editor, text_input};
use iced::{Element, Length, Task};
use jterm_core::agent_workspace::{
    ConversationId, ConversationKind, MissionWorkbench, WorkspaceMission,
    WorkspaceMissionRepositoryReport,
};
use std::collections::HashMap;
use std::path::PathBuf;

pub(crate) struct MissionPanel {
    pub(crate) controller: MissionWorkbench,
    pub(crate) open: bool,
    draft: Option<WorkspaceMission>,
    saved: Option<WorkspaceMission>,
    goal: text_editor::Content,
    checks: HashMap<String, String>,
    only_attention: bool,
    new_open: bool,
    new_title: String,
    new_goal: text_editor::Content,
    new_paths: Vec<(String, String)>,
}

impl Default for MissionPanel {
    fn default() -> Self {
        Self {
            controller: MissionWorkbench::default(),
            open: false,
            draft: None,
            saved: None,
            goal: text_editor::Content::new(),
            checks: HashMap::new(),
            only_attention: false,
            new_open: false,
            new_title: "Four-terminal release".into(),
            new_goal: text_editor::Content::new(),
            new_paths: ["jterm_core", "anvil", "forge", "ember", "frost"]
                .iter()
                .map(|name| (format!("beamiter/{name}"), String::new()))
                .collect(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct MissionRef {
    id: String,
    revision: u64,
}

impl From<&WorkspaceMission> for MissionRef {
    fn from(mission: &WorkspaceMission) -> Self {
        Self {
            id: mission.id.clone(),
            revision: mission.revision,
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) enum MissionEdit {
    Title(String),
    Goal(text_editor::Action),
    Environment(String),
    Path(String, String),
    Target(String, String),
    Checks(String, String),
}

#[derive(Clone, Debug)]
pub(crate) enum MissionAction {
    Open,
    Close,
    Refresh,
    Select(String),
    NewToggle,
    NewTitle(String),
    NewGoal(text_editor::Action),
    NewPath(usize, String),
    Create,
    Edit(MissionRef, MissionEdit),
    Save(MissionRef),
    Reload(MissionRef),
    Inspect(MissionRef),
    Adopt(MissionRef),
    NeedsYou(bool),
    Link(MissionRef, ConversationId),
    OpenTask(ConversationId),
}

impl MissionPanel {
    pub(crate) fn poll(&mut self) {
        self.controller.poll();
        self.sync();
    }

    fn edited_plan(&self) -> Option<WorkspaceMission> {
        let mut plan = self.draft.clone()?;
        for repository in &mut plan.repositories {
            if let Some(checks) = self.checks.get(&repository.identity.full_name) {
                repository.required_checks = checks
                    .split(',')
                    .map(str::trim)
                    .filter(|check| !check.is_empty())
                    .map(str::to_owned)
                    .collect();
            }
        }
        Some(plan)
    }

    fn dirty(&self) -> bool {
        self.edited_plan() != self.saved
    }

    fn replace_draft(&mut self, selected: &WorkspaceMission) {
        self.goal = text_editor::Content::with_text(&selected.goal);
        self.checks = selected
            .repositories
            .iter()
            .map(|repository| {
                (
                    repository.identity.full_name.clone(),
                    repository.required_checks.join(", "),
                )
            })
            .collect();
        self.draft = Some(selected.clone());
    }

    fn sync(&mut self) {
        let Some(selected) = self.controller.selected.clone() else {
            return;
        };
        // Preserve unsaved edits on readback, including an uncertain-write or
        // CAS-conflict readback. Only the matching successful save advances it.
        let matches_saved_edit = self.edited_plan().is_some_and(|mut plan| {
            plan.revision = selected.revision;
            plan.updated_at_ms = selected.updated_at_ms;
            plan.evidence_after_ms = selected.evidence_after_ms;
            plan == selected
        });
        if self.draft.as_ref().is_none_or(|draft| draft.id != selected.id)
            || !self.dirty()
            || matches_saved_edit
        {
            self.replace_draft(&selected);
        }
        self.saved = Some(selected);
    }

    fn matches(&self, reference: &MissionRef) -> bool {
        self.draft
            .as_ref()
            .is_some_and(|draft| MissionRef::from(draft) == *reference)
            && self
                .controller
                .selected
                .as_ref()
                .is_some_and(|selected| selected.id == reference.id)
    }

    fn edit(&mut self, reference: &MissionRef, edit: MissionEdit) {
        if !self.matches(reference) || self.controller.busy {
            return;
        }
        let Some(draft) = &mut self.draft else {
            return;
        };
        match edit {
            MissionEdit::Title(value) => draft.title = value,
            MissionEdit::Environment(value) => draft.required_environment = value,
            MissionEdit::Goal(action) => {
                self.goal.perform(action);
                draft.goal = self.goal.text();
            }
            MissionEdit::Path(name, value) => {
                if let Some(repository) = draft
                    .repositories
                    .iter_mut()
                    .find(|repository| repository.identity.full_name == name)
                {
                    repository.path = value.into();
                }
            }
            MissionEdit::Target(name, value) => {
                if let Some(repository) = draft
                    .repositories
                    .iter_mut()
                    .find(|repository| repository.identity.full_name == name)
                {
                    repository.target_ref = value;
                }
            }
            MissionEdit::Checks(name, value) => {
                if draft
                    .repositories
                    .iter()
                    .any(|repository| repository.identity.full_name == name)
                {
                    self.checks.insert(name, value);
                }
            }
        }
    }
}

impl Frost {
    pub(crate) fn workspace_mission_action(&mut self, action: MissionAction) -> Task<Message> {
        if matches!(action, MissionAction::Open) {
            self.agent_sessions.missions.open = true;
            self.agent_sessions.missions.only_attention =
                self.agent_sessions.status == Some(jterm_core::agent_sessions::SessionStatus::Waiting);
            self.agent_sessions.missions.controller.refresh();
            self.agent_sessions.context_input_focused = false;
            self.reference_navigation_drawer = false;
            self.reference_inspector_tab = InspectorTab::Context;
            self.reference_inspector_open = true;
            self.reference_inspector_drawer = self.reference_layout().inspector.is_none();
            self.sync_reference_inspector_geometry();
            self.native_enter_ownership.lock().claim_held();
            return Task::none();
        }
        if !self.agent_sessions.missions.open {
            return Task::none();
        }
        if let MissionAction::OpenTask(id) = &action {
            if let Some((task, _)) = self
                .workspace_task_rows()
                .into_iter()
                .find(|(_, row)| row.id == *id)
            {
                self.agent_sessions.missions.open = false;
                self.agent_sessions.context_input_focused = false;
                return self.update(Message::WorkspaceTask(task));
            }
            self.agent_sessions.missions.controller.notice =
                Some("This linked task is not in the current host catalog. Refresh its task view before opening it.".into());
            return Task::none();
        }
        let panel = &mut self.agent_sessions.missions;
        let ready = !panel.controller.busy;
        match action {
            MissionAction::Close => {
                panel.open = false;
                self.agent_sessions.context_input_focused = false;
            }
            MissionAction::Refresh => panel.controller.refresh(),
            MissionAction::Select(id) if ready && !panel.dirty() => {
                panel.controller.select(&id);
                panel.sync();
                self.agent_sessions.context_input_focused = false;
            }
            MissionAction::NewToggle => panel.new_open = !panel.new_open,
            MissionAction::NewTitle(value) if ready && panel.new_open => panel.new_title = value,
            MissionAction::NewGoal(action) if ready && panel.new_open => panel.new_goal.perform(action),
            MissionAction::NewPath(index, value) if ready && panel.new_open => {
                if let Some((_, path)) = panel.new_paths.get_mut(index) {
                    *path = value;
                }
            }
            MissionAction::Create if ready && panel.new_open && !panel.dirty() => {
                let paths: Vec<_> = panel
                    .new_paths
                    .iter()
                    .map(|(name, path)| (name.clone(), PathBuf::from(path)))
                    .collect();
                panel.controller.create(&panel.new_title, &panel.new_goal.text(), &paths);
                panel.sync();
            }
            MissionAction::Edit(reference, edit) => panel.edit(&reference, edit),
            MissionAction::Save(reference) if ready && panel.matches(&reference) => {
                if let Some(plan) = panel.edited_plan() {
                    panel.controller.save(plan);
                }
            }
            MissionAction::Reload(reference) if ready && panel.matches(&reference) => {
                if let Some(selected) = panel.controller.selected.clone() {
                    panel.replace_draft(&selected);
                    panel.saved = Some(selected);
                }
            }
            MissionAction::Inspect(reference)
                if ready && !panel.dirty() && panel.matches(&reference) =>
            {
                panel.controller.inspect();
            }
            MissionAction::Adopt(reference)
                if ready && !panel.dirty() && panel.matches(&reference) =>
            {
                panel.controller.adopt_candidates();
            }
            MissionAction::NeedsYou(value) => panel.only_attention = value,
            MissionAction::Link(reference, id)
                if ready && !panel.dirty() && panel.matches(&reference) =>
            {
                panel.controller.link_conversation(&id);
            }
            _ => {}
        }
        Task::none()
    }

    pub(crate) fn workspace_mission_view(&self) -> Element<'_, Message> {
        let size = self.chrome_font_size();
        let panel = &self.agent_sessions.missions;
        let ready = !panel.controller.busy;
        let dirty = panel.dirty();
        let action = Message::WorkspaceMission;
        let mut body = column![
            text("Missions").size(size + 2.0),
            text("Acceptance goals, exact versions and delivery receipts").size(size - 1.0),
            row![
                button(text("Refresh").size(size)).on_press(action(MissionAction::Refresh)),
                button(text("Back").size(size)).on_press(action(MissionAction::Close)),
                button(text("New Mission").size(size)).on_press(action(MissionAction::NewToggle)),
            ].spacing(6).wrap(),
            checkbox(panel.only_attention).label("Needs you: unfinished repositories")
                .size(size).on_toggle(move |value| action(MissionAction::NeedsYou(value))),
        ].spacing(10);
        if panel.controller.busy {
            body = body.push(text("Reading the host…").size(size).style(text::secondary));
        }
        if let Some(notice) = &panel.controller.notice {
            body = body.push(text(visible(notice)).size(size).style(text::danger));
        }
        for mission in &panel.controller.missions {
            body = body.push(
                button(text(visible(&mission.title)).size(size))
                    .on_press_maybe((ready && !dirty).then(|| action(MissionAction::Select(mission.id.clone()))))
                    .style(if panel.draft.as_ref().is_some_and(|draft| draft.id == mission.id) {
                        button::primary
                    } else {
                        button::text
                    }),
            );
        }
        if panel.new_open {
            let mut form = column![
                text("New five-repository Mission").size(size + 1.0),
                input("Title", &panel.new_title, size, ready, move |value| action(MissionAction::NewTitle(value))),
                text("Acceptance goal").size(size - 1.0),
            ].spacing(7);
            let mut editor = text_editor(&panel.new_goal).size(size).height(size * 4.5 + 24.0).padding(10);
            if ready {
                editor = editor.on_action(move |value| action(MissionAction::NewGoal(value)));
            }
            form = form.push(editor);
            for (index, (name, path)) in panel.new_paths.iter().enumerate() {
                form = form.push(input(name, path, size, ready, move |value| {
                    action(MissionAction::NewPath(index, value))
                }));
            }
            form = form.push(text("Enter each absolute checkout or task worktree path.").size(size - 1.0))
                .push(button(text("Create Mission").size(size))
                    .on_press_maybe((ready && !dirty).then_some(action(MissionAction::Create))));
            body = body.push(form);
        }
        let Some(draft) = &panel.draft else {
            return container(scrollable(body.push(text("Select a saved plan or create a Mission.").size(size))))
                .padding(12).height(Length::Fill).into();
        };
        let reference = MissionRef::from(draft);
        let edit_ref = reference.clone();
        body = body.push(text(format!("Plan revision {}", draft.revision)).size(size - 1.0))
            .push(input("Title", &draft.title, size, ready, move |value| {
                action(MissionAction::Edit(edit_ref.clone(), MissionEdit::Title(value)))
            }))
            .push(text("Acceptance goal").size(size - 1.0));
        let goal_ref = reference.clone();
        let mut editor = text_editor(&panel.goal).size(size).height(size * 4.5 + 24.0).padding(10);
        if ready {
            editor = editor.on_action(move |value| action(MissionAction::Edit(goal_ref.clone(), MissionEdit::Goal(value))));
        }
        body = body.push(editor);
        let environment_ref = reference.clone();
        body = body.push(input("Required environment", &draft.required_environment, size, ready, move |value| {
            action(MissionAction::Edit(environment_ref.clone(), MissionEdit::Environment(value)))
        })).push(text("Local validation is recorded as local. Wayland, GPU and remote coverage require matching evidence.")
            .size(size - 1.0).style(text::secondary));
        if dirty {
            body = body.push(text("Unsaved edits: save or reload before selecting another plan or inspecting delivery.")
                .size(size).style(text::warning));
        }
        if panel.saved.as_ref().is_some_and(|saved| saved.revision != draft.revision) {
            body = body.push(text("The shared revision changed. Your draft is retained; compare it before reloading.")
                .size(size).style(text::warning));
        }
        body = body.push(row![
            button(text("Save plan").size(size)).on_press_maybe((ready && dirty).then(|| action(MissionAction::Save(reference.clone())))),
            button(text("Reload saved").size(size)).on_press_maybe((ready && dirty).then(|| action(MissionAction::Reload(reference.clone())))),
            button(text("Inspect delivery").size(size)).on_press_maybe((ready && !dirty).then(|| action(MissionAction::Inspect(reference.clone())))),
            button(text("Use observed commits").size(size)).on_press_maybe((ready && !dirty && panel.controller.report.is_some()).then(|| action(MissionAction::Adopt(reference.clone())))),
        ].spacing(6).wrap());
        if let Some(selected) = self.workspace_selected_row().filter(|row| row.kind == ConversationKind::Task) {
            body = body.push(button(text("Link selected task run").size(size))
                .on_press_maybe((ready && !dirty).then(|| action(MissionAction::Link(reference.clone(), selected.id)))));
        }
        let report = panel.controller.report.as_ref().filter(|report| report.id == draft.id && report.revision == draft.revision && !dirty);
        if let Some(report) = report {
            body = body.push(text(report.state.label()).size(size + 1.0));
            for error in &report.errors {
                body = body.push(text(visible(error)).size(size).style(text::danger));
            }
        } else {
            body = body.push(text("Delivery has not been inspected for this plan revision.").size(size));
        }
        let task_rows = self.workspace_task_rows();
        for repository in &draft.repositories {
            let observed = report.and_then(|report| report.repositories.iter().find(|entry| entry.identity.matches(&repository.identity)));
            if panel.only_attention && observed.is_some_and(WorkspaceMissionRepositoryReport::complete) {
                continue;
            }
            let name = repository.identity.full_name.clone();
            let path_ref = reference.clone();
            let path_name = name.clone();
            let target_ref = reference.clone();
            let target_name = name.clone();
            let checks_ref = reference.clone();
            let checks_name = name.clone();
            let path = repository.path.to_string_lossy().into_owned();
            let mut section = column![
                text(visible(&name)).size(size + 1.0),
                input_owned("Checkout / task worktree", path, size, ready, move |value| action(MissionAction::Edit(path_ref.clone(), MissionEdit::Path(path_name.clone(), value)))),
                input("Target branch ref", &repository.target_ref, size, ready, move |value| action(MissionAction::Edit(target_ref.clone(), MissionEdit::Target(target_name.clone(), value)))),
                input("Required checks (comma-separated)", panel.checks.get(&name).map_or("", String::as_str), size, ready, move |value| action(MissionAction::Edit(checks_ref.clone(), MissionEdit::Checks(checks_name.clone(), value)))),
                detail("Baseline", &repository.baseline, size),
                detail("Candidate", repository.candidate.as_deref().unwrap_or("Not selected"), size),
                detail("Required core pin", repository.required_core_pin.as_deref().unwrap_or("Not required"), size),
            ].spacing(7);
            if let Some(receipt) = observed {
                section = section.push(receipt_view(receipt, size));
            } else {
                section = section.push(text("No receipt for this revision.").size(size));
            }
            for linked in &repository.tasks {
                let task = task_rows.iter().find(|(_, row)| row.id == linked.conversation);
                let label = task.map(|(_, row)| format!("Execution: {} · {}", visible(&row.title), row.presentation().status_label))
                    .unwrap_or_else(|| format!("Execution: task {} · catalog unavailable", linked.task_id));
                section = section.push(text(label).size(size - 1.0))
                    .push(detail("Linked run", &linked.run_id, size))
                    .push(button(text("Open linked task").size(size))
                        .on_press_maybe(task.is_some().then(|| action(MissionAction::OpenTask(linked.conversation.clone())))));
            }
            body = body.push(container(section).padding(10).style(container::bordered_box));
        }
        body = body.push(text("Receipts describe the last host observation. Inspect again after new commits, remote updates or validation. Task execution, acceptance and publication remain separate.")
            .size(size - 1.0).style(text::secondary));
        crate::widget_identity_scope::scope_key(
            ("mission-inspector", draft.id.clone()),
            container(scrollable(body).height(Length::Fill)).padding(12).height(Length::Fill),
        )
    }
}

fn visible(value: &str) -> String {
    crate::review_text::visible_bounded(value, 4096)
}

fn input<'a>(label: &str, value: &str, size: f32, enabled: bool, callback: impl Fn(String) -> Message + 'a) -> Element<'a, Message> {
    input_owned(label, value.to_owned(), size, enabled, callback)
}

fn input_owned<'a>(label: &str, value: String, size: f32, enabled: bool, callback: impl Fn(String) -> Message + 'a) -> Element<'a, Message> {
    let mut control = text_input("", &value).size(size);
    if enabled {
        control = control.on_input(callback);
    }
    column![text(label.to_owned()).size(size - 1.0), control].spacing(3).into()
}

fn detail(label: &str, value: &str, size: f32) -> Element<'static, Message> {
    text(format!("{label}: {}", visible(value))).size(size - 1.0).into()
}

fn receipt_view(receipt: &WorkspaceMissionRepositoryReport, size: f32) -> Element<'static, Message> {
    let observed = receipt.observed.is_some();
    let stage = |label, passed| detail(label, if passed { "Confirmed" } else { "Unconfirmed" }, size);
    let mut body = column![
        detail("Changed files", if !observed { "Not inspected" } else if receipt.modified { "Changes observed" } else { "No changes observed" }, size),
        stage("Candidate committed", receipt.committed),
        stage("Remote candidate receipt", receipt.pushed),
        stage("Merged into target ref", receipt.integrated),
        stage("This candidate validated", receipt.validated),
        stage("Core dependency pins", receipt.pins_current),
    ].spacing(4);
    if let Some(local) = &receipt.observed {
        body = body.push(detail("Observed HEAD", &local.head, size))
            .push(detail("Fingerprint", &local.fingerprint, size))
            .push(detail("Working tree", if local.dirty { "Dirty input" } else { "Clean" }, size))
            .push(detail("Observed at (Unix ms)", &local.observed_at_ms.to_string(), size));
    }
    for check in &receipt.checks {
        body = body.push(detail(&format!("Validation · {} · {}", visible(&check.check_name), visible(&check.environment)), check.status.label(), size));
        if let Some(reason) = &check.detail {
            body = body.push(text(visible(reason)).size(size - 1.0));
        }
    }
    for error in &receipt.errors {
        body = body.push(text(visible(error)).size(size - 1.0).style(text::danger));
    }
    body.into()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plan() -> WorkspaceMission {
        let mut plan = WorkspaceMission::new("Release", "Validate five exact commits");
        plan.revision = 1;
        plan
    }

    #[test]
    fn conflicting_readback_preserves_the_local_draft_and_its_revision() {
        let mut panel = MissionPanel::default();
        let initial = plan();
        panel.controller.selected = Some(initial.clone());
        panel.sync();
        let reference = MissionRef::from(&initial);
        panel.edit(&reference, MissionEdit::Title("My unsaved plan".into()));
        let mut remote = initial;
        remote.revision += 1;
        remote.title = "Another window's plan".into();
        panel.controller.selected = Some(remote.clone());
        panel.sync();
        assert_eq!(panel.draft.as_ref().unwrap().title, "My unsaved plan");
        assert_eq!(panel.draft.as_ref().unwrap().revision, 1);
        assert_eq!(panel.saved, Some(remote));
        assert!(panel.dirty());
    }

    #[test]
    fn successful_save_advances_the_draft_and_retires_old_callbacks() {
        let mut panel = MissionPanel::default();
        let initial = plan();
        panel.controller.selected = Some(initial.clone());
        panel.sync();
        let old = MissionRef::from(&initial);
        panel.edit(&old, MissionEdit::Title("Accepted plan".into()));
        let mut saved = panel.edited_plan().unwrap();
        saved.revision += 1;
        saved.updated_at_ms = 123;
        saved.evidence_after_ms = 123;
        panel.controller.selected = Some(saved.clone());
        panel.sync();
        assert_eq!(panel.draft, Some(saved));
        assert!(!panel.dirty());
        panel.edit(&old, MissionEdit::Title("Late callback".into()));
        assert_eq!(panel.draft.as_ref().unwrap().title, "Accepted plan");
    }

    #[test]
    fn callback_from_previous_selection_cannot_edit_the_new_mission() {
        let mut panel = MissionPanel::default();
        let initial = plan();
        panel.controller.selected = Some(initial.clone());
        panel.sync();
        let reference = MissionRef::from(&initial);
        let replacement = plan();
        panel.controller.selected = Some(replacement.clone());
        panel.sync();
        panel.edit(&reference, MissionEdit::Title("Wrong target".into()));
        assert_eq!(panel.draft, Some(replacement));
    }
}
