//! Frozen native scalar orchestration before GraphicWrite extraction.
use super::*;
use std::cell::RefCell;
impl TerminalState {
    fn frozen_transaction_char(&mut self, ch: char, track_idle_output: bool) {
        let ch = self.vt.screen.active_charset.translate(ch);
        let width = crate::char_width::cached_char_width(ch);
        if width == 0 {
            if let Some(effect) = self.vt.screen.compose_frost_legacy_mark(ch) {
                observe(self, "compose");
                self.mark_row_dirty(effect.row);
            }
            return;
        }

        let autowrap = self.vt.protocol.modes().contains(&7);

        if self.vt.screen.take_pending_wrap(autowrap) {
            observe(self, "wrap");
            self.move_vertical(VerticalMotion::Wrap);
        }
        if self.vt.screen.prepare_character_width(width, autowrap) {
            observe(self, "wrap");
            self.move_vertical(VerticalMotion::Wrap);
        }

        if track_idle_output {
            observe(self, "idle");
            self.note_idle_background_cells(
                self.vt.screen.cursor_col,
                self.vt.screen.cursor_col.saturating_add(width),
                !ch.is_whitespace(),
            );
        }
        let write_col = self.vt.screen.cursor_col;

        self.vt.screen
            .write_frost_legacy_char_in_row(ch, width, autowrap);
        // Mark the row as dirty after writing character
        observe(self, "write");
        self.mark_row_dirty(self.vt.screen.cursor_row);
        self.mark_command_echo_extent(write_col, write_col.saturating_add(width));
    }
}

type Snapshot = std::collections::BTreeMap<String, serde_json::Value>;
thread_local! { static TRACE: RefCell<Option<Vec<(String, Snapshot)>>> = const { RefCell::new(None) }; }
pub(super) fn observe(t: &mut TerminalState, event: &str) {
    if TRACE.with(|trace| trace.borrow().is_none()) {
        return;
    }
    let snapshot = parser_oracle_tests::snapshot(t);
    TRACE.with(|trace| {
        trace
            .borrow_mut()
            .as_mut()
            .unwrap()
            .push((event.to_owned(), snapshot))
    });
}
fn snapshot(t: &mut TerminalState) -> Snapshot {
    parser_oracle_tests::snapshot(t)
}
fn capture(f: impl FnOnce()) -> Vec<(String, Snapshot)> {
    TRACE.with(|trace| *trace.borrow_mut() = Some(Vec::new()));
    f();
    TRACE.with(|trace| trace.borrow_mut().take().unwrap())
}
fn seeded(cols: usize, seed: usize) -> TerminalState {
    let mut t = TerminalState::new(cols, 3);
    t.process_input(b"old\r\nhistory\r\nlast");
    if seed & 16 != 0 {
        t.process_input(b"\x1b[?1049h");
    }
    t.vt.screen.insert_mode = seed & 2 != 0;
    t.vt.screen.pending_wrap = seed & 1 != 0;
    t.vt.screen.cursor_row = 2;
    t.vt.screen.cursor_col = cols - 1;
    t.vt.screen.scroll_region_top = 1;
    t.vt.screen.scroll_region_bottom = 2;
    t.vt.screen.current_fg = Color::Indexed(3);
    t.vt.screen.current_flags.set_protected(true);
    t.vt.screen.grid[2][0].character = 'e';
    t.vt.screen.last_printed_char = Some('X');
    if seed & 4 != 0 {
        t.vt.screen.active_charset = Charset::DecSpecialGraphics;
    }
    if seed & 8 != 0 {
        t.vt.protocol.modes_mut().insert(7);
    } else {
        t.vt.protocol.modes_mut().remove(&7);
    }
    t
}
#[test]
fn graphic_write_and_repeat_match_frozen_native_effect_order() {
    for cols in [1, 2, 7] {
        for seed in 0..64 {
            let mut actual = seeded(cols, seed);
            let mut expected = seeded(cols, seed);
            for ch in "q界e\u{301} Z\u{17d8}".chars() {
                let a = capture(|| actual.put_char(ch, seed & 32 != 0));
                let e = capture(|| expected.frozen_transaction_char(ch, seed & 32 != 0));
                assert_eq!(a, e, "write cols={cols} seed={seed} ch={ch:?}");
                assert_eq!(snapshot(&mut actual), snapshot(&mut expected));
                for params in [&[][..], &[0][..], &[2][..]] {
                    let a = capture(|| {
                        actual.handle_escape_sequence(params, b"", 'b', Some(b'?'), b" ")
                    });
                    let e = capture(|| {
                        let count = params.first().copied().unwrap_or(1).max(1);
                        if let Some(source) = expected.vt.screen.last_printed_char {
                            for _ in 0..count {
                                expected.frozen_transaction_char(source, false);
                            }
                        }
                    });
                    assert_eq!(a, e, "REP cols={cols} seed={seed} params={params:?}");
                    assert_eq!(snapshot(&mut actual), snapshot(&mut expected));
                }
            }
        }
    }
}
