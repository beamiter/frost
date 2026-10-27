//! Shared cubic paths rasterized once by the native graphics library.
//! The retained terminal still decides safe placement, ownership and motion.
use iced::advanced::image::Handle;
use jterm_core::reference_organism::{Command, SHAPES};

pub(crate) fn handle() -> Handle {
    static BODY: once_cell::sync::Lazy<Handle> = once_cell::sync::Lazy::new(|| {
        const SCALE: f32 = 3.0;
        let mut pixmap = tiny_skia::Pixmap::new(192, 192).expect("native organism surface");
        for shape in SHAPES {
            let mut builder = tiny_skia::PathBuilder::new();
            for command in shape.path {
                match *command {
                    Command::Move(x, y) => builder.move_to(x, y),
                    Command::Line(x, y) => builder.line_to(x, y),
                    Command::Cubic(a, b, c, d, e, f) => builder.cubic_to(a, b, c, d, e, f),
                    Command::Close => builder.close(),
                }
            }
            let Some(path) = builder.finish() else {
                continue;
            };
            let transform = tiny_skia::Transform::from_scale(SCALE, SCALE);
            if let Some([r, g, b, a]) = shape.fill {
                let mut paint = tiny_skia::Paint::default();
                paint.set_color_rgba8(r, g, b, a);
                pixmap.fill_path(&path, &paint, tiny_skia::FillRule::Winding, transform, None);
            }
            if let Some([r, g, b, a]) = shape.stroke {
                let mut paint = tiny_skia::Paint::default();
                paint.set_color_rgba8(r, g, b, a);
                pixmap.stroke_path(
                    &path,
                    &paint,
                    &tiny_skia::Stroke {
                        width: shape.stroke_width,
                        line_cap: tiny_skia::LineCap::Round,
                        line_join: tiny_skia::LineJoin::Round,
                        ..Default::default()
                    },
                    transform,
                    None,
                );
            }
        }
        // The native renderer consumes straight-alpha RGBA; tiny-skia stores
        // premultiplied channels. Decode once to avoid dark glow fringes.
        let rgba: Vec<u8> = pixmap
            .pixels()
            .iter()
            .flat_map(|p| {
                let c = p.demultiply();
                [c.red(), c.green(), c.blue(), c.alpha()]
            })
            .collect();
        Handle::from_rgba(192, 192, rgba)
    });
    BODY.clone()
}
