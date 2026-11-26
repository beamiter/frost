//! Frozen pre-extraction C0/ESC/nF bodies, independent of FrostStateCommand.
use super::*;
use jterm_engine::frost_controls::FrostStateEffect;
use std::cell::RefCell;
impl TerminalState {
    fn save_cursor(&mut self) {
        self.vt.screen.save_cursor(self.vt.protocol.modes().contains(&7));
    }
    fn restore_cursor(&mut self) {
        // FrostLegacyV1 restores the drawing state and keeps the live DECAWM.
        let _ = self.vt.screen.restore_cursor();
    }
    fn handle_nf_escape(&mut self, intermediates: &[u8], final_byte: u8) {
        match (intermediates, final_byte) {
            // DECALN: fill the screen with 'E'.
            (b"#", b'8') => {
                observe(self, FrostStateEffect::AlignmentTest);
                self.decaln()
            }
            // G0/G1 designation. Multi-byte designators (`ESC ( % 5`) and
            // G2/G3 (`ESC *`, `ESC +`) name sets frost does not carry, so they
            // are consumed without changing anything.
            (b"(" | b")", designator) => {
                let is_g0 = intermediates == b"(";
                let charset = Self::charset_from_designator(designator);
                observe(
                    self,
                    FrostStateEffect::CharsetNotice {
                        bank: intermediates[0],
                        designator,
                        charset,
                    },
                );
                crate::debug_log!(
                    "[CHARSET] ESC {} designator={} (0x{:02x}) charset={:?}",
                    if is_g0 { '(' } else { ')' },
                    designator as char,
                    designator,
                    charset
                );
                if is_g0 {
                    self.vt.screen.g0_charset = charset;
                    self.vt.screen.active_charset = self.vt.screen.g0_charset;
                } else {
                    self.vt.screen.g1_charset = charset;
                }
            }
            _ => {}
        }
    }
    fn charset_from_designator(byte: u8) -> Charset {
        match byte {
            b'0' => Charset::DecSpecialGraphics,
            _ => Charset::Ascii,
        }
    }
    fn frozen_control(&mut self, byte: u8) {
        match byte {
            8 => self.vt.screen.cursor_controls().move_backward(1),
            9 => self.vt.screen.cursor_controls().tab_forward(1),
            10..=12 => {
                observe(self, FrostStateEffect::Vertical(VerticalMotion::LineFeed));
                self.move_vertical(VerticalMotion::LineFeed)
            }
            13 => self.vt.screen.cursor_controls().set_column(1),
            14 => self.vt.screen.active_charset = self.vt.screen.g1_charset,
            15 => self.vt.screen.active_charset = self.vt.screen.g0_charset,
            _ => {} // Frost ignores BEL and other unhandled C0.
        }
    }
    fn frozen_escape(&mut self, intermediates: &[u8], final_byte: u8) {
        if intermediates.is_empty() {
            match final_byte {
                b'7' => self.save_cursor(),
                b'8' => self.restore_cursor(),
                b'E' => {
                    self.vt.screen.begin_next_line();
                    {
                        observe(self, FrostStateEffect::Vertical(VerticalMotion::LineFeed));
                        self.move_vertical(VerticalMotion::LineFeed)
                    };
                }
                b'D' => {
                    observe(self, FrostStateEffect::Vertical(VerticalMotion::LineFeed));
                    self.move_vertical(VerticalMotion::LineFeed)
                }
                b'M' => {
                    observe(
                        self,
                        FrostStateEffect::Vertical(VerticalMotion::ReverseIndex),
                    );
                    self.move_vertical(VerticalMotion::ReverseIndex)
                }
                b'H' => self.vt.screen.cursor_controls().set_tab_stop(true),
                b'c' => {
                    observe(self, FrostStateEffect::HardReset);
                    self.full_reset()
                }
                b'>' => {
                    self.vt.protocol.modes_mut().remove(&66);
                }
                b'=' => {
                    self.vt.protocol.modes_mut().insert(66);
                }
                _ => {}
            }
        } else {
            self.handle_nf_escape(&intermediates, final_byte);
        }
    }
}

type Snapshot = std::collections::BTreeMap<String, serde_json::Value>;
thread_local! { static TRACE: RefCell<Option<Vec<(FrostStateEffect,Snapshot)>>> = const { RefCell::new(None) }; }
pub(super) fn observe(t: &mut TerminalState, effect: FrostStateEffect) {
    if TRACE.with(|trace| trace.borrow().is_none()) {
        return;
    }
    let snapshot = parser_oracle_tests::snapshot(t);
    TRACE.with(|trace| {
        trace
            .borrow_mut()
            .as_mut()
            .unwrap()
            .push((effect, snapshot))
    });
}
fn capture(f: impl FnOnce()) -> Vec<(FrostStateEffect, Snapshot)> {
    TRACE.with(|trace| *trace.borrow_mut() = Some(Vec::new()));
    f();
    TRACE.with(|trace| trace.borrow_mut().take().unwrap())
}
fn seed(bits: usize) -> TerminalState {
    let mut t = TerminalState::new(6, 3);
    t.process_input(b"old\r\nhistory\r\nmore\r\nlast\x1b[31;44m");
    if bits & 1 != 0 {
        t.process_input(b"\x1b[?1049hALT");
    }
    if bits & 2 != 0 {
        t.process_input(b"\x1b[?2026h");
    }
    t.vt.screen.cursor_row = 2;
    t.vt.screen.cursor_col = 5;
    t.vt.screen.pending_wrap = bits & 4 != 0;
    t.vt.screen.g0_charset = Charset::DecSpecialGraphics;
    t.vt.screen.g1_charset = Charset::Ascii;
    t.vt.screen.active_charset = Charset::DecSpecialGraphics;
    t.vt.screen.scroll_region_top = 1;
    t.vt.screen.scroll_region_bottom = 2;
    if bits & 8 != 0 {
        t.vt.protocol.modes_mut().remove(&7);
    }
    t
}
#[test]
fn frost_control_commands_match_frozen_native_state_and_ordered_effects() {
    use jterm_engine::frost_controls::FrostStateCommand;
    for bits in 0..16 {
        for byte in [0, 7, 8, 9, 10, 11, 12, 13, 14, 15, 31] {
            let mut a = seed(bits);
            let mut e = seed(bits);
            let actual = capture(|| a.apply_frost_state_command(FrostStateCommand::control(byte)));
            let expected = capture(|| e.frozen_control(byte));
            assert_eq!(actual, expected, "control={byte} seed={bits}");
            assert_eq!(
                parser_oracle_tests::snapshot(&mut a),
                parser_oracle_tests::snapshot(&mut e)
            );
        }
        for intermediates in [&b""[..], b"#", b"(", b")", b"*", b"+", b"( %", b"( "] {
            let mut a = seed(bits);
            let mut e = seed(bits);
            // A sequence preserves save/restore and designation dependencies,
            // while comparing state and every synchronous hook after each event.
            for byte in [
                b'7', b'0', b'B', b'8', b'H', b'=', b'>', b'<', b'E', b'D', b'M', b'c',
            ] {
                let actual = capture(|| {
                    a.apply_frost_state_command(FrostStateCommand::escape(intermediates, byte))
                });
                let expected = capture(|| e.frozen_escape(intermediates, byte));
                assert_eq!(
                    actual, expected,
                    "escape={intermediates:?}/{byte} seed={bits}"
                );
                assert_eq!(
                    parser_oracle_tests::snapshot(&mut a),
                    parser_oracle_tests::snapshot(&mut e)
                );
            }
        }
    }
}
