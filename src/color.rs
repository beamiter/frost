use crate::terminal::{Color, DynamicColorPalette};
use crate::theme::Theme;
use crate::theme::ThemeExt as _;
use iced::Color as IColor;

/// Map a named ANSI color to an index into the theme's 16-color palette.
/// Indexed, RGB, and default colors have no named slot and return `None`.
fn named_ansi_index(color: Color) -> Option<usize> {
    Some(match color {
        Color::Black => 0,
        Color::Red => 1,
        Color::Green => 2,
        Color::Yellow => 3,
        Color::Blue => 4,
        Color::Magenta => 5,
        Color::Cyan => 6,
        Color::White => 7,
        Color::BrightBlack => 8,
        Color::BrightRed => 9,
        Color::BrightGreen => 10,
        Color::BrightYellow => 11,
        Color::BrightBlue => 12,
        Color::BrightMagenta => 13,
        Color::BrightCyan => 14,
        Color::BrightWhite => 15,
        Color::Indexed(_) | Color::Rgb(_, _, _) | Color::Default => return None,
    })
}

fn apply_dim(base: IColor, dim: bool) -> IColor {
    if dim {
        IColor {
            r: base.r * 2.0 / 3.0,
            g: base.g * 2.0 / 3.0,
            b: base.b * 2.0 / 3.0,
            a: base.a,
        }
    } else {
        base
    }
}

fn named_ansi_iced(
    color: Color,
    theme: &Theme,
    palette: Option<&DynamicColorPalette>,
    bold: bool,
) -> IColor {
    let Some(idx) = named_ansi_index(color) else {
        return theme.terminal_foreground();
    };
    // VTE4: bold + standard color (0-7) promotes to bright variant (8-15)
    let idx = if bold && idx < 8 { idx + 8 } else { idx };
    palette
        .and_then(|p| p[idx])
        .map(|(r, g, b)| IColor::from_rgb8(r, g, b))
        .unwrap_or_else(|| theme.ansi_color(idx))
}

/// Resolve a foreground color using the theme palette, with VTE4-compatible
/// bold-brightening and dim attenuation.
#[cfg(test)]
fn resolve_fg(color: Color, theme: &Theme, bold: bool, dim: bool) -> IColor {
    resolve_fg_with_palette(color, theme, None, bold, dim)
}

pub fn resolve_fg_with_palette(
    color: Color,
    theme: &Theme,
    palette: Option<&DynamicColorPalette>,
    bold: bool,
    dim: bool,
) -> IColor {
    let base = match color {
        Color::Default => theme.terminal_foreground(),
        Color::Indexed(idx) => color_256(idx, theme, palette),
        Color::Rgb(r, g, b) => IColor::from_rgb8(r, g, b),
        named => named_ansi_iced(named, theme, palette, bold),
    };
    apply_dim(base, dim)
}

/// Resolve a background color using the theme palette.
#[cfg(test)]
fn resolve_bg(color: Color, theme: &Theme) -> IColor {
    resolve_bg_with_palette(color, theme, None)
}

pub fn resolve_bg_with_palette(
    color: Color,
    theme: &Theme,
    palette: Option<&DynamicColorPalette>,
) -> IColor {
    match color {
        Color::Default => theme.terminal_background(),
        Color::Indexed(idx) => color_256(idx, theme, palette),
        Color::Rgb(r, g, b) => IColor::from_rgb8(r, g, b),
        named => {
            let Some(idx) = named_ansi_index(named) else {
                return theme.terminal_background();
            };
            palette
                .and_then(|p| p[idx])
                .map(|(r, g, b)| IColor::from_rgb8(r, g, b))
                .unwrap_or_else(|| theme.ansi_color(idx))
        }
    }
}

/// 256-color palette resolution using theme colors for indices 0-15.
pub fn color_256(idx: u8, theme: &Theme, palette: Option<&DynamicColorPalette>) -> IColor {
    if let Some((r, g, b)) = palette.and_then(|p| p[idx as usize]) {
        return IColor::from_rgb8(r, g, b);
    }
    match idx {
        0..=15 => theme.ansi_color(idx as usize),
        16..=231 => {
            let idx = idx - 16;
            let r_idx = idx / 36;
            let g_idx = (idx % 36) / 6;
            let b_idx = idx % 6;
            let r = if r_idx == 0 { 0 } else { 55 + r_idx * 40 };
            let g = if g_idx == 0 { 0 } else { 55 + g_idx * 40 };
            let b = if b_idx == 0 { 0 } else { 55 + b_idx * 40 };
            IColor::from_rgb8(r, g, b)
        }
        232..=255 => {
            let gray = 8 + (idx - 232) * 10;
            IColor::from_rgb8(gray, gray, gray)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::Theme;

    fn theme() -> Theme {
        Theme::default()
    }

    #[test]
    fn named_ansi_colors_match_theme_slots() {
        let theme = theme();
        let named = [
            (Color::Black, 0),
            (Color::Red, 1),
            (Color::Green, 2),
            (Color::Yellow, 3),
            (Color::Blue, 4),
            (Color::Magenta, 5),
            (Color::Cyan, 6),
            (Color::White, 7),
            (Color::BrightBlack, 8),
            (Color::BrightRed, 9),
            (Color::BrightGreen, 10),
            (Color::BrightYellow, 11),
            (Color::BrightBlue, 12),
            (Color::BrightMagenta, 13),
            (Color::BrightCyan, 14),
            (Color::BrightWhite, 15),
        ];
        for (color, idx) in named {
            assert_eq!(named_ansi_index(color), Some(idx), "slot {idx}");
            assert_eq!(
                resolve_fg(color, &theme, false, false),
                theme.ansi_color(idx)
            );
            assert_eq!(resolve_bg(color, &theme), theme.ansi_color(idx));
        }
        assert_eq!(named_ansi_index(Color::Default), None);
        assert_eq!(named_ansi_index(Color::Indexed(7)), None);
        assert_eq!(named_ansi_index(Color::Rgb(1, 2, 3)), None);
    }

    #[test]
    fn bold_promotes_standard_ansi_to_bright() {
        let theme = theme();
        assert_eq!(
            resolve_fg(Color::Red, &theme, true, false),
            theme.ansi_color(9)
        );
        assert_eq!(
            resolve_fg(Color::BrightRed, &theme, true, false),
            theme.ansi_color(9)
        );
        assert_eq!(
            resolve_bg(Color::Red, &theme),
            theme.ansi_color(1),
            "background must not bold-brighten"
        );
    }

    #[test]
    fn dim_attenuates_resolved_foreground() {
        let theme = theme();
        let full = resolve_fg(Color::Default, &theme, false, false);
        let dim = resolve_fg(Color::Default, &theme, false, true);
        assert!((dim.r - full.r * 2.0 / 3.0).abs() < f32::EPSILON);
        assert!((dim.g - full.g * 2.0 / 3.0).abs() < f32::EPSILON);
        assert!((dim.b - full.b * 2.0 / 3.0).abs() < f32::EPSILON);
        assert_eq!(dim.a, full.a);
    }

    #[test]
    fn palette_override_beats_theme_and_cube() {
        let theme = theme();
        let mut palette: DynamicColorPalette = [None; 256];
        palette[1] = Some((9, 8, 7));
        palette[200] = Some((1, 2, 3));
        assert_eq!(
            resolve_fg_with_palette(Color::Red, &theme, Some(&palette), false, false),
            IColor::from_rgb8(9, 8, 7)
        );
        assert_eq!(
            color_256(200, &theme, Some(&palette)),
            IColor::from_rgb8(1, 2, 3)
        );
    }

    #[test]
    fn color_256_cube_and_gray_are_deterministic() {
        let theme = theme();
        assert_eq!(color_256(16, &theme, None), IColor::from_rgb8(0, 0, 0));
        assert_eq!(color_256(21, &theme, None), IColor::from_rgb8(0, 0, 255));
        assert_eq!(color_256(232, &theme, None), IColor::from_rgb8(8, 8, 8));
        assert_eq!(
            color_256(255, &theme, None),
            IColor::from_rgb8(238, 238, 238)
        );
        assert_eq!(
            resolve_fg(Color::Indexed(7), &theme, false, false),
            theme.ansi_color(7)
        );
        assert_eq!(
            resolve_fg(Color::Rgb(10, 20, 30), &theme, false, false),
            IColor::from_rgb8(10, 20, 30)
        );
        assert_eq!(
            resolve_bg(Color::Default, &theme),
            theme.terminal_background()
        );
        assert_eq!(
            resolve_fg(Color::Default, &theme, false, false),
            theme.terminal_foreground()
        );
        assert_eq!(
            resolve_fg(Color::Red, &theme, false, false),
            resolve_fg_with_palette(Color::Red, &theme, None, false, false)
        );
        assert_eq!(
            resolve_bg(Color::Blue, &theme),
            resolve_bg_with_palette(Color::Blue, &theme, None)
        );
    }
}
