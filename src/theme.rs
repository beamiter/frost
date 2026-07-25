//! Theme data (structs, builtins, custom-theme persistence) lives in
//! `jterm_core::theme`. This module re-exports it and adds the iced color
//! conversions as an extension trait.

use iced::Color;

pub use jterm_core::theme::*;

/// iced color views over the shared RGB theme data.
pub trait ThemeExt {
    fn rgb_to_color32(rgb: [u8; 3]) -> Color;
    #[allow(dead_code)]
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
