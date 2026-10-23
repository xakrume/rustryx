//! Negative cache: remembers "the upstream said 404" for a configured time so a
//! missing artifact does not turn every request into an upstream call.
//!
//! Bounded (max capacity, entries evicted by `TinyLFU`) and per-entry TTL taken from
//! the repository's `negative_cache_ttl_secs`. A TTL of zero disables remembering.

use std::time::Duration;

use moka::Expiry;
use moka::future::Cache;
use rustryx_core::ids::RepositoryId;

type Key = (RepositoryId, String);

struct PerEntryTtl;

impl Expiry<Key, Duration> for PerEntryTtl {
    fn expire_after_create(
        &self,
        _key: &Key,
        ttl: &Duration,
        _created_at: std::time::Instant,
    ) -> Option<Duration> {
        Some(*ttl)
    }
}

/// Bounded cache of upstream 404 answers.
#[derive(Clone)]
pub struct NegativeCache {
    cache: Cache<Key, Duration>,
}

impl NegativeCache {
    /// Creates a cache holding at most `capacity` entries.
    #[must_use]
    pub fn new(capacity: u64) -> Self {
        Self {
            cache: Cache::builder()
                .max_capacity(capacity)
                .expire_after(PerEntryTtl)
                .build(),
        }
    }

    /// True when a fresh "not found" for this path is remembered.
    pub async fn contains(&self, repository: RepositoryId, path: &str) -> bool {
        self.cache
            .get(&(repository, path.to_owned()))
            .await
            .is_some()
    }

    /// Remembers a 404 for `ttl`. A zero TTL remembers nothing.
    pub async fn insert(&self, repository: RepositoryId, path: &str, ttl: Duration) {
        if ttl.is_zero() {
            return;
        }
        self.cache.insert((repository, path.to_owned()), ttl).await;
    }

    /// Forgets a path, for example after it was cached successfully.
    pub async fn remove(&self, repository: RepositoryId, path: &str) {
        self.cache.invalidate(&(repository, path.to_owned())).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn remembers_and_forgets() {
        let cache = NegativeCache::new(100);
        let repo = RepositoryId::generate();
        assert!(!cache.contains(repo, "a").await);
        cache.insert(repo, "a", Duration::from_secs(60)).await;
        assert!(cache.contains(repo, "a").await);
        assert!(!cache.contains(repo, "b").await);
        assert!(
            !cache.contains(RepositoryId::generate(), "a").await,
            "keys are per repository"
        );
        cache.remove(repo, "a").await;
        assert!(!cache.contains(repo, "a").await);
    }

    #[tokio::test]
    async fn zero_ttl_disables_remembering() {
        let cache = NegativeCache::new(100);
        let repo = RepositoryId::generate();
        cache.insert(repo, "a", Duration::ZERO).await;
        assert!(!cache.contains(repo, "a").await);
    }

    #[tokio::test]
    async fn entries_expire_after_their_ttl() {
        let cache = NegativeCache::new(100);
        let repo = RepositoryId::generate();
        cache
            .insert(repo, "short", Duration::from_millis(150))
            .await;
        cache.insert(repo, "long", Duration::from_secs(60)).await;
        tokio::time::sleep(Duration::from_millis(400)).await;
        assert!(!cache.contains(repo, "short").await);
        assert!(cache.contains(repo, "long").await);
    }

    #[tokio::test]
    async fn must_stay_bounded() {
        let cache = NegativeCache::new(10);
        let repo = RepositoryId::generate();
        for i in 0..500 {
            cache
                .insert(repo, &format!("p{i}"), Duration::from_secs(60))
                .await;
        }
        cache.cache.run_pending_tasks().await;
        assert!(
            cache.cache.entry_count() <= 10,
            "count {}",
            cache.cache.entry_count()
        );
    }
}
