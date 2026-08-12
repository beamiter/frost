//! Pure logic for Warp-style command blocks (anvil/forge design contract):
//! outcome classification, badge text, row-span math, and badge fitting.
//! Everything here is renderer-agnostic so it can be unit tested; the paint
//! code in `terminal_view` and the per-frame builder in `main` stay thin.

use std::collections::HashSet;

/// Pane-local bookmarks for retained command blocks.
///
/// Bookmarks key on stable zone ids, just like [`BlockSelection`], but carry
/// no active edge or range semantics. The terminal owns a bounded zone deque,
/// so callers reconcile this set against its oldest-first live id list before
/// displaying or acting on bookmarks. [`Self::neighbor`] performs that
/// reconciliation itself so a stale id can never become a navigation target.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BlockBookmarks {
    bookmarked: HashSet<u64>,
}

impl BlockBookmarks {
    pub fn contains(&self, id: u64) -> bool {
        self.bookmarked.contains(&id)
    }

    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.bookmarked.len()
    }

    #[cfg(test)]
    pub fn is_empty(&self) -> bool {
        self.bookmarked.is_empty()
    }

    /// Toggle one stable zone id and return whether it is bookmarked now.
    pub fn toggle(&mut self, id: u64) -> bool {
        if self.bookmarked.remove(&id) {
            false
        } else {
            self.bookmarked.insert(id);
            true
        }
    }

    pub fn clear(&mut self) {
        self.bookmarked.clear();
    }

    /// Drop ids no longer retained by the terminal and report whether any
    /// live bookmarks remain.
    pub fn retain(&mut self, ids: &[u64]) -> bool {
        self.bookmarked.retain(|id| ids.contains(id));
        !self.bookmarked.is_empty()
    }

    /// Find the nearest bookmarked block strictly older/newer than `current`,
    /// wrapping to the opposite edge. `ids` is terminal order, oldest first.
    /// With no live current id, navigation enters at the requested edge:
    /// newest for `older`, oldest for newer. A sole bookmark therefore wraps
    /// to itself. Stale bookmark ids are removed before searching.
    pub fn neighbor(&mut self, ids: &[u64], current: Option<u64>, older: bool) -> Option<u64> {
        if !self.retain(ids) {
            return None;
        }

        let oldest = || {
            ids.iter()
                .find(|&&id| self.bookmarked.contains(&id))
                .copied()
        };
        let newest = || {
            ids.iter()
                .rev()
                .find(|&&id| self.bookmarked.contains(&id))
                .copied()
        };
        let position = current.and_then(|id| ids.iter().position(|&live| live == id));
        if older {
            position
                .and_then(|position| {
                    ids[..position]
                        .iter()
                        .rev()
                        .find(|&&id| self.bookmarked.contains(&id))
                        .copied()
                })
                .or_else(newest)
        } else {
            position
                .and_then(|position| {
                    ids[position + 1..]
                        .iter()
                        .find(|&&id| self.bookmarked.contains(&id))
                        .copied()
                })
                .or_else(oldest)
        }
    }
}

/// Pane-local Warp-style finished-block selection.
///
/// `active` is the moving edge used by keyboard navigation and `anchor` is
/// the fixed edge used by Shift+click / Shift+Up/Down. Keeping the set here,
/// rather than as three loosely-related fields on the UI session, makes every
/// transition preserve the same invariants: active/anchor always name a
/// selected block, and an empty set has neither.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BlockSelection {
    selected: HashSet<u64>,
    active: Option<u64>,
    anchor: Option<u64>,
}

impl BlockSelection {
    pub fn is_empty(&self) -> bool {
        self.selected.is_empty()
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.selected.len()
    }

    pub fn contains(&self, id: u64) -> bool {
        self.selected.contains(&id)
    }

    pub fn active(&self) -> Option<u64> {
        self.active
    }

    pub fn clear(&mut self) {
        self.selected.clear();
        self.active = None;
        self.anchor = None;
    }

    /// Replace the selection with one active block (or clear it).
    pub fn replace(&mut self, id: Option<u64>) {
        self.clear();
        if let Some(id) = id {
            self.selected.insert(id);
            self.active = Some(id);
            self.anchor = Some(id);
        }
    }

    /// Select the entire oldest-first block list. The newest block is the
    /// active edge and the oldest is the fixed range anchor, matching
    /// anvil/forge.
    pub fn select_all(&mut self, ids: &[u64]) {
        self.clear();
        let (Some(&first), Some(&last)) = (ids.first(), ids.last()) else {
            return;
        };
        self.selected.extend(ids.iter().copied());
        self.anchor = Some(first);
        self.active = Some(last);
    }

    /// Apply the family card-click contract. Plain (including Ctrl-only on a
    /// header) replaces, Shift selects the inclusive range from the fixed
    /// anchor, and Ctrl+Shift toggles one block.
    pub fn click(&mut self, ids: &[u64], target: u64, ctrl: bool, shift: bool) {
        if ctrl && shift {
            self.toggle(ids, target);
        } else if shift {
            let anchor = self.anchor.or(self.active).unwrap_or(target);
            self.select_range(ids, anchor, target);
        } else {
            self.replace(Some(target));
        }
    }

    /// Make a context-clicked block the active/anchor edge without collapsing
    /// a multi-selection that already contains it.
    pub fn activate(&mut self, ids: &[u64], target: u64) {
        self.retain(ids);
        if !self.selected.contains(&target) {
            self.replace(Some(target));
            return;
        }
        self.active = Some(target);
        self.anchor = Some(target);
    }

    /// Extend/contract the active edge by one item while retaining the fixed
    /// anchor. Returns the new active id, or `None` at the requested edge.
    pub fn extend_step(&mut self, ids: &[u64], older: bool) -> Option<u64> {
        let active = self.active?;
        let position = ids.iter().position(|&id| id == active)?;
        let target_index = if older {
            position.checked_sub(1)?
        } else if position + 1 < ids.len() {
            position + 1
        } else {
            return None;
        };
        let target = ids[target_index];
        let anchor = self.anchor.unwrap_or(active);
        self.select_range(ids, anchor, target);
        Some(target)
    }

    /// Remove ids no longer retained by the terminal's bounded zone history
    /// and report whether a live selection remains.
    pub fn retain(&mut self, ids: &[u64]) -> bool {
        self.selected.retain(|id| ids.contains(id));
        if self.selected.is_empty() {
            self.clear();
            return false;
        }
        if self.active.is_none_or(|id| !self.selected.contains(&id)) {
            self.active = ids
                .iter()
                .rev()
                .find(|&&id| self.selected.contains(&id))
                .copied();
        }
        if self.anchor.is_none_or(|id| !self.selected.contains(&id)) {
            self.anchor = self.active;
        }
        true
    }

    fn select_range(&mut self, ids: &[u64], anchor: u64, target: u64) {
        let Some(anchor_index) = ids.iter().position(|&id| id == anchor) else {
            self.replace(Some(target));
            return;
        };
        let Some(target_index) = ids.iter().position(|&id| id == target) else {
            self.replace(Some(target));
            return;
        };
        let (start, end) = if anchor_index <= target_index {
            (anchor_index, target_index)
        } else {
            (target_index, anchor_index)
        };
        self.selected.clear();
        self.selected.extend(ids[start..=end].iter().copied());
        self.active = Some(target);
        self.anchor = Some(anchor);
    }

    fn toggle(&mut self, ids: &[u64], target: u64) {
        if self.selected.remove(&target) {
            if self.selected.is_empty() {
                self.clear();
                return;
            }
            if self.active == Some(target)
                || self
                    .active
                    .is_none_or(|id| !self.selected.contains(&id) || !ids.contains(&id))
            {
                self.active = ids
                    .iter()
                    .rev()
                    .find(|&&id| self.selected.contains(&id))
                    .copied();
                // A stale-only remainder is not a usable selection. Preserve
                // the type invariant instead of leaving an invisible non-empty
                // set that still owns keyboard input.
                if self.active.is_none() {
                    self.clear();
                    return;
                }
            }
            if self.anchor == Some(target)
                || self
                    .anchor
                    .is_none_or(|id| !self.selected.contains(&id) || !ids.contains(&id))
            {
                self.anchor = self.active;
            }
        } else {
            self.selected.insert(target);
            self.active = Some(target);
            self.anchor = Some(target);
        }
    }
}

/// Why a multi-block command reinput could not be built atomically.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SelectedCommandsError {
    Empty,
    Truncated,
    TooLarge,
}

/// Sanitization-ready selected command text plus the number of command-bearing
/// blocks that contributed to it. The count deliberately excludes selected
/// background/blank blocks so success UI describes what will actually be
/// inserted rather than the size of the visual selection.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SelectedCommands {
    pub text: String,
    pub block_count: usize,
}

/// Collect selected commands in terminal order. Background blocks are
/// skipped. A shell-truncated command or aggregate beyond `max_bytes` rejects
/// the whole operation: returning a partial command set could have different
/// shell semantics from what the user reviewed.
pub fn selected_commands<'a, I>(
    zones: I,
    selection: &BlockSelection,
    max_bytes: usize,
) -> Result<SelectedCommands, SelectedCommandsError>
where
    I: IntoIterator<Item = (u64, Option<&'a str>, bool)>,
{
    let mut output = String::new();
    let mut block_count = 0usize;
    for (id, command, truncated) in zones {
        if !selection.contains(id) {
            continue;
        }
        let Some(command) = command.filter(|command| !command.trim().is_empty()) else {
            continue;
        };
        if truncated {
            return Err(SelectedCommandsError::Truncated);
        }
        let separator = usize::from(!output.is_empty());
        let Some(next_len) = output
            .len()
            .checked_add(separator)
            .and_then(|length| length.checked_add(command.len()))
        else {
            return Err(SelectedCommandsError::TooLarge);
        };
        if next_len > max_bytes {
            return Err(SelectedCommandsError::TooLarge);
        }
        if separator != 0 {
            output.push('\n');
        }
        output.push_str(command);
        block_count += 1;
    }
    if output.is_empty() {
        Err(SelectedCommandsError::Empty)
    } else {
        Ok(SelectedCommands {
            text: output,
            block_count,
        })
    }
}

/// How a completed command block ended. `Unknown` is deliberately distinct
/// from `Success`: an OSC 133 `D` without an exit code reports *nothing*, and
/// rendering it as a green check would be a success this terminal never
/// observed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BlockOutcome {
    /// No command at all (empty prompt line, or output with no command).
    Background,
    Success,
    Failed(i32),
    Unknown,
}

/// Classify a completed zone. Background (no/blank command) trumps the exit
/// code; otherwise only an explicit `Some(0)` may read as success.
pub fn classify(command: Option<&str>, exit_code: Option<i32>) -> BlockOutcome {
    use jterm_core::block_contract::CompletedBlockOutcome;

    match jterm_core::block_contract::classify_completed(command, exit_code) {
        CompletedBlockOutcome::Background => BlockOutcome::Background,
        CompletedBlockOutcome::Success => BlockOutcome::Success,
        CompletedBlockOutcome::Failed(code) => BlockOutcome::Failed(code),
        CompletedBlockOutcome::Unknown => BlockOutcome::Unknown,
    }
}

/// Human duration for the block badge (family contract, same as forge).
/// Minute-plus durations keep their seconds ("1m32s") — a bare "2m" can't
/// distinguish a 61s build from a 179s one, which is exactly the range users
/// compare across runs.
pub fn format_duration(dur_ms: u64) -> String {
    if dur_ms < 1000 {
        format!("{dur_ms}ms")
    } else if dur_ms < 60_000 {
        format!("{:.1}s", dur_ms as f64 / 1000.0)
    } else if dur_ms < 3_600_000 {
        let m = dur_ms / 60_000;
        let s = (dur_ms % 60_000) / 1000;
        if s == 0 {
            format!("{m}m")
        } else {
            format!("{m}m{s:02}s")
        }
    } else {
        let h = dur_ms / 3_600_000;
        let m = (dur_ms % 3_600_000) / 60_000;
        if m == 0 {
            format!("{h}h")
        } else {
            format!("{h}h{m:02}m")
        }
    }
}

/// First-row card badge text (running blocks have their own live badge).
/// Background output and unknown completions stay explicit rather than looking
/// like ordinary successful commands.
/// Failed blocks name the killing signal right after the code (the
/// `jterm_core::bottom_bar` convention), with the duration last.
pub fn badge_text(outcome: BlockOutcome, duration_ms: Option<u64>) -> Option<String> {
    match outcome {
        BlockOutcome::Background => Some("↻ Background".to_string()),
        BlockOutcome::Success => Some(match duration_ms {
            Some(ms) => format!("✓ {}", format_duration(ms)),
            None => "✓".to_string(),
        }),
        BlockOutcome::Failed(code) => {
            let mut text = format!("✗ exit:{code}");
            if let Some(signal) = jterm_core::exit_status::signal_name_for_exit(code) {
                text.push_str(&format!(" {signal}"));
            }
            if let Some(ms) = duration_ms {
                text.push_str(&format!(" · {}", format_duration(ms)));
            }
            Some(text)
        }
        BlockOutcome::Unknown => Some("? exit:?".to_string()),
    }
}

/// Compact live badge for an OSC 133 command whose `C` arrived but whose
/// `D` has not. Forge keeps an elapsed running header visible while a command
/// executes; frost uses the same feedback in the command's prompt row, where
/// space is much tighter. The arrow is deliberately distinct from the final
/// success/failure glyphs so an in-flight command can never look completed.
pub fn running_badge_text(elapsed_ms: u64) -> String {
    format!("▶ {}", format_duration(elapsed_ms))
}

/// Row span (start inclusive, end exclusive, absolute buffer rows) of each
/// completed block, oldest first. Block `i` runs from its own prompt row to
/// the next block's prompt row; the newest block is closed by
/// `live_boundary` — the in-flight OSC 133 state's prompt row when one
/// exists, otherwise the end of the buffer.
pub fn spans(starts: &[usize], live_boundary: usize) -> Vec<(usize, usize)> {
    starts
        .iter()
        .enumerate()
        .map(|(i, &start)| {
            let end = starts.get(i + 1).copied().unwrap_or(live_boundary);
            (start, end.max(start))
        })
        .collect()
}

/// End-exclusive header hit column for one physical row of a finished card.
/// `usize::MAX` means the full row. Command-bearing cards own every prompt /
/// wrapped-command row before output begins, plus only the command-side cells
/// when output starts on the same row. Background cards have no command span,
/// so their first row remains the synthetic header target.
pub fn finished_header_end_col(
    has_command: bool,
    prompt_start: usize,
    output_start: Option<usize>,
    output_start_col: usize,
    row: usize,
) -> usize {
    if !has_command {
        return if row == prompt_start { usize::MAX } else { 0 };
    }
    match output_start {
        None => usize::MAX,
        Some(output_row) if row < output_row => usize::MAX,
        Some(output_row) if row == output_row => output_start_col,
        Some(_) => 0,
    }
}

/// Family minimum for the live input/running-command surface. Frost maps this
/// to paint metadata only; it never inserts terminal rows or resizes the PTY.
pub const MIN_INPUT_ROWS: usize = 6;

/// Visible slice of the live input/running card in absolute buffer rows.
/// `real_top`/`real_bottom` say whether the corresponding target edge is
/// actually visible, rather than manufactured by viewport clipping.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg(test)]
pub struct VisibleActiveSpan {
    pub start: usize,
    pub end: usize,
    pub real_top: bool,
    pub real_bottom: bool,
}

/// Project the family-sized live card into one viewport. Its target end grows
/// with output/cursor movement but stays at least [`MIN_INPUT_ROWS`] below the
/// prompt. The terminal/grid end remains a hard limit because this is visual
/// chrome over Frost's existing continuous grid.
#[cfg(test)]
pub fn visible_active_span(
    active_start: usize,
    cursor_absolute_row: usize,
    terminal_end: usize,
    viewport_start: usize,
    viewport_end: usize,
) -> Option<VisibleActiveSpan> {
    let target_end = active_start
        .saturating_add(MIN_INPUT_ROWS)
        .max(cursor_absolute_row.saturating_add(1))
        .min(terminal_end);
    let start = active_start.max(viewport_start);
    let end = target_end.min(viewport_end);
    (start < end).then_some(VisibleActiveSpan {
        start,
        end,
        real_top: start == active_start,
        real_bottom: end == target_end,
    })
}

/// Fence for one Markdown code block: `max(3, longest consecutive-backtick
/// run in the body + 1)` backticks (anvil's `markdown_fence` rule), so a body
/// containing ``` still round-trips as a single fenced block.
pub fn markdown_fence(body: &str) -> String {
    let mut longest = 0usize;
    let mut current = 0usize;
    for ch in body.chars() {
        if ch == '`' {
            current += 1;
            longest = longest.max(current);
        } else {
            current = 0;
        }
    }
    "`".repeat(longest.saturating_add(1).max(3))
}

/// One fenced code section (ember's `fenced`, byte-identical): per-body
/// fence, body normalized to exactly one trailing newline before the closing
/// fence (no doubling), and an empty body collapsing to an empty fence pair
/// (`{fence}\n{fence}`) rather than fencing a spurious blank line.
fn fenced(body: &str) -> String {
    let fence = markdown_fence(body);
    let body = body.strip_suffix('\n').unwrap_or(body);
    if body.is_empty() {
        format!("{fence}\n{fence}")
    } else {
        format!("{fence}\n{body}\n{fence}")
    }
}

/// Render untrusted single-line metadata as an inert Markdown code span.
/// OSC-provided cwd values may contain link/image/HTML syntax; a delimiter
/// longer than every backtick run keeps that syntax data-only. Visual-spoof
/// controls are omitted entirely so exported review text cannot reorder or
/// hide the path around it.
fn markdown_meta_code(value: &str) -> String {
    if crate::review_text::contains_visual_spoofing(value) {
        return "`[unsafe path omitted]`".to_string();
    }
    let mut longest = 0usize;
    let mut current = 0usize;
    for character in value.chars() {
        if character == '`' {
            current += 1;
            longest = longest.max(current);
        } else {
            current = 0;
        }
    }
    let fence = "`".repeat(longest.saturating_add(1).max(1));
    if longest > 0 || value.starts_with(' ') || value.ends_with(' ') {
        format!("{fence} {value} {fence}")
    } else {
        format!("{fence}{value}{fence}")
    }
}

/// Text copied for a whole block (anvil's `block_clipboard_text` family
/// rule): command, newline, output — but a blank/absent output copies the
/// bare command with NO trailing newline, and a background block (no
/// command) copies its output alone. Both blank/absent → `None`, so the
/// caller can toast "Block is empty" instead of writing an empty clipboard.
pub fn block_copy_text(command: Option<&str>, output: Option<&str>) -> Option<String> {
    let command = command.filter(|command| !command.trim().is_empty());
    let output = output.filter(|output| !output.trim().is_empty());
    match (command, output) {
        (Some(command), Some(output)) => Some(format!("{command}\n{output}")),
        (Some(command), None) => Some(command.to_string()),
        (None, Some(output)) => Some(output.to_string()),
        (None, None) => None,
    }
}

/// Same aggregate clipboard ceiling as Forge. Although captured block output
/// is already bounded per zone, a 256-block selection must not turn one copy
/// gesture into an unexpectedly large UI allocation.
pub const SELECTED_CLIPBOARD_MAX_BYTES: usize = 32 * 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SelectedClipboardMode {
    Commands,
    Outputs,
    Blocks,
}

/// Output state supplied to [`selected_clipboard_text`]. `Unavailable` is
/// distinct from `Empty`: silently omitting evicted output would make a
/// multi-block clipboard look complete when it is not.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ClipboardOutput {
    Available(String),
    Empty,
    Unavailable,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SelectedClipboardError {
    Empty,
    OutputUnavailable,
    TooLarge,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SelectedClipboard {
    pub text: String,
    pub block_count: usize,
}

/// Aggregate one whole-block selection in terminal order. Commands use one
/// newline (the family's editable command-list form); output and full blocks
/// use one blank line between contributing blocks, matching their visual card
/// grouping. Output-dependent modes fail atomically when any selected block's
/// previously non-blank output is no longer retained or the byte cap would be
/// exceeded.
pub fn selected_clipboard_text<'a, I>(
    blocks: I,
    selection: &BlockSelection,
    mode: SelectedClipboardMode,
    max_bytes: usize,
) -> Result<SelectedClipboard, SelectedClipboardError>
where
    I: IntoIterator<Item = (u64, Option<&'a str>, ClipboardOutput)>,
{
    let mut text = String::new();
    let mut block_count = 0usize;
    for (id, command, output) in blocks {
        if !selection.contains(id) {
            continue;
        }
        let part = match mode {
            SelectedClipboardMode::Commands => command
                .filter(|command| !command.trim().is_empty())
                .map(str::to_string),
            SelectedClipboardMode::Outputs => match output {
                ClipboardOutput::Available(output) if !output.trim().is_empty() => Some(output),
                ClipboardOutput::Available(_) | ClipboardOutput::Empty => None,
                ClipboardOutput::Unavailable => {
                    return Err(SelectedClipboardError::OutputUnavailable);
                }
            },
            SelectedClipboardMode::Blocks => {
                let output = match output {
                    ClipboardOutput::Available(output) => Some(output),
                    ClipboardOutput::Empty => None,
                    ClipboardOutput::Unavailable => {
                        return Err(SelectedClipboardError::OutputUnavailable);
                    }
                };
                block_copy_text(command, output.as_deref())
            }
        };
        let Some(part) = part else {
            continue;
        };
        let separator = if text.is_empty() {
            ""
        } else if mode == SelectedClipboardMode::Commands {
            "\n"
        } else {
            "\n\n"
        };
        let next_len = text
            .len()
            .checked_add(separator.len())
            .and_then(|length| length.checked_add(part.len()))
            .ok_or(SelectedClipboardError::TooLarge)?;
        if next_len > max_bytes {
            return Err(SelectedClipboardError::TooLarge);
        }
        text.push_str(separator);
        text.push_str(&part);
        block_count += 1;
    }
    if text.is_empty() {
        Err(SelectedClipboardError::Empty)
    } else {
        Ok(SelectedClipboard { text, block_count })
    }
}

/// Join already-rendered Markdown snippets for a whole-block selection in
/// terminal order. A thematic break separates blocks, matching anvil/forge.
/// The aggregation is atomic: exceeding the clipboard cap returns an error
/// instead of copying a misleading prefix of the selection.
pub fn selected_markdown_text<I>(
    blocks: I,
    selection: &BlockSelection,
    max_bytes: usize,
) -> Result<SelectedClipboard, SelectedClipboardError>
where
    I: IntoIterator<Item = (u64, String)>,
{
    let mut text = String::new();
    let mut block_count = 0usize;
    for (id, part) in blocks {
        if !selection.contains(id) {
            continue;
        }
        let separator = if text.is_empty() { "" } else { "\n---\n\n" };
        let next_len = text
            .len()
            .checked_add(separator.len())
            .and_then(|length| length.checked_add(part.len()))
            .ok_or(SelectedClipboardError::TooLarge)?;
        if next_len > max_bytes {
            return Err(SelectedClipboardError::TooLarge);
        }
        text.push_str(separator);
        text.push_str(&part);
        block_count += 1;
    }
    if text.is_empty() {
        Err(SelectedClipboardError::Empty)
    } else {
        Ok(SelectedClipboard { text, block_count })
    }
}

/// Proleptic-Gregorian civil date/time for a unix-epoch millisecond
/// timestamp shifted by `offset_secs` (a fixed UTC offset), via Howard
/// Hinnant's `civil_from_days`. Hand-rolled because this crate deliberately
/// carries no date dependency; the offset is applied to the epoch seconds
/// before the civil math, so day rollover across the offset is exact.
fn civil_from_unix_ms(unix_ms: u64, offset_secs: i32) -> (i64, u32, u32, u32, u32, u32) {
    let secs = (unix_ms / 1000) as i64 + i64::from(offset_secs);
    let days = secs.div_euclid(86_400);
    let secs_of_day = secs.rem_euclid(86_400);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = (if mp < 10 { mp + 3 } else { mp - 9 }) as u32;
    let year = yoe + era * 400 + i64::from(month <= 2);
    let hour = (secs_of_day / 3600) as u32;
    let minute = (secs_of_day % 3600 / 60) as u32;
    let second = (secs_of_day % 60) as u32;
    (year, month, day, hour, minute, second)
}

/// `±HH:MM` rendering of a fixed UTC offset in seconds (`+08:00`, `-05:30`;
/// zero is `+00:00`). Sub-minute offset components are truncated.
fn format_offset(offset_secs: i32) -> String {
    let sign = if offset_secs < 0 { '-' } else { '+' };
    let abs = offset_secs.unsigned_abs();
    format!("{sign}{:02}:{:02}", abs / 3600, (abs % 3600) / 60)
}

/// `YYYY-MM-DD HH:MM:SS ±HH:MM` for a unix-epoch millisecond timestamp at a
/// fixed UTC offset — the Markdown export's "Finished" line (family
/// contract: local time with an explicit offset). Pure: the runtime obtains
/// `offset_secs` from [`local_offset_secs`].
pub fn timestamp_at_offset(unix_ms: u64, offset_secs: i32) -> String {
    let (year, month, day, hour, minute, second) = civil_from_unix_ms(unix_ms, offset_secs);
    format!(
        "{year:04}-{month:02}-{day:02} {hour:02}:{minute:02}:{second:02} {}",
        format_offset(offset_secs)
    )
}

/// Filename-safe local timestamp used by whole-session exports. Kept beside
/// [`timestamp_at_offset`] so export names and Markdown metadata share the
/// exact same civil-time and UTC-offset arithmetic.
pub fn compact_timestamp_at_offset(unix_ms: u64, offset_secs: i32) -> String {
    let (year, month, day, hour, minute, second) = civil_from_unix_ms(unix_ms, offset_secs);
    format!("{year:04}{month:02}{day:02}-{hour:02}{minute:02}{second:02}")
}

/// Bare `HH:MM:SS` for a unix-epoch millisecond timestamp at a fixed UTC
/// offset — the selected block's badge suffix (family contract: local time,
/// no offset marker on the badge).
pub fn clock_at_offset(unix_ms: u64, offset_secs: i32) -> String {
    let (_, _, _, hour, minute, second) = civil_from_unix_ms(unix_ms, offset_secs);
    format!("{hour:02}:{minute:02}:{second:02}")
}

/// The local timezone's UTC offset in seconds at the given unix-epoch
/// second, via `localtime_r` (so DST is resolved for that instant, not for
/// "now"). Falls back to 0 (UTC) if libc cannot resolve the local time.
pub fn local_offset_secs(unix_secs: i64) -> i32 {
    let time = unix_secs as libc::time_t;
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    let result = unsafe { libc::localtime_r(&time, &mut tm) };
    if result.is_null() {
        0
    } else {
        tm.tm_gmtoff as i32
    }
}

/// Everything the Markdown export needs from one completed zone. Borrowed so
/// callers hand over the zone fields and extracted output without cloning.
pub struct MarkdownBlock<'a> {
    pub command: Option<&'a str>,
    pub output: &'a str,
    /// The output extraction hit its byte cap
    /// (`TerminalState::zone_output_text_capped`), so `output` is not the
    /// whole story; emits the `- Note: output truncated` meta line.
    pub output_truncated: bool,
    pub exit_code: Option<i32>,
    pub duration_ms: Option<u64>,
    pub finished_at_ms: Option<u64>,
    /// Fixed UTC offset (seconds) the "Finished" line is rendered at; the
    /// runtime passes [`local_offset_secs`] for the finish instant.
    pub tz_offset_secs: i32,
    pub cwd: Option<&'a str>,
}

/// Render one block as the family's shared Markdown snippet (ember ships the
/// same format — a change here must change there too). Background zones omit
/// the Exit line and the whole "Command:" section; unknown duration /
/// finish time / cwd omit their lines. The contract also defines a
/// `- Note: command reconstructed from screen` meta line (after the
/// truncation note), but frost never emits it: frost commands come only from
/// OSC 133 metadata / prompt extraction at `C`, never from a screen-scrape
/// reconstruction — that line is ember-only.
#[cfg(test)]
fn markdown_export(block: &MarkdownBlock<'_>) -> String {
    markdown_export_with_state(block, false, false, true)
}

/// Markdown export with lifecycle/capture facts that are stored beside the
/// family's shared block fields. These notes keep a retained-but-incomplete
/// OSC 133 lifecycle, a shell-truncated command, or output lost to both
/// retention budgets from looking like complete data.
pub fn markdown_export_with_state(
    block: &MarkdownBlock<'_>,
    command_truncated: bool,
    output_unavailable: bool,
    completion_observed: bool,
) -> String {
    let background = matches!(
        classify(block.command, block.exit_code),
        BlockOutcome::Background
    );
    let mut out = String::from("## Command Block\n");

    let mut meta = String::new();
    if !background {
        let exit = match block.exit_code {
            None => "not reported".to_string(),
            Some(code) => match jterm_core::exit_status::signal_name_for_exit(code) {
                Some(signal) => format!("{code} {signal}"),
                None => code.to_string(),
            },
        };
        meta.push_str(&format!("- Exit: {exit}\n"));
    }
    if let Some(ms) = block.duration_ms {
        meta.push_str(&format!("- Duration: {}\n", format_duration(ms)));
    }
    if let Some(ms) = block.finished_at_ms {
        meta.push_str(&format!(
            "- Finished: {}\n",
            timestamp_at_offset(ms, block.tz_offset_secs)
        ));
    }
    if let Some(cwd) = block.cwd {
        // Meta-line values must stay single-line/control-free or they break
        // the line-oriented meta block. Ingest guarantees it: the OSC 133
        // cwd/cwd_url params and the OSC 7 fallback are both rejected on any
        // control character before they reach a zone.
        debug_assert!(
            !cwd.chars().any(char::is_control),
            "zone cwd must be control-free (ingest guarantees it)"
        );
        meta.push_str(&format!("- Cwd: {}\n", markdown_meta_code(cwd)));
    }
    if output_unavailable {
        meta.push_str("- Note: output unavailable (snapshot and scrollback evicted)\n");
    } else if block.output_truncated {
        meta.push_str("- Note: output truncated\n");
    }
    if command_truncated {
        meta.push_str("- Note: command truncated or unavailable\n");
    }
    // Background output was never a command lifecycle, so there is no command
    // completion marker to be missing. Keep this note for incomplete command
    // zones only.
    if !background && !completion_observed {
        meta.push_str("- Note: command completion not observed\n");
    }
    if !meta.is_empty() {
        out.push('\n');
        out.push_str(&meta);
    }

    if !background {
        let command = block.command.unwrap_or_default();
        out.push_str(&format!("\nCommand:\n\n{}\n", fenced(command)));
    }
    out.push_str(&format!("\nOutput:\n\n{}\n", fenced(block.output)));
    out
}

/// Keyboard block navigation over completed zones (`ids` oldest-first, the
/// same set the card gestures select). Up with no live selection enters at the
/// newest block; Down passes through. Up clamps (and remains owned) at the
/// oldest block, while Down past the newest clears selection.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SelectionNavigation {
    /// Block navigation does not own this key; retain ordinary terminal scroll.
    Passthrough,
    /// Replace the selection with this block and consume the key.
    Select(u64),
    /// Clear the selection and consume the key.
    Clear,
}

pub fn selection_navigation(ids: &[u64], current: Option<u64>, older: bool) -> SelectionNavigation {
    let Some(&newest) = ids.last() else {
        return SelectionNavigation::Passthrough;
    };
    let Some(position) = current.and_then(|id| ids.iter().position(|&zone| zone == id)) else {
        return if older {
            SelectionNavigation::Select(newest)
        } else {
            SelectionNavigation::Passthrough
        };
    };
    if older {
        // The oldest block clamps and still consumes the key; falling through
        // here would unexpectedly scroll while block selection is active.
        SelectionNavigation::Select(ids[position.saturating_sub(1)])
    } else if position + 1 < ids.len() {
        SelectionNavigation::Select(ids[position + 1])
    } else {
        // Moving down past the newest block exits selection mode, matching the
        // anvil/forge history-canvas contract.
        SelectionNavigation::Clear
    }
}

/// Step the block selection across FAILED zones only (`zones` oldest-first as
/// `(id, is_failed)` — the same [`classify`]-based predicate the scrollbar
/// markers use). No selection, or a dangling one, enters from the edge in the
/// requested direction (newest for `older`, oldest for newer). From a live
/// selection — failed or not — the step goes to the nearest failed zone
/// strictly older/newer, wrapping to the far edge. `None` therefore means
/// there are no failed zones at all. This matches anvil/forge's shared marked-
/// block navigation contract.
pub fn select_failed_neighbor(
    zones: &[(u64, bool)],
    current: Option<u64>,
    older: bool,
) -> Option<u64> {
    let oldest_failed = || zones.iter().find(|&&(_, failed)| failed).map(|&(id, _)| id);
    let newest_failed = || {
        zones
            .iter()
            .rev()
            .find(|&&(_, failed)| failed)
            .map(|&(id, _)| id)
    };
    let position = current.and_then(|id| zones.iter().position(|&(zone, _)| zone == id));
    if older {
        position
            .and_then(|position| {
                zones[..position]
                    .iter()
                    .rev()
                    .find(|&&(_, failed)| failed)
                    .map(|&(id, _)| id)
            })
            .or_else(newest_failed)
    } else {
        position
            .and_then(|position| {
                zones[position + 1..]
                    .iter()
                    .find(|&&(_, failed)| failed)
                    .map(|&(id, _)| id)
            })
            .or_else(oldest_failed)
    }
}

/// Hard cap on the hits one block search returns; the scan stops early once
/// it is reached (the query can always be refined).
pub const BLOCK_SEARCH_HIT_CAP: usize = 500;

/// Maximum original-case command/output payload retained while a block-search
/// index is prepared. Sources are collected newest-first, so hitting this cap
/// drops older zones before they can accumulate unbounded live-row fallbacks.
pub const BLOCK_SEARCH_SOURCE_MAX_BYTES: usize = 8 * 1024 * 1024;

/// Maximum heap resident bytes of a completed block-search cache. This counts
/// every original/lowercase String allocation plus the cache Vec allocation;
/// query scans therefore have a stable upper bound independent of scrollback.
pub const BLOCK_SEARCH_CACHE_MAX_BYTES: usize = 16 * 1024 * 1024;

/// Character cap of a hit's matching-line preview.
pub const BLOCK_SEARCH_LINE_CHARS: usize = 200;

/// Character cap of a hit's command preview.
pub const BLOCK_SEARCH_COMMAND_CHARS: usize = 80;

/// One row of the cross-block search picker.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BlockSearchHit {
    pub zone_id: u64,
    /// False when the match is on the command line itself.
    pub is_output_line: bool,
    /// 1-based output line number of the match; 0 for command-line hits
    /// (`is_output_line` is the discriminator).
    pub line_no: usize,
    /// Character range of the first match in the complete, unclipped logical
    /// line. The range is 0-based, end-exclusive, and counts Unicode scalar
    /// values from the original-case text (not bytes in its lowercase cache).
    /// Filter-only browse rows carry `None` because no query was matched.
    pub match_span: Option<std::ops::Range<usize>>,
    /// The matching line, clipped to [`BLOCK_SEARCH_LINE_CHARS`].
    pub line_text: String,
    /// The zone's command line, clipped to [`BLOCK_SEARCH_COMMAND_CHARS`]
    /// (empty for background zones).
    pub command_preview: String,
}

/// Owned, original-case input to the background block-search cache builder.
/// The UI thread extracts only a bounded newest-first set; lowercasing happens
/// after this value has moved to a worker.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BlockSearchSource {
    pub zone_id: u64,
    pub command: Option<String>,
    pub output: Option<String>,
}

impl BlockSearchSource {
    pub fn new(zone_id: u64, command: Option<String>, output: Option<String>) -> Self {
        Self {
            zone_id,
            command,
            output,
        }
    }

    fn resident_bytes(&self) -> usize {
        self.command.as_ref().map_or(0, String::capacity)
            + self.output.as_ref().map_or(0, String::capacity)
    }
}

/// Bounded source snapshot, stored newest-first so both source and cache
/// budgets always preserve the most recent command blocks.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BlockSearchSourceSnapshot {
    pub sources: Vec<BlockSearchSource>,
    pub older_not_indexed: bool,
    pub resident_bytes: usize,
}

/// Consume a lazy newest-first source stream until its heap payload budget is
/// full. Laziness matters: callers may extract a zone from live terminal rows,
/// and older zones beyond the budget must never be extracted speculatively.
pub fn bounded_block_search_sources(
    sources_newest_first: impl IntoIterator<Item = BlockSearchSource>,
    max_bytes: usize,
) -> BlockSearchSourceSnapshot {
    let mut snapshot = BlockSearchSourceSnapshot::default();
    for source in sources_newest_first {
        let bytes = source.resident_bytes();
        if snapshot.resident_bytes.saturating_add(bytes) > max_bytes {
            snapshot.older_not_indexed = true;
            break;
        }
        snapshot.resident_bytes += bytes;
        snapshot.sources.push(source);
    }
    snapshot
}

/// One zone's precomputed text for [`search_blocks`] (ember's
/// `CachedBlockSearchRecord`). Built ONCE per picker-open — the picker
/// closes on any session churn, so the cache can never outlive its session
/// — and each keystroke then only rescans these strings instead of
/// re-extracting terminal output. The lowercase copies are precomputed here
/// so a query run allocates nothing beyond its needle and its hits.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CachedBlockSearchZone {
    pub zone_id: u64,
    /// The zone's command, original case; `None` for background zones
    /// (absent or blank command).
    pub command: Option<String>,
    /// Lowercased `command`, for case-insensitive matching.
    pub command_lowercase: Option<String>,
    /// The zone's output, original case (the captured-snapshot-first
    /// extraction — exactly what the copy paths would read); `None` when it
    /// has none.
    pub output: Option<String>,
    /// Lowercased `output`. Unicode lowercasing never adds or removes line
    /// breaks, so `output.lines()` and `output_lowercase.lines()` stay in
    /// step and hits can report original-case line text.
    pub output_lowercase: Option<String>,
}

impl CachedBlockSearchZone {
    /// Normalize one zone at cache-build time: a blank command counts as
    /// none (background zone), and the lowercase copies are precomputed.
    #[cfg(test)]
    pub fn new(zone_id: u64, command: Option<&str>, output: Option<String>) -> Self {
        Self::from_source(BlockSearchSource::new(
            zone_id,
            command.map(str::to_string),
            output,
        ))
    }

    fn from_source(source: BlockSearchSource) -> Self {
        let command = source.command.filter(|command| !command.trim().is_empty());
        let command_lowercase = command.as_deref().map(str::to_lowercase);
        let output_lowercase = source.output.as_deref().map(str::to_lowercase);
        Self {
            zone_id: source.zone_id,
            command,
            command_lowercase,
            output: source.output,
            output_lowercase,
        }
    }

    fn resident_bytes(&self) -> usize {
        self.command.as_ref().map_or(0, String::capacity)
            + self.command_lowercase.as_ref().map_or(0, String::capacity)
            + self.output.as_ref().map_or(0, String::capacity)
            + self.output_lowercase.as_ref().map_or(0, String::capacity)
    }
}

/// Result installed after the background lowercasing pass. `zones` retains
/// the search engine's historical oldest-first order even though both budgets
/// preferentially keep newest sources.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BlockSearchCacheBuild {
    pub zones: Vec<CachedBlockSearchZone>,
    pub older_not_indexed: bool,
    pub resident_bytes: usize,
}

/// Build the lowercase cache under an exact retained-heap budget. This is a
/// pure CPU pass intended for `spawn_blocking`; original Strings move into the
/// cache instead of being cloned a second time.
pub fn build_block_search_cache(
    snapshot: BlockSearchSourceSnapshot,
    max_bytes: usize,
) -> BlockSearchCacheBuild {
    let zones = Vec::with_capacity(snapshot.sources.len());
    let vec_bytes = zones
        .capacity()
        .saturating_mul(std::mem::size_of::<CachedBlockSearchZone>());
    let mut build = BlockSearchCacheBuild {
        older_not_indexed: snapshot.older_not_indexed,
        resident_bytes: vec_bytes,
        zones,
    };
    if vec_bytes > max_bytes {
        build.older_not_indexed |= !snapshot.sources.is_empty();
        return build;
    }

    for source in snapshot.sources {
        let zone = CachedBlockSearchZone::from_source(source);
        let bytes = zone.resident_bytes();
        if build.resident_bytes.saturating_add(bytes) > max_bytes {
            build.older_not_indexed = true;
            break;
        }
        build.resident_bytes += bytes;
        build.zones.push(zone);
    }
    build.zones.reverse();
    build
}

/// A finished block search: hits (newest zones first) plus whether the
/// [`BLOCK_SEARCH_HIT_CAP`] stopped the scan with matches unreported.
pub struct BlockSearchResults {
    pub hits: Vec<BlockSearchHit>,
    pub capped: bool,
}

/// Clip `text` to `max_chars` characters, marking the cut with an ellipsis.
fn clipped(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        text.to_string()
    } else {
        let mut short: String = text.chars().take(max_chars.saturating_sub(1)).collect();
        short.push('…');
        short
    }
}

/// Map one match in a cached lowercase line back to a character range in the
/// original line. Unicode lowercasing may expand one scalar (`İ` ->
/// `i̇`), so byte or character offsets in `lowercase` cannot be copied
/// directly into `text`.
fn original_match_span(
    text: &str,
    lowercase: &str,
    needle: &str,
) -> Option<std::ops::Range<usize>> {
    let lower_start = lowercase.find(needle)?;
    let lower_end = lower_start.checked_add(needle.len())?;
    let mut lower_cursor = 0usize;
    let mut original_start = None;
    let mut original_end = 0usize;

    for (index, character) in text.chars().enumerate() {
        let expanded_bytes: usize = character.to_lowercase().map(char::len_utf8).sum();
        let next = lower_cursor.saturating_add(expanded_bytes);
        if next > lower_start && lower_cursor < lower_end {
            original_start.get_or_insert(index);
            original_end = index + 1;
        }
        lower_cursor = next;
        if lower_cursor >= lower_end && original_start.is_some() {
            break;
        }
    }

    original_start.map(|start| start..original_end)
}

/// Clip a matching line around the match instead of blindly keeping its
/// prefix. This guarantees a long-line result actually shows why it matched.
fn clipped_around_match(
    text: &str,
    match_span: &std::ops::Range<usize>,
    max_chars: usize,
) -> String {
    let total = text.chars().count();
    if total <= max_chars {
        return text.to_string();
    }
    if max_chars == 0 {
        return String::new();
    }
    if max_chars <= 2 {
        return "…".to_string();
    }
    let inner = max_chars - 2;
    let match_start = match_span.start.min(total);
    let match_end = match_span.end.max(match_start).min(total);
    let visible_match = match_end.saturating_sub(match_start).min(inner);
    let context = inner.saturating_sub(visible_match);
    let mut start = match_start
        .saturating_sub(context / 3)
        .min(total.saturating_sub(inner));
    let mut end = start.saturating_add(inner).min(total);
    // A long needle may consume the whole preview. Prefer its beginning, but
    // never let an otherwise-short match fall outside the chosen window.
    if visible_match < inner && end < match_end {
        end = match_end;
        start = end.saturating_sub(inner);
    }
    // Spend the second ellipsis' character budget on content when the window
    // naturally reaches either edge of the line.
    if start == 0 {
        end = end.saturating_add(1).min(total);
    } else if end == total {
        start = start.saturating_sub(1);
    }
    let mut clipped = String::new();
    if start > 0 {
        clipped.push('…');
    }
    clipped.extend(text.chars().skip(start).take(end - start));
    if end < total {
        clipped.push('…');
    }
    clipped
}

/// Case-insensitive substring search across every cached zone's command
/// line and output lines. `cache` comes oldest-first (the zone deque's
/// order); hits are emitted newest zones first, a zone's command hit before
/// its output hits, output hits in line order. Matching runs entirely
/// against the precomputed lowercase copies (ember's per-open cache design)
/// — beyond the lowercased needle, allocations happen only for hits. A
/// blank query matches nothing — an empty picker, not the whole history.
pub fn search_blocks(cache: &[CachedBlockSearchZone], query: &str) -> BlockSearchResults {
    search_blocks_filtered(cache, query, |_| true)
}

/// [`search_blocks`] with a zero-allocation zone predicate. The UI uses this
/// for outcome/slow/bookmark filters without cloning cached multi-megabyte
/// output strings or letting excluded newer zones consume the hit cap.
pub fn search_blocks_filtered(
    cache: &[CachedBlockSearchZone],
    query: &str,
    mut include_zone: impl FnMut(u64) -> bool,
) -> BlockSearchResults {
    let needle = query.trim().to_lowercase();
    let mut results = BlockSearchResults {
        hits: Vec::new(),
        capped: false,
    };
    if needle.is_empty() {
        return results;
    }
    'zones: for zone in cache.iter().rev() {
        if !include_zone(zone.zone_id) {
            continue;
        }
        // Shared by the zone's hits; computed only when one exists.
        let mut command_preview: Option<String> = None;
        let mut preview = |command: Option<&str>| -> String {
            command_preview
                .get_or_insert_with(|| {
                    clipped(command.unwrap_or_default(), BLOCK_SEARCH_COMMAND_CHARS)
                })
                .clone()
        };
        let command_hit = zone
            .command_lowercase
            .as_deref()
            .zip(zone.command.as_deref())
            .into_iter()
            .filter_map(|(lowercase, command)| {
                original_match_span(command, lowercase, &needle)
                    .map(|span| (false, 0usize, command, span))
            });
        let output_lines = zone.output.as_deref().into_iter().flat_map(str::lines);
        let lowercase_lines = zone
            .output_lowercase
            .as_deref()
            .into_iter()
            .flat_map(str::lines);
        let output_hits = output_lines.zip(lowercase_lines).enumerate().filter_map(
            |(index, (line, lowercase))| {
                original_match_span(line, lowercase, &needle)
                    .map(|span| (true, index + 1, line, span))
            },
        );
        for (is_output_line, line_no, line, match_span) in command_hit.chain(output_hits) {
            if results.hits.len() >= BLOCK_SEARCH_HIT_CAP {
                results.capped = true;
                break 'zones;
            }
            let command_preview = preview(zone.command.as_deref());
            results.hits.push(BlockSearchHit {
                zone_id: zone.zone_id,
                is_output_line,
                line_no,
                line_text: clipped_around_match(line, &match_span, BLOCK_SEARCH_LINE_CHARS),
                match_span: Some(match_span),
                command_preview,
            });
        }
    }
    results
}

/// Scrollbar-track fractions (0.0 = top of the buffer, approaching 1.0 at the
/// bottom) for failed-zone markers: each zone's first absolute row over the
/// total buffer rows (scrollback + grid). Zones are absolute rows so this is
/// exact, not an approximation.
pub fn marker_fractions(rows: &[usize], total_rows: usize) -> Vec<f32> {
    if total_rows == 0 {
        return Vec::new();
    }
    rows.iter()
        .map(|&row| (row as f32 / total_rows as f32).clamp(0.0, 1.0))
        .collect()
}

/// True when the last `needed` cells of a row are blank, so the right-aligned
/// badge can be painted there without covering any text.
pub fn badge_fits(row: &[char], needed: usize) -> bool {
    needed <= row.len()
        && row[row.len() - needed..]
            .iter()
            .all(|&ch| ch == ' ' || ch == '\0')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classification_never_promotes_a_missing_exit_to_success() {
        assert_eq!(classify(Some("ls"), Some(0)), BlockOutcome::Success);
        assert_eq!(classify(Some("ls"), Some(2)), BlockOutcome::Failed(2));
        assert_eq!(classify(Some("ls"), None), BlockOutcome::Unknown);
        // Empty or absent command is background regardless of any exit code.
        assert_eq!(classify(None, Some(0)), BlockOutcome::Background);
        assert_eq!(classify(Some(""), Some(1)), BlockOutcome::Background);
        assert_eq!(classify(Some("   "), None), BlockOutcome::Background);
        assert_eq!(
            classify(None, Some(127)),
            BlockOutcome::Background,
            "a raw non-zero status cannot turn background output into a failure"
        );
    }

    #[test]
    fn duration_format_pins_the_family_contract() {
        assert_eq!(format_duration(743), "743ms");
        assert_eq!(format_duration(999), "999ms");
        assert_eq!(format_duration(1_000), "1.0s");
        assert_eq!(format_duration(12_300), "12.3s");
        assert_eq!(format_duration(59_999), "60.0s");
        assert_eq!(format_duration(60_000), "1m");
        assert_eq!(format_duration(92_000), "1m32s");
        assert_eq!(format_duration(3_600_000), "1h");
        assert_eq!(format_duration(2 * 3_600_000 + 5 * 60_000), "2h05m");
    }

    #[test]
    fn badge_text_covers_all_outcomes() {
        assert_eq!(
            badge_text(BlockOutcome::Success, Some(1_200)).as_deref(),
            Some("✓ 1.2s")
        );
        assert_eq!(
            badge_text(BlockOutcome::Success, None).as_deref(),
            Some("✓")
        );
        assert_eq!(
            badge_text(BlockOutcome::Failed(130), Some(2_300)).as_deref(),
            Some("✗ exit:130 SIGINT · 2.3s")
        );
        assert_eq!(
            badge_text(BlockOutcome::Failed(1), None).as_deref(),
            Some("✗ exit:1")
        );
        // Unknown shows no exit number at all — nothing was reported.
        assert_eq!(
            badge_text(BlockOutcome::Unknown, Some(500)).as_deref(),
            Some("? exit:?")
        );
        assert_eq!(
            badge_text(BlockOutcome::Background, Some(500)).as_deref(),
            Some("↻ Background")
        );
    }

    #[test]
    fn running_badge_is_compact_and_never_looks_completed() {
        assert_eq!(running_badge_text(0), "▶ 0ms");
        assert_eq!(running_badge_text(1_250), "▶ 1.2s");
        assert_eq!(running_badge_text(92_000), "▶ 1m32s");
        assert!(!running_badge_text(1_250).contains('✓'));
        assert!(!running_badge_text(1_250).contains('✗'));
    }

    #[test]
    fn spans_end_at_the_next_prompt_and_the_live_boundary() {
        assert_eq!(spans(&[10, 20, 35], 50), vec![(10, 20), (20, 35), (35, 50)]);
        // A single block is closed by the live boundary alone.
        assert_eq!(spans(&[7], 9), vec![(7, 9)]);
        assert_eq!(spans(&[], 9), Vec::<(usize, usize)>::new());
        // A boundary that never precedes its start (clamped, not panicking).
        assert_eq!(spans(&[5], 3), vec![(5, 5)]);
    }

    #[test]
    fn finished_header_hit_covers_wrapped_command_and_stops_at_same_row_output() {
        // A wrapped command occupies all rows before output and only the first
        // five cells of the physical row where output begins.
        assert_eq!(
            finished_header_end_col(true, 10, Some(13), 5, 10),
            usize::MAX
        );
        assert_eq!(
            finished_header_end_col(true, 10, Some(13), 5, 12),
            usize::MAX
        );
        assert_eq!(finished_header_end_col(true, 10, Some(13), 5, 13), 5);
        assert_eq!(finished_header_end_col(true, 10, Some(13), 5, 14), 0);
        assert_eq!(finished_header_end_col(true, 10, Some(10), 7, 10), 7);

        // No-output commands remain header-selectable throughout their card;
        // background output retains just its first-row header target.
        assert_eq!(finished_header_end_col(true, 10, None, 0, 14), usize::MAX);
        assert_eq!(
            finished_header_end_col(false, 10, Some(10), 0, 10),
            usize::MAX
        );
        assert_eq!(finished_header_end_col(false, 10, Some(10), 0, 11), 0);
    }

    #[test]
    fn active_span_is_six_rows_grows_with_cursor_and_preserves_clip_edges() {
        assert_eq!(
            visible_active_span(10, 10, 100, 0, 30),
            Some(VisibleActiveSpan {
                start: 10,
                end: 16,
                real_top: true,
                real_bottom: true,
            })
        );
        assert_eq!(
            visible_active_span(10, 20, 100, 0, 30),
            Some(VisibleActiveSpan {
                start: 10,
                end: 21,
                real_top: true,
                real_bottom: true,
            })
        );

        // Neither viewport boundary is allowed to masquerade as a card edge.
        assert_eq!(
            visible_active_span(10, 10, 100, 12, 15),
            Some(VisibleActiveSpan {
                start: 12,
                end: 15,
                real_top: false,
                real_bottom: false,
            })
        );
        assert_eq!(
            visible_active_span(10, 10, 100, 12, 30),
            Some(VisibleActiveSpan {
                start: 12,
                end: 16,
                real_top: false,
                real_bottom: true,
            })
        );

        // Near the terminal bottom the existing grid is the hard limit; no
        // synthetic rows are introduced to satisfy the visual minimum.
        assert_eq!(
            visible_active_span(97, 97, 100, 90, 100),
            Some(VisibleActiveSpan {
                start: 97,
                end: 100,
                real_top: true,
                real_bottom: true,
            })
        );
        assert_eq!(visible_active_span(10, 10, 100, 30, 40), None);
    }

    #[test]
    fn markdown_fence_always_outruns_the_body() {
        assert_eq!(markdown_fence("plain text"), "```");
        assert_eq!(markdown_fence(""), "```");
        // A body containing a three-backtick run gets a four-backtick fence.
        assert_eq!(markdown_fence("code ``` inside"), "````");
        assert_eq!(markdown_fence("````"), "`````");
        // Runs reset on non-backtick characters; two short runs stay at 3.
        assert_eq!(markdown_fence("`` a ``"), "```");
    }

    #[test]
    fn block_copy_text_follows_the_anvil_clipboard_rule() {
        assert_eq!(
            block_copy_text(Some("echo ok"), Some("ok")).as_deref(),
            Some("echo ok\nok")
        );
        // Blank/absent output: the bare command, NO trailing newline.
        assert_eq!(block_copy_text(Some("true"), None).as_deref(), Some("true"));
        assert_eq!(
            block_copy_text(Some("true"), Some("   \n")).as_deref(),
            Some("true")
        );
        // Background zones (no command) copy output alone.
        assert_eq!(
            block_copy_text(None, Some("worker done")).as_deref(),
            Some("worker done")
        );
        // Nothing on either side: nothing to copy (caller toasts).
        assert_eq!(block_copy_text(None, None), None);
        assert_eq!(block_copy_text(Some("  "), Some("\n")), None);
    }

    #[test]
    fn selected_clipboard_copy_is_ordered_grouped_and_background_aware() {
        use ClipboardOutput::{Available, Empty};
        use SelectedClipboardMode::{Blocks, Commands, Outputs};

        let ids = [1, 2, 3, 4];
        let mut selection = BlockSelection::default();
        selection.select_all(&ids);
        let source = || {
            [
                (1, Some("first"), Available("one".to_string())),
                (2, None, Available("background".to_string())),
                (3, Some("third"), Empty),
                (4, Some("  "), Empty),
            ]
        };

        assert_eq!(
            selected_clipboard_text(source(), &selection, Commands, 1024),
            Ok(SelectedClipboard {
                text: "first\nthird".to_string(),
                block_count: 2,
            })
        );
        assert_eq!(
            selected_clipboard_text(source(), &selection, Outputs, 1024),
            Ok(SelectedClipboard {
                text: "one\n\nbackground".to_string(),
                block_count: 2,
            })
        );
        assert_eq!(
            selected_clipboard_text(source(), &selection, Blocks, 1024),
            Ok(SelectedClipboard {
                text: "first\none\n\nbackground\n\nthird".to_string(),
                block_count: 3,
            })
        );
    }

    #[test]
    fn selected_clipboard_copy_is_bounded_and_output_atomic() {
        use ClipboardOutput::{Available, Unavailable};
        use SelectedClipboardError::{OutputUnavailable, TooLarge};
        use SelectedClipboardMode::{Blocks, Commands, Outputs};

        let mut selection = BlockSelection::default();
        selection.select_all(&[1, 2]);
        let unavailable = || {
            [
                (1, Some("first"), Available("one".to_string())),
                (2, Some("second"), Unavailable),
            ]
        };
        assert_eq!(
            selected_clipboard_text(unavailable(), &selection, Outputs, 1024),
            Err(OutputUnavailable)
        );
        assert_eq!(
            selected_clipboard_text(unavailable(), &selection, Blocks, 1024),
            Err(OutputUnavailable)
        );
        // Copying commands does not depend on retained output.
        assert_eq!(
            selected_clipboard_text(unavailable(), &selection, Commands, 12),
            Ok(SelectedClipboard {
                text: "first\nsecond".to_string(),
                block_count: 2,
            })
        );
        assert_eq!(
            selected_clipboard_text(unavailable(), &selection, Commands, 11),
            Err(TooLarge)
        );
    }

    #[test]
    fn selected_markdown_copy_is_ordered_separated_and_bounded() {
        let mut selection = BlockSelection::default();
        // Deliberately select out of insertion order: source/terminal order wins.
        selection.click(&[1, 2, 3], 2, true, true);
        selection.click(&[1, 2, 3], 1, true, true);
        let source = || {
            [
                (1, "## Command Block\n\nfirst\n".to_string()),
                (2, "## Command Block\n\nsecond\n".to_string()),
                (3, "ignored".to_string()),
            ]
        };

        assert_eq!(
            selected_markdown_text(source(), &selection, 1024),
            Ok(SelectedClipboard {
                text: "## Command Block\n\nfirst\n\n---\n\n## Command Block\n\nsecond\n"
                    .to_string(),
                block_count: 2,
            })
        );
        assert_eq!(
            selected_markdown_text(source(), &selection, 40),
            Err(SelectedClipboardError::TooLarge)
        );
        assert_eq!(
            selected_markdown_text(source(), &BlockSelection::default(), 1024),
            Err(SelectedClipboardError::Empty)
        );
    }

    #[test]
    fn timestamps_pin_fixed_offsets_including_day_rollover() {
        // date -u -d @1234567890 => Fri Feb 13 23:31:30 UTC 2009
        const EPOCH_MS: u64 = 1_234_567_890_123;
        // UTC itself renders as an explicit +00:00.
        assert_eq!(
            timestamp_at_offset(EPOCH_MS, 0),
            "2009-02-13 23:31:30 +00:00"
        );
        assert_eq!(clock_at_offset(EPOCH_MS, 0), "23:31:30");
        // +08:00 rolls the civil date forward past midnight.
        assert_eq!(
            timestamp_at_offset(EPOCH_MS, 8 * 3600),
            "2009-02-14 07:31:30 +08:00"
        );
        assert_eq!(clock_at_offset(EPOCH_MS, 8 * 3600), "07:31:30");
        // A negative half-hour offset (India-style granularity, west of UTC).
        assert_eq!(
            timestamp_at_offset(EPOCH_MS, -(5 * 3600 + 30 * 60)),
            "2009-02-13 18:01:30 -05:30"
        );
        assert_eq!(clock_at_offset(EPOCH_MS, -(5 * 3600 + 30 * 60)), "18:01:30");
        // A negative offset rolls the civil date BACKWARD across midnight.
        // date -u -d @1583020800 => Sun Mar  1 00:00:00 UTC 2020 (leap year).
        assert_eq!(
            timestamp_at_offset(1_583_020_800_000, -(5 * 3600 + 30 * 60)),
            "2020-02-29 18:30:00 -05:30"
        );
        // …and a positive one rolls epoch day zero forward.
        assert_eq!(
            timestamp_at_offset(0, 8 * 3600),
            "1970-01-01 08:00:00 +08:00"
        );
    }

    #[test]
    fn compact_timestamp_is_filename_safe_and_uses_the_same_offset() {
        assert_eq!(compact_timestamp_at_offset(0, 8 * 3600), "19700101-080000");
        assert_eq!(
            compact_timestamp_at_offset(0, -5 * 3600 - 30 * 60),
            "19691231-183000"
        );
    }

    #[test]
    fn markdown_export_pins_the_family_shape() {
        let markdown = markdown_export(&MarkdownBlock {
            command: Some("sleep 99"),
            output: "^C",
            output_truncated: false,
            exit_code: Some(130),
            duration_ms: Some(2_300),
            finished_at_ms: Some(1_234_567_890_123),
            tz_offset_secs: 8 * 3600,
            cwd: Some("/home/user/project"),
        });
        assert_eq!(
            markdown,
            "## Command Block\n\
             \n\
             - Exit: 130 SIGINT\n\
             - Duration: 2.3s\n\
             - Finished: 2009-02-14 07:31:30 +08:00\n\
             - Cwd: `/home/user/project`\n\
             \n\
             Command:\n\
             \n\
             ```\n\
             sleep 99\n\
             ```\n\
             \n\
             Output:\n\
             \n\
             ```\n\
             ^C\n\
             ```\n"
        );
    }

    #[test]
    fn markdown_export_collapses_an_empty_output_to_an_empty_fence_pair() {
        // Empty body → `{fence}\n{fence}`, never a fenced blank line
        // (ember's `fenced` rule, pinned as the whole document).
        let markdown = markdown_export(&MarkdownBlock {
            command: Some("true"),
            output: "",
            output_truncated: false,
            exit_code: Some(0),
            duration_ms: None,
            finished_at_ms: None,
            tz_offset_secs: 0,
            cwd: None,
        });
        assert_eq!(
            markdown,
            "## Command Block\n\
             \n\
             - Exit: 0\n\
             \n\
             Command:\n\
             \n\
             ```\n\
             true\n\
             ```\n\
             \n\
             Output:\n\
             \n\
             ```\n\
             ```\n"
        );
    }

    #[test]
    fn markdown_export_strips_at_most_one_trailing_newline_before_fencing() {
        let block = |output: &'static str| MarkdownBlock {
            command: Some("ls"),
            output,
            output_truncated: false,
            exit_code: Some(0),
            duration_ms: None,
            finished_at_ms: None,
            tz_offset_secs: 0,
            cwd: None,
        };
        // One trailing newline is normalized away (no doubled blank line)…
        assert!(markdown_export(&block("hi\n")).contains("Output:\n\n```\nhi\n```\n"));
        // …but only one: a genuinely blank last line survives.
        assert!(markdown_export(&block("hi\n\n")).contains("Output:\n\n```\nhi\n\n```\n"));
        // A body that is exactly one newline collapses to the empty pair.
        assert!(markdown_export(&block("\n")).contains("Output:\n\n```\n```\n"));
        // The same normalization applies to the Command body.
        let markdown = markdown_export(&MarkdownBlock {
            command: Some("ls\n"),
            ..block("ok")
        });
        assert!(markdown.contains("Command:\n\n```\nls\n```\n"));
    }

    #[test]
    fn markdown_export_notes_truncated_output_after_cwd() {
        let block = |truncated: bool| MarkdownBlock {
            command: Some("yes"),
            output: "y\ny",
            output_truncated: truncated,
            exit_code: Some(0),
            duration_ms: None,
            finished_at_ms: None,
            tz_offset_secs: 0,
            cwd: Some("/srv"),
        };
        let markdown = markdown_export(&block(true));
        assert!(markdown.contains("- Cwd: `/srv`\n- Note: output truncated\n\nCommand:"));
        // The note is absent when nothing was cut.
        assert!(!markdown_export(&block(false)).contains("- Note: output truncated"));
    }

    #[test]
    fn markdown_export_keeps_untrusted_cwd_syntax_inert_and_omits_bidi() {
        let render = |cwd| {
            markdown_export(&MarkdownBlock {
                command: Some("pwd"),
                output: "",
                output_truncated: false,
                exit_code: Some(0),
                duration_ms: None,
                finished_at_ms: None,
                tz_offset_secs: 0,
                cwd: Some(cwd),
            })
        };

        let html = render("<img src=https://example.invalid/pixel>");
        assert!(html.contains("- Cwd: `<img src=https://example.invalid/pixel>`\n"));
        assert!(!html.contains("- Cwd: <img"));

        let image = render("![](https://example.invalid/pixel)");
        assert!(image.contains("- Cwd: `![](https://example.invalid/pixel)`\n"));
        assert!(!image.contains("- Cwd: ![]("));

        let backticks = render("/tmp/`project`");
        assert!(backticks.contains("- Cwd: `` /tmp/`project` ``\n"));

        let bidi = render("/safe/\u{202e}gpj.exe");
        assert!(bidi.contains("- Cwd: `[unsafe path omitted]`\n"));
        assert!(!bidi.contains('\u{202e}'));
    }

    #[test]
    fn markdown_export_states_retention_and_lifecycle_gaps() {
        let block = MarkdownBlock {
            command: Some("very-long-command"),
            output: "",
            output_truncated: false,
            exit_code: None,
            duration_ms: None,
            finished_at_ms: None,
            tz_offset_secs: 0,
            cwd: None,
        };
        let markdown = markdown_export_with_state(&block, true, true, false);
        assert!(markdown.contains("- Note: output unavailable (snapshot and scrollback evicted)\n"));
        assert!(markdown.contains("- Note: command truncated or unavailable\n"));
        assert!(markdown.contains("- Note: command completion not observed\n"));
        assert!(!markdown.contains("- Note: output truncated\n"));
    }

    #[test]
    fn markdown_export_omits_unknown_metadata_lines() {
        let markdown = markdown_export(&MarkdownBlock {
            command: Some("true"),
            output: "",
            output_truncated: false,
            exit_code: None,
            duration_ms: None,
            finished_at_ms: None,
            tz_offset_secs: 0,
            cwd: None,
        });
        assert!(markdown.contains("- Exit: not reported\n"));
        assert!(!markdown.contains("- Duration:"));
        assert!(!markdown.contains("- Finished:"));
        assert!(!markdown.contains("- Cwd:"));
        assert!(!markdown.contains("- Note:"));
    }

    #[test]
    fn markdown_export_background_zone_has_no_command_section_or_exit_line() {
        let markdown = markdown_export_with_state(
            &MarkdownBlock {
                command: None,
                output: "stray output",
                output_truncated: false,
                exit_code: Some(1),
                duration_ms: Some(500),
                finished_at_ms: None,
                tz_offset_secs: 0,
                cwd: None,
            },
            false,
            false,
            false,
        );
        assert_eq!(
            markdown,
            "## Command Block\n\
             \n\
             - Duration: 500ms\n\
             \n\
             Output:\n\
             \n\
             ```\n\
             stray output\n\
             ```\n"
        );
    }

    #[test]
    fn markdown_export_widens_fences_around_backtick_heavy_output() {
        let markdown = markdown_export(&MarkdownBlock {
            command: Some("cat README.md"),
            output: "```rust\nfn main() {}\n```",
            output_truncated: false,
            exit_code: Some(0),
            duration_ms: None,
            finished_at_ms: None,
            tz_offset_secs: 0,
            cwd: None,
        });
        // The output body contains ``` so its fence must be ```` — and the
        // command's fence stays at three (independent bodies, independent
        // fences).
        assert!(markdown.contains("Output:\n\n````\n```rust\nfn main() {}\n```\n````\n"));
        assert!(markdown.contains("Command:\n\n```\ncat README.md\n```\n"));
    }

    #[test]
    fn selection_navigation_orders_clamps_clears_and_passes_through() {
        use SelectionNavigation::{Clear, Passthrough, Select};

        let ids = [10, 20, 30];
        // Ctrl+Up enters selection at the newest block; Ctrl+Down retains its
        // ordinary scroll behavior until a live selection exists.
        assert_eq!(selection_navigation(&ids, None, true), Select(30));
        assert_eq!(selection_navigation(&ids, None, false), Passthrough);
        assert_eq!(selection_navigation(&ids, Some(999), true), Select(30));
        assert_eq!(selection_navigation(&ids, Some(999), false), Passthrough);
        // Stepping moves one zone at a time.
        assert_eq!(selection_navigation(&ids, Some(30), true), Select(20));
        assert_eq!(selection_navigation(&ids, Some(20), true), Select(10));
        assert_eq!(selection_navigation(&ids, Some(10), false), Select(20));
        // Up clamps and consumes at the oldest edge; Down past newest clears.
        assert_eq!(selection_navigation(&ids, Some(10), true), Select(10));
        assert_eq!(selection_navigation(&ids, Some(30), false), Clear);
        // No zones at all: nothing to select.
        assert_eq!(selection_navigation(&[], None, true), Passthrough);
        assert_eq!(selection_navigation(&[], Some(1), false), Passthrough);
    }

    #[test]
    fn block_bookmarks_toggle_clear_and_retain_only_live_ids() {
        let mut bookmarks = BlockBookmarks::default();
        assert!(bookmarks.is_empty());
        assert_eq!(bookmarks.len(), 0);

        assert!(bookmarks.toggle(10));
        assert!(bookmarks.toggle(20));
        assert!(bookmarks.contains(10));
        assert!(bookmarks.contains(20));
        assert_eq!(bookmarks.len(), 2);

        assert!(!bookmarks.toggle(20));
        assert!(!bookmarks.contains(20));
        assert_eq!(bookmarks.len(), 1);

        assert!(bookmarks.toggle(999));
        assert!(bookmarks.retain(&[10, 20, 30]));
        assert!(bookmarks.contains(10));
        assert!(!bookmarks.contains(999));
        assert_eq!(bookmarks.len(), 1);

        assert!(!bookmarks.retain(&[]));
        assert!(bookmarks.is_empty());
        assert!(bookmarks.toggle(30));
        bookmarks.clear();
        assert!(bookmarks.is_empty());
    }

    #[test]
    fn block_bookmark_neighbors_step_wrap_and_remove_stale_ids() {
        let ids = [10, 20, 30, 40, 50];
        let mut bookmarks = BlockBookmarks::default();
        assert!(bookmarks.toggle(20));
        assert!(bookmarks.toggle(40));
        assert!(bookmarks.toggle(999));

        // No current block (or a dangling one) enters from the requested edge.
        assert_eq!(bookmarks.neighbor(&ids, None, true), Some(40));
        assert!(!bookmarks.contains(999));
        assert_eq!(bookmarks.len(), 2);
        assert_eq!(bookmarks.neighbor(&ids, None, false), Some(20));
        assert_eq!(bookmarks.neighbor(&ids, Some(999), true), Some(40));
        assert_eq!(bookmarks.neighbor(&ids, Some(999), false), Some(20));

        // A bookmarked or ordinary current block steps to the nearest mark.
        assert_eq!(bookmarks.neighbor(&ids, Some(40), true), Some(20));
        assert_eq!(bookmarks.neighbor(&ids, Some(20), false), Some(40));
        assert_eq!(bookmarks.neighbor(&ids, Some(30), true), Some(20));
        assert_eq!(bookmarks.neighbor(&ids, Some(30), false), Some(40));

        // Both ends wrap to the far bookmark.
        assert_eq!(bookmarks.neighbor(&ids, Some(20), true), Some(40));
        assert_eq!(bookmarks.neighbor(&ids, Some(40), false), Some(20));
        assert_eq!(bookmarks.neighbor(&ids, Some(10), true), Some(40));
        assert_eq!(bookmarks.neighbor(&ids, Some(50), false), Some(20));

        // One bookmark loops to itself; no live bookmarks produce no target.
        assert!(!bookmarks.toggle(40));
        assert_eq!(bookmarks.neighbor(&ids, Some(20), true), Some(20));
        assert_eq!(bookmarks.neighbor(&ids, Some(20), false), Some(20));
        assert_eq!(bookmarks.neighbor(&[], Some(20), true), None);
        assert!(bookmarks.is_empty());
    }

    #[test]
    fn block_selection_clicks_toggle_and_select_inclusive_ranges() {
        let ids = [10, 20, 30, 40];
        let mut selection = BlockSelection::default();

        selection.click(&ids, 20, false, false);
        assert_eq!(selection.active(), Some(20));
        assert!(selection.contains(20));
        assert_eq!(selection.len(), 1);

        selection.click(&ids, 40, true, true);
        assert_eq!(selection.active(), Some(40));
        assert!(selection.contains(20));
        assert!(selection.contains(40));
        assert_eq!(selection.len(), 2);

        // Ctrl toggling the active edge off falls back to the newest selected
        // id still present in terminal order.
        selection.click(&ids, 40, true, true);
        assert_eq!(selection.active(), Some(20));
        assert_eq!(selection.len(), 1);

        // A fresh plain click fixes the anchor; Shift selects both endpoints
        // and everything between, in either direction.
        selection.click(&ids, 40, false, false);
        selection.click(&ids, 20, false, true);
        assert_eq!(selection.active(), Some(20));
        assert!(selection.contains(20));
        assert!(selection.contains(30));
        assert!(selection.contains(40));
        assert_eq!(selection.len(), 3);

        // Ctrl+Shift is the cross-card toggle gesture; Shift alone owns range.
        selection.click(&ids, 10, false, false);
        selection.click(&ids, 30, false, true);
        assert_eq!(selection.active(), Some(30));
        assert!(selection.contains(10));
        assert!(selection.contains(20));
        assert!(selection.contains(30));
        assert_eq!(selection.len(), 3);

        selection.click(&ids, 20, true, true);
        assert!(!selection.contains(20));
        assert!(selection.contains(10));
        assert!(selection.contains(30));
        assert_eq!(selection.active(), Some(30));

        // Ctrl alone on a header is a replace, not the toggle chord.
        selection.click(&ids, 40, true, false);
        assert_eq!(selection.active(), Some(40));
        assert_eq!(selection.len(), 1);
    }

    #[test]
    fn ctrl_toggle_clears_a_stale_only_remainder() {
        let mut selection = BlockSelection::default();
        selection.click(&[10, 20], 10, false, false);

        // Simulate a missed reconciliation after 10 was evicted, then add and
        // remove a live id. The stale id must not leave an invisible selection.
        selection.click(&[20, 30], 30, true, true);
        selection.click(&[20, 30], 30, true, true);
        assert!(selection.is_empty());
        assert_eq!(selection.active(), None);
    }

    #[test]
    fn context_activation_preserves_selected_range_and_resets_anchor() {
        let ids = [10, 20, 30, 40];
        let mut selection = BlockSelection::default();
        selection.click(&ids, 10, false, false);
        selection.click(&ids, 30, false, true);

        selection.activate(&ids, 20);
        assert_eq!(selection.active(), Some(20));
        assert_eq!(selection.len(), 3);
        assert!(selection.contains(10));
        assert!(selection.contains(20));
        assert!(selection.contains(30));

        // A subsequent Shift gesture starts from the context-clicked card.
        selection.click(&ids, 40, false, true);
        assert_eq!(selection.active(), Some(40));
        assert!(!selection.contains(10));
        assert!(selection.contains(20));
        assert!(selection.contains(30));
        assert!(selection.contains(40));

        // Context-clicking an unselected card collapses to that stable target.
        selection.activate(&ids, 10);
        assert_eq!(selection.active(), Some(10));
        assert_eq!(selection.len(), 1);
    }

    #[test]
    fn select_all_and_shift_steps_preserve_anchor_while_contracting() {
        let ids = [10, 20, 30, 40];
        let mut selection = BlockSelection::default();
        selection.select_all(&ids);
        assert_eq!(selection.active(), Some(40));
        assert_eq!(selection.len(), 4);

        assert_eq!(selection.extend_step(&ids, true), Some(30));
        assert_eq!(selection.active(), Some(30));
        assert_eq!(selection.len(), 3);
        assert!(!selection.contains(40));

        assert_eq!(selection.extend_step(&ids, false), Some(40));
        assert_eq!(selection.len(), 4);
        assert_eq!(selection.extend_step(&ids, false), None);
    }

    #[test]
    fn retaining_live_ids_repairs_or_clears_selection_edges() {
        let ids = [10, 20, 30];
        let mut selection = BlockSelection::default();
        selection.select_all(&ids);
        assert!(selection.retain(&[10, 20]));
        assert_eq!(selection.active(), Some(20));
        assert_eq!(selection.len(), 2);

        // Callers deciding whether block mode owns the next key can use the
        // return value directly; an asynchronously evicted stale-only
        // selection must look empty before key routing.
        assert!(!selection.retain(&[]));
        assert!(selection.is_empty());
        assert_eq!(selection.active(), None);
    }

    #[test]
    fn selected_commands_are_ordered_bounded_and_atomic() {
        let ids = [1, 2, 3, 4];
        let mut selection = BlockSelection::default();
        selection.select_all(&ids);
        let zones = [
            (1, Some("first"), false),
            (2, None, false),
            (3, Some("third"), false),
            (4, Some("  "), false),
        ];
        assert_eq!(
            selected_commands(zones, &selection, 256),
            Ok(SelectedCommands {
                text: "first\nthird".to_string(),
                // The selected background and whitespace-only blocks do not
                // contribute to either the payload or its success count.
                block_count: 2,
            })
        );

        assert_eq!(
            selected_commands(
                [(1, Some("first"), false), (3, Some("third"), true)],
                &selection,
                256
            ),
            Err(SelectedCommandsError::Truncated)
        );
        assert_eq!(
            selected_commands(
                [(1, Some("first"), false), (3, Some("third"), false)],
                &selection,
                8
            ),
            Err(SelectedCommandsError::TooLarge)
        );

        selection.clear();
        assert_eq!(
            selected_commands(zones, &selection, 256),
            Err(SelectedCommandsError::Empty)
        );
    }

    #[test]
    fn select_failed_neighbor_steps_only_over_failures() {
        // Oldest-first: 10 ok, 20 FAILED, 30 ok, 40 FAILED, 50 ok.
        let zones = [
            (10, false),
            (20, true),
            (30, false),
            (40, true),
            (50, false),
        ];
        // No selection (or a dangling id) enters from the requested edge.
        assert_eq!(select_failed_neighbor(&zones, None, true), Some(40));
        assert_eq!(select_failed_neighbor(&zones, None, false), Some(20));
        assert_eq!(select_failed_neighbor(&zones, Some(999), true), Some(40));
        assert_eq!(select_failed_neighbor(&zones, Some(999), false), Some(20));
        // From a failed selection: the nearest failed strictly beyond it.
        assert_eq!(select_failed_neighbor(&zones, Some(40), true), Some(20));
        assert_eq!(select_failed_neighbor(&zones, Some(20), false), Some(40));
        // From a NON-failed selection: same rule, skipping non-failures.
        assert_eq!(select_failed_neighbor(&zones, Some(30), true), Some(20));
        assert_eq!(select_failed_neighbor(&zones, Some(30), false), Some(40));
        assert_eq!(select_failed_neighbor(&zones, Some(50), true), Some(40));
        // Ends wrap to the far failed block; a non-failed selection beyond the
        // last failure wraps in the newer direction as well.
        assert_eq!(select_failed_neighbor(&zones, Some(20), true), Some(40));
        assert_eq!(select_failed_neighbor(&zones, Some(40), false), Some(20));
        assert_eq!(select_failed_neighbor(&zones, Some(50), false), Some(20));
        // No zones, or no failures at all: nothing to land on.
        assert_eq!(select_failed_neighbor(&[], None, true), None);
        assert_eq!(
            select_failed_neighbor(&[(1, false), (2, false)], None, false),
            None
        );
        assert_eq!(
            select_failed_neighbor(&[(1, false), (2, false)], Some(1), false),
            None
        );
    }

    fn source(zone_id: u64, command: Option<&str>, output: Option<&str>) -> CachedBlockSearchZone {
        CachedBlockSearchZone::new(zone_id, command, output.map(str::to_string))
    }

    #[test]
    fn cache_build_precomputes_lowercase_and_drops_blank_commands() {
        let zone = CachedBlockSearchZone::new(7, Some("Grep ERROR log"), Some("OK\nDone".into()));
        assert_eq!(zone.command.as_deref(), Some("Grep ERROR log"));
        assert_eq!(zone.command_lowercase.as_deref(), Some("grep error log"));
        assert_eq!(zone.output.as_deref(), Some("OK\nDone"));
        assert_eq!(zone.output_lowercase.as_deref(), Some("ok\ndone"));
        // A blank command normalizes to none (background zone), and a zone
        // without output caches none.
        let background = CachedBlockSearchZone::new(8, Some("  \t"), None);
        assert_eq!(background.command, None);
        assert_eq!(background.command_lowercase, None);
        assert_eq!(background.output, None);
        assert_eq!(background.output_lowercase, None);
    }

    #[test]
    fn source_snapshot_is_lazy_bounded_and_keeps_newest_zones() {
        let extracted = std::cell::Cell::new(0usize);
        let chunk = 1 << 20;
        let sources = (1..=9u64).rev().map(|zone_id| {
            extracted.set(extracted.get() + 1);
            BlockSearchSource::new(zone_id, None, Some("x".repeat(chunk)))
        });
        let snapshot = bounded_block_search_sources(sources, BLOCK_SEARCH_SOURCE_MAX_BYTES);

        assert_eq!(snapshot.sources.len(), 8);
        assert_eq!(snapshot.sources[0].zone_id, 9);
        assert_eq!(snapshot.sources[7].zone_id, 2);
        assert_eq!(snapshot.resident_bytes, BLOCK_SEARCH_SOURCE_MAX_BYTES);
        assert!(snapshot.older_not_indexed);
        // The first over-budget zone is the only discarded source extracted;
        // an arbitrarily long iterator is never drained after the cap.
        assert_eq!(extracted.get(), 9);
    }

    #[test]
    fn cache_builder_counts_lowercase_residency_and_drops_older_zones() {
        let sources = vec![
            BlockSearchSource::new(3, Some("NEWEST İ".into()), Some("RESULT İ".into())),
            BlockSearchSource::new(2, Some("middle".into()), Some("middle output".into())),
            BlockSearchSource::new(1, Some("oldest".into()), Some("old output".into())),
        ];
        let snapshot = BlockSearchSourceSnapshot {
            resident_bytes: sources.iter().map(BlockSearchSource::resident_bytes).sum(),
            sources,
            older_not_indexed: false,
        };
        let newest = CachedBlockSearchZone::from_source(snapshot.sources[0].clone());
        let vec_bytes = snapshot.sources.len() * std::mem::size_of::<CachedBlockSearchZone>();
        let budget = vec_bytes + newest.resident_bytes();
        let build = build_block_search_cache(snapshot, budget);

        assert_eq!(build.zones.len(), 1);
        assert_eq!(build.zones[0].zone_id, 3);
        assert!(build.older_not_indexed);
        assert!(build.resident_bytes <= budget);
        assert_eq!(
            build.resident_bytes,
            build.zones.capacity() * std::mem::size_of::<CachedBlockSearchZone>()
                + build
                    .zones
                    .iter()
                    .map(CachedBlockSearchZone::resident_bytes)
                    .sum::<usize>()
        );
        assert_eq!(
            build.zones[0].command_lowercase.as_deref(),
            Some("newest i̇")
        );
    }

    #[test]
    fn cache_builder_restores_oldest_first_search_order() {
        let sources = vec![
            BlockSearchSource::new(3, Some("three".into()), None),
            BlockSearchSource::new(2, Some("two".into()), None),
            BlockSearchSource::new(1, Some("one".into()), None),
        ];
        let snapshot = BlockSearchSourceSnapshot {
            resident_bytes: sources.iter().map(BlockSearchSource::resident_bytes).sum(),
            sources,
            older_not_indexed: false,
        };
        let build = build_block_search_cache(snapshot, BLOCK_SEARCH_CACHE_MAX_BYTES);
        assert_eq!(
            build
                .zones
                .iter()
                .map(|zone| zone.zone_id)
                .collect::<Vec<_>>(),
            vec![1, 2, 3]
        );
        assert!(!build.older_not_indexed);
        assert!(build.resident_bytes <= BLOCK_SEARCH_CACHE_MAX_BYTES);
    }

    #[test]
    fn search_blocks_matches_commands_and_output_lines_case_insensitively() {
        let sources = [
            source(1, Some("make test"), Some("ok\nError: boom\ndone")),
            source(2, Some("grep ERROR log"), Some("nothing here")),
        ];
        let results = search_blocks(&sources, "error");
        assert!(!results.capped);
        // Newest zone first; a zone's command hit precedes its output hits;
        // output line numbers are 1-based.
        assert_eq!(results.hits.len(), 2);
        assert_eq!(results.hits[0].zone_id, 2);
        assert!(!results.hits[0].is_output_line);
        assert_eq!(results.hits[0].line_no, 0);
        assert_eq!(results.hits[0].match_span, Some(5..10));
        assert_eq!(results.hits[0].line_text, "grep ERROR log");
        assert_eq!(results.hits[0].command_preview, "grep ERROR log");
        assert_eq!(results.hits[1].zone_id, 1);
        assert!(results.hits[1].is_output_line);
        assert_eq!(results.hits[1].line_no, 2);
        assert_eq!(results.hits[1].match_span, Some(0..5));
        assert_eq!(results.hits[1].line_text, "Error: boom");
        assert_eq!(results.hits[1].command_preview, "make test");
        // The query is folded too, and background zones search their output.
        let background = [source(7, None, Some("Worker READY"))];
        let hit = &search_blocks(&background, "ReAdY").hits[0];
        assert!(hit.is_output_line);
        assert_eq!(hit.line_no, 1);
        assert_eq!(hit.match_span, Some(7..12));
        assert_eq!(hit.command_preview, "");
    }

    #[test]
    fn search_blocks_blank_query_matches_nothing() {
        let sources = [source(1, Some("ls"), Some("file"))];
        assert!(search_blocks(&sources, "").hits.is_empty());
        assert!(search_blocks(&sources, "   ").hits.is_empty());
        assert!(search_blocks(&sources, "absent").hits.is_empty());
    }

    #[test]
    fn search_blocks_caps_at_500_hits_and_stops_early() {
        // One zone with 600 matching lines: the scan stops at the cap.
        let output = "match\n".repeat(600);
        let sources = [
            source(1, Some("old zone"), Some("match here too")),
            source(2, None, Some(&output)),
        ];
        let results = search_blocks(&sources, "match");
        assert_eq!(results.hits.len(), BLOCK_SEARCH_HIT_CAP);
        assert!(results.capped);
        // Newest-first means the capped scan never reached the older zone.
        assert!(results.hits.iter().all(|hit| hit.zone_id == 2));
        // Exactly at the cap: no false "capped" flag.
        let exact = "match\n".repeat(BLOCK_SEARCH_HIT_CAP);
        let sources = [source(3, None, Some(&exact))];
        let results = search_blocks(&sources, "match");
        assert_eq!(results.hits.len(), BLOCK_SEARCH_HIT_CAP);
        assert!(!results.capped);
    }

    #[test]
    fn filtered_search_excludes_zones_before_the_hit_cap_is_counted() {
        let cache: Vec<_> = (0..=BLOCK_SEARCH_HIT_CAP as u64)
            .map(|zone_id| CachedBlockSearchZone::new(zone_id, Some("match"), None))
            .collect();
        let results = search_blocks_filtered(&cache, "match", |zone_id| zone_id == 0);
        assert_eq!(results.hits.len(), 1);
        assert_eq!(results.hits[0].zone_id, 0);
        assert!(!results.capped);
    }

    #[test]
    fn search_blocks_clips_previews_with_an_ellipsis() {
        let long_line = format!("needle {}", "x".repeat(300));
        let long_command = format!("needle {}", "c".repeat(300));
        let sources = [source(1, Some(&long_command), Some(&long_line))];
        let results = search_blocks(&sources, "needle");
        let command_hit = &results.hits[0];
        assert_eq!(
            command_hit.line_text.chars().count(),
            BLOCK_SEARCH_LINE_CHARS
        );
        assert!(command_hit.line_text.ends_with('…'));
        assert_eq!(
            command_hit.command_preview.chars().count(),
            BLOCK_SEARCH_COMMAND_CHARS
        );
        assert!(command_hit.command_preview.ends_with('…'));
        let output_hit = &results.hits[1];
        assert_eq!(
            output_hit.line_text.chars().count(),
            BLOCK_SEARCH_LINE_CHARS
        );
        assert!(output_hit.line_text.ends_with('…'));
    }

    #[test]
    fn search_preview_centers_a_match_beyond_the_long_line_prefix() {
        let line = format!("{}VISIBLE-NEEDLE{}", "x".repeat(300), "y".repeat(300));
        let cache = [CachedBlockSearchZone::new(7, None, Some(line))];
        let results = search_blocks(&cache, "visible-needle");
        let hit = &results.hits[0];
        assert_eq!(hit.match_span, Some(300..314));
        let preview = &hit.line_text;
        assert!(preview.contains("VISIBLE-NEEDLE"));
        assert!(preview.starts_with('…'));
        assert!(preview.ends_with('…'));
        assert!(preview.chars().count() <= BLOCK_SEARCH_LINE_CHARS);
    }

    #[test]
    fn search_match_span_maps_lowercase_expansion_back_to_original_chars() {
        // U+0130 lowercases to two scalars (`i` + combining dot). The cached
        // lowercase byte/char offsets therefore differ from the original, and
        // the multibyte prefix makes a raw byte offset wrong as well.
        let line = "界界İY tail";
        let cache = [CachedBlockSearchZone::new(9, None, Some(line.into()))];

        let expanded = search_blocks(&cache, "İY");
        assert_eq!(expanded.hits[0].match_span, Some(2..4));
        assert_eq!(expanded.hits[0].line_text, line);

        // A query matching only the first scalar of that expansion still
        // points at the one original character which produced it.
        let partial = search_blocks(&cache, "i");
        assert_eq!(partial.hits[0].match_span, Some(2..3));
    }

    #[test]
    fn marker_fractions_are_exact_row_ratios() {
        assert_eq!(marker_fractions(&[0, 25, 50], 100), vec![0.0, 0.25, 0.5]);
        // Rows at (or past) the total clamp to 1.0 instead of overshooting.
        assert_eq!(marker_fractions(&[150], 100), vec![1.0]);
        // A zero-row buffer yields no markers rather than dividing by zero.
        assert_eq!(marker_fractions(&[3], 0), Vec::<f32>::new());
        assert_eq!(marker_fractions(&[], 100), Vec::<f32>::new());
    }

    #[test]
    fn badge_only_fits_over_blank_trailing_cells() {
        let blank_tail: Vec<char> = "ls -la      ".chars().collect();
        assert!(badge_fits(&blank_tail, 5));
        assert!(!badge_fits(&blank_tail, 8)); // would cover the "a"
        assert!(!badge_fits(&blank_tail, 13)); // wider than the row
        let nul_tail: Vec<char> = vec!['x', '\0', '\0'];
        assert!(badge_fits(&nul_tail, 2));
    }
}
