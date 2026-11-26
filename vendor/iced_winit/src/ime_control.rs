//! Retire native composition when an application changes its logical text owner.
//! This controls the real platform input context; it never fabricates IME text.
use crate::core::window::Id;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock, Weak};
use winit::window::Window;

static WINDOWS: OnceLock<Mutex<HashMap<Id, Weak<Window>>>> = OnceLock::new();

pub(crate) fn register(id: Id, window: &Arc<Window>) {
    let _ = WINDOWS
        .get_or_init(Default::default)
        .lock()
        .unwrap()
        .insert(id, Arc::downgrade(window));
}

pub(crate) fn unregister(id: Id) {
    if let Some(windows) = WINDOWS.get() {
        let _ = windows.lock().unwrap().remove(&id);
    }
}

/// Discard the old owner's native composition and reopen a fresh platform cycle.
/// The weak registration neither retains a closed window nor waits for painting.
/// Returns false when the window has already been removed.
pub fn reset(id: Id) -> bool {
    let window = WINDOWS
        .get()
        .and_then(|windows| windows.lock().unwrap().get(&id).and_then(Weak::upgrade));
    let Some(window) = window else {
        return false;
    };
    window.set_ime_allowed(false);
    window.set_ime_allowed(true);
    true
}
