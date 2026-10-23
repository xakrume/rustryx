//! The 60 second cache of verified Basic passwords (SEC-07).
//!
//! Why it exists: every Basic request of a local account pays one argon2id verification
//! (64 MiB, about 100 ms of one core), and a package manager that authenticates with a
//! password sends hundreds of requests a minute. Remembering "this exact user and password
//! verified a moment ago" turns all but the first into a few indexed reads.
//!
//! What it never does, and what holds each rule:
//!
//! - It never stores a password, not even hashed. The key is `HMAC-SHA256(process key,
//!   user ++ password)`: an offline guess needs the process key, which exists only in this
//!   process's memory and is random per start. A heap dump holds digests that cannot be
//!   replayed as credentials and cannot be brute forced without the key.
//! - It never answers from its own memory alone. A hit re-reads the account and serves only
//!   when the row still says what it said at verification: the account exists, is local and
//!   enabled, its stored password hash is the one that was verified (a password change
//!   moves it), and `tokens_valid_after` has not moved (an administrator revoked the
//!   sessions). Anything else drops the entry and the request takes the full verification
//!   path, which then gives the real answer (`AccountDisabled`, `InvalidCredentials`, ...).
//!   This is why a missing invalidation event cannot make a changed password or a disabled
//!   account live on: the event only frees memory sooner.
//! - It never caches a role. Roles are computed by `AuthService::finish` on every request
//!   from the account's rows, so a role change is visible at once, hit or not.
//! - It never caches an LDAP bind. The directory owns that password: a change or a disabled
//!   account there cannot be observed or invalidated from here, and a bind costs a few
//!   network round trips, not a hash. Only accounts with source `local` are remembered.
//! - It never skips the login guard: the guard runs before the lookup, so a locked account
//!   stays locked even for the right password, and a wrong password (a different key) always
//!   takes the verification path and is counted.
//!
//! The entry is created only from a verification that provably used the row it describes: the
//! row is read before the verification and again after it, and nothing is stored unless both
//! reads carry the same hash and the same `tokens_valid_after`. A password changed during the
//! verification therefore cannot leave the OLD password cached under the NEW hash.

use std::time::Duration;

use hmac::{Hmac, KeyInit, Mac};
use moka::future::Cache;
use rustryx_core::identity::{Principal, User, UserSource};
use rustryx_core::ids::UserId;
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;
use time::OffsetDateTime;

use crate::login_guard::fold_account;

/// Longest a verification is remembered (SEC-07). A configured value can only lower it.
pub const MAX_BASIC_CACHE_TTL: Duration = Duration::from_mins(1);

type HmacSha256 = Hmac<Sha256>;
type Digest32 = [u8; 32];

/// Domain separation of the key derivation: the same process key is never used for another
/// purpose, and the label pins the layout of the input below.
const KEY_LABEL: &[u8] = b"rustryx-auth/basic-cache/v1";

/// What the cache remembers about one verified credential.
#[derive(Clone)]
pub(crate) struct Verified {
    /// The principal as the local authenticator produced it (before roles).
    pub(crate) principal: Principal,
    user_id: UserId,
    /// SHA-256 of the stored password hash that was verified.
    hash_fingerprint: Digest32,
    tokens_valid_after: Option<OffsetDateTime>,
    valid_until: OffsetDateTime,
}

impl std::fmt::Debug for Verified {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // No key material and no fingerprint: neither helps a reader of a log.
        f.debug_struct("Verified")
            .field("user_id", &self.user_id)
            .field("valid_until", &self.valid_until)
            .finish_non_exhaustive()
    }
}

/// The facts of an account row that a cached verification depends on. The id is part of it:
/// two reads that name different accounts (one deleted and another created under the same login
/// in between) must never agree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Snapshot {
    user_id: UserId,
    hash_fingerprint: Digest32,
    tokens_valid_after: Option<OffsetDateTime>,
}

impl Snapshot {
    /// `None` for an account that cannot have a verified local password: not local, or no
    /// stored hash.
    pub(crate) fn of(user: &User) -> Option<Self> {
        if user.source != UserSource::Local {
            return None;
        }
        let hash = user.password_hash.as_ref()?;
        Some(Self {
            user_id: user.id,
            hash_fingerprint: Sha256::digest(hash.expose().as_bytes()).into(),
            tokens_valid_after: user.tokens_valid_after,
        })
    }
}

/// Bounded cache of verified Basic credentials. Disabled (`ttl` zero) it stores nothing.
pub(crate) struct BasicCache {
    key: Digest32,
    ttl: Duration,
    entries: Cache<Digest32, Verified>,
}

impl std::fmt::Debug for BasicCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BasicCache")
            .field("ttl", &self.ttl)
            .finish_non_exhaustive()
    }
}

impl BasicCache {
    /// A cache with a random process key. `ttl` is clamped to [`MAX_BASIC_CACHE_TTL`].
    pub(crate) fn new(ttl: Duration, capacity: u64) -> Self {
        let mut key = [0_u8; 32];
        rand::fill(&mut key);
        Self::with_key(key, ttl, capacity)
    }

    fn with_key(key: Digest32, ttl: Duration, capacity: u64) -> Self {
        let ttl = ttl.min(MAX_BASIC_CACHE_TTL);
        Self {
            key,
            ttl,
            entries: Cache::builder()
                .max_capacity(capacity)
                // Memory bound only: every decision compares `valid_until` with the injected
                // clock, so the cache's own timer cannot make an entry live longer.
                .time_to_live(ttl.max(Duration::from_secs(1)))
                // Needed for `invalidate_user`: without it the predicate API is refused.
                .support_invalidation_closures()
                .build(),
        }
    }

    pub(crate) const fn enabled(&self) -> bool {
        !self.ttl.is_zero()
    }

    /// The cache key of a credential. Length prefixed, so `("ab", "c")` and `("a", "bc")`
    /// are different inputs; the user name is folded the way the login guard folds it, because
    /// local user names are case insensitive and one account must be one entry.
    pub(crate) fn digest(&self, username: &str, password: &str) -> Digest32 {
        let user = fold_account(username);
        let Ok(mut mac) = HmacSha256::new_from_slice(&self.key) else {
            // HMAC accepts a key of any length, so this cannot happen. Should it ever, a
            // digest of constant zeros is still safe: `recall` and `remember` compare the
            // account row, and a wrong digest only ever causes a miss.
            return [0_u8; 32];
        };
        mac.update(KEY_LABEL);
        mac.update(&u32::try_from(user.len()).unwrap_or(u32::MAX).to_be_bytes());
        mac.update(user.as_bytes());
        mac.update(password.as_bytes());
        mac.finalize().into_bytes().into()
    }

    /// The remembered verification of this credential, if it is still within its lifetime.
    /// The caller must still revalidate it against the account row ([`Self::still_valid`]).
    pub(crate) async fn recall(&self, digest: &Digest32, now: OffsetDateTime) -> Option<Verified> {
        if !self.enabled() {
            return None;
        }
        let entry = self.entries.get(digest).await?;
        if now >= entry.valid_until {
            self.entries.invalidate(digest).await;
            return None;
        }
        Some(entry)
    }

    /// True when `user` is still the account that was verified: same hash, same
    /// `tokens_valid_after`, local, enabled.
    pub(crate) fn still_valid(verified: &Verified, user: &User) -> bool {
        if user.id != verified.user_id || !user.enabled {
            return false;
        }
        let Some(current) = Snapshot::of(user) else {
            return false;
        };
        let same_hash: bool = current
            .hash_fingerprint
            .ct_eq(&verified.hash_fingerprint)
            .into();
        same_hash && current.tokens_valid_after == verified.tokens_valid_after
    }

    /// Remembers a verification. `before` and `after` are the account snapshots read around
    /// the verification: unless they agree the verification may have run against another
    /// hash than the one described, and nothing is stored.
    pub(crate) async fn remember(
        &self,
        digest: Digest32,
        principal: &Principal,
        user_id: UserId,
        (before, after): (&Snapshot, &Snapshot),
        now: OffsetDateTime,
    ) {
        if !self.enabled() || before != after {
            return;
        }
        let Ok(ttl) = time::Duration::try_from(self.ttl) else {
            return;
        };
        self.entries
            .insert(
                digest,
                Verified {
                    principal: principal.clone(),
                    user_id,
                    hash_fingerprint: after.hash_fingerprint,
                    tokens_valid_after: after.tokens_valid_after,
                    valid_until: now.saturating_add(ttl),
                },
            )
            .await;
    }

    pub(crate) async fn forget(&self, digest: &Digest32) {
        self.entries.invalidate(digest).await;
    }

    /// Drops every entry of one user.
    pub(crate) fn invalidate_user(&self, user: UserId) {
        let result = self
            .entries
            .invalidate_entries_if(move |_, verified| verified.user_id == user);
        if let Err(error) = result {
            // Cannot happen with the builder above; dropping everything is the safe fallback.
            tracing::error!(%error, "per-user basic cache invalidation unavailable, clearing all");
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
    use rustryx_core::Secret;
    use rustryx_core::identity::AuthMethod;

    use super::*;

    fn now() -> OffsetDateTime {
        OffsetDateTime::UNIX_EPOCH + time::Duration::days(20_000)
    }

    fn account(hash: &str) -> User {
        User {
            id: UserId::generate(),
            username: "alice".to_owned(),
            source: UserSource::Local,
            issuer: None,
            subject: None,
            email: None,
            display_name: None,
            enabled: true,
            password_hash: Some(Secret::new(hash)),
            tokens_valid_after: None,
            created_at: now(),
        }
    }

    fn principal(user: &User) -> Principal {
        let mut principal = Principal::new(
            user.username.clone(),
            UserSource::Local,
            AuthMethod::BasicLocal,
        );
        principal.user_id = Some(user.id);
        principal
    }

    async fn remembered(cache: &BasicCache, user: &User) -> Digest32 {
        let digest = cache.digest("alice", "correct horse");
        let snapshot = Snapshot::of(user).expect("local account with a hash");
        cache
            .remember(
                digest,
                &principal(user),
                user.id,
                (&snapshot, &snapshot),
                now(),
            )
            .await;
        digest
    }

    #[test]
    fn hmac_matches_the_published_test_vector() {
        // RFC 4231 test case 2 pins the primitive this module builds on.
        let mut mac = HmacSha256::new_from_slice(b"Jefe").unwrap();
        mac.update(b"what do ya want for nothing?");
        assert_eq!(
            hex::encode(mac.finalize().into_bytes()),
            "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"
        );
    }

    #[test]
    fn the_digest_depends_on_the_process_key_and_never_contains_the_password() {
        let one = BasicCache::new(Duration::from_secs(60), 10);
        let two = BasicCache::new(Duration::from_secs(60), 10);
        let a = one.digest("alice", "correct horse");
        assert_eq!(a, one.digest("alice", "correct horse"));
        assert_ne!(a, two.digest("alice", "correct horse"), "keyed per process");
        assert_ne!(a, one.digest("alice", "correct horsf"));
        assert!(!a.windows(5).any(|w| w == b"horse"));
        let unkeyed: Digest32 = Sha256::digest(b"alicecorrect horse").into();
        assert_ne!(a, unkeyed, "not a plain hash an attacker could precompute");
    }

    #[test]
    fn the_digest_cannot_confuse_where_the_user_ends_and_the_password_starts() {
        let cache = BasicCache::new(Duration::from_secs(60), 10);
        assert_ne!(cache.digest("ab", "c"), cache.digest("a", "bc"));
        assert_ne!(cache.digest("a", "bc"), cache.digest("abc", ""));
    }

    #[test]
    fn user_names_fold_like_the_login_guard() {
        let cache = BasicCache::new(Duration::from_secs(60), 10);
        assert_eq!(
            cache.digest("Alice ", "pw pw pw"),
            cache.digest("alice", "pw pw pw")
        );
    }

    #[tokio::test]
    async fn an_entry_lives_for_the_ttl_and_not_a_moment_longer() {
        let cache = BasicCache::new(Duration::from_secs(60), 10);
        let user = account("$argon2id$one");
        let digest = remembered(&cache, &user).await;
        assert!(cache.recall(&digest, now()).await.is_some());
        assert!(
            cache
                .recall(&digest, now() + time::Duration::seconds(59))
                .await
                .is_some()
        );
        assert!(
            cache
                .recall(&digest, now() + time::Duration::seconds(60))
                .await
                .is_none()
        );
        assert!(
            cache.recall(&digest, now()).await.is_none(),
            "gone for good"
        );
    }

    #[tokio::test]
    async fn must_not_remember_longer_than_sixty_seconds_whatever_is_configured() {
        let cache = BasicCache::new(Duration::from_hours(1), 10);
        let user = account("$argon2id$one");
        let digest = remembered(&cache, &user).await;
        assert!(
            cache
                .recall(&digest, now() + time::Duration::seconds(59))
                .await
                .is_some()
        );
        assert!(
            cache
                .recall(&digest, now() + time::Duration::seconds(60))
                .await
                .is_none()
        );
    }

    #[tokio::test]
    async fn a_zero_ttl_disables_the_cache() {
        let cache = BasicCache::new(Duration::ZERO, 10);
        assert!(!cache.enabled());
        let user = account("$argon2id$one");
        let digest = remembered(&cache, &user).await;
        assert!(cache.recall(&digest, now()).await.is_none());
        assert_eq!(cache.len().await, 0);
    }

    #[tokio::test]
    async fn must_not_remember_a_verification_that_ran_while_the_hash_changed() {
        let cache = BasicCache::new(Duration::from_secs(60), 10);
        let user = account("$argon2id$one");
        let changed = account("$argon2id$two");
        let before = Snapshot::of(&user).unwrap();
        let after = Snapshot::of(&changed).unwrap();
        cache
            .remember(
                cache.digest("alice", "x x x x"),
                &principal(&user),
                user.id,
                (&before, &after),
                now(),
            )
            .await;
        assert_eq!(cache.len().await, 0);
    }

    #[tokio::test]
    async fn must_not_remember_a_verification_whose_two_reads_name_different_accounts() {
        // Same hash text, different account: only the id tells them apart.
        let cache = BasicCache::new(Duration::from_secs(60), 10);
        let first = account("$argon2id$one");
        let recreated = account("$argon2id$one");
        let before = Snapshot::of(&first).unwrap();
        let after = Snapshot::of(&recreated).unwrap();
        assert_ne!(before, after);
        cache
            .remember(
                cache.digest("alice", "x x x x"),
                &principal(&first),
                first.id,
                (&before, &after),
                now(),
            )
            .await;
        assert_eq!(cache.len().await, 0);
    }

    #[tokio::test]
    async fn a_remembered_entry_is_valid_only_for_the_unchanged_account() {
        let cache = BasicCache::new(Duration::from_secs(60), 10);
        let user = account("$argon2id$one");
        let digest = remembered(&cache, &user).await;
        let verified = cache.recall(&digest, now()).await.unwrap();
        assert!(BasicCache::still_valid(&verified, &user));

        let mut changed_password = user.clone();
        changed_password.password_hash = Some(Secret::new("$argon2id$two"));
        assert!(!BasicCache::still_valid(&verified, &changed_password));

        let mut disabled = user.clone();
        disabled.enabled = false;
        assert!(!BasicCache::still_valid(&verified, &disabled));

        let mut revoked = user.clone();
        revoked.tokens_valid_after = Some(now());
        assert!(!BasicCache::still_valid(&verified, &revoked));

        let mut other_source = user.clone();
        other_source.source = UserSource::Ldap;
        assert!(!BasicCache::still_valid(&verified, &other_source));

        let mut no_hash = user.clone();
        no_hash.password_hash = None;
        assert!(!BasicCache::still_valid(&verified, &no_hash));

        let mut other_account = user.clone();
        other_account.id = UserId::generate();
        assert!(!BasicCache::still_valid(&verified, &other_account));
    }

    #[test]
    fn only_local_accounts_with_a_hash_have_a_snapshot() {
        let mut user = account("$argon2id$one");
        assert!(Snapshot::of(&user).is_some());
        user.source = UserSource::Oidc;
        assert!(Snapshot::of(&user).is_none());
        user.source = UserSource::Local;
        user.password_hash = None;
        assert!(Snapshot::of(&user).is_none());
    }

    #[tokio::test]
    async fn invalidation_by_user_by_digest_and_total() {
        let cache = BasicCache::new(Duration::from_secs(60), 10);
        let (alice, bob) = (account("$argon2id$a"), account("$argon2id$b"));
        let da = cache.digest("alice", "pw one two");
        let db = cache.digest("bob", "pw one two");
        for (digest, user) in [(da, &alice), (db, &bob)] {
            let snapshot = Snapshot::of(user).unwrap();
            cache
                .remember(
                    digest,
                    &principal(user),
                    user.id,
                    (&snapshot, &snapshot),
                    now(),
                )
                .await;
        }
        cache.invalidate_user(alice.id);
        assert!(cache.recall(&da, now()).await.is_none());
        assert!(cache.recall(&db, now()).await.is_some());
        cache.forget(&db).await;
        assert!(cache.recall(&db, now()).await.is_none());
        let snapshot = Snapshot::of(&bob).unwrap();
        cache
            .remember(db, &principal(&bob), bob.id, (&snapshot, &snapshot), now())
            .await;
        cache.invalidate_all();
        assert!(cache.recall(&db, now()).await.is_none());
    }

    #[tokio::test]
    async fn capacity_is_bounded() {
        let cache = BasicCache::new(Duration::from_secs(60), 5);
        let user = account("$argon2id$a");
        let snapshot = Snapshot::of(&user).unwrap();
        for i in 0..200 {
            cache
                .remember(
                    cache.digest("alice", &format!("pw {i} pw")),
                    &principal(&user),
                    user.id,
                    (&snapshot, &snapshot),
                    now(),
                )
                .await;
        }
        assert!(cache.len().await <= 5);
    }

    #[tokio::test]
    async fn debug_output_shows_neither_digests_nor_fingerprints() {
        let cache = BasicCache::new(Duration::from_secs(60), 10);
        let user = account("$argon2id$secret-looking-hash");
        let digest = remembered(&cache, &user).await;
        let verified = cache.recall(&digest, now()).await.unwrap();
        let shown = format!("{cache:?} {verified:?}");
        assert!(!shown.contains("secret-looking-hash"));
        assert!(!shown.contains(&hex::encode(digest)));
        assert!(!shown.contains(&hex::encode(verified.hash_fingerprint)));
    }
}
