use crate::theme::ThemeExt as _;
pub(crate) use jterm_core::char_width;
use jterm_core::click_cursor;
use jterm_core::pane_layout::{
    self, collect_pane_rects, directional_focus_target, equalize_shares, normalized_shares,
    set_divider_share, split_node_rect, Axis, DividerId, PaneDirection, PaneRect, PaneTree,
};
use jterm_core::pty_input::{self, PasteModes, PastePolicy, UnbracketedMultiline};
mod agent;
mod agent_task;
mod agent_task_ui;
mod ansi;
mod block_export;
mod block_mode;
mod color;
mod command_palette;
mod config;
mod debug;
mod history_picker;
mod image_drop;
mod keybindings;
mod kitty_graphics;
mod link;
mod persistence;
mod pty;
mod remote_fs;
mod review_text;
mod search;
mod search_replace;
mod search_replace_panel;
mod session_persistence;
mod sidebar;
mod terminal;
mod terminal_view;
mod theme;

use std::hash::Hash;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::sync::{Arc, Mutex};

use config::Config;
use iced::widget::{
    button, checkbox, column, container, mouse_area, pick_list, row, scrollable, slider, stack,
    text, text_input, Space,
};
use iced::{keyboard, Color, Element, Length, Size, Subscription, Task};
use pty::{Pty, ReaderPoll};
use terminal::{
    ProjectedViewport, ProjectionKey, ProjectionPolicy, ProjectionViewState, TerminalState,
};
use terminal_view::{
    BlockMouseAction, KittyRender, Metrics, MouseButton, MouseInput, SummaryActivation, TermWidget,
};
use theme::Theme;

/// Must stay equal to the installed entry's basename
/// (`data/io.github.beamiter.frost.desktop`): the desktop shell pairs a window
/// with its launcher entry through this id.
const WINDOW_APP_ID: &str = "io.github.beamiter.frost";

/// The same artwork the installer puts in the icon theme, embedded so a window
/// carries its icon even when the .desktop entry is not installed.
const WINDOW_ICON_PNG: &[u8] = include_bytes!("../data/io.github.beamiter.frost-128.png");

/// Height reserved for the tab bar at the top of the window.
const TAB_BAR_H: f32 = 30.0;
/// Height reserved for the status bar at the bottom of the window. The family
/// constant, so all four jterms reserve the same room for the same bar.
const STATUS_BAR_H: f32 = jterm_core::bottom_bar::BAR_HEIGHT;
/// Default width of the file-tree sidebar when shown.
const SIDEBAR_W: f32 = 220.0;
/// Drag-resize bounds for the sidebar width.
const SIDEBAR_W_MIN: f32 = 120.0;
const SIDEBAR_W_MAX: f32 = 500.0;
/// Thickness of the divider drawn between split panes (also its drag hit area).
const DIVIDER: f32 = 6.0;
/// Hit width of the invisible window-resize strips along the window edges.
/// The window is undecorated (see `main`), so nothing else offers a resize
/// grip. Wide enough to grab without swallowing clicks meant for the chrome.
const RESIZE_EDGE: f32 = 5.0;
/// Hit size of the diagonal resize grips in the window corners.
const RESIZE_CORNER: f32 = 12.0;

/// Vertical chrome around the terminal area: the always-present tab bar plus
/// the bottom bar when the `bottom_bar` toggle is on.
fn chrome_height(bottom_bar: bool) -> f32 {
    TAB_BAR_H + if bottom_bar { STATUS_BAR_H } else { 0.0 }
}

/// Whether an existing block selection may consume plain navigation/Enter.
/// Running and alternate-screen applications always retain their keyboard.
fn block_selection_owns_keys(
    block_mode: bool,
    has_selection: bool,
    alt_screen: bool,
    command_running: bool,
) -> bool {
    block_mode && has_selection && !alt_screen && !command_running
}

/// Resolve contextual Ctrl+Up/Down block navigation. `Passthrough` means the
/// caller must retain ordinary scrolling; `Select`/`Clear` own the key even at
/// the selection boundary.
fn ctrl_scroll_block_navigation(
    block_mode: bool,
    older: bool,
    alt_screen: bool,
    command_running: bool,
    ids: &[u64],
    current: Option<u64>,
) -> block_mode::SelectionNavigation {
    if !block_mode || alt_screen || command_running || (!older && current.is_none()) {
        return block_mode::SelectionNavigation::Passthrough;
    }
    block_mode::selection_navigation(ids, current, older)
}

fn block_escape_owns_key(
    block_mode: bool,
    has_selection: bool,
    alt_screen: bool,
    command_running: bool,
    no_modifier: bool,
) -> bool {
    no_modifier && block_selection_owns_keys(block_mode, has_selection, alt_screen, command_running)
}

fn block_enter_reinputs_selection(
    selection_owns_keys: bool,
    prompt_status: terminal::AgentPromptStatus,
) -> bool {
    selection_owns_keys && prompt_status.is_ready()
}

/// Keyboard bookmark toggling is deliberately selection-only. Falling back to
/// the newest block would steal Ctrl+Shift+B from the shell with no visible
/// target and differs from the Block family contract.
fn active_bookmark_target(ids: &[u64], active: Option<u64>) -> Option<u64> {
    active.filter(|id| ids.contains(id))
}

/// Finished history rows are local static cards. Application mouse reporting
/// owns only active/live rows (or every row in alternate screen, where there
/// are no finalized cards); Shift and link activation remain local overrides.
fn app_owns_terminal_mouse(
    mouse_enabled: bool,
    shift: bool,
    app_eligible: bool,
    link_override: bool,
) -> bool {
    mouse_enabled && !shift && app_eligible && !link_override
}

fn app_owns_terminal_wheel(mouse_enabled: bool, shift: bool, app_eligible: bool) -> bool {
    mouse_enabled && !shift && app_eligible
}

fn app_mouse_uses_full_grid(block_mode: bool, alt_screen: bool, usable_partitions: bool) -> bool {
    !block_mode || alt_screen || !usable_partitions
}

fn prompt_jump_target(
    rows: impl Iterator<Item = usize>,
    viewport_top: usize,
    older: bool,
) -> Option<usize> {
    rows.filter(|row| {
        if older {
            *row < viewport_top
        } else {
            *row > viewport_top
        }
    })
    .reduce(|best, row| if older { best.max(row) } else { best.min(row) })
}

/// Resolve the finalized card currently covering one viewport row. This is
/// kept independent of `Frost` so stale-render tests can drive a real PTY
/// lifecycle and change only the viewport between render and dispatch.
fn finalized_block_at_viewport_row(
    block_mode_enabled: bool,
    terminal: &terminal::TerminalState,
    projection: &ProjectedViewport,
    row: usize,
) -> Option<u64> {
    if !block_mode_enabled || terminal.is_alt_buffer_active() {
        return None;
    }
    match projection.row_kinds().get(row)? {
        terminal::ProjectedRowKind::CollapsedSummary { key, .. } => {
            return (key.policy_revision == projection.policy_revision()
                && projection.effective_collapsed().contains(&key.zone_id)
                && terminal
                    .zone_by_id(key.zone_id)
                    .is_some_and(|zone| !zone.rows_evicted))
            .then_some(key.zone_id);
        }
        terminal::ProjectedRowKind::Padding => return None,
        terminal::ProjectedRowKind::Raw => {}
    }
    let abs_row = projection.view_row_absolute(row)?;
    let total = terminal.scrollback_len() + terminal.grid.rows();
    let live_boundary = terminal
        .running_zone_start()
        .or(terminal.live_prompt_row())
        .unwrap_or(total);
    let zones: Vec<&terminal::CommandZone> = terminal
        .command_zones
        .iter()
        .filter(|zone| !zone.rows_evicted)
        .collect();
    let starts: Vec<usize> = zones.iter().map(|zone| zone.prompt_start).collect();
    zones
        .iter()
        .zip(block_mode::spans(&starts, live_boundary))
        .find(|(_, (span_start, span_end))| abs_row >= *span_start && abs_row < *span_end)
        .map(|(zone, _)| zone.id)
}

/// Map one raw-buffer search match through the exact immutable projection.
/// A match split across display rows, trimmed origin, or structural padding is
/// not guessed onto a nearby cell.
fn project_search_match(
    terminal: &terminal::TerminalState,
    projection: &ProjectedViewport,
    matched: &search::SearchMatch,
) -> Option<search::SearchMatch> {
    let end_col = matched.col_end.checked_sub(1)?;
    let start_origin = terminal.raw_cell_origin_at_absolute(matched.line, matched.col_start)?;
    let end_origin = terminal.raw_cell_origin_at_absolute(matched.line, end_col)?;
    let start = projection.raw_to_view(start_origin)?;
    let end = projection.raw_to_view(end_origin)?;
    if projection.view_to_raw(start) != Some(start_origin)
        || projection.view_to_raw(end) != Some(end_origin)
    {
        return None;
    }
    (start.row == end.row).then_some(search::SearchMatch {
        line: start.row,
        col_start: start.col,
        col_end: end.col.saturating_add(1),
    })
}

/// Project a Kitty placement as one indivisible rectangle. Reflow or collapse
/// may split its backing raw rows; rendering only a surviving fragment would
/// visually bridge hidden output, so every occupied row must map to a
/// consecutive display row at the same column.
fn projected_kitty_anchor(
    terminal: &terminal::TerminalState,
    projection: &ProjectedViewport,
    buffer_row: usize,
    col: usize,
    cols: usize,
    rows: usize,
) -> Option<terminal::ViewportCell> {
    let rows = rows.max(1);
    let cols = cols.max(1);
    if rows > projection.cells().len() || cols > projection.cells().first().map_or(0, Vec::len) {
        return None;
    }
    let mut first = None;
    for row_delta in 0..rows {
        let absolute_row = buffer_row.checked_add(row_delta)?;
        let origin = terminal.raw_cell_origin_at_absolute(absolute_row, col)?;
        let mapped = projection.raw_range_to_view(origin, cols)?;
        match first {
            None => first = Some(mapped),
            Some(anchor)
                if mapped.row == anchor.row.checked_add(row_delta)? && mapped.col == anchor.col => {
            }
            Some(_) => return None,
        }
    }
    first
}

/// Translate a visible projected cell back onto the live PTY grid. History,
/// padding and synthetic rows fail closed; only this mapping may feed app
/// mouse reports or click-to-cursor movement while collapse shifts rows.
fn projected_live_grid_cell(
    terminal: &terminal::TerminalState,
    projection: &ProjectedViewport,
    row: usize,
    col: usize,
) -> Option<(usize, usize)> {
    let origin = projection.view_to_raw(terminal::ViewportCell { row, col })?;
    let absolute = projection.view_row_absolute(row)?;
    let history = terminal.scrollback_len();
    if absolute < history || terminal.raw_row_id_at_absolute(absolute)? != origin.row {
        return None;
    }
    Some((origin.col, absolute - history))
}

struct ProjectedZoneMemberships {
    rows: Vec<Option<(usize, usize)>>,
    #[cfg(test)]
    scan_steps: usize,
}

/// Assign sorted projected raw rows to sorted zone spans in one forward pass.
/// `None` rows are structural viewport padding and remain unowned.
fn projected_zone_memberships(
    view_absolute_rows: &[Option<usize>],
    zone_spans: &[(usize, usize)],
) -> ProjectedZoneMemberships {
    let mut zone_index = 0;
    #[cfg(test)]
    let mut scan_steps = 0;
    let rows = view_absolute_rows
        .iter()
        .map(|absolute_row| {
            #[cfg(test)]
            {
                scan_steps += 1;
            }
            let absolute_row = (*absolute_row)?;
            while zone_index < zone_spans.len() && absolute_row >= zone_spans[zone_index].1 {
                zone_index += 1;
                #[cfg(test)]
                {
                    scan_steps += 1;
                }
            }
            zone_spans
                .get(zone_index)
                .filter(|(start, end)| absolute_row >= *start && absolute_row < *end)
                .map(|_| (zone_index, absolute_row))
        })
        .collect();
    ProjectedZoneMemberships {
        rows,
        #[cfg(test)]
        scan_steps,
    }
}

fn projected_card_real_top(
    visible_raw_top: Option<usize>,
    summary_top: Option<usize>,
    outcome: block_mode::BlockOutcome,
) -> Option<usize> {
    visible_raw_top.or_else(|| {
        // Collapse hides output only. A command card whose header is merely
        // above the viewport must stay top-clipped; only Background has no
        // surviving header and may promote its summary to the semantic top.
        matches!(outcome, block_mode::BlockOutcome::Background)
            .then_some(summary_top)
            .flatten()
    })
}

/// A claimed card gesture may use only the stable id painted under the press.
/// A later viewport mapping is evidence for validation, never a replacement
/// target: moving onto a neighbour fails closed.
fn validated_claimed_block_target(
    rendered_zone_id: Option<u64>,
    current_row_zone_id: Option<u64>,
    retained_finalized: bool,
) -> Option<u64> {
    let rendered_zone_id = rendered_zone_id?;
    (retained_finalized && current_row_zone_id == Some(rendered_zone_id))
        .then_some(rendered_zone_id)
}

/// A link press is meaningful only in the immutable projection it was painted
/// from. Revision zero means the projection identity counter exhausted and is
/// deliberately never activatable.
fn link_projection_matches(rendered_revision: u64, current_revision: u64) -> bool {
    rendered_revision != 0 && rendered_revision == current_revision
}

fn validated_summary_target(
    projection: &ProjectedViewport,
    activation: &SummaryActivation,
) -> Option<u64> {
    let key = activation.key;
    (projection.key() == activation.projection_key
        && key.policy_revision == projection.policy_revision()
        && projection.effective_collapsed().contains(&key.zone_id)
        && projection.row_kinds().iter().any(|kind| {
            matches!(
                kind,
                terminal::ProjectedRowKind::CollapsedSummary { key: current, .. }
                    if *current == key
            )
        }))
    .then_some(key.zone_id)
}

fn agent_context_exit_label(exit_code: i32) -> String {
    if exit_code == -1 {
        "no reported exit status".to_string()
    } else {
        format!("exit {exit_code}")
    }
}

const NO_REPORTED_EXIT_STATUS_NOTE: &str =
    "[terminal] the shell reported no exit status for this command";

fn bounded_ai_block_output(output: &str, no_reported_status: bool) -> (String, bool) {
    let prepared = if no_reported_status {
        if output.is_empty() {
            NO_REPORTED_EXIT_STATUS_NOTE.to_string()
        } else {
            format!("{NO_REPORTED_EXIT_STATUS_NOTE}\n{output}")
        }
    } else {
        output.to_string()
    };
    let bounded = jterm_core::ai::truncate_for_context(&prepared, 80);
    let truncated = bounded != prepared;
    (bounded, truncated)
}

fn clear_stale_hidden_match_diagnostic(error: &mut Option<String>) {
    if error
        .as_deref()
        .is_some_and(|message| message.starts_with("Match is hidden in collapsed block #"))
    {
        *error = None;
    }
}

/// Commands whose configured chords must fall through to the PTY whenever
/// block history is hidden/unsafe (Block Mode off or alternate screen). This
/// preflight is keybinding-only; palette/menu actions still explain refusal
/// with a toast through `ensure_block_action_available`.
fn command_requires_block_context(command: &keybindings::Command) -> bool {
    use keybindings::Command as C;
    matches!(
        command,
        C::TerminalPromptPrev
            | C::TerminalPromptNext
            | C::TerminalCopyLastOutput
            | C::BlockJumpFirstFailed
            | C::BlockJumpPrevFailed
            | C::BlockJumpNextFailed
            | C::BlockCopyCommand
            | C::BlockCopyOutput
            | C::BlockRecallCommand
            | C::BlockSelectAll
            | C::BlockClear
            | C::BlockSelectPrev
            | C::BlockSelectNext
            | C::BlockReinputSelectedCommands
            | C::BlockCopyBlock
            | C::BlockCopyMarkdown
            | C::BlockExportSessionMarkdown
            | C::BlockExportSessionJson
            | C::BlockSearch
            | C::BlockToggleBookmark
            | C::BlockJumpPrevBookmark
            | C::BlockJumpNextBookmark
            | C::BlockFixWithAgent
            | C::BlockExplainWithAgent
            | C::BlockRetryFailed
    )
}

/// Shared refusal reason for block actions that replace the shell's editable
/// command line. A stopped command is not enough evidence: OSC 133 must also
/// identify an empty prompt and the shell must still own the foreground PTY.
fn block_prompt_replace_blocker(
    prompt_status: terminal::AgentPromptStatus,
) -> Option<&'static str> {
    use terminal::AgentPromptStatus;
    match prompt_status {
        AgentPromptStatus::Ready => None,
        AgentPromptStatus::Busy => Some("the terminal is busy"),
        AgentPromptStatus::InputNotEmpty => Some("the prompt already contains input"),
        AgentPromptStatus::UnsafeCommand => Some("the prompt state is unsafe"),
        AgentPromptStatus::ShellIntegrationUnavailable => {
            Some("waiting for an empty OSC 133 shell prompt")
        }
    }
}

/// Captured command history is untrusted terminal input. Keep controls
/// stripped and, crucially, never send embedded newlines without bracketed
/// paste: the first-line fallback prevents later selected commands executing.
fn block_reinput_policy() -> PastePolicy {
    PastePolicy::prompt_insert(UnbracketedMultiline::FirstLineOnly)
}

/// Guarded failed-block retry auto-submits: the same de-fanging as reinput,
/// plus a trailing CR after the bracketed frame. Only reached for exact,
/// non-truncated, single-line commands whose recorded cwd was verified.
fn block_retry_policy() -> PastePolicy {
    PastePolicy {
        submit: true,
        ..block_reinput_policy()
    }
}
/// Height of the status strip above each pane while split. A single pane has
/// no strip: the tab bar and status bar already name it, and the row would
/// only cost a terminal line.
const PANE_HEADER_H: f32 = 20.0;
/// Maximum total leaves (panes) across the whole layout tree; a PTY guard.
const MAX_PANES: usize = 12;
/// Fraction of a pane reserved for directional tab-to-split drop zones.
/// Keeping a generous dead zone in the middle makes an accidental drop a
/// harmless cancel instead of unexpectedly rearranging a running workspace.
const SPLIT_DROP_EDGE_FRACTION: f32 = 0.28;
/// Hovering another tab for this long during a drag previews its page. A quick
/// pass across the strip remains a pure reorder gesture.
const TAB_DRAG_HOVER_SWITCH_MS: u64 = 450;
const SPLIT_RATIO_KEY_STEP: f32 = 0.05;
/// Two presses on the same divider within this window count as a double-click
/// (equalizes every pane).
const DIVIDER_DOUBLE_CLICK_MS: u64 = 400;
/// Bound pending user/protocol input while a child is not reading its PTY.
const MAX_PTY_WRITE_QUEUE_BYTES: usize = 8 * 1024 * 1024;
/// Responses are retried separately so a full user-input queue cannot discard
/// terminal protocol replies. The combined per-session backlog remains bounded.
const MAX_PTY_RESPONSE_QUEUE_BYTES: usize = 8 * 1024 * 1024;
/// Byte caps alone do not cover allocator/Vec metadata for one-byte writes.
const MAX_PTY_QUEUE_ENTRIES: usize = 4096;
const PTY_QUEUE_COALESCE_BYTES: usize = 64 * 1024;
/// Maximum queued input written during one UI update.
const PTY_WRITE_DRAIN_BUDGET: usize = 256 * 1024;
/// jagent accepts at most 16 KiB per proposed command; reserve framing,
/// Ctrl+U and submitting CR before moving the state machine to approved.
const MAX_AGENT_APPROVAL_PAYLOAD_BYTES: usize = 16 * 1024 + 32;
/// Never reflect an unexpectedly huge host clipboard through a terminal escape.
const MAX_CLIPBOARD_RESPONSE_BYTES: usize = 1024 * 1024;

/// Keep the physical viewport fixed while the application-level UI scale
/// changes. iced updates its logical viewport for this case without emitting a
/// window `Resized` event, so the terminal must mirror that conversion itself.
fn logical_viewport_after_scale(size: Size, old_scale: f32, new_scale: f32) -> Size {
    let ratio = old_scale / new_scale;
    Size::new(size.width * ratio, size.height * ratio)
}

/// Stable widget ids so the overlays' text inputs can be focused on open.
static SEARCH_INPUT_ID: once_cell::sync::Lazy<iced::widget::Id> =
    once_cell::sync::Lazy::new(|| iced::widget::Id::new("jterm-search-input"));
static PALETTE_INPUT_ID: once_cell::sync::Lazy<iced::widget::Id> =
    once_cell::sync::Lazy::new(|| iced::widget::Id::new("jterm-palette-input"));
static PALETTE_LIST_ID: once_cell::sync::Lazy<iced::widget::Id> =
    once_cell::sync::Lazy::new(|| iced::widget::Id::new("jterm-palette-list"));
static AGENT_INPUT_ID: once_cell::sync::Lazy<iced::widget::Id> =
    once_cell::sync::Lazy::new(|| iced::widget::Id::new("jterm-agent-input"));
static AGENT_EDIT_INPUT_ID: once_cell::sync::Lazy<iced::widget::Id> =
    once_cell::sync::Lazy::new(|| iced::widget::Id::new("jterm-agent-edit-input"));
static TAB_SWITCHER_INPUT_ID: once_cell::sync::Lazy<iced::widget::Id> =
    once_cell::sync::Lazy::new(|| iced::widget::Id::new("jterm-tab-switcher-input"));
static HISTORY_PICKER_INPUT_ID: once_cell::sync::Lazy<iced::widget::Id> =
    once_cell::sync::Lazy::new(|| iced::widget::Id::new("jterm-history-picker-input"));
static BLOCK_SEARCH_INPUT_ID: once_cell::sync::Lazy<iced::widget::Id> =
    once_cell::sync::Lazy::new(|| iced::widget::Id::new("jterm-block-search-input"));
static BLOCK_SEARCH_LIST_ID: once_cell::sync::Lazy<iced::widget::Id> =
    once_cell::sync::Lazy::new(|| iced::widget::Id::new("jterm-block-search-list"));
static TAB_RENAME_INPUT_ID: once_cell::sync::Lazy<iced::widget::Id> =
    once_cell::sync::Lazy::new(|| iced::widget::Id::new("jterm-tab-rename-input"));
static SIDEBAR_DIALOG_INPUT_ID: once_cell::sync::Lazy<iced::widget::Id> =
    once_cell::sync::Lazy::new(|| iced::widget::Id::new("jterm-sidebar-dialog-input"));
static SEARCH_REPLACE_FIND_ID: once_cell::sync::Lazy<iced::widget::Id> =
    once_cell::sync::Lazy::new(|| iced::widget::Id::new("jterm-search-replace-find"));

/// Toast kind drives the accent color of the floating notification.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ToastKind {
    Info,
    Success,
    Warning,
}

/// Transient bottom-right notification. `expires_at` is absolute monotonic time.
#[derive(Debug, Clone)]
struct Toast {
    text: String,
    kind: ToastKind,
    expires_at: std::time::Instant,
}

/// A pane-header press that may turn into a rearrange drag.
///
/// The source is held by stable session id rather than index: a background
/// session can exit mid-drag, which shifts every later index and would
/// otherwise make the release swap two unrelated panes.
///
/// A press that never leaves its own pane simply focuses it, exactly like
/// clicking into the terminal; only a release over a *different* pane swaps.
#[derive(Debug, Clone)]
struct PaneDrag {
    session_id: usize,
    /// Stable session id the pointer is currently over, when it differs from
    /// the source. `None` means releasing in the pane area would do nothing.
    target: Option<usize>,
}

/// A validated directional target shown while an ordinary tab is dragged over
/// the visible pane tree. Both identities survive session-vector reindexing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct TabSplitDrop {
    target_session_id: usize,
    direction: PaneDirection,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TabDragReleaseAction {
    Activate(usize),
    Reorder { from: usize, to: usize },
    RestoreOrigin,
}

fn tab_drag_release_action(
    source: Option<usize>,
    target: Option<usize>,
    moved: bool,
) -> TabDragReleaseAction {
    match (source, target) {
        (Some(from), Some(to)) if from != to => TabDragReleaseAction::Reorder { from, to },
        (Some(tab), Some(_)) if !moved => TabDragReleaseAction::Activate(tab),
        _ => TabDragReleaseAction::RestoreOrigin,
    }
}

fn tab_drag_hover_left_source(source: Option<usize>, hovered: Option<usize>) -> bool {
    source.is_some_and(|source| hovered != Some(source))
}

fn tab_split_commit_allowed(
    source_tab: Option<usize>,
    source_is_plain: bool,
    active_tab: usize,
    target_tab: Option<usize>,
    target_pane_count: usize,
    zoomed: bool,
) -> bool {
    source_is_plain
        && source_tab.is_some_and(|source| source != active_tab)
        && target_tab == Some(active_tab)
        && target_pane_count < MAX_PANES
        && !zoomed
}

/// A tab and the panes it owns.
///
/// Tabs own their panes: splitting only touches the active tab's tree, so it
/// never adds a row to the tab bar, and closing a tab closes every session in
/// it. Previously one global tree held every session while the tab bar showed
/// one tab per session — a pane and a tab were the same object seen from two
/// places, so a split spawned a tab and clicking a tab reached into a split to
/// swap that session into the focused pane.
struct Tab {
    /// Stable identity for UI events. Tab-bar gestures (drag, close, context
    /// menu) must survive index shifts *and* focus moving between this tab's
    /// panes, so they cannot key off a session id.
    id: usize,
    /// tmux-style recursive pane layout. `Leaf(s)` when this tab has one pane.
    tree: PaneTree,
    /// Session shown by the pane holding keyboard focus in this tab. The tab's
    /// label and the focus restored when the tab is activated both read this.
    focus: usize,
    /// Title set from the context menu's Rename. `None` falls back to the
    /// focused session's own label, so a renamed tab keeps its name while an
    /// untouched one keeps following the shell.
    title: Option<String>,
    /// Pinned tabs sort to the front of the strip and stay there.
    pinned: bool,
    /// Marking is this family's multi-select model: "Close Marked Tabs" acts
    /// on exactly the marked set.
    marked: bool,
    /// Redact the tab's real title everywhere outside its terminal content.
    private_title: bool,
}

impl Tab {
    fn new(id: usize, session: usize) -> Self {
        Tab {
            id,
            tree: PaneTree::Leaf(session),
            focus: session,
            title: None,
            pinned: false,
            marked: false,
            private_title: false,
        }
    }

    fn sessions(&self) -> Vec<usize> {
        self.tree.leaves()
    }

    fn contains(&self, session: usize) -> bool {
        self.tree.contains_session(session)
    }

    /// Keep `focus` pointing at a pane this tab still owns.
    fn repair_focus(&mut self) {
        if !self.tree.contains_session(self.focus) {
            if let Some(&first) = self.tree.leaves().first() {
                self.focus = first;
            }
        }
    }
}

/// One tab as it came out of a snapshot: the pane tree plus the per-tab state
/// that rides along with it. The caller validates every index before any of
/// this becomes a live [`Tab`].
struct RestoredTab {
    tree: PaneTree,
    focus: Option<usize>,
    title: Option<String>,
    pinned: bool,
    marked: bool,
    private_title: bool,
}

impl RestoredTab {
    /// A tab recovered from a layout that carries no per-tab state (the v1
    /// single-tree snapshots, and the tests).
    fn plain(tree: PaneTree, focus: Option<usize>) -> Self {
        RestoredTab {
            tree,
            focus,
            title: None,
            pinned: false,
            marked: false,
            private_title: false,
        }
    }
}

/// Stable-partition `tabs` so pinned ones lead, and report where the tab at
/// `active` ended up. Mirrors anvil's `reorder_pinned_first`: relative order
/// inside the pinned and unpinned groups is preserved, and the active tab
/// stays active — pinning a tab must never switch which session is on screen.
fn sort_pinned_first(tabs: &mut [Tab], active: usize) -> usize {
    let active_id = tabs.get(active).map(|tab| tab.id);
    tabs.sort_by_key(|tab| !tab.pinned);
    active_id
        .and_then(|id| tabs.iter().position(|tab| tab.id == id))
        .unwrap_or(active)
}

/// Reorder one tab without ever crossing the pinned/unpinned boundary. `to`
/// keeps the existing strip semantics (dropping on a later tab places the
/// source after it), while the returned index follows the previously active
/// tab by identity.
fn reorder_tabs_preserving_pinned_prefix(
    tabs: &mut Vec<Tab>,
    active: usize,
    from: usize,
    to: usize,
) -> Option<usize> {
    if from >= tabs.len() || to >= tabs.len() || from == to {
        return None;
    }
    let moved = tabs.remove(from);
    let pinned_boundary = tabs.iter().take_while(|tab| tab.pinned).count();
    let requested = to.min(tabs.len());
    let insert_at = if moved.pinned {
        requested.min(pinned_boundary)
    } else {
        requested.max(pinned_boundary)
    };
    tabs.insert(insert_at, moved);
    Some(match active {
        index if index == from => insert_at,
        index if from < insert_at && index > from && index <= insert_at => index - 1,
        index if insert_at < from && index >= insert_at && index < from => index + 1,
        index => index,
    })
}

/// New tabs are unpinned and therefore belong after the complete pinned
/// prefix, even when the active tab sits inside that prefix.
fn new_unpinned_tab_index(tabs: &[Tab], active: usize) -> usize {
    let first_unpinned = tabs
        .iter()
        .position(|tab| !tab.pinned)
        .unwrap_or(tabs.len());
    active.saturating_add(1).max(first_unpinned).min(tabs.len())
}

/// What a confirmed close should actually tear down. Closing a tab takes every
/// pane in it, so the confirmation has to remember which of the two was asked
/// for rather than always closing the one busy session it named.
#[derive(Debug, Clone, Copy)]
enum PendingClose {
    /// Close this one session (one pane), then optionally activate another tab.
    Session { activate_after: Option<usize> },
    /// Close the whole tab that owns the named session.
    Tab,
}

/// What `restore_or_spawn` recovered: the sessions it could bring back, plus
/// the snapshot's layout fields, which the caller validates against them.
#[derive(Default)]
struct RestoredState {
    sessions: Vec<Session>,
    active: usize,
    next_id: usize,
    tabs: Vec<session_persistence::TabSnapshot>,
    active_tab: Option<usize>,
    /// v1 fallbacks: one global tree, or the even older flat single-axis split.
    legacy_tree: Option<session_persistence::PaneTreeSnapshot>,
    legacy_split: Option<session_persistence::SplitSnapshot>,
    diagnostic: Option<String>,
    /// The configured snapshot was unreadable and could not be quarantined.
    /// Preserve its bytes for this process lifetime instead of letting the
    /// first autosave destroy the only recoverable copy.
    session_writes_blocked: bool,
}

/// State for the Ctrl+Shift+L quick tab switcher overlay.
#[derive(Debug, Clone, Default)]
struct TabSwitcherState {
    query: String,
    /// Highlighted row in the filtered list.
    selected: usize,
}

/// State for the `block:search` cross-block search picker (Ctrl+Alt+F): a
/// case-insensitive substring query over every completed zone's command and
/// output (captured-snapshot-first, so trimmed-away zones still match).
///
/// Zone text is extracted into a bounded cache; lowercasing is performed on a
/// worker and each keystroke only rescans the finished index. The owning
/// session and monotonic build epoch reject late worker results. Finalized-zone
/// version changes rebuild the open picker so newly completed blocks appear.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum BlockSearchFilter {
    #[default]
    All,
    Failed,
    Slow,
    Bookmarked,
    Background,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct BlockSearchBuildIdentity {
    session_id: usize,
    epoch: u64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct BlockSearchZoneVersion {
    len: usize,
    oldest: Option<u64>,
    newest: Option<u64>,
}

impl BlockSearchZoneVersion {
    fn from_terminal(terminal: &terminal::TerminalState) -> Self {
        Self {
            len: terminal.command_zones.len(),
            oldest: terminal.command_zones.front().map(|zone| zone.id),
            newest: terminal.command_zones.back().map(|zone| zone.id),
        }
    }
}

fn next_block_search_epoch(epoch: &mut u64) -> Option<u64> {
    let next = epoch.checked_add(1)?;
    *epoch = next;
    Some(next)
}

#[derive(Default)]
struct BlockSearchState {
    /// Pane identity owning the cache. Zone ids are only pane-local.
    session_id: usize,
    /// Monotonic window-wide generation of the cache build currently owned by
    /// this picker. Session id alone cannot distinguish close/reopen races.
    epoch: u64,
    /// True while source text is being lowercased on the worker.
    loading: bool,
    /// The source or resident budget omitted at least one older zone.
    older_not_indexed: bool,
    /// Finalized-zone set represented by the current/in-flight cache.
    zone_version: BlockSearchZoneVersion,
    query: String,
    filter: BlockSearchFilter,
    /// Highlighted row among `hits` (all hits are drawn and navigable, in a
    /// scrollable list).
    selected: usize,
    hits: Vec<block_mode::BlockSearchHit>,
    /// The 500-hit matching cap stopped the scan early (older zones were
    /// left unscanned).
    capped: bool,
    /// Bounded cache the hits are computed from, oldest zone first (the zone
    /// deque's order). Rebuilt only when the finalized-zone version changes.
    cache: Vec<block_mode::CachedBlockSearchZone>,
}

impl BlockSearchState {
    fn accepts_build(&self, identity: BlockSearchBuildIdentity) -> bool {
        self.session_id == identity.session_id && self.epoch == identity.epoch
    }

    fn select_next(&mut self) {
        let len = self.hits.len();
        self.selected = if len == 0 {
            0
        } else {
            (self.selected + 1) % len
        };
    }

    fn select_prev(&mut self) {
        let len = self.hits.len();
        self.selected = if len == 0 {
            0
        } else if self.selected == 0 {
            len - 1
        } else {
            self.selected - 1
        };
    }

    /// Result-count line: `"N matches"` (`"1 match"`), plus ember's
    /// `" · older blocks not searched"` when the run stopped at the hit cap
    /// — which is what `capped` actually means, not that more matches
    /// necessarily exist.
    fn count_label(&self) -> String {
        let count = self.hits.len();
        let noun = if count == 1 { "match" } else { "matches" };
        let mut label = format!("{count} {noun}");
        if self.capped {
            label.push_str(" · older blocks not searched");
        }
        if self.older_not_indexed {
            label.push_str(" · older blocks not indexed");
        }
        label
    }
}

/// Resolve the best live buffer row for one cached block-search hit. Exact
/// match positioning is attempted first; a captured snapshot whose span no
/// longer agrees with live rows safely degrades to the logical-line start.
fn block_search_reveal_row(
    terminal: &terminal::TerminalState,
    hit: &block_mode::BlockSearchHit,
) -> Option<usize> {
    if !hit.is_output_line || hit.line_no == 0 {
        return None;
    }
    hit.match_span
        .as_ref()
        .and_then(|span| {
            terminal.zone_output_match_row(hit.zone_id, hit.line_no, span.start, span.end)
        })
        .or_else(|| terminal.zone_output_line_row(hit.zone_id, hit.line_no))
}

/// Place a pointer-anchored overlay inside the visible window. Prefer below
/// the pointer; flip above when the estimated panel height would cross the
/// bottom edge. Tiny windows keep the panel's top-left reachable.
fn anchored_overlay_position(
    anchor: iced::Point,
    window: iced::Size,
    panel: iced::Size,
    top_floor: f32,
) -> iced::Point {
    const GAP: f32 = 6.0;
    let anchor_x = if anchor.x.is_finite() { anchor.x } else { 0.0 };
    let anchor_y = if anchor.y.is_finite() {
        anchor.y
    } else {
        top_floor
    };
    let max_x = (window.width - panel.width - GAP).max(GAP);
    let x = (anchor_x + GAP).clamp(GAP, max_x);
    let max_y = (window.height - panel.height - GAP).max(top_floor);
    let below = anchor_y + GAP;
    let preferred_y = if below + panel.height <= window.height - GAP {
        below
    } else {
        anchor_y - panel.height - GAP
    };
    iced::Point::new(x, preferred_y.clamp(top_floor, max_y))
}

/// Stable target for the block action menu opened from a completed card.
/// Session identity is stored alongside the zone id because zone ids restart
/// at zero for every pane; actions fail closed if focus changed underneath it.
#[derive(Clone, Copy, Debug, PartialEq)]
struct BlockMenuState {
    session_id: usize,
    zone_id: u64,
    anchor: iced::Point,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct BlockMenuSelectionSummary {
    selected_count: usize,
    has_selected_commands: bool,
    clicked_has_command: bool,
}

fn block_menu_selection_summary<'a, I>(
    zones: I,
    selection: &block_mode::BlockSelection,
    clicked_id: u64,
) -> BlockMenuSelectionSummary
where
    I: IntoIterator<Item = (u64, Option<&'a str>)>,
{
    let mut summary = BlockMenuSelectionSummary::default();
    for (id, command) in zones {
        let has_command = command.is_some_and(|command| !command.trim().is_empty());
        if id == clicked_id {
            summary.clicked_has_command = has_command;
        }
        if selection.contains(id) {
            summary.selected_count += 1;
            summary.has_selected_commands |= has_command;
        }
    }
    summary
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BlockMenuAction {
    CopyCommand,
    AskAi,
    CopyOutput,
    CopyBlock,
    CopyMarkdown,
    RecallCommand,
    ReinputSelected,
    ToggleBookmark,
    JumpTop,
    JumpBottom,
    Search,
    ExportMarkdown,
    ExportJson,
    CollapseOutput,
    ExpandOutput,
    FixWithAgent,
    ExplainWithAgent,
    CreateTask,
    Retry,
    Clear,
}

/// Which fresh Agent task a failed block starts (ember's Fix/Explain).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FailedBlockAgentIntent {
    Fix,
    Explain,
}

/// Stable, counted target for the destructive Clear Blocks confirmation.
/// The count is part of the confirmation contract: if PTY output completes or
/// evicts a block while the modal is open, confirmation is re-armed with the
/// new count instead of deleting a different set than the user reviewed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct BlockClearConfirmation {
    session_id: usize,
    block_count: usize,
    latest_zone_id: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BlockClearResolution {
    Clear,
    Refresh(BlockClearConfirmation),
    Empty,
    Stale,
}

impl BlockClearConfirmation {
    fn new(session_id: usize, block_count: usize, latest_zone_id: Option<u64>) -> Option<Self> {
        let latest_zone_id = latest_zone_id.filter(|_| block_count > 0)?;
        Some(Self {
            session_id,
            block_count,
            latest_zone_id,
        })
    }

    /// Revalidate immediately before deletion against the active pane and its
    /// current finalized-zone count/newest id. A focus or history change fails
    /// closed or requires a second, freshly counted confirmation.
    fn resolve(self, active: Option<(usize, usize, Option<u64>)>) -> BlockClearResolution {
        let Some((session_id, block_count, latest_zone_id)) = active else {
            return BlockClearResolution::Stale;
        };
        if session_id != self.session_id {
            return BlockClearResolution::Stale;
        }
        if block_count == 0 {
            return BlockClearResolution::Empty;
        }
        let Some(latest_zone_id) = latest_zone_id else {
            return BlockClearResolution::Empty;
        };
        if block_count != self.block_count || latest_zone_id != self.latest_zone_id {
            return BlockClearResolution::Refresh(
                Self::new(session_id, block_count, Some(latest_zone_id))
                    .expect("a positive live block count has a newest zone"),
            );
        }
        BlockClearResolution::Clear
    }
}

/// Which content the left sidebar dock currently shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SidebarPanel {
    /// File-tree browser (doubles as a path picker).
    Files,
    /// Vertical session tab list.
    Tabs,
    /// Experimental agent Tasks dashboard (config-gated).
    Tasks,
}

/// Actions of the file tree's right-click menu. New File/New Folder/Paste act
/// inside the clicked directory (or the clicked file's parent); Rename/
/// Delete/Copy/Cut act on the clicked node itself.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SidebarMenuAction {
    NewFile,
    NewFolder,
    Rename,
    Delete,
    Copy,
    Cut,
    Paste,
    Refresh,
}

/// Open file-tree context menu: the right-clicked node plus the pointer
/// position frozen at press time (the floating panel anchors to it).
#[derive(Clone, Debug)]
struct SidebarMenuState {
    path: std::path::PathBuf,
    is_dir: bool,
    at: iced::Point,
}

/// What the sidebar's modal text input is collecting a name for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SidebarDialogKind {
    NewFile,
    NewFolder,
    Rename,
}

/// Modal New File / New Folder / Rename input. `path` is the target directory
/// for creates and the node being renamed for Rename; `error` shows the last
/// failed validation inline.
#[derive(Clone, Debug)]
struct SidebarDialogState {
    kind: SidebarDialogKind,
    path: std::path::PathBuf,
    input: String,
    error: Option<String>,
}

/// Location-scoped sidebar clipboard. Paste is offered only while the tree
/// shows the same location the entry was taken from — a remote path pasted
/// onto a different machine would be a silent category error.
#[derive(Clone, Debug)]
struct FsClipboard {
    loc: remote_fs::FsLocation,
    path: std::path::PathBuf,
    is_dir: bool,
    cut: bool,
}

/// One filesystem mutation handed to a worker task. `Move` is the cut-paste
/// case: it clears the clipboard on success, `Rename` does not. `Transfer` is
/// a cross-location copy (download/upload/relay), `TransferMove` the
/// cross-location cut: transfer first, then delete the source.
#[derive(Clone, Debug)]
enum SidebarOp {
    CreateFile(std::path::PathBuf),
    CreateDir(std::path::PathBuf),
    Rename {
        src: std::path::PathBuf,
        dst: std::path::PathBuf,
    },
    Delete(std::path::PathBuf),
    Copy {
        src: std::path::PathBuf,
        dst: std::path::PathBuf,
    },
    Move {
        src: std::path::PathBuf,
        dst: std::path::PathBuf,
    },
    Transfer {
        src_loc: remote_fs::FsLocation,
        dst_loc: remote_fs::FsLocation,
        src: std::path::PathBuf,
        dst: std::path::PathBuf,
        is_dir: bool,
    },
    TransferMove {
        src_loc: remote_fs::FsLocation,
        dst_loc: remote_fs::FsLocation,
        src: std::path::PathBuf,
        dst: std::path::PathBuf,
        is_dir: bool,
    },
}

/// Worker report for a finished [`SidebarOp`], matched back against the
/// tree's current location before any refresh is issued. `location` is the
/// tree the op changed (the destination for transfers). `warning` rides
/// along an `Ok` for partial successes (a cut whose source would not delete).
#[derive(Clone, Debug)]
struct SidebarOpReport {
    location: remote_fs::FsLocation,
    op: SidebarOp,
    warning: Option<String>,
    result: Result<(), String>,
}

/// One entry of the file tree's location picker: a location plus the label
/// built from the current config (`Local`, `ssh: dev`, `docker: myubuntu`).
#[derive(Clone, Debug, PartialEq)]
struct SidebarLocationChoice {
    location: remote_fs::FsLocation,
    label: String,
}

impl std::fmt::Display for SidebarLocationChoice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.label)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ChromeShortcut {
    CommandPalette,
    Help,
    TabSwitcher,
    HistoryPicker,
    RemoteHosts,
    Debug,
}

fn chrome_shortcut(key: &keyboard::Key, modifiers: keyboard::Modifiers) -> Option<ChromeShortcut> {
    use keyboard::key::Named;
    use keyboard::Key;

    if matches!(key, Key::Named(Named::F12)) {
        return Some(ChromeShortcut::Debug);
    }
    if !(modifiers.control() && modifiers.shift()) {
        return None;
    }
    let Key::Character(s) = key else {
        return None;
    };
    match s.chars().next()?.to_ascii_lowercase() {
        'p' => Some(ChromeShortcut::CommandPalette),
        '/' | '?' => Some(ChromeShortcut::Help),
        'l' => Some(ChromeShortcut::TabSwitcher),
        // Same chord as forge's history palette (Ctrl+R stays with readline).
        'h' => Some(ChromeShortcut::HistoryPicker),
        // Same chord as the rest of the family's remote host picker.
        's' => Some(ChromeShortcut::RemoteHosts),
        _ => None,
    }
}

fn last_session_index(session_count: usize) -> Option<usize> {
    session_count.checked_sub(1)
}

fn axis_from_str(s: &str) -> Option<Axis> {
    match s {
        "vertical" => Some(Axis::Vertical),
        "horizontal" => Some(Axis::Horizontal),
        _ => None,
    }
}

fn axis_to_str(axis: Axis) -> &'static str {
    match axis {
        Axis::Vertical => "vertical",
        Axis::Horizontal => "horizontal",
    }
}

/// Serialize a live layout tree for session persistence.
/// Exchange two session indices wherever they appear in a pane tree.
///
/// One pass, not two: remapping `a`→`b` and then `b`→`a` would send both
/// leaves to the same session and lose one of them.
fn swap_sessions_in_tree(tree: &mut PaneTree, a: usize, b: usize) {
    let remap = |session: usize| {
        if session == a {
            b
        } else if session == b {
            a
        } else {
            session
        }
    };
    let remap_ref: &dyn Fn(usize) -> usize = &remap;
    tree.remap_sessions(remap_ref);
}

fn pane_tree_to_snapshot(tree: &PaneTree) -> session_persistence::PaneTreeSnapshot {
    match tree {
        PaneTree::Leaf(session) => {
            session_persistence::PaneTreeSnapshot::Leaf { session: *session }
        }
        PaneTree::Split {
            axis,
            children,
            ratios,
        } => session_persistence::PaneTreeSnapshot::Split {
            axis: axis_to_str(*axis).to_string(),
            ratios: ratios.clone(),
            children: children.iter().map(pane_tree_to_snapshot).collect(),
        },
    }
}

/// Rebuild a layout tree from an (untrusted) snapshot. Ratios are normalized,
/// falling back to an even split when unusable; unknown axes or splits with
/// fewer than two children are rejected. Session indices are validated by the
/// caller against the restored session count.
fn pane_tree_from_snapshot(snap: &session_persistence::PaneTreeSnapshot) -> Option<PaneTree> {
    match snap {
        session_persistence::PaneTreeSnapshot::Leaf { session } => Some(PaneTree::Leaf(*session)),
        session_persistence::PaneTreeSnapshot::Split {
            axis,
            ratios,
            children,
        } => {
            let axis = axis_from_str(axis)?;
            if children.len() < 2 {
                return None;
            }
            let kids = children
                .iter()
                .map(pane_tree_from_snapshot)
                .collect::<Option<Vec<_>>>()?;
            let r = normalized_shares(kids.len(), ratios);
            Some(PaneTree::Split {
                axis,
                children: kids,
                ratios: r,
            })
        }
    }
}

/// Convert a legacy single-axis split snapshot into a depth-1 tree so older
/// session files keep restoring their layout.
fn pane_tree_from_legacy(split: &session_persistence::SplitSnapshot) -> Option<PaneTree> {
    let axis = axis_from_str(&split.mode)?;
    let n = split.panes.len();
    if n < 2 {
        return None;
    }
    let ratios = normalized_shares(n, &split.ratios);
    Some(PaneTree::Split {
        axis,
        children: split.panes.iter().map(|&s| PaneTree::Leaf(s)).collect(),
        ratios,
    })
}

/// Turn validated pane trees into the tab list, and say which tab is active.
///
/// Every index came from a snapshot, so it is checked here: a tab keeps only
/// leaves naming a session that exists and that no earlier tab already claimed
/// — two tabs sharing a pane would fight over one PTY.
///
/// The closing pass adopts orphans. Validation can reject a tab, a shell can
/// fail to restore, and a v1 snapshot's sessions may not appear in its single
/// tree at all; every session no tab claims gets a one-pane tab of its own,
/// because an unclaimed session is a live PTY nothing can switch to.
fn build_restored_tabs(
    trees: Vec<RestoredTab>,
    session_count: usize,
    active_session: usize,
    saved_active_tab: Option<usize>,
) -> (Vec<Tab>, usize, usize) {
    let mut claimed: std::collections::HashSet<usize> = std::collections::HashSet::new();
    let mut tabs: Vec<Tab> = Vec::new();
    let mut next_id = 0usize;
    for restored in trees {
        let RestoredTab {
            tree,
            focus: saved_focus,
            title,
            pinned,
            marked,
            private_title,
        } = restored;
        if !valid_restored_layout(&tree, session_count) {
            continue;
        }
        let leaves = tree.leaves();
        if leaves.iter().any(|leaf| claimed.contains(leaf)) {
            // A leaf is already owned by an earlier tab; drop this whole tab
            // and let the adoption pass place its sessions.
            continue;
        }
        claimed.extend(leaves.iter().copied());
        let focus = saved_focus
            .filter(|session| tree.contains_session(*session))
            .or_else(|| leaves.first().copied());
        let Some(focus) = focus else { continue };
        tabs.push(Tab {
            id: next_id,
            tree,
            focus,
            title,
            pinned,
            marked,
            private_title,
        });
        next_id += 1;
    }
    for session in 0..session_count {
        if claimed.insert(session) {
            tabs.push(Tab::new(next_id, session));
            next_id += 1;
        }
    }
    if tabs.is_empty() {
        // Zero tabs is not a renderable state.
        tabs.push(Tab::new(next_id, 0));
        next_id += 1;
    }

    let active_tab = saved_active_tab
        .filter(|index| *index < tabs.len())
        // No recorded tab (or it did not survive): follow the active session.
        .or_else(|| tabs.iter().position(|tab| tab.contains(active_session)))
        .unwrap_or(0);
    // A snapshot can interleave pinned and unpinned tabs (it predates pinning,
    // or an adopted orphan landed at the end). Restore the invariant here so
    // the strip never shows an order the app itself would not produce.
    let active_tab = sort_pinned_first(&mut tabs, active_tab);
    tabs[active_tab].repair_focus();
    (tabs, active_tab, next_id)
}

/// Re-index every tab's tree after a session was inserted at `inserted`.
///
/// Session indices are global, so an insert in the middle shifts the panes of
/// every tab, not just the active one.
fn reindex_tabs_for_insert(tabs: &mut [Tab], inserted: usize) {
    let remap = |s: usize| if s >= inserted { s + 1 } else { s };
    for tab in tabs {
        tab.tree.remap_sessions(&remap);
        tab.focus = remap(tab.focus);
    }
}

/// Re-index every tab's tree after the session at `removed` left the vector.
///
/// The tab owning that session drops its leaf — folding the freed share into a
/// neighbor — and then every tab shifts the indices above the removed slot
/// down. A tab whose only pane was the removed session keeps its (now stale)
/// leaf: emptying a tree is not representable, so the caller removes that tab
/// before calling here.
fn reindex_tabs_for_removal(tabs: &mut [Tab], removed: usize) {
    let remap = |s: usize| if s > removed { s - 1 } else { s };
    for tab in tabs {
        if tab.tree.contains_session(removed) && tab.tree.leaf_count() > 1 {
            tab.tree.remove_leaf(removed);
        }
        tab.tree.remap_sessions(&remap);
        tab.focus = remap(tab.focus);
        tab.repair_focus();
    }
}

/// Validate a candidate restored layout: every leaf session must be in range and
/// appear at most once, and the pane count must stay within `MAX_PANES`.
///
/// One leaf is valid — a tab with a single pane is the ordinary case now that
/// tabs own their panes. It used to be rejected because a one-leaf tree meant
/// "not split" back when there was a single global layout.
fn valid_restored_layout(tree: &PaneTree, session_count: usize) -> bool {
    let leaves = tree.leaves();
    let n = leaves.len();
    if !(1..=MAX_PANES).contains(&n) {
        return false;
    }
    if leaves.iter().any(|&s| s >= session_count) {
        return false;
    }
    let distinct = leaves
        .iter()
        .collect::<std::collections::HashSet<_>>()
        .len();
    distinct == n
}

/// Resolve the directional split edge under `point`. The center of a pane is
/// deliberately a dead zone: a release there cancels instead of guessing.
fn split_drop_direction(rect: pane_layout::Rect, point: iced::Point) -> Option<PaneDirection> {
    if !rect.x.is_finite()
        || !rect.y.is_finite()
        || !rect.width.is_finite()
        || !rect.height.is_finite()
        || !point.x.is_finite()
        || !point.y.is_finite()
        || rect.width <= 0.0
        || rect.height <= 0.0
        || point.x < rect.x
        || point.x >= rect.x + rect.width
        || point.y < rect.y
        || point.y >= rect.y + rect.height
    {
        return None;
    }

    let distances = [
        ((point.x - rect.x) / rect.width, PaneDirection::Left),
        (
            (rect.x + rect.width - point.x) / rect.width,
            PaneDirection::Right,
        ),
        ((point.y - rect.y) / rect.height, PaneDirection::Up),
        (
            (rect.y + rect.height - point.y) / rect.height,
            PaneDirection::Down,
        ),
    ];
    distances
        .into_iter()
        .min_by(|(a, _), (b, _)| a.total_cmp(b))
        .filter(|(distance, _)| *distance <= SPLIT_DROP_EDGE_FRACTION)
        .map(|(_, direction)| direction)
}

fn tab_drag_hover_ready(
    source_tab: Option<usize>,
    source_is_plain: bool,
    active_tab: Option<usize>,
    hovered_tab: Option<usize>,
    pending_target: usize,
    elapsed: std::time::Duration,
) -> bool {
    source_is_plain
        && source_tab.is_some_and(|source| source != pending_target)
        && active_tab != Some(pending_target)
        && hovered_tab == Some(pending_target)
        && elapsed >= std::time::Duration::from_millis(TAB_DRAG_HOVER_SWITCH_MS)
}

/// Move a one-pane tab into a directional edge of another tab's pane tree.
///
/// All fallible checks and tree construction happen before the source tab is
/// removed. The session itself is never cloned or reindexed: only its single
/// layout leaf changes owner.
fn move_plain_tab_into_split(
    tabs: &mut Vec<Tab>,
    source_tab_id: usize,
    target_session: usize,
    direction: PaneDirection,
) -> Option<(usize, usize)> {
    let source_index = tabs.iter().position(|tab| tab.id == source_tab_id)?;
    let source_session = match tabs.get(source_index)?.tree {
        PaneTree::Leaf(session) => session,
        PaneTree::Split { .. } => return None,
    };
    if source_session == target_session {
        return None;
    }

    // Reject corrupt/ambiguous ownership rather than turning one bad leaf into
    // a duplicated live PTY in two visible locations.
    let source_claims = tabs
        .iter()
        .flat_map(Tab::sessions)
        .filter(|session| *session == source_session)
        .count();
    let target_claims = tabs
        .iter()
        .flat_map(Tab::sessions)
        .filter(|session| *session == target_session)
        .count();
    if source_claims != 1 || target_claims != 1 {
        return None;
    }

    let target_index = tabs.iter().position(|tab| tab.contains(target_session))?;
    if source_index == target_index || tabs[target_index].tree.leaf_count() >= MAX_PANES {
        return None;
    }
    let mut next_tree = tabs[target_index].tree.clone();
    if !next_tree.split_leaf(target_session, direction.axis(), source_session) {
        return None;
    }
    if !direction.forward() {
        swap_sessions_in_tree(&mut next_tree, target_session, source_session);
    }

    // Commit. All identities used below were proven unique above.
    tabs.remove(source_index);
    let target_index = target_index - usize::from(source_index < target_index);
    tabs[target_index].tree = next_tree;
    tabs[target_index].focus = source_session;
    Some((target_index, source_session))
}

/// Detach one leaf from a split and promote it to a new ordinary tab. The
/// source tree is cloned and collapsed before either live collection changes,
/// making every invalid/stale drop a true no-op.
fn promote_split_pane_to_tab(
    tabs: &mut Vec<Tab>,
    next_tab_id: &mut usize,
    source_session: usize,
    after_tab_id: Option<usize>,
) -> Option<(usize, usize)> {
    if tabs
        .iter()
        .flat_map(Tab::sessions)
        .filter(|session| *session == source_session)
        .count()
        != 1
    {
        return None;
    }
    let owner_index = tabs.iter().position(|tab| tab.contains(source_session))?;
    if tabs[owner_index].tree.is_leaf() {
        return None;
    }
    let anchor_index = match after_tab_id {
        Some(id) => tabs.iter().position(|tab| tab.id == id)?,
        None => owner_index,
    };
    let id = *next_tab_id;
    let next_id = id.checked_add(1)?;
    if tabs.iter().any(|tab| tab.id == id) {
        return None;
    }

    let mut source_tree = tabs[owner_index].tree.clone();
    if !source_tree.remove_leaf(source_session) || source_tree.contains_session(source_session) {
        return None;
    }

    // Commit. New tabs are unpinned, so never insert one inside the leading
    // pinned block even if the pointer was released over a pinned tab.
    tabs[owner_index].tree = source_tree;
    tabs[owner_index].repair_focus();
    let first_unpinned = tabs
        .iter()
        .position(|tab| !tab.pinned)
        .unwrap_or(tabs.len());
    let insert_at = (anchor_index + 1).max(first_unpinned).min(tabs.len());
    tabs.insert(insert_at, Tab::new(id, source_session));
    *next_tab_id = next_id;
    Some((insert_at, source_session))
}

/// Linear blend between two colors (t=0 → a, t=1 → b); result is fully opaque.
fn blend(a: Color, b: Color, t: f32) -> Color {
    Color {
        r: a.r + (b.r - a.r) * t,
        g: a.g + (b.g - a.g) * t,
        b: a.b + (b.b - a.b) * t,
        a: 1.0,
    }
}

fn resolve_mono_font(family: &str) -> iced::Font {
    let f = family.trim();
    if f.is_empty() {
        iced::Font::MONOSPACE
    } else {
        // iced stores family names as `&'static str`. Intern each distinct name
        // once so repeatedly applying settings does not leak another allocation.
        static INTERNED_FONTS: once_cell::sync::Lazy<
            Mutex<std::collections::HashMap<String, &'static str>>,
        > = once_cell::sync::Lazy::new(|| Mutex::new(std::collections::HashMap::new()));
        let mut names = INTERNED_FONTS
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let name = *names
            .entry(f.to_string())
            .or_insert_with(|| Box::leak(f.to_string().into_boxed_str()));
        iced::Font::with_name(name)
    }
}

fn resolve_optional_font(family: Option<&str>) -> Option<iced::Font> {
    family.map(resolve_mono_font)
}

fn main() -> iced::Result {
    // Shared jterm_core modules brand themselves per app (env prefixes,
    // prompt strings) from this identity.
    jterm_core::identity::init(jterm_core::identity::AppIdentity {
        app_name: "frost",
        app_id: "io.github.beamiter.frost",
        // From the app crate, not jterm_core's: a shell that pairs
        // TERM_PROGRAM with TERM_PROGRAM_VERSION must read this binary's
        // version, not the shared library's.
        app_version: env!("CARGO_PKG_VERSION"),
    });
    env_logger::init();
    let config::ConfigLoad {
        config,
        diagnostic: config_diagnostic,
        revision: config_revision,
    } = Config::load_with_diagnostics();
    let win = iced::window::Settings {
        size: Size::new(config.initial_width, config.initial_height),
        // Without this the window ships an empty WM_CLASS/app_id, so neither
        // X11 nor Wayland can tie it to the installed .desktop entry: the shell
        // shows an unbranded window that cannot be pinned. iced feeds this to
        // winit as both the X11 WM_CLASS pair and the Wayland app_id.
        platform_specific: iced::window::settings::PlatformSpecific {
            application_id: WINDOW_APP_ID.to_string(),
            ..Default::default()
        },
        // The .desktop entry only covers windows the shell can match to it.
        // Without an icon of its own the window has no _NET_WM_ICON at all, so
        // anything else — a bare `cargo run`, a session where the entry is not
        // installed — falls back to a blank placeholder in the dock and the
        // window switcher.
        icon: match iced::window::icon::from_file_data(
            WINDOW_ICON_PNG,
            Some(image::ImageFormat::Png),
        ) {
            Ok(icon) => Some(icon),
            Err(err) => {
                log::warn!("Failed to decode the embedded window icon: {err}");
                None
            }
        },
        // Route window-manager close requests through our foreground-job guard.
        exit_on_close_request: false,
        // Ask for an alpha-capable surface so the configured background opacity
        // (config `opacity`, Ctrl+Alt+=/-) can actually show the desktop
        // through the window, matching the rest of the jterm family.
        transparent: true,
        // Draw our own title bar. GNOME and other wlroots-style compositors
        // offer no server-side decorations, so winit falls back to drawing one
        // with `sctk-adwaita`, whose renderer maps the whole title through a
        // single system font with no fallback chain — every CJK codepoint came
        // out as .notdef. frost's own chrome uses the configured CJK fallback
        // font, so the title renders like the rest of the UI. See
        // `Frost::top_bar_with_close` and `Frost::window_resize_edges`.
        decorations: false,
        ..Default::default()
    };
    iced::application(
        move || {
            Frost::new(
                config.clone(),
                config_diagnostic.clone(),
                config_revision.clone(),
            )
        },
        Frost::update,
        Frost::view,
    )
    .title(Frost::title)
    .subscription(Frost::subscription)
    .theme(Frost::iced_theme)
    .style(Frost::app_style)
    .scale_factor(Frost::scale_factor)
    // MSAA forces wgpu down the multisample path; on Intel/Mesa that triggers
    // the "manual shader clears for srgb textures" path, which flashes the whole
    // surface on heavy redraws (e.g. multi-line `ls` output). Glyph and quad
    // rendering don't benefit from geometry MSAA, so disabling it is free here.
    .antialiasing(false)
    .window(win)
    .run()
}

#[derive(Debug, Clone)]
enum Message {
    // AI agent panel (per-command approval agent over jterm_core).
    AgentInput(String),
    AgentSubmit,
    AgentApprove(jterm_core::agent::ProposalId),
    AgentEditStart(jterm_core::agent::ProposalId, String),
    AgentEditInput(String),
    AgentEditApprove(jterm_core::agent::ProposalId),
    AgentEditCancel,
    AgentReject(jterm_core::agent::ProposalId),
    /// One streamed fragment of the in-flight model reply for the given
    /// request task epoch/generation (stale ones dropped).
    AgentModelDelta(agent::ModelRequestIdentity, String),
    /// One model reply for the given request identity (stale ones dropped).
    AgentModelReply(agent::ModelRequestIdentity, Result<String, String>),
    AgentContinueTask,
    AgentNewTask,
    AgentClearContext,
    AgentClose,
    // Experimental Tasks dashboard (native Codex runtime + isolated worktrees).
    TaskPanelToggle,
    TaskSelect(agent_task::TaskId),
    TaskHide(agent_task::TaskId),
    TaskStartCodex(agent_task::TaskId),
    TaskCancelCodex(agent_task::TaskId),
    TaskFinishCodex(agent_task::TaskId),
    TaskFollowUpInput(String),
    TaskFollowUpSend(agent_task::TaskId),
    TaskApprovalDeny(agent_task::TaskId, agent_task::ApprovalId),
    TaskDiffOpen(agent_task::TaskId),
    TaskDiffClose,
    TaskValidationStart(agent_task::TaskId),
    TaskMarkComplete(agent_task::TaskId),
    TaskTerminalOpen(agent_task::TaskId),
    /// Periodic reducer/driver poll while the dashboard is open or a provider
    /// session is active.
    TaskTick,
    /// Result of the background jsh update check (boxed: one rare message must
    /// not widen every other variant).
    JshChecked(Box<jterm_core::jsh_install::Status>),
    /// Install jsh, or update the installed one, in a dedicated session.
    JshInstall,
    /// Close the remote host picker overlay.
    RemotePickerClose,
    /// Open the picked `[[remote_hosts]]` entry in a new session.
    RemotePickerConnect(usize),
    /// Per-field edits of the indexed `[[remote_hosts]]` entry from Settings.
    RemoteHostName(usize, String),
    RemoteHostHost(usize, String),
    RemoteHostUser(usize, String),
    RemoteHostDocker(usize, bool),
    RemoteHostDeploy(usize, String),
    /// Append a template `[[remote_hosts]]` entry for in-place editing.
    RemoteHostAdd,
    RemoteHostRemove(usize),
    /// Hide the jsh notice until the next launch.
    JshNoticeDismiss,
    SetAiEnabled(bool),
    SetAiProvider(String),
    SetAiModel(String),
    SetAiBaseUrl(String),
    SetAiMaxTokens(u32),
    SetAiTemperature(String),
    SetAiRedactSecrets(bool),
    SetAiShareCommandContext(bool),
    SetExperimentalTaskSidebar(bool),
    SetAiStream(bool),
    SetAiKeyFile(String),
    SetAiKeyDraft(String),
    StoreAiKey,
    SetAgentMaxTurns(u32),
    PtyOutput(usize, RawFd, Vec<u8>),
    PtyExited(usize, RawFd, i32),
    Key(keyboard::Event),
    /// An input-method (IME) composition event: open/close, pre-edit updates,
    /// and committed text.
    Ime(iced::advanced::input_method::Event),
    ModifiersChanged(keyboard::Modifiers),
    /// A mouse interaction within the pane showing the stable session id.
    MousePane(usize, MouseInput),
    /// Stable release of a host-owned collapsed-output summary row.
    SummaryActivate(usize, SummaryActivation),
    /// Clipboard result scoped to the stable session that requested the paste.
    Pasted(usize, Option<String>),
    /// One decoded local path delivered by the window system's file-drop event.
    ImageDropped(std::path::PathBuf),
    /// Text appended by search-replace or a sidebar path pick. Both sources
    /// can contain terminal/file-system controlled bytes, so this is an
    /// untrusted paste and never receives the control-preserving policy.
    PromptInsert(usize, String),
    /// A command recalled from history onto the prompt. Unlike `PromptInsert`
    /// it kills the pending line first: a recall that merely appends is glued
    /// to whatever the user had half-typed (the failure mode forge shipped),
    /// and the mangled line is one Enter away from running.
    PromptRecall(usize, String),
    /// System clipboard contents read in response to an OSC 52 query from the
    /// app running in the session identified by the file descriptor.
    Osc52Query(usize, RawFd, Option<String>),
    /// System clipboard contents read in response to an OSC 5522 MIME-data read
    /// request. Carries the requesting fd and the MIME type that was requested.
    Osc5522Data(usize, RawFd, String, Option<String>),
    Resized(Size),
    Focus(bool),
    NewSession,
    /// Close the tab with this stable session id.
    CloseTab(usize),
    WindowClose,
    /// Title-bar press on empty chrome: hand the window to the compositor to
    /// be moved. The window is undecorated, so nothing else offers this.
    WindowDrag,
    /// Press on one of the invisible edge/corner strips: resize in that
    /// direction for as long as the button is held.
    WindowResizeDrag(iced::window::Direction),
    WindowMinimize,
    WindowToggleMaximize,
    TabHover(Option<usize>),
    /// User pressed the mouse over a tab — start tracking its stable tab id.
    TabDragStart(usize),
    /// User released the mouse over a tab. Both endpoints are stable tab ids.
    TabDragEnd(usize),
    /// Pointer movement over the pane area while an ordinary tab is held.
    TabDragMove(iced::Point),
    /// Pointer left the pane area; clear its directional preview, not the drag.
    TabDragLeavePaneArea,
    /// Short-lived timer that opens a tab only after a deliberate drag hover.
    TabDragHoverTick,
    /// Release over a highlighted pane edge commits the tab-to-split move.
    TabSplitDrop,
    /// Global mouse-up: clear `dragging_tab` if a drag was started but the
    /// release happened outside any tab.
    TabDragCancel,
    ToggleSidebar,
    SetSidebarPanel(SidebarPanel),
    SetTabPosition(config::TabPosition),
    SidebarDragStart,
    SidebarDragMove(iced::Point),
    SidebarDragEnd,
    SidebarToggleNode(std::path::PathBuf),
    SidebarInsertPath(std::path::PathBuf),
    SidebarGoParent,
    SidebarRefresh,
    SidebarLoaded(sidebar::DirectoryResult),
    /// Switch the file tree between Local and a configured remote host.
    SidebarSetLocation(remote_fs::FsLocation),
    /// Async start-directory resolution for a location switch (generation-guarded).
    SidebarLocationResolved(u64, Result<std::path::PathBuf, String>),
    /// Pointer enter/exit on the dock gates the menu-anchor tracking below.
    SidebarHover(bool),
    /// Window-space pointer position while hovering the dock; the file-ops
    /// menu anchors to the last one seen before the right-press.
    SidebarPointerMoved(iced::Point),
    /// Right-press on a tree row: open the file-ops menu for that node
    /// (path, is_dir).
    SidebarMenuOpen(std::path::PathBuf, bool),
    /// Right-press on the empty area below the tree: menu for the root dir.
    SidebarMenuOpenRoot,
    SidebarMenuClose,
    SidebarMenuAction(SidebarMenuAction),
    SidebarDialogInput(String),
    SidebarDialogSubmit,
    SidebarDialogCancel,
    SidebarDeleteConfirm,
    SidebarDeleteCancel,
    SidebarOpFinished(SidebarOpReport),
    /// Press on a divider (identified by its owning split node + gap).
    DividerDragStart(DividerId),
    DividerDragMove(iced::Point),
    DividerDragEnd,
    DividerHover(Option<DividerId>),
    /// Press on a pane's header strip, identified by stable session id: focuses
    /// it, and may become a drag that swaps it with the release target.
    PaneDragStart(usize),
    PaneDragMove(iced::Point),
    /// Pointer left the pane area for the tab strip; preserve the source drag.
    PaneDragLeavePaneArea,
    PaneDragEnd,
    /// Release a split pane on tab chrome, optionally after a specific tab.
    PanePromoteToTab(Option<usize>),
    SearchToggleRegex,
    SearchToggleCase,
    SearchInput(String),
    SearchReplaceFindInput(String),
    SearchReplaceReplaceInput(String),
    SearchReplaceToggleRegex,
    SearchReplaceToggleCase,
    SearchReplaceToggleWord,
    SearchReplaceToggleAll,
    /// Run the Find & Replace panel against the current selection and route
    /// the result to the clipboard or the prompt.
    SearchReplaceApply(search_replace_panel::SearchReplaceAction),
    SearchReplaceClose,
    PaletteInput(String),
    PaletteExecute(usize),
    ToggleConfigPanel,
    SetTheme(String),
    SetFontSize(f32),
    SetUiScale(f32),
    SetLineSpacing(f32),
    SetPadding(f32),
    SetOpacity(f32),
    SetScrollback(u32),
    SetScrollSpeed(u32),
    SetFontFamily(String),
    SetScrollbarAlways(bool),
    SetDisableAltScreen(bool),
    SetAllowClipboardRead(bool),
    SetNotifyLongBlocks(bool),
    SetBlockMode(bool),
    SetBlockCompact(bool),
    SetShowRepoStrip(bool),
    SetBottomBar(bool),
    ThemeEditOpen,
    ThemeEditClose,
    ThemeEditName(String),
    ThemeEditColor(usize, String),
    ThemeEditSave,
    ThemeDelete(String),
    ConfigSave,
    ConfigReset,
    ConfigTick,
    BlinkTick,
    /// Redraw the compact elapsed-time badge on a visible running block.
    BlockElapsedTick,
    PtyWriteTick,
    SearchRefreshTick,
    HistoryReflowTick,
    /// Right-click on a tab opened its context menu (close/duplicate/etc).
    TabMenuOpen(usize),
    /// Dismiss the tab context menu without an action.
    TabMenuClose,
    /// Execute a menu action against the target tab.
    TabMenuAction(TabMenuAction),
    /// Pointer moved while a tab was hovered (window logical coordinates).
    /// Only subscribed to while the pointer is actually over a tab.
    TabPointerMoved(iced::Point),
    /// Turn the open tab menu into a rename editor for that tab.
    TabRenameStart(usize),
    /// Rename draft edited.
    TabRenameInput(String),
    /// Commit the rename draft (Enter or the Rename button).
    TabRenameSubmit,
    /// Toast queue tick (drop expired entries).
    ToastTick,
    /// Dismiss a specific toast by index.
    ToastDismiss(usize),
    /// Filter text changed in the tab switcher.
    TabSwitcherInput(String),
    /// Cancel the tab switcher overlay.
    TabSwitcherClose,
    /// Jump to the given stable session id from the tab switcher (and close it).
    TabSwitcherJump(usize),
    /// Filter text changed in the history picker.
    HistoryPickerInput(String),
    /// Cancel the history picker overlay.
    HistoryPickerClose,
    /// Type the clicked command into the active pane's prompt (and close).
    HistoryPickerAccept(String),
    /// Query text changed in the block search picker.
    BlockSearchInput(String),
    /// Restrict cross-block search/browse by block metadata.
    BlockSearchSetFilter(BlockSearchFilter),
    /// Cancel the block search picker overlay.
    BlockSearchClose,
    /// A bounded background cache build completed. Session + monotonic epoch
    /// are both required before its data can replace the live picker cache.
    BlockSearchCacheBuilt(
        BlockSearchBuildIdentity,
        Result<block_mode::BlockSearchCacheBuild, String>,
    ),
    /// Select and reveal the clicked hit's zone (and close the picker).
    BlockSearchAccept(block_mode::BlockSearchHit),
    /// Dismiss or execute the completed-card action menu.
    BlockMenuClose,
    BlockMenuAction(BlockMenuAction),
    /// Confirm or cancel permanently clearing the active pane's completed
    /// block records. Every entry point opens this same counted modal.
    BlockClearConfirmYes,
    BlockClearConfirmNo,
    /// Background whole-session export completed.
    BlockExportFinished(
        block_export::SessionExportFormat,
        Result<std::path::PathBuf, String>,
    ),
    /// User confirmed closing a tab with a running foreground process.
    TabCloseConfirmYes,
    /// User cancelled the close-confirmation overlay.
    TabCloseConfirmNo,
}

/// Context-menu actions that target a stable tab id.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TabMenuAction {
    Close(usize),
    CloseOthers(usize),
    CloseToRight(usize),
    /// Close every marked tab at once (marking is the multi-select model).
    CloseMarked,
    Duplicate(usize),
    NewTab,
    TogglePinned(usize),
    ToggleMarked(usize),
    TogglePrivateTitle(usize),
    /// Open a `[[remote_hosts]]` entry (by config index) in a new tab.
    ConnectRemote(usize),
}

/// Subscription identity plus a reader descriptor duplicated synchronously when
/// the session is created. Equality/hash intentionally ignore the descriptor
/// object: the monotonic session id and original fd identify the iced stream.
#[derive(Clone)]
struct PtySubscriptionKey {
    id: usize,
    master_fd: RawFd,
    reader_fd: Arc<OwnedFd>,
}

struct PtyWriteChunk {
    data: Vec<u8>,
    response: bool,
}

impl PartialEq for PtySubscriptionKey {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id && self.master_fd == other.master_fd
    }
}

impl Eq for PtySubscriptionKey {}

impl Hash for PtySubscriptionKey {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.id.hash(state);
        self.master_fd.hash(state);
    }
}

/// In-progress custom theme being edited in the theme editor overlay. UI-chrome
/// colors are inherited from `base`; only the terminal palette is editable here.
struct ThemeEditState {
    base: Theme,
    name: String,
    /// Hex buffers aligned with `Theme::editable_color_labels()` (19 entries).
    hexes: Vec<String>,
    error: Option<String>,
}

/// A single terminal session: its own PTY child and terminal state.
struct Session {
    id: usize,
    terminal: TerminalState,
    pty: Pty,
    master_fd: RawFd,
    reader_fd: Arc<OwnedFd>,
    /// One immutable materialization shared by rendering, links, and stable
    /// coordinate consumers for this refresh.
    projection: Arc<ProjectedViewport>,
    projection_block_mode: bool,
    /// User-owned transforms survive Block Mode/alternate-screen bypass. The
    /// companion view state owns scrolling only while a transform is active.
    projection_policy: ProjectionPolicy,
    projection_view_state: ProjectionViewState,
    cursor: (usize, usize),
    cursor_visible: bool,
    /// Cached working directory, refreshed periodically so the status bar can
    /// display it without a `readlink` syscall on every render frame.
    cwd_cache: Option<String>,
    /// Cached foreground process name (via `tcgetpgrp` + `/proc/<pgid>/comm`),
    /// refreshed on the same cadence as `cwd_cache`. Empty/None when the
    /// shell itself is in the foreground so tab labels can hide it.
    fg_proc_cache: Option<String>,
    /// Git metadata for `cwd_cache`, refreshed on the same cadence (plus once
    /// when a command finishes) via the coalesced background probe in
    /// `jterm_core::git_meta` — the pane header and bottom bar only ever read
    /// this cache, so git never runs per frame. None outside a repo.
    git_meta_cache: Option<jterm_core::git_meta::RepoMeta>,
    /// Exit code of the last OSC 133 command that finished in this session,
    /// retained for the bottom bar. None until a command completes (or when
    /// the shell omitted the code).
    last_exit: Option<i32>,
    /// Task-bound terminals (Agent CLI fallback, validation runs) stay open
    /// after the child exits so their transcript remains reviewable; ordinary
    /// sessions still close on exit. Set when the exit reducer ran.
    hold_after_exit: bool,
    /// Wall-clock duration of that command, when the shell reported an
    /// execution phase.
    last_duration_ms: Option<u64>,
    /// Warp-style block-mode multi-selection, keyed by stable command-zone id.
    /// It survives scrollback row trimming; the terminal's bounded zone cap is
    /// reconciled before actions. Native text/prompt presses or PTY-bound input
    /// clears the whole selection.
    block_selection: block_mode::BlockSelection,
    /// Pane-local bookmarks for important finalized blocks. Reconciled against
    /// the bounded zone deque before paint/navigation and cleared with blocks.
    block_bookmarks: block_mode::BlockBookmarks,
    /// Non-blocking PTY writes may be partial. Keep the remainder here and let a
    /// short-lived timer drain it without ever stalling iced's UI thread.
    write_queue: std::collections::VecDeque<PtyWriteChunk>,
    write_queue_offset: usize,
    queued_write_bytes: usize,
    queued_response_bytes: usize,
    /// Host clipboard access is asynchronous. Limit PTY-originated reads to one
    /// per session so a hostile child cannot accumulate work across UI batches.
    clipboard_read_in_flight: bool,
}

fn agent_prompt_status_with_foreground(
    terminal_status: terminal::AgentPromptStatus,
    shell_pid: i32,
    foreground_pgid: Option<i32>,
) -> terminal::AgentPromptStatus {
    if !terminal_status.is_ready() {
        return terminal_status;
    }
    match foreground_pgid {
        Some(pgid) if pgid == shell_pid => terminal::AgentPromptStatus::Ready,
        Some(_) => terminal::AgentPromptStatus::Busy,
        None => terminal::AgentPromptStatus::ShellIntegrationUnavailable,
    }
}

/// Suffix appended to a session's label while it shows a held-open task
/// transcript, so the dead tab reads differently from a live shell wherever
/// the label appears (tab strip, dock, window title).
const READ_ONLY_LABEL_SUFFIX: &str = " (exited)";

/// Whether a session in this state refuses PTY-bound user bytes: a held-open
/// task transcript's child has exited (that is exactly what `hold_after_exit`
/// records), so its bytes would only hit EIO on the dead master fd. Ordinary
/// sessions forward everything.
fn user_input_blocked(hold_after_exit: bool) -> bool {
    hold_after_exit
}

/// A session's strip/window label with the read-only suffix applied when the
/// session shows a held-open task transcript.
fn session_label(base: String, transcript_read_only: bool) -> String {
    if transcript_read_only {
        format!("{base}{READ_ONLY_LABEL_SUFFIX}")
    } else {
        base
    }
}

impl Session {
    fn spawn(
        config: &Config,
        id: usize,
        cols: usize,
        rows: usize,
        cwd: Option<&str>,
    ) -> anyhow::Result<Session> {
        Self::spawn_argv(config, id, cols, rows, cwd, None)
    }

    /// Spawn a session that runs an explicit argv instead of the configured
    /// shell — used for one-shot helpers such as the jsh installer.
    fn spawn_argv(
        config: &Config,
        id: usize,
        cols: usize,
        rows: usize,
        cwd: Option<&str>,
        command_argv: Option<&[String]>,
    ) -> anyhow::Result<Session> {
        Self::spawn_argv_env(config, id, cols, rows, cwd, command_argv, &[])
    }

    /// [`Self::spawn_argv`] plus explicit child-environment overrides. The
    /// task-validation terminal uses this to neutralize shell startup files.
    fn spawn_argv_env(
        config: &Config,
        id: usize,
        cols: usize,
        rows: usize,
        cwd: Option<&str>,
        command_argv: Option<&[String]>,
        extra_env: &[(&str, &str)],
    ) -> anyhow::Result<Session> {
        let pty = Pty::new_with_cwd_env(
            cols,
            rows,
            cwd,
            None,
            config.shell.as_deref(),
            command_argv,
            extra_env,
        )
        .map_err(|error| anyhow::anyhow!("cannot create terminal session: {error}"))?;
        let master_fd = pty.master_fd();
        let reader_fd = unsafe { libc::fcntl(master_fd, libc::F_DUPFD_CLOEXEC, 0) };
        if reader_fd < 0 {
            return Err(anyhow::anyhow!(
                "cannot duplicate PTY reader: {}",
                std::io::Error::last_os_error()
            ));
        }
        // SAFETY: `fcntl(F_DUPFD_CLOEXEC)` returned a fresh owned descriptor.
        let reader_fd = Arc::new(unsafe { OwnedFd::from_raw_fd(reader_fd) });
        let mut terminal = TerminalState::new(cols, rows);
        terminal.set_max_scrollback(config.scrollback_lines);
        terminal.set_disable_alt_screen(config.disable_alt_screen);
        let projection_policy = ProjectionPolicy::new();
        let mut projection_view_state = ProjectionViewState::new();
        let projection = terminal.get_projected_viewport_with_state(
            config.block_mode,
            &projection_policy,
            &mut projection_view_state,
        );
        let cursor = terminal.get_cursor_pos();
        let cursor_visible = terminal.is_cursor_visible();
        Ok(Session {
            id,
            terminal,
            pty,
            master_fd,
            reader_fd,
            projection,
            projection_block_mode: config.block_mode,
            projection_policy,
            projection_view_state,
            cursor,
            cursor_visible,
            cwd_cache: None,
            fg_proc_cache: None,
            git_meta_cache: None,
            last_exit: None,
            hold_after_exit: false,
            last_duration_ms: None,
            block_selection: block_mode::BlockSelection::default(),
            block_bookmarks: block_mode::BlockBookmarks::default(),
            write_queue: std::collections::VecDeque::new(),
            write_queue_offset: 0,
            queued_write_bytes: 0,
            queued_response_bytes: 0,
            clipboard_read_in_flight: false,
        })
    }

    /// Tab label: prefer an OSC-set window title; otherwise show the foreground
    /// process and/or cwd basename so a fresh shell with no title still tells
    /// the user where they are. Falls back to "Session N" only when none of
    /// those are known yet.
    fn label(&self) -> String {
        session_label(self.label_base(), self.transcript_read_only())
    }

    fn label_base(&self) -> String {
        let t = self.terminal.window_title.trim();
        if !t.is_empty() {
            return t.to_string();
        }
        let cwd_short = self.cwd_cache.as_deref().and_then(Self::cwd_basename);
        match (&self.fg_proc_cache, cwd_short) {
            (Some(p), Some(d)) => format!("{p} · {d}"),
            (Some(p), None) => p.clone(),
            (None, Some(d)) => d,
            (None, None) => format!("Session {}", self.id + 1),
        }
    }

    /// Pane-header title. Same preference order as [`Session::label`], minus
    /// the foreground process: the header shows that in its own field, and
    /// repeating it there would crowd out the directory.
    fn pane_title(&self) -> String {
        let title = self.terminal.window_title.trim();
        if !title.is_empty() {
            return title.to_string();
        }
        self.cwd_cache
            .as_deref()
            .and_then(Self::cwd_basename)
            .unwrap_or_else(|| format!("Session {}", self.id + 1))
    }

    /// Full working directory for the pane header, with `$HOME` collapsed to
    /// `~`. `None` while the cwd is unknown.
    fn cwd_display(&self) -> Option<String> {
        let cwd = self.cwd_cache.as_deref()?;
        let Some(home) = std::env::var_os("HOME") else {
            return Some(cwd.to_string());
        };
        let home = home.to_string_lossy();
        if home.is_empty() {
            return Some(cwd.to_string());
        }
        if cwd == home {
            return Some("~".to_string());
        }
        // Only substitute at a component boundary: `/home/user2` merely shares
        // a prefix with `/home/user` and is a different directory.
        match cwd.strip_prefix(home.as_ref()) {
            Some(rest) if rest.starts_with('/') => Some(format!("~{rest}")),
            _ => Some(cwd.to_string()),
        }
    }

    /// Git metadata for the pane header and bottom bar, probed through the
    /// coalesced background worker in `jterm_core::git_meta` (bounded UI wait,
    /// git runs off-thread). Callers cache the result; None outside a
    /// repository or while `cwd_cache` is unknown.
    fn git_meta(&self) -> Option<jterm_core::git_meta::RepoMeta> {
        let cwd = self.cwd_cache.as_deref()?;
        jterm_core::git_meta::read(std::path::Path::new(cwd))
    }

    /// Short, human-friendly form of an absolute cwd: "~" for $HOME, just the
    /// basename otherwise. Returns None for "/" or unparsable paths.
    fn cwd_basename(cwd: &str) -> Option<String> {
        if let Some(home) = std::env::var_os("HOME") {
            let home = home.to_string_lossy();
            if cwd == home {
                return Some("~".to_string());
            }
        }
        let p = std::path::Path::new(cwd);
        p.file_name()
            .and_then(|n| n.to_str())
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
    }

    /// Foreground process name on the PTY, or None when it's the shell itself
    /// (so the tab label doesn't redundantly show "bash" / "zsh" / "fish").
    fn fg_proc(&self) -> Option<String> {
        let pgid = jterm_core::process::tty_foreground_pgid(self.master_fd)?;
        // Hide when the foreground process *is* the shell — that's the idle case.
        if pgid == self.pty.get_child_pid() {
            return None;
        }
        let comm = jterm_core::process::process_comm(pgid)?;
        // App policy: also hide other interactive shells so the tab label never
        // redundantly shows a shell name (e.g. a nested `zsh` under bash).
        const SHELLS: &[&str] = &["bash", "zsh", "fish", "sh", "dash", "ksh", "tcsh"];
        if SHELLS.contains(&comm.as_str()) {
            return None;
        }
        Some(comm)
    }

    /// Agent approval requires both a trusted terminal-state boundary and the
    /// interactive shell itself owning the foreground process group. OSC 133
    /// emitted by a running editor/test process must not make that process the
    /// recipient of an approved shell command.
    fn agent_prompt_status(&mut self) -> terminal::AgentPromptStatus {
        let status = self.terminal.agent_prompt_status();
        if !status.is_ready() {
            return status;
        }
        if !self.pty.is_alive() {
            return terminal::AgentPromptStatus::ShellIntegrationUnavailable;
        }
        agent_prompt_status_with_foreground(
            status,
            self.pty.get_child_pid(),
            jterm_core::process::tty_foreground_pgid(self.master_fd),
        )
    }

    fn refresh(&mut self) {
        // The identity/P0 hot path must not allocate or scan every retained
        // zone on each PTY batch. Reconcile only after a transform exists.
        if !self.projection_policy.is_identity() {
            let stale_collapses: Vec<u64> = self
                .projection_policy
                .collapsed_zone_ids()
                .filter(|id| {
                    self.terminal
                        .zone_by_id(*id)
                        .is_none_or(|zone| zone.rows_evicted)
                })
                .collect();
            for id in stale_collapses {
                self.projection_policy.expand(id);
            }
        }
        self.projection = self.terminal.get_projected_viewport_with_state(
            self.projection_block_mode,
            &self.projection_policy,
            &mut self.projection_view_state,
        );
        if self.projection.is_identity() {
            debug_assert_eq!(
                self.projection.uses_identity_fast_path(),
                self.terminal.scroll_offset == 0
            );
        }
        let raw_cursor = self.terminal.get_cursor_pos();
        if self.projection.mode() == terminal::ProjectionMode::Transformed {
            let absolute_row = self.terminal.scrollback_len().saturating_add(raw_cursor.0);
            let mapped = self
                .terminal
                .raw_cell_origin_at_absolute(absolute_row, raw_cursor.1)
                .and_then(|origin| self.projection.raw_to_view(origin));
            self.cursor = mapped
                .map(|cell| (cell.row, cell.col))
                .unwrap_or(raw_cursor);
            self.cursor_visible = self.terminal.is_cursor_visible() && mapped.is_some();
        } else {
            self.cursor = raw_cursor;
            self.cursor_visible = self.terminal.is_cursor_visible();
        }
    }

    fn scroll(&mut self, lines: isize) {
        if self.projection.mode() == terminal::ProjectionMode::Transformed {
            self.projection_view_state.scroll(lines, &self.projection);
        } else {
            self.terminal.scroll(lines);
        }
        self.refresh();
    }

    fn set_scroll_offset(&mut self, offset: usize) {
        if self.projection.mode() == terminal::ProjectionMode::Transformed {
            self.projection_view_state
                .set_offset(offset, &self.projection);
        } else {
            self.terminal.set_scroll_offset(offset);
        }
        self.refresh();
    }

    fn scroll_to_bottom(&mut self) {
        self.terminal.scroll_to_bottom();
        self.projection_view_state.scroll_to_bottom();
        self.refresh();
    }

    /// Navigate OSC 133 prompt boundaries in the currently displayed
    /// document. Transformed history owns a projected offset, so its prompt
    /// jumps reveal stable raw headers instead of mutating the dormant legacy
    /// scroll offset.
    fn jump_prompt(&mut self, older: bool) -> bool {
        if self.projection.mode() != terminal::ProjectionMode::Transformed {
            let moved = if older {
                self.terminal.jump_to_prev_prompt()
            } else {
                self.terminal.jump_to_next_prompt()
            };
            if moved {
                self.refresh();
            }
            return moved;
        }

        let top = self
            .projection
            .row_kinds()
            .iter()
            .enumerate()
            .skip(self.projection.top_padding())
            .find_map(|(row, kind)| match kind {
                terminal::ProjectedRowKind::Raw => self.projection.view_row_absolute(row),
                terminal::ProjectedRowKind::CollapsedSummary { key, .. } => self
                    .terminal
                    .zone_by_id(key.zone_id)
                    .and_then(|zone| zone.output_start)
                    .or_else(|| {
                        self.terminal
                            .zone_by_id(key.zone_id)
                            .map(|zone| zone.prompt_start)
                    }),
                terminal::ProjectedRowKind::Padding => None,
            })
            .unwrap_or_else(|| self.terminal.scrollback_len());
        let active_prompt = self
            .terminal
            .running_zone_start()
            .or(self.terminal.live_prompt_row());
        let target = prompt_jump_target(
            self.terminal
                .command_zones
                .iter()
                .filter(|zone| !zone.rows_evicted)
                .map(|zone| zone.prompt_start)
                .chain(active_prompt),
            top,
            older,
        );

        match target {
            Some(row) => {
                let Some(origin) = self.terminal.raw_cell_origin_at_absolute(row, 0) else {
                    return false;
                };
                let moved = self.terminal.reveal_raw_cell_in_projection(
                    &self.projection_policy,
                    &mut self.projection_view_state,
                    origin,
                );
                if moved {
                    self.refresh();
                }
                moved
            }
            None if !older && self.projection.scroll_offset() > 0 => {
                self.scroll_to_bottom();
                true
            }
            None => false,
        }
    }

    fn reveal_absolute_cell(&mut self, absolute_row: usize, col: usize) -> bool {
        let Some(origin) = self.terminal.raw_cell_origin_at_absolute(absolute_row, col) else {
            return false;
        };
        match self
            .terminal
            .locate_raw_cell_in_projection(&self.projection, origin)
        {
            terminal::ProjectedRawCellLocation::Visible(_) => true,
            terminal::ProjectedRawCellLocation::Retained => {
                let revealed = self.terminal.reveal_raw_cell_in_projection(
                    &self.projection_policy,
                    &mut self.projection_view_state,
                    origin,
                );
                if revealed {
                    self.refresh();
                }
                revealed
            }
            terminal::ProjectedRawCellLocation::Hidden { .. }
            | terminal::ProjectedRawCellLocation::Unmapped => false,
        }
    }

    fn queue_accepts_entry(
        queue: &std::collections::VecDeque<PtyWriteChunk>,
        len: usize,
        response: bool,
    ) -> bool {
        queue.len() < MAX_PTY_QUEUE_ENTRIES
            || queue.back().is_some_and(|back| {
                back.response == response
                    && len <= PTY_QUEUE_COALESCE_BYTES.saturating_sub(back.data.len())
            })
    }

    fn push_queue_owned(
        queue: &mut std::collections::VecDeque<PtyWriteChunk>,
        data: Vec<u8>,
        response: bool,
    ) {
        let coalesce = queue.back().is_some_and(|back| {
            back.response == response
                && data.len() <= PTY_QUEUE_COALESCE_BYTES.saturating_sub(back.data.len())
        });
        if coalesce {
            if let Some(back) = queue.back_mut() {
                back.data.extend_from_slice(&data);
                return;
            }
        }
        queue.push_back(PtyWriteChunk { data, response });
    }

    fn push_queue_copy(
        queue: &mut std::collections::VecDeque<PtyWriteChunk>,
        data: &[u8],
        response: bool,
    ) {
        let coalesce = queue.back().is_some_and(|back| {
            back.response == response
                && data.len() <= PTY_QUEUE_COALESCE_BYTES.saturating_sub(back.data.len())
        });
        if coalesce {
            if let Some(back) = queue.back_mut() {
                back.data.extend_from_slice(data);
                return;
            }
        }
        queue.push_back(PtyWriteChunk {
            data: data.to_vec(),
            response,
        });
    }

    fn flush_responses(&mut self) {
        let out = self.terminal.get_output();
        if out.is_empty() {
            return;
        }
        if !self.flush_write_queue() {
            return;
        }
        if out.len() > MAX_PTY_RESPONSE_QUEUE_BYTES.saturating_sub(self.queued_response_bytes)
            || !Self::queue_accepts_entry(&self.write_queue, out.len(), true)
        {
            log::warn!(
                "[PTY] response queue limit reached for session {} ({} queued, {} incoming)",
                self.id,
                self.queued_response_bytes,
                out.len()
            );
            return;
        }
        self.queued_response_bytes += out.len();
        Self::push_queue_owned(&mut self.write_queue, out, true);
        self.terminal.note_protocol_response();
        let _ = self.flush_write_queue();
    }

    /// Drain prior work and report whether a user payload can be prepared while
    /// staying inside both the byte and allocation-count limits.
    fn can_queue_user_bytes(&mut self, len: usize) -> bool {
        self.flush_write_queue()
            && len <= MAX_PTY_WRITE_QUEUE_BYTES.saturating_sub(self.queued_write_bytes)
            && Self::queue_accepts_entry(&self.write_queue, len, false)
    }

    /// True while this tab is a held-open task transcript whose child has
    /// already exited (`hold_after_exit` is set exactly then). User bytes
    /// have nowhere to go: every PTY-bound write path refuses them up front
    /// and the chrome shows the transcript as read-only.
    fn transcript_read_only(&self) -> bool {
        user_input_blocked(self.hold_after_exit)
    }

    /// Queue data in-order and make one non-blocking drain attempt. Returns false
    /// if the bounded queue rejected the write or the PTY has failed.
    fn write_pty(&mut self, data: &[u8]) -> bool {
        self.write_pty_with_origin(data, true)
    }

    /// Queue an already-reviewed Agent payload. The terminal was armed before
    /// this call, so these exact bytes must not taint their own prompt; any
    /// subsequent ordinary input still goes through `write_pty` and does.
    fn write_agent_pty(&mut self, data: &[u8]) -> bool {
        self.write_pty_with_origin(data, false)
    }

    fn write_pty_with_origin(&mut self, data: &[u8], taint_prompt: bool) -> bool {
        if data.is_empty() {
            return true;
        }
        // A held-open task transcript's child already exited; its bytes would
        // only hit EIO on the dead master fd. The keyboard/paste dispatchers
        // pre-check `transcript_read_only` to show the read-only hint; this
        // guard catches every remaining user-write path (mouse reports,
        // Agent payloads) silently.
        if self.transcript_read_only() {
            return false;
        }
        if !self.can_queue_user_bytes(data.len()) {
            log::warn!(
                "[PTY] input backpressure for session {} ({} input, {} response, {} incoming)",
                self.id,
                self.queued_write_bytes,
                self.queued_response_bytes,
                data.len()
            );
            return false;
        }
        self.queued_write_bytes += data.len();
        Self::push_queue_copy(&mut self.write_queue, data, false);
        // The write may complete before its echo returns through the reader.
        // Taint the current OSC 133 prompt now so Agent approval cannot race a
        // locally typed-but-not-yet-visible edit line.
        if taint_prompt {
            self.terminal.note_user_input(data);
        }
        self.flush_write_queue()
    }

    fn flush_write_queue(&mut self) -> bool {
        let mut budget = PTY_WRITE_DRAIN_BUDGET;
        while let Some(front) = self.write_queue.front() {
            if budget == 0 {
                return true;
            }
            let front_len = front.data.len();
            let is_response = front.response;
            let end = (self.write_queue_offset + budget).min(front_len);
            match self.pty.write(&front.data[self.write_queue_offset..end]) {
                Ok(0) => return true,
                Ok(written) => {
                    budget = budget.saturating_sub(written);
                    self.write_queue_offset += written;
                    if is_response {
                        self.queued_response_bytes =
                            self.queued_response_bytes.saturating_sub(written);
                    } else {
                        self.queued_write_bytes = self.queued_write_bytes.saturating_sub(written);
                    }
                    if self.write_queue_offset == front_len {
                        self.write_queue.pop_front();
                        self.write_queue_offset = 0;
                    }
                }
                Err(error) => {
                    log::warn!("[PTY] write failed for session {}: {error}", self.id);
                    self.write_queue.clear();
                    self.write_queue_offset = 0;
                    self.queued_write_bytes = 0;
                    self.queued_response_bytes = 0;
                    return false;
                }
            }
        }
        true
    }

    fn has_pending_write(&self) -> bool {
        self.queued_write_bytes != 0 || self.queued_response_bytes != 0
    }

    /// Working directory of the shell child, used when spawning a sibling and
    /// when persisting the session.
    ///
    /// OSC 7 first: `/proc/<pid>/cwd` is the *direct* child's directory, so it
    /// reports where ssh was launched from rather than where the user is, and it
    /// does not exist at all on a non-Linux kernel. The OSC 7 value has already
    /// been rejected unless it named a local host (`TerminalState::decode_osc7_cwd`).
    fn cwd(&self) -> Option<String> {
        self.terminal
            .current_working_dir()
            .map(str::to_string)
            .or_else(|| jterm_core::process::process_cwd(self.pty.get_child_pid()))
    }
}

/// Texture-cache key for one Kitty placement: stable session id, image id and
/// the clamped source-crop rectangle the placement shows.
type KittyHandleKey = (usize, u32, kitty_graphics::Crop);

/// Route every event in one terminal mouse gesture to the owner chosen at
/// press time. Mouse-reporting mode and Shift may change before release; that
/// must never synthesize a half gesture for either the app or local selection.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct TerminalMouseGesture {
    session_id: usize,
    button: MouseButton,
    report_to_app: bool,
    /// The app layer claimed this sequence after the widget had already
    /// published an ordinary press (for example an inactive pane whose link
    /// cache was refreshed only after focus moved). Subsequent drag/release
    /// messages close the slot without touching selection or the PTY.
    consumed: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum OverlayReleaseDisposition {
    Reject,
    ClearOnly,
    DispatchApp,
}

fn overlay_release_disposition(
    gesture: Option<TerminalMouseGesture>,
    source_session_id: usize,
    button: MouseButton,
) -> OverlayReleaseDisposition {
    let Some(gesture) = gesture else {
        return OverlayReleaseDisposition::Reject;
    };
    if gesture.session_id != source_session_id || gesture.button != button {
        return OverlayReleaseDisposition::Reject;
    }
    if gesture.consumed || !gesture.report_to_app {
        OverlayReleaseDisposition::ClearOnly
    } else {
        OverlayReleaseDisposition::DispatchApp
    }
}

struct Frost {
    config: Config,
    theme: Theme,
    metrics: Metrics,
    sessions: Vec<Session>,
    active: usize,
    next_id: usize,
    cols: usize,
    rows: usize,
    focused: bool,
    modifiers: keyboard::Modifiers,
    /// Tells a plain click apart from the start of a selection drag, so only
    /// the former places the shell's edit cursor.
    click_tracker: jterm_core::click_cursor::ClickTracker,
    terminal_mouse_gestures: [Option<TerminalMouseGesture>; 3],
    mono: iced::Font,
    cjk_mono: Option<iced::Font>,
    symbol_mono: Option<iced::Font>,
    math_symbol: Option<iced::Font>,
    nerd_symbol: Option<iced::Font>,
    search: search::SearchState,
    /// PTY output marks active-search results stale; a short timer coalesces
    /// bursts so each chunk does not rescan the entire scrollback.
    search_dirty: bool,
    /// Find & Replace modal (Ctrl+Alt+R): rewrites the current selection into
    /// the clipboard or the prompt — the scrollback itself is never mutated.
    search_replace: search_replace_panel::SearchReplacePanelState,
    palette: command_palette::PaletteState,
    /// Whole-session exports can retain a bounded multi-megabyte snapshot and
    /// serialized buffer. Keep only one worker in flight across the window so
    /// a repeated shortcut cannot queue unbounded copies of terminal output.
    block_export_in_flight: bool,
    agent: agent::AgentUi,
    /// Provider-neutral task lifecycle reducer for the experimental Tasks
    /// dashboard (runtime-only metadata; nothing is persisted).
    task_manager: agent_task::TaskManager,
    /// Owner of native Codex provider sessions and their bounded views.
    agent_runtime: agent_task::AgentRuntimeManager,
    /// iced-side state for the Tasks dashboard overlay.
    task_panel: agent_task_ui::TaskPanel,
    keybindings: keybindings::KeyBindings,
    config_panel_open: bool,
    help_open: bool,
    debug_open: bool,
    /// Blink clock phase, toggled by a timer; drives blinking-attribute cells.
    blink_on: bool,
    win_size: Size,
    /// Exact bytes this in-memory config was loaded/saved against. `None`
    /// fails closed because a concurrent-safe comparison is impossible.
    config_revision: Option<persistence::FileRevision>,
    config_diagnostic: Option<String>,
    /// A malformed/unreadable user config must never be overwritten by
    /// background auto-save. Explicit Reset is the recovery escape hatch.
    config_write_blocked: bool,
    keybindings_revision: Option<persistence::FileRevision>,
    keybindings_diagnostics: Vec<String>,
    session_diagnostic: Option<String>,
    /// Persistent settings-panel changes are live-applied immediately and saved
    /// on the next config tick. Ephemeral font zoom never sets this flag.
    /// 温度输入框的原始文本（含合法值之外的中间编辑态）。
    ai_temperature_draft: String,
    /// 设置面板中待存储的 API key 明文；提交后立即清空，从不落入配置。
    ai_key_draft: String,
    config_dirty: bool,
    link_detector: link::LinkDetector,
    links: Vec<link::Link>,
    /// Stable session plus the complete immutable projection identity used by
    /// the cached visible links.
    links_cache_key: Option<(usize, ProjectionKey)>,
    /// Cached GPU image handles keyed by (stable session id, Kitty image id,
    /// source-crop rectangle). The generation invalidates same-sized
    /// retransmissions; the crop is part of the key because `x=`/`y=`/`w=`/`h=`
    /// let two placements show different sub-rectangles of one image.
    kitty_handles: std::collections::HashMap<KittyHandleKey, (iced::advanced::image::Handle, u64)>,
    /// Last persisted session-snapshot JSON, to skip redundant disk writes.
    last_session_save: Option<String>,
    /// Set when session state that feeds the snapshot may have changed (PTY
    /// output can move the cwd, tab switches move the active index). The periodic
    /// save is skipped while this is false, so a fully idle app does no per-tab
    /// `readlink` or JSON serialization on every tick.
    session_dirty: bool,
    /// Fail closed after an unreadable startup snapshot could not be moved
    /// aside. The user can repair/move it and restart; this instance will not
    /// overwrite evidence it was unable to preserve.
    session_writes_blocked: bool,
    /// Diagnostics (F12): wall-clock microseconds spent ingesting the
    /// most recent PTY-output batch (parse + refresh) and its byte count, used
    /// to derive a throughput figure for profiling.
    last_ingest_us: u128,
    last_ingest_bytes: usize,
    /// Open tabs, each owning its own pane tree. Invariants: every session
    /// appears in exactly one leaf of exactly one tab, `active_tab` is in
    /// range, and `active == tabs[active_tab].focus`.
    tabs: Vec<Tab>,
    active_tab: usize,
    /// Monotonic source of `Tab::id`.
    next_tab_id: usize,
    /// Active custom-theme editor overlay, or `None` when closed.
    theme_editor: Option<ThemeEditState>,
    /// File-tree sidebar (left panel) and whether it is currently shown.
    sidebar: sidebar::Sidebar,
    sidebar_open: bool,
    /// Which content the sidebar dock shows (file tree or tab list).
    sidebar_panel: SidebarPanel,
    /// Current dock width in pixels (drag-resizable).
    dock_width: f32,
    /// Whether the sidebar-resize divider is being dragged.
    dragging_sidebar: bool,
    /// Right-click file-ops menu of the file tree (floating, pointer-anchored).
    sidebar_menu: Option<SidebarMenuState>,
    /// Modal New File / New Folder / Rename input of the file tree.
    sidebar_dialog: Option<SidebarDialogState>,
    /// Delete confirmation target; the modal shows the full path before dispatch.
    sidebar_delete_confirm: Option<std::path::PathBuf>,
    /// Location-scoped clipboard for sidebar Copy/Cut/Paste.
    sidebar_clipboard: Option<FsClipboard>,
    /// Pointer-over-dock flag gating the window-space pointer tracker.
    sidebar_hovered: bool,
    /// Last window-space pointer position over the dock (menu anchor).
    sidebar_pointer: iced::Point,
    /// Transient status line under the files header (op failures/success).
    sidebar_notice: Option<String>,
    /// Divider being dragged, identified by its owning split node's path + gap.
    dragging_divider: Option<DividerId>,
    /// Divider under the pointer (drives its hover highlight).
    hovered_divider: Option<DividerId>,
    /// Last divider press (time + divider id), for double-click detection
    /// (double-click equalizes the panes of that divider's node).
    last_divider_press: Option<(std::time::Instant, DividerId)>,
    /// Focused pane temporarily expanded to the full terminal area (tmux-style
    /// zoom). Only meaningful while split; cleared when the split collapses.
    pane_zoomed: bool,
    /// In-flight header drag that will swap two panes on release.
    pane_drag: Option<PaneDrag>,
    /// Directional pane edge currently armed for a tab-to-split drop.
    tab_split_drop: Option<TabSplitDrop>,
    /// Stable id of the tab the pointer is hovering (drives close-button reveal).
    hovered_tab: Option<usize>,
    /// Source-tab id recorded on mouse press over a tab. Cleared on mouse
    /// release (anywhere) by the global mouse-up listener; in between, it
    /// drives tab-drag visual feedback and the reorder-on-release.
    dragging_tab: Option<usize>,
    /// Tab that was active before drag-hover previews began, restored on cancel.
    tab_drag_origin: Option<usize>,
    /// Whether the pointer actually left the pressed tab during this gesture.
    /// Distinguishes a click-to-activate from a drag that returned to source.
    tab_drag_moved: bool,
    /// Stable tab id and timestamp for a pending delayed hover preview.
    tab_drag_hover_since: Option<(usize, std::time::Instant)>,
    /// Right-click context menu state: stable id of its target tab, or None.
    /// Rendered as a centered floating panel (Esc / click-outside dismiss).
    tab_menu: Option<usize>,
    /// Rename editor inside the open tab menu: target tab id plus the draft.
    /// Lives beside `tab_menu` rather than inside it because dismissing the
    /// menu must also drop a half-typed name.
    tab_rename: Option<(usize, String)>,
    /// Last pointer position seen over a tab, in the same logical space the
    /// widget layout uses. `mouse_area` right-presses carry no coordinates, so
    /// this is what the context menu anchors to.
    tab_pointer: iced::Point,
    /// Where the open tab menu was summoned. Frozen at open time: the pointer
    /// keeps moving as the user walks over to the panel.
    tab_menu_at: iced::Point,
    /// Transient bottom-right toast queue with absolute expiry timestamps.
    /// Cleared lazily on each render and on ConfigTick.
    toasts: Vec<Toast>,
    /// Offer produced by the background "is a newer jsh published?" check, and
    /// whether the user waved it away for this launch.
    jsh_prompt: Option<jterm_core::jsh_install::Prompt>,
    jsh_notice_dismissed: bool,
    /// Tab-switcher overlay (Ctrl+Shift+L): when open, a small fuzzy list of
    /// tab labels lets the user jump by typing. Field holds the typed query
    /// and current selection index.
    tab_switcher: Option<TabSwitcherState>,
    /// Remote host picker overlay: `Some(selected index)` while open.
    remote_picker: Option<usize>,
    /// History-picker overlay (Ctrl+Shift+H): fuzzy search over the persisted
    /// command-history index; Enter types the selection into the active pane.
    history_picker: Option<history_picker::HistoryPickerState>,
    /// Cross-block search picker overlay (`block:search`, Ctrl+Alt+F).
    block_search: Option<BlockSearchState>,
    /// Window-wide monotonic source for async block-search build identities.
    /// It deliberately survives picker close/reopen within the same pane.
    next_block_search_epoch: u64,
    /// Context actions for the finalized block right-clicked anywhere in its card.
    block_menu: Option<BlockMenuState>,
    /// Counted destructive confirmation for clearing one stable pane's block
    /// history. Revalidated before deletion in case PTY output changed it.
    block_clear_confirm: Option<BlockClearConfirmation>,
    /// Close-confirmation overlay for a pane or tab with a running foreground
    /// process. Holds `(target_id, process_name, what_to_close)`.
    tab_close_confirm: Option<(usize, String, PendingClose)>,
    /// Last desktop notification launch. OSC 9/777 originates inside the PTY
    /// (and may be remote over SSH), so process spawning is globally rate-limited.
    last_notification_at: Option<std::time::Instant>,
    /// Last read-only hint for typing into a held-open task transcript. The
    /// toast is throttled so key repeat cannot stack duplicates.
    read_only_hint_at: Option<std::time::Instant>,
    /// Sessions whose history needs one width-normalization pass after resize
    /// activity settles, keyed by stable session id.
    history_reflow_sessions: std::collections::HashSet<usize>,
    history_reflow_due: Option<std::time::Instant>,
    /// Held for the process lifetime to enforce single-instance behavior. When
    /// `None`, another instance already holds the lock and this one runs fresh
    /// (no session restore, no snapshot writes) to avoid clobbering its history.
    _instance_lock: Option<std::fs::File>,
    is_first_instance: bool,
}

impl Frost {
    fn new(
        config: Config,
        config_diagnostic: Option<String>,
        config_revision: Option<persistence::FileRevision>,
    ) -> (Self, Task<Message>) {
        let ai_temperature_draft = config
            .ai_temperature
            .map(|t| format!("{t}"))
            .unwrap_or_default();
        let theme = Theme::get_theme(&config.theme).unwrap_or_default();
        let metrics = Metrics::new(
            config.font_size,
            config.line_spacing,
            config.padding,
            config.block_mode,
        );
        let cols = config.cols.max(1);
        let rows = config.rows.max(1);
        let win_size = Size::new(config.initial_width, config.initial_height);
        let keybindings_load = keybindings::KeyBindings::load_with_diagnostics();
        let keybindings_revision = keybindings_load.revision.clone();

        // Single-instance lock: a second instance starts fresh and never writes
        // the session snapshot, so it cannot clobber the first instance's history.
        let instance_lock = session_persistence::try_acquire_instance_lock();
        let is_first_instance = instance_lock.is_some();
        if !is_first_instance {
            eprintln!("[SessionPersistence] Another instance is running, starting fresh");
        }

        let mono = resolve_mono_font(&config.font_family);
        let cjk_mono = resolve_optional_font(Config::cjk_monospace_font_family());
        let symbol_mono = resolve_optional_font(Config::symbol_monospace_font_family());
        let math_symbol = resolve_optional_font(Config::math_symbol_font_family());
        let nerd_symbol = resolve_optional_font(Config::nerd_symbol_font_family());

        // Restore prior tabs (their cwds + active index) when enabled and we are
        // the first instance; otherwise start with a single default session.
        let RestoredState {
            sessions,
            active,
            next_id,
            tabs: saved_tabs,
            active_tab: saved_active_tab,
            legacy_tree: saved_tree,
            legacy_split: saved_split,
            diagnostic: session_diagnostic,
            session_writes_blocked,
        } = Self::restore_or_spawn(&config, cols, rows, is_first_instance);

        // In Side mode the dock hosts the tab list and starts open (there is no
        // top bar to show tabs otherwise); in Top mode it starts collapsed.
        let side_tabs = config.tab_position == config::TabPosition::Side;
        let sidebar_panel = if side_tabs {
            SidebarPanel::Tabs
        } else {
            SidebarPanel::Files
        };
        let sidebar_open = side_tabs;

        let mut app = Frost {
            config,
            theme,
            metrics,
            sessions,
            active,
            next_id,
            cols,
            rows,
            focused: true,
            modifiers: keyboard::Modifiers::default(),
            click_tracker: jterm_core::click_cursor::ClickTracker::default(),
            terminal_mouse_gestures: [None; 3],
            mono,
            cjk_mono,
            symbol_mono,
            math_symbol,
            nerd_symbol,
            search: search::SearchState::new(),
            search_dirty: false,
            search_replace: search_replace_panel::SearchReplacePanelState::new(),
            palette: command_palette::PaletteState::new(),
            block_export_in_flight: false,
            agent: agent::AgentUi::new(),
            task_manager: agent_task::TaskManager::new(),
            agent_runtime: agent_task::AgentRuntimeManager::new(),
            task_panel: agent_task_ui::TaskPanel::new(),
            keybindings: keybindings_load.bindings,
            config_panel_open: false,
            help_open: false,
            debug_open: false,
            blink_on: true,
            win_size,
            config_revision,
            config_write_blocked: config_diagnostic.is_some(),
            config_diagnostic,
            keybindings_revision,
            keybindings_diagnostics: keybindings_load.diagnostics,
            session_diagnostic,
            ai_temperature_draft,
            ai_key_draft: String::new(),
            config_dirty: false,
            link_detector: link::LinkDetector::new(link::LinkDetectionConfig::default()),
            links: Vec::new(),
            links_cache_key: None,
            kitty_handles: std::collections::HashMap::new(),
            last_session_save: None,
            session_dirty: true,
            session_writes_blocked,
            last_ingest_us: 0,
            last_ingest_bytes: 0,
            // Placeholder; the real tabs are installed below, after the
            // snapshot has been validated against the sessions that spawned.
            tabs: vec![Tab::new(0, active)],
            active_tab: 0,
            next_tab_id: 1,
            theme_editor: None,
            sidebar: sidebar::Sidebar::new(),
            sidebar_open,
            sidebar_panel,
            dock_width: SIDEBAR_W,
            dragging_sidebar: false,
            sidebar_menu: None,
            sidebar_dialog: None,
            sidebar_delete_confirm: None,
            sidebar_clipboard: None,
            sidebar_hovered: false,
            sidebar_pointer: iced::Point::ORIGIN,
            sidebar_notice: None,
            dragging_divider: None,
            hovered_divider: None,
            last_divider_press: None,
            pane_zoomed: false,
            pane_drag: None,
            tab_split_drop: None,
            hovered_tab: None,
            dragging_tab: None,
            tab_drag_origin: None,
            tab_drag_moved: false,
            tab_drag_hover_since: None,
            tab_menu: None,
            tab_rename: None,
            tab_pointer: iced::Point::ORIGIN,
            tab_menu_at: iced::Point::ORIGIN,
            toasts: Vec::new(),
            jsh_prompt: None,
            jsh_notice_dismissed: false,
            tab_switcher: None,
            remote_picker: None,
            history_picker: None,
            block_search: None,
            next_block_search_epoch: 0,
            block_menu: None,
            block_clear_confirm: None,
            tab_close_confirm: None,
            last_notification_at: None,
            read_only_hint_at: None,
            history_reflow_sessions: std::collections::HashSet::new(),
            history_reflow_due: None,
            _instance_lock: instance_lock,
            is_first_instance,
        };
        // Re-apply the saved tabs once the sessions exist. The snapshot is
        // external input, so every index is validated before use.
        app.restore_tabs(saved_tabs, saved_active_tab, saved_tree, saved_split);
        app.relayout();
        // The file tree carries a snapshot of the configured remote hosts.
        app.sidebar.set_hosts(app.config.remote_hosts.clone());
        // frost prefers jsh as its shell, so it is worth noticing when the
        // machine has none or an old one. Nothing is installed without an
        // explicit click.
        let jsh_check = Self::jsh_update_check_task(&app.config.jsh_update_check);
        (app, jsh_check)
    }

    fn title(&self) -> String {
        let title = self.tab_label(self.active_tab);
        if title.is_empty() {
            "frost".to_string()
        } else {
            title
        }
    }

    fn iced_theme(&self) -> iced::Theme {
        iced::Theme::custom(
            "frost".to_string(),
            iced::theme::Palette {
                background: self.theme.terminal_background(),
                text: self.theme.terminal_foreground(),
                primary: self.theme.cursor_color(),
                success: self.theme.ansi_color(2),
                warning: self.theme.ansi_color(3),
                danger: self.theme.ansi_color(1),
            },
        )
    }

    /// Application-level surface style. With `transparent: true` on the window
    /// this is the layer that makes the configured background opacity real:
    /// the whole surface is cleared with the theme background at that alpha,
    /// and the terminal widget skips its own default fill below 1.0 so the two
    /// never stack (see `TermWidget::draw`).
    fn app_style(&self, theme: &iced::Theme) -> iced::theme::Style {
        let palette = theme.palette();
        iced::theme::Style {
            background_color: self.with_window_opacity(palette.background),
            text_color: palette.text,
        }
    }

    /// Scale a chrome color's alpha by the configured window opacity so tab
    /// bar, status bar, and dock go translucent together with the terminal.
    fn with_window_opacity(&self, color: Color) -> Color {
        Color {
            a: color.a * self.config.opacity,
            ..color
        }
    }

    fn scale_factor(&self) -> f32 {
        self.config.ui_scale.unwrap_or(1.0)
    }

    fn effective_font_size(&self) -> f32 {
        Config::clamp_font_size(self.config.font_size)
    }

    /// Single re-apply path for live config changes (Set*, Reset, hot reload):
    /// re-resolve the theme, rebuild metrics, and regrid every session.
    fn apply_config(&mut self) {
        if !self.config.block_mode {
            self.block_search = None;
            self.block_menu = None;
            self.block_clear_confirm = None;
            for sess in &mut self.sessions {
                sess.block_selection.clear();
            }
        }
        self.theme = Theme::get_theme(&self.config.theme).unwrap_or_default();
        self.mono = resolve_mono_font(&self.config.font_family);
        self.cjk_mono = resolve_optional_font(Config::cjk_monospace_font_family());
        self.symbol_mono = resolve_optional_font(Config::symbol_monospace_font_family());
        self.math_symbol = resolve_optional_font(Config::math_symbol_font_family());
        self.nerd_symbol = resolve_optional_font(Config::nerd_symbol_font_family());
        self.metrics = Metrics::new(
            self.effective_font_size(),
            self.config.line_spacing,
            self.config.padding,
            self.config.block_mode,
        );
        let term_h = self.term_height();
        let term_w = (self.term_width() - terminal_view::SCROLLBAR_WIDTH).max(0.0);
        let (cols, rows) = self.metrics.grid_size(term_w, term_h);
        let resized = cols != self.cols || rows != self.rows;
        if resized {
            self.cols = cols;
            self.rows = rows;
        }
        for sess in &mut self.sessions {
            let projection_mode_changed = sess.projection_block_mode != self.config.block_mode;
            sess.projection_block_mode = self.config.block_mode;
            sess.terminal
                .set_max_scrollback(self.config.scrollback_lines);
            sess.terminal
                .set_disable_alt_screen(self.config.disable_alt_screen);
            if projection_mode_changed {
                sess.refresh();
            }
        }
        self.relayout();
        if resized {
            self.refresh_active_context();
        }
        // Keep the file tree's remote-host snapshot aligned with the config;
        // requests and file ops travel with this copy, never a live borrow.
        if self.sidebar.hosts_snapshot() != self.config.remote_hosts.as_slice() {
            self.sidebar.set_hosts(self.config.remote_hosts.clone());
        }
    }

    fn sync_tab_position_ui(&mut self) {
        match self.config.tab_position {
            config::TabPosition::Side => {
                self.sidebar_open = true;
                self.sidebar_panel = SidebarPanel::Tabs;
            }
            config::TabPosition::Top => {
                self.sidebar_open = false;
                self.sidebar_panel = SidebarPanel::Files;
            }
        }
    }

    /// Step the terminal font size (hotkey / Ctrl+wheel path). The new value
    /// is clamped to the config range and persisted via the live-config path.
    fn adjust_font_size(&mut self, delta: f32) {
        let current = self.effective_font_size();
        let next = Config::clamp_font_size(current + delta);
        if (next - current).abs() < f32::EPSILON {
            return;
        }
        self.config.font_size = next;
        self.config_dirty = true;
        self.apply_config();
    }

    /// Restore the default font size (Ctrl+0) and persist it.
    fn reset_font_size(&mut self) {
        let default = Config::default().font_size;
        if (self.config.font_size - default).abs() < f32::EPSILON {
            return;
        }
        self.config.font_size = default;
        self.config_dirty = true;
        self.apply_config();
    }

    /// Step the window background opacity (hotkey path). The new value is
    /// clamped to the config range, persisted via the live-config path, and
    /// echoed in a toast. Repeat presses update the existing opacity toast
    /// instead of stacking a new one per step.
    fn adjust_opacity(&mut self, delta: f32) {
        let next = Config::clamp_opacity(self.config.opacity + delta);
        if (next - self.config.opacity).abs() > f32::EPSILON {
            self.config.opacity = next;
            self.config_dirty = true;
        }
        self.toasts.retain(|t| !t.text.starts_with("Opacity: "));
        self.push_toast(
            format!("Opacity: {:.0}%", self.config.opacity * 100.0),
            ToastKind::Info,
        );
    }

    fn save_config_checked(&mut self) -> Result<(), persistence::AtomicWriteError> {
        let candidate = self.config.clone().normalized();
        match candidate.save_if_unchanged(self.config_revision.as_ref()) {
            Ok(revision) => {
                self.config = candidate;
                self.config_revision = Some(revision);
                self.config_dirty = false;
                Ok(())
            }
            Err(error) => {
                // A rename can become visible before syncing its parent
                // directory fails. Adopt that exact disk revision, but keep
                // the state dirty so the next tick retries durability instead
                // of misclassifying our own write as an external conflict.
                if let Some(revision) = error.committed_revision().cloned() {
                    self.config = candidate;
                    self.config_revision = Some(revision);
                    self.config_dirty = true;
                }
                Err(error)
            }
        }
    }

    fn block_config_writes(&mut self, diagnostic: String) {
        let changed = self.config_diagnostic.as_deref() != Some(diagnostic.as_str());
        self.config_write_blocked = true;
        self.config_diagnostic = Some(diagnostic);
        if changed {
            self.push_toast(
                "Config changed or became unreadable; keeping last-known-good values",
                ToastKind::Warning,
            );
        }
    }

    fn note_config_save_error(&mut self, error: &persistence::AtomicWriteError) {
        eprintln!("[Config] Save failed: {error}");
        if error.blocks_automatic_writes() {
            self.block_config_writes(error.to_string());
        }
    }

    fn persist_live_config(&mut self) {
        if !self.config_dirty || self.config_write_blocked {
            return;
        }
        if let Err(error) = self.save_config_checked() {
            self.note_config_save_error(&error);
        }
    }

    /// Observe external edits before attempting auto-save. Exact content
    /// revisions detect changes even on filesystems with coarse mtimes. Dirty
    /// local state is never merged or overwritten silently: it stays live and
    /// writes are blocked until the user explicitly resets/resolves it.
    fn reload_config_if_changed(&mut self) {
        let disk_revision = match Config::config_revision() {
            Ok(revision) => revision,
            Err(error) => {
                self.block_config_writes(error);
                return;
            }
        };
        let changed = self.config_revision.as_ref() != Some(&disk_revision);
        if !changed && !self.config_write_blocked {
            return;
        }
        if self.config_dirty {
            if changed {
                self.config_revision = Some(disk_revision);
                self.block_config_writes(
                    "Config changed outside this window while local edits were pending; Reset explicitly before overwriting it"
                        .to_string(),
                );
            }
            return;
        }
        // Preserve the panel's stable editing surface. With no local dirty
        // state there is nothing to save, so deferring the reload is safe.
        if self.config_panel_open {
            return;
        }

        let path = match Config::config_path() {
            Ok(path) => path,
            Err(error) => {
                self.block_config_writes(error.to_string());
                return;
            }
        };
        self.config_revision = Some(disk_revision.clone());
        match Config::from_revision(&path, &disk_revision) {
            Ok(config) => {
                let recovered = self.config_diagnostic.take().is_some();
                let old_scale = self.scale_factor();
                self.config = config;
                self.win_size =
                    logical_viewport_after_scale(self.win_size, old_scale, self.scale_factor());
                self.ai_temperature_draft = self
                    .config
                    .ai_temperature
                    .map(|temperature| temperature.to_string())
                    .unwrap_or_default();
                self.config_dirty = false;
                self.config_write_blocked = false;
                self.sync_tab_position_ui();
                self.apply_config();
                if recovered {
                    self.push_toast("Config fixed and reloaded", ToastKind::Success);
                }
            }
            Err(error) => self.block_config_writes(error),
        }
    }

    /// Whether the left dock is shown. Follows the manual `sidebar_open` toggle
    /// in both tab-position modes, so the dock can always be collapsed.
    fn dock_open(&self) -> bool {
        self.sidebar_open
    }

    /// Whether the terminal itself owns text/IME input. Every overlay with an
    /// editable field or modal action takes ownership until it closes.
    fn terminal_input_active(&self) -> bool {
        !self.search.is_open
            && !self.search_replace.is_open
            && !self.palette.is_open
            && !self.config_panel_open
            && !self.help_open
            && !self.debug_open
            && self.tab_menu.is_none()
            && self.tab_switcher.is_none()
            && self.history_picker.is_none()
            && self.block_search.is_none()
            && self.block_menu.is_none()
            && self.block_clear_confirm.is_none()
            && self.remote_picker.is_none()
            && self.tab_close_confirm.is_none()
            && self.sidebar_menu.is_none()
            && self.sidebar_dialog.is_none()
            && self.sidebar_delete_confirm.is_none()
    }

    /// Search is intentionally non-modal for scrolling/selection. The remaining
    /// overlays block pointer actions from reaching panes underneath them.
    fn terminal_mouse_active(&self) -> bool {
        !self.search_replace.is_open
            && !self.palette.is_open
            && !self.config_panel_open
            && !self.help_open
            && !self.debug_open
            && self.tab_menu.is_none()
            && self.tab_switcher.is_none()
            && self.history_picker.is_none()
            && self.block_search.is_none()
            && self.block_menu.is_none()
            && self.block_clear_confirm.is_none()
            && self.remote_picker.is_none()
            && self.tab_close_confirm.is_none()
            && self.sidebar_menu.is_none()
            && self.sidebar_dialog.is_none()
            && self.sidebar_delete_confirm.is_none()
    }

    /// Toggle the left dock and refresh its file root when it becomes visible.
    /// Keeping this in one place makes the toolbar, shortcut, and command
    /// palette behave identically.
    fn toggle_sidebar(&mut self) -> Task<Message> {
        self.sidebar_open = !self.sidebar_open;
        // The cwd follow is local-only, exactly as in SetSidebarPanel.
        let follow_local = self.sidebar.location == remote_fs::FsLocation::Local;
        let request = if self.sidebar_open && self.sidebar_panel == SidebarPanel::Files {
            if follow_local {
                if let Some(cwd) = self
                    .sessions
                    .get(self.active)
                    .and_then(|s| s.cwd_cache.clone().or_else(|| s.cwd()))
                {
                    Some(self.sidebar.set_current_dir(std::path::PathBuf::from(cwd)))
                } else {
                    Some(self.sidebar.refresh())
                }
            } else {
                Some(self.sidebar.refresh())
            }
        } else {
            None
        };
        self.apply_config();
        request.map_or_else(Task::none, sidebar_load_task)
    }

    /// Dispatch one file-tree context-menu action against its frozen target.
    /// Directory-scope actions (New File/New Folder/Paste) act inside the
    /// clicked directory, or inside the clicked file's parent.
    fn execute_sidebar_menu_action(
        &mut self,
        menu: SidebarMenuState,
        action: SidebarMenuAction,
    ) -> Task<Message> {
        let target_dir = if menu.is_dir {
            menu.path.clone()
        } else {
            menu.path
                .parent()
                .map(std::path::Path::to_path_buf)
                .unwrap_or_else(|| menu.path.clone())
        };
        match action {
            SidebarMenuAction::NewFile | SidebarMenuAction::NewFolder => {
                self.sidebar_dialog = Some(SidebarDialogState {
                    kind: if action == SidebarMenuAction::NewFile {
                        SidebarDialogKind::NewFile
                    } else {
                        SidebarDialogKind::NewFolder
                    },
                    path: target_dir,
                    input: String::new(),
                    error: None,
                });
                iced::widget::operation::focus(SIDEBAR_DIALOG_INPUT_ID.clone())
            }
            SidebarMenuAction::Rename => {
                // Seed the editor with the clicked node's current name, so a
                // rename starts from what the user just right-clicked.
                let input = menu
                    .path
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_default();
                self.sidebar_dialog = Some(SidebarDialogState {
                    kind: SidebarDialogKind::Rename,
                    path: menu.path,
                    input,
                    error: None,
                });
                iced::widget::operation::focus(SIDEBAR_DIALOG_INPUT_ID.clone())
            }
            SidebarMenuAction::Delete => {
                self.sidebar_delete_confirm = Some(menu.path);
                Task::none()
            }
            SidebarMenuAction::Copy | SidebarMenuAction::Cut => {
                self.sidebar_clipboard = Some(FsClipboard {
                    loc: self.sidebar.location.clone(),
                    path: menu.path,
                    is_dir: menu.is_dir,
                    cut: action == SidebarMenuAction::Cut,
                });
                Task::none()
            }
            SidebarMenuAction::Paste => {
                let Some(clipboard) = self.sidebar_clipboard.clone() else {
                    return Task::none();
                };
                let op = match sidebar_paste_op(&clipboard, &self.sidebar.location, &target_dir) {
                    Ok(op) => op,
                    Err(problem) => {
                        self.sidebar_notice = Some(problem);
                        return Task::none();
                    }
                };
                // Transfers can run long; say so in the panel while they do.
                if clipboard.loc != self.sidebar.location {
                    let verb = transfer_verb(&clipboard.loc, &self.sidebar.location, clipboard.cut);
                    let name = clipboard
                        .path
                        .file_name()
                        .map(|name| name.to_string_lossy().into_owned())
                        .unwrap_or_default();
                    self.sidebar_notice = Some(format!("{verb} {name}…"));
                }
                sidebar_op_task(
                    self.sidebar.location.clone(),
                    self.sidebar.hosts_snapshot().to_vec(),
                    op,
                )
            }
            SidebarMenuAction::Refresh => sidebar_load_task(self.sidebar.refresh()),
        }
    }

    /// Validate the modal name input and turn it into a create/rename op.
    /// A failed validation stays in the dialog as its inline error.
    fn submit_sidebar_dialog(&mut self) -> Task<Message> {
        let Some(dialog) = self.sidebar_dialog.clone() else {
            return Task::none();
        };
        if let Err(problem) = remote_fs::validate_new_name(&dialog.input) {
            if let Some(current) = self.sidebar_dialog.as_mut() {
                current.error = Some(problem);
            }
            return Task::none();
        }
        self.sidebar_dialog = None;
        let op = match dialog.kind {
            SidebarDialogKind::NewFile => SidebarOp::CreateFile(dialog.path.join(&dialog.input)),
            SidebarDialogKind::NewFolder => SidebarOp::CreateDir(dialog.path.join(&dialog.input)),
            SidebarDialogKind::Rename => {
                let Some(parent) = dialog.path.parent() else {
                    return Task::none();
                };
                let dst = parent.join(&dialog.input);
                if dst == dialog.path {
                    // Unchanged name: nothing to do, and the probe would
                    // (correctly) refuse it as already-existing.
                    return Task::none();
                }
                SidebarOp::Rename {
                    src: dialog.path,
                    dst,
                }
            }
        };
        sidebar_op_task(
            self.sidebar.location.clone(),
            self.sidebar.hosts_snapshot().to_vec(),
            op,
        )
    }

    /// Run the confirmed deletion. The one absolute rule (never `/`) is
    /// re-checked here, at dispatch time, not only in the menu layer.
    fn confirm_sidebar_delete(&mut self) -> Task<Message> {
        let Some(path) = self.sidebar_delete_confirm.take() else {
            return Task::none();
        };
        if let Err(problem) = remote_fs::validate_delete_path(&path) {
            self.sidebar_notice = Some(problem);
            return Task::none();
        }
        sidebar_op_task(
            self.sidebar.location.clone(),
            self.sidebar.hosts_snapshot().to_vec(),
            SidebarOp::Delete(path),
        )
    }

    /// Terminal area height: window minus the tab bar and (when enabled) the
    /// status bar. The top bar is always reserved (even in side-tab mode, where
    /// it hosts the dock toggle) so floating chrome never overlaps terminal
    /// content.
    fn term_height(&self) -> f32 {
        (self.win_size.height - chrome_height(self.config.bottom_bar)).max(0.0)
    }

    /// Terminal area width: window minus the sidebar (when shown).
    fn term_width(&self) -> f32 {
        (self.win_size.width - self.sidebar_width()).max(0.0)
    }

    /// Current sidebar width (0 when hidden), including the resize divider.
    fn sidebar_width(&self) -> f32 {
        if self.dock_open() {
            self.dock_width + DIVIDER
        } else {
            0.0
        }
    }

    fn session_by_identity(&mut self, id: usize, fd: RawFd) -> Option<&mut Session> {
        self.sessions
            .iter_mut()
            .find(|session| session.id == id && session.master_fd == fd)
    }

    /// Refresh every cache/state object whose coordinates belong to the active
    /// session. All tab/pane activation paths call this before accepting input.
    fn refresh_active_context(&mut self) {
        self.links_cache_key = None;
        if self.search.is_open {
            let reflow_pending = self
                .sessions
                .get(self.active)
                .is_some_and(|session| self.history_reflow_sessions.contains(&session.id));
            if reflow_pending {
                self.search_dirty = true;
            } else {
                self.recompute_search();
                self.reveal_current_search_match();
            }
        }
        self.recompute_links();
        self.refresh_kitty_handles();
    }

    /// Startup session setup: when `restore_session` is enabled and a snapshot
    /// exists, respawn one session per saved tab at its recorded cwd; otherwise
    /// (or on any failure) fall back to a single default session. The fourth
    /// element is the saved split layout (validated and applied by the caller).
    fn restore_or_spawn(
        config: &Config,
        cols: usize,
        rows: usize,
        is_first_instance: bool,
    ) -> RestoredState {
        let default = |id_start: usize| match Session::spawn(config, id_start, cols, rows, None) {
            Ok(session) => RestoredState {
                sessions: vec![session],
                next_id: id_start + 1,
                ..RestoredState::default()
            },
            Err(error) => RestoredState {
                next_id: id_start,
                diagnostic: Some(error.to_string()),
                ..RestoredState::default()
            },
        };
        if !config.restore_session || !is_first_instance {
            return default(0);
        }
        let Ok(path) = config.session_history_path() else {
            return default(0);
        };
        let snapshot = match session_persistence::SessionsSnapshot::load(&path) {
            session_persistence::SnapshotLoad::Loaded(s) if !s.sessions.is_empty() => s,
            session_persistence::SnapshotLoad::Loaded(_)
            | session_persistence::SnapshotLoad::Missing => return default(0),
            // Quarantine before returning, because returning is what lets the
            // app start — and `save_session_snapshot` then writes a fresh
            // snapshot over this same path on the first periodic tick. Retaining
            // the unreadable file and overwriting it seconds later destroys the
            // only copy of the user's tabs.
            session_persistence::SnapshotLoad::Unreadable(reason) => {
                let mut state = default(0);
                let note = match jterm_core::snapshot_file::quarantine_corrupt(&path) {
                    Ok(backup) => {
                        log::warn!(
                            "[SessionPersistence] Cannot read {} ({reason}); moved it to {}",
                            path.display(),
                            backup.display()
                        );
                        format!(
                            "Could not read the saved session ({reason}).\nThe old file was kept as {}",
                            backup.display()
                        )
                    }
                    Err(move_error) => {
                        state.session_writes_blocked = true;
                        log::warn!(
                            "[SessionPersistence] Cannot read {} ({reason}) and cannot move it aside ({move_error})",
                            path.display()
                        );
                        format!(
                            "Could not read the saved session ({reason}), and it could not be moved aside ({move_error}). Automatic session saving is disabled for this run so the original is not overwritten; repair or move it, then restart"
                        )
                    }
                };
                state.diagnostic = Some(match state.diagnostic.take() {
                    Some(existing) => format!("{note}\n{existing}"),
                    None => note,
                });
                return state;
            }
        };
        let mut sessions = Vec::new();
        let mut next_id = 0usize;
        let mut restore_warnings = Vec::new();
        if snapshot.sessions.len() > session_persistence::MAX_RESTORED_SESSIONS {
            log::warn!(
                "[SessionPersistence] Snapshot has {} sessions; restoring only the first {}",
                snapshot.sessions.len(),
                session_persistence::MAX_RESTORED_SESSIONS
            );
        }
        for snap in snapshot
            .sessions
            .iter()
            .take(session_persistence::MAX_RESTORED_SESSIONS)
        {
            match Session::spawn(config, next_id, cols, rows, snap.cwd.as_deref()) {
                Ok(session) => {
                    sessions.push(session);
                    next_id += 1;
                }
                Err(error) if snap.cwd.is_some() => {
                    let cwd = snap.cwd.as_deref().unwrap_or_default();
                    log::warn!(
                        "[SessionPersistence] Cannot restore cwd {cwd:?}: {error}; using default cwd"
                    );
                    match Session::spawn(config, next_id, cols, rows, None) {
                        Ok(session) => {
                            restore_warnings.push(format!(
                                "Restored missing cwd {cwd:?} in the default folder"
                            ));
                            sessions.push(session);
                            next_id += 1;
                        }
                        Err(fallback_error) => restore_warnings
                            .push(format!("Cannot restore terminal session: {fallback_error}")),
                    }
                }
                Err(error) => {
                    restore_warnings.push(format!("Cannot restore terminal session: {error}"));
                }
            }
        }
        if sessions.is_empty() {
            return default(0);
        }
        let active = snapshot.active_index.unwrap_or(0).min(sessions.len() - 1);
        eprintln!(
            "[SessionPersistence] Restored {} session(s) from {}",
            sessions.len(),
            path.display()
        );
        RestoredState {
            sessions,
            active,
            next_id,
            tabs: snapshot.tabs,
            active_tab: snapshot.active_tab,
            legacy_tree: snapshot.tree,
            legacy_split: snapshot.split,
            diagnostic: (!restore_warnings.is_empty()).then(|| restore_warnings.join("\n")),
            session_writes_blocked: false,
        }
    }

    /// Persist the current tabs (live cwd of each + active index) when enabled.
    /// De-duplicated against the last write to avoid redundant disk churn.
    fn save_session_snapshot(&mut self) {
        if self.sessions.is_empty() || !self.config.restore_session || !self.is_first_instance {
            self.session_dirty = false;
            return;
        }
        if self.session_writes_blocked {
            return;
        }
        let snaps: Vec<session_persistence::SessionSnapshot> = self
            .sessions
            .iter()
            .map(|s| session_persistence::SessionSnapshot { cwd: s.cwd() })
            .collect();
        // Task-bound terminals (Agent CLI fallback, validation runs) and any
        // held-open exited transcript stay out of the snapshot: their task
        // metadata is runtime-only, so restoring one would produce a plain
        // shell that happens to sit in a task worktree. `prune_sessions`
        // rewrites every pane-tree leaf, focus, and active index against the
        // post-filter session indices.
        let keep: Vec<bool> = self
            .sessions
            .iter()
            .map(|s| {
                !s.hold_after_exit
                    && self
                        .task_manager
                        .task_for_terminal_session(&agent_task_ui::terminal_session_id(s.id))
                        .is_none()
            })
            .collect();
        let pruned = session_persistence::prune_sessions(
            snaps,
            // Persist every tab's pane tree so a restart restores the same
            // tabs with the same panes in each.
            self.tabs
                .iter()
                .map(|tab| session_persistence::TabSnapshot {
                    tree: pane_tree_to_snapshot(&tab.tree),
                    focus: Some(tab.focus),
                    title: tab.title.clone(),
                    pinned: tab.pinned,
                    marked: tab.marked,
                    private_title: tab.private_title,
                })
                .collect(),
            Some(self.active),
            Some(self.active_tab),
            &keep,
        );
        let snapshot = session_persistence::SessionsSnapshot::new(
            pruned.sessions,
            pruned.active_index,
            pruned.tabs,
            pruned.active_tab,
        );
        let Some(json) = snapshot.to_json() else {
            log::warn!("[SessionPersistence] Cannot serialize the current session snapshot");
            return;
        };
        if self.last_session_save.as_deref() == Some(json.as_str()) {
            self.session_dirty = false;
            return;
        }
        let path = match self.config.session_history_path() {
            Ok(path) => path,
            Err(error) => {
                log::warn!("[SessionPersistence] Cannot resolve snapshot path: {error}");
                return;
            }
        };
        match snapshot.save(&path) {
            Ok(()) => {
                self.last_session_save = Some(json);
                self.session_dirty = false;
            }
            Err(error) => {
                // Keep dirty so the periodic tick retries transient I/O
                // failures instead of silently abandoning this generation.
                log::warn!(
                    "[SessionPersistence] Cannot save {}: {error}",
                    path.display()
                );
            }
        }
    }

    fn new_session(&mut self) {
        let cwd = self.sessions.get(self.active).and_then(|s| s.cwd());
        match Session::spawn(
            &self.config,
            self.next_id,
            self.cols,
            self.rows,
            cwd.as_deref(),
        ) {
            Ok(session) => {
                self.session_diagnostic = None;
                self.next_id += 1;
                let insert = (self.active + 1).min(self.sessions.len());
                self.sessions.insert(insert, session);
                self.reindex_tabs_after_insert(insert);
                // A new session opens its own tab; it is never grafted into
                // the current tab's split.
                self.open_tab_with(insert);
                self.relayout();
                self.refresh_active_context();
                self.save_session_snapshot();
            }
            Err(error) => {
                let message = error.to_string();
                log::error!("[PTY] {message}");
                self.session_diagnostic = Some(message.clone());
                self.push_toast(message, ToastKind::Warning);
            }
        }
    }

    /// Run the jsh installer in its own session. The script narrates what it
    /// does, so the session is the progress UI — the user can read a failure or
    /// interrupt it with Ctrl+C like any other command.
    fn install_or_update_jsh(&mut self) {
        self.jsh_notice_dismissed = true;
        let argv = match jterm_core::jsh_install::install_argv() {
            Ok(argv) => argv,
            Err(error) => {
                log::warn!("cannot stage the jsh installer: {error}");
                self.push_toast(
                    format!("Could not write the installer script: {error}"),
                    ToastKind::Warning,
                );
                return;
            }
        };
        match Session::spawn_argv(
            &self.config,
            self.next_id,
            self.cols,
            self.rows,
            None,
            Some(&argv),
        ) {
            Ok(session) => {
                self.session_diagnostic = None;
                self.next_id += 1;
                let insert = (self.active + 1).min(self.sessions.len());
                self.sessions.insert(insert, session);
                self.reindex_tabs_after_insert(insert);
                // A new session opens its own tab; it is never grafted into
                // the current tab's split.
                self.open_tab_with(insert);
                self.relayout();
                self.refresh_active_context();
                self.save_session_snapshot();
            }
            Err(error) => {
                let message = error.to_string();
                log::error!("[PTY] {message}");
                self.push_toast(message, ToastKind::Warning);
            }
        }
    }

    /// Ask the installer what is published, off the UI thread. A check that
    /// fails, or finds nothing to do, stays silent: an offline laptop must not
    /// be nagged about a button that cannot work.
    fn jsh_update_check_task(policy: &str) -> Task<Message> {
        // "startup" asks the network every launch; "daily" reuses the
        // installer's cache, which every jterm on this machine shares.
        let Some(max_age) = jterm_core::jsh_install::UpdateCheck::parse(policy).max_age() else {
            return Task::none();
        };
        Task::perform(
            async move {
                tokio::task::spawn_blocking(move || {
                    jterm_core::jsh_install::check_blocking(max_age)
                })
                .await
                .unwrap_or_else(|error| {
                    log::warn!("jsh update check did not finish: {error}");
                    jterm_core::jsh_install::Status::default()
                })
            },
            |status| Message::JshChecked(Box::new(status)),
        )
    }

    /// Close every session in `tab`. This is what the tab bar's × does: panes
    /// belong to their tab, so none of them outlive it as a hidden PTY.
    ///
    /// Sessions are closed highest-index-first so the indices still queued are
    /// never shifted by an earlier removal.
    fn close_tab(&mut self, tab: usize) -> Task<Message> {
        let Some(state) = self.tabs.get(tab) else {
            return Task::none();
        };
        let mut owned = state.sessions();
        owned.sort_unstable_by(|a, b| b.cmp(a));
        let mut tasks = Vec::new();
        for index in owned {
            tasks.push(self.close_session(index));
        }
        Task::batch(tasks)
    }

    /// Ask to close every session in `tab`, confirming first if any of them is
    /// running a foreground process.
    fn request_close_tab(&mut self, tab: usize) -> Task<Message> {
        let owned = self
            .tabs
            .get(tab)
            .map(|state| state.sessions())
            .unwrap_or_default();
        if let Some((index, process)) = owned
            .iter()
            .find_map(|&index| self.busy_session_name(index).map(|name| (index, name)))
        {
            // Show the user which pane is holding the tab open before it is
            // torn down together with its siblings.
            self.focus_session(index);
            self.refresh_active_context();
            if let Some(session) = self.sessions.get(index) {
                self.tab_close_confirm = Some((session.id, process, PendingClose::Tab));
            }
            return Task::none();
        }
        self.close_tab(tab)
    }

    /// Carry out a close the user just confirmed.
    fn execute_pending_close(&mut self, index: usize, pending: PendingClose) -> Task<Message> {
        match pending {
            PendingClose::Session { activate_after } => {
                self.close_session_then(index, activate_after)
            }
            PendingClose::Tab => match self.tab_of_session(index) {
                Some(tab) => self.close_tab(tab),
                None => self.close_session(index),
            },
        }
    }

    fn close_session(&mut self, index: usize) -> Task<Message> {
        if index >= self.sessions.len() {
            return Task::none();
        }
        // Session removal can invalidate a tab/pane drag source or target and
        // reindex every remaining leaf. Cancel while all stable identities are
        // still resolvable, then perform the close from a clean UI state.
        self.cancel_layout_drags();
        // ANY session close (user close, close_tab, an async `PtyExited`)
        // invalidates the block search picker: session indices shift and the
        // close may hand `active` to a different session whose zone ids —
        // they restart at 0 per session — would silently resolve a held
        // hit against the wrong session. Close it; see
        // `close_block_search_on_session_change`.
        self.close_block_search_on_session_change();
        // Closing the last session quits the app.
        if self.sessions.len() == 1 {
            self.save_session_snapshot();
            let _ = self.sessions[0].pty.terminate();
            return iced::exit();
        }
        let mut sess = self.sessions.remove(index);
        let closed_id = sess.id;
        // A user-initiated close of a task-bound terminal cancels the binding
        // (no child exit status was observed). Sessions already reported
        // through `handle_terminal_session_exit` are in a terminal state, so
        // this is a no-op for them.
        self.task_manager
            .handle_terminal_session_closed(&agent_task_ui::terminal_session_id(closed_id));
        self.history_reflow_sessions.remove(&closed_id);
        // The strip's transient state is keyed by tab id; a closed tab is
        // dropped from it in `prune_closed_pane` once we know whether the tab
        // itself went away.

        let _ = sess.pty.terminate();
        // `prune_closed_pane` is authoritative for `active` (it must pick a
        // neighbor leaf when the focused pane's session is the one closing).
        let old_active = self.active;
        self.prune_closed_pane(index, old_active);
        self.refresh_active_context();
        self.save_session_snapshot();
        Task::none()
    }

    /// Reconcile every tab after `sessions[index]` was removed (in old index
    /// space): the owning tab drops its leaf — folding its share into a
    /// neighbor and collapsing any split left with one child — or, if that was
    /// its only pane, the whole tab goes away. Every tab then shifts the
    /// indices above the removed slot down by one.
    ///
    /// When the removed pane held keyboard focus, focus follows the freed
    /// space into the preceding leaf of the same tab. `old_active` is the
    /// focused session before removal.
    fn prune_closed_pane(&mut self, index: usize, old_active: usize) {
        let owner = self.tab_of_session(index);
        // Neighbor of the closing leaf, computed in old index space before the
        // tree mutates (previous leaf in render order, else the next).
        let neighbor = owner.and_then(|tab| {
            let leaves = self.tabs[tab].sessions();
            let pos = leaves.iter().position(|&s| s == index)?;
            if pos > 0 {
                leaves.get(pos - 1).copied()
            } else {
                leaves.get(1).copied()
            }
        });
        if let (Some(tab), Some(neighbor)) = (owner, neighbor) {
            // Keep the tab focused on a surviving pane before the reindex, so
            // `repair_focus` does not have to guess.
            if self.tabs[tab].focus == index {
                self.tabs[tab].focus = neighbor;
            }
        }

        // A tab whose only pane just closed has nothing left to show.
        let emptied = owner.filter(|&tab| self.tabs[tab].tree.leaf_count() <= 1);
        if let Some(tab) = emptied {
            if self.tabs.len() > 1 {
                self.tabs.remove(tab);
                if tab < self.active_tab {
                    self.active_tab -= 1;
                }
                self.active_tab = self.active_tab.min(self.tabs.len() - 1);
            }
        }

        self.reindex_tabs_after_removal(index);

        let remap = |s: usize| if s > index { s - 1 } else { s };
        let fallback = remap(old_active).min(self.sessions.len().saturating_sub(1));
        self.active_tab = self.active_tab.min(self.tabs.len().saturating_sub(1));
        self.active = self
            .tabs
            .get(self.active_tab)
            .map(|tab| tab.focus)
            .unwrap_or(fallback);
        if emptied.is_some() {
            self.pane_zoomed = false;
            self.hovered_divider = None;
            self.dragging_divider = None;
        }
        self.relayout();
    }

    fn busy_session_name(&self, index: usize) -> Option<String> {
        self.sessions
            .get(index)
            .and_then(|session| session.fg_proc_cache.clone().or_else(|| session.fg_proc()))
    }

    /// Public entry point for close requests originating from user actions.
    /// Pops a confirmation overlay when the target tab is running a non-shell
    /// foreground process; otherwise closes immediately. Batch close operations
    /// preflight every affected session before reaching the force-close helper.
    fn request_close_session(&mut self, index: usize) -> Task<Message> {
        self.request_close_session_then(index, None)
    }

    fn request_close_session_then(
        &mut self,
        index: usize,
        activate_after: Option<usize>,
    ) -> Task<Message> {
        let busy = self.busy_session_name(index);
        if let Some(name) = busy {
            if let Some(session) = self.sessions.get(index) {
                self.tab_close_confirm =
                    Some((session.id, name, PendingClose::Session { activate_after }));
            }
            return Task::none();
        }
        self.close_session_then(index, activate_after)
    }

    fn close_session_then(&mut self, index: usize, activate_after: Option<usize>) -> Task<Message> {
        let task = self.close_session(index);
        if let Some(id) = activate_after {
            if let Some(remaining) = self.sessions.iter().position(|session| session.id == id) {
                // The target lives in some tab's pane; switch to it there
                // instead of pulling it into the current tab.
                self.focus_session(remaining);
                self.relayout();
                self.refresh_active_context();
                self.save_session_snapshot();
            }
        }
        task
    }

    /// Refuse a whole-window exit while a foreground job is still attached.
    /// The user can inspect and close that tab explicitly, which uses the normal
    /// per-process confirmation flow instead of silently terminating work.
    fn request_window_close(&mut self) -> Task<Message> {
        if let Some((index, process)) = (0..self.sessions.len())
            .find_map(|index| self.busy_session_name(index).map(|name| (index, name)))
        {
            self.focus_session(index);
            self.relayout();
            self.refresh_active_context();
            self.push_toast(
                format!("{process} is still running — close its tab first"),
                ToastKind::Warning,
            );
            return Task::none();
        }
        self.agent.persist();
        self.save_session_snapshot();
        if !jterm_core::execution_journal::flush(std::time::Duration::from_secs(2)) {
            log::warn!("jsh execution journal did not flush before exit");
        }
        if let Err(error) =
            jterm_core::command_history::flush_pending(std::time::Duration::from_secs(2))
        {
            log::warn!("command history did not flush before exit: {error}");
        }
        iced::exit()
    }

    /// Next/Prev walk tabs, not the session vector: the extra sessions a split
    /// creates live inside a tab, and cycling through them here would treat
    /// panes as tabs. Moving between panes is PaneNext/PanePrev.
    fn next_session(&mut self) {
        if self.tabs.len() > 1 {
            self.activate_tab((self.active_tab + 1) % self.tabs.len());
        }
    }

    fn prev_session(&mut self) {
        if self.tabs.len() > 1 {
            self.activate_tab((self.active_tab + self.tabs.len() - 1) % self.tabs.len());
        }
    }

    /// Alt+N selects the Nth tab, matching the order in the strip.
    fn jump_session(&mut self, index: usize) {
        self.activate_tab(index);
    }

    /// Push a transient bottom-right toast. Auto-expires; dismissable.
    fn push_toast(&mut self, text: impl Into<String>, kind: ToastKind) {
        const TOAST_TTL_MS: u64 = 2400;
        const MAX_TOASTS: usize = 4;
        self.toasts.push(Toast {
            text: text.into(),
            kind,
            expires_at: std::time::Instant::now() + std::time::Duration::from_millis(TOAST_TTL_MS),
        });
        // Drop oldest if we exceed cap so the stack never grows past MAX_TOASTS.
        if self.toasts.len() > MAX_TOASTS {
            let drop = self.toasts.len() - MAX_TOASTS;
            self.toasts.drain(0..drop);
        }
    }

    /// Drop expired toasts. Cheap; called from the periodic tick.
    fn expire_toasts(&mut self) {
        let now = std::time::Instant::now();
        self.toasts.retain(|t| t.expires_at > now);
    }

    /// Typing or pasting into a held-open task transcript cannot reach a
    /// child — the PTY is gone. Show one throttled hint instead of a dead
    /// echo; the pane header carries the persistent "exited" marker.
    fn hint_read_only_transcript(&mut self) {
        const HINT_INTERVAL: std::time::Duration = std::time::Duration::from_millis(1600);
        let now = std::time::Instant::now();
        if self
            .read_only_hint_at
            .is_some_and(|at| now.duration_since(at) < HINT_INTERVAL)
        {
            return;
        }
        self.read_only_hint_at = Some(now);
        self.push_toast(
            "Task terminal exited; its transcript is read-only",
            ToastKind::Info,
        );
    }

    /// Apply a tab context-menu action. Close/CloseOthers/CloseToRight close
    /// whole tabs — every pane in them, PTYs included; Duplicate opens a new
    /// tab at the target's cwd next to it.
    ///
    /// The ids are tab ids, and every batch preflights each affected tab for a
    /// running foreground process before anything is torn down.
    fn execute_tab_menu_action(&mut self, action: TabMenuAction) -> Task<Message> {
        match action {
            TabMenuAction::Close(id) => {
                let Some(tab) = self.tab_index_by_id(id) else {
                    return Task::none();
                };
                self.request_close_tab(tab)
            }
            TabMenuAction::CloseOthers(keep_id) => {
                let Some(keep) = self.tab_index_by_id(keep_id) else {
                    return Task::none();
                };
                let targets: Vec<usize> = (0..self.tabs.len()).filter(|&i| i != keep).collect();
                self.close_tabs("Closed other tabs", targets)
            }
            TabMenuAction::CloseToRight(anchor_id) => {
                let Some(anchor) = self.tab_index_by_id(anchor_id) else {
                    return Task::none();
                };
                let targets: Vec<usize> = ((anchor + 1)..self.tabs.len()).collect();
                self.close_tabs("Closed tabs to the right", targets)
            }
            TabMenuAction::Duplicate(id) => {
                let Some(tab) = self.tab_index_by_id(id) else {
                    return Task::none();
                };
                // Duplicate copies the tab's selected pane, matching the label
                // the user right-clicked.
                let Some(source) = self.tab_focus(tab) else {
                    return Task::none();
                };
                let private_title = self.tabs[tab].private_title;
                let cwd = self
                    .sessions
                    .get(source)
                    .and_then(|s| s.cwd_cache.clone().or_else(|| s.cwd()));
                match Session::spawn(
                    &self.config,
                    self.next_id,
                    self.cols,
                    self.rows,
                    cwd.as_deref(),
                ) {
                    Ok(session) => {
                        self.session_diagnostic = None;
                        self.next_id += 1;
                        let insert = (source + 1).min(self.sessions.len());
                        self.sessions.insert(insert, session);
                        self.reindex_tabs_after_insert(insert);
                        self.active_tab = tab;
                        self.open_tab_with(insert);
                        self.tabs[self.active_tab].private_title = private_title;
                        self.relayout();
                        self.refresh_active_context();
                        self.save_session_snapshot();
                        self.push_toast("Duplicated tab", ToastKind::Success);
                    }
                    Err(error) => {
                        let message = error.to_string();
                        log::error!("[PTY] {message}");
                        self.session_diagnostic = Some(message.clone());
                        self.push_toast(message, ToastKind::Warning);
                    }
                }
                Task::none()
            }
            TabMenuAction::CloseMarked => {
                let targets: Vec<usize> = (0..self.tabs.len())
                    .filter(|&tab| self.tabs[tab].marked)
                    .collect();
                if targets.is_empty() {
                    return Task::none();
                }
                self.close_tabs("Closed marked tabs", targets)
            }
            TabMenuAction::NewTab => {
                self.new_session();
                Task::none()
            }
            TabMenuAction::TogglePinned(id) => {
                let Some(tab) = self.tab_index_by_id(id) else {
                    return Task::none();
                };
                let pinned = !self.tabs[tab].pinned;
                self.tabs[tab].pinned = pinned;
                // Pinning reorders the strip, so the active tab has to be
                // re-found by identity rather than kept by index.
                self.active_tab = sort_pinned_first(&mut self.tabs, self.active_tab);
                self.dragging_tab = None;
                self.session_dirty = true;
                self.push_toast(
                    if pinned { "Pinned tab" } else { "Unpinned tab" },
                    ToastKind::Info,
                );
                Task::none()
            }
            TabMenuAction::ToggleMarked(id) => {
                let Some(tab) = self.tab_index_by_id(id) else {
                    return Task::none();
                };
                let marked = !self.tabs[tab].marked;
                self.tabs[tab].marked = marked;
                self.session_dirty = true;
                self.push_toast(
                    if marked {
                        "Marked tab as important"
                    } else {
                        "Cleared tab mark"
                    },
                    ToastKind::Info,
                );
                Task::none()
            }
            TabMenuAction::TogglePrivateTitle(id) => {
                let Some(tab) = self.tab_index_by_id(id) else {
                    return Task::none();
                };
                let private = !self.tabs[tab].private_title;
                self.tabs[tab].private_title = private;
                self.session_dirty = true;
                self.push_toast(
                    if private {
                        "Tab title details hidden"
                    } else {
                        "Tab title details visible"
                    },
                    ToastKind::Info,
                );
                Task::none()
            }
            TabMenuAction::ConnectRemote(index) => {
                self.connect_remote_host(index);
                Task::none()
            }
        }
    }

    /// Apply the context menu's Rename. An empty name clears the custom title,
    /// putting the tab back to following its focused session's label.
    fn apply_tab_rename(&mut self, id: usize, raw: String) {
        let Some(tab) = self.tab_index_by_id(id) else {
            return;
        };
        // The title is persisted and drawn verbatim in the strip; hold it to
        // the same contract the snapshot loader enforces on the way back in.
        // `take` counts chars while the snapshot bound counts bytes, and a char
        // is at most 4 bytes, so this can never exceed that bound.
        let max_chars = session_persistence::MAX_RESTORED_TAB_TITLE_BYTES / 4;
        let cleaned: String = raw
            .trim()
            .chars()
            .filter(|c| !c.is_control())
            .take(max_chars)
            .collect();
        self.tabs[tab].title = (!cleaned.is_empty()).then_some(cleaned);
        self.session_dirty = true;
    }

    /// Close a set of tabs, refusing the whole batch if any pane in any of them
    /// still runs a foreground process — a bulk action should not be the way a
    /// running job gets killed without a prompt.
    ///
    /// Tabs are resolved to stable ids up front and closed one at a time, since
    /// each close shifts the indices of the tabs still queued.
    fn close_tabs(&mut self, done_message: &str, targets: Vec<usize>) -> Task<Message> {
        if let Some((index, process)) = targets
            .iter()
            .flat_map(|&tab| self.tabs_sessions(tab))
            .find_map(|session| self.busy_session_name(session).map(|name| (session, name)))
        {
            self.focus_session(index);
            self.relayout();
            self.refresh_active_context();
            self.push_toast(
                format!("{process} is still running — close that tab explicitly"),
                ToastKind::Warning,
            );
            return Task::none();
        }
        let ids: Vec<usize> = targets
            .iter()
            .filter_map(|&tab| self.tabs.get(tab).map(|tab| tab.id))
            .collect();
        let mut tasks: Vec<Task<Message>> = Vec::new();
        for id in ids {
            if let Some(tab) = self.tab_index_by_id(id) {
                tasks.push(self.close_tab(tab));
            }
        }
        self.push_toast(done_message.to_string(), ToastKind::Info);
        Task::batch(tasks)
    }

    fn tabs_sessions(&self, tab: usize) -> Vec<usize> {
        self.tabs
            .get(tab)
            .map(|tab| tab.sessions())
            .unwrap_or_default()
    }

    /// Move tab `from` to position `to`, shifting the tabs in between.
    ///
    /// Only the strip's order changes. The session vector and every tab's
    /// panes stay put: dragging one tab must not permute the panes inside
    /// another one, which is exactly what reordering the session vector did
    /// back when a tab *was* a session.
    fn reorder_tab(&mut self, from: usize, to: usize) {
        let Some(active_tab) =
            reorder_tabs_preserving_pinned_prefix(&mut self.tabs, self.active_tab, from, to)
        else {
            return;
        };
        self.active_tab = active_tab;
        self.session_dirty = true;
        self.refresh_active_context();
        self.save_session_snapshot();
    }

    /// Commit the currently previewed ordinary-tab → split-pane drop.
    fn finish_tab_split_drop(&mut self) {
        // The pointer may arm an edge and then switch/zoom the visible page
        // without moving again. Re-resolve every final topology condition here
        // so a stale overlay cannot graft the source into a hidden old target.
        let request =
            self.dragging_tab
                .zip(self.tab_split_drop)
                .and_then(|(source_tab_id, drop)| {
                    let source_tab = self.tab_index_by_id(source_tab_id);
                    let source_is_plain = source_tab
                        .and_then(|tab| self.tabs.get(tab))
                        .is_some_and(|tab| tab.tree.is_leaf());
                    let target_session = self.session_index_by_id(drop.target_session_id)?;
                    let target_tab = self.tab_of_session(target_session);
                    tab_split_commit_allowed(
                        source_tab,
                        source_is_plain,
                        self.active_tab,
                        target_tab,
                        self.layout().leaf_count(),
                        self.pane_zoomed,
                    )
                    .then_some((source_tab_id, target_session, drop.direction))
                });
        let result = request.and_then(|(source_tab_id, target_session, direction)| {
            move_plain_tab_into_split(&mut self.tabs, source_tab_id, target_session, direction)
        });
        let origin = self.tab_drag_origin.take();
        self.dragging_tab = None;
        self.tab_split_drop = None;
        self.tab_drag_moved = false;
        self.tab_drag_hover_since = None;
        let Some((target_tab, focused_session)) = result else {
            self.restore_tab_drag_origin(origin);
            return;
        };

        if focused_session != self.active {
            self.close_block_search_on_session_change();
        }
        self.active_tab = target_tab;
        self.active = focused_session;
        self.pane_zoomed = false;
        self.hovered_divider = None;
        self.dragging_divider = None;
        self.session_dirty = true;
        self.relayout();
        self.refresh_active_context();
        self.save_session_snapshot();
        self.push_toast("Tab moved into split".to_string(), ToastKind::Success);
    }

    fn restore_tab_drag_origin(&mut self, origin: Option<usize>) {
        let Some(tab) = origin.and_then(|id| self.tab_index_by_id(id)) else {
            return;
        };
        if tab != self.active_tab {
            self.activate_tab(tab);
        }
    }

    fn cancel_tab_drag(&mut self) {
        let origin = self.tab_drag_origin.take();
        self.dragging_tab = None;
        self.tab_split_drop = None;
        self.tab_drag_moved = false;
        self.tab_drag_hover_since = None;
        self.restore_tab_drag_origin(origin);
    }

    fn cancel_layout_drags(&mut self) {
        self.cancel_tab_drag();
        self.pane_drag = None;
        self.dragging_divider = None;
        self.hovered_divider = None;
        self.dragging_sidebar = false;
    }

    /// Commit the current split-pane → ordinary-tab drop. `after_tab_id` is
    /// stable across tab reordering; `None` means immediately after its owner.
    fn finish_pane_promotion(&mut self, after_tab_id: Option<usize>) {
        let Some(drag) = self.pane_drag.take() else {
            return;
        };
        let Some(source_session) = self.session_index_by_id(drag.session_id) else {
            return;
        };
        let Some((new_tab, focused_session)) = promote_split_pane_to_tab(
            &mut self.tabs,
            &mut self.next_tab_id,
            source_session,
            after_tab_id,
        ) else {
            return;
        };

        if focused_session != self.active {
            self.close_block_search_on_session_change();
        }
        self.active_tab = new_tab;
        self.active = focused_session;
        self.pane_zoomed = false;
        self.hovered_divider = None;
        self.dragging_divider = None;
        self.session_dirty = true;
        self.relayout();
        self.refresh_active_context();
        self.save_session_snapshot();
        self.push_toast("Pane moved to a new tab".to_string(), ToastKind::Success);
    }

    /// The active tab's pane tree. Every split/focus/divider operation goes
    /// through here, so it cannot reach into another tab.
    fn layout(&self) -> &PaneTree {
        &self.tabs[self.active_tab.min(self.tabs.len() - 1)].tree
    }

    fn layout_mut(&mut self) -> &mut PaneTree {
        let idx = self.active_tab.min(self.tabs.len() - 1);
        &mut self.tabs[idx].tree
    }

    /// Move keyboard focus to a pane of the *active* tab. Also records the
    /// choice on the tab, so returning to it restores the same pane.
    fn set_focus(&mut self, session: usize) {
        if session != self.active {
            // Every pane/tab focus switch funnels through here (pane clicks,
            // Alt+arrows, `focus_session`, the tab switcher): the block
            // search picker must not survive the active session changing.
            self.close_block_search_on_session_change();
        }
        self.active = session;
        let idx = self.active_tab.min(self.tabs.len() - 1);
        self.tabs[idx].focus = session;
    }

    /// The tab owning `session`. Each session lives in exactly one pane of one
    /// tab, so there is at most one answer.
    fn tab_of_session(&self, session: usize) -> Option<usize> {
        self.tabs.iter().position(|tab| tab.contains(session))
    }

    /// The session a tab displays: its selected pane. Tab labels, the tab
    /// switcher and the window title all read this.
    fn tab_focus(&self, tab: usize) -> Option<usize> {
        self.tabs.get(tab).map(|tab| tab.focus)
    }

    /// One label per tab, in strip order — what the switcher matches against.
    fn tab_labels(&self) -> Vec<String> {
        (0..self.tabs.len()).map(|i| self.tab_label(i)).collect()
    }

    /// A tab's strip label. A title set through the context menu's Rename wins;
    /// otherwise the tab keeps following its focused session's own label.
    fn tab_label(&self, tab: usize) -> String {
        if self.tabs.get(tab).is_some_and(|tab| tab.private_title) {
            return "Private".to_string();
        }
        self.tab_real_label(tab)
    }

    /// The title retained behind the privacy mask. Shell title updates and
    /// rename keep changing this value so revealing it restores immediately.
    fn tab_real_label(&self, tab: usize) -> String {
        if let Some(title) = self.tabs.get(tab).and_then(|tab| tab.title.clone()) {
            return title;
        }
        self.tab_focus(tab)
            .and_then(|index| self.sessions.get(index))
            .map(|session| session.label())
            .unwrap_or_default()
    }

    /// Pin / mark indicators prefixed to a tab's label. They are the only
    /// on-strip evidence of state the context menu can set, so both placements
    /// (top strip and sidebar dock) render them.
    fn tab_state_prefix(&self, tab: usize) -> String {
        let Some(tab) = self.tabs.get(tab) else {
            return String::new();
        };
        let mut prefix = String::new();
        if tab.pinned {
            prefix.push('◆');
        }
        if tab.marked {
            prefix.push('★');
        }
        if !prefix.is_empty() {
            prefix.push(' ');
        }
        prefix
    }

    /// Switch to `tab` and hand keyboard focus to the pane it had selected.
    fn activate_tab(&mut self, tab: usize) {
        if tab >= self.tabs.len() {
            return;
        }
        self.active_tab = tab;
        self.tabs[tab].repair_focus();
        if self.tabs[tab].focus != self.active {
            // Tab activation bypasses `set_focus`; same rule — the block
            // search picker must not survive the active session changing.
            self.close_block_search_on_session_change();
        }
        self.active = self.tabs[tab].focus;
        self.pane_zoomed = false;
        self.hovered_divider = None;
        self.dragging_divider = None;
        self.session_dirty = true;
        self.relayout();
        self.refresh_active_context();
    }

    /// Focus `session` wherever it lives, switching tabs if it belongs to
    /// another one. Unlike the old `focus_or_replace_session`, a session is
    /// never moved into a different pane: pane ownership is fixed.
    fn focus_session(&mut self, session: usize) -> bool {
        let Some(tab) = self.tab_of_session(session) else {
            return false;
        };
        if tab != self.active_tab {
            self.active_tab = tab;
            self.pane_zoomed = false;
            self.hovered_divider = None;
            self.dragging_divider = None;
        }
        self.set_focus(session);
        true
    }

    /// Rebuild the tab list from a snapshot, after the sessions have spawned.
    ///
    /// The snapshot is external input, so every index is validated: a tab keeps
    /// only leaves that name a session that exists and that no earlier tab
    /// already claimed — two tabs sharing a pane would fight over one PTY.
    ///
    /// The closing step adopts orphans. Validation can empty a tab, a shell can
    /// fail to restore, and a v1 snapshot's sessions may not appear in its
    /// single tree at all; every session no tab claims gets a one-pane tab,
    /// because an unclaimed session is a live PTY nothing can switch to.
    fn restore_tabs(
        &mut self,
        saved_tabs: Vec<session_persistence::TabSnapshot>,
        saved_active_tab: Option<usize>,
        legacy_tree: Option<session_persistence::PaneTreeSnapshot>,
        legacy_split: Option<session_persistence::SplitSnapshot>,
    ) {
        if self.sessions.is_empty() {
            return;
        }
        // A v1 snapshot has one global tree: it described the panes the user
        // last saw, so it migrates into the first tab rather than scattering.
        let migrated: Vec<RestoredTab> = if saved_tabs.is_empty() {
            legacy_tree
                .as_ref()
                .and_then(pane_tree_from_snapshot)
                .or_else(|| legacy_split.as_ref().and_then(pane_tree_from_legacy))
                // v1 had no per-tab focus; the snapshot's active session is
                // the pane the user was in, so seed the migrated tab with it.
                .map(|tree| vec![RestoredTab::plain(tree, Some(self.active))])
                .unwrap_or_default()
        } else {
            saved_tabs
                .iter()
                .filter_map(|snapshot| {
                    pane_tree_from_snapshot(&snapshot.tree).map(|tree| RestoredTab {
                        tree,
                        focus: snapshot.focus,
                        title: snapshot.title.clone(),
                        pinned: snapshot.pinned,
                        marked: snapshot.marked,
                        private_title: snapshot.private_title,
                    })
                })
                .collect()
        };

        let (tabs, active_tab, next_tab_id) =
            build_restored_tabs(migrated, self.sessions.len(), self.active, saved_active_tab);
        self.tabs = tabs;
        self.active_tab = active_tab;
        self.next_tab_id = next_tab_id;
        self.active = self.tabs[active_tab].focus;
    }

    /// Current index of the tab with this stable id, if it is still open.
    /// Anything held across UI events must go through here: tab indices shift
    /// when a tab is closed or the strip is reordered.
    fn tab_index_by_id(&self, id: usize) -> Option<usize> {
        self.tabs.iter().position(|tab| tab.id == id)
    }

    /// Open `session` in a brand new unpinned tab after the active one without
    /// ever splitting the leading pinned partition.
    fn open_tab_with(&mut self, session: usize) {
        if session != self.active {
            // New-tab activation bypasses `set_focus`; the block search
            // picker must not survive the active session changing.
            self.close_block_search_on_session_change();
        }
        let at = new_unpinned_tab_index(&self.tabs, self.active_tab);
        let id = self.next_tab_id;
        self.next_tab_id += 1;
        self.tabs.insert(at, Tab::new(id, session));
        self.active_tab = at;
        self.active = session;
        self.pane_zoomed = false;
        self.hovered_divider = None;
        self.dragging_divider = None;
    }

    /// Re-index every tab's tree after a session was inserted at `inserted`.
    /// Session indices are global, so an insert in the middle shifts the panes
    /// of every tab, not just the active one.
    fn reindex_tabs_after_insert(&mut self, inserted: usize) {
        reindex_tabs_for_insert(&mut self.tabs, inserted);
        self.active = if self.active >= inserted {
            self.active + 1
        } else {
            self.active
        };
    }

    /// Re-index every tab's tree after `removed` left the session vector.
    /// The owning tab drops its leaf first; a tab left with no pane is gone
    /// (its caller removed it), and the rest only shift indices down.
    fn reindex_tabs_after_removal(&mut self, removed: usize) {
        reindex_tabs_for_removal(&mut self.tabs, removed);
    }

    /// Whether the active tab is currently split (more than one pane).
    fn is_split(&self) -> bool {
        !self.layout().is_leaf()
    }

    fn tab_split_drag_eligible(&self) -> bool {
        self.dragging_tab.and_then(|source_id| {
            let source = self.tab_index_by_id(source_id)?;
            matches!(self.tabs[source].tree, PaneTree::Leaf(_)).then_some(source != self.active_tab)
        }) == Some(true)
            && !self.pane_zoomed
            && self.layout().leaf_count() < MAX_PANES
    }

    /// The whole terminal-area rectangle the pane tree is laid out within.
    fn layout_area(&self) -> pane_layout::Rect {
        pane_layout::Rect {
            x: 0.0,
            y: 0.0,
            width: self.term_width(),
            height: self.term_height(),
        }
    }

    /// Every leaf's session index and pixel rectangle, in render order.
    fn pane_rects(&self) -> Vec<PaneRect> {
        let mut out = Vec::new();
        collect_pane_rects(self.layout(), self.layout_area(), DIVIDER, &mut out);
        out
    }

    /// The focused leaf's position in depth-first order (for status readouts).
    fn focused_pane_pos(&self) -> usize {
        self.layout()
            .leaves()
            .iter()
            .position(|&s| s == self.active)
            .unwrap_or(0)
    }

    fn grid_pixel_size(&self, cols: usize, rows: usize) -> (u32, u32) {
        let width = (cols as f32 * self.metrics.cell_w).round().max(0.0) as u32;
        let height = (rows as f32 * self.metrics.cell_h).round().max(0.0) as u32;
        (width, height)
    }

    /// Resize one session's terminal + PTY (no-op when already that size).
    fn resize_session(&mut self, index: usize, cols: usize, rows: usize) -> Option<usize> {
        let (pixel_w, pixel_h) = self.grid_pixel_size(cols, rows);
        if let Some(sess) = self.sessions.get_mut(index) {
            sess.terminal.set_viewport_pixel_size(pixel_w, pixel_h);
            let old_dimensions = sess.terminal.get_dimensions();
            if old_dimensions != (cols, rows) {
                sess.terminal.on_resize(cols, rows);
                let _ = sess.pty.resize(cols, rows);
            }
            sess.refresh();
            return (old_dimensions.0 != cols).then_some(sess.id);
        }
        None
    }

    /// Resize every session once for the current layout. Background tabs use the
    /// full terminal area; sessions displayed in a split use their pane size.
    /// While a pane is zoomed every session gets the full area, so the zoomed
    /// pane renders full-size and unzooming is a plain relayout.
    fn relayout(&mut self) {
        let mut targets = vec![(self.cols, self.rows); self.sessions.len()];
        if self.is_split() && !self.pane_zoomed {
            for pane in self.pane_rects() {
                if pane.session < targets.len() {
                    let w = (pane.rect.width - terminal_view::SCROLLBAR_WIDTH).max(0.0);
                    // The header strip sits inside the pane rect, above the
                    // grid. Charging the PTY for those pixels would make the
                    // shell believe it has one row more than it can show.
                    let h = (pane.rect.height - PANE_HEADER_H).max(0.0);
                    targets[pane.session] = self.metrics.grid_size(w, h);
                }
            }
        }
        let mut width_changed = false;
        for (index, (cols, rows)) in targets.into_iter().enumerate() {
            if let Some(id) = self.resize_session(index, cols, rows) {
                self.history_reflow_sessions.insert(id);
                width_changed = true;
            }
        }
        if width_changed {
            self.history_reflow_due =
                Some(std::time::Instant::now() + std::time::Duration::from_millis(150));
        }
    }

    /// Split the focused pane along `axis`, spawning a fresh session at its cwd
    /// (tmux `split-window`). If the focused leaf's parent already splits along
    /// `axis` the new pane joins as a sibling; otherwise the leaf becomes a
    /// nested split. Capped at [`MAX_PANES`] total leaves as a PTY guard.
    fn split(&mut self, axis: Axis) {
        if self.layout().leaf_count() >= MAX_PANES {
            self.push_toast(
                format!("Split limit reached ({MAX_PANES} panes)"),
                ToastKind::Warning,
            );
            return;
        }
        let cwd = self.sessions.get(self.active).and_then(|s| s.cwd());
        match Session::spawn(
            &self.config,
            self.next_id,
            self.cols,
            self.rows,
            cwd.as_deref(),
        ) {
            Ok(session) => {
                self.session_diagnostic = None;
                self.next_id += 1;
                self.sessions.push(session);
                // Appending keeps every existing index valid, so no tab needs
                // reindexing here.
                let new_idx = self.sessions.len() - 1;
                let focused = self.active;
                self.layout_mut().split_leaf(focused, axis, new_idx);
                self.set_focus(new_idx);
                // Splitting while zoomed lands in the new multi-pane layout.
                self.pane_zoomed = false;
                self.relayout();
                self.refresh_active_context();
                self.save_session_snapshot();
            }
            Err(error) => {
                let message = error.to_string();
                log::error!("[PTY] {message}");
                self.session_diagnostic = Some(message.clone());
                self.push_toast(message, ToastKind::Warning);
            }
        }
    }

    /// Move keyboard focus to the next leaf in render order (wraps).
    fn focus_next_pane(&mut self) {
        let leaves = self.layout().leaves();
        if leaves.len() < 2 {
            return;
        }
        let pos = leaves.iter().position(|&s| s == self.active).unwrap_or(0);
        self.set_focus(leaves[(pos + 1) % leaves.len()]);
        self.refresh_active_context();
    }

    /// Move keyboard focus to the previous leaf in render order (wraps).
    fn focus_prev_pane(&mut self) {
        let leaves = self.layout().leaves();
        if leaves.len() < 2 {
            return;
        }
        let pos = leaves.iter().position(|&s| s == self.active).unwrap_or(0);
        self.set_focus(leaves[(pos + leaves.len() - 1) % leaves.len()]);
        self.refresh_active_context();
    }

    /// Activate `sessions[index]` through the single tab/session switching path:
    /// switch to the tab that owns it and focus its pane there. A session is
    /// never moved into another pane — pane ownership is fixed, so split
    /// topology and ratios are untouched.
    fn activate_session(&mut self, index: usize) {
        if index >= self.sessions.len() || !self.focus_session(index) {
            return;
        }
        self.session_dirty = true;
        self.relayout();
        self.refresh_active_context();
    }

    /// Current index of the session with this stable id, if it is still open.
    /// Anything held across UI events must go through here: indices shift when
    /// a session is closed or the tabs are reordered.
    fn session_index_by_id(&self, id: usize) -> Option<usize> {
        self.sessions.iter().position(|sess| sess.id == id)
    }

    /// Exchange the sessions shown by two panes. Only the contents move: the
    /// split topology and every ratio the user arranged stay exactly as they
    /// were, and focus follows the dragged session into its new pane.
    fn swap_pane_sessions(&mut self, dragged: usize, target: usize) {
        if dragged == target
            || !self.layout().contains_session(dragged)
            || !self.layout().contains_session(target)
        {
            return;
        }
        swap_sessions_in_tree(self.layout_mut(), dragged, target);
        self.set_focus(dragged);
        self.session_dirty = true;
        // The two panes usually differ in size, so both shells need a resize.
        self.relayout();
        self.refresh_active_context();
        self.save_session_snapshot();
    }

    /// Move keyboard focus to the pane physically adjacent in `direction`
    /// (tmux-style spatial navigation across nesting). No wrap at the edges.
    fn focus_pane_direction(&mut self, direction: PaneDirection) {
        let rects = self.pane_rects();
        if let Some(session) = directional_focus_target(&rects, self.active, direction) {
            self.set_focus(session);
            self.refresh_active_context();
        }
    }

    /// Grow/shrink the focused pane toward `direction` by nudging the divider on
    /// that side. Walks up to the nearest ancestor split whose axis matches the
    /// direction; no-op if there is no such divider.
    fn resize_pane_direction(&mut self, direction: PaneDirection) {
        let Some(path) = self.layout().path_to_session(self.active) else {
            return;
        };
        let wanted = direction.axis();
        let forward = direction.forward();
        // From the deepest ancestor outward, find a split on the wanted axis
        // that has a divider on `direction`'s side of the focused subtree.
        for k in (0..path.len()).rev() {
            let node_path = &path[..k];
            let child = path[k];
            let Some(PaneTree::Split {
                axis,
                children,
                ratios,
            }) = self.layout_mut().node_at_path_mut(node_path)
            else {
                continue;
            };
            if *axis != wanted {
                continue;
            }
            let gap = if forward {
                (child + 1 < children.len()).then_some(child)
            } else {
                child.checked_sub(1)
            };
            let Some(gap) = gap else {
                continue;
            };
            let step = if forward {
                SPLIT_RATIO_KEY_STEP
            } else {
                -SPLIT_RATIO_KEY_STEP
            };
            let first = ratios[gap] + step;
            if set_divider_share(ratios, gap, first, false) {
                self.relayout();
                self.refresh_active_context();
            }
            return;
        }
    }

    /// Toggle tmux-style zoom: the focused pane temporarily takes the whole
    /// terminal area without destroying the split. No-op when not split.
    fn toggle_pane_zoom(&mut self) {
        if !self.is_split() {
            return;
        }
        self.pane_zoomed = !self.pane_zoomed;
        self.relayout();
        self.refresh_active_context();
    }

    /// Exchange the focused pane's session with the next leaf's (render order);
    /// geometry stays put and focus follows the moved session, tmux-style.
    fn swap_panes(&mut self) {
        let leaves = self.layout().leaves();
        if leaves.len() < 2 {
            return;
        }
        let pos = leaves.iter().position(|&s| s == self.active).unwrap_or(0);
        let other = leaves[(pos + 1) % leaves.len()];
        self.swap_pane_sessions(self.active, other);
    }

    /// Close the focused pane's session; the remaining panes keep the split
    /// (which collapses on its own once only one pane is left).
    fn close_focused_pane(&mut self) -> Task<Message> {
        if !self.is_split() {
            return self.request_close_session(self.active);
        }
        let victim = self.active;
        // Focus lands on the preceding leaf (or the next when closing the first),
        // matching where the freed space goes.
        let leaves = self.layout().leaves();
        let pos = leaves.iter().position(|&s| s == victim).unwrap_or(0);
        let keep = if pos > 0 {
            leaves.get(pos - 1)
        } else {
            leaves.get(1)
        };
        let keep_id = keep.and_then(|&idx| self.sessions.get(idx).map(|session| session.id));
        self.request_close_session_then(victim, keep_id)
    }

    /// Look up a key event in the configurable keybindings and run the bound
    /// command. Returns the resulting task when a binding matched and applied,
    /// or `None` to let the key fall through to other handlers / the PTY.
    fn handle_keybinding(
        &mut self,
        key: &keyboard::Key,
        mods: keyboard::Modifiers,
    ) -> Option<Task<Message>> {
        let cmd = resolve_keybinding_command(&self.keybindings, key, mods)?;
        self.dispatch_command(cmd)
    }

    /// Execute a bound [`keybindings::Command`]. Returns `None` for commands
    /// that don't apply in the current context (e.g. search navigation while
    /// the search bar is closed) so the key can fall through.
    fn dispatch_command(&mut self, cmd: keybindings::Command) -> Option<Task<Message>> {
        use keybindings::Command as C;
        if command_requires_block_context(&cmd) && !self.block_binding_available() {
            return None;
        }
        // Write raw bytes to the focused session's PTY (control-key commands).
        let mut send = |bytes: &[u8]| {
            if self
                .sessions
                .get(self.active)
                .is_some_and(Session::transcript_read_only)
            {
                self.hint_read_only_transcript();
                return;
            }
            if let Some(sess) = self.sessions.get_mut(self.active) {
                // A PTY-bound keystroke dismisses the block selection.
                sess.block_selection.clear();
                sess.terminal.scroll_to_bottom();
                sess.projection_view_state.scroll_to_bottom();
                sess.write_pty(bytes);
                sess.refresh();
            }
        };
        let task = match cmd {
            C::SessionNew => {
                self.new_session();
                Task::none()
            }
            // Closing "the session" from a keybinding means the tab, panes
            // and all; a single pane is TerminalClosePane.
            C::SessionClose => return Some(self.request_close_tab(self.active_tab)),
            C::WindowClose => return Some(self.request_window_close()),
            C::SessionNext => {
                self.next_session();
                Task::none()
            }
            C::SessionPrev => {
                self.prev_session();
                Task::none()
            }
            C::SessionJump(n) => {
                self.jump_session(n);
                Task::none()
            }
            C::SessionLast => {
                if let Some(last) = last_session_index(self.tabs.len()) {
                    self.jump_session(last);
                }
                Task::none()
            }
            C::EditCopy | C::EditCopyBlockOutput => {
                self.edit_copy_task(matches!(cmd, C::EditCopyBlockOutput))
            }
            C::EditPaste => {
                let id = self.sessions.get(self.active)?.id;
                iced::clipboard::read().map(move |text| Message::Pasted(id, text))
            }
            C::SearchOpen => {
                self.search.toggle();
                self.recompute_search();
                self.reveal_current_search_match();
                if self.search.is_open {
                    iced::widget::operation::focus(SEARCH_INPUT_ID.clone())
                } else {
                    Task::none()
                }
            }
            C::SearchClose => {
                if !self.search.is_open {
                    return None;
                }
                self.search.close();
                Task::none()
            }
            C::SearchNext => {
                if !self.search.is_open {
                    return None;
                }
                self.search.next_match();
                self.reveal_current_search_match();
                Task::none()
            }
            C::SearchPrev => {
                if !self.search.is_open {
                    return None;
                }
                self.search.prev_match();
                self.reveal_current_search_match();
                Task::none()
            }
            C::SearchHistoryPrev => {
                if !self.search.is_open {
                    return None;
                }
                self.search.history_prev();
                self.search.current_match_index = 0;
                self.recompute_search();
                self.reveal_current_search_match();
                Task::none()
            }
            C::SearchHistoryNext => {
                if !self.search.is_open {
                    return None;
                }
                self.search.history_next();
                self.search.current_match_index = 0;
                self.recompute_search();
                self.reveal_current_search_match();
                Task::none()
            }
            C::SearchReplaceToggle => {
                self.search_replace.toggle();
                if self.search_replace.is_open {
                    iced::widget::operation::focus(SEARCH_REPLACE_FIND_ID.clone())
                } else {
                    Task::none()
                }
            }
            C::TerminalSendSigint => {
                send(&[0x03]);
                Task::none()
            }
            C::TerminalSendEof => {
                send(&[0x04]);
                Task::none()
            }
            C::TerminalClear => {
                send(&[0x0c]);
                Task::none()
            }
            C::TerminalScrollUp | C::TerminalScrollDown => {
                let older = matches!(cmd, C::TerminalScrollUp);
                let block_navigation = self.sessions.get_mut(self.active).map_or(
                    block_mode::SelectionNavigation::Passthrough,
                    |sess| {
                        let ids: Vec<u64> = sess
                            .terminal
                            .command_zones
                            .iter()
                            .map(|zone| zone.id)
                            .collect();
                        sess.block_selection.retain(&ids);
                        ctrl_scroll_block_navigation(
                            self.config.block_mode,
                            older,
                            sess.terminal.is_alt_buffer_active(),
                            sess.terminal.is_command_running(),
                            &ids,
                            sess.block_selection.active(),
                        )
                    },
                );
                match block_navigation {
                    block_mode::SelectionNavigation::Select(target) => {
                        self.select_and_reveal_block(target);
                        return Some(Task::none());
                    }
                    block_mode::SelectionNavigation::Clear => {
                        if let Some(sess) = self.sessions.get_mut(self.active) {
                            sess.block_selection.clear();
                            sess.refresh();
                        }
                        return Some(Task::none());
                    }
                    block_mode::SelectionNavigation::Passthrough => {}
                }
                let speed = self.config.scroll_speed.max(1) as isize;
                let delta = if older { speed } else { -speed };
                if let Some(sess) = self.sessions.get_mut(self.active) {
                    sess.scroll(delta);
                }
                Task::none()
            }
            C::TerminalCopyLastOutput => self.copy_last_output_task(),
            C::BlockJumpFirstFailed => self.block_jump_first_failed(),
            C::BlockJumpPrevFailed => self.block_jump_failed_step(true),
            C::BlockJumpNextFailed => self.block_jump_failed_step(false),
            C::BlockCopyCommand => self.block_copy_command_task(),
            C::BlockCopyOutput => self.block_copy_output_task(),
            C::BlockRecallCommand => self.block_recall_command_task(),
            C::BlockSelectAll => self.block_select_all(),
            C::BlockClear => self.request_block_clear(),
            C::BlockSelectPrev => self.block_select_step(true),
            C::BlockSelectNext => self.block_select_step(false),
            C::BlockReinputSelectedCommands => self.block_reinput_selected_commands_task(),
            C::BlockCopyBlock => self.block_copy_block_task(),
            C::BlockCopyMarkdown => self.block_copy_markdown_task(),
            C::BlockExportSessionMarkdown => {
                self.block_export_session_task(block_export::SessionExportFormat::Markdown)
            }
            C::BlockExportSessionJson => {
                self.block_export_session_task(block_export::SessionExportFormat::Json)
            }
            C::BlockSearch => self.toggle_block_search(),
            C::BlockToggleBookmark => {
                let target = {
                    let sess = self.sessions.get_mut(self.active)?;
                    let ids: Vec<u64> = sess
                        .terminal
                        .command_zones
                        .iter()
                        .map(|zone| zone.id)
                        .collect();
                    sess.block_selection.retain(&ids);
                    active_bookmark_target(&ids, sess.block_selection.active())
                }?;
                self.block_toggle_bookmark(target)
            }
            C::BlockJumpPrevBookmark => self.block_jump_bookmark(true),
            C::BlockJumpNextBookmark => self.block_jump_bookmark(false),
            C::BlockFixWithAgent => {
                self.palette_failed_block_agent_task(FailedBlockAgentIntent::Fix)
            }
            C::BlockExplainWithAgent => {
                self.palette_failed_block_agent_task(FailedBlockAgentIntent::Explain)
            }
            C::BlockRetryFailed => self.palette_failed_block_retry_task(),
            C::TerminalPromptPrev | C::TerminalPromptNext => {
                if !self.ensure_block_action_available("Prompt navigation") {
                    return Some(Task::none());
                }
                if let Some(sess) = self.sessions.get_mut(self.active) {
                    sess.jump_prompt(matches!(cmd, C::TerminalPromptPrev));
                }
                Task::none()
            }
            C::TerminalSplitVertical => {
                self.split(Axis::Vertical);
                Task::none()
            }
            C::TerminalSplitHorizontal => {
                self.split(Axis::Horizontal);
                Task::none()
            }
            C::TerminalClosePane => return Some(self.close_focused_pane()),
            C::PaneFocusNext => {
                self.focus_next_pane();
                Task::none()
            }
            C::PaneFocusPrev => {
                self.focus_prev_pane();
                Task::none()
            }
            C::PaneFocusLeft => {
                self.focus_pane_direction(PaneDirection::Left);
                Task::none()
            }
            C::PaneFocusRight => {
                self.focus_pane_direction(PaneDirection::Right);
                Task::none()
            }
            C::PaneFocusUp => {
                self.focus_pane_direction(PaneDirection::Up);
                Task::none()
            }
            C::PaneFocusDown => {
                self.focus_pane_direction(PaneDirection::Down);
                Task::none()
            }
            C::PaneResizeLeft => {
                self.resize_pane_direction(PaneDirection::Left);
                Task::none()
            }
            C::PaneResizeRight => {
                self.resize_pane_direction(PaneDirection::Right);
                Task::none()
            }
            C::PaneResizeUp => {
                self.resize_pane_direction(PaneDirection::Up);
                Task::none()
            }
            C::PaneResizeDown => {
                self.resize_pane_direction(PaneDirection::Down);
                Task::none()
            }
            C::PaneZoomToggle => {
                self.toggle_pane_zoom();
                Task::none()
            }
            C::PaneSwap => {
                self.swap_panes();
                Task::none()
            }
            C::ConfigOpen => {
                self.config_panel_open = true;
                Task::none()
            }
            C::ConfigClose => {
                self.config_panel_open = false;
                Task::none()
            }
            C::ConfigToggle => {
                self.config_panel_open = !self.config_panel_open;
                Task::none()
            }
            C::SidebarToggle => self.toggle_sidebar(),
            C::AgentToggle => {
                if self.agent.is_open {
                    self.agent.close();
                    Task::none()
                } else {
                    let session_id = self.sessions.get(self.active).map(|s| s.id).unwrap_or(0);
                    self.agent.open(&self.config, session_id);
                    let focus = iced::widget::operation::focus(AGENT_INPUT_ID.clone());
                    match self.agent_drive_task() {
                        Some(task) => Task::batch([focus, task]),
                        None => focus,
                    }
                }
            }
            C::FontZoomIn => {
                self.adjust_font_size(1.0);
                Task::none()
            }
            C::FontZoomOut => {
                self.adjust_font_size(-1.0);
                Task::none()
            }
            C::FontZoomReset => {
                self.reset_font_size();
                Task::none()
            }
            C::OpacityIncrease => {
                self.adjust_opacity(0.025);
                Task::none()
            }
            C::OpacityDecrease => {
                self.adjust_opacity(-0.025);
                Task::none()
            }
        };
        Some(task)
    }

    /// Non-configurable app-chrome shortcuts that have no [`keybindings::Command`]
    /// (command palette, diagnostics, and help overlays). Returns `Some` when the
    /// keypress was consumed.
    fn handle_tab_shortcut(
        &mut self,
        key: &keyboard::Key,
        mods: keyboard::Modifiers,
    ) -> Option<Task<Message>> {
        match chrome_shortcut(key, mods)? {
            ChromeShortcut::CommandPalette => {
                self.palette.toggle();
                Some(if self.palette.is_open {
                    iced::widget::operation::focus(PALETTE_INPUT_ID.clone())
                } else {
                    Task::none()
                })
            }
            ChromeShortcut::Help => {
                self.help_open = !self.help_open;
                Some(Task::none())
            }
            ChromeShortcut::TabSwitcher => {
                if self.tab_switcher.is_some() {
                    self.tab_switcher = None;
                    return Some(Task::none());
                }
                self.tab_switcher = Some(TabSwitcherState::default());
                Some(iced::widget::operation::focus(
                    TAB_SWITCHER_INPUT_ID.clone(),
                ))
            }
            ChromeShortcut::HistoryPicker => {
                if self.history_picker.is_some() {
                    self.history_picker = None;
                    return Some(Task::none());
                }
                Some(self.open_history_picker())
            }
            ChromeShortcut::RemoteHosts => {
                if self.remote_picker.is_some() {
                    self.remote_picker = None;
                } else {
                    self.remote_picker = Some(0);
                }
                Some(Task::none())
            }
            ChromeShortcut::Debug => {
                self.debug_open = !self.debug_open;
                Some(Task::none())
            }
        }
    }

    /// Remote picker key handling. Mirrors `handle_tab_switcher_key`: arrows
    /// move, Enter connects, Esc or the toggle chord dismisses.
    fn handle_remote_picker_key(
        &mut self,
        key: &keyboard::Key,
        mods: keyboard::Modifiers,
    ) -> Option<Task<Message>> {
        use keyboard::key::Named;
        use keyboard::Key;
        if chrome_shortcut(key, mods) == Some(ChromeShortcut::RemoteHosts) {
            self.remote_picker = None;
            return Some(Task::none());
        }
        let count = self.config.remote_hosts.len();
        let selected = self.remote_picker.as_mut()?;
        match key {
            Key::Named(Named::Escape) => {
                self.remote_picker = None;
                Some(Task::none())
            }
            Key::Named(Named::Enter) => {
                let index = *selected;
                self.remote_picker = None;
                if index < count {
                    self.connect_remote_host(index);
                }
                Some(Task::none())
            }
            Key::Named(Named::ArrowDown) if count > 0 => {
                *selected = (*selected + 1) % count;
                Some(Task::none())
            }
            Key::Named(Named::ArrowUp) if count > 0 => {
                *selected = if *selected == 0 {
                    count - 1
                } else {
                    *selected - 1
                };
                Some(Task::none())
            }
            _ => Some(Task::none()),
        }
    }

    /// Open a `[[remote_hosts]]` destination in its own session. The argv is
    /// the family-shared builder's: the deploy launcher when the entry asks
    /// for it — lending the local jsh when that one is static — and a plain
    /// ssh / `docker exec` otherwise.
    fn connect_remote_host(&mut self, index: usize) {
        let Some(host) = self.config.remote_hosts.get(index).cloned() else {
            return;
        };
        if let Err(problem) = host.validate() {
            self.push_toast(
                format!("Remote host {}: {problem}", host.display_name()),
                ToastKind::Warning,
            );
            return;
        }
        let (argv, degraded) = host.tab_argv();
        if let Some(error) = degraded {
            // The tab still opens — a plain connection beats no connection —
            // but quietly pretending jsh was deployed would be worse than
            // either.
            log::warn!("cannot publish jsh-remote.sh: {error}; connecting without deployment");
            self.push_toast(
                format!(
                    "Deploy unavailable; connecting to {} plainly",
                    host.display_name()
                ),
                ToastKind::Warning,
            );
        }
        match Session::spawn_argv(
            &self.config,
            self.next_id,
            self.cols,
            self.rows,
            None,
            Some(&argv),
        ) {
            Ok(session) => {
                self.session_diagnostic = None;
                self.next_id += 1;
                let insert = (self.active + 1).min(self.sessions.len());
                self.sessions.insert(insert, session);
                self.reindex_tabs_after_insert(insert);
                // A remote session opens its own tab; it is never grafted into
                // the current tab's split.
                self.open_tab_with(insert);
                self.relayout();
                self.refresh_active_context();
                self.save_session_snapshot();
            }
            Err(error) => {
                let message = error.to_string();
                log::error!("[PTY] {message}");
                self.push_toast(message, ToastKind::Warning);
            }
        }
    }

    /// Tab switcher key handling. Mirrors `handle_palette_key`: filters by
    /// typed text, arrows move selection, Enter jumps, Esc closes.
    fn handle_tab_switcher_key(
        &mut self,
        key: &keyboard::Key,
        mods: keyboard::Modifiers,
        text: Option<&str>,
    ) -> Option<Task<Message>> {
        use keyboard::key::Named;
        use keyboard::Key;
        if chrome_shortcut(key, mods) == Some(ChromeShortcut::TabSwitcher) {
            self.tab_switcher = None;
            return Some(Task::none());
        }
        let labels = self.tab_labels();
        let state = self.tab_switcher.as_mut()?;
        // Recompute the visible order once so Enter/arrows agree with what's drawn.
        let filtered = tab_switcher_filtered(&labels, &state.query);
        match key {
            Key::Named(Named::Escape) => {
                self.tab_switcher = None;
                return Some(Task::none());
            }
            Key::Named(Named::Enter) => {
                let target = filtered.get(state.selected).map(|&(_, i)| i);
                self.tab_switcher = None;
                if let Some(i) = target {
                    if i < self.sessions.len() && i != self.active {
                        self.activate_session(i);
                    }
                }
                return Some(Task::none());
            }
            Key::Named(Named::ArrowDown) => {
                if !filtered.is_empty() {
                    state.selected = (state.selected + 1) % filtered.len();
                }
                return Some(Task::none());
            }
            Key::Named(Named::ArrowUp) => {
                if !filtered.is_empty() {
                    state.selected = if state.selected == 0 {
                        filtered.len() - 1
                    } else {
                        state.selected - 1
                    };
                }
                return Some(Task::none());
            }
            Key::Named(Named::Backspace) => {
                state.query.pop();
                state.selected = 0;
                return Some(Task::none());
            }
            _ => {}
        }
        if !mods.control() && !mods.alt() {
            if let Some(t) = text {
                let printable: String = t.chars().filter(|c| !c.is_control()).collect();
                if !printable.is_empty() {
                    state.query.push_str(&printable);
                    state.selected = 0;
                    return Some(Task::none());
                }
            }
        }
        // Swallow all other keys while the overlay owns the keyboard.
        Some(Task::none())
    }

    /// Open the history picker over a bounded recent slice of the persisted
    /// command index. Shared by Ctrl+Shift+H and the command palette so both
    /// entry points behave identically, including the disabled-history hint.
    fn open_history_picker(&mut self) -> Task<Message> {
        let Some(path) = self.config.resolved_command_history_path() else {
            self.push_toast(
                "Command history is disabled (command_history_enabled = false)",
                ToastKind::Info,
            );
            return Task::none();
        };
        self.history_picker = Some(history_picker::HistoryPickerState::load(&path));
        iced::widget::operation::focus(HISTORY_PICKER_INPUT_ID.clone())
    }

    /// One gate for commands that act on block history. Block chrome is hidden
    /// in the alternate screen, so neither keybindings nor palette actions may
    /// mutate or read an invisible selection there.
    fn block_action_available(&self) -> bool {
        self.config.block_mode
            && self
                .sessions
                .get(self.active)
                .is_some_and(|session| !session.terminal.is_alt_buffer_active())
    }

    /// Physical chords defer to a foreground command even on the primary
    /// screen. Explicit palette/menu clicks may still inspect history while a
    /// command runs, but a terminal key must never be stolen from that app.
    fn block_binding_available(&self) -> bool {
        self.block_action_available()
            && self
                .sessions
                .get(self.active)
                .is_some_and(|session| !session.terminal.is_command_running())
    }

    fn ensure_block_action_available(&mut self, action: &str) -> bool {
        if !self.config.block_mode {
            self.push_toast(
                format!("{action} unavailable: block mode is disabled"),
                ToastKind::Info,
            );
            return false;
        }
        if !self.block_action_available() {
            self.push_toast(
                format!("{action} unavailable while a full-screen program is active"),
                ToastKind::Info,
            );
            return false;
        }
        true
    }

    /// Reveal one edge of a finalized block without changing the current
    /// multi-selection. The bottom edge is the last row before the next block
    /// or live prompt, matching the same span used by gutter paint/hit-tests.
    fn block_reveal_edge(&mut self, zone_id: u64, bottom: bool) -> Task<Message> {
        if !self.ensure_block_action_available("Block navigation") {
            return Task::none();
        }
        let target = self.sessions.get(self.active).and_then(|sess| {
            let terminal = &sess.terminal;
            let total = terminal.scrollback_len() + terminal.grid.rows();
            let live_boundary = terminal
                .running_zone_start()
                .or(terminal.live_prompt_row())
                .unwrap_or(total);
            let zones: Vec<&terminal::CommandZone> = terminal
                .command_zones
                .iter()
                .filter(|zone| !zone.rows_evicted)
                .collect();
            let starts: Vec<usize> = zones.iter().map(|zone| zone.prompt_start).collect();
            zones
                .iter()
                .zip(block_mode::spans(&starts, live_boundary))
                .find(|(zone, _)| zone.id == zone_id)
                .map(|(zone, (start, end))| {
                    if bottom {
                        end.saturating_sub(1).max(start)
                    } else {
                        zone.prompt_start
                    }
                })
        });
        let Some(target) = target else {
            self.push_toast(
                "Block rows are no longer retained".to_string(),
                ToastKind::Info,
            );
            return Task::none();
        };
        if let Some(sess) = self.sessions.get_mut(self.active) {
            let revealed = if bottom && sess.projection.effective_collapsed().contains(&zone_id) {
                let revealed = sess.terminal.reveal_collapsed_summary(
                    &sess.projection_policy,
                    &mut sess.projection_view_state,
                    zone_id,
                );
                if revealed {
                    sess.refresh();
                }
                revealed
            } else {
                sess.reveal_absolute_cell(target, 0)
            };
            if !revealed {
                self.push_toast(
                    "Block position is no longer available".to_string(),
                    ToastKind::Info,
                );
            }
        }
        Task::none()
    }

    fn block_toggle_bookmark(&mut self, zone_id: u64) -> Task<Message> {
        if !self.ensure_block_action_available("Block bookmark") {
            return Task::none();
        }
        let Some(sess) = self.sessions.get_mut(self.active) else {
            return Task::none();
        };
        let ids: Vec<u64> = sess
            .terminal
            .command_zones
            .iter()
            .map(|zone| zone.id)
            .collect();
        sess.block_bookmarks.retain(&ids);
        if !ids.contains(&zone_id) {
            self.push_toast("Block is no longer retained".to_string(), ToastKind::Info);
            return Task::none();
        }
        let bookmarked = sess.block_bookmarks.toggle(zone_id);
        sess.refresh();
        self.push_toast(
            if bookmarked {
                "Bookmarked block"
            } else {
                "Removed block bookmark"
            },
            ToastKind::Success,
        );
        Task::none()
    }

    fn block_toggle_target_bookmark(&mut self) -> Task<Message> {
        if !self.ensure_block_action_available("Block bookmark") {
            return Task::none();
        }
        let target = self.sessions.get_mut(self.active).and_then(|sess| {
            let ids: Vec<u64> = sess
                .terminal
                .command_zones
                .iter()
                .map(|zone| zone.id)
                .collect();
            sess.block_selection.retain(&ids);
            active_bookmark_target(&ids, sess.block_selection.active())
        });
        let Some(id) = target else {
            self.push_toast(
                "Select a block to toggle its bookmark".to_string(),
                ToastKind::Info,
            );
            return Task::none();
        };
        self.block_toggle_bookmark(id)
    }

    fn block_jump_bookmark(&mut self, older: bool) -> Task<Message> {
        if !self.ensure_block_action_available("Block bookmark navigation") {
            return Task::none();
        }
        let target = {
            let Some(sess) = self.sessions.get_mut(self.active) else {
                return Task::none();
            };
            let ids: Vec<u64> = sess
                .terminal
                .command_zones
                .iter()
                .map(|zone| zone.id)
                .collect();
            let current = sess.block_selection.active();
            sess.block_bookmarks.neighbor(&ids, current, older)
        };
        match target {
            Some(id) => self.select_and_reveal_block(id),
            None => self.push_toast("No bookmarked command blocks".to_string(), ToastKind::Info),
        }
        Task::none()
    }

    /// Dispatch a context-menu action against the stable pane/zone captured by
    /// the right-click. Focus changes and bounded-history eviction fail closed
    /// instead of silently applying the action to another pane's same id.
    fn execute_block_menu_action(&mut self, action: BlockMenuAction) -> Task<Message> {
        let Some(menu) = self.block_menu.take() else {
            return Task::none();
        };
        // Fix/Explain/Retry look the source session up by its stable id rather
        // than through the focused pane: the Agent task or replay stays bound
        // to the terminal that produced the block even when focus moved after
        // the menu opened.
        match action {
            BlockMenuAction::FixWithAgent => {
                return self.failed_block_agent_task(
                    menu.session_id,
                    menu.zone_id,
                    FailedBlockAgentIntent::Fix,
                );
            }
            BlockMenuAction::ExplainWithAgent => {
                return self.failed_block_agent_task(
                    menu.session_id,
                    menu.zone_id,
                    FailedBlockAgentIntent::Explain,
                );
            }
            BlockMenuAction::Retry => {
                return self.failed_block_retry_task(menu.session_id, menu.zone_id);
            }
            BlockMenuAction::CreateTask => {
                self.task_create_from_block(menu.session_id, menu.zone_id);
                return Task::none();
            }
            _ => {}
        }
        let target_is_live = self.sessions.get(self.active).is_some_and(|sess| {
            sess.id == menu.session_id && sess.terminal.zone_by_id(menu.zone_id).is_some()
        });
        if !target_is_live {
            self.push_toast(
                "Block menu target is no longer available".to_string(),
                ToastKind::Info,
            );
            return Task::none();
        }
        match action {
            BlockMenuAction::CopyCommand => self.block_copy_command_task(),
            BlockMenuAction::AskAi => self.block_ask_ai_task(menu.zone_id),
            BlockMenuAction::CopyOutput => self.block_copy_output_task(),
            BlockMenuAction::CopyBlock => self.block_copy_block_task(),
            BlockMenuAction::CopyMarkdown => self.block_copy_markdown_task(),
            BlockMenuAction::RecallCommand => {
                if let Some(sess) = self.sessions.get_mut(self.active) {
                    sess.block_selection.replace(Some(menu.zone_id));
                }
                self.block_recall_command_task()
            }
            BlockMenuAction::ReinputSelected => self.block_reinput_selected_commands_task(),
            BlockMenuAction::ToggleBookmark => self.block_toggle_bookmark(menu.zone_id),
            BlockMenuAction::JumpTop => self.block_reveal_edge(menu.zone_id, false),
            BlockMenuAction::JumpBottom => self.block_reveal_edge(menu.zone_id, true),
            BlockMenuAction::Search => self.toggle_block_search(),
            BlockMenuAction::ExportMarkdown => self
                .block_export_zone_task(menu.zone_id, block_export::SessionExportFormat::Markdown),
            BlockMenuAction::ExportJson => {
                self.block_export_zone_task(menu.zone_id, block_export::SessionExportFormat::Json)
            }
            BlockMenuAction::CollapseOutput => {
                let changed = self.sessions.get_mut(self.active).is_some_and(|sess| {
                    sess.id == menu.session_id
                        && !sess.projection_policy.is_collapsed(menu.zone_id)
                        && sess.terminal.finished_output_range(menu.zone_id).is_some()
                        && sess.projection_policy.collapse(menu.zone_id)
                });
                if changed {
                    if let Some(sess) = self.sessions.get_mut(self.active) {
                        sess.refresh();
                    }
                    self.refresh_active_context();
                }
                Task::none()
            }
            BlockMenuAction::ExpandOutput => {
                let changed = self.sessions.get_mut(self.active).is_some_and(|sess| {
                    sess.id == menu.session_id
                        // Requested state remains user-recoverable even when
                        // stale/overlapping provenance made it ineffective.
                        && sess.projection_policy.is_collapsed(menu.zone_id)
                        && sess.projection_policy.expand(menu.zone_id)
                });
                if changed {
                    if let Some(sess) = self.sessions.get_mut(self.active) {
                        sess.refresh();
                    }
                    self.refresh_active_context();
                }
                Task::none()
            }
            BlockMenuAction::Clear => self.request_block_clear(),
            // Returned by the stable-id dispatch above, before the focus-bound
            // liveness check these remaining actions require.
            BlockMenuAction::FixWithAgent
            | BlockMenuAction::ExplainWithAgent
            | BlockMenuAction::CreateTask
            | BlockMenuAction::Retry => Task::none(),
        }
    }

    /// Attach one stable, right-clicked block to the existing Agent panel.
    /// Output is bounded twice: terminal export never exceeds 1 MiB, then the
    /// shared AI helper retains only the useful head/tail line window.
    fn block_ask_ai_task(&mut self, id: u64) -> Task<Message> {
        let Some(sess) = self.sessions.get(self.active) else {
            return Task::none();
        };
        let Some(zone) = sess.terminal.zone_by_id(id) else {
            self.push_toast("Block no longer available".to_string(), ToastKind::Info);
            return Task::none();
        };
        let (output, output_truncated) = match sess.terminal.zone_output_export_capped(id) {
            Some(terminal::ZoneOutputExport::Available { text, truncated }) => (text, truncated),
            Some(terminal::ZoneOutputExport::Empty) => (String::new(), false),
            Some(terminal::ZoneOutputExport::Unavailable) => (
                "[output unavailable: retained snapshot and scrollback rows were evicted]"
                    .to_string(),
                true,
            ),
            None => {
                self.push_toast("Block no longer available".to_string(), ToastKind::Info);
                return Task::none();
            }
        };
        let no_reported_status = zone.exit_code.is_none();
        let (ai_output, ai_output_truncated) = bounded_ai_block_output(&output, no_reported_status);
        let context = jterm_core::ai::BlockContext {
            cmd: zone.command.clone().unwrap_or_default(),
            output: ai_output,
            cwd: zone.cwd.clone(),
            // BlockContext currently has no Option status. -1 is the family
            // sentinel for "the shell reported no exit status"; never turn an
            // unknown/background lifecycle into a false success.
            exit_code: zone.exit_code.unwrap_or(-1),
            truncated: zone.command_truncated || output_truncated || ai_output_truncated,
        };
        let session_id = sess.id;
        if !self.agent.is_open || self.agent.bound_session_id != Some(session_id) {
            self.agent.open(&self.config, session_id);
        }
        self.agent.last_manual_completed = Some(context);
        iced::widget::operation::focus(AGENT_INPUT_ID.clone())
    }

    /// Start a fresh Agent task for one failed block (ember's Fix/Explain,
    /// adapted to frost's per-command-approval Shell Agent). The source
    /// session is found by stable id — not by focus — so the task stays bound
    /// to the terminal that produced the block. Every guard fails closed with
    /// a toast and leaves any live Agent task untouched.
    fn failed_block_agent_task(
        &mut self,
        session_id: usize,
        zone_id: u64,
        intent: FailedBlockAgentIntent,
    ) -> Task<Message> {
        let prepared = self
            .sessions
            .iter()
            .find(|sess| sess.id == session_id)
            .map(|sess| {
                let Some(zone) = sess.terminal.zone_by_id(zone_id) else {
                    return Err("Block menu target is no longer available".to_string());
                };
                if !matches!(
                    block_mode::classify(zone.command.as_deref(), zone.exit_code),
                    block_mode::BlockOutcome::Failed(_)
                ) {
                    return Err(
                        "Fix/Explain are available for failed command blocks".to_string()
                    );
                }
                if let Some(reason) = block_mode::failed_block_agent_disabled_reason(
                    zone.command.as_deref(),
                    zone.command_truncated,
                    zone.cwd.as_deref(),
                ) {
                    return Err(format!("Cannot start an Agent task: {reason}"));
                }
                // cwd provenance: the block's recorded cwd is self-reported
                // OSC 133 data, so it must agree with an independent local
                // observation of the shell process before it can anchor a
                // task. SSH/tmux-style wrappers fail closed here.
                let recorded = zone.cwd.clone().expect("eligibility guarantees a cwd");
                let reported = sess.terminal.current_working_dir().map(str::to_string);
                let process = jterm_core::process::process_cwd(sess.pty.get_child_pid());
                if !block_mode::verified_local_command_cwd(
                    &recorded,
                    reported.as_deref(),
                    process.as_deref(),
                ) {
                    return Err(
                        "Cannot start an Agent task: the recorded cwd is not independently verified; return a local shell to the command's directory first"
                            .to_string(),
                    );
                }
                let (output, output_truncated) =
                    match sess.terminal.zone_output_export_capped(zone_id) {
                        Some(terminal::ZoneOutputExport::Available { text, truncated }) => {
                            (text, truncated)
                        }
                        Some(terminal::ZoneOutputExport::Empty) => (String::new(), false),
                        Some(terminal::ZoneOutputExport::Unavailable) => (
                            "[output unavailable: retained snapshot and scrollback rows were evicted]"
                                .to_string(),
                            true,
                        ),
                        None => {
                            return Err("Block menu target is no longer available".to_string())
                        }
                    };
                // A failed block always has a reported status.
                let (ai_output, ai_output_truncated) = bounded_ai_block_output(&output, false);
                Ok(jterm_core::ai::BlockContext {
                    cmd: zone
                        .command
                        .clone()
                        .expect("eligibility guarantees a command"),
                    output: ai_output,
                    cwd: Some(recorded),
                    exit_code: zone.exit_code.unwrap_or(-1),
                    truncated: output_truncated || ai_output_truncated,
                })
            });
        let context = match prepared {
            Some(Ok(context)) => context,
            Some(Err(message)) => {
                self.push_toast(message, ToastKind::Warning);
                return Task::none();
            }
            None => {
                self.push_toast(
                    "Block menu target is no longer available".to_string(),
                    ToastKind::Info,
                );
                return Task::none();
            }
        };
        // The instruction never interpolates command or output text: both are
        // untrusted PTY evidence and travel only inside the framed context.
        let prompt = match intent {
            FailedBlockAgentIntent::Fix => "Fix the attached failed command. Diagnose the root cause from its captured output and propose the smallest safe fix; every command you propose is reviewed before it runs.",
            FailedBlockAgentIntent::Explain => "Explain the attached failed command: identify the root cause, cite the relevant evidence in its captured output, and propose the smallest safe next step. Do not propose changes unless I ask.",
        };
        match self
            .agent
            .start_for_block(&self.config, session_id, context, prompt)
        {
            Ok(()) => {
                self.push_toast(
                    match intent {
                        FailedBlockAgentIntent::Fix => "Agent is working on the failed command",
                        FailedBlockAgentIntent::Explain => "Agent is explaining the failed command",
                    },
                    ToastKind::Success,
                );
                self.agent_drive_task().unwrap_or_else(Task::none)
            }
            Err(message) => {
                self.push_toast(message, ToastKind::Warning);
                Task::none()
            }
        }
    }

    /// Guarded semantic replay of one failed block's exact command into its
    /// source pane (ember's Retry). Nothing is written unless the command is
    /// exact, non-truncated, single-line and safe, its recorded cwd matches an
    /// independently observed local shell cwd, and the pane sits at an idle,
    /// empty, bracketed-paste prompt on the main screen. Every refusal fails
    /// closed with a toast.
    fn failed_block_retry_task(&mut self, session_id: usize, zone_id: u64) -> Task<Message> {
        let Some(index) = self.sessions.iter().position(|sess| sess.id == session_id) else {
            self.push_toast(
                "Block menu target is no longer available".to_string(),
                ToastKind::Info,
            );
            return Task::none();
        };
        let prepared: Result<String, String> = {
            let sess = &mut self.sessions[index];
            (|| {
                let Some(zone) = sess.terminal.zone_by_id(zone_id) else {
                    return Err("Block menu target is no longer available".to_string());
                };
                if !matches!(
                    block_mode::classify(zone.command.as_deref(), zone.exit_code),
                    block_mode::BlockOutcome::Failed(_)
                ) {
                    return Err("Retry is available for failed command blocks".to_string());
                }
                let command = zone.command.clone();
                let command_truncated = zone.command_truncated;
                let recorded = zone.cwd.clone();
                let reported = sess.terminal.current_working_dir().map(str::to_string);
                let process = jterm_core::process::process_cwd(sess.pty.get_child_pid());
                if let Some(reason) = block_mode::retry_replay_disabled_reason(
                    command.as_deref(),
                    command_truncated,
                    recorded.as_deref(),
                    reported.as_deref(),
                    process.as_deref(),
                ) {
                    return Err(format!("Command not retried: {reason}"));
                }
                if sess.terminal.is_alt_buffer_active() {
                    return Err("Command not retried: a full-screen program is active".to_string());
                }
                if let Some(reason) = block_prompt_replace_blocker(sess.agent_prompt_status()) {
                    return Err(format!("Command not retried: {reason}"));
                }
                if !sess.terminal.is_bracketed_paste_enabled() {
                    return Err(
                        "Command not retried: safe replay requires bracketed paste mode"
                            .to_string(),
                    );
                }
                let command = command.expect("eligibility guarantees a command");
                let command = command.trim_end_matches(['\r', '\n']).to_string();
                // Byte-exact replay: any control or visual spoof rejects the
                // whole command instead of altering what will run.
                crate::review_text::validate_single_line(
                    &command,
                    block_mode::FAILED_BLOCK_COMMAND_MAX_BYTES,
                )
                .map_err(|error| format!("Command not retried: {error}"))?;
                Ok(command)
            })()
        };
        let command = match prepared {
            Ok(command) => command,
            Err(message) => {
                self.push_toast(message, ToastKind::Warning);
                return Task::none();
            }
        };
        // Final prompt-ownership re-check at the write boundary, same as
        // reinput: the prompt that was safe above may have changed since.
        if let Err(reason) = self.session_prompt_replace_ready(session_id) {
            self.push_toast(format!("Command not retried: {reason}"), ToastKind::Warning);
            return Task::none();
        }
        if self.write_paste_to_session(session_id, &command, block_retry_policy(), true) {
            self.push_toast("Command queued to run", ToastKind::Success);
        }
        Task::none()
    }

    /// The active pane's target for the palette's failed-block actions: the
    /// selected block when it is a failed one, otherwise the newest failed
    /// block (the family's "selected (or latest)" rule).
    fn palette_failed_block_target(&mut self) -> Option<(usize, u64)> {
        let sess = self.sessions.get_mut(self.active)?;
        let ids: Vec<u64> = sess
            .terminal
            .command_zones
            .iter()
            .map(|zone| zone.id)
            .collect();
        sess.block_selection.retain(&ids);
        let is_failed = |zone: &&terminal::CommandZone| {
            matches!(
                block_mode::classify(zone.command.as_deref(), zone.exit_code),
                block_mode::BlockOutcome::Failed(_)
            )
        };
        let target = sess
            .block_selection
            .active()
            .filter(|id| {
                sess.terminal
                    .zone_by_id(*id)
                    .is_some_and(|zone| is_failed(&zone))
            })
            .or_else(|| {
                sess.terminal
                    .command_zones
                    .iter()
                    .rev()
                    .find(is_failed)
                    .map(|zone| zone.id)
            })?;
        Some((sess.id, target))
    }

    /// Palette entry point for Fix/Explain on the active pane's failed block.
    fn palette_failed_block_agent_task(&mut self, intent: FailedBlockAgentIntent) -> Task<Message> {
        match self.palette_failed_block_target() {
            Some((session_id, zone_id)) => {
                self.failed_block_agent_task(session_id, zone_id, intent)
            }
            None => {
                self.push_toast(
                    "No failed command block (needs OSC 133 shell integration)".to_string(),
                    ToastKind::Info,
                );
                Task::none()
            }
        }
    }

    /// Palette entry point for Retry on the active pane's failed block.
    fn palette_failed_block_retry_task(&mut self) -> Task<Message> {
        match self.palette_failed_block_target() {
            Some((session_id, zone_id)) => self.failed_block_retry_task(session_id, zone_id),
            None => {
                self.push_toast(
                    "No failed command block (needs OSC 133 shell integration)".to_string(),
                    ToastKind::Info,
                );
                Task::none()
            }
        }
    }

    /// Toggle the cross-block search picker (`block:search`). While it is
    /// open its own key handler owns the chord, so dispatch normally only
    /// sees the closed state — the guard here covers the palette entry.
    /// Opening snapshots at most 8 MiB of newest source text, then lowercases
    /// it under a 16 MiB resident-cache budget on a worker. Per-keystroke
    /// searches remain synchronous but only rescan that bounded cache.
    fn toggle_block_search(&mut self) -> Task<Message> {
        if self.block_search.is_some() {
            self.block_search = None;
            return Task::none();
        }
        if !self.ensure_block_action_available("Block search") {
            return Task::none();
        }
        let Some(session_id) = self.sessions.get(self.active).map(|sess| sess.id) else {
            return Task::none();
        };
        self.block_search = Some(BlockSearchState {
            session_id,
            ..BlockSearchState::default()
        });
        let build = self.begin_block_search_rebuild();
        Task::batch(vec![
            iced::widget::operation::focus(BLOCK_SEARCH_INPUT_ID.clone()),
            build,
        ])
    }

    /// Snapshot the newest source zones without exceeding the UI-thread
    /// extraction budget. The iterator is lazy: after the first over-budget
    /// zone, older live rows are not rendered into throwaway Strings.
    fn block_search_source_snapshot(sess: &Session) -> block_mode::BlockSearchSourceSnapshot {
        block_mode::bounded_block_search_sources(
            sess.terminal.command_zones.iter().rev().map(|zone| {
                block_mode::BlockSearchSource::new(
                    zone.id,
                    zone.command.clone(),
                    sess.terminal.zone_output_text(zone.id),
                )
            }),
            block_mode::BLOCK_SEARCH_SOURCE_MAX_BYTES,
        )
    }

    /// Start (or restart) one bounded cache build. Clearing the prior cache
    /// before source extraction prevents a live refresh from retaining two
    /// multi-megabyte indexes at once. Query/filter text survives and is
    /// evaluated when the matching build result arrives.
    fn begin_block_search_rebuild(&mut self) -> Task<Message> {
        let Some(session_id) = self.block_search.as_ref().map(|state| state.session_id) else {
            return Task::none();
        };
        let Some(version) = self
            .sessions
            .get(self.active)
            .filter(|sess| sess.id == session_id)
            .map(|sess| BlockSearchZoneVersion::from_terminal(&sess.terminal))
        else {
            self.block_search = None;
            return Task::none();
        };
        let Some(epoch) = next_block_search_epoch(&mut self.next_block_search_epoch) else {
            log::error!("block-search epoch space exhausted; closing picker");
            self.block_search = None;
            return Task::none();
        };
        let identity = BlockSearchBuildIdentity { session_id, epoch };
        if let Some(state) = self.block_search.as_mut() {
            state.epoch = epoch;
            state.loading = true;
            state.older_not_indexed = false;
            state.zone_version = version;
            state.cache.clear();
            state.hits.clear();
            state.capped = false;
            state.selected = 0;
        }
        let Some(snapshot) = self
            .sessions
            .get(self.active)
            .filter(|sess| sess.id == session_id)
            .map(Self::block_search_source_snapshot)
        else {
            self.block_search = None;
            return Task::none();
        };

        Task::perform(
            async move {
                tokio::task::spawn_blocking(move || {
                    block_mode::build_block_search_cache(
                        snapshot,
                        block_mode::BLOCK_SEARCH_CACHE_MAX_BYTES,
                    )
                })
                .await
                .map_err(|error| format!("block-search cache worker failed: {error}"))
            },
            move |result| Message::BlockSearchCacheBuilt(identity, result),
        )
    }

    /// Close the block search picker because the session landscape changed
    /// under it: the active session switched (tab/pane focus) or a session
    /// closed. Its hits and cache were computed against ONE session, zone
    /// ids restart at 0 per session, and session indices shift on close —
    /// resolving a held hit afterwards could silently select a block of the
    /// WRONG session. Dropping the whole state (query, hits, cache) is the
    /// reset; a closed picker cannot hold stale hits.
    fn close_block_search_on_session_change(&mut self) {
        self.block_search = None;
        self.block_menu = None;
        self.block_clear_confirm = None;
    }

    /// Re-run the block search over the bounded cache with the current query.
    /// Never re-extracts terminal output; while a worker is loading, edits are
    /// retained and the matching completed build performs the recompute.
    fn block_search_recompute(&mut self) {
        let Some(open) = self.block_search.as_ref() else {
            return;
        };
        if open.loading {
            return;
        }
        let filter = open.filter;
        let slow_threshold_ms = self.config.notify_long_block_threshold_ms;
        let eligible: std::collections::HashSet<u64> = self
            .sessions
            .get(self.active)
            .map(|sess| {
                sess.terminal
                    .command_zones
                    .iter()
                    .filter(|zone| match filter {
                        BlockSearchFilter::All => true,
                        BlockSearchFilter::Failed => matches!(
                            block_mode::classify(zone.command.as_deref(), zone.exit_code),
                            block_mode::BlockOutcome::Failed(_)
                        ),
                        BlockSearchFilter::Slow => zone
                            .duration_ms
                            .is_some_and(|duration| duration >= slow_threshold_ms),
                        BlockSearchFilter::Bookmarked => sess.block_bookmarks.contains(zone.id),
                        BlockSearchFilter::Background => matches!(
                            block_mode::classify(zone.command.as_deref(), zone.exit_code),
                            block_mode::BlockOutcome::Background
                        ),
                    })
                    .map(|zone| zone.id)
                    .collect()
            })
            .unwrap_or_default();
        let Some(state) = self.block_search.as_mut() else {
            return;
        };
        let results = if state.query.trim().is_empty() && filter != BlockSearchFilter::All {
            let mut hits = Vec::new();
            let mut capped = false;
            for zone in state
                .cache
                .iter()
                .rev()
                .filter(|zone| eligible.contains(&zone.zone_id))
            {
                if hits.len() >= block_mode::BLOCK_SEARCH_HIT_CAP {
                    capped = true;
                    break;
                }
                let (line_text, is_output_line, line_no) = if let Some(command) = &zone.command {
                    (command.clone(), false, 0)
                } else if let Some(line) =
                    zone.output.as_deref().and_then(|text| text.lines().next())
                {
                    (line.to_string(), true, 1)
                } else {
                    ("Background output".to_string(), false, 0)
                };
                let clip = |text: &str, cap: usize| {
                    if text.chars().count() <= cap {
                        text.to_string()
                    } else {
                        let mut clipped: String =
                            text.chars().take(cap.saturating_sub(1)).collect();
                        clipped.push('…');
                        clipped
                    }
                };
                hits.push(block_mode::BlockSearchHit {
                    zone_id: zone.zone_id,
                    is_output_line,
                    line_no,
                    match_span: None,
                    line_text: clip(&line_text, block_mode::BLOCK_SEARCH_LINE_CHARS),
                    command_preview: clip(
                        zone.command.as_deref().unwrap_or_default(),
                        block_mode::BLOCK_SEARCH_COMMAND_CHARS,
                    ),
                });
            }
            block_mode::BlockSearchResults { hits, capped }
        } else if filter == BlockSearchFilter::All {
            block_mode::search_blocks(&state.cache, &state.query)
        } else {
            block_mode::search_blocks_filtered(&state.cache, &state.query, |zone_id| {
                eligible.contains(&zone_id)
            })
        };
        state.hits = results.hits;
        state.capped = results.capped;
        state.selected = 0;
    }

    /// Keep the highlighted hit visible in the picker's scrollable list.
    /// Rows are uniform, so snapping to `selected / (len - 1)` places the
    /// selected row at that same fraction of the viewport — always inside
    /// it, top row at the top, bottom row at the bottom.
    fn block_search_snap_task(&self) -> Task<Message> {
        let Some(state) = self.block_search.as_ref() else {
            return Task::none();
        };
        let y = match state.hits.len() {
            0 | 1 => 0.0,
            len => state.selected as f32 / (len - 1) as f32,
        };
        iced::widget::operation::snap_to(
            BLOCK_SEARCH_LIST_ID.clone(),
            iced::widget::operation::RelativeOffset { x: 0.0, y },
        )
    }

    fn block_search_target_is_live(&self, session_id: usize, zone_id: u64) -> bool {
        self.sessions.get(self.active).is_some_and(|session| {
            session.id == session_id && session.terminal.zone_by_id(zone_id).is_some()
        })
    }

    /// Block search picker key handling, mirroring
    /// [`Self::handle_history_picker_key`]: typed text edits the query,
    /// arrows move the selection, Enter selects and reveals the highlighted
    /// hit's zone, Esc (or the block:search chord) dismisses. Runs BEFORE the
    /// keybinding/`encode_key` layers, which is what keeps the picker's own
    /// keystrokes from reaching the PTY and clearing the block selection.
    fn handle_block_search_key(
        &mut self,
        key: &keyboard::Key,
        mods: keyboard::Modifiers,
        text: Option<&str>,
    ) -> Option<Task<Message>> {
        use keyboard::key::Named;
        use keyboard::Key;
        if !self.config.block_mode
            || self
                .sessions
                .get(self.active)
                .is_some_and(|sess| sess.terminal.is_alt_buffer_active())
        {
            // The overlay may have been open when asynchronous PTY output
            // entered the alternate screen. Drop it and let this very key
            // continue to the foreground application.
            self.block_search = None;
            return None;
        }
        // The (configurable) toggle chord closes the picker from inside.
        if key_to_binding_string(key, mods)
            .and_then(|binding| self.keybindings.get_command(&binding))
            == Some(keybindings::Command::BlockSearch)
        {
            self.block_search = None;
            return Some(Task::none());
        }
        let state = self.block_search.as_mut()?;
        match key {
            Key::Named(Named::Escape) => {
                self.block_search = None;
                return Some(Task::none());
            }
            Key::Named(Named::Enter) => {
                let owner = state.session_id;
                let target = state.hits.get(state.selected).cloned();
                self.block_search = None;
                if let Some(hit) = target {
                    if self.block_search_target_is_live(owner, hit.zone_id) {
                        self.reveal_block_search_hit(&hit);
                    }
                }
                return Some(Task::none());
            }
            Key::Named(Named::ArrowDown) => {
                state.select_next();
                return Some(self.block_search_snap_task());
            }
            Key::Named(Named::ArrowUp) => {
                state.select_prev();
                return Some(self.block_search_snap_task());
            }
            Key::Named(Named::Backspace) => {
                state.query.pop();
                self.block_search_recompute();
                return Some(self.block_search_snap_task());
            }
            _ => {}
        }
        if !mods.control() && !mods.alt() {
            if let Some(t) = text {
                let printable: String = t.chars().filter(|c| !c.is_control()).collect();
                if !printable.is_empty() {
                    state.query.push_str(&printable);
                    self.block_search_recompute();
                    return Some(self.block_search_snap_task());
                }
            }
        }
        // Swallow all other keys while the overlay owns the keyboard.
        Some(Task::none())
    }

    /// Paste-to-prompt: queue `text` into the active pane with the same framing,
    /// input-queue bounds and rejection toast as a clipboard paste, and never
    /// append Enter — the user still submits explicitly. The text is appended
    /// to the pending line; command recall goes through
    /// [`Self::recall_into_active_pane`] instead.
    fn type_into_active_pane(&mut self, text: String) -> Task<Message> {
        let Some(id) = self.sessions.get(self.active).map(|session| session.id) else {
            return Task::none();
        };
        Task::done(Message::PromptInsert(id, text))
    }

    /// History recall: replace the prompt's pending line with `command`. Still
    /// never appends Enter — the user submits explicitly.
    fn recall_into_active_pane(&mut self, command: String) -> Task<Message> {
        let Some(id) = self.sessions.get(self.active).map(|session| session.id) else {
            return Task::none();
        };
        Task::done(Message::PromptRecall(id, command))
    }

    /// Re-check prompt ownership at the final PTY-write boundary. A recall is
    /// delivered through `Task::done`, so the prompt that was safe when the
    /// task was created may have received input or entered a foreground app by
    /// the next update.
    fn session_prompt_replace_ready(&mut self, id: usize) -> Result<(), &'static str> {
        let sess = self
            .sessions
            .iter_mut()
            .find(|session| session.id == id)
            .ok_or("the terminal session is no longer available")?;
        if sess.terminal.is_alt_buffer_active() {
            return Err("a full-screen program is active");
        }
        match block_prompt_replace_blocker(sess.agent_prompt_status()) {
            Some(reason) => Err(reason),
            None => Ok(()),
        }
    }

    /// Encode one payload for session `id` and queue it on that PTY.
    ///
    /// The single choke point for everything this app writes as a *payload*
    /// rather than a keystroke, and it goes through `pty_input` because that
    /// unconditionally removes `ESC[200~`/`ESC[201~` from the body.
    /// The local encoder this replaced framed the clipboard verbatim, so a
    /// payload carrying its own `ESC[201~` closed the frame early and the shell
    /// read the remainder as typed lines and ran them. Do not add a second path
    /// that frames a payload without coming through here.
    ///
    /// `clear_line_first` prefixes a `Ctrl+U`; with it `false` this is exactly
    /// `encode_paste`. True for command recall, false for pastes and appends.
    fn write_paste_to_session(
        &mut self,
        id: usize,
        text: &str,
        policy: PastePolicy,
        clear_line_first: bool,
    ) -> bool {
        let mut rejected = false;
        let mut written = false;
        let mut dead_input = false;
        if let Some(sess) = self.sessions.iter_mut().find(|session| session.id == id) {
            if sess.transcript_read_only() {
                // Held-open task transcript: pasting is a visible no-op.
                dead_input = true;
            } else {
                let modes = PasteModes {
                    bracketed: sess.terminal.is_bracketed_paste_enabled(),
                };
                let paste = pty_input::encode_prompt_insert(text, modes, policy, clear_line_first);
                // A clipboard that was nothing but paste markers normalizes away
                // entirely; writing zero bytes would toast about a full queue.
                if paste.is_empty() {
                    return false;
                }
                // Size-check the *encoded* bytes: framing and control stripping have
                // already changed the length the queue must accept.
                if !sess.can_queue_user_bytes(paste.bytes.len()) {
                    rejected = true;
                } else {
                    // PTY-bound input dismisses the block selection, same as
                    // `encode_key` and IME commit. Covers every paste flavor
                    // (clipboard, middle-click primary, prompt insert/recall).
                    sess.block_selection.clear();
                    sess.terminal.scroll_to_bottom();
                    sess.projection_view_state.scroll_to_bottom();
                    written = sess.write_pty(&paste.bytes);
                    rejected = !written;
                    sess.refresh();
                }
            }
        }
        if dead_input {
            self.hint_read_only_transcript();
            return false;
        }
        if rejected {
            self.push_toast(
                "Paste rejected: terminal input queue is full",
                ToastKind::Warning,
            );
        }
        written
    }

    /// History picker key handling. Mirrors `handle_tab_switcher_key`: typed
    /// text filters, arrows move the selection, Enter types the highlighted
    /// command into the active pane's prompt, Esc/Ctrl+Shift+H dismisses.
    fn handle_history_picker_key(
        &mut self,
        key: &keyboard::Key,
        mods: keyboard::Modifiers,
        text: Option<&str>,
    ) -> Option<Task<Message>> {
        use keyboard::key::Named;
        use keyboard::Key;
        if chrome_shortcut(key, mods) == Some(ChromeShortcut::HistoryPicker) {
            self.history_picker = None;
            return Some(Task::none());
        }
        let state = self.history_picker.as_mut()?;
        match key {
            Key::Named(Named::Escape) => {
                self.history_picker = None;
                return Some(Task::none());
            }
            Key::Named(Named::Enter) => {
                let command = state.selected_command();
                self.history_picker = None;
                return Some(match command {
                    Some(command) => self.recall_into_active_pane(command),
                    None => Task::none(),
                });
            }
            Key::Named(Named::ArrowDown) => {
                state.select_next();
                return Some(Task::none());
            }
            Key::Named(Named::ArrowUp) => {
                state.select_prev();
                return Some(Task::none());
            }
            Key::Named(Named::Backspace) => {
                state.query.pop();
                state.selected = 0;
                return Some(Task::none());
            }
            _ => {}
        }
        if !mods.control() && !mods.alt() {
            if let Some(t) = text {
                let printable: String = t.chars().filter(|c| !c.is_control()).collect();
                if !printable.is_empty() {
                    state.query.push_str(&printable);
                    state.selected = 0;
                    return Some(Task::none());
                }
            }
        }
        // Swallow all other keys while the overlay owns the keyboard.
        Some(Task::none())
    }

    /// Stable id of the finalized block covering viewport `row` in the pane
    /// identified by `session_id`. Running/live-prompt rows deliberately have
    /// no target. Mouse events retain this stable identity across focus moves.
    fn block_at_viewport_row(&self, session_id: usize, row: usize) -> Option<u64> {
        let sess = self.sessions.iter().find(|sess| sess.id == session_id)?;
        finalized_block_at_viewport_row(
            self.config.block_mode,
            &sess.terminal,
            &sess.projection,
            row,
        )
    }

    fn handle_summary_activation(
        &mut self,
        source_session_id: usize,
        activation: SummaryActivation,
    ) -> Task<Message> {
        let Some(source) = self.session_index_by_id(source_session_id) else {
            return Task::none();
        };
        // A release from a pane that left the visible layout must not mutate a
        // parked tab/session. Stable identity prevents retargeting; visibility
        // is the final ownership check.
        if !self.layout().contains_session(source) {
            return Task::none();
        }
        let Some(sess) = self.sessions.get_mut(source) else {
            return Task::none();
        };
        let Some(zone_id) = validated_summary_target(&sess.projection, &activation) else {
            return Task::none();
        };
        if sess.projection_policy.expand(zone_id) {
            sess.refresh();
            self.block_menu = None;
            if self.layout().contains_session(source) {
                self.set_focus(source);
                self.session_dirty = true;
            }
            self.refresh_active_context();
        }
        Task::none()
    }

    /// Route a grid mouse interaction either to the running application (when it
    /// has enabled mouse reporting and Shift is not held) or to local selection
    /// and scrollback handling.
    fn handle_mouse(&mut self, source_session_id: usize, input: MouseInput) -> Task<Message> {
        let speed = self.config.scroll_speed.max(1) as isize;

        // Block selection first. The widget snapshots the exact family action
        // at press time and never arms native drag/release for a claimed card
        // gesture. Re-resolve its stable pane and row target here: a
        // completion/trim, focus switch, or alternate-screen transition
        // between render and update must fail closed.
        if let MouseInput::Press {
            block: Some(action),
            block_zone_id,
            row,
            button,
            x,
            y,
            ..
        } = input
        {
            self.terminal_mouse_gestures[button.slot()] = None;
            let Some(source) = self.session_index_by_id(source_session_id) else {
                return Task::none();
            };
            let current_row_zone_id = self.block_at_viewport_row(source_session_id, row);
            let retained_finalized = block_zone_id.is_some_and(|id| {
                self.sessions[source]
                    .terminal
                    .zone_by_id(id)
                    .is_some_and(|zone| !zone.rows_evicted)
            });
            let Some(id) = validated_claimed_block_target(
                block_zone_id,
                current_row_zone_id,
                retained_finalized,
            ) else {
                return Task::none();
            };
            let Some(sess) = self.sessions.get_mut(source) else {
                return Task::none();
            };
            let ids: Vec<u64> = sess
                .terminal
                .command_zones
                .iter()
                .map(|zone| zone.id)
                .collect();
            sess.block_selection.retain(&ids);
            match action {
                BlockMouseAction::Replace => sess.block_selection.click(&ids, id, false, false),
                BlockMouseAction::Range => sess.block_selection.click(&ids, id, false, true),
                BlockMouseAction::Toggle => sess.block_selection.click(&ids, id, true, true),
                BlockMouseAction::Menu => {
                    sess.block_selection.activate(&ids, id);
                    self.block_menu = Some(BlockMenuState {
                        session_id: sess.id,
                        zone_id: id,
                        anchor: iced::Point::new(x, y),
                    });
                    self.block_search = None;
                }
            }
            if action != BlockMouseAction::Menu {
                self.block_menu = None;
            }
            // A claimed card gesture replaces any older native cell range.
            // Otherwise the ordinary Copy shortcut would keep preferring a
            // stale highlight over the block selection the user just made.
            sess.terminal.clear_text_selection();
            sess.refresh();
            return Task::none();
        }

        if let MouseInput::Press { button, .. } = input {
            // An unclaimed left/middle press begins ordinary terminal text /
            // paste interaction and exits historical card selection, whether
            // it landed in completed output or the live/input surface.
            if button != MouseButton::Right {
                if let Some(source) = self.session_index_by_id(source_session_id) {
                    let sess = &mut self.sessions[source];
                    sess.block_selection.clear();
                    sess.refresh();
                }
            }
            self.block_menu = None;
        }

        // Link metadata is active-pane scoped. An inactive pane therefore
        // cannot classify the link inside its widget; focusing it refreshes
        // `self.links` before this handler runs. If that second, authoritative
        // lookup claims the press, retain a consumed app-layer slot so the
        // widget's already-armed Drag/Release sequence is swallowed whole.
        if let MouseInput::Press {
            col,
            row,
            button,
            ctrl,
            shift,
            count,
            link_eligible,
            link: widget_claimed_link,
            link_revision,
            ..
        } = input
        {
            let revision_matches = self
                .session_index_by_id(source_session_id)
                .and_then(|source| self.sessions.get(source))
                .is_some_and(|session| {
                    link_projection_matches(link_revision, session.projection.view_revision())
                });
            let current_link = revision_matches
                .then(|| {
                    terminal_view::ctrl_link_eligible(button, count, ctrl, shift, link_eligible)
                        .then(|| {
                            self.links
                                .iter()
                                .find(|link| {
                                    link.line == row && col >= link.col_start && col < link.col_end
                                })
                                .cloned()
                        })
                        .flatten()
                })
                .flatten();
            if widget_claimed_link || current_link.is_some() {
                self.click_tracker.cancel();
                self.terminal_mouse_gestures[button.slot()] =
                    (!widget_claimed_link).then_some(TerminalMouseGesture {
                        session_id: source_session_id,
                        button,
                        report_to_app: false,
                        consumed: true,
                    });
                if let Some(link) = current_link {
                    let cwd = self
                        .session_index_by_id(source_session_id)
                        .and_then(|source| self.sessions.get(source))
                        .and_then(|session| session.cwd_cache.clone().or_else(|| session.cwd()));
                    if let Err(error) =
                        link::open_link(&link, cwd.as_deref().map(std::path::Path::new))
                    {
                        self.push_toast(
                            format!("Could not open link: {error}"),
                            ToastKind::Warning,
                        );
                    }
                }
                return Task::none();
            }
        }

        // Resolve each event to the stable pane chosen at press time. Focus may
        // move (or an overlay may open) while buttons are held; neither may
        // redirect the trailing event to the newly active PTY.
        let (report_to_app, target_session_id) = match input {
            MouseInput::Press {
                button,
                shift,
                app_eligible,
                ..
            } => {
                let Some(source) = self.session_index_by_id(source_session_id) else {
                    return Task::none();
                };
                let report_to_app = app_owns_terminal_mouse(
                    self.sessions[source].terminal.is_mouse_enabled(),
                    shift,
                    app_eligible,
                    false,
                );
                self.terminal_mouse_gestures[button.slot()] = Some(TerminalMouseGesture {
                    session_id: source_session_id,
                    button,
                    report_to_app,
                    consumed: false,
                });
                (report_to_app, source_session_id)
            }
            MouseInput::Drag { .. } => {
                let Some(gesture) = self.terminal_mouse_gestures[MouseButton::Left.slot()] else {
                    return Task::none();
                };
                if gesture.button != MouseButton::Left
                    || gesture.session_id != source_session_id
                    || gesture.consumed
                {
                    return Task::none();
                }
                (gesture.report_to_app, gesture.session_id)
            }
            MouseInput::Release { button, .. } => {
                let Some(gesture) = self.terminal_mouse_gestures[button.slot()].take() else {
                    return Task::none();
                };
                if gesture.button != button || gesture.session_id != source_session_id {
                    return Task::none();
                }
                if gesture.consumed {
                    return Task::none();
                }
                (gesture.report_to_app, gesture.session_id)
            }
            MouseInput::Wheel {
                shift,
                app_eligible,
                ..
            } => {
                let Some(source) = self.session_index_by_id(source_session_id) else {
                    return Task::none();
                };
                (
                    app_owns_terminal_wheel(
                        self.sessions[source].terminal.is_mouse_enabled(),
                        shift,
                        app_eligible,
                    ),
                    source_session_id,
                )
            }
            MouseInput::ScrollTo { .. } => (false, source_session_id),
        };

        // Click-to-place-cursor acts on release, so that a drag which happens
        // to select nothing does not also walk the shell's cursor. Only the
        // left-button sequence may mutate this tracker; interleaved right or
        // middle presses must not cancel an in-flight local click.
        let click_moves_cursor = self.config.click_moves_cursor;
        let clicked_cell = match input {
            MouseInput::Press {
                col,
                row,
                button: MouseButton::Left,
                alt,
                shift,
                ctrl,
                count,
                ..
            } => {
                let plain = count == 1 && !alt && !shift && !ctrl && !report_to_app;
                self.click_tracker
                    .press(click_cursor::Cell::new(row as i64, col as i64), plain);
                None
            }
            MouseInput::Drag { col, row, .. } => {
                self.click_tracker
                    .pointer_at(click_cursor::Cell::new(row as i64, col as i64));
                None
            }
            MouseInput::Release {
                button: MouseButton::Left,
                ..
            } => self.click_tracker.release(),
            MouseInput::ScrollTo { .. } => {
                self.click_tracker.cancel();
                None
            }
            _ => None,
        };

        let Some(target) = self.session_index_by_id(target_session_id) else {
            return Task::none();
        };
        let Some(sess) = self.sessions.get_mut(target) else {
            return Task::none();
        };

        match input {
            MouseInput::Press {
                col,
                row,
                button,
                alt,
                count,
                ..
            } => {
                if report_to_app {
                    if let Some((grid_col, grid_row)) =
                        projected_live_grid_cell(&sess.terminal, &sess.projection, row, col)
                    {
                        if let Some(report) =
                            sess.terminal
                                .get_mouse_report(btn_code(button), grid_col, grid_row)
                        {
                            sess.write_pty(&report);
                        }
                    }
                    return Task::none();
                }
                match button {
                    MouseButton::Left => match count {
                        2 => sess
                            .terminal
                            .select_word_in_projection(&sess.projection, row, col),
                        n if n >= 3 => sess
                            .terminal
                            .select_line_in_projection(&sess.projection, row),
                        _ if alt => sess.terminal.start_selection_in_projection(
                            &sess.projection,
                            (row, col),
                            terminal::SelectionMode::Block,
                        ),
                        _ => sess.terminal.start_selection_in_projection(
                            &sess.projection,
                            (row, col),
                            terminal::SelectionMode::Normal,
                        ),
                    },
                    MouseButton::Middle => {
                        let id = sess.id;
                        return iced::clipboard::read_primary()
                            .map(move |text| Message::Pasted(id, text));
                    }
                    MouseButton::Right => {}
                }
            }
            MouseInput::Drag { col, row, count } => {
                if report_to_app {
                    if sess.terminal.is_mouse_motion_enabled() {
                        if let Some((grid_col, grid_row)) =
                            projected_live_grid_cell(&sess.terminal, &sess.projection, row, col)
                        {
                            if let Some(report) =
                                sess.terminal.get_mouse_report(32, grid_col, grid_row)
                            {
                                sess.write_pty(&report);
                            }
                        }
                    }
                    return Task::none();
                }
                match count {
                    2 => sess.terminal.extend_word_selection_in_projection(
                        &sess.projection,
                        row,
                        col,
                    ),
                    n if n >= 3 => sess
                        .terminal
                        .extend_line_selection_in_projection(&sess.projection, row),
                    _ => sess
                        .terminal
                        .update_selection_in_projection(&sess.projection, (row, col)),
                }
            }
            MouseInput::Release { col, row, button } => {
                if report_to_app {
                    if let Some((grid_col, grid_row)) =
                        projected_live_grid_cell(&sess.terminal, &sess.projection, row, col)
                    {
                        if let Some(report) = sess.terminal.get_mouse_release_report(
                            btn_code(button),
                            grid_col,
                            grid_row,
                        ) {
                            sess.write_pty(&report);
                        }
                    }
                    return Task::none();
                }
                if button == MouseButton::Left {
                    // A plain click must be handled before the selection copy:
                    // the press already anchored a one-cell selection, so
                    // `copy_selection` would return that cell's character and
                    // swallow the click.
                    if let Some(cell) = clicked_cell {
                        sess.terminal.clear_text_selection();
                        let bytes = projected_live_grid_cell(
                            &sess.terminal,
                            &sess.projection,
                            cell.row.max(0) as usize,
                            cell.col.max(0) as usize,
                        )
                        .map(|(grid_col, grid_row)| {
                            sess.terminal
                                .click_cursor_move(grid_row, grid_col, click_moves_cursor)
                        })
                        .unwrap_or_default();
                        if !bytes.is_empty() {
                            sess.write_pty(&bytes);
                        }
                        sess.refresh();
                    } else if let Some(text) =
                        sess.terminal.copy_selection().filter(|t| !t.is_empty())
                    {
                        return iced::clipboard::write_primary(text);
                    }
                }
            }
            MouseInput::Wheel {
                col,
                row,
                up,
                ctrl,
                shift: _,
                lines,
                ..
            } => {
                if ctrl {
                    let delta = if up { 1.0 } else { -1.0 } * lines.max(1) as f32;
                    self.adjust_font_size(delta);
                    return Task::none();
                }
                if report_to_app {
                    let code = if up { 64 } else { 65 };
                    if let Some((grid_col, grid_row)) =
                        projected_live_grid_cell(&sess.terminal, &sess.projection, row, col)
                    {
                        // One wheel report per line so apps see the full magnitude.
                        for _ in 0..lines.max(1) {
                            if let Some(report) =
                                sess.terminal.get_mouse_report(code, grid_col, grid_row)
                            {
                                sess.write_pty(&report);
                            }
                        }
                    }
                    return Task::none();
                }
                let step = speed * lines.max(1) as isize;
                sess.scroll(if up { step } else { -step });
            }
            MouseInput::ScrollTo { offset } => {
                sess.set_scroll_offset(offset);
            }
        }
        Task::none()
    }

    /// Shift+Page/Home/End scrolls the scrollback viewport. Returns true if the
    /// keypress was consumed.
    fn handle_scroll_shortcut(&mut self, key: &keyboard::Key, mods: keyboard::Modifiers) -> bool {
        use keyboard::key::Named;
        use keyboard::Key;
        if !mods.shift() {
            return false;
        }
        let Some(sess) = self.sessions.get_mut(self.active) else {
            return false;
        };
        // Page by the active pane's own row count, not the whole window — when
        // split, a pane is shorter than `self.rows`.
        let page = sess.terminal.grid.rows().saturating_sub(1).max(1) as isize;
        match key {
            Key::Named(Named::PageUp) => sess.scroll(page),
            Key::Named(Named::PageDown) => sess.scroll(-page),
            Key::Named(Named::Home) => {
                let len = sess.projection.max_scroll_offset();
                sess.set_scroll_offset(len);
            }
            Key::Named(Named::End) => sess.scroll_to_bottom(),
            _ => return false,
        }
        true
    }

    /// Re-run the search over the active session's full scrollback + live grid.
    /// Match rows remain absolute, so scrolling does not invalidate them.
    fn recompute_search(&mut self) {
        self.search_dirty = false;
        if !self.search.is_open {
            return;
        }
        let Some(sess) = self.sessions.get(self.active) else {
            self.search.matches.clear();
            return;
        };
        let (matches, error) = search::SearchEngine::search_lines(
            sess.terminal.search_lines(),
            &self.search.query,
            self.search.use_regex,
            self.search.case_sensitive,
            &mut self.search.regex_cache,
        );
        self.search.matches = matches;
        self.search.error_message = error;
        if self.search.matches.is_empty()
            || self.search.current_match_index >= self.search.matches.len()
        {
            self.search.current_match_index = 0;
        }
    }

    /// Reveal the active full-buffer search result and refresh the session's
    /// visible snapshot. Kept separate from recomputation so streaming PTY
    /// output never steals the user's manually chosen scroll position.
    fn reveal_current_search_match(&mut self) {
        // This is a location diagnostic for the previously active match, not
        // a search-engine error. Recompute it for the new target.
        clear_stale_hidden_match_diagnostic(&mut self.search.error_message);
        let Some(found) = self.search.current_match() else {
            return;
        };
        if let Some(sess) = self.sessions.get_mut(self.active) {
            let Some(origin) = sess
                .terminal
                .raw_cell_origin_at_absolute(found.line, found.col_start)
            else {
                return;
            };
            match sess
                .terminal
                .locate_raw_cell_in_projection(&sess.projection, origin)
            {
                terminal::ProjectedRawCellLocation::Hidden { zone_id } => {
                    self.search.error_message = Some(format!(
                        "Match is hidden in collapsed block #{zone_id}; expand its output to reveal"
                    ));
                }
                terminal::ProjectedRawCellLocation::Visible(_) => {}
                terminal::ProjectedRawCellLocation::Retained => {
                    if sess.terminal.reveal_raw_cell_in_projection(
                        &sess.projection_policy,
                        &mut sess.projection_view_state,
                        origin,
                    ) {
                        sess.refresh();
                    }
                }
                terminal::ProjectedRawCellLocation::Unmapped => {}
            }
        }
        self.links_cache_key = None;
    }

    /// Route a keypress into the search bar while it is open. Returns true if
    /// the key was consumed (and must not reach the PTY).
    fn handle_search_key(
        &mut self,
        key: &keyboard::Key,
        mods: keyboard::Modifiers,
        text: Option<&str>,
    ) -> bool {
        use keyboard::key::Named;
        use keyboard::Key;
        if !self.search.is_open {
            return false;
        }
        if mods.control() && mods.shift() {
            if let Key::Character(c) = key {
                if c.eq_ignore_ascii_case("f") {
                    self.search.close();
                    return true;
                }
            }
        }
        match key {
            Key::Named(Named::Escape) => {
                self.search.close();
                return true;
            }
            Key::Named(Named::Enter) => {
                if mods.shift() {
                    self.search.prev_match();
                } else {
                    self.search.next_match();
                }
                self.reveal_current_search_match();
                return true;
            }
            Key::Named(Named::Backspace) => {
                self.search.query.pop();
                self.search.history_nav_index = None;
                self.recompute_search();
                return true;
            }
            Key::Named(Named::ArrowUp) => {
                self.search.history_prev();
                self.search.current_match_index = 0;
                self.recompute_search();
                self.reveal_current_search_match();
                return true;
            }
            Key::Named(Named::ArrowDown) => {
                self.search.history_next();
                self.search.current_match_index = 0;
                self.recompute_search();
                self.reveal_current_search_match();
                return true;
            }
            // Ctrl+R toggles regex, Ctrl+I toggles case sensitivity (Alt is the
            // JWM window-manager modifier, so it is avoided here).
            Key::Character(c) if mods.control() => {
                match c.chars().next().map(|c| c.to_ascii_lowercase()) {
                    Some('r') => {
                        self.search.toggle_regex();
                        self.recompute_search();
                        self.reveal_current_search_match();
                    }
                    Some('i') => {
                        self.search.toggle_case_sensitive();
                        self.recompute_search();
                        self.reveal_current_search_match();
                    }
                    _ => {}
                }
                return true;
            }
            _ => {}
        }
        // Printable input appends to the query.
        if !mods.control() && !mods.alt() {
            if let Some(t) = text {
                let printable: String = t.chars().filter(|c| !c.is_control()).collect();
                if !printable.is_empty() {
                    self.search.query.push_str(&printable);
                    self.search.history_nav_index = None;
                    self.search.current_match_index = 0;
                    self.recompute_search();
                    self.reveal_current_search_match();
                    return true;
                }
            }
        }
        // Swallow any other key while the search bar owns the keyboard.
        true
    }

    /// While the Find & Replace panel is open, swallow keys so they don't
    /// reach the PTY; its text inputs handle their own events while focused.
    /// Esc or the toggle chord closes the panel.
    fn handle_search_replace_key(
        &mut self,
        key: &keyboard::Key,
        mods: keyboard::Modifiers,
    ) -> Option<Task<Message>> {
        use keyboard::key::Named;
        use keyboard::Key;
        if !self.search_replace.is_open {
            return None;
        }
        let toggle_chord = key_to_binding_string(key, mods).is_some_and(|binding| {
            self.keybindings.get_command(&binding)
                == Some(keybindings::Command::SearchReplaceToggle)
        });
        if toggle_chord || matches!(key, Key::Named(Named::Escape)) {
            self.search_replace.is_open = false;
        }
        Some(Task::none())
    }

    /// Run the Find & Replace panel against the active pane's selection and
    /// route the result. Mirrors ember's semantics: the scrollback is
    /// read-only program output and is never mutated — the transformed text
    /// goes to the clipboard, or to the prompt via the paste path (bracketed
    /// framing, bounded input queue, no trailing newline).
    fn apply_search_replace(
        &mut self,
        action: search_replace_panel::SearchReplaceAction,
    ) -> Task<Message> {
        let selection = self
            .sessions
            .get(self.active)
            .and_then(|s| s.terminal.copy_selection())
            .filter(|t| !t.is_empty());
        let Some(text) = selection else {
            self.search_replace.status = "No selection".to_string();
            return Task::none();
        };
        let Some(result) = self.search_replace.apply(&text) else {
            return Task::none();
        };
        match action {
            search_replace_panel::SearchReplaceAction::ReplaceToClipboard => {
                iced::clipboard::write(result)
            }
            search_replace_panel::SearchReplaceAction::TypeIntoTerminal => {
                self.search_replace.status = "Typed into terminal".to_string();
                self.type_into_active_pane(result)
            }
        }
    }

    /// While the config panel is open, swallow keys so they don't reach the
    /// PTY; Esc closes it. The panel's own widgets handle their own events.
    fn handle_config_panel_key(
        &mut self,
        key: &keyboard::Key,
        mods: keyboard::Modifiers,
    ) -> Option<Task<Message>> {
        use keyboard::key::Named;
        use keyboard::Key;
        if !self.config_panel_open {
            return None;
        }
        if mods.control() && mods.shift() {
            if let Key::Character(c) = key {
                if c.eq_ignore_ascii_case("o") {
                    self.theme_editor = None;
                    self.config_panel_open = false;
                    return Some(Task::none());
                }
            }
        }
        if let Key::Named(Named::Escape) = key {
            // Esc backs out of the theme editor first, then the panel itself.
            if self.theme_editor.is_some() {
                self.theme_editor = None;
            } else {
                self.config_panel_open = false;
            }
        }
        Some(Task::none())
    }

    fn palette_snap_task(&self) -> Task<Message> {
        let len = self.palette.filtered().len();
        let y = match len {
            0 | 1 => 0.0,
            len => self.palette.selected.min(len - 1) as f32 / (len - 1) as f32,
        };
        iced::widget::operation::snap_to(
            PALETTE_LIST_ID.clone(),
            iced::widget::operation::RelativeOffset { x: 0.0, y },
        )
    }

    fn palette_page_selection(&mut self, delta: isize) {
        let len = self.palette.filtered().len();
        if len == 0 {
            self.palette.selected = 0;
            return;
        }
        self.palette.selected = self
            .palette
            .selected
            .saturating_add_signed(delta)
            .min(len - 1);
    }

    /// Route a keypress into the command palette while it is open. Returns
    /// `Some(task)` if consumed (and must not reach the PTY), `None` otherwise.
    fn handle_palette_key(
        &mut self,
        key: &keyboard::Key,
        mods: keyboard::Modifiers,
        text: Option<&str>,
    ) -> Option<Task<Message>> {
        use keyboard::key::Named;
        use keyboard::Key;
        if !self.palette.is_open {
            return None;
        }
        if mods.control() && mods.shift() {
            if let Key::Character(c) = key {
                if c.eq_ignore_ascii_case("p") {
                    self.palette.close();
                    return Some(Task::none());
                }
            }
        }
        match key {
            Key::Named(Named::Escape) => {
                self.palette.close();
                return Some(Task::none());
            }
            Key::Named(Named::Enter) => {
                let action = self.palette.selected_action();
                self.palette.close();
                return Some(match action {
                    Some(a) => self.execute_palette_action(a),
                    None => Task::none(),
                });
            }
            Key::Named(Named::ArrowUp) => {
                self.palette.select_prev();
                return Some(self.palette_snap_task());
            }
            Key::Named(Named::ArrowDown) => {
                self.palette.select_next();
                return Some(self.palette_snap_task());
            }
            Key::Named(Named::PageUp) => {
                self.palette_page_selection(-8);
                return Some(self.palette_snap_task());
            }
            Key::Named(Named::PageDown) => {
                self.palette_page_selection(8);
                return Some(self.palette_snap_task());
            }
            Key::Named(Named::Home) => {
                self.palette.selected = 0;
                return Some(self.palette_snap_task());
            }
            Key::Named(Named::End) => {
                self.palette.selected = self.palette.filtered().len().saturating_sub(1);
                return Some(self.palette_snap_task());
            }
            Key::Named(Named::Backspace) => {
                self.palette.query.pop();
                self.palette.selected = 0;
                return Some(self.palette_snap_task());
            }
            _ => {}
        }
        // Printable input filters the list.
        if !mods.control() && !mods.alt() {
            if let Some(t) = text {
                let printable: String = t.chars().filter(|c| !c.is_control()).collect();
                if !printable.is_empty() {
                    self.palette.query.push_str(&printable);
                    self.palette.selected = 0;
                    return Some(self.palette_snap_task());
                }
            }
        }
        // Swallow any other key while the palette owns the keyboard.
        Some(Task::none())
    }

    /// Common Copy behavior for both configurable keybindings and the command
    /// palette: a visible terminal text selection wins, otherwise a whole-block
    /// selection is copied in terminal order (`output_only` is Alt+Copy).
    fn edit_copy_task(&mut self, output_only: bool) -> Task<Message> {
        let text = self
            .sessions
            .get(self.active)
            .and_then(|session| session.terminal.copy_selection())
            .filter(|text| !text.is_empty());
        if let Some(text) = text {
            let count = text.chars().count();
            self.push_toast(
                format!("Copied {} char{}", count, if count == 1 { "" } else { "s" }),
                ToastKind::Success,
            );
            return iced::clipboard::write(text);
        }
        let mode = if output_only {
            block_mode::SelectedClipboardMode::Outputs
        } else {
            block_mode::SelectedClipboardMode::Blocks
        };
        self.block_copy_selected_task(mode)
            .unwrap_or_else(Task::none)
    }

    /// Dispatch a palette action to the matching existing operation.
    fn execute_palette_action(&mut self, action: command_palette::PaletteAction) -> Task<Message> {
        use command_palette::PaletteAction;
        self.palette.record_use(action);
        match action {
            PaletteAction::NewTab => {
                self.new_session();
                Task::none()
            }
            PaletteAction::InstallJsh => {
                self.install_or_update_jsh();
                Task::none()
            }
            PaletteAction::CloseTab => self.request_close_session(self.active),
            PaletteAction::NextTab => {
                self.next_session();
                Task::none()
            }
            PaletteAction::PrevTab => {
                self.prev_session();
                Task::none()
            }
            PaletteAction::Copy => self.edit_copy_task(false),
            PaletteAction::Paste => {
                let Some(id) = self.sessions.get(self.active).map(|session| session.id) else {
                    return Task::none();
                };
                iced::clipboard::read().map(move |text| Message::Pasted(id, text))
            }
            PaletteAction::OpenSearch => {
                self.search.toggle();
                self.recompute_search();
                if self.search.is_open {
                    iced::widget::operation::focus(SEARCH_INPUT_ID.clone())
                } else {
                    Task::none()
                }
            }
            PaletteAction::OpenSearchReplace => {
                self.search_replace.toggle();
                if self.search_replace.is_open {
                    iced::widget::operation::focus(SEARCH_REPLACE_FIND_ID.clone())
                } else {
                    Task::none()
                }
            }
            PaletteAction::SplitVertical => {
                self.split(Axis::Vertical);
                Task::none()
            }
            PaletteAction::SplitHorizontal => {
                self.split(Axis::Horizontal);
                Task::none()
            }
            PaletteAction::FocusPaneLeft => {
                self.focus_pane_direction(PaneDirection::Left);
                Task::none()
            }
            PaletteAction::FocusPaneRight => {
                self.focus_pane_direction(PaneDirection::Right);
                Task::none()
            }
            PaletteAction::FocusPaneUp => {
                self.focus_pane_direction(PaneDirection::Up);
                Task::none()
            }
            PaletteAction::FocusPaneDown => {
                self.focus_pane_direction(PaneDirection::Down);
                Task::none()
            }
            PaletteAction::ResizePaneLeft => {
                self.resize_pane_direction(PaneDirection::Left);
                Task::none()
            }
            PaletteAction::ResizePaneRight => {
                self.resize_pane_direction(PaneDirection::Right);
                Task::none()
            }
            PaletteAction::ResizePaneUp => {
                self.resize_pane_direction(PaneDirection::Up);
                Task::none()
            }
            PaletteAction::ResizePaneDown => {
                self.resize_pane_direction(PaneDirection::Down);
                Task::none()
            }
            PaletteAction::ZoomPane => {
                self.toggle_pane_zoom();
                Task::none()
            }
            PaletteAction::SwapPanes => {
                self.swap_panes();
                Task::none()
            }
            PaletteAction::ClosePane => self.close_focused_pane(),
            PaletteAction::ToggleSidebar => self.toggle_sidebar(),
            PaletteAction::ToggleAgent => {
                if self.agent.is_open {
                    self.agent.close();
                    Task::none()
                } else {
                    let session_id = self.sessions.get(self.active).map(|s| s.id).unwrap_or(0);
                    self.agent.open(&self.config, session_id);
                    let focus = iced::widget::operation::focus(AGENT_INPUT_ID.clone());
                    match self.agent_drive_task() {
                        Some(task) => Task::batch([focus, task]),
                        None => focus,
                    }
                }
            }
            PaletteAction::ToggleTasks => Task::done(Message::TaskPanelToggle),
            PaletteAction::OpenSettings => {
                self.config_panel_open = true;
                Task::none()
            }
            PaletteAction::QuickTabSwitch => {
                self.tab_switcher = Some(TabSwitcherState::default());
                iced::widget::operation::focus(TAB_SWITCHER_INPUT_ID.clone())
            }
            PaletteAction::OpenHelp => {
                self.help_open = true;
                Task::none()
            }
            PaletteAction::ZoomIn => {
                self.adjust_font_size(1.0);
                Task::none()
            }
            PaletteAction::ZoomOut => {
                self.adjust_font_size(-1.0);
                Task::none()
            }
            PaletteAction::ZoomReset => {
                self.reset_font_size();
                Task::none()
            }
            PaletteAction::OpacityIncrease => {
                self.adjust_opacity(0.025);
                Task::none()
            }
            PaletteAction::OpacityDecrease => {
                self.adjust_opacity(-0.025);
                Task::none()
            }
            PaletteAction::ScrollToTop => {
                if let Some(sess) = self.sessions.get_mut(self.active) {
                    let len = sess.projection.max_scroll_offset();
                    sess.set_scroll_offset(len);
                }
                Task::none()
            }
            PaletteAction::ScrollToBottom => {
                if let Some(sess) = self.sessions.get_mut(self.active) {
                    sess.scroll_to_bottom();
                }
                Task::none()
            }
            PaletteAction::CopyLastOutput => self.copy_last_output_task(),
            PaletteAction::BlockJumpFirstFailed => self.block_jump_first_failed(),
            PaletteAction::BlockJumpPrevFailed => self.block_jump_failed_step(true),
            PaletteAction::BlockJumpNextFailed => self.block_jump_failed_step(false),
            PaletteAction::BlockCopyCommand => self.block_copy_command_task(),
            PaletteAction::BlockCopyOutput => self.block_copy_output_task(),
            PaletteAction::BlockRecallCommand => self.block_recall_command_task(),
            PaletteAction::BlockSelectAll => self.block_select_all(),
            PaletteAction::BlockClear => self.request_block_clear(),
            PaletteAction::BlockSelectPrev => self.block_select_step(true),
            PaletteAction::BlockSelectNext => self.block_select_step(false),
            PaletteAction::BlockReinputSelectedCommands => {
                self.block_reinput_selected_commands_task()
            }
            PaletteAction::BlockCopyBlock => self.block_copy_block_task(),
            PaletteAction::BlockCopyMarkdown => self.block_copy_markdown_task(),
            PaletteAction::BlockExportSessionMarkdown => {
                self.block_export_session_task(block_export::SessionExportFormat::Markdown)
            }
            PaletteAction::BlockExportSessionJson => {
                self.block_export_session_task(block_export::SessionExportFormat::Json)
            }
            PaletteAction::BlockSearch => self.toggle_block_search(),
            PaletteAction::BlockToggleBookmark => self.block_toggle_target_bookmark(),
            PaletteAction::BlockJumpPrevBookmark => self.block_jump_bookmark(true),
            PaletteAction::BlockJumpNextBookmark => self.block_jump_bookmark(false),
            PaletteAction::BlockFixWithAgent => {
                self.palette_failed_block_agent_task(FailedBlockAgentIntent::Fix)
            }
            PaletteAction::BlockExplainWithAgent => {
                self.palette_failed_block_agent_task(FailedBlockAgentIntent::Explain)
            }
            PaletteAction::BlockRetryFailed => self.palette_failed_block_retry_task(),
            PaletteAction::CommandHistory => self.open_history_picker(),
            PaletteAction::PromptJumpPrev | PaletteAction::PromptJumpNext => {
                if !self.ensure_block_action_available("Prompt navigation") {
                    return Task::none();
                }
                if let Some(sess) = self.sessions.get_mut(self.active) {
                    sess.jump_prompt(matches!(action, PaletteAction::PromptJumpPrev));
                }
                Task::none()
            }
            PaletteAction::ClearScreen => {
                if let Some(sess) = self.sessions.get_mut(self.active) {
                    // Clear screen + scrollback and home the cursor via the
                    // terminal's own parser (shell-agnostic).
                    sess.terminal.process_batch(b"\x1b[3J\x1b[2J\x1b[H");
                    sess.refresh();
                }
                Task::none()
            }
        }
    }

    /// Copy the previous command's output (OSC 133 zones) to the clipboard,
    /// shared by the Ctrl+Shift+G binding and the command palette.
    fn copy_last_output_task(&mut self) -> Task<Message> {
        if !self.ensure_block_action_available("Last block output copy") {
            return Task::none();
        }
        let text = self
            .sessions
            .get(self.active)
            .and_then(|s| s.terminal.last_command_output_text());
        match text {
            Some(text) => {
                let n = text.chars().count();
                self.push_toast(
                    format!("Copied last output ({} chars)", n),
                    ToastKind::Success,
                );
                iced::clipboard::write(text)
            }
            None => {
                self.push_toast(
                    "No command output to copy (needs OSC 133 shell integration)".to_string(),
                    ToastKind::Info,
                );
                Task::none()
            }
        }
    }

    /// Select zone `id` in the active session and reveal its first row — the
    /// shared tail of every "jump the selection somewhere" action (select
    /// step, failed jumps, the block search picker). A zone whose rows were
    /// trimmed out of scrollback is still selected (its metadata actions all
    /// work) but cannot be scrolled to: it toasts instead, with ember's
    /// wording. A vanished id — evicted by the 256-zone cap while (say) the
    /// search picker held the hit — toasts too, with the same wording the
    /// other block actions use for a gone zone, instead of silently doing
    /// nothing.
    fn select_and_reveal_block(&mut self, id: u64) {
        let Some(sess) = self.sessions.get(self.active) else {
            return;
        };
        let Some((prompt_row, rows_evicted)) = sess
            .terminal
            .zone_by_id(id)
            .map(|zone| (zone.prompt_start, zone.rows_evicted))
        else {
            self.push_toast("Block no longer available".to_string(), ToastKind::Warning);
            return;
        };
        if let Some(sess) = self.sessions.get_mut(self.active) {
            sess.block_selection.replace(Some(id));
            if !rows_evicted {
                sess.reveal_absolute_cell(prompt_row, 0);
            }
            sess.refresh();
        }
        if rows_evicted {
            self.push_toast(
                "Command position is no longer in scrollback".to_string(),
                ToastKind::Info,
            );
        }
    }

    /// Select a cross-block search result and reveal the physical soft-wrap row
    /// containing its first match when those live rows still exist. Captured
    /// snapshots can outlive or differ from scrollback, so an invalid span
    /// falls back first to the logical line and then to the ordinary block
    /// header reveal performed by `select_and_reveal_block`.
    fn reveal_block_search_hit(&mut self, hit: &block_mode::BlockSearchHit) {
        self.select_and_reveal_block(hit.zone_id);
        if !hit.is_output_line || hit.line_no == 0 {
            return;
        }
        let row = self
            .sessions
            .get(self.active)
            .and_then(|sess| block_search_reveal_row(&sess.terminal, hit));
        if let Some(row) = row {
            if let Some(sess) = self.sessions.get_mut(self.active) {
                sess.reveal_absolute_cell(row, 0);
            }
        }
    }

    /// Select and reveal the oldest block whose command failed (exit reported
    /// and nonzero). Shared by the keybinding and the command palette; works
    /// even while block-mode rendering is disabled.
    fn block_jump_first_failed(&mut self) -> Task<Message> {
        if !self.ensure_block_action_available("Failed-block navigation") {
            return Task::none();
        }
        let target = self
            .sessions
            .get(self.active)
            .and_then(|sess| sess.terminal.first_failed_zone().map(|zone| zone.id));
        match target {
            Some(id) => self.select_and_reveal_block(id),
            None => self.push_toast(
                "No failed command block (needs OSC 133 shell integration)".to_string(),
                ToastKind::Info,
            ),
        }
        Task::none()
    }

    /// Step the block selection across FAILED zones only (`older` = toward
    /// the top) — the failed-only counterpart of [`Self::block_select_step`],
    /// classified exactly like the scrollbar's failure markers. No (or a
    /// dangling) selection enters at the newest failure when moving older and
    /// the oldest when moving newer; stepping past an edge wraps. Zero failed
    /// zones toasts (jump-first-failed's wording).
    fn block_jump_failed_step(&mut self, older: bool) -> Task<Message> {
        if !self.ensure_block_action_available("Failed-block navigation") {
            return Task::none();
        }
        let Some(sess) = self.sessions.get(self.active) else {
            return Task::none();
        };
        let zones: Vec<(u64, bool)> = sess
            .terminal
            .command_zones
            .iter()
            .map(|zone| {
                (
                    zone.id,
                    matches!(
                        block_mode::classify(zone.command.as_deref(), zone.exit_code),
                        block_mode::BlockOutcome::Failed(_)
                    ),
                )
            })
            .collect();
        if !zones.iter().any(|&(_, failed)| failed) {
            self.push_toast(
                "No failed command block (needs OSC 133 shell integration)".to_string(),
                ToastKind::Info,
            );
            return Task::none();
        }
        let current = sess.block_selection.active();
        let Some(target) = block_mode::select_failed_neighbor(&zones, current, older) else {
            return Task::none();
        };
        self.select_and_reveal_block(target);
        Task::none()
    }

    /// Drop a block selection whose zone no longer exists (trimmed away with
    /// old scrollback) and tell the user why nothing was copied or recalled.
    fn clear_stale_block_selection(&mut self) {
        if let Some(sess) = self.sessions.get_mut(self.active) {
            sess.block_selection.clear();
        }
        self.push_toast("Block no longer available".to_string(), ToastKind::Warning);
    }

    /// Copy a live whole-block selection in terminal order. `None` means no
    /// block selection exists and lets ordinary copy/fallback behavior proceed;
    /// `Some` means block selection owned the request, including an error toast.
    fn block_copy_selected_task(
        &mut self,
        mode: block_mode::SelectedClipboardMode,
    ) -> Option<Task<Message>> {
        let result = {
            let sess = self.sessions.get_mut(self.active)?;
            if !self.config.block_mode || sess.terminal.is_alt_buffer_active() {
                return None;
            }
            if sess.block_selection.is_empty() {
                return None;
            }
            let ids: Vec<u64> = sess
                .terminal
                .command_zones
                .iter()
                .map(|zone| zone.id)
                .collect();
            if !sess.block_selection.retain(&ids) {
                Err(block_mode::SelectedClipboardError::Empty)
            } else {
                block_mode::selected_clipboard_text(
                    sess.terminal
                        .command_zones
                        .iter()
                        .filter(|zone| sess.block_selection.contains(zone.id))
                        .map(|zone| {
                            let output = if mode == block_mode::SelectedClipboardMode::Commands {
                                block_mode::ClipboardOutput::Empty
                            } else {
                                match sess.terminal.zone_output_export_capped(zone.id) {
                                    Some(terminal::ZoneOutputExport::Available {
                                        text, ..
                                    }) => block_mode::ClipboardOutput::Available(text),
                                    Some(terminal::ZoneOutputExport::Empty) => {
                                        block_mode::ClipboardOutput::Empty
                                    }
                                    Some(terminal::ZoneOutputExport::Unavailable) | None => {
                                        block_mode::ClipboardOutput::Unavailable
                                    }
                                }
                            };
                            (zone.id, zone.command.as_deref(), output)
                        }),
                    &sess.block_selection,
                    mode,
                    block_mode::SELECTED_CLIPBOARD_MAX_BYTES,
                )
            }
        };

        match result {
            Ok(copied) => {
                let noun = match mode {
                    block_mode::SelectedClipboardMode::Commands => "command",
                    block_mode::SelectedClipboardMode::Outputs => "output",
                    block_mode::SelectedClipboardMode::Blocks => "block",
                };
                self.push_toast(
                    format!(
                        "Copied {} selected block {noun}{}",
                        copied.block_count,
                        if copied.block_count == 1 { "" } else { "s" }
                    ),
                    ToastKind::Success,
                );
                Some(iced::clipboard::write(copied.text))
            }
            Err(block_mode::SelectedClipboardError::Empty) => {
                let message = match mode {
                    block_mode::SelectedClipboardMode::Commands => {
                        "Selected blocks have no commands to copy"
                    }
                    block_mode::SelectedClipboardMode::Outputs => {
                        "Selected blocks have no output to copy"
                    }
                    block_mode::SelectedClipboardMode::Blocks => {
                        "Selected blocks have no content to copy"
                    }
                };
                self.push_toast(message.to_string(), ToastKind::Info);
                Some(Task::none())
            }
            Err(block_mode::SelectedClipboardError::OutputUnavailable) => {
                self.push_toast(
                    "Selected block output is no longer retained; nothing was copied".to_string(),
                    ToastKind::Warning,
                );
                Some(Task::none())
            }
            Err(block_mode::SelectedClipboardError::TooLarge) => {
                self.push_toast(
                    "Selected block content is too large to copy".to_string(),
                    ToastKind::Warning,
                );
                Some(Task::none())
            }
        }
    }

    /// The command line a block copy/recall action should act on, paired with
    /// whether shell metadata or Frost's bounded capture marked it truncated.
    /// With a block selected, only
    /// THAT block may supply it: a vanished zone clears the selection, a
    /// command-less zone toasts — never a silent substitution of a different
    /// block. Only when no block is selected does this fall back to the
    /// newest completed block with a command. `None` means "do nothing" and a
    /// toast was already pushed (`verb` names the action in the no-fallback
    /// message).
    fn block_action_command(&mut self, verb: &str) -> Option<(String, bool)> {
        let sess = self.sessions.get(self.active)?;
        let Some(id) = sess.block_selection.active() else {
            let fallback = sess.terminal.command_zones.iter().rev().find_map(|zone| {
                zone.command
                    .clone()
                    .map(|command| (command, zone.command_truncated))
            });
            if fallback.is_none() {
                self.push_toast(
                    format!("No block command to {verb} (needs OSC 133 shell integration)"),
                    ToastKind::Info,
                );
            }
            return fallback;
        };
        let Some(zone) = sess.terminal.zone_by_id(id) else {
            self.clear_stale_block_selection();
            return None;
        };
        let command = zone
            .command
            .clone()
            .map(|command| (command, zone.command_truncated));
        if command.is_none() {
            self.push_toast("Selected block has no command".to_string(), ToastKind::Info);
        }
        command
    }

    /// Copy the selected block's command line to the clipboard (the newest
    /// block's only when nothing is selected). A truncated command may still
    /// be copied — only re-running it is unsafe.
    fn block_copy_command_task(&mut self) -> Task<Message> {
        if !self.ensure_block_action_available("Block command copy") {
            return Task::none();
        }
        if let Some(task) =
            self.block_copy_selected_task(block_mode::SelectedClipboardMode::Commands)
        {
            return task;
        }
        match self.block_action_command("copy") {
            Some((text, _)) => {
                self.push_toast("Copied block command", ToastKind::Success);
                iced::clipboard::write(text)
            }
            None => Task::none(),
        }
    }

    /// Copy the selected block's output to the clipboard. Falling back to the
    /// newest completed block with output is only allowed when no block is
    /// selected; a selected block with no output (or a vanished zone) toasts
    /// instead of substituting another block's output.
    fn block_copy_output_task(&mut self) -> Task<Message> {
        if !self.ensure_block_action_available("Block output copy") {
            return Task::none();
        }
        if let Some(task) =
            self.block_copy_selected_task(block_mode::SelectedClipboardMode::Outputs)
        {
            return task;
        }
        let Some(sess) = self.sessions.get(self.active) else {
            return Task::none();
        };
        let text = match sess.block_selection.active() {
            None => {
                let fallback = sess.terminal.last_command_output_text();
                if fallback.is_none() {
                    self.push_toast(
                        "No block output to copy (needs OSC 133 shell integration)".to_string(),
                        ToastKind::Info,
                    );
                }
                fallback
            }
            Some(id) => {
                if sess.terminal.zone_by_id(id).is_none() {
                    self.clear_stale_block_selection();
                    return Task::none();
                }
                match sess.terminal.zone_output_export_capped(id) {
                    Some(terminal::ZoneOutputExport::Available { text, .. }) => Some(text),
                    Some(terminal::ZoneOutputExport::Empty) => {
                        self.push_toast(
                            "Selected block has no output".to_string(),
                            ToastKind::Info,
                        );
                        None
                    }
                    Some(terminal::ZoneOutputExport::Unavailable) => {
                        self.push_toast(
                            "Selected block output is no longer retained".to_string(),
                            ToastKind::Warning,
                        );
                        None
                    }
                    None => None,
                }
            }
        };
        match text {
            Some(text) => {
                let n = text.chars().count();
                self.push_toast(
                    format!("Copied block output ({} chars)", n),
                    ToastKind::Success,
                );
                iced::clipboard::write(text)
            }
            None => Task::none(),
        }
    }

    /// Type (never execute) the selected block's command into the prompt
    /// (the newest block's only when nothing is selected), through the same
    /// sanitized recall path as the history picker. Refused unless OSC 133
    /// reports a trusted empty prompt owned by the foreground shell — merely
    /// observing that no command is running would still overwrite typed input.
    fn block_recall_command_task(&mut self) -> Task<Message> {
        if !self.ensure_block_action_available("Block command recall") {
            return Task::none();
        }
        let Some(sess) = self.sessions.get_mut(self.active) else {
            return Task::none();
        };
        let prompt_status = sess.agent_prompt_status();
        if let Some(reason) = block_prompt_replace_blocker(prompt_status) {
            self.push_toast(
                format!("Command not recalled: {reason}"),
                ToastKind::Warning,
            );
            return Task::none();
        }
        match self.block_action_command("recall") {
            // An incomplete capture is not the command that ran; typing it
            // back could execute something else.
            Some((_, true)) => {
                self.push_toast(
                    "Command not recalled: capture is truncated or unavailable".to_string(),
                    ToastKind::Warning,
                );
                Task::none()
            }
            Some((command, false)) => self.recall_into_active_pane(command),
            None => Task::none(),
        }
    }

    /// Select every retained finalized block in the active pane. The oldest
    /// block becomes the fixed range anchor and the newest the active edge.
    fn block_select_all(&mut self) -> Task<Message> {
        if !self.ensure_block_action_available("Block selection") {
            return Task::none();
        }
        let Some(sess) = self.sessions.get_mut(self.active) else {
            return Task::none();
        };
        let ids: Vec<u64> = sess
            .terminal
            .command_zones
            .iter()
            .map(|zone| zone.id)
            .collect();
        if ids.is_empty() {
            self.push_toast(
                "No command blocks to select (needs OSC 133 shell integration)".to_string(),
                ToastKind::Info,
            );
            return Task::none();
        }
        sess.block_selection.select_all(&ids);
        sess.refresh();
        Task::none()
    }

    /// Open the one counted confirmation path shared by keybindings, the
    /// command palette, and the block context menu. Nothing is removed here.
    fn request_block_clear(&mut self) -> Task<Message> {
        if !self.ensure_block_action_available("Clear Blocks") {
            return Task::none();
        }
        let Some(sess) = self.sessions.get(self.active) else {
            return Task::none();
        };
        let Some(confirm) = BlockClearConfirmation::new(
            sess.id,
            sess.terminal.command_zones.len(),
            sess.terminal.command_zones.back().map(|zone| zone.id),
        ) else {
            self.push_toast(
                "No completed command blocks to clear".to_string(),
                ToastKind::Info,
            );
            return Task::none();
        };
        self.block_clear_confirm = Some(confirm);
        Task::none()
    }

    /// Revalidate the stable pane and exact displayed count, then either
    /// execute, fail closed, or update the modal for a deliberate second
    /// confirmation when asynchronous terminal output changed the block set.
    fn confirm_block_clear(&mut self) -> Task<Message> {
        let Some(confirm) = self.block_clear_confirm.take() else {
            return Task::none();
        };
        let active = self.sessions.get(self.active).map(|sess| {
            (
                sess.id,
                sess.terminal.command_zones.len(),
                sess.terminal.command_zones.back().map(|zone| zone.id),
            )
        });
        match confirm.resolve(active) {
            BlockClearResolution::Clear => self.execute_block_clear(confirm.session_id),
            BlockClearResolution::Refresh(updated) => {
                self.block_clear_confirm = Some(updated);
                self.push_toast(
                    "Block history changed — review the current total and confirm again"
                        .to_string(),
                    ToastKind::Warning,
                );
                Task::none()
            }
            BlockClearResolution::Empty => {
                self.push_toast(
                    "No completed command blocks remain to clear".to_string(),
                    ToastKind::Info,
                );
                Task::none()
            }
            BlockClearResolution::Stale => {
                self.push_toast(
                    "Clear Blocks target is no longer active".to_string(),
                    ToastKind::Info,
                );
                Task::none()
            }
        }
    }

    /// The sole destructive implementation behind the confirmation. The
    /// terminal keeps the current prompt or running command, while app-owned
    /// selection/search state is discarded atomically with the zone history.
    fn execute_block_clear(&mut self, session_id: usize) -> Task<Message> {
        if !self.ensure_block_action_available("Clear Blocks") {
            return Task::none();
        }
        let Some(sess) = self
            .sessions
            .get_mut(self.active)
            .filter(|sess| sess.id == session_id)
        else {
            self.push_toast(
                "Clear Blocks target is no longer active".to_string(),
                ToastKind::Info,
            );
            return Task::none();
        };

        let cleared = sess.terminal.clear_completed_blocks();
        sess.block_selection.clear();
        sess.block_bookmarks.clear();
        sess.terminal.clear_text_selection();
        sess.refresh();

        self.block_search = None;
        self.search.close();
        self.search.matches.clear();
        self.search.current_match_index = 0;
        self.search.error_message = None;
        self.search_dirty = false;
        self.links_cache_key = None;
        self.links.clear();
        self.refresh_kitty_handles();
        if cleared == 0 {
            return Task::none();
        }
        self.push_toast(
            format!(
                "Cleared {cleared} command block{}",
                if cleared == 1 { "" } else { "s" }
            ),
            ToastKind::Success,
        );
        Task::none()
    }

    /// Expand or contract the selected range by moving its active edge one
    /// completed block. Used by Shift+Up/Down while a block selection owns
    /// keyboard navigation.
    fn block_extend_selection_step(&mut self, older: bool) -> Task<Message> {
        if !self.ensure_block_action_available("Block selection") {
            return Task::none();
        }
        let Some(sess) = self.sessions.get_mut(self.active) else {
            return Task::none();
        };
        let ids: Vec<u64> = sess
            .terminal
            .command_zones
            .iter()
            .map(|zone| zone.id)
            .collect();
        sess.block_selection.retain(&ids);
        let Some(target) = sess.block_selection.extend_step(&ids, older) else {
            return Task::none();
        };
        if let Some(prompt_start) = sess
            .terminal
            .zone_by_id(target)
            .filter(|zone| !zone.rows_evicted)
            .map(|zone| zone.prompt_start)
        {
            sess.reveal_absolute_cell(prompt_start, 0);
        }
        sess.refresh();
        Task::none()
    }

    /// Reinput every selected command in terminal order without submitting
    /// it. A multi-command selection is one editable bracketed-paste buffer;
    /// without DECSET 2004 the shared safe policy keeps only the first logical
    /// line, so later selected commands cannot execute as embedded newlines.
    fn block_reinput_selected_commands_task(&mut self) -> Task<Message> {
        if !self.ensure_block_action_available("Block command reinput") {
            return Task::none();
        }
        let Some(sess) = self.sessions.get_mut(self.active) else {
            return Task::none();
        };
        if sess.block_selection.is_empty() {
            self.push_toast("No command blocks selected".to_string(), ToastKind::Info);
            return Task::none();
        }
        let prompt_status = sess.agent_prompt_status();
        if let Some(reason) = block_prompt_replace_blocker(prompt_status) {
            self.push_toast(
                format!("Commands not reinput: {reason}"),
                ToastKind::Warning,
            );
            return Task::none();
        }

        let (session_id, bracketed, commands) = {
            let sess = &mut self.sessions[self.active];
            let ids: Vec<u64> = sess
                .terminal
                .command_zones
                .iter()
                .map(|zone| zone.id)
                .collect();
            sess.block_selection.retain(&ids);
            let commands = block_mode::selected_commands(
                sess.terminal
                    .command_zones
                    .iter()
                    .map(|zone| (zone.id, zone.command.as_deref(), zone.command_truncated)),
                &sess.block_selection,
                crate::review_text::MAX_PROMPT_INSERT_BYTES,
            );
            (
                sess.id,
                sess.terminal.is_bracketed_paste_enabled(),
                commands,
            )
        };
        let selected_commands = match commands {
            Ok(commands) => commands,
            Err(block_mode::SelectedCommandsError::Empty) => {
                self.push_toast(
                    "Selected blocks have no commands to reinput".to_string(),
                    ToastKind::Info,
                );
                return Task::none();
            }
            Err(block_mode::SelectedCommandsError::Truncated) => {
                self.push_toast(
                    "Commands not reinput: a selected command was truncated or unavailable"
                        .to_string(),
                    ToastKind::Warning,
                );
                return Task::none();
            }
            Err(block_mode::SelectedCommandsError::TooLarge) => {
                self.push_toast(
                    "Commands not reinput: the selected commands are too large".to_string(),
                    ToastKind::Warning,
                );
                return Task::none();
            }
        };
        let command_count = selected_commands.block_count;
        let commands = selected_commands.text;
        let commands = match crate::review_text::sanitize_prompt_payload(
            &commands,
            crate::review_text::MAX_PROMPT_INSERT_BYTES,
        ) {
            Ok(commands) => commands,
            Err(error) => {
                self.push_toast(format!("Commands not reinput: {error}"), ToastKind::Warning);
                return Task::none();
            }
        };
        if commands.trim().is_empty() {
            self.push_toast(
                "Selected blocks have no safe command text to reinput".to_string(),
                ToastKind::Warning,
            );
            return Task::none();
        }
        let multiline_fallback = !bracketed && commands.contains('\n');
        if let Err(reason) = self.session_prompt_replace_ready(session_id) {
            self.push_toast(
                format!("Commands not reinput: {reason}"),
                ToastKind::Warning,
            );
            return Task::none();
        }
        if self.write_paste_to_session(session_id, &commands, block_reinput_policy(), true) {
            if multiline_fallback {
                self.push_toast(
                    "Reinput only the first logical line because bracketed paste is disabled"
                        .to_string(),
                    ToastKind::Info,
                );
            } else {
                self.push_toast(
                    format!(
                        "Reinput {command_count} selected block{} (not run)",
                        if command_count == 1 { "" } else { "s" }
                    ),
                    ToastKind::Success,
                );
            }
        }
        Task::none()
    }

    /// Move the block selection to a neighbouring completed zone (`older` =
    /// toward the top) and reveal its first row, exactly the row
    /// [`Self::block_jump_first_failed`] scrolls to. With no selection — or a
    /// selection whose zone was trimmed away — Up resets to the newest completed
    /// zone while Down remains unowned. Up clamps at the oldest block; Down past
    /// the newest clears selection, matching anvil/forge.
    fn block_select_step(&mut self, older: bool) -> Task<Message> {
        if !self.ensure_block_action_available("Block selection") {
            return Task::none();
        }
        let navigation = {
            let Some(sess) = self.sessions.get_mut(self.active) else {
                return Task::none();
            };
            let ids: Vec<u64> = sess
                .terminal
                .command_zones
                .iter()
                .map(|zone| zone.id)
                .collect();
            sess.block_selection.retain(&ids);
            block_mode::selection_navigation(&ids, sess.block_selection.active(), older)
        };
        match navigation {
            block_mode::SelectionNavigation::Select(target) => {
                self.select_and_reveal_block(target);
            }
            block_mode::SelectionNavigation::Clear => {
                if let Some(sess) = self.sessions.get_mut(self.active) {
                    sess.block_selection.clear();
                    sess.refresh();
                }
            }
            block_mode::SelectionNavigation::Passthrough => {}
        }
        Task::none()
    }

    /// The completed zone a whole-block action (copy block / copy Markdown)
    /// should act on: the selected zone — a vanished selection toasts and
    /// cancels (never a silent substitution) — else the newest completed
    /// zone. `None` means a toast was already pushed.
    fn block_target_zone_id(&mut self, verb: &str) -> Option<u64> {
        let sess = self.sessions.get(self.active)?;
        match sess.block_selection.active() {
            Some(id) => {
                if sess.terminal.zone_by_id(id).is_none() {
                    self.clear_stale_block_selection();
                    return None;
                }
                Some(id)
            }
            None => {
                let newest = sess.terminal.command_zones.back().map(|zone| zone.id);
                if newest.is_none() {
                    self.push_toast(
                        format!("No block to {verb} (needs OSC 133 shell integration)"),
                        ToastKind::Info,
                    );
                }
                newest
            }
        }
    }

    /// Copy the selected block (newest when nothing is selected) as plain
    /// text via [`block_mode::block_copy_text`] (anvil's clipboard rule):
    /// command + newline + output, a blank output copying the bare command
    /// with no trailing newline, and background zones (no command) copying
    /// their output alone.
    fn block_copy_block_task(&mut self) -> Task<Message> {
        if !self.ensure_block_action_available("Block copy") {
            return Task::none();
        }
        if let Some(task) = self.block_copy_selected_task(block_mode::SelectedClipboardMode::Blocks)
        {
            return task;
        }
        let Some(id) = self.block_target_zone_id("copy") else {
            return Task::none();
        };
        let Some(sess) = self.sessions.get(self.active) else {
            return Task::none();
        };
        let Some(zone) = sess.terminal.zone_by_id(id) else {
            return Task::none();
        };
        let output = match sess.terminal.zone_output_export_capped(id) {
            Some(terminal::ZoneOutputExport::Available { text, .. }) => Some(text),
            Some(terminal::ZoneOutputExport::Empty) => None,
            Some(terminal::ZoneOutputExport::Unavailable) => {
                self.push_toast(
                    "Block output is no longer retained; copy the command separately".to_string(),
                    ToastKind::Warning,
                );
                return Task::none();
            }
            None => return Task::none(),
        };
        let Some(text) = block_mode::block_copy_text(zone.command.as_deref(), output.as_deref())
        else {
            self.push_toast("Block is empty".to_string(), ToastKind::Info);
            return Task::none();
        };
        self.push_toast("Copied block", ToastKind::Success);
        iced::clipboard::write(text)
    }

    fn block_markdown(terminal: &terminal::TerminalState, zone: &terminal::CommandZone) -> String {
        let (output, output_truncated, output_unavailable) = match terminal
            .zone_output_export_capped(zone.id)
        {
            Some(terminal::ZoneOutputExport::Available { text, truncated }) => {
                (text, truncated, false)
            }
            Some(terminal::ZoneOutputExport::Empty) => (String::new(), false, false),
            Some(terminal::ZoneOutputExport::Unavailable) | None => (String::new(), false, true),
        };
        // Resolve the offset at the finish instant, not at copy time, so a
        // block that crossed a DST change keeps the correct local timestamp.
        let tz_offset_secs = block_mode::local_offset_secs(zone.finished_at_ms.map_or_else(
            || {
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_or(0, |elapsed| elapsed.as_secs() as i64)
            },
            |ms| (ms / 1000) as i64,
        ));
        block_mode::markdown_export_with_state(
            &block_mode::MarkdownBlock {
                command: zone.command.as_deref(),
                output: &output,
                output_truncated,
                exit_code: zone.exit_code,
                duration_ms: zone.duration_ms,
                finished_at_ms: zone.finished_at_ms,
                tz_offset_secs,
                cwd: zone.cwd.as_deref(),
            },
            zone.command_truncated,
            output_unavailable,
            zone.completion_observed,
        )
    }

    /// Copy every selected block as Markdown in terminal order. Rendering is
    /// lazy and aggregation is bounded, so an oversized selection fails
    /// atomically instead of placing a partial history on the clipboard.
    fn block_copy_selected_markdown_task(&mut self) -> Option<Task<Message>> {
        let result = {
            let sess = self.sessions.get_mut(self.active)?;
            if !self.config.block_mode || sess.terminal.is_alt_buffer_active() {
                return None;
            }
            if sess.block_selection.is_empty() {
                return None;
            }
            let ids: Vec<u64> = sess
                .terminal
                .command_zones
                .iter()
                .map(|zone| zone.id)
                .collect();
            if !sess.block_selection.retain(&ids) {
                Err(block_mode::SelectedClipboardError::Empty)
            } else {
                block_mode::selected_markdown_text(
                    sess.terminal
                        .command_zones
                        .iter()
                        .filter(|zone| sess.block_selection.contains(zone.id))
                        .map(|zone| (zone.id, Self::block_markdown(&sess.terminal, zone))),
                    &sess.block_selection,
                    block_mode::SELECTED_CLIPBOARD_MAX_BYTES,
                )
            }
        };

        match result {
            Ok(copied) => {
                self.push_toast(
                    format!(
                        "Copied {} selected block{} as Markdown",
                        copied.block_count,
                        if copied.block_count == 1 { "" } else { "s" }
                    ),
                    ToastKind::Success,
                );
                Some(iced::clipboard::write(copied.text))
            }
            Err(block_mode::SelectedClipboardError::Empty) => {
                self.push_toast("Selected blocks are no longer available", ToastKind::Info);
                Some(Task::none())
            }
            Err(
                block_mode::SelectedClipboardError::TooLarge
                | block_mode::SelectedClipboardError::OutputUnavailable,
            ) => {
                self.push_toast(
                    "Selected block Markdown is too large to copy",
                    ToastKind::Warning,
                );
                Some(Task::none())
            }
        }
    }

    /// Copy the selected block(s), or newest block when nothing is selected,
    /// as the family's shared Markdown snippet, including retained
    /// lifecycle/capture notes when the available data is incomplete.
    fn block_copy_markdown_task(&mut self) -> Task<Message> {
        if !self.ensure_block_action_available("Block Markdown copy") {
            return Task::none();
        }
        if let Some(task) = self.block_copy_selected_markdown_task() {
            return task;
        }
        let Some(id) = self.block_target_zone_id("copy") else {
            return Task::none();
        };
        let Some(sess) = self.sessions.get(self.active) else {
            return Task::none();
        };
        let Some(zone) = sess.terminal.zone_by_id(id) else {
            return Task::none();
        };
        let markdown = Self::block_markdown(&sess.terminal, zone);
        self.push_toast("Copied block as Markdown", ToastKind::Success);
        iced::clipboard::write(markdown)
    }

    fn start_block_export(
        &mut self,
        snapshot: block_export::SessionExportSnapshot,
        format: block_export::SessionExportFormat,
        description: &str,
    ) -> Task<Message> {
        self.block_export_in_flight = true;
        self.push_toast(
            format!("Exporting {} {description}…", format.label()),
            ToastKind::Info,
        );
        Task::perform(
            async move {
                tokio::task::spawn_blocking(move || {
                    block_export::export_session_to_file(&snapshot, format)
                        .map_err(|error| error.to_string())
                })
                .await
                .unwrap_or_else(|error| Err(format!("export worker failed: {error}")))
            },
            move |result| Message::BlockExportFinished(format, result),
        )
    }

    /// Export exactly the stable card captured by a right click. The snapshot
    /// intentionally never clones unrelated blocks from the same pane.
    fn block_export_zone_task(
        &mut self,
        zone_id: u64,
        format: block_export::SessionExportFormat,
    ) -> Task<Message> {
        if !self.ensure_block_action_available("Block export") {
            return Task::none();
        }
        if self.block_export_in_flight {
            self.push_toast(
                "A block export is already in progress".to_string(),
                ToastKind::Info,
            );
            return Task::none();
        }
        let Some(sess) = self.sessions.get(self.active) else {
            return Task::none();
        };
        let snapshot = match block_export::snapshot_block(&sess.terminal, sess.id, zone_id) {
            Ok(snapshot) => snapshot,
            Err(error) => {
                self.push_toast(
                    format!("Could not prepare block export: {error}"),
                    ToastKind::Warning,
                );
                return Task::none();
            }
        };
        self.start_block_export(snapshot, format, "block")
    }

    /// Snapshot every retained finalized block in the active pane, then serialize and
    /// durably write it off the UI thread. The snapshot is bounded before the
    /// task starts, and the worker owns it, so later output/session churn cannot
    /// change the document halfway through an export.
    fn block_export_session_task(
        &mut self,
        format: block_export::SessionExportFormat,
    ) -> Task<Message> {
        if !self.ensure_block_action_available("Block session export") {
            return Task::none();
        }
        if self.block_export_in_flight {
            self.push_toast(
                "A session export is already in progress".to_string(),
                ToastKind::Info,
            );
            return Task::none();
        }
        let Some(sess) = self.sessions.get(self.active) else {
            return Task::none();
        };
        if sess.terminal.command_zones.is_empty() {
            self.push_toast(
                "No retained command blocks to export (needs OSC 133 shell integration)"
                    .to_string(),
                ToastKind::Info,
            );
            return Task::none();
        }
        let snapshot = match block_export::snapshot_session(&sess.terminal, sess.id) {
            Ok(snapshot) => snapshot,
            Err(error) => {
                self.push_toast(
                    format!("Could not prepare session export: {error}"),
                    ToastKind::Warning,
                );
                return Task::none();
            }
        };
        self.start_block_export(snapshot, format, "session blocks")
    }

    fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::PtyOutput(id, fd, data) => {
                let t0 = std::time::Instant::now();
                let is_active_output = self
                    .sessions
                    .get(self.active)
                    .is_some_and(|session| session.id == id && session.master_fd == fd);
                let mut clip_set: Option<String> = None;
                let mut clip_query = false;
                let mut clip_requests: Vec<terminal::ClipboardReadKind> = Vec::new();
                let mut notifications: Vec<(String, String)> = Vec::new();
                let mut completed_commands: Vec<terminal::CompletedCommand> = Vec::new();
                if let Some(sess) = self.session_by_identity(id, fd) {
                    sess.terminal.process_batch(&data);
                    let retained_block_ids: Vec<u64> = sess
                        .terminal
                        .command_zones
                        .iter()
                        .map(|zone| zone.id)
                        .collect();
                    sess.block_selection.retain(&retained_block_ids);
                    sess.block_bookmarks.retain(&retained_block_ids);
                    sess.flush_responses();
                    sess.refresh();
                    clip_set = sess.terminal.take_osc52_clipboard_set();
                    clip_query = sess.terminal.take_osc52_clipboard_query();
                    clip_requests = sess
                        .terminal
                        .take_clipboard_read_requests()
                        .into_iter()
                        .map(|r| r.kind)
                        .collect();
                    notifications = sess.terminal.pending_notifications.drain(..).collect();
                    completed_commands = sess.terminal.take_completed_commands();
                    // Retain the newest completion for the bottom bar; the
                    // drain above is the only place finished commands surface.
                    if let Some(last) = completed_commands.last() {
                        sess.last_exit = last.exit_code;
                        sess.last_duration_ms = last.duration_ms;
                    }
                }
                self.last_ingest_us = t0.elapsed().as_micros();
                self.last_ingest_bytes = data.len();
                if is_active_output
                    && self
                        .sessions
                        .get(self.active)
                        .is_some_and(|session| session.terminal.is_alt_buffer_active())
                {
                    self.block_search = None;
                    self.block_menu = None;
                    self.block_clear_confirm = None;
                }
                let refresh_block_search = is_active_output
                    && self.block_search.as_ref().is_some_and(|state| {
                        state.session_id == id
                            && self.sessions.get(self.active).is_some_and(|sess| {
                                sess.id == id
                                    && BlockSearchZoneVersion::from_terminal(&sess.terminal)
                                        != state.zone_version
                            })
                    });
                // Output may have moved the shell's cwd; let the next periodic
                // tick reconcile the session snapshot.
                self.session_dirty = true;
                if is_active_output && self.search.is_open {
                    self.search_dirty = true;
                }

                // Desktop notifications requested via OSC 9 / OSC 777.
                if let Some((title, body)) = notifications.into_iter().next() {
                    let now = std::time::Instant::now();
                    let allowed = self.last_notification_at.is_none_or(|last| {
                        now.duration_since(last) >= std::time::Duration::from_secs(2)
                    });
                    if allowed {
                        self.last_notification_at = Some(now);
                        enqueue_desktop_notification(title, body);
                    }
                }

                // Desktop notification when a long-running command finishes
                // (duration measured by the OSC 133 bookkeeping). Mirrors the
                // block-view gating in anvil: an opt-out config flag plus a
                // duration threshold, with iced window focus standing in for
                // its background-block check — a command the user is actively
                // watching (focused window, active pane) never toasts.
                if self.config.notify_long_blocks && !(self.focused && is_active_output) {
                    for completed in &completed_commands {
                        if let Some(ms) = completed.duration_ms {
                            if ms >= self.config.notify_long_block_threshold_ms {
                                jterm_core::notify::long_block_finished(
                                    &completed.command,
                                    completed.exit_code.unwrap_or(0),
                                    ms,
                                );
                            }
                        }
                    }
                }

                // A finished command may have changed branch/dirty state;
                // re-probe the pane's git meta now instead of waiting for the
                // next periodic tick (same immediate refresh anvil does).
                // Both the pane-header strip and the bottom bar read the cache.
                if !completed_commands.is_empty()
                    && (self.config.show_repo_strip || self.config.bottom_bar)
                {
                    if let Some(sess) = self.session_by_identity(id, fd) {
                        sess.git_meta_cache = sess.git_meta();
                    }
                }

                // Clipboard set/query via OSC 52. The query path reads the
                // system clipboard asynchronously and writes the base64
                // response back to the originating session's PTY.
                let mut tasks: Vec<Task<Message>> = Vec::new();
                if refresh_block_search {
                    tasks.push(self.begin_block_search_rebuild());
                }
                if let Some(text) = clip_set {
                    tasks.push(iced::clipboard::write(text));
                }
                if clip_query && self.config.allow_clipboard_read {
                    let start_read = if let Some(sess) = self.session_by_identity(id, fd) {
                        if sess.clipboard_read_in_flight {
                            false
                        } else {
                            sess.clipboard_read_in_flight = true;
                            true
                        }
                    } else {
                        false
                    };
                    if start_read {
                        tasks.push(
                            iced::clipboard::read().map(move |c| Message::Osc52Query(id, fd, c)),
                        );
                    } else if let Some(sess) = self.session_by_identity(id, fd) {
                        // OSC 52 has no structured busy status; an empty response
                        // is the interoperable refusal while another read runs.
                        sess.terminal.respond_osc52_clipboard("");
                        sess.flush_responses();
                    }
                } else if clip_query {
                    // An empty OSC 52 response reports that clipboard reads are
                    // unavailable without exposing host clipboard contents.
                    if let Some(sess) = self.session_by_identity(id, fd) {
                        sess.terminal.respond_osc52_clipboard("");
                        sess.flush_responses();
                    }
                }

                // OSC 5522 extended-clipboard read requests. iced's clipboard is
                // text-only, so we advertise a text MIME and serve text reads via
                // an async clipboard read; non-text MIME types get ENOSYS.
                for kind in clip_requests {
                    if !self.config.allow_clipboard_read {
                        if let Some(sess) = self.session_by_identity(id, fd) {
                            let resp = osc_5522_packet("type=read:status=EPERM", None);
                            sess.terminal.output_buffer.extend_from_slice(&resp);
                            sess.flush_responses();
                            sess.refresh();
                        }
                        continue;
                    }
                    match kind {
                        terminal::ClipboardReadKind::MimeList => {
                            if let Some(sess) = self.session_by_identity(id, fd) {
                                let resp = sess
                                    .terminal
                                    .build_paste_event(&["text/plain;charset=utf-8".to_string()]);
                                sess.terminal.output_buffer.extend_from_slice(&resp);
                                sess.flush_responses();
                                sess.refresh();
                            }
                        }
                        terminal::ClipboardReadKind::MimeData(mime) => {
                            if mime.starts_with("text") {
                                let start_read =
                                    if let Some(sess) = self.session_by_identity(id, fd) {
                                        if sess.clipboard_read_in_flight {
                                            false
                                        } else {
                                            sess.clipboard_read_in_flight = true;
                                            true
                                        }
                                    } else {
                                        false
                                    };
                                if start_read {
                                    tasks.push(iced::clipboard::read().map(move |c| {
                                        Message::Osc5522Data(id, fd, mime.clone(), c)
                                    }));
                                } else if let Some(sess) = self.session_by_identity(id, fd) {
                                    let resp = osc_5522_packet("type=read:status=EBUSY", None);
                                    sess.terminal.output_buffer.extend_from_slice(&resp);
                                    sess.flush_responses();
                                    sess.refresh();
                                }
                            } else if let Some(sess) = self.session_by_identity(id, fd) {
                                let resp = osc_5522_packet("type=read:status=ENOSYS", None);
                                sess.terminal.output_buffer.extend_from_slice(&resp);
                                sess.flush_responses();
                                sess.refresh();
                            }
                        }
                    }
                }

                // Mirror captured command output into jsh's execution journal
                // (no-op unless JSH_EXECUTION_JOURNAL is enabled).
                for completed in &completed_commands {
                    let Some(id) = completed.id.clone() else {
                        continue;
                    };
                    if let Err(error) = jterm_core::execution_journal::submit(
                        jterm_core::execution_journal::CompletedExecution {
                            id,
                            output: completed.output.clone(),
                            output_available: completed.output_available,
                            truncated: completed.truncated,
                            total_bytes: completed.total_bytes,
                        },
                    ) {
                        log::warn!("cannot journal completed command output: {error:?}");
                    }
                }

                // Persist each finished command into the family-shared JSONL
                // history index (anvil/forge file format) so the
                // Ctrl+Shift+H picker recalls it across restarts. Writes go
                // through jterm_core's bounded background writer; unsafe
                // reconstructions (multiline heredoc text) are skipped rather
                // than rejected noisily.
                if !completed_commands.is_empty() {
                    if let Some(path) = self.config.resolved_command_history_path() {
                        if let Err(error) = persistence::prepare_command_history_path(&path, true) {
                            log::warn!("unsafe command-history path rejected: {error}");
                        } else {
                            let max_entries = self.config.command_history_max_entries as usize;
                            let cwd = self
                                .session_by_identity(id, fd)
                                .and_then(|sess| sess.cwd_cache.clone().or_else(|| sess.cwd()))
                                .filter(|cwd| history_picker::sanitized_cwd(cwd).is_some());
                            let end_time_ms = std::time::SystemTime::now()
                                .duration_since(std::time::UNIX_EPOCH)
                                .ok()
                                .and_then(|duration| u64::try_from(duration.as_millis()).ok());
                            for completed in &completed_commands {
                                let Some(command) =
                                    history_picker::sanitized_command(&completed.command)
                                else {
                                    continue;
                                };
                                if let Err(error) = jterm_core::command_history::enqueue(
                                    &path,
                                    max_entries,
                                    command,
                                    cwd.as_deref(),
                                    completed.exit_code.unwrap_or(0),
                                    end_time_ms,
                                ) {
                                    log::warn!("command history: {error}");
                                }
                            }
                        }
                    }
                }
                for completed in &completed_commands {
                    self.agent.handle_completed(id, completed);
                }
                if let Some(task) = self.agent_drive_task() {
                    tasks.push(task);
                }
                if !tasks.is_empty() {
                    return Task::batch(tasks);
                }
            }
            Message::JshChecked(status) => {
                if let Some(error) = &status.error {
                    log::info!("jsh update check unavailable: {error}");
                }
                if let Some(other) = &status.shadowed_by {
                    // Some other binary named jsh, earlier on PATH. Installing
                    // does not fix PATH order, so the installer explains it in
                    // the session; here it is only worth a log line.
                    log::warn!("PATH resolves jsh to {other}, which frost does not manage");
                }
                self.jsh_prompt = jterm_core::jsh_install::prompt_for(&status);
                if let Some(prompt) = &self.jsh_prompt {
                    log::info!("jsh notice: {}", prompt.banner_title());
                }
            }
            Message::JshInstall => self.install_or_update_jsh(),
            Message::RemotePickerClose => self.remote_picker = None,
            Message::RemotePickerConnect(index) => {
                self.remote_picker = None;
                self.connect_remote_host(index);
            }
            Message::RemoteHostName(index, name) => {
                if let Some(host) = self.config.remote_hosts.get_mut(index) {
                    host.name = name;
                    self.config_dirty = true;
                }
            }
            Message::RemoteHostHost(index, value) => {
                if let Some(host) = self.config.remote_hosts.get_mut(index) {
                    host.host = value;
                    self.config_dirty = true;
                }
            }
            Message::RemoteHostUser(index, user) => {
                if let Some(host) = self.config.remote_hosts.get_mut(index) {
                    // Blank clears the login/exec user rather than storing "".
                    host.user = Some(user).filter(|u| !u.trim().is_empty());
                    self.config_dirty = true;
                }
            }
            Message::RemoteHostDocker(index, docker) => {
                if let Some(host) = self.config.remote_hosts.get_mut(index) {
                    host.docker = docker;
                    self.config_dirty = true;
                }
            }
            Message::RemoteHostDeploy(index, deploy) => {
                if let Some(host) = self.config.remote_hosts.get_mut(index) {
                    host.deploy = deploy;
                    self.config_dirty = true;
                }
            }
            Message::RemoteHostAdd => {
                // Template the inline editor fills in. deploy "persist" so a
                // fresh host brings jsh along by default; validation stays
                // advisory until the host field is typed.
                self.config
                    .remote_hosts
                    .push(jterm_core::jsh_remote::RemoteHostConfig {
                        name: String::new(),
                        host: String::new(),
                        user: None,
                        docker: false,
                        remote_shell: "jsh".to_string(),
                        session: None,
                        ssh_args: Vec::new(),
                        deploy: "persist".to_string(),
                        deploy_artifact: None,
                    });
                self.config_dirty = true;
            }
            Message::RemoteHostRemove(index) => {
                if index < self.config.remote_hosts.len() {
                    self.config.remote_hosts.remove(index);
                    self.config_dirty = true;
                }
            }
            Message::JshNoticeDismiss => self.jsh_notice_dismissed = true,
            Message::AgentClose => self.agent.close(),
            Message::TaskPanelToggle => {
                self.toggle_task_panel();
            }
            Message::TaskSelect(task_id) => {
                self.task_panel.selected = Some(task_id);
                self.task_panel.follow_up.clear();
            }
            Message::TaskHide(task_id) => {
                if let Err(error) = self.task_manager.archive(task_id) {
                    self.push_toast(error.to_string(), ToastKind::Warning);
                } else if self.task_panel.selected == Some(task_id) {
                    self.task_panel.selected = None;
                }
            }
            Message::TaskStartCodex(task_id) => self.task_start_codex(task_id),
            Message::TaskCancelCodex(task_id) => {
                if let Err(error) = self.agent_runtime.cancel(task_id) {
                    self.push_toast(error.to_string(), ToastKind::Warning);
                }
            }
            Message::TaskFinishCodex(task_id) => {
                if let Err(error) = self.agent_runtime.finish_codex(&self.task_manager, task_id) {
                    self.push_toast(error.to_string(), ToastKind::Warning);
                }
            }
            Message::TaskFollowUpInput(value) => {
                if value.len() <= agent_task::NATIVE_AGENT_FOLLOW_UP_MAX_BYTES {
                    self.task_panel.follow_up = value;
                }
            }
            Message::TaskFollowUpSend(task_id) => {
                let text = self.task_panel.follow_up.clone();
                match self.agent_runtime.prompt_codex(
                    &self.task_manager,
                    task_id,
                    &text,
                    agent_task_ui::prompt_policy(&self.config),
                ) {
                    Ok(()) => self.task_panel.follow_up.clear(),
                    Err(error) => self.push_toast(error.to_string(), ToastKind::Warning),
                }
            }
            Message::TaskApprovalDeny(task_id, approval_id) => {
                if let Err(error) = self.agent_runtime.decide_approval(
                    task_id,
                    approval_id,
                    agent_task::ApprovalDecision::Deny { reason: None },
                ) {
                    self.push_toast(error.to_string(), ToastKind::Warning);
                }
            }
            Message::TaskDiffOpen(task_id) => {
                if let Some(task) = self.task_manager.get(task_id) {
                    let result = self
                        .task_panel
                        .diff
                        .request_from(task.worktree_path.clone(), task.base_commit.clone());
                    if let Err(error) = result {
                        self.push_toast(error.to_string(), ToastKind::Warning);
                    }
                }
            }
            Message::TaskDiffClose => {
                self.task_panel.diff.is_open = false;
            }
            Message::TaskValidationStart(task_id) => self.task_start_validation(task_id),
            Message::TaskMarkComplete(task_id) => {
                match self.task_manager.complete_after_validation(task_id) {
                    Ok(()) => self.push_toast("Task marked complete", ToastKind::Success),
                    Err(error) => self.push_toast(error.to_string(), ToastKind::Warning),
                }
            }
            Message::TaskTerminalOpen(task_id) => self.task_open_terminal(task_id),
            Message::TaskTick => self.tasks_tick(),
            Message::AgentInput(value) => self.agent.input = value,
            Message::AgentSubmit => {
                self.agent.submit_input();
                if let Some(task) = self.agent_drive_task() {
                    return task;
                }
            }
            Message::AgentApprove(id) => {
                if let Some(task) = self.agent_run_approved(id, None) {
                    return task;
                }
            }
            Message::AgentEditStart(id, command) => {
                match crate::review_text::validate_single_line(
                    &command,
                    crate::review_text::MAX_AGENT_COMMAND_BYTES,
                ) {
                    Ok(_) => self.agent.edit = Some((id, command)),
                    Err(error) => {
                        self.agent.status = format!("Agent edit rejected: {error}");
                        self.agent.edit = None;
                    }
                }
            }
            Message::AgentEditInput(value) => {
                if let Some((_, buffer)) = self.agent.edit.as_mut() {
                    if value.is_empty() {
                        buffer.clear();
                    } else {
                        match crate::review_text::validate_single_line(
                            &value,
                            crate::review_text::MAX_AGENT_COMMAND_BYTES,
                        ) {
                            Ok(_) => *buffer = value,
                            Err(error) => {
                                buffer.clear();
                                self.agent.status =
                                    format!("Agent edit cleared before display: {error}");
                            }
                        }
                    }
                }
            }
            Message::AgentEditCancel => self.agent.edit = None,
            Message::AgentEditApprove(id) => {
                let edited = self
                    .agent
                    .edit
                    .take()
                    .filter(|(edit_id, _)| *edit_id == id)
                    .map(|(_, buffer)| buffer);
                if let Some(task) = self.agent_run_approved(id, edited) {
                    return task;
                }
            }
            Message::AgentReject(id) => {
                self.agent.reject(id);
                if let Some(task) = self.agent_drive_task() {
                    return task;
                }
            }
            Message::AgentModelDelta(identity, fragment) => {
                self.agent.model_delta(identity, &fragment);
            }
            Message::AgentModelReply(identity, result) => {
                self.agent.model_reply(identity, result);
                if let Some(task) = self.agent_drive_task() {
                    return task;
                }
            }
            Message::AgentContinueTask => {
                self.agent.continue_task();
            }
            Message::AgentNewTask => {
                self.agent.new_task();
            }
            Message::AgentClearContext => {
                self.agent.last_manual_completed = None;
            }
            Message::SetAiEnabled(enabled) => {
                self.config.ai_enabled = enabled;
                self.config_dirty = true;
            }
            Message::SetAiProvider(provider) => {
                self.config.ai_provider = provider;
                self.config_dirty = true;
            }
            Message::SetAiModel(model) => {
                self.config.ai_model = model;
                self.config_dirty = true;
            }
            Message::SetAiBaseUrl(url) => {
                self.config.ai_base_url = url;
                self.config_dirty = true;
            }
            Message::SetAiMaxTokens(tokens) => {
                self.config.ai_max_tokens = tokens.clamp(64, 32_768);
                self.config_dirty = true;
            }
            Message::SetAiTemperature(raw) => {
                // Keep the raw editing text; only a valid value reaches config.
                self.config.ai_temperature = raw
                    .trim()
                    .parse::<f32>()
                    .ok()
                    .filter(|t| t.is_finite() && (0.0..=2.0).contains(t));
                self.ai_temperature_draft = raw;
                self.config_dirty = true;
            }
            Message::SetAiRedactSecrets(redact) => {
                self.config.ai_redact_secrets = redact;
                self.config_dirty = true;
            }
            Message::SetAiShareCommandContext(share) => {
                self.config.ai_share_command_context = share;
                self.config_dirty = true;
            }
            Message::SetExperimentalTaskSidebar(enabled) => {
                self.config.experimental_task_sidebar = enabled;
                self.config_dirty = true;
                if !enabled && self.sidebar_panel == SidebarPanel::Tasks {
                    self.sidebar_panel = SidebarPanel::Tabs;
                }
            }
            Message::SetAiStream(stream) => {
                self.config.ai_stream = stream;
                self.config_dirty = true;
            }
            Message::SetAiKeyFile(path) => {
                // Keep the raw editing text; only fully-blank clears the key.
                self.config.ai_api_key_file = Some(path).filter(|p| !p.trim().is_empty());
                self.config_dirty = true;
            }
            Message::SetAiKeyDraft(value) => {
                self.ai_key_draft = value;
            }
            Message::StoreAiKey => {
                let key = self.ai_key_draft.trim().to_string();
                if key.is_empty() {
                    self.push_toast("Paste an API key first", ToastKind::Warning);
                } else if self.config_write_blocked {
                    self.push_toast(
                        "API key not stored: config cannot safely record its credential-file path",
                        ToastKind::Warning,
                    );
                } else {
                    // Same write target rule as forge: the configured path,
                    // otherwise the per-app default. The environment override
                    // stays read-only and is never chosen as a write target.
                    let path = self
                        .config
                        .ai_api_key_file
                        .clone()
                        .filter(|p| !p.trim().is_empty())
                        .unwrap_or_else(jterm_core::ai::default_api_key_path);
                    match persistence::write_api_key_file(&path, &key) {
                        Ok(()) => {
                            let mut candidate = self.config.clone();
                            candidate.ai_api_key_file = Some(path.clone());
                            let candidate = candidate.normalized();
                            let needs_config_save = self.config_dirty
                                || candidate.ai_api_key_file != self.config.ai_api_key_file;
                            self.ai_key_draft.clear();
                            if needs_config_save {
                                match candidate.save_if_unchanged(self.config_revision.as_ref()) {
                                    Ok(revision) => {
                                        self.config = candidate;
                                        self.config_revision = Some(revision);
                                        self.config_dirty = false;
                                        self.push_toast(
                                            "API key stored (0600) and config path saved",
                                            ToastKind::Success,
                                        );
                                    }
                                    Err(error) => {
                                        let message = error.to_string();
                                        if let Some(revision) = error.committed_revision().cloned()
                                        {
                                            // The pointer-bearing config is
                                            // already visible. Keep memory in
                                            // lockstep with it and retry the
                                            // uncertain durability boundary.
                                            self.config = candidate;
                                            self.config_revision = Some(revision);
                                            self.config_dirty = true;
                                        }
                                        self.note_config_save_error(&error);
                                        self.push_toast(
                                            format!(
                                                "Key file was stored securely, but config persistence needs attention: {message}"
                                            ),
                                            ToastKind::Warning,
                                        );
                                    }
                                }
                            } else {
                                self.push_toast("API key stored (0600)", ToastKind::Success);
                            }
                        }
                        Err(error) => self
                            .push_toast(format!("API key not saved: {error}"), ToastKind::Warning),
                    }
                }
            }
            Message::SetAgentMaxTurns(turns) => {
                self.config.agent_max_turns = turns.clamp(1, 100);
                self.config_dirty = true;
            }
            Message::Osc52Query(id, fd, content) => {
                let allow_clipboard_read = self.config.allow_clipboard_read;
                if let Some(sess) = self.session_by_identity(id, fd) {
                    sess.clipboard_read_in_flight = false;
                    let content = content
                        .as_deref()
                        .filter(|value| {
                            allow_clipboard_read && value.len() <= MAX_CLIPBOARD_RESPONSE_BYTES
                        })
                        .unwrap_or("");
                    sess.terminal.respond_osc52_clipboard(content);
                    sess.flush_responses();
                    sess.refresh();
                }
            }
            Message::Osc5522Data(id, fd, mime, content) => {
                let allow_clipboard_read = self.config.allow_clipboard_read;
                if let Some(sess) = self.session_by_identity(id, fd) {
                    sess.clipboard_read_in_flight = false;
                    let data = content.unwrap_or_default();
                    let resp = if !allow_clipboard_read {
                        osc_5522_packet("type=read:status=EPERM", None)
                    } else if data.len() > MAX_CLIPBOARD_RESPONSE_BYTES {
                        osc_5522_packet("type=read:status=EFBIG", None)
                    } else if data.is_empty() {
                        osc_5522_packet("type=read:status=ENOSYS", None)
                    } else {
                        clipboard_5522_response_for_mime(&mime, data.as_bytes())
                    };
                    sess.terminal.output_buffer.extend_from_slice(&resp);
                    sess.flush_responses();
                    sess.refresh();
                }
            }
            Message::PtyExited(id, fd, _code) => {
                if let Some(index) = self
                    .sessions
                    .iter()
                    .position(|session| session.id == id && session.master_fd == fd)
                {
                    // A task-bound terminal (Agent or validation) reports its
                    // real child exit status to the reducer and stays open so
                    // its transcript remains reviewable; the task card drives
                    // the lifecycle from here. Ordinary sessions still close.
                    let session_key = agent_task_ui::terminal_session_id(id);
                    let task_bound = self
                        .task_manager
                        .task_for_terminal_session(&session_key)
                        .is_some();
                    let exit_code = self.sessions[index].pty.exited_code();
                    self.task_manager
                        .handle_terminal_session_exit(&session_key, exit_code);
                    if task_bound {
                        self.sessions[index].hold_after_exit = true;
                        self.push_toast(
                            "Task terminal exited; its transcript stays open for review",
                            ToastKind::Info,
                        );
                        return Task::none();
                    }
                    return self.close_session(index);
                }
            }
            Message::Key(event) => {
                if let keyboard::Event::KeyPressed {
                    key,
                    location,
                    modifiers,
                    text,
                    ..
                } = event
                {
                    // The destructive block confirmation is the top-most modal.
                    // Enter confirms, Esc cancels, and every other key is swallowed.
                    if self.block_clear_confirm.is_some() {
                        if matches!(key, keyboard::Key::Named(keyboard::key::Named::Enter)) {
                            return self.confirm_block_clear();
                        }
                        if matches!(key, keyboard::Key::Named(keyboard::key::Named::Escape)) {
                            self.block_clear_confirm = None;
                        }
                        return Task::none();
                    }
                    if self.tab_close_confirm.is_some() {
                        if matches!(key, keyboard::Key::Named(keyboard::key::Named::Enter)) {
                            if let Some((id, _, pending)) = self.tab_close_confirm.take() {
                                if let Some(index) = self.session_index_by_id(id) {
                                    return self.execute_pending_close(index, pending);
                                }
                            }
                        } else if matches!(key, keyboard::Key::Named(keyboard::key::Named::Escape))
                        {
                            self.tab_close_confirm = None;
                        }
                        return Task::none();
                    }
                    if self.block_menu.is_some() {
                        if matches!(key, keyboard::Key::Named(keyboard::key::Named::Escape)) {
                            self.block_menu = None;
                        }
                        return Task::none();
                    }
                    // The sidebar's file dialogs are modal over the terminal:
                    // Enter submits (the focused input also reports it via
                    // on_submit), Esc backs out, and no other key reaches the
                    // PTY while one is up.
                    if self.sidebar_dialog.is_some() {
                        if matches!(key, keyboard::Key::Named(keyboard::key::Named::Enter)) {
                            return self.submit_sidebar_dialog();
                        }
                        if matches!(key, keyboard::Key::Named(keyboard::key::Named::Escape)) {
                            self.sidebar_dialog = None;
                        }
                        return Task::none();
                    }
                    if self.sidebar_delete_confirm.is_some() {
                        if matches!(key, keyboard::Key::Named(keyboard::key::Named::Enter)) {
                            return self.confirm_sidebar_delete();
                        }
                        if matches!(key, keyboard::Key::Named(keyboard::key::Named::Escape)) {
                            self.sidebar_delete_confirm = None;
                        }
                        return Task::none();
                    }
                    // The sidebar file menu is pointer-driven; keep every
                    // keypress out of the PTY while it is visible.
                    if self.sidebar_menu.is_some() {
                        if matches!(key, keyboard::Key::Named(keyboard::key::Named::Escape)) {
                            self.sidebar_menu = None;
                        }
                        return Task::none();
                    }
                    // Apart from the rename editor (whose keys the focused text
                    // input captures before this point), the tab menu is
                    // pointer-driven; keep every other keypress out of the PTY
                    // while it is visible.
                    if self.tab_menu.is_some() {
                        if matches!(key, keyboard::Key::Named(keyboard::key::Named::Escape)) {
                            // Esc backs out of the rename first, then the menu.
                            if self.tab_rename.take().is_none() {
                                self.tab_menu = None;
                            }
                        }
                        return Task::none();
                    }
                    // Tab switcher swallows keys while open (Enter to jump,
                    // arrows to move, Esc/Ctrl+Shift+L to dismiss). Handle before
                    // generic keybindings so its toggle shortcut wins.
                    if self.tab_switcher.is_some() {
                        if let Some(task) =
                            self.handle_tab_switcher_key(&key, modifiers, text.as_deref())
                        {
                            return task;
                        }
                    }
                    // The remote host picker owns the keyboard the same way
                    // (Enter connects, arrows move, Esc/Ctrl+Shift+S dismiss).
                    if self.remote_picker.is_some() {
                        if let Some(task) = self.handle_remote_picker_key(&key, modifiers) {
                            return task;
                        }
                    }
                    // The history picker owns the keyboard the same way (Enter
                    // types the selection, Esc/Ctrl+Shift+H dismisses).
                    if self.history_picker.is_some() {
                        if let Some(task) =
                            self.handle_history_picker_key(&key, modifiers, text.as_deref())
                        {
                            return task;
                        }
                    }
                    // The block search picker owns the keyboard the same way
                    // (Enter selects and reveals the hit's zone, Esc/
                    // Ctrl+Alt+F dismiss). It MUST consume keys before the
                    // encode_key path below: PTY-bound input clears
                    // the block selection, and the picker's own keystrokes must
                    // never do that.
                    if self.block_search.is_some() {
                        if let Some(task) =
                            self.handle_block_search_key(&key, modifiers, text.as_deref())
                        {
                            return task;
                        }
                    }
                    if self.help_open || self.debug_open {
                        let active_overlay_toggle = (self.help_open
                            && chrome_shortcut(&key, modifiers) == Some(ChromeShortcut::Help))
                            || (self.debug_open
                                && chrome_shortcut(&key, modifiers) == Some(ChromeShortcut::Debug));
                        if active_overlay_toggle
                            || matches!(key, keyboard::Key::Named(keyboard::key::Named::Escape))
                        {
                            self.help_open = false;
                            self.debug_open = false;
                        }
                        return Task::none();
                    }
                    // Input-owning overlays route before global keybindings so a
                    // shortcut or printable key cannot mutate the hidden terminal.
                    if let Some(task) = self.handle_config_panel_key(&key, modifiers) {
                        return task;
                    }
                    if let Some(task) = self.handle_palette_key(&key, modifiers, text.as_deref()) {
                        return task;
                    }
                    if let Some(task) = self.handle_search_replace_key(&key, modifiers) {
                        return task;
                    }
                    if self.handle_search_key(&key, modifiers, text.as_deref()) {
                        return Task::none();
                    }
                    // Escape dismisses a visible block selection locally. It
                    // must not also reach the PTY: that would both clear the
                    // UI state and send an unrelated cancel key to the child.
                    let no_modifier = !modifiers.shift()
                        && !modifiers.control()
                        && !modifiers.alt()
                        && !modifiers.logo();
                    // Zone metadata is bounded independently of the UI state.
                    // Reconcile a non-empty selection before deciding whether
                    // block mode owns this key, so an asynchronously evicted
                    // stale-only selection cannot swallow one Enter/Down.
                    let block_key_context = self.sessions.get_mut(self.active).map(|sess| {
                        let has_selection = if sess.block_selection.is_empty() {
                            false
                        } else {
                            let ids: Vec<u64> = sess
                                .terminal
                                .command_zones
                                .iter()
                                .map(|zone| zone.id)
                                .collect();
                            sess.block_selection.retain(&ids)
                        };
                        (
                            has_selection,
                            sess.terminal.is_alt_buffer_active(),
                            sess.terminal.is_command_running(),
                        )
                    });
                    if matches!(key, keyboard::Key::Named(keyboard::key::Named::Escape))
                        && block_key_context.is_some_and(
                            |(has_selection, alt_screen, command_running)| {
                                block_escape_owns_key(
                                    self.config.block_mode,
                                    has_selection,
                                    alt_screen,
                                    command_running,
                                    no_modifier,
                                )
                            },
                        )
                    {
                        if let Some(sess) = self.sessions.get_mut(self.active) {
                            sess.block_selection.clear();
                            sess.refresh();
                        }
                        return Task::none();
                    }
                    // Warp-style block selection owns the unmodified arrows
                    // only once a selection exists: Up/Down collapses it to
                    // one neighboring block, Shift+Up/Down moves the active
                    // range edge, Ctrl+Shift+Up/Down reveals that card's top /
                    // bottom, and Enter reinputs the selected commands.
                    // A running/full-screen program keeps every key, including
                    // Enter, so block chrome can never steal its stdin.
                    let shift_only = modifiers.shift()
                        && !modifiers.control()
                        && !modifiers.alt()
                        && !modifiers.logo();
                    let ctrl_shift_only = modifiers.shift()
                        && modifiers.control()
                        && !modifiers.alt()
                        && !modifiers.logo();
                    let selection_owns_keys = block_key_context.is_some_and(
                        |(has_selection, alt_screen, command_running)| {
                            block_selection_owns_keys(
                                self.config.block_mode,
                                has_selection,
                                alt_screen,
                                command_running,
                            )
                        },
                    );
                    if selection_owns_keys {
                        match &key {
                            keyboard::Key::Named(keyboard::key::Named::ArrowUp) if no_modifier => {
                                return self.block_select_step(true);
                            }
                            keyboard::Key::Named(keyboard::key::Named::ArrowDown)
                                if no_modifier =>
                            {
                                return self.block_select_step(false);
                            }
                            keyboard::Key::Named(keyboard::key::Named::ArrowUp) if shift_only => {
                                return self.block_extend_selection_step(true);
                            }
                            keyboard::Key::Named(keyboard::key::Named::ArrowDown) if shift_only => {
                                return self.block_extend_selection_step(false);
                            }
                            keyboard::Key::Named(keyboard::key::Named::ArrowUp)
                                if ctrl_shift_only =>
                            {
                                if let Some(id) = self
                                    .sessions
                                    .get(self.active)
                                    .and_then(|sess| sess.block_selection.active())
                                {
                                    return self.block_reveal_edge(id, false);
                                }
                            }
                            keyboard::Key::Named(keyboard::key::Named::ArrowDown)
                                if ctrl_shift_only =>
                            {
                                if let Some(id) = self
                                    .sessions
                                    .get(self.active)
                                    .and_then(|sess| sess.block_selection.active())
                                {
                                    return self.block_reveal_edge(id, true);
                                }
                            }
                            keyboard::Key::Named(keyboard::key::Named::Enter) if no_modifier => {
                                // A selection can be created after the user has
                                // already typed at the live prompt. Only an OSC
                                // 133-confirmed empty prompt owned by the shell
                                // may be replaced with recalled commands.
                                let prompt_status = self
                                    .sessions
                                    .get_mut(self.active)
                                    .map(Session::agent_prompt_status);
                                if prompt_status.is_some_and(|status| {
                                    block_enter_reinputs_selection(selection_owns_keys, status)
                                }) {
                                    return self.block_reinput_selected_commands_task();
                                }
                                // Dirty/untrusted prompt state: exit block
                                // selection and let Enter follow the ordinary
                                // keybinding/PTY path, preserving visible input.
                                if let Some(sess) = self.sessions.get_mut(self.active) {
                                    sess.block_selection.clear();
                                    sess.refresh();
                                }
                            }
                            _ => {}
                        }
                    }
                    if let Some(task) = self.handle_keybinding(&key, modifiers) {
                        return task;
                    }
                    if let Some(task) = self.handle_tab_shortcut(&key, modifiers) {
                        return task;
                    }
                    if self.handle_scroll_shortcut(&key, modifiers) {
                        return Task::none();
                    }
                    let mut dead_input = false;
                    let Some(sess) = self.sessions.get_mut(self.active) else {
                        return Task::none();
                    };
                    let app_cursor = sess.terminal.is_application_cursor_keys();
                    let enh = KeyboardEnhancements {
                        kitty_flags: sess.terminal.keyboard_enhancement_flags(),
                        modify_other_keys: sess.terminal.xterm_modify_other_keys(),
                        format_other_keys: sess.terminal.xterm_format_other_keys(),
                        report_all_keys: sess.terminal.is_report_all_keys_enabled(),
                        application_keypad: sess.terminal.is_application_keypad(),
                    };
                    if let Some(bytes) =
                        encode_key(&key, location, modifiers, text.as_deref(), app_cursor, enh)
                    {
                        if sess.transcript_read_only() {
                            // Held-open task transcript: the PTY is gone, so
                            // typing becomes a visible no-op instead of an EIO
                            // in the log.
                            dead_input = true;
                        } else {
                            // Typing into the shell dismisses the block selection
                            // (any PTY-bound key, not Escape specifically).
                            sess.block_selection.clear();
                            sess.terminal.scroll_to_bottom();
                            sess.projection_view_state.scroll_to_bottom();
                            sess.write_pty(&bytes);
                            sess.refresh();
                        }
                    }
                    if dead_input {
                        self.hint_read_only_transcript();
                    }
                }
            }
            Message::Ime(event) => {
                use iced::advanced::input_method::Event as Ime;
                if !self.terminal_input_active() {
                    return Task::none();
                }
                let mut dead_input = false;
                let Some(sess) = self.sessions.get_mut(self.active) else {
                    return Task::none();
                };
                match event {
                    Ime::Opened => {
                        sess.terminal.ime_enabled = true;
                    }
                    Ime::Closed => {
                        sess.terminal.ime_enabled = false;
                        sess.terminal.clear_preedit();
                        sess.refresh();
                    }
                    Ime::Preedit(content, selection) => {
                        sess.terminal.set_preedit(content, selection);
                        sess.refresh();
                    }
                    Ime::Commit(text) => {
                        sess.terminal.clear_preedit();
                        if sess.transcript_read_only() {
                            // Same read-only rule as typed keys.
                            dead_input = true;
                        } else {
                            // PTY-bound input dismisses the block selection, same
                            // as `encode_key` and the paste paths.
                            sess.block_selection.clear();
                            sess.terminal.scroll_to_bottom();
                            sess.projection_view_state.scroll_to_bottom();
                            sess.write_pty(text.as_bytes());
                        }
                        sess.refresh();
                    }
                }
                if dead_input {
                    self.hint_read_only_transcript();
                }
            }
            Message::ModifiersChanged(mods) => {
                self.modifiers = mods;
            }
            Message::SummaryActivate(session_id, activation) => {
                if !self.terminal_mouse_active() {
                    return Task::none();
                }
                return self.handle_summary_activation(session_id, activation);
            }
            Message::MousePane(session_id, input) => {
                if !self.terminal_mouse_active() {
                    // Only an app-owned release may cross modal ownership, and
                    // it still goes to its stable origin pane below. A local or
                    // consumed release merely closes bookkeeping: executing a
                    // hidden click-to-caret/copy after a context menu opened
                    // would mutate the prompt or clipboard behind the overlay.
                    let MouseInput::Release { button, .. } = input else {
                        return Task::none();
                    };
                    let slot = button.slot();
                    match overlay_release_disposition(
                        self.terminal_mouse_gestures[slot],
                        session_id,
                        button,
                    ) {
                        OverlayReleaseDisposition::Reject => return Task::none(),
                        OverlayReleaseDisposition::ClearOnly => {
                            self.terminal_mouse_gestures[slot] = None;
                            if button == MouseButton::Left {
                                self.click_tracker.cancel();
                            }
                            return Task::none();
                        }
                        OverlayReleaseDisposition::DispatchApp => {}
                    }
                }
                // Only a press switches the focused pane. Release/Drag aren't
                // bounds-gated in the widget, so every pane emits them — letting
                // those move focus would let the wrong pane steal it on release.
                if matches!(input, MouseInput::Press { .. }) {
                    let Some(session) = self.session_index_by_id(session_id) else {
                        return Task::none();
                    };
                    if !self.layout().contains_session(session) {
                        return Task::none();
                    }
                    self.set_focus(session);
                    self.session_dirty = true;
                    self.refresh_active_context();
                }
                return self.handle_mouse(session_id, input);
            }
            Message::ImageDropped(path) => {
                if !self.terminal_input_active() {
                    self.push_toast(
                        "Image drop ignored while another panel owns input",
                        ToastKind::Info,
                    );
                    return Task::none();
                }
                match image_drop::prompt_payload(&[path]) {
                    Ok(payload) => {
                        let Some(id) = self.sessions.get(self.active).map(|session| session.id)
                        else {
                            return Task::none();
                        };
                        return Task::done(Message::PromptInsert(id, payload));
                    }
                    Err(error) => {
                        self.push_toast(format!("Image drop rejected: {error}"), ToastKind::Warning)
                    }
                }
            }
            Message::Pasted(id, Some(text)) => {
                match crate::review_text::sanitize_prompt_payload(
                    &text,
                    MAX_PTY_WRITE_QUEUE_BYTES,
                ) {
                    Ok(text) => {
                        self.write_paste_to_session(
                            id,
                            &text,
                            PastePolicy::clipboard(UnbracketedMultiline::SendVerbatim),
                            false,
                        );
                    }
                    Err(error) => self.push_toast(
                        format!(
                            "Paste rejected: this build has no safe Unicode-risk confirmation ({error})"
                        ),
                        ToastKind::Warning,
                    ),
                }
            }
            Message::Pasted(_, None) => {}
            Message::PromptInsert(id, text) => {
                match crate::review_text::sanitize_prompt_payload(
                    &text,
                    crate::review_text::MAX_PROMPT_INSERT_BYTES,
                ) {
                    Ok(text) => {
                        self.write_paste_to_session(
                            id,
                            &text,
                            PastePolicy::clipboard(UnbracketedMultiline::SendVerbatim),
                            false,
                        );
                    }
                    Err(error) => self.push_toast(
                        format!("Prompt insertion rejected: {error}"),
                        ToastKind::Warning,
                    ),
                }
            }
            Message::PromptRecall(id, command) => {
                match crate::review_text::sanitize_untrusted_single_line(
                    &command,
                    crate::review_text::MAX_HISTORY_COMMAND_BYTES,
                ) {
                    Ok(command) => {
                        if let Err(reason) = self.session_prompt_replace_ready(id) {
                            self.push_toast(
                                format!("History recall rejected: {reason}"),
                                ToastKind::Warning,
                            );
                            return Task::none();
                        }
                        self.write_paste_to_session(
                            id,
                            &command,
                            PastePolicy::clipboard(UnbracketedMultiline::SendVerbatim),
                            true,
                        );
                    }
                    Err(error) => self.push_toast(
                        format!("History recall rejected: {error}"),
                        ToastKind::Warning,
                    ),
                }
            }
            Message::Resized(size) => {
                self.win_size = size;
                let term_h = self.term_height();
                let term_w = (self.term_width() - terminal_view::SCROLLBAR_WIDTH).max(0.0);
                let (cols, rows) = self.metrics.grid_size(term_w, term_h);
                if cols != self.cols || rows != self.rows {
                    self.cols = cols;
                    self.rows = rows;
                    // Apply either full-tab or pane dimensions exactly once.
                    self.relayout();
                    self.refresh_active_context();
                }
            }
            Message::Focus(f) => {
                if !f {
                    self.cancel_layout_drags();
                    self.hovered_tab = None;
                    self.terminal_mouse_gestures = [None; 3];
                }
                self.focused = f;
                // The blink tick stops while unfocused; leave the cursor solid so
                // it can't get stuck in the "off" half of a blink.
                if !f {
                    self.blink_on = true;
                }
                if let Some(sess) = self.sessions.get_mut(self.active) {
                    if sess.terminal.is_focus_event_mode() {
                        if f {
                            sess.terminal.emit_focus_in();
                        } else {
                            sess.terminal.emit_focus_out();
                        }
                        sess.flush_responses();
                    }
                }
            }
            Message::NewSession => self.new_session(),
            Message::CloseTab(id) => {
                // Closing a tab closes every pane in it.
                if let Some(tab) = self.tab_index_by_id(id) {
                    return self.request_close_tab(tab);
                }
            }
            Message::WindowClose => return self.request_window_close(),
            // Window moves and resizes are compositor operations: we only get
            // to ask, and only while the button that started them is held.
            // `latest()` resolves the single application window.
            Message::WindowDrag => {
                return iced::window::latest().and_then(iced::window::drag);
            }
            Message::WindowResizeDrag(direction) => {
                return iced::window::latest()
                    .and_then(move |id| iced::window::drag_resize(id, direction));
            }
            Message::WindowMinimize => {
                return iced::window::latest().and_then(|id| iced::window::minimize(id, true));
            }
            Message::WindowToggleMaximize => {
                return iced::window::latest().and_then(iced::window::toggle_maximize);
            }
            Message::TabHover(id) => {
                self.hovered_tab = id;
                if tab_drag_hover_left_source(self.dragging_tab, id) {
                    self.tab_drag_moved = true;
                }
                let active_id = self.tabs.get(self.active_tab).map(|tab| tab.id);
                self.tab_drag_hover_since = match (self.dragging_tab, id) {
                    (Some(source), Some(target))
                        if source != target && active_id != Some(target) =>
                    {
                        Some((target, std::time::Instant::now()))
                    }
                    _ => None,
                };
            }
            Message::TabDragStart(id) => {
                if self.tab_index_by_id(id).is_some() {
                    self.pane_drag = None;
                    self.tab_split_drop = None;
                    self.tab_drag_origin = self.tabs.get(self.active_tab).map(|tab| tab.id);
                    self.tab_drag_moved = false;
                    self.tab_drag_hover_since = None;
                    self.dragging_tab = Some(id);
                }
            }
            Message::TabDragEnd(target_id) => {
                if self.pane_drag.is_some() {
                    self.finish_pane_promotion(Some(target_id));
                } else if let Some(source_id) = self.dragging_tab.take() {
                    let origin = self.tab_drag_origin.take();
                    let moved = std::mem::take(&mut self.tab_drag_moved);
                    self.tab_drag_hover_since = None;
                    let source = self.tab_index_by_id(source_id);
                    let target = self.tab_index_by_id(target_id);
                    match tab_drag_release_action(source, target, moved) {
                        TabDragReleaseAction::Activate(tab) => self.activate_tab(tab),
                        TabDragReleaseAction::Reorder { from, to } => {
                            // Reordering moves tabs only; the session vector
                            // and every tab's panes stay as they are.
                            self.reorder_tab(from, to);
                            self.restore_tab_drag_origin(origin);
                            // `reorder_tab` saved before the hover-preview
                            // origin was restored; persist the final focus too.
                            self.save_session_snapshot();
                        }
                        TabDragReleaseAction::RestoreOrigin => {
                            self.restore_tab_drag_origin(origin);
                        }
                    }
                }
                self.tab_drag_origin = None;
                self.tab_drag_moved = false;
                self.tab_drag_hover_since = None;
                self.tab_split_drop = None;
            }
            Message::TabDragMove(point) => {
                self.tab_drag_moved = true;
                self.tab_split_drop = self
                    .tab_split_drag_eligible()
                    .then(|| {
                        self.pane_rects().into_iter().find_map(|pane| {
                            let direction = split_drop_direction(pane.rect, point)?;
                            let target_session_id = self.sessions.get(pane.session)?.id;
                            Some(TabSplitDrop {
                                target_session_id,
                                direction,
                            })
                        })
                    })
                    .flatten();
            }
            Message::TabDragLeavePaneArea => self.tab_split_drop = None,
            Message::TabDragHoverTick => {
                let source_is_plain = self
                    .dragging_tab
                    .and_then(|id| self.tab_index_by_id(id))
                    .is_some_and(|tab| self.tabs[tab].tree.is_leaf());
                let active_id = self.tabs.get(self.active_tab).map(|tab| tab.id);
                let ready = self.tab_drag_hover_since.is_some_and(|(target, since)| {
                    tab_drag_hover_ready(
                        self.dragging_tab,
                        source_is_plain,
                        active_id,
                        self.hovered_tab,
                        target,
                        since.elapsed(),
                    )
                });
                if !source_is_plain
                    || self
                        .tab_drag_hover_since
                        .is_some_and(|(target, _)| active_id == Some(target))
                {
                    self.tab_drag_hover_since = None;
                }
                if ready {
                    let target = self
                        .tab_drag_hover_since
                        .take()
                        .and_then(|(id, _)| self.tab_index_by_id(id));
                    if let Some(target) = target {
                        self.activate_tab(target);
                    }
                }
            }
            Message::TabSplitDrop => {
                self.finish_tab_split_drop();
            }
            Message::TabDragCancel => {
                self.cancel_layout_drags();
            }
            Message::DividerDragStart(divider) => {
                let now = std::time::Instant::now();
                // Double-click on a divider equalizes the panes of its node.
                let double = self.last_divider_press.as_ref().is_some_and(|(prev, d)| {
                    *d == divider
                        && now.duration_since(*prev)
                            < std::time::Duration::from_millis(DIVIDER_DOUBLE_CLICK_MS)
                });
                self.last_divider_press = Some((now, divider.clone()));
                if double {
                    if let Some(PaneTree::Split { ratios, .. }) =
                        self.layout_mut().node_at_path_mut(&divider.path)
                    {
                        equalize_shares(ratios);
                        self.relayout();
                        self.refresh_active_context();
                    }
                }
                self.dragging_divider = Some(divider);
            }
            Message::DividerDragEnd => self.dragging_divider = None,
            Message::DividerHover(divider) => self.hovered_divider = divider,
            Message::DividerDragMove(pt) => {
                if let Some(divider) = self.dragging_divider.clone() {
                    // Locate the dragged divider's owning split node rectangle so
                    // the pointer maps to a fraction of that node's own extent.
                    let Some((axis, node_rect)) =
                        split_node_rect(self.layout(), &divider.path, self.layout_area(), DIVIDER)
                    else {
                        return Task::none();
                    };
                    let local = match axis {
                        Axis::Vertical => (pt.x - node_rect.x) / node_rect.width.max(1.0),
                        Axis::Horizontal => (pt.y - node_rect.y) / node_rect.height.max(1.0),
                    };
                    if let Some(PaneTree::Split { ratios, .. }) =
                        self.layout_mut().node_at_path_mut(&divider.path)
                    {
                        if divider.gap + 1 < ratios.len() {
                            // Pointer fraction minus the children before this gap
                            // gives the dragged child's new share of its pair.
                            let before: f32 = ratios[..divider.gap].iter().sum();
                            let first = local - before;
                            if set_divider_share(ratios, divider.gap, first, true) {
                                self.relayout();
                                self.refresh_active_context();
                            }
                        }
                    }
                }
            }
            Message::PaneDragStart(session_id) => {
                // A press on the header focuses its pane, exactly like a click
                // in the terminal below it. The swap only happens if the
                // pointer is released somewhere else.
                if let Some(session) = self.session_index_by_id(session_id) {
                    if !self.layout().contains_session(session) {
                        return Task::none();
                    }
                    self.set_focus(session);
                    self.session_dirty = true;
                    self.refresh_active_context();
                    self.cancel_tab_drag();
                    self.pane_drag = Some(PaneDrag {
                        session_id,
                        target: None,
                    });
                }
            }
            Message::PaneDragMove(pt) => {
                let source = self
                    .pane_drag
                    .as_ref()
                    .and_then(|drag| self.session_index_by_id(drag.session_id));
                let target = source.and_then(|source| {
                    self.pane_rects()
                        .into_iter()
                        .find(|pane| {
                            pt.x >= pane.rect.x
                                && pt.x < pane.rect.x + pane.rect.width
                                && pt.y >= pane.rect.y
                                && pt.y < pane.rect.y + pane.rect.height
                        })
                        .map(|pane| pane.session)
                        .filter(|hit| *hit != source)
                        .and_then(|hit| self.sessions.get(hit).map(|session| session.id))
                });
                if let Some(drag) = self.pane_drag.as_mut() {
                    drag.target = target;
                }
            }
            Message::PaneDragLeavePaneArea => {
                if let Some(drag) = self.pane_drag.as_mut() {
                    drag.target = None;
                }
            }
            Message::PaneDragEnd => {
                if let Some(drag) = self.pane_drag.take() {
                    if let (Some(source), Some(target)) = (
                        self.session_index_by_id(drag.session_id),
                        drag.target.and_then(|id| self.session_index_by_id(id)),
                    ) {
                        self.swap_pane_sessions(source, target);
                    }
                }
            }
            Message::PanePromoteToTab(after_tab_id) => {
                self.finish_pane_promotion(after_tab_id);
            }
            Message::SidebarDragStart => self.dragging_sidebar = true,
            Message::SidebarDragEnd => self.dragging_sidebar = false,
            Message::SidebarDragMove(pt) => {
                if self.dragging_sidebar {
                    // pt.x is relative to the dock+body row, which starts at the
                    // window's left edge, so it is the desired dock width directly.
                    let w = pt.x.clamp(SIDEBAR_W_MIN, SIDEBAR_W_MAX);
                    if (w - self.dock_width).abs() > f32::EPSILON {
                        self.dock_width = w;
                        self.apply_config();
                    }
                }
            }
            Message::ToggleSidebar => return self.toggle_sidebar(),
            Message::SetSidebarPanel(panel) => {
                self.sidebar_panel = panel;
                // Opening the file tree should reflect the active tab's cwd.
                // That follow is a local-filesystem notion only: with a remote
                // location active, keep its root and just reload it.
                if panel == SidebarPanel::Files {
                    if self.sidebar.location == remote_fs::FsLocation::Local {
                        if let Some(cwd) = self
                            .sessions
                            .get(self.active)
                            .and_then(|s| s.cwd_cache.clone().or_else(|| s.cwd()))
                        {
                            let request =
                                self.sidebar.set_current_dir(std::path::PathBuf::from(cwd));
                            return sidebar_load_task(request);
                        }
                    }
                    return sidebar_load_task(self.sidebar.refresh());
                }
            }
            Message::SetTabPosition(pos) => {
                if self.config.tab_position != pos {
                    self.config.tab_position = pos;
                    self.config_dirty = true;
                    self.sync_tab_position_ui();
                    // Layout chrome changed (top bar shown/hidden, dock width):
                    // recompute the grid.
                    self.apply_config();
                }
            }
            Message::SidebarToggleNode(path) => {
                if let Some(request) = self.sidebar.toggle_node(&path) {
                    return sidebar_load_task(request);
                }
            }
            Message::SidebarInsertPath(path) => {
                // Type the (shell-quoted) path into the active terminal so the
                // sidebar doubles as a path picker.
                // Trailing space so the picked path is ready to extend.
                let mut quoted = jterm_core::process::shell_quote_path(&path.to_string_lossy());
                quoted.push(' ');
                // Through the paste choke point, not a raw write: quoting
                // protects the *shell parser*, but at the input layer a
                // filename carrying a raw CR would still submit the pending
                // line and an embedded `ESC[201~` would still close a paste
                // frame. (An insert, not a recall: the picked path is appended
                // to whatever command the user is composing.)
                return self.type_into_active_pane(quoted);
            }
            Message::SidebarGoParent => {
                if let Some(parent) = self
                    .sidebar
                    .current_dir
                    .parent()
                    .map(std::path::Path::to_path_buf)
                {
                    let request = self.sidebar.set_current_dir(parent);
                    return sidebar_load_task(request);
                }
            }
            Message::SidebarRefresh => {
                let request = self.sidebar.refresh();
                return sidebar_load_task(request);
            }
            Message::SidebarLoaded(result) => {
                self.sidebar.apply_load(result);
            }
            Message::SidebarSetLocation(location) => {
                if location != self.sidebar.location {
                    let generation = self.sidebar.begin_location_change(location.clone());
                    let hosts = self.sidebar.hosts_snapshot().to_vec();
                    return Task::perform(
                        async move {
                            remote_fs::start_dir(&location, &hosts)
                                .map_err(|error| error.to_string())
                        },
                        move |result| Message::SidebarLocationResolved(generation, result),
                    );
                }
            }
            Message::SidebarLocationResolved(generation, start) => {
                if let Some(request) = self.sidebar.resolve_location(generation, start) {
                    return sidebar_load_task(request);
                }
            }
            Message::SidebarHover(hovered) => self.sidebar_hovered = hovered,
            Message::SidebarPointerMoved(position) => self.sidebar_pointer = position,
            Message::SidebarMenuOpen(path, is_dir) => {
                self.sidebar_menu = Some(SidebarMenuState {
                    path,
                    is_dir,
                    // Freeze the anchor now; the pointer moves on toward the
                    // panel as soon as it appears.
                    at: self.sidebar_pointer,
                });
            }
            Message::SidebarMenuOpenRoot => {
                self.sidebar_menu = Some(SidebarMenuState {
                    path: self.sidebar.current_dir.clone(),
                    is_dir: true,
                    at: self.sidebar_pointer,
                });
            }
            Message::SidebarMenuClose => self.sidebar_menu = None,
            Message::SidebarMenuAction(action) => {
                if let Some(menu) = self.sidebar_menu.take() {
                    return self.execute_sidebar_menu_action(menu, action);
                }
            }
            Message::SidebarDialogInput(value) => {
                if let Some(dialog) = self.sidebar_dialog.as_mut() {
                    dialog.input = value;
                    dialog.error = None;
                }
            }
            Message::SidebarDialogSubmit => return self.submit_sidebar_dialog(),
            Message::SidebarDialogCancel => self.sidebar_dialog = None,
            Message::SidebarDeleteConfirm => return self.confirm_sidebar_delete(),
            Message::SidebarDeleteCancel => self.sidebar_delete_confirm = None,
            Message::SidebarOpFinished(report) => {
                match report.result {
                    Ok(()) => {
                        // A completed cut-paste consumes the clipboard; copies
                        // stay reusable. That holds for a cross-location cut
                        // even when the source delete came back as a warning.
                        if matches!(
                            report.op,
                            SidebarOp::Move { .. } | SidebarOp::TransferMove { .. }
                        ) {
                            self.sidebar_clipboard = None;
                        }
                        self.sidebar_notice = report.warning;
                        // Refresh only when the tree still shows the location
                        // the op ran against — a switch mid-op must not yank
                        // the freshly switched tree away.
                        if report.location == self.sidebar.location {
                            return sidebar_load_task(self.sidebar.refresh());
                        }
                    }
                    Err(error) => self.sidebar_notice = Some(error),
                }
            }
            Message::SearchToggleRegex => {
                self.search.toggle_regex();
                self.recompute_search();
                self.reveal_current_search_match();
            }
            Message::SearchToggleCase => {
                self.search.toggle_case_sensitive();
                self.recompute_search();
                self.reveal_current_search_match();
            }
            Message::SearchInput(value) => {
                self.search.query = value;
                self.search.history_nav_index = None;
                self.search.current_match_index = 0;
                self.recompute_search();
                self.reveal_current_search_match();
            }
            Message::SearchReplaceFindInput(value) => {
                self.search_replace.search_input = value;
            }
            Message::SearchReplaceReplaceInput(value) => {
                self.search_replace.replace_input = value;
            }
            Message::SearchReplaceToggleRegex => {
                self.search_replace.config.use_regex = !self.search_replace.config.use_regex;
            }
            Message::SearchReplaceToggleCase => {
                self.search_replace.config.case_sensitive =
                    !self.search_replace.config.case_sensitive;
            }
            Message::SearchReplaceToggleWord => {
                self.search_replace.config.whole_word = !self.search_replace.config.whole_word;
            }
            Message::SearchReplaceToggleAll => {
                self.search_replace.options.replace_all = !self.search_replace.options.replace_all;
            }
            Message::SearchReplaceApply(action) => {
                return self.apply_search_replace(action);
            }
            Message::SearchReplaceClose => self.search_replace.is_open = false,
            Message::PaletteInput(value) => {
                self.palette.query = value;
                self.palette.selected = 0;
                return self.palette_snap_task();
            }
            Message::PaletteExecute(i) => {
                let action = self.palette.action_at(i);
                self.palette.close();
                if let Some(a) = action {
                    return self.execute_palette_action(a);
                }
            }
            Message::ToggleConfigPanel => {
                self.config_panel_open = !self.config_panel_open;
            }
            Message::BlinkTick => {
                self.blink_on = !self.blink_on;
            }
            Message::BlockElapsedTick => {}
            Message::PtyWriteTick => {
                let mut failed = false;
                for session in &mut self.sessions {
                    if !session.flush_write_queue() {
                        failed = true;
                    }
                }
                if failed {
                    self.push_toast("Terminal input write failed", ToastKind::Warning);
                }
            }
            Message::SearchRefreshTick => {
                let active_reflow_pending = self
                    .sessions
                    .get(self.active)
                    .is_some_and(|session| self.history_reflow_sessions.contains(&session.id));
                if self.search_dirty && !active_reflow_pending {
                    self.recompute_search();
                }
            }
            Message::HistoryReflowTick => {
                if self
                    .history_reflow_due
                    .is_some_and(|due| std::time::Instant::now() >= due)
                {
                    let pending = std::mem::take(&mut self.history_reflow_sessions);
                    let mut normalized_any = false;
                    for session in &mut self.sessions {
                        if pending.contains(&session.id) {
                            if session.terminal.normalize_scrollback_width() {
                                normalized_any = true;
                            } else {
                                self.history_reflow_sessions.insert(session.id);
                            }
                            session.refresh();
                        }
                    }
                    self.history_reflow_due = (!self.history_reflow_sessions.is_empty())
                        .then(|| std::time::Instant::now() + std::time::Duration::from_millis(500));
                    let active_reflow_pending = self
                        .sessions
                        .get(self.active)
                        .is_some_and(|session| self.history_reflow_sessions.contains(&session.id));
                    if self.search.is_open
                        && !active_reflow_pending
                        && (normalized_any || self.search_dirty)
                    {
                        self.recompute_search();
                        self.reveal_current_search_match();
                    }
                    self.links_cache_key = None;
                }
            }
            Message::SetTheme(name) => {
                self.config.theme = name;
                self.config_dirty = true;
                self.apply_config();
            }
            Message::SetFontSize(v) => {
                self.config.font_size = Config::clamp_font_size(v);
                self.config_dirty = true;
                self.apply_config();
            }
            Message::SetUiScale(v) => {
                let old_scale = self.scale_factor();
                let new_scale = v.clamp(0.5, 4.0);
                self.win_size = logical_viewport_after_scale(self.win_size, old_scale, new_scale);
                self.config.ui_scale = Some(new_scale);
                self.config_dirty = true;
                self.apply_config();
            }
            Message::SetLineSpacing(v) => {
                self.config.line_spacing = Config::clamp_line_spacing(v);
                self.config_dirty = true;
                self.apply_config();
            }
            Message::SetPadding(v) => {
                self.config.padding = Config::clamp_padding(v);
                self.config_dirty = true;
                self.apply_config();
            }
            Message::SetOpacity(v) => {
                // Read directly at view/style time, so no apply_config rebuild.
                self.config.opacity = Config::clamp_opacity(v);
                self.config_dirty = true;
            }
            Message::SetScrollback(v) => {
                self.config.scrollback_lines = Config::clamp_scrollback_lines(v as usize);
                self.config_dirty = true;
                self.apply_config();
            }
            Message::SetScrollSpeed(v) => {
                self.config.scroll_speed = Config::clamp_scroll_speed(v);
                self.config_dirty = true;
            }
            Message::SetFontFamily(name) => {
                self.config.font_family = name;
                self.config_dirty = true;
                self.apply_config();
            }
            Message::SetScrollbarAlways(always) => {
                self.config.scrollbar_visibility = if always {
                    config::ScrollbarVisibility::Always
                } else {
                    config::ScrollbarVisibility::Auto
                };
                self.config_dirty = true;
            }
            Message::SetDisableAltScreen(disable) => {
                self.config.disable_alt_screen = disable;
                self.config_dirty = true;
                self.apply_config();
            }
            Message::SetAllowClipboardRead(allow) => {
                self.config.allow_clipboard_read = allow;
                self.config_dirty = true;
            }
            Message::SetNotifyLongBlocks(enabled) => {
                self.config.notify_long_blocks = enabled;
                self.config_dirty = true;
            }
            Message::SetBlockMode(enabled) => {
                // The 8px block gutter is real layout space, so toggling mode
                // regrids panes as well as clearing hidden interaction state.
                self.config.block_mode = enabled;
                self.config_dirty = true;
                self.apply_config();
            }
            Message::SetBlockCompact(compact) => {
                // Paint-only in Frost's continuous grid: current nested panes
                // update immediately without a PTY resize or history reflow.
                self.config.block_compact = compact;
                self.config_dirty = true;
            }
            Message::SetShowRepoStrip(show) => {
                self.config.show_repo_strip = show;
                // Hide immediately; the periodic tick would otherwise show a
                // stale strip until the next refresh. The bottom bar reads the
                // same cache, so keep it while the bar still wants git.
                if !show && !self.config.bottom_bar {
                    for sess in self.sessions.iter_mut() {
                        sess.git_meta_cache = None;
                    }
                }
                self.config_dirty = true;
            }
            Message::SetBottomBar(show) => {
                self.config.bottom_bar = show;
                self.config_dirty = true;
                // The bar's row returns to (or leaves) the grid; resize now.
                self.apply_config();
            }
            Message::ThemeEditOpen => {
                // Seed the editor from the current theme; suggest a fresh name so
                // saving doesn't silently overwrite a builtin.
                let base = self.theme.clone();
                let suggested = if Theme::is_builtin(&base.name) {
                    format!("{}-custom", base.name)
                } else {
                    base.name.clone()
                };
                let hexes = base.editable_color_hexes();
                self.theme_editor = Some(ThemeEditState {
                    base,
                    name: suggested,
                    hexes,
                    error: None,
                });
            }
            Message::ThemeEditClose => {
                self.theme_editor = None;
            }
            Message::ThemeEditName(name) => {
                if let Some(ed) = &mut self.theme_editor {
                    ed.name = name;
                }
            }
            Message::ThemeEditColor(idx, hex) => {
                if let Some(ed) = &mut self.theme_editor {
                    if let Some(slot) = ed.hexes.get_mut(idx) {
                        *slot = hex;
                    }
                }
            }
            Message::ThemeEditSave => {
                let mut save_error: Option<String> = None;
                if let Some(ed) = &mut self.theme_editor {
                    let name = ed.name.trim().to_string();
                    if let Err(message) = Theme::validate_custom_theme_name(&name) {
                        ed.error = Some(message);
                    } else if Theme::is_builtin(&name) {
                        ed.error = Some("Name collides with a builtin theme".to_string());
                    } else if let Some(bad) =
                        ed.hexes.iter().position(|h| Theme::hex_to_rgb(h).is_none())
                    {
                        let labels = Theme::editable_color_labels();
                        ed.error = Some(format!("Invalid hex for {}", labels[bad]));
                    } else {
                        let mut theme = ed.base.clone();
                        theme.name = name.clone();
                        for (i, h) in ed.hexes.iter().enumerate() {
                            theme.set_editable_color(i, h);
                        }
                        match theme.save_custom_theme() {
                            Ok(()) => {
                                self.config.theme = name.clone();
                                self.config_dirty = true;
                                self.theme_editor = None;
                                self.apply_config();
                                self.push_toast(
                                    format!("Saved theme \"{}\"", name),
                                    ToastKind::Success,
                                );
                            }
                            Err(e) => {
                                let msg = format!("Save failed: {}", e);
                                ed.error = Some(msg.clone());
                                save_error = Some(msg);
                            }
                        }
                    }
                }
                if let Some(msg) = save_error {
                    self.push_toast(format!("Theme {}", msg), ToastKind::Warning);
                }
            }
            Message::ThemeDelete(name) => {
                match Theme::delete_custom_theme(&name) {
                    Ok(()) => {
                        self.push_toast(format!("Deleted theme \"{}\"", name), ToastKind::Info)
                    }
                    Err(e) => self.push_toast(format!("Delete failed: {}", e), ToastKind::Warning),
                }
                if self.config.theme == name {
                    self.config.theme = "dark".to_string();
                    self.config_dirty = true;
                    self.apply_config();
                }
            }
            Message::ConfigSave => {
                if self.config_write_blocked {
                    self.push_toast(
                        "Config not saved: fix the file error or Reset explicitly",
                        ToastKind::Warning,
                    );
                } else {
                    match self.save_config_checked() {
                        Ok(()) => {
                            self.push_toast("Config saved", ToastKind::Success);
                        }
                        Err(error) => {
                            let message = error.to_string();
                            self.note_config_save_error(&error);
                            self.push_toast(format!("Save failed: {message}"), ToastKind::Warning);
                        }
                    }
                }
            }
            Message::ConfigReset => {
                // Stage and durably persist first: a failed reset must not say
                // "applied" while only mutating this process's memory.
                let reset = Config::default();
                match reset.save_force_revision() {
                    Ok(revision) => {
                        let old_scale = self.scale_factor();
                        self.config = reset;
                        self.config_revision = Some(revision);
                        self.win_size = logical_viewport_after_scale(
                            self.win_size,
                            old_scale,
                            self.scale_factor(),
                        );
                        self.config_dirty = false;
                        self.config_write_blocked = false;
                        self.config_diagnostic = None;
                        self.ai_temperature_draft.clear();
                        self.sync_tab_position_ui();
                        self.apply_config();
                        self.push_toast("Config reset to defaults", ToastKind::Info);
                    }
                    Err(error) => {
                        if let Some(revision) = error.committed_revision().cloned() {
                            // The reset inode is already visible even though
                            // the final directory sync failed. Reflect it in
                            // memory and retry it as dirty on the next tick.
                            let old_scale = self.scale_factor();
                            self.config = reset;
                            self.config_revision = Some(revision);
                            self.win_size = logical_viewport_after_scale(
                                self.win_size,
                                old_scale,
                                self.scale_factor(),
                            );
                            self.config_dirty = true;
                            self.config_write_blocked = false;
                            self.config_diagnostic = None;
                            self.ai_temperature_draft.clear();
                            self.sync_tab_position_ui();
                            self.apply_config();
                            self.push_toast(
                                format!(
                                    "Config reset is visible but needs a durability retry: {error}"
                                ),
                                ToastKind::Warning,
                            );
                        } else {
                            self.push_toast(
                                format!("Reset not applied: {error}"),
                                ToastKind::Warning,
                            );
                        }
                    }
                }
            }
            Message::ConfigTick => {
                // Reconcile external writers before local auto-save so a dirty
                // instance cannot erase a newer file without noticing it.
                self.reload_config_if_changed();
                self.persist_live_config();

                let keybindings_changed = keybindings::KeyBindings::config_revision()
                    .map(|revision| self.keybindings_revision.as_ref() != Some(&revision))
                    // A transient read failure must be retried; never record it
                    // as the last-known-good revision.
                    .unwrap_or(true);
                if keybindings_changed {
                    let keybindings::KeyBindingsLoad {
                        bindings,
                        diagnostics,
                        usable,
                        revision,
                    } = keybindings::KeyBindings::load_with_diagnostics();
                    if usable {
                        self.keybindings = bindings;
                    }
                    if let Some(revision) = revision {
                        self.keybindings_revision = Some(revision);
                    }
                    let changed = diagnostics != self.keybindings_diagnostics;
                    self.keybindings_diagnostics = diagnostics;
                    if changed {
                        if self.keybindings_diagnostics.is_empty() {
                            self.push_toast("Keybindings reloaded", ToastKind::Success);
                        } else {
                            self.push_toast(
                                "Some keybindings could not be loaded",
                                ToastKind::Warning,
                            );
                        }
                    }
                }
                // Periodically persist tabs so a recent snapshot (with up-to-date
                // cwds) survives even an abrupt exit. Only when something that
                // feeds the snapshot may have changed since the last save.
                if self.session_dirty {
                    self.save_session_snapshot();
                }
                // Refresh cwd + foreground-process caches for every session so
                // tab labels reflect both. These are cheap /proc reads at 1.5s
                // cadence and let inactive tabs still show "vim · src" etc.
                // The git meta rides the same cadence; its probe is served by
                // a coalesced background worker with a bounded wait. Probed
                // while either consumer (pane header, bottom bar) is on.
                let want_git = self.config.show_repo_strip || self.config.bottom_bar;
                for sess in self.sessions.iter_mut() {
                    sess.terminal.check_sync_output_timeout();
                    sess.refresh();
                    sess.cwd_cache = sess.cwd();
                    sess.fg_proc_cache = sess.fg_proc();
                    sess.git_meta_cache = if want_git { sess.git_meta() } else { None };
                }
                self.expire_toasts();
            }
            Message::TabMenuOpen(id) => {
                // The menu is keyed on the tab id the strip handed out, not on
                // a session id: a tab with several panes has one id here and a
                // different one per pane.
                if self.tab_index_by_id(id).is_some() {
                    self.tab_menu = Some(id);
                    self.tab_rename = None;
                    // Freeze the anchor now; the pointer moves on toward the
                    // panel as soon as it appears.
                    self.tab_menu_at = self.tab_pointer;
                }
            }
            Message::TabPointerMoved(position) => self.tab_pointer = position,
            Message::TabMenuClose => {
                self.tab_menu = None;
                self.tab_rename = None;
            }
            Message::TabMenuAction(action) => {
                self.tab_menu = None;
                self.tab_rename = None;
                return self.execute_tab_menu_action(action);
            }
            Message::TabRenameStart(id) => {
                let Some(tab) = self.tab_index_by_id(id) else {
                    return Task::none();
                };
                // Seed the editor with what the strip currently shows, so a
                // rename starts from the label the user just right-clicked.
                self.tab_rename = Some((id, self.tab_real_label(tab)));
                return iced::widget::operation::focus(TAB_RENAME_INPUT_ID.clone());
            }
            Message::TabRenameInput(draft) => {
                if let Some((_, current)) = self.tab_rename.as_mut() {
                    *current = draft;
                }
            }
            Message::TabRenameSubmit => {
                if let Some((id, draft)) = self.tab_rename.take() {
                    self.apply_tab_rename(id, draft);
                }
                self.tab_menu = None;
            }
            Message::ToastTick => self.expire_toasts(),
            Message::ToastDismiss(i) => {
                if i < self.toasts.len() {
                    self.toasts.remove(i);
                }
            }
            Message::TabSwitcherClose => self.tab_switcher = None,
            Message::TabSwitcherInput(q) => {
                if let Some(s) = self.tab_switcher.as_mut() {
                    s.query = q;
                    s.selected = 0;
                }
            }
            Message::TabSwitcherJump(id) => {
                self.tab_switcher = None;
                if let Some(index) = self.sessions.iter().position(|session| session.id == id) {
                    if index != self.active {
                        self.activate_session(index);
                    }
                }
            }
            Message::HistoryPickerClose => self.history_picker = None,
            Message::BlockSearchClose => self.block_search = None,
            Message::BlockSearchCacheBuilt(identity, result) => {
                let accepts = self
                    .block_search
                    .as_ref()
                    .is_some_and(|state| state.accepts_build(identity))
                    && self.sessions.get(self.active).is_some_and(|sess| {
                        sess.id == identity.session_id && !sess.terminal.is_alt_buffer_active()
                    });
                if !accepts {
                    return Task::none();
                }
                match result {
                    Ok(build) => {
                        if let Some(state) = self.block_search.as_mut() {
                            state.cache = build.zones;
                            state.older_not_indexed = build.older_not_indexed;
                            state.loading = false;
                        }
                        self.block_search_recompute();
                        return self.block_search_snap_task();
                    }
                    Err(error) => {
                        if let Some(state) = self.block_search.as_mut() {
                            state.loading = false;
                            state.cache.clear();
                            state.hits.clear();
                            state.capped = false;
                        }
                        log::warn!("{error}");
                        self.push_toast(
                            "Could not index command blocks".to_string(),
                            ToastKind::Warning,
                        );
                    }
                }
            }
            Message::BlockSearchInput(query) => {
                if let Some(state) = self.block_search.as_mut() {
                    state.query = query;
                }
                self.block_search_recompute();
                // The result set changed and the highlight reset to the top:
                // bring the list back to the top with it.
                return self.block_search_snap_task();
            }
            Message::BlockSearchSetFilter(filter) => {
                if let Some(state) = self.block_search.as_mut() {
                    state.filter = filter;
                }
                self.block_search_recompute();
                return self.block_search_snap_task();
            }
            Message::BlockSearchAccept(hit) => {
                let owner = self.block_search.take().map(|state| state.session_id);
                let target_is_live = owner.is_some_and(|session_id| {
                    self.block_search_target_is_live(session_id, hit.zone_id)
                });
                if target_is_live && self.ensure_block_action_available("Block search") {
                    self.reveal_block_search_hit(&hit);
                }
            }
            Message::BlockMenuClose => self.block_menu = None,
            Message::BlockMenuAction(action) => {
                return self.execute_block_menu_action(action);
            }
            Message::BlockClearConfirmNo => self.block_clear_confirm = None,
            Message::BlockClearConfirmYes => return self.confirm_block_clear(),
            Message::BlockExportFinished(format, result) => {
                self.block_export_in_flight = false;
                match result {
                    Ok(path) => self.push_toast(
                        format!("Exported {} blocks to {}", format.label(), path.display()),
                        ToastKind::Success,
                    ),
                    Err(error) => self.push_toast(
                        format!("Could not export {} blocks: {error}", format.label()),
                        ToastKind::Warning,
                    ),
                }
            }
            Message::HistoryPickerInput(q) => {
                if let Some(s) = self.history_picker.as_mut() {
                    s.query = q;
                    s.selected = 0;
                }
            }
            Message::HistoryPickerAccept(command) => {
                self.history_picker = None;
                return self.recall_into_active_pane(command);
            }
            Message::TabCloseConfirmNo => {
                self.tab_close_confirm = None;
            }
            Message::TabCloseConfirmYes => {
                if let Some((id, _, pending)) = self.tab_close_confirm.take() {
                    if let Some(index) = self.session_index_by_id(id) {
                        return self.execute_pending_close(index, pending);
                    }
                }
            }
        }
        self.recompute_links();
        self.refresh_kitty_handles();
        Task::none()
    }

    /// Build/refresh cached image handles for the active session's Kitty
    /// placements. New, content-changed or differently-cropped placements get a
    /// fresh handle; handles no longer referenced by any placement are dropped.
    fn refresh_kitty_handles(&mut self) {
        type PendingHandle = (KittyHandleKey, u64, u32, u32, Vec<u8>);
        // Collect, under an immutable borrow, which images need a (re)build and
        // which ids are still live, then release the borrow before mutating.
        let mut needed: Vec<PendingHandle> = Vec::new();
        let mut live_keys = std::collections::HashSet::new();
        {
            let Some(sess) = self.sessions.get(self.active) else {
                self.kitty_handles.clear();
                return;
            };
            let kg = &sess.terminal.kitty_graphics;
            for p in kg.get_placements() {
                let Some(img) = kg.get_image(p.image_id) else {
                    continue;
                };
                // `x=`/`y=`/`w=`/`h=` select a sub-rectangle of the image, so the
                // uploaded texture is the crop, not the whole image.
                let Some(crop) = kitty_graphics::placement_crop(img, p) else {
                    continue;
                };
                let key = (sess.id, p.image_id, crop);
                // Many placements may reference one image. Schedule/cache each
                // texture once so placement fan-out cannot clone and upload the
                // same (potentially large) pixel buffer hundreds of times.
                if !live_keys.insert(key) {
                    continue;
                }
                let stale = self
                    .kitty_handles
                    .get(&key)
                    .map(|(_, generation)| *generation != img.generation)
                    .unwrap_or(true);
                if stale {
                    needed.push((
                        key,
                        img.generation,
                        crop.2,
                        crop.3,
                        kitty_graphics::crop_rgba(img, crop),
                    ));
                }
            }
        }
        self.kitty_handles.retain(|key, _| live_keys.contains(key));
        for (key, generation, w, h, data) in needed {
            let handle = iced::advanced::image::Handle::from_rgba(w, h, data);
            self.kitty_handles.insert(key, (handle, generation));
        }
    }

    /// Build the renderable image list for a session from its placements and the
    /// cached handles. Placements are already z-sorted by the graphics state.
    fn kitty_images(&self, sess: &Session) -> Vec<KittyRender> {
        let kg = &sess.terminal.kitty_graphics;
        kg.get_placements()
            .iter()
            .filter_map(|p| {
                let img = kg.get_image(p.image_id)?;
                let crop = kitty_graphics::placement_crop(img, p)?;
                let (handle, _) = self.kitty_handles.get(&(sess.id, p.image_id, crop))?;
                // `row` records the protocol-time screen coordinate; retained
                // rendering deliberately keys off the stable `buffer_row`.
                let _protocol_screen_row = p.row;
                let rows = (p.rows as usize).max(1);
                let anchor = projected_kitty_anchor(
                    &sess.terminal,
                    &sess.projection,
                    p.buffer_row,
                    p.col as usize,
                    (p.cols as usize).max(1),
                    rows,
                )?;
                Some(KittyRender {
                    handle: handle.clone(),
                    col: anchor.col,
                    row: anchor.row,
                    cols: (p.cols as usize).max(1),
                    rows,
                    id: p.image_id,
                    px_w: crop.2,
                    px_h: crop.3,
                })
            })
            .collect()
    }

    /// Re-detect links in the active session's visible grid. Version-gated so it
    /// is a no-op when neither the grid, the scroll position, nor the tab changed.
    fn recompute_links(&mut self) {
        let Some(sess) = self.sessions.get(self.active) else {
            self.links.clear();
            return;
        };
        let key = (sess.id, sess.projection.key());
        let cacheable = sess.projection.view_revision() != 0;
        if cacheable && self.links_cache_key == Some(key.clone()) {
            return;
        }
        self.links_cache_key = cacheable.then_some(key);
        let mut links = self
            .link_detector
            .detect_links_in_visible_cells_with_wrapping(
                sess.projection.cells(),
                sess.projection.row_wrapped(),
            );
        // OSC 8 is metadata rather than visible URL text. Give its exact cell
        // spans precedence over heuristic matches, while still routing every
        // target through the same `is_openable_url` policy in the terminal,
        // here, and once more immediately before the opener is spawned.
        let osc8_links = sess
            .terminal
            .osc8_links_in_visible_cells(sess.projection.cells());
        links.retain(|detected| {
            !osc8_links.iter().any(|explicit| {
                detected.line == explicit.line
                    && detected.col_start < explicit.col_end
                    && explicit.col_start < detected.col_end
            })
        });
        links.extend(
            osc8_links
                .into_iter()
                .filter(|link| link::is_openable_url(&link.text)),
        );
        self.links = links;
    }

    // --- Theme-derived chrome colors and styles ---------------------------
    fn c_panel(&self) -> Color {
        Theme::rgb_to_color32(self.theme.ui.panel_bg)
    }
    fn c_text(&self) -> Color {
        Theme::rgb_to_color32(self.theme.ui.text)
    }
    fn c_text_dim(&self) -> Color {
        Theme::rgb_to_color32(self.theme.ui.text_disabled)
    }
    fn c_border(&self) -> Color {
        Theme::rgb_to_color32(self.theme.ui.border)
    }
    fn c_accent(&self) -> Color {
        Theme::rgb_to_color32(self.theme.tabbar.active_border)
    }

    /// Top tab bar / status bar background, matching the theme's tabbar color.
    fn chrome_bar_style(&self) -> impl Fn(&iced::Theme) -> container::Style {
        let bg = self.with_window_opacity(Theme::rgb_to_color32(self.theme.tabbar.bg));
        let text = self.c_text();
        move |_| container::Style {
            text_color: Some(text),
            background: Some(bg.into()),
            ..Default::default()
        }
    }

    /// Sidebar dock background, matching the theme's panel color.
    fn panel_style(&self) -> impl Fn(&iced::Theme) -> container::Style {
        let bg = self.with_window_opacity(self.c_panel());
        let text = self.c_text();
        move |_| container::Style {
            text_color: Some(text),
            background: Some(bg.into()),
            ..Default::default()
        }
    }

    /// `active` (hovered or mid-drag) tints the strip with the accent color so
    /// the user can see the divider is grabbable / being dragged.
    fn divider_style(&self, active: bool) -> impl Fn(&iced::Theme) -> container::Style {
        let bg = if active {
            blend(self.c_border(), self.c_accent(), 0.6)
        } else {
            self.c_border()
        };
        move |_| container::Style {
            background: Some(bg.into()),
            ..Default::default()
        }
    }

    /// Container-flavored variant of `tab_btn_style`, used when wrapping a tab
    /// in `mouse_area` (which can't hand the hover status off to a Button).
    /// `hovered`/`dragging` are pushed in by the caller from `self.hovered_tab`
    /// and `self.dragging_tab`.
    fn tab_container_style(
        &self,
        active: bool,
        hovered: bool,
        dragging: bool,
    ) -> impl Fn(&iced::Theme) -> container::Style {
        let base = Theme::rgb_to_color32(self.theme.tabbar.bg);
        let accent = self.c_accent();
        let active_text = Theme::rgb_to_color32(self.theme.tabbar.active_text);
        let inactive_text = Theme::rgb_to_color32(self.theme.tabbar.inactive_text);
        move |_t| {
            let (mut bg, txt, bw) = if active {
                (blend(base, accent, 0.22), active_text, 1.0)
            } else if hovered {
                (blend(base, accent, 0.10), inactive_text, 0.0)
            } else {
                (base, inactive_text, 0.0)
            };
            // Dim the source tab while it is being dragged so the user sees
            // which one will move.
            if dragging {
                bg = Color { a: 0.55, ..bg };
            }
            container::Style {
                text_color: Some(txt),
                background: Some(bg.into()),
                border: iced::Border {
                    color: accent,
                    width: bw,
                    radius: 4.0.into(),
                },
                ..Default::default()
            }
        }
    }

    /// Tab button: accent-tinted + bordered when active, flat otherwise.
    fn tab_btn_style(
        &self,
        active: bool,
    ) -> impl Fn(&iced::Theme, button::Status) -> button::Style {
        let base = Theme::rgb_to_color32(self.theme.tabbar.bg);
        let accent = self.c_accent();
        let active_text = Theme::rgb_to_color32(self.theme.tabbar.active_text);
        let inactive_text = Theme::rgb_to_color32(self.theme.tabbar.inactive_text);
        move |_t, status| {
            let (bg, txt, bw) = if active {
                (blend(base, accent, 0.22), active_text, 1.0)
            } else {
                let bg = match status {
                    button::Status::Hovered => blend(base, accent, 0.10),
                    _ => base,
                };
                (bg, inactive_text, 0.0)
            };
            button::Style {
                background: Some(bg.into()),
                text_color: txt,
                border: iced::Border {
                    color: accent,
                    width: bw,
                    radius: 4.0.into(),
                },
                ..Default::default()
            }
        }
    }

    /// Flat button (toggles, file rows, "+ New"): transparent, accent on hover.
    fn ghost_btn_style(&self) -> impl Fn(&iced::Theme, button::Status) -> button::Style {
        let base = self.c_panel();
        let accent = self.c_accent();
        let text = self.c_text();
        move |_t, status| {
            let bg = match status {
                button::Status::Hovered => Some(blend(base, accent, 0.16).into()),
                _ => None,
            };
            button::Style {
                background: bg,
                text_color: text,
                border: iced::Border {
                    color: Color::TRANSPARENT,
                    width: 0.0,
                    radius: 4.0.into(),
                },
                ..Default::default()
            }
        }
    }

    /// Close (×) button using the theme's close-button colors.
    fn close_btn_style(&self) -> impl Fn(&iced::Theme, button::Status) -> button::Style {
        let normal = Theme::rgb_to_color32(self.theme.tabbar.close_btn_bg);
        let hover = Theme::rgb_to_color32(self.theme.tabbar.close_btn_hover);
        let text = self.c_text();
        move |_t, status| {
            let bg = match status {
                button::Status::Hovered => hover,
                _ => normal,
            };
            button::Style {
                background: Some(bg.into()),
                text_color: text,
                border: iced::Border {
                    color: Color::TRANSPARENT,
                    width: 0.0,
                    radius: 4.0.into(),
                },
                ..Default::default()
            }
        }
    }

    /// Close button embedded in a tab. The tab container owns the shared
    /// background and outline; only the close button's hover affordance paints
    /// a separate background.
    fn tab_close_btn_style(&self) -> impl Fn(&iced::Theme, button::Status) -> button::Style {
        let hover = Theme::rgb_to_color32(self.theme.tabbar.close_btn_hover);
        let text = self.c_text();
        move |_t, status| {
            let background = match status {
                button::Status::Hovered | button::Status::Pressed => Some(hover.into()),
                _ => None,
            };
            button::Style {
                background,
                text_color: text,
                border: iced::Border {
                    color: Color::TRANSPARENT,
                    width: 0.0,
                    radius: 3.0.into(),
                },
                ..Default::default()
            }
        }
    }

    fn tab_bar(&self) -> Element<'_, Message> {
        let mut tabs = row![].spacing(2).padding(2);
        // Sidebar/dock toggle button at the far left of the tab bar.
        tabs = tabs.push(
            button(text("☰").size(13))
                .on_press(Message::ToggleSidebar)
                .padding([3, 8])
                .style(self.tab_btn_style(self.sidebar_open)),
        );
        // In side-tab mode the tab list lives in the dock; the top bar keeps only
        // the dock toggle plus a button to move tabs back to the top.
        if self.config.tab_position == config::TabPosition::Side {
            tabs = tabs.push(
                button(text("▔").size(13))
                    .on_press(Message::SetTabPosition(config::TabPosition::Top))
                    .padding([3, 8])
                    .style(self.ghost_btn_style()),
            );
            if self.pane_drag.is_some() {
                tabs = tabs.push(self.pane_to_tab_drop_hint(false));
                let tabs = mouse_area(tabs.width(Length::Fill))
                    .on_release(Message::PanePromoteToTab(None))
                    .interaction(iced::mouse::Interaction::Grabbing);
                return self.top_bar_with_close(tabs.into());
            }
            return self.top_bar_with_close(tabs.into());
        }
        // Dock the tab strip into the left sidebar (vertical tab list).
        tabs = tabs.push(
            button(text("◧").size(13))
                .on_press(Message::SetTabPosition(config::TabPosition::Side))
                .padding([3, 8])
                .style(self.ghost_btn_style()),
        );
        for i in 0..self.tabs.len() {
            let id = self.tabs[i].id;
            let active = i == self.active_tab;
            // A tab shows its selected pane.
            let label = self.tab_label(i);
            let label = if label.chars().count() > 24 {
                let truncated: String = label.chars().take(23).collect();
                format!("{truncated}…")
            } else {
                label
            };
            // The label and close button share one styled outer container. The
            // label keeps its own mouse area so dragging it cannot accidentally
            // trigger the close button.
            let hovered = self.hovered_tab == Some(id);
            let dragging_this = self.dragging_tab == Some(id);
            let label = format!("{}{label}", self.tab_state_prefix(i));
            let tab_label = container(text(label).size(13)).padding([3, 8]);
            // Drag press/release lives on the label so a press on the close
            // button never starts a tab drag. Right-click opens the context menu.
            let tab: Element<'_, Message> = mouse_area(tab_label)
                .on_press(Message::TabDragStart(id))
                .on_release(Message::TabDragEnd(id))
                .on_right_press(Message::TabMenuOpen(id))
                .into();
            // Reveal the close button only on the active or hovered tab to cut
            // visual noise; keep its footprint reserved otherwise so tabs don't
            // jump when hovered.
            let show_close = active || hovered;
            let close: Element<'_, Message> = if show_close {
                button(text("×").size(13))
                    .on_press(Message::CloseTab(id))
                    .padding([3, 6])
                    .style(self.tab_close_btn_style())
                    .into()
            } else {
                Space::new().width(Length::Fixed(18.0)).into()
            };
            let cell = container(row![tab, close].align_y(iced::Alignment::Center))
                .style(self.tab_container_style(active, hovered, dragging_this));
            // Hover tracking on the whole cell so moving onto the close
            // button does not collapse it out of the layout.
            tabs = tabs.push(
                mouse_area(cell)
                    .on_enter(Message::TabHover(Some(id)))
                    .on_exit(Message::TabHover(None)),
            );
        }
        tabs = tabs.push(
            button(text("+").size(13))
                .on_press(Message::NewSession)
                .padding([3, 8])
                .style(self.ghost_btn_style()),
        );
        if self.pane_drag.is_some() {
            tabs = tabs.push(self.pane_to_tab_drop_hint(false));
        }
        let scroller: Element<'_, Message> = scrollable(tabs)
            .direction(scrollable::Direction::Horizontal(
                scrollable::Scrollbar::new().width(0).scroller_width(0),
            ))
            .width(Length::Fill)
            .into();
        let scroller = if self.pane_drag.is_some() {
            mouse_area(scroller)
                .on_release(Message::PanePromoteToTab(None))
                .interaction(iced::mouse::Interaction::Grabbing)
                .into()
        } else {
            scroller
        };
        self.top_bar_with_close(scroller)
    }

    /// The top bar doubles as the window's title bar: the window is
    /// undecorated, so this row owns the title, the window buttons, and
    /// drag-to-move.
    fn top_bar_with_close<'a>(&'a self, content: Element<'a, Message>) -> Element<'a, Message> {
        let window_btn = |glyph: &str, msg: Message| {
            button(text(glyph.to_string()).size(13))
                .on_press(msg)
                .padding([3, 9])
                .style(self.ghost_btn_style())
        };
        let close = button(text("×").size(14))
            .on_press(Message::WindowClose)
            .padding([3, 9])
            .style(self.close_btn_style());

        let mut bar = row![container(content).width(Length::Fill)];
        // In Side mode the tab labels live in the dock, so the title is the
        // only place the active session is named and there is room for it. In
        // Top mode the active tab already shows the very same string
        // (`Frost::title` is the active session's label), so repeating it here
        // would only steal width from the strip.
        if self.config.tab_position == config::TabPosition::Side {
            bar = bar.push(
                container(
                    text(self.title())
                        .size(13)
                        .wrapping(text::Wrapping::None)
                        .style(text::secondary),
                )
                .padding([0, 8])
                .clip(true),
            );
        }
        let bar = bar
            .push(window_btn("—", Message::WindowMinimize))
            .push(window_btn("▢", Message::WindowToggleMaximize))
            .push(close)
            .align_y(iced::Alignment::Center)
            .width(Length::Fill);
        // Presses that no button or tab consumed fall through to here and move
        // the window, the way a title bar is expected to behave. `mouse_area`
        // runs its content first and bails once the event is captured, so the
        // chrome above keeps working.
        let bar = mouse_area(bar)
            .on_press(Message::WindowDrag)
            .on_double_click(Message::WindowToggleMaximize);
        container(bar)
            .width(Length::Fill)
            .height(Length::Fixed(TAB_BAR_H))
            .style(self.chrome_bar_style())
            .into()
    }

    /// Floating tab context menu, the same item set anvil/forge put on their
    /// sidebar tabs: New Tab, Duplicate, Rename, Mark, Pin, the close family,
    /// and one entry per configured `[[remote_hosts]]` destination.
    ///
    /// Background mouse_area dismisses on outside-click; Esc also closes via
    /// the key handler (and backs out of an open rename editor first).
    fn tab_context_menu(&self, id: usize) -> Element<'_, Message> {
        // Everything here is keyed on the tab id, never on a session id: a tab
        // with several panes owns several sessions but only one strip entry.
        let i = self.tab_index_by_id(id).unwrap_or(self.active_tab);
        let label = self.tab_label(i);
        let label = if label.is_empty() {
            format!("Tab {}", i + 1)
        } else {
            label
        };
        let row_btn = |t: &str, msg: Message| -> Element<'_, Message> {
            button(text(t.to_string()).size(13))
                .on_press(msg)
                .padding([4, 10])
                .width(Length::Fill)
                .style(self.ghost_btn_style())
                .into()
        };
        let only_one = self.tabs.len() <= 1;
        let last_idx = self.tabs.len().saturating_sub(1);
        let pinned = self.tabs.get(i).is_some_and(|tab| tab.pinned);
        let private_title = self.tabs.get(i).is_some_and(|tab| tab.private_title);
        let marked_count = self.tabs.iter().filter(|tab| tab.marked).count();
        let is_marked = self.tabs.get(i).is_some_and(|tab| tab.marked);

        let mut menu = column![text(label).size(12).style(text::secondary)].spacing(2);

        // Renaming replaces the item list with an inline editor so the menu
        // stays one panel; Enter commits, Esc backs out to the items.
        if let Some((_, draft)) = self.tab_rename.as_ref().filter(|(target, _)| *target == id) {
            menu = menu.push(
                text_input("Tab name (empty = follow shell)", draft)
                    .id(TAB_RENAME_INPUT_ID.clone())
                    .on_input(Message::TabRenameInput)
                    .on_submit(Message::TabRenameSubmit)
                    .size(13),
            );
            menu = menu.push(row_btn("Rename", Message::TabRenameSubmit));
            menu = menu.push(row_btn("Cancel", Message::TabMenuClose));
            // 3 rows: the input plus two buttons.
            return self.float_tab_menu(menu, 3);
        }

        menu = menu.push(row_btn(
            "New Tab",
            Message::TabMenuAction(TabMenuAction::NewTab),
        ));
        menu = menu.push(row_btn(
            "Duplicate",
            Message::TabMenuAction(TabMenuAction::Duplicate(id)),
        ));
        menu = menu.push(row_btn("Rename", Message::TabRenameStart(id)));
        menu = menu.push(row_btn(
            if private_title {
                "Show Title Details"
            } else {
                "Hide Title Details"
            },
            Message::TabMenuAction(TabMenuAction::TogglePrivateTitle(id)),
        ));
        menu = menu.push(row_btn(
            if is_marked {
                "Unmark"
            } else {
                "Mark Important"
            },
            Message::TabMenuAction(TabMenuAction::ToggleMarked(id)),
        ));
        menu = menu.push(row_btn(
            if pinned { "Unpin Tab" } else { "Pin Tab" },
            Message::TabMenuAction(TabMenuAction::TogglePinned(id)),
        ));
        menu = menu.push(row_btn(
            "Close",
            Message::TabMenuAction(TabMenuAction::Close(id)),
        ));
        if !only_one {
            menu = menu.push(row_btn(
                "Close Others",
                Message::TabMenuAction(TabMenuAction::CloseOthers(id)),
            ));
        }
        if i < last_idx {
            menu = menu.push(row_btn(
                "Close to Right",
                Message::TabMenuAction(TabMenuAction::CloseToRight(id)),
            ));
        }
        if marked_count > 0 {
            menu = menu.push(row_btn(
                &format!("Close Marked Tabs ({marked_count})"),
                Message::TabMenuAction(TabMenuAction::CloseMarked),
            ));
        }
        for (index, host) in self.config.remote_hosts.iter().enumerate() {
            menu = menu.push(row_btn(
                &format!("Remote: {}", host.display_name()),
                Message::TabMenuAction(TabMenuAction::ConnectRemote(index)),
            ));
        }

        // Item count for the height estimate the placement clamps against:
        // the seven always-present entries plus the conditional ones.
        let rows = 7
            + usize::from(!only_one)
            + usize::from(i < last_idx)
            + usize::from(marked_count > 0)
            + self.config.remote_hosts.len();
        self.float_tab_menu(menu, rows)
    }

    /// Place an open tab menu at the pointer position that summoned it, over a
    /// dismiss-on-outside-click sheet.
    ///
    /// The panel is a free-floating overlay, so it has to be positioned by
    /// hand: iced has no popup anchored to a widget. Centering it (as this did
    /// before) puts it nowhere near the tab that was right-clicked — obviously
    /// wrong once the tab list is docked in the sidebar. `rows` drives the
    /// height estimate that keeps the panel inside the window; being a little
    /// off only shifts a menu that was near the bottom edge anyway.
    fn float_tab_menu<'a>(
        &'a self,
        menu: iced::widget::Column<'a, Message>,
        rows: usize,
    ) -> Element<'a, Message> {
        const PANEL_W: f32 = 240.0;
        const ROW_H: f32 = 27.0;
        /// Header line + the panel's own vertical padding.
        const PANEL_CHROME_H: f32 = 40.0;
        const EDGE_GAP: f32 = 4.0;

        let panel_h = PANEL_CHROME_H + rows as f32 * ROW_H;
        // Keep the whole panel on screen. `max(EDGE_GAP)` runs last so a window
        // too small to hold the panel still shows its top-left corner rather
        // than pushing it off past the left/top edge.
        let x = (self.tab_menu_at.x)
            .min(self.win_size.width - PANEL_W - EDGE_GAP)
            .max(EDGE_GAP);
        let y = (self.tab_menu_at.y)
            .min(self.win_size.height - panel_h - EDGE_GAP)
            .max(TAB_BAR_H);

        let panel = container(menu)
            .width(Length::Fixed(PANEL_W))
            .padding(8)
            .style(container::dark);

        // Dismiss-on-outside-click sheet behind the panel.
        let dismiss = mouse_area(
            container(Space::new())
                .width(Length::Fill)
                .height(Length::Fill),
        )
        .on_press(Message::TabMenuClose);
        let placed = container(panel)
            .align_left(Length::Fill)
            .align_top(Length::Fill)
            .padding(iced::Padding::from(0).top(y).left(x));
        stack![Element::from(dismiss), Element::from(placed)].into()
    }

    /// Floating file-ops menu opened by right-clicking a file-tree row (or the
    /// empty area below the tree, which targets the root dir). Placement is
    /// the tab menu's: pointer-anchored and clamped into the window.
    fn sidebar_menu_view(&self, state: &SidebarMenuState) -> Element<'_, Message> {
        let row_btn = |label: &str, action: SidebarMenuAction| {
            button(text(label.to_string()).size(13))
                .on_press(Message::SidebarMenuAction(action))
                .padding([4, 10])
                .width(Length::Fill)
                .style(self.ghost_btn_style())
        };
        let target = state.path.display().to_string();
        let mut menu = column![text(target).size(12).style(text::secondary)].spacing(2);
        menu = menu.push(row_btn("New File", SidebarMenuAction::NewFile));
        menu = menu.push(row_btn("New Folder", SidebarMenuAction::NewFolder));
        menu = menu.push(row_btn("Rename", SidebarMenuAction::Rename));
        menu = menu.push(row_btn("Delete", SidebarMenuAction::Delete));
        menu = menu.push(row_btn("Copy", SidebarMenuAction::Copy));
        menu = menu.push(row_btn("Cut", SidebarMenuAction::Cut));
        // Paste is offered for any clipboard; within one location it copies
        // or moves, across locations it downloads/uploads (remote→remote via
        // a local relay). The label previews what the click would do.
        let paste_clip = self.sidebar_clipboard.as_ref();
        let paste_label = match paste_clip {
            Some(clip) => {
                let name = clip
                    .path
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_default();
                let suffix = if clip.is_dir { "/" } else { "" };
                let verb = transfer_verb(&clip.loc, &self.sidebar.location, clip.cut);
                format!("Paste {name}{suffix} ({verb})")
            }
            None => "Paste".to_string(),
        };
        let paste = button(text(paste_label).size(13))
            .padding([4, 10])
            .width(Length::Fill)
            .style(self.ghost_btn_style());
        menu = menu.push(if paste_clip.is_some() {
            paste.on_press(Message::SidebarMenuAction(SidebarMenuAction::Paste))
        } else {
            paste
        });
        menu = menu.push(row_btn("Refresh", SidebarMenuAction::Refresh));

        const PANEL_W: f32 = 220.0;
        const ROW_H: f32 = 27.0;
        /// Header line + the panel's own vertical padding.
        const PANEL_CHROME_H: f32 = 40.0;
        const EDGE_GAP: f32 = 4.0;
        let panel_h = PANEL_CHROME_H + 8.0 * ROW_H;
        let x = (state.at.x)
            .min(self.win_size.width - PANEL_W - EDGE_GAP)
            .max(EDGE_GAP);
        let y = (state.at.y)
            .min(self.win_size.height - panel_h - EDGE_GAP)
            .max(TAB_BAR_H);
        let panel = container(menu)
            .width(Length::Fixed(PANEL_W))
            .padding(8)
            .style(container::dark);
        // Dismiss-on-outside-click sheet behind the panel.
        let dismiss = mouse_area(
            container(Space::new())
                .width(Length::Fill)
                .height(Length::Fill),
        )
        .on_press(Message::SidebarMenuClose);
        let placed = container(panel)
            .align_left(Length::Fill)
            .align_top(Length::Fill)
            .padding(iced::Padding::from(0).top(y).left(x));
        stack![Element::from(dismiss), Element::from(placed)].into()
    }

    /// Centered modal collecting a name for New File / New Folder / Rename.
    /// Validation errors show inline and keep the dialog open; Enter submits,
    /// Esc cancels (both also handled at the app key layer).
    fn sidebar_dialog_view(&self, state: &SidebarDialogState) -> Element<'_, Message> {
        let (title, placeholder, confirm) = match state.kind {
            SidebarDialogKind::NewFile => ("New file", "file name", "Create"),
            SidebarDialogKind::NewFolder => ("New folder", "folder name", "Create"),
            SidebarDialogKind::Rename => ("Rename", "new name", "Rename"),
        };
        let mut body = column![
            text(title).size(14),
            text(state.path.display().to_string())
                .size(11)
                .wrapping(text::Wrapping::Word)
                .style(text::secondary),
            text_input(placeholder, &state.input)
                .id(SIDEBAR_DIALOG_INPUT_ID.clone())
                .on_input(Message::SidebarDialogInput)
                .on_submit(Message::SidebarDialogSubmit)
                .size(13),
        ]
        .spacing(8);
        if let Some(error) = &state.error {
            body = body.push(text(error.clone()).size(11).style(text::danger));
        }
        body = body.push(
            row![
                button(text("Cancel").size(13))
                    .on_press(Message::SidebarDialogCancel)
                    .padding([4, 12])
                    .style(self.ghost_btn_style()),
                Space::new().width(Length::Fill),
                button(text(confirm).size(13))
                    .on_press(Message::SidebarDialogSubmit)
                    .padding([4, 12]),
            ]
            .spacing(8)
            .align_y(iced::Alignment::Center),
        );
        let panel = container(body)
            .width(Length::Fixed(360.0))
            .padding(14)
            .style(container::dark);
        let dismiss = mouse_area(
            container(Space::new())
                .width(Length::Fill)
                .height(Length::Fill),
        )
        .on_press(Message::SidebarDialogCancel);
        let centered = container(panel)
            .center_x(Length::Fill)
            .center_y(Length::Fill);
        stack![Element::from(dismiss), Element::from(centered)].into()
    }

    /// Centered modal confirming one deletion with the full path spelled out.
    /// There is no trash and no undo, locally or over ssh.
    fn sidebar_delete_confirm_view(&self, path: &std::path::Path) -> Element<'_, Message> {
        let body = column![
            text("Delete permanently?").size(14),
            text(path.display().to_string())
                .size(12)
                .wrapping(text::Wrapping::Word)
                .style(text::secondary),
            text("Directories are removed with everything inside them. This cannot be undone.")
                .size(12)
                .wrapping(text::Wrapping::Word)
                .style(text::danger),
            row![
                button(text("Cancel").size(13))
                    .on_press(Message::SidebarDeleteCancel)
                    .padding([4, 12])
                    .style(self.ghost_btn_style()),
                Space::new().width(Length::Fill),
                button(text("Delete").size(13))
                    .on_press(Message::SidebarDeleteConfirm)
                    .padding([4, 12])
                    .style(button::danger),
            ]
            .spacing(8)
            .align_y(iced::Alignment::Center),
        ]
        .spacing(10);
        let panel = container(body)
            .width(Length::Fixed(380.0))
            .padding(14)
            .style(container::dark);
        let dismiss = mouse_area(
            container(Space::new())
                .width(Length::Fill)
                .height(Length::Fill),
        )
        .on_press(Message::SidebarDeleteCancel);
        let centered = container(panel)
            .center_x(Length::Fill)
            .center_y(Length::Fill);
        stack![Element::from(dismiss), Element::from(centered)].into()
    }

    /// Centered modal: "Tab is running `<proc>`. Close anyway?". Esc / outside
    /// click cancel; only TabCloseConfirmYes proceeds with the close.
    fn tab_close_confirm_view(&self, id: usize, proc_name: &str) -> Element<'_, Message> {
        let label = self
            .sessions
            .iter()
            .find(|session| session.id == id)
            .map(|s| s.label())
            .unwrap_or_else(|| format!("Session {}", id + 1));
        let body = column![
            text(format!("Close \"{}\"?", label)).size(14),
            text(format!("Foreground process: {}", proc_name))
                .size(12)
                .style(text::secondary),
            row![
                button(text("Cancel").size(13))
                    .on_press(Message::TabCloseConfirmNo)
                    .padding([4, 12])
                    .style(self.ghost_btn_style()),
                Space::new().width(Length::Fill),
                button(text("Close anyway").size(13))
                    .on_press(Message::TabCloseConfirmYes)
                    .padding([4, 12])
                    .style(button::danger),
            ]
            .spacing(8)
            .align_y(iced::Alignment::Center),
        ]
        .spacing(10);
        let panel = container(body)
            .width(Length::Fixed(320.0))
            .padding(14)
            .style(container::dark);
        let dismiss = mouse_area(
            container(Space::new())
                .width(Length::Fill)
                .height(Length::Fill),
        )
        .on_press(Message::TabCloseConfirmNo);
        let centered = container(panel)
            .center_x(Length::Fill)
            .center_y(Length::Fill);
        stack![Element::from(dismiss), Element::from(centered)].into()
    }

    /// Counted, stable-pane confirmation for Clear Blocks. The destructive
    /// wording is deliberately explicit: clearing also discards bookmarks and
    /// captured output, and there is no undo path after this modal.
    fn block_clear_confirm_view(&self, confirm: BlockClearConfirmation) -> Element<'_, Message> {
        let count = confirm.block_count;
        let noun = if count == 1 { "block" } else { "blocks" };
        let body = column![
            text(format!("Clear {count} command {noun}?")).size(14),
            text(format!(
                "This permanently removes {count} completed command {noun}, their bookmarks, and captured output from this pane."
            ))
            .size(12)
            .wrapping(text::Wrapping::Word)
            .style(text::secondary),
            text("This cannot be undone.").size(12).style(text::danger),
            row![
                button(text("Cancel").size(13))
                    .on_press(Message::BlockClearConfirmNo)
                    .padding([4, 12])
                    .style(self.ghost_btn_style()),
                Space::new().width(Length::Fill),
                button(text(format!("Clear {count} {noun}")).size(13))
                    .on_press(Message::BlockClearConfirmYes)
                    .padding([4, 12])
                    .style(button::danger),
            ]
            .spacing(8)
            .align_y(iced::Alignment::Center),
        ]
        .spacing(10);
        let panel_width = (self.win_size.width - 32.0).clamp(240.0, 380.0);
        let panel = container(body)
            .width(Length::Fixed(panel_width))
            .padding(14)
            .style(container::dark);
        let dismiss = mouse_area(
            container(Space::new())
                .width(Length::Fill)
                .height(Length::Fill),
        )
        .on_press(Message::BlockClearConfirmNo);
        let centered = container(panel)
            .center_x(Length::Fill)
            .center_y(Length::Fill);
        stack![Element::from(dismiss), Element::from(centered)].into()
    }

    /// Bottom-right toast stack. Each toast is click-dismissable.
    fn toast_overlay(&self) -> Element<'_, Message> {
        let mut col = column![].spacing(6);
        for (idx, t) in self.toasts.iter().enumerate() {
            let accent = match t.kind {
                ToastKind::Info => self.c_accent(),
                ToastKind::Success => self.theme.ansi_color(2),
                ToastKind::Warning => self.theme.ansi_color(3),
            };
            let style_accent = accent;
            let style = move |_t: &iced::Theme| container::Style {
                background: Some(iced::Background::Color(Color {
                    a: 0.96,
                    ..Color::BLACK
                })),
                text_color: Some(Color::WHITE),
                border: iced::Border {
                    color: style_accent,
                    width: 1.0,
                    radius: 6.0.into(),
                },
                ..Default::default()
            };
            let body = container(text(t.text.clone()).size(13))
                .padding([6, 12])
                .style(style);
            let clickable = mouse_area(body).on_press(Message::ToastDismiss(idx));
            col = col.push(clickable);
        }
        // Sit just above the bottom bar — or the window edge when it's off.
        let bottom = if self.config.bottom_bar {
            STATUS_BAR_H + 12.0
        } else {
            12.0
        };
        container(col)
            .align_right(Length::Fill)
            .align_bottom(Length::Fill)
            .padding(iced::Padding::from(0).right(16.0).bottom(bottom))
            .into()
    }

    /// Persistent load diagnostics. Unlike transient toasts, these remain
    /// visible until the user fixes the underlying file (or explicitly resets
    /// the main config), so a fallback can never look like a successful load.
    fn diagnostics_overlay(&self) -> Element<'_, Message> {
        let mut content = column![text("frost needs attention").size(13)]
            .spacing(4)
            .width(Length::Fill);
        if let Some(error) = &self.config_diagnostic {
            content = content.push(
                text(error.clone())
                    .size(11)
                    .wrapping(text::Wrapping::Word)
                    .style(text::warning),
            );
            content = content.push(
                text("Auto-save is paused to preserve the file. Fix it externally or use Reset.")
                    .size(10)
                    .wrapping(text::Wrapping::Word)
                    .style(text::secondary),
            );
        }
        if let Some(error) = &self.session_diagnostic {
            content = content.push(
                text(error.clone())
                    .size(11)
                    .wrapping(text::Wrapping::Word)
                    .style(text::danger),
            );
        }
        for diagnostic in self.keybindings_diagnostics.iter().take(3) {
            content = content.push(
                text(diagnostic.clone())
                    .size(11)
                    .wrapping(text::Wrapping::Word)
                    .style(text::warning),
            );
        }
        if self.keybindings_diagnostics.len() > 3 {
            content = content.push(
                text(format!(
                    "…and {} more keybinding issue(s)",
                    self.keybindings_diagnostics.len() - 3
                ))
                .size(10)
                .style(text::secondary),
            );
        }
        let panel_width = (self.win_size.width - 32.0).clamp(240.0, 520.0);
        let panel = container(content)
            .width(Length::Fixed(panel_width))
            .padding([8, 12])
            .style(container::dark);
        container(panel)
            .align_right(Length::Fill)
            .align_top(Length::Fill)
            .padding([40, 8])
            .into()
    }

    /// Ctrl+Shift+L fuzzy tab switcher overlay (palette-style).
    fn tab_switcher_view(&self, state: &TabSwitcherState) -> Element<'_, Message> {
        let labels = self.tab_labels();
        let filtered = tab_switcher_filtered(&labels, &state.query);

        let query: Element<'_, Message> = text_input("Jump to tab…", &state.query)
            .id(TAB_SWITCHER_INPUT_ID.clone())
            .on_input(Message::TabSwitcherInput)
            .size(14)
            .into();
        let query_line = row![text("↦").size(16), query]
            .spacing(8)
            .align_y(iced::Alignment::Center);

        let mut list = column![].spacing(2);
        if filtered.is_empty() {
            list = list.push(text("No tabs match").size(13).style(text::secondary));
        } else {
            for &(pos, idx) in filtered.iter() {
                let selected = pos == state.selected;
                let Some(session) = self.sessions.get(idx) else {
                    continue;
                };
                let label = session.label();
                let id = session.id;
                let info = row![
                    text(format!("{:>2}", idx + 1))
                        .size(12)
                        .style(text::secondary),
                    text(label).size(13),
                    Space::new().width(Length::Fill),
                ]
                .spacing(10)
                .align_y(iced::Alignment::Center);
                let accent = self.c_accent();
                let body = container(info).width(Length::Fill).padding([3, 8]).style(
                    move |_t: &iced::Theme| container::Style {
                        background: if selected {
                            Some(iced::Background::Color(Color { a: 0.28, ..accent }))
                        } else {
                            None
                        },
                        border: iced::Border {
                            radius: 4.0.into(),
                            ..Default::default()
                        },
                        ..Default::default()
                    },
                );
                let row_btn = mouse_area(body).on_press(Message::TabSwitcherJump(id));
                list = list.push(row_btn);
            }
        }

        let body = column![query_line, list].spacing(8);
        let panel = container(body)
            .width(Length::Fixed(420.0))
            .max_height(420.0)
            .padding(12)
            .style(container::dark);
        let dismiss = mouse_area(
            container(Space::new())
                .width(Length::Fill)
                .height(Length::Fill),
        )
        .on_press(Message::TabSwitcherClose);
        let centered = container(panel)
            .center_x(Length::Fill)
            .center_y(Length::Fill);
        stack![Element::from(dismiss), Element::from(centered)].into()
    }

    /// Ctrl+Shift+S remote host picker overlay. Enter/click opens the entry
    /// in a new session; a host that fails validation is shown with its
    /// reason rather than hidden, so a config typo is discovered here and not
    /// by its absence.
    fn remote_picker_view(&self, selected: usize) -> Element<'_, Message> {
        let mut list = column![].spacing(2);
        if self.config.remote_hosts.is_empty() {
            list = list.push(
                text(
                    "No [[remote_hosts]] configured. Add one in Settings (Ctrl+Shift+O, Remote hosts) or in config.toml:\n\n[[remote_hosts]]  # ssh\nname = \"dev\"\nhost = \"dev.example.com\"\nuser = \"yj\"\ndeploy = \"persist\"\nssh_args = [\"-p\", \"22\"]\n\n[[remote_hosts]]  # running container\nname = \"myubuntu\"\nhost = \"myubuntu\"\ndocker = true\ndeploy = \"persist\"",
                )
                .size(13)
                .style(text::secondary),
            );
        }
        for (index, host) in self.config.remote_hosts.iter().enumerate() {
            let transport = if host.docker { "docker" } else { "ssh" };
            let deploy = if host.deploy.is_empty() {
                "off"
            } else {
                host.deploy.as_str()
            };
            match host.validate() {
                Ok(()) => {
                    let info = row![
                        text(format!("{:>2}", index + 1))
                            .size(12)
                            .style(text::secondary),
                        text(host.display_name().to_string()).size(13),
                        Space::new().width(Length::Fill),
                        text(format!("{transport} · deploy {deploy}"))
                            .size(12)
                            .style(text::secondary),
                    ]
                    .spacing(10)
                    .align_y(iced::Alignment::Center);
                    let accent = self.c_accent();
                    let highlighted = index == selected;
                    let body = container(info).width(Length::Fill).padding([3, 8]).style(
                        move |_t: &iced::Theme| container::Style {
                            background: if highlighted {
                                Some(iced::Background::Color(Color { a: 0.28, ..accent }))
                            } else {
                                None
                            },
                            border: iced::Border {
                                radius: 4.0.into(),
                                ..Default::default()
                            },
                            ..Default::default()
                        },
                    );
                    list =
                        list.push(mouse_area(body).on_press(Message::RemotePickerConnect(index)));
                }
                Err(problem) => {
                    list = list.push(
                        row![
                            text(host.display_name().to_string())
                                .size(13)
                                .style(text::secondary),
                            Space::new().width(Length::Fill),
                            text(problem).size(12).style(text::secondary),
                        ]
                        .spacing(10)
                        .align_y(iced::Alignment::Center),
                    );
                }
            }
        }
        list = list.push(
            text("deploy off connects plainly; persist/incognito bring jsh along.")
                .size(11)
                .style(text::secondary),
        );

        let body = column![
            row![text("🖧").size(16), text("Remote hosts").size(14)]
                .spacing(8)
                .align_y(iced::Alignment::Center),
            list
        ]
        .spacing(8);
        let panel = container(body)
            .width(Length::Fixed(460.0))
            .max_height(420.0)
            .padding(12)
            .style(container::dark);
        let dismiss = mouse_area(
            container(Space::new())
                .width(Length::Fill)
                .height(Length::Fill),
        )
        .on_press(Message::RemotePickerClose);
        let centered = container(panel)
            .center_x(Length::Fill)
            .center_y(Length::Fill);
        stack![Element::from(dismiss), Element::from(centered)].into()
    }

    /// Ctrl+Shift+H persisted-command history picker overlay (palette-style).
    /// Enter/click types the command into the active pane; nothing executes.
    fn history_picker_view(
        &self,
        state: &history_picker::HistoryPickerState,
    ) -> Element<'_, Message> {
        let filtered = state.filtered();

        let query: Element<'_, Message> = text_input("Recall a command…", &state.query)
            .id(HISTORY_PICKER_INPUT_ID.clone())
            .on_input(Message::HistoryPickerInput)
            .size(14)
            .into();
        let query_line = row![text("↺").size(16), query]
            .spacing(8)
            .align_y(iced::Alignment::Center);

        let mut list = column![].spacing(2);
        if filtered.is_empty() {
            let hint = if state.query.is_empty() {
                "No persisted commands yet (recorded via OSC 133 shell integration)"
            } else {
                "No commands match"
            };
            list = list.push(text(hint).size(13).style(text::secondary));
        } else {
            for (pos, record) in filtered.iter().enumerate() {
                let selected = pos == state.selected;
                let mut info =
                    row![text(history_picker::display_command(&record.command)).size(13)]
                        .spacing(10)
                        .align_y(iced::Alignment::Center);
                info = info.push(Space::new().width(Length::Fill));
                if record.exit_code != 0 {
                    info = info.push(
                        text(format!("✗ {}", record.exit_code))
                            .size(12)
                            .style(text::danger),
                    );
                }
                if let Some(cwd) = record.cwd.as_deref() {
                    info = info.push(text(abbreviate_home(cwd)).size(12).style(text::secondary));
                }
                let accent = self.c_accent();
                let body = container(info).width(Length::Fill).padding([3, 8]).style(
                    move |_t: &iced::Theme| container::Style {
                        background: if selected {
                            Some(iced::Background::Color(Color { a: 0.28, ..accent }))
                        } else {
                            None
                        },
                        border: iced::Border {
                            radius: 4.0.into(),
                            ..Default::default()
                        },
                        ..Default::default()
                    },
                );
                let row_btn =
                    mouse_area(body).on_press(Message::HistoryPickerAccept(record.command.clone()));
                list = list.push(row_btn);
            }
        }

        let body = column![query_line, list].spacing(8);
        let panel = container(body)
            .width(Length::Fixed(560.0))
            .max_height(480.0)
            .padding(12)
            .style(container::dark);
        let dismiss = mouse_area(
            container(Space::new())
                .width(Length::Fill)
                .height(Length::Fill),
        )
        .on_press(Message::HistoryPickerClose);
        let centered = container(panel)
            .center_x(Length::Fill)
            .center_y(Length::Fill);
        stack![Element::from(dismiss), Element::from(centered)].into()
    }

    /// Ctrl+Alt+F cross-block search picker overlay (palette-style). Enter or
    /// a click selects the hit's zone and reveals it; nothing executes.
    fn block_search_view(&self, state: &BlockSearchState) -> Element<'_, Message> {
        let query: Element<'_, Message> = text_input("Search command blocks…", &state.query)
            .id(BLOCK_SEARCH_INPUT_ID.clone())
            .on_input(Message::BlockSearchInput)
            .size(14)
            .into();
        let query_line = row![text("⌕").size(16), query]
            .spacing(8)
            .align_y(iced::Alignment::Center);
        let filter_btn = |label: &str, filter: BlockSearchFilter| {
            button(text(label.to_string()).size(11))
                .on_press(Message::BlockSearchSetFilter(filter))
                .padding([3, 7])
                .style(if state.filter == filter {
                    button::primary
                } else {
                    button::secondary
                })
        };
        let filters_top = row![
            filter_btn("All", BlockSearchFilter::All),
            filter_btn("Failed", BlockSearchFilter::Failed),
            filter_btn("Slow", BlockSearchFilter::Slow),
        ]
        .spacing(4)
        .align_y(iced::Alignment::Center);
        let filters_bottom = row![
            filter_btn("Bookmarked", BlockSearchFilter::Bookmarked),
            filter_btn("Background", BlockSearchFilter::Background),
        ]
        .spacing(4)
        .align_y(iced::Alignment::Center);
        let filters = column![filters_top, filters_bottom].spacing(4);

        // The count line stays outside the scrollable so it is always
        // visible; EVERY hit is drawn inside it (ember renders the full hit
        // list in a scroll area) and keyboard navigation wraps across all of
        // them, `block_search_snap_task` keeping the highlight in view.
        let mut body = column![query_line, filters].spacing(8);
        if state.loading {
            body = body.push(text("Indexing blocks…").size(13).style(text::secondary));
        } else if state.query.trim().is_empty() && state.filter == BlockSearchFilter::All {
            let hint = if state.older_not_indexed {
                "Type to search, or choose a filter · older blocks not indexed"
            } else {
                "Type to search, or choose a filter to browse blocks"
            };
            body = body.push(text(hint).size(13).style(text::secondary));
        } else if state.hits.is_empty() {
            let empty = if state.older_not_indexed {
                "No matching indexed blocks · older blocks not indexed"
            } else {
                "No matching blocks"
            };
            body = body.push(text(empty).size(13).style(text::secondary));
        } else {
            body = body.push(text(state.count_label()).size(12).style(text::secondary));
            let mut list = column![].spacing(2);
            for (pos, hit) in state.hits.iter().enumerate() {
                let selected = pos == state.selected;
                let context = if hit.command_preview.is_empty() {
                    "(no command)".to_string()
                } else {
                    hit.command_preview.clone()
                };
                let context = if hit.is_output_line {
                    format!("{context} · L{}", hit.line_no)
                } else {
                    format!("{context} · command")
                };
                let info = column![
                    text(hit.line_text.clone())
                        .size(13)
                        .wrapping(text::Wrapping::None),
                    text(context)
                        .size(11)
                        .wrapping(text::Wrapping::None)
                        .style(text::secondary),
                ]
                .spacing(1)
                .width(Length::Fill);
                let accent = self.c_accent();
                let row_body = container(info).width(Length::Fill).padding([3, 8]).style(
                    move |_t: &iced::Theme| container::Style {
                        background: if selected {
                            Some(iced::Background::Color(Color { a: 0.28, ..accent }))
                        } else {
                            None
                        },
                        border: iced::Border {
                            radius: 4.0.into(),
                            ..Default::default()
                        },
                        ..Default::default()
                    },
                );
                let row_btn =
                    mouse_area(row_body).on_press(Message::BlockSearchAccept(hit.clone()));
                list = list.push(row_btn);
            }
            body = body.push(
                scrollable(list)
                    .id(BLOCK_SEARCH_LIST_ID.clone())
                    .height(Length::Shrink),
            );
        }

        let panel_width = (self.win_size.width - 32.0).clamp(280.0, 720.0);
        let panel_height = (self.win_size.height - 32.0).clamp(180.0, 560.0);
        let panel = container(body)
            .width(Length::Fixed(panel_width))
            .max_height(panel_height)
            .padding(12)
            .style(container::dark);
        let dismiss = mouse_area(
            container(Space::new())
                .width(Length::Fill)
                .height(Length::Fill),
        )
        .on_press(Message::BlockSearchClose);
        let centered = container(panel)
            .center_x(Length::Fill)
            .center_y(Length::Fill);
        stack![Element::from(dismiss), Element::from(centered)].into()
    }

    /// Pointer-anchored actions opened by right-clicking any row of a finalized
    /// card. Existing copy/recall/export paths remain the single implementation
    /// of each action; this panel is only a stable-target UI.
    fn block_menu_view(&self, state: BlockMenuState) -> Element<'_, Message> {
        let target = self
            .sessions
            .iter()
            .find(|sess| sess.id == state.session_id)
            .and_then(|sess| {
                sess.terminal
                    .zone_by_id(state.zone_id)
                    .map(|zone| (sess, zone))
            });
        let row_btn = |label: &str, action: BlockMenuAction| {
            button(text(label.to_string()).size(12))
                .on_press(Message::BlockMenuAction(action))
                .padding([5, 9])
                .width(Length::Fill)
                .style(self.ghost_btn_style())
        };

        let mut body = column![].spacing(7);
        if let Some((sess, zone)) = target {
            let command = zone.command.as_deref().unwrap_or("Background output");
            let mut preview: String = command.chars().take(240).collect();
            if command.chars().count() > 240 {
                preview.push('…');
            }
            let outcome = block_mode::classify(zone.command.as_deref(), zone.exit_code);
            let status = block_mode::badge_text(outcome, zone.duration_ms)
                .unwrap_or_else(|| "Background output".to_string());
            let bookmarked = sess.block_bookmarks.contains(zone.id);
            let mut meta = status;
            if bookmarked {
                meta.push_str(" · ◆ bookmarked");
            }
            if let Some(cwd) = zone.cwd.as_deref() {
                meta.push_str(" · ");
                meta.push_str(cwd);
            }
            // Metadata-only: rendering an open menu must not clone/extract up
            // to 1 MiB of output every frame. Copy/AI actions do the bounded
            // extraction only when clicked.
            let retention = if zone
                .captured_output
                .as_ref()
                .is_some_and(|(_, truncated)| *truncated)
            {
                Some("Output is truncated")
            } else if zone.captured_output_evicted {
                Some(if zone.rows_evicted {
                    "Output snapshot and rows were evicted"
                } else {
                    "Output snapshot was evicted; live rows remain available"
                })
            } else {
                None
            };

            body = body
                .push(
                    row![
                        text(format!("Command Block #{}", zone.id)).size(16),
                        Space::new().width(Length::Fill),
                        button(text("×").size(13))
                            .on_press(Message::BlockMenuClose)
                            .padding([2, 7])
                            .style(self.ghost_btn_style()),
                    ]
                    .align_y(iced::Alignment::Center),
                )
                .push(
                    text(preview)
                        .size(13)
                        .wrapping(text::Wrapping::Word)
                        .width(Length::Fill),
                )
                .push(
                    text(meta)
                        .size(11)
                        .wrapping(text::Wrapping::Word)
                        .style(text::secondary),
                );
            if let Some(retention) = retention {
                body = body.push(text(retention).size(11).style(text::warning));
            }
            if sess.projection_policy.is_collapsed(zone.id) {
                body = body.push(row_btn("Expand output", BlockMenuAction::ExpandOutput));
            } else if !sess.projection_policy.is_collapsed(zone.id)
                && sess.terminal.finished_output_range(zone.id).is_some()
            {
                body = body.push(row_btn("Collapse output", BlockMenuAction::CollapseOutput));
            }
            let selection = block_menu_selection_summary(
                sess.terminal
                    .command_zones
                    .iter()
                    .map(|zone| (zone.id, zone.command.as_deref())),
                &sess.block_selection,
                zone.id,
            );
            body = if selection.has_selected_commands {
                body.push(
                    row![
                        row_btn(
                            if selection.selected_count > 1 {
                                "Copy commands"
                            } else {
                                "Copy command"
                            },
                            BlockMenuAction::CopyCommand,
                        ),
                        row_btn("Ask AI about block", BlockMenuAction::AskAi),
                    ]
                    .spacing(6),
                )
            } else {
                body.push(row_btn("Ask AI about block", BlockMenuAction::AskAi))
            };
            // Failed completed blocks expose the ember-style action chain:
            // fresh Fix/Explain Agent tasks and a guarded semantic Retry.
            if matches!(outcome, block_mode::BlockOutcome::Failed(_)) {
                body = body.push(
                    row![
                        row_btn("Fix with Agent", BlockMenuAction::FixWithAgent),
                        row_btn("Explain with Agent", BlockMenuAction::ExplainWithAgent),
                        row_btn("Retry", BlockMenuAction::Retry),
                    ]
                    .spacing(6),
                );
                // The experimental dashboard adds worktree task creation for
                // the same failed block (ember's Create task).
                if self.config.experimental_task_sidebar {
                    body = body.push(row_btn("Create task", BlockMenuAction::CreateTask));
                }
            }
            body = body.push(
                row![
                    row_btn(
                        if selection.selected_count > 1 {
                            "Copy outputs"
                        } else if selection.clicked_has_command {
                            "Copy output"
                        } else {
                            "Copy background output"
                        },
                        BlockMenuAction::CopyOutput,
                    ),
                    row_btn(
                        if selection.selected_count > 1 {
                            "Copy blocks"
                        } else {
                            "Copy block"
                        },
                        BlockMenuAction::CopyBlock,
                    ),
                ]
                .spacing(6),
            );
            body = body.push(row_btn(
                if selection.selected_count > 1 {
                    "Copy blocks as Markdown"
                } else {
                    "Copy block as Markdown"
                },
                BlockMenuAction::CopyMarkdown,
            ));
            if selection.clicked_has_command {
                body = body.push(row_btn(
                    "Recall this command",
                    BlockMenuAction::RecallCommand,
                ));
            }
            if selection.has_selected_commands {
                body = body.push(row_btn(
                    if selection.selected_count > 1 {
                        "Reinput selected commands"
                    } else {
                        "Reinput command"
                    },
                    BlockMenuAction::ReinputSelected,
                ));
            }
            body = body
                .push(
                    row![
                        row_btn(
                            if bookmarked {
                                "Remove bookmark"
                            } else {
                                "Bookmark block"
                            },
                            BlockMenuAction::ToggleBookmark,
                        ),
                        row_btn("Search blocks", BlockMenuAction::Search),
                    ]
                    .spacing(6),
                )
                .push(
                    row![
                        row_btn("Jump to top", BlockMenuAction::JumpTop),
                        row_btn("Jump to bottom", BlockMenuAction::JumpBottom),
                    ]
                    .spacing(6),
                )
                .push(
                    row![
                        row_btn(
                            "Export this block · Markdown",
                            BlockMenuAction::ExportMarkdown
                        ),
                        row_btn("Export this block · JSON", BlockMenuAction::ExportJson),
                    ]
                    .spacing(6),
                )
                .push(
                    button(text("Clear all completed blocks…").size(12))
                        .on_press(Message::BlockMenuAction(BlockMenuAction::Clear))
                        .padding([5, 9])
                        .width(Length::Fill)
                        .style(button::danger),
                );
        } else {
            body = body
                .push(text("Command block is no longer available").size(14))
                .push(
                    button(text("Close").size(12))
                        .on_press(Message::BlockMenuClose)
                        .style(self.ghost_btn_style()),
                );
        }

        let panel_width = (self.win_size.width - 12.0).clamp(1.0, 360.0);
        let panel_height = (self.win_size.height - TAB_BAR_H - 12.0).clamp(1.0, 520.0);
        let panel = container(scrollable(body).height(Length::Shrink))
            .width(Length::Fixed(panel_width))
            .max_height(panel_height)
            .padding(12)
            .style(container::dark);
        let dismiss = mouse_area(
            container(Space::new())
                .width(Length::Fill)
                .height(Length::Fill),
        )
        .on_press(Message::BlockMenuClose);
        let position = anchored_overlay_position(
            state.anchor,
            self.win_size,
            iced::Size::new(panel_width, panel_height),
            TAB_BAR_H,
        );
        let placed = container(panel)
            .align_left(Length::Fill)
            .align_top(Length::Fill)
            .padding(iced::Padding::from(0).top(position.y).left(position.x));
        stack![Element::from(dismiss), Element::from(placed)].into()
    }

    /// Family-wide bottom bar (`jterm_core::bottom_bar`): cwd and git on the
    /// left; last-command status, grid size, and tab count on the right.
    fn status_bar(&self) -> Element<'_, Message> {
        let sess = self.sessions.get(self.active);
        let cwd = sess
            .and_then(|s| s.cwd_cache.as_deref())
            .map(std::path::Path::new);
        let home = std::env::var_os("HOME").map(std::path::PathBuf::from);
        // Report the active pane's own grid size; when split it differs from the
        // whole-window `self.cols`×`self.rows`.
        let (grid_cols, grid_rows) = sess
            .map(|s| (s.terminal.grid.cols(), s.terminal.grid.rows()))
            .unwrap_or((self.cols, self.rows));
        let snapshot = jterm_core::bottom_bar::Snapshot {
            cwd,
            home: home.as_deref(),
            git: sess.and_then(|s| s.git_meta_cache.as_ref()),
            running: sess.is_some_and(|s| s.terminal.is_command_running()),
            last_exit: sess.and_then(|s| s.last_exit),
            last_duration_ms: sess.and_then(|s| s.last_duration_ms),
            cols: grid_cols as u16,
            rows: grid_rows as u16,
            tab_index: self.active_tab,
            tab_count: self.tabs.len(),
        };
        let content = jterm_core::bottom_bar::compose(&snapshot);

        // One label per segment, colored by its tone — the renderer contract.
        let segment = |seg: &jterm_core::bottom_bar::Segment| {
            text(seg.text.clone())
                .size(11)
                .color(Theme::rgb_to_color32(seg.tone.color(&self.theme)))
        };
        let mut left = row![].spacing(12).align_y(iced::Alignment::Center);
        for seg in &content.left {
            left = left.push(segment(seg));
        }
        let mut right = row![].spacing(12).align_y(iced::Alignment::Center);
        for seg in &content.right {
            right = right.push(segment(seg));
        }

        let bar = row![left, Space::new().width(Length::Fill), right]
            .spacing(12)
            .align_y(iced::Alignment::Center);
        container(bar)
            .width(Length::Fill)
            .height(Length::Fixed(STATUS_BAR_H))
            .padding([0, 10])
            .align_y(iced::Alignment::Center)
            .style(self.chrome_bar_style())
            .into()
    }

    /// One-line offer to install or update jsh, shown under the tab bar only
    /// while the background check has something actionable and the user has not
    /// waved it away.
    fn jsh_notice(&self) -> Option<Element<'_, Message>> {
        if self.jsh_notice_dismissed {
            return None;
        }
        let prompt = self.jsh_prompt.as_ref()?;
        let dim = self.c_text_dim();
        let bar = row![
            text(prompt.banner_title())
                .size(12)
                .style(move |_t: &iced::Theme| text::Style { color: Some(dim) }),
            Space::new().width(Length::Fill),
            button(text(prompt.button_label()).size(12))
                .on_press(Message::JshInstall)
                .padding([3, 9])
                .style(self.ghost_btn_style()),
            button(text("×").size(12))
                .on_press(Message::JshNoticeDismiss)
                .padding([3, 9])
                .style(self.ghost_btn_style()),
        ]
        .spacing(8)
        .align_y(iced::Alignment::Center);
        Some(
            container(bar)
                .width(Length::Fill)
                .padding([2, 10])
                .style(self.chrome_bar_style())
                .into(),
        )
    }

    /// The live block badge when it fits over blank cells on `viewport_row`,
    /// paired with the elapsed time that produced it. Paint and subscription
    /// gating share this check so a hidden badge never owns a redraw timer.
    fn block_badge_cell_char(cell: &terminal::TerminalCell) -> char {
        if cell.flags.wide_continuation() {
            // A continuation cell carries no standalone character, but it is
            // still occupied by the wide glyph immediately to its left.
            '\u{fffd}'
        } else {
            cell.character
        }
    }

    fn fitting_running_badge(&self, sess: &Session, viewport_row: usize) -> Option<(String, u64)> {
        let elapsed_ms = sess.terminal.running_duration_ms()?;
        let badge = block_mode::running_badge_text(elapsed_ms);
        let inset = ((terminal_view::SCROLLBAR_WIDTH + 16.0) / self.metrics.cell_w.max(1.0)).ceil()
            as usize;
        let chars: Vec<char> = sess
            .projection
            .cells()
            .get(viewport_row)?
            .iter()
            .map(Self::block_badge_cell_char)
            .collect();
        block_mode::badge_fits(&chars, badge.chars().count() + inset).then_some((badge, elapsed_ms))
    }

    /// Block-mode metadata for each of `sess`'s visible rows: card grouping,
    /// real (not viewport-clipped) edges, state/tint, outcome stripes and
    /// first-row badges. Raw zone anchors enter the snapshot only through exact
    /// projected origins; structural padding and stale rows fail closed.
    /// Empty when the feature is off or a full-screen app owns the grid.
    fn block_paint_rows(&self, sess: &Session) -> Vec<terminal_view::BlockPaintRow> {
        use block_mode::BlockOutcome;

        if !self.config.block_mode || sess.terminal.is_alt_buffer_active() {
            return Vec::new();
        }
        let rows = sess.projection.cells().len();
        if rows == 0 {
            return Vec::new();
        }
        let terminal = &sess.terminal;
        let total = terminal.scrollback_len() + terminal.grid.rows();
        let running = terminal.running_zone_start();
        let live_prompt = terminal.live_prompt_row();
        let live_boundary = running.or(live_prompt).unwrap_or(total);

        // Zones whose rows were trimmed away have no rows to paint (their
        // clamped prompt_start would corrupt the span arithmetic).
        let zones: Vec<&terminal::CommandZone> = terminal
            .command_zones
            .iter()
            .filter(|zone| !zone.rows_evicted)
            .collect();
        let starts: Vec<usize> = zones.iter().map(|zone| zone.prompt_start).collect();
        let spans = block_mode::spans(&starts, live_boundary);
        // UI/navigation accent stays independent of OSC 12 cursor overrides,
        // matching the semantic Block palette in Anvil/Forge.
        let accent = self.c_accent();
        let mut paint = vec![terminal_view::BlockPaintRow::default(); rows];
        let view_absolute_rows: Vec<_> = (0..rows)
            .map(|row| sess.projection.view_row_absolute(row))
            .collect();
        let mut memberships = projected_zone_memberships(&view_absolute_rows, &spans);
        let zone_indexes: std::collections::HashMap<u64, usize> = zones
            .iter()
            .enumerate()
            .map(|(index, zone)| (zone.id, index))
            .collect();
        let mut summary_bounds = vec![(None, None); zones.len()];
        for (view_row, kind) in sess.projection.row_kinds().iter().enumerate() {
            let terminal::ProjectedRowKind::CollapsedSummary { key, .. } = kind else {
                continue;
            };
            let Some(&zone_index) = zone_indexes.get(&key.zone_id) else {
                continue;
            };
            let zone = zones[zone_index];
            memberships.rows[view_row] =
                Some((zone_index, zone.output_start.unwrap_or(zone.prompt_start)));
            let (first, last) = &mut summary_bounds[zone_index];
            *first = Some(first.map_or(view_row, |row: usize| row.min(view_row)));
            *last = Some(last.map_or(view_row, |row: usize| row.max(view_row)));
        }
        let zone_view_edges: Vec<_> = spans
            .iter()
            .enumerate()
            .map(|(zone_index, &(zone_start, zone_end))| {
                let visible_raw_top = terminal
                    .raw_row_id_at_absolute(zone_start)
                    .and_then(|row| sess.projection.raw_row_view_bounds(row))
                    .map(|(first, _)| first);
                let top = projected_card_real_top(
                    visible_raw_top,
                    summary_bounds[zone_index].0,
                    block_mode::classify(
                        zones[zone_index].command.as_deref(),
                        zones[zone_index].exit_code,
                    ),
                );
                let bottom = if sess
                    .projection
                    .effective_collapsed()
                    .contains(&zones[zone_index].id)
                {
                    // The synthetic summary is the real visual bottom even
                    // when a same-row command prefix leaves the raw row mapped.
                    summary_bounds[zone_index].1.or_else(|| {
                        zone_end
                            .checked_sub(1)
                            .and_then(|row| terminal.raw_row_id_at_absolute(row))
                            .and_then(|row| sess.projection.raw_row_view_bounds(row))
                            .map(|(_, last)| last)
                    })
                } else {
                    zone_end
                        .checked_sub(1)
                        .and_then(|row| terminal.raw_row_id_at_absolute(row))
                        .and_then(|row| sess.projection.raw_row_view_bounds(row))
                        .map(|(_, last)| last)
                };
                (top, bottom)
            })
            .collect();

        // Populate all finished-card rows in one projection-order sweep. The
        // old per-zone `filter().collect()` rescanned and allocated for every
        // zone (up to 256 * viewport rows on every frame).
        for (view_row, membership) in memberships.rows.iter().copied().enumerate() {
            let Some((zone_index, absolute_row)) = membership else {
                continue;
            };
            let zone = zones[zone_index];
            let (zone_start, _) = spans[zone_index];
            let (top_view, bottom_view) = zone_view_edges[zone_index];
            let outcome = block_mode::classify(zone.command.as_deref(), zone.exit_code);
            let color = match outcome {
                BlockOutcome::Success => self.theme.ansi_color(2),
                BlockOutcome::Failed(_) => self.theme.ansi_color(1),
                BlockOutcome::Unknown => self.theme.ansi_color(3),
                BlockOutcome::Background => accent,
            };
            let card_kind = match outcome {
                BlockOutcome::Success => terminal_view::BlockCardKind::Finished,
                BlockOutcome::Failed(_) => terminal_view::BlockCardKind::Failed,
                BlockOutcome::Unknown => terminal_view::BlockCardKind::Unknown,
                BlockOutcome::Background => terminal_view::BlockCardKind::Background,
            };
            let selected = sess.block_selection.contains(zone.id);
            let active = sess.block_selection.active() == Some(zone.id);
            let row = &mut paint[view_row];
            row.selectable = true;
            row.zone_id = Some(zone.id);
            row.header_end_col = block_mode::finished_header_end_col(
                zone.command
                    .as_deref()
                    .is_some_and(|command| !command.trim().is_empty()),
                zone_start,
                zone.output_start,
                zone.output_start_col,
                absolute_row,
            );
            if let Some(terminal::ProjectedRowKind::CollapsedSummary {
                key,
                hidden_display_rows,
                ..
            }) = sess.projection.row_kinds().get(view_row)
            {
                row.header_end_col = 0;
                row.collapsed_summary = Some(terminal_view::CollapsedSummaryPaint {
                    key: *key,
                    hidden_display_rows: *hidden_display_rows,
                });
            }
            row.stripe = Some(color);
            row.stripe_strong = active;
            // Zero is reserved for the live/input card below. The value is
            // viewport-local; stable interactions continue to key on the
            // terminal zone id rather than this paint grouping.
            row.card_group = Some(zone_index + 1);
            row.card_kind = card_kind;
            row.card_top = top_view == Some(view_row);
            row.card_bottom = bottom_view == Some(view_row);
            row.card_selected = selected;
            row.card_selection_active = active;
        }

        // Separators and badges exist only on a zone's real projected top, so
        // this bounded zone pass never scans viewport rows.
        for (zone_index, zone) in zones.iter().copied().enumerate() {
            let Some(top_view) = zone_view_edges[zone_index].0 else {
                continue;
            };
            let outcome = block_mode::classify(zone.command.as_deref(), zone.exit_code);
            let color = match outcome {
                BlockOutcome::Success => self.theme.ansi_color(2),
                BlockOutcome::Failed(_) => self.theme.ansi_color(1),
                BlockOutcome::Unknown => self.theme.ansi_color(3),
                BlockOutcome::Background => accent,
            };
            let active = sess.block_selection.active() == Some(zone.id);
            let row = &mut paint[top_view];
            row.separator = true;
            row.bookmarked = sess.block_bookmarks.contains(zone.id);
            if let Some(plain) = block_mode::badge_text(outcome, zone.duration_ms) {
                // The selected block's badge appends its LOCAL finish time.
                // If the suffix no longer fits, retain the plain badge.
                let suffixed = zone.finished_at_ms.filter(|_| active).map(|ms| {
                    let offset = block_mode::local_offset_secs((ms / 1000) as i64);
                    format!("{plain} · {}", block_mode::clock_at_offset(ms, offset))
                });
                let inset = ((terminal_view::SCROLLBAR_WIDTH + 16.0) / self.metrics.cell_w.max(1.0))
                    .ceil() as usize;
                let chars: Vec<char> = sess.projection.cells()[top_view]
                    .iter()
                    .map(Self::block_badge_cell_char)
                    .collect();
                for badge in suffixed.into_iter().chain(std::iter::once(plain)) {
                    let needed = badge.chars().count() + inset;
                    if block_mode::badge_fits(&chars, needed) {
                        row.badge = Some((badge, color));
                        break;
                    }
                }
            }
        }

        // Running output and the editable prompt are one native iced active
        // card. Match the family's six-row input minimum and grow only through
        // the current output/cursor row, without changing Frost's PTY or cell
        // geometry.
        if let Some(active_start) = running.or(live_prompt) {
            let active_extent_row = terminal.active_app_extent_row().unwrap_or_else(|| {
                terminal
                    .scrollback_len()
                    .saturating_add(terminal.get_cursor_pos().0)
            });
            let active_end = active_extent_row
                .saturating_add(1)
                .max(active_start.saturating_add(block_mode::MIN_INPUT_ROWS))
                .min(total);
            let active_top_view = terminal
                .raw_row_id_at_absolute(active_start)
                .and_then(|row| sess.projection.raw_row_view_bounds(row))
                .map(|(first, _)| first);
            let active_bottom_view = active_end
                .checked_sub(1)
                .and_then(|row| terminal.raw_row_id_at_absolute(row))
                .and_then(|row| sess.projection.raw_row_view_bounds(row))
                .map(|(_, last)| last);
            for (view_row, absolute_row) in view_absolute_rows.iter().copied().enumerate() {
                if absolute_row.is_some_and(|row| row >= active_start && row < active_end) {
                    let row = &mut paint[view_row];
                    row.app_eligible = true;
                    row.stripe = Some(accent);
                    row.card_group = Some(0);
                    row.card_kind = terminal_view::BlockCardKind::Active;
                    row.card_top = active_top_view == Some(view_row);
                    row.card_bottom = active_bottom_view == Some(view_row);
                }
            }
            if let Some(active_top_view) = active_top_view {
                let row = &mut paint[active_top_view];
                row.separator = true;
                if running.is_some() {
                    if let Some((badge, _)) = self.fitting_running_badge(sess, active_top_view) {
                        row.badge = Some((badge, accent));
                    }
                }
            }
        }
        paint
    }

    /// Scrollbar-track fractions of each FAILED zone's first row, for the red
    /// failure markers painted along the scrollbar. Same gates as
    /// [`Self::block_paint_rows`] (feature off / alt screen → empty), and the
    /// same red as the Failed stripe. The 256-zone cap keeps this scan
    /// trivial.
    fn block_marker_fractions(&self, sess: &Session) -> Vec<f32> {
        if !self.config.block_mode || sess.terminal.is_alt_buffer_active() {
            return Vec::new();
        }
        if sess.projection.mode() == terminal::ProjectionMode::Transformed {
            return Vec::new();
        }
        let terminal = &sess.terminal;
        let total = terminal.scrollback_len() + terminal.grid.rows();
        let failed: Vec<usize> = terminal
            .command_zones
            .iter()
            .filter(|zone| {
                // Rows-evicted zones have no track position to mark.
                !zone.rows_evicted
                    && matches!(
                        block_mode::classify(zone.command.as_deref(), zone.exit_code),
                        block_mode::BlockOutcome::Failed(_)
                    )
            })
            .map(|zone| zone.prompt_start)
            .collect();
        block_mode::marker_fractions(&failed, total)
    }

    fn block_bookmark_marker_fractions(&self, sess: &Session) -> Vec<f32> {
        if !self.config.block_mode || sess.terminal.is_alt_buffer_active() {
            return Vec::new();
        }
        // Raw document fractions are invalid once rows have been vertically
        // projected. Hide them until markers carry stable raw origins.
        if sess.projection.mode() == terminal::ProjectionMode::Transformed {
            return Vec::new();
        }
        let total = sess.terminal.scrollback_len() + sess.terminal.grid.rows();
        let rows: Vec<usize> = sess
            .terminal
            .command_zones
            .iter()
            .filter(|zone| !zone.rows_evicted && sess.block_bookmarks.contains(zone.id))
            .map(|zone| zone.prompt_start)
            .collect();
        block_mode::marker_fractions(&rows, total)
    }

    /// Build the terminal widget for the pane showing `sess_idx`.
    /// Overlay-style decorations (search, links, Kitty images) are only attached
    /// to the active pane; the other panes render plain.
    fn pane_view(&self, sess_idx: usize) -> Element<'_, Message> {
        let sess = &self.sessions[sess_idx];
        let session_id = sess.id;
        let is_active = sess_idx == self.active;
        // An open overlay input owns the keyboard and IME, so the terminal pane
        // renders unfocused (no blinking cursor, no competing IME request).
        let focused = self.focused && is_active && self.terminal_input_active();
        // Only walk the grid to build per-row selection spans when a selection
        // actually exists; otherwise hand the widget an empty Vec (no highlight).
        let selection: Vec<Option<(usize, usize)>> = if sess.terminal.has_text_selection() {
            (0..sess.projection.cells().len())
                .map(|r| {
                    sess.terminal
                        .row_selection_cols_in_projection(&sess.projection, r)
                })
                .collect()
        } else {
            Vec::new()
        };
        // Only paint match highlights while the search bar is open; otherwise
        // stale matches (whose line indices drift as the grid scrolls) linger.
        let (search_matches, current) = if is_active && self.search.is_open {
            let visible = self
                .search
                .matches
                .iter()
                .filter_map(|matched| {
                    project_search_match(&sess.terminal, &sess.projection, matched)
                })
                .collect();
            let current = self
                .search
                .current_match()
                .and_then(|matched| {
                    project_search_match(&sess.terminal, &sess.projection, &matched)
                })
                .map(|m| (m.line, m.col_start));
            (visible, current)
        } else {
            (Vec::new(), None)
        };
        let links: &[link::Link] = if is_active { &self.links } else { &[] };
        let images = if is_active {
            self.kitty_images(sess)
        } else {
            Vec::new()
        };
        let blocks = self.block_paint_rows(sess);
        let app_mouse_full_grid = app_mouse_uses_full_grid(
            self.config.block_mode,
            sess.terminal.is_alt_buffer_active(),
            sess.terminal.has_usable_block_partitions(),
        );
        TermWidget::new(
            sess.projection.cells(),
            sess.cursor,
            sess.cursor_visible,
            sess.terminal.cursor_shape,
            focused,
            &self.theme,
            self.metrics,
            self.mono,
            self.cjk_mono,
            self.symbol_mono,
            self.math_symbol,
            self.nerd_symbol,
            selection,
            sess.projection.scroll_offset(),
            sess.projection.max_scroll_offset(),
        )
        .modifiers(
            self.modifiers.shift(),
            self.modifiers.alt(),
            self.modifiers.control(),
        )
        .scrollbar_always(matches!(
            self.config.scrollbar_visibility,
            config::ScrollbarVisibility::Always
        ))
        .search(search_matches, current)
        .blocks(blocks)
        .app_mouse_full_grid(app_mouse_full_grid)
        .block_compact(self.config.block_compact)
        .block_markers(self.block_marker_fractions(sess))
        .block_bookmark_markers(self.block_bookmark_marker_fractions(sess))
        .links(links, sess.projection.view_revision())
        .dynamic_palette(&sess.terminal.dynamic_palette)
        .dynamic_defaults(
            sess.terminal.dynamic_fg,
            sess.terminal.dynamic_bg,
            sess.terminal.dynamic_cursor_color,
        )
        .images(images)
        .preedit(if focused && !sess.terminal.preedit_text.is_empty() {
            Some((
                sess.terminal.preedit_text.clone(),
                sess.terminal.preedit_selection.clone(),
            ))
        } else {
            None
        })
        .blink_on(self.blink_on)
        .opacity(self.config.opacity)
        .on_mouse(move |inp| Message::MousePane(session_id, inp))
        .on_summary(sess.projection.key(), move |activation| {
            Message::SummaryActivate(session_id, activation)
        })
        .into()
    }

    /// Left dock. A header lets the user switch between the file tree and the
    /// vertical tab list and dock the tab strip back to the top.
    fn sidebar_view(&self) -> Element<'_, Message> {
        // Panel switcher: highlight the active panel.
        let panel_btn = |label: &str, panel: SidebarPanel| {
            let active = self.sidebar_panel == panel;
            button(text(label.to_string()).size(12))
                .on_press(Message::SetSidebarPanel(panel))
                .padding([2, 8])
                .style(self.tab_btn_style(active))
        };
        let header = row![
            panel_btn("Tabs", SidebarPanel::Tabs),
            panel_btn("Files", SidebarPanel::Files),
            Space::new().width(Length::Fill),
        ]
        .spacing(4)
        .align_y(iced::Alignment::Center);
        // The experimental Tasks dashboard joins the switcher only when the
        // feature flag is on; with the flag off no task UI is reachable.
        let header = if self.config.experimental_task_sidebar {
            row![
                panel_btn("Tabs", SidebarPanel::Tabs),
                panel_btn("Files", SidebarPanel::Files),
                panel_btn("Tasks", SidebarPanel::Tasks),
                Space::new().width(Length::Fill),
            ]
            .spacing(4)
            .align_y(iced::Alignment::Center)
        } else {
            header
        };
        let header = container(header).padding([4, 6]);

        let panel: Element<'_, Message> = match self.sidebar_panel {
            SidebarPanel::Tabs => self.sidebar_tabs_view(),
            SidebarPanel::Files => self.sidebar_files_view(),
            SidebarPanel::Tasks => self.sidebar_tasks_view(),
        };

        container(column![header, panel].spacing(2))
            .width(Length::Fixed(self.dock_width))
            .height(Length::Fill)
            .style(self.panel_style())
            .into()
    }

    /// Invisible resize grips hugging the window border, stacked over the rest
    /// of the UI. An undecorated window has no frame to grab, so without these
    /// the window could not be resized at all.
    ///
    /// Only the strips themselves react: the middle of the overlay is bare
    /// [`Space`], which captures nothing, so `stack` passes those events down
    /// to the terminal underneath.
    fn window_resize_edges(&self) -> Element<'_, Message> {
        use iced::mouse::Interaction;
        use iced::window::Direction;

        fn grip<'a>(
            width: Length,
            height: Length,
            direction: Direction,
            cursor: Interaction,
        ) -> Element<'a, Message> {
            mouse_area(container(Space::new()).width(width).height(height))
                .on_press(Message::WindowResizeDrag(direction))
                .interaction(cursor)
                .into()
        }
        let corner = Length::Fixed(RESIZE_CORNER);
        let edge = Length::Fixed(RESIZE_EDGE);

        // Corners come first in each row so a diagonal drag wins over the
        // edge it overlaps.
        let top: Element<'_, Message> = row![
            grip(
                corner,
                edge,
                Direction::NorthWest,
                Interaction::ResizingDiagonallyDown
            ),
            grip(
                Length::Fill,
                edge,
                Direction::North,
                Interaction::ResizingVertically
            ),
            grip(
                corner,
                edge,
                Direction::NorthEast,
                Interaction::ResizingDiagonallyUp
            ),
        ]
        .width(Length::Fill)
        .into();
        let middle: Element<'_, Message> = row![
            grip(
                edge,
                Length::Fill,
                Direction::West,
                Interaction::ResizingHorizontally
            ),
            Space::new().width(Length::Fill).height(Length::Fill),
            grip(
                edge,
                Length::Fill,
                Direction::East,
                Interaction::ResizingHorizontally
            ),
        ]
        .width(Length::Fill)
        .height(Length::Fill)
        .into();
        let bottom: Element<'_, Message> = row![
            grip(
                corner,
                edge,
                Direction::SouthWest,
                Interaction::ResizingDiagonallyUp
            ),
            grip(
                Length::Fill,
                edge,
                Direction::South,
                Interaction::ResizingVertically
            ),
            grip(
                corner,
                edge,
                Direction::SouthEast,
                Interaction::ResizingDiagonallyDown
            ),
        ]
        .width(Length::Fill)
        .into();

        // Every row has to fill the width explicitly: a `Row` defaults to
        // `Shrink`, which would collapse the fill between the grips and bunch
        // them all against the left border instead of hugging it.
        column![top, middle, bottom]
            .width(Length::Fill)
            .height(Length::Fill)
            .into()
    }

    /// Draggable vertical strip between the dock and the terminal body. Pressing
    /// it starts a width-resize drag (continued via the row's `on_move`).
    fn sidebar_divider(&self) -> Element<'_, Message> {
        let strip = container(Space::new())
            .width(Length::Fixed(DIVIDER))
            .height(Length::Fill);
        mouse_area(strip.style(self.divider_style(self.dragging_sidebar)))
            .on_press(Message::SidebarDragStart)
            .interaction(iced::mouse::Interaction::ResizingHorizontally)
            .into()
    }

    /// File-tree panel body. Directories toggle expand/collapse on click; files
    /// type their (quoted) path into the active terminal. A location picker
    /// switches the tree between this machine and the configured remote hosts;
    /// right-clicking a row (or the empty area below it) opens the file-ops
    /// menu.
    fn sidebar_files_view(&self) -> Element<'_, Message> {
        let title = self
            .sidebar
            .current_dir
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("/")
            .to_string();
        let mut up = button(text("↑").size(12))
            .padding([2, 6])
            .style(self.ghost_btn_style());
        if self.sidebar.current_dir.parent().is_some() {
            up = up.on_press(Message::SidebarGoParent);
        }
        let header = row![
            up,
            text(title)
                .size(12)
                .font(iced::Font {
                    weight: iced::font::Weight::Bold,
                    ..iced::Font::DEFAULT
                })
                .width(Length::Fill),
            button(text("↻").size(12))
                .on_press(Message::SidebarRefresh)
                .padding([2, 6])
                .style(self.ghost_btn_style()),
        ]
        .spacing(4)
        .align_y(iced::Alignment::Center);
        let mut rows: Vec<Element<'_, Message>> = vec![container(header).padding([4, 6]).into()];
        // The location picker only exists once at least one remote host is
        // configured; with none, "Local" would be the only entry.
        if !self.config.remote_hosts.is_empty() {
            let mut choices = vec![SidebarLocationChoice {
                location: remote_fs::FsLocation::Local,
                label: remote_fs::FsLocation::Local.label(&self.config.remote_hosts),
            }];
            for index in 0..self.config.remote_hosts.len() {
                let location = remote_fs::FsLocation::Remote(index);
                choices.push(SidebarLocationChoice {
                    label: location.label(&self.config.remote_hosts),
                    location,
                });
            }
            let selected = choices
                .iter()
                .find(|choice| choice.location == self.sidebar.location)
                .cloned()
                .or_else(|| choices.first().cloned());
            let picker = pick_list(choices, selected, |choice| {
                Message::SidebarSetLocation(choice.location)
            })
            .text_size(12)
            .width(Length::Fill);
            rows.push(container(picker).padding([0, 6]).into());
        }
        // Transient op feedback (validation failures, ssh errors) lives in the
        // panel itself, where the user is looking.
        if let Some(notice) = &self.sidebar_notice {
            rows.push(
                container(
                    text(notice.clone())
                        .size(11)
                        .wrapping(text::Wrapping::Word)
                        .style(text::danger),
                )
                .padding([2, 8])
                .into(),
            );
        }
        match &self.sidebar.root.state {
            sidebar::DirectoryState::Loading => rows.push(
                container(text("Loading…").size(11).style(text::secondary))
                    .padding([4, 8])
                    .into(),
            ),
            sidebar::DirectoryState::Error(error) => rows.push(
                container(
                    text(error.clone())
                        .size(11)
                        .wrapping(text::Wrapping::Word)
                        .style(text::danger),
                )
                .padding([4, 8])
                .into(),
            ),
            sidebar::DirectoryState::Loaded if self.sidebar.root.children.is_empty() => rows.push(
                container(text("Empty directory").size(11).style(text::secondary))
                    .padding([4, 8])
                    .into(),
            ),
            _ => {}
        }
        for child in &self.sidebar.root.children {
            self.collect_sidebar_nodes(child, 0, &mut rows);
        }
        let list = iced::widget::Column::with_children(rows).spacing(1);
        let list: Element<'_, Message> = scrollable(list).height(Length::Fill).into();
        // Right-press on the empty area below the tree targets the root dir;
        // row menus are captured by the rows' own mouse areas first.
        mouse_area(list)
            .on_right_press(Message::SidebarMenuOpenRoot)
            .into()
    }

    /// Vertical session tab list shown in the dock. Mirrors the top tab strip:
    /// click to select, hover to reveal close, and a trailing "new tab" button.
    fn sidebar_tabs_view(&self) -> Element<'_, Message> {
        let mut list = column![].spacing(2).padding([2, 4]);
        for i in 0..self.tabs.len() {
            let id = self.tabs[i].id;
            let active = i == self.active_tab;
            let label = self.tab_label(i);
            let label = if label.chars().count() > 22 {
                let truncated: String = label.chars().take(21).collect();
                format!("{truncated}…")
            } else {
                label
            };
            let hovered = self.hovered_tab == Some(id);
            let dragging_this = self.dragging_tab == Some(id);
            let label = format!("{}{label}", self.tab_state_prefix(i));
            let tab_label = container(text(label).size(13).wrapping(text::Wrapping::None))
                .width(Length::Fill)
                .padding([4, 8]);
            // Right-click opens the same tab menu the top strip uses — this is
            // the tab list in Side mode, so it must offer the same actions.
            let tab: Element<'_, Message> = mouse_area(tab_label)
                .on_press(Message::TabDragStart(id))
                .on_release(Message::TabDragEnd(id))
                .on_right_press(Message::TabMenuOpen(id))
                .into();
            // Reveal the close button on the active or hovered tab only.
            let show_close = active || hovered;
            let close_inner: Element<'_, Message> = if show_close {
                button(text("×").size(13))
                    .on_press(Message::CloseTab(id))
                    .padding([4, 6])
                    .style(self.tab_close_btn_style())
                    .into()
            } else {
                Space::new().into()
            };
            let close = container(close_inner)
                .width(Length::Fixed(24.0))
                .center_x(Length::Fixed(24.0));
            let cell = container(row![tab, close].align_y(iced::Alignment::Center))
                .width(Length::Fill)
                .style(self.tab_container_style(active, hovered, dragging_this));
            list = list.push(
                mouse_area(cell)
                    .on_enter(Message::TabHover(Some(id)))
                    .on_exit(Message::TabHover(None)),
            );
        }
        // A compact, flat "+ New" sits apart from the filled tab rows so it does
        // not read as just another tab.
        let new_tab = container(
            button(text("+ New").size(12))
                .on_press(Message::NewSession)
                .padding([2, 10])
                .style(self.ghost_btn_style()),
        )
        .width(Length::Fill)
        .center_x(Length::Fill)
        .padding([4, 0]);
        list = list.push(new_tab);
        if self.pane_drag.is_some() {
            list = list.push(self.pane_to_tab_drop_hint(true));
        }
        let list: Element<'_, Message> = scrollable(list).height(Length::Fill).into();
        if self.pane_drag.is_some() {
            mouse_area(list)
                .on_release(Message::PanePromoteToTab(None))
                .interaction(iced::mouse::Interaction::Grabbing)
                .into()
        } else {
            list
        }
    }

    fn pane_to_tab_drop_hint(&self, compact: bool) -> Element<'_, Message> {
        let label = if compact {
            "↓ Drop pane as tab"
        } else {
            "Drop pane here → new tab"
        };
        container(text(label).size(11).color(self.c_text()))
            .padding(if compact { [5, 8] } else { [3, 9] })
            .style(self.split_drop_zone_style())
            .into()
    }

    /// Recursively flatten a file-tree node (and expanded descendants) into rows.
    fn collect_sidebar_nodes<'a>(
        &self,
        node: &'a sidebar::FileTreeNode,
        depth: usize,
        out: &mut Vec<Element<'a, Message>>,
    ) {
        let indent = 6.0 + depth as f32 * 12.0;
        let icon = if !node.is_dir {
            "·"
        } else {
            match &node.state {
                sidebar::DirectoryState::Loading => "◌",
                sidebar::DirectoryState::Error(_) => "!",
                sidebar::DirectoryState::Unloaded | sidebar::DirectoryState::Loaded => {
                    if node.expanded {
                        "▾"
                    } else {
                        "▸"
                    }
                }
            }
        };
        let label = row![
            Space::new().width(Length::Fixed(indent)),
            text(icon).size(12).width(Length::Fixed(14.0)),
            text(node.name.clone()).size(12),
        ]
        .align_y(iced::Alignment::Center);
        let msg = if node.is_dir {
            Message::SidebarToggleNode(node.path.clone())
        } else {
            Message::SidebarInsertPath(node.path.clone())
        };
        let row_button = button(label)
            .on_press(msg)
            .width(Length::Fill)
            .padding([1, 2])
            .style(self.ghost_btn_style());
        // Left-click keeps its old meaning (toggle/insert); right-click opens
        // the file-ops menu for this node.
        out.push(
            mouse_area(row_button)
                .on_right_press(Message::SidebarMenuOpen(node.path.clone(), node.is_dir))
                .into(),
        );
        if node.is_dir && node.expanded {
            if let sidebar::DirectoryState::Error(error) = &node.state {
                out.push(
                    container(
                        text(error.clone())
                            .size(10)
                            .wrapping(text::Wrapping::Word)
                            .style(text::danger),
                    )
                    .padding([2, (20.0 + depth as f32 * 12.0) as u16])
                    .into(),
                );
            }
            for child in &node.children {
                self.collect_sidebar_nodes(child, depth + 1, out);
            }
        }
    }

    /// The draggable divider strip for one gap of a split node (identified by
    /// `id`). Pressing it starts a resize drag (continued via the body's
    /// `on_move` while `dragging_divider` is set).
    fn divider(&self, axis: Axis, id: DividerId) -> Element<'_, Message> {
        let horizontal = matches!(axis, Axis::Horizontal);
        let d = if horizontal {
            container(Space::new())
                .width(Length::Fill)
                .height(Length::Fixed(DIVIDER))
        } else {
            container(Space::new())
                .width(Length::Fixed(DIVIDER))
                .height(Length::Fill)
        };
        let interaction = if horizontal {
            iced::mouse::Interaction::ResizingVertically
        } else {
            iced::mouse::Interaction::ResizingHorizontally
        };
        let active = self.hovered_divider.as_ref() == Some(&id)
            || self.dragging_divider.as_ref() == Some(&id);
        mouse_area(d.style(self.divider_style(active)))
            .on_press(Message::DividerDragStart(id.clone()))
            .on_enter(Message::DividerHover(Some(id.clone())))
            .on_exit(Message::DividerHover(None))
            .interaction(interaction)
            .into()
    }

    /// Recursively build the pane layout widget tree. `path` is the child-index
    /// route from the root to `node`, used to tag each divider with a
    /// [`DividerId`].
    fn render_tree(&self, node: &PaneTree, path: &[usize]) -> Element<'_, Message> {
        match node {
            PaneTree::Leaf(session) => {
                // The focus outline and the header strip are only meaningful
                // while split: a lone pane is already named by the tab bar.
                let split = self.is_split();
                let focused = split && *session == self.active;
                let body: Element<'_, Message> = if split {
                    column![
                        self.pane_header(*session),
                        container(self.pane_view(*session))
                            .width(Length::Fill)
                            .height(Length::Fill),
                    ]
                    .width(Length::Fill)
                    .height(Length::Fill)
                    .into()
                } else {
                    self.pane_view(*session)
                };
                let framed: Element<'_, Message> = container(body)
                    .style(self.pane_frame_style(focused))
                    .width(Length::Fill)
                    .height(Length::Fill)
                    .into();
                let direction = self.sessions.get(*session).and_then(|session| {
                    self.tab_split_drop
                        .filter(|drop| drop.target_session_id == session.id)
                        .map(|drop| drop.direction)
                });
                if let Some(direction) = direction {
                    stack![framed, self.split_drop_overlay(direction)].into()
                } else {
                    framed
                }
            }
            PaneTree::Split {
                axis,
                children,
                ratios,
            } => {
                let horizontal = matches!(axis, Axis::Horizontal);
                let n = children.len();
                let mut items: Vec<Element<'_, Message>> =
                    Vec::with_capacity(2 * n.saturating_sub(1) + 1);
                for (i, child) in children.iter().enumerate() {
                    if i > 0 {
                        items.push(self.divider(
                            *axis,
                            DividerId {
                                path: path.to_vec(),
                                gap: i - 1,
                            },
                        ));
                    }
                    let share = ratios.get(i).copied().unwrap_or(1.0 / n as f32);
                    let portion = (share * 1000.0).round().max(1.0) as u16;
                    let mut child_path = path.to_vec();
                    child_path.push(i);
                    let el = self.render_tree(child, &child_path);
                    let el: Element<'_, Message> = if horizontal {
                        container(el)
                            .width(Length::Fill)
                            .height(Length::FillPortion(portion))
                            .into()
                    } else {
                        container(el)
                            .width(Length::FillPortion(portion))
                            .height(Length::Fill)
                            .into()
                    };
                    items.push(el);
                }
                if horizontal {
                    iced::widget::Column::with_children(items)
                        .width(Length::Fill)
                        .height(Length::Fill)
                        .into()
                } else {
                    iced::widget::Row::with_children(items)
                        .width(Length::Fill)
                        .height(Length::Fill)
                        .into()
                }
            }
        }
    }

    /// Status strip above a split pane: its position in the layout, its title,
    /// its working directory, and the command it is currently running.
    ///
    /// The strip doubles as the rearrange handle. Pressing it focuses the pane
    /// (as clicking the terminal would); releasing over a different pane swaps
    /// the two sessions without disturbing the split geometry.
    fn pane_header(&self, session: usize) -> Element<'_, Message> {
        let Some(sess) = self.sessions.get(session) else {
            return Space::new().height(Length::Fixed(PANE_HEADER_H)).into();
        };
        let position = self
            .layout()
            .leaves()
            .iter()
            .position(|&leaf| leaf == session)
            .unwrap_or(0);
        let focused = session == self.active;
        let drag_source = self
            .pane_drag
            .as_ref()
            .is_some_and(|drag| drag.session_id == sess.id);
        let drop_target = self
            .pane_drag
            .as_ref()
            .is_some_and(|drag| drag.target == Some(sess.id));

        let mut line = row![
            text(format!("{}", position + 1))
                .size(11)
                .color(self.c_accent()),
            text(sess.pane_title()).size(11),
        ]
        .spacing(6)
        .align_y(iced::Alignment::Center);

        // The title is usually the directory's leaf; the full path only earns
        // its space when it says something the title does not.
        if let Some(cwd) = sess.cwd_display() {
            if cwd != sess.pane_title() {
                line = line.push(text(cwd).size(11).color(self.c_text_dim()));
            }
        }
        // Branch/dirty strip for the pane's repo, straight from the session
        // cache (refreshed on the periodic tick and after command completion).
        if self.config.show_repo_strip {
            if let Some(meta) = sess.git_meta_cache.as_ref() {
                let git = jterm_core::git_meta::format_strip(meta);
                line = line.push(text(git).size(11).color(blend(
                    self.c_text_dim(),
                    self.c_accent(),
                    0.35,
                )));
            }
        }
        if let Some(command) = sess.fg_proc_cache.as_deref() {
            line = line.push(text(format!("▶ {command}")).size(11).color(blend(
                self.c_text_dim(),
                self.c_accent(),
                0.6,
            )));
        }
        // Held-open task transcript: the child exited, so the pane is
        // read-only. The chip mirrors the hint toast's wording.
        if sess.transcript_read_only() {
            line = line.push(text("■ exited").size(11).color(self.c_text_dim()));
        }
        line = line.push(Space::new().width(Length::Fill));
        if drop_target {
            line = line.push(text("⇄").size(12).color(self.c_accent()));
        }

        let strip = container(line)
            .width(Length::Fill)
            .height(Length::Fixed(PANE_HEADER_H))
            .padding([0, 6])
            .clip(true)
            .style(self.pane_header_style(focused, drag_source, drop_target));

        mouse_area(strip)
            .on_press(Message::PaneDragStart(sess.id))
            .interaction(iced::mouse::Interaction::Grab)
            .into()
    }

    /// Pane header background: accent-tinted when focused, strongly tinted
    /// when it is the pending drop target, and faded while it is the pane
    /// being dragged away.
    fn pane_header_style(
        &self,
        focused: bool,
        drag_source: bool,
        drop_target: bool,
    ) -> impl Fn(&iced::Theme) -> container::Style {
        let base = Theme::rgb_to_color32(self.theme.tabbar.bg);
        let accent = self.c_accent();
        let border = self.c_border();
        let active_text = Theme::rgb_to_color32(self.theme.tabbar.active_text);
        let inactive_text = Theme::rgb_to_color32(self.theme.tabbar.inactive_text);
        move |_| {
            let mut bg = if drop_target {
                blend(base, accent, 0.45)
            } else if focused {
                blend(base, accent, 0.18)
            } else {
                base
            };
            if drag_source {
                bg = Color { a: 0.55, ..bg };
            }
            container::Style {
                text_color: Some(if focused || drop_target {
                    active_text
                } else {
                    inactive_text
                }),
                background: Some(bg.into()),
                border: iced::Border {
                    color: if drop_target { accent } else { border },
                    width: if drop_target { 1.0 } else { 0.0 },
                    radius: 0.0.into(),
                },
                ..Default::default()
            }
        }
    }

    /// Thin frame around a split pane: the focused pane gets an accent outline
    /// so keyboard focus is visible at a glance; the other pane stays plain.
    fn pane_frame_style(&self, focused: bool) -> impl Fn(&iced::Theme) -> container::Style {
        let accent = self.c_accent();
        move |_| container::Style {
            border: iced::Border {
                color: if focused { accent } else { Color::TRANSPARENT },
                width: if focused { 1.0 } else { 0.0 },
                radius: 0.0.into(),
            },
            ..Default::default()
        }
    }

    /// Directional overlay shown only after the pointer enters a pane edge's
    /// armed zone. The untouched center remains visible as a safe cancel area.
    fn split_drop_overlay(&self, direction: PaneDirection) -> Element<'_, Message> {
        let label = match direction {
            PaneDirection::Left => "← Split left",
            PaneDirection::Right => "Split right →",
            PaneDirection::Up => "↑ Split above",
            PaneDirection::Down => "Split below ↓",
        };
        let zone = container(text(label).size(13).color(self.c_text()))
            .center(Length::Fill)
            .style(self.split_drop_zone_style());
        let overlay: Element<'_, Message> = match direction {
            PaneDirection::Left => row![
                container(zone).width(Length::FillPortion(28)),
                Space::new().width(Length::FillPortion(72)),
            ]
            .height(Length::Fill)
            .into(),
            PaneDirection::Right => row![
                Space::new().width(Length::FillPortion(72)),
                container(zone).width(Length::FillPortion(28)),
            ]
            .height(Length::Fill)
            .into(),
            PaneDirection::Up => column![
                container(zone).height(Length::FillPortion(28)),
                Space::new().height(Length::FillPortion(72)),
            ]
            .width(Length::Fill)
            .into(),
            PaneDirection::Down => column![
                Space::new().height(Length::FillPortion(72)),
                container(zone).height(Length::FillPortion(28)),
            ]
            .width(Length::Fill)
            .into(),
        };
        container(overlay)
            .width(Length::Fill)
            .height(Length::Fill)
            .into()
    }

    fn split_drop_zone_style(&self) -> impl Fn(&iced::Theme) -> container::Style {
        let base = Theme::rgb_to_color32(self.theme.tabbar.bg);
        let accent = self.c_accent();
        move |_| {
            let mut background = blend(base, accent, 0.48);
            background.a = 0.88;
            container::Style {
                background: Some(background.into()),
                border: iced::Border {
                    color: accent,
                    width: 2.0,
                    radius: 6.0.into(),
                },
                ..Default::default()
            }
        }
    }

    fn tab_to_split_instruction(&self) -> Element<'_, Message> {
        let mut background = blend(
            Theme::rgb_to_color32(self.theme.tabbar.bg),
            self.c_accent(),
            0.28,
        );
        background.a = 0.92;
        let accent = self.c_accent();
        let foreground = self.c_text();
        let chip = container(text("Drop near a pane edge to split · center cancels").size(11))
            .padding([5, 10])
            .style(move |_| container::Style {
                text_color: Some(foreground),
                background: Some(background.into()),
                border: iced::Border {
                    color: accent,
                    width: 1.0,
                    radius: 12.0.into(),
                },
                ..Default::default()
            });
        container(chip)
            .center_x(Length::Fill)
            .align_top(Length::Fill)
            .padding(8)
            .into()
    }

    fn view(&self) -> Element<'_, Message> {
        if self.sessions.is_empty() {
            let message = self
                .session_diagnostic
                .as_deref()
                .unwrap_or("No terminal session is available");
            let panel_width = (self.win_size.width - 48.0).clamp(240.0, 520.0);
            let empty: Element<'_, Message> = container(
                column![
                    text("Terminal could not start").size(20),
                    text(message.to_string())
                        .size(12)
                        .wrapping(text::Wrapping::Word)
                        .style(text::danger),
                    button(text("Retry").size(13)).on_press(Message::NewSession),
                ]
                .spacing(12)
                .width(Length::Fixed(panel_width)),
            )
            .center(Length::Fill)
            .padding(24)
            .into();
            return if self.config_diagnostic.is_some() || !self.keybindings_diagnostics.is_empty() {
                stack![empty, self.diagnostics_overlay()].into()
            } else {
                empty
            };
        }
        let panes_body: Element<'_, Message> = if self.is_split() && self.pane_zoomed {
            // Zoomed: the focused pane fills the whole area; the hidden panes
            // keep running in the background exactly like inactive tabs.
            self.pane_view(self.active)
        } else {
            // Recursive tiled layout with a draggable divider between each pair
            // of siblings. Integer FillPortions approximate the float shares.
            self.render_tree(self.layout(), &[])
        };
        let panes_body: Element<'_, Message> = if self.tab_split_drag_eligible() {
            stack![panes_body, self.tab_to_split_instruction()].into()
        } else {
            panes_body
        };
        // While dragging the divider, wrap the panes in a mouse_area so pointer
        // moves drive the resize and release ends it. The handler is attached
        // only during a drag to avoid emitting a message on every idle move.
        let panes_body: Element<'_, Message> = if self.dragging_divider.is_some() {
            mouse_area(panes_body)
                .on_move(Message::DividerDragMove)
                .on_release(Message::DividerDragEnd)
                .on_exit(Message::DividerDragEnd)
                .into()
        } else if self.pane_drag.is_some() {
            // Same pattern as the divider drag: pointer moves track which pane
            // is under the cursor and release commits the swap. Leaving the
            // pane area preserves the source so the tab strip can promote it.
            mouse_area(panes_body)
                .on_move(Message::PaneDragMove)
                .on_release(Message::PaneDragEnd)
                .on_exit(Message::PaneDragLeavePaneArea)
                .interaction(iced::mouse::Interaction::Grabbing)
                .into()
        } else if self.dragging_tab.is_some() {
            // An ordinary tab can be moved into any edge of the visible page.
            // Movement arms one explicit edge; release in the center cancels.
            mouse_area(panes_body)
                .on_move(Message::TabDragMove)
                .on_release(Message::TabSplitDrop)
                .on_exit(Message::TabDragLeavePaneArea)
                .interaction(iced::mouse::Interaction::Grabbing)
                .into()
        } else {
            panes_body
        };
        let body = container(panes_body)
            .width(Length::Fill)
            .height(Length::Fill);
        let body: Element<'_, Message> = if self.config_panel_open {
            let overlay = if self.theme_editor.is_some() {
                self.theme_editor_view()
            } else {
                self.config_panel()
            };
            stack![body, overlay].into()
        } else if self.palette.is_open {
            stack![body, self.command_palette()].into()
        } else if self.search_replace.is_open {
            stack![body, self.search_replace_panel()].into()
        } else if self.search.is_open {
            stack![body, self.search_bar()].into()
        } else {
            body.into()
        };
        // Help and the debug panel float above any other overlay so they can be
        // summoned at any time (and the debug panel can sit alongside others).
        // Agent panel floats above the terminal but below help/debug.
        let body: Element<'_, Message> = if self.agent.is_open {
            stack![body, self.agent_panel()].into()
        } else {
            body
        };
        let body: Element<'_, Message> = if self.help_open {
            stack![body, self.help_panel()].into()
        } else if self.debug_open {
            stack![body, self.debug_panel()].into()
        } else {
            body
        };

        // Optional left dock (file tree and/or tab list) beside the terminal,
        // separated by a draggable resize divider. Enter/exit tracking on the
        // dock gates the window-space pointer subscription the file-ops menu
        // anchors to (the tab strip does the same for its menu).
        let main_area: Element<'_, Message> = if self.dock_open() {
            let dock = mouse_area(self.sidebar_view())
                .on_enter(Message::SidebarHover(true))
                .on_exit(Message::SidebarHover(false));
            let dock_row = row![dock, self.sidebar_divider(), body]
                .width(Length::Fill)
                .height(Length::Fill);
            // While dragging, pointer moves drive the resize and release ends it.
            if self.dragging_sidebar {
                mouse_area(dock_row)
                    .on_move(Message::SidebarDragMove)
                    .on_release(Message::SidebarDragEnd)
                    .on_exit(Message::SidebarDragEnd)
                    .into()
            } else {
                dock_row.into()
            }
        } else {
            body
        };
        // The top bar is always present: in Top mode it holds the tab strip; in
        // Side mode it holds the dock toggle so chrome never overlaps the grid.
        // The jsh notice, when there is one, sits directly under it.
        let mut chrome = column![self.tab_bar()];
        if let Some(notice) = self.jsh_notice() {
            chrome = chrome.push(notice);
        }
        chrome = chrome.push(main_area);
        if self.config.bottom_bar {
            chrome = chrome.push(self.status_bar());
        }
        let root: Element<'_, Message> = chrome.width(Length::Fill).height(Length::Fill).into();
        // Tab context menu, tab switcher, history picker, and toasts float
        // above everything so they remain accessible regardless of which
        // other panel is open.
        let root = if let Some(i) = self.tab_menu {
            stack![root, self.tab_context_menu(i)].into()
        } else {
            root
        };
        let root: Element<'_, Message> = if let Some(s) = &self.tab_switcher {
            stack![root, self.tab_switcher_view(s)].into()
        } else {
            root
        };
        let root: Element<'_, Message> = if let Some(s) = &self.history_picker {
            stack![root, self.history_picker_view(s)].into()
        } else {
            root
        };
        let root: Element<'_, Message> = if let Some(s) = &self.block_search {
            stack![root, self.block_search_view(s)].into()
        } else {
            root
        };
        let root: Element<'_, Message> = if let Some(menu) = self.block_menu {
            stack![root, self.block_menu_view(menu)].into()
        } else {
            root
        };
        let root: Element<'_, Message> = if let Some(selected) = self.remote_picker {
            stack![root, self.remote_picker_view(selected)].into()
        } else {
            root
        };
        let root: Element<'_, Message> = if let Some(confirm) = self.block_clear_confirm {
            stack![root, self.block_clear_confirm_view(confirm)].into()
        } else {
            root
        };
        let root: Element<'_, Message> = if let Some((id, process, _)) = &self.tab_close_confirm {
            stack![root, self.tab_close_confirm_view(*id, process)].into()
        } else {
            root
        };
        // The file tree's own overlays: menu below the modals, deletion last
        // (top-most) since it is the destructive one.
        let root: Element<'_, Message> = if let Some(menu) = &self.sidebar_menu {
            stack![root, self.sidebar_menu_view(menu)].into()
        } else {
            root
        };
        let root: Element<'_, Message> = if let Some(dialog) = &self.sidebar_dialog {
            stack![root, self.sidebar_dialog_view(dialog)].into()
        } else {
            root
        };
        let root: Element<'_, Message> = if let Some(path) = &self.sidebar_delete_confirm {
            stack![root, self.sidebar_delete_confirm_view(path)].into()
        } else {
            root
        };
        let root: Element<'_, Message> = if self.config_diagnostic.is_some()
            || self.session_diagnostic.is_some()
            || !self.keybindings_diagnostics.is_empty()
        {
            stack![root, self.diagnostics_overlay()].into()
        } else {
            root
        };
        let root: Element<'_, Message> = if self.toasts.is_empty() {
            root
        } else {
            stack![root, self.toast_overlay()].into()
        };
        // Resize grips sit above everything, including the overlays: the
        // window border has to stay grabbable whatever panel is open.
        stack![root, self.window_resize_edges()].into()
    }

    /// Search bar overlaid at the top-right of the terminal. The query is an
    /// editable `text_input`; Enter/Esc/arrows are still handled at the app level
    /// (the input deliberately has no `on_submit` so Shift+Enter can mean "prev").
    fn search_bar(&self) -> Element<'_, Message> {
        let status = if let Some(err) = &self.search.error_message {
            err.clone()
        } else if !self.search.matches.is_empty() {
            format!(
                "{}/{}",
                self.search.current_match_index + 1,
                self.search.matches.len()
            )
        } else if !self.search.query.is_empty() {
            "No matches".to_string()
        } else {
            String::new()
        };

        // Clickable mode toggles (also bound to Ctrl+R / Ctrl+I).
        let regex_btn = button(text(".*").size(12))
            .on_press(Message::SearchToggleRegex)
            .padding([2, 6])
            .style(if self.search.use_regex {
                button::primary
            } else {
                button::secondary
            });
        let case_btn = button(text("Aa").size(12))
            .on_press(Message::SearchToggleCase)
            .padding([2, 6])
            .style(if self.search.case_sensitive {
                button::primary
            } else {
                button::secondary
            });

        let input = text_input("search…", &self.search.query)
            .id(SEARCH_INPUT_ID.clone())
            .on_input(Message::SearchInput)
            .size(13)
            .width(Length::Fixed(220.0));
        let mut bar = row![text("Find:").size(13), input]
            .spacing(8)
            .align_y(iced::Alignment::Center);
        if !status.is_empty() {
            bar = bar.push(text(status).size(13));
        }
        bar = bar.push(regex_btn).push(case_btn);
        let inner = container(bar).padding([4, 10]).style(container::dark);
        container(inner)
            .align_right(Length::Fill)
            .align_top(Length::Fill)
            .padding(8)
            .into()
    }

    /// Centered Find & Replace modal (Ctrl+Alt+R). The scrollback is read-only
    /// program output, so the replacement runs on the current selection and the
    /// result goes to the clipboard or — without a trailing newline — to the
    /// active pane's prompt.
    fn search_replace_panel(&self) -> Element<'_, Message> {
        let label = |s: &str| container(text(s.to_string()).size(13)).width(Length::Fixed(64.0));
        let find_input = text_input("find…", &self.search_replace.search_input)
            .id(SEARCH_REPLACE_FIND_ID.clone())
            .on_input(Message::SearchReplaceFindInput)
            .size(13);
        let replace_input = text_input("replace with…", &self.search_replace.replace_input)
            .on_input(Message::SearchReplaceReplaceInput)
            .size(13);
        let inputs = column![
            row![label("Find:"), find_input]
                .spacing(8)
                .align_y(iced::Alignment::Center),
            row![label("Replace:"), replace_input]
                .spacing(8)
                .align_y(iced::Alignment::Center),
        ]
        .spacing(6);

        let toggles = row![
            checkbox(self.search_replace.config.use_regex)
                .label("Regex")
                .text_size(13)
                .on_toggle(|_| Message::SearchReplaceToggleRegex),
            checkbox(self.search_replace.config.case_sensitive)
                .label("Case")
                .text_size(13)
                .on_toggle(|_| Message::SearchReplaceToggleCase),
            checkbox(self.search_replace.config.whole_word)
                .label("Word")
                .text_size(13)
                .on_toggle(|_| Message::SearchReplaceToggleWord),
            checkbox(self.search_replace.options.replace_all)
                .label("All")
                .text_size(13)
                .on_toggle(|_| Message::SearchReplaceToggleAll),
        ]
        .spacing(12);

        let actions = row![
            button(text("Replace → Clipboard").size(13)).on_press(Message::SearchReplaceApply(
                search_replace_panel::SearchReplaceAction::ReplaceToClipboard
            )),
            button(text("Type into terminal").size(13)).on_press(Message::SearchReplaceApply(
                search_replace_panel::SearchReplaceAction::TypeIntoTerminal
            )),
        ]
        .spacing(8);

        let title = row![
            text("Find & Replace").size(14),
            Space::new().width(Length::Fill),
            button(text("✕").size(12))
                .padding([2, 6])
                .style(button::text)
                .on_press(Message::SearchReplaceClose),
        ]
        .align_y(iced::Alignment::Center);

        let mut body = column![title, inputs, toggles, actions].spacing(10);
        if !self.search_replace.status.is_empty() {
            body = body.push(
                text(self.search_replace.status.clone())
                    .size(12)
                    .style(text::secondary),
            );
        }
        body = body.push(
            text("Runs on the current selection · Esc close")
                .size(10)
                .style(text::secondary),
        );

        // Stay inside the window like the settings modal does.
        let panel_width = (self.win_size.width - 32.0).clamp(240.0, 380.0);
        let panel = container(body)
            .width(Length::Fixed(panel_width))
            .padding(12)
            .style(container::dark);
        let dismiss = mouse_area(
            container(Space::new())
                .width(Length::Fill)
                .height(Length::Fill),
        )
        .on_press(Message::SearchReplaceClose);
        let centered = container(panel)
            .center_x(Length::Fill)
            .center_y(Length::Fill);
        stack![Element::from(dismiss), Element::from(centered)].into()
    }

    /// Centered, fuzzy-filtered command palette overlay. Keys are handled at
    /// the app level (`handle_palette_key`); rows are also mouse-clickable.
    fn command_palette(&self) -> Element<'_, Message> {
        let query = text_input("Type to filter…", &self.palette.query)
            .id(PALETTE_INPUT_ID.clone())
            .on_input(Message::PaletteInput)
            .size(14);
        let query_line = row![text("›").size(16), query]
            .spacing(8)
            .align_y(iced::Alignment::Center);

        let mut list = column![].spacing(2);
        let filtered = self.palette.filtered();
        if filtered.is_empty() {
            list = list.push(text("No commands").size(13).style(text::secondary));
        } else {
            for (pos, (idx, item)) in filtered.iter().enumerate() {
                let labels = column![
                    text(item.name).size(14),
                    text(item.description)
                        .size(11)
                        .wrapping(text::Wrapping::None)
                        .style(text::secondary),
                ]
                .spacing(1)
                .width(Length::Fill);
                let mut info = row![labels, Space::new().width(Length::Fill)]
                    .spacing(10)
                    .align_y(iced::Alignment::Center);
                if !item.shortcut.is_empty() {
                    info = info.push(text(item.shortcut).size(11).style(text::secondary));
                }
                let row_btn = button(info)
                    .on_press(Message::PaletteExecute(*idx))
                    .width(Length::Fill)
                    .padding([4, 8])
                    .style(if pos == self.palette.selected {
                        button::primary
                    } else {
                        button::text
                    });
                list = list.push(row_btn);
            }
        }

        let footer = text("↑↓ navigate · PgUp/PgDn jump · Enter run · Esc close")
            .size(10)
            .style(text::secondary);
        let panel_width = (self.win_size.width - 32.0).clamp(300.0, 640.0);
        let panel_height = (self.win_size.height - 48.0).clamp(220.0, 520.0);
        let inner = container(
            column![
                query_line,
                scrollable(list)
                    .id(PALETTE_LIST_ID.clone())
                    .height(Length::Shrink),
                footer
            ]
            .spacing(8),
        )
        .width(Length::Fixed(panel_width))
        .max_height(panel_height)
        .padding(12)
        .style(container::dark);
        container(inner)
            .center_x(Length::Fill)
            .center_y(Length::Fill)
            .into()
    }

    /// Centered settings overlay (Ctrl+Shift+O). Controls live-apply on change;
    /// Save persists to disk, Reset restores defaults.
    fn config_panel(&self) -> Element<'_, Message> {
        let mut themes: Vec<String> = Theme::available_themes()
            .into_iter()
            .map(|s| s.to_string())
            .collect();
        themes.extend(Theme::custom_theme_names());
        let current_theme = Some(self.config.theme.clone());
        let is_custom = !Theme::is_builtin(&self.config.theme);

        // Keep the modal inside the current window and switch to a stacked form
        // before horizontal controls become cramped. The content itself scrolls
        // below, so every setting remains reachable in short windows.
        let panel_width = (self.win_size.width - 24.0).clamp(1.0, 520.0);
        let panel_height = (self.win_size.height - 24.0).clamp(1.0, 560.0);
        let compact = panel_width < 430.0;
        let panel_padding = if compact || panel_height < 360.0 {
            10.0
        } else {
            16.0
        };

        let theme_picker = pick_list(themes, current_theme, Message::SetTheme)
            .text_size(13)
            .width(Length::Fill);
        let mut theme_actions =
            row![button(text("Edit…").size(13)).on_press(Message::ThemeEditOpen),]
                .spacing(8)
                .align_y(iced::Alignment::Center);
        if is_custom {
            theme_actions = theme_actions.push(
                button(text("Delete").size(13))
                    .on_press(Message::ThemeDelete(self.config.theme.clone()))
                    .style(button::danger),
            );
        }
        let theme_row: Element<'_, Message> = if compact {
            column![text("Theme").size(13), theme_picker, theme_actions]
                .spacing(6)
                .into()
        } else {
            row![
                text("Theme").size(13).width(Length::Fixed(120.0)),
                theme_picker,
                theme_actions,
            ]
            .spacing(10)
            .align_y(iced::Alignment::Center)
            .into()
        };

        // Monospace families detected via fc-list (cached, scanned on first open).
        // Ensure the configured family is present so the pick_list shows it.
        let mut fonts: Vec<String> = Config::get_monospace_fonts().clone();
        if !self.config.font_family.trim().is_empty()
            && !fonts.iter().any(|f| f == &self.config.font_family)
        {
            fonts.insert(0, self.config.font_family.clone());
        }
        let font_picker = pick_list(
            fonts,
            Some(self.config.font_family.clone()),
            Message::SetFontFamily,
        )
        .text_size(13)
        .width(Length::Fill);
        let font_family_row: Element<'_, Message> = if compact {
            column![text("Font").size(13), font_picker]
                .spacing(6)
                .into()
        } else {
            row![
                text("Font").size(13).width(Length::Fixed(120.0)),
                font_picker,
            ]
            .spacing(10)
            .align_y(iced::Alignment::Center)
            .into()
        };

        fn responsive_slider_row<'a>(
            compact: bool,
            label: &'static str,
            value: String,
            control: Element<'a, Message>,
        ) -> Element<'a, Message> {
            if compact {
                column![
                    row![
                        text(label).size(13).width(Length::Fill),
                        text(value).size(13),
                    ]
                    .align_y(iced::Alignment::Center),
                    control,
                ]
                .spacing(6)
                .into()
            } else {
                slider_row(label, value, control)
            }
        }

        let font_size = responsive_slider_row(
            compact,
            "Font Size",
            format!("{:.0}", self.config.font_size),
            slider(8.0..=72.0, self.config.font_size, Message::SetFontSize)
                .step(1.0_f32)
                .into(),
        );
        let ui_scale_value = self.config.ui_scale.unwrap_or(1.0);
        let ui_scale = responsive_slider_row(
            compact,
            "UI Scale",
            format!("{:.0}%", ui_scale_value * 100.0),
            slider(0.5..=4.0, ui_scale_value, Message::SetUiScale)
                .step(0.05_f32)
                .into(),
        );
        let line_spacing = responsive_slider_row(
            compact,
            "Line Spacing",
            format!("{:.2}", self.config.line_spacing),
            slider(0.8..=3.0, self.config.line_spacing, Message::SetLineSpacing)
                .step(0.05_f32)
                .into(),
        );
        let padding = responsive_slider_row(
            compact,
            "Padding",
            format!("{:.0}", self.config.padding),
            slider(0.0..=20.0, self.config.padding, Message::SetPadding)
                .step(1.0_f32)
                .into(),
        );
        let opacity = responsive_slider_row(
            compact,
            "Opacity",
            format!("{:.0}%", self.config.opacity * 100.0),
            slider(0.05..=1.0, self.config.opacity, Message::SetOpacity)
                .step(0.025_f32)
                .into(),
        );
        let scrollback = responsive_slider_row(
            compact,
            "Scrollback",
            format!("{}", self.config.scrollback_lines),
            slider(
                100..=100_000u32,
                self.config.scrollback_lines as u32,
                Message::SetScrollback,
            )
            .step(100u32)
            .into(),
        );
        let scroll_speed = responsive_slider_row(
            compact,
            "Scroll Speed",
            format!("{}", self.config.scroll_speed),
            slider(1..=10u32, self.config.scroll_speed, Message::SetScrollSpeed)
                .step(1u32)
                .into(),
        );
        fn responsive_control_row<'a>(
            compact: bool,
            label: &'static str,
            control: Element<'a, Message>,
        ) -> Element<'a, Message> {
            if compact {
                column![text(label).size(13), control].spacing(6).into()
            } else {
                row![text(label).size(13).width(Length::Fixed(120.0)), control,]
                    .spacing(10)
                    .align_y(iced::Alignment::Center)
                    .into()
            }
        }

        let scrollbar_row = responsive_control_row(
            compact,
            "Scrollbar",
            checkbox(matches!(
                self.config.scrollbar_visibility,
                config::ScrollbarVisibility::Always
            ))
            .label("Always show")
            .text_size(13)
            .on_toggle(Message::SetScrollbarAlways)
            .into(),
        );

        let alt_screen_row = responsive_control_row(
            compact,
            "Alt Screen",
            checkbox(self.config.disable_alt_screen)
                .label("Disable")
                .text_size(13)
                .on_toggle(Message::SetDisableAltScreen)
                .into(),
        );

        let clipboard_row = responsive_control_row(
            compact,
            "Clipboard",
            checkbox(self.config.allow_clipboard_read)
                .label("Allow PTY reads (unsafe over SSH)")
                .text_size(13)
                .on_toggle(Message::SetAllowClipboardRead)
                .into(),
        );

        let tab_position_row = responsive_control_row(
            compact,
            "Tabs",
            checkbox(self.config.tab_position == config::TabPosition::Side)
                .label("In sidebar")
                .text_size(13)
                .on_toggle(|side| {
                    Message::SetTabPosition(if side {
                        config::TabPosition::Side
                    } else {
                        config::TabPosition::Top
                    })
                })
                .into(),
        );

        let notify_row = responsive_control_row(
            compact,
            "Notify",
            checkbox(self.config.notify_long_blocks)
                .label("Toast when long commands finish unwatched")
                .text_size(13)
                .on_toggle(Message::SetNotifyLongBlocks)
                .into(),
        );

        let repo_strip_row = responsive_control_row(
            compact,
            "Git",
            checkbox(self.config.show_repo_strip)
                .label("Branch/dirty in pane headers")
                .text_size(13)
                .on_toggle(Message::SetShowRepoStrip)
                .into(),
        );

        let bottom_bar_row = responsive_control_row(
            compact,
            "Bar",
            checkbox(self.config.bottom_bar)
                .label("Bottom status bar (cwd, git, last command)")
                .text_size(13)
                .on_toggle(Message::SetBottomBar)
                .into(),
        );

        let block_mode_row = responsive_control_row(
            compact,
            "Blocks",
            checkbox(self.config.block_mode)
                .label("Command cards (OSC 133)")
                .text_size(13)
                .on_toggle(Message::SetBlockMode)
                .into(),
        );

        let block_compact_row = responsive_control_row(
            compact,
            "Block spacing",
            checkbox(self.config.block_compact)
                .label("Compact Block Spacing")
                .text_size(13)
                .on_toggle(Message::SetBlockCompact)
                .into(),
        );

        // ── AI & Agent ────────────────────────────────────────────────────
        let ai_header = text("AI & Agent").size(15);
        let ai_enable_row = responsive_control_row(
            compact,
            "AI",
            checkbox(self.config.ai_enabled)
                .label("Enable AI features")
                .text_size(13)
                .on_toggle(Message::SetAiEnabled)
                .into(),
        );
        let ai_providers = vec![
            "anthropic".to_string(),
            "openai-compatible".to_string(),
            "ollama".to_string(),
        ];
        let ai_provider_row = responsive_control_row(
            compact,
            "Provider",
            pick_list(
                ai_providers,
                Some(self.config.ai_provider.clone()),
                Message::SetAiProvider,
            )
            .text_size(13)
            .width(Length::Fill)
            .into(),
        );
        let ai_model_row = responsive_control_row(
            compact,
            "Model",
            text_input("claude-sonnet-4-6", &self.config.ai_model)
                .on_input(Message::SetAiModel)
                .size(13)
                .into(),
        );
        let ai_base_url_row = responsive_control_row(
            compact,
            "Base URL",
            text_input("https://api.anthropic.com", &self.config.ai_base_url)
                .on_input(Message::SetAiBaseUrl)
                .size(13)
                .into(),
        );
        let ai_tokens_row = responsive_slider_row(
            compact,
            "Max tokens",
            format!("{}", self.config.ai_max_tokens),
            slider(
                64..=32_768u32,
                self.config.ai_max_tokens,
                Message::SetAiMaxTokens,
            )
            .into(),
        );
        let ai_temperature_row = responsive_control_row(
            compact,
            "Temperature",
            text_input("provider default (0.0-2.0)", &self.ai_temperature_draft)
                .on_input(Message::SetAiTemperature)
                .size(13)
                .into(),
        );
        let ai_key_file_row = responsive_control_row(
            compact,
            "Key file",
            text_input(
                "~/.config/frost/ai.key (chmod 600)",
                self.config.ai_api_key_file.as_deref().unwrap_or(""),
            )
            .on_input(Message::SetAiKeyFile)
            .size(13)
            .into(),
        );
        let ai_key_store_row = responsive_control_row(
            compact,
            "API Key",
            text_input(
                "paste key, Enter stores it as a 600 file",
                &self.ai_key_draft,
            )
            .secure(true)
            .on_input(Message::SetAiKeyDraft)
            .on_submit(Message::StoreAiKey)
            .size(13)
            .into(),
        );
        let ai_redact_row = responsive_control_row(
            compact,
            "Privacy",
            checkbox(self.config.ai_redact_secrets)
                .label("Redact secrets in AI-bound text")
                .text_size(13)
                .on_toggle(Message::SetAiRedactSecrets)
                .into(),
        );
        let ai_stream_row = responsive_control_row(
            compact,
            "Streaming",
            checkbox(self.config.ai_stream)
                .label("Stream agent replies as they arrive")
                .text_size(13)
                .on_toggle(Message::SetAiStream)
                .into(),
        );
        let ai_share_row = responsive_control_row(
            compact,
            "Cloud context",
            checkbox(self.config.ai_share_command_context)
                .label("Share command context with non-local AI")
                .text_size(13)
                .on_toggle(Message::SetAiShareCommandContext)
                .into(),
        );
        let task_sidebar_row = responsive_control_row(
            compact,
            "Tasks",
            checkbox(self.config.experimental_task_sidebar)
                .label("Experimental Tasks dashboard (Codex worktrees)")
                .text_size(13)
                .on_toggle(Message::SetExperimentalTaskSidebar)
                .into(),
        );
        let agent_turns_row = responsive_slider_row(
            compact,
            "Agent turns",
            format!("{}", self.config.agent_max_turns),
            slider(
                1..=100u32,
                self.config.agent_max_turns,
                Message::SetAgentMaxTurns,
            )
            .into(),
        );

        // ── Remote hosts ──────────────────────────────────────────────────
        // Edited in place; entries ride the same auto-save as every other
        // setting. Validation is live but advisory — editing and saving are
        // never blocked, and the Ctrl+Shift+S picker shows the same reason
        // for an entry it refuses to open.
        let mut remote_hosts_section = column![text("Remote hosts").size(15)].spacing(8);
        if self.config.remote_hosts.is_empty() {
            remote_hosts_section = remote_hosts_section.push(
                text("None configured. Ctrl+Shift+S opens the picker once a host is added.")
                    .size(12)
                    .style(text::secondary),
            );
        }
        for (i, host) in self.config.remote_hosts.iter().enumerate() {
            let transport = if host.docker { "docker" } else { "ssh" };
            let deploy = if host.deploy.is_empty() {
                "off"
            } else {
                host.deploy.as_str()
            };
            let header = row![
                text(host.display_name().to_string()).size(13),
                Space::new().width(Length::Fill),
                text(format!("{transport} · deploy {deploy}"))
                    .size(12)
                    .style(text::secondary),
                button(text("Delete").size(12))
                    .on_press(Message::RemoteHostRemove(i))
                    .style(button::danger),
            ]
            .spacing(8)
            .align_y(iced::Alignment::Center);

            let name_input = text_input("display name", &host.name)
                .on_input(move |s| Message::RemoteHostName(i, s))
                .size(13);
            let host_placeholder = if host.docker {
                "container name"
            } else {
                "ssh host"
            };
            let host_input = text_input(host_placeholder, &host.host)
                .on_input(move |s| Message::RemoteHostHost(i, s))
                .size(13);
            let user_input = text_input("user (optional)", host.user.as_deref().unwrap_or(""))
                .on_input(move |s| Message::RemoteHostUser(i, s))
                .size(13);
            let docker_box = checkbox(host.docker)
                .label("docker")
                .text_size(13)
                .on_toggle(move |v| Message::RemoteHostDocker(i, v));
            let deploy_pick = pick_list(
                vec![
                    "off".to_string(),
                    "persist".to_string(),
                    "incognito".to_string(),
                ],
                Some(deploy.to_string()),
                move |v| Message::RemoteHostDeploy(i, v),
            )
            .text_size(13)
            .width(Length::Fixed(110.0));
            let toggles = row![docker_box, deploy_pick]
                .spacing(10)
                .align_y(iced::Alignment::Center);
            let fields: Element<'_, Message> = if compact {
                column![name_input, host_input, user_input, toggles]
                    .spacing(6)
                    .into()
            } else {
                column![
                    row![name_input, host_input].spacing(8),
                    row![user_input, toggles].spacing(10),
                ]
                .spacing(6)
                .into()
            };
            let mut entry = column![header, fields].spacing(6);
            if let Err(problem) = host.validate() {
                entry = entry.push(text(problem).size(11).style(text::danger));
            }
            remote_hosts_section = remote_hosts_section.push(
                container(entry)
                    .width(Length::Fill)
                    .padding([6, 8])
                    .style(container::bordered_box),
            );
        }
        remote_hosts_section = remote_hosts_section
            .push(button(text("Add host").size(13)).on_press(Message::RemoteHostAdd));

        let buttons = row![
            button(text("Save").size(13)).on_press(Message::ConfigSave),
            button(text("Reset").size(13))
                .on_press(Message::ConfigReset)
                .style(button::danger),
            button(text("Close").size(13))
                .on_press(Message::ToggleConfigPanel)
                .style(button::secondary),
        ]
        .spacing(8);

        let footer = text("Changes auto-save · Ctrl+Shift+O toggles · Esc closes")
            .size(10)
            .width(Length::Fill)
            .wrapping(text::Wrapping::Word)
            .style(text::secondary);

        let content = column![
            text("Settings").size(18),
            theme_row,
            font_family_row,
            font_size,
            ui_scale,
            line_spacing,
            padding,
            opacity,
            scrollback,
            scroll_speed,
            scrollbar_row,
            alt_screen_row,
            clipboard_row,
            tab_position_row,
            notify_row,
            repo_strip_row,
            bottom_bar_row,
            block_mode_row,
            block_compact_row,
            ai_header,
            ai_enable_row,
            ai_provider_row,
            ai_model_row,
            ai_base_url_row,
            ai_tokens_row,
            ai_temperature_row,
            ai_key_file_row,
            ai_key_store_row,
            ai_redact_row,
            ai_stream_row,
            ai_share_row,
            task_sidebar_row,
            agent_turns_row,
            remote_hosts_section,
            buttons,
            footer,
        ]
        .spacing(12)
        .width(Length::Fill);

        let inner = container(scrollable(content).width(Length::Fill).height(Length::Fill))
            .width(Length::Fixed(panel_width))
            .height(Length::Fixed(panel_height))
            .padding(panel_padding)
            .style(container::dark);
        container(inner)
            .center_x(Length::Fill)
            .center_y(Length::Fill)
            .into()
    }

    /// Custom-theme editor overlay: name field plus a hex input per terminal
    /// palette color, with a live swatch. UI-chrome colors are inherited from the
    /// theme the editor was opened on.
    fn theme_editor_view(&self) -> Element<'_, Message> {
        let Some(ed) = &self.theme_editor else {
            return Space::new().into();
        };
        let labels = Theme::editable_color_labels();

        let name_row = row![
            text("Name").size(13).width(Length::Fixed(150.0)),
            text_input("theme name", &ed.name)
                .on_input(Message::ThemeEditName)
                .size(13),
        ]
        .spacing(10)
        .align_y(iced::Alignment::Center);

        let mut list = column![].spacing(6);
        for (i, label) in labels.iter().enumerate() {
            let hex = ed.hexes.get(i).cloned().unwrap_or_default();
            // Live swatch when the hex parses, else a neutral placeholder.
            let swatch_color = Theme::hex_to_rgb(&hex)
                .map(Theme::rgb_to_color32)
                .unwrap_or(iced::Color::from_rgb(0.3, 0.3, 0.3));
            let swatch = container(Space::new())
                .width(Length::Fixed(22.0))
                .height(Length::Fixed(22.0))
                .style(move |_| container::Style {
                    background: Some(swatch_color.into()),
                    border: iced::Border {
                        color: iced::Color::from_rgb(0.5, 0.5, 0.5),
                        width: 1.0,
                        radius: 3.0.into(),
                    },
                    ..Default::default()
                });
            let r = row![
                text(*label).size(12).width(Length::Fixed(150.0)),
                swatch,
                text_input("#RRGGBB", &hex)
                    .on_input(move |s| Message::ThemeEditColor(i, s))
                    .size(12)
                    .width(Length::Fixed(110.0)),
            ]
            .spacing(10)
            .align_y(iced::Alignment::Center);
            list = list.push(r);
        }

        let buttons = row![
            button(text("Save").size(13)).on_press(Message::ThemeEditSave),
            button(text("Cancel").size(13))
                .on_press(Message::ThemeEditClose)
                .style(button::secondary),
        ]
        .spacing(8);

        let mut content = column![
            text("Theme Editor").size(18),
            name_row,
            scrollable(list).height(Length::Fixed(300.0)),
        ]
        .spacing(12);
        if let Some(err) = &ed.error {
            content = content.push(text(err.clone()).size(12).style(text::danger));
        }
        content = content.push(buttons);

        let inner = container(content)
            .width(Length::Fixed(420.0))
            .max_height(560.0)
            .padding(16)
            .style(container::dark);
        container(inner)
            .center_x(Length::Fill)
            .center_y(Length::Fill)
            .into()
    }

    /// Centered keybindings cheat-sheet (Ctrl+Shift+/). Pane direction chords
    /// combine Ctrl with Alt so JWM's bare-Alt shortcuts remain untouched.
    fn help_panel(&self) -> Element<'_, Message> {
        let section = |title: &str| -> Element<'_, Message> {
            text(title.to_string()).size(13).style(text::primary).into()
        };
        let kb = |key: &str, desc: &str| -> Element<'_, Message> {
            row![
                container(text(key.to_string()).size(12).font(iced::Font::MONOSPACE))
                    .width(Length::Fixed(190.0)),
                text(desc.to_string()).size(12).style(text::secondary),
            ]
            .spacing(8)
            .into()
        };

        let body = column![
            text("Keyboard Shortcuts").size(18),
            section("Tabs / Sessions"),
            kb("Ctrl+Shift+T", "New tab"),
            kb("Ctrl+Shift+W", "Close current tab"),
            kb("Ctrl+Tab / Ctrl+PgDn", "Next tab"),
            kb("Ctrl+Shift+Tab / Ctrl+PgUp", "Previous tab"),
            kb("Ctrl+1 .. Ctrl+8", "Jump to tab 1-8"),
            kb("Ctrl+9", "Jump to last tab"),
            kb("Ctrl+Shift+L", "Fuzzy tab switcher"),
            section("Splits / Panes"),
            kb("Ctrl+Shift+E", "Add pane right (re-orients a row split)"),
            kb("Ctrl+Shift+D", "Add pane below (re-orients a column split)"),
            kb("Ctrl+Alt+Arrow", "Focus adjacent pane"),
            kb("Ctrl+Alt+Shift+Arrow", "Resize focused pane"),
            kb("Ctrl+Shift+Z", "Zoom focused pane (toggle)"),
            kb("Ctrl+Shift+X", "Swap pane with the next one"),
            kb("Double-click divider", "Equalize all panes"),
            kb("Ctrl+Shift+W", "Close focused pane / tab"),
            section("Edit / Clipboard"),
            kb("Ctrl+Shift+C", "Copy selection"),
            kb("Ctrl+Shift+V", "Paste"),
            kb("Ctrl+Shift+G", "Copy last command output (OSC 133)"),
            kb(
                "Ctrl+Shift+H",
                "Command history picker (types into the prompt)"
            ),
            kb("Drag", "Select text"),
            kb("Ctrl+Click", "Open link under cursor"),
            section("Command Blocks"),
            kb("Header click", "Select one finalized block"),
            kb("Shift+Click", "Select a range from any card row"),
            kb("Ctrl+Shift+Click", "Toggle a block from any card row"),
            kb(
                "Right-click card",
                "Open block actions without losing selection"
            ),
            kb(
                "Ctrl+Alt+F",
                "Search/filter blocks and reveal the matching line"
            ),
            kb("Ctrl+Shift+B", "Toggle bookmark on selected block"),
            kb("Ctrl+, / Ctrl+.", "Previous / next bookmark (wraps)"),
            kb(
                "Ctrl+Alt+X / E",
                "Fix / explain selected (or latest) failed block with Agent"
            ),
            kb(
                "Ctrl+Alt+T",
                "Retry selected (or latest) failed block's command"
            ),
            kb(
                "Ctrl+Shift+A / I / K",
                "Select all / reinput / clear blocks"
            ),
            kb("Ctrl+Shift+↑ / ↓", "Reveal selected block top / bottom"),
            kb("Command palette", "Block copy, search, export, navigation"),
            kb("Enter / Esc", "Reinput safe selection / clear selection"),
            section("Scroll / Search"),
            kb("Shift+Home", "Scroll to top"),
            kb("Shift+End", "Scroll to bottom (live)"),
            kb("Ctrl+Shift+Up / Down", "Previous / next prompt (OSC 133)"),
            kb("Ctrl+Shift+F", "Find"),
            kb(
                "Ctrl+Alt+R",
                "Find & replace in selection (clipboard / prompt)"
            ),
            section("Panels"),
            kb("Ctrl+\\", "Toggle tabs / files sidebar"),
            kb("Ctrl+Shift+P", "Command palette"),
            kb("Ctrl+Shift+O", "Settings"),
            kb("F12", "Debug / diagnostics"),
            kb("Ctrl+Shift+/", "This help"),
            kb("Esc", "Close any panel"),
            section("Appearance"),
            kb("Ctrl+= / Ctrl+-", "Increase / decrease font size"),
            kb("Ctrl+0", "Reset font size"),
            kb(
                "Ctrl+Alt+= / Ctrl+Alt+-",
                "Increase / decrease window opacity"
            ),
        ]
        .spacing(6);

        let panel_width = (self.win_size.width - 32.0).clamp(300.0, 560.0);
        let panel_height = (self.win_size.height - 32.0).clamp(180.0, 620.0);
        let inner = container(scrollable(body).height(Length::Shrink))
            .width(Length::Fixed(panel_width))
            .max_height(panel_height)
            .padding(16)
            .style(container::dark);
        container(inner)
            .center_x(Length::Fill)
            .center_y(Length::Fill)
            .into()
    }

    /// Top-right diagnostics overlay (F12): live grid / session /
    /// scrollback / Kitty-image / process-memory stats for the active session.
    fn debug_panel(&self) -> Element<'_, Message> {
        let stat = |label: &str, value: String| -> Element<'_, Message> {
            row![
                container(text(label.to_string()).size(11).style(text::primary))
                    .width(Length::Fixed(110.0)),
                text(value).size(11).font(iced::Font::MONOSPACE),
            ]
            .spacing(8)
            .into()
        };

        let mut lines = column![text("Diagnostics").size(13)].spacing(3);
        lines = lines
            .push(stat("Grid", format!("{}x{}", self.cols, self.rows)))
            .push(stat("Sessions", format!("{}", self.sessions.len())))
            .push(stat("Active", format!("#{}", self.active + 1)))
            .push(stat(
                "Split",
                if self.is_split() {
                    format!(
                        "{}/{} panes",
                        self.focused_pane_pos() + 1,
                        self.layout().leaf_count()
                    )
                } else {
                    "Single".to_string()
                },
            ));
        if let Some(sess) = self.sessions.get(self.active) {
            lines = lines
                .push(stat(
                    "Scrollback",
                    format!(
                        "{} / {}",
                        sess.terminal.scrollback_len(),
                        self.config.scrollback_lines
                    ),
                ))
                .push(stat(
                    "Scroll Off",
                    format!("{}", sess.terminal.scroll_offset),
                ))
                .push(stat(
                    "Kitty Imgs",
                    format!("{}", sess.terminal.kitty_graphics.image_count()),
                ))
                .push(stat(
                    "Kitty Mem",
                    format!("{} MB", sess.terminal.kitty_graphics.image_memory_mb()),
                ));
        }
        lines = lines.push(stat(
            "Memory",
            match read_rss_mb() {
                Some(mb) => format!("{:.1} MB", mb),
                None => "N/A".to_string(),
            },
        ));
        lines = lines.push(stat("Links", format!("{}", self.links.len())));
        // Ingest cost of the last PTY-output batch. bytes/µs is numerically equal
        // to MB/s, so the throughput needs no extra scaling.
        let ingest = if self.last_ingest_us > 0 {
            format!(
                "{} B / {} µs ({:.0} MB/s)",
                self.last_ingest_bytes,
                self.last_ingest_us,
                self.last_ingest_bytes as f64 / self.last_ingest_us as f64,
            )
        } else {
            format!("{} B / <1 µs", self.last_ingest_bytes)
        };
        lines = lines.push(stat("Ingest", ingest));

        let inner = container(lines)
            .width(Length::Fixed(240.0))
            .padding(10)
            .style(container::dark);
        container(inner)
            .align_right(Length::Fill)
            .align_top(Length::Fill)
            .padding(8)
            .into()
    }

    /// Launch the next agent model request (if the protocol is waiting on
    /// the model) as a blocking task off the UI thread.
    fn agent_drive_task(&mut self) -> Option<Task<Message>> {
        if !self.agent.is_open {
            return None;
        }
        let cwd = self
            .agent
            .bound_session_id
            .and_then(|sid| self.sessions.iter().find(|s| s.id == sid))
            .and_then(|s| s.cwd_cache.clone());
        let request = self
            .agent
            .next_model_request(&self.config, cwd.as_deref())?;
        let agent::ModelRequest {
            identity,
            client,
            system,
            user,
            token,
        } = request;
        let turns = [jterm_core::ai::Turn {
            role: jterm_core::ai::Role::User,
            text: user,
        }];
        if self.config.ai_stream {
            // Streaming: a dedicated worker thread runs the request and
            // forwards each assistant fragment (and finally the complete
            // reply) as Messages through an unbounded channel the runtime
            // drains as a Task stream. The complete returned text is the
            // single source of truth — `AgentUi::model_reply` records it
            // exactly as the blocking path would; fragments only feed the
            // live preview. The same cancellation token kills curl
            // mid-stream when the panel closes.
            let (tx, rx) = iced::futures::channel::mpsc::unbounded::<Message>();
            std::thread::spawn(move || {
                let mut on_delta = |fragment: &str| {
                    let _ =
                        tx.unbounded_send(Message::AgentModelDelta(identity, fragment.to_string()));
                };
                let result = client
                    .send_turns_streaming_cancellable(Some(&system), &turns, &token, &mut on_delta)
                    .map_err(|error| error.to_string());
                let _ = tx.unbounded_send(Message::AgentModelReply(identity, result));
            });
            return Some(Task::stream(rx));
        }
        Some(Task::perform(
            async move {
                let result = tokio::task::spawn_blocking(move || {
                    client
                        .send_turns_blocking_cancellable(Some(&system), &turns, &token)
                        .map_err(|error| error.to_string())
                })
                .await;
                match result {
                    Ok(result) => result,
                    Err(error) => Err(format!("AI worker task failed: {error}")),
                }
            },
            move |result| Message::AgentModelReply(identity, result),
        ))
    }

    /// Approve a proposal, put it on the bound session's prompt in place of the
    /// pending line, submit it, then continue driving the protocol.
    fn agent_run_approved(
        &mut self,
        id: jterm_core::agent::ProposalId,
        edited: Option<String>,
    ) -> Option<Task<Message>> {
        let bound = self.agent.bound_session_id?;
        let Some(session_index) = self.sessions.iter().position(|session| session.id == bound)
        else {
            self.agent.status = "Agent session's terminal no longer exists".to_string();
            return None;
        };

        // Gate *before* approving in the pure Agent state machine. A rejected
        // gate therefore leaves the proposal pending and reviewable.
        let prompt_status = self.sessions[session_index].agent_prompt_status();
        if !prompt_status.is_ready() {
            self.agent.status = prompt_status.blocked_message().to_string();
            return None;
        }
        if !self.sessions[session_index].can_queue_user_bytes(MAX_AGENT_APPROVAL_PAYLOAD_BYTES) {
            self.agent.status = "Agent command not run: PTY input queue is full".to_string();
            return None;
        }

        let approved = self.agent.approve(id, edited)?;
        let sess = &mut self.sessions[session_index];
        let paste = match agent_command_payload(
            &approved.command,
            sess.terminal.is_bracketed_paste_enabled(),
        ) {
            Ok(paste) => paste,
            Err(error) => {
                self.agent.execution_start_failed(
                    approved.generation,
                    format!("Agent command rejected at the PTY boundary: {error}"),
                );
                return None;
            }
        };
        if let Err(status) = sess
            .terminal
            .arm_agent_execution(approved.generation, &approved.command)
        {
            self.agent.execution_start_failed(
                approved.generation,
                format!("{}; Agent session stopped", status.blocked_message()),
            );
            return None;
        }
        sess.terminal.scroll_to_bottom();
        sess.projection_view_state.scroll_to_bottom();
        if !sess.write_agent_pty(&paste.bytes) {
            sess.terminal.disarm_agent_execution(approved.generation);
            self.agent.execution_start_failed(
                approved.generation,
                "Agent command could not be written; Agent session stopped",
            );
            return None;
        }
        sess.refresh();
        self.agent_drive_task()
    }

    // ===== Experimental Tasks dashboard (agent_task) =====

    /// Show or hide the Tasks dock panel. With the feature flag off the
    /// toggle only explains how to enable it.
    fn toggle_task_panel(&mut self) {
        if !self.config.experimental_task_sidebar {
            self.push_toast(
                "Enable the experimental Tasks dashboard in Settings first",
                ToastKind::Info,
            );
            return;
        }
        let showing = self.sidebar_open && self.sidebar_panel == SidebarPanel::Tasks;
        if showing {
            self.sidebar_open = false;
        } else {
            self.sidebar_open = true;
            self.sidebar_panel = SidebarPanel::Tasks;
        }
    }

    /// Create an isolated-worktree task from one failed command block. The
    /// eligibility gates mirror the Fix/Explain Agent path; the actual Git
    /// work is prepared on a background worker and registered on TaskTick.
    fn task_create_from_block(&mut self, session_id: usize, zone_id: u64) {
        if !self.config.experimental_task_sidebar {
            return;
        }
        if self.task_panel.pending_creation.is_some() {
            self.push_toast(
                "Another task worktree is still being created",
                ToastKind::Info,
            );
            return;
        }
        let prepared = self
            .sessions
            .iter()
            .find(|sess| sess.id == session_id)
            .map(|sess| {
                let Some(zone) = sess.terminal.zone_by_id(zone_id) else {
                    return Err("Block menu target is no longer available".to_string());
                };
                if !matches!(
                    block_mode::classify(zone.command.as_deref(), zone.exit_code),
                    block_mode::BlockOutcome::Failed(_)
                ) {
                    return Err("Tasks can be created for failed command blocks".to_string());
                }
                if let Some(reason) = block_mode::failed_block_agent_disabled_reason(
                    zone.command.as_deref(),
                    zone.command_truncated,
                    zone.cwd.as_deref(),
                ) {
                    return Err(format!("Cannot create a task: {reason}"));
                }
                let recorded = zone.cwd.clone().expect("eligibility guarantees a cwd");
                let reported = sess.terminal.current_working_dir().map(str::to_string);
                let process = jterm_core::process::process_cwd(sess.pty.get_child_pid());
                if !block_mode::verified_local_command_cwd(
                    &recorded,
                    reported.as_deref(),
                    process.as_deref(),
                ) {
                    return Err(
                        "Cannot create a task: the recorded cwd is not independently verified; return a local shell to the command's directory first"
                            .to_string(),
                    );
                }
                let (output_text, output_truncated, output_available) =
                    match sess.terminal.zone_output_export_capped(zone_id) {
                        Some(terminal::ZoneOutputExport::Available { text, truncated }) => {
                            (text, truncated, true)
                        }
                        Some(terminal::ZoneOutputExport::Empty) => (String::new(), false, true),
                        Some(terminal::ZoneOutputExport::Unavailable) => {
                            (String::new(), false, false)
                        }
                        None => {
                            return Err("Block menu target is no longer available".to_string())
                        }
                    };
                let command = zone.command.clone().expect("eligibility guarantees a command");
                Ok(agent_task::SemanticCommandContext {
                    source_session_id: agent_task_ui::terminal_session_id(session_id),
                    source_execution_id: format!("zone-{zone_id}"),
                    source_sequence: zone_id,
                    source_shell: pty::resolved_shell_identity(
                        self.config.shell.as_deref(),
                        Some(&recorded),
                    ),
                    command: Some(command),
                    command_exact: zone.command_exact,
                    command_truncated: zone.command_truncated,
                    cwd: Some(recorded),
                    cwd_after: None,
                    exit_code: zone.exit_code,
                    duration_ms: zone.duration_ms,
                    output_text,
                    output_available,
                    output_truncated,
                    output_total_bytes: 0,
                    started_at: None,
                    finished_at: None,
                })
            });
        let context = match prepared {
            Some(Ok(context)) => context,
            Some(Err(message)) => {
                self.push_toast(message, ToastKind::Warning);
                return;
            }
            None => {
                self.push_toast("Block menu target is no longer available", ToastKind::Info);
                return;
            }
        };
        match agent_task_ui::begin_worktree_creation(context, agent_task::AgentProvider::Codex) {
            Ok(pending) => {
                self.task_panel.pending_creation = Some(pending);
                self.sidebar_open = true;
                self.sidebar_panel = SidebarPanel::Tasks;
                self.push_toast(
                    "Creating an isolated Git worktree for Codex…",
                    ToastKind::Info,
                );
            }
            Err(error) => self.push_toast(error, ToastKind::Warning),
        }
    }

    /// Enter the bounded, cancellable background preparation phase for a
    /// native Codex session. Consent is re-evaluated here and again when the
    /// prepared result lands; a revoked policy never spawns a provider.
    fn task_start_codex(&mut self, task_id: agent_task::TaskId) {
        let policy = agent_task_ui::prompt_policy(&self.config);
        if !policy.share_command_context {
            self.push_toast(
                "Start Codex requires AI and command-context sharing in Settings",
                ToastKind::Warning,
            );
            return;
        }
        match self
            .agent_runtime
            .start_codex(&mut self.task_manager, task_id, policy)
        {
            Ok(()) => self.push_toast("Preparing an isolated Codex session…", ToastKind::Info),
            Err(error) => self.push_toast(error.to_string(), ToastKind::Warning),
        }
    }

    /// Drain pending worktree creation, native provider events, and the diff
    /// worker. Driven by the iced tick subscription; never blocks.
    fn tasks_tick(&mut self) {
        let pending = self
            .task_panel
            .pending_creation
            .as_ref()
            .map(|pending| pending.receiver.try_recv());
        match pending {
            None | Some(Err(std::sync::mpsc::TryRecvError::Empty)) => {}
            Some(Err(std::sync::mpsc::TryRecvError::Disconnected)) => {
                self.task_panel.pending_creation = None;
                self.push_toast(
                    "Task worktree worker stopped unexpectedly",
                    ToastKind::Warning,
                );
            }
            Some(Ok(Err(error))) => {
                self.task_panel.pending_creation = None;
                self.push_toast(
                    format!("Could not create task worktree: {error}"),
                    ToastKind::Warning,
                );
            }
            Some(Ok(Ok(prepared))) => {
                self.task_panel.pending_creation = None;
                let provider_name = prepared.provider.display_name();
                let worktree = prepared.worktree;
                let new_task = agent_task::NewTask {
                    title: prepared.title,
                    provider: prepared.provider,
                    repo_root: worktree.repository,
                    worktree_path: worktree.path,
                    branch: worktree.branch,
                    base_commit: worktree.head,
                    source_context: Some(prepared.context),
                };
                match self.task_manager.create(new_task) {
                    Ok(task_id) => {
                        self.task_panel.selected = Some(task_id);
                        self.push_toast(
                            format!("Created an isolated {provider_name} task; choose Start Codex"),
                            ToastKind::Success,
                        );
                    }
                    Err(error) => self.push_toast(
                        format!("Worktree was preserved, but task registration failed: {error}"),
                        ToastKind::Warning,
                    ),
                }
            }
        }

        if self.agent_runtime.has_any_activity() {
            let report = self.agent_runtime.poll(
                &mut self.task_manager,
                agent_task_ui::prompt_policy(&self.config),
            );
            for issue in report.issues {
                self.push_toast(
                    crate::review_text::visible_bounded(
                        &issue.detail,
                        agent_task_ui::MAX_TASK_DETAIL_DISPLAY_BYTES,
                    ),
                    ToastKind::Warning,
                );
            }
            for completion in report.completions {
                let outcome = match completion.outcome {
                    agent_task::AgentSessionOutcome::Clean => "finished",
                    agent_task::AgentSessionOutcome::Failed => "failed",
                    agent_task::AgentSessionOutcome::Cancelled => "was cancelled",
                };
                self.push_toast(format!("Codex session {outcome}"), ToastKind::Info);
            }
        }

        self.task_panel.diff.poll();
    }

    /// Open the opaque PTY compatibility path: a new terminal tab running the
    /// provider CLI directly inside the task worktree. Also implements the
    /// explicit terminal fallback after an unsuccessful native session and
    /// the retry of an exited task terminal (the new PTY atomically replaces
    /// the exited transcript binding; sticky provenance is preserved).
    fn task_open_terminal(&mut self, task_id: agent_task::TaskId) {
        if self.agent_runtime.has_preparing(task_id) {
            self.push_toast(
                "Cancel native Codex preparation before starting a terminal",
                ToastKind::Info,
            );
            return;
        }
        let failed_terminal_retry = self
            .task_manager
            .terminal_retry_session_id(task_id)
            .ok()
            .map(str::to_owned);
        let native_recovery = failed_terminal_retry.is_none()
            && self.agent_runtime.can_continue_in_terminal(task_id)
            && self
                .task_manager
                .native_terminal_fallback_eligible(task_id)
                .is_ok();
        let launch = self.task_manager.get(task_id).and_then(|task| {
            ((task.status == agent_task::TaskStatus::Created && task.terminal_session_id.is_none())
                || (native_recovery && task.terminal_session_id.is_none())
                || failed_terminal_retry
                    .as_deref()
                    .is_some_and(|old| task.terminal_session_id.as_deref() == Some(old)))
            .then(|| {
                (
                    task.provider,
                    task.repo_root.clone(),
                    task.worktree_path.clone(),
                )
            })
        });
        let Some((provider, repository, worktree)) = launch else {
            self.push_toast(
                "Task is no longer waiting for an Agent terminal",
                ToastKind::Info,
            );
            return;
        };
        let launch = match agent_task::AgentLaunchSpec::resolve(provider, &repository, &worktree) {
            Ok(launch) => launch,
            Err(error) => {
                if failed_terminal_retry.is_none() && !native_recovery {
                    let _ = self.task_manager.update_status(
                        task_id,
                        agent_task::TaskStatus::Created,
                        Some(error.to_string()),
                    );
                }
                self.push_toast(error.to_string(), ToastKind::Warning);
                return;
            }
        };
        if failed_terminal_retry.is_none() && !native_recovery {
            let _ =
                self.task_manager
                    .update_status(task_id, agent_task::TaskStatus::Starting, None);
        }
        let session_id = self.next_id;
        let session_key = agent_task_ui::terminal_session_id(session_id);
        let spawned = Session::spawn_argv(
            &self.config,
            session_id,
            self.cols,
            self.rows,
            worktree.to_str(),
            Some(&launch.argv),
        );
        let session = match spawned {
            Ok(session) => session,
            Err(error) => {
                if failed_terminal_retry.is_none() && !native_recovery {
                    let _ = self.task_manager.update_status(
                        task_id,
                        agent_task::TaskStatus::Created,
                        Some(error.to_string()),
                    );
                }
                self.push_toast(
                    format!("Could not start {}: {error}", provider.display_name()),
                    ToastKind::Warning,
                );
                return;
            }
        };
        let binding = if let Some(old_session) = failed_terminal_retry.as_deref() {
            self.task_manager
                .bind_terminal_retry_session(task_id, old_session, session_key.clone())
        } else if native_recovery {
            self.task_manager
                .bind_native_terminal_fallback_session(task_id, session_key.clone())
        } else {
            self.task_manager
                .bind_terminal_session(task_id, session_key)
        };
        if let Err(error) = binding {
            // The PTY is live but never gained task authority; tear it down
            // and restore the pre-spawn task state.
            let mut session = session;
            let _ = session.pty.terminate();
            if failed_terminal_retry.is_none() && !native_recovery {
                let _ = self.task_manager.update_status(
                    task_id,
                    agent_task::TaskStatus::Created,
                    Some(error.to_string()),
                );
            }
            self.push_toast(error.to_string(), ToastKind::Warning);
            return;
        }
        if native_recovery {
            self.agent_runtime.clear_retained(task_id);
        }
        self.session_diagnostic = None;
        self.next_id += 1;
        let insert = (self.active + 1).min(self.sessions.len());
        self.sessions.insert(insert, session);
        self.reindex_tabs_after_insert(insert);
        self.open_tab_with(insert);
        self.relayout();
        self.refresh_active_context();
        self.save_session_snapshot();
        self.push_toast(
            format!(
                "Opened {} in an isolated task terminal; task context stays in Frost",
                provider.display_name()
            ),
            ToastKind::Success,
        );
    }

    /// Re-run the task's exact source command in a separate validation
    /// terminal inside the isolated worktree. Preflight re-proves the Git
    /// registration and pins the cwd through an open directory descriptor
    /// before any PTY exists.
    fn task_start_validation(&mut self, task_id: agent_task::TaskId) {
        if let Err(error) = self.task_manager.next_validation_attempt(task_id) {
            self.push_toast(error.to_string(), ToastKind::Warning);
            return;
        }
        let prepared = {
            let Some(task) = self.task_manager.get(task_id) else {
                self.push_toast("Task is no longer available", ToastKind::Info);
                return;
            };
            match agent_task::prepare_task_validation(task) {
                Ok(prepared) => prepared,
                Err(error) => {
                    self.push_toast(error.to_string(), ToastKind::Warning);
                    return;
                }
            }
        };
        let argv = match agent_task_ui::validation_command_argv(
            Some(&prepared.source_shell),
            &prepared.command,
        ) {
            Ok(argv) => argv,
            Err(error) => {
                self.push_toast(error, ToastKind::Warning);
                return;
            }
        };
        let session_id = self.next_id;
        let session_key = agent_task_ui::terminal_session_id(session_id);
        // The child chdirs through the pinned descriptor path, so a replaced
        // worktree directory cannot redirect validation after preflight.
        let pinned_cwd = prepared.pinned_cwd.proc_path();
        let spawned = Session::spawn_argv_env(
            &self.config,
            session_id,
            self.cols,
            self.rows,
            pinned_cwd.to_str(),
            Some(&argv),
            &agent_task_ui::VALIDATION_ENV_OVERRIDES,
        );
        let session = match spawned {
            Ok(session) => session,
            Err(error) => {
                self.push_toast(
                    format!("Could not start the validation terminal: {error}"),
                    ToastKind::Warning,
                );
                return;
            }
        };
        if let Err(error) = self
            .task_manager
            .bind_validation_session(task_id, session_key)
        {
            let mut session = session;
            let _ = session.pty.terminate();
            self.push_toast(error.to_string(), ToastKind::Warning);
            return;
        }
        self.session_diagnostic = None;
        self.next_id += 1;
        let insert = (self.active + 1).min(self.sessions.len());
        self.sessions.insert(insert, session);
        self.reindex_tabs_after_insert(insert);
        self.open_tab_with(insert);
        self.relayout();
        self.refresh_active_context();
        self.save_session_snapshot();
        self.push_toast(
            "Running the exact source command in a validation terminal",
            ToastKind::Info,
        );
    }

    /// The Tasks dashboard dock panel: task list plus the selected task card.
    fn sidebar_tasks_view(&self) -> Element<'_, Message> {
        if !self.config.experimental_task_sidebar {
            return text("Enable the experimental Tasks dashboard in Settings")
                .size(12)
                .style(text::secondary)
                .into();
        }
        let mut body = column![].spacing(6);
        if self.task_panel.pending_creation.is_some() {
            body = body.push(
                text("Creating isolated worktree…")
                    .size(12)
                    .style(text::secondary),
            );
        }
        let mut tasks: Vec<&agent_task::AgentTask> = self
            .task_manager
            .tasks()
            .iter()
            .filter(|task| task.status != agent_task::TaskStatus::Archived)
            .collect();
        tasks.sort_by_key(|task| std::cmp::Reverse(task.updated_at_ms));
        if tasks.is_empty() && self.task_panel.pending_creation.is_none() {
            body = body.push(text("No Agent tasks yet").size(13));
            body = body.push(
                text("Create one from a failed command block's menu (Create task). Each task gets its own Git worktree.")
                    .size(11)
                    .style(text::secondary),
            );
        }
        for task in &tasks {
            let mut label = crate::review_text::visible_bounded(
                &task.title,
                agent_task_ui::MAX_TASK_TITLE_DISPLAY_BYTES,
            );
            if self.task_manager.task_needs_attention(task.id) {
                label = format!("● {label}");
            }
            let row_button = button(text(label).size(12))
                .on_press(Message::TaskSelect(task.id))
                .padding([2, 6])
                .style(self.tab_btn_style(self.task_panel.selected == Some(task.id)));
            body = body.push(
                column![
                    row_button,
                    text(format!(
                        "{} · {}",
                        task.provider.display_name(),
                        task.status.label()
                    ))
                    .size(10)
                    .style(text::secondary),
                ]
                .spacing(1),
            );
        }
        if let Some(task_id) = self.task_panel.selected {
            if let Some(task) = self.task_manager.get(task_id) {
                body = body.push(self.task_card(task));
            }
        }
        scrollable(body).height(Length::Fill).into()
    }

    /// The selected task's detail card: lifecycle state, native turn view,
    /// approvals (display-and-deny), review/validation/terminal actions.
    fn task_card(&self, task: &agent_task::AgentTask) -> Element<'_, Message> {
        use agent_task::{TaskRuntimeKind, TaskStatus, TaskValidationStatus};

        let mut card = column![].spacing(6);
        card = card.push(text(crate::review_text::visible_bounded(&task.title, 200)).size(13));
        card = card.push(
            text(format!(
                "Status: {} · Runtime: {:?}",
                task.status.label(),
                task.runtime_kind
            ))
            .size(11)
            .style(text::secondary),
        );
        card = card.push(
            text(format!(
                "Branch {} · {}",
                crate::review_text::visible_bounded(&task.branch, 128),
                agent_task::diff::visible_diff_cwd(&task.worktree_path),
            ))
            .size(10)
            .style(text::secondary),
        );
        if let Some(detail) = task.status_detail.as_deref() {
            card = card.push(
                text(crate::review_text::visible_bounded(
                    detail,
                    agent_task_ui::MAX_TASK_DETAIL_DISPLAY_BYTES,
                ))
                .size(11)
                .style(text::secondary),
            );
        }

        let preparing = self.agent_runtime.has_preparing(task.id);
        let running = self.agent_runtime.has_running(task.id);
        let stream_active = self.task_manager.has_active_agent_event_stream(task.id);

        // Native session projection: current/latest turn plus pending
        // approvals (display-and-deny) and bounded completed-turn history.
        if let Some(snapshot) = self.agent_runtime.snapshot(task.id) {
            card = card.push(
                text(format!("Native session: {:?}", snapshot.phase))
                    .size(11)
                    .style(text::secondary),
            );
            if !snapshot.agent_text.is_empty() {
                let truncated = if snapshot.agent_text_truncated {
                    " (compacted)"
                } else {
                    ""
                };
                card = card.push(
                    container(
                        scrollable(
                            text(format!(
                                "{}{}",
                                crate::review_text::visible_bounded(
                                    &snapshot.agent_text,
                                    64 * 1024
                                ),
                                truncated
                            ))
                            .size(11)
                            .font(iced::Font::MONOSPACE),
                        )
                        .height(Length::Fixed(160.0)),
                    )
                    .padding(6)
                    .style(container::bordered_box),
                );
            }
            for command in &snapshot.commands {
                card = card.push(
                    text(format!(
                        "$ {} · {}",
                        crate::review_text::visible_bounded(&command.command, 512),
                        command.status
                    ))
                    .size(10)
                    .style(text::secondary),
                );
            }
            for approval in &snapshot.pending_approvals {
                let mut approval_card = column![].spacing(4);
                approval_card = approval_card.push(
                    text(format!(
                        "Managed approval request ({:?}); accepting is disabled",
                        approval.kind
                    ))
                    .size(11)
                    .style(text::danger),
                );
                if let Some(command) = approval.command.as_deref() {
                    approval_card = approval_card.push(
                        text(crate::review_text::visible_bounded(command, 1024))
                            .size(10)
                            .font(iced::Font::MONOSPACE),
                    );
                }
                for path in &approval.file_paths {
                    approval_card = approval_card.push(
                        text(crate::review_text::visible_bounded(path, 512))
                            .size(10)
                            .font(iced::Font::MONOSPACE),
                    );
                }
                if let Some(reason) = approval.reason.as_deref() {
                    approval_card = approval_card.push(
                        text(crate::review_text::visible_bounded(reason, 512))
                            .size(10)
                            .style(text::secondary),
                    );
                }
                approval_card = approval_card.push(
                    button(text("Deny").size(11))
                        .style(button::danger)
                        .on_press(Message::TaskApprovalDeny(task.id, approval.id)),
                );
                card = card.push(
                    container(approval_card)
                        .padding(6)
                        .style(container::bordered_box),
                );
            }
            if !snapshot.turn_history.is_empty() {
                card = card.push(
                    text(format!(
                        "Completed turns: {} ({} in history)",
                        snapshot.completed_turns,
                        snapshot.turn_history.len()
                    ))
                    .size(10)
                    .style(text::secondary),
                );
            }
        }

        // Action rows.
        let mut actions = row![].spacing(6);
        if preparing {
            actions = actions.push(text("Preparing…").size(11).style(text::secondary));
            actions = actions.push(
                button(text("Cancel").size(11))
                    .style(button::secondary)
                    .on_press(Message::TaskCancelCodex(task.id)),
            );
        } else if task.status == TaskStatus::Created
            && task.runtime_kind == TaskRuntimeKind::Unassigned
        {
            let mut start = button(text("Start Codex").size(11)).style(button::primary);
            if agent_task_ui::prompt_policy(&self.config).share_command_context {
                start = start.on_press(Message::TaskStartCodex(task.id));
            }
            actions = actions.push(start);
            actions = actions.push(
                button(text("Open terminal Agent").size(11))
                    .style(button::secondary)
                    .on_press(Message::TaskTerminalOpen(task.id)),
            );
        }
        if running && stream_active {
            actions = actions.push(
                button(text("Cancel Codex").size(11))
                    .style(button::danger)
                    .on_press(Message::TaskCancelCodex(task.id)),
            );
            if task.status == TaskStatus::ReadyForReview {
                actions = actions.push(
                    button(text("Finish Codex").size(11))
                        .style(button::primary)
                        .on_press(Message::TaskFinishCodex(task.id)),
                );
            }
        }
        card = card.push(actions);

        // Review feedback starts another sequential turn on the live native
        // session.
        if running
            && stream_active
            && task.status == TaskStatus::ReadyForReview
            && task.runtime_kind == TaskRuntimeKind::Native
        {
            let completed_turns = self
                .agent_runtime
                .snapshot(task.id)
                .map(|snapshot| snapshot.completed_turns)
                .unwrap_or(0);
            let input = text_input(
                "Review feedback for the next turn…",
                &self.task_panel.follow_up,
            )
            .on_input(Message::TaskFollowUpInput)
            .size(12);
            let mut send = button(text("Send turn").size(11));
            if agent_task_ui::native_follow_up_can_send(&self.task_panel.follow_up, completed_turns)
            {
                send = send.on_press(Message::TaskFollowUpSend(task.id));
            }
            card = card.push(row![input, send].spacing(6));
        }

        // Review / validation / completion actions once the provider has
        // fully stopped.
        if task.status == TaskStatus::ReadyForReview && !stream_active {
            let mut review = row![].spacing(6);
            review = review.push(
                button(text("Review diff").size(11))
                    .style(button::secondary)
                    .on_press(Message::TaskDiffOpen(task.id)),
            );
            if task.validation.status != TaskValidationStatus::Running {
                review = review.push(
                    button(text("Run validation").size(11))
                        .style(button::secondary)
                        .on_press(Message::TaskValidationStart(task.id)),
                );
            }
            let mut complete = button(text("Mark complete").size(11)).style(button::primary);
            if task.validation.status == TaskValidationStatus::Passed {
                complete = complete.on_press(Message::TaskMarkComplete(task.id));
            }
            review = review.push(complete);
            card = card.push(review);
            card = card.push(
                text(format!(
                    "Validation: {}{}",
                    task.validation.status.label(),
                    task.validation
                        .status_detail
                        .as_deref()
                        .map(|detail| format!(
                            " — {}",
                            crate::review_text::visible_bounded(
                                detail,
                                agent_task_ui::MAX_TASK_DETAIL_DISPLAY_BYTES
                            )
                        ))
                        .unwrap_or_default()
                ))
                .size(10)
                .style(text::secondary),
            );
        }

        // Explicit terminal fallback after an unsuccessful native session, or
        // retry of an exited task terminal.
        let terminal_retry = self.task_manager.terminal_retry_session_id(task.id).is_ok();
        let native_fallback = self.agent_runtime.can_continue_in_terminal(task.id)
            && self
                .task_manager
                .native_terminal_fallback_eligible(task.id)
                .is_ok();
        if task.status == TaskStatus::Failed && (terminal_retry || native_fallback) {
            card = card.push(
                button(
                    text(if native_fallback {
                        "Continue in terminal"
                    } else {
                        "Retry terminal Agent"
                    })
                    .size(11),
                )
                .style(button::secondary)
                .on_press(Message::TaskTerminalOpen(task.id)),
            );
        }

        if !task.status.is_running() && task.validation.status != TaskValidationStatus::Running {
            card = card.push(
                button(text("Hide task").size(11))
                    .style(button::secondary)
                    .on_press(Message::TaskHide(task.id)),
            );
        }

        // Bounded worktree diff surface (status + tracked diff against the
        // task's immutable base commit).
        if self.task_panel.diff.is_open {
            let state = self.task_panel.diff.state();
            let mut diff_card = column![].spacing(4);
            diff_card = diff_card.push(
                row![
                    text(format!(
                        "git diff {}",
                        self.task_panel.diff.requested_base().unwrap_or("HEAD")
                    ))
                    .size(10)
                    .style(text::secondary),
                    Space::new().width(Length::Fill),
                    button(text("✕").size(10))
                        .style(button::secondary)
                        .padding(2)
                        .on_press(Message::TaskDiffClose),
                ]
                .align_y(iced::Alignment::Center),
            );
            if state.loading {
                diff_card = diff_card.push(
                    text("Loading tracked changes…")
                        .size(11)
                        .style(text::secondary),
                );
            }
            if let Some(error) = &state.error {
                diff_card = diff_card.push(
                    text(crate::review_text::visible_bounded(error, 1024))
                        .size(11)
                        .style(text::danger),
                );
            }
            if state.truncated {
                diff_card = diff_card.push(
                    text("Diff exceeded the retained display limits")
                        .size(10)
                        .style(text::danger),
                );
            }
            if !state.loading && state.error.is_none() {
                let body_text = if state.text.is_empty() {
                    "No tracked changes relative to the task baseline.".to_string()
                } else {
                    state.text.clone()
                };
                diff_card = diff_card.push(
                    container(
                        scrollable(text(body_text).size(10).font(iced::Font::MONOSPACE))
                            .height(Length::Fixed(240.0)),
                    )
                    .padding(6)
                    .style(container::bordered_box),
                );
            }
            card = card.push(diff_card);
        }

        container(card)
            .padding(8)
            .style(container::bordered_box)
            .into()
    }

    /// Overlay panel for Agent mode: transcript, per-command approval cards,
    /// and the composer. All state lives in `agent::AgentUi`.
    fn agent_panel(&self) -> Element<'_, Message> {
        use jterm_core::agent::{AgentState, ProposalStatus, Turn as AgentTurn};

        let mut transcript = column![].spacing(8);
        let session = self.agent.session.as_ref();
        if let Some(session) = session {
            for (index, turn) in session.transcript().iter().enumerate() {
                let element: Element<'_, Message> = match turn {
                    AgentTurn::User(message) => text(format!("You: {message}")).size(13).into(),
                    AgentTurn::AssistantThought(thought) => text(format!("thought: {thought}"))
                        .size(12)
                        .style(text::secondary)
                        .into(),
                    AgentTurn::AssistantSay(message) => {
                        text(format!("Agent: {message}")).size(13).into()
                    }
                    AgentTurn::ProtocolError(message) => text(format!("protocol: {message}"))
                        .size(12)
                        .style(text::danger)
                        .into(),
                    AgentTurn::Observation {
                        exit_code,
                        output_sample,
                        ..
                    } => {
                        let head = text(format!(
                            "Output (exit {exit_code}, {} bytes)",
                            output_sample.len()
                        ))
                        .size(12)
                        .style(if *exit_code == 0 {
                            text::secondary
                        } else {
                            text::danger
                        });
                        let body = text(output_sample.clone())
                            .size(12)
                            .font(iced::Font::MONOSPACE);
                        container(column![head, body].spacing(4))
                            .padding(6)
                            .style(container::bordered_box)
                            .width(Length::Fill)
                            .into()
                    }
                    AgentTurn::AssistantProposed {
                        id,
                        command,
                        status,
                    } => {
                        let danger = jterm_core::agent::is_dangerous(command);
                        let is_current = matches!(
                            session.state(),
                            AgentState::AwaitingApproval { proposal_id }
                                if proposal_id == *id
                        );
                        let mut card = column![].spacing(6);
                        if let Some(reason) = danger {
                            card = card.push(
                                text(format!("⚠ destructive: {reason}"))
                                    .size(12)
                                    .style(text::danger),
                            );
                        }
                        if let Some((edit_id, buffer)) = self
                            .agent
                            .edit
                            .as_ref()
                            .filter(|(edit_id, _)| edit_id == id)
                        {
                            let visible_buffer = crate::review_text::visible_bounded(
                                buffer,
                                crate::review_text::MAX_AGENT_COMMAND_BYTES,
                            );
                            card = card.push(
                                text_input("command", &visible_buffer)
                                    .id(AGENT_EDIT_INPUT_ID.clone())
                                    .on_input(Message::AgentEditInput)
                                    .on_submit(Message::AgentEditApprove(*edit_id))
                                    .size(13)
                                    .font(iced::Font::MONOSPACE),
                            );
                            card = card.push(
                                row![
                                    button(text("Approve edited").size(12))
                                        .on_press(Message::AgentEditApprove(*edit_id)),
                                    button(text("Cancel").size(12))
                                        .style(button::secondary)
                                        .on_press(Message::AgentEditCancel),
                                ]
                                .spacing(6),
                            );
                        } else {
                            card = card.push(
                                text(crate::review_text::visible_bounded(
                                    command,
                                    crate::review_text::MAX_AGENT_COMMAND_BYTES,
                                ))
                                .size(13)
                                .font(iced::Font::MONOSPACE),
                            );
                            let status_row: Element<'_, Message> = match status {
                                ProposalStatus::Pending if is_current => {
                                    let approve_label = if danger.is_some() {
                                        "Approve & Run (destructive)"
                                    } else {
                                        "Approve & Run"
                                    };
                                    let approve = button(text(approve_label).size(12))
                                        .style(if danger.is_some() {
                                            button::danger
                                        } else {
                                            button::primary
                                        })
                                        .on_press(Message::AgentApprove(*id));
                                    row![
                                        approve,
                                        button(text("Edit").size(12))
                                            .style(button::secondary)
                                            .on_press(Message::AgentEditStart(
                                                *id,
                                                command.clone()
                                            )),
                                        button(text("Reject").size(12))
                                            .style(button::secondary)
                                            .on_press(Message::AgentReject(*id)),
                                    ]
                                    .spacing(6)
                                    .into()
                                }
                                ProposalStatus::Pending => {
                                    text("pending").size(12).style(text::secondary).into()
                                }
                                ProposalStatus::Approved => {
                                    text("✓ ran").size(12).style(text::secondary).into()
                                }
                                ProposalStatus::Rejected => {
                                    text("✗ rejected").size(12).style(text::secondary).into()
                                }
                                ProposalStatus::ManualReview => text("moved to manual review")
                                    .size(12)
                                    .style(text::secondary)
                                    .into(),
                            };
                            card = card.push(status_row);
                        }
                        let _ = index;
                        container(card)
                            .padding(8)
                            .style(container::bordered_box)
                            .width(Length::Fill)
                            .into()
                    }
                };
                transcript = transcript.push(element);
            }
        }
        // While a reply streams in, its readable fields grow in place of the
        // static waiting line; the completed reply then replaces the preview
        // with real transcript turns. After a mid-stream failure the partial
        // text stays visible next to the recorded protocol error.
        match self.agent.reply_preview() {
            Some(preview) => {
                if let Some(thought) = &preview.thought {
                    transcript = transcript.push(
                        text(format!("thought: {thought}"))
                            .size(12)
                            .style(text::secondary),
                    );
                }
                if let Some(command) = &preview.command {
                    transcript = transcript.push(
                        text(format!("proposing: {command}"))
                            .size(13)
                            .font(iced::Font::MONOSPACE),
                    );
                }
                if let Some(message) = &preview.message {
                    transcript = transcript.push(text(format!("Agent: {message}")).size(13));
                }
                if !self.agent.loading {
                    transcript =
                        transcript.push(text("reply interrupted").size(11).style(text::secondary));
                }
            }
            None if self.agent.loading => {
                transcript = transcript.push(
                    text("waiting for the model…")
                        .size(12)
                        .style(text::secondary),
                );
            }
            None => {}
        }

        let (turns_used, max_turns, state_line, can_submit) = match session {
            Some(session) => (
                session.turns_used(),
                session.max_turns(),
                match session.state() {
                    AgentState::Ready => "ready",
                    AgentState::AwaitingModel => "waiting for the model",
                    AgentState::AwaitingApproval { .. } => "a command is waiting for approval",
                    AgentState::AwaitingObservation { .. } => {
                        "waiting for the approved command to finish"
                    }
                    AgentState::Completed => "task completed",
                    AgentState::Cancelled => "session cancelled",
                    AgentState::TurnLimitReached => "turn limit reached",
                },
                session.state() == AgentState::Ready && session.turns_used() < session.max_turns(),
            ),
            None => (0, 0, "no session", false),
        };

        let header = row![
            text("AI Agent").size(14),
            text(if self.agent.provider_label.is_empty() {
                "not configured".to_string()
            } else {
                self.agent.provider_label.clone()
            })
            .size(12)
            .style(text::secondary),
            Space::new().width(Length::Fill),
            text(format!("turns {turns_used}/{max_turns}"))
                .size(12)
                .style(text::secondary),
            button(text("✕").size(12))
                .style(button::secondary)
                .on_press(Message::AgentClose),
        ]
        .spacing(10)
        .align_y(iced::Alignment::Center);

        let status_line: Element<'_, Message> = if self.agent.status.is_empty() {
            text(state_line).size(11).style(text::secondary).into()
        } else {
            text(self.agent.status.clone())
                .size(11)
                .style(text::danger)
                .into()
        };
        let context_line: Element<'_, Message> = match self.agent.last_manual_completed.as_ref() {
            Some(context) => {
                let status = agent_context_exit_label(context.exit_code);
                row![
                    text(format!("attached context: `{}` ({status})", context.cmd))
                        .size(10)
                        .style(text::secondary),
                    button(text("✕").size(10))
                        .style(button::secondary)
                        .padding(2)
                        .on_press(Message::AgentClearContext),
                ]
                .spacing(6)
                .align_y(iced::Alignment::Center)
                .into()
            }
            None => Space::new().into(),
        };

        // A finished task can be followed up (same transcript, budget
        // permitting) or replaced by a fresh one in the same binding.
        let (can_continue, can_restart) = match session {
            Some(session) => (
                session.can_continue_after_completion(),
                matches!(
                    session.state(),
                    AgentState::Completed | AgentState::TurnLimitReached
                ),
            ),
            None => (false, false),
        };
        let followup_row: Element<'_, Message> = if can_continue || can_restart {
            let mut buttons = row![].spacing(6);
            if can_continue {
                buttons = buttons.push(
                    button(text("Continue task").size(12)).on_press(Message::AgentContinueTask),
                );
            }
            if can_restart {
                buttons = buttons.push(
                    button(text("New task").size(12))
                        .style(button::secondary)
                        .on_press(Message::AgentNewTask),
                );
            }
            buttons.into()
        } else {
            Space::new().into()
        };

        let mut input = text_input(
            "What do you want to do? Every command needs your approval.",
            &self.agent.input,
        )
        .id(AGENT_INPUT_ID.clone())
        .size(13);
        if can_submit {
            input = input
                .on_input(Message::AgentInput)
                .on_submit(Message::AgentSubmit);
        }
        let mut send = button(text("Send").size(12));
        if can_submit {
            send = send.on_press(Message::AgentSubmit);
        }

        let inner = container(
            column![
                header,
                scrollable(transcript).height(Length::Fill).anchor_bottom(),
                status_line,
                context_line,
                followup_row,
                row![input, send].spacing(6),
            ]
            .spacing(8),
        )
        .width(Length::Fixed(560.0))
        .height(Length::Fixed(520.0))
        .padding(12)
        .style(container::dark);
        container(inner)
            .align_right(Length::Fill)
            .align_top(Length::Fill)
            .padding(10)
            .into()
    }

    fn subscription(&self) -> Subscription<Message> {
        let mut subs: Vec<Subscription<Message>> = self
            .sessions
            .iter()
            .map(|s| {
                pty_subscription(PtySubscriptionKey {
                    id: s.id,
                    master_fd: s.master_fd,
                    reader_fd: Arc::clone(&s.reader_fd),
                })
            })
            .collect();
        let events = iced::event::listen_with(|event, status, _id| match event {
            iced::Event::Keyboard(keyboard::Event::ModifiersChanged(m)) => {
                Some(Message::ModifiersChanged(m))
            }
            // When an overlay text input is focused it captures the keys it
            // consumes (typing, Backspace, cursor movement). Dropping captured
            // keyboard events here keeps them from also reaching the terminal,
            // so editing the search/palette query never double-inputs.
            iced::Event::Keyboard(_) if status == iced::event::Status::Captured => None,
            iced::Event::Keyboard(k) => Some(Message::Key(k)),
            iced::Event::InputMethod(_) if status == iced::event::Status::Captured => None,
            iced::Event::InputMethod(ime) => Some(Message::Ime(ime)),
            iced::Event::Window(iced::window::Event::Resized(size)) => Some(Message::Resized(size)),
            iced::Event::Window(iced::window::Event::FileDropped(path)) => {
                Some(Message::ImageDropped(path))
            }
            iced::Event::Window(iced::window::Event::Focused) => Some(Message::Focus(true)),
            iced::Event::Window(iced::window::Event::Unfocused) => Some(Message::Focus(false)),
            iced::Event::Window(iced::window::Event::CloseRequested) => Some(Message::WindowClose),
            // Catch every left-button release so a tab drag that ends outside
            // any tab still clears `dragging_tab`. When the release lands on a
            // tab, mouse_area's on_release fires Message::TabDragEnd first
            // (which already consumes `dragging_tab`), so this becomes a no-op.
            iced::Event::Mouse(iced::mouse::Event::ButtonReleased(iced::mouse::Button::Left)) => {
                Some(Message::TabDragCancel)
            }
            iced::Event::Touch(
                iced::touch::Event::FingerLifted { .. } | iced::touch::Event::FingerLost { .. },
            ) => Some(Message::TabDragCancel),
            _ => None,
        });
        subs.push(events);
        // A right-press on a tab carries no coordinates, so the context menu
        // needs the pointer tracked separately. Track it only while a tab is
        // hovered: a motion message per event over the whole window would
        // rebuild the view (grid included) on every mouse move, which is why
        // the terminal itself only reports motion while dragging.
        if self.hovered_tab.is_some() {
            subs.push(iced::event::listen_with(
                |event, _status, _id| match event {
                    iced::Event::Mouse(iced::mouse::Event::CursorMoved { position }) => {
                        Some(Message::TabPointerMoved(position))
                    }
                    _ => None,
                },
            ));
        }
        // Same trick for the file-ops menu anchor: track the window-space
        // pointer only while it is over the dock with the file tree showing,
        // so motion elsewhere never rebuilds the view.
        if self.sidebar_hovered && self.dock_open() && self.sidebar_panel == SidebarPanel::Files {
            subs.push(iced::event::listen_with(
                |event, _status, _id| match event {
                    iced::Event::Mouse(iced::mouse::Event::CursorMoved { position }) => {
                        Some(Message::SidebarPointerMoved(position))
                    }
                    _ => None,
                },
            ));
        }
        if self.tab_drag_hover_since.is_some() {
            subs.push(
                iced::time::every(std::time::Duration::from_millis(50))
                    .map(|_| Message::TabDragHoverTick),
            );
        }
        subs.push(
            iced::time::every(std::time::Duration::from_millis(1500)).map(|_| Message::ConfigTick),
        );
        // Tasks dashboard pump: fast while a provider is starting/running or a
        // worktree is being created, slow while the panel merely shows parked
        // review state. With the flag off and no activity there is no tick.
        let tasks_open = self.sidebar_open && self.sidebar_panel == SidebarPanel::Tasks;
        if self.agent_runtime.has_any_activity()
            || self.task_panel.pending_creation.is_some()
            || self.task_panel.diff.is_open
            || tasks_open
        {
            let interval = if self.agent_runtime.needs_fast_poll()
                || self.task_panel.pending_creation.is_some()
                || self.task_panel.diff.state().loading
            {
                std::time::Duration::from_millis(50)
            } else {
                std::time::Duration::from_millis(500)
            };
            subs.push(iced::time::every(interval).map(|_| Message::TaskTick));
        }
        // The blink tick redraws and re-shapes the whole grid every 530ms purely
        // to animate blinking cells. Run it only while focused AND when a visible
        // pane actually has blinking text — the common case (no blink, or
        // unfocused) then stays fully idle.
        let has_blink = self.layout().leaves().iter().any(|&idx| {
            self.sessions.get(idx).is_some_and(|s| {
                s.projection
                    .cells()
                    .iter()
                    .flatten()
                    .any(|cell| cell.flags.blink())
            })
        });
        if self.focused && has_blink {
            subs.push(
                iced::time::every(std::time::Duration::from_millis(530))
                    .map(|_| Message::BlinkTick),
            );
        }
        // Running output can be quiet for minutes (sleep, a remote build, a
        // network wait), so PTY reads alone cannot animate its elapsed badge.
        // Keep the timer scoped to visible panes while block chrome is on;
        // the normal idle terminal still has no extra redraw subscription.
        let pane_running_badge_interval = |index: usize| {
            self.sessions.get(index).and_then(|session| {
                let terminal = &session.terminal;
                if terminal.is_alt_buffer_active() {
                    return None;
                }
                let row = terminal.running_zone_start()?;
                let raw_row = terminal.raw_row_id_at_absolute(row)?;
                let (view_row, _) = session.projection.raw_row_view_bounds(raw_row)?;
                let (_, elapsed_ms) = self.fitting_running_badge(session, view_row)?;
                Some(if elapsed_ms < 3_600_000 {
                    std::time::Duration::from_secs(1)
                } else {
                    // The family duration formatter drops seconds at one
                    // hour, so its visible text changes once per minute.
                    std::time::Duration::from_secs(60)
                })
            })
        };
        let running_badge_interval = if !self.config.block_mode {
            None
        } else if self.is_split() && self.pane_zoomed {
            pane_running_badge_interval(self.active)
        } else {
            self.layout()
                .leaves()
                .iter()
                .filter_map(|&idx| pane_running_badge_interval(idx))
                .min()
        };
        if let Some(interval) = running_badge_interval {
            subs.push(iced::time::every(interval).map(|_| Message::BlockElapsedTick));
        }
        if self.sessions.iter().any(Session::has_pending_write) {
            subs.push(
                iced::time::every(std::time::Duration::from_millis(8))
                    .map(|_| Message::PtyWriteTick),
            );
        }
        if self.search.is_open && self.search_dirty {
            subs.push(
                iced::time::every(std::time::Duration::from_millis(50))
                    .map(|_| Message::SearchRefreshTick),
            );
        }
        if self.history_reflow_due.is_some() {
            subs.push(
                iced::time::every(std::time::Duration::from_millis(50))
                    .map(|_| Message::HistoryReflowTick),
            );
        }
        if !self.toasts.is_empty() {
            subs.push(
                iced::time::every(std::time::Duration::from_millis(250))
                    .map(|_| Message::ToastTick),
            );
        }
        Subscription::batch(subs)
    }
}

/// A labeled settings row: fixed-width label, the control, then its value.
fn slider_row<'a>(
    label: &'static str,
    value: String,
    control: Element<'a, Message>,
) -> Element<'a, Message> {
    row![
        text(label).size(13).width(Length::Fixed(120.0)),
        control,
        text(value).size(13).width(Length::Fixed(64.0)),
    ]
    .spacing(10)
    .align_y(iced::Alignment::Center)
    .into()
}

fn sidebar_load_task(request: sidebar::DirectoryRequest) -> Task<Message> {
    Task::perform(
        async move { sidebar::load_directory(request) },
        Message::SidebarLoaded,
    )
}

/// The menu/notice verb for a paste: copy/move within one location,
/// download/upload (or a relay) across locations.
fn transfer_verb(
    src_loc: &remote_fs::FsLocation,
    dst_loc: &remote_fs::FsLocation,
    cut: bool,
) -> String {
    use remote_fs::FsLocation;
    let transport = match (src_loc, dst_loc) {
        (FsLocation::Local, FsLocation::Remote(_)) => "upload",
        (FsLocation::Remote(_), FsLocation::Local) => "download",
        (FsLocation::Remote(i), FsLocation::Remote(j)) if i != j => "relay",
        _ => return if cut { "move" } else { "copy" }.to_string(),
    };
    if cut {
        format!("{transport} + delete")
    } else {
        transport.to_string()
    }
}

/// Turn the clipboard into the op a paste should run. Same-location keeps the
/// copy/move semantics; cross-location becomes a download, an upload, or a
/// remote→remote relay, with a cut meaning transfer-then-delete-source.
fn sidebar_paste_op(
    clipboard: &FsClipboard,
    current: &remote_fs::FsLocation,
    target_dir: &std::path::Path,
) -> Result<SidebarOp, String> {
    let Some(dst) = remote_fs::paste_destination(target_dir, &clipboard.path) else {
        return Err("That source has no file name to paste".to_string());
    };
    let src = clipboard.path.clone();
    if clipboard.loc == *current {
        return Ok(if clipboard.cut {
            SidebarOp::Move { src, dst }
        } else {
            SidebarOp::Copy { src, dst }
        });
    }
    let src_loc = clipboard.loc.clone();
    let dst_loc = current.clone();
    let is_dir = clipboard.is_dir;
    Ok(if clipboard.cut {
        SidebarOp::TransferMove {
            src_loc,
            dst_loc,
            src,
            dst,
            is_dir,
        }
    } else {
        SidebarOp::Transfer {
            src_loc,
            dst_loc,
            src,
            dst,
            is_dir,
        }
    })
}

/// Run one sidebar file operation off the UI thread. The location and hosts
/// snapshot travel with the op, so a config edit or location switch made
/// while it runs cannot redirect it to another machine.
fn sidebar_op_task(
    location: remote_fs::FsLocation,
    hosts: Vec<jterm_core::jsh_remote::RemoteHostConfig>,
    op: SidebarOp,
) -> Task<Message> {
    Task::perform(
        async move {
            let (report_location, warning, result) = run_sidebar_op(&location, &hosts, &op);
            SidebarOpReport {
                location: report_location,
                op,
                warning,
                result: result.map_err(|error| error.to_string()),
            }
        },
        Message::SidebarOpFinished,
    )
}

/// Execute one op and report `(changed_location, warning, result)`. Factored
/// out of the task so the ordering (transfer first, delete source after) is
/// testable headlessly.
fn run_sidebar_op(
    location: &remote_fs::FsLocation,
    hosts: &[jterm_core::jsh_remote::RemoteHostConfig],
    op: &SidebarOp,
) -> (remote_fs::FsLocation, Option<String>, std::io::Result<()>) {
    match op {
        SidebarOp::CreateFile(path) => (
            location.clone(),
            None,
            remote_fs::create_file(location, hosts, path),
        ),
        SidebarOp::CreateDir(path) => (
            location.clone(),
            None,
            remote_fs::create_dir(location, hosts, path),
        ),
        SidebarOp::Rename { src, dst } | SidebarOp::Move { src, dst } => (
            location.clone(),
            None,
            remote_fs::rename(location, hosts, src, dst),
        ),
        SidebarOp::Delete(path) => (
            location.clone(),
            None,
            remote_fs::delete(location, hosts, path),
        ),
        SidebarOp::Copy { src, dst } => (
            location.clone(),
            None,
            remote_fs::copy(location, hosts, src, dst),
        ),
        SidebarOp::Transfer {
            src_loc,
            dst_loc,
            src,
            dst,
            is_dir,
        } => (
            dst_loc.clone(),
            None,
            remote_fs::transfer(src_loc, dst_loc, hosts, src, dst, *is_dir),
        ),
        SidebarOp::TransferMove {
            src_loc,
            dst_loc,
            src,
            dst,
            is_dir,
        } => {
            // Cut across locations = copy, then delete the source. A failed
            // transfer never touches the source; a failed delete after a
            // successful transfer is a partial success, reported as a warning.
            if let Err(error) = remote_fs::transfer(src_loc, dst_loc, hosts, src, dst, *is_dir) {
                return (dst_loc.clone(), None, Err(error));
            }
            match remote_fs::delete(src_loc, hosts, src) {
                Ok(()) => (dst_loc.clone(), None, Ok(())),
                Err(error) => (
                    dst_loc.clone(),
                    Some(format!(
                        "Copied to {}, but deleting the source failed: {error}",
                        dst.display()
                    )),
                    Ok(()),
                ),
            }
        }
    }
}

/// Score and sort tabs against the switcher query. Empty query returns all in
/// strip order; otherwise returns matches highest score first as
/// `(filtered_position, tab_index)` tuples. Used by both the renderer and the
/// key handler so navigation matches the visible list.
///
/// `labels` holds one entry per tab, taken from that tab's selected pane.
fn tab_switcher_filtered(labels: &[String], query: &str) -> Vec<(usize, usize)> {
    use fuzzy_matcher::skim::SkimMatcherV2;
    use fuzzy_matcher::FuzzyMatcher;
    if query.is_empty() {
        return labels.iter().enumerate().map(|(i, _)| (i, i)).collect();
    }
    let matcher = SkimMatcherV2::default();
    let mut scored: Vec<(i64, usize)> = labels
        .iter()
        .enumerate()
        .filter_map(|(i, label)| matcher.fuzzy_match(label, query).map(|sc| (sc, i)))
        .collect();
    scored.sort_by_key(|item| std::cmp::Reverse(item.0));
    scored
        .into_iter()
        .enumerate()
        .map(|(pos, (_, idx))| (pos, idx))
        .collect()
}

/// Resident set size of this process in MB (Linux /proc), for the debug panel.
fn read_rss_mb() -> Option<f64> {
    let content = std::fs::read_to_string("/proc/self/status").ok()?;
    for line in content.lines() {
        if let Some(rest) = line.strip_prefix("VmRSS:") {
            let kb: u64 = rest.split_whitespace().next()?.parse().ok()?;
            return Some(kb as f64 / 1024.0);
        }
    }
    None
}

/// xterm button code for press/motion reports.
fn btn_code(button: MouseButton) -> u8 {
    match button {
        MouseButton::Left => 0,
        MouseButton::Middle => 1,
        MouseButton::Right => 2,
    }
}

/// Abbreviate the home directory to `~` for compact path display (status bar,
/// history picker rows).
fn abbreviate_home(path: &str) -> String {
    if let Some(home) = dirs::home_dir().and_then(|h| h.to_str().map(String::from)) {
        if let Some(rest) = path.strip_prefix(&home) {
            return format!("~{rest}");
        }
    }
    path.to_string()
}

/// Submit an OSC 9/777 notification to one bounded worker. The worker owns and
/// waits for every `notify-send` child, preventing zombies; a stuck notifier can
/// fill at most this small queue instead of spawning unbounded processes/threads.
fn enqueue_desktop_notification(title: String, body: String) {
    type Notification = (String, String);
    static SENDER: std::sync::OnceLock<std::sync::mpsc::SyncSender<Notification>> =
        std::sync::OnceLock::new();

    let sender = SENDER.get_or_init(|| {
        let (sender, receiver) = std::sync::mpsc::sync_channel::<Notification>(8);
        let _ = std::thread::Builder::new()
            .name("frost-notifications".to_string())
            .spawn(move || {
                while let Ok((title, body)) = receiver.recv() {
                    let _ = jterm_core::helper::notify_send(&title, &body);
                }
            });
        sender
    });
    let _ = sender.try_send((title, body));
}

/// Bytes that put an approved agent command on the bound session's prompt and
/// run it.
///
/// `clear_line_first` is unconditional: the "prompt is ready" the approval UI
/// implies says nothing about the line buffer being empty, so without the
/// `Ctrl+U` the command is appended to whatever the user had half-typed and
/// submitted in that mangled form. The submitting CR lands outside the frame
/// because readline deliberately does not execute newlines inside a bracketed
/// paste. The local compatibility gate rejects visual spoofing, and the
/// payload policy strips C0/C1 again immediately before the PTY write.
fn agent_command_payload(
    command: &str,
    bracketed: bool,
) -> Result<pty_input::Paste, crate::review_text::ReviewTextError> {
    let command = crate::review_text::sanitize_untrusted_single_line(
        command,
        crate::review_text::MAX_AGENT_COMMAND_BYTES,
    )?;
    Ok(pty_input::encode_prompt_insert(
        &command,
        PasteModes { bracketed },
        PastePolicy {
            strip_controls: true,
            submit: true,
            ..PastePolicy::prompt_insert(UnbracketedMultiline::SendVerbatim)
        },
        true,
    ))
}

/// Build a single OSC 5522 packet: `ESC ] 5522 ; <metadata> [; <payload>] ESC \`.
fn osc_5522_packet(metadata: &str, payload: Option<&str>) -> Vec<u8> {
    let mut packet = Vec::new();
    packet.extend_from_slice(b"\x1b]5522;");
    packet.extend_from_slice(metadata.as_bytes());
    if let Some(payload) = payload {
        packet.extend_from_slice(b";");
        packet.extend_from_slice(payload.as_bytes());
    }
    packet.extend_from_slice(b"\x1b\\");
    packet
}

/// Build the OK/DATA/DONE sequence answering an OSC 5522 MIME-data read.
fn clipboard_5522_response_for_mime(mime_type: &str, data: &[u8]) -> Vec<u8> {
    use base64::Engine;
    let engine = base64::engine::general_purpose::STANDARD;
    let encoded_mime = engine.encode(mime_type.as_bytes());
    let encoded_data = engine.encode(data);
    let mut output = Vec::new();
    output.extend_from_slice(&osc_5522_packet("type=read:status=OK", None));
    output.extend_from_slice(&osc_5522_packet(
        &format!("type=read:status=DATA:mime={encoded_mime}"),
        Some(&encoded_data),
    ));
    output.extend_from_slice(&osc_5522_packet("type=read:status=DONE", None));
    output
}

fn pty_subscription(key: PtySubscriptionKey) -> Subscription<Message> {
    // Key on the stable session id (not the raw fd): a closed session's fd
    // number can be reused by a new session, and keying on fd alone would let
    // iced confuse the two and reuse the old reader thread on the reused fd.
    Subscription::run_with(key, |key: &PtySubscriptionKey| pty_stream(key.clone()))
}

fn pty_stream(key: PtySubscriptionKey) -> impl iced::futures::Stream<Item = Message> {
    use iced::futures::{SinkExt, StreamExt};
    iced::stream::channel(
        2,
        move |mut output: iced::futures::channel::mpsc::Sender<Message>| async move {
            let id = key.id;
            let fd = key.master_fd;
            // Each message is capped at 1 MiB below. Two shallow handoff queues
            // keep only a few MiB resident per session while backpressuring read(2).
            let (mut tx, mut rx) = iced::futures::channel::mpsc::channel::<Message>(2);
            // Self-pipe so dropping this subscription (session/tab closed) wakes the
            // reader thread and stops it BEFORE it can read from a PTY fd whose
            // number may have been reused by a freshly spawned session.
            let (shutdown_r, shutdown_w) = match Pty::make_shutdown_pipe() {
                Ok((read_fd, write_fd)) => {
                    // SAFETY: make_shutdown_pipe returns two fresh owned fds.
                    unsafe {
                        (
                            OwnedFd::from_raw_fd(read_fd),
                            OwnedFd::from_raw_fd(write_fd),
                        )
                    }
                }
                Err(error) => {
                    log::error!("[PTY] failed to create reader shutdown pipe: {error}");
                    let _ = output.send(Message::PtyExited(id, fd, -1)).await;
                    return;
                }
            };
            let reader_fd = key.reader_fd;
            let spawn_result = std::thread::Builder::new()
                .name(format!("frost-pty-{id}"))
                .spawn(move || {
                    let reader_raw = reader_fd.as_raw_fd();
                    let shutdown_raw = shutdown_r.as_raw_fd();
                    // Drain everything currently readable into one message instead of
                    // emitting a separate message per 64 KiB read. Bursty output (e.g.
                    // `cat bigfile`) then triggers far fewer process/refresh/render
                    // cycles, while a lone keystroke still hits WouldBlock immediately
                    // and is delivered with no added latency. Capped so the UI gets a
                    // chance to repaint between very large bursts.
                    const COALESCE_CAP: usize = 1 << 20; // 1 MiB per message
                    let mut buf = vec![0u8; 65536];
                    loop {
                        match Pty::wait_fd_or_shutdown(reader_raw, shutdown_raw, 200) {
                            Ok(ReaderPoll::Shutdown) => break,
                            Ok(ReaderPoll::Timeout) => continue,
                            Ok(ReaderPoll::Data) => {
                                let mut acc: Vec<u8> = Vec::new();
                                let mut exited = false;
                                let mut errored = false;
                                loop {
                                    let n = unsafe {
                                        libc::read(
                                            reader_raw,
                                            buf.as_mut_ptr() as *mut libc::c_void,
                                            buf.len(),
                                        )
                                    };
                                    if n > 0 {
                                        acc.extend_from_slice(&buf[..n as usize]);
                                        if acc.len() >= COALESCE_CAP {
                                            break;
                                        }
                                    } else if n == 0 {
                                        exited = true;
                                        break;
                                    } else {
                                        let err = std::io::Error::last_os_error();
                                        if err.kind() == std::io::ErrorKind::WouldBlock {
                                            break;
                                        }
                                        if err.raw_os_error() == Some(libc::EINTR) {
                                            continue;
                                        }
                                        errored = true;
                                        break;
                                    }
                                }
                                if !acc.is_empty()
                                    && iced::futures::executor::block_on(
                                        tx.send(Message::PtyOutput(id, fd, acc)),
                                    )
                                    .is_err()
                                {
                                    break;
                                }
                                if exited {
                                    let _ = iced::futures::executor::block_on(
                                        tx.send(Message::PtyExited(id, fd, 0)),
                                    );
                                    break;
                                }
                                if errored {
                                    let _ = iced::futures::executor::block_on(
                                        tx.send(Message::PtyExited(id, fd, -1)),
                                    );
                                    break;
                                }
                            }
                            Err(_) => {
                                let _ = iced::futures::executor::block_on(
                                    tx.send(Message::PtyExited(id, fd, -1)),
                                );
                                break;
                            }
                        }
                    }
                });
            if let Err(error) = spawn_result {
                log::error!("[PTY] failed to spawn reader thread: {error}");
                let _ = output.send(Message::PtyExited(id, fd, -1)).await;
                return;
            }
            // Dropping this owned write end (subscription removed) signals the reader.
            let _shutdown_guard = shutdown_w;
            while let Some(msg) = rx.next().await {
                if output.send(msg).await.is_err() {
                    break;
                }
            }
        },
    )
}

/// Build the canonical binding string (e.g. `"ctrl+shift+t"`) for a key event
/// by constructing a `jterm_core::keybindings::Chord` from the iced key data
/// and rendering `Chord::canonical()` — the exact form `KeyBinding::canonical`
/// produces for keybindings.toml strings, so the runtime and config sides can
/// never disagree on folding. (The previous hand-rolled version lowercased
/// with `to_ascii_lowercase`, so a non-ASCII binding like `ctrl+ю` was never
/// stored in the same case the keyboard delivered.)
/// Returns `None` for keys that should never be treated as shortcuts — plain
/// character input (no Ctrl/Alt/Super) and unmappable named keys — so ordinary
/// typing is never swallowed by the keybinding layer.
fn key_to_binding_string(key: &keyboard::Key, mods: keyboard::Modifiers) -> Option<String> {
    use jterm_core::keybindings::{Chord, KeySym, Mods, NamedKey};
    use keyboard::key::Named;
    use keyboard::Key;
    let sym = match key {
        Key::Character(s) => {
            // Shift alone just changes case; require a "real" modifier so typing
            // an uppercase letter can't trigger a command.
            if !(mods.control() || mods.alt() || mods.logo()) {
                return None;
            }
            // Unicode-lowercase, matching the chord core's storage invariant.
            // A char whose lowercase expands to several chars ('İ') has no
            // storable chord form; core's parser rejects it too.
            let mut lower = s.chars().next()?.to_lowercase();
            match (lower.next(), lower.next()) {
                (Some(c), None) => KeySym::Char(c),
                _ => return None,
            }
        }
        Key::Named(named) => match named {
            Named::Tab => KeySym::Named(NamedKey::Tab),
            Named::Enter => KeySym::Named(NamedKey::Return),
            Named::Escape => KeySym::Named(NamedKey::Escape),
            Named::Backspace => KeySym::Named(NamedKey::Backspace),
            Named::Delete => KeySym::Named(NamedKey::Delete),
            Named::Insert => KeySym::Named(NamedKey::Insert),
            Named::Home => KeySym::Named(NamedKey::Home),
            Named::End => KeySym::Named(NamedKey::End),
            Named::PageUp => KeySym::Named(NamedKey::PageUp),
            Named::PageDown => KeySym::Named(NamedKey::PageDown),
            Named::ArrowUp => KeySym::Named(NamedKey::Up),
            Named::ArrowDown => KeySym::Named(NamedKey::Down),
            Named::ArrowLeft => KeySym::Named(NamedKey::Left),
            Named::ArrowRight => KeySym::Named(NamedKey::Right),
            Named::Space => KeySym::Named(NamedKey::Space),
            Named::F1 => KeySym::Function(1),
            Named::F2 => KeySym::Function(2),
            Named::F3 => KeySym::Function(3),
            Named::F4 => KeySym::Function(4),
            Named::F5 => KeySym::Function(5),
            Named::F6 => KeySym::Function(6),
            Named::F7 => KeySym::Function(7),
            Named::F8 => KeySym::Function(8),
            Named::F9 => KeySym::Function(9),
            Named::F10 => KeySym::Function(10),
            Named::F11 => KeySym::Function(11),
            Named::F12 => KeySym::Function(12),
            _ => return None,
        },
        _ => return None,
    };
    let chord = Chord {
        mods: Mods {
            ctrl: mods.control(),
            shift: mods.shift(),
            alt: mods.alt(),
            sup: mods.logo(),
        },
        key: sym,
    };
    Some(chord.canonical())
}

/// Resolve a configured chord, then apply Anvil's output-only copy modifier:
/// when an Alt chord has no explicit binding but the same chord without Alt is
/// the configured Copy action, treat it as Copy Block Output. Exact bindings
/// always win, so users can deliberately override Alt+their-copy shortcut.
fn resolve_keybinding_command(
    bindings: &keybindings::KeyBindings,
    key: &keyboard::Key,
    mods: keyboard::Modifiers,
) -> Option<keybindings::Command> {
    let binding = key_to_binding_string(key, mods)?;
    if let Some(command) = bindings.get_command(&binding) {
        return Some(command);
    }
    if !mods.alt() {
        return None;
    }
    let without_alt = mods & !keyboard::Modifiers::ALT;
    let base = key_to_binding_string(key, without_alt)?;
    matches!(
        bindings.get_command(&base),
        Some(keybindings::Command::EditCopy)
    )
    .then_some(keybindings::Command::EditCopyBlockOutput)
}

/// Flags describing which enhanced-keyboard protocols an application has
/// enabled, sampled from the focused terminal before encoding a key press.
#[derive(Clone, Copy, Default)]
struct KeyboardEnhancements {
    kitty_flags: u16,
    modify_other_keys: u16,
    format_other_keys: u16,
    report_all_keys: bool,
    application_keypad: bool,
}

/// Translate an iced key press into the bytes to send to the PTY.
fn encode_key(
    key: &keyboard::Key,
    location: keyboard::Location,
    mods: keyboard::Modifiers,
    text: Option<&str>,
    app_cursor: bool,
    enh: KeyboardEnhancements,
) -> Option<Vec<u8>> {
    use keyboard::key::Named;
    use keyboard::Key;

    let ctrl = mods.control();
    let alt = mods.alt();

    // Enhanced keyboard protocols (Kitty / xterm modifyOtherKeys) take
    // precedence when an app has enabled them. Unlike ember/egui, iced puts
    // committed text on this same key event; there is no second text event to
    // suppress. Skipping an alphanumeric key here would therefore violate
    // Kitty's report-all-keys mode and send plain text instead.
    if let Some(enc) = kitty_encode_key(key, mods, text, enh.kitty_flags) {
        return Some(enc);
    }
    if let Some(enc) = xterm_modify_other_keys_encode(
        key,
        mods,
        text,
        enh.modify_other_keys,
        enh.format_other_keys,
        enh.report_all_keys,
    ) {
        return Some(enc);
    }

    let csi = |c: &str| -> Vec<u8> { format!("\x1b[{c}").into_bytes() };
    let ss3 = |c: &str| -> Vec<u8> { format!("\x1bO{c}").into_bytes() };

    match key {
        Key::Named(named) => {
            let mut bytes = match named {
                Named::Enter => {
                    if enh.application_keypad && location == keyboard::Location::Numpad {
                        ss3("M")
                    } else {
                        vec![b'\r']
                    }
                }
                Named::Backspace => vec![if ctrl { 0x08 } else { 0x7f }],
                Named::Tab => {
                    if mods.shift() {
                        csi("Z")
                    } else {
                        vec![b'\t']
                    }
                }
                Named::Escape => vec![0x1b],
                Named::Space => vec![if ctrl { 0x00 } else { b' ' }],
                _ => {
                    return legacy_function_key_sequence(
                        named,
                        mods,
                        app_cursor,
                        enh.report_all_keys,
                    )
                }
            };
            if alt {
                bytes.insert(0, 0x1b);
            }
            Some(bytes)
        }
        Key::Character(s) => {
            let c = s.chars().next()?;
            if ctrl {
                // Map Ctrl+key to the corresponding control byte.
                let b = c.to_ascii_lowercase() as u8;
                let ctrl_byte = match b {
                    b'a'..=b'z' => b & 0x1f,
                    b'@' => 0,
                    b'[' => 0x1b,
                    b'\\' => 0x1c,
                    b']' => 0x1d,
                    b'^' => 0x1e,
                    b'_' => 0x1f,
                    b' ' => 0,
                    _ => return text.map(|t| t.as_bytes().to_vec()),
                };
                let mut v = Vec::new();
                if alt {
                    v.push(0x1b);
                }
                v.push(ctrl_byte);
                Some(v)
            } else if let Some(t) = text {
                let mut v = Vec::new();
                if alt {
                    v.push(0x1b);
                }
                v.extend_from_slice(t.as_bytes());
                Some(v)
            } else {
                let mut v = Vec::new();
                if alt {
                    v.push(0x1b);
                }
                v.extend_from_slice(s.as_bytes());
                Some(v)
            }
        }
        Key::Unidentified => text.map(|t| t.as_bytes().to_vec()),
    }
}

/// Encode the legacy xterm/terminfo functional-key family. Modified cursor,
/// editing, and function keys carry a parameter instead of losing Ctrl/Shift
/// or being represented as an ambiguous ESC prefix.
fn legacy_function_key_sequence(
    named: &keyboard::key::Named,
    mods: keyboard::Modifiers,
    app_cursor: bool,
    force_modifier: bool,
) -> Option<Vec<u8>> {
    use keyboard::key::Named;

    let csi = |body: &str| format!("\x1b[{body}").into_bytes();
    let ss3 = |final_byte: char| format!("\x1bO{final_byte}").into_bytes();
    let has_modifier = mods.shift() || mods.alt() || mods.control() || mods.logo();
    let modified = force_modifier || has_modifier;
    let modifier = keyboard_modifier_value(mods);
    let cursor = |final_byte: char| {
        if modified {
            csi(&format!("1;{modifier}{final_byte}"))
        } else if app_cursor {
            ss3(final_byte)
        } else {
            csi(&final_byte.to_string())
        }
    };
    let tilde = |code: u8| {
        if modified {
            csi(&format!("{code};{modifier}~"))
        } else {
            csi(&format!("{code}~"))
        }
    };
    let function = |final_byte: char| {
        if modified {
            csi(&format!("1;{modifier}{final_byte}"))
        } else {
            ss3(final_byte)
        }
    };

    Some(match named {
        Named::ArrowUp => cursor('A'),
        Named::ArrowDown => cursor('B'),
        Named::ArrowRight => cursor('C'),
        Named::ArrowLeft => cursor('D'),
        Named::Home => cursor('H'),
        Named::End => cursor('F'),
        Named::PageUp => tilde(5),
        Named::PageDown => tilde(6),
        Named::Delete => tilde(3),
        Named::Insert => tilde(2),
        Named::F1 => function('P'),
        Named::F2 => function('Q'),
        Named::F3 => function('R'),
        Named::F4 => function('S'),
        Named::F5 => tilde(15),
        Named::F6 => tilde(17),
        Named::F7 => tilde(18),
        Named::F8 => tilde(19),
        Named::F9 => tilde(20),
        Named::F10 => tilde(21),
        Named::F11 => tilde(23),
        Named::F12 => tilde(24),
        _ => return None,
    })
}

/// The base Unicode codepoint a key reports under the Kitty keyboard protocol.
/// Kitty uses the unshifted/lowercase form for text keys and C0 values for the
/// handful of named keys that have legacy control-byte encodings.
fn kitty_text_key_code(key: &keyboard::Key) -> Option<u32> {
    use keyboard::key::Named;
    use keyboard::Key;

    match key {
        Key::Character(s) => s.chars().next()?.to_lowercase().next().map(u32::from),
        Key::Named(Named::Escape) => Some(27),
        Key::Named(Named::Enter) => Some(13),
        Key::Named(Named::Tab) => Some(9),
        Key::Named(Named::Backspace) => Some(127),
        Key::Named(Named::Space) => Some(32),
        _ => None,
    }
}

/// Codepoint for the xterm modifyOtherKeys report; like [`kitty_text_key_code`]
/// but prefers iced's committed text, which carries the character the keyboard
/// layout actually produced. Shift is not the only way to reach the upper case
/// form — Caps Lock produces "A" with no modifier set at all — so the committed
/// text wins whenever no modifier rewrote the key into a control byte.
fn text_key_code(
    key: &keyboard::Key,
    mods: keyboard::Modifiers,
    text: Option<&str>,
) -> Option<u32> {
    let codepoint = kitty_text_key_code(key)?;
    if !(mods.control() || mods.alt() || mods.logo()) {
        if let Some(character) = text.and_then(|value| value.chars().find(|c| !c.is_control())) {
            return Some(character as u32);
        }
    }
    if mods.shift() {
        if let Some(character) = text.and_then(|value| value.chars().find(|c| !c.is_control())) {
            return Some(character as u32);
        }
        if let keyboard::Key::Character(s) = key {
            return s.chars().next()?.to_uppercase().next().map(u32::from);
        }
    }
    Some(codepoint)
}

/// The CSI-u / modifyOtherKeys modifier value: a bitfield + 1.
fn keyboard_modifier_value(mods: keyboard::Modifiers) -> u8 {
    let mut bits = 0u8;
    if mods.shift() {
        bits |= 0b1;
    }
    if mods.alt() {
        bits |= 0b10;
    }
    if mods.control() {
        bits |= 0b100;
    }
    if mods.logo() {
        bits |= 0b1000;
    }
    bits + 1
}

/// Encode a key press as a Kitty keyboard protocol report (`CSI codepoint;mod u`)
/// when the app has enabled disambiguation or report-all-keys. Returns `None`
/// when the protocol is inactive or the key needs no special report.
fn kitty_encode_key(
    key: &keyboard::Key,
    mods: keyboard::Modifiers,
    text: Option<&str>,
    kitty_flags: u16,
) -> Option<Vec<u8>> {
    let disambiguate = (kitty_flags & 0b1) != 0;
    let report_alternate_keys = (kitty_flags & 0b100) != 0;
    let report_all_keys = (kitty_flags & 0b1000) != 0;
    let report_associated_text = (kitty_flags & 0b10000) != 0;
    if !disambiguate && !report_all_keys {
        return None;
    }
    let codepoint = kitty_text_key_code(key)?;
    let legacy_c0_exception = matches!(
        key,
        keyboard::Key::Named(
            keyboard::key::Named::Enter
                | keyboard::key::Named::Tab
                | keyboard::key::Named::Backspace
        )
    );
    if legacy_c0_exception && !report_all_keys {
        return None;
    }
    let is_escape = matches!(key, keyboard::Key::Named(keyboard::key::Named::Escape));
    let should_encode = report_all_keys || is_escape || mods.control() || mods.alt() || mods.logo();
    if !should_encode {
        return None;
    }
    // The key code is always the unshifted key, so the shifted character has to
    // travel in the fields the app asked for: the alternate-key field (flag 4)
    // and/or the associated-text field (flag 16). Without them an app in
    // report-all-keys mode has to derive the case itself.
    let committed = text.filter(|t| !t.is_empty() && !t.chars().any(char::is_control));
    let mut key_field = codepoint.to_string();
    if report_alternate_keys && mods.shift() {
        if let Some(shifted) = committed
            .and_then(|t| t.chars().next())
            .map(u32::from)
            .filter(|shifted| *shifted != codepoint)
        {
            key_field = format!("{codepoint}:{shifted}");
        }
    }
    let mut sequence = format!("\x1b[{};{}", key_field, keyboard_modifier_value(mods));
    if report_associated_text {
        if let Some(t) = committed {
            let codepoints: Vec<String> = t.chars().map(|c| u32::from(c).to_string()).collect();
            sequence.push(';');
            sequence.push_str(&codepoints.join(":"));
        }
    }
    sequence.push('u');
    Some(sequence.into_bytes())
}

/// Encode a key press under xterm's modifyOtherKeys/formatOtherKeys regime.
fn xterm_modify_other_keys_encode(
    key: &keyboard::Key,
    mods: keyboard::Modifiers,
    text: Option<&str>,
    modify_other_keys: u16,
    format_other_keys: u16,
    report_all_keys: bool,
) -> Option<Vec<u8>> {
    let codepoint = text_key_code(key, mods, text)?;
    let modifier_value = keyboard_modifier_value(mods);
    let has_non_shift_modifier = mods.control() || mods.alt() || mods.logo();
    // xterm builds these reports for keys "modified by Control-, Alt- or
    // Meta-modifiers" — Shift is not one of them, because a shifted printable
    // key already produced its own character. Escaping Shift at level 2 turned
    // every capital letter into a CSI report the app had not asked for.
    let should_encode = if report_all_keys {
        true
    } else {
        match modify_other_keys {
            0 => false,
            1 => mods.alt() || mods.logo(),
            2 => has_non_shift_modifier,
            _ => true,
        }
    };
    if !should_encode {
        return None;
    }
    if format_other_keys == 1 || report_all_keys {
        Some(format!("\x1b[{};{}u", codepoint, modifier_value).into_bytes())
    } else {
        Some(format!("\x1b[27;{};{}~", modifier_value, codepoint).into_bytes())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use iced::keyboard::key::Named;

    #[test]
    fn block_badge_treats_wide_continuations_as_occupied() {
        let mut continuation = terminal::TerminalCell {
            character: '\0',
            ..terminal::TerminalCell::default()
        };
        continuation.flags.set_wide_continuation(true);
        let chars = [' ', Frost::block_badge_cell_char(&continuation)];
        assert!(!block_mode::badge_fits(&chars, 1));

        continuation.flags.set_wide_continuation(false);
        let chars = [' ', Frost::block_badge_cell_char(&continuation)];
        assert!(block_mode::badge_fits(&chars, 1));
    }

    #[test]
    fn held_transcript_blocks_user_input_and_marks_the_label() {
        // The dead-PTY input guard: only a held-open task transcript refuses
        // user bytes; ordinary sessions forward everything.
        assert!(user_input_blocked(true));
        assert!(!user_input_blocked(false));

        // The chrome affordance rides the same predicate.
        assert_eq!(
            session_label("codex".to_string(), true),
            format!("codex{READ_ONLY_LABEL_SUFFIX}")
        );
        assert_eq!(session_label("codex".to_string(), false), "codex");
    }

    #[test]
    fn block_key_ownership_preserves_running_program_input() {
        assert!(block_selection_owns_keys(true, true, false, false));
        assert!(!block_selection_owns_keys(true, true, false, true));
        assert!(!block_selection_owns_keys(true, true, true, false));
        assert!(!block_selection_owns_keys(false, true, false, false));

        // Escape is local only while an idle primary-screen block selection is
        // visible; running and alternate-screen programs retain ESC.
        assert!(block_escape_owns_key(true, true, false, false, true));
        assert!(!block_escape_owns_key(true, false, false, false, true));
        assert!(!block_escape_owns_key(true, true, false, false, false));
        assert!(!block_escape_owns_key(true, true, false, true, true));
        assert!(!block_escape_owns_key(true, true, true, false, true));
    }

    #[test]
    fn terminal_mouse_routing_prefers_finished_history_over_live_app_mode() {
        // Primary-screen app reporting owns its live surface.
        assert!(app_owns_terminal_mouse(true, false, true, false));
        // Finished or pre-zone scrollback, padding and scrollbar strips all
        // arrive ineligible and cannot be inferred from `!finalized`.
        assert!(!app_owns_terminal_mouse(true, false, false, false));
        // Shift and Ctrl+link remain local overrides on the live surface too.
        assert!(!app_owns_terminal_mouse(true, true, true, false));
        assert!(!app_owns_terminal_mouse(true, false, true, true));
        assert!(!app_owns_terminal_mouse(false, false, true, false));

        assert!(app_owns_terminal_wheel(true, false, true));
        assert!(!app_owns_terminal_wheel(true, false, false));
        assert!(!app_owns_terminal_wheel(true, true, true));
        assert!(!app_owns_terminal_wheel(false, false, true));

        assert!(!app_mouse_uses_full_grid(true, false, true));
        assert!(app_mouse_uses_full_grid(true, false, false));
        assert!(app_mouse_uses_full_grid(true, true, true));
        assert!(app_mouse_uses_full_grid(false, false, true));
    }

    #[test]
    fn projected_prompt_navigation_chooses_the_nearest_strict_boundary() {
        let prompts = [2, 8, 15, 23];
        assert_eq!(prompt_jump_target(prompts.into_iter(), 15, true), Some(8));
        assert_eq!(prompt_jump_target(prompts.into_iter(), 15, false), Some(23));
        assert_eq!(prompt_jump_target(prompts.into_iter(), 2, true), None);
        assert_eq!(prompt_jump_target(prompts.into_iter(), 23, false), None);
    }

    #[test]
    fn search_matches_use_exact_projected_origins_and_fail_closed() {
        let mut terminal = terminal::TerminalState::new(4, 3);
        let mut raw = vec![terminal::TerminalCell::default(); 8];
        for (cell, ch) in raw.iter_mut().zip("abcdef".chars()) {
            cell.character = ch;
        }
        terminal
            .scrollback
            .push_back(terminal::ScrollbackLine::compress(&raw, false));
        terminal.set_scroll_offset(1);
        let projection = terminal.get_projected_viewport(true);

        assert_eq!(
            project_search_match(
                &terminal,
                &projection,
                &search::SearchMatch {
                    line: 0,
                    col_start: 4,
                    col_end: 6,
                },
            ),
            Some(search::SearchMatch {
                line: 0,
                col_start: 0,
                col_end: 2,
            })
        );
        assert_eq!(
            project_search_match(
                &terminal,
                &projection,
                &search::SearchMatch {
                    line: 0,
                    col_start: 5,
                    col_end: 8,
                },
            ),
            None,
            "trimmed trailing padding must not inherit the nearest origin"
        );
    }

    #[test]
    fn moving_from_hidden_search_match_clears_only_location_diagnostic() {
        let mut hidden =
            Some("Match is hidden in collapsed block #7; expand its output to reveal".to_string());
        clear_stale_hidden_match_diagnostic(&mut hidden);
        assert_eq!(hidden, None);

        let mut regex_error = Some("invalid regular expression".to_string());
        clear_stale_hidden_match_diagnostic(&mut regex_error);
        assert_eq!(regex_error.as_deref(), Some("invalid regular expression"));
    }

    #[test]
    fn collapsed_summary_target_is_stable_and_kitty_is_all_or_nothing() {
        let mut terminal = terminal::TerminalState::new(24, 8);
        terminal.process_input(
            b"\x1b]133;A\x07$ \x1b]133;B\x07cmd\r\n\x1b]133;C\x07out\r\nmore\r\n\x1b]133;D;0\x07tail",
        );
        let zone = terminal.command_zones.back().expect("finished block");
        let zone_id = zone.id;
        let output_start = zone.output_start.expect("retained output start");
        let output_col = zone.output_start_col;

        let identity = terminal.get_projected_viewport(true);
        assert!(
            projected_kitty_anchor(&terminal, &identity, output_start, output_col, 1, 2).is_some()
        );

        let mut policy = terminal::ProjectionPolicy::new();
        assert!(policy.collapse(zone_id));
        let mut view_state = terminal::ProjectionViewState::new();
        let collapsed = terminal.get_projected_viewport_with_state(true, &policy, &mut view_state);
        let (summary_row, summary_key) = collapsed
            .row_kinds()
            .iter()
            .enumerate()
            .find_map(|(row, kind)| match kind {
                terminal::ProjectedRowKind::CollapsedSummary { key, .. } => Some((row, *key)),
                terminal::ProjectedRowKind::Raw | terminal::ProjectedRowKind::Padding => None,
            })
            .expect("visible collapsed summary");
        let activation = SummaryActivation {
            key: summary_key,
            projection_key: collapsed.key(),
        };
        assert_eq!(
            validated_summary_target(&collapsed, &activation),
            Some(zone_id)
        );
        assert_eq!(
            finalized_block_at_viewport_row(true, &terminal, &collapsed, summary_row),
            Some(zone_id),
            "summary retains the right-click block target"
        );
        assert_eq!(
            projected_kitty_anchor(&terminal, &collapsed, output_start, output_col, 1, 2),
            None,
            "a placement touching hidden output must disappear as a whole"
        );

        // A placement whose anchor survives on a shared header/output row is
        // still hidden when its horizontal extent crosses the collapsed
        // suffix. Checking only the anchor would incorrectly draw the image.
        let mut same_row = terminal::TerminalState::new(32, 4);
        same_row.process_input(
            b"\x1b]133;A\x07$ \x1b]133;B\x07cmd\x1b]133;C\x07out\x1b]133;D;0\x07 tail",
        );
        let same_zone = same_row.command_zones.back().expect("same-row block");
        let same_zone_id = same_zone.id;
        let same_output_row = same_zone.output_start.expect("same-row output");
        let same_output_col = same_zone.output_start_col;
        assert!(same_output_col > 0);
        let same_identity = same_row.get_projected_viewport(true);
        assert!(projected_kitty_anchor(
            &same_row,
            &same_identity,
            same_output_row,
            same_output_col - 1,
            2,
            1,
        )
        .is_some());
        let mut same_policy = terminal::ProjectionPolicy::new();
        assert!(same_policy.collapse(same_zone_id));
        let mut same_state = terminal::ProjectionViewState::new();
        let same_collapsed =
            same_row.get_projected_viewport_with_state(true, &same_policy, &mut same_state);
        assert_eq!(
            projected_kitty_anchor(
                &same_row,
                &same_collapsed,
                same_output_row,
                same_output_col - 1,
                2,
                1,
            ),
            None,
            "horizontal overlap with a hidden suffix hides the whole placement"
        );

        let mut stale = activation.clone();
        stale.projection_key.scroll_offset = stale.projection_key.scroll_offset.saturating_add(1);
        assert_eq!(validated_summary_target(&collapsed, &stale), None);
        let mut wrong_row = activation;
        wrong_row.key.zone_id = wrong_row.key.zone_id.saturating_add(1);
        assert_eq!(validated_summary_target(&collapsed, &wrong_row), None);
    }

    #[test]
    fn projected_zone_sweep_is_linear_and_keeps_padding_unowned() {
        let view_rows: Vec<_> = (0..512).map(|row| (row != 257).then_some(row)).collect();
        let spans: Vec<_> = (0..256).map(|zone| (zone * 2, zone * 2 + 2)).collect();
        let memberships = projected_zone_memberships(&view_rows, &spans);

        assert!(memberships.scan_steps <= view_rows.len() + spans.len());
        assert_eq!(memberships.rows[0], Some((0, 0)));
        assert_eq!(memberships.rows[256], Some((128, 256)));
        assert_eq!(memberships.rows[257], None);
        assert_eq!(memberships.rows[511], Some((255, 511)));
    }

    #[test]
    fn collapsed_summary_is_real_top_only_for_headerless_background() {
        assert_eq!(
            projected_card_real_top(None, Some(3), block_mode::BlockOutcome::Success,),
            None,
            "an offscreen command header remains a clipped card edge"
        );
        assert_eq!(
            projected_card_real_top(None, Some(3), block_mode::BlockOutcome::Background,),
            Some(3),
            "a Background summary replaces the entire headerless card"
        );
        assert_eq!(
            projected_card_real_top(Some(1), Some(3), block_mode::BlockOutcome::Background,),
            Some(1),
            "a visible real top always wins"
        );
    }

    #[test]
    fn empty_history_row_retains_finished_block_target_without_cell_origin() {
        let mut terminal = terminal::TerminalState::new(8, 3);
        let mut first = vec![terminal::TerminalCell::default(); 8];
        first[0].character = 'A';
        let empty = vec![terminal::TerminalCell::default(); 8];
        let mut last = vec![terminal::TerminalCell::default(); 8];
        last[0].character = 'Z';
        terminal
            .scrollback
            .push_back(terminal::ScrollbackLine::compress(&first, false));
        terminal
            .scrollback
            .push_back(terminal::ScrollbackLine::compress(&empty, false));
        terminal
            .scrollback
            .push_back(terminal::ScrollbackLine::compress(&last, false));
        terminal.command_zones.push_back(terminal::CommandZone {
            id: 41,
            prompt_start: 0,
            command_start: Some(0),
            output_start: Some(0),
            output_start_col: 0,
            output_end: Some(2),
            exit_code: Some(0),
            command: Some("fixture".into()),
            duration_ms: None,
            finished_at_ms: None,
            command_truncated: false,
            command_exact: false,
            cwd: None,
            captured_output: None,
            captured_output_evicted: false,
            completion_observed: true,
            rows_evicted: false,
        });
        terminal.set_scroll_offset(terminal.scrollback_len());
        let projection = terminal.get_projected_viewport(true);
        let blank_view_row = (0..projection.cells().len())
            .find(|row| projection.view_row_absolute(*row) == Some(1))
            .expect("pure empty history row remains projected");

        assert_eq!(
            projection.view_to_raw(terminal::ViewportCell {
                row: blank_view_row,
                col: 0,
            }),
            None,
            "row ownership must not manufacture a selectable cell"
        );
        assert_eq!(
            finalized_block_at_viewport_row(true, &terminal, &projection, blank_view_row),
            Some(41),
            "stripe/card and right-click target retain the finished zone"
        );
        assert_eq!(
            validated_claimed_block_target(Some(41), Some(41), true),
            Some(41)
        );
    }

    #[test]
    fn claimed_block_press_does_not_retarget_after_pty_viewport_shift() {
        let mut terminal = terminal::TerminalState::new(24, 4);
        for index in 0..4 {
            let lifecycle = format!(
                "\x1b]133;A\x07$ \x1b]133;B\x07cmd{index}\r\n\x1b]133;C\x07out{index}\r\n\x1b]133;D;0\x07"
            );
            terminal.process_input(lifecycle.as_bytes());
        }
        terminal.process_input(b"\x1b]133;A\x07$ \x1b]133;B\x07");

        // Find one physical viewport row that names different finalized zones
        // at two real scroll offsets. This models the render→update race: the
        // press carries the first id while PTY/scroll activity changes the
        // current row mapping before dispatch.
        let rows = terminal.get_dimensions().1;
        let mut mappings = Vec::new();
        for offset in 0..=terminal.scrollback_len() {
            terminal.set_scroll_offset(offset);
            let projection = terminal.get_projected_viewport(true);
            for row in 0..rows {
                if let Some(id) = finalized_block_at_viewport_row(true, &terminal, &projection, row)
                {
                    mappings.push((offset, row, id));
                }
            }
        }
        let ((render_offset, row, rendered_id), (dispatch_offset, _, current_id)) = mappings
            .iter()
            .copied()
            .enumerate()
            .find_map(|(index, rendered)| {
                mappings[index + 1..]
                    .iter()
                    .copied()
                    .find(|current| current.1 == rendered.1 && current.2 != rendered.2)
                    .map(|current| (rendered, current))
            })
            .expect("fixture must shift one viewport row onto a neighbouring block");

        terminal.set_scroll_offset(render_offset);
        let render_projection = terminal.get_projected_viewport(true);
        assert_eq!(
            finalized_block_at_viewport_row(true, &terminal, &render_projection, row),
            Some(rendered_id)
        );
        terminal.set_scroll_offset(dispatch_offset);
        let dispatch_projection = terminal.get_projected_viewport(true);
        assert_eq!(
            finalized_block_at_viewport_row(true, &terminal, &dispatch_projection, row),
            Some(current_id)
        );
        assert!(terminal.zone_by_id(rendered_id).is_some());
        assert_eq!(
            validated_claimed_block_target(Some(rendered_id), Some(current_id), true),
            None,
            "the neighbour under the shifted viewport must never replace the painted id"
        );
        assert_eq!(
            validated_claimed_block_target(Some(rendered_id), Some(rendered_id), true),
            Some(rendered_id)
        );
        assert_eq!(
            validated_claimed_block_target(Some(rendered_id), Some(rendered_id), false),
            None,
            "an evicted/non-finalized painted target fails closed"
        );
    }

    #[test]
    fn stale_or_exhausted_link_projection_revision_fails_closed() {
        assert!(link_projection_matches(41, 41));
        assert!(!link_projection_matches(41, 42));
        assert!(!link_projection_matches(0, 0));
        assert!(!link_projection_matches(0, 41));
    }

    #[test]
    fn terminal_mouse_gestures_are_independent_per_button() {
        let mut gestures = [None; 3];
        for button in [MouseButton::Left, MouseButton::Right, MouseButton::Middle] {
            gestures[button.slot()] = Some(TerminalMouseGesture {
                session_id: 7,
                button,
                report_to_app: true,
                consumed: false,
            });
        }
        for button in [MouseButton::Right, MouseButton::Left, MouseButton::Middle] {
            let gesture = gestures[button.slot()].take().expect("owned release");
            assert_eq!(gesture.button, button);
        }
        assert!(gestures.iter().all(Option::is_none));
    }

    #[test]
    fn overlay_only_dispatches_origin_app_release() {
        let app_left = TerminalMouseGesture {
            session_id: 7,
            button: MouseButton::Left,
            report_to_app: true,
            consumed: false,
        };
        let local_right = TerminalMouseGesture {
            session_id: 8,
            button: MouseButton::Right,
            report_to_app: false,
            consumed: false,
        };
        let consumed_middle = TerminalMouseGesture {
            session_id: 8,
            button: MouseButton::Middle,
            report_to_app: false,
            consumed: true,
        };

        assert_eq!(
            overlay_release_disposition(Some(app_left), 7, MouseButton::Left),
            OverlayReleaseDisposition::DispatchApp
        );
        assert_eq!(
            overlay_release_disposition(Some(app_left), 8, MouseButton::Left),
            OverlayReleaseDisposition::Reject,
            "a focused pane cannot steal another pane's button-up"
        );
        assert_eq!(
            overlay_release_disposition(Some(local_right), 8, MouseButton::Right),
            OverlayReleaseDisposition::ClearOnly,
            "local release behind an overlay must not copy or move the caret"
        );
        assert_eq!(
            overlay_release_disposition(Some(consumed_middle), 8, MouseButton::Middle),
            OverlayReleaseDisposition::ClearOnly
        );
    }

    #[test]
    fn ctrl_link_requires_an_unshifted_single_left_press() {
        for (name, over_grid, finalized, header, expected) in [
            ("finished header", true, true, true, false),
            ("finished output", true, true, false, true),
            ("active/live", true, false, false, true),
            ("alt/non-Block", true, false, false, true),
            ("pre-zone history", true, false, false, true),
            ("padding/scrollbar", false, false, false, false),
        ] {
            assert_eq!(
                terminal_view::link_surface_eligible(over_grid, finalized, header),
                expected,
                "{name}"
            );
        }

        assert!(terminal_view::ctrl_link_eligible(
            MouseButton::Left,
            1,
            true,
            false,
            true,
        ));
        assert!(!terminal_view::ctrl_link_eligible(
            MouseButton::Left,
            1,
            true,
            true,
            true,
        ));
        assert!(!terminal_view::ctrl_link_eligible(
            MouseButton::Left,
            2,
            true,
            false,
            true,
        ));
        assert!(!terminal_view::ctrl_link_eligible(
            MouseButton::Left,
            3,
            true,
            false,
            true,
        ));
        assert!(!terminal_view::ctrl_link_eligible(
            MouseButton::Right,
            1,
            true,
            false,
            true,
        ));
        assert!(!terminal_view::ctrl_link_eligible(
            MouseButton::Left,
            1,
            false,
            false,
            true,
        ));
        assert!(!terminal_view::ctrl_link_eligible(
            MouseButton::Left,
            1,
            true,
            false,
            false, // finished command/header row
        ));
    }

    #[test]
    fn unknown_ai_block_status_is_not_presented_as_success() {
        assert_eq!(agent_context_exit_label(-1), "no reported exit status");
        assert_eq!(agent_context_exit_label(0), "exit 0");
        assert_eq!(agent_context_exit_label(7), "exit 7");
    }

    #[test]
    fn bookmark_toggle_has_no_implicit_latest_target() {
        let ids = [10, 20, 30];
        assert_eq!(active_bookmark_target(&ids, Some(20)), Some(20));
        assert_eq!(active_bookmark_target(&ids, None), None);
        assert_eq!(active_bookmark_target(&ids, Some(99)), None);
    }

    #[test]
    fn ai_block_output_reports_secondary_context_truncation() {
        let short = "one\ntwo";
        assert_eq!(
            bounded_ai_block_output(short, false),
            (short.to_string(), false)
        );
        let (unknown, truncated) = bounded_ai_block_output(short, true);
        assert!(!truncated);
        assert_eq!(unknown, format!("{NO_REPORTED_EXIT_STATUS_NOTE}\n{short}"));

        let long = (0..200)
            .map(|line| format!("line {line}"))
            .collect::<Vec<_>>()
            .join("\n");
        let (bounded, truncated) = bounded_ai_block_output(&long, true);
        assert!(truncated);
        assert_ne!(bounded, long);
        assert!(bounded.starts_with(NO_REPORTED_EXIT_STATUS_NOTE));
        assert!(bounded.contains("line 0"));
        assert!(bounded.contains("line 199"));
    }

    #[test]
    fn pointer_anchored_block_menu_flips_and_clamps_inside_window() {
        let window = iced::Size::new(1000.0, 700.0);
        let panel = iced::Size::new(360.0, 500.0);
        assert_eq!(
            anchored_overlay_position(iced::Point::new(100.0, 80.0), window, panel, TAB_BAR_H),
            iced::Point::new(106.0, 86.0)
        );
        // Bottom-right summons above the pointer and clamps away from the edge.
        assert_eq!(
            anchored_overlay_position(iced::Point::new(990.0, 690.0), window, panel, TAB_BAR_H),
            iced::Point::new(634.0, 184.0)
        );
        // Corrupt/non-finite pointer state stays reachable instead of
        // propagating NaNs into iced layout.
        assert_eq!(
            anchored_overlay_position(
                iced::Point::new(f32::NAN, f32::NAN),
                window,
                panel,
                TAB_BAR_H,
            ),
            iced::Point::new(6.0, 36.0)
        );
    }

    #[test]
    fn block_menu_batch_commands_do_not_depend_on_clicked_background() {
        let mut selection = block_mode::BlockSelection::default();
        selection.select_all(&[10, 20]);
        let summary =
            block_menu_selection_summary([(10, Some("printf ok")), (20, None)], &selection, 20);
        assert_eq!(summary.selected_count, 2);
        assert!(summary.has_selected_commands);
        assert!(!summary.clicked_has_command);
    }

    #[test]
    fn unavailable_block_bindings_are_classified_for_pty_passthrough() {
        use keybindings::Command as C;

        for command in [
            C::BlockSearch,
            C::BlockToggleBookmark,
            C::BlockJumpPrevBookmark,
            C::BlockCopyOutput,
            C::BlockClear,
            C::BlockReinputSelectedCommands,
            C::TerminalPromptPrev,
            C::TerminalCopyLastOutput,
        ] {
            assert!(command_requires_block_context(&command), "{command}");
        }
        for command in [C::EditCopy, C::TerminalClear, C::PaneFocusLeft] {
            assert!(!command_requires_block_context(&command), "{command}");
        }
    }

    #[test]
    fn block_clear_confirmation_is_counted_and_fails_closed() {
        assert_eq!(BlockClearConfirmation::new(7, 0, None), None);
        assert_eq!(BlockClearConfirmation::new(7, 3, None), None);
        let confirm = BlockClearConfirmation::new(7, 3, Some(30)).expect("non-empty confirmation");
        assert_eq!(confirm.block_count, 3);
        assert_eq!(
            confirm.resolve(Some((7, 3, Some(30)))),
            BlockClearResolution::Clear
        );
        assert_eq!(confirm.resolve(None), BlockClearResolution::Stale);
        assert_eq!(
            confirm.resolve(Some((8, 3, Some(30)))),
            BlockClearResolution::Stale
        );
        assert_eq!(
            confirm.resolve(Some((7, 0, None))),
            BlockClearResolution::Empty
        );
    }

    #[test]
    fn block_clear_confirmation_rearms_when_the_live_history_changes() {
        let confirm = BlockClearConfirmation::new(11, 2, Some(20)).expect("non-empty confirmation");
        let updated = BlockClearConfirmation::new(11, 4, Some(40)).expect("updated confirmation");
        assert_eq!(
            confirm.resolve(Some((11, 4, Some(40)))),
            BlockClearResolution::Refresh(updated)
        );
        assert_eq!(
            updated.resolve(Some((11, 4, Some(40)))),
            BlockClearResolution::Clear
        );

        // A full bounded history may replace its oldest block without changing
        // the count; the monotonic newest id still forces a fresh review.
        let same_count_new_block =
            BlockClearConfirmation::new(11, 2, Some(30)).expect("replacement confirmation");
        assert_eq!(
            confirm.resolve(Some((11, 2, Some(30)))),
            BlockClearResolution::Refresh(same_count_new_block)
        );
    }

    #[test]
    fn enter_reinput_requires_a_trusted_empty_prompt() {
        use terminal::AgentPromptStatus;

        assert!(block_enter_reinputs_selection(
            true,
            AgentPromptStatus::Ready
        ));
        for status in [
            AgentPromptStatus::Busy,
            AgentPromptStatus::InputNotEmpty,
            AgentPromptStatus::UnsafeCommand,
            AgentPromptStatus::ShellIntegrationUnavailable,
        ] {
            assert!(!block_enter_reinputs_selection(true, status));
        }
        assert!(!block_enter_reinputs_selection(
            false,
            AgentPromptStatus::Ready
        ));
    }

    #[test]
    fn ctrl_arrows_preserve_scroll_and_own_selection_edges() {
        use block_mode::SelectionNavigation::{Clear, Passthrough, Select};

        let ids = [10, 20, 30];
        assert_eq!(
            ctrl_scroll_block_navigation(true, true, false, false, &ids, None),
            Select(30)
        );
        assert_eq!(
            ctrl_scroll_block_navigation(true, false, false, false, &ids, None),
            Passthrough
        );
        assert_eq!(
            ctrl_scroll_block_navigation(true, false, false, false, &ids, Some(20)),
            Select(30)
        );
        // Selection edges are owned: Up clamps, Down exits selection mode.
        assert_eq!(
            ctrl_scroll_block_navigation(true, true, false, false, &ids, Some(10)),
            Select(10)
        );
        assert_eq!(
            ctrl_scroll_block_navigation(true, false, false, false, &ids, Some(30)),
            Clear
        );
        assert_eq!(
            ctrl_scroll_block_navigation(true, true, false, false, &[], None),
            Passthrough
        );
        // Running/full-screen programs retain Ctrl+Up too.
        assert_eq!(
            ctrl_scroll_block_navigation(true, true, false, true, &ids, None),
            Passthrough
        );
        assert_eq!(
            ctrl_scroll_block_navigation(true, true, true, false, &ids, None),
            Passthrough
        );
    }

    #[test]
    fn selected_command_reinput_never_executes_unbracketed_later_lines() {
        let bracketed = pty_input::encode_prompt_insert(
            "first\nsecond",
            PasteModes { bracketed: true },
            block_reinput_policy(),
            true,
        );
        assert_eq!(bracketed.bytes, b"\x15\x1b[200~first\nsecond\x1b[201~");
        assert!(!bracketed.bytes.ends_with(b"\r"));

        let plain = pty_input::encode_prompt_insert(
            "first\nsecond",
            PasteModes { bracketed: false },
            block_reinput_policy(),
            true,
        );
        assert_eq!(plain.bytes, b"\x15first");
        assert_eq!(plain.echo_text, "first");
        assert!(plain.risk.truncated_to_first_line);
        assert!(!plain.bytes.contains(&b'\n'));
        assert!(!plain.bytes.contains(&b'\r'));
    }

    #[test]
    fn block_prompt_replacement_requires_a_trusted_empty_shell_prompt() {
        use terminal::AgentPromptStatus;

        assert_eq!(block_prompt_replace_blocker(AgentPromptStatus::Ready), None);
        assert_eq!(
            block_prompt_replace_blocker(AgentPromptStatus::Busy),
            Some("the terminal is busy")
        );
        assert_eq!(
            block_prompt_replace_blocker(AgentPromptStatus::InputNotEmpty),
            Some("the prompt already contains input")
        );
        assert_eq!(
            block_prompt_replace_blocker(AgentPromptStatus::ShellIntegrationUnavailable),
            Some("waiting for an empty OSC 133 shell prompt")
        );
    }

    #[test]
    fn agent_prompt_requires_the_shell_to_own_the_foreground_pty() {
        use terminal::AgentPromptStatus;

        assert_eq!(
            agent_prompt_status_with_foreground(AgentPromptStatus::Ready, 100, Some(100)),
            AgentPromptStatus::Ready
        );
        assert_eq!(
            agent_prompt_status_with_foreground(AgentPromptStatus::Ready, 100, Some(200)),
            AgentPromptStatus::Busy
        );
        assert_eq!(
            agent_prompt_status_with_foreground(AgentPromptStatus::Ready, 100, None),
            AgentPromptStatus::ShellIntegrationUnavailable
        );
        // Terminal-level blockers always win, even if the shell is foreground.
        assert_eq!(
            agent_prompt_status_with_foreground(AgentPromptStatus::InputNotEmpty, 100, Some(100)),
            AgentPromptStatus::InputNotEmpty
        );
    }

    #[test]
    fn ui_scale_change_resizes_logical_viewport_and_terminal_grid() {
        let old_viewport = Size::new(1200.0, 800.0);
        let old_scale = 1.0;
        let new_scale = 2.0;
        let new_viewport = logical_viewport_after_scale(old_viewport, old_scale, new_scale);

        assert_eq!(new_viewport, Size::new(600.0, 400.0));
        assert_eq!(
            new_viewport.width * new_scale,
            old_viewport.width * old_scale
        );
        assert_eq!(
            new_viewport.height * new_scale,
            old_viewport.height * old_scale
        );

        let metrics = Metrics::new(10.0, 1.0, 0.0, false);
        let old_grid = metrics.grid_size(
            old_viewport.width - terminal_view::SCROLLBAR_WIDTH,
            old_viewport.height - chrome_height(true),
        );
        let new_grid = metrics.grid_size(
            new_viewport.width - terminal_view::SCROLLBAR_WIDTH,
            new_viewport.height - chrome_height(true),
        );
        assert!(new_grid.0 < old_grid.0);
        assert!(new_grid.1 < old_grid.1);
        assert_eq!(new_grid, (98, 29));
    }

    #[test]
    fn disabling_the_bottom_bar_returns_its_rows_to_the_grid() {
        let metrics = Metrics::new(10.0, 1.0, 0.0, false);
        let viewport = Size::new(1200.0, 800.0);
        let width = viewport.width - terminal_view::SCROLLBAR_WIDTH;
        let with_bar = metrics.grid_size(width, viewport.height - chrome_height(true));
        let without_bar = metrics.grid_size(width, viewport.height - chrome_height(false));
        assert_eq!(with_bar.0, without_bar.0);
        assert!(without_bar.1 > with_bar.1);
    }

    #[test]
    fn app_chrome_shortcuts_keep_palette_help_switcher_and_f12_contract() {
        let ctrl_shift = keyboard::Modifiers::CTRL | keyboard::Modifiers::SHIFT;
        let character = |s: &str| keyboard::Key::Character(s.into());

        assert_eq!(
            chrome_shortcut(&character("p"), ctrl_shift),
            Some(ChromeShortcut::CommandPalette)
        );
        assert_eq!(
            chrome_shortcut(&character("/"), ctrl_shift),
            Some(ChromeShortcut::Help)
        );
        assert_eq!(
            chrome_shortcut(&character("l"), ctrl_shift),
            Some(ChromeShortcut::TabSwitcher)
        );
        assert_eq!(
            chrome_shortcut(&character("h"), ctrl_shift),
            Some(ChromeShortcut::HistoryPicker)
        );
        assert_eq!(
            chrome_shortcut(&keyboard::Key::Named(Named::F12), keyboard::Modifiers::NONE),
            Some(ChromeShortcut::Debug)
        );

        assert_eq!(chrome_shortcut(&character("g"), ctrl_shift), None);
        assert_eq!(chrome_shortcut(&character("k"), ctrl_shift), None);
        assert_eq!(
            chrome_shortcut(&character("p"), keyboard::Modifiers::CTRL),
            None
        );
    }

    #[test]
    fn physical_key_events_match_sidebar_focus_and_resize_binding_names() {
        assert_eq!(
            key_to_binding_string(
                &keyboard::Key::Character("\\".into()),
                keyboard::Modifiers::CTRL
            )
            .as_deref(),
            Some("ctrl+backslash")
        );

        let focus_mods = keyboard::Modifiers::CTRL | keyboard::Modifiers::ALT;
        let resize_mods = focus_mods | keyboard::Modifiers::SHIFT;
        let cases = [
            (Named::ArrowLeft, focus_mods, "ctrl+alt+left"),
            (Named::ArrowRight, focus_mods, "ctrl+alt+right"),
            (Named::ArrowUp, focus_mods, "ctrl+alt+up"),
            (Named::ArrowDown, focus_mods, "ctrl+alt+down"),
            (Named::ArrowLeft, resize_mods, "ctrl+shift+alt+left"),
            (Named::ArrowRight, resize_mods, "ctrl+shift+alt+right"),
            (Named::ArrowUp, resize_mods, "ctrl+shift+alt+up"),
            (Named::ArrowDown, resize_mods, "ctrl+shift+alt+down"),
        ];
        for (named, modifiers, expected) in cases {
            assert_eq!(
                key_to_binding_string(&keyboard::Key::Named(named), modifiers).as_deref(),
                Some(expected),
                "{named:?}"
            );
        }
    }

    /// Regression: the runtime mapper used `to_ascii_lowercase` while the
    /// config side folded with Unicode `to_lowercase`, so a non-ASCII chord
    /// canonicalized differently depending on which side produced it. Both
    /// paths now go through `jterm_core::keybindings`, and must agree.
    #[test]
    fn runtime_and_config_paths_agree_on_non_ascii_case_folding() {
        // The keyboard delivers the shifted/uppercase 'Ю'; the config file
        // says "ctrl+ю". Identical canonical form or the binding never fires.
        let runtime = key_to_binding_string(
            &keyboard::Key::Character("Ю".into()),
            keyboard::Modifiers::CTRL,
        );
        assert_eq!(runtime.as_deref(), Some("ctrl+ю"));
        assert_eq!(runtime, keybindings::KeyBinding::canonical("ctrl+ю"));
        assert_eq!(runtime, keybindings::KeyBinding::canonical("Ctrl+Ю"));

        // 'ß' has no single-char uppercase; both sides store it verbatim.
        let runtime = key_to_binding_string(
            &keyboard::Key::Character("ß".into()),
            keyboard::Modifiers::CTRL,
        );
        assert_eq!(runtime.as_deref(), Some("ctrl+ß"));
        assert_eq!(runtime, keybindings::KeyBinding::canonical("ctrl+ß"));

        // End to end: a non-ASCII user binding is found from the key event.
        let loaded =
            keybindings::KeyBindings::from_toml_with_diagnostics("\"ctrl+ю\" = \"session:new\"\n")
                .expect("valid TOML");
        assert!(loaded.diagnostics.is_empty());
        let binding = key_to_binding_string(
            &keyboard::Key::Character("Ю".into()),
            keyboard::Modifiers::CTRL,
        )
        .expect("chord expected");
        assert_eq!(
            loaded.bindings.get_command(&binding),
            Some(keybindings::Command::SessionNew)
        );
    }

    #[test]
    fn alt_derives_output_copy_from_a_remapped_copy_unless_explicitly_overridden() {
        let key = keyboard::Key::Character("x".into());
        let alt_copy_mods =
            keyboard::Modifiers::CTRL | keyboard::Modifiers::SHIFT | keyboard::Modifiers::ALT;
        let remapped = keybindings::KeyBindings::from_toml_with_diagnostics(
            "\"ctrl+shift+x\" = \"edit:copy\"\n",
        )
        .expect("valid remap")
        .bindings;
        assert_eq!(
            resolve_keybinding_command(&remapped, &key, alt_copy_mods),
            Some(keybindings::Command::EditCopyBlockOutput)
        );

        let overridden = keybindings::KeyBindings::from_toml_with_diagnostics(
            "\"ctrl+shift+x\" = \"edit:copy\"\n\"ctrl+alt+shift+x\" = \"terminal:send_eof\"\n",
        )
        .expect("valid explicit override")
        .bindings;
        assert_eq!(
            resolve_keybinding_command(&overridden, &key, alt_copy_mods),
            Some(keybindings::Command::TerminalSendEof)
        );
    }

    #[test]
    fn pane_tree_snapshot_round_trips() {
        let tree = PaneTree::Split {
            axis: Axis::Vertical,
            children: vec![
                PaneTree::Leaf(0),
                PaneTree::Split {
                    axis: Axis::Horizontal,
                    children: vec![PaneTree::Leaf(1), PaneTree::Leaf(2)],
                    ratios: vec![0.5, 0.5],
                },
            ],
            ratios: vec![0.6, 0.4],
        };
        let snap = pane_tree_to_snapshot(&tree);
        let back = pane_tree_from_snapshot(&snap).unwrap();
        assert_eq!(back, tree);
        assert!(valid_restored_layout(&back, 3));
        // Out-of-range session indices are rejected.
        assert!(!valid_restored_layout(&back, 2));
    }

    fn split_tab(id: usize, sessions: &[usize]) -> Tab {
        Tab {
            id,
            title: None,
            pinned: false,
            marked: false,
            private_title: false,
            tree: PaneTree::Split {
                axis: Axis::Vertical,
                children: sessions.iter().map(|&s| PaneTree::Leaf(s)).collect(),
                ratios: vec![1.0 / sessions.len() as f32; sessions.len()],
            },
            focus: sessions[0],
        }
    }

    #[test]
    fn tab_split_drop_zones_choose_the_nearest_edge_and_keep_a_dead_center() {
        let rect = pane_layout::Rect {
            x: 10.0,
            y: 20.0,
            width: 100.0,
            height: 80.0,
        };
        assert_eq!(
            split_drop_direction(rect, iced::Point::new(11.0, 60.0)),
            Some(PaneDirection::Left)
        );
        assert_eq!(
            split_drop_direction(rect, iced::Point::new(109.0, 60.0)),
            Some(PaneDirection::Right)
        );
        assert_eq!(
            split_drop_direction(rect, iced::Point::new(60.0, 21.0)),
            Some(PaneDirection::Up)
        );
        assert_eq!(
            split_drop_direction(rect, iced::Point::new(60.0, 99.0)),
            Some(PaneDirection::Down)
        );
        assert_eq!(
            split_drop_direction(rect, iced::Point::new(60.0, 60.0)),
            None
        );
        assert_eq!(
            split_drop_direction(rect, iced::Point::new(110.0, 60.0)),
            None
        );
        assert_eq!(
            split_drop_direction(rect, iced::Point::new(f32::NAN, 60.0)),
            None
        );
    }

    #[test]
    fn tab_split_commit_revalidates_visible_target_zoom_and_capacity() {
        assert!(tab_split_commit_allowed(
            Some(0),
            true,
            1,
            Some(1),
            2,
            false
        ));
        assert!(!tab_split_commit_allowed(
            Some(0),
            true,
            1,
            Some(2),
            2,
            false
        ));
        assert!(!tab_split_commit_allowed(
            Some(0),
            true,
            1,
            Some(1),
            2,
            true
        ));
        assert!(!tab_split_commit_allowed(
            Some(0),
            true,
            1,
            Some(1),
            MAX_PANES,
            false
        ));
        assert!(!tab_split_commit_allowed(
            Some(0),
            false,
            1,
            Some(1),
            2,
            false
        ));
    }

    #[test]
    fn drag_hover_switch_requires_the_same_target_for_the_full_delay() {
        let almost = std::time::Duration::from_millis(TAB_DRAG_HOVER_SWITCH_MS - 1);
        let ready = std::time::Duration::from_millis(TAB_DRAG_HOVER_SWITCH_MS);
        assert!(!tab_drag_hover_ready(
            Some(1),
            true,
            Some(3),
            Some(2),
            2,
            almost
        ));
        assert!(tab_drag_hover_ready(
            Some(1),
            true,
            Some(3),
            Some(2),
            2,
            ready
        ));
        assert!(!tab_drag_hover_ready(
            Some(1),
            true,
            Some(3),
            Some(4),
            2,
            ready
        ));
        assert!(!tab_drag_hover_ready(
            Some(2),
            true,
            Some(3),
            Some(2),
            2,
            ready
        ));
        assert!(!tab_drag_hover_ready(
            None,
            true,
            Some(3),
            Some(2),
            2,
            ready
        ));
        assert!(!tab_drag_hover_ready(
            Some(1),
            false,
            Some(3),
            Some(2),
            2,
            ready
        ));
        // Hovering the page that is already active must not invoke
        // `activate_tab` and unexpectedly clear that page's zoom state.
        assert!(!tab_drag_hover_ready(
            Some(1),
            true,
            Some(2),
            Some(2),
            2,
            ready
        ));
    }

    #[test]
    fn a_drag_returning_to_its_source_restores_origin_but_a_click_activates() {
        assert!(tab_drag_hover_left_source(Some(2), None));
        assert!(tab_drag_hover_left_source(Some(2), Some(3)));
        assert!(!tab_drag_hover_left_source(Some(2), Some(2)));
        assert_eq!(
            tab_drag_release_action(Some(2), Some(2), false),
            TabDragReleaseAction::Activate(2)
        );
        assert_eq!(
            tab_drag_release_action(Some(2), Some(2), true),
            TabDragReleaseAction::RestoreOrigin
        );
        assert_eq!(
            tab_drag_release_action(Some(2), Some(3), true),
            TabDragReleaseAction::Reorder { from: 2, to: 3 }
        );
        assert_eq!(
            tab_drag_release_action(None, Some(3), true),
            TabDragReleaseAction::RestoreOrigin
        );
    }

    #[test]
    fn ordinary_tab_moves_to_each_direction_without_duplicating_its_session() {
        let cases = [
            (PaneDirection::Left, Axis::Vertical, vec![0, 1]),
            (PaneDirection::Right, Axis::Vertical, vec![1, 0]),
            (PaneDirection::Up, Axis::Horizontal, vec![0, 1]),
            (PaneDirection::Down, Axis::Horizontal, vec![1, 0]),
        ];
        for (direction, expected_axis, expected_leaves) in cases {
            let mut tabs = vec![Tab::new(10, 0), Tab::new(20, 1)];
            let result = move_plain_tab_into_split(&mut tabs, 10, 1, direction);

            assert_eq!(result, Some((0, 0)), "{direction:?}");
            assert_eq!(tabs.len(), 1, "{direction:?}");
            assert_eq!(tabs[0].sessions(), expected_leaves, "{direction:?}");
            assert_eq!(tabs[0].focus, 0, "{direction:?}");
            let PaneTree::Split { axis, .. } = &tabs[0].tree else {
                panic!("directional drop did not create a split")
            };
            assert_eq!(*axis, expected_axis, "{direction:?}");
            let all = tabs.iter().flat_map(Tab::sessions).collect::<Vec<_>>();
            assert_eq!(all.iter().filter(|session| **session == 0).count(), 1);
        }
    }

    #[test]
    fn invalid_tab_to_split_drops_are_transactional_no_ops() {
        let mut split_source = vec![split_tab(10, &[0, 1]), Tab::new(20, 2)];
        let before = split_source
            .iter()
            .map(|tab| (tab.id, tab.sessions(), tab.focus))
            .collect::<Vec<_>>();
        assert_eq!(
            move_plain_tab_into_split(&mut split_source, 10, 2, PaneDirection::Left),
            None
        );
        assert_eq!(
            split_source
                .iter()
                .map(|tab| (tab.id, tab.sessions(), tab.focus))
                .collect::<Vec<_>>(),
            before
        );

        let target_sessions = (1..=MAX_PANES).collect::<Vec<_>>();
        let mut full_target = vec![Tab::new(10, 0), split_tab(20, &target_sessions)];
        let before = full_target
            .iter()
            .map(|tab| (tab.id, tab.sessions(), tab.focus))
            .collect::<Vec<_>>();
        assert_eq!(
            move_plain_tab_into_split(&mut full_target, 10, 1, PaneDirection::Right),
            None
        );
        assert_eq!(
            full_target
                .iter()
                .map(|tab| (tab.id, tab.sessions(), tab.focus))
                .collect::<Vec<_>>(),
            before
        );

        let mut same_tab = vec![Tab::new(10, 0), Tab::new(20, 1)];
        assert_eq!(
            move_plain_tab_into_split(&mut same_tab, 10, 0, PaneDirection::Down),
            None
        );
        assert_eq!(same_tab[0].sessions(), vec![0]);
    }

    #[test]
    fn nested_split_pane_promotes_to_one_new_tab_and_collapses_its_source() {
        let mut tabs = vec![
            Tab {
                id: 7,
                tree: PaneTree::Split {
                    axis: Axis::Vertical,
                    children: vec![
                        PaneTree::Leaf(0),
                        PaneTree::Split {
                            axis: Axis::Horizontal,
                            children: vec![PaneTree::Leaf(1), PaneTree::Leaf(2)],
                            ratios: vec![0.5, 0.5],
                        },
                    ],
                    ratios: vec![0.4, 0.6],
                },
                focus: 1,
                title: None,
                pinned: false,
                marked: false,
                private_title: false,
            },
            Tab::new(8, 3),
        ];
        let mut next_id = 9;

        let promoted = promote_split_pane_to_tab(&mut tabs, &mut next_id, 1, Some(8));

        assert_eq!(promoted, Some((2, 1)));
        assert_eq!(next_id, 10);
        assert_eq!(tabs[0].sessions(), vec![0, 2]);
        assert_eq!(tabs[0].focus, 0);
        assert_eq!(tabs[2].id, 9);
        assert_eq!(tabs[2].sessions(), vec![1]);
        let mut all = tabs.iter().flat_map(Tab::sessions).collect::<Vec<_>>();
        all.sort_unstable();
        assert_eq!(all, vec![0, 1, 2, 3]);
    }

    #[test]
    fn invalid_pane_promotions_leave_layout_and_id_counter_untouched() {
        let mut tabs = vec![split_tab(1, &[0, 1]), Tab::new(2, 2)];
        let before = tabs
            .iter()
            .map(|tab| (tab.id, tab.sessions(), tab.focus))
            .collect::<Vec<_>>();
        let mut next_id = 3;
        assert_eq!(
            promote_split_pane_to_tab(&mut tabs, &mut next_id, 1, Some(999)),
            None
        );
        assert_eq!(next_id, 3);
        assert_eq!(
            tabs.iter()
                .map(|tab| (tab.id, tab.sessions(), tab.focus))
                .collect::<Vec<_>>(),
            before
        );

        assert_eq!(
            promote_split_pane_to_tab(&mut tabs, &mut next_id, 2, None),
            None
        );
        assert_eq!(next_id, 3);
    }

    /// The headline rule: a tab owns its panes, so closing it takes every
    /// session in it and leaves the neighbouring tabs pointing where they were.
    #[test]
    fn closing_a_tab_takes_all_its_sessions_without_disturbing_neighbours() {
        // tab0: [0]  tab1: [1, 2, 3]  tab2: [4]
        let mut tabs = vec![Tab::new(0, 0), split_tab(1, &[1, 2, 3]), Tab::new(2, 4)];

        // What `close_tab` does: drop the tab, then close its sessions from the
        // highest index down so the ones still queued do not shift.
        let owned = tabs[1].sessions();
        assert_eq!(owned, vec![1, 2, 3]);
        tabs.remove(1);
        for session in owned.into_iter().rev() {
            reindex_tabs_for_removal(&mut tabs, session);
        }

        assert_eq!(tabs.len(), 2);
        assert_eq!(tabs[0].sessions(), vec![0]);
        // Session 4 slid down to 1 as the three below it went away.
        assert_eq!(tabs[1].sessions(), vec![1]);
        assert_eq!(tabs[1].focus, 1);
    }

    #[test]
    fn closing_one_pane_leaves_its_tab_and_reindexes_the_rest() {
        let mut tabs = vec![split_tab(0, &[0, 1]), Tab::new(1, 2)];
        tabs[0].focus = 1;

        reindex_tabs_for_removal(&mut tabs, 0);

        // The tab survives with its remaining pane, now at index 0.
        assert_eq!(tabs[0].sessions(), vec![0]);
        assert_eq!(tabs[0].focus, 0);
        assert_eq!(tabs[1].sessions(), vec![1]);
    }

    #[test]
    fn inserting_a_session_shifts_every_tab_not_just_the_active_one() {
        let mut tabs = vec![Tab::new(0, 0), split_tab(1, &[1, 2])];

        reindex_tabs_for_insert(&mut tabs, 1);

        assert_eq!(tabs[0].sessions(), vec![0]);
        assert_eq!(tabs[1].sessions(), vec![2, 3]);
        assert_eq!(tabs[1].focus, 2);
    }

    #[test]
    fn a_v1_layout_becomes_one_tab_and_loose_sessions_are_adopted() {
        // The old global tree held sessions 0 and 1; session 2 was a hidden tab.
        let tree = PaneTree::Split {
            axis: Axis::Vertical,
            children: vec![PaneTree::Leaf(0), PaneTree::Leaf(1)],
            ratios: vec![0.5, 0.5],
        };

        // v1 has no per-tab focus, so the restore path seeds it with the
        // snapshot's active session (1 here).
        let (tabs, active, next_id) =
            build_restored_tabs(vec![RestoredTab::plain(tree, Some(1))], 3, 1, None);

        assert_eq!(tabs.len(), 2);
        assert_eq!(tabs[0].sessions(), vec![0, 1]);
        assert_eq!(tabs[0].focus, 1);
        // The orphan was adopted rather than left unreachable.
        assert_eq!(tabs[1].sessions(), vec![2]);
        assert_eq!(active, 0);
        assert_eq!(next_id, 2);
    }

    #[test]
    fn a_session_claimed_twice_lands_in_exactly_one_tab() {
        // Two tabs both name session 0; the second is dropped, and the adoption
        // pass then gives session 1 its own tab.
        let (tabs, _, _) = build_restored_tabs(
            vec![
                RestoredTab::plain(PaneTree::Leaf(0), None),
                RestoredTab::plain(PaneTree::Leaf(0), None),
            ],
            2,
            0,
            Some(0),
        );

        assert_eq!(tabs.len(), 2);
        assert_eq!(tabs[0].sessions(), vec![0]);
        assert_eq!(tabs[1].sessions(), vec![1]);
    }

    #[test]
    fn an_out_of_range_active_tab_falls_back_to_the_active_session() {
        let (tabs, active, _) = build_restored_tabs(
            vec![
                RestoredTab::plain(PaneTree::Leaf(0), None),
                RestoredTab::plain(PaneTree::Leaf(1), None),
            ],
            2,
            1,
            Some(9),
        );

        assert_eq!(tabs.len(), 2);
        assert_eq!(active, 1);
        assert_eq!(tabs[active].focus, 1);
    }

    /// Pin / mark / title are per-tab state the context menu sets. They ride
    /// the snapshot, and pinned tabs have to lead the strip once restored —
    /// otherwise "pin" silently becomes a no-op across a restart.
    #[test]
    fn restored_tabs_keep_their_menu_state_and_pinned_ones_lead() {
        let restored = vec![
            RestoredTab::plain(PaneTree::Leaf(0), None),
            RestoredTab {
                tree: PaneTree::Leaf(1),
                focus: None,
                title: Some("build".to_string()),
                pinned: false,
                marked: true,
                private_title: true,
            },
            RestoredTab {
                tree: PaneTree::Leaf(2),
                focus: None,
                title: None,
                pinned: true,
                marked: false,
                private_title: false,
            },
        ];

        // The user was last on the tab holding session 1 (index 1 in the file).
        let (tabs, active, _) = build_restored_tabs(restored, 3, 1, Some(1));

        assert_eq!(tabs.len(), 3);
        // The pinned tab moved to the front; the other two kept their order.
        assert_eq!(tabs[0].sessions(), vec![2]);
        assert!(tabs[0].pinned);
        assert_eq!(tabs[1].sessions(), vec![0]);
        assert_eq!(tabs[2].sessions(), vec![1]);
        assert_eq!(tabs[2].title.as_deref(), Some("build"));
        assert!(tabs[2].marked);
        assert!(tabs[2].private_title);
        // The reorder followed the active tab instead of leaving the index put.
        assert_eq!(tabs[active].sessions(), vec![1]);
    }

    /// Pinning must not change which tab is on screen, and the relative order
    /// inside each group has to survive.
    #[test]
    fn sorting_pinned_first_is_stable_and_follows_the_active_tab() {
        let mut tabs = vec![
            Tab::new(0, 0),
            Tab::new(1, 1),
            Tab::new(2, 2),
            Tab::new(3, 3),
        ];
        tabs[2].pinned = true;

        let active = sort_pinned_first(&mut tabs, 1);

        assert_eq!(
            tabs.iter().map(|tab| tab.id).collect::<Vec<_>>(),
            vec![2, 0, 1, 3]
        );
        // The tab that was active (id 1) is still the active one.
        assert_eq!(tabs[active].id, 1);

        // Unpinning puts it back at the head of the unpinned group, and the
        // active tab still does not move.
        tabs[0].pinned = false;
        let active = sort_pinned_first(&mut tabs, active);
        assert_eq!(
            tabs.iter().map(|tab| tab.id).collect::<Vec<_>>(),
            vec![2, 0, 1, 3]
        );
        assert_eq!(tabs[active].id, 1);
    }

    #[test]
    fn drag_reorder_clamps_both_sides_of_the_pinned_boundary() {
        let mut tabs = vec![
            Tab::new(0, 0),
            Tab::new(1, 1),
            Tab::new(2, 2),
            Tab::new(3, 3),
        ];
        tabs[0].pinned = true;
        tabs[1].pinned = true;

        // An unpinned tab dropped on the first pinned tab lands immediately
        // after the pinned prefix; the formerly active id 2 remains active.
        let active = reorder_tabs_preserving_pinned_prefix(&mut tabs, 2, 3, 0).unwrap();
        assert_eq!(
            tabs.iter().map(|tab| tab.id).collect::<Vec<_>>(),
            vec![0, 1, 3, 2]
        );
        assert_eq!(tabs[active].id, 2);
        assert!(tabs[..2].iter().all(|tab| tab.pinned));
        assert!(tabs[2..].iter().all(|tab| !tab.pinned));

        // A pinned tab dropped on an unpinned tab stays at the other side of
        // the same boundary instead of persisting an invalid mixed prefix.
        let active = reorder_tabs_preserving_pinned_prefix(&mut tabs, active, 0, 3).unwrap();
        assert_eq!(
            tabs.iter().map(|tab| tab.id).collect::<Vec<_>>(),
            vec![1, 0, 3, 2]
        );
        assert_eq!(tabs[active].id, 2);
        assert!(tabs[..2].iter().all(|tab| tab.pinned));
        assert!(tabs[2..].iter().all(|tab| !tab.pinned));
    }

    #[test]
    fn a_new_unpinned_tab_never_splits_the_pinned_prefix() {
        let mut tabs = vec![
            Tab::new(0, 0),
            Tab::new(1, 1),
            Tab::new(2, 2),
            Tab::new(3, 3),
        ];
        tabs[0].pinned = true;
        tabs[1].pinned = true;
        tabs[2].pinned = true;

        assert_eq!(new_unpinned_tab_index(&tabs, 0), 3);
        assert_eq!(new_unpinned_tab_index(&tabs, 2), 3);
        assert_eq!(new_unpinned_tab_index(&tabs, 3), 4);
    }

    #[test]
    fn a_restored_tab_may_hold_a_single_pane() {
        // One leaf used to be rejected: it meant "not split" when there was a
        // single global layout. It is the ordinary case for a tab.
        assert!(valid_restored_layout(&PaneTree::Leaf(0), 1));
        assert!(!valid_restored_layout(&PaneTree::Leaf(3), 1));
    }

    #[test]
    fn swapping_two_panes_exchanges_only_them_and_keeps_the_shape() {
        let original = PaneTree::Split {
            axis: Axis::Vertical,
            children: vec![
                PaneTree::Leaf(0),
                PaneTree::Split {
                    axis: Axis::Horizontal,
                    children: vec![PaneTree::Leaf(1), PaneTree::Leaf(2)],
                    ratios: vec![0.3, 0.7],
                },
            ],
            ratios: vec![0.6, 0.4],
        };

        let mut tree = original.clone();
        swap_sessions_in_tree(&mut tree, 0, 2);
        assert_eq!(tree.leaves(), vec![2, 1, 0]);

        // Ratios and nesting describe the geometry; a swap must not touch them.
        let ratios_of = |tree: &PaneTree| match tree {
            PaneTree::Split {
                ratios, children, ..
            } => {
                let nested = match &children[1] {
                    PaneTree::Split { ratios, .. } => ratios.clone(),
                    PaneTree::Leaf(_) => Vec::new(),
                };
                (ratios.clone(), nested)
            }
            PaneTree::Leaf(_) => (Vec::new(), Vec::new()),
        };
        assert_eq!(ratios_of(&tree), ratios_of(&original));

        // Swapping back restores the original exactly; a two-pass remap would
        // instead have collapsed both leaves onto one session.
        swap_sessions_in_tree(&mut tree, 0, 2);
        assert_eq!(tree, original);

        // Swapping a pane with itself is a no-op rather than a corruption.
        swap_sessions_in_tree(&mut tree, 1, 1);
        assert_eq!(tree, original);
    }

    #[test]
    fn session_last_targets_the_final_index_without_underflow() {
        assert_eq!(last_session_index(0), None);
        assert_eq!(last_session_index(1), Some(0));
        assert_eq!(last_session_index(12), Some(11));
    }

    /// Replaces the old `bracketed_paste_framing_preserves_payload`, which
    /// pinned the local encoder's *verbatim* framing — the bug. What must hold
    /// now is that a payload cannot close the frame it is carried in.
    #[test]
    fn paste_framing_cannot_be_closed_by_its_own_payload() {
        let policy = PastePolicy::clipboard(UnbracketedMultiline::SendVerbatim);
        let paste = pty_input::encode_paste(
            "docs\x1b[201~\rrm -rf ~\r",
            PasteModes { bracketed: true },
            policy,
        );

        // One frame, and the terminator appears exactly once: at the end.
        assert!(paste.bytes.starts_with(b"\x1b[200~"));
        assert!(paste.bytes.ends_with(b"\x1b[201~"));
        assert_eq!(
            paste
                .bytes
                .windows(6)
                .filter(|window| *window == b"\x1b[201~")
                .count(),
            1,
            "payload still carries a frame terminator: {:?}",
            String::from_utf8_lossy(&paste.bytes)
        );
        assert!(paste.risk.had_embedded_paste_marker);
        // frost sends every line of an unbracketed multiline paste, unchanged.
        assert_eq!(paste.echo_text, "docs\nrm -rf ~\n");
    }

    /// frost keeps `SendVerbatim`: a multiline paste into a shell that never
    /// advertised DECSET 2004 is still sent whole, framing omitted.
    #[test]
    fn unbracketed_multiline_paste_stays_verbatim() {
        let paste = pty_input::encode_paste(
            "one\ntwo",
            PasteModes { bracketed: false },
            PastePolicy::clipboard(UnbracketedMultiline::SendVerbatim),
        );
        assert_eq!(paste.bytes, b"one\ntwo");
        assert!(!paste.risk.truncated_to_first_line);
    }

    /// Search-replace, sidebar paths and recalled history can all contain
    /// child/filesystem-controlled bytes. Production PromptInsert/Recall uses
    /// this control-stripping policy; no untrusted route receives the raw
    /// control-preserving prompt-insert policy.
    #[test]
    fn untrusted_prompt_payloads_strip_control_bytes() {
        let modes = PasteModes { bracketed: true };
        let sanitized = crate::review_text::sanitize_prompt_payload(
            "printf '\x1b[31m'",
            crate::review_text::MAX_PROMPT_INSERT_BYTES,
        )
        .unwrap();
        let untrusted = pty_input::encode_paste(
            &sanitized,
            modes,
            PastePolicy::clipboard(UnbracketedMultiline::SendVerbatim),
        );
        assert_eq!(untrusted.echo_text, "printf '[31m'");

        // frost's pinned core has no visual-risk confirmation UI, so a pure
        // clipboard paste containing hidden formatting must fail closed too.
        assert!(matches!(
            crate::review_text::sanitize_prompt_payload(
                "printf safe\u{202e}hidden",
                MAX_PTY_WRITE_QUEUE_BYTES,
            ),
            Err(crate::review_text::ReviewTextError::VisualSpoof)
        ));
    }

    /// Replaces the raw `command + CR` write in `agent_run_approved`: the
    /// approved command must replace the pending line (`Ctrl+U` first), carry
    /// no frame terminator of its own, and submit *outside* the frame.
    #[test]
    fn an_approved_agent_command_replaces_the_line_and_submits_outside_the_frame() {
        let paste = agent_command_payload("git status\x1b[201~; rm -rf ~", true).unwrap();
        assert_eq!(paste.bytes[0], 0x15, "line kill must come first");
        assert!(paste.bytes[1..].starts_with(b"\x1b[200~"));
        assert!(paste.bytes.ends_with(b"\x1b[201~\r"));
        // The embedded ESC control was stripped, leaving inert visible text.
        assert_eq!(paste.echo_text, "git status[201~; rm -rf ~");
        assert!(!paste.risk.had_embedded_paste_marker);

        // A shell without DECSET 2004 still gets the kill and the submit.
        let plain = agent_command_payload("echo hi", false).unwrap();
        assert_eq!(plain.bytes, b"\x15echo hi\r");
        assert!(matches!(
            agent_command_payload("echo safe\u{202e}hidden", true),
            Err(crate::review_text::ReviewTextError::VisualSpoof)
        ));
    }

    /// `Message::PromptRecall` (history picker) kills the pending line before
    /// typing; `Message::PromptInsert` (search-replace, sidebar path picks)
    /// appends instead. Both use the control-stripping untrusted policy.
    #[test]
    fn history_recall_replaces_the_pending_line_and_inserts_append() {
        let modes = PasteModes { bracketed: true };
        let policy = PastePolicy::clipboard(UnbracketedMultiline::SendVerbatim);
        let recall = pty_input::encode_prompt_insert("cargo test", modes, policy, true);
        assert_eq!(recall.bytes[0], 0x15);
        assert_eq!(&recall.bytes[1..], b"\x1b[200~cargo test\x1b[201~");

        let insert = pty_input::encode_prompt_insert("cargo test", modes, policy, false);
        assert_eq!(insert.bytes, b"\x1b[200~cargo test\x1b[201~");
    }

    /// Why `SidebarInsertPath` routes through the paste choke point instead of
    /// writing raw bytes: `shell_quote_path` protects the *shell parser*, but at
    /// the input layer a filename carrying a raw CR would still submit the
    /// pending line and an embedded `ESC[201~` would still close a paste frame.
    #[test]
    fn a_sidebar_path_pick_cannot_close_a_frame_or_submit_the_line() {
        let hostile = "evil\x1b[201~\rrm -rf ~";
        let mut quoted = jterm_core::process::shell_quote_path(hostile);
        quoted.push(' ');
        let quoted = crate::review_text::sanitize_prompt_payload(
            &quoted,
            crate::review_text::MAX_PROMPT_INSERT_BYTES,
        )
        .unwrap();
        let paste = pty_input::encode_prompt_insert(
            &quoted,
            PasteModes { bracketed: true },
            PastePolicy::clipboard(UnbracketedMultiline::SendVerbatim),
            false,
        );
        // Exactly one frame terminator, and it is the frame's own.
        assert_eq!(
            paste
                .bytes
                .windows(6)
                .filter(|window| *window == b"\x1b[201~")
                .count(),
            1
        );
        assert!(paste.bytes.ends_with(b"\x1b[201~"));
        // The control-stripping prompt boundary removes the CR entirely, so no
        // executable line break reaches the shell quote or paste frame.
        assert!(!paste.echo_text.contains('\r'));
        assert!(!paste.echo_text.contains('\n'));
        assert!(!paste.bytes.ends_with(b"\x1b[201~\r"));

        let rejected_path = jterm_core::process::shell_quote_path("safe\u{2066}hidden");
        assert_eq!(rejected_path, "''", "the shared quoter fails closed");
        let sanitized = crate::review_text::sanitize_prompt_payload(
            &rejected_path,
            crate::review_text::MAX_PROMPT_INSERT_BYTES,
        )
        .unwrap();
        assert_eq!(sanitized, "''");
    }

    #[test]
    fn tiny_pty_writes_are_coalesced_and_entry_bounded() {
        let mut queue = std::collections::VecDeque::new();
        for _ in 0..1000 {
            Session::push_queue_copy(&mut queue, b"x", false);
        }
        assert_eq!(queue.len(), 1);
        assert_eq!(queue[0].data.len(), 1000);

        Session::push_queue_copy(&mut queue, b"response", true);
        Session::push_queue_copy(&mut queue, b"later-input", false);
        assert_eq!(queue.len(), 3, "different classes must preserve FIFO order");
        assert!(!queue[0].response);
        assert!(queue[1].response);
        assert!(!queue[2].response);

        queue.resize_with(MAX_PTY_QUEUE_ENTRIES, || PtyWriteChunk {
            data: Vec::new(),
            response: false,
        });
        queue.back_mut().expect("queue is populated").data = vec![0; PTY_QUEUE_COALESCE_BYTES];
        assert!(!Session::queue_accepts_entry(&queue, 1, false));
    }

    #[test]
    fn modified_function_keys_keep_their_xterm_modifier_parameters() {
        let cases = [
            (
                Named::ArrowLeft,
                keyboard::Modifiers::CTRL,
                false,
                b"\x1b[1;5D".as_slice(),
            ),
            (
                Named::F5,
                keyboard::Modifiers::SHIFT,
                false,
                b"\x1b[15;2~".as_slice(),
            ),
            (
                Named::PageDown,
                keyboard::Modifiers::ALT,
                false,
                b"\x1b[6;3~".as_slice(),
            ),
            (
                Named::F1,
                keyboard::Modifiers::CTRL | keyboard::Modifiers::SHIFT,
                false,
                b"\x1b[1;6P".as_slice(),
            ),
            (
                Named::ArrowUp,
                keyboard::Modifiers::NONE,
                true,
                b"\x1bOA".as_slice(),
            ),
        ];

        for (named, modifiers, app_cursor, expected) in cases {
            let encoded = encode_key(
                &keyboard::Key::Named(named),
                keyboard::Location::Standard,
                modifiers,
                None,
                app_cursor,
                KeyboardEnhancements::default(),
            );
            assert_eq!(encoded.as_deref(), Some(expected), "{named:?}");
        }

        let report_all_arrow = encode_key(
            &keyboard::Key::Named(Named::ArrowUp),
            keyboard::Location::Standard,
            keyboard::Modifiers::NONE,
            None,
            true,
            KeyboardEnhancements {
                report_all_keys: true,
                ..Default::default()
            },
        );
        assert_eq!(report_all_arrow.as_deref(), Some(&b"\x1b[1;1A"[..]));
    }

    #[test]
    fn legacy_control_keys_preserve_ctrl_and_alt_semantics() {
        let ctrl_backspace = encode_key(
            &keyboard::Key::Named(Named::Backspace),
            keyboard::Location::Standard,
            keyboard::Modifiers::CTRL,
            None,
            false,
            KeyboardEnhancements::default(),
        );
        assert_eq!(ctrl_backspace.as_deref(), Some(&b"\x08"[..]));

        let ctrl_alt_backspace = encode_key(
            &keyboard::Key::Named(Named::Backspace),
            keyboard::Location::Standard,
            keyboard::Modifiers::CTRL | keyboard::Modifiers::ALT,
            None,
            false,
            KeyboardEnhancements::default(),
        );
        assert_eq!(ctrl_alt_backspace.as_deref(), Some(&b"\x1b\x08"[..]));

        let ctrl_space = encode_key(
            &keyboard::Key::Named(Named::Space),
            keyboard::Location::Standard,
            keyboard::Modifiers::CTRL,
            Some(" "),
            false,
            KeyboardEnhancements::default(),
        );
        assert_eq!(ctrl_space.as_deref(), Some(&b"\0"[..]));
    }

    #[test]
    fn kitty_report_all_and_disambiguation_do_not_fall_back_to_plain_text() {
        let report_all = KeyboardEnhancements {
            kitty_flags: 0b1000,
            report_all_keys: true,
            ..Default::default()
        };
        let letter = encode_key(
            &keyboard::Key::Character("a".into()),
            keyboard::Location::Standard,
            keyboard::Modifiers::NONE,
            Some("a"),
            false,
            report_all,
        );
        assert_eq!(letter.as_deref(), Some(&b"\x1b[97;1u"[..]));

        let enter = encode_key(
            &keyboard::Key::Named(Named::Enter),
            keyboard::Location::Standard,
            keyboard::Modifiers::NONE,
            None,
            false,
            report_all,
        );
        assert_eq!(enter.as_deref(), Some(&b"\x1b[13;1u"[..]));

        let disambiguate = KeyboardEnhancements {
            kitty_flags: 0b1,
            ..Default::default()
        };
        let escape = encode_key(
            &keyboard::Key::Named(Named::Escape),
            keyboard::Location::Standard,
            keyboard::Modifiers::NONE,
            None,
            false,
            disambiguate,
        );
        assert_eq!(escape.as_deref(), Some(&b"\x1b[27;1u"[..]));

        let legacy_enter = encode_key(
            &keyboard::Key::Named(Named::Enter),
            keyboard::Location::Standard,
            keyboard::Modifiers::NONE,
            None,
            false,
            disambiguate,
        );
        assert_eq!(legacy_enter.as_deref(), Some(&b"\r"[..]));

        let ctrl_super = encode_key(
            &keyboard::Key::Character("a".into()),
            keyboard::Location::Standard,
            keyboard::Modifiers::CTRL | keyboard::Modifiers::LOGO,
            None,
            false,
            disambiguate,
        );
        assert_eq!(ctrl_super.as_deref(), Some(&b"\x1b[97;13u"[..]));
    }

    #[test]
    fn modify_other_keys_handles_shifted_text_and_level_three() {
        // Shift alone already produced the character, so xterm leaves it as
        // text at level 2 — only Ctrl/Alt/Meta build a report.
        let shifted_symbol = encode_key(
            &keyboard::Key::Character("1".into()),
            keyboard::Location::Standard,
            keyboard::Modifiers::SHIFT,
            Some("!"),
            false,
            KeyboardEnhancements {
                modify_other_keys: 2,
                ..Default::default()
            },
        );
        assert_eq!(shifted_symbol.as_deref(), Some(&b"!"[..]));

        let ctrl_shifted_symbol = encode_key(
            &keyboard::Key::Character("1".into()),
            keyboard::Location::Standard,
            keyboard::Modifiers::CTRL | keyboard::Modifiers::SHIFT,
            Some("!"),
            false,
            KeyboardEnhancements {
                modify_other_keys: 2,
                ..Default::default()
            },
        );
        assert_eq!(ctrl_shifted_symbol.as_deref(), Some(&b"\x1b[27;6;33~"[..]));

        let shifted_tab = encode_key(
            &keyboard::Key::Named(Named::Tab),
            keyboard::Location::Standard,
            keyboard::Modifiers::SHIFT,
            None,
            false,
            KeyboardEnhancements {
                modify_other_keys: 2,
                ..Default::default()
            },
        );
        assert_eq!(shifted_tab.as_deref(), Some(&b"\x1b[Z"[..]));

        let unmodified_level_three = encode_key(
            &keyboard::Key::Character("x".into()),
            keyboard::Location::Standard,
            keyboard::Modifiers::NONE,
            Some("x"),
            false,
            KeyboardEnhancements {
                modify_other_keys: 3,
                ..Default::default()
            },
        );
        assert_eq!(
            unmodified_level_three.as_deref(),
            Some(&b"\x1b[27;1;120~"[..])
        );
    }

    /// The Claude CLI enables private mode 2031 (in-band theme-change
    /// notification) on startup. It must not put the keyboard into any enhanced
    /// mode: every capital letter reached the app as a CSI report carrying the
    /// unshifted codepoint, so typing "This" produced "this".
    #[test]
    fn theme_notification_mode_leaves_capital_letters_as_text() {
        let mut terminal = terminal::TerminalState::new(8, 2);
        terminal.process_input(b"\x1b[?2031h");
        assert!(!terminal.is_report_all_keys_enabled());

        let enh = KeyboardEnhancements {
            kitty_flags: terminal.keyboard_enhancement_flags(),
            modify_other_keys: terminal.xterm_modify_other_keys(),
            format_other_keys: terminal.xterm_format_other_keys(),
            report_all_keys: terminal.is_report_all_keys_enabled(),
            application_keypad: terminal.is_application_keypad(),
        };

        let shifted = encode_key(
            &keyboard::Key::Character("t".into()),
            keyboard::Location::Standard,
            keyboard::Modifiers::SHIFT,
            Some("T"),
            false,
            enh,
        );
        assert_eq!(shifted.as_deref(), Some(&b"T"[..]));

        // Caps Lock reports no modifier at all; only the committed text says
        // the key produced an upper case character.
        let caps_lock = encode_key(
            &keyboard::Key::Character("t".into()),
            keyboard::Location::Standard,
            keyboard::Modifiers::NONE,
            Some("T"),
            false,
            enh,
        );
        assert_eq!(caps_lock.as_deref(), Some(&b"T"[..]));
    }

    /// Under Kitty's report-all-keys mode the key code is the *unshifted* key,
    /// so the shifted character only survives in the alternate-key (flag 4) and
    /// associated-text (flag 16) fields the app opted into.
    #[test]
    fn kitty_reports_the_shifted_character_when_the_app_asks_for_it() {
        let base = encode_key(
            &keyboard::Key::Character("t".into()),
            keyboard::Location::Standard,
            keyboard::Modifiers::SHIFT,
            Some("T"),
            false,
            KeyboardEnhancements {
                kitty_flags: 0b1000,
                report_all_keys: true,
                ..Default::default()
            },
        );
        assert_eq!(base.as_deref(), Some(&b"\x1b[116;2u"[..]));

        let alternate_and_text = encode_key(
            &keyboard::Key::Character("t".into()),
            keyboard::Location::Standard,
            keyboard::Modifiers::SHIFT,
            Some("T"),
            false,
            KeyboardEnhancements {
                kitty_flags: 0b11100,
                report_all_keys: true,
                ..Default::default()
            },
        );
        assert_eq!(
            alternate_and_text.as_deref(),
            Some(&b"\x1b[116:84;2;84u"[..])
        );

        // Ctrl+letter commits a control byte, which is never "associated text".
        let ctrl_letter = encode_key(
            &keyboard::Key::Character("t".into()),
            keyboard::Location::Standard,
            keyboard::Modifiers::CTRL,
            Some("\u{14}"),
            false,
            KeyboardEnhancements {
                kitty_flags: 0b11100,
                report_all_keys: true,
                ..Default::default()
            },
        );
        assert_eq!(ctrl_letter.as_deref(), Some(&b"\x1b[116;5u"[..]));
    }

    #[test]
    fn enter_honors_key_location_in_application_keypad_mode() {
        let plain = encode_key(
            &keyboard::Key::Named(Named::Enter),
            keyboard::Location::Standard,
            keyboard::Modifiers::default(),
            None,
            false,
            KeyboardEnhancements::default(),
        );
        assert_eq!(plain.as_deref(), Some(&b"\r"[..]));

        let standard_in_keypad_mode = encode_key(
            &keyboard::Key::Named(Named::Enter),
            keyboard::Location::Standard,
            keyboard::Modifiers::default(),
            None,
            false,
            KeyboardEnhancements {
                application_keypad: true,
                ..Default::default()
            },
        );
        assert_eq!(standard_in_keypad_mode.as_deref(), Some(&b"\r"[..]));

        let numpad_in_keypad_mode = encode_key(
            &keyboard::Key::Named(Named::Enter),
            keyboard::Location::Numpad,
            keyboard::Modifiers::default(),
            None,
            false,
            KeyboardEnhancements {
                application_keypad: true,
                ..Default::default()
            },
        );
        assert_eq!(numpad_in_keypad_mode.as_deref(), Some(&b"\x1bOM"[..]));
    }

    fn block_search_hit(zone_id: u64) -> block_mode::BlockSearchHit {
        block_mode::BlockSearchHit {
            zone_id,
            is_output_line: false,
            line_no: 0,
            match_span: None,
            line_text: String::new(),
            command_preview: String::new(),
        }
    }

    #[test]
    fn block_search_build_identity_rejects_old_epochs_even_in_the_same_session() {
        let mut epoch = 0;
        let first = BlockSearchBuildIdentity {
            session_id: 7,
            epoch: next_block_search_epoch(&mut epoch).unwrap(),
        };
        let second = BlockSearchBuildIdentity {
            session_id: 7,
            epoch: next_block_search_epoch(&mut epoch).unwrap(),
        };
        assert_eq!(first.epoch, 1);
        assert_eq!(second.epoch, 2);

        let state = BlockSearchState {
            session_id: 7,
            epoch: second.epoch,
            ..BlockSearchState::default()
        };
        assert!(!state.accepts_build(first));
        assert!(state.accepts_build(second));
        assert!(!state.accepts_build(BlockSearchBuildIdentity {
            session_id: 8,
            epoch: second.epoch,
        }));

        let mut exhausted = u64::MAX;
        assert_eq!(next_block_search_epoch(&mut exhausted), None);
        assert_eq!(exhausted, u64::MAX);
    }

    #[test]
    fn block_search_zone_version_changes_only_when_finalized_zones_change() {
        let mut completed = terminal::TerminalState::new(40, 8);
        let empty = BlockSearchZoneVersion::from_terminal(&completed);
        completed.process_input(
            b"\x1b]133;A\x07$ \x1b]133;B\x07ok\r\n\x1b]133;C\x07done\r\n\x1b]133;D;0\x07",
        );
        let one = BlockSearchZoneVersion::from_terminal(&completed);
        assert_ne!(one, empty);
        assert_eq!(one.len, 1);
        completed.process_input(b"ordinary idle paint");
        assert_eq!(BlockSearchZoneVersion::from_terminal(&completed), one);

        // A missing D is not represented until the following A seals the
        // stale lifecycle, even though it never enters CompletedCommand.
        let mut stale = terminal::TerminalState::new(40, 8);
        stale.process_input(b"\x1b]133;A\x07$ \x1b]133;B\x07lost-d\r\n\x1b]133;C\x07output\r\n");
        assert_eq!(BlockSearchZoneVersion::from_terminal(&stale), empty);
        stale.process_input(b"\x1b]133;A\x07");
        assert_ne!(BlockSearchZoneVersion::from_terminal(&stale), empty);

        // Commandless idle output is finalized by the next prompt marker and
        // must refresh the Background filter as well.
        let mut background = terminal::TerminalState::new(40, 8);
        background.process_input(b"\x1b]133;A\x07$ \x1b]133;B\x07daemon ready\r\n");
        assert_eq!(BlockSearchZoneVersion::from_terminal(&background), empty);
        background.process_input(b"\x1b]133;A\x07");
        assert_ne!(BlockSearchZoneVersion::from_terminal(&background), empty);
    }

    #[test]
    fn block_search_reveal_targets_soft_wrap_and_falls_back_safely() {
        let mut terminal = terminal::TerminalState::new(5, 8);
        terminal.process_input(
            b"\x1b]133;A\x07$ \x1b]133;B\x07x\r\n\x1b]133;C\x07abcdef\r\ngh\r\n\x1b]133;D;0\x07",
        );
        let zone = terminal
            .command_zones
            .back()
            .expect("completed command block");
        let zone_id = zone.id;
        let output_start = zone.output_start.expect("retained output rows");
        let cache = [block_mode::CachedBlockSearchZone::new(
            zone_id,
            zone.command.as_deref(),
            terminal.zone_output_text(zone_id),
        )];
        let hit = block_mode::search_blocks(&cache, "f")
            .hits
            .into_iter()
            .find(|hit| hit.is_output_line)
            .expect("match in second physical row");
        assert_eq!(hit.match_span, Some(5..6));
        assert_eq!(
            block_search_reveal_row(&terminal, &hit),
            Some(output_start + 1)
        );

        // Filter-only browsing has no match span and intentionally lands at
        // the logical line start. A stale snapshot span does the same rather
        // than guessing a physical row from unrelated live content.
        let mut browse = hit.clone();
        browse.match_span = None;
        assert_eq!(
            block_search_reveal_row(&terminal, &browse),
            Some(output_start)
        );
        let mut stale = hit;
        stale.match_span = Some(999..1_000);
        assert_eq!(
            block_search_reveal_row(&terminal, &stale),
            Some(output_start)
        );

        terminal.command_zones.back_mut().unwrap().rows_evicted = true;
        assert_eq!(block_search_reveal_row(&terminal, &stale), None);
    }

    #[test]
    fn block_search_selection_wraps_over_all_hits() {
        // Every hit is drawn (in a scrollable) and navigable — the selection
        // wraps across the FULL hit list, not a visible-rows prefix.
        let mut state = BlockSearchState {
            hits: (0..40).map(block_search_hit).collect(),
            ..BlockSearchState::default()
        };
        state.select_prev();
        assert_eq!(state.selected, 39);
        state.select_next();
        assert_eq!(state.selected, 0);
        for _ in 0..39 {
            state.select_next();
        }
        assert_eq!(state.selected, 39);
        // Empty hits pin the selection at 0 in both directions.
        let mut empty = BlockSearchState::default();
        empty.select_next();
        empty.select_prev();
        assert_eq!(empty.selected, 0);
    }

    #[test]
    fn block_search_count_label_reports_all_hits_and_the_cap() {
        let mut state = BlockSearchState {
            hits: vec![block_search_hit(1)],
            ..BlockSearchState::default()
        };
        assert_eq!(state.count_label(), "1 match");
        state.hits.push(block_search_hit(2));
        assert_eq!(state.count_label(), "2 matches");
        // The hit cap uses ember's wording: it means older blocks went
        // unscanned, not that more matches necessarily exist.
        state.capped = true;
        assert_eq!(state.count_label(), "2 matches · older blocks not searched");
        state.older_not_indexed = true;
        assert_eq!(
            state.count_label(),
            "2 matches · older blocks not searched · older blocks not indexed"
        );
    }

    fn sidebar_clipboard(
        loc: remote_fs::FsLocation,
        path: &std::path::Path,
        is_dir: bool,
        cut: bool,
    ) -> FsClipboard {
        FsClipboard {
            loc,
            path: path.to_path_buf(),
            is_dir,
            cut,
        }
    }

    #[test]
    fn sidebar_paste_op_dispatches_same_and_cross_location() {
        use remote_fs::FsLocation;
        let dir = std::path::Path::new("/target");
        // Same location keeps the copy/move semantics.
        let clip = sidebar_clipboard(
            FsLocation::Local,
            std::path::Path::new("/a/file.txt"),
            false,
            false,
        );
        match sidebar_paste_op(&clip, &FsLocation::Local, dir) {
            Ok(SidebarOp::Copy { src, dst }) => {
                assert_eq!(src, std::path::PathBuf::from("/a/file.txt"));
                assert_eq!(dst, std::path::PathBuf::from("/target/file.txt"));
            }
            other => panic!("expected Copy, got {other:?}"),
        }
        let clip = sidebar_clipboard(
            FsLocation::Local,
            std::path::Path::new("/a/file.txt"),
            false,
            true,
        );
        assert!(matches!(
            sidebar_paste_op(&clip, &FsLocation::Local, dir),
            Ok(SidebarOp::Move { .. })
        ));
        // Cross-location becomes a transfer: upload, download, or relay,
        // with a cut meaning transfer-then-delete-source.
        match sidebar_paste_op(&clip, &FsLocation::Remote(0), dir) {
            Ok(SidebarOp::TransferMove {
                src_loc,
                dst_loc,
                dst,
                is_dir,
                ..
            }) => {
                assert_eq!(src_loc, FsLocation::Local);
                assert_eq!(dst_loc, FsLocation::Remote(0));
                assert_eq!(dst, std::path::PathBuf::from("/target/file.txt"));
                assert!(!is_dir);
            }
            other => panic!("expected TransferMove, got {other:?}"),
        }
        let clip = sidebar_clipboard(
            FsLocation::Remote(1),
            std::path::Path::new("/src/packed"),
            true,
            false,
        );
        match sidebar_paste_op(&clip, &FsLocation::Local, dir) {
            Ok(SidebarOp::Transfer {
                src_loc,
                dst_loc,
                is_dir,
                ..
            }) => {
                assert_eq!(src_loc, FsLocation::Remote(1));
                assert_eq!(dst_loc, FsLocation::Local);
                assert!(is_dir);
            }
            other => panic!("expected Transfer, got {other:?}"),
        }
        // A nameless source ("/") can never be pasted anywhere.
        let clip = sidebar_clipboard(FsLocation::Local, std::path::Path::new("/"), true, false);
        assert!(sidebar_paste_op(&clip, &FsLocation::Local, dir).is_err());
    }

    #[test]
    fn transfer_verb_names_the_transport() {
        use remote_fs::FsLocation;
        assert_eq!(
            transfer_verb(&FsLocation::Local, &FsLocation::Local, false),
            "copy"
        );
        assert_eq!(
            transfer_verb(&FsLocation::Local, &FsLocation::Local, true),
            "move"
        );
        assert_eq!(
            transfer_verb(&FsLocation::Remote(0), &FsLocation::Remote(0), false),
            "copy"
        );
        assert_eq!(
            transfer_verb(&FsLocation::Local, &FsLocation::Remote(0), false),
            "upload"
        );
        assert_eq!(
            transfer_verb(&FsLocation::Remote(0), &FsLocation::Local, false),
            "download"
        );
        assert_eq!(
            transfer_verb(&FsLocation::Remote(0), &FsLocation::Remote(1), false),
            "relay"
        );
        assert_eq!(
            transfer_verb(&FsLocation::Remote(0), &FsLocation::Local, true),
            "download + delete"
        );
    }

    #[test]
    fn sidebar_transfer_move_copies_then_deletes_the_source() {
        use remote_fs::FsLocation;
        let root = std::env::temp_dir().join(format!("frost-op-{}", uuid::Uuid::new_v4()));
        let dst_dir = root.join("dst");
        std::fs::create_dir_all(&dst_dir).expect("create test tree");
        let src = root.join("file.txt");
        std::fs::write(&src, b"payload").expect("write source");
        // Local→Local runs headlessly: the destination appears and the source
        // is deleted only afterwards.
        let op = SidebarOp::TransferMove {
            src_loc: FsLocation::Local,
            dst_loc: FsLocation::Local,
            src: src.clone(),
            dst: dst_dir.join("file.txt"),
            is_dir: false,
        };
        let (changed, warning, result) = run_sidebar_op(&FsLocation::Local, &[], &op);
        assert!(result.is_ok(), "transfer move failed: {result:?}");
        assert!(warning.is_none());
        assert_eq!(changed, FsLocation::Local);
        assert_eq!(
            std::fs::read(dst_dir.join("file.txt")).expect("read moved file"),
            b"payload"
        );
        assert!(!src.exists(), "cut source must be deleted after the copy");

        // A failed transfer must never touch the source.
        let stranded = root.join("stranded.txt");
        std::fs::write(&stranded, b"keep me").expect("write stranded");
        let op = SidebarOp::TransferMove {
            src_loc: FsLocation::Local,
            dst_loc: FsLocation::Remote(9),
            src: stranded.clone(),
            dst: std::path::PathBuf::from("/tmp/stranded.txt"),
            is_dir: false,
        };
        let (changed, _warning, result) = run_sidebar_op(&FsLocation::Remote(9), &[], &op);
        assert!(result.is_err(), "unknown host must fail the transfer");
        assert_eq!(changed, FsLocation::Remote(9));
        assert!(stranded.exists(), "source survives a failed transfer");
        std::fs::remove_dir_all(root).expect("remove test tree");
    }
}
