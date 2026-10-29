//! Natural Inspector content budget. The widget uses these same row heights;
//! short windows scroll the full content instead of squeezing the output pane.
use crate::glass_profile as shell;
use crate::ui_typography;

pub(crate) const CONTENT_GAP: f32 = 6.0;
pub(crate) const CARD_TOP: f32 = 17.0;
pub(crate) const CARD_SIDE: f32 = 17.0;
pub(crate) const OUTER_BOTTOM: f32 = 14.0;

#[derive(Clone, Copy, Debug)]
pub(crate) struct Metrics {
    pub header: f32,
    pub badge: f32,
    pub subtitle: f32,
    pub progress: f32,
    pub metadata_row: f32,
    pub current_label: f32,
    pub current: f32,
    pub queue_label: f32,
    pub queue_row: f32,
    pub output_label: f32,
    pub output: f32,
    pub minimum: f32,
}

fn line(role: f32, scale: f32) -> f32 {
    (role * scale * 1.3).ceil()
}

pub(crate) fn queue_row_height(scale: f32) -> f32 {
    if scale <= 1.0 {
        shell::INSPECTOR_QUEUE_ROW_HEIGHT
    } else {
        ui_typography::row_height(10.0 * scale, shell::INSPECTOR_QUEUE_ROW_HEIGHT, 10.0)
    }
}

impl Metrics {
    pub fn new(scale: f32, metadata_count: usize, queue_count: usize) -> Self {
        let badge = if scale <= 1.0 {
            shell::INSPECTOR_BADGE_HEIGHT
        } else {
            ui_typography::row_height(
                shell::INSPECTOR_BADGE_FONT_SIZE * scale,
                shell::INSPECTOR_BADGE_HEIGHT,
                shell::INSPECTOR_BADGE_VERTICAL_PADDING * 2.0,
            )
        };
        let metadata_row = if scale <= 1.0 {
            shell::INSPECTOR_METADATA_ROW_HEIGHT
        } else {
            ui_typography::row_height(10.0 * scale, shell::INSPECTOR_METADATA_ROW_HEIGHT, 4.0)
        };
        let mut metrics = Self {
            header: line(12.0, scale).max(badge).max(16.0),
            badge,
            subtitle: line(10.0, scale),
            progress: 12.0 + 5.0 + 8.0,
            metadata_row,
            current_label: line(9.0, scale).max(20.0),
            current: if scale <= 1.0 {
                shell::INSPECTOR_CURRENT_HEIGHT
            } else {
                ui_typography::row_height(19.0 * scale, shell::INSPECTOR_CURRENT_HEIGHT, 20.0)
            },
            queue_label: line(8.0, scale),
            queue_row: queue_row_height(scale),
            output_label: line(9.0, scale),
            output: shell::INSPECTOR_OUTPUT_HEIGHT,
            minimum: 0.0,
        };
        // Eleven column children include the elastic spacer. Its natural
        // height is zero; only actual remaining viewport space can expand it.
        metrics.minimum = CARD_TOP
            + shell::INSPECTOR_OUTPUT_BOTTOM
            + metrics
                .section_heights(metadata_count, queue_count)
                .iter()
                .sum::<f32>()
            + 10.0 * CONTENT_GAP;
        metrics
    }

    fn section_heights(self, metadata_count: usize, queue_count: usize) -> [f32; 11] {
        [
            self.header,
            self.subtitle,
            self.progress,
            metadata_count as f32 * self.metadata_row,
            self.current_label,
            self.current,
            self.queue_label,
            queue_count.max(1) as f32 * self.queue_row
                + queue_count.saturating_sub(1) as f32 * shell::INSPECTOR_QUEUE_GAP,
            0.0, // the elastic spacer has no invented minimum height
            self.output_label,
            self.output,
        ]
    }

    pub fn card_height(self, viewport: f32) -> f32 {
        self.minimum.max((viewport - OUTER_BOTTOM).max(0.0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Real Iced Column/Container layout, with no drawing or font services.
    /// Release iced_core intentionally omits its debug-only `()` renderer.
    struct LayoutRenderer;
    impl iced::advanced::Renderer for LayoutRenderer {
        fn start_layer(&mut self, _: iced::Rectangle) {
            unreachable!("layout-only test");
        }
        fn end_layer(&mut self) {
            unreachable!("layout-only test");
        }
        fn start_transformation(&mut self, _: iced::Transformation) {
            unreachable!("layout-only test");
        }
        fn end_transformation(&mut self) {
            unreachable!("layout-only test");
        }
        fn fill_quad(&mut self, _: iced::advanced::renderer::Quad, _: impl Into<iced::Background>) {
            unreachable!("layout-only test");
        }
        fn reset(&mut self, _: iced::Rectangle) {
            unreachable!("layout-only test");
        }
        fn allocate_image(
            &mut self,
            _: &iced::advanced::image::Handle,
            _: impl FnOnce(Result<iced::advanced::image::Allocation, iced::advanced::image::Error>)
                + Send
                + 'static,
        ) {
            unreachable!("layout-only test");
        }
    }

    #[test]
    fn actual_reference_rows_fit_without_inventing_a_620px_base() {
        let metrics = Metrics::new(1.4, 5, 5);
        assert_eq!(metrics.minimum, 740.0);
        assert_eq!(metrics.queue_row, 32.0);
        assert_eq!(metrics.output, 170.0);
        assert_eq!(metrics.card_height(788.0), 774.0);
        assert!(metrics.minimum <= 788.0 - OUTER_BOTTOM);
    }

    #[test]
    fn inspector_widget_uses_the_same_declared_metrics_and_padding() {
        let source = include_str!("reference_conversation.rs");
        let inspector = source
            .split_once("pub(crate) fn inspector_view(")
            .unwrap()
            .1
            .split_once("/// Production boundary:")
            .unwrap()
            .0;
        for field in [
            "header",
            "badge",
            "subtitle",
            "progress",
            "metadata_row",
            "current_label",
            "current",
            "queue_label",
            "queue_row",
            "output_label",
            "output",
        ] {
            assert_eq!(
                inspector
                    .matches(&format!(".height(metrics.{field})"))
                    .count(),
                1,
                "actual Inspector row must consume its declared metric: {field}"
            );
        }
        assert!(inspector.contains(".height(metrics.card_height(viewport_height))"));
        for token in ["CONTENT_GAP", "CARD_TOP", "CARD_SIDE", "OUTER_BOTTOM"] {
            assert!(inspector.contains(&format!("crate::inspector_layout::{token}")));
        }
        assert!(!inspector.contains("620.0"));
        assert!(inspector.contains("scrollable("));
    }

    #[test]
    fn short_windows_and_long_data_retain_full_rows_and_output() {
        for scale in [0.8, 1.0, 1.4, 2.4, 3.2] {
            for metadata in [0, 5, 20] {
                for queue in [0, 1, 5, 30] {
                    let metrics = Metrics::new(scale, metadata, queue);
                    for viewport in [120.0, 350.0, 490.0, 788.0] {
                        assert!(metrics.card_height(viewport) >= metrics.minimum);
                        assert_eq!(metrics.output, 170.0);
                    }
                }
            }
        }
        assert!(Metrics::new(1.4, 5, 5).minimum > 490.0 - OUTER_BOTTOM);
        assert!(Metrics::new(1.4, 20, 30).minimum > 788.0 - OUTER_BOTTOM);
    }

    #[test]
    fn iced_column_keeps_all_sections_and_the_full_output_in_its_real_limits() {
        use iced::advanced::{layout, widget::Tree};
        use iced::widget::{container, Column, Space};
        use iced::{Element, Length, Padding, Size};

        for (viewport, metadata, queue, scale) in [
            (788.0, 5, 5, 1.4),
            (490.0, 5, 5, 1.4),
            (788.0, 5, 0, 1.4),
            (350.0, 20, 30, 1.4),
            (490.0, 5, 5, 3.2),
        ] {
            let metrics = Metrics::new(scale, metadata, queue);
            let sections = metrics.section_heights(metadata, queue);
            let mut content: Column<'_, (), iced::Theme, LayoutRenderer> =
                Column::new().spacing(CONTENT_GAP);
            for (index, height) in sections.into_iter().enumerate() {
                content = content.push(Space::new().height(if index == 8 {
                    Length::Fill
                } else {
                    Length::Fixed(height)
                }));
            }
            let mut card: Element<'_, (), iced::Theme, LayoutRenderer> = container(content)
                .padding(Padding {
                    top: CARD_TOP,
                    right: CARD_SIDE,
                    bottom: shell::INSPECTOR_OUTPUT_BOTTOM,
                    left: CARD_SIDE,
                })
                .height(metrics.card_height(viewport))
                .into();
            let mut tree = Tree::new(card.as_widget());
            // A real Scrollable allows its child the full natural content
            // height. Its finite viewport is compared separately below.
            let node = card.as_widget_mut().layout(
                &mut tree,
                &LayoutRenderer,
                &layout::Limits::new(Size::ZERO, Size::new(320.0, f32::INFINITY)),
            );
            let rows = node.children()[0].children();
            assert_eq!(rows.len(), 11);
            for (index, row) in rows.iter().enumerate() {
                if index != 8 {
                    assert_eq!(row.bounds().height, sections[index], "section {index}");
                }
            }
            assert_eq!(rows[10].bounds().height, 170.0);
            let output_bottom = CARD_TOP + rows[10].bounds().y + rows[10].bounds().height;
            assert_eq!(
                output_bottom + shell::INSPECTOR_OUTPUT_BOTTOM,
                node.size().height
            );
            if metrics.minimum <= viewport - OUTER_BOTTOM {
                assert!(output_bottom <= viewport - OUTER_BOTTOM);
            } else {
                assert!(node.size().height > viewport - OUTER_BOTTOM);
            }
        }
    }
}
