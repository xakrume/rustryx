//! In-memory distributed lock with a TTL.

use std::collections::HashMap;
use std::fmt;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use rustryx_core::error::StoreError;
use rustryx_core::ports::{Clock, LockBackend, LockGuard};
use time::OffsetDateTime;

use crate::support::{add_std, lock};

/// Where "now" comes from when a lock expires.
#[derive(Clone)]
enum TimeSource {
    /// Tokio's monotonic clock, anchored to a wall-clock reading. It follows
    /// `tokio::time::pause` and `advance`, so TTL tests need no real sleeping.
    Tokio {
        wall_anchor: OffsetDateTime,
        instant_anchor: tokio::time::Instant,
    },
    /// An injected clock, usually a `FixedClock`.
    Clock(Arc<dyn Clock>),
}

impl TimeSource {
    fn tokio() -> Self {
        Self::Tokio {
            wall_anchor: OffsetDateTime::now_utc(),
            instant_anchor: tokio::time::Instant::now(),
        }
    }

    fn now(&self) -> OffsetDateTime {
        match self {
            Self::Tokio {
                wall_anchor,
                instant_anchor,
            } => add_std(*wall_anchor, instant_anchor.elapsed()),
            Self::Clock(clock) => clock.now(),
        }
    }
}

struct Held {
    token: u64,
    expires_at: OffsetDateTime,
}

#[derive(Default)]
struct Table {
    held: HashMap<String, Held>,
    next_token: u64,
}

impl Table {
    /// A lock counts as held only while unexpired: an expired entry is dead weight that
    /// the next `try_acquire` may overwrite, exactly like an expired Redis key.
    fn holds(&self, key: &str, token: u64, now: OffsetDateTime) -> bool {
        self.held
            .get(key)
            .is_some_and(|h| h.token == token && h.expires_at > now)
    }
}

/// [`LockBackend`] that keeps lock state in process memory.
///
/// Every successful acquire gets a unique token, so a guard whose lock expired and was
/// taken over can never release or renew the new owner's lock.
#[derive(Clone)]
pub struct MemLockBackend {
    table: Arc<Mutex<Table>>,
    time: TimeSource,
}

impl MemLockBackend {
    /// Creates a backend whose TTLs follow tokio time (virtual under `start_paused`).
    #[must_use]
    pub fn new() -> Self {
        Self {
            table: Arc::new(Mutex::new(Table::default())),
            time: TimeSource::tokio(),
        }
    }

    /// Creates a backend whose TTLs follow `clock`, so a test can expire locks by
    /// advancing a shared `FixedClock`.
    #[must_use]
    pub fn with_clock(clock: Arc<dyn Clock>) -> Self {
        Self {
            table: Arc::new(Mutex::new(Table::default())),
            time: TimeSource::Clock(clock),
        }
    }

    /// Drops the lock on `key` as if its TTL had run out and nobody renewed it.
    /// Returns whether a lock was present. The old guard then sees `renew() == false`
    /// and `is_lost() == true`, while another caller can acquire the key.
    #[allow(
        clippy::must_use_candidate,
        reason = "tests usually just trigger the expiry; `unused_must_use` is denied workspace wide and would reject them"
    )]
    pub fn force_expire(&self, key: &str) -> bool {
        lock(&self.table).held.remove(key).is_some()
    }

    /// True while `key` is held and unexpired.
    #[must_use]
    pub fn is_held(&self, key: &str) -> bool {
        let now = self.time.now();
        lock(&self.table)
            .held
            .get(key)
            .is_some_and(|h| h.expires_at > now)
    }
}

impl Default for MemLockBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Debug for MemLockBackend {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MemLockBackend").finish_non_exhaustive()
    }
}

#[async_trait]
impl LockBackend for MemLockBackend {
    async fn try_acquire(
        &self,
        key: &str,
        ttl: Duration,
    ) -> Result<Option<Box<dyn LockGuard>>, StoreError> {
        let now = self.time.now();
        let mut table = lock(&self.table);
        if table.held.get(key).is_some_and(|h| h.expires_at > now) {
            return Ok(None);
        }
        table.next_token += 1;
        let token = table.next_token;
        table.held.insert(
            key.to_owned(),
            Held {
                token,
                expires_at: add_std(now, ttl),
            },
        );
        Ok(Some(Box::new(MemLockGuard {
            table: Arc::clone(&self.table),
            time: self.time.clone(),
            key: key.to_owned(),
            token,
            ttl,
            lost: false,
        })))
    }
}

/// Guard returned by [`MemLockBackend`]. Dropping it without `release` leaves the lock
/// to expire by TTL, which is what a crashed holder does in production.
struct MemLockGuard {
    table: Arc<Mutex<Table>>,
    time: TimeSource,
    key: String,
    token: u64,
    ttl: Duration,
    lost: bool,
}

#[async_trait]
impl LockGuard for MemLockGuard {
    async fn renew(&mut self) -> Result<bool, StoreError> {
        let now = self.time.now();
        let mut table = lock(&self.table);
        // An expired lock cannot be renewed even when nobody took it over yet: the
        // holder may already have paused longer than the TTL, so it must stop writing.
        let renewed = match table.held.get_mut(&self.key) {
            Some(held) if held.token == self.token && held.expires_at > now => {
                held.expires_at = add_std(now, self.ttl);
                true
            }
            _ => false,
        };
        if !renewed {
            self.lost = true;
        }
        Ok(renewed)
    }

    fn is_lost(&self) -> bool {
        // Stricter than the port requires (it only promises to report loss seen by a
        // renewal): loss is reported as soon as it is visible in memory, so tests do not
        // have to wait for a heartbeat tick after `force_expire` or a clock jump.
        self.lost || !lock(&self.table).holds(&self.key, self.token, self.time.now())
    }

    async fn release(self: Box<Self>) -> Result<(), StoreError> {
        let mut table = lock(&self.table);
        // Remove only our own entry: after a takeover the key belongs to someone else.
        if table
            .held
            .get(&self.key)
            .is_some_and(|h| h.token == self.token)
        {
            table.held.remove(&self.key);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::FixedClock;

    const TTL: Duration = Duration::from_secs(10);

    fn backend_with_clock() -> (MemLockBackend, FixedClock) {
        let clock = FixedClock::default();
        (MemLockBackend::with_clock(Arc::new(clock.clone())), clock)
    }

    #[tokio::test]
    async fn must_not_grant_a_held_lock_to_a_second_caller() {
        let (backend, _clock) = backend_with_clock();
        let guard = backend.try_acquire("job", TTL).await.unwrap();
        assert!(guard.is_some());
        assert!(backend.try_acquire("job", TTL).await.unwrap().is_none());
        assert!(backend.is_held("job"));
    }

    #[tokio::test]
    async fn different_keys_do_not_block_each_other() {
        let (backend, _clock) = backend_with_clock();
        let _a = backend.try_acquire("a", TTL).await.unwrap().unwrap();
        assert!(backend.try_acquire("b", TTL).await.unwrap().is_some());
    }

    #[tokio::test]
    async fn release_frees_the_key_for_the_next_caller() {
        let (backend, _clock) = backend_with_clock();
        let guard = backend.try_acquire("job", TTL).await.unwrap().unwrap();
        guard.release().await.unwrap();
        assert!(!backend.is_held("job"));
        assert!(backend.try_acquire("job", TTL).await.unwrap().is_some());
    }

    #[tokio::test]
    async fn expiry_lets_another_caller_take_over_and_marks_the_old_guard_lost() {
        let (backend, clock) = backend_with_clock();
        let mut old = backend.try_acquire("job", TTL).await.unwrap().unwrap();
        clock.advance(TTL + Duration::from_secs(1));
        assert!(old.is_lost());
        let mut new = backend.try_acquire("job", TTL).await.unwrap().unwrap();
        assert!(!old.renew().await.unwrap());
        assert!(old.is_lost());
        assert!(new.renew().await.unwrap());
    }

    #[tokio::test]
    async fn must_not_let_a_lost_guard_release_the_new_owners_lock() {
        let (backend, clock) = backend_with_clock();
        let old = backend.try_acquire("job", TTL).await.unwrap().unwrap();
        clock.advance(TTL * 2);
        let _new = backend.try_acquire("job", TTL).await.unwrap().unwrap();
        old.release().await.unwrap();
        assert!(backend.is_held("job"));
        assert!(backend.try_acquire("job", TTL).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn renew_extends_the_ttl() {
        let (backend, clock) = backend_with_clock();
        let mut guard = backend.try_acquire("job", TTL).await.unwrap().unwrap();
        clock.advance(Duration::from_secs(6));
        assert!(guard.renew().await.unwrap());
        clock.advance(Duration::from_secs(6));
        assert!(backend.is_held("job"));
        assert!(!guard.is_lost());
    }

    #[tokio::test]
    async fn must_not_renew_an_expired_lock_even_without_a_takeover() {
        let (backend, clock) = backend_with_clock();
        let mut guard = backend.try_acquire("job", TTL).await.unwrap().unwrap();
        clock.advance(TTL);
        assert!(!guard.renew().await.unwrap());
        assert!(guard.is_lost());
    }

    #[tokio::test]
    async fn force_expire_simulates_losing_the_lock() {
        let (backend, _clock) = backend_with_clock();
        let mut guard = backend.try_acquire("job", TTL).await.unwrap().unwrap();
        assert!(!guard.is_lost());
        assert!(backend.force_expire("job"));
        assert!(!backend.force_expire("job"));
        assert!(!guard.renew().await.unwrap());
        assert!(guard.is_lost());
        assert!(backend.try_acquire("job", TTL).await.unwrap().is_some());
    }

    #[tokio::test(start_paused = true)]
    async fn default_backend_follows_tokio_virtual_time() {
        let backend = MemLockBackend::new();
        let mut guard = backend.try_acquire("job", TTL).await.unwrap().unwrap();
        tokio::time::advance(Duration::from_secs(9)).await;
        assert!(guard.renew().await.unwrap());
        tokio::time::advance(Duration::from_secs(9)).await;
        assert!(backend.try_acquire("job", TTL).await.unwrap().is_none());
        tokio::time::advance(Duration::from_secs(2)).await;
        assert!(backend.try_acquire("job", TTL).await.unwrap().is_some());
        assert!(guard.is_lost());
    }

    #[test]
    fn debug_output_is_opaque() {
        let (backend, _clock) = backend_with_clock();
        assert_eq!(format!("{backend:?}"), "MemLockBackend { .. }");
    }
}
