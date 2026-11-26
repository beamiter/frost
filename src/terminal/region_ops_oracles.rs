//! Frozen Frost structural CSI dispatch and pre-shift history capture.
//! The expected path retains its original operation loop and native callback order.
use super::*;
use std::cell::RefCell;
thread_local! { static TRACE: RefCell<Option<Vec<String>>> = const { RefCell::new(None) }; }

pub(super) fn observe_dirty(t: &TerminalState, start: usize, end: usize) {
    TRACE.with(|slot| {
        if let Some(trace) = slot.borrow_mut().as_mut() {
            let history: Vec<_> = t
                .scrollback
                .iter()
                .map(|line| (line.decompress(), line.is_wrapped))
                .collect();
            trace.push(format!(
                "{:?}",
                (
                    start,
                    end,
                    t.vt.screen.grid.to_vec(),
                    t.vt.screen.grid.row_wrapped.as_slice(),
                    history,
                    &t.row_versions,
                    t.grid_version,
                    t.history_revision,
                    t.scroll_offset,
                    t.scrollback_pushes,
                    t.full_screen_scroll_revision
                )
            ));
        }
    });
}

impl TerminalState {
    fn frozen_region_dispatch(&mut self, params: &[u16], cmd: char) {
        let count = params.first().copied().unwrap_or(1).max(1) as usize;
        match cmd {
            'L' | 'M' => {
                let direction = if cmd == 'L' {
                    RegionDirection::Down
                } else {
                    RegionDirection::Up
                };
                let damage = jterm_engine::semantics::frost_legacy_splice_lines(
                    &mut self.vt.screen,
                    direction,
                    count,
                );
                self.mark_rows_dirty(*damage.start(), *damage.end());
            }
            'S' | 'T' => {
                let direction = if cmd == 'S' {
                    RegionDirection::Up
                } else {
                    RegionDirection::Down
                };
                let height = self
                    .vt.screen
                    .scroll_region_bottom
                    .saturating_sub(self.vt.screen.scroll_region_top)
                    .saturating_add(1);
                for _ in 0..count.min(height) {
                    self.frozen_scroll_region_one(direction);
                }
            }
            _ => {}
        }
    }
    fn frozen_scroll_region_one(&mut self, direction: RegionDirection) {
        let top = self.vt.screen.scroll_region_top;
        let bottom = self.vt.screen.scroll_region_bottom;
        if top > bottom || bottom >= self.vt.screen.grid.rows() {
            return;
        }

        // Preserve the first row only on an upward shift from the first screen
        // row. Synchronized alternate content can be archived unless an inline
        // CLI owns the screen. The host keeps this admission policy.
        let allow_alt_scrollback = self.vt.screen.use_alt_buffer
            && self.vt.sync_output.active
            && !self.inline_cli_owner_active();
        let scrollback_line = (direction == RegionDirection::Up
            && top == 0
            && (!self.vt.screen.use_alt_buffer || allow_alt_scrollback))
            .then(|| self.vt.screen.grid.archive_frost_legacy_row(top));

        let Some(effect) = self.vt.screen.scroll_region_one(
            direction,
            top,
            bottom,
            false,
            RawRowId::allocate_process_unique,
        ) else {
            return;
        };
        self.mark_rows_dirty(effect.top, effect.bottom);

        if let Some(line) = scrollback_line {
            self.push_scrollback_compressed_with_options(line, allow_alt_scrollback);
            if !self.vt.screen.use_alt_buffer && bottom + 1 == self.vt.screen.grid.rows() {
                self.full_screen_scroll_revision = self.full_screen_scroll_revision.wrapping_add(1);
            }
        }
    }
}

fn seeded(context: usize, geometry: usize, cursor: usize) -> TerminalState {
    let mut t = TerminalState::new(6, 5);
    t.set_max_scrollback(3);
    t.process_input(b"first\r\nsecond\r\nthird\r\nfourth\r\nfifth\r\nsixth\r\nseventh");
    if context & 1 != 0 {
        t.process_input(b"\x1b[?1049h");
    }
    if context & 2 != 0 {
        t.vt.sync_output.active = true;
    }
    if context & 4 != 0 {
        t.set_inline_cli_launch(true);
    }
    t.process_input(b"\x1b[44m");
    for row in 0..5 {
        t.vt.screen.grid.row_wrapped[row] = row & 1 != 0;
        for col in 0..6 {
            t.vt.screen.grid.get_mut(row, col).character = char::from(b'A' + row as u8);
        }
    }
    let (top, bottom) = [(0, 4), (1, 3), (2, 2), (3, 1), (0, 7)][geometry];
    t.vt.screen.scroll_region_top = top;
    t.vt.screen.scroll_region_bottom = bottom;
    t.vt.screen.cursor_row = cursor;
    t.vt.screen.pending_wrap = true;
    t.set_scroll_offset(1);
    t
}

fn run(
    mut t: TerminalState,
    params: &[u16],
    cmd: char,
    prefix: Option<u8>,
    intermediate: &[u8],
    original: bool,
) -> (
    std::collections::BTreeMap<String, serde_json::Value>,
    Vec<String>,
) {
    TRACE.with(|slot| *slot.borrow_mut() = Some(Vec::new()));
    if original {
        t.frozen_region_dispatch(params, cmd);
    } else {
        t.handle_escape_sequence(params, b"", cmd, prefix, intermediate);
    }
    let trace = TRACE.with(|slot| slot.borrow_mut().take().unwrap());
    (parser_oracle_tests::snapshot(&mut t), trace)
}

#[test]
fn region_ops_match_frozen_dispatch_effect_order_and_host_state() {
    for context in [0, 1, 2, 3, 7] {
        for geometry in 0..5 {
            for cursor in [0, 2, 4] {
                for cmd in ['L', 'M', 'S', 'T'] {
                    for params in [vec![], vec![0], vec![1], vec![2], vec![u16::MAX]] {
                        for (prefix, intermediate) in [
                            (None, b"".as_slice()),
                            (Some(b'?'), b""),
                            (Some(b'>'), b"! "),
                        ] {
                            assert_eq!(run(seeded(context, geometry, cursor), &params, cmd, prefix, intermediate, false), run(seeded(context, geometry, cursor), &params, cmd, prefix, intermediate, true), "context={context} geometry={geometry} cursor={cursor} cmd={cmd} params={params:?} prefix={prefix:?} intermediate={intermediate:?}");
                        }
                    }
                }
            }
        }
    }
}
