//! Authored task drafts and read-only task evidence for the native workspace.
use crate::agent_task::{AgentProvider, AgentTask, TaskBrief, TaskValidationRecipe};
use crate::Message;
use iced::widget::{button, column, container, row, scrollable, text, text_editor, text_input};
use iced::{Element, Length};
use std::path::PathBuf;
use std::sync::Arc;

const GOAL_LIMIT: usize = 16 * 1024;

#[derive(Clone, Debug)]
pub(crate) enum ComposerEdit {
    Title(String),
    Project(String),
    Goal(text_editor::Action),
    Provider(AgentProvider),
    Validation(String),
    Shell(String),
}

pub(crate) struct TaskComposer {
    pub(crate) title: String,
    pub(crate) project: String,
    pub(crate) goal: text_editor::Content,
    pub(crate) provider: AgentProvider,
    pub(crate) validation: String,
    pub(crate) shell: String,
    pub(crate) notice: Option<String>,
    pub(crate) busy: bool,
    epoch: Arc<()>,
}

pub(crate) struct AuthoredTask {
    pub(crate) title: String,
    pub(crate) project: PathBuf,
    pub(crate) provider: AgentProvider,
    pub(crate) brief: TaskBrief,
}

impl TaskComposer {
    pub(crate) fn new(project: String, shell: String, provider: AgentProvider) -> Self {
        Self {
            title: String::new(),
            project,
            goal: text_editor::Content::new(),
            provider,
            validation: String::new(),
            shell,
            notice: None,
            busy: false,
            epoch: Arc::new(()),
        }
    }

    pub(crate) fn reference(&self) -> Arc<()> {
        self.epoch.clone()
    }

    pub(crate) fn retire_reference(&mut self) {
        self.epoch = Arc::new(());
    }

    pub(crate) fn accepts(&self, reference: &Arc<()>) -> bool {
        !self.busy && Arc::ptr_eq(&self.epoch, reference)
    }

    pub(crate) fn edit(&mut self, reference: &Arc<()>, edit: ComposerEdit) {
        if !self.accepts(reference) {
            return;
        }
        let too_large = match &edit {
            ComposerEdit::Title(value) => value.len() > 512,
            ComposerEdit::Project(value) | ComposerEdit::Shell(value) => value.len() > 4096,
            ComposerEdit::Validation(value) => value.len() > GOAL_LIMIT,
            _ => false,
        };
        if too_large {
            self.notice =
                Some("That value exceeds the input limit. Your existing draft was kept.".into());
            return;
        }
        match edit {
            ComposerEdit::Title(value) => self.title = value,
            ComposerEdit::Project(value) => self.project = value,
            ComposerEdit::Provider(value) => self.provider = value,
            ComposerEdit::Validation(value) => self.validation = value,
            ComposerEdit::Shell(value) => self.shell = value,
            ComposerEdit::Goal(action) => {
                if matches!(&action, text_editor::Action::Edit(text_editor::Edit::Paste(value)) if value.len() > GOAL_LIMIT)
                {
                    self.notice = Some(
                        "The goal must fit within 16 KiB. Your existing draft was kept.".into(),
                    );
                    return;
                }
                let previous = self.goal.text();
                let editing = action.is_edit();
                self.goal.perform(action);
                if self.goal.text().len() > GOAL_LIMIT {
                    self.goal = text_editor::Content::with_text(&previous);
                    self.notice = Some(
                        "The goal must fit within 16 KiB. Your existing draft was kept.".into(),
                    );
                    return;
                }
                if !editing {
                    return;
                }
            }
        }
        self.notice = None;
    }

    pub(crate) fn prepare(&self) -> Result<AuthoredTask, String> {
        let project = PathBuf::from(self.project.trim());
        if !project.is_absolute() || self.project.chars().any(char::is_control) {
            return Err("Choose an absolute local Git project path.".into());
        }
        let validation = if self.validation.trim().is_empty() {
            None
        } else {
            Some(TaskValidationRecipe::new(
                self.validation.clone(),
                self.shell.clone(),
            )?)
        };
        let brief = TaskBrief::new(self.goal.text(), validation)?;
        let title = if self.title.trim().is_empty() {
            jterm_core::review_input::safe_inline_display(
                brief
                    .goal
                    .lines()
                    .find(|line| !line.trim().is_empty())
                    .unwrap_or("New task"),
                crate::agent_task_ui::MAX_TASK_TITLE_DISPLAY_BYTES,
            )
        } else {
            let title = self.title.trim();
            if title.len() > 256
                || title.chars().any(char::is_control)
                || jterm_core::review_input::contains_visual_spoofing(title)
            {
                return Err(
                    "Use a task name of at most 256 bytes without control characters.".into(),
                );
            }
            title.to_owned()
        };
        Ok(AuthoredTask {
            title,
            project,
            provider: self.provider,
            brief,
        })
    }
}

fn composer_input<'a>(
    placeholder: &'a str,
    value: &'a str,
    composer: &TaskComposer,
    size: f32,
    edit: fn(String) -> ComposerEdit,
) -> iced::widget::TextInput<'a, Message> {
    let reference = composer.reference();
    text_input(placeholder, value).size(size).on_input_maybe(
        (!composer.busy)
            .then_some(move |value| Message::TaskComposerEdit(reference.clone(), edit(value))),
    )
}

pub(crate) fn composer_view(composer: &TaskComposer, size: f32) -> Element<'_, Message> {
    let reference = composer.reference();
    let mut goal = text_editor(&composer.goal)
        .placeholder("What should the Agent accomplish?")
        .size(size)
        .height(Length::Fixed(144.0));
    if !composer.busy {
        let reference = reference.clone();
        goal = goal.on_action(move |action| {
            Message::TaskComposerEdit(reference.clone(), ComposerEdit::Goal(action))
        });
    }
    let mut body = column![
        text("New project task").size(size + 2.0),
        text("Describe the goal, then create an isolated worktree. Start the Agent when you are ready.").size(size - 1.0),
        composer_input("Task name (optional)", &composer.title, composer, size, ComposerEdit::Title),
        text("Project folder").size(size),
        composer_input("/absolute/path/to/project", &composer.project, composer, size, ComposerEdit::Project),
        text("Goal").size(size),
        goal,
        text(format!("{} / 16384 bytes", composer.goal.text().len())).size(size - 2.0).style(text::secondary),
    ].spacing(7);
    let mut providers = row![].spacing(4);
    for provider in AgentProvider::ALL {
        providers = providers.push(
            button(text(provider.display_name()).size(size - 1.0))
                .style(if composer.provider == provider {
                    button::primary
                } else {
                    button::secondary
                })
                .on_press_maybe((!composer.busy).then(|| {
                    Message::TaskComposerEdit(reference.clone(), ComposerEdit::Provider(provider))
                })),
        );
    }
    body = body
        .push(providers.wrap())
        .push(text("Validation command (optional)").size(size))
        .push(composer_input(
            "For example: cargo test --locked",
            &composer.validation,
            composer,
            size,
            ComposerEdit::Validation,
        ));
    if !composer.validation.is_empty() {
        body = body
            .push(text("Validation shell (absolute path)").size(size - 1.0))
            .push(composer_input(
                "/bin/bash",
                &composer.shell,
                composer,
                size,
                ComposerEdit::Shell,
            ));
    }
    body = body.push(text(if composer.validation.trim().is_empty() {
        "Without validation, completion requires your explicit review of the work."
    } else {
        "Validation runs at the project root inside the worktree, only when you choose Run validation."
    }).size(size - 1.0).style(text::secondary));
    if let Some(notice) = &composer.notice {
        body = body.push(text(notice).size(size).style(text::danger));
    }
    body = body
        .push(crate::widget_identity_scope::scope_key(
            (
                composer.title.clone(),
                composer.project.clone(),
                composer.goal.text(),
                composer.provider,
                composer.validation.clone(),
                composer.shell.clone(),
            ),
            row![
                button(
                    text(if composer.busy {
                        "Creating worktree…"
                    } else {
                        "Create task"
                    })
                    .size(size)
                )
                .on_press_maybe(
                    (!composer.busy).then(|| Message::TaskComposerCreate(reference.clone()))
                ),
                button(text("Close draft").size(size))
                    .style(button::secondary)
                    .on_press_maybe(
                        (!composer.busy).then(|| Message::TaskComposerClose(reference.clone()))
                    ),
            ]
            .spacing(6)
            .wrap(),
        ))
        .push(
            text("Goal drafts stay in this running window. Re-enter them after restarting Frost.")
                .size(size - 2.0)
                .style(text::secondary),
        );
    crate::widget_identity_scope::scope(reference, body)
}

impl crate::Frost {
    pub(crate) fn task_composer_visible(&self) -> bool {
        self.config.experimental_task_sidebar
            && !self.agent_sessions.open
            && self.dock_open()
            && self.sidebar_panel == crate::SidebarPanel::Tasks
            && self.task_panel.composer_open
    }

    pub(crate) fn task_composer_open(&mut self) {
        if !self.config.experimental_task_sidebar {
            self.push_toast(
                "Tasks are hidden in Settings. Enable the Tasks dashboard to create a task.",
                crate::ToastKind::Info,
            );
            return;
        }
        if self.task_panel.pending_creation.is_some() {
            return;
        }
        self.task_panel.retire_panel_callbacks();
        if self.task_panel.composer.is_none() {
            let project = self
                .sessions
                .get(self.active)
                .and_then(|session| session.cwd())
                .unwrap_or_default();
            let shell =
                crate::pty::resolved_shell_identity(self.config.shell.as_deref(), Some(&project))
                    .unwrap_or_default();
            let provider = AgentProvider::from_config_value(&self.config.preferred_agent_provider)
                .unwrap_or(AgentProvider::Codex);
            self.task_panel.composer = Some(TaskComposer::new(project, shell, provider));
        }
        self.task_panel.composer_open = true;
        self.set_task_panel_open(true);
    }

    pub(crate) fn task_composer_create(&mut self, reference: Arc<()>) {
        if !self.task_composer_visible() || self.task_panel.pending_creation.is_some() {
            return;
        }
        let Some(composer) = self
            .task_panel
            .composer
            .as_mut()
            .filter(|composer| composer.accepts(&reference))
        else {
            return;
        };
        let task = match composer.prepare() {
            Ok(task) => task,
            Err(error) => {
                composer.notice = Some(error);
                return;
            }
        };
        match crate::agent_task_ui::begin_authored_creation(task) {
            Ok(pending) => {
                composer.busy = true;
                composer.notice = None;
                composer.retire_reference();
                self.task_panel.pending_creation = Some(pending);
            }
            Err(error) => composer.notice = Some(error),
        }
    }

    pub(crate) fn task_composer_failed(&mut self, error: &str) {
        if let Some(composer) = self
            .task_panel
            .composer
            .as_mut()
            .filter(|composer| composer.busy)
        {
            composer.busy = false;
            composer.notice = Some(error.to_owned());
            composer.retire_reference();
        }
    }
}

pub(crate) fn inspector_view(task: &AgentTask, size: f32) -> Element<'static, Message> {
    let inspector = crate::agent_task::TaskInspector::from_task(task);
    let mut body = column![text("Task details").size(size + 1.0)].spacing(4);
    for field in inspector.fields {
        // The workspace shows a runtime-aware TaskGuidance card above its
        // actions. The inspector's legacy static hint would duplicate it.
        if field.label == "Next step" {
            continue;
        }
        body = body.push(text(format!("{}: {}", field.label, field.value)).size(size - 1.0));
    }
    if let Some(goal) = inspector.goal {
        body = body
            .push(text("Goal").size(size))
            .push(scrollable(text(goal).size(size)).height(Length::Fixed(108.0)));
    }
    container(body)
        .padding(7)
        .style(container::bordered_box)
        .into()
}

pub(crate) fn guidance_view(
    task: &AgentTask,
    activity: crate::agent_task::DashboardActivity,
    size: f32,
) -> Element<'static, Message> {
    let guidance = crate::agent_task::TaskGuidance::from_task(task, activity);
    let mut body = column![
        text(format!("Next step · {}", guidance.title)).size(size + 1.0),
        text(guidance.detail).size(size).style(text::secondary),
    ]
    .spacing(4);
    if let Some(reason) = guidance.reason {
        body = body.push(text(reason).size(size).style(text::danger));
    }
    container(body)
        .padding(8)
        .style(container::bordered_box)
        .width(Length::Fill)
        .into()
}

/// Render the runtime's retained evidence. No timestamps or lifecycle events
/// are reconstructed from polling, and history never creates action authority.
pub(crate) fn history_view(
    snapshot: &crate::agent_task::CodexAppServerViewSnapshot,
    size: f32,
) -> Element<'static, Message> {
    let mut body = column![].spacing(6);
    let display = |value: &str, limit| crate::review_text::visible_bounded(value, limit);
    if let Some(error) = &snapshot.last_error {
        body = body.push(text(display(error, 2048)).size(size).style(text::danger));
    }
    if !snapshot.file_changes.is_empty() {
        body = body.push(text("Current turn · changed files").size(size));
        for item in &snapshot.file_changes {
            for change in &item.changes {
                body = body.push(
                    text(format!(
                        "{} · {} · {}{}",
                        display(&change.kind, 64),
                        display(&change.path, 512),
                        display(&item.status, 64),
                        if change.path_exact {
                            ""
                        } else {
                            " (path shortened)"
                        }
                    ))
                    .size(size - 1.0),
                );
            }
            if item.changes_truncated {
                body = body.push(
                    text("Additional file changes were omitted.")
                        .size(size - 1.0)
                        .style(text::secondary),
                );
            }
        }
    }
    if !snapshot.turn_history.is_empty() {
        body = body.push(
            text(format!(
                "Earlier turns · {} retained",
                snapshot.turn_history.len()
            ))
            .size(size),
        );
        let mut history = column![].spacing(8);
        for turn in snapshot.turn_history.iter().rev() {
            let mut entry = column![text(format!("Turn {}", turn.ordinal)).size(size)].spacing(4);
            if let Some(feedback) = &turn.follow_up_feedback {
                entry = entry.push(
                    text(format!("Your feedback: {}", display(feedback, 4096))).size(size - 1.0),
                );
            }
            if !turn.agent_text.is_empty() {
                entry = entry.push(text(display(&turn.agent_text, 8192)).size(size - 1.0));
            }
            if turn.agent_text_truncated || turn.agent_text.len() > 8192 {
                entry = entry.push(
                    text("Agent response shortened.")
                        .size(size - 2.0)
                        .style(text::secondary),
                );
            }
            for command in &turn.commands {
                entry = entry.push(
                    text(format!(
                        "$ {} · {}{}",
                        display(&command.command, 1024),
                        display(&command.status, 64),
                        if command.output_omitted {
                            " · output omitted"
                        } else {
                            ""
                        }
                    ))
                    .size(size - 1.0),
                );
            }
            for file in &turn.file_changes {
                entry = entry.push(
                    text(format!(
                        "{} · {} change(s) · {}{}",
                        display(file.path.as_deref().unwrap_or("File changes"), 512),
                        file.change_count,
                        display(&file.status, 64),
                        if file.changes_truncated || file.path_truncated {
                            " · shortened"
                        } else {
                            ""
                        }
                    ))
                    .size(size - 1.0),
                );
            }
            if turn.dropped_updates > 0 {
                entry = entry.push(
                    text(format!("{} updates omitted", turn.dropped_updates))
                        .size(size - 2.0)
                        .style(text::secondary),
                );
            }
            history = history.push(container(entry).padding(6).style(container::bordered_box));
        }
        body = body.push(scrollable(history).height(Length::Fixed(260.0)));
    }
    if snapshot.dropped_turns > 0 || snapshot.dropped_updates > 0 {
        body = body.push(
            text(format!(
                "Retention limit: {} earlier turns and {} updates omitted",
                snapshot.dropped_turns, snapshot.dropped_updates
            ))
            .size(size - 1.0)
            .style(text::secondary),
        );
    }
    body.into()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn draft() -> TaskComposer {
        let mut composer = TaskComposer::new(
            "/tmp/project".into(),
            "/bin/bash".into(),
            AgentProvider::Codex,
        );
        composer.goal = text_editor::Content::with_text(
            "Improve project navigation\nKeep keyboard selection predictable.",
        );
        composer
    }

    #[test]
    fn authored_goal_and_optional_recipe_keep_their_distinct_intent() {
        for provider in AgentProvider::ALL {
            let mut composer = draft();
            composer.provider = provider;
            let task = composer.prepare().unwrap();
            assert_eq!(task.provider, provider);
            assert_eq!(task.title, "Improve project navigation");
            assert!(task.brief.goal.contains('\n'));
            assert!(task.brief.validation.is_none());
            assert_eq!(task.project, PathBuf::from("/tmp/project"));
            composer.validation = "cargo test --locked".into();
            let task = composer.prepare().unwrap();
            let recipe = task.brief.validation.unwrap();
            assert_eq!(recipe.command, "cargo test --locked");
            assert_eq!(recipe.shell, "/bin/bash");
        }
    }

    #[test]
    fn invalid_recipe_or_project_preserves_the_original_draft() {
        let mut composer = draft();
        composer.validation = "cargo test\necho unreviewed".into();
        assert!(composer.prepare().is_err());
        assert_eq!(composer.validation, "cargo test\necho unreviewed");
        composer.validation = "cargo test".into();
        composer.shell = "bash".into();
        assert!(composer.prepare().is_err());
        composer.validation.clear();
        composer.project = "relative-project".into();
        assert!(composer.prepare().is_err());
        assert_eq!(composer.goal.text(), draft().goal.text());
    }

    #[test]
    fn hidden_or_submitting_composer_rejects_retained_edits() {
        let mut composer = draft();
        let old = composer.reference();
        composer.retire_reference();
        composer.edit(&old, ComposerEdit::Provider(AgentProvider::Kimi));
        assert_eq!(composer.provider, AgentProvider::Codex);
        let current = composer.reference();
        composer.busy = true;
        composer.edit(&current, ComposerEdit::Title("late edit".into()));
        assert!(composer.title.is_empty());
        composer.busy = false;
        composer.edit(&current, ComposerEdit::Title("Reviewed title".into()));
        assert_eq!(composer.prepare().unwrap().title, "Reviewed title");
    }

    #[test]
    fn oversized_paste_does_not_turn_into_a_different_task_or_command() {
        let mut composer = draft();
        let original = composer.goal.text();
        let reference = composer.reference();
        composer.edit(
            &reference,
            ComposerEdit::Goal(text_editor::Action::Edit(text_editor::Edit::Paste(
                Arc::new("x".repeat(GOAL_LIMIT + 1)),
            ))),
        );
        assert_eq!(composer.goal.text(), original);
        assert!(composer.notice.is_some());
        composer.validation = "cargo test".into();
        composer.edit(
            &reference,
            ComposerEdit::Validation("x".repeat(GOAL_LIMIT + 1)),
        );
        assert_eq!(composer.validation, "cargo test");
    }

    #[test]
    fn goal_enter_is_an_editor_action_and_controls_are_rejected_before_creation() {
        let mut composer = draft();
        let reference = composer.reference();
        composer.edit(
            &reference,
            ComposerEdit::Goal(text_editor::Action::Move(text_editor::Motion::DocumentEnd)),
        );
        composer.edit(
            &reference,
            ComposerEdit::Goal(text_editor::Action::Edit(text_editor::Edit::Enter)),
        );
        assert!(!composer.busy);
        assert!(composer.goal.text().ends_with('\n'));
        composer.goal = text_editor::Content::with_text("A goal with \u{202e} hidden direction");
        assert!(composer.prepare().is_err());
    }
}
