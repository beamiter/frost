//! Shared vector paths painted by Iced, independent of glyph fallback fonts.
use iced::widget::canvas::{self, Canvas, Frame, Geometry, Path, Stroke};
use iced::{Color, Element, Point, Rectangle};
pub(crate) use jterm_core::reference_icons::Icon;

struct Glyph {
    icon: Icon,
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
        for points in self.icon.paths() {
            let path = Path::new(|builder| {
                if let Some((x, y)) = points.first() {
                    builder.move_to(Point::new(x * scale, y * scale));
                    for (x, y) in &points[1..] {
                        builder.line_to(Point::new(x * scale, y * scale));
                    }
                }
            });
            frame.stroke(
                &path,
                Stroke::default()
                    .with_color(self.color)
                    .with_width(1.5 * scale)
                    .with_line_cap(canvas::LineCap::Round)
                    .with_line_join(canvas::LineJoin::Round),
            );
        }
        vec![frame.into_geometry()]
    }
}
pub(crate) fn icon<'a, Message: 'a>(value: Icon, color: Color, size: f32) -> Element<'a, Message> {
    Canvas::new(Glyph { icon: value, color })
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
