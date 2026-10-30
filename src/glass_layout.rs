//! Measured Ember/Frost glass geometry. One budget feeds native layout and PTY.
//! This is a Frost-local profile; the original four-app core layout is intact.
use crate::glass_shell_tokens as token;

pub const INPUT_STRIP_HEIGHT: f32 = 44.0;
pub const MAX_INPUT_HEIGHT: f32 = 180.0;
pub const MIN_CENTER_WIDTH: f32 = 300.0;
pub const MIN_SIDEBAR_WIDTH: f32 = 180.0;
pub const MAX_SIDEBAR_WIDTH: f32 = 480.0;
pub const MIN_INSPECTOR_WIDTH: f32 = 260.0;
pub const MAX_INSPECTOR_WIDTH: f32 = 520.0;

pub fn sidebar_width(value: f32) -> Option<f32> {
    value
        .is_finite()
        .then(|| value.clamp(MIN_SIDEBAR_WIDTH, MAX_SIDEBAR_WIDTH))
}

pub fn inspector_width(value: f32) -> Option<f32> {
    value
        .is_finite()
        .then(|| value.clamp(MIN_INSPECTOR_WIDTH, MAX_INSPECTOR_WIDTH))
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

impl Rect {
    #[cfg(test)]
    pub fn right(self) -> f32 {
        self.x + self.width
    }
    pub fn bottom(self) -> f32 {
        self.y + self.height
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Layout {
    pub titlebar: Rect,
    pub rail: Rect,
    pub navigation: Option<Rect>,
    pub center: Rect,
    pub header: Rect,
    pub inspector: Option<Rect>,
    /// Fixed tools row; the real Chat editor has a separate native budget.
    pub composer: Rect,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct NativeBudget {
    pub raw: Rect,
    pub editor: Option<Rect>,
    pub input_height: f32,
    pub composer: Rect,
}

impl Layout {
    #[cfg(test)]
    pub fn for_size(width: f32, height: f32) -> Self {
        Self::with_panels(width, height, None, None, true, true)
    }

    /// Requested widths are preferences, never overwritten by a temporary
    /// narrow allocation or a hidden panel. Both panels yield space before
    /// consuming the protected terminal width.
    pub fn with_panels(
        width: f32,
        height: f32,
        navigation: Option<f32>,
        inspector: Option<f32>,
        navigation_open: bool,
        inspector_open: bool,
    ) -> Self {
        let width = if width.is_finite() {
            width.max(0.0)
        } else {
            1612.0
        };
        let height = if height.is_finite() {
            height.max(0.0)
        } else {
            976.0
        };
        let short = height < token::SHORT_HEIGHT;
        let bottom = if short {
            token::SHORT_BOTTOM_GAP
        } else {
            token::MAIN_BOTTOM_GAP
        };
        let navigation_bottom = if short {
            token::SHORT_BOTTOM_GAP
        } else {
            token::NAVIGATION_BOTTOM_GAP
        };
        let (rail_width, navigation_width) = (token::RAIL_WIDTH, token::NAVIGATION_WIDTH);
        let outer_left = token::OUTER_LEFT.min(width);
        let outer_right = token::OUTER_RIGHT.min((width - outer_left).max(0.0));
        let rail_width = rail_width.min((width - outer_left - outer_right).max(0.0));
        let rail_right = outer_left + rail_width;
        let available = (width - rail_right - token::LEFT_GAP - outer_right).max(0.0);
        let navigation_visible =
            navigation_open && available >= MIN_CENTER_WIDTH + MIN_SIDEBAR_WIDTH;
        let inspector_visible = inspector_open
            && width >= token::INSPECTOR_DRAWER_BREAKPOINT
            && available
                >= MIN_CENTER_WIDTH
                    + token::INSPECTOR_GAP
                    + MIN_INSPECTOR_WIDTH
                    + if navigation_visible {
                        MIN_SIDEBAR_WIDTH
                    } else {
                        0.0
                    };
        let navigation_min = if navigation_visible {
            MIN_SIDEBAR_WIDTH
        } else {
            0.0
        };
        let inspector_min = if inspector_visible {
            MIN_INSPECTOR_WIDTH
        } else {
            0.0
        };
        // Keep the navigation request stable; the inspector uses the remaining
        // width above its own minimum. Changing either request therefore never
        // silently rewrites the other preference or scales both drag distances.
        let inspector_gap = if inspector_visible {
            token::INSPECTOR_GAP
        } else {
            0.0
        };
        let navigation_width = if navigation_visible {
            navigation
                .and_then(sidebar_width)
                .unwrap_or(navigation_width)
                .min(
                    (available - MIN_CENTER_WIDTH - inspector_min - inspector_gap)
                        .max(navigation_min),
                )
        } else {
            0.0
        };
        let inspector_width = if inspector_visible {
            inspector
                .and_then(inspector_width)
                .unwrap_or(token::INSPECTOR_WIDTH)
                .min(
                    (available - MIN_CENTER_WIDTH - navigation_width - inspector_gap)
                        .max(inspector_min),
                )
        } else {
            0.0
        };
        let center_x = (rail_right + navigation_width + token::LEFT_GAP).min(width - outer_right);
        let center_right = width
            - outer_right
            - if inspector_width > 0.0 {
                inspector_width + token::INSPECTOR_GAP
            } else {
                0.0
            };
        let title_height = token::TOPBAR_HEIGHT.min(height);
        let main_top = token::MAIN_TOP.min(height);
        let center_height = (height - main_top - bottom).max(0.0);
        let center = Rect {
            x: center_x,
            y: main_top,
            width: (center_right - center_x).max(0.0),
            height: center_height,
        };
        let header = Rect {
            height: (if short {
                token::SHORT_HEADER_HEIGHT
            } else {
                token::HEADER_HEIGHT
            })
            .min(center_height),
            ..center
        };
        let mut layout = Self {
            titlebar: Rect {
                x: 0.0,
                y: 0.0,
                width,
                height: title_height,
            },
            rail: Rect {
                x: outer_left,
                y: title_height,
                width: rail_width,
                height: (height - title_height - navigation_bottom).max(0.0),
            },
            navigation: (navigation_width > 0.0).then_some(Rect {
                x: rail_right,
                y: title_height,
                width: navigation_width,
                height: (height - title_height - navigation_bottom).max(0.0),
            }),
            center,
            header,
            inspector: (inspector_width > 0.0).then_some(Rect {
                x: width - outer_right - inspector_width,
                y: main_top,
                width: inspector_width,
                height: center_height,
            }),
            composer: Rect::default(),
        };
        layout.update_center_geometry();
        layout
    }

    /// Larger UI text can grow its own rows without changing terminal metrics.
    /// Apply this before both painting and native PTY/input budget allocation.
    pub fn with_chrome_font_size(mut self, font_size: f32) -> Self {
        if self.center.height == 0.0 {
            return self;
        }
        let control_height = chrome_control_height(font_size);
        let topbar_height = token::TOPBAR_HEIGHT.max(control_height + 8.0);
        let viewport_height = if self.titlebar.height < token::TOPBAR_HEIGHT {
            self.titlebar.height
        } else {
            self.rail.bottom() + token::NAVIGATION_BOTTOM_GAP
        };
        let main_top =
            (topbar_height + token::MAIN_TOP - token::TOPBAR_HEIGHT).min(viewport_height);
        let delta = (main_top - self.center.y).max(0.0);
        self.titlebar.height = topbar_height.min(viewport_height);
        for panel in [&mut self.rail, &mut self.center] {
            panel.y += delta;
            panel.height = (panel.height - delta).max(0.0);
        }
        // Rail/navigation start directly beneath the titlebar, before the 4px
        // workspace inset used by the terminal and inspector.
        self.rail.y = self.titlebar.height;
        if let Some(nav) = &mut self.navigation {
            nav.y = self.rail.y;
            nav.height = self.rail.height;
        }
        if let Some(inspector) = &mut self.inspector {
            inspector.y = self.center.y;
            inspector.height = self.center.height;
        }
        self.header.y = self.center.y;
        self.header.height = token::HEADER_HEIGHT
            .max(control_height + 4.0)
            .min(self.center.height);
        self.update_center_geometry();
        let tools_height = token::TOOLBAR_HEIGHT
            .max(control_height + 10.0)
            .min((self.center.height - self.header.height).max(0.0));
        self.composer.y = self.center.bottom() - tools_height;
        self.composer.height = tools_height;
        self
    }

    fn update_center_geometry(&mut self) {
        self.header.x = self.center.x;
        self.header.width = self.center.width;
        let height = token::TOOLBAR_HEIGHT.min((self.center.height - self.header.height).max(0.0));
        let left = token::TOOLBAR_INSET_LEFT.min(self.center.width);
        let right = token::TOOLBAR_INSET_RIGHT.min((self.center.width - left).max(0.0));
        self.composer = Rect {
            x: self.center.x + left,
            y: self.center.bottom() - height,
            width: (self.center.width - left - right).max(0.0),
            height,
        };
    }

    #[cfg(test)]
    pub fn without_inspector(mut self) -> Self {
        if self.inspector.take().is_some() {
            self.center.width = (self.titlebar.width - token::OUTER_RIGHT - self.center.x).max(0.0);
            self.update_center_geometry();
        }
        self
    }

    #[cfg(test)]
    pub fn without_navigation(mut self) -> Self {
        if let Some(nav) = self.navigation.take() {
            self.center.x -= nav.width;
            self.center.width += nav.width;
            self.update_center_geometry();
        }
        self
    }

    /// The existing native editor is attached above the fixed tools row only
    /// in Chat. Direct Block input stays inside the real terminal pane.
    fn raw_rect(self, reading_bar: f32, recovery: f32, chat: bool) -> Rect {
        if chat {
            return self
                .native_budget(reading_bar, recovery, Some(INPUT_STRIP_HEIGHT))
                .raw;
        }
        let left = token::TERMINAL_INSET_LEFT.min(self.center.width);
        let right = token::TERMINAL_INSET_RIGHT.min((self.center.width - left).max(0.0));
        let y = self.center.y
            + self.header.height
            + token::TERMINAL_TOP_GAP
            + recovery.max(0.0)
            + reading_bar.max(0.0);
        let bottom = self.composer.y - token::TERMINAL_FOOTER_GAP;
        let y = y.min(bottom.max(self.center.y));
        Rect {
            x: self.center.x + left,
            y,
            width: (self.center.width - left - right).max(0.0),
            height: (bottom - y).max(0.0),
        }
    }

    #[cfg(test)]
    fn editable_composer(self, input_height: f32) -> Rect {
        self.native_budget(0.0, 0.0, Some(input_height)).composer
    }

    pub fn input_cap(self, reading_bar: f32, recovery: f32) -> f32 {
        let top = self.raw_rect(reading_bar, recovery, false).y;
        let available = (self.composer.y - token::TERMINAL_FOOTER_GAP - top).max(0.0);
        (available * 0.45)
            .clamp(INPUT_STRIP_HEIGHT, MAX_INPUT_HEIGHT)
            .min(available)
    }

    /// Joint entry consumed by native paint and PTY sizing together.
    pub fn native_budget(
        self,
        reading_bar: f32,
        recovery: f32,
        input_height: Option<f32>,
    ) -> NativeBudget {
        let mut raw = self.raw_rect(reading_bar, recovery, false);
        let actual_height = input_height.and_then(|height| {
            let height = (if height.is_finite() {
                height.max(INPUT_STRIP_HEIGHT)
            } else {
                INPUT_STRIP_HEIGHT
            })
            .min(self.input_cap(reading_bar, recovery));
            (height > 0.0).then_some(height)
        });
        let editor = actual_height.map(|height| Rect {
            x: raw.x,
            y: self.composer.y - height,
            width: raw.width,
            height,
        });
        if let Some(editor) = editor {
            raw.height = (editor.y - token::TERMINAL_FOOTER_GAP - raw.y).max(0.0);
        }
        let composer = editor.map_or(self.composer, |editor| Rect {
            y: editor.y,
            height: editor.height + self.composer.height,
            ..self.composer
        });
        NativeBudget {
            raw,
            editor,
            input_height: editor.map_or(0.0, |rect| rect.height),
            composer,
        }
    }
}

/// 16.5px is the largest compact control label at the default 14px UI size.
pub fn chrome_control_height(font_size: f32) -> f32 {
    let scale = if font_size.is_finite() {
        (font_size / 14.0).clamp(1.0, 72.0 / 14.0)
    } else {
        1.0
    };
    (16.5 * scale * 1.3 + 8.0)
        .ceil()
        .max(token::TOOL_BUTTON_HEIGHT)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn larger_ui_text_grows_only_chrome_rows_and_keeps_native_input_disjoint() {
        for height in [400.0, 640.0, 976.0] {
            let compact = Layout::for_size(1356.0, height);
            assert_eq!(compact.with_chrome_font_size(14.0), compact);
            for font in [8.0, 14.0, 24.0, 32.0, 40.0, 72.0] {
                let layout = compact.with_chrome_font_size(font);
                let control = chrome_control_height(font);
                assert!(layout.titlebar.height >= control + 8.0);
                assert!(layout.header.height >= control + 4.0);
                assert!(layout.composer.height >= control + 10.0);
                assert_eq!(layout.center.y - layout.titlebar.height, 4.0);
                assert_eq!(layout.center.width, compact.center.width);
                for input in [44.0, 180.0] {
                    let native = layout.native_budget(0.0, 0.0, Some(input));
                    if let Some(editor) = native.editor {
                        assert_eq!(native.raw.bottom() + token::TERMINAL_FOOTER_GAP, editor.y);
                        assert_eq!(editor.bottom(), layout.composer.y);
                    } else {
                        assert_eq!(native.input_height, 0.0);
                    }
                    assert!(native.raw.height >= 0.0);
                    if height >= 640.0 {
                        assert!(native.raw.height > 0.0);
                    }
                }
            }
        }
    }

    #[test]
    fn custom_panel_widths_keep_one_native_grid_budget_and_restore_after_hide_or_narrowing() {
        let requested = (Some(420.0), Some(500.0));
        let wide = Layout::with_panels(1800.0, 976.0, requested.0, requested.1, true, true);
        assert_eq!(wide.navigation.unwrap().width, 420.0);
        assert_eq!(wide.inspector.unwrap().width, 500.0);
        for (width, left, right) in [
            (1180.0, true, true),
            (900.0, true, true),
            (1800.0, false, true),
            (1800.0, true, false),
        ] {
            let current = Layout::with_panels(width, 976.0, requested.0, requested.1, left, right);
            assert!(current.center.width >= MIN_CENTER_WIDTH);
            if let Some(inspector) = current.inspector {
                assert_eq!(current.center.right() + token::INSPECTOR_GAP, inspector.x);
            }
            let native = current.native_budget(0.0, 0.0, Some(44.0));
            assert_eq!(native.raw.x, current.center.x + token::TERMINAL_INSET_LEFT);
            assert_eq!(
                native.raw.width,
                current.center.width - token::TERMINAL_INSET_LEFT - token::TERMINAL_INSET_RIGHT
            );
            assert_eq!(
                Layout::with_panels(1800.0, 976.0, requested.0, requested.1, true, true),
                wide
            );
        }
        let bounded = Layout::with_panels(1180.0, 704.0, Some(480.0), Some(520.0), true, true);
        assert!((bounded.center.width - MIN_CENTER_WIDTH).abs() < 0.01);
        assert!(bounded.navigation.unwrap().width >= MIN_SIDEBAR_WIDTH);
        assert!(bounded.inspector.unwrap().width >= MIN_INSPECTOR_WIDTH);
    }

    #[test]
    fn floating_gap_remains_between_history_and_the_actual_editor() {
        for (w, h) in [(1612.0, 976.0), (1356.0, 704.0), (800.0, 640.0)] {
            let layout = Layout::for_size(w, h);
            for height in [44.0, 83.2, 180.0] {
                let budget = layout.native_budget(0.0, 0.0, Some(height));
                let editor = budget.editor.unwrap();
                assert_eq!(budget.raw.bottom() + token::TERMINAL_FOOTER_GAP, editor.y);
                assert_eq!(budget.raw.x, editor.x);
                assert_eq!(budget.raw.width, editor.width);
            }
        }
    }

    #[test]
    fn compact_rectangles_reserve_the_native_terminal_before_decoration() {
        let p = Layout::for_size(1612.0, 976.0);
        assert_eq!(
            p.rail,
            Rect {
                x: 4.0,
                y: 40.0,
                width: 44.0,
                height: 930.0
            }
        );
        assert_eq!(
            p.navigation,
            Some(Rect {
                x: 48.0,
                y: 40.0,
                width: 216.0,
                height: 930.0
            })
        );
        assert_eq!(
            p.center,
            Rect {
                x: 270.0,
                y: 44.0,
                width: 1044.0,
                height: 926.0
            }
        );
        assert_eq!(
            p.inspector,
            Some(Rect {
                x: 1320.0,
                y: 44.0,
                width: 288.0,
                height: 926.0
            })
        );
        assert_eq!(
            p.composer,
            Rect {
                x: 270.0,
                y: 928.0,
                width: 1044.0,
                height: 42.0
            }
        );
        assert_eq!(
            p.raw_rect(0.0, 0.0, false),
            Rect {
                x: 278.0,
                y: 86.0,
                width: 1028.0,
                height: 836.0
            }
        );
    }

    #[test]
    fn inspector_close_reclaims_the_panel_and_its_gap() {
        for width in [1612.0, 1356.0, 1180.0] {
            let shown = Layout::for_size(width, 976.0);
            let hidden = shown.without_inspector();
            assert!(hidden.inspector.is_none());
            assert_eq!(hidden.titlebar, shown.titlebar);
            assert_eq!(hidden.rail, shown.rail);
            assert_eq!(hidden.navigation, shown.navigation);
            assert_eq!(hidden.center.width - shown.center.width, 294.0);
            assert_eq!(hidden.header.width, hidden.center.width);
            assert_eq!(hidden.composer.right(), width - token::OUTER_RIGHT);
        }
    }

    #[test]
    fn narrow_drawer_does_not_reserve_an_invisible_panel() {
        for width in [1179.0, 960.0, 800.0, 700.0] {
            let layout = Layout::for_size(width, 640.0);
            assert!(layout.inspector.is_none());
            assert_eq!(layout, layout.without_inspector());
            assert_eq!(layout.center.right(), width - token::OUTER_RIGHT);
            assert!(layout.center.width >= 300.0);
            assert_eq!(layout.header.height, 36.0);
            assert_eq!(layout.composer.bottom(), 634.0);
        }
    }

    #[test]
    fn real_chat_editor_extends_the_toolbar_without_a_second_owner() {
        let layout = Layout::for_size(1612.0, 976.0);
        assert_eq!(
            layout.raw_rect(0.0, 0.0, true).bottom()
                + token::TERMINAL_FOOTER_GAP
                + INPUT_STRIP_HEIGHT,
            layout.composer.y
        );
        for requested in [44.0, 80.0, 180.0, 500.0] {
            let outer = layout.editable_composer(requested);
            assert_eq!(outer.bottom(), layout.composer.bottom());
            assert_eq!(outer.width, layout.composer.width);
            assert!(outer.height <= token::TOOLBAR_HEIGHT + MAX_INPUT_HEIGHT);
        }
        assert_eq!(layout.editable_composer(44.0).height, 86.0);
        assert_eq!(layout.editable_composer(180.0).height, 222.0);
    }

    #[test]
    fn recovery_and_reading_strip_consume_the_same_native_budget() {
        for (width, height) in [(1612.0, 976.0), (1356.0, 704.0), (800.0, 640.0)] {
            let layout = Layout::for_size(width, height);
            for chat in [false, true] {
                let base = layout.raw_rect(0.0, 0.0, chat);
                let clipped = layout.raw_rect(36.0, 28.0, chat);
                assert_eq!(clipped.y - base.y, 64.0);
                assert_eq!(base.height - clipped.height, 64.0);
                assert_eq!(base.bottom(), clipped.bottom());
                assert!(clipped.height > 0.0);
            }
        }
    }

    #[test]
    fn visibility_and_extreme_sizes_never_overlap_panels_or_produce_nan() {
        for width in [0.0, 80.0, 320.0, 600.0, 800.0, 1612.0, 2400.0, f32::NAN] {
            for height in [0.0, 100.0, 480.0, 640.0, 976.0, f32::INFINITY] {
                let layout = Layout::for_size(width, height);
                for rect in [
                    layout.titlebar,
                    layout.rail,
                    layout.center,
                    layout.header,
                    layout.composer,
                ] {
                    assert!(
                        rect.x.is_finite()
                            && rect.y.is_finite()
                            && rect.width >= 0.0
                            && rect.height >= 0.0
                    );
                }
                if let Some(inspector) = layout.inspector {
                    assert_eq!(layout.center.right() + token::INSPECTOR_GAP, inspector.x);
                }
                let hidden = layout.without_navigation();
                assert_eq!(hidden.header.width, hidden.center.width);
                assert!(hidden.navigation.is_none());
            }
        }
    }

    #[test]
    fn actual_editor_height_and_native_grid_share_one_nonoverlapping_budget() {
        for (width, height) in [(1612.0, 976.0), (1356.0, 704.0), (800.0, 640.0)] {
            let layout = Layout::for_size(width, height);
            for requested in [44.0, 4.0 * 16.8 + 16.0, 180.0, 500.0] {
                let budget = layout.native_budget(0.0, 0.0, Some(requested));
                let editor = budget.editor.expect("real Chat editor");
                assert!(budget.raw.bottom() <= editor.y,
                    "PTY pixels overlap real editor: size={width}/{height}, requested={requested}, {budget:?}");
                assert_eq!(budget.raw.bottom() + token::TERMINAL_FOOTER_GAP, editor.y);
                assert_eq!(editor.bottom(), layout.composer.y);
                assert_eq!(budget.input_height, editor.height);
                assert_eq!(budget.composer.y, editor.y);
                assert_eq!(budget.composer.bottom(), layout.composer.bottom());
                assert!(budget.raw.height > 0.0);
                assert!(budget.input_height <= MAX_INPUT_HEIGHT);
                let visible = layout.composer.y - token::TERMINAL_FOOTER_GAP - budget.raw.y;
                assert!(
                    budget.raw.height + 0.001 >= visible * 0.55,
                    "short-window editor consumed the protected native viewport"
                );
            }
        }
    }
}
