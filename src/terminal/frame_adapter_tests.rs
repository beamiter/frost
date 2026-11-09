use super::*;

fn snapshots() -> Vec<String> {
    let mut terminal = TerminalState::new(9, 3);
    let mut out = Vec::new();
    for (step, bytes) in [
        b"".as_slice(),
        b"hello",
        b"\x1b[2;3H",
        b"X",
        b"\x1b[31mred\x1b[0m",
        "中e\u{301}".as_bytes(),
        b"\r\nnext\r\nlast\r\nscroll",
        b"\x1b[1;1Htop",
        b"\x1b[?1049hALT\x1b[?25l",
        b"\x1b[?1049l\x1b[?25h",
    ]
    .into_iter()
    .enumerate()
    {
        terminal.process_batch(bytes);
        if step == 8 {
            terminal.on_resize(7, 4);
        }
        if step == 9 {
            terminal.on_resize(9, 3);
        }
        for offset in [0, 1, 0] {
            terminal.scroll_offset = offset;
            let viewport = terminal.get_projected_viewport(true);
            out.push(format!(
                "{:?}|{:?}|{:?}|{:?}",
                viewport.cells(),
                viewport.row_wrapped(),
                terminal.get_cursor_pos(),
                terminal.is_cursor_visible()
            ));
        }
    }
    out
}

#[test]
fn frame_adapter_original_output_oracle() {
    let actual = snapshots();
    let expected: Vec<String> = serde_json::from_str(include_str!(
        "../../tests/fixtures/frame-original-output.json"
    ))
    .unwrap();
    assert_eq!(actual, expected);
}

// Frozen before the shared-frame integration from the recovered parser/grid
// baseline on Frost 4c1196f. This retains the old Arc<Vec<Vec<Cell>>> ownership
// behavior, including the unique-owner incremental fast path.
impl TerminalState {
    fn get_live_cell_frame(&mut self) -> Arc<jterm_engine::frame::CellFrame> {
        Arc::new(
            self.get_visible_cells()
                .raw_frame()
                .expect("live frame")
                .clone(),
        )
    }
    fn original_visible_cells(&mut self) -> std::sync::Arc<Vec<Vec<TerminalCell>>> {
        if self.use_alt_buffer {
            self.scroll_offset = 0;
        }
        if let Some((cached_version, cached_offset, ref cells, _)) =
            self.original_visible_cells_cache
        {
            if cached_version == self.grid_version && cached_offset == self.scroll_offset {
                return std::sync::Arc::clone(cells);
            }
        }

        // Cache miss - rebuild
        let rows = self.grid.rows();
        let cols = if rows > 0 { self.grid.row_len() } else { 80 };

        // Try to recycle the previous allocation. The renderer drops its returned
        // Arc each frame, so by the next miss we are usually the sole owner and can
        // refill the existing nested Vecs in place instead of reallocating per row.
        let prev = self.original_visible_cells_cache.take();
        let prev_version = prev.as_ref().map(|(v, _, _, _)| *v);
        let prev_offset = prev.as_ref().map(|(_, o, _, _)| *o);
        let mut recycled = prev.map(|(_, _, a, _)| a);

        if self.scroll_offset == 0 {
            // Fast path: copy current grid, reusing inner Vec capacity when possible.
            if let Some(buf) = recycled.as_mut().and_then(std::sync::Arc::get_mut) {
                // Incremental path: if the recycled buffer already holds a same-sized
                // snapshot taken at scroll_offset==0, only re-copy rows whose
                // row_versions changed since that snapshot. Untouched rows already
                // hold valid data, turning an O(rows*cols) copy into O(dirty cells).
                let can_incremental = prev_offset == Some(0)
                    && buf.len() == rows
                    && buf.iter().all(|r| r.len() == cols);
                if can_incremental {
                    let base = prev_version.unwrap_or(0);
                    for (r, (dst, chunk)) in buf.iter_mut().zip(self.grid.iter()).enumerate() {
                        if self.row_versions[r] > base {
                            dst.clear();
                            dst.extend_from_slice(chunk);
                        }
                    }
                } else {
                    buf.resize_with(rows, Vec::new);
                    for (dst, chunk) in buf.iter_mut().zip(self.grid.iter()) {
                        dst.clear();
                        dst.extend_from_slice(chunk);
                    }
                }
                let arc = recycled.unwrap();
                self.original_visible_cells_cache = Some((
                    self.grid_version,
                    self.scroll_offset,
                    std::sync::Arc::clone(&arc),
                    None,
                ));
                return arc;
            }
        }

        let (cells, origin_spans) = if self.scroll_offset == 0 {
            // Fast path (shared allocation): fresh copy of current grid.
            (self.grid.to_vec(), None)
        } else {
            let (cells, spans) = self.materialize_scrolled_viewport(rows, cols);
            (cells, Some(std::sync::Arc::new(spans)))
        };

        // Reuse the recycled Arc's outer allocation if we still solely own it.
        let arc = match recycled.as_mut().and_then(std::sync::Arc::get_mut) {
            Some(buf) => {
                *buf = cells;
                recycled.unwrap()
            }
            None => std::sync::Arc::new(cells),
        };
        self.original_visible_cells_cache = Some((
            self.grid_version,
            self.scroll_offset,
            std::sync::Arc::clone(&arc),
            origin_spans,
        ));
        arc
    }
}

fn new_frame_cells(
    old: &jterm_engine::frame::CellFrame,
    new: &jterm_engine::frame::CellFrame,
) -> usize {
    new.rows()
        .iter()
        .enumerate()
        .filter(|(i, row)| {
            old.rows()
                .get(*i)
                .is_none_or(|prev| !Arc::ptr_eq(&prev.cells, &row.cells))
        })
        .map(|(_, row)| row.cells.len())
        .sum()
}
fn new_control_cells(old: &[Vec<TerminalCell>], new: &[Vec<TerminalCell>]) -> usize {
    new.iter()
        .enumerate()
        .filter(|(i, row)| old.get(*i).is_none_or(|prev| prev.as_ptr() != row.as_ptr()))
        .map(|(_, row)| row.len())
        .sum()
}

#[test]
fn frame_adapter_retained_copy_control() {
    let mut terminal = TerminalState::new(9, 3);
    terminal.process_batch(b"abc");
    let old_control = terminal.original_visible_cells();
    let old_frame = terminal.get_live_cell_frame();
    let unchanged_control = terminal.original_visible_cells();
    let unchanged_frame = terminal.get_live_cell_frame();
    assert_eq!(new_control_cells(&old_control, &unchanged_control), 0);
    assert_eq!(new_frame_cells(&old_frame, &unchanged_frame), 0);
    terminal.process_batch(b"\x1b[2;2H");
    let cursor_control = terminal.original_visible_cells();
    let cursor_frame = terminal.get_live_cell_frame();
    assert_eq!(new_control_cells(&old_control, &cursor_control), 27);
    assert_eq!(new_frame_cells(&old_frame, &cursor_frame), 0);
    assert_ne!(old_frame.cursor(), cursor_frame.cursor());
    terminal.process_batch(b"X");
    let row_control = terminal.original_visible_cells();
    let row_frame = terminal.get_live_cell_frame();
    assert_eq!(new_control_cells(&cursor_control, &row_control), 27);
    assert_eq!(new_frame_cells(&cursor_frame, &row_frame), 9);
    assert_eq!(
        row_frame
            .rows()
            .iter()
            .map(|r| r.cells.to_vec())
            .collect::<Vec<_>>(),
        *row_control
    );
    // Host cursor preferences can change without a PTY/grid revision.
    terminal.cursor_shape = CursorShape::Beam;
    let shape_frame = terminal.get_live_cell_frame();
    assert_eq!(shape_frame.cursor().shape, CursorShape::Beam);
    assert_eq!(new_frame_cells(&row_frame, &shape_frame), 0);
    terminal.modes.remove(&25);
    let hidden = terminal.get_live_cell_frame();
    assert!(!hidden.cursor().visible);
    terminal.modes.insert(25);
    let shown = terminal.get_live_cell_frame();
    assert!(shown.cursor().visible);
    assert_eq!(new_frame_cells(&hidden, &shown), 0);

    let before = terminal.get_projected_viewport(false);
    terminal.cursor_shape = CursorShape::Underline;
    let after = terminal.get_projected_viewport(false);
    assert_eq!(
        before.cells.raw_frame().unwrap().cursor().shape,
        CursorShape::Beam
    );
    assert_eq!(
        after.cells.raw_frame().unwrap().cursor().shape,
        CursorShape::Underline
    );
    assert_eq!(
        new_frame_cells(
            before.cells.raw_frame().unwrap(),
            after.cells.raw_frame().unwrap()
        ),
        0
    );
}

#[test]
fn frame_adapter_held_snapshot_is_immutable() {
    let mut terminal = TerminalState::new(9, 3);
    terminal.process_batch(b"first\r\nsecond");
    let held = terminal.get_live_cell_frame();
    let expected = held.as_ref().clone();
    terminal.process_batch(b"\x1b[1;1HX\x1b[2;1HY");
    let later = terminal.get_live_cell_frame();
    assert_eq!(*held, expected);
    assert_ne!(held.rows()[0].cells, later.rows()[0].cells);
    assert!(Arc::ptr_eq(&held.rows()[2].cells, &later.rows()[2].cells));
    assert!(later.stamp().revision > held.stamp().revision);
}

#[test]
fn frame_adapter_layout_and_alternate_are_full_boundaries() {
    let mut terminal = TerminalState::new(9, 3);
    terminal.process_batch(b"primary");
    let primary = terminal.get_live_cell_frame();
    terminal.process_batch(b"\x1b[?1049hALT");
    let alternate = terminal.get_live_cell_frame();
    assert_eq!(
        alternate.stamp().screen,
        jterm_engine::frame::Screen::Alternate
    );
    assert!(primary.delta_to(&alternate).is_err());
    terminal.on_resize(7, 4);
    let resized = terminal.get_live_cell_frame();
    assert_eq!((resized.cols(), resized.rows().len()), (7, 4));
    assert!(alternate.delta_to(&resized).is_err());
    terminal.process_batch(b"\x1b[?1049l");
    let restored = terminal.get_live_cell_frame();
    assert_eq!(
        restored.stamp().screen,
        jterm_engine::frame::Screen::Primary
    );
    assert_eq!(primary.rows()[0].cells[0].character, 'p');
    terminal.process_batch(b"\x1b[4;7H\x1b7");
    terminal.on_resize(2, 1);
    terminal.process_batch(b"\x1b8");
    let shrunk = terminal.get_live_cell_frame();
    assert!(shrunk.cursor().row < 1 && shrunk.cursor().col < 2);
    // Legacy host state could temporarily be outside the new viewport.
    terminal.cursor_row = 3;
    terminal.cursor_col = 6;
    let exceptional = terminal.get_live_cell_frame();
    assert_eq!(exceptional.cursor().paint_position(), (0, 1));
    assert!(!exceptional.cursor().visible);
    assert_eq!(terminal.get_cursor_pos(), (3, 6));
}

#[test]
fn frame_adapter_native_rows_do_not_materialize_compatibility_cells() {
    let mut terminal = TerminalState::new(9, 3);
    terminal.process_batch(b"abcdefghi");
    let frame = terminal.get_live_cell_frame();
    let view = terminal.get_projected_viewport(false);
    assert!(!view.cells.compatibility_materialized());
    assert!(view.cells.raw_frame().is_some());
    assert_eq!(view.row_view()[0].as_ptr(), frame.rows()[0].cells.as_ptr());
    assert_eq!(view.row_view().len(), 3);
    assert!(!view.cells.compatibility_materialized());
    assert!(frame.cursor().pending_wrap);
    assert_eq!(frame.cursor().paint_position(), terminal.get_cursor_pos());
    assert_eq!(view.cells()[0], *frame.rows()[0].cells);
    assert!(view.cells.compatibility_materialized());
}
