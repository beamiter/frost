//! Intercept Ctrl+wheel at real layout bounds before scrollable/terminal children.
use crate::font_zoom::Group;
use iced::advanced::widget::{tree, Tree};
use iced::advanced::{layout, mouse, overlay, renderer, Clipboard, Layout, Shell, Widget};
use iced::{Element, Event, Length, Rectangle, Size, Vector};

struct Scope<'a, Message, Theme, Renderer> {
    group: Group,
    ctrl: bool,
    enabled: bool,
    on_wheel: fn(Group, mouse::ScrollDelta) -> Message,
    content: Element<'a, Message, Theme, Renderer>,
}

struct State {
    ctrl: bool,
}

pub(crate) fn scope<'a, Message: 'a, Theme: 'a, Renderer: renderer::Renderer + 'a>(
    group: Group,
    ctrl: bool,
    enabled: bool,
    on_wheel: fn(Group, mouse::ScrollDelta) -> Message,
    content: impl Into<Element<'a, Message, Theme, Renderer>>,
) -> Element<'a, Message, Theme, Renderer> {
    Element::new(Scope {
        group,
        ctrl,
        enabled,
        on_wheel,
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
        tree::State::new(State { ctrl: self.ctrl })
    }

    fn children(&self) -> Vec<Tree> {
        vec![Tree::new(self.content.as_widget())]
    }

    fn diff(&self, tree: &mut Tree) {
        tree.children[0].diff(self.content.as_widget());
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
        let state = tree.state.downcast_mut::<State>();
        if let Event::Keyboard(iced::keyboard::Event::ModifiersChanged(modifiers)) = event {
            state.ctrl = modifiers.control();
        }
        if let Event::Window(iced::window::Event::Unfocused) = event {
            state.ctrl = false;
        }
        if shell.is_event_captured() {
            return;
        }
        // Half-open bounds make adjoining regions unambiguous. Levitating
        // cursors belong to a higher stack layer, never to this font group.
        if let Event::Mouse(mouse::Event::WheelScrolled { delta }) = event {
            let hit = !cursor.is_levitating()
                && cursor.position().is_some_and(|p| {
                    [layout.bounds(), *viewport].iter().all(|r| {
                        p.x >= r.x && p.x < r.x + r.width && p.y >= r.y && p.y < r.y + r.height
                    })
                });
            let y = match delta {
                mouse::ScrollDelta::Lines { y, .. } | mouse::ScrollDelta::Pixels { y, .. } => *y,
            };
            // Horizontal-only, zero, and invalid vertical deltas retain the
            // child's original behavior; only usable vertical input zooms.
            if self.enabled && state.ctrl && hit && y.is_finite() && y != 0.0 {
                shell.publish((self.on_wheel)(self.group, *delta));
                shell.capture_event();
                return;
            }
        }
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

    #[derive(Debug, PartialEq)]
    enum Output {
        Zoom(Group),
        Child,
    }

    struct Probe;
    impl Widget<Output, (), TestRenderer> for Probe {
        fn size(&self) -> Size<Length> {
            Size::new(Length::Fixed(100.0), Length::Fixed(100.0))
        }
        fn layout(&mut self, _: &mut Tree, _: &TestRenderer, _: &layout::Limits) -> layout::Node {
            layout::Node::new(Size::new(100.0, 100.0))
        }
        fn draw(
            &self,
            _: &Tree,
            _: &mut TestRenderer,
            _: &(),
            _: &renderer::Style,
            _: Layout<'_>,
            _: mouse::Cursor,
            _: &Rectangle,
        ) {
        }
        fn update(
            &mut self,
            _: &mut Tree,
            event: &Event,
            layout: Layout<'_>,
            cursor: mouse::Cursor,
            _: &TestRenderer,
            _: &mut dyn Clipboard,
            shell: &mut Shell<'_, Output>,
            _: &Rectangle,
        ) {
            if matches!(event, Event::Mouse(mouse::Event::WheelScrolled { .. }))
                && cursor.is_over(layout.bounds())
            {
                shell.publish(Output::Child);
                shell.capture_event();
            }
        }
    }

    fn run(
        ctrl: bool,
        enabled: bool,
        cursor: mouse::Cursor,
        viewport: Rectangle,
        prior_capture: bool,
        group: Group,
    ) -> Vec<Output> {
        let mut element = scope(
            group,
            ctrl,
            enabled,
            |group, _| Output::Zoom(group),
            Element::new(Probe),
        );
        let mut tree = Tree::new(element.as_widget());
        let node = element.as_widget_mut().layout(
            &mut tree,
            &TestRenderer,
            &layout::Limits::new(Size::ZERO, Size::new(100.0, 100.0)),
        );
        let mut output = Vec::new();
        let mut shell = Shell::new(&mut output);
        if prior_capture {
            shell.capture_event();
        }
        element.as_widget_mut().update(
            &mut tree,
            &Event::Mouse(mouse::Event::WheelScrolled {
                delta: mouse::ScrollDelta::Pixels { x: 0.0, y: 0.1 },
            }),
            Layout::new(&node),
            cursor,
            &TestRenderer,
            &mut iced::advanced::clipboard::Null,
            &mut shell,
            &viewport,
        );
        output
    }

    #[test]
    fn control_wheel_is_captured_before_child_in_every_group() {
        let viewport = Rectangle::with_size(Size::new(100.0, 100.0));
        for group in [Group::Chrome, Group::Content] {
            assert_eq!(
                run(
                    true,
                    true,
                    mouse::Cursor::Available(iced::Point::new(50.0, 50.0)),
                    viewport,
                    false,
                    group
                ),
                vec![Output::Zoom(group)]
            );
            assert_eq!(
                run(
                    false,
                    true,
                    mouse::Cursor::Available(iced::Point::new(50.0, 50.0)),
                    viewport,
                    false,
                    group
                ),
                vec![Output::Child]
            );
            assert_eq!(
                run(
                    true,
                    false,
                    mouse::Cursor::Available(iced::Point::new(50.0, 50.0)),
                    viewport,
                    false,
                    group
                ),
                vec![Output::Child]
            );
        }
    }

    #[test]
    fn unavailable_levitating_clipped_and_half_open_edges_never_zoom() {
        let viewport = Rectangle::with_size(Size::new(100.0, 100.0));
        for cursor in [
            mouse::Cursor::Unavailable,
            mouse::Cursor::Available(iced::Point::new(10.0, 10.0)).levitate(),
            mouse::Cursor::Available(iced::Point::new(100.0, 10.0)),
            mouse::Cursor::Available(iced::Point::new(10.0, 100.0)),
            mouse::Cursor::Available(iced::Point::new(-0.1, 10.0)),
        ] {
            assert!(!run(true, true, cursor, viewport, false, Group::Chrome)
                .contains(&Output::Zoom(Group::Chrome)));
        }
        let clipped = Rectangle::with_size(Size::new(40.0, 40.0));
        assert!(!run(
            true,
            true,
            mouse::Cursor::Available(iced::Point::new(50.0, 50.0)),
            clipped,
            false,
            Group::Content
        )
        .contains(&Output::Zoom(Group::Content)));
        assert_eq!(
            run(
                true,
                true,
                mouse::Cursor::Available(iced::Point::ORIGIN),
                viewport,
                false,
                Group::Chrome
            ),
            vec![Output::Zoom(Group::Chrome)]
        );
    }

    #[test]
    fn pre_captured_events_cannot_publish_a_second_zoom() {
        let viewport = Rectangle::with_size(Size::new(100.0, 100.0));
        assert!(!run(
            true,
            true,
            mouse::Cursor::Available(iced::Point::new(50.0, 50.0)),
            viewport,
            true,
            Group::Content
        )
        .contains(&Output::Zoom(Group::Content)));
    }
    #[test]
    fn modifiers_in_one_event_batch_and_focus_loss_are_observed_without_rebuild() {
        let mut element = scope(
            Group::Content,
            false,
            true,
            |group, _| Output::Zoom(group),
            Element::new(Probe),
        );
        let mut tree = Tree::new(element.as_widget());
        let node = element.as_widget_mut().layout(
            &mut tree,
            &TestRenderer,
            &layout::Limits::new(Size::ZERO, Size::new(100.0, 100.0)),
        );
        let wheel = Event::Mouse(mouse::Event::WheelScrolled {
            delta: mouse::ScrollDelta::Lines { x: 0.0, y: 1.0 },
        });
        let mut output = Vec::new();
        for event in [
            Event::Keyboard(iced::keyboard::Event::ModifiersChanged(
                iced::keyboard::Modifiers::CTRL,
            )),
            wheel.clone(),
            Event::Window(iced::window::Event::Unfocused),
            wheel.clone(),
            Event::Keyboard(iced::keyboard::Event::ModifiersChanged(
                iced::keyboard::Modifiers::CTRL,
            )),
            Event::Keyboard(iced::keyboard::Event::ModifiersChanged(
                iced::keyboard::Modifiers::empty(),
            )),
            wheel,
        ] {
            let mut shell = Shell::new(&mut output);
            element.as_widget_mut().update(
                &mut tree,
                &event,
                Layout::new(&node),
                mouse::Cursor::Available(iced::Point::new(50.0, 50.0)),
                &TestRenderer,
                &mut iced::advanced::clipboard::Null,
                &mut shell,
                &Rectangle::with_size(Size::new(100.0, 100.0)),
            );
        }
        assert_eq!(
            output,
            vec![Output::Zoom(Group::Content), Output::Child, Output::Child]
        );
    }
    #[test]
    fn horizontal_zero_and_nonfinite_control_wheel_reach_child_in_both_units() {
        for group in [Group::Chrome, Group::Content] {
            for pixels in [false, true] {
                for y in [
                    0.0,
                    -0.0,
                    f32::NAN,
                    f32::INFINITY,
                    f32::NEG_INFINITY,
                    0.125,
                    -1.0,
                ] {
                    let mut element = scope(
                        group,
                        true,
                        true,
                        |group, _| Output::Zoom(group),
                        Element::new(Probe),
                    );
                    let mut tree = Tree::new(element.as_widget());
                    let node = element.as_widget_mut().layout(
                        &mut tree,
                        &TestRenderer,
                        &layout::Limits::new(Size::ZERO, Size::new(100.0, 100.0)),
                    );
                    let delta = if pixels {
                        mouse::ScrollDelta::Pixels { x: 5.0, y }
                    } else {
                        mouse::ScrollDelta::Lines { x: 5.0, y }
                    };
                    let mut output = Vec::new();
                    element.as_widget_mut().update(
                        &mut tree,
                        &Event::Mouse(mouse::Event::WheelScrolled { delta }),
                        Layout::new(&node),
                        mouse::Cursor::Available(iced::Point::new(50.0, 50.0)),
                        &TestRenderer,
                        &mut iced::advanced::clipboard::Null,
                        &mut Shell::new(&mut output),
                        &Rectangle::with_size(Size::new(100.0, 100.0)),
                    );
                    let expected = if y.is_finite() && y != 0.0 {
                        Output::Zoom(group)
                    } else {
                        Output::Child
                    };
                    assert_eq!(
                        output,
                        vec![expected],
                        "group={group:?}, pixels={pixels}, y={y}"
                    );
                }
            }
        }
    }
}
