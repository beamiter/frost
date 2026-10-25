//! iced-side state and helpers for the experimental Tasks dashboard.
//!
//! The provider-neutral domain (task reducer, native runtime, worktree
//! containment, validation preflight) lives in [`crate::agent_task`]. This
//! module owns only the pieces iced's update/view loop needs: panel view
//! state, background worktree creation, the prompt-consent projection of the
//! user configuration, and the validation terminal's argv/environment
//! contract. Everything here is either pure or structured so the pure parts
//! are headless-testable.

/// Resolve the current binding without interpreting its opaque session identity.
pub(crate) fn task_terminal_binding(
    task: &crate::agent_task::AgentTask,
    role: crate::agent_task::TaskTerminalRole,
) -> Option<&str> {
    match role {
        crate::agent_task::TaskTerminalRole::Agent => task.terminal_session_id.as_deref(),
        crate::agent_task::TaskTerminalRole::Validation => {
            task.validation.terminal_session_id.as_deref()
        }
    }
}

/// Read-only activity membership against one frame's live terminal inventory.
/// Kept separate from focus resolution, which must still recheck its owner.
pub(crate) fn task_terminal_is_live(
    task: &crate::agent_task::AgentTask,
    role: crate::agent_task::TaskTerminalRole,
    live: &std::collections::HashSet<String>,
) -> bool {
    task_terminal_binding(task, role).is_some_and(|binding| live.contains(binding))
}

/// Resolve against live identities, never against an old tab or pane index.
pub(crate) fn resolve_task_terminal<T>(
    binding: Option<&str>,
    terminals: impl IntoIterator<Item = (String, T)>,
) -> Option<T> {
    let binding = binding?;
    terminals
        .into_iter()
        .find_map(|(identity, target)| (identity == binding).then_some(target))
}

pub(crate) fn task_focus_allowed(
    visible: bool,
    selected: Option<TaskId>,
    requested: TaskId,
) -> bool {
    visible && selected == Some(requested)
}

/// Truncate a single-line reason by terminal display cells, leaving room for an ellipsis.
pub(crate) fn reason_preview(detail: &str, columns: usize) -> String {
    let width = |text: &str| {
        text.chars()
            .map(jterm_core::char_width::cached_char_width)
            .sum::<usize>()
    };
    if width(detail) <= columns {
        return detail.to_owned();
    }
    if columns == 0 {
        return String::new();
    }
    let mut used = 0;
    let mut preview = String::new();
    for character in detail.chars() {
        let next = jterm_core::char_width::cached_char_width(character);
        if used + next > columns.saturating_sub(1) {
            break;
        }
        preview.push(character);
        used += next;
    }
    preview.push('…');
    preview
}

use crate::agent_task::{
    AgentProvider, ManagedWorktree, NativePromptPolicy, SemanticCommandContext, TaskId,
    WorktreeService, CODEX_APP_SERVER_LIVE_TURN_MAX, NATIVE_AGENT_FOLLOW_UP_MAX_BYTES,
};
use crate::config::Config;
use crate::review_text::visible_bounded;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::sync::Arc;
use std::thread::JoinHandle;

/// Display bound for one task title in list rows.
pub(crate) const MAX_TASK_TITLE_DISPLAY_BYTES: usize = 112;
/// Display bound for branch names, status details, and validation details.
pub(crate) const MAX_TASK_DETAIL_DISPLAY_BYTES: usize = 256;

/// Keep both panes readable at large text sizes. The ordinary narrow dock
/// switches between list and inspector instead of squeezing either column.
pub(crate) fn task_workspace_has_columns(width: f32, font_size: f32) -> bool {
    width >= 480.0 * (font_size / 14.0).max(1.0)
}

/// Keep the search widget under the same parents as wide-layout results change.
/// Replacing the row with a bare list on no matches loses input focus, sending
/// the rest of a typed query into the terminal instead.
pub(crate) fn task_workspace_layout<'a>(
    list: iced::Element<'a, crate::Message>,
    inspector: iced::Element<'a, crate::Message>,
    has_columns: bool,
    show_inspector: bool,
) -> iced::Element<'a, crate::Message> {
    use iced::widget::{container, row};
    use iced::Length;

    if has_columns {
        row![
            container(list).width(Length::FillPortion(2)),
            container(inspector).width(Length::FillPortion(3)),
        ]
        .spacing(8)
        .height(Length::Fill)
        .into()
    } else if show_inspector {
        inspector
    } else {
        list
    }
}

/// The active workspace never lists hidden (archived) tasks; the Hidden view
/// lists only them, so `is:archived` and Restore have something to act on.
pub(crate) fn task_in_dashboard_scope(
    task: &crate::agent_task::AgentTask,
    show_hidden: bool,
) -> bool {
    (task.status == crate::agent_task::TaskStatus::Archived) == show_hidden
}

/// Badge text and tooltip for tasks needing the user, shown outside the
/// dock. `None` when nothing waits, so quiet workspaces stay unbadged.
pub(crate) fn attention_badge(
    counts: crate::agent_task::DashboardAttentionCounts,
) -> Option<(usize, String)> {
    Some((counts.total(), counts.summary()?))
}

/// Why Hide is unavailable, phrased as the step that makes it available.
pub(crate) fn hide_blocked_reason(error: &crate::agent_task::TaskError) -> String {
    match error {
        crate::agent_task::TaskError::CannotArchiveRunning(_) => {
            "Hide becomes available after the Agent, its native session and any validation stop."
                .to_owned()
        }
        error => visible_bounded(&error.to_string(), MAX_TASK_DETAIL_DISPLAY_BYTES),
    }
}

/// How long one user action keeps the runtime on the fast poll cadence.
pub(crate) const PROMPT_POLL_WINDOW: std::time::Duration = std::time::Duration::from_secs(2);

/// Report whether any text field inside the Tasks dock holds keyboard focus.
///
/// Iced text inputs leave keys they do not consume (arrows, Tab, Enter
/// without a submit action, most Ctrl chords) uncaptured, so the app must
/// know a dock field is focused to keep those keys away from the terminal.
/// After every interaction and redraw the wrapper compares the live focus
/// state with the app's flag (`reported`) and publishes `on_change` only when
/// they differ, which also clears the flag after a focused field disappears.
pub(crate) fn report_input_focus<'a, Message: 'a, Theme: 'a, Renderer>(
    content: impl Into<iced::Element<'a, Message, Theme, Renderer>>,
    reported: bool,
    on_change: fn(bool) -> Message,
) -> iced::Element<'a, Message, Theme, Renderer>
where
    Renderer: iced::advanced::renderer::Renderer + 'a,
{
    iced::Element::new(FocusReport {
        content: content.into(),
        reported,
        on_change,
    })
}

struct FocusReport<'a, Message, Theme, Renderer> {
    content: iced::Element<'a, Message, Theme, Renderer>,
    reported: bool,
    on_change: fn(bool) -> Message,
}

/// Visits focusable widgets only, stopping at the first focused one.
struct FocusProbe(bool);

impl iced::advanced::widget::Operation for FocusProbe {
    fn traverse(&mut self, operate: &mut dyn FnMut(&mut dyn iced::advanced::widget::Operation)) {
        if !self.0 {
            operate(self);
        }
    }

    fn focusable(
        &mut self,
        _id: Option<&iced::advanced::widget::Id>,
        _bounds: iced::Rectangle,
        state: &mut dyn iced::advanced::widget::operation::Focusable,
    ) {
        self.0 |= state.is_focused();
    }
}

/// Focus moves on clicks, touches, keys and IME events, and a focused field
/// can vanish with a rebuilt view; cursor movement alone never changes it.
fn focus_may_have_changed(event: &iced::Event) -> bool {
    use iced::{mouse, Event};
    matches!(
        event,
        Event::Keyboard(_)
            | Event::Touch(_)
            | Event::InputMethod(_)
            | Event::Mouse(mouse::Event::ButtonPressed(_) | mouse::Event::ButtonReleased(_))
            | Event::Window(iced::window::Event::RedrawRequested(_))
    )
}

impl<Message, Theme, Renderer> iced::advanced::Widget<Message, Theme, Renderer>
    for FocusReport<'_, Message, Theme, Renderer>
where
    Renderer: iced::advanced::renderer::Renderer,
{
    fn children(&self) -> Vec<iced::advanced::widget::Tree> {
        vec![iced::advanced::widget::Tree::new(&self.content)]
    }

    fn diff(&self, tree: &mut iced::advanced::widget::Tree) {
        tree.diff_children(std::slice::from_ref(&self.content));
    }

    fn size(&self) -> iced::Size<iced::Length> {
        self.content.as_widget().size()
    }

    fn size_hint(&self) -> iced::Size<iced::Length> {
        self.content.as_widget().size_hint()
    }

    fn layout(
        &mut self,
        tree: &mut iced::advanced::widget::Tree,
        renderer: &Renderer,
        limits: &iced::advanced::layout::Limits,
    ) -> iced::advanced::layout::Node {
        self.content
            .as_widget_mut()
            .layout(&mut tree.children[0], renderer, limits)
    }

    fn draw(
        &self,
        tree: &iced::advanced::widget::Tree,
        renderer: &mut Renderer,
        theme: &Theme,
        style: &iced::advanced::renderer::Style,
        layout: iced::advanced::Layout<'_>,
        cursor: iced::mouse::Cursor,
        viewport: &iced::Rectangle,
    ) {
        self.content.as_widget().draw(
            &tree.children[0],
            renderer,
            theme,
            style,
            layout,
            cursor,
            viewport,
        );
    }

    fn operate(
        &mut self,
        tree: &mut iced::advanced::widget::Tree,
        layout: iced::advanced::Layout<'_>,
        renderer: &Renderer,
        operation: &mut dyn iced::advanced::widget::Operation,
    ) {
        self.content
            .as_widget_mut()
            .operate(&mut tree.children[0], layout, renderer, operation);
    }

    fn update(
        &mut self,
        tree: &mut iced::advanced::widget::Tree,
        event: &iced::Event,
        layout: iced::advanced::Layout<'_>,
        cursor: iced::mouse::Cursor,
        renderer: &Renderer,
        clipboard: &mut dyn iced::advanced::Clipboard,
        shell: &mut iced::advanced::Shell<'_, Message>,
        viewport: &iced::Rectangle,
    ) {
        self.content.as_widget_mut().update(
            &mut tree.children[0],
            event,
            layout,
            cursor,
            renderer,
            clipboard,
            shell,
            viewport,
        );
        if !focus_may_have_changed(event) {
            return;
        }
        let mut probe = FocusProbe(false);
        self.content
            .as_widget_mut()
            .operate(&mut tree.children[0], layout, renderer, &mut probe);
        if probe.0 != self.reported {
            // Publish one change per rebuilt view, not one per event.
            self.reported = probe.0;
            shell.publish((self.on_change)(probe.0));
        }
    }

    fn mouse_interaction(
        &self,
        tree: &iced::advanced::widget::Tree,
        layout: iced::advanced::Layout<'_>,
        cursor: iced::mouse::Cursor,
        viewport: &iced::Rectangle,
        renderer: &Renderer,
    ) -> iced::mouse::Interaction {
        self.content.as_widget().mouse_interaction(
            &tree.children[0],
            layout,
            cursor,
            viewport,
            renderer,
        )
    }

    fn overlay<'b>(
        &'b mut self,
        tree: &'b mut iced::advanced::widget::Tree,
        layout: iced::advanced::Layout<'b>,
        renderer: &Renderer,
        viewport: &iced::Rectangle,
        translation: iced::Vector,
    ) -> Option<iced::advanced::overlay::Element<'b, Message, Theme, Renderer>> {
        self.content.as_widget_mut().overlay(
            &mut tree.children[0],
            layout,
            renderer,
            viewport,
            translation,
        )
    }
}

/// Counts describe current search matches before the selected quick view.
pub(crate) fn dashboard_filter_label(
    view: crate::agent_task::DashboardView,
    counts: &crate::agent_task::DashboardCounts,
) -> String {
    use crate::agent_task::DashboardView;
    let (label, count) = match view {
        DashboardView::All => ("All", counts.total),
        DashboardView::NeedsAttention => ("Needs you", counts.attention),
        DashboardView::Running => ("Running", counts.running),
        DashboardView::NotStarted => ("Not started", counts.created),
        DashboardView::Review => ("Review", counts.review),
        DashboardView::Finished => ("Finished", counts.done + counts.failed + counts.cancelled),
    };
    format!("{label} {count}")
}

/// Visible before starting a print-mode provider; a worktree is not containment.
pub(crate) fn native_start_notice(provider: AgentProvider) -> Option<&'static str> {
    match provider {
        AgentProvider::Claude => Some("Claude skips its own permission prompts and can automatically run tools with your user account's file access. This worktree is not a sandbox."),
        AgentProvider::Kimi => Some("Kimi uses automatic tool permission and can run tools with your user account's file access. This worktree is not a sandbox."),
        AgentProvider::Codex | AgentProvider::OpenCode => None,
    }
}

/// Consent projection for any native provider prompt. `share_command_context`
/// requires both the AI master switch and the explicit command-context
/// sharing opt-in, mirroring the Settings copy; secret redaction follows the
/// user's AI redaction policy.
pub(crate) fn prompt_policy(config: &Config) -> NativePromptPolicy {
    NativePromptPolicy {
        share_command_context: config.ai_enabled && config.ai_share_command_context,
        redact_secrets: config.ai_redact_secrets,
    }
}

/// Stable string identity for one PTY session at the task boundary.
///
/// Task metadata outlives tab/pane positions, so the reducer keys terminal
/// bindings on this string rather than a session index. The grammar matches
/// the family-shared jsh session-id rule (alphanumeric, `-`, `_`).
pub(crate) fn terminal_session_id(session_id: usize) -> String {
    format!("frost-{}-{session_id}", std::process::id())
}

/// A follow-up turn may be sent only when it carries visible text, stays
/// inside the native byte budget, the live session has turn headroom, and the
/// payload is not a neutralized spoofing paste.
pub(crate) fn native_follow_up_can_send(text: &str, completed_turns: usize) -> bool {
    !text
        .trim_matches(|character| matches!(character, ' ' | '\n' | '\t'))
        .is_empty()
        && text.len() <= NATIVE_AGENT_FOLLOW_UP_MAX_BYTES
        && completed_turns < CODEX_APP_SERVER_LIVE_TURN_MAX
        && !follow_up_is_unsafe(text)
}

pub(crate) fn follow_up_is_unsafe(text: &str) -> bool {
    text.chars().any(|character| {
        if matches!(character, '\n' | '\t') {
            false
        } else {
            character == '\u{fffd}'
                || character.is_control()
                || jterm_core::review_input::is_visual_spoofing_character(character)
        }
    })
}

/// Bound the Tasks follow-up composer. Newlines and tabs stay (the send gate
/// already treats them as structural whitespace); other controls are dropped
/// and overflow truncates on a UTF-8 boundary so a paste cannot bounce.
pub(crate) fn bound_follow_up(text: impl Into<String>) -> String {
    let mut text: String = text
        .into()
        .chars()
        .filter_map(|character| {
            if matches!(character, '\n' | '\t') {
                Some(character)
            } else if character.is_control() {
                None
            } else if jterm_core::review_input::is_visual_spoofing_character(character) {
                Some('\u{fffd}')
            } else {
                Some(character)
            }
        })
        .collect();
    if text.len() > NATIVE_AGENT_FOLLOW_UP_MAX_BYTES {
        let mut end = NATIVE_AGENT_FOLLOW_UP_MAX_BYTES;
        while end > 0 && !text.is_char_boundary(end) {
            end -= 1;
        }
        text.truncate(end);
    }
    // Drop spare allocation from a large paste before retaining this draft.
    text.into_boxed_str().into_string()
}

/// Fully prepared task registration produced by the background worktree
/// worker. The UI thread only registers it with the task manager.
pub(crate) struct PreparedTask {
    pub(crate) context: Option<SemanticCommandContext>,
    pub(crate) brief: Option<crate::agent_task::TaskBrief>,
    pub(crate) title: String,
    pub(crate) provider: AgentProvider,
    pub(crate) worktree: ManagedWorktree,
}

/// In-flight isolated-worktree creation for one new task.
///
/// Git operations run on a bounded worker thread so the UI never blocks; the
/// cancel flag lets panel teardown ask the worker to stop early. Dropping the
/// receiver without registering the result leaves the created worktree to the
/// managed root's ordinary cleanup.
pub(crate) struct PendingTaskCreation {
    pub(crate) receiver: Receiver<Result<PreparedTask, String>>,
    cancel: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}

impl Drop for PendingTaskCreation {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

/// Start creating one isolated task worktree off the UI thread.
///
/// The source command's recorded cwd anchors the repository lookup; the
/// worker resolves the repository root, creates a `frost/task-<token>` branch
/// worktree under the per-user data directory, and returns everything the UI
/// thread needs to register the task atomically.
pub(crate) fn begin_worktree_creation(
    context: SemanticCommandContext,
    provider: AgentProvider,
) -> Result<PendingTaskCreation, String> {
    let cwd = context
        .cwd
        .as_deref()
        .filter(|cwd| !cwd.is_empty())
        .map(PathBuf::from)
        .ok_or_else(|| "source command has no working directory".to_string())?;
    let command = context.command.as_deref().unwrap_or("failed command");
    let title = format!(
        "Fix {}",
        visible_bounded(command, MAX_TASK_TITLE_DISPLAY_BYTES)
    );
    begin_task_worktree(cwd, title, provider, Some(context), None)
}

/// Authored work remains unstarted after registration for every provider.
pub(crate) fn begin_authored_creation(
    task: crate::agent_task_workspace::AuthoredTask,
) -> Result<PendingTaskCreation, String> {
    begin_task_worktree(
        task.project,
        task.title,
        task.provider,
        None,
        Some(task.brief),
    )
}

fn begin_task_worktree(
    cwd: PathBuf,
    title: String,
    provider: AgentProvider,
    context: Option<SemanticCommandContext>,
    brief: Option<crate::agent_task::TaskBrief>,
) -> Result<PendingTaskCreation, String> {
    let worktree_root = dirs::data_local_dir()
        .ok_or_else(|| "cannot locate the per-user data directory".to_string())?
        .join("frost")
        .join("agent-tasks");
    let token = uuid::Uuid::new_v4().simple().to_string();
    let task_name = format!("task-{token}");
    let branch = format!("frost/{task_name}");
    let (sender, receiver) = mpsc::sync_channel(1);
    let cancel = Arc::new(AtomicBool::new(false));
    let worker_cancel = Arc::clone(&cancel);
    let worker = std::thread::Builder::new()
        .name("frost-task-worktree".to_string())
        .spawn(move || {
            let result = (|| {
                let service = WorktreeService::new(worktree_root)
                    .map_err(|error| error.to_string())?
                    .with_cancel_flag(worker_cancel);
                let repository = service
                    .resolve_repository_root(&cwd)
                    .map_err(|error| error.to_string())?;
                let request = crate::agent_task::CreateWorktreeRequest::new(
                    repository, task_name, branch, "HEAD",
                );
                let worktree = service
                    .create(&request)
                    .map_err(|error| error.to_string())?;
                Ok(PreparedTask {
                    context,
                    brief,
                    title,
                    provider,
                    worktree,
                })
            })();
            let _ = sender.send(result);
        })
        .map_err(|error| format!("could not start task worktree worker: {error}"))?;
    Ok(PendingTaskCreation {
        receiver,
        cancel,
        worker: Some(worker),
    })
}

/// True when `path` names an interactive jsh build, including
/// version-suffixed binaries. Anything resolving to another basename is not
/// treated as jsh.
fn is_interactive_jsh(path: &std::path::Path) -> bool {
    let resolved = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let Some(name) = resolved.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    name == "jsh" || name.starts_with("jsh-") || name.starts_with("jsh.")
}

/// Resolve the shell captured by the source pane and return an explicit argv
/// for a single user-approved validation command.
///
/// Validation runs in a fresh process inside the task worktree. Passing the
/// command as one argv element (rather than interpolating it into a wrapper
/// script) preserves its exact shell syntax and avoids a second quoting
/// language. Command mode deliberately is not login mode: a login profile may
/// change directory after the PTY has entered the validated worktree, causing
/// the command to run against unrelated files. Supported shells also receive
/// their no-rc flag; unknown shell families fail closed because their
/// non-interactive startup contract is not known.
pub(crate) fn validation_command_argv(
    source_shell: Option<&str>,
    command: &str,
) -> Result<Vec<String>, String> {
    use std::ffi::OsStr;
    use std::path::Path;

    let source_shell = source_shell
        .filter(|shell| !shell.is_empty())
        .ok_or_else(|| "Validation source shell identity is missing".to_string())?;
    let shell = jterm_core::host::resolve_configured_program(source_shell, None)
        .ok_or_else(|| format!("Validation source shell is no longer executable: {source_shell}"))?
        .to_string_lossy()
        .into_owned();
    if is_interactive_jsh(Path::new(&shell)) {
        return Ok(vec![
            shell,
            "--norc".to_string(),
            "-c".to_string(),
            command.to_string(),
        ]);
    }
    let resolved = std::fs::canonicalize(&shell).unwrap_or_else(|_| Path::new(&shell).into());
    let family = resolved
        .file_name()
        .and_then(OsStr::to_str)
        .unwrap_or_default()
        .to_ascii_lowercase();
    let mut argv = vec![shell];
    match family.as_str() {
        "bash" => argv.extend(["--noprofile".to_string(), "--norc".to_string()]),
        "zsh" => argv.push("-f".to_string()),
        "fish" => argv.push("--no-config".to_string()),
        "sh" | "dash" | "ksh" | "ksh93" | "mksh" => {}
        _ => {
            return Err(format!(
                "Unsupported source shell for isolated validation: {}",
                resolved.display()
            ));
        }
    }
    argv.extend(["-c".to_string(), command.to_string()]);
    Ok(argv)
}

/// Environment overrides that keep a validation child from sourcing startup
/// files even when the shell family is only partially covered by argv flags.
pub(crate) const VALIDATION_ENV_OVERRIDES: [(&str, &str); 3] = [
    ("BASH_ENV", "/dev/null"),
    ("ENV", "/dev/null"),
    ("ZDOTDIR", "/dev/null"),
];

/// One immutable provider-choice opening. Retained callbacks cannot consume
/// a later context even when the same failed command is opened again.
pub(crate) struct TaskProviderPicker {
    pub(crate) context: SemanticCommandContext,
    identity: Arc<()>,
}

impl TaskProviderPicker {
    pub(crate) fn new(context: SemanticCommandContext) -> Self {
        Self {
            context,
            identity: Arc::new(()),
        }
    }

    pub(crate) fn reference(&self) -> Arc<()> {
        self.identity.clone()
    }

    pub(crate) fn retire_reference(&mut self) {
        self.identity = Arc::new(());
    }

    pub(crate) fn take_current(
        picker: &mut Option<Self>,
        reference: &Arc<()>,
    ) -> Option<SemanticCommandContext> {
        if !picker
            .as_ref()
            .is_some_and(|current| Arc::ptr_eq(&current.identity, reference))
        {
            return None;
        }
        picker.take().map(|current| current.context)
    }
}

/// View state for the Tasks dashboard dock panel.
/// A rendered composer belongs to one selected task and one completed turn.
/// Retained Arc identity also distinguishes close/reopen and A→B→A selection.
#[derive(Clone, Debug)]
pub(crate) struct TaskFollowUpRef {
    pub(crate) task_id: TaskId,
    pub(crate) completed_turns: usize,
    selection: Arc<()>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct TaskRepositoryChoice(pub(crate) Option<PathBuf>);

impl std::fmt::Display for TaskRepositoryChoice {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.0 {
            Some(path) => formatter.write_str(&jterm_core::review_input::safe_inline_display(
                &path.to_string_lossy(),
                256,
            )),
            None => formatter.write_str("All projects"),
        }
    }
}

pub(crate) struct TaskPanel {
    pub(crate) selected: Option<TaskId>,
    /// Narrow docks show either the list or inspector; wide docks show both.
    /// The selected identity and its draft remain independent of this view.
    pub(crate) inspector_open: bool,
    /// List hidden (archived) tasks instead of the active workspace. The
    /// only lifecycle action offered there is Restore.
    pub(crate) show_hidden: bool,
    /// A text field in the visible Tasks dock has keyboard focus, as last
    /// reported by [`report_input_focus`]. It owns typing until it blurs.
    pub(crate) input_focused: bool,
    /// The user's own deny, reply, finish or cancel requests a short burst
    /// of fast runtime polls (see [`TaskPanel::request_prompt_poll`]).
    prompt_poll_until: Option<std::time::Instant>,
    pub(crate) dashboard_query: String,
    /// Shared-parser explanation for `dashboard_query`, kept with it so a
    /// frame does not parse the query again just to show the error.
    dashboard_query_error: Option<String>,
    pub(crate) dashboard_query_notice: Option<&'static str>,
    pub(crate) dashboard_view: crate::agent_task::DashboardView,
    pub(crate) dashboard_attention: Option<crate::agent_task::DashboardAttentionKind>,
    pub(crate) dashboard_search_help: bool,
    pub(crate) dashboard_filters_open: bool,
    pub(crate) dashboard_provider: Option<AgentProvider>,
    pub(crate) dashboard_repo: Option<PathBuf>,
    pub(crate) dashboard_sort: crate::agent_task::DashboardSort,
    /// Draft review feedback for the selected task's next native turn.
    pub(crate) follow_up: String,
    follow_up_epoch: Arc<()>,
    follow_up_notice: Option<&'static str>,
    draft_discard_confirmation: Option<Arc<()>>,
    follow_up_drafts: std::collections::HashMap<TaskId, (String, Option<&'static str>)>,
    pub(crate) pending_creation: Option<PendingTaskCreation>,
    /// Failed-block context waiting for an explicit provider choice.
    pub(crate) provider_picker: Option<TaskProviderPicker>,
    pub(crate) composer: Option<crate::agent_task_workspace::TaskComposer>,
    pub(crate) composer_open: bool,
    /// Bounded read-only `git status`/`git diff` surface for the selected
    /// task's worktree (worker-owned; polled from the iced tick).
    pub(crate) diff: crate::agent_task::AgentDiffPanel,
    diff_task: Option<TaskId>,
    diff_epoch: Arc<()>,
}

impl TaskPanel {
    pub(crate) fn new() -> Self {
        Self {
            selected: None,
            inspector_open: false,
            show_hidden: false,
            input_focused: false,
            prompt_poll_until: None,
            dashboard_query: String::new(),
            dashboard_query_error: None,
            dashboard_query_notice: None,
            dashboard_view: crate::agent_task::DashboardView::All,
            dashboard_attention: None,
            dashboard_search_help: false,
            dashboard_filters_open: false,
            dashboard_provider: None,
            dashboard_repo: None,
            dashboard_sort: crate::agent_task::DashboardSort::default(),
            follow_up: String::new(),
            follow_up_epoch: Arc::new(()),
            follow_up_notice: None,
            draft_discard_confirmation: None,
            follow_up_drafts: std::collections::HashMap::new(),
            pending_creation: None,
            provider_picker: None,
            composer: None,
            composer_open: false,
            diff: crate::agent_task::AgentDiffPanel::new(),
            diff_task: None,
            diff_epoch: Arc::new(()),
        }
    }

    pub(crate) fn open_dashboard_result(
        &mut self,
        dashboard: &crate::agent_task::DashboardSnapshot,
    ) {
        if self.dashboard_query_notice.is_some() {
            return;
        }
        if let Some(row) = dashboard.rows.first() {
            if self.selected != Some(row.id) {
                self.select(Some(row.id));
            }
            self.inspector_open = true;
        }
    }

    pub(crate) fn step_dashboard(
        &mut self,
        dashboard: &crate::agent_task::DashboardSnapshot,
        forward: bool,
    ) {
        if let Some(next) = dashboard.step_selection(self.selected, forward) {
            if self.selected != Some(next) {
                self.select(Some(next));
            }
            self.inspector_open = true;
        }
    }

    pub(crate) fn next_attention(&mut self, dashboard: &crate::agent_task::DashboardSnapshot) {
        self.step_attention(dashboard, true);
    }

    pub(crate) fn previous_attention(&mut self, dashboard: &crate::agent_task::DashboardSnapshot) {
        self.step_attention(dashboard, false);
    }

    fn step_attention(&mut self, dashboard: &crate::agent_task::DashboardSnapshot, forward: bool) {
        if let Some(next) = dashboard.step_attention(self.selected, forward) {
            if self.selected != Some(next) {
                self.select(Some(next));
            }
            self.inspector_open = true;
        }
    }

    /// Clear the filters that narrow the current list. The Hidden view uses
    /// only the search, provider, project and sort, so there it leaves the
    /// workspace's quick view and attention reason untouched (see
    /// [`TaskPanel::set_show_hidden`]).
    pub(crate) fn clear_dashboard_filters(&mut self) {
        self.retire_panel_callbacks();
        self.dashboard_query.clear();
        self.dashboard_query_error = None;
        self.dashboard_query_notice = None;
        if !self.show_hidden {
            self.dashboard_view = crate::agent_task::DashboardView::All;
            self.dashboard_attention = None;
        }
        self.dashboard_provider = None;
        self.dashboard_repo = None;
        self.dashboard_sort = crate::agent_task::DashboardSort::default();
    }

    /// Whether any filter narrows the current list, which is when Clear
    /// filters is offered. Quick views and attention reasons do not apply
    /// to the Hidden view and do not count there.
    pub(crate) fn has_dashboard_filters(&self) -> bool {
        !self.dashboard_query.is_empty()
            || self.dashboard_provider.is_some()
            || self.dashboard_repo.is_some()
            || self.dashboard_sort != crate::agent_task::DashboardSort::default()
            || (!self.show_hidden
                && (self.dashboard_view != crate::agent_task::DashboardView::All
                    || self.dashboard_attention.is_some()))
    }

    pub(crate) fn set_dashboard_view(&mut self, view: crate::agent_task::DashboardView) {
        self.retire_panel_callbacks();
        self.dashboard_view = view;
        self.dashboard_attention = None;
    }

    pub(crate) fn set_dashboard_query(&mut self, query: String) {
        self.retire_panel_callbacks();
        // Keep the complete grammar for the shared parser. Truncating before
        // parsing could silently drop an exclusion and broaden the results.
        // Text over core's 256-byte limit is kept and explained by core with
        // zero matches; only an implausible paste is refused whole.
        if query.len() > 4096 {
            self.dashboard_query_notice = Some(
                "Search paste exceeds 4 KiB. Previous query kept; edit it before pressing Enter.",
            );
            return;
        }
        self.dashboard_query_error = crate::agent_task::dashboard_query_error(&query);
        self.dashboard_query = query;
        self.dashboard_query_notice = None;
    }

    /// Core's explanation for the current search text, parsed once per edit.
    pub(crate) fn dashboard_query_error(&self) -> Option<&str> {
        self.dashboard_query_error.as_deref()
    }

    /// Switch between the active workspace and hidden (archived) tasks. The
    /// search, provider, project and sort stay; quick views and attention
    /// reasons apply only to the active workspace and are left untouched.
    pub(crate) fn set_show_hidden(&mut self, show: bool) {
        if self.show_hidden != show {
            self.retire_panel_callbacks();
            self.show_hidden = show;
        }
    }

    /// The user's own deny, reply, finish or cancel is handled by a runtime
    /// worker. Core no longer asks for fast polling while a native task waits
    /// for an approval or a reply, so poll quickly for a short window to show
    /// the outcome promptly instead of waiting for the slow lifecycle tick.
    pub(crate) fn request_prompt_poll(&mut self) {
        self.prompt_poll_until = Some(std::time::Instant::now() + PROMPT_POLL_WINDOW);
    }

    pub(crate) fn prompt_poll_active(&self) -> bool {
        self.prompt_poll_until
            .is_some_and(|until| std::time::Instant::now() < until)
    }

    pub(crate) fn set_dashboard_attention(
        &mut self,
        kind: crate::agent_task::DashboardAttentionKind,
    ) {
        self.retire_panel_callbacks();
        self.dashboard_view = crate::agent_task::DashboardView::All;
        self.dashboard_attention = Some(kind);
    }

    /// Quick views and attention categories narrow the same searched scope.
    /// The caller retains that full scope for count badges and project filters.
    pub(crate) fn dashboard_from_scope(
        &self,
        scope: &crate::agent_task::DashboardSnapshot,
    ) -> crate::agent_task::DashboardSnapshot {
        if self.show_hidden {
            // Hidden tasks never match a lifecycle quick view or attention
            // reason; list every searched hidden task in the chosen order.
            return scope.view(
                crate::agent_task::DashboardView::All,
                None,
                self.dashboard_sort,
            );
        }
        scope.view(
            self.dashboard_view,
            self.dashboard_attention,
            self.dashboard_sort,
        )
    }

    pub(crate) fn select(&mut self, task_id: Option<TaskId>) {
        self.close_diff();
        // Remove the destination before parking the current draft, so moving
        // between retained drafts never temporarily exceeds the budget.
        let restored = task_id.and_then(|id| self.follow_up_drafts.remove(&id));
        if let Some(previous) = self.selected {
            if !self.follow_up.is_empty() && task_id != self.selected {
                self.follow_up_drafts.insert(
                    previous,
                    (
                        std::mem::take(&mut self.follow_up),
                        self.follow_up_notice.take(),
                    ),
                );
            }
        }
        if task_id != self.selected {
            let (draft, notice) = restored.unwrap_or_default();
            self.follow_up = draft;
            self.follow_up_notice = notice;
        }
        self.selected = task_id;
        self.inspector_open = task_id.is_some();
        self.retire_follow_up();
    }

    pub(crate) fn show_task_list(&mut self) {
        self.inspector_open = false;
        self.retire_panel_callbacks();
    }

    pub(crate) fn show_task_inspector(&mut self) {
        self.inspector_open = self.selected.is_some();
        self.retire_panel_callbacks();
    }

    pub(crate) fn inspector_is_visible(
        &self,
        task_id: TaskId,
        has_columns: bool,
        dashboard: &crate::agent_task::DashboardSnapshot,
    ) -> bool {
        self.selected == Some(task_id)
            && (self.inspector_open || has_columns)
            && dashboard.selected_index(Some(task_id)).is_some()
    }

    /// Hiding a diff never drops its receiver: an old worker retains the
    /// single-flight slot until poll retires it, but cannot regain visibility.
    pub(crate) fn close_diff(&mut self) {
        self.diff.is_open = false;
        self.diff_task = None;
        self.diff_epoch = Arc::new(());
    }

    pub(crate) fn diff_reference(&self) -> Arc<()> {
        self.diff_epoch.clone()
    }

    pub(crate) fn close_current_diff(&mut self, reference: &Arc<()>) {
        if Arc::ptr_eq(&self.diff_epoch, reference) {
            self.close_diff();
        }
    }

    pub(crate) fn owns_visible_diff(&self, task_id: TaskId) -> bool {
        self.diff.is_open && self.selected == Some(task_id) && self.diff_task == Some(task_id)
    }

    pub(crate) fn request_diff(
        &mut self,
        task_id: TaskId,
        cwd: std::path::PathBuf,
        base: String,
    ) -> Result<(), crate::agent_task::DiffRequestError> {
        let result = self.diff.request_from(cwd, base);
        // Spawn failure has an admitted, visible error state. Busy/invalid
        // requests must not relabel the previous task's retained result.
        if result.is_ok()
            || matches!(
                &result,
                Err(crate::agent_task::DiffRequestError::WorkerSpawn(_))
            )
        {
            self.diff_task = Some(task_id);
            self.diff_epoch = Arc::new(());
        }
        result
    }

    /// Panel visibility transitions revoke both composer and picker callbacks,
    /// preserving the current draft/context for the next visible opening.
    pub(crate) fn retire_panel_callbacks(&mut self) {
        self.retire_follow_up();
        self.diff_epoch = Arc::new(());
        if let Some(picker) = self.provider_picker.as_mut() {
            picker.retire_reference();
        }
        if let Some(composer) = self.composer.as_mut() {
            composer.retire_reference();
        }
    }

    pub(crate) fn review_reference(&self) -> Arc<()> {
        self.follow_up_epoch.clone()
    }

    pub(crate) fn accepts_review(&self, task_id: TaskId, reference: &Arc<()>) -> bool {
        self.selected == Some(task_id) && Arc::ptr_eq(&self.follow_up_epoch, reference)
    }

    /// Hiding the composer revokes callbacks but keeps its editable draft.
    pub(crate) fn retire_follow_up(&mut self) {
        self.follow_up_epoch = Arc::new(());
        self.draft_discard_confirmation = None;
    }

    pub(crate) fn follow_up_reference(
        &self,
        task_id: TaskId,
        completed_turns: usize,
    ) -> TaskFollowUpRef {
        TaskFollowUpRef {
            task_id,
            completed_turns,
            selection: self.follow_up_epoch.clone(),
        }
    }

    pub(crate) fn accepts_follow_up(
        &self,
        reference: &TaskFollowUpRef,
        completed_turns: usize,
    ) -> bool {
        self.selected == Some(reference.task_id)
            && reference.completed_turns == completed_turns
            && Arc::ptr_eq(&self.follow_up_epoch, &reference.selection)
    }

    pub(crate) fn edit_follow_up(
        &mut self,
        reference: &TaskFollowUpRef,
        completed_turns: usize,
        text: String,
    ) -> bool {
        if !self.accepts_follow_up(reference, completed_turns) {
            return false;
        }
        self.set_follow_up(text);
        true
    }

    /// Consume callback identity before dispatch, retaining the draft until
    /// success. Even a failed attempt cannot be repeated by a queued duplicate;
    /// the user can retry through the newly rendered composer identity.
    pub(crate) fn begin_follow_up_send(
        &mut self,
        reference: &TaskFollowUpRef,
        completed_turns: usize,
    ) -> Option<String> {
        if !self.accepts_follow_up(reference, completed_turns)
            || !native_follow_up_can_send(&self.follow_up, completed_turns)
        {
            return None;
        }
        let text = self.follow_up.clone();
        self.retire_follow_up();
        Some(text)
    }

    pub(crate) fn saved_draft_count(&self) -> usize {
        self.follow_up_drafts.len()
    }

    pub(crate) fn draft_cleanup_reference(&self) -> Arc<()> {
        self.follow_up_epoch.clone()
    }

    pub(crate) fn request_discard_saved_drafts(&mut self, reference: &Arc<()>) {
        if Arc::ptr_eq(reference, &self.follow_up_epoch) && !self.follow_up_drafts.is_empty() {
            self.draft_discard_confirmation = Some(Arc::new(()));
        }
    }

    pub(crate) fn draft_discard_confirmation(&self) -> Option<Arc<()>> {
        self.draft_discard_confirmation.clone()
    }

    pub(crate) fn cancel_discard_saved_drafts(&mut self, reference: &Arc<()>) {
        if self
            .draft_discard_confirmation
            .as_ref()
            .is_some_and(|current| Arc::ptr_eq(current, reference))
        {
            self.draft_discard_confirmation = None;
        }
    }

    pub(crate) fn discard_saved_drafts(&mut self, reference: &Arc<()>) {
        if self
            .draft_discard_confirmation
            .as_ref()
            .is_some_and(|current| Arc::ptr_eq(current, reference))
        {
            self.follow_up_drafts.clear();
            // The selected draft and its truncation/rejection notice are not
            // part of this explicitly confirmed parked-draft deletion.
            self.retire_follow_up();
        }
    }

    pub(crate) fn clear_follow_up(&mut self) {
        self.follow_up = String::new();
        self.follow_up_notice = None;
    }

    pub(crate) fn follow_up_feedback(&self, completed_turns: usize) -> String {
        let reason = if self.follow_up.is_empty() && self.follow_up_drafts.len() >= 16 {
            "Draft limit reached (16 drafts / 256 KiB). Open a saved draft to clear/send it, or discard saved drafts below."
        } else if completed_turns >= CODEX_APP_SERVER_LIVE_TURN_MAX {
            "Turn limit reached."
        } else if let Some(notice) = self.follow_up_notice {
            notice
        } else if !native_follow_up_can_send(&self.follow_up, completed_turns) {
            "Enter visible review feedback to send."
        } else {
            "Ready to send."
        };
        format!(
            "{} / {} bytes · {} / {} turns · {}",
            self.follow_up.len(),
            NATIVE_AGENT_FOLLOW_UP_MAX_BYTES,
            completed_turns,
            CODEX_APP_SERVER_LIVE_TURN_MAX,
            reason
        )
    }

    pub(crate) fn set_follow_up(&mut self, text: impl Into<String>) {
        let raw = text.into();
        // Existing drafts remain editable and navigable at capacity; only a
        // seventeenth draft is refused. No user text is silently evicted.
        if self.follow_up.is_empty() && self.follow_up_drafts.len() >= 16 && !raw.is_empty() {
            self.follow_up_notice = Some("Draft limit reached. Input rejected; saved drafts kept.");
            return;
        }
        // Check the entire paste before bounding it: an unsafe tail must not
        // disappear at the byte limit and silently turn rejection into sending.
        if raw.chars().any(|ch| {
            ch == '\u{fffd}'
                || (!ch.is_control() && jterm_core::review_input::is_visual_spoofing_character(ch))
        }) {
            self.follow_up_notice =
                Some("Input rejected: invisible or misleading characters. Draft kept.");
            return;
        }
        let clean_bytes: usize = raw
            .chars()
            .filter(|ch| matches!(ch, '\n' | '\t') || !ch.is_control())
            .map(char::len_utf8)
            .sum();
        let controls_removed = raw
            .chars()
            .any(|ch| ch.is_control() && !matches!(ch, '\n' | '\t'));
        self.follow_up = bound_follow_up(raw);
        self.follow_up_notice = if clean_bytes > NATIVE_AGENT_FOLLOW_UP_MAX_BYTES {
            Some("Input truncated to the byte limit; review before sending.")
        } else if controls_removed {
            Some("Terminal control characters removed; review before sending.")
        } else {
            None
        };
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn task_workspace_inspector_preserves_drafts_and_retires_hidden_actions() {
        let mut panel = super::TaskPanel::new();
        let id = super::TaskId::new();
        panel.select(Some(id));
        panel.set_follow_up("Keep this exact review feedback");
        let old_review = panel.review_reference();
        let old_send = panel.follow_up_reference(id, 2);
        panel.diff_task = Some(id);
        panel.diff.is_open = true;
        let old_close = panel.diff_reference();
        assert!(panel.inspector_open);

        panel.show_task_list();
        assert!(!panel.inspector_open);
        assert_eq!(panel.selected, Some(id));
        assert_eq!(panel.follow_up, "Keep this exact review feedback");
        assert!(!panel.accepts_review(id, &old_review));
        assert!(!panel.accepts_follow_up(&old_send, 2));
        panel.close_current_diff(&old_close);
        assert!(
            panel.owns_visible_diff(id),
            "navigation preserves the diff and rejects its old close callback"
        );

        panel.show_task_inspector();
        assert!(panel.inspector_open);
        assert_eq!(panel.selected, Some(id));
        assert_eq!(panel.follow_up, "Keep this exact review feedback");
        assert!(!panel.accepts_review(id, &old_review));
        panel.select(None);
        panel.show_task_inspector();
        assert!(!panel.inspector_open);
    }

    #[test]
    fn attention_filters_preserve_query_scope_and_never_select_hidden_tasks() {
        use crate::agent_task::{
            DashboardAttentionKind, DashboardFilter, DashboardSnapshot, DashboardView, NewTask,
            TaskManager, TaskStatus,
        };
        let mut manager = TaskManager::new();
        for _ in 0..3 {
            manager
                .create(NewTask {
                    title: "Same name".into(),
                    provider: super::AgentProvider::Codex,
                    repo_root: "/fixture".into(),
                    worktree_path: "/fixture/worktree".into(),
                    branch: "test/workspace".into(),
                    base_commit: "a".repeat(40),
                    source_context: None,
                })
                .unwrap();
        }
        let mut tasks = manager.tasks().to_vec();
        tasks[0].status = TaskStatus::WaitingForApproval;
        tasks[1].status = TaskStatus::WaitingForHuman;
        tasks[2].status = TaskStatus::Failed;
        let scope =
            DashboardSnapshot::project(&tasks, |_| Default::default(), DashboardFilter::default());
        let mut panel = super::TaskPanel::new();
        panel.dashboard_query = "provider:codex \"Same name\"".into();
        panel.dashboard_provider = Some(super::AgentProvider::Codex);
        panel.dashboard_repo = Some("/fixture".into());
        panel.select(Some(tasks[0].id));
        panel.set_follow_up("feedback owned by the approval task");
        panel.set_dashboard_attention(DashboardAttentionKind::Input);
        let visible = panel.dashboard_from_scope(&scope);
        assert_eq!(visible.rows.len(), 1);
        assert_eq!(visible.rows[0].id, tasks[1].id);
        assert_eq!(visible.counts.total, 3);
        assert_eq!(panel.selected, Some(tasks[0].id));
        assert!(visible.selected_index(panel.selected).is_none());
        assert!(!panel.inspector_is_visible(tasks[0].id, true, &visible));
        assert_eq!(panel.dashboard_query, "provider:codex \"Same name\"");
        assert_eq!(panel.dashboard_provider, Some(super::AgentProvider::Codex));
        assert_eq!(
            panel.dashboard_repo.as_deref(),
            Some(std::path::Path::new("/fixture"))
        );
        panel.next_attention(&visible);
        assert_eq!(panel.selected, Some(tasks[1].id));
        assert!(panel.inspector_is_visible(tasks[1].id, false, &visible));
        panel.show_task_list();
        assert!(!panel.inspector_is_visible(tasks[1].id, false, &visible));
        assert!(panel.inspector_is_visible(tasks[1].id, true, &visible));
        assert!(!panel.inspector_is_visible(tasks[0].id, true, &visible));
        panel.set_dashboard_query("x".repeat(4097));
        panel.open_dashboard_result(&visible);
        assert!(
            !panel.inspector_open,
            "rejected paste must not turn Enter into opening an older query"
        );
        panel.set_dashboard_query("Same name".into());
        panel.open_dashboard_result(&visible);
        assert!(panel.inspector_open);
        panel.select(Some(tasks[0].id));
        assert_eq!(panel.follow_up, "feedback owned by the approval task");
        panel.set_dashboard_view(DashboardView::Running);
        assert!(panel.dashboard_attention.is_none());
        panel.set_dashboard_attention(DashboardAttentionKind::Approval);
        assert_eq!(panel.dashboard_view, DashboardView::All);
        panel.clear_dashboard_filters();
        assert!(panel.dashboard_attention.is_none());
        assert!(panel.dashboard_query.is_empty());
        assert!(manager
            .tasks()
            .iter()
            .all(|task| task.status == TaskStatus::Created));
    }

    #[test]
    fn task_workspace_columns_respect_readable_width_and_font_scale() {
        assert!(!super::task_workspace_has_columns(220.0, 14.0));
        assert!(!super::task_workspace_has_columns(450.0, 14.0));
        assert!(super::task_workspace_has_columns(500.0, 14.0));
        assert!(!super::task_workspace_has_columns(500.0, 20.0));
    }

    #[test]
    fn task_workspace_search_keeps_focus_when_inspector_disappears_and_returns() {
        use iced::advanced::{text, widget::tree, widget::Tree};
        use iced::widget::{scrollable, text as label, text_input};
        type InputState = text_input::State<<iced::Renderer as text::Renderer>::Paragraph>;

        fn input(tree: &mut Tree) -> Option<&mut InputState> {
            if tree.tag == tree::Tag::of::<InputState>() {
                Some(tree.state.downcast_mut::<InputState>())
            } else {
                tree.children.iter_mut().find_map(input)
            }
        }
        fn workspace(has_selection: bool) -> iced::Element<'static, crate::Message> {
            let search = text_input("Search tasks", "provider:")
                .on_input(crate::Message::TaskDashboardQuery);
            let inspector = if has_selection {
                crate::widget_identity_scope::scope_key(1_u8, scrollable(label("Task inspector")))
            } else {
                label("Selected task is hidden by filters").into()
            };
            super::task_workspace_layout(scrollable(search).into(), inspector, true, has_selection)
        }

        let first = workspace(true);
        let mut tree = Tree::new(first.as_widget());
        input(&mut tree).unwrap().focus();
        for has_selection in [false, true, false] {
            let next = workspace(has_selection);
            tree.diff(next.as_widget());
            assert!(input(&mut tree).unwrap().is_focused());
        }
    }

    #[test]
    fn task_search_never_silently_truncates_an_exclusion() {
        let mut panel = super::TaskPanel::new();
        let query = format!("{} -provider:codex", "review ".repeat(45));
        panel.set_dashboard_query(query.clone());
        assert_eq!(panel.dashboard_query, query);
        assert!(crate::agent_task::dashboard_query_error(&panel.dashboard_query).is_some());
        panel.set_dashboard_query("x".repeat(4097));
        assert_eq!(panel.dashboard_query, query);
        assert!(panel.dashboard_query_notice.is_some());
        panel.set_dashboard_query("project:\"my project\" -provider:claude".into());
        assert!(panel.dashboard_query_notice.is_none());
        assert!(crate::agent_task::dashboard_query_error(&panel.dashboard_query).is_none());
        panel.clear_dashboard_filters();
        assert!(panel.dashboard_query_notice.is_none());
    }

    #[test]
    fn task_reason_preview_fits_single_line_cells() {
        assert_eq!(super::reason_preview("界界界", 5), "界界…");
        assert_eq!(super::reason_preview("abcdef", 4), "abc…");
        assert_eq!(super::reason_preview("abc", 4), "abc");
        assert_eq!(super::reason_preview("abc", 0), "");
    }

    #[test]
    fn task_terminal_resolution_survives_reorder_and_rejects_replacement() {
        use crate::agent_task::{AgentProvider, NewTask, TaskManager, TaskTerminalRole};
        let mut manager = TaskManager::new();
        let mut ids = Vec::new();
        for title in ["first", "second"] {
            ids.push(
                manager
                    .create(NewTask {
                        title: title.into(),
                        provider: AgentProvider::Codex,
                        repo_root: "/fixture".into(),
                        worktree_path: "/fixture/worktree".into(),
                        branch: "test/focus".into(),
                        base_commit: "a".repeat(40),
                        source_context: None,
                    })
                    .unwrap(),
            );
        }
        manager
            .bind_terminal_session(ids[0], "terminal-first".into())
            .unwrap();
        manager
            .bind_terminal_session(ids[1], "terminal-second".into())
            .unwrap();
        manager
            .update_status(ids[1], crate::agent_task::TaskStatus::ReadyForReview, None)
            .unwrap();
        manager
            .bind_validation_session(ids[1], "validation-second".into())
            .unwrap();
        manager.handle_terminal_session_exit("validation-second", Some(0));
        let before = manager.tasks().to_vec();
        let binding =
            super::task_terminal_binding(manager.get(ids[1]).unwrap(), TaskTerminalRole::Agent);
        assert_eq!(
            super::resolve_task_terminal(
                binding,
                [
                    ("terminal-first".into(), 11),
                    ("terminal-second".into(), 22)
                ]
            ),
            Some(22)
        );
        assert_eq!(
            super::resolve_task_terminal(
                binding,
                [
                    ("terminal-second".into(), 22),
                    ("terminal-first".into(), 11)
                ]
            ),
            Some(22)
        );
        assert_eq!(
            super::resolve_task_terminal(
                binding,
                [("replacement".into(), 22), ("terminal-first".into(), 11)]
            ),
            None
        );
        let validation = super::task_terminal_binding(
            manager.get(ids[1]).unwrap(),
            TaskTerminalRole::Validation,
        );
        assert_eq!(
            super::resolve_task_terminal(validation, [("validation-second".into(), 33)]),
            Some(33),
            "finished validation output stays focusable"
        );
        let task = manager.get(ids[1]).unwrap();
        let mut live = std::collections::HashSet::from([
            "terminal-second".to_string(),
            "validation-second".to_string(),
        ]);
        assert!(super::task_terminal_is_live(
            task,
            TaskTerminalRole::Agent,
            &live
        ));
        assert!(super::task_terminal_is_live(
            task,
            TaskTerminalRole::Validation,
            &live
        ));
        live.remove("terminal-second");
        live.insert("replacement".into());
        assert!(!super::task_terminal_is_live(
            task,
            TaskTerminalRole::Agent,
            &live
        ));
        assert!(super::task_terminal_is_live(
            task,
            TaskTerminalRole::Validation,
            &live
        ));
        live.clear();
        assert!(!super::task_terminal_is_live(
            task,
            TaskTerminalRole::Validation,
            &live
        ));
        assert!(super::task_focus_allowed(true, Some(ids[1]), ids[1]));
        assert!(!super::task_focus_allowed(false, Some(ids[1]), ids[1]));
        assert!(!super::task_focus_allowed(true, Some(ids[0]), ids[1]));
        assert_eq!(format!("{:?}", manager.tasks()), format!("{before:?}"));
    }

    #[test]
    fn dashboard_filter_labels_map_activity_and_finished_counts() {
        use crate::agent_task::{DashboardCounts, DashboardView};
        let counts = DashboardCounts {
            total: 20,
            attention: 7,
            running: 9,
            created: 3,
            review: 4,
            done: 2,
            failed: 5,
            cancelled: 1,
            archived: 99,
            ..Default::default()
        };
        for (view, expected) in [
            (DashboardView::All, "All 20"),
            (DashboardView::NeedsAttention, "Needs you 7"),
            (DashboardView::Running, "Running 9"),
            (DashboardView::NotStarted, "Not started 3"),
            (DashboardView::Review, "Review 4"),
            (DashboardView::Finished, "Finished 8"),
        ] {
            assert_eq!(super::dashboard_filter_label(view, &counts), expected);
        }
        assert_eq!(
            super::dashboard_filter_label(DashboardView::Running, &DashboardCounts::default()),
            "Running 0"
        );
    }

    #[test]
    fn print_provider_notice_discloses_permissions_without_promising_a_sandbox() {
        use super::{native_start_notice, AgentProvider};
        let claude = native_start_notice(AgentProvider::Claude).unwrap();
        assert!(claude.contains("skips its own permission prompts"));
        let kimi = native_start_notice(AgentProvider::Kimi).unwrap();
        assert!(kimi.contains("automatic tool permission"));
        for notice in [claude, kimi] {
            assert!(notice.contains("user account's file access"));
            assert!(notice.contains("not a sandbox"));
        }
        assert_eq!(native_start_notice(AgentProvider::Codex), None);
        assert_eq!(native_start_notice(AgentProvider::OpenCode), None);
    }
    use super::*;

    #[test]
    fn prompt_policy_requires_both_ai_and_sharing_consent() {
        let mut config = Config::default();
        assert!(!prompt_policy(&config).share_command_context);
        config.ai_enabled = true;
        assert!(!prompt_policy(&config).share_command_context);
        config.ai_share_command_context = true;
        assert!(prompt_policy(&config).share_command_context);
        assert!(prompt_policy(&config).redact_secrets);
        config.ai_redact_secrets = false;
        assert!(!prompt_policy(&config).redact_secrets);
    }

    #[test]
    fn terminal_session_ids_match_the_jsh_grammar_and_distinguish_sessions() {
        let first = terminal_session_id(1);
        let second = terminal_session_id(2);
        assert!(jterm_core::execution_journal::is_valid_jsh_session_id(
            &first
        ));
        assert!(jterm_core::execution_journal::is_valid_jsh_session_id(
            &second
        ));
        assert_ne!(first, second);
    }

    fn picker_context(command: &str) -> SemanticCommandContext {
        SemanticCommandContext {
            source_session_id: "test".to_string(),
            source_execution_id: "test".to_string(),
            source_sequence: 1,
            source_shell: None,
            command: Some(command.to_string()),
            command_exact: true,
            command_truncated: false,
            cwd: Some("/fixture".to_string()),
            cwd_after: None,
            exit_code: Some(1),
            duration_ms: None,
            output_text: String::new(),
            output_available: true,
            output_truncated: false,
            output_total_bytes: 0,
            started_at: None,
            finished_at: None,
        }
    }

    #[test]
    fn provider_choice_and_cancel_only_consume_the_current_opening_once() {
        let mut picker = Some(TaskProviderPicker::new(picker_context("A")));
        let old_a = picker.as_ref().unwrap().reference();
        assert!(TaskProviderPicker::take_current(&mut picker, &old_a).is_some());
        assert!(TaskProviderPicker::take_current(&mut picker, &old_a).is_none());
        picker = Some(TaskProviderPicker::new(picker_context("B")));
        assert!(TaskProviderPicker::take_current(&mut picker, &old_a).is_none());
        assert_eq!(
            picker.as_ref().unwrap().context.command.as_deref(),
            Some("B")
        );
        let hidden_b = picker.as_ref().unwrap().reference();
        picker.as_mut().unwrap().retire_reference();
        assert!(TaskProviderPicker::take_current(&mut picker, &hidden_b).is_none());
        let current_b = picker.as_ref().unwrap().reference();
        let context = TaskProviderPicker::take_current(&mut picker, &current_b).unwrap();
        assert_eq!(context.command.as_deref(), Some("B"));
        // A failed availability check restores the same text as a fresh opening.
        picker = Some(TaskProviderPicker::new(context));
        assert!(TaskProviderPicker::take_current(&mut picker, &current_b).is_none());
        let retry = picker.as_ref().unwrap().reference();
        assert!(TaskProviderPicker::take_current(&mut picker, &retry).is_some());
        picker = Some(TaskProviderPicker::new(picker_context("A")));
        assert!(TaskProviderPicker::take_current(&mut picker, &old_a).is_none());
    }

    #[test]
    fn provider_choice_keeps_current_visibility_and_availability_checks() {
        let source = include_str!("main.rs");
        let method = source
            .split("fn task_create_with_provider(")
            .nth(1)
            .unwrap()
            .split("/// Enter the bounded")
            .next()
            .unwrap();
        assert!(
            method
                .find("!self.config.experimental_task_sidebar")
                .unwrap()
                < method.find("TaskProviderPicker::take_current").unwrap()
        );
        assert!(
            method.find("TaskProviderPicker::take_current").unwrap()
                < method.find("ensure_executable_available").unwrap()
        );
        assert!(
            method.find("ensure_executable_available").unwrap()
                < method.find("begin_worktree_creation").unwrap()
        );
        assert!(method.contains("TaskProviderPicker::new(context)"));
        assert!(source.contains("Message::TaskCreateWithProvider(reference.clone(), provider)"));
        assert!(source.contains("Message::TaskCreateProviderCancel(reference)"));
        let cancel = source
            .split("Message::TaskCreateProviderCancel(reference) => {")
            .nth(1)
            .unwrap()
            .split("Message::TaskStartNative")
            .next()
            .unwrap();
        assert!(cancel.contains("TaskProviderPicker::take_current"));
        for start in [
            "fn toggle_sidebar(",
            "fn sync_tab_position_ui(",
            "fn set_task_panel_open(",
        ] {
            let body = source
                .split(start)
                .nth(1)
                .unwrap()
                .split("\n    fn ")
                .next()
                .unwrap();
            assert!(body.contains("retire_panel_callbacks()"));
        }
    }

    #[test]
    fn dashboard_submit_selects_filtered_identity_without_changing_tasks() {
        use crate::agent_task::{
            DashboardFilter, DashboardSnapshot, DashboardView, NewTask, TaskManager,
        };
        let mut manager = TaskManager::new();
        let mut ids = Vec::new();
        for title in ["first", "review"] {
            ids.push(
                manager
                    .create(NewTask {
                        title: title.into(),
                        provider: AgentProvider::Codex,
                        repo_root: "/fixture".into(),
                        worktree_path: "/fixture/worktree".into(),
                        branch: "test/dashboard".into(),
                        base_commit: "a".repeat(40),
                        source_context: None,
                    })
                    .unwrap(),
            );
        }
        let before: Vec<_> = manager
            .tasks()
            .iter()
            .map(|task| (task.id, task.status, task.title.clone()))
            .collect();
        let mut panel = TaskPanel::new();
        panel.select(Some(ids[0]));
        panel.dashboard_query = "review".into();
        let filtered = DashboardSnapshot::project(
            manager.tasks(),
            |_| Default::default(),
            DashboardFilter {
                text: &panel.dashboard_query,
                ..Default::default()
            },
        );
        assert_eq!(
            panel.selected,
            Some(ids[0]),
            "typing must not move selection"
        );
        panel.open_dashboard_result(&filtered);
        assert_eq!(panel.selected, Some(ids[1]));
        let empty = DashboardSnapshot::filter_rows(&filtered.rows, "missing", DashboardView::All);
        panel.open_dashboard_result(&empty);
        assert_eq!(panel.selected, Some(ids[1]), "empty results keep selection");
        assert_eq!(
            manager
                .tasks()
                .iter()
                .map(|task| (task.id, task.status, task.title.clone()))
                .collect::<Vec<_>>(),
            before
        );
    }

    #[test]
    fn dashboard_navigation_preserves_drafts_and_never_changes_task_lifecycle() {
        use crate::agent_task::{
            DashboardFilter, DashboardSnapshot, DashboardSort, NewTask, TaskManager, TaskStatus,
        };
        let mut manager = TaskManager::new();
        for title in ["Alpha", "Beta", "Gamma"] {
            manager
                .create(NewTask {
                    title: title.into(),
                    provider: AgentProvider::Codex,
                    repo_root: "/fixture".into(),
                    worktree_path: "/fixture/worktree".into(),
                    branch: "test/dashboard".into(),
                    base_commit: "a".repeat(40),
                    source_context: None,
                })
                .unwrap();
        }
        let mut tasks = manager.tasks().to_vec();
        tasks[1].status = TaskStatus::WaitingForHuman;
        let mut dashboard =
            DashboardSnapshot::project(&tasks, |_| Default::default(), DashboardFilter::default());
        dashboard.sort_rows(DashboardSort::Title);
        let first = dashboard.rows[0].id;
        let second = dashboard.rows[1].id;
        let mut panel = TaskPanel::new();
        panel.step_dashboard(&dashboard, true);
        assert_eq!(panel.selected, Some(first));
        panel.set_follow_up("Keep my review feedback");
        panel.step_dashboard(&dashboard, true);
        assert_eq!(panel.selected, Some(second));
        assert!(panel.follow_up.is_empty());
        panel.step_dashboard(&dashboard, false);
        assert_eq!(panel.selected, Some(first));
        assert_eq!(panel.follow_up, "Keep my review feedback");
        panel.next_attention(&dashboard);
        assert_eq!(panel.selected, Some(second));
        panel.step_dashboard(&dashboard, true);
        panel.previous_attention(&dashboard);
        assert_eq!(panel.selected, Some(second));
        panel.select(Some(first));
        assert_eq!(panel.follow_up, "Keep my review feedback");
        panel.previous_attention(&dashboard);
        assert_eq!(panel.selected, Some(second));
        panel.dashboard_provider = Some(AgentProvider::Claude);
        panel.dashboard_repo = Some("/another-project".into());
        panel.dashboard_sort = DashboardSort::Recent;
        panel.clear_dashboard_filters();
        assert_eq!(panel.selected, Some(second));
        assert!(panel.dashboard_provider.is_none());
        assert!(panel.dashboard_repo.is_none());
        assert_eq!(panel.dashboard_sort, DashboardSort::Attention);
        assert!(manager
            .tasks()
            .iter()
            .all(|task| task.status == TaskStatus::Created));
    }

    #[test]
    fn task_diff_selection_and_close_do_not_relabel_late_results() {
        let mut panel = TaskPanel::new();
        let a = TaskId::new();
        let b = TaskId::new();
        panel.select(Some(a));
        panel.diff_task = Some(a);
        panel.diff.is_open = true;
        assert!(panel.owns_visible_diff(a));
        assert!(!panel.owns_visible_diff(b));
        panel.select(Some(b));
        assert!(!panel.owns_visible_diff(b));
        assert!(!panel.owns_visible_diff(a));
        panel.select(Some(a));
        assert!(!panel.owns_visible_diff(a));
        // A fresh request can become visible; closing/deletion revokes it.
        panel.diff_task = Some(a);
        panel.diff.is_open = true;
        let old_close = panel.diff_reference();
        panel.close_diff();
        assert!(!panel.owns_visible_diff(a));
        panel.diff_task = Some(a);
        panel.diff.is_open = true;
        panel.close_current_diff(&old_close);
        assert!(panel.owns_visible_diff(a));
        panel.diff_task = Some(a);
        panel.diff.is_open = true;
        panel.select(None);
        assert!(!panel.owns_visible_diff(a));
    }

    #[test]
    fn diff_production_display_and_request_are_bound_to_selected_task() {
        let source = include_str!("main.rs");
        assert!(source.contains("if self.task_panel.owns_visible_diff(task.id)"));
        let open = source
            .split("Message::TaskDiffOpen(task_id) => {")
            .nth(1)
            .unwrap()
            .split("Message::TaskDiffClose")
            .next()
            .unwrap();
        assert!(
            open.find("!self.task_inspector_visible(task_id)").unwrap()
                < open.find("request_diff(").unwrap()
        );
        let pump = source
            .split("let tasks_open =")
            .nth(1)
            .unwrap()
            .split("let interval =")
            .next()
            .unwrap();
        assert!(pump.contains("self.task_panel.diff.state().loading"));
        let ui = include_str!("agent_task_ui.rs");
        let close = ui
            .split("pub(crate) fn close_diff(&mut self) {")
            .nth(1)
            .unwrap()
            .split("pub(crate) fn diff_reference")
            .next()
            .unwrap();
        assert!(!close.contains("self.diff ="));
        assert!(!close.contains("pending"));
        let worker = include_str!("agent_task/diff.rs");
        let poll = worker
            .split("pub fn poll(&mut self) -> bool {")
            .nth(1)
            .unwrap()
            .split("/// Base revision")
            .next()
            .unwrap();
        assert!(!poll.contains("is_open = true"));
        let apply = worker
            .split("fn apply_result(&mut self, result: WorkerResult) {")
            .nth(1)
            .unwrap()
            .split("\n}")
            .next()
            .unwrap();
        assert!(!apply.contains("is_open = true"));
    }

    #[test]
    fn stale_follow_up_cannot_read_or_replace_another_tasks_draft() {
        let mut panel = TaskPanel::new();
        let a = TaskId::new();
        let b = TaskId::new();
        panel.select(Some(a));
        let old_a = panel.follow_up_reference(a, 1);
        panel.select(Some(b));
        let current_b = panel.follow_up_reference(b, 1);
        assert!(panel.edit_follow_up(&current_b, 1, "feedback for B".into()));
        assert!(!panel.edit_follow_up(&old_a, 1, "late A input".into()));
        assert_eq!(panel.begin_follow_up_send(&old_a, 1), None);
        assert_eq!(panel.follow_up, "feedback for B");
        assert_eq!(
            panel.begin_follow_up_send(&current_b, 1).as_deref(),
            Some("feedback for B")
        );
        // Dispatch failure keeps the draft, but a queued duplicate cannot retry.
        assert_eq!(panel.follow_up, "feedback for B");
        assert_eq!(panel.begin_follow_up_send(&current_b, 1), None);
        assert!(!panel.edit_follow_up(&current_b, 1, "late edit".into()));
        let retry = panel.follow_up_reference(b, 1);
        assert!(panel.begin_follow_up_send(&retry, 1).is_some());
        panel.follow_up.clear(); // Existing handler clears only after success.
        assert_eq!(panel.begin_follow_up_send(&retry, 1), None);
    }

    #[test]
    fn follow_up_selection_visibility_and_turn_changes_revoke_callbacks() {
        let mut panel = TaskPanel::new();
        let a = TaskId::new();
        let b = TaskId::new();
        panel.select(Some(a));
        let first = panel.follow_up_reference(a, 1);
        panel.select(Some(b));
        panel.select(Some(a));
        assert!(!panel.accepts_follow_up(&first, 1));
        let reopened = panel.follow_up_reference(a, 1);
        assert!(panel.edit_follow_up(&reopened, 1, "keep draft".into()));
        panel.retire_follow_up(); // Close/reopen keeps text, revokes old view.
        assert!(!panel.accepts_follow_up(&reopened, 1));
        assert_eq!(panel.follow_up, "keep draft");
        let current = panel.follow_up_reference(a, 1);
        assert!(panel.accepts_follow_up(&current, 1));
        assert!(!panel.accepts_follow_up(&current, 2));
        assert_eq!(panel.begin_follow_up_send(&current, 2), None);
        panel.select(None); // Removed/hidden selection.
        assert!(!panel.accepts_follow_up(&current, 1));
    }

    #[test]
    fn production_follow_up_admission_precedes_dispatch_and_draft_clear() {
        let source = include_str!("main.rs");
        let send = source
            .split("Message::TaskFollowUpSend(reference) => {")
            .nth(1)
            .unwrap()
            .split("Message::TaskApprovalDeny")
            .next()
            .unwrap();
        let guard = send.find("task_follow_up_is_current").unwrap();
        let take = send.find("begin_follow_up_send").unwrap();
        let dispatch = send.find("prompt_codex").unwrap();
        let clear = send
            .find("Ok(()) => self.task_panel.clear_follow_up()")
            .unwrap();
        assert!(guard < take && take < dispatch && dispatch < clear);
        let input = source
            .split("Message::TaskFollowUpInput(reference, value) => {")
            .nth(1)
            .unwrap()
            .split("Message::TaskFollowUpSend")
            .next()
            .unwrap();
        assert!(
            input.find("task_follow_up_is_current").unwrap()
                < input.find("edit_follow_up").unwrap()
        );
        assert!(source.contains("Message::TaskFollowUpInput(input_reference.clone(), value)"));
        assert!(source.contains("Message::TaskFollowUpSend(reference)"));
        let direct_assignment = regex::Regex::new(r"self\.task_panel\.selected\s*=[^=]").unwrap();
        assert!(direct_assignment.is_match("self.task_panel.selected = Some(id);"));
        assert!(!direct_assignment.is_match("self.task_panel.selected == Some(id)"));
        assert!(!direct_assignment.is_match(source));
    }

    #[test]
    fn follow_up_drafts_and_notices_follow_task_identity() {
        let mut panel = TaskPanel::new();
        let a = TaskId::new();
        let b = TaskId::new();
        panel.select(Some(a));
        panel.set_follow_up("A draft");
        let stale = panel.follow_up_reference(a, 1);
        panel.select(Some(b));
        assert!(panel.follow_up.is_empty());
        panel.set_follow_up("B\rdraft");
        assert!(panel.follow_up_feedback(1).contains("removed"));
        panel.select(Some(a));
        assert_eq!(panel.follow_up, "A draft");
        assert!(!panel.follow_up_feedback(1).contains("removed"));
        assert!(!panel.edit_follow_up(&stale, 1, "stale".into()));
        assert_eq!(panel.begin_follow_up_send(&stale, 1), None);
        let send = panel.follow_up_reference(a, 1);
        assert_eq!(
            panel.begin_follow_up_send(&send, 1).as_deref(),
            Some("A draft")
        );
        // Failed dispatch preserves the draft but retires the attempt.
        assert_eq!(panel.begin_follow_up_send(&send, 1), None);
        panel.select(Some(b));
        assert_eq!(panel.follow_up, "Bdraft");
        assert!(panel.follow_up_feedback(1).contains("removed"));
        panel.select(Some(a));
        assert_eq!(panel.follow_up, "A draft");
        let retry = panel.follow_up_reference(a, 1);
        assert!(panel.begin_follow_up_send(&retry, 1).is_some());
        panel.clear_follow_up(); // Successful synchronous runtime admission.
        panel.select(Some(b));
        panel.select(Some(a));
        assert!(panel.follow_up.is_empty());
        assert!(panel.follow_up_feedback(1).contains("Enter visible"));
    }

    #[test]
    fn follow_up_feedback_reports_bounds_and_rejects_unsafe_tail() {
        let mut panel = TaskPanel::new();
        panel.set_follow_up("keep\n\tthis");
        for unsafe_character in ['\u{202e}', '\u{e0000}', '\u{fffd}', '\u{00a0}'] {
            panel.set_follow_up(format!(
                "{}{}",
                "x".repeat(NATIVE_AGENT_FOLLOW_UP_MAX_BYTES),
                unsafe_character
            ));
            assert_eq!(panel.follow_up, "keep\n\tthis");
            assert!(panel.follow_up_feedback(1).contains("rejected"));
        }
        panel.set_follow_up("界".repeat(NATIVE_AGENT_FOLLOW_UP_MAX_BYTES));
        assert!(panel.follow_up.len() <= NATIVE_AGENT_FOLLOW_UP_MAX_BYTES);
        assert!(panel.follow_up.ends_with('界'));
        assert!(panel.follow_up_feedback(1).contains("truncated"));
        assert!(panel.follow_up_feedback(1).contains(&format!(
            "{} / {} bytes",
            panel.follow_up.len(),
            NATIVE_AGENT_FOLLOW_UP_MAX_BYTES
        )));
        panel.set_follow_up("review\r\n\tthis");
        assert_eq!(panel.follow_up, "review\n\tthis");
        assert!(panel.follow_up_feedback(1).contains("removed"));
        assert!(panel
            .follow_up_feedback(CODEX_APP_SERVER_LIVE_TURN_MAX)
            .contains("Turn limit"));
        assert!(!native_follow_up_can_send(
            &panel.follow_up,
            CODEX_APP_SERVER_LIVE_TURN_MAX
        ));
        panel.set_follow_up("\n\t ");
        assert!(panel.follow_up_feedback(1).contains("Enter visible"));
    }

    #[test]
    fn follow_up_large_paste_does_not_retain_large_allocation() {
        let mut panel = TaskPanel::new();
        let a = TaskId::new();
        let b = TaskId::new();
        panel.select(Some(a));
        panel.set_follow_up("x".repeat(8 * 1024 * 1024));
        assert_eq!(panel.follow_up.len(), NATIVE_AGENT_FOLLOW_UP_MAX_BYTES);
        assert_eq!(panel.follow_up.capacity(), panel.follow_up.len());
        panel.select(Some(b));
        assert_eq!(
            panel.follow_up_drafts[&a].0.capacity(),
            NATIVE_AGENT_FOLLOW_UP_MAX_BYTES
        );
        panel.select(Some(a));
        panel.clear_follow_up();
        assert_eq!(panel.follow_up.capacity(), 0);
        panel.select(Some(b));
        assert!(!panel.follow_up_drafts.contains_key(&a));
    }

    #[test]
    fn follow_up_storage_is_bounded_without_silent_eviction() {
        let mut panel = TaskPanel::new();
        for _ in 0..10_000 {
            panel.select(Some(TaskId::new()));
        }
        assert!(panel.follow_up_drafts.is_empty());
        let ids: Vec<_> = (0..16).map(|_| TaskId::new()).collect();
        for id in &ids {
            panel.select(Some(*id));
            panel.set_follow_up("x".repeat(NATIVE_AGENT_FOLLOW_UP_MAX_BYTES));
        }
        let refused = TaskId::new();
        panel.select(Some(refused));
        assert_eq!(panel.selected, Some(refused));
        assert!(panel.follow_up.is_empty());
        panel.set_follow_up("seventeenth draft");
        assert!(panel.follow_up.is_empty());
        assert!(panel.follow_up_feedback(1).contains("Draft limit reached"));
        assert_eq!(panel.follow_up_drafts.len(), 16);
        let total_capacity = panel.follow_up.capacity()
            + panel
                .follow_up_drafts
                .values()
                .map(|(text, _)| text.capacity())
                .sum::<usize>();
        assert!(total_capacity <= 256 * 1024);
        let stale_cleanup = panel.draft_cleanup_reference();
        // Existing drafts remain accessible/editable at capacity.
        panel.select(Some(ids[0]));
        assert_eq!(panel.follow_up.len(), NATIVE_AGENT_FOLLOW_UP_MAX_BYTES);
        panel.set_follow_up("edited existing draft");
        assert_eq!(panel.follow_up, "edited existing draft");
        panel.discard_saved_drafts(&stale_cleanup);
        assert_eq!(panel.saved_draft_count(), 15);
        panel.clear_follow_up();
        panel.select(Some(refused));
        panel.set_follow_up("now admitted");
        assert_eq!(panel.follow_up, "now admitted");
        let cleanup = panel.draft_cleanup_reference();
        panel.request_discard_saved_drafts(&cleanup);
        let confirmation = panel.draft_discard_confirmation().unwrap();
        panel.discard_saved_drafts(&confirmation);
        assert_eq!(panel.saved_draft_count(), 0);
        assert_eq!(panel.follow_up, "now admitted");
        panel.select(None); // Archive/hide must not be blocked by draft state.
        assert_eq!(panel.selected, None);
    }

    #[test]
    fn saved_draft_discard_requires_current_confirmation_and_keeps_current_notice() {
        let mut panel = TaskPanel::new();
        let a = TaskId::new();
        let b = TaskId::new();
        panel.select(Some(a));
        panel.set_follow_up("saved draft");
        panel.select(Some(b));
        panel.set_follow_up("current\rdraft");
        let notice = panel.follow_up_notice;
        let request = panel.draft_cleanup_reference();
        panel.discard_saved_drafts(&request);
        assert_eq!(panel.saved_draft_count(), 1); // First click cannot delete.
        panel.request_discard_saved_drafts(&request);
        let cancelled = panel.draft_discard_confirmation().unwrap();
        panel.cancel_discard_saved_drafts(&cancelled);
        panel.discard_saved_drafts(&cancelled);
        assert_eq!(panel.saved_draft_count(), 1);
        panel.request_discard_saved_drafts(&request);
        let stale = panel.draft_discard_confirmation().unwrap();
        panel.retire_follow_up(); // Selection, visibility and send epochs use this.
        panel.discard_saved_drafts(&stale);
        assert_eq!(panel.saved_draft_count(), 1);
        panel.request_discard_saved_drafts(&request);
        assert!(panel.draft_discard_confirmation().is_none());
        let current = panel.draft_cleanup_reference();
        panel.request_discard_saved_drafts(&current);
        let confirmed = panel.draft_discard_confirmation().unwrap();
        panel.discard_saved_drafts(&confirmed);
        assert_eq!(panel.saved_draft_count(), 0);
        assert_eq!(panel.follow_up, "currentdraft");
        assert_eq!(panel.follow_up_notice, notice);
        assert!(panel.follow_up_feedback(1).contains("removed"));
        assert!(panel.draft_discard_confirmation().is_none());
        panel.select(Some(a));
        panel.set_follow_up("new saved draft");
        panel.select(Some(b));
        panel.discard_saved_drafts(&confirmed);
        assert_eq!(panel.saved_draft_count(), 1); // Duplicate cannot clear new drafts.
    }

    #[test]
    fn follow_up_gate_bounds_text_and_turn_count() {
        assert!(!native_follow_up_can_send("", 0));
        assert!(!native_follow_up_can_send("  \n\t ", 0));
        assert!(native_follow_up_can_send("please adjust the fix", 0));
        assert!(!native_follow_up_can_send(
            "x".repeat(NATIVE_AGENT_FOLLOW_UP_MAX_BYTES + 1).as_str(),
            0
        ));
        assert!(native_follow_up_can_send(
            "x".repeat(NATIVE_AGENT_FOLLOW_UP_MAX_BYTES).as_str(),
            CODEX_APP_SERVER_LIVE_TURN_MAX - 1
        ));
        assert!(!native_follow_up_can_send(
            "ok",
            CODEX_APP_SERVER_LIVE_TURN_MAX
        ));
        assert!(!native_follow_up_can_send("please\n\u{fffd}adjust", 0));
        assert!(!native_follow_up_can_send("please\n\u{202e}adjust", 0));
    }

    #[test]
    fn follow_up_composer_keeps_newlines_and_truncates() {
        assert_eq!(bound_follow_up("please\n\u{1b}adjust"), "please\nadjust");
        assert_eq!(bound_follow_up("a\tb"), "a\tb");
        let filled = bound_follow_up(format!("{}y", "x".repeat(NATIVE_AGENT_FOLLOW_UP_MAX_BYTES)));
        assert_eq!(filled.len(), NATIVE_AGENT_FOLLOW_UP_MAX_BYTES);
        assert!(!filled.contains('y'));
        let mut panel = TaskPanel::new();
        panel.set_follow_up("ok\n\u{07}go");
        assert_eq!(panel.follow_up, "ok\ngo");
        panel.set_follow_up("keep me");
        panel.set_follow_up("please\n\u{202e}adjust");
        assert_eq!(panel.follow_up, "keep me");
        assert!(!panel.follow_up.contains('\u{202e}'));
        assert!(!panel.follow_up.contains('\u{fffd}'));
    }

    #[test]
    fn validation_argv_uses_non_login_no_rc_command_mode() {
        let bash = validation_command_argv(Some("/bin/bash"), "cargo test")
            .expect("bash is a supported validation shell");
        assert_eq!(&bash[1..], ["--noprofile", "--norc", "-c", "cargo test"]);

        let sh = validation_command_argv(Some("/bin/sh"), "cargo test")
            .expect("sh is a supported validation shell");
        assert_eq!(&sh[1..], ["-c", "cargo test"]);

        assert!(validation_command_argv(None, "cargo test").is_err());
        assert!(validation_command_argv(Some(""), "cargo test").is_err());
        assert!(validation_command_argv(Some("/nonexistent-shell"), "cargo test").is_err());
    }

    fn fixture_task(manager: &mut crate::agent_task::TaskManager, title: &str) -> TaskId {
        manager
            .create(crate::agent_task::NewTask {
                title: title.into(),
                provider: AgentProvider::Codex,
                repo_root: "/fixture".into(),
                worktree_path: "/fixture/worktree".into(),
                branch: "test/hidden".into(),
                base_commit: "a".repeat(40),
                source_context: None,
            })
            .unwrap()
    }

    fn project_scope(
        manager: &crate::agent_task::TaskManager,
        panel: &TaskPanel,
    ) -> crate::agent_task::DashboardSnapshot {
        let scope = crate::agent_task::DashboardSnapshot::project_unsorted(
            manager
                .tasks()
                .iter()
                .filter(|task| task_in_dashboard_scope(task, panel.show_hidden)),
            |_| Default::default(),
            crate::agent_task::DashboardFilter {
                text: &panel.dashboard_query,
                ..Default::default()
            },
        );
        panel.dashboard_from_scope(&scope)
    }

    #[test]
    fn hidden_view_lists_only_hidden_tasks_and_restore_returns_them() {
        use crate::agent_task::{DashboardAttentionKind, DashboardView, TaskManager, TaskStatus};
        let mut manager = TaskManager::new();
        let visible = fixture_task(&mut manager, "Visible work");
        let hidden = fixture_task(&mut manager, "Hidden work");
        manager
            .update_status(hidden, TaskStatus::ReadyForReview, None)
            .unwrap();
        assert_eq!(manager.can_archive(hidden), Ok(()));
        manager.archive(hidden).unwrap();
        assert_eq!(manager.archived_count(), 1);

        let mut panel = TaskPanel::new();
        panel.set_dashboard_attention(DashboardAttentionKind::Review);
        let active = project_scope(&manager, &panel);
        assert!(active.selected_index(Some(hidden)).is_none());

        // The Hidden view ignores lifecycle quick views and attention reasons
        // (hidden tasks match none of them) but keeps the user's search.
        panel.set_dashboard_query("is:archived".into());
        panel.set_show_hidden(true);
        let listed = project_scope(&manager, &panel);
        assert_eq!(listed.rows.len(), 1);
        assert_eq!(listed.rows[0].id, hidden);
        assert_eq!(panel.dashboard_query, "is:archived");
        assert_eq!(
            panel.dashboard_attention,
            Some(DashboardAttentionKind::Review)
        );
        panel.select(Some(hidden));
        assert!(panel.inspector_is_visible(hidden, false, &listed));

        manager.restore(hidden).unwrap();
        assert_eq!(
            manager.get(hidden).unwrap().status,
            TaskStatus::ReadyForReview
        );
        assert_eq!(manager.archived_count(), 0);
        assert!(project_scope(&manager, &panel).rows.is_empty());
        panel.set_show_hidden(false);
        panel.set_dashboard_view(DashboardView::All);
        panel.clear_dashboard_filters();
        let back = project_scope(&manager, &panel);
        assert!(back.selected_index(Some(hidden)).is_some());
        assert!(back.selected_index(Some(visible)).is_some());
        assert_eq!(
            panel.selected,
            Some(hidden),
            "the restored task stays selected"
        );
    }

    #[test]
    fn hidden_view_clear_filters_keeps_the_workspace_quick_view() {
        use crate::agent_task::{DashboardAttentionKind, DashboardView};
        let mut panel = TaskPanel::new();
        panel.set_dashboard_view(DashboardView::Running);
        panel.set_dashboard_attention(DashboardAttentionKind::Approval);
        panel.set_show_hidden(true);
        // Quick views and attention reasons do not narrow the Hidden view,
        // so they neither offer Clear filters there nor get cleared by it.
        assert!(!panel.has_dashboard_filters());
        panel.set_dashboard_query("release".into());
        assert!(panel.has_dashboard_filters());
        panel.clear_dashboard_filters();
        assert!(panel.dashboard_query.is_empty());
        assert!(!panel.has_dashboard_filters());
        assert_eq!(
            panel.dashboard_attention,
            Some(DashboardAttentionKind::Approval)
        );
        // In the workspace they are filters again, and Clear filters clears
        // them.
        panel.set_show_hidden(false);
        assert!(panel.has_dashboard_filters());
        panel.clear_dashboard_filters();
        assert_eq!(panel.dashboard_view, DashboardView::All);
        assert_eq!(panel.dashboard_attention, None);
        assert!(!panel.has_dashboard_filters());
    }

    #[test]
    fn every_way_of_showing_tasks_or_creating_one_leaves_the_hidden_view() {
        let source = include_str!("main.rs");
        let body = |start: &str, end: &str| {
            source
                .split_once(start)
                .unwrap_or_else(|| panic!("{start}"))
                .1
                .split_once(end)
                .unwrap_or_else(|| panic!("{end}"))
                .0
                .to_owned()
        };
        for (start, end) in [
            // Toggle, the library's Tasks button, composer and block "Create
            // task" all open the dock through this setter.
            (
                "    fn set_task_panel_open(",
                "    /// Show or hide the Tasks dock panel",
            ),
            (
                "    fn toggle_sidebar(",
                "        self.sidebar_open = !self.sidebar_open;",
            ),
            (
                "            Message::SetSidebarPanel(panel) => {",
                "                self.sidebar_panel = panel;",
            ),
            // A registered task is selected in the workspace.
            (
                "                    Ok(task_id) => {",
                "                        self.task_panel.select(Some(task_id));",
            ),
            // Restore shows the restored task in the workspace.
            (
                "            Message::TaskRestore(task_id) => {",
                "            Message::TaskInputFocus(",
            ),
        ] {
            assert!(
                body(start, end).contains("self.task_panel.set_show_hidden(false);"),
                "{start}"
            );
        }
        // Nothing hidden remains: the view (and its toggle) closes itself.
        let tick = body(
            "    fn tasks_tick(&mut self) {",
            "    fn task_terminal_index(",
        );
        assert!(tick.contains(
            "if self.task_panel.show_hidden && self.task_manager.archived_count() == 0 {"
        ));
        // Clear filters is offered only for filters that narrow this list.
        assert!(source.contains("if self.task_panel.has_dashboard_filters() {"));
    }

    #[test]
    fn focused_task_fields_pass_app_commands_but_never_terminal_bindings() {
        use crate::keybindings::Command as C;
        for command in [
            C::TerminalSendSigint,
            C::TerminalSendEof,
            C::TerminalClear,
            C::TerminalScrollUp,
            C::EditPaste,
            C::EditCopy,
            C::BlockSelectNext,
            C::BlockReinputSelectedCommands,
            C::BlockRetryFailed,
            C::TerminalPromptPrev,
        ] {
            assert!(crate::command_targets_terminal(&command), "{command}");
        }
        for command in [
            C::SessionNew,
            C::SessionClose,
            C::SessionNext,
            C::TaskToggle,
            C::SidebarToggle,
            C::AgentSessions,
            C::FontZoomIn,
            C::FontZoomReset,
            C::PaneFocusNext,
            C::SearchOpen,
            C::ConfigToggle,
        ] {
            assert!(!crate::command_targets_terminal(&command), "{command}");
        }
    }

    #[test]
    fn hide_is_offered_only_when_the_manager_accepts_it() {
        use crate::agent_task::{TaskError, TaskManager};
        let mut manager = TaskManager::new();
        let id = fixture_task(&mut manager, "Busy");
        manager
            .bind_terminal_session(id, "busy-terminal".into())
            .unwrap();
        let refused = manager.can_archive(id).unwrap_err();
        assert_eq!(refused, TaskError::CannotArchiveRunning(id));
        assert!(hide_blocked_reason(&refused).contains("after the Agent"));
        assert!(manager.archive(id).is_err());
        manager.handle_terminal_session_exit("busy-terminal", Some(0));
        assert_eq!(manager.can_archive(id), Ok(()));
        let unknown = TaskId::new();
        let reason = hide_blocked_reason(&manager.can_archive(unknown).unwrap_err());
        assert!(!reason.is_empty() && reason.len() <= MAX_TASK_DETAIL_DISPLAY_BYTES);
    }

    #[test]
    fn attention_badge_counts_and_explains_tasks_needing_the_user() {
        use crate::agent_task::{TaskManager, TaskStatus};
        let mut manager = TaskManager::new();
        assert_eq!(attention_badge(manager.attention_counts()), None);
        let approval = fixture_task(&mut manager, "Approval");
        let reply = fixture_task(&mut manager, "Reply");
        let quiet = fixture_task(&mut manager, "Quiet");
        manager
            .update_status(approval, TaskStatus::WaitingForApproval, None)
            .unwrap();
        manager
            .update_status(reply, TaskStatus::WaitingForHuman, None)
            .unwrap();
        assert_eq!(
            attention_badge(manager.attention_counts()),
            Some((2, "2 tasks need you: 1 approval, 1 reply".to_owned()))
        );
        manager.archive(quiet).unwrap();
        assert_eq!(attention_badge(manager.attention_counts()).unwrap().0, 2);
    }

    #[test]
    fn user_runtime_actions_request_a_bounded_fast_poll_burst() {
        let mut panel = TaskPanel::new();
        assert!(!panel.prompt_poll_active());
        panel.request_prompt_poll();
        assert!(panel.prompt_poll_active());
        panel.prompt_poll_until =
            Some(std::time::Instant::now() - std::time::Duration::from_millis(1));
        assert!(!panel.prompt_poll_active(), "the burst expires on its own");
        let source = include_str!("main.rs");
        let pump = source
            .split("let tasks_open =")
            .nth(1)
            .unwrap()
            .split("if self.agent_sessions.open")
            .next()
            .unwrap();
        assert!(pump.contains("|| self.task_panel.prompt_poll_active()"));
        for (start, end) in [
            (
                "Message::TaskCancelNative(task_id) => {",
                "Message::TaskFinishNative",
            ),
            (
                "Message::TaskFinishNative(task_id) => {",
                "Message::AgentLaunch",
            ),
            (
                "Message::TaskFollowUpSend(reference) => {",
                "Message::TaskApprovalDeny",
            ),
            (
                "Message::TaskApprovalDeny(task_id, approval_id) => {",
                "Message::TaskDiffOpen",
            ),
        ] {
            let body = source
                .split(start)
                .nth(1)
                .unwrap()
                .split(end)
                .next()
                .unwrap();
            assert!(
                body.contains("self.task_panel.request_prompt_poll();"),
                "{start} must poll promptly after the user's own action"
            );
        }
    }

    #[test]
    fn dashboard_query_error_is_parsed_once_per_edit() {
        let mut panel = TaskPanel::new();
        panel.set_dashboard_query("status:nonsense".into());
        assert_eq!(
            panel.dashboard_query_error().map(str::to_owned),
            crate::agent_task::dashboard_query_error("status:nonsense")
        );
        // Over core's 256-byte limit: kept whole and explained, not truncated.
        let long = format!("{} -provider:codex", "x".repeat(300));
        panel.set_dashboard_query(long.clone());
        assert_eq!(panel.dashboard_query, long);
        assert!(panel.dashboard_query_error().is_some());
        panel.clear_dashboard_filters();
        assert!(panel.dashboard_query_error().is_none());
        let view = include_str!("main.rs")
            .split("    fn sidebar_tasks_view(")
            .nth(1)
            .unwrap()
            .split("\n    fn ")
            .next()
            .unwrap();
        assert!(!view.contains("agent_task::dashboard_query_error("));
        assert!(!view.contains("let tasks: Vec<_>"));
    }

    #[test]
    fn focus_report_publishes_changes_and_clears_after_the_field_disappears() {
        use iced::advanced::widget::{operation, Id, Tree};
        use iced::advanced::{layout, Layout, Shell};
        use iced::widget::{button, column, text_input};
        use iced::{Element, Rectangle, Size};

        #[derive(Clone, Debug, PartialEq)]
        enum Msg {
            Input(String),
            Focus(bool),
            Press,
        }
        let id = Id::new("task-search-test");
        let view = |reported: bool, with_input: bool| -> Element<'static, Msg, iced::Theme, ()> {
            let mut content = column![button("Next").on_press(Msg::Press)];
            if with_input {
                content = content.push(
                    text_input("Search", "query")
                        .id(id.clone())
                        .on_input(Msg::Input),
                );
            }
            report_input_focus(content, reported, Msg::Focus)
        };
        let viewport = Rectangle::with_size(Size::new(400.0, 200.0));
        let redraw = iced::Event::Window(iced::window::Event::RedrawRequested(
            std::time::Instant::now(),
        ));
        let mut element = view(false, true);
        let mut tree = Tree::new(element.as_widget());
        let mut node = element.as_widget_mut().layout(
            &mut tree,
            &(),
            &layout::Limits::new(Size::ZERO, viewport.size()),
        );
        let mut messages = Vec::new();
        let update = |element: &mut Element<'static, Msg, iced::Theme, ()>,
                      tree: &mut Tree,
                      node: &layout::Node,
                      event: &iced::Event,
                      messages: &mut Vec<Msg>| {
            element.as_widget_mut().update(
                tree,
                event,
                Layout::new(node),
                iced::mouse::Cursor::Unavailable,
                &(),
                &mut iced::advanced::clipboard::Null,
                &mut Shell::new(messages),
                &viewport,
            );
        };
        update(&mut element, &mut tree, &node, &redraw, &mut messages);
        assert!(messages.is_empty(), "nothing is focused yet");
        element.as_widget_mut().operate(
            &mut tree,
            Layout::new(&node),
            &(),
            &mut operation::focusable::focus(id.clone()),
        );
        update(&mut element, &mut tree, &node, &redraw, &mut messages);
        update(&mut element, &mut tree, &node, &redraw, &mut messages);
        assert_eq!(messages, [Msg::Focus(true)], "one report per change");

        // The app adopted the report; an unchanged state stays quiet.
        let mut element = view(true, true);
        tree.diff(element.as_widget());
        node = element.as_widget_mut().layout(
            &mut tree,
            &(),
            &layout::Limits::new(Size::ZERO, viewport.size()),
        );
        messages.clear();
        update(&mut element, &mut tree, &node, &redraw, &mut messages);
        assert!(messages.is_empty());
        // A cursor move cannot change focus and does not probe.
        update(
            &mut element,
            &mut tree,
            &node,
            &iced::Event::Mouse(iced::mouse::Event::CursorMoved {
                position: iced::Point::ORIGIN,
            }),
            &mut messages,
        );
        assert!(messages.is_empty());

        // The focused field disappears with its card: the stale flag clears.
        let mut element = view(true, false);
        tree.diff(element.as_widget());
        node = element.as_widget_mut().layout(
            &mut tree,
            &(),
            &layout::Limits::new(Size::ZERO, viewport.size()),
        );
        update(&mut element, &mut tree, &node, &redraw, &mut messages);
        assert_eq!(messages, [Msg::Focus(false)]);
    }

    #[test]
    fn focused_task_fields_keep_unconsumed_keys_from_the_terminal() {
        let source = include_str!("main.rs");
        let input = source
            .split("    fn terminal_input_active(")
            .nth(1)
            .unwrap()
            .split("    fn chat_input_ready(")
            .next()
            .unwrap();
        assert!(input.contains("&& !self.task_input_owns_keys()"));
        let keys = source
            .split("            Message::Key(event) => {")
            .nth(1)
            .unwrap()
            .split("            Message::MousePane(")
            .next()
            .unwrap();
        // A terminal-targeting binding is dropped before any handler runs;
        // block selection is skipped; app commands, chrome shortcuts and
        // scrolling run as usual; and a hard stop precedes the PTY write.
        let owner = keys
            .find("let task_input = self.task_input_owns_keys();")
            .unwrap();
        let dropped = keys
            .find(".is_some_and(|command| command_targets_terminal(&command))")
            .unwrap();
        let blocks = keys.find(".filter(|_| !task_input)").unwrap();
        let block_context = keys.find("let block_key_context = self").unwrap();
        let binding = keys
            .find("if let Some(task) = self.handle_keybinding(&key, modifiers, retreat_before_key)")
            .unwrap();
        let chrome = keys
            .find("self.handle_tab_shortcut(&key, modifiers, retreat_before_key)")
            .unwrap();
        let scroll = keys
            .find("if self.handle_scroll_shortcut(&key, modifiers) {")
            .unwrap();
        let stop = keys
            .find("if task_input {\n                        return Task::none();\n                    }")
            .unwrap();
        let pty = keys
            .find("encode_key(&key, location, modifiers, text.as_deref(), app_cursor, enh)")
            .unwrap();
        assert!(owner < dropped && dropped < block_context && block_context < blocks);
        assert!(
            blocks
                < keys
                    .find("let selection_owns_keys = block_key_context")
                    .unwrap()
        );
        assert!(blocks < binding && binding < chrome && chrome < scroll);
        assert!(scroll < stop && stop < pty);
        // The composer owns typing too, but its documented toggles work.
        let composer = keys
            .split("if self.task_composer_visible() {")
            .nth(1)
            .unwrap()
            .split("if self.agent_sessions.open {")
            .next()
            .unwrap();
        assert!(composer.contains("keybindings::Command::TaskToggle"));
        assert!(composer.contains("if let Some(task) = self.dispatch_command(command) {"));
        let owner = source
            .split("    fn task_input_owns_keys(")
            .nth(1)
            .unwrap()
            .split("\n    }\n")
            .next()
            .unwrap();
        for required in [
            "self.task_panel.input_focused",
            "self.sidebar_panel == SidebarPanel::Tasks",
            "self.dock_open()",
        ] {
            assert!(owner.contains(required), "{required}");
        }
        assert!(source.contains(
            "agent_task_ui::report_input_focus(\n                self.sidebar_tasks_view(),"
        ));
    }
}

/// A bounded, redacted title and a stable ID prefix; never use prompt/output text.
fn feedback_identity(manager: &crate::agent_task::TaskManager, id: TaskId) -> String {
    let full = id.to_string();
    let peers: Vec<_> = manager
        .tasks()
        .iter()
        .filter(|task| task.id != id)
        .map(|task| task.id.to_string())
        .collect();
    let length = feedback_id_prefix_length(&full, &peers);
    let title = manager
        .get(id)
        .map(|task| feedback_title(&task.title))
        .unwrap_or_else(|| "Task no longer available".into());
    format!("{title} [{}]", &full[..length])
}

fn feedback_id_prefix_length(full: &str, peers: &[String]) -> usize {
    let mut length = 8;
    while length < full.len() && peers.iter().any(|peer| peer.starts_with(&full[..length])) {
        length = (length + 4).min(full.len());
    }
    length
}

fn feedback_title(title: &str) -> String {
    // Redact before bounding so a secret cannot evade detection at the cut point.
    let redacted = jterm_core::redact::redact_secrets(title);
    visible_bounded(
        &redacted.split_whitespace().collect::<Vec<_>>().join(" "),
        64,
    )
}

pub(crate) fn task_completion_feedback(
    manager: &crate::agent_task::TaskManager,
    completions: impl IntoIterator<Item = (TaskId, crate::agent_task::AgentSessionOutcome)>,
) -> Option<String> {
    use crate::agent_task::AgentSessionOutcome;
    let completions: Vec<_> = completions.into_iter().collect();
    let (id, outcome) = *completions.first()?;
    if completions.len() > 1 {
        let failed = completions
            .iter()
            .filter(|(_, outcome)| *outcome == AgentSessionOutcome::Failed)
            .count();
        let cancelled = completions
            .iter()
            .filter(|(_, outcome)| *outcome == AgentSessionOutcome::Cancelled)
            .count();
        return Some(format!("{} agent sessions stopped: {} clean, {cancelled} cancelled, {failed} failed; open Tasks for details", completions.len(), completions.len() - failed - cancelled));
    }
    let outcome = match outcome {
        AgentSessionOutcome::Clean => {
            "Agent session stopped cleanly; review changes, then validate"
        }
        AgentSessionOutcome::Cancelled => "Agent session cancelled and stopped",
        AgentSessionOutcome::Failed => "Agent session failed; open Tasks for details",
    };
    Some(format!("{}: {outcome}", feedback_identity(manager, id)))
}

pub(crate) fn task_validation_feedback(
    manager: &crate::agent_task::TaskManager,
    id: TaskId,
    session: &str,
) -> Option<String> {
    use crate::agent_task::TaskValidationStatus;
    let task = manager.get(id)?;
    let validation = &task.validation;
    if validation.terminal_session_id.as_deref() != Some(session) {
        return None;
    }
    let outcome = match validation.status {
        TaskValidationStatus::NotRun | TaskValidationStatus::Running => return None,
        TaskValidationStatus::Passed => "passed".into(),
        TaskValidationStatus::Failed => validation
            .exit_code
            .map(|code| format!("failed (exit {code})"))
            .unwrap_or_else(|| "failed; open Tasks for details".into()),
        TaskValidationStatus::Cancelled => "cancelled".into(),
        TaskValidationStatus::Inconclusive => {
            "ended without a confirmed result; open Tasks for details".into()
        }
    };
    Some(format!(
        "{}: Validation #{} {outcome}",
        feedback_identity(manager, id),
        validation.attempt
    ))
}

/// Apply the reducer once, and announce only an accepted current-role transition.
/// A reducer return value alone is insufficient: repeated exits also return an ID.
pub(crate) fn task_terminal_exit_feedback(
    manager: &mut crate::agent_task::TaskManager,
    session: &str,
    exit_code: Option<i32>,
) -> Option<String> {
    use crate::agent_task::{
        AgentSessionOutcome, TaskStatus, TaskTerminalRole, TaskValidationStatus,
    };
    let before = manager
        .task_and_role_for_terminal_session(session)
        .map(|(task, role)| {
            (
                task.id,
                role,
                task.status,
                task.validation.status,
                task.validation.attempt,
                task_terminal_binding(task, role) == Some(session),
            )
        });
    let accepted = manager.handle_terminal_session_exit(session, exit_code);
    let (id, role, status, validation_status, attempt, bound) = before?;
    if !bound || accepted != Some(id) {
        return None;
    }
    let task = manager.get(id)?;
    if task_terminal_binding(task, role) != Some(session) {
        return None;
    }
    match role {
        TaskTerminalRole::Validation => {
            if validation_status != TaskValidationStatus::Running
                || task.validation.attempt != attempt
                || task.validation.status == validation_status
            {
                return None;
            }
            task_validation_feedback(manager, id, session)
        }
        TaskTerminalRole::Agent => {
            if task.status == status {
                return None;
            }
            let outcome = match task.status {
                TaskStatus::ReadyForReview => AgentSessionOutcome::Clean,
                TaskStatus::Failed => AgentSessionOutcome::Failed,
                _ => return None,
            };
            task_completion_feedback(manager, [(id, outcome)])
        }
    }
}

#[cfg(test)]
mod feedback_tests {
    use super::*;
    use crate::agent_task::{AgentSessionOutcome, NewTask, TaskManager, TaskStatus};

    fn add_task(manager: &mut TaskManager, title: &str) -> TaskId {
        manager
            .create(NewTask {
                title: title.into(),
                provider: AgentProvider::Codex,
                repo_root: "/fixture".into(),
                worktree_path: "/fixture/worktree".into(),
                branch: "test/feedback".into(),
                base_commit: "a".repeat(40),
                source_context: None,
            })
            .unwrap()
    }

    #[test]
    fn completion_feedback_identifies_same_title_tasks_and_missing_task() {
        let mut manager = TaskManager::new();
        let a = add_task(&mut manager, "Same title");
        let b = add_task(&mut manager, "Same title");
        let message =
            |id| task_completion_feedback(&manager, [(id, AgentSessionOutcome::Failed)]).unwrap();
        assert_ne!(message(a), message(b));
        assert!(message(a).contains(&a.to_string()[..8]));
        assert!(message(b).contains(&b.to_string()[..8]));
        let missing = TaskId::new();
        assert!(message(missing).contains("Task no longer available"));
        assert!(message(missing).contains(&missing.to_string()[..8]));
        assert!(task_completion_feedback(&manager, []).is_none());
        let batch = task_completion_feedback(
            &manager,
            [
                (a, AgentSessionOutcome::Clean),
                (b, AgentSessionOutcome::Cancelled),
                (a, AgentSessionOutcome::Failed),
            ],
        )
        .unwrap();
        assert_eq!(
            batch,
            "3 agent sessions stopped: 1 clean, 1 cancelled, 1 failed; open Tasks for details"
        );
        assert!(!batch.contains("3 tasks"));
    }

    #[test]
    fn feedback_id_prefix_extends_for_live_collisions() {
        let a = "12345678-1111-4111-8111-111111111111";
        let b = "12345678-2222-4222-8222-222222222222";
        assert_eq!(feedback_id_prefix_length(a, &[]), 8);
        let length = feedback_id_prefix_length(a, &[b.into()]);
        assert_eq!(length, 12);
        assert_ne!(&a[..length], &b[..length]);
    }

    #[test]
    fn feedback_title_is_redacted_single_line_and_utf8_bounded() {
        let secret = format!("sk-{}", "a".repeat(32));
        assert!(!feedback_title(&format!("Fix {secret}")).contains(&secret));
        let hostile = feedback_title("one\n\t\u{202e}two\u{2066}\u{1b}[31m");
        assert!(!hostile.chars().any(char::is_control));
        assert!(!hostile.contains('\u{202e}'));
        assert!(!hostile.contains('\u{2066}'));
        assert!(hostile.contains("one "));
        let long = feedback_title(&"界🙂".repeat(80));
        assert!(long.len() <= 64);
        assert!(long.is_char_boundary(long.len()));
        assert!(long.ends_with('…'));
    }

    #[test]
    fn validation_feedback_uses_bound_attempt_and_stored_result() {
        let mut manager = TaskManager::new();
        let a = add_task(&mut manager, "Same title");
        let b = add_task(&mut manager, "Same title");
        for (id, session) in [(a, "agent-a"), (b, "agent-b")] {
            manager.bind_terminal_session(id, session.into()).unwrap();
            manager
                .update_status(id, TaskStatus::ReadyForReview, None)
                .unwrap();
        }
        manager
            .bind_validation_session(a, "validation-a".into())
            .unwrap();
        manager
            .bind_validation_session(b, "validation-b".into())
            .unwrap();
        assert!(task_validation_feedback(&manager, a, "validation-a").is_none());
        manager.handle_terminal_session_exit("validation-a", Some(7));
        assert_eq!(
            manager.handle_terminal_session_closed("validation-a"),
            Some(a)
        );
        let failed = task_validation_feedback(&manager, a, "validation-a").unwrap();
        assert!(failed.contains(&a.to_string()[..8]));
        assert!(failed.ends_with("Validation #1 failed (exit 7)"));
        assert!(task_validation_feedback(&manager, b, "validation-a").is_none());
        manager
            .bind_validation_session(a, "validation-a-retry".into())
            .unwrap();
        assert!(manager
            .handle_terminal_session_exit("validation-a", Some(0))
            .is_none());
        assert!(task_validation_feedback(&manager, a, "validation-a").is_none());
        assert!(task_validation_feedback(&manager, a, "validation-a-retry").is_none());
        manager.handle_terminal_session_exit("validation-a-retry", Some(0));
        assert_eq!(
            manager.handle_terminal_session_closed("validation-a-retry"),
            Some(a)
        );
        assert!(task_validation_feedback(&manager, b, "validation-b").is_none());
        assert!(task_validation_feedback(&manager, a, "validation-a-retry")
            .unwrap()
            .ends_with("Validation #2 passed"));
        assert!(task_validation_feedback(&TaskManager::new(), a, "validation-a-retry").is_none());
    }

    #[test]
    fn terminal_feedback_only_announces_current_accepted_transitions() {
        for (code, result) in [
            (Some(0), "passed"),
            (Some(9), "failed (exit 9)"),
            (
                None,
                "ended without a confirmed result; open Tasks for details",
            ),
        ] {
            let mut manager = TaskManager::new();
            let id = add_task(&mut manager, "Validate");
            manager.bind_terminal_session(id, "agent".into()).unwrap();
            let stopped = task_terminal_exit_feedback(&mut manager, "agent", Some(0)).unwrap();
            assert!(
                stopped.contains("Agent session stopped cleanly; review changes, then validate")
            );
            assert!(task_terminal_exit_feedback(&mut manager, "agent", Some(7)).is_none());
            manager
                .bind_validation_session(id, "validation-old".into())
                .unwrap();
            let message =
                task_terminal_exit_feedback(&mut manager, "validation-old", code).unwrap();
            assert!(message.ends_with(&format!("Validation #1 {result}")));
            assert!(task_terminal_exit_feedback(&mut manager, "validation-old", Some(0)).is_none());
            assert_eq!(manager.get(id).unwrap().validation.exit_code, code);
            manager
                .bind_validation_session(id, "validation-new".into())
                .unwrap();
            assert!(task_terminal_exit_feedback(&mut manager, "validation-old", Some(0)).is_none());
            assert_eq!(
                manager.get(id).unwrap().validation.status,
                crate::agent_task::TaskValidationStatus::Running
            );
            assert!(
                task_terminal_exit_feedback(&mut manager, "validation-new", Some(0))
                    .unwrap()
                    .ends_with("Validation #2 passed")
            );
            assert!(task_terminal_exit_feedback(&mut manager, "unknown", Some(0)).is_none());
        }
    }

    #[test]
    fn terminal_feedback_does_not_reannounce_finished_or_cancelled_tasks() {
        for status in [
            TaskStatus::ReadyForReview,
            TaskStatus::Completed,
            TaskStatus::Failed,
            TaskStatus::Cancelled,
            TaskStatus::Archived,
        ] {
            let mut manager = TaskManager::new();
            let id = add_task(&mut manager, "Stopped");
            manager.bind_terminal_session(id, "agent".into()).unwrap();
            if status == TaskStatus::Completed {
                manager
                    .update_status(id, TaskStatus::ReadyForReview, None)
                    .unwrap();
                manager
                    .bind_validation_session(id, "validation".into())
                    .unwrap();
                manager.handle_terminal_session_exit("validation", Some(0));
                manager.complete_after_validation(id).unwrap();
            } else {
                manager.update_status(id, status, None).unwrap();
            }
            assert!(task_terminal_exit_feedback(&mut manager, "agent", Some(0)).is_none());
            assert_eq!(manager.get(id).unwrap().status, status);
        }
        for code in [Some(3), None] {
            let mut manager = TaskManager::new();
            let id = add_task(&mut manager, "Failed");
            manager.bind_terminal_session(id, "agent".into()).unwrap();
            assert!(task_terminal_exit_feedback(&mut manager, "agent", code)
                .unwrap()
                .contains("Agent session failed"));
            assert!(task_terminal_exit_feedback(&mut manager, "agent", code).is_none());
        }
    }
}
