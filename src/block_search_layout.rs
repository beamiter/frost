//! Bounded compact search panels and one-shot measured result/feedback reveal.
use iced::advanced::widget::{self, operation, Id, Operation};
use iced::{Rectangle, Size, Task, Vector};

pub(crate) const PANEL_ID: &str = "block-search-panel";
pub(crate) const SCROLL_ID: &str = "block-search-compact-scroll";
pub(crate) const SELECTED_ID: &str = "block-search-selected";
pub(crate) const FEEDBACK_ID: &str = "block-search-feedback";

pub(crate) fn panel_size(window: Size) -> Size {
    Size::new(
        (window.width - 32.0).clamp(1.0, 720.0),
        (window.height - 32.0).clamp(1.0, 560.0),
    )
}

pub(crate) fn compact(panel: Size) -> bool {
    panel.height < 360.0
}

/// Run only after an explicit query, navigation, filter, rebuild or resize.
/// Manual scrolling and ordinary redraws never schedule this operation.
pub(crate) fn reveal<T: Send + 'static>() -> Task<T> {
    widget::operate(operation::scope(Id::new(PANEL_ID), Reveal::default()))
}

#[derive(Default)]
struct Reveal {
    viewport: Option<(Rectangle, Vector)>,
    selected: Option<Rectangle>,
    feedback: Option<Rectangle>,
}

impl<T: 'static> Operation<T> for Reveal {
    fn traverse(&mut self, operate: &mut dyn FnMut(&mut dyn Operation<T>)) {
        operate(self);
    }

    fn container(&mut self, id: Option<&Id>, bounds: Rectangle) {
        if id == Some(&Id::new(SELECTED_ID)) {
            self.selected = Some(bounds);
        } else if id == Some(&Id::new(FEEDBACK_ID)) {
            self.feedback = Some(bounds);
        }
    }

    fn scrollable(
        &mut self,
        id: Option<&Id>,
        bounds: Rectangle,
        _content_bounds: Rectangle,
        translation: Vector,
        _state: &mut dyn operation::Scrollable,
    ) {
        if id == Some(&Id::new(SCROLL_ID)) {
            self.viewport = Some((bounds, translation));
        }
    }

    fn finish(&self) -> operation::Outcome<T> {
        let Some((viewport, translation)) = self.viewport else {
            return operation::Outcome::None;
        };
        let Some(target) = self.selected.or(self.feedback) else {
            return operation::Outcome::None;
        };
        match reveal_offset(viewport, translation.y, target) {
            Some(y) => operation::Outcome::Chain(Box::new(operation::scrollable::scroll_to(
                Id::new(SCROLL_ID),
                operation::scrollable::AbsoluteOffset {
                    x: None,
                    y: Some(y),
                },
            ))),
            None => operation::Outcome::None,
        }
    }
}

fn reveal_offset(viewport: Rectangle, offset: f32, target: Rectangle) -> Option<f32> {
    let margin = 4.0;
    let top = target.y - offset;
    let bottom = target.y + target.height - offset;
    if top < viewport.y + margin || target.height + margin * 2.0 > viewport.height {
        Some((target.y - viewport.y - margin).max(0.0))
    } else if bottom > viewport.y + viewport.height - margin {
        Some((target.y + target.height - viewport.y - viewport.height + margin).max(0.0))
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn small_panels_fit_the_window_without_changing_normal_size() {
        for window in [Size::new(280.0, 200.0), Size::new(160.0, 120.0)] {
            let panel = panel_size(window);
            assert!(panel.width + 32.0 <= window.width);
            assert!(panel.height + 32.0 <= window.height);
            assert!(compact(panel));
        }
        assert_eq!(
            panel_size(Size::new(1280.0, 720.0)),
            Size::new(720.0, 560.0)
        );
        assert!(!compact(panel_size(Size::new(1280.0, 720.0))));
    }

    #[test]
    fn reveal_keeps_visible_results_still_and_starts_oversized_feedback_at_top() {
        let viewport = Rectangle::new(iced::Point::new(20.0, 60.0), Size::new(224.0, 108.0));
        let target = |y, height| Rectangle {
            y,
            height,
            ..viewport
        };
        assert_eq!(reveal_offset(viewport, 300.0, target(376.0, 38.0)), None);
        assert_eq!(
            reveal_offset(viewport, 0.0, target(400.0, 38.0)),
            Some(274.0)
        );
        assert_eq!(
            reveal_offset(viewport, 600.0, target(200.0, 38.0)),
            Some(136.0)
        );
        assert_eq!(
            reveal_offset(viewport, 0.0, target(200.0, 180.0)),
            Some(136.0)
        );
    }

    #[test]
    fn real_scroll_operation_reveals_selection_or_feedback_and_keeps_query_fixed() {
        use iced::advanced::{layout, widget::Tree, Layout};
        use iced::widget::{column, container, scrollable, Space};
        use iced::Length;

        for target_id in [SELECTED_ID, FEEDBACK_ID] {
            let mut element: iced::Element<'_, (), iced::Theme, ()> = container(
                column![
                    container(Space::new().height(28)).id("query"),
                    scrollable(column![
                        Space::new().height(160),
                        container(Space::new().height(40)).id(target_id),
                        Space::new().height(200),
                    ])
                    .id(SCROLL_ID)
                    .height(Length::Fill),
                ]
                .spacing(8),
            )
            .id(PANEL_ID)
            .padding(12)
            .width(248)
            .height(168)
            .into();
            let mut tree = Tree::new(element.as_widget());
            let node = element.as_widget_mut().layout(
                &mut tree,
                &(),
                &layout::Limits::new(Size::ZERO, Size::new(280.0, 200.0)),
            );
            let mut apply = |mut op: Box<dyn Operation>| loop {
                element
                    .as_widget_mut()
                    .operate(&mut tree, Layout::new(&node), &(), op.as_mut());
                match op.finish() {
                    operation::Outcome::Chain(next) => op = next,
                    _ => break,
                }
            };
            apply(Box::new(operation::scope(
                Id::new(PANEL_ID),
                Reveal::default(),
            )));
            let mut measured = Reveal::default();
            element
                .as_widget_mut()
                .operate(&mut tree, Layout::new(&node), &(), &mut measured);
            let (viewport, translation) = measured.viewport.unwrap();
            let target = measured.selected.or(measured.feedback).unwrap();
            assert_eq!(viewport.y, 48.0); // Query + padding + gap never scroll.
            assert!(translation.y > 0.0);
            assert!(target.y - translation.y >= viewport.y);
            assert!(target.y + target.height - translation.y <= viewport.y + viewport.height);
            assert!(matches!(
                <Reveal as Operation<()>>::finish(&measured),
                operation::Outcome::None
            ));
        }
    }
}
