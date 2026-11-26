//! Frozen original scalar-cell composition bodies, independent of the shared
//! target-selection and NFC composition methods being introduced.
use super::*;
use unicode_normalization::UnicodeNormalization;

impl TerminalState {
    fn frozen_compose_mark(&mut self, mark: char) {
        if self.vt.screen.cursor_col == 0 && !self.vt.screen.pending_wrap {
            return;
        }
        let mut base_col = if self.vt.screen.pending_wrap {
            self.vt.screen.cursor_col
        } else {
            self.vt.screen.cursor_col - 1
        };
        if base_col > 0
            && self
                .vt.screen
                .grid
                .get(self.vt.screen.cursor_row, base_col)
                .flags
                .wide_continuation()
        {
            base_col -= 1;
        }
        let cell = self.vt.screen.grid.get_mut(self.vt.screen.cursor_row, base_col);
        let mut combined = String::with_capacity(8);
        combined.push(cell.character);
        combined.push(mark);
        let nfc: String = combined.nfc().collect();
        let mut chars = nfc.chars();
        if let (Some(c0), None) = (chars.next(), chars.next()) {
            cell.character = c0;
            self.mark_row_dirty(self.vt.screen.cursor_row);
        }
    }
}

#[test]
fn shared_composition_matches_frozen_host_state() {
    for seed in [0, 3, 7] {
        for cursor_col in [0, 1, 5] {
            for pending_wrap in [false, true] {
                for continuation in [false, true] {
                    for base in [
                        'e', 'A', '\u{00c5}', '\u{1100}', '\u{ac00}', ' ', '\0', '界', '\u{0344}',
                        '\u{0340}', '\u{05c8}',
                    ] {
                        let mut actual = TerminalState::new(6, 5);
                        let mut original = TerminalState::new(6, 5);
                        for t in [&mut actual, &mut original] {
                            if seed & 1 != 0 {
                                t.process_input(b"\x1b[?1049h");
                            }
                            t.vt.screen.cursor_row = seed % 5;
                            t.vt.screen.cursor_col = cursor_col;
                            t.vt.screen.pending_wrap = pending_wrap;
                            for col in 0..6 {
                                let cell = t.vt.screen.grid.get_mut(t.vt.screen.cursor_row, col);
                                cell.character = base;
                                cell.flags.set_wide_continuation(continuation);
                            }
                        }
                        for mark in [
                            '\u{0301}', '\u{0308}', '\u{030a}', '\u{1161}', '\u{11a8}', '\u{fe0f}',
                            '\u{200d}', '\u{05c8}',
                        ] {
                            actual.combine_with_previous(mark);
                            original.frozen_compose_mark(mark);
                            assert_eq!(parser_oracle_tests::snapshot(&mut actual), parser_oracle_tests::snapshot(&mut original),
                                "seed={seed} col={cursor_col} pending={pending_wrap} continuation={continuation} base={base:?} mark={mark:?}");
                        }
                    }
                }
            }
        }
    }
}
