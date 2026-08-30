use crate::kitty_graphics::{KittyGraphicsState, KittyImage, KittyPlacement};
use base64::Engine;
use jterm_core::click_cursor;
use smallvec::SmallVec;
use std::borrow::Cow;
use std::collections::{BTreeSet, HashMap, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use unicode_normalization::UnicodeNormalization;

#[cfg(test)]
thread_local! {
    static PROJECTED_HISTORY_DECOMPRESS_COUNT: std::cell::Cell<usize> = const {
        std::cell::Cell::new(0)
    };
    /// Bytes of encoded cell records inspected while answering layout queries.
    /// Cached layouts keep this at zero regardless of history depth.
    static PROJECTED_HISTORY_LAYOUT_BYTE_SCAN_COUNT: std::cell::Cell<usize> = const {
        std::cell::Cell::new(0)
    };
    static PROJECTION_PLAN_BUILD_COUNT: std::cell::Cell<usize> = const {
        std::cell::Cell::new(0)
    };
    /// Times a row trim or reflow rescanned `finished_output_provenance`
    /// against the whole zone deque. Trims that evict no zone keep this at
    /// zero regardless of how many zones or provenance entries are retained.
    static PROVENANCE_ORPHAN_SCAN_COUNT: std::cell::Cell<usize> = const {
        std::cell::Cell::new(0)
    };
}

/// Character class for word selection boundaries.
#[derive(PartialEq)]
enum CharClass {
    Word,
    Whitespace,
    Symbol,
}

fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// Whether a cell is styled the way shells paint an inline suggestion.
///
/// There is no protocol for "this text is a preview", only a convention, and
/// the convention is a muted grey: `jsh` prints its suggestion in ANSI colour 8
/// (`ESC[38;5;8m`), zsh-autosuggestions defaults to the same colour, and `dim`
/// (SGR 2) is the other spelling. Being wrong in the permissive direction
/// accepts text the user never typed, so a cell that merely *might* be a
/// suggestion is treated as one — the cost is that a click cannot place the
/// cursor inside genuinely grey text, which is a click that does nothing.
fn is_inline_suggestion_cell(cell: &TerminalCell) -> bool {
    cell.flags.dim() || matches!(cell.foreground, Color::Indexed(8) | Color::BrightBlack)
}

fn is_whitespace_char(c: char) -> bool {
    c == ' ' || c == '\t' || c == '\0'
}

fn char_class(c: char) -> CharClass {
    if is_word_char(c) {
        CharClass::Word
    } else if is_whitespace_char(c) {
        CharClass::Whitespace
    } else {
        CharClass::Symbol
    }
}

fn is_extended_token_separator(c: char) -> bool {
    matches!(
        c,
        '/' | '\\' | '.' | ':' | '-' | '~' | '?' | '&' | '=' | '#' | '%' | '+' | '@'
    )
}

fn is_extended_token_char(c: char) -> bool {
    is_word_char(c) || is_extended_token_separator(c)
}

fn is_token_prefix_wrapper(c: char) -> bool {
    matches!(c, '"' | '\'' | '`' | '(' | '[' | '{' | '<')
}

fn is_token_suffix_wrapper(c: char) -> bool {
    matches!(
        c,
        '"' | '\'' | '`' | ')' | ']' | '}' | '>' | ',' | ';' | '!'
    )
}

const PRIMARY_DEVICE_ATTRIBUTES_RESPONSE: &[u8] = b"\x1b[?65;1;9c";
const SECONDARY_DEVICE_ATTRIBUTES_RESPONSE: &[u8] = b"\x1b[>1;7802;0c";
const XTERM_VERSION_RESPONSE: &[u8] = b"\x1bP>|VTE(7802)\x1b\\";
const MAX_TERMINAL_TITLE_CHARS: usize = 256;
/// OSC 8 fields are terminal-controlled input. Bound both fields before
/// allocating or interning them; the URI limit intentionally matches the
/// single opener policy in `link::is_openable_url`.
const MAX_OSC8_URI_BYTES: usize = 2 * 1024;
const MAX_OSC8_ID_BYTES: usize = 256;
/// Cell keys are `u16`, but a much smaller terminal-local cap bounds retained
/// URI/id memory even when a child emits a fresh id for every character.
const MAX_OSC8_HYPERLINKS: usize = 4 * 1024;
/// Cap on one buffered unfinished escape/string sequence carried across PTY
/// reads. Generous enough for legitimate large payloads (e.g. OSC 52
/// clipboard); kitty graphics APCs stream against the same cap via
/// `pending_apc` instead of being dropped wholesale.
const MAX_PENDING_ESCAPE: usize = 1 << 20; // 1 MiB
pub const MAX_TERMINAL_COLS: usize = 1024;
pub const MAX_TERMINAL_ROWS: usize = 512;
pub type DynamicColorPalette = [Option<(u8, u8, u8)>; 256];

#[derive(Debug, PartialEq, Eq, Hash)]
struct Osc8Hyperlink {
    uri: String,
    id: Option<String>,
}

/// Stable identity of one retained physical terminal row.
///
/// The identity follows a row while it moves between the live grid and
/// scrollback. Operations that physically re-materialize history (rather than
/// merely moving a retained row) allocate new identities, so stale projected
/// coordinates fail closed instead of being guessed onto unrelated content.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct RawRowId(u64);

impl RawRowId {
    const UNTRACKED: Self = Self(0);

    fn fresh() -> Self {
        static NEXT_RAW_ROW_ID: AtomicU64 = AtomicU64::new(1);
        NEXT_RAW_ROW_ID
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |next| {
                next.checked_add(1)
            })
            .map(Self)
            .unwrap_or(Self::UNTRACKED)
    }

    #[inline]
    fn is_tracked(self) -> bool {
        self != Self::UNTRACKED
    }
}

pub fn clamp_terminal_dimensions(cols: usize, rows: usize) -> (usize, usize) {
    (
        cols.clamp(1, MAX_TERMINAL_COLS),
        rows.clamp(1, MAX_TERMINAL_ROWS),
    )
}

/// 连续内存网格存储 - 优化内存局部性和缓存命中率
/// 内存布局：`cells[row * cols + col]` 对应 `grid[row][col]`。
#[derive(Clone)]
pub struct TerminalGrid {
    cells: Vec<TerminalCell>,
    rows: usize,
    cols: usize,
    pub row_wrapped: Vec<bool>,
    row_ids: Vec<RawRowId>,
    identity_revision: u64,
}

impl TerminalGrid {
    pub fn new(rows: usize, cols: usize) -> Self {
        TerminalGrid {
            cells: vec![TerminalCell::default(); rows * cols],
            rows,
            cols,
            row_wrapped: vec![false; rows],
            row_ids: (0..rows).map(|_| RawRowId::fresh()).collect(),
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
        self.row_ids[self.rows - 1] = RawRowId::fresh();
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
        let mut new_row_ids: Vec<_> = (0..new_rows).map(|_| RawRowId::fresh()).collect();
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

impl std::ops::Index<usize> for TerminalGrid {
    type Output = [TerminalCell];
    fn index(&self, row: usize) -> &[TerminalCell] {
        let start = row * self.cols;
        &self.cells[start..start + self.cols]
    }
}

impl std::ops::IndexMut<usize> for TerminalGrid {
    fn index_mut(&mut self, row: usize) -> &mut [TerminalCell] {
        let start = row * self.cols;
        &mut self.cells[start..start + self.cols]
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Color {
    Black,
    Red,
    Green,
    Yellow,
    Blue,
    Magenta,
    Cyan,
    White,
    BrightBlack,
    BrightRed,
    BrightGreen,
    BrightYellow,
    BrightBlue,
    BrightMagenta,
    BrightCyan,
    BrightWhite,
    Indexed(u8),
    Rgb(u8, u8, u8),
    Default,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CursorShape {
    #[default]
    Block,
    Underline,
    Beam,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum UnderlineStyle {
    #[default]
    None,
    Single,
    Double,
    #[allow(dead_code)]
    Curly, // SGR 4:3
    #[allow(dead_code)]
    Dotted, // SGR 4:4
    #[allow(dead_code)]
    Dashed, // SGR 4:5
}

/// Packed style flags in a u16 bitfield (includes wide character bits).
/// Layout:
///   bit 0: bold
///   bit 1: italic
///   bit 2-4: underline style (3 bits, 0-5)
///   bit 5: inverse
///   bit 6: dim
///   bit 7: blink
///   bit 8: strikethrough
///   bit 9: wide
///   bit 10: wide_continuation
#[derive(Clone, Copy, PartialEq, Eq, Default)]
#[repr(transparent)]
pub struct StyleFlags(u16);

impl std::fmt::Debug for StyleFlags {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StyleFlags")
            .field("bold", &self.bold())
            .field("italic", &self.italic())
            .field("underline", &self.underline())
            .field("inverse", &self.inverse())
            .field("dim", &self.dim())
            .field("blink", &self.blink())
            .field("strikethrough", &self.strikethrough())
            .finish()
    }
}

const BOLD_BIT: u16 = 1 << 0;
const ITALIC_BIT: u16 = 1 << 1;
const UNDERLINE_SHIFT: u32 = 2;
const UNDERLINE_MASK: u16 = 0b111 << 2;
const INVERSE_BIT: u16 = 1 << 5;
const DIM_BIT: u16 = 1 << 6;
const BLINK_BIT: u16 = 1 << 7;
const STRIKETHROUGH_BIT: u16 = 1 << 8;
const WIDE_BIT: u16 = 1 << 9;
const WIDE_CONT_BIT: u16 = 1 << 10;

impl StyleFlags {
    #[inline(always)]
    pub fn new() -> Self {
        Self(0)
    }

    #[inline(always)]
    pub fn bold(&self) -> bool {
        self.0 & BOLD_BIT != 0
    }
    #[inline(always)]
    pub fn italic(&self) -> bool {
        self.0 & ITALIC_BIT != 0
    }
    #[inline(always)]
    pub fn underline(&self) -> UnderlineStyle {
        match (self.0 & UNDERLINE_MASK) >> UNDERLINE_SHIFT {
            1 => UnderlineStyle::Single,
            2 => UnderlineStyle::Double,
            3 => UnderlineStyle::Curly,
            4 => UnderlineStyle::Dotted,
            5 => UnderlineStyle::Dashed,
            _ => UnderlineStyle::None,
        }
    }
    #[inline(always)]
    pub fn inverse(&self) -> bool {
        self.0 & INVERSE_BIT != 0
    }
    #[inline(always)]
    pub fn dim(&self) -> bool {
        self.0 & DIM_BIT != 0
    }
    #[inline(always)]
    pub fn blink(&self) -> bool {
        self.0 & BLINK_BIT != 0
    }
    #[inline(always)]
    pub fn strikethrough(&self) -> bool {
        self.0 & STRIKETHROUGH_BIT != 0
    }
    #[inline(always)]
    pub fn wide(&self) -> bool {
        self.0 & WIDE_BIT != 0
    }
    #[inline(always)]
    pub fn wide_continuation(&self) -> bool {
        self.0 & WIDE_CONT_BIT != 0
    }

    #[inline(always)]
    pub fn set_bold(&mut self, v: bool) {
        if v {
            self.0 |= BOLD_BIT;
        } else {
            self.0 &= !BOLD_BIT;
        }
    }
    #[inline(always)]
    pub fn set_italic(&mut self, v: bool) {
        if v {
            self.0 |= ITALIC_BIT;
        } else {
            self.0 &= !ITALIC_BIT;
        }
    }
    #[inline(always)]
    pub fn set_underline(&mut self, v: UnderlineStyle) {
        self.0 = (self.0 & !UNDERLINE_MASK) | ((v as u16) << UNDERLINE_SHIFT);
    }
    #[inline(always)]
    pub fn set_inverse(&mut self, v: bool) {
        if v {
            self.0 |= INVERSE_BIT;
        } else {
            self.0 &= !INVERSE_BIT;
        }
    }
    #[inline(always)]
    pub fn set_dim(&mut self, v: bool) {
        if v {
            self.0 |= DIM_BIT;
        } else {
            self.0 &= !DIM_BIT;
        }
    }
    #[inline(always)]
    pub fn set_blink(&mut self, v: bool) {
        if v {
            self.0 |= BLINK_BIT;
        } else {
            self.0 &= !BLINK_BIT;
        }
    }
    #[inline(always)]
    pub fn set_strikethrough(&mut self, v: bool) {
        if v {
            self.0 |= STRIKETHROUGH_BIT;
        } else {
            self.0 &= !STRIKETHROUGH_BIT;
        }
    }
    #[inline(always)]
    pub fn set_wide(&mut self, v: bool) {
        if v {
            self.0 |= WIDE_BIT;
        } else {
            self.0 &= !WIDE_BIT;
        }
    }
    #[inline(always)]
    pub fn set_wide_continuation(&mut self, v: bool) {
        if v {
            self.0 |= WIDE_CONT_BIT;
        } else {
            self.0 &= !WIDE_CONT_BIT;
        }
    }

    #[inline(always)]
    pub fn is_default_style(&self) -> bool {
        self.0 & 0x1FF == 0
    }
}

#[derive(Clone, Debug)]
pub struct ScrollbackLine {
    data: CompressedLineData,
    pub is_wrapped: bool,
    cols: u16,
    raw_row_id: RawRowId,
    projected_active_len: u16,
    projected_wide_continuations: SmallVec<[u16; 2]>,
}

#[derive(Clone, Debug)]
enum CompressedLineData {
    Plain(String, u16),
    Encoded(Vec<u8>),
}

/// One searchable buffer row: either borrowed narrow text with an identity
/// character→column map (the common all-default-attributes scrollback row),
/// or full cell data that still needs the wide/continuation-aware mapping.
pub enum SearchLine<'a> {
    Text(&'a str),
    Cells(Cow<'a, [TerminalCell]>),
}

/// Allocation-free geometry needed to plan a projected history document.
///
/// `active_len` is exactly the prefix retained by P0's
/// [`TerminalState::strip_trailing_blanks`] history reflow. This intentionally
/// differs from compression: foreground/style-only trailing blanks are
/// structural padding here, while a non-default background remains content.
/// Wide continuation columns are the only per-cell facts reflow additionally
/// needs in order to avoid splitting a glyph.
#[derive(Clone, Debug, PartialEq, Eq)]
#[allow(dead_code)] // P1 plan core; consumer wiring lands in the collapse slice.
struct RawRowLayout {
    absolute_row: usize,
    raw_row: RawRowId,
    active_len: usize,
    wide_continuations: SmallVec<[usize; 2]>,
    wrapped: bool,
}

impl RawRowLayout {
    /// Build layout metadata for a live-grid row. Unlike scrollback, the grid
    /// is already resident, so this is a borrowed scan with no cell cloning.
    #[allow(dead_code)] // P1 plan core; consumer wiring lands in the collapse slice.
    fn from_cells(
        cells: &[TerminalCell],
        wrapped: bool,
        absolute_row: usize,
        raw_row: RawRowId,
    ) -> Self {
        let mut active_len = cells.len();
        while active_len > 0
            && cells[active_len - 1].character == ' '
            && cells[active_len - 1].background == Color::Default
            && !cells[active_len - 1].flags.wide()
            && !cells[active_len - 1].flags.wide_continuation()
            && cells[active_len - 1].hyperlink == 0
        {
            active_len -= 1;
        }
        let wide_continuations = cells[..active_len]
            .iter()
            .enumerate()
            .filter_map(|(col, cell)| cell.flags.wide_continuation().then_some(col))
            .collect();
        Self {
            absolute_row,
            raw_row,
            active_len,
            wide_continuations,
            wrapped,
        }
    }
}

impl ScrollbackLine {
    pub fn compress(cells: &[TerminalCell], is_wrapped: bool) -> Self {
        Self::compress_with_raw_row_id(cells, is_wrapped, RawRowId::fresh())
    }

    fn compress_with_raw_row_id(
        cells: &[TerminalCell],
        is_wrapped: bool,
        raw_row_id: RawRowId,
    ) -> Self {
        let cols = cells.len() as u16;
        // Cache exactly the geometry used by P0 history reflow. This is not
        // compression's retention rule: foreground/style-only trailing blanks
        // remain encoded for round trips but are structural projection padding.
        let projected_active_len = cells
            .iter()
            .rposition(|cell| {
                cell.character != ' '
                    || cell.background != Color::Default
                    || cell.flags.wide()
                    || cell.flags.wide_continuation()
                    || cell.hyperlink != 0
            })
            .map_or(0, |col| col + 1);
        let projected_wide_continuations = cells[..projected_active_len]
            .iter()
            .enumerate()
            .filter_map(|(col, cell)| {
                if cell.flags.wide_continuation() {
                    u16::try_from(col).ok()
                } else {
                    None
                }
            })
            .collect();
        let projected_active_len = u16::try_from(projected_active_len).unwrap_or(u16::MAX);
        let trailing_blanks = cells
            .iter()
            .rev()
            .take_while(|c| {
                c.character == ' '
                    && c.foreground == Color::Default
                    && c.background == Color::Default
                    && c.flags.is_default_style()
                    && !c.flags.wide()
                    && !c.flags.wide_continuation()
                    && c.hyperlink == 0
            })
            .count();

        let active_len = cells.len() - trailing_blanks;
        let all_default_attrs = cells[..active_len].iter().all(|c| {
            c.foreground == Color::Default
                && c.background == Color::Default
                && c.flags.is_default_style()
                && !c.flags.wide()
                && !c.flags.wide_continuation()
                && c.hyperlink == 0
        });

        if all_default_attrs {
            let text: String = cells[..active_len].iter().map(|c| c.character).collect();
            ScrollbackLine {
                data: CompressedLineData::Plain(text, trailing_blanks as u16),
                is_wrapped,
                cols,
                raw_row_id,
                projected_active_len,
                projected_wide_continuations,
            }
        } else {
            let encoded = Self::encode_cells(&cells[..active_len]);
            ScrollbackLine {
                data: CompressedLineData::Encoded(encoded),
                is_wrapped,
                cols,
                raw_row_id,
                projected_active_len,
                projected_wide_continuations,
            }
        }
    }

    /// Identity of this retained physical scrollback row.
    pub fn raw_row_id(&self) -> RawRowId {
        self.raw_row_id
    }

    pub fn decompress(&self) -> Vec<TerminalCell> {
        #[cfg(test)]
        PROJECTED_HISTORY_DECOMPRESS_COUNT.with(|count| count.set(count.get() + 1));
        match &self.data {
            CompressedLineData::Plain(text, trailing) => {
                let mut cells: Vec<TerminalCell> = text
                    .chars()
                    .map(|ch| TerminalCell {
                        character: ch,
                        ..Default::default()
                    })
                    .collect();
                cells.resize(cells.len() + *trailing as usize, TerminalCell::default());
                cells
            }
            CompressedLineData::Encoded(data) => Self::decode_cells(data, self.cols as usize),
        }
    }

    /// Searchable text for this retained row without materializing cells.
    /// `Plain` rows are borrowed directly: their active cells are narrow
    /// with default attributes by construction, so the character index is
    /// the terminal column and trailing blanks stay unsearchable exactly
    /// like the decompressed path's structural-padding trim. `Encoded` rows
    /// fall back to decompressing for the wide/continuation-aware mapping.
    pub fn search_text(&self) -> SearchLine<'_> {
        match &self.data {
            CompressedLineData::Plain(text, _) => SearchLine::Text(text),
            CompressedLineData::Encoded(_) => SearchLine::Cells(Cow::Owned(self.decompress())),
        }
    }

    #[allow(dead_code)] // P1 plan core; consumer wiring lands in the collapse slice.
    fn layout(&self, absolute_row: usize) -> RawRowLayout {
        RawRowLayout {
            absolute_row,
            raw_row: self.raw_row_id,
            active_len: usize::from(self.projected_active_len),
            wide_continuations: self
                .projected_wide_continuations
                .iter()
                .copied()
                .map(usize::from)
                .collect(),
            wrapped: self.is_wrapped,
        }
    }

    #[allow(dead_code)]
    pub fn cells(&self) -> Vec<TerminalCell> {
        self.decompress()
    }

    fn encode_color(color: &Color, buf: &mut Vec<u8>) {
        match color {
            Color::Default => buf.push(0),
            Color::Black => buf.push(1),
            Color::Red => buf.push(2),
            Color::Green => buf.push(3),
            Color::Yellow => buf.push(4),
            Color::Blue => buf.push(5),
            Color::Magenta => buf.push(6),
            Color::Cyan => buf.push(7),
            Color::White => buf.push(8),
            Color::BrightBlack => buf.push(9),
            Color::BrightRed => buf.push(10),
            Color::BrightGreen => buf.push(11),
            Color::BrightYellow => buf.push(12),
            Color::BrightBlue => buf.push(13),
            Color::BrightMagenta => buf.push(14),
            Color::BrightCyan => buf.push(15),
            Color::BrightWhite => buf.push(16),
            Color::Indexed(i) => {
                buf.push(17);
                buf.push(*i);
            }
            Color::Rgb(r, g, b) => {
                buf.push(18);
                buf.push(*r);
                buf.push(*g);
                buf.push(*b);
            }
        }
    }

    fn decode_color(data: &[u8], pos: &mut usize) -> Color {
        if *pos >= data.len() {
            return Color::Default;
        }
        let tag = data[*pos];
        *pos += 1;
        match tag {
            0 => Color::Default,
            1 => Color::Black,
            2 => Color::Red,
            3 => Color::Green,
            4 => Color::Yellow,
            5 => Color::Blue,
            6 => Color::Magenta,
            7 => Color::Cyan,
            8 => Color::White,
            9 => Color::BrightBlack,
            10 => Color::BrightRed,
            11 => Color::BrightGreen,
            12 => Color::BrightYellow,
            13 => Color::BrightBlue,
            14 => Color::BrightMagenta,
            15 => Color::BrightCyan,
            16 => Color::BrightWhite,
            17 => {
                let i = data.get(*pos).copied().unwrap_or(0);
                *pos += 1;
                Color::Indexed(i)
            }
            18 => {
                let r = data.get(*pos).copied().unwrap_or(0);
                let g = data.get(*pos + 1).copied().unwrap_or(0);
                let b = data.get(*pos + 2).copied().unwrap_or(0);
                *pos += 3;
                Color::Rgb(r, g, b)
            }
            _ => Color::Default,
        }
    }

    fn encode_flags(flags: &StyleFlags) -> u8 {
        let mut f = 0u8;
        if flags.bold() {
            f |= 1;
        }
        if flags.italic() {
            f |= 2;
        }
        match flags.underline() {
            UnderlineStyle::None => {}
            UnderlineStyle::Single => f |= 4,
            UnderlineStyle::Double => f |= 8,
            UnderlineStyle::Curly => f |= 12,
            UnderlineStyle::Dotted => f |= 16,
            UnderlineStyle::Dashed => f |= 20,
        }
        if flags.inverse() {
            f |= 32;
        }
        if flags.dim() {
            f |= 64;
        }
        if flags.strikethrough() {
            f |= 128;
        }
        f
    }

    fn decode_flags(f: u8) -> StyleFlags {
        let underline = match (f >> 2) & 0x7 {
            0 => UnderlineStyle::None,
            1 => UnderlineStyle::Single,
            2 => UnderlineStyle::Double,
            3 => UnderlineStyle::Curly,
            4 => UnderlineStyle::Dotted,
            5 => UnderlineStyle::Dashed,
            _ => UnderlineStyle::None,
        };
        let mut flags = StyleFlags::new();
        flags.set_bold(f & 1 != 0);
        flags.set_italic(f & 2 != 0);
        flags.set_underline(underline);
        flags.set_inverse(f & 32 != 0);
        flags.set_dim(f & 64 != 0);
        flags.set_strikethrough(f & 128 != 0);
        flags
    }

    fn encode_cells(cells: &[TerminalCell]) -> Vec<u8> {
        let mut buf = Vec::with_capacity(cells.len() * 3);
        let mut i = 0;
        while i < cells.len() {
            let cell = &cells[i];
            let ch_str = cell.character.to_string();
            let ch_bytes = ch_str.as_bytes();

            // RLE: count consecutive identical cells (packed flags comparison is a single u16 ==)
            let mut run = 1u8;
            while (run as u16) < 255 && (i + run as usize) < cells.len() {
                let next = &cells[i + run as usize];
                if next.character == cell.character
                    && next.foreground == cell.foreground
                    && next.background == cell.background
                    && next.flags == cell.flags
                    && next.hyperlink == cell.hyperlink
                {
                    run += 1;
                } else {
                    break;
                }
            }

            // Format:
            // [char_len:1][char_bytes][fg][bg][style_flags:1][extra_flags:1]
            // [hyperlink:2][run:1]
            // style_flags uses every bit, so blink shares extra_flags with the
            // two wide-character markers.
            buf.push(ch_bytes.len() as u8);
            buf.extend_from_slice(ch_bytes);
            Self::encode_color(&cell.foreground, &mut buf);
            Self::encode_color(&cell.background, &mut buf);
            let f = Self::encode_flags(&cell.flags);
            buf.push(f);
            let extra_flags = if cell.flags.wide() { 1u8 } else { 0 }
                | if cell.flags.wide_continuation() { 2 } else { 0 }
                | if cell.flags.blink() { 4 } else { 0 };
            buf.push(extra_flags);
            buf.extend_from_slice(&cell.hyperlink.to_le_bytes());
            buf.push(run);

            i += run as usize;
        }
        buf
    }

    fn decode_cells(data: &[u8], cols: usize) -> Vec<TerminalCell> {
        let mut cells = Vec::with_capacity(cols);
        let mut pos = 0;
        while pos < data.len() {
            let ch_len = data[pos] as usize;
            pos += 1;
            if pos + ch_len > data.len() {
                break;
            }
            let ch = std::str::from_utf8(&data[pos..pos + ch_len])
                .ok()
                .and_then(|s| s.chars().next())
                .unwrap_or(' ');
            pos += ch_len;

            let fg = Self::decode_color(data, &mut pos);
            let bg = Self::decode_color(data, &mut pos);
            let f = data.get(pos).copied().unwrap_or(0);
            pos += 1;
            let extra_flags = data.get(pos).copied().unwrap_or(0);
            pos += 1;
            let hyperlink = u16::from_le_bytes([
                data.get(pos).copied().unwrap_or(0),
                data.get(pos + 1).copied().unwrap_or(0),
            ]);
            pos += 2;
            let run = data.get(pos).copied().unwrap_or(1).max(1);
            pos += 1;

            let mut flags = Self::decode_flags(f);
            flags.set_wide(extra_flags & 1 != 0);
            flags.set_wide_continuation(extra_flags & 2 != 0);
            flags.set_blink(extra_flags & 4 != 0);

            let cell = TerminalCell {
                character: ch,
                foreground: fg,
                background: bg,
                flags,
                hyperlink,
            };
            for _ in 0..run {
                cells.push(cell);
            }
        }
        // Pad to cols
        cells.resize(cols, TerminalCell::default());
        cells
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TerminalCell {
    pub character: char,
    pub foreground: Color,
    pub background: Color,
    pub flags: StyleFlags,
    /// Terminal-local OSC 8 intern key; zero means no explicit hyperlink.
    pub(crate) hyperlink: u16,
}

impl Default for TerminalCell {
    fn default() -> Self {
        TerminalCell {
            character: ' ',
            foreground: Color::Default,
            background: Color::Default,
            flags: StyleFlags::new(),
            hyperlink: 0,
        }
    }
}

const _: () = assert!(std::mem::size_of::<TerminalCell>() == 16);
type VisibleCellsCache = (
    u64,
    usize,
    std::sync::Arc<Vec<Vec<TerminalCell>>>,
    Option<std::sync::Arc<ProjectedProvenance>>,
);

/// Stable raw provenance of one displayed cell.
///
/// Wide-character continuation cells retain their own raw column. Synthetic
/// padding has no origin and therefore cannot accidentally target a raw cell.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct RawCellOrigin {
    pub row: RawRowId,
    pub col: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProjectedRawCellLocation {
    Visible(ViewportCell),
    Hidden { zone_id: u64 },
    Retained,
    Unmapped,
}

/// A cell coordinate in the currently projected viewport.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ViewportCell {
    pub row: usize,
    pub col: usize,
}

/// Revisions of the raw sources used to build a projected viewport.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ProjectionSourceRevision {
    pub grid: u64,
    pub history: u64,
    pub row_identity: u64,
    pub alternate_screen: bool,
}

/// Whether semantic history projection is eligible for this snapshot. P0's
/// cell output is identical in both modes, but the distinction is part of the
/// cache contract so disabling Block Mode or entering alt screen can never
/// reuse a future transformed projection.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ProjectionMode {
    Identity,
    Transformed,
    Bypass,
}

/// Stable boundary in the retained raw terminal document. `col` is allowed
/// to equal the physical row width when the boundary is end-exclusive.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct RawCellBoundary {
    pub row: RawRowId,
    pub col: usize,
}

/// Exact retained output range for one finalized command zone.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct FinishedOutputRange {
    pub zone_id: u64,
    pub start: RawCellBoundary,
    pub end: RawCellBoundary,
}

/// Identity of a synthetic collapse row within one policy revision.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[allow(dead_code)] // Stage A contract; renderer wiring lands in the next slice.
pub struct SyntheticRowKey {
    pub zone_id: u64,
    pub policy_revision: u64,
}

/// Provenance class for one projected document row.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[allow(dead_code)] // Stage A contract; renderer wiring lands in the next slice.
pub enum ProjectedRowKind {
    Raw,
    Padding,
    CollapsedSummary {
        key: SyntheticRowKey,
        hidden_range: FinishedOutputRange,
        hidden_display_rows: usize,
    },
}

/// One contiguous piece of a physical raw row referenced by a planned
/// display row. The plan deliberately carries geometry only: terminal cells
/// remain compressed in scrollback (or resident in the live grid) until a
/// viewport slice actually needs to materialize them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[allow(dead_code)] // P1 plan core; consumer wiring lands in the collapse slice.
struct RawSliceSource {
    absolute_row: usize,
    col_start: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[allow(dead_code)] // P1 plan core; consumer wiring lands in the collapse slice.
struct RawSliceOrigin {
    row: RawRowId,
    col_start: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[allow(dead_code)] // P1 plan core; consumer wiring lands in the collapse slice.
struct RawSlice {
    view_col_start: usize,
    source: RawSliceSource,
    origin: Option<RawSliceOrigin>,
    len: usize,
    narrow_wide_body: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
#[allow(dead_code)] // P1 plan core; consumer wiring lands in the collapse slice.
struct ProjectionPlanRow {
    raw_slices: SmallVec<[RawSlice; 2]>,
    row_source: Option<RowSource>,
    wrapped: bool,
    kind: ProjectedRowKind,
}

/// Snapshot-local placement of one physical raw row in the planned document.
/// Rows that are structurally elided by reflow remain present with no view
/// bounds; collapse resolution must then fail closed instead of guessing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[allow(dead_code)] // P1 plan core; consumer wiring lands in the next slice.
struct RawRowPlacement {
    absolute_row: usize,
    raw_row: RawRowId,
    first_view_row: Option<usize>,
    last_view_row: Option<usize>,
}

/// Allocation-light geometry for the complete projected document. This is
/// built before viewport slicing and contains no `TerminalCell` vectors.
#[derive(Clone, Debug, PartialEq, Eq)]
#[allow(dead_code)] // P1 plan core; consumer wiring lands in the collapse slice.
struct ProjectionPlan {
    cols: usize,
    rows: Vec<ProjectionPlanRow>,
    raw_rows: Vec<RawRowPlacement>,
    raw_slice_count: usize,
    policy_revision: u64,
    effective_collapsed: BTreeSet<u64>,
    resolved_collapses: Vec<ResolvedCollapse>,
    plan_revision: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ResolvedCollapse {
    range: FinishedOutputRange,
    start_absolute: usize,
    end_absolute: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct HideSegment {
    collapse: usize,
    view_start: usize,
    view_end: usize,
}

#[allow(dead_code)] // P1 plan core; consumer wiring lands in the collapse slice.
/// Per-group scratch reused across a whole plan build.
///
/// The group geometry is discarded as soon as the group's rows are emitted, so
/// it does not need to be a fresh allocation each time — and an unwrapped
/// history row forms a group by itself, which made this two `Vec` allocations
/// for every row of scrollback on every rebuild.
#[derive(Default)]
struct GroupScratch {
    logical_sources: Vec<(usize, RawSlice)>,
    logical_wide_continuations: Vec<usize>,
}

impl ProjectionPlan {
    fn identity(
        history_layouts: impl IntoIterator<Item = RawRowLayout>,
        grid_layouts: impl IntoIterator<Item = RawRowLayout>,
        cols: usize,
    ) -> Self {
        debug_assert!(cols > 0);
        // Consume the layouts as streams and build `raw_rows` in the same
        // walk. Collecting them first, then cloning each one out again, and
        // then re-walking both to derive placements cost three passes and two
        // document-sized vectors per rebuild — and this whole function runs
        // once per appended output line while any block is collapsed.
        let history_layouts = history_layouts.into_iter();
        let grid_layouts = grid_layouts.into_iter();
        let hint = history_layouts
            .size_hint()
            .0
            .saturating_add(grid_layouts.size_hint().0);
        let mut rows = Vec::with_capacity(hint);
        let mut raw_rows = Vec::with_capacity(hint);
        let mut group = Vec::new();
        // Reused across every group instead of allocated per group: a plain
        // history row is its own group, so this was two allocations per row.
        let mut scratch = GroupScratch::default();
        let placement = |layout: &RawRowLayout| RawRowPlacement {
            absolute_row: layout.absolute_row,
            raw_row: layout.raw_row,
            first_view_row: None,
            last_view_row: None,
        };

        // History is a reflowable logical document. The live grid is already
        // at display width and P0 appends each physical row without joining it
        // to a trailing wrapped scrollback row (or to adjacent grid rows).
        for layout in history_layouts {
            raw_rows.push(placement(&layout));
            let wrapped = layout.wrapped;
            group.push(layout);
            if !wrapped {
                Self::append_identity_group(&mut rows, &group, cols, &mut scratch);
                group.clear();
            }
        }
        if !group.is_empty() {
            Self::append_identity_group(&mut rows, &group, cols, &mut scratch);
        }
        for layout in grid_layouts {
            raw_rows.push(placement(&layout));
            Self::append_grid_row(&mut rows, layout, cols);
        }

        let raw_slice_count = rows.iter().map(|row| row.raw_slices.len()).sum();
        let mut plan = Self {
            cols,
            rows,
            raw_rows,
            raw_slice_count,
            policy_revision: 0,
            effective_collapsed: BTreeSet::new(),
            resolved_collapses: Vec::new(),
            plan_revision: 0,
        };
        plan.rebuild_raw_row_placements();
        plan
    }

    fn append_grid_row(output: &mut Vec<ProjectionPlanRow>, layout: RawRowLayout, cols: usize) {
        let row_source = layout.raw_row.is_tracked().then_some(RowSource {
            raw_row: layout.raw_row,
            raw_absolute_row: layout.absolute_row,
        });
        let raw_slices = std::iter::once(RawSlice {
            view_col_start: 0,
            source: RawSliceSource {
                absolute_row: layout.absolute_row,
                col_start: 0,
            },
            origin: layout.raw_row.is_tracked().then_some(RawSliceOrigin {
                row: layout.raw_row,
                col_start: 0,
            }),
            // P0 live-grid provenance covers every physical cell,
            // including default trailing blanks. Those are raw cells,
            // unlike padding introduced by history reflow.
            len: cols,
            narrow_wide_body: false,
        })
        .collect();
        output.push(ProjectionPlanRow {
            raw_slices,
            row_source,
            wrapped: layout.wrapped,
            kind: ProjectedRowKind::Raw,
        });
    }

    fn append_identity_group(
        output: &mut Vec<ProjectionPlanRow>,
        group: &[RawRowLayout],
        cols: usize,
        scratch: &mut GroupScratch,
    ) {
        let group_first_source = group.first().and_then(|layout| {
            layout.raw_row.is_tracked().then_some(RowSource {
                raw_row: layout.raw_row,
                raw_absolute_row: layout.absolute_row,
            })
        });
        let logical_len = group
            .iter()
            .fold(0usize, |len, layout| len.saturating_add(layout.active_len));
        if logical_len == 0 {
            output.push(ProjectionPlanRow {
                raw_slices: SmallVec::new(),
                row_source: group_first_source,
                wrapped: false,
                kind: ProjectedRowKind::Raw,
            });
            return;
        }

        if cols == 1 {
            let group_start = output.len();
            for layout in group {
                let mut continuation = layout.wide_continuations.iter().copied().peekable();
                for raw_col in 0..layout.active_len {
                    if continuation.peek().copied() == Some(raw_col) {
                        continuation.next();
                        continue;
                    }
                    let raw_slice = RawSlice {
                        view_col_start: 0,
                        source: RawSliceSource {
                            absolute_row: layout.absolute_row,
                            col_start: raw_col,
                        },
                        origin: layout.raw_row.is_tracked().then_some(RawSliceOrigin {
                            row: layout.raw_row,
                            col_start: raw_col,
                        }),
                        len: 1,
                        narrow_wide_body: layout
                            .wide_continuations
                            .binary_search(&(raw_col.saturating_add(1)))
                            .is_ok(),
                    };
                    output.push(ProjectionPlanRow {
                        raw_slices: std::iter::once(raw_slice).collect(),
                        row_source: raw_slice.origin.map(|origin| RowSource {
                            raw_row: origin.row,
                            raw_absolute_row: raw_slice.source.absolute_row,
                        }),
                        wrapped: true,
                        kind: ProjectedRowKind::Raw,
                    });
                }
            }
            if let Some(last) = output
                .get_mut(group_start..)
                .and_then(|rows| rows.last_mut())
            {
                last.wrapped = false;
            }
            return;
        }

        // Temporary source geometry is linear in physical rows / wide glyphs
        // and is discarded once the plan has been emitted. Keeping cursors in
        // both vectors makes planning O(source rows + planned rows + slices).
        let GroupScratch {
            logical_sources,
            logical_wide_continuations,
        } = scratch;
        logical_sources.clear();
        logical_wide_continuations.clear();
        let mut source_start = 0usize;
        for layout in group {
            logical_wide_continuations.extend(
                layout
                    .wide_continuations
                    .iter()
                    .map(|raw_col| source_start.saturating_add(*raw_col)),
            );
            if layout.active_len > 0 {
                logical_sources.push((
                    source_start,
                    RawSlice {
                        view_col_start: 0,
                        source: RawSliceSource {
                            absolute_row: layout.absolute_row,
                            col_start: 0,
                        },
                        origin: layout.raw_row.is_tracked().then_some(RawSliceOrigin {
                            row: layout.raw_row,
                            col_start: 0,
                        }),
                        len: layout.active_len,
                        narrow_wide_body: false,
                    },
                ));
            }
            source_start = source_start.saturating_add(layout.active_len);
        }

        let mut logical_offset = 0usize;
        let mut source_cursor = 0usize;
        let mut wide_cursor = 0usize;
        while logical_offset < logical_len {
            let mut end = logical_offset.saturating_add(cols).min(logical_len);
            while logical_wide_continuations
                .get(wide_cursor)
                .is_some_and(|position| *position < end)
            {
                wide_cursor += 1;
            }
            if end < logical_len
                && logical_wide_continuations.get(wide_cursor).copied() == Some(end)
            {
                end -= 1;
            }
            while let Some((start, slice)) = logical_sources.get(source_cursor) {
                if start.saturating_add(slice.len) > logical_offset {
                    break;
                }
                source_cursor += 1;
            }
            let mut raw_slices: SmallVec<[RawSlice; 2]> = SmallVec::new();
            for (start, slice) in logical_sources[source_cursor..].iter().copied() {
                if start >= end {
                    break;
                }
                let source_end = start.saturating_add(slice.len);
                let overlap_start = start.max(logical_offset);
                let overlap_end = source_end.min(end);
                if overlap_start < overlap_end {
                    raw_slices.push(RawSlice {
                        view_col_start: overlap_start - logical_offset,
                        source: RawSliceSource {
                            absolute_row: slice.source.absolute_row,
                            col_start: slice.source.col_start + overlap_start - start,
                        },
                        origin: slice.origin.map(|origin| RawSliceOrigin {
                            row: origin.row,
                            col_start: origin.col_start + overlap_start - start,
                        }),
                        len: overlap_end - overlap_start,
                        narrow_wide_body: false,
                    });
                }
            }
            let row_source = raw_slices.iter().find_map(|slice| {
                slice.origin.map(|origin| RowSource {
                    raw_row: origin.row,
                    raw_absolute_row: slice.source.absolute_row,
                })
            });
            output.push(ProjectionPlanRow {
                raw_slices,
                row_source,
                wrapped: end < logical_len,
                kind: ProjectedRowKind::Raw,
            });
            logical_offset = end;
        }
    }

    fn rebuild_raw_row_placements(&mut self) {
        for placement in &mut self.raw_rows {
            placement.first_view_row = None;
            placement.last_view_row = None;
        }
        for (view_row, row) in self.rows.iter().enumerate() {
            for absolute_row in row
                .raw_slices
                .iter()
                .map(|slice| slice.source.absolute_row)
                .chain(
                    row.row_source
                        .into_iter()
                        .map(|source| source.raw_absolute_row),
                )
            {
                let Some(placement) = self.raw_rows.get_mut(absolute_row) else {
                    continue;
                };
                if placement.absolute_row != absolute_row {
                    continue;
                }
                placement.first_view_row = Some(
                    placement
                        .first_view_row
                        .map_or(view_row, |first| first.min(view_row)),
                );
                placement.last_view_row = Some(
                    placement
                        .last_view_row
                        .map_or(view_row, |last| last.max(view_row)),
                );
            }
        }
    }

    fn clipped_slices(
        row: &ProjectionPlanRow,
        view_start: usize,
        view_end: usize,
    ) -> SmallVec<[RawSlice; 2]> {
        row.raw_slices
            .iter()
            .filter_map(|slice| {
                let slice_start = slice.view_col_start;
                let slice_end = slice_start.saturating_add(slice.len);
                let overlap_start = slice_start.max(view_start);
                let overlap_end = slice_end.min(view_end);
                (overlap_start < overlap_end).then(|| {
                    let delta = overlap_start - slice_start;
                    RawSlice {
                        view_col_start: overlap_start,
                        source: RawSliceSource {
                            absolute_row: slice.source.absolute_row,
                            col_start: slice.source.col_start + delta,
                        },
                        origin: slice.origin.map(|origin| RawSliceOrigin {
                            row: origin.row,
                            col_start: origin.col_start + delta,
                        }),
                        len: overlap_end - overlap_start,
                        narrow_wide_body: slice.narrow_wide_body,
                    }
                })
            })
            .collect()
    }

    fn push_raw_fragment(
        output: &mut Vec<ProjectionPlanRow>,
        slices: SmallVec<[RawSlice; 2]>,
        wrapped: bool,
    ) {
        if slices.is_empty() {
            return;
        }
        let row_source = slices.iter().find_map(|slice| {
            slice.origin.map(|origin| RowSource {
                raw_row: origin.row,
                raw_absolute_row: slice.source.absolute_row,
            })
        });
        output.push(ProjectionPlanRow {
            raw_slices: slices,
            row_source,
            wrapped,
            kind: ProjectedRowKind::Raw,
        });
    }

    /// Apply already validated, non-overlapping collapse ranges to the full
    /// identity document. No cells are materialized: slices continue to point
    /// at compressed history or the resident live grid.
    fn splice_collapses(mut self, collapses: &[ResolvedCollapse], policy_revision: u64) -> Self {
        if collapses.is_empty() {
            self.policy_revision = policy_revision;
            return self;
        }

        let raw_count = self.raw_rows.len();
        let mut owners_by_raw: Vec<SmallVec<[usize; 2]>> =
            (0..raw_count).map(|_| SmallVec::new()).collect();
        for (index, collapse) in collapses.iter().enumerate() {
            for absolute_row in collapse.start_absolute..=collapse.end_absolute {
                if let Some(owners) = owners_by_raw.get_mut(absolute_row) {
                    owners.push(index);
                }
            }
        }
        let mut owner_cursors = vec![0usize; raw_count];

        let mut row_segments: Vec<SmallVec<[HideSegment; 2]>> = Vec::with_capacity(self.rows.len());
        let mut hidden_display_rows = vec![0usize; collapses.len()];
        for row in &self.rows {
            let mut segments: SmallVec<[HideSegment; 2]> = SmallVec::new();
            for slice in &row.raw_slices {
                let Some(owners) = owners_by_raw.get(slice.source.absolute_row) else {
                    continue;
                };
                let slice_start = slice.source.col_start;
                let slice_end = slice_start.saturating_add(slice.len);
                let cursor = &mut owner_cursors[slice.source.absolute_row];
                while let Some(collapse_index) = owners.get(*cursor).copied() {
                    let collapse = collapses[collapse_index];
                    let raw_end = if slice.source.absolute_row == collapse.end_absolute {
                        collapse.range.end.col
                    } else {
                        usize::MAX
                    };
                    if raw_end > slice_start {
                        break;
                    }
                    *cursor += 1;
                }
                let mut owner_index = *cursor;
                while let Some(collapse_index) = owners.get(owner_index).copied() {
                    let collapse = collapses[collapse_index];
                    let raw_start = if slice.source.absolute_row == collapse.start_absolute {
                        collapse.range.start.col
                    } else {
                        0
                    };
                    if raw_start >= slice_end {
                        break;
                    }
                    let raw_end = if slice.source.absolute_row == collapse.end_absolute {
                        collapse.range.end.col
                    } else {
                        usize::MAX
                    };
                    let overlap_start = slice_start.max(raw_start);
                    let overlap_end = slice_end.min(raw_end);
                    if overlap_start < overlap_end {
                        segments.push(HideSegment {
                            collapse: collapse_index,
                            view_start: slice.view_col_start + overlap_start - slice_start,
                            view_end: slice.view_col_start + overlap_end - slice_start,
                        });
                    }
                    if raw_end <= slice_end {
                        owner_index += 1;
                        *cursor = owner_index;
                    } else {
                        break;
                    }
                }
            }

            // A fully blank history line has row provenance but no cell span.
            // Hiding it still changes the projected document by one row.
            if segments.is_empty() && row.raw_slices.is_empty() {
                if let Some(source) = row.row_source {
                    if let Some(owners) = owners_by_raw.get(source.raw_absolute_row) {
                        for collapse_index in owners.iter().copied() {
                            segments.push(HideSegment {
                                collapse: collapse_index,
                                view_start: 0,
                                view_end: self.cols,
                            });
                        }
                    }
                }
            }

            let mut merged: SmallVec<[HideSegment; 2]> = SmallVec::new();
            for segment in segments {
                debug_assert!(merged.last().is_none_or(|previous| {
                    (previous.view_start, previous.collapse)
                        <= (segment.view_start, segment.collapse)
                }));
                if let Some(previous) = merged.last_mut() {
                    if previous.collapse == segment.collapse
                        && segment.view_start <= previous.view_end
                    {
                        previous.view_end = previous.view_end.max(segment.view_end);
                        continue;
                    }
                }
                merged.push(segment);
            }
            let mut last_counted = None;
            for collapse_index in merged.iter().map(|segment| segment.collapse) {
                if last_counted != Some(collapse_index) {
                    hidden_display_rows[collapse_index] =
                        hidden_display_rows[collapse_index].saturating_add(1);
                    last_counted = Some(collapse_index);
                }
            }
            row_segments.push(merged);
        }

        let effective: Vec<bool> = hidden_display_rows
            .iter()
            .map(|hidden_rows| *hidden_rows > 0)
            .collect();
        let mut summary_emitted = vec![false; collapses.len()];
        let mut output = Vec::with_capacity(
            self.rows
                .len()
                .saturating_add(effective.iter().filter(|value| **value).count()),
        );
        for (row, segments) in self.rows.iter().zip(row_segments) {
            let segments: SmallVec<[HideSegment; 2]> = segments
                .into_iter()
                .filter(|segment| effective[segment.collapse])
                .collect();
            if segments.is_empty() {
                output.push(row.clone());
                continue;
            }

            let mut cursor = 0usize;
            for segment in segments {
                Self::push_raw_fragment(
                    &mut output,
                    Self::clipped_slices(row, cursor, segment.view_start),
                    false,
                );
                if !summary_emitted[segment.collapse] {
                    let collapse = collapses[segment.collapse];
                    output.push(ProjectionPlanRow {
                        raw_slices: SmallVec::new(),
                        row_source: None,
                        wrapped: false,
                        kind: ProjectedRowKind::CollapsedSummary {
                            key: SyntheticRowKey {
                                zone_id: collapse.range.zone_id,
                                policy_revision,
                            },
                            hidden_range: collapse.range,
                            hidden_display_rows: hidden_display_rows[segment.collapse],
                        },
                    });
                    summary_emitted[segment.collapse] = true;
                }
                cursor = cursor.max(segment.view_end);
            }
            Self::push_raw_fragment(
                &mut output,
                Self::clipped_slices(row, cursor, self.cols),
                row.wrapped,
            );
        }

        self.rows = output;
        self.raw_slice_count = self.rows.iter().map(|row| row.raw_slices.len()).sum();
        self.policy_revision = policy_revision;
        self.effective_collapsed = collapses
            .iter()
            .enumerate()
            .filter_map(|(index, collapse)| effective[index].then_some(collapse.range.zone_id))
            .collect();
        self.resolved_collapses = collapses
            .iter()
            .enumerate()
            .filter_map(|(index, collapse)| effective[index].then_some(*collapse))
            .collect();
        self.rebuild_raw_row_placements();
        self
    }

    fn summary_row(&self, zone_id: u64) -> Option<usize> {
        self.rows.iter().position(|row| {
            matches!(
                row.kind,
                ProjectedRowKind::CollapsedSummary { key, .. } if key.zone_id == zone_id
            )
        })
    }

    fn raw_absolute_row(&self, raw_row: RawRowId) -> Option<usize> {
        self.raw_rows
            .iter()
            .find(|placement| placement.raw_row == raw_row)
            .map(|placement| placement.absolute_row)
    }

    fn raw_cell_document_row(&self, origin: RawCellOrigin) -> Option<usize> {
        self.rows.iter().position(|row| {
            row.raw_slices.iter().any(|slice| {
                slice.origin.is_some_and(|slice_origin| {
                    slice_origin.row == origin.row
                        && origin.col >= slice_origin.col_start
                        && origin.col < slice_origin.col_start.saturating_add(slice.len)
                })
            })
        })
    }

    /// Document row and column this stable endpoint occupies in *this* plan,
    /// or `None` when it can no longer be placed safely.
    ///
    /// Fails closed on purpose, and deliberately has no summary fallback: the
    /// scroll anchor may land on a collapsed block's summary row, but a
    /// selection endpoint must not. Summary and padding rows carry no raw
    /// slices, so an endpoint parked on one would either copy nothing or, with
    /// both endpoints collapsing onto the same summary, silently yield an
    /// empty clipboard.
    fn selection_point_for_anchor(
        &self,
        anchor: ProjectedSelectionAnchor,
    ) -> Option<(usize, usize)> {
        match anchor {
            ProjectedSelectionAnchor::Cell(origin) => {
                if let Some(row) = self.raw_cell_document_row(origin) {
                    return Some((row, origin.col));
                }
                // A live grid row carries an origin for every column, blanks
                // included, but the same row in scrollback keeps slices only up
                // to its real text. Without this arm an endpoint dropped past
                // the end of a line — which is every triple-click line
                // selection — would evaporate the moment that row scrolled.
                let placement = self
                    .raw_rows
                    .iter()
                    .find(|placement| placement.raw_row == origin.row)?;
                let (first, last) = (placement.first_view_row?, placement.last_view_row?);
                if first != last {
                    // Soft-wrapped across several planned rows: which one holds
                    // a column past the text is ambiguous, so refuse.
                    return None;
                }
                self.row_is_raw(first)
                    .then(|| (first, origin.col.min(self.cols.saturating_sub(1))))
            }
            ProjectedSelectionAnchor::Row { row, col } => {
                let document_row = self.raw_row_document_row(row)?;
                self.row_is_raw(document_row)
                    .then(|| (document_row, col.min(self.cols.saturating_sub(1))))
            }
        }
    }

    /// Whether a planned row carries real content rather than a collapsed
    /// summary or structural padding.
    fn row_is_raw(&self, document_row: usize) -> bool {
        self.rows
            .get(document_row)
            .is_some_and(|row| matches!(row.kind, ProjectedRowKind::Raw))
    }

    fn raw_row_document_row(&self, raw_row: RawRowId) -> Option<usize> {
        self.raw_rows
            .iter()
            .find(|placement| placement.raw_row == raw_row)
            .and_then(|placement| placement.first_view_row)
    }

    fn summary_owning_raw_cell(&self, origin: RawCellOrigin) -> Option<usize> {
        let absolute = self.raw_absolute_row(origin.row)?;
        let collapse = self.resolved_collapses.iter().find(|collapse| {
            if absolute < collapse.start_absolute || absolute > collapse.end_absolute {
                return false;
            }
            let after_start =
                absolute > collapse.start_absolute || origin.col >= collapse.range.start.col;
            let before_end =
                absolute < collapse.end_absolute || origin.col < collapse.range.end.col;
            after_start && before_end
        })?;
        self.summary_row(collapse.range.zone_id)
    }

    fn document_row_for_anchor(&self, anchor: ProjectedTopAnchor) -> Option<usize> {
        match anchor {
            ProjectedTopAnchor::RawCell(origin) => self
                .raw_cell_document_row(origin)
                .or_else(|| self.summary_owning_raw_cell(origin)),
            ProjectedTopAnchor::RawRow(row) => self.raw_row_document_row(row).or_else(|| {
                let absolute = self.raw_absolute_row(row)?;
                let collapse = self.resolved_collapses.iter().find(|collapse| {
                    absolute >= collapse.start_absolute && absolute <= collapse.end_absolute
                })?;
                self.summary_row(collapse.range.zone_id)
            }),
            ProjectedTopAnchor::Summary {
                zone_id,
                hidden_range,
            } => self.summary_row(zone_id).or_else(|| {
                self.raw_cell_document_row(RawCellOrigin {
                    row: hidden_range.start.row,
                    col: hidden_range.start.col,
                })
                .or_else(|| self.raw_row_document_row(hidden_range.start.row))
                .or_else(|| {
                    let start = self.raw_absolute_row(hidden_range.start.row)?;
                    let end = self.raw_absolute_row(hidden_range.end.row)?;
                    self.raw_rows
                        .iter()
                        .filter(|placement| {
                            placement.absolute_row >= start && placement.absolute_row <= end
                        })
                        .find_map(|placement| placement.first_view_row)
                })
            }),
        }
    }

    #[cfg(test)]
    fn metadata_units(&self) -> usize {
        self.rows.len() + self.raw_rows.len() + self.raw_slice_count
    }
}

/// User-owned semantic transforms applied to the primary Block Mode history.
/// An empty policy is the identity and must retain the P0 projection fast path.
#[derive(Clone, Debug, PartialEq, Eq)]
#[allow(dead_code)] // Stage A contract; policy storage/wiring lands in the next slice.
pub struct ProjectionPolicy {
    revision: u64,
    collapsed: BTreeSet<u64>,
}

impl Default for ProjectionPolicy {
    fn default() -> Self {
        Self {
            revision: 1,
            collapsed: BTreeSet::new(),
        }
    }
}

#[allow(dead_code)]
impl ProjectionPolicy {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn revision(&self) -> u64 {
        self.revision
    }

    pub fn is_identity(&self) -> bool {
        self.collapsed.is_empty()
    }

    pub fn is_collapsed(&self, zone_id: u64) -> bool {
        self.collapsed.contains(&zone_id)
    }

    pub fn collapsed_zone_ids(&self) -> impl Iterator<Item = u64> + '_ {
        self.collapsed.iter().copied()
    }

    fn ids(&self) -> SmallVec<[u64; 4]> {
        self.collapsed.iter().copied().collect()
    }

    pub fn collapse(&mut self, zone_id: u64) -> bool {
        if self.collapsed.contains(&zone_id) {
            return false;
        }
        let Some(next_revision) = self.revision.checked_add(1) else {
            return false;
        };
        self.collapsed.insert(zone_id);
        self.revision = next_revision;
        true
    }

    pub fn expand(&mut self, zone_id: u64) -> bool {
        if !self.collapsed.contains(&zone_id) {
            return false;
        }
        let Some(next_revision) = self.revision.checked_add(1) else {
            return false;
        };
        self.collapsed.remove(&zone_id);
        self.revision = next_revision;
        true
    }
}

/// Complete immutable identity of a projected viewport materialization.
/// Consumer caches use this key instead of the diagnostic view revision.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ProjectionKey {
    pub source: ProjectionSourceRevision,
    pub scroll_offset: usize,
    pub rows: usize,
    pub cols: usize,
    pub mode: ProjectionMode,
    pub policy_revision: u64,
    pub policy_ids: std::sync::Arc<[u64]>,
    pub document_rows: usize,
}

/// Immutable view of the existing terminal materialization plus stable raw
/// provenance. P0 is deliberately identity-only: it cannot collapse, filter,
/// delete, or otherwise change visible cells.
#[derive(Clone, Debug)]
#[allow(dead_code)] // Transformed fields are consumed by the P1 UI wiring slice.
pub struct ProjectedViewport {
    cells: std::sync::Arc<Vec<Vec<TerminalCell>>>,
    row_wrapped: std::sync::Arc<Vec<bool>>,
    row_kinds: std::sync::Arc<Vec<ProjectedRowKind>>,
    provenance: std::sync::Arc<ProjectedProvenance>,
    raw_span_index: std::sync::Arc<Vec<usize>>,
    /// Sorted `(raw row, display row)` pairs. A display row may contain cells
    /// from more than one soft-wrapped raw row, while a logically empty raw
    /// row contributes only row provenance and no cell-origin span.
    raw_row_index: std::sync::Arc<Vec<(RawRowId, usize)>>,
    source_revision: ProjectionSourceRevision,
    view_revision: u64,
    identity_fast_path: bool,
    mode: ProjectionMode,
    scroll_offset: usize,
    policy_revision: u64,
    policy_ids: std::sync::Arc<[u64]>,
    document_rows: usize,
    document_start: usize,
    top_padding: usize,
    effective_collapsed: std::sync::Arc<BTreeSet<u64>>,
    plan_revision: u64,
}

#[allow(dead_code)] // Transformed accessors are consumed by the P1 UI wiring slice.
impl ProjectedViewport {
    pub fn cells(&self) -> &[Vec<TerminalCell>] {
        self.cells.as_ref()
    }

    pub fn row_wrapped(&self) -> &[bool] {
        self.row_wrapped.as_ref()
    }

    pub fn row_kinds(&self) -> &[ProjectedRowKind] {
        self.row_kinds.as_ref()
    }

    pub fn view_to_raw(&self, cell: ViewportCell) -> Option<RawCellOrigin> {
        let start = self
            .provenance
            .origin_spans
            .partition_point(|span| span.view_row < cell.row);
        self.provenance.origin_spans[start..]
            .iter()
            .take_while(|span| span.view_row == cell.row)
            .find_map(|span| span.view_to_raw(cell))
    }

    pub fn raw_to_view(&self, origin: RawCellOrigin) -> Option<ViewportCell> {
        if !origin.row.is_tracked() {
            return None;
        }
        let start = self
            .raw_span_index
            .partition_point(|index| self.provenance.origin_spans[*index].raw_row < origin.row);
        self.raw_span_index[start..]
            .iter()
            .map(|index| &self.provenance.origin_spans[*index])
            .take_while(|span| span.raw_row == origin.row)
            .find_map(|span| span.raw_to_view(origin))
    }

    /// Map a non-empty half-open raw range only when one affine origin span
    /// covers it in full. This is an O(log spans) all-or-nothing query for
    /// consumers such as Kitty placements; it never bridges a collapsed gap.
    pub fn raw_range_to_view(&self, start: RawCellOrigin, len: usize) -> Option<ViewportCell> {
        if len == 0 || !start.row.is_tracked() {
            return None;
        }
        let raw_end = start.col.checked_add(len)?;
        let insertion = self.raw_span_index.partition_point(|index| {
            let span = self.provenance.origin_spans[*index];
            (span.raw_row, span.raw_col_start) <= (start.row, start.col)
        });
        let span = insertion
            .checked_sub(1)
            .and_then(|position| self.raw_span_index.get(position))
            .map(|index| self.provenance.origin_spans[*index])?;
        let span_len = span.view_col_end.checked_sub(span.view_col_start)?;
        let span_raw_end = span.raw_col_start.checked_add(span_len)?;
        if span.raw_row != start.row || start.col < span.raw_col_start || raw_end > span_raw_end {
            return None;
        }
        Some(ViewportCell {
            row: span.view_row,
            col: span.view_col_start + start.col - span.raw_col_start,
        })
    }

    /// Inclusive display-row bounds occupied by one tracked raw row.
    pub fn raw_row_view_bounds(&self, row: RawRowId) -> Option<(usize, usize)> {
        if !row.is_tracked() {
            return None;
        }
        let start = self
            .raw_row_index
            .partition_point(|(raw_row, _)| *raw_row < row);
        let mut rows = self.raw_row_index[start..]
            .iter()
            .take_while(|(raw_row, _)| *raw_row == row)
            .map(|(_, view_row)| *view_row);
        let first = rows.next()?;
        Some(rows.fold((first, first), |(min, max), view_row| {
            (min.min(view_row), max.max(view_row))
        }))
    }

    /// Snapshot-local absolute buffer row backing the first real cell on a
    /// display row. Structural padding has no answer and fails closed.
    pub fn view_row_absolute(&self, view_row: usize) -> Option<usize> {
        self.provenance
            .row_sources
            .get(view_row)
            .copied()
            .flatten()
            .map(|source| source.raw_absolute_row)
    }

    pub fn key(&self) -> ProjectionKey {
        ProjectionKey {
            source: self.source_revision,
            scroll_offset: self.scroll_offset,
            rows: self.cells.len(),
            cols: self.cells.first().map_or(0, Vec::len),
            mode: self.mode,
            policy_revision: self.policy_revision,
            policy_ids: std::sync::Arc::clone(&self.policy_ids),
            document_rows: self.document_rows,
        }
    }

    pub fn view_revision(&self) -> u64 {
        self.view_revision
    }

    /// P0 projections are semantic identities even when the terminal's legacy
    /// history materializer itself has to reflow retained rows for display.
    pub fn is_identity(&self) -> bool {
        self.mode != ProjectionMode::Transformed
    }

    /// Whether provenance was built by the direct live-grid O(rows * cols)
    /// path, with no legacy history reflow required.
    pub fn uses_identity_fast_path(&self) -> bool {
        self.identity_fast_path
    }

    pub fn mode(&self) -> ProjectionMode {
        self.mode
    }

    pub fn scroll_offset(&self) -> usize {
        self.scroll_offset
    }

    pub fn policy_revision(&self) -> u64 {
        self.policy_revision
    }

    pub fn document_rows(&self) -> usize {
        self.document_rows
    }

    pub fn document_start(&self) -> usize {
        self.document_start
    }

    pub fn top_padding(&self) -> usize {
        self.top_padding
    }

    pub fn view_document_row(&self, view_row: usize) -> Option<usize> {
        (view_row >= self.top_padding && view_row < self.cells.len()).then(|| {
            self.document_start
                .saturating_add(view_row - self.top_padding)
        })
    }

    pub fn max_scroll_offset(&self) -> usize {
        self.document_rows.saturating_sub(self.cells.len())
    }

    pub fn effective_collapsed(&self) -> &BTreeSet<u64> {
        self.effective_collapsed.as_ref()
    }

    #[cfg(test)]
    fn origin_span_count(&self) -> usize {
        self.provenance.origin_spans.len()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ProjectedViewportCacheKey {
    grid_version: u64,
    history_revision: u64,
    row_identity_revision: u64,
    scroll_offset: usize,
    rows: usize,
    cols: usize,
    use_alt_buffer: bool,
    mode: ProjectionMode,
    policy_revision: u64,
    policy_ids: SmallVec<[u64; 4]>,
    view_scroll_offset: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ProjectionPlanCacheKey {
    history_revision: u64,
    row_identity_revision: u64,
    rows: usize,
    cols: usize,
    row_wrapped: SmallVec<[bool; 64]>,
    policy_revision: u64,
    policy_ids: SmallVec<[u64; 4]>,
    next_zone_id: u64,
    zone_count: usize,
    provenance_count: usize,
}

type ProjectionPlanCache = (ProjectionPlanCacheKey, std::sync::Arc<ProjectionPlan>);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ProjectedTopAnchor {
    RawCell(RawCellOrigin),
    RawRow(RawRowId),
    Summary {
        zone_id: u64,
        hidden_range: FinishedOutputRange,
    },
}

/// Session-owned scroll state for a transformed block document. Alternate
/// screen and Block-off bypass park this state instead of rewriting it.
#[derive(Clone, Debug)]
#[allow(dead_code)] // Session wiring lands in the next slice.
pub struct ProjectionViewState {
    offset_from_bottom: usize,
    follow_bottom: bool,
    top_anchor: Option<ProjectedTopAnchor>,
    last_plan_key: Option<ProjectionPlanCacheKey>,
}

impl Default for ProjectionViewState {
    fn default() -> Self {
        Self {
            offset_from_bottom: 0,
            follow_bottom: true,
            top_anchor: None,
            last_plan_key: None,
        }
    }
}

#[allow(dead_code)] // Session wiring lands in the next slice.
impl ProjectionViewState {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn offset_from_bottom(&self) -> usize {
        self.offset_from_bottom
    }

    pub fn set_offset(&mut self, offset: usize, viewport: &ProjectedViewport) {
        self.offset_from_bottom = offset.min(viewport.max_scroll_offset());
        self.follow_bottom = self.offset_from_bottom == 0;
    }

    pub fn scroll(&mut self, lines: isize, viewport: &ProjectedViewport) {
        let offset = if lines > 0 {
            self.offset_from_bottom.saturating_add(lines as usize)
        } else {
            self.offset_from_bottom.saturating_sub(lines.unsigned_abs())
        };
        self.set_offset(offset, viewport);
    }

    pub fn scroll_to_bottom(&mut self) {
        self.offset_from_bottom = 0;
        self.follow_bottom = true;
        self.top_anchor = None;
    }
}

type ProjectedViewportCache = (ProjectedViewportCacheKey, std::sync::Arc<ProjectedViewport>);

#[derive(Debug)]
struct ProjectedLine {
    cells: Vec<TerminalCell>,
    spans: Vec<LineOriginSpan>,
    row_source: Option<RowSource>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct RowSource {
    raw_row: RawRowId,
    raw_absolute_row: usize,
}

#[derive(Debug)]
struct ProjectedProvenance {
    origin_spans: Vec<OriginSpan>,
    row_sources: Vec<Option<RowSource>>,
}

struct MaterializedProjection {
    cells: Vec<Vec<TerminalCell>>,
    row_wrapped: Vec<bool>,
    row_kinds: Vec<ProjectedRowKind>,
    provenance: ProjectedProvenance,
    scroll_offset: usize,
    document_start: usize,
    top_padding: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct LineOriginSpan {
    view_col_start: usize,
    view_col_end: usize,
    raw_row: RawRowId,
    raw_col_start: usize,
    raw_absolute_row: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct OriginSpan {
    view_row: usize,
    view_col_start: usize,
    view_col_end: usize,
    raw_row: RawRowId,
    raw_col_start: usize,
}

impl OriginSpan {
    fn view_to_raw(&self, cell: ViewportCell) -> Option<RawCellOrigin> {
        (cell.row == self.view_row
            && (self.view_col_start..self.view_col_end).contains(&cell.col)
            && self.raw_row.is_tracked())
        .then_some(RawCellOrigin {
            row: self.raw_row,
            col: self.raw_col_start + cell.col - self.view_col_start,
        })
    }

    fn raw_to_view(&self, origin: RawCellOrigin) -> Option<ViewportCell> {
        let len = self.view_col_end - self.view_col_start;
        (origin.row == self.raw_row
            && (self.raw_col_start..self.raw_col_start + len).contains(&origin.col))
        .then_some(ViewportCell {
            row: self.view_row,
            col: self.view_col_start + origin.col - self.raw_col_start,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SelectionMode {
    Normal,
    Block,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Selection {
    pub anchor: (usize, usize),
    pub active: (usize, usize),
    pub mode: SelectionMode,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ProjectedSelection {
    plan_revision: u64,
    /// Projected width the endpoints were minted at. A rebuild at a different
    /// width rewraps every history group, so the rows *between* the endpoints
    /// change shape even when both endpoints still resolve — re-anchoring
    /// across a resize would silently select different characters.
    plan_cols: usize,
    /// Identity of the effectively-hidden set at mint time.
    ///
    /// This deliberately tracks the *effective* collapses rather than the
    /// requested policy: a collapse can stop being effective with the policy
    /// untouched (its recorded row range stops verifying), which would un-hide
    /// rows between the endpoints and silently extend the highlight over text
    /// the user never dragged across.
    hidden: std::sync::Arc<BTreeSet<u64>>,
    anchor: ProjectedSelectionEndpoint,
    active: ProjectedSelectionEndpoint,
    mode: SelectionMode,
}

/// Stable identity of one projected selection endpoint.
///
/// Document rows are plan-relative and every rebuild renumbers them: once
/// scrollback is at its cap, a single appended line trims the head and shifts
/// every document row by one. An endpoint therefore remembers the retained raw
/// cell it was placed on and is resolved again against each incoming plan.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ProjectedSelectionAnchor {
    /// The endpoint sits on a retained raw cell.
    Cell(RawCellOrigin),
    /// The endpoint names a retained physical row but no retained cell — a
    /// logically blank row, or a column past the row's real text. `col` is a
    /// projected column and is only meaningful at the minting width.
    Row { row: RawRowId, col: usize },
}

/// One endpoint of a projected selection: where it sits in the plan in force
/// right now, plus the identity used to place it again after a rebuild.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ProjectedSelectionEndpoint {
    /// `(document_row, projected_col)` against `ProjectedSelection::plan_revision`.
    point: (usize, usize),
    anchor: ProjectedSelectionAnchor,
}

impl ProjectedSelectionAnchor {
    fn raw_row(self) -> RawRowId {
        match self {
            Self::Cell(origin) => origin.row,
            Self::Row { row, .. } => row,
        }
    }

    /// Re-aim this identity at another column of the same physical row.
    ///
    /// Word and line extension synthesize a column from the *other* endpoint,
    /// which routinely lands past the row's real text, so the result is always
    /// the row-scoped variant: it must never claim to be a retained cell that
    /// does not exist.
    fn with_col(self, col: usize) -> Self {
        Self::Row {
            row: self.raw_row(),
            col,
        }
    }
}

impl ProjectedSelectionEndpoint {
    fn with_col(self, row: usize, col: usize) -> Self {
        Self {
            point: (row, col),
            anchor: self.anchor.with_col(col),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Charset {
    #[default]
    Ascii,
    DecSpecialGraphics,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ClipboardReadKind {
    MimeList,
    MimeData(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClipboardReadRequest {
    pub kind: ClipboardReadKind,
}

#[derive(Clone, Debug)]
pub struct CommandZone {
    /// Stable per-terminal identity. Zone *indices* shift whenever scrollback
    /// trimming drops old zones, so anything that remembers a zone across
    /// frames (block selection) must key on this instead.
    pub id: u64,
    pub prompt_start: usize,
    pub command_start: Option<usize>,
    pub output_start: Option<usize>,
    /// First output column on [`Self::output_start`]. Commands normally start
    /// at column zero; idle asynchronous output may begin beside the prompt.
    pub(crate) output_start_col: usize,
    pub output_end: Option<usize>,
    pub exit_code: Option<i32>,
    /// The executed command line; `None` for background zones (empty prompt).
    pub command: Option<String>,
    /// Wall-clock run time: the shell-reported `duration`/`duration_ms` param
    /// when `D` carried one (shell-measured beats locally measured — family
    /// rule), else the locally timed `C`→`D` span; `None` without either.
    pub duration_ms: Option<u64>,
    /// Unix wall-clock milliseconds when `D` arrived. Rendered on the
    /// selected block's badge and in the Markdown export.
    pub finished_at_ms: Option<u64>,
    /// [`Self::command`] is incomplete: either the shell reported
    /// `cmd_truncated=`, or Frost's bounded capture retained only a safe prefix
    /// (including the fail-closed unavailable placeholder). It is not safe to
    /// re-run; copying is still fine.
    pub command_truncated: bool,
    /// [`Self::command`] came from exact OSC 133 command metadata rather than
    /// prompt-row reconstruction. Task validation replay requires this.
    pub command_exact: bool,
    /// Working directory the command ran in: the shell's OSC 133 `cwd`/
    /// `cwd_url` param when one arrived, else the OSC 7 cwd at `D`.
    pub cwd: Option<String>,
    /// Output text snapshotted at finalization (`D`, or the stale-lifecycle
    /// close at the next `A`), with the same extraction and 1 MiB cap as the
    /// live path; the flag is "rows were dropped by the cap". `Some` only for
    /// non-blank output. This is what keeps copy/Markdown working after the
    /// zone's rows fall out of scrollback (ember's captured-output rule);
    /// `None` once the [`TerminalState::MAX_CAPTURED_OUTPUT_BYTES`] budget
    /// evicted it, in which case live extraction is the fallback.
    pub captured_output: Option<(String, bool)>,
    /// A non-blank [`Self::captured_output`] snapshot was actually discarded
    /// by the aggregate byte budget. Scrollback trimming does not set this:
    /// retained snapshots remain authoritative after their live rows vanish.
    pub(crate) captured_output_evicted: bool,
    /// Whether the matching OSC 133 `C` command-start mark was observed.
    /// Stored independently of row anchors so scrollback eviction cannot
    /// downgrade lifecycle evidence.
    pub(crate) start_mark_seen: bool,
    /// Evidence that closed this zone. This is orthogonal to `exit_code`: a
    /// boundary-inferred close stays unknown, while a shell-reported `D` may
    /// itself omit an exit status.
    pub(crate) completion_provenance: crate::block_mode::CompletionProvenance,
    /// The zone's rows were trimmed out of scrollback. The entry stays (id,
    /// metadata, snapshot — v2 dropped the whole zone here) but all row
    /// fields are meaningless: `prompt_start` is clamped to 0 and the
    /// `Option` rows are `None`. Row consumers (stripes, gutter, markers,
    /// prompt jumps, reveal) must skip such zones.
    pub rows_evicted: bool,
}

#[derive(Clone, Debug)]
struct FinishedOutputProvenance {
    range: FinishedOutputRange,
    row_ids: Vec<RawRowId>,
}

/// Export-facing output state for one retained command zone.
///
/// Unlike [`TerminalState::zone_output_text_capped`], this preserves the
/// distinction between genuinely blank output and non-blank output whose
/// captured snapshot was budget-evicted after its live rows disappeared.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ZoneOutputExport {
    Available { text: String, truncated: bool },
    Empty,
    Unavailable,
}

#[derive(Clone, Debug, Default)]
enum ZoneState {
    #[default]
    Idle,
    PromptStarted(usize),
    CommandStarted(usize, usize),
    OutputStarted(usize, usize, usize),
}

/// Everything [`TerminalState::clear_completed_blocks`] removed, kept so an
/// explicit undo can rebuild it. Single-level: only a clear that actually
/// removed blocks replaces the stash, so a reflexive second Clear Blocks
/// cannot destroy the snapshot (anvil's cleared-stash rule). The bounds are
/// the buffer's own caps: at most [`MAX_COMMAND_ZONES`] zones,
/// `max_scrollback` retained rows, one grid of blanked rows, and the Kitty
/// cache's image-memory budget.
struct ClearedBlocksSnapshot {
    /// Drained scrollback prefix in original row order, keeping each row's
    /// [`RawRowId`] so restored output provenance still validates.
    scrollback: Vec<ScrollbackLine>,
    /// Blanked grid rows as compressed lines (original row ids, top to
    /// bottom). Undo prepends them as scrollback rather than rewriting grid
    /// cells that output produced since the clear may already occupy.
    grid_rows: Vec<ScrollbackLine>,
    zones: VecDeque<CommandZone>,
    provenance: HashMap<u64, FinishedOutputProvenance>,
    captured_output_bytes: usize,
    /// Placements anchored before the live lifecycle at clear time, plus the
    /// image data they referenced. Both return with their original absolute
    /// rows, which name the same text again once the rows are back.
    placements: Vec<KittyPlacement>,
    images: Vec<(u32, KittyImage)>,
}

const MAX_CAPTURED_COMMAND_BYTES: usize = 16 * 1024;
const MAX_PENDING_COMPLETED_COMMANDS: usize = 32;
const MAX_CONSUMED_EXECUTION_IDS: usize = 256;
/// Bound on retained finalized OSC 133 zones; the push path and the
/// undo-clear restore both evict the oldest beyond it.
const MAX_COMMAND_ZONES: usize = 256;
const UNAVAILABLE_COMMAND_TEXT: &str = "<command unavailable>";

/// Bounded command text reconstructed at OSC 133 `C`.
///
/// `truncated` covers both a real byte-cap cut and the fail-closed placeholder
/// used when a `C` lifecycle exists but neither metadata nor retained rows can
/// supply its command. In both cases recall must refuse the stored text.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct CommandCapture {
    text: String,
    truncated: bool,
}

impl CommandCapture {
    fn from_text(text: &str) -> Self {
        if text.len() <= MAX_CAPTURED_COMMAND_BYTES {
            return Self {
                text: text.to_string(),
                truncated: false,
            };
        }
        let mut end = MAX_CAPTURED_COMMAND_BYTES;
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        Self {
            text: text[..end].to_string(),
            truncated: true,
        }
    }

    fn unavailable() -> Self {
        Self {
            text: UNAVAILABLE_COMMAND_TEXT.to_string(),
            truncated: true,
        }
    }

    /// Preserve a real non-blank prefix. A truncated all-whitespace prefix
    /// cannot identify a command and would still classify as Background, so
    /// make the uncertainty explicit and non-executable instead.
    fn ensure_command_identity(mut self) -> Self {
        if self.truncated && self.text.trim().is_empty() {
            self.text = UNAVAILABLE_COMMAND_TEXT.to_string();
        }
        self
    }

    fn push_char(&mut self, character: char) -> bool {
        if self.text.len().saturating_add(character.len_utf8()) > MAX_CAPTURED_COMMAND_BYTES {
            self.truncated = true;
            return false;
        }
        self.text.push(character);
        true
    }
}

/// Visible asynchronous output observed after OSC 133 `B` while the prompt
/// was still clean. It is finalized only by the next `A`, matching
/// Anvil/Forge; a local edit keeps the bytes already observed but prevents
/// later echo/completion text from joining the block. Raw bytes live in a
/// bounded ring independent of the grid, so resize and scrollback eviction
/// cannot rewrite or erase the pending result.
#[derive(Clone, Debug)]
struct IdleBackgroundRawChunk {
    bytes: Vec<u8>,
    start: usize,
    splittable_ascii: bool,
}

impl IdleBackgroundRawChunk {
    fn len(&self) -> usize {
        self.bytes.len().saturating_sub(self.start)
    }
}

#[derive(Clone, Debug)]
struct IdleBackgroundOutput {
    start_row: usize,
    start_row_id: Option<RawRowId>,
    start_col: usize,
    last_row: usize,
    last_row_id: Option<RawRowId>,
    last_col_end: usize,
    anchor_started: bool,
    rows_evicted: bool,
    raw_chunks: VecDeque<IdleBackgroundRawChunk>,
    raw_len: usize,
    raw_truncated: bool,
}

impl IdleBackgroundOutput {
    const CHUNK_BYTES: usize = 4 * 1024;

    fn new(start_row: usize, start_col: usize) -> Self {
        Self {
            start_row,
            start_row_id: None,
            start_col,
            last_row: start_row,
            last_row_id: None,
            last_col_end: start_col,
            anchor_started: false,
            rows_evicted: false,
            raw_chunks: VecDeque::new(),
            raw_len: 0,
            raw_truncated: false,
        }
    }

    /// Append one token emitted by the live parser. Whole-token chunk
    /// eviction prevents the ring head from landing inside CSI/UTF-8, while
    /// omitting terminal control strings ensures their private payload can
    /// never become visible merely because older bytes were evicted.
    fn append(&mut self, input: &[u8], limit: usize) {
        if input.is_empty() {
            return;
        }

        if input.starts_with(b"\x1b")
            && matches!(input.get(1), Some(b']' | b'P' | b'X' | b'^' | b'_'))
        {
            return;
        }

        if limit == 0 {
            self.raw_truncated = true;
            self.raw_chunks.clear();
            self.raw_len = 0;
            return;
        }

        let splittable_ascii = input.iter().all(|byte| (0x20..=0x7e).contains(byte));
        if input.len() > limit {
            self.raw_truncated = true;
            self.raw_chunks.clear();
            self.raw_len = 0;
            // Only plain printable ASCII is safe to split within a parser
            // token. An oversized escape token is discarded atomically.
            if splittable_ascii {
                for piece in input[input.len() - limit..].chunks(Self::CHUNK_BYTES) {
                    self.append_chunk(piece, true, limit);
                }
            }
            return;
        }

        if splittable_ascii {
            for piece in input.chunks(Self::CHUNK_BYTES) {
                self.append_chunk(piece, true, limit);
            }
        } else {
            self.append_chunk(input, false, limit);
        }
    }

    fn append_chunk(&mut self, input: &[u8], splittable_ascii: bool, limit: usize) {
        while self.raw_len.saturating_add(input.len()) > limit {
            let overflow = self.raw_len + input.len() - limit;
            let Some(front) = self.raw_chunks.front_mut() else {
                break;
            };
            let front_len = front.len();
            if front.splittable_ascii && overflow < front_len {
                front.start += overflow;
                self.raw_len -= overflow;
                self.raw_truncated = true;
                break;
            }
            self.raw_chunks.pop_front();
            self.raw_len = self.raw_len.saturating_sub(front_len);
            self.raw_truncated = true;
        }

        let append_to_back = self
            .raw_chunks
            .back()
            .is_some_and(|back| back.bytes.len().saturating_add(input.len()) <= Self::CHUNK_BYTES);
        if append_to_back {
            let back = self
                .raw_chunks
                .back_mut()
                .expect("back chunk was just observed");
            back.bytes.extend_from_slice(input);
            // A mixed-token chunk remains safe to evict atomically from its
            // parser-token boundary, but only an all-printable-ASCII chunk is
            // safe to trim at an arbitrary byte offset.
            back.splittable_ascii &= splittable_ascii;
        } else {
            self.raw_chunks.push_back(IdleBackgroundRawChunk {
                bytes: input.to_vec().into_boxed_slice().into_vec(),
                start: 0,
                splittable_ascii,
            });
        }
        self.raw_len += input.len();
    }

    fn raw_bytes(&self) -> Vec<u8> {
        let mut raw = Vec::with_capacity(self.raw_len);
        for chunk in &self.raw_chunks {
            raw.extend_from_slice(&chunk.bytes[chunk.start..]);
        }
        raw
    }
}

/// Decode retained terminal bytes without inventing U+FFFD glyphs that the
/// live decoder never painted. Valid scalars survive (including a genuine
/// encoded U+FFFD); malformed and incomplete byte runs are dropped and mark
/// the snapshot truncated.
fn decode_utf8_without_replacement(input: &[u8]) -> (String, bool) {
    let mut output = String::with_capacity(input.len());
    let mut remaining = input;
    let mut invalid = false;
    while !remaining.is_empty() {
        match std::str::from_utf8(remaining) {
            Ok(valid) => {
                output.push_str(valid);
                break;
            }
            Err(error) => {
                let valid = error.valid_up_to();
                output.push_str(
                    std::str::from_utf8(&remaining[..valid])
                        .expect("Utf8Error::valid_up_to prefix must be valid"),
                );
                // Do not simply delete malformed bytes: bytes on either side
                // could then join into an ANSI/OSC introducer that never
                // existed in the live stream. NUL is ignored by both the live
                // terminal and our plain renderer, but safely breaks such a
                // sequence (for example ESC, invalid, "[2J").
                output.push('\0');
                invalid = true;
                let skipped = error
                    .error_len()
                    .unwrap_or_else(|| remaining.len().saturating_sub(valid));
                remaining = &remaining[valid.saturating_add(skipped)..];
            }
        }
    }
    (output, invalid)
}

fn has_effective_typeahead(input: &[u8]) -> bool {
    input.iter().any(|byte| !matches!(byte, 0x03 | 0x04))
}

fn has_input_after_submission(input: &[u8]) -> bool {
    let Some(first) = input.iter().position(|byte| matches!(byte, b'\r' | b'\n')) else {
        return false;
    };
    let mut next = first + 1;
    if next < input.len() && matches!((input[first], input[next]), (b'\r', b'\n') | (b'\n', b'\r'))
    {
        next += 1;
    }
    has_effective_typeahead(&input[next..])
}

/// Whether Agent is allowed to submit a reviewed command to this terminal.
///
/// `Ready` deliberately requires OSC 133's prompt-end marker. Guessing from a
/// cursor shape or process name is not sufficient: either can also be true
/// while a foreground program owns the PTY.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AgentPromptStatus {
    Ready,
    Busy,
    InputNotEmpty,
    UnsafeCommand,
    ShellIntegrationUnavailable,
}

impl AgentPromptStatus {
    pub fn is_ready(self) -> bool {
        self == Self::Ready
    }

    pub fn blocked_message(self) -> &'static str {
        match self {
            Self::Ready => "Agent command is ready to run",
            Self::Busy => "Agent command not run: the terminal is busy",
            Self::InputNotEmpty => {
                "Agent command not run: the prompt already contains input; use a fresh empty prompt"
            }
            Self::UnsafeCommand => {
                "Agent command not run: reviewed text contains unsafe invisible or control characters"
            }
            Self::ShellIntegrationUnavailable => {
                "Agent command not run: waiting for an OSC 133 shell prompt"
            }
        }
    }
}

#[derive(Clone, Debug)]
struct ArmedAgentExecution {
    generation: u64,
    prompt_generation: u64,
    command: String,
}

#[derive(Clone, Debug)]
struct ActiveAgentExecution {
    generation: u64,
    execution_id: Option<String>,
}

#[derive(Clone, Debug)]
struct CompletedCommandMetadata {
    exit_code: Option<i32>,
    duration_ms: Option<u64>,
    execution_id: Option<String>,
    agent_generation: Option<u64>,
    completion_provenance: crate::block_mode::CompletionProvenance,
}

#[derive(Clone, Debug, Default)]
struct TerminalModes {
    bits: u64,
}

impl TerminalModes {
    const fn bit_index(mode: u16) -> Option<u32> {
        match mode {
            7 => Some(0),
            25 => Some(1),
            1000 => Some(2),
            1001 => Some(3),
            1002 => Some(4),
            1003 => Some(5),
            1004 => Some(6),
            1006 => Some(7),
            1049 => Some(8),
            2004 => Some(9),
            2026 => Some(10),
            2031 => Some(11),
            5522 => Some(12),
            1 => Some(13),    // DECCKM application cursor keys
            4 => Some(14),    // IRM insert/replace mode
            6 => Some(15),    // DECOM origin mode
            1005 => Some(16), // UTF-8 mouse encoding
            1015 => Some(17), // urxvt mouse encoding
            66 => Some(18),   // DECNKM application keypad mode (ESC = / ESC >)
            47 => Some(19),   // Alternate screen buffer
            1047 => Some(20), // Alternate screen buffer
            1048 => Some(21), // Save/restore cursor
            _ => None,
        }
    }

    #[inline]
    fn contains(&self, mode: &u16) -> bool {
        match Self::bit_index(*mode) {
            Some(bit) => self.bits & (1 << bit) != 0,
            None => false,
        }
    }

    #[inline]
    fn insert(&mut self, mode: u16) {
        if let Some(bit) = Self::bit_index(mode) {
            self.bits |= 1 << bit;
        }
    }

    #[inline]
    fn remove(&mut self, mode: &u16) {
        if let Some(bit) = Self::bit_index(*mode) {
            self.bits &= !(1 << bit);
        }
    }
}

/// Full cursor state saved by DECSC (ESC 7) / CSI s and restored by DECRC (ESC 8) / CSI u.
/// Per the VT spec this captures more than position: SGR attributes, the active charsets,
/// and origin mode.
#[derive(Clone, Copy)]
struct SavedCursor {
    row: usize,
    col: usize,
    fg: Color,
    bg: Color,
    flags: StyleFlags,
    g0: Charset,
    g1: Charset,
    active: Charset,
    origin_mode: bool,
    pending_wrap: bool,
}

pub struct TerminalState {
    pub grid: TerminalGrid,
    alt_grid: TerminalGrid,
    pub scrollback: VecDeque<ScrollbackLine>,
    pub selection: Option<Selection>,
    projected_selection: Option<ProjectedSelection>,
    pub scroll_offset: usize,
    /// Rows an app asked to erase with ED 3 while the viewport was scrolled
    /// back, held until the viewport returns to the live bottom.
    pending_saved_line_purge: usize,
    /// Monotonic count of rows appended to scrollback, used to tell whether the
    /// provisional alternate-screen snapshot is still the tail.
    scrollback_pushes: u64,
    /// The alternate screen's superseding snapshot: `(rows, pushes)` recorded
    /// when it was appended. Replaced by the next synchronized frame, and left
    /// as permanent history once the alternate screen ends or the app scrolls
    /// real rows in behind it.
    provisional_alt_snapshot: Option<(usize, u64)>,
    max_scrollback: usize,
    use_alt_buffer: bool,
    disable_alt_screen: bool,
    viewport_pixel_width: u32,
    viewport_pixel_height: u32,

    pub cursor_row: usize,
    pub cursor_col: usize,
    // Cursor position saved when switching to the alternate screen (mode 1049).
    // Kept separate from the DECSC slot so the two don't clobber each other.
    saved_cursor_row: usize,
    saved_cursor_col: usize,
    // DECSC/DECRC (and CSI s/u) saved full cursor state.
    saved_cursor: Option<SavedCursor>,
    // Full primary-screen drawing state saved across alt-buffer swaps so
    // fullscreen app colors do not leak into hidden main-buffer resizes.
    saved_primary_screen_state: Option<SavedCursor>,
    saved_primary_global_bg: Color,
    saved_primary_cursor_shape: CursorShape,
    saved_primary_dynamic_fg: Option<(u8, u8, u8)>,
    saved_primary_dynamic_bg: Option<(u8, u8, u8)>,
    saved_primary_dynamic_cursor_color: Option<(u8, u8, u8)>,
    saved_primary_dynamic_palette: DynamicColorPalette,
    // Per-column horizontal tab stops (HTS/TBC); index = column.
    tab_stops: Vec<bool>,
    // Last printed character, for REP (CSI b).
    last_printed_char: Option<char>,
    // DEC Last Column Flag: writing in the final column defers autowrap until
    // the next printable character. VTE preserves this through DECSC/DECRC.
    pending_wrap: bool,
    alt_cursor_row: usize,
    alt_cursor_col: usize,
    pub cursor_shape: CursorShape,

    current_fg: Color,
    current_bg: Color,
    current_flags: StyleFlags,
    pub window_title: String,
    icon_title: String,
    title_stack: Vec<(Option<String>, Option<String>)>,
    /// Working directory the child reported through OSC 7, if any. Without it
    /// the cwd comes only from `/proc/<pid>/cwd`, which is the *local* shell's
    /// directory and therefore always wrong once the pane is running ssh.
    current_working_dir: Option<String>,

    // Global background color set by vim (CSI ... m)
    pub global_bg: Color,

    // Scrolling region (DECSTBM)
    scroll_region_top: usize,
    scroll_region_bottom: usize,

    // UTF-8 decoding buffer
    utf8_buf: [u8; 4],
    utf8_len: u8,
    utf8_expected: u8,

    // Incomplete escape sequence buffer across PTY reads. Only short
    // CSI/charset/lone-ESC prefixes land here; the unbounded string states
    // (OSC/DCS/SOS/PM/APC) stream against their own buffers below.
    pending_escape: Vec<u8>,

    // Unterminated OSC (`ESC ]` … BEL/ST) and DCS/SOS/PM (`ESC P`/`ESC X`/
    // `ESC ^` … ST) carried across PTY reads. Like the APC buffer they
    // stream: the scan cursor marks where the terminator scan resumes, so a
    // string split across many reads stays O(n) instead of re-scanning from
    // byte 0 on every read. Overflow keeps the old `pending_escape`
    // semantics exactly: a buffered prefix past the 1 MiB cap is abandoned
    // wholesale and the next read is parsed as ordinary input.
    pending_osc: Vec<u8>,
    pending_osc_scan_from: usize,
    pending_dcs: Vec<u8>,
    pending_dcs_scan_from: usize,

    // Unterminated kitty APC (`ESC _ ... ESC \`) carried across PTY reads.
    // Unlike `pending_escape` (re-scanned from its start each read), the APC
    // buffer streams: `pending_apc_scan_from` marks where the terminator scan
    // resumes, so multi-read image transfers stay O(n) instead of O(n^2), and
    // an oversized packet is rejected + discarded through its ST rather than
    // dropped wholesale (which used to parse the base64 tail as plain input).
    pending_apc: Vec<u8>,
    pending_apc_scan_from: usize,
    // An oversized APC is being discarded: consume bytes without buffering
    // until its ST arrives, tracking a trailing ESC that may precede it.
    discarding_oversized_apc: bool,
    discarding_apc_prev_escape: bool,

    g0_charset: Charset,
    g1_charset: Charset,
    active_charset: Charset,

    // IME support
    pub ime_enabled: bool,
    pub preedit_text: String,
    /// Byte range within `preedit_text` the IME marks as the active cursor /
    /// selection, used to highlight it in the over-the-spot overlay.
    pub preedit_selection: Option<std::ops::Range<usize>>,

    modes: TerminalModes,

    // Output buffer for DSR/CPR responses to be sent back to PTY
    pub output_buffer: Vec<u8>,

    keyboard_enhancement_flags: u16,
    keyboard_enhancement_stack: Vec<u16>,
    alt_keyboard_enhancement_flags: u16,
    alt_keyboard_enhancement_stack: Vec<u16>,
    xterm_modify_other_keys: u16,
    xterm_format_other_keys: u16,
    pending_clipboard_requests: Vec<ClipboardReadRequest>,
    pending_paste_password: Option<String>,

    // Kitty graphics protocol support
    pub kitty_graphics: KittyGraphicsState,

    // P4 优化：行版本化追踪
    pub grid_version: u64,      // 全局网格版本号
    pub row_versions: Vec<u64>, // 每行的修改版本号

    // Cached visible cells to avoid per-frame cloning
    visible_cells_cache: Option<VisibleCellsCache>,
    /// Monotonic revision of retained-history membership/identity.
    history_revision: u64,
    /// Identity projection cache. Its key includes every viewport and raw-row
    /// structural input; cell contents continue to use `grid_version`.
    projected_viewport_cache: Option<ProjectedViewportCache>,
    projection_plan_cache: Option<ProjectionPlanCache>,
    next_projected_view_revision: u64,
    next_projection_plan_revision: u64,

    // OSC 8 hyperlink tracking. Cells retain only the compact terminal-local
    // key; targets live in this bounded interner and are revalidated when
    // materialized into clickable view spans.
    current_hyperlink: Option<u16>,
    osc8_hyperlinks: Vec<Arc<Osc8Hyperlink>>,
    osc8_hyperlink_keys: HashMap<Arc<Osc8Hyperlink>, u16>,

    // Synchronized output (mode 2026): suppress rendering until mode is cleared
    pub sync_output_active: bool,
    sync_output_start: Option<std::time::Instant>,
    last_archived_screen_snapshot: Vec<String>,
    last_synced_primary_screen_snapshot: Vec<String>,

    // OSC 52 clipboard set requests (selection_param, decoded_text)
    pub pending_osc52_clipboard_set: Option<String>,
    // OSC 52 clipboard query pending (needs clipboard read + response)
    pub pending_osc52_clipboard_query: bool,

    // OSC 133 shell integration: command zones for prompt navigation
    pub command_zones: VecDeque<CommandZone>,
    finished_output_provenance: HashMap<u64, FinishedOutputProvenance>,
    /// Next [`CommandZone::id`]; monotonic for the life of the terminal.
    next_zone_id: u64,
    current_zone_state: ZoneState,
    /// Exact cursor column at OSC 133 `B`, after the prompt finished drawing.
    /// The existing zone model stores only rows for navigation; Agent needs
    /// the column as well so prompt text cannot be mistaken for command text.
    current_command_start_col: Option<usize>,
    /// Furthest row on which command-line echo wrote a character after `B`.
    /// This catches text to the right of a cursor moved backwards by readline.
    current_command_extent_row: Option<usize>,
    /// First captured column on the OSC 133 `C` row. Initialized from the
    /// cursor at `C`, then lowered when output moves left and paints there.
    current_output_start_col: Option<usize>,
    current_output_start_row_id: Option<RawRowId>,
    /// Furthest absolute row on which output wrote a character after `C`.
    /// The `D`/next-`A` cursor row alone is not enough: output without a final
    /// newline leaves the cursor on the last output row, which must be included
    /// in the otherwise end-exclusive captured range.
    current_output_extent_row: Option<usize>,
    current_output_extent_col: Option<usize>,
    current_output_extent_row_id: Option<RawRowId>,
    /// Once local input is accepted for this prompt, approval stays blocked
    /// until a fresh `B`. This closes the write-before-echo race.
    agent_prompt_input_tainted: bool,
    /// A CR/LF was already admitted for the current editable prompt, but OSC
    /// 133 C has not arrived yet. A later write in this window is typeahead
    /// for a future prompt rather than another ordinary edit.
    prompt_submission_pending: bool,
    /// Ctrl-C was admitted while editing the current prompt. Unlike a redraw,
    /// the next A/B then represents a deliberate cancellation and may start
    /// clean unless effective bytes followed the interrupt as typeahead.
    prompt_cancel_pending: bool,
    /// Independent prompt-edit gate for idle asynchronous output. Agent input
    /// deliberately does not taint Agent identity, but its echo still must not
    /// become a commandless background block.
    idle_prompt_input_dirty: bool,
    /// Input accepted outside the editable B..C window may survive as shell
    /// typeahead. Carry the conservative taint into the next `B`.
    pending_prompt_typeahead: bool,
    idle_background_output: Option<IdleBackgroundOutput>,
    /// Monotonic identity of the current OSC 133 prompt.
    agent_prompt_generation: u64,
    /// Reviewed command waiting for the next exact OSC 133 `C` transition.
    armed_agent_execution: Option<ArmedAgentExecution>,
    /// Reviewed command whose exact `C` was accepted and whose `D` is pending.
    active_agent_execution: Option<ActiveAgentExecution>,
    /// Bounded command captured at `C`, preferring jsh's `cmdline_url`
    /// metadata. A real lifecycle always stores a non-blank identity here
    /// unless the command itself was exactly blank.
    current_command_text: Option<String>,
    /// Whether [`Self::current_command_text`] came from exact OSC 133
    /// command metadata rather than prompt-row reconstruction. Only
    /// metadata-exact commands may authorize task validation replay.
    current_command_exact: bool,

    // OSC 10/11/12 dynamic colors
    pub dynamic_fg: Option<(u8, u8, u8)>,
    pub dynamic_bg: Option<(u8, u8, u8)>,
    pub dynamic_cursor_color: Option<(u8, u8, u8)>,
    pub dynamic_palette: DynamicColorPalette,

    // OSC 9/777 pending notifications
    pub pending_notifications: Vec<(String, String)>,

    /// Commands finished per OSC 133 `D`, with their text reconstructed from
    /// the buffer. Bounded; drained by the AI agent panel each PTY batch.
    pub pending_completed_commands: std::collections::VecDeque<CompletedCommand>,
    /// jsh execution id announced by the most recent OSC 133 params.
    current_command_id: Option<String>,
    /// Execution id carried specifically by OSC 133 `C`. Kept separate from
    /// [`Self::current_command_id`], which may have been announced earlier on
    /// `A`/`B`, so D correlation can prefer C and then fall back to the prompt
    /// identity without accepting a stale id.
    current_command_start_id: Option<String>,
    /// Recently consumed shell execution ids. Command zones retain a distinct
    /// UI id, so this bounded tombstone deque prevents a delayed duplicate D
    /// from being adopted by a later anonymous lifecycle after the original
    /// completion has already been drained.
    consumed_execution_ids: VecDeque<String>,
    /// When the currently executing command began (OSC 133 `C`), so the
    /// finished record can carry a wall-clock duration.
    current_command_started_at: Option<std::time::Instant>,
    /// Working directory reported by an OSC 133 `cwd`/`cwd_url` param during
    /// the current prompt lifecycle (reset at `A`, consumed at `D`).
    current_command_cwd: Option<String>,
    /// The current command line is incomplete: shell `cmd_truncated=` or the
    /// terminal's bounded/unavailable capture. Carried into the zone at `D`.
    current_command_truncated: bool,
    /// Total bytes of all zones' [`CommandZone::captured_output`] snapshots,
    /// kept under [`Self::MAX_CAPTURED_OUTPUT_BYTES`] by
    /// [`Self::enforce_captured_output_budget`].
    captured_output_bytes: usize,
    /// Blocks removed by the most recent [`Self::clear_completed_blocks`],
    /// kept as data so an explicit undo can rebuild them. Single-level:
    /// replaced by the next clear that removes blocks; consumed by undo.
    cleared_blocks: Option<ClearedBlocksSnapshot>,
}

/// One OSC 133 command lifecycle captured for the AI agent: the typed
/// command line, its exit code (when the shell reported one), and a bounded
/// sample of its output.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompletedCommand {
    pub command: String,
    pub exit_code: Option<i32>,
    pub output: String,
    /// jsh correlation id from an OSC 133 `id=`/`execution_id=` param.
    pub id: Option<String>,
    /// Internal, one-shot Agent approval identity. PTY output cannot choose
    /// this value; it is attached only after an armed command exactly matches
    /// the command captured at OSC 133 `C` (and C/D ids agree when present).
    pub agent_generation: Option<u64>,
    /// Whether the shell reported an output region for this command.
    pub output_available: bool,
    /// The captured output hit the byte cap.
    pub truncated: bool,
    /// Bytes retained (row-reconstructed capture, equals `output.len()`).
    pub total_bytes: usize,
    /// Wall-clock run time from OSC 133 `C` (execution start) to `D`. None
    /// when the shell never reported an execution phase.
    pub duration_ms: Option<u64>,
    /// Evidence that closed the lifecycle. Consumers must not persist or
    /// notify a boundary-inferred event as though the shell reported `D`.
    pub completion_provenance: crate::block_mode::CompletionProvenance,
}

impl TerminalState {
    fn parse_csi_params(param_bytes: &[u8]) -> SmallVec<[u16; 8]> {
        let mut params = SmallVec::new();
        let mut current: u16 = 0;
        let mut has_digits = false;

        for &byte in param_bytes {
            match byte {
                b'0'..=b'9' => {
                    current = current
                        .saturating_mul(10)
                        .saturating_add((byte - b'0') as u16);
                    has_digits = true;
                }
                b';' | b':' => {
                    if has_digits {
                        params.push(current);
                    }
                    current = 0;
                    has_digits = false;
                }
                _ => {}
            }
        }

        if has_digits {
            params.push(current);
        }

        params
    }

    /// Parse SGR parameters into groups. Top-level parameters are separated by ';';
    /// within a group, ':' introduces sub-parameters (ISO 8613-6 / curly underline,
    /// e.g. `4:3` or `38:2:r:g:b`). Empty fields parse as 0 so positions are preserved.
    fn parse_sgr_groups(param_bytes: &[u8]) -> SmallVec<[SmallVec<[u16; 6]>; 8]> {
        let mut groups: SmallVec<[SmallVec<[u16; 6]>; 8]> = SmallVec::new();
        let mut group: SmallVec<[u16; 6]> = SmallVec::new();
        let mut current: u16 = 0;

        for &byte in param_bytes {
            match byte {
                b'0'..=b'9' => {
                    current = current
                        .saturating_mul(10)
                        .saturating_add((byte - b'0') as u16);
                }
                b':' => {
                    group.push(current);
                    current = 0;
                }
                b';' => {
                    group.push(current);
                    groups.push(std::mem::take(&mut group));
                    current = 0;
                }
                _ => {}
            }
        }
        group.push(current);
        groups.push(group);
        groups
    }

    pub fn new(cols: usize, rows: usize) -> Self {
        let (cols, rows) = clamp_terminal_dimensions(cols, rows);
        let grid = TerminalGrid::new(rows, cols);
        let alt_grid = TerminalGrid::new(rows, cols);

        let mut modes = TerminalModes::default();
        modes.insert(25);
        modes.insert(7);

        TerminalState {
            grid,
            alt_grid,
            scrollback: VecDeque::new(),
            selection: None,
            projected_selection: None,
            scroll_offset: 0,
            pending_saved_line_purge: 0,
            scrollback_pushes: 0,
            provisional_alt_snapshot: None,
            max_scrollback: 10000,
            use_alt_buffer: false,
            disable_alt_screen: false,
            viewport_pixel_width: (cols as u32).saturating_mul(8),
            viewport_pixel_height: (rows as u32).saturating_mul(16),
            cursor_row: 0,
            cursor_col: 0,
            saved_cursor_row: 0,
            saved_cursor_col: 0,
            saved_cursor: None,
            saved_primary_screen_state: None,
            saved_primary_global_bg: Color::Default,
            saved_primary_cursor_shape: CursorShape::default(),
            saved_primary_dynamic_fg: None,
            saved_primary_dynamic_bg: None,
            saved_primary_dynamic_cursor_color: None,
            saved_primary_dynamic_palette: [None; 256],
            tab_stops: Self::default_tab_stops(cols),
            last_printed_char: None,
            pending_wrap: false,
            alt_cursor_row: 0,
            alt_cursor_col: 0,
            cursor_shape: CursorShape::default(),
            current_fg: Color::Default,
            current_bg: Color::Default,
            current_flags: StyleFlags::default(),
            window_title: String::new(),
            icon_title: String::new(),
            title_stack: Vec::new(),
            current_working_dir: None,
            global_bg: Color::Default,
            utf8_buf: [0; 4],
            utf8_len: 0,
            utf8_expected: 0,
            pending_escape: Vec::new(),
            pending_osc: Vec::new(),
            pending_osc_scan_from: 0,
            pending_dcs: Vec::new(),
            pending_dcs_scan_from: 0,
            pending_apc: Vec::new(),
            pending_apc_scan_from: 0,
            discarding_oversized_apc: false,
            discarding_apc_prev_escape: false,
            g0_charset: Charset::Ascii,
            g1_charset: Charset::Ascii,
            active_charset: Charset::Ascii,
            ime_enabled: false,
            preedit_text: String::new(),
            preedit_selection: None,
            scroll_region_top: 0,
            scroll_region_bottom: rows.saturating_sub(1),
            modes,
            output_buffer: Vec::new(),
            keyboard_enhancement_flags: 0,
            keyboard_enhancement_stack: Vec::new(),
            alt_keyboard_enhancement_flags: 0,
            alt_keyboard_enhancement_stack: Vec::new(),
            xterm_modify_other_keys: 0,
            xterm_format_other_keys: 0,
            pending_clipboard_requests: Vec::new(),
            pending_paste_password: None,
            kitty_graphics: KittyGraphicsState::new(),
            grid_version: 1,
            // IMPORTANT: row_versions must match grid.rows(), not the parameter 'rows'
            // This ensures dirty tracking works correctly even with scrollback
            row_versions: vec![1; rows], // Use 'rows' here since grid.rows() == rows at init
            visible_cells_cache: None,
            history_revision: 1,
            projected_viewport_cache: None,
            projection_plan_cache: None,
            next_projected_view_revision: 1,
            next_projection_plan_revision: 1,
            current_hyperlink: None,
            osc8_hyperlinks: Vec::new(),
            osc8_hyperlink_keys: HashMap::new(),
            sync_output_active: false,
            sync_output_start: None,
            last_archived_screen_snapshot: Vec::new(),
            last_synced_primary_screen_snapshot: Vec::new(),
            pending_osc52_clipboard_set: None,
            pending_osc52_clipboard_query: false,
            command_zones: VecDeque::new(),
            finished_output_provenance: HashMap::new(),
            next_zone_id: 0,
            current_zone_state: ZoneState::default(),
            current_command_start_col: None,
            current_command_extent_row: None,
            current_output_start_col: None,
            current_output_start_row_id: None,
            current_output_extent_row: None,
            current_output_extent_col: None,
            current_output_extent_row_id: None,
            agent_prompt_input_tainted: false,
            prompt_submission_pending: false,
            prompt_cancel_pending: false,
            idle_prompt_input_dirty: false,
            pending_prompt_typeahead: false,
            idle_background_output: None,
            agent_prompt_generation: 0,
            armed_agent_execution: None,
            active_agent_execution: None,
            current_command_text: None,
            current_command_exact: false,
            dynamic_fg: None,
            dynamic_bg: None,
            dynamic_cursor_color: None,
            dynamic_palette: [None; 256],
            pending_notifications: Vec::new(),
            pending_completed_commands: std::collections::VecDeque::new(),
            current_command_id: None,
            current_command_start_id: None,
            consumed_execution_ids: VecDeque::new(),
            current_command_started_at: None,
            current_command_cwd: None,
            current_command_truncated: false,
            captured_output_bytes: 0,
            cleared_blocks: None,
        }
    }

    fn decode_base64(value: &str) -> Option<String> {
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(value)
            .ok()?;
        String::from_utf8(bytes).ok()
    }

    fn osc_terminator() -> &'static [u8] {
        b"\x1b\\"
    }

    fn append_osc_5522_status(&mut self, metadata: &str, payload: Option<&str>) {
        self.output_buffer.extend_from_slice(b"\x1b]5522;");
        self.output_buffer.extend_from_slice(metadata.as_bytes());
        if let Some(payload) = payload {
            self.output_buffer.extend_from_slice(b";");
            self.output_buffer.extend_from_slice(payload.as_bytes());
        }
        self.output_buffer.extend_from_slice(Self::osc_terminator());
    }

    fn append_osc_color_response(&mut self, command: &str, color: (u8, u8, u8)) {
        let response = format!(
            "\x1b]{};rgb:{:04x}/{:04x}/{:04x}\x1b\\",
            command,
            (color.0 as u16) * 257,
            (color.1 as u16) * 257,
            (color.2 as u16) * 257,
        );
        self.output_buffer.extend_from_slice(response.as_bytes());
    }

    fn append_osc_palette_response(&mut self, idx: u8, color: (u8, u8, u8)) {
        let response = format!(
            "\x1b]4;{};rgb:{:04x}/{:04x}/{:04x}\x1b\\",
            idx,
            (color.0 as u16) * 257,
            (color.1 as u16) * 257,
            (color.2 as u16) * 257,
        );
        self.output_buffer.extend_from_slice(response.as_bytes());
    }

    fn default_256_color(idx: u8) -> (u8, u8, u8) {
        const ANSI: [(u8, u8, u8); 16] = [
            (0, 0, 0),
            (205, 0, 0),
            (0, 205, 0),
            (205, 205, 0),
            (0, 0, 238),
            (205, 0, 205),
            (0, 205, 205),
            (229, 229, 229),
            (127, 127, 127),
            (255, 0, 0),
            (0, 255, 0),
            (255, 255, 0),
            (92, 92, 255),
            (255, 0, 255),
            (0, 255, 255),
            (255, 255, 255),
        ];
        match idx {
            0..=15 => ANSI[idx as usize],
            16..=231 => {
                let idx = idx - 16;
                let r_idx = idx / 36;
                let g_idx = (idx % 36) / 6;
                let b_idx = idx % 6;
                let scale = |v: u8| if v == 0 { 0 } else { 55 + v * 40 };
                (scale(r_idx), scale(g_idx), scale(b_idx))
            }
            232..=255 => {
                let gray = 8 + (idx - 232) * 10;
                (gray, gray, gray)
            }
        }
    }

    fn handle_osc_color(&mut self, command: &str, value: &str) {
        if value == "?" {
            // Query: respond with current color
            let color = match command {
                "10" => self.dynamic_fg.unwrap_or((255, 255, 255)),
                "11" => self.dynamic_bg.unwrap_or((0, 0, 0)),
                "12" => self.dynamic_cursor_color.unwrap_or((255, 255, 255)),
                _ => return,
            };
            self.append_osc_color_response(command, color);
        } else if let Some(rgb) = Self::parse_color_spec(value) {
            match command {
                "10" => self.dynamic_fg = Some(rgb),
                "11" => self.dynamic_bg = Some(rgb),
                "12" => self.dynamic_cursor_color = Some(rgb),
                _ => {}
            }
        }
    }

    /// Apply one OSC 8 state transition (`params;URI`). Invalid, oversized, or
    /// non-openable targets close the current link instead of leaving an older
    /// target armed across attacker-controlled text.
    fn handle_osc8(&mut self, value: &str) {
        let Some((params, uri)) = value.split_once(';') else {
            self.current_hyperlink = None;
            return;
        };
        if uri.is_empty() {
            self.current_hyperlink = None;
            return;
        }

        // Check borrowed fields before any owned allocation enters the
        // interner. URI truncation is deliberately forbidden: it could turn a
        // rejected target into a different, valid destination.
        if uri.len() > MAX_OSC8_URI_BYTES || !crate::link::is_openable_url(uri) {
            self.current_hyperlink = None;
            return;
        }
        let id = params
            .split(':')
            .find_map(|parameter| parameter.strip_prefix("id="));
        if id.is_some_and(|id| id.len() > MAX_OSC8_ID_BYTES) {
            self.current_hyperlink = None;
            return;
        }

        let candidate = Osc8Hyperlink {
            uri: uri.to_owned(),
            id: id.map(str::to_owned),
        };
        if let Some(&key) = self.osc8_hyperlink_keys.get(&candidate) {
            self.current_hyperlink = Some(key);
            return;
        }
        if self.osc8_hyperlinks.len() >= MAX_OSC8_HYPERLINKS {
            self.current_hyperlink = None;
            return;
        }
        let Some(key) = self
            .osc8_hyperlinks
            .len()
            .checked_add(1)
            .and_then(|key| u16::try_from(key).ok())
        else {
            self.current_hyperlink = None;
            return;
        };
        let target = Arc::new(candidate);
        self.osc8_hyperlinks.push(Arc::clone(&target));
        self.osc8_hyperlink_keys.insert(target, key);
        self.current_hyperlink = Some(key);
    }

    /// Materialize OSC 8 cell metadata into visible link spans. Every target
    /// passes the opener policy again here so stale/corrupt keys fail closed;
    /// `open_link` performs the final check at activation time as well.
    pub fn osc8_links_in_visible_cells(
        &self,
        visible_cells: &[Vec<TerminalCell>],
    ) -> Vec<crate::link::Link> {
        let mut links = Vec::new();
        for (line, cells) in visible_cells.iter().enumerate() {
            let mut col = 0;
            while col < cells.len() {
                let key = cells[col].hyperlink;
                if key == 0 {
                    col += 1;
                    continue;
                }
                let col_start = col;
                col += 1;
                while col < cells.len() && cells[col].hyperlink == key {
                    col += 1;
                }
                let Some(target) = self.osc8_hyperlinks.get(usize::from(key) - 1) else {
                    continue;
                };
                if !crate::link::is_openable_url(&target.uri) {
                    continue;
                }
                links.push(crate::link::Link {
                    line,
                    col_start,
                    col_end: col,
                    link_type: crate::link::LinkType::Url,
                    text: target.uri.clone(),
                });
            }
        }
        links
    }

    #[cfg(test)]
    fn osc8_interned_count(&self) -> usize {
        self.osc8_hyperlinks.len()
    }

    fn reset_osc_color(&mut self, command: &str) {
        match command {
            "110" => self.dynamic_fg = None,
            "111" => self.dynamic_bg = None,
            "112" => self.dynamic_cursor_color = None,
            _ => {}
        }
    }

    fn handle_osc_palette(&mut self, value: &str) {
        let mut parts = value.split(';');
        while let Some(idx_s) = parts.next() {
            let Some(color_s) = parts.next() else {
                break;
            };
            let Ok(idx) = idx_s.parse::<u8>() else {
                continue;
            };
            if color_s == "?" {
                let color = self.dynamic_palette[idx as usize]
                    .unwrap_or_else(|| Self::default_256_color(idx));
                self.append_osc_palette_response(idx, color);
            } else if let Some(rgb) = Self::parse_color_spec(color_s) {
                self.dynamic_palette[idx as usize] = Some(rgb);
            }
        }
    }

    fn reset_osc_palette(&mut self, value: &str) {
        if value.is_empty() {
            self.dynamic_palette = [None; 256];
            return;
        }
        for idx_s in value.split(';').filter(|s| !s.is_empty()) {
            if let Ok(idx) = idx_s.parse::<u8>() {
                self.dynamic_palette[idx as usize] = None;
            }
        }
    }

    fn parse_color_spec(spec: &str) -> Option<(u8, u8, u8)> {
        // Parse rgb:RR/GG/BB or rgb:RRRR/GGGG/BBBB or #RRGGBB
        if let Some(hex) = spec.strip_prefix('#') {
            if hex.len() == 6 {
                let r = u8::from_str_radix(&hex[0..2], 16).ok()?;
                let g = u8::from_str_radix(&hex[2..4], 16).ok()?;
                let b = u8::from_str_radix(&hex[4..6], 16).ok()?;
                return Some((r, g, b));
            }
        } else if let Some(rgb) = spec.strip_prefix("rgb:") {
            let parts: Vec<&str> = rgb.split('/').collect();
            if parts.len() == 3 {
                let r = u16::from_str_radix(parts[0], 16).ok()?;
                let g = u16::from_str_radix(parts[1], 16).ok()?;
                let b = u16::from_str_radix(parts[2], 16).ok()?;
                // Normalize to 8-bit
                let scale = if parts[0].len() == 4 { 257 } else { 1 };
                return Some(((r / scale) as u8, (g / scale) as u8, (b / scale) as u8));
            }
        }
        None
    }

    fn decode_osc_metadata(value: &str, max_bytes: usize) -> Option<String> {
        if value.len() > max_bytes.saturating_mul(3) {
            return None;
        }
        let bytes = value.as_bytes();
        let mut decoded = Vec::with_capacity(bytes.len().min(max_bytes));
        let mut index = 0;
        while index < bytes.len() {
            if decoded.len() >= max_bytes {
                return None;
            }
            if bytes[index] == b'%' {
                let high = *bytes.get(index + 1)?;
                let low = *bytes.get(index + 2)?;
                let nibble = |byte: u8| match byte {
                    b'0'..=b'9' => Some(byte - b'0'),
                    b'a'..=b'f' => Some(byte - b'a' + 10),
                    b'A'..=b'F' => Some(byte - b'A' + 10),
                    _ => None,
                };
                decoded.push((nibble(high)? << 4) | nibble(low)?);
                index += 3;
            } else {
                decoded.push(bytes[index]);
                index += 1;
            }
        }
        String::from_utf8(decoded).ok()
    }

    /// Percent-decode command metadata into a UTF-8-safe bounded prefix.
    /// Parsing stops once the retained prefix is full, so a large OSC cannot
    /// force a second unbounded allocation. Malformed/control-bearing retained
    /// text is rejected and the visible prompt capture remains the fallback.
    fn decode_osc_command_metadata(value: &str) -> Option<CommandCapture> {
        let bytes = value.as_bytes();
        let mut decoded = Vec::with_capacity(bytes.len().min(MAX_CAPTURED_COMMAND_BYTES));
        let mut index = 0usize;
        let mut truncated = false;
        while index < bytes.len() {
            if decoded.len() >= MAX_CAPTURED_COMMAND_BYTES {
                truncated = true;
                break;
            }
            if bytes[index] == b'%' {
                let high = *bytes.get(index + 1)?;
                let low = *bytes.get(index + 2)?;
                let nibble = |byte: u8| match byte {
                    b'0'..=b'9' => Some(byte - b'0'),
                    b'a'..=b'f' => Some(byte - b'a' + 10),
                    b'A'..=b'F' => Some(byte - b'A' + 10),
                    _ => None,
                };
                decoded.push((nibble(high)? << 4) | nibble(low)?);
                index += 3;
            } else {
                decoded.push(bytes[index]);
                index += 1;
            }
        }
        truncated |= index < bytes.len();

        let valid_len = match std::str::from_utf8(&decoded) {
            Ok(_) => decoded.len(),
            Err(error) if truncated && error.error_len().is_none() => error.valid_up_to(),
            Err(_) => return None,
        };
        decoded.truncate(valid_len);
        let text = String::from_utf8(decoded).ok()?;
        if text.chars().any(char::is_control) {
            return None;
        }
        Some(CommandCapture { text, truncated })
    }

    /// Exact visible command input after OSC 133 `B`, excluding the prompt.
    fn current_prompt_command_capture(&self) -> Option<CommandCapture> {
        let ZoneState::CommandStarted(_, start_row) = self.current_zone_state else {
            return None;
        };
        let start_col = self.current_command_start_col?;
        let last_row = self
            .current_command_extent_row
            .unwrap_or(start_row)
            .max(start_row);
        let total_rows = self.scrollback.len() + self.grid.rows();
        if start_row >= total_rows {
            return None;
        }
        let last_row = last_row.min(total_rows.saturating_sub(1));
        let mut capture = CommandCapture::default();

        for absolute_row in start_row..=last_row {
            let append_cells = |capture: &mut CommandCapture,
                                cells: &[TerminalCell],
                                wrapped: bool| {
                let first_col = if absolute_row == start_row {
                    start_col.min(cells.len())
                } else {
                    0
                };
                let cells = &cells[first_col..];
                let end = if wrapped {
                    cells.len()
                } else {
                    cells
                        .iter()
                        .rposition(|cell| !cell.flags.wide_continuation() && cell.character != ' ')
                        .map_or(0, |index| index + 1)
                };
                for cell in &cells[..end] {
                    if !cell.flags.wide_continuation() && !capture.push_char(cell.character) {
                        return false;
                    }
                }
                true
            };

            let (appended, wrapped) = if absolute_row < self.scrollback.len() {
                let line = &self.scrollback[absolute_row];
                let cells = line.decompress();
                (
                    append_cells(&mut capture, &cells, line.is_wrapped),
                    line.is_wrapped,
                )
            } else {
                let grid_row = absolute_row - self.scrollback.len();
                let wrapped = self.grid.row_wrapped[grid_row];
                (
                    append_cells(&mut capture, &self.grid[grid_row], wrapped),
                    wrapped,
                )
            };
            if !appended {
                return Some(capture.ensure_command_identity());
            }
            if !wrapped && absolute_row < last_row && !capture.push_char('\n') {
                return Some(capture.ensure_command_identity());
            }
        }
        Some(capture)
    }

    pub fn agent_prompt_status(&self) -> AgentPromptStatus {
        match self.current_zone_state {
            ZoneState::CommandStarted(_, _) => {
                if self.agent_prompt_input_tainted
                    || self
                        .current_prompt_command_capture()
                        .is_none_or(|capture| !capture.text.is_empty())
                {
                    AgentPromptStatus::InputNotEmpty
                } else {
                    AgentPromptStatus::Ready
                }
            }
            ZoneState::OutputStarted(_, _, _) => AgentPromptStatus::Busy,
            ZoneState::Idle | ZoneState::PromptStarted(_) => {
                AgentPromptStatus::ShellIntegrationUnavailable
            }
        }
    }

    /// Record accepted local input before its echo can return from the PTY.
    /// Clearing/editing that line does not re-authorize Agent: a new OSC 133
    /// prompt is the unambiguous reset boundary.
    pub fn note_user_input(&mut self, input: &[u8]) {
        if input.is_empty() {
            return;
        }
        if matches!(self.current_zone_state, ZoneState::CommandStarted(_, _)) {
            if ((self.prompt_submission_pending || self.prompt_cancel_pending)
                && has_effective_typeahead(input))
                || has_input_after_submission(input)
            {
                self.pending_prompt_typeahead = true;
            }
            if let Some(interrupt) = input.iter().position(|byte| *byte == 0x03) {
                if has_effective_typeahead(&input[interrupt + 1..]) {
                    self.pending_prompt_typeahead = true;
                }
                self.prompt_cancel_pending = true;
            }
            self.prompt_submission_pending |=
                input.iter().any(|byte| matches!(byte, b'\r' | b'\n'));
            self.agent_prompt_input_tainted = true;
            self.idle_prompt_input_dirty = true;
        } else if has_effective_typeahead(input) {
            // Input sent to a running command may be readline typeahead. Do
            // not let the next B immediately classify its delayed echo as
            // asynchronous output or authorize an Agent replacement.
            self.pending_prompt_typeahead = true;
        }
    }

    /// Record a terminal-generated protocol reply once it has been admitted
    /// to the PTY write queue. A shell/readline can echo or otherwise consume
    /// these bytes just like typeahead, so they must never authorize Agent or
    /// seed a commandless Background block.
    pub(crate) fn note_protocol_response(&mut self) {
        if matches!(self.current_zone_state, ZoneState::CommandStarted(_, _)) {
            if self.prompt_submission_pending || self.prompt_cancel_pending {
                self.pending_prompt_typeahead = true;
            }
            self.agent_prompt_input_tainted = true;
            self.idle_prompt_input_dirty = true;
        } else {
            self.pending_prompt_typeahead = true;
        }
    }

    pub fn arm_agent_execution(
        &mut self,
        generation: u64,
        command: &str,
    ) -> Result<(), AgentPromptStatus> {
        if crate::review_text::validate_single_line(
            command,
            crate::review_text::MAX_AGENT_COMMAND_BYTES,
        )
        .is_err()
        {
            return Err(AgentPromptStatus::UnsafeCommand);
        }
        let status = self.agent_prompt_status();
        if !status.is_ready() || self.armed_agent_execution.is_some() {
            return Err(if status.is_ready() {
                AgentPromptStatus::Busy
            } else {
                status
            });
        }
        self.armed_agent_execution = Some(ArmedAgentExecution {
            generation,
            prompt_generation: self.agent_prompt_generation,
            command: command.to_string(),
        });
        // Approved Agent payloads submit the line outside bracketed-paste
        // framing. Until C confirms execution, any ordinary write admitted
        // afterward may survive as typeahead for the next prompt.
        self.prompt_submission_pending = true;
        self.idle_prompt_input_dirty = true;
        Ok(())
    }

    pub fn disarm_agent_execution(&mut self, generation: u64) {
        if self
            .armed_agent_execution
            .as_ref()
            .is_some_and(|armed| armed.generation == generation)
        {
            self.armed_agent_execution = None;
            self.prompt_submission_pending = false;
        }
    }

    fn mark_command_echo_extent(&mut self, write_col: usize, write_col_end: usize) {
        // A primary-screen lifecycle deliberately survives an alternate-screen
        // detour, but the detour's cells and cursor coordinates never belong to
        // that lifecycle's command/output rows.
        if self.use_alt_buffer {
            return;
        }
        let absolute_row = self.scrollback.len() + self.cursor_row;
        let raw_row_id = self.grid.raw_row_id(self.cursor_row);
        match self.current_zone_state {
            ZoneState::CommandStarted(_, _) => {
                self.current_command_extent_row = Some(
                    self.current_command_extent_row
                        .unwrap_or(absolute_row)
                        .max(absolute_row),
                );
            }
            ZoneState::OutputStarted(_, _, output_start) => {
                match self.current_output_extent_row.cmp(&Some(absolute_row)) {
                    std::cmp::Ordering::Less => {
                        self.current_output_extent_row = Some(absolute_row);
                        self.current_output_extent_col = Some(write_col_end);
                        self.current_output_extent_row_id = raw_row_id;
                    }
                    std::cmp::Ordering::Equal => {
                        if self.current_output_extent_row_id != raw_row_id {
                            self.current_output_extent_row_id = None;
                        }
                        self.current_output_extent_col = Some(
                            self.current_output_extent_col
                                .unwrap_or(write_col_end)
                                .max(write_col_end),
                        );
                    }
                    std::cmp::Ordering::Greater => {}
                }
                // Output may move left on its first physical row before
                // painting (CR, BS, CUP, progress redraws). The cursor column
                // at C is therefore only the initial lower bound.
                if absolute_row == output_start {
                    self.current_output_start_col = Some(
                        self.current_output_start_col
                            .unwrap_or(write_col)
                            .min(write_col),
                    );
                }
            }
            ZoneState::Idle | ZoneState::PromptStarted(_) => {}
        }
    }

    /// Record a direct printable-cell write while the shell is resting after
    /// OSC 133 `B`. Control-only redraws never call this hook, whitespace can
    /// update an existing candidate but cannot start one, and alt-screen bytes
    /// are deliberately excluded.
    fn note_idle_background_cells(&mut self, col: usize, col_end: usize, has_visible_text: bool) {
        if self.use_alt_buffer
            || self.idle_prompt_input_dirty
            || !matches!(self.current_zone_state, ZoneState::CommandStarted(_, _))
        {
            return;
        }
        let row = self.scrollback.len().saturating_add(self.cursor_row);
        let row_id = self.grid.raw_row_id(self.cursor_row);
        let Some(pending) = self.idle_background_output.as_mut() else {
            return;
        };
        if !pending.anchor_started {
            if has_visible_text {
                pending.start_row = row;
                pending.start_row_id = row_id;
                pending.start_col = col;
                pending.last_row = row;
                pending.last_row_id = row_id;
                pending.last_col_end = col_end;
                pending.anchor_started = true;
            }
            return;
        }
        if row < pending.start_row {
            pending.start_row = row;
            pending.start_row_id = row_id;
            pending.start_col = col;
        } else if row == pending.start_row {
            if pending.start_row_id != row_id {
                pending.start_row_id = None;
            }
            pending.start_col = pending.start_col.min(col);
        }
        if row > pending.last_row {
            pending.last_row = row;
            pending.last_row_id = row_id;
            pending.last_col_end = col_end;
        } else if row == pending.last_row {
            if pending.last_row_id != row_id {
                pending.last_row_id = None;
            }
            pending.last_col_end = pending.last_col_end.max(col_end);
        }
    }

    fn idle_background_capture_active(&self) -> bool {
        !self.use_alt_buffer
            && !self.idle_prompt_input_dirty
            && matches!(self.current_zone_state, ZoneState::CommandStarted(_, _))
            && self.idle_background_output.is_some()
    }

    fn append_idle_background_bytes(&mut self, input: &[u8]) {
        if let Some(pending) = self.idle_background_output.as_mut() {
            pending.append(input, Self::IDLE_BACKGROUND_CAPTURE_BYTES);
        }
    }

    fn execution_id_was_consumed(&self, id: &str) -> bool {
        self.consumed_execution_ids
            .iter()
            .any(|consumed| consumed == id)
    }

    fn remember_consumed_execution_id(&mut self, id: Option<&str>) {
        let Some(id) = id.filter(|id| !id.is_empty()) else {
            return;
        };
        if self.execution_id_was_consumed(id) {
            return;
        }
        if self.consumed_execution_ids.len() >= MAX_CONSUMED_EXECUTION_IDS {
            self.consumed_execution_ids.pop_front();
        }
        self.consumed_execution_ids.push_back(id.to_string());
    }

    /// Dispatch a completed OSC payload (`ESC ]` and the BEL/ST terminator
    /// already stripped), shared by the in-buffer parse and the fragmented
    /// `pending_osc` resume path.
    fn handle_osc_payload(&mut self, payload_bytes: &[u8]) {
        if let Ok(payload) = std::str::from_utf8(payload_bytes) {
            let (command, value) = payload.split_once(';').unwrap_or((payload, ""));
            if !command.is_empty() {
                if command == "0" {
                    let title = Self::sanitized_title(value);
                    self.icon_title.clone_from(&title);
                    self.window_title = title;
                } else if command == "1" {
                    self.icon_title = Self::sanitized_title(value);
                } else if command == "2" {
                    self.window_title = Self::sanitized_title(value);
                } else if command == "7" {
                    // OSC 7 — the child reporting its cwd
                    // as `file://host/%-encoded-path`, or
                    // a bare path. Only overwrite on a
                    // payload we accept: a rejected
                    // remote path must leave the last
                    // known local directory alone rather
                    // than blanking it.
                    if let Some(cwd) = Self::decode_osc7_cwd(value) {
                        self.current_working_dir = Some(cwd);
                    }
                } else if command == "8" {
                    // OSC 8 - Hyperlinks
                    // Format: ESC ] 8 ; params ; URI ST
                    self.handle_osc8(value);
                } else if command == "4" {
                    self.handle_osc_palette(value);
                } else if command == "10" || command == "11" || command == "12" {
                    self.handle_osc_color(command, value);
                } else if command == "110" || command == "111" || command == "112" {
                    self.reset_osc_color(command);
                } else if command == "104" {
                    self.reset_osc_palette(value);
                } else if command == "9" {
                    // Desktop notification (iTerm2/ConEmu)
                    if self.pending_notifications.len() < 8 {
                        let title = "frost".to_string();
                        let body = value.chars().take(256).collect();
                        self.pending_notifications.push((title, body));
                    }
                } else if command == "777" {
                    // rxvt notification: 777;notify;title;body
                    let parts: Vec<&str> = value.splitn(3, ';').collect();
                    if parts.len() >= 2 && parts[0] == "notify" {
                        let title = parts.get(1).unwrap_or(&"").chars().take(256).collect();
                        let body = parts.get(2).unwrap_or(&"").chars().take(256).collect();
                        if self.pending_notifications.len() < 8 {
                            self.pending_notifications.push((title, body));
                        }
                    }
                } else if command == "133" {
                    self.handle_osc_133(value);
                } else if command == "52" {
                    self.handle_osc_52(value);
                } else if command == "5522" {
                    let (metadata, osc_payload) =
                        if let Some((metadata, osc_payload)) = value.split_once(';') {
                            (metadata, Some(osc_payload))
                        } else {
                            (value, None)
                        };
                    self.handle_osc_5522(metadata, osc_payload);
                }
            }
        }
    }

    fn handle_osc_133(&mut self, value: &str) {
        const MAX_EXECUTION_ID_BYTES: usize = 192;
        const MAX_COMMAND_METADATA_BYTES: usize = 16 * 1024;
        // OSC 133 while the alternate screen is active is dropped entirely
        // (ember's rule): `absolute_row` would be computed against the alt
        // grid's cursor, and any zone it produced would corrupt the
        // primary-screen history. A command that flips to the alt screen
        // mid-lifecycle (vim) keeps its pending [`ZoneState`] and finalizes
        // normally when its `D` arrives back on the primary screen.
        if self.use_alt_buffer {
            return;
        }
        let absolute_row = self.scrollback.len() + self.cursor_row;
        let mut parts = value.split(';');
        let mark = match parts.next() {
            Some("A") => 'A',
            Some("B") => 'B',
            Some("C") => 'C',
            Some("D") => 'D',
            _ => return,
        };
        let mut mark_id = None;
        let mut mark_id_present = false;
        let mut metadata_command: Option<CommandCapture> = None;
        let mut metadata_cwd = None;
        let mut metadata_duration_ms = None;
        let mut metadata_truncated = false;
        // jsh correlation metadata rides on C/D as percent-encoded key/value
        // params. Parse it per mark instead of leaving an id in global state
        // where an unrelated later D could inherit it.
        for part in parts {
            if let Some((key, id)) = part.split_once('=') {
                if matches!(key, "id" | "jsh_id" | "execution_id" | "command_id") {
                    mark_id_present = true;
                    mark_id = Self::decode_osc_metadata(id, MAX_EXECUTION_ID_BYTES)
                        .filter(|id| !id.is_empty() && !id.chars().any(char::is_control));
                } else if key == "cmdline_url" {
                    metadata_command = Self::decode_osc_command_metadata(id);
                } else if key == "command" && !id.chars().any(char::is_control) {
                    metadata_command = Some(CommandCapture::from_text(id));
                } else if matches!(key, "cwd" | "cwd_url") {
                    // Both spellings are percent-decoded, so a literal `%`
                    // in a plain `cwd` path is mangled. Family-consistent:
                    // ember decodes the bare key the same way, and shells
                    // that send `cwd` unencoded with a literal `%` are the
                    // ones off-contract.
                    metadata_cwd = Self::decode_osc_metadata(id, MAX_COMMAND_METADATA_BYTES)
                        .filter(|cwd| !cwd.chars().any(char::is_control));
                } else if matches!(key, "duration" | "duration_ms") {
                    metadata_duration_ms = id.trim().parse::<u64>().ok();
                } else if matches!(key, "cmd_truncated" | "command_truncated") {
                    metadata_truncated = matches!(
                        id.trim().to_ascii_lowercase().as_str(),
                        "1" | "true" | "yes" | "on"
                    );
                }
            }
        }
        match mark {
            'A' => {
                // A fresh prompt while a command was still mid-output (`C`
                // seen, `D` lost in an alt-screen detour or crashed shell
                // integration) finalizes that zone instead of silently
                // discarding it — otherwise the pending state would paint
                // the accent "running" stripe forever (ember's rule). A
                // pending `PromptStarted`/`CommandStarted` is NOT finalized:
                // that is just an idle prompt being redrawn (ctrl+l, resize)
                // re-emitting its embedded `A`/`B` marks.
                let carry_dirty_edit =
                    matches!(self.current_zone_state, ZoneState::CommandStarted(_, _))
                        && self.idle_prompt_input_dirty
                        && !self.prompt_submission_pending
                        && !self.prompt_cancel_pending;
                if carry_dirty_edit {
                    self.pending_prompt_typeahead = true;
                }
                self.finalize_stale_zone(absolute_row);
                self.finalize_idle_background_output();
                self.record_abandoned_armed_agent_command();
                // Prompt start. Any leftover execution timestamp belongs to a
                // command that never reported `D`; drop it so it cannot leak
                // into a later command's duration.
                self.current_zone_state = ZoneState::PromptStarted(absolute_row);
                self.current_command_started_at = None;
                self.current_command_start_col = None;
                self.current_command_extent_row = None;
                self.current_output_start_col = None;
                self.current_output_start_row_id = None;
                self.current_output_extent_row = None;
                self.current_output_extent_col = None;
                self.current_output_extent_row_id = None;
                self.current_command_text = None;
                self.current_command_exact = false;
                self.current_command_id = mark_id;
                self.current_command_start_id = None;
                self.current_command_cwd = metadata_cwd;
                self.current_command_truncated = false;
                self.agent_prompt_input_tainted = false;
                self.prompt_submission_pending = false;
                self.prompt_cancel_pending = false;
                self.idle_prompt_input_dirty = false;
                self.armed_agent_execution = None;
                self.active_agent_execution = None;
            }
            'B' => {
                // Command start (user is typing the command)
                if let ZoneState::PromptStarted(prompt_start) = self.current_zone_state {
                    self.current_zone_state = ZoneState::CommandStarted(prompt_start, absolute_row);
                    self.current_command_start_col = Some(self.cursor_col);
                    self.current_command_extent_row = Some(absolute_row);
                    self.current_output_start_col = None;
                    self.current_output_start_row_id = None;
                    self.current_output_extent_row = None;
                    self.current_output_extent_col = None;
                    self.current_output_extent_row_id = None;
                    self.current_command_text = None;
                    self.current_command_exact = false;
                    if mark_id.is_some() {
                        self.current_command_id.clone_from(&mark_id);
                    }
                    if metadata_cwd.is_some() {
                        self.current_command_cwd = metadata_cwd;
                    }
                    let typeahead = std::mem::take(&mut self.pending_prompt_typeahead);
                    self.agent_prompt_input_tainted = typeahead;
                    self.idle_prompt_input_dirty = typeahead;
                    self.idle_background_output =
                        Some(IdleBackgroundOutput::new(absolute_row, self.cursor_col));
                    self.prompt_submission_pending = false;
                    self.prompt_cancel_pending = false;
                    self.agent_prompt_generation = self.agent_prompt_generation.wrapping_add(1);
                    self.armed_agent_execution = None;
                    self.active_agent_execution = None;
                }
            }
            'C' => {
                // Command executed (output begins)
                if let ZoneState::CommandStarted(prompt_start, cmd_start) = self.current_zone_state
                {
                    // Anything printed while the prompt was idle stays inline
                    // when a command starts before the next A; Anvil/Forge do
                    // not splice it into the command's output block.
                    self.idle_background_output = None;
                    self.prompt_submission_pending = false;
                    self.prompt_cancel_pending = false;
                    let metadata_capture = metadata_command
                        .filter(|capture| capture.truncated || !capture.text.trim().is_empty());
                    // Exact means the shell supplied the command as OSC 133
                    // metadata; prompt-row reconstruction stays inexact even
                    // when it happens to match what ran.
                    let command_exact = metadata_capture.is_some();
                    let capture = metadata_capture
                        .or_else(|| self.current_prompt_command_capture())
                        .unwrap_or_else(CommandCapture::unavailable)
                        .ensure_command_identity();
                    let captured_command = capture.text;
                    self.current_command_truncated |= capture.truncated || metadata_truncated;
                    self.current_command_text = Some(captured_command.clone());
                    self.current_command_exact = command_exact;
                    self.current_output_start_col = Some(self.cursor_col);
                    self.current_output_start_row_id = self.grid.raw_row_id(self.cursor_row);
                    self.current_output_extent_row = None;
                    self.current_output_extent_col = None;
                    self.current_output_extent_row_id = None;
                    self.current_command_start_id.clone_from(&mark_id);
                    if mark_id.is_some() {
                        self.current_command_id.clone_from(&mark_id);
                    }
                    if metadata_cwd.is_some() {
                        self.current_command_cwd = metadata_cwd;
                    }
                    let matching_generation = self
                        .armed_agent_execution
                        .as_ref()
                        .filter(|armed| {
                            !self.agent_prompt_input_tainted
                                && !self.current_command_truncated
                                && armed.prompt_generation == self.agent_prompt_generation
                                && armed.command == captured_command
                        })
                        .map(|armed| armed.generation);
                    if let Some(generation) = matching_generation {
                        self.armed_agent_execution = None;
                        self.active_agent_execution = Some(ActiveAgentExecution {
                            generation,
                            execution_id: mark_id.clone(),
                        });
                    }
                    self.current_zone_state =
                        ZoneState::OutputStarted(prompt_start, cmd_start, absolute_row);
                    self.current_command_started_at = Some(std::time::Instant::now());
                }
            }
            'D' => {
                // D closes only a lifecycle that actually reached C. Some
                // integrations emit D on an empty Enter; accepting it here
                // would mint an empty background card and could steal pending
                // asynchronous output that belongs at the next A boundary.
                let ZoneState::OutputStarted(prompt_start, cmd_start, out_start) =
                    self.current_zone_state
                else {
                    return;
                };
                // Command finished. Exit code arrives positionally (`D;0`) or
                // as an jsh-style `exit=`/`exit_code=` param.
                let exit_code =
                    value
                        .split(';')
                        .skip(1)
                        .find_map(|part| match part.split_once('=') {
                            Some(("exit" | "exit_code" | "exit_status", v)) => {
                                v.trim().parse::<i32>().ok()
                            }
                            Some(_) => None,
                            None => part.trim().parse::<i32>().ok(),
                        });
                let d_mark_id = mark_id.clone();
                // An explicitly empty, malformed, control-bearing, or
                // oversized id is not the same as an omitted id. It cannot
                // authorize fallback to whichever execution happens to be
                // current.
                if mark_id_present && d_mark_id.is_none() {
                    return;
                }
                if d_mark_id
                    .as_deref()
                    .is_some_and(|id| self.execution_id_was_consumed(id))
                {
                    return;
                }
                // Correlate ordinary command lifecycles too, not only Agent
                // executions. Prefer C's execution id; when C omitted one,
                // retain the id already adopted from A/B. A stale/spoofed D
                // must not close the live block or consume ids needed by the
                // later matching completion.
                if let Some(finished) = d_mark_id.as_ref() {
                    let expected = self
                        .current_command_start_id
                        .as_ref()
                        .or(self.current_command_id.as_ref());
                    if expected.is_some_and(|expected| expected != finished) {
                        return;
                    }
                }
                // Once an approved command started with a real jsh execution
                // id, only the D carrying that same id may consume it. A fake
                // or stale D is ignored and cannot steal the approval.
                if let Some(active) = self.active_agent_execution.as_ref() {
                    match active.execution_id.as_ref() {
                        Some(expected) if d_mark_id.as_ref() != Some(expected) => return,
                        // A reviewed command whose C had no id must complete
                        // with the same anonymous D shape. A suddenly supplied
                        // id could be a delayed completion from another run.
                        None if d_mark_id.is_some() => return,
                        _ => {}
                    }
                }
                let started_id = self.current_command_id.take();
                let finished_id = mark_id.or(started_id);
                let consumed_id = finished_id.clone();
                let agent_generation = self
                    .active_agent_execution
                    .take()
                    .map(|active| active.generation);
                // A shell-measured `duration=`/`duration_ms=` param wins over
                // the locally timed `C`→`D` span (family rule: the shell saw
                // the whole execution, the terminal only saw the marks).
                let local_duration_ms = self
                    .current_command_started_at
                    .take()
                    .map(|started| started.elapsed().as_millis() as u64);
                let duration_ms = metadata_duration_ms.or(local_duration_ms);
                let command_truncated = self.current_command_truncated || metadata_truncated;
                let cwd = if metadata_cwd.is_some() {
                    metadata_cwd
                } else {
                    self.current_command_cwd.take()
                }
                .or_else(|| self.control_free_osc7_cwd());
                let output_start_col = self.current_output_start_col.unwrap_or(0);
                let output_end = self.live_output_end_row(absolute_row);
                let provenance = self.bind_live_output_provenance(
                    out_start,
                    output_start_col,
                    absolute_row,
                    self.cursor_col,
                );
                let zone = CommandZone {
                    id: 0, // assigned by push_command_zone
                    prompt_start,
                    command_start: Some(cmd_start),
                    output_start: Some(out_start),
                    output_start_col,
                    output_end: Some(output_end),
                    exit_code,
                    command: self.zone_command_text(),
                    duration_ms,
                    finished_at_ms: Self::wall_clock_ms(),
                    command_truncated,
                    command_exact: self.current_command_exact,
                    cwd,
                    captured_output: self.capture_zone_output(
                        out_start,
                        output_end,
                        output_start_col,
                    ),
                    captured_output_evicted: false,
                    start_mark_seen: true,
                    completion_provenance: crate::block_mode::CompletionProvenance::ShellReported,
                    rows_evicted: false,
                };
                self.push_command_zone_with_provenance(zone, provenance);
                self.record_completed_command(
                    cmd_start,
                    out_start,
                    Some((out_start, output_end, output_start_col)),
                    CompletedCommandMetadata {
                        exit_code,
                        duration_ms,
                        execution_id: finished_id,
                        agent_generation,
                        completion_provenance:
                            crate::block_mode::CompletionProvenance::ShellReported,
                    },
                );
                self.remember_consumed_execution_id(consumed_id.as_deref());
                self.current_zone_state = ZoneState::Idle;
                self.current_command_start_col = None;
                self.current_command_extent_row = None;
                self.current_output_start_col = None;
                self.current_output_start_row_id = None;
                self.current_output_extent_row = None;
                self.current_output_extent_col = None;
                self.current_output_extent_row_id = None;
                self.current_command_text = None;
                self.current_command_exact = false;
                self.current_command_start_id = None;
                self.current_command_cwd = None;
                self.current_command_truncated = false;
                self.agent_prompt_input_tainted = false;
                self.prompt_submission_pending = false;
                self.prompt_cancel_pending = false;
                self.idle_prompt_input_dirty = false;
                self.idle_background_output = None;
            }
            _ => {}
        }
    }

    /// Global cap on the bytes held across all zones'
    /// [`CommandZone::captured_output`] snapshots. When exceeded, the OLDEST
    /// zones lose their snapshot (the zone entry stays); a zone whose rows
    /// are still in scrollback then falls back to live extraction.
    pub const MAX_CAPTURED_OUTPUT_BYTES: usize = 8 * 1024 * 1024;

    /// Byte cap of one zone's extracted output text (shared by the live
    /// extraction and the snapshot taken at finalization).
    const ZONE_OUTPUT_CAP_BYTES: usize = 1 << 20;

    /// Newest raw clean-prompt bytes retained until the next OSC 133 `A`.
    /// This matches Anvil/Forge's independent background-output ring and
    /// prevents resize/scrollback geometry from becoming a data-retention
    /// boundary. The finalized plain snapshot is still capped at 1 MiB.
    const IDLE_BACKGROUND_CAPTURE_BYTES: usize = 8 << 20;

    /// Append a finished zone, assigning its stable id and enforcing both the
    /// 256-entry cap and the captured-snapshot byte budget.
    #[allow(dead_code)] // Test/backward-compatible wrapper; production binds provenance.
    fn push_command_zone(&mut self, zone: CommandZone) {
        self.push_command_zone_with_provenance(zone, None);
    }

    fn push_command_zone_with_provenance(
        &mut self,
        mut zone: CommandZone,
        mut provenance: Option<FinishedOutputProvenance>,
    ) {
        let Some(next_zone_id) = self.next_zone_id.checked_add(1) else {
            // Stable ids are UI capabilities. Once the u64 space is exhausted,
            // seal block history instead of reusing an id that a stale menu,
            // selection, or bookmark could still hold.
            log::error!("OSC 133 block id space exhausted; dropping finalized block");
            return;
        };
        zone.id = self.next_zone_id;
        self.next_zone_id = next_zone_id;
        if let Some(provenance) = provenance.as_mut() {
            provenance.range.zone_id = zone.id;
            self.finished_output_provenance
                .insert(zone.id, provenance.clone());
        }
        self.captured_output_bytes = self.captured_output_bytes.saturating_add(
            zone.captured_output
                .as_ref()
                .map_or(0, |(text, _)| text.len()),
        );
        self.command_zones.push_back(zone);
        if self.command_zones.len() > MAX_COMMAND_ZONES {
            if let Some(evicted) = self.command_zones.pop_front() {
                self.finished_output_provenance.remove(&evicted.id);
                self.captured_output_bytes = self.captured_output_bytes.saturating_sub(
                    evicted
                        .captured_output
                        .as_ref()
                        .map_or(0, |(text, _)| text.len()),
                );
            }
        }
        self.enforce_captured_output_budget(Self::MAX_CAPTURED_OUTPUT_BYTES);
    }

    /// Drop the OLDEST zones' snapshots (never the entries themselves) until
    /// the total fits `budget`. The newest zone's snapshot is exempt: one
    /// snapshot is at most [`Self::ZONE_OUTPUT_CAP_BYTES`], well under the
    /// real budget, so evicting older ones always suffices — the exemption
    /// only matters for the tiny budgets tests drive this with.
    fn enforce_captured_output_budget(&mut self, budget: usize) {
        let newest = self.command_zones.back().map(|zone| zone.id);
        while self.captured_output_bytes > budget {
            let Some(zone) = self
                .command_zones
                .iter_mut()
                .find(|zone| zone.captured_output.is_some() && Some(zone.id) != newest)
            else {
                break;
            };
            if let Some((text, _)) = zone.captured_output.take() {
                self.captured_output_bytes = self.captured_output_bytes.saturating_sub(text.len());
                zone.captured_output_evicted = true;
            }
        }
    }

    /// End-exclusive output row at a `D`/next-`A` boundary. If output wrote on
    /// the boundary row, include it even though the cursor never advanced to a
    /// following row (the common `printf foo` without a trailing newline).
    fn live_output_end_row(&self, boundary_row: usize) -> usize {
        let total_rows = self.scrollback.len().saturating_add(self.grid.rows());
        self.current_output_extent_row
            .map(|row| row.saturating_add(1))
            .unwrap_or(boundary_row)
            .max(boundary_row)
            .min(total_rows)
    }

    fn bind_finished_output_provenance(
        &self,
        start_row: usize,
        start_col: usize,
        end_row: usize,
        end_col: usize,
    ) -> Option<FinishedOutputProvenance> {
        let total_rows = self.scrollback.len().checked_add(self.grid.rows())?;
        if start_row >= total_rows || end_row >= total_rows || start_row > end_row {
            return None;
        }
        let (start_id, start_width) = self.retained_raw_row(start_row)?;
        let (end_id, end_width) = self.retained_raw_row(end_row)?;
        if !start_id.is_tracked() || !end_id.is_tracked() {
            return None;
        }

        let mut start_col = start_col.min(start_width);
        if start_col > 0
            && start_col < start_width
            && self.retained_cell_is_wide_continuation(start_row, start_col)?
        {
            start_col -= 1;
        }
        let mut end_col = end_col.min(end_width);
        if end_col < end_width {
            let end_cell = if let Some(line) = self.scrollback.get(end_row) {
                line.decompress().get(end_col).copied()
            } else {
                let grid_row = end_row.checked_sub(self.scrollback.len())?;
                (end_col < self.grid.row_len()).then(|| *self.grid.get(grid_row, end_col))
            }?;
            if end_cell.flags.wide_continuation() {
                end_col = end_col.saturating_add(1).min(end_width);
            }
        }
        if start_row == end_row && start_col >= end_col {
            return None;
        }

        let row_ids: Vec<_> = (start_row..=end_row)
            .map(|absolute_row| self.retained_raw_row(absolute_row).map(|row| row.0))
            .collect::<Option<_>>()?;
        if row_ids.iter().any(|row| !row.is_tracked()) {
            return None;
        }
        Some(FinishedOutputProvenance {
            range: FinishedOutputRange {
                zone_id: 0,
                start: RawCellBoundary {
                    row: start_id,
                    col: start_col,
                },
                end: RawCellBoundary {
                    row: end_id,
                    col: end_col,
                },
            },
            row_ids,
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn bind_output_provenance(
        &self,
        start_row: usize,
        start_row_id: RawRowId,
        start_col: usize,
        extent_row: usize,
        extent_row_id: RawRowId,
        extent_col: usize,
        boundary_row: usize,
        boundary_col: usize,
        pending_wrap: bool,
    ) -> Option<FinishedOutputProvenance> {
        if self.retained_raw_row(start_row)?.0 != start_row_id
            || self.retained_raw_row(extent_row)?.0 != extent_row_id
        {
            return None;
        }
        let (end_row, end_col) = if boundary_row > extent_row {
            let end_row = boundary_row.checked_sub(1)?;
            (end_row, self.retained_raw_row(end_row)?.1)
        } else if boundary_row == extent_row {
            if pending_wrap {
                (extent_row, self.retained_raw_row(extent_row)?.1)
            } else if boundary_col < extent_col {
                return None;
            } else {
                (extent_row, boundary_col)
            }
        } else {
            return None;
        };
        self.bind_finished_output_provenance(start_row, start_col, end_row, end_col)
    }

    fn bind_live_output_provenance(
        &self,
        start_row: usize,
        start_col: usize,
        boundary_row: usize,
        boundary_col: usize,
    ) -> Option<FinishedOutputProvenance> {
        self.bind_output_provenance(
            start_row,
            self.current_output_start_row_id?,
            start_col,
            self.current_output_extent_row?,
            self.current_output_extent_row_id?,
            self.current_output_extent_col?,
            boundary_row,
            boundary_col,
            self.pending_wrap,
        )
    }

    /// Snapshot one zone's output rows for [`CommandZone::captured_output`]:
    /// the exact live extraction (same trimming, same 1 MiB cap, same
    /// whole-rows-only truncation flag), blank output collapsing to `None`.
    fn capture_zone_output(
        &self,
        start: usize,
        end: usize,
        start_col: usize,
    ) -> Option<(String, bool)> {
        let (out, capped) =
            self.rows_text_from_column(start, end, start_col, Self::ZONE_OUTPUT_CAP_BYTES);
        if out.trim().is_empty() {
            None
        } else {
            Some((out, capped))
        }
    }

    /// At the next prompt boundary, render clean-prompt bytes through an
    /// isolated plain terminal model and turn meaningful output into one
    /// commandless block. It is history/UI only: no command-completion/Agent
    /// event is emitted.
    fn finalize_idle_background_output(&mut self) {
        let Some(pending) = self.idle_background_output.take() else {
            return;
        };
        let ZoneState::CommandStarted(prompt_start, _) = self.current_zone_state else {
            return;
        };
        if !pending.anchor_started {
            return;
        }
        let raw = pending.raw_bytes();
        let IdleBackgroundOutput {
            start_row,
            start_row_id,
            start_col,
            last_row,
            last_row_id,
            last_col_end,
            anchor_started: _,
            rows_evicted,
            raw_chunks: _,
            raw_len: _,
            raw_truncated,
        } = pending;
        let (raw, invalid_utf8) = decode_utf8_without_replacement(&raw);
        let (plain, render_truncated) =
            crate::ansi::terminal_plain_text(&raw, Self::ZONE_OUTPUT_CAP_BYTES);
        if !plain
            .chars()
            .any(|character| !character.is_whitespace() && !character.is_control())
        {
            return;
        }
        let captured_output = (plain, raw_truncated || invalid_utf8 || render_truncated);
        let cwd = self
            .current_command_cwd
            .clone()
            .or_else(|| self.control_free_osc7_cwd());
        let total_rows = self.scrollback.len().saturating_add(self.grid.rows());
        let output_end = last_row.saturating_add(1).min(total_rows);
        let (prompt_start, output_start, output_start_col, output_end) = if rows_evicted {
            (0, None, 0, None)
        } else {
            (prompt_start, Some(start_row), start_col, Some(output_end))
        };
        let provenance = (!rows_evicted)
            .then(|| {
                let start_id = start_row_id?;
                let last_id = last_row_id?;
                if self.retained_raw_row(start_row)?.0 != start_id
                    || self.retained_raw_row(last_row)?.0 != last_id
                {
                    return None;
                }
                self.bind_finished_output_provenance(start_row, start_col, last_row, last_col_end)
            })
            .flatten();
        self.push_command_zone_with_provenance(
            CommandZone {
                id: 0,
                prompt_start,
                command_start: None,
                output_start,
                output_start_col,
                output_end,
                exit_code: None,
                command: None,
                duration_ms: None,
                finished_at_ms: Self::wall_clock_ms(),
                command_truncated: false,
                command_exact: false,
                cwd,
                captured_output: Some(captured_output),
                captured_output_evicted: false,
                start_mark_seen: false,
                completion_provenance: crate::block_mode::CompletionProvenance::BoundaryInferred,
                rows_evicted,
            },
            provenance,
        );
    }

    /// The OSC 7 cwd, refused if it carries control characters: a zone cwd
    /// feeds line-oriented consumers (the Markdown export's `- Cwd:` line),
    /// so it gets the same control-free guarantee as the OSC 133 params
    /// (OSC 7 itself only rejects NUL — a path with, say, a newline is still
    /// openable).
    fn control_free_osc7_cwd(&self) -> Option<String> {
        self.current_working_dir
            .clone()
            .filter(|cwd| !cwd.chars().any(char::is_control))
    }

    /// Close out a lifecycle whose command really ran (`C` fired) but whose
    /// `D` never arrived (lost in an alt-screen detour, crashed shell
    /// integration) when the next `A` shows up (ember's stale-record rule).
    /// ONLY an un-finalized `OutputStarted` finalizes: `CommandStarted` is
    /// the RESTING state of an idle prompt (`B` fires at prompt-end, `C`
    /// only at execution), and shells re-emit `A`/`B` on every prompt
    /// repaint — readline's ctrl+l, fish re-running `fish_prompt` on
    /// SIGWINCH — so finalizing it would mint one junk zone per redraw.
    /// Pending `PromptStarted`/`CommandStarted` states are silently
    /// discarded, exactly as before the stale-finalize rule existed.
    ///
    /// The finalized zone ends where the new prompt begins, keeps whatever
    /// the shell did report (command text captured at `C`, cwd,
    /// `cmd_truncated`) and invents nothing: no exit code (⇒ the existing
    /// Unknown `?` badge), no duration, no finish timestamp. It does publish a
    /// provenance-tagged completion event so a locally correlated Agent wait
    /// can terminate cleanly; persistence and notifications explicitly accept
    /// only `ShellReported` events.
    fn finalize_stale_zone(&mut self, boundary_row: usize) {
        let ZoneState::OutputStarted(prompt_start, cmd_start, out_start) = self.current_zone_state
        else {
            return;
        };
        let cwd = self
            .current_command_cwd
            .take()
            .or_else(|| self.control_free_osc7_cwd());
        let output_start_col = self.current_output_start_col.unwrap_or(0);
        let output_end = self.live_output_end_row(boundary_row);
        let provenance = self.bind_live_output_provenance(
            out_start,
            output_start_col,
            boundary_row,
            self.cursor_col,
        );
        let zone = CommandZone {
            id: 0, // assigned by push_command_zone
            prompt_start,
            command_start: Some(cmd_start),
            output_start: Some(out_start),
            output_start_col,
            output_end: Some(output_end),
            exit_code: None,
            command: self.zone_command_text(),
            duration_ms: None,
            finished_at_ms: None,
            command_truncated: self.current_command_truncated,
            command_exact: self.current_command_exact,
            cwd,
            captured_output: self.capture_zone_output(out_start, output_end, output_start_col),
            captured_output_evicted: false,
            start_mark_seen: true,
            completion_provenance: crate::block_mode::CompletionProvenance::BoundaryInferred,
            rows_evicted: false,
        };
        self.push_command_zone_with_provenance(zone, provenance);
        let execution_id = self.current_command_id.take();
        let consumed_id = execution_id.clone();
        let agent_generation = self
            .active_agent_execution
            .take()
            .map(|active| active.generation);
        self.record_completed_command(
            cmd_start,
            out_start,
            Some((out_start, output_end, output_start_col)),
            CompletedCommandMetadata {
                exit_code: None,
                duration_ms: None,
                execution_id,
                agent_generation,
                completion_provenance: crate::block_mode::CompletionProvenance::BoundaryInferred,
            },
        );
        self.remember_consumed_execution_id(consumed_id.as_deref());
    }

    /// Command line for a finished zone: the exact text captured at `C`
    /// (metadata or prompt-row extraction), or `None` when `C` never fired.
    /// There is deliberately no whole-row fallback: reading raw rows would
    /// scrape the rendered prompt into the "command" (an empty-prompt Enter
    /// under bash-preexec-style integrations emits `D` without `C`), turning
    /// idle prompts into phantom Failed blocks. No `C` means no command, and
    /// the zone classifies as Background. Blank commands also collapse to
    /// `None` so a background zone is recognizable as such.
    fn zone_command_text(&self) -> Option<String> {
        self.current_command_text
            .clone()
            .filter(|command| !command.trim().is_empty())
    }

    fn wall_clock_ms() -> Option<u64> {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .ok()
            .map(|elapsed| elapsed.as_millis() as u64)
    }

    /// Keep OSC 133 bookkeeping aligned with the buffer after `count` rows
    /// fell off the top of scrollback. Completed zones anchored in the
    /// trimmed region KEEP their entry (id, metadata, captured snapshot) but
    /// lose their rows — v2 dropped the whole zone here, silently emptying
    /// copy/Markdown for old zones; the snapshot taken at finalization is
    /// what makes retention meaningful. The in-progress zone clamps to row
    /// zero so a still-running command keeps producing a usable zone.
    fn bump_history_revision(&mut self) {
        self.history_revision = self.history_revision.wrapping_add(1).max(1);
        self.visible_cells_cache = None;
        self.projected_viewport_cache = None;
    }

    /// ED 3 ("erase saved lines").
    ///
    /// Honored immediately while the viewport already follows the live bottom,
    /// which is where a user-typed `clear` always runs — typing scrolls to the
    /// bottom first. An app that erases saved lines *while the user is scrolled
    /// back reading them* is a different situation: full-screen TUIs re-lay out
    /// their transcript by emitting `ED 2` + `ED 3` and repainting (codex-cli
    /// does this on every re-render), and obeying that mid-read deleted the
    /// history under the reader and snapped the viewport to the bottom. Defer
    /// instead: remember how many rows the app asked to drop and retire exactly
    /// that prefix once the viewport is back at the bottom, so the app's request
    /// still lands without interrupting the read.
    fn erase_saved_lines(&mut self) {
        if self.scroll_offset > 0 {
            self.pending_saved_line_purge = self.scrollback.len();
            return;
        }
        self.purge_saved_lines(self.scrollback.len());
    }

    /// Retire the oldest `rows` scrollback rows, rebasing every row-indexed
    /// structure through the shared trim bookkeeping.
    fn purge_saved_lines(&mut self, rows: usize) {
        self.pending_saved_line_purge = 0;
        let rows = rows.min(self.scrollback.len());
        if rows == 0 {
            return;
        }
        self.scrollback.drain(..rows);
        self.on_scrollback_rows_trimmed(rows);
        self.clear_text_selection();
        self.scroll_offset = self.scroll_offset.min(self.scrollback.len());
        self.grid_version = self.grid_version.wrapping_add(1);
        self.visible_cells_cache = None;
    }

    /// Apply a deferred [`Self::erase_saved_lines`] once the viewport follows
    /// the live bottom again.
    fn settle_pending_saved_line_purge(&mut self) {
        if self.pending_saved_line_purge > 0 && self.scroll_offset == 0 {
            self.purge_saved_lines(self.pending_saved_line_purge);
        }
    }

    /// Drop captured-output provenance for zones that no longer own live
    /// rows. Callers gate this on an actual eviction: provenance is orphaned
    /// only when a zone flips `rows_evicted`, leaves the bounded deque, or the
    /// deque is cleared outright, and those last two drop provenance
    /// themselves.
    fn drop_orphaned_output_provenance(&mut self) {
        #[cfg(test)]
        PROVENANCE_ORPHAN_SCAN_COUNT.with(|count| count.set(count.get().saturating_add(1)));
        self.finished_output_provenance.retain(|zone_id, _| {
            self.command_zones
                .iter()
                .any(|zone| zone.id == *zone_id && !zone.rows_evicted)
        });
    }

    fn on_scrollback_rows_trimmed(&mut self, count: usize) {
        if count == 0 {
            return;
        }
        // Rows the cap (or any other trim) already dropped are rows a deferred
        // ED 3 must no longer count, or settling it would eat live content.
        self.pending_saved_line_purge = self.pending_saved_line_purge.saturating_sub(count);
        self.bump_history_revision();
        self.kitty_graphics.on_buffer_rows_trimmed(count);
        // Only a zone flipping to `rows_evicted` can orphan provenance, and a
        // single-row trim flips at most one. Rescanning the whole deque on
        // every trimmed row would be a fixed per-line tax on streaming output
        // once scrollback is capped and zones have accumulated.
        let mut evicted_any = false;
        for zone in &mut self.command_zones {
            if zone.rows_evicted {
                continue;
            }
            if zone.prompt_start < count {
                zone.rows_evicted = true;
                evicted_any = true;
                zone.prompt_start = 0;
                zone.command_start = None;
                zone.output_start = None;
                zone.output_start_col = 0;
                zone.output_end = None;
                continue;
            }
            zone.prompt_start -= count;
            for row in [
                zone.command_start.as_mut(),
                zone.output_start.as_mut(),
                zone.output_end.as_mut(),
            ]
            .into_iter()
            .flatten()
            {
                *row = row.saturating_sub(count);
            }
        }
        if evicted_any {
            self.drop_orphaned_output_provenance();
        }
        let live_prompt_trimmed = match self.current_zone_state {
            ZoneState::PromptStarted(prompt_start)
            | ZoneState::CommandStarted(prompt_start, _)
            | ZoneState::OutputStarted(prompt_start, _, _) => prompt_start < count,
            ZoneState::Idle => false,
        };
        let live_output_start_trimmed = matches!(
            self.current_zone_state,
            ZoneState::OutputStarted(_, _, output_start) if output_start < count
        );
        self.current_zone_state = match std::mem::take(&mut self.current_zone_state) {
            ZoneState::Idle => ZoneState::Idle,
            ZoneState::PromptStarted(p) => ZoneState::PromptStarted(p.saturating_sub(count)),
            ZoneState::CommandStarted(p, c) => {
                ZoneState::CommandStarted(p.saturating_sub(count), c.saturating_sub(count))
            }
            ZoneState::OutputStarted(p, c, o) => ZoneState::OutputStarted(
                p.saturating_sub(count),
                c.saturating_sub(count),
                o.saturating_sub(count),
            ),
        };
        self.current_command_extent_row = self
            .current_command_extent_row
            .map(|row| row.saturating_sub(count));
        self.current_output_extent_row = self
            .current_output_extent_row
            .map(|row| row.saturating_sub(count));
        if live_output_start_trimmed {
            self.current_output_start_col = Some(0);
        }
        if let Some(pending) = self.idle_background_output.as_mut() {
            pending.rows_evicted |= live_prompt_trimmed || pending.start_row < count;
            if pending.start_row < count {
                pending.start_row = 0;
                pending.start_col = 0;
            } else {
                pending.start_row -= count;
            }
            if pending.last_row < count {
                pending.last_row = 0;
            } else {
                pending.last_row -= count;
            }
        }
    }

    /// Inverse of [`Self::on_scrollback_rows_trimmed`] for undo-clear: `count`
    /// rows came back at the front of the buffer, so every anchor into
    /// post-clear rows shifts up to stay on the same text. The restored
    /// structures keep their pre-clear anchors, which name the same rows again
    /// once the prefix is back. `pending_saved_line_purge` deliberately does
    /// not grow: a deferred ED 3 counted rows the application wanted gone,
    /// and an explicit user undo does not re-enlist the restored rows.
    fn on_scrollback_rows_inserted(&mut self, count: usize) {
        if count == 0 {
            return;
        }
        for zone in &mut self.command_zones {
            if zone.rows_evicted {
                continue;
            }
            zone.prompt_start = zone.prompt_start.saturating_add(count);
            for row in [
                zone.command_start.as_mut(),
                zone.output_start.as_mut(),
                zone.output_end.as_mut(),
            ]
            .into_iter()
            .flatten()
            {
                *row = row.saturating_add(count);
            }
        }
        self.current_zone_state = match std::mem::take(&mut self.current_zone_state) {
            ZoneState::Idle => ZoneState::Idle,
            ZoneState::PromptStarted(p) => ZoneState::PromptStarted(p.saturating_add(count)),
            ZoneState::CommandStarted(p, c) => {
                ZoneState::CommandStarted(p.saturating_add(count), c.saturating_add(count))
            }
            ZoneState::OutputStarted(p, c, o) => ZoneState::OutputStarted(
                p.saturating_add(count),
                c.saturating_add(count),
                o.saturating_add(count),
            ),
        };
        self.current_command_extent_row = self
            .current_command_extent_row
            .map(|row| row.saturating_add(count));
        self.current_output_extent_row = self
            .current_output_extent_row
            .map(|row| row.saturating_add(count));
        if let Some(pending) = self.idle_background_output.as_mut() {
            pending.start_row = pending.start_row.saturating_add(count);
            pending.last_row = pending.last_row.saturating_add(count);
        }
    }

    /// Absolute buffer row of every known shell prompt (OSC 133), oldest
    /// first, including the prompt of a command that is still running.
    fn prompt_rows(&self) -> impl Iterator<Item = usize> + '_ {
        let active = match self.current_zone_state {
            ZoneState::Idle => None,
            ZoneState::PromptStarted(p)
            | ZoneState::CommandStarted(p, _)
            | ZoneState::OutputStarted(p, _, _) => Some(p),
        };
        self.command_zones
            .iter()
            .filter(|zone| !zone.rows_evicted)
            .map(|zone| zone.prompt_start)
            .chain(active)
    }

    /// Whether this pane has seen any OSC 133 prompt mark at all.
    ///
    /// Distinguishes "your shell reports no prompts" from "there is no prompt
    /// in that direction": both make the jump helpers return false, but only
    /// the first is worth telling the user how to fix.
    pub fn has_prompt_marks(&self) -> bool {
        self.prompt_rows().next().is_some()
    }

    /// Plain text of the most recent completed command's output (OSC 133),
    /// soft-wrapped rows joined and per-line trailing padding trimmed.
    /// Returns None when no zone recorded any output or the output is blank.
    /// Capped at 1 MiB so a huge scrollback range cannot balloon a clipboard
    /// write. Reads through the same captured-snapshot-first path as
    /// [`Self::zone_output_text`], so it keeps answering after the zone's
    /// rows fell out of scrollback.
    pub fn last_command_output_text(&self) -> Option<String> {
        let zone = self
            .command_zones
            .iter()
            .rev()
            .find(|zone| zone.captured_output.is_some() || zone.output_start.is_some())?;
        self.zone_output_text(zone.id)
    }

    /// Look up a retained finalized zone by its stable id. This includes a
    /// lifecycle closed at the next prompt after `D` was lost; consult
    /// [`CommandZone::completion_provenance`] when that distinction matters.
    /// `None` means the bounded zone deque no longer retains the id.
    pub fn zone_by_id(&self, id: u64) -> Option<&CommandZone> {
        self.command_zones.iter().find(|zone| zone.id == id)
    }

    #[allow(dead_code)] // Used by the Stage A resolver before consumer wiring lands.
    fn retained_raw_row(&self, absolute_row: usize) -> Option<(RawRowId, usize)> {
        if let Some(line) = self.scrollback.get(absolute_row) {
            return Some((line.raw_row_id, line.cols as usize));
        }
        let grid_row = absolute_row.checked_sub(self.scrollback.len())?;
        Some((self.grid.raw_row_id(grid_row)?, self.grid.row_len()))
    }

    #[allow(dead_code)] // Used by the Stage A resolver before consumer wiring lands.
    fn retained_cell_is_wide_continuation(&self, absolute_row: usize, col: usize) -> Option<bool> {
        if let Some(line) = self.scrollback.get(absolute_row) {
            return line
                .decompress()
                .get(col)
                .map(|cell| cell.flags.wide_continuation());
        }
        let grid_row = absolute_row.checked_sub(self.scrollback.len())?;
        (col < self.grid.row_len()).then(|| self.grid.get(grid_row, col).flags.wide_continuation())
    }

    /// Resolve a finalized zone's live output rows to stable, exact raw-cell
    /// boundaries. Trimmed, malformed, empty, or untracked ranges fail closed.
    /// If the recorded start cuts a wide glyph, the range expands left to hide
    /// the complete glyph. The end is encoded on the last included row so an
    /// output ending at the bottom of the document needs no fictitious next id.
    #[allow(dead_code)] // Public Stage A contract; projection wiring lands next.
    pub fn finished_output_range(&self, zone_id: u64) -> Option<FinishedOutputRange> {
        let zone = self.zone_by_id(zone_id)?;
        if zone.rows_evicted {
            return None;
        }
        let provenance = self.finished_output_provenance.get(&zone_id)?;
        let total_rows = self.scrollback.len().checked_add(self.grid.rows())?;
        let start_absolute = zone.output_start?;
        let end_absolute = zone.output_end?;
        if start_absolute >= end_absolute
            || end_absolute > total_rows
            || end_absolute - start_absolute != provenance.row_ids.len()
        {
            return None;
        }
        for (offset, expected) in provenance.row_ids.iter().copied().enumerate() {
            if self.retained_raw_row(start_absolute + offset)?.0 != expected {
                return None;
            }
        }
        let start_width = self.retained_raw_row(start_absolute)?.1;
        let end_width = self.retained_raw_row(end_absolute - 1)?.1;
        if provenance.range.start.col > start_width || provenance.range.end.col > end_width {
            return None;
        }
        Some(provenance.range)
    }

    /// Remove every finalized OSC 133 block while leaving the live prompt (or
    /// the command currently running) intact. This is the terminal-state half
    /// of Warp's "Clear Blocks" action.
    ///
    /// Sending form-feed or ED 2 to the PTY is deliberately not involved: a
    /// foreground program may own stdin, and ED 2 archives the visible screen
    /// in this emulator. Instead, discard only buffer rows older than the live
    /// OSC 133 lifecycle, blank completed rows that still occupy the grid, and
    /// rebase the lifecycle's absolute row anchors by the removed scrollback.
    /// The zone id counter remains monotonic so a stale UI id can never target
    /// a block created after the clear.
    ///
    /// Everything removed is stashed in [`Self::cleared_blocks`] so
    /// [`Self::undo_clear_completed_blocks`] can rebuild it.
    pub fn clear_completed_blocks(&mut self) -> usize {
        let cleared = self.command_zones.len();
        if cleared == 0 {
            return 0;
        }

        let old_scrollback_len = self.scrollback.len();
        let live_start = match self.current_zone_state {
            ZoneState::PromptStarted(prompt_start)
            | ZoneState::CommandStarted(prompt_start, _)
            | ZoneState::OutputStarted(prompt_start, _, _) => Some(prompt_start),
            ZoneState::Idle => None,
        };

        // Retain the live lifecycle's tail if it has itself scrolled above the
        // grid. With no live lifecycle, every displayed row belongs to history.
        let retained_scrollback_start = live_start
            .map(|start| start.min(old_scrollback_len))
            .unwrap_or(old_scrollback_len);
        let completed_grid_rows = live_start
            .map(|start| start.saturating_sub(old_scrollback_len))
            .unwrap_or(self.grid.rows())
            .min(self.grid.rows());

        // Stash everything the clear removes. The stash is single-level: only
        // a clear that actually removed blocks replaces it, so a reflexive
        // second Clear Blocks cannot destroy the snapshot (anvil's rule).
        let stashed_placements: Vec<KittyPlacement> = self
            .kitty_graphics
            .get_placements()
            .iter()
            .filter(|placement| live_start.is_none_or(|start| placement.buffer_row < start))
            .cloned()
            .collect();
        let mut stashed_image_ids: Vec<u32> = stashed_placements
            .iter()
            .map(|placement| placement.image_id)
            .collect();
        stashed_image_ids.sort_unstable();
        stashed_image_ids.dedup();
        let stashed_images: Vec<(u32, KittyImage)> = stashed_image_ids
            .into_iter()
            .filter_map(|id| {
                self.kitty_graphics
                    .get_image(id)
                    .map(|image| (id, image.clone()))
            })
            .collect();
        // Blanked grid rows keep their original row identities in the stash so
        // restored output provenance still validates; the blank rows left on
        // the grid get fresh identities below, since a dead row's id must not
        // alias when the row is reused (the scroll/erase rule).
        let stashed_grid_rows: Vec<ScrollbackLine> = (0..completed_grid_rows)
            .map(|row| {
                ScrollbackLine::compress_with_raw_row_id(
                    &self.grid[row],
                    self.grid.row_wrapped[row],
                    self.grid.row_ids[row],
                )
            })
            .collect();
        self.cleared_blocks = Some(ClearedBlocksSnapshot {
            scrollback: self.scrollback.drain(..retained_scrollback_start).collect(),
            grid_rows: stashed_grid_rows,
            zones: std::mem::take(&mut self.command_zones),
            provenance: std::mem::take(&mut self.finished_output_provenance),
            captured_output_bytes: std::mem::take(&mut self.captured_output_bytes),
            placements: stashed_placements,
            images: stashed_images,
        });

        self.kitty_graphics
            .retain_placements_from_buffer_row(live_start);
        self.on_scrollback_rows_trimmed(retained_scrollback_start);

        if completed_grid_rows > 0 {
            let blank = TerminalCell::default();
            for row in 0..completed_grid_rows {
                self.grid[row].fill(blank);
                self.grid.row_wrapped[row] = false;
                self.grid.row_ids[row] = RawRowId::fresh();
            }
            self.mark_rows_dirty(0, completed_grid_rows - 1);
        } else {
            // Scrollback-only deletion still changes the visible buffer.
            self.grid_version = self.grid_version.wrapping_add(1);
            self.visible_cells_cache = None;
        }

        self.clear_text_selection();
        self.scroll_offset = 0;
        self.last_archived_screen_snapshot.clear();
        self.last_synced_primary_screen_snapshot.clear();
        cleared
    }

    /// Rebuild the blocks removed by the most recent
    /// [`Self::clear_completed_blocks`]. They are older than anything produced
    /// since, so their rows are prepended to scrollback and their zones
    /// re-enter ahead of the current ones under their original absolute
    /// anchors; zones created after the clear and the live lifecycle shift up
    /// by the reinserted row count. Returns how many zones were restored.
    ///
    /// Single-level: the stash is consumed on the way out. While an
    /// alt-screen app owns the viewport the stash is kept instead, so undo
    /// still works after it exits (anvil's rule).
    pub fn undo_clear_completed_blocks(&mut self) -> usize {
        if self.use_alt_buffer {
            return 0;
        }
        let Some(snapshot) = self.cleared_blocks.take() else {
            return 0;
        };
        let restored_rows = snapshot.scrollback.len() + snapshot.grid_rows.len();
        if restored_rows > 0 {
            self.on_scrollback_rows_inserted(restored_rows);
            for line in snapshot.grid_rows.into_iter().rev() {
                self.scrollback.push_front(line);
            }
            for line in snapshot.scrollback.into_iter().rev() {
                self.scrollback.push_front(line);
            }
        }
        self.kitty_graphics.restore_cleared_placements(
            restored_rows,
            snapshot.placements,
            snapshot.images,
        );

        self.captured_output_bytes = self
            .captured_output_bytes
            .saturating_add(snapshot.captured_output_bytes);
        self.finished_output_provenance.extend(snapshot.provenance);
        let mut restored = snapshot.zones.len();
        for zone in snapshot.zones.into_iter().rev() {
            self.command_zones.push_front(zone);
        }

        // The combined history can exceed the buffer's own caps. Evict from
        // the oldest (the restored prefix) first, mirroring anvil's retention
        // plan on restore with the push path's cap bookkeeping.
        while self.command_zones.len() > MAX_COMMAND_ZONES {
            if let Some(evicted) = self.command_zones.pop_front() {
                self.finished_output_provenance.remove(&evicted.id);
                self.captured_output_bytes = self.captured_output_bytes.saturating_sub(
                    evicted
                        .captured_output
                        .as_ref()
                        .map_or(0, |(text, _)| text.len()),
                );
                restored = restored.saturating_sub(1);
            }
        }
        while self.scrollback.len() > self.max_scrollback {
            self.scrollback.pop_front();
            self.on_scrollback_rows_trimmed(1);
        }
        self.enforce_captured_output_budget(Self::MAX_CAPTURED_OUTPUT_BYTES);

        self.bump_history_revision();
        self.grid_version = self.grid_version.wrapping_add(1);
        self.visible_cells_cache = None;
        restored
    }

    /// Plain text of one zone's output (same trimming and 1 MiB cap as
    /// [`Self::last_command_output_text`]). `None` when the zone is gone,
    /// recorded no output range, or the output is blank.
    pub fn zone_output_text(&self, id: u64) -> Option<String> {
        self.zone_output_text_capped(id).map(|(text, _)| text)
    }

    /// [`Self::zone_output_text`] plus a truncation flag: `true` when the
    /// 1 MiB cap cut whole output rows off the end (the Markdown export
    /// reports it as `- Note: output truncated`). The extraction never cuts
    /// mid-row, so the flag is exact: it is set only when rows were skipped.
    ///
    /// The snapshot captured at finalization wins over live extraction when
    /// both exist (ember's captured-first rule — one consistent answer, and
    /// it survives scrollback trimming). Live extraction only serves command
    /// zones whose snapshot was evicted by the byte budget while their rows
    /// are still present. A commandless background block never falls back to
    /// the grid because its isolated raw-byte reconstruction is authoritative
    /// (cursor repainting can leave different stale cells in the live grid).
    pub fn zone_output_text_capped(&self, id: u64) -> Option<(String, bool)> {
        let zone = self.zone_by_id(id)?;
        if let Some((text, truncated)) = &zone.captured_output {
            return Some((text.clone(), *truncated));
        }
        if zone.command.is_none() && zone.captured_output_evicted {
            return None;
        }
        let start = zone.output_start?;
        let end = zone.output_end.unwrap_or(start);
        let (out, capped) = self.rows_text_from_column(
            start,
            end,
            zone.output_start_col,
            Self::ZONE_OUTPUT_CAP_BYTES,
        );
        if out.trim().is_empty() {
            None
        } else {
            Some((out, capped))
        }
    }

    /// Export-oriented form of [`Self::zone_output_text_capped`]. `None`
    /// means the zone id itself is no longer retained. [`ZoneOutputExport::Empty`]
    /// means the retained zone never had non-blank output; `Unavailable`
    /// means a non-blank snapshot was budget-evicted and live rows can no
    /// longer supply it.
    pub(crate) fn zone_output_export_capped(&self, id: u64) -> Option<ZoneOutputExport> {
        let zone = self.zone_by_id(id)?;
        match self.zone_output_text_capped(id) {
            Some((text, truncated)) => Some(ZoneOutputExport::Available { text, truncated }),
            None if zone.captured_output_evicted => Some(ZoneOutputExport::Unavailable),
            None => Some(ZoneOutputExport::Empty),
        }
    }

    /// Absolute buffer row where the requested 1-based logical output line
    /// begins. Soft-wrapped physical rows stay in the same logical line; only
    /// a physical row without the wrapped flag advances the line number.
    ///
    /// This is intentionally a live-row lookup, not a snapshot lookup: a zone
    /// whose rows were evicted still has searchable/copyable captured text but
    /// no buffer position that can safely be revealed.
    pub fn zone_output_line_row(&self, id: u64, line_no: usize) -> Option<usize> {
        if line_no == 0 {
            return None;
        }
        let zone = self.zone_by_id(id)?;
        if zone.rows_evicted {
            return None;
        }
        let start = zone.output_start?;
        let total_rows = self.scrollback.len().saturating_add(self.grid.rows());
        let end = zone.output_end?.min(total_rows);
        if start >= end {
            return None;
        }
        if line_no == 1 {
            return Some(start);
        }

        let mut logical_line = 1usize;
        for row in start..end.saturating_sub(1) {
            let wrapped = if row < self.scrollback.len() {
                self.scrollback[row].is_wrapped
            } else {
                self.grid
                    .row_wrapped
                    .get(row - self.scrollback.len())
                    .copied()
                    .unwrap_or(false)
            };
            if !wrapped {
                logical_line += 1;
                if logical_line == line_no {
                    return Some(row + 1);
                }
            }
        }
        None
    }

    /// Absolute physical buffer row containing the start of a cached search
    /// match within one logical output line. `line_no` is 1-based;
    /// `match_start..match_end` is a 0-based, end-exclusive Unicode-scalar
    /// range in the original logical line returned by output extraction.
    ///
    /// The walk deliberately mirrors [`Self::rows_text_from_column`]: the
    /// first output row begins at its recorded column, wide-continuation cells
    /// do not become characters, soft-wrapped rows concatenate, and only hard
    /// row endings trim trailing spaces. The complete range is validated even
    /// though only its start row is returned. Thus a captured snapshot which
    /// outlived trimming or no longer agrees with live rows fails closed with
    /// `None` instead of scrolling to an unrelated physical row.
    pub fn zone_output_match_row(
        &self,
        id: u64,
        line_no: usize,
        match_start: usize,
        match_end: usize,
    ) -> Option<usize> {
        if line_no == 0 || match_start >= match_end {
            return None;
        }
        let (output_start, output_start_col, output_end) = {
            let zone = self.zone_by_id(id)?;
            if zone.rows_evicted {
                return None;
            }
            (zone.output_start?, zone.output_start_col, zone.output_end?)
        };
        let total_rows = self.scrollback.len().saturating_add(self.grid.rows());
        let end = output_end.min(total_rows);
        let line_start = self.zone_output_line_row(id, line_no)?;
        if line_start >= end {
            return None;
        }

        let segment_len = |cells: &[TerminalCell], first_col: usize, wrapped: bool| {
            let mut chars = 0usize;
            let mut through_last_non_space = 0usize;
            for cell in cells.iter().skip(first_col.min(cells.len())) {
                if cell.flags.wide_continuation() {
                    continue;
                }
                chars += 1;
                if cell.character != ' ' {
                    through_last_non_space = chars;
                }
            }
            if wrapped {
                chars
            } else {
                through_last_non_space
            }
        };

        let mut remaining_start = match_start;
        let mut remaining_end = match_end;
        let mut target_row = None;
        for row in line_start..end {
            let first_col = if row == output_start {
                output_start_col
            } else {
                0
            };
            let (wrapped, chars) = if row < self.scrollback.len() {
                let line = &self.scrollback[row];
                let cells = line.decompress();
                (
                    line.is_wrapped,
                    segment_len(&cells, first_col, line.is_wrapped),
                )
            } else {
                let grid_row = row - self.scrollback.len();
                let wrapped = *self.grid.row_wrapped.get(grid_row)?;
                let cells = self.grid.cells.get(
                    grid_row.saturating_mul(self.grid.row_len())
                        ..grid_row
                            .saturating_add(1)
                            .saturating_mul(self.grid.row_len()),
                )?;
                (wrapped, segment_len(cells, first_col, wrapped))
            };

            if target_row.is_none() && remaining_start < chars {
                target_row = Some(row);
            }
            if remaining_end <= chars {
                return target_row;
            }
            if !wrapped {
                return None;
            }
            remaining_start = remaining_start.saturating_sub(chars);
            remaining_end = remaining_end.saturating_sub(chars);
        }
        None
    }

    /// The OLDEST completed zone that failed (exit reported and nonzero) —
    /// "jump to first failed" starts at the earliest failure still in scope.
    pub fn first_failed_zone(&self) -> Option<&CommandZone> {
        self.command_zones.iter().find(|zone| {
            matches!(
                crate::block_mode::classify(zone.command.as_deref(), zone.exit_code),
                crate::block_mode::BlockOutcome::Failed(_)
            )
        })
    }

    /// Absolute prompt row of a command currently executing (`C` seen, `D`
    /// still pending); its block stripe runs from here to the buffer bottom.
    pub fn running_zone_start(&self) -> Option<usize> {
        match self.current_zone_state {
            ZoneState::OutputStarted(prompt_start, _, _) => Some(prompt_start),
            _ => None,
        }
    }

    /// Absolute prompt row of a live prompt still being edited (OSC 133 `A`
    /// or `B` seen, no `C` yet). Gets a block separator but no stripe.
    pub fn live_prompt_row(&self) -> Option<usize> {
        match self.current_zone_state {
            ZoneState::PromptStarted(prompt_start) | ZoneState::CommandStarted(prompt_start, _) => {
                Some(prompt_start)
            }
            _ => None,
        }
    }

    /// Furthest absolute row still owned by the active primary-screen
    /// lifecycle. Cursor motion may move back above text already painted by a
    /// running command (for example CUP-based progress UIs), so application
    /// mouse routing must retain the greater of the cursor and recorded write
    /// extent instead of shrinking with the cursor.
    pub fn active_app_extent_row(&self) -> Option<usize> {
        let lifecycle_start = match self.current_zone_state {
            ZoneState::PromptStarted(prompt_start)
            | ZoneState::CommandStarted(prompt_start, _)
            | ZoneState::OutputStarted(prompt_start, _, _) => prompt_start,
            ZoneState::Idle => return None,
        };
        let cursor = self.scrollback.len().saturating_add(self.cursor_row);
        let written = match self.current_zone_state {
            ZoneState::CommandStarted(_, _) => self.current_command_extent_row,
            ZoneState::OutputStarted(_, _, _) => self.current_output_extent_row,
            ZoneState::PromptStarted(_) | ZoneState::Idle => None,
        };
        let last_row = self
            .scrollback
            .len()
            .saturating_add(self.grid.rows())
            .saturating_sub(1);
        Some(
            written
                .unwrap_or(cursor)
                .max(cursor)
                .max(lifecycle_start)
                .min(last_row),
        )
    }

    /// Whether Block Mode has any retained row partition to distinguish
    /// static history from the active application surface. Without usable OSC
    /// 133 evidence, mouse routing falls back to the ordinary full grid.
    pub fn has_usable_block_partitions(&self) -> bool {
        !matches!(self.current_zone_state, ZoneState::Idle)
            || self.command_zones.iter().any(|zone| !zone.rows_evicted)
    }

    /// Capture one finished OSC 133 command for the AI agent queue. The
    /// command line spans `cmd_start..cmd_end`; `output` gives the output row
    /// range when the shell reported one.
    fn record_completed_command(
        &mut self,
        cmd_start: usize,
        cmd_end: usize,
        output: Option<(usize, usize, usize)>,
        metadata: CompletedCommandMetadata,
    ) {
        const MAX_COMMAND_BYTES: usize = 16 * 1024;
        const MAX_OUTPUT_BYTES: usize = 256 * 1024;
        let command_capture_truncated = self.current_command_truncated;
        let command = self.current_command_text.take().unwrap_or_else(|| {
            self.rows_text(cmd_start, cmd_end, MAX_COMMAND_BYTES)
                .0
                .trim()
                .to_string()
        });
        // CompletedCommand has no command-truncation bit and feeds Agent,
        // persistent history, and notifications as executable-looking text.
        // Keep the bounded prefix on the CommandZone, but never publish it to
        // those consumers as if it were the complete command.
        if command.is_empty() || command_capture_truncated {
            return;
        }
        let output_available = output.is_some();
        let (output, truncated) = output
            .map(|(start, end, start_col)| {
                self.rows_text_from_column(start, end, start_col, MAX_OUTPUT_BYTES)
            })
            .unwrap_or_default();
        if self.pending_completed_commands.len() >= MAX_PENDING_COMPLETED_COMMANDS {
            self.pending_completed_commands.pop_front();
        }
        let total_bytes = output.len();
        self.pending_completed_commands.push_back(CompletedCommand {
            command,
            exit_code: metadata.exit_code,
            output,
            id: metadata.execution_id,
            agent_generation: metadata.agent_generation,
            output_available,
            truncated,
            total_bytes,
            duration_ms: metadata.duration_ms,
            completion_provenance: metadata.completion_provenance,
        });
    }

    fn queue_agent_termination(
        &mut self,
        command: String,
        generation: u64,
        execution_id: Option<String>,
    ) {
        if self.pending_completed_commands.len() >= MAX_PENDING_COMPLETED_COMMANDS {
            self.pending_completed_commands.pop_front();
        }
        self.pending_completed_commands.push_back(CompletedCommand {
            command,
            exit_code: None,
            output: String::new(),
            id: execution_id.clone(),
            agent_generation: Some(generation),
            output_available: false,
            truncated: false,
            total_bytes: 0,
            duration_ms: None,
            completion_provenance: crate::block_mode::CompletionProvenance::BoundaryInferred,
        });
        self.remember_consumed_execution_id(execution_id.as_deref());
    }

    fn record_abandoned_armed_agent_command(&mut self) {
        let Some(armed) = self.armed_agent_execution.take() else {
            return;
        };
        let execution_id = self.current_command_id.clone();
        self.queue_agent_termination(armed.command, armed.generation, execution_id);
    }

    /// Publish a lifecycle termination for a locally authorized Agent command
    /// before RIS clears terminal state. Prefer an execution correlated at C;
    /// otherwise seal an approval still armed at the prompt. Neither path
    /// supplies an invented exit status or output.
    fn record_reset_interrupted_agent_command(&mut self) {
        if let Some(active) = self.active_agent_execution.take() {
            let execution_id = active
                .execution_id
                .or_else(|| self.current_command_id.clone());
            self.queue_agent_termination(
                self.current_command_text.clone().unwrap_or_default(),
                active.generation,
                execution_id,
            );
        } else {
            self.record_abandoned_armed_agent_command();
        }
    }

    /// Drain commands finished since the last call (for the AI agent panel).
    pub fn take_completed_commands(&mut self) -> Vec<CompletedCommand> {
        self.pending_completed_commands.drain(..).collect()
    }

    /// True while an OSC 133 execution is in flight (`C` seen, `D` not yet) —
    /// the bottom bar's "running" state.
    pub fn is_command_running(&self) -> bool {
        self.current_command_started_at.is_some()
    }

    /// Elapsed wall time for the live OSC 133 command. This is renderer-only
    /// state: the completed zone still prefers a shell-reported duration at
    /// `D`, while the live block badge uses this local monotonic clock.
    pub fn running_duration_ms(&self) -> Option<u64> {
        self.current_command_started_at
            .map(|started| u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX))
    }

    /// Plain text of the absolute buffer rows `start..end` (scrollback plus
    /// live grid), soft-wrapped rows joined, per-line trailing padding
    /// trimmed, and the total capped at `max_bytes`. The second return is
    /// `true` when the cap dropped remaining rows (rows are never cut
    /// mid-segment, so text is only ever lost whole rows at a time).
    fn rows_text(&self, start: usize, end: usize, max_bytes: usize) -> (String, bool) {
        self.rows_text_from_column(start, end, 0, max_bytes)
    }

    /// [`Self::rows_text`] with an exact first-row column. Background output
    /// may begin beside a still-visible prompt, so starting at column zero
    /// would leak prompt furniture into copy/search/export.
    fn rows_text_from_column(
        &self,
        start: usize,
        end: usize,
        start_col: usize,
        max_bytes: usize,
    ) -> (String, bool) {
        let scrollback_len = self.scrollback.len();
        let end = end.min(scrollback_len + self.grid.rows());
        if end <= start {
            return (String::new(), false);
        }
        let mut out = String::new();
        let mut capped = false;
        for abs_row in start..end {
            let (mut segment, wrapped) = if abs_row < scrollback_len {
                let line = &self.scrollback[abs_row];
                let cells = line.decompress();
                let text: String = cells
                    .iter()
                    .skip(if abs_row == start {
                        start_col.min(cells.len())
                    } else {
                        0
                    })
                    .filter(|cell| !cell.flags.wide_continuation())
                    .map(|cell| cell.character)
                    .collect();
                (text, line.is_wrapped)
            } else {
                let grid_row = abs_row - scrollback_len;
                if grid_row >= self.grid.rows() {
                    break;
                }
                let first_col = if abs_row == start {
                    start_col.min(self.grid.row_len())
                } else {
                    0
                };
                let text: String = (first_col..self.grid.row_len())
                    .map(|col| self.grid.get(grid_row, col))
                    .filter(|cell| !cell.flags.wide_continuation())
                    .map(|cell| cell.character)
                    .collect();
                (text, self.grid.row_wrapped[grid_row])
            };
            if !wrapped {
                // Hard line end: trailing cell padding is not real output.
                segment.truncate(segment.trim_end_matches(' ').len());
            }
            if out.len().saturating_add(segment.len()) > max_bytes {
                capped = true;
                break;
            }
            out.push_str(&segment);
            if !wrapped && abs_row + 1 < end {
                if out.len() == max_bytes {
                    capped = true;
                    break;
                }
                out.push('\n');
            }
        }
        (out, capped)
    }

    /// Scroll so the closest prompt above the current view lands at the top
    /// of the viewport. Returns true when the viewport moved.
    pub fn jump_to_prev_prompt(&mut self) -> bool {
        let start = self.viewport_absolute_start();
        let Some(target) = self.prompt_rows().filter(|&row| row < start).max() else {
            return false;
        };
        let before = self.scroll_offset;
        self.set_scroll_offset(self.scrollback.len().saturating_sub(target));
        self.scroll_offset != before
    }

    /// Scroll so the closest prompt below the current view lands at the top
    /// of the viewport; past the last prompt this returns to the live view.
    /// Returns true when the viewport moved.
    pub fn jump_to_next_prompt(&mut self) -> bool {
        if self.scroll_offset == 0 {
            return false;
        }
        let start = self.viewport_absolute_start();
        let before = self.scroll_offset;
        match self.prompt_rows().filter(|&row| row > start).min() {
            Some(target) if target < self.scrollback.len() => {
                self.set_scroll_offset(self.scrollback.len() - target);
            }
            _ => self.scroll_to_bottom(),
        }
        self.scroll_offset != before
    }

    fn handle_osc_52(&mut self, value: &str) {
        // OSC 52 format: <selection>;<base64-data>
        // selection: c=clipboard, p=primary, s=select (we treat all as clipboard)
        // data: ? means query, base64 means set
        //
        // Cap on payload size: a remote process should not be able to push
        // arbitrary multi-MB blobs into the host clipboard. xterm uses 100 KB
        // by default; we match that.
        const OSC52_MAX_BYTES: usize = 100 * 1024;
        if let Some((_sel, data)) = value.split_once(';') {
            if data == "?" {
                // Query: signal main loop to read clipboard and respond
                self.pending_osc52_clipboard_query = true;
            } else if !data.is_empty() {
                if data.len() > OSC52_MAX_BYTES.saturating_mul(4) / 3 + 8 {
                    // Reject before even attempting to decode.
                    crate::debug_log!(
                        "[OSC52] rejecting clipboard set: encoded {} bytes exceeds limit",
                        data.len()
                    );
                    return;
                }
                // Set: decode base64 and store for main loop to apply
                if let Some(decoded) = Self::decode_base64(data) {
                    if decoded.len() <= OSC52_MAX_BYTES {
                        self.pending_osc52_clipboard_set = Some(decoded);
                    } else {
                        crate::debug_log!(
                            "[OSC52] rejecting clipboard set: decoded {} bytes exceeds {}",
                            decoded.len(),
                            OSC52_MAX_BYTES
                        );
                    }
                }
            }
        }
    }

    fn handle_osc_5522(&mut self, metadata: &str, _payload: Option<&str>) {
        crate::debug_log!("[OSC5522] metadata={} payload={:?}", metadata, _payload);

        let mut message_type = None;
        let mut mime = None;
        let mut password = None;

        for part in metadata.split(':') {
            if let Some(value) = part.strip_prefix("type=") {
                message_type = Some(value);
            } else if let Some(value) = part.strip_prefix("mime=") {
                mime = Self::decode_base64(value);
            } else if let Some(value) = part.strip_prefix("password=") {
                password = Self::decode_base64(value);
            } else if let Some(value) = part.strip_prefix("pw=") {
                password = Self::decode_base64(value);
            }
        }

        if message_type != Some("read") {
            return;
        }

        let kind = if let Some(mime_type) = mime {
            if let Some(expected) = &self.pending_paste_password {
                if password.as_deref() != Some(expected.as_str()) {
                    self.append_osc_5522_status("type=read:status=EPERM", None);
                    return;
                }
            }
            self.pending_paste_password = None;
            ClipboardReadKind::MimeData(mime_type)
        } else {
            ClipboardReadKind::MimeList
        };

        // A PTY can emit thousands of tiny requests in one read batch. Keep the
        // amount of UI work / async clipboard tasks strictly bounded.
        const MAX_PENDING_CLIPBOARD_REQUESTS: usize = 8;
        if self.pending_clipboard_requests.len() < MAX_PENDING_CLIPBOARD_REQUESTS {
            self.pending_clipboard_requests
                .push(ClipboardReadRequest { kind });
        }
    }

    fn set_keyboard_enhancement_flags(&mut self, flags: u16, mode: u16) {
        match mode {
            1 => self.keyboard_enhancement_flags = flags,
            2 => self.keyboard_enhancement_flags |= flags,
            3 => self.keyboard_enhancement_flags &= !flags,
            _ => {}
        }
    }

    fn push_keyboard_enhancement_flags(&mut self, flags: u16) {
        if self.keyboard_enhancement_stack.len() >= 32 {
            self.keyboard_enhancement_stack.remove(0);
        }
        self.keyboard_enhancement_stack
            .push(self.keyboard_enhancement_flags);
        self.keyboard_enhancement_flags = flags;
    }

    fn pop_keyboard_enhancement_flags(&mut self, count: usize) {
        for _ in 0..count.max(1) {
            match self.keyboard_enhancement_stack.pop() {
                Some(flags) => self.keyboard_enhancement_flags = flags,
                None => {
                    self.keyboard_enhancement_flags = 0;
                    break;
                }
            }
        }
    }

    fn decrqm_private_mode_state(&self, mode: u16) -> u8 {
        match mode {
            1
            | 6
            | 7
            | 25
            | 47
            | 66
            | 1000..=1006
            | 1015
            | 1047..=1049
            | 2004
            | 2026
            | 2031
            | 5522 => {
                if self.modes.contains(&mode) {
                    1
                } else {
                    2
                }
            }
            _ => 0,
        }
    }

    fn report_private_mode_status(&mut self, mode: u16) {
        let state = self.decrqm_private_mode_state(mode);
        let response = format!("\x1b[?{};{}$y", mode, state);
        self.output_buffer.extend_from_slice(response.as_bytes());
    }

    /// The cwd the child last reported through OSC 7, if any.
    pub fn current_working_dir(&self) -> Option<&str> {
        self.current_working_dir.as_deref()
    }

    /// Decode an OSC 7 payload into a local filesystem path.
    ///
    /// Accepts `file://host/%-encoded-path` or a bare absolute path. A non-local
    /// hostname is rejected, and that check is not optional: this value drives
    /// the file-tree sidebar and the cwd a split inherits, so without it a shell
    /// on the far side of ssh could point the sidebar at any local directory it
    /// named — and the session snapshot would restore the next launch there.
    /// Ported from ember `src/terminal/state.rs::decode_osc7_cwd`.
    fn decode_osc7_cwd(value: &str) -> Option<String> {
        let path_part = if let Some(rest) = value.strip_prefix("file://") {
            let slash = rest.find('/')?;
            if !Self::osc7_host_is_local(&rest[..slash]) {
                return None;
            }
            &rest[slash..]
        } else if value.starts_with('/') {
            value
        } else {
            // A relative path has no anchor here — the shell's idea of "here" is
            // exactly what this sequence was supposed to tell us.
            return None;
        };

        // Percent-decode by hand: the alphabet is three characters wide and a
        // url crate is not worth linking for it.
        let bytes = path_part.as_bytes();
        let mut out = Vec::with_capacity(bytes.len());
        let mut i = 0;
        while i < bytes.len() {
            if bytes[i] == b'%' && i + 2 < bytes.len() {
                let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).ok()?;
                out.push(u8::from_str_radix(hex, 16).ok()?);
                i += 3;
            } else {
                out.push(bytes[i]);
                i += 1;
            }
        }
        let decoded = String::from_utf8(out).ok()?;
        // A decoded path with an interior NUL cannot be opened and would be
        // truncated by any C API it reached, so reject it rather than store it.
        if decoded.is_empty() || decoded.contains('\0') {
            return None;
        }
        Some(decoded)
    }

    fn osc7_host_is_local(host: &str) -> bool {
        if host.is_empty() || host.eq_ignore_ascii_case("localhost") {
            return true;
        }
        let local_hostname = std::env::var("HOSTNAME").ok().or_else(|| {
            std::fs::read_to_string("/etc/hostname")
                .ok()
                .map(|hostname| hostname.trim().to_string())
        });
        local_hostname.is_some_and(|local| host.eq_ignore_ascii_case(&local))
    }

    fn sanitized_title(title: &str) -> String {
        title
            .chars()
            // Titles are rendered in trusted app chrome. Drop line/layout
            // controls and bidi overrides/isolation marks so PTY output cannot
            // create multiline tabs or visually reorder the window title.
            .filter(|&ch| {
                !ch.is_control()
                    && !matches!(
                        ch,
                        '\u{061c}'
                            | '\u{200e}'
                            | '\u{200f}'
                            | '\u{202a}'..='\u{202e}'
                            | '\u{2066}'..='\u{2069}'
                    )
            })
            .take(MAX_TERMINAL_TITLE_CHARS)
            .collect()
    }

    fn save_titles(&mut self, target: u16) {
        if self.title_stack.len() >= 16 {
            self.title_stack.remove(0);
        }
        match target {
            1 => self.title_stack.push((Some(self.icon_title.clone()), None)),
            2 => self
                .title_stack
                .push((None, Some(self.window_title.clone()))),
            _ => self.title_stack.push((
                Some(self.icon_title.clone()),
                Some(self.window_title.clone()),
            )),
        }
    }

    fn restore_titles(&mut self, target: u16) {
        let Some((icon_title, window_title)) = self.title_stack.pop() else {
            return;
        };
        match target {
            1 => {
                if let Some(icon_title) = icon_title {
                    self.icon_title = icon_title;
                }
            }
            2 => {
                if let Some(window_title) = window_title {
                    self.window_title = window_title;
                }
            }
            _ => {
                if let Some(icon_title) = icon_title {
                    self.icon_title = icon_title;
                }
                if let Some(window_title) = window_title {
                    self.window_title = window_title;
                }
            }
        }
    }

    fn handle_window_ops(&mut self, params: &[u16]) {
        let op = params.first().copied().unwrap_or(0);
        let (cols, rows) = self.get_dimensions();
        match op {
            // Report window state: normal/non-iconified.
            11 => self.output_buffer.extend_from_slice(b"\x1b[1t"),
            // Report window position. frost does not track compositor position,
            // so report a stable origin like VTE-compatible terminals commonly do
            // when the window manager will not expose coordinates.
            13 => self.output_buffer.extend_from_slice(b"\x1b[3;0;0t"),
            // Report text area size in pixels.
            14 => {
                let response = format!(
                    "\x1b[4;{};{}t",
                    self.viewport_pixel_height, self.viewport_pixel_width
                );
                self.output_buffer.extend_from_slice(response.as_bytes());
            }
            // Report text area size in characters.
            18 => {
                let response = format!("\x1b[8;{};{}t", rows, cols);
                self.output_buffer.extend_from_slice(response.as_bytes());
            }
            // Report screen size in characters. We do not know monitor geometry
            // here, so mirror the current terminal grid instead of lying wildly.
            19 => {
                let response = format!("\x1b[9;{};{}t", rows, cols);
                self.output_buffer.extend_from_slice(response.as_bytes());
            }
            // Report icon label / window title.
            20 => {
                let response = format!("\x1b]L{}\x1b\\", Self::sanitized_title(&self.icon_title));
                self.output_buffer.extend_from_slice(response.as_bytes());
            }
            21 => {
                let response = format!("\x1b]l{}\x1b\\", Self::sanitized_title(&self.window_title));
                self.output_buffer.extend_from_slice(response.as_bytes());
            }
            // Save/restore icon/window title. Parameter 0 means both; 1 icon;
            // 2 window, matching xterm/VTE title stack behavior closely enough
            // for shells that temporarily annotate the title.
            22 => self.save_titles(params.get(1).copied().unwrap_or(0)),
            23 => self.restore_titles(params.get(1).copied().unwrap_or(0)),
            _ => {}
        }
    }

    /// Advance to the start of the next line, honoring the DECSTBM scroll region.
    /// When the cursor sits on the region's bottom row this scrolls the region up
    /// (pushing to scrollback only for a full-screen region); otherwise it just
    /// moves down. Used by autowrap and linefeed so both stay region-aware.
    fn wrap_to_next_line(&mut self) {
        self.grid.row_wrapped[self.cursor_row] = true;
        self.cursor_col = 0;
        self.index();
    }

    /// IND / LF: move down one row, scrolling the active region at the bottom.
    fn index(&mut self) {
        if self.cursor_row == self.scroll_region_bottom {
            self.scroll_region_up(self.scroll_region_top, self.scroll_region_bottom);
        } else if self.cursor_row + 1 < self.grid.rows() {
            self.cursor_row += 1;
        }
    }

    /// NEL: carriage return plus IND.
    fn next_line(&mut self) {
        self.cursor_col = 0;
        self.index();
    }

    /// RI: move up one row, scrolling the active region down at the top.
    fn reverse_index(&mut self) {
        if self.cursor_row == self.scroll_region_top {
            self.scroll_region_down(self.scroll_region_top, self.scroll_region_bottom);
        } else if self.cursor_row > 0 {
            self.cursor_row -= 1;
        }
    }

    /// IRM (insert mode): shift cells at/after `col` right by `count`, dropping the
    /// rightmost `count` cells off the end of the row.
    fn shift_cells_right(&mut self, row: usize, col: usize, count: usize) {
        let cols = self.grid.row_len();
        if count == 0 || col >= cols {
            return;
        }
        let blank = self.create_blank_cell();
        let line = &mut self.grid[row];
        if col + count < cols {
            line.copy_within(col..cols - count, col + count);
        }
        for cell in &mut line[col..(col + count).min(cols)] {
            *cell = blank;
        }
        self.mark_row_dirty(row);
    }

    /// Merge a zero-width combining mark into the preceding base cell when the
    /// pair has a single precomposed form (NFC); otherwise drop it.
    fn combine_with_previous(&mut self, mark: char) {
        if self.cursor_col == 0 && !self.pending_wrap {
            return;
        }
        let mut base_col = if self.pending_wrap {
            self.cursor_col
        } else {
            self.cursor_col - 1
        };
        if base_col > 0
            && self
                .grid
                .get(self.cursor_row, base_col)
                .flags
                .wide_continuation()
        {
            base_col -= 1;
        }
        let cell = self.grid.get_mut(self.cursor_row, base_col);
        let mut combined = String::with_capacity(8);
        combined.push(cell.character);
        combined.push(mark);
        let nfc: String = combined.nfc().collect();
        let mut chars = nfc.chars();
        if let (Some(c0), None) = (chars.next(), chars.next()) {
            cell.character = c0;
            self.mark_row_dirty(self.cursor_row);
        }
    }

    fn default_tab_stops(cols: usize) -> Vec<bool> {
        (0..cols).map(|c| c % 8 == 0 && c != 0).collect()
    }

    /// Next tab stop strictly right of `col`, or the last column if none.
    fn next_tab_stop(&self, col: usize) -> usize {
        let cols = self.grid.row_len();
        let mut c = col + 1;
        while c < cols {
            if self.tab_stops.get(c).copied().unwrap_or(false) {
                return c;
            }
            c += 1;
        }
        cols.saturating_sub(1)
    }

    /// Previous tab stop strictly left of `col`, or column 0 if none.
    fn prev_tab_stop(&self, col: usize) -> usize {
        let mut c = col;
        while c > 0 {
            c -= 1;
            if self.tab_stops.get(c).copied().unwrap_or(false) {
                return c;
            }
        }
        0
    }

    fn save_cursor(&mut self) {
        self.saved_cursor = Some(SavedCursor {
            row: self.cursor_row,
            col: self.cursor_col,
            fg: self.current_fg,
            bg: self.current_bg,
            flags: self.current_flags,
            g0: self.g0_charset,
            g1: self.g1_charset,
            active: self.active_charset,
            origin_mode: self.modes.contains(&6),
            pending_wrap: self.pending_wrap,
        });
    }

    fn snapshot_cursor_state(&self) -> SavedCursor {
        SavedCursor {
            row: self.cursor_row,
            col: self.cursor_col,
            fg: self.current_fg,
            bg: self.current_bg,
            flags: self.current_flags,
            g0: self.g0_charset,
            g1: self.g1_charset,
            active: self.active_charset,
            origin_mode: self.modes.contains(&6),
            pending_wrap: self.pending_wrap,
        }
    }

    fn restore_cursor(&mut self) {
        if let Some(s) = self.saved_cursor {
            self.cursor_row = s.row.min(self.grid.rows().saturating_sub(1));
            self.cursor_col = s.col.min(self.grid.row_len().saturating_sub(1));
            self.current_fg = s.fg;
            self.current_bg = s.bg;
            self.current_flags = s.flags;
            self.g0_charset = s.g0;
            self.g1_charset = s.g1;
            self.active_charset = s.active;
            if s.origin_mode {
                self.modes.insert(6);
            } else {
                self.modes.remove(&6);
            }
            self.pending_wrap = s.pending_wrap;
        } else {
            self.cursor_row = 0;
            self.cursor_col = 0;
            self.pending_wrap = false;
        }
    }

    /// Place the cursor for CUP/HVP (CSI H / f). Honors DECOM origin mode: when set,
    /// the row is relative to the scroll region and clamped within it.
    fn place_cursor(&mut self, row_1based: usize, col_1based: usize) {
        self.pending_wrap = false;
        let row0 = row_1based.saturating_sub(1);
        let col0 = col_1based.saturating_sub(1);
        if self.modes.contains(&6) {
            self.cursor_row = (self.scroll_region_top + row0).min(self.scroll_region_bottom);
        } else {
            self.cursor_row = row0.min(self.grid.rows().saturating_sub(1));
        }
        self.cursor_col = col0.min(self.grid.row_len().saturating_sub(1));
    }

    /// VPA (CSI d): move to an absolute row, honoring origin mode, keeping the column.
    fn set_cursor_row_abs(&mut self, row_1based: usize) {
        self.pending_wrap = false;
        let row0 = row_1based.saturating_sub(1);
        if self.modes.contains(&6) {
            self.cursor_row = (self.scroll_region_top + row0).min(self.scroll_region_bottom);
        } else {
            self.cursor_row = row0.min(self.grid.rows().saturating_sub(1));
        }
    }

    fn put_char(&mut self, ch: char, track_idle_output: bool) {
        let ch = self.translate_char(ch);
        let width = crate::char_width::cached_char_width(ch);
        if width == 0 {
            self.combine_with_previous(ch);
            return;
        }

        let cols = self.grid.row_len();
        let blank_cell = self.create_blank_cell();
        let autowrap = self.modes.contains(&7);

        if self.pending_wrap {
            self.pending_wrap = false;
            if autowrap {
                self.wrap_to_next_line();
            }
        }

        // If character doesn't fit at end of line, handle based on autowrap mode
        if self.cursor_col + width > cols {
            // Only wrap to next line if autowrap mode (mode 7) is enabled
            if autowrap {
                self.wrap_to_next_line();
            } else {
                // Autowrap disabled: clamp cursor to last column instead of wrapping
                self.cursor_col = cols.saturating_sub(width);
            }
        }

        if track_idle_output {
            self.note_idle_background_cells(
                self.cursor_col,
                self.cursor_col.saturating_add(width),
                !ch.is_whitespace(),
            );
        }
        let write_col = self.cursor_col;

        // IRM insert mode (mode 4): make room by shifting the row right.
        if self.modes.contains(&4) {
            self.shift_cells_right(self.cursor_row, self.cursor_col, width);
        }

        // If current position has a continuation cell to its left, clear the wide character
        if self.cursor_col > 0
            && self
                .grid
                .get(self.cursor_row, self.cursor_col)
                .flags
                .wide_continuation()
        {
            *self.grid.get_mut(self.cursor_row, self.cursor_col - 1) = blank_cell;
        }

        // If current position has a wide character, clear its continuation cell
        if self.grid.get(self.cursor_row, self.cursor_col).flags.wide()
            && self.cursor_col + 1 < cols
        {
            *self.grid.get_mut(self.cursor_row, self.cursor_col + 1) = blank_cell;
        }

        // Write character
        let cell = self.grid.get_mut(self.cursor_row, self.cursor_col);
        cell.character = ch;
        cell.foreground = self.current_fg;
        cell.background = self.current_bg;
        cell.flags = self.current_flags;
        cell.flags.set_wide(width == 2);
        cell.flags.set_wide_continuation(false);
        cell.hyperlink = self.current_hyperlink.unwrap_or(0);

        // Set up wide character continuation cell if needed
        if width == 2 && self.cursor_col + 1 < cols {
            let cont_cell = self.grid.get_mut(self.cursor_row, self.cursor_col + 1);
            *cont_cell = blank_cell;
            cont_cell.flags.set_wide_continuation(true);
            cont_cell.hyperlink = self.current_hyperlink.unwrap_or(0);
        }

        self.cursor_col += width;
        self.last_printed_char = Some(ch);
        if self.cursor_col >= cols {
            self.cursor_col = cols.saturating_sub(width);
            if autowrap {
                self.pending_wrap = true;
            }
        }
        // Mark the row as dirty after writing character
        self.mark_row_dirty(self.cursor_row);
        self.mark_command_echo_extent(write_col, write_col.saturating_add(width));
    }

    fn put_ascii_run(&mut self, bytes: &[u8]) {
        let cols = self.grid.row_len();
        let autowrap = self.modes.contains(&7);
        let mut pos = 0;
        if let Some(&last) = bytes.last() {
            self.last_printed_char = Some(last as char);
        }

        while pos < bytes.len() {
            if self.pending_wrap {
                self.pending_wrap = false;
                if autowrap {
                    self.wrap_to_next_line();
                }
            }

            let remaining = cols - self.cursor_col;
            let chunk_len = (bytes.len() - pos).min(remaining);

            // Write chunk to grid directly through a single row slice
            // (avoids recomputing row*cols + bounds-check on every cell)
            let fg = self.current_fg;
            let bg = self.current_bg;
            let mut flags = self.current_flags;
            flags.set_wide(false);
            flags.set_wide_continuation(false);
            let hyperlink = self.current_hyperlink.unwrap_or(0);
            let col = self.cursor_col;
            let end = col + chunk_len;
            self.note_idle_background_cells(
                col,
                end,
                bytes[pos..pos + chunk_len]
                    .iter()
                    .any(|byte| !byte.is_ascii_whitespace()),
            );
            let blank = self.create_blank_cell();
            // Preserve the wide-cell pair invariant at both edges of the bulk
            // overwrite. The interior is fully replaced below, but a CJK body or
            // continuation just outside the slice must not be left orphaned.
            if col > 0
                && self
                    .grid
                    .get(self.cursor_row, col)
                    .flags
                    .wide_continuation()
            {
                *self.grid.get_mut(self.cursor_row, col - 1) = blank;
            }
            if end < cols && self.grid.get(self.cursor_row, end - 1).flags.wide() {
                *self.grid.get_mut(self.cursor_row, end) = blank;
            }
            let row = &mut self.grid[self.cursor_row][col..col + chunk_len];
            for (cell, &byte) in row.iter_mut().zip(&bytes[pos..pos + chunk_len]) {
                cell.character = byte as char;
                cell.foreground = fg;
                cell.background = bg;
                cell.flags = flags;
                cell.hyperlink = hyperlink;
            }

            self.cursor_col += chunk_len;
            pos += chunk_len;

            self.mark_row_dirty(self.cursor_row);
            self.mark_command_echo_extent(col, end);

            if self.cursor_col >= cols {
                self.cursor_col = cols - 1;
                if autowrap {
                    self.pending_wrap = true;
                }
            }
        }
    }

    fn create_blank_cell(&self) -> TerminalCell {
        self.blank_cell_with_bg(self.current_bg)
    }

    fn blank_cell_with_bg(&self, bg: Color) -> TerminalCell {
        TerminalCell {
            character: ' ',
            foreground: Color::Default,
            background: bg,
            flags: StyleFlags::default(),
            hyperlink: 0,
        }
    }

    fn blank_line(&self, cols: usize) -> Vec<TerminalCell> {
        vec![self.create_blank_cell(); cols]
    }

    fn normalize_line_width(&self, mut line: Vec<TerminalCell>, cols: usize) -> Vec<TerminalCell> {
        match line.len().cmp(&cols) {
            std::cmp::Ordering::Equal => line,
            std::cmp::Ordering::Greater => {
                line.truncate(cols);
                line
            }
            std::cmp::Ordering::Less => {
                line.resize(cols, self.create_blank_cell());
                line
            }
        }
    }

    fn line_is_blank(&self, row: usize) -> bool {
        let blank = self.create_blank_cell();
        self.grid[row].iter().all(|cell| {
            cell.character == blank.character
                && cell.foreground == blank.foreground
                && cell.background == blank.background
                && cell.flags == blank.flags
                && cell.hyperlink == 0
        })
    }

    fn archive_visible_screen_to_scrollback(&mut self) {
        self.archive_visible_screen_to_scrollback_with_options(false, false);
    }

    fn visible_screen_snapshot(&self) -> Option<Vec<String>> {
        if self.grid.rows() == 0 {
            return None;
        }

        let first = (0..self.grid.rows()).find(|&row| !self.line_is_blank(row));
        let last = (0..self.grid.rows()).rfind(|&row| !self.line_is_blank(row));
        let (Some(first), Some(last)) = (first, last) else {
            return None;
        };

        Some(
            (first..=last)
                .map(|row| self.grid[row].iter().map(|cell| cell.character).collect())
                .collect(),
        )
    }

    fn archive_primary_screen_unless_last_synced_snapshot(&mut self) {
        let Some(snapshot) = self.visible_screen_snapshot() else {
            return;
        };

        if snapshot == self.last_synced_primary_screen_snapshot {
            return;
        }

        self.archive_visible_screen_to_scrollback();
    }

    fn archive_visible_screen_to_scrollback_with_options(
        &mut self,
        allow_alt_buffer: bool,
        dedupe_snapshot: bool,
    ) -> usize {
        if (self.use_alt_buffer && !allow_alt_buffer) || self.grid.rows() == 0 {
            return 0;
        }

        let first = (0..self.grid.rows()).find(|&row| !self.line_is_blank(row));
        let last = (0..self.grid.rows()).rfind(|&row| !self.line_is_blank(row));
        let (Some(first), Some(last)) = (first, last) else {
            return 0;
        };

        if dedupe_snapshot {
            let snapshot = self.visible_screen_snapshot().unwrap_or_default();
            if snapshot == self.last_archived_screen_snapshot {
                return 0;
            }
            self.last_archived_screen_snapshot = snapshot;
        }

        let before = self.scrollback_pushes;
        for row in first..=last {
            let line = ScrollbackLine::compress(&self.grid[row], self.grid.row_wrapped[row]);
            self.push_scrollback_compressed_with_options(line, allow_alt_buffer);
        }
        self.scrollback_pushes.wrapping_sub(before) as usize
    }

    /// One synchronized frame of a full-screen app, kept scrollable.
    ///
    /// The alternate screen is transient: a repaint is the *same* page redrawn,
    /// not new history. Recording every frame let an animated TUI (a spinner, an
    /// elapsed timer) append a whole screen 20+ times a second, which buried and
    /// then evicted the real history behind thousands of near-identical copies.
    /// So the alternate screen contributes at most one *provisional* snapshot,
    /// superseded by each following frame, and promoted to permanent history
    /// only when the app erases that screen. Content that genuinely scrolls off
    /// the top still reaches scrollback through the scroll-region path, so a TUI
    /// whose transcript scrolls stays fully scrollable.
    fn archive_alt_screen_frame(&mut self) {
        // A reader holding a scrolled-back viewport owns the history: never
        // rewrite it underneath them for a frame that is still on screen. The
        // snapshot stays provisional so returning to the bottom can still
        // supersede it — unless the app scrolled real rows in behind it, which
        // `retire_provisional_alt_snapshot` detects and refuses.
        if self.scroll_offset > 0 {
            return;
        }
        let Some(snapshot) = self.visible_screen_snapshot() else {
            return;
        };
        if snapshot == self.last_archived_screen_snapshot {
            return;
        }
        let old_grid_base = self.scrollback.len();
        self.retire_provisional_alt_snapshot();
        let appended = self.archive_visible_screen_to_scrollback_with_options(true, true);
        self.provisional_alt_snapshot =
            (appended > 0).then_some((appended, self.scrollback_pushes));
        self.rebase_raw_selection_for_grid_base_change(old_grid_base, self.scrollback.len());
    }

    /// Synchronized alternate-screen frames are copied into (and superseded
    /// within) scrollback without moving the live grid. Raw selection rows are
    /// absolute `scrollback + grid` coordinates, so a changing snapshot height
    /// must move live-grid anchors with the grid base. Codex does this on every
    /// repaint.
    fn rebase_raw_selection_for_grid_base_change(&mut self, old_base: usize, new_base: usize) {
        let Some(selection) = self.selection.as_mut() else {
            return;
        };
        for point in [&mut selection.anchor, &mut selection.active] {
            if point.0 < old_base {
                continue;
            }
            point.0 = if new_base >= old_base {
                point.0.saturating_add(new_base - old_base)
            } else {
                point.0.saturating_sub(old_base - new_base)
            };
        }
    }

    /// Drop the superseded snapshot. Exactly undoes the append that created it,
    /// which is sound only while those rows are still the scrollback tail — any
    /// row the app scrolled off since then is real history sitting behind them.
    fn retire_provisional_alt_snapshot(&mut self) {
        let Some((rows, pushes)) = self.provisional_alt_snapshot.take() else {
            return;
        };
        if pushes != self.scrollback_pushes || self.scroll_offset > 0 {
            return;
        }
        let rows = rows.min(self.scrollback.len());
        if rows == 0 {
            return;
        }
        self.scrollback.truncate(self.scrollback.len() - rows);
        self.bump_history_revision();
    }

    fn push_scrollback_compressed(&mut self, line: ScrollbackLine) {
        self.push_scrollback_compressed_with_options(line, false);
    }

    fn push_scrollback_compressed_with_options(
        &mut self,
        line: ScrollbackLine,
        allow_alt_buffer: bool,
    ) {
        if self.use_alt_buffer && !allow_alt_buffer {
            return;
        }
        if self.scrollback.len() >= self.max_scrollback {
            self.scrollback.pop_front();
            self.on_scrollback_rows_trimmed(1);
        }
        self.scrollback.push_back(line);
        self.scrollback_pushes = self.scrollback_pushes.wrapping_add(1);
        self.bump_history_revision();
        // Pin the viewport when the user is reading history: without this,
        // start_idx = scrollback.len() - scroll_offset - rows drifts by +1
        // for every new line, sliding the visible region toward the bottom.
        if self.scroll_offset > 0 {
            self.scroll_offset = (self.scroll_offset + 1).min(self.scrollback.len());
            self.visible_cells_cache = None;
        }
    }

    fn scroll_region_down(&mut self, top: usize, bottom: usize) {
        if top >= self.grid.rows() || bottom >= self.grid.rows() || top > bottom {
            return;
        }
        let cols = self.grid.row_len();
        // Shift rows down: move [top..bottom) to [top+1..=bottom]
        let src_start = top * cols;
        let src_end = bottom * cols;
        let dst = (top + 1) * cols;
        let blank = self.create_blank_cell();
        self.grid.cells.copy_within(src_start..src_end, dst);
        // Clear top row
        self.grid.cells[src_start..src_start + cols].fill(blank);
        self.grid.row_wrapped.copy_within(top..bottom, top + 1);
        self.grid.row_wrapped[top] = false;
        self.grid.row_ids.copy_within(top..bottom, top + 1);
        self.grid.row_ids[top] = RawRowId::fresh();
        self.grid.bump_identity_revision();
        self.mark_rows_dirty(top, bottom);
    }

    fn scroll_region_up(&mut self, top: usize, bottom: usize) {
        if top >= self.grid.rows() || bottom >= self.grid.rows() || top > bottom {
            return;
        }

        let cols = self.grid.row_len();
        // VTE saves lines scrolled off the top margin into scrollback whenever
        // the scrolling region starts at the first screen row. The bottom margin
        // may be above the last row so TUIs can keep prompts/status lines fixed
        // while the history area scrolls.
        let scrolls_off_screen_top = top == 0;

        // Compress the removed line directly from the grid slice before mutating,
        // avoiding a per-line Vec allocation from get_row.
        let allow_alt_scrollback = self.use_alt_buffer && self.sync_output_active;
        let scrollback_line =
            if scrolls_off_screen_top && (!self.use_alt_buffer || allow_alt_scrollback) {
                Some(ScrollbackLine::compress_with_raw_row_id(
                    &self.grid[top],
                    self.grid.row_wrapped[top],
                    self.grid.row_ids[top],
                ))
            } else {
                None
            };

        let src_start = (top + 1) * cols;
        let src_end = (bottom + 1) * cols;
        let dst_start = top * cols;
        let blank = self.create_blank_cell();
        self.grid.cells.copy_within(src_start..src_end, dst_start);
        let blank_start = bottom * cols;
        self.grid.cells[blank_start..blank_start + cols].fill(blank);
        self.grid.row_wrapped.copy_within(top + 1..=bottom, top);
        self.grid.row_wrapped[bottom] = false;
        self.grid.row_ids.copy_within(top + 1..bottom + 1, top);
        self.grid.row_ids[bottom] = RawRowId::fresh();
        self.grid.bump_identity_revision();

        self.mark_rows_dirty(top, bottom);

        if let Some(line) = scrollback_line {
            self.push_scrollback_compressed_with_options(line, allow_alt_scrollback);
        }
    }

    fn charset_from_designator(byte: u8) -> Charset {
        match byte {
            b'0' => Charset::DecSpecialGraphics,
            _ => Charset::Ascii,
        }
    }

    fn translate_char(&self, ch: char) -> char {
        match self.active_charset {
            Charset::Ascii => ch,
            Charset::DecSpecialGraphics => match ch {
                '`' => '◆',
                'a' => '▒',
                'f' => '°',
                'g' => '±',
                'j' => '┘',
                'k' => '┐',
                'l' => '┌',
                'm' => '└',
                'n' => '┼',
                'o' => '⎺',
                'p' => '⎻',
                'q' => '─',
                'r' => '⎼',
                's' => '⎽',
                't' => '├',
                'u' => '┤',
                'v' => '┴',
                'w' => '┬',
                'x' => '│',
                'y' => '≤',
                'z' => '≥',
                '{' => 'π',
                '|' => '≠',
                '}' => '£',
                '~' => '·',
                _ => ch,
            },
        }
    }

    fn clear_cell(&mut self, row: usize, col: usize) {
        let cols = self.grid.row_len();
        let bg_color = self.current_bg;
        let blank_cell = TerminalCell {
            character: ' ',
            foreground: Color::Default,
            background: bg_color,
            flags: StyleFlags::default(),
            hyperlink: 0,
        };
        // If clearing a continuation cell, also clear the wide character body
        if self.grid.get(row, col).flags.wide_continuation() && col > 0 {
            *self.grid.get_mut(row, col - 1) = blank_cell;
        }
        // If clearing a wide character body, also clear the continuation cell
        if self.grid.get(row, col).flags.wide() && col + 1 < cols {
            *self.grid.get_mut(row, col + 1) = blank_cell;
        }
        *self.grid.get_mut(row, col) = blank_cell;
    }

    /// P3 优化：批量处理输入数据，只在处理完成后触发一次网格版本更新
    /// 相比多次 process_input，这个方法避免了多次网格版本递增
    pub fn process_batch(&mut self, input: &[u8]) {
        // Resize, an alternate-screen swap and the projection restore all park
        // the viewport at the bottom without going through the scroll helpers.
        self.settle_pending_saved_line_purge();
        self.grid_version = self.grid_version.wrapping_add(1);
        self.process_input(input);
    }

    #[inline]
    fn mark_row_dirty(&mut self, row: usize) {
        if row < self.row_versions.len() {
            self.row_versions[row] = self.grid_version;
        }
    }

    #[inline]
    fn mark_rows_dirty(&mut self, start: usize, end: usize) {
        for row in start..=end.min(self.row_versions.len().saturating_sub(1)) {
            self.row_versions[row] = self.grid_version;
        }
    }

    pub fn take_osc52_clipboard_set(&mut self) -> Option<String> {
        self.pending_osc52_clipboard_set.take()
    }

    pub fn take_osc52_clipboard_query(&mut self) -> bool {
        let q = self.pending_osc52_clipboard_query;
        self.pending_osc52_clipboard_query = false;
        q
    }

    pub fn respond_osc52_clipboard(&mut self, content: &str) {
        use base64::Engine;
        let encoded = base64::engine::general_purpose::STANDARD.encode(content.as_bytes());
        self.output_buffer.extend_from_slice(b"\x1b]52;c;");
        self.output_buffer.extend_from_slice(encoded.as_bytes());
        self.output_buffer.extend_from_slice(Self::osc_terminator());
    }

    /// Feed one APC-G payload to the graphics state and write back whatever the
    /// protocol responder produced. Replies go into `output_buffer`, the same
    /// path OSC color and OSC 52 clipboard queries answer through, so clients
    /// that address a command with `i=` (kitten icat) no longer wait for a
    /// timeout.
    fn handle_kitty_graphics_apc(&mut self, payload: &[u8]) {
        let cursor_col = self.cursor_col as u32;
        let cursor_row = self.cursor_row as u32;
        let buffer_row = self.scrollback.len().saturating_add(self.cursor_row);
        if let Err(_error) = self
            .kitty_graphics
            .parse_graphics_payload_at_buffer_row(payload, cursor_col, cursor_row, buffer_row)
        {
            crate::debug_log!("[APC] Kitty graphics error: {}", _error);
        }
        let responses = self.kitty_graphics.take_responses();
        if !responses.is_empty() {
            self.output_buffer.extend_from_slice(&responses);
        }
    }

    /// Begin buffering an unterminated APC (`ESC _` through the end of the
    /// current read). A first fragment that already exceeds the cap is
    /// rejected without being retained; the parser then discards through ST.
    fn begin_pending_apc(&mut self, tail: &[u8]) {
        if tail.len() > MAX_PENDING_ESCAPE {
            if let Some(payload) = tail.strip_prefix(b"\x1b_") {
                if jterm_core::kitty_graphics::is_graphics_payload(payload) {
                    self.kitty_graphics.reject_graphics_payload(
                        payload,
                        "Kitty graphics APC exceeded the parser size limit",
                    );
                    let responses = self.kitty_graphics.take_responses();
                    if !responses.is_empty() {
                        self.output_buffer.extend_from_slice(&responses);
                    }
                }
            }
            self.pending_apc.clear();
            self.pending_apc_scan_from = 0;
            self.discarding_oversized_apc = true;
            self.discarding_apc_prev_escape = tail.last() == Some(&0x1b);
            return;
        }
        self.pending_apc.clear();
        self.pending_apc.extend_from_slice(tail);
        self.pending_apc_scan_from = self.pending_apc.len().saturating_sub(1);
    }

    /// Answer a buffered kitty APC that is being abandoned: echo a bounded
    /// control prefix (identifier + quiet level) in the rejection so an
    /// addressed client sees EINVAL instead of a timeout. `suffix` carries
    /// the fragment that tripped the limit, in case fragmentation split the
    /// control section unusually early.
    fn reject_buffered_kitty_apc_with_suffix(&mut self, suffix: &[u8], error: &str) {
        let Some(payload) = self.pending_apc.strip_prefix(b"\x1b_") else {
            return;
        };
        if !jterm_core::kitty_graphics::is_graphics_payload(payload) {
            return;
        }

        let limit = jterm_core::kitty_graphics::MAX_CONTROL_BYTES;
        let mut recovery =
            Vec::with_capacity(limit.min(payload.len().saturating_add(suffix.len())));
        recovery.extend_from_slice(&payload[..payload.len().min(limit)]);
        if recovery.len() < limit && !recovery.contains(&b';') {
            recovery.extend_from_slice(&suffix[..suffix.len().min(limit - recovery.len())]);
        }
        self.kitty_graphics
            .reject_graphics_payload(&recovery, error);
        let responses = self.kitty_graphics.take_responses();
        if !responses.is_empty() {
            self.output_buffer.extend_from_slice(&responses);
        }
    }

    /// Resume a fragmented kitty APC. Returns true when this function consumed
    /// the input (including any bytes after the ST, processed recursively).
    fn resume_pending_apc(&mut self, input: &[u8]) -> bool {
        if self.discarding_oversized_apc {
            let mut previous_escape = self.discarding_apc_prev_escape;
            for (index, byte) in input.iter().copied().enumerate() {
                if previous_escape && byte == b'\\' {
                    self.discarding_oversized_apc = false;
                    self.discarding_apc_prev_escape = false;
                    if index + 1 < input.len() {
                        self.process_input(&input[index + 1..]);
                    }
                    return true;
                }
                previous_escape = byte == 0x1b;
            }
            self.discarding_apc_prev_escape = previous_escape;
            return true;
        }

        if self.pending_apc.is_empty() {
            return false;
        }

        // Everything before pending_apc_scan_from was proved not to contain
        // ST in the previous call. A new terminator can therefore only
        // straddle the old/new boundary or live entirely in input. Search the
        // new bytes once before doing the capacity check: bytes after ST are
        // normal terminal input and must not be charged to the APC size limit.
        let scan_from = self
            .pending_apc_scan_from
            .min(self.pending_apc.len().saturating_sub(1));
        let terminator = if scan_from + 1 == self.pending_apc.len()
            && self.pending_apc[scan_from] == 0x1b
            && input.first() == Some(&b'\\')
        {
            Some((scan_from, 1))
        } else {
            input
                .windows(2)
                .position(|window| window == b"\x1b\\")
                .map(|offset| (self.pending_apc.len() + offset, offset + 2))
        };

        if let Some((terminator, consumed)) = terminator {
            let packet_len = self.pending_apc.len().saturating_add(consumed);
            if packet_len > MAX_PENDING_ESCAPE {
                self.reject_buffered_kitty_apc_with_suffix(
                    &input[..consumed],
                    "Kitty graphics APC exceeded the parser size limit",
                );
                self.pending_apc.clear();
                self.pending_apc_scan_from = 0;
            } else {
                self.pending_apc.extend_from_slice(&input[..consumed]);
                let packet = std::mem::take(&mut self.pending_apc);
                self.pending_apc_scan_from = 0;
                if packet.starts_with(b"\x1b_")
                    && jterm_core::kitty_graphics::is_graphics_payload(&packet[2..terminator])
                {
                    self.handle_kitty_graphics_apc(&packet[2..terminator]);
                }
            }
            if consumed < input.len() {
                self.process_input(&input[consumed..]);
            }
            return true;
        }

        if self.pending_apc.len().saturating_add(input.len()) > MAX_PENDING_ESCAPE {
            let previous_escape = input
                .last()
                .copied()
                .or_else(|| self.pending_apc.last().copied())
                == Some(0x1b);
            self.reject_buffered_kitty_apc_with_suffix(
                input,
                "Kitty graphics APC exceeded the parser size limit",
            );
            self.pending_apc.clear();
            self.pending_apc_scan_from = 0;
            self.discarding_oversized_apc = true;
            self.discarding_apc_prev_escape = previous_escape;
            return true;
        }

        self.pending_apc.extend_from_slice(input);
        self.pending_apc_scan_from = self.pending_apc.len().saturating_sub(1);
        true
    }

    /// Begin buffering an unterminated OSC (`ESC ]` through the end of the
    /// current read). Like the old `pending_escape` stash there is no size
    /// check here: a prefix that grows past the cap is abandoned by the
    /// entry check in `resume_pending_osc` on the next read.
    fn begin_pending_osc(&mut self, tail: &[u8]) {
        self.pending_osc.clear();
        self.pending_osc.extend_from_slice(tail);
        self.pending_osc_scan_from = self.pending_osc.len().saturating_sub(1);
    }

    /// Resume a fragmented OSC (`ESC ]` … BEL/ST). Returns true when this
    /// function consumed the input (including any bytes after the
    /// terminator, processed recursively).
    fn resume_pending_osc(&mut self, input: &[u8]) -> bool {
        if self.pending_osc.is_empty() {
            return false;
        }

        // Mirror the old `pending_escape` entry check exactly: a buffered
        // prefix past the cap is abandoned wholesale and this read is parsed
        // as ordinary input (payload tail prints, BEL is a plain bell).
        if self.pending_osc.len() > MAX_PENDING_ESCAPE {
            self.pending_osc.clear();
            self.pending_osc_scan_from = 0;
            return false;
        }

        // Everything before pending_osc_scan_from was proved not to contain
        // BEL or ST in the previous call. A terminator can therefore only
        // straddle the old/new boundary (ESC at the stashed tail, `\` here)
        // or live entirely in input. Scan the new bytes once, before any
        // capacity concern: like the merged re-parse, a packet whose
        // terminator shows up is dispatched regardless of its size.
        let scan_from = self
            .pending_osc_scan_from
            .min(self.pending_osc.len().saturating_sub(1));
        let straddled_st = scan_from + 1 == self.pending_osc.len()
            && self.pending_osc[scan_from] == 0x1b
            && input.first() == Some(&b'\\');
        let terminator = if straddled_st {
            Some((scan_from, 1))
        } else {
            let bel = input.iter().position(|&byte| byte == 0x07);
            let st = input.windows(2).position(|window| window == b"\x1b\\");
            match (bel, st) {
                (Some(bel), Some(st)) if bel < st => Some((self.pending_osc.len() + bel, bel + 1)),
                (Some(_), Some(st)) => Some((self.pending_osc.len() + st, st + 2)),
                (Some(bel), None) => Some((self.pending_osc.len() + bel, bel + 1)),
                (None, Some(st)) => Some((self.pending_osc.len() + st, st + 2)),
                (None, None) => None,
            }
        };

        if let Some((payload_end, consumed)) = terminator {
            let capture_idle_background = self.idle_background_capture_active();
            self.pending_osc.extend_from_slice(&input[..consumed]);
            let packet = std::mem::take(&mut self.pending_osc);
            self.pending_osc_scan_from = 0;
            self.handle_osc_payload(&packet[2..payload_end]);
            if capture_idle_background && self.idle_background_capture_active() {
                self.append_idle_background_bytes(&packet);
            }
            if consumed < input.len() {
                self.process_input(&input[consumed..]);
            }
            return true;
        }

        self.pending_osc.extend_from_slice(input);
        self.pending_osc_scan_from = self.pending_osc.len().saturating_sub(1);
        true
    }

    /// Begin buffering an unterminated DCS/SOS/PM (`ESC P`/`ESC X`/`ESC ^`
    /// through the end of the current read). Same streaming and overflow
    /// rules as `begin_pending_osc`.
    fn begin_pending_dcs(&mut self, tail: &[u8]) {
        self.pending_dcs.clear();
        self.pending_dcs.extend_from_slice(tail);
        self.pending_dcs_scan_from = self.pending_dcs.len().saturating_sub(1);
    }

    /// Resume a fragmented DCS/SOS/PM. Only ST terminates these strings; the
    /// payload itself is dropped, exactly like the merged re-parse did.
    /// Returns true when this function consumed the input.
    fn resume_pending_dcs(&mut self, input: &[u8]) -> bool {
        if self.pending_dcs.is_empty() {
            return false;
        }

        // Same overflow rule as the OSC path (old `pending_escape` entry
        // check): abandon the prefix wholesale, parse this read fresh.
        if self.pending_dcs.len() > MAX_PENDING_ESCAPE {
            self.pending_dcs.clear();
            self.pending_dcs_scan_from = 0;
            return false;
        }

        // See resume_pending_osc: only the stashed tail ESC can straddle the
        // boundary into an ST; every earlier byte was proved clean.
        let scan_from = self
            .pending_dcs_scan_from
            .min(self.pending_dcs.len().saturating_sub(1));
        let consumed = if scan_from + 1 == self.pending_dcs.len()
            && self.pending_dcs[scan_from] == 0x1b
            && input.first() == Some(&b'\\')
        {
            Some(1)
        } else {
            input
                .windows(2)
                .position(|window| window == b"\x1b\\")
                .map(|offset| offset + 2)
        };

        if let Some(consumed) = consumed {
            let capture_idle_background = self.idle_background_capture_active();
            self.pending_dcs.extend_from_slice(&input[..consumed]);
            let packet = std::mem::take(&mut self.pending_dcs);
            self.pending_dcs_scan_from = 0;
            if capture_idle_background && self.idle_background_capture_active() {
                self.append_idle_background_bytes(&packet);
            }
            if consumed < input.len() {
                self.process_input(&input[consumed..]);
            }
            return true;
        }

        self.pending_dcs.extend_from_slice(input);
        self.pending_dcs_scan_from = self.pending_dcs.len().saturating_sub(1);
        true
    }

    /// Check if sync output timed out (>1s) and auto-clear if so
    pub fn check_sync_output_timeout(&mut self) {
        if self.sync_output_active {
            if let Some(start) = self.sync_output_start {
                if start.elapsed() > std::time::Duration::from_secs(1) {
                    if self.use_alt_buffer {
                        self.archive_alt_screen_frame();
                    } else {
                        self.last_synced_primary_screen_snapshot =
                            self.visible_screen_snapshot().unwrap_or_default();
                    }
                    self.sync_output_active = false;
                    self.sync_output_start = None;
                    self.modes.remove(&2026);
                    self.mark_rows_dirty(0, self.grid.rows().saturating_sub(1));
                }
            }
        }
    }

    #[allow(dead_code)]
    pub fn is_focus_event_mode(&self) -> bool {
        self.modes.contains(&1004)
    }

    #[allow(dead_code)]
    pub fn is_bracketed_paste_mode(&self) -> bool {
        self.modes.contains(&2004)
    }

    #[allow(dead_code)]
    pub fn emit_focus_in(&mut self) {
        if self.modes.contains(&1004) {
            self.output_buffer.extend_from_slice(b"\x1b[I");
        }
    }

    #[allow(dead_code)]
    pub fn emit_focus_out(&mut self) {
        if self.modes.contains(&1004) {
            self.output_buffer.extend_from_slice(b"\x1b[O");
        }
    }

    /// Consume a UTF-8 lead byte together with its continuation bytes.
    ///
    /// - Not enough bytes left in this read: stash the lead byte and wait for
    ///   the next batch.
    /// - Continuation bytes all present: decode and print the character; if
    ///   the sequence is well-formed in shape but invalid in content
    ///   (overlong encoding, surrogate), print U+FFFD instead.
    /// - A non-continuation byte inside the expected span: the sequence is
    ///   malformed, so print U+FFFD and consume only the lead byte, leaving
    ///   the offending byte to be processed normally.
    fn consume_utf8_lead(&mut self, byte: u8, expected: u8, data: &[u8], i: &mut usize) {
        let need = expected as usize;
        if *i + need > data.len() {
            // Incomplete: stash the lead byte and wait for the next batch.
            self.utf8_buf[0] = byte;
            self.utf8_len = 1;
            self.utf8_expected = expected;
            *i += 1;
            return;
        }

        let all_continuation = (1..need).all(|k| (data[*i + k] & 0xC0) == 0x80);
        if all_continuation {
            match std::str::from_utf8(&data[*i..*i + need]) {
                Ok(s) => {
                    if let Some(ch) = s.chars().next() {
                        self.put_char(ch, true);
                    }
                }
                Err(_) => self.put_char('\u{FFFD}', true),
            }
            *i += need;
        } else {
            // Lead byte directly followed by a non-continuation byte.
            self.put_char('\u{FFFD}', true);
            *i += 1;
        }
    }

    pub fn process_input(&mut self, input: &[u8]) {
        // A fragmented kitty APC streams against its own bounded buffer; this
        // must run before the pending_escape merge so the APC never pays the
        // O(n^2) re-scan and its tail is never parsed as ordinary input.
        if self.resume_pending_apc(input) {
            return;
        }
        // Fragmented OSC and DCS/SOS/PM strings stream the same way: at most
        // one string state can be in flight, so whichever buffer is non-empty
        // owns the input until its terminator (or the overflow abandon).
        if self.resume_pending_osc(input) {
            return;
        }
        if self.resume_pending_dcs(input) {
            return;
        }
        // Guard against an unterminated escape sequence. Such a sequence
        // is buffered into `pending_escape` and re-scanned from its start on every
        // read, which is both O(n^2) in CPU and unbounded in memory. Once the
        // buffered prefix exceeds this cap, abandon the partial sequence. The cap
        // is generous enough for legitimate large payloads (e.g. OSC 52 clipboard).
        // Only short CSI/charset prefixes still land in `pending_escape` — the
        // unbounded OSC/DCS/SOS/PM string states stream against their own
        // scan-cursor buffers above.
        if self.pending_escape.len() > MAX_PENDING_ESCAPE {
            self.pending_escape.clear();
        }

        // Fast path: if no pending escape, process input directly without allocation
        let data;
        let data_slice: &[u8] = if self.pending_escape.is_empty() {
            input
        } else {
            // Slow path: merge pending escape with new input
            let mut combined = std::mem::take(&mut self.pending_escape);
            combined.extend_from_slice(input);
            data = combined;
            &data
        };

        let mut i = 0;

        while i < data_slice.len() {
            let token_start = i;
            let capture_idle_background = self.idle_background_capture_active();
            let byte = data_slice[i];

            // A pending multi-byte UTF-8 sequence interrupted by a
            // non-continuation byte is malformed: emit the U+FFFD replacement
            // character per Unicode guidance and reset, then process the
            // current byte normally so the partial sequence is neither
            // silently dropped nor allowed to swallow later bytes.
            if self.utf8_len > 0 && (byte & 0xC0) != 0x80 {
                self.put_char('\u{FFFD}', true);
                self.utf8_len = 0;
            }

            match byte {
                b'\x08' => {
                    // Backspace (0x08) - just move cursor left.
                    // Shell handles actual deletion and sends back updated display.
                    self.pending_wrap = false;
                    if self.cursor_col > 0 {
                        self.cursor_col -= 1;
                    }
                    i += 1;
                }
                b'\x7f' => {
                    // DEL (0x7f) is a fill/padding character; xterm ignores it.
                    i += 1;
                }
                b'\n' => {
                    // Linefeed - move cursor down or scroll the region.
                    self.pending_wrap = false;
                    self.index();
                    i += 1;
                }
                b'\r' => {
                    self.pending_wrap = false;
                    self.cursor_col = 0;
                    i += 1;
                }
                b'\x0e' => {
                    self.active_charset = self.g1_charset;
                    i += 1;
                }
                b'\x0f' => {
                    self.active_charset = self.g0_charset;
                    i += 1;
                }
                b'\x07' => {
                    // Bell - ignore
                    i += 1;
                }
                b'\t' => {
                    // Tab - advance to the next tab stop.
                    self.pending_wrap = false;
                    self.cursor_col = self.next_tab_stop(self.cursor_col);
                    i += 1;
                }
                b'\x1b' => {
                    let esc_start = i;

                    if i + 1 >= data_slice.len() {
                        self.pending_escape
                            .extend_from_slice(&data_slice[esc_start..]);
                        break;
                    }

                    match data_slice[i + 1] {
                        b'7' => {
                            // DECSC - Save cursor (position + SGR + charset + origin)
                            self.save_cursor();
                            i += 2;
                        }
                        b'8' => {
                            // DECRC - Restore cursor
                            self.restore_cursor();
                            i += 2;
                        }
                        b'E' => {
                            // NEL - Next Line (linefeed + carriage return)
                            self.pending_wrap = false;
                            self.next_line();
                            i += 2;
                        }
                        b'D' => {
                            // IND - Index (linefeed without carriage return)
                            self.pending_wrap = false;
                            self.index();
                            i += 2;
                        }
                        b'M' => {
                            // RI - Reverse Index
                            self.pending_wrap = false;
                            self.reverse_index();
                            i += 2;
                        }
                        b'H' => {
                            // HTS - set a horizontal tab stop at the current column
                            if let Some(stop) = self.tab_stops.get_mut(self.cursor_col) {
                                *stop = true;
                            }
                            i += 2;
                        }
                        b'c' => {
                            // RIS - Reset to Initial State
                            self.full_reset();
                            i += 2;
                        }
                        b'#' => {
                            // DEC private: ESC # 8 = DECALN (fill screen with 'E')
                            if i + 2 < data_slice.len() && data_slice[i + 2] == b'8' {
                                self.decaln();
                                i += 3;
                            } else {
                                i += 2;
                            }
                        }
                        b']' => {
                            i += 2;

                            let payload_start = i;

                            let mut terminated = false;
                            while i < data_slice.len() {
                                if data_slice[i] == 0x07 {
                                    i += 1;
                                    terminated = true;
                                    break;
                                } else if i + 1 < data_slice.len()
                                    && data_slice[i] == 0x1b
                                    && data_slice[i + 1] == 0x5c
                                {
                                    i += 2;
                                    terminated = true;
                                    break;
                                } else {
                                    i += 1;
                                }
                            }

                            if !terminated {
                                // Fragmented OSC: stream against its own
                                // bounded buffer instead of re-scanning
                                // pending_escape from byte 0 each read.
                                self.begin_pending_osc(&data_slice[esc_start..]);
                                break;
                            }

                            let payload_end = if data_slice[i - 1] == 0x07 {
                                i - 1
                            } else {
                                i - 2
                            };
                            if payload_end >= payload_start {
                                self.handle_osc_payload(&data_slice[payload_start..payload_end]);
                            }
                        }
                        b'P' | b'X' | b'^' | b'_' => {
                            // DCS (ESC P), SOS (ESC X), PM (ESC ^) and APC (ESC _)
                            // all end at ST, but only APC carries Kitty graphics.
                            // Sniffing every string for `a=` used to hand an
                            // unrelated DCS to the graphics state.
                            let is_apc = data_slice[i + 1] == b'_';
                            i += 2;

                            let mut terminated = false;
                            let dcs_start = i;
                            while i < data_slice.len() {
                                if i + 1 < data_slice.len()
                                    && data_slice[i] == 0x1b
                                    && data_slice[i + 1] == 0x5c
                                {
                                    let payload = &data_slice[dcs_start..i];

                                    if is_apc
                                        && jterm_core::kitty_graphics::is_graphics_payload(payload)
                                    {
                                        self.handle_kitty_graphics_apc(payload);
                                    }

                                    i += 2;
                                    terminated = true;
                                    break;
                                }
                                i += 1;
                            }

                            if !terminated {
                                if is_apc {
                                    // Kitty transfers stream against their
                                    // own bounded buffer instead of the
                                    // re-scanning pending_escape.
                                    self.begin_pending_apc(&data_slice[esc_start..]);
                                } else {
                                    // DCS/SOS/PM stream the same way; their
                                    // payload stays dropped on completion.
                                    self.begin_pending_dcs(&data_slice[esc_start..]);
                                }
                                break;
                            }
                        }
                        b'>' => {
                            // ESC > - DECKPNM (Keypad Numeric Mode)
                            self.modes.remove(&66);
                            i += 2;
                        }
                        b'<' => {
                            // ESC < - DECKPM (Keypad Application Mode) or other private sequence
                            // Just skip it
                            i += 2;
                        }
                        b'=' => {
                            // ESC = - DECKPAM (Keypad Application Mode)
                            self.modes.insert(66);
                            i += 2;
                        }
                        b'(' | b')' => {
                            if i + 2 >= data_slice.len() {
                                self.pending_escape
                                    .extend_from_slice(&data_slice[esc_start..]);
                                break;
                            }

                            // Character set selection: ESC ( X or ESC ) X
                            // data_slice[i] = ESC, data_slice[i+1] = '(' or ')', data_slice[i+2] = designator
                            let is_g0 = data_slice[i + 1] == b'(';
                            let designator = data_slice[i + 2];
                            let charset = Self::charset_from_designator(designator);

                            crate::debug_log!(
                                "[CHARSET] ESC {} designator={} (0x{:02x}) charset={:?}",
                                if is_g0 { '(' } else { ')' },
                                designator as char,
                                designator,
                                charset
                            );

                            if is_g0 {
                                self.g0_charset = charset;
                                self.active_charset = self.g0_charset;
                            } else {
                                self.g1_charset = charset;
                            }

                            i += 3;
                        }
                        b'[' => {
                            i += 2;

                            // Use stack arrays for CSI params (typical CSI sequences are short)
                            let mut param_bytes = [0u8; 128];
                            let mut param_len = 0;
                            let mut intermediates = [0u8; 8];
                            let mut inter_len = 0;
                            let mut final_byte = None;
                            let mut cancelled = false;

                            while i < data_slice.len() {
                                match data_slice[i] {
                                    0x30..=0x3f => {
                                        if param_len < param_bytes.len() {
                                            param_bytes[param_len] = data_slice[i];
                                            param_len += 1;
                                        }
                                    }
                                    0x20..=0x2f => {
                                        if inter_len < intermediates.len() {
                                            intermediates[inter_len] = data_slice[i];
                                            inter_len += 1;
                                        }
                                    }
                                    0x40..=0x7e => {
                                        final_byte = Some(data_slice[i]);
                                        break;
                                    }
                                    // ECMA-48 allows C0 controls inside a control
                                    // sequence. Pagers such as util-linux `more`
                                    // can insert CR/LF while wrapping colored text,
                                    // even between the digits of an SGR parameter.
                                    // Execute those controls immediately and keep
                                    // collecting the surrounding CSI sequence.
                                    b'\x08' => {
                                        self.pending_wrap = false;
                                        self.cursor_col = self.cursor_col.saturating_sub(1);
                                    }
                                    b'\t' => {
                                        self.pending_wrap = false;
                                        self.cursor_col = self.next_tab_stop(self.cursor_col);
                                    }
                                    b'\n' | b'\x0b' | b'\x0c' => {
                                        self.pending_wrap = false;
                                        self.index();
                                    }
                                    b'\r' => {
                                        self.pending_wrap = false;
                                        self.cursor_col = 0;
                                    }
                                    b'\x0e' => self.active_charset = self.g1_charset,
                                    b'\x0f' => self.active_charset = self.g0_charset,
                                    // CAN and SUB cancel the current control
                                    // sequence. Leave a following ESC untouched so
                                    // the outer parser can begin the replacement.
                                    b'\x18' | b'\x1a' => {
                                        cancelled = true;
                                        i += 1;
                                        break;
                                    }
                                    b'\x1b' => {
                                        cancelled = true;
                                        break;
                                    }
                                    // Other C0 controls are either padding or have
                                    // no visible effect in Frost, but they do not
                                    // make the CSI incomplete.
                                    0x00..=0x1f => {}
                                    _ => break,
                                }
                                i += 1;
                            }

                            if cancelled {
                                continue;
                            }

                            let Some(final_byte) = final_byte else {
                                // C0 controls embedded in the partial CSI were
                                // already executed above. Buffer only the CSI
                                // syntax so a later PTY read does not replay a
                                // linefeed or carriage return.
                                self.pending_escape.extend_from_slice(b"\x1b[");
                                self.pending_escape
                                    .extend_from_slice(&param_bytes[..param_len]);
                                self.pending_escape
                                    .extend_from_slice(&intermediates[..inter_len]);
                                break;
                            };

                            let private_prefix = match param_bytes.first().copied() {
                                Some(prefix @ (b'<' | b'=' | b'>' | b'?')) => {
                                    // Shift remaining params left
                                    for j in 0..param_len - 1 {
                                        param_bytes[j] = param_bytes[j + 1];
                                    }
                                    param_len -= 1;
                                    Some(prefix)
                                }
                                _ => None,
                            };
                            let params = Self::parse_csi_params(&param_bytes[..param_len]);
                            let cmd = final_byte as char;

                            self.handle_escape_sequence(
                                &params,
                                &param_bytes[..param_len],
                                cmd,
                                private_prefix,
                                &intermediates[..inter_len],
                            );
                            i += 1;
                        }
                        _ => {
                            // Unknown 2-byte escape (e.g. SS2 `ESC N`, SS3 `ESC O`).
                            // Consume BOTH bytes so the trailing letter isn't printed
                            // as literal text.
                            i += 2;
                        }
                    }
                }
                32..=126 => {
                    // ASCII fast path: scan for run of printable ASCII and process in bulk.
                    // Insert mode (IRM) needs per-cell shifting, so fall back to put_char.
                    if self.utf8_len == 0
                        && self.active_charset == Charset::Ascii
                        && !self.modes.contains(&4)
                    {
                        let run_start = i;
                        i += 1;
                        while i < data_slice.len() {
                            let b = data_slice[i];
                            if !(32..=126).contains(&b) {
                                break;
                            }
                            i += 1;
                        }
                        self.put_ascii_run(&data_slice[run_start..i]);
                    } else {
                        self.put_char(byte as char, true);
                        i += 1;
                    }
                }
                // UTF-8 multi-byte sequences: try to consume all bytes eagerly
                0xC2..=0xDF => {
                    self.consume_utf8_lead(byte, 2, data_slice, &mut i);
                }
                0xE0..=0xEF => {
                    self.consume_utf8_lead(byte, 3, data_slice, &mut i);
                }
                0xF0..=0xF4 => {
                    self.consume_utf8_lead(byte, 4, data_slice, &mut i);
                }
                _ => {
                    // Reaching here, byte is either a continuation byte (the
                    // abandoned-sequence check above guarantees a sequence is
                    // really pending) or an invalid UTF-8 lead byte
                    // (0xC0/0xC1/0xF5..=0xFF), or an unhandled C0 control.
                    if self.utf8_len > 0 && (byte & 0xC0) == 0x80 {
                        self.utf8_buf[self.utf8_len as usize] = byte;
                        self.utf8_len += 1;
                        if self.utf8_len == self.utf8_expected {
                            match std::str::from_utf8(&self.utf8_buf[..self.utf8_len as usize]) {
                                Ok(s) => {
                                    if let Some(ch) = s.chars().next() {
                                        self.put_char(ch, true);
                                    }
                                }
                                // Full length but invalid content (overlong
                                // encoding / surrogate): emit U+FFFD.
                                Err(_) => self.put_char('\u{FFFD}', true),
                            }
                            self.utf8_len = 0;
                        }
                    } else {
                        // Orphan continuation bytes and invalid lead bytes are
                        // malformed UTF-8 and get U+FFFD; unhandled C0
                        // controls stay ignored.
                        if byte >= 0x80 {
                            self.put_char('\u{FFFD}', true);
                        }
                        self.utf8_len = 0;
                    }
                    i += 1;
                }
            }
            if capture_idle_background && self.idle_background_capture_active() && i > token_start {
                self.append_idle_background_bytes(&data_slice[token_start..i]);
            }
        }
    }

    fn handle_escape_sequence(
        &mut self,
        params: &[u16],
        raw_params: &[u8],
        cmd: char,
        private_prefix: Option<u8>,
        intermediates: &[u8],
    ) {
        if matches!(
            cmd,
            'A' | 'B' | 'C' | 'D' | 'E' | 'F' | 'G' | 'H' | 'I' | 'Z' | 'f' | 'd' | '`'
        ) {
            self.pending_wrap = false;
        }

        match cmd {
            'A' => {
                // CUU - cursor up. Stops at the top margin (or row 0 if the
                // cursor starts above the region); never scrolls.
                let n = params.first().copied().unwrap_or(1).max(1) as usize;
                let limit = if self.cursor_row >= self.scroll_region_top {
                    self.scroll_region_top
                } else {
                    0
                };
                self.cursor_row = self.cursor_row.saturating_sub(n).max(limit);
            }
            'B' => {
                // CUD - cursor down. Stops at the bottom margin (or last row if
                // the cursor starts below the region); never scrolls.
                let n = params.first().copied().unwrap_or(1).max(1) as usize;
                let limit = if self.cursor_row <= self.scroll_region_bottom {
                    self.scroll_region_bottom
                } else {
                    self.grid.rows() - 1
                };
                self.cursor_row = (self.cursor_row + n).min(limit);
            }
            'C' => {
                let n = params.first().copied().unwrap_or(1).max(1) as usize;
                self.cursor_col = (self.cursor_col + n).min(self.grid.row_len() - 1);
            }
            'D' => {
                let n = params.first().copied().unwrap_or(1).max(1) as usize;
                self.cursor_col = self.cursor_col.saturating_sub(n);
            }
            'E' => {
                // CNL - cursor next line. Down n, to column 0, bounded by the
                // bottom margin (matching CUD); never scrolls.
                let n = params.first().copied().unwrap_or(1).max(1) as usize;
                let limit = if self.cursor_row <= self.scroll_region_bottom {
                    self.scroll_region_bottom
                } else {
                    self.grid.rows() - 1
                };
                self.cursor_row = (self.cursor_row + n).min(limit);
                self.cursor_col = 0;
            }
            'F' => {
                // CPL - cursor previous line. Up n, to column 0, bounded by the
                // top margin (matching CUU); never scrolls.
                let n = params.first().copied().unwrap_or(1).max(1) as usize;
                let limit = if self.cursor_row >= self.scroll_region_top {
                    self.scroll_region_top
                } else {
                    0
                };
                self.cursor_row = self.cursor_row.saturating_sub(n).max(limit);
                self.cursor_col = 0;
            }
            'G' | '`' => {
                // CHA / HPA - move cursor to absolute column (1-based)
                let col = params.first().copied().unwrap_or(1) as usize;
                self.cursor_col = col.saturating_sub(1).min(self.grid.row_len() - 1);
            }
            'd' => {
                // VPA - move cursor to absolute row (1-based), honoring origin mode
                let row = params.first().copied().unwrap_or(1) as usize;
                self.set_cursor_row_abs(row);
            }
            'I' => {
                // CHT - cursor forward tabulation (n tab stops)
                let n = params.first().copied().unwrap_or(1).max(1) as usize;
                for _ in 0..n {
                    self.cursor_col = self.next_tab_stop(self.cursor_col);
                }
            }
            'Z' => {
                // CBT - cursor backward tabulation (n tab stops)
                let n = params.first().copied().unwrap_or(1).max(1) as usize;
                for _ in 0..n {
                    self.cursor_col = self.prev_tab_stop(self.cursor_col);
                }
            }
            'b' => {
                // REP - repeat the last printed character n times
                let n = params.first().copied().unwrap_or(1).max(1) as usize;
                if let Some(ch) = self.last_printed_char {
                    for _ in 0..n {
                        self.put_char(ch, false);
                    }
                }
            }
            'g' => {
                // TBC - tab clear (0 = at cursor, 3 = all)
                match params.first().copied().unwrap_or(0) {
                    0 => {
                        if let Some(stop) = self.tab_stops.get_mut(self.cursor_col) {
                            *stop = false;
                        }
                    }
                    3 => {
                        for stop in self.tab_stops.iter_mut() {
                            *stop = false;
                        }
                    }
                    _ => {}
                }
            }
            'H' => {
                let row = params.first().copied().unwrap_or(1) as usize;
                let col = params.get(1).copied().unwrap_or(1) as usize;
                self.place_cursor(row, col);
            }
            'f' => {
                if private_prefix == Some(b'>') && intermediates.is_empty() {
                    let resource = params.first().copied().unwrap_or(0);
                    let value = params.get(1).copied().unwrap_or(0);
                    if resource == 4 {
                        crate::debug_log!(
                            "[XTFMTKEYS] formatOtherKeys={} previous={}",
                            value,
                            self.xterm_format_other_keys
                        );
                        self.xterm_format_other_keys = value;
                    }
                } else {
                    let row = params.first().copied().unwrap_or(1) as usize;
                    let col = params.get(1).copied().unwrap_or(1) as usize;
                    self.place_cursor(row, col);
                }
            }
            'J' => {
                match params.first().copied().unwrap_or(0) {
                    0 => {
                        // Clear from cursor to end of display
                        for col in self.cursor_col..self.grid.row_len() {
                            self.clear_cell(self.cursor_row, col);
                        }
                        for row in (self.cursor_row + 1)..self.grid.rows() {
                            for col in 0..self.grid.row_len() {
                                self.clear_cell(row, col);
                            }
                        }
                        self.mark_rows_dirty(self.cursor_row, self.grid.rows().saturating_sub(1));
                    }
                    1 => {
                        // Clear from start to cursor
                        for row in 0..=self.cursor_row {
                            let end_col = if row == self.cursor_row {
                                self.cursor_col + 1
                            } else {
                                self.grid.row_len()
                            };
                            for col in 0..end_col {
                                self.clear_cell(row, col);
                            }
                        }
                        self.mark_rows_dirty(0, self.cursor_row);
                    }
                    2 => {
                        // ED 2: erase the whole screen but leave the cursor in place.
                        self.clear_screen_no_home();
                    }
                    3 => {
                        // Clear scrollback (xterm extension)
                        self.erase_saved_lines();
                    }
                    _ => {}
                }
            }
            'K' => {
                // Clear line
                match params.first().copied().unwrap_or(0) {
                    0 => {
                        // Clear from cursor to end of line
                        for col in self.cursor_col..self.grid.row_len() {
                            self.clear_cell(self.cursor_row, col);
                        }
                        self.mark_row_dirty(self.cursor_row);
                    }
                    1 => {
                        // Clear from start of line to cursor
                        for col in 0..=self.cursor_col {
                            self.clear_cell(self.cursor_row, col);
                        }
                        self.mark_row_dirty(self.cursor_row);
                    }
                    2 => {
                        // Clear entire line
                        for col in 0..self.grid.row_len() {
                            self.clear_cell(self.cursor_row, col);
                        }
                        self.mark_row_dirty(self.cursor_row);
                    }
                    _ => {}
                }
            }
            'L' => {
                let n = params.first().copied().unwrap_or(1).max(1) as usize;
                let blank = self.create_blank_cell();
                for _ in 0..n {
                    if self.cursor_row >= self.scroll_region_top
                        && self.cursor_row <= self.scroll_region_bottom
                    {
                        let cols = self.grid.row_len();
                        let src_start = self.cursor_row * cols;
                        let src_end = self.scroll_region_bottom * cols;
                        let dst = (self.cursor_row + 1) * cols;
                        self.grid.cells.copy_within(src_start..src_end, dst);
                        self.grid.cells[src_start..src_start + cols].fill(blank);
                        self.grid.row_ids.copy_within(
                            self.cursor_row..self.scroll_region_bottom,
                            self.cursor_row + 1,
                        );
                        self.grid.row_ids[self.cursor_row] = RawRowId::fresh();
                        self.grid.bump_identity_revision();
                    }
                }
                self.mark_rows_dirty(self.cursor_row, self.scroll_region_bottom);
            }
            'M' => {
                let n = params.first().copied().unwrap_or(1).max(1) as usize;
                let blank = self.create_blank_cell();
                for _ in 0..n {
                    if self.cursor_row >= self.scroll_region_top
                        && self.cursor_row <= self.scroll_region_bottom
                    {
                        let cols = self.grid.row_len();
                        let src_start = (self.cursor_row + 1) * cols;
                        let src_end = (self.scroll_region_bottom + 1) * cols;
                        let dst = self.cursor_row * cols;
                        self.grid.cells.copy_within(src_start..src_end, dst);
                        let blank_start = self.scroll_region_bottom * cols;
                        self.grid.cells[blank_start..blank_start + cols].fill(blank);
                        self.grid.row_ids.copy_within(
                            self.cursor_row + 1..self.scroll_region_bottom + 1,
                            self.cursor_row,
                        );
                        self.grid.row_ids[self.scroll_region_bottom] = RawRowId::fresh();
                        self.grid.bump_identity_revision();
                    }
                }
                self.mark_rows_dirty(self.cursor_row, self.scroll_region_bottom);
            }
            'm' => {
                if private_prefix == Some(b'>') && intermediates.is_empty() {
                    let resource = params.first().copied().unwrap_or(0);
                    let value = params.get(1).copied().unwrap_or(0);
                    if resource == 4 {
                        crate::debug_log!(
                            "[XTMODKEYS] modifyOtherKeys={} previous={}",
                            value,
                            self.xterm_modify_other_keys
                        );
                        self.xterm_modify_other_keys = value;
                    }
                } else {
                    // SGR - Select Graphic Rendition
                    self.handle_sgr(&Self::parse_sgr_groups(raw_params));
                }
            }
            's' => {
                if private_prefix.is_none() && intermediates.is_empty() {
                    self.save_cursor();
                }
            }
            'u' => {
                if intermediates.is_empty() {
                    match private_prefix {
                        None => {
                            self.restore_cursor();
                        }
                        Some(b'?') => {
                            crate::debug_log!(
                                "[KEYBOARD_PROTO] query current kitty flags -> {}",
                                self.keyboard_enhancement_flags
                            );
                            let response = format!("\x1b[?{}u", self.keyboard_enhancement_flags);
                            self.output_buffer.extend_from_slice(response.as_bytes());
                        }
                        Some(b'=') => {
                            let flags = params.first().copied().unwrap_or(0);
                            let mode = params.get(1).copied().unwrap_or(1);
                            crate::debug_log!(
                                "[KEYBOARD_PROTO] set kitty flags flags={} mode={} previous={}",
                                flags,
                                mode,
                                self.keyboard_enhancement_flags
                            );
                            self.set_keyboard_enhancement_flags(flags, mode);
                            crate::debug_log!(
                                "[KEYBOARD_PROTO] new kitty flags={}",
                                self.keyboard_enhancement_flags
                            );
                        }
                        Some(b'>') => {
                            let flags = params.first().copied().unwrap_or(0);
                            crate::debug_log!(
                                "[KEYBOARD_PROTO] push kitty flags current={} new={}",
                                self.keyboard_enhancement_flags,
                                flags
                            );
                            self.push_keyboard_enhancement_flags(flags);
                        }
                        Some(b'<') => {
                            let count = params.first().copied().unwrap_or(1) as usize;
                            crate::debug_log!(
                                "[KEYBOARD_PROTO] pop kitty flags count={} current={} stack_depth={}",
                                count,
                                self.keyboard_enhancement_flags,
                                self.keyboard_enhancement_stack.len()
                            );
                            self.pop_keyboard_enhancement_flags(count);
                            crate::debug_log!(
                                "[KEYBOARD_PROTO] new kitty flags={}",
                                self.keyboard_enhancement_flags
                            );
                        }
                        _ => {}
                    }
                }
            }
            'S' => {
                // Scroll up (Scroll Up, SU) - content moves up, new lines appear at bottom
                let n = params.first().copied().unwrap_or(1).max(1) as usize;
                // Scroll within the scroll region by moving lines
                for _ in 0..n {
                    self.scroll_region_up(self.scroll_region_top, self.scroll_region_bottom);
                }
            }
            'T' => {
                // Scroll down (Scroll Down, SD) - content moves down, new lines appear at top
                let n = params.first().copied().unwrap_or(1).max(1) as usize;
                for _ in 0..n {
                    self.scroll_region_down(self.scroll_region_top, self.scroll_region_bottom);
                }
            }
            't' => {
                if private_prefix.is_none() && intermediates.is_empty() {
                    self.handle_window_ops(params);
                }
            }
            'n' => {
                // DSR - Device Status Report
                match params.first().copied().unwrap_or(0) {
                    5 => {
                        // Report device OK: CSI 0 n
                        self.output_buffer.extend_from_slice(b"\x1b[0n");
                    }
                    6 => {
                        // CPR - Cursor Position Report: CSI row ; col R (1-indexed)
                        let row = (self.cursor_row + 1) as u16;
                        let col = (self.cursor_col + 1) as u16;
                        let response = format!("\x1b[{};{}R", row, col);
                        self.output_buffer.extend(response.as_bytes());
                    }
                    _ => {}
                }
            }
            'c' => {
                if intermediates.is_empty() {
                    match private_prefix {
                        None => {
                            crate::debug_log!("[DA] primary device attributes request");
                            self.output_buffer
                                .extend_from_slice(PRIMARY_DEVICE_ATTRIBUTES_RESPONSE);
                        }
                        Some(b'>') => {
                            crate::debug_log!("[DA] secondary device attributes request");
                            self.output_buffer
                                .extend_from_slice(SECONDARY_DEVICE_ATTRIBUTES_RESPONSE);
                        }
                        _ => {}
                    }
                }
            }
            'p' => {
                if intermediates == *b"!" && private_prefix.is_none() {
                    // DECSTR (CSI ! p) - soft terminal reset.
                    self.soft_reset();
                } else if private_prefix == Some(b'?') && intermediates == *b"$" {
                    for &mode in params {
                        self.report_private_mode_status(mode);
                    }
                }
            }
            'h' => {
                // Set mode: DECSET (CSI ? Pn h) vs ANSI SM (CSI Pn h). The two
                // share parameter numbers (e.g. 4 = DECSCLM private vs IRM ANSI),
                // so the private prefix must be threaded through.
                let private = private_prefix == Some(b'?');
                for &mode in params {
                    self.set_mode(mode, private);
                }
            }
            'l' => {
                // Reset mode: DECRST (CSI ? Pn l) vs ANSI RM (CSI Pn l).
                let private = private_prefix == Some(b'?');
                for &mode in params {
                    self.reset_mode(mode, private);
                }
            }
            'r' => {
                // Set scroll region (DECSTBM)
                let top = match params.first().copied().unwrap_or(1) {
                    0 => 1,
                    v => v as usize,
                };
                let bottom = match params.get(1).copied().unwrap_or(self.grid.rows() as u16) {
                    0 => self.grid.rows(),
                    v => v as usize,
                };

                // Convert from 1-indexed to 0-indexed, and clamp to valid range
                self.scroll_region_top = top
                    .saturating_sub(1)
                    .min(self.grid.rows().saturating_sub(1));
                self.scroll_region_bottom = bottom
                    .saturating_sub(1)
                    .min(self.grid.rows().saturating_sub(1));

                // If range is invalid, reset to full screen
                if self.scroll_region_top > self.scroll_region_bottom {
                    self.scroll_region_top = 0;
                    self.scroll_region_bottom = self.grid.rows().saturating_sub(1);
                }

                // Move cursor to home position when setting scroll region
                self.cursor_row = 0;
                self.cursor_col = 0;
                self.pending_wrap = false;
            }
            '@' => {
                // ICH - Insert Character(s)
                let n = params.first().copied().unwrap_or(1).max(1) as usize;
                let cols = self.grid.row_len();
                let blank_cell = self.create_blank_cell();
                if self.cursor_col < cols {
                    // Insert n blank cells at cursor position, shifting content right
                    // insert_cell_in_row shifts cells right and discards the last cell
                    for _ in 0..n {
                        if self.cursor_col < cols {
                            self.grid.insert_cell_in_row(
                                self.cursor_row,
                                self.cursor_col,
                                blank_cell,
                            );
                        }
                    }
                    // Mark row as dirty after modification
                    self.mark_row_dirty(self.cursor_row);
                }
            }
            'P' => {
                // DCH - Delete Character(s)
                let n = params.first().copied().unwrap_or(1).max(1) as usize;
                let blank_cell = self.create_blank_cell();
                for _ in 0..n {
                    if self.cursor_col < self.grid.row_len() {
                        self.grid
                            .remove_cell_from_row(self.cursor_row, self.cursor_col);
                        // Fill the last cell with proper blank (remove_cell_from_row uses default)
                        let last_col = self.grid.row_len() - 1;
                        *self.grid.get_mut(self.cursor_row, last_col) = blank_cell;
                    }
                }
                // Mark row as dirty after modification
                self.mark_row_dirty(self.cursor_row);
            }
            'X' => {
                // ECH - Erase Character(s)
                let n = params.first().copied().unwrap_or(1).max(1) as usize;
                for i in 0..n {
                    let col = self.cursor_col + i;
                    if col < self.grid.row_len() {
                        self.clear_cell(self.cursor_row, col);
                    } else {
                        break;
                    }
                }
                // Mark row as dirty after modification
                self.mark_row_dirty(self.cursor_row);
            }
            'q' => {
                if private_prefix == Some(b'>')
                    && intermediates.is_empty()
                    && params.first().copied().unwrap_or(0) == 0
                {
                    crate::debug_log!("[XTVERSION] report terminal version request");
                    self.output_buffer.extend_from_slice(XTERM_VERSION_RESPONSE);
                }

                // DECSCUSR - Set cursor style
                if private_prefix.is_none() && intermediates == *b" " {
                    let shape = params.first().copied().unwrap_or(0) as u8;
                    self.cursor_shape = match shape {
                        0..=2 => CursorShape::Block,
                        3 | 4 => CursorShape::Underline,
                        5 | 6 => CursorShape::Beam,
                        _ => CursorShape::Block,
                    };
                }
            }
            _ => {}
        }
    }

    /// Resolve an extended color (SGR 38/48/58) from either the colon sub-parameter
    /// form (within a single group, e.g. `38:2:r:g:b` or `38:2:cs:r:g:b`) or the
    /// legacy semicolon form (`38;2;r;g;b`), advancing `gi` past consumed groups.
    fn parse_ext_color(groups: &[SmallVec<[u16; 6]>], gi: &mut usize) -> Option<Color> {
        let g = &groups[*gi];
        if g.len() >= 2 {
            // Colon sub-parameter form: everything lives in this group.
            match g[1] {
                5 => g.get(2).map(|&n| Color::Indexed(n as u8)),
                2 => {
                    // 38:2:r:g:b (len 5) or 38:2:colorspace:r:g:b (len >= 6)
                    if g.len() >= 6 {
                        Some(Color::Rgb(g[3] as u8, g[4] as u8, g[5] as u8))
                    } else if g.len() >= 5 {
                        Some(Color::Rgb(g[2] as u8, g[3] as u8, g[4] as u8))
                    } else {
                        None
                    }
                }
                _ => None,
            }
        } else {
            // Legacy semicolon form: the kind and components are separate groups.
            let first = |idx: usize| groups.get(idx).and_then(|x| x.first().copied());
            match first(*gi + 1) {
                Some(5) => {
                    let n = first(*gi + 2);
                    *gi += 2;
                    n.map(|n| Color::Indexed(n as u8))
                }
                Some(2) => {
                    let (r, gg, b) = (first(*gi + 2), first(*gi + 3), first(*gi + 4));
                    *gi += 4;
                    match (r, gg, b) {
                        (Some(r), Some(gg), Some(b)) => {
                            Some(Color::Rgb(r as u8, gg as u8, b as u8))
                        }
                        _ => None,
                    }
                }
                _ => None,
            }
        }
    }

    fn handle_sgr(&mut self, groups: &[SmallVec<[u16; 6]>]) {
        // CSI m with no parameters is a full reset.
        if groups.len() == 1 && groups[0].len() == 1 && groups[0][0] == 0 {
            self.current_flags = StyleFlags::default();
            self.current_fg = Color::Default;
            self.current_bg = Color::Default;
            return;
        }

        let mut gi = 0;
        while gi < groups.len() {
            let g = &groups[gi];
            let param = g.first().copied().unwrap_or(0);
            match param {
                0 => {
                    self.current_flags = StyleFlags::default();
                    self.current_fg = Color::Default;
                    self.current_bg = Color::Default;
                }
                1 => self.current_flags.set_bold(true),
                2 => self.current_flags.set_dim(true),
                3 => self.current_flags.set_italic(true),
                4 => {
                    // Colon sub-parameter (4:n) selects the underline style; plain 4 is
                    // a single underline. Semicolon-separated values are NOT consumed here.
                    let style = if g.len() >= 2 { g[1] } else { 1 };
                    self.current_flags.set_underline(match style {
                        0 => UnderlineStyle::None,
                        1 => UnderlineStyle::Single,
                        2 => UnderlineStyle::Double,
                        3 => UnderlineStyle::Curly,
                        4 => UnderlineStyle::Dotted,
                        5 => UnderlineStyle::Dashed,
                        _ => UnderlineStyle::Single,
                    });
                }
                5 => self.current_flags.set_blink(true),
                7 => self.current_flags.set_inverse(true),
                9 => self.current_flags.set_strikethrough(true),
                21 => self.current_flags.set_underline(UnderlineStyle::Double),
                22 => {
                    self.current_flags.set_bold(false);
                    self.current_flags.set_dim(false);
                }
                23 => self.current_flags.set_italic(false),
                24 => self.current_flags.set_underline(UnderlineStyle::None),
                25 => self.current_flags.set_blink(false),
                27 => self.current_flags.set_inverse(false),
                29 => self.current_flags.set_strikethrough(false),
                39 => self.current_fg = Color::Default,
                30..=37 => {
                    self.current_fg = match param {
                        30 => Color::Black,
                        31 => Color::Red,
                        32 => Color::Green,
                        33 => Color::Yellow,
                        34 => Color::Blue,
                        35 => Color::Magenta,
                        36 => Color::Cyan,
                        37 => Color::White,
                        _ => Color::Default,
                    };
                }
                49 => self.current_bg = Color::Default,
                40..=47 => {
                    self.current_bg = match param {
                        40 => Color::Black,
                        41 => Color::Red,
                        42 => Color::Green,
                        43 => Color::Yellow,
                        44 => Color::Blue,
                        45 => Color::Magenta,
                        46 => Color::Cyan,
                        47 => Color::White,
                        _ => Color::Default,
                    };
                    self.global_bg = self.current_bg; // Update global background
                }
                90..=97 => {
                    self.current_fg = match param {
                        90 => Color::BrightBlack,
                        91 => Color::BrightRed,
                        92 => Color::BrightGreen,
                        93 => Color::BrightYellow,
                        94 => Color::BrightBlue,
                        95 => Color::BrightMagenta,
                        96 => Color::BrightCyan,
                        97 => Color::BrightWhite,
                        _ => Color::Default,
                    };
                }
                100..=107 => {
                    self.current_bg = match param {
                        100 => Color::BrightBlack,
                        101 => Color::BrightRed,
                        102 => Color::BrightGreen,
                        103 => Color::BrightYellow,
                        104 => Color::BrightBlue,
                        105 => Color::BrightMagenta,
                        106 => Color::BrightCyan,
                        107 => Color::BrightWhite,
                        _ => Color::Default,
                    };
                    self.global_bg = self.current_bg; // Update global background
                }
                38 => {
                    if let Some(color) = Self::parse_ext_color(groups, &mut gi) {
                        self.current_fg = color;
                    }
                }
                48 => {
                    if let Some(color) = Self::parse_ext_color(groups, &mut gi) {
                        self.current_bg = color;
                        self.global_bg = self.current_bg;
                    }
                }
                58 => {
                    // SGR 58: set underline color. We don't render a distinct
                    // underline color yet, but its arguments MUST be consumed so
                    // the legacy `58;2;r;g;b` form doesn't leak r/g/b as SGR codes.
                    let _ = Self::parse_ext_color(groups, &mut gi);
                }
                59 => {
                    // SGR 59: reset underline color to default - no-op.
                }
                _ => {}
            }
            gi += 1;
        }
    }

    /// DECALN (ESC # 8): fill the entire screen with 'E', used for alignment tests.
    fn decaln(&mut self) {
        for row in self.grid.iter_mut() {
            for cell in row.iter_mut() {
                *cell = TerminalCell {
                    character: 'E',
                    foreground: Color::Default,
                    background: Color::Default,
                    flags: StyleFlags::default(),
                    hyperlink: 0,
                };
            }
        }
        self.cursor_row = 0;
        self.cursor_col = 0;
        self.pending_wrap = false;
        self.mark_rows_dirty(0, self.grid.rows().saturating_sub(1));
    }

    /// RIS (ESC c): reset the terminal to its initial state.
    fn full_reset(&mut self) {
        if self.use_alt_buffer {
            self.reset_mode(1049, true);
        }
        self.record_reset_interrupted_agent_command();
        self.current_fg = Color::Default;
        self.current_bg = Color::Default;
        self.global_bg = Color::Default;
        self.current_flags = StyleFlags::default();
        self.g0_charset = Charset::Ascii;
        self.g1_charset = Charset::Ascii;
        self.active_charset = Charset::Ascii;
        self.scroll_region_top = 0;
        self.scroll_region_bottom = self.grid.rows().saturating_sub(1);
        self.tab_stops = Self::default_tab_stops(self.grid.row_len());
        self.saved_cursor = None;
        self.modes = TerminalModes::default();
        self.modes.insert(25); // cursor visible
        self.modes.insert(7); // autowrap on
        self.scroll_offset = 0;
        self.pending_wrap = false;
        // xterm RIS also discards saved lines and resets cursor style, dynamic
        // colors, keyboard-protocol state, selection, and any open hyperlink.
        if !self.scrollback.is_empty() {
            self.bump_history_revision();
        }
        self.scrollback.clear();
        self.pending_saved_line_purge = 0;
        self.provisional_alt_snapshot = None;
        self.command_zones.clear();
        self.finished_output_provenance.clear();
        self.captured_output_bytes = 0;
        // RIS destroys the rows the undo stash points at; it cannot rebuild
        // blocks whose text is gone.
        self.cleared_blocks = None;
        self.current_zone_state = ZoneState::default();
        self.current_command_start_col = None;
        self.current_command_extent_row = None;
        self.current_output_start_col = None;
        self.current_output_start_row_id = None;
        self.current_output_extent_row = None;
        self.current_output_extent_col = None;
        self.current_output_extent_row_id = None;
        self.agent_prompt_input_tainted = false;
        self.prompt_submission_pending = false;
        self.prompt_cancel_pending = false;
        self.idle_prompt_input_dirty = false;
        self.pending_prompt_typeahead = false;
        self.idle_background_output = None;
        self.armed_agent_execution = None;
        self.active_agent_execution = None;
        self.current_command_text = None;
        self.current_command_exact = false;
        self.current_command_id = None;
        self.current_command_start_id = None;
        self.current_command_started_at = None;
        self.last_archived_screen_snapshot.clear();
        self.last_synced_primary_screen_snapshot.clear();
        self.cursor_shape = CursorShape::default();
        self.dynamic_fg = None;
        self.dynamic_bg = None;
        self.dynamic_cursor_color = None;
        self.dynamic_palette = [None; 256];
        self.keyboard_enhancement_flags = 0;
        self.keyboard_enhancement_stack.clear();
        self.alt_keyboard_enhancement_flags = 0;
        self.alt_keyboard_enhancement_stack.clear();
        self.clear_text_selection();
        self.current_hyperlink = None;
        self.osc8_hyperlinks.clear();
        self.osc8_hyperlink_keys.clear();
        // RIS resets the graphics namespace with the text screen: neither a
        // visible placement nor a half-finished upload may survive it. The
        // parser-side state of an in-flight transfer goes with it.
        self.kitty_graphics.clear();
        self.pending_apc.clear();
        self.pending_apc_scan_from = 0;
        self.discarding_oversized_apc = false;
        self.discarding_apc_prev_escape = false;
        // The other in-flight string states go with the reset as well.
        self.pending_osc.clear();
        self.pending_osc_scan_from = 0;
        self.pending_dcs.clear();
        self.pending_dcs_scan_from = 0;
        self.clear_screen();
    }

    /// DECSTR (CSI ! p): soft terminal reset. Unlike RIS this does NOT clear the
    /// screen or scrollback; it resets modes, margins, SGR, charsets and the
    /// saved cursor to their power-on defaults.
    fn soft_reset(&mut self) {
        self.current_fg = Color::Default;
        self.current_bg = Color::Default;
        self.global_bg = Color::Default;
        self.current_flags = StyleFlags::default();
        self.g0_charset = Charset::Ascii;
        self.g1_charset = Charset::Ascii;
        self.active_charset = Charset::Ascii;
        self.scroll_region_top = 0;
        self.scroll_region_bottom = self.grid.rows().saturating_sub(1);
        self.saved_cursor = None;
        // Reset the modes DECSTR is defined to touch: DECOM (6) off, IRM (4) off,
        // DECTCEM (25) on, DECAWM (7) on. Leave everything else as-is.
        self.modes.remove(&6);
        self.modes.remove(&4);
        self.modes.insert(25);
        self.modes.insert(7);
        self.pending_wrap = false;
    }

    fn clear_screen(&mut self) {
        self.clear_screen_no_home();
        self.cursor_row = 0;
        self.cursor_col = 0;
        self.pending_wrap = false;
    }

    /// Erase the whole screen WITHOUT moving the cursor (ED / CSI 2 J).
    fn clear_screen_no_home(&mut self) {
        if self.sync_output_active {
            if self.use_alt_buffer {
                // The alternate screen holds no document, only the screen the
                // app is currently painting, so an erase here is part of a
                // repaint rather than history being destroyed. Treat it as one
                // more frame: promoting it would let a clear-every-frame TUI
                // make every repaint permanent, which is the flood again.
                self.archive_alt_screen_frame();
            } else {
                self.archive_primary_screen_unless_last_synced_snapshot();
            }
        } else {
            self.archive_visible_screen_to_scrollback();
        }
        let bg_color = self.current_bg;
        for row in self.grid.iter_mut() {
            for cell in row.iter_mut() {
                *cell = TerminalCell {
                    character: ' ',
                    foreground: Color::Default,
                    background: bg_color,
                    flags: StyleFlags::default(),
                    hyperlink: 0,
                };
            }
        }
        self.mark_rows_dirty(0, self.grid.rows().saturating_sub(1));
    }

    fn set_mode(&mut self, mode: u16, private: bool) {
        if !private {
            // ANSI Set Mode (CSI Pn h). The only one we implement is IRM (4).
            // Everything else (GATM 1, ERM 6, VEM 7, LNM 20, …) is ignored so
            // it can't collide with the identically-numbered DEC private modes.
            if mode == 4 {
                self.modes.insert(4);
            }
            return;
        }
        match mode {
            4 => {
                // DECSCLM (smooth scroll) — accepted and ignored. Must NOT fall
                // through to the IRM bit that ANSI mode 4 uses.
            }
            25 => {
                // Show cursor (mode 25)
                self.modes.insert(25);
            }
            1004 => {
                // Focus event reporting
                self.modes.insert(1004);
            }
            2004 => {
                // Bracketed paste mode
                self.modes.insert(2004);
            }
            1000..=1003 => {
                // Mouse reporting modes
                self.modes.insert(mode);
            }
            1006 => {
                // SGR mouse reporting format
                self.modes.insert(mode);
            }
            1048 => {
                // Save cursor (DECSC equivalent), no buffer switch.
                self.save_cursor();
                self.modes.insert(1048);
            }
            47 | 1047 | 1049 => {
                if self.disable_alt_screen {
                    return;
                }
                // Alternate screen buffer (47/1047 = swap only, 1049 also saves
                // the main-screen cursor). We treat all three as a buffer swap.
                if !self.use_alt_buffer {
                    // Save main buffer state (cursor position)
                    self.saved_cursor_row = self.cursor_row;
                    self.saved_cursor_col = self.cursor_col;
                    self.saved_primary_screen_state = Some(self.snapshot_cursor_state());
                    self.saved_primary_global_bg = self.global_bg;
                    self.saved_primary_cursor_shape = self.cursor_shape;
                    self.saved_primary_dynamic_fg = self.dynamic_fg;
                    self.saved_primary_dynamic_bg = self.dynamic_bg;
                    self.saved_primary_dynamic_cursor_color = self.dynamic_cursor_color;
                    self.saved_primary_dynamic_palette = self.dynamic_palette;

                    // Reset scroll offset so we don't show scrollback in alt buffer
                    self.scroll_offset = 0;
                    self.provisional_alt_snapshot = None;

                    // Switch to alternate buffer
                    std::mem::swap(&mut self.grid, &mut self.alt_grid);
                    self.alt_cursor_row = self.cursor_row;
                    self.alt_cursor_col = self.cursor_col;
                    std::mem::swap(
                        &mut self.keyboard_enhancement_flags,
                        &mut self.alt_keyboard_enhancement_flags,
                    );
                    std::mem::swap(
                        &mut self.keyboard_enhancement_stack,
                        &mut self.alt_keyboard_enhancement_stack,
                    );
                    self.use_alt_buffer = true;
                    // Retained cells keep their keys, but an unterminated
                    // primary-screen link must not arm alternate-screen text.
                    self.current_hyperlink = None;

                    // Selection anchors are absolute (scrollback+grid) row indices
                    // tied to the buffer that was visible. After a buffer swap they
                    // would highlight unrelated lines, so drop the selection.
                    self.clear_text_selection();
                    // DECSTBM is a per-buffer attribute; reset to full-screen so a
                    // partial scroll region from the main buffer doesn't leak in.
                    self.scroll_region_top = 0;
                    self.scroll_region_bottom = self.grid.rows().saturating_sub(1);

                    // Clear alt buffer and move cursor to home
                    self.clear_screen();
                    self.modes.insert(mode);
                }
            }
            2026 => {
                // Synchronized output: suppress rendering until cleared
                self.modes.insert(2026);
                self.sync_output_active = true;
                self.sync_output_start = Some(std::time::Instant::now());
            }
            7 => {
                // Autowrap mode
                self.modes.insert(7);
            }
            _ => {
                // Unknown mode, just store it
                self.modes.insert(mode);
            }
        }
    }

    fn reset_mode(&mut self, mode: u16, private: bool) {
        if !private {
            // ANSI Reset Mode (CSI Pn l). Only IRM (4) is implemented.
            if mode == 4 {
                self.modes.remove(&4);
            }
            return;
        }
        match mode {
            4 => {
                // DECSCLM reset — ignored (see set_mode).
            }
            25 => {
                // Hide cursor
                self.modes.remove(&25);
            }
            1004 => {
                // Disable focus event reporting
                self.modes.remove(&1004);
            }
            2004 => {
                // Disable bracketed paste mode
                self.modes.remove(&2004);
            }
            1000..=1003 => {
                // Disable mouse reporting
                self.modes.remove(&mode);
            }
            1006 => {
                // Disable SGR mouse reporting format
                self.modes.remove(&mode);
            }
            1048 => {
                // Restore cursor (DECRC equivalent), no buffer switch.
                self.restore_cursor();
                self.modes.remove(&1048);
            }
            47 | 1047 | 1049 => {
                // Restore main screen buffer
                if self.use_alt_buffer {
                    // Save alt buffer state (cursor position)
                    self.alt_cursor_row = self.cursor_row;
                    self.alt_cursor_col = self.cursor_col;

                    // Switch back to main buffer
                    std::mem::swap(&mut self.grid, &mut self.alt_grid);
                    self.cursor_row = self.saved_cursor_row;
                    self.cursor_col = self.saved_cursor_col;
                    std::mem::swap(
                        &mut self.keyboard_enhancement_flags,
                        &mut self.alt_keyboard_enhancement_flags,
                    );
                    std::mem::swap(
                        &mut self.keyboard_enhancement_stack,
                        &mut self.alt_keyboard_enhancement_stack,
                    );
                    self.use_alt_buffer = false;
                    // Whatever the alternate screen last left in scrollback is
                    // all that remains of it, so it stops being provisional.
                    self.provisional_alt_snapshot = None;
                    self.modes.remove(&mode);
                    self.pending_wrap = false;
                    // Likewise, alt-screen OSC 8 state never leaks back onto
                    // newly printed primary-screen cells.
                    self.current_hyperlink = None;

                    // See the matching set_mode arm: clear selection because its
                    // anchors point into the alt buffer, and reset DECSTBM so the
                    // alt buffer's scroll region doesn't carry into the main one.
                    self.clear_text_selection();
                    self.scroll_region_top = 0;
                    self.scroll_region_bottom = self.grid.rows().saturating_sub(1);

                    // Restore the hidden primary screen's drawing state so any
                    // resize that happened while a fullscreen app was active
                    // does not leave the main buffer tinted with alt colors.
                    if let Some(saved) = self.saved_primary_screen_state.take() {
                        self.current_fg = saved.fg;
                        self.current_bg = saved.bg;
                        self.current_flags = saved.flags;
                        self.g0_charset = saved.g0;
                        self.g1_charset = saved.g1;
                        self.active_charset = saved.active;
                        if saved.origin_mode {
                            self.modes.insert(6);
                        } else {
                            self.modes.remove(&6);
                        }
                        self.pending_wrap = saved.pending_wrap;
                    } else {
                        self.current_fg = Color::Default;
                        self.current_bg = Color::Default;
                        self.current_flags = StyleFlags::default();
                        self.modes.remove(&6);
                    }
                    self.global_bg = self.saved_primary_global_bg;
                    self.saved_primary_global_bg = Color::Default;
                    self.cursor_shape = self.saved_primary_cursor_shape;
                    self.dynamic_fg = self.saved_primary_dynamic_fg;
                    self.dynamic_bg = self.saved_primary_dynamic_bg;
                    self.dynamic_cursor_color = self.saved_primary_dynamic_cursor_color;
                    self.dynamic_palette = self.saved_primary_dynamic_palette;
                    self.saved_primary_dynamic_fg = None;
                    self.saved_primary_dynamic_bg = None;
                    self.saved_primary_dynamic_cursor_color = None;
                    self.saved_primary_dynamic_palette = [None; 256];

                    // Mark all rows dirty after grid swap to force full re-render
                    // Increment by rows+1 to trigger grid_version_jumped in ui.rs
                    self.grid_version += self.grid.rows() as u64 + 1;
                    for row_ver in &mut self.row_versions {
                        *row_ver = self.grid_version;
                    }
                }
            }
            2026 => {
                // End synchronized output: force full render
                if self.use_alt_buffer {
                    self.archive_alt_screen_frame();
                } else {
                    self.last_synced_primary_screen_snapshot =
                        self.visible_screen_snapshot().unwrap_or_default();
                }
                self.modes.remove(&2026);
                self.sync_output_active = false;
                self.sync_output_start = None;
                self.mark_rows_dirty(0, self.grid.rows().saturating_sub(1));
            }
            7 => {
                // Disable autowrap
                self.modes.remove(&7);
            }
            _ => {
                // Unknown mode, just remove it
                self.modes.remove(&mode);
            }
        }
    }

    /// Number of lines currently retained in scrollback (above the live grid).
    /// This is the maximum value `scroll_offset` may take.
    pub fn scrollback_len(&self) -> usize {
        self.scrollback.len()
    }

    /// Iterate over the complete searchable buffer in absolute row order.
    /// Plain history rows are borrowed as text (no cell materialization),
    /// encoded history rows are decompressed lazily, and live-grid rows stay
    /// borrowed — a full-buffer search no longer rebuilds every row.
    pub fn search_lines(&self) -> impl Iterator<Item = SearchLine<'_>> + '_ {
        self.scrollback
            .iter()
            .map(ScrollbackLine::search_text)
            .chain(
                self.grid
                    .iter()
                    .map(|row| SearchLine::Cells(Cow::Borrowed(row))),
            )
    }

    /// Absolute buffer row represented by viewport row zero.
    pub fn viewport_absolute_start(&self) -> usize {
        self.scrollback.len().saturating_sub(self.scroll_offset)
    }

    /// Stable identity of one currently retained absolute buffer row.
    pub fn raw_row_id_at_absolute(&self, absolute_row: usize) -> Option<RawRowId> {
        if absolute_row < self.scrollback.len() {
            self.scrollback
                .get(absolute_row)
                .map(ScrollbackLine::raw_row_id)
                .filter(|row| row.is_tracked())
        } else {
            self.grid
                .raw_row_id(absolute_row - self.scrollback.len())
                .filter(|row| row.is_tracked())
        }
    }

    /// Stable identity of one currently retained absolute buffer cell.
    pub fn raw_cell_origin_at_absolute(
        &self,
        absolute_row: usize,
        col: usize,
    ) -> Option<RawCellOrigin> {
        let width = if absolute_row < self.scrollback.len() {
            self.scrollback.get(absolute_row)?.cols as usize
        } else {
            self.grid.cols()
        };
        if col >= width {
            return None;
        }
        Some(RawCellOrigin {
            row: self.raw_row_id_at_absolute(absolute_row)?,
            col,
        })
    }

    /// Scroll just enough to reveal an absolute (`scrollback + grid`) row,
    /// centering historical matches when possible.
    pub fn reveal_buffer_row(&mut self, row: usize) {
        let history = self.scrollback.len();
        let rows = self.grid.rows().max(1);
        if row >= history + self.grid.rows() {
            return;
        }
        let start = self.viewport_absolute_start();
        if row >= start && row < start.saturating_add(rows) {
            return;
        }
        let desired_start = row.saturating_sub(rows / 2).min(history);
        self.set_scroll_offset(history.saturating_sub(desired_start));
    }

    /// Set the absolute scrollback offset (0 = live view at bottom), clamped.
    /// In alternate screen, this is allowed only when a synchronized full-screen
    /// app has produced local history snapshots.
    pub fn set_scroll_offset(&mut self, offset: usize) {
        if self.use_alt_buffer && self.scrollback.is_empty() {
            return;
        }
        self.scroll_offset = offset.min(self.scrollback.len());
        self.settle_pending_saved_line_purge();
    }

    pub fn set_max_scrollback(&mut self, max_scrollback: usize) {
        self.max_scrollback = max_scrollback.max(1);

        let mut trimmed = 0usize;
        while self.scrollback.len() > self.max_scrollback {
            self.scrollback.pop_front();
            trimmed += 1;
        }
        self.on_scrollback_rows_trimmed(trimmed);

        self.scroll_offset = self.scroll_offset.min(self.scrollback.len());
    }

    pub fn set_disable_alt_screen(&mut self, disable_alt_screen: bool) {
        // Turning the option on while an application is already in the alternate
        // buffer must first restore the primary buffer. The option only blocks
        // future *entries*; exit sequences are always honored.
        if disable_alt_screen && !self.disable_alt_screen && self.use_alt_buffer {
            self.reset_mode(1049, true);
        }
        self.disable_alt_screen = disable_alt_screen;
    }

    pub fn set_viewport_pixel_size(&mut self, width: u32, height: u32) {
        self.viewport_pixel_width = width;
        self.viewport_pixel_height = height;
    }

    pub fn is_cursor_visible(&self) -> bool {
        // Cursor is visible when mode 25 is SET (via \x1b[?25h)
        // Hidden when mode 25 is RESET (via \x1b[?25l)
        // While viewing scrollback we intentionally hide the live cursor,
        // because the viewport no longer tracks the active prompt line.
        self.modes.contains(&25) && self.scroll_offset == 0
    }

    pub fn scroll_to_bottom(&mut self) {
        self.scroll_offset = 0;
        self.settle_pending_saved_line_purge();
    }

    fn push_utf8_mouse_coord(output: &mut Vec<u8>, value: usize) {
        let codepoint = 32 + value.saturating_add(1).min(2015) as u32;
        if let Some(ch) = char::from_u32(codepoint) {
            let mut buf = [0u8; 4];
            output.extend_from_slice(ch.encode_utf8(&mut buf).as_bytes());
        }
    }

    fn x10_mouse_report(button: u8, col: usize, row: usize) -> Vec<u8> {
        let mut output = Vec::with_capacity(6);
        output.extend_from_slice(b"\x1b[M");
        output.push(32 + button);
        output.push(32 + (col.saturating_add(1).min(223) as u8));
        output.push(32 + (row.saturating_add(1).min(223) as u8));
        output
    }

    fn utf8_mouse_report(button: u8, col: usize, row: usize) -> Vec<u8> {
        let mut output = Vec::with_capacity(12);
        output.extend_from_slice(b"\x1b[M");
        output.push(32 + button);
        Self::push_utf8_mouse_coord(&mut output, col);
        Self::push_utf8_mouse_coord(&mut output, row);
        output
    }

    fn urxvt_mouse_report(button: u8, col: usize, row: usize) -> Vec<u8> {
        format!(
            "\x1b[{};{};{}M",
            32 + button,
            col.saturating_add(1),
            row.saturating_add(1)
        )
        .into_bytes()
    }

    pub fn get_mouse_report(&self, button: u8, col: usize, row: usize) -> Option<Vec<u8>> {
        // Check if any mouse reporting mode is enabled
        if !self.modes.contains(&1000) && !self.modes.contains(&1002) && !self.modes.contains(&1003)
        {
            return None;
        }

        // SGR format (mode 1006) is preferred: CSI < button ; col ; row M/m
        // urxvt format (mode 1015): CSI button ; col ; row M
        // UTF-8 format (mode 1005): CSI M button col row, with UTF-8 coords
        // Standard format (mode 1000/1002): CSI M button col row (raw bytes)

        if self.modes.contains(&1006) {
            // SGR format: CSI < button ; x ; y M (button press) or m (button release)
            // For now, we'll generate press events (M) - release tracking would need more state
            // SGR encodes coordinates as decimal integers, so the 223/255 cap that
            // applies to the legacy X10 byte form is not needed here.
            let x = col as u32 + 1;
            let y = row as u32 + 1;
            Some(format!("\x1b[<{};{};{}M", button, x, y).into_bytes())
        } else if self.modes.contains(&1015) {
            Some(Self::urxvt_mouse_report(button, col, row))
        } else if self.modes.contains(&1005) {
            Some(Self::utf8_mouse_report(button, col, row))
        } else {
            Some(Self::x10_mouse_report(button, col, row))
        }
    }

    pub fn get_mouse_release_report(&self, button: u8, col: usize, row: usize) -> Option<Vec<u8>> {
        if !self.modes.contains(&1000) && !self.modes.contains(&1002) && !self.modes.contains(&1003)
        {
            return None;
        }

        if self.modes.contains(&1006) {
            // SGR format: lowercase 'm' for release
            let x = col as u32 + 1;
            let y = row as u32 + 1;
            Some(format!("\x1b[<{};{};{}m", button, x, y).into_bytes())
        } else if self.modes.contains(&1015) {
            Some(Self::urxvt_mouse_report(3, col, row))
        } else if self.modes.contains(&1005) {
            Some(Self::utf8_mouse_report(3, col, row))
        } else {
            Some(Self::x10_mouse_report(3, col, row))
        }
    }

    pub fn is_mouse_enabled(&self) -> bool {
        self.modes.contains(&1000) || self.modes.contains(&1002) || self.modes.contains(&1003)
    }

    /// True when the app requested button-drag (1002) or any-motion (1003) reporting.
    pub fn is_mouse_motion_enabled(&self) -> bool {
        self.modes.contains(&1002) || self.modes.contains(&1003)
    }

    /// Public view of the alt-screen mode switch; block-mode chrome (and
    /// tests) read it — a full-screen app owns the whole grid.
    pub fn is_alt_buffer_active(&self) -> bool {
        self.use_alt_buffer
    }

    pub fn is_bracketed_paste_enabled(&self) -> bool {
        self.modes.contains(&2004)
    }

    pub fn is_application_cursor_keys(&self) -> bool {
        self.modes.contains(&1)
    }

    /// How much of the shell's work this terminal can actually see.
    fn shell_phase(&self) -> click_cursor::ShellPhase {
        match self.current_zone_state {
            ZoneState::CommandStarted(_, _) => click_cursor::ShellPhase::Editing,
            ZoneState::OutputStarted(_, _, _) => click_cursor::ShellPhase::Running,
            // A shell without OSC 133 integration never leaves `Idle`. Staying
            // `Unknown` keeps the feature working under plain bash.
            ZoneState::Idle | ZoneState::PromptStarted(_) => click_cursor::ShellPhase::Unknown,
        }
    }

    /// The cells a click is allowed to travel over: the whole soft-wrapped
    /// logical line the cursor sits on, ending one past its last character.
    ///
    /// The prompt is inside this span. That is deliberate — clicking it means
    /// "go to the start of the line", and a line editor ignores the extra
    /// `Left`s once the buffer start is reached. The *end* is what has to be
    /// exact: a `Right` past the buffer end is what accepts jsh's inline
    /// suggestion.
    fn editable_span(&self) -> Option<click_cursor::InputSpan> {
        let rows = self.grid.rows();
        let cols = self.grid.cols();
        if rows == 0 || cols == 0 {
            return None;
        }

        let cursor_row = self.cursor_row.min(rows - 1);
        let mut first = cursor_row;
        while first > 0 && self.grid.row_wrapped[first - 1] {
            first -= 1;
        }
        let mut last = cursor_row;
        while last + 1 < rows && self.grid.row_wrapped[last] {
            last += 1;
        }

        let occupied = |row: usize, col: usize| {
            let cell = self.grid.get(row, col);
            // A wide character's continuation cell holds a blank but is
            // still occupied, so trailing CJK must not be trimmed away.
            cell.flags.wide_continuation() || !matches!(cell.character, ' ' | '\0' | '\u{a0}')
        };
        // One past the last occupied cell, looking only at columns before
        // `col_bound` on `row_bound` itself.
        let scan_back = |row_bound: usize, col_bound: usize| {
            let mut end = click_cursor::Cell::new(first as i64, 0);
            'scan: for row in (first..=row_bound).rev() {
                let cols_here = if row == row_bound { col_bound } else { cols };
                for col in (0..cols_here).rev() {
                    if occupied(row, col) {
                        end = click_cursor::Cell::new(row as i64, col as i64 + 1);
                        break 'scan;
                    }
                }
            }
            end
        };
        let mut end = scan_back(last, cols);

        // A right-aligned decoration — jsh and fish paint the previous
        // command's duration flush with the right edge of the input row — is
        // on the row but not in the buffer. Its shape gives it away: a
        // trailing run that reaches the right edge, parted from everything
        // before it by a wide blank gap, entirely right of the cursor. Clip
        // it, or a click in the gap overshoots the buffer end — and past-end
        // `Right`s are how jsh accepts an inline suggestion, even one that is
        // not on screen at the moment.
        if end.col + 1 >= cols as i64 && end.col > 0 {
            let end_row = end.row as usize;
            let mut run_start = end.col as usize;
            while run_start > 0 && occupied(end_row, run_start - 1) {
                run_start -= 1;
            }
            let mut gap_start = run_start;
            while gap_start > 0 && !occupied(end_row, gap_start - 1) {
                gap_start -= 1;
            }
            if run_start - gap_start >= 3
                && (end_row as i64, run_start as i64) > (cursor_row as i64, self.cursor_col as i64)
            {
                end = scan_back(end_row, gap_start);
            }
        }

        // Trailing spaces the user typed are part of the buffer even though the
        // scan above cannot tell them from padding, so never place the end
        // before where the shell has its cursor.
        let cursor = click_cursor::Cell::new(cursor_row as i64, self.cursor_col as i64);
        if (end.row, end.col) < (cursor.row, cursor.col) {
            end = cursor;
        }

        // A fish-style shell paints its inline suggestion past the cursor and
        // then parks the cursor back at the end of what was typed. Those cells
        // are a preview, not buffer — the backwards scan above cannot tell them
        // from typed text, and every `Right` spent on them is the shell
        // *accepting* the suggestion. Cut the span at the first one.
        if let Some(ghost) = self.inline_suggestion_start(cursor, end) {
            end = ghost;
        }

        Some(click_cursor::InputSpan {
            start: click_cursor::Cell::new(first as i64, 0),
            end,
        })
    }

    /// Where inline-suggestion ("ghost") text begins between `from` and `end`,
    /// if it begins at all.
    ///
    /// The scan runs forward from the cursor because that is where a suggestion
    /// starts: shells only offer one when the caret is at the end of the
    /// buffer, so the first suggestion-styled cell at or after the cursor is
    /// where the real input stops.
    fn inline_suggestion_start(
        &self,
        from: click_cursor::Cell,
        end: click_cursor::Cell,
    ) -> Option<click_cursor::Cell> {
        let rows = self.grid.rows();
        let cols = self.grid.cols();
        let mut row = from.row.max(0) as usize;
        let mut col = from.col.max(0) as usize;
        while row < rows && (row as i64, col as i64) < (end.row, end.col) {
            if col >= cols {
                row += 1;
                col = 0;
                continue;
            }
            if is_inline_suggestion_cell(self.grid.get(row, col)) {
                return Some(click_cursor::Cell::new(row as i64, col as i64));
            }
            col += 1;
        }
        None
    }

    /// Arrow-key bytes that walk the shell's line editor to a clicked cell, or
    /// nothing when this click must not move it.
    ///
    /// `click_row`/`click_col` are viewport coordinates, which only line up
    /// with the grid while the scrollback is at the bottom — the
    /// `scrolled_back` guard is what makes that assumption safe.
    pub fn click_cursor_move(&self, click_row: usize, click_col: usize, enabled: bool) -> Vec<u8> {
        let guards = click_cursor::Guards {
            enabled,
            mouse_reporting: self.is_mouse_enabled(),
            alt_screen: self.use_alt_buffer,
            scrolled_back: self.scroll_offset != 0,
            phase: self.shell_phase(),
        };
        if !click_cursor::click_may_move_cursor(&guards) {
            return Vec::new();
        }

        let columns = self.grid.cols() as i64;
        let cursor = click_cursor::Cell::new(self.cursor_row as i64, self.cursor_col as i64);
        let click = click_cursor::Cell::new(click_row as i64, click_col as i64);
        let Some(span) = self.editable_span() else {
            return Vec::new();
        };
        // The pinned core still clamps every out-of-span click to Home/End.
        // Refuse rows belonging to completed blocks here so selecting history
        // cannot silently move the live shell cursor.
        let first_row = span.start.row.min(span.end.row);
        let last_row = span.start.row.max(span.end.row);
        if click.row < first_row || click.row > last_row {
            return Vec::new();
        }
        let Some(target) = click_cursor::target_cell(cursor, click, columns, Some(span)) else {
            return Vec::new();
        };

        let steps = click_cursor::char_steps(cursor, target, columns, |row, col| {
            row >= 0
                && col >= 0
                && (row as usize) < self.grid.rows()
                && (col as usize) < self.grid.cols()
                && self
                    .grid
                    .get(row as usize, col as usize)
                    .flags
                    .wide_continuation()
        });
        click_cursor::arrow_bytes(steps, self.is_application_cursor_keys())
    }

    pub fn is_application_keypad(&self) -> bool {
        self.modes.contains(&66)
    }

    pub fn keyboard_enhancement_flags(&self) -> u16 {
        self.keyboard_enhancement_flags
    }

    pub fn xterm_modify_other_keys(&self) -> u16 {
        self.xterm_modify_other_keys
    }

    pub fn xterm_format_other_keys(&self) -> u16 {
        self.xterm_format_other_keys
    }

    /// True only when the app asked for every key press as an escape code, i.e.
    /// the Kitty "report all keys" flag (0b1000). Private mode 2031 is *not* a
    /// keyboard mode — it is the in-band light/dark theme-change notification
    /// (VTE/foot/contour), which apps such as the Claude CLI enable on startup.
    /// Treating it as a keyboard mode turned ordinary typing into CSI-u reports
    /// and lost the shifted form of every character.
    pub fn is_report_all_keys_enabled(&self) -> bool {
        (self.keyboard_enhancement_flags & 0b1000) != 0
    }

    pub fn build_paste_event(&mut self, mime_types: &[String]) -> Vec<u8> {
        let password = uuid::Uuid::new_v4().to_string();
        self.pending_paste_password = Some(password.clone());
        let encoded_password =
            base64::engine::general_purpose::STANDARD.encode(password.as_bytes());
        let mut output = Vec::new();

        output.extend_from_slice(b"\x1b]5522;type=read:status=OK:password=");
        output.extend_from_slice(encoded_password.as_bytes());
        output.extend_from_slice(Self::osc_terminator());

        for mime_type in mime_types {
            let encoded_mime =
                base64::engine::general_purpose::STANDARD.encode(mime_type.as_bytes());
            output.extend_from_slice(b"\x1b]5522;type=read:status=DATA:mime=");
            output.extend_from_slice(encoded_mime.as_bytes());
            output.extend_from_slice(Self::osc_terminator());
        }

        output.extend_from_slice(b"\x1b]5522;type=read:status=DONE\x1b\\");
        output
    }

    pub fn take_clipboard_read_requests(&mut self) -> Vec<ClipboardReadRequest> {
        std::mem::take(&mut self.pending_clipboard_requests)
    }

    /// Plan the complete identity document before any viewport slicing. The
    /// scrollback half uses compressed-row layout metadata only, so even a
    /// very deep history stays cold until visible cells are requested.
    #[allow(dead_code)] // P1 plan core; consumer wiring lands in the collapse slice.
    fn identity_projection_plan(&self, cols: usize) -> ProjectionPlan {
        let history_layouts = self
            .scrollback
            .iter()
            .enumerate()
            .map(|(absolute_row, line)| line.layout(absolute_row));
        let history_len = self.scrollback.len();
        let grid_layouts = self.grid.iter().enumerate().map(|(grid_row, cells)| {
            RawRowLayout::from_cells(
                cells,
                self.grid.row_wrapped[grid_row],
                history_len + grid_row,
                self.grid.row_ids[grid_row],
            )
        });
        ProjectionPlan::identity(history_layouts, grid_layouts, cols.max(1))
    }

    /// Resolve requested collapse ids against the exact finalized-zone
    /// provenance captured by the terminal. Any stale/malformed candidate is
    /// ignored, and every member of an overlapping component is rejected so
    /// policy order can never decide which block wins.
    #[allow(dead_code)] // P1 plan core; consumer wiring lands in the next slice.
    fn resolved_collapses(&self, policy: &ProjectionPolicy) -> Vec<ResolvedCollapse> {
        let mut candidates: Vec<_> = policy
            .collapsed_zone_ids()
            .filter_map(|zone_id| {
                let zone = self.zone_by_id(zone_id)?;
                if zone.rows_evicted {
                    return None;
                }
                let provenance = self.finished_output_provenance.get(&zone_id)?;
                let start_absolute = zone.output_start?;
                let end_absolute = zone.output_end?.checked_sub(1)?;
                let expected_rows = end_absolute.checked_sub(start_absolute)?.checked_add(1)?;
                (start_absolute <= end_absolute
                    && provenance.row_ids.len() == expected_rows
                    && provenance.range.zone_id == zone_id
                    && provenance.range.start.row.is_tracked()
                    && provenance.range.end.row.is_tracked())
                .then_some(ResolvedCollapse {
                    range: provenance.range,
                    start_absolute,
                    end_absolute,
                })
            })
            .collect();
        candidates.sort_unstable_by_key(|collapse| {
            (
                collapse.start_absolute,
                collapse.range.start.col,
                collapse.end_absolute,
                collapse.range.end.col,
                collapse.range.zone_id,
            )
        });

        let mut rejected = vec![false; candidates.len()];
        let mut group_start = 0usize;
        while group_start < candidates.len() {
            let mut group_end = group_start + 1;
            let mut furthest_end = (
                candidates[group_start].end_absolute,
                candidates[group_start].range.end.col,
            );
            while group_end < candidates.len() {
                let next_start = (
                    candidates[group_end].start_absolute,
                    candidates[group_end].range.start.col,
                );
                if next_start >= furthest_end {
                    break;
                }
                furthest_end = furthest_end.max((
                    candidates[group_end].end_absolute,
                    candidates[group_end].range.end.col,
                ));
                group_end += 1;
            }
            if group_end - group_start > 1 {
                rejected[group_start..group_end].fill(true);
            }
            group_start = group_end;
        }

        candidates
            .into_iter()
            .enumerate()
            .filter_map(|(index, collapse)| {
                if rejected[index]
                    || self.finished_output_range(collapse.range.zone_id) != Some(collapse.range)
                {
                    None
                } else {
                    Some(collapse)
                }
            })
            .collect()
    }

    #[allow(dead_code)] // P1 plan core; consumer wiring lands in the next slice.
    fn collapsed_projection_plan(&self, cols: usize, policy: &ProjectionPolicy) -> ProjectionPlan {
        let mut identity = self.identity_projection_plan(cols);
        if policy.is_identity() {
            identity.policy_revision = policy.revision();
            return identity;
        }
        let collapses = self.resolved_collapses(policy);
        identity.splice_collapses(&collapses, policy.revision())
    }

    fn cached_collapsed_projection_plan(
        &mut self,
        cols: usize,
        policy: &ProjectionPolicy,
    ) -> Option<std::sync::Arc<ProjectionPlan>> {
        let key = self.projection_plan_cache_key(cols, policy);
        if let Some((cached_key, plan)) = &self.projection_plan_cache {
            if *cached_key == key {
                return Some(std::sync::Arc::clone(plan));
            }
        }
        let collapses = self.resolved_collapses(policy);
        if collapses.is_empty() {
            return None;
        }
        #[cfg(test)]
        PROJECTION_PLAN_BUILD_COUNT.with(|count| count.set(count.get().saturating_add(1)));
        let mut plan = self
            .identity_projection_plan(cols)
            .splice_collapses(&collapses, policy.revision());
        if plan.effective_collapsed.is_empty() {
            return None;
        }
        plan.plan_revision = self.next_projection_plan_revision;
        if self.next_projection_plan_revision != 0 {
            self.next_projection_plan_revision = self
                .next_projection_plan_revision
                .checked_add(1)
                .unwrap_or(0);
        }
        let plan = std::sync::Arc::new(plan);
        self.projection_plan_cache = Some((key, std::sync::Arc::clone(&plan)));
        Some(plan)
    }

    fn projection_plan_cache_key(
        &self,
        cols: usize,
        policy: &ProjectionPolicy,
    ) -> ProjectionPlanCacheKey {
        ProjectionPlanCacheKey {
            history_revision: self.history_revision,
            row_identity_revision: self.grid.identity_revision,
            rows: self.grid.rows(),
            cols,
            row_wrapped: self.grid.row_wrapped.iter().copied().collect(),
            policy_revision: policy.revision(),
            policy_ids: policy.ids(),
            next_zone_id: self.next_zone_id,
            zone_count: self.command_zones.len(),
            provenance_count: self.finished_output_provenance.len(),
        }
    }

    /// Test oracle for the future viewport-slice materializer. It intentionally
    /// consumes only `RawSlice::source`; missing raw origins must never erase
    /// terminal bytes from the projected document.
    #[cfg(test)]
    fn materialize_identity_projection_plan(
        &self,
        plan: &ProjectionPlan,
    ) -> Vec<Vec<TerminalCell>> {
        let mut history_cache: HashMap<usize, Vec<TerminalCell>> = HashMap::new();
        plan.rows
            .iter()
            .map(|planned_row| {
                let mut cells = vec![TerminalCell::default(); plan.cols];
                for slice in &planned_row.raw_slices {
                    let source = if slice.source.absolute_row < self.scrollback.len() {
                        history_cache
                            .entry(slice.source.absolute_row)
                            .or_insert_with(|| {
                                self.scrollback[slice.source.absolute_row].decompress()
                            })
                            .as_slice()
                    } else {
                        let grid_row = slice.source.absolute_row - self.scrollback.len();
                        &self.grid[grid_row]
                    };
                    let source_end = slice.source.col_start + slice.len;
                    let view_end = slice.view_col_start + slice.len;
                    cells[slice.view_col_start..view_end]
                        .copy_from_slice(&source[slice.source.col_start..source_end]);
                    if slice.narrow_wide_body {
                        cells[slice.view_col_start].flags.set_wide(false);
                    }
                }
                cells
            })
            .collect()
    }

    fn reflow_projected_origins(
        lines: &[ScrollbackLine],
        new_cols: usize,
        raw_absolute_start: usize,
    ) -> Vec<ProjectedLine> {
        let mut result = Vec::new();
        let mut i = 0;

        while i < lines.len() {
            let group_first_source = lines[i].raw_row_id.is_tracked().then_some(RowSource {
                raw_row: lines[i].raw_row_id,
                raw_absolute_row: raw_absolute_start + i,
            });
            let mut logical_cells: Vec<TerminalCell> = Vec::new();
            let mut logical_spans: Vec<LineOriginSpan> = Vec::new();
            let mut append_line = |line: &ScrollbackLine, raw_absolute_row: usize| {
                let decompressed = line.decompress();
                let retained = Self::strip_trailing_blanks(&decompressed);
                let view_col_start = logical_cells.len();
                logical_cells.extend_from_slice(retained);
                if !retained.is_empty() && line.raw_row_id.is_tracked() {
                    logical_spans.push(LineOriginSpan {
                        view_col_start,
                        view_col_end: logical_cells.len(),
                        raw_row: line.raw_row_id,
                        raw_col_start: 0,
                        raw_absolute_row,
                    });
                }
            };

            append_line(&lines[i], raw_absolute_start + i);
            while i < lines.len() && lines[i].is_wrapped {
                i += 1;
                if i < lines.len() {
                    append_line(&lines[i], raw_absolute_start + i);
                }
            }
            i += 1;

            if logical_cells.is_empty() {
                result.push(ProjectedLine {
                    cells: vec![TerminalCell::default(); new_cols],
                    spans: Vec::new(),
                    row_source: group_first_source,
                });
                continue;
            }

            if new_cols == 1 {
                for (logical_col, cell) in logical_cells.iter().enumerate() {
                    if cell.flags.wide_continuation() {
                        continue;
                    }
                    let mut projected_cell = *cell;
                    projected_cell.flags.set_wide(false);
                    let spans = logical_spans
                        .iter()
                        .find(|span| {
                            (span.view_col_start..span.view_col_end).contains(&logical_col)
                        })
                        .map(|span| {
                            vec![LineOriginSpan {
                                view_col_start: 0,
                                view_col_end: 1,
                                raw_row: span.raw_row,
                                raw_col_start: span.raw_col_start + logical_col
                                    - span.view_col_start,
                                raw_absolute_row: span.raw_absolute_row,
                            }]
                        })
                        .unwrap_or_default();
                    result.push(ProjectedLine {
                        cells: vec![projected_cell],
                        row_source: spans.first().map(|span| RowSource {
                            raw_row: span.raw_row,
                            raw_absolute_row: span.raw_absolute_row,
                        }),
                        spans,
                    });
                }
                continue;
            }

            let mut offset = 0;
            while offset < logical_cells.len() {
                let mut end = (offset + new_cols).min(logical_cells.len());
                if end < logical_cells.len()
                    && logical_cells[end].flags.wide_continuation()
                    && end > offset
                {
                    end -= 1;
                }
                let spans: Vec<LineOriginSpan> = logical_spans
                    .iter()
                    .filter_map(|span| {
                        let overlap_start = span.view_col_start.max(offset);
                        let overlap_end = span.view_col_end.min(end);
                        (overlap_start < overlap_end).then(|| LineOriginSpan {
                            view_col_start: overlap_start - offset,
                            view_col_end: overlap_end - offset,
                            raw_row: span.raw_row,
                            raw_col_start: span.raw_col_start + overlap_start - span.view_col_start,
                            raw_absolute_row: span.raw_absolute_row,
                        })
                    })
                    .collect();
                let mut cells = logical_cells[offset..end].to_vec();
                cells.resize(new_cols, TerminalCell::default());
                let row_source = spans.first().map(|span| RowSource {
                    raw_row: span.raw_row,
                    raw_absolute_row: span.raw_absolute_row,
                });
                result.push(ProjectedLine {
                    cells,
                    spans,
                    row_source,
                });
                offset = end;
            }
        }

        result
    }

    fn live_view_provenance(&self, rows: usize, cols: usize) -> ProjectedProvenance {
        let mut origin_spans = Vec::with_capacity(rows);
        let mut row_sources = Vec::with_capacity(rows);
        for (view_row, raw_row) in self.grid.row_ids.iter().copied().take(rows).enumerate() {
            let source = raw_row.is_tracked().then_some(RowSource {
                raw_row,
                raw_absolute_row: self.scrollback.len() + view_row,
            });
            row_sources.push(source);
            if let Some(source) = source {
                origin_spans.push(OriginSpan {
                    view_row,
                    view_col_start: 0,
                    view_col_end: cols,
                    raw_row: source.raw_row,
                    raw_col_start: 0,
                });
            }
        }
        ProjectedProvenance {
            origin_spans,
            row_sources,
        }
    }

    fn materialize_scrolled_viewport(
        &self,
        rows: usize,
        cols: usize,
    ) -> (Vec<Vec<TerminalCell>>, ProjectedProvenance) {
        let mut start_idx = self
            .scrollback
            .len()
            .saturating_sub(self.scroll_offset + rows);
        while start_idx > 0 && self.scrollback[start_idx - 1].is_wrapped {
            start_idx -= 1;
        }
        let history: Vec<_> = self.scrollback.iter().skip(start_idx).cloned().collect();
        let reflowed = Self::reflow_projected_origins(&history, cols, start_idx);
        let skip = reflowed.len().saturating_sub(self.scroll_offset + rows);
        let visible_start = skip + (reflowed.len() - skip).saturating_sub(self.scroll_offset);
        let history_rows = (reflowed.len() - visible_start).min(rows);
        let mut cells: Vec<Vec<TerminalCell>> = reflowed[visible_start..]
            .iter()
            .take(rows)
            .map(|line| line.cells.clone())
            .collect();
        let mut row_sources: Vec<_> = reflowed[visible_start..]
            .iter()
            .take(rows)
            .map(|line| line.row_source)
            .collect();
        let mut spans = Vec::new();
        for (view_row, line) in reflowed[visible_start..].iter().take(rows).enumerate() {
            spans.extend(line.spans.iter().map(|span| OriginSpan {
                view_row,
                view_col_start: span.view_col_start,
                view_col_end: span.view_col_end,
                raw_row: span.raw_row,
                raw_col_start: span.raw_col_start,
            }));
        }

        for (grid_row, raw_row) in self.grid.row_ids.iter().copied().enumerate() {
            let view_row = history_rows + grid_row;
            if view_row >= rows {
                break;
            }
            cells.push(self.normalize_line_width(self.grid[grid_row].to_vec(), cols));
            let row_source = raw_row.is_tracked().then_some(RowSource {
                raw_row,
                raw_absolute_row: self.scrollback.len() + grid_row,
            });
            row_sources.push(row_source);
            if let Some(row_source) = row_source {
                spans.push(OriginSpan {
                    view_row,
                    view_col_start: 0,
                    view_col_end: cols,
                    raw_row: row_source.raw_row,
                    raw_col_start: 0,
                });
            }
        }

        while cells.len() < rows {
            cells.push(self.blank_line(cols));
            row_sources.push(None);
        }

        (
            cells,
            ProjectedProvenance {
                origin_spans: spans,
                row_sources,
            },
        )
    }

    /// Return the P0 identity history projection. Its cells are the exact Arc
    /// produced by the legacy visible-cell materializer; the only added data is
    /// stable, fail-closed raw provenance and revision metadata.
    pub fn get_projected_viewport(
        &mut self,
        block_mode: bool,
    ) -> std::sync::Arc<ProjectedViewport> {
        // Identity and bypass coordinates are raw-buffer coordinates. A
        // projected selection can never cross this boundary, while an
        // existing raw selection remains valid and must be preserved.
        self.projected_selection = None;
        let rows = self.grid.rows();
        let cols = if rows > 0 { self.grid.row_len() } else { 80 };
        let mode = if block_mode && !self.use_alt_buffer {
            ProjectionMode::Identity
        } else {
            ProjectionMode::Bypass
        };
        let key = ProjectedViewportCacheKey {
            grid_version: self.grid_version,
            history_revision: self.history_revision,
            row_identity_revision: self.grid.identity_revision,
            scroll_offset: self.scroll_offset,
            rows,
            cols,
            use_alt_buffer: self.use_alt_buffer,
            mode,
            policy_revision: 0,
            policy_ids: SmallVec::new(),
            view_scroll_offset: self.scroll_offset,
        };
        if let Some((cached_key, viewport)) = &self.projected_viewport_cache {
            if *cached_key == key {
                return std::sync::Arc::clone(viewport);
            }
        }

        let cells = self.get_visible_cells();
        let row_wrapped = std::sync::Arc::new(self.get_visible_row_wrapped());
        let provenance = if self.scroll_offset == 0 {
            std::sync::Arc::new(self.live_view_provenance(rows, cols))
        } else {
            self.visible_cells_cache
                .as_ref()
                .and_then(|(_, _, _, provenance)| provenance.as_ref())
                .map(std::sync::Arc::clone)
                .expect("scrolled visible cache includes projection provenance")
        };
        debug_assert_eq!(provenance.row_sources.len(), cells.len());
        debug_assert!(provenance.origin_spans.iter().all(|span| {
            span.view_row < cells.len()
                && span.view_col_start < span.view_col_end
                && span.view_col_end <= cells[span.view_row].len()
                && span.raw_row.is_tracked()
        }));
        let mut raw_span_index: Vec<_> = (0..provenance.origin_spans.len()).collect();
        raw_span_index.sort_unstable_by_key(|index| {
            let span = provenance.origin_spans[*index];
            (
                span.raw_row,
                span.raw_col_start,
                span.view_row,
                span.view_col_start,
            )
        });
        // Cell spans cover every non-empty raw row represented on a display
        // row, including a later raw-row suffix/prefix after reflow. Row
        // sources add the empty-line case without making structural padding
        // selectable. Sorting and deduplication keep lookup logarithmic and
        // storage proportional to row/span count rather than cell count.
        let mut raw_row_index: Vec<_> =
            provenance
                .origin_spans
                .iter()
                .map(|span| (span.raw_row, span.view_row))
                .chain(provenance.row_sources.iter().enumerate().filter_map(
                    |(view_row, source)| source.map(|source| (source.raw_row, view_row)),
                ))
                .collect();
        raw_row_index.sort_unstable();
        raw_row_index.dedup();

        let view_revision = self.next_projected_view_revision;
        if view_revision != 0 {
            self.next_projected_view_revision = view_revision.checked_add(1).unwrap_or(0);
        }
        let viewport = std::sync::Arc::new(ProjectedViewport {
            row_kinds: std::sync::Arc::new(vec![ProjectedRowKind::Raw; cells.len()]),
            cells,
            row_wrapped,
            provenance,
            raw_span_index: std::sync::Arc::new(raw_span_index),
            raw_row_index: std::sync::Arc::new(raw_row_index),
            source_revision: ProjectionSourceRevision {
                grid: self.grid_version,
                history: self.history_revision,
                row_identity: self.grid.identity_revision,
                alternate_screen: self.use_alt_buffer,
            },
            view_revision,
            identity_fast_path: self.scroll_offset == 0,
            mode,
            scroll_offset: self.scroll_offset,
            policy_revision: 0,
            policy_ids: std::sync::Arc::from([]),
            document_rows: self.scrollback.len().saturating_add(rows),
            document_start: self.viewport_absolute_start(),
            top_padding: 0,
            effective_collapsed: std::sync::Arc::new(BTreeSet::new()),
            plan_revision: 0,
        });
        if view_revision != 0 {
            self.projected_viewport_cache = Some((key, std::sync::Arc::clone(&viewport)));
        }
        viewport
    }

    fn materialize_projection_plan(
        &self,
        plan: &ProjectionPlan,
        requested_scroll_offset: usize,
        rows: usize,
        cols: usize,
    ) -> MaterializedProjection {
        let scroll_offset = requested_scroll_offset.min(plan.rows.len().saturating_sub(rows));
        let visible_end = plan.rows.len().saturating_sub(scroll_offset);
        let visible_start = visible_end.saturating_sub(rows);
        let visible_plan_rows = &plan.rows[visible_start..visible_end];
        let top_padding = rows.saturating_sub(visible_plan_rows.len());

        let mut cells = Vec::with_capacity(rows);
        let mut row_wrapped = Vec::with_capacity(rows);
        let mut row_kinds = Vec::with_capacity(rows);
        let mut row_sources = Vec::with_capacity(rows);
        let mut origin_spans = Vec::new();
        for _ in 0..top_padding {
            cells.push(vec![TerminalCell::default(); cols]);
            row_wrapped.push(false);
            row_kinds.push(ProjectedRowKind::Padding);
            row_sources.push(None);
        }

        let mut history_cache: HashMap<usize, Vec<TerminalCell>> = HashMap::new();
        for planned in visible_plan_rows {
            let view_row = cells.len();
            let mut line = vec![TerminalCell::default(); cols];
            if matches!(planned.kind, ProjectedRowKind::Raw) {
                for slice in &planned.raw_slices {
                    let source = if slice.source.absolute_row < self.scrollback.len() {
                        history_cache
                            .entry(slice.source.absolute_row)
                            .or_insert_with(|| {
                                self.scrollback[slice.source.absolute_row].decompress()
                            })
                            .as_slice()
                    } else {
                        let Some(grid_row) =
                            slice.source.absolute_row.checked_sub(self.scrollback.len())
                        else {
                            continue;
                        };
                        let Some(source) =
                            (grid_row < self.grid.rows()).then(|| &self.grid[grid_row])
                        else {
                            continue;
                        };
                        source
                    };
                    let source_end = slice.source.col_start.saturating_add(slice.len);
                    let view_end = slice.view_col_start.saturating_add(slice.len);
                    let Some(source_cells) = source.get(slice.source.col_start..source_end) else {
                        continue;
                    };
                    let Some(view_cells) = line.get_mut(slice.view_col_start..view_end) else {
                        continue;
                    };
                    view_cells.copy_from_slice(source_cells);
                    if slice.narrow_wide_body {
                        if let Some(cell) = line.get_mut(slice.view_col_start) {
                            cell.flags.set_wide(false);
                        }
                    }
                    if let Some(origin) = slice.origin {
                        origin_spans.push(OriginSpan {
                            view_row,
                            view_col_start: slice.view_col_start,
                            view_col_end: view_end,
                            raw_row: origin.row,
                            raw_col_start: origin.col_start,
                        });
                    }
                }
            }
            cells.push(line);
            row_wrapped.push(planned.wrapped);
            row_kinds.push(planned.kind);
            row_sources.push(
                matches!(planned.kind, ProjectedRowKind::Raw)
                    .then_some(planned.row_source)
                    .flatten(),
            );
        }
        while cells.len() < rows {
            cells.push(vec![TerminalCell::default(); cols]);
            row_wrapped.push(false);
            row_kinds.push(ProjectedRowKind::Padding);
            row_sources.push(None);
        }

        MaterializedProjection {
            cells,
            row_wrapped,
            row_kinds,
            provenance: ProjectedProvenance {
                origin_spans,
                row_sources,
            },
            scroll_offset,
            document_start: visible_start,
            top_padding,
        }
    }

    fn projection_indexes(
        provenance: &ProjectedProvenance,
    ) -> (Vec<usize>, Vec<(RawRowId, usize)>) {
        let mut raw_span_index: Vec<_> = (0..provenance.origin_spans.len()).collect();
        raw_span_index.sort_unstable_by_key(|index| {
            let span = provenance.origin_spans[*index];
            (
                span.raw_row,
                span.raw_col_start,
                span.view_row,
                span.view_col_start,
            )
        });
        let mut raw_row_index: Vec<_> =
            provenance
                .origin_spans
                .iter()
                .map(|span| (span.raw_row, span.view_row))
                .chain(provenance.row_sources.iter().enumerate().filter_map(
                    |(view_row, source)| source.map(|source| (source.raw_row, view_row)),
                ))
                .collect();
        raw_row_index.sort_unstable();
        raw_row_index.dedup();
        (raw_span_index, raw_row_index)
    }

    /// Materialize a viewport from a session-owned projection policy. Empty,
    /// stale, alternate-screen, and Block-disabled policies share the legacy
    /// P0 cell Arc; active collapses decode only raw rows referenced by the
    /// requested projected viewport slice.
    #[allow(dead_code)] // Session/UI wiring lands in the next slice.
    pub fn get_projected_viewport_with_policy(
        &mut self,
        block_mode: bool,
        policy: &ProjectionPolicy,
        view_scroll_offset: usize,
    ) -> std::sync::Arc<ProjectedViewport> {
        if !block_mode || self.use_alt_buffer || policy.is_identity() {
            return self.get_projected_viewport(block_mode);
        }

        let rows = self.grid.rows();
        let cols = self.grid.row_len().max(1);
        let cache_key = ProjectedViewportCacheKey {
            grid_version: self.grid_version,
            history_revision: self.history_revision,
            row_identity_revision: self.grid.identity_revision,
            scroll_offset: self.scroll_offset,
            rows,
            cols,
            use_alt_buffer: self.use_alt_buffer,
            mode: ProjectionMode::Transformed,
            policy_revision: policy.revision(),
            policy_ids: policy.ids(),
            view_scroll_offset,
        };
        if let Some((cached_key, viewport)) = &self.projected_viewport_cache {
            if *cached_key == cache_key {
                let viewport = std::sync::Arc::clone(viewport);
                self.enter_transformed_selection_space(viewport.plan_revision);
                return viewport;
            }
        }

        let Some(plan) = self.cached_collapsed_projection_plan(cols, policy) else {
            return self.get_projected_viewport(true);
        };
        self.enter_transformed_selection_space(plan.plan_revision);

        let document_rows = plan.rows.len();
        let effective_collapsed = std::sync::Arc::new(plan.effective_collapsed.clone());
        let materialized =
            self.materialize_projection_plan(plan.as_ref(), view_scroll_offset, rows, cols);
        let (raw_span_index, raw_row_index) = Self::projection_indexes(&materialized.provenance);
        let view_revision = self.next_projected_view_revision;
        if view_revision != 0 {
            self.next_projected_view_revision = view_revision.checked_add(1).unwrap_or(0);
        }
        let viewport = std::sync::Arc::new(ProjectedViewport {
            cells: std::sync::Arc::new(materialized.cells),
            row_wrapped: std::sync::Arc::new(materialized.row_wrapped),
            row_kinds: std::sync::Arc::new(materialized.row_kinds),
            provenance: std::sync::Arc::new(materialized.provenance),
            raw_span_index: std::sync::Arc::new(raw_span_index),
            raw_row_index: std::sync::Arc::new(raw_row_index),
            source_revision: ProjectionSourceRevision {
                grid: self.grid_version,
                history: self.history_revision,
                row_identity: self.grid.identity_revision,
                alternate_screen: self.use_alt_buffer,
            },
            view_revision,
            identity_fast_path: false,
            mode: ProjectionMode::Transformed,
            scroll_offset: materialized.scroll_offset,
            policy_revision: policy.revision(),
            policy_ids: std::sync::Arc::from(policy.collapsed_zone_ids().collect::<Vec<_>>()),
            document_rows,
            document_start: materialized.document_start,
            top_padding: materialized.top_padding,
            effective_collapsed,
            plan_revision: plan.plan_revision,
        });
        if view_revision != 0 {
            self.projected_viewport_cache = Some((cache_key, std::sync::Arc::clone(&viewport)));
        }
        viewport
    }

    /// Enter projected-document coordinates. Raw selections cannot describe
    /// a transformed document. A projected selection survives ordinary cell
    /// updates and viewport scrolling only while its exact plan is retained.
    fn enter_transformed_selection_space(&mut self, plan_revision: u64) {
        self.selection = None;
        if plan_revision == 0
            || self
                .projected_selection
                .as_ref()
                .is_some_and(|selection| selection.plan_revision != plan_revision)
        {
            self.projected_selection = None;
        }
    }

    fn projected_top_anchor(viewport: &ProjectedViewport) -> Option<ProjectedTopAnchor> {
        let view_row = viewport.top_padding;
        match viewport.row_kinds.get(view_row).copied()? {
            ProjectedRowKind::Padding => None,
            ProjectedRowKind::CollapsedSummary {
                key, hidden_range, ..
            } => Some(ProjectedTopAnchor::Summary {
                zone_id: key.zone_id,
                hidden_range,
            }),
            ProjectedRowKind::Raw => viewport
                .provenance
                .origin_spans
                .iter()
                .filter(|span| span.view_row == view_row)
                .min_by_key(|span| span.view_col_start)
                .map(|span| {
                    ProjectedTopAnchor::RawCell(RawCellOrigin {
                        row: span.raw_row,
                        col: span.raw_col_start,
                    })
                })
                .or_else(|| {
                    viewport
                        .provenance
                        .row_sources
                        .get(view_row)
                        .copied()
                        .flatten()
                        .map(|source| ProjectedTopAnchor::RawRow(source.raw_row))
                }),
        }
    }

    fn retained_absolute_for_raw(&self, raw_row: RawRowId) -> Option<usize> {
        self.scrollback
            .iter()
            .position(|line| line.raw_row_id() == raw_row)
            .or_else(|| {
                self.grid
                    .row_ids
                    .iter()
                    .position(|row| *row == raw_row)
                    .map(|grid_row| self.scrollback.len() + grid_row)
            })
    }

    fn restore_identity_scroll_from_projection(&mut self, state: &ProjectionViewState) {
        if state.follow_bottom {
            self.scroll_offset = 0;
            return;
        }
        let raw_row = match state.top_anchor {
            Some(ProjectedTopAnchor::RawCell(origin)) => Some(origin.row),
            Some(ProjectedTopAnchor::RawRow(row)) => Some(row),
            Some(ProjectedTopAnchor::Summary { hidden_range, .. }) => Some(hidden_range.start.row),
            None => None,
        };
        let Some(absolute) = raw_row.and_then(|row| self.retained_absolute_for_raw(row)) else {
            self.scroll_offset = self.scroll_offset.min(self.scrollback.len());
            return;
        };
        let desired_start = absolute.min(self.scrollback.len());
        self.scroll_offset = self.scrollback.len().saturating_sub(desired_start);
    }

    /// Resolve a session-owned projected scroll state against the current
    /// document. Rebuilds preserve the top raw/synthetic anchor while a view
    /// at offset zero continues following the bottom.
    #[allow(dead_code)] // Session wiring lands in the next slice.
    pub fn get_projected_viewport_with_state(
        &mut self,
        block_mode: bool,
        policy: &ProjectionPolicy,
        state: &mut ProjectionViewState,
    ) -> std::sync::Arc<ProjectedViewport> {
        if !block_mode || self.use_alt_buffer {
            self.projected_selection = None;
            return self.get_projected_viewport(block_mode);
        }
        if policy.is_identity() {
            if state.last_plan_key.is_some() {
                self.restore_identity_scroll_from_projection(state);
                state.last_plan_key = None;
            }
            return self.get_projected_viewport(true);
        }

        let cols = self.grid.row_len().max(1);
        if state.last_plan_key.is_none() && self.scroll_offset > 0 {
            let identity = self.get_projected_viewport(true);
            state.top_anchor = Self::projected_top_anchor(&identity);
            state.follow_bottom = false;
        }
        let plan_key = self.projection_plan_cache_key(cols, policy);
        let Some(plan) = self.cached_collapsed_projection_plan(cols, policy) else {
            if state.last_plan_key.is_some() {
                self.restore_identity_scroll_from_projection(state);
                state.last_plan_key = None;
            }
            return self.get_projected_viewport(true);
        };

        if state.last_plan_key.as_ref() != Some(&plan_key) {
            self.reanchor_projected_selection(&plan);
            let max_start = plan.rows.len().saturating_sub(self.grid.rows());
            if state.follow_bottom {
                state.offset_from_bottom = 0;
            } else if let Some(target) = state
                .top_anchor
                .and_then(|anchor| plan.document_row_for_anchor(anchor))
            {
                state.offset_from_bottom = max_start.saturating_sub(target.min(max_start));
            } else {
                state.offset_from_bottom = state.offset_from_bottom.min(max_start);
            }
        }

        let viewport =
            self.get_projected_viewport_with_policy(true, policy, state.offset_from_bottom);
        state.offset_from_bottom = viewport.scroll_offset();
        state.follow_bottom = state.offset_from_bottom == 0;
        state.top_anchor = (!state.follow_bottom)
            .then(|| Self::projected_top_anchor(&viewport))
            .flatten();
        state.last_plan_key = Some(plan_key);
        viewport
    }

    pub fn locate_raw_cell_in_projection(
        &self,
        projection: &ProjectedViewport,
        origin: RawCellOrigin,
    ) -> ProjectedRawCellLocation {
        if let Some(cell) = projection.raw_to_view(origin) {
            return ProjectedRawCellLocation::Visible(cell);
        }
        if projection.mode() != ProjectionMode::Transformed {
            return if self.retained_absolute_for_raw(origin.row).is_some() {
                ProjectedRawCellLocation::Retained
            } else {
                ProjectedRawCellLocation::Unmapped
            };
        }
        let Some((key, plan)) = self.projection_plan_cache.as_ref() else {
            return ProjectedRawCellLocation::Unmapped;
        };
        if !self.projection_plan_key_matches_current_source(key) {
            return ProjectedRawCellLocation::Unmapped;
        }
        if let Some(summary_row) = plan.summary_owning_raw_cell(origin) {
            if let ProjectedRowKind::CollapsedSummary { key, .. } = plan.rows[summary_row].kind {
                return ProjectedRawCellLocation::Hidden {
                    zone_id: key.zone_id,
                };
            }
        }
        if plan.raw_cell_document_row(origin).is_some()
            || plan.raw_row_document_row(origin.row).is_some()
        {
            ProjectedRawCellLocation::Retained
        } else {
            ProjectedRawCellLocation::Unmapped
        }
    }

    /// Move a transformed view to a retained raw cell without changing the
    /// collapse policy. Callers explicitly expand a hidden owner first, then
    /// invoke this method to reveal and highlight the same stable raw match.
    pub fn reveal_raw_cell_in_projection(
        &mut self,
        policy: &ProjectionPolicy,
        state: &mut ProjectionViewState,
        origin: RawCellOrigin,
    ) -> bool {
        if policy.is_identity() || self.use_alt_buffer {
            let Some(absolute) = self.retained_absolute_for_raw(origin.row) else {
                return false;
            };
            self.reveal_buffer_row(absolute);
            return true;
        }
        let cols = self.grid.row_len().max(1);
        let plan_key = self.projection_plan_cache_key(cols, policy);
        let Some(plan) = self.cached_collapsed_projection_plan(cols, policy) else {
            return false;
        };
        let Some(target) = plan
            .raw_cell_document_row(origin)
            .or_else(|| plan.raw_row_document_row(origin.row))
        else {
            return false;
        };
        let max_start = plan.rows.len().saturating_sub(self.grid.rows());
        state.offset_from_bottom = max_start.saturating_sub(target.min(max_start));
        state.follow_bottom = false;
        state.top_anchor = Some(ProjectedTopAnchor::RawCell(origin));
        state.last_plan_key = Some(plan_key);
        true
    }

    /// Reveal one effective collapsed summary without mutating the projection
    /// policy. This gives block navigation a stable destination even though
    /// every raw cell owned by the block is intentionally hidden.
    #[allow(dead_code)] // Public navigation hook; UI wiring lands separately.
    pub fn reveal_collapsed_summary(
        &mut self,
        policy: &ProjectionPolicy,
        state: &mut ProjectionViewState,
        zone_id: u64,
    ) -> bool {
        if policy.is_identity() || self.use_alt_buffer {
            return false;
        }
        let cols = self.grid.row_len().max(1);
        let plan_key = self.projection_plan_cache_key(cols, policy);
        let Some(plan) = self.cached_collapsed_projection_plan(cols, policy) else {
            return false;
        };
        let Some(summary_row) = plan.summary_row(zone_id) else {
            return false;
        };
        let ProjectedRowKind::CollapsedSummary {
            key, hidden_range, ..
        } = plan.rows[summary_row].kind
        else {
            return false;
        };
        if key.zone_id != zone_id || key.policy_revision != policy.revision() {
            return false;
        }

        let max_start = plan.rows.len().saturating_sub(self.grid.rows());
        state.offset_from_bottom = max_start.saturating_sub(summary_row.min(max_start));
        state.follow_bottom = false;
        state.top_anchor = Some(ProjectedTopAnchor::Summary {
            zone_id,
            hidden_range,
        });
        state.last_plan_key = Some(plan_key);
        true
    }

    pub fn get_visible_cells(&mut self) -> std::sync::Arc<Vec<Vec<TerminalCell>>> {
        if let Some((cached_version, cached_offset, ref cells, _)) = self.visible_cells_cache {
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
        let prev = self.visible_cells_cache.take();
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
                self.visible_cells_cache = Some((
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
        self.visible_cells_cache = Some((
            self.grid_version,
            self.scroll_offset,
            std::sync::Arc::clone(&arc),
            origin_spans,
        ));
        arc
    }

    pub fn get_cursor_pos(&self) -> (usize, usize) {
        (self.cursor_row, self.cursor_col)
    }

    /// 获取当前可见行的wrapped状态，用于跨行链接检测
    pub fn get_visible_row_wrapped(&self) -> Vec<bool> {
        let rows = self.grid.rows();

        if self.scroll_offset == 0 {
            // Fast path: just get current grid wrapped flags
            self.grid.row_wrapped.clone()
        } else {
            // Slow path: need to reconstruct from scrollback
            // For simplicity, when scrolling we disable wrapped link detection
            // by returning all false (can be improved later with full reflow)
            vec![false; rows]
        }
    }

    pub fn get_output(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.output_buffer)
    }

    #[inline]
    #[cfg(test)]
    fn viewport_row_to_absolute(&self, viewport_row: usize) -> usize {
        self.scrollback.len().saturating_sub(self.scroll_offset) + viewport_row
    }

    #[inline]
    fn projected_row_to_absolute(
        &self,
        projection: &ProjectedViewport,
        viewport_row: usize,
    ) -> usize {
        self.scrollback
            .len()
            .saturating_sub(projection.scroll_offset())
            + viewport_row
    }

    #[allow(dead_code)]
    pub fn select_text(&mut self, anchor: (usize, usize), active: (usize, usize)) {
        self.projected_selection = None;
        self.selection = Some(Selection {
            anchor,
            active,
            mode: SelectionMode::Normal,
        });
    }

    pub fn clear_text_selection(&mut self) {
        self.selection = None;
        self.projected_selection = None;
    }

    pub fn has_text_selection(&self) -> bool {
        self.selection.is_some() || self.projected_selection.is_some()
    }

    fn projection_selection_revision(&self, projection: &ProjectedViewport) -> Option<u64> {
        if projection.mode() != ProjectionMode::Transformed {
            return None;
        }
        let (key, plan) = self.projection_plan_cache.as_ref()?;
        (plan.plan_revision != 0
            && projection.plan_revision == plan.plan_revision
            && self.projection_plan_key_matches_current_source(key)
            && projection.source_revision.history == key.history_revision
            && projection.source_revision.row_identity == key.row_identity_revision
            && projection.source_revision.alternate_screen == self.use_alt_buffer
            && plan.rows.len() == projection.document_rows()
            && plan.policy_revision == projection.policy_revision())
        .then_some(plan.plan_revision)
    }

    fn projection_plan_key_matches_current_source(&self, key: &ProjectionPlanCacheKey) -> bool {
        key.history_revision == self.history_revision
            && key.row_identity_revision == self.grid.identity_revision
            && key.rows == self.grid.rows()
            && key.cols == self.grid.row_len().max(1)
            && key.row_wrapped.as_slice() == self.grid.row_wrapped.as_slice()
            && key.next_zone_id == self.next_zone_id
            && key.zone_count == self.command_zones.len()
            && key.provenance_count == self.finished_output_provenance.len()
    }

    /// Carry a projected text selection across a plan rebuild instead of
    /// destroying it.
    ///
    /// A rebuild renumbers every document row, so the stored coordinates go
    /// stale — which is why this used to just clear. But the scroll position is
    /// re-anchored across this very same rebuild a few lines below, through
    /// `document_row_for_anchor`, and a selection can use the same trick: each
    /// endpoint remembers the retained raw cell it was placed on and is
    /// resolved again against the incoming plan. Without this, one line of
    /// output while any block is collapsed wiped a drag-selection.
    ///
    /// Fails closed in every ambiguous case — the selection is dropped, which
    /// is exactly the old behavior — and refuses outright in three cases where
    /// surviving would be *worse* than dying:
    ///
    /// * Block (column) mode: only the endpoints are re-anchored, while the
    ///   rows between them are re-planned. A live grid row spans the full
    ///   width but the same row in scrollback stops at its real text, so the
    ///   rectangle would silently cover different characters.
    /// * A width change, which rewraps every history group for the same reason.
    /// * A change to the effectively-hidden set, which would let rows that just
    ///   became visible fall inside a selection the user never dragged over
    ///   them.
    ///
    /// The raw (non-projected) selection stays cleared unconditionally: its
    /// copy path reads scrollback directly with no collapse awareness, so it
    /// must never be revived here.
    fn reanchor_projected_selection(&mut self, plan: &ProjectionPlan) {
        self.selection = None;
        let Some(selection) = self.projected_selection.clone() else {
            return;
        };
        if selection.mode == SelectionMode::Block
            || selection.plan_cols != plan.cols
            || selection.hidden.as_ref() != &plan.effective_collapsed
        {
            self.projected_selection = None;
            return;
        }
        let (Some(anchor), Some(active)) = (
            plan.selection_point_for_anchor(selection.anchor.anchor),
            plan.selection_point_for_anchor(selection.active.anchor),
        ) else {
            self.projected_selection = None;
            return;
        };
        self.projected_selection = Some(ProjectedSelection {
            plan_revision: plan.plan_revision,
            anchor: ProjectedSelectionEndpoint {
                point: anchor,
                ..selection.anchor
            },
            active: ProjectedSelectionEndpoint {
                point: active,
                ..selection.active
            },
            ..selection
        });
    }

    /// Assemble a projected selection with the guards a later re-anchor needs:
    /// the width the endpoints were minted at, and the identity of the
    /// effectively-hidden set they were dragged across.
    fn new_projected_selection(
        projection: &ProjectedViewport,
        plan_revision: u64,
        anchor: ProjectedSelectionEndpoint,
        active: ProjectedSelectionEndpoint,
        mode: SelectionMode,
    ) -> ProjectedSelection {
        ProjectedSelection {
            plan_revision,
            plan_cols: projection.cells().first().map_or(0, |row| row.len()),
            hidden: std::sync::Arc::clone(&projection.effective_collapsed),
            anchor,
            active,
            mode,
        }
    }

    /// Mint one selection endpoint from a viewport position: its document
    /// coordinates in the current plan, plus the stable raw identity used to
    /// place it again after the plan is rebuilt.
    fn projected_selection_point(
        projection: &ProjectedViewport,
        viewport_pos: (usize, usize),
    ) -> Option<ProjectedSelectionEndpoint> {
        let mut cell = ViewportCell {
            row: viewport_pos.0,
            col: viewport_pos.1,
        };
        if projection
            .cells()
            .get(cell.row)?
            .get(cell.col)?
            .flags
            .wide_continuation()
            && cell.col > 0
        {
            cell.col -= 1;
        }
        // The raw origin was already computed here and thrown away; it is
        // exactly the identity the endpoint needs to survive a rebuild.
        let anchor = match projection.view_to_raw(cell) {
            Some(origin) => ProjectedSelectionAnchor::Cell(origin),
            None => {
                let span_start = projection
                    .provenance
                    .origin_spans
                    .partition_point(|span| span.view_row < cell.row);
                if projection.provenance.origin_spans[span_start..]
                    .first()
                    .is_some_and(|span| span.view_row == cell.row)
                {
                    return None;
                }
                let row_source = projection
                    .provenance
                    .row_sources
                    .get(cell.row)
                    .copied()
                    .flatten()?;
                if !row_source.raw_row.is_tracked()
                    || !matches!(
                        projection.row_kinds().get(cell.row),
                        Some(ProjectedRowKind::Raw)
                    )
                {
                    return None;
                }
                ProjectedSelectionAnchor::Row {
                    row: row_source.raw_row,
                    col: cell.col,
                }
            }
        };
        Some(ProjectedSelectionEndpoint {
            point: (projection.view_document_row(cell.row)?, cell.col),
            anchor,
        })
    }

    fn projected_row_real_column_bounds(
        projection: &ProjectedViewport,
        row: usize,
    ) -> Option<(usize, usize)> {
        let start = projection
            .provenance
            .origin_spans
            .partition_point(|span| span.view_row < row);
        let mut spans = projection.provenance.origin_spans[start..]
            .iter()
            .take_while(|span| span.view_row == row);
        let Some(first) = spans.next() else {
            let source = projection
                .provenance
                .row_sources
                .get(row)
                .copied()
                .flatten()?;
            return (source.raw_row.is_tracked()
                && matches!(projection.row_kinds().get(row), Some(ProjectedRowKind::Raw)))
            .then(|| {
                (
                    0,
                    projection
                        .cells()
                        .get(row)
                        .map_or(0, |cells| cells.len().saturating_sub(1)),
                )
            });
        };
        Some(spans.fold(
            (first.view_col_start, first.view_col_end.saturating_sub(1)),
            |(left, right), span| {
                (
                    left.min(span.view_col_start),
                    right.max(span.view_col_end.saturating_sub(1)),
                )
            },
        ))
    }

    /// Start a new selection at a viewport-relative position.
    /// Converts to absolute buffer coordinates internally.
    #[cfg(test)]
    pub fn start_selection(&mut self, viewport_pos: (usize, usize)) {
        self.start_selection_with_mode(viewport_pos, SelectionMode::Normal);
    }

    pub fn start_selection_in_projection(
        &mut self,
        projection: &ProjectedViewport,
        viewport_pos: (usize, usize),
        mode: SelectionMode,
    ) {
        if projection.mode() == ProjectionMode::Transformed {
            let Some(plan_revision) = self.projection_selection_revision(projection) else {
                self.clear_text_selection();
                return;
            };
            let Some(point) = Self::projected_selection_point(projection, viewport_pos) else {
                self.clear_text_selection();
                return;
            };
            self.selection = None;
            self.projected_selection = Some(Self::new_projected_selection(
                projection,
                plan_revision,
                point,
                point,
                mode,
            ));
            return;
        }
        self.projected_selection = None;
        let abs = (
            self.projected_row_to_absolute(projection, viewport_pos.0),
            viewport_pos.1,
        );
        self.selection = Some(Selection {
            anchor: abs,
            active: abs,
            mode,
        });
    }

    #[cfg(test)]
    fn start_selection_with_mode(&mut self, viewport_pos: (usize, usize), mode: SelectionMode) {
        let abs = (
            self.viewport_row_to_absolute(viewport_pos.0),
            viewport_pos.1,
        );
        self.selection = Some(Selection {
            anchor: abs,
            active: abs,
            mode,
        });
    }

    /// Update the active end of the current selection with a viewport-relative position.
    #[cfg(test)]
    pub fn update_selection(&mut self, viewport_pos: (usize, usize)) {
        let abs_row = self.viewport_row_to_absolute(viewport_pos.0);
        if let Some(ref mut sel) = self.selection {
            sel.active = (abs_row, viewport_pos.1);
        }
    }

    pub fn update_selection_in_projection(
        &mut self,
        projection: &ProjectedViewport,
        viewport_pos: (usize, usize),
    ) {
        if projection.mode() == ProjectionMode::Transformed {
            let Some(plan_revision) = self.projection_selection_revision(projection) else {
                self.clear_text_selection();
                return;
            };
            let Some(point) = Self::projected_selection_point(projection, viewport_pos) else {
                return;
            };
            let Some(selection) = self.projected_selection.as_mut() else {
                return;
            };
            if selection.plan_revision != plan_revision {
                self.clear_text_selection();
                return;
            }
            selection.active = point;
            return;
        }
        let abs_row = self.projected_row_to_absolute(projection, viewport_pos.0);
        if let Some(ref mut sel) = self.selection {
            sel.active = (abs_row, viewport_pos.1);
        }
    }

    /// Select the word at the given (row, col) position in the visible grid.
    /// Word boundaries are determined by character class: alphanumeric/underscore,
    /// whitespace, or punctuation/symbols.
    #[cfg(test)]
    pub fn select_word_at(&mut self, row: usize, col: usize) {
        let Some((abs_row, left, right)) = self.word_span_at(row, col) else {
            return;
        };

        self.selection = Some(Selection {
            anchor: (abs_row, left),
            active: (abs_row, right),
            mode: SelectionMode::Normal,
        });
    }

    pub fn select_word_in_projection(
        &mut self,
        projection: &ProjectedViewport,
        row: usize,
        col: usize,
    ) {
        let Some(line) = projection.cells().get(row) else {
            return;
        };
        let Some((left, right)) = Self::word_columns_at(line, col) else {
            return;
        };
        if projection.mode() == ProjectionMode::Transformed {
            let Some(plan_revision) = self.projection_selection_revision(projection) else {
                self.clear_text_selection();
                return;
            };
            let Some(anchor) = Self::projected_selection_point(projection, (row, left)) else {
                self.clear_text_selection();
                return;
            };
            let Some(active) = Self::projected_selection_point(projection, (row, right)) else {
                self.clear_text_selection();
                return;
            };
            self.selection = None;
            self.projected_selection = Some(Self::new_projected_selection(
                projection,
                plan_revision,
                anchor,
                active,
                SelectionMode::Normal,
            ));
            return;
        }
        self.projected_selection = None;
        let abs_row = self.projected_row_to_absolute(projection, row);
        self.selection = Some(Selection {
            anchor: (abs_row, left),
            active: (abs_row, right),
            mode: SelectionMode::Normal,
        });
    }

    #[cfg(test)]
    fn word_span_at(&mut self, row: usize, col: usize) -> Option<(usize, usize, usize)> {
        let visible = self.get_visible_cells();
        if row >= visible.len() {
            return None;
        }
        let line = &visible[row];
        let (left, right) = Self::word_columns_at(line, col)?;
        let abs_row = self.viewport_row_to_absolute(row);
        Some((abs_row, left, right))
    }

    fn word_columns_at(line: &[TerminalCell], col: usize) -> Option<(usize, usize)> {
        let cols = line.len();
        if col >= cols {
            return None;
        }

        // Skip wide_continuation to find the real character
        let mut start_col = col;
        if line[start_col].flags.wide_continuation() && start_col > 0 {
            start_col -= 1;
        }

        if let Some((left, right)) = Self::select_extended_token_span(line, start_col) {
            return Some((left, right));
        }

        let ch = line[start_col].character;
        let class = char_class(ch);

        // Expand left
        let mut left = start_col;
        while left > 0 {
            let prev = left - 1;
            let c = line[prev].character;
            if line[prev].flags.wide_continuation() {
                left = prev;
                continue;
            }
            if char_class(c) != class {
                break;
            }
            left = prev;
        }

        // Expand right
        let mut right = start_col;
        loop {
            let next = if line[right].flags.wide() {
                right + 2
            } else {
                right + 1
            };
            if next >= cols {
                break;
            }
            if line[next].flags.wide_continuation() {
                // shouldn't happen after a non-wide char, but skip
                if next + 1 < cols {
                    if char_class(line[next + 1].character) != class {
                        break;
                    }
                    right = next + 1;
                    continue;
                }
                break;
            }
            if char_class(line[next].character) != class {
                break;
            }
            right = next;
        }
        // If the selected end is a wide char, include its continuation cell
        if line[right].flags.wide() && right + 1 < cols {
            right += 1;
        }

        Some((left, right))
    }

    /// Extend an existing double-click selection using word boundaries.
    #[cfg(test)]
    pub fn extend_word_selection_to(&mut self, row: usize, col: usize) {
        let Some((target_row, target_left, target_right)) = self.word_span_at(row, col) else {
            return;
        };
        self.extend_word_selection_to_span(target_row, target_left, target_right);
    }

    pub fn extend_word_selection_in_projection(
        &mut self,
        projection: &ProjectedViewport,
        row: usize,
        col: usize,
    ) {
        let Some(line) = projection.cells().get(row) else {
            return;
        };
        let Some((target_left, target_right)) = Self::word_columns_at(line, col) else {
            return;
        };
        if projection.mode() == ProjectionMode::Transformed {
            let Some(plan_revision) = self.projection_selection_revision(projection) else {
                self.clear_text_selection();
                return;
            };
            let Some(target_start) =
                Self::projected_selection_point(projection, (row, target_left))
            else {
                return;
            };
            let Some(target_end) = Self::projected_selection_point(projection, (row, target_right))
            else {
                return;
            };
            let Some(selection) = self.projected_selection.as_mut() else {
                self.projected_selection = Some(Self::new_projected_selection(
                    projection,
                    plan_revision,
                    target_start,
                    target_end,
                    SelectionMode::Normal,
                ));
                return;
            };
            if selection.plan_revision != plan_revision {
                self.clear_text_selection();
                return;
            }
            let (origin_start, origin_end) = if selection.anchor.point <= selection.active.point {
                (selection.anchor, selection.active)
            } else {
                (selection.active, selection.anchor)
            };
            if target_start.point < origin_start.point {
                selection.anchor = origin_end;
                selection.active = target_start;
            } else {
                selection.anchor = origin_start;
                selection.active = target_end;
            }
            selection.mode = SelectionMode::Normal;
            return;
        }
        let target_row = self.projected_row_to_absolute(projection, row);
        self.extend_word_selection_to_span(target_row, target_left, target_right);
    }

    fn extend_word_selection_to_span(
        &mut self,
        target_row: usize,
        target_left: usize,
        target_right: usize,
    ) {
        let Some(sel) = self.selection else {
            self.selection = Some(Selection {
                anchor: (target_row, target_left),
                active: (target_row, target_right),
                mode: SelectionMode::Normal,
            });
            return;
        };

        let (origin_start, origin_end) = if sel.anchor <= sel.active {
            (sel.anchor, sel.active)
        } else {
            (sel.active, sel.anchor)
        };
        let target_start = (target_row, target_left);
        let target_end = (target_row, target_right);

        self.selection = Some(if target_start < origin_start {
            Selection {
                anchor: origin_end,
                active: target_start,
                mode: SelectionMode::Normal,
            }
        } else {
            Selection {
                anchor: origin_start,
                active: target_end,
                mode: SelectionMode::Normal,
            }
        });
    }

    /// Extend an existing triple-click selection to whole viewport rows.
    #[cfg(test)]
    pub fn extend_line_selection_to(&mut self, row: usize) {
        let Some(sel) = self.selection else {
            return;
        };
        let cols = self.grid.row_len();
        let target_row = self.viewport_row_to_absolute(row);
        let origin_row = sel.anchor.0;

        self.selection = Some(if target_row < origin_row {
            Selection {
                anchor: (origin_row, cols.saturating_sub(1)),
                active: (target_row, 0),
                mode: SelectionMode::Normal,
            }
        } else {
            Selection {
                anchor: (origin_row, 0),
                active: (target_row, cols.saturating_sub(1)),
                mode: SelectionMode::Normal,
            }
        });
    }

    pub fn extend_line_selection_in_projection(
        &mut self,
        projection: &ProjectedViewport,
        row: usize,
    ) {
        if projection.mode() == ProjectionMode::Transformed {
            let Some(plan_revision) = self.projection_selection_revision(projection) else {
                self.clear_text_selection();
                return;
            };
            let Some((left, right)) = Self::projected_row_real_column_bounds(projection, row)
            else {
                return;
            };
            let Some(target_start) = Self::projected_selection_point(projection, (row, left))
            else {
                return;
            };
            let Some(target_end) = Self::projected_selection_point(projection, (row, right)) else {
                return;
            };
            let Some(selection) = self.projected_selection.as_mut() else {
                return;
            };
            if selection.plan_revision != plan_revision {
                self.clear_text_selection();
                return;
            }
            let origin_row = selection.anchor.point.0;
            if target_start.point.0 < origin_row {
                let col = selection.anchor.point.1.max(selection.active.point.1);
                selection.anchor = selection.anchor.with_col(origin_row, col);
                selection.active = target_start;
            } else {
                let col = selection.anchor.point.1.min(selection.active.point.1);
                selection.anchor = selection.anchor.with_col(origin_row, col);
                selection.active = target_end;
            }
            selection.mode = SelectionMode::Normal;
            return;
        }
        let Some(sel) = self.selection else {
            return;
        };
        let cols = projection.cells().first().map_or(0, Vec::len);
        let target_row = self.projected_row_to_absolute(projection, row);
        let origin_row = sel.anchor.0;

        self.selection = Some(if target_row < origin_row {
            Selection {
                anchor: (origin_row, cols.saturating_sub(1)),
                active: (target_row, 0),
                mode: SelectionMode::Normal,
            }
        } else {
            Selection {
                anchor: (origin_row, 0),
                active: (target_row, cols.saturating_sub(1)),
                mode: SelectionMode::Normal,
            }
        });
    }

    pub fn select_line_in_projection(&mut self, projection: &ProjectedViewport, row: usize) {
        if projection.mode() != ProjectionMode::Transformed {
            let cols = projection.cells().first().map_or(0, Vec::len);
            self.projected_selection = None;
            let abs_row = self.projected_row_to_absolute(projection, row);
            self.selection = Some(Selection {
                anchor: (abs_row, 0),
                active: (abs_row, cols.saturating_sub(1)),
                mode: SelectionMode::Normal,
            });
            return;
        }
        let Some(plan_revision) = self.projection_selection_revision(projection) else {
            self.clear_text_selection();
            return;
        };
        let Some((left, right)) = Self::projected_row_real_column_bounds(projection, row) else {
            self.clear_text_selection();
            return;
        };
        let Some(anchor) = Self::projected_selection_point(projection, (row, left)) else {
            self.clear_text_selection();
            return;
        };
        let Some(active) = Self::projected_selection_point(projection, (row, right)) else {
            self.clear_text_selection();
            return;
        };
        self.selection = None;
        self.projected_selection = Some(Self::new_projected_selection(
            projection,
            plan_revision,
            anchor,
            active,
            SelectionMode::Normal,
        ));
    }

    fn select_extended_token_span(
        line: &[TerminalCell],
        start_col: usize,
    ) -> Option<(usize, usize)> {
        let cols = line.len();
        if start_col >= cols {
            return None;
        }

        let start_char = line[start_col].character;
        if !is_extended_token_char(start_char) {
            return None;
        }

        let mut left = start_col;
        while left > 0 {
            let prev = left - 1;
            if line[prev].flags.wide_continuation() {
                left = prev;
                continue;
            }
            if !is_extended_token_char(line[prev].character) {
                break;
            }
            left = prev;
        }

        let mut right = start_col;
        loop {
            let next = if line[right].flags.wide() {
                right + 2
            } else {
                right + 1
            };
            if next >= cols {
                break;
            }
            if line[next].flags.wide_continuation() {
                if next + 1 < cols && is_extended_token_char(line[next + 1].character) {
                    right = next + 1;
                    continue;
                }
                break;
            }
            if !is_extended_token_char(line[next].character) {
                break;
            }
            right = next;
        }

        while left < start_col && is_token_prefix_wrapper(line[left].character) {
            left += 1;
        }

        while right > start_col && is_token_suffix_wrapper(line[right].character) {
            right -= if line[right].flags.wide_continuation() && right > 0 {
                2
            } else {
                1
            };
        }

        if left > right || start_col < left || start_col > right {
            return None;
        }

        let mut has_alnum = false;
        let mut has_separator = false;
        for cell in &line[left..=right] {
            if cell.flags.wide_continuation() {
                continue;
            }
            let ch = cell.character;
            has_alnum |= ch.is_alphanumeric();
            has_separator |= is_extended_token_separator(ch);
        }

        if !has_alnum || !has_separator {
            return None;
        }

        if line[right].flags.wide() && right + 1 < cols {
            right += 1;
        }

        Some((left, right))
    }

    fn copy_projected_selection(&self) -> Option<String> {
        let selection = self.projected_selection.as_ref()?;
        let (plan_key, plan) = self.projection_plan_cache.as_ref()?;
        if plan.plan_revision == 0
            || plan.plan_revision != selection.plan_revision
            || !self.projection_plan_key_matches_current_source(plan_key)
        {
            return None;
        }
        let (start, end) = if selection.anchor.point <= selection.active.point {
            (selection.anchor.point, selection.active.point)
        } else {
            (selection.active.point, selection.anchor.point)
        };
        if start.0 >= plan.rows.len() || end.0 >= plan.rows.len() {
            return None;
        }

        let mut result = String::new();
        let mut history_cache: Option<(usize, Vec<TerminalCell>)> = None;
        let mut hard_break_pending = false;
        for document_row in start.0..=end.0 {
            let planned = &plan.rows[document_row];
            let (selected_left, selected_right) = if selection.mode == SelectionMode::Block {
                (
                    selection.anchor.point.1.min(selection.active.point.1),
                    selection.anchor.point.1.max(selection.active.point.1),
                )
            } else {
                (
                    if document_row == start.0 { start.1 } else { 0 },
                    if document_row == end.0 {
                        end.1
                    } else {
                        plan.cols.saturating_sub(1)
                    },
                )
            };

            if matches!(planned.kind, ProjectedRowKind::CollapsedSummary { .. }) {
                hard_break_pending = !result.ends_with('\n');
                continue;
            }
            if matches!(planned.kind, ProjectedRowKind::Raw) {
                if hard_break_pending {
                    result.push('\n');
                    hard_break_pending = false;
                }
                if planned.raw_slices.is_empty()
                    && planned.row_source.is_some()
                    && selected_left <= selected_right
                {
                    result.extend(std::iter::repeat_n(
                        ' ',
                        selected_right
                            .min(plan.cols.saturating_sub(1))
                            .saturating_sub(selected_left)
                            .saturating_add(1),
                    ));
                }
                for slice in &planned.raw_slices {
                    let slice_start = slice.view_col_start;
                    let slice_end = slice_start.saturating_add(slice.len);
                    let overlap_start = slice_start.max(selected_left);
                    let overlap_end = slice_end.min(selected_right.saturating_add(1));
                    if overlap_start >= overlap_end {
                        continue;
                    }
                    let source = if slice.source.absolute_row < self.scrollback.len() {
                        if history_cache.as_ref().map(|(row, _)| *row)
                            != Some(slice.source.absolute_row)
                        {
                            history_cache = Some((
                                slice.source.absolute_row,
                                self.scrollback[slice.source.absolute_row].decompress(),
                            ));
                        }
                        history_cache.as_ref()?.1.as_slice()
                    } else {
                        let grid_row = slice
                            .source
                            .absolute_row
                            .checked_sub(self.scrollback.len())?;
                        if grid_row >= self.grid.rows() {
                            return None;
                        }
                        &self.grid[grid_row]
                    };
                    let source_start = slice
                        .source
                        .col_start
                        .saturating_add(overlap_start - slice_start);
                    let source_end = source_start.saturating_add(overlap_end - overlap_start);
                    for cell in source.get(source_start..source_end)? {
                        if !cell.flags.wide_continuation() {
                            result.push(cell.character);
                        }
                    }
                }
                if document_row < end.0
                    && (selection.mode == SelectionMode::Block || !planned.wrapped)
                {
                    result.push('\n');
                    hard_break_pending = false;
                }
            }
        }
        Some(result)
    }

    pub fn copy_selection(&self) -> Option<String> {
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
            let grid_rows = self.grid.rows();
            let cols = self.grid.row_len();
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
                    grid_row < grid_rows && self.grid.row_wrapped[grid_row]
                };

                if abs_row < scrollback_len {
                    // Read from scrollback
                    let line = self.scrollback[abs_row].decompress();
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
                            let cell = self.grid.get(grid_row, col);
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

    pub fn scroll(&mut self, lines: isize) {
        // Don't scroll ordinary alternate-screen apps (less, vim, git log, etc.).
        // Synchronized TUIs such as Codex may archive snapshots into local
        // scrollback, in which case wheel/scrollbar navigation should work.
        if self.use_alt_buffer && self.scrollback.is_empty() {
            return;
        }

        if lines > 0 {
            // Scroll up (show earlier lines)
            self.scroll_offset = self.scroll_offset.saturating_add(lines as usize);
        } else {
            // Scroll down (show later lines)
            self.scroll_offset = self.scroll_offset.saturating_sub((-lines) as usize);
        }

        // Clamp scroll_offset to valid range
        let max_scroll = self.scrollback.len();
        self.scroll_offset = self.scroll_offset.min(max_scroll);

        // Back at the live bottom: a purge deferred mid-read now takes effect.
        self.settle_pending_saved_line_purge();
    }

    fn strip_trailing_blanks(cells: &[TerminalCell]) -> &[TerminalCell] {
        let mut end = cells.len();
        while end > 0
            && cells[end - 1].character == ' '
            && cells[end - 1].background == Color::Default
            && !cells[end - 1].flags.wide()
            && !cells[end - 1].flags.wide_continuation()
            && cells[end - 1].hyperlink == 0
        {
            end -= 1;
        }
        &cells[..end]
    }

    fn reflow_lines(
        lines: &[ScrollbackLine],
        new_cols: usize,
        blank_cell: &TerminalCell,
    ) -> Vec<ScrollbackLine> {
        let mut result = Vec::new();
        let len = lines.len();
        let mut i = 0;

        while i < len {
            let mut logical_line: Vec<TerminalCell> = Vec::new();
            let decompressed = lines[i].decompress();
            logical_line.extend_from_slice(Self::strip_trailing_blanks(&decompressed));
            while i < len && lines[i].is_wrapped {
                i += 1;
                if i < len {
                    let dc = lines[i].decompress();
                    logical_line.extend_from_slice(Self::strip_trailing_blanks(&dc));
                }
            }
            i += 1;

            if logical_line.is_empty() {
                result.push(ScrollbackLine::compress(
                    &vec![*blank_cell; new_cols],
                    false,
                ));
                continue;
            }

            if new_cols == 1 {
                // A two-cell glyph cannot fit. Keep its body as a narrow cell and
                // discard the continuation placeholder so rows remain valid.
                let group_start = result.len();
                for cell in logical_line
                    .iter()
                    .filter(|cell| !cell.flags.wide_continuation())
                {
                    let mut cell = *cell;
                    cell.flags.set_wide(false);
                    result.push(ScrollbackLine::compress(&[cell], true));
                }
                if result.len() > group_start {
                    if let Some(last) = result.last_mut() {
                        last.is_wrapped = false;
                    }
                }
                continue;
            }

            let mut offset = 0;
            while offset < logical_line.len() {
                let mut end = (offset + new_cols).min(logical_line.len());
                // Never split a wide body from its continuation at a row edge.
                if end < logical_line.len()
                    && logical_line[end].flags.wide_continuation()
                    && end > offset
                {
                    end -= 1;
                }
                let mut cells = logical_line[offset..end].to_vec();
                cells.resize(new_cols, *blank_cell);
                result.push(ScrollbackLine::compress(&cells, end < logical_line.len()));
                offset = end;
            }
        }

        result
    }

    /// Normalize retained history after a burst of width changes. This is kept
    /// separate from `on_resize` so window/divider drags can debounce the O(n)
    /// decompress/reflow/recompress pass instead of freezing every pointer step.
    /// Returns `false` while a reader is scrolled back, or when a live lifecycle
    /// already entered scrollback, and the caller must retry later; `true` when
    /// normalization is complete (including an empty history).
    pub fn normalize_scrollback_width(&mut self) -> bool {
        if self.scrollback.is_empty() {
            return true;
        }
        // Reflow changes the number of physical rows, so doing it underneath a
        // reader invalidates the viewport's distance-from-bottom anchor. Keep
        // the old-width rows (the viewport materializer pads/crops them safely)
        // until the user returns to the live bottom, then normalize once.
        if self.scroll_offset > 0 {
            return false;
        }
        let old_scrollback_len = self.scrollback.len();
        let live_start = match self.current_zone_state {
            ZoneState::PromptStarted(prompt_start)
            | ZoneState::CommandStarted(prompt_start, _)
            | ZoneState::OutputStarted(prompt_start, _, _) => Some(prompt_start),
            ZoneState::Idle => None,
        };
        // Once a long-running block's prompt has entered scrollback, changing
        // physical wrapping would require mapping anchors inside a logical
        // line. Defer that optional history cleanup instead of severing the
        // running lifecycle (and Agent execution correlation) mid-command.
        if live_start.is_some_and(|start| start < old_scrollback_len) {
            self.clear_text_selection();
            return false;
        }
        let cols = self.grid.row_len().max(1);
        // Historical padding must stay neutral even when a live application has
        // a non-default background active at the instant the resize settles.
        let blank_cell = TerminalCell::default();
        let source: Vec<_> = self.scrollback.drain(..).collect();
        self.scrollback = Self::reflow_lines(&source, cols, &blank_cell).into();
        while self.scrollback.len() > self.max_scrollback {
            self.scrollback.pop_front();
        }
        self.bump_history_revision();
        let new_scrollback_len = self.scrollback.len();
        self.kitty_graphics
            .on_scrollback_reflow(old_scrollback_len, new_scrollback_len);
        let translate_grid_row =
            |row: usize| new_scrollback_len.saturating_add(row.saturating_sub(old_scrollback_len));
        self.scroll_offset = 0;
        // Selection endpoints cannot be mapped through arbitrary reflow. Block
        // metadata can: zones whose prompt was in reflowed history lose only
        // their row anchors while keeping identity/metadata/output snapshots;
        // zones and a live lifecycle still in the unchanged grid shift by the
        // new scrollback prefix length.
        self.clear_text_selection();
        // Reflow evicts on a different boundary than a row trim, so this loop
        // owns its own flag rather than sharing one with the trim path.
        let mut evicted_any = false;
        for zone in &mut self.command_zones {
            if zone.rows_evicted {
                continue;
            }
            if zone.prompt_start < old_scrollback_len {
                zone.rows_evicted = true;
                evicted_any = true;
                zone.prompt_start = 0;
                zone.command_start = None;
                zone.output_start = None;
                zone.output_start_col = 0;
                zone.output_end = None;
            } else {
                zone.prompt_start = translate_grid_row(zone.prompt_start);
                for row in [
                    zone.command_start.as_mut(),
                    zone.output_start.as_mut(),
                    zone.output_end.as_mut(),
                ]
                .into_iter()
                .flatten()
                {
                    *row = translate_grid_row(*row);
                }
            }
        }
        if evicted_any {
            self.drop_orphaned_output_provenance();
        }
        self.current_zone_state = match std::mem::take(&mut self.current_zone_state) {
            ZoneState::Idle => ZoneState::Idle,
            ZoneState::PromptStarted(prompt_start) => {
                ZoneState::PromptStarted(translate_grid_row(prompt_start))
            }
            ZoneState::CommandStarted(prompt_start, command_start) => ZoneState::CommandStarted(
                translate_grid_row(prompt_start),
                translate_grid_row(command_start),
            ),
            ZoneState::OutputStarted(prompt_start, command_start, output_start) => {
                ZoneState::OutputStarted(
                    translate_grid_row(prompt_start),
                    translate_grid_row(command_start),
                    translate_grid_row(output_start),
                )
            }
        };
        self.current_command_extent_row = self.current_command_extent_row.map(translate_grid_row);
        self.current_output_extent_row = self.current_output_extent_row.map(translate_grid_row);
        if let Some(pending) = self.idle_background_output.as_mut() {
            pending.start_row = translate_grid_row(pending.start_row);
            pending.last_row = translate_grid_row(pending.last_row);
        }
        self.grid_version = self.grid_version.wrapping_add(1);
        self.visible_cells_cache = None;
        true
    }

    pub fn on_resize(&mut self, cols: usize, rows: usize) {
        if cols == 0 || rows == 0 {
            return;
        }

        let (cols, rows) = clamp_terminal_dimensions(cols, rows);
        let old_rows = self.grid.rows();
        let had_full_screen_region = old_rows == 0
            || (self.scroll_region_top == 0 && self.scroll_region_bottom + 1 >= old_rows);

        let active_blank_cell = self.create_blank_cell();
        let primary_blank_bg = if self.use_alt_buffer {
            self.saved_primary_screen_state
                .map(|state| state.bg)
                .unwrap_or(Color::Default)
        } else {
            self.current_bg
        };
        let primary_blank_cell = self.blank_cell_with_bg(primary_blank_bg);
        let alt_blank_cell = active_blank_cell;

        // When the row count shrinks on the primary screen, mirror a real
        // terminal: push the oldest on-screen lines into scrollback and shift the
        // rest up, rather than letting TerminalGrid::resize silently truncate the
        // BOTTOM rows (where the prompt/cursor usually live). The cursor is kept
        // on-screen. (Column reflow on width change is not done here.)
        if !self.use_alt_buffer && old_rows > rows {
            let to_remove = old_rows - rows;
            // Take as many rows off the top as possible without scrolling the
            // cursor above row 0; any remainder is truncated from the bottom.
            let top_remove = to_remove.min(self.cursor_row);
            if top_remove > 0 {
                let cols_now = self.grid.row_len();
                for r in 0..top_remove {
                    let line = ScrollbackLine::compress_with_raw_row_id(
                        &self.grid[r],
                        self.grid.row_wrapped[r],
                        self.grid.row_ids[r],
                    );
                    self.push_scrollback_compressed(line);
                }
                let src_start = top_remove * cols_now;
                let total = old_rows * cols_now;
                self.grid.cells.copy_within(src_start..total, 0);
                self.grid.row_wrapped.copy_within(top_remove..old_rows, 0);
                self.grid.row_ids.copy_within(top_remove..old_rows, 0);
                self.grid.bump_identity_revision();
                self.cursor_row -= top_remove;
                self.saved_cursor_row = self.saved_cursor_row.saturating_sub(top_remove);
            }
        }

        if self.use_alt_buffer {
            self.grid.resize(rows, cols, alt_blank_cell);
            self.alt_grid.resize(rows, cols, primary_blank_cell);
        } else {
            self.grid.resize(rows, cols, active_blank_cell);
            self.alt_grid.resize(rows, cols, active_blank_cell);
        }

        // CRITICAL: Sync row_versions size with grid size to prevent dirty mark loss
        // When grid grows, we need to extend row_versions; when it shrinks, truncate it
        if rows != self.row_versions.len() {
            self.row_versions.resize(rows, self.grid_version);
        }

        // Keep the tab-stop table sized to the new column count, defaulting any
        // newly added columns to the standard every-8 stops.
        if cols != self.tab_stops.len() {
            let old_len = self.tab_stops.len();
            self.tab_stops.resize(cols, false);
            for c in old_len..cols {
                self.tab_stops[c] = c % 8 == 0 && c != 0;
            }
        }

        // Resizing the live grid must not cancel a user's scrollback read. A
        // row shrink may have pushed grid rows into history above, and the
        // push helper already increased the offset to keep the same top row.
        // Growing or changing width leaves the retained-history distance valid.
        self.scroll_offset = self.scroll_offset.min(self.scrollback.len());
        self.cursor_row = self.cursor_row.min(rows.saturating_sub(1));
        self.cursor_col = self.cursor_col.min(cols.saturating_sub(1));
        // Width resize mutates grid rows, but retained scrollback keeps its old
        // width until the deferred normalization pass. Clamp a C-row column
        // only while that row is still in the resized (possibly hidden
        // primary) grid.
        if matches!(
            self.current_zone_state,
            ZoneState::OutputStarted(_, _, output_start)
                if output_start >= self.scrollback.len()
        ) {
            self.current_output_start_col = self
                .current_output_start_col
                .map(|col| col.min(cols.saturating_sub(1)));
        }
        let last_buffer_row = self.scrollback.len().saturating_add(rows).saturating_sub(1);
        self.current_output_extent_row = self
            .current_output_extent_row
            .map(|row| row.min(last_buffer_row));
        self.pending_wrap = false;
        self.saved_cursor_row = self.saved_cursor_row.min(rows.saturating_sub(1));
        self.saved_cursor_col = self.saved_cursor_col.min(cols.saturating_sub(1));
        self.alt_cursor_row = self.alt_cursor_row.min(rows.saturating_sub(1));
        self.alt_cursor_col = self.alt_cursor_col.min(cols.saturating_sub(1));
        if had_full_screen_region {
            self.scroll_region_top = 0;
            self.scroll_region_bottom = rows.saturating_sub(1);
        } else {
            self.scroll_region_top = self.scroll_region_top.min(rows.saturating_sub(1));
            self.scroll_region_bottom = self.scroll_region_bottom.min(rows.saturating_sub(1));

            if self.scroll_region_top > self.scroll_region_bottom {
                self.scroll_region_top = 0;
                self.scroll_region_bottom = rows.saturating_sub(1);
            }
        }

        // Resizing mutates every structural assumption behind the visible-cell
        // cache. Bump the version and invalidate explicitly; otherwise a cached
        // pre-resize Arc can be returned with stale row/column dimensions.
        self.grid_version = self.grid_version.wrapping_add(1);
        self.row_versions.fill(self.grid_version);
        self.visible_cells_cache = None;
    }

    pub fn get_dimensions(&self) -> (usize, usize) {
        if self.grid.is_empty() {
            (0, 0)
        } else {
            (self.grid.row_len(), self.grid.rows())
        }
    }

    #[inline]
    #[cfg(test)]
    pub fn row_selection_cols(&self, viewport_row: usize) -> Option<(usize, usize)> {
        let abs_row = self.viewport_row_to_absolute(viewport_row);
        self.selection_cols_at_absolute_row(abs_row)
    }

    #[inline]
    pub fn row_selection_cols_in_projection(
        &self,
        projection: &ProjectedViewport,
        viewport_row: usize,
    ) -> Option<(usize, usize)> {
        if projection.mode() == ProjectionMode::Transformed {
            let selection = self.projected_selection.as_ref()?;
            let (plan_key, plan) = self.projection_plan_cache.as_ref()?;
            if plan.plan_revision == 0
                || plan.plan_revision != selection.plan_revision
                || projection.plan_revision != selection.plan_revision
                || !self.projection_plan_key_matches_current_source(plan_key)
                || !matches!(
                    projection.row_kinds().get(viewport_row),
                    Some(ProjectedRowKind::Raw)
                )
            {
                return None;
            }
            let document_row = projection.view_document_row(viewport_row)?;
            let (start, end) = if selection.anchor.point <= selection.active.point {
                (selection.anchor.point, selection.active.point)
            } else {
                (selection.active.point, selection.anchor.point)
            };
            if document_row < start.0 || document_row > end.0 {
                return None;
            }
            let (mut left, mut right) = if selection.mode == SelectionMode::Block {
                (
                    selection.anchor.point.1.min(selection.active.point.1),
                    selection.anchor.point.1.max(selection.active.point.1),
                )
            } else {
                (
                    if document_row == start.0 { start.1 } else { 0 },
                    if document_row == end.0 {
                        end.1
                    } else {
                        usize::MAX
                    },
                )
            };
            let (real_left, real_right) =
                Self::projected_row_real_column_bounds(projection, viewport_row)?;
            left = left.max(real_left);
            right = right.min(real_right);
            if projection
                .cells()
                .get(viewport_row)
                .and_then(|cells| cells.get(right))
                .is_some_and(|cell| cell.flags.wide())
            {
                right = right.saturating_add(1).min(real_right);
            }
            return (left <= right).then_some((left, right));
        }
        let abs_row = self.projected_row_to_absolute(projection, viewport_row);
        self.selection_cols_at_absolute_row(abs_row)
    }

    fn selection_cols_at_absolute_row(&self, abs_row: usize) -> Option<(usize, usize)> {
        let sel = self.selection?;
        let (start, end) = if sel.anchor <= sel.active {
            (sel.anchor, sel.active)
        } else {
            (sel.active, sel.anchor)
        };

        if abs_row < start.0 || abs_row > end.0 {
            return None;
        }

        match sel.mode {
            SelectionMode::Block => {
                let col_min = sel.anchor.1.min(sel.active.1);
                let col_max = sel.anchor.1.max(sel.active.1);
                Some((col_min, col_max))
            }
            SelectionMode::Normal => {
                let col_start = if abs_row == start.0 { start.1 } else { 0 };
                let col_end = if abs_row == end.0 { end.1 } else { usize::MAX };
                Some((col_start, col_end))
            }
        }
    }

    // IME support methods
    pub fn set_preedit(&mut self, text: String, selection: Option<std::ops::Range<usize>>) {
        self.preedit_text = text;
        self.preedit_selection = selection;
    }

    pub fn clear_preedit(&mut self) {
        self.preedit_text.clear();
        self.preedit_selection = None;
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn osc_133_requires_an_exact_marker_field() {
        let mut terminal = super::TerminalState::new(40, 8);

        terminal.process_input(b"\x1b]133;Attack\x07");
        assert_eq!(terminal.live_prompt_row(), None);

        terminal.process_input(b"\x1b]133;A\x07$ ");
        terminal.process_input(b"\x1b]133;Bogus\x07");
        assert_eq!(
            terminal.agent_prompt_status(),
            super::AgentPromptStatus::ShellIntegrationUnavailable
        );

        terminal.process_input(b"\x1b]133;B\x07echo ok\r\n");
        terminal.process_input(b"\x1b]133;Command\x07");
        assert!(!terminal.is_command_running());

        terminal.process_input(b"\x1b]133;C;id=run-1\x07ok");
        terminal.process_input(b"\x1b]133;Danger;0;id=run-1\x07");
        assert!(terminal.is_command_running());
        assert!(terminal.command_zones.is_empty());
    }

    #[test]
    fn ordinary_osc_133_lifecycle_rejects_a_mismatched_d_id_without_consuming_state() {
        let mut terminal = super::TerminalState::new(40, 8);
        terminal.process_input(
            b"\x1b]133;A\x07$ \x1b]133;B\x07echo ok\r\n\x1b]133;C;id=run-1\x07ok\r\n",
        );

        terminal.process_input(b"\x1b]133;D;0;id=spoof\x07");
        assert!(terminal.is_command_running());
        assert!(terminal.command_zones.is_empty());
        assert!(terminal.take_completed_commands().is_empty());

        terminal.process_input(b"\x1b]133;D;0;id=run-1\x07");
        assert!(!terminal.is_command_running());
        assert_eq!(terminal.command_zones.len(), 1);
        let completed = terminal.take_completed_commands();
        assert_eq!(completed.len(), 1);
        assert_eq!(completed[0].id.as_deref(), Some("run-1"));
    }

    #[test]
    fn explicit_invalid_d_ids_never_fall_back_to_the_live_command() {
        let mut terminal = super::TerminalState::new(40, 8);
        terminal.process_input(
            b"\x1b]133;A;id=run-1\x07$ \x1b]133;B;id=run-1\x07echo ok\r\n\x1b]133;C;id=run-1\x07ok\r\n",
        );

        for invalid in ["", "bad%ZZ"] {
            terminal.process_input(format!("\x1b]133;D;0;id={invalid}\x07").as_bytes());
            assert!(terminal.is_command_running(), "accepted {invalid:?}");
            assert!(terminal.command_zones.is_empty());
            assert!(terminal.take_completed_commands().is_empty());
        }
        let oversized = "x".repeat(193);
        terminal.process_input(format!("\x1b]133;D;0;id={oversized}\x07").as_bytes());
        assert!(terminal.is_command_running());
        assert!(terminal.command_zones.is_empty());
        assert!(terminal.take_completed_commands().is_empty());

        terminal.process_input(b"\x1b]133;D;0;id=run-1\x07");
        assert_eq!(terminal.take_completed_commands().len(), 1);
    }

    #[test]
    fn ordinary_d_id_falls_back_to_a_b_identity_when_c_omits_it() {
        let mut terminal = super::TerminalState::new(40, 8);
        terminal.process_input(b"\x1b]133;A;id=run-7\x07$ \x1b]133;B;id=run-7\x07");
        terminal.process_input(b"echo ok\r\n\x1b]133;C;cmdline_url=echo%20ok\x07ok\r\n");

        terminal.process_input(b"\x1b]133;D;0;id=other\x07");
        assert!(terminal.is_command_running());
        assert!(terminal.take_completed_commands().is_empty());

        terminal.process_input(b"\x1b]133;D;0;id=run-7\x07");
        let completed = terminal.take_completed_commands();
        assert_eq!(completed.len(), 1);
        assert_eq!(completed[0].id.as_deref(), Some("run-7"));
        assert_eq!(completed[0].agent_generation, None);
    }

    #[test]
    fn agent_with_anonymous_c_rejects_even_the_a_b_id_until_anonymous_d() {
        let mut terminal = super::TerminalState::new(40, 8);
        terminal.process_input(b"\x1b]133;A;id=run-8\x07$ \x1b]133;B;id=run-8\x07");
        terminal
            .arm_agent_execution(8, "echo ok")
            .expect("fresh prompt is ready");
        terminal.process_input(b"echo ok\r\n\x1b]133;C;cmdline_url=echo%20ok\x07ok\r\n");

        terminal.process_input(b"\x1b]133;D;0;id=run-8\x07");
        assert!(terminal.is_command_running());
        assert!(terminal.take_completed_commands().is_empty());

        terminal.process_input(b"\x1b]133;D;0\x07");
        let completed = terminal.take_completed_commands();
        assert_eq!(completed.len(), 1);
        assert_eq!(completed[0].id.as_deref(), Some("run-8"));
        assert_eq!(completed[0].agent_generation, Some(8));
    }

    #[test]
    fn consumed_d_id_cannot_be_adopted_by_a_later_anonymous_command() {
        let mut terminal = super::TerminalState::new(40, 8);
        terminal.process_input(
            b"\x1b]133;A;id=run-1\x07$ \x1b]133;B;id=run-1\x07one\r\n\x1b]133;C;id=run-1\x07one\x1b]133;D;0;id=run-1\x07",
        );
        assert_eq!(terminal.take_completed_commands().len(), 1);

        terminal.process_input(b"\x1b]133;A\x07$ \x1b]133;B\x07two\r\n\x1b]133;C\x07two");
        terminal.process_input(b"\x1b]133;D;0;id=run-1\x07");
        assert!(terminal.is_command_running());
        assert!(terminal.take_completed_commands().is_empty());

        terminal.process_input(b"\x1b]133;D;0\x07");
        let completed = terminal.take_completed_commands();
        assert_eq!(completed.len(), 1);
        assert_eq!(completed[0].command, "two");
        assert_eq!(completed[0].id, None);
    }

    #[test]
    fn consumed_execution_id_authority_is_bounded_to_the_recent_window() {
        let mut terminal = super::TerminalState::new(8, 2);
        for index in 0..=super::MAX_CONSUMED_EXECUTION_IDS {
            terminal.remember_consumed_execution_id(Some(&format!("run-{index}")));
        }
        assert_eq!(
            terminal.consumed_execution_ids.len(),
            super::MAX_CONSUMED_EXECUTION_IDS
        );
        assert!(!terminal.execution_id_was_consumed("run-0"));
        assert!(terminal
            .execution_id_was_consumed(&format!("run-{}", super::MAX_CONSUMED_EXECUTION_IDS)));
    }

    #[test]
    fn anonymous_active_agent_rejects_a_late_named_d_and_keeps_its_generation() {
        let mut terminal = super::TerminalState::new(40, 8);
        terminal.process_input(
            b"\x1b]133;A;id=run-1\x07$ \x1b]133;B;id=run-1\x07one\r\n\x1b]133;C;id=run-1\x07one\x1b]133;D;0;id=run-1\x07",
        );
        terminal.take_completed_commands();

        terminal.process_input(b"\x1b]133;A\x07$ \x1b]133;B\x07");
        terminal
            .arm_agent_execution(8, "echo safe")
            .expect("anonymous prompt is ready");
        terminal.process_input(b"echo safe\r\n\x1b]133;C;cmdline_url=echo%20safe\x07safe");

        terminal.process_input(b"\x1b]133;D;0;id=run-1\x07");
        assert!(terminal.is_command_running());
        assert!(terminal.take_completed_commands().is_empty());

        terminal.process_input(b"\x1b]133;D;0\x07");
        let completed = terminal.take_completed_commands();
        assert_eq!(completed.len(), 1);
        assert_eq!(completed[0].command, "echo safe");
        assert_eq!(completed[0].id, None);
        assert_eq!(completed[0].agent_generation, Some(8));
    }

    #[test]
    fn osc_133_visible_command_capture_is_bounded_without_becoming_background() {
        fn run(command: &str) -> super::TerminalState {
            let mut terminal = super::TerminalState::new(40, 8);
            terminal.process_input(b"\x1b]133;A\x07$ \x1b]133;B\x07");
            terminal.process_input(command.as_bytes());
            terminal.process_input(b"\r\n\x1b]133;C\x07out\r\n\x1b]133;D;0\x07");
            terminal
        }

        // Reaching the cap exactly is still a complete capture.
        let exact = "x".repeat(super::MAX_CAPTURED_COMMAND_BYTES);
        let mut terminal = run(&exact);
        let zone = terminal.command_zones.back().expect("exact command zone");
        assert_eq!(zone.command.as_deref(), Some(exact.as_str()));
        assert!(!zone.command_truncated);
        assert_eq!(terminal.take_completed_commands().len(), 1);

        // One byte beyond it retains an exact safe prefix and explicitly
        // remains a command lifecycle. The incomplete prefix is not emitted
        // to executable-looking CompletedCommand consumers.
        let oversized = format!("echo {}", "x".repeat(super::MAX_CAPTURED_COMMAND_BYTES));
        let mut terminal = run(&oversized);
        let zone = terminal
            .command_zones
            .back()
            .expect("oversized command zone");
        let command = zone.command.as_deref().expect("bounded command identity");
        assert_eq!(command.len(), super::MAX_CAPTURED_COMMAND_BYTES);
        assert_eq!(command, &oversized[..super::MAX_CAPTURED_COMMAND_BYTES]);
        assert!(zone.command_truncated);
        assert!(!matches!(
            crate::block_mode::classify(zone.command.as_deref(), zone.exit_code),
            crate::block_mode::BlockOutcome::Background
        ));
        assert!(terminal.take_completed_commands().is_empty());
    }

    #[test]
    fn osc_133_long_command_metadata_keeps_a_utf8_safe_prefix() {
        let prefix = "x".repeat(super::MAX_CAPTURED_COMMAND_BYTES - 1);
        // The first byte of the encoded emoji reaches the byte cap. Capture
        // must back up to the preceding UTF-8 boundary, not reject the whole
        // command and silently classify the C lifecycle as Background.
        let c_mark = format!("\x1b]133;C;cmdline_url={prefix}%F0%9F%98%80\x07");
        let mut terminal = super::TerminalState::new(40, 8);
        terminal.process_input(b"\x1b]133;A\x07$ \x1b]133;B\x07");
        terminal.process_input(c_mark.as_bytes());
        terminal.process_input(b"out\r\n\x1b]133;D;0\x07");

        let zone = terminal
            .command_zones
            .back()
            .expect("metadata command zone");
        assert_eq!(zone.command.as_deref(), Some(prefix.as_str()));
        assert!(zone.command_truncated);
        assert!(!matches!(
            crate::block_mode::classify(zone.command.as_deref(), zone.exit_code),
            crate::block_mode::BlockOutcome::Background
        ));
        assert!(terminal.take_completed_commands().is_empty());
    }

    #[test]
    fn exhausted_block_ids_seal_history_instead_of_reusing_an_identity() {
        let mut terminal = super::TerminalState::new(40, 8);
        terminal.next_zone_id = u64::MAX;
        terminal.process_input(
            b"\x1b]133;A\x07$ \x1b]133;B\x07true\r\n\x1b]133;C\x07ok\r\n\x1b]133;D;0\x07",
        );

        assert!(terminal.command_zones.is_empty());
        assert_eq!(terminal.next_zone_id, u64::MAX);
        // Non-UI consumers still receive the bounded completion observation.
        assert_eq!(terminal.take_completed_commands().len(), 1);
    }

    #[test]
    fn osc_133_captures_output_without_a_trailing_newline_at_d_or_next_a() {
        let mut completed = super::TerminalState::new(40, 8);
        completed.process_input(
            b"\x1b]133;A\x07$ \x1b]133;B\x07printf foo\r\n\x1b]133;C\x07foo\x1b]133;D;0\x07",
        );
        let zone = completed.command_zones.back().expect("completed block");
        assert_eq!(completed.zone_output_text(zone.id).as_deref(), Some("foo"));
        assert_eq!(completed.take_completed_commands()[0].output, "foo");

        let mut stale = super::TerminalState::new(40, 8);
        stale.process_input(
            b"\x1b]133;A\x07$ \x1b]133;B\x07printf foo\r\n\x1b]133;C\x07foo\x1b]133;A\x07",
        );
        let zone = stale.command_zones.back().expect("stale block");
        assert_eq!(
            zone.completion_provenance,
            crate::block_mode::CompletionProvenance::BoundaryInferred
        );
        assert_eq!(stale.zone_output_text(zone.id).as_deref(), Some("foo"));
    }

    #[test]
    fn osc_133_first_row_output_tracks_leftward_cr_bs_and_cup_writes() {
        let cases: [(&[u8], &str); 3] = [
            (b"\rRESULT", "RESULT"),
            (b"\x08Z", "Z"),
            (b"\x1b[1;1HRESULT", "RESULT"),
        ];
        for (motion_and_output, expected) in cases {
            let mut terminal = super::TerminalState::new(40, 8);
            terminal.process_input(b"\x1b]133;A\x07$ \x1b]133;B\x07cmd\x1b]133;C\x07");
            terminal.process_input(motion_and_output);
            terminal.process_input(b"\x1b]133;D;0\x07");

            let id = terminal.command_zones.back().expect("completed block").id;
            assert_eq!(terminal.zone_output_text(id).as_deref(), Some(expected));
            assert_eq!(terminal.take_completed_commands()[0].output, expected);
        }

        let mut stale = super::TerminalState::new(40, 8);
        stale.process_input(
            b"\x1b]133;A\x07$ \x1b]133;B\x07cmd\x1b]133;C\x07\rRESULT\x1b]133;A\x07",
        );
        let id = stale.command_zones.back().expect("stale block").id;
        assert_eq!(stale.zone_output_text(id).as_deref(), Some("RESULT"));
    }

    #[test]
    fn active_app_extent_does_not_shrink_when_output_moves_cursor_up() {
        let mut terminal = super::TerminalState::new(40, 8);
        assert!(!terminal.has_usable_block_partitions());
        terminal.process_input(b"\x1b]133;A\x07$ \x1b]133;B\x07cmd\x1b]133;C\x07");
        terminal.process_input(b"\r\none\r\ntwo");
        let painted_extent = terminal.active_app_extent_row().expect("running extent");
        assert!(painted_extent >= 2);

        // Move above already painted output without writing there. Mouse
        // ownership must keep the lower output row in the active surface.
        terminal.process_input(b"\x1b[1;1H");
        let cursor = terminal.scrollback_len() + terminal.get_cursor_pos().0;
        assert!(cursor < painted_extent);
        assert_eq!(terminal.active_app_extent_row(), Some(painted_extent));
        assert!(terminal.has_usable_block_partitions());
    }

    #[test]
    fn osc_133_alt_screen_paint_never_extends_primary_output() {
        let mut terminal = super::TerminalState::new(20, 8);
        // A sentinel below the primary cursor makes an alt-derived end row
        // observable as leaked command output, not merely excess blank rows.
        terminal.process_input(b"\x1b[8;1HSECRET\x1b[1;1H");
        terminal.process_input(b"\x1b]133;A\x07$ \x1b]133;B\x07vim\r\n\x1b]133;C\x07");
        let output_start = match terminal.current_zone_state {
            super::ZoneState::OutputStarted(_, _, output_start) => output_start,
            _ => panic!("running command"),
        };

        terminal.process_input(b"\x1b[?1049h\x1b[8;1HALT PAINT\x1b[?1049l");
        terminal.process_input(b"\x1b]133;D;0\x07");

        let zone = terminal.command_zones.back().expect("completed block");
        assert_eq!(zone.output_end, Some(output_start));
        assert_eq!(terminal.zone_output_text(zone.id), None);
        assert_eq!(terminal.take_completed_commands()[0].output, "");
    }

    #[test]
    fn resize_keeps_live_output_coordinates_inside_the_resized_buffer() {
        let mut narrow = super::TerminalState::new(12, 4);
        narrow.process_input(b"\x1b]133;A\x07$ \x1b]133;B\x0712345678\x1b]133;C\x07");
        narrow.on_resize(5, 4);
        assert_eq!(narrow.current_output_start_col, Some(4));
        narrow.process_input(b"Z\x1b]133;D;0\x07");
        let id = narrow.command_zones.back().expect("completed block").id;
        assert_eq!(narrow.zone_output_text(id).as_deref(), Some("Z"));

        // Once the C row is in scrollback it still has the old width; only
        // grid-local columns follow an immediate width resize.
        let mut history = super::TerminalState::new(20, 2);
        history.process_input(b"\x1b]133;A\x07$ \x1b]133;B\x0712345678\x1b]133;C\x07Z\r\none\r\n");
        let output_start = match history.current_zone_state {
            super::ZoneState::OutputStarted(_, _, output_start) => output_start,
            _ => panic!("running command"),
        };
        assert!(output_start < history.scrollback.len());
        history.on_resize(5, 2);
        assert_eq!(history.current_output_start_col, Some(10));
        history.process_input(b"\x1b]133;D;0\x07");
        let id = history.command_zones.back().expect("completed block").id;
        assert_eq!(history.zone_output_text(id).as_deref(), Some("Z\none"));

        let mut truncated = super::TerminalState::new(10, 4);
        truncated.process_input(b"\x1b]133;A\x07$ \x1b]133;B\x07cmd\x1b]133;C\x07\x1b[4;1HOLDTAIL");
        truncated.process_input(b"\x1b[1;1H");
        truncated.on_resize(10, 2);
        assert_eq!(truncated.current_output_extent_row, Some(1));
        truncated.process_input(b"\x1b]133;D;0\x07");
        let id = truncated.command_zones.back().expect("completed block").id;
        assert_eq!(truncated.command_zones.back().unwrap().output_end, Some(2));
        assert_eq!(truncated.zone_output_text(id), None);

        // Growing the buffer later must not bring newly painted rows inside
        // the finalized output range that was truncated by the shrink.
        truncated.on_resize(10, 4);
        truncated.process_input(b"\x1b[4;1HNEW");
        assert_eq!(truncated.zone_output_text(id), None);
    }

    #[test]
    fn osc_133_id_params_correlate_completed_commands() {
        let mut terminal = super::TerminalState::new(40, 8);
        terminal.process_input(b"\x1b]133;A;id=jsh-abc.123\x07$ ");
        terminal.process_input(b"\x1b]133;B\x07echo hi\r\n");
        terminal.process_input(b"\x1b]133;C\x07hi\r\n");
        terminal.process_input(b"\x1b]133;D;0\x07");

        let completed = terminal.take_completed_commands();
        assert_eq!(completed.len(), 1);
        assert_eq!(completed[0].id.as_deref(), Some("jsh-abc.123"));
        assert!(completed[0].output_available);
        assert!(!completed[0].truncated);
        assert_eq!(completed[0].total_bytes, completed[0].output.len());
        assert_eq!(completed[0].exit_code, Some(0));

        // The id is consumed with its command; the next one must not inherit it.
        terminal.process_input(b"\x1b]133;A\x07$ ");
        terminal.process_input(b"\x1b]133;B\x07");
        terminal.note_user_input(b"true\r");
        terminal.process_input(b"true\r\n");
        terminal.process_input(b"\x1b]133;C\x07");
        terminal.process_input(b"\x1b]133;D;exit_code=1\x07");
        let completed = terminal.take_completed_commands();
        assert_eq!(completed.len(), 1);
        assert_eq!(completed[0].id, None);
        assert_eq!(completed[0].exit_code, Some(1));
    }

    #[test]
    fn osc_133_duration_spans_command_execution() {
        let mut terminal = super::TerminalState::new(40, 8);
        terminal.process_input(b"\x1b]133;A\x07$ ");
        terminal.process_input(b"\x1b]133;B\x07make\r\n");
        terminal.process_input(b"\x1b]133;C\x07building\r\n");
        terminal.process_input(b"\x1b]133;D;0\x07");

        let completed = terminal.take_completed_commands();
        assert_eq!(completed.len(), 1);
        assert!(completed[0].duration_ms.is_some());

        // Without a `C` there is no execution lifecycle at all: D is ignored
        // and cannot mint an empty record or inherit the previous duration.
        terminal.process_input(b"\x1b]133;A\x07$ ");
        terminal.process_input(b"\x1b]133;B\x07");
        terminal.note_user_input(b"true\r");
        terminal.process_input(b"true\r\n");
        terminal.process_input(b"\x1b]133;D;0\x07");
        assert!(terminal.take_completed_commands().is_empty());
        assert_eq!(terminal.command_zones.len(), 1);
    }

    #[test]
    fn osc_133_inside_the_alt_screen_creates_no_zone() {
        let mut terminal = super::TerminalState::new(40, 8);
        terminal.process_input(b"\x1b[?1049h");
        assert!(terminal.is_alt_buffer_active());
        // A full lifecycle emitted while a full-screen app owns the grid is
        // dropped entirely (ember's semantic): no zone, no agent record.
        terminal.process_input(b"\x1b]133;A\x07$ ");
        terminal.process_input(b"\x1b]133;B\x07make\r\n");
        terminal.process_input(b"\x1b]133;C\x07building\r\n");
        terminal.process_input(b"\x1b]133;D;1\x07");
        assert!(terminal.command_zones.is_empty());
        assert!(terminal.take_completed_commands().is_empty());
        assert!(!terminal.is_command_running());

        // Back on the primary screen, a fresh lifecycle records normally.
        terminal.process_input(b"\x1b[?1049l");
        terminal.process_input(b"\x1b]133;A\x07$ ");
        terminal.process_input(b"\x1b]133;B\x07echo hi\r\n");
        terminal.process_input(b"\x1b]133;C\x07hi\r\n");
        terminal.process_input(b"\x1b]133;D;0\x07");
        assert_eq!(terminal.command_zones.len(), 1);
        assert_eq!(
            terminal.command_zones[0].command.as_deref(),
            Some("echo hi")
        );
        assert_eq!(terminal.command_zones[0].exit_code, Some(0));
    }

    #[test]
    fn osc_133_zone_survives_a_mid_lifecycle_alt_screen_detour() {
        // The guard's promise: a zone opened on the primary screen keeps its
        // pending ZoneState across an alt-screen detour (vim mid-command) and
        // finalizes normally when `D` arrives back on the primary screen.
        let mut terminal = super::TerminalState::new(40, 8);
        terminal.process_input(b"\x1b]133;A\x07$ ");
        terminal.process_input(b"\x1b]133;B\x07vim notes\r\n");
        terminal.process_input(b"\x1b]133;C\x07");
        assert!(terminal.is_command_running());

        // The app takes over the alt screen and paints freely; none of it
        // may disturb the pending primary-screen zone.
        terminal.process_input(b"\x1b[?1049h");
        assert!(terminal.is_alt_buffer_active());
        terminal.process_input(b"~ full screen ui\r\nmore ui\r\n");

        terminal.process_input(b"\x1b[?1049l");
        assert!(!terminal.is_alt_buffer_active());
        terminal.process_input(b"\x1b]133;D;0\x07");

        assert_eq!(terminal.command_zones.len(), 1);
        let zone = &terminal.command_zones[0];
        assert_eq!(zone.command.as_deref(), Some("vim notes"));
        assert_eq!(zone.exit_code, Some(0));
        // Rows were all captured on the primary screen: prompt row 0,
        // command row 0 (typed on the prompt line), output from row 1, and
        // the closing row wherever the primary cursor was restored to.
        assert_eq!(zone.prompt_start, 0);
        assert_eq!(zone.command_start, Some(0));
        assert_eq!(zone.output_start, Some(1));
        assert!(zone.output_end.is_some());
        assert!(zone.output_end >= zone.output_start);
        assert!(!terminal.is_command_running());
    }

    #[test]
    fn osc_133_d_during_alt_screen_is_dropped_and_the_next_prompt_starts_clean() {
        // A `D` that arrives WHILE the alt screen is active is still dropped
        // (its row would be computed against the alt grid), so the exit code
        // is lost — but the zone itself no longer is: the next primary-screen
        // `A` finalizes the pending lifecycle as Unknown (v3's stale-Running
        // rule) instead of v2's silent discard.
        let mut terminal = super::TerminalState::new(40, 8);
        terminal.process_input(b"\x1b]133;A\x07$ ");
        terminal.process_input(b"\x1b]133;B\x07vim notes\r\n");
        terminal.process_input(b"\x1b]133;C\x07");
        terminal.process_input(b"\x1b[?1049h");
        terminal.process_input(b"\x1b]133;D;1\x07"); // dropped: alt is active
        assert!(terminal.command_zones.is_empty());
        assert!(terminal.take_completed_commands().is_empty());

        // Back on the primary screen, the next `A` closes the stale zone
        // (exit honestly unreported) and resets every per-command field; the
        // next lifecycle records cleanly beside it.
        terminal.process_input(b"\x1b[?1049l");
        terminal.process_input(b"\x1b]133;A\x07$ ");
        assert_eq!(terminal.command_zones.len(), 1);
        assert_eq!(
            terminal.command_zones[0].command.as_deref(),
            Some("vim notes")
        );
        assert_eq!(terminal.command_zones[0].exit_code, None);
        assert!(!terminal.is_command_running());
        let inferred = terminal.take_completed_commands();
        assert_eq!(inferred.len(), 1);
        assert_eq!(
            inferred[0].completion_provenance,
            crate::block_mode::CompletionProvenance::BoundaryInferred
        );
        terminal.process_input(b"\x1b]133;B\x07echo hi\r\n");
        terminal.process_input(b"\x1b]133;C\x07hi\r\n");
        terminal.process_input(b"\x1b]133;D;0\x07");
        assert_eq!(terminal.command_zones.len(), 2);
        let zone = &terminal.command_zones[1];
        assert_eq!(zone.command.as_deref(), Some("echo hi"));
        assert_eq!(zone.exit_code, Some(0));
        let completed = terminal.take_completed_commands();
        assert_eq!(completed.len(), 1);
        assert_eq!(completed[0].command, "echo hi");
        assert!(!terminal.is_command_running());
    }

    #[test]
    fn osc_133_shell_reported_duration_beats_local_timing() {
        let mut terminal = super::TerminalState::new(40, 8);
        terminal.process_input(b"\x1b]133;A\x07$ ");
        terminal.process_input(b"\x1b]133;B\x07make\r\n");
        terminal.process_input(b"\x1b]133;C\x07building\r\n");
        terminal.process_input(b"\x1b]133;D;0;duration_ms=4200\x07");
        assert_eq!(terminal.command_zones[0].duration_ms, Some(4_200));
        assert_eq!(
            terminal.take_completed_commands()[0].duration_ms,
            Some(4_200)
        );

        // A D without C is not a command lifecycle, even if it carries a
        // duration-looking parameter.
        terminal.process_input(b"\x1b]133;A\x07$ ");
        terminal.process_input(b"\x1b]133;B\x07");
        terminal.note_user_input(b"true\r");
        terminal.process_input(b"true\r\n");
        terminal.process_input(b"\x1b]133;D;exit=0;duration=7\x07");
        assert_eq!(terminal.command_zones.len(), 1);

        // Local timing remains the fallback when no param arrives.
        terminal.process_input(b"\x1b]133;A\x07$ ");
        terminal.process_input(b"\x1b]133;B\x07ls\r\n");
        terminal.process_input(b"\x1b]133;C\x07f\r\n");
        terminal.process_input(b"\x1b]133;D;0\x07");
        assert!(terminal.command_zones[1].duration_ms.is_some());
    }

    #[test]
    fn osc_133_cmd_truncated_param_marks_the_zone() {
        let mut terminal = super::TerminalState::new(40, 8);
        terminal.process_input(b"\x1b]133;A\x07$ ");
        terminal.process_input(b"\x1b]133;B\x07ls\r\n");
        terminal.process_input(b"\x1b]133;C;cmd_truncated=1\x07f\r\n");
        terminal.process_input(b"\x1b]133;D;0\x07");
        assert!(terminal.command_zones[0].command_truncated);

        // The flag does not leak into the next lifecycle, and only truthy
        // spellings set it.
        terminal.process_input(b"\x1b]133;A\x07$ ");
        terminal.process_input(b"\x1b]133;B\x07ls\r\n");
        terminal.process_input(b"\x1b]133;C;cmd_truncated=0\x07f\r\n");
        terminal.process_input(b"\x1b]133;D;0\x07");
        assert!(!terminal.command_zones[1].command_truncated);

        // The alternate spelling on `D` is honored too.
        terminal.process_input(b"\x1b]133;A\x07$ ");
        terminal.process_input(b"\x1b]133;B\x07ls\r\n");
        terminal.process_input(b"\x1b]133;C\x07f\r\n");
        terminal.process_input(b"\x1b]133;D;0;command_truncated=true\x07");
        assert!(terminal.command_zones[2].command_truncated);
    }

    #[test]
    fn osc_133_cwd_param_is_decoded_capped_and_control_checked() {
        let mut terminal = super::TerminalState::new(40, 8);
        terminal.process_input(b"\x1b]133;A\x07$ ");
        terminal.process_input(b"\x1b]133;B\x07ls\r\n");
        terminal.process_input(b"\x1b]133;C;cwd_url=%2Ftmp%2Fmy%20dir\x07f\r\n");
        terminal.process_input(b"\x1b]133;D;0\x07");
        assert_eq!(
            terminal.command_zones[0].cwd.as_deref(),
            Some("/tmp/my dir")
        );

        // A control character in the decoded value rejects the whole param
        // (falling back to the OSC 7 cwd — none here).
        terminal.process_input(b"\x1b]133;A\x07$ ");
        terminal.process_input(b"\x1b]133;B\x07ls\r\n");
        terminal.process_input(b"\x1b]133;C;cwd=%2Ftmp%0Aevil\x07f\r\n");
        terminal.process_input(b"\x1b]133;D;0\x07");
        assert_eq!(terminal.command_zones[1].cwd, None);

        // An over-cap (16 KiB) value is rejected rather than half-decoded.
        let oversized = format!("C;cwd={}", "a".repeat(16 * 1024 + 1));
        terminal.process_input(b"\x1b]133;A\x07$ ");
        terminal.process_input(b"\x1b]133;B\x07ls\r\n");
        terminal.handle_osc_133(&oversized);
        terminal.process_input(b"\x1b]133;D;0\x07");
        assert_eq!(terminal.command_zones[2].cwd, None);

        // Without any OSC 133 cwd param, the OSC 7 cwd fills in.
        terminal.process_input(b"\x1b]7;file://localhost/srv\x07");
        terminal.process_input(b"\x1b]133;A\x07$ ");
        terminal.process_input(b"\x1b]133;B\x07ls\r\n");
        terminal.process_input(b"\x1b]133;C\x07f\r\n");
        terminal.process_input(b"\x1b]133;D;0\x07");
        assert_eq!(terminal.command_zones[3].cwd.as_deref(), Some("/srv"));

        // An OSC 7 path smuggling a control character (OSC 7 itself only
        // rejects NUL) is refused at zone finalization: every zone cwd is
        // control-free, whatever its source.
        terminal.process_input(b"\x1b]7;file://localhost/tmp%0Aevil\x07");
        terminal.process_input(b"\x1b]133;A\x07$ ");
        terminal.process_input(b"\x1b]133;B\x07ls\r\n");
        terminal.process_input(b"\x1b]133;C\x07f\r\n");
        terminal.process_input(b"\x1b]133;D;0\x07");
        assert_eq!(terminal.command_zones[4].cwd, None);
    }

    #[test]
    fn rows_text_flags_only_real_row_dropping_truncation() {
        let mut terminal = super::TerminalState::new(10, 4);
        terminal.process_input(b"aaaa\r\nbbbb\r\ncccc\r\n");
        // A cap that cannot hold every row drops whole rows and says so.
        let (text, capped) = terminal.rows_text(0, 3, 4);
        assert_eq!(text, "aaaa");
        assert!(capped);
        // A roomy cap is not truncation…
        let (text, capped) = terminal.rows_text(0, 3, 1024);
        assert_eq!(text, "aaaa\nbbbb\ncccc");
        assert!(!capped);
        // …and neither is a cap reached exactly on the final row.
        let (text, capped) = terminal.rows_text(0, 3, 14);
        assert_eq!(text, "aaaa\nbbbb\ncccc");
        assert!(!capped);
    }

    #[test]
    fn completed_command_uses_the_extractor_truncation_flag() {
        const CAP: usize = 256 * 1024;
        let metadata = || super::CompletedCommandMetadata {
            exit_code: Some(0),
            duration_ms: None,
            execution_id: None,
            agent_generation: None,
            completion_provenance: crate::block_mode::CompletionProvenance::ShellReported,
        };

        // The next whole row does not fit, so extraction is capped while the
        // retained prefix itself remains strictly smaller than the byte cap.
        let mut row_dropped = super::TerminalState::new(1024, 256);
        for row in 0..256 {
            for cell in &mut row_dropped.grid[row] {
                cell.character = 'x';
            }
        }
        let (prefix, capped) = row_dropped.rows_text(0, 256, CAP);
        assert!(capped);
        assert!(prefix.len() < CAP);
        row_dropped.current_command_text = Some("cmd".to_string());
        row_dropped.record_completed_command(0, 0, Some((0, 256, 0)), metadata());
        assert!(row_dropped.take_completed_commands()[0].truncated);

        // Conversely, a complete extraction may occupy exactly the cap. Its
        // length alone must not turn it into a false truncation report.
        let mut exact = super::TerminalState::new(1024, 64);
        for row in 0..63 {
            for cell in &mut exact.grid[row] {
                cell.character = '\u{1f600}';
            }
        }
        for col in 0..1008 {
            exact.grid[63][col].character = '\u{1f600}';
        }
        exact.grid[63][1008].character = 'x';
        let (text, capped) = exact.rows_text(0, 64, CAP);
        assert_eq!(text.len(), CAP);
        assert!(!capped);
        exact.current_command_text = Some("cmd".to_string());
        exact.record_completed_command(0, 0, Some((0, 64, 0)), metadata());
        assert!(!exact.take_completed_commands()[0].truncated);
    }

    #[test]
    fn agent_prompt_requires_idle_fresh_and_empty_osc_133_input() {
        use super::AgentPromptStatus;

        let mut terminal = super::TerminalState::new(40, 8);
        assert_eq!(
            terminal.agent_prompt_status(),
            AgentPromptStatus::ShellIntegrationUnavailable
        );

        terminal.process_input(b"\x1b]133;A\x07$ \x1b]133;B\x07");
        assert_eq!(terminal.agent_prompt_status(), AgentPromptStatus::Ready);

        // Local input blocks approval immediately, before PTY echo arrives.
        terminal.note_user_input(b"typed but not echoed");
        assert_eq!(
            terminal.agent_prompt_status(),
            AgentPromptStatus::InputNotEmpty
        );

        // A fresh prompt clears the local-input taint, but visible command
        // text is independently detected even without note_user_input.
        terminal.process_input(b"\x1b]133;A\x07$ \x1b]133;B\x07typed");
        assert_eq!(
            terminal.agent_prompt_status(),
            AgentPromptStatus::InputNotEmpty
        );
        terminal.process_input(b"\r\n\x1b]133;C\x07");
        assert_eq!(terminal.agent_prompt_status(), AgentPromptStatus::Busy);
    }

    #[test]
    fn protocol_replies_taint_current_or_next_prompt_before_echo() {
        use super::AgentPromptStatus;

        let mut idle = super::TerminalState::new(40, 8);
        idle.process_input(b"\x1b]133;A\x07$ \x1b]133;B\x07");
        idle.note_protocol_response();
        assert_eq!(idle.agent_prompt_status(), AgentPromptStatus::InputNotEmpty);
        idle.process_input(b"reply\x1b]133;A\x07");
        assert!(idle.command_zones.is_empty());

        let mut running = super::TerminalState::new(40, 8);
        running.process_input(b"\x1b]133;A\x07$ \x1b]133;B\x07cmd\r\n\x1b]133;C\x07out\r\n");
        running.note_protocol_response();
        running.process_input(b"\x1b]133;D;0\x07\x1b]133;A\x07$ \x1b]133;B\x07");
        assert_eq!(
            running.agent_prompt_status(),
            AgentPromptStatus::InputNotEmpty
        );
    }

    #[test]
    fn agent_arm_rejects_visual_spoofing_at_the_terminal_boundary() {
        use super::AgentPromptStatus;

        let mut terminal = super::TerminalState::new(40, 8);
        terminal.process_input(b"\x1b]133;A\x07$ \x1b]133;B\x07");
        assert_eq!(
            terminal.arm_agent_execution(1, "printf safe\u{202e}hidden"),
            Err(AgentPromptStatus::UnsafeCommand)
        );
        assert_eq!(terminal.agent_prompt_status(), AgentPromptStatus::Ready);
    }

    #[test]
    fn agent_completion_uses_exact_command_generation_and_execution_id_once() {
        let mut terminal = super::TerminalState::new(40, 8);
        terminal.process_input(b"\x1b]133;A\x07$ \x1b]133;B\x07");
        terminal
            .arm_agent_execution(41, "ls -la")
            .expect("fresh prompt is ready");
        terminal
            .process_input(b"ls -la\r\n\x1b]133;C;id=jsh-41;cmdline_url=ls%20-la\x07total 0\r\n");

        // A D with a different id cannot steal the armed execution.
        terminal.process_input(b"\x1b]133;D;0;id=spoof\x07");
        assert!(terminal.take_completed_commands().is_empty());
        terminal.process_input(b"\x1b]133;D;0;id=jsh-41\x07");

        let completed = terminal.take_completed_commands();
        assert_eq!(completed.len(), 1);
        assert_eq!(completed[0].command, "ls -la");
        assert_eq!(completed[0].id.as_deref(), Some("jsh-41"));
        assert_eq!(completed[0].agent_generation, Some(41));

        // The generation and zone were consumed by the first valid D.
        terminal.process_input(b"\x1b]133;D;0;id=jsh-41\x07");
        assert!(terminal.take_completed_commands().is_empty());
    }

    #[test]
    fn ris_releases_an_active_agent_generation_as_boundary_inferred() {
        let mut terminal = super::TerminalState::new(40, 8);
        terminal.process_input(b"\x1b]133;A\x07$ \x1b]133;B\x07");
        terminal
            .arm_agent_execution(51, "printf '\\ec'")
            .expect("fresh prompt is ready");
        terminal.process_input(
            b"printf '\\ec'\r\n\x1b]133;C;id=ris-51;cmdline_url=printf%20%27%5Cec%27\x07",
        );

        terminal.process_input(b"\x1bc");

        assert!(!terminal.is_command_running());
        assert!(
            terminal.command_zones.is_empty(),
            "RIS still clears history"
        );
        let completed = terminal.take_completed_commands();
        assert_eq!(completed.len(), 1);
        assert_eq!(completed[0].command, "printf '\\ec'");
        assert_eq!(completed[0].id.as_deref(), Some("ris-51"));
        assert_eq!(completed[0].agent_generation, Some(51));
        assert_eq!(completed[0].exit_code, None);
        assert!(!completed[0].output_available);
        assert_eq!(
            completed[0].completion_provenance,
            crate::block_mode::CompletionProvenance::BoundaryInferred
        );

        terminal.process_input(b"\x1b]133;A\x07$ \x1b]133;B\x07next\r\n\x1b]133;C\x07next");
        terminal.process_input(b"\x1b]133;D;0;id=ris-51\x07");
        assert!(terminal.is_command_running());
        assert!(terminal.take_completed_commands().is_empty());
        terminal.process_input(b"\x1b]133;D;0\x07");
        assert_eq!(terminal.take_completed_commands().len(), 1);
    }

    #[test]
    fn fresh_prompt_releases_an_agent_approval_that_never_reached_c() {
        let mut terminal = super::TerminalState::new(40, 8);
        terminal.process_input(b"\x1b]133;A;id=armed-61\x07$ \x1b]133;B;id=armed-61\x07");
        terminal
            .arm_agent_execution(61, "echo safe")
            .expect("fresh prompt is ready");

        terminal.process_input(b"\x1b]133;A;id=next\x07$ ");

        let completed = terminal.take_completed_commands();
        assert_eq!(completed.len(), 1);
        assert_eq!(completed[0].command, "echo safe");
        assert_eq!(completed[0].id.as_deref(), Some("armed-61"));
        assert_eq!(completed[0].agent_generation, Some(61));
        assert_eq!(completed[0].exit_code, None);
        assert_eq!(
            completed[0].completion_provenance,
            crate::block_mode::CompletionProvenance::BoundaryInferred
        );
    }

    #[test]
    fn ris_releases_an_agent_approval_that_never_reached_c() {
        let mut terminal = super::TerminalState::new(40, 8);
        terminal.process_input(b"\x1b]133;A;id=armed-62\x07$ \x1b]133;B;id=armed-62\x07");
        terminal
            .arm_agent_execution(62, "echo safe")
            .expect("fresh prompt is ready");

        terminal.process_input(b"\x1bc");

        let completed = terminal.take_completed_commands();
        assert_eq!(completed.len(), 1);
        assert_eq!(completed[0].command, "echo safe");
        assert_eq!(completed[0].id.as_deref(), Some("armed-62"));
        assert_eq!(completed[0].agent_generation, Some(62));
        assert_eq!(completed[0].exit_code, None);
        assert_eq!(
            completed[0].completion_provenance,
            crate::block_mode::CompletionProvenance::BoundaryInferred
        );
    }

    #[test]
    fn agent_suffix_collision_never_receives_the_armed_generation() {
        let mut terminal = super::TerminalState::new(40, 8);
        terminal.process_input(b"\x1b]133;A\x07$ \x1b]133;B\x07");
        terminal
            .arm_agent_execution(9, "ls -la")
            .expect("fresh prompt is ready");
        terminal.process_input(b"echo prefix; ls -la\r\n\x1b]133;C;id=jsh-other\x07spoof\r\n");
        terminal.process_input(b"\x1b]133;D;0;id=jsh-other\x07");

        let completed = terminal.take_completed_commands();
        assert_eq!(completed.len(), 1);
        assert_eq!(completed[0].command, "echo prefix; ls -la");
        assert_eq!(completed[0].agent_generation, None);
    }

    #[test]
    fn local_input_after_agent_arm_revokes_the_generation_before_echo() {
        let mut terminal = super::TerminalState::new(40, 8);
        terminal.process_input(b"\x1b]133;A\x07$ \x1b]133;B\x07");
        terminal
            .arm_agent_execution(9, "ls -la")
            .expect("fresh prompt is ready");
        terminal.note_user_input(b"unrelated local input");
        // Even if hostile metadata reports the originally approved text, the
        // local input boundary revoked authorization before PTY echo.
        terminal
            .process_input(b"ls -la\r\n\x1b]133;C;id=jsh-other;cmdline_url=ls%20-la\x07spoof\r\n");
        terminal.process_input(b"\x1b]133;D;0;id=jsh-other\x07");

        let completed = terminal.take_completed_commands();
        assert_eq!(completed.len(), 1);
        assert_eq!(completed[0].agent_generation, None);
    }

    #[test]
    fn input_after_agent_submit_taints_the_following_prompt() {
        let mut terminal = super::TerminalState::new(40, 8);
        terminal.process_input(b"\x1b]133;A\x07$ \x1b]133;B\x07");
        terminal
            .arm_agent_execution(9, "ls -la")
            .expect("fresh prompt is ready");
        terminal.note_user_input(b"queued");
        terminal.process_input(b"ls -la\r\n\x1b]133;C\x07out\r\n\x1b]133;D;0\x07");
        terminal.process_input(b"\x1b]133;A\x07$ \x1b]133;B\x07");

        assert_eq!(
            terminal.agent_prompt_status(),
            super::AgentPromptStatus::InputNotEmpty
        );
        terminal.process_input(b"queued\r\n\x1b]133;A\x07");
        assert_eq!(terminal.command_zones.len(), 1);
    }

    use super::{
        AgentPromptStatus, ClipboardReadKind, Color, CursorShape, IdleBackgroundOutput,
        ScrollbackLine, TerminalCell, TerminalState, ZoneOutputExport, MAX_COMMAND_ZONES,
        MAX_OSC8_ID_BYTES, MAX_OSC8_URI_BYTES, MAX_PENDING_ESCAPE, MAX_TERMINAL_TITLE_CHARS,
    };

    fn osc8_spans(terminal: &mut TerminalState) -> Vec<crate::link::Link> {
        let visible = terminal.get_visible_cells();
        terminal.osc8_links_in_visible_cells(visible.as_ref())
    }

    #[test]
    fn osc8_marks_exact_cells_and_closes_without_marking_following_text() {
        let mut terminal = TerminalState::new(20, 3);
        terminal.process_batch(
            b"\x1b]8;id=docs;https://example.com/target\x1b\\click\x1b]8;;\x1b\\ plain",
        );

        assert_eq!(
            osc8_spans(&mut terminal),
            [crate::link::Link {
                line: 0,
                col_start: 0,
                col_end: 5,
                link_type: crate::link::LinkType::Url,
                text: "https://example.com/target".to_string(),
            }]
        );
    }

    #[test]
    fn osc8_rejects_unsafe_or_oversized_fields_before_interning() {
        let mut terminal = TerminalState::new(12, 2);
        terminal.process_batch(b"\x1b]8;;file:///etc/passwd\x1b\\unsafe");
        assert!(osc8_spans(&mut terminal).is_empty());
        assert_eq!(terminal.osc8_interned_count(), 0);

        let oversized_uri = format!(
            "\x1b]8;;https://example.com/{}\x1b\\U",
            "x".repeat(MAX_OSC8_URI_BYTES)
        );
        terminal.process_batch(oversized_uri.as_bytes());
        let oversized_id = format!(
            "\x1b]8;id={};https://example.com\x1b\\I",
            "x".repeat(MAX_OSC8_ID_BYTES + 1)
        );
        terminal.process_batch(oversized_id.as_bytes());
        assert_eq!(terminal.osc8_interned_count(), 0);
        assert!(osc8_spans(&mut terminal).is_empty());
    }

    #[test]
    fn osc8_survives_scrollback_and_resize_but_not_buffer_state_leaks() {
        let mut terminal = TerminalState::new(8, 2);
        terminal
            .process_batch(b"\x1b]8;;https://example.com/a\x1b\\linked\x1b]8;;\x1b\\\r\nnext\r\n");
        terminal.scroll(1);
        assert!(osc8_spans(&mut terminal)
            .iter()
            .any(|link| link.text == "https://example.com/a"));

        terminal.on_resize(10, 3);
        terminal.scroll(isize::MAX);
        assert!(osc8_spans(&mut terminal)
            .iter()
            .any(|link| link.text == "https://example.com/a"));

        terminal.scroll_to_bottom();
        terminal.process_batch(b"\x1b]8;;https://example.com/leak\x1b\\\x1b[?1049hALT");
        assert!(osc8_spans(&mut terminal).is_empty());
        terminal.process_batch(b"\x1b]8;;https://example.com/alt\x1b\\A\x1b[?1049lP");
        assert!(osc8_spans(&mut terminal)
            .iter()
            .all(|link| link.text != "https://example.com/alt"));
    }

    #[test]
    fn osc8_metadata_survives_compressed_scrollback_round_trip() {
        let mut terminal = TerminalState::new(8, 2);
        terminal
            .process_batch(b"\x1b]8;;https://example.com/x\x1b\\link\x1b]8;;\x1b\\\r\nnext\r\n");
        let restored = terminal
            .scrollback
            .front()
            .expect("linked row reached scrollback")
            .decompress();
        let links = terminal.osc8_links_in_visible_cells(&[restored]);
        assert_eq!(links.len(), 1);
        assert_eq!((links[0].col_start, links[0].col_end), (0, 4));
    }

    #[test]
    fn osc8_erase_and_ris_leave_no_clickable_metadata() {
        let mut terminal = TerminalState::new(12, 2);
        terminal.process_batch(b"\x1b]8;;https://example.com/x\x1b\\linked");
        assert_eq!(osc8_spans(&mut terminal).len(), 1);

        terminal.process_batch(b"\r\x1b[2K");
        assert!(osc8_spans(&mut terminal).is_empty());
        terminal.process_batch(b"\x1b]8;;https://example.com/y\x1b\\again\x1bc");
        assert_eq!(terminal.osc8_interned_count(), 0);
        assert!(osc8_spans(&mut terminal).is_empty());
    }

    #[test]
    fn scrollback_compression_round_trips_blink_style() {
        let mut cells = vec![TerminalCell::default(); 4];
        cells[0].character = 'A';
        cells[0].foreground = Color::BrightCyan;
        cells[0].flags.set_blink(true);
        cells[1] = cells[0];
        cells[2].character = 'B';
        cells[2].flags.set_strikethrough(true);

        let restored = ScrollbackLine::compress(&cells, false).decompress();

        assert_eq!(restored.len(), cells.len());
        for (column, (actual, expected)) in restored.iter().zip(&cells).enumerate() {
            assert_eq!(
                actual.character, expected.character,
                "character differs at column {column}"
            );
            assert_eq!(
                actual.foreground, expected.foreground,
                "foreground differs at column {column}"
            );
            assert_eq!(
                actual.background, expected.background,
                "background differs at column {column}"
            );
            assert_eq!(
                actual.flags, expected.flags,
                "flags differ at column {column}"
            );
        }
    }

    #[test]
    fn compressed_row_layout_is_exact_without_decompressing_cells() {
        let mut encoded_cells = vec![TerminalCell::default(); 8];
        encoded_cells[0].character = 'A';
        // A styled space is retained by compression and must contribute to
        // layout even though its glyph is visually blank.
        encoded_cells[2].foreground = Color::Red;
        encoded_cells[4].character = '中';
        encoded_cells[4].flags.set_wide(true);
        encoded_cells[5].flags.set_wide_continuation(true);
        let encoded = ScrollbackLine::compress(&encoded_cells, true);

        super::PROJECTED_HISTORY_DECOMPRESS_COUNT.with(|count| count.set(0));
        let layout = encoded.layout(17);
        assert_eq!(layout.absolute_row, 17);
        assert_eq!(layout.raw_row, encoded.raw_row_id());
        assert_eq!(layout.active_len, 6);
        assert_eq!(layout.wide_continuations.as_slice(), &[5]);
        assert!(layout.wrapped);
        assert_eq!(
            super::PROJECTED_HISTORY_DECOMPRESS_COUNT.with(|count| count.get()),
            0,
            "layout planning must not materialize terminal cells"
        );

        let mut plain_cells = vec![TerminalCell::default(); 8];
        plain_cells[0].character = 'A';
        plain_cells[1].character = 'B';
        let plain = ScrollbackLine::compress(&plain_cells, false).layout(3);
        assert_eq!(plain.active_len, 2);
        assert!(plain.wide_continuations.is_empty());
        assert!(!plain.wrapped);

        let mut styled_blank = vec![TerminalCell::default(); 8];
        styled_blank[3].foreground = Color::Red;
        let styled_blank = ScrollbackLine::compress(&styled_blank, false).layout(4);
        assert_eq!(
            styled_blank.active_len, 0,
            "P0 history reflow strips a trailing foreground-styled blank"
        );

        let mut decorated_blank = vec![TerminalCell::default(); 8];
        decorated_blank[3].flags.set_bold(true);
        let decorated_blank = ScrollbackLine::compress(&decorated_blank, false).layout(5);
        assert_eq!(
            decorated_blank.active_len, 0,
            "P0 history reflow strips a trailing style-only blank"
        );

        let mut background_blank = vec![TerminalCell::default(); 8];
        background_blank[3].background = Color::Blue;
        let background_blank = ScrollbackLine::compress(&background_blank, false).layout(6);
        assert_eq!(
            background_blank.active_len, 4,
            "P0 history reflow retains a trailing background-painted blank"
        );
    }

    #[test]
    fn identity_projection_plan_matches_eager_history_reflow_across_widths() {
        for cols in 1..=9 {
            let mut terminal = TerminalState::new(cols, 3);
            let mut first = vec![TerminalCell::default(); 7];
            first[0].character = 'A';
            first[1].character = '中';
            first[1].flags.set_wide(true);
            first[2].flags.set_wide_continuation(true);
            first[4].foreground = Color::Red;
            let mut second = vec![TerminalCell::default(); 7];
            second[0].character = 'B';
            second[1].character = 'C';
            let history = vec![
                ScrollbackLine::compress(&first, true),
                ScrollbackLine::compress(&second, false),
                ScrollbackLine::compress(&[TerminalCell::default(); 7], false),
            ];
            for line in &history {
                terminal.push_scrollback_compressed(line.clone());
            }

            let plan = terminal.identity_projection_plan(cols);
            let eager = TerminalState::reflow_projected_origins(&history, cols, 0);
            assert_eq!(
                plan.rows.len(),
                eager.len() + terminal.grid.rows(),
                "width {cols}"
            );
            for (planned, eager) in plan.rows.iter().zip(&eager) {
                let mut expected: Vec<_> = eager
                    .spans
                    .iter()
                    .map(|span| super::RawSlice {
                        view_col_start: span.view_col_start,
                        source: super::RawSliceSource {
                            absolute_row: span.raw_absolute_row,
                            col_start: span.raw_col_start,
                        },
                        origin: Some(super::RawSliceOrigin {
                            row: span.raw_row,
                            col_start: span.raw_col_start,
                        }),
                        len: span.view_col_end - span.view_col_start,
                        narrow_wide_body: false,
                    })
                    .collect();
                if cols == 1 {
                    for slice in &mut expected {
                        let line = &history[slice.source.absolute_row];
                        slice.narrow_wide_body = line
                            .layout(slice.source.absolute_row)
                            .wide_continuations
                            .binary_search(&(slice.source.col_start + 1))
                            .is_ok();
                    }
                }
                assert_eq!(planned.raw_slices.as_slice(), expected, "width {cols}");
                assert_eq!(planned.row_source, eager.row_source, "width {cols}");
            }
        }
    }

    #[test]
    fn identity_projection_plan_trailing_blank_rules_match_eager_p0() {
        let mut foreground = vec![TerminalCell::default(); 8];
        foreground[3].foreground = Color::Red;
        let mut style = vec![TerminalCell::default(); 8];
        style[3].flags.set_bold(true);
        let mut background = vec![TerminalCell::default(); 8];
        background[3].background = Color::Blue;
        let history = vec![
            ScrollbackLine::compress(&foreground, false),
            ScrollbackLine::compress(&style, false),
            ScrollbackLine::compress(&background, false),
        ];

        let plan = super::ProjectionPlan::identity(
            history
                .iter()
                .enumerate()
                .map(|(row, line)| line.layout(row)),
            std::iter::empty(),
            2,
        );
        let eager = TerminalState::reflow_projected_origins(&history, 2, 0);
        assert_eq!(plan.rows.len(), eager.len());
        for (planned, eager) in plan.rows.iter().zip(&eager) {
            assert_eq!(planned.raw_slices.len(), eager.spans.len());
            for (slice, span) in planned.raw_slices.iter().zip(&eager.spans) {
                assert_eq!(slice.view_col_start, span.view_col_start);
                assert_eq!(slice.origin.map(|origin| origin.row), Some(span.raw_row));
                assert_eq!(slice.source.absolute_row, span.raw_absolute_row);
                assert_eq!(slice.source.col_start, span.raw_col_start);
                assert_eq!(slice.len, span.view_col_end - span.view_col_start);
            }
        }
        assert!(plan.rows[0].raw_slices.is_empty());
        assert!(plan.rows[1].raw_slices.is_empty());
        assert_eq!(plan.rows[2].raw_slices[0].len, 2);
        assert_eq!(plan.rows[3].raw_slices[0].source.col_start, 2);
    }

    #[test]
    fn identity_projection_plan_keeps_history_grid_boundary_and_wide_narrowing() {
        let mut terminal = TerminalState::new(1, 2);
        let mut wide = vec![TerminalCell::default(); 3];
        wide[0].character = '中';
        wide[0].flags.set_wide(true);
        wide[1].flags.set_wide_continuation(true);
        terminal.push_scrollback_compressed(ScrollbackLine::compress(&wide, true));
        terminal.grid.get_mut(0, 0).character = 'G';
        terminal.grid.row_wrapped[0] = true;

        let plan = terminal.identity_projection_plan(1);
        assert_eq!(
            plan.rows.len(),
            3,
            "one history glyph row plus two grid rows"
        );
        assert!(plan.rows[0].raw_slices[0].narrow_wide_body);
        assert_eq!(
            plan.rows[1].row_source,
            Some(super::RowSource {
                raw_row: terminal.grid.row_ids[0],
                raw_absolute_row: 1,
            })
        );
        assert_eq!(plan.rows[1].raw_slices[0].source.col_start, 0);
        assert!(
            plan.rows[1].wrapped,
            "grid wrapping is metadata, not a join"
        );
    }

    #[test]
    fn identity_projection_plan_untracked_sources_keep_bytes_but_origins_fail_closed() {
        let mut terminal = TerminalState::new(6, 2);
        terminal.grid.get_mut(0, 0).character = 'x';
        terminal.grid.get_mut(1, 0).character = 'g';
        terminal.grid.row_ids[1] = super::RawRowId::UNTRACKED;
        let mut history_cells = vec![TerminalCell::default(); 6];
        history_cells[0].character = 'h';
        history_cells[1].foreground = Color::Red;
        terminal.push_scrollback_compressed(ScrollbackLine::compress_with_raw_row_id(
            &history_cells,
            false,
            super::RawRowId::UNTRACKED,
        ));

        let plan = terminal.identity_projection_plan(6);
        assert_eq!(plan.rows.len(), 3);
        assert_eq!(plan.rows[0].raw_slices.len(), 1);
        assert_eq!(plan.rows[0].raw_slices[0].view_col_start, 0);
        assert_eq!(plan.rows[0].raw_slices[0].source.col_start, 0);
        assert_eq!(plan.rows[0].raw_slices[0].origin, None);
        assert_eq!(plan.rows[0].row_source, None);
        assert_eq!(plan.rows[1].raw_slices[0].len, 6);
        assert_eq!(
            plan.rows[1].row_source,
            Some(super::RowSource {
                raw_row: terminal.grid.row_ids[0],
                raw_absolute_row: 1,
            })
        );
        assert_eq!(plan.rows[2].raw_slices.len(), 1);
        assert_eq!(plan.rows[2].raw_slices[0].origin, None);
        assert_eq!(plan.rows[2].row_source, None);

        let actual = terminal.materialize_identity_projection_plan(&plan);
        let mut expected =
            TerminalState::reflow_projected_origins(terminal.scrollback.make_contiguous(), 6, 0)
                .into_iter()
                .map(|line| line.cells)
                .collect::<Vec<_>>();
        expected.extend(terminal.grid.to_vec());
        assert_eq!(actual, expected, "UNTRACKED affects origins, never cells");
    }

    #[test]
    fn identity_projection_plan_is_layout_only_and_linear_for_deep_history() {
        let mut terminal = TerminalState::new(80, 2);
        terminal.set_max_scrollback(10_001);
        let mut cells = vec![TerminalCell::default(); 80];
        cells[0].character = 'x';
        cells[0].foreground = Color::Red;
        for _ in 0..10_000 {
            terminal.push_scrollback_compressed(ScrollbackLine::compress(&cells, false));
        }
        assert!(terminal
            .scrollback
            .iter()
            .all(|line| matches!(line.data, super::CompressedLineData::Encoded(_))));

        super::PROJECTED_HISTORY_DECOMPRESS_COUNT.with(|count| count.set(0));
        super::PROJECTED_HISTORY_LAYOUT_BYTE_SCAN_COUNT.with(|count| count.set(0));
        let plan = terminal.identity_projection_plan(80);
        assert_eq!(
            super::PROJECTED_HISTORY_DECOMPRESS_COUNT.with(|count| count.get()),
            0,
            "full-document planning must not materialize scrollback cells"
        );
        assert_eq!(
            super::PROJECTED_HISTORY_LAYOUT_BYTE_SCAN_COUNT.with(|count| count.get()),
            0,
            "cached layout planning must not scan encoded cell records"
        );
        assert_eq!(plan.rows.len(), 10_000 + terminal.grid.rows());
        assert!(plan.raw_slice_count <= plan.rows.len());
        assert_eq!(
            plan.metadata_units(),
            plan.rows.len() + plan.raw_rows.len() + plan.raw_slice_count
        );
        assert!(plan.metadata_units() <= 3 * plan.rows.len());
    }

    #[test]
    fn identity_projection_matches_live_cells_and_round_trips_every_cell() {
        let mut terminal = TerminalState::new(6, 3);
        terminal.process_batch("A中Z".as_bytes());
        let expected = terminal.get_visible_cells();
        let row_ids: Vec<_> = (0..terminal.grid.rows())
            .map(|row| terminal.grid.raw_row_id(row).expect("retained row id"))
            .collect();

        let projection = terminal.get_projected_viewport(true);
        assert!(projection.is_identity());
        assert!(projection.uses_identity_fast_path());
        assert_eq!(projection.cells(), expected.as_ref());
        assert_eq!(projection.row_wrapped(), terminal.grid.row_wrapped);
        assert!(std::sync::Arc::ptr_eq(&expected, &projection.cells));

        for (row, row_id) in row_ids.into_iter().enumerate() {
            for col in 0..terminal.grid.cols() {
                let view = super::ViewportCell { row, col };
                let origin = projection.view_to_raw(view).expect("live raw origin");
                assert_eq!(origin, super::RawCellOrigin { row: row_id, col });
                assert_eq!(projection.raw_to_view(origin), Some(view));
            }
        }

        let wide_body = projection
            .view_to_raw(super::ViewportCell { row: 0, col: 1 })
            .expect("wide body origin");
        let continuation = projection
            .view_to_raw(super::ViewportCell { row: 0, col: 2 })
            .expect("wide continuation origin");
        assert_eq!(wide_body.row, continuation.row);
        assert_eq!(continuation.col, wide_body.col + 1);
        assert_ne!(wide_body, continuation);

        let cached = terminal.get_projected_viewport(true);
        assert!(std::sync::Arc::ptr_eq(&projection, &cached));
        assert_eq!(projection.view_revision(), cached.view_revision());
    }

    #[test]
    fn finished_output_range_keeps_same_row_prompt_suffix_outside() {
        let mut terminal = TerminalState::new(20, 4);
        terminal.process_input(
            "\x1b]133;A\x07$ \x1b]133;B\x07cmd\x1b]133;C\x07中Z\x1b]133;D;0\x07".as_bytes(),
        );
        let zone_id = terminal.command_zones.back().expect("finished zone").id;
        let row_id = terminal.grid.raw_row_id(0).expect("tracked output row");
        let before_suffix = terminal
            .finished_output_range(zone_id)
            .expect("exact finished output");
        assert_eq!(
            before_suffix.start,
            super::RawCellBoundary {
                row: row_id,
                col: 5
            }
        );
        assert_eq!(
            before_suffix.end,
            super::RawCellBoundary {
                row: row_id,
                col: 8
            }
        );

        terminal.process_input("界$ next".as_bytes());
        assert_eq!(terminal.finished_output_range(zone_id), Some(before_suffix));
    }

    #[test]
    fn finished_output_range_rejects_cursor_back_but_accepts_pending_wrap() {
        let mut cursor_back = TerminalState::new(8, 4);
        cursor_back.process_input(
            b"\x1b]133;A\x07$ \x1b]133;B\x07x\r\n\x1b]133;C\x07abc\r\x1b]133;D;0\x07",
        );
        let id = cursor_back.command_zones.back().expect("finished zone").id;
        assert_eq!(cursor_back.finished_output_range(id), None);

        let mut wrapped = TerminalState::new(8, 4);
        wrapped.process_input(
            b"\x1b]133;A\x07$ \x1b]133;B\x07x\r\n\x1b]133;C\x0712345678\x1b]133;D;0\x07",
        );
        let id = wrapped.command_zones.back().expect("finished zone").id;
        let range = wrapped
            .finished_output_range(id)
            .expect("pending-wrap range");
        assert_eq!(range.end.col, 8);
    }

    #[test]
    fn finished_output_range_includes_trailing_blank_rows_before_marker() {
        let mut terminal = TerminalState::new(12, 5);
        terminal.process_input(
            b"\x1b]133;A\x07$ \x1b]133;B\x07x\r\n\x1b]133;C\x07out\r\n\r\n\x1b]133;D;0\x07",
        );
        let zone = terminal.command_zones.back().expect("finished zone");
        assert_eq!(zone.output_start, Some(1));
        assert_eq!(zone.output_end, Some(3));
        let range = terminal
            .finished_output_range(zone.id)
            .expect("blank rows retain exact provenance");
        assert_eq!(
            range.end.row,
            terminal.grid.raw_row_id(2).expect("last blank output row")
        );
        assert_eq!(range.end.col, 12);
        assert_eq!(
            terminal.finished_output_provenance[&zone.id].row_ids.len(),
            2
        );
    }

    #[test]
    fn finished_output_range_expands_wide_boundaries_and_rejects_replacement() {
        let mut terminal = TerminalState::new(8, 3);
        terminal.grid[0][2].character = '中';
        terminal.grid[0][2].flags.set_wide(true);
        terminal.grid[0][3].flags.set_wide_continuation(true);
        let provenance = terminal
            .bind_finished_output_provenance(0, 3, 0, 3)
            .expect("wide boundaries become nonempty");
        assert_eq!(provenance.range.start.col, 2);
        assert_eq!(provenance.range.end.col, 4);
        let lead_end = terminal
            .bind_finished_output_provenance(0, 0, 0, 2)
            .expect("wide lead is an exact boundary before the glyph");
        assert_eq!(lead_end.range.end.col, 2);

        terminal.process_input(
            b"\r\x1b]133;A\x07$ \x1b]133;B\x07x\r\n\x1b]133;C\x07out\x1b]133;D;0\x07",
        );
        let zone_id = terminal.command_zones.back().expect("finished zone").id;
        let range = terminal
            .finished_output_range(zone_id)
            .expect("bound provenance");
        let row = terminal
            .grid
            .row_ids
            .iter()
            .position(|row| *row == range.start.row)
            .expect("range row retained");
        terminal.grid.row_ids[row] = super::RawRowId::fresh();
        assert_eq!(terminal.finished_output_range(zone_id), None);
    }

    #[test]
    fn collapse_plan_splices_same_row_prefix_summary_and_suffix_without_mutating_raw() {
        let mut terminal = TerminalState::new(20, 4);
        terminal.process_input(
            "\x1b]133;A\x07$ \x1b]133;B\x07cmd\x1b]133;C\x07中Z\x1b]133;D;0\x07界$ next".as_bytes(),
        );
        let zone_id = terminal.command_zones.back().expect("finished zone").id;
        let raw_before = terminal.grid.to_vec();
        let identity = terminal.identity_projection_plan(20);
        let mut policy = super::ProjectionPolicy::new();
        assert!(policy.collapse(zone_id));
        let collapsed = terminal.collapsed_projection_plan(20, &policy);

        assert_eq!(
            terminal.grid.to_vec(),
            raw_before,
            "projection is read-only"
        );
        assert_eq!(
            collapsed.effective_collapsed,
            std::collections::BTreeSet::from([zone_id])
        );
        assert_eq!(collapsed.rows.len(), identity.rows.len() + 2);
        assert_eq!(collapsed.rows[0].raw_slices[0].source.col_start, 0);
        assert_eq!(collapsed.rows[0].raw_slices[0].len, 5);
        assert!(matches!(
            collapsed.rows[1].kind,
            super::ProjectedRowKind::CollapsedSummary {
                key: super::SyntheticRowKey { zone_id: id, .. },
                hidden_display_rows: 1,
                ..
            } if id == zone_id
        ));
        assert!(collapsed.rows[1].raw_slices.is_empty());
        assert_eq!(collapsed.rows[2].raw_slices[0].view_col_start, 8);
        assert_eq!(collapsed.rows[2].raw_slices[0].source.col_start, 8);
        assert_eq!(
            collapsed.rows[2].raw_slices[0].len, 12,
            "the post-output prompt suffix keeps its original columns"
        );
        let materialized = terminal.materialize_identity_projection_plan(&collapsed);
        assert_eq!(materialized[0][0].character, '$');
        assert!(materialized[1]
            .iter()
            .all(|cell| *cell == TerminalCell::default()));
        assert_eq!(materialized[2][8].character, '界');
        assert_eq!(materialized[2][10].character, '$');
    }

    #[test]
    fn collapse_plan_hides_multiple_rows_and_counts_blank_output_row() {
        let mut terminal = TerminalState::new(12, 5);
        terminal.process_input(
            b"\x1b]133;A\x07$ \x1b]133;B\x07x\r\n\x1b]133;C\x07out\r\n\r\n\x1b]133;D;0\x07tail",
        );
        let zone_id = terminal.command_zones.back().expect("finished zone").id;
        let mut policy = super::ProjectionPolicy::new();
        assert!(policy.collapse(zone_id));
        let collapsed = terminal.collapsed_projection_plan(12, &policy);
        let summary = collapsed
            .rows
            .iter()
            .find_map(|row| match row.kind {
                super::ProjectedRowKind::CollapsedSummary {
                    hidden_display_rows,
                    ..
                } => Some(hidden_display_rows),
                super::ProjectedRowKind::Raw | super::ProjectedRowKind::Padding => None,
            })
            .expect("one summary row");
        assert_eq!(summary, 2, "visible output plus its blank line are hidden");
        assert_eq!(
            collapsed
                .rows
                .iter()
                .filter(|row| matches!(row.kind, super::ProjectedRowKind::CollapsedSummary { .. }))
                .count(),
            1
        );
        let visible = terminal.materialize_identity_projection_plan(&collapsed);
        assert!(visible.iter().flatten().any(|cell| cell.character == 't'));
        assert!(!visible.iter().flatten().any(|cell| cell.character == 'o'));
    }

    #[test]
    fn collapse_plan_keeps_two_disjoint_ranges_on_one_raw_row() {
        let mut terminal = TerminalState::new(12, 2);
        terminal.process_input(b"abcdefghijkl");
        let raw_row = terminal.grid.raw_row_id(0).expect("tracked grid row");
        let base = terminal.identity_projection_plan(12);
        let collapses = [
            super::ResolvedCollapse {
                range: super::FinishedOutputRange {
                    zone_id: 41,
                    start: super::RawCellBoundary {
                        row: raw_row,
                        col: 2,
                    },
                    end: super::RawCellBoundary {
                        row: raw_row,
                        col: 4,
                    },
                },
                start_absolute: 0,
                end_absolute: 0,
            },
            super::ResolvedCollapse {
                range: super::FinishedOutputRange {
                    zone_id: 42,
                    start: super::RawCellBoundary {
                        row: raw_row,
                        col: 6,
                    },
                    end: super::RawCellBoundary {
                        row: raw_row,
                        col: 8,
                    },
                },
                start_absolute: 0,
                end_absolute: 0,
            },
        ];
        let collapsed = base.splice_collapses(&collapses, 9);
        assert_eq!(
            collapsed.effective_collapsed,
            std::collections::BTreeSet::from([41, 42])
        );
        assert_eq!(
            collapsed
                .rows
                .iter()
                .filter(|row| matches!(row.kind, super::ProjectedRowKind::CollapsedSummary { .. }))
                .count(),
            2
        );
        let visible = terminal.materialize_identity_projection_plan(&collapsed);
        let text: String = visible
            .iter()
            .flat_map(|row| row.iter().map(|cell| cell.character))
            .collect();
        assert!(text.contains("ab"));
        assert!(text.contains("ef"));
        assert!(text.contains("ijkl"));
        assert!(!text.contains('c'));
        assert!(!text.contains('g'));
    }

    #[test]
    fn has_prompt_marks_separates_no_shell_integration_from_no_prompt_that_way() {
        // A shell that never emits OSC 133: prompt navigation refuses, and the
        // pane must be able to say the refusal is about shell integration.
        let mut plain = TerminalState::new(20, 4);
        plain.process_input(b"$ ls\r\nfile\r\n");
        assert!(!plain.has_prompt_marks());
        assert!(!plain.jump_to_prev_prompt());

        // With marks present the same refusal means "no prompt that way",
        // which is a different message.
        let mut marked = TerminalState::new(20, 4);
        marked.process_input(
            b"\x1b]133;A\x07$ \x1b]133;B\x07one\x1b]133;C\x07out\r\n\x1b]133;D;0\x07",
        );
        assert!(marked.has_prompt_marks());
        // Already at the live view, so there is no earlier prompt above it.
        assert!(!marked.jump_to_next_prompt());

        // Marks survive their rows being trimmed only while a zone keeps live
        // rows; an all-evicted pane reports no marks rather than pretending.
        marked.set_max_scrollback(1);
        for _ in 0..64 {
            marked.process_input(b"filler\r\n");
        }
        assert!(marked.command_zones.iter().all(|zone| zone.rows_evicted));
        assert!(!marked.has_prompt_marks());
    }

    #[test]
    fn provenance_orphan_scan_runs_only_on_trims_that_evict_a_zone() {
        // Two finished zones with captured-output provenance, then a small
        // scrollback cap so every further line trims exactly one row.
        let mut terminal = TerminalState::new(20, 2);
        terminal.set_max_scrollback(8);
        terminal.process_input(
            b"\x1b]133;A\x07$ \x1b]133;B\x07one\x1b]133;C\x07out1\r\n\x1b]133;D;0\x07",
        );
        terminal.process_input(
            b"\x1b]133;A\x07$ \x1b]133;B\x07two\x1b]133;C\x07out2\r\n\x1b]133;D;0\x07",
        );
        let zones: Vec<u64> = terminal.command_zones.iter().map(|zone| zone.id).collect();
        assert_eq!(zones.len(), 2);
        assert_eq!(terminal.finished_output_provenance.len(), 2);

        // Fill scrollback to the cap without evicting either zone's rows.
        while terminal.scrollback.len() < 8 {
            terminal.process_input(b"filler\r\n");
        }
        assert!(terminal.command_zones.iter().all(|zone| !zone.rows_evicted));

        // Steady state: every line now trims a row. Until a trim reaches a
        // zone's prompt row the rescan must not run at all.
        super::PROVENANCE_ORPHAN_SCAN_COUNT.with(|count| count.set(0));
        let evicted_before = terminal
            .command_zones
            .iter()
            .filter(|zone| zone.rows_evicted)
            .count();
        terminal.process_input(b"filler\r\n");
        let evicted_after = terminal
            .command_zones
            .iter()
            .filter(|zone| zone.rows_evicted)
            .count();
        let scans = super::PROVENANCE_ORPHAN_SCAN_COUNT.with(|count| count.get());
        assert_eq!(scans, usize::from(evicted_after > evicted_before));

        // Trim until both zones lose their rows; provenance is still dropped.
        for _ in 0..32 {
            terminal.process_input(b"filler\r\n");
        }
        assert!(terminal.command_zones.iter().all(|zone| zone.rows_evicted));
        assert!(terminal.finished_output_provenance.is_empty());
        assert!(super::PROVENANCE_ORPHAN_SCAN_COUNT.with(|count| count.get()) > 0);
    }

    #[test]
    fn collapse_policy_prunes_stale_and_overlapping_ranges_as_a_component() {
        let mut terminal = TerminalState::new(20, 4);
        terminal.process_input(
            b"\x1b]133;A\x07$ \x1b]133;B\x07cmd\x1b]133;C\x07output\x1b]133;D;0\x07",
        );
        let first = terminal
            .command_zones
            .back()
            .expect("finished zone")
            .clone();
        let first_provenance = terminal.finished_output_provenance[&first.id].clone();
        let second_id = first.id + 10_000;
        let mut second = first.clone();
        second.id = second_id;
        let mut second_provenance = first_provenance;
        second_provenance.range.zone_id = second_id;
        terminal.command_zones.push_back(second);
        terminal
            .finished_output_provenance
            .insert(second_id, second_provenance);

        let mut policy = super::ProjectionPolicy::new();
        assert!(policy.collapse(first.id));
        assert!(policy.collapse(second_id));
        assert!(policy.collapse(u64::MAX));
        assert!(terminal.resolved_collapses(&policy).is_empty());
        let plan = terminal.collapsed_projection_plan(20, &policy);
        assert!(plan.effective_collapsed.is_empty());
        assert!(plan
            .rows
            .iter()
            .all(|row| matches!(row.kind, super::ProjectedRowKind::Raw)));
    }

    #[test]
    fn collapse_plan_is_layout_only_and_metadata_linear_for_deep_history() {
        let mut terminal = TerminalState::new(80, 2);
        terminal.set_max_scrollback(10_001);
        let mut cells = vec![TerminalCell::default(); 80];
        cells[0].character = 'x';
        cells[0].foreground = Color::Red;
        for _ in 0..10_000 {
            terminal.push_scrollback_compressed(ScrollbackLine::compress(&cells, false));
        }
        let identity = terminal.identity_projection_plan(80);
        let collapses: Vec<_> = (0..128)
            .map(|index| {
                let absolute = index * 64;
                let row = terminal.scrollback[absolute].raw_row_id();
                super::ResolvedCollapse {
                    range: super::FinishedOutputRange {
                        zone_id: index as u64,
                        start: super::RawCellBoundary { row, col: 0 },
                        end: super::RawCellBoundary { row, col: 1 },
                    },
                    start_absolute: absolute,
                    end_absolute: absolute,
                }
            })
            .collect();
        super::PROJECTED_HISTORY_DECOMPRESS_COUNT.with(|count| count.set(0));
        let base_units = identity.metadata_units();
        let collapsed = identity.splice_collapses(&collapses, 2);
        assert_eq!(
            super::PROJECTED_HISTORY_DECOMPRESS_COUNT.with(|count| count.get()),
            0
        );
        assert_eq!(collapsed.effective_collapsed.len(), collapses.len());
        assert!(collapsed.metadata_units() <= base_units + 4 * collapses.len());
        assert!(collapsed.rows.len() <= 10_002 + 2 * collapses.len());
    }

    #[test]
    fn projected_viewport_identity_and_stale_policies_share_the_p0_arc() {
        let mut terminal = TerminalState::new(12, 4);
        terminal.process_input(b"plain");
        let identity = super::ProjectionPolicy::new();
        let base = terminal.get_projected_viewport(true);
        let same = terminal.get_projected_viewport_with_policy(true, &identity, 0);
        assert!(std::sync::Arc::ptr_eq(&base, &same));
        assert!(std::sync::Arc::ptr_eq(&base.cells, &same.cells));

        let mut stale = super::ProjectionPolicy::new();
        assert!(stale.collapse(u64::MAX));
        let stale_view = terminal.get_projected_viewport_with_policy(true, &stale, 0);
        assert!(std::sync::Arc::ptr_eq(&base, &stale_view));

        let bypass = terminal.get_projected_viewport(false);
        let bypass_policy = terminal.get_projected_viewport_with_policy(false, &stale, 99);
        assert!(std::sync::Arc::ptr_eq(&bypass, &bypass_policy));
        assert_eq!(bypass_policy.mode(), super::ProjectionMode::Bypass);
    }

    #[test]
    fn deep_stale_collapse_policy_does_not_build_a_projection_plan() {
        let mut terminal = TerminalState::new(80, 4);
        terminal.set_max_scrollback(10_001);
        let mut cells = vec![TerminalCell::default(); 80];
        cells[0].character = 'x';
        for _ in 0..10_000 {
            terminal.push_scrollback_compressed(ScrollbackLine::compress(&cells, false));
        }
        let base = terminal.get_projected_viewport(true);
        let mut stale = super::ProjectionPolicy::new();
        assert!(stale.collapse(u64::MAX));
        super::PROJECTION_PLAN_BUILD_COUNT.with(|count| count.set(0));

        let projected = terminal.get_projected_viewport_with_policy(true, &stale, 0);

        assert!(std::sync::Arc::ptr_eq(&base, &projected));
        assert_eq!(
            super::PROJECTION_PLAN_BUILD_COUNT.with(|count| count.get()),
            0
        );
    }

    #[test]
    fn collapse_with_no_projected_cell_intersection_shares_the_identity_arc() {
        let mut terminal = TerminalState::new(20, 2);
        terminal
            .process_input(b"\x1b]133;A\x07$ \x1b]133;B\x07cmd\x1b]133;C\x07   \x1b]133;D;0\x07");
        let zone_id = terminal.command_zones.back().expect("finished zone").id;
        let range = terminal
            .finished_output_range(zone_id)
            .expect("spaces still have exact raw provenance");
        assert!(range.start.col < range.end.col);
        terminal.process_input(b"\r\n\r\n\r\n");
        assert!(terminal.scrollback.iter().any(|line| {
            line.raw_row_id() == range.start.row
                && usize::from(line.projected_active_len) == range.start.col
        }));

        let identity = terminal.get_projected_viewport(true);
        let mut policy = super::ProjectionPolicy::new();
        assert!(policy.collapse(zone_id));
        let projected = terminal.get_projected_viewport_with_policy(true, &policy, 0);

        assert!(std::sync::Arc::ptr_eq(&identity, &projected));
        assert_eq!(projected.mode(), super::ProjectionMode::Identity);
        assert!(projected.effective_collapsed().is_empty());
    }

    #[test]
    fn transformed_viewport_materializes_summary_origins_and_same_row_suffix() {
        let mut terminal = TerminalState::new(20, 6);
        terminal.process_input(
            "\x1b]133;A\x07$ \x1b]133;B\x07cmd\x1b]133;C\x07中Z\x1b]133;D;0\x07界$ next".as_bytes(),
        );
        let zone_id = terminal.command_zones.back().expect("finished zone").id;
        let range = terminal
            .finished_output_range(zone_id)
            .expect("exact output range");
        let mut policy = super::ProjectionPolicy::new();
        assert!(policy.collapse(zone_id));
        terminal.current_bg = Color::Red;
        let viewport = terminal.get_projected_viewport_with_policy(true, &policy, 2);

        assert_eq!(viewport.mode(), super::ProjectionMode::Transformed);
        assert!(!viewport.is_identity());
        assert_eq!(viewport.policy_revision(), policy.revision());
        assert_eq!(
            viewport.effective_collapsed(),
            &std::collections::BTreeSet::from([zone_id])
        );
        assert_eq!(viewport.document_rows(), 8);
        assert_eq!(viewport.document_start(), 0);
        assert_eq!(viewport.max_scroll_offset(), 2);
        let summary_row = viewport
            .row_kinds()
            .iter()
            .position(|kind| matches!(kind, super::ProjectedRowKind::CollapsedSummary { .. }))
            .expect("summary row visible");
        assert!(viewport.cells()[summary_row]
            .iter()
            .all(|cell| *cell == TerminalCell::default()));
        assert_eq!(
            viewport.view_to_raw(super::ViewportCell {
                row: summary_row,
                col: 0,
            }),
            None
        );
        assert_eq!(
            viewport.raw_to_view(super::RawCellOrigin {
                row: range.start.row,
                col: range.start.col,
            }),
            None,
            "hidden output has no projected origin"
        );
        let suffix = viewport
            .raw_to_view(super::RawCellOrigin {
                row: range.end.row,
                col: range.end.col,
            })
            .expect("same-row suffix remains mapped");
        assert_eq!(
            viewport.raw_range_to_view(
                super::RawCellOrigin {
                    row: range.end.row,
                    col: range.end.col,
                },
                2,
            ),
            Some(suffix),
            "one affine suffix span maps in one logarithmic query"
        );
        assert_eq!(
            viewport.raw_range_to_view(
                super::RawCellOrigin {
                    row: range.start.row,
                    col: range.start.col.saturating_sub(1),
                },
                range
                    .end
                    .col
                    .saturating_sub(range.start.col)
                    .saturating_add(2),
            ),
            None,
            "a raw range must not bridge the collapsed output hole"
        );
        assert_eq!(
            viewport.raw_range_to_view(
                super::RawCellOrigin {
                    row: range.end.row,
                    col: range.end.col,
                },
                0,
            ),
            None
        );
        assert_eq!(
            viewport.raw_range_to_view(
                super::RawCellOrigin {
                    row: range.end.row,
                    col: usize::MAX,
                },
                1,
            ),
            None,
            "overflowing half-open ranges fail closed"
        );
        assert_eq!(suffix.col, range.end.col);
        assert_eq!(viewport.cells()[suffix.row][suffix.col].character, '界');
        assert_eq!(viewport.view_document_row(summary_row), Some(1));

        terminal.select_line_in_projection(&viewport, suffix.row);
        let selected_suffix = terminal
            .copy_selection()
            .expect("suffix-only line selection");
        assert!(selected_suffix.contains("界$ next"));
        assert!(!selected_suffix.contains('中'));
    }

    #[test]
    fn transformed_viewport_late_materializer_decodes_only_visible_unique_history() {
        let mut terminal = TerminalState::new(80, 2);
        terminal.set_max_scrollback(10_001);
        let mut cells = vec![TerminalCell::default(); 80];
        cells[0].character = 'x';
        cells[0].foreground = Color::Red;
        for _ in 0..10_000 {
            terminal.push_scrollback_compressed(ScrollbackLine::compress(&cells, false));
        }
        let identity = terminal.identity_projection_plan(80);
        let absolute = 5_000;
        let row = terminal.scrollback[absolute].raw_row_id();
        let collapse = super::ResolvedCollapse {
            range: super::FinishedOutputRange {
                zone_id: 77,
                start: super::RawCellBoundary { row, col: 0 },
                end: super::RawCellBoundary { row, col: 1 },
            },
            start_absolute: absolute,
            end_absolute: absolute,
        };
        let plan = identity.splice_collapses(&[collapse], 2);
        let summary_row = plan
            .rows
            .iter()
            .position(|row| matches!(row.kind, super::ProjectedRowKind::CollapsedSummary { .. }))
            .expect("summary in full plan");
        let max_start = plan.rows.len().saturating_sub(terminal.grid.rows());
        let offset = max_start.saturating_sub(summary_row.min(max_start));

        super::PROJECTED_HISTORY_DECOMPRESS_COUNT.with(|count| count.set(0));
        let materialized =
            terminal.materialize_projection_plan(&plan, offset, terminal.grid.rows(), 80);
        assert_eq!(materialized.document_start, summary_row);
        assert!(matches!(
            materialized.row_kinds[0],
            super::ProjectedRowKind::CollapsedSummary { .. }
        ));
        assert!(materialized
            .provenance
            .origin_spans
            .iter()
            .all(|span| span.raw_row != row));
        assert_eq!(
            super::PROJECTED_HISTORY_DECOMPRESS_COUNT.with(|count| count.get()),
            1,
            "summary source stays cold and the one visible raw row decodes once"
        );
    }

    #[test]
    fn projected_structural_padding_ignores_the_live_sgr_background() {
        let mut terminal = TerminalState::new(6, 2);
        terminal.current_bg = Color::Red;
        let plan = terminal.identity_projection_plan(6);
        let materialized = terminal.materialize_projection_plan(&plan, 0, 4, 6);

        assert_eq!(materialized.top_padding, 2);
        for row in &materialized.cells[..materialized.top_padding] {
            assert!(row.iter().all(|cell| *cell == TerminalCell::default()));
        }
        assert!(materialized.row_kinds[..materialized.top_padding]
            .iter()
            .all(|kind| *kind == super::ProjectedRowKind::Padding));
    }

    #[test]
    fn transformed_plan_cache_survives_view_scroll_and_ordinary_cell_updates() {
        let mut terminal = TerminalState::new(20, 6);
        terminal.process_input(
            b"\x1b]133;A\x07$ \x1b]133;B\x07cmd\x1b]133;C\x07output\x1b]133;D;0\x07",
        );
        let zone_id = terminal.command_zones.back().expect("finished zone").id;
        let mut policy = super::ProjectionPolicy::new();
        assert!(policy.collapse(zone_id));
        super::PROJECTION_PLAN_BUILD_COUNT.with(|count| count.set(0));

        let first = terminal.get_projected_viewport_with_policy(true, &policy, 2);
        let cached = terminal.get_projected_viewport_with_policy(true, &policy, 2);
        assert!(std::sync::Arc::ptr_eq(&first, &cached));
        let _scrolled = terminal.get_projected_viewport_with_policy(true, &policy, 1);
        terminal.process_batch(b"!");
        let updated = terminal.get_projected_viewport_with_policy(true, &policy, 1);
        assert!(!std::sync::Arc::ptr_eq(&_scrolled, &updated));
        assert!(updated
            .cells()
            .iter()
            .flatten()
            .any(|cell| cell.character == '!'));
        assert_eq!(
            super::PROJECTION_PLAN_BUILD_COUNT.with(|count| count.get()),
            1,
            "scroll and cell content updates reuse the full-document plan"
        );
    }

    #[test]
    fn projection_view_state_preserves_bottom_and_raw_or_summary_top_anchor() {
        let mut terminal = TerminalState::new(12, 4);
        for index in 0..8 {
            terminal.process_input(
                format!(
                    "\x1b]133;A\x07$ \x1b]133;B\x07c{index}\r\n\x1b]133;C\x07out{index}\r\n\x1b]133;D;0\x07"
                )
                .as_bytes(),
            );
        }
        let zone_id = terminal.command_zones[3].id;
        let range = terminal
            .finished_output_range(zone_id)
            .expect("exact middle output");
        let mut policy = super::ProjectionPolicy::new();
        assert!(policy.collapse(zone_id));

        let mut bottom = super::ProjectionViewState::new();
        let bottom_view = terminal.get_projected_viewport_with_state(true, &policy, &mut bottom);
        assert_eq!(bottom.offset_from_bottom(), 0);
        assert_eq!(bottom_view.scroll_offset(), 0);
        terminal.process_input(b"tail");
        let appended = terminal.get_projected_viewport_with_state(true, &policy, &mut bottom);
        assert_eq!(appended.scroll_offset(), 0, "bottom continues following");

        let summary_document_row = terminal
            .cached_collapsed_projection_plan(12, &policy)
            .expect("effective collapse plan")
            .summary_row(zone_id)
            .expect("summary in document");
        let max_start = appended
            .document_rows()
            .saturating_sub(terminal.grid.rows());
        let summary_offset = max_start.saturating_sub(summary_document_row.min(max_start));
        bottom.set_offset(summary_offset, &appended);
        let summary_top = terminal.get_projected_viewport_with_state(true, &policy, &mut bottom);
        assert!(matches!(
            summary_top.row_kinds()[summary_top.top_padding],
            super::ProjectedRowKind::CollapsedSummary { .. }
        ));

        assert!(policy.expand(zone_id));
        let expanded = terminal.get_projected_viewport_with_state(true, &policy, &mut bottom);
        let output_start = expanded
            .raw_to_view(super::RawCellOrigin {
                row: range.start.row,
                col: range.start.col,
            })
            .expect("summary anchor expands to output start");
        assert_eq!(output_start.row, expanded.top_padding);
        assert_ne!(expanded.mode(), super::ProjectionMode::Transformed);
    }

    #[test]
    fn expanding_blank_output_start_preserves_summary_top_with_other_collapse() {
        let mut terminal = TerminalState::new(12, 2);
        terminal.process_input(
            b"\x1b]133;A\x07$ \x1b]133;B\x07one\r\n\x1b]133;C\x07\r\nout-one\r\n\x1b]133;D;0\x07",
        );
        terminal.process_input(
            b"\x1b]133;A\x07$ \x1b]133;B\x07two\r\n\x1b]133;C\x07out-two\r\n\x1b]133;D;0\x07",
        );
        let first_id = terminal.command_zones[0].id;
        let second_id = terminal.command_zones[1].id;
        let first_range = terminal
            .finished_output_range(first_id)
            .expect("first output provenance");
        let identity = terminal.identity_projection_plan(12);
        let start_placement = identity
            .raw_rows
            .iter()
            .find(|placement| placement.raw_row == first_range.start.row)
            .expect("blank output-start row retained");
        assert!(
            identity.rows[start_placement.first_view_row.expect("blank row mapped")]
                .raw_slices
                .is_empty()
        );

        let mut policy = super::ProjectionPolicy::new();
        assert!(policy.collapse(first_id));
        assert!(policy.collapse(second_id));
        let mut state = super::ProjectionViewState::new();
        let initial = terminal.get_projected_viewport_with_state(true, &policy, &mut state);
        let summary_document_row = terminal
            .cached_collapsed_projection_plan(12, &policy)
            .expect("effective collapse plan")
            .summary_row(first_id)
            .expect("first summary");
        let max_start = initial.document_rows().saturating_sub(terminal.grid.rows());
        state.set_offset(
            max_start.saturating_sub(summary_document_row.min(max_start)),
            &initial,
        );
        let summary_top = terminal.get_projected_viewport_with_state(true, &policy, &mut state);
        assert!(matches!(
            summary_top.row_kinds()[summary_top.top_padding],
            super::ProjectedRowKind::CollapsedSummary { key, .. } if key.zone_id == first_id
        ));

        assert!(policy.expand(first_id));
        let expanded = terminal.get_projected_viewport_with_state(true, &policy, &mut state);
        assert_eq!(expanded.mode(), super::ProjectionMode::Transformed);
        assert_eq!(
            expanded
                .raw_row_view_bounds(first_range.start.row)
                .map(|bounds| bounds.0),
            Some(expanded.top_padding),
            "a summary anchored to an empty raw row expands back to that row"
        );
    }

    #[test]
    fn projection_view_state_scroll_is_projection_owned_and_bypass_parks_it() {
        let mut terminal = TerminalState::new(12, 4);
        for index in 0..6 {
            terminal.process_input(
                format!(
                    "\x1b]133;A\x07$ \x1b]133;B\x07c{index}\r\n\x1b]133;C\x07out{index}\r\n\x1b]133;D;0\x07"
                )
                .as_bytes(),
            );
        }
        let zone_id = terminal.command_zones[2].id;
        let mut policy = super::ProjectionPolicy::new();
        assert!(policy.collapse(zone_id));
        let raw_offset = terminal.scroll_offset;
        let mut state = super::ProjectionViewState::new();
        let view = terminal.get_projected_viewport_with_state(true, &policy, &mut state);
        state.scroll(3, &view);
        let scrolled = terminal.get_projected_viewport_with_state(true, &policy, &mut state);
        assert_eq!(
            scrolled.scroll_offset(),
            3.min(scrolled.max_scroll_offset())
        );
        assert_eq!(
            terminal.scroll_offset, raw_offset,
            "raw PTY scroll is untouched"
        );

        let parked_offset = state.offset_from_bottom();
        let bypass = terminal.get_projected_viewport_with_state(false, &policy, &mut state);
        assert_eq!(bypass.mode(), super::ProjectionMode::Bypass);
        assert_eq!(state.offset_from_bottom(), parked_offset);
    }

    #[test]
    fn projected_selection_survives_output_while_a_block_is_collapsed() {
        // Before re-anchoring, one scrolling output line rebuilt the plan and
        // wiped the highlight — so a drag-selection could not survive a
        // background job printing a single line while any block was folded.
        let mut terminal = TerminalState::new(24, 8);
        for (command, output) in [("one", "LEFT"), ("two", "SECRET"), ("three", "RIGHT")] {
            terminal.process_input(
                format!(
                    "\x1b]133;A\x07$ \x1b]133;B\x07{command}\r\n\x1b]133;C\x07{output}\r\n\x1b]133;D;0\x07"
                )
                .as_bytes(),
            );
        }
        let hidden_id = terminal.command_zones[1].id;
        for i in 0..12 {
            terminal.process_input(format!("F{i} filler\r\n").as_bytes());
        }
        terminal.process_input(b"MARKONE\r\nMARKTWO\r\n");
        assert!(!terminal.scrollback.is_empty(), "fixture needs scrollback");

        let mut policy = super::ProjectionPolicy::new();
        assert!(policy.collapse(hidden_id));
        let mut state = super::ProjectionViewState::new();
        let viewport = terminal.get_projected_viewport_with_state(true, &policy, &mut state);
        let locate = |needle: char| {
            viewport
                .cells()
                .iter()
                .enumerate()
                .find_map(|(row, cells)| {
                    cells
                        .iter()
                        .position(|cell| cell.character == needle)
                        .map(|col| (row, col))
                })
                .expect("visible endpoint")
        };
        terminal.start_selection_in_projection(
            &viewport,
            locate('M'),
            super::SelectionMode::Normal,
        );
        terminal.update_selection_in_projection(&viewport, locate('W'));
        let before = terminal.copy_selection().expect("selection copies");
        assert!(before.contains("MARKONE"));

        // One ordinary output line, no keystroke, collapse still active.
        super::PROJECTION_PLAN_BUILD_COUNT.with(|count| count.set(0));
        terminal.process_input(b"tick\r\n");
        let _ = terminal.get_projected_viewport_with_state(true, &policy, &mut state);
        assert_eq!(
            super::PROJECTION_PLAN_BUILD_COUNT.with(|count| count.get()),
            1,
            "fixture must actually rebuild the plan"
        );

        assert!(
            terminal.has_text_selection(),
            "the highlight must survive a line of output"
        );
        let after = terminal.copy_selection().expect("selection still copies");
        // Trailing blanks differ once a row migrates from the live grid into
        // scrollback, so compare the text, not the padding.
        assert!(
            after
                .lines()
                .map(str::trim_end)
                .eq(before.lines().map(str::trim_end)),
            "re-anchored onto different text: {before:?} -> {after:?}"
        );
        // The collapsed block must stay hidden through the round trip.
        assert!(!after.contains("SECRET"));
    }

    #[test]
    fn projected_selection_reanchor_refuses_every_case_that_would_shift_it() {
        let build = || {
            let mut terminal = TerminalState::new(24, 8);
            for (command, output) in [("one", "LEFT"), ("two", "SECRET"), ("three", "RIGHT")] {
                terminal.process_input(
                    format!(
                        "\x1b]133;A\x07$ \x1b]133;B\x07{command}\r\n\x1b]133;C\x07{output}\r\n\x1b]133;D;0\x07"
                    )
                    .as_bytes(),
                );
            }
            let hidden_id = terminal.command_zones[1].id;
            for i in 0..12 {
                terminal.process_input(format!("F{i} filler\r\n").as_bytes());
            }
            terminal.process_input(b"MARKONE\r\nMARKTWO\r\n");
            (terminal, hidden_id)
        };
        let locate = |viewport: &super::ProjectedViewport, needle: char| {
            viewport
                .cells()
                .iter()
                .enumerate()
                .find_map(|(row, cells)| {
                    cells
                        .iter()
                        .position(|cell| cell.character == needle)
                        .map(|col| (row, col))
                })
                .expect("visible endpoint")
        };

        // Block (column) mode: only the endpoints would be re-anchored while
        // the rows between them are re-planned, so the rectangle could cover
        // characters that were never dragged over. Dropping is the safe answer.
        let (mut terminal, hidden_id) = build();
        let mut policy = super::ProjectionPolicy::new();
        assert!(policy.collapse(hidden_id));
        let mut state = super::ProjectionViewState::new();
        let viewport = terminal.get_projected_viewport_with_state(true, &policy, &mut state);
        let (start, end) = (locate(&viewport, 'M'), locate(&viewport, 'W'));
        terminal.start_selection_in_projection(&viewport, start, super::SelectionMode::Block);
        terminal.update_selection_in_projection(&viewport, end);
        assert!(terminal.has_text_selection());
        terminal.process_input(b"tick\r\n");
        let _ = terminal.get_projected_viewport_with_state(true, &policy, &mut state);
        assert!(
            !terminal.has_text_selection(),
            "a column selection must not be carried across a re-plan"
        );

        // Un-hiding a block would put rows the user never dragged over between
        // the endpoints.
        let (mut terminal, hidden_id) = build();
        let mut policy = super::ProjectionPolicy::new();
        assert!(policy.collapse(hidden_id));
        let mut state = super::ProjectionViewState::new();
        let viewport = terminal.get_projected_viewport_with_state(true, &policy, &mut state);
        let (start, end) = (locate(&viewport, 'M'), locate(&viewport, 'W'));
        terminal.start_selection_in_projection(&viewport, start, super::SelectionMode::Normal);
        terminal.update_selection_in_projection(&viewport, end);
        assert!(terminal.has_text_selection());
        assert!(policy.expand(hidden_id));
        let _ = terminal.get_projected_viewport_with_state(true, &policy, &mut state);
        assert!(
            !terminal.has_text_selection(),
            "changing what is hidden must drop the selection"
        );
    }

    #[test]
    fn projected_selection_copy_skips_hidden_output_and_summary_rows() {
        let mut terminal = TerminalState::new(24, 12);
        for (command, output) in [("one", "LEFT"), ("two", "SECRET"), ("three", "RIGHT")] {
            terminal.process_input(
                format!(
                    "\x1b]133;A\x07$ \x1b]133;B\x07{command}\r\n\x1b]133;C\x07{output}\r\n\x1b]133;D;0\x07"
                )
                .as_bytes(),
            );
        }
        let hidden_id = terminal.command_zones[1].id;
        let mut policy = super::ProjectionPolicy::new();
        assert!(policy.collapse(hidden_id));
        let mut state = super::ProjectionViewState::new();
        let viewport = terminal.get_projected_viewport_with_state(true, &policy, &mut state);
        let locate = |needle: char| {
            viewport
                .cells()
                .iter()
                .enumerate()
                .find_map(|(row, cells)| {
                    cells
                        .iter()
                        .position(|cell| cell.character == needle)
                        .map(|col| (row, col))
                })
                .expect("visible endpoint")
        };
        let locate_last = |needle: char| {
            viewport
                .cells()
                .iter()
                .enumerate()
                .rev()
                .find_map(|(row, cells)| {
                    cells
                        .iter()
                        .rposition(|cell| cell.character == needle)
                        .map(|col| (row, col))
                })
                .expect("visible endpoint")
        };
        let start = locate('L');
        let end = locate_last('T');

        terminal.start_selection_in_projection(&viewport, start, super::SelectionMode::Normal);
        terminal.update_selection_in_projection(&viewport, end);
        let copied = terminal.copy_selection().expect("projected selection");

        assert!(copied.contains("LEFT"));
        assert!(copied.contains("RIGHT"));
        assert!(!copied.contains("SECRET"));
        assert!(copied.contains('\n'));
        assert_eq!(copied.lines().count(), 4);
        assert!(copied.lines().all(|line| !line.trim().is_empty()));
        assert!(terminal.has_text_selection());

        terminal.on_resize(24, 10);
        assert_eq!(
            terminal.copy_selection(),
            None,
            "a structural source change invalidates the old projected selection"
        );

        assert!(policy.expand(hidden_id));
        let _identity = terminal.get_projected_viewport_with_state(true, &policy, &mut state);
        assert!(!terminal.has_text_selection());
        assert_eq!(terminal.copy_selection(), None);
    }

    #[test]
    fn projection_returns_keep_selection_coordinate_spaces_mutually_exclusive() {
        let mut terminal = TerminalState::new(24, 12);
        for (command, output) in [("one", "SECRET"), ("two", "VISIBLE")] {
            terminal.process_input(
                format!(
                    "\x1b]133;A\x07$ \x1b]133;B\x07{command}\r\n\x1b]133;C\x07{output}\r\n\x1b]133;D;0\x07"
                )
                .as_bytes(),
            );
        }
        let hidden_id = terminal.command_zones[0].id;
        let second_id = terminal.command_zones[1].id;
        let hidden_range = terminal
            .finished_output_range(hidden_id)
            .expect("hidden output range");
        let hidden_absolute = terminal.command_zones[0]
            .output_start
            .expect("hidden absolute row");
        let mut policy = super::ProjectionPolicy::new();
        assert!(policy.collapse(hidden_id));
        let mut state = super::ProjectionViewState::new();
        let transformed = terminal.get_projected_viewport_with_state(true, &policy, &mut state);
        let (visible_row, visible_col) = transformed
            .cells()
            .iter()
            .enumerate()
            .find_map(|(row, cells)| {
                cells
                    .iter()
                    .position(|cell| cell.character == 'V')
                    .map(|col| (row, col))
            })
            .expect("visible projected word");
        terminal.select_word_in_projection(&transformed, visible_row, visible_col);
        assert_eq!(terminal.copy_selection().as_deref(), Some("VISIBLE"));

        let same_plan =
            terminal.get_projected_viewport_with_policy(true, &policy, transformed.scroll_offset());
        assert_eq!(same_plan.plan_revision, transformed.plan_revision);
        assert_eq!(terminal.copy_selection().as_deref(), Some("VISIBLE"));

        let bypass = terminal.get_projected_viewport_with_state(false, &policy, &mut state);
        assert_eq!(bypass.mode(), super::ProjectionMode::Bypass);
        assert!(terminal.projected_selection.is_none());
        terminal.select_text(
            (hidden_absolute, hidden_range.start.col),
            (hidden_absolute, hidden_range.end.col.saturating_sub(1)),
        );
        assert!(terminal
            .copy_selection()
            .is_some_and(|text| text.contains("SECRET")));
        let _bypass_again = terminal.get_projected_viewport(false);
        assert!(
            terminal.selection.is_some(),
            "bypass preserves raw selection"
        );
        assert!(terminal.projected_selection.is_none());

        let transformed_again =
            terminal.get_projected_viewport_with_state(true, &policy, &mut state);
        assert_eq!(transformed_again.plan_revision, transformed.plan_revision);
        assert!(
            terminal.selection.is_none(),
            "transformed clears raw selection"
        );
        assert!(terminal.projected_selection.is_none());
        assert_eq!(terminal.copy_selection(), None);

        let (visible_row, visible_col) = transformed_again
            .cells()
            .iter()
            .enumerate()
            .find_map(|(row, cells)| {
                cells
                    .iter()
                    .position(|cell| cell.character == 'V')
                    .map(|col| (row, col))
            })
            .expect("visible projected word after return");
        terminal.select_word_in_projection(&transformed_again, visible_row, visible_col);
        assert!(terminal.projected_selection.is_some());
        assert!(policy.collapse(second_id));
        let changed = terminal.get_projected_viewport_with_policy(true, &policy, 0);
        assert_eq!(changed.mode(), super::ProjectionMode::Transformed);
        assert!(
            terminal.projected_selection.is_none(),
            "new plan clears projected selection"
        );
        assert!(terminal.selection.is_none());
    }

    #[test]
    fn projected_empty_raw_row_selects_but_padding_and_summary_do_not() {
        let mut terminal = TerminalState::new(12, 6);
        terminal.process_input(
            b"\x1b]133;A\x07$ \x1b]133;B\x07one\r\n\x1b]133;C\x07\r\nout\r\n\x1b]133;D;0\x07",
        );
        terminal.process_input(
            b"\x1b]133;A\x07$ \x1b]133;B\x07two\r\n\x1b]133;C\x07hide\r\n\x1b]133;D;0\x07",
        );
        let first_range = terminal
            .finished_output_range(terminal.command_zones[0].id)
            .expect("blank output start");
        let hidden_id = terminal.command_zones[1].id;
        let mut policy = super::ProjectionPolicy::new();
        assert!(policy.collapse(hidden_id));
        let mut state = super::ProjectionViewState::new();
        let viewport = terminal.get_projected_viewport_with_state(true, &policy, &mut state);
        let blank_row = viewport
            .raw_row_view_bounds(first_range.start.row)
            .expect("empty raw row provenance")
            .0;
        terminal.start_selection_in_projection(
            &viewport,
            (blank_row, 0),
            super::SelectionMode::Normal,
        );
        terminal.update_selection_in_projection(&viewport, (blank_row, 5));
        assert!(terminal.has_text_selection());
        assert_eq!(terminal.copy_selection().as_deref(), Some("      "));

        let summary = viewport
            .row_kinds()
            .iter()
            .position(|kind| matches!(kind, super::ProjectedRowKind::CollapsedSummary { .. }))
            .expect("summary visible");
        terminal.start_selection_in_projection(
            &viewport,
            (summary, 0),
            super::SelectionMode::Normal,
        );
        assert!(!terminal.has_text_selection());
    }

    #[test]
    fn projected_wide_selection_highlights_body_and_continuation() {
        let mut terminal = TerminalState::new(12, 6);
        terminal.process_input(
            "\x1b]133;A\x07$ \x1b]133;B\x07one\r\n\x1b]133;C\x07中\r\n\x1b]133;D;0\x07".as_bytes(),
        );
        terminal.process_input(
            b"\x1b]133;A\x07$ \x1b]133;B\x07two\r\n\x1b]133;C\x07hide\r\n\x1b]133;D;0\x07",
        );
        let hidden_id = terminal.command_zones[1].id;
        let mut policy = super::ProjectionPolicy::new();
        assert!(policy.collapse(hidden_id));
        let mut state = super::ProjectionViewState::new();
        let viewport = terminal.get_projected_viewport_with_state(true, &policy, &mut state);
        let (row, body) = viewport
            .cells()
            .iter()
            .enumerate()
            .find_map(|(row, cells)| {
                cells
                    .iter()
                    .position(|cell| cell.character == '中')
                    .map(|col| (row, col))
            })
            .expect("wide glyph visible");
        assert!(viewport.cells()[row][body + 1].flags.wide_continuation());

        terminal.start_selection_in_projection(
            &viewport,
            (row, body + 1),
            super::SelectionMode::Normal,
        );

        assert_eq!(terminal.copy_selection().as_deref(), Some("中"));
        assert_eq!(
            terminal.row_selection_cols_in_projection(&viewport, row),
            Some((body, body + 1))
        );
    }

    #[test]
    fn reveal_collapsed_summary_moves_projected_state_without_expanding_policy() {
        let mut terminal = TerminalState::new(20, 4);
        for index in 0..7 {
            terminal.process_input(
                format!(
                    "\x1b]133;A\x07$ \x1b]133;B\x07c{index}\r\n\x1b]133;C\x07out{index}\r\n\x1b]133;D;0\x07"
                )
                .as_bytes(),
            );
        }
        let zone_id = terminal.command_zones[2].id;
        let mut policy = super::ProjectionPolicy::new();
        assert!(policy.collapse(zone_id));
        let revision = policy.revision();
        let mut state = super::ProjectionViewState::new();

        assert!(terminal.reveal_collapsed_summary(&policy, &mut state, zone_id));
        assert!(policy.is_collapsed(zone_id));
        assert_eq!(policy.revision(), revision, "reveal never expands policy");
        assert!(!state.follow_bottom);
        assert!(state.last_plan_key.is_some());
        assert!(matches!(
            state.top_anchor,
            Some(super::ProjectedTopAnchor::Summary { zone_id: id, .. }) if id == zone_id
        ));

        let revealed = terminal.get_projected_viewport_with_state(true, &policy, &mut state);
        assert!(revealed.row_kinds().iter().any(|kind| matches!(
            kind,
            super::ProjectedRowKind::CollapsedSummary { key, .. } if key.zone_id == zone_id
        )));
        assert!(policy.is_collapsed(zone_id));

        let parked = state.offset_from_bottom();
        assert!(!terminal.reveal_collapsed_summary(&policy, &mut state, u64::MAX));
        assert_eq!(state.offset_from_bottom(), parked);
    }

    #[test]
    fn hidden_raw_match_requires_explicit_expand_before_reveal() {
        let mut terminal = TerminalState::new(20, 3);
        for (command, output) in [("one", "hidden-one"), ("two", "hidden-two")] {
            terminal.process_input(
                format!(
                    "\x1b]133;A\x07$ \x1b]133;B\x07{command}\r\n\x1b]133;C\x07{output}\r\n\x1b]133;D;0\x07"
                )
                .as_bytes(),
            );
        }
        let first_id = terminal.command_zones[0].id;
        let second_id = terminal.command_zones[1].id;
        let first_range = terminal
            .finished_output_range(first_id)
            .expect("first exact output");
        let origin = super::RawCellOrigin {
            row: first_range.start.row,
            col: first_range.start.col,
        };
        let mut policy = super::ProjectionPolicy::new();
        assert!(policy.collapse(first_id));
        assert!(policy.collapse(second_id));
        let mut state = super::ProjectionViewState::new();
        let collapsed = terminal.get_projected_viewport_with_state(true, &policy, &mut state);
        assert_eq!(
            terminal.locate_raw_cell_in_projection(&collapsed, origin),
            super::ProjectedRawCellLocation::Hidden { zone_id: first_id }
        );
        assert!(!terminal.reveal_raw_cell_in_projection(&policy, &mut state, origin));

        assert!(policy.expand(first_id));
        assert!(terminal.reveal_raw_cell_in_projection(&policy, &mut state, origin));
        let revealed = terminal.get_projected_viewport_with_state(true, &policy, &mut state);
        assert!(matches!(
            terminal.locate_raw_cell_in_projection(&revealed, origin),
            super::ProjectedRawCellLocation::Visible(_)
        ));
        assert!(revealed.effective_collapsed().contains(&second_id));
    }

    #[test]
    fn raw_row_identity_follows_grid_rows_into_scrollback_and_survives_trim() {
        let mut terminal = TerminalState::new(4, 2);
        terminal.grid[0][0].character = 'A';
        terminal.grid[1][0].character = 'B';
        let first = terminal.grid.raw_row_id(0).expect("first row id");
        let second = terminal.grid.raw_row_id(1).expect("second row id");
        terminal.cursor_row = 1;

        terminal.process_batch(b"\n");
        assert_eq!(terminal.scrollback[0].raw_row_id(), first);
        assert_eq!(terminal.grid.raw_row_id(0), Some(second));
        let fresh_bottom = terminal.grid.raw_row_id(1).expect("fresh blank row");
        assert_ne!(fresh_bottom, first);
        assert_ne!(fresh_bottom, second);

        terminal.grid[1][0].character = 'C';
        terminal.process_batch(b"\n");
        assert_eq!(terminal.scrollback[1].raw_row_id(), second);
        terminal.set_max_scrollback(1);
        assert_eq!(terminal.scrollback.len(), 1);
        assert_eq!(terminal.scrollback[0].raw_row_id(), second);

        terminal.set_scroll_offset(1);
        let projection = terminal.get_projected_viewport(true);
        assert_eq!(
            projection.raw_to_view(super::RawCellOrigin { row: first, col: 0 }),
            None,
            "trimmed identities must fail closed"
        );
        assert!(projection
            .raw_to_view(super::RawCellOrigin {
                row: second,
                col: 0,
            })
            .is_some());
    }

    #[test]
    fn inserted_deleted_and_reverse_indexed_rows_move_identity_with_content() {
        let mut inserted = TerminalState::new(3, 4);
        let before: Vec<_> = (0..4)
            .map(|row| inserted.grid.raw_row_id(row).unwrap())
            .collect();
        inserted.process_batch(b"\x1b[2;1H\x1b[L");
        assert_eq!(inserted.grid.raw_row_id(0), Some(before[0]));
        assert_ne!(inserted.grid.raw_row_id(1), Some(before[1]));
        assert_eq!(inserted.grid.raw_row_id(2), Some(before[1]));
        assert_eq!(inserted.grid.raw_row_id(3), Some(before[2]));

        let mut deleted = TerminalState::new(3, 4);
        let before: Vec<_> = (0..4)
            .map(|row| deleted.grid.raw_row_id(row).unwrap())
            .collect();
        deleted.process_batch(b"\x1b[2;1H\x1b[M");
        assert_eq!(deleted.grid.raw_row_id(0), Some(before[0]));
        assert_eq!(deleted.grid.raw_row_id(1), Some(before[2]));
        assert_eq!(deleted.grid.raw_row_id(2), Some(before[3]));
        assert!(!before.contains(&deleted.grid.raw_row_id(3).unwrap()));

        let mut reverse_indexed = TerminalState::new(3, 4);
        let before: Vec<_> = (0..4)
            .map(|row| reverse_indexed.grid.raw_row_id(row).unwrap())
            .collect();
        reverse_indexed.process_batch(b"\x1b[1;1H\x1bM");
        assert!(!before.contains(&reverse_indexed.grid.raw_row_id(0).unwrap()));
        assert_eq!(reverse_indexed.grid.raw_row_id(1), Some(before[0]));
        assert_eq!(reverse_indexed.grid.raw_row_id(2), Some(before[1]));
        assert_eq!(reverse_indexed.grid.raw_row_id(3), Some(before[2]));
    }

    #[test]
    fn resize_preserves_retained_rows_but_normalization_reidentifies_history() {
        let mut terminal = TerminalState::new(5, 4);
        for row in 0..4 {
            terminal.grid[row][0].character = char::from(b'A' + row as u8);
        }
        let before: Vec<_> = (0..4)
            .map(|row| terminal.grid.raw_row_id(row).expect("row id"))
            .collect();
        terminal.cursor_row = 3;
        terminal.on_resize(5, 2);

        assert_eq!(terminal.scrollback[0].raw_row_id(), before[0]);
        assert_eq!(terminal.scrollback[1].raw_row_id(), before[1]);
        assert_eq!(terminal.grid.raw_row_id(0), Some(before[2]));
        assert_eq!(terminal.grid.raw_row_id(1), Some(before[3]));

        let retained_grid_ids = [before[2], before[3]];
        terminal.on_resize(5, 4);
        assert_eq!(terminal.grid.raw_row_id(0), Some(retained_grid_ids[0]));
        assert_eq!(terminal.grid.raw_row_id(1), Some(retained_grid_ids[1]));
        assert!(!retained_grid_ids.contains(&terminal.grid.raw_row_id(2).unwrap()));
        assert!(!retained_grid_ids.contains(&terminal.grid.raw_row_id(3).unwrap()));

        let old_history_ids: Vec<_> = terminal
            .scrollback
            .iter()
            .map(ScrollbackLine::raw_row_id)
            .collect();
        terminal.start_selection((0, 0));
        assert!(terminal.normalize_scrollback_width());
        assert!(terminal.selection.is_none());
        assert!(terminal
            .scrollback
            .iter()
            .all(|line| !old_history_ids.contains(&line.raw_row_id())));

        terminal.set_scroll_offset(terminal.scrollback.len());
        let projection = terminal.get_projected_viewport(true);
        for old in old_history_ids {
            assert_eq!(
                projection.raw_to_view(super::RawCellOrigin { row: old, col: 0 }),
                None,
                "physically normalized origins must fail closed"
            );
        }
    }

    #[test]
    fn history_projection_preserves_soft_wrap_wide_origins_and_padding() {
        for cols in 1..=8 {
            let mut terminal = TerminalState::new(cols, 3);
            let mut first = vec![TerminalCell::default(); 4];
            first[0].character = 'A';
            first[1].character = '中';
            first[1].flags.set_wide(true);
            first[2].flags.set_wide_continuation(true);
            first[3].character = 'B';
            let mut second = vec![TerminalCell::default(); 4];
            second[0].character = 'C';
            let first_line = ScrollbackLine::compress(&first, true);
            let first_id = first_line.raw_row_id();
            let second_line = ScrollbackLine::compress(&second, false);
            terminal.push_scrollback_compressed(first_line);
            terminal.push_scrollback_compressed(second_line);
            terminal.set_scroll_offset(terminal.scrollback.len());

            let expected = terminal.get_visible_cells();
            let projection = terminal.get_projected_viewport(true);
            assert!(projection.is_identity());
            assert!(!projection.uses_identity_fast_path());
            assert_eq!(projection.cells(), expected.as_ref(), "width {cols}");
            assert_eq!(
                projection.row_wrapped(),
                terminal.get_visible_row_wrapped(),
                "width {cols}"
            );
            for row in 0..projection.cells().len() {
                for col in 0..projection.cells()[row].len() {
                    let view = super::ViewportCell { row, col };
                    if let Some(origin) = projection.view_to_raw(view) {
                        assert_eq!(projection.raw_to_view(origin), Some(view), "width {cols}");
                    }
                }
            }

            if cols >= 3 {
                let body = projection
                    .raw_to_view(super::RawCellOrigin {
                        row: first_id,
                        col: 1,
                    })
                    .expect("wide body visible");
                let continuation = projection
                    .raw_to_view(super::RawCellOrigin {
                        row: first_id,
                        col: 2,
                    })
                    .expect("wide continuation visible");
                assert_eq!(body.row, continuation.row, "width {cols}");
                assert_eq!(body.col + 1, continuation.col, "width {cols}");
                assert_eq!(
                    projection.view_to_raw(continuation),
                    Some(super::RawCellOrigin {
                        row: first_id,
                        col: 2,
                    }),
                    "continuation keeps its own raw column at width {cols}"
                );
            }

            if cols == 3 {
                assert_eq!(
                    projection.view_to_raw(super::ViewportCell { row: 1, col: 2 }),
                    None,
                    "reflow padding is structural"
                );
            }
        }
    }

    #[test]
    fn projected_row_bounds_cover_multi_source_rows_and_empty_rows() {
        let mut terminal = TerminalState::new(6, 4);
        let mut suffix = vec![TerminalCell::default(); 6];
        suffix[0].character = 'A';
        suffix[1].character = 'a';
        let suffix = ScrollbackLine::compress(&suffix, true);
        let suffix_id = suffix.raw_row_id();
        let mut prefix = vec![TerminalCell::default(); 6];
        prefix[0].character = 'B';
        prefix[1].character = 'b';
        let prefix = ScrollbackLine::compress(&prefix, false);
        let prefix_id = prefix.raw_row_id();
        let empty = ScrollbackLine::compress(&[TerminalCell::default(); 6], false);
        let empty_id = empty.raw_row_id();
        terminal.push_scrollback_compressed(suffix);
        terminal.push_scrollback_compressed(prefix);
        terminal.push_scrollback_compressed(empty);
        terminal.set_scroll_offset(terminal.scrollback.len());

        let projection = terminal.get_projected_viewport(true);
        assert_eq!(projection.raw_row_view_bounds(suffix_id), Some((0, 0)));
        assert_eq!(
            projection.raw_row_view_bounds(prefix_id),
            Some((0, 0)),
            "the later raw-row prefix sharing a display row must be indexed"
        );
        assert_eq!(projection.raw_row_view_bounds(empty_id), Some((1, 1)));
        assert_eq!(projection.view_row_absolute(1), Some(2));
        assert_eq!(
            projection.view_to_raw(super::ViewportCell { row: 1, col: 0 }),
            None,
            "empty-row ownership must not invent a selectable cell origin"
        );
        assert_eq!(
            projection.view_to_raw(super::ViewportCell { row: 0, col: 4 }),
            None,
            "display padding must remain origin-free"
        );
    }

    #[test]
    fn scrolled_projection_decodes_each_source_line_once_and_cache_hits_decode_none() {
        let mut terminal = TerminalState::new(4, 3);
        for ch in ['A', 'B', 'C', 'D'] {
            let mut cells = vec![TerminalCell::default(); 4];
            cells[0].character = ch;
            terminal.push_scrollback_compressed(ScrollbackLine::compress(&cells, false));
        }
        terminal.set_scroll_offset(terminal.scrollback.len());

        super::PROJECTED_HISTORY_DECOMPRESS_COUNT.with(|count| count.set(0));
        let first = terminal.get_projected_viewport(true);
        let miss_decodes = super::PROJECTED_HISTORY_DECOMPRESS_COUNT.with(|count| count.get());
        assert_eq!(miss_decodes, terminal.scrollback.len());

        let cached = terminal.get_projected_viewport(true);
        let hit_decodes = super::PROJECTED_HISTORY_DECOMPRESS_COUNT.with(|count| count.get());
        assert!(std::sync::Arc::ptr_eq(&first, &cached));
        assert_eq!(hit_decodes, miss_decodes);

        let visible = terminal.get_visible_cells();
        let visible_decodes = super::PROJECTED_HISTORY_DECOMPRESS_COUNT.with(|count| count.get());
        assert!(std::sync::Arc::ptr_eq(&visible, &first.cells));
        assert_eq!(visible_decodes, miss_decodes);
    }

    #[test]
    fn projection_revisions_track_view_source_resize_and_alt_bypass() {
        let mut terminal = TerminalState::new(4, 2);
        terminal.grid[0][0].character = 'A';
        terminal.cursor_row = 1;
        terminal.process_batch(b"\n");

        let live = terminal.get_projected_viewport(true);
        assert_eq!(live.mode(), super::ProjectionMode::Identity);
        let bypass = terminal.get_projected_viewport(false);
        assert_eq!(bypass.mode(), super::ProjectionMode::Bypass);
        assert_ne!(live.view_revision(), bypass.view_revision());
        assert_ne!(live.key(), bypass.key());
        assert_eq!(live.cells(), bypass.cells());
        let identity_again = terminal.get_projected_viewport(true);
        assert_ne!(bypass.view_revision(), identity_again.view_revision());
        assert_eq!(live.key(), identity_again.key());
        terminal.set_scroll_offset(1);
        let scrolled = terminal.get_projected_viewport(true);
        assert_ne!(live.view_revision(), scrolled.view_revision());
        assert_eq!(live.key().source, scrolled.key().source);

        terminal.on_resize(6, 2);
        let resized = terminal.get_projected_viewport(true);
        assert_ne!(scrolled.view_revision(), resized.view_revision());
        assert_ne!(
            scrolled.key().source.row_identity,
            resized.key().source.row_identity
        );

        terminal.process_batch(b"\x1b[?1049h");
        let alternate = terminal.get_projected_viewport(true);
        assert!(alternate.is_identity());
        assert_eq!(alternate.mode(), super::ProjectionMode::Bypass);
        assert!(alternate.key().source.alternate_screen);
        assert_eq!(alternate.cells(), terminal.get_visible_cells().as_ref());
        assert_ne!(resized.view_revision(), alternate.view_revision());

        terminal.process_batch(b"\x1b[?1049l");
        let primary = terminal.get_projected_viewport(true);
        assert!(!primary.key().source.alternate_screen);
        assert!(primary.is_identity());
        assert_ne!(alternate.view_revision(), primary.view_revision());
    }

    #[test]
    fn selection_snapshot_passthrough_preserves_raw_storage_and_copy_bytes() {
        let mut legacy = TerminalState::new(12, 3);
        let mut projected = TerminalState::new(12, 3);
        legacy.process_batch(b"hello world");
        projected.process_batch(b"hello world");

        legacy.start_selection((0, 1));
        legacy.update_selection((0, 4));
        let snapshot = projected.get_projected_viewport(true);
        projected.start_selection_in_projection(&snapshot, (0, 1), super::SelectionMode::Normal);
        projected.update_selection_in_projection(&snapshot, (0, 4));
        assert_eq!(projected.selection, legacy.selection);
        assert_eq!(projected.copy_selection(), legacy.copy_selection());
        assert_eq!(
            projected.row_selection_cols_in_projection(&snapshot, 0),
            legacy.row_selection_cols(0)
        );

        legacy.select_word_at(0, 7);
        projected.select_word_in_projection(&snapshot, 0, 7);
        assert_eq!(projected.selection, legacy.selection);
        assert_eq!(projected.copy_selection(), legacy.copy_selection());

        legacy.extend_line_selection_to(2);
        projected.extend_line_selection_in_projection(&snapshot, 2);
        assert_eq!(projected.selection, legacy.selection);
        assert_eq!(projected.copy_selection(), legacy.copy_selection());
    }

    #[test]
    fn maximum_live_projection_origin_storage_scales_with_rows_not_cells() {
        let mut terminal = TerminalState::new(super::MAX_TERMINAL_COLS, super::MAX_TERMINAL_ROWS);
        let projection = terminal.get_projected_viewport(true);

        assert_eq!(projection.cells().len(), super::MAX_TERMINAL_ROWS);
        assert!(projection
            .cells()
            .iter()
            .all(|row| row.len() == super::MAX_TERMINAL_COLS));
        assert_eq!(projection.origin_span_count(), super::MAX_TERMINAL_ROWS);
        assert_eq!(projection.raw_span_index.len(), super::MAX_TERMINAL_ROWS);
        assert!(
            projection.origin_span_count() * 8
                < super::MAX_TERMINAL_COLS * super::MAX_TERMINAL_ROWS,
            "origin metadata must remain row/span proportional"
        );
    }

    #[test]
    fn resize_preserves_full_screen_scroll_region() {
        let mut terminal = TerminalState::new(4, 3);

        terminal.on_resize(4, 6);

        assert_eq!(terminal.scroll_region_top, 0);
        assert_eq!(terminal.scroll_region_bottom, 5);
    }

    #[test]
    fn decstbm_zero_bottom_defaults_to_full_screen() {
        let mut terminal = TerminalState::new(4, 4);

        terminal.process_input(b"\x1b[1;0r");

        assert_eq!(terminal.scroll_region_top, 0);
        assert_eq!(terminal.scroll_region_bottom, 3);
    }

    #[test]
    fn codex_resume_style_output_populates_scrollback() {
        let mut terminal = TerminalState::new(8, 3);

        terminal.process_input(b"\x1b[?2026h\x1b[1;0r\x1b[1;1H");
        terminal.process_input(b"line-1\r\nline-2\r\nline-3\r\nline-4\r\nline-5\r\n");
        terminal.process_input(b"\x1b[?2026l");

        assert!(
            terminal.scrollback_len() >= 3,
            "expected resumed TUI output to enter scrollback"
        );

        terminal.scroll(2);
        let visible = terminal.get_visible_cells();
        let text: String = visible[0]
            .iter()
            .map(|cell| cell.character)
            .collect::<String>()
            .trim_end()
            .to_string();

        assert!(
            text.starts_with("line-"),
            "expected scrollback viewport to show historical output, got {text:?}"
        );
    }

    #[test]
    fn codex_synchronized_main_screen_history_reaches_above_last_page() {
        let mut terminal = TerminalState::new(16, 5);

        terminal.process_input(b"\x1b[?2026h\x1b[1;0r\x1b[1;1H");
        for i in 0..14 {
            terminal.process_input(format!("\x1b[;m\x1b[Kitem-{i:02}\r\n").as_bytes());
        }
        terminal.process_input(b"\x1b[r\x1b[1;1H\x1b[?2026l");

        assert!(
            terminal.scrollback_len() > terminal.grid.rows(),
            "expected main-screen synchronized output to retain more than one page"
        );

        terminal.set_scroll_offset(terminal.scrollback_len());
        let visible = terminal.get_visible_cells();
        let text = visible
            .iter()
            .flat_map(|row| {
                row.iter()
                    .map(|cell| cell.character)
                    .chain(std::iter::once('\n'))
            })
            .collect::<String>();

        assert!(
            text.contains("item-00"),
            "expected earliest synchronized output to remain reachable, got {text:?}"
        );
    }

    #[test]
    fn erase_saved_lines_at_the_bottom_still_clears_scrollback() {
        let mut terminal = TerminalState::new(8, 3);
        for index in 0..10 {
            terminal.process_input(format!("line-{index}\r\n").as_bytes());
        }
        assert!(terminal.scrollback_len() > 0);

        terminal.process_input(b"\x1b[3J");

        assert_eq!(
            terminal.scrollback_len(),
            0,
            "a purge requested at the live bottom is honored immediately"
        );
        assert_eq!(terminal.scroll_offset, 0);
    }

    #[test]
    fn erase_saved_lines_does_not_yank_a_reader_off_their_history() {
        let mut terminal = TerminalState::new(8, 3);
        for index in 0..10 {
            terminal.process_input(format!("line-{index}\r\n").as_bytes());
        }
        let history = terminal.scrollback_len();
        terminal.scroll(4);
        let reading_at = terminal.scroll_offset;
        assert!(reading_at > 0, "fixture must leave the viewport in history");

        // codex-cli re-renders its transcript with exactly this pair.
        terminal.process_input(b"\x1b[H\x1b[3J");

        assert_eq!(
            terminal.scrollback_len(),
            history,
            "history a user is reading must survive the app's purge"
        );
        assert_eq!(
            terminal.scroll_offset, reading_at,
            "the viewport must not be snapped to the live bottom mid-read"
        );
    }

    #[test]
    fn deferred_purge_retires_exactly_the_pre_purge_prefix() {
        let mut terminal = TerminalState::new(8, 3);
        for index in 0..10 {
            terminal.process_input(format!("old-{index}\r\n").as_bytes());
        }
        terminal.scroll(4);
        let erased = terminal.scrollback_len();
        assert!(erased > 0, "fixture must have saved lines to erase");

        terminal.process_input(b"\x1b[3J");
        assert_eq!(terminal.scrollback_len(), erased, "the purge is deferred");

        // Rows the app repaints after the purge are new history, not erased.
        for index in 0..6 {
            terminal.process_input(format!("new-{index}\r\n").as_bytes());
        }
        let before_settling = terminal.scrollback_len();

        terminal.scroll_to_bottom();

        assert_eq!(
            terminal.scrollback_len(),
            before_settling - erased,
            "settling retires the erased prefix, and only that prefix"
        );
        assert_eq!(terminal.scroll_offset, 0);
        let survivors: Vec<String> = terminal
            .scrollback
            .iter()
            .map(|line| {
                line.decompress()
                    .iter()
                    .map(|cell| cell.character)
                    .collect::<String>()
                    .trim_end()
                    .to_string()
            })
            .collect();
        assert!(
            !survivors.contains(&"old-0".to_string()),
            "the oldest erased row must be gone once the reader returns, got {survivors:?}"
        );
    }

    #[test]
    fn deferred_purge_never_outlives_the_rows_the_cap_already_trimmed() {
        let mut terminal = TerminalState::new(8, 3);
        terminal.set_max_scrollback(6);
        for index in 0..10 {
            terminal.process_input(format!("old-{index}\r\n").as_bytes());
        }
        terminal.scroll(3);
        terminal.process_input(b"\x1b[3J");

        // The cap evicts the deferred rows itself while the reader stays put.
        for index in 0..6 {
            terminal.process_input(format!("new-{index}\r\n").as_bytes());
        }
        assert_eq!(
            terminal.pending_saved_line_purge, 0,
            "rows the cap already dropped must stop counting toward the purge"
        );
        let before_settling = terminal.scrollback_len();

        terminal.scroll_to_bottom();

        assert_eq!(
            terminal.scrollback_len(),
            before_settling,
            "a purge whose rows the cap already evicted must not take live rows too"
        );
    }

    #[test]
    fn synchronized_primary_screen_redraws_do_not_fill_scrollback() {
        let mut terminal = TerminalState::new(24, 4);

        for seconds in 1..=3 {
            terminal.process_input(b"\x1b[?2026h\x1b[1;1H\x1b[2J");
            terminal.process_input(b">_ OpenAI Codex\r\n");
            terminal.process_input(format!("Booting MCP server ({seconds}s)").as_bytes());
            terminal.process_input(b"\x1b[?2026l");
        }

        assert_eq!(
            terminal.scrollback_len(),
            0,
            "primary-screen synchronized redraws should not be recorded as history"
        );
    }

    #[test]
    fn top_margin_scroll_region_pushes_scrolled_lines_to_scrollback() {
        let mut terminal = TerminalState::new(24, 6);

        terminal.process_input(b"\x1b[1;4r\x1b[1;1H");
        terminal.process_input(b"hist-1\r\nhist-2\r\nhist-3\r\nhist-4\r\nhist-5\r\n");
        terminal.process_input(b"\x1b[r\x1b[5;1Hprompt\r\nstatus");

        let history: Vec<String> = terminal
            .scrollback
            .iter()
            .map(|line| {
                line.decompress()
                    .iter()
                    .map(|cell| cell.character)
                    .collect::<String>()
                    .trim_end()
                    .to_string()
            })
            .collect();

        assert_eq!(
            history,
            ["hist-1", "hist-2"],
            "expected lines scrolled off a top-anchored region to remain scrollable"
        );

        assert_eq!(terminal.grid[4][0].character, 'p');
        assert_eq!(terminal.grid[5][0].character, 's');
    }

    #[test]
    fn synchronized_primary_screen_entry_preserves_existing_history() {
        let mut terminal = TerminalState::new(24, 4);

        terminal.process_input(b"previous log\r\nshell prompt");
        terminal.process_input(b"\x1b[?2026h\x1b[1;1H\x1b[2J");
        terminal.process_input(b">_ OpenAI Codex\r\nBooting MCP server");
        terminal.process_input(b"\x1b[?2026l");
        terminal.process_input(b"\x1b[?2026h\x1b[1;1H\x1b[2J");
        terminal.process_input(b">_ OpenAI Codex\r\nBooting MCP server");
        terminal.process_input(b"\x1b[?2026l");

        assert_eq!(terminal.scrollback_len(), 2);
        let history: Vec<String> = terminal
            .scrollback
            .iter()
            .map(|line| {
                line.decompress()
                    .iter()
                    .map(|cell| cell.character)
                    .collect::<String>()
                    .trim_end()
                    .to_string()
            })
            .collect();

        assert_eq!(history, ["previous log", "shell prompt"]);
    }

    #[test]
    fn synchronized_alt_screen_snapshots_can_be_scrolled() {
        let mut terminal = TerminalState::new(12, 3);

        terminal.process_input(b"\x1b[?1049h");
        assert!(terminal.is_alt_buffer_active());

        terminal.process_input(b"\x1b[?2026h\x1b[1;1H");
        terminal.process_input(b"first page\r\nalpha\r\nomega");
        terminal.process_input(b"\x1b[?2026l");
        terminal.process_input(b"\x1b[?2026h\x1b[1;1H");
        terminal.process_input(b"second page\r\nbeta\r\ndone ");
        terminal.process_input(b"\x1b[?2026l");

        // Each frame supersedes the previous one: the alternate screen holds a
        // scrollable copy of what it shows, not one copy per repaint.
        assert_eq!(
            terminal.scrollback_len(),
            3,
            "expected exactly one synchronized alt-screen snapshot in scrollback"
        );

        terminal.scroll(3);
        assert!(terminal.scroll_offset > 0);
        let visible = terminal.get_visible_cells();
        let text = visible
            .iter()
            .flat_map(|row| {
                row.iter()
                    .map(|cell| cell.character)
                    .chain(std::iter::once('\n'))
            })
            .collect::<String>();

        assert!(
            text.contains("second page"),
            "expected the newest archived synchronized screen, got {text:?}"
        );
    }

    #[test]
    fn synchronized_alt_screen_redraw_rebases_live_selection() {
        let mut terminal = TerminalState::new(12, 3);
        terminal.process_input(b"\x1b[?1049h");
        terminal.process_input(b"\x1b[?2026h\x1b[1;1Hfirst\r\nsecond\x1b[?2026l");

        terminal.start_selection((0, 0));
        terminal.update_selection((0, 4));
        let old_base = terminal.scrollback_len();
        assert_eq!(terminal.selection.unwrap().anchor.0, old_base);

        terminal.process_input(b"\x1b[?2026h\x1b[1;1Hfresh\r\nsecond\r\nthird\x1b[?2026l");

        let new_base = terminal.scrollback_len();
        assert_ne!(new_base, old_base);
        assert_eq!(terminal.selection.unwrap().anchor.0, new_base);
        assert_eq!(terminal.row_selection_cols(0), Some((0, 4)));
        assert_eq!(terminal.copy_selection().as_deref(), Some("fresh"));
    }

    #[test]
    fn animated_alt_screen_frames_do_not_grow_scrollback() {
        let mut terminal = TerminalState::new(16, 3);
        terminal.process_input(b"\x1b[?1049h");

        for frame in 0..200 {
            terminal.process_input(b"\x1b[?2026h\x1b[1;1H");
            terminal.process_input(
                format!("working {frame:03}\r\nrow-a {frame:03}\r\nrow-b {frame:03}").as_bytes(),
            );
            terminal.process_input(b"\x1b[?2026l");
        }

        assert_eq!(
            terminal.scrollback_len(),
            3,
            "an animated TUI must not append one screen per repaint"
        );
    }

    #[test]
    fn alt_screen_frames_that_clear_every_repaint_do_not_grow_scrollback() {
        let mut terminal = TerminalState::new(16, 3);
        terminal.process_input(b"\x1b[?1049h");

        for frame in 0..200 {
            terminal.process_input(b"\x1b[?2026h\x1b[H\x1b[2J\x1b[1;1H");
            terminal.process_input(
                format!("working {frame:03}\r\nrow-a {frame:03}\r\nrow-b {frame:03}").as_bytes(),
            );
            terminal.process_input(b"\x1b[?2026l");
        }

        assert_eq!(
            terminal.scrollback_len(),
            3,
            "an erase inside the alternate screen is a repaint, not new history"
        );
    }

    #[test]
    fn alt_screen_frames_leave_a_scrolled_back_reader_untouched() {
        let mut terminal = TerminalState::new(16, 3);
        for index in 0..12 {
            terminal.process_input(format!("hist-{index}\r\n").as_bytes());
        }
        terminal.process_input(b"\x1b[?1049h");
        terminal.process_input(b"\x1b[?2026h\x1b[1;1Hframe zero\x1b[?2026l");

        terminal.scroll(4);
        let reading_at = terminal.scroll_offset;
        let history = terminal.scrollback_len();
        assert!(reading_at > 0, "fixture must leave the viewport in history");

        for frame in 1..50 {
            terminal.process_input(b"\x1b[?2026h\x1b[1;1H");
            terminal.process_input(format!("frame {frame:03}").as_bytes());
            terminal.process_input(b"\x1b[?2026l");
        }

        assert_eq!(
            terminal.scrollback_len(),
            history,
            "frames still on screen must not be appended under a reader"
        );
        assert_eq!(
            terminal.scroll_offset, reading_at,
            "the reader's viewport must not move"
        );
    }

    #[test]
    fn reading_history_and_returning_leaves_one_alt_screen_snapshot() {
        let mut terminal = TerminalState::new(16, 3);
        for index in 0..12 {
            terminal.process_input(format!("hist-{index}\r\n").as_bytes());
        }
        terminal.process_input(b"\x1b[?1049h");
        terminal.process_input(b"\x1b[?2026h\x1b[1;1Hframe zero\x1b[?2026l");
        let settled = terminal.scrollback_len();

        // Scroll away, let the app paint, then come back — repeatedly.
        for cycle in 0..5 {
            terminal.scroll(4);
            terminal.process_input(b"\x1b[?2026h\x1b[1;1H");
            terminal.process_input(format!("away {cycle}").as_bytes());
            terminal.process_input(b"\x1b[?2026l");
            terminal.scroll_to_bottom();
            terminal.process_input(b"\x1b[?2026h\x1b[1;1H");
            terminal.process_input(format!("back {cycle}").as_bytes());
            terminal.process_input(b"\x1b[?2026l");
        }

        assert_eq!(
            terminal.scrollback_len(),
            settled,
            "each read-and-return must not strand another snapshot in history"
        );
    }

    #[test]
    fn alt_screen_content_that_scrolls_off_the_top_is_still_kept() {
        let mut terminal = TerminalState::new(16, 3);
        terminal.process_input(b"\x1b[?1049h");

        // A transcript-style TUI: each frame pushes a line off the screen top.
        for index in 0..8 {
            terminal.process_input(b"\x1b[?2026h");
            terminal.process_input(format!("scrolled-{index}\r\n").as_bytes());
            terminal.process_input(b"\x1b[?2026l");
        }

        let history: Vec<String> = terminal
            .scrollback
            .iter()
            .map(|line| {
                line.decompress()
                    .iter()
                    .map(|cell| cell.character)
                    .collect::<String>()
                    .trim_end()
                    .to_string()
            })
            .collect();
        assert!(
            history.iter().any(|row| row == "scrolled-0"),
            "rows the app scrolled off the alternate screen stay scrollable, got {history:?}"
        );
    }

    #[test]
    fn utf8_cjk_input_stores_wide_cells() {
        let mut terminal = TerminalState::new(8, 2);

        terminal.process_input("中文".as_bytes());

        assert_eq!(terminal.grid[0][0].character, '中');
        assert!(terminal.grid[0][0].flags.wide());
        assert!(terminal.grid[0][1].flags.wide_continuation());
        assert_eq!(terminal.grid[0][2].character, '文');
        assert!(terminal.grid[0][2].flags.wide());
        assert!(terminal.grid[0][3].flags.wide_continuation());
        assert_eq!(terminal.cursor_col, 4);
    }

    #[test]
    fn malformed_utf8_bytes_emit_replacement_character() {
        let mut terminal = TerminalState::new(20, 2);

        // Invalid lead byte (0xFF) and orphan continuation byte (0x80):
        // one U+FFFD each.
        terminal.process_input(b"\xff\x80");
        assert_eq!(terminal.grid[0][0].character, '\u{FFFD}');
        assert_eq!(terminal.grid[0][1].character, '\u{FFFD}');
        assert_eq!(terminal.cursor_col, 2);
    }

    #[test]
    fn malformed_utf8_sequence_emits_replacement_without_swallowing_text() {
        let mut terminal = TerminalState::new(20, 2);

        // Lead byte directly followed by ASCII: U+FFFD for the lead byte,
        // then the ASCII byte is printed normally.
        terminal.process_input(b"\xc3A");
        assert_eq!(terminal.grid[0][0].character, '\u{FFFD}');
        assert_eq!(terminal.grid[0][1].character, 'A');

        // Full length but invalid content (overlong encoding): a single
        // U+FFFD for the whole sequence.
        terminal.process_input(b"\xe0\x80\x80");
        assert_eq!(terminal.grid[0][2].character, '\u{FFFD}');

        // Encoded surrogate half: likewise a single U+FFFD.
        terminal.process_input(b"\xed\xa0\x80");
        assert_eq!(terminal.grid[0][3].character, '\u{FFFD}');
        assert_eq!(terminal.cursor_col, 4);
    }

    #[test]
    fn truncated_utf8_sequence_across_reads_emits_one_replacement() {
        let mut terminal = TerminalState::new(20, 2);

        // A valid sequence split across reads still decodes.
        terminal.process_input(b"\xe4\xb8");
        terminal.process_input(b"\xad");
        assert_eq!(terminal.grid[0][0].character, '中');

        // A pending sequence abandoned by the next read emits a single
        // U+FFFD (not one per buffered byte), and the buffered bytes can no
        // longer combine with a later continuation byte into a spurious char.
        terminal.process_input(b"\xe4\xb8");
        terminal.process_input(b"X\xa9");
        assert_eq!(terminal.grid[0][2].character, '\u{FFFD}');
        assert_eq!(terminal.grid[0][3].character, 'X');
        assert_eq!(terminal.grid[0][4].character, '\u{FFFD}');
        assert_eq!(terminal.cursor_col, 5);
    }

    #[test]
    fn osc52_set_accepts_payload_at_the_100kib_boundary() {
        use base64::Engine as _;
        let mut terminal = TerminalState::new(8, 2);

        let payload = base64::engine::general_purpose::STANDARD.encode("x".repeat(100 * 1024));
        terminal.process_input(format!("\x1b]52;c;{payload}\x07").as_bytes());
        assert_eq!(
            terminal.take_osc52_clipboard_set().map(|text| text.len()),
            Some(100 * 1024)
        );
    }

    #[test]
    fn osc52_set_rejects_payload_above_the_100kib_cap() {
        use base64::Engine as _;
        let mut terminal = TerminalState::new(8, 2);

        // 100 KiB + 1 passes the encoded-length pre-check (its base64 size
        // matches the boundary payload) but exceeds the decoded cap.
        let payload = base64::engine::general_purpose::STANDARD.encode("x".repeat(100 * 1024 + 1));
        terminal.process_input(format!("\x1b]52;c;{payload}\x07").as_bytes());
        assert_eq!(terminal.take_osc52_clipboard_set(), None);
    }

    #[test]
    fn osc52_set_rejects_oversized_base64_before_decoding() {
        use base64::Engine as _;
        let mut terminal = TerminalState::new(8, 2);

        // Encoded length beyond the pre-check limit: rejected without
        // decoding, leaving no pending clipboard write.
        let payload = base64::engine::general_purpose::STANDARD.encode("x".repeat(200 * 1024));
        terminal.process_input(format!("\x1b]52;c;{payload}\x07").as_bytes());
        assert_eq!(terminal.take_osc52_clipboard_set(), None);

        // Ordinary small writes and the query path are unaffected by the cap.
        let small = base64::engine::general_purpose::STANDARD.encode("hello");
        terminal.process_input(format!("\x1b]52;c;{small}\x07").as_bytes());
        assert_eq!(
            terminal.take_osc52_clipboard_set().as_deref(),
            Some("hello")
        );

        terminal.process_input(b"\x1b]52;c;?\x07");
        assert!(terminal.take_osc52_clipboard_query());
    }

    #[test]
    fn ascii_fast_path_clears_overwritten_wide_cell_partners() {
        let mut terminal = TerminalState::new(8, 2);
        terminal.process_input("中文".as_bytes());

        // Overwrite only the continuation half of the first glyph.
        terminal.process_input(b"\x1b[1;2HA");
        assert_eq!(terminal.grid[0][0].character, ' ');
        assert!(!terminal.grid[0][0].flags.wide());
        assert_eq!(terminal.grid[0][1].character, 'A');
        assert!(!terminal.grid[0][1].flags.wide_continuation());

        // Overwrite only the body half of the second glyph.
        terminal.process_input(b"\x1b[1;3HB");
        assert_eq!(terminal.grid[0][2].character, 'B');
        assert!(!terminal.grid[0][2].flags.wide());
        assert_eq!(terminal.grid[0][3].character, ' ');
        assert!(!terminal.grid[0][3].flags.wide_continuation());
    }

    #[test]
    fn linefeed_at_bottom_pushes_to_scrollback_for_full_screen_region() {
        let mut terminal = TerminalState::new(4, 2);
        terminal.grid[0][0].character = 'A';
        terminal.grid[1][0].character = 'B';
        terminal.cursor_row = 1;
        terminal.cursor_col = 0;

        terminal.process_input(b"\n");

        assert_eq!(terminal.scrollback.len(), 1);
        assert_eq!(terminal.scrollback[0].decompress()[0].character, 'A');
        assert_eq!(terminal.grid[0][0].character, 'B');
        assert_eq!(terminal.grid[1][0].character, ' ');
    }

    #[test]
    fn visible_cells_keep_rectangular_shape_after_resize_with_scrollback() {
        let mut terminal = TerminalState::new(4, 2);
        terminal.grid.get_mut(0, 0).character = 'A';
        terminal.grid.get_mut(1, 0).character = 'B';
        terminal.cursor_row = 1;

        terminal.process_input(b"\n");
        terminal.on_resize(5, 2);
        terminal.scroll(1);

        let visible = terminal.get_visible_cells();

        assert_eq!(visible.len(), 2);
        assert!(visible.iter().all(|row| row.len() == 5));
        assert_eq!(visible[0][0].character, 'A');
        assert_eq!(visible[0][4].character, ' ');
    }

    #[test]
    fn resize_does_not_yank_scrolled_back_reader_to_live_bottom() {
        let mut terminal = TerminalState::new(8, 4);
        for row in 0..10 {
            terminal.process_input(format!("row-{row}\r\n").as_bytes());
        }
        terminal.scroll(3);

        let initial_offset = terminal.scroll_offset;
        let initial_start = terminal.viewport_absolute_start();
        let initial_top = terminal
            .raw_row_id_at_absolute(initial_start)
            .expect("top history row");
        assert!(initial_offset > 0);

        terminal.on_resize(10, 4);
        assert_eq!(terminal.scroll_offset, initial_offset);
        assert_eq!(terminal.viewport_absolute_start(), initial_start);
        assert_eq!(
            terminal.raw_row_id_at_absolute(initial_start),
            Some(initial_top)
        );

        // Shrinking the grid moves two rows into history. Its history push path
        // must increase the offset by the same amount so the visible anchor is
        // still the exact row the reader was looking at.
        terminal.on_resize(10, 2);
        assert_eq!(terminal.scroll_offset, initial_offset + 2);
        assert_eq!(terminal.viewport_absolute_start(), initial_start);
        assert_eq!(
            terminal.raw_row_id_at_absolute(initial_start),
            Some(initial_top)
        );
    }

    #[test]
    fn history_reflow_waits_until_scrolled_back_reader_returns_to_bottom() {
        let mut terminal = TerminalState::new(4, 2);
        for row in 0..6 {
            terminal.process_input(format!("{row}\r\n").as_bytes());
        }
        terminal.on_resize(7, 2);
        terminal.scroll(2);
        terminal.start_selection((0, 0));

        let history_len = terminal.scrollback_len();
        let offset = terminal.scroll_offset;
        let viewport_start = terminal.viewport_absolute_start();
        assert!(!terminal.normalize_scrollback_width());
        assert_eq!(terminal.scrollback_len(), history_len);
        assert_eq!(terminal.scroll_offset, offset);
        assert_eq!(terminal.viewport_absolute_start(), viewport_start);
        assert!(terminal.selection.is_some());
        assert!(terminal
            .scrollback
            .iter()
            .all(|line| line.decompress().len() == 4));

        terminal.scroll_to_bottom();
        assert!(terminal.normalize_scrollback_width());
        assert!(terminal
            .scrollback
            .iter()
            .all(|line| line.decompress().len() == 7));
    }

    #[test]
    fn history_reflow_padding_does_not_inherit_live_background() {
        let mut terminal = TerminalState::new(3, 2);
        terminal.grid[0][0].character = 'A';
        terminal.cursor_row = 1;
        terminal.process_input(b"\n");
        assert_eq!(terminal.scrollback.len(), 1);

        // A live red SGR background should affect newly erased screen cells, but
        // never synthetic padding added while old history is being reflowed.
        terminal.process_input(b"\x1b[41m");
        terminal.on_resize(5, 2);
        terminal.scroll(1);
        let visible = terminal.get_visible_cells();
        assert_eq!(visible[0][0].character, 'A');
        assert_eq!(visible[0][4].background, Color::Default);

        terminal.scroll_to_bottom();
        assert!(terminal.normalize_scrollback_width());
        let history = terminal.scrollback[0].decompress();
        assert_eq!(history.len(), 5);
        assert_eq!(history[4].background, Color::Default);
    }

    #[test]
    fn history_reflow_preserves_blocks_live_prompt_and_agent_correlation() {
        let mut terminal = TerminalState::new(24, 7);
        emit_zone(&mut terminal, 0);
        emit_zone(&mut terminal, 1);
        let ids: Vec<u64> = terminal.command_zones.iter().map(|zone| zone.id).collect();
        let captured_before = terminal.captured_output_bytes;
        assert!(terminal.scrollback_len() > 0);
        assert_eq!(terminal.take_completed_commands().len(), 2);

        terminal.process_input(b"\x1b]133;A\x07$ \x1b]133;B\x07");
        assert_eq!(terminal.agent_prompt_status(), AgentPromptStatus::Ready);
        terminal
            .arm_agent_execution(77, "echo live")
            .expect("trusted empty prompt");
        terminal.process_input(b"echo live\r\n\x1b]133;C;id=run-1\x07");
        let old_scrollback_len = terminal.scrollback_len();
        let old_running_start = terminal.running_zone_start().expect("running block");
        assert!(old_running_start >= old_scrollback_len);

        terminal.on_resize(11, 7);
        assert!(terminal.normalize_scrollback_width());

        assert_eq!(
            terminal
                .command_zones
                .iter()
                .map(|zone| zone.id)
                .collect::<Vec<_>>(),
            ids
        );
        assert_eq!(terminal.captured_output_bytes, captured_before);
        assert_eq!(
            terminal.zone_output_text(ids[0]).as_deref(),
            Some("out\nout\nout")
        );
        assert!(terminal.is_command_running());
        assert_eq!(
            terminal.running_zone_start(),
            Some(
                terminal
                    .scrollback_len()
                    .saturating_add(old_running_start - old_scrollback_len)
            )
        );

        terminal.process_input(b"done\r\n\x1b]133;D;0;id=run-1\x07");
        let completed = terminal.take_completed_commands();
        assert_eq!(completed.len(), 1);
        assert_eq!(completed[0].command, "echo live");
        assert_eq!(completed[0].agent_generation, Some(77));
        assert_eq!(
            terminal
                .command_zones
                .back()
                .and_then(|zone| zone.command.as_deref()),
            Some("echo live")
        );
    }

    #[test]
    fn history_reflow_defers_while_live_block_has_entered_scrollback() {
        let mut terminal = TerminalState::new(20, 4);
        emit_zone(&mut terminal, 0);
        terminal.process_input(
            b"\x1b]133;A\x07$ \x1b]133;B\x07long job\r\n\x1b]133;C\x07one\r\ntwo\r\nthree\r\nfour\r\n",
        );
        let running_start = terminal.running_zone_start().expect("running block");
        let history_len = terminal.scrollback_len();
        assert!(running_start < history_len);
        let old_width = terminal
            .scrollback
            .front()
            .expect("history")
            .decompress()
            .len();

        terminal.on_resize(9, 4);
        assert!(!terminal.normalize_scrollback_width());

        assert_eq!(terminal.scrollback_len(), history_len);
        assert_eq!(terminal.running_zone_start(), Some(running_start));
        assert_eq!(
            terminal
                .scrollback
                .front()
                .expect("history")
                .decompress()
                .len(),
            old_width,
            "the unsafe reflow is deferred until the live lifecycle closes"
        );
        terminal.process_input(b"done\r\n\x1b]133;D;0\x07");
        assert!(!terminal.is_command_running());
        assert_eq!(
            terminal
                .command_zones
                .back()
                .and_then(|zone| zone.command.as_deref()),
            Some("long job")
        );
        assert!(terminal.normalize_scrollback_width());
        assert!(terminal
            .scrollback
            .iter()
            .all(|line| line.decompress().len() == 9));
    }

    #[test]
    fn resize_invalidates_live_visible_cells_cache() {
        let mut terminal = TerminalState::new(4, 2);
        let before = terminal.get_visible_cells();
        assert_eq!(before[0].len(), 4);

        terminal.on_resize(7, 3);
        let after = terminal.get_visible_cells();

        assert_eq!(after.len(), 3);
        assert!(after.iter().all(|row| row.len() == 7));
    }

    #[test]
    fn shrinking_grid_does_not_leave_orphaned_wide_cells() {
        let mut terminal = TerminalState::new(4, 2);
        terminal.process_input("中".as_bytes());

        terminal.on_resize(1, 2);

        assert_eq!(terminal.grid[0][0].character, ' ');
        assert!(!terminal.grid[0][0].flags.wide());
        assert!(!terminal.grid[0][0].flags.wide_continuation());
    }

    #[test]
    fn scrollback_reflow_keeps_wide_cell_pairs_on_one_row() {
        let mut cells = vec![TerminalCell::default(); 4];
        cells[0].character = 'A';
        cells[1].character = '中';
        cells[1].flags.set_wide(true);
        cells[2].flags.set_wide_continuation(true);
        cells[3].character = 'B';
        let source = vec![ScrollbackLine::compress(&cells, false)];

        let reflowed = TerminalState::reflow_lines(&source, 2, &TerminalCell::default());
        let rows: Vec<_> = reflowed.iter().map(ScrollbackLine::decompress).collect();

        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0][0].character, 'A');
        assert_eq!(rows[1][0].character, '中');
        assert!(rows[1][0].flags.wide());
        assert!(rows[1][1].flags.wide_continuation());
        assert_eq!(rows[2][0].character, 'B');
        assert!(rows.iter().all(|row| !row[0].flags.wide_continuation()));
    }

    #[test]
    fn cursor_is_hidden_while_viewing_scrollback() {
        let mut terminal = TerminalState::new(4, 2);
        terminal.grid.get_mut(0, 0).character = 'A';
        terminal.grid.get_mut(1, 0).character = 'B';
        terminal.cursor_row = 1;

        terminal.process_input(b"\n");

        assert!(terminal.is_cursor_visible());

        terminal.scroll(1);

        assert!(!terminal.is_cursor_visible());
    }

    #[test]
    fn scroll_to_bottom_restores_live_cursor_visibility() {
        let mut terminal = TerminalState::new(4, 2);
        terminal.grid.get_mut(0, 0).character = 'A';
        terminal.grid.get_mut(1, 0).character = 'B';
        terminal.cursor_row = 1;

        terminal.process_input(b"\n");
        terminal.scroll(1);
        terminal.scroll_to_bottom();

        assert_eq!(terminal.scroll_offset, 0);
        assert!(terminal.is_cursor_visible());
    }

    #[test]
    fn sgr_39_and_49_restore_default_colors() {
        let mut terminal = TerminalState::new(8, 2);

        terminal.process_input(b"\x1b[36;44mA\x1b[39;49mB");

        let first = &terminal.grid[0][0];
        let second = &terminal.grid[0][1];

        assert_eq!(first.foreground, Color::Cyan);
        assert_eq!(first.background, Color::Blue);
        assert_eq!(second.foreground, Color::Default);
        assert_eq!(second.background, Color::Default);
    }

    #[test]
    fn cleared_cells_keep_active_background() {
        let mut terminal = TerminalState::new(8, 2);

        terminal.process_input(b"\x1b[44mAB\x1b[1;1H\x1b[K");

        assert_eq!(terminal.grid[0][0].background, Color::Blue);
        assert_eq!(terminal.grid[0][1].background, Color::Blue);
    }

    #[test]
    fn empty_sgr_sequence_resets_attributes() {
        let mut terminal = TerminalState::new(8, 2);

        terminal.process_input(b"\x1b[7;36;44mA\x1b[mB");

        let first = &terminal.grid[0][0];
        let second = &terminal.grid[0][1];

        assert!(first.flags.inverse());
        assert_eq!(first.foreground, Color::Cyan);
        assert_eq!(first.background, Color::Blue);

        assert!(!second.flags.inverse());
        assert_eq!(second.foreground, Color::Default);
        assert_eq!(second.background, Color::Default);
    }

    #[test]
    fn split_truecolor_sequence_does_not_leak_text() {
        let mut terminal = TerminalState::new(32, 2);

        terminal.process_input(b"\x1b[38");
        terminal.process_input(b";2;81;175;239msrc");

        assert_eq!(terminal.grid[0][0].character, 's');
        assert_eq!(terminal.grid[0][1].character, 'r');
        assert_eq!(terminal.grid[0][2].character, 'c');
        assert_eq!(terminal.grid[0][0].foreground, Color::Rgb(81, 175, 239));
    }

    #[test]
    fn csi_executes_embedded_crlf_and_continues_parsing() {
        let mut terminal = TerminalState::new(16, 3);

        // util-linux `more` can wrap Git's colored output in the middle of an
        // SGR parameter, producing this exact ESC [ 3 CR LF 3 m shape.
        terminal.process_input(b"A\x1b[3\r\n3mB\x1b[mC");

        assert!(terminal.pending_escape.is_empty());
        assert_eq!(terminal.cursor_row, 1);
        assert_eq!(terminal.grid[1][0].character, 'B');
        assert_eq!(terminal.grid[1][0].foreground, Color::Yellow);
        assert_eq!(terminal.grid[1][1].character, 'C');
        assert_eq!(terminal.grid[1][1].foreground, Color::Default);
    }

    #[test]
    fn partial_csi_does_not_replay_embedded_linefeed_on_next_chunk() {
        let mut terminal = TerminalState::new(16, 4);

        terminal.process_input(b"\x1b[3\r\n");
        assert_eq!(terminal.cursor_row, 1);
        assert_eq!(terminal.pending_escape, b"\x1b[3");

        terminal.process_input(b"3mX");

        assert!(terminal.pending_escape.is_empty());
        assert_eq!(terminal.cursor_row, 1);
        assert_eq!(terminal.grid[1][0].character, 'X');
        assert_eq!(terminal.grid[1][0].foreground, Color::Yellow);
    }

    #[test]
    fn trailing_escape_is_buffered_until_next_chunk() {
        let mut terminal = TerminalState::new(8, 2);

        terminal.process_input(b"\x1b");
        terminal.process_input(b"[31mX");

        assert_eq!(terminal.grid[0][0].character, 'X');
        assert_eq!(terminal.grid[0][0].foreground, Color::Red);
    }

    #[test]
    fn dec_special_graphics_charset_maps_line_drawing() {
        let mut terminal = TerminalState::new(8, 2);

        terminal.process_input(b"\x1b(0qx\x0fA");

        assert_eq!(terminal.grid[0][0].character, '─');
        assert_eq!(terminal.grid[0][1].character, '│');
        assert_eq!(terminal.grid[0][2].character, 'A');
    }

    #[test]
    fn decscusr_with_intermediate_space_does_not_leak_text() {
        let mut terminal = TerminalState::new(8, 2);

        terminal.process_input(b"\x1b[0 qX");

        assert_eq!(terminal.grid[0][0].character, 'X');
    }

    #[test]
    fn decscusr_uses_xterm_vte_cursor_shape_mapping() {
        let mut terminal = TerminalState::new(8, 2);

        terminal.process_input(b"\x1b[2 q");
        assert_eq!(terminal.cursor_shape, CursorShape::Block);

        terminal.process_input(b"\x1b[3 q");
        assert_eq!(terminal.cursor_shape, CursorShape::Underline);
        terminal.process_input(b"\x1b[4 q");
        assert_eq!(terminal.cursor_shape, CursorShape::Underline);

        terminal.process_input(b"\x1b[5 q");
        assert_eq!(terminal.cursor_shape, CursorShape::Beam);
        terminal.process_input(b"\x1b[6 q");
        assert_eq!(terminal.cursor_shape, CursorShape::Beam);
    }

    #[test]
    fn private_csi_u_sequence_does_not_restore_cursor_or_leak() {
        let mut terminal = TerminalState::new(8, 2);

        terminal.process_input(b"AB");
        terminal.process_input(b"\x1b[?4uC");

        assert_eq!(terminal.grid[0][0].character, 'A');
        assert_eq!(terminal.grid[0][1].character, 'B');
        assert_eq!(terminal.grid[0][2].character, 'C');
    }

    #[test]
    fn csi_with_gt_prefix_is_consumed_without_printing_parameters() {
        let mut terminal = TerminalState::new(8, 2);

        terminal.process_input(b"\x1b[>4;1mZ");

        assert_eq!(terminal.grid[0][0].character, 'Z');
        assert_eq!(terminal.grid[0][1].character, ' ');
    }

    #[test]
    fn dcs_sequence_is_consumed_without_leaking_text() {
        let mut terminal = TerminalState::new(8, 2);

        terminal.process_input(b"\x1bP$q q\x1b\\X");

        assert_eq!(terminal.grid[0][0].character, 'X');
        assert_eq!(terminal.grid[0][1].character, ' ');
    }

    #[test]
    fn standard_kitty_apc_reaches_graphics_state_without_leaking_text() {
        let mut terminal = TerminalState::new(8, 2);

        terminal.process_input(b"\x1b_Gf=32,s=1,v=1,a=T,i=42;AQIDBA==\x1b\\X");

        let image = terminal.kitty_graphics.get_image(42).unwrap();
        assert_eq!((image.width, image.height), (1, 1));
        assert_eq!(image.data, [1, 2, 3, 4]);
        assert_eq!(terminal.kitty_graphics.get_placements().len(), 1);
        assert_eq!(terminal.grid[0][0].character, 'X');
        assert_eq!(terminal.grid[0][1].character, ' ');
    }

    #[test]
    fn kitty_graphics_does_not_route_dcs_sos_pm_or_non_g_apc() {
        // Only an APC whose payload starts with `G` is a graphics command. The
        // old `payload.contains("a=")` sniff handed an unrelated DCS — a
        // DECRQSS reply, a tmux passthrough — straight to the graphics state.
        let body = b"Ga=t,i=41,f=32,s=1,v=1;/wAA/w==";

        for introducer in *b"PX^" {
            let mut terminal = TerminalState::new(8, 2);
            let mut sequence = vec![0x1b, introducer];
            sequence.extend_from_slice(body);
            sequence.extend_from_slice(b"\x1b\\");

            terminal.process_input(&sequence);
            assert!(
                terminal.kitty_graphics.get_image(41).is_none(),
                "non-APC introducer {introducer:#x} was routed as Kitty graphics"
            );
            assert!(terminal.get_output().is_empty());
        }

        // An APC that is not a graphics command is ignored too.
        let mut terminal = TerminalState::new(8, 2);
        terminal.process_input(b"\x1b_a=t,i=41,f=32,s=1,v=1;/wAA/w==\x1b\\");
        assert!(terminal.kitty_graphics.get_image(41).is_none());
        assert!(terminal.get_output().is_empty());
    }

    #[test]
    fn kitty_graphics_answers_an_addressed_command_through_the_output_buffer() {
        let mut terminal = TerminalState::new(8, 2);

        terminal.process_input(b"\x1b_Ga=q,i=31\x1b\\");

        assert_eq!(
            String::from_utf8(terminal.get_output()).unwrap(),
            "\x1b_Gi=31;OK\x1b\\"
        );
    }

    #[test]
    fn kitty_graphics_reports_an_unsupported_file_transport() {
        let mut terminal = TerminalState::new(8, 2);

        // t=f used to hand a base64-encoded *file path* to the image decoder and
        // fail silently; it is now a typed ENOTSUP the client can see.
        terminal.process_input(b"\x1b_Gf=32,t=f,s=1,v=1,a=T,i=32;L3RtcC9pbWFnZQ==\x1b\\");

        assert!(terminal.kitty_graphics.get_image(32).is_none());
        assert_eq!(
            String::from_utf8(terminal.get_output()).unwrap(),
            "\x1b_Gi=32;ENOTSUP:unsupported kitty graphics transport\x1b\\"
        );
    }

    #[test]
    fn kitty_placement_anchors_at_the_cursor_and_x_y_crop_the_source() {
        let mut terminal = TerminalState::new(8, 4);

        // Park the cursor at row 2, column 3, then transmit-and-display a 2x2
        // image cropped to its bottom-right pixel.
        terminal.process_input(b"\x1b[3;4H");
        terminal
            .process_input(b"\x1b_Gf=32,s=2,v=2,a=T,i=33,x=1,y=1;AAAA/wEAAP8AAQD/AQEA/w==\x1b\\");

        let placement = &terminal.kitty_graphics.get_placements()[0];
        assert_eq!((placement.col, placement.row), (3, 2));
        assert_eq!((placement.src_x, placement.src_y), (1, 1));
    }

    #[test]
    fn ris_drops_an_unfinished_kitty_transfer() {
        let mut terminal = TerminalState::new(8, 2);

        terminal.process_input(b"\x1b_Gf=32,s=1,v=1,a=t,i=34,m=1;AQID\x1b\\");
        terminal.process_input(b"\x1bc");
        let _ = terminal.get_output();
        terminal.process_input(b"\x1b_Gm=0;BA==\x1b\\");

        assert!(terminal.kitty_graphics.get_image(34).is_none());
    }

    #[test]
    fn fragmented_kitty_apc_advances_its_scan_cursor_and_stays_bounded() {
        let mut terminal = TerminalState::new(8, 2);
        terminal.process_input(b"\x1b_Gi=52,f=32,s=1,v=1;");
        assert_eq!(
            terminal.pending_apc_scan_from,
            terminal.pending_apc.len().saturating_sub(1)
        );

        for fragment in [
            b"AQ".as_slice(),
            b"ID".as_slice(),
            b"BA".as_slice(),
            b"==".as_slice(),
        ] {
            let old_len = terminal.pending_apc.len();
            terminal.process_input(fragment);
            assert_eq!(terminal.pending_apc.len(), old_len + fragment.len());
            assert_eq!(
                terminal.pending_apc_scan_from,
                terminal.pending_apc.len().saturating_sub(1),
                "unterminated fragments must resume scanning at the previous tail"
            );
        }
        terminal.process_input(b"\x1b\\");
        assert!(terminal.pending_apc.is_empty());
        assert!(terminal.kitty_graphics.get_image(52).is_some());
        let _ = terminal.get_output();

        // An oversized packet is rejected with a bounded response and then
        // discarded through its ST — never buffered wholesale and never
        // parsed as ordinary input afterwards.
        let mut oversized = b"\x1b_Ga=p,i=53,q=0;".to_vec();
        oversized.resize(MAX_PENDING_ESCAPE + 1, b'A');
        terminal.process_input(&oversized);
        assert!(terminal.pending_apc.is_empty());
        assert!(terminal.discarding_oversized_apc);
        let response = terminal.get_output();
        assert!(
            response.starts_with(b"\x1b_Gi=53;EINVAL:"),
            "{}",
            String::from_utf8_lossy(&response)
        );
        assert!(response.len() < 256);

        // Discard through ST without allocating the oversized packet, then
        // resume ordinary terminal parsing on the same input batch.
        terminal.process_input(b"\x1b\\Z");
        assert!(!terminal.discarding_oversized_apc);
        assert_eq!(terminal.grid[0][0].character, 'Z');

        // The bytes after ST belong to the normal stream, not the APC. Even
        // when they make the whole read exceed the cap, a packet whose
        // terminator is itself within the cap must be completed and the
        // remainder preserved.
        let mut near_limit = b"\x1b_Gi=55,f=32,s=1,v=1,q=2;".to_vec();
        near_limit.resize(MAX_PENDING_ESCAPE - 2, b'A');
        terminal.process_input(&near_limit);
        terminal.process_input(b"\x1b\\Y");
        assert!(!terminal.discarding_oversized_apc);
        assert!(terminal.pending_apc.is_empty());
        assert_eq!(terminal.grid[0][1].character, 'Y');
    }

    #[test]
    fn a_kitty_apc_split_across_reads_is_never_dropped_wholesale() {
        // Regression coverage for the old behavior: a transfer larger than
        // the coalesced read size used to land in pending_escape, get cleared
        // at the 1 MiB cap, and have its base64 tail parsed as text.
        let payload = b"\x1b_Gf=32,s=1,v=1,a=t,i=56;AQIDBA==\x1b\\";
        for split_at in 1..payload.len() {
            let mut terminal = TerminalState::new(8, 2);
            terminal.process_input(&payload[..split_at]);
            assert!(
                terminal.kitty_graphics.get_image(56).is_none(),
                "incomplete APC was applied at split {split_at}"
            );
            terminal.process_input(&payload[split_at..]);
            assert!(
                terminal.kitty_graphics.get_image(56).is_some(),
                "APC was lost at input split {split_at}"
            );
            assert_eq!(
                terminal.grid[0][0].character, ' ',
                "APC bytes leaked onto the grid at split {split_at}"
            );
        }
    }

    #[test]
    fn fragmented_osc_advances_its_scan_cursor_and_sets_the_title() {
        let mut terminal = TerminalState::new(16, 2);
        terminal.process_input(b"\x1b]0;ti");
        assert_eq!(
            terminal.pending_osc_scan_from,
            terminal.pending_osc.len().saturating_sub(1)
        );

        for fragment in [b"tl".as_slice(), b"e-".as_slice(), b"x".as_slice()] {
            let old_len = terminal.pending_osc.len();
            terminal.process_input(fragment);
            assert_eq!(terminal.pending_osc.len(), old_len + fragment.len());
            assert_eq!(
                terminal.pending_osc_scan_from,
                terminal.pending_osc.len().saturating_sub(1),
                "unterminated fragments must resume scanning at the previous tail"
            );
        }
        assert_eq!(terminal.window_title, "");
        terminal.process_input(b"\x07Z");
        assert!(terminal.pending_osc.is_empty());
        assert_eq!(terminal.window_title, "title-x");
        // Bytes after the BEL are ordinary input again.
        assert_eq!(terminal.grid[0][0].character, 'Z');
    }

    #[test]
    fn fragmented_osc_st_terminator_straddles_the_read_boundary() {
        let mut terminal = TerminalState::new(16, 2);
        // The ESC introducing ST ends one read; the `\` opens the next.
        terminal.process_input(b"\x1b]2;win\x1b");
        assert_eq!(
            terminal.pending_osc_scan_from,
            terminal.pending_osc.len().saturating_sub(1)
        );
        terminal.process_input(b"\\Y");
        assert!(terminal.pending_osc.is_empty());
        assert_eq!(terminal.window_title, "win");
        assert_eq!(terminal.grid[0][0].character, 'Y');

        // An ESC not followed by `\` stays payload, even across reads.
        let mut terminal = TerminalState::new(16, 2);
        terminal.process_input(b"\x1b]0;a\x1b");
        terminal.process_input(b"Xb\x07");
        assert_eq!(terminal.window_title, "aXb");
        assert_eq!(terminal.grid[0][0].character, ' ');
    }

    #[test]
    fn an_osc_split_across_reads_is_never_dropped_wholesale() {
        // Same regression shape as the kitty APC split test: every split
        // point must deliver the title exactly once and leak nothing.
        for payload in [
            b"\x1b]0;split-title\x07".as_slice(),
            b"\x1b]0;split-title\x1b\\".as_slice(),
        ] {
            for split_at in 1..payload.len() {
                let mut terminal = TerminalState::new(16, 2);
                terminal.process_input(&payload[..split_at]);
                assert_eq!(
                    terminal.window_title, "",
                    "incomplete OSC was applied at split {split_at}"
                );
                terminal.process_input(&payload[split_at..]);
                assert_eq!(
                    terminal.window_title, "split-title",
                    "OSC was lost at input split {split_at}"
                );
                assert_eq!(
                    terminal.grid[0][0].character, ' ',
                    "OSC bytes leaked onto the grid at split {split_at}"
                );
            }
        }
    }

    #[test]
    fn oversized_osc_keeps_the_old_pending_escape_overflow_semantics() {
        let mut terminal = TerminalState::new(64, 2);

        // A terminator inside the read that crosses the cap still completes
        // the packet, exactly like the old merged re-parse did.
        terminal.process_input(b"\x1b]0;huge");
        let mut crossing = Vec::new();
        crossing.resize(MAX_PENDING_ESCAPE + 8, b'A');
        crossing.extend_from_slice(b"\x07");
        terminal.process_input(&crossing);
        assert!(terminal.pending_osc.is_empty());
        assert!(terminal.window_title.starts_with("huge"));

        // Without a terminator the prefix is retained past the cap, then
        // abandoned wholesale on the next read; that read parses as ordinary
        // input (the old pending_escape clear behavior).
        let mut terminal = TerminalState::new(64, 2);
        let mut oversized = b"\x1b]0;never".to_vec();
        oversized.resize(MAX_PENDING_ESCAPE + 8, b'A');
        terminal.process_input(&oversized);
        assert!(!terminal.pending_osc.is_empty());
        terminal.process_input(b"tail\x07");
        assert!(terminal.pending_osc.is_empty());
        assert_eq!(terminal.window_title, "");
        assert_eq!(terminal.grid[0][0].character, 't');
        assert_eq!(terminal.grid[0][1].character, 'a');
        assert_eq!(terminal.grid[0][2].character, 'i');
        assert_eq!(terminal.grid[0][3].character, 'l');
    }

    #[test]
    fn fragmented_dcs_streams_and_drops_its_payload() {
        let mut terminal = TerminalState::new(16, 2);
        terminal.process_input(b"\x1bP1$r");
        assert_eq!(
            terminal.pending_dcs_scan_from,
            terminal.pending_dcs.len().saturating_sub(1)
        );
        terminal.process_input(b"more-payload\x1b");
        terminal.process_input(b"\\Q");
        assert!(terminal.pending_dcs.is_empty());
        // The DCS payload is dropped; only the byte after ST lands.
        assert_eq!(terminal.grid[0][0].character, 'Q');

        // SOS and PM share the DCS shape: ST-terminated, payload dropped.
        let mut terminal = TerminalState::new(16, 2);
        terminal.process_input(b"\x1bXsos-payload");
        terminal.process_input(b"\x1b\\");
        assert!(terminal.pending_dcs.is_empty());
        terminal.process_input(b"\x1b^pm-payload\x1b");
        terminal.process_input(b"\\W");
        assert!(terminal.pending_dcs.is_empty());
        assert_eq!(terminal.grid[0][0].character, 'W');

        // BEL does not terminate a DCS, and an oversized prefix is abandoned
        // on the next read just like the OSC path.
        let mut terminal = TerminalState::new(64, 2);
        let mut oversized = b"\x1bPpayload-with-bel\x07".to_vec();
        oversized.resize(MAX_PENDING_ESCAPE + 8, b'A');
        terminal.process_input(&oversized);
        assert!(!terminal.pending_dcs.is_empty());
        terminal.process_input(b"S");
        assert!(terminal.pending_dcs.is_empty());
        assert_eq!(terminal.grid[0][0].character, 'S');
    }

    #[test]
    fn primary_and_secondary_device_attributes_are_reported() {
        let mut terminal = TerminalState::new(8, 2);

        terminal.process_input(b"\x1b[c\x1b[>c");

        assert_eq!(
            String::from_utf8(terminal.get_output()).unwrap(),
            "\x1b[?65;1;9c\x1b[>1;7802;0c"
        );
    }

    #[test]
    fn xtversion_query_is_reported() {
        let mut terminal = TerminalState::new(8, 2);

        terminal.process_input(b"\x1b[>0q");

        assert_eq!(
            String::from_utf8(terminal.get_output()).unwrap(),
            "\x1bP>|VTE(7802)\x1b\\"
        );
    }

    #[test]
    fn xtwinops_queries_are_reported() {
        let mut terminal = TerminalState::new(80, 24);
        terminal.set_viewport_pixel_size(640, 384);
        terminal.process_input(b"\x1b]0;demo\x1b\\");

        terminal.process_input(b"\x1b[11t\x1b[13t\x1b[14t\x1b[18t\x1b[19t\x1b[20t\x1b[21t");

        assert_eq!(
            String::from_utf8(terminal.get_output()).unwrap(),
            "\x1b[1t\x1b[3;0;0t\x1b[4;384;640t\x1b[8;24;80t\x1b[9;24;80t\x1b]Ldemo\x1b\\\x1b]ldemo\x1b\\"
        );
    }

    #[test]
    fn osc_icon_and_window_titles_are_tracked_separately() {
        let mut terminal = TerminalState::new(80, 24);

        terminal.process_input(b"\x1b]1;icon\x1b\\\x1b]2;window\x1b\\");
        terminal.process_input(b"\x1b[20t\x1b[21t");

        assert_eq!(
            String::from_utf8(terminal.get_output()).unwrap(),
            "\x1b]Licon\x1b\\\x1b]lwindow\x1b\\"
        );
    }

    #[test]
    fn osc7_reports_the_childs_cwd_through_the_terminal_state() {
        let mut terminal = TerminalState::new(80, 24);
        assert_eq!(terminal.current_working_dir(), None);

        terminal.process_input(b"\x1b]7;file://localhost/home/user/My%20Files\x1b\\");
        assert_eq!(terminal.current_working_dir(), Some("/home/user/My Files"));

        // An empty host is the shape `vte.sh` emits most often.
        terminal.process_input(b"\x1b]7;file:///tmp\x1b\\");
        assert_eq!(terminal.current_working_dir(), Some("/tmp"));

        // A bare absolute path is accepted too; some shells emit only that.
        terminal.process_input(b"\x1b]7;/srv\x1b\\");
        assert_eq!(terminal.current_working_dir(), Some("/srv"));
    }

    /// The reason OSC 7 could not be added without the host check: this value
    /// drives the file-tree sidebar and the cwd a split or a restored session
    /// inherits, so a shell on the far side of ssh must not be able to steer it.
    #[test]
    fn osc7_from_a_remote_host_does_not_move_the_local_cwd() {
        let mut terminal = TerminalState::new(80, 24);
        terminal.process_input(b"\x1b]7;file:///home/user\x1b\\");

        terminal.process_input(b"\x1b]7;file://definitely-remote.invalid/etc\x1b\\");
        assert_eq!(
            terminal.current_working_dir(),
            Some("/home/user"),
            "a rejected payload must leave the last known local directory alone"
        );

        // Malformed payloads are rejected on the same terms.
        for payload in [
            "\x1b]7;\x1b\\",
            "\x1b]7;relative/path\x1b\\",
            "\x1b]7;file://localhost/%zz\x1b\\",
            "\x1b]7;file://localhost/tmp/%00etc\x1b\\",
        ] {
            terminal.process_input(payload.as_bytes());
            assert_eq!(
                terminal.current_working_dir(),
                Some("/home/user"),
                "payload {payload:?} must be rejected"
            );
        }
    }

    #[test]
    fn osc_titles_are_bounded_and_safe_for_app_chrome() {
        let mut terminal = TerminalState::new(80, 24);
        let hostile = format!(
            "\x1b]2;safe\n\u{202e}{}tail\x1b\\",
            "x".repeat(MAX_TERMINAL_TITLE_CHARS + 64)
        );

        terminal.process_input(hostile.as_bytes());

        assert_eq!(
            terminal.window_title.chars().count(),
            MAX_TERMINAL_TITLE_CHARS
        );
        assert!(terminal.window_title.starts_with("safe"));
        assert!(!terminal.window_title.contains('\n'));
        assert!(!terminal.window_title.contains('\u{202e}'));
        assert!(!terminal.window_title.ends_with("tail"));
    }

    #[test]
    fn xtwinops_save_and_restore_titles() {
        let mut terminal = TerminalState::new(80, 24);

        terminal.process_input(b"\x1b]0;base\x1b\\");
        terminal.process_input(b"\x1b[22;0t");
        terminal.process_input(b"\x1b]1;temp-icon\x1b\\\x1b]2;temp-window\x1b\\");
        terminal.process_input(b"\x1b[23;0t\x1b[20t\x1b[21t");

        assert_eq!(
            String::from_utf8(terminal.get_output()).unwrap(),
            "\x1b]Lbase\x1b\\\x1b]lbase\x1b\\"
        );
    }

    #[test]
    fn double_click_selects_full_url() {
        let mut terminal = TerminalState::new(64, 2);

        terminal.process_input(b"see https://example.com/path?a=1&b=2 now");
        terminal.select_word_at(0, 12);

        assert_eq!(
            terminal.copy_selection().as_deref(),
            Some("https://example.com/path?a=1&b=2")
        );
    }

    #[test]
    fn double_click_selects_file_path_with_line_number() {
        let mut terminal = TerminalState::new(64, 2);

        terminal.process_input(b"open src/main.rs:1480 please");
        terminal.select_word_at(0, 8);

        assert_eq!(
            terminal.copy_selection().as_deref(),
            Some("src/main.rs:1480")
        );
    }

    #[test]
    fn double_click_excludes_wrapping_punctuation() {
        let mut terminal = TerminalState::new(64, 2);

        terminal.process_input(b"(https://example.com/path), next");
        terminal.select_word_at(0, 10);

        assert_eq!(
            terminal.copy_selection().as_deref(),
            Some("https://example.com/path")
        );
    }

    #[test]
    fn double_click_drag_keeps_word_boundaries() {
        let mut terminal = TerminalState::new(64, 2);

        terminal.process_input(b"Cargo.lock  Cargo.toml  src  target");
        terminal.select_word_at(0, 4);
        terminal.extend_word_selection_to(0, 18);

        assert_eq!(
            terminal.copy_selection().as_deref(),
            Some("Cargo.lock  Cargo.toml")
        );
    }

    #[test]
    fn triple_click_drag_on_same_row_keeps_full_line() {
        let mut terminal = TerminalState::new(64, 2);

        terminal.process_input(b"Cargo.lock  Cargo.toml  src  target");
        terminal.start_selection((0, 0));
        terminal.update_selection((0, 63));
        terminal.extend_line_selection_to(0);

        assert_eq!(terminal.row_selection_cols(0), Some((0, 63)));
        assert_eq!(
            terminal.copy_selection().as_deref().map(str::trim_end),
            Some("Cargo.lock  Cargo.toml  src  target")
        );
    }

    #[test]
    fn bracketed_paste_mode_is_tracked() {
        let mut terminal = TerminalState::new(8, 2);

        terminal.process_input(b"\x1b[?2004h");
        assert!(terminal.is_bracketed_paste_enabled());

        terminal.process_input(b"\x1b[?2004l");
        assert!(!terminal.is_bracketed_paste_enabled());
    }

    #[test]
    fn kitty_keyboard_flags_can_be_set_queried_and_popped() {
        let mut terminal = TerminalState::new(8, 2);

        terminal.process_input(b"\x1b[=1u");
        assert_eq!(terminal.keyboard_enhancement_flags(), 1);

        terminal.process_input(b"\x1b[?u");
        assert_eq!(
            String::from_utf8(terminal.get_output()).unwrap(),
            "\x1b[?1u"
        );

        terminal.process_input(b"\x1b[>5u");
        assert_eq!(terminal.keyboard_enhancement_flags(), 5);

        terminal.process_input(b"\x1b[<u");
        assert_eq!(terminal.keyboard_enhancement_flags(), 1);
    }

    #[test]
    fn xtmodkeys_and_xtfmtkeys_state_is_tracked() {
        let mut terminal = TerminalState::new(8, 2);

        terminal.process_input(b"\x1b[>4;2m\x1b[>4;1f");

        assert_eq!(terminal.xterm_modify_other_keys(), 2);
        assert_eq!(terminal.xterm_format_other_keys(), 1);
    }

    #[test]
    fn report_all_keys_follows_the_kitty_flag_not_theme_notifications() {
        let mut terminal = TerminalState::new(8, 2);

        // Mode 2031 is the in-band theme-change notification, not a keyboard
        // mode: it is tracked, but the keyboard stays in its legacy encoding.
        terminal.process_input(b"\x1b[?2031h");
        assert!(terminal.modes.contains(&2031));
        assert!(!terminal.is_report_all_keys_enabled());

        terminal.process_input(b"\x1b[?2031l");
        assert!(!terminal.is_report_all_keys_enabled());

        terminal.process_input(b"\x1b[>8u");
        assert!(terminal.is_report_all_keys_enabled());

        terminal.process_input(b"\x1b[<u");
        assert!(!terminal.is_report_all_keys_enabled());
    }

    #[test]
    fn osc_5522_read_request_is_queued() {
        let mut terminal = TerminalState::new(8, 2);

        terminal.process_input(b"\x1b]5522;type=read;Lg==\x1b\\");

        let requests = terminal.take_clipboard_read_requests();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].kind, ClipboardReadKind::MimeList);
    }

    #[test]
    fn osc_5522_read_requests_are_bounded_per_ui_batch() {
        let mut terminal = TerminalState::new(8, 2);
        let request = b"\x1b]5522;type=read;Lg==\x1b\\";

        for _ in 0..64 {
            terminal.process_input(request);
        }

        assert_eq!(terminal.take_clipboard_read_requests().len(), 8);
    }

    #[test]
    fn osc_4_palette_set_query_and_reset() {
        let mut terminal = TerminalState::new(8, 2);

        terminal.process_input(b"\x1b]4;1;#112233;2;rgb:4455/6677/8899\x1b\\");
        assert_eq!(terminal.dynamic_palette[1], Some((0x11, 0x22, 0x33)));
        assert_eq!(terminal.dynamic_palette[2], Some((0x44, 0x66, 0x88)));

        terminal.process_input(b"\x1b]4;1;?;2;?\x1b\\");
        assert_eq!(
            String::from_utf8(terminal.get_output()).unwrap(),
            "\x1b]4;1;rgb:1111/2222/3333\x1b\\\x1b]4;2;rgb:4444/6666/8888\x1b\\"
        );

        terminal.process_input(b"\x1b]104;1\x1b\\");
        assert_eq!(terminal.dynamic_palette[1], None);
        assert_eq!(terminal.dynamic_palette[2], Some((0x44, 0x66, 0x88)));

        terminal.process_input(b"\x1b]104\x1b\\");
        assert_eq!(terminal.dynamic_palette[2], None);
    }

    #[test]
    fn osc_110_111_112_reset_dynamic_colors() {
        let mut terminal = TerminalState::new(8, 2);

        terminal.process_input(b"\x1b]10;#112233\x1b\\\x1b]11;#445566\x1b\\\x1b]12;#778899\x1b\\");
        assert_eq!(terminal.dynamic_fg, Some((0x11, 0x22, 0x33)));
        assert_eq!(terminal.dynamic_bg, Some((0x44, 0x55, 0x66)));
        assert_eq!(terminal.dynamic_cursor_color, Some((0x77, 0x88, 0x99)));

        terminal.process_input(b"\x1b]110\x1b\\\x1b]111\x1b\\\x1b]112\x1b\\");
        assert_eq!(terminal.dynamic_fg, None);
        assert_eq!(terminal.dynamic_bg, None);
        assert_eq!(terminal.dynamic_cursor_color, None);
    }

    #[test]
    fn decrqm_reports_5522_support() {
        let mut terminal = TerminalState::new(8, 2);

        terminal.process_input(b"\x1b[?5522$p");

        assert_eq!(
            String::from_utf8(terminal.get_output()).unwrap(),
            "\x1b[?5522;2$y"
        );
    }

    #[test]
    fn decrqm_reports_common_vte_private_modes() {
        let mut terminal = TerminalState::new(8, 2);

        terminal.process_input(b"\x1b[?1;6;7;25;47;66;1004;1006;1047;1048;1049;2004;2026;2031$p");
        assert_eq!(
            String::from_utf8(terminal.get_output()).unwrap(),
            "\x1b[?1;2$y\x1b[?6;2$y\x1b[?7;1$y\x1b[?25;1$y\x1b[?47;2$y\x1b[?66;2$y\x1b[?1004;2$y\x1b[?1006;2$y\x1b[?1047;2$y\x1b[?1048;2$y\x1b[?1049;2$y\x1b[?2004;2$y\x1b[?2026;2$y\x1b[?2031;2$y"
        );

        terminal.process_input(
            b"\x1b[?1h\x1b[?6h\x1b=\x1b[?1004h\x1b[?1006h\x1b[?1048h\x1b[?2004h\x1b[?2031h",
        );
        terminal.process_input(b"\x1b[?1;6;66;1004;1006;1048;2004;2031$p");
        assert_eq!(
            String::from_utf8(terminal.get_output()).unwrap(),
            "\x1b[?1;1$y\x1b[?6;1$y\x1b[?66;1$y\x1b[?1004;1$y\x1b[?1006;1$y\x1b[?1048;1$y\x1b[?2004;1$y\x1b[?2031;1$y"
        );
    }

    #[test]
    fn deckpam_and_deckpnm_toggle_application_keypad() {
        let mut terminal = TerminalState::new(8, 2);

        terminal.process_input(b"\x1b=");
        assert!(terminal.is_application_keypad());

        terminal.process_input(b"\x1b>");
        assert!(!terminal.is_application_keypad());
    }

    #[test]
    fn scrollback_viewport_pinned_when_new_output_arrives() {
        // Reading history while output streams in should not slide the viewport
        // toward the bottom: scroll_offset must compensate when push_scrollback
        // grows the deque.
        let mut terminal = TerminalState::new(2, 2);
        // Push three lines into scrollback.
        terminal.grid.get_mut(0, 0).character = 'A';
        terminal.grid.get_mut(1, 0).character = 'B';
        terminal.cursor_row = 1;
        terminal.process_input(b"\n");
        terminal.grid.get_mut(1, 0).character = 'C';
        terminal.process_input(b"\n");
        terminal.grid.get_mut(1, 0).character = 'D';
        terminal.process_input(b"\n");

        // Scroll up to view 'A','B'.
        terminal.set_scroll_offset(3);
        let before = terminal.get_visible_cells();
        let top_before = before[0][0].character;
        assert_eq!(top_before, 'A');

        // New line arrives — viewport must still show the same top row.
        terminal.grid.get_mut(1, 0).character = 'E';
        terminal.process_input(b"\n");
        let after = terminal.get_visible_cells();
        assert_eq!(after[0][0].character, 'A');
    }

    #[test]
    fn ind_preserves_column_and_scrolls_like_vte() {
        let mut terminal = TerminalState::new(4, 2);
        terminal.grid.get_mut(0, 0).character = 'A';
        terminal.grid.get_mut(1, 0).character = 'B';
        terminal.cursor_row = 1;
        terminal.cursor_col = 2;

        terminal.process_input(b"\x1bD");

        assert_eq!(terminal.cursor_row, 1);
        assert_eq!(terminal.cursor_col, 2);
        assert_eq!(terminal.scrollback.len(), 1);
        assert_eq!(terminal.scrollback[0].decompress()[0].character, 'A');
        assert_eq!(terminal.grid[0][0].character, 'B');
    }

    #[test]
    fn ri_scrolls_region_down_at_top_margin() {
        let mut terminal = TerminalState::new(4, 4);
        terminal.process_input(b"\x1b[2;3r");
        terminal.grid.get_mut(1, 0).character = 'B';
        terminal.grid.get_mut(2, 0).character = 'C';
        terminal.cursor_row = 1;
        terminal.cursor_col = 2;

        terminal.process_input(b"\x1bM");

        assert_eq!(terminal.cursor_row, 1);
        assert_eq!(terminal.cursor_col, 2);
        assert_eq!(terminal.grid[1][0].character, ' ');
        assert_eq!(terminal.grid[2][0].character, 'B');
        assert_eq!(terminal.grid[3][0].character, ' ');
    }

    #[test]
    fn alt_buffer_switch_clears_selection_and_scroll_region() {
        let mut terminal = TerminalState::new(4, 4);
        // Set a partial scroll region and a selection on the main buffer.
        terminal.process_input(b"\x1b[2;3r");
        assert_eq!(terminal.scroll_region_top, 1);
        assert_eq!(terminal.scroll_region_bottom, 2);
        terminal.start_selection((0, 0));
        terminal.update_selection((1, 1));
        assert!(terminal.selection.is_some());

        // Enter alt buffer.
        terminal.process_input(b"\x1b[?1049h");

        assert!(terminal.is_alt_buffer_active());
        assert!(
            terminal.selection.is_none(),
            "selection must clear on alt switch"
        );
        assert_eq!(terminal.scroll_region_top, 0);
        assert_eq!(terminal.scroll_region_bottom, 3);

        // Restore some region & selection in alt buffer, then leave.
        terminal.process_input(b"\x1b[1;2r");
        terminal.start_selection((0, 0));
        terminal.update_selection((1, 1));
        terminal.process_input(b"\x1b[?1049l");

        assert!(!terminal.is_alt_buffer_active());
        assert!(
            terminal.selection.is_none(),
            "selection must clear on alt restore"
        );
        assert_eq!(terminal.scroll_region_top, 0);
        assert_eq!(terminal.scroll_region_bottom, 3);
    }

    #[test]
    fn structural_blank_lines_honor_background_color_erase() {
        let expected = Color::Rgb(40, 44, 52);

        let mut scrolled_up = TerminalState::new(4, 3);
        scrolled_up.process_input(b"\x1b[48;2;40;44;52m\x1b[3;1H\n");
        assert!(scrolled_up.grid[2]
            .iter()
            .all(|cell| cell.background == expected));

        let mut scrolled_down = TerminalState::new(4, 3);
        scrolled_down.process_input(b"\x1b[48;2;40;44;52m\x1b[1;1H\x1bM");
        assert!(scrolled_down.grid[0]
            .iter()
            .all(|cell| cell.background == expected));

        let mut inserted = TerminalState::new(4, 3);
        inserted.process_input(b"\x1b[48;2;40;44;52m\x1b[2;1H\x1b[L");
        assert!(inserted.grid[1]
            .iter()
            .all(|cell| cell.background == expected));

        let mut deleted = TerminalState::new(4, 3);
        deleted.process_input(b"\x1b[48;2;40;44;52m\x1b[2;1H\x1b[M");
        assert!(deleted.grid[2]
            .iter()
            .all(|cell| cell.background == expected));
    }

    #[test]
    fn resizing_hidden_primary_screen_does_not_inherit_alt_background() {
        let mut terminal = TerminalState::new(4, 2);
        terminal.process_input(b"main");

        terminal.process_input(b"\x1b[?1049h");
        terminal.process_input(b"\x1b[44m");
        assert_eq!(terminal.current_bg, Color::Blue);

        terminal.on_resize(6, 2);
        terminal.process_input(b"\x1b[?1049l");

        assert_eq!(terminal.current_bg, Color::Default);
        assert_eq!(terminal.global_bg, Color::Default);
        assert_eq!(terminal.grid[0][4].background, Color::Default);
        assert_eq!(terminal.grid[0][5].background, Color::Default);
        assert_eq!(terminal.grid[1][0].background, Color::Default);
    }

    #[test]
    fn alt_buffer_restores_primary_cursor_shape_and_dynamic_colors() {
        let mut terminal = TerminalState::new(8, 2);
        terminal.process_input(b"\x1b]10;#112233\x1b\\\x1b]11;#445566\x1b\\\x1b]12;#778899\x1b\\");
        terminal.process_input(b"\x1b[3 q");
        terminal.process_input(b"\x1b]4;1;#abcdef\x1b\\");

        terminal.process_input(b"\x1b[?1049h");
        terminal.process_input(b"\x1b]10;#010203\x1b\\\x1b]11;#040506\x1b\\\x1b]12;#070809\x1b\\");
        terminal.process_input(b"\x1b[5 q");
        terminal.process_input(b"\x1b]4;1;#102030\x1b\\");
        terminal.process_input(b"\x1b[?1049l");

        assert_eq!(terminal.cursor_shape, CursorShape::Underline);
        assert_eq!(terminal.dynamic_fg, Some((0x11, 0x22, 0x33)));
        assert_eq!(terminal.dynamic_bg, Some((0x44, 0x55, 0x66)));
        assert_eq!(terminal.dynamic_cursor_color, Some((0x77, 0x88, 0x99)));
        assert_eq!(terminal.dynamic_palette[1], Some((0xab, 0xcd, 0xef)));
    }

    #[test]
    fn erase_display_archives_visible_primary_screen_into_scrollback() {
        let mut terminal = TerminalState::new(8, 3);
        terminal.process_input(b"one\r\ntwo\r\nthree");

        terminal.process_input(b"\x1b[2J");

        assert_eq!(terminal.scrollback.len(), 3);
        assert_eq!(terminal.scrollback[0].decompress()[0].character, 'o');
        assert_eq!(terminal.scrollback[1].decompress()[0].character, 't');
        assert_eq!(terminal.scrollback[2].decompress()[0].character, 't');
        assert!(terminal
            .grid
            .iter()
            .flatten()
            .all(|cell| cell.character == ' '));
    }

    #[test]
    fn erase_scrollback_rebases_live_and_completed_block_rows() {
        let mut terminal = TerminalState::new(20, 4);
        emit_zone(&mut terminal, 0);
        emit_zone(&mut terminal, 1);
        terminal.process_input(b"\x1b]133;A\x07$ \x1b]133;B\x07draft");
        let old_scrollback = terminal.scrollback_len();
        let old_prompt = terminal.live_prompt_row().expect("live prompt");
        let old_zone_rows: Vec<usize> = terminal
            .command_zones
            .iter()
            .map(|zone| zone.prompt_start)
            .collect();
        assert!(old_scrollback > 0);

        terminal.process_input(b"\x1b[3J");

        assert_eq!(terminal.scrollback_len(), 0);
        assert_eq!(
            terminal.live_prompt_row(),
            Some(old_prompt.saturating_sub(old_scrollback))
        );
        for (zone, old_row) in terminal.command_zones.iter().zip(old_zone_rows) {
            if old_row < old_scrollback {
                assert!(zone.rows_evicted);
                assert_eq!(zone.output_start, None);
            } else {
                assert!(!zone.rows_evicted);
                assert_eq!(zone.prompt_start, old_row - old_scrollback);
            }
        }
    }

    #[test]
    fn entering_alt_buffer_does_not_archive_primary_screen() {
        let mut terminal = TerminalState::new(4, 3);
        terminal.process_input(b"main");

        terminal.process_input(b"\x1b[?1049h");

        assert!(terminal.scrollback.is_empty());
        assert!(terminal.is_alt_buffer_active());
    }

    #[test]
    fn printable_at_last_column_defers_autowrap_until_next_printable() {
        let mut terminal = TerminalState::new(4, 3);

        terminal.process_input(b"abcd");

        assert_eq!(terminal.cursor_row, 0);
        assert_eq!(terminal.cursor_col, 3);
        assert!(terminal.pending_wrap);
        assert!(terminal.scrollback.is_empty());

        terminal.process_input(b"X");

        assert_eq!(terminal.cursor_row, 1);
        assert_eq!(terminal.cursor_col, 1);
        assert_eq!(terminal.grid[1][0].character, 'X');
        assert!(terminal.grid.row_wrapped[0]);
    }

    #[test]
    fn autowrap_disabled_overwrites_last_column() {
        let mut terminal = TerminalState::new(3, 3);

        terminal.process_input(b"\x1b[?7l");
        terminal.process_input(b"abcd");

        assert_eq!(terminal.cursor_row, 0);
        assert_eq!(terminal.cursor_col, 2);
        assert!(!terminal.pending_wrap);
        assert_eq!(terminal.grid[0][0].character, 'a');
        assert_eq!(terminal.grid[0][1].character, 'b');
        assert_eq!(terminal.grid[0][2].character, 'd');
    }

    #[test]
    fn decrc_restores_pending_wrap_state() {
        let mut terminal = TerminalState::new(6, 3);

        terminal.process_input(b"P>");
        terminal.process_input(b"\x1b7");
        assert!(!terminal.pending_wrap);

        terminal.process_input(b"\x1b[6GR");
        assert_eq!(terminal.cursor_col, 5);
        assert!(terminal.pending_wrap);

        terminal.process_input(b"\x1b8");
        assert_eq!(terminal.cursor_row, 0);
        assert_eq!(terminal.cursor_col, 2);
        assert!(!terminal.pending_wrap);

        terminal.process_input(b"x");
        assert_eq!(terminal.cursor_row, 0);
        assert_eq!(terminal.cursor_col, 3);
        assert_eq!(terminal.grid[0][2].character, 'x');
    }

    #[test]
    fn disable_alt_screen_ignores_alt_buffer_switch_sequences() {
        let mut terminal = TerminalState::new(4, 3);
        terminal.set_disable_alt_screen(true);
        terminal.process_input(b"main");

        terminal.process_input(b"\x1b[?1049h");
        terminal.process_input(b"\x1b[2;2HXY");
        terminal.process_input(b"\x1b[?1049l");

        assert!(!terminal.is_alt_buffer_active());
        assert_eq!(terminal.grid[0][0].character, 'm');
        assert_eq!(terminal.grid[1][1].character, 'X');
        assert_eq!(terminal.grid[1][2].character, 'Y');
    }

    #[test]
    fn enabling_disable_alt_screen_restores_an_active_primary_buffer() {
        let mut terminal = TerminalState::new(6, 3);
        terminal.process_input(b"main");
        terminal.process_input(b"\x1b[?1049h");
        assert!(terminal.is_alt_buffer_active());

        terminal.set_disable_alt_screen(true);

        assert!(!terminal.is_alt_buffer_active());
        assert_eq!(terminal.grid[0][0].character, 'm');
        terminal.process_input(b"\x1b[?1049h");
        assert!(!terminal.is_alt_buffer_active());
    }

    #[test]
    fn sgr_mouse_report_is_not_capped_at_255() {
        let mut terminal = TerminalState::new(400, 50);
        // Enable mouse tracking + SGR encoding.
        terminal.process_input(b"\x1b[?1000h\x1b[?1006h");

        let report = terminal.get_mouse_report(0, 299, 10).unwrap();
        // 1-indexed: column 300, row 11. Pre-fix this would have been 256.
        assert_eq!(report, b"\x1b[<0;300;11M");

        let release = terminal.get_mouse_release_report(0, 299, 10).unwrap();
        assert_eq!(release, b"\x1b[<0;300;11m");
    }

    #[test]
    fn x10_mouse_report_uses_raw_bytes() {
        let mut terminal = TerminalState::new(120, 50);
        terminal.process_input(b"\x1b[?1000h");

        let report = terminal.get_mouse_report(0, 100, 10).unwrap();
        assert_eq!(report, vec![0x1b, b'[', b'M', 32, 133, 43]);

        let release = terminal.get_mouse_release_report(0, 100, 10).unwrap();
        assert_eq!(release, vec![0x1b, b'[', b'M', 35, 133, 43]);
    }

    #[test]
    fn utf8_mouse_report_encodes_coordinates_as_utf8() {
        let mut terminal = TerminalState::new(400, 50);
        terminal.process_input(b"\x1b[?1000h\x1b[?1005h");

        let report = terminal.get_mouse_report(0, 299, 10).unwrap();
        assert_eq!(report, b"\x1b[M \xc5\x8c+");

        let release = terminal.get_mouse_release_report(0, 299, 10).unwrap();
        assert_eq!(release, b"\x1b[M#\xc5\x8c+");
    }

    #[test]
    fn urxvt_mouse_report_uses_decimal_csi_format() {
        let mut terminal = TerminalState::new(400, 50);
        terminal.process_input(b"\x1b[?1000h\x1b[?1015h");

        let report = terminal.get_mouse_report(0, 299, 10).unwrap();
        assert_eq!(report, b"\x1b[32;300;11M");

        let release = terminal.get_mouse_release_report(0, 299, 10).unwrap();
        assert_eq!(release, b"\x1b[35;300;11M");
    }

    /// Emit one complete OSC 133 command zone: prompt, command line, output.
    fn emit_zone(terminal: &mut TerminalState, index: usize) {
        terminal.process_input(b"\x1b]133;A\x07\x1b]133;B\x07");
        terminal.process_input(format!("$ cmd{index}\r\n").as_bytes());
        terminal.process_input(b"\x1b]133;C\x07");
        terminal.process_input(b"out\r\nout\r\nout\r\n");
        terminal.process_input(b"\x1b]133;D;0\x07");
    }

    /// Text content of one `search_lines` row, mirroring a raw cell scan.
    fn search_line_text(line: super::SearchLine<'_>) -> String {
        match line {
            super::SearchLine::Text(text) => text.to_string(),
            super::SearchLine::Cells(cells) => cells.iter().map(|cell| cell.character).collect(),
        }
    }

    /// Text content of an absolute buffer row (scrollback + live grid).
    fn buffer_row_text(terminal: &TerminalState, row: usize) -> String {
        terminal
            .search_lines()
            .nth(row)
            .map(search_line_text)
            .unwrap_or_default()
    }

    #[test]
    fn search_lines_borrows_plain_scrollback_and_decodes_wide_rows() {
        let mut terminal = TerminalState::new(32, 4);
        terminal.process_input(b"plain alpha row\r\n");
        terminal.process_input("wide 中x row\r\n".as_bytes());
        terminal.process_input(b"\x1b[31mstyled beta row\x1b[m\r\n");
        terminal.process_input(b"filler one\r\nfiller two\r\nfiller three\r\n");

        // The first three rows scrolled into history: plain text stays a
        // borrowed string while styled/wide rows keep encoded cell data.
        assert!(matches!(
            &terminal.scrollback[0].data,
            super::CompressedLineData::Plain(..)
        ));
        assert!(matches!(
            &terminal.scrollback[1].data,
            super::CompressedLineData::Encoded(_)
        ));
        assert!(matches!(
            &terminal.scrollback[2].data,
            super::CompressedLineData::Encoded(_)
        ));

        let mut cache = None;
        let (matches, _) = crate::search::SearchEngine::search_lines(
            terminal.search_lines(),
            "alpha",
            false,
            true,
            &mut cache,
        );
        assert_eq!(
            matches,
            vec![crate::search::SearchMatch {
                line: 0,
                col_start: 6,
                col_end: 11,
            }]
        );

        // Wide characters in encoded rows still span their continuation
        // cell when the rows around them are borrowed text.
        let (matches, _) = crate::search::SearchEngine::search_lines(
            terminal.search_lines(),
            "中x",
            false,
            true,
            &mut cache,
        );
        assert_eq!(
            matches,
            vec![crate::search::SearchMatch {
                line: 1,
                col_start: 5,
                col_end: 8,
            }]
        );
    }

    #[test]
    fn command_zones_shift_and_lose_rows_as_scrollback_trims() {
        // DELIBERATE v3 semantic change: v2 dropped a zone once its prompt
        // row was trimmed; with output snapshotted at `D` the entry now
        // outlives its rows as a rows-evicted zone (id, metadata, snapshot).
        const MAX: usize = 8;
        let mut terminal = TerminalState::new(20, 4);
        terminal.set_max_scrollback(MAX);

        emit_zone(&mut terminal, 0);
        emit_zone(&mut terminal, 1);
        assert_eq!(terminal.command_zones.len(), 2);
        let first = terminal.command_zones[0].prompt_start;
        assert!(buffer_row_text(&terminal, first).starts_with("$ cmd0"));

        // Fill to capacity, then trim exactly enough rows to push the first
        // zone's prompt off the top.
        let fills = (MAX - terminal.scrollback_len()) + first + 1;
        for _ in 0..fills {
            terminal.process_input(b"fill\r\n");
        }
        assert_eq!(terminal.command_zones.len(), 2);
        let evicted = &terminal.command_zones[0];
        assert!(evicted.rows_evicted);
        assert_eq!(evicted.output_start, None);
        assert_eq!(evicted.output_end, None);
        // …but its snapshot still answers the copy/Markdown paths.
        assert_eq!(
            terminal
                .zone_output_text(terminal.command_zones[0].id)
                .as_deref(),
            Some("out\nout\nout")
        );
        // The in-range zone still anchors the second command's prompt.
        let shifted = terminal.command_zones[1].prompt_start;
        assert!(!terminal.command_zones[1].rows_evicted);
        assert!(
            buffer_row_text(&terminal, shifted).starts_with("$ cmd1"),
            "zone anchors {:?}",
            buffer_row_text(&terminal, shifted)
        );
        // Evicted zones stop contributing rows to prompt navigation.
        assert_eq!(terminal.prompt_rows().count(), 1);

        // Trim past the second prompt as well: both entries survive, evicted.
        let fills = terminal.command_zones[1].prompt_start + 1;
        for _ in 0..fills {
            terminal.process_input(b"fill\r\n");
        }
        assert_eq!(terminal.command_zones.len(), 2);
        assert!(terminal.command_zones.iter().all(|zone| zone.rows_evicted));
        assert_eq!(terminal.prompt_rows().count(), 0);
    }

    #[test]
    fn zone_state_accessors_expose_the_live_block_boundary() {
        let mut terminal = TerminalState::new(40, 8);
        assert_eq!(terminal.running_zone_start(), None);
        assert_eq!(terminal.live_prompt_row(), None);

        terminal.process_input(b"\x1b]133;A\x07$ ");
        let prompt = terminal.live_prompt_row().expect("prompt started");
        terminal.process_input(b"\x1b]133;B\x07sleep 5\r\n");
        assert_eq!(terminal.live_prompt_row(), Some(prompt));
        assert_eq!(terminal.running_zone_start(), None);

        // `C` flips the block from "editing" to "running": the accent stripe
        // anchors at the same prompt row until `D` completes the zone.
        terminal.process_input(b"\x1b]133;C\x07");
        assert_eq!(terminal.live_prompt_row(), None);
        assert_eq!(terminal.running_zone_start(), Some(prompt));
        assert!(terminal.running_duration_ms().is_some());

        terminal.process_input(b"\x1b]133;D;0\x07");
        assert_eq!(terminal.running_zone_start(), None);
        assert_eq!(terminal.live_prompt_row(), None);
        assert_eq!(terminal.running_duration_ms(), None);
    }

    #[test]
    fn clear_completed_blocks_preserves_idle_prompt_and_monotonic_ids() {
        let mut terminal = TerminalState::new(40, 6);
        emit_zone(&mut terminal, 0);
        emit_zone(&mut terminal, 1);
        terminal.process_input(b"\x1b]133;A\x07$ \x1b]133;B\x07draft");
        let next_id = terminal.next_zone_id;
        let prompt_before = terminal.live_prompt_row().expect("live prompt");
        let scrollback_before = terminal.scrollback_len();
        assert_eq!(terminal.command_zones.len(), 2);
        assert!(terminal.captured_output_bytes > 0);

        assert_eq!(terminal.clear_completed_blocks(), 2);

        assert!(terminal.command_zones.is_empty());
        assert_eq!(terminal.captured_output_bytes, 0);
        assert_eq!(terminal.scroll_offset, 0);
        assert_eq!(terminal.next_zone_id, next_id);
        assert_eq!(
            terminal.live_prompt_row(),
            Some(prompt_before.saturating_sub(prompt_before.min(scrollback_before)))
        );
        let retained: String = terminal.search_lines().map(search_line_text).collect();
        assert!(
            retained.contains("$ draft"),
            "retained buffer: {retained:?}"
        );
        assert!(!retained.contains("cmd0"), "retained buffer: {retained:?}");
        assert!(!retained.contains("cmd1"), "retained buffer: {retained:?}");

        // The still-live editor completes normally, and a stale pre-clear id
        // cannot alias the newly created zone.
        terminal.process_input(b"\r\n\x1b]133;C\x07new output\r\n\x1b]133;D;0\x07");
        assert_eq!(terminal.command_zones.len(), 1);
        assert_eq!(terminal.command_zones[0].id, next_id);
        assert_eq!(terminal.command_zones[0].command.as_deref(), Some("draft"));
    }

    #[test]
    fn clear_completed_blocks_keeps_a_running_block_that_reached_scrollback() {
        let mut terminal = TerminalState::new(32, 4);
        emit_zone(&mut terminal, 0);
        terminal.process_input(
            b"\x1b]133;A\x07$ \x1b]133;B\x07sleep 5\r\n\x1b]133;C\x07start\r\nline2\r\nline3\r\nline4\r\n",
        );
        let start_before = terminal.running_zone_start().expect("running block");
        assert!(start_before < terminal.scrollback_len());

        assert_eq!(terminal.clear_completed_blocks(), 1);

        assert!(terminal.is_command_running());
        assert_eq!(terminal.running_zone_start(), Some(0));
        let retained: String = terminal.search_lines().map(search_line_text).collect();
        assert!(
            retained.contains("sleep 5"),
            "retained buffer: {retained:?}"
        );
        assert!(retained.contains("line4"), "retained buffer: {retained:?}");
        assert!(!retained.contains("cmd0"), "retained buffer: {retained:?}");

        terminal.process_input(b"done\r\n\x1b]133;D;0\x07");
        assert_eq!(terminal.command_zones.len(), 1);
        assert_eq!(
            terminal.command_zones[0].command.as_deref(),
            Some("sleep 5")
        );
        assert_eq!(
            terminal
                .zone_output_text(terminal.command_zones[0].id)
                .as_deref(),
            Some("start\nline2\nline3\nline4\ndone")
        );
    }

    #[test]
    fn clear_completed_blocks_removes_finished_graphics_and_keeps_live_graphics() {
        let mut terminal = TerminalState::new(30, 8);
        terminal.process_input(
            b"\x1b]133;A\x07$ \x1b]133;B\x07image old\r\n\x1b]133;C\x07\x1b_Gf=32,s=1,v=1,a=T,i=51;AQIDBA==\x1b\\\r\n\x1b]133;D;0\x07",
        );
        terminal.process_input(
            b"\x1b]133;A\x07$ \x1b]133;B\x07image live\r\n\x1b]133;C\x07\x1b_Gf=32,s=1,v=1,a=T,i=52;BQYHCA==\x1b\\",
        );
        assert!(terminal.is_command_running());
        assert_eq!(terminal.kitty_graphics.get_placements().len(), 2);
        assert!(terminal.kitty_graphics.get_image(51).is_some());
        assert!(terminal.kitty_graphics.get_image(52).is_some());

        assert_eq!(terminal.clear_completed_blocks(), 1);

        let placements = terminal.kitty_graphics.get_placements();
        assert_eq!(placements.len(), 1);
        assert_eq!(placements[0].image_id, 52);
        assert!(terminal.kitty_graphics.get_image(51).is_none());
        assert!(terminal.kitty_graphics.get_image(52).is_some());
        assert_eq!(terminal.kitty_graphics.image_count(), 1);
        assert!(terminal.is_command_running());

        terminal.process_input(b"\r\n\x1b]133;D;0\x07");
        assert_eq!(
            terminal
                .command_zones
                .back()
                .and_then(|zone| zone.command.as_deref()),
            Some("image live")
        );
    }

    #[test]
    fn clear_completed_blocks_is_a_noop_without_finished_zones() {
        let mut terminal = TerminalState::new(20, 3);
        terminal.process_input(b"plain text");
        let before: String = terminal.search_lines().map(search_line_text).collect();

        assert_eq!(terminal.clear_completed_blocks(), 0);

        let after: String = terminal.search_lines().map(search_line_text).collect();
        assert_eq!(after, before);
    }

    #[test]
    fn undo_clear_completed_blocks_restores_zones_rows_and_live_prompt() {
        let mut terminal = TerminalState::new(40, 6);
        emit_zone(&mut terminal, 0);
        emit_zone(&mut terminal, 1);
        terminal.process_input(b"\x1b]133;A\x07$ \x1b]133;B\x07draft");
        let next_id = terminal.next_zone_id;
        let output_before: Vec<Option<String>> = terminal
            .command_zones
            .iter()
            .map(|zone| terminal.zone_output_text(zone.id))
            .collect();
        let captured_before = terminal.captured_output_bytes;
        assert!(captured_before > 0);
        assert!(terminal
            .command_zones
            .iter()
            .all(|zone| terminal.finished_output_range(zone.id).is_some()));

        assert_eq!(terminal.clear_completed_blocks(), 2);
        assert!(terminal.command_zones.is_empty());

        assert_eq!(terminal.undo_clear_completed_blocks(), 2);

        // Zones return with their ids, commands, outputs, and provenance.
        assert_eq!(terminal.command_zones.len(), 2);
        assert_eq!(terminal.captured_output_bytes, captured_before);
        for (index, zone) in terminal.command_zones.iter().enumerate() {
            assert_eq!(zone.id, index as u64);
            assert_eq!(
                zone.command.as_deref(),
                Some(format!("$ cmd{index}").as_str())
            );
            assert!(
                buffer_row_text(&terminal, zone.prompt_start).contains(&format!("cmd{index}")),
                "restored prompt row for cmd{index}"
            );
            assert_eq!(
                terminal.zone_output_text(zone.id),
                output_before[index],
                "restored output for cmd{index}"
            );
            assert!(
                terminal.finished_output_range(zone.id).is_some(),
                "restored output provenance for cmd{index}"
            );
        }
        // The live draft prompt still owns the bottom of the buffer, and the
        // stash was consumed on the way out.
        let prompt_row = terminal.live_prompt_row().expect("live prompt");
        assert!(buffer_row_text(&terminal, prompt_row).contains("$ draft"));
        assert_eq!(terminal.undo_clear_completed_blocks(), 0);

        // Completing the still-live editor appends after the restored zones
        // with the monotonic id a stale pre-clear id cannot alias.
        terminal.process_input(b"\r\n\x1b]133;C\x07new output\r\n\x1b]133;D;0\x07");
        assert_eq!(terminal.command_zones.len(), 3);
        let last = terminal.command_zones.back().expect("new zone");
        assert_eq!(last.id, next_id);
        assert_eq!(last.command.as_deref(), Some("draft"));
    }

    #[test]
    fn undo_clear_completed_blocks_restores_a_running_block_boundary_exactly() {
        let mut terminal = TerminalState::new(32, 4);
        emit_zone(&mut terminal, 0);
        terminal.process_input(
            b"\x1b]133;A\x07$ \x1b]133;B\x07sleep 5\r\n\x1b]133;C\x07start\r\nline2\r\nline3\r\nline4\r\n",
        );
        let start_before = terminal.running_zone_start().expect("running block");
        // The live lifecycle reached scrollback, so no grid rows are blanked
        // and undo restores the exact pre-clear buffer.
        assert!(start_before < terminal.scrollback_len());
        let buffer_before: String = terminal.search_lines().map(search_line_text).collect();

        assert_eq!(terminal.clear_completed_blocks(), 1);
        assert_eq!(terminal.undo_clear_completed_blocks(), 1);

        assert_eq!(terminal.running_zone_start(), Some(start_before));
        let buffer_after: String = terminal.search_lines().map(search_line_text).collect();
        assert_eq!(buffer_after, buffer_before);

        terminal.process_input(b"done\r\n\x1b]133;D;0\x07");
        assert_eq!(terminal.command_zones.len(), 2);
        assert_eq!(
            terminal
                .zone_output_text(terminal.command_zones[1].id)
                .as_deref(),
            Some("start\nline2\nline3\nline4\ndone")
        );
    }

    #[test]
    fn undo_clear_completed_blocks_is_single_level_and_survives_an_empty_clear() {
        let mut terminal = TerminalState::new(40, 6);
        // Nothing stashed yet: undo is a no-op.
        assert_eq!(terminal.undo_clear_completed_blocks(), 0);

        emit_zone(&mut terminal, 0);
        assert_eq!(terminal.clear_completed_blocks(), 1);
        // A clear that removes nothing keeps the stash (anvil's reflexive
        // second Ctrl+Shift+K rule).
        assert_eq!(terminal.clear_completed_blocks(), 0);
        assert_eq!(terminal.undo_clear_completed_blocks(), 1);
        assert_eq!(terminal.command_zones.len(), 1);
        // Single-level: consumed by the first undo.
        assert_eq!(terminal.undo_clear_completed_blocks(), 0);
    }

    #[test]
    fn undo_clear_completed_blocks_waits_out_the_alt_screen() {
        let mut terminal = TerminalState::new(40, 6);
        emit_zone(&mut terminal, 0);
        assert_eq!(terminal.clear_completed_blocks(), 1);

        terminal.process_input(b"\x1b[?1049h");
        assert_eq!(terminal.undo_clear_completed_blocks(), 0);
        assert!(terminal.command_zones.is_empty());

        terminal.process_input(b"\x1b[?1049l");
        assert_eq!(terminal.undo_clear_completed_blocks(), 1);
        assert_eq!(terminal.command_zones.len(), 1);
    }

    #[test]
    fn undo_clear_completed_blocks_restores_finished_graphics() {
        let mut terminal = TerminalState::new(30, 8);
        terminal.process_input(
            b"\x1b]133;A\x07$ \x1b]133;B\x07image old\r\n\x1b]133;C\x07\x1b_Gf=32,s=1,v=1,a=T,i=51;AQIDBA==\x1b\\\r\n\x1b]133;D;0\x07",
        );
        terminal.process_input(
            b"\x1b]133;A\x07$ \x1b]133;B\x07image live\r\n\x1b]133;C\x07\x1b_Gf=32,s=1,v=1,a=T,i=52;BQYHCA==\x1b\\",
        );
        let stats_before = terminal.kitty_graphics.get_stats();
        let old_buffer_row = terminal.kitty_graphics.get_placements()[0].buffer_row;

        assert_eq!(terminal.clear_completed_blocks(), 1);
        assert_eq!(terminal.kitty_graphics.get_placements().len(), 1);
        assert!(terminal.kitty_graphics.get_image(51).is_none());

        assert_eq!(terminal.undo_clear_completed_blocks(), 1);

        let placements = terminal.kitty_graphics.get_placements();
        assert_eq!(placements.len(), 2);
        // The finished placement returns with its original absolute row and
        // its image data re-admitted against the memory budget; the live one
        // stays anchored to its (rebased) text.
        assert_eq!(placements[0].image_id, 51);
        assert_eq!(placements[0].buffer_row, old_buffer_row);
        assert_eq!(placements[1].image_id, 52);
        assert!(terminal.kitty_graphics.get_image(51).is_some());
        assert_eq!(terminal.kitty_graphics.image_count(), 2);
        assert_eq!(terminal.kitty_graphics.get_stats(), stats_before);
    }

    #[test]
    fn undo_clear_completed_blocks_trims_the_restored_prefix_to_the_scrollback_cap() {
        let mut terminal = TerminalState::new(40, 6);
        emit_zone(&mut terminal, 0);
        emit_zone(&mut terminal, 1);
        emit_zone(&mut terminal, 2);
        // Idle prompt: every row belongs to history, so the stash holds the
        // whole scrollback plus the blanked grid rows.
        let restored_rows = terminal.scrollback_len() + 6;
        let cap = 8;
        let trimmed = restored_rows - cap;
        let survivors: Vec<u64> = terminal
            .command_zones
            .iter()
            .filter(|zone| zone.prompt_start >= trimmed)
            .map(|zone| zone.id)
            .collect();
        assert!(!survivors.is_empty());

        assert_eq!(terminal.clear_completed_blocks(), 3);
        terminal.set_max_scrollback(cap);

        // All three zones come back, but rows beyond the cap are evicted from
        // the oldest (the restored prefix), flipping their zones to
        // rows_evicted exactly like an ordinary trim.
        assert_eq!(terminal.undo_clear_completed_blocks(), 3);
        assert_eq!(terminal.command_zones.len(), 3);
        assert_eq!(terminal.scrollback_len(), cap);
        for zone in &terminal.command_zones {
            assert_eq!(zone.rows_evicted, !survivors.contains(&zone.id));
        }
        let newest = terminal.command_zones.back().expect("newest zone");
        assert!(!newest.rows_evicted);
        assert!(buffer_row_text(&terminal, newest.prompt_start).contains("cmd2"));
    }

    #[test]
    fn undo_clear_completed_blocks_evicts_the_restored_prefix_at_the_zone_cap() {
        let mut terminal = TerminalState::new(40, 8);
        for index in 0..(MAX_COMMAND_ZONES + 4) {
            emit_zone(&mut terminal, index);
        }
        assert_eq!(terminal.command_zones.len(), MAX_COMMAND_ZONES);
        let front_id = terminal.command_zones[0].id;

        assert_eq!(terminal.clear_completed_blocks(), MAX_COMMAND_ZONES);
        for index in 0..4 {
            emit_zone(&mut terminal, MAX_COMMAND_ZONES + 4 + index);
        }
        assert_eq!(terminal.command_zones.len(), 4);

        // The combined history exceeds the cap by four: the four oldest
        // restored zones are evicted, the post-clear zones stay at the back.
        assert_eq!(
            terminal.undo_clear_completed_blocks(),
            MAX_COMMAND_ZONES - 4
        );
        assert_eq!(terminal.command_zones.len(), MAX_COMMAND_ZONES);
        assert_eq!(terminal.command_zones[0].id, front_id + 4);
        assert_eq!(
            terminal.command_zones.back().expect("newest zone").id,
            (MAX_COMMAND_ZONES + 8 - 1) as u64
        );
    }

    #[test]
    fn command_zones_are_enriched_at_both_push_sites() {
        let mut terminal = TerminalState::new(40, 8);
        // Full lifecycle (A/B/C/D): command captured, duration measured.
        terminal.process_input(b"\x1b]133;A\x07$ \x1b]133;B\x07echo hi\r\n");
        terminal.process_input(b"\x1b]133;C\x07hi\r\n\x1b]133;D;0\x07");
        // Clean-prompt asynchronous output finalizes at the next A.
        terminal.process_input(b"\x1b]133;A\x07$ \x1b]133;B\x07\r\nworker done\r\n");
        terminal.process_input(b"\x1b]133;A\x07");

        assert_eq!(terminal.command_zones.len(), 2);
        let full = &terminal.command_zones[0];
        assert_eq!(full.id, 0);
        assert_eq!(full.command.as_deref(), Some("echo hi"));
        assert!(full.duration_ms.is_some());
        assert!(full.finished_at_ms.is_some());
        assert_eq!(full.exit_code, Some(0));
        assert_eq!(
            full.completion_provenance,
            crate::block_mode::CompletionProvenance::ShellReported
        );
        assert!(!full.captured_output_evicted);

        let background = &terminal.command_zones[1];
        assert_eq!(background.id, 1);
        assert_eq!(background.command, None);
        assert_eq!(
            crate::block_mode::classify(background.command.as_deref(), background.exit_code),
            crate::block_mode::BlockOutcome::Background
        );
        assert_eq!(background.duration_ms, None);
        assert!(background.finished_at_ms.is_some());
        assert_eq!(background.exit_code, None);
        assert_eq!(
            background.completion_provenance,
            crate::block_mode::CompletionProvenance::BoundaryInferred
        );
        assert_eq!(
            background.captured_output.as_ref().map(|v| v.0.as_str()),
            Some("\nworker done\n")
        );
        assert!(!background.captured_output_evicted);
    }

    #[test]
    fn empty_prompt_enter_and_d_without_c_mint_no_zone() {
        // bash-preexec-style integrations emit `D` from precmd without a `C`
        // on every empty-prompt Enter. The rendered prompt must not be
        // scraped as the zone's command, and the zone must not surface as a
        // Failed block even when the previous command left a nonzero status.
        let mut terminal = TerminalState::new(40, 8);
        terminal.process_input(b"\x1b]133;A\x07$ \x1b]133;B\x07\r\n\x1b]133;D;1\x07");
        terminal.process_input(b"\x1b]133;A\x07$ \x1b]133;B\x07");

        assert!(terminal.command_zones.is_empty());
        assert!(terminal.take_completed_commands().is_empty());
    }

    #[test]
    fn scrollback_trim_shifts_rows_but_keeps_zone_identity() {
        const MAX: usize = 8;
        let mut terminal = TerminalState::new(20, 4);
        terminal.set_max_scrollback(MAX);
        emit_zone(&mut terminal, 0);
        emit_zone(&mut terminal, 1);
        let survivor = terminal.command_zones[1].clone();
        assert!(survivor
            .command
            .as_deref()
            .is_some_and(|c| c.contains("cmd1")));

        // Trim exactly past the first zone's prompt row.
        let fills = (MAX - terminal.scrollback_len()) + terminal.command_zones[0].prompt_start + 1;
        for _ in 0..fills {
            terminal.process_input(b"fill\r\n");
        }
        // DELIBERATE v3 semantic change: the trimmed zone keeps its entry
        // (v2 dropped it and this test pinned `zone_by_id(0).is_none()`).
        assert_eq!(terminal.command_zones.len(), 2);
        let shifted = &terminal.command_zones[1];
        // Identity and metadata are untouched; only the rows moved.
        assert_eq!(shifted.id, survivor.id);
        assert_eq!(shifted.command, survivor.command);
        assert_eq!(shifted.duration_ms, survivor.duration_ms);
        assert_eq!(shifted.finished_at_ms, survivor.finished_at_ms);
        assert!(shifted.prompt_start < survivor.prompt_start);
        assert!(!shifted.rows_evicted);
        // Id-keyed lookups survive the index shift; the trimmed id resolves
        // to a rows-evicted entry that still knows what it was.
        assert!(terminal.zone_by_id(survivor.id).is_some());
        let evicted = terminal.zone_by_id(0).expect("evicted entry retained");
        assert!(evicted.rows_evicted);
        assert!(evicted
            .command
            .as_deref()
            .is_some_and(|command| command.contains("cmd0")));
    }

    #[test]
    fn first_failed_zone_picks_the_oldest_failure() {
        let mut terminal = TerminalState::new(40, 8);
        // A D mark without a C mark is background output even if it carries a
        // raw non-zero status; failed navigation must skip it.
        terminal.process_input(b"\x1b]133;A\x07$ \x1b]133;B\x07\x1b]133;D;9\x07");
        for exit in ["0", "2", "130", "0"] {
            terminal.process_input(b"\x1b]133;A\x07$ \x1b]133;B\x07cmd\r\n");
            terminal
                .process_input(format!("\x1b]133;C\x07out\r\n\x1b]133;D;{exit}\x07").as_bytes());
        }
        let failed = terminal.first_failed_zone().expect("two failures exist");
        assert_eq!(failed.exit_code, Some(2));
        assert_eq!(failed.id, 1);
    }

    #[test]
    fn zone_output_text_reads_one_zone_by_id() {
        let mut terminal = TerminalState::new(20, 6);
        emit_zone(&mut terminal, 0);
        emit_zone(&mut terminal, 1);
        let first_id = terminal.command_zones[0].id;
        assert_eq!(
            terminal.zone_output_text(first_id).as_deref(),
            Some("out\nout\nout")
        );
        assert_eq!(terminal.zone_output_text(999), None);
        // The capped variant reports the same text plus an (unset here)
        // truncation flag; the flag's mechanics are pinned on `rows_text`.
        assert_eq!(
            terminal.zone_output_text_capped(first_id),
            Some(("out\nout\nout".to_string(), false))
        );
        assert_eq!(terminal.zone_output_text_capped(999), None);
    }

    #[test]
    fn zone_output_snapshot_is_taken_at_d() {
        let mut terminal = TerminalState::new(20, 6);
        emit_zone(&mut terminal, 0);
        assert_eq!(
            terminal.command_zones[0].captured_output,
            Some(("out\nout\nout".to_string(), false))
        );
        assert_eq!(terminal.captured_output_bytes, "out\nout\nout".len());
        // D without C is ignored and therefore records no zone or snapshot.
        terminal.process_input(b"\x1b]133;A\x07$ \x1b]133;B\x07\r\n\x1b]133;D;0\x07");
        terminal.process_input(b"\x1b]133;A\x07");
        assert_eq!(terminal.command_zones.len(), 1);
        assert_eq!(terminal.captured_output_bytes, "out\nout\nout".len());
    }

    #[test]
    fn zone_output_line_rows_count_hard_lines_but_not_soft_wraps() {
        let mut terminal = TerminalState::new(5, 8);
        terminal.process_input(
            b"\x1b]133;A\x07$ \x1b]133;B\x07x\r\n\x1b]133;C\x07abcdef\r\ngh\r\n\x1b]133;D;0\x07",
        );
        let zone = terminal.command_zones.back().expect("completed block");
        let id = zone.id;
        let output_start = zone.output_start.expect("retained output rows");

        assert!(if output_start < terminal.scrollback.len() {
            terminal.scrollback[output_start].is_wrapped
        } else {
            terminal.grid.row_wrapped[output_start - terminal.scrollback.len()]
        });
        assert_eq!(terminal.zone_output_line_row(id, 0), None);
        assert_eq!(terminal.zone_output_line_row(id, 1), Some(output_start));
        assert_eq!(terminal.zone_output_line_row(id, 2), Some(output_start + 2));
        assert_eq!(terminal.zone_output_line_row(id, 3), None);
        assert_eq!(terminal.zone_output_line_row(u64::MAX, 1), None);

        // `abcdef` occupies two physical rows at width five. A match wholly
        // in the continuation lands there; a cross-wrap match lands on the
        // row containing its first character and is validated through row 2.
        assert_eq!(
            terminal.zone_output_match_row(id, 1, 5, 6),
            Some(output_start + 1)
        );
        assert_eq!(
            terminal.zone_output_match_row(id, 1, 4, 6),
            Some(output_start)
        );
        assert_eq!(
            terminal.zone_output_match_row(id, 2, 0, 2),
            Some(output_start + 2)
        );
        assert_eq!(terminal.zone_output_match_row(id, 1, 5, 7), None);
        assert_eq!(terminal.zone_output_match_row(id, 1, 6, 6), None);
        assert_eq!(terminal.zone_output_match_row(id, 0, 0, 1), None);
        assert_eq!(terminal.zone_output_match_row(u64::MAX, 1, 0, 1), None);

        terminal.command_zones.back_mut().unwrap().rows_evicted = true;
        assert_eq!(terminal.zone_output_line_row(id, 1), None);
        assert_eq!(terminal.zone_output_match_row(id, 1, 0, 1), None);
    }

    #[test]
    fn zone_output_match_row_reaches_a_match_beyond_200_characters() {
        let mut terminal = TerminalState::new(20, 40);
        terminal.process_input(b"\x1b]133;A\x07$ \x1b]133;B\x07x\r\n\x1b]133;C\x07");
        let output = format!("{}needle\r\n", "x".repeat(250));
        terminal.process_input(output.as_bytes());
        terminal.process_input(b"\x1b]133;D;0\x07");
        let zone = terminal.command_zones.back().expect("completed block");
        let output_start = zone.output_start.expect("retained output rows");

        assert_eq!(
            terminal.zone_output_match_row(zone.id, 1, 250, 256),
            Some(output_start + 12)
        );
    }

    #[test]
    fn snapshot_budget_evicts_oldest_snapshots_but_keeps_zone_entries() {
        let mut terminal = TerminalState::new(20, 6);
        for i in 0..3 {
            emit_zone(&mut terminal, i);
        }
        let per_zone = "out\nout\nout".len();
        assert_eq!(terminal.captured_output_bytes, 3 * per_zone);
        let ids: Vec<u64> = terminal.command_zones.iter().map(|zone| zone.id).collect();

        // Force a budget that fits one snapshot: the two OLDEST lose theirs
        // (entries retained), the newest keeps its own.
        terminal.enforce_captured_output_budget(per_zone);
        assert_eq!(terminal.command_zones.len(), 3);
        assert!(terminal.command_zones[0].captured_output.is_none());
        assert!(terminal.command_zones[1].captured_output.is_none());
        assert!(terminal.command_zones[2].captured_output.is_some());
        assert!(terminal.command_zones[0].captured_output_evicted);
        assert!(terminal.command_zones[1].captured_output_evicted);
        assert!(!terminal.command_zones[2].captured_output_evicted);
        assert_eq!(terminal.captured_output_bytes, per_zone);

        // A zone that lost its snapshot but still has rows in scrollback
        // falls back to live extraction.
        assert_eq!(
            terminal.zone_output_text(ids[0]).as_deref(),
            Some("out\nout\nout")
        );
        assert_eq!(
            terminal.zone_output_export_capped(ids[0]),
            Some(ZoneOutputExport::Available {
                text: "out\nout\nout".to_string(),
                truncated: false,
            })
        );

        // The newest zone's snapshot survives even an impossible budget (a
        // fresh snapshot always fits the real 8 MiB budget: one snapshot is
        // capped at 1 MiB).
        terminal.enforce_captured_output_budget(0);
        assert!(terminal.command_zones[2].captured_output.is_some());
    }

    #[test]
    fn export_output_distinguishes_empty_from_evicted_and_unavailable() {
        let mut terminal = TerminalState::new(20, 4);
        terminal.set_max_scrollback(6);

        emit_zone(&mut terminal, 0);
        let output_id = terminal.command_zones[0].id;

        // A full C/D lifecycle with no bytes between the marks is genuinely
        // empty: there was never a non-blank snapshot to lose.
        terminal.process_input(b"\x1b]133;A\x07\x1b]133;B\x07");
        terminal.process_input(b"$ true\r\n\x1b]133;C\x07\x1b]133;D;0\x07");
        let empty_id = terminal.command_zones[1].id;
        assert_eq!(terminal.command_zones[1].captured_output, None);
        assert!(!terminal.command_zones[1].captured_output_evicted);
        assert_eq!(
            terminal.zone_output_export_capped(empty_id),
            Some(ZoneOutputExport::Empty)
        );

        // Row trimming alone does not claim snapshot eviction, and the
        // retained snapshot remains available after all live rows vanish.
        for _ in 0..30 {
            terminal.process_input(b"fill\r\n");
        }
        assert!(terminal.command_zones[0].rows_evicted);
        assert!(!terminal.command_zones[0].captured_output_evicted);
        assert!(matches!(
            terminal.zone_output_export_capped(output_id),
            Some(ZoneOutputExport::Available { .. })
        ));

        // Only an actual budget eviction flips the bit. With both the
        // snapshot and live rows gone, export can now report Unavailable
        // without confusing it with the truly empty zone above.
        terminal.enforce_captured_output_budget(0);
        assert!(terminal.command_zones[0].captured_output_evicted);
        assert_eq!(
            terminal.zone_output_export_capped(output_id),
            Some(ZoneOutputExport::Unavailable)
        );
        assert_eq!(
            terminal.zone_output_export_capped(empty_id),
            Some(ZoneOutputExport::Empty)
        );
        assert_eq!(terminal.zone_output_export_capped(u64::MAX), None);
    }

    #[test]
    fn zone_cap_eviction_releases_snapshot_bytes() {
        let mut terminal = TerminalState::new(20, 4);
        let per_zone = "out\nout\nout".len();
        for i in 0..257 {
            emit_zone(&mut terminal, i);
        }
        // The 256-entry deque cap evicted the oldest zone WITH its snapshot
        // bytes — the budget counter must not leak.
        assert_eq!(terminal.command_zones.len(), 256);
        let with_snapshot = terminal
            .command_zones
            .iter()
            .filter(|zone| zone.captured_output.is_some())
            .count();
        assert_eq!(terminal.captured_output_bytes, with_snapshot * per_zone);
    }

    #[test]
    fn a_new_prompt_finalizes_a_running_zone_whose_d_never_arrived() {
        let mut terminal = TerminalState::new(40, 8);
        terminal.process_input(b"\x1b]133;A\x07$ \x1b]133;B\x07make\r\n");
        terminal.process_input(b"\x1b]133;C\x07building\r\n");
        assert!(terminal.is_command_running());

        // `D` is lost (killed shell integration, alt-screen exit): the next
        // prompt closes the zone as honestly Unknown instead of leaving the
        // accent "running" stripe pinned forever.
        terminal.process_input(b"\x1b]133;A\x07$ ");
        assert!(!terminal.is_command_running());
        assert_eq!(terminal.command_zones.len(), 1);
        let zone = &terminal.command_zones[0];
        assert_eq!(zone.command.as_deref(), Some("make"));
        assert_eq!(zone.exit_code, None);
        // Nothing is invented: no fake duration, no fake finish instant.
        assert_eq!(zone.duration_ms, None);
        assert_eq!(zone.finished_at_ms, None);
        assert_eq!(
            zone.completion_provenance,
            crate::block_mode::CompletionProvenance::BoundaryInferred
        );
        // The Unknown badge (`?`) is exactly what an unreported exit shows.
        assert_eq!(
            crate::block_mode::classify(zone.command.as_deref(), zone.exit_code),
            crate::block_mode::BlockOutcome::Unknown
        );
        // Its output up to the new prompt row was still snapshotted.
        assert_eq!(zone.captured_output, Some(("building".to_string(), false)));
        // A tagged event releases a strictly correlated Agent wait, while
        // persistence and notifications filter it as non-shell evidence.
        let inferred = terminal.take_completed_commands();
        assert_eq!(inferred.len(), 1);
        assert_eq!(inferred[0].exit_code, None);
        assert_eq!(
            inferred[0].completion_provenance,
            crate::block_mode::CompletionProvenance::BoundaryInferred
        );

        // The new lifecycle records cleanly after the forced close.
        terminal.process_input(b"\x1b]133;B\x07echo hi\r\n");
        terminal.process_input(b"\x1b]133;C\x07hi\r\n\x1b]133;D;0\x07");
        assert_eq!(terminal.command_zones.len(), 2);
        assert_eq!(
            terminal.command_zones[1].command.as_deref(),
            Some("echo hi")
        );
        assert_eq!(terminal.command_zones[1].exit_code, Some(0));
        assert_eq!(terminal.take_completed_commands().len(), 1);
    }

    #[test]
    fn a_new_prompt_discards_a_half_typed_prompt_without_a_zone() {
        // `B` seen but no `C`: the command never ran — `CommandStarted` is
        // the RESTING state of every idle prompt (B fires at prompt-end),
        // so it must be silently discarded, never finalized into a zone.
        let mut terminal = TerminalState::new(40, 8);
        terminal.process_input(b"\x1b]133;A\x07$ \x1b]133;B\x07");
        terminal.note_user_input(b"half-typed");
        terminal.process_input(b"half-typed");
        terminal.process_input(b"\r\n\x1b]133;A\x07$ ");
        assert!(terminal.command_zones.is_empty());
        assert!(terminal.take_completed_commands().is_empty());
        // A bare abandoned prompt (`A` with no `B`) spawns nothing either.
        terminal.process_input(b"\x1b]133;A\x07$ ");
        assert!(terminal.command_zones.is_empty());
    }

    #[test]
    fn clean_prompt_async_output_becomes_one_background_zone_at_next_a() {
        let mut terminal = TerminalState::new(40, 8);
        terminal.process_input(b"\x1b]133;A\x07$ \x1b]133;B\x07notice");
        // A D without C is ignored and must neither consume the pending text
        // nor lend its status to the eventual background block.
        terminal.process_input(b"\x1b]133;D;17\x07\x1b]133;A\x07$ \x1b]133;B\x07");

        assert_eq!(terminal.command_zones.len(), 1);
        let zone = &terminal.command_zones[0];
        assert_eq!(zone.command, None);
        assert_eq!(zone.exit_code, None);
        assert_eq!(zone.duration_ms, None);
        assert_eq!(
            zone.completion_provenance,
            crate::block_mode::CompletionProvenance::BoundaryInferred
        );
        assert_eq!(zone.output_start_col, 2);
        assert_eq!(
            terminal.zone_output_text(zone.id).as_deref(),
            Some("notice")
        );
        let range = terminal
            .finished_output_range(zone.id)
            .expect("background keeps exact live provenance");
        assert_eq!(range.start.col, 2);
        assert_eq!(range.end.col, 8);
        assert!(terminal.take_completed_commands().is_empty());
    }

    #[test]
    fn background_snapshot_preserves_real_indentation_and_trailing_line() {
        let mut terminal = TerminalState::new(40, 8);
        terminal.process_input(b"\x1b]133;A\x07$ \x1b]133;B\x07  indented\r\n");
        terminal.process_input(b"\x1b]133;A\x07");

        let zone = terminal.command_zones.back().expect("background block");
        assert_eq!(
            zone.captured_output.as_ref().map(|entry| entry.0.as_str()),
            Some("  indented\n")
        );
        assert_eq!(
            terminal.zone_output_text(zone.id).as_deref(),
            Some("  indented\n")
        );
    }

    #[test]
    fn background_provenance_stops_at_last_recorded_cell_before_later_a() {
        let mut terminal = TerminalState::new(20, 6);
        terminal.process_input(b"\x1b]133;A\x07$ \x1b]133;B\x07notice\r\n");
        terminal.process_input(b"\r\n\x1b]133;A\x07");
        let zone = terminal.command_zones.back().expect("background block");
        let range = terminal
            .finished_output_range(zone.id)
            .expect("exact background range");
        assert_eq!(range.start.col, 2);
        assert_eq!(range.end.col, 8);
        assert_eq!(range.start.row, range.end.row);

        let mut tainted = TerminalState::new(20, 6);
        tainted.process_input(b"\x1b]133;A\x07$ \x1b]133;B\x07notice");
        tainted.note_user_input(b"typed");
        tainted.process_input(b"typed\r\n\x1b]133;A\x07");
        let zone = tainted
            .command_zones
            .back()
            .expect("frozen background block");
        let range = tainted
            .finished_output_range(zone.id)
            .expect("later echo cannot extend background provenance");
        assert_eq!(range.start.col, 2);
        assert_eq!(range.end.col, 8);
    }

    #[test]
    fn background_capture_tracks_carriage_return_before_its_first_column() {
        let mut terminal = TerminalState::new(20, 4);
        terminal.process_input(b"\x1b]133;A\x07$ \x1b]133;B\x07foo\rbar");
        terminal.process_input(b"\x1b]133;A\x07");

        let zone = &terminal.command_zones[0];
        assert_eq!(zone.output_start_col, 0);
        assert_eq!(terminal.zone_output_text(zone.id).as_deref(), Some("bar"));
    }

    #[test]
    fn evicted_background_snapshot_never_falls_back_to_stale_grid_cells() {
        let mut terminal = TerminalState::new(40, 8);
        terminal.process_input(b"\x1b]133;A\x07$ \x1b]133;B\x07foo\rbar");
        terminal.process_input(
            b"\x1b]133;A\x07$ \x1b]133;B\x07echo ok\r\n\x1b]133;C\x07ok\r\n\x1b]133;D;0\x07",
        );

        let background_id = terminal.command_zones[0].id;
        assert_eq!(
            terminal.zone_output_text(background_id).as_deref(),
            Some("bar")
        );
        assert!(!terminal.command_zones[0].rows_evicted);

        // The newest command snapshot is exempt, so a zero test budget
        // evicts the older background snapshot while its grid rows remain.
        terminal.enforce_captured_output_budget(0);
        assert!(terminal.command_zones[0].captured_output_evicted);
        assert_eq!(terminal.zone_output_text_capped(background_id), None);
        assert_eq!(
            terminal.zone_output_export_capped(background_id),
            Some(ZoneOutputExport::Unavailable)
        );
    }

    #[test]
    fn local_input_freezes_prior_async_output_and_excludes_later_echo() {
        let mut terminal = TerminalState::new(40, 8);
        terminal.process_input(b"\x1b]133;A\x07$ \x1b]133;B\x07\r\nasync one\r\n");
        terminal.note_user_input(b"typed");
        terminal.process_input(b"typed\r\ncompletion text\r\n\x1b]133;A\x07");

        assert_eq!(terminal.command_zones.len(), 1);
        let zone = &terminal.command_zones[0];
        assert_eq!(
            terminal.zone_output_text(zone.id).as_deref(),
            Some("\nasync one\n")
        );

        // If the editor was already dirty before output arrived, none of it
        // is split away from the live prompt.
        terminal.process_input(b"$ \x1b]133;B\x07");
        terminal.note_user_input(b"x");
        terminal.process_input(b"x\r\nnot background\r\n\x1b]133;A\x07");
        assert_eq!(terminal.command_zones.len(), 1);
    }

    #[test]
    fn blank_control_redraw_and_alt_screen_output_mint_no_background_zone() {
        let mut terminal = TerminalState::new(30, 6);
        terminal.process_input(
            b"\x1b]133;A\x07$ \x1b]133;B\x07\r\n   \r\n\x1b[0m\x1b[2J\x1b[H\x1b]133;A\x07$ \x1b]133;B\x07",
        );
        assert!(terminal.command_zones.is_empty());

        terminal.process_input(b"\x1b[?1049hvisible only in alt\x1b[?1049l\x1b]133;A\x07");
        assert!(terminal.command_zones.is_empty());

        terminal.process_input(b"$ \x1b]133;B\x07\x1bPsecret\x07LEAK\x1b\\\x1b]133;A\x07");
        assert!(terminal.command_zones.is_empty());
    }

    #[test]
    fn erased_or_invalid_idle_bytes_do_not_invent_visible_background_text() {
        let mut terminal = TerminalState::new(30, 6);
        terminal.process_input(b"\x1b]133;A\x07$ \x1b]133;B\x07x\r\x1b[1K\x1b]133;A\x07");
        assert!(terminal.command_zones.is_empty());

        terminal.process_input(b"$ \x1b]133;B\x07ok");
        terminal.process_input(b"\xff");
        terminal.process_input(b"\x1b]133;A\x07");
        let zone = terminal.command_zones.back().expect("visible ok block");
        assert_eq!(
            zone.captured_output.as_ref().map(|entry| entry.0.as_str()),
            Some("ok")
        );
        assert!(zone.captured_output.as_ref().is_some_and(|entry| entry.1));

        terminal.process_input(b"$ \x1b]133;B\x07ok\x1b\xff[2Jevil\x1b]133;A\x07");
        let zone = terminal.command_zones.back().expect("literal suffix block");
        assert_eq!(
            zone.captured_output.as_ref().map(|entry| entry.0.as_str()),
            Some("ok[2Jevil")
        );
        assert!(zone.captured_output.as_ref().is_some_and(|entry| entry.1));

        terminal.process_input(b"$ \x1b]133;B\x07ok\xe2\x80\xae\x1b]133;A\x07");
        let zone = terminal.command_zones.back().expect("visible safe block");
        assert_eq!(
            zone.captured_output.as_ref().map(|entry| entry.0.as_str()),
            Some("ok")
        );
        let zone_count = terminal.command_zones.len();
        terminal.process_input(b"$ \x1b]133;B\x07\xe2\x80\xae\x1b]133;A\x07");
        assert_eq!(terminal.command_zones.len(), zone_count);
    }

    #[test]
    fn background_output_survives_scrollback_trim_as_a_bounded_snapshot() {
        let mut terminal = TerminalState::new(12, 2);
        terminal.set_max_scrollback(3);
        terminal.process_input(b"\x1b]133;A\x07$ \x1b]133;B\x07");
        for index in 0..12 {
            terminal.process_input(format!("\r\nline-{index:02}").as_bytes());
        }
        terminal.process_input(b"\r\n\x1b]133;A\x07");

        let zone = &terminal.command_zones[0];
        let (output, truncated) = terminal.zone_output_text_capped(zone.id).unwrap();
        assert!(!truncated, "raw capture is independent from scrollback");
        assert!(zone.rows_evicted);
        assert_eq!(terminal.finished_output_range(zone.id), None);
        assert!(!terminal.finished_output_provenance.contains_key(&zone.id));
        assert!(output.contains("line-00"), "retained head: {output:?}");
        assert!(output.contains("line-11"), "retained tail: {output:?}");
        assert!(output.len() <= TerminalState::ZONE_OUTPUT_CAP_BYTES);
    }

    #[test]
    fn control_only_scrolling_cannot_erase_pending_background_output() {
        let mut terminal = TerminalState::new(12, 2);
        terminal.set_max_scrollback(2);
        terminal.process_input(b"\x1b]133;A\x07$ \x1b]133;B\x07visible");
        for _ in 0..12 {
            terminal.process_input(b"\r\n");
        }
        terminal.process_input(b"\x1b]133;A\x07");

        let zone = &terminal.command_zones[0];
        assert!(zone.rows_evicted);
        let expected = format!("visible{}", "\n".repeat(12));
        assert_eq!(
            terminal.zone_output_text(zone.id).as_deref(),
            Some(expected.as_str())
        );
    }

    #[test]
    fn background_snapshot_survives_resize_before_prompt_boundary() {
        let mut terminal = TerminalState::new(24, 4);
        terminal.process_input(b"\x1b]133;A\x07$ \x1b]133;B\x07\r\nresize-safe output");
        terminal.on_resize(12, 4);
        assert!(terminal.normalize_scrollback_width());
        terminal.process_input(b"\r\nafter resize");
        terminal.process_input(b"\r\n\x1b]133;A\x07");

        let zone = &terminal.command_zones[0];
        let output = terminal.zone_output_text(zone.id).unwrap();
        assert!(
            output.contains("resize-safe output"),
            "resized background output: {output:?}"
        );
        assert!(
            output.contains("after resize"),
            "continued output: {output:?}"
        );
    }

    #[test]
    fn idle_background_raw_ring_retains_only_the_newest_eight_mib() {
        let limit = TerminalState::IDLE_BACKGROUND_CAPTURE_BYTES;
        let mut pending = IdleBackgroundOutput::new(0, 0);
        pending.append(&vec![b'a'; limit], limit);
        pending.append(b"tail", limit);

        let raw = pending.raw_bytes();
        assert_eq!(pending.raw_len, limit);
        assert_eq!(raw.len(), limit);
        assert!(pending.raw_truncated);
        assert_eq!(&raw[limit - 4..], b"tail");
        assert!(raw[..limit - 4].iter().all(|byte| *byte == b'a'));
    }

    #[test]
    fn idle_background_ring_excludes_private_control_string_payloads() {
        let mut pending = IdleBackgroundOutput::new(0, 0);
        pending.append(b"visible", 32);
        pending.append(b"\x1b_Gsecret-base64\x1b\\", 32);
        pending.append(b" tail", 32);

        assert_eq!(pending.raw_bytes(), b"visible tail");
        assert!(!pending.raw_truncated);
    }

    #[test]
    fn idle_background_ring_bounds_chunk_metadata_for_mixed_tokens() {
        let limit = 64 * 1024;
        let mut pending = IdleBackgroundOutput::new(0, 0);
        for _ in 0..limit / 2 {
            pending.append(b"a", limit);
            pending.append(b"\r", limit);
        }

        assert_eq!(pending.raw_len, limit);
        assert!(pending.raw_chunks.len() <= limit.div_ceil(IdleBackgroundOutput::CHUNK_BYTES));
        assert_eq!(pending.raw_bytes().len(), limit);
    }

    #[test]
    fn typeahead_before_the_next_b_keeps_that_prompt_dirty() {
        let mut terminal = TerminalState::new(40, 6);
        terminal.process_input(b"\x1b]133;A\x07$ \x1b]133;B\x07cmd\r\n\x1b]133;C\x07running\r\n");
        terminal.note_user_input(b"queued");
        terminal.process_input(b"\x1b]133;D;0\x07\x1b]133;A\x07$ \x1b]133;B\x07");
        assert_eq!(
            terminal.agent_prompt_status(),
            AgentPromptStatus::InputNotEmpty
        );

        terminal.process_input(b"queued\r\nlate echo\r\n\x1b]133;A\x07");
        assert_eq!(
            terminal.command_zones.len(),
            1,
            "only the real command block"
        );
    }

    #[test]
    fn same_write_submit_tail_taints_next_prompt_but_plain_enter_does_not() {
        let mut terminal = TerminalState::new(40, 6);
        terminal.process_input(b"\x1b]133;A\x07$ \x1b]133;B\x07");
        terminal.note_user_input(b"cmd\rqueued");
        terminal.process_input(b"cmd\rqueued\r\n\x1b]133;C\x07out\r\n\x1b]133;D;0\x07");
        terminal.process_input(b"\x1b]133;A\x07$ \x1b]133;B\x07");
        assert_eq!(
            terminal.agent_prompt_status(),
            AgentPromptStatus::InputNotEmpty
        );
        terminal.process_input(b"queued\r\n\x1b]133;A\x07");
        assert_eq!(terminal.command_zones.len(), 1);

        let mut plain_enter = TerminalState::new(40, 6);
        plain_enter.process_input(b"\x1b]133;A\x07$ \x1b]133;B\x07");
        plain_enter.note_user_input(b"cmd\r");
        plain_enter.process_input(b"cmd\r\n\x1b]133;C\x07out\r\n\x1b]133;D;0\x07");
        plain_enter.process_input(b"\x1b]133;A\x07$ \x1b]133;B\x07");
        assert_eq!(plain_enter.agent_prompt_status(), AgentPromptStatus::Ready);
    }

    #[test]
    fn prompt_redraw_carries_dirty_edit_but_ctrl_c_starts_clean() {
        let mut redraw = TerminalState::new(40, 6);
        redraw.process_input(b"\x1b]133;A\x07$ \x1b]133;B\x07");
        redraw.note_user_input(b"x");
        redraw.process_input(b"x");
        redraw.process_input(b"\x1b[2J\x1b[H\x1b]133;A\x07$ \x1b]133;B\x07x");
        assert_eq!(
            redraw.agent_prompt_status(),
            AgentPromptStatus::InputNotEmpty
        );
        redraw.process_input(b"\x1b]133;A\x07");
        assert!(redraw.command_zones.is_empty());

        let mut cancelled = TerminalState::new(40, 6);
        cancelled.process_input(b"\x1b]133;A\x07$ \x1b]133;B\x07");
        cancelled.note_user_input(b"x");
        cancelled.process_input(b"x");
        cancelled.note_user_input(b"\x03");
        cancelled.process_input(b"^C\r\n\x1b]133;A\x07$ \x1b]133;B\x07");
        assert_eq!(cancelled.agent_prompt_status(), AgentPromptStatus::Ready);
        assert!(cancelled.command_zones.is_empty());
    }

    #[test]
    fn one_background_snapshot_never_exceeds_the_one_mib_cap() {
        let mut terminal = TerminalState::new(1024, 4);
        terminal.process_input(b"\x1b]133;A\x07$ \x1b]133;B\x07");
        let output = vec![b'x'; TerminalState::ZONE_OUTPUT_CAP_BYTES + 2048];
        terminal.process_input(&output);
        terminal.process_input(b"\x1b]133;A\x07");

        let zone = &terminal.command_zones[0];
        let (captured, truncated) = zone.captured_output.as_ref().unwrap();
        assert!(truncated);
        assert!(captured.len() <= TerminalState::ZONE_OUTPUT_CAP_BYTES);
        assert_eq!(terminal.captured_output_bytes, captured.len());
    }

    #[test]
    fn idle_prompt_redraws_mint_no_zones() {
        // readline's ctrl+l (or fish re-running fish_prompt on SIGWINCH)
        // re-emits the prompt's embedded `A`/`B` marks on every repaint —
        // often after a \x1b[2J\x1b[H clear, so the marks land on rows at or
        // BEFORE earlier ones. None of these repaints ran a command; none
        // may mint a zone (empirically pre-fix: 5 redraws → 5 junk zones,
        // with non-monotonic prompt_start rows breaking spans' sorted
        // assumption).
        let mut terminal = TerminalState::new(40, 8);
        terminal.process_input(b"\x1b]133;A\x07$ \x1b]133;B\x07");
        for _ in 0..5 {
            terminal.process_input(b"\x1b[2J\x1b[H\x1b]133;A\x07$ \x1b]133;B\x07");
        }
        assert!(terminal.command_zones.is_empty());
        assert!(terminal.take_completed_commands().is_empty());
        assert!(!terminal.is_command_running());
        // The prompt is still live for the next real command.
        terminal.process_input(b"echo hi\r\n\x1b]133;C\x07hi\r\n\x1b]133;D;0\x07");
        assert_eq!(terminal.command_zones.len(), 1);
        assert_eq!(
            terminal.command_zones[0].command.as_deref(),
            Some("echo hi")
        );
        assert_eq!(terminal.command_zones[0].exit_code, Some(0));
    }

    #[test]
    fn set_max_scrollback_shrink_realigns_command_zones() {
        let mut terminal = TerminalState::new(20, 4);
        for _ in 0..12 {
            terminal.process_input(b"fill\r\n");
        }
        emit_zone(&mut terminal, 7);
        assert_eq!(terminal.command_zones.len(), 1);

        terminal.set_max_scrollback(6);

        assert_eq!(terminal.command_zones.len(), 1);
        let row = terminal.command_zones[0].prompt_start;
        assert!(
            buffer_row_text(&terminal, row).starts_with("$ cmd7"),
            "zone anchors {:?}",
            buffer_row_text(&terminal, row)
        );
    }

    #[test]
    fn last_command_output_text_returns_trimmed_output_lines() {
        let mut terminal = TerminalState::new(20, 6);
        assert_eq!(terminal.last_command_output_text(), None);

        emit_zone(&mut terminal, 0);
        assert_eq!(
            terminal.last_command_output_text().as_deref(),
            Some("out\nout\nout")
        );

        // A newer command's output supersedes the previous zone.
        terminal.process_input(b"\x1b]133;A\x07\x1b]133;B\x07");
        terminal.process_input(b"$ true\r\n");
        terminal.process_input(b"\x1b]133;C\x07final line\r\n\x1b]133;D;0\x07");
        assert_eq!(
            terminal.last_command_output_text().as_deref(),
            Some("final line")
        );
    }

    #[test]
    fn last_command_output_survives_scrollback_trim_via_snapshot() {
        // DELIBERATE v3 semantic change: v2 pinned that a trimmed zone's
        // output disappears; the snapshot taken at `D` now keeps answering.
        let mut terminal = TerminalState::new(20, 4);
        terminal.set_max_scrollback(6);
        emit_zone(&mut terminal, 0);
        for _ in 0..30 {
            terminal.process_input(b"fill\r\n");
        }
        assert!(terminal.command_zones[0].rows_evicted);
        assert_eq!(
            terminal.last_command_output_text().as_deref(),
            Some("out\nout\nout")
        );
        // Once the budget also evicts the snapshot, nothing is left to read.
        terminal.command_zones[0].captured_output = None;
        assert_eq!(terminal.last_command_output_text(), None);
    }

    #[test]
    fn prompt_jumps_walk_history_and_return_to_live_view() {
        let mut terminal = TerminalState::new(20, 4);
        for i in 0..6 {
            emit_zone(&mut terminal, i);
        }
        let history = terminal.scrollback_len();
        let prompts: Vec<usize> = terminal
            .command_zones
            .iter()
            .map(|zone| zone.prompt_start)
            .collect();
        assert!(prompts.len() >= 4);

        // At the live view, "next prompt" has nowhere to go.
        assert!(!terminal.jump_to_next_prompt());

        // Walking up visits each prompt in scrollback, newest first, landing
        // the prompt row exactly at the top of the viewport.
        let mut visited = Vec::new();
        while terminal.jump_to_prev_prompt() {
            visited.push(terminal.viewport_absolute_start());
        }
        let mut expected: Vec<usize> = prompts.iter().copied().filter(|&r| r < history).collect();
        expected.reverse();
        assert_eq!(visited, expected);

        // Walking down visits them in order and ends back at the live view.
        let mut down = Vec::new();
        while terminal.jump_to_next_prompt() {
            down.push(terminal.viewport_absolute_start());
        }
        let mut expected_down: Vec<usize> = expected.iter().rev().skip(1).copied().collect();
        expected_down.push(history);
        assert_eq!(down, expected_down);
        assert_eq!(terminal.scroll_offset, 0);
    }

    // --- click-to-place-cursor --------------------------------------------
    //
    // The arithmetic lives in `jterm_core::click_cursor`; what these pin is the
    // terminal's half of the contract — which cells count as the editable span,
    // and which states refuse the click outright.

    /// A prompt with `cmd` typed at it and the cursor left at the end.
    fn terminal_at_prompt(cols: usize, rows: usize, cmd: &str) -> super::TerminalState {
        let mut terminal = super::TerminalState::new(cols, rows);
        terminal.process_input(b"\x1b]133;A\x1b\\$ \x1b]133;B\x1b\\");
        terminal.process_input(cmd.as_bytes());
        terminal
    }

    #[test]
    fn a_click_left_of_the_cursor_walks_back_to_it() {
        let terminal = terminal_at_prompt(32, 4, "echo hello");
        assert_eq!((terminal.cursor_row, terminal.cursor_col), (0, 12));
        assert_eq!(
            terminal.click_cursor_move(0, 7, true),
            b"\x1b[D".repeat(5),
            "five characters back from the end of `hello`"
        );
    }

    #[test]
    fn a_click_past_the_command_stops_at_its_end() {
        // The dangerous direction: in jsh a `Right` at end-of-buffer accepts
        // the inline suggestion, so clicking the empty space after a command
        // must not spend a single extra arrow.
        let mut terminal = terminal_at_prompt(32, 4, "echo hi");
        assert!(terminal.click_cursor_move(0, 30, true).is_empty());

        terminal.process_input(b"\x1b[D\x1b[D\x1b[D");
        assert_eq!((terminal.cursor_row, terminal.cursor_col), (0, 6));
        assert_eq!(terminal.click_cursor_move(0, 30, true), b"\x1b[C".repeat(3));
    }

    /// A prompt with `typed` at it and `ghost` painted past the cursor the way
    /// a fish-style shell previews a completion.
    ///
    /// The byte shape is jsh's own, captured from the running shell: the
    /// suggestion in ANSI colour 8, then the cursor parked back at the end of
    /// the typed text with CHA.
    fn terminal_with_suggestion(
        cols: usize,
        rows: usize,
        typed: &str,
        ghost: &str,
    ) -> super::TerminalState {
        let mut terminal = terminal_at_prompt(cols, rows, typed);
        let col = terminal.cursor_col;
        terminal.process_input(format!("\x1b[38;5;8m{ghost}\x1b[0m\x1b[{}G", col + 1).as_bytes());
        terminal
    }

    #[test]
    fn an_inline_suggestion_is_not_part_of_the_input() {
        // The whole reason the span has to end where the *buffer* ends: those
        // grey cells are a preview, and every `Right` spent on them is jsh
        // accepting a command the user never typed.
        let terminal = terminal_with_suggestion(32, 4, "echo he", "llo world");
        assert_eq!((terminal.cursor_row, terminal.cursor_col), (0, 9));

        assert!(
            terminal.click_cursor_move(0, 30, true).is_empty(),
            "clicking the empty space past the suggestion must not accept it"
        );
        assert!(
            terminal.click_cursor_move(0, 12, true).is_empty(),
            "nor may clicking the suggestion itself, which is not a place to edit"
        );
        assert_eq!(
            terminal.click_cursor_move(0, 5, true),
            b"\x1b[D".repeat(4),
            "moving back into what was really typed still works"
        );
    }

    /// A prompt with `typed` at it, a right-aligned decoration painted flush
    /// with the terminal's right edge (the way jsh and fish show the previous
    /// command's duration), and the cursor back at the end of the typed text.
    fn terminal_with_rprompt(
        cols: usize,
        rows: usize,
        typed: &str,
        rprompt: &str,
    ) -> super::TerminalState {
        let mut terminal = terminal_at_prompt(cols, rows, typed);
        let col = terminal.cursor_col;
        terminal.process_input(
            format!(
                "\x1b[{}G\x1b[33m{rprompt}\x1b[0m\x1b[{}G",
                cols - rprompt.chars().count() + 1,
                col + 1
            )
            .as_bytes(),
        );
        terminal
    }

    #[test]
    fn a_right_aligned_duration_is_not_part_of_the_input() {
        // jsh keeps its last suggestion even while the cursor sits mid-buffer
        // — it just stops drawing it. Arrows sent past the buffer end would
        // accept that invisible text, so the span must stop at the typed
        // command, not at the duration display parked against the right edge.
        let mut terminal = terminal_with_rprompt(32, 4, "echo hello", "2.3s");
        terminal.process_input(b"\x1b[D\x1b[D\x1b[D\x1b[D\x1b[D");
        assert_eq!((terminal.cursor_row, terminal.cursor_col), (0, 7));

        assert_eq!(
            terminal.click_cursor_move(0, 30, true),
            b"\x1b[C".repeat(5),
            "a click on the duration walks to the end of the command and stops"
        );
        assert_eq!(
            terminal.click_cursor_move(0, 20, true),
            b"\x1b[C".repeat(5),
            "so does a click in the gap before it"
        );
    }

    #[test]
    fn an_interior_gap_away_from_the_edge_stays_reachable() {
        // The decoration rule must not eat genuine input: a wide run of
        // spaces inside a command whose tail stops short of the right edge is
        // buffer.
        let mut terminal = terminal_at_prompt(40, 4, "echo 'a          b'");
        terminal.process_input(b"\x1b[D".repeat(15).as_slice());
        assert_eq!((terminal.cursor_row, terminal.cursor_col), (0, 6));
        assert_eq!(
            terminal.click_cursor_move(0, 38, true),
            b"\x1b[C".repeat(15),
            "clicking past the command still reaches its real end"
        );
    }

    #[test]
    fn ordinary_text_past_the_cursor_is_still_reachable() {
        // The mirror image: text right of the cursor that is *not* suggestion-
        // styled belongs to the buffer, so a click must still travel to it.
        let mut terminal = terminal_at_prompt(32, 4, "echo hello");
        terminal.process_input(b"\x1b[D\x1b[D\x1b[D\x1b[D\x1b[D");
        assert_eq!((terminal.cursor_row, terminal.cursor_col), (0, 7));
        assert_eq!(terminal.click_cursor_move(0, 30, true), b"\x1b[C".repeat(5));
    }

    #[test]
    fn a_click_on_the_prompt_goes_to_the_start_of_the_line() {
        let terminal = terminal_at_prompt(32, 4, "ls");
        assert_eq!(terminal.click_cursor_move(0, 0, true), b"\x1b[D".repeat(4));
    }

    #[test]
    fn a_click_in_a_completed_block_preserves_the_live_cursor() {
        let mut terminal = super::TerminalState::new(32, 4);
        terminal.process_input(b"completed output\r\n");
        terminal.process_input(b"\x1b]133;A\x1b\\$ \x1b]133;B\x1b\\echo hello");
        assert_eq!((terminal.cursor_row, terminal.cursor_col), (1, 12));
        assert!(
            terminal.click_cursor_move(0, 7, true).is_empty(),
            "history interaction must not synthesize Left/Home for the live editor"
        );
        assert_eq!(
            terminal.click_cursor_move(1, 7, true),
            b"\x1b[D".repeat(5),
            "click-to-place remains active on the current input row"
        );
    }

    #[test]
    fn a_click_follows_a_soft_wrap_onto_the_previous_row() {
        // 10 columns: "$ " plus 12 characters wraps onto a second row.
        let terminal = terminal_at_prompt(10, 4, "abcdefghijkl");
        assert_eq!((terminal.cursor_row, terminal.cursor_col), (1, 4));
        assert_eq!(terminal.click_cursor_move(0, 4, true), b"\x1b[D".repeat(10));
    }

    #[test]
    fn wide_characters_cost_one_arrow_each() {
        let terminal = terminal_at_prompt(32, 4, "echo 你好世界");
        assert_eq!((terminal.cursor_row, terminal.cursor_col), (0, 15));
        assert_eq!(
            terminal.click_cursor_move(0, 7, true),
            b"\x1b[D".repeat(4),
            "four characters, not the eight cells they cover"
        );
    }

    #[test]
    fn a_disabled_config_sends_nothing() {
        let terminal = terminal_at_prompt(32, 4, "echo hello");
        assert!(terminal.click_cursor_move(0, 7, false).is_empty());
    }

    #[test]
    fn a_running_command_keeps_the_click() {
        let mut terminal = terminal_at_prompt(32, 4, "less big.log");
        terminal.process_input(b"\x1b]133;C\x1b\\");
        assert!(terminal.click_cursor_move(0, 7, true).is_empty());
    }

    #[test]
    fn mouse_reporting_and_the_alternate_screen_keep_the_click() {
        let mut terminal = terminal_at_prompt(32, 4, "echo hello");
        terminal.process_input(b"\x1b[?1000h");
        assert!(terminal.click_cursor_move(0, 7, true).is_empty());
        terminal.process_input(b"\x1b[?1000l");
        assert!(!terminal.click_cursor_move(0, 7, true).is_empty());

        terminal.process_input(b"\x1b[?1049h");
        assert!(terminal.click_cursor_move(0, 7, true).is_empty());
    }

    #[test]
    fn scrolled_back_clicks_are_history_not_input() {
        let mut terminal = terminal_at_prompt(20, 3, "echo hello");
        terminal.process_input(b"\r\nfiller\r\nfiller\r\nfiller\r\n");
        terminal.scroll_offset = 1;
        assert!(
            terminal.click_cursor_move(0, 3, true).is_empty(),
            "viewport rows no longer line up with grid rows"
        );
    }

    #[test]
    fn application_cursor_keys_switch_the_arrow_encoding() {
        let mut terminal = terminal_at_prompt(32, 4, "echo hello");
        terminal.process_input(b"\x1b[?1h");
        assert_eq!(terminal.click_cursor_move(0, 11, true), b"\x1bOD".to_vec());
    }
}
