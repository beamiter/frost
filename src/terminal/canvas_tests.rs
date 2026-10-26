use super::*;

fn fixture(chat: bool) -> (TerminalState, ProjectionPolicy, ProjectionViewState) {
    let mut terminal = TerminalState::new(32, 8);
    for index in 0..20 {
        terminal.process_input(format!(
            "\x1b]133;A\x07$ \x1b]133;B\x07echo {index}\r\n\x1b]133;C\x07line {index} 界\r\n\x1b]133;D;0\x07"
        ).as_bytes());
    }
    terminal.process_input(b"\x1b]133;A\x07$ \x1b]133;B\x07");
    let mut policy = ProjectionPolicy::new();
    policy.set_native_chrome(true, 0);
    policy.set_chat(chat);
    (terminal, policy, ProjectionViewState::new())
}

#[test]
fn canvas_overscan_preserves_cells_extent_dock_and_all_raw_provenance() {
    for chat in [false, true] {
        let (mut terminal, policy, mut state) = fixture(chat);
        let first = terminal.get_projected_viewport_with_state(true, &policy, &mut state);
        assert_eq!(first.mode(), ProjectionMode::Transformed);
        for offset in [
            0,
            1,
            7,
            first.max_scroll_offset().saturating_sub(1),
            first.max_scroll_offset(),
        ] {
            state.set_offset(offset, &first);
            let base = terminal.get_projected_viewport_with_state(true, &policy, &mut state);
            let canvas = terminal.canvas_viewport(&base, &policy);
            assert!(canvas.has_canvas_head());
            assert_eq!(canvas.cells().len(), base.cells().len() + 1);
            assert_eq!(&canvas.cells()[1..], base.cells());
            assert_eq!(canvas.document_rows(), base.document_rows());
            assert_eq!(canvas.max_scroll_offset(), base.max_scroll_offset());
            assert_eq!(canvas.scroll_offset(), base.scroll_offset());
            for row in 0..base.cells().len() {
                for col in 0..32 {
                    let raw = base.view_to_raw(ViewportCell { row, col });
                    assert_eq!(canvas.view_to_raw(ViewportCell { row: row + 1, col }), raw);
                    if let Some(origin) = raw {
                        assert_eq!(
                            canvas.raw_to_view(origin),
                            Some(ViewportCell { row: row + 1, col })
                        );
                    }
                }
                assert_eq!(
                    canvas.view_document_row(row + 1),
                    base.view_document_row(row)
                );
            }
            if chat {
                assert_eq!(
                    canvas.chat_rows()[1..]
                        .iter()
                        .filter(|row| row.role == crate::chat_mode::ChatRole::Composer)
                        .count(),
                    base.chat_rows()
                        .iter()
                        .filter(|row| row.role == crate::chat_mode::ChatRole::Composer)
                        .count()
                );
            }
            assert_eq!(
                TerminalState::projected_top_anchor(&canvas),
                TerminalState::projected_top_anchor(&base)
            );
        }
    }
}

#[test]
fn canvas_materializes_only_viewport_plus_one_row_and_reuses_document_plan() {
    let (mut terminal, policy, mut state) = fixture(false);
    let base = terminal.get_projected_viewport_with_state(true, &policy, &mut state);
    PROJECTION_MATERIALIZED_ROWS.with(|count| count.set(0));
    PROJECTION_PLAN_BUILD_COUNT.with(|count| count.set(0));
    let canvas = terminal.canvas_viewport(&base, &policy);
    assert_eq!(PROJECTION_PLAN_BUILD_COUNT.with(|count| count.get()), 0);
    assert!(PROJECTION_MATERIALIZED_ROWS.with(|count| count.get()) <= terminal.grid.rows() + 1);
    assert_eq!(terminal.grid.rows(), 8);
    assert_eq!(canvas.cells().len(), 9);
}

#[test]
fn canvas_history_anchor_survives_running_to_finished_transition() {
    for chat in [false, true] {
        let (mut terminal, policy, mut state) = fixture(chat);
        let initial = terminal.get_projected_viewport_with_state(true, &policy, &mut state);
        state.set_offset(12, &initial);
        let base = terminal.get_projected_viewport_with_state(true, &policy, &mut state);
        let canvas = terminal.canvas_viewport(&base, &policy);
        let anchor = TerminalState::projected_top_anchor(&base);
        state.hold_reading_position(&canvas);
        terminal.process_input(b"stream\r\n\x1b]133;C\x07first\r\nsecond\r\n");
        let running = terminal.get_projected_viewport_with_state(true, &policy, &mut state);
        assert_eq!(TerminalState::projected_top_anchor(&running), anchor);
        terminal.process_input(b"third\r\n\x1b]133;D;0\x07\x1b]133;A\x07$ \x1b]133;B\x07");
        let finished = terminal.get_projected_viewport_with_state(true, &policy, &mut state);
        assert_eq!(TerminalState::projected_top_anchor(&finished), anchor);
    }
}

#[test]
fn canvas_short_history_keeps_its_top_origin_while_new_blocks_and_lines_grow_down() {
    for chat in [false, true] {
        let mut terminal = TerminalState::new(40, 30);
        let mut policy = ProjectionPolicy::new();
        policy.set_native_chrome(true, 0);
        policy.set_chat(chat);
        let mut state = ProjectionViewState::new();
        terminal.process_input(b"\x1b]133;A\x07$ \x1b]133;B\x07first\r\n\x1b]133;C\x07alpha\r\n\x1b]133;D;0\x07\x1b]133;A\x07$ \x1b]133;B\x07");
        let origin = terminal.raw_cell_origin_at_absolute(0, 0).unwrap();
        let first = terminal.get_projected_viewport_with_state(true, &policy, &mut state);
        assert_eq!(first.max_scroll_offset(), 0);
        assert_ne!(first.row_kinds()[0], ProjectedRowKind::Padding);
        let anchor = first.raw_to_view(origin).unwrap();
        for bytes in [
            b"second\r\n\x1b]133;C\x07beta\r\n\x1b]133;D;0\x07\x1b]133;A\x07$ \x1b]133;B\x07"
                .as_slice(),
            b"stream\r\n\x1b]133;C\x07one\r\n",
            b"two\r\n",
            b"three\r\n",
        ] {
            terminal.process_input(bytes);
            let next = terminal.get_projected_viewport_with_state(true, &policy, &mut state);
            assert_eq!(
                next.max_scroll_offset(),
                0,
                "short canvas must not manufacture scroll range"
            );
            assert_eq!(
                next.raw_to_view(origin),
                Some(anchor),
                "old text must not climb as new text is written"
            );
            let canvas = terminal.canvas_viewport(&next, &policy);
            assert_eq!(
                canvas.raw_to_view(origin),
                Some(ViewportCell {
                    row: anchor.row + 1,
                    col: anchor.col
                })
            );
            for (row, kind) in next.row_kinds().iter().enumerate() {
                if *kind == ProjectedRowKind::Padding {
                    assert_eq!(next.view_document_row(row), None);
                    assert_eq!(next.view_to_raw(ViewportCell { row, col: 0 }), None);
                }
            }
        }
    }
}

#[test]
fn canvas_short_running_output_keeps_its_screen_origin_when_command_finishes() {
    for chat in [false, true] {
        let mut terminal = TerminalState::new(40, 30);
        let mut policy = ProjectionPolicy::new();
        policy.set_native_chrome(true, 0);
        policy.set_chat(chat);
        let mut state = ProjectionViewState::new();
        terminal.process_input(b"\x1b]133;A\x07$ \x1b]133;B\x07first\r\n\x1b]133;C\x07alpha\r\n");
        let origin = terminal.raw_cell_origin_at_absolute(1, 0).unwrap();
        let running = terminal.get_projected_viewport_with_state(true, &policy, &mut state);
        let anchor = running.raw_to_view(origin).unwrap();
        assert_eq!(running.cells()[anchor.row][anchor.col].character, 'a');
        assert_eq!(running.max_scroll_offset(), 0);
        let running_canvas = terminal.canvas_viewport(&running, &policy);
        terminal.process_input(b"\x1b]133;D;0\x07\x1b]133;A\x07$ \x1b]133;B\x07");
        let finished = terminal.get_projected_viewport_with_state(true, &policy, &mut state);
        assert_eq!(finished.max_scroll_offset(), 0);
        assert_eq!(
            finished.raw_to_view(origin),
            Some(anchor),
            "completion must fill existing chrome without moving live output; chat={chat}"
        );
        let finished_canvas = terminal.canvas_viewport(&finished, &policy);
        for canvas in [&running_canvas, &finished_canvas] {
            assert_eq!(canvas.cells().len(), 31);
            assert_eq!(
                canvas.raw_to_view(origin),
                Some(ViewportCell {
                    row: anchor.row + 1,
                    col: anchor.col
                })
            );
        }
    }
}

#[test]
fn canvas_chat_keeps_native_cards_through_separately_received_completion_and_prompt_marks() {
    let mut terminal = TerminalState::new(40, 30);
    let mut policy = ProjectionPolicy::new();
    policy.set_chat(true);
    policy.set_native_chrome(true, 0);
    let mut state = ProjectionViewState::new();
    terminal.process_input(b"\x1b]133;A\x07$ \x1b]133;B\x07first\r\n\x1b]133;C\x07alpha\r\n");
    let origin = terminal.raw_cell_origin_at_absolute(1, 0).unwrap();
    let running = terminal.get_projected_viewport_with_state(true, &policy, &mut state);
    let anchor = running.raw_to_view(origin).unwrap();
    for bytes in [
        b"\x1b]133;D;0\x07".as_slice(),
        b"\x1b]133;A\x07$ ",
        b"\x1b]133;B\x07",
    ] {
        terminal.process_input(bytes);
        let view = terminal.get_projected_viewport_with_state(true, &policy, &mut state);
        assert_eq!(
            view.mode(),
            ProjectionMode::Transformed,
            "a normal shell gap must not flash native grid"
        );
        assert!(!view.chat_rows().is_empty());
        assert_eq!(view.raw_to_view(origin), Some(anchor));
        if bytes != b"\x1b]133;B\x07" {
            assert_eq!(
                terminal.chat_surface(),
                crate::chat_mode::ChatSurface::Passthrough
            );
        }
    }
    terminal.note_user_input(b"\r");
    let pending = terminal.get_projected_viewport_with_state(true, &policy, &mut state);
    assert_eq!(pending.mode(), ProjectionMode::Transformed);
    assert_eq!(
        terminal.chat_surface(),
        crate::chat_mode::ChatSurface::Passthrough
    );
    assert_eq!(pending.raw_to_view(origin), Some(anchor));
    terminal.process_input(b"\x1b[?1049hfull screen");
    assert_eq!(
        terminal
            .get_projected_viewport_with_state(true, &policy, &mut state)
            .mode(),
        ProjectionMode::Bypass
    );
}
