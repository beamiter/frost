use super::*;

fn launch(terminal: &mut TerminalState, command: &str) {
    terminal.process_input(
        format!(
            "\x1b]133;A\x07$ \x1b]133;B\x07{command}\r\n\x1b]133;C;cmdline_url={}\x07",
            command.replace(' ', "%20")
        )
        .as_bytes(),
    );
}

fn fixture(command: &str) -> TerminalState {
    let mut terminal = TerminalState::new(40, 8);
    for row in 0..60 {
        terminal.process_input(format!("SHELL{row:04}\r\n").as_bytes());
    }
    launch(&mut terminal, command);
    for row in 0..100 {
        terminal.process_input(format!("CLI{row:04} 界\r\n").as_bytes());
    }
    terminal
}

fn scoped_document(terminal: &mut TerminalState) -> String {
    let policy = terminal
        .inline_cli_projection_policy()
        .expect("known primary CLI scope");
    let plan = terminal
        .cached_collapsed_projection_plan(terminal.grid.row_len(), &policy)
        .unwrap();
    terminal
        .materialize_identity_projection_plan(&plan)
        .iter()
        .map(|row| {
            row.iter()
                .filter(|cell| !cell.flags.wide_continuation())
                .map(|cell| cell.character)
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn top(terminal: &mut TerminalState, chat: bool) -> Arc<ProjectedViewport> {
    let mut policy = ProjectionPolicy::new();
    policy.set_chat(chat);
    policy.set_native_chrome(true, 0);
    let mut state = ProjectionViewState::new();
    let base = terminal.get_projected_viewport_with_state(chat, &policy, &mut state);
    state.set_offset(base.max_scroll_offset(), &base);
    terminal.get_projected_viewport_with_state(chat, &policy, &mut state)
}

#[test]
fn cli_scope_top_and_copy_are_current_execution_only_in_block_chat_and_native() {
    for command in ["codex resume fixed-id", "claude --resume fixed-id"] {
        for chat in [false, true] {
            let mut terminal = fixture(command);
            let view = top(&mut terminal, chat);
            assert_eq!(view.mode(), ProjectionMode::Transformed);
            assert!(
                view.chat_rows().is_empty(),
                "native CLI must not get a synthetic composer"
            );
            let row: String = view.cells()[0].iter().map(|cell| cell.character).collect();
            assert!(row.starts_with("CLI0000"), "{row:?}");
            terminal.start_selection_in_projection(&view, (0, 0), SelectionMode::Normal);
            terminal.update_selection_in_projection(&view, (0, 6));
            assert_eq!(terminal.copy_selection().as_deref(), Some("CLI0000"));
            let policy = ProjectionPolicy::new();
            let canvas = terminal.canvas_viewport(&view, &policy);
            assert!(canvas.has_canvas_head());
            assert_eq!(&canvas.cells()[1..], view.cells());
            terminal.start_selection_in_projection(&canvas, (1, 0), SelectionMode::Normal);
            terminal.update_selection_in_projection(&canvas, (1, 6));
            assert_eq!(terminal.copy_selection().as_deref(), Some("CLI0000"));
            let document = scoped_document(&mut terminal);
            assert!(!document.contains("SHELL"));
            assert!(document.contains("CLI0000") && document.contains("CLI0099"));
        }
    }
}

#[test]
fn cli_redraw_snapshots_are_not_history_but_real_top_margin_transfers_are() {
    let mut terminal = fixture("codex");
    let before = terminal.scrollback.len();
    let retained = scoped_document(&mut terminal);
    for frame in 0..4 {
        terminal.process_input(
            format!(
                "\x1b[?2026h\x1b[H\x1b[2JOLD_COMPOSER_{frame}\r\nOLD_STATUS_{frame}\x1b[?2026l"
            )
            .as_bytes(),
        );
    }
    assert_eq!(terminal.scrollback.len(), before);
    assert!(!terminal.scrollback.iter().any(|row| row
        .decompress()
        .iter()
        .map(|cell| cell.character)
        .collect::<String>()
        .contains("OLD_COMPOSER")));
    terminal.process_input(b"\x1b[H\x1b[2J\x1b[1;6r");
    for row in 0..30 {
        terminal.process_input(format!("NEW{row:04}\r\n").as_bytes());
    }
    terminal.process_input(b"\x1b[r");
    let document = scoped_document(&mut terminal);
    assert!(document.contains("CLI0000") && document.contains("NEW0000"));
    assert!(retained.contains("CLI0000"));
    assert!(!document.contains("SHELL") && !document.contains("OLD_COMPOSER"));
}

#[test]
fn cli_temporary_alt_sync_frames_and_alt_linefeeds_never_pollute_primary() {
    for direct in [false, true] {
        let mut terminal = if direct {
            let mut terminal = TerminalState::new(40, 8);
            terminal.set_inline_cli_launch(true);
            for row in 0..100 {
                terminal.process_input(format!("CLI{row:04}\r\n").as_bytes());
            }
            terminal
        } else {
            fixture("codex resume fixed-id")
        };
        let before = scoped_document(&mut terminal);
        let history_len = terminal.scrollback.len();
        terminal.process_input(b"\x1b[?1049h\x1b[?2026h\x1b[H\x1b[2J");
        assert!(terminal.inline_cli_owner_active());
        assert!(terminal.inline_cli_scope_floor().is_none());
        for row in 0..40 {
            terminal.process_input(format!("ALT_ONLY{row:04}\r\n").as_bytes());
        }
        terminal.process_input(b"\x1b[3J\x1b[?2026l");
        terminal.scroll_offset = 12; // even a stale pre-swap host offset cannot escape ALT
        let view = terminal.get_projected_viewport(false);
        assert_eq!(view.max_scroll_offset(), 0);
        assert_eq!(view.scroll_offset(), 0);
        assert_eq!(terminal.scrollback.len(), history_len);
        terminal.start_selection_in_projection(&view, (0, 0), SelectionMode::Normal);
        terminal.update_selection_in_projection(&view, (0, 7));
        assert_eq!(terminal.copy_selection().as_deref(), Some("ALT_ONLY"));
        terminal.set_scroll_offset(100);
        terminal.scroll(100);
        assert_eq!(terminal.scroll_offset, 0);
        terminal.process_input(b"\x1b[?2026h\x1b[H\x1b[2JALT_FINAL\x1b[?2026l\x1b[?1049l");
        assert_eq!(terminal.scrollback.len(), history_len);
        assert_eq!(scoped_document(&mut terminal), before);
        assert!(!scoped_document(&mut terminal).contains("ALT_"));
    }
}

#[test]
fn cli_scope_rebases_on_eviction_and_prefix_restoration() {
    let mut terminal = fixture("claude");
    let document = scoped_document(&mut terminal);
    let floor = terminal.inline_cli_scope_floor().unwrap();
    terminal.on_scrollback_rows_inserted(5);
    for _ in 0..5 {
        terminal.scrollback.push_front(ScrollbackLine::compress(
            &vec![TerminalCell::default(); 40],
            false,
        ));
    }
    terminal.bump_history_revision();
    assert_eq!(terminal.inline_cli_scope_floor(), Some(floor + 5));
    assert_eq!(scoped_document(&mut terminal), document);
    terminal.set_max_scrollback(12);
    assert_eq!(terminal.inline_cli_scope_floor(), Some(0));
    let view = top(&mut terminal, true);
    assert!(view.cells()[0]
        .iter()
        .map(|cell| cell.character)
        .collect::<String>()
        .starts_with("CLI"));
    assert!(!scoped_document(&mut terminal).contains("SHELL"));
}

#[test]
fn cli_lifecycle_completion_and_next_command_retire_scoped_ownership() {
    let mut terminal = fixture("codex");
    let floor = terminal.inline_cli_scope_floor();
    terminal.process_input(b"\x1b]133;C;cmdline_url=claude\x07");
    assert_eq!(
        terminal.inline_cli_scope_floor(),
        floor,
        "duplicate C cannot rebind a running command"
    );
    terminal.process_input(b"\x1b]133;D;0\x07");
    assert!(terminal.inline_cli_scope_floor().is_none());
    launch(&mut terminal, "sleep 1");
    assert!(!terminal.inline_cli_owner_active());
    terminal.process_input(b"\x1b]133;D;0\x07");
    launch(&mut terminal, "claude");
    terminal.process_input(b"SECOND_SESSION\r\n");
    let document = scoped_document(&mut terminal);
    assert!(!document.contains("CLI0000") && document.contains("SECOND_SESSION"));
}

#[test]
fn cli_direct_reset_preserves_execution_and_metadata_reset_retires_it() {
    let mut direct = TerminalState::new(20, 6);
    direct.set_inline_cli_launch(true);
    direct.process_input(b"before\r\n\x1bcafter\r\n");
    assert_eq!(direct.inline_cli_scope_floor(), Some(0));
    assert!(scoped_document(&mut direct).contains("after"));
    let mut shell = fixture("codex");
    shell.process_input(b"\x1bcafter\r\n");
    assert!(shell.inline_cli_scope_floor().is_none());
}

#[test]
fn cli_lazy_width_reflow_preserves_owner_and_does_not_rewrite_history() {
    let mut terminal = TerminalState::new(20, 6);
    terminal.set_inline_cli_launch(true);
    for row in 0..30 {
        terminal.process_input(format!("CLI{row:04} wide 界 and wrapped content\r\n").as_bytes());
    }
    let ids: Vec<_> = terminal
        .scrollback
        .iter()
        .map(ScrollbackLine::raw_row_id)
        .collect();
    terminal.on_resize(11, 6);
    assert!(!terminal.normalize_scrollback_width());
    assert_eq!(
        terminal
            .scrollback
            .iter()
            .map(ScrollbackLine::raw_row_id)
            .collect::<Vec<_>>(),
        ids
    );
    let view = top(&mut terminal, false);
    assert_eq!(view.mode(), ProjectionMode::Transformed);
    assert!(scoped_document(&mut terminal).contains("CLI0000"));
}

#[test]
fn cli_scope_revocation_invalidates_selection_without_a_cell_write() {
    let mut terminal = TerminalState::new(30, 6);
    terminal.set_inline_cli_launch(true);
    for row in 0..15 {
        terminal.process_input(format!("CLI{row:04}\r\n").as_bytes());
    }
    let view = top(&mut terminal, false);
    terminal.start_selection_in_projection(&view, (0, 0), SelectionMode::Normal);
    terminal.update_selection_in_projection(&view, (0, 6));
    assert_eq!(terminal.copy_selection().as_deref(), Some("CLI0000"));
    let revision = terminal.grid_version;
    terminal.set_inline_cli_launch(false);
    assert_eq!(terminal.grid_version, revision);
    assert!(terminal.projection_selection_revision(&view).is_none());
    assert!(terminal.copy_selection().is_none());
}

#[test]
fn cli_search_reveal_uses_scoped_identity_even_with_original_chat_policy() {
    let mut terminal = fixture("codex");
    let first = terminal
        .raw_cell_origin_at_absolute(terminal.inline_cli_scope_floor().unwrap(), 0)
        .unwrap();
    let outside = terminal.raw_cell_origin_at_absolute(0, 0).unwrap();
    let mut original_chat = ProjectionPolicy::new();
    original_chat.set_chat(true);
    original_chat.set_native_chrome(true, 0);
    let mut state = ProjectionViewState::new();
    let live = terminal.get_projected_viewport_with_state(true, &original_chat, &mut state);
    assert_eq!(
        terminal.locate_raw_cell_in_projection(&live, first),
        ProjectedRawCellLocation::Retained
    );
    assert!(terminal.reveal_raw_cell_in_projection(&original_chat, &mut state, first));
    let revealed = terminal.get_projected_viewport_with_state(true, &original_chat, &mut state);
    assert!(revealed.raw_to_view(first).is_some());
    assert!(!terminal.reveal_raw_cell_in_projection(&original_chat, &mut state, outside));
    assert!(
        terminal.raw_cell_origin_at_absolute(0, 0).is_some(),
        "the omitted shell source still exists"
    );
}

#[test]
fn cli_alt_erase_saved_lines_preserves_a_deferred_primary_purge() {
    let mut terminal = fixture("codex");
    terminal.scroll(3);
    terminal.erase_saved_lines();
    let requested = terminal.pending_saved_line_purge;
    let history = terminal.scrollback.len();
    assert!(requested > 0);
    terminal.process_input(b"\x1b[?1049h\x1b[3J");
    terminal.scroll_to_bottom();
    assert_eq!(terminal.scrollback.len(), history);
    assert_eq!(terminal.pending_saved_line_purge, requested);
    terminal.process_input(b"\x1b[?1049l");
    terminal.scroll_to_bottom();
    assert_eq!(terminal.pending_saved_line_purge, 0);
    assert_eq!(terminal.scrollback.len(), 0);
}
