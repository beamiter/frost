//! iced-side state and helpers for the experimental Tasks dashboard.
//!
//! The provider-neutral domain (task reducer, native runtime, worktree
//! containment, validation preflight) lives in [`crate::agent_task`]. This
//! module owns only the pieces iced's update/view loop needs: panel view
//! state, background worktree creation, the prompt-consent projection of the
//! user configuration, and the validation terminal's argv/environment
//! contract. Everything here is either pure or structured so the pure parts
//! are headless-testable.

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
/// inside the native byte budget, and the live session has turn headroom.
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
    text
}

/// Fully prepared task registration produced by the background worktree
/// worker. The UI thread only registers it with the task manager.
pub(crate) struct PreparedTask {
    pub(crate) context: SemanticCommandContext,
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
    let worktree_root = dirs::data_local_dir()
        .ok_or_else(|| "cannot locate the per-user data directory".to_string())?
        .join("frost")
        .join("agent-tasks");
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
        Self { context, identity: Arc::new(()) }
    }

    pub(crate) fn reference(&self) -> Arc<()> {
        self.identity.clone()
    }

    pub(crate) fn retire_reference(&mut self) {
        self.identity = Arc::new(());
    }

    pub(crate) fn take_current(picker: &mut Option<Self>, reference: &Arc<()>) -> Option<SemanticCommandContext> {
        if !picker.as_ref().is_some_and(|current| Arc::ptr_eq(&current.identity, reference)) {
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

pub(crate) struct TaskPanel {
    pub(crate) selected: Option<TaskId>,
    /// Draft review feedback for the selected task's next native turn.
    pub(crate) follow_up: String,
    follow_up_epoch: Arc<()>,
    pub(crate) pending_creation: Option<PendingTaskCreation>,
    /// Failed-block context waiting for an explicit provider choice.
    pub(crate) provider_picker: Option<TaskProviderPicker>,
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
            follow_up: String::new(),
            follow_up_epoch: Arc::new(()),
            pending_creation: None,
            provider_picker: None,
            diff: crate::agent_task::AgentDiffPanel::new(),
            diff_task: None,
            diff_epoch: Arc::new(()),
        }
    }

    pub(crate) fn select(&mut self, task_id: Option<TaskId>) {
        self.close_diff();
        self.selected = task_id;
        self.follow_up.clear();
        self.retire_follow_up();
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
            || matches!(&result, Err(crate::agent_task::DiffRequestError::WorkerSpawn(_)))
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
        if let Some(picker) = self.provider_picker.as_mut() {
            picker.retire_reference();
        }
    }

    /// Hiding the composer revokes callbacks but keeps its editable draft.
    pub(crate) fn retire_follow_up(&mut self) {
        self.follow_up_epoch = Arc::new(());
    }

    pub(crate) fn follow_up_reference(&self, task_id: TaskId, completed_turns: usize) -> TaskFollowUpRef {
        TaskFollowUpRef { task_id, completed_turns, selection: self.follow_up_epoch.clone() }
    }

    pub(crate) fn accepts_follow_up(&self, reference: &TaskFollowUpRef, completed_turns: usize) -> bool {
        self.selected == Some(reference.task_id)
            && reference.completed_turns == completed_turns
            && Arc::ptr_eq(&self.follow_up_epoch, &reference.selection)
    }

    pub(crate) fn edit_follow_up(&mut self, reference: &TaskFollowUpRef, completed_turns: usize, text: String) -> bool {
        if !self.accepts_follow_up(reference, completed_turns) {
            return false;
        }
        self.set_follow_up(text);
        true
    }

    /// Consume callback identity before dispatch, retaining the draft until
    /// success. Even a failed attempt cannot be repeated by a queued duplicate;
    /// the user can retry through the newly rendered composer identity.
    pub(crate) fn begin_follow_up_send(&mut self, reference: &TaskFollowUpRef, completed_turns: usize) -> Option<String> {
        if !self.accepts_follow_up(reference, completed_turns)
            || !native_follow_up_can_send(&self.follow_up, completed_turns)
        {
            return None;
        }
        let text = self.follow_up.clone();
        self.retire_follow_up();
        Some(text)
    }

    pub(crate) fn set_follow_up(&mut self, text: impl Into<String>) {
        let text = bound_follow_up(text);
        if follow_up_is_unsafe(&text) {
            return;
        }
        self.follow_up = text;
    }
}

#[cfg(test)]
mod tests {
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
        assert_eq!(picker.as_ref().unwrap().context.command.as_deref(), Some("B"));
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
        let method = source.split("fn task_create_with_provider(").nth(1).unwrap()
            .split("/// Enter the bounded").next().unwrap();
        assert!(method.find("!self.config.experimental_task_sidebar").unwrap()
            < method.find("TaskProviderPicker::take_current").unwrap());
        assert!(method.find("TaskProviderPicker::take_current").unwrap()
            < method.find("ensure_executable_available").unwrap());
        assert!(method.find("ensure_executable_available").unwrap()
            < method.find("begin_worktree_creation").unwrap());
        assert!(method.contains("TaskProviderPicker::new(context)"));
        assert!(source.contains("Message::TaskCreateWithProvider(reference.clone(), provider)"));
        assert!(source.contains("Message::TaskCreateProviderCancel(reference)"));
        let cancel = source.split("Message::TaskCreateProviderCancel(reference) => {").nth(1).unwrap()
            .split("Message::TaskStartNative").next().unwrap();
        assert!(cancel.contains("TaskProviderPicker::take_current"));
        for start in ["fn toggle_sidebar(", "fn sync_tab_position_ui(", "fn set_task_panel_open("] {
            let body = source.split(start).nth(1).unwrap().split("\n    fn ").next().unwrap();
            assert!(body.contains("retire_panel_callbacks()"));
        }
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
        let open = source.split("Message::TaskDiffOpen(task_id) => {").nth(1).unwrap()
            .split("Message::TaskDiffClose").next().unwrap();
        assert!(open.find("self.task_panel.selected != Some(task_id)").unwrap()
            < open.find("request_diff(task_id").unwrap());
        let pump = source.split("// Tasks dashboard pump:").nth(1).unwrap()
            .split("let interval =").next().unwrap();
        assert!(pump.contains("self.task_panel.diff.state().loading"));
        let ui = include_str!("agent_task_ui.rs");
        let close = ui.split("pub(crate) fn close_diff(&mut self) {").nth(1).unwrap()
            .split("pub(crate) fn diff_reference").next().unwrap();
        assert!(!close.contains("self.diff ="));
        assert!(!close.contains("pending"));
        let worker = include_str!("agent_task/diff.rs");
        let poll = worker.split("pub fn poll(&mut self) -> bool {").nth(1).unwrap()
            .split("/// Base revision").next().unwrap();
        assert!(!poll.contains("is_open = true"));
        let apply = worker.split("fn apply_result(&mut self, result: WorkerResult) {").nth(1).unwrap()
            .split("\n}").next().unwrap();
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
        assert_eq!(panel.begin_follow_up_send(&current_b, 1).as_deref(), Some("feedback for B"));
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
        let send = source.split("Message::TaskFollowUpSend(reference) => {").nth(1).unwrap()
            .split("Message::TaskApprovalDeny").next().unwrap();
        let guard = send.find("task_follow_up_is_current").unwrap();
        let take = send.find("begin_follow_up_send").unwrap();
        let dispatch = send.find("prompt_codex").unwrap();
        let clear = send.find("Ok(()) => self.task_panel.follow_up.clear()").unwrap();
        assert!(guard < take && take < dispatch && dispatch < clear);
        let input = source.split("Message::TaskFollowUpInput(reference, value) => {").nth(1).unwrap()
            .split("Message::TaskFollowUpSend").next().unwrap();
        assert!(input.find("task_follow_up_is_current").unwrap() < input.find("edit_follow_up").unwrap());
        assert!(source.contains("Message::TaskFollowUpInput(input_reference.clone(), value)"));
        assert!(source.contains("Message::TaskFollowUpSend(reference)"));
        assert!(!source.contains("self.task_panel.selected ="));
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
}
