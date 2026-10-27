//! Production pixel budget shared by the PTY model and retained native surface.
//! Lifecycle, companion expansion and draft height cannot change this budget.
use jterm_core::reference_shell::{self as shell, Rect};

pub(crate) const CONTENT_TOP: f32 = 16.0;
pub(crate) const TOOLS_HEIGHT: f32 = shell::CONTROL_HEIGHT + shell::COMPOSER_CONTROL_BOTTOM;
pub(crate) const INPUT_STRIP_HEIGHT: f32 = shell::COMPOSER_HEIGHT - TOOLS_HEIGHT;

/// The Chat outer surface includes the prompt/status strip. The native child
/// always receives this smaller height at its canonical origin, in every state.
pub(crate) fn native_body_height(outer_surface_height: f32) -> f32 {
    (outer_surface_height - INPUT_STRIP_HEIGHT).max(0.0)
}

/// The raw grid rectangle after actual visible chrome. It ends exactly above
/// the composer. There is deliberately no lifecycle, draft or companion input.
pub(crate) fn raw_rect(center: Rect, reading_bar: f32, recovery: f32) -> Rect {
    let body_top = center.y + shell::CANVAS_HEADER_HEIGHT + CONTENT_TOP;
    let outer_height = center.height
        - shell::CANVAS_HEADER_HEIGHT
        - CONTENT_TOP
        - TOOLS_HEIGHT
        - shell::COMPOSER_BOTTOM
        - recovery.max(0.0);
    let inset = shell::COMPOSER_INSET.min(center.width / 2.0);
    Rect {
        x: center.x + inset,
        y: body_top + reading_bar.max(0.0) + recovery.max(0.0),
        width: (center.width - 2.0 * inset).max(0.0),
        height: (native_body_height(outer_height) - reading_bar.max(0.0)).max(0.0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use jterm_core::chat_mode::ChatSurface;

    #[test]
    fn reference_raw_grid_ends_at_the_composer_without_masking_cells() {
        let layout = shell::Layout::for_size(1356.0, 704.0);
        let raw = raw_rect(layout.center, 0.0, 0.0);
        assert_eq!(
            (raw.x, raw.y, raw.width, raw.height),
            (320.0, 107.0, 702.0, 506.0)
        );
        assert_eq!(raw.y + raw.height, layout.composer.y);
        assert_eq!(INPUT_STRIP_HEIGHT + TOOLS_HEIGHT, 80.0);
    }

    #[test]
    fn prompt_foreground_tui_and_multiline_share_the_canonical_budget() {
        let layout = shell::Layout::for_size(1356.0, 704.0);
        let outer = layout.center.height
            - shell::CANVAS_HEADER_HEIGHT
            - CONTENT_TOP
            - TOOLS_HEIGHT
            - shell::COMPOSER_BOTTOM;
        for surface in [
            ChatSurface::Prompt,
            ChatSurface::Running,
            ChatSurface::Fullscreen,
            ChatSurface::Passthrough,
        ] {
            for draft_height in [44.0_f32, 100.0, 153.0, 180.0] {
                let raw = raw_rect(layout.center, 0.0, 0.0);
                assert_eq!(
                    native_body_height(outer),
                    raw.height,
                    "{surface:?}/{draft_height}"
                );
                assert_eq!(raw.height, 506.0, "draft growth must not resize the PTY");
            }
        }
        let block_body = layout.center.height
            - shell::CANVAS_HEADER_HEIGHT
            - CONTENT_TOP
            - shell::COMPOSER_HEIGHT
            - shell::COMPOSER_BOTTOM;
        assert_eq!(
            block_body,
            native_body_height(outer),
            "Prompt may not deduct 44px twice"
        );
    }

    #[test]
    fn real_header_and_recovery_heights_are_charged_once() {
        for (width, height) in [
            (620.0, 400.0),
            (800.0, 704.0),
            (1356.0, 704.0),
            (1600.0, 950.0),
        ] {
            let layout = shell::Layout::for_size(width, height);
            for bar in [0.0, 36.0] {
                for recovery in [0.0, 40.0] {
                    let raw = raw_rect(layout.center, bar, recovery);
                    assert_eq!(raw.y + raw.height, layout.composer.y);
                    assert!(raw.height >= 0.0);
                }
            }
        }
    }

    #[test]
    fn exhausted_geometry_is_nonnegative() {
        for height in [0.0, 1.0, 44.0, 80.0] {
            assert!(native_body_height(height) >= 0.0);
            assert_eq!(
                raw_rect(
                    Rect {
                        x: 0.0,
                        y: 0.0,
                        width: 20.0,
                        height
                    },
                    36.0,
                    40.0
                )
                .height,
                0.0
            );
        }
    }
}
