//! Applies cache invalidation messages from the bus to one node's caches.

use std::sync::Arc;

use futures::StreamExt;
use futures::stream::BoxStream;
use rustryx_core::ports::InvalidationEvent;
use tokio::task::{JoinError, JoinHandle};
use tokio_util::sync::CancellationToken;

use crate::selector::SelectorEngine;
use crate::state::State;

/// Handle of the invalidation task started by
/// [`Rbac::spawn_invalidation_listener`](crate::Rbac::spawn_invalidation_listener).
///
/// Dropping it detaches the task, which then still stops on cancellation.
/// Store it and await [`join`](Self::join) during shutdown so that the task is
/// known to have finished.
#[derive(Debug)]
pub struct InvalidationListener {
    handle: JoinHandle<()>,
}

impl InvalidationListener {
    /// True once the task has ended.
    #[must_use]
    pub fn is_finished(&self) -> bool {
        self.handle.is_finished()
    }

    /// Waits for the task to end. Cancel safe: dropping the future only stops
    /// waiting, the task keeps running until its token fires.
    pub async fn join(self) -> Result<(), JoinError> {
        self.handle.await
    }
}

/// Spawns the listener. `events` was subscribed by the caller before this call
/// so no message published afterwards is lost.
pub(crate) fn spawn_listener<E: SelectorEngine>(
    state: Arc<State<E>>,
    mut events: BoxStream<'static, InvalidationEvent>,
    cancel: CancellationToken,
) -> InvalidationListener {
    let handle = tokio::spawn(async move {
        loop {
            tokio::select! {
                biased;
                () = cancel.cancelled() => break,
                event = events.next() => {
                    let Some(event) = event else {
                        // The bus is gone, so nothing will announce changes any
                        // more. Drop everything now; from here on a node serves
                        // changes only as fast as the TTL (at most five minutes).
                        tracing::warn!("rbac invalidation stream ended, caches fall back to their TTL");
                        state.apply(&InvalidationEvent::Reset);
                        break;
                    };
                    state.apply(&event);
                },
            }
        }
    });
    InvalidationListener { handle }
}
