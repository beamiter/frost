//! Frost display mutations frozen before shared-owner adoption.
//! The original bodies below stay independent of the new screen methods.
use super::*;

impl TerminalState {
    fn frozen_erase_display(&mut self, mode: u16) {
        match mode {
            0 => {
                if self.vt.screen.cursor_row == 0 && self.vt.screen.cursor_col == 0 {
                    self.inline_cli_screen_erased();
                }
                let cols = self.vt.screen.grid.row_len();
                let blank = self.create_blank_cell();
                self.vt.screen.grid.erase_cells(
                    self.vt.screen.cursor_row,
                    self.vt.screen.cursor_col..cols,
                    blank,
                );
                for row in self.vt.screen.cursor_row + 1..self.vt.screen.grid.rows() {
                    self.vt.screen.grid.erase_cells(row, 0..cols, blank);
                }
                self.mark_rows_dirty(
                    self.vt.screen.cursor_row,
                    self.vt.screen.grid.rows().saturating_sub(1),
                );
            }
            1 => {
                let cols = self.vt.screen.grid.row_len();
                let blank = self.create_blank_cell();
                for row in 0..self.vt.screen.cursor_row {
                    self.vt.screen.grid.erase_cells(row, 0..cols, blank);
                }
                self.vt.screen.grid.erase_cells(
                    self.vt.screen.cursor_row,
                    0..self.vt.screen.cursor_col + 1,
                    blank,
                );
                self.mark_rows_dirty(0, self.vt.screen.cursor_row);
            }
            2 => self.clear_screen_no_home(),
            3 => self.erase_saved_lines(),
            _ => {}
        }
    }
    fn frozen_alignment(&mut self) {
        self.vt.screen.grid.fill_cells(TerminalCell {
            character: 'E',
            ..TerminalCell::default()
        });
        self.vt.screen.cursor_row = 0;
        self.vt.screen.cursor_col = 0;
        self.vt.screen.pending_wrap = false;
        self.mark_rows_dirty(0, self.vt.screen.grid.rows().saturating_sub(1));
    }
}

fn seeded(cols: usize, rows: usize, alternate: bool, row: usize, col: usize) -> TerminalState {
    let mut terminal = TerminalState::new(cols, rows);
    terminal.process_input(b"old\r\nretained\r\n");
    if alternate {
        terminal.process_input(b"\x1b[?1049h");
    }
    terminal.process_input(b"\x1b[31;44;1m\x1b]8;id=test;https://example.test/a\x1b\\");
    for r in 0..rows {
        for c in 0..cols {
            let mut cell = TerminalCell {
                character: char::from_u32(65 + ((r * cols + c) % 26) as u32).unwrap(),
                foreground: Color::Indexed(3),
                background: Color::Indexed(5),
                ..TerminalCell::default()
            };
            cell.flags.set_protected(c % 2 == 0);
            *terminal.vt.screen.grid.get_mut(r, c) = cell;
        }
        terminal.vt.screen.grid.row_wrapped[r] = r % 2 == 0;
    }
    terminal.vt.screen.scroll_region_top = rows.saturating_sub(2);
    terminal.vt.screen.scroll_region_bottom = rows.saturating_sub(1);
    terminal.vt.screen.cursor_row = row;
    terminal.vt.screen.cursor_col = col;
    terminal.vt.screen.pending_wrap = true;
    terminal.vt.screen.origin_mode = true;
    terminal
}

#[test]
fn frost_display_mutations_match_frozen_host_state() {
    for (cols, rows) in [(1, 1), (5, 4), (9, 6)] {
        for alternate in [false, true] {
            for row in 0..rows {
                for col in 0..cols {
                    for mode in [0, 1, 8] {
                        let mut actual = seeded(cols, rows, alternate, row, col);
                        let mut original = seeded(cols, rows, alternate, row, col);
                        let row_ids: Vec<_> = (0..rows)
                            .map(|r| actual.vt.screen.grid.raw_row_id(r))
                            .collect();
                        if mode == 8 {
                            actual.decaln();
                            original.frozen_alignment();
                        } else {
                            actual.handle_escape_sequence(&[mode], b"", 'J', None, b"");
                            original.frozen_erase_display(mode);
                        }
                        assert_eq!(
                            parser_oracle_tests::snapshot(&mut actual),
                            parser_oracle_tests::snapshot(&mut original),
                            "{cols}x{rows} alternate={alternate} row={row} col={col} mode={mode}"
                        );
                        assert_eq!(
                            row_ids,
                            (0..rows)
                                .map(|r| actual.vt.screen.grid.raw_row_id(r))
                                .collect::<Vec<_>>()
                        );
                    }
                }
            }
        }
    }
}
