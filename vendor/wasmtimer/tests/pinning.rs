//! Regressions for the local replacement of pin-utils projections.
use std::{cell::Cell, marker::PhantomPinned, pin::Pin, rc::Rc, task::Context, task::Poll};

use futures::{task::noop_waker_ref, Future};
use std::time::Duration;
use wasm_bindgen_test::wasm_bindgen_test;
use wasmtimer::{
    std::Instant,
    tokio::{timeout, timeout_at},
};

struct AddressSensitiveFuture {
    address: Cell<Option<usize>>,
    dropped: Rc<Cell<bool>>,
    _pin: PhantomPinned,
}

impl Future for AddressSensitiveFuture {
    type Output = u8;

    fn poll(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.as_ref().get_ref();
        let address = this as *const Self as usize;
        if let Some(previous) = this.address.get() {
            assert_eq!(address, previous, "pinned inner future moved between polls");
            Poll::Ready(42)
        } else {
            this.address.set(Some(address));
            Poll::Pending
        }
    }
}

impl Drop for AddressSensitiveFuture {
    fn drop(&mut self) {
        if let Some(address) = self.address.get() {
            assert_eq!(self as *const Self as usize, address);
        }
        self.dropped.set(true);
    }
}

fn address_sensitive_future(dropped: Rc<Cell<bool>>) -> AddressSensitiveFuture {
    AddressSensitiveFuture {
        address: Cell::new(None),
        dropped,
        _pin: PhantomPinned,
    }
}

#[wasm_bindgen_test]
fn timeout_preserves_non_unpin_future_address() {
    let dropped = Rc::new(Cell::new(false));
    let mut timeout = Box::pin(timeout(
        Duration::from_secs(60),
        address_sensitive_future(dropped.clone()),
    ));
    let mut cx = Context::from_waker(noop_waker_ref());
    assert!(timeout.as_mut().poll(&mut cx).is_pending());
    assert_eq!(timeout.as_mut().poll(&mut cx), Poll::Ready(Ok(42)));
    drop(timeout);
    assert!(dropped.get());
}

#[wasm_bindgen_test]
fn cancelling_timeout_drops_non_unpin_future_in_place() {
    let dropped = Rc::new(Cell::new(false));
    let mut timeout = Box::pin(timeout(
        Duration::from_secs(60),
        address_sensitive_future(dropped.clone()),
    ));
    let mut cx = Context::from_waker(noop_waker_ref());
    assert!(timeout.as_mut().poll(&mut cx).is_pending());
    drop(timeout);
    assert!(dropped.get());
}

#[wasm_bindgen_test]
fn ready_future_still_wins_over_expired_deadline() {
    let mut timeout = Box::pin(timeout_at(Instant::now(), async { 42 }));
    let mut cx = Context::from_waker(noop_waker_ref());
    assert_eq!(timeout.as_mut().poll(&mut cx), Poll::Ready(Ok(42)));
}
