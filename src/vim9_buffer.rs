//! Native Vim buffer text, independent of terminal cells and ANSI palettes.
//!
//! The established PTY input adapter is retained for mouse, focus and IME.
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
    Vim9PixelRect, Vim9RenderFrame, Vim9RenderMetrics, Vim9RenderRect, Vim9RenderRun,
    Vim9RenderStyle, Vim9ScrollAccumulator, Vim9ScrollAnimation,
};
use std::cell::RefCell;
use std::collections::{BTreeMap, VecDeque};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::terminal_view::{Metrics, TermWidget};

#[derive(Debug, Clone, Copy)]
pub(crate) struct NativeWheel {
    pub window: i64,
    /// Positive means down. Precision devices retain fractional lines.
    pub lines: f64,
}

struct Motion {
    previous: Arc<Vim9RenderFrame>,
    outgoing: VecDeque<Arc<Vim9RenderFrame>>,
    animation: Vim9ScrollAnimation,
    total_pixels: f32,
}

/// Scrolling belongs to a Vim window, never to the terminal's scrollback.
/// Animate only acknowledged viewport changes following a real wheel event.
/// Source edits, keyboard navigation, layout changes and fallback settle at once.
pub(crate) struct ScrollState {
    epoch: Instant,
    accumulator: Vim9ScrollAccumulator,
    requested: BTreeMap<i64, Instant>,
    motions: BTreeMap<i64, Motion>,
}

impl Default for ScrollState {
    fn default() -> Self {
        Self {
            epoch: Instant::now(),
            accumulator: Vim9ScrollAccumulator::default(),
            requested: BTreeMap::new(),
            motions: BTreeMap::new(),
        }
    }
}

impl ScrollState {
    pub(crate) fn clear(&mut self) {
        self.accumulator.clear();
        self.requested.clear();
        self.motions.clear();
    }

    pub(crate) fn wheel(&mut self, wheel: NativeWheel) -> i32 {
        let lines = self.accumulator.add(wheel.window, wheel.lines);
        if lines != 0 {
            self.requested.insert(wheel.window, Instant::now());
        }
        lines
    }

    fn seconds(&self, now: Instant) -> f64 {
        now.saturating_duration_since(self.epoch).as_secs_f64()
    }

    fn active(&self, now: Instant) -> bool {
        let seconds = self.seconds(now);
        self.motions
            .values()
            .any(|motion| motion.animation.is_active(seconds))
    }

    pub(crate) fn observe(
        &mut self,
        previous: Option<&Arc<Vim9RenderFrame>>,
        next: Option<&Arc<Vim9RenderFrame>>,
        line_height: f32,
        enabled: bool,
    ) {
        self.observe_at(previous, next, line_height, enabled, Instant::now());
    }

    fn observe_at(
        &mut self,
        previous: Option<&Arc<Vim9RenderFrame>>,
        next: Option<&Arc<Vim9RenderFrame>>,
        line_height: f32,
        enabled: bool,
        now: Instant,
    ) {
        if !enabled {
            self.motions.clear();
            return;
        }
        let Some((previous, next)) = previous.zip(next).filter(|(old, new)| {
            old.supported
                && new.supported
                && old.rows == new.rows
                && old.columns == new.columns
                && old.default_style == new.default_style
                && line_height.is_finite()
                && line_height > 0.0
        }) else {
            self.clear();
            return;
        };
        let seconds = self.seconds(now);
        self.requested
            .retain(|_, at| now.saturating_duration_since(*at) < Duration::from_millis(350));
        self.motions.retain(|id, motion| {
            motion.animation.is_active(seconds)
                && next.windows.iter().any(|window| window.window == *id)
        });
        for window in &next.windows {
            let Some(old) = previous.windows.iter().find(|old| {
                old.window == window.window
                    && old.buffer == window.buffer
                    && old.changedtick == window.changedtick
                    && old.view == window.view
                    && old.leftcol == window.leftcol
                    && old.textoff == window.textoff
            }) else {
                self.motions.remove(&window.window);
                continue;
            };
            let delta = i128::from(window.topline) - i128::from(old.topline);
            if delta == 0 {
                continue;
            }
            if !self.requested.contains_key(&window.window)
                || delta.unsigned_abs() >= window.view.rows as u128
            {
                self.motions.remove(&window.window);
                continue;
            }
            let motion = self.motions.entry(window.window).or_insert_with(|| Motion {
                previous: Arc::clone(previous),
                outgoing: VecDeque::new(),
                animation: Vim9ScrollAnimation::default(),
                total_pixels: 0.0,
            });
            motion
                .animation
                .retarget(delta as f64 * f64::from(line_height), seconds);
            motion.outgoing.push_back(Arc::clone(previous));
            // Retargeting can leave more than one wheel step in flight. Keep a
            // bounded set of outgoing viewports and choose the newest one that
            // covers the exposed edge. Keeping the gesture's first frame would
            // force a visible snap every time it travelled one viewport.
            while motion.outgoing.len() > 32 {
                motion.outgoing.pop_front();
            }
            let offset = motion.animation.offset(seconds);
            let height = window.view.rows as f64 * f64::from(line_height);
            if offset.abs() >= height {
                self.motions.remove(&window.window);
                continue;
            }
            let covering = motion
                .outgoing
                .iter()
                .enumerate()
                .rev()
                .find_map(|(index, frame)| {
                    let origin = frame
                        .windows
                        .iter()
                        .find(|origin| origin.window == window.window)?;
                    let total = (i128::from(window.topline) - i128::from(origin.topline)) as f64
                        * f64::from(line_height);
                    let covers = if offset >= 0.0 {
                        total >= offset && total <= height
                    } else {
                        total <= offset && total >= -height
                    };
                    covers.then(|| (index, Arc::clone(frame), total as f32))
                });
            if let Some((index, frame, total)) = covering {
                motion.previous = frame;
                motion.total_pixels = total;
                // Keep the selected frame and every newer candidate: a frame
                // that is too recent now can cover the edge on the next step.
                drop(motion.outgoing.drain(..index));
            } else {
                // A displacement larger than the available viewport cannot be
                // presented without inventing text; show Vim's current frame.
                self.motions.remove(&window.window);
            }
        }
    }
}

fn wheel_at(
    frame: &Vim9RenderFrame,
    geometry: Vim9RenderMetrics,
    position: Point,
    delta: mouse::ScrollDelta,
) -> Option<NativeWheel> {
    let (row, column) = geometry.cell_at(position.x, position.y)?;
    let window = frame
        .windows
        .iter()
        .find(|window| window.view.contains(row, column))?;
    let lines = match delta {
        mouse::ScrollDelta::Lines { y, .. } => -f64::from(y) * 3.0,
        mouse::ScrollDelta::Pixels { y, .. } => -f64::from(y) / f64::from(geometry.line_height),
    };
    (lines.is_finite() && lines != 0.0).then_some(NativeWheel {
        window: window.window,
        lines,
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
    window: i64,
    logical: &'a Vim9RenderRect,
    clip: Rectangle,
    runs: Vec<&'a Vim9RenderRun>,
}

/// Prepare each window once, preserving run order for background/text layering.
/// Window identity selects paint ownership; overlapping pixel clips never move
/// a run to another buffer. The same logical-pixel transform is used by the
/// shared semantic anchor API for future GUI overlays.
fn paint_views(
    frame: &Vim9RenderFrame,
    metrics: Vim9RenderMetrics,
    clip: Rectangle,
) -> Vec<PaintView<'_>> {
    let mut indices = std::collections::BTreeMap::new();
    let mut views = Vec::new();
    let windows: Vec<_> = if frame.windows.is_empty() {
        vec![(frame.window, &frame.view)]
    } else {
        frame.windows.iter().map(|w| (w.window, &w.view)).collect()
    };
    for (window, logical) in windows {
        if let Some(visible) = metrics
            .rect(logical)
            .map(rectangle)
            .and_then(|rect| rect.intersection(&clip))
            .filter(|rect| rect.area() > 0.0)
        {
            indices.insert(window, views.len());
            views.push(PaintView {
                window,
                logical,
                clip: visible,
                runs: Vec::new(),
            });
        }
    }
    for run in &frame.runs {
        let window = if frame.windows.is_empty() {
            frame.window
        } else {
            run.window
        };
        if let Some(index) = indices.get(&window) {
            let view = &mut views[*index];
            if run.cells > 0 && view.logical.contains(run.row, run.column) {
                view.runs.push(run);
            }
        }
    }
    views
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
    frame: &'a Vim9RenderFrame,
    metrics: Metrics,
    font: Font,
    focused: bool,
    blink_on: bool,
    scroll: Option<&'a RefCell<ScrollState>>,
    modifiers: keyboard::Modifiers,
    on_scroll: Option<Box<dyn Fn(NativeWheel) -> Message + 'a>>,
}

impl<'a, Message> NativeBuffer<'a, Message> {
    pub(crate) fn new(
        input: TermWidget<'a, Message>,
        frame: &'a Vim9RenderFrame,
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
        }
    }

    pub(crate) fn scroll(
        mut self,
        state: &'a RefCell<ScrollState>,
        modifiers: keyboard::Modifiers,
        on_scroll: impl Fn(NativeWheel) -> Message + 'a,
    ) -> Self {
        self.scroll = Some(state);
        self.modifiers = modifiers;
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
            // Never generate input against a moving visual row. Settle before
            // delegating button/selection/IME geometry to the real PTY adapter.
            if matches!(event, Event::Mouse(mouse::Event::ButtonPressed(_))) {
                state.borrow_mut().clear();
                shell.request_redraw();
            }
            if let Event::Window(iced::window::Event::RedrawRequested(_)) = event {
                if state.borrow().active(Instant::now()) {
                    shell.request_redraw();
                }
            }
        }
        if self.focused
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
                    if let Some(wheel) = wheel_at(
                        self.frame,
                        render_metrics(layout.bounds(), self.metrics),
                        position,
                        *delta,
                    ) {
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
        let m = self.metrics;
        let geometry = render_metrics(bounds, m);
        if !geometry.is_valid() {
            self.input
                .draw(tree, renderer, theme, style, layout, cursor, viewport);
            return;
        }
        let frame = self.frame;
        let views = paint_views(frame, geometry, clip);
        if views.is_empty() {
            self.input
                .draw(tree, renderer, theme, style, layout, cursor, viewport);
            return;
        }
        let rectangles: Vec<_> = views.iter().map(|view| view.clip).collect();
        for band in compatibility_bands(clip, &rectangles) {
            renderer.with_layer(band, |renderer| {
                self.input
                    .draw(tree, renderer, theme, style, layout, cursor, &band);
            });
        }
        let (_, normal_bg, _) = colors(&frame.default_style, &frame.default_style);
        let scroll = self.scroll.map(RefCell::borrow);
        let seconds = scroll.as_ref().map(|state| state.seconds(Instant::now()));
        for prepared in views {
            renderer.fill_quad(quad(prepared.clip), Background::Color(normal_bg));
            let motion = scroll
                .as_ref()
                .and_then(|state| state.motions.get(&prepared.window))
                .filter(|motion| motion.animation.is_active(seconds.unwrap_or(0.0)));
            let offset = motion.map_or(0.0, |motion| {
                motion.animation.offset(seconds.unwrap_or(0.0)) as f32
            });
            if let Some(motion) = motion {
                // The old viewport supplies the exposed edge while the new one
                // moves into place. Both stay clipped to this same Vim split.
                if let Some(previous) = paint_views(&motion.previous, geometry, prepared.clip)
                    .into_iter()
                    .find(|view| view.window == prepared.window)
                {
                    self.paint_native_view(
                        renderer,
                        &motion.previous,
                        &previous,
                        geometry,
                        offset - motion.total_pixels,
                        false,
                    );
                }
            }
            self.paint_native_view(renderer, frame, &prepared, geometry, offset, true);
        }
    }
}

impl<Message> NativeBuffer<'_, Message> {
    fn paint_native_view<Renderer>(
        &self,
        renderer: &mut Renderer,
        frame: &Vim9RenderFrame,
        prepared: &PaintView<'_>,
        geometry: Vim9RenderMetrics,
        offset: f32,
        draw_cursor: bool,
    ) where
        Renderer: text::Renderer<Font = Font>,
    {
        let m = self.metrics;
        let logical_view = prepared.logical;
        let geometry = Vim9RenderMetrics {
            origin_y: geometry.origin_y + offset,
            ..geometry
        };
        let Some(view) = geometry
            .rect(logical_view)
            .map(rectangle)
            .and_then(|moved| moved.intersection(&prepared.clip))
        else {
            return;
        };
        let (normal_fg, normal_bg, _) = colors(&frame.default_style, &frame.default_style);
        renderer.with_layer(view, |renderer| {
            renderer.fill_quad(quad(view), Background::Color(normal_bg));
            // Keep italic overhang intact across adjacent highlight runs.
            for run in &prepared.runs {
                let Some(area) = geometry
                    .cell_rect(run.row, run.column, run.cells)
                    .map(rectangle)
                else {
                    continue;
                };
                let (_, bg, _) = colors(&run.style, &frame.default_style);
                renderer.fill_quad(quad(area), Background::Color(bg));
            }
            for run in &prepared.runs {
                let Some(area) = geometry
                    .cell_rect(run.row, run.column, run.cells)
                    .map(rectangle)
                else {
                    continue;
                };
                let text_area = Rectangle {
                    x: area.x - m.font_size * 0.25,
                    width: area.width + m.font_size * 0.5,
                    ..area
                };
                let Some(run_clip) = text_area.intersection(&view) else {
                    continue;
                };
                let (fg, _, special) = colors(&run.style, &frame.default_style);
                let font = Font {
                    weight: if run.style.bold {
                        iced::font::Weight::Bold
                    } else {
                        iced::font::Weight::Normal
                    },
                    style: if run.style.italic {
                        iced::font::Style::Italic
                    } else {
                        iced::font::Style::Normal
                    },
                    ..self.font
                };
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
            if draw_cursor
                && frame.cursor.visible
                && logical_view.contains(frame.cursor.row, frame.cursor.column)
                && (!self.focused || self.blink_on)
            {
                let Some(cursor) = geometry.cursor_rect(&frame.cursor).map(rectangle) else {
                    return;
                };
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

    fn scrolled_frame(topline: u64) -> Arc<Vim9RenderFrame> {
        let mut frame = split_frame();
        frame.windows[0].topline = 1;
        frame.windows[1].topline = topline;
        frame.windows[1].botline = topline + 17;
        Arc::new(frame)
    }

    #[test]
    fn precision_wheel_uses_hovered_split_and_rejects_non_buffer_pixels() {
        let frame = split_frame();
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
    fn acknowledged_scroll_keeps_previous_edge_rows_and_retargets_continuously() {
        let mut state = ScrollState::default();
        let first = scrolled_frame(1);
        let second = scrolled_frame(4);
        let third = scrolled_frame(7);
        let now = state.epoch;
        state.requested.insert(20, now);
        state.observe_at(Some(&first), Some(&second), 20.0, true, now);
        assert_eq!(state.motions.len(), 1);
        let motion = &state.motions[&20];
        assert!(Arc::ptr_eq(&motion.previous, &first));
        assert_eq!(motion.total_pixels, 60.0);
        assert_eq!(motion.animation.offset(0.0), 60.0);
        assert!(!state.motions.contains_key(&10), "peer split must not move");
        state.observe_at(
            Some(&second),
            Some(&third),
            20.0,
            true,
            now + Duration::from_millis(50),
        );
        let motion = &state.motions[&20];
        assert!(
            Arc::ptr_eq(&motion.previous, &first),
            "retain the initial edge during rapid scrolling"
        );
        assert_eq!(motion.total_pixels, 120.0);
        assert!((motion.animation.offset(0.05) - 67.5).abs() < 1e-5);
        // The old and new viewport extents overlap throughout the transition;
        // no blank strip or stale terminal row may fill the exposed edge.
        for step in 0..=10 {
            let offset = motion.animation.offset(0.05 + f64::from(step) * 0.01) as f32;
            let old_start = offset - motion.total_pixels;
            let old_end = old_start + 340.0;
            let new_start = offset;
            let new_end = new_start + 340.0;
            assert!(old_start <= 0.0);
            assert!(new_end >= 340.0);
            assert!(old_end >= new_start);
        }
        assert!(!state.active(now + Duration::from_millis(160)));
    }

    #[test]
    fn sustained_scroll_rebases_edge_frames_without_periodic_snaps() {
        for direction in [1_i64, -1] {
            let mut state = ScrollState::default();
            let epoch = state.epoch;
            let mut previous = scrolled_frame(100);
            for step in 0..30_u64 {
                let now = epoch + Duration::from_millis(step * 16);
                let seconds = state.seconds(now);
                let before = state
                    .motions
                    .get(&20)
                    .map_or(0.0, |motion| motion.animation.offset(seconds));
                let topline = (100 + (step as i64 + 1) * direction * 3) as u64;
                let next = scrolled_frame(topline);
                state.requested.insert(20, now);
                state.observe_at(Some(&previous), Some(&next), 20.0, true, now);
                let motion = state
                    .motions
                    .get(&20)
                    .expect("continuous scrolling must not snap after one viewport");
                let offset = motion.animation.offset(seconds);
                assert!((offset - before - direction as f64 * 60.0).abs() < 1e-5);
                assert!(offset.abs() > 0.0 && offset.abs() < 340.0);
                assert!(motion.outgoing.len() <= 32);
                for tick in 0..=10 {
                    let offset = motion.animation.offset(seconds + f64::from(tick) * 0.01);
                    let old_start = offset - f64::from(motion.total_pixels);
                    let old_end = old_start + 340.0;
                    let new_start = offset;
                    let new_end = new_start + 340.0;
                    assert!(old_start.min(new_start) <= 0.0);
                    assert!(old_end.max(new_end) >= 340.0);
                    assert!(old_start.max(new_start) <= old_end.min(new_end));
                }
                previous = next;
            }
        }
    }

    #[test]
    fn non_wheel_navigation_edits_fallback_and_reduced_motion_settle_immediately() {
        let first = scrolled_frame(1);
        let second = scrolled_frame(4);
        let mut state = ScrollState::default();
        let now = state.epoch;
        state.observe_at(Some(&first), Some(&second), 20.0, true, now);
        assert!(
            state.motions.is_empty(),
            "keyboard navigation is never eased"
        );
        state.requested.insert(20, now);
        state.observe_at(Some(&first), Some(&second), 20.0, true, now);
        assert_eq!(state.motions.len(), 1);
        let mut edited = (*second).clone();
        edited.windows[1].changedtick += 1;
        state.observe_at(Some(&second), Some(&Arc::new(edited)), 20.0, true, now);
        assert!(state.motions.is_empty());
        state.observe_at(Some(&first), Some(&second), 20.0, true, now);
        assert_eq!(state.motions.len(), 1);
        state.observe_at(Some(&first), Some(&second), 20.0, false, now);
        assert!(state.motions.is_empty());
        state.observe_at(Some(&first), Some(&second), 20.0, true, now);
        state.observe_at(Some(&second), None, 20.0, true, now);
        assert!(state.motions.is_empty());
        assert!(state.requested.is_empty());
        state.requested.insert(20, now);
        state.observe_at(Some(&first), Some(&scrolled_frame(100)), 20.0, true, now);
        assert!(
            state.motions.is_empty(),
            "large jumps are displayed immediately"
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
        let views = paint_views(&frame, render_metrics(bounds, metrics), bounds);
        assert_eq!(views.len(), 2);
        assert_eq!(
            views[0]
                .runs
                .iter()
                .map(|r| r.text.as_str())
                .collect::<Vec<_>>(),
            vec!["界", "next"]
        );
        assert_eq!(
            views[1]
                .runs
                .iter()
                .map(|r| r.text.as_str())
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
            let views = paint_views(&frame, geometry, bounds);
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
