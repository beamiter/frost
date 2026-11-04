//! Native Changes / Context / Run. Every callback carries its source identity.
use crate::{native_shell::InspectorTab, workspace_context::ContextField, Frost, Message};
use iced::widget::{button, checkbox, column, container, row, scrollable, text, text_editor};
use iced::{Element, Length};
use jterm_core::agent_workspace::{
    ConversationId, ConversationKind, MaterialKind, WorkspaceContext, WorkspaceMaterial,
    WorkspaceOverview, WorkspaceRow, WorkspaceTextDocument,
};
use std::collections::HashMap;

#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub(crate) enum ContextInputKey {
    Field(ContextField),
    Material(String),
}

#[derive(Default)]
pub(crate) struct ContextInputs {
    conversation: Option<ConversationId>,
    contents: HashMap<ContextInputKey, text_editor::Content>,
}
impl ContextInputs {
    pub(crate) fn sync(&mut self, draft: &WorkspaceContext) {
        if self.conversation.as_ref() != Some(&draft.conversation) {
            self.conversation = Some(draft.conversation.clone());
            self.contents.clear();
        }
        let mut values = vec![
            (ContextInputKey::Field(ContextField::Goal), &draft.goal),
            (
                ContextInputKey::Field(ContextField::Completed),
                &draft.completed,
            ),
            (
                ContextInputKey::Field(ContextField::Remaining),
                &draft.remaining,
            ),
            (
                ContextInputKey::Field(ContextField::Constraints),
                &draft.constraints,
            ),
        ];
        values.extend(
            draft
                .materials
                .iter()
                .map(|item| (ContextInputKey::Material(item.id.clone()), &item.text)),
        );
        self.contents
            .retain(|key, _| values.iter().any(|(current, _)| current == key));
        for (key, value) in values {
            if self
                .contents
                .get(&key)
                .is_none_or(|content| content.text() != *value)
            {
                self.contents
                    .insert(key, text_editor::Content::with_text(value));
            }
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) enum ContextAction {
    Save,
    Refresh,
    AddNote,
    Remove(String),
    KeepLocal,
    UseShared,
    Build(String),
    Create { revision: u64, provider: String },
    AddEvidence(WorkspaceTextDocument),
}
#[derive(Clone, Debug)]
pub(crate) enum ReviewAction {
    Refresh,
    QueueAdd,
    QueueRemove,
    Dependency(ConversationId, bool),
    AddCapture {
        sequence: u64,
        run: String,
        diff: bool,
    },
}

fn field(label: &str, value: &str, size: f32) -> Element<'static, Message> {
    column![
        text(label.to_owned())
            .size(size - 2.0)
            .style(text::secondary),
        text(crate::review_text::visible_bounded(value, 4096)).size(size)
    ]
    .spacing(3)
    .into()
}

impl Frost {
    pub(crate) fn sync_workspace_inspector(&mut self) {
        self.agent_sessions.review.start();
        let tasks: Vec<_> = self
            .workspace_task_rows()
            .into_iter()
            .map(|(_, row)| row)
            .collect();
        self.agent_sessions.review.index_tasks(&tasks);
        if let Some(row) = self.workspace_selected_row() {
            if row.kind == ConversationKind::Task
                && row.facts.source_sequence.is_none()
                && !self.agent_sessions.review.task_indexed(&row.id)
            {
                self.agent_sessions.context.deselect();
                self.agent_sessions.review.clear_task();
                return;
            }
            self.agent_sessions.context.select(row.id.clone());
            if row.kind == ConversationKind::Task {
                self.agent_sessions.review.select_task(row.id);
            } else {
                self.agent_sessions.review.clear_task();
            }
        } else {
            self.agent_sessions.context.deselect();
            self.agent_sessions.review.clear_task();
        }
        if let Some(draft) = self.agent_sessions.context.draft() {
            self.agent_sessions.context_inputs.sync(draft);
        }
    }
    pub(crate) fn workspace_context_edit(
        &mut self,
        source: ConversationId,
        key: ContextInputKey,
        action: text_editor::Action,
    ) {
        if self.workspace_selected_row().as_ref().map(|row| &row.id) != Some(&source)
            || self.agent_sessions.context.selected() != Some(&source)
            || self.agent_sessions.context_inputs.conversation.as_ref() != Some(&source)
        {
            return;
        }
        if matches!(&action, text_editor::Action::Edit(text_editor::Edit::Paste(value)) if value.len() > 256 * 1024)
        {
            self.agent_sessions.context.notice =
                Some("That paste is too large; your previous draft is retained.".into());
            return;
        }
        let Some(content) = self.agent_sessions.context_inputs.contents.get_mut(&key) else {
            return;
        };
        let before = content.text();
        content.perform(action);
        let after = content.text();
        if before == after {
            return;
        }
        match key {
            ContextInputKey::Field(field) => self.agent_sessions.context.set_field(field, after),
            ContextInputKey::Material(id) => self.agent_sessions.context.set_material(&id, after),
        }
    }
    pub(crate) fn workspace_context_action(
        &mut self,
        source: ConversationId,
        action: ContextAction,
    ) {
        if self.workspace_selected_row().as_ref().map(|row| &row.id) != Some(&source)
            || self.agent_sessions.context.selected() != Some(&source)
        {
            return;
        }
        match action {
            ContextAction::Save => self.agent_sessions.context.save(),
            ContextAction::Refresh => self.agent_sessions.context.reload(),
            ContextAction::AddNote => self.agent_sessions.context.add_note(),
            ContextAction::Remove(id) => self.agent_sessions.context.remove_material(&id),
            ContextAction::KeepLocal => self.agent_sessions.context.keep_local(),
            ContextAction::UseShared => self.agent_sessions.context.use_shared(),
            ContextAction::Build(provider) => self.agent_sessions.context.build_handoff(&provider),
            ContextAction::AddEvidence(document) => {
                if self
                    .agent_sessions
                    .tools
                    .document
                    .as_ref()
                    .is_some_and(|current| {
                        current.conversation == document.conversation
                            && current.event_id == document.event_id
                            && current.revision == document.revision
                    })
                {
                    self.agent_sessions.context.add_evidence(&document);
                }
            }
            ContextAction::Create { revision, provider } => {
                if !self.agent_sessions.context.packet_current() {
                    return;
                }
                let Some(packet) = self.agent_sessions.context.packet.clone().filter(|packet| {
                    packet.source == source
                        && packet.source_revision == revision
                        && packet.target_provider == provider
                }) else {
                    return;
                };
                let outcome = if provider == "api" {
                    self.ai_chats.open(&self.config);
                    self.ai_chats
                        .workspace
                        .create_with_draft(packet.title.clone(), packet.body.clone());
                    self.agent_sessions.workspace_task = None;
                    self.reference_navigation_drawer = false;
                    self.reference_inspector_drawer = false;
                    Ok(())
                } else {
                    self.open_task_handoff(&packet)
                };
                self.agent_sessions.context.notice = Some(match outcome {
                    Ok(()) => {
                        "New handoff draft prepared. Review it before creating or sending.".into()
                    }
                    Err(error) => error,
                });
            }
        }
        if let Some(draft) = self.agent_sessions.context.draft() {
            self.agent_sessions.context_inputs.sync(draft);
        }
    }
    pub(crate) fn workspace_review_action(&mut self, source: ConversationId, action: ReviewAction) {
        if self
            .workspace_selected_row()
            .as_ref()
            .is_none_or(|row| row.id != source || row.kind != ConversationKind::Task)
            || self.agent_sessions.review.selected() != Some(&source)
        {
            return;
        }
        match action {
            ReviewAction::Refresh => self.agent_sessions.review.refresh_result(),
            ReviewAction::QueueAdd => self.agent_sessions.review.set_queue(source, Vec::new()),
            ReviewAction::QueueRemove => self.agent_sessions.review.remove_queue(source),
            ReviewAction::Dependency(id, enabled) => self
                .agent_sessions
                .review
                .set_dependency(source, id, enabled),
            ReviewAction::AddCapture {
                sequence,
                run,
                diff,
            } => {
                if self.agent_sessions.context.selected() != Some(&source) {
                    return;
                }
                let Some(report) = self.agent_sessions.review.report.as_ref().filter(|report| {
                    report.conversation == source
                        && report.source_sequence == sequence
                        && report.run_id == run
                }) else {
                    return;
                };
                let material = if diff {
                    let Some(current) = &report.current else {
                        return;
                    };
                    WorkspaceMaterial {
                        id: uuid::Uuid::new_v4().to_string(),
                        label: format!("Diff · run {run} · event {sequence}"),
                        kind: MaterialKind::Diff,
                        text: format!(
                            "Captured HEAD: {}\nFingerprint: {}\n{}\n\n{}",
                            current.head, current.fingerprint, current.attribution, current.diff
                        ),
                        source: None,
                        truncated: false,
                    }
                } else {
                    WorkspaceMaterial {
                        id: uuid::Uuid::new_v4().to_string(),
                        label: format!("Validation · run {run} · event {sequence}"),
                        kind: MaterialKind::Log,
                        text: report.validation_output.clone(),
                        source: None,
                        truncated: report.validation_output_truncated,
                    }
                };
                self.agent_sessions.context.add_material(material);
            }
        }
    }
    pub(crate) fn workspace_context_owns_keys(&self) -> bool {
        self.agent_sessions.context_input_focused
            && self.reference_inspector_visible()
            && self.reference_inspector_tab == InspectorTab::Context
            && self.workspace_inspector_active()
            && (self.agent_sessions.missions.open
                || self.agent_sessions.tools.selected_hit.is_none())
    }
    pub(crate) fn workspace_inspector_active(&self) -> bool {
        self.agent_sessions.missions.open
            || self.workspace_has_conversation()
            || self.agent_sessions.tools.selected_hit.is_some()
    }
    pub(crate) fn workspace_inspector(&self, tab: InspectorTab) -> Element<'_, Message> {
        if self.agent_sessions.missions.open && tab == InspectorTab::Context {
            return self.workspace_mission_view();
        }
        let size = self.chrome_font_size();
        if tab == InspectorTab::Context && self.agent_sessions.tools.selected_hit.is_some() {
            return self.workspace_evidence();
        }
        let Some(row) = self.workspace_selected_row() else {
            return container(scrollable(
                column![
                    self.workspace_overview(),
                    text("Select a conversation to inspect changes, context and run.").size(size)
                ]
                .spacing(12),
            ))
            .padding(12)
            .height(Length::Fill)
            .into();
        };
        if row.kind == ConversationKind::Task
            && row.facts.source_sequence.is_none()
            && !self.agent_sessions.review.task_indexed(&row.id)
        {
            let mut pending = column![
                text(crate::review_text::visible_bounded(&row.title, 180)).size(size + 1.0),
                text("Registering this task in the shared workspace…").size(size),
            ]
            .spacing(12);
            if let Some(notice) = &self.agent_sessions.review.notice {
                pending = pending.push(text(notice).size(size));
            }
            return container(pending).padding(12).height(Length::Fill).into();
        }
        let presentation = row.presentation();
        let mut body = column![
            text(crate::review_text::visible_bounded(&row.title, 180)).size(size + 1.0),
            text(format!("{} · {}", row.provider, presentation.status_label))
                .size(size - 1.0)
                .style(text::secondary)
        ]
        .spacing(10);
        let task = self
            .agent_sessions
            .workspace_task
            .and_then(|id| self.task_manager.get(id));
        match tab {
            InspectorTab::Changes => {
                if let Some(task) = task {
                    body = body
                        .push(field("Task baseline", &task.base_commit, size))
                        .push(field(
                            "Worktree",
                            &task.worktree_path.to_string_lossy(),
                            size,
                        ))
                        .push(
                            button(text("Review task diff").size(size))
                                .on_press(Message::TaskDiffOpen(task.id)),
                        )
                        .push(self.workspace_result(&row, true));
                    if let Some(view) = self.agent_runtime.shared_snapshot(task.id) {
                        if view.file_changes.is_empty() {
                            body = body.push(
                                text("No structured file events in the retained turn.")
                                    .size(size - 1.0),
                            );
                        }
                        for item in &view.file_changes {
                            for change in &item.changes {
                                body = body.push(field(
                                    &format!("{} · {}", change.kind, item.status),
                                    &change.path,
                                    size,
                                ));
                                if !change.path_exact {
                                    body = body.push(
                                        text("Provider path was shortened.").size(size - 1.0),
                                    );
                                }
                            }
                            if item.changes_truncated {
                                body = body.push(
                                    text("Additional events were omitted by the retention limit.")
                                        .size(size - 1.0),
                                );
                            }
                        }
                    }
                } else {
                    body = body.push(text("This conversation has no structured task diff. CLI output and API text do not establish file ownership.").size(size));
                }
            }
            InspectorTab::Context => {
                body = body.push(field("Project / working directory", &row.cwd, size));
                if let Some(branch) = &row.facts.branch {
                    body = body.push(field("Branch", branch, size));
                }
                if let Some(task) = task {
                    let inspector = jterm_core::agent_task::TaskInspector::from_task(task);
                    if let Some(goal) = inspector.goal {
                        body = body.push(field("Task goal", &goal, size));
                    }
                    for item in inspector.fields.iter().filter(|item| {
                        matches!(
                            item.label,
                            "Origin"
                                | "Isolated worktree"
                                | "Review baseline"
                                | "Source session"
                                | "Source execution"
                                | "Validation command"
                        )
                    }) {
                        body = body.push(field(item.label, &item.value, size));
                    }
                }
                body = body.push(self.workspace_context(&row));
            }
            _ => {
                body = body.push(field("Execution", presentation.runtime_label, size));
                if let Some(state) = presentation.task_label {
                    body = body.push(field("Task", state, size));
                }
                if let Some(state) = presentation.validation_label {
                    body = body.push(field("Validation", state, size));
                }
                if let Some(detail) = presentation.detail {
                    body = body.push(field("Latest evidence", &detail, size));
                }
                if let Some(recorded) = row.facts.recorded_status {
                    body = body
                        .push(field(
                            "Last recorded provider event",
                            recorded.label(),
                            size,
                        ))
                        .push(
                            text("Recorded history is not a live permission request.")
                                .size(size - 1.0)
                                .style(text::secondary),
                        );
                }
                if let Some(sequence) = row.facts.source_sequence {
                    body = body.push(field("Host event sequence", &sequence.to_string(), size));
                }
                if let Some(task) = task {
                    if let Some(view) = self.agent_runtime.shared_snapshot(task.id) {
                        body = body
                            .push(field("Provider session", view.phase.label(), size))
                            .push(field(
                                "Reported usage",
                                &view
                                    .usage
                                    .as_ref()
                                    .map(|usage| usage.summary())
                                    .unwrap_or_else(|| {
                                        "Unavailable from this provider adapter".into()
                                    }),
                                size,
                            ));
                        if let Some(error) = &view.last_error {
                            body = body.push(field("Provider error", error, size));
                        }
                        if let Some(approval) = view.pending_approvals.first() {
                            body = body
                                .push(
                                    text(format!(
                                        "{} pending request(s)",
                                        view.pending_approvals.len()
                                    ))
                                    .size(size),
                                )
                                .push(button(text("View current request").size(size)).on_press(
                                    Message::WorkspaceTaskAttention(task.id, approval.id),
                                ));
                        }
                    } else {
                        body = body.push(text("Provider usage is unavailable.").size(size));
                    }
                    body = body
                        .push(self.workspace_result(&row, false))
                        .push(self.workspace_queue(&row));
                }
                if !presentation.capabilities.known {
                    body = body.push(
                        text("Adapter capabilities have not been reported by the host.")
                            .size(size - 1.0),
                    );
                } else if presentation.capabilities.terminal {
                    body = body.push(
                        text("The provider TUI owns input; there is no second composer.")
                            .size(size - 1.0),
                    );
                }
                body = body.push(text("The host owns background work while alive. A host crash leaves unconfirmed work for explicit recovery; opening a view never replays it.").size(size - 1.0).style(text::secondary))
                    .push(self.workspace_overview());
            }
        }
        let key = format!("workspace-inspector-{}-{tab:?}", row.id);
        crate::widget_identity_scope::scope_key(
            key.clone(),
            container(
                scrollable(body)
                    .id(iced::widget::Id::from(key))
                    .height(Length::Fill),
            )
            .padding(12)
            .height(Length::Fill),
        )
    }

    fn context_input(
        &self,
        source: &ConversationId,
        key: ContextInputKey,
        label: &str,
    ) -> Element<'_, Message> {
        let size = self.chrome_font_size();
        let Some(content) = self
            .agent_sessions
            .context_inputs
            .contents
            .get(&key)
            .filter(|_| self.agent_sessions.context_inputs.conversation.as_ref() == Some(source))
        else {
            return text("Loading editor…").size(size).into();
        };
        let id = source.clone();
        column![
            text(label.to_owned())
                .size(size - 1.0)
                .style(text::secondary),
            text_editor(content)
                .size(size)
                .height((size * 1.5 * 4.0).max(90.0))
                .padding(iced::Padding {
                    top: 8.0,
                    right: 8.0,
                    bottom: size * 1.5,
                    left: 8.0
                })
                .on_action(move |action| Message::WorkspaceContextEdit(
                    id.clone(),
                    key.clone(),
                    action
                ))
        ]
        .spacing(4)
        .into()
    }
    fn workspace_context(&self, source: &WorkspaceRow) -> Element<'_, Message> {
        let size = self.chrome_font_size();
        let context = &self.agent_sessions.context;
        let action = |action| Message::WorkspaceContext(source.id.clone(), action);
        let mut body = column![text("Handoff context").size(size + 1.0),
            text("Only reviewed fields and materials enter a new handoff draft. Reading history does not select it automatically.").size(size - 1.0)].spacing(10);
        let Some(draft) = context
            .draft()
            .filter(|draft| draft.conversation == source.id)
        else {
            if let Some(notice) = &context.notice {
                body = body.push(text(notice).size(size));
            }
            return body
                .push(
                    button(text("Load context").size(size))
                        .on_press(action(ContextAction::Refresh)),
                )
                .into();
        };
        body = body.push(
            text(format!(
                "Saved revision {}{}",
                draft.revision,
                if context.dirty() {
                    " · local changes"
                } else {
                    ""
                }
            ))
            .size(size - 1.0),
        );
        for (key, label) in [
            (ContextField::Goal, "Goal"),
            (ContextField::Completed, "Completed"),
            (ContextField::Remaining, "Remaining"),
            (ContextField::Constraints, "Constraints"),
        ] {
            body = body.push(self.context_input(&source.id, ContextInputKey::Field(key), label));
        }
        for material in &draft.materials {
            body = body.push(self.context_input(
                &source.id,
                ContextInputKey::Material(material.id.clone()),
                &crate::review_text::visible_bounded(&material.label, 240),
            ));
            if let Some(source) = &material.source {
                body = body.push(
                    text(format!(
                        "{} · revision {}",
                        source.event_id, source.revision
                    ))
                    .size(size - 2.0),
                );
            }
            if material.truncated {
                body = body.push(text("Selected or edited excerpt; inspect the referenced source for its original text.").size(size - 2.0));
            }
            body = body.push(
                button(text("Remove material").size(size))
                    .on_press(action(ContextAction::Remove(material.id.clone()))),
            );
        }
        if context.conflicted() {
            body = body
                .push(
                    text("A newer saved context exists. Your local draft is retained.")
                        .size(size)
                        .style(text::danger),
                )
                .push(
                    row![
                        button(text("Keep local for review").size(size))
                            .on_press(action(ContextAction::KeepLocal)),
                        button(text("Use saved context").size(size))
                            .on_press(action(ContextAction::UseShared))
                    ]
                    .spacing(4)
                    .wrap(),
                );
        }
        body = body.push(
            row![
                button(text("Add note").size(size)).on_press(action(ContextAction::AddNote)),
                button(text("Refresh saved").size(size)).on_press_maybe(
                    (!context.saving() && !context.loading())
                        .then(|| action(ContextAction::Refresh))
                ),
                button(text("Save context").size(size)).on_press_maybe(
                    (context.dirty()
                        && !context.saving()
                        && !context.loading()
                        && !context.conflicted()
                        && draft.validate().is_ok())
                    .then(|| action(ContextAction::Save))
                ),
            ]
            .spacing(4)
            .wrap(),
        );
        if let Some(notice) = &context.notice {
            body = body.push(text(notice).size(size));
        }
        let ready = !context.dirty()
            && !context.conflicted()
            && !context.loading()
            && !context.saving()
            && !context.building();
        let mut providers = row![].spacing(4);
        for (provider, label) in [
            ("codex", "Codex"),
            ("claude", "Claude"),
            ("kimi", "Kimi"),
            ("api", "API chat"),
        ] {
            providers = providers.push(
                button(text(label).size(size))
                    .on_press_maybe(ready.then(|| action(ContextAction::Build(provider.into())))),
            );
        }
        body = body
            .push(text("Preview handoff to").size(size - 1.0))
            .push(providers.wrap());
        if let Some(packet) = context.packet.as_ref().filter(|_| context.packet_current()) {
            body = body.push(text(format!("New {} draft · {} selected materials", packet.target_provider, packet.materials_count)).size(size))
                .push(text(crate::review_text::visible_bounded(&packet.body, 16 * 1024)).size(size))
                .push(text("Creates a new conversation. The source is not resumed and nothing is sent until you submit the draft.").size(size - 1.0))
                .push(button(text(if packet.target_provider == "api" { "Create API draft" } else { "Create task draft" }).size(size))
                    .on_press(action(ContextAction::Create { revision: packet.source_revision, provider: packet.target_provider.clone() })));
        }
        body.into()
    }

    fn workspace_result(&self, source: &WorkspaceRow, changes: bool) -> Element<'_, Message> {
        let size = self.chrome_font_size();
        let review = &self.agent_sessions.review;
        let action = |action| Message::WorkspaceReview(source.id.clone(), action);
        let mut body = column![
            text("Captured result").size(size + 1.0),
            button(text("Refresh result evidence").size(size))
                .on_press_maybe((!review.report_loading).then(|| action(ReviewAction::Refresh)))
        ]
        .spacing(10);
        if review.report_loading {
            body = body.push(text("Capturing read-only evidence…").size(size));
        }
        if let Some(notice) = &review.notice {
            body = body.push(text(notice).size(size));
        }
        let Some(report) = review
            .report
            .as_ref()
            .filter(|report| report.conversation == source.id)
        else {
            return body
                .push(text("No host result capture is available yet.").size(size))
                .into();
        };
        body = body.push(
            text(format!(
                "Run {} · captured event {}",
                report.run_id, report.source_sequence
            ))
            .size(size - 1.0),
        );
        if source
            .facts
            .source_sequence
            .is_some_and(|sequence| sequence != report.source_sequence)
        {
            body = body.push(text("Live and captured event versions differ. Refresh before relying on this capture.").size(size).style(text::danger));
        }
        if let Some(error) = &report.error {
            body = body.push(text(error).size(size).style(text::danger));
        }
        body = body.push(field(
            "Validation evidence",
            &report.validation_status,
            size,
        ));
        if !report.validation_current {
            body = body.push(text("Validation is unbound or stale for these files. A previous pass is not a current pass.").size(size - 1.0));
        }
        if let Some(validation) = &report.validation {
            body = body
                .push(field("Validation command", &validation.command, size))
                .push(field("Validation directory", &validation.cwd, size))
                .push(field(
                    "Validated fingerprint",
                    &validation.fingerprint,
                    size,
                ));
        }
        let context_ready = self.agent_sessions.context.selected() == Some(&source.id)
            && self.agent_sessions.context.draft().is_some();
        if changes {
            if let Some(baseline) = &report.baseline {
                body = body.push(field(
                    "Captured baseline fingerprint",
                    &baseline.fingerprint,
                    size,
                ));
            }
            if let Some(current) = &report.current {
                body = body
                    .push(field("Captured HEAD", &current.head, size))
                    .push(field("Current fingerprint", &current.fingerprint, size))
                    .push(text(&current.attribution).size(size - 1.0))
                    .push(
                        text(format!(
                            "{} untracked file(s) · captured {} ms",
                            current.untracked_files, current.captured_at_ms
                        ))
                        .size(size - 1.0),
                    )
                    .push(
                        text(crate::review_text::visible_bounded(
                            &current.diff,
                            64 * 1024,
                        ))
                        .size(size),
                    );
                if current.diff.len() > 64 * 1024 {
                    body = body.push(text("Preview shortened to 64 KiB. Use Review task diff for the retained diff.").size(size - 1.0));
                }
                body = body.push(
                    button(text("Add captured diff to context").size(size)).on_press_maybe(
                        context_ready.then(|| {
                            action(ReviewAction::AddCapture {
                                sequence: report.source_sequence,
                                run: report.run_id.clone(),
                                diff: true,
                            })
                        }),
                    ),
                );
            }
        } else if !report.validation_output.is_empty() {
            body = body.push(
                text(crate::review_text::visible_bounded(
                    &report.validation_output,
                    64 * 1024,
                ))
                .size(size),
            );
            if report.validation_output_truncated || report.validation_output.len() > 64 * 1024 {
                body = body.push(text("Validation log is a retained excerpt.").size(size - 1.0));
            }
            body = body.push(
                button(text("Add validation log to context").size(size)).on_press_maybe(
                    context_ready.then(|| {
                        action(ReviewAction::AddCapture {
                            sequence: report.source_sequence,
                            run: report.run_id.clone(),
                            diff: false,
                        })
                    }),
                ),
            );
        }
        body.into()
    }
    fn workspace_queue(&self, source: &WorkspaceRow) -> Element<'_, Message> {
        let size = self.chrome_font_size();
        let local: Vec<_> = self
            .workspace_task_rows()
            .into_iter()
            .map(|(_, row)| row)
            .collect();
        let review = &self.agent_sessions.review;
        let rows = review.combined_rows(&local);
        let action = |action| Message::WorkspaceReview(source.id.clone(), action);
        let mut body = column![text("Task queue").size(size + 1.0), text("Dependencies wait for accepted tasks. Ready tasks need an explicit Start from their task controls. Execution: Local.").size(size - 1.0)].spacing(10);
        if let Some(projected) = review
            .queue
            .project(&rows)
            .into_iter()
            .find(|item| item.id == source.id)
        {
            body = body.push(field("Queue state", projected.state.label(), size));
            if let Some(reason) = projected.reason {
                body = body.push(text(reason).size(size - 1.0));
            }
        }
        let intent = review
            .queue
            .entries
            .iter()
            .find(|entry| entry.intent.id == source.id)
            .map(|entry| &entry.intent);
        let enabled = !review.queue_loading && !review.queue_saving;
        if let Some(intent) = intent {
            body = body
                .push(
                    button(text("Remove from queue").size(size))
                        .on_press_maybe(enabled.then(|| action(ReviewAction::QueueRemove))),
                )
                .push(text("Wait for accepted dependencies").size(size - 1.0));
            for candidate in rows.iter().filter(|candidate| {
                candidate.kind == ConversationKind::Task
                    && candidate.id != source.id
                    && !candidate.archived
            }) {
                let id = candidate.id.clone();
                let source_id = source.id.clone();
                let mut control = checkbox(intent.dependencies.contains(&id))
                    .label(crate::review_text::visible_bounded(&candidate.title, 100))
                    .size(size);
                if enabled {
                    control = control.on_toggle(move |enabled| {
                        Message::WorkspaceReview(
                            source_id.clone(),
                            ReviewAction::Dependency(id.clone(), enabled),
                        )
                    });
                }
                body = body.push(control);
            }
        } else {
            body = body.push(
                button(text("Add task to queue").size(size))
                    .on_press_maybe(enabled.then(|| action(ReviewAction::QueueAdd))),
            );
        }
        body.into()
    }
    fn workspace_overview(&self) -> Element<'static, Message> {
        let size = self.chrome_font_size();
        let local: Vec<_> = self
            .workspace_task_rows()
            .into_iter()
            .map(|(_, row)| row)
            .collect();
        let rows = self.agent_sessions.review.combined_rows(&local);
        let overview =
            WorkspaceOverview::from_rows(&rows, self.agent_sessions.review.catalog_complete);
        let mut body = column![
            text("Workspace overview").size(size + 1.0),
            text(overview.resource_label()).size(size),
            text(format!(
                "{} task(s) · {} accepted · {} unconfirmed · {} need you",
                overview.tasks,
                overview.accepted,
                overview.unconfirmed,
                overview.attention_total()
            ))
            .size(size - 1.0)
        ]
        .spacing(10);
        for item in self.agent_sessions.review.queue.project(&rows) {
            body = body.push(
                text(format!(
                    "{} · {}",
                    crate::review_text::visible_bounded(&item.title, 100),
                    item.state.label()
                ))
                .size(size),
            );
            if let Some(reason) = item.reason {
                body = body.push(text(reason).size(size - 1.0));
            }
        }
        body.push(button(text("Refresh workspace").size(size)).on_press(Message::WorkspaceRefresh))
            .into()
    }
    fn workspace_evidence(&self) -> Element<'_, Message> {
        let size = self.chrome_font_size();
        let tools = &self.agent_sessions.tools;
        let Some(hit) = &tools.selected_hit else {
            return text("No selected evidence").into();
        };
        let mut body = column![
            text("Saved evidence").size(size + 1.0),
            field("Conversation", &hit.row.title, size),
            field("Source", hit.source.label(), size),
            field("Event", &hit.anchor.event_id, size),
            field("Revision", &hit.anchor.revision, size)
        ]
        .spacing(10);
        if tools.document_loading {
            body = body.push(text("Reading this revision…").size(size));
        }
        if let Some(error) = &tools.notice {
            body = body.push(text(error).size(size).style(text::danger));
        }
        if let Some(document) = &tools.document {
            body = body.push(
                text(crate::review_text::visible_multiline_bounded(
                    &document.text,
                    256 * 1024,
                ))
                .size(size),
            );
            if document.truncated {
                body = body.push(
                    text("Only the retained, indexed excerpt is available.").size(size - 1.0),
                );
            }
            if let Some(target) = self.workspace_selected_row().filter(|row| {
                self.agent_sessions.context.selected() == Some(&row.id)
                    && self.agent_sessions.context.draft().is_some()
            }) {
                body = body.push(
                    button(
                        text(format!(
                            "Add excerpt to {} context",
                            crate::review_text::visible_bounded(&target.title, 60)
                        ))
                        .size(size),
                    )
                    .on_press(Message::WorkspaceContext(
                        target.id,
                        ContextAction::AddEvidence(document.clone()),
                    )),
                );
            }
        }
        body = body
            .push(
                text(
                    "Read-only evidence. Nothing has resumed and this text is not sent to a model.",
                )
                .size(size - 1.0),
            )
            .push(
                button(text("Open conversation").size(size)).on_press_maybe(
                    self.workspace_evidence_can_open()
                        .then_some(Message::WorkspaceEvidenceOpen),
                ),
            )
            .push(
                button(text("Close evidence").size(size)).on_press(Message::WorkspaceSearchClose),
            );
        container(scrollable(body).height(Length::Fill))
            .padding(12)
            .height(Length::Fill)
            .into()
    }
}
