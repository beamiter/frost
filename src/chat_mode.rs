//! Paint-only roles for the Chat projection. No input text or execution state.
pub use jterm_core::chat_mode::{resolve_surface, ChatSessionState, ChatSurface};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ControlKind {
    Submit,
    Interrupt,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ControlAction {
    Submit(crate::terminal::ProjectionKey),
    Interrupt(u64),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ControlToken {
    pub session: usize,
    pub foreground: i32,
    pub action: ControlAction,
}

impl ControlToken {
    pub fn kind(&self) -> ControlKind {
        match self.action {
            ControlAction::Submit(_) => ControlKind::Submit,
            ControlAction::Interrupt(_) => ControlKind::Interrupt,
        }
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct InputBacklog {
    pub entries: usize,
    pub offset: usize,
    pub user_bytes: usize,
    pub response_bytes: usize,
}

impl InputBacklog {
    pub fn is_empty(self) -> bool {
        self.entries == 0 && self.offset == 0 && self.user_bytes == 0 && self.response_bytes == 0
    }
}

/// Same eligibility policy for painting and final admission. Multi-byte keys
/// have no proven atomic PTY write: optional buttons yield to the keyboard.
pub fn control_enabled(bytes: &[u8], backlog: InputBacklog, input_ready: bool) -> bool {
    bytes.len() == 1 && backlog.is_empty() && input_ready
}

/// A one-shot native write, never a queue entry, drain, retry or tail buffer.
/// The caller supplies freshly resolved parser/foreground ownership directly
/// before this boundary. A one-byte write cannot leave an admitted ESC prefix.
pub fn admit_control<E>(
    displayed: &ControlToken,
    current: Option<&ControlToken>,
    bytes: &[u8],
    backlog: InputBacklog,
    input_ready: bool,
    write: impl FnOnce(&[u8]) -> Result<usize, E>,
) -> bool {
    control_enabled(bytes, backlog, input_ready)
        && current == Some(displayed)
        && matches!(write(bytes), Ok(1))
}

/// Optional controls act on the displayed press, never on release. Replacing
/// a same-shaped control while held cannot migrate a release to a new owner.
pub fn press_control<
    'a,
    Message: Clone + 'a,
    Theme: 'a,
    Renderer: iced::advanced::Renderer + 'a,
>(
    content: impl Into<iced::Element<'a, Message, Theme, Renderer>>,
    message: Option<Message>,
) -> iced::Element<'a, Message, Theme, Renderer> {
    let area = iced::widget::mouse_area(content);
    if let Some(message) = message {
        area.on_press(message)
            .interaction(iced::mouse::Interaction::Pointer)
            .into()
    } else {
        area.into()
    }
}

/// Exact per-cell ownership, not whole-row classification. Host padding must
/// never become clipboard, caret, selection or application input.
pub fn terminal_press_allowed(
    projection: &crate::terminal::ProjectedViewport,
    revision: u64,
    row: usize,
    col: usize,
) -> bool {
    projection.view_revision() == revision
        && projection
            .view_to_raw(crate::terminal::ViewportCell { row, col })
            .is_some()
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ChatRole {
    #[default]
    Unclassified,
    Command,
    Output,
    Composer,
    Companion,
    Spacer,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ChatRow {
    pub role: ChatRole,
    pub group: usize,
    pub left_col: usize,
    pub top: bool,
    pub bottom: bool,
    pub app_eligible: bool,
    pub status_line: Option<(usize, usize)>,
    pub status_zone: Option<u64>,
}

/// Short commands move as one rectangle; wide commands and all output retain
/// the entire native column budget. No text is rewrapped to a bubble width.
pub fn command_left(cols: usize, occupied: usize) -> usize {
    cols.saturating_sub(occupied.max((cols / 3).min(24)))
        .saturating_sub(1)
}

#[cfg(test)]
mod tests {
    #[test]
    fn right_alignment_never_narrows_wide_content() {
        assert!(super::command_left(80, 12) > 40);
        assert_eq!(super::command_left(80, 80), 0);
        assert_eq!(super::command_left(1, 1), 0);
        assert_eq!(super::command_left(80, 160), 0);
    }
}

#[cfg(test)]
mod admission_tests {
    use super::*;
    use std::cell::Cell;

    fn interrupt() -> ControlToken {
        ControlToken {
            session: 7,
            foreground: 42,
            action: ControlAction::Interrupt(9),
        }
    }

    #[test]
    fn chat_immediate_wouldblock_error_and_zero_never_accept_or_retry() {
        let token = interrupt();
        let calls = Cell::new(0);
        for result in [Ok(0), Err(()), Ok(2)] {
            assert!(!admit_control(
                &token,
                Some(&token),
                b"\x03",
                InputBacklog::default(),
                true,
                |bytes| {
                    calls.set(calls.get() + 1);
                    assert_eq!(bytes, b"\x03");
                    result
                }
            ));
        }
        assert_eq!(calls.get(), 3);
        // No retained payload, scheduled retry or writer exists in the result.
        // A later owner needs a new displayed press and a new validation.
        let later = ControlToken {
            foreground: 43,
            ..token.clone()
        };
        assert!(!admit_control(
            &token,
            Some(&later),
            b"\x03",
            InputBacklog::default(),
            true,
            |_| -> Result<usize, ()> {
                panic!("rejected control must not replay to a later owner")
            }
        ));
    }

    #[test]
    fn chat_immediate_rejects_every_fifo_backlog_without_draining() {
        let token = interrupt();
        for backlog in [
            InputBacklog {
                entries: 1,
                ..InputBacklog::default()
            },
            InputBacklog {
                offset: 1,
                ..InputBacklog::default()
            },
            InputBacklog {
                user_bytes: 1,
                ..InputBacklog::default()
            },
            InputBacklog {
                response_bytes: 1,
                ..InputBacklog::default()
            },
            InputBacklog {
                entries: 2,
                user_bytes: 131_072,
                ..InputBacklog::default()
            },
        ] {
            assert!(!control_enabled(b"\x03", backlog, true));
            assert!(!admit_control(
                &token,
                Some(&token),
                b"\x03",
                backlog,
                true,
                |_| -> Result<usize, ()> {
                    panic!("a button cannot drain keyboard/response backlog")
                }
            ));
        }
    }

    #[test]
    fn chat_immediate_rejects_multibyte_before_any_partial_escape_write() {
        let token = interrupt();
        let writes = Cell::new(0);
        for bytes in [b"\x1b[13u".as_slice(), b"\x1b[99;5u", b"", b"\r\r"] {
            assert!(!control_enabled(bytes, InputBacklog::default(), true));
            assert!(!admit_control(
                &token,
                Some(&token),
                bytes,
                InputBacklog::default(),
                true,
                |_| {
                    writes.set(writes.get() + 1);
                    Ok::<usize, ()>(1) // would have accepted only an ESC prefix
                }
            ));
        }
        assert_eq!(writes.get(), 0);
    }

    #[test]
    fn chat_immediate_revalidates_session_generation_foreground_and_input_barriers() {
        let token = interrupt();
        let replacements = [
            None,
            Some(ControlToken {
                session: 8,
                ..token.clone()
            }),
            Some(ControlToken {
                foreground: 43,
                ..token.clone()
            }),
            Some(ControlToken {
                action: ControlAction::Interrupt(10),
                ..token.clone()
            }),
        ];
        for current in &replacements {
            assert!(!admit_control(
                &token,
                current.as_ref(),
                b"\x03",
                InputBacklog::default(),
                true,
                |_| -> Result<usize, ()> {
                    panic!("stale/unknown identity must not reach the writer")
                }
            ));
        }
        assert!(!admit_control(
            &token,
            Some(&token),
            b"\x03",
            InputBacklog::default(),
            false,
            |_| -> Result<usize, ()> {
                panic!("focus, liveness, modal/IME/Enter/reflow barriers must fail closed")
            }
        ));
        // There is deliberately no 'foreground != shell' rule for Stop:
        // read and other shell-owned builtins must remain interruptible.
        let shell_read = ControlToken {
            foreground: 7,
            ..token
        };
        let writes = Cell::new(0);
        let accepted = admit_control(
            &shell_read,
            Some(&shell_read),
            b"\x03",
            InputBacklog::default(),
            true,
            |bytes| {
                assert_eq!(bytes, b"\x03");
                writes.set(writes.get() + 1);
                Ok::<usize, ()>(1)
            },
        );
        assert!(accepted);
        assert_eq!(writes.get(), 1);
    }

    #[test]
    fn chat_immediate_submit_locks_only_after_native_admission_and_rejects_repeat() {
        let mut terminal = crate::terminal::TerminalState::new(40, 20);
        terminal.process_batch(b"\x1b]133;A\x07$ \x1b]133;B\x07draft");
        let view = terminal.get_projected_viewport(true);
        let token = ControlToken {
            session: 7,
            foreground: 7,
            action: ControlAction::Submit(view.key()),
        };
        let mut accepted_feedback = 0;
        for result in [Ok(0), Err(()), Ok(1)] {
            let accepted = admit_control(
                &token,
                Some(&token),
                b"\r",
                InputBacklog::default(),
                true,
                |_| result,
            );
            if accepted {
                terminal.note_user_input(b"\r");
                terminal.note_chat_submit();
                accepted_feedback += 1;
            } else {
                assert!(terminal.chat_surface().allows_submit_affordance());
            }
        }
        assert_eq!(accepted_feedback, 1);
        let current = terminal
            .chat_surface()
            .allows_submit_affordance()
            .then_some(&token);
        assert!(!admit_control(
            &token,
            current,
            b"\r",
            InputBacklog::default(),
            true,
            |_| -> Result<usize, ()> { panic!("rapid repeated submit must not write again") }
        ));
    }

    #[test]
    fn chat_buttons_use_native_encoder_without_downgrading_enhanced_protocols() {
        for flags in [0, 1, 8, 9] {
            for (key, modifiers) in [
                (
                    iced::keyboard::Key::Named(iced::keyboard::key::Named::Enter),
                    iced::keyboard::Modifiers::empty(),
                ),
                (
                    iced::keyboard::Key::Character("c".into()),
                    iced::keyboard::Modifiers::CTRL,
                ),
            ] {
                let bytes = crate::encode_key(
                    &key,
                    iced::keyboard::Location::Standard,
                    modifiers,
                    None,
                    false,
                    crate::KeyboardEnhancements {
                        kitty_flags: flags,
                        ..Default::default()
                    },
                )
                .unwrap();
                assert_eq!(
                    control_enabled(&bytes, InputBacklog::default(), true),
                    bytes.len() == 1
                );
                if flags == 8 || flags == 9 {
                    assert!(bytes.len() > 1);
                }
            }
        }
    }
}

#[cfg(test)]
mod press_tests {
    use super::press_control;
    use iced::advanced::{layout, mouse, renderer, widget::Tree, Layout, Shell};
    use iced::{Element, Event, Rectangle, Size};
    struct Renderer;
    impl renderer::Renderer for Renderer {
        fn start_layer(&mut self, _: Rectangle) {}
        fn end_layer(&mut self) {}
        fn start_transformation(&mut self, _: iced::Transformation) {}
        fn end_transformation(&mut self) {}
        fn fill_quad(&mut self, _: renderer::Quad, _: impl Into<iced::Background>) {}
        fn reset(&mut self, _: Rectangle) {}
        fn allocate_image(
            &mut self,
            _: &iced::advanced::image::Handle,
            callback: impl FnOnce(Result<iced::advanced::image::Allocation, iced::advanced::image::Error>)
                + Send
                + 'static,
        ) {
            callback(Err(iced::advanced::image::Error::Unsupported));
        }
    }
    fn send(
        element: &mut Element<'_, u8, iced::Theme, Renderer>,
        tree: &mut Tree,
        event: mouse::Event,
    ) -> Vec<u8> {
        let node = element.as_widget_mut().layout(
            tree,
            &Renderer,
            &layout::Limits::new(Size::ZERO, Size::new(100.0, 100.0)),
        );
        let mut messages = Vec::new();
        let captured = {
            let mut shell = Shell::new(&mut messages);
            element.as_widget_mut().update(
                tree,
                &Event::Mouse(event),
                Layout::new(&node),
                mouse::Cursor::Available(iced::Point::new(10.0, 10.0)),
                &Renderer,
                &mut iced::advanced::clipboard::Null,
                &mut shell,
                &Rectangle::with_size(Size::new(100.0, 100.0)),
            );
            shell.is_event_captured()
        };
        if !messages.is_empty() {
            assert!(captured, "control press must not also drag the title bar");
        }
        messages
    }
    #[test]
    fn chat_stop_press_is_single_shot_and_held_release_cannot_target_replacement() {
        let content = || iced::widget::Space::new().width(40).height(30);
        let mut old = press_control(content(), Some(1_u8));
        let mut tree = Tree::new(old.as_widget());
        assert_eq!(
            send(
                &mut old,
                &mut tree,
                mouse::Event::ButtonPressed(mouse::Button::Left)
            ),
            [1]
        );
        let mut replacement = press_control(content(), Some(2_u8));
        tree.diff(replacement.as_widget());
        assert!(send(
            &mut replacement,
            &mut tree,
            mouse::Event::ButtonReleased(mouse::Button::Left)
        )
        .is_empty());
        assert_eq!(
            send(
                &mut replacement,
                &mut tree,
                mouse::Event::ButtonPressed(mouse::Button::Left)
            ),
            [2]
        );
        assert!(send(
            &mut replacement,
            &mut tree,
            mouse::Event::ButtonReleased(mouse::Button::Left)
        )
        .is_empty());
        let mut disabled = press_control(content(), None);
        tree.diff(disabled.as_widget());
        assert!(send(
            &mut disabled,
            &mut tree,
            mouse::Event::ButtonPressed(mouse::Button::Left)
        )
        .is_empty());
    }
}
