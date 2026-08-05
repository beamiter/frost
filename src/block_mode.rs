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
    match command {
        None => BlockOutcome::Background,
        Some(command) if command.trim().is_empty() => BlockOutcome::Background,
        Some(_) => match exit_code {
            Some(0) => BlockOutcome::Success,
            Some(code) => BlockOutcome::Failed(code),
            None => BlockOutcome::Unknown,
        },
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
    fn badge_only_fits_over_blank_trailing_cells() {
        let blank_tail: Vec<char> = "ls -la      ".chars().collect();
        assert!(badge_fits(&blank_tail, 5));
        assert!(!badge_fits(&blank_tail, 8)); // would cover the "a"
        assert!(!badge_fits(&blank_tail, 13)); // wider than the row
        let nul_tail: Vec<char> = vec!['x', '\0', '\0'];
        assert!(badge_fits(&nul_tail, 2));
    }
}
