//! Full terminal projection entry points. No parser input or OS providers.
//! Runs in the ordinary Frost test binary (not the isolated kernel runner).
use super::projection_properties::{fixture, Op, CASES};
use super::*;

#[test]
fn seeded_terminal_resize_collapse_and_mode_entry_points() {
    let replay = std::env::var("FROST_PROJECTION_CASE")
        .ok()
        .map(|s| s.parse::<usize>().unwrap());
    let cases = replay.map_or(0..CASES, |case| case..case + 1);
    for case in cases {
        let (mut terminal, raw, ranges, ops) = fixture(case);
        for range in &ranges {
            let start = range.start_absolute;
            let end = range.end_absolute;
            let id = range.range.zone_id;
            terminal.command_zones.push_back(CommandZone {
                id,
                prompt_start: start,
                command_start: None,
                output_start: Some(start),
                output_start_col: 0,
                output_end: Some(end + 1),
                exit_code: Some(0),
                command: None,
                duration_ms: None,
                finished_at_ms: None,
                command_truncated: false,
                command_exact: false,
                cwd: None,
                captured_output: None,
                output_preview: String::new(),
                captured_output_evicted: false,
                start_mark_seen: false,
                completion_provenance: crate::block_mode::CompletionProvenance::BoundaryInferred,
                rows_evicted: false,
            });
            terminal.finished_output_provenance.insert(
                id,
                FinishedOutputProvenance {
                    range: range.range,
                    row_ids: raw[start..=end].iter().map(|r| r.id).collect(),
                },
            );
            terminal.spacing_prompt_rows.insert(raw[start].id);
        }
        terminal.next_zone_id = 100;
        terminal.bump_history_revision();
        let mut policy = ProjectionPolicy::new();
        let mut state = ProjectionViewState::new();
        let mut enabled = true;
        let mut previous = terminal.get_projected_viewport_with_state(enabled, &policy, &mut state);
        for (step, op) in ops.iter().enumerate() {
            match *op {
                Op::Resize(width) => terminal.on_resize(width, 3),
                Op::Toggle(index) => {
                    let id = ranges[index].range.zone_id;
                    if policy.is_collapsed(id) {
                        policy.expand(id);
                    } else {
                        policy.collapse(id);
                    }
                }
                Op::Mode(mode) => {
                    enabled = mode != 0;
                    policy.set_block_spacing(mode == 1);
                    policy.set_native_chrome(mode == 2, 0);
                }
                Op::Scroll(offset) => state.set_offset(offset, &previous),
            }
            let view = terminal.get_projected_viewport_with_state(enabled, &policy, &mut state);
            let context = format!("case={case}, prefix={}, ops={:?}", step + 1, &ops[..=step]);
            if !enabled {
                assert_eq!(view.mode(), ProjectionMode::Bypass, "{context}");
            }
            for (row, cells) in view.cells().iter().enumerate() {
                for (col, cell) in cells.iter().enumerate() {
                    let point = ViewportCell { row, col };
                    if let Some(origin) = view.view_to_raw(point) {
                        assert_eq!(view.raw_to_view(origin), Some(point), "{context}");
                        if let Some(source) = raw.iter().find(|r| r.id == origin.row) {
                            let mut expected = source.cells[origin.col];
                            if cells.len() == 1 {
                                expected.flags.set_wide(false);
                            }
                            assert_eq!(*cell, expected, "{context}: raw identity/content diverged");
                        } else {
                            assert_eq!(
                                *cell,
                                TerminalCell::default(),
                                "{context}: grid fixture is blank"
                            );
                        }
                    }
                }
            }
            // Same-source redraw must not change the snapshot's identities.
            let redraw = terminal.get_projected_viewport_with_state(enabled, &policy, &mut state);
            assert_eq!(view.cells(), redraw.cells(), "{context}");
            for row in 0..view.cells().len() {
                for col in 0..view.cells()[row].len() {
                    let point = ViewportCell { row, col };
                    assert_eq!(
                        view.view_to_raw(point),
                        redraw.view_to_raw(point),
                        "{context}"
                    );
                }
            }
            previous = view;
        }
    }
}
