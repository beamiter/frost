//! Frozen native ASCII batch orchestration before NativeAsciiWrite extraction.
use super::*;
use std::cell::RefCell;
impl TerminalState {
    fn frozen_ascii_batch(&mut self, bytes: &[u8]) {
        let cols = self.vt.screen.grid.row_len();
        let autowrap = self.vt.protocol.modes().contains(&7);
        let mut pos = 0;
        if let Some(&last) = bytes.last() {
            self.vt.screen.last_printed_char = Some(last as char);
        }

        while pos < bytes.len() {
            if self.vt.screen.take_pending_wrap(autowrap) {
                observe(self, "wrap");
                self.move_vertical(VerticalMotion::Wrap);
            }

            let remaining = cols - self.vt.screen.cursor_col;
            let chunk_len = (bytes.len() - pos).min(remaining);

            let col = self.vt.screen.cursor_col;
            let end = col + chunk_len;
            let chunk = &bytes[pos..pos + chunk_len];
            observe(self, "idle");
            self.note_idle_background_cells(
                col,
                end,
                chunk.iter().any(|byte| !byte.is_ascii_whitespace()),
            );
            let (consumed, _) = self
                .vt.screen
                .write_frost_legacy_ascii_in_row(chunk, autowrap)
                .expect("nonempty ASCII chunk within the current row");
            pos += consumed;

            observe(self, "write");
            self.mark_row_dirty(self.vt.screen.cursor_row);
            self.mark_command_echo_extent(col, end);
        }
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
    t.vt.screen.insert_mode = false;
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
fn ascii_batch_matches_frozen_effect_order_and_chunk_boundaries() {
    let bytes = b"a bCDefgh ijklMNopqrst";
    for cols in [1, 2, 7] {
        for seed in 0..32 {
            for split in 0..=bytes.len() {
                let mut actual = seeded(cols, seed);
                let mut expected = seeded(cols, seed);
                for chunk in [&bytes[..split], &bytes[split..]] {
                    let a = capture(|| actual.put_ascii_run(chunk));
                    let e = capture(|| expected.frozen_ascii_batch(chunk));
                    assert_eq!(
                        a, e,
                        "cols={cols} seed={seed} split={split} chunk={chunk:?}"
                    );
                    assert_eq!(snapshot(&mut actual), snapshot(&mut expected));
                }
            }
        }
    }
}
