use crate::kitty_graphics::KittyGraphicsState;
use base64::Engine;
use jterm_core::click_cursor;
use smallvec::SmallVec;
use std::borrow::Cow;
use std::collections::VecDeque;
use unicode_normalization::UnicodeNormalization;

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
pub const MAX_TERMINAL_COLS: usize = 1024;
pub const MAX_TERMINAL_ROWS: usize = 512;
pub type DynamicColorPalette = [Option<(u8, u8, u8)>; 256];

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
}

impl TerminalGrid {
    pub fn new(rows: usize, cols: usize) -> Self {
        TerminalGrid {
            cells: vec![TerminalCell::default(); rows * cols],
            rows,
            cols,
            row_wrapped: vec![false; rows],
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
        self.rows = new_rows;
        self.cols = new_cols;
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
}

#[derive(Clone, Debug)]
enum CompressedLineData {
    Plain(String, u16),
    Encoded(Vec<u8>),
}

impl ScrollbackLine {
    pub fn compress(cells: &[TerminalCell], is_wrapped: bool) -> Self {
        let cols = cells.len() as u16;
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
            })
            .count();

        let active_len = cells.len() - trailing_blanks;
        let all_default_attrs = cells[..active_len].iter().all(|c| {
            c.foreground == Color::Default
                && c.background == Color::Default
                && c.flags.is_default_style()
                && !c.flags.wide()
                && !c.flags.wide_continuation()
        });

        if all_default_attrs {
            let text: String = cells[..active_len].iter().map(|c| c.character).collect();
            ScrollbackLine {
                data: CompressedLineData::Plain(text, trailing_blanks as u16),
                is_wrapped,
                cols,
            }
        } else {
            let encoded = Self::encode_cells(&cells[..active_len]);
            ScrollbackLine {
                data: CompressedLineData::Encoded(encoded),
                is_wrapped,
                cols,
            }
        }
    }

    pub fn decompress(&self) -> Vec<TerminalCell> {
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
                {
                    run += 1;
                } else {
                    break;
                }
            }

            // Format:
            // [char_len:1][char_bytes][fg][bg][style_flags:1][extra_flags:1][run:1]
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

#[derive(Clone, Copy, Debug)]
pub struct TerminalCell {
    pub character: char,
    pub foreground: Color,
    pub background: Color,
    pub flags: StyleFlags,
}

impl Default for TerminalCell {
    fn default() -> Self {
        TerminalCell {
            character: ' ',
            foreground: Color::Default,
            background: Color::Default,
            flags: StyleFlags::new(),
        }
    }
}

const _: () = assert!(std::mem::size_of::<TerminalCell>() == 16);
type VisibleCellsCache = (u64, usize, std::sync::Arc<Vec<Vec<TerminalCell>>>);

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
    /// The shell reported (`cmd_truncated=`) that [`Self::command`] was cut
    /// short. A truncated command line is not safe to re-run, so recall must
    /// refuse it; copying is still fine.
    pub command_truncated: bool,
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
    /// The zone's rows were trimmed out of scrollback. The entry stays (id,
    /// metadata, snapshot — v2 dropped the whole zone here) but all row
    /// fields are meaningless: `prompt_start` is clamped to 0 and the
    /// `Option` rows are `None`. Row consumers (stripes, gutter, markers,
    /// prompt jumps, reveal) must skip such zones.
    pub rows_evicted: bool,
}

#[derive(Clone, Debug, Default)]
enum ZoneState {
    #[default]
    Idle,
    PromptStarted(usize),
    CommandStarted(usize, usize),
    OutputStarted(usize, usize, usize),
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
    pub scroll_offset: usize,
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

    // Incomplete escape sequence buffer across PTY reads
    pending_escape: Vec<u8>,

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

    // OSC 8 hyperlink tracking
    current_hyperlink: Option<(String, Option<String>)>, // (url, id)
    #[allow(dead_code)]
    osc8_hyperlinks: Vec<crate::link::Link>, // Stored hyperlinks from OSC 8

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
    /// Once local input is accepted for this prompt, approval stays blocked
    /// until a fresh `B`. This closes the write-before-echo race.
    agent_prompt_input_tainted: bool,
    /// Monotonic identity of the current OSC 133 prompt.
    agent_prompt_generation: u64,
    /// Reviewed command waiting for the next exact OSC 133 `C` transition.
    armed_agent_execution: Option<ArmedAgentExecution>,
    /// Reviewed command whose exact `C` was accepted and whose `D` is pending.
    active_agent_execution: Option<ActiveAgentExecution>,
    /// Exact command captured at `C`, preferring jsh's `cmdline_url` metadata.
    current_command_text: Option<String>,

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
    /// When the currently executing command began (OSC 133 `C`), so the
    /// finished record can carry a wall-clock duration.
    current_command_started_at: Option<std::time::Instant>,
    /// Working directory reported by an OSC 133 `cwd`/`cwd_url` param during
    /// the current prompt lifecycle (reset at `A`, consumed at `D`).
    current_command_cwd: Option<String>,
    /// The shell flagged the current command line as truncated
    /// (`cmd_truncated=`); carried into the zone at `D`.
    current_command_truncated: bool,
    /// Total bytes of all zones' [`CommandZone::captured_output`] snapshots,
    /// kept under [`Self::MAX_CAPTURED_OUTPUT_BYTES`] by
    /// [`Self::enforce_captured_output_budget`].
    captured_output_bytes: usize,
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
            scroll_offset: 0,
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
            current_hyperlink: None,
            osc8_hyperlinks: Vec::new(),
            sync_output_active: false,
            sync_output_start: None,
            last_archived_screen_snapshot: Vec::new(),
            last_synced_primary_screen_snapshot: Vec::new(),
            pending_osc52_clipboard_set: None,
            pending_osc52_clipboard_query: false,
            command_zones: VecDeque::new(),
            next_zone_id: 0,
            current_zone_state: ZoneState::default(),
            current_command_start_col: None,
            current_command_extent_row: None,
            agent_prompt_input_tainted: false,
            agent_prompt_generation: 0,
            armed_agent_execution: None,
            active_agent_execution: None,
            current_command_text: None,
            dynamic_fg: None,
            dynamic_bg: None,
            dynamic_cursor_color: None,
            dynamic_palette: [None; 256],
            pending_notifications: Vec::new(),
            pending_completed_commands: std::collections::VecDeque::new(),
            current_command_id: None,
            current_command_started_at: None,
            current_command_cwd: None,
            current_command_truncated: false,
            captured_output_bytes: 0,
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

    /// Exact visible command input after OSC 133 `B`, excluding the prompt.
    fn current_prompt_command_text(&self) -> Option<String> {
        const MAX_COMMAND_BYTES: usize = 16 * 1024;
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
        let mut out = String::new();

        for absolute_row in start_row..=last_row {
            let append_cells = |out: &mut String, cells: &[TerminalCell], wrapped: bool| {
                let first_col = if absolute_row == start_row {
                    start_col.min(cells.len())
                } else {
                    0
                };
                let mut segment: String = cells[first_col..]
                    .iter()
                    .filter(|cell| !cell.flags.wide_continuation())
                    .map(|cell| cell.character)
                    .collect();
                if !wrapped {
                    segment.truncate(segment.trim_end_matches(' ').len());
                }
                out.push_str(&segment);
                wrapped
            };

            let wrapped = if absolute_row < self.scrollback.len() {
                let line = &self.scrollback[absolute_row];
                let cells = line.decompress();
                append_cells(&mut out, &cells, line.is_wrapped)
            } else {
                let grid_row = absolute_row - self.scrollback.len();
                append_cells(
                    &mut out,
                    &self.grid[grid_row],
                    self.grid.row_wrapped[grid_row],
                )
            };
            if out.len() >= MAX_COMMAND_BYTES {
                return None;
            }
            if !wrapped && absolute_row < last_row {
                out.push('\n');
            }
        }
        Some(out)
    }

    pub fn agent_prompt_status(&self) -> AgentPromptStatus {
        match self.current_zone_state {
            ZoneState::CommandStarted(_, _) => {
                if self.agent_prompt_input_tainted
                    || self
                        .current_prompt_command_text()
                        .is_none_or(|command| !command.is_empty())
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
        if !input.is_empty() && matches!(self.current_zone_state, ZoneState::CommandStarted(_, _)) {
            self.agent_prompt_input_tainted = true;
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
        Ok(())
    }

    pub fn disarm_agent_execution(&mut self, generation: u64) {
        if self
            .armed_agent_execution
            .as_ref()
            .is_some_and(|armed| armed.generation == generation)
        {
            self.armed_agent_execution = None;
        }
    }

    fn mark_command_echo_extent(&mut self) {
        if matches!(self.current_zone_state, ZoneState::CommandStarted(_, _)) {
            let absolute_row = self.scrollback.len() + self.cursor_row;
            self.current_command_extent_row = Some(
                self.current_command_extent_row
                    .unwrap_or(absolute_row)
                    .max(absolute_row),
            );
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
        let mark = value.chars().next().unwrap_or('\0');
        let mut mark_id = None;
        let mut metadata_command = None;
        let mut metadata_cwd = None;
        let mut metadata_duration_ms = None;
        let mut metadata_truncated = false;
        // jsh correlation metadata rides on C/D as percent-encoded key/value
        // params. Parse it per mark instead of leaving an id in global state
        // where an unrelated later D could inherit it.
        for part in value.split(';').skip(1) {
            if let Some((key, id)) = part.split_once('=') {
                if matches!(key, "id" | "jsh_id" | "execution_id" | "command_id") && !id.is_empty()
                {
                    mark_id = Self::decode_osc_metadata(id, MAX_EXECUTION_ID_BYTES)
                        .filter(|id| !id.is_empty() && !id.chars().any(char::is_control));
                } else if key == "cmdline_url" {
                    metadata_command = Self::decode_osc_metadata(id, MAX_COMMAND_METADATA_BYTES)
                        .filter(|command| !command.chars().any(char::is_control));
                } else if key == "command"
                    && id.len() <= MAX_COMMAND_METADATA_BYTES
                    && !id.chars().any(char::is_control)
                {
                    metadata_command = Some(id.to_string());
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
                self.finalize_stale_zone(absolute_row);
                // Prompt start. Any leftover execution timestamp belongs to a
                // command that never reported `D`; drop it so it cannot leak
                // into a later command's duration.
                self.current_zone_state = ZoneState::PromptStarted(absolute_row);
                self.current_command_started_at = None;
                self.current_command_start_col = None;
                self.current_command_extent_row = None;
                self.current_command_text = None;
                self.current_command_id = mark_id;
                self.current_command_cwd = metadata_cwd;
                self.current_command_truncated = false;
                self.agent_prompt_input_tainted = false;
                self.armed_agent_execution = None;
                self.active_agent_execution = None;
            }
            'B' => {
                // Command start (user is typing the command)
                if let ZoneState::PromptStarted(prompt_start) = self.current_zone_state {
                    self.current_zone_state = ZoneState::CommandStarted(prompt_start, absolute_row);
                    self.current_command_start_col = Some(self.cursor_col);
                    self.current_command_extent_row = Some(absolute_row);
                    self.current_command_text = None;
                    if mark_id.is_some() {
                        self.current_command_id.clone_from(&mark_id);
                    }
                    if metadata_cwd.is_some() {
                        self.current_command_cwd = metadata_cwd;
                    }
                    self.agent_prompt_input_tainted = false;
                    self.agent_prompt_generation = self.agent_prompt_generation.wrapping_add(1);
                    self.armed_agent_execution = None;
                    self.active_agent_execution = None;
                }
            }
            'C' => {
                // Command executed (output begins)
                if let ZoneState::CommandStarted(prompt_start, cmd_start) = self.current_zone_state
                {
                    let captured_command = metadata_command
                        .or_else(|| self.current_prompt_command_text())
                        .unwrap_or_default();
                    self.current_command_text = Some(captured_command.clone());
                    if mark_id.is_some() {
                        self.current_command_id.clone_from(&mark_id);
                    }
                    if metadata_cwd.is_some() {
                        self.current_command_cwd = metadata_cwd;
                    }
                    if metadata_truncated {
                        self.current_command_truncated = true;
                    }

                    let matching_generation = self
                        .armed_agent_execution
                        .as_ref()
                        .filter(|armed| {
                            !self.agent_prompt_input_tainted
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
                let started_id = self.current_command_id.take();
                let finished_id = mark_id.or(started_id);
                // Once an approved command started with a real jsh execution
                // id, only the D carrying that same id may consume it. A fake
                // or stale D is ignored and cannot steal the approval.
                if self.active_agent_execution.as_ref().is_some_and(|active| {
                    active
                        .execution_id
                        .as_ref()
                        .is_some_and(|expected| d_mark_id.as_ref() != Some(expected))
                }) {
                    return;
                }
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
                match self.current_zone_state {
                    ZoneState::OutputStarted(prompt_start, cmd_start, out_start) => {
                        let zone = CommandZone {
                            id: 0, // assigned by push_command_zone
                            prompt_start,
                            command_start: Some(cmd_start),
                            output_start: Some(out_start),
                            output_end: Some(absolute_row),
                            exit_code,
                            command: self.zone_command_text(),
                            duration_ms,
                            finished_at_ms: Self::wall_clock_ms(),
                            command_truncated,
                            cwd: cwd.clone(),
                            captured_output: self.capture_zone_output(out_start, absolute_row),
                            rows_evicted: false,
                        };
                        self.push_command_zone(zone);
                        self.record_completed_command(
                            cmd_start,
                            out_start,
                            Some((out_start, absolute_row)),
                            CompletedCommandMetadata {
                                exit_code,
                                duration_ms,
                                execution_id: finished_id.clone(),
                                agent_generation,
                            },
                        );
                    }
                    ZoneState::CommandStarted(prompt_start, cmd_start) => {
                        let zone = CommandZone {
                            id: 0, // assigned by push_command_zone
                            prompt_start,
                            command_start: Some(cmd_start),
                            output_start: None,
                            output_end: Some(absolute_row),
                            exit_code,
                            command: self.zone_command_text(),
                            // Without a `C` there was no locally observed
                            // execution phase, so only a shell-reported
                            // duration param is meaningful here.
                            duration_ms: metadata_duration_ms,
                            finished_at_ms: Self::wall_clock_ms(),
                            command_truncated,
                            cwd,
                            // No `C` means no output range to snapshot.
                            captured_output: None,
                            rows_evicted: false,
                        };
                        self.push_command_zone(zone);
                        // Same rule for the agent record: shell-reported
                        // duration or nothing.
                        self.record_completed_command(
                            cmd_start,
                            absolute_row,
                            None,
                            CompletedCommandMetadata {
                                exit_code,
                                duration_ms: metadata_duration_ms,
                                execution_id: finished_id,
                                agent_generation,
                            },
                        );
                    }
                    _ => {}
                }
                self.current_zone_state = ZoneState::Idle;
                self.current_command_start_col = None;
                self.current_command_extent_row = None;
                self.current_command_text = None;
                self.current_command_cwd = None;
                self.current_command_truncated = false;
                self.agent_prompt_input_tainted = false;
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

    /// Append a finished zone, assigning its stable id and enforcing both the
    /// 256-entry cap and the captured-snapshot byte budget.
    fn push_command_zone(&mut self, mut zone: CommandZone) {
        zone.id = self.next_zone_id;
        self.next_zone_id += 1;
        self.captured_output_bytes = self.captured_output_bytes.saturating_add(
            zone.captured_output
                .as_ref()
                .map_or(0, |(text, _)| text.len()),
        );
        self.command_zones.push_back(zone);
        if self.command_zones.len() > 256 {
            if let Some(evicted) = self.command_zones.pop_front() {
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
            }
        }
    }

    /// Snapshot one zone's output rows for [`CommandZone::captured_output`]:
    /// the exact live extraction (same trimming, same 1 MiB cap, same
    /// whole-rows-only truncation flag), blank output collapsing to `None`.
    fn capture_zone_output(&self, start: usize, end: usize) -> Option<(String, bool)> {
        let (out, capped) = self.rows_text(start, end, Self::ZONE_OUTPUT_CAP_BYTES);
        if out.trim().is_empty() {
            None
        } else {
            Some((out, capped))
        }
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
    /// Unknown `?` badge), no duration, no finish timestamp. Deliberately
    /// NOT queued to `record_completed_command`: that queue feeds command
    /// history/journal/notifications, which all treat an entry as an
    /// observed completion.
    fn finalize_stale_zone(&mut self, boundary_row: usize) {
        let ZoneState::OutputStarted(prompt_start, cmd_start, out_start) = self.current_zone_state
        else {
            return;
        };
        let cwd = self
            .current_command_cwd
            .take()
            .or_else(|| self.control_free_osc7_cwd());
        let zone = CommandZone {
            id: 0, // assigned by push_command_zone
            prompt_start,
            command_start: Some(cmd_start),
            output_start: Some(out_start),
            output_end: Some(boundary_row),
            exit_code: None,
            command: self.zone_command_text(),
            duration_ms: None,
            finished_at_ms: None,
            command_truncated: self.current_command_truncated,
            cwd,
            captured_output: self.capture_zone_output(out_start, boundary_row),
            rows_evicted: false,
        };
        self.push_command_zone(zone);
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
    fn on_scrollback_rows_trimmed(&mut self, count: usize) {
        if count == 0 {
            return;
        }
        for zone in &mut self.command_zones {
            if zone.rows_evicted {
                continue;
            }
            if zone.prompt_start < count {
                zone.rows_evicted = true;
                zone.prompt_start = 0;
                zone.command_start = None;
                zone.output_start = None;
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

    /// Look up a completed zone by its stable id. `None` when the zone has
    /// been trimmed away with old scrollback (a stale block selection).
    pub fn zone_by_id(&self, id: u64) -> Option<&CommandZone> {
        self.command_zones.iter().find(|zone| zone.id == id)
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
    /// it survives scrollback trimming). Live extraction only serves zones
    /// whose snapshot was evicted by the byte budget while their rows are
    /// still present.
    pub fn zone_output_text_capped(&self, id: u64) -> Option<(String, bool)> {
        let zone = self.zone_by_id(id)?;
        if let Some((text, truncated)) = &zone.captured_output {
            return Some((text.clone(), *truncated));
        }
        let start = zone.output_start?;
        let end = zone.output_end.unwrap_or(start);
        let (out, capped) = self.rows_text(start, end, Self::ZONE_OUTPUT_CAP_BYTES);
        if out.trim().is_empty() {
            None
        } else {
            Some((out, capped))
        }
    }

    /// The OLDEST completed zone that failed (exit reported and nonzero) —
    /// "jump to first failed" starts at the earliest failure still in scope.
    pub fn first_failed_zone(&self) -> Option<&CommandZone> {
        self.command_zones
            .iter()
            .find(|zone| zone.exit_code.is_some_and(|code| code != 0))
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

    /// Capture one finished OSC 133 command for the AI agent queue. The
    /// command line spans `cmd_start..cmd_end`; `output` gives the output row
    /// range when the shell reported one.
    fn record_completed_command(
        &mut self,
        cmd_start: usize,
        cmd_end: usize,
        output: Option<(usize, usize)>,
        metadata: CompletedCommandMetadata,
    ) {
        const MAX_COMMAND_BYTES: usize = 16 * 1024;
        const MAX_OUTPUT_BYTES: usize = 256 * 1024;
        const MAX_PENDING_COMPLETED: usize = 32;
        let command = self.current_command_text.take().unwrap_or_else(|| {
            self.rows_text(cmd_start, cmd_end, MAX_COMMAND_BYTES)
                .0
                .trim()
                .to_string()
        });
        if command.is_empty() {
            return;
        }
        let output_available = output.is_some();
        let output = output
            .map(|(start, end)| self.rows_text(start, end, MAX_OUTPUT_BYTES).0)
            .unwrap_or_default();
        if self.pending_completed_commands.len() >= MAX_PENDING_COMPLETED {
            self.pending_completed_commands.pop_front();
        }
        let truncated = output.len() >= MAX_OUTPUT_BYTES;
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
        });
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

    /// Plain text of the absolute buffer rows `start..end` (scrollback plus
    /// live grid), soft-wrapped rows joined, per-line trailing padding
    /// trimmed, and the total capped at `max_bytes`. The second return is
    /// `true` when the cap dropped remaining rows (rows are never cut
    /// mid-segment, so text is only ever lost whole rows at a time).
    fn rows_text(&self, start: usize, end: usize, max_bytes: usize) -> (String, bool) {
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
                let text: String = line
                    .decompress()
                    .iter()
                    .filter(|cell| !cell.flags.wide_continuation())
                    .map(|cell| cell.character)
                    .collect();
                (text, line.is_wrapped)
            } else {
                let grid_row = abs_row - scrollback_len;
                if grid_row >= self.grid.rows() {
                    break;
                }
                let text: String = (0..self.grid.row_len())
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
            out.push_str(&segment);
            if out.len() >= max_bytes {
                // Truncated only if the break actually skips content.
                capped = abs_row + 1 < end;
                break;
            }
            if !wrapped && abs_row + 1 < end {
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
        if let Some((_sel, data)) = value.split_once(';') {
            if data == "?" {
                // Query: signal main loop to read clipboard and respond
                self.pending_osc52_clipboard_query = true;
            } else if !data.is_empty() {
                // Set: decode base64 and store for main loop to apply
                if let Some(decoded) = Self::decode_base64(data) {
                    self.pending_osc52_clipboard_set = Some(decoded);
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

    fn put_char(&mut self, ch: char) {
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

        // Set up wide character continuation cell if needed
        if width == 2 && self.cursor_col + 1 < cols {
            let cont_cell = self.grid.get_mut(self.cursor_row, self.cursor_col + 1);
            *cont_cell = blank_cell;
            cont_cell.flags.set_wide_continuation(true);
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
        self.mark_command_echo_extent();
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
            let col = self.cursor_col;
            let end = col + chunk_len;
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
            }

            self.cursor_col += chunk_len;
            pos += chunk_len;

            self.mark_row_dirty(self.cursor_row);
            self.mark_command_echo_extent();

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
    ) {
        if (self.use_alt_buffer && !allow_alt_buffer) || self.grid.rows() == 0 {
            return;
        }

        let first = (0..self.grid.rows()).find(|&row| !self.line_is_blank(row));
        let last = (0..self.grid.rows()).rfind(|&row| !self.line_is_blank(row));
        let (Some(first), Some(last)) = (first, last) else {
            return;
        };

        if dedupe_snapshot {
            let snapshot = self.visible_screen_snapshot().unwrap_or_default();
            if snapshot == self.last_archived_screen_snapshot {
                return;
            }
            self.last_archived_screen_snapshot = snapshot;
        }

        for row in first..=last {
            let line = ScrollbackLine::compress(&self.grid[row], self.grid.row_wrapped[row]);
            self.push_scrollback_compressed_with_options(line, allow_alt_buffer);
        }
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
                Some(ScrollbackLine::compress(
                    &self.grid[top],
                    self.grid.row_wrapped[top],
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

    /// P4：获取网格版本号（用于缓存比较）
    pub fn get_grid_version(&self) -> u64 {
        self.grid_version
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
        if let Err(_error) = self
            .kitty_graphics
            .parse_graphics_payload_at(payload, cursor_col, cursor_row)
        {
            crate::debug_log!("[APC] Kitty graphics error: {}", _error);
        }
        let responses = self.kitty_graphics.take_responses();
        if !responses.is_empty() {
            self.output_buffer.extend_from_slice(&responses);
        }
    }

    /// Check if sync output timed out (>1s) and auto-clear if so
    pub fn check_sync_output_timeout(&mut self) {
        if self.sync_output_active {
            if let Some(start) = self.sync_output_start {
                if start.elapsed() > std::time::Duration::from_secs(1) {
                    if self.use_alt_buffer {
                        self.archive_visible_screen_to_scrollback_with_options(true, true);
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

    pub fn process_input(&mut self, input: &[u8]) {
        // Guard against an unterminated OSC/DCS/escape sequence. Such a sequence
        // is buffered into `pending_escape` and re-scanned from its start on every
        // read, which is both O(n^2) in CPU and unbounded in memory. Once the
        // buffered prefix exceeds this cap, abandon the partial sequence. The cap
        // is generous enough for legitimate large payloads (e.g. OSC 52 clipboard).
        const MAX_PENDING_ESCAPE: usize = 1 << 20; // 1 MiB
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
            let byte = data_slice[i];

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
                                self.pending_escape
                                    .extend_from_slice(&data_slice[esc_start..]);
                                break;
                            }

                            let payload_end = if data_slice[i - 1] == 0x07 {
                                i - 1
                            } else {
                                i - 2
                            };
                            if payload_end >= payload_start {
                                if let Ok(payload) =
                                    std::str::from_utf8(&data_slice[payload_start..payload_end])
                                {
                                    let (command, value) =
                                        payload.split_once(';').unwrap_or((payload, ""));
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
                                            // params can include id=<identifier>
                                            // Empty URI = close hyperlink
                                            if let Some((params, uri)) = value.split_once(';') {
                                                if uri.is_empty() {
                                                    // Close hyperlink
                                                    self.current_hyperlink = None;
                                                } else {
                                                    // Open hyperlink
                                                    let id = params
                                                        .split(':')
                                                        .find_map(|p| p.strip_prefix("id="))
                                                        .map(|s| s.to_string());
                                                    self.current_hyperlink =
                                                        Some((uri.to_string(), id));
                                                }
                                            } else if value.is_empty() {
                                                // OSC 8 ; ; (close hyperlink)
                                                self.current_hyperlink = None;
                                            }
                                        } else if command == "4" {
                                            self.handle_osc_palette(value);
                                        } else if command == "10"
                                            || command == "11"
                                            || command == "12"
                                        {
                                            self.handle_osc_color(command, value);
                                        } else if command == "110"
                                            || command == "111"
                                            || command == "112"
                                        {
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
                                                let title = parts
                                                    .get(1)
                                                    .unwrap_or(&"")
                                                    .chars()
                                                    .take(256)
                                                    .collect();
                                                let body = parts
                                                    .get(2)
                                                    .unwrap_or(&"")
                                                    .chars()
                                                    .take(256)
                                                    .collect();
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
                                                if let Some((metadata, osc_payload)) =
                                                    value.split_once(';')
                                                {
                                                    (metadata, Some(osc_payload))
                                                } else {
                                                    (value, None)
                                                };
                                            self.handle_osc_5522(metadata, osc_payload);
                                        }
                                    }
                                }
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
                                self.pending_escape
                                    .extend_from_slice(&data_slice[esc_start..]);
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
                                    _ => break,
                                }
                                i += 1;
                            }

                            let Some(final_byte) = final_byte else {
                                self.pending_escape
                                    .extend_from_slice(&data_slice[esc_start..]);
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
                        self.put_char(byte as char);
                        i += 1;
                    }
                }
                // UTF-8 multi-byte sequences: try to consume all bytes eagerly
                0xC2..=0xDF => {
                    let expected: u8 = 2;
                    if i + 1 < data_slice.len() && (data_slice[i + 1] & 0xC0) == 0x80 {
                        let buf = [byte, data_slice[i + 1], 0, 0];
                        if let Ok(s) = std::str::from_utf8(&buf[..2]) {
                            if let Some(ch) = s.chars().next() {
                                self.put_char(ch);
                            }
                        }
                        i += 2;
                    } else {
                        self.utf8_buf[0] = byte;
                        self.utf8_len = 1;
                        self.utf8_expected = expected;
                        i += 1;
                    }
                }
                0xE0..=0xEF => {
                    let expected: u8 = 3;
                    if i + 2 < data_slice.len()
                        && (data_slice[i + 1] & 0xC0) == 0x80
                        && (data_slice[i + 2] & 0xC0) == 0x80
                    {
                        let buf = [byte, data_slice[i + 1], data_slice[i + 2], 0];
                        if let Ok(s) = std::str::from_utf8(&buf[..3]) {
                            if let Some(ch) = s.chars().next() {
                                self.put_char(ch);
                            }
                        }
                        i += 3;
                    } else {
                        self.utf8_buf[0] = byte;
                        self.utf8_len = 1;
                        self.utf8_expected = expected;
                        i += 1;
                    }
                }
                0xF0..=0xF4 => {
                    let expected: u8 = 4;
                    if i + 3 < data_slice.len()
                        && (data_slice[i + 1] & 0xC0) == 0x80
                        && (data_slice[i + 2] & 0xC0) == 0x80
                        && (data_slice[i + 3] & 0xC0) == 0x80
                    {
                        let buf = [
                            byte,
                            data_slice[i + 1],
                            data_slice[i + 2],
                            data_slice[i + 3],
                        ];
                        if let Ok(s) = std::str::from_utf8(&buf[..4]) {
                            if let Some(ch) = s.chars().next() {
                                self.put_char(ch);
                            }
                        }
                        i += 4;
                    } else {
                        self.utf8_buf[0] = byte;
                        self.utf8_len = 1;
                        self.utf8_expected = expected;
                        i += 1;
                    }
                }
                _ => {
                    if self.utf8_len > 0 && (byte & 0xC0) == 0x80 {
                        self.utf8_buf[self.utf8_len as usize] = byte;
                        self.utf8_len += 1;
                        if self.utf8_len == self.utf8_expected {
                            if let Ok(s) =
                                std::str::from_utf8(&self.utf8_buf[..self.utf8_len as usize])
                            {
                                if let Some(ch) = s.chars().next() {
                                    self.put_char(ch);
                                }
                            }
                            self.utf8_len = 0;
                        }
                    } else {
                        self.utf8_len = 0;
                    }
                    i += 1;
                }
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
                        self.put_char(ch);
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
                        self.scrollback.clear();
                        self.scroll_offset = 0;
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
        self.scrollback.clear();
        self.command_zones.clear();
        self.captured_output_bytes = 0;
        self.current_zone_state = ZoneState::default();
        self.current_command_start_col = None;
        self.current_command_extent_row = None;
        self.agent_prompt_input_tainted = false;
        self.armed_agent_execution = None;
        self.active_agent_execution = None;
        self.current_command_text = None;
        self.current_command_id = None;
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
        self.selection = None;
        self.current_hyperlink = None;
        // A half-finished Kitty upload must not survive into the reset screen.
        // There is no wall clock behind this: RIS plus the shared aggregate
        // in-flight cap replace the old 10-second pending-transfer expiry.
        self.kitty_graphics.reset_transfers();
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
                self.archive_visible_screen_to_scrollback_with_options(true, true);
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

                    // Selection anchors are absolute (scrollback+grid) row indices
                    // tied to the buffer that was visible. After a buffer swap they
                    // would highlight unrelated lines, so drop the selection.
                    self.selection = None;
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
                    self.modes.remove(&mode);
                    self.pending_wrap = false;

                    // See the matching set_mode arm: clear selection because its
                    // anchors point into the alt buffer, and reset DECSTBM so the
                    // alt buffer's scroll region doesn't carry into the main one.
                    self.selection = None;
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
                    self.archive_visible_screen_to_scrollback_with_options(true, true);
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
    /// History rows are decompressed lazily while live-grid rows stay borrowed,
    /// avoiding a second full-buffer allocation for every search keystroke.
    pub fn search_lines(&self) -> impl Iterator<Item = Cow<'_, [TerminalCell]>> + '_ {
        self.scrollback
            .iter()
            .map(|line| Cow::Owned(line.decompress()))
            .chain(self.grid.iter().map(Cow::Borrowed))
    }

    /// Absolute buffer row represented by viewport row zero.
    pub fn viewport_absolute_start(&self) -> usize {
        self.scrollback.len().saturating_sub(self.scroll_offset)
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
        let Some(target) = click_cursor::target_cell(cursor, click, columns, self.editable_span())
        else {
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

    pub fn get_visible_cells(&mut self) -> std::sync::Arc<Vec<Vec<TerminalCell>>> {
        if let Some((cached_version, cached_offset, ref cells)) = self.visible_cells_cache {
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
        let prev_version = prev.as_ref().map(|(v, _, _)| *v);
        let prev_offset = prev.as_ref().map(|(_, o, _)| *o);
        let mut recycled = prev.map(|(_, _, a)| a);

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
                ));
                return arc;
            }
        }

        let cells = if self.scroll_offset == 0 {
            // Fast path (shared allocation): fresh copy of current grid.
            self.grid.to_vec()
        } else {
            // Slow path: reflow scrollback
            // Padding added to retained history is structural, not newly erased
            // live-screen content, so it must not inherit the current SGR color.
            let blank_cell = TerminalCell::default();

            let mut start_idx = self
                .scrollback
                .len()
                .saturating_sub(self.scroll_offset + rows);
            while start_idx > 0 && self.scrollback[start_idx - 1].is_wrapped {
                start_idx -= 1;
            }
            let end_idx = self.scrollback.len();
            let to_reflow: Vec<ScrollbackLine> = self
                .scrollback
                .iter()
                .skip(start_idx)
                .take(end_idx - start_idx)
                .cloned()
                .collect();

            let reflowed = Self::reflow_lines(&to_reflow, cols, &blank_cell);
            let skip = reflowed.len().saturating_sub(self.scroll_offset + rows);
            let visible_start = skip + (reflowed.len() - skip).saturating_sub(self.scroll_offset);
            let mut result: Vec<Vec<TerminalCell>> = reflowed[visible_start..]
                .iter()
                .map(|l| l.decompress())
                .collect();

            if result.len() > rows {
                result.truncate(rows);
            }

            for row in self.grid.iter() {
                if result.len() < rows {
                    result.push(self.normalize_line_width(row.to_vec(), cols));
                } else {
                    break;
                }
            }

            while result.len() < rows {
                result.push(self.blank_line(cols));
            }

            result
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
    fn viewport_row_to_absolute(&self, viewport_row: usize) -> usize {
        self.scrollback.len().saturating_sub(self.scroll_offset) + viewport_row
    }

    #[allow(dead_code)]
    pub fn select_text(&mut self, anchor: (usize, usize), active: (usize, usize)) {
        self.selection = Some(Selection {
            anchor,
            active,
            mode: SelectionMode::Normal,
        });
    }

    /// Start a new selection at a viewport-relative position.
    /// Converts to absolute buffer coordinates internally.
    pub fn start_selection(&mut self, viewport_pos: (usize, usize)) {
        self.start_selection_with_mode(viewport_pos, SelectionMode::Normal);
    }

    pub fn start_block_selection(&mut self, viewport_pos: (usize, usize)) {
        self.start_selection_with_mode(viewport_pos, SelectionMode::Block);
    }

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
    pub fn update_selection(&mut self, viewport_pos: (usize, usize)) {
        let abs_row = self.viewport_row_to_absolute(viewport_pos.0);
        if let Some(ref mut sel) = self.selection {
            sel.active = (abs_row, viewport_pos.1);
        }
    }

    /// Select the word at the given (row, col) position in the visible grid.
    /// Word boundaries are determined by character class: alphanumeric/underscore,
    /// whitespace, or punctuation/symbols.
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

    fn word_span_at(&mut self, row: usize, col: usize) -> Option<(usize, usize, usize)> {
        let visible = self.get_visible_cells();
        if row >= visible.len() {
            return None;
        }
        let line = &visible[row];
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
            let abs_row = self.viewport_row_to_absolute(row);
            return Some((abs_row, left, right));
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

        let abs_row = self.viewport_row_to_absolute(row);
        Some((abs_row, left, right))
    }

    /// Extend an existing double-click selection using word boundaries.
    pub fn extend_word_selection_to(&mut self, row: usize, col: usize) {
        let Some((target_row, target_left, target_right)) = self.word_span_at(row, col) else {
            return;
        };
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

    pub fn copy_selection(&self) -> Option<String> {
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

        // When scrolling to bottom (offset 0), reset to live view
        if self.scroll_offset == 0 {
            self.scroll_offset = 0;
        }
    }

    fn strip_trailing_blanks(cells: &[TerminalCell]) -> &[TerminalCell] {
        let mut end = cells.len();
        while end > 0
            && cells[end - 1].character == ' '
            && cells[end - 1].background == Color::Default
            && !cells[end - 1].flags.wide()
            && !cells[end - 1].flags.wide_continuation()
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
    pub fn normalize_scrollback_width(&mut self) {
        if self.scrollback.is_empty() {
            return;
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
        self.scroll_offset = 0;
        // These structures use absolute physical rows. Discarding them is safer
        // than leaving selections/zones pointing at unrelated text.
        self.selection = None;
        self.command_zones.clear();
        self.captured_output_bytes = 0;
        self.current_zone_state = ZoneState::default();
        self.current_command_start_col = None;
        self.current_command_extent_row = None;
        self.agent_prompt_input_tainted = false;
        self.armed_agent_execution = None;
        self.active_agent_execution = None;
        self.current_command_text = None;
        self.current_command_id = None;
        self.current_command_started_at = None;
        self.grid_version = self.grid_version.wrapping_add(1);
        self.visible_cells_cache = None;
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
                    let line = ScrollbackLine::compress(&self.grid[r], self.grid.row_wrapped[r]);
                    self.push_scrollback_compressed(line);
                }
                let src_start = top_remove * cols_now;
                let total = old_rows * cols_now;
                self.grid.cells.copy_within(src_start..total, 0);
                self.grid.row_wrapped.copy_within(top_remove..old_rows, 0);
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

        self.scroll_offset = 0;
        self.cursor_row = self.cursor_row.min(rows.saturating_sub(1));
        self.cursor_col = self.cursor_col.min(cols.saturating_sub(1));
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
    pub fn row_selection_cols(&self, viewport_row: usize) -> Option<(usize, usize)> {
        let sel = self.selection?;
        let abs_row = self.viewport_row_to_absolute(viewport_row);
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
        terminal.process_input(b"\x1b]133;B\x07true\r\n");
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

        // Without a `C` there is no execution phase, so no duration — and the
        // previous command's start must not leak into this record.
        terminal.process_input(b"\x1b]133;A\x07$ ");
        terminal.process_input(b"\x1b]133;B\x07true\r\n");
        terminal.process_input(b"\x1b]133;D;0\x07");
        let completed = terminal.take_completed_commands();
        assert_eq!(completed.len(), 1);
        assert_eq!(completed[0].duration_ms, None);
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

        // Even without a `C` (no locally observed execution phase), an
        // explicit shell-measured duration is trusted.
        terminal.process_input(b"\x1b]133;A\x07$ ");
        terminal.process_input(b"\x1b]133;B\x07true\r\n");
        terminal.process_input(b"\x1b]133;D;exit=0;duration=7\x07");
        assert_eq!(terminal.command_zones[1].duration_ms, Some(7));

        // Local timing remains the fallback when no param arrives.
        terminal.process_input(b"\x1b]133;A\x07$ ");
        terminal.process_input(b"\x1b]133;B\x07ls\r\n");
        terminal.process_input(b"\x1b]133;C\x07f\r\n");
        terminal.process_input(b"\x1b]133;D;0\x07");
        assert!(terminal.command_zones[2].duration_ms.is_some());
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

    use super::{
        ClipboardReadKind, Color, CursorShape, ScrollbackLine, TerminalCell, TerminalState,
        MAX_TERMINAL_TITLE_CHARS,
    };

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

        assert!(
            terminal.scrollback_len() >= 6,
            "expected synchronized alt-screen snapshots in scrollback"
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
            text.contains("first page") || text.contains("second page"),
            "expected archived synchronized screen content, got {text:?}"
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
        terminal.normalize_scrollback_width();
        let history = terminal.scrollback[0].decompress();
        assert_eq!(history.len(), 5);
        assert_eq!(history[4].background, Color::Default);
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

    /// Text content of an absolute buffer row (scrollback + live grid).
    fn buffer_row_text(terminal: &TerminalState, row: usize) -> String {
        terminal
            .search_lines()
            .nth(row)
            .map(|line| line.iter().map(|cell| cell.character).collect())
            .unwrap_or_default()
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

        terminal.process_input(b"\x1b]133;D;0\x07");
        assert_eq!(terminal.running_zone_start(), None);
        assert_eq!(terminal.live_prompt_row(), None);
    }

    #[test]
    fn command_zones_are_enriched_at_both_push_sites() {
        let mut terminal = TerminalState::new(40, 8);
        // Full lifecycle (A/B/C/D): command captured, duration measured.
        terminal.process_input(b"\x1b]133;A\x07$ \x1b]133;B\x07echo hi\r\n");
        terminal.process_input(b"\x1b]133;C\x07hi\r\n\x1b]133;D;0\x07");
        // No `C` (empty prompt submit): no output range, no duration.
        terminal.process_input(b"\x1b]133;A\x07$ \x1b]133;B\x07true\r\n");
        terminal.process_input(b"\x1b]133;D;1\x07");

        assert_eq!(terminal.command_zones.len(), 2);
        let full = &terminal.command_zones[0];
        assert_eq!(full.id, 0);
        assert_eq!(full.command.as_deref(), Some("echo hi"));
        assert!(full.duration_ms.is_some());
        assert!(full.finished_at_ms.is_some());
        assert_eq!(full.exit_code, Some(0));

        let short = &terminal.command_zones[1];
        assert_eq!(short.id, 1);
        // Without a `C` the command was never captured; scraping the rows
        // would swallow the rendered prompt, so the zone carries no command
        // and classifies as Background regardless of the exit code.
        assert_eq!(short.command, None);
        assert_eq!(
            crate::block_mode::classify(short.command.as_deref(), short.exit_code),
            crate::block_mode::BlockOutcome::Background
        );
        assert_eq!(short.duration_ms, None);
        assert!(short.finished_at_ms.is_some());
        assert_eq!(short.exit_code, Some(1));
    }

    #[test]
    fn empty_prompt_enter_without_c_is_background_not_a_failed_prompt_scrape() {
        // bash-preexec-style integrations emit `D` from precmd without a `C`
        // on every empty-prompt Enter. The rendered prompt must not be
        // scraped as the zone's command, and the zone must not surface as a
        // Failed block even when the previous command left a nonzero status.
        let mut terminal = TerminalState::new(40, 8);
        terminal.process_input(b"\x1b]133;A\x07$ \x1b]133;B\x07\r\n\x1b]133;D;1\x07");

        assert_eq!(terminal.command_zones.len(), 1);
        let zone = &terminal.command_zones[0];
        assert_eq!(zone.command, None);
        assert_eq!(zone.exit_code, Some(1));
        assert_eq!(
            crate::block_mode::classify(zone.command.as_deref(), zone.exit_code),
            crate::block_mode::BlockOutcome::Background
        );
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
        // A no-`C` zone records no output range, so no snapshot either.
        terminal.process_input(b"\x1b]133;A\x07$ \x1b]133;B\x07\r\n\x1b]133;D;0\x07");
        assert_eq!(terminal.command_zones[1].captured_output, None);
        assert_eq!(terminal.captured_output_bytes, "out\nout\nout".len());
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
        assert_eq!(terminal.captured_output_bytes, per_zone);

        // A zone that lost its snapshot but still has rows in scrollback
        // falls back to live extraction.
        assert_eq!(
            terminal.zone_output_text(ids[0]).as_deref(),
            Some("out\nout\nout")
        );

        // The newest zone's snapshot survives even an impossible budget (a
        // fresh snapshot always fits the real 8 MiB budget: one snapshot is
        // capped at 1 MiB).
        terminal.enforce_captured_output_budget(0);
        assert!(terminal.command_zones[2].captured_output.is_some());
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
        // The Unknown badge (`?`) is exactly what an unreported exit shows.
        assert_eq!(
            crate::block_mode::classify(zone.command.as_deref(), zone.exit_code),
            crate::block_mode::BlockOutcome::Unknown
        );
        // Its output up to the new prompt row was still snapshotted.
        assert_eq!(zone.captured_output, Some(("building".to_string(), false)));
        // Deliberately NOT queued as a completed command: that queue feeds
        // history/journal/notifications, which treat entries as observed
        // completions.
        assert!(terminal.take_completed_commands().is_empty());

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
        terminal.process_input(b"\x1b]133;A\x07$ \x1b]133;B\x07half-typed");
        terminal.process_input(b"\r\n\x1b]133;A\x07$ ");
        assert!(terminal.command_zones.is_empty());
        assert!(terminal.take_completed_commands().is_empty());
        // A bare abandoned prompt (`A` with no `B`) spawns nothing either.
        terminal.process_input(b"\x1b]133;A\x07$ ");
        assert!(terminal.command_zones.is_empty());
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
