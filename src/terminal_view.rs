use crate::color::{resolve_bg_with_palette, resolve_fg_with_palette};
use crate::search::SearchMatch;
use crate::terminal::{
    clamp_terminal_dimensions, CursorShape, DynamicColorPalette, ProjectionKey, SyntheticRowKey,
    TerminalCell, UnderlineStyle,
};
use crate::theme::Theme;
use crate::theme::ThemeExt as _;

use std::time::Instant;

use iced::advanced::input_method::{self, InputMethod};
use iced::advanced::layout::{self, Layout};
use iced::advanced::renderer::{self, Quad};
use iced::advanced::text::{self, Text};
use iced::advanced::widget::{tree, Tree, Widget};
use iced::advanced::{Clipboard, Shell};
use iced::mouse;
use iced::{
    border, Background, Border, Color, Element, Event, Length, Pixels, Point, Rectangle, Shadow,
    Size, Vector,
};

/// Which mouse button a [`MouseInput`] refers to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseButton {
    Left,
    Middle,
    Right,
}

impl MouseButton {
    pub const fn slot(self) -> usize {
        match self {
            Self::Left => 0,
            Self::Middle => 1,
            Self::Right => 2,
        }
    }
}

/// A completed-card press that belongs to Block Mode instead of the PTY or
/// native terminal text selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockMouseAction {
    /// Plain single press on the card's prompt/header row.
    Replace,
    /// Shift+single press anywhere in the completed card.
    Range,
    /// Ctrl+Shift+single press anywhere in the completed card.
    Toggle,
    /// Right press anywhere in the completed card.
    Menu,
}

/// A collapsed-output summary that completed a stable click gesture. The
/// application revalidates both identities against the session's current
/// projection before mutating its policy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SummaryActivation {
    pub key: SyntheticRowKey,
    pub projection_key: ProjectionKey,
}

/// A semantic mouse interaction over the terminal grid, in 0-indexed cell
/// coordinates. Emitted by [`TermWidget`] and handled by the application.
#[derive(Debug, Clone, Copy)]
pub enum MouseInput {
    Press {
        col: usize,
        row: usize,
        button: MouseButton,
        shift: bool,
        alt: bool,
        ctrl: bool,
        count: u32,
        /// Press landed on a finalized history-card row. This stays true for
        /// ordinary output text selection even when no Block action claimed it.
        finalized: bool,
        /// Press is over a real grid cell that may activate a host-side link.
        /// Every grid surface qualifies except a finalized command/header;
        /// padding, gutter, and scrollbar never do.
        link_eligible: bool,
        /// Press is over real application-owned grid cells: an active/live
        /// card row, or any real grid row in alternate/non-Block mode. Padding,
        /// scrollbar, pre-zone history, and finalized cards remain local.
        app_eligible: bool,
        /// Block action claimed at press time. `None` keeps the ordinary PTY /
        /// native text-selection pipeline, including double/triple click.
        block: Option<BlockMouseAction>,
        /// Stable finalized-zone identity from the exact row snapshot used to
        /// classify `block`. The app must never re-target a later neighbour
        /// merely because the viewport moved before update handled the press.
        block_zone_id: Option<u64>,
        /// A Ctrl+single-left link activation claimed for its entire gesture.
        /// No Drag/Release is emitted, so it can never mutate text selection or
        /// leak half a mouse-reporting sequence after opening the link.
        link: bool,
        /// Immutable terminal projection revision used to classify `link`.
        /// The application revalidates it before opening so resize, scrolling,
        /// buffer swaps, and output cannot retarget a stale press.
        link_revision: u64,
        /// Window-local pointer position. Context menus freeze this at open
        /// time so later pointer movement cannot move the stable-target panel.
        x: f32,
        y: f32,
    },
    Drag {
        col: usize,
        row: usize,
        count: u32,
    },
    Release {
        col: usize,
        row: usize,
        button: MouseButton,
    },
    Wheel {
        col: usize,
        row: usize,
        up: bool,
        ctrl: bool,
        shift: bool,
        /// Number of whole lines this event scrolls (≥1).
        lines: usize,
        /// Wheel is over real live/active grid cells (not padding, scrollbar,
        /// or finalized history) and is therefore eligible for app reporting.
        app_eligible: bool,
    },
    /// Drag/jump the scrollbar to an absolute scrollback offset
    /// (0 = bottom/live view).
    ScrollTo {
        offset: usize,
    },
}

/// A Kitty-graphics image to paint: a cached RGBA handle plus its grid-cell
/// placement (col/row origin, cell span) and source pixel dimensions.
#[derive(Clone)]
pub struct KittyRender {
    pub handle: iced::advanced::image::Handle,
    pub col: usize,
    pub row: usize,
    pub cols: usize,
    pub rows: usize,
    pub id: u32,
    pub px_w: u32,
    pub px_h: u32,
}

/// Width of the scrollbar gutter on the right edge, in pixels.
pub const SCROLLBAR_WIDTH: f32 = 10.0;
/// Minimum thumb height so it stays grabbable with deep scrollback.
const SCROLLBAR_MIN_THUMB: f32 = 24.0;
/// Width of the block-outcome stripe at the widget's left edge, in pixels.
const BLOCK_STRIPE_WIDTH: f32 = 3.0;
/// Width of the stripe on the active edge of a block selection (full opacity too).
const BLOCK_STRIPE_SELECTED_WIDTH: f32 = 5.0;
/// Finished/live cards use the same horizontal inset as the normal
/// Anvil/Forge Block surface. Compact mode halves it without moving column 0:
/// the inset is paint-only and therefore never changes PTY geometry.
const BLOCK_CARD_INSET: f32 = 8.0;
const BLOCK_CARD_COMPACT_INSET: f32 = 4.0;
const BLOCK_CARD_RADIUS: f32 = 10.0;
const BLOCK_CARD_COMPACT_RADIUS: f32 = 6.0;
/// A one-pixel breathing gap at real card edges gives adjacent single-grid
/// blocks separation without inserting synthetic terminal rows.
const BLOCK_CARD_EDGE_GAP: f32 = 1.0;
/// Width of the reserved card gutter. Ties to the stripe widths above: it must
/// cover the widest stripe
/// (`BLOCK_STRIPE_SELECTED_WIDTH` = 5px, plus 1px slack) so a press anywhere
/// on the visible stripe selects the block, even when the configured left
/// padding is narrower than the stripe. It must also exceed the window's
/// `RESIZE_EDGE` grip (5px): on a pane flush with the window's left border the
/// grip overlay swallows presses in its band, so anything narrower would leave
/// the stripe unclickable there.
/// Layout-owned space to the left of column zero while Block Mode is enabled.
/// Paint and hit testing stay inside this reservation and never cover glyphs.
const BLOCK_GUTTER_WIDTH: f32 = 8.0;

/// Visual role of one card. This is intentionally renderer-owned: Frost keeps
/// a single terminal grid while Anvil/Forge use separate VTE widgets, but the
/// user-visible state and theme-relative treatment remain the same.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum BlockCardKind {
    #[default]
    Finished,
    Failed,
    Unknown,
    Background,
    Active,
}

fn block_mouse_action(
    button: MouseButton,
    row_has_selectable_block: bool,
    row_is_header: bool,
    shift: bool,
    ctrl: bool,
    count: u32,
) -> Option<BlockMouseAction> {
    if !row_has_selectable_block {
        return None;
    }
    match button {
        MouseButton::Right => Some(BlockMouseAction::Menu),
        MouseButton::Left if count != 1 => None,
        MouseButton::Left if ctrl && shift => Some(BlockMouseAction::Toggle),
        MouseButton::Left if shift => Some(BlockMouseAction::Range),
        MouseButton::Left if row_is_header => Some(BlockMouseAction::Replace),
        MouseButton::Left | MouseButton::Middle => None,
    }
}

fn app_mouse_surface_eligible(
    over_grid_rows: bool,
    over_grid_columns: bool,
    full_grid: bool,
    row_app_eligible: bool,
) -> bool {
    over_grid_rows && over_grid_columns && (full_grid || row_app_eligible)
}

pub fn link_surface_eligible(
    over_real_grid: bool,
    finalized: bool,
    finalized_header: bool,
) -> bool {
    over_real_grid && !(finalized && finalized_header)
}

pub fn ctrl_link_eligible(
    button: MouseButton,
    count: u32,
    ctrl: bool,
    shift: bool,
    link_surface: bool,
) -> bool {
    link_surface && button == MouseButton::Left && count == 1 && ctrl && !shift
}

/// Block-mode chrome for one visible grid row, precomputed by the app the
/// same way the per-row `selection` spans are. `Default` (all off) rows cost
/// nothing to draw.
#[derive(Clone, Debug, Default)]
pub struct BlockPaintRow {
    /// This row belongs to a finalized block and participates in card hit
    /// routing. Running/live-prompt chrome deliberately leaves this false so
    /// their presses remain ordinary terminal/application input.
    pub selectable: bool,
    /// Stable retained zone represented by this finalized row. Unlike
    /// `card_group`, this survives viewport re-layout and is safe for actions.
    pub zone_id: Option<u64>,
    /// This primary-buffer row belongs to the active/live card and may be
    /// reported to an application that enabled mouse mode.
    pub app_eligible: bool,
    /// End-exclusive column of the command/header hit span on this row.
    /// `0` means output body; `usize::MAX` means the full physical row.
    pub header_end_col: usize,
    /// First row of a user-bookmarked block. Drawn as a small amber notch in
    /// the gutter so bookmark state does not rely on badge space or color of
    /// the command outcome stripe.
    pub bookmarked: bool,
    /// Gutter stripe color for this row; `None` draws no stripe.
    pub stripe: Option<Color>,
    /// Draw the stripe wider and at full opacity (active selection edge).
    pub stripe_strong: bool,
    /// Viewport-local identity shared by every visible row of one card. This
    /// lets the renderer batch a card into one backdrop and one border rather
    /// than repainting a full rectangle once per terminal row.
    pub card_group: Option<usize>,
    pub card_kind: BlockCardKind,
    /// These name real terminal/card edges, not merely viewport clipping.
    /// A long card entering from above therefore gets no fake rounded top, and
    /// one leaving below gets no fake bottom edge.
    pub card_top: bool,
    pub card_bottom: bool,
    pub card_selected: bool,
    pub card_selection_active: bool,
    /// Legacy/fallback 1px prompt separator. Cards use their real top border;
    /// non-card rows may still opt into this line.
    pub separator: bool,
    /// Right-aligned first-row badge (text, color). Only set when the cells
    /// it covers are blank — the app checks before asking for it.
    pub badge: Option<(String, Color)>,
    /// Host-owned collapse affordance. Summary rows never participate in
    /// links, terminal text selection, cursor placement, or app mouse mode.
    pub collapsed_summary: Option<CollapsedSummaryPaint>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CollapsedSummaryPaint {
    pub key: SyntheticRowKey,
    pub hidden_display_rows: usize,
}

/// One contiguous visible slice of a card. `real_top`/`real_bottom` survive
/// viewport clipping so geometry never invents a rounded edge in the middle of
/// a long command's output.
#[derive(Clone, Copy, Debug, PartialEq)]
struct BlockCardSegment {
    group: usize,
    start_row: usize,
    end_row: usize,
    kind: BlockCardKind,
    real_top: bool,
    real_bottom: bool,
    selected: bool,
    selection_active: bool,
    stripe: Option<Color>,
    stripe_strong: bool,
}

/// Collect visible cards in O(visible rows). The app has already mapped block
/// zones to rows; draw/hover code must never rescan scrollback or zone history.
fn block_card_segments(rows: &[BlockPaintRow]) -> Vec<BlockCardSegment> {
    let mut segments = Vec::with_capacity(rows.len().min(32));
    let mut start = 0usize;
    while start < rows.len() {
        let Some(group) = rows[start].card_group else {
            start += 1;
            continue;
        };
        let mut end = start + 1;
        while end < rows.len() && rows[end].card_group == Some(group) {
            end += 1;
        }
        let first = &rows[start];
        let last = &rows[end - 1];
        segments.push(BlockCardSegment {
            group,
            start_row: start,
            end_row: end,
            kind: first.card_kind,
            real_top: first.card_top,
            real_bottom: last.card_bottom,
            selected: first.card_selected,
            selection_active: first.card_selection_active,
            stripe: first.stripe,
            stripe_strong: first.stripe_strong,
        });
        start = end;
    }
    segments
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct BlockCardGeometry {
    body: Rectangle,
    radius: border::Radius,
}

/// Paint-only card geometry. Terminal `Metrics`, column count and row count do
/// not depend on this function, which is the key invariant for a native iced
/// mapping of the multi-widget Anvil/Forge design.
fn block_card_geometry(
    bounds: Rectangle,
    content_right: f32,
    grid_origin_y: f32,
    cell_height: f32,
    segment: BlockCardSegment,
    compact: bool,
) -> BlockCardGeometry {
    let inset = if compact {
        BLOCK_CARD_COMPACT_INSET
    } else {
        BLOCK_CARD_INSET
    };
    let corner = if compact {
        BLOCK_CARD_COMPACT_RADIUS
    } else {
        BLOCK_CARD_RADIUS
    };
    let mut top = grid_origin_y + segment.start_row as f32 * cell_height;
    let mut bottom = grid_origin_y + segment.end_row as f32 * cell_height;
    if segment.real_top {
        top += BLOCK_CARD_EDGE_GAP;
    }
    if segment.real_bottom {
        bottom -= BLOCK_CARD_EDGE_GAP;
    }
    bottom = bottom.max(top);
    let left = bounds.x + inset;
    let right = (content_right - inset).max(left);
    let body = Rectangle {
        x: left,
        y: top,
        // Keep the global history scrollbar outside every card, like the
        // Anvil/Forge outer block scroller. `content_right` is its track's
        // left edge, already accounting for configured terminal padding.
        width: right - left,
        height: bottom - top,
    };
    BlockCardGeometry {
        body,
        radius: border::Radius {
            top_left: if segment.real_top { corner } else { 0.0 },
            top_right: if segment.real_top { corner } else { 0.0 },
            bottom_right: if segment.real_bottom { corner } else { 0.0 },
            bottom_left: if segment.real_bottom { corner } else { 0.0 },
        },
    }
}

fn block_card_stripe_bounds(
    widget_bounds: Rectangle,
    body: Rectangle,
    requested_width: f32,
    glyph_origin_x: f32,
) -> Rectangle {
    // The window resize overlay owns the outermost 5px. Start at or inside its
    // inner edge; a strong stripe may overlap the card body, but always keeps
    // a visible/clickable portion inside the existing 8px Block gutter.
    const WINDOW_RESIZE_EDGE: f32 = 5.0;
    let x = (body.x - requested_width).max(widget_bounds.x + WINDOW_RESIZE_EDGE);
    let width = (glyph_origin_x - x).clamp(0.0, requested_width.max(0.0));
    Rectangle {
        x,
        y: body.y,
        width,
        height: body.height,
    }
}

fn block_card_hover_contains(
    widget_bounds: Rectangle,
    body: Rectangle,
    pointer_x: f32,
    pointer_y: f32,
) -> bool {
    let left = (body.x - BLOCK_GUTTER_WIDTH).max(widget_bounds.x);
    let right = body.x + body.width;
    pointer_x >= left
        && pointer_x < right
        && pointer_y >= body.y
        && pointer_y < body.y + body.height
}

/// Rectangular pieces for a viewport-clipped card outline. A regular iced
/// `Quad` always paints all four border edges, so using it for a long card
/// would manufacture a horizontal cap at the viewport clip. The continuing
/// sides are always present; only real semantic top/bottom edges get caps.
fn clipped_block_card_border_bounds(
    body: Rectangle,
    real_top: bool,
    real_bottom: bool,
    requested_width: f32,
) -> [Option<Rectangle>; 4] {
    let width = requested_width
        .max(0.0)
        .min(body.width * 0.5)
        .min(body.height * 0.5);
    if width <= 0.0 {
        return [None; 4];
    }
    let left = Rectangle { width, ..body };
    let right = Rectangle {
        x: body.x + body.width - width,
        width,
        ..body
    };
    let top = Rectangle {
        height: width,
        ..body
    };
    let bottom = Rectangle {
        y: body.y + body.height - width,
        height: width,
        ..body
    };
    [
        Some(left),
        Some(right),
        real_top.then_some(top),
        real_bottom.then_some(bottom),
    ]
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct BlockCardVisual {
    background: Color,
    border: Color,
    border_width: f32,
    shadow: Shadow,
}

fn alpha(color: Color, value: f32) -> Color {
    Color {
        a: color.a * value.clamp(0.0, 1.0),
        ..color
    }
}

/// Shared card-state treatment expressed using theme colors instead of fixed
/// swatches. Alpha values mirror the Anvil/Forge CSS contract.
fn block_card_visual(
    segment: BlockCardSegment,
    foreground: Color,
    accent: Color,
    hovered: bool,
    opacity: f32,
) -> BlockCardVisual {
    let stripe = segment.stripe.unwrap_or(accent);
    let (background, border, border_width) = if segment.selection_active {
        (alpha(accent, 0.14), alpha(accent, 0.92), 2.0)
    } else if segment.selected {
        (alpha(accent, 0.08), alpha(accent, 0.48), 1.0)
    } else if hovered {
        (alpha(foreground, 0.05), alpha(foreground, 0.16), 1.0)
    } else {
        match segment.kind {
            BlockCardKind::Failed => (alpha(stripe, 0.11), alpha(foreground, 0.08), 1.0),
            BlockCardKind::Background => (alpha(accent, 0.07), alpha(accent, 0.24), 1.0),
            BlockCardKind::Active => (alpha(accent, 0.035), alpha(accent, 0.32), 1.0),
            BlockCardKind::Finished | BlockCardKind::Unknown => {
                (alpha(foreground, 0.03), alpha(foreground, 0.08), 1.0)
            }
        }
    };
    let shadow = if segment.kind == BlockCardKind::Active {
        Shadow {
            color: alpha(Color::BLACK, 0.18 * opacity),
            offset: Vector::new(0.0, 2.0),
            blur_radius: 8.0,
        }
    } else if hovered {
        Shadow {
            color: alpha(Color::BLACK, 0.22 * opacity),
            offset: Vector::new(0.0, 4.0),
            blur_radius: 14.0,
        }
    } else {
        Shadow::default()
    };
    BlockCardVisual {
        background: alpha(background, opacity),
        border,
        border_width,
        shadow,
    }
}

fn block_card_shadow(segment: BlockCardSegment, shadow: Shadow) -> Shadow {
    if segment.real_top && segment.real_bottom {
        shadow
    } else {
        // A shadow around a viewport-clipped rectangle would reintroduce the
        // same false horizontal cap that selective borders avoid.
        Shadow::default()
    }
}

fn hovered_link_color() -> Color {
    Color::from_rgb8(100, 200, 255)
}

/// Per-widget interaction state retained across frames.
struct State {
    dragging: bool,
    scrollbar_dragging: bool,
    /// Ordinary presses published to the app layer, independently per button.
    /// Right/middle do not arm
    /// text dragging, but their release must still follow the press across a
    /// pane boundary so mouse-reporting applications never get stuck buttons.
    published_presses: [bool; 3],
    /// Block presses are complete one-shot gestures. Remember every consumed
    /// button so interleaved releases cannot leak to a mouse-reporting app.
    consumed_presses: [bool; 3],
    last_click: Option<(Instant, usize, usize)>,
    click_count: u32,
    /// Fractional wheel lines not yet consumed, so sub-line trackpad pixel
    /// deltas accumulate into whole-line scrolls instead of being lost.
    scroll_accum: f32,
    summary_press: Option<SummaryPress>,
}

#[derive(Clone, Debug)]
struct SummaryPress {
    key: SyntheticRowKey,
    projection_key: ProjectionKey,
    point: Point,
    dragged: bool,
}

fn stable_summary_activation(
    press: SummaryPress,
    current: Option<CollapsedSummaryPaint>,
    current_projection: Option<&ProjectionKey>,
) -> Option<SummaryActivation> {
    (!press.dragged
        && current_projection == Some(&press.projection_key)
        && current.is_some_and(|summary| summary.key == press.key))
    .then_some(SummaryActivation {
        key: press.key,
        projection_key: press.projection_key,
    })
}

impl Default for State {
    fn default() -> Self {
        Self {
            dragging: false,
            scrollbar_dragging: false,
            published_presses: [false; 3],
            consumed_presses: [false; 3],
            last_click: None,
            click_count: 0,
            scroll_accum: 0.0,
            summary_press: None,
        }
    }
}

fn owns_mouse_release(
    published_presses: &[bool; 3],
    dragging: bool,
    scrollbar_dragging: bool,
    button: MouseButton,
) -> bool {
    published_presses[button.slot()]
        || (button == MouseButton::Left && (dragging || scrollbar_dragging))
}

/// Max gap between presses (ms) for them to count as a multi-click.
const MULTI_CLICK_MS: u128 = 400;

/// Pixel metrics for the terminal grid.
#[derive(Clone, Copy, Debug)]
pub struct Metrics {
    pub font_size: f32,
    pub cell_w: f32,
    pub cell_h: f32,
    pub padding: f32,
    pub block_gutter: bool,
    /// `cell_w` is the primary font's real measured advance, so a run of
    /// primary-font narrow ASCII glyphs shaped together lands exactly on the
    /// grid. False with the heuristic width: glyphs must be emitted per cell.
    pub mono_advance_exact: bool,
}

impl Metrics {
    pub fn new(font_size: f32, line_spacing: f32, padding: f32, block_gutter: bool) -> Self {
        let cell_w = (font_size * 0.6).max(1.0);
        let cell_h = (font_size * 1.2 * line_spacing).max(1.0);
        Metrics {
            font_size,
            cell_w,
            cell_h,
            padding,
            block_gutter,
            mono_advance_exact: false,
        }
    }

    /// Metrics whose cell width is the primary font's measured advance
    /// (cosmic-text, the same shaper iced renders with), which enables
    /// run-batched glyph emission. Falls back to the heuristic width — and
    /// per-cell emission — whenever the font cannot be measured or is not
    /// uniformly monospaced across printable ASCII.
    pub fn with_font(
        font: iced::Font,
        font_size: f32,
        line_spacing: f32,
        padding: f32,
        block_gutter: bool,
    ) -> Self {
        let mut metrics = Self::new(font_size, line_spacing, padding, block_gutter);
        if let Some(advance) = measure_mono_ascii_advance(font, font_size) {
            metrics.cell_w = advance;
            metrics.mono_advance_exact = true;
        }
        metrics
    }

    fn block_gutter_width(&self) -> f32 {
        if self.block_gutter {
            BLOCK_GUTTER_WIDTH
        } else {
            0.0
        }
    }

    /// Compute (cols, rows) that fit into the given pixel area.
    pub fn grid_size(&self, width: f32, height: f32) -> (usize, usize) {
        let usable_w = (width - self.padding * 2.0 - self.block_gutter_width()).max(0.0);
        let usable_h = (height - self.padding * 2.0).max(0.0);
        let cols = (usable_w / self.cell_w).floor() as usize;
        let rows = (usable_h / self.cell_h).floor() as usize;
        clamp_terminal_dimensions(cols.max(1), rows.max(1))
    }
}

/// A custom widget that renders a terminal grid snapshot using the advanced
/// renderer (quads for backgrounds/cursor, real text shaping for glyphs).
pub struct TermWidget<'a, Message> {
    grid: &'a [Vec<TerminalCell>],
    cursor: (usize, usize),
    cursor_visible: bool,
    cursor_shape: CursorShape,
    focused: bool,
    theme: &'a Theme,
    dynamic_palette: Option<&'a DynamicColorPalette>,
    dynamic_fg: Option<(u8, u8, u8)>,
    dynamic_bg: Option<(u8, u8, u8)>,
    dynamic_cursor: Option<(u8, u8, u8)>,
    metrics: Metrics,
    mono: iced::Font,
    cjk_mono: Option<iced::Font>,
    symbol_mono: Option<iced::Font>,
    math_symbol: Option<iced::Font>,
    nerd_symbol: Option<iced::Font>,
    /// Per visible row: the inclusive (start_col, end_col) span to highlight,
    /// or `None` for rows with no selection. `end_col` may exceed the row width.
    selection: Vec<Option<(usize, usize)>>,
    scroll_offset: usize,
    scrollback_len: usize,
    /// Per visible row block chrome (stripes, separators, badges), aligned
    /// with the grid rows exactly like `selection`. Empty = no block mode.
    blocks: Vec<BlockPaintRow>,
    /// Alternate screen and disabled Block Mode have no history-card split;
    /// every real grid cell is application-owned there.
    app_mouse_full_grid: bool,
    /// Paint-only density switch shared with Anvil/Forge. It changes card
    /// inset/radius but deliberately not [`Metrics`] or terminal dimensions.
    block_compact: bool,
    /// Scrollbar-track fractions (0 = buffer top) of failed blocks; each gets
    /// a small red marker on the track. Empty = no block mode / no failures.
    block_markers: Vec<f32>,
    /// Scrollbar positions of bookmarked blocks, painted amber beside failure
    /// markers so off-screen bookmarks remain visible and navigable.
    block_bookmark_markers: Vec<f32>,
    /// Search matches in visible-grid coordinates (line = grid row index).
    search_matches: Vec<SearchMatch>,
    /// Identity `(line, col_start)` of the active match, highlighted distinctly.
    current_match: Option<(usize, usize)>,
    shift: bool,
    alt: bool,
    ctrl: bool,
    on_mouse: Option<Box<dyn Fn(MouseInput) -> Message + 'a>>,
    on_summary: Option<Box<dyn Fn(SummaryActivation) -> Message + 'a>>,
    projection_key: Option<ProjectionKey>,
    /// Detected clickable links in visible-grid coordinates (line = grid row).
    links: &'a [crate::link::Link],
    link_revision: u64,
    /// Kitty-graphics placements to paint over the grid.
    images: Vec<KittyRender>,
    /// When false (Auto), the scrollbar is only drawn while scrolled up; when
    /// true (Always), it is drawn whenever scrollback exists.
    scrollbar_always: bool,
    /// Active IME pre-edit (composition) text plus the byte range the IME marks
    /// as its cursor/selection. Supplied to the runtime each redraw so it can
    /// paint the over-the-spot composition overlay at the terminal cursor.
    preedit: Option<(String, Option<std::ops::Range<usize>>)>,
    /// Current phase of the blink clock: when false, cells with the blink
    /// attribute hide their glyph (drawn as background only).
    blink_on: bool,
    /// Window background opacity (0.05..=1.0). Below 1.0 the widget's own
    /// default-background fill is skipped so the translucent app background
    /// shows through; non-default cell backgrounds stay opaque, like ember.
    opacity: f32,
}

impl<'a, Message> TermWidget<'a, Message> {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        grid: &'a [Vec<TerminalCell>],
        cursor: (usize, usize),
        cursor_visible: bool,
        cursor_shape: CursorShape,
        focused: bool,
        theme: &'a Theme,
        metrics: Metrics,
        mono: iced::Font,
        cjk_mono: Option<iced::Font>,
        symbol_mono: Option<iced::Font>,
        math_symbol: Option<iced::Font>,
        nerd_symbol: Option<iced::Font>,
        selection: Vec<Option<(usize, usize)>>,
        scroll_offset: usize,
        scrollback_len: usize,
    ) -> Self {
        TermWidget {
            grid,
            cursor,
            cursor_visible,
            cursor_shape,
            focused,
            theme,
            dynamic_palette: None,
            dynamic_fg: None,
            dynamic_bg: None,
            dynamic_cursor: None,
            metrics,
            mono,
            cjk_mono,
            symbol_mono,
            math_symbol,
            nerd_symbol,
            selection,
            scroll_offset,
            scrollback_len,
            blocks: Vec::new(),
            app_mouse_full_grid: false,
            block_compact: false,
            block_markers: Vec::new(),
            block_bookmark_markers: Vec::new(),
            search_matches: Vec::new(),
            current_match: None,
            shift: false,
            alt: false,
            ctrl: false,
            on_mouse: None,
            on_summary: None,
            projection_key: None,
            links: &[],
            link_revision: 0,
            images: Vec::new(),
            scrollbar_always: true,
            preedit: None,
            blink_on: true,
            opacity: 1.0,
        }
    }

    /// Set the window background opacity applied to the widget's default
    /// background fill.
    pub fn opacity(mut self, opacity: f32) -> Self {
        self.opacity = opacity;
        self
    }

    /// Set the blink clock phase (true = glyphs visible).
    pub fn blink_on(mut self, on: bool) -> Self {
        self.blink_on = on;
        self
    }

    /// Supply the active IME pre-edit so the runtime can render the composition
    /// overlay and keep the input method enabled while this pane is focused.
    pub fn preedit(mut self, preedit: Option<(String, Option<std::ops::Range<usize>>)>) -> Self {
        self.preedit = preedit;
        self
    }

    /// Set scrollbar visibility: `true` = always shown, `false` = auto (only
    /// while scrolled up).
    pub fn scrollbar_always(mut self, always: bool) -> Self {
        self.scrollbar_always = always;
        self
    }

    /// Supply detected links to color, underline, and make clickable.
    pub fn links(mut self, links: &'a [crate::link::Link], revision: u64) -> Self {
        self.links = links;
        self.link_revision = revision;
        self
    }

    pub fn dynamic_palette(mut self, palette: &'a DynamicColorPalette) -> Self {
        self.dynamic_palette = Some(palette);
        self
    }

    /// Supply OSC 10/11/12 overrides for the default foreground, background,
    /// and cursor colors.
    pub fn dynamic_defaults(
        mut self,
        foreground: Option<(u8, u8, u8)>,
        background: Option<(u8, u8, u8)>,
        cursor: Option<(u8, u8, u8)>,
    ) -> Self {
        self.dynamic_fg = foreground;
        self.dynamic_bg = background;
        self.dynamic_cursor = cursor;
        self
    }

    /// Supply Kitty-graphics placements to paint over the grid.
    pub fn images(mut self, images: Vec<KittyRender>) -> Self {
        self.images = images;
        self
    }

    /// Find the link covering a given (col, row) cell, if any.
    fn link_at(&self, col: usize, row: usize) -> Option<&crate::link::Link> {
        self.links.iter().find(|l| {
            l.line == row
                && col >= l.col_start
                && col < l.col_end
                && match l.link_type {
                    crate::link::LinkType::Url => crate::link::is_openable_url(&l.text),
                    crate::link::LinkType::FilePath | crate::link::LinkType::IpAddress => true,
                }
        })
    }

    /// Resolve pointer hover through the same real-grid and Block Mode surface
    /// rules used by press handling, so the hand cursor/highlight never
    /// promises an activation the application will refuse.
    fn link_at_position(&self, position: Point, bounds: Rectangle) -> Option<&crate::link::Link> {
        let (col, row) = self.cell_at(position, bounds);
        let before_scrollbar = self
            .scrollbar_geometry(bounds)
            .is_none_or(|(_, _, scrollbar_x, _, _)| position.x < scrollbar_x);
        let grid_top = bounds.y + self.metrics.padding;
        let grid_bottom = grid_top + self.grid.len() as f32 * self.metrics.cell_h;
        let grid_left = bounds.x + self.metrics.padding + self.metrics.block_gutter_width();
        let grid_width = self
            .grid
            .first()
            .map_or(0.0, |cells| cells.len() as f32 * self.metrics.cell_w);
        let block = self.blocks.get(row);
        let over_real_grid = before_scrollbar
            && position.y >= grid_top
            && position.y < grid_bottom
            && position.x >= grid_left
            && position.x < grid_left + grid_width;
        let eligible = block.is_none_or(|block| block.collapsed_summary.is_none())
            && link_surface_eligible(
                over_real_grid,
                block.is_some_and(|block| block.selectable),
                block.is_some_and(|block| col < block.header_end_col),
            );
        eligible.then(|| self.link_at(col, row)).flatten()
    }

    /// Supply search matches (and the active match identity) to highlight.
    pub fn search(mut self, matches: Vec<SearchMatch>, current: Option<(usize, usize)>) -> Self {
        self.search_matches = matches;
        self.current_match = current;
        self
    }

    /// Supply per-visible-row block chrome (empty disables block painting).
    pub fn blocks(mut self, blocks: Vec<BlockPaintRow>) -> Self {
        self.blocks = blocks;
        self
    }

    pub fn app_mouse_full_grid(mut self, full_grid: bool) -> Self {
        self.app_mouse_full_grid = full_grid;
        self
    }

    pub fn block_compact(mut self, compact: bool) -> Self {
        self.block_compact = compact;
        self
    }

    /// Supply failed-block positions as fractions along the scrollbar track
    /// (block mode); empty draws no markers.
    pub fn block_markers(mut self, markers: Vec<f32>) -> Self {
        self.block_markers = markers;
        self
    }

    pub fn block_bookmark_markers(mut self, markers: Vec<f32>) -> Self {
        self.block_bookmark_markers = markers;
        self
    }

    /// Scrollbar track + thumb geometry, or `None` when there is nothing to
    /// scroll. Returns `(track_top, track_h, x, thumb_y, thumb_h)`.
    fn scrollbar_geometry(&self, bounds: Rectangle) -> Option<(f32, f32, f32, f32, f32)> {
        if self.scrollback_len == 0 {
            return None;
        }
        // Auto mode: only reveal the scrollbar while scrolled up into history.
        if !self.scrollbar_always && self.scroll_offset == 0 {
            return None;
        }
        let pad = self.metrics.padding;
        let rows = self.grid.len();
        let total = self.scrollback_len + rows;
        if total == 0 {
            return None;
        }
        let track_top = bounds.y + pad;
        let track_h = (bounds.height - pad * 2.0).max(1.0);
        let x = bounds.x + bounds.width - pad - SCROLLBAR_WIDTH;
        let thumb_h = ((rows as f32 / total as f32) * track_h)
            .clamp(SCROLLBAR_MIN_THUMB.min(track_h), track_h);
        // offset == 0 → thumb at bottom (live view); offset == max → top.
        let frac = self.scroll_offset as f32 / self.scrollback_len as f32;
        let thumb_y = track_top + (track_h - thumb_h) * (1.0 - frac);
        Some((track_top, track_h, x, thumb_y, thumb_h))
    }

    /// Map a pointer y-coordinate (centering the thumb on the cursor) to an
    /// absolute scrollback offset.
    fn offset_from_y(&self, y: f32, bounds: Rectangle) -> usize {
        let Some((track_top, track_h, _, _, thumb_h)) = self.scrollbar_geometry(bounds) else {
            return 0;
        };
        let usable = (track_h - thumb_h).max(1.0);
        let rel = (y - track_top - thumb_h / 2.0).clamp(0.0, usable);
        let frac = 1.0 - rel / usable;
        (frac * self.scrollback_len as f32).round() as usize
    }

    /// Register a callback that maps grid mouse interactions to messages.
    pub fn on_mouse(mut self, f: impl Fn(MouseInput) -> Message + 'a) -> Self {
        self.on_mouse = Some(Box::new(f));
        self
    }

    /// Register the stable release-time action for a collapsed-output summary.
    pub fn on_summary(
        mut self,
        projection_key: ProjectionKey,
        f: impl Fn(SummaryActivation) -> Message + 'a,
    ) -> Self {
        self.projection_key = Some(projection_key);
        self.on_summary = Some(Box::new(f));
        self
    }

    /// Supply the keyboard modifier state tracked by the application, used to
    /// distinguish selection (shift) and block-selection (alt) from app mouse
    /// reporting.
    pub fn modifiers(mut self, shift: bool, alt: bool, ctrl: bool) -> Self {
        self.shift = shift;
        self.alt = alt;
        self.ctrl = ctrl;
        self
    }

    /// Convert an absolute pixel position into a clamped 0-indexed (col, row).
    fn cell_at(&self, pos: Point, bounds: Rectangle) -> (usize, usize) {
        let pad = self.metrics.padding;
        let cw = self.metrics.cell_w.max(1.0);
        let ch = self.metrics.cell_h.max(1.0);
        let rel_x = (pos.x - bounds.x - pad - self.metrics.block_gutter_width()).max(0.0);
        let rel_y = (pos.y - bounds.y - pad).max(0.0);
        let max_row = self.grid.len().saturating_sub(1);
        let max_col = self
            .grid
            .first()
            .map(|r| r.len())
            .unwrap_or(1)
            .saturating_sub(1);
        let col = ((rel_x / cw) as usize).min(max_col);
        let row = ((rel_y / ch) as usize).min(max_row);
        (col, row)
    }
}

fn should_use_symbol_fallback_font(ch: char) -> bool {
    matches!(
        ch as u32,
        0x2190..=0x21FF
            | 0x2200..=0x22FF
            | 0x2300..=0x23FF
            | 0x2500..=0x259F
            | 0x25A0..=0x25FF
            | 0x2600..=0x26FF
            | 0x2700..=0x27BF
            | 0x27C0..=0x27FF
            | 0x2800..=0x28FF
            | 0x2900..=0x2AFF
            | 0x2B00..=0x2BFF
    )
}

fn should_use_math_symbol_fallback_font(ch: char) -> bool {
    matches!(ch as u32, 0x1D400..=0x1D7FF)
}

fn should_use_nerd_symbol_fallback_font(ch: char) -> bool {
    matches!(ch as u32, 0xE000..=0xF8FF | 0xF0000..=0xFFFFD | 0x100000..=0x10FFFD)
}

fn should_use_cjk_fallback_font(ch: char) -> bool {
    matches!(
        ch as u32,
        0x2E80..=0x2EFF
            | 0x3000..=0x303F
            | 0x3040..=0x30FF
            | 0x3100..=0x312F
            | 0x3130..=0x318F
            | 0x31A0..=0x31BF
            | 0x31C0..=0x31EF
            | 0x31F0..=0x31FF
            | 0x3400..=0x4DBF
            | 0x4E00..=0x9FFF
            | 0xAC00..=0xD7AF
            | 0xF900..=0xFAFF
            | 0xFF00..=0xFFEF
            | 0x20000..=0x2FA1F
    )
}

/// The primary font's advance width for printable ASCII at `font_size`,
/// measured through cosmic-text — the same library and version iced's
/// renderers shape with. Returns `None` when the font cannot be resolved or
/// the per-glyph advances are not exactly uniform: the caller then keeps the
/// heuristic cell width and per-cell glyph emission, so a mismatched
/// measurement can never shift the grid.
fn measure_mono_ascii_advance(font: iced::Font, font_size: f32) -> Option<f32> {
    use cosmic_text::{
        Attrs, Buffer, Family, Metrics as TextMetrics, Shaping, Stretch, Style, Weight,
    };

    if !(font_size.is_finite() && font_size > 0.0) {
        return None;
    }
    let family = match font.family {
        iced::font::Family::Name(name) => Family::Name(name),
        iced::font::Family::Serif => Family::Serif,
        iced::font::Family::SansSerif => Family::SansSerif,
        iced::font::Family::Cursive => Family::Cursive,
        iced::font::Family::Fantasy => Family::Fantasy,
        iced::font::Family::Monospace => Family::Monospace,
    };
    let weight = match font.weight {
        iced::font::Weight::Thin => 100,
        iced::font::Weight::ExtraLight => 200,
        iced::font::Weight::Light => 300,
        iced::font::Weight::Normal => 400,
        iced::font::Weight::Medium => 500,
        iced::font::Weight::Semibold => 600,
        iced::font::Weight::Bold => 700,
        iced::font::Weight::ExtraBold => 800,
        iced::font::Weight::Black => 900,
    };
    let stretch = match font.stretch {
        iced::font::Stretch::UltraCondensed => Stretch::UltraCondensed,
        iced::font::Stretch::ExtraCondensed => Stretch::ExtraCondensed,
        iced::font::Stretch::Condensed => Stretch::Condensed,
        iced::font::Stretch::SemiCondensed => Stretch::SemiCondensed,
        iced::font::Stretch::Normal => Stretch::Normal,
        iced::font::Stretch::SemiExpanded => Stretch::SemiExpanded,
        iced::font::Stretch::Expanded => Stretch::Expanded,
        iced::font::Stretch::ExtraExpanded => Stretch::ExtraExpanded,
        iced::font::Stretch::UltraExpanded => Stretch::UltraExpanded,
    };
    let style = match font.style {
        iced::font::Style::Normal => Style::Normal,
        iced::font::Style::Italic => Style::Italic,
        iced::font::Style::Oblique => Style::Oblique,
    };

    thread_local! {
        // FontSystem::new scans the system font database once; reuse it
        // across font-size zooms. It lives on the UI thread, like iced's own.
        static FONT_SYSTEM: std::cell::RefCell<Option<cosmic_text::FontSystem>> =
            const { std::cell::RefCell::new(None) };
    }
    FONT_SYSTEM.with(|slot| {
        let mut slot = slot.borrow_mut();
        let font_system = slot.get_or_insert_with(cosmic_text::FontSystem::new);
        let mut buffer = Buffer::new(font_system, TextMetrics::new(font_size, font_size));
        buffer.set_size(font_system, Some(4096.0), Some(font_size * 2.0));
        let attrs = Attrs::new()
            .family(family)
            .weight(Weight(weight))
            .stretch(stretch)
            .style(style);
        // Printable ASCII in one Basic-shaping pass: advances there are raw
        // hmtx advances (no kerning or ligatures), identical to what iced's
        // Basic shaper produces for the same run.
        const PROBE: &str = "!\"#$%&'()*+,-./0123456789:;<=>?@ABCDEFGHIJKLMNOPQRSTUVWXYZ[\\]^_`abcdefghijklmnopqrstuvwxyz{|}~";
        buffer.set_text(font_system, PROBE, &attrs, Shaping::Basic, None);
        buffer.shape_until_scroll(font_system, false);
        let mut advance: Option<f32> = None;
        let mut glyphs = 0usize;
        for run in buffer.layout_runs() {
            for glyph in run.glyphs {
                if !(glyph.w.is_finite() && glyph.w > 0.0) {
                    return None;
                }
                if advance.is_some_and(|a| a != glyph.w) {
                    return None;
                }
                advance = Some(glyph.w);
                glyphs += 1;
            }
        }
        let advance = advance.filter(|_| glyphs == PROBE.chars().count())?;
        // Plausibility bound: a real monospace advance sits well inside
        // [0.3, 1.0] of the em size. Anything else means a different face was
        // resolved than the renderer will use; do not let it shift the grid.
        (advance >= font_size * 0.3 && advance <= font_size).then_some(advance)
    })
}

/// One narrow glyph may join the pending run only when the cell width IS the
/// primary font's measured advance: a shared shape pass then lands every
/// glyph exactly on its column. Fallback fonts (italics included) and
/// non-ASCII cells keep per-cell emission. Selection and inverse-video enter
/// through `fg`, so run-flush on `fg` change covers those boundaries;
/// backgrounds are painted per cell in their own pass and never break a run.
fn glyph_joins_run(
    metrics: Metrics,
    glyph: char,
    glyph_font: iced::Font,
    primary: iced::Font,
) -> bool {
    metrics.mono_advance_exact && glyph.is_ascii() && glyph_font == primary
}

fn terminal_glyph_font(
    ch: char,
    primary: iced::Font,
    cjk: Option<iced::Font>,
    symbol: Option<iced::Font>,
    math_symbol: Option<iced::Font>,
    nerd_symbol: Option<iced::Font>,
    italic: bool,
) -> iced::Font {
    if should_use_nerd_symbol_fallback_font(ch) {
        nerd_symbol.unwrap_or_else(|| symbol.unwrap_or(iced::Font::MONOSPACE))
    } else if should_use_math_symbol_fallback_font(ch) {
        math_symbol.unwrap_or_else(|| symbol.unwrap_or(iced::Font::MONOSPACE))
    } else if should_use_symbol_fallback_font(ch) {
        symbol.unwrap_or(iced::Font::MONOSPACE)
    } else if should_use_cjk_fallback_font(ch) {
        cjk.unwrap_or(primary)
    } else if italic {
        iced::Font {
            style: iced::font::Style::Italic,
            ..primary
        }
    } else {
        primary
    }
}

/// Basic shaping never falls back to another font, so any glyph the routed
/// family lacks renders as the .notdef box. Non-ASCII cells use Advanced
/// shaping, which searches every loaded system font for the glyph; ASCII
/// stays on the cheaper Basic shaper.
fn glyph_shaping(content: &str) -> text::Shaping {
    if content.is_ascii() {
        text::Shaping::Basic
    } else {
        text::Shaping::Advanced
    }
}

fn solid_quad(bounds: Rectangle) -> Quad {
    Quad {
        bounds,
        border: Border::default(),
        shadow: Shadow::default(),
        snap: true,
    }
}

#[cfg(test)]
#[allow(clippy::items_after_test_module)]
mod tests {
    use super::{
        app_mouse_surface_eligible, block_card_geometry, block_card_hover_contains,
        block_card_segments, block_card_shadow, block_card_stripe_bounds, block_card_visual,
        block_mouse_action, clipped_block_card_border_bounds, ctrl_link_eligible, glyph_joins_run,
        glyph_shaping, link_surface_eligible, owns_mouse_release, should_use_cjk_fallback_font,
        should_use_math_symbol_fallback_font, should_use_nerd_symbol_fallback_font,
        should_use_symbol_fallback_font, stable_summary_activation, terminal_glyph_font,
        BlockCardKind, BlockCardSegment, BlockMouseAction, BlockPaintRow, CollapsedSummaryPaint,
        Metrics, MouseButton, SummaryPress, BLOCK_CARD_COMPACT_INSET, BLOCK_CARD_COMPACT_RADIUS,
        BLOCK_CARD_INSET, BLOCK_CARD_RADIUS, BLOCK_GUTTER_WIDTH,
    };
    use iced::{Color, Point, Rectangle};

    #[test]
    fn collapsed_summary_activation_requires_same_key_projection_and_no_drag() {
        let synthetic = crate::terminal::SyntheticRowKey {
            zone_id: 7,
            policy_revision: 3,
        };
        let projection = crate::terminal::ProjectionKey {
            source: crate::terminal::ProjectionSourceRevision {
                grid: 1,
                history: 2,
                row_identity: 3,
                alternate_screen: false,
            },
            scroll_offset: 0,
            rows: 4,
            cols: 20,
            mode: crate::terminal::ProjectionMode::Transformed,
            policy_revision: 3,
            policy_ids: std::sync::Arc::from([7]),
            document_rows: 8,
        };
        let paint = CollapsedSummaryPaint {
            key: synthetic,
            hidden_display_rows: 4,
        };
        let press = |dragged| SummaryPress {
            key: synthetic,
            projection_key: projection.clone(),
            point: Point::ORIGIN,
            dragged,
        };

        assert!(stable_summary_activation(press(false), Some(paint), Some(&projection)).is_some());
        assert!(stable_summary_activation(press(true), Some(paint), Some(&projection)).is_none());
        let mut stale_projection = projection.clone();
        stale_projection.scroll_offset = 1;
        assert!(
            stable_summary_activation(press(false), Some(paint), Some(&stale_projection)).is_none()
        );
        assert!(stable_summary_activation(
            press(false),
            Some(CollapsedSummaryPaint {
                key: crate::terminal::SyntheticRowKey {
                    zone_id: 8,
                    policy_revision: 3,
                },
                hidden_display_rows: 4,
            }),
            Some(&projection),
        )
        .is_none());
    }

    #[test]
    fn block_card_press_contract_preserves_native_text_gestures() {
        let action = |button, selectable, header, shift, ctrl, count| {
            block_mouse_action(button, selectable, header, shift, ctrl, count)
        };

        assert_eq!(
            action(MouseButton::Left, true, true, false, false, 1),
            Some(BlockMouseAction::Replace)
        );
        // Ctrl alone retains header selection semantics; in output it remains
        // available to native link/text handling.
        assert_eq!(
            action(MouseButton::Left, true, true, false, true, 1),
            Some(BlockMouseAction::Replace)
        );
        assert_eq!(action(MouseButton::Left, true, false, false, true, 1), None);
        assert_eq!(
            action(MouseButton::Left, true, false, true, false, 1),
            Some(BlockMouseAction::Range)
        );
        assert_eq!(
            action(MouseButton::Left, true, false, true, true, 1),
            Some(BlockMouseAction::Toggle)
        );
        assert_eq!(
            action(MouseButton::Right, true, false, false, false, 1),
            Some(BlockMouseAction::Menu)
        );
        assert_eq!(
            action(MouseButton::Middle, true, true, false, false, 1),
            None
        );
        // Double/triple click always remains native, including on the header.
        assert_eq!(action(MouseButton::Left, true, true, false, false, 2), None);
        assert_eq!(action(MouseButton::Left, true, true, true, true, 3), None);
        // Live/running/alternate-screen rows never become block targets.
        assert_eq!(
            action(MouseButton::Right, false, true, false, false, 1),
            None
        );
    }

    #[test]
    fn link_gesture_requires_ctrl_single_left_and_a_real_link_surface() {
        assert!(ctrl_link_eligible(
            MouseButton::Left,
            1,
            true,
            false,
            link_surface_eligible(true, false, false),
        ));
        assert!(!ctrl_link_eligible(
            MouseButton::Left,
            1,
            true,
            false,
            link_surface_eligible(false, false, false),
        ));
        assert!(!ctrl_link_eligible(
            MouseButton::Left,
            1,
            true,
            false,
            link_surface_eligible(true, true, true),
        ));
        assert!(!ctrl_link_eligible(MouseButton::Left, 2, true, false, true,));
        assert!(!ctrl_link_eligible(
            MouseButton::Right,
            1,
            true,
            false,
            true,
        ));
        assert!(!ctrl_link_eligible(MouseButton::Left, 1, true, true, true,));
    }

    #[test]
    fn interleaved_buttons_own_their_independent_release_outside_the_pane() {
        let mut published = [false; 3];
        for button in [MouseButton::Left, MouseButton::Right, MouseButton::Middle] {
            published[button.slot()] = true;
        }
        for button in [MouseButton::Right, MouseButton::Left, MouseButton::Middle] {
            assert!(owns_mouse_release(&published, false, false, button));
            published[button.slot()] = false;
        }
        assert_eq!(published, [false; 3]);

        let none = [false; 3];
        assert!(owns_mouse_release(&none, false, true, MouseButton::Left,));
        assert!(!owns_mouse_release(
            &none,
            false,
            false,
            MouseButton::Middle,
        ));
        assert!(!owns_mouse_release(&none, false, false, MouseButton::Right,));
    }

    #[test]
    fn app_wheel_requires_live_grid_rows_and_columns() {
        // Primary-screen history is eligible only on the explicitly active
        // card row. Pre-zone scrollback is neither finalized nor app-owned.
        assert!(app_mouse_surface_eligible(true, true, false, true));
        assert!(!app_mouse_surface_eligible(true, true, false, false));
        // Padding and the scrollbar/right-side non-grid strip stay local.
        assert!(!app_mouse_surface_eligible(false, true, false, true));
        assert!(!app_mouse_surface_eligible(true, false, false, true));
        // Alternate/non-Block mode owns every *real* grid cell, not padding.
        assert!(app_mouse_surface_eligible(true, true, true, false));
        assert!(!app_mouse_surface_eligible(true, false, true, false));
    }

    #[test]
    fn block_gutter_is_reserved_before_column_zero() {
        let with_gutter = Metrics::new(10.0, 1.0, 2.0, true);
        let without_gutter = Metrics::new(10.0, 1.0, 2.0, false);
        assert_eq!(with_gutter.block_gutter_width(), BLOCK_GUTTER_WIDTH);
        assert_eq!(without_gutter.block_gutter_width(), 0.0);

        // 72px = 4px padding + 8px gutter + ten 6px cells. Turning the
        // gutter off recovers exactly one additional cell without overlap.
        assert_eq!(with_gutter.grid_size(72.0, 20.0).0, 10);
        assert_eq!(without_gutter.grid_size(72.0, 20.0).0, 11);
    }

    #[test]
    fn card_segments_batch_rows_and_preserve_real_viewport_edges() {
        let mut rows = vec![BlockPaintRow::default(); 6];
        for row in &mut rows[..3] {
            row.card_group = Some(4);
            row.card_kind = BlockCardKind::Failed;
            row.card_selected = true;
            row.stripe = Some(Color::from_rgb8(200, 40, 30));
        }
        // This card entered from above, so its first visible row is not a real
        // top. Its bottom is retained inside the viewport.
        rows[2].card_bottom = true;
        for row in &mut rows[4..] {
            row.card_group = Some(9);
            row.card_kind = BlockCardKind::Active;
            row.stripe = Some(Color::from_rgb8(80, 180, 255));
        }
        rows[4].card_top = true;
        rows[5].card_bottom = true;

        let segments = block_card_segments(&rows);
        assert_eq!(segments.len(), 2, "one draw segment per visible card");
        assert_eq!((segments[0].start_row, segments[0].end_row), (0, 3));
        assert!(!segments[0].real_top, "viewport clip is not a card top");
        assert!(segments[0].real_bottom);
        assert!(segments[0].selected);
        assert_eq!(segments[1].kind, BlockCardKind::Active);
        assert!(segments[1].real_top);
        assert!(segments[1].real_bottom, "live card seals at grid bottom");
    }

    fn segment(real_top: bool, real_bottom: bool) -> BlockCardSegment {
        BlockCardSegment {
            group: 1,
            start_row: 2,
            end_row: 5,
            kind: BlockCardKind::Finished,
            real_top,
            real_bottom,
            selected: false,
            selection_active: false,
            stripe: Some(Color::from_rgb8(10, 200, 40)),
            stripe_strong: false,
        }
    }

    #[test]
    fn card_geometry_is_paint_only_clipped_and_clear_of_scrollbar() {
        let bounds = Rectangle {
            x: 10.0,
            y: 4.0,
            width: 300.0,
            height: 180.0,
        };
        // Track begins at x=288 after right padding; cards must end before it.
        let normal = block_card_geometry(bounds, 288.0, 6.0, 10.0, segment(false, true), false);
        assert_eq!(normal.body.x, bounds.x + BLOCK_CARD_INSET);
        assert_eq!(normal.body.y, 26.0, "clipped top gets no synthetic gap");
        assert_eq!(normal.body.y + normal.body.height, 55.0);
        assert_eq!(normal.body.x + normal.body.width, 288.0 - BLOCK_CARD_INSET);
        assert!(normal.body.x + normal.body.width <= 288.0);
        assert_eq!(normal.radius.top_left, 0.0);
        assert_eq!(normal.radius.top_right, 0.0);
        assert_eq!(normal.radius.bottom_left, BLOCK_CARD_RADIUS);
        assert_eq!(normal.radius.bottom_right, BLOCK_CARD_RADIUS);

        let clipped_both =
            block_card_geometry(bounds, 288.0, 6.0, 10.0, segment(false, false), false);
        assert_eq!(
            clipped_both.body.y + clipped_both.body.height,
            56.0,
            "clipped bottom gets no synthetic gap"
        );
        assert_eq!(clipped_both.radius.bottom_left, 0.0);
        assert_eq!(clipped_both.radius.bottom_right, 0.0);

        let compact = block_card_geometry(bounds, 288.0, 6.0, 10.0, segment(true, true), true);
        assert_eq!(compact.body.x, bounds.x + BLOCK_CARD_COMPACT_INSET);
        assert_eq!(compact.radius.top_left, BLOCK_CARD_COMPACT_RADIUS);
        assert_eq!(compact.radius.bottom_right, BLOCK_CARD_COMPACT_RADIUS);

        // Density changes card paint only; the PTY/grid calculation stays the
        // one Metrics value used by both modes.
        let metrics = Metrics::new(10.0, 1.0, 2.0, true);
        assert_eq!(metrics.grid_size(300.0, 180.0), (48, 14));
    }

    #[test]
    fn card_stripe_stays_inside_resize_safe_clickable_gutter() {
        let widget = Rectangle {
            x: 0.0,
            y: 0.0,
            width: 300.0,
            height: 100.0,
        };
        let body = Rectangle {
            x: BLOCK_CARD_INSET,
            y: 1.0,
            width: 260.0,
            height: 40.0,
        };
        let normal = block_card_stripe_bounds(widget, body, 3.0, BLOCK_GUTTER_WIDTH);
        assert_eq!(normal.x, 5.0, "stripe clears the 5px resize grip");
        assert_eq!(normal.x + normal.width, body.x);
        assert!(normal.x < BLOCK_GUTTER_WIDTH);

        let strong = block_card_stripe_bounds(widget, body, 5.0, BLOCK_GUTTER_WIDTH + 2.0);
        assert_eq!(strong.x, 5.0);
        assert!(strong.x < BLOCK_GUTTER_WIDTH);
        assert!(strong.x + strong.width > body.x);

        // Zero terminal padding puts column zero exactly at the 8px gutter
        // edge. Even the strong stripe must shrink instead of covering it.
        let zero_padding = block_card_stripe_bounds(widget, body, 5.0, BLOCK_GUTTER_WIDTH);
        assert_eq!(zero_padding.x + zero_padding.width, BLOCK_GUTTER_WIDTH);
    }

    #[test]
    fn card_hover_excludes_scrollbar_and_right_inset() {
        let widget = Rectangle {
            x: 0.0,
            y: 0.0,
            width: 300.0,
            height: 100.0,
        };
        let body = Rectangle {
            x: 8.0,
            y: 10.0,
            width: 270.0,
            height: 40.0,
        };
        assert!(block_card_hover_contains(widget, body, 4.0, 20.0));
        assert!(block_card_hover_contains(widget, body, 277.0, 20.0));
        assert!(!block_card_hover_contains(widget, body, 278.0, 20.0));
        assert!(!block_card_hover_contains(widget, body, 290.0, 20.0));
    }

    #[test]
    fn clipped_card_border_has_only_real_horizontal_caps() {
        let body = Rectangle {
            x: 8.0,
            y: 10.0,
            width: 120.0,
            height: 60.0,
        };
        let clipped = clipped_block_card_border_bounds(body, false, true, 2.0);
        assert!(clipped[0].is_some() && clipped[1].is_some());
        assert!(clipped[2].is_none(), "viewport top is not a card cap");
        assert_eq!(clipped[3].expect("real bottom").y, 68.0);

        let continuing = clipped_block_card_border_bounds(body, false, false, 1.0);
        assert!(continuing[0].is_some() && continuing[1].is_some());
        assert!(continuing[2].is_none() && continuing[3].is_none());
    }

    #[test]
    fn card_visuals_follow_theme_relative_state_precedence() {
        let fg = Color::from_rgb8(220, 220, 220);
        let accent = Color::from_rgb8(80, 180, 255);
        let red = Color::from_rgb8(205, 49, 49);

        let mut failed_segment = segment(true, true);
        failed_segment.kind = BlockCardKind::Failed;
        failed_segment.stripe = Some(red);
        let failed = block_card_visual(failed_segment, fg, accent, false, 1.0);
        assert_eq!(
            (
                failed.background.r,
                failed.background.g,
                failed.background.b
            ),
            (red.r, red.g, red.b)
        );
        assert!((failed.background.a - 0.11).abs() < f32::EPSILON);

        let mut background_segment = segment(true, true);
        background_segment.kind = BlockCardKind::Background;
        background_segment.stripe = Some(accent);
        let background = block_card_visual(background_segment, fg, accent, false, 1.0);
        assert_eq!(
            (background.background.r, background.background.g),
            (accent.r, accent.g)
        );
        assert!((background.background.a - 0.07).abs() < f32::EPSILON);

        let mut active_segment = segment(true, true);
        active_segment.kind = BlockCardKind::Active;
        active_segment.stripe = Some(accent);
        let active = block_card_visual(active_segment, fg, accent, false, 1.0);
        assert!((active.border.a - 0.32).abs() < f32::EPSILON);
        assert!(active.shadow.blur_radius > 0.0);

        failed_segment.selected = true;
        failed_segment.selection_active = true;
        let selected = block_card_visual(failed_segment, fg, accent, true, 1.0);
        assert_eq!(selected.border_width, 2.0);
        assert!((selected.border.a - 0.92).abs() < f32::EPSILON);
        assert_eq!(
            (selected.background.r, selected.background.b),
            (accent.r, accent.b)
        );

        assert!(block_card_shadow(segment(true, true), active.shadow).blur_radius > 0.0);
        assert_eq!(
            block_card_shadow(segment(false, true), active.shadow).blur_radius,
            0.0,
            "viewport clipping must not manufacture a horizontal shadow cap"
        );
    }

    #[test]
    fn non_ascii_glyphs_use_advanced_shaping_for_font_fallback() {
        use iced::advanced::text::Shaping;
        // ASCII stays on the fast path.
        assert_eq!(glyph_shaping("A"), Shaping::Basic);
        assert_eq!(glyph_shaping("ls -la"), Shaping::Basic);
        // Symbols the routed fallback font may lack (e.g. U+23BF `⎿` is
        // missing from DejaVu Sans Mono) need Advanced shaping so
        // cosmic-text can fall back across the system font database.
        assert_eq!(glyph_shaping("⎿"), Shaping::Advanced);
        assert_eq!(glyph_shaping("※"), Shaping::Advanced);
        assert_eq!(glyph_shaping("中"), Shaping::Advanced);
    }

    #[test]
    fn terminal_symbols_use_fallback_font() {
        assert!(should_use_symbol_fallback_font('⌃'));
        assert!(should_use_symbol_fallback_font('⌅'));
        assert!(should_use_symbol_fallback_font('⋮'));
        assert!(should_use_symbol_fallback_font('─'));
        assert!(should_use_symbol_fallback_font('☰'));
        assert!(should_use_symbol_fallback_font('✓'));
        assert!(should_use_symbol_fallback_font('⟂'));
        assert!(should_use_symbol_fallback_font('⮕'));
        assert!(should_use_symbol_fallback_font('⣿'));
        assert!(!should_use_symbol_fallback_font('𝟏'));
        assert!(!should_use_symbol_fallback_font('中'));
        assert!(!should_use_symbol_fallback_font('A'));
    }

    #[test]
    fn math_alphanumeric_symbols_use_math_fallback_font() {
        assert!(should_use_math_symbol_fallback_font('𝟏'));
        assert!(should_use_math_symbol_fallback_font('𝟘'));
        assert!(should_use_math_symbol_fallback_font('𝐀'));
        assert!(!should_use_math_symbol_fallback_font('1'));
        assert!(!should_use_math_symbol_fallback_font('中'));
    }

    #[test]
    fn private_use_symbols_use_nerd_fallback_font() {
        assert!(should_use_nerd_symbol_fallback_font('\u{e0b0}'));
        assert!(should_use_nerd_symbol_fallback_font('\u{f0131}'));
        assert!(!should_use_nerd_symbol_fallback_font('𝟏'));
        assert!(!should_use_nerd_symbol_fallback_font('中'));
        assert!(!should_use_nerd_symbol_fallback_font('A'));
    }

    #[test]
    fn cjk_uses_cjk_fallback_font() {
        assert!(should_use_cjk_fallback_font('中'));
        assert!(should_use_cjk_fallback_font('あ'));
        assert!(!should_use_cjk_fallback_font('⌃'));
        assert!(!should_use_cjk_fallback_font('A'));
    }

    #[test]
    fn symbol_font_is_preferred_for_terminal_symbols() {
        let primary = iced::Font::with_name("Primary");
        let cjk = iced::Font::with_name("Cjk");
        let symbol = iced::Font::with_name("Symbol");
        let math = iced::Font::with_name("Math");
        let nerd = iced::Font::with_name("Nerd");
        let font_for = |ch, italic| {
            terminal_glyph_font(
                ch,
                primary,
                Some(cjk),
                Some(symbol),
                Some(math),
                Some(nerd),
                italic,
            )
        };

        assert_eq!(font_for('⌃', false), symbol);
        assert_eq!(font_for('⋮', false), symbol);
        assert_eq!(font_for('✓', false), symbol);
        assert_eq!(font_for('⣿', false), symbol);
        assert_eq!(font_for('\u{e0b0}', false), nerd);
        assert_eq!(font_for('𝟏', false), math);
        assert_eq!(font_for('中', false), cjk);
        assert_eq!(font_for('A', false), primary);
    }

    #[test]
    fn italic_style_does_not_override_fallback_fonts() {
        let primary = iced::Font::with_name("Primary");
        let cjk = iced::Font::with_name("Cjk");
        let symbol = iced::Font::with_name("Symbol");
        let math = iced::Font::with_name("Math");
        let nerd = iced::Font::with_name("Nerd");
        let italic_primary = iced::Font {
            style: iced::font::Style::Italic,
            ..primary
        };
        let font_for = |ch, italic| {
            terminal_glyph_font(
                ch,
                primary,
                Some(cjk),
                Some(symbol),
                Some(math),
                Some(nerd),
                italic,
            )
        };

        assert_eq!(font_for('中', true), cjk);
        assert_eq!(font_for('⌃', true), symbol);
        assert_eq!(font_for('𝟏', true), math);
        assert_eq!(font_for('\u{e0b0}', true), nerd);
        assert_eq!(font_for('A', true), italic_primary);
    }

    #[test]
    fn glyph_runs_require_measured_advance_primary_font_and_ascii() {
        let mut metrics = Metrics::new(10.0, 1.0, 2.0, false);
        let primary = iced::Font::with_name("Primary");
        let fallback = iced::Font::with_name("Fallback");
        let italic_primary = iced::Font {
            style: iced::font::Style::Italic,
            ..primary
        };

        // Heuristic width: nothing batches (today's per-cell behavior).
        assert!(!metrics.mono_advance_exact);
        assert!(!glyph_joins_run(metrics, 'A', primary, primary));

        // Measured width: only primary-font narrow ASCII joins a run.
        metrics.mono_advance_exact = true;
        assert!(glyph_joins_run(metrics, 'A', primary, primary));
        assert!(glyph_joins_run(metrics, '~', primary, primary));
        assert!(!glyph_joins_run(metrics, 'A', fallback, primary));
        assert!(!glyph_joins_run(metrics, 'A', italic_primary, primary));
        assert!(!glyph_joins_run(metrics, '中', primary, primary));
        assert!(!glyph_joins_run(metrics, 'é', primary, primary));
    }

    #[test]
    fn with_font_keeps_a_sane_cell_width_in_any_environment() {
        // On a fontless system the measurement returns None and the metrics
        // keep the heuristic width; on a normal desktop the real advance is
        // adopted. Either way the grid must stay sane.
        let heuristic = Metrics::new(10.0, 1.0, 2.0, false);
        let measured = Metrics::with_font(iced::Font::MONOSPACE, 10.0, 1.0, 2.0, false);
        assert!(measured.cell_w >= 1.0);
        assert_eq!(measured.cell_h, heuristic.cell_h);
        if !measured.mono_advance_exact {
            assert_eq!(measured.cell_w, heuristic.cell_w);
        } else {
            assert!(measured.cell_w >= 3.0 && measured.cell_w <= 10.0);
        }
    }
}

impl<Message, Renderer> Widget<Message, iced::Theme, Renderer> for TermWidget<'_, Message>
where
    Renderer: text::Renderer<Font = iced::Font>
        + iced::advanced::image::Renderer<Handle = iced::advanced::image::Handle>,
{
    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<State>()
    }

    fn state(&self) -> tree::State {
        tree::State::new(State::default())
    }

    fn size(&self) -> Size<Length> {
        Size::new(Length::Fill, Length::Fill)
    }

    fn layout(
        &mut self,
        _tree: &mut Tree,
        _renderer: &Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        layout::Node::new(limits.max())
    }

    fn update(
        &mut self,
        tree: &mut Tree,
        event: &Event,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        _renderer: &Renderer,
        _clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, Message>,
        _viewport: &Rectangle,
    ) {
        let bounds = layout.bounds();

        // Keep the input method enabled and positioned at the text cursor while
        // this pane is focused. The runtime only honors the request during a
        // RedrawRequested, and renders any supplied pre-edit as an over-the-spot
        // overlay anchored to `cursor`.
        if self.focused {
            if let Event::Window(iced::window::Event::RedrawRequested(_)) = event {
                let pad = self.metrics.padding;
                let (row, col) = self.cursor;
                let cursor_rect = Rectangle::new(
                    Point::new(
                        bounds.x
                            + pad
                            + self.metrics.block_gutter_width()
                            + col as f32 * self.metrics.cell_w,
                        bounds.y + pad + row as f32 * self.metrics.cell_h,
                    ),
                    Size::new(self.metrics.cell_w, self.metrics.cell_h),
                );
                let preedit =
                    self.preedit
                        .as_ref()
                        .map(|(content, selection)| input_method::Preedit {
                            content: content.as_str(),
                            selection: selection.clone(),
                            text_size: Some(Pixels(self.metrics.font_size)),
                        });
                shell.request_input_method(&InputMethod::Enabled {
                    cursor: cursor_rect,
                    purpose: input_method::Purpose::Terminal,
                    preedit,
                });
            }
        }

        let Some(on_mouse) = self.on_mouse.as_ref() else {
            return;
        };
        let state = tree.state.downcast_mut::<State>();

        match event {
            Event::Mouse(mouse::Event::ButtonPressed(btn)) => {
                let Some(pos) = cursor.position_over(bounds) else {
                    return;
                };
                let (col, row) = self.cell_at(pos, bounds);
                let button = match btn {
                    mouse::Button::Left => MouseButton::Left,
                    mouse::Button::Middle => MouseButton::Middle,
                    mouse::Button::Right => MouseButton::Right,
                    _ => return,
                };
                if button == MouseButton::Left {
                    // A second press always supersedes an unfinished summary
                    // gesture, even if the release was lost outside the pane.
                    state.summary_press = None;
                }
                let (shift, alt, ctrl) = (self.shift, self.alt, self.ctrl);
                // Grabbing the scrollbar gutter starts a scroll drag, not a
                // text selection.
                if button == MouseButton::Left {
                    if let Some((_, _, sb_x, _, _)) = self.scrollbar_geometry(bounds) {
                        if pos.x >= sb_x {
                            state.scrollbar_dragging = true;
                            state.published_presses[MouseButton::Left.slot()] = false;
                            let offset = self.offset_from_y(pos.y, bounds);
                            shell.publish(on_mouse(MouseInput::ScrollTo { offset }));
                            shell.capture_event();
                            return;
                        }
                    }
                }
                // Count every left press before deciding ownership. A first
                // header click belongs to Block Mode, but a rapid second/third
                // click on the same cell must fall through to native word/line
                // selection instead of being trapped as repeated block clicks.
                let count = if button == MouseButton::Left {
                    let now = Instant::now();
                    let same_cell = state
                        .last_click
                        .map(|(t, c, r)| {
                            c == col
                                && r == row
                                && now.duration_since(t).as_millis() <= MULTI_CLICK_MS
                        })
                        .unwrap_or(false);
                    state.click_count = if same_cell { state.click_count + 1 } else { 1 };
                    state.last_click = Some((now, col, row));
                    state.click_count
                } else {
                    1
                };

                // Finished rows are a static card surface even if a live app
                // currently has primary-buffer mouse reporting enabled. Only
                // the active/live rows belong to that app. Exclude the
                // scrollbar track and vertical padding from card hit testing.
                let grid_top = bounds.y + self.metrics.padding;
                let grid_bottom = grid_top + self.grid.len() as f32 * self.metrics.cell_h;
                let before_scrollbar = self
                    .scrollbar_geometry(bounds)
                    .is_none_or(|(_, _, sb_x, _, _)| pos.x < sb_x);
                let over_grid = pos.y >= grid_top && pos.y < grid_bottom;
                let grid_left = bounds.x + self.metrics.padding + self.metrics.block_gutter_width();
                let grid_width = self
                    .grid
                    .first()
                    .map_or(0.0, |cells| cells.len() as f32 * self.metrics.cell_w);
                let scrollbar_left = self
                    .scrollbar_geometry(bounds)
                    .map_or(bounds.x + bounds.width, |(_, _, x, _, _)| x);
                let grid_right = (grid_left + grid_width).min(scrollbar_left);
                let over_grid_columns = pos.x >= grid_left && pos.x < grid_right;
                let block_row = self.blocks.get(row);
                let collapsed_summary = (over_grid && before_scrollbar && over_grid_columns)
                    .then(|| block_row.and_then(|block| block.collapsed_summary))
                    .flatten();
                let finalized = over_grid
                    && before_scrollbar
                    && block_row.is_some_and(|block| block.selectable);
                let row_is_header = collapsed_summary.is_none()
                    && block_row.is_some_and(|block| col < block.header_end_col);
                let app_eligible = app_mouse_surface_eligible(
                    over_grid,
                    over_grid_columns,
                    self.app_mouse_full_grid,
                    collapsed_summary.is_none()
                        && block_row.is_some_and(|block| block.app_eligible),
                );
                let link_eligible = collapsed_summary.is_none()
                    && link_surface_eligible(
                        over_grid && over_grid_columns,
                        finalized,
                        row_is_header,
                    );
                // A synthetic summary is a host control, not terminal text.
                // Left activation completes only after stable release; middle
                // is deliberately swallowed; right continues into the stable
                // finalized-card menu path below.
                if let Some(summary) = collapsed_summary {
                    if button != MouseButton::Right {
                        state.consumed_presses[button.slot()] = true;
                        state.published_presses[button.slot()] = false;
                        state.dragging = false;
                        if button == MouseButton::Left && count == 1 {
                            if let Some(projection_key) = self.projection_key.clone() {
                                state.summary_press = Some(SummaryPress {
                                    key: summary.key,
                                    projection_key,
                                    point: pos,
                                    dragged: false,
                                });
                            }
                        }
                        shell.capture_event();
                        return;
                    }
                }
                let block =
                    block_mouse_action(button, finalized, row_is_header, shift, ctrl, count);
                let block_zone_id = block.and_then(|_| block_row.and_then(|row| row.zone_id));
                let link = block.is_none()
                    && ctrl_link_eligible(button, count, ctrl, shift, link_eligible)
                    && self.link_at(col, row).is_some();
                let consumed = block.is_some() || link;
                if consumed {
                    state.consumed_presses[button.slot()] = true;
                    state.published_presses[button.slot()] = false;
                } else {
                    state.consumed_presses[button.slot()] = false;
                    state.published_presses[button.slot()] = true;
                }
                if button == MouseButton::Left {
                    state.dragging = !consumed;
                }
                shell.publish(on_mouse(MouseInput::Press {
                    col,
                    row,
                    button,
                    shift,
                    alt,
                    ctrl,
                    count,
                    finalized,
                    link_eligible,
                    app_eligible,
                    block,
                    block_zone_id,
                    link,
                    link_revision: self.link_revision,
                    x: pos.x,
                    y: pos.y,
                }));
                shell.capture_event();
            }
            Event::Mouse(mouse::Event::CursorMoved { .. }) => {
                let pos = cursor.position().unwrap_or(Point::new(bounds.x, bounds.y));
                if let Some(press) = state.summary_press.as_mut() {
                    let dx = pos.x - press.point.x;
                    let dy = pos.y - press.point.y;
                    if dx * dx + dy * dy > 9.0 {
                        press.dragged = true;
                    }
                    shell.capture_event();
                    return;
                }
                if state.scrollbar_dragging {
                    let offset = self.offset_from_y(pos.y, bounds);
                    shell.publish(on_mouse(MouseInput::ScrollTo { offset }));
                    return;
                }
                if !state.dragging {
                    return;
                }
                let (col, row) = self.cell_at(pos, bounds);
                shell.publish(on_mouse(MouseInput::Drag {
                    col,
                    row,
                    count: state.click_count,
                }));
            }
            Event::Mouse(mouse::Event::ButtonReleased(btn)) => {
                let button = match btn {
                    mouse::Button::Left => MouseButton::Left,
                    mouse::Button::Middle => MouseButton::Middle,
                    mouse::Button::Right => MouseButton::Right,
                    _ => return,
                };
                if button == MouseButton::Left {
                    if let Some(press) = state.summary_press.take() {
                        state.consumed_presses[button.slot()] = false;
                        state.published_presses[button.slot()] = false;
                        state.dragging = false;
                        let current = cursor.position_over(bounds).and_then(|position| {
                            let (col, row) = self.cell_at(position, bounds);
                            let grid_left =
                                bounds.x + self.metrics.padding + self.metrics.block_gutter_width();
                            let grid_width = self
                                .grid
                                .first()
                                .map_or(0.0, |cells| cells.len() as f32 * self.metrics.cell_w);
                            let scrollbar_left = self
                                .scrollbar_geometry(bounds)
                                .map_or(bounds.x + bounds.width, |(_, _, x, _, _)| x);
                            (position.x >= grid_left
                                && position.x < (grid_left + grid_width).min(scrollbar_left))
                            .then(|| {
                                self.blocks
                                    .get(row)
                                    .and_then(|block| block.collapsed_summary)
                                    .map(|summary| (col, summary))
                            })
                            .flatten()
                        });
                        let activation = stable_summary_activation(
                            press,
                            current.map(|(_, summary)| summary),
                            self.projection_key.as_ref(),
                        );
                        if let Some(activation) = activation {
                            if let Some(on_summary) = self.on_summary.as_ref() {
                                shell.publish(on_summary(activation));
                            }
                        }
                        shell.capture_event();
                        return;
                    }
                }
                if state.consumed_presses[button.slot()] {
                    state.consumed_presses[button.slot()] = false;
                    shell.capture_event();
                    return;
                }
                // Press ownership, not current hover, routes release. This is
                // essential for right/middle application-mouse gestures: they
                // never arm text dragging, but moving outside the pane before
                // release must still deliver the matching button-up.
                if !owns_mouse_release(
                    &state.published_presses,
                    state.dragging,
                    state.scrollbar_dragging,
                    button,
                ) {
                    return;
                }
                state.published_presses[button.slot()] = false;
                if button == MouseButton::Left {
                    if state.scrollbar_dragging {
                        state.scrollbar_dragging = false;
                        return;
                    }
                    // Only a press that armed the drag pipeline emits a
                    // Release. A consumed block press never sets `dragging`,
                    // so no Release (and no selection copy) follows it.
                    if !state.dragging {
                        return;
                    }
                    state.dragging = false;
                }
                let pos = cursor.position().unwrap_or(Point::new(bounds.x, bounds.y));
                let (col, row) = self.cell_at(pos, bounds);
                shell.publish(on_mouse(MouseInput::Release { col, row, button }));
            }
            Event::Mouse(mouse::Event::WheelScrolled { delta }) => {
                let Some(pos) = cursor.position_over(bounds) else {
                    return;
                };
                // Normalize both delta kinds to lines: Lines is already in lines;
                // Pixels is divided by the cell height. Fractions accumulate so a
                // trackpad's stream of sub-line pixel deltas still scrolls.
                let dy = match delta {
                    mouse::ScrollDelta::Lines { y, .. } => *y,
                    mouse::ScrollDelta::Pixels { y, .. } => *y / self.metrics.cell_h.max(1.0),
                };
                if dy == 0.0 {
                    return;
                }
                let state = tree.state.downcast_mut::<State>();
                // Drop any leftover fraction of the opposite sign on a direction
                // reversal, otherwise it cancels part of the new delta and the
                // first reversed tick gets swallowed.
                if state.scroll_accum != 0.0 && (dy > 0.0) != (state.scroll_accum > 0.0) {
                    state.scroll_accum = 0.0;
                }
                state.scroll_accum += dy;
                let whole = state.scroll_accum.trunc();
                if whole == 0.0 {
                    shell.capture_event();
                    return;
                }
                state.scroll_accum -= whole;
                let (col, row) = self.cell_at(pos, bounds);
                let block_row = self.blocks.get(row);
                let grid_top = bounds.y + self.metrics.padding;
                let grid_bottom = grid_top + self.grid.len() as f32 * self.metrics.cell_h;
                let grid_left = bounds.x + self.metrics.padding + self.metrics.block_gutter_width();
                let grid_width = self
                    .grid
                    .first()
                    .map_or(0.0, |cells| cells.len() as f32 * self.metrics.cell_w);
                let scrollbar_left = self
                    .scrollbar_geometry(bounds)
                    .map_or(bounds.x + bounds.width, |(_, _, x, _, _)| x);
                let grid_right = (grid_left + grid_width).min(scrollbar_left);
                let app_eligible = app_mouse_surface_eligible(
                    pos.y >= grid_top && pos.y < grid_bottom,
                    pos.x >= grid_left && pos.x < grid_right,
                    self.app_mouse_full_grid,
                    block_row.is_some_and(|block| block.app_eligible),
                );
                shell.publish(on_mouse(MouseInput::Wheel {
                    col,
                    row,
                    up: whole > 0.0,
                    ctrl: self.ctrl,
                    shift: self.shift,
                    lines: whole.abs() as usize,
                    app_eligible,
                }));
                shell.capture_event();
            }
            _ => {}
        }
    }

    fn draw(
        &self,
        _tree: &Tree,
        renderer: &mut Renderer,
        _theme: &iced::Theme,
        _style: &renderer::Style,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
    ) {
        let bounds = layout.bounds();
        let clip = bounds.intersection(viewport).unwrap_or(bounds);

        // The link currently under the pointer (brightened on hover).
        let hovered: Option<&crate::link::Link> = cursor
            .position_over(bounds)
            .and_then(|position| self.link_at_position(position, bounds));
        let pad = self.metrics.padding;
        let cw = self.metrics.cell_w;
        let ch = self.metrics.cell_h;
        let ox = bounds.x + pad + self.metrics.block_gutter_width();
        let oy = bounds.y + pad;
        let scrollbar_track_left = bounds.x + bounds.width - pad - SCROLLBAR_WIDTH;
        let default_fg = self
            .dynamic_fg
            .map(|(r, g, b)| Color::from_rgb8(r, g, b))
            .unwrap_or_else(|| self.theme.terminal_foreground());
        let default_bg = self
            .dynamic_bg
            .map(|(r, g, b)| Color::from_rgb8(r, g, b))
            .unwrap_or_else(|| self.theme.terminal_background());

        // Whole-widget background. At full opacity this is the classic opaque
        // fill. Below 1.0 the app-level style already clears the surface with
        // the translucent theme background, so repainting it here would stack
        // two alpha layers and defeat the transparency; only a per-pane OSC 11
        // override still needs its own (equally translucent) fill.
        if self.opacity >= 1.0 {
            renderer.fill_quad(solid_quad(bounds), Background::Color(default_bg));
        } else if self.dynamic_bg.is_some() {
            renderer.fill_quad(
                solid_quad(bounds),
                Background::Color(Color {
                    a: self.opacity,
                    ..default_bg
                }),
            );
        }

        // The app has already projected retained zones onto visible rows.
        // Collapse those rows once into card slices and reuse the result for
        // background, hover and foreground chrome. This stays O(viewport rows)
        // even when scrollback contains thousands of lines.
        let card_segments = block_card_segments(&self.blocks);
        let hovered_card = cursor.position_over(bounds).and_then(|position| {
            let row = self.cell_at(position, bounds).1;
            let group = self
                .blocks
                .get(row)
                .filter(|row| row.selectable)?
                .card_group?;
            let segment = card_segments
                .iter()
                .find(|segment| segment.group == group)?;
            let geometry = block_card_geometry(
                bounds,
                scrollbar_track_left,
                oy,
                ch,
                *segment,
                self.block_compact,
            );
            block_card_hover_contains(bounds, geometry.body, position.x, position.y)
                .then_some(group)
        });
        let block_accent = Theme::rgb_to_color32(self.theme.tabbar.active_border);

        // Card backdrops sit below every terminal background, glyph and Kitty
        // image. Explicit cell backgrounds therefore keep exact terminal
        // semantics, while default cells reveal the light theme-relative tint.
        for segment in &card_segments {
            let geometry = block_card_geometry(
                bounds,
                scrollbar_track_left,
                oy,
                ch,
                *segment,
                self.block_compact,
            );
            if geometry.body.width <= 0.0 || geometry.body.height <= 0.0 {
                continue;
            }
            let visual = block_card_visual(
                *segment,
                default_fg,
                block_accent,
                hovered_card == Some(segment.group),
                self.opacity,
            );
            renderer.fill_quad(
                Quad {
                    bounds: geometry.body,
                    border: Border {
                        color: Color::TRANSPARENT,
                        width: 0.0,
                        radius: geometry.radius,
                    },
                    shadow: block_card_shadow(*segment, visual.shadow),
                    snap: true,
                },
                Background::Color(visual.background),
            );
        }

        // Bucket links by visible row so the per-cell hit test scans only the
        // links on that row instead of the whole list. Skipped entirely (no
        // allocation) in the common case where no links are present.
        let links_by_row: Vec<Vec<&crate::link::Link>> = if self.links.is_empty() {
            Vec::new()
        } else {
            let mut buckets: Vec<Vec<&crate::link::Link>> = vec![Vec::new(); self.grid.len()];
            for l in self.links {
                if l.line < buckets.len() {
                    buckets[l.line].push(l);
                }
            }
            buckets
        };

        for (row_idx, row) in self.grid.iter().enumerate() {
            let y = oy + row_idx as f32 * ch;

            // Backgrounds.
            for (col_idx, cell) in row.iter().enumerate() {
                if cell.flags.wide_continuation() {
                    continue;
                }
                let mut bg = if cell.background == crate::terminal::Color::Default {
                    default_bg
                } else {
                    resolve_bg_with_palette(cell.background, self.theme, self.dynamic_palette)
                };
                let mut fg = if cell.foreground == crate::terminal::Color::Default {
                    default_fg
                } else {
                    resolve_fg_with_palette(
                        cell.foreground,
                        self.theme,
                        self.dynamic_palette,
                        cell.flags.bold(),
                        cell.flags.dim(),
                    )
                };
                if cell.flags.inverse() {
                    std::mem::swap(&mut bg, &mut fg);
                }
                let span = if cell.flags.wide() { 2.0 } else { 1.0 };
                if bg != default_bg {
                    let x = ox + col_idx as f32 * cw;
                    renderer.fill_quad(
                        solid_quad(Rectangle {
                            x,
                            y,
                            width: cw * span,
                            height: ch,
                        }),
                        Background::Color(bg),
                    );
                }
            }

            // A legacy separator is only needed for non-card chrome. Card
            // boundaries are drawn once per contiguous segment below.
            if let Some(block) = self.blocks.get(row_idx) {
                let text_right = bounds.x + bounds.width - pad - SCROLLBAR_WIDTH;
                if block.separator && block.card_group.is_none() {
                    renderer.fill_quad(
                        solid_quad(Rectangle {
                            x: ox,
                            y,
                            width: (text_right - ox).max(0.0),
                            height: 1.0,
                        }),
                        Background::Color(Color {
                            a: 0.15,
                            ..self.theme.terminal_foreground()
                        }),
                    );
                }
            }

            // Selection highlight (semi-transparent overlay).
            if let Some(Some((sc, ec))) = self.selection.get(row_idx) {
                let last = row.len().saturating_sub(1);
                let start = (*sc).min(last);
                let end = (*ec).min(last);
                if end >= start {
                    let x = ox + start as f32 * cw;
                    let width = (end - start + 1) as f32 * cw;
                    renderer.fill_quad(
                        solid_quad(Rectangle {
                            x,
                            y,
                            width,
                            height: ch,
                        }),
                        Background::Color(self.theme.selection_color()),
                    );
                }
            }

            // Search match highlights (semi-transparent overlay).
            if !self.search_matches.is_empty() {
                let last = row.len().saturating_sub(1);
                for m in self.search_matches.iter().filter(|m| m.line == row_idx) {
                    let start = m.col_start.min(last);
                    let end = m.col_end.saturating_sub(1).min(last);
                    if end >= start {
                        let color = if self.current_match == Some((m.line, m.col_start)) {
                            self.theme.search_current_color()
                        } else {
                            self.theme.search_match_color()
                        };
                        let x = ox + start as f32 * cw;
                        let width = (end - start + 1) as f32 * cw;
                        renderer.fill_quad(
                            solid_quad(Rectangle {
                                x,
                                y,
                                width,
                                height: ch,
                            }),
                            Background::Color(color),
                        );
                    }
                }
            }

            // Glyphs + decorations. When `cell_w` is the primary font's
            // measured advance (`Metrics::with_font`), consecutive same-font/
            // same-fg narrow ASCII glyphs shape as one run that lands exactly
            // on the grid; everything else stays one cell per fill_text. With
            // the heuristic width a font's real advance can differ slightly
            // from `cell_w`; shaping multiple cells together would accumulate
            // that difference and make later glyphs drift away from the grid.
            let font = self.mono;
            let font_size = self.metrics.font_size;
            // Cells covered by the active selection draw their glyphs in the
            // selection foreground color so text stays legible over the overlay.
            let sel_range = self.selection.get(row_idx).copied().flatten();
            let mut run_text = String::new();
            let mut run_len: usize = 0;
            let mut run_fg = Color::TRANSPARENT;
            let mut run_start = 0usize;
            let mut run_font = font;
            let emit_run = |renderer: &mut Renderer,
                            text: &mut String,
                            len: &mut usize,
                            start: usize,
                            fg: Color,
                            run_font: iced::Font| {
                if *len == 0 {
                    return;
                }
                let rx = ox + start as f32 * cw;
                let content = std::mem::take(text);
                let shaping = glyph_shaping(&content);
                renderer.fill_text(
                    Text {
                        content,
                        bounds: Size::new(cw * *len as f32, ch),
                        size: Pixels(font_size),
                        line_height: text::LineHeight::Absolute(Pixels(ch)),
                        font: run_font,
                        align_x: text::Alignment::Left,
                        align_y: iced::alignment::Vertical::Center,
                        shaping,
                        wrapping: text::Wrapping::None,
                    },
                    Point::new(rx, y + ch / 2.0),
                    fg,
                    clip,
                );
                *len = 0;
            };

            for (col_idx, cell) in row.iter().enumerate() {
                if cell.flags.wide_continuation() {
                    continue;
                }
                let glyph = cell.character;
                let is_wide = cell.flags.wide();
                let span = if is_wide { 2.0 } else { 1.0 };
                let x = ox + col_idx as f32 * cw;
                let mut fg = if cell.foreground == crate::terminal::Color::Default {
                    default_fg
                } else {
                    resolve_fg_with_palette(
                        cell.foreground,
                        self.theme,
                        self.dynamic_palette,
                        cell.flags.bold(),
                        cell.flags.dim(),
                    )
                };
                if cell.flags.inverse() {
                    fg = if cell.background == crate::terminal::Color::Default {
                        default_bg
                    } else {
                        resolve_bg_with_palette(cell.background, self.theme, self.dynamic_palette)
                    };
                }
                let selected = sel_range.is_some_and(|(sc, ec)| col_idx >= sc && col_idx <= ec);
                if selected {
                    fg = self.theme.selection_fg_color();
                }
                let glyph_font = terminal_glyph_font(
                    glyph,
                    font,
                    self.cjk_mono,
                    self.symbol_mono,
                    self.math_symbol,
                    self.nerd_symbol,
                    cell.flags.italic(),
                );
                // Blink: during the off phase, blinking cells show no glyph.
                let blink_hidden = cell.flags.blink() && !self.blink_on;

                // Clickable links keep their terminal color unless hovered.
                let row_links: &[&crate::link::Link] =
                    links_by_row.get(row_idx).map(Vec::as_slice).unwrap_or(&[]);
                let is_link = row_links
                    .iter()
                    .any(|l| col_idx >= l.col_start && col_idx < l.col_end);
                if is_link {
                    let is_hovered = hovered.is_some_and(|h| {
                        h.line == row_idx && col_idx >= h.col_start && col_idx < h.col_end
                    });
                    if is_hovered {
                        fg = hovered_link_color();
                    }
                }

                let printable = glyph != ' ' && glyph != '\0' && !blink_hidden;

                if printable && !is_wide {
                    if glyph_joins_run(self.metrics, glyph, glyph_font, font) {
                        // Accumulate into the pending run; flush at any fg or
                        // column-continuity boundary. The font boundary is
                        // covered by `glyph_joins_run` itself.
                        if run_len != 0 && (fg != run_fg || col_idx != run_start + run_len) {
                            emit_run(
                                renderer,
                                &mut run_text,
                                &mut run_len,
                                run_start,
                                run_fg,
                                run_font,
                            );
                        }
                        if run_len == 0 {
                            run_start = col_idx;
                            run_fg = fg;
                            run_font = glyph_font;
                        }
                        run_text.push(glyph);
                        run_len += 1;
                    } else {
                        // Fallback fonts, italics and non-ASCII cells keep
                        // per-cell emission: flush any pending batch, then
                        // draw this cell as a one-cell run exactly as before.
                        emit_run(
                            renderer,
                            &mut run_text,
                            &mut run_len,
                            run_start,
                            run_fg,
                            run_font,
                        );
                        run_start = col_idx;
                        run_fg = fg;
                        run_font = glyph_font;
                        run_text.push(glyph);
                        run_len += 1;
                        emit_run(
                            renderer,
                            &mut run_text,
                            &mut run_len,
                            run_start,
                            run_fg,
                            run_font,
                        );
                    }
                } else {
                    // Spaces and wide glyphs end any pending run; wide glyphs are
                    // drawn individually, centered over their two-cell span.
                    emit_run(
                        renderer,
                        &mut run_text,
                        &mut run_len,
                        run_start,
                        run_fg,
                        run_font,
                    );
                    if printable {
                        let content = glyph.to_string();
                        let shaping = glyph_shaping(&content);
                        renderer.fill_text(
                            Text {
                                content,
                                bounds: Size::new(cw * span, ch),
                                size: Pixels(font_size),
                                line_height: text::LineHeight::Absolute(Pixels(ch)),
                                font: glyph_font,
                                align_x: text::Alignment::Center,
                                align_y: iced::alignment::Vertical::Center,
                                shaping,
                                wrapping: text::Wrapping::None,
                            },
                            Point::new(x + cw * span / 2.0, y + ch / 2.0),
                            fg,
                            clip,
                        );
                    }
                }

                if is_link || cell.flags.underline() != UnderlineStyle::None {
                    renderer.fill_quad(
                        solid_quad(Rectangle {
                            x,
                            y: y + ch - 2.0,
                            width: cw * span,
                            height: 1.0,
                        }),
                        Background::Color(fg),
                    );
                }
                if cell.flags.strikethrough() {
                    renderer.fill_quad(
                        solid_quad(Rectangle {
                            x,
                            y: y + ch * 0.5,
                            width: cw * span,
                            height: 1.0,
                        }),
                        Background::Color(fg),
                    );
                }
            }
            // Flush any run that reached the end of the row.
            emit_run(
                renderer,
                &mut run_text,
                &mut run_len,
                run_start,
                run_fg,
                run_font,
            );

            if let Some(summary) = self
                .blocks
                .get(row_idx)
                .and_then(|block| block.collapsed_summary)
            {
                let content = format!(
                    "▸ {} output rows hidden — click to expand",
                    summary.hidden_display_rows
                );
                let text_right = scrollbar_track_left - 8.0;
                renderer.fill_text(
                    Text {
                        content,
                        bounds: Size::new((text_right - ox - cw).max(0.0), ch),
                        size: Pixels((font_size * 0.9).max(8.0)),
                        line_height: text::LineHeight::Absolute(Pixels(ch)),
                        font,
                        align_x: text::Alignment::Left,
                        align_y: iced::alignment::Vertical::Center,
                        shaping: text::Shaping::Advanced,
                        wrapping: text::Wrapping::None,
                    },
                    Point::new(ox + cw, y + ch / 2.0),
                    Color {
                        a: 0.82,
                        ..default_fg
                    },
                    clip,
                );
            }
        }

        // Kitty graphics: paint each placement (already z-sorted) as a texture
        // stretched into its cell span. Block UI chrome and the cursor follow,
        // so images cannot hide selection/bookmark affordances or the caret.
        for im in &self.images {
            let x = ox + im.col as f32 * cw;
            let y = oy + im.row as f32 * ch;
            let w = im.cols as f32 * cw;
            let h = im.rows as f32 * ch;
            let rect = Rectangle {
                x,
                y,
                width: w,
                height: h,
            };
            renderer.draw_image(
                iced::advanced::image::Image::new(im.handle.clone()),
                rect,
                clip,
            );
        }

        // Borders and outcome stripes remain above images so the command-card
        // state cannot disappear under a terminal graphic. They are still
        // batched per visible card, not repeated for every row.
        for segment in &card_segments {
            let geometry = block_card_geometry(
                bounds,
                scrollbar_track_left,
                oy,
                ch,
                *segment,
                self.block_compact,
            );
            if geometry.body.width <= 0.0 || geometry.body.height <= 0.0 {
                continue;
            }
            let stripe = segment.stripe.unwrap_or(block_accent);
            let visual = block_card_visual(
                *segment,
                default_fg,
                block_accent,
                hovered_card == Some(segment.group),
                self.opacity,
            );
            if segment.real_top && segment.real_bottom {
                renderer.fill_quad(
                    Quad {
                        bounds: geometry.body,
                        border: Border {
                            color: visual.border,
                            width: visual.border_width,
                            radius: geometry.radius,
                        },
                        shadow: Shadow::default(),
                        snap: true,
                    },
                    Background::Color(Color::TRANSPARENT),
                );
            } else {
                for edge in clipped_block_card_border_bounds(
                    geometry.body,
                    segment.real_top,
                    segment.real_bottom,
                    visual.border_width,
                )
                .into_iter()
                .flatten()
                {
                    renderer.fill_quad(solid_quad(edge), Background::Color(visual.border));
                }
            }

            let requested_width = if segment.stripe_strong {
                BLOCK_STRIPE_SELECTED_WIDTH
            } else {
                BLOCK_STRIPE_WIDTH
            };
            let stripe_bounds =
                block_card_stripe_bounds(bounds, geometry.body, requested_width, ox);
            if stripe_bounds.width > 0.0 {
                renderer.fill_quad(
                    Quad {
                        bounds: stripe_bounds,
                        border: Border {
                            color: Color::TRANSPARENT,
                            width: 0.0,
                            radius: border::Radius {
                                top_left: geometry.radius.top_left,
                                top_right: 0.0,
                                bottom_right: 0.0,
                                bottom_left: geometry.radius.bottom_left,
                            },
                        },
                        shadow: Shadow::default(),
                        snap: true,
                    },
                    Background::Color(alpha(stripe, if segment.stripe_strong { 1.0 } else { 0.7 })),
                );
            }
        }

        // Block-mode UI chrome (per-row, precomputed by the app) sits above
        // glyphs/images. The badge only occupies cells verified blank, so its
        // text never collides with terminal text.
        for (row_idx, block) in self.blocks.iter().enumerate() {
            let y = oy + row_idx as f32 * ch;
            let text_right = bounds.x + bounds.width - pad - SCROLLBAR_WIDTH;

            if block.bookmarked {
                let marker = self.theme.ansi_color(3);
                let size = ch
                    .min((self.metrics.block_gutter_width() - 2.0).max(1.0))
                    .clamp(1.0, 7.0);
                renderer.fill_quad(
                    Quad {
                        bounds: Rectangle {
                            x: bounds.x + 1.0,
                            y: y + (ch - size) / 2.0,
                            width: size,
                            height: size,
                        },
                        border: Border {
                            color: Color::TRANSPARENT,
                            width: 0.0,
                            radius: 1.5.into(),
                        },
                        shadow: Shadow::default(),
                        snap: true,
                    },
                    Background::Color(Color { a: 0.95, ..marker }),
                );
            }
            if let Some((badge, color)) = &block.badge {
                let badge_w = badge.chars().count() as f32 * cw;
                let card_inset = if self.block_compact {
                    BLOCK_CARD_COMPACT_INSET
                } else {
                    BLOCK_CARD_INSET
                };
                let right = text_right - card_inset - 4.0;
                renderer.fill_quad(
                    Quad {
                        bounds: Rectangle {
                            x: right - badge_w - 8.0,
                            y: y + 1.0,
                            width: badge_w + 8.0,
                            height: ch - 2.0,
                        },
                        border: Border {
                            color: Color::TRANSPARENT,
                            width: 0.0,
                            radius: ((ch - 2.0) / 3.0).into(),
                        },
                        shadow: Shadow::default(),
                        snap: true,
                    },
                    Background::Color(Color { a: 0.12, ..*color }),
                );
                let shaping = glyph_shaping(badge);
                renderer.fill_text(
                    Text {
                        content: badge.clone(),
                        bounds: Size::new(badge_w + 8.0, ch),
                        size: Pixels(self.metrics.font_size),
                        line_height: text::LineHeight::Absolute(Pixels(ch)),
                        font: self.mono,
                        align_x: text::Alignment::Right,
                        align_y: iced::alignment::Vertical::Center,
                        shaping,
                        wrapping: text::Wrapping::None,
                    },
                    Point::new(right - 4.0, y + ch / 2.0),
                    *color,
                    clip,
                );
            }
        }

        // Cursor.
        if self.cursor_visible {
            let (cr, cc) = self.cursor;
            let x = ox + cc as f32 * cw;
            let y = oy + cr as f32 * ch;
            let cur = self
                .dynamic_cursor
                .map(|(r, g, b)| Color::from_rgb8(r, g, b))
                .unwrap_or_else(|| self.theme.cursor_color());
            let cursor_cell = self.grid.get(cr).and_then(|r| r.get(cc));
            // A wide (CJK) glyph occupies two cells; the cursor must cover both.
            let cursor_w = if cursor_cell.is_some_and(|c| c.flags.wide()) {
                cw * 2.0
            } else {
                cw
            };
            let shape_bounds = match self.cursor_shape {
                CursorShape::Block => Rectangle {
                    x,
                    y,
                    width: cursor_w,
                    height: ch,
                },
                CursorShape::Underline => {
                    let h = (ch * 0.12).clamp(1.0, 3.0);
                    Rectangle {
                        x,
                        y: y + ch - h,
                        width: cursor_w,
                        height: h,
                    }
                }
                CursorShape::Beam => {
                    let w = (cw * 0.12).clamp(1.0, 3.0);
                    Rectangle {
                        x,
                        y,
                        width: w,
                        height: ch,
                    }
                }
            };
            if self.focused {
                renderer.fill_quad(solid_quad(shape_bounds), Background::Color(cur));
                if self.cursor_shape == CursorShape::Block {
                    if let Some(cell) = cursor_cell {
                        let glyph = cell.character;
                        if glyph != ' ' && glyph != '\0' {
                            let content = glyph.to_string();
                            let shaping = glyph_shaping(&content);
                            renderer.fill_text(
                                Text {
                                    content,
                                    bounds: Size::new(cursor_w, ch),
                                    size: Pixels(self.metrics.font_size),
                                    line_height: text::LineHeight::Absolute(Pixels(ch)),
                                    font: terminal_glyph_font(
                                        glyph,
                                        self.mono,
                                        self.cjk_mono,
                                        self.symbol_mono,
                                        self.math_symbol,
                                        self.nerd_symbol,
                                        cell.flags.italic(),
                                    ),
                                    align_x: text::Alignment::Center,
                                    align_y: iced::alignment::Vertical::Center,
                                    shaping,
                                    wrapping: text::Wrapping::None,
                                },
                                Point::new(x + cursor_w / 2.0, y + ch / 2.0),
                                default_bg,
                                clip,
                            );
                        }
                    }
                }
            } else if self.cursor_shape == CursorShape::Block {
                let cursor_border = Quad {
                    bounds: shape_bounds,
                    border: Border {
                        color: cur,
                        width: 1.0,
                        radius: 0.0.into(),
                    },
                    shadow: Shadow::default(),
                    snap: true,
                };
                renderer.fill_quad(cursor_border, Background::Color(Color::TRANSPARENT));
            } else {
                renderer.fill_quad(solid_quad(shape_bounds), Background::Color(cur));
            }
        }

        // Scrollbar (only when scrollback exists).
        if let Some((track_top, track_h, sb_x, thumb_y, thumb_h)) = self.scrollbar_geometry(bounds)
        {
            let fg = self.theme.terminal_foreground();
            let track = Color { a: 0.10, ..fg };
            let thumb = Color { a: 0.45, ..fg };
            renderer.fill_quad(
                solid_quad(Rectangle {
                    x: sb_x,
                    y: track_top,
                    width: SCROLLBAR_WIDTH,
                    height: track_h,
                }),
                Background::Color(track),
            );
            renderer.fill_quad(
                Quad {
                    bounds: Rectangle {
                        x: sb_x + 1.0,
                        y: thumb_y,
                        width: SCROLLBAR_WIDTH - 2.0,
                        height: thumb_h,
                    },
                    border: Border {
                        color: Color::TRANSPARENT,
                        width: 0.0,
                        radius: ((SCROLLBAR_WIDTH - 2.0) / 2.0).into(),
                    },
                    shadow: Shadow::default(),
                    snap: true,
                },
                Background::Color(thumb),
            );
            // Failed-block markers (block mode): a short stripe at each
            // failed zone's exact position in the buffer, the same red as
            // its gutter stripe. Painted after the thumb so a failure under
            // the thumb stays visible.
            if !self.block_markers.is_empty() {
                const MARKER_HEIGHT: f32 = 3.0;
                let red = self.theme.ansi_color(1);
                // Family formula (ember): the fraction spans the track MINUS
                // the marker height, so fraction 1.0 puts the marker's bottom
                // edge exactly at the track's bottom instead of overshooting.
                let span = (track_h - MARKER_HEIGHT).max(0.0);
                for &fraction in &self.block_markers {
                    renderer.fill_quad(
                        solid_quad(Rectangle {
                            x: sb_x,
                            y: track_top + fraction.clamp(0.0, 1.0) * span,
                            width: SCROLLBAR_WIDTH,
                            height: MARKER_HEIGHT,
                        }),
                        Background::Color(Color { a: 0.9, ..red }),
                    );
                }
            }
            if !self.block_bookmark_markers.is_empty() {
                const MARKER_HEIGHT: f32 = 3.0;
                let amber = self.theme.ansi_color(3);
                let span = (track_h - MARKER_HEIGHT).max(0.0);
                for &fraction in &self.block_bookmark_markers {
                    renderer.fill_quad(
                        solid_quad(Rectangle {
                            x: sb_x + SCROLLBAR_WIDTH / 2.0,
                            y: track_top + fraction.clamp(0.0, 1.0) * span,
                            width: SCROLLBAR_WIDTH / 2.0,
                            height: MARKER_HEIGHT,
                        }),
                        Background::Color(Color { a: 0.95, ..amber }),
                    );
                }
            }
        }
    }

    fn mouse_interaction(
        &self,
        _tree: &Tree,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        _viewport: &Rectangle,
        _renderer: &Renderer,
    ) -> mouse::Interaction {
        if let Some(p) = cursor.position_over(layout.bounds()) {
            let (c, r) = self.cell_at(p, layout.bounds());
            let before_scrollbar = self
                .scrollbar_geometry(layout.bounds())
                .is_none_or(|(_, _, sb_x, _, _)| p.x < sb_x);
            if before_scrollbar
                && self.blocks.get(r).is_some_and(|block| {
                    block.collapsed_summary.is_some()
                        || (block.selectable && (c < block.header_end_col || self.shift))
                })
            {
                return mouse::Interaction::Pointer;
            }
            if self.link_at_position(p, layout.bounds()).is_some() {
                return mouse::Interaction::Pointer;
            }
        }
        mouse::Interaction::default()
    }
}

impl<'a, Message, Renderer> From<TermWidget<'a, Message>>
    for Element<'a, Message, iced::Theme, Renderer>
where
    Renderer: text::Renderer<Font = iced::Font>
        + iced::advanced::image::Renderer<Handle = iced::advanced::image::Handle>
        + 'a,
    Message: 'a,
{
    fn from(w: TermWidget<'a, Message>) -> Self {
        Element::new(w)
    }
}
