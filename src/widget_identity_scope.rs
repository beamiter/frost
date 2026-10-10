//! Reset an explicitly scoped form's retained widget state at its identity boundary.
//! Used by the remote editor and workflow picker/Args forms; unrelated widgets stay intact.
//! Message tokens reject queued callbacks; this scope also retires pressed/input
//! state before a newly rendered callback can inherit it.
use iced::advanced::widget::{tree, Tree};
use iced::advanced::{layout, mouse, overlay, renderer, Clipboard, Layout, Shell, Widget};
use iced::{Element, Event, Length, Rectangle, Size, Vector};
use std::sync::Arc;

struct Scope<'a, Message, Theme, Renderer> {
    identity: Arc<()>,
    content: Element<'a, Message, Theme, Renderer>,
}

struct State {
    identity: Arc<()>,
}

pub(crate) fn scope<'a, Message: 'a, Theme: 'a, Renderer: renderer::Renderer + 'a>(
    identity: Arc<()>,
    content: impl Into<Element<'a, Message, Theme, Renderer>>,
) -> Element<'a, Message, Theme, Renderer> {
    Element::new(Scope {
        identity,
        content: content.into(),
    })
}

impl<Message, Theme, Renderer: renderer::Renderer> Widget<Message, Theme, Renderer>
    for Scope<'_, Message, Theme, Renderer>
{
    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<State>()
    }

    fn state(&self) -> tree::State {
        tree::State::new(State {
            identity: self.identity.clone(),
        })
    }

    fn children(&self) -> Vec<Tree> {
        vec![Tree::new(self.content.as_widget())]
    }

    fn diff(&self, tree: &mut Tree) {
        let state = tree.state.downcast_mut::<State>();
        if Arc::ptr_eq(&state.identity, &self.identity) {
            tree.children[0].diff(self.content.as_widget());
        } else {
            state.identity = self.identity.clone();
            tree.children[0] = Tree::new(self.content.as_widget());
        }
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

#[cfg(test)]
mod tests {
    use super::*;
    use iced::widget::{button, Space};

    // Independent of iced_core's debug-only () renderer; never draws or loads assets.
    struct TestRenderer;

    impl renderer::Renderer for TestRenderer {
        fn start_layer(&mut self, _bounds: Rectangle) {}
        fn end_layer(&mut self) {}
        fn start_transformation(&mut self, _transformation: iced::Transformation) {}
        fn end_transformation(&mut self) {}
        fn fill_quad(&mut self, _quad: renderer::Quad, _background: impl Into<iced::Background>) {}
        fn reset(&mut self, _new_bounds: Rectangle) {}
        fn allocate_image(
            &mut self,
            _handle: &iced::advanced::image::Handle,
            callback: impl FnOnce(Result<iced::advanced::image::Allocation, iced::advanced::image::Error>)
                + Send
                + 'static,
        ) {
            callback(Err(iced::advanced::image::Error::Unsupported));
        }
    }

    #[test]
    fn picker_query_edits_retain_tree_but_same_shape_reopening_resets_it() {
        let mut picker = crate::workflow_picker::WorkflowPickerState::new(0, Vec::new());
        let content = || Space::new().width(20).height(20);
        let initial: Element<'_, (), iced::Theme, TestRenderer> =
            scope(picker.widget_identity(), content());
        let mut tree = Tree::new(initial.as_widget());
        tree.children[0].state = tree::State::new(41_usize);
        picker.set_query("query");
        let edited: Element<'_, (), iced::Theme, TestRenderer> =
            scope(picker.widget_identity(), content());
        tree.diff(edited.as_widget());
        assert_eq!(*tree.children[0].state.downcast_ref::<usize>(), 41);
        // The old tree is still cached when close/reopen occur in one app batch.
        let reopened = crate::workflow_picker::WorkflowPickerState::new(0, Vec::new());
        let replacement: Element<'_, (), iced::Theme, TestRenderer> =
            scope(reopened.widget_identity(), content());
        tree.diff(replacement.as_widget());
        assert!(matches!(tree.children[0].state, tree::State::None));
    }

    #[test]
    fn same_identity_preserves_sentinel_and_equal_length_new_identity_resets_it() {
        let identity = Arc::new(());
        let content = || Space::new().width(20).height(20);
        let initial: Element<'_, (), iced::Theme, TestRenderer> =
            scope(identity.clone(), content());
        let mut tree = Tree::new(initial.as_widget());
        tree.children[0].state = tree::State::new(41_usize);
        let same: Element<'_, (), iced::Theme, TestRenderer> = scope(identity.clone(), content());
        tree.diff(same.as_widget());
        assert_eq!(*tree.children[0].state.downcast_ref::<usize>(), 41);
        let changed: Element<'_, (), iced::Theme, TestRenderer> = scope(Arc::new(()), content());
        tree.diff(changed.as_widget());
        assert_eq!(tree.children.len(), 1);
        assert!(matches!(tree.children[0].state, tree::State::None));
    }

    #[test]
    fn epoch_reset_is_limited_to_the_remote_editor_subtree() {
        let identity = Arc::new(());
        let build = |identity| {
            iced::widget::column![
                Space::new().width(20).height(20),
                scope(identity, Space::new().width(20).height(20)),
            ]
        };
        let old: Element<'_, (), iced::Theme, TestRenderer> = build(identity).into();
        let mut tree = Tree::new(old.as_widget());
        tree.children[0].state = tree::State::new(73_usize);
        tree.children[1].children[0].state = tree::State::new(41_usize);
        let new: Element<'_, (), iced::Theme, TestRenderer> = build(Arc::new(())).into();
        tree.diff(new.as_widget());
        assert_eq!(*tree.children[0].state.downcast_ref::<usize>(), 73);
        assert!(matches!(
            tree.children[1].children[0].state,
            tree::State::None
        ));
    }

    fn send(
        element: &mut Element<'_, u8, iced::Theme, TestRenderer>,
        tree: &mut Tree,
        event: mouse::Event,
    ) -> Vec<u8> {
        let node = element.as_widget_mut().layout(
            tree,
            &TestRenderer,
            &layout::Limits::new(Size::ZERO, Size::new(100.0, 100.0)),
        );
        let mut messages = Vec::new();
        let mut shell = Shell::new(&mut messages);
        element.as_widget_mut().update(
            tree,
            &Event::Mouse(event),
            Layout::new(&node),
            mouse::Cursor::Available(iced::Point::new(10.0, 10.0)),
            &TestRenderer,
            &mut iced::advanced::clipboard::Null,
            &mut shell,
            &Rectangle::with_size(Size::new(100.0, 100.0)),
        );
        messages
    }

    #[test]
    fn pressed_button_cannot_publish_replacement_callback_after_epoch_change() {
        let identity = Arc::new(());
        let content = |message| button(Space::new().width(40).height(30)).on_press(message);
        let mut old: Element<'_, u8, iced::Theme, TestRenderer> =
            scope(identity.clone(), content(1));
        let mut tree = Tree::new(old.as_widget());
        assert!(send(
            &mut old,
            &mut tree,
            mouse::Event::ButtonPressed(mouse::Button::Left)
        )
        .is_empty());
        let mut replacement: Element<'_, u8, iced::Theme, TestRenderer> =
            scope(Arc::new(()), content(2));
        tree.diff(replacement.as_widget());
        assert!(send(
            &mut replacement,
            &mut tree,
            mouse::Event::ButtonReleased(mouse::Button::Left)
        )
        .is_empty());
        assert!(send(
            &mut replacement,
            &mut tree,
            mouse::Event::ButtonPressed(mouse::Button::Left)
        )
        .is_empty());
        assert_eq!(
            send(
                &mut replacement,
                &mut tree,
                mouse::Event::ButtonReleased(mouse::Button::Left)
            ),
            [2]
        );
    }

    #[test]
    fn same_epoch_preserves_an_ordinary_button_press_until_release() {
        let identity = Arc::new(());
        let content = || button(Space::new().width(40).height(30)).on_press(7);
        let mut old: Element<'_, u8, iced::Theme, TestRenderer> =
            scope(identity.clone(), content());
        let mut tree = Tree::new(old.as_widget());
        assert!(send(
            &mut old,
            &mut tree,
            mouse::Event::ButtonPressed(mouse::Button::Left)
        )
        .is_empty());
        let mut redraw: Element<'_, u8, iced::Theme, TestRenderer> = scope(identity, content());
        tree.diff(redraw.as_widget());
        assert_eq!(
            send(
                &mut redraw,
                &mut tree,
                mouse::Event::ButtonReleased(mouse::Button::Left)
            ),
            [7]
        );
    }
}
