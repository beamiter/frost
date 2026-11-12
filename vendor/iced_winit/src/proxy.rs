use crate::futures::futures::{
    Future, Sink,
    task::{Context, Poll},
};
use crate::graphics::shell;
use crate::runtime::Action;
use std::{
    collections::HashMap,
    fmt,
    pin::Pin,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    task::Waker,
};

const MAX_IN_FLIGHT: usize = 256;
const MAX_WAITERS: usize = 256;

/// Runtime-owned events. Opaque `Action<T>` payload bytes are NOT bounded here.
#[derive(Debug)]
pub enum Event<T: 'static> {
    Action {
        action: Action<T>,
        lease: Lease,
    },
    /// One coalesced request to poll/rebuild/redraw, independent of data capacity.
    Wake,
    /// Explicit exit action; never competes with ordinary data for capacity.
    Exit,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SendError {
    Closed,
    TooManyWaiters,
}
impl fmt::Display for SendError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Closed => "event loop closed",
            Self::TooManyWaiters => "proxy waiter limit exceeded; input quarantined",
        })
    }
}
impl std::error::Error for SendError {}

#[derive(Debug, Default)]
struct State {
    in_flight: usize,
    next_id: u64,
    closed: bool,
    waiters: HashMap<u64, Waker>,
}
#[derive(Debug, Default)]
struct Ledger {
    state: Mutex<State>,
}

impl Ledger {
    fn id(&self) -> u64 {
        let mut s = self.state.lock().expect("proxy ledger poisoned");
        s.next_id = s.next_id.checked_add(1).expect("proxy identity exhausted");
        s.next_id
    }
    fn acquire(
        self: &Arc<Self>,
        waiter: Option<(u64, &Waker)>,
    ) -> Result<Option<Lease>, SendError> {
        let mut s = self.state.lock().expect("proxy ledger poisoned");
        if s.closed {
            return Err(SendError::Closed);
        }
        if s.in_flight < MAX_IN_FLIGHT {
            s.in_flight += 1;
            if let Some((id, _)) = waiter {
                let _ = s.waiters.remove(&id);
            }
            return Ok(Some(Lease {
                ledger: self.clone(),
            }));
        }
        if let Some((id, waker)) = waiter {
            if !s.waiters.contains_key(&id) && s.waiters.len() >= MAX_WAITERS {
                return Err(SendError::TooManyWaiters);
            }
            let _ = s.waiters.insert(id, waker.clone());
        }
        Ok(None)
    }
    fn forget(&self, id: u64) {
        let _ = self
            .state
            .lock()
            .expect("proxy ledger poisoned")
            .waiters
            .remove(&id);
    }
    fn close(&self) {
        let waiters = {
            let mut s = self.state.lock().expect("proxy ledger poisoned");
            s.closed = true;
            std::mem::take(&mut s.waiters)
        };
        for (_, waker) in waiters {
            waker.wake();
        }
    }
}

/// A slot stays occupied until the event AND its derived batch have completed.
/// The shell must move this lease with deferred messages, not drop it on dequeue.
#[derive(Debug)]
pub struct Lease {
    ledger: Arc<Ledger>,
}
impl Drop for Lease {
    fn drop(&mut self) {
        let waiters = {
            let mut s = self.ledger.state.lock().expect("proxy ledger poisoned");
            s.in_flight = s
                .in_flight
                .checked_sub(1)
                .expect("proxy lease released twice");
            std::mem::take(&mut s.waiters)
        };
        for (_, waker) in waiters {
            waker.wake();
        }
    }
}

/// Aggregate admission across every clone, with bounded cancellable waiters.
/// Bounds retained action COUNT, not unknown generic payload heap or OS queues.
#[derive(Debug)]
pub struct Proxy<T: 'static> {
    raw: winit::event_loop::EventLoopProxy<Event<T>>,
    ledger: Arc<Ledger>,
    id: u64,
    reserved: Option<Lease>,
    wake_pending: Arc<AtomicBool>,
    layout_pending: Arc<AtomicBool>,
    exit_pending: Arc<AtomicBool>,
    disconnected: bool,
}
impl<T: 'static> Clone for Proxy<T> {
    fn clone(&self) -> Self {
        Self {
            raw: self.raw.clone(),
            ledger: self.ledger.clone(),
            id: self.ledger.id(),
            reserved: None,
            wake_pending: self.wake_pending.clone(),
            layout_pending: self.layout_pending.clone(),
            exit_pending: self.exit_pending.clone(),
            disconnected: self.disconnected,
        }
    }
}
impl<T: 'static> Drop for Proxy<T> {
    fn drop(&mut self) {
        self.ledger.forget(self.id);
    }
}

impl<T: 'static> Proxy<T> {
    /// Creates a shared bounded proxy and its compatibility worker.
    pub fn new(
        raw: winit::event_loop::EventLoopProxy<Event<T>>,
    ) -> (Self, impl Future<Output = ()>) {
        let ledger = Arc::new(Ledger::default());
        (
            Self {
                raw,
                id: ledger.id(),
                ledger,
                reserved: None,
                wake_pending: Arc::new(AtomicBool::new(false)),
                layout_pending: Arc::new(AtomicBool::new(false)),
                exit_pending: Arc::new(AtomicBool::new(false)),
                disconnected: false,
            },
            std::future::ready(()),
        )
    }
    fn transmit(&self, event: Event<T>) -> Result<(), SendError> {
        self.raw.send_event(event).map_err(|_| {
            self.ledger.close();
            SendError::Closed
        })
    }
    fn wake(&self) {
        if !self.wake_pending.swap(true, Ordering::AcqRel) {
            let _ = self.transmit(Event::Wake);
        }
    }
    /// Acknowledge only when the shell has scheduled the requested poll/rebuild.
    pub fn acknowledge_wake(&self) {
        self.wake_pending.store(false, Ordering::Release);
        // A layout request may have raced the consumer's earlier take while
        // Wake was still coalesced. Re-arm it rather than losing that request.
        if self.layout_pending.load(Ordering::Acquire) {
            self.wake();
        }
    }
    /// Consume the layout intent when the shell actually rebuilds its batch.
    pub fn take_layout_request(&self) -> bool {
        self.layout_pending.swap(false, Ordering::AcqRel)
    }
    fn exit(&self) -> Result<(), SendError> {
        if !self.exit_pending.swap(true, Ordering::AcqRel) {
            self.transmit(Event::Exit)?;
        }
        Ok(())
    }
    fn overload(&self) {
        crate::futures::subscription::Event::report_input_overload();
        self.wake();
    }
    /// Attempts nonblocking delivery, quarantining input on overload.
    pub fn send(&self, value: T) {
        self.send_action(Action::Output(value));
    }
    /// Nonblocking admission. Overflow explicitly quarantines host input.
    pub fn send_action(&self, action: Action<T>) {
        if self.disconnected {
            return;
        }
        if matches!(&action, Action::Exit) {
            let _ = self.exit();
            return;
        }
        match self.ledger.acquire(None) {
            Ok(Some(lease)) => {
                let _ = self.transmit(Event::Action { action, lease });
            }
            Ok(None) | Err(SendError::TooManyWaiters) => self.overload(),
            Err(SendError::Closed) => {}
        }
    }
}
// Runtime's public Sink contract fixes the error to futures mpsc::SendError.
// Construct its disconnected variant through the supported channel API.
fn sink_closed_error() -> crate::futures::futures::channel::mpsc::SendError {
    let (mut sender, receiver) = crate::futures::futures::channel::mpsc::channel::<()>(0);
    drop(receiver);
    sender
        .try_send(())
        .expect_err("receiver was dropped")
        .into_send_error()
}

impl<T: 'static> Sink<Action<T>> for Proxy<T> {
    type Error = crate::futures::futures::channel::mpsc::SendError;
    fn poll_ready(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        if self.disconnected {
            return Poll::Ready(Err(sink_closed_error()));
        }
        if self.reserved.is_some() {
            return Poll::Ready(Ok(()));
        }
        match self.ledger.acquire(Some((self.id, cx.waker()))) {
            Ok(Some(lease)) => {
                self.reserved = Some(lease);
                Poll::Ready(Ok(()))
            }
            Ok(None) => Poll::Pending,
            Err(error) => {
                if error == SendError::TooManyWaiters {
                    self.overload();
                }
                Poll::Ready(Err(sink_closed_error()))
            }
        }
    }
    fn start_send(mut self: Pin<&mut Self>, action: Action<T>) -> Result<(), Self::Error> {
        if self.disconnected {
            return Err(sink_closed_error());
        }
        if matches!(&action, Action::Exit) {
            drop(self.reserved.take());
            return self.exit().map_err(|_| sink_closed_error());
        }
        let Some(lease) = self.reserved.take() else {
            // A caller violating Sink's poll_ready contract must not bypass limits.
            self.overload();
            return Err(sink_closed_error());
        };
        self.transmit(Event::Action { action, lease })
            .map_err(|_| sink_closed_error())
    }
    fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        // There is no intermediate worker: accepted sends already entered winit.
        Poll::Ready(Ok(()))
    }
    fn poll_close(
        mut self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
    ) -> Poll<Result<(), Self::Error>> {
        self.disconnected = true;
        drop(self.reserved.take());
        self.ledger.forget(self.id);
        Poll::Ready(Ok(()))
    }
}
impl<T: Send> shell::Notifier for Proxy<T> {
    fn request_redraw(&self) {
        self.wake();
    }
    fn invalidate_layout(&self) {
        self.layout_pending.store(true, Ordering::Release);
        self.wake();
    }
}

#[cfg(test)]
#[path = "proxy_tests.rs"]
mod tests;
