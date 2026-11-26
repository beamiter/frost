//! Frozen Frost J admission and host callbacks, independent of engine planning.
use super::*;
impl TerminalState {
    fn frozen_display_erase_dispatch(
        &mut self,
        params: &[u16],
        _private_prefix: Option<u8>,
        _intermediates: &[u8],
    ) {
        let cmd = 'J';
        match cmd {
            'J' => match params.first().copied().unwrap_or(0) {
                0 => {
                    if self.vt.screen.cursor_row == 0 && self.vt.screen.cursor_col == 0 {
                        self.inline_cli_screen_erased();
                    }
                    let damage = self.vt.screen.erase_frost_legacy_display(0).unwrap();
                    self.mark_rows_dirty(*damage.start(), *damage.end());
                }
                1 => {
                    let damage = self.vt.screen.erase_frost_legacy_display(1).unwrap();
                    self.mark_rows_dirty(*damage.start(), *damage.end());
                }
                2 => self.clear_screen_no_home(),
                3 => self.erase_saved_lines(),
                _ => {}
            },
            _ => {}
        }
    }
}

fn seeded(seed: usize, pending: bool, protected: bool) -> TerminalState {
    let mut t = TerminalState::new(6, 5);
    t.process_input(b"old\r\nretained\r\nmore\r\nrows\r\nlast\r\n");
    if seed & 1 != 0 {
        t.process_input(b"\x1b[?1049h");
    }
    if seed & 2 != 0 {
        t.set_inline_cli_launch(true);
    }
    if seed & 4 != 0 {
        t.set_scroll_offset(1);
    }
    t.process_input(b"\x1b[31;44;1m");
    for row in 0..5 {
        for col in 0..6 {
            let cell = t.vt.screen.grid.get_mut(row, col);
            cell.character = if col == 1 {
                '界'
            } else if col == 2 {
                ' '
            } else {
                'X'
            };
            cell.flags.set_wide(col == 1);
            cell.flags.set_wide_continuation(col == 2);
            cell.flags.set_protected(
                protected
                    || if col == 1 || col == 2 {
                        row & 1 == 0
                    } else {
                        col & 1 == 0
                    },
            );
        }
    }
    t.vt.screen.cursor_row = seed % 5;
    t.vt.screen.cursor_col = seed % 6;
    t.vt.screen.pending_wrap = pending;
    t
}

fn run(
    mut t: TerminalState,
    params: &[u16],
    prefix: Option<u8>,
    intermediates: &[u8],
    original: bool,
) -> std::collections::BTreeMap<String, serde_json::Value> {
    if original {
        t.frozen_display_erase_dispatch(params, prefix, intermediates);
    } else {
        t.handle_escape_sequence(params, b"", 'J', prefix, intermediates);
    }
    parser_oracle_tests::snapshot(&mut t)
}

#[test]
fn display_erase_admission_mutations_and_order_match_frozen_host() {
    for seed in [0, 3, 7] {
        for pending in [false, true] {
            for protected in [false, true] {
                for params in [
                    vec![],
                    vec![0],
                    vec![1],
                    vec![2],
                    vec![3],
                    vec![9],
                    vec![0, 3],
                ] {
                    for prefix in [None, Some(b'?'), Some(b'>'), Some(b'=')] {
                        for intermediates in [b"".as_slice(), b"!", b"$"] {
                            let params = params.as_slice();
                            assert_eq!(run(seeded(seed, pending, protected), params, prefix, intermediates, false), run(seeded(seed, pending, protected), params, prefix, intermediates, true), "seed={seed} pending={pending} protected={protected} params={params:?} prefix={prefix:?} intermediates={intermediates:?}");
                        }
                    }
                }
            }
        }
    }
}
