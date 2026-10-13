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
        if ch.is_control() || matches!(ch, '/' | '\\') {
            continue;
        }
        let ch = if jterm_core::review_input::is_visual_spoofing_character(ch) {
            '\u{fffd}'
        } else {
            ch
        };
        if bounded.len().saturating_add(ch.len_utf8()) > MAX_CUSTOM_THEME_NAME_BYTES {
            break;
        }
        bounded.push(ch);
    }
    bounded
}

pub(crate) fn accepted_custom_theme_name(name: impl Into<String>) -> Option<String> {
    let name = bound_custom_theme_name(name);
    if name.contains('\u{fffd}')
        || name
            .chars()
            .any(jterm_core::review_input::is_visual_spoofing_character)
    {
        None
    } else {
        Some(name)
    }
}

/// Persist-time check: iced may show U+FFFD after ingest, but a replacement
/// character must not become a theme filename.
pub(crate) fn validate_saved_custom_theme_name(name: &str) -> Result<(), String> {
    if name.contains('\u{fffd}') {
        return Err(
            "Name cannot contain path separators, controls, or invisible formatting characters"
                .to_string(),
        );
    }
    Theme::validate_custom_theme_name(name)
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
    let shown =
        jterm_core::review_input::safe_inline_display(&text.into(), MAX_THEME_EDITOR_ERROR_BYTES);
    let shown: String = shown.chars().filter(|ch| *ch != '\u{fffd}').collect();
    if shown.trim().is_empty() {
        "Theme editor error".to_string()
    } else {
        shown
    }
}

/// Commit the selected-theme fallback only after deletion succeeds. Keeping
/// this result boundary pure lets failures be tested without deleting themes.
pub(crate) fn apply_custom_theme_deletion<E>(
    result: Result<(), E>,
    deleted: &str,
    selected: &mut String,
    dirty: &mut bool,
) -> Result<bool, E> {
    result?;
    if selected.as_str() == deleted {
        *selected = "dark".to_string();
        *dirty = true;
        Ok(true)
    } else {
        Ok(false)
    }
}

/// Conservative popup bounds for configured-font rows with 4px vertical
/// button padding, 2px row spacing and 8px panel padding. Overflow scrolls.
pub(crate) fn popup_menu_size(font_size: f32, rows: usize, available: iced::Size) -> iced::Size {
    let font_size = crate::config::Config::clamp_font_size(font_size);
    let width = (240.0 * (font_size / 14.0).max(1.0)).min(available.width.max(1.0));
    let line_height = font_size * 1.3;
    let height =
        (line_height + 18.0 + rows as f32 * (line_height + 10.0)).min(available.height.max(1.0));
    iced::Size::new(width, height)
}

/// Input fields in semantic popups use the same effective UI palette.
pub(crate) fn popup_input_style(
    theme: &Theme,
    status: iced::widget::text_input::Status,
) -> iced::widget::text_input::Style {
    use iced::widget::text_input::Status;
    let text = Theme::rgb_to_color32(theme.ui.text);
    let dim = Theme::rgb_to_color32(theme.ui.text_disabled);
    let accent = Theme::rgb_to_color32(theme.tabbar.active_border);
    iced::widget::text_input::Style {
        background: Theme::rgb_to_color32(theme.ui.panel_bg).into(),
        border: iced::Border {
            color: if matches!(status, Status::Focused { .. } | Status::Hovered) {
                accent
            } else {
                Theme::rgb_to_color32(theme.ui.border)
            },
            width: 1.0,
            radius: 4.0.into(),
        },
        icon: dim,
        placeholder: dim,
        value: if status == Status::Disabled {
            dim
        } else {
            text
        },
        selection: Color { a: 0.35, ..accent },
    }
}

/// Context menus stay readable independently of terminal transparency.
/// Use the UI palette so custom and light themes update open popups too.
pub(crate) fn popup_style(theme: &Theme) -> iced::widget::container::Style {
    iced::widget::container::Style {
        text_color: Some(Theme::rgb_to_color32(theme.ui.text)),
        background: Some(Theme::rgb_to_color32(theme.ui.panel_bg).into()),
        border: iced::Border {
            color: Theme::rgb_to_color32(theme.ui.border),
            width: 1.0,
            radius: 6.0.into(),
        },
        ..Default::default()
    }
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
        accepted_custom_theme_name, bound_custom_theme_name, bound_theme_editor_error,
        bound_theme_hex_draft, validate_saved_custom_theme_name, Theme, ThemeExt as _,
        MAX_CUSTOM_THEME_NAME_BYTES, MAX_THEME_EDITOR_ERROR_BYTES,
    };
    use iced::Color;

    #[test]
    fn popup_input_uses_ui_text_background_and_focus_colors() {
        use iced::widget::text_input::Status;
        let mut theme = Theme::builtin_light();
        theme.ui.panel_bg = [21, 43, 65];
        theme.ui.text = [211, 233, 255];
        theme.ui.text_disabled = [123, 145, 167];
        theme.ui.border = [54, 76, 98];
        theme.tabbar.active_border = [190, 130, 70];
        for status in [
            Status::Active,
            Status::Hovered,
            Status::Focused { is_hovered: false },
            Status::Focused { is_hovered: true },
            Status::Disabled,
        ] {
            let style = super::popup_input_style(&theme, status);
            assert_eq!(
                style.background,
                Theme::rgb_to_color32(theme.ui.panel_bg).into()
            );
            assert_eq!(
                style.placeholder,
                Theme::rgb_to_color32(theme.ui.text_disabled)
            );
            assert_eq!(
                style.value,
                Theme::rgb_to_color32(if status == Status::Disabled {
                    theme.ui.text_disabled
                } else {
                    theme.ui.text
                })
            );
            let focused = matches!(status, Status::Hovered | Status::Focused { .. });
            assert_eq!(
                style.border.color,
                Theme::rgb_to_color32(if focused {
                    theme.tabbar.active_border
                } else {
                    theme.ui.border
                })
            );
        }
    }

    #[test]
    fn popup_bounds_follow_font_size_and_scroll_inside_small_windows() {
        let available = iced::Size::new(800.0, 600.0);
        let normal = super::popup_menu_size(14.0, 8, available);
        let large = super::popup_menu_size(28.0, 8, available);
        assert!(large.width > normal.width);
        assert!(large.height > normal.height);
        assert!(large.height <= available.height);
        let small = iced::Size::new(180.0, 140.0);
        let bounded = super::popup_menu_size(72.0, 100, small);
        assert_eq!(bounded, small);
    }

    #[test]
    fn popup_palette_follows_live_dark_light_and_custom_ui_colors_opaquely() {
        let mut custom = Theme::builtin_light();
        custom.ui.panel_bg = [31, 57, 83];
        custom.ui.text = [213, 225, 237];
        custom.ui.border = [101, 123, 145];
        // Rebuilding against each effective theme models switching while a
        // popup is open; terminal colors must not substitute for UI colors.
        for theme in [Theme::default(), Theme::builtin_light(), custom] {
            let style = super::popup_style(&theme);
            assert_eq!(
                style.background,
                Some(Theme::rgb_to_color32(theme.ui.panel_bg).into())
            );
            assert_eq!(style.text_color, Some(Theme::rgb_to_color32(theme.ui.text)));
            assert_eq!(style.border.color, Theme::rgb_to_color32(theme.ui.border));
            assert_eq!(style.border.width, 1.0);
            assert_eq!(style.text_color.unwrap().a, 1.0);
            let Some(iced::Background::Color(background)) = style.background else {
                panic!("solid popup background");
            };
            assert_eq!(background.a, 1.0);
        }
    }

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
        let spoofed = bound_custom_theme_name("ok\u{202e}");
        assert_eq!(spoofed, "ok\u{fffd}");
        assert!(!spoofed.contains('\u{202e}'));
        assert!(accepted_custom_theme_name("ok\u{202e}").is_none());
        assert_eq!(accepted_custom_theme_name("ok").as_deref(), Some("ok"));
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
        assert!(validate_saved_custom_theme_name(&filled).is_ok());
        assert!(validate_saved_custom_theme_name("ok\u{fffd}").is_err());
        assert!(validate_saved_custom_theme_name("ok\u{202e}").is_err());
        let listed = bound_custom_theme_name(format!("dusk\u{1b}[31m\u{202e}{}", "n".repeat(400)));
        assert!(!listed.contains('\u{1b}'));
        assert!(!listed.contains('\u{202e}'));
        assert!(listed.contains('\u{fffd}'));
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
        assert!(!shown.contains('\u{fffd}'));
        assert!(shown.len() <= MAX_THEME_EDITOR_ERROR_BYTES);
        assert!(shown.starts_with("Save failed:"));
        assert_eq!(bound_theme_editor_error("\u{202e}"), "Theme editor error");
    }
}

#[cfg(test)]
mod deletion_tests {
    use super::apply_custom_theme_deletion;

    #[test]
    fn failed_deletion_preserves_selected_theme_and_dirty_state() {
        for initial_dirty in [false, true] {
            let mut selected = "custom".to_string();
            let mut dirty = initial_dirty;
            assert_eq!(
                apply_custom_theme_deletion(
                    Err("injected I/O failure"),
                    "custom",
                    &mut selected,
                    &mut dirty
                ),
                Err("injected I/O failure")
            );
            assert_eq!(selected, "custom");
            assert_eq!(dirty, initial_dirty);
        }
    }

    #[test]
    fn only_successfully_deleted_selected_theme_changes_preference() {
        let mut selected = "custom".to_string();
        let mut dirty = false;
        assert_eq!(
            apply_custom_theme_deletion(Ok::<(), ()>(()), "other", &mut selected, &mut dirty),
            Ok(false)
        );
        assert_eq!(selected, "custom");
        assert!(!dirty);
        assert_eq!(
            apply_custom_theme_deletion(Ok::<(), ()>(()), "custom", &mut selected, &mut dirty),
            Ok(true)
        );
        assert_eq!(selected, "dark");
        assert!(dirty);
    }

    #[test]
    fn production_delete_commits_only_after_result_admission() {
        let source = include_str!("main.rs");
        let handler = source
            .split("Message::ThemeDelete(name) => {")
            .nth(1)
            .unwrap()
            .split("Message::ConfigSave")
            .next()
            .unwrap();
        assert!(handler.contains("apply_custom_theme_deletion("));
        assert!(handler.contains("Ok(changed)"));
        assert!(!handler.contains("self.config.theme ="));
        assert!(!handler.contains("self.config_dirty ="));
    }
}
