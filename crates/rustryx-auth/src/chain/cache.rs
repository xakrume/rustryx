//! In-process cache of authenticated principals, keyed by the SHA-256 of the bearer token.
//!
//! Why SHA-256 of the token and not the token: the cache is memory only, but a
//! heap dump should not hand out live credentials. Why a bounded lifetime:
//! a cached principal skips every check that depends on mutable state (account
//! disabled, `tokens_valid_after`, revoked `tid`, role mappings, group membership),
//! so an entry may live only as long as the credential itself, never longer than
//! [`MAX_CACHE_TTL`], and is dropped at once when the invalidation bus says the
//! user or the RBAC data changed.
//!
//! `ryx_*` API tokens are not cached by the chain: revoking a leaked token must take
//! effect on the next request, and the lookup is one indexed read.

use std::time::{Duration, Instant};

use moka::Expiry;
use moka::future::Cache;
use rustryx_core::identity::Principal;
use rustryx_core::ids::UserId;
use sha2::{Digest, Sha256};
use time::OffsetDateTime;

/// Longest a principal stays cached (security spec: 5 minutes).
pub const MAX_CACHE_TTL: Duration = Duration::from_mins(5);
/// Default capacity: an upper bound on memory, not a tuning knob.
pub const DEFAULT_CACHE_CAPACITY: u64 = 10_000;

/// Cache key: SHA-256 of the presented bearer token.
pub(crate) type TokenKey = [u8; 32];

pub(crate) fn token_key(token: &str) -> TokenKey {
    Sha256::digest(token.as_bytes()).into()
}

#[derive(Debug, Clone)]
pub(crate) struct CachedPrincipal {
    pub(crate) principal: Principal,
    /// Instant after which the entry is dead. Checked on read against the injected
    /// clock; the cache's own timer only reclaims memory.
    valid_until: OffsetDateTime,
    ttl: Duration,
}

struct PerEntryTtl;

impl Expiry<TokenKey, CachedPrincipal> for PerEntryTtl {
    fn expire_after_create(
        &self,
        _key: &TokenKey,
        value: &CachedPrincipal,
        _created_at: Instant,
    ) -> Option<Duration> {
        Some(value.ttl)
    }
}

/// Bounded principal cache with per-entry lifetime and per-user invalidation.
pub(crate) struct PrincipalCache {
    entries: Cache<TokenKey, CachedPrincipal>,
}

impl PrincipalCache {
    pub(crate) fn new(capacity: u64) -> Self {
        Self {
            entries: Cache::builder()
                .max_capacity(capacity)
                .expire_after(PerEntryTtl)
                // Needed for `invalidate_user`: without it the predicate API is refused.
                .support_invalidation_closures()
                .build(),
        }
    }

    pub(crate) async fn get(&self, key: &TokenKey, now: OffsetDateTime) -> Option<Principal> {
        let hit = self.entries.get(key).await?;
        if now >= hit.valid_until {
            self.entries.invalidate(key).await;
            return None;
        }
        Some(hit.principal)
    }

    /// Stores a principal until `valid_until`, capped at [`MAX_CACHE_TTL`] from `now`.
    /// An entry that would already be dead is not stored.
    pub(crate) async fn put(
        &self,
        key: TokenKey,
        principal: &Principal,
        valid_until: OffsetDateTime,
        now: OffsetDateTime,
    ) {
        let cap = time::Duration::try_from(MAX_CACHE_TTL).unwrap_or(time::Duration::MAX);
        let valid_until = valid_until.min(now.saturating_add(cap));
        let Ok(ttl) = Duration::try_from(valid_until - now) else {
            return;
        };
        if ttl.is_zero() {
            return;
        }
        self.entries
            .insert(
                key,
                CachedPrincipal {
                    principal: principal.clone(),
                    valid_until,
                    ttl,
                },
            )
            .await;
    }

    /// Drops every entry of one user.
    pub(crate) fn invalidate_user(&self, user: UserId) {
        let result = self
            .entries
            .invalidate_entries_if(move |_, cached| cached.principal.user_id == Some(user));
        if let Err(error) = result {
            // Cannot happen with the builder above; if it ever does, dropping everything
            // is the only safe fallback.
            tracing::error!(%error, "per-user cache invalidation unavailable, clearing all");
            self.entries.invalidate_all();
        }
    }

    pub(crate) fn invalidate_all(&self) {
        self.entries.invalidate_all();
    }

    #[cfg(test)]
    pub(crate) async fn len(&self) -> u64 {
        self.entries.run_pending_tasks().await;
        self.entries.entry_count()
    }
}

#[cfg(test)]
mod tests {
    use rustryx_core::identity::{AuthMethod, UserSource};

    use super::*;

    fn principal(user: UserId) -> Principal {
        let mut principal = Principal::new("u", UserSource::Local, AuthMethod::InternalJwt);
        principal.user_id = Some(user);
        principal
    }

    fn now() -> OffsetDateTime {
        OffsetDateTime::UNIX_EPOCH + time::Duration::days(20_000)
    }

    #[test]
    fn key_is_the_sha256_of_the_token_and_not_the_token() {
        let key = token_key("abc");
        assert_eq!(
            hex::encode(key),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert!(!key.windows(3).any(|w| w == b"abc"));
    }

    #[tokio::test]
    async fn entry_lives_until_valid_until_and_not_a_moment_longer() {
        let cache = PrincipalCache::new(10);
        let key = token_key("t");
        let user = UserId::generate();
        cache
            .put(
                key,
                &principal(user),
                now() + time::Duration::seconds(60),
                now(),
            )
            .await;
        assert!(cache.get(&key, now()).await.is_some());
        assert!(
            cache
                .get(&key, now() + time::Duration::seconds(59))
                .await
                .is_some()
        );
        assert!(
            cache
                .get(&key, now() + time::Duration::seconds(60))
                .await
                .is_none()
        );
        assert!(
            cache.get(&key, now()).await.is_none(),
            "an expired entry is gone for good"
        );
    }

    #[tokio::test]
    async fn must_not_keep_an_entry_longer_than_five_minutes() {
        let cache = PrincipalCache::new(10);
        let key = token_key("t");
        cache
            .put(
                key,
                &principal(UserId::generate()),
                now() + time::Duration::hours(8),
                now(),
            )
            .await;
        assert!(
            cache
                .get(&key, now() + time::Duration::seconds(299))
                .await
                .is_some()
        );
        assert!(
            cache
                .get(&key, now() + time::Duration::seconds(300))
                .await
                .is_none()
        );
    }

    #[tokio::test]
    async fn dead_on_arrival_entries_are_not_stored() {
        let cache = PrincipalCache::new(10);
        let key = token_key("t");
        for valid_until in [now(), now() - time::Duration::seconds(5)] {
            cache
                .put(key, &principal(UserId::generate()), valid_until, now())
                .await;
        }
        assert_eq!(cache.len().await, 0);
    }

    #[tokio::test]
    async fn invalidation_by_user_and_total() {
        let cache = PrincipalCache::new(10);
        let (alice, bob) = (UserId::generate(), UserId::generate());
        let until = now() + time::Duration::seconds(120);
        cache
            .put(token_key("a1"), &principal(alice), until, now())
            .await;
        cache
            .put(token_key("a2"), &principal(alice), until, now())
            .await;
        cache
            .put(token_key("b1"), &principal(bob), until, now())
            .await;
        cache.invalidate_user(alice);
        assert!(cache.get(&token_key("a1"), now()).await.is_none());
        assert!(cache.get(&token_key("a2"), now()).await.is_none());
        assert!(cache.get(&token_key("b1"), now()).await.is_some());
        cache.invalidate_all();
        assert!(cache.get(&token_key("b1"), now()).await.is_none());
    }

    #[tokio::test]
    async fn capacity_is_bounded() {
        let cache = PrincipalCache::new(5);
        let until = now() + time::Duration::seconds(120);
        for i in 0..200 {
            cache
                .put(
                    token_key(&format!("t{i}")),
                    &principal(UserId::generate()),
                    until,
                    now(),
                )
                .await;
        }
        assert!(cache.len().await <= 5);
    }
}
