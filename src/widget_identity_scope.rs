//! Reset an explicitly scoped form or row's retained state at its identity boundary.
//! Remote editor, workflow forms and palette items use it; unrelated widgets stay intact.
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
    fn companion_compact_toggle_captures_click_without_greeting_or_terminal_delivery() {
        use crate::{
            companion_compact_slot,
            organism::{AttentionFacts, Organism},
            Message,
        };
        let mut organism = Organism::default();
        organism.set_enabled(true);
        organism.prime_session(1, false);
        organism.set_remote(1, false);
        organism.sync_live_attention_owner(Some(1));
        let offer = organism.live_greeting_offer(1).unwrap();
        let area = iced::widget::mouse_area(Space::new().width(64).height(22))
            .on_enter(Message::OrganismHoverEnter(1, offer))
            .on_exit(Message::OrganismHoverExit(1, offer));
        let toggle = button(Space::new())
            .width(20)
            .height(22)
            .padding(0)
            .on_press(Message::SetOrganismExpanded(true));
        let mut element: Element<'_, Message, iced::Theme, TestRenderer> =
            companion_compact_slot(area.into(), toggle.into()).into();
        let mut tree = Tree::new(element.as_widget());
        let node = element.as_widget_mut().layout(
            &mut tree,
            &TestRenderer,
            &layout::Limits::new(Size::ZERO, Size::new(100.0, 100.0)),
        );
        let mut messages = Vec::new();
        let arrow = iced::Point::new(74.0, 11.0);
        for event in [
            mouse::Event::ButtonPressed(mouse::Button::Left),
            mouse::Event::ButtonReleased(mouse::Button::Left),
        ] {
            let mut shell = Shell::new(&mut messages);
            element.as_widget_mut().update(
                &mut tree,
                &Event::Mouse(event),
                Layout::new(&node),
                mouse::Cursor::Available(arrow),
                &TestRenderer,
                &mut iced::advanced::clipboard::Null,
                &mut shell,
                &Rectangle::with_size(Size::new(100.0, 100.0)),
            );
            // Test native widget capture here; the source/geometry regression
            // separately verifies that chrome is not rerouted into terminal input.
            assert!(shell.is_event_captured());
        }
        assert_eq!(messages.len(), 1);
        assert!(matches!(messages[0], Message::SetOrganismExpanded(true)));
        assert!(organism
            .live_greeting_snapshot(AttentionFacts::known_idle_for_test(true, true))
            .unwrap()
            .candidate
            .is_none());
        messages.clear();
        for position in [iced::Point::new(10.0, 11.0), arrow] {
            element.as_widget_mut().update(
                &mut tree,
                &Event::Mouse(mouse::Event::CursorMoved { position }),
                Layout::new(&node),
                mouse::Cursor::Available(position),
                &TestRenderer,
                &mut iced::advanced::clipboard::Null,
                &mut Shell::new(&mut messages),
                &Rectangle::with_size(Size::new(100.0, 100.0)),
            );
        }
        assert_eq!(messages.len(), 2);
        assert!(matches!(messages[0], Message::OrganismHoverEnter(1, _)));
        assert!(matches!(messages[1], Message::OrganismHoverExit(1, _)));
    }

    #[test]
    fn wide_companion_reserves_glyph_and_toggle_beside_overflowing_status() {
        use crate::{companion_compact_slot, companion_status_with_slot, Message};
        for width in [640.0, 800.0, 1024.0] {
            for height in [1.0, 8.0, 22.0] {
                for available in [false, true] {
                    for expanded in [false, true] {
                        let status = iced::widget::row![
                            Space::new().width(4000).height(22),
                            Space::new().width(Length::Fill),
                            Space::new().width(4000).height(22),
                        ]
                        .spacing(12)
                        .align_y(iced::Alignment::Center);
                        let glyph = Space::new().width(64).height(22).into();
                        let toggle = button(Space::new())
                            .width(20)
                            .height(22)
                            .padding(0)
                            .on_press_maybe(
                                available.then_some(Message::SetOrganismExpanded(!expanded)),
                            );
                        let slot = companion_compact_slot(glyph, toggle.into()).into();
                        let mut element: Element<'_, Message, iced::Theme, TestRenderer> =
                            companion_status_with_slot(status.into(), slot, 12);
                        let mut tree = Tree::new(element.as_widget());
                        let viewport = Rectangle::with_size(Size::new(width - 20.0, height));
                        let node = element.as_widget_mut().layout(
                            &mut tree,
                            &TestRenderer,
                            &layout::Limits::new(Size::ZERO, viewport.size()),
                        );
                        let content = node.children()[0].bounds();
                        let slot = Layout::new(&node).children().nth(1).unwrap();
                        let slot_row = slot.children().next().unwrap();
                        let glyph = slot_row.children().next().unwrap().bounds();
                        let arrow = slot_row.children().nth(1).unwrap().bounds();
                        assert_eq!(node.bounds().width, width - 20.0);
                        assert_eq!(node.bounds().height, height);
                        assert!(content.x + content.width <= slot.bounds().x);
                        assert_eq!(slot.bounds().width, 84.0);
                        assert_eq!(glyph.width, 64.0);
                        assert_eq!(arrow.width, 20.0);
                        assert_eq!(arrow.height, height);
                        assert!(arrow.x + arrow.width <= width - 20.0);
                        let mut messages = Vec::new();
                        for _ in 0..2 {
                            for event in [
                                mouse::Event::ButtonPressed(mouse::Button::Left),
                                mouse::Event::ButtonReleased(mouse::Button::Left),
                            ] {
                                let mut shell = Shell::new(&mut messages);
                                element.as_widget_mut().update(
                                    &mut tree,
                                    &Event::Mouse(event),
                                    Layout::new(&node),
                                    mouse::Cursor::Available(arrow.center()),
                                    &TestRenderer,
                                    &mut iced::advanced::clipboard::Null,
                                    &mut shell,
                                    &viewport,
                                );
                                assert_eq!(shell.is_event_captured(), available);
                            }
                        }
                        assert_eq!(messages.len(), if available { 2 } else { 0 });
                        assert!(messages.iter().all(|message| matches!(message,
                            Message::SetOrganismExpanded(next) if *next == !expanded
                        )));
                    }
                }
            }
        }
    }

    #[test]
    fn narrow_reopen_reserves_clickable_control_beside_overflowing_status() {
        use crate::{companion_status_with_slot, Message};
        for width in [220.0, 359.99, 360.0, 639.99] {
            for available in [false, true] {
                let status = Space::new().width(4000).height(22).into();
                let toggle = button(Space::new())
                    .width(20)
                    .height(22)
                    .padding(0)
                    .on_press_maybe(available.then_some(Message::SetOrganismExpanded(true)));
                let mut element: Element<'_, Message, iced::Theme, TestRenderer> =
                    companion_status_with_slot(status, toggle.into(), 8);
                let mut tree = Tree::new(element.as_widget());
                let node = element.as_widget_mut().layout(
                    &mut tree,
                    &TestRenderer,
                    &layout::Limits::new(Size::ZERO, Size::new(width - 20.0, 22.0)),
                );
                let content = node.children()[0].bounds();
                let arrow = node.children()[1].bounds();
                assert_eq!(node.bounds().width, width - 20.0);
                assert!(content.x + content.width <= arrow.x);
                assert_eq!(arrow.width, 20.0);
                assert_eq!(arrow.height, 22.0);
                assert!(arrow.x + arrow.width <= width - 20.0);
                let mut messages = Vec::new();
                for _ in 0..2 {
                    for event in [
                        mouse::Event::ButtonPressed(mouse::Button::Left),
                        mouse::Event::ButtonReleased(mouse::Button::Left),
                    ] {
                        let mut shell = Shell::new(&mut messages);
                        element.as_widget_mut().update(
                            &mut tree,
                            &Event::Mouse(event),
                            Layout::new(&node),
                            mouse::Cursor::Available(arrow.center()),
                            &TestRenderer,
                            &mut iced::advanced::clipboard::Null,
                            &mut shell,
                            &Rectangle::with_size(Size::new(width - 20.0, 22.0)),
                        );
                        assert_eq!(shell.is_event_captured(), available);
                    }
                }
                assert_eq!(messages.len(), if available { 2 } else { 0 });
                assert!(messages
                    .iter()
                    .all(|message| matches!(message, Message::SetOrganismExpanded(true))));
                // The neighboring status content and its clipped tail are not
                // an alias of the control, even with repeated pointer events.
                messages.clear();
                for event in [
                    mouse::Event::ButtonPressed(mouse::Button::Left),
                    mouse::Event::ButtonReleased(mouse::Button::Left),
                ] {
                    element.as_widget_mut().update(
                        &mut tree,
                        &Event::Mouse(event),
                        Layout::new(&node),
                        mouse::Cursor::Available(iced::Point::new(10.0, 11.0)),
                        &TestRenderer,
                        &mut iced::advanced::clipboard::Null,
                        &mut Shell::new(&mut messages),
                        &Rectangle::with_size(Size::new(width - 20.0, 22.0)),
                    );
                }
                assert!(messages.is_empty());
            }
        }
    }

    #[test]
    fn companion_card_native_layout_keeps_body_and_collapse_in_reserved_chrome() {
        use crate::{companion_card_content, companion_strip_height, organism_layout, Message};
        for width in [220.0, 359.99, 360.0, 639.99, 640.0, 1024.0] {
            let layout = organism_layout(width, true, true, true);
            let body: Element<'_, Message, iced::Theme, TestRenderer> =
                Space::new().width(94).height(48).into();
            let detail = layout.explanation.then(|| {
                iced::widget::container(Space::new().width(4000).height(4000))
                    .width(Length::Fill)
                    .height(Length::Fill)
                    .into()
            });
            let toggle = button(Space::new())
                .width(20)
                .height(22)
                .padding(0)
                .on_press(Message::SetOrganismExpanded(false));
            let mut card: Element<'_, Message, iced::Theme, TestRenderer> =
                companion_card_content(body, detail, toggle.into()).into();
            let mut tree = Tree::new(card.as_widget());
            let node = card.as_widget_mut().layout(
                &mut tree,
                &TestRenderer,
                &layout::Limits::new(
                    Size::ZERO,
                    Size::new(width - 16.0, companion_strip_height(true, true, true) - 6.0),
                ),
            );
            let row = &node.children()[0];
            let body = &row.children()[0];
            let detail = &row.children()[1];
            let toggle = &row.children()[2];
            assert_eq!(body.bounds().width, crate::COMPANION_CARD_SPRITE_WIDTH);
            assert!(body.bounds().height >= 48.0);
            assert_eq!(body.children()[0].bounds().height, 48.0);
            assert!(body.bounds().x + body.bounds().width <= detail.bounds().x);
            assert!(detail.bounds().x + detail.bounds().width <= toggle.bounds().x);
            assert_eq!(toggle.bounds().width, 20.0);
            assert!(toggle.bounds().x + toggle.bounds().width <= row.bounds().width + 0.01);
            assert!(node.bounds().height <= 82.0);
        }
    }

    #[test]
    fn mouse_area_queues_same_offer_enter_and_exit_before_view_rebuild() {
        use crate::{
            organism::{AttentionFacts, Organism},
            Message,
        };
        let mut organism = Organism::default();
        organism.set_enabled(true);
        for id in [1, 2] {
            organism.prime_session(id, false);
            organism.set_remote(id, false);
        }
        organism.sync_live_attention_owner(Some(1));
        let offer = organism.live_greeting_offer(1).unwrap();
        let exit = organism.live_greeting_exit_reference(1).unwrap();
        assert_eq!(offer, exit);
        let mut area: Element<'_, Message, iced::Theme, TestRenderer> =
            iced::widget::mouse_area(Space::new().width(84).height(30))
                .on_enter(Message::OrganismHoverEnter(1, offer))
                .on_exit(Message::OrganismHoverExit(1, exit))
                .into();
        let mut tree = Tree::new(area.as_widget());
        let node = area.as_widget_mut().layout(
            &mut tree,
            &TestRenderer,
            &layout::Limits::new(Size::ZERO, Size::new(100.0, 100.0)),
        );
        let mut messages = Vec::new();
        // Both native edges are dispatched against the same tree and callbacks.
        for position in [iced::Point::new(10.0, 10.0), iced::Point::new(95.0, 95.0)] {
            area.as_widget_mut().update(
                &mut tree,
                &Event::Mouse(mouse::Event::CursorMoved { position }),
                Layout::new(&node),
                mouse::Cursor::Available(position),
                &TestRenderer,
                &mut iced::advanced::clipboard::Null,
                &mut Shell::new(&mut messages),
                &Rectangle::with_size(Size::new(100.0, 100.0)),
            );
        }
        assert_eq!(messages.len(), 2);
        match messages[0] {
            Message::OrganismHoverEnter(id, ticket) => {
                assert_eq!((id, ticket), (1, offer));
                organism.enter_live_greeting(
                    id,
                    ticket,
                    AttentionFacts::known_idle_for_test(true, true),
                );
            }
            _ => panic!("expected native enter"),
        }
        assert!(organism
            .live_greeting_snapshot(AttentionFacts::known_idle_for_test(true, true))
            .unwrap()
            .candidate
            .is_some());
        match messages[1] {
            Message::OrganismHoverExit(id, ticket) => {
                assert_eq!((id, ticket), (1, offer));
                organism.exit_live_greeting(
                    id,
                    ticket,
                    AttentionFacts::known_idle_for_test(true, true),
                );
            }
            _ => panic!("expected native exit"),
        }
        let snapshot = organism
            .live_greeting_snapshot(AttentionFacts::known_idle_for_test(true, true))
            .unwrap();
        assert!(snapshot.entry.is_none());
        assert!(snapshot.candidate.is_none());
        organism.sync_live_attention_owner(Some(2));
        let new_offer = organism.live_greeting_offer(2).unwrap();
        organism.enter_live_greeting(1, offer, AttentionFacts::known_idle_for_test(true, true));
        assert_eq!(organism.live_greeting_offer(2), Some(new_offer));
        assert!(organism
            .live_greeting_snapshot(AttentionFacts::known_idle_for_test(true, true))
            .unwrap()
            .candidate
            .is_none());
    }

    // Exercise the real pinned Container draw boundary without fonts or a GPU.
    struct ViewportSpy<'a>(&'a std::cell::Cell<Option<Rectangle>>);

    impl<Message> Widget<Message, iced::Theme, TestRenderer> for ViewportSpy<'_> {
        fn size(&self) -> Size<Length> {
            Size::new(Length::Fill, Length::Fill)
        }

        fn layout(
            &mut self,
            _tree: &mut Tree,
            _renderer: &TestRenderer,
            limits: &layout::Limits,
        ) -> layout::Node {
            layout::Node::new(limits.max())
        }

        fn draw(
            &self,
            _tree: &Tree,
            _renderer: &mut TestRenderer,
            _theme: &iced::Theme,
            _style: &renderer::Style,
            _layout: Layout<'_>,
            _cursor: mouse::Cursor,
            viewport: &Rectangle,
        ) {
            self.0.set(Some(*viewport));
        }
    }

    #[test]
    fn companion_card_short_viewports_clip_body_and_details_without_terminal_overlap() {
        use crate::{companion_card_content, Message};
        for height in [0.0, 1.0, 8.0, 40.0, 88.0] {
            let body_viewport = std::cell::Cell::new(None);
            let detail_viewport = std::cell::Cell::new(None);
            let body = Element::new(ViewportSpy(&body_viewport));
            let detail = Element::new(ViewportSpy(&detail_viewport));
            let toggle = button(Space::new())
                .width(20)
                .height(22)
                .padding(0)
                .on_press(Message::SetOrganismExpanded(false));
            let card = companion_card_content(body, Some(detail), toggle.into());
            let mut strip: Element<'_, Message, iced::Theme, TestRenderer> =
                iced::widget::container(card)
                    .width(640)
                    .height(88)
                    .padding([3.0, 8.0])
                    .clip(true)
                    .into();
            let mut tree = Tree::new(strip.as_widget());
            let node = strip.as_widget_mut().layout(
                &mut tree,
                &TestRenderer,
                &layout::Limits::new(Size::ZERO, Size::new(640.0, height)),
            );
            strip.as_widget().draw(
                &tree,
                &mut TestRenderer,
                &iced::Theme::Dark,
                &renderer::Style::default(),
                Layout::new(&node),
                mouse::Cursor::Unavailable,
                &Rectangle::with_size(Size::INFINITE),
            );
            assert!(node.bounds().height <= height);
            if height == 88.0 {
                assert!(body_viewport.get().is_some());
                assert!(detail_viewport.get().is_some());
            }
            for seen in [body_viewport.get(), detail_viewport.get()]
                .into_iter()
                .flatten()
            {
                assert!(seen.x >= 0.0 && seen.y >= 0.0);
                assert!(seen.x + seen.width <= node.bounds().width + 0.01);
                assert!(seen.y + seen.height <= node.bounds().height + 0.01);
            }
        }
    }

    fn container_viewport(
        clip: bool,
        available_height: f32,
        viewport: Rectangle,
    ) -> (Rectangle, Rectangle, Option<Rectangle>) {
        let received = std::cell::Cell::new(None);
        let child: Element<'_, (), iced::Theme, TestRenderer> =
            Element::new(ViewportSpy(&received));
        let mut container: Element<'_, (), iced::Theme, TestRenderer> =
            iced::widget::container(child)
                .width(200)
                .height(88)
                .padding(8)
                .clip(clip)
                .into();
        let mut tree = Tree::new(container.as_widget());
        let node = container
            .as_widget_mut()
            .layout(
                &mut tree,
                &TestRenderer,
                &layout::Limits::new(Size::ZERO, Size::new(220.0, available_height)),
            )
            .move_to(iced::Point::new(10.0, 20.0));
        container.as_widget().draw(
            &tree,
            &mut TestRenderer,
            &iced::Theme::Dark,
            &renderer::Style::default(),
            Layout::new(&node),
            mouse::Cursor::Unavailable,
            &viewport,
        );
        (node.bounds(), node.children()[0].bounds(), received.get())
    }

    #[test]
    fn clipped_container_bounds_infinite_tooltip_viewport_without_changing_layout() {
        let infinite = Rectangle::with_size(Size::INFINITE);
        for height in [1.0, 8.0, 88.0, 160.0] {
            let (bounds, child, clipped) = container_viewport(true, height, infinite);
            let (plain_bounds, plain_child, plain) = container_viewport(false, height, infinite);
            assert_eq!((bounds, child), (plain_bounds, plain_child));
            assert_eq!(bounds.height, height.min(88.0));
            assert_eq!(clipped, Some(bounds));
            assert_eq!(plain, Some(infinite));
        }
    }

    #[test]
    fn clipped_container_intersects_finite_parent_and_skips_outside_draw() {
        let viewport = Rectangle {
            x: 15.0,
            y: 22.0,
            width: 80.0,
            height: 3.0,
        };
        let (bounds, _, received) = container_viewport(true, 8.0, viewport);
        assert_eq!(received, bounds.intersection(&viewport));
        assert_eq!(received, Some(viewport));
        let outside = Rectangle {
            x: 500.0,
            y: 500.0,
            width: 10.0,
            height: 10.0,
        };
        assert_eq!(container_viewport(true, 88.0, outside).2, None);
    }

    fn preview_button(
        sequence: &crate::organism_preview::PreviewSequence,
        action: crate::organism_preview::PreviewAction,
        message: u8,
    ) -> Element<'static, u8, iced::Theme, TestRenderer> {
        scope(
            sequence.control_epoch(),
            scope(
                sequence.action_identity(action),
                button(Space::new().width(40).height(30)).on_press(message),
            ),
        )
    }

    #[test]
    fn preview_pause_and_hello_press_cannot_transfer_across_control_revisions() {
        use crate::organism_preview::{PreviewAction, PreviewSequence};
        use std::time::Duration;
        for old_action in [PreviewAction::Pause, PreviewAction::Hello] {
            let mut sequence = PreviewSequence::default();
            if old_action == PreviewAction::Pause {
                sequence.start(Duration::ZERO);
            }
            let mut old = preview_button(&sequence, old_action, 1);
            let mut tree = Tree::new(old.as_widget());
            assert!(send(
                &mut old,
                &mut tree,
                mouse::Event::ButtonPressed(mouse::Button::Left)
            )
            .is_empty());
            let next_action = if old_action == PreviewAction::Pause {
                sequence.pause(Duration::from_millis(500));
                PreviewAction::Resume
            } else {
                sequence.start(Duration::ZERO);
                sequence.stop();
                PreviewAction::Hello
            };
            let mut next = preview_button(&sequence, next_action, 2);
            tree.diff(next.as_widget());
            assert!(send(
                &mut next,
                &mut tree,
                mouse::Event::ButtonReleased(mouse::Button::Left)
            )
            .is_empty());
            assert!(send(
                &mut next,
                &mut tree,
                mouse::Event::ButtonPressed(mouse::Button::Left),
            )
            .is_empty());
            assert_eq!(
                send(
                    &mut next,
                    &mut tree,
                    mouse::Event::ButtonReleased(mouse::Button::Left)
                ),
                [2]
            );
        }
    }

    #[test]
    fn naturally_finished_demo_stop_press_cannot_become_play_without_epoch_mutation() {
        use crate::organism_preview::{PreviewAction, PreviewSequence};
        use std::time::Duration;
        let mut sequence = PreviewSequence::default();
        sequence.start(Duration::ZERO);
        let epoch = sequence.control_epoch();
        let mut old = preview_button(&sequence, PreviewAction::Stop, 1);
        let mut tree = Tree::new(old.as_widget());
        assert!(send(
            &mut old,
            &mut tree,
            mouse::Event::ButtonPressed(mouse::Button::Left),
        )
        .is_empty());
        assert_eq!(sequence.sample(Duration::from_secs(10)), None);
        assert!(Arc::ptr_eq(&epoch, &sequence.control_epoch()));
        let mut next = preview_button(&sequence, PreviewAction::Play, 2);
        tree.diff(next.as_widget());
        assert!(send(
            &mut next,
            &mut tree,
            mouse::Event::ButtonReleased(mouse::Button::Left)
        )
        .is_empty());
    }

    fn first_palette_button(
        palette: &crate::command_palette::PaletteState,
    ) -> Element<'static, u8, iced::Theme, TestRenderer> {
        let (index, _) = palette.filtered()[0];
        scope(
            palette.row_identity(index).unwrap(),
            button(Space::new().width(40).height(30)).on_press(index as u8),
        )
    }

    #[test]
    fn palette_filter_replacement_cannot_inherit_another_actions_pressed_button() {
        let mut palette = crate::command_palette::PaletteState::new();
        palette.open();
        let old_index = palette.filtered()[0].0;
        let mut old = first_palette_button(&palette);
        let mut tree = Tree::new(old.as_widget());
        assert!(send(
            &mut old,
            &mut tree,
            mouse::Event::ButtonPressed(mouse::Button::Left)
        )
        .is_empty());
        palette.set_query("hello");
        let new_index = palette.filtered()[0].0;
        assert_ne!(old_index, new_index);
        let mut replacement = first_palette_button(&palette);
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
            [new_index as u8]
        );
    }

    #[test]
    fn palette_same_item_retains_an_ordinary_press_across_query_redraw() {
        let mut palette = crate::command_palette::PaletteState::new();
        palette.open();
        palette.set_query("Greet companion");
        let index = palette.filtered()[0].0;
        let mut old = first_palette_button(&palette);
        let mut tree = Tree::new(old.as_widget());
        assert!(send(
            &mut old,
            &mut tree,
            mouse::Event::ButtonPressed(mouse::Button::Left)
        )
        .is_empty());
        palette.set_query("hello");
        assert_eq!(palette.filtered()[0].0, index);
        let mut redraw = first_palette_button(&palette);
        tree.diff(redraw.as_widget());
        assert_eq!(
            send(
                &mut redraw,
                &mut tree,
                mouse::Event::ButtonReleased(mouse::Button::Left)
            ),
            [index as u8]
        );
    }

    #[test]
    fn palette_query_preserves_outer_state_but_new_opening_resets_cached_subtree() {
        let mut palette = crate::command_palette::PaletteState::new();
        palette.open();
        let build = |palette: &crate::command_palette::PaletteState| -> Element<'static, u8, iced::Theme, TestRenderer> {
            scope(palette.epoch(), iced::widget::column![
                Space::new().width(20).height(20),
                first_palette_button(palette),
            ])
        };
        let old = build(&palette);
        let mut tree = Tree::new(old.as_widget());
        tree.children[0].children[0].state = tree::State::new(73_usize);
        palette.set_query("hello");
        let edited = build(&palette);
        tree.diff(edited.as_widget());
        assert_eq!(
            *tree.children[0].children[0].state.downcast_ref::<usize>(),
            73
        );
        palette.close();
        palette.open();
        let reopened = build(&palette);
        tree.diff(reopened.as_widget());
        assert!(matches!(
            tree.children[0].children[0].state,
            tree::State::None
        ));
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
