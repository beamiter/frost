//! Keep one navigation tree while its presentation changes between dock and drawer.
use iced::advanced::widget::{tree, Tree};
use iced::advanced::{layout, mouse, overlay, renderer, Clipboard, Layout, Shell, Widget};
use iced::{Element, Event, Length, Rectangle, Size, Vector};

#[derive(Clone, Copy)]
pub(crate) struct Placement {
    pub panel: Rectangle,
    pub backdrop: Rectangle,
    pub visible: bool,
    pub drawer: bool,
}

pub(crate) fn surface<'a, Message: 'a>(
    native: Element<'a, Message>,
    backdrop: Element<'a, Message>,
    navigation: Element<'a, Message>,
    placement: Placement,
) -> Element<'a, Message> {
    Element::new(Surface {
        native,
        backdrop,
        navigation,
        placement,
    })
}

fn covered_pointer(event: &Event, cursor: mouse::Cursor, bounds: Rectangle) -> bool {
    match event {
        Event::Mouse(_) => cursor.is_over(bounds),
        Event::Touch(
            iced::touch::Event::FingerPressed { position, .. }
            | iced::touch::Event::FingerMoved { position, .. }
            | iced::touch::Event::FingerLifted { position, .. }
            | iced::touch::Event::FingerLost { position, .. },
        ) => bounds.contains(*position),
        _ => false,
    }
}

struct Surface<'a, Message> {
    native: Element<'a, Message>,
    backdrop: Element<'a, Message>,
    navigation: Element<'a, Message>,
    placement: Placement,
}

// The default stateless tag is shared by Stack and other unrelated widgets.
// Overlay insertion/removal must rebuild those trees before indexing our
// fixed native/backdrop/navigation children.
struct SurfaceTag;

impl<Message> Widget<Message, iced::Theme, iced::Renderer> for Surface<'_, Message> {
    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<SurfaceTag>()
    }

    fn children(&self) -> Vec<Tree> {
        vec![
            Tree::new(self.native.as_widget()),
            Tree::new(self.backdrop.as_widget()),
            Tree::new(self.navigation.as_widget()),
        ]
    }
    fn diff(&self, tree: &mut Tree) {
        tree.diff_children(&[
            self.native.as_widget(),
            self.backdrop.as_widget(),
            self.navigation.as_widget(),
        ]);
    }
    fn size(&self) -> Size<Length> {
        Size::new(Length::Fill, Length::Fill)
    }
    fn layout(
        &mut self,
        tree: &mut Tree,
        renderer: &iced::Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        let native = self
            .native
            .as_widget_mut()
            .layout(&mut tree.children[0], renderer, limits);
        let backdrop = self
            .backdrop
            .as_widget_mut()
            .layout(
                &mut tree.children[1],
                renderer,
                &layout::Limits::new(
                    self.placement.backdrop.size(),
                    self.placement.backdrop.size(),
                ),
            )
            .move_to(self.placement.backdrop.position());
        // Hidden navigation keeps its real allocation and retained editor/scroll state.
        let navigation = self
            .navigation
            .as_widget_mut()
            .layout(
                &mut tree.children[2],
                renderer,
                &layout::Limits::new(self.placement.panel.size(), self.placement.panel.size()),
            )
            .move_to(self.placement.panel.position());
        layout::Node::with_children(native.size(), vec![native, backdrop, navigation])
    }
    fn draw(
        &self,
        tree: &Tree,
        renderer: &mut iced::Renderer,
        theme: &iced::Theme,
        style: &renderer::Style,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
    ) {
        use iced::advanced::Renderer as _;
        let mut children = layout.children();
        let native = children.next().unwrap();
        let backdrop = children.next().unwrap();
        let navigation = children.next().unwrap();
        self.native.as_widget().draw(
            &tree.children[0],
            renderer,
            theme,
            style,
            native,
            if self.placement.drawer && cursor.is_over(backdrop.bounds()) {
                mouse::Cursor::Unavailable
            } else {
                cursor
            },
            viewport,
        );
        if self.placement.drawer {
            self.backdrop.as_widget().draw(
                &tree.children[1],
                renderer,
                theme,
                style,
                backdrop,
                cursor,
                viewport,
            );
        }
        if self.placement.visible {
            if let Some(clip) = navigation.bounds().intersection(viewport) {
                renderer.with_layer(clip, |renderer| {
                    self.navigation.as_widget().draw(
                        &tree.children[2],
                        renderer,
                        theme,
                        style,
                        navigation,
                        cursor,
                        &clip,
                    )
                });
            }
        }
    }
    fn update(
        &mut self,
        tree: &mut Tree,
        event: &Event,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        renderer: &iced::Renderer,
        clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, Message>,
        viewport: &Rectangle,
    ) {
        let mut children = layout.children();
        let native = children.next().unwrap();
        let backdrop = children.next().unwrap();
        let navigation = children.next().unwrap();
        if self.placement.visible {
            self.navigation.as_widget_mut().update(
                &mut tree.children[2],
                event,
                navigation,
                cursor,
                renderer,
                clipboard,
                shell,
                viewport,
            );
            if shell.is_event_captured() {
                return;
            }
        }
        if self.placement.drawer {
            self.backdrop.as_widget_mut().update(
                &mut tree.children[1],
                event,
                backdrop,
                cursor,
                renderer,
                clipboard,
                shell,
                viewport,
            );
            if shell.is_event_captured() {
                return;
            }
            // The drawer owns keyboard/IME and all covered pointer input. The
            // titlebar and rail remain interactive. Redraw/window events still
            // reach the native child so streams and animation keep updating.
            if matches!(event, Event::Keyboard(_) | Event::InputMethod(_)) {
                return;
            }
            if covered_pointer(event, cursor, backdrop.bounds()) {
                shell.capture_event();
                return;
            }
        }
        self.native.as_widget_mut().update(
            &mut tree.children[0],
            event,
            native,
            cursor,
            renderer,
            clipboard,
            shell,
            viewport,
        );
    }
    fn operate(
        &mut self,
        tree: &mut Tree,
        layout: Layout<'_>,
        renderer: &iced::Renderer,
        operation: &mut dyn iced::advanced::widget::Operation,
    ) {
        let mut children = layout.children();
        self.native.as_widget_mut().operate(
            &mut tree.children[0],
            children.next().unwrap(),
            renderer,
            operation,
        );
        let backdrop = children.next().unwrap();
        if self.placement.drawer {
            self.backdrop.as_widget_mut().operate(
                &mut tree.children[1],
                backdrop,
                renderer,
                operation,
            );
        }
        self.navigation.as_widget_mut().operate(
            &mut tree.children[2],
            children.next().unwrap(),
            renderer,
            operation,
        );
    }
    fn mouse_interaction(
        &self,
        tree: &Tree,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
        renderer: &iced::Renderer,
    ) -> mouse::Interaction {
        let mut children = layout.children();
        let native = children.next().unwrap();
        let backdrop = children.next().unwrap();
        let navigation = children.next().unwrap();
        if self.placement.visible && cursor.is_over(navigation.bounds()) {
            self.navigation.as_widget().mouse_interaction(
                &tree.children[2],
                navigation,
                cursor,
                viewport,
                renderer,
            )
        } else if self.placement.drawer && cursor.is_over(backdrop.bounds()) {
            self.backdrop.as_widget().mouse_interaction(
                &tree.children[1],
                backdrop,
                cursor,
                viewport,
                renderer,
            )
        } else {
            self.native.as_widget().mouse_interaction(
                &tree.children[0],
                native,
                cursor,
                viewport,
                renderer,
            )
        }
    }
    fn overlay<'a>(
        &'a mut self,
        tree: &'a mut Tree,
        layout: Layout<'a>,
        renderer: &iced::Renderer,
        viewport: &Rectangle,
        translation: Vector,
    ) -> Option<overlay::Element<'a, Message, iced::Theme, iced::Renderer>> {
        let mut layouts = layout.children();
        let native_layout = layouts.next().unwrap();
        layouts.next();
        let navigation_layout = layouts.next().unwrap();
        let (native_tree, rest) = tree.children.split_at_mut(1);
        let mut overlays = Vec::new();
        if let Some(element) = self.native.as_widget_mut().overlay(
            &mut native_tree[0],
            native_layout,
            renderer,
            viewport,
            translation,
        ) {
            overlays.push(element);
        }
        if self.placement.visible {
            if let Some(element) = self.navigation.as_widget_mut().overlay(
                &mut rest[1],
                navigation_layout,
                renderer,
                viewport,
                translation,
            ) {
                overlays.push(element);
            }
        }
        (!overlays.is_empty()).then(|| overlay::Group::with_children(overlays).overlay())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use iced::advanced::widget::tree;
    struct Retained;
    impl Widget<(), iced::Theme, iced::Renderer> for Retained {
        fn tag(&self) -> tree::Tag {
            tree::Tag::of::<u64>()
        }
        fn state(&self) -> tree::State {
            tree::State::new(0_u64)
        }
        fn size(&self) -> Size<Length> {
            Size::new(Length::Fill, Length::Fill)
        }
        fn layout(
            &mut self,
            _: &mut Tree,
            _: &iced::Renderer,
            limits: &layout::Limits,
        ) -> layout::Node {
            layout::Node::new(limits.max())
        }
        fn draw(
            &self,
            _: &Tree,
            _: &mut iced::Renderer,
            _: &iced::Theme,
            _: &renderer::Style,
            _: Layout<'_>,
            _: mouse::Cursor,
            _: &Rectangle,
        ) {
        }
    }
    fn view(visible: bool, drawer: bool) -> Element<'static, ()> {
        surface(
            Element::new(Retained),
            Element::new(Retained),
            Element::new(Retained),
            Placement {
                panel: Rectangle::new(iced::Point::new(48.0, 42.0), Size::new(312.0, 600.0)),
                backdrop: Rectangle::new(iced::Point::new(48.0, 42.0), Size::new(700.0, 600.0)),
                visible,
                drawer,
            },
        )
    }
    #[test]
    fn navigation_replaces_unrelated_stateless_trees_when_overlay_layers_change() {
        use iced::widget::{Space, Stack};

        // Opening a context menu inserts a Stack above the workspace. Its
        // stateless descendants must never be mistaken for this surface's
        // fixed native/backdrop/navigation subtree when Iced reconciles it.
        for count in [1, 0, 2, 3, 4] {
            let foreign: Element<'_, ()> =
                Stack::with_children((0..count).map(|_| Element::from(Space::new()))).into();
            let mut tree = Tree::new(foreign.as_widget());
            tree.diff(view(true, false).as_widget());
            assert_eq!(tree.children.len(), 3);
            for child in &tree.children {
                assert_eq!(*child.state.downcast_ref::<u64>(), 0);
            }
            tree.diff(foreign.as_widget());
            assert_eq!(tree.children.len(), count);
        }
    }

    #[test]
    fn context_menu_open_close_preserves_native_and_navigation_state() {
        use iced::widget::Space;

        let menu_view = |open: bool, visible, drawer| {
            crate::widget_identity_scope::transient_overlay(
                view(visible, drawer),
                open.then(|| Element::from(Space::new())),
            )
        };
        let mut tree = Tree::new(menu_view(false, true, false).as_widget());
        for (index, child) in tree.children[0].children.iter_mut().enumerate() {
            *child.state.downcast_mut::<u64>() = 31 + index as u64;
        }
        for (open, visible, drawer) in [
            (true, true, false),
            (false, true, false),
            (true, false, false),
            (false, false, false),
            (true, true, true),
            (false, true, true),
        ] {
            tree.diff(menu_view(open, visible, drawer).as_widget());
            assert_eq!(tree.children.len(), 1 + usize::from(open));
            assert_eq!(tree.children[0].children.len(), 3);
            for (index, child) in tree.children[0].children.iter().enumerate() {
                assert_eq!(*child.state.downcast_ref::<u64>(), 31 + index as u64);
            }
        }
    }

    #[test]
    fn dock_drawer_and_hidden_transitions_preserve_the_same_native_and_navigation_state() {
        let first = view(true, false);
        let mut tree = Tree::new(first.as_widget());
        for (index, child) in tree.children.iter_mut().enumerate() {
            *child.state.downcast_mut::<u64>() = 31 + index as u64;
        }
        for (visible, drawer) in [
            (false, false),
            (true, true),
            (true, false),
            (true, true),
            (false, false),
        ] {
            tree.diff(view(visible, drawer).as_widget());
            assert_eq!(tree.children.len(), 3);
            for (index, child) in tree.children.iter().enumerate() {
                assert_eq!(*child.state.downcast_ref::<u64>(), 31 + index as u64);
            }
        }
    }
    #[test]
    fn covered_touch_and_wheel_stay_in_the_drawer_without_a_mouse_cursor() {
        let bounds = Rectangle::new(iced::Point::new(48.0, 42.0), Size::new(492.0, 600.0));
        let touch = Event::Touch(iced::touch::Event::FingerPressed {
            id: iced::touch::Finger(1),
            position: iced::Point::new(400.0, 200.0),
        });
        assert!(covered_pointer(&touch, mouse::Cursor::Unavailable, bounds));
        let rail = Event::Touch(iced::touch::Event::FingerPressed {
            id: iced::touch::Finger(1),
            position: iced::Point::new(24.0, 200.0),
        });
        assert!(!covered_pointer(&rail, mouse::Cursor::Unavailable, bounds));
        let wheel = Event::Mouse(mouse::Event::WheelScrolled {
            delta: mouse::ScrollDelta::Lines { x: 0.0, y: -1.0 },
        });
        assert!(covered_pointer(
            &wheel,
            mouse::Cursor::Available(iced::Point::new(400.0, 200.0)),
            bounds
        ));
    }
}
