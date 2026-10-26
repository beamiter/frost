use crate::core::{Color, Rectangle, Size};
use crate::graphics::compositor::{self, Information};
use crate::graphics::damage;
use crate::graphics::error::{self, Error};
use crate::graphics::{self, Shell, Viewport};
use crate::{Layer, Renderer, Settings};

use std::collections::VecDeque;
use std::num::NonZeroU32;

pub struct Compositor {
    context: softbuffer::Context<Box<dyn compositor::Display>>,
    settings: Settings,
}

pub struct Surface {
    window: softbuffer::Surface<Box<dyn compositor::Display>, Box<dyn compositor::Window>>,
    clip_mask: tiny_skia::Mask,
    layer_stack: VecDeque<Vec<Layer>>,
    background_color: Color,
    max_age: u8,
}

impl crate::graphics::Compositor for Compositor {
    type Renderer = Renderer;
    type Surface = Surface;

    async fn with_backend(
        settings: graphics::Settings,
        display: impl compositor::Display,
        _compatible_window: impl compositor::Window,
        _shell: Shell,
        backend: Option<&str>,
    ) -> Result<Self, Error> {
        match backend {
            None | Some("tiny-skia") | Some("tiny_skia") => Ok(new(settings.into(), display)),
            Some(backend) => Err(Error::GraphicsAdapterNotFound {
                backend: "tiny-skia",
                reason: error::Reason::DidNotMatch {
                    preferred_backend: backend.to_owned(),
                },
            }),
        }
    }

    fn create_renderer(&self) -> Self::Renderer {
        Renderer::new(self.settings.default_font, self.settings.default_text_size)
    }

    fn create_surface<W: compositor::Window + Clone>(
        &mut self,
        window: W,
        width: u32,
        height: u32,
    ) -> Self::Surface {
        let window = softbuffer::Surface::new(&self.context, Box::new(window.clone()) as _)
            .expect("Create softbuffer surface for window");

        let mut surface = Surface {
            window,
            clip_mask: tiny_skia::Mask::new(1, 1).expect("Create clip mask"),
            layer_stack: VecDeque::new(),
            background_color: Color::BLACK,
            max_age: 0,
        };

        if width > 0 && height > 0 {
            self.configure_surface(&mut surface, width, height);
        }

        surface
    }

    fn configure_surface(&mut self, surface: &mut Self::Surface, width: u32, height: u32) {
        surface
            .window
            .resize(
                NonZeroU32::new(width).expect("Non-zero width"),
                NonZeroU32::new(height).expect("Non-zero height"),
            )
            .expect("Resize surface");

        surface.clip_mask = tiny_skia::Mask::new(width, height).expect("Create clip mask");
        surface.layer_stack.clear();
    }

    fn information(&self) -> Information {
        Information {
            adapter: String::from("CPU"),
            backend: String::from("tiny-skia"),
        }
    }

    fn present(
        &mut self,
        renderer: &mut Self::Renderer,
        surface: &mut Self::Surface,
        viewport: &Viewport,
        background_color: Color,
        on_pre_present: impl FnOnce(),
    ) -> Result<(), compositor::SurfaceError> {
        present(
            renderer,
            surface,
            viewport,
            background_color,
            on_pre_present,
        )
    }

    fn screenshot(
        &mut self,
        renderer: &mut Self::Renderer,
        viewport: &Viewport,
        background_color: Color,
    ) -> Vec<u8> {
        screenshot(renderer, viewport, background_color)
    }
}

pub fn new(settings: Settings, display: impl compositor::Display) -> Compositor {
    #[allow(unsafe_code)]
    let context =
        softbuffer::Context::new(Box::new(display) as _).expect("Create softbuffer context");

    Compositor { context, settings }
}

pub fn present(
    renderer: &mut Renderer,
    surface: &mut Surface,
    viewport: &Viewport,
    background_color: Color,
    on_pre_present: impl FnOnce(),
) -> Result<(), compositor::SurfaceError> {
    let physical_size = viewport.physical_size();

    let mut buffer = surface
        .window
        .buffer_mut()
        .map_err(|_| compositor::SurfaceError::Lost)?;

    let last_layers = {
        let age = buffer.age();

        surface.max_age = surface.max_age.max(age);
        surface.layer_stack.truncate(surface.max_age as usize);

        if age > 0 {
            surface.layer_stack.get(age as usize - 1)
        } else {
            None
        }
    };

    let damage = last_layers
        .and_then(|last_layers| {
            (surface.background_color == background_color).then(|| {
                damage::diff(
                    last_layers,
                    renderer.layers(),
                    |layer| vec![layer.bounds],
                    Layer::damage,
                )
            })
        })
        .unwrap_or_else(|| vec![Rectangle::with_size(viewport.logical_size())]);

    if damage.is_empty() {
        if let Some(last_layers) = last_layers {
            surface.layer_stack.push_front(last_layers.clone());
        }
    } else {
        surface.layer_stack.push_front(renderer.layers().to_vec());
        surface.background_color = background_color;

        let damage = limit_damage_regions(damage::group(
            damage,
            Rectangle::with_size(viewport.logical_size()),
        ));

        let mut pixels = tiny_skia::PixmapMut::from_bytes(
            bytemuck::cast_slice_mut(&mut buffer),
            physical_size.width,
            physical_size.height,
        )
        .expect("Create pixel map");

        renderer.draw(
            &mut pixels,
            &mut surface.clip_mask,
            viewport,
            &damage,
            background_color,
        );
    }

    on_pre_present();
    buffer.present().map_err(|_| compositor::SurfaceError::Lost)
}

/// Every damage rectangle replays the intersecting layer stack. Fractional
/// transcript movement can fragment a small viewport into hundreds of glyph
/// regions, making partial repaint much slower than one complete canvas pass.
/// A union is a conservative repaint superset; compositing and clipping stay
/// identical. Ordinary sparse cursor/editor damage retains its small regions.
fn limit_damage_regions(damage: Vec<Rectangle>) -> Vec<Rectangle> {
    // A native partial pass costs several milliseconds even for a small strip:
    // keep the software frame budget bounded while retaining sparse carets.
    const MAX_REGIONS: usize = 4;
    if damage.len() <= MAX_REGIONS {
        return damage;
    }
    damage
        .into_iter()
        .reduce(|union, region| union.union(&region))
        .into_iter()
        .collect()
}

pub fn screenshot(
    renderer: &mut Renderer,
    viewport: &Viewport,
    background_color: Color,
) -> Vec<u8> {
    let size = viewport.physical_size();

    let mut offscreen_buffer: Vec<u32> = vec![0; size.width as usize * size.height as usize];

    let mut clip_mask = tiny_skia::Mask::new(size.width, size.height).expect("Create clip mask");

    renderer.draw(
        &mut tiny_skia::PixmapMut::from_bytes(
            bytemuck::cast_slice_mut(&mut offscreen_buffer),
            size.width,
            size.height,
        )
        .expect("Create offscreen pixel map"),
        &mut clip_mask,
        viewport,
        &[Rectangle::with_size(Size::new(
            size.width as f32,
            size.height as f32,
        ))],
        background_color,
    );

    offscreen_buffer.iter().fold(
        Vec::with_capacity(offscreen_buffer.len() * 4),
        |mut acc, pixel| {
            const A_MASK: u32 = 0xFF_00_00_00;
            const R_MASK: u32 = 0x00_FF_00_00;
            const G_MASK: u32 = 0x00_00_FF_00;
            const B_MASK: u32 = 0x00_00_00_FF;

            let a = ((A_MASK & pixel) >> 24) as u8;
            let r = ((R_MASK & pixel) >> 16) as u8;
            let g = ((G_MASK & pixel) >> 8) as u8;
            let b = (B_MASK & pixel) as u8;

            acc.extend([r, g, b, a]);
            acc
        },
    )
}

#[cfg(test)]
mod damage_tests {
    use super::*;
    use crate::core::renderer::Quad;
    use crate::core::{Background, Font, Pixels, Point, Renderer as _};

    #[test]
    fn excessive_damage_keeps_every_pixel_inside_a_bounded_union() {
        let bounds = Rectangle::with_size(Size::new(10_000.0, 10_000.0));
        let input: Vec<_> = (0..5)
            .map(|index| Rectangle {
                x: index as f32 * 400.0,
                y: index as f32 * 400.0,
                width: 10.0,
                height: 10.0,
            })
            .collect();
        let grouped = damage::group(input.clone(), bounds);
        assert!(
            grouped.len() > 4,
            "fixture must reproduce fragmented scrolling damage"
        );
        let output = limit_damage_regions(grouped);
        assert_eq!(output.len(), 1);
        assert!(output[0].is_within(&bounds));
        assert!(input.iter().all(|region| region.is_within(&output[0])));
    }

    #[test]
    fn sparse_cursor_damage_is_unchanged() {
        let regions = vec![
            Rectangle::new(Point::new(2.0, 3.0), Size::new(1.0, 16.0)),
            Rectangle::new(Point::new(900.0, 600.0), Size::new(2.0, 16.0)),
        ];
        assert_eq!(limit_damage_regions(regions.clone()), regions);
        assert!(limit_damage_regions(Vec::new()).is_empty());
    }

    #[test]
    fn merged_damage_is_pixel_identical_with_layer_clipping_and_alpha() {
        let size = Size::new(64, 64);
        let bounds = Rectangle::with_size(Size::new(64.0, 64.0));
        let viewport = Viewport::with_physical_size(size, 1.0);
        let background = Color::from_rgb(0.08, 0.12, 0.20);
        let regions: Vec<_> = (0..25)
            .map(|index| Rectangle {
                x: 2.0 + (index % 5) as f32 * 11.0,
                y: 3.0 + (index / 5) as f32 * 11.0,
                width: 8.0,
                height: 8.0,
            })
            .collect();
        let mut renderer = Renderer::new(Font::MONOSPACE, Pixels(14.0));
        renderer.reset(bounds);
        let mut base = vec![0; 64 * 64 * 4];
        let mut mask = tiny_skia::Mask::new(64, 64).unwrap();
        renderer.draw(
            &mut tiny_skia::PixmapMut::from_bytes(&mut base, 64, 64).unwrap(),
            &mut mask,
            &viewport,
            &[bounds],
            background,
        );
        // Two independent clips represent a scrolling history and fixed dock.
        for clip in [
            Rectangle {
                height: 32.0,
                ..bounds
            },
            Rectangle {
                y: 32.0,
                height: 32.0,
                ..bounds
            },
        ] {
            renderer.with_layer(clip, |renderer| {
                for region in &regions {
                    renderer.fill_quad(
                        Quad {
                            bounds: *region,
                            border: crate::core::Border {
                                radius: 3.0.into(),
                                ..crate::core::Border::default()
                            },
                            ..Quad::default()
                        },
                        Background::Color(Color::from_rgba(0.85, 0.2, 0.7, 0.55)),
                    );
                }
            });
        }
        let mut fragmented = base.clone();
        renderer.draw(
            &mut tiny_skia::PixmapMut::from_bytes(&mut fragmented, 64, 64).unwrap(),
            &mut mask,
            &viewport,
            &regions,
            background,
        );
        let mut merged = base.clone();
        renderer.draw(
            &mut tiny_skia::PixmapMut::from_bytes(&mut merged, 64, 64).unwrap(),
            &mut mask,
            &viewport,
            &limit_damage_regions(regions),
            background,
        );
        assert_eq!(fragmented, merged);
        let mut complete = base;
        renderer.draw(
            &mut tiny_skia::PixmapMut::from_bytes(&mut complete, 64, 64).unwrap(),
            &mut mask,
            &viewport,
            &[bounds],
            background,
        );
        assert_eq!(
            merged, complete,
            "conservative partial repaint must equal a full repaint"
        );
    }
}
