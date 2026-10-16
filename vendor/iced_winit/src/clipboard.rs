//! Access the clipboard.

use crate::core::clipboard::Kind;
use crate::futures::futures::channel::oneshot;
use std::sync::{Arc, Mutex};
use winit::window::{Window, WindowId};

/// A buffer for short-term storage and transfer within and between
/// applications.
pub struct Clipboard {
    state: State,
}

enum State {
    Connected {
        clipboard: window_clipboard::Clipboard,
        // Held until drop to satisfy the safety invariants of
        // `window_clipboard::Clipboard`.
        //
        // Note that the field ordering is load-bearing.
        #[allow(dead_code)]
        window: Arc<Window>,
    },
    Unavailable,
}

impl Clipboard {
    /// Creates a new [`Clipboard`] for the given window.
    pub fn connect(window: Arc<Window>) -> Clipboard {
        // SAFETY: The window handle will stay alive throughout the entire
        // lifetime of the `window_clipboard::Clipboard` because we hold
        // the `Arc<Window>` together with `State`, and enum variant fields
        // get dropped in declaration order.
        #[allow(unsafe_code)]
        let clipboard = unsafe {
            #[cfg(target_os = "linux")]
            {
                window_clipboard::Clipboard::connect_bounded(&window)
            }
            #[cfg(not(target_os = "linux"))]
            {
                window_clipboard::Clipboard::connect(&window)
            }
        };

        let state = match clipboard {
            Ok(clipboard) => State::Connected { clipboard, window },
            Err(_) => State::Unavailable,
        };

        Clipboard { state }
    }

    /// Creates a new [`Clipboard`] that isn't associated with a window.
    /// This clipboard will never contain a copied value.
    pub fn unconnected() -> Clipboard {
        Clipboard {
            state: State::Unavailable,
        }
    }

    /// Reads the current content of the [`Clipboard`] as text.
    pub fn read(&self, kind: Kind) -> Option<String> {
        match &self.state {
            State::Connected { clipboard, .. } => match kind {
                Kind::Standard => clipboard.read().ok(),
                Kind::Primary => clipboard.read_primary().and_then(Result::ok),
            },
            State::Unavailable => None,
        }
    }

    /// Resolves an Action read without waiting on the Linux clipboard owner.
    /// Synchronous core-widget reads intentionally retain their separate API.
    pub fn read_async(&self, kind: Kind, channel: oneshot::Sender<Option<String>>) {
        #[cfg(not(target_os = "linux"))]
        {
            let _ = channel.send(self.read(kind));
        }
        #[cfg(target_os = "linux")]
        {
            let State::Connected { clipboard, .. } = &self.state else {
                let _ = channel.send(None);
                return;
            };
            let sender = Arc::new(Mutex::new(Some(channel)));
            let cancelled = sender.clone();
            let completed = sender.clone();
            let result = clipboard.read_async(
                matches!(kind, Kind::Primary),
                1024 * 1024,
                std::time::Instant::now() + std::time::Duration::from_secs(3),
                Box::new(move || match cancelled.lock() {
                    Ok(sender) => sender.as_ref().is_none_or(oneshot::Sender::is_canceled),
                    Err(_) => true,
                }),
                Box::new(move |result| {
                    // Take under lock, invoke send only after releasing it.
                    let channel = completed.lock().ok().and_then(|mut sender| sender.take());
                    if let Some(channel) = channel {
                        let _ = channel.send(result.ok());
                    }
                }),
            );
            if result.is_err() {
                let channel = sender.lock().ok().and_then(|mut sender| sender.take());
                if let Some(channel) = channel {
                    let _ = channel.send(None);
                }
            }
        }
    }

    /// Writes the given text contents to the [`Clipboard`].
    pub fn write(&mut self, kind: Kind, contents: String) {
        match &mut self.state {
            State::Connected { clipboard, .. } => {
                let result = match kind {
                    Kind::Standard => clipboard.write(contents),
                    Kind::Primary => clipboard.write_primary(contents).unwrap_or(Ok(())),
                };

                match result {
                    Ok(()) => {}
                    Err(error) => {
                        log::warn!("error writing to clipboard: {error}");
                    }
                }
            }
            State::Unavailable => {}
        }
    }

    /// Returns the identifier of the window used to create the [`Clipboard`], if any.
    pub fn window_id(&self) -> Option<WindowId> {
        match &self.state {
            State::Connected { window, .. } => Some(window.id()),
            State::Unavailable => None,
        }
    }
}

impl crate::core::Clipboard for Clipboard {
    fn read(&self, kind: Kind) -> Option<String> {
        self.read(kind)
    }

    fn write(&mut self, kind: Kind, contents: String) {
        self.write(kind, contents);
    }
}
