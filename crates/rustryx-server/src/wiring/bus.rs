//! One PostgreSQL `LISTEN` connection for every in-process subscriber.
//!
//! `PgInvalidationBus::subscribe` opens its own direct connection on the first poll, and this
//! node has three consumers of the bus (the RBAC caches, the authentication caches and the blob
//! store registry). Three listening sessions per node would be three connections that a
//! connection-limited PostgreSQL has to spare for nothing, so one pump task reads the real bus
//! and fans the events out to whoever subscribed here.
//!
//! A subscriber that cannot keep up does not miss events silently: the broadcast channel tells
//! it how many it lost, and that is delivered as [`InvalidationEvent::Reset`], the one event
//! every cache treats as "drop everything" (the contract of the port).

use std::sync::Arc;

use async_trait::async_trait;
use futures::StreamExt;
use futures::stream::BoxStream;
use rustryx_core::StoreError;
use rustryx_core::ports::{InvalidationBus, InvalidationEvent};
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;

use crate::supervisor::BackgroundTask;

/// Events buffered per subscriber before it is told to reset. Events are tiny and consumers
/// only drop cache entries, so this is generous.
const CHANNEL_CAPACITY: usize = 1024;

/// An [`InvalidationBus`] that shares one upstream subscription.
#[derive(Clone)]
pub struct SharedBus {
    inner: Arc<dyn InvalidationBus>,
    fanout: broadcast::Sender<InvalidationEvent>,
}

impl SharedBus {
    /// Wraps `inner`. Nothing is read until the pump of [`SharedBus::pump_task`] runs.
    #[must_use]
    pub fn new(inner: Arc<dyn InvalidationBus>) -> Self {
        let (fanout, _unused) = broadcast::channel(CHANNEL_CAPACITY);
        Self { inner, fanout }
    }

    /// The task that reads the upstream bus and republishes every event to the subscribers.
    ///
    /// It returns only when `cancel` fires or the upstream stream ends; the supervisor restarts
    /// it in the second case, and the subscribers are told to reset because events may have been
    /// missed in between.
    #[must_use]
    pub fn pump_task(&self) -> BackgroundTask {
        let bus = self.clone();
        BackgroundTask::new("invalidation_bus", move |cancel| {
            let bus = bus.clone();
            async move { bus.pump(&cancel).await }
        })
    }

    async fn pump(&self, cancel: &CancellationToken) {
        let mut events = self.inner.subscribe();
        loop {
            tokio::select! {
                () = cancel.cancelled() => return,
                event = events.next() => {
                    // A send error only means nobody is subscribed right now. An ended upstream
                    // may have missed events on the way out, so it ends with a reset.
                    let ended = event.is_none();
                    let _ = self.fanout.send(event.unwrap_or(InvalidationEvent::Reset));
                    if ended {
                        return;
                    }
                }
            }
        }
    }
}

impl std::fmt::Debug for SharedBus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SharedBus")
            .field("subscribers", &self.fanout.receiver_count())
            .finish_non_exhaustive()
    }
}

#[async_trait]
impl InvalidationBus for SharedBus {
    async fn publish(&self, event: InvalidationEvent) -> Result<(), StoreError> {
        self.inner.publish(event).await
    }

    fn subscribe(&self) -> BoxStream<'static, InvalidationEvent> {
        let receiver = self.fanout.subscribe();
        futures::stream::unfold(receiver, |mut receiver| async move {
            match receiver.recv().await {
                Ok(event) => Some((event, receiver)),
                // Events were dropped for this subscriber: the safe reading is "reset".
                Err(broadcast::error::RecvError::Lagged(_)) => {
                    Some((InvalidationEvent::Reset, receiver))
                }
                Err(broadcast::error::RecvError::Closed) => None,
            }
        })
        .boxed()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;
    use std::time::Duration;

    use super::*;

    /// A bus whose upstream stream is fed by the test.
    struct Scripted {
        feed: Mutex<Option<tokio::sync::mpsc::UnboundedReceiver<InvalidationEvent>>>,
        published: Mutex<Vec<InvalidationEvent>>,
    }

    #[async_trait]
    impl InvalidationBus for Scripted {
        async fn publish(&self, event: InvalidationEvent) -> Result<(), StoreError> {
            self.published.lock().unwrap().push(event);
            Ok(())
        }

        fn subscribe(&self) -> BoxStream<'static, InvalidationEvent> {
            let receiver = self.feed.lock().unwrap().take().expect("subscribed once");
            futures::stream::unfold(receiver, |mut r| async move {
                r.recv().await.map(|event| (event, r))
            })
            .boxed()
        }
    }

    fn scripted() -> (
        Arc<Scripted>,
        tokio::sync::mpsc::UnboundedSender<InvalidationEvent>,
    ) {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let bus = Arc::new(Scripted {
            feed: Mutex::new(Some(rx)),
            published: Mutex::new(Vec::new()),
        });
        (bus, tx)
    }

    async fn next(stream: &mut BoxStream<'static, InvalidationEvent>) -> InvalidationEvent {
        tokio::time::timeout(Duration::from_secs(5), stream.next())
            .await
            .expect("an event arrives")
            .expect("stream open")
    }

    #[tokio::test]
    async fn every_subscriber_sees_every_event_over_one_upstream_subscription() {
        let (upstream, feed) = scripted();
        let shared = SharedBus::new(upstream);
        let mut first = shared.subscribe();
        let mut second = shared.subscribe();
        let cancel = CancellationToken::new();
        let pump = tokio::spawn({
            let shared = shared.clone();
            let cancel = cancel.clone();
            async move { shared.pump(&cancel).await }
        });
        feed.send(InvalidationEvent::Rbac).unwrap();
        feed.send(InvalidationEvent::Projects).unwrap();
        assert_eq!(next(&mut first).await, InvalidationEvent::Rbac);
        assert_eq!(next(&mut first).await, InvalidationEvent::Projects);
        assert_eq!(next(&mut second).await, InvalidationEvent::Rbac);
        assert_eq!(next(&mut second).await, InvalidationEvent::Projects);
        cancel.cancel();
        pump.await.unwrap();
    }

    #[tokio::test]
    async fn must_not_lose_events_silently_when_a_subscriber_falls_behind() {
        let (upstream, feed) = scripted();
        let shared = SharedBus::new(upstream);
        let mut slow = shared.subscribe();
        let cancel = CancellationToken::new();
        let pump = tokio::spawn({
            let shared = shared.clone();
            let cancel = cancel.clone();
            async move { shared.pump(&cancel).await }
        });
        // Far more than the channel holds, while the subscriber reads nothing.
        for _ in 0..(CHANNEL_CAPACITY * 2) {
            feed.send(InvalidationEvent::Rbac).unwrap();
        }
        // The pump runs while the test sleeps and finds every event ready.
        tokio::time::sleep(Duration::from_millis(300)).await;
        let mut saw_reset = false;
        for _ in 0..(CHANNEL_CAPACITY * 3) {
            if next(&mut slow).await == InvalidationEvent::Reset {
                saw_reset = true;
                break;
            }
        }
        assert!(
            saw_reset,
            "the lag is reported as a reset, never as nothing"
        );
        cancel.cancel();
        pump.await.unwrap();
    }

    #[tokio::test]
    async fn publishing_goes_straight_to_the_upstream_bus() {
        let (upstream, _feed) = scripted();
        let shared = SharedBus::new(upstream.clone());
        shared.publish(InvalidationEvent::Rbac).await.unwrap();
        assert_eq!(
            upstream.published.lock().unwrap().as_slice(),
            [InvalidationEvent::Rbac]
        );
    }

    #[tokio::test]
    async fn a_pump_whose_upstream_ends_tells_subscribers_to_reset() {
        let (upstream, feed) = scripted();
        let shared = SharedBus::new(upstream);
        let mut events = shared.subscribe();
        let cancel = CancellationToken::new();
        let pump = tokio::spawn({
            let shared = shared.clone();
            async move { shared.pump(&cancel).await }
        });
        drop(feed);
        assert_eq!(next(&mut events).await, InvalidationEvent::Reset);
        pump.await.unwrap();
    }
}
