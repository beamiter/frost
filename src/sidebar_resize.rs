//! Native sidebar grips and one pointer-only capture surface. Keyboard focus,
//! IME ownership and retained terminal widgets are left untouched during drag.
use iced::advanced::{layout, mouse, renderer, widget::tree, Clipboard, Layout, Shell, Widget};
use iced::{Element, Event, Length, Point, Rectangle, Size};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Side {
    Navigation,
    Inspector,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct Drag {
    pub side: Side,
    start_x: f32,
    start_width: f32,
    pub moved: bool,
    available_at_start: Option<f32>,
}

impl Drag {
    pub fn new(side: Side, pointer: Point, width: f32) -> Option<Self> {
        (pointer.x.is_finite() && width.is_finite() && width > 0.0).then_some(Self {
            side,
            start_x: pointer.x,
            start_width: width,
            moved: false,
            available_at_start: None,
        })
    }

    /// Anchor to displayed width, not a requested width that may currently be
    /// clamped. A press/release without actual motion never rewrites a setting.
    pub fn width_at(&mut self, pointer: Point, available: f32) -> Option<f32> {
        if !pointer.x.is_finite() || !available.is_finite() || available <= 0.0 {
            return None;
        }
        if !self.moved && (pointer.x - self.start_x).abs() < 1.0 {
            return None;
        }
        self.moved = true;
        // Freeze the drag budget. A temporarily constrained opposite panel
        // may expand as this one shrinks; reversing must still reach the
        // original width rather than ratchet inward on each move.
        let available = *self.available_at_start.get_or_insert(available);
        let (min, max, delta) = match self.side {
            Side::Navigation => (
                crate::glass_layout::MIN_SIDEBAR_WIDTH,
                crate::glass_layout::MAX_SIDEBAR_WIDTH,
                pointer.x - self.start_x,
            ),
            Side::Inspector => (
                crate::glass_layout::MIN_INSPECTOR_WIDTH,
                crate::glass_layout::MAX_INSPECTOR_WIDTH,
                self.start_x - pointer.x,
            ),
        };
        Some((self.start_width + delta).clamp(min.min(available), max.min(available).max(0.0)))
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum Action {
    Start(Point),
    Move(Point),
    End,
}

#[derive(Default)]
struct State {
    hovered: bool,
}

struct Surface<'a, Message> {
    width: Option<f32>,
    active: bool,
    accent: iced::Color,
    border: iced::Color,
    on_action: Box<dyn Fn(Action) -> Message + 'a>,
}

pub(crate) fn grip<'a, Message: 'a, Renderer: renderer::Renderer + 'a>(
    width: f32,
    active: bool,
    accent: iced::Color,
    border: iced::Color,
    on_action: impl Fn(Action) -> Message + 'a,
) -> Element<'a, Message, iced::Theme, Renderer> {
    Element::new(Surface {
        width: Some(width),
        active,
        accent,
        border,
        on_action: Box::new(on_action),
    })
}

pub(crate) fn capture<'a, Message: 'a, Renderer: renderer::Renderer + 'a>(
    on_action: impl Fn(Action) -> Message + 'a,
) -> Element<'a, Message, iced::Theme, Renderer> {
    Element::new(Surface {
        width: None,
        active: true,
        accent: iced::Color::TRANSPARENT,
        border: iced::Color::TRANSPARENT,
        on_action: Box::new(on_action),
    })
}

impl<Message, Renderer: renderer::Renderer> Widget<Message, iced::Theme, Renderer>
    for Surface<'_, Message>
{
    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<State>()
    }
    fn state(&self) -> tree::State {
        tree::State::new(State::default())
    }
    fn size(&self) -> Size<Length> {
        Size::new(self.width.map_or(Length::Fill, Length::Fixed), Length::Fill)
    }
    fn layout(
        &mut self,
        _: &mut tree::Tree,
        _: &Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        layout::Node::new(limits.resolve(
            self.width.map_or(Length::Fill, Length::Fixed),
            Length::Fill,
            Size::ZERO,
        ))
    }
    fn draw(
        &self,
        tree: &tree::Tree,
        renderer: &mut Renderer,
        _: &iced::Theme,
        _: &renderer::Style,
        layout: Layout<'_>,
        _: mouse::Cursor,
        _: &Rectangle,
    ) {
        if self.width.is_none() {
            return;
        }
        let hovered = tree.state.downcast_ref::<State>().hovered;
        let bounds = layout.bounds();
        let color = if self.active || hovered {
            self.accent
                .scale_alpha(if self.active { 0.9 } else { 0.65 })
        } else {
            self.border.scale_alpha(0.22)
        };
        renderer.fill_quad(
            renderer::Quad {
                bounds: Rectangle {
                    x: bounds.center_x() - 1.0,
                    y: bounds.y + 16.0_f32.min(bounds.height * 0.25),
                    width: 2.0,
                    height: (bounds.height - 32.0).max(0.0),
                },
                border: iced::border::rounded(2.0),
                ..Default::default()
            },
            color,
        );
    }
    fn update(
        &mut self,
        tree: &mut tree::Tree,
        event: &Event,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        _: &Renderer,
        _: &mut dyn Clipboard,
        shell: &mut Shell<'_, Message>,
        _: &Rectangle,
    ) {
        let state = tree.state.downcast_mut::<State>();
        let hovered = cursor.is_over(layout.bounds());
        if hovered != state.hovered {
            state.hovered = hovered;
            shell.request_redraw();
        }
        let capturing = self.width.is_none();
        let action = match event {
            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left))
                if hovered && !capturing =>
            {
                cursor.position().map(Action::Start)
            }
            Event::Touch(iced::touch::Event::FingerPressed { position, .. })
                if !capturing && layout.bounds().contains(*position) =>
            {
                Some(Action::Start(*position))
            }
            Event::Mouse(mouse::Event::CursorMoved { position }) if capturing => {
                Some(Action::Move(*position))
            }
            Event::Touch(iced::touch::Event::FingerMoved { position, .. }) if capturing => {
                Some(Action::Move(*position))
            }
            Event::Mouse(
                mouse::Event::ButtonReleased(mouse::Button::Left) | mouse::Event::CursorLeft,
            ) if capturing => Some(Action::End),
            Event::Touch(
                iced::touch::Event::FingerLifted { .. } | iced::touch::Event::FingerLost { .. },
            ) if capturing => Some(Action::End),
            Event::Window(iced::window::Event::Unfocused) if capturing => Some(Action::End),
            _ => None,
        };
        if let Some(action) = action {
            shell.publish((self.on_action)(action));
            shell.capture_event();
        } else if capturing && matches!(event, Event::Mouse(_) | Event::Touch(_)) {
            // The initiating press belongs to the grip; every trailing pointer
            // event belongs to this capture, including scroll and extra buttons.
            shell.capture_event();
        }
    }
    fn mouse_interaction(
        &self,
        _: &tree::Tree,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        _: &Rectangle,
        _: &Renderer,
    ) -> mouse::Interaction {
        if self.active || cursor.is_over(layout.bounds()) {
            mouse::Interaction::ResizingHorizontally
        } else {
            mouse::Interaction::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use iced::advanced::widget::Tree;

    fn send(
        element: &mut Element<'_, Action, iced::Theme, ()>,
        tree: &mut Tree,
        event: Event,
        pointer: Point,
    ) -> (Vec<Action>, bool) {
        let node = element
            .as_widget_mut()
            .layout(
                tree,
                &(),
                &layout::Limits::new(Size::ZERO, Size::new(300.0, 400.0)),
            )
            .move_to(Point::new(100.0, 20.0));
        let mut messages = Vec::new();
        let mut shell = Shell::new(&mut messages);
        element.as_widget_mut().update(
            tree,
            &event,
            Layout::new(&node),
            mouse::Cursor::Available(pointer),
            &(),
            &mut iced::advanced::clipboard::Null,
            &mut shell,
            &Rectangle::with_size(Size::new(800.0, 600.0)),
        );
        let captured = shell.is_event_captured();
        (messages, captured)
    }

    #[test]
    fn actual_grip_captures_only_its_press_and_reports_window_coordinates() {
        let mut grip: Element<'_, Action, iced::Theme, ()> = grip(
            11.0,
            false,
            iced::Color::WHITE,
            iced::Color::BLACK,
            |action| action,
        );
        let mut tree = Tree::new(grip.as_widget());
        let point = Point::new(105.0, 100.0);
        let (messages, captured) = send(
            &mut grip,
            &mut tree,
            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)),
            point,
        );
        assert_eq!(messages, vec![Action::Start(point)]);
        assert!(captured);
        let (messages, captured) = send(
            &mut grip,
            &mut tree,
            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)),
            Point::new(150.0, 100.0),
        );
        assert!(messages.is_empty());
        assert!(!captured, "grip must not steal a terminal press");
    }

    #[test]
    fn capture_owns_moves_and_release_outside_grip_but_leaves_keyboard_alone() {
        let mut capture: Element<'_, Action, iced::Theme, ()> = capture(|action| action);
        let mut tree = Tree::new(capture.as_widget());
        let point = Point::new(700.0, 250.0);
        let (messages, captured) = send(
            &mut capture,
            &mut tree,
            Event::Mouse(mouse::Event::CursorMoved { position: point }),
            point,
        );
        assert_eq!(messages, vec![Action::Move(point)]);
        assert!(captured, "drag cannot turn into terminal selection");
        let (messages, captured) = send(
            &mut capture,
            &mut tree,
            Event::Keyboard(iced::keyboard::Event::ModifiersChanged(
                iced::keyboard::Modifiers::CTRL,
            )),
            point,
        );
        assert!(messages.is_empty());
        assert!(!captured, "resize must not take keyboard/editor focus");
        for event in [
            Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)),
            Event::Mouse(mouse::Event::CursorLeft),
            Event::Window(iced::window::Event::Unfocused),
        ] {
            let (messages, captured) = send(&mut capture, &mut tree, event, point);
            assert_eq!(messages, vec![Action::End]);
            assert!(captured, "terminal cannot receive the trailing release");
        }
    }

    #[test]
    fn constrained_pair_drag_tracks_pointer_and_reverses_without_rebalancing_the_other_preference()
    {
        use crate::glass_layout::{Layout, MIN_CENTER_WIDTH};
        let inspector_preference = Some(520.0);
        let original =
            Layout::with_panels(1180.0, 704.0, Some(480.0), inspector_preference, true, true);
        let navigation = original.navigation.unwrap();
        assert!((original.center.width - MIN_CENTER_WIDTH).abs() < 0.01);
        let start = Point::new(navigation.x + navigation.width + 5.0, 200.0);
        let mut drag = Drag::new(Side::Navigation, start, navigation.width).unwrap();
        // No free center width: pushing right must stay put, never shrink.
        let width = drag
            .width_at(Point::new(start.x + 20.0, start.y), navigation.width)
            .unwrap();
        let pushed =
            Layout::with_panels(1180.0, 704.0, Some(width), inspector_preference, true, true);
        assert_eq!(pushed.navigation.unwrap().width, navigation.width);
        // Shrinking follows the pointer exactly; returning can reclaim the
        // original width even if the constrained opposite panel expanded.
        let width = drag
            .width_at(Point::new(start.x - 50.0, start.y), navigation.width)
            .unwrap();
        let shrunk =
            Layout::with_panels(1180.0, 704.0, Some(width), inspector_preference, true, true);
        assert_eq!(shrunk.navigation.unwrap().width, navigation.width - 50.0);
        assert!(shrunk.center.width >= MIN_CENTER_WIDTH);
        let width = drag
            .width_at(start, shrunk.navigation.unwrap().width)
            .unwrap();
        let restored =
            Layout::with_panels(1180.0, 704.0, Some(width), inspector_preference, true, true);
        assert_eq!(restored, original);
        assert_eq!(inspector_preference, Some(520.0));
        // Right edge has the inverse delta and cannot consume the center's
        // protected width or change the navigation request.
        let right = original.inspector.unwrap();
        let start = Point::new(right.x - 5.0, 200.0);
        let mut drag = Drag::new(Side::Inspector, start, right.width).unwrap();
        let width = drag
            .width_at(Point::new(start.x + 25.0, start.y), right.width)
            .unwrap();
        let shrunk = Layout::with_panels(1180.0, 704.0, Some(480.0), Some(width), true, true);
        assert_eq!(shrunk.inspector.unwrap().width, right.width - 25.0);
        assert_eq!(shrunk.navigation.unwrap().width, navigation.width);
        let width = drag.width_at(start, right.width).unwrap();
        assert_eq!(
            Layout::with_panels(1180.0, 704.0, Some(480.0), Some(width), true, true),
            original
        );
    }

    #[test]
    fn drag_anchors_effective_width_uses_opposite_right_delta_and_preserves_no_motion() {
        let mut left = Drag::new(Side::Navigation, Point::new(300.0, 100.0), 240.0).unwrap();
        assert_eq!(left.width_at(Point::new(300.0, 100.0), 600.0), None);
        assert!(
            !left.moved,
            "a click cannot replace a larger preferred width"
        );
        assert_eq!(left.width_at(Point::new(340.0, 100.0), 600.0), Some(280.0));
        assert_eq!(left.width_at(Point::new(360.0, 100.0), 600.0), Some(300.0));
        assert_eq!(left.width_at(Point::new(300.0, 100.0), 600.0), Some(240.0));
        assert_eq!(
            left.width_at(Point::new(900.0, 100.0), 340.0),
            Some(crate::glass_layout::MAX_SIDEBAR_WIDTH)
        );
        let mut constrained = Drag::new(Side::Navigation, Point::new(300.0, 100.0), 240.0).unwrap();
        assert_eq!(
            constrained.width_at(Point::new(900.0, 100.0), 340.0),
            Some(340.0)
        );
        let mut right = Drag::new(Side::Inspector, Point::new(900.0, 100.0), 350.0).unwrap();
        assert_eq!(right.width_at(Point::new(850.0, 100.0), 600.0), Some(400.0));
        assert_eq!(
            right.width_at(Point::new(1000.0, 100.0), 600.0),
            Some(crate::glass_layout::MIN_INSPECTOR_WIDTH)
        );
        assert_eq!(right.width_at(Point::new(f32::NAN, 0.0), 600.0), None);
    }
}
