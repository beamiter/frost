//! Immutable paint storage. History and synthetic projections retain host policy.
use super::TerminalCell;
use jterm_engine::frame::CellFrame;
use std::sync::OnceLock;

#[derive(Clone, Debug)]
enum Storage {
    Raw(CellFrame),
    Materialized(Vec<Vec<TerminalCell>>),
}

/// Raw rows remain shared even while older viewports are retained. Only explicit
/// legacy slice consumers allocate a nested-Vec compatibility copy.
#[derive(Clone, Debug)]
pub struct VisibleRows {
    storage: Storage,
    compatibility: OnceLock<Vec<Vec<TerminalCell>>>,
}
impl VisibleRows {
    pub fn raw(frame: CellFrame) -> Self {
        Self {
            storage: Storage::Raw(frame),
            compatibility: OnceLock::new(),
        }
    }
    pub fn materialized(rows: Vec<Vec<TerminalCell>>) -> Self {
        Self {
            storage: Storage::Materialized(rows),
            compatibility: OnceLock::new(),
        }
    }
    pub fn raw_frame(&self) -> Option<&CellFrame> {
        match &self.storage {
            Storage::Raw(frame) => Some(frame),
            Storage::Materialized(_) => None,
        }
    }
    pub fn view(&self) -> RowView<'_> {
        match &self.storage {
            Storage::Raw(frame) => RowView::Frame(frame),
            Storage::Materialized(rows) => RowView::Materialized(rows),
        }
    }
    pub fn len(&self) -> usize {
        self.view().len()
    }
    pub fn is_empty(&self) -> bool {
        self.view().is_empty()
    }
    pub fn first(&self) -> Option<&[TerminalCell]> {
        self.view().first()
    }
    pub fn iter(&self) -> impl ExactSizeIterator<Item = &[TerminalCell]> + DoubleEndedIterator {
        self.view().iter()
    }
    #[cfg(test)]
    pub fn compatibility_materialized(&self) -> bool {
        self.compatibility.get().is_some()
    }
}
impl std::ops::Deref for VisibleRows {
    type Target = [Vec<TerminalCell>];
    fn deref(&self) -> &Self::Target {
        match &self.storage {
            Storage::Materialized(rows) => rows,
            Storage::Raw(_) => self
                .compatibility
                .get_or_init(|| self.view().iter().map(<[_]>::to_vec).collect()),
        }
    }
}
impl std::ops::Index<usize> for VisibleRows {
    type Output = [TerminalCell];
    fn index(&self, row: usize) -> &Self::Output {
        self.view().get(row).expect("row in bounds")
    }
}
impl PartialEq for VisibleRows {
    fn eq(&self, other: &Self) -> bool {
        self.view().iter().eq(other.view().iter())
    }
}
impl PartialEq<VisibleRows> for [Vec<TerminalCell>] {
    fn eq(&self, other: &VisibleRows) -> bool {
        self.iter().map(Vec::as_slice).eq(other.iter())
    }
}
impl PartialEq<[Vec<TerminalCell>]> for VisibleRows {
    fn eq(&self, other: &[Vec<TerminalCell>]) -> bool {
        other == self
    }
}
impl PartialEq<Vec<Vec<TerminalCell>>> for VisibleRows {
    fn eq(&self, other: &Vec<Vec<TerminalCell>>) -> bool {
        other.as_slice() == self
    }
}

/// Borrowed row slices keep the renderer independent of raw versus projected
/// storage, without rebuilding row or cell allocations on each paint.
#[derive(Clone, Copy, Debug)]
pub enum RowView<'a> {
    Materialized(&'a [Vec<TerminalCell>]),
    Frame(&'a CellFrame),
}
impl<'a> RowView<'a> {
    pub fn len(self) -> usize {
        match self {
            Self::Materialized(rows) => rows.len(),
            Self::Frame(frame) => frame.rows().len(),
        }
    }
    pub fn is_empty(self) -> bool {
        self.len() == 0
    }
    pub fn get(self, row: usize) -> Option<&'a [TerminalCell]> {
        match self {
            Self::Materialized(rows) => rows.get(row).map(Vec::as_slice),
            Self::Frame(frame) => frame.rows().get(row).map(|r| r.cells.as_ref()),
        }
    }
    pub fn first(self) -> Option<&'a [TerminalCell]> {
        self.get(0)
    }
    pub fn iter(self) -> impl ExactSizeIterator<Item = &'a [TerminalCell]> + DoubleEndedIterator {
        (0..self.len()).map(move |row| self.get(row).expect("row in bounds"))
    }
}
impl std::ops::Index<usize> for RowView<'_> {
    type Output = [TerminalCell];
    fn index(&self, row: usize) -> &Self::Output {
        self.get(row).expect("row in bounds")
    }
}
impl<'a> From<&'a VisibleRows> for RowView<'a> {
    fn from(rows: &'a VisibleRows) -> Self {
        rows.view()
    }
}
impl<'a> From<&'a [Vec<TerminalCell>]> for RowView<'a> {
    fn from(rows: &'a [Vec<TerminalCell>]) -> Self {
        Self::Materialized(rows)
    }
}
impl<'a> From<&'a Vec<Vec<TerminalCell>>> for RowView<'a> {
    fn from(rows: &'a Vec<Vec<TerminalCell>>) -> Self {
        Self::Materialized(rows)
    }
}
impl<'a, const N: usize> From<&'a [Vec<TerminalCell>; N]> for RowView<'a> {
    fn from(rows: &'a [Vec<TerminalCell>; N]) -> Self {
        Self::Materialized(rows)
    }
}
