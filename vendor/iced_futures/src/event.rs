//! Listen to runtime events.
use crate::MaybeSend;
use crate::core::event::{self, Event};
use crate::core::{keyboard, window};
use crate::subscription::{self, Subscription};

/// State sampled with a native keyboard event before widget dispatch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeyboardMetadata {
    /// Modifier state including the current modifier key's transition.
    pub modifiers: keyboard::Modifiers,
    /// Caps Lock state from the platform keyboard, when available.
    pub caps_lock: Option<bool>,
    /// Num Lock state from the platform keyboard, when available.
    pub num_lock: Option<bool>,
    /// Whether this was a keypad press.
    pub keypad_press: bool,
}

impl KeyboardMetadata {
    /// Effective Num Lock for a native keypad press.
    pub fn keypad_num_lock(self) -> Option<bool> {
        self.num_lock.filter(|_| self.keypad_press)
    }
}

/// Returns a [`Subscription`] to all the ignored runtime events.
///
/// This subscription will notify your application of any [`Event`] that was
/// not captured by any widget.
pub fn listen() -> Subscription<Event> {
    listen_with(|event, status, _window| match status {
        event::Status::Ignored => Some(event),
        event::Status::Captured => None,
    })
}

/// Creates a [`Subscription`] that listens and filters all the runtime events
/// with the provided function, producing messages accordingly.
///
/// This subscription will call the provided function for every [`Event`]
/// handled by the runtime. If the function:
///
/// - Returns `None`, the [`Event`] will be discarded.
/// - Returns `Some` message, the `Message` will be produced.
pub fn listen_with<Message>(
    f: fn(Event, event::Status, window::Id) -> Option<Message>,
) -> Subscription<Message>
where
    Message: 'static + MaybeSend,
{
    listen_filtered(f, move |event, status, window, _| f(event, status, window))
}

/// Like [`listen_with`], with the effective Num Lock state for native keypad
/// presses on platforms that report it.
pub fn listen_with_keypad<Message>(
    f: fn(Event, event::Status, window::Id, Option<bool>) -> Option<Message>,
) -> Subscription<Message>
where
    Message: 'static + MaybeSend,
{
    listen_filtered(f, move |event, status, window, keyboard| {
        f(event, status, window, keyboard.and_then(KeyboardMetadata::keypad_num_lock))
    })
}

/// Like [`listen_with`], with the state sampled for each native key transition.
pub fn listen_with_keyboard<Message>(
    f: fn(Event, event::Status, window::Id, Option<KeyboardMetadata>) -> Option<Message>,
) -> Subscription<Message>
where
    Message: 'static + MaybeSend,
{
    listen_filtered(f, f)
}

fn listen_filtered<Message>(
    identity: impl std::hash::Hash + 'static,
    f: impl Fn(Event, event::Status, window::Id, Option<KeyboardMetadata>) -> Option<Message>
        + MaybeSend
        + 'static,
) -> Subscription<Message>
where
    Message: 'static + MaybeSend,
{
    #[derive(Hash)]
    struct EventsWith;

    subscription::filter_map((EventsWith, identity), move |event| match event {
        subscription::Event::Interaction {
            event: Event::Window(window::Event::RedrawRequested(_)),
            ..
        }
        | subscription::Event::SystemThemeChanged(_) => None,
        subscription::Event::Interaction {
            window,
            event,
            status,
            keyboard,
        } => f(event, status, window, keyboard),
    })
}

/// Creates a [`Subscription`] that produces a message for every runtime event,
/// including the redraw request events.
///
/// **Warning:** This [`Subscription`], if unfiltered, may produce messages in
/// an infinite loop.
pub fn listen_raw<Message>(
    f: fn(Event, event::Status, window::Id) -> Option<Message>,
) -> Subscription<Message>
where
    Message: 'static + MaybeSend,
{
    #[derive(Hash)]
    struct RawEvents;

    subscription::filter_map((RawEvents, f), move |event| match event {
        subscription::Event::Interaction {
            window,
            event,
            status,
            ..
        } => f(event, status, window),
        subscription::Event::SystemThemeChanged(_) => None,
    })
}
