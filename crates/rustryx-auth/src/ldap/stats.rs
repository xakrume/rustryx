//! Counters for events that must be visible but never fail a request.
//!
//! The crate has no metrics registry dependency (that is `rustryx-observability`'s
//! job), so the counters are plain atomics with a snapshot accessor that the
//! composition root can export.

use std::sync::atomic::{AtomicU64, Ordering};

#[derive(Debug, Clone, Copy)]
pub(crate) enum Counter {
    GroupDepthLimit,
    GroupSizeLimit,
    GroupLookupFailure,
    GroupsServedStale,
    GroupsExpired,
    PoolUnhealthy,
    PoolDiscarded,
    AmbiguousUser,
    BreakerTrip,
    BreakerRejected,
}

const COUNTERS: usize = 10;

/// Live counters.
#[derive(Debug, Default)]
pub struct LdapStats {
    values: [AtomicU64; COUNTERS],
}

/// Point-in-time copy of [`LdapStats`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct LdapStatsSnapshot {
    /// Group walks that stopped expanding at the depth limit.
    pub group_depth_limit_hits: u64,
    /// Group walks that stopped adding groups at the size limit.
    pub group_size_limit_hits: u64,
    /// Group lookups that failed or timed out (before any stale fallback).
    pub group_lookup_failures: u64,
    /// Failed lookups answered with a previously confirmed set younger than 5 minutes.
    pub groups_served_stale: u64,
    /// Failed lookups with nothing younger than 5 minutes to fall back on: fail closed.
    pub groups_expired: u64,
    /// Idle service connections dropped by the health check on checkout.
    pub pool_unhealthy_evictions: u64,
    /// Service connections discarded instead of returned after an interrupted operation.
    pub pool_discarded: u64,
    /// User searches that matched more than one entry (or hit a size limit).
    pub ambiguous_user_matches: u64,
    /// Times the circuit breaker in front of the service connections opened.
    pub breaker_trips: u64,
    /// Connection attempts refused at once because the breaker was open.
    pub breaker_rejections: u64,
}

impl LdapStats {
    pub(crate) fn bump(&self, counter: Counter) {
        self.values[counter as usize].fetch_add(1, Ordering::Relaxed);
    }

    /// Reads all counters.
    #[must_use]
    pub fn snapshot(&self) -> LdapStatsSnapshot {
        let read = |counter: Counter| self.values[counter as usize].load(Ordering::Relaxed);
        LdapStatsSnapshot {
            group_depth_limit_hits: read(Counter::GroupDepthLimit),
            group_size_limit_hits: read(Counter::GroupSizeLimit),
            group_lookup_failures: read(Counter::GroupLookupFailure),
            groups_served_stale: read(Counter::GroupsServedStale),
            groups_expired: read(Counter::GroupsExpired),
            pool_unhealthy_evictions: read(Counter::PoolUnhealthy),
            pool_discarded: read(Counter::PoolDiscarded),
            ambiguous_user_matches: read(Counter::AmbiguousUser),
            breaker_trips: read(Counter::BreakerTrip),
            breaker_rejections: read(Counter::BreakerRejected),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_counter_has_its_own_slot() {
        let stats = LdapStats::default();
        for (index, counter) in [
            Counter::GroupDepthLimit,
            Counter::GroupSizeLimit,
            Counter::GroupLookupFailure,
            Counter::GroupsServedStale,
            Counter::GroupsExpired,
            Counter::PoolUnhealthy,
            Counter::PoolDiscarded,
            Counter::AmbiguousUser,
            Counter::BreakerTrip,
            Counter::BreakerRejected,
        ]
        .into_iter()
        .enumerate()
        {
            assert_eq!(counter as usize, index);
            stats.bump(counter);
        }
        let snapshot = stats.snapshot();
        assert_eq!(snapshot.group_depth_limit_hits, 1);
        assert_eq!(snapshot.group_size_limit_hits, 1);
        assert_eq!(snapshot.group_lookup_failures, 1);
        assert_eq!(snapshot.groups_served_stale, 1);
        assert_eq!(snapshot.groups_expired, 1);
        assert_eq!(snapshot.pool_unhealthy_evictions, 1);
        assert_eq!(snapshot.pool_discarded, 1);
        assert_eq!(snapshot.ambiguous_user_matches, 1);
        assert_eq!(snapshot.breaker_trips, 1);
        assert_eq!(snapshot.breaker_rejections, 1);
        assert_eq!(COUNTERS, 10);
    }
}
