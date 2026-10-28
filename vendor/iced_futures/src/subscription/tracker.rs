use crate::subscription::{Event, Hasher, Recipe};
use crate::{BoxFuture, MaybeSend};

use futures::channel::mpsc;
use futures::sink::{Sink, SinkExt};
use rustc_hash::FxHashMap;

use std::hash::Hasher as _;

/// A registry of subscription streams.
///
/// If you have an application that continuously returns a [`Subscription`],
/// you can use a [`Tracker`] to keep track of the different recipes and keep
/// its executions alive.
///
/// [`Subscription`]: crate::Subscription
#[derive(Debug, Default)]
pub struct Tracker {
    subscriptions: FxHashMap<u64, Execution>,
}

#[derive(Debug)]
pub struct Execution {
    _cancel: futures::channel::oneshot::Sender<()>,
    listener: Option<futures::channel::mpsc::UnboundedSender<Event>>,
}

impl Tracker {
    /// Creates a new empty [`Tracker`].
    pub fn new() -> Self {
        Self {
            subscriptions: FxHashMap::default(),
        }
    }

    /// Updates the [`Tracker`] with the given [`Subscription`].
    ///
    /// A [`Subscription`] can cause new streams to be spawned or old streams
    /// to be closed.
    ///
    /// The [`Tracker`] keeps track of these streams between calls to this
    /// method:
    ///
    /// - If the provided [`Subscription`] contains a new [`Recipe`] that is
    ///   currently not being run, it will spawn a new stream and keep it alive.
    /// - On the other hand, if a [`Recipe`] is currently in execution and the
    ///   provided [`Subscription`] does not contain it anymore, then the
    ///   [`Tracker`] will close and drop the relevant stream.
    ///
    /// It returns a list of futures that need to be spawned to materialize
    /// the [`Tracker`] changes.
    ///
    /// [`Recipe`]: crate::subscription::Recipe
    /// [`Subscription`]: crate::Subscription
    pub fn update<Message, Receiver>(
        &mut self,
        recipes: impl Iterator<Item = Box<dyn Recipe<Output = Message>>>,
        receiver: Receiver,
    ) -> Vec<BoxFuture<()>>
    where
        Message: 'static + MaybeSend,
        Receiver: 'static + Sink<Message, Error = mpsc::SendError> + Unpin + MaybeSend + Clone,
    {
        use futures::stream::StreamExt;

        let mut futures: Vec<BoxFuture<()>> = Vec::new();
        let mut alive = std::collections::HashSet::new();

        for recipe in recipes {
            let id = {
                let mut hasher = Hasher::default();
                recipe.hash(&mut hasher);

                hasher.finish()
            };

            let _ = alive.insert(id);

            if self.subscriptions.contains_key(&id) {
                continue;
            }

            let (cancel, mut canceled) = futures::channel::oneshot::channel();

            // These events include keyboard bytes, modifier transitions, IME
            // commits and focus. A full bounded inbox must not silently turn
            // a received command into a different command. Keep the one FIFO
            // without blocking the UI thread; the consumer still applies its
            // original downstream backpressure. Cancellation drops the inbox.
            let (event_sender, event_receiver) = futures::channel::mpsc::unbounded();

            let mut receiver = receiver.clone();
            let mut stream = recipe.stream(event_receiver.boxed());

            let future = async move {
                loop {
                    let select = futures::future::select(&mut canceled, stream.next());

                    match select.await {
                        futures::future::Either::Left(_)
                        | futures::future::Either::Right((None, _)) => break,
                        futures::future::Either::Right((Some(message), _)) => {
                            let send = receiver.send(message);
                            futures::pin_mut!(send);
                            // Cancellation must also wake a stalled output
                            // send, not just a wait for the next input event.
                            // A message already accepted by the sink remains
                            // there; the internal inbox is dropped on exit.
                            match futures::future::select(&mut canceled, send).await {
                                futures::future::Either::Left(_)
                                | futures::future::Either::Right((Err(_), _)) => {
                                    break;
                                }
                                futures::future::Either::Right((Ok(()), _)) => {}
                            }
                        }
                    }
                }
            };

            let _ = self.subscriptions.insert(
                id,
                Execution {
                    _cancel: cancel,
                    listener: if event_sender.is_closed() {
                        None
                    } else {
                        Some(event_sender)
                    },
                },
            );

            futures.push(Box::pin(future));
        }

        self.subscriptions.retain(|id, _| alive.contains(id));

        futures
    }

    /// Broadcasts an event to the subscriptions currently alive.
    ///
    /// A subscription's [`Recipe::stream`] always receives a stream of events
    /// as input. This stream can be used by some subscription to listen to
    /// shell events.
    ///
    /// This method publishes the given event to all the subscription streams
    /// currently open.
    ///
    /// [`Recipe::stream`]: crate::subscription::Recipe::stream
    pub fn broadcast(&mut self, event: Event) {
        for connection in self.subscriptions.values_mut() {
            if let Some(listener) = connection.listener.as_mut() {
                if let Err(error) = listener.unbounded_send(event.clone()) {
                    log::warn!("Error sending event to subscription: {error:?}");
                    // Only disconnection can fail an unbounded send. Release
                    // the closed sender and avoid retaining a dead listener.
                    connection.listener = None;
                }
            }
        }
    }
}

#[cfg(test)]
#[path = "tracker_tests.rs"]
mod tests;
