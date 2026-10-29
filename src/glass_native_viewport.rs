//! Glass profile adapter. Model and native widgets consume one joint budget.
use crate::glass_layout::{Layout, NativeBudget, Rect};

pub(crate) const CONTENT_TOP: f32 = crate::glass_shell_tokens::TERMINAL_TOP_GAP;
pub(crate) const TOOLS_HEIGHT: f32 = crate::glass_shell_tokens::TOOLBAR_HEIGHT;
pub(crate) const INPUT_STRIP_HEIGHT: f32 = crate::glass_layout::INPUT_STRIP_HEIGHT;

pub(crate) fn raw_rect(
    layout: Layout,
    reading_bar: f32,
    recovery: f32,
    actual_input_height: Option<f32>,
) -> Rect {
    native_budget(layout, reading_bar, recovery, actual_input_height).raw
}

pub(crate) fn native_budget(
    layout: Layout,
    reading_bar: f32,
    recovery: f32,
    actual_input_height: Option<f32>,
) -> NativeBudget {
    layout.native_budget(reading_bar, recovery, actual_input_height)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn actual_native_and_editor_are_disjoint_for_every_requested_height() {
        for (w, h) in [(1612.0, 976.0), (1356.0, 704.0), (800.0, 640.0)] {
            for requested in [44.0, 83.2, 180.0, 500.0] {
                let layout = Layout::for_size(w, h);
                let budget = native_budget(layout, 0.0, 0.0, Some(requested));
                assert_eq!(raw_rect(layout, 0.0, 0.0, Some(requested)), budget.raw);
                let editor = budget.editor.unwrap();
                assert_eq!(budget.raw.bottom() + 13.0, editor.y);
                assert_eq!(editor.bottom(), layout.composer.y);
                assert_eq!(editor.height, budget.input_height);
                assert_eq!(budget.composer.bottom(), layout.composer.bottom());
            }
        }
    }

    #[test]
    fn native_direct_input_has_only_the_reference_toolbar() {
        let layout = Layout::for_size(1612.0, 976.0);
        let direct = native_budget(layout, 0.0, 0.0, None);
        assert!(direct.editor.is_none());
        assert_eq!(direct.input_height, 0.0);
        assert_eq!(direct.composer, layout.composer);
        assert_eq!(direct.raw.height, 648.0);
        assert_eq!(direct.raw.bottom() + 13.0, layout.composer.y);
    }
}
