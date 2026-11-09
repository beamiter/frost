//! Differential oracle frozen from Frost's original physical grid before the
//! shared-storage migration. Keep its storage/mutation algorithms independent
//! of the engine; only names and the test-local ID allocator differ.
use super::*;
use std::cell::Cell;
use std::collections::HashMap;

thread_local! {
    static NEXT_ORIGINAL_ID: Cell<u64> = const { Cell::new(1) };
}

fn fresh_original_row_id() -> RawRowId {
    NEXT_ORIGINAL_ID.with(|next| {
        let id = next.get();
        next.set(id.checked_add(1).expect("bounded test identity corpus"));
        RawRowId::new(id)
    })
}

/// 连续内存网格存储 - 优化内存局部性和缓存命中率
/// 内存布局：`cells[row * cols + col]` 对应 `grid[row][col]`。
#[derive(Clone)]
pub struct OriginalGrid {
    cells: Vec<TerminalCell>,
    rows: usize,
    cols: usize,
    pub row_wrapped: Vec<bool>,
    row_ids: Vec<RawRowId>,
    identity_revision: u64,
}

#[allow(dead_code)]
impl OriginalGrid {
    pub fn new(rows: usize, cols: usize) -> Self {
        OriginalGrid {
            cells: vec![TerminalCell::default(); rows * cols],
            rows,
            cols,
            row_wrapped: vec![false; rows],
            row_ids: (0..rows).map(|_| fresh_original_row_id()).collect(),
            identity_revision: 1,
        }
    }

    #[inline]
    pub fn get(&self, row: usize, col: usize) -> &TerminalCell {
        &self.cells[row * self.cols + col]
    }

    #[inline]
    pub fn get_mut(&mut self, row: usize, col: usize) -> &mut TerminalCell {
        &mut self.cells[row * self.cols + col]
    }

    #[inline]
    pub fn rows(&self) -> usize {
        self.rows
    }

    #[inline]
    pub fn cols(&self) -> usize {
        self.cols
    }

    /// Identity of a retained physical grid row.
    #[inline]
    pub fn raw_row_id(&self, row: usize) -> Option<RawRowId> {
        self.row_ids.get(row).copied()
    }

    #[inline]
    fn bump_identity_revision(&mut self) {
        self.identity_revision = self.identity_revision.wrapping_add(1).max(1);
    }

    /// 获取行作Vec引用（用于兼容旧代码）
    pub fn get_row(&self, row: usize) -> Vec<TerminalCell> {
        let start = row * self.cols;
        let end = start + self.cols;
        self.cells[start..end].to_vec()
    }

    /// 返回行数（兼容 grid.len()）
    #[inline]
    #[allow(dead_code)]
    pub fn len(&self) -> usize {
        self.rows
    }

    /// 返回行数（兼容 `grid[i].len()`）。
    #[inline]
    pub fn row_len(&self) -> usize {
        self.cols
    }

    /// 设置整行
    #[allow(dead_code)]
    pub fn set_row(&mut self, row: usize, cells: Vec<TerminalCell>) {
        let start = row * self.cols;
        let copy_len = cells.len().min(self.cols);
        self.cells[start..start + copy_len].copy_from_slice(&cells[..copy_len]);
    }

    /// 获取所有行为 `Vec<Vec<_>>`（用于兼容旧代码）。
    pub fn to_vec(&self) -> Vec<Vec<TerminalCell>> {
        self.cells
            .chunks_exact(self.cols)
            .map(|chunk| chunk.to_vec())
            .collect()
    }

    /// 在行内指定列插入一个cell，右侧cell右移，末尾cell被丢弃
    #[cfg(test)]
    pub fn insert_cell_in_row(&mut self, row: usize, col: usize, cell: TerminalCell) {
        if row >= self.rows || col >= self.cols {
            return;
        }
        let start = row * self.cols;
        self.cells
            .copy_within(start + col..start + self.cols - 1, start + col + 1);
        self.cells[start + col] = cell;
    }

    /// 删除行内指定列的cell，右侧cell左移，末尾补blank
    #[cfg(test)]
    pub fn remove_cell_from_row(&mut self, row: usize, col: usize) {
        if row >= self.rows || col >= self.cols {
            return;
        }
        let start = row * self.cols;
        self.cells
            .copy_within(start + col + 1..start + self.cols, start + col);
        // Fill last cell with default
        self.cells[start + self.cols - 1] = TerminalCell::default();
    }

    /// 删除第一行，向上移动所有行，末尾补新行
    #[allow(dead_code)]
    pub fn remove_first_row(&mut self) -> (Vec<TerminalCell>, bool) {
        let removed = self.get_row(0);
        let was_wrapped = self.row_wrapped[0];
        self.shift_rows_up();
        (removed, was_wrapped)
    }

    /// Shift all rows up by one (discard first row, blank last row).
    /// Does not return the removed row - use get_row(0) before calling if needed.
    #[inline]
    pub fn shift_rows_up(&mut self) {
        self.cells.copy_within(self.cols.., 0);
        let last_start = (self.rows - 1) * self.cols;
        self.cells[last_start..].fill(TerminalCell::default());
        self.row_wrapped.copy_within(1.., 0);
        self.row_wrapped[self.rows - 1] = false;
        self.row_ids.copy_within(1.., 0);
        self.row_ids[self.rows - 1] = fresh_original_row_id();
        self.bump_identity_revision();
    }

    /// 是否为空
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.rows == 0
    }

    /// 调整网格大小
    pub fn resize(&mut self, new_rows: usize, new_cols: usize, default_cell: TerminalCell) {
        let mut new_cells = vec![default_cell; new_rows * new_cols];
        let copy_rows = self.rows.min(new_rows);
        let copy_cols = self.cols.min(new_cols);
        for row in 0..copy_rows {
            let src_start = row * self.cols;
            let dst_start = row * new_cols;
            new_cells[dst_start..dst_start + copy_cols]
                .copy_from_slice(&self.cells[src_start..src_start + copy_cols]);
            let line = &mut new_cells[dst_start..dst_start + new_cols];
            for col in 0..new_cols {
                if line[col].flags.wide()
                    && (col + 1 >= new_cols || !line[col + 1].flags.wide_continuation())
                {
                    line[col] = default_cell;
                }
            }
            for col in 0..new_cols {
                if line[col].flags.wide_continuation() && (col == 0 || !line[col - 1].flags.wide())
                {
                    line[col] = default_cell;
                }
            }
        }
        self.cells = new_cells;
        let mut new_wrapped = vec![false; new_rows];
        new_wrapped[..copy_rows].copy_from_slice(&self.row_wrapped[..copy_rows]);
        self.row_wrapped = new_wrapped;
        let mut new_row_ids: Vec<_> = (0..new_rows).map(|_| fresh_original_row_id()).collect();
        new_row_ids[..copy_rows].copy_from_slice(&self.row_ids[..copy_rows]);
        self.row_ids = new_row_ids;
        self.rows = new_rows;
        self.cols = new_cols;
        self.bump_identity_revision();
    }

    /// 获取mut访问所有行
    pub fn iter_mut(&mut self) -> std::slice::ChunksMut<'_, TerminalCell> {
        self.cells.chunks_mut(self.cols)
    }

    /// 获取只读访问所有行
    pub fn iter(&self) -> std::slice::Chunks<'_, TerminalCell> {
        self.cells.chunks(self.cols)
    }
}

impl std::ops::Index<usize> for OriginalGrid {
    type Output = [TerminalCell];
    fn index(&self, row: usize) -> &[TerminalCell] {
        let start = row * self.cols;
        &self.cells[start..start + self.cols]
    }
}

impl std::ops::IndexMut<usize> for OriginalGrid {
    fn index_mut(&mut self, row: usize) -> &mut [TerminalCell] {
        let start = row * self.cols;
        &mut self.cells[start..start + self.cols]
    }
}

#[derive(Default)]
struct IdentityMap {
    actual_to_original: HashMap<RawRowId, RawRowId>,
    original_to_actual: HashMap<RawRowId, RawRowId>,
}

impl IdentityMap {
    fn assert_grid(&mut self, actual: &TerminalGrid, original: &OriginalGrid) {
        assert_eq!(
            (actual.rows(), actual.cols()),
            (original.rows(), original.cols())
        );
        assert_eq!(actual.len(), original.len());
        assert_eq!(actual.row_len(), original.row_len());
        assert_eq!(actual.is_empty(), original.is_empty());
        assert_eq!(actual.identity_revision, original.identity_revision);
        assert_eq!(actual.row_wrapped(), original.row_wrapped);
        for row in 0..actual.rows() {
            assert_eq!(&actual[row], &original[row]);
            assert_eq!(actual.get_row(row), original.get_row(row));
            assert_eq!(actual.row_slice(row), Some(&original[row]));
            let a = actual.raw_row_id(row).unwrap();
            let o = original.raw_row_id(row).unwrap();
            assert_eq!(a.is_tracked(), o.is_tracked());
            if a.is_tracked() {
                // A bijection across every step distinguishes genuinely fresh
                // IDs from identities retained/moved by earlier operations.
                if let Some(previous) = self.actual_to_original.insert(a, o) {
                    assert_eq!(previous, o, "a retained row was given the wrong identity");
                }
                if let Some(previous) = self.original_to_actual.insert(o, a) {
                    assert_eq!(previous, a, "a retained identity was replaced or aliased");
                }
            }
        }
        assert_eq!(actual.raw_row_id(actual.rows()), None);
        assert_eq!(actual.raw_row_id(usize::MAX), None);
        assert!(actual.row_slice(actual.rows()).is_none());
        if actual.cols() > 0 {
            assert_eq!(actual.to_vec(), original.to_vec());
            assert_eq!(
                actual.iter().collect::<Vec<_>>(),
                original.iter().collect::<Vec<_>>()
            );
        }
    }
}

fn next(seed: &mut u64) -> u64 {
    *seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
    *seed
}

fn sample_cell(seed: &mut u64) -> TerminalCell {
    let value = next(seed);
    let mut cell = TerminalCell {
        character: [' ', 'x', '界', 'é', '\0'][(value >> 8) as usize % 5],
        foreground: Color::Indexed((value >> 16) as u8),
        background: if value & 1 == 0 {
            Color::Default
        } else {
            Color::Blue
        },
        hyperlink_id: HyperlinkId::from_raw((value >> 32) as u16),
        ..Default::default()
    };
    cell.flags.set_blink(value & 2 != 0);
    cell.flags.set_wide(value & 4 != 0);
    cell.flags.set_wide_continuation(value & 8 != 0);
    cell
}

#[test]
fn shared_grid_matches_original_for_seeded_storage_operations() {
    for case in 0..128u64 {
        let mut seed = 0x4652_4f53_5447_5244u64.wrapping_add(case);
        let rows = next(&mut seed) as usize % 6;
        let cols = next(&mut seed) as usize % 9;
        let mut actual = TerminalGrid::new(rows, cols);
        let mut original = OriginalGrid::new(rows, cols);
        let mut identity = IdentityMap::default();
        identity.assert_grid(&actual, &original);
        for _step in 0..256 {
            let op = next(&mut seed) % 10;
            let row = next(&mut seed) as usize % (actual.rows() + 3);
            let col = next(&mut seed) as usize % (actual.cols() + 3);
            let cell = sample_cell(&mut seed);
            match op {
                0 => {
                    let rows = next(&mut seed) as usize % 6;
                    let cols = next(&mut seed) as usize % 9;
                    actual.resize(rows, cols, cell);
                    original.resize(rows, cols, cell);
                }
                1 if row < actual.rows() && col < actual.cols() => {
                    *actual.get_mut(row, col) = cell;
                    *original.get_mut(row, col) = cell;
                    assert_eq!(actual.get(row, col), original.get(row, col));
                }
                2 if row < actual.rows() => {
                    let cells = vec![cell; col];
                    actual.set_row(row, cells.clone());
                    original.set_row(row, cells);
                }
                3 => {
                    actual.insert_cell_in_row(row, col, cell);
                    original.insert_cell_in_row(row, col, cell);
                }
                4 => {
                    actual.remove_cell_from_row(row, col);
                    original.remove_cell_from_row(row, col);
                }
                5 if actual.rows() > 0 => {
                    actual.shift_rows_up();
                    original.shift_rows_up();
                }
                6 if row < actual.rows() => {
                    actual.row_wrapped_mut()[row] = cell.flags.blink();
                    original.row_wrapped[row] = cell.flags.blink();
                }
                7 if row < actual.rows() => {
                    actual.row_ids_mut()[row] = RawRowId::UNTRACKED;
                    original.row_ids[row] = RawRowId::UNTRACKED;
                }
                8 if actual.cols() > 0 => {
                    for cells in actual.iter_mut() {
                        cells.fill(cell);
                    }
                    for cells in original.iter_mut() {
                        cells.fill(cell);
                    }
                }
                9 if actual.rows() > 0 => {
                    assert_eq!(actual.remove_first_row(), original.remove_first_row());
                }
                _ => {}
            }
            identity.assert_grid(&actual, &original);
            identity.assert_grid(&actual.clone(), &original.clone());
        }
    }
}

#[test]
fn zero_and_small_grid_resizes_match_original() {
    for rows in 0..=3 {
        for cols in 0..=3 {
            for new_rows in 0..=3 {
                for new_cols in 0..=3 {
                    let mut actual = TerminalGrid::new(rows, cols);
                    let mut original = OriginalGrid::new(rows, cols);
                    let mut identity = IdentityMap::default();
                    identity.assert_grid(&actual, &original);
                    if rows > 0 && cols > 0 {
                        let mut lead = TerminalCell {
                            character: '界',
                            ..Default::default()
                        };
                        lead.flags.set_wide(true);
                        actual[0][0] = lead;
                        original[0][0] = lead;
                        if cols > 1 {
                            let mut continuation = TerminalCell::default();
                            continuation.flags.set_wide_continuation(true);
                            actual[0][1] = continuation;
                            original[0][1] = continuation;
                        }
                        actual.row_wrapped_mut()[0] = true;
                        original.row_wrapped[0] = true;
                    }
                    let blank = TerminalCell {
                        background: Color::Blue,
                        ..Default::default()
                    };
                    actual.resize(new_rows, new_cols, blank);
                    original.resize(new_rows, new_cols, blank);
                    identity.assert_grid(&actual, &original);
                    actual.resize(rows, cols, TerminalCell::default());
                    original.resize(rows, cols, TerminalCell::default());
                    identity.assert_grid(&actual, &original);
                }
            }
        }
    }
}

#[test]
fn zero_grid_invalid_operations_keep_original_panic_contract() {
    use std::panic::{catch_unwind, AssertUnwindSafe};
    for cols in [0, 1, 3] {
        let mut actual = TerminalGrid::new(0, cols);
        let mut original = OriginalGrid::new(0, cols);
        assert!(catch_unwind(AssertUnwindSafe(|| actual.shift_rows_up())).is_err());
        assert!(catch_unwind(AssertUnwindSafe(|| original.shift_rows_up())).is_err());
    }
    for rows in [0, 1, 3] {
        let actual = TerminalGrid::new(rows, 0);
        let original = OriginalGrid::new(rows, 0);
        assert!(catch_unwind(|| actual.to_vec()).is_err());
        assert!(catch_unwind(|| original.to_vec()).is_err());
        assert!(catch_unwind(|| actual.iter()).is_err());
        assert!(catch_unwind(|| original.iter()).is_err());
        assert_eq!(actual.get_row(0), original.get_row(0));
    }
}

#[test]
fn row_identity_and_revision_preserve_frost_host_policy() {
    let mut grid = TerminalGrid::new(3, 2);
    let first = grid.row_ids().to_vec();
    assert!(first.iter().all(|id| id.is_tracked()));
    assert_eq!(
        first.iter().collect::<std::collections::HashSet<_>>().len(),
        3
    );
    grid.row_ids_mut()[1] = RawRowId::UNTRACKED;
    grid.row_wrapped_mut()[1] = true;
    grid.shift_rows_up();
    assert_eq!(grid.identity_revision, 2);
    assert_eq!(grid.row_ids()[0], RawRowId::UNTRACKED);
    assert_eq!(grid.row_ids()[1], first[2]);
    assert!(grid.row_ids()[2].is_tracked());
    assert!(!first.contains(&grid.row_ids()[2]));
    assert_eq!(grid.row_wrapped(), [true, false, false]);
    let retained = grid.row_ids().to_vec();
    grid.resize(5, 1, TerminalCell::default());
    assert_eq!(grid.identity_revision, 3);
    assert_eq!(&grid.row_ids()[..3], retained);
    assert!(grid.row_ids()[3..]
        .iter()
        .all(|id| id.is_tracked() && !retained.contains(id)));
    assert_ne!(grid.row_ids()[3], grid.row_ids()[4]);
    grid.identity_revision = u64::MAX;
    grid.resize(5, 1, TerminalCell::default());
    assert_eq!(grid.identity_revision, 1);
    grid.resize(0, 0, TerminalCell::default());
    assert_eq!(grid.identity_revision, 2);
    assert!(grid.row_ids().is_empty());
    grid.resize(1, 1, TerminalCell::default());
    assert!(grid.raw_row_id(0).unwrap().is_tracked());
    assert!(!retained.contains(&grid.raw_row_id(0).unwrap()));
}

#[test]
fn host_grid_views_alias_the_engine_storage() {
    let mut grid = TerminalGrid::new(2, 3);
    assert!(std::ptr::eq(grid.get(0, 0), grid.storage.get(0, 0)));
    assert!(std::ptr::eq(
        grid.row_ids().as_ptr(),
        grid.storage.row_ids().as_ptr()
    ));
    assert!(std::ptr::eq(
        grid.row_wrapped().as_ptr(),
        grid.storage.row_wrapped.as_ptr()
    ));
    grid.storage[0][0].character = 'x';
    assert_eq!(grid[0][0].character, 'x');
    grid[1][2].character = 'y';
    assert_eq!(grid.storage[1][2].character, 'y');
    grid.cells_mut()[1].character = 'z';
    assert_eq!(grid.storage[0][1].character, 'z');
    let clone = grid.clone();
    assert!(!std::ptr::eq(grid.get(0, 0), clone.get(0, 0)));
    assert_eq!(grid.row_ids(), clone.row_ids());
    assert_eq!(grid.identity_revision, clone.identity_revision);
}
