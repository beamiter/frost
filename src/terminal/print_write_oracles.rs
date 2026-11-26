//! Original Frost printable write bodies frozen before shared screen adoption.
//! These bodies intentionally do not call the candidate write methods.
use super::*;
use jterm_engine::grid::{AsciiOverwritePolicy, WideContinuationProtection};

impl TerminalState {
    fn frozen_put_char(&mut self, ch: char, track_idle_output: bool) {
        let ch = self.vt.screen.active_charset.translate(ch);
        let width = crate::char_width::cached_char_width(ch);
        if width == 0 {
            self.combine_with_previous(ch);
            return;
        }

        let cols = self.vt.screen.grid.row_len();
        let blank_cell = self.create_blank_cell();
        let autowrap = self.vt.protocol.modes().contains(&7);

        if self.vt.screen.pending_wrap {
            self.vt.screen.pending_wrap = false;
            if autowrap {
                self.move_vertical(VerticalMotion::Wrap);
            }
        }

        // If character doesn't fit at end of line, handle based on autowrap mode
        if self.vt.screen.cursor_col + width > cols {
            // Only wrap to next line if autowrap mode (mode 7) is enabled
            if autowrap {
                self.move_vertical(VerticalMotion::Wrap);
            } else {
                // Autowrap disabled: clamp cursor to last column instead of wrapping
                self.vt.screen.cursor_col = cols.saturating_sub(width);
            }
        }

        if track_idle_output {
            self.note_idle_background_cells(
                self.vt.screen.cursor_col,
                self.vt.screen.cursor_col.saturating_add(width),
                !ch.is_whitespace(),
            );
        }
        let write_col = self.vt.screen.cursor_col;

        // IRM insert mode (mode 4): make room by shifting the row right.
        if self.vt.screen.insert_mode {
            self.vt.screen.insert_frost_legacy_blanks(width);
        }

        self.vt.screen.grid.overwrite_graphic(
            self.vt.screen.cursor_row,
            self.vt.screen.cursor_col,
            width,
            TerminalCell {
                character: ch,
                foreground: self.vt.screen.current_fg,
                background: self.vt.screen.current_bg,
                flags: self.vt.screen.current_flags,
                hyperlink_id: self.vt.screen.current_hyperlink,
            },
            blank_cell,
            WideContinuationProtection::FromBlank,
        );

        self.vt.screen.cursor_col += width;
        self.vt.screen.last_printed_char = Some(ch);
        if self.vt.screen.cursor_col >= cols {
            self.vt.screen.cursor_col = cols.saturating_sub(width);
            if autowrap {
                self.vt.screen.pending_wrap = true;
            }
        }
        // Mark the row as dirty after writing character
        self.mark_row_dirty(self.vt.screen.cursor_row);
        self.mark_command_echo_extent(write_col, write_col.saturating_add(width));
    }

    fn frozen_put_ascii_run(&mut self, bytes: &[u8]) {
        let cols = self.vt.screen.grid.row_len();
        let autowrap = self.vt.protocol.modes().contains(&7);
        let mut pos = 0;
        if let Some(&last) = bytes.last() {
            self.vt.screen.last_printed_char = Some(last as char);
        }

        while pos < bytes.len() {
            if self.vt.screen.pending_wrap {
                self.vt.screen.pending_wrap = false;
                if autowrap {
                    self.move_vertical(VerticalMotion::Wrap);
                }
            }

            let remaining = cols - self.vt.screen.cursor_col;
            let chunk_len = (bytes.len() - pos).min(remaining);

            let rendition = TerminalCell {
                character: ' ',
                foreground: self.vt.screen.current_fg,
                background: self.vt.screen.current_bg,
                flags: self.vt.screen.current_flags,
                hyperlink_id: self.vt.screen.current_hyperlink,
            };
            let col = self.vt.screen.cursor_col;
            let end = col + chunk_len;
            let chunk = &bytes[pos..pos + chunk_len];
            self.note_idle_background_cells(
                col,
                end,
                chunk.iter().any(|byte| !byte.is_ascii_whitespace()),
            );
            let blank = self.create_blank_cell();
            self.vt.screen.grid.overwrite_ascii(
                self.vt.screen.cursor_row,
                col,
                chunk,
                rendition,
                blank,
                AsciiOverwritePolicy::FrostLegacy,
            );

            self.vt.screen.cursor_col += chunk_len;
            pos += chunk_len;

            self.mark_row_dirty(self.vt.screen.cursor_row);
            self.mark_command_echo_extent(col, end);

            if self.vt.screen.cursor_col >= cols {
                self.vt.screen.cursor_col = cols - 1;
                if autowrap {
                    self.vt.screen.pending_wrap = true;
                }
            }
        }
    }
}

fn seeded(
    cols: usize,
    rows: usize,
    alternate: bool,
    insert: bool,
    autowrap: bool,
) -> TerminalState {
    let mut t = TerminalState::new(cols, rows);
    t.process_input(b"\x1b]133;A\x07$ \x1b]133;B\x07draft\r\n");
    if alternate {
        t.process_input(b"\x1b[?1049h");
    }
    t.process_input(
        "界ab\r\nwide界\x1b[31;44;1m\x1b]8;id=print;https://example.test/print\x1b\\".as_bytes(),
    );
    t.vt.screen.current_flags.set_protected(true);
    t.vt.screen.insert_mode = insert;
    if autowrap {
        t.vt.protocol.modes_mut().insert(7);
    } else {
        t.vt.protocol.modes_mut().remove(&7);
    }
    t.vt.screen.scroll_region_top = rows.saturating_sub(2);
    t.vt.screen.scroll_region_bottom = rows.saturating_sub(1);
    t.vt.screen.cursor_row = rows - 1;
    t.vt.screen.cursor_col = cols - 1;
    t.vt.screen.pending_wrap = true;
    t
}

#[test]
fn frost_graphic_writes_match_frozen_host_state() {
    for (cols, rows) in [(1, 1), (2, 3), (5, 4), (9, 6)] {
        for alternate in [false, true] {
            for insert in [false, true] {
                for autowrap in [false, true] {
                    for track in [false, true] {
                        let mut actual = seeded(cols, rows, alternate, insert, autowrap);
                        let mut original = seeded(cols, rows, alternate, insert, autowrap);
                        for ch in "ab界e\u{301}🙂Z \u{308}\u{17d8}".chars() {
                            actual.put_char(ch, track);
                            original.frozen_put_char(ch, track);
                            assert_eq!(parser_oracle_tests::snapshot(&mut actual), parser_oracle_tests::snapshot(&mut original),
                                "{cols}x{rows} alt={alternate} insert={insert} wrap={autowrap} track={track} ch={ch:?}");
                        }
                    }
                }
            }
        }
    }
}

#[test]
fn frost_ascii_writes_match_frozen_host_state() {
    for (cols, rows) in [(1, 1), (2, 3), (5, 4), (9, 6)] {
        for alternate in [false, true] {
            for autowrap in [false, true] {
                let mut actual = seeded(cols, rows, alternate, false, autowrap);
                let mut original = seeded(cols, rows, alternate, false, autowrap);
                for bytes in [b"".as_slice(), b"A", b"abcdefghijklmnop", b"  final  "] {
                    actual.put_ascii_run(bytes);
                    original.frozen_put_ascii_run(bytes);
                    assert_eq!(
                        parser_oracle_tests::snapshot(&mut actual),
                        parser_oracle_tests::snapshot(&mut original),
                        "{cols}x{rows} alt={alternate} wrap={autowrap} bytes={bytes:?}"
                    );
                }
            }
        }
    }
}
