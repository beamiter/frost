// Headless event tests for the production Find widget and stable reveal guard.
use super::*;
use iced::advanced::{layout, mouse, text as advanced_text, widget::Tree, Layout, Shell};
use iced::{Element, Event, Rectangle, Size};

type InputState = text_input::State<<() as advanced_text::Renderer>::Paragraph>;

fn enter(modifiers: keyboard::Modifiers, repeat: bool, pressed: bool) -> Event {
    let key = keyboard::Key::Named(keyboard::key::Named::Enter);
    let physical_key = keyboard::key::Physical::Code(keyboard::key::Code::Enter);
    Event::Keyboard(if pressed {
        keyboard::Event::KeyPressed {
            key: key.clone(),
            modified_key: key,
            physical_key,
            location: keyboard::Location::Standard,
            modifiers,
            text: None,
            repeat,
        }
    } else {
        keyboard::Event::KeyReleased {
            key: key.clone(),
            modified_key: key,
            physical_key,
            location: keyboard::Location::Standard,
            modifiers,
        }
    })
}

struct FindWidget {
    element: Element<'static, Message, iced::Theme, ()>,
    tree: Tree,
    node: layout::Node,
    viewport: Rectangle,
    target: Option<SearchRevealTarget>,
}

impl FindWidget {
    fn element(
        target: Option<SearchRevealTarget>,
        enabled: bool,
    ) -> Element<'static, Message, iced::Theme, ()> {
        let callback = target.map(|target| {
            Box::new(move |event| Message::SearchRevealKey(Box::new(target.clone()), event))
                as Box<dyn Fn(keyboard::Event) -> Message>
        });
        find_focus::input(
            iced::advanced::widget::Id::new("find-key-test"),
            "needle",
            text_input("", "needle")
                .width(200)
                .on_input(Message::SearchInput),
            callback,
            enabled,
        )
    }

    fn new(target: Option<SearchRevealTarget>, focused: bool) -> Self {
        let mut element = Self::element(target.clone(), true);
        let mut tree = Tree::new(element.as_widget());
        let viewport = Rectangle::with_size(Size::new(300.0, 60.0));
        let node = element.as_widget_mut().layout(
            &mut tree,
            &(),
            &layout::Limits::new(Size::ZERO, viewport.size()),
        );
        if focused {
            tree.children[0].state.downcast_mut::<InputState>().focus();
        }
        Self {
            element,
            tree,
            node,
            viewport,
            target,
        }
    }

    fn rebuild(&mut self, enabled: bool) {
        self.element = Self::element(self.target.clone(), enabled);
        self.tree.diff(self.element.as_widget());
        self.node = self.element.as_widget_mut().layout(
            &mut self.tree,
            &(),
            &layout::Limits::new(Size::ZERO, self.viewport.size()),
        );
    }

    fn focus(&mut self, id: &'static str) {
        self.element.as_widget_mut().operate(
            &mut self.tree,
            Layout::new(&self.node),
            &(),
            &mut iced::advanced::widget::operation::focusable::focus(
                iced::advanced::widget::Id::new(id),
            ),
        );
    }

    fn event(&mut self, event: Event) -> (Vec<Message>, bool) {
        self.event_at(event, mouse::Cursor::Unavailable)
    }

    fn event_at(&mut self, event: Event, cursor: mouse::Cursor) -> (Vec<Message>, bool) {
        let mut messages = Vec::new();
        let mut shell = Shell::new(&mut messages);
        self.element.as_widget_mut().update(
            &mut self.tree,
            &event,
            Layout::new(&self.node),
            cursor,
            &(),
            &mut iced::advanced::clipboard::Null,
            &mut shell,
            &self.viewport,
        );
        let captured = shell.is_event_captured();
        (messages, captured)
    }
}

fn target(fixture: &SearchRevealFixture, dirty: bool) -> Option<SearchRevealTarget> {
    search_reveal_target(
        &fixture.search,
        dirty,
        7,
        &fixture.terminal,
        &fixture.projection,
        &fixture.policy,
    )
}

#[test]
fn ctrl_enter_requires_real_focus_target_and_exact_modifiers() {
    let fixture = search_reveal_fixture();
    for focused in [false, true] {
        for available in [false, true] {
            for bits in 0..16 {
                let mut mods = keyboard::Modifiers::empty();
                for (bit, modifier) in [
                    (1, keyboard::Modifiers::CTRL),
                    (2, keyboard::Modifiers::SHIFT),
                    (4, keyboard::Modifiers::ALT),
                    (8, keyboard::Modifiers::LOGO),
                ] {
                    if bits & bit != 0 {
                        mods |= modifier;
                    }
                }
                for repeat in [false, true] {
                    let mut widget =
                        FindWidget::new(available.then(|| fixture.target.clone()), focused);
                    let (messages, captured) = widget.event(enter(mods, repeat, true));
                    let owned = focused && available && bits == 1;
                    assert_eq!(
                        captured, owned,
                        "focus={focused} target={available} modifiers={bits} repeat={repeat}"
                    );
                    assert_eq!(messages.len(), usize::from(owned && !repeat));
                    if let Some(Message::SearchRevealKey(found, _)) = messages.first() {
                        assert_eq!(found.as_ref(), &fixture.target);
                    } else {
                        assert!(messages.is_empty());
                    }
                    let (messages, captured) = widget.event(enter(mods, false, false));
                    assert!(messages.is_empty());
                    assert!(
                        !captured,
                        "existing Enter ownership receives release normally"
                    );
                }
            }
        }
    }
}

#[test]
fn keyboard_target_excludes_closed_dirty_no_match_visible_and_offscreen() {
    for scenario in ["closed", "dirty", "no_match", "visible", "offscreen"] {
        let mut fixture = search_reveal_fixture();
        let mut dirty = false;
        match scenario {
            "closed" => fixture.search.close(),
            "dirty" => dirty = true,
            "no_match" => fixture.search.matches.clear(),
            "visible" | "offscreen" => {
                fixture.policy.expand(fixture.target.zone_id);
                if scenario == "offscreen" {
                    fixture.terminal.on_resize(40, 1);
                }
                fixture.projection = fixture.terminal.get_projected_viewport_with_state(
                    true,
                    &fixture.policy,
                    &mut fixture.view_state,
                );
                let location = fixture
                    .terminal
                    .locate_raw_cell_in_projection(&fixture.projection, fixture.target.origin);
                assert!(!matches!(
                    location,
                    terminal::ProjectedRawCellLocation::Hidden { .. }
                ));
                if scenario == "offscreen" {
                    assert_eq!(location, terminal::ProjectedRawCellLocation::Retained);
                }
            }
            _ => unreachable!(),
        }
        let offered = target(&fixture, dirty);
        assert!(offered.is_none(), "{scenario}");
        let mut widget = FindWidget::new(offered, true);
        let (messages, captured) = widget.event(enter(keyboard::Modifiers::CTRL, false, true));
        assert!(messages.is_empty(), "{scenario}");
        assert!(
            !captured,
            "{scenario}: preserve existing no-target Enter route"
        );
    }
}

#[test]
fn keyboard_reveal_uses_current_target_and_changes_only_its_collapse_policy() {
    let mut fixture = search_reveal_fixture();
    let raw = fixture.terminal.grid.to_vec();
    let query = fixture.search.query.clone();
    let index = fixture.search.current_match_index;
    let mut widget = FindWidget::new(target(&fixture, false), true);
    let (messages, captured) = widget.event(enter(keyboard::Modifiers::CTRL, false, true));
    assert!(captured);
    let [Message::SearchRevealKey(target, _)] = messages.as_slice() else {
        panic!("focused Ctrl+Enter must emit exactly the stable reveal action");
    };
    assert!(apply_search_reveal(
        target,
        &fixture.search,
        false,
        7,
        &fixture.terminal,
        &fixture.projection,
        &mut fixture.policy
    ));
    assert!(!fixture.policy.is_collapsed(target.zone_id));
    assert!(fixture.policy.is_collapsed(fixture.other_zone));
    assert_eq!(fixture.terminal.grid.to_vec(), raw);
    assert_eq!(fixture.search.query, query);
    assert_eq!(fixture.search.current_match_index, index);
    let revision = fixture.policy.revision();
    assert!(!apply_search_reveal(
        target,
        &fixture.search,
        false,
        7,
        &fixture.terminal,
        &fixture.projection,
        &mut fixture.policy
    ));
    assert_eq!(
        fixture.policy.revision(),
        revision,
        "duplicate dispatch is inert"
    );
}

#[test]
fn keyboard_message_revalidates_stale_identity_before_expansion() {
    for stale in [
        "query",
        "session",
        "selection",
        "lost_match",
        "mode",
        "case",
        "closed",
        "dirty",
        "policy",
        "output",
        "origin",
        "evicted",
    ] {
        let mut fixture = search_reveal_fixture();
        let mut widget = FindWidget::new(target(&fixture, false), true);
        let (messages, _) = widget.event(enter(keyboard::Modifiers::CTRL, false, true));
        let [Message::SearchRevealKey(target, _)] = messages.as_slice() else {
            panic!("reveal action");
        };
        let mut target = target.clone();
        let mut session = 7;
        let mut dirty = false;
        match stale {
            "query" => fixture.search.set_query("changed"),
            "session" => session = 8,
            "selection" => fixture.search.current_match_index = 1,
            "lost_match" => fixture.search.matches.clear(),
            "mode" => fixture.search.use_regex = true,
            "case" => fixture.search.case_sensitive = true,
            "closed" => fixture.search.close(),
            "dirty" => dirty = true,
            "policy" => {
                fixture.policy.expand(fixture.other_zone);
            }
            "output" => fixture.terminal.process_input(b"\x1b[2;1H\x1b[2Kgone"),
            "origin" => target.origin.col += 1,
            "evicted" => {
                fixture.terminal.clear_completed_blocks();
            }
            _ => unreachable!(),
        }
        let revision = fixture.policy.revision();
        assert!(
            !apply_search_reveal(
                &target,
                &fixture.search,
                dirty,
                session,
                &fixture.terminal,
                &fixture.projection,
                &mut fixture.policy
            ),
            "{stale}"
        );
        assert!(
            fixture.policy.is_collapsed(fixture.target.zone_id),
            "{stale}"
        );
        assert_eq!(fixture.policy.revision(), revision, "{stale}");
    }
}

#[test]
fn preedit_and_empty_preedit_cannot_activate_keyboard_reveal() {
    use iced::advanced::input_method::Event as Ime;
    let fixture = search_reveal_fixture();
    let mut widget = FindWidget::new(Some(fixture.target), true);
    for event in [
        Ime::Opened,
        Ime::Preedit("中文".into(), None),
        Ime::Preedit(String::new(), None),
    ] {
        let opened = matches!(event, Ime::Opened);
        widget.event(Event::InputMethod(event));
        if !opened {
            let (messages, captured) = widget.event(enter(keyboard::Modifiers::CTRL, false, true));
            assert!(captured);
            assert!(messages.is_empty());
        }
    }
    let (messages, captured) = widget.event(Event::InputMethod(Ime::Commit("中文".into())));
    assert!(captured);
    assert!(matches!(messages.as_slice(), [Message::SearchInput(query)] if query == "needle中文"));
    // A repeat after candidate confirmation may not reveal, even after Commit.
    let (messages, captured) = widget.event(enter(keyboard::Modifiers::CTRL, true, true));
    assert!(captured);
    assert!(messages.is_empty());
    widget.event(Event::Window(iced::window::Event::Unfocused));
    let (messages, captured) = widget.event(enter(keyboard::Modifiers::CTRL, false, true));
    assert!(messages.is_empty());
    assert!(!captured);
}

#[test]
fn production_reveal_routing_preserves_ime_native_ownership_and_navigation() {
    let source = include_str!("main.rs");
    let handler = source
        .split_once("            Message::SearchRevealKey(target, event) => {")
        .unwrap()
        .1
        .split_once("            Message::SearchInput(value)")
        .unwrap()
        .0;
    let claim = handler
        .find("self.native_enter_ownership.lock().claim_pressed()")
        .unwrap();
    let ime = handler
        .find("self.terminal_ime.claim_enter(&event)")
        .unwrap();
    let reveal = handler
        .find("return self.reveal_hidden_search_match(&target)")
        .unwrap();
    assert!(claim < ime && ime < reveal);
    assert!(handler.contains("|| !self.focused"));
    assert!(handler.contains("|| !self.terminal_mouse_active()"));
    assert!(source.contains(".filter(|_| self.terminal_mouse_active())"));
    let navigation = source
        .split_once("    fn handle_search_key(")
        .unwrap()
        .1
        .split_once("    /// While the Find & Replace")
        .unwrap()
        .0;
    assert!(navigation.contains("self.navigate_search(mods.shift());"));
    assert!(!navigation.contains("SearchReveal"));
    let captured = source
        .split_once("            Message::CapturedEnterPress(id, event) => {")
        .unwrap()
        .1
        .split_once("            Message::WindowKey")
        .unwrap()
        .0;
    assert!(captured.contains("native.acknowledge_captured(id, &event)"));
    assert!(source.contains("Reveal match (Ctrl+Enter)"));
}

#[test]
fn captured_reveal_hold_cannot_navigate_after_target_disappears() {
    let fixture = search_reveal_fixture();
    let mut widget = FindWidget::new(Some(fixture.target), true);
    let (messages, captured) = widget.event(enter(keyboard::Modifiers::CTRL, false, true));
    assert!(captured);
    assert!(matches!(
        messages.as_slice(),
        [Message::SearchRevealKey(_, _)]
    ));
    let mut latch = PromptRecallEnterLatch::default();
    // Same existing fallback call made by CapturedEnterPress, never another
    // reveal or a key redispatch. No native observer is installed by this test.
    latch.captured_press(keyboard::Location::Standard);
    for modifiers in [keyboard::Modifiers::CTRL, keyboard::Modifiers::empty()] {
        let Event::Keyboard(repeat) = enter(modifiers, true, true) else {
            unreachable!()
        };
        assert!(latch.consume(&repeat, false));
    }
    let Event::Keyboard(release) = enter(keyboard::Modifiers::empty(), false, false) else {
        unreachable!()
    };
    assert!(latch.consume(&release, false));
    let Event::Keyboard(next) = enter(keyboard::Modifiers::empty(), false, true) else {
        unreachable!()
    };
    assert!(
        !latch.consume(&next, false),
        "fresh Enter retains normal navigation"
    );
}

#[test]
fn cancelled_terminal_preedit_keeps_ownership_of_keyboard_reveal_event() {
    use iced::advanced::input_method::Event as Ime;
    let fixture = search_reveal_fixture();
    let mut widget = FindWidget::new(Some(fixture.target), true);
    let (messages, _) = widget.event(enter(keyboard::Modifiers::CTRL, false, true));
    let [Message::SearchRevealKey(_, event)] = messages.as_slice() else {
        panic!("reveal action")
    };
    let mut ime = terminal_ime::Ownership::default();
    ime.sync(Some(terminal_ime::Owner {
        session: 7,
        instance: Arc::new(()),
    }));
    assert!(ime.admit(&Ime::Preedit("中文".into(), None)));
    assert!(ime.sync(None), "Find replaced terminal composition owner");
    assert!(
        ime.claim_enter(event),
        "production reducer must stop before reveal"
    );
    assert!(
        !ime.admit(&Ime::Commit("中文".into())),
        "no stale terminal text replay"
    );
}

#[test]
fn empty_preedit_start_and_cancel_keep_one_candidate_key_then_recover() {
    use iced::advanced::input_method::Event as Ime;
    for had_nonempty in [false, true] {
        let fixture = search_reveal_fixture();
        let mut widget = FindWidget::new(Some(fixture.target), true);
        widget.event(Event::InputMethod(Ime::Opened));
        if had_nonempty {
            widget.event(Event::InputMethod(Ime::Preedit("中文".into(), None)));
        }
        widget.event(Event::InputMethod(Ime::Preedit(String::new(), None)));
        let (messages, captured) = widget.event(enter(keyboard::Modifiers::CTRL, false, true));
        assert!(captured);
        assert!(
            messages.is_empty(),
            "initial/ending empty preedit still owns its candidate key"
        );
        for _ in 0..3 {
            let (messages, captured) = widget.event(enter(keyboard::Modifiers::CTRL, true, true));
            assert!(captured);
            assert!(
                messages.is_empty(),
                "same candidate hold cannot reveal after retiring empty preedit"
            );
        }
        widget.event(enter(keyboard::Modifiers::CTRL, false, false));
        let (messages, captured) = widget.event(enter(keyboard::Modifiers::CTRL, false, true));
        assert!(captured);
        assert!(
            matches!(messages.as_slice(), [Message::SearchRevealKey(_, _)]),
            "fresh chord must recover even when cancellation has no Commit/Closed"
        );
    }
}

#[test]
fn lost_find_ownership_retires_preedit_across_pointer_operation_window_and_modal() {
    use iced::advanced::input_method::Event as Ime;
    for transition in ["pointer", "operation", "window", "modal"] {
        let fixture = search_reveal_fixture();
        let mut widget = FindWidget::new(Some(fixture.target), true);
        widget.event(Event::InputMethod(Ime::Opened));
        widget.event(Event::InputMethod(Ime::Preedit("中文".into(), None)));
        match transition {
            "pointer" => {
                widget.event_at(
                    Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)),
                    mouse::Cursor::Available(iced::Point::new(280.0, 50.0)),
                );
                assert!(!widget.tree.children[0]
                    .state
                    .downcast_ref::<InputState>()
                    .is_focused());
            }
            "operation" => widget.focus("another-input"),
            "window" => {
                widget.event(Event::Window(iced::window::Event::Unfocused));
            }
            "modal" => {
                widget.rebuild(false);
                // A modal's noncaptured Preedit can reach the covered Find;
                // its captured Commit need not reach Find at all.
                widget.event(Event::InputMethod(Ime::Preedit("other field".into(), None)));
                let (messages, captured) =
                    widget.event(enter(keyboard::Modifiers::CTRL, false, true));
                assert!(messages.is_empty());
                assert!(!captured, "disabled Find cannot claim modal keys");
            }
            _ => unreachable!(),
        }
        widget.event(Event::InputMethod(Ime::Preedit(String::new(), None)));
        if transition == "window" {
            widget.event(Event::Window(iced::window::Event::Focused));
        }
        widget.rebuild(true);
        widget.focus("find-key-test");
        let (messages, captured) = widget.event(enter(keyboard::Modifiers::CTRL, false, true));
        assert!(
            messages.is_empty(),
            "{transition}: cancelled candidate remains owned once"
        );
        assert!(captured);
        let (messages, _) = widget.event(enter(keyboard::Modifiers::CTRL, true, true));
        assert!(messages.is_empty());
        widget.event(enter(keyboard::Modifiers::CTRL, false, false));
        let (messages, captured) = widget.event(enter(keyboard::Modifiers::CTRL, false, true));
        assert!(captured);
        assert!(
            matches!(messages.as_slice(), [Message::SearchRevealKey(_, _)]),
            "{transition}: no persistent IME poison"
        );
    }
}

#[test]
fn active_find_tracks_its_composition_even_before_a_reveal_target_exists() {
    use iced::advanced::input_method::Event as Ime;
    let fixture = search_reveal_fixture();
    let mut widget = FindWidget::new(None, true);
    widget.event(Event::InputMethod(Ime::Opened));
    widget.event(Event::InputMethod(Ime::Preedit("中文".into(), None)));
    widget.target = Some(fixture.target);
    widget.rebuild(true);
    let (messages, captured) = widget.event(enter(keyboard::Modifiers::CTRL, false, true));
    assert!(captured);
    assert!(
        messages.is_empty(),
        "optional target is not Find's composition ownership"
    );
}
