//! Optional native observation before Iced conversion loses physical-key data.
//!
//! The handler is process-local and installed once by the application. Without
//! an installed handler the upstream event path is unchanged. Only a real key
//! press can be consumed; releases and non-key events always continue.

use crate::core::window;
use std::sync::OnceLock;
use winit::event::{ElementState, KeyEvent, WindowEvent};

type Handler = dyn Fn(window::Id, &WindowEvent) -> bool + Send + Sync;
static HANDLER: OnceLock<Box<Handler>> = OnceLock::new();

/// Installs a fast native observer once, returning false if already installed.
/// Returning true requests consumption of a real key press only. The callback
/// must not reenter Iced or wait for application event processing.
pub fn install(handler: impl Fn(window::Id, &WindowEvent) -> bool + Send + Sync + 'static) -> bool {
    HANDLER.set(Box::new(handler)).is_ok()
}

pub(crate) fn observe(id: window::Id, event: &WindowEvent) -> bool {
    let Some(handler) = HANDLER.get() else {
        return false;
    };
    handler(id, event)
        && matches!(
            event,
            WindowEvent::KeyboardInput {
                event: KeyEvent {
                    state: ElementState::Pressed,
                    ..
                },
                is_synthetic: false,
                ..
            }
        )
}
