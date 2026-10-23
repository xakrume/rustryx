//! Bounded, expiring caches for authorization data.
//!
//! Two clocks bound the life of an entry:
//!
//! - `moka`'s `time_to_live` uses the real monotonic clock and is the hard
//!   limit that production relies on.
//! - Each entry also carries a `tokio::time::Instant` stamp that is checked on
//!   every read. It is the same duration, but it follows tokio's mockable
//!   clock, which is what lets the tests prove the TTL with paused time
//!   (moka itself cannot be driven by a fake clock from outside the crate).
//!
//! Whichever expires first wins, so a stale entry is never served after the
//! TTL. Neither clock is the wall clock: a time step by NTP cannot extend the
//! life of an entry.
//!
//! A failed load is returned to every waiter and nothing is cached, so a store
//! outage cannot keep an old answer alive past its TTL.

use std::convert::Infallible;
use std::future::Future;
use std::hash::Hash;
use std::sync::Arc;
use std::time::Duration;

use moka::future::Cache;
use tokio::time::Instant;

/// Upper bound for the lifetime of any cached authorization answer.
pub const MAX_DECISION_TTL: Duration = Duration::from_secs(300);

#[derive(Clone)]
struct Stamped<V> {
    stored_at: Instant,
    value: V,
}

/// A bounded cache with a time to live and single flight loading.
pub(crate) struct TtlCache<K, V> {
    inner: Cache<K, Stamped<V>>,
    ttl: Duration,
}

impl<K, V> TtlCache<K, V>
where
    K: Hash + Eq + Clone + Send + Sync + 'static,
    V: Clone + Send + Sync + 'static,
{
    /// Creates a cache of at most `max_capacity` entries living at most `ttl`
    /// (clamped to [`MAX_DECISION_TTL`]). `with_predicates` enables
    /// [`TtlCache::invalidate_where`].
    pub(crate) fn new(max_capacity: u64, ttl: Duration, with_predicates: bool) -> Self {
        let ttl = ttl.min(MAX_DECISION_TTL);
        let mut builder = Cache::builder()
            .max_capacity(max_capacity)
            .time_to_live(ttl);
        if with_predicates {
            builder = builder.support_invalidation_closures();
        }
        Self {
            inner: builder.build(),
            ttl,
        }
    }

    /// The effective time to live.
    #[cfg(test)]
    pub(crate) fn ttl(&self) -> Duration {
        self.ttl
    }

    /// Returns the fresh cached value or loads it. Concurrent callers for one
    /// key share a single load; if it fails, all of them get the error.
    pub(crate) async fn get_or_load<E, Fut>(
        &self,
        key: K,
        load: impl FnOnce() -> Fut,
    ) -> Result<V, Arc<E>>
    where
        E: Send + Sync + 'static,
        Fut: Future<Output = Result<V, E>>,
    {
        if let Some(entry) = self.inner.get(&key).await {
            if entry.stored_at.elapsed() < self.ttl {
                return Ok(entry.value);
            }
            self.inner.invalidate(&key).await;
        }
        let entry = self
            .inner
            .try_get_with(key, async {
                load().await.map(|value| Stamped {
                    stored_at: Instant::now(),
                    value,
                })
            })
            .await?;
        Ok(entry.value)
    }

    /// [`TtlCache::get_or_load`] for a loader that cannot fail.
    pub(crate) async fn get_or_compute<Fut>(&self, key: K, compute: impl FnOnce() -> Fut) -> V
    where
        Fut: Future<Output = V>,
    {
        match self
            .get_or_load::<Infallible, _>(key, || async { Ok(compute().await) })
            .await
        {
            Ok(value) => value,
            Err(never) => match *never {},
        }
    }

    /// Drops every entry. Entries become unreachable immediately.
    pub(crate) fn invalidate_all(&self) {
        self.inner.invalidate_all();
    }

    /// Drops the entries whose key matches. Only valid when the cache was
    /// created with predicate support.
    pub(crate) fn invalidate_where(&self, predicate: impl Fn(&K) -> bool + Send + Sync + 'static) {
        // `get` never returns an entry that matched a registered predicate, so
        // the invalidation is effective at once; moka removes the entry later.
        // The only failure is a cache built without predicate support, which is
        // a programming error: fall back to dropping everything, the safe side.
        if self
            .inner
            .invalidate_entries_if(move |key, _| predicate(key))
            .is_err()
        {
            self.inner.invalidate_all();
        }
    }

    /// Number of live entries after pending maintenance, for tests.
    #[cfg(test)]
    pub(crate) async fn len(&self) -> u64 {
        self.inner.run_pending_tasks().await;
        self.inner.entry_count()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU32, Ordering};

    use super::*;

    fn cache(ttl_secs: u64) -> TtlCache<u32, String> {
        TtlCache::new(100, Duration::from_secs(ttl_secs), true)
    }

    async fn load(cache: &TtlCache<u32, String>, key: u32, calls: &AtomicU32) -> String {
        cache
            .get_or_load::<String, _>(key, || async {
                calls.fetch_add(1, Ordering::SeqCst);
                Ok(format!("v{}", calls.load(Ordering::SeqCst)))
            })
            .await
            .unwrap()
    }

    #[tokio::test(start_paused = true)]
    async fn serves_from_cache_until_the_ttl_then_reloads() {
        let c = cache(60);
        let calls = AtomicU32::new(0);
        assert_eq!(load(&c, 1, &calls).await, "v1");
        assert_eq!(load(&c, 1, &calls).await, "v1");
        tokio::time::advance(Duration::from_secs(59)).await;
        assert_eq!(load(&c, 1, &calls).await, "v1");
        tokio::time::advance(Duration::from_secs(2)).await;
        assert_eq!(load(&c, 1, &calls).await, "v2");
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn ttl_is_clamped_to_five_minutes() {
        let c: TtlCache<u32, u32> = TtlCache::new(10, Duration::from_hours(24), false);
        assert_eq!(c.ttl(), MAX_DECISION_TTL);
    }

    #[tokio::test(start_paused = true)]
    async fn zero_ttl_never_serves_a_cached_value() {
        let c = cache(0);
        let calls = AtomicU32::new(0);
        assert_eq!(load(&c, 1, &calls).await, "v1");
        assert_eq!(load(&c, 1, &calls).await, "v2");
    }

    #[tokio::test]
    async fn errors_are_returned_and_not_cached() {
        let c = cache(60);
        let first = c
            .get_or_load::<String, _>(1, || async { Err("down".to_owned()) })
            .await;
        assert_eq!(*first.unwrap_err(), "down");
        assert_eq!(c.len().await, 0);
        let calls = AtomicU32::new(0);
        assert_eq!(load(&c, 1, &calls).await, "v1");
    }

    #[tokio::test]
    async fn invalidate_all_and_predicate_drop_entries() {
        let c = cache(60);
        let calls = AtomicU32::new(0);
        for key in 1..=3 {
            load(&c, key, &calls).await;
        }
        c.invalidate_where(|k| *k == 2);
        assert_eq!(load(&c, 1, &calls).await, "v1");
        assert_eq!(load(&c, 2, &calls).await, "v4");
        c.invalidate_all();
        assert_eq!(load(&c, 1, &calls).await, "v5");
    }

    #[tokio::test]
    async fn predicate_on_a_cache_without_support_drops_everything() {
        let c: TtlCache<u32, String> = TtlCache::new(10, Duration::from_secs(60), false);
        let calls = AtomicU32::new(0);
        load(&c, 1, &calls).await;
        c.invalidate_where(|_| false);
        assert_eq!(load(&c, 1, &calls).await, "v2");
    }

    #[tokio::test]
    async fn concurrent_loads_of_one_key_share_one_call() {
        let c = Arc::new(cache(60));
        let calls = Arc::new(AtomicU32::new(0));
        let mut tasks = Vec::new();
        for _ in 0..8 {
            let (c, calls) = (c.clone(), calls.clone());
            tasks.push(tokio::spawn(async move {
                c.get_or_load::<String, _>(7, || async {
                    tokio::time::sleep(Duration::from_millis(20)).await;
                    calls.fetch_add(1, Ordering::SeqCst);
                    Ok("shared".to_owned())
                })
                .await
                .unwrap()
            }));
        }
        for t in tasks {
            assert_eq!(t.await.unwrap(), "shared");
        }
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn capacity_is_bounded() {
        let c: TtlCache<u32, u32> = TtlCache::new(10, Duration::from_secs(60), false);
        for key in 0..200 {
            c.get_or_load::<String, _>(key, || async move { Ok(key) })
                .await
                .unwrap();
        }
        assert!(c.len().await <= 10);
    }
}
