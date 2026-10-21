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
    mouse, Background, Color, Element, Event, Font, Length, Pixels, Point, Rectangle, Size,
};
use jterm_core::vim9::{
    Vim9PixelRect, Vim9RenderFrame, Vim9RenderMetrics, Vim9RenderRect, Vim9RenderRun,
    Vim9RenderStyle,
};

use crate::terminal_view::{Metrics, TermWidget};

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
        }
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
        let (normal_fg, normal_bg, _) = colors(&frame.default_style, &frame.default_style);
        for prepared in views {
            let logical_view = prepared.logical;
            let view = prepared.clip;
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
                if frame.cursor.visible
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
