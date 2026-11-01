//! Window-scoped, captured metadata for producer-confirmed X11 scene feedback.
//! A successful Compositor::present return alone never invokes an observer.
use crate::core::window;
use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

type Callback = Box<dyn FnOnce(u64) + Send>;
type Capture = Arc<dyn Fn() -> Option<Callback> + Send + Sync>;
struct Observer {
    before: Arc<dyn Fn() + Send + Sync>,
    capture: Capture,
    lease: Arc<()>,
    pending: BTreeMap<u64, Callback>,
}
fn observers() -> &'static Mutex<HashMap<window::Id, Observer>> {
    static OBSERVERS: OnceLock<Mutex<HashMap<window::Id, Observer>>> = OnceLock::new();
    OBSERVERS.get_or_init(Default::default)
}
/// RAII registration. Metadata is captured at submission, never on a later ack.
pub struct Registration {
    id: window::Id,
    lease: Arc<()>,
}
/// Register one window. `capture` runs after drawing and before native present;
/// returning `None` avoids a proof request. `presented` receives that captured
/// value only after exact producer publication and compositor scene feedback.
pub fn register<S: Send + 'static>(
    id: window::Id,
    before_draw: impl Fn() + Send + Sync + 'static,
    capture: impl Fn() -> Option<S> + Send + Sync + 'static,
    presented: impl Fn(S) + Send + Sync + 'static,
) -> Registration {
    register_with_receipt(id, before_draw, capture, move |_, snapshot| {
        presented(snapshot)
    })
}

/// As `register`, additionally identifying the exact original native receipt.
/// The serial is supplied only by `presented(id, serial)` after its matching
/// producer/scene acknowledgement, never by a later capture or a UI counter.
pub fn register_with_receipt<S: Send + 'static>(
    id: window::Id,
    before_draw: impl Fn() + Send + Sync + 'static,
    capture: impl Fn() -> Option<S> + Send + Sync + 'static,
    presented: impl Fn(u64, S) + Send + Sync + 'static,
) -> Registration {
    let presented = Arc::new(presented);
    let capture = Arc::new(move || {
        let snapshot = capture()?;
        let after = presented.clone();
        Some(Box::new(move |serial| after(serial, snapshot)) as Callback)
    });
    let lease = Arc::new(());
    let _ = observers().lock().unwrap().insert(
        id,
        Observer {
            before: Arc::new(before_draw),
            capture,
            lease: lease.clone(),
            pending: Default::default(),
        },
    );
    Registration { id, lease }
}
impl Drop for Registration {
    fn drop(&mut self) {
        let mut observers = observers().lock().unwrap();
        if observers
            .get(&self.id)
            .is_some_and(|o| Arc::ptr_eq(&o.lease, &self.lease))
        {
            let _ = observers.remove(&self.id);
        }
    }
}
pub(crate) fn begin_draw(id: window::Id) {
    let callback = observers()
        .lock()
        .unwrap()
        .get(&id)
        .map(|o| o.before.clone());
    if let Some(callback) = callback {
        callback();
    }
}
pub(crate) fn prepare(id: window::Id) -> Option<u64> {
    let (capture, lease) = observers()
        .lock()
        .unwrap()
        .get(&id)
        .map(|o| (o.capture.clone(), o.lease.clone()))?;
    let callback = capture()?;
    let mut observers = observers().lock().unwrap();
    let observer = observers.get_mut(&id)?;
    if !Arc::ptr_eq(&observer.lease, &lease) {
        return None;
    }
    let serial = next_serial()?;
    if observer.pending.len() == 64 {
        let _ = observer.pending.pop_first();
    }
    let _ = observer.pending.insert(serial, callback);
    Some(serial)
}
pub(crate) fn next_serial() -> Option<u64> {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    let mut value = NEXT.load(Ordering::Relaxed);
    loop {
        let next = value.checked_add(1)?;
        match NEXT.compare_exchange_weak(value, next, Ordering::Relaxed, Ordering::Relaxed) {
            Ok(previous) => return Some(previous),
            Err(current) => value = current,
        }
    }
}
pub(crate) fn cancel(id: window::Id, serial: u64) {
    if let Some(observer) = observers().lock().unwrap().get_mut(&id) {
        let _ = observer.pending.remove(&serial);
    }
}
pub(crate) fn presented(id: window::Id, serial: u64) {
    let callback = observers()
        .lock()
        .unwrap()
        .get_mut(&id)
        .and_then(|o| o.pending.remove(&serial));
    if let Some(callback) = callback {
        callback(serial);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    #[test]
    fn native_receipt_serial_remains_bound_to_its_original_captured_tree() {
        let id = window::Id::unique();
        let tree = Arc::new(AtomicU64::new(209));
        let seen = Arc::new(Mutex::new(None));
        let captured = tree.clone();
        let output = seen.clone();
        let _registration = register_with_receipt(
            id,
            || {},
            move || Some(captured.load(Ordering::Acquire)),
            move |native, snapshot| *output.lock().unwrap() = Some((native, snapshot)),
        );
        let original = prepare(id).unwrap();
        tree.store(999, Ordering::Release);
        presented(id, original + 10000);
        assert_eq!(*seen.lock().unwrap(), None);
        presented(id, original);
        assert_eq!(*seen.lock().unwrap(), Some((original, 209)));
        let canceled = prepare(id).unwrap();
        cancel(id, canceled);
        presented(id, canceled);
        assert_eq!(*seen.lock().unwrap(), Some((original, 209)));
    }
    #[test]
    fn delayed_receipt_uses_captured_metadata_and_is_window_scoped() {
        let a = window::Id::unique();
        let b = window::Id::unique();
        let frame = Arc::new(AtomicU64::new(11));
        let out = Arc::new(AtomicU64::new(0));
        let source = frame.clone();
        let target = out.clone();
        let guard = register(
            a,
            || {},
            move || Some(source.load(Ordering::Acquire)),
            move |serial| target.store(serial, Ordering::Release),
        );
        let first = prepare(a).unwrap();
        frame.store(99, Ordering::Release);
        presented(b, first);
        assert_eq!(out.load(Ordering::Acquire), 0);
        presented(a, first);
        assert_eq!(out.load(Ordering::Acquire), 11);
        let canceled = prepare(a).unwrap();
        cancel(a, canceled);
        presented(a, canceled);
        assert_eq!(out.load(Ordering::Acquire), 11);
        let closed = prepare(a).unwrap();
        drop(guard);
        presented(a, closed);
        assert_eq!(out.load(Ordering::Acquire), 11);
    }
    #[test]
    fn replaced_registration_rejects_old_receipts_and_old_guard_cannot_remove_it() {
        let id = window::Id::unique();
        let value = Arc::new(AtomicU64::new(0));
        let first_out = value.clone();
        let old_guard = register(
            id,
            || {},
            || Some(10),
            move |n| first_out.store(n, Ordering::Release),
        );
        let old = prepare(id).unwrap();
        let second_out = value.clone();
        let _guard = register(
            id,
            || {},
            || Some(20),
            move |n| second_out.store(n, Ordering::Release),
        );
        let new = prepare(id).unwrap();
        assert_ne!(old, new);
        drop(old_guard);
        presented(id, old);
        assert_eq!(value.load(Ordering::Acquire), 0);
        presented(id, new);
        assert_eq!(value.load(Ordering::Acquire), 20);
    }
}
