//! Raw-copy bodies frozen before the shared inclusive-selection extraction.
//! Expected bytes retain the native profile's original bounds and whitespace.
use super::*;

impl TerminalState {
    fn frozen_raw_copy(&self) -> Option<String> {
        if self.projection_failure.is_some() {
            return None;
        }
        if let Some(selection) = &self.inline_text_selection {
            return (!self.is_alt_buffer_active())
                .then(|| selection.copy())
                .flatten();
        }
        if self.projected_selection.is_some() {
            return self.copy_projected_selection();
        }
        self.selection.map(|sel| {
            let (start, end) = if sel.anchor <= sel.active {
                (sel.anchor, sel.active)
            } else {
                (sel.active, sel.anchor)
            };
            let mut result = String::new();
            let scrollback_len = self.scrollback.len();
            let grid_rows = self.vt.screen.grid.rows();
            let cols = self.vt.screen.grid.row_len();
            let total_rows = scrollback_len + grid_rows;

            let block = matches!(sel.mode, SelectionMode::Block);
            let last_abs_row = end.0.min(total_rows.saturating_sub(1));
            for abs_row in start.0..=last_abs_row {
                let (start_col, end_col) = if block {
                    // Rectangular: same column span on every row.
                    let lo = sel.anchor.1.min(sel.active.1);
                    let hi = sel.anchor.1.max(sel.active.1);
                    (lo, hi.min(cols.saturating_sub(1)))
                } else {
                    let s = if abs_row == start.0 { start.1 } else { 0 };
                    let e = if abs_row == end.0 {
                        end.1.min(cols.saturating_sub(1))
                    } else {
                        cols.saturating_sub(1)
                    };
                    (s, e)
                };

                let row_is_wrapped = if abs_row < scrollback_len {
                    self.scrollback[abs_row].is_wrapped
                } else {
                    let grid_row = abs_row - scrollback_len;
                    grid_row < grid_rows && self.vt.screen.grid.row_wrapped[grid_row]
                };

                if abs_row < scrollback_len {
                    // Read from scrollback
                    let line = decompress_history_row(&self.scrollback[abs_row]);
                    let end = end_col.min(line.len().saturating_sub(1));
                    for cell in line.iter().take(end + 1).skip(start_col) {
                        if !cell.flags.wide_continuation() {
                            result.push(cell.character);
                        }
                    }
                } else {
                    // Read from current grid
                    let grid_row = abs_row - scrollback_len;
                    if grid_row < grid_rows {
                        for col in start_col..=end_col {
                            let cell = self.vt.screen.grid.get(grid_row, col);
                            if !cell.flags.wide_continuation() {
                                result.push(cell.character);
                            }
                        }
                    }
                }

                // In block mode each row is an independent record; always
                // separate with newline. Otherwise, soft-wrapped rows continue
                // onto the next physical row without a hard newline.
                if abs_row < last_abs_row && (block || !row_is_wrapped) {
                    result.push('\n');
                }
            }

            result
        })
    }
}

fn seeded() -> TerminalState {
    let mut t = TerminalState::new(6, 4);
    for (index, width) in [3, 9, 0, 6].into_iter().enumerate() {
        let mut cells = vec![TerminalCell::default(); width];
        for (col, cell) in cells.iter_mut().enumerate() {
            cell.character = if col + 2 >= width {
                ' '
            } else {
                char::from(b'a' + col as u8)
            };
        }
        if width >= 3 {
            cells[1].character = '界';
            cells[1].flags.set_wide(true);
            cells[2].flags.set_wide_continuation(true);
        }
        t.scrollback
            .push_back(ScrollbackLine::compress_frost_legacy(
                &cells,
                index % 2 == 0,
            ));
    }
    for row in 0..4 {
        for (col, character) in ['a', '界', ' ', '\u{00a0}', ' ', ' ']
            .into_iter()
            .enumerate()
        {
            let cell = t.vt.screen.grid.get_mut(row, col);
            cell.character = character;
            cell.flags.set_wide(col == 1);
            cell.flags.set_wide_continuation(col == 2);
        }
        t.vt.screen.grid.row_wrapped[row] = row % 2 == 0;
    }
    t
}

#[test]
fn shared_raw_copy_matches_frozen_native_bytes() {
    let mut t = seeded();
    let total = t.scrollback.len() + t.vt.screen.grid.rows();
    let points: Vec<_> = [0, 1, t.scrollback.len(), total - 1, total, total + 1]
        .into_iter()
        .flat_map(|row| {
            [0, 1, 2, 5, 8, usize::MAX]
                .into_iter()
                .map(move |col| (row, col))
        })
        .collect();
    for &anchor in &points {
        for &active in &points {
            for block in [false, true] {
                t.selection = Some(Selection {
                    anchor,
                    active,
                    mode: if block {
                        SelectionMode::Block
                    } else {
                        SelectionMode::Normal
                    },
                });
                {
                    assert_eq!(
                        t.copy_displayed_selection(),
                        t.frozen_raw_copy(),
                        "anchor={anchor:?} active={active:?} block={block}"
                    );
                }
            }
        }
    }
    t.selection = None;
    {
        assert_eq!(t.copy_displayed_selection(), t.frozen_raw_copy());
    }
}

#[test]
fn shared_raw_copy_keeps_retention_and_resize_selection_contracts() {
    for cap in [1, 2, 4] {
        for reverse in [false, true] {
            for block in [false, true] {
                let mut t = TerminalState::new(6, 2);
                t.set_max_scrollback(cap);
                t.process_input(b"one\r\ntwo\r\nthree\r\nfour");
                let (anchor, active) = if reverse {
                    ((2, 4), (0, 1))
                } else {
                    ((0, 1), (2, 4))
                };
                t.selection = Some(Selection {
                    anchor,
                    active,
                    mode: if block {
                        SelectionMode::Block
                    } else {
                        SelectionMode::Normal
                    },
                });
                for bytes in [b"".as_slice(), b"\r\nnext", b"\r\nlast"] {
                    t.process_input(bytes);
                    {
                        assert_eq!(t.copy_displayed_selection(), t.frozen_raw_copy());
                    }
                }
                t.set_max_scrollback(1);
                {
                    assert_eq!(t.copy_displayed_selection(), t.frozen_raw_copy());
                }
                t.on_resize(3, 3);
                {
                    assert_eq!(t.copy_displayed_selection(), t.frozen_raw_copy());
                }
            }
        }
    }
}

#[test]
fn empty_raw_store_matches_frozen_copy_without_indexing_a_live_row() {
    let mut t = seeded();
    t.scrollback.clear();
    t.vt.screen.grid = TerminalGrid::new(0, 6);
    t.selection = Some(Selection {
        anchor: (0, 0),
        active: (9, 3),
        mode: SelectionMode::Normal,
    });
    assert_eq!(t.copy_displayed_selection(), t.frozen_raw_copy());
}
