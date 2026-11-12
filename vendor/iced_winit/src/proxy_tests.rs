use super::*;
use crate::futures::futures::task::{ArcWake, noop_waker, waker};
use std::sync::atomic::AtomicUsize;

#[derive(Default)]
struct Counter(AtomicUsize);
impl ArcWake for Counter {
    fn wake_by_ref(arc_self: &Arc<Self>) {
        let _ = arc_self.0.fetch_add(1, Ordering::SeqCst);
    }
}

fn fill(ledger: &Arc<Ledger>) -> Vec<Lease> {
    (0..MAX_IN_FLIGHT)
        .map(|_| ledger.acquire(None).unwrap().unwrap())
        .collect()
}

#[test]
fn clones_share_capacity_and_dequeue_does_not_release_lease() {
    let ledger = Arc::new(Ledger::default());
    let mut leases = fill(&ledger);
    let clone = ledger.clone();
    assert!(clone.acquire(None).unwrap().is_none());
    // Move into the deferred batch without releasing it.
    let moved_to_batch = leases.pop().unwrap();
    assert!(clone.acquire(None).unwrap().is_none());
    drop(moved_to_batch);
    let replacement = clone.acquire(None).unwrap().unwrap();
    assert!(ledger.acquire(None).unwrap().is_none());
    drop(replacement);
    drop(leases);
    assert_eq!(ledger.state.lock().unwrap().in_flight, 0);
}

#[test]
fn all_registered_producers_are_woken_without_a_single_waker_overwrite() {
    let ledger = Arc::new(Ledger::default());
    let mut leases = fill(&ledger);
    let a = Arc::new(Counter::default());
    let b = Arc::new(Counter::default());
    assert!(
        ledger
            .acquire(Some((ledger.id(), &waker(a.clone()))))
            .unwrap()
            .is_none()
    );
    assert!(
        ledger
            .acquire(Some((ledger.id(), &waker(b.clone()))))
            .unwrap()
            .is_none()
    );
    drop(leases.pop());
    assert_eq!(a.0.load(Ordering::SeqCst), 1);
    assert_eq!(b.0.load(Ordering::SeqCst), 1);
    assert!(ledger.state.lock().unwrap().waiters.is_empty());
}

#[test]
fn wait_registration_is_bounded_cancellable_and_replaces_existing_waker() {
    let ledger = Arc::new(Ledger::default());
    let _leases = fill(&ledger);
    let wake = noop_waker();
    let ids: Vec<_> = (0..MAX_WAITERS).map(|_| ledger.id()).collect();
    for &id in &ids {
        assert!(ledger.acquire(Some((id, &wake))).unwrap().is_none());
    }
    assert_eq!(ledger.state.lock().unwrap().waiters.len(), MAX_WAITERS);
    assert!(matches!(
        ledger.acquire(Some((ledger.id(), &wake))),
        Err(SendError::TooManyWaiters)
    ));
    // Repolling an existing waiter is not a new allocation or another slot.
    assert!(ledger.acquire(Some((ids[0], &wake))).unwrap().is_none());
    ledger.forget(ids[0]);
    assert!(
        ledger
            .acquire(Some((ledger.id(), &wake)))
            .unwrap()
            .is_none()
    );
    assert_eq!(ledger.state.lock().unwrap().waiters.len(), MAX_WAITERS);
}

#[test]
fn close_wakes_waiters_and_rejects_new_admission_without_leaking_slots() {
    let ledger = Arc::new(Ledger::default());
    let leases = fill(&ledger);
    let counter = Arc::new(Counter::default());
    assert!(
        ledger
            .acquire(Some((ledger.id(), &waker(counter.clone()))))
            .unwrap()
            .is_none()
    );
    ledger.close();
    assert_eq!(counter.0.load(Ordering::SeqCst), 1);
    assert!(matches!(ledger.acquire(None), Err(SendError::Closed)));
    drop(leases);
    assert_eq!(ledger.state.lock().unwrap().in_flight, 0);
    assert!(ledger.state.lock().unwrap().waiters.is_empty());
}

#[test]
fn cancelled_waiter_is_not_kept_alive_by_ledger() {
    let ledger = Arc::new(Ledger::default());
    let _leases = fill(&ledger);
    let counter = Arc::new(Counter::default());
    let weak = Arc::downgrade(&counter);
    let id = ledger.id();
    assert!(
        ledger
            .acquire(Some((id, &waker(counter.clone()))))
            .unwrap()
            .is_none()
    );
    drop(counter);
    assert!(weak.upgrade().is_some());
    ledger.forget(id);
    assert!(weak.upgrade().is_none());
}
