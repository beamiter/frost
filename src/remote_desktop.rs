//! Frost's native controls for the JWM viewer.

use crate::jwm_remote_session::{
    ClipboardDirection, ClipboardMode, ClipboardOptions, ConnectionOptions, RemoteDesktop, Scaling,
    Selection, SourceChoice, Viewer, CLIPBOARD_FILE_LIMIT_BYTES,
};
use crate::{Frost, Message};
use iced::widget::{button, checkbox, column, container, opaque, pick_list, row, text_input};
use iced::{Element, Length};

pub(crate) const HOST_INPUT: &str = "jwm-remote-desktop-host";

#[derive(Clone, Debug)]
pub(crate) enum Event {
    Address(String),
    KeyFile(String),
    Viewer(Viewer),
    Scaling(Scaling),
    Control(bool),
    ReconnectAutomatically(bool),
    Gestures(bool),
    Clipboard(bool),
    ClipboardMode(ClipboardMode),
    ClipboardDirection(ClipboardDirection),
    Connect,
    Disconnect,
    ReleaseInput,
    RequestInput(Selection),
    Select(SourceChoice),
    Close,
    Tick,
}

#[derive(Default)]
pub(crate) struct Panel {
    options: ConnectionOptions,
    clipboard: Option<(ClipboardMode, ClipboardDirection)>,
    session: RemoteDesktop,
}

impl Panel {
    pub(crate) fn active(&self) -> bool {
        self.session.active()
    }

    fn reconnect_available(&self) -> bool {
        self.options
            .backend
            .allows_reconnect(self.options.control, self.clipboard.is_some())
    }

    pub(crate) fn update(&mut self, event: Event) {
        let editable = !self.session.active();
        match event {
            Event::Address(value) if editable => {
                self.options.address = value.chars().take(255).collect();
            }
            Event::KeyFile(value) if editable => self.options.key_file = value,
            Event::Viewer(viewer) if editable && self.options.backend != viewer => {
                self.options.backend = viewer;
                self.options.control = false;
                self.options.wayland_gestures = false;
                self.clipboard = None;
            }
            Event::Scaling(scaling) if editable => self.options.scaling = scaling,
            Event::Control(control) if editable => {
                self.options.control = control;
                if !control {
                    self.options.wayland_gestures = false;
                }
            }
            Event::ReconnectAutomatically(enabled) if editable && self.reconnect_available() => {
                self.options.reconnect_automatically = enabled;
            }
            Event::Gestures(gestures)
                if editable && self.options.control && self.options.backend == Viewer::Wayland =>
            {
                self.options.wayland_gestures = gestures;
            }
            Event::Clipboard(enabled) if editable && self.options.backend == Viewer::X11 => {
                self.clipboard =
                    enabled.then_some((ClipboardMode::Text, ClipboardDirection::FromHost));
            }
            Event::ClipboardMode(mode) if editable => {
                if let Some((current, _)) = self.clipboard.as_mut() {
                    *current = mode;
                }
            }
            Event::ClipboardDirection(direction) if editable => {
                if let Some((_, current)) = self.clipboard.as_mut() {
                    *current = direction;
                }
            }
            Event::Connect => {
                let mut options = self.options.clone();
                options.clipboard = self
                    .clipboard
                    .filter(|_| options.backend == Viewer::X11)
                    .map(|(mode, direction)| {
                        let file_limit = if mode == ClipboardMode::All {
                            CLIPBOARD_FILE_LIMIT_BYTES
                        } else {
                            0
                        };
                        ClipboardOptions::new(mode, direction, file_limit)
                    });
                self.session.connect(options);
            }
            Event::Disconnect => self.session.disconnect(),
            Event::ReleaseInput => self.session.release_input(),
            Event::RequestInput(selection) => self.session.request_input(&selection),
            Event::Select(source) => self.session.select_source(&source),
            Event::Tick => self.session.refresh(),
            _ => {}
        }
        if !self.reconnect_available() || !editable && !self.session.active() {
            self.options.reconnect_automatically = false;
        }
    }

    pub(crate) fn view<'a>(&'a self, app: &'a Frost) -> Element<'a, Message> {
        let editable = !self.session.active();
        let mut host = text_input("Host or IP address", &self.options.address)
            .id(iced::widget::Id::new(HOST_INPUT))
            .style(app.popup_input_style());
        let mut key =
            text_input("Default JWM key", &self.options.key_file).style(app.popup_input_style());
        if editable {
            host = host.on_input(|value| Message::RemoteDesktop(Event::Address(value)));
            key = key.on_input(|value| Message::RemoteDesktop(Event::KeyFile(value)));
        }

        let mut control = checkbox(self.options.control).label("Allow keyboard and pointer");
        if editable {
            control = control.on_toggle(|value| Message::RemoteDesktop(Event::Control(value)));
        }
        let choices = row![
            app.popup_text("Viewer"),
            pick_list(
                if editable { &Viewer::CHOICES[..] } else { &[] },
                Some(self.options.backend),
                |viewer| Message::RemoteDesktop(Event::Viewer(viewer)),
            )
            .width(Length::Fixed(110.0)),
            app.popup_text("Scaling"),
            pick_list(
                if editable { &Scaling::CHOICES[..] } else { &[] },
                Some(self.options.scaling),
                |scaling| Message::RemoteDesktop(Event::Scaling(scaling)),
            )
            .width(Length::Fixed(95.0))
        ]
        .align_y(iced::Alignment::Center)
        .spacing(8);

        let mut body = column![
            app.popup_text("Remote Desktop"),
            row![app.popup_text("Host"), host]
                .align_y(iced::Alignment::Center)
                .spacing(12),
            row![app.popup_text("Key File"), key]
                .align_y(iced::Alignment::Center)
                .spacing(12),
            choices,
            control,
        ]
        .spacing(8);
        if self.options.control && self.options.backend == Viewer::Wayland {
            let mut gestures =
                checkbox(self.options.wayland_gestures).label("Modifier clicks and dragging");
            if editable {
                gestures =
                    gestures.on_toggle(|value| Message::RemoteDesktop(Event::Gestures(value)));
            }
            body = body.push(gestures);
        }
        if self.options.backend == Viewer::X11 {
            let mut clipboard = checkbox(self.clipboard.is_some()).label("Share clipboard");
            if editable {
                clipboard =
                    clipboard.on_toggle(|value| Message::RemoteDesktop(Event::Clipboard(value)));
            }
            body = body.push(clipboard);
            if let Some((mode, direction)) = self.clipboard {
                let content = pick_list(
                    if editable {
                        &ClipboardMode::CHOICES[..]
                    } else {
                        &[]
                    },
                    Some(mode),
                    |mode| Message::RemoteDesktop(Event::ClipboardMode(mode)),
                )
                .width(Length::Fixed(220.0));
                let direction = pick_list(
                    if editable {
                        &ClipboardDirection::CHOICES[..]
                    } else {
                        &[]
                    },
                    Some(direction),
                    |direction| Message::RemoteDesktop(Event::ClipboardDirection(direction)),
                )
                .width(Length::Fixed(120.0));
                body = body
                    .push(
                        row![app.popup_text("Content"), content]
                            .align_y(iced::Alignment::Center)
                            .spacing(8),
                    )
                    .push(
                        row![app.popup_text("Direction"), direction]
                            .align_y(iced::Alignment::Center)
                            .spacing(8),
                    );
                body = body
                    .push(app.popup_text("Both sides must grant clipboard sharing."))
                    .push(app.popup_text("Sending to the host can include the current clipboard."));
                if mode == ClipboardMode::All {
                    body = body.push(app.popup_text("File sharing is limited to 2 GiB per copy."));
                }
            }
        }
        if self.reconnect_available() {
            let mut reconnect =
                checkbox(self.options.reconnect_automatically).label("Reconnect automatically");
            if editable {
                reconnect = reconnect.on_toggle(|enabled| {
                    Message::RemoteDesktop(Event::ReconnectAutomatically(enabled))
                });
            }
            body = body.push(reconnect);
        }
        body = body.push(app.popup_text(self.session.status()));

        if !self.session.closing() {
            if let Some(current) = self.session.current_source() {
                body = body.push(
                    row![
                        app.popup_text("Source"),
                        pick_list(self.session.sources(), Some(current), |source| {
                            Message::RemoteDesktop(Event::Select(source))
                        }),
                    ]
                    .align_y(iced::Alignment::Center)
                    .spacing(8),
                );
            }
        }
        let clipboard = self.session.clipboard_summary();
        if !clipboard.is_empty() {
            body = body.push(app.popup_text(clipboard));
        }
        for line in [self.session.metrics(), self.session.feedback()] {
            if !line.is_empty() {
                body = body.push(app.popup_text(line));
            }
        }

        let input = if let Some(source) = self
            .session
            .current_source()
            .filter(|_| self.session.can_request_input())
        {
            button(app.popup_text("Request Input")).on_press(Message::RemoteDesktop(
                Event::RequestInput(source.selection.clone()),
            ))
        } else {
            button(app.popup_text("Release Input")).on_press_maybe(
                self.session
                    .can_release_input()
                    .then(|| Message::RemoteDesktop(Event::ReleaseInput)),
            )
        };
        body = body.push(
            row![
                button(app.popup_text("Connect"))
                    .on_press_maybe(editable.then(|| Message::RemoteDesktop(Event::Connect))),
                input,
                button(app.popup_text("Disconnect")).on_press_maybe(
                    (!editable && !self.session.closing())
                        .then(|| Message::RemoteDesktop(Event::Disconnect)),
                ),
                button(app.popup_text("Close")).on_press(Message::RemoteDesktop(Event::Close)),
            ]
            .spacing(8),
        );
        let panel = container(body)
            .width(Length::Fixed(450.0))
            .padding(12)
            .style(app.popup_style());
        opaque(
            container(panel)
                .center_x(Length::Fill)
                .center_y(Length::Fill),
        )
    }
}
