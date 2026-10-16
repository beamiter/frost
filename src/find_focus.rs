//! Restore Find keyboard focus without moving its caret or reversing selection.
use iced::advanced::text;
use iced::advanced::widget::Tree;
use iced::advanced::widget::{operation, Id, Operation};
use iced::advanced::{layout, mouse, overlay, renderer, Clipboard, Layout, Shell, Widget};
use iced::widget::text_input::{self, Value};
use iced::{Element, Event, Length, Rectangle, Size, Vector};
use std::any::Any;

struct Input<'a, Message, Theme, Renderer> {
    id: Id,
    value: Value,
    content: Element<'a, Message, Theme, Renderer>,
}

/// Only the Find input opts in; all pointer, touch, keyboard and IME events
/// still pass through the unmodified Iced input.
pub(crate) fn input<
    'a,
    Message: Clone + 'a,
    Theme: text_input::Catalog + 'a,
    Renderer: text::Renderer + 'a,
>(
    id: Id,
    value: &str,
    content: text_input::TextInput<'a, Message, Theme, Renderer>,
) -> Element<'a, Message, Theme, Renderer> {
    Element::new(Input {
        id: id.clone(),
        value: Value::new(value),
        content: content.id(id).into(),
    })
}

impl<Message, Theme, Renderer: text::Renderer> Widget<Message, Theme, Renderer>
    for Input<'_, Message, Theme, Renderer>
{
    fn children(&self) -> Vec<Tree> {
        vec![Tree::new(self.content.as_widget())]
    }
    fn diff(&self, tree: &mut Tree) {
        tree.diff_children(std::slice::from_ref(&self.content));
    }

    fn size(&self) -> Size<Length> {
        self.content.as_widget().size()
    }

    fn size_hint(&self) -> Size<Length> {
        self.content.as_widget().size_hint()
    }

    fn layout(
        &mut self,
        tree: &mut Tree,
        renderer: &Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        self.content
            .as_widget_mut()
            .layout(&mut tree.children[0], renderer, limits)
    }

    fn draw(
        &self,
        tree: &Tree,
        renderer: &mut Renderer,
        theme: &Theme,
        style: &renderer::Style,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
    ) {
        self.content.as_widget().draw(
            &tree.children[0],
            renderer,
            theme,
            style,
            layout,
            cursor,
            viewport,
        );
    }

    fn operate(
        &mut self,
        tree: &mut Tree,
        layout: Layout<'_>,
        renderer: &Renderer,
        operation: &mut dyn iced::advanced::widget::Operation,
    ) {
        self.content
            .as_widget_mut()
            .operate(&mut tree.children[0], layout, renderer, operation);
        let mut request = RestoreFocus(false);
        operation.custom(Some(&self.id), layout.bounds(), &mut request);
        if request.0 {
            let state = tree.children[0]
                .state
                .downcast_mut::<text_input::State<Renderer::Paragraph>>();
            restore_focus(state, &self.value);
        }
    }

    fn update(
        &mut self,
        tree: &mut Tree,
        event: &Event,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        renderer: &Renderer,
        clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, Message>,
        viewport: &Rectangle,
    ) {
        self.content.as_widget_mut().update(
            &mut tree.children[0],
            event,
            layout,
            cursor,
            renderer,
            clipboard,
            shell,
            viewport,
        );
    }

    fn mouse_interaction(
        &self,
        tree: &Tree,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
        renderer: &Renderer,
    ) -> mouse::Interaction {
        self.content.as_widget().mouse_interaction(
            &tree.children[0],
            layout,
            cursor,
            viewport,
            renderer,
        )
    }

    fn overlay<'a>(
        &'a mut self,
        tree: &'a mut Tree,
        layout: Layout<'a>,
        renderer: &Renderer,
        viewport: &Rectangle,
        translation: Vector,
    ) -> Option<overlay::Element<'a, Message, Theme, Renderer>> {
        self.content.as_widget_mut().overlay(
            &mut tree.children[0],
            layout,
            renderer,
            viewport,
            translation,
        )
    }
}

struct RestoreFocus(bool);

pub(crate) fn refocus<T: Send + 'static>(id: Id) -> iced::Task<T> {
    iced::advanced::widget::operate(Refocus(id))
}

struct Refocus(Id);
impl<T> Operation<T> for Refocus {
    fn traverse(&mut self, visit: &mut dyn FnMut(&mut dyn Operation<T>)) {
        visit(self);
    }
    fn custom(&mut self, id: Option<&Id>, _: Rectangle, state: &mut dyn Any) {
        if id == Some(&self.0) {
            if let Some(request) = state.downcast_mut::<RestoreFocus>() {
                request.0 = true;
            }
        }
    }
    fn focusable(&mut self, id: Option<&Id>, _: Rectangle, state: &mut dyn operation::Focusable) {
        if id != Some(&self.0) {
            state.unfocus();
        }
    }
}

fn restore_focus<P: text::Paragraph>(state: &mut text_input::State<P>, value: &Value) {
    let cursor = state.cursor().state(value);
    state.focus();
    match cursor {
        text_input::cursor::State::Index(index) => state.move_cursor_to(index),
        text_input::cursor::State::Selection { start, end } => state.select_range(start, end),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use iced::widget::{button, row, text_input};

    type InputState = text_input::State<<() as text::Renderer>::Paragraph>;
    #[derive(Debug, Clone, PartialEq, Eq)]
    enum Message {
        Input(String),
        Toggle,
    }

    fn state(tree: &mut Tree) -> &mut InputState {
        tree.children[0].children[0]
            .state
            .downcast_mut::<InputState>()
    }

    #[test]
    fn control_click_restores_directed_selection_without_changing_query_or_committing_ime() {
        for label in ["Aa", ".*", "Reveal match"] {
            for touch in [false, true] {
                for (start, end) in [(2, 2), (1, 3), (3, 1)] {
                    let query = "aé👩‍💻z";
                    let value = Value::new(query);
                    let id = Id::new("find-test");
                    let mut element: Element<'_, Message, iced::Theme, ()> = row![
                        input(
                            id.clone(),
                            query,
                            text_input("", query).width(200).on_input(Message::Input)
                        ),
                        button(label).width(60).on_press(Message::Toggle),
                    ]
                    .into();
                    let mut tree = Tree::new(element.as_widget());
                    let viewport = Rectangle::with_size(Size::new(400.0, 100.0));
                    let node = element.as_widget_mut().layout(
                        &mut tree,
                        &(),
                        &layout::Limits::new(Size::ZERO, viewport.size()),
                    );
                    state(&mut tree).focus();
                    state(&mut tree).select_range(start, end);
                    let before = state(&mut tree).cursor().state(&value);
                    let point = node.children()[1].bounds().center();
                    let mut messages = Vec::new();
                    let update = |element: &mut Element<'_, Message, iced::Theme, ()>,
                                  tree: &mut Tree,
                                  messages: &mut Vec<Message>,
                                  event| {
                        element.as_widget_mut().update(
                            tree,
                            &event,
                            Layout::new(&node),
                            mouse::Cursor::Available(point),
                            &(),
                            &mut iced::advanced::clipboard::Null,
                            &mut Shell::new(messages),
                            &viewport,
                        );
                    };
                    update(
                        &mut element,
                        &mut tree,
                        &mut messages,
                        Event::InputMethod(iced::advanced::input_method::Event::Opened),
                    );
                    update(
                        &mut element,
                        &mut tree,
                        &mut messages,
                        Event::InputMethod(iced::advanced::input_method::Event::Preedit(
                            "候補".into(),
                            None,
                        )),
                    );
                    let press = if touch {
                        Event::Touch(iced::touch::Event::FingerPressed {
                            id: iced::touch::Finger(1),
                            position: point,
                        })
                    } else {
                        Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left))
                    };
                    update(&mut element, &mut tree, &mut messages, press);
                    // Pointer down must retain Iced's normal blur. No eager refocus
                    // or synthetic commit before the control is activated on release.
                    assert!(!state(&mut tree).is_focused());
                    assert!(messages.is_empty());
                    let release = if touch {
                        Event::Touch(iced::touch::Event::FingerLifted {
                            id: iced::touch::Finger(1),
                            position: point,
                        })
                    } else {
                        Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left))
                    };
                    update(&mut element, &mut tree, &mut messages, release);
                    assert_eq!(messages, [Message::Toggle]);
                    for _ in 0..2 {
                        element.as_widget_mut().operate(
                            &mut tree,
                            Layout::new(&node),
                            &(),
                            &mut Refocus(id.clone()),
                        );
                        assert!(state(&mut tree).is_focused());
                        assert_eq!(state(&mut tree).cursor().state(&value), before);
                        assert_eq!(messages, [Message::Toggle]);
                    }
                    update(
                        &mut element,
                        &mut tree,
                        &mut messages,
                        Event::InputMethod(iced::advanced::input_method::Event::Commit("X".into())),
                    );
                    let expected = if start == end {
                        "aéX👩‍💻z"
                    } else {
                        "aXz"
                    };
                    assert_eq!(messages, [Message::Toggle, Message::Input(expected.into())]);
                }
            }
        }
    }

    #[test]
    fn ordinary_outside_click_and_other_focus_operations_are_unchanged() {
        let id = Id::new("find-test");
        let mut element: Element<'_, (), iced::Theme, ()> = input(
            id.clone(),
            "draft",
            text_input("", "draft").width(100).on_input(|_| ()),
        );
        let mut tree = Tree::new(element.as_widget());
        let viewport = Rectangle::with_size(Size::new(400.0, 100.0));
        let node = element.as_widget_mut().layout(
            &mut tree,
            &(),
            &layout::Limits::new(Size::ZERO, viewport.size()),
        );
        let input_state = |tree: &mut Tree| {
            tree.children[0]
                .state
                .downcast_mut::<InputState>()
                .is_focused()
        };
        element.as_widget_mut().operate(
            &mut tree,
            Layout::new(&node),
            &(),
            &mut operation::focusable::focus(id),
        );
        assert!(input_state(&mut tree));
        element.as_widget_mut().update(
            &mut tree,
            &Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)),
            Layout::new(&node),
            mouse::Cursor::Available(iced::Point::new(300.0, 50.0)),
            &(),
            &mut iced::advanced::clipboard::Null,
            &mut Shell::new(&mut Vec::new()),
            &viewport,
        );
        assert!(!input_state(&mut tree));
        element.as_widget_mut().operate(
            &mut tree,
            Layout::new(&node),
            &(),
            &mut Refocus(Id::new("missing")),
        );
        assert!(!input_state(&mut tree));
    }

    #[test]
    fn only_find_control_handlers_request_cursor_preserving_refocus() {
        let source = include_str!("main.rs");
        for message in ["SearchToggleRegex", "SearchToggleCase"] {
            let handler = source
                .split_once(&format!("            Message::{message} => {{"))
                .unwrap()
                .1
                .split_once("\n            }")
                .unwrap()
                .0;
            assert!(handler.contains("if self.search.is_open"));
            assert!(!handler.contains("return "));
            assert!(
                handler.contains("post_update_task = find_focus::refocus(SEARCH_INPUT_ID.clone())")
            );
        }
        let update = source
            .split_once("    fn update_message(")
            .unwrap()
            .1
            .split_once("    /// Cache crops")
            .unwrap()
            .0;
        assert!(update.contains("let mut post_update_task = Task::none();"));
        assert!(update.contains("self.recompute_links();\n        self.refresh_kitty_handles();\n        post_update_task"));

        let reveal = source
            .split_once("    fn reveal_hidden_search_match(")
            .unwrap()
            .1
            .split_once("    /// Route a keypress")
            .unwrap()
            .0;
        assert!(reveal.contains("find_focus::refocus(SEARCH_INPUT_ID.clone())"));
        let bar = source
            .split_once("    fn search_bar(&self)")
            .unwrap()
            .1
            .split_once("    fn search_replace_panel")
            .unwrap()
            .0;
        assert!(bar.contains("find_focus::input("));
    }
}
