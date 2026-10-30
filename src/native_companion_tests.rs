//! Migrated shell contracts. These use the production admission/allocator and
//! the real retained TermWidget, not the removed ASCII footer's widget tree.
use super::*;
use crate::native_companion::{
    self as admission, BodyGuards, Interaction, Observation, SessionOwner,
};
use crate::organism::{AttentionFacts, Motion, Organism};
use iced::advanced::{layout, renderer, widget::Tree, Layout, Shell, Widget};
use iced::{mouse, Event, Size};
use jterm_core::organism_live::LiveActivityKnowledge as Knowledge;

#[derive(Clone, Debug)]
enum Output {
    Body(OrganismBodyObservation),
    Expanded(bool),
    Mouse(MouseInput),
    Summary,
}

fn observation() -> Observation {
    Observation {
        enabled: true,
        card_mode: true,
        bottom_bar: false,
        focused: true,
        input_active: true,
        presentable: true,
        session: Some(SessionOwner {
            id: 7,
            managed_remote: false,
            read_only: false,
            alternate_screen: false,
            local: true,
            remote_foreground: false,
        }),
    }
}

fn interaction() -> Interaction {
    Interaction {
        presentable: true,
        has_owner: true,
        pointer_down: false,
        dragging: false,
        application_gesture: false,
        running: false,
    }
}

fn engine() -> Organism {
    let mut engine = Organism::default();
    engine.set_enabled(true);
    engine.prime_session(7, false);
    engine.set_remote(7, false);
    engine.sync_live_attention_owner(Some(7));
    engine
}

fn attention_state(engine: &Organism) -> Option<jterm_core::organism_live::LiveAttentionSnapshot> {
    engine
        .live_greeting_snapshot(AttentionFacts::known_idle_for_test(true, true))
        .map(|mut snapshot| {
            snapshot.observed_at = std::time::Duration::ZERO;
            snapshot
        })
}

fn with_widget(test: impl FnOnce(&mut TermWidget<'_, Output>, Rectangle, &mut Organism)) {
    let mut terminal = crate::terminal::TerminalState::new(80, 8);
    let projection = terminal.get_projected_viewport(false);
    let theme = Theme::default();
    let mut engine = engine();
    let presentation = engine.presentation_with_attention(
        7,
        false,
        false,
        Some(Motion::Calm),
        AttentionFacts::known_idle_for_test(true, true),
    );
    let mut blocks = vec![BlockPaintRow::default(); 8];
    for (index, row) in blocks.iter_mut().enumerate() {
        row.card_group = Some(0);
        row.card_kind = BlockCardKind::Active;
        row.app_eligible = true;
        row.card_top = index == 0;
        row.card_bottom = index == 7;
    }
    let mut widget = TermWidget::new(
        projection.cells(),
        (0, 0),
        true,
        CursorShape::Block,
        true,
        &theme,
        Metrics::new(10.0, 1.0, 4.0, true),
        iced::Font::MONOSPACE,
        None,
        None,
        None,
        None,
        Vec::new(),
        0,
        0,
    )
    .blocks(blocks)
    .organism_status(None, true, Output::Expanded)
    .organism_body(
        Some(OrganismBody::from_presentation(
            terminal.raw_row_id_at_absolute(0).unwrap(),
            9,
            presentation,
            0,
        )),
        engine.live_greeting_offer(7),
        engine.live_greeting_exit_reference(7),
        Output::Body,
    )
    .on_mouse(Output::Mouse)
    .on_summary(projection.key(), |_| Output::Summary);
    let bounds = Rectangle::with_size(Size::new(760.0, 150.0));
    test(&mut widget, bounds, &mut engine);
}

fn tree(widget: &TermWidget<'_, Output>) -> Tree {
    Tree::new(widget as &dyn Widget<Output, iced::Theme, ()>)
}

fn send(
    widget: &mut TermWidget<'_, Output>,
    tree: &mut Tree,
    bounds: Rectangle,
    viewport: Rectangle,
    event: Event,
    point: Point,
) -> (Vec<Output>, bool) {
    let node = <TermWidget<'_, Output> as Widget<Output, iced::Theme, ()>>::layout(
        widget,
        tree,
        &(),
        &layout::Limits::new(Size::ZERO, bounds.size()),
    )
    .move_to(bounds.position());
    let mut output = Vec::new();
    let mut shell = Shell::new(&mut output);
    <TermWidget<'_, Output> as Widget<Output, iced::Theme, ()>>::update(
        widget,
        tree,
        &event,
        Layout::new(&node),
        mouse::Cursor::Available(point),
        &(),
        &mut iced::advanced::clipboard::Null,
        &mut shell,
        &viewport,
    );
    let captured = shell.is_event_captured();
    (output, captured)
}

fn moved(point: Point) -> Event {
    Event::Mouse(mouse::Event::CursorMoved { position: point })
}

fn paint(widget: &TermWidget<'_, Output>, tree: &Tree, bounds: Rectangle, viewport: Rectangle) {
    // Actual Widget::draw runs with Iced's debug-only null renderer. Geometry
    // assertions inspect the same production helpers draw/update consume.
    let node = layout::Node::new(bounds.size()).move_to(bounds.position());
    <TermWidget<'_, Output> as Widget<Output, iced::Theme, ()>>::draw(
        widget,
        tree,
        &mut (),
        &iced::Theme::Dark,
        &renderer::Style::default(),
        Layout::new(&node),
        mouse::Cursor::Unavailable,
        &viewport,
    );
}

#[test]
fn companion_strip_and_tooltip_clip_to_their_allocated_bounds() {
    with_widget(|widget, bounds, _| {
        let body = widget.organism_geometry(bounds, &bounds).unwrap();
        let speech = widget.organism_speech_geometry(&body).unwrap();
        assert!(native_body_rect(bounds).includes(body.rect));
        assert!(body.travel.includes(native_body_rect(speech)));
        let clipped = Rectangle {
            height: body.rect.y + body.rect.height - 1.0,
            ..bounds
        };
        assert!(widget.organism_geometry(bounds, &clipped).is_none());
        paint(widget, &tree(widget), bounds, clipped);
    });
}

#[test]
fn live_rhythm_explanation_is_shared_by_body_and_tooltip() {
    let engine = engine();
    let presentation = engine.presentation_with_attention(
        7,
        false,
        false,
        Some(Motion::Calm),
        AttentionFacts::known_idle_for_test(true, true),
    );
    let expected = (
        presentation.status.to_string(),
        presentation.explanation.to_string(),
        presentation.life,
    );
    let terminal = crate::terminal::TerminalState::new(2, 2);
    let body = OrganismBody::from_presentation(
        terminal.raw_row_id_at_absolute(0).unwrap(),
        4,
        presentation,
        19,
    );
    assert_eq!((body.status, body.explanation, body.life), expected);
    assert_eq!((body.epoch, body.frame), (4, 19));
}

#[test]
fn native_body_uses_shared_context_and_current_owner_without_new_timers() {
    let allowed = BodyGuards {
        card_mode: true,
        ..BodyGuards::default()
    };
    assert!(allowed.allows());
    let schedule = crate::organism_wake_plan::ScheduleFacts {
        motion: jterm_core::organism_motion::Motion::Full,
        focused: true,
        live_presentable: true,
        native_visible: true,
        live_owner: true,
        preview_visible: false,
        preview_animation_enabled: false,
    };
    assert!(crate::organism_wake_plan::periodic(schedule).is_some());
    assert!(
        crate::organism_wake_plan::periodic(crate::organism_wake_plan::ScheduleFacts {
            native_visible: false,
            ..schedule
        })
        .is_none()
    );
    for rejected in [
        BodyGuards {
            card_mode: false,
            ..allowed
        },
        BodyGuards {
            epoch_exhausted: true,
            ..allowed
        },
        BodyGuards {
            pointer_down: true,
            ..allowed
        },
        BodyGuards {
            dragging: true,
            ..allowed
        },
        BodyGuards {
            application_gesture: true,
            ..allowed
        },
        BodyGuards {
            selected: true,
            ..allowed
        },
        BodyGuards {
            mouse_reporting: true,
            ..allowed
        },
        BodyGuards {
            preedit: true,
            ..allowed
        },
        BodyGuards {
            images: true,
            ..allowed
        },
    ] {
        assert!(!rejected.allows());
    }
    let owner = observation();
    assert_eq!(owner.owner(), Some(7));
    assert_eq!(
        Observation {
            input_active: false,
            ..owner
        }
        .owner(),
        None
    );
    assert_eq!(
        Observation {
            session: None,
            ..owner
        }
        .owner(),
        None
    );
}

#[test]
fn narrow_reopen_is_configuration_only_and_preserves_live_thresholds() {
    // The old220..640 reopen footer is retired. The actual native layout
    // admits the same body independently of details expansion/bottom bar.
    for width in [0.0, 159.0, 160.0, 220.0, 360.0, 640.0, 1356.0, f32::NAN] {
        for enabled in [false, true] {
            for expanded in [false, true] {
                for bottom_bar in [false, true] {
                    let layout =
                        crate::organism_host_layout(width, enabled, expanded, bottom_bar, true);
                    assert_eq!(layout.body, enabled && width.is_finite() && width >= 160.0);
                    assert!(!layout.glyph, "no second ASCII-footer target");
                    assert_eq!(
                        layout.explanation,
                        layout.body && expanded && width >= 360.0
                    );
                }
            }
        }
    }
}

#[test]
fn companion_card_header_and_narrow_reopen_do_not_add_live_inputs() {
    with_widget(|widget, bounds, engine| {
        let before = attention_state(engine);
        let mut state = tree(widget);
        let body = widget.organism_geometry(bounds, &bounds).unwrap();
        let center = Point::new(body.rect.x + 1.0, body.rect.y + 1.0);
        let (messages, captured) = send(widget, &mut state, bounds, bounds, moved(center), center);
        assert!(
            !captured,
            "passive body observation cannot own terminal input"
        );
        assert!(messages.iter().all(|m| matches!(m, Output::Body(_))));
        assert_eq!(attention_state(engine), before);
    });
}

#[test]
fn companion_care_admission_keeps_native_guards_without_excluding_static_motion() {
    let base = interaction();
    assert!(base.available());
    for denied in [
        Interaction {
            presentable: false,
            ..base
        },
        Interaction {
            has_owner: false,
            ..base
        },
        Interaction {
            pointer_down: true,
            ..base
        },
        Interaction {
            dragging: true,
            ..base
        },
        Interaction {
            application_gesture: true,
            ..base
        },
        Interaction {
            running: true,
            ..base
        },
    ] {
        assert!(!denied.available());
    }
    for motion in [Motion::Full, Motion::Calm, Motion::Static] {
        assert_eq!(
            admission::attention_available(base.available(), Some(motion)),
            motion != Motion::Static
        );
        assert!(
            admission::care_available(true, base.available(), true, true, || Knowledge::KnownIdle),
            "{motion:?}"
        );
        for known in [Knowledge::Unknown, Knowledge::Busy] {
            assert!(!admission::care_available(true, true, true, true, || known));
        }
    }
    for (enabled, available, visible, clean) in [
        (false, true, true, true),
        (true, false, true, true),
        (true, true, false, true),
        (true, true, true, false),
    ] {
        assert!(!admission::care_available(
            enabled,
            available,
            visible,
            clean,
            || panic!("ineligible care must not inspect the prompt")
        ));
    }
}

#[test]
fn narrow_companion_owner_view_and_subscription_share_geometry_without_second_target() {
    with_widget(|widget, bounds, _| {
        let mut retained = tree(widget);
        let inside = widget.organism_geometry(bounds, &bounds).unwrap().rect;
        let point = Point::new(inside.x + 2.0, inside.y + 2.0);
        let (first, _) = send(widget, &mut retained, bounds, bounds, moved(point), point);
        assert!(first
            .iter()
            .any(|m| matches!(m, Output::Body(o) if o.edge == BodyEdge::Geometry)));
        widget.organism_body.as_mut().unwrap().epoch += 1;
        let (changed, _) = send(widget, &mut retained, bounds, bounds, moved(point), point);
        assert!(changed
            .iter()
            .all(|m| !matches!(m, Output::Body(o) if o.edge == BodyEdge::Enter)));
    });
}

#[test]
fn companion_height_is_stable_and_tiny_terminal_viewports_are_nonnegative() {
    for height in [0.0, 1.0, 40.0, 88.0, 100.0, 704.0] {
        let layout = crate::glass_layout::Layout::for_size(800.0, height);
        for reading in [0.0, 64.0, 116.0] {
            for recovery in [0.0, 40.0] {
                // This is the direct native surface, without a separate editor.
                let raw = crate::native_viewport::raw_rect(layout, reading, recovery, None);
                assert!(raw.height >= 0.0 && raw.width >= 0.0);
                let expected = (layout.center.height
                    - layout.header.height
                    - crate::native_viewport::CONTENT_TOP
                    - layout.composer.height
                    - crate::glass_shell_tokens::TERMINAL_FOOTER_GAP
                    - reading
                    - recovery)
                    .max(0.0);
                assert_eq!(raw.height, expected);
            }
        }
    }
}

#[test]
fn expanded_companion_reuses_one_snapshot_and_shared_geometry() {
    // Baseline fixture correction, not an Eva geometry change: the current
    // glass profile has 8px side insets, 6px top gap and a 42px tool row.
    // Keep exact pixel snapshots as well as the independent composer boundary.
    for (width, height, expected) in [
        (1612.0, 976.0, (278.0, 86.0, 1028.0, 836.0)),
        (1356.0, 704.0, (278.0, 86.0, 772.0, 564.0)),
        (800.0, 640.0, (278.0, 86.0, 510.0, 500.0)),
    ] {
        let layout = crate::glass_layout::Layout::for_size(width, height);
        let raw = crate::native_viewport::raw_rect(layout, 0.0, 0.0, None);
        assert_eq!((raw.x, raw.y, raw.width, raw.height), expected);
        assert_eq!(
            raw.bottom() + crate::glass_shell_tokens::TERMINAL_FOOTER_GAP,
            layout.composer.y
        );
    }
    assert_eq!(crate::native_viewport::INPUT_STRIP_HEIGHT, 44.0);
    assert_eq!(crate::native_viewport::TOOLS_HEIGHT, 42.0);
    assert_eq!(
        crate::native_viewport::INPUT_STRIP_HEIGHT + crate::native_viewport::TOOLS_HEIGHT,
        86.0
    );
    with_widget(|widget, bounds, _| {
        let state = tree(widget);
        let body = widget.organism_geometry(bounds, &bounds).unwrap();
        paint(widget, &state, bounds, bounds);
        widget.organism_expanded = false;
        paint(widget, &state, bounds, bounds);
        assert_eq!(widget.organism_geometry(bounds, &bounds), Some(body));
    });
}

#[test]
fn compact_chrome_returns_native_rows_and_columns_at_the_same_terminal_font() {
    // Previous native content rectangles, including the existing sidebar state.
    // These baselines make a geometry regression fail even if paint and PTY
    // accidentally agree on a newly oversized chrome budget.
    for (width, height, previous_width, previous_height) in [
        (1612.0, 976.0, 859.0, 648.0),
        (1356.0, 704.0, 603.0, 430.0),
        (800.0, 640.0, 456.0, 366.0),
        (700.0, 400.0, 356.0, 126.0),
    ] {
        let layout = crate::glass_layout::Layout::for_size(width, height);
        for font in [14.0, 36.0, 72.0] {
            let metrics = Metrics::new(font, 1.0, 2.0, false);
            for requested in [None, Some(44.0_f32), Some(180.0_f32)] {
                let native = crate::native_viewport::native_budget(layout, 0.0, 0.0, requested);
                let previous_input = requested.map_or(0.0, |input| {
                    input.min((previous_height * 0.45_f32).clamp(44.0, 180.0))
                });
                let previous_native_height = previous_height - previous_input;
                let old_grid = metrics.grid_size(
                    (previous_width - SCROLLBAR_WIDTH).max(0.0),
                    previous_native_height,
                );
                let grid = metrics.grid_size(
                    (native.raw.width - SCROLLBAR_WIDTH).max(0.0),
                    native.raw.height,
                );
                assert!(native.raw.width > previous_width);
                assert!(native.raw.height > previous_native_height);
                assert!(
                    grid.0 > old_grid.0,
                    "columns at {width}×{height}, font {font}, input {requested:?}"
                );
                assert!(
                    grid.1 >= old_grid.1,
                    "rows at {width}×{height}, font {font}, input {requested:?}"
                );
                if requested.is_none() {
                    assert!(grid.1 > old_grid.1, "direct native rows at font {font}");
                }
                if let Some(editor) = native.editor {
                    assert_eq!(editor.width, native.raw.width);
                    assert_eq!(
                        native.raw.bottom() + crate::glass_shell_tokens::TERMINAL_FOOTER_GAP,
                        editor.y
                    );
                    assert_eq!(editor.bottom(), layout.composer.y);
                    assert!(editor.height <= crate::glass_layout::MAX_INPUT_HEIGHT);
                }
            }
        }
    }
}

#[test]
fn unavailable_companion_has_no_passive_clone_or_extra_wakes() {
    let base = observation();
    for facts in [
        Observation {
            enabled: false,
            ..base
        },
        Observation {
            focused: false,
            ..base
        },
        Observation {
            input_active: false,
            ..base
        },
        Observation {
            presentable: false,
            ..base
        },
    ] {
        assert_eq!(facts.owner(), None);
    }
    for card_mode in [false, true] {
        for bottom_bar in [false, true] {
            assert_eq!(
                Observation {
                    card_mode,
                    bottom_bar,
                    ..base
                }
                .owner(),
                (card_mode || bottom_bar).then_some(7),
                "legacy bottom-bar ownership must not disable public Block/Chat"
            );
        }
    }
    let session = base.session.unwrap();
    for rejected in [
        SessionOwner {
            managed_remote: true,
            ..session
        },
        SessionOwner {
            read_only: true,
            ..session
        },
        SessionOwner {
            alternate_screen: true,
            ..session
        },
        SessionOwner {
            local: false,
            ..session
        },
        SessionOwner {
            remote_foreground: true,
            ..session
        },
    ] {
        assert_eq!(
            Observation {
                session: Some(rejected),
                ..base
            }
            .owner(),
            None
        );
    }
    with_widget(|widget, bounds, _| {
        widget.organism_body = None;
        assert!(widget.organism_geometry(bounds, &bounds).is_none());
        paint(widget, &tree(widget), bounds, bounds);
    });
}

#[test]
fn companion_toggle_is_separate_config_chrome_without_terminal_or_greeting_actions() {
    with_widget(|widget, bounds, _| {
        let body = widget.organism_geometry(bounds, &bounds).unwrap();
        let speech = widget.organism_speech_geometry(&body).unwrap();
        let close = Point::new(speech.x + speech.width - 12.0, speech.y + 12.0);
        let mut state = tree(widget);
        let (pressed, captured) = send(
            widget,
            &mut state,
            bounds,
            bounds,
            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)),
            close,
        );
        assert!(captured);
        assert_eq!(
            pressed
                .iter()
                .filter(|m| matches!(m, Output::Expanded(false)))
                .count(),
            1
        );
        assert!(!pressed
            .iter()
            .any(|m| matches!(m, Output::Mouse(_) | Output::Body(_))));
        let (released, _) = send(
            widget,
            &mut state,
            bounds,
            bounds,
            Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)),
            close,
        );
        assert!(!released
            .iter()
            .any(|m| matches!(m, Output::Mouse(_) | Output::Expanded(_))));
    });
}

#[test]
fn live_sprite_detail_describes_the_owner_without_another_sprite() {
    with_widget(|widget, bounds, _| {
        let before = widget.organism_body.as_ref().unwrap();
        let text = (
            before.status.clone(),
            before.explanation.clone(),
            before.life,
        );
        let body = widget.organism_geometry(bounds, &bounds).unwrap();
        let speech = widget.organism_speech_geometry(&body).unwrap();
        assert!(body.travel.includes(native_body_rect(speech)));
        paint(widget, &tree(widget), bounds, bounds);
        let after = widget.organism_body.as_ref().unwrap();
        assert_eq!(
            (after.status.clone(), after.explanation.clone(), after.life),
            text
        );
    });
}

#[test]
fn ui_scale_change_resizes_logical_viewport_and_terminal_grid() {
    let old = Size::new(1200.0, 800.0);
    let scaled = crate::logical_viewport_after_scale(old, 1.0, 2.0);
    assert_eq!(scaled, Size::new(600.0, 400.0));
    assert_eq!(scaled.width * 2.0, old.width);
    assert_eq!(scaled.height * 2.0, old.height);
    for font in [10.0, 14.0, 24.0, 72.0] {
        let metrics = Metrics::new(font, 1.0, 0.0, false);
        let layout = crate::glass_layout::Layout::for_size(scaled.width, scaled.height);
        let raw = crate::native_viewport::raw_rect(layout, 0.0, 0.0, None);
        let grid = metrics.grid_size((raw.width - SCROLLBAR_WIDTH).max(0.0), raw.height);
        assert!(grid.0 >= 2 && grid.1 >= 1);
        assert_eq!(
            raw.bottom() + crate::glass_shell_tokens::TERMINAL_FOOTER_GAP,
            layout.composer.y
        );
        // Side panel thresholds can add width, so monotone columns are not an oracle.
        assert!(raw.width <= scaled.width && raw.height <= scaled.height);
    }
}

#[test]
fn disabling_the_bottom_bar_returns_its_rows_to_the_grid() {
    // The obsolete bottom bar is not drawn by the current Block/Chat shell.
    // A legacy setting must not deduct phantom rows from either presentation.
    let layout = crate::glass_layout::Layout::for_size(1356.0, 704.0);
    // Native Block and Chat fallback have no editor; a retained Chat prompt
    // reserves its actual editor height above the same fixed glass tool row.
    // Current baseline tokens give y=44+36+6=86 and bottom=704-6-42-6=650.
    // Each retained editor subtracts its exact height, not the obsolete 44px toolbar.
    for (mode, input_height, expected_height, expected_bottom) in [
        (crate::config::TerminalMode::Block, None, 564.0, 650.0),
        (crate::config::TerminalMode::Chat, None, 564.0, 650.0),
        (crate::config::TerminalMode::Chat, Some(44.0), 520.0, 606.0),
        (crate::config::TerminalMode::Chat, Some(180.0), 384.0, 470.0),
    ] {
        let expected = crate::native_viewport::raw_rect(layout, 0.0, 0.0, input_height);
        for bottom_bar in [false, true] {
            let config = crate::config::Config {
                terminal_mode: Some(mode),
                bottom_bar,
                ..Default::default()
            };
            assert!(config.is_card_mode());
            let body = crate::organism_host_layout(
                expected.width,
                true,
                config.ascii_organism_expanded,
                config.bottom_bar,
                config.is_card_mode(),
            );
            assert!(body.body && !body.glyph);
            assert_eq!(expected.height, expected_height);
            assert_eq!(expected.bottom(), expected_bottom);
            assert_eq!(
                expected.bottom()
                    + crate::glass_shell_tokens::TERMINAL_FOOTER_GAP
                    + input_height.unwrap_or(0.0),
                layout.composer.y
            );
        }
    }
}

#[test]
fn activity_facts_reconcile_only_fresh_active_output_after_context_updates() {
    assert!(admission::active_output(Some((7, 19)), 7, 19));
    for (id, fd) in [(8, 19), (7, 20), (8, 20)] {
        assert!(!admission::active_output(Some((7, 19)), id, fd));
    }
    assert!(!admission::active_output(None, 7, 19));
    let mut terminal = crate::terminal::TerminalState::new(80, 24);
    terminal.process_input(b"\x1b]133;A\x07$ \x1b]133;B\x07");
    assert_eq!(
        admission::activity(false, true, || terminal.live_activity_facts().knowledge()),
        Knowledge::KnownIdle
    );
    terminal.process_input(b"echo one\r\n\x1b]133;C\x07");
    assert_eq!(
        admission::activity(true, true, || panic!("Busy cannot scan prompt")),
        Knowledge::Busy
    );
    assert_eq!(
        admission::activity(false, false, || panic!("ineligible cannot scan prompt")),
        Knowledge::Unknown
    );
    terminal.process_input(b"one\r\n\x1b]133;D;0\x07\x1b]133;A\x07$ \x1b]133;B\x07");
    assert_eq!(
        admission::activity(false, true, || terminal.live_activity_facts().knowledge()),
        Knowledge::KnownIdle
    );
}

#[test]
fn palette_greeting_uses_opening_target_after_close_and_presentable_admission() {
    let mut palette = crate::command_palette::PaletteState::default();
    palette.open();
    palette.bind_greeting_target(Some(7));
    let original = palette.greeting_target();
    let epoch = palette.epoch();
    palette.close();
    assert_eq!(palette.greeting_target(), None);
    assert!(admission::greeting_target(original, Some(7), true));
    assert!(!admission::greeting_target(original, Some(8), true));
    assert!(!admission::greeting_target(original, Some(7), false));
    palette.open();
    assert!(!palette.accepts_epoch(&epoch));
    assert!(!admission::greeting_target(
        palette.greeting_target(),
        Some(7),
        true
    ));
}

#[test]
fn hover_candidate_cue_stays_inside_glyph_slot_and_uses_existing_admission() {
    with_widget(|widget, bounds, engine| {
        let body = widget.organism_geometry(bounds, &bounds).unwrap();
        assert!(body.travel.includes(body.rect));
        assert_eq!(widget.organism_offer, engine.live_greeting_offer(7));
        assert_eq!(widget.organism_exit, engine.live_greeting_exit_reference(7));
        // There is no separate2px ASCII-footer cue or actor.
        assert!(widget.organism_status.is_none());
    });
}

#[test]
fn live_hover_is_passive_owner_bound_and_deadline_driven() {
    with_widget(|widget, bounds, _| {
        let body = widget.organism_geometry(bounds, &bounds).unwrap();
        let inside = Point::new(body.rect.x + 3.0, body.rect.y + 3.0);
        let outside = Point::new(10.0, 10.0);
        let mut state = tree(widget);
        send(widget, &mut state, bounds, bounds, moved(outside), outside);
        let (entry, captured) = send(widget, &mut state, bounds, bounds, moved(inside), inside);
        assert!(!captured);
        assert!(entry.iter().any(|m| matches!(m, Output::Body(o) if o.edge == BodyEdge::Enter && o.ticket == widget.organism_offer)));
        let (exit, captured) = send(widget, &mut state, bounds, bounds, moved(outside), outside);
        assert!(!captured);
        assert!(exit.iter().any(|m| matches!(m, Output::Body(o) if o.edge == BodyEdge::Exit && o.ticket == widget.organism_exit)));
        assert!(!entry
            .iter()
            .chain(&exit)
            .any(|m| matches!(m, Output::Mouse(_) | Output::Expanded(_))));
    });
}

#[test]
fn organism_observation_reuses_terminal_input_ownership() {
    let current = observation();
    assert_eq!(current.owner(), Some(7));
    for modal in [
        "Settings",
        "AI chats",
        "AI ask",
        "Tools",
        "palette",
        "embedded editor",
    ] {
        assert_eq!(
            Observation {
                input_active: false,
                ..current
            }
            .owner(),
            None,
            "{modal}"
        );
    }
    let block = Some(4_u64);
    let snapshot = admission::Snapshot {
        owner: 7,
        epoch: 9,
        projection: &12,
        block: &block,
    };
    assert!(snapshot.matches(&admission::Snapshot {
        owner: 7,
        epoch: 9,
        projection: &12,
        block: &block
    }));
    for (owner, epoch, projection, key) in [
        (8, 9, 12, block),
        (7, 10, 12, block),
        (7, 9, 13, block),
        (7, 9, 12, Some(5)),
    ] {
        assert!(!snapshot.matches(&admission::Snapshot {
            owner,
            epoch,
            projection: &projection,
            block: &key
        }));
    }
}

#[test]
fn modal_ai_surfaces_gate_terminal_input_and_mouse_ownership() {
    for (chats, ask) in [(true, false), (false, true), (true, true)] {
        assert!(crate::native_input_surface::ai_surface_active(chats, ask));
    }
    assert!(!crate::native_input_surface::ai_surface_active(
        false, false
    ));
    with_widget(|widget, bounds, _| {
        widget.focused = false;
        assert!(widget.organism_geometry(bounds, &bounds).is_none());
        let mut state = tree(widget);
        let point = Point::new(20.0, 20.0);
        let (messages, _) = send(
            widget,
            &mut state,
            bounds,
            bounds,
            Event::Window(iced::window::Event::RedrawRequested(
                std::time::Instant::now(),
            )),
            point,
        );
        assert!(messages
            .iter()
            .all(|m| !matches!(m, Output::Expanded(_) | Output::Mouse(_))));
        // The actual native owner predicate is what blocks observations beneath the modal.
        assert_eq!(
            Observation {
                input_active: false,
                ..observation()
            }
            .owner(),
            None
        );
    });
}

#[test]
fn companion_compact_toggle_captures_click_without_greeting_or_terminal_delivery() {
    with_widget(|widget, bounds, engine| {
        let before = attention_state(engine);
        let body = widget.organism_geometry(bounds, &bounds).unwrap();
        let speech = widget.organism_speech_geometry(&body).unwrap();
        let close = Point::new(speech.x + speech.width - 12.0, speech.y + 12.0);
        let mut retained = tree(widget);
        let (messages, captured) = send(
            widget,
            &mut retained,
            bounds,
            bounds,
            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)),
            close,
        );
        assert!(captured);
        assert_eq!(messages.len(), 1);
        assert!(matches!(messages[0], Output::Expanded(false)));
        assert_eq!(attention_state(engine), before);
        assert_eq!(widget.organism_geometry(bounds, &bounds), Some(body));
    });
}

#[test]
fn wide_companion_reserves_glyph_and_toggle_beside_overflowing_status() {
    with_widget(|widget, bounds, _| {
        widget.organism_body.as_mut().unwrap().explanation = "long status ".repeat(400);
        let body = widget.organism_geometry(bounds, &bounds).unwrap();
        let speech = widget.organism_speech_geometry(&body).unwrap();
        assert!(body.travel.includes(native_body_rect(speech)));
        assert!(speech.x + speech.width < body.rect.x);
        paint(widget, &tree(widget), bounds, bounds);
    });
}

#[test]
fn narrow_reopen_reserves_clickable_control_beside_overflowing_status() {
    with_widget(|widget, bounds, _| {
        for width in [1.0, 80.0, 120.0] {
            let narrow = Rectangle { width, ..bounds };
            assert!(widget.organism_geometry(narrow, &narrow).is_none());
            let mut state = tree(widget);
            let point = Point::new(8.0, 8.0);
            let (messages, _) = send(
                widget,
                &mut state,
                narrow,
                narrow,
                Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)),
                point,
            );
            assert!(!messages.iter().any(|m| matches!(m, Output::Expanded(_))));
            for message in &messages {
                if let Output::Mouse(MouseInput::Press { col, row, .. }) = message {
                    assert!(*col < 80 && *row < 8);
                }
            }
        }
    });
}

#[test]
fn companion_card_native_layout_keeps_body_and_collapse_in_reserved_chrome() {
    with_widget(|widget, bounds, _| {
        let body = widget.organism_geometry(bounds, &bounds).unwrap();
        assert!(body.travel.includes(body.rect));
        assert!(body.rect.y > widget.row_y(bounds, widget.cursor.0));
        let rows = widget.grid.len();
        widget.organism_expanded = false;
        assert_eq!(widget.organism_geometry(bounds, &bounds), Some(body));
        assert_eq!(widget.grid.len(), rows);
    });
}

#[test]
fn companion_card_short_viewports_clip_body_and_details_without_terminal_overlap() {
    with_widget(|widget, bounds, _| {
        let owner = widget.organism_body.as_ref().unwrap().epoch;
        for height in [0.0, 1.0, 24.0, 64.0] {
            let clip = Rectangle { height, ..bounds };
            assert!(widget.organism_geometry(bounds, &clip).is_none());
            let mut state = tree(widget);
            paint(widget, &state, bounds, clip);
            let point = Point::new(20.0, height + 1.0);
            let (messages, captured) = send(widget, &mut state, bounds, clip, moved(point), point);
            assert!(!captured);
            assert!(!messages
                .iter()
                .any(|m| matches!(m, Output::Body(o) if o.edge == BodyEdge::Enter)));
        }
        assert_eq!(widget.organism_body.as_ref().unwrap().epoch, owner);
        widget.organism_body = None;
        assert!(widget.organism_geometry(bounds, &bounds).is_none());
    });
}

#[test]
fn mouse_area_queues_same_offer_enter_and_exit_before_view_rebuild() {
    // No view rebuild between edges: both come from the same real native body.
    with_widget(|widget, bounds, _| {
        assert_eq!(widget.organism_offer, widget.organism_exit);
        let body = widget.organism_geometry(bounds, &bounds).unwrap();
        let inside = Point::new(body.rect.x + 3.0, body.rect.y + 3.0);
        let outside = Point::new(10.0, 10.0);
        let mut retained = tree(widget);
        send(
            widget,
            &mut retained,
            bounds,
            bounds,
            moved(outside),
            outside,
        );
        let mut edges = Vec::new();
        for point in [inside, outside] {
            let (messages, captured) =
                send(widget, &mut retained, bounds, bounds, moved(point), point);
            assert!(!captured);
            edges.extend(messages.into_iter().filter_map(|m| match m {
                Output::Body(observation) => Some((observation.edge, observation.ticket)),
                _ => None,
            }));
        }
        assert_eq!(
            edges,
            vec![
                (BodyEdge::Enter, widget.organism_offer),
                (BodyEdge::Exit, widget.organism_exit)
            ]
        );
    });
}

#[test]
fn native_body_withdrawals_and_entries_validate_exact_native_snapshot() {
    with_widget(|widget, bounds, _| {
        let mut retained = tree(widget);
        let geometry = widget.organism_geometry(bounds, &bounds).unwrap();
        let point = Point::new(geometry.rect.x + 1.0, geometry.rect.y + 1.0);
        send(widget, &mut retained, bounds, bounds, moved(point), point);
        let before = admission::Snapshot {
            owner: 7,
            epoch: geometry.epoch,
            projection: &geometry.projection,
            block: &geometry.block,
        };
        widget.organism_body.as_mut().unwrap().epoch += 1;
        let current = widget.organism_geometry(bounds, &bounds).unwrap();
        assert!(!before.matches(&admission::Snapshot {
            owner: 7,
            epoch: current.epoch,
            projection: &current.projection,
            block: &current.block,
        }));
        let (messages, captured) = send(widget, &mut retained, bounds, bounds, moved(point), point);
        assert!(!captured);
        assert!(messages
            .iter()
            .any(|m| matches!(m, Output::Body(o) if o.edge == BodyEdge::Geometry)));
        assert!(messages
            .iter()
            .all(|m| !matches!(m, Output::Body(o) if o.edge == BodyEdge::Enter)));
        widget.organism_body = None;
        let (messages, _) = send(widget, &mut retained, bounds, bounds, moved(point), point);
        assert!(messages.iter().any(
            |m| matches!(m, Output::Body(o) if o.geometry.is_none() && o.edge == BodyEdge::Geometry)
        ));
    });
}

#[test]
fn shared_attention_coordinates_identity_without_accepting_before_input() {
    let mut engine = engine();
    let facts = AttentionFacts::known_idle_for_test(true, true);
    let before = attention_state(&engine).unwrap();
    assert!(before.entry.is_none() && before.candidate.is_none() && before.active.is_none());
    engine.sync_live_attention_owner(Some(7));
    engine.poll_live_greeting(Some(7), facts);
    let reconciled = attention_state(&engine).unwrap();
    assert!(
        reconciled.entry.is_none() && reconciled.candidate.is_none() && reconciled.active.is_none()
    );
    assert!(!admission::greeting_target(Some(7), Some(8), true));
    assert!(!admission::greeting_target(Some(7), Some(7), false));
    assert!(admission::greeting_target(Some(7), Some(7), true));
}
