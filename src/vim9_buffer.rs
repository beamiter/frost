//! Native Vim buffer text, independent of terminal cells and ANSI palettes.
//!
//! The established PTY input adapter retains focus and compatibility gestures.
//! Its painter is clipped to Vim's command line / compatible margins. The
//! buffer rectangles are drawn exclusively from authenticated GUI text runs.

use iced::advanced::layout::{self, Layout};
use iced::advanced::renderer::{self, Quad};
use iced::advanced::text::{self, Text};
use iced::advanced::widget::{tree, Tree, Widget};
use iced::advanced::{input_method, Clipboard, Shell};
use iced::{
    keyboard, mouse, Background, Color, Element, Event, Font, Length, Pixels, Point, Rectangle,
    Size,
};
use jterm_core::vim9::{
    Vim9BufferPosition, Vim9Command, Vim9CursorBlink, Vim9PixelRect, Vim9PixelScroll,
    Vim9PointerPhase, Vim9ProjectedRun, Vim9RenderFrame, Vim9RenderMetrics, Vim9RenderRect,
    Vim9RenderRun, Vim9RenderStyle, Vim9Viewport,
};
use std::cell::RefCell;
use std::collections::{BTreeMap, HashSet};
use std::sync::Arc;

use crate::terminal_view::{Metrics, MouseButton, MouseInput, TermWidget};

#[derive(Debug, Clone)]
pub(crate) struct NativeWheel {
    pub window: i64,
    /// Positive means down. Precision devices retain fractional lines.
    pub lines: f64,
    discrete: bool,
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

#[derive(Clone, Debug)]
struct PresentedFrame {
    frame: Arc<Vim9RenderFrame>,
    metrics: Vim9RenderMetrics,
    local_metrics: Vim9RenderMetrics,
    offsets: BTreeMap<i64, f32>,
    clip: Rectangle,
}

/// Frozen input ownership for a widget message deferred until pane focus.
#[derive(Clone, Debug, Default)]
pub(crate) struct BodyMouseSnapshot {
    position: Option<Point>,
    presented: Option<PresentedFrame>,
    modifiers: keyboard::Modifiers,
}

impl BodyMouseSnapshot {
    pub(crate) fn modifiers(&self) -> keyboard::Modifiers {
        self.modifiers
    }

    pub(crate) fn source_current(
        &self,
        frame: Option<&Vim9RenderFrame>,
        metrics: Vim9RenderMetrics,
    ) -> bool {
        let Some(presented) = &self.presented else {
            return true;
        };
        let Some(point) = self.position else {
            return true;
        };
        let Some(old) = presented.frame.windows.iter().find(|window| {
            presented
                .metrics
                .rect(&window.view)
                .is_some_and(|rect| rect.contains(point.x, point.y))
        }) else {
            return true;
        };
        frame.is_some_and(|frame| {
            frame.supported
                && presented.local_metrics == metrics
                && frame.windows.iter().any(|new| {
                    (
                        new.window,
                        new.buffer,
                        new.changedtick,
                        &new.view,
                        new.leftcol,
                        new.textoff,
                    ) == (
                        old.window,
                        old.buffer,
                        old.changedtick,
                        &old.view,
                        old.leftcol,
                        old.textoff,
                    ) && (presented.offsets.get(&old.window).copied().unwrap_or(0.0) != 0.0
                        || new.topline == old.topline)
                })
        })
    }
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
    cursor_blink: Vim9CursorBlink,
    pixels: BTreeMap<i64, Vim9PixelScroll>,
    pointer: Option<PointerGesture>,
    // A cancelled native press still owns its drag/release tail. Never leak it
    // as a compatibility drag without a corresponding compatibility press.
    native_pressed: bool,
    consumed_buttons: [bool; 3],
    next_gesture: u64,
    position: Option<Point>,
    presented: Option<PresentedFrame>,
    event_modifiers: keyboard::Modifiers,
    model_modifiers: Option<keyboard::Modifiers>,
}

impl ScrollState {
    pub(crate) fn clear(&mut self) {
        self.pixels.clear();
        self.pointer = None;
    }

    pub(crate) fn compatibility_mouse(&mut self, input: MouseInput) -> Option<MouseInput> {
        if matches!(input, MouseInput::Press { .. }) {
            self.cursor_blink.reset();
        }
        self.compatibility_mouse_with(input, true)
    }

    fn compatibility_mouse_with(
        &mut self,
        input: MouseInput,
        protect_fractional: bool,
    ) -> Option<MouseInput> {
        self.clear();
        if let MouseInput::Press { button, .. } = input {
            if protect_fractional && self.fractional_surface_at_pointer() {
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
        if self.pointer.is_some()
            || frame.has_overlays()
            || !matches!(frame.mode.as_str(), "n" | "i")
        {
            return;
        }
        let Some(window) = frame.windows.iter().find(|w| {
            w.native_scroll
                && w.window == wheel.window
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
            if wheel.discrete {
                scroll.push_wheel(wheel.lines);
            } else {
                scroll.push(wheel.lines);
            }
        }
    }

    pub(crate) fn animating(&self) -> bool {
        self.pixels.values().any(Vim9PixelScroll::is_animating)
    }

    /// Sample pending viewport acknowledgements on Iced's display frame clock
    /// even when the current source margins cannot expose any more pixels.
    pub(crate) fn needs_frame(&self) -> bool {
        self.animating()
            || self
                .pixels
                .values()
                .any(|scroll| scroll.request_id().is_some())
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
        if frame.has_overlays() {
            self.clear();
            return;
        }
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
        if frame.has_overlays() {
            return 0.0;
        }
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

    pub(crate) fn seed_modifiers(&mut self, modifiers: keyboard::Modifiers) {
        if self.model_modifiers != Some(modifiers) {
            self.model_modifiers = Some(modifiers);
            self.event_modifiers = modifiers;
        }
    }
    fn observe_modifiers(&mut self, modifiers: keyboard::Modifiers) {
        self.event_modifiers = modifiers;
    }
    pub(crate) fn event_modifiers(&self) -> keyboard::Modifiers {
        self.event_modifiers
    }

    pub(crate) fn capture_body_mouse(&self) -> BodyMouseSnapshot {
        BodyMouseSnapshot {
            position: self.position,
            presented: self.presented.clone(),
            modifiers: self.event_modifiers,
        }
    }

    pub(crate) fn route_body_mouse(
        &mut self,
        frame: Option<&Vim9RenderFrame>,
        metrics: Vim9RenderMetrics,
        mut input: MouseInput,
        snapshot: BodyMouseSnapshot,
    ) -> NativeInput {
        if !snapshot.source_current(frame, metrics) {
            self.clear();
            match input {
                MouseInput::Press {
                    button: MouseButton::Left,
                    ..
                } => self.native_pressed = true,
                MouseInput::Press { button, .. } => self.consumed_buttons[button.slot()] = true,
                MouseInput::Release { button, .. } => {
                    self.consumed_buttons[button.slot()] = false;
                    if button == MouseButton::Left {
                        self.native_pressed = false;
                    }
                }
                _ => {}
            }
            return NativeInput::Handled;
        }
        let modifiers = snapshot.modifiers;
        crate::sync_embedded_body_modifiers(&mut input, modifiers);
        let latest_position = std::mem::replace(&mut self.position, snapshot.position);
        let latest_presentation = std::mem::replace(&mut self.presented, snapshot.presented);
        let routed = if let Some(frame) = frame {
            self.route_mouse(frame, metrics, modifiers, input)
        } else {
            self.compatibility_mouse(input)
                .map_or(NativeInput::Handled, NativeInput::Compatibility)
        };
        self.position = latest_position;
        self.presented = latest_presentation;
        routed
    }

    pub(crate) fn route_mouse(
        &mut self,
        frame: &Vim9RenderFrame,
        metrics: Vim9RenderMetrics,
        modifiers: keyboard::Modifiers,
        input: MouseInput,
    ) -> NativeInput {
        if matches!(input, MouseInput::Press { .. }) {
            self.cursor_blink.reset();
        }
        // Vim owns popup/completion screen-cell mouse semantics, including
        // closing and selection. Never translate an overlay to source text.
        if frame.has_overlays() {
            return self
                .compatibility_mouse_with(input, false)
                .map_or(NativeInput::Handled, NativeInput::Compatibility);
        }
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
                        for scroll in self.pixels.values_mut() {
                            scroll.stop_animation();
                        }
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
    if frame.has_overlays() || !matches!(frame.mode.as_str(), "n" | "i") {
        return None;
    }
    let (row, column) = geometry.cell_at(position.x, position.y)?;
    let window = frame
        .windows
        .iter()
        .find(|window| window.native_scroll && window.view.contains(row, column))?;
    let lines = match delta {
        mouse::ScrollDelta::Lines { y, .. } => -f64::from(y) * 3.0,
        mouse::ScrollDelta::Pixels { y, .. } => -f64::from(y) / f64::from(geometry.line_height),
    };
    (lines.is_finite() && lines != 0.0).then_some(NativeWheel {
        window: window.window,
        lines,
        // X11 XI2 reports smooth touchpad axes as fractional Lines too.
        // Preserve those samples directly instead of adding notch easing.
        discrete: matches!(delta, mouse::ScrollDelta::Lines { y, .. } if y.fract() == 0.0),
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

/// The fallback is used only for ASCII with one byte per authoritative Vim
/// cell. The ink clip includes italic overhang; source/Unicode shaping stays
/// intact on the normal whole-run path.
fn visible_ascii_columns(
    rect: Rectangle,
    clip: Rectangle,
    cell_width: f32,
    cells: usize,
) -> std::ops::Range<usize> {
    let start = ((clip.x - rect.x) / cell_width).floor().max(0.0) as usize;
    let end = ((clip.x + clip.width - rect.x) / cell_width)
        .ceil()
        .max(0.0) as usize;
    start.min(cells)..end.min(cells)
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
    (
        fg,
        bg,
        style.special.or(normal.special).map(rgb).unwrap_or(fg),
    )
}

fn quad(bounds: Rectangle) -> Quad {
    Quad {
        bounds,
        ..Quad::default()
    }
}

/// Keep decoration outside Vim's content geometry and inside the editor.
/// Completion adds an outline at its original rectangle, even when clipped;
/// ordinary popup borders come from Vim's runs; tagged documentation cards
/// use a native shell inside the same authoritative rectangle.
fn paint_overlay_surface<Renderer: renderer::Renderer>(
    renderer: &mut Renderer,
    view: Rectangle,
    editor_clip: Rectangle,
    foreground: Color,
    outline: bool,
    card: bool,
    paint_content: impl FnOnce(&mut Renderer, Rectangle),
) {
    let Some(clip) = view.intersection(&editor_clip) else {
        return;
    };
    renderer.with_layer(editor_clip, |renderer| {
        for (spread, alpha) in [(4.0, 4.0), (3.0, 5.0), (2.0, 7.0), (1.0, 10.0)] {
            renderer.fill_quad(
                Quad {
                    border: iced::Border {
                        radius: (if card { 6.0 + spread } else { 0.0 }).into(),
                        ..Default::default()
                    },
                    ..quad(Rectangle {
                        x: view.x - spread,
                        y: view.y - spread,
                        width: view.width + spread * 2.0,
                        height: view.height + spread * 2.0,
                    })
                },
                Background::Color(Color {
                    a: alpha / 255.0,
                    ..Color::BLACK
                }),
            );
        }
        renderer.with_layer(clip, |renderer| {
            paint_content(renderer, clip);
            if outline || card {
                renderer.fill_quad(
                    Quad {
                        bounds: view,
                        border: iced::Border {
                            color: Color {
                                a: foreground.a * 0.20,
                                ..foreground
                            },
                            width: 1.0,
                            radius: (if card { 6.0 } else { 0.0 }).into(),
                        },
                        ..Quad::default()
                    },
                    Background::Color(Color::TRANSPARENT),
                );
            }
        });
    });
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

/// The protocol places each window's resolved Normal below all decorations.
/// Legacy frames without explicit full-width backgrounds inherit global Normal.
fn window_normal<'a>(
    frame: &'a Vim9RenderFrame,
    window: &jterm_core::vim9::Vim9RenderWindow,
) -> &'a Vim9RenderStyle {
    frame
        .runs
        .iter()
        .find(|run| {
            run.window == window.window
                && run.line == 0
                && run.text.is_empty()
                && run.byte_start == 0
                && run.byte_end == 0
                && run.column == window.view.column
                && run.cells == window.view.columns
                && window.view.contains(run.row, run.column)
        })
        .map_or(&frame.default_style, |run| &run.style)
}

struct PaintView<'a> {
    normal: &'a Vim9RenderStyle,
    clip: Rectangle,
    active: bool,
    runs: Vec<Vim9ProjectedRun<'a>>,
    cursor: Option<Rectangle>,
    raw_cursor: Option<Rectangle>,
}

impl PaintView<'_> {
    fn cursor_style<'a>(
        &'a self,
        cursor: Rectangle,
        normal: &'a Vim9RenderStyle,
    ) -> &'a Vim9RenderStyle {
        // Row/column backgrounds come first; the final glyph or selection
        // style determines the appearance of the text under the cursor.
        self.runs
            .iter()
            .rev()
            .find(|run| rectangle(run.rect).contains(cursor.center()))
            .map_or(normal, |run| &run.run.style)
    }
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
            let offset = offset(window.window);
            let viewport = Vim9Viewport::new(frame, window.window, metrics, offset)?;
            let visible = rectangle(viewport.clip())
                .intersection(&clip)
                .filter(|r| r.area() > 0.0)?;
            Some(PaintView {
                normal: window_normal(frame, window),
                clip: visible,
                active: window.window == frame.window,
                runs: viewport.runs().collect(),
                // Clip the full cell only after computing the actual cursor
                // shape. A partial row must not shrink or relocate its caret.
                raw_cursor: (window.window == frame.window)
                    .then(|| metrics.cursor_rect(&window.cursor))
                    .flatten()
                    .map(|rect| {
                        rectangle(Vim9PixelRect {
                            y: rect.y + offset,
                            ..rect
                        })
                    }),
                cursor: (window.window == frame.window)
                    .then(|| viewport.cursor_rect())
                    .flatten()
                    .map(rectangle)
                    .and_then(|r| r.intersection(&visible)),
            })
        })
        .collect()
}

/// Paint strictly inside the existing buffer clip: separators and Vim's
/// status rows continue to belong to the compatibility surface.
fn paint_split_focus<Renderer: renderer::Renderer>(
    renderer: &mut Renderer,
    frame: &Vim9RenderFrame,
    prepared: &PaintView<'_>,
    focused: bool,
) {
    if frame.windows.len() <= 1 || !prepared.active {
        return;
    }
    let view = prepared.clip;
    let width = 1.0_f32.min(view.width / 2.0).min(view.height / 2.0);
    let normal = prepared.normal;
    let mut accent = frame.cursor.appearance.as_ref().map_or_else(
        || normal.foreground.unwrap_or(0xd7dce2),
        |appearance| appearance.colors(normal, normal).1,
    );
    if rgb(accent) == colors(normal, normal).1 {
        accent = normal.foreground.unwrap_or(0xd7dce2);
    }
    let color = Color {
        a: if focused { 0.42 } else { 0.16 },
        ..rgb(accent)
    };
    let edges = [
        Rectangle {
            height: width,
            ..view
        },
        Rectangle {
            y: view.y + view.height - width,
            height: width,
            ..view
        },
        Rectangle {
            y: view.y + width,
            width,
            height: view.height - 2.0 * width,
            ..view
        },
        Rectangle {
            x: view.x + view.width - width,
            y: view.y + width,
            width,
            height: view.height - 2.0 * width,
        },
    ];
    renderer.with_layer(view, |renderer| {
        for edge in edges.into_iter().filter(|edge| edge.area() > 0.0) {
            renderer.fill_quad(quad(edge), Background::Color(color));
        }
    });
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

/// Elide fills already supplied by the window backdrop. Once a row contains
/// another color, preserve all later fills: a Normal run may erase a guide or
/// selection beneath it. Empty sets allocate nothing for ordinary source text.
struct BackgroundRows<C> {
    normal: C,
    changed: HashSet<usize>,
}

impl<C: Copy + PartialEq> BackgroundRows<C> {
    fn new(normal: C) -> Self {
        Self {
            normal,
            changed: HashSet::new(),
        }
    }

    fn needs_fill(&mut self, row: usize, background: C) -> bool {
        if background != self.normal {
            self.changed.insert(row);
            true
        } else {
            self.changed.contains(&row)
        }
    }
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
    smooth_scroll: bool,
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
            smooth_scroll: true,
        }
    }

    pub(crate) fn scroll(
        mut self,
        state: &'a RefCell<ScrollState>,
        modifiers: keyboard::Modifiers,
        smooth_scroll: bool,
        on_scroll: impl Fn(NativeWheel) -> Message + 'a,
    ) -> Self {
        self.scroll = Some(state);
        state.borrow_mut().seed_modifiers(modifiers);
        self.modifiers = modifiers;
        self.smooth_scroll = smooth_scroll;
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
            match event {
                Event::Keyboard(keyboard::Event::ModifiersChanged(modifiers))
                | Event::Keyboard(keyboard::Event::KeyPressed { modifiers, .. })
                | Event::Keyboard(keyboard::Event::KeyReleased { modifiers, .. }) => {
                    state.borrow_mut().observe_modifiers(*modifiers)
                }
                Event::Window(iced::window::Event::Unfocused) => state
                    .borrow_mut()
                    .observe_modifiers(keyboard::Modifiers::empty()),
                _ => {}
            }
            self.modifiers = state.borrow().event_modifiers();
            self.input.set_event_modifiers(
                self.modifiers.shift(),
                self.modifiers.alt(),
                self.modifiers.control(),
            );
            if state.borrow().animating() {
                shell.request_redraw();
            }
            if let Some(frame) = self.frame.filter(|f| f.cursor.appearance.is_some()) {
                let now = std::time::Instant::now();
                let (_, next) = state
                    .borrow_mut()
                    .cursor_blink
                    .sample(frame, self.focused, now);
                if let Some(delay) = next {
                    shell.request_redraw_at(now + delay);
                }
            }
            state.borrow_mut().position = cursor.position();
            let interrupts_scroll = match event {
                Event::Keyboard(keyboard::Event::KeyPressed { .. })
                | Event::InputMethod(input_method::Event::Commit(_))
                | Event::Window(iced::window::Event::Unfocused) => true,
                Event::InputMethod(input_method::Event::Preedit(value, _)) => !value.is_empty(),
                _ => false,
            };
            if interrupts_scroll {
                state.borrow_mut().clear();
                state.borrow_mut().cursor_blink.reset();
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
            && self.frame.is_some_and(|frame| {
                matches!(frame.mode.as_str(), "n" | "i") && !frame.has_overlays()
            })
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
                    if let Some(mut wheel) = wheel {
                        // Precision input already carries the device's pixel
                        // trajectory. The option only controls the extra
                        // transition synthesized for discrete wheel notches.
                        wheel.discrete &= self.smooth_scroll;
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
        let cursor_on = if frame.cursor.appearance.is_some() {
            self.scroll.is_none_or(|state| {
                state
                    .borrow_mut()
                    .cursor_blink
                    .sample(frame, self.focused, std::time::Instant::now())
                    .0
            })
        } else {
            self.blink_on
        };
        for prepared in views {
            self.paint_native_view(renderer, frame, &prepared, cursor_on);
            paint_split_focus(renderer, frame, &prepared, self.focused);
        }
        // Popup geometry belongs to the full editor grid. Draw it after all
        // splits and cursors so it can cover a neighboring split or separator.
        self.paint_overlays(renderer, frame, geometry, clip);
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
    fn paint_ink<Renderer>(
        &self,
        renderer: &mut Renderer,
        normal: &Vim9RenderStyle,
        projected: &Vim9ProjectedRun<'_>,
        view: Rectangle,
        override_color: Option<Color>,
    ) where
        Renderer: text::Renderer<Font = Font>,
    {
        let m = self.metrics;
        let run = projected.run;
        let area = rectangle(projected.rect);
        let text_area = Rectangle {
            x: area.x - m.font_size * 0.25,
            width: area.width + m.font_size * 0.5,
            ..area
        };
        let Some(run_clip) = text_area
            .intersection(&view)
            .filter(|clip| clip.area() > 0.0)
        else {
            return;
        };
        let (fg, _, special) = colors(&run.style, normal);
        let fg = override_color.unwrap_or(fg);
        let special = override_color.unwrap_or(special);
        // Backgrounds are already painted separately. Empty and ASCII-space
        // runs have no glyph ink, but their decorations must still be drawn.
        if run.text.bytes().any(|byte| byte != b' ') {
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
            // Closed-fold filler repeats one glyph that a fallback font may
            // advance differently; it is placed per cell the same way.
            if let Some(glyph) = run.repeated_glyph().filter(|glyph| !glyph.is_ascii()) {
                let columns = visible_ascii_columns(
                    area,
                    Rectangle {
                        x: run_clip.x - m.font_size * 0.25,
                        width: run_clip.width + m.font_size * 0.5,
                        ..run_clip
                    },
                    m.cell_w,
                    run.cells,
                );
                for column in columns {
                    draw_text(glyph.to_string(), area.x + column as f32 * m.cell_w);
                }
            } else if run.text.is_ascii()
                && run.text.len() == run.cells
                && (!m.mono_advance_exact
                    || (font != self.font
                        && !crate::terminal_view::native_editor_font_matches_grid(font, m)))
            {
                // Clipped splits and cursor inversion can expose only one
                // cell of a long run. Do not submit every hidden ASCII glyph.
                let columns = visible_ascii_columns(
                    area,
                    Rectangle {
                        x: run_clip.x - m.font_size * 0.25,
                        width: run_clip.width + m.font_size * 0.5,
                        ..run_clip
                    },
                    m.cell_w,
                    run.cells,
                );
                for (offset, ch) in run.text[columns.clone()].chars().enumerate() {
                    let column = columns.start + offset;
                    draw_text(ch.to_string(), area.x + column as f32 * m.cell_w);
                }
            } else {
                draw_text(run.text.clone(), area.x);
            }
        }
        let thickness = (m.font_size / 14.0).clamp(1.0, 2.0);
        let underline_y = area.y + area.height - thickness * 1.5;
        if run.style.undercurl {
            let step = (m.font_size / 5.0).max(2.0);
            // Begin at the first visible segment, retaining its original
            // parity. GPU clipping alone still constructs every hidden quad.
            let start = ((run_clip.x - area.x) / step).floor().max(0.0) as usize;
            let mut x = area.x + start as f32 * step;
            let mut up = start.is_multiple_of(2);
            let end = (area.x + area.width).min(run_clip.x + run_clip.width);
            while x < end {
                renderer.fill_quad(
                    quad(Rectangle {
                        x,
                        y: underline_y - if up { thickness } else { 0.0 },
                        width: step.min(end - x),
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

    fn paint_overlays<Renderer>(
        &self,
        renderer: &mut Renderer,
        frame: &Vim9RenderFrame,
        metrics: Vim9RenderMetrics,
        editor_clip: Rectangle,
    ) where
        Renderer: text::Renderer<Font = Font>,
    {
        // Vim reserves zindex 100 for the completion menu. The producer
        // supplies stable popup order; all surfaces stay outside source paint.
        for popup in frame.popups.iter().filter(|popup| popup.zindex < 100) {
            self.paint_overlay(
                renderer,
                frame,
                metrics,
                editor_clip,
                &popup.view,
                &popup.default_style,
                &popup.runs,
                false,
                popup.is_documentation_card(),
            );
        }
        if let Some(menu) = &frame.completion {
            self.paint_overlay(
                renderer,
                frame,
                metrics,
                editor_clip,
                &menu.view,
                &menu.default_style,
                &menu.runs,
                true,
                false,
            );
        }
        for popup in frame.popups.iter().filter(|popup| popup.zindex >= 100) {
            self.paint_overlay(
                renderer,
                frame,
                metrics,
                editor_clip,
                &popup.view,
                &popup.default_style,
                &popup.runs,
                false,
                popup.is_documentation_card(),
            );
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn paint_overlay<Renderer>(
        &self,
        renderer: &mut Renderer,
        frame: &Vim9RenderFrame,
        metrics: Vim9RenderMetrics,
        editor_clip: Rectangle,
        bounds: &Vim9RenderRect,
        normal: &Vim9RenderStyle,
        runs: &[Vim9RenderRun],
        outline: bool,
        card: bool,
    ) where
        Renderer: text::Renderer<Font = Font>,
    {
        let Some(view) = metrics.rect(bounds).map(rectangle) else {
            return;
        };
        let (foreground, background, _) = colors(normal, &frame.default_style);
        paint_overlay_surface(
            renderer,
            view,
            editor_clip,
            foreground,
            outline,
            card,
            |renderer, clip| {
                renderer.fill_quad(
                    Quad {
                        border: iced::Border {
                            radius: (if card { 6.0 } else { 0.0 }).into(),
                            ..Default::default()
                        },
                        ..quad(view)
                    },
                    Background::Color(background),
                );
                let runs: Vec<_> = runs
                    .iter()
                    .filter_map(|run| {
                        Some(Vim9ProjectedRun {
                            run,
                            rect: metrics.cell_rect(run.row, run.column, run.cells)?,
                            clip: Vim9PixelRect {
                                x: clip.x,
                                y: clip.y,
                                width: clip.width,
                                height: clip.height,
                            },
                        })
                    })
                    .collect();
                // Tagged providers reserve a padding cell on every edge.
                // Default row fills must leave the rounded shell intact; actual
                // highlighted backgrounds and all glyph coordinates stay Vim-owned.
                for projected in &runs {
                    let (_, run_background, _) = colors(&projected.run.style, normal);
                    if card && run_background == background {
                        continue;
                    }
                    renderer.fill_quad(
                        quad(rectangle(projected.rect)),
                        Background::Color(run_background),
                    );
                }
                for projected in &runs {
                    self.paint_ink(renderer, normal, projected, clip, None);
                }
            },
        );
    }

    fn paint_native_view<Renderer>(
        &self,
        renderer: &mut Renderer,
        frame: &Vim9RenderFrame,
        prepared: &PaintView<'_>,
        cursor_on: bool,
    ) where
        Renderer: text::Renderer<Font = Font>,
    {
        let m = self.metrics;
        let view = prepared.clip;
        let normal = prepared.normal;
        let (normal_fg, normal_bg, _) = colors(normal, normal);
        renderer.with_layer(view, |renderer| {
            renderer.fill_quad(quad(view), Background::Color(normal_bg));
            // Keep italic overhang intact across adjacent highlight runs.
            let mut backgrounds = BackgroundRows::new(normal_bg);
            for projected in &prepared.runs {
                let run = projected.run;
                let area = rectangle(projected.rect);
                let (_, bg, _) = colors(&run.style, normal);
                // Pixel Y also identifies sampled rows outside the viewport.
                if backgrounds.needs_fill(projected.rect.y.to_bits() as usize, bg) {
                    renderer.fill_quad(quad(area), Background::Color(bg));
                }
            }
            for projected in &prepared.runs {
                self.paint_ink(renderer, normal, projected, view, None);
            }
            if let Some(cursor) = prepared
                .raw_cursor
                .filter(|_| prepared.cursor.is_some() && (!self.focused || cursor_on))
            {
                if let Some(appearance) = &frame.cursor.appearance {
                    let under = prepared.cursor_style(cursor, normal);
                    let (fg, bg) = appearance.colors(under, normal);
                    if !self.focused {
                        renderer.fill_quad(
                            Quad {
                                bounds: cursor,
                                border: iced::Border {
                                    color: rgb(bg),
                                    width: 1.0,
                                    ..Default::default()
                                },
                                ..Default::default()
                            },
                            Background::Color(Color::TRANSPARENT),
                        );
                    } else {
                        let caret = rectangle(appearance.paint_rect(
                            Vim9PixelRect {
                                x: cursor.x,
                                y: cursor.y,
                                width: cursor.width,
                                height: cursor.height,
                            },
                            m.cell_w,
                            1.0,
                        ));
                        renderer.fill_quad(quad(caret), Background::Color(rgb(bg)));
                        renderer.with_layer(caret, |renderer| {
                            for projected in prepared
                                .runs
                                .iter()
                                .filter(|p| rectangle(p.rect).intersects(&caret))
                            {
                                self.paint_ink(renderer, normal, projected, caret, Some(rgb(fg)));
                            }
                        });
                    }
                } else if self.focused && frame.mode.starts_with('i') {
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

    #[test]
    fn background_elision_preserves_overlapping_layers_and_row_isolation() {
        let normal = 0x102030;
        let mut backgrounds = BackgroundRows::new(normal);
        let mut full = [[normal; 12]; 4];
        let mut reduced = full;
        let mut state = 41_u32;
        // Normal text, overlapping guides/selection, and a subsequent Normal
        // layer exercise restoration as well as the no-fill common case.
        for i in 0..300 {
            state = state.wrapping_mul(1664525).wrapping_add(1013904223);
            let row = (state as usize >> 8) % 4;
            let start = (state as usize >> 16) % 10;
            let end = (start + 1 + i % 4).min(12);
            let color = if i % 7 < 5 { normal } else { state & 0xffffff };
            full[row][start..end].fill(color);
            if backgrounds.needs_fill(row, color) {
                reduced[row][start..end].fill(color);
            }
            assert_eq!(full, reduced, "paint order changed at run {i}");
        }
        let mut backgrounds = BackgroundRows::new(normal);
        assert!(!backgrounds.needs_fill(0, normal));
        assert!(backgrounds.needs_fill(0, 0xabcdef));
        assert!(backgrounds.needs_fill(0, normal));
        assert!(!backgrounds.needs_fill(1, normal));
    }

    #[test]
    fn background_elision_removes_plain_syntax_fills_without_allocating() {
        let mut backgrounds = BackgroundRows::new(0x102030);
        let fills = (0..10000)
            .filter(|index| backgrounds.needs_fill(index / 100, 0x102030))
            .count();
        assert_eq!(fills, 0);
        assert_eq!(backgrounds.changed.capacity(), 0);
    }

    #[derive(Default)]
    struct CompletionRecorder {
        clips: Vec<Rectangle>,
        quads: Vec<(Quad, Background, Rectangle)>,
        text: Vec<(String, Point, Color, Rectangle)>,
    }

    impl text::Renderer for CompletionRecorder {
        type Font = Font;
        type Paragraph = iced::advanced::graphics::text::Paragraph;
        type Editor = iced::advanced::graphics::text::Editor;
        const ICON_FONT: Font = Font::MONOSPACE;
        const CHECKMARK_ICON: char = 'x';
        const ARROW_DOWN_ICON: char = 'v';
        const SCROLL_UP_ICON: char = '^';
        const SCROLL_DOWN_ICON: char = 'v';
        const SCROLL_LEFT_ICON: char = '<';
        const SCROLL_RIGHT_ICON: char = '>';
        const ICED_LOGO: char = 'i';

        fn default_font(&self) -> Font {
            Font::MONOSPACE
        }
        fn default_size(&self) -> Pixels {
            Pixels(14.0)
        }
        fn fill_paragraph(&mut self, _: &Self::Paragraph, _: Point, _: Color, _: Rectangle) {
            panic!("native runs must use fill_text");
        }
        fn fill_editor(&mut self, _: &Self::Editor, _: Point, _: Color, _: Rectangle) {
            panic!("native runs must use fill_text");
        }
        fn fill_text(
            &mut self,
            text: Text<String, Font>,
            position: Point,
            color: Color,
            clip: Rectangle,
        ) {
            self.text.push((text.content, position, color, clip));
        }
    }

    impl renderer::Renderer for CompletionRecorder {
        fn start_layer(&mut self, bounds: Rectangle) {
            self.clips.push(bounds);
        }
        fn end_layer(&mut self) {
            self.clips.pop().unwrap();
        }
        fn start_transformation(&mut self, _: iced::Transformation) {}
        fn end_transformation(&mut self) {}
        fn fill_quad(&mut self, quad: Quad, background: impl Into<Background>) {
            self.quads
                .push((quad, background.into(), *self.clips.last().unwrap()));
        }
        fn reset(&mut self, _: Rectangle) {}
        fn allocate_image(
            &mut self,
            _: &iced::advanced::image::Handle,
            callback: impl FnOnce(Result<iced::advanced::image::Allocation, iced::advanced::image::Error>)
                + Send
                + 'static,
        ) {
            callback(Err(iced::advanced::image::Error::Unsupported));
        }
    }

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
            window.native_scroll = true;
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
                fold_end: 0,
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
            appearance: None,
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
        assert!(notch.discrete);
        let smooth_x11 = wheel_at(
            &frame,
            geometry,
            position,
            mouse::ScrollDelta::Lines { x: 0.0, y: -0.125 },
        )
        .unwrap();
        assert_eq!(smooth_x11.lines, 0.375);
        assert!(!smooth_x11.discrete);
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
    fn precision_scroll_is_independent_of_pointer_ownership_in_normal_and_insert_modes() {
        let metrics = geometry();
        for mode in ["n", "i"] {
            let mut frame = direct_frame();
            frame.mode = mode.into();
            frame.windows[1].native_pointer = false;
            let point = Point::new(340.0, 45.0);
            let event = wheel_at(
                &frame,
                metrics,
                point,
                mouse::ScrollDelta::Pixels { x: 0.0, y: -3.0 },
            )
            .expect("click mappings and virtualedit must not disable precision scrolling");
            let mut state = ScrollState::default();
            state.wheel(&frame, metrics, event);
            assert!((state.offset(&frame, 20, metrics) + 3.0).abs() < 1e-6);
            assert!(state.scroll_commands().is_empty());
            state.observe(Some(&frame), metrics, true, |_| false);
            assert!((state.offset(&frame, 20, metrics) + 3.0).abs() < 1e-6);

            frame.windows[1].native_scroll = false;
            assert!(wheel_at(
                &frame,
                metrics,
                point,
                mouse::ScrollDelta::Pixels { x: 0.0, y: -3.0 }
            )
            .is_none());
            state.observe(Some(&frame), metrics, true, |_| false);
            assert!(state.pixels.is_empty());
        }
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
        assert_eq!(
            state.offset(&acknowledged, 20, metrics),
            0.0,
            "ack must not reveal the ten pixels stalled behind missing margin rows"
        );
        assert!(state.scroll_commands().is_empty());
        assert!(
            state.animating(),
            "remaining pixels must enable Iced's frame subscription"
        );
    }

    #[test]
    fn successive_raw_pixels_keep_text_caret_and_ime_continuous_across_ack() {
        let mut frame = direct_frame();
        // Keep one source line and its cursor visible on both sides of a
        // whole-row Vim acknowledgement, with two genuine rows of coverage.
        frame.windows[1].cursor.row = 3;
        frame.windows[1].cursor.line = 10;
        frame.windows[1].line_lengths[1] = 0;
        frame.windows[1].line_lengths[2] = 4;
        frame.cursor = frame.windows[1].cursor.clone();
        frame
            .runs
            .iter_mut()
            .find(|run| run.window == 20 && run.line == 9)
            .map(|run| {
                run.row = 3;
                run.line = 10;
            })
            .unwrap();
        let mut edge = frame.windows[1].overscan[0].clone();
        edge.row += 1;
        edge.line += 1;
        frame.windows[1].overscan.push(edge);
        let metrics = geometry();
        let bounds = Rectangle::with_size(Size::new(640.0, 400.0));
        let mut state = ScrollState::default();
        let sample = |frame: &Vim9RenderFrame, state: &ScrollState| {
            let views = paint_views(frame, metrics, bounds, |window| {
                state.offset(frame, window, metrics)
            });
            let view = &views[1];
            let text = view
                .runs
                .iter()
                .find(|run| run.run.line == 10)
                .unwrap()
                .rect
                .y;
            let caret = view.cursor.unwrap();
            let projection = state.projection(&Arc::new(frame.clone()), metrics, metrics, bounds);
            assert_eq!(
                projection.caret(),
                Some(caret),
                "IME and drawn cursor must use identical pixel geometry"
            );
            (text, caret.y)
        };
        let start = sample(&frame, &state);
        let mut pixels = 0.0;
        let mut submitted = false;
        for delta in [1.0, 0.25].into_iter().cycle().take(34) {
            let event = wheel_at(
                &frame,
                metrics,
                Point::new(340.0, 55.0),
                mouse::ScrollDelta::Pixels { x: 0.0, y: -delta },
            )
            .unwrap();
            assert!(!event.discrete);
            state.wheel(&frame, metrics, event);
            pixels += delta;
            let position = sample(&frame, &state);
            assert!(
                (position.0 - (start.0 - pixels)).abs() < 0.001,
                "text quantized after {pixels}px"
            );
            assert!(
                (position.1 - (start.1 - pixels)).abs() < 0.001,
                "caret quantized after {pixels}px"
            );
            assert_eq!(state.offset(&frame, 10, metrics), 0.0);
            if !submitted && !state.scroll_commands().is_empty() {
                state.scroll_submitted(20, 73);
                submitted = true;
            }
        }
        assert!(submitted);
        let before_ack = sample(&frame, &state);
        frame.windows[1].topline += 1;
        frame.windows[1].botline += 1;
        frame.windows[1].scroll_ack = 73;
        frame.windows[1].cursor.row -= 1;
        frame.windows[1].line_lengths.rotate_left(1);
        for edge in &mut frame.windows[1].overscan {
            edge.line += 1;
            for run in &mut edge.runs {
                run.line += 1;
            }
        }
        frame.cursor = frame.windows[1].cursor.clone();
        frame.runs.retain(|run| run.window != 20 || run.line >= 9);
        for run in frame.runs.iter_mut().filter(|run| run.window == 20) {
            run.row = run.row.saturating_sub(1);
        }
        state.observe(Some(&frame), metrics, true, |_| false);
        let after_ack = sample(&frame, &state);
        assert!((after_ack.0 - before_ack.0).abs() < 0.001);
        assert!((after_ack.1 - before_ack.1).abs() < 0.001);
        assert!(
            !state.animating(),
            "raw pixels must not acquire duplicate touchpad momentum"
        );
    }

    #[test]
    fn pending_precision_ack_keeps_display_frames_until_settled_or_cancelled() {
        let frame = direct_frame();
        let metrics = geometry();
        let mut state = ScrollState::default();
        state.wheel(&frame, metrics, wheel(1.0));
        state.scroll_submitted(20, 76);
        assert!(
            !state.animating(),
            "sampled margin is already fully exposed"
        );
        assert!(
            state.needs_frame(),
            "pending ack must be polled on every display frame"
        );
        let mut acknowledged = frame.clone();
        acknowledged.windows[1].topline += 1;
        acknowledged.windows[1].scroll_ack = 76;
        state.observe(Some(&acknowledged), metrics, true, |_| false);
        assert!(
            !state.needs_frame(),
            "idle editor must stop requesting display frames"
        );
        state.wheel(&acknowledged, metrics, wheel(1.0));
        state.scroll_submitted(20, 77);
        assert!(state.needs_frame());
        state.clear();
        assert!(!state.needs_frame());
    }

    #[test]
    fn completion_resets_pixel_offsets_and_retains_raw_mouse_ownership() {
        let mut frame = direct_frame();
        let metrics = geometry();
        let mut state = ScrollState::default();
        state.wheel(&frame, metrics, wheel(0.5));
        assert_eq!(state.offset(&frame, 20, metrics), -10.0);
        frame.completion = Some(
            serde_json::from_value(serde_json::json!({
                "view": {"row": 2, "column": 38, "rows": 2, "columns": 16},
                "default_style": {"background": 0x123456},
                "runs": [{"row": 2, "column": 38, "text": "native candidate", "cells": 16}]
            }))
            .unwrap(),
        );
        // The overlay can extend from one split across the separator. Its
        // appearance immediately cancels stale source-bound wheel motion.
        assert_eq!(state.offset(&frame, 20, metrics), 0.0);
        assert!(wheel_at(
            &frame,
            metrics,
            Point::new(340.0, 35.0),
            mouse::ScrollDelta::Lines { x: 0.0, y: -1.0 }
        )
        .is_none());
        state.observe(Some(&frame), metrics, true, |_| false);
        assert!(state.pixels.is_empty());
        state.wheel(&frame, metrics, wheel(1.0));
        assert!(state.scroll_commands().is_empty());
        assert!(matches!(
            state.route_mouse(&frame, metrics, keyboard::Modifiers::default(), press(1)),
            NativeInput::Compatibility(_)
        ));
    }

    #[test]
    fn vim_popups_reset_motion_without_capturing_vim_mouse_input() {
        let mut frame = direct_frame();
        let metrics = geometry();
        let mut state = ScrollState::default();
        state.wheel(&frame, metrics, wheel(0.5));
        assert_eq!(state.offset(&frame, 20, metrics), -10.0);
        frame.popups = serde_json::from_value(serde_json::json!([
            {"id":1001,"zindex":50,
             "view":{"row":2,"column":38,"rows":2,"columns":16},
             "default_style":{"background":0x123456},
             "runs":[{"row":2,"column":38,"text":"文档","cells":4}]}
        ]))
        .unwrap();
        assert_eq!(state.offset(&frame, 20, metrics), 0.0);
        assert!(wheel_at(
            &frame,
            metrics,
            Point::new(340.0, 35.0),
            mouse::ScrollDelta::Lines { x: 0.0, y: -1.0 }
        )
        .is_none());
        state.observe(Some(&frame), metrics, true, |_| false);
        assert!(state.pixels.is_empty());
        state.wheel(&frame, metrics, wheel(1.0));
        assert!(state.scroll_commands().is_empty());
        assert!(matches!(
            state.route_mouse(&frame, metrics, keyboard::Modifiers::default(), press(1)),
            NativeInput::Compatibility(_)
        ));
    }

    #[test]
    fn completion_consumes_preexisting_native_drag_tail_but_passes_new_presses() {
        let frame = Arc::new(direct_frame());
        let metrics = geometry();
        let mut state = ScrollState {
            position: Some(Point::new(340.0, 35.0)),
            ..Default::default()
        };
        state.wheel(&frame, metrics, wheel(0.5));
        state.record_presentation(
            &frame,
            metrics,
            metrics,
            Rectangle::with_size(Size::new(640.0, 400.0)),
        );
        assert!(matches!(
            state.route_mouse(&frame, metrics, keyboard::Modifiers::default(), press(1)),
            NativeInput::Handled
        ));
        assert!(state.native_pressed);
        let mut completion = (*frame).clone();
        completion.completion = Some(Default::default());
        let release = MouseInput::Release {
            col: 42,
            row: 2,
            button: MouseButton::Left,
        };
        assert!(
            matches!(
                state.route_mouse(
                    &completion,
                    metrics,
                    keyboard::Modifiers::default(),
                    release
                ),
                NativeInput::Handled
            ),
            "a captured pre-menu press must not leak an orphan PTY release"
        );
        assert!(
            matches!(
                state.route_mouse(
                    &completion,
                    metrics,
                    keyboard::Modifiers::default(),
                    press(1)
                ),
                NativeInput::Compatibility(_)
            ),
            "a fresh menu press belongs to Vim even before its first draw"
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
    fn queued_stationary_source_changes_consume_press_drag_release() {
        let old = Arc::new(direct_frame());
        let metrics = geometry();
        let bounds = Rectangle::with_size(Size::new(640.0, 400.0));
        for change in 0..3 {
            let mut state = ScrollState {
                position: Some(Point::new(340.0, 35.0)),
                ..Default::default()
            };
            state.record_presentation(&old, metrics, metrics, bounds);
            let snapshot = state.capture_body_mouse();
            let mut current = (*old).clone();
            match change {
                0 => current.windows[1].buffer += 1,
                1 => current.windows[1].changedtick += 1,
                _ => current.windows[1].topline += 1,
            }
            let current = Arc::new(current);
            assert!(!snapshot.source_current(Some(&current), metrics));
            state.record_presentation(&current, metrics, metrics, bounds);
            assert!(matches!(
                state.route_body_mouse(Some(&current), metrics, press(1), snapshot),
                NativeInput::Handled
            ));
            for input in [
                MouseInput::Drag {
                    col: 43,
                    row: 2,
                    count: 1,
                },
                MouseInput::Release {
                    col: 43,
                    row: 2,
                    button: MouseButton::Left,
                },
            ] {
                let snapshot = state.capture_body_mouse();
                assert!(matches!(
                    state.route_body_mouse(Some(&current), metrics, input, snapshot),
                    NativeInput::Handled
                ));
            }
            assert!(state.pointer_command().is_none());
            assert!(!state.native_pressed);
        }
        let mut first_compat = ScrollState::default();
        assert!(
            matches!(
                first_compat.route_body_mouse(
                    None,
                    metrics,
                    press(1),
                    BodyMouseSnapshot::default()
                ),
                NativeInput::Compatibility(MouseInput::Press { .. })
            ),
            "a genuine first compatibility surface remains raw"
        );
    }

    #[test]
    fn body_focus_precedes_source_press_and_static_click_counts_remain_raw() {
        let frame = Arc::new(direct_frame());
        let metrics = geometry();
        let bounds = Rectangle::with_size(Size::new(640.0, 400.0));
        let panes = crate::PaneTree::Split {
            axis: crate::Axis::Vertical,
            children: vec![crate::PaneTree::Leaf(0), crate::PaneTree::Leaf(1)],
            ratios: vec![0.5, 0.5],
        };
        let mut states = [ScrollState::default(), ScrollState::default()];
        states[1].wheel(&frame, metrics, wheel(0.5));
        states[1].record_presentation(&frame, metrics, metrics, bounds);
        let target =
            crate::visible_embedded_body_session(&panes, 202, true, [(0, 101), (1, 202)]).unwrap();
        assert_eq!(target, 1);
        // Production focus_embedded clears old state before this raw press is
        // routed. The retained presentation still supplies its exact source.
        states[target].position = Some(Point::new(340.0, 35.0));
        let snapshot = states[target].capture_body_mouse();
        states[target].clear();
        let mut replacement = (*frame).clone();
        replacement.windows[1].topline += 5;
        for run in replacement.runs.iter_mut().filter(|run| run.window == 20) {
            run.line += 5;
        }
        let replacement = Arc::new(replacement);
        states[target].record_presentation(&replacement, metrics, metrics, bounds);
        states[target].position = Some(Point::new(20.0, 35.0));
        assert!(matches!(
            states[target].route_body_mouse(Some(&replacement), metrics, press(1), snapshot),
            NativeInput::Handled
        ));
        assert!(matches!(
            states[target].pointer_command(),
            Some(Vim9Command::Pointer {
                window: 20,
                buffer: 4,
                line: 9,
                phase: Vim9PointerPhase::Press,
                ..
            })
        ));
        assert!(
            states[0].pointer_command().is_none(),
            "the formerly focused pane receives nothing"
        );
        assert_eq!(
            states[1].presented.as_ref().unwrap().frame.windows[1].topline,
            replacement.windows[1].topline,
            "routing a frozen event must not replace the newest painted frame"
        );
        states[1].clear();
        states[1].native_pressed = false;
        states[1].record_presentation(&frame, metrics, metrics, bounds);
        for count in [1, 2, 3] {
            assert!(
                matches!(states[1].route_mouse(&frame, metrics, keyboard::Modifiers::empty(), press(count)),
                NativeInput::Compatibility(MouseInput::Press { count: actual, .. }) if actual == count)
            );
            assert!(states[1].pointer_command().is_none());
        }
    }

    #[test]
    fn clicking_the_same_cell_restarts_native_cursor_wait_even_on_compatibility_path() {
        let mut frame = direct_frame();
        frame.cursor.appearance = Some(jterm_core::vim9::Vim9CursorAppearance {
            blinkwait: 100,
            blinkoff: 50,
            blinkon: 50,
            ..Default::default()
        });
        let mut state = ScrollState::default();
        let now = std::time::Instant::now();
        assert!(state.cursor_blink.sample(&frame, true, now).0);
        let off = now + std::time::Duration::from_millis(101);
        assert!(!state.cursor_blink.sample(&frame, true, off).0);
        assert!(matches!(
            state.route_mouse(&frame, geometry(), keyboard::Modifiers::empty(), press(1)),
            NativeInput::Compatibility(_)
        ));
        let (visible, wait) = state.cursor_blink.sample(&frame, true, off);
        assert!(visible);
        assert_eq!(wait, Some(std::time::Duration::from_millis(100)));
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
        let completion_frame = Arc::new(Vim9RenderFrame {
            completion: Some(Default::default()),
            ..(*frame).clone()
        });
        let mut render_only = (*frame).clone();
        render_only.windows[1].native_pointer = false;
        render_only.windows[1].native_scroll = false;
        let render_only = Arc::new(render_only);
        let input_frame = RefCell::new(frame.clone());
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
                &input_frame.borrow(),
                local_metrics(metrics),
                keyboard::Modifiers::default(),
                input,
            ))
        });
        let mut widget =
            NativeBuffer::new(input, Some(&frame), metrics, Font::MONOSPACE, true, true).scroll(
                &state,
                keyboard::Modifiers::default(),
                false,
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
        assert!(
            !wheel.discrete,
            "raw pixels must survive smooth_scroll=false"
        );
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
        let released = keyboard::Key::Character("i".into());
        for event in [
            Event::InputMethod(input_method::Event::Opened),
            Event::InputMethod(input_method::Event::Closed),
            Event::InputMethod(input_method::Event::Preedit(String::new(), None)),
            Event::Keyboard(keyboard::Event::KeyReleased {
                key: released.clone(),
                modified_key: released,
                physical_key: keyboard::key::Physical::Code(keyboard::key::Code::KeyI),
                location: keyboard::Location::Standard,
                modifiers: keyboard::Modifiers::default(),
            }),
        ] {
            <NativeBuffer<'_, Output> as Widget<Output, iced::Theme, ()>>::update(
                &mut widget,
                &mut tree,
                &event,
                Layout::new(&node),
                mouse::Cursor::Available(point),
                &(),
                &mut iced::advanced::clipboard::Null,
                &mut Shell::new(&mut output),
                &bounds,
            );
            assert_eq!(
                state.borrow().offset(&frame, 20, local_metrics(metrics)),
                -10.0,
                "IME lifecycle and key releases must not cancel precision motion"
            );
        }
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
        output.clear();
        <NativeBuffer<'_, Output> as Widget<Output, iced::Theme, ()>>::update(
            &mut widget,
            &mut tree,
            &Event::Mouse(mouse::Event::WheelScrolled {
                delta: mouse::ScrollDelta::Lines { x: 0.0, y: -1.0 },
            }),
            Layout::new(&node),
            mouse::Cursor::Available(point),
            &(),
            &mut iced::advanced::clipboard::Null,
            &mut Shell::new(&mut output),
            &bounds,
        );
        assert!(
            matches!(output.as_slice(), [Output::Wheel(wheel)] if wheel.lines == 3.0 && !wheel.discrete),
            "smooth_scroll=false must only disable the synthesized notch animation"
        );
        // A menu can arrive between the last buffer paint and the next
        // event. Its wheel must reach the PTY, even while the stored
        // presentation still advertises normal-mode semantic scrolling.
        widget.frame = Some(&completion_frame);
        output.clear();
        <NativeBuffer<'_, Output> as Widget<Output, iced::Theme, ()>>::update(
            &mut widget,
            &mut tree,
            &Event::Mouse(mouse::Event::WheelScrolled {
                delta: mouse::ScrollDelta::Lines { x: 0.0, y: -1.0 },
            }),
            Layout::new(&node),
            mouse::Cursor::Available(point),
            &(),
            &mut iced::advanced::clipboard::Null,
            &mut Shell::new(&mut output),
            &bounds,
        );
        assert!(
            matches!(
                output.as_slice(),
                [Output::Mouse(NativeInput::Compatibility(
                    MouseInput::Wheel { .. }
                ))]
            ),
            "a completion wheel must not be captured against an older native presentation"
        );
        // A new render-only frame must not reinterpret a previously shown
        // fractional image as stationary PTY cells before it has been drawn.
        state
            .borrow_mut()
            .wheel(&frame, local_metrics(metrics), self::wheel(0.5));
        state.borrow_mut().record_presentation(
            &frame,
            render_metrics(bounds, metrics),
            local_metrics(metrics),
            bounds,
        );
        state
            .borrow_mut()
            .observe(Some(&render_only), local_metrics(metrics), true, |_| false);
        *input_frame.borrow_mut() = render_only.clone();
        widget.frame = Some(&render_only);
        output.clear();
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
        let [Output::Wheel(stale_wheel)] = output.as_slice() else {
            panic!("the old fractional wheel must not fall through to PTY");
        };
        state
            .borrow_mut()
            .wheel(&render_only, local_metrics(metrics), stale_wheel.clone());
        assert!(
            !state.borrow().needs_frame(),
            "current render-only capabilities reject the stale native wheel"
        );
        output.clear();
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
        assert_eq!(
            output.len(),
            2,
            "the obsolete image owns both press and release"
        );
        assert!(output
            .iter()
            .all(|event| matches!(event, Output::Mouse(NativeInput::Handled))));
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
        output.clear();
        for event in [
            mouse::Event::WheelScrolled {
                delta: mouse::ScrollDelta::Lines { x: 0.0, y: -1.0 },
            },
            mouse::Event::ButtonPressed(mouse::Button::Left),
            mouse::Event::ButtonReleased(mouse::Button::Left),
            mouse::Event::ButtonPressed(mouse::Button::Left),
            mouse::Event::ButtonReleased(mouse::Button::Left),
            mouse::Event::ButtonPressed(mouse::Button::Middle),
            mouse::Event::ButtonReleased(mouse::Button::Middle),
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
        assert!(matches!(
            output.first(),
            Some(Output::Mouse(NativeInput::Compatibility(
                MouseInput::Wheel { .. }
            )))
        ));
        assert!(output
            .iter()
            .all(|event| matches!(event, Output::Mouse(NativeInput::Compatibility(_)))));
        assert!(output.iter().any(|event| matches!(event, Output::Mouse(NativeInput::Compatibility(MouseInput::Press { count, .. })) if *count >= 2)));
        assert!(state.borrow().pointer_command().is_none());
        assert!(!state.borrow().needs_frame());
        output.clear();
        let enter = keyboard::Key::Named(keyboard::key::Named::Enter);
        let mut shell = Shell::new(&mut output);
        <NativeBuffer<'_, Output> as Widget<Output, iced::Theme, ()>>::update(
            &mut widget,
            &mut tree,
            &Event::Keyboard(keyboard::Event::KeyPressed {
                key: enter.clone(),
                modified_key: enter,
                physical_key: keyboard::key::Physical::Code(keyboard::key::Code::Enter),
                location: keyboard::Location::Standard,
                modifiers: keyboard::Modifiers::default(),
                text: Some("\r".into()),
                repeat: false,
            }),
            Layout::new(&node),
            mouse::Cursor::Available(point),
            &(),
            &mut iced::advanced::clipboard::Null,
            &mut shell,
            &bounds,
        );
        assert!(
            !shell.is_event_captured(),
            "Enter must remain available to the existing PTY keyboard subscription"
        );
        assert!(output.is_empty());
        // Disabling semantic input on one split must not affect its peer.
        <NativeBuffer<'_, Output> as Widget<Output, iced::Theme, ()>>::update(
            &mut widget,
            &mut tree,
            &Event::Mouse(mouse::Event::WheelScrolled {
                delta: mouse::ScrollDelta::Pixels { x: 0.0, y: -10.0 },
            }),
            Layout::new(&node),
            mouse::Cursor::Available(Point::new(20.0, 35.0)),
            &(),
            &mut iced::advanced::clipboard::Null,
            &mut Shell::new(&mut output),
            &bounds,
        );
        assert!(matches!(output.as_slice(), [Output::Wheel(wheel)] if wheel.window == 10));
        // Real widget event masks must outlive later model/key-up messages.
        for expected in [keyboard::Modifiers::CTRL, keyboard::Modifiers::SHIFT] {
            state.borrow_mut().seed_modifiers(expected);
            output.clear();
            <NativeBuffer<'_, Output> as Widget<Output, iced::Theme, ()>>::update(
                &mut widget,
                &mut tree,
                &Event::Keyboard(keyboard::Event::ModifiersChanged(expected)),
                Layout::new(&node),
                mouse::Cursor::Available(point),
                &(),
                &mut iced::advanced::clipboard::Null,
                &mut Shell::new(&mut output),
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
            let raw = output
                .iter()
                .find_map(|event| match event {
                    Output::Mouse(NativeInput::Compatibility(input @ MouseInput::Press { .. })) => {
                        Some(*input)
                    }
                    _ => None,
                })
                .expect("the child must publish its actual modified press");
            assert!(
                matches!(raw, MouseInput::Press { ctrl, shift, .. } if ctrl == expected.control() && shift == expected.shift())
            );
            let snapshot = state.borrow().capture_body_mouse();
            <NativeBuffer<'_, Output> as Widget<Output, iced::Theme, ()>>::update(
                &mut widget,
                &mut tree,
                &Event::Keyboard(keyboard::Event::ModifiersChanged(
                    keyboard::Modifiers::empty(),
                )),
                Layout::new(&node),
                mouse::Cursor::Available(point),
                &(),
                &mut iced::advanced::clipboard::Null,
                &mut Shell::new(&mut output),
                &bounds,
            );
            state.borrow_mut().seed_modifiers(expected);
            assert!(
                state.borrow().event_modifiers().is_empty(),
                "an unchanged stale view seed must not roll back key-up"
            );
            assert_eq!(snapshot.modifiers(), expected);
            assert!(
                matches!(state.borrow_mut().route_body_mouse(Some(&render_only), local_metrics(metrics), raw, snapshot),
                NativeInput::Compatibility(MouseInput::Press { ctrl, shift, .. }) if ctrl == expected.control() && shift == expected.shift())
            );
            assert!(state.borrow().pointer_command().is_none());
        }
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
    fn measured_monospace_styles_batch_native_runs_without_changing_grid_positions() {
        let font = crate::terminal_view::resolve_native_editor_font(Font::MONOSPACE);
        let mut checked = 0;
        let mut styled_batches = 0;
        for size in [14.0, 18.9] {
            let metrics = Metrics::with_font(font, size, 1.0, 0.0, false);
            if !metrics.mono_advance_exact {
                continue;
            }
            let mut terminal = crate::terminal::TerminalState::new(80, 20);
            let projection = terminal.get_projected_viewport(false);
            let theme = crate::Theme::default();
            let input: TermWidget<'_, ()> = TermWidget::new(
                projection.cells(),
                (2, 42),
                true,
                crate::terminal::CursorShape::Block,
                true,
                &theme,
                metrics,
                font,
                None,
                None,
                None,
                None,
                Vec::new(),
                0,
                0,
            );
            let widget = NativeBuffer::new(input, None, metrics, font, true, true);
            let view = Rectangle::with_size(Size::new(2000.0, 200.0));
            for bold in [false, true] {
                for italic in [false, true] {
                    let text = "let value = foo != 0; // ASCII";
                    let run = Vim9RenderRun {
                        text: text.into(),
                        cells: text.len(),
                        style: Vim9RenderStyle {
                            bold,
                            italic,
                            foreground: Some(0x12abef),
                            ..Default::default()
                        },
                        ..Default::default()
                    };
                    let projected = Vim9ProjectedRun {
                        run: &run,
                        rect: Vim9PixelRect {
                            x: 10.0,
                            y: 5.0,
                            width: run.cells as f32 * metrics.cell_w,
                            height: metrics.cell_h,
                        },
                        clip: Vim9PixelRect {
                            x: view.x,
                            y: view.y,
                            width: view.width,
                            height: view.height,
                        },
                    };
                    let resolved =
                        crate::terminal_view::native_editor_glyph_font(font, text, bold, italic);
                    let batch = resolved == font
                        || crate::terminal_view::native_editor_font_matches_grid(resolved, metrics);
                    let mut recorder = CompletionRecorder::default();
                    renderer::Renderer::start_layer(&mut recorder, view);
                    widget.paint_ink(
                        &mut recorder,
                        &Vim9RenderStyle::default(),
                        &projected,
                        view,
                        None,
                    );
                    assert_eq!(
                        recorder.text.len(),
                        if batch { 1 } else { text.len() },
                        "bold={bold} italic={italic} size={size}"
                    );
                    assert_eq!(
                        recorder
                            .text
                            .iter()
                            .map(|(text, _, _, _)| text.as_str())
                            .collect::<String>(),
                        text
                    );
                    for (index, (_, position, color, _)) in recorder.text.iter().enumerate() {
                        assert_eq!(position.x, 10.0 + index as f32 * metrics.cell_w);
                        assert_eq!(*color, rgb(0x12abef));
                    }
                    if batch && (bold || italic) {
                        styled_batches += 1;
                    }
                    checked += 1;
                }
            }
        }
        eprintln!(
            "verified {checked} native font styles, {styled_batches} styled whole-run batches"
        );
    }

    #[test]
    fn clipped_long_runs_submit_only_visible_fallback_glyphs_and_curl_segments() {
        let mut terminal = crate::terminal::TerminalState::new(80, 20);
        let projection = terminal.get_projected_viewport(false);
        let theme = crate::Theme::default();
        let mut metrics = Metrics::new(14.0, 1.0, 0.0, false);
        metrics.cell_w = 10.0;
        metrics.cell_h = 20.0;
        metrics.mono_advance_exact = false;
        let input: TermWidget<'_, ()> = TermWidget::new(
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
        );
        let widget = NativeBuffer::new(input, None, metrics, Font::MONOSPACE, true, true);
        let view = Rectangle {
            x: 10.25,
            y: 0.0,
            width: 80.0,
            height: 10.0,
        };
        let run = Vim9RenderRun {
            text: "x".repeat(20000),
            cells: 20000,
            style: Vim9RenderStyle {
                undercurl: true,
                italic: true,
                ..Default::default()
            },
            ..Default::default()
        };
        let rect = Vim9PixelRect {
            x: -100000.25,
            y: -7.5,
            width: 200000.0,
            height: 20.0,
        };
        let projected = Vim9ProjectedRun {
            run: &run,
            rect,
            clip: Vim9PixelRect {
                x: view.x,
                y: view.y,
                width: view.width,
                height: view.height,
            },
        };
        let mut recorder = CompletionRecorder::default();
        renderer::Renderer::start_layer(&mut recorder, view);
        widget.paint_ink(
            &mut recorder,
            &Vim9RenderStyle::default(),
            &projected,
            view,
            None,
        );
        assert_eq!(
            recorder.text.len(),
            10,
            "only visible columns and neighboring overhang candidates submit glyphs"
        );
        assert!(
            recorder.quads.len() <= 31,
            "hidden curl segments must not submit quads"
        );
        let cell = Rectangle {
            x: 20.0,
            y: 0.0,
            width: 10.0,
            height: 20.0,
        };
        let whole = Rectangle::with_size(Size::new(100.0, 20.0));
        assert_eq!(
            visible_ascii_columns(
                whole,
                Rectangle {
                    x: cell.x - 3.5,
                    width: cell.width + 7.0,
                    ..cell
                },
                10.0,
                10
            ),
            1..4,
            "adjacent italic glyphs may overhang into a boundary-aligned clip"
        );
        let step = (metrics.font_size / 5.0).max(2.0);
        let start = ((view.x - rect.x) / step).floor() as usize;
        let thickness = (metrics.font_size / 14.0).clamp(1.0, 2.0);
        let baseline = rect.y + rect.height - thickness * 1.5;
        assert_eq!(
            recorder.quads[0].0.bounds.y,
            baseline
                - if start.is_multiple_of(2) {
                    thickness
                } else {
                    0.0
                }
        );
        assert!(recorder
            .text
            .iter()
            .all(|(text, _, _, clip)| text == "x" && *clip == view));
        let hidden = Rectangle {
            x: 300000.0,
            ..view
        };
        let counts = (recorder.text.len(), recorder.quads.len());
        widget.paint_ink(
            &mut recorder,
            &Vim9RenderStyle::default(),
            &projected,
            hidden,
            None,
        );
        assert_eq!((recorder.text.len(), recorder.quads.len()), counts);
        let unicode = Vim9RenderRun {
            text: "界e\u{301}".into(),
            cells: 3,
            ..Default::default()
        };
        let projected = Vim9ProjectedRun {
            run: &unicode,
            rect: Vim9PixelRect {
                x: 0.0,
                y: 0.0,
                width: 30.0,
                height: 20.0,
            },
            ..projected
        };
        widget.paint_ink(
            &mut recorder,
            &Vim9RenderStyle::default(),
            &projected,
            view,
            None,
        );
        assert_eq!(
            recorder.text.last().unwrap().0,
            unicode.text,
            "Unicode/combining text must retain whole-run shaping"
        );
        let ascii = Vim9RenderRun {
            text: "abcdefghij".into(),
            cells: 10,
            style: Vim9RenderStyle {
                italic: true,
                ..Default::default()
            },
            ..Default::default()
        };
        let projected = Vim9ProjectedRun {
            run: &ascii,
            rect: Vim9PixelRect {
                x: 0.0,
                y: 0.0,
                width: 100.0,
                height: 20.0,
            },
            ..projected
        };
        let clip = Rectangle {
            x: 20.0,
            y: 0.0,
            width: 10.0,
            height: 20.0,
        };
        recorder.text.clear();
        widget.paint_ink(
            &mut recorder,
            &Vim9RenderStyle::default(),
            &projected,
            clip,
            None,
        );
        assert_eq!(
            recorder
                .text
                .iter()
                .map(|(text, _, _, _)| text.as_str())
                .collect::<String>(),
            "bcd",
            "both italic neighbors must be submitted at a cell-aligned clip"
        );
        assert_eq!(
            recorder
                .text
                .iter()
                .map(|(_, pos, _, _)| pos.x)
                .collect::<Vec<_>>(),
            vec![10.0, 20.0, 30.0]
        );
    }

    #[test]
    fn space_only_runs_skip_glyphs_but_keep_backgrounds_and_decorations() {
        let mut terminal = crate::terminal::TerminalState::new(80, 20);
        let projection = terminal.get_projected_viewport(false);
        let theme = crate::Theme::default();
        let mut metrics = Metrics::new(14.0, 1.0, 0.0, false);
        metrics.cell_w = 8.0;
        metrics.cell_h = 20.0;
        let input: TermWidget<'_, ()> = TermWidget::new(
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
        );
        let widget = NativeBuffer::new(input, None, metrics, Font::MONOSPACE, true, true);
        let editor = Rectangle::with_size(Size::new(640.0, 400.0));
        for text in ["", "        ", "   x   ", "·", "\u{a0}", "界", "e\u{301}"] {
            for decoration in 0..4 {
                let mut frame = direct_frame();
                frame.runs.truncate(1);
                frame.runs[0].text = text.into();
                frame.runs[0].cells = 8;
                frame.runs[0].style = Vim9RenderStyle {
                    background: Some(0x123456),
                    underline: decoration == 1,
                    undercurl: decoration == 2,
                    strikethrough: decoration == 3,
                    ..Default::default()
                };
                for window in &mut frame.windows {
                    window.overscan.clear();
                }
                let mut recorder = CompletionRecorder::default();
                for prepared in
                    paint_views(&frame, render_metrics(editor, metrics), editor, |_| 0.0)
                {
                    widget.paint_native_view(&mut recorder, &frame, &prepared, false);
                }
                assert_eq!(
                    !recorder.text.is_empty(),
                    !text.is_empty() && text != "        ",
                    "glyph submission for {text:?} with decoration {decoration}"
                );
                assert!(recorder
                    .quads
                    .iter()
                    .any(|(_, background, _)| *background == Background::Color(rgb(0x123456))));
                let decorated = recorder
                    .quads
                    .iter()
                    .any(|(quad, _, _)| quad.bounds.height < 5.0);
                assert_eq!(decorated, decoration != 0);
            }
        }
    }

    #[test]
    fn cursor_inverts_topmost_glyph_style_above_background_runs() {
        let mut frame = direct_frame();
        let index = frame
            .runs
            .iter()
            .position(|run| {
                run.row == frame.cursor.row
                    && run.column == frame.cursor.column
                    && run.window == frame.window
            })
            .unwrap();
        frame.runs[index].style.foreground = Some(0xaabbcc);
        frame.runs[index].style.background = Some(0x223344);
        let mut background = frame.runs[index].clone();
        background.text.clear();
        background.line = 0;
        background.byte_start = 0;
        background.byte_end = 0;
        background.style.foreground = Some(0x556677);
        background.style.background = Some(0x112233);
        frame.runs.insert(0, background);
        let bounds = Rectangle::with_size(Size::new(640.0, 400.0));
        let views = paint_views(&frame, geometry(), bounds, |_| 0.0);
        let active = views.iter().find(|view| view.active).unwrap();
        let appearance = jterm_core::vim9::Vim9CursorAppearance::default();
        assert_eq!(
            appearance.colors(
                active.cursor_style(active.raw_cursor.unwrap(), &frame.default_style),
                &frame.default_style
            ),
            (0x223344, 0xaabbcc)
        );
        let background_only = PaintView {
            normal: active.normal,
            clip: active.clip,
            active: true,
            runs: vec![active.runs[0]],
            cursor: active.cursor,
            raw_cursor: active.raw_cursor,
        };
        assert_eq!(
            appearance.colors(
                background_only.cursor_style(active.raw_cursor.unwrap(), &frame.default_style),
                &frame.default_style
            ),
            (0x112233, 0x556677),
            "an empty source cell must still inherit its painted background"
        );
    }

    #[test]
    fn source_row_highlight_is_not_a_window_normal_marker() {
        let mut frame = direct_frame();
        let window = &frame.windows[0];
        frame.runs.insert(
            0,
            jterm_core::vim9::Vim9RenderRun {
                window: window.window,
                row: window.view.row,
                column: window.view.column,
                cells: window.view.columns,
                line: window.topline,
                style: Vim9RenderStyle {
                    background: Some(0xaa2266),
                    ..Default::default()
                },
                ..Default::default()
            },
        );
        let views = paint_views(
            &frame,
            geometry(),
            Rectangle::with_size(Size::new(640.0, 400.0)),
            |_| 0.0,
        );
        assert_eq!(
            views[0].normal, &frame.default_style,
            "a QuickFixLine full-width source-row background must not become pane Normal"
        );
        assert_eq!(
            views[0].runs[0].run.style.background,
            Some(0xaa2266),
            "the highlighted row still retains its own background"
        );
    }

    #[test]
    fn scrolled_splits_keep_independent_normal_and_focus_colors() {
        let mut frame = direct_frame();
        frame.cursor.appearance = Some(Default::default());
        let palette = [(0xaabbcc, 0x102030), (0xddaabb, 0x304050)];
        for (window, (fg, bg)) in frame.windows.iter().zip(palette) {
            frame.runs.insert(
                0,
                jterm_core::vim9::Vim9RenderRun {
                    window: window.window,
                    row: window.view.row,
                    column: window.view.column,
                    cells: window.view.columns,
                    text: String::new(),
                    style: Vim9RenderStyle {
                        foreground: Some(fg),
                        background: Some(bg),
                        ..Default::default()
                    },
                    ..Default::default()
                },
            );
        }
        let bounds = Rectangle::with_size(Size::new(640.0, 400.0));
        let views = paint_views(&frame, geometry(), bounds, |_| -12.5);
        for (view, (fg, bg)) in views.iter().zip(palette) {
            assert_eq!(
                colors(view.normal, view.normal),
                (rgb(fg), rgb(bg), rgb(fg))
            );
        }
        let active = views.iter().find(|view| view.active).unwrap();
        assert_eq!(
            frame.cursor.appearance.as_ref().unwrap().colors(
                active.cursor_style(active.raw_cursor.unwrap(), active.normal),
                active.normal
            ),
            (0x304050, 0xddaabb),
            "unspecified glyph styles must inherit the owning split Normal"
        );
        let mut recorder = CompletionRecorder::default();
        paint_split_focus(&mut recorder, &frame, active, true);
        assert!(!recorder.quads.is_empty());
        assert!(
            recorder.quads.iter().all(|(_, color, _)| *color
                == Background::Color(Color {
                    a: 0.42,
                    ..rgb(0xddaabb)
                })),
            "focus outline must use the active window palette rather than global Normal"
        );
    }

    #[test]
    fn fractional_scroll_clips_cursor_without_resizing_its_source_cell() {
        let mut frame = direct_frame();
        let metrics = geometry();
        let bounds = Rectangle::with_size(Size::new(640.0, 400.0));
        frame.windows[1].cursor.row = frame.windows[1].view.row;
        frame.cursor = frame.windows[1].cursor.clone();
        let views = paint_views(
            &frame,
            metrics,
            bounds,
            |id| if id == 20 { -12.5 } else { 0.0 },
        );
        let view = views.iter().find(|view| view.active).unwrap();
        let raw = view.raw_cursor.unwrap();
        let visible = view.cursor.unwrap();
        assert_eq!(raw.height, metrics.line_height);
        assert_eq!(visible.y, view.clip.y);
        assert_eq!(raw.y + 12.5, visible.y);
        assert_eq!(visible.height, metrics.line_height - 12.5);
        let appearance = jterm_core::vim9::Vim9CursorAppearance {
            shape: jterm_core::vim9::Vim9CursorShape::Horizontal,
            percentage: 20,
            ..Default::default()
        };
        let shape = rectangle(appearance.paint_rect(
            Vim9PixelRect {
                x: raw.x,
                y: raw.y,
                width: raw.width,
                height: raw.height,
            },
            metrics.cell_width,
            1.0,
        ));
        assert_eq!(shape.height, metrics.line_height * 0.2);
        assert_eq!(shape.y + shape.height, raw.y + raw.height);
        // Cropping the bottom of the same cell must hide its underline, not
        // move the underline up to the visible edge.
        let clipped = Rectangle {
            height: raw.height * 0.5,
            ..raw
        };
        assert!(shape.intersection(&clipped).is_none());
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
        assert!(views[0].active);
        assert!(!views[1].active);
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
    fn active_split_outline_stays_inside_its_view_and_dims_on_focus_loss() {
        let mut frame = split_frame();
        frame.default_style.foreground = Some(0x123456);
        frame.default_style.background = Some(0x102030);
        frame.cursor.appearance = Some(jterm_core::vim9::Vim9CursorAppearance {
            style: Vim9RenderStyle {
                background: Some(0x102030),
                ..Default::default()
            },
            ..Default::default()
        });
        let bounds = Rectangle::with_size(Size::new(800.0, 600.0));
        let draw = |frame: &Vim9RenderFrame, focused| {
            let mut recorder = CompletionRecorder::default();
            for view in paint_views(frame, geometry(), bounds, |_| 0.0) {
                paint_split_focus(&mut recorder, frame, &view, focused);
            }
            recorder.quads
        };
        let focused = draw(&frame, true);
        assert_eq!(focused.len(), 4);
        assert_eq!(
            focused[0].1,
            Background::Color(Color {
                a: 0.42,
                ..rgb(0x123456)
            })
        );
        let active = paint_views(&frame, geometry(), bounds, |_| 0.0)[0].clip;
        for (quad, _, clip) in &focused {
            assert_eq!(*clip, active);
            assert_eq!(quad.bounds.intersection(&active), Some(quad.bounds));
            assert!(quad.bounds.width == 1.0 || quad.bounds.height == 1.0);
        }
        let area: f32 = focused.iter().map(|(quad, _, _)| quad.bounds.area()).sum();
        assert_eq!(area, active.width * 2.0 + (active.height - 2.0) * 2.0);
        let dimmed = draw(&frame, false);
        for ((_, bright, _), (_, dim, _)) in focused.iter().zip(&dimmed) {
            let (Background::Color(bright), Background::Color(dim)) = (bright, dim) else {
                panic!("split outlines use a solid accent");
            };
            assert!(dim.a < bright.a);
            assert_eq!((dim.r, dim.g, dim.b), (bright.r, bright.g, bright.b));
        }
        frame.window = 20;
        let peer = paint_views(&frame, geometry(), bounds, |_| 0.0)[1].clip;
        assert!(draw(&frame, true).iter().all(|(_, _, clip)| *clip == peer));
        frame.windows.remove(0);
        assert!(draw(&frame, true).is_empty());
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
            special: Some(0xc78d53),
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
        style.special = None;
        assert_eq!(colors(&style, &normal).2, rgb(0xc78d53));
        assert_eq!(
            colors(&style, &Vim9RenderStyle::default()).2,
            colors(&style, &Vim9RenderStyle::default()).0
        );
    }

    #[test]
    fn virtual_text_repaints_without_a_source_edit_and_keeps_split_geometry() {
        let mut frame = direct_frame();
        frame.cursor.visible = false;
        for window in &mut frame.windows {
            window.cursor.visible = false;
        }
        let source = frame.runs.clone();
        for (column, text, cells) in [(6, "参", 2), (8, "e\u{301}", 1)] {
            frame.runs.push(jterm_core::vim9::Vim9RenderRun {
                window: 10,
                row: 1,
                column,
                text: text.into(),
                cells,
                style: Vim9RenderStyle {
                    foreground: Some(0x19c3a7),
                    background: Some(0x563412),
                    ..Default::default()
                },
                ..Default::default()
            });
        }
        let mut terminal = crate::terminal::TerminalState::new(80, 20);
        let projection = terminal.get_projected_viewport(false);
        let theme = crate::Theme::default();
        let mut metrics = Metrics::new(14.0, 1.0, 0.0, false);
        metrics.cell_w = 8.0;
        metrics.cell_h = 20.0;
        let input: TermWidget<'_, ()> = TermWidget::new(
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
        );
        let widget = NativeBuffer::new(input, None, metrics, Font::MONOSPACE, true, true);
        let editor = Rectangle::with_size(Size::new(640.0, 400.0));
        let geometry = render_metrics(editor, metrics);
        let draw = |frame: &Vim9RenderFrame| {
            let mut recorder = CompletionRecorder::default();
            for prepared in paint_views(frame, geometry, editor, |_| 0.0) {
                widget.paint_native_view(&mut recorder, frame, &prepared, false);
            }
            assert!(recorder.clips.is_empty());
            recorder
        };
        let rendered = draw(&frame);
        let (_, position, color, clip) = rendered
            .text
            .iter()
            .find(|(text, ..)| text == "参")
            .expect("source-free virtual label must reach the native text painter");
        assert_eq!(*position, Point::new(48.0, 30.0));
        assert_eq!(*color, rgb(0x19c3a7));
        assert!(clip.x + clip.width <= 39.0 * 8.0);
        assert!(rendered.text.iter().any(|(text, ..)| text == "e\u{301}"));
        assert!(rendered.quads.iter().any(|(quad, color, _)| {
            *color == Background::Color(rgb(0x563412)) && quad.bounds.width == 16.0
        }));
        assert!(frame.anchor_at_cell(1, 6).is_none());

        frame.runs[source.len()].style.foreground = Some(0x0abcde);
        frame.revision += 1;
        assert!(draw(&frame)
            .text
            .iter()
            .any(|(text, _, color, _)| { text == "参" && *color == rgb(0x0abcde) }));
        frame.runs.truncate(source.len());
        frame.revision += 1;
        assert_eq!(frame.changedtick, 8);
        assert_eq!(frame.runs, source);
        let cleared = draw(&frame);
        assert!(!cleared
            .text
            .iter()
            .any(|(text, ..)| ["参", "e\u{301}"].contains(&text.as_str())));
        assert!(!cleared
            .quads
            .iter()
            .any(|(_, color, _)| *color == Background::Color(rgb(0x563412))));
    }

    #[test]
    fn inline_hints_shift_source_caret_and_ime_then_clear_at_each_zoom() {
        let mut frame = direct_frame();
        frame.window = 10;
        frame.buffer = 3;
        frame.view = frame.windows[0].view.clone();
        frame.windows[0].native_pointer = false;
        frame.windows[0].line_lengths[0] = 4;
        frame.runs.retain(|run| !(run.window == 10 && run.row == 1));
        for (column, text, cells, line, byte_start, byte_end) in [
            (2, "ab", 2, 1, 0, 2),
            (4, "参", 2, 0, 0, 0),
            (6, "e\u{301}", 1, 0, 0, 0),
            (7, "cd", 2, 1, 2, 4),
        ] {
            frame.runs.push(Vim9RenderRun {
                window: 10,
                row: 1,
                column,
                text: text.into(),
                cells,
                line,
                byte_start,
                byte_end,
                style: Vim9RenderStyle {
                    foreground: Some(if line == 0 { 0x19c3a7 } else { 0xddeeff }),
                    background: (line == 0).then_some(0x563412),
                    ..Default::default()
                },
            });
        }
        frame.cursor.row = 1;
        frame.cursor.column = 7;
        frame.cursor.line = 1;
        frame.cursor.byte_column = 2;
        frame.windows[0].cursor = frame.cursor.clone();
        let source_bytes: Vec<_> = frame
            .runs
            .iter()
            .filter(|r| r.line > 0)
            .map(|r| (r.window, r.line, r.byte_start, r.byte_end, r.text.clone()))
            .collect();
        assert!(frame.anchor_at_cell(1, 4).is_none());
        assert_eq!(frame.anchor_at_cell(1, 7).unwrap().byte_start, 2);
        let labeled = frame;
        for (cell_width, line_height) in [(8.0, 20.0), (10.5, 23.0)] {
            let mut terminal = crate::terminal::TerminalState::new(80, 20);
            let projection = terminal.get_projected_viewport(false);
            let theme = crate::Theme::default();
            let mut metrics = Metrics::new(14.0, 1.0, 0.0, false);
            metrics.cell_w = cell_width;
            metrics.cell_h = line_height;
            let input: TermWidget<'_, ()> = TermWidget::new(
                projection.cells(),
                (1, 7),
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
            );
            let widget = NativeBuffer::new(input, None, metrics, Font::MONOSPACE, true, true);
            let editor = Rectangle::with_size(Size::new(80.0 * cell_width, 20.0 * line_height));
            let geometry = render_metrics(editor, metrics);
            let draw = |frame: &Vim9RenderFrame| {
                let mut recorder = CompletionRecorder::default();
                for prepared in paint_views(frame, geometry, editor, |_| 0.0) {
                    widget.paint_native_view(&mut recorder, frame, &prepared, true);
                }
                assert!(recorder.clips.is_empty());
                recorder
            };
            let rendered = draw(&labeled);
            // A fallback face may paint ASCII as individual fixed-cell glyphs.
            for (text, column) in [("a", 2), ("参", 4), ("e\u{301}", 6), ("c", 7)] {
                assert!(
                    rendered
                        .text
                        .iter()
                        .any(|(value, position, _, _)| value.starts_with(text)
                            && position.x == column as f32 * cell_width),
                    "missing {text} at its Vim column"
                );
            }
            assert!(rendered
                .text
                .iter()
                .any(|(text, _, color, clip)| text == "参"
                    && *color == rgb(0x19c3a7)
                    && clip.x + clip.width <= 39.0 * cell_width));
            assert!(rendered
                .text
                .iter()
                .any(|(text, position, _, _)| text.starts_with('p')
                    && position.x == 42.0 * cell_width));
            let views = paint_views(&labeled, geometry, editor, |_| 0.0);
            let caret = views.iter().find(|v| v.active).unwrap().cursor.unwrap();
            let presented = PresentedFrame {
                frame: Arc::new(labeled.clone()),
                metrics: geometry,
                local_metrics: geometry,
                offsets: BTreeMap::new(),
                clip: editor,
            };
            assert_eq!(presented.caret(), Some(caret));
            assert_eq!(caret.x, 7.0 * cell_width);

            let mut cleared = labeled.clone();
            cleared.runs.retain(|run| run.line != 0);
            cleared
                .runs
                .iter_mut()
                .find(|run| run.window == 10 && run.text == "cd")
                .unwrap()
                .column = 4;
            cleared.cursor.column = 4;
            cleared.windows[0].cursor = cleared.cursor.clone();
            cleared.revision += 1;
            assert_eq!(cleared.changedtick, labeled.changedtick);
            assert_eq!(
                cleared
                    .runs
                    .iter()
                    .filter(|r| r.line > 0)
                    .map(|r| (r.window, r.line, r.byte_start, r.byte_end, r.text.clone()))
                    .collect::<Vec<_>>(),
                source_bytes
            );
            let rendered = draw(&cleared);
            assert!(!rendered
                .text
                .iter()
                .any(|(text, ..)| ["参", "e\u{301}"].contains(&text.as_str())));
            assert!(!rendered
                .quads
                .iter()
                .any(|(_, color, _)| *color == Background::Color(rgb(0x563412))));
            assert!(rendered
                .text
                .iter()
                .any(|(text, position, _, _)| text.starts_with('c')
                    && position.x == 4.0 * cell_width));
        }
    }

    #[test]
    fn vim_popup_surfaces_cover_splits_and_preserve_zorder_rgb_and_unicode() {
        let mut frame = direct_frame();
        frame.mode = "i".into();
        frame.popups = serde_json::from_value(serde_json::json!([
            {"id":1003,"zindex":50,
             "view":{"row":2,"column":38,"rows":2,"columns":16},
             "default_style":{"background":0x352413},"runs":[]},
            {"id":1001,"zindex":50,
             "view":{"row":2,"column":38,"rows":2,"columns":16},
             "default_style":{"foreground":0xeeddaa,"background":0x294761},
             "runs":[{"row":2,"column":38,"text":"文档","cells":4,
                      "style":{"foreground":0x19c3a7,"background":0x8351a7}}]},
            {"id":1002,"zindex":150,
             "view":{"row":2,"column":38,"rows":2,"columns":16},
             "default_style":{"background":0x135724},
             "runs":[{"row":3,"column":38,"text":"e\u{301}","cells":1}]}
        ]))
        .unwrap();
        frame.completion = Some(
            serde_json::from_value(serde_json::json!({
                "view":{"row":2,"column":38,"rows":2,"columns":16},
                "default_style":{"background":0x246813},"runs":[]
            }))
            .unwrap(),
        );
        let mut terminal = crate::terminal::TerminalState::new(80, 20);
        let projection = terminal.get_projected_viewport(false);
        let theme = crate::Theme::default();
        let mut metrics = Metrics::new(14.0, 1.0, 0.0, false);
        metrics.cell_w = 8.0;
        metrics.cell_h = 20.0;
        let input: TermWidget<'_, ()> = TermWidget::new(
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
        );
        let widget = NativeBuffer::new(input, None, metrics, Font::MONOSPACE, true, true);
        let editor = Rectangle::with_size(Size::new(640.0, 400.0));
        let geometry = render_metrics(editor, metrics);
        let view = rectangle(geometry.rect(&frame.popups[0].view).unwrap());
        let mut recorder = CompletionRecorder::default();
        widget.paint_overlays(&mut recorder, &frame, geometry, editor);
        assert!(recorder.clips.is_empty());
        let surfaces: Vec<_> = recorder
            .quads
            .iter()
            .filter_map(|(quad, color, clip)| {
                if quad.bounds == view
                    && [
                        Background::Color(rgb(0x352413)),
                        Background::Color(rgb(0x294761)),
                        Background::Color(rgb(0x246813)),
                        Background::Color(rgb(0x135724)),
                    ]
                    .contains(color)
                {
                    assert_eq!(*clip, view);
                    Some(*color)
                } else {
                    None
                }
            })
            .collect();
        assert_eq!(
            surfaces,
            vec![
                Background::Color(rgb(0x352413)),
                Background::Color(rgb(0x294761)),
                Background::Color(rgb(0x246813)),
                Background::Color(rgb(0x135724))
            ],
            "equal-zindex older popups stay above newer popups, before completion"
        );
        assert!(
            recorder.quads.iter().any(|(quad, color, clip)| {
                *color == Background::Color(rgb(0x8351a7))
                    && quad.bounds.width == 32.0
                    && *clip == view
            }),
            "CJK popup text must retain its four display cells across the separator"
        );
        let (_, at, color, clip) = recorder
            .text
            .iter()
            .find(|(text, ..)| text == "文档")
            .unwrap();
        assert_eq!(*at, Point::new(view.x, view.y + metrics.cell_h / 2.0));
        assert_eq!(*color, rgb(0x19c3a7));
        assert!(clip.intersection(&view).is_some());
        assert!(recorder.text.iter().any(|(text, ..)| text == "e\u{301}"));
        assert_eq!(
            recorder
                .quads
                .iter()
                .filter(|(quad, ..)| quad.border.width > 0.0)
                .count(),
            1,
            "Vim supplies popup borders; only completion adds its native outline"
        );
        frame.completion = None;
        frame.popups.clear();
        let mut closed = CompletionRecorder::default();
        widget.paint_overlays(&mut closed, &frame, geometry, editor);
        assert!(closed.quads.is_empty() && closed.text.is_empty());
    }

    #[test]
    fn completion_elevation_preserves_vim_bounds_colors_and_paint_order() {
        use renderer::Renderer as _;

        let view = Rectangle {
            x: 12.25,
            y: 14.5,
            width: 48.5,
            height: 30.25,
        };
        let foreground = rgb(0xd8e2ef);
        let background = rgb(0x183047);
        let editor = Rectangle {
            x: 10.0,
            y: 10.0,
            width: 80.0,
            height: 60.0,
        };
        for editor_clip in [
            editor,
            Rectangle {
                width: 40.0,
                height: 25.0,
                ..editor
            },
        ] {
            let mut recorder = CompletionRecorder::default();
            let content_clip = view.intersection(&editor_clip).unwrap();
            paint_overlay_surface(
                &mut recorder,
                view,
                editor_clip,
                foreground,
                true,
                false,
                |renderer, clip| {
                    assert_eq!(clip, content_clip);
                    renderer.fill_quad(quad(view), Background::Color(background));
                },
            );
            assert!(recorder.clips.is_empty());
            assert_eq!(recorder.quads.len(), 6);
            let (outer_shadow, shadow_color, shadow_clip) = recorder.quads[0];
            assert_eq!(
                outer_shadow.bounds,
                Rectangle {
                    x: view.x - 4.0,
                    y: view.y - 4.0,
                    width: view.width + 8.0,
                    height: view.height + 8.0,
                }
            );
            assert_eq!(shadow_clip, editor_clip);
            assert_eq!(
                shadow_color,
                Background::Color(Color {
                    a: 4.0 / 255.0,
                    ..Color::BLACK
                })
            );
            assert!(recorder.quads[..4]
                .iter()
                .all(|(_, _, clip)| *clip == editor_clip));
            let (content, content_color, clip) = recorder.quads[4];
            assert_eq!(
                content.bounds, view,
                "decoration must not move Vim's content"
            );
            assert_eq!(content_color, Background::Color(background));
            assert_eq!(clip, content_clip);
            let (border, fill, clip) = recorder.quads[5];
            assert_eq!(
                border.bounds, view,
                "clipping must not introduce a new border edge"
            );
            assert_eq!(border.border.width, 1.0);
            assert_eq!(
                border.border.color,
                Color {
                    a: 0.20,
                    ..foreground
                }
            );
            assert_eq!(fill, Background::Color(Color::TRANSPARENT));
            assert_eq!(clip, content_clip);
        }
        let mut recorder = CompletionRecorder::default();
        paint_overlay_surface(
            &mut recorder,
            view,
            Rectangle::with_size(Size::new(4.0, 4.0)),
            foreground,
            true,
            false,
            |_, _| panic!("offscreen completion must not paint"),
        );
        assert!(recorder.quads.is_empty());
    }
    #[test]
    fn documentation_card_shell_keeps_geometry_clip_and_rounds_only_cards() {
        let view = Rectangle {
            x: 20.0,
            y: 20.0,
            width: 80.0,
            height: 60.0,
        };
        let editor = Rectangle::with_size(Size::new(200.0, 120.0));
        for card in [false, true] {
            let mut recorder = CompletionRecorder::default();
            paint_overlay_surface(
                &mut recorder,
                view,
                editor,
                Color::WHITE,
                false,
                card,
                |renderer, clip| {
                    assert_eq!(clip, view);
                    iced::advanced::Renderer::fill_quad(
                        renderer,
                        quad(view),
                        Background::Color(Color::BLACK),
                    );
                },
            );
            assert!(recorder.clips.is_empty());
            assert_eq!(recorder.quads[4].0.bounds, view);
            assert_eq!(
                recorder.quads[0].0.border.radius.top_left,
                if card { 10.0 } else { 0.0 }
            );
            assert_eq!(recorder.quads.len(), if card { 6 } else { 5 });
            if card {
                let border = recorder.quads.last().unwrap();
                assert_eq!(border.0.bounds, view);
                assert_eq!(border.0.border.radius.top_left, 6.0);
                assert_eq!(border.2, view);
            }
        }
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
