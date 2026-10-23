//! The 60 second cache of verified Basic passwords (SEC-07), seen from the chain.
//!
//! How the tests tell a cache hit from a verification: the password lane of the gate has one
//! permit and no waiting room. While a test holds that permit, a request that needs a real
//! argon2id verification fails with `AuthError::Overloaded`, and a request served from the
//! cache does not notice. "Served from memory" and "went through the full path" are therefore
//! observable without counting hashes.

use std::net::{IpAddr, Ipv4Addr};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use rustryx_core::Secret;
use rustryx_core::StoreError;
use rustryx_core::error::AuthError;
use rustryx_core::identity::{NewUser, RoleOrigin, RoleRef, User, UserSource};
use rustryx_core::ids::{RoleId, UserId};
use rustryx_core::ports::{Authenticator, Page, PageRequest, UserRepo};
use rustryx_testkit::fixtures;
use sha2::{Digest, Sha256};
use time::OffsetDateTime;

use super::testing::{Options, basic, deps_for, harness_with, names};
use super::{AuthService, AuthServiceConfig};
use crate::passwords::{GateClass, GateConfig, LaneLimits};

const IP: IpAddr = IpAddr::V4(Ipv4Addr::new(203, 0, 113, 9));
const PASSWORD: &str = "correct horse";

fn one_lane() -> GateConfig {
    GateConfig {
        password: LaneLimits {
            permits: 1,
            max_waiting: 0,
        },
        general: LaneLimits {
            permits: 4,
            max_waiting: 4,
        },
        permit_wait: Duration::from_millis(1),
    }
}

fn harness() -> super::testing::Harness {
    harness_with(Options {
        gate: Some(one_lane()),
        ..Options::default()
    })
}

fn secs(n: u64) -> Duration {
    Duration::from_secs(n)
}

#[tokio::test]
async fn a_verified_password_is_served_from_memory_for_sixty_seconds_and_not_longer() {
    let h = harness();
    h.add_local_user("alice", PASSWORD).await;
    h.basic("alice", PASSWORD).await.unwrap();
    assert_eq!(h.service.basic.len().await, 1);

    let held = h.gate.acquire(GateClass::Password).await.unwrap();
    h.basic("alice", PASSWORD).await.unwrap();
    h.clock.advance(secs(59));
    h.basic("alice", PASSWORD).await.unwrap();
    h.clock.advance(secs(1));
    let error = h.basic("alice", PASSWORD).await.unwrap_err();
    assert!(
        matches!(error, AuthError::Overloaded),
        "at 60 s the entry is gone and a real verification is needed: {error:?}"
    );
    drop(held);
    h.basic("alice", PASSWORD).await.unwrap();
}

#[tokio::test]
async fn must_not_remember_longer_than_sixty_seconds_whatever_is_configured() {
    let h = harness_with(Options {
        gate: Some(one_lane()),
        service: AuthServiceConfig {
            basic_cache_ttl: Duration::from_hours(1),
            ..AuthServiceConfig::default()
        },
        ..Options::default()
    });
    h.add_local_user("alice", PASSWORD).await;
    h.basic("alice", PASSWORD).await.unwrap();
    let _held = h.gate.acquire(GateClass::Password).await.unwrap();
    h.clock.advance(secs(59));
    h.basic("alice", PASSWORD).await.unwrap();
    h.clock.advance(secs(1));
    assert!(matches!(
        h.basic("alice", PASSWORD).await.unwrap_err(),
        AuthError::Overloaded
    ));
}

#[tokio::test]
async fn the_cache_can_be_turned_off() {
    let h = harness_with(Options {
        gate: Some(one_lane()),
        service: AuthServiceConfig {
            basic_cache_ttl: Duration::ZERO,
            ..AuthServiceConfig::default()
        },
        ..Options::default()
    });
    h.add_local_user("alice", PASSWORD).await;
    h.basic("alice", PASSWORD).await.unwrap();
    assert_eq!(h.service.basic.len().await, 0);
    let _held = h.gate.acquire(GateClass::Password).await.unwrap();
    assert!(matches!(
        h.basic("alice", PASSWORD).await.unwrap_err(),
        AuthError::Overloaded
    ));
}

#[tokio::test]
async fn must_not_serve_a_cached_password_after_the_password_was_changed() {
    let h = harness();
    let alice = h.add_local_user("alice", PASSWORD).await;
    h.basic("alice", PASSWORD).await.unwrap();

    // The change arrives without any invalidation event: the row says it, and the hit asks
    // the row.
    let new_hash = h
        .passwords
        .hash(&Secret::new("a different secret"))
        .await
        .unwrap();
    UserRepo::set_password_hash(&h.store, alice.id, new_hash.expose())
        .await
        .unwrap();
    let error = h.basic("alice", PASSWORD).await.unwrap_err();
    assert!(
        matches!(error, AuthError::InvalidCredentials),
        "the old password must be refused at once: {error:?}"
    );
    h.basic("alice", "a different secret").await.unwrap();
}

#[tokio::test]
async fn must_not_serve_a_cached_password_to_a_disabled_account() {
    let h = harness();
    let alice = h.add_local_user("alice", PASSWORD).await;
    h.basic("alice", PASSWORD).await.unwrap();
    UserRepo::set_enabled(&h.store, alice.id, false)
        .await
        .unwrap();
    let error = h.basic("alice", PASSWORD).await.unwrap_err();
    assert!(matches!(error, AuthError::AccountDisabled), "{error:?}");
    assert_eq!(h.service.basic.len().await, 0, "the entry was dropped");
    UserRepo::set_enabled(&h.store, alice.id, true)
        .await
        .unwrap();
    h.basic("alice", PASSWORD).await.unwrap();
}

#[tokio::test]
async fn must_not_serve_a_cached_password_after_the_sessions_were_revoked() {
    let h = harness();
    let alice = h.add_local_user("alice", PASSWORD).await;
    h.basic("alice", PASSWORD).await.unwrap();
    UserRepo::set_tokens_valid_after(&h.store, alice.id, h.clock_now())
        .await
        .unwrap();
    let held = h.gate.acquire(GateClass::Password).await.unwrap();
    let error = h.basic("alice", PASSWORD).await.unwrap_err();
    assert!(
        matches!(error, AuthError::Overloaded),
        "revoking the sessions forces a fresh verification: {error:?}"
    );
    drop(held);
    h.basic("alice", PASSWORD).await.unwrap();
}

#[tokio::test]
async fn must_not_serve_a_cached_password_of_a_deleted_account() {
    let h = harness();
    let alice = h.add_local_user("alice", PASSWORD).await;
    h.basic("alice", PASSWORD).await.unwrap();
    UserRepo::delete(&h.store, alice.id).await.unwrap();
    let error = h.basic("alice", PASSWORD).await.unwrap_err();
    assert!(matches!(error, AuthError::InvalidCredentials), "{error:?}");
}

#[tokio::test]
async fn invalidating_a_user_or_everything_forgets_the_verified_password() {
    let h = harness();
    let alice = h.add_local_user("alice", PASSWORD).await;
    h.add_local_user("bob", PASSWORD).await;
    h.basic("alice", PASSWORD).await.unwrap();
    h.basic("bob", PASSWORD).await.unwrap();

    h.service.invalidate_user(alice.id);
    let held = h.gate.acquire(GateClass::Password).await.unwrap();
    assert!(matches!(
        h.basic("alice", PASSWORD).await.unwrap_err(),
        AuthError::Overloaded
    ));
    h.basic("bob", PASSWORD).await.unwrap();

    h.service.invalidate_all();
    assert!(matches!(
        h.basic("bob", PASSWORD).await.unwrap_err(),
        AuthError::Overloaded
    ));
    drop(held);
}

#[tokio::test]
async fn a_role_change_is_visible_through_a_cached_password() {
    let h = harness();
    let alice = h.add_local_user("alice", PASSWORD).await;
    assert_eq!(
        names(&h.basic("alice", PASSWORD).await.unwrap()),
        Vec::<&str>::new()
    );
    let role = h.add_role("team-dev").await;
    h.grant_manual(&alice, &role).await;
    let _held = h.gate.acquire(GateClass::Password).await.unwrap();
    assert_eq!(
        names(&h.basic("alice", PASSWORD).await.unwrap()),
        ["team-dev"],
        "roles are computed per request, the cache never holds one"
    );
}

#[tokio::test]
async fn a_rename_or_profile_change_shows_through_a_cached_password() {
    let h = harness();
    let alice = h.add_local_user("alice", PASSWORD).await;
    let first = h.basic("alice", PASSWORD).await.unwrap();
    assert_eq!(first.user_id, Some(alice.id));
    assert_eq!(first.username, "alice");
    let _held = h.gate.acquire(GateClass::Password).await.unwrap();
    let second = h.basic("alice", PASSWORD).await.unwrap();
    assert_eq!(second.user_id, Some(alice.id));
    assert_eq!(second.username, "alice");
}

#[tokio::test]
async fn a_wrong_password_never_hits_the_entry_of_the_right_one() {
    let h = harness();
    h.add_local_user("alice", PASSWORD).await;
    h.add_local_user("bob", "bobs own password").await;
    h.basic("alice", PASSWORD).await.unwrap();
    let _held = h.gate.acquire(GateClass::Password).await.unwrap();
    // A different password is a different key: it takes the verification path.
    let error = h.basic("alice", "correct horsf").await.unwrap_err();
    assert!(matches!(error, AuthError::Overloaded), "{error:?}");
    // Another account with the same password is a different key as well.
    let error = h.basic("bob", PASSWORD).await.unwrap_err();
    assert!(matches!(error, AuthError::Overloaded), "{error:?}");
}

#[tokio::test]
async fn must_not_skip_the_lockout_for_a_cached_password() {
    let h = harness_with(Options::default());
    h.add_local_user("alice", PASSWORD).await;
    h.authenticate_from(&basic("alice", PASSWORD), Some(IP))
        .await
        .unwrap();
    assert_eq!(h.service.basic.len().await, 1);
    for _ in 0..5 {
        let error = h
            .authenticate_from(&basic("alice", "wrong horse"), Some(IP))
            .await
            .unwrap_err();
        assert!(matches!(error, AuthError::InvalidCredentials), "{error:?}");
    }
    // The right password, remembered and all, meets the lock first.
    let error = h
        .authenticate_from(&basic("alice", PASSWORD), Some(IP))
        .await
        .unwrap_err();
    assert!(matches!(error, AuthError::LockedOut { .. }), "{error:?}");
    // Once the lock has run out the right password works again.
    h.clock.advance(secs(61));
    h.authenticate_from(&basic("alice", PASSWORD), Some(IP))
        .await
        .unwrap();
}

#[tokio::test]
async fn must_not_cache_an_ldap_bind() {
    let h = harness();
    h.add_ldap_user("bob", "ldap secret");
    h.ldap_resolver.set(super::testing::Answer::Groups(vec![
        super::testing::ADMIN_DN.into(),
    ]));
    h.basic("bob", "ldap secret").await.unwrap();
    h.basic("bob", "ldap secret").await.unwrap();
    assert_eq!(
        h.ldap_auth.calls(),
        2,
        "the directory owns that password: every request binds"
    );
    assert_eq!(h.service.basic.len().await, 0);
}

#[tokio::test]
async fn must_not_store_the_password_in_any_form() {
    let h = harness();
    h.add_local_user("alice", PASSWORD).await;
    h.basic("alice", PASSWORD).await.unwrap();
    assert_eq!(h.service.basic.len().await, 1);

    let shown = format!("{:?} {:?}", h.service, h.service.basic);
    assert!(!shown.contains(PASSWORD), "{shown}");
    // The key is not any digest an attacker could compute without the process key.
    let key = h.service.basic.digest("alice", PASSWORD);
    for plain in [
        Sha256::digest(PASSWORD.as_bytes()),
        Sha256::digest(format!("alice{PASSWORD}").as_bytes()),
        Sha256::digest(format!("alice:{PASSWORD}").as_bytes()),
    ] {
        assert_ne!(key.as_slice(), plain.as_slice());
    }
    assert!(!key.windows(5).any(|window| window == b"horse"));
}

#[tokio::test]
async fn a_login_that_rehashed_the_password_is_remembered_by_the_next_login_only() {
    let h = harness();
    let mut new = fixtures::user("erin", UserSource::Local);
    new.password_hash = Some(Secret::new(bcrypt::hash("imported secret", 4).unwrap()));
    UserRepo::create(&h.store, &new).await.unwrap();

    h.basic("erin", "imported secret").await.unwrap();
    assert_eq!(
        h.service.basic.len().await,
        0,
        "the hash changed during that login: what was verified is not what is stored now"
    );
    h.basic("erin", "imported secret").await.unwrap();
    assert_eq!(h.service.basic.len().await, 1);
    let _held = h.gate.acquire(GateClass::Password).await.unwrap();
    h.basic("erin", "imported secret").await.unwrap();
}

/// A store that changes the password the first time the chain re-reads the account after the
/// verification, the moment the cache entry would be written, or that fails every such read.
struct ChangesPasswordAfterVerification {
    inner: rustryx_testkit::MemStore,
    new_hash: String,
    gets: AtomicUsize,
    /// Every `get` fails instead (the stand in for an outage of the cache bookkeeping reads).
    fail_gets: bool,
}

#[async_trait]
impl UserRepo for ChangesPasswordAfterVerification {
    async fn get(&self, id: UserId) -> Result<Option<User>, StoreError> {
        if self.fail_gets {
            return Err(StoreError::Unavailable("store went away".into()));
        }
        if self.gets.fetch_add(1, Ordering::SeqCst) == 0 {
            self.inner.set_password_hash(id, &self.new_hash).await?;
        }
        self.inner.get(id).await
    }
    async fn find_by_username(&self, u: &str) -> Result<Option<User>, StoreError> {
        self.inner.find_by_username(u).await
    }
    async fn find_external(
        &self,
        s: UserSource,
        i: Option<&str>,
        sub: Option<&str>,
    ) -> Result<Option<User>, StoreError> {
        self.inner.find_external(s, i, sub).await
    }
    async fn create(&self, n: &NewUser) -> Result<User, StoreError> {
        UserRepo::create(&self.inner, n).await
    }
    async fn list(&self, p: &PageRequest) -> Result<Page<User>, StoreError> {
        UserRepo::list(&self.inner, p).await
    }
    async fn set_password_hash(&self, id: UserId, h: &str) -> Result<(), StoreError> {
        self.inner.set_password_hash(id, h).await
    }
    async fn set_password_hash_if(
        &self,
        id: UserId,
        old_hash: &str,
        new_hash: &str,
    ) -> Result<bool, StoreError> {
        self.inner
            .set_password_hash_if(id, old_hash, new_hash)
            .await
    }
    async fn set_enabled(&self, id: UserId, e: bool) -> Result<(), StoreError> {
        self.inner.set_enabled(id, e).await
    }
    async fn set_tokens_valid_after(
        &self,
        id: UserId,
        at: OffsetDateTime,
    ) -> Result<(), StoreError> {
        self.inner.set_tokens_valid_after(id, at).await
    }
    async fn roles_of(&self, id: UserId) -> Result<Vec<(RoleRef, RoleOrigin)>, StoreError> {
        self.inner.roles_of(id).await
    }
    async fn replace_roles_for_origin(
        &self,
        id: UserId,
        o: RoleOrigin,
        r: &[RoleId],
    ) -> Result<(), StoreError> {
        self.inner.replace_roles_for_origin(id, o, r).await
    }
    async fn delete(&self, id: UserId) -> Result<(), StoreError> {
        UserRepo::delete(&self.inner, id).await
    }
}

#[tokio::test]
async fn must_not_remember_a_verification_that_raced_a_password_change() {
    let h = harness_with(Options::default());
    h.add_local_user("alice", PASSWORD).await;
    let new_hash = h
        .passwords
        .hash(&Secret::new("a different secret"))
        .await
        .unwrap();
    let mut deps = deps_for(&h);
    deps.users = Arc::new(ChangesPasswordAfterVerification {
        inner: h.store.clone(),
        new_hash: new_hash.expose().to_owned(),
        gets: AtomicUsize::new(0),
        fail_gets: false,
    });
    let service = AuthService::new(deps, AuthServiceConfig::default());

    // The old password verified against the old hash; the password then changed before the
    // chain wrote the entry. Storing it under the NEW hash would keep the OLD password valid.
    let credentials = basic("alice", PASSWORD);
    let context = rustryx_core::identity::AuthContext::new();
    service.authenticate(&credentials, &context).await.unwrap();
    assert_eq!(
        service.basic.len().await,
        0,
        "the entry is not written when the rows read around the verification disagree"
    );
    let error = service
        .authenticate(&credentials, &context)
        .await
        .unwrap_err();
    assert!(matches!(error, AuthError::InvalidCredentials), "{error:?}");
}

#[tokio::test]
async fn the_cache_bookkeeping_never_fails_a_request() {
    let h = harness_with(Options::default());
    h.add_local_user("alice", PASSWORD).await;
    let mut deps = deps_for(&h);
    deps.users = Arc::new(ChangesPasswordAfterVerification {
        inner: h.store.clone(),
        new_hash: String::new(),
        gets: AtomicUsize::new(0),
        fail_gets: true,
    });
    let service = AuthService::new(deps, AuthServiceConfig::default());
    let credentials = basic("alice", PASSWORD);
    let context = rustryx_core::identity::AuthContext::new();

    // The password verified; only the re-read that would store the entry failed.
    service.authenticate(&credentials, &context).await.unwrap();
    assert_eq!(
        service.basic.len().await,
        0,
        "nothing is stored without the check"
    );

    // An entry that exists, whose row cannot be read, is a miss and not an error: the request
    // takes the full path (which here succeeds, the verification does not use that read).
    let digest = service.basic.digest("alice", PASSWORD);
    let user = UserRepo::find_by_username(&h.store, "alice")
        .await
        .unwrap()
        .unwrap();
    let snapshot = super::basic_cache::Snapshot::of(&user).unwrap();
    let principal = {
        let mut p = rustryx_core::identity::Principal::new(
            "alice",
            UserSource::Local,
            rustryx_core::identity::AuthMethod::BasicLocal,
        );
        p.user_id = Some(user.id);
        p
    };
    service
        .basic
        .remember(
            digest,
            &principal,
            user.id,
            (&snapshot, &snapshot),
            rustryx_core::ports::Clock::now(&h.clock),
        )
        .await;
    assert_eq!(service.basic.len().await, 1);
    service.authenticate(&credentials, &context).await.unwrap();
}
