//! Frozen Frost b72d1e4 entry/leave bodies, before coordinator extraction.
//! Original contiguous bodies SHA256: 72fc088b100d58db7be480ca74dbe2afb120cee8bbc7eb6ed247cad0ff396dfc
//! These bodies do not call the new coordinator; compare actual history,
//! style and protocol state.
use super::*;
use jterm_engine::screen::AlternateRestorePolicy;

impl TerminalState {
    fn frozen_enter_alternate_screen(&mut self, mode: u16) {
        if self.disable_alt_screen || self.vt.screen.use_alt_buffer {
            return;
        }
        self.saved_primary_global_bg = self.global_bg;
        self.vt.screen.saved_primary_cursor_shape = Some(self.vt.screen.cursor_shape);
        self.saved_primary_colors = self.colors.clone();
        self.scroll_offset = 0;
        self.provisional_alt_snapshot = None;

        // Frost saves the primary cursor and clears the alternate grid
        // for 47, 1047 and 1049 alike.
        let autowrap = self.vt.protocol.modes().contains(&7);
        let entered = self
            .vt.screen
            .alternate_screen_controls()
            .enter(true, autowrap);
        debug_assert!(entered);
        self.vt.protocol.switch_screen();
        self.watch_batch.interrupted = true;
        self.watch_batch.boundary = true;
        self.projection_scroll_guard.revoke();
        self.organism_idle_cwd = None;
        self.vt.screen.current_hyperlink = HyperlinkId::NONE;
        self.clear_text_selection();
        self.clear_screen();
        self.vt.protocol.modes_mut().insert(mode);
    }

    fn frozen_leave_alternate_screen(&mut self, mode: u16) {
        let Some(effect) = self
            .vt.screen
            .alternate_screen_controls()
            .leave(AlternateRestorePolicy::Frost)
        else {
            return;
        };
        self.vt.protocol.switch_screen();
        self.watch_batch.interrupted = true;
        self.watch_batch.boundary = true;
        self.projection_scroll_guard.revoke();
        // The alternate screen's retained snapshot is now permanent.
        self.provisional_alt_snapshot = None;
        self.vt.protocol.modes_mut().remove(&mode);
        // Alternate OSC 8 state never leaks onto the primary screen.
        self.vt.screen.current_hyperlink = HyperlinkId::NONE;

        self.clear_text_selection();
        self.global_bg = self.saved_primary_global_bg;
        self.saved_primary_global_bg = Color::Default;
        if let Some(shape) = self.vt.screen.saved_primary_cursor_shape {
            self.vt.screen.cursor_shape = shape;
        }
        self.colors = self.saved_primary_colors.clone();
        self.saved_primary_colors.reset_dynamic();

        // Force a full re-render after the grid swap.
        self.grid_version += effect.dirty_rows.len() as u64 + 1;
        self.row_versions.fill(self.grid_version);
    }
}

fn seeded(seed: usize) -> TerminalState {
    let mut t = TerminalState::new(6, 5);
    t.process_input(b"retained\r\nprimary\r\nrow\r\nlast\r\nnext\r\nmore");
    t.process_input(b"\x1b[31;44m\x1b[2;4r\x1b[3;6HX\x1b7\x1b]10;#010203\x07\x1b]11;#040506\x07\x1b]4;9;#070809\x07\x1b]8;;https://example.com/primary\x1b\\");
    t.vt.protocol.push_keyboard_enhancement_flags(17);
    t.vt.protocol.switch_screen();
    t.vt.protocol.push_keyboard_enhancement_flags(8);
    t.vt.protocol.switch_screen();
    t.global_bg = Color::Indexed(17);
    t.vt.screen.cursor_shape = CursorShape::Underline;
    t.organism_idle_cwd = Some("/alternate-oracle".into());
    t.provisional_alt_snapshot = Some((0, 19));
    t.scroll_offset = 1;
    t.disable_alt_screen = seed & 1 != 0;
    if seed & 2 != 0 {
        t.process_input(b"\x1b[?2026h");
    }
    t
}

fn snapshot(t: &mut TerminalState) -> std::collections::BTreeMap<String, serde_json::Value> {
    let mut out = parser_oracle_tests::snapshot(t);
    out.insert(
        "provisional_alt_snapshot".into(),
        serde_json::json!(t.provisional_alt_snapshot),
    );
    out.insert(
        "keyboard_stack".into(),
        serde_json::json!(format!("{:?}", t.vt.protocol)),
    );
    out
}

#[test]
fn native_alternate_matches_frozen_frost_history_style_and_protocol() {
    for seed in 0..4 {
        for enter in [47, 1047, 1049] {
            for exit in [47, 1047, 1049] {
                let mut actual = seeded(seed);
                let mut frozen = seeded(seed);
                for (enabled, mode) in [
                    (false, exit),
                    (true, enter),
                    (true, 1049),
                    (false, exit),
                    (false, exit),
                    (true, 47),
                    (false, 1049),
                ] {
                    let before_actual = actual.projection_scroll_guard.snapshot();
                    let before_frozen = frozen.projection_scroll_guard.snapshot();
                    if enabled {
                        actual.enter_alternate_screen(mode);
                        frozen.frozen_enter_alternate_screen(mode);
                    } else {
                        actual.leave_alternate_screen(mode);
                        frozen.frozen_leave_alternate_screen(mode);
                    }
                    assert_eq!(
                        actual.projection_scroll_guard.snapshot() != before_actual,
                        frozen.projection_scroll_guard.snapshot() != before_frozen
                    );
                    assert_eq!(
                        snapshot(&mut actual),
                        snapshot(&mut frozen),
                        "seed={seed} enter={enter} exit={exit} enabled={enabled} mode={mode}"
                    );
                    for t in [&mut actual, &mut frozen] {
                        t.process_input(b"\x1b]8;;https://example.com/current\x1b\\\x1b[33;42mZ\x1b[?7l\x1b[?u\x1b[?1049$p");
                    }
                }
            }
        }
    }
}
