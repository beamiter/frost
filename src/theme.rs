//! Theme data (structs, builtins, custom-theme persistence) lives in
//! `jterm_core::theme`. This module re-exports it and adds the iced color
//! conversions as an extension trait.

use iced::Color;

pub use jterm_core::theme::*;

/// Matches core's custom-theme filename envelope so the iced editor cannot
/// hold more than `Theme::validate_custom_theme_name` will accept.
pub(crate) const MAX_CUSTOM_THEME_NAME_BYTES: usize = 160;

pub(crate) fn bound_custom_theme_name(name: impl Into<String>) -> String {
    let mut bounded = String::new();
    for ch in name.into().chars() {
        if ch.is_control()
            || matches!(ch, '/' | '\\')
            || jterm_core::review_input::is_visual_spoofing_character(ch)
        {
            continue;
        }
        if bounded.len().saturating_add(ch.len_utf8()) > MAX_CUSTOM_THEME_NAME_BYTES {
            break;
        }
        bounded.push(ch);
    }
    bounded
}

/// Live theme-editor hex field: optional `#` plus six digits, matching
/// `Theme::hex_to_rgb` so a paste cannot sit unbounded next to a swatch.
pub(crate) const MAX_THEME_HEX_DIGITS: usize = 6;

pub(crate) fn bound_theme_hex_draft(hex: impl Into<String>) -> String {
    let mut bounded = String::new();
    let mut digits = 0usize;
    for ch in hex.into().chars() {
        if ch.is_control() {
            continue;
        }
        if ch == '#' && bounded.is_empty() {
            bounded.push('#');
            continue;
        }
        if ch.is_ascii_hexdigit() && digits < MAX_THEME_HEX_DIGITS {
            bounded.push(ch);
            digits += 1;
        }
    }
    bounded
}

/// Theme-editor error line: IO and validation messages are drawn as danger
/// chrome, so they cannot carry ESC/bidi or grow without bound.
pub(crate) const MAX_THEME_EDITOR_ERROR_BYTES: usize = 256;

pub(crate) fn bound_theme_editor_error(text: impl Into<String>) -> String {
    jterm_core::review_input::safe_inline_display(&text.into(), MAX_THEME_EDITOR_ERROR_BYTES)
}

/// iced color views over the shared RGB theme data.
pub trait ThemeExt {
    fn rgb_to_color32(rgb: [u8; 3]) -> Color;
    #[cfg_attr(not(test), allow(dead_code))]
    fn rgba_to_color32(rgba: [u8; 4]) -> Color;
    fn terminal_foreground(&self) -> Color;
    fn terminal_background(&self) -> Color;
    fn cursor_color(&self) -> Color;
    fn selection_color(&self) -> Color;
    fn selection_fg_color(&self) -> Color;
    fn search_match_color(&self) -> Color;
    fn search_current_color(&self) -> Color;
    fn ansi_color(&self, index: usize) -> Color;
}

impl ThemeExt for Theme {
    /// 将 RGB 数组转换为 iced::Color
    fn rgb_to_color32(rgb: [u8; 3]) -> Color {
        Color::from_rgb8(rgb[0], rgb[1], rgb[2])
    }

    /// 将 RGBA 数组转换为 iced::Color
    fn rgba_to_color32(rgba: [u8; 4]) -> Color {
        Color::from_rgba8(rgba[0], rgba[1], rgba[2], rgba[3] as f32 / 255.0)
    }

    /// 获取终端前景色
    fn terminal_foreground(&self) -> Color {
        Self::rgb_to_color32(self.terminal.foreground)
    }

    /// 获取终端背景色
    fn terminal_background(&self) -> Color {
        Self::rgb_to_color32(self.terminal.background)
    }

    /// 获取光标颜色
    fn cursor_color(&self) -> Color {
        Self::rgb_to_color32(self.terminal.cursor)
    }

    /// 获取选择背景色 - 基于前景色计算，确保与任意主题的高对比度
    fn selection_color(&self) -> Color {
        let fg = self.terminal.foreground;
        Color::from_rgba8(fg[0], fg[1], fg[2], 90.0 / 255.0)
    }

    /// 获取选中文本的前景色 - 使用背景色确保与选择背景的对比度
    fn selection_fg_color(&self) -> Color {
        Self::rgb_to_color32(self.terminal.background)
    }

    /// 搜索匹配项高亮色（半透明黄色叠加）
    fn search_match_color(&self) -> Color {
        Color::from_rgba(0.85, 0.75, 0.20, 0.40)
    }

    /// 当前搜索匹配项高亮色（更强的橙色叠加）
    fn search_current_color(&self) -> Color {
        Color::from_rgba(1.0, 0.55, 0.10, 0.70)
    }

    /// 获取 ANSI 颜色
    fn ansi_color(&self, index: usize) -> Color {
        if index < 16 {
            Self::rgb_to_color32(self.terminal.ansi_colors[index])
        } else {
            Self::rgb_to_color32(self.terminal.foreground)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        bound_custom_theme_name, bound_theme_editor_error, bound_theme_hex_draft, Theme,
        ThemeExt as _, MAX_CUSTOM_THEME_NAME_BYTES, MAX_THEME_EDITOR_ERROR_BYTES,
    };
    use iced::Color;

    #[test]
    fn rgba_converts_with_alpha_scale() {
        let color = Theme::rgba_to_color32([10, 20, 30, 128]);
        assert_eq!(color.r, Color::from_rgba8(10, 20, 30, 128.0 / 255.0).r);
        assert_eq!(color.g, Color::from_rgba8(10, 20, 30, 128.0 / 255.0).g);
        assert_eq!(color.b, Color::from_rgba8(10, 20, 30, 128.0 / 255.0).b);
        assert!((color.a - 128.0 / 255.0).abs() < f32::EPSILON);
    }

    #[test]
    fn ansi_color_out_of_range_falls_back_to_foreground() {
        let theme = Theme::default();
        assert_eq!(
            theme.ansi_color(0),
            Theme::rgb_to_color32(theme.terminal.ansi_colors[0])
        );
        assert_eq!(
            theme.ansi_color(15),
            Theme::rgb_to_color32(theme.terminal.ansi_colors[15])
        );
        assert_eq!(theme.ansi_color(16), theme.terminal_foreground());
        assert_eq!(theme.ansi_color(usize::MAX), theme.terminal_foreground());
    }

    #[test]
    fn custom_theme_name_draft_drops_path_syntax_and_stays_inside_the_filename_envelope() {
        assert_eq!(bound_custom_theme_name("dusk\n\u{1b}/night\\"), "dusknight");
        assert_eq!(bound_custom_theme_name("ok\u{202e}"), "ok");
        let filled =
            bound_custom_theme_name(format!("{}y", "x".repeat(MAX_CUSTOM_THEME_NAME_BYTES)));
        assert_eq!(filled.len(), MAX_CUSTOM_THEME_NAME_BYTES);
        assert!(!filled.contains('y'));
        let overflow =
            bound_custom_theme_name(format!("{}z", "界".repeat(MAX_CUSTOM_THEME_NAME_BYTES)));
        assert!(overflow.len() <= MAX_CUSTOM_THEME_NAME_BYTES);
        assert!(overflow.is_char_boundary(overflow.len()));
        assert!(!overflow.contains('z'));
        assert!(Theme::validate_custom_theme_name(&filled).is_ok());
        let listed = bound_custom_theme_name(format!("dusk\u{1b}[31m\u{202e}{}", "n".repeat(400)));
        assert!(!listed.contains('\u{1b}'));
        assert!(!listed.contains('\u{202e}'));
        assert!(listed.len() <= MAX_CUSTOM_THEME_NAME_BYTES);
        assert!(listed.starts_with("dusk"));
    }

    #[test]
    fn theme_hex_draft_keeps_an_optional_hash_and_six_digits() {
        assert_eq!(bound_theme_hex_draft("#aA\n\u{1b}bbcczz"), "#aAbbcc");
        assert_eq!(bound_theme_hex_draft("1122334455"), "112233");
        assert_eq!(bound_theme_hex_draft("##ff00aa"), "#ff00aa");
        assert_eq!(
            Theme::hex_to_rgb(&bound_theme_hex_draft("#00ff00")),
            Some([0, 255, 0])
        );
    }

    #[test]
    fn theme_editor_error_replaces_controls_and_stays_bounded() {
        let shown = bound_theme_editor_error(format!(
            "Save failed: \u{1b}[31m\u{202e}{}",
            "x".repeat(400)
        ));
        assert!(!shown.contains('\u{1b}'));
        assert!(!shown.contains('\u{202e}'));
        assert!(shown.contains('\u{fffd}'));
        assert!(shown.len() <= MAX_THEME_EDITOR_ERROR_BYTES);
        assert!(shown.starts_with("Save failed:"));
    }
}
