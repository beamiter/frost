//! Real software pixels: partial shadow repaint must have the same result as
//! clearing and painting the entire scene. No window, display or GPU is used.
use super::*;
use crate::core::{Border, Font, Pixels, Point, Renderer as _, Shadow};
use crate::graphics::Viewport;
use crate::{Layer, Renderer};

const WIDTH: f32 = 64.0;
const HEIGHT: f32 = 48.0;

fn bounds() -> Rectangle {
    Rectangle::with_size(Size::new(WIDTH, HEIGHT))
}

fn background() -> Color {
    Color::from_rgba(0.03, 0.06, 0.11, 0.35)
}

fn quad() -> Quad {
    Quad {
        bounds: Rectangle::new(Point::new(18.0, 14.0), Size::new(28.0, 22.0)),
        border: Border {
            radius: 5.0.into(),
            ..Border::default()
        },
        shadow: Shadow {
            color: Color::from_rgba8(71, 115, 255, 0.16),
            offset: Vector::new(-9.0, -6.0),
            blur_radius: 10.0,
        },
        ..Quad::default()
    }
}

fn fill() -> Background {
    Background::Color(Color::from_rgba(0.09, 0.16, 0.28, 0.45))
}

fn scene(quads: &[Quad], clip: Rectangle) -> Renderer {
    let mut renderer = Renderer::new(Font::MONOSPACE, Pixels(14.0));
    renderer.reset(bounds());
    renderer.with_layer(clip, |renderer| {
        for quad in quads {
            renderer.fill_quad(*quad, fill());
        }
    });
    renderer
}

fn empty_pixels(scale: f32) -> Vec<u8> {
    vec![0; (WIDTH * scale) as usize * (HEIGHT * scale) as usize * 4]
}

fn draw(
    renderer: &mut Renderer,
    pixels: &mut [u8],
    scale: f32,
    damage: &[Rectangle],
) {
    let width = (WIDTH * scale) as u32;
    let height = (HEIGHT * scale) as u32;
    let viewport =
        Viewport::with_physical_size(Size::new(width, height), scale);
    let mut mask = tiny_skia::Mask::new(width, height).unwrap();
    renderer.draw(
        &mut tiny_skia::PixmapMut::from_bytes(pixels, width, height).unwrap(),
        &mut mask,
        &viewport,
        damage,
        background(),
    );
}

fn full(renderer: &mut Renderer, scale: f32) -> Vec<u8> {
    let mut pixels = empty_pixels(scale);
    draw(renderer, &mut pixels, scale, &[bounds()]);
    pixels
}

#[test]
fn shadow_damage_sparse_replay_never_accumulates_or_writes_outside_clip() {
    let clip = Rectangle::new(Point::new(8.0, 8.0), Size::new(40.0, 28.0));
    let damage = Rectangle::new(Point::new(24.0, 20.0), Size::new(8.0, 8.0));
    for scale in [1.0, 1.25, 1.5, 2.0] {
        let mut renderer = scene(&[quad()], clip);
        let expected = full(&mut renderer, scale);
        let plain = full(&mut scene(&[], bounds()), scale);
        let width = (WIDTH * scale) as usize;
        let height = (HEIGHT * scale) as usize;
        for y in 0..height {
            for x in 0..width {
                if x < (clip.x * scale) as usize
                    || x >= ((clip.x + clip.width) * scale) as usize
                    || y < (clip.y * scale) as usize
                    || y >= ((clip.y + clip.height) * scale) as usize
                {
                    let at = (y * width + x) * 4;
                    assert_eq!(
                        &expected[at..at + 4],
                        &plain[at..at + 4],
                        "shadow escaped its layer at DPR{scale}"
                    );
                }
            }
        }
        let mut partial = expected.clone();
        for _ in 0..40 {
            draw(&mut renderer, &mut partial, scale, &[damage]);
            assert_eq!(
                partial, expected,
                "unchanged scene accumulated outside sparse damage at DPR{scale}"
            );
        }
    }
}

#[test]
fn shadow_damage_repaints_a_region_intersecting_only_shadow() {
    let mut quad = quad();
    quad.bounds = Rectangle::new(Point::new(26.0, 14.0), Size::new(18.0, 20.0));
    quad.shadow.offset = Vector::new(-12.0, 0.0);
    let damage = Rectangle::new(Point::new(8.0, 18.0), Size::new(6.0, 10.0));
    assert!(!quad.bounds.intersects(&damage));
    for scale in [1.0, 1.5] {
        let mut renderer = scene(&[quad], bounds());
        let expected = full(&mut renderer, scale);
        let plain = full(&mut scene(&[], bounds()), scale);
        let mut actual = plain.clone();
        draw(&mut renderer, &mut actual, scale, &[damage]);
        let width = (WIDTH * scale) as usize;
        let height = (HEIGHT * scale) as usize;
        let mut positive_pixels = 0;
        for y in 0..height {
            for x in 0..width {
                let at = (y * width + x) * 4;
                let inside = x >= (damage.x * scale) as usize
                    && x < ((damage.x + damage.width) * scale) as usize
                    && y >= (damage.y * scale) as usize
                    && y < ((damage.y + damage.height) * scale) as usize;
                let reference = if inside { &expected } else { &plain };
                assert_eq!(
                    &actual[at..at + 4],
                    &reference[at..at + 4],
                    "shadow-only intersection or clip failed at DPR{scale}"
                );
                if inside && expected[at..at + 4] != plain[at..at + 4] {
                    positive_pixels += 1;
                }
            }
        }
        assert!(
            positive_pixels > 0,
            "the shadow-only fixture must paint real pixels"
        );
    }
}

#[test]
fn shadow_damage_move_and_remove_restore_all_painted_pixels() {
    let original = quad();
    let mut moved = original;
    moved.bounds.x += 6.0;
    moved.bounds.y += 3.0;
    moved.shadow.offset = Vector::new(4.0, 3.0);
    for current in [None, Some(moved)] {
        let previous_layer = Layer {
            bounds: bounds(),
            quads: vec![(original, fill())],
            ..Layer::default()
        };
        let current_quads: Vec<_> = current.into_iter().collect();
        let current_layer = Layer {
            bounds: bounds(),
            quads: current_quads
                .iter()
                .copied()
                .map(|quad| (quad, fill()))
                .collect(),
            ..Layer::default()
        };
        let damage = Layer::damage(&previous_layer, &current_layer);
        assert!(!damage.is_empty());
        assert!(
            damage.iter().all(|area| area.area() < bounds().area()),
            "shadow repair must retain bounded partial damage"
        );
        for scale in [1.0, 1.25, 1.5, 2.0] {
            let mut previous = scene(&[original], bounds());
            let mut actual = full(&mut previous, scale);
            let mut next = scene(&current_quads, bounds());
            draw(&mut next, &mut actual, scale, &damage);
            let expected = full(&mut next, scale);
            assert_eq!(
                actual, expected,
                "moving/removing a shadow left stale pixels at DPR{scale}"
            );
        }
    }
}

#[test]
fn shadow_damage_single_paint_keeps_original_alpha_and_color() {
    let mut quad = quad();
    quad.bounds = Rectangle::new(Point::new(24.0, 20.0), Size::new(24.0, 20.0));
    quad.shadow.offset = Vector::new(-10.0, 0.0);
    quad.shadow.blur_radius = 8.0;
    let mut pixels = empty_pixels(1.0);
    let mut mask = tiny_skia::Mask::new(WIDTH as u32, HEIGHT as u32).unwrap();
    adjust_clip_mask(&mut mask, bounds());
    Engine::new().draw_quad(
        &quad,
        &Background::Color(Color::TRANSPARENT),
        Transformation::IDENTITY,
        &mut tiny_skia::PixmapMut::from_bytes(
            &mut pixels,
            WIDTH as u32,
            HEIGHT as u32,
        )
        .unwrap(),
        &mut mask,
        bounds(),
    );
    let at = (30 * WIDTH as usize + 18) * 4;
    // BGRA bytes: shadow0.16 times interior falloff0.5 => alpha20/255.
    assert_eq!(&pixels[at..at + 4], &[20, 9, 6, 20]);
}
