//! Native Vim buffer text, independent of terminal cells and ANSI palettes.
//!
//! The established PTY input adapter retains focus and compatibility gestures.
//! Its painter is clipped to Vim's command line / compatible margins. The
//! buffer rectangles are drawn exclusively from authenticated GUI text runs.

use iced::advanced::layout::{self, Layout};
use iced::advanced::renderer::{self, Quad};
use iced::advanced::text::{self, Text};
use iced::advanced::widget::{tree, Tree, Widget};
use iced::advanced::{Clipboard, Shell};
use iced::{
    keyboard, mouse, Background, Color, Element, Event, Font, Length, Pixels, Point, Rectangle,
    Size,
};
use jterm_core::vim9::{
    Vim9BufferPosition, Vim9Command, Vim9PixelRect, Vim9PixelScroll, Vim9PointerPhase,
    Vim9ProjectedRun, Vim9RenderFrame, Vim9RenderMetrics, Vim9RenderRect, Vim9RenderStyle,
    Vim9Viewport,
};
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::sync::Arc;

use crate::terminal_view::{Metrics, MouseButton, MouseInput, TermWidget};

#[derive(Debug, Clone)]
pub(crate) struct NativeWheel {
    pub window: i64,
    /// Positive means down. Precision devices retain fractional lines.
    pub lines: f64,
    buffer: i64,
    changedtick: u64,
    view: Vim9RenderRect,
    cell_width: f32,
    line_height: f32,
}

/// Compatibility gestures keep the existing terminal click counter and modifiers.
/// Only an advertised plain primary gesture is translated to source bytes.
#[derive(Debug, Clone)]
pub(crate) enum NativeInput {
    Compatibility(MouseInput),
    Handled,
}

#[derive(Clone, Debug)]
struct PointerGesture {
    id: u64,
    metrics: Vim9RenderMetrics,
    view: Vim9RenderRect,
    source: Vim9BufferPosition,
    last: Vim9BufferPosition,
    press: Option<Vim9BufferPosition>,
    latest: Option<(Vim9BufferPosition, Vim9PointerPhase)>,
    pending: Option<u64>,
    released: bool,
}

#[derive(Clone)]
struct PresentedFrame {
    frame: Arc<Vim9RenderFrame>,
    metrics: Vim9RenderMetrics,
    local_metrics: Vim9RenderMetrics,
    offsets: BTreeMap<i64, f32>,
    clip: Rectangle,
}

impl PresentedFrame {
    fn viewport(&self, window: i64) -> Option<Vim9Viewport<'_>> {
        Vim9Viewport::new(
            &self.frame,
            window,
            self.metrics,
            *self.offsets.get(&window).unwrap_or(&0.0),
        )
    }

    fn caret(&self) -> Option<Rectangle> {
        self.viewport(self.frame.window)?
            .cursor_rect()
            .map(rectangle)
            .and_then(|rect| rect.intersection(&self.clip))
    }
}

/// One source-bound precision transaction per split. Paint, raw mouse hits and
/// IME use the same viewport offset. No second momentum curve is synthesized.
#[derive(Default)]
pub(crate) struct ScrollState {
    pixels: BTreeMap<i64, Vim9PixelScroll>,
    pointer: Option<PointerGesture>,
    // A cancelled native press still owns its drag/release tail. Never leak it
    // as a compatibility drag without a corresponding compatibility press.
    native_pressed: bool,
    consumed_buttons: [bool; 3],
    next_gesture: u64,
    position: Option<Point>,
    presented: Option<PresentedFrame>,
}

impl ScrollState {
    pub(crate) fn clear(&mut self) {
        self.pixels.clear();
        self.pointer = None;
    }

    pub(crate) fn compatibility_mouse(&mut self, input: MouseInput) -> Option<MouseInput> {
        self.clear();
        if let MouseInput::Press { button, .. } = input {
            if self.fractional_surface_at_pointer() {
                // The renderer can lose native ownership before its fallback
                // image is drawn. Do not reinterpret that last moving image
                // as stationary PTY cells in this intervening event batch.
                if button == MouseButton::Left {
                    self.native_pressed = true;
                } else {
                    self.consumed_buttons[button.slot()] = true;
                }
                return None;
            }
        }
        match input {
            MouseInput::Release { button, .. } if self.consumed_buttons[button.slot()] => {
                self.consumed_buttons[button.slot()] = false;
                None
            }
            MouseInput::Drag { .. } if self.native_pressed => None,
            MouseInput::Release {
                button: MouseButton::Left,
                ..
            } if self.native_pressed => {
                self.native_pressed = false;
                None
            }
            MouseInput::Press {
                button: MouseButton::Left,
                ..
            } => {
                self.native_pressed = false;
                Some(input)
            }
            _ => Some(input),
        }
    }

    pub(crate) fn wheel(
        &mut self,
        frame: &Vim9RenderFrame,
        metrics: Vim9RenderMetrics,
        wheel: NativeWheel,
    ) {
        if self.pointer.is_some() || frame.mode != "n" {
            return;
        }
        let Some(window) = frame.windows.iter().find(|w| {
            w.window == wheel.window
                && w.buffer == wheel.buffer
                && w.changedtick == wheel.changedtick
                && w.view == wheel.view
                && metrics.cell_width == wheel.cell_width
                && metrics.line_height == wheel.line_height
        }) else {
            return;
        };
        if let std::collections::btree_map::Entry::Vacant(entry) = self.pixels.entry(wheel.window) {
            let Some(scroll) = Vim9PixelScroll::new(window, metrics) else {
                return;
            };
            entry.insert(scroll);
        }
        if let Some(scroll) = self.pixels.get_mut(&wheel.window) {
            scroll.push(wheel.lines);
        }
    }

    pub(crate) fn observe(
        &mut self,
        frame: Option<&Vim9RenderFrame>,
        metrics: Vim9RenderMetrics,
        enabled: bool,
        pending: impl Fn(u64) -> bool,
    ) {
        let Some(frame) = frame.filter(|f| f.supported && enabled) else {
            self.clear();
            return;
        };
        self.pixels.retain(|id, scroll| {
            frame
                .windows
                .iter()
                .find(|w| w.window == *id)
                .is_some_and(|window| {
                    let waiting = scroll.request_id().is_some_and(&pending);
                    scroll.reconcile(window, metrics, waiting)
                })
        });
        if let Some(gesture) = self.pointer.as_mut() {
            let valid = gesture.metrics == metrics
                && frame.windows.iter().any(|window| {
                    window.native_pointer
                        && window.window == gesture.source.window
                        && window.buffer == gesture.source.buffer
                        && window.changedtick == gesture.source.changedtick
                        && window.view == gesture.view
                });
            if !valid {
                self.pointer = None;
            } else if gesture.pending.is_some_and(|id| !pending(id)) {
                gesture.pending = None;
                if gesture.released && gesture.press.is_none() && gesture.latest.is_none() {
                    self.pointer = None;
                }
            }
        }
    }

    pub(crate) fn offset(
        &self,
        frame: &Vim9RenderFrame,
        window: i64,
        metrics: Vim9RenderMetrics,
    ) -> f32 {
        self.pixels
            .get(&window)
            .map_or(0.0, |scroll| scroll.offset(frame, metrics))
    }

    pub(crate) fn scroll_commands(&self) -> Vec<(i64, Vim9Command)> {
        if self.pointer.is_some() {
            return Vec::new();
        }
        self.pixels
            .iter()
            .filter_map(|(window, scroll)| scroll.command().map(|command| (*window, command)))
            .collect()
    }

    pub(crate) fn scroll_submitted(&mut self, window: i64, id: u64) {
        if let Some(scroll) = self.pixels.get_mut(&window) {
            scroll.submitted(id);
        }
    }

    pub(crate) fn scroll_failed(&mut self, window: i64) {
        self.pixels.remove(&window);
    }

    pub(crate) fn pointer_command(&self) -> Option<Vim9Command> {
        let gesture = self.pointer.as_ref()?;
        if gesture.pending.is_some() {
            return None;
        }
        let (target, phase) = gesture
            .press
            .as_ref()
            .map(|p| (p, Vim9PointerPhase::Press))
            .or_else(|| gesture.latest.as_ref().map(|(p, phase)| (p, *phase)))?;
        Some(pointer_command(target, gesture.id, phase))
    }

    pub(crate) fn pointer_submitted(&mut self, id: u64) {
        if let Some(gesture) = self.pointer.as_mut() {
            if gesture.press.take().is_none() {
                gesture.latest = None;
            }
            gesture.pending = Some(id);
        }
    }

    pub(crate) fn pointer_failed(&mut self) {
        self.pointer = None;
    }

    fn fractional_surface_at_pointer(&self) -> bool {
        let Some((presented, point)) = self.presented.as_ref().zip(self.position) else {
            return false;
        };
        presented.clip.contains(point)
            && presented.frame.windows.iter().any(|window| {
                presented
                    .offsets
                    .get(&window.window)
                    .copied()
                    .unwrap_or(0.0)
                    .abs()
                    >= 0.01
                    && presented
                        .viewport(window.window)
                        .is_some_and(|view| view.clip().contains(point.x, point.y))
            })
    }

    fn anchor_at(
        &self,
        current: &Vim9RenderFrame,
        metrics: Vim9RenderMetrics,
        fractional_only: bool,
    ) -> Option<Vim9BufferPosition> {
        let point = self.position?;
        let presented = self
            .presented
            .as_ref()
            .filter(|p| p.local_metrics == metrics && p.clip.contains(point))?;
        presented
            .frame
            .windows
            .iter()
            .filter(|w| w.native_pointer)
            .find_map(|window| {
                if fractional_only
                    && presented
                        .offsets
                        .get(&window.window)
                        .copied()
                        .unwrap_or(0.0)
                        .abs()
                        < 0.01
                {
                    return None;
                }
                if !current.windows.iter().any(|w| {
                    w.native_pointer
                        && w.window == window.window
                        && w.buffer == window.buffer
                        && w.changedtick == window.changedtick
                        && w.view == window.view
                }) {
                    return None;
                }
                presented
                    .viewport(window.window)?
                    .anchor_at(point.x, point.y)
                    .map(|anchor| anchor.position)
            })
    }

    fn presented_view(&self, window: i64) -> Option<Vim9RenderRect> {
        self.presented
            .as_ref()?
            .frame
            .windows
            .iter()
            .find(|w| w.window == window)
            .map(|w| w.view.clone())
    }

    fn projection(
        &self,
        frame: &Arc<Vim9RenderFrame>,
        metrics: Vim9RenderMetrics,
        local_metrics: Vim9RenderMetrics,
        clip: Rectangle,
    ) -> PresentedFrame {
        let offsets = frame
            .windows
            .iter()
            .map(|w| (w.window, self.offset(frame, w.window, local_metrics)))
            .collect();
        PresentedFrame {
            frame: Arc::clone(frame),
            metrics,
            local_metrics,
            offsets,
            clip,
        }
    }

    fn record_presentation(
        &mut self,
        frame: &Arc<Vim9RenderFrame>,
        metrics: Vim9RenderMetrics,
        local_metrics: Vim9RenderMetrics,
        clip: Rectangle,
    ) {
        self.presented = Some(self.projection(frame, metrics, local_metrics, clip));
    }

    pub(crate) fn route_mouse(
        &mut self,
        frame: &Vim9RenderFrame,
        metrics: Vim9RenderMetrics,
        modifiers: keyboard::Modifiers,
        input: MouseInput,
    ) -> NativeInput {
        let unmodified = modifiers.is_empty();
        if let MouseInput::Press {
            button,
            count,
            app_eligible,
            ..
        } = input
        {
            self.consumed_buttons[button.slot()] = false;
            let supported = button == MouseButton::Left && count == 1 && unmodified && app_eligible;
            if self.fractional_surface_at_pointer() && !supported {
                // Settle unsupported gestures without sending their moving
                // screen coordinates to the stationary PTY. The next press on
                // a drawn stationary frame retains the normal Vim semantics.
                self.clear();
                if button == MouseButton::Left {
                    self.native_pressed = true;
                } else {
                    self.consumed_buttons[button.slot()] = true;
                }
                return NativeInput::Handled;
            }
        }
        match input {
            MouseInput::Release { button, .. } if self.consumed_buttons[button.slot()] => {
                self.consumed_buttons[button.slot()] = false;
                return NativeInput::Handled;
            }
            MouseInput::Press {
                button: MouseButton::Left,
                count,
                app_eligible,
                ..
            } => {
                // The terminal adapter has already counted this press. It sees
                // every click, including the first native click of a double.
                self.native_pressed = false;
                self.pointer = None;
                if count == 1 && unmodified && app_eligible {
                    if let Some(target) = self.anchor_at(frame, metrics, true) {
                        self.next_gesture = self.next_gesture.wrapping_add(1).max(1);
                        self.native_pressed = true;
                        self.pointer = Some(PointerGesture {
                            id: self.next_gesture,
                            metrics,
                            view: self.presented_view(target.window).unwrap(),
                            source: target,
                            last: target,
                            press: Some(target),
                            latest: None,
                            pending: None,
                            released: false,
                        });
                        return NativeInput::Handled;
                    }
                    if self.fractional_surface_at_pointer() {
                        // The visible image has no valid source target now.
                        // Do not replay its click into a replacement buffer.
                        self.clear();
                        self.native_pressed = true;
                        return NativeInput::Handled;
                    }
                }
                self.clear();
            }
            MouseInput::Drag { .. } if self.native_pressed => {
                let target = unmodified
                    .then(|| self.anchor_at(frame, metrics, false))
                    .flatten();
                if let (Some(gesture), Some(target)) = (self.pointer.as_mut(), target) {
                    if target.window == gesture.source.window
                        && target.buffer == gesture.source.buffer
                        && target.changedtick == gesture.source.changedtick
                        && target != gesture.last
                    {
                        gesture.last = target;
                        gesture.latest = Some((target, Vim9PointerPhase::Drag));
                    }
                } else if !unmodified {
                    self.pointer = None;
                }
                return NativeInput::Handled;
            }
            MouseInput::Release {
                button: MouseButton::Left,
                ..
            } if self.native_pressed => {
                self.native_pressed = false;
                let target = self.anchor_at(frame, metrics, false);
                if let Some(gesture) = self.pointer.as_mut() {
                    let target = target
                        .filter(|target| {
                            target.window == gesture.source.window
                                && target.buffer == gesture.source.buffer
                                && target.changedtick == gesture.source.changedtick
                        })
                        .unwrap_or(gesture.last);
                    gesture.last = target;
                    gesture.latest = Some((target, Vim9PointerPhase::Release));
                    gesture.released = true;
                }
                return NativeInput::Handled;
            }
            _ => self.clear(),
        }
        NativeInput::Compatibility(input)
    }
}

fn pointer_command(
    target: &Vim9BufferPosition,
    gesture: u64,
    phase: Vim9PointerPhase,
) -> Vim9Command {
    Vim9Command::Pointer {
        window: target.window,
        buffer: target.buffer,
        changedtick: target.changedtick,
        line: target.line,
        byte_column: target.byte_column,
        gesture,
        phase,
    }
}

pub(crate) fn local_metrics(metrics: Metrics) -> Vim9RenderMetrics {
    render_metrics(Rectangle::with_size(Size::ZERO), metrics)
}

fn wheel_at(
    frame: &Vim9RenderFrame,
    geometry: Vim9RenderMetrics,
    position: Point,
    delta: mouse::ScrollDelta,
) -> Option<NativeWheel> {
    if frame.mode != "n" {
        return None;
    }
    let (row, column) = geometry.cell_at(position.x, position.y)?;
    let window = frame
        .windows
        .iter()
        .find(|window| window.native_pointer && window.view.contains(row, column))?;
    let lines = match delta {
        mouse::ScrollDelta::Lines { y, .. } => -f64::from(y) * 3.0,
        mouse::ScrollDelta::Pixels { y, .. } => -f64::from(y) / f64::from(geometry.line_height),
    };
    (lines.is_finite() && lines != 0.0).then_some(NativeWheel {
        window: window.window,
        lines,
        buffer: window.buffer,
        changedtick: window.changedtick,
        view: window.view.clone(),
        cell_width: geometry.cell_width,
        line_height: geometry.line_height,
    })
}

pub(crate) fn usable(frame: &Vim9RenderFrame, rows: usize, columns: usize) -> bool {
    frame.supported
        && frame.rows == rows
        && frame.columns == columns
        && frame.paint_views().all(|view| {
            view.rows > 0
                && view.columns > 0
                && view.row.saturating_add(view.rows) <= rows
                && view.column.saturating_add(view.columns) <= columns
        })
}

fn rgb(value: u32) -> Color {
    Color::from_rgb8((value >> 16) as u8, (value >> 8) as u8, value as u8)
}

fn colors(style: &Vim9RenderStyle, normal: &Vim9RenderStyle) -> (Color, Color, Color) {
    let mut fg = rgb(style.foreground.or(normal.foreground).unwrap_or(0xd7dce2));
    let mut bg = rgb(style.background.or(normal.background).unwrap_or(0x15191f));
    if style.reverse {
        std::mem::swap(&mut fg, &mut bg);
    }
    (fg, bg, style.special.map(rgb).unwrap_or(fg))
}

fn quad(bounds: Rectangle) -> Quad {
    Quad {
        bounds,
        ..Quad::default()
    }
}

fn rectangle(rect: Vim9PixelRect) -> Rectangle {
    Rectangle {
        x: rect.x,
        y: rect.y,
        width: rect.width,
        height: rect.height,
    }
}

fn render_metrics(bounds: Rectangle, metrics: Metrics) -> Vim9RenderMetrics {
    Vim9RenderMetrics {
        origin_x: bounds.x + metrics.padding,
        origin_y: bounds.y + metrics.padding,
        cell_width: metrics.cell_w,
        line_height: metrics.cell_h,
    }
}

struct PaintView<'a> {
    clip: Rectangle,
    runs: Vec<Vim9ProjectedRun<'a>>,
    cursor: Option<Rectangle>,
}

/// Prepare each window once, preserving run order for background/text layering.
/// Window identity selects paint ownership; overlapping pixel clips never move
/// a run to another buffer. The same logical-pixel transform is used by the
/// shared semantic anchor API for future GUI overlays.
fn paint_views(
    frame: &Vim9RenderFrame,
    metrics: Vim9RenderMetrics,
    clip: Rectangle,
    offset: impl Fn(i64) -> f32,
) -> Vec<PaintView<'_>> {
    frame
        .windows
        .iter()
        .filter_map(|window| {
            let viewport = Vim9Viewport::new(frame, window.window, metrics, offset(window.window))?;
            let visible = rectangle(viewport.clip())
                .intersection(&clip)
                .filter(|r| r.area() > 0.0)?;
            Some(PaintView {
                clip: visible,
                runs: viewport.runs().collect(),
                cursor: (window.window == frame.window)
                    .then(|| viewport.cursor_rect())
                    .flatten()
                    .map(rectangle)
                    .and_then(|r| r.intersection(&visible)),
            })
        })
        .collect()
}

/// Subtract one contained rectangle without overlapping the remaining bands.
fn outside(bounds: Rectangle, view: Rectangle) -> [Rectangle; 4] {
    [
        Rectangle {
            height: (view.y - bounds.y).max(0.0),
            ..bounds
        },
        Rectangle {
            y: view.y + view.height,
            height: (bounds.y + bounds.height - view.y - view.height).max(0.0),
            ..bounds
        },
        Rectangle {
            x: bounds.x,
            y: view.y,
            width: (view.x - bounds.x).max(0.0),
            height: view.height,
        },
        Rectangle {
            x: view.x + view.width,
            y: view.y,
            width: (bounds.x + bounds.width - view.x - view.width).max(0.0),
            height: view.height,
        },
    ]
}

/// Preserve every split separator, status row and outer margin. Subtracting
/// each window from the disjoint bands also works for mixed horizontal and
/// vertical splits; no terminal glyph can later overpaint a native buffer.
fn compatibility_bands(bounds: Rectangle, views: &[Rectangle]) -> Vec<Rectangle> {
    let mut bands = vec![bounds];
    for view in views {
        bands = bands
            .into_iter()
            .flat_map(|band| match band.intersection(view) {
                Some(covered) if covered.area() > 0.0 => outside(band, covered)
                    .into_iter()
                    .filter(|part| part.width > 0.0 && part.height > 0.0)
                    .collect::<Vec<_>>(),
                _ => vec![band],
            })
            .collect();
    }
    bands
}

pub(crate) struct NativeBuffer<'a, Message> {
    input: TermWidget<'a, Message>,
    frame: Option<&'a Arc<Vim9RenderFrame>>,
    metrics: Metrics,
    font: Font,
    focused: bool,
    blink_on: bool,
    scroll: Option<&'a RefCell<ScrollState>>,
    modifiers: keyboard::Modifiers,
    on_scroll: Option<Box<dyn Fn(NativeWheel) -> Message + 'a>>,
    precision_enabled: bool,
}

impl<'a, Message> NativeBuffer<'a, Message> {
    pub(crate) fn new(
        input: TermWidget<'a, Message>,
        frame: Option<&'a Arc<Vim9RenderFrame>>,
        metrics: Metrics,
        font: Font,
        focused: bool,
        blink_on: bool,
    ) -> Self {
        Self {
            input,
            frame,
            metrics,
            font,
            focused,
            blink_on,
            scroll: None,
            modifiers: keyboard::Modifiers::default(),
            on_scroll: None,
            precision_enabled: true,
        }
    }

    pub(crate) fn scroll(
        mut self,
        state: &'a RefCell<ScrollState>,
        modifiers: keyboard::Modifiers,
        precision_enabled: bool,
        on_scroll: impl Fn(NativeWheel) -> Message + 'a,
    ) -> Self {
        self.scroll = Some(state);
        self.modifiers = modifiers;
        self.precision_enabled = precision_enabled;
        self.on_scroll = Some(Box::new(on_scroll));
        self
    }
}

impl<Message, Renderer> Widget<Message, iced::Theme, Renderer> for NativeBuffer<'_, Message>
where
    Renderer: text::Renderer<Font = Font>
        + iced::advanced::image::Renderer<Handle = iced::advanced::image::Handle>,
{
    fn tag(&self) -> tree::Tag {
        <TermWidget<'_, Message> as Widget<Message, iced::Theme, Renderer>>::tag(&self.input)
    }
    fn state(&self) -> tree::State {
        <TermWidget<'_, Message> as Widget<Message, iced::Theme, Renderer>>::state(&self.input)
    }
    fn diff(&self, tree: &mut Tree) {
        <TermWidget<'_, Message> as Widget<Message, iced::Theme, Renderer>>::diff(
            &self.input,
            tree,
        );
    }
    fn size(&self) -> Size<Length> {
        Size::new(Length::Fill, Length::Fill)
    }
    fn layout(
        &mut self,
        tree: &mut Tree,
        renderer: &Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        self.input.layout(tree, renderer, limits)
    }
    fn update(
        &mut self,
        tree: &mut Tree,
        event: &Event,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        renderer: &Renderer,
        clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, Message>,
        viewport: &Rectangle,
    ) {
        if let Some(state) = self.scroll {
            state.borrow_mut().position = cursor.position();
            if matches!(
                event,
                Event::Keyboard(_)
                    | Event::InputMethod(_)
                    | Event::Window(iced::window::Event::Unfocused)
            ) {
                state.borrow_mut().clear();
            }
            // Iced asks for IME geometry on RedrawRequested immediately
            // before drawing. Resolve from that same immutable projection;
            // pointer events continue to use the last image actually drawn.
            if let Some(frame) = self.frame {
                let caret = state
                    .borrow()
                    .projection(
                        frame,
                        render_metrics(layout.bounds(), self.metrics),
                        local_metrics(self.metrics),
                        layout
                            .bounds()
                            .intersection(viewport)
                            .unwrap_or(Rectangle::with_size(Size::ZERO)),
                    )
                    .caret();
                self.input.set_native_ime_cursor(caret);
            }
        }
        if self.focused
            && self.precision_enabled
            && !self.modifiers.control()
            && !self.modifiers.shift()
            && !self.modifiers.alt()
            && !self.modifiers.logo()
        {
            if let (
                Event::Mouse(mouse::Event::WheelScrolled { delta }),
                Some(callback),
                Some(position),
            ) = (
                event,
                self.on_scroll.as_ref(),
                cursor.position_over(layout.bounds()),
            ) {
                if viewport.contains(position) {
                    let wheel = self.scroll.and_then(|state| {
                        let state = state.borrow();
                        let presented = state.presented.as_ref()?;
                        wheel_at(&presented.frame, presented.metrics, position, *delta)
                    });
                    if let Some(wheel) = wheel {
                        shell.publish(callback(wheel));
                        shell.capture_event();
                        return;
                    }
                }
            }
        }
        self.input.update(
            tree, event, layout, cursor, renderer, clipboard, shell, viewport,
        );
    }
    fn mouse_interaction(
        &self,
        tree: &Tree,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
        renderer: &Renderer,
    ) -> mouse::Interaction {
        self.input
            .mouse_interaction(tree, layout, cursor, viewport, renderer)
    }
    fn draw(
        &self,
        tree: &Tree,
        renderer: &mut Renderer,
        theme: &iced::Theme,
        style: &renderer::Style,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
    ) {
        let bounds = layout.bounds();
        let Some(clip) = bounds.intersection(viewport) else {
            return;
        };
        let Some(frame) = self.frame else {
            self.input
                .draw(tree, renderer, theme, style, layout, cursor, viewport);
            if let Some(state) = self.scroll {
                state.borrow_mut().presented = None;
            }
            return;
        };
        let m = self.metrics;
        let geometry = render_metrics(bounds, m);
        if !geometry.is_valid() {
            self.input
                .draw(tree, renderer, theme, style, layout, cursor, viewport);
            if let Some(state) = self.scroll {
                state.borrow_mut().presented = None;
            }
            return;
        }
        let views = {
            let scroll = self.scroll.map(RefCell::borrow);
            paint_views(frame, geometry, clip, |window| {
                scroll.as_ref().map_or(0.0, |state| {
                    state.offset(frame, window, local_metrics(self.metrics))
                })
            })
        };
        if views.is_empty() {
            self.input
                .draw(tree, renderer, theme, style, layout, cursor, viewport);
            if let Some(state) = self.scroll {
                state.borrow_mut().presented = None;
            }
            return;
        }
        let rectangles: Vec<_> = views.iter().map(|view| view.clip).collect();
        for band in compatibility_bands(clip, &rectangles) {
            renderer.with_layer(band, |renderer| {
                self.input
                    .draw(tree, renderer, theme, style, layout, cursor, &band);
            });
        }
        for prepared in views {
            self.paint_native_view(renderer, frame, &prepared);
        }
        if let Some(state) = self.scroll {
            state.borrow_mut().record_presentation(
                frame,
                geometry,
                local_metrics(self.metrics),
                clip,
            );
        }
    }
}

impl<Message> NativeBuffer<'_, Message> {
    fn paint_native_view<Renderer>(
        &self,
        renderer: &mut Renderer,
        frame: &Vim9RenderFrame,
        prepared: &PaintView<'_>,
    ) where
        Renderer: text::Renderer<Font = Font>,
    {
        let m = self.metrics;
        let view = prepared.clip;
        let (normal_fg, normal_bg, _) = colors(&frame.default_style, &frame.default_style);
        renderer.with_layer(view, |renderer| {
            renderer.fill_quad(quad(view), Background::Color(normal_bg));
            // Keep italic overhang intact across adjacent highlight runs.
            for projected in &prepared.runs {
                let run = projected.run;
                let area = rectangle(projected.rect);
                let (_, bg, _) = colors(&run.style, &frame.default_style);
                renderer.fill_quad(quad(area), Background::Color(bg));
            }
            for projected in &prepared.runs {
                let run = projected.run;
                let area = rectangle(projected.rect);
                let text_area = Rectangle {
                    x: area.x - m.font_size * 0.25,
                    width: area.width + m.font_size * 0.5,
                    ..area
                };
                let Some(run_clip) = text_area.intersection(&view) else {
                    continue;
                };
                let (fg, _, special) = colors(&run.style, &frame.default_style);
                let font = crate::terminal_view::native_editor_glyph_font(
                    self.font,
                    &run.text,
                    run.style.bold,
                    run.style.italic,
                );
                let mut draw_text = |content: String, x: f32| {
                    renderer.fill_text(
                        Text {
                            content,
                            // iced 0.14's text cache can wrap despite Wrapping::None
                            // at a finite rounded grid width. Clipping belongs to
                            // the native run/view, never to the shaping bounds.
                            bounds: Size::new(f32::INFINITY, m.cell_h),
                            size: Pixels(m.font_size),
                            line_height: text::LineHeight::Absolute(Pixels(m.cell_h)),
                            font,
                            align_x: text::Alignment::Left,
                            align_y: iced::alignment::Vertical::Center,
                            shaping: text::Shaping::Advanced,
                            wrapping: text::Wrapping::None,
                        },
                        Point::new(x, area.center_y()),
                        fg,
                        run_clip,
                    );
                };
                // A legal configured face or its bold/italic fallback need
                // not have the measured primary advance. Preserve native GUI
                // glyphs/colors but place ASCII at Vim's authoritative columns.
                if run.text.is_ascii()
                    && run.text.len() == run.cells
                    && (!m.mono_advance_exact || font != self.font)
                {
                    for (column, ch) in run.text.chars().enumerate() {
                        draw_text(ch.to_string(), area.x + column as f32 * m.cell_w);
                    }
                } else {
                    draw_text(run.text.clone(), area.x);
                }
                let thickness = (m.font_size / 14.0).clamp(1.0, 2.0);
                let underline_y = area.y + area.height - thickness * 1.5;
                if run.style.undercurl {
                    let step = (m.font_size / 5.0).max(2.0);
                    let mut x = area.x;
                    let mut up = true;
                    while x < area.x + area.width {
                        renderer.fill_quad(
                            quad(Rectangle {
                                x,
                                y: underline_y - if up { thickness } else { 0.0 },
                                width: step.min(area.x + area.width - x),
                                height: thickness,
                            }),
                            Background::Color(special),
                        );
                        x += step;
                        up = !up;
                    }
                } else if run.style.underline {
                    renderer.fill_quad(
                        quad(Rectangle {
                            y: underline_y,
                            height: thickness,
                            ..area
                        }),
                        Background::Color(special),
                    );
                }
                if run.style.strikethrough {
                    renderer.fill_quad(
                        quad(Rectangle {
                            y: area.center_y(),
                            height: thickness,
                            ..area
                        }),
                        Background::Color(special),
                    );
                }
            }
            if let Some(cursor) = prepared.cursor.filter(|_| !self.focused || self.blink_on) {
                if self.focused && frame.mode.starts_with('i') {
                    renderer.fill_quad(
                        quad(Rectangle {
                            width: 2.0,
                            ..cursor
                        }),
                        Background::Color(normal_fg),
                    );
                } else if self.focused && frame.mode.starts_with('R') {
                    let height = (cursor.height * 0.2).max(2.0);
                    renderer.fill_quad(
                        quad(Rectangle {
                            y: cursor.y + cursor.height - height,
                            height,
                            ..cursor
                        }),
                        Background::Color(normal_fg),
                    );
                } else {
                    renderer.fill_quad(
                        Quad {
                            bounds: cursor,
                            border: iced::Border {
                                color: normal_fg,
                                width: 1.0,
                                radius: 1.0.into(),
                            },
                            ..Quad::default()
                        },
                        Background::Color(Color {
                            a: if self.focused { 0.18 } else { 0.0 },
                            ..normal_fg
                        }),
                    );
                }
            }
        });
    }
}

impl<'a, Message: 'a, Renderer> From<NativeBuffer<'a, Message>>
    for Element<'a, Message, iced::Theme, Renderer>
where
    Renderer: text::Renderer<Font = Font>
        + iced::advanced::image::Renderer<Handle = iced::advanced::image::Handle>
        + 'a,
{
    fn from(widget: NativeBuffer<'a, Message>) -> Self {
        Self::new(widget)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn split_frame() -> Vim9RenderFrame {
        serde_json::from_value(serde_json::json!({
            "revision": 7, "supported": true, "rows": 20, "columns": 80,
            "window": 10, "buffer": 3, "changedtick": 8,
            "view": {"row": 1, "column": 0, "rows": 17, "columns": 39},
            "windows": [
                {"window": 10, "buffer": 3, "changedtick": 8,
                 "view": {"row": 1, "column": 0, "rows": 17, "columns": 39}},
                {"window": 20, "buffer": 4, "changedtick": 2,
                 "view": {"row": 1, "column": 40, "rows": 17, "columns": 40}}
            ],
            "runs": [
                {"window": 10, "row": 1, "column": 2, "text": "界", "cells": 2,
                 "line": 1, "byte_start": 0, "byte_end": 3},
                {"window": 20, "row": 1, "column": 42, "text": "peer", "cells": 4,
                 "line": 8, "byte_start": 0, "byte_end": 4},
                {"window": 10, "row": 2, "column": 2, "text": "next", "cells": 4,
                 "line": 2, "byte_start": 0, "byte_end": 4}
            ]
        }))
        .unwrap()
    }

    fn direct_frame() -> Vim9RenderFrame {
        let mut frame = split_frame();
        frame.mode = "n".into();
        frame.window = 20;
        for window in &mut frame.windows {
            window.native_pointer = true;
            window.topline = if window.window == 20 { 8 } else { 1 };
            window.botline = window.topline + window.view.rows as u64;
            window.line_count = 100;
            window.line_lengths = vec![0; window.view.rows];
            window.line_lengths[0] = if window.window == 20 { 4 } else { 3 };
            window.line_lengths[1] = 4;
            window.overscan = vec![jterm_core::vim9::Vim9RenderMarginRow {
                row: window.view.rows as i32,
                line: window.topline + window.view.rows as u64,
                byte_length: 4,
                runs: vec![jterm_core::vim9::Vim9RenderRun {
                    window: window.window,
                    column: window.view.column + 2,
                    text: "edge".into(),
                    cells: 4,
                    line: window.topline + window.view.rows as u64,
                    byte_start: 0,
                    byte_end: 4,
                    ..Default::default()
                }],
            }];
        }
        frame.runs.push(jterm_core::vim9::Vim9RenderRun {
            window: 20,
            row: 2,
            column: 42,
            text: "next".into(),
            cells: 4,
            line: 9,
            byte_start: 0,
            byte_end: 4,
            ..Default::default()
        });
        frame.windows[1].cursor = jterm_core::vim9::Vim9RenderCursor {
            row: 2,
            column: 42,
            width: 1,
            visible: true,
            line: 9,
            byte_column: 0,
        };
        frame.cursor = frame.windows[1].cursor.clone();
        frame
    }

    fn geometry() -> Vim9RenderMetrics {
        Vim9RenderMetrics {
            origin_x: 0.0,
            origin_y: 0.0,
            cell_width: 8.0,
            line_height: 20.0,
        }
    }

    fn wheel(lines: f64) -> NativeWheel {
        wheel_at(
            &direct_frame(),
            geometry(),
            Point::new(340.0, 35.0),
            mouse::ScrollDelta::Pixels {
                x: 0.0,
                y: -(lines as f32) * 20.0,
            },
        )
        .unwrap()
    }

    fn press(count: u32) -> MouseInput {
        MouseInput::Press {
            col: 42,
            row: 2,
            button: MouseButton::Left,
            shift: false,
            alt: false,
            ctrl: false,
            count,
            finalized: false,
            link_eligible: false,
            app_eligible: true,
            block: None,
            block_zone_id: None,
            link: false,
            link_revision: 0,
            x: 340.0,
            y: 35.0,
        }
    }

    #[test]
    fn precision_wheel_uses_hovered_split_and_rejects_non_buffer_pixels() {
        let frame = direct_frame();
        let geometry = Vim9RenderMetrics {
            origin_x: 3.25,
            origin_y: 5.5,
            cell_width: 9.5,
            line_height: 22.5,
        };
        let position = Point::new(geometry.origin_x + 45.0 * geometry.cell_width, 50.0);
        let pixels = wheel_at(
            &frame,
            geometry,
            position,
            mouse::ScrollDelta::Pixels { x: 0.0, y: -4.5 },
        )
        .unwrap();
        assert_eq!(
            pixels.window, 20,
            "scroll must follow hover, not active Vim window"
        );
        assert!((pixels.lines - 0.2).abs() < 1e-6);
        let notch = wheel_at(
            &frame,
            geometry,
            position,
            mouse::ScrollDelta::Lines { x: 0.0, y: 1.0 },
        )
        .unwrap();
        assert_eq!(notch.lines, -3.0);
        for point in [
            Point::new(0.0, 50.0),
            Point::new(geometry.origin_x + 39.5 * geometry.cell_width, 50.0),
            Point::new(position.x, geometry.origin_y + 18.5 * geometry.line_height),
        ] {
            assert!(wheel_at(
                &frame,
                geometry,
                point,
                mouse::ScrollDelta::Lines { x: 0.0, y: -1.0 }
            )
            .is_none());
        }
        assert!(wheel_at(
            &frame,
            geometry,
            position,
            mouse::ScrollDelta::Pixels { x: 10.0, y: 0.0 }
        )
        .is_none());
        assert!(wheel_at(
            &frame,
            geometry,
            position,
            mouse::ScrollDelta::Pixels {
                x: 0.0,
                y: f32::NAN
            }
        )
        .is_none());
    }

    #[test]
    fn direct_fraction_is_presented_before_vim_reply_and_rebases_once() {
        let frame = direct_frame();
        let metrics = geometry();
        let mut state = ScrollState::default();
        state.wheel(&frame, metrics, wheel(0.25));
        assert_eq!(state.offset(&frame, 20, metrics), -5.0);
        assert_eq!(state.offset(&frame, 10, metrics), 0.0);
        assert!(state.scroll_commands().is_empty());
        state.observe(Some(&frame), metrics, true, |_| false);
        assert_eq!(state.offset(&frame, 20, metrics), -5.0);
        state.wheel(&frame, metrics, wheel(1.0));
        assert!(matches!(
            state.scroll_commands().as_slice(),
            [(
                _,
                Vim9Command::ScrollView {
                    lines: 1,
                    topline: 8,
                    ..
                }
            )]
        ));
        state.scroll_submitted(20, 41);
        state.wheel(&frame, metrics, wheel(0.25));
        assert!(
            state.scroll_commands().is_empty(),
            "one request per split may be in flight"
        );
        state.observe(Some(&frame), metrics, true, |id| id == 41);
        assert_eq!(
            state.offset(&frame, 20, metrics),
            -20.0,
            "presentation is bounded to sampled margins"
        );
        let mut acknowledged = frame.clone();
        acknowledged.windows[1].topline += 1;
        acknowledged.windows[1].scroll_ack = 41;
        state.observe(Some(&acknowledged), metrics, true, |_| false);
        assert_eq!(state.offset(&acknowledged, 20, metrics), -10.0);
        assert!(state.scroll_commands().is_empty());
        state.observe(Some(&acknowledged), metrics, true, |_| false);
        assert_eq!(
            state.offset(&acknowledged, 20, metrics),
            -10.0,
            "a repeated frame cannot double-rebase"
        );
    }

    #[test]
    fn queued_wheel_survives_its_ack_but_cannot_scroll_a_replacement_source() {
        let old = direct_frame();
        let metrics = geometry();
        let event = wheel(0.25);
        let mut next = old.clone();
        next.windows[1].topline += 1;
        next.windows[1].scroll_ack = 22;
        let mut state = ScrollState::default();
        state.wheel(&next, metrics, event.clone());
        assert_eq!(state.offset(&next, 20, metrics), -5.0);
        for case in 0..4 {
            let mut frame = next.clone();
            let mut changed_metrics = metrics;
            match case {
                0 => frame.windows[1].buffer += 1,
                1 => frame.windows[1].changedtick += 1,
                2 => frame.windows[1].view.column += 1,
                _ => changed_metrics.line_height += 1.0,
            }
            let mut state = ScrollState::default();
            state.wheel(&frame, changed_metrics, event.clone());
            assert!(state.pixels.is_empty());
        }
    }

    #[test]
    fn moving_text_paint_source_hit_and_caret_share_one_projection() {
        let frame = direct_frame();
        let metrics = geometry();
        let mut state = ScrollState::default();
        state.wheel(&frame, metrics, wheel(0.5));
        let views = paint_views(
            &frame,
            metrics,
            Rectangle::with_size(Size::new(640.0, 400.0)),
            |window| state.offset(&frame, window, metrics),
        );
        let view = &views[1];
        let text = view.runs.iter().find(|p| p.run.text == "next").unwrap();
        assert_eq!(text.rect.y, 30.0);
        assert_eq!(view.cursor.unwrap().y, text.rect.y);
        assert!(
            view.runs.iter().any(|p| p.run.text == "edge"),
            "fractional bottom edge uses sampled text"
        );
        state.record_presentation(
            &Arc::new(frame.clone()),
            metrics,
            metrics,
            Rectangle::with_size(Size::new(640.0, 400.0)),
        );
        state.position = Some(Point::new(340.0, 35.0));
        assert!(matches!(
            state.route_mouse(&frame, metrics, keyboard::Modifiers::default(), press(1)),
            NativeInput::Handled
        ));
        assert!(matches!(
            state.pointer_command(),
            Some(Vim9Command::Pointer {
                line: 9,
                byte_column: 0,
                phase: Vim9PointerPhase::Press,
                ..
            })
        ));
        assert_eq!(
            state.offset(&frame, 20, metrics),
            -10.0,
            "native press must not snap its captured image"
        );
    }

    #[test]
    fn pointer_coalesces_motion_and_keeps_press_release_order() {
        let frame = direct_frame();
        let metrics = geometry();
        let mut state = ScrollState::default();
        state.wheel(&frame, metrics, wheel(0.5));
        state.record_presentation(
            &Arc::new(frame.clone()),
            metrics,
            metrics,
            Rectangle::with_size(Size::new(640.0, 400.0)),
        );
        state.position = Some(Point::new(340.0, 35.0));
        state.route_mouse(&frame, metrics, keyboard::Modifiers::default(), press(1));
        state.pointer_submitted(51);
        for x in [348.0, 356.0, 364.0] {
            state.position = Some(Point::new(x, 35.0));
            state.route_mouse(
                &frame,
                metrics,
                keyboard::Modifiers::default(),
                MouseInput::Drag {
                    col: 45,
                    row: 2,
                    count: 1,
                },
            );
        }
        assert!(state.pointer_command().is_none());
        state.position = None;
        state.route_mouse(
            &frame,
            metrics,
            keyboard::Modifiers::default(),
            MouseInput::Release {
                col: 79,
                row: 19,
                button: MouseButton::Left,
            },
        );
        state.observe(Some(&frame), metrics, true, |_| false);
        assert!(
            matches!(
                state.pointer_command(),
                Some(Vim9Command::Pointer {
                    line: 9,
                    byte_column: 3,
                    phase: Vim9PointerPhase::Release,
                    ..
                })
            ),
            "release outside the split must use the last valid captured source"
        );
        state.pointer_submitted(52);
        state.observe(Some(&frame), metrics, true, |_| false);
        assert!(state.pointer.is_none());
    }

    #[test]
    fn cancelled_native_press_never_leaks_an_unpaired_compatibility_drag() {
        let frame = direct_frame();
        let metrics = geometry();
        let mut state = ScrollState::default();
        state.wheel(&frame, metrics, wheel(0.5));
        state.record_presentation(
            &Arc::new(frame.clone()),
            metrics,
            metrics,
            Rectangle::with_size(Size::new(640.0, 400.0)),
        );
        state.position = Some(Point::new(340.0, 35.0));
        state.route_mouse(&frame, metrics, keyboard::Modifiers::default(), press(1));
        let mut edited = frame.clone();
        edited.windows[1].changedtick += 1;
        state.observe(Some(&edited), metrics, true, |_| false);
        assert!(state.pointer_command().is_none());
        assert!(state
            .compatibility_mouse(MouseInput::Drag {
                col: 45,
                row: 2,
                count: 1
            })
            .is_none());
        assert!(state
            .compatibility_mouse(MouseInput::Release {
                col: 45,
                row: 2,
                button: MouseButton::Left
            })
            .is_none());
        state.record_presentation(
            &Arc::new(edited),
            metrics,
            metrics,
            Rectangle::with_size(Size::new(640.0, 400.0)),
        );
        assert!(state.compatibility_mouse(press(1)).is_some());
        state.wheel(&frame, metrics, wheel(0.5));
        state.record_presentation(
            &Arc::new(frame.clone()),
            metrics,
            metrics,
            Rectangle::with_size(Size::new(640.0, 400.0)),
        );
        state.position = Some(Point::new(340.0, 35.0));
        state.route_mouse(&frame, metrics, keyboard::Modifiers::default(), press(1));
        state.clear(); // Escape, preedit, resize, zoom or focus loss.
        assert!(matches!(
            state.route_mouse(
                &frame,
                metrics,
                keyboard::Modifiers::default(),
                MouseInput::Drag {
                    col: 45,
                    row: 2,
                    count: 1
                }
            ),
            NativeInput::Handled
        ));
        assert!(state.pointer_command().is_none());
    }

    #[test]
    fn modifiers_multiclick_and_unadvertised_pointer_keep_compatibility_semantics() {
        let frame = direct_frame();
        let metrics = geometry();
        for (count, modifiers) in [
            (2, keyboard::Modifiers::default()),
            (3, keyboard::Modifiers::default()),
            (1, keyboard::Modifiers::SHIFT),
            (1, keyboard::Modifiers::CTRL),
            (1, keyboard::Modifiers::ALT),
        ] {
            let mut state = ScrollState::default();
            state.record_presentation(
                &Arc::new(frame.clone()),
                metrics,
                metrics,
                Rectangle::with_size(Size::new(640.0, 400.0)),
            );
            state.position = Some(Point::new(340.0, 35.0));
            assert!(
                matches!(state.route_mouse(&frame, metrics, modifiers, press(count)),
                NativeInput::Compatibility(MouseInput::Press { count: actual, .. }) if count == actual)
            );
            assert!(state.pointer_command().is_none());
        }
        let mut unsupported = frame.clone();
        unsupported.windows[1].native_pointer = false;
        let mut state = ScrollState {
            position: Some(Point::new(340.0, 45.0)),
            ..Default::default()
        };
        assert!(matches!(
            state.route_mouse(
                &unsupported,
                metrics,
                keyboard::Modifiers::default(),
                press(1)
            ),
            NativeInput::Compatibility(_)
        ));
        assert!(wheel_at(
            &unsupported,
            metrics,
            Point::new(340.0, 45.0),
            mouse::ScrollDelta::Pixels { x: 0.0, y: -3.0 }
        )
        .is_none());
    }

    #[test]
    fn unsupported_fractional_press_settles_and_consumes_its_tail() {
        let frame = direct_frame();
        let metrics = geometry();
        for (count, modifiers) in [
            (2, keyboard::Modifiers::default()),
            (1, keyboard::Modifiers::SHIFT),
        ] {
            let mut state = ScrollState::default();
            state.wheel(&frame, metrics, wheel(0.5));
            state.record_presentation(
                &Arc::new(frame.clone()),
                metrics,
                metrics,
                Rectangle::with_size(Size::new(640.0, 400.0)),
            );
            state.position = Some(Point::new(340.0, 35.0));
            assert!(matches!(
                state.route_mouse(&frame, metrics, modifiers, press(count)),
                NativeInput::Handled
            ));
            assert!(state.pixels.is_empty());
            assert!(state.pointer_command().is_none());
            assert!(state
                .compatibility_mouse(MouseInput::Release {
                    col: 42,
                    row: 1,
                    button: MouseButton::Left
                })
                .is_none());
            state.record_presentation(
                &Arc::new(frame.clone()),
                metrics,
                metrics,
                Rectangle::with_size(Size::new(640.0, 400.0)),
            );
            assert!(matches!(
                state.route_mouse(&frame, metrics, modifiers, press(count)),
                NativeInput::Compatibility(_)
            ));
        }
    }

    #[test]
    fn changed_source_under_fractional_image_consumes_click_instead_of_retargeting() {
        let frame = direct_frame();
        let metrics = geometry();
        let mut state = ScrollState::default();
        state.wheel(&frame, metrics, wheel(0.5));
        state.record_presentation(
            &Arc::new(frame.clone()),
            metrics,
            metrics,
            Rectangle::with_size(Size::new(640.0, 400.0)),
        );
        state.position = Some(Point::new(340.0, 35.0));
        let mut next = frame.clone();
        next.windows[1].buffer += 1;
        assert!(matches!(
            state.route_mouse(&next, metrics, keyboard::Modifiers::default(), press(1)),
            NativeInput::Handled
        ));
        assert!(state.pointer_command().is_none());
        assert!(state
            .compatibility_mouse(MouseInput::Drag {
                col: 45,
                row: 2,
                count: 1
            })
            .is_none());
    }

    #[test]
    fn stationary_click_stays_in_vim_and_unpainted_ack_cannot_retarget_hit() {
        let frame = direct_frame();
        let metrics = geometry();
        let mut state = ScrollState::default();
        state.record_presentation(
            &Arc::new(frame.clone()),
            metrics,
            metrics,
            Rectangle::with_size(Size::new(640.0, 400.0)),
        );
        state.position = Some(Point::new(340.0, 35.0));
        assert!(matches!(
            state.route_mouse(&frame, metrics, keyboard::Modifiers::default(), press(1)),
            NativeInput::Compatibility(_)
        ));
        state.wheel(&frame, metrics, wheel(0.5));
        state.record_presentation(
            &Arc::new(frame.clone()),
            metrics,
            metrics,
            Rectangle::with_size(Size::new(640.0, 400.0)),
        );
        let mut newer = frame.clone();
        newer.revision += 1;
        newer.windows[1].topline += 1;
        newer
            .runs
            .iter_mut()
            .filter(|r| r.window == 20)
            .for_each(|r| r.line += 1);
        state.position = Some(Point::new(340.0, 35.0));
        assert!(matches!(
            state.route_mouse(&newer, metrics, keyboard::Modifiers::default(), press(1)),
            NativeInput::Handled
        ));
        assert!(
            matches!(
                state.pointer_command(),
                Some(Vim9Command::Pointer {
                    line: 9,
                    byte_column: 0,
                    ..
                })
            ),
            "hit must follow the drawn row, even when a newer ack arrived before paint"
        );
    }

    #[test]
    fn native_widget_projects_raw_mouse_and_ime_from_the_submitted_image() {
        #[derive(Debug, Clone)]
        enum Output {
            Mouse(NativeInput),
            Wheel(NativeWheel),
        }
        let frame = Arc::new(direct_frame());
        let state = RefCell::new(ScrollState::default());
        let mut terminal = crate::terminal::TerminalState::new(80, 20);
        let projection = terminal.get_projected_viewport(false);
        let theme = crate::Theme::default();
        let mut metrics = Metrics::new(14.0, 1.0, 0.0, false);
        metrics.cell_w = 8.0;
        metrics.cell_h = 20.0;
        let input = TermWidget::new(
            projection.cells(),
            (2, 42),
            true,
            crate::terminal::CursorShape::Block,
            true,
            &theme,
            metrics,
            Font::MONOSPACE,
            None,
            None,
            None,
            None,
            Vec::new(),
            0,
            0,
        )
        .app_mouse_full_grid(true)
        .on_mouse(|input| {
            Output::Mouse(state.borrow_mut().route_mouse(
                &frame,
                local_metrics(metrics),
                keyboard::Modifiers::default(),
                input,
            ))
        });
        let mut widget =
            NativeBuffer::new(input, Some(&frame), metrics, Font::MONOSPACE, true, true).scroll(
                &state,
                keyboard::Modifiers::default(),
                true,
                Output::Wheel,
            );
        let mut tree = Tree::new(&widget as &dyn Widget<Output, iced::Theme, ()>);
        let bounds = Rectangle::with_size(Size::new(640.0, 400.0));
        let node = layout::Node::new(bounds.size());
        let point = Point::new(340.0, 35.0);
        <NativeBuffer<'_, Output> as Widget<Output, iced::Theme, ()>>::draw(
            &widget,
            &tree,
            &mut (),
            &iced::Theme::Dark,
            &renderer::Style::default(),
            Layout::new(&node),
            mouse::Cursor::Available(point),
            &bounds,
        );
        let mut output = Vec::new();
        <NativeBuffer<'_, Output> as Widget<Output, iced::Theme, ()>>::update(
            &mut widget,
            &mut tree,
            &Event::Mouse(mouse::Event::WheelScrolled {
                delta: mouse::ScrollDelta::Pixels { x: 0.0, y: -10.0 },
            }),
            Layout::new(&node),
            mouse::Cursor::Available(point),
            &(),
            &mut iced::advanced::clipboard::Null,
            &mut Shell::new(&mut output),
            &bounds,
        );
        let Output::Wheel(wheel) = output.remove(0) else {
            panic!("native raw pixel wheel expected");
        };
        assert_eq!(wheel.lines, 0.5);
        state
            .borrow_mut()
            .wheel(&frame, local_metrics(metrics), wheel);
        let mut shell = Shell::new(&mut output);
        <NativeBuffer<'_, Output> as Widget<Output, iced::Theme, ()>>::update(
            &mut widget,
            &mut tree,
            &Event::Window(iced::window::Event::RedrawRequested(
                std::time::Instant::now(),
            )),
            Layout::new(&node),
            mouse::Cursor::Available(point),
            &(),
            &mut iced::advanced::clipboard::Null,
            &mut shell,
            &bounds,
        );
        assert!(
            matches!(shell.input_method(), iced::advanced::input_method::InputMethod::Enabled { cursor, .. }
            if cursor.y == 30.0 && cursor.x == 336.0)
        );
        <NativeBuffer<'_, Output> as Widget<Output, iced::Theme, ()>>::draw(
            &widget,
            &tree,
            &mut (),
            &iced::Theme::Dark,
            &renderer::Style::default(),
            Layout::new(&node),
            mouse::Cursor::Available(point),
            &bounds,
        );
        for event in [
            mouse::Event::ButtonPressed(mouse::Button::Left),
            mouse::Event::ButtonReleased(mouse::Button::Left),
        ] {
            <NativeBuffer<'_, Output> as Widget<Output, iced::Theme, ()>>::update(
                &mut widget,
                &mut tree,
                &Event::Mouse(event),
                Layout::new(&node),
                mouse::Cursor::Available(point),
                &(),
                &mut iced::advanced::clipboard::Null,
                &mut Shell::new(&mut output),
                &bounds,
            );
        }
        state.borrow_mut().clear();
        <NativeBuffer<'_, Output> as Widget<Output, iced::Theme, ()>>::draw(
            &widget,
            &tree,
            &mut (),
            &iced::Theme::Dark,
            &renderer::Style::default(),
            Layout::new(&node),
            mouse::Cursor::Available(point),
            &bounds,
        );
        <NativeBuffer<'_, Output> as Widget<Output, iced::Theme, ()>>::update(
            &mut widget,
            &mut tree,
            &Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)),
            Layout::new(&node),
            mouse::Cursor::Available(point),
            &(),
            &mut iced::advanced::clipboard::Null,
            &mut Shell::new(&mut output),
            &bounds,
        );
        assert!(matches!(output[0], Output::Mouse(NativeInput::Handled)));
        assert!(matches!(output[1], Output::Mouse(NativeInput::Handled)));
        assert!(
            matches!(
                output[2],
                Output::Mouse(NativeInput::Compatibility(MouseInput::Press {
                    count: 2,
                    ..
                }))
            ),
            "the real terminal widget must see the first click when counting a multi-click"
        );
    }

    #[test]
    fn fallback_draw_retires_the_old_fractional_image_but_consumes_its_press_tail() {
        let frame = Arc::new(direct_frame());
        let mut state = ScrollState::default();
        let bounds = Rectangle::with_size(Size::new(640.0, 400.0));
        state.wheel(&frame, geometry(), wheel(0.5));
        state.record_presentation(&frame, geometry(), geometry(), bounds);
        state.clear();
        let state = RefCell::new(state);
        let mut terminal = crate::terminal::TerminalState::new(80, 20);
        let projection = terminal.get_projected_viewport(false);
        let theme = crate::Theme::default();
        let mut metrics = Metrics::new(14.0, 1.0, 0.0, false);
        metrics.cell_w = 8.0;
        metrics.cell_h = 20.0;
        let input = TermWidget::new(
            projection.cells(),
            (2, 42),
            true,
            crate::terminal::CursorShape::Block,
            true,
            &theme,
            metrics,
            Font::MONOSPACE,
            None,
            None,
            None,
            None,
            Vec::new(),
            0,
            0,
        )
        .app_mouse_full_grid(true)
        .on_mouse(|input| state.borrow_mut().compatibility_mouse(input));
        let mut widget = NativeBuffer::new(input, None, metrics, Font::MONOSPACE, true, true)
            .scroll(&state, keyboard::Modifiers::default(), true, |_| None);
        let mut tree = Tree::new(&widget as &dyn Widget<Option<MouseInput>, iced::Theme, ()>);
        let node = layout::Node::new(bounds.size());
        let point = Point::new(340.0, 35.0);
        let mut output = Vec::new();
        <NativeBuffer<'_, Option<MouseInput>> as Widget<Option<MouseInput>, iced::Theme, ()>>::update(
            &mut widget,
            &mut tree,
            &Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)),
            Layout::new(&node),
            mouse::Cursor::Available(point),
            &(),
            &mut iced::advanced::clipboard::Null,
            &mut Shell::new(&mut output),
            &bounds,
        );
        assert_eq!(output.len(), 1);
        assert!(
            output[0].is_none(),
            "the old moving image still owns this press"
        );
        <NativeBuffer<'_, Option<MouseInput>> as Widget<Option<MouseInput>, iced::Theme, ()>>::draw(
            &widget,
            &tree,
            &mut (),
            &iced::Theme::Dark,
            &renderer::Style::default(),
            Layout::new(&node),
            mouse::Cursor::Available(point),
            &bounds,
        );
        assert!(state.borrow().presented.is_none());
        for event in [
            mouse::Event::ButtonReleased(mouse::Button::Left),
            mouse::Event::ButtonPressed(mouse::Button::Left),
        ] {
            <NativeBuffer<'_, Option<MouseInput>> as Widget<Option<MouseInput>, iced::Theme, ()>>::update(
                &mut widget,
                &mut tree,
                &Event::Mouse(event),
                Layout::new(&node),
                mouse::Cursor::Available(point),
                &(),
                &mut iced::advanced::clipboard::Null,
                &mut Shell::new(&mut output),
                &bounds,
            );
        }
        assert!(
            output[1].is_none(),
            "the consumed press must retain its release"
        );
        assert!(
            matches!(output[2], Some(MouseInput::Press { .. })),
            "the actually drawn fallback must restore ordinary Vim mouse input"
        );
    }

    #[test]
    fn prepared_views_keep_window_identity_and_stable_paint_order() {
        let mut frame = split_frame();
        // A wrong-window run must not become visible merely because its pixel
        // coordinates happen to lie inside another split.
        let mut wrong_window = frame.runs[0].clone();
        wrong_window.window = 20;
        wrong_window.text = "wrong".into();
        frame.runs.push(wrong_window);
        let metrics = Metrics::new(14.0, 1.0, 4.0, false);
        let bounds = Rectangle::with_size(Size::new(1000.0, 700.0));
        let views = paint_views(&frame, render_metrics(bounds, metrics), bounds, |_| 0.0);
        assert_eq!(views.len(), 2);
        assert_eq!(
            views[0]
                .runs
                .iter()
                .map(|r| r.run.text.as_str())
                .collect::<Vec<_>>(),
            vec!["界", "next"]
        );
        assert_eq!(
            views[1]
                .runs
                .iter()
                .map(|r| r.run.text.as_str())
                .collect::<Vec<_>>(),
            vec!["peer"]
        );
    }

    #[test]
    fn native_text_and_semantic_anchors_share_fractional_pixel_geometry() {
        let frame = split_frame();
        for font_size in [11.0, 14.5, 22.0] {
            let metrics = Metrics::new(font_size, 1.15, 3.25, false);
            let bounds = Rectangle {
                x: 7.25,
                y: 9.5,
                width: 1200.0,
                height: 800.0,
            };
            let geometry = render_metrics(bounds, metrics);
            let anchor = frame.anchor_at_cell(1, 3).unwrap();
            assert_eq!(anchor.window, 10);
            assert_eq!(anchor.buffer, 3);
            let anchor_rect = rectangle(geometry.rect(&anchor.rect).unwrap());
            let run_rect = rectangle(geometry.cell_rect(1, 2, 2).unwrap());
            assert_eq!(anchor_rect, run_rect);
            assert_eq!(
                geometry.cell_at(run_rect.x + metrics.cell_w * 1.5, run_rect.center_y()),
                Some((1, 3))
            );
            let views = paint_views(&frame, geometry, bounds, |_| 0.0);
            assert!(views[0].clip.contains(anchor_rect.center()));
            assert!(!views[1].clip.contains(anchor_rect.center()));
        }
    }
    #[test]
    fn rgb_highlights_preserve_gui_colors() {
        let normal = Vim9RenderStyle {
            foreground: Some(0x142536),
            background: Some(0x102030),
            ..Default::default()
        };
        let mut style = Vim9RenderStyle {
            foreground: Some(0x19c3a7),
            special: Some(0xe83967),
            ..Default::default()
        };
        assert_eq!(
            colors(&style, &normal),
            (rgb(0x19c3a7), rgb(0x102030), rgb(0xe83967))
        );
        style.reverse = true;
        assert_eq!(
            colors(&style, &normal),
            (rgb(0x102030), rgb(0x19c3a7), rgb(0xe83967))
        );
    }
    #[test]
    fn terminal_bands_never_cover_native_buffer() {
        let bounds = Rectangle {
            x: 0.0,
            y: 0.0,
            width: 800.0,
            height: 600.0,
        };
        let view = Rectangle {
            x: 10.0,
            y: 10.0,
            width: 780.0,
            height: 560.0,
        };
        let bands = outside(bounds, view);
        for band in bands {
            if let Some(intersection) = band.intersection(&view) {
                assert_eq!(intersection.area(), 0.0);
            }
        }
        let total: f32 = bands.iter().map(Rectangle::area).sum();
        assert_eq!(total + view.area(), bounds.area());
    }

    #[test]
    fn mixed_split_bands_preserve_status_and_separator_at_fractional_scales() {
        for scale in [1.0, 1.25, 2.0] {
            let rect = |x, y, width, height| Rectangle {
                x: x * scale,
                y: y * scale,
                width: width * scale,
                height: height * scale,
            };
            let bounds = rect(3.5, 7.25, 800.0, 600.0);
            let views = [
                rect(13.5, 17.25, 380.0, 260.0),
                rect(403.5, 17.25, 390.0, 540.0),
                rect(13.5, 297.25, 380.0, 260.0),
            ];
            let bands = compatibility_bands(bounds, &views);
            for (i, band) in bands.iter().enumerate() {
                for view in &views {
                    assert!(band.intersection(view).is_none_or(|r| r.area() < 0.01));
                }
                for other in bands.iter().skip(i + 1) {
                    assert!(band.intersection(other).is_none_or(|r| r.area() < 0.01));
                }
            }
            let area: f32 = bands.iter().chain(&views).map(Rectangle::area).sum();
            assert!((area - bounds.area()).abs() < 0.1);
            for (x, y) in [(398.5, 100.0), (100.0, 287.25), (100.0, 580.0)] {
                let point = Point::new(x * scale, y * scale);
                assert_eq!(
                    bands.iter().filter(|band| band.contains(point)).count(),
                    1,
                    "separator/status/command point must be drawn exactly once"
                );
            }
        }
    }

    #[test]
    fn every_split_must_fit_current_terminal_geometry() {
        let mut frame: Vim9RenderFrame = serde_json::from_value(serde_json::json!({
            "supported": true, "rows": 30, "columns": 120,
            "view": {"row": 0, "column": 0, "rows": 28, "columns": 59},
            "windows": [
                {"window": 1, "view": {"row": 0, "column": 0, "rows": 28, "columns": 59}},
                {"window": 2, "view": {"row": 0, "column": 60, "rows": 28, "columns": 60}}
            ]
        }))
        .unwrap();
        assert!(usable(&frame, 30, 120));
        assert!(
            !usable(&frame, 30, 119),
            "resize must expose current Vim screen"
        );
        frame.windows[1].view.columns = 61;
        assert!(
            !usable(&frame, 30, 120),
            "invalid inactive window cannot be hidden"
        );
    }
}
