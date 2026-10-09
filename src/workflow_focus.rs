//! Scope workflow keyboard focus and reveal explicit navigation without idle snapping.
use iced::advanced::widget::{self, operation, Id, Operation};
use iced::{Rectangle, Task, Vector};
use std::sync::LazyLock;

pub(crate) const FORM_ID: &str = "workflow-argument-form";
pub(crate) const SCROLL_ID: &str = "workflow-argument-scroll";
pub(crate) const PICKER_ID: &str = "workflow-picker-panel";
pub(crate) const PICKER_SCROLL_ID: &str = "workflow-picker-scroll";
pub(crate) const PICKER_SELECTED_ID: &str = "workflow-picker-selected";
pub(crate) const ERROR_ID: &str = "workflow-argument-error";

static ROW_IDS: LazyLock<Vec<Id>> = LazyLock::new(|| {
    (0..jterm_core::workflows::MAX_WORKFLOW_ARGS)
        .map(|_| Id::unique())
        .collect()
});

pub(crate) fn row_id(index: usize) -> Id {
    ROW_IDS.get(index).cloned().unwrap_or_else(Id::unique)
}

pub(crate) fn navigate<T: Send + 'static>(backwards: bool) -> Task<T> {
    widget::operate(focus_operation(backwards)).chain(reveal())
}

fn focus_operation<T: Send + 'static>(backwards: bool) -> Box<dyn Operation<T>> {
    if backwards {
        Box::new(operation::scope(
            Id::new(FORM_ID),
            operation::focusable::focus_previous(),
        ))
    } else {
        Box::new(operation::scope(
            Id::new(FORM_ID),
            operation::focusable::focus_next(),
        ))
    }
}

pub(crate) fn reveal<T: Send + 'static>() -> Task<T> {
    widget::operate(operation::scope(Id::new(FORM_ID), Reveal::default()))
}

pub(crate) fn reveal_picker<T: Send + 'static>() -> Task<T> {
    widget::operate(operation::scope(
        Id::new(PICKER_ID),
        Reveal {
            target: RevealTarget::Picker,
            ..Reveal::default()
        },
    ))
}

pub(crate) fn reveal_error<T: Send + 'static>() -> Task<T> {
    widget::operate(operation::scope(
        Id::new(FORM_ID),
        Reveal {
            target: RevealTarget::Error,
            ..Reveal::default()
        },
    ))
}

#[derive(Default, PartialEq, Eq)]
enum RevealTarget {
    #[default]
    Focus,
    Picker,
    Error,
}

#[derive(Default)]
struct Reveal {
    target: RevealTarget,
    viewport: Option<(Rectangle, Vector)>,
    focused: Option<(Rectangle, Option<Rectangle>)>,
    current_row: Option<Rectangle>,
    next_row: Option<Rectangle>,
}

impl<T: 'static> Operation<T> for Reveal {
    fn traverse(&mut self, operate: &mut dyn FnMut(&mut dyn Operation<T>)) {
        let previous = self.current_row;
        if let Some(row) = self.next_row.take() {
            self.current_row = Some(row);
        }
        operate(self);
        self.current_row = previous;
    }

    fn container(&mut self, id: Option<&Id>, bounds: Rectangle) {
        match self.target {
            RevealTarget::Picker if id == Some(&Id::new(PICKER_SELECTED_ID)) => {
                self.focused = Some((bounds, None));
            }
            RevealTarget::Error if id == Some(&Id::new(ERROR_ID)) => {
                self.focused = Some((bounds, None));
            }
            RevealTarget::Focus if id.is_some_and(|id| ROW_IDS.contains(id)) => {
                self.next_row = Some(bounds);
            }
            _ => {}
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
        let scroll_id = if self.target == RevealTarget::Picker {
            PICKER_SCROLL_ID
        } else {
            SCROLL_ID
        };
        if id == Some(&Id::new(scroll_id)) {
            self.viewport = Some((bounds, translation));
        }
    }

    fn focusable(
        &mut self,
        _id: Option<&Id>,
        bounds: Rectangle,
        state: &mut dyn operation::Focusable,
    ) {
        if self.target == RevealTarget::Focus && state.is_focused() {
            self.focused = Some((bounds, self.current_row));
        }
    }

    fn finish(&self) -> operation::Outcome<T> {
        let Some((viewport, translation)) = self.viewport else {
            return operation::Outcome::None;
        };
        let Some((input, row)) = self.focused else {
            return operation::Outcome::None;
        };
        // Include the field label when it fits. A very long description must
        // not push the actual input out of a short viewport.
        let focused = row
            .filter(|row| row.height + 8.0 <= viewport.height)
            .unwrap_or(input);
        match reveal_offset(viewport, translation.y, focused) {
            Some(y) => operation::Outcome::Chain(Box::new(operation::scrollable::scroll_to(
                Id::new(if self.target == RevealTarget::Picker {
                    PICKER_SCROLL_ID
                } else {
                    SCROLL_ID
                }),
                operation::scrollable::AbsoluteOffset {
                    x: None,
                    y: Some(y),
                },
            ))),
            None => operation::Outcome::None,
        }
    }
}

fn reveal_offset(viewport: Rectangle, offset: f32, focused: Rectangle) -> Option<f32> {
    let margin = 4.0;
    let top = focused.y - offset;
    let bottom = focused.y + focused.height - offset;
    if top < viewport.y + margin || focused.height + margin * 2.0 > viewport.height {
        Some((focused.y - viewport.y - margin).max(0.0))
    } else if bottom > viewport.y + viewport.height - margin {
        Some((focused.y + focused.height - viewport.y - viewport.height + margin).max(0.0))
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn actual_form_navigation_never_focuses_background_inputs() {
        use iced::advanced::{layout, widget::Tree, Layout};
        use iced::widget::{column, container, text_input};
        let input = |id| text_input("value", "text").id(id).on_input(|_| ());
        let mut element: iced::Element<'_, (), iced::Theme, ()> = column![
            input("outside-before"),
            container(column![input("inside-first"), input("inside-last")]).id(FORM_ID),
            input("outside-after"),
        ]
        .into();
        let mut tree = Tree::new(element.as_widget());
        let node = element.as_widget_mut().layout(
            &mut tree,
            &(),
            &layout::Limits::new(iced::Size::ZERO, iced::Size::new(400.0, 400.0)),
        );
        let apply = |element: &mut iced::Element<'_, (), iced::Theme, ()>,
                     tree: &mut Tree,
                     mut op: Box<dyn Operation>| {
            loop {
                element
                    .as_widget_mut()
                    .operate(tree, Layout::new(&node), &(), op.as_mut());
                match op.finish() {
                    operation::Outcome::Chain(next) => op = next,
                    _ => break,
                }
            }
        };
        #[derive(Default)]
        struct Focused(Vec<Id>);
        impl Operation for Focused {
            fn traverse(&mut self, operate: &mut dyn FnMut(&mut dyn Operation)) {
                operate(self);
            }
            fn focusable(
                &mut self,
                id: Option<&Id>,
                _: Rectangle,
                state: &mut dyn operation::Focusable,
            ) {
                if state.is_focused() {
                    self.0.extend(id.cloned());
                }
            }
        }
        for (backwards, first, last) in [
            (false, "inside-first", "inside-last"),
            (true, "inside-last", "inside-first"),
        ] {
            apply(
                &mut element,
                &mut tree,
                Box::new(operation::focusable::focus(Id::new(first))),
            );
            for expected in [Some(last), None, Some(first)] {
                apply(&mut element, &mut tree, focus_operation(backwards));
                let mut focused = Focused::default();
                element
                    .as_widget_mut()
                    .operate(&mut tree, Layout::new(&node), &(), &mut focused);
                assert_eq!(
                    focused.0,
                    expected.into_iter().map(Id::new).collect::<Vec<_>>()
                );
            }
        }
    }

    #[test]
    fn reveal_uses_current_scroll_and_leaves_visible_focus_in_place() {
        let viewport = Rectangle {
            x: 20.0,
            y: 100.0,
            width: 240.0,
            height: 100.0,
        };
        let field = |y| Rectangle {
            x: 20.0,
            y,
            width: 180.0,
            height: 24.0,
        };
        assert_eq!(reveal_offset(viewport, 0.0, field(120.0)), None);
        assert_eq!(reveal_offset(viewport, 300.0, field(420.0)), None);
        assert_eq!(reveal_offset(viewport, 0.0, field(1000.0)), Some(828.0));
        assert_eq!(reveal_offset(viewport, 900.0, field(100.0)), Some(0.0));
        let tiny = Rectangle {
            height: 12.0,
            ..viewport
        };
        assert_eq!(reveal_offset(tiny, 0.0, field(160.0)), Some(56.0));
    }
}
