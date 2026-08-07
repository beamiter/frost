//! Pure logic for Warp-style command blocks (anvil/forge design contract):
//! outcome classification, badge text, row-span math, and badge fitting.
//! Everything here is renderer-agnostic so it can be unit tested; the paint
//! code in `terminal_view` and the per-frame builder in `main` stay thin.

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

/// First-row badge text, or `None` when the block gets no badge (background
/// blocks, and running blocks which never reach this function). An unknown
/// exit shows a bare `?` — there is no number to show, so none is invented.
/// Failed blocks name the killing signal right after the code (the
/// `jterm_core::bottom_bar` convention), with the duration last.
pub fn badge_text(outcome: BlockOutcome, duration_ms: Option<u64>) -> Option<String> {
    match outcome {
        BlockOutcome::Background => None,
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
        BlockOutcome::Unknown => Some("?".to_string()),
    }
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
pub fn markdown_export(block: &MarkdownBlock<'_>) -> String {
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
        meta.push_str(&format!("- Cwd: {cwd}\n"));
    }
    if block.output_truncated {
        meta.push_str("- Note: output truncated\n");
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
/// same set the gutter click selects). No current selection — or a selection
/// whose zone is gone — resets to the NEWEST zone in either direction;
/// otherwise `older` steps toward the front and `!older` toward the back.
/// `None` means "do nothing": no zones at all, or the selection is already at
/// the requested end (a silent clamp).
pub fn select_neighbor(ids: &[u64], current: Option<u64>, older: bool) -> Option<u64> {
    let newest = *ids.last()?;
    let Some(position) = current.and_then(|id| ids.iter().position(|&zone| zone == id)) else {
        return Some(newest);
    };
    let target = if older {
        position.checked_sub(1)?
    } else if position + 1 < ids.len() {
        position + 1
    } else {
        return None;
    };
    Some(ids[target])
}

/// Step the block selection across FAILED zones only (`zones` oldest-first as
/// `(id, is_failed)` — the same [`classify`]-based predicate the scrollbar
/// markers use). No selection, or a dangling one, lands on the NEWEST failed
/// zone in either direction (mirroring [`select_neighbor`]'s reset rule);
/// from a live selection — failed or not — the step goes to the nearest
/// failed zone strictly older (`older`) or strictly newer. `None` means "do
/// nothing": no failed zone exists in the requested direction. This function
/// is silent about WHY it returned `None`; a caller wanting a "no failed
/// blocks at all" toast must check for failures itself.
pub fn select_failed_neighbor(
    zones: &[(u64, bool)],
    current: Option<u64>,
    older: bool,
) -> Option<u64> {
    let newest_failed = |slice: &[(u64, bool)]| {
        slice
            .iter()
            .rev()
            .find(|&&(_, failed)| failed)
            .map(|&(id, _)| id)
    };
    let Some(position) = current.and_then(|id| zones.iter().position(|&(zone, _)| zone == id))
    else {
        return newest_failed(zones);
    };
    if older {
        newest_failed(&zones[..position])
    } else {
        zones[position + 1..]
            .iter()
            .find(|&&(_, failed)| failed)
            .map(|&(id, _)| id)
    }
}

/// Hard cap on the hits one block search returns; the scan stops early once
/// it is reached (the query can always be refined).
pub const BLOCK_SEARCH_HIT_CAP: usize = 500;

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
    /// The matching line, clipped to [`BLOCK_SEARCH_LINE_CHARS`].
    pub line_text: String,
    /// The zone's command line, clipped to [`BLOCK_SEARCH_COMMAND_CHARS`]
    /// (empty for background zones).
    pub command_preview: String,
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
    pub fn new(zone_id: u64, command: Option<&str>, output: Option<String>) -> Self {
        let command = command
            .filter(|command| !command.trim().is_empty())
            .map(str::to_string);
        let command_lowercase = command.as_deref().map(str::to_lowercase);
        let output_lowercase = output.as_deref().map(str::to_lowercase);
        Self {
            zone_id,
            command,
            command_lowercase,
            output,
            output_lowercase,
        }
    }
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

/// Case-insensitive substring search across every cached zone's command
/// line and output lines. `cache` comes oldest-first (the zone deque's
/// order); hits are emitted newest zones first, a zone's command hit before
/// its output hits, output hits in line order. Matching runs entirely
/// against the precomputed lowercase copies (ember's per-open cache design)
/// — beyond the lowercased needle, allocations happen only for hits. A
/// blank query matches nothing — an empty picker, not the whole history.
pub fn search_blocks(cache: &[CachedBlockSearchZone], query: &str) -> BlockSearchResults {
    let needle = query.trim().to_lowercase();
    let mut results = BlockSearchResults {
        hits: Vec::new(),
        capped: false,
    };
    if needle.is_empty() {
        return results;
    }
    'zones: for zone in cache.iter().rev() {
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
            .filter(|lowercase| lowercase.contains(&needle))
            .and(zone.command.as_deref())
            .map(|command| (false, 0usize, command));
        let output_lines = zone.output.as_deref().into_iter().flat_map(str::lines);
        let lowercase_lines = zone
            .output_lowercase
            .as_deref()
            .into_iter()
            .flat_map(str::lines);
        let output_hits = output_lines
            .zip(lowercase_lines)
            .enumerate()
            .filter(|(_, (_, lowercase))| lowercase.contains(&needle))
            .map(|(index, (line, _))| (true, index + 1, line));
        for (is_output_line, line_no, line) in command_hit.into_iter().chain(output_hits) {
            if results.hits.len() >= BLOCK_SEARCH_HIT_CAP {
                results.capped = true;
                break 'zones;
            }
            let command_preview = preview(zone.command.as_deref());
            results.hits.push(BlockSearchHit {
                zone_id: zone.zone_id,
                is_output_line,
                line_no,
                line_text: clipped(line, BLOCK_SEARCH_LINE_CHARS),
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
            Some("?")
        );
        assert_eq!(badge_text(BlockOutcome::Background, Some(500)), None);
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
             - Cwd: /home/user/project\n\
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
        assert!(markdown.contains("- Cwd: /srv\n- Note: output truncated\n\nCommand:"));
        // The note is absent when nothing was cut.
        assert!(!markdown_export(&block(false)).contains("- Note: output truncated"));
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
        let markdown = markdown_export(&MarkdownBlock {
            command: None,
            output: "stray output",
            output_truncated: false,
            exit_code: Some(1),
            duration_ms: Some(500),
            finished_at_ms: None,
            tz_offset_secs: 0,
            cwd: None,
        });
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
    fn select_neighbor_orders_clamps_and_resets() {
        let ids = [10, 20, 30];
        // No selection (or a stale id): both directions pick the newest.
        assert_eq!(select_neighbor(&ids, None, true), Some(30));
        assert_eq!(select_neighbor(&ids, None, false), Some(30));
        assert_eq!(select_neighbor(&ids, Some(999), true), Some(30));
        // Stepping moves one zone at a time.
        assert_eq!(select_neighbor(&ids, Some(30), true), Some(20));
        assert_eq!(select_neighbor(&ids, Some(20), true), Some(10));
        assert_eq!(select_neighbor(&ids, Some(10), false), Some(20));
        // Ends clamp silently.
        assert_eq!(select_neighbor(&ids, Some(10), true), None);
        assert_eq!(select_neighbor(&ids, Some(30), false), None);
        // No zones at all: nothing to select.
        assert_eq!(select_neighbor(&[], None, true), None);
        assert_eq!(select_neighbor(&[], Some(1), false), None);
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
        // No selection (or a dangling id): both directions pick the NEWEST
        // failed zone.
        assert_eq!(select_failed_neighbor(&zones, None, true), Some(40));
        assert_eq!(select_failed_neighbor(&zones, None, false), Some(40));
        assert_eq!(select_failed_neighbor(&zones, Some(999), true), Some(40));
        // From a failed selection: the nearest failed strictly beyond it.
        assert_eq!(select_failed_neighbor(&zones, Some(40), true), Some(20));
        assert_eq!(select_failed_neighbor(&zones, Some(20), false), Some(40));
        // From a NON-failed selection: same rule, skipping non-failures.
        assert_eq!(select_failed_neighbor(&zones, Some(30), true), Some(20));
        assert_eq!(select_failed_neighbor(&zones, Some(30), false), Some(40));
        assert_eq!(select_failed_neighbor(&zones, Some(50), true), Some(40));
        // Ends clamp silently: nothing further in that direction.
        assert_eq!(select_failed_neighbor(&zones, Some(20), true), None);
        assert_eq!(select_failed_neighbor(&zones, Some(40), false), None);
        assert_eq!(select_failed_neighbor(&zones, Some(50), false), None);
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
        assert_eq!(results.hits[0].line_text, "grep ERROR log");
        assert_eq!(results.hits[0].command_preview, "grep ERROR log");
        assert_eq!(results.hits[1].zone_id, 1);
        assert!(results.hits[1].is_output_line);
        assert_eq!(results.hits[1].line_no, 2);
        assert_eq!(results.hits[1].line_text, "Error: boom");
        assert_eq!(results.hits[1].command_preview, "make test");
        // The query is folded too, and background zones search their output.
        let background = [source(7, None, Some("Worker READY"))];
        let hit = &search_blocks(&background, "ReAdY").hits[0];
        assert!(hit.is_output_line);
        assert_eq!(hit.line_no, 1);
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
