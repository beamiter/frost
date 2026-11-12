//! Bounded transport from one blocking AI request to the UI task stream.
//!
//! Preview fragments are lossless and FIFO until cancellation/overflow. A full
//! inbox backpressures only the blocking worker, never the UI. The final result
//! has a separate reserved slot, so it cannot be lost behind a full preview
//! queue. This does not bound the provider's own transport buffers or Iced's
//! native subscription inbox; those are separate resource boundaries.
use iced::futures::{task::AtomicWaker, Stream};
use jterm_core::ai::AiCancellationToken;
use std::collections::VecDeque;
use std::pin::Pin;
use std::sync::{Arc, Condvar, Mutex};
use std::task::{Context, Poll};
use std::time::Duration;

pub(crate) const MAX_AGENT_MODEL_REPLY_BYTES: usize = 128 * 1024;
const FRAGMENT_BYTES: usize = 4096;
const FRAGMENT_SLOTS: usize = 8;
const ERROR_BYTES: usize = 4096;
const CANCEL_POLL: Duration = Duration::from_millis(20);

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Event {
    Delta(String),
    Finished(Result<String, String>),
}

#[derive(Default)]
struct State {
    fragments: VecDeque<String>,
    final_result: Option<Result<String, String>>,
    finished: bool,
    closed: bool,
}

struct Shared {
    state: Mutex<State>,
    space: Condvar,
    ready: AtomicWaker,
    token: AiCancellationToken,
}

pub(crate) struct Producer {
    shared: Arc<Shared>,
    admitted_bytes: usize,
    overflowed: bool,
}

pub(crate) struct Receiver(Arc<Shared>);

pub(crate) fn channel(token: AiCancellationToken) -> (Producer, Receiver) {
    let shared = Arc::new(Shared {
        state: Mutex::new(State::default()),
        space: Condvar::new(),
        ready: AtomicWaker::new(),
        token,
    });
    (
        Producer {
            shared: shared.clone(),
            admitted_bytes: 0,
            overflowed: false,
        },
        Receiver(shared),
    )
}

fn overflow_error() -> String {
    format!("AI reply exceeded the {MAX_AGENT_MODEL_REPLY_BYTES}-byte limit")
}

impl Producer {
    /// Called on the blocking transport worker. Cancellation is observed even
    /// when the UI remains alive but stops polling. False means stop admitting
    /// preview bytes; the provider's cancellation token has also been signaled.
    pub(crate) fn delta(&mut self, fragment: &str) -> bool {
        if self.overflowed || self.shared.token.is_cancelled() {
            return false;
        }
        if fragment.len() > MAX_AGENT_MODEL_REPLY_BYTES - self.admitted_bytes {
            self.overflowed = true;
            self.shared.token.cancel();
            return false;
        }
        let mut rest = fragment;
        while !rest.is_empty() {
            let mut end = rest.len().min(FRAGMENT_BYTES);
            while !rest.is_char_boundary(end) {
                end -= 1;
            }
            let mut state = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
            while state.fragments.len() == FRAGMENT_SLOTS
                && !state.closed
                && !self.shared.token.is_cancelled()
            {
                state = self
                    .shared
                    .space
                    .wait_timeout(state, CANCEL_POLL)
                    .unwrap_or_else(|e| e.into_inner())
                    .0;
            }
            if state.closed || state.finished || self.shared.token.is_cancelled() {
                self.shared.token.cancel();
                return false;
            }
            // Allocate only after admission: a stalled producer has at most the
            // provider-owned borrowed fragment, not another queued copy of it.
            state.fragments.push_back(rest[..end].to_owned());
            self.admitted_bytes += end;
            rest = &rest[end..];
            drop(state);
            self.shared.ready.wake();
        }
        true
    }

    /// Always nonblocking. The reserved final slot follows all admitted deltas.
    /// Bound both success and error payloads before retaining them in the inbox.
    pub(crate) fn finish(self, result: Result<String, String>) {
        let result = if self.overflowed {
            Err(overflow_error())
        } else {
            match result {
                Ok(value) if value.len() > MAX_AGENT_MODEL_REPLY_BYTES => Err(overflow_error()),
                Err(value) if value.len() > ERROR_BYTES => {
                    // Do not retain a large allocation via String::truncate.
                    let mut end = ERROR_BYTES;
                    while !value.is_char_boundary(end) {
                        end -= 1;
                    }
                    Err(value[..end].to_owned())
                }
                Ok(value) if value.capacity() > MAX_AGENT_MODEL_REPLY_BYTES => {
                    Ok(value.as_str().to_owned())
                }
                Err(value) if value.capacity() > ERROR_BYTES => Err(value.as_str().to_owned()),
                result => result,
            }
        };
        let mut state = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
        if !state.closed {
            state.final_result = Some(result);
        }
        state.finished = true;
        drop(state);
        self.shared.ready.wake();
    }
}

impl Drop for Producer {
    fn drop(&mut self) {
        let mut state = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
        if !state.finished {
            if !state.closed {
                state.final_result = Some(Err("AI streaming worker ended without a reply".into()));
            }
            state.finished = true;
        }
        drop(state);
        self.shared.ready.wake();
    }
}

impl Stream for Receiver {
    type Item = Event;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Event>> {
        // Register before observing state, so a producer cannot race a Pending
        // return without arranging another poll.
        self.0.ready.register(cx.waker());
        let mut state = self.0.state.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(fragment) = state.fragments.pop_front() {
            drop(state);
            self.0.space.notify_one();
            Poll::Ready(Some(Event::Delta(fragment)))
        } else if let Some(result) = state.final_result.take() {
            Poll::Ready(Some(Event::Finished(result)))
        } else if state.finished {
            Poll::Ready(None)
        } else {
            Poll::Pending
        }
    }
}

impl Drop for Receiver {
    fn drop(&mut self) {
        let mut state = self.0.state.lock().unwrap_or_else(|e| e.into_inner());
        state.closed = true;
        state.fragments.clear();
        state.final_result = None;
        self.0.token.cancel();
        drop(state);
        self.0.space.notify_all();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use iced::futures::{executor::block_on, StreamExt};
    use std::sync::mpsc;
    use std::thread;

    #[test]
    fn slow_consumer_preserves_utf8_and_final_reply_order() {
        let (mut sender, receiver) = channel(AiCancellationToken::new());
        let expected = "abc界🙂".repeat(9000);
        let input = expected.clone();
        let worker = thread::spawn(move || {
            assert!(sender.delta(&input));
            sender.finish(Ok(input));
        });
        let events: Vec<_> = block_on(receiver.collect());
        let mut actual = String::new();
        for event in &events[..events.len() - 1] {
            let Event::Delta(fragment) = event else {
                panic!("final reply overtook preview")
            };
            assert!(fragment.len() <= FRAGMENT_BYTES);
            actual.push_str(fragment);
        }
        assert_eq!(actual, expected);
        assert_eq!(events.last(), Some(&Event::Finished(Ok(expected))));
        worker.join().unwrap();
    }

    #[test]
    fn final_result_has_reserved_slot_even_when_preview_is_full() {
        let (mut sender, receiver) = channel(AiCancellationToken::new());
        assert!(sender.delta(&"a".repeat(FRAGMENT_SLOTS * FRAGMENT_BYTES)));
        assert_eq!(
            receiver.0.state.lock().unwrap().fragments.len(),
            FRAGMENT_SLOTS
        );
        sender.finish(Ok("complete".into()));
        let events: Vec<_> = block_on(receiver.collect());
        assert_eq!(events.len(), FRAGMENT_SLOTS + 1);
        assert_eq!(events.last(), Some(&Event::Finished(Ok("complete".into()))));
    }

    #[test]
    fn stalled_producer_is_bounded_and_cancellation_releases_it() {
        let token = AiCancellationToken::new();
        let (mut sender, receiver) = channel(token.clone());
        assert!(sender.delta(&"a".repeat(FRAGMENT_SLOTS * FRAGMENT_BYTES)));
        let (done, completed) = mpsc::channel();
        let worker = thread::spawn(move || {
            done.send(sender.delta("pending")).unwrap();
            sender.finish(Err("cancelled".into()));
        });
        assert!(completed.recv_timeout(Duration::from_millis(60)).is_err());
        let state = receiver.0.state.lock().unwrap();
        assert_eq!(state.fragments.len(), FRAGMENT_SLOTS);
        assert_eq!(
            state.fragments.iter().map(String::len).sum::<usize>(),
            32768
        );
        drop(state);
        token.cancel();
        assert!(!completed.recv_timeout(Duration::from_secs(2)).unwrap());
        worker.join().unwrap();
        let events: Vec<_> = block_on(receiver.collect());
        assert_eq!(
            events.last(),
            Some(&Event::Finished(Err("cancelled".into())))
        );
    }

    #[test]
    fn dropping_receiver_cancels_blocked_worker_and_releases_backlog() {
        let token = AiCancellationToken::new();
        let (mut sender, receiver) = channel(token.clone());
        assert!(sender.delta(&"a".repeat(FRAGMENT_SLOTS * FRAGMENT_BYTES)));
        let (done, completed) = mpsc::channel();
        let worker = thread::spawn(move || done.send(sender.delta("pending")).unwrap());
        drop(receiver);
        assert!(!completed.recv_timeout(Duration::from_secs(2)).unwrap());
        assert!(token.is_cancelled());
        worker.join().unwrap();
    }

    #[test]
    fn oversized_preview_cancels_without_queueing_and_keeps_explicit_final_error() {
        let token = AiCancellationToken::new();
        let (mut sender, receiver) = channel(token.clone());
        assert!(!sender.delta(&"x".repeat(MAX_AGENT_MODEL_REPLY_BYTES + 1)));
        assert!(token.is_cancelled());
        assert!(!sender.delta("later"));
        sender.finish(Err("transport cancelled".into()));
        assert_eq!(
            block_on(receiver.collect::<Vec<_>>()),
            vec![Event::Finished(Err(overflow_error()))]
        );
    }

    #[test]
    fn cumulative_preview_budget_is_not_reset_by_drain() {
        let token = AiCancellationToken::new();
        let (mut sender, mut receiver) = channel(token.clone());
        for _ in 0..MAX_AGENT_MODEL_REPLY_BYTES / FRAGMENT_BYTES {
            assert!(sender.delta(&"x".repeat(FRAGMENT_BYTES)));
            assert!(matches!(block_on(receiver.next()), Some(Event::Delta(_))));
        }
        assert!(!sender.delta("x"));
        sender.finish(Ok("ignored".into()));
        assert_eq!(
            block_on(receiver.next()),
            Some(Event::Finished(Err(overflow_error())))
        );
        assert_eq!(block_on(receiver.next()), None);
    }

    #[test]
    fn huge_final_success_and_error_are_bounded() {
        let (sender, receiver) = channel(AiCancellationToken::new());
        sender.finish(Ok("x".repeat(MAX_AGENT_MODEL_REPLY_BYTES + 1)));
        assert_eq!(
            block_on(receiver.collect::<Vec<_>>()),
            vec![Event::Finished(Err(overflow_error()))]
        );
        let (sender, receiver) = channel(AiCancellationToken::new());
        sender.finish(Err("界".repeat(ERROR_BYTES)));
        let events: Vec<_> = block_on(receiver.collect());
        let Event::Finished(Err(error)) = &events[0] else {
            panic!("wrong result")
        };
        assert!(error.len() <= ERROR_BYTES);
        assert!(error.capacity() <= ERROR_BYTES);
    }

    #[test]
    fn final_payload_does_not_retain_excess_reserved_capacity() {
        let (sender, receiver) = channel(AiCancellationToken::new());
        let mut value = String::with_capacity(2 * MAX_AGENT_MODEL_REPLY_BYTES);
        value.push_str("ok");
        sender.finish(Ok(value));
        let events: Vec<_> = block_on(receiver.collect());
        let Event::Finished(Ok(value)) = &events[0] else {
            panic!("wrong result")
        };
        assert_eq!(value, "ok");
        assert!(value.capacity() <= MAX_AGENT_MODEL_REPLY_BYTES);
    }

    #[test]
    fn abandoned_producer_delivers_failure_not_permanent_loading() {
        let (mut sender, receiver) = channel(AiCancellationToken::new());
        assert!(sender.delta("partial"));
        drop(sender);
        let events: Vec<_> = block_on(receiver.collect());
        assert_eq!(events[0], Event::Delta("partial".into()));
        assert!(matches!(&events[1], Event::Finished(Err(_))));
    }
}
