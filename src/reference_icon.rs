//! Shared vector paths painted by Iced, independent of glyph fallback fonts.
use iced::widget::canvas::{self, Canvas, Frame, Geometry, Path, Stroke};
use iced::{Color, Element, Point, Rectangle};
pub(crate) use jterm_core::reference_icons::Icon;

type Strokes = &'static [&'static [(f32, f32)]];

/// Frost-only strokes for shell controls the shared icon set does not have.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum LocalIcon {
    Sparkle,
    ChevronRight,
    Sliders,
}
impl LocalIcon {
    const fn paths(self) -> Strokes {
        match self {
            Self::Sparkle => &[
                &[
                    (11., 3.),
                    (12.8, 9.2),
                    (19., 11.),
                    (12.8, 12.8),
                    (11., 19.),
                    (9.2, 12.8),
                    (3., 11.),
                    (9.2, 9.2),
                    (11., 3.),
                ],
                &[(18.5, 15.), (18.5, 21.)],
                &[(15.5, 18.), (21.5, 18.)],
            ],
            Self::ChevronRight => &[&[(9., 6.), (15., 12.), (9., 18.)]],
            Self::Sliders => &[
                &[(4., 8.), (20., 8.)],
                &[(4., 16.), (20., 16.)],
                &[(9., 5.), (9., 11.)],
                &[(15., 13.), (15., 19.)],
            ],
        }
    }
}

struct Glyph {
    paths: Strokes,
    color: Color,
}
impl<Message> canvas::Program<Message> for Glyph {
    type State = ();
    fn draw(
        &self,
        _: &(),
        renderer: &iced::Renderer,
        _: &iced::Theme,
        bounds: Rectangle,
        _: iced::mouse::Cursor,
    ) -> Vec<Geometry> {
        let mut frame = Frame::new(renderer, bounds.size());
        let scale = bounds.width.min(bounds.height) / 24.0;
        // Snap vertices to pixel centres so thin axis-aligned strokes (the
        // vertical bar of "+", box edges) stay crisp at small sizes.
        let snap = |value: f32| ((value * scale) - 0.5).round() + 0.5;
        for points in self.paths {
            let path = Path::new(|builder| {
                if let Some((x, y)) = points.first() {
                    builder.move_to(Point::new(snap(*x), snap(*y)));
                    for (x, y) in &points[1..] {
                        builder.line_to(Point::new(snap(*x), snap(*y)));
                    }
                }
            });
            frame.stroke(
                &path,
                Stroke::default()
                    .with_color(self.color)
                    .with_width((1.5 * scale).max(1.25))
                    .with_line_cap(canvas::LineCap::Round)
                    .with_line_join(canvas::LineJoin::Round),
            );
        }
        vec![frame.into_geometry()]
    }
}
pub(crate) fn icon<'a, Message: 'a>(value: Icon, color: Color, size: f32) -> Element<'a, Message> {
    Canvas::new(Glyph {
        paths: value.paths(),
        color,
    })
    .width(size)
    .height(size)
    .into()
}
pub(crate) fn local_icon<'a, Message: 'a>(
    value: LocalIcon,
    color: Color,
    size: f32,
) -> Element<'a, Message> {
    Canvas::new(Glyph {
        paths: value.paths(),
        color,
    })
    .width(size)
    .height(size)
    .into()
}

/// Native image handles are stable, so shipped PNG identities decode once.
pub(crate) fn identity<'a, Message: 'a>(product: &str, size: f32) -> Element<'a, Message> {
    static HANDLES: once_cell::sync::Lazy<[iced::widget::image::Handle; 4]> =
        once_cell::sync::Lazy::new(|| {
            ["anvil", "forge", "ember", "frost"].map(|name| {
                let rgba = image::load_from_memory(
                    jterm_core::reference_identity::png(name).expect("shared product identity"),
                )
                .expect("valid shipped product PNG")
                .into_rgba8();
                let (width, height) = rgba.dimensions();
                iced::widget::image::Handle::from_rgba(width, height, rgba.into_raw())
            })
        });
    let index = ["anvil", "forge", "ember", "frost"]
        .iter()
        .position(|name| product.eq_ignore_ascii_case(name))
        .unwrap_or(3);
    iced::widget::image(HANDLES[index].clone())
        .width(size)
        .height(size)
        .content_fit(iced::ContentFit::Contain)
        .into()
}
