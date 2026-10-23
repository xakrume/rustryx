//! Cache of confirmed group membership.
//!
//! Two clocks are in play and they must not be confused:
//! - `moka` expires entries by real time. That is only a memory bound (entries cannot
//!   outlive the staleness window plus a minute, and the capacity is counted in group
//!   strings, not entries, so a few users with thousands of groups cannot crowd out memory);
//! - every DECISION about whether an entry may be used is made here, by comparing
//!   `confirmed_at` with the injected [`Clock`]. That is what makes the five minute
//!   rule testable and what keeps it correct when the process clock is stepped.
//!
//! The age of an entry whose `confirmed_at` lies in the future (the wall clock was
//! stepped backwards) is unknown, so such an entry is treated as unusable rather than
//! as brand new.

use std::time::Duration;

use moka::future::Cache;
use time::OffsetDateTime;

use super::config::MAX_GROUP_STALENESS;

/// Capacity in weight units: one per group DN plus one per entry. About 500 thousand
/// strings, which is a few tens of megabytes at the worst.
const MAX_WEIGHT: u64 = 500_000;

/// A confirmed set of groups.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CachedGroups {
    /// DNs as the directory returned them.
    pub(crate) groups: Vec<String>,
    /// When the directory last confirmed this set.
    pub(crate) confirmed_at: OffsetDateTime,
}

pub(crate) struct GroupCache {
    entries: Cache<String, CachedGroups>,
    ttl: Duration,
}

/// Age of `confirmed_at` at `now`, or `None` when it lies in the future.
fn age(now: OffsetDateTime, confirmed_at: OffsetDateTime) -> Option<Duration> {
    Duration::try_from(now - confirmed_at).ok()
}

impl GroupCache {
    pub(crate) fn new(ttl: Duration) -> Self {
        let entries = Cache::builder()
            .weigher(|_username: &String, entry: &CachedGroups| {
                u32::try_from(entry.groups.len().saturating_add(1)).unwrap_or(u32::MAX)
            })
            .max_capacity(MAX_WEIGHT)
            .time_to_live(MAX_GROUP_STALENESS + Duration::from_secs(60))
            .build();
        Self { entries, ttl }
    }

    /// An entry young enough to be served without asking the directory again.
    pub(crate) async fn fresh(&self, username: &str, now: OffsetDateTime) -> Option<CachedGroups> {
        let entry = self.entries.get(username).await?;
        (age(now, entry.confirmed_at)? < self.ttl).then_some(entry)
    }

    /// An entry that may still be used when the directory cannot be asked: at most
    /// five minutes since the last successful search.
    pub(crate) async fn within_window(
        &self,
        username: &str,
        now: OffsetDateTime,
    ) -> Option<CachedGroups> {
        let entry = self.entries.get(username).await?;
        (age(now, entry.confirmed_at)? <= MAX_GROUP_STALENESS).then_some(entry)
    }

    pub(crate) async fn put(&self, username: &str, entry: CachedGroups) {
        self.entries.insert(username.to_owned(), entry).await;
    }

    pub(crate) async fn remove(&self, username: &str) {
        self.entries.invalidate(username).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use time::macros::datetime;

    const T0: OffsetDateTime = datetime!(2026-10-24 12:00:00 UTC);

    fn entry(at: OffsetDateTime) -> CachedGroups {
        CachedGroups {
            groups: vec!["cn=a,dc=x".into()],
            confirmed_at: at,
        }
    }

    fn secs(n: i64) -> time::Duration {
        time::Duration::seconds(n)
    }

    #[tokio::test]
    async fn an_entry_is_fresh_only_until_the_ttl() {
        let cache = GroupCache::new(Duration::from_secs(60));
        cache.put("alice", entry(T0)).await;
        assert!(cache.fresh("alice", T0).await.is_some());
        assert!(cache.fresh("alice", T0 + secs(59)).await.is_some());
        assert!(cache.fresh("alice", T0 + secs(60)).await.is_none());
        assert!(cache.fresh("bob", T0).await.is_none());
    }

    #[tokio::test]
    async fn a_zero_ttl_never_serves_from_cache() {
        let cache = GroupCache::new(Duration::ZERO);
        cache.put("alice", entry(T0)).await;
        assert!(cache.fresh("alice", T0).await.is_none());
        assert!(cache.within_window("alice", T0).await.is_some());
    }

    #[tokio::test]
    async fn must_not_use_an_entry_older_than_five_minutes() {
        let cache = GroupCache::new(Duration::from_secs(60));
        cache.put("alice", entry(T0)).await;
        assert!(cache.within_window("alice", T0 + secs(299)).await.is_some());
        assert!(
            cache.within_window("alice", T0 + secs(300)).await.is_some(),
            "at most 5 minutes"
        );
        assert!(cache.within_window("alice", T0 + secs(301)).await.is_none());
        assert!(
            cache
                .within_window("alice", T0 + secs(3600))
                .await
                .is_none()
        );
    }

    #[tokio::test]
    async fn must_not_trust_an_entry_confirmed_in_the_future() {
        let cache = GroupCache::new(Duration::from_secs(60));
        cache.put("alice", entry(T0 + secs(10))).await;
        assert!(cache.fresh("alice", T0).await.is_none());
        assert!(cache.within_window("alice", T0).await.is_none());
    }

    #[tokio::test]
    async fn removal_drops_the_entry() {
        let cache = GroupCache::new(Duration::from_secs(60));
        cache.put("alice", entry(T0)).await;
        cache.remove("alice").await;
        assert!(cache.within_window("alice", T0).await.is_none());
    }

    #[tokio::test]
    async fn the_capacity_is_counted_in_group_strings() {
        let cache = GroupCache::new(Duration::from_secs(60));
        let groups: Vec<String> = (0..10).map(|n| format!("cn=g{n},dc=x")).collect();
        cache
            .put(
                "alice",
                CachedGroups {
                    groups,
                    confirmed_at: T0,
                },
            )
            .await;
        cache.entries.run_pending_tasks().await;
        assert_eq!(
            cache.entries.weighted_size(),
            11,
            "ten groups plus the entry itself"
        );
    }
}
