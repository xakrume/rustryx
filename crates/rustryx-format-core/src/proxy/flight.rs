//! In-process singleflight for proxy cache fills.
//!
//! Many requests for the same missing `(repository, path)` must cause exactly one
//! upstream request. The first caller becomes the *leader* and owns a
//! [`LeaderGuard`]; everyone else becomes a [`Follower`] and waits for the leader's
//! result.
//!
//! Invariants, and what holds them:
//!
//! * **The key never outlives its leader.** [`LeaderGuard`] removes the key in
//!   `Drop`, so success, error, cancellation and panic of the leader all clean up.
//!   A leaked key would make every later request for that path wait forever.
//! * **No guard is held across an await.** The map entry guard lives inside
//!   [`Flights::join`] only; waiting happens on a `watch` receiver that holds no
//!   lock while pending.
//! * **A follower can tell success from abandonment.** If the leader goes away
//!   without a result the `watch` sender is dropped, and the follower gets
//!   [`WaitError::Aborted`] instead of hanging, so it can retry as a new leader.
//!
//! The fill itself runs in a task of its own (not in the leader's request future),
//! so a client that disconnects is merely one consumer less: the guard travels with
//! the task, not with the request.
//!
//! Between nodes there is no deduplication; correctness never depends on it.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use dashmap::DashMap;
use dashmap::mapref::entry::Entry;
use rustryx_core::ids::RepositoryId;
use tokio::sync::watch;

use crate::metrics;

/// Identity of a cache fill.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct FlightKey {
    /// Repository being filled.
    pub repository: RepositoryId,
    /// Cache path (normalized).
    pub path: String,
}

struct FlightEntry<T> {
    id: u64,
    template: watch::Receiver<Option<T>>,
}

/// Registry of running fills.
pub struct Flights<T> {
    map: Arc<DashMap<FlightKey, FlightEntry<T>>>,
    next_id: Arc<AtomicU64>,
}

impl<T> Clone for Flights<T> {
    fn clone(&self) -> Self {
        Self {
            map: Arc::clone(&self.map),
            next_id: Arc::clone(&self.next_id),
        }
    }
}

impl<T> Default for Flights<T> {
    fn default() -> Self {
        Self {
            map: Arc::new(DashMap::new()),
            next_id: Arc::new(AtomicU64::new(1)),
        }
    }
}

/// Role assigned by [`Flights::join`].
pub enum Join<T> {
    /// This caller must perform the fill and report the result.
    Leader(LeaderGuard<T>),
    /// Someone else is filling; wait for their result.
    Follower(Follower<T>),
}

impl<T: Clone> Flights<T> {
    /// Creates an empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Number of fills in progress.
    #[must_use]
    pub fn in_flight(&self) -> usize {
        self.map.len()
    }

    /// Joins the fill for `key`, creating it when absent.
    ///
    /// Synchronous on purpose: the map shard is locked only for this call.
    #[must_use]
    pub fn join(&self, key: FlightKey) -> Join<T> {
        match self.map.entry(key.clone()) {
            Entry::Occupied(existing) => {
                let rx = existing.get().template.clone();
                metrics::singleflight("follower");
                Join::Follower(Follower { rx })
            }
            Entry::Vacant(slot) => {
                let (tx, rx) = watch::channel(None);
                let id = self.next_id.fetch_add(1, Ordering::Relaxed);
                slot.insert(FlightEntry { id, template: rx });
                metrics::singleflight("leader");
                Join::Leader(LeaderGuard {
                    map: Arc::clone(&self.map),
                    key,
                    id,
                    tx,
                })
            }
        }
    }
}

/// Held by the party that performs the fill. Dropping it (for any reason) removes the key.
pub struct LeaderGuard<T> {
    map: Arc<DashMap<FlightKey, FlightEntry<T>>>,
    key: FlightKey,
    id: u64,
    tx: watch::Sender<Option<T>>,
}

impl<T: Clone> LeaderGuard<T> {
    /// A follower handle for the leader's own flight, so the leader can wait for the
    /// result of the fill it started (the fill runs in a task that owns this guard).
    #[must_use]
    pub fn follower(&self) -> Follower<T> {
        Follower {
            rx: self.tx.subscribe(),
        }
    }
}

impl<T> LeaderGuard<T> {
    /// Publishes the result to all followers. The key is removed when the guard drops.
    pub fn complete(self, value: T) {
        // `send_replace` stores the value even when no follower is listening yet,
        // so a follower that joins right after still sees it.
        self.tx.send_replace(Some(value));
    }
}

impl<T> Drop for LeaderGuard<T> {
    fn drop(&mut self) {
        // `remove_if` with the id: if a newer flight already replaced this key
        // (it cannot today, but the check is what makes the removal safe to reason
        // about), only our own entry is removed.
        self.map
            .remove_if(&self.key, |_, entry| entry.id == self.id);
    }
}

/// Why a [`Follower`] got no result.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WaitError {
    /// The leader vanished without a result (cancelled by shutdown, panicked).
    Aborted,
    /// The safety ceiling passed.
    Timeout,
}

/// Waits for the leader's result.
pub struct Follower<T> {
    rx: watch::Receiver<Option<T>>,
}

impl<T: Clone> Follower<T> {
    /// Waits for the result, at most `max_wait`.
    ///
    /// Cancel safe: dropping the future just stops waiting.
    ///
    /// # Errors
    /// [`WaitError::Aborted`] when the leader disappeared, [`WaitError::Timeout`]
    /// when `max_wait` passed.
    pub async fn wait(mut self, max_wait: Duration) -> Result<T, WaitError> {
        loop {
            // The `Ref` is a temporary of this statement: no read lock survives the await below.
            let current = self.rx.borrow().as_ref().cloned();
            if let Some(value) = current {
                return Ok(value);
            }
            match tokio::time::timeout(max_wait, self.rx.changed()).await {
                Ok(Ok(())) => {}
                Ok(Err(_sender_dropped)) => {
                    // The sender may have published and dropped in one go.
                    return self.rx.borrow().as_ref().cloned().ok_or(WaitError::Aborted);
                }
                Err(_elapsed) => return Err(WaitError::Timeout),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(path: &str) -> FlightKey {
        FlightKey {
            repository: RepositoryId::from_uuid(uuid::Uuid::nil()),
            path: path.to_owned(),
        }
    }

    #[tokio::test]
    async fn first_caller_leads_the_rest_follow_and_all_get_the_result() {
        let flights: Flights<u32> = Flights::new();
        let Join::Leader(leader) = flights.join(key("a")) else {
            panic!("first caller must lead");
        };
        let followers: Vec<_> = (0..5)
            .map(|_| match flights.join(key("a")) {
                Join::Follower(f) => f,
                Join::Leader(_) => panic!("only one leader per key"),
            })
            .collect();
        assert_eq!(flights.in_flight(), 1);
        let waiters: Vec<_> = followers
            .into_iter()
            .map(|f| tokio::spawn(f.wait(Duration::from_secs(5))))
            .collect();
        leader.complete(42);
        for w in waiters {
            assert_eq!(w.await.unwrap(), Ok(42));
        }
        assert_eq!(
            flights.in_flight(),
            0,
            "the key must be gone after completion"
        );
    }

    #[tokio::test]
    async fn must_remove_key_when_leader_is_dropped_without_result() {
        let flights: Flights<u32> = Flights::new();
        let Join::Leader(leader) = flights.join(key("a")) else {
            panic!("leader expected");
        };
        let Join::Follower(follower) = flights.join(key("a")) else {
            panic!("follower expected");
        };
        drop(leader); // cancelled, errored or panicked: all end here
        assert_eq!(flights.in_flight(), 0);
        assert_eq!(
            follower.wait(Duration::from_secs(5)).await,
            Err(WaitError::Aborted)
        );
        assert!(
            matches!(flights.join(key("a")), Join::Leader(_)),
            "next caller becomes a new leader"
        );
    }

    #[tokio::test]
    async fn must_remove_key_when_the_leader_task_is_cancelled() {
        let flights: Flights<u32> = Flights::new();
        let Join::Leader(leader) = flights.join(key("a")) else {
            panic!("leader expected");
        };
        let task = tokio::spawn(async move {
            let _guard = leader;
            futures::future::pending::<()>().await;
        });
        tokio::task::yield_now().await;
        assert_eq!(flights.in_flight(), 1);
        task.abort();
        let _ = task.await;
        assert_eq!(flights.in_flight(), 0);
    }

    #[tokio::test]
    async fn must_remove_key_when_the_leader_panics() {
        let flights: Flights<u32> = Flights::new();
        let Join::Leader(leader) = flights.join(key("a")) else {
            panic!("leader expected");
        };
        let task = tokio::spawn(async move {
            let _guard = leader;
            panic!("fill blew up");
        });
        assert!(task.await.is_err());
        assert_eq!(flights.in_flight(), 0);
    }

    #[tokio::test]
    async fn different_keys_do_not_share_a_flight() {
        let flights: Flights<u32> = Flights::new();
        assert!(matches!(flights.join(key("a")), Join::Leader(_)));
        let second = flights.join(key("b"));
        assert!(matches!(second, Join::Leader(_)));
    }

    #[tokio::test]
    async fn late_joiner_sees_a_result_published_just_before() {
        let flights: Flights<u32> = Flights::new();
        let Join::Leader(leader) = flights.join(key("a")) else {
            panic!("leader expected");
        };
        let Join::Follower(follower) = flights.join(key("a")) else {
            panic!("follower expected");
        };
        leader.complete(7);
        assert_eq!(follower.wait(Duration::from_millis(10)).await, Ok(7));
    }

    #[tokio::test(start_paused = true)]
    async fn follower_times_out_at_the_safety_ceiling() {
        let flights: Flights<u32> = Flights::new();
        let Join::Leader(_leader) = flights.join(key("a")) else {
            panic!("leader expected");
        };
        let Join::Follower(follower) = flights.join(key("a")) else {
            panic!("follower expected");
        };
        assert_eq!(
            follower.wait(Duration::from_secs(60)).await,
            Err(WaitError::Timeout)
        );
    }

    #[tokio::test]
    async fn flights_clone_shares_the_registry() {
        let flights: Flights<u32> = Flights::new();
        let copy = flights.clone();
        let _leader = flights.join(key("a"));
        assert!(matches!(copy.join(key("a")), Join::Follower(_)));
    }
}
