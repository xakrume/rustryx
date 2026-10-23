//! In-process invalidation bus.

use async_trait::async_trait;
use futures::StreamExt;
use futures::stream::BoxStream;
use rustryx_core::error::StoreError;
use rustryx_core::ports::{InvalidationBus, InvalidationEvent};
use tokio::sync::broadcast;
use tokio::sync::broadcast::error::RecvError;

/// Events buffered per subscriber before the slowest one starts lagging.
const DEFAULT_CAPACITY: usize = 256;

/// [`InvalidationBus`] over `tokio::sync::broadcast`.
///
/// A subscriber that falls more than `capacity` events behind gets
/// [`InvalidationEvent::Reset`] in place of the events it missed. That is the same
/// recovery signal the PostgreSQL `LISTEN` bus emits after a reconnect, so consumers
/// exercise the "drop everything" path against this implementation as well.
#[derive(Debug, Clone)]
pub struct MemInvalidationBus {
    sender: broadcast::Sender<InvalidationEvent>,
}

impl MemInvalidationBus {
    /// Creates a bus with the default per-subscriber buffer.
    #[must_use]
    pub fn new() -> Self {
        Self::with_capacity(DEFAULT_CAPACITY)
    }

    /// Creates a bus whose subscribers lag after `capacity` unread events. Use a small
    /// value to provoke `Reset` in a test. Zero is raised to one because the underlying
    /// channel rejects an empty buffer.
    #[must_use]
    pub fn with_capacity(capacity: usize) -> Self {
        let (sender, _) = broadcast::channel(capacity.max(1));
        Self { sender }
    }

    /// Synchronous publish, for callers that cannot await (the in-memory stores).
    /// Having no subscriber is not an error: nobody needs to invalidate anything.
    pub fn emit(&self, event: InvalidationEvent) {
        // `send` only fails when there is no receiver, which is the case described above.
        let _ = self.sender.send(event);
    }
}

impl Default for MemInvalidationBus {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl InvalidationBus for MemInvalidationBus {
    async fn publish(&self, event: InvalidationEvent) -> Result<(), StoreError> {
        self.emit(event);
        Ok(())
    }

    fn subscribe(&self) -> BoxStream<'static, InvalidationEvent> {
        // The receiver is created here, not on first poll: an event published between
        // `subscribe()` and the first poll must still be delivered.
        let receiver = self.sender.subscribe();
        futures::stream::unfold(receiver, |mut receiver| async move {
            match receiver.recv().await {
                Ok(event) => Some((event, receiver)),
                Err(RecvError::Lagged(_)) => Some((InvalidationEvent::Reset, receiver)),
                Err(RecvError::Closed) => None,
            }
        })
        .boxed()
    }
}

#[cfg(test)]
mod tests {
    use rustryx_core::ids::UserId;

    use super::*;

    #[tokio::test]
    async fn delivers_events_to_every_subscriber_in_order() {
        let bus = MemInvalidationBus::new();
        let mut first = bus.subscribe();
        let mut second = bus.subscribe();
        let user = UserId::generate().as_uuid();
        bus.publish(InvalidationEvent::Rbac).await.unwrap();
        bus.publish(InvalidationEvent::User(user)).await.unwrap();
        for stream in [&mut first, &mut second] {
            assert_eq!(stream.next().await, Some(InvalidationEvent::Rbac));
            assert_eq!(stream.next().await, Some(InvalidationEvent::User(user)));
        }
    }

    #[tokio::test]
    async fn event_published_before_first_poll_is_not_lost() {
        let bus = MemInvalidationBus::new();
        let mut stream = bus.subscribe();
        bus.emit(InvalidationEvent::Rbac);
        assert_eq!(stream.next().await, Some(InvalidationEvent::Rbac));
    }

    #[tokio::test]
    async fn publishing_without_subscribers_succeeds() {
        let bus = MemInvalidationBus::default();
        bus.publish(InvalidationEvent::Reset).await.unwrap();
    }

    #[tokio::test]
    async fn lagging_subscriber_receives_reset_then_the_retained_tail() {
        let bus = MemInvalidationBus::with_capacity(2);
        let mut stream = bus.subscribe();
        let users: Vec<_> = (0..5).map(|_| UserId::generate().as_uuid()).collect();
        for user in &users {
            bus.emit(InvalidationEvent::User(*user));
        }
        assert_eq!(stream.next().await, Some(InvalidationEvent::Reset));
        assert_eq!(stream.next().await, Some(InvalidationEvent::User(users[3])));
        assert_eq!(stream.next().await, Some(InvalidationEvent::User(users[4])));
    }

    #[tokio::test]
    async fn stream_ends_when_the_bus_is_dropped() {
        let bus = MemInvalidationBus::with_capacity(0);
        let mut stream = bus.subscribe();
        drop(bus);
        assert_eq!(stream.next().await, None);
    }
}
