//! Frost's scoped native presentation-submission notification.
//! The submission callback runs only after Compositor::present returns Ok.
//! A separate before-draw callback only invalidates stale markers. Neither asserts GPU
//! completion, compositor acknowledgement, physical scanout or surface destruction.
use crate::core::window;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};
type Callback = Arc<dyn Fn() + Send + Sync>;
fn observers() -> &'static Mutex<HashMap<window::Id, (Callback, Callback)>> {
    static OBSERVERS: OnceLock<Mutex<HashMap<window::Id, (Callback, Callback)>>> = OnceLock::new();
    OBSERVERS.get_or_init(Default::default)
}
/// RAII registration for one native window; dropping it cancels callbacks.
pub struct Registration {
    id: window::Id,
}
/// The registration must be owned by the window and dropped when it closes.
/// `before_draw` invalidates previous frame markers before the native tree draws;
/// `callback` runs only after successful native presentation submission.
pub fn register(
    id: window::Id,
    before_draw: impl Fn() + Send + Sync + 'static,
    callback: impl Fn() + Send + Sync + 'static,
) -> Registration {
    let _ = observers()
        .lock()
        .unwrap()
        .insert(id, (Arc::new(before_draw), Arc::new(callback)));
    Registration { id }
}
impl Drop for Registration {
    fn drop(&mut self) {
        let _ = observers().lock().unwrap().remove(&self.id);
    }
}
pub(crate) fn begin_draw(id: window::Id) {
    let callback = observers()
        .lock()
        .unwrap()
        .get(&id)
        .map(|(before, _)| before.clone());
    if let Some(callback) = callback {
        callback();
    }
}
pub(crate) fn submitted(id: window::Id) {
    let callback = observers()
        .lock()
        .unwrap()
        .get(&id)
        .map(|(_, after)| after.clone());
    if let Some(callback) = callback {
        callback();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    #[test]
    fn callbacks_are_window_scoped_and_canceled_on_drop() {
        let a = window::Id::unique();
        let b = window::Id::unique();
        let count = Arc::new(AtomicUsize::new(0));
        let observer = count.clone();
        let guard = register(
            a,
            || {},
            move || {
                let _ = observer.fetch_add(1, Ordering::Relaxed);
            },
        );
        submitted(b);
        assert_eq!(count.load(Ordering::Relaxed), 0);
        submitted(a);
        assert_eq!(count.load(Ordering::Relaxed), 1);
        drop(guard);
        submitted(a);
        assert_eq!(count.load(Ordering::Relaxed), 1);
    }
}
