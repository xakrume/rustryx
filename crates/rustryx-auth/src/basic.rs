//! HTTP Basic for local accounts.
//!
//! Claims `Basic user:password` for users whose source is `local`. Everything else
//! returns `Ok(None)` so the chain can try the next authenticator:
//!
//! - a password starting with `ryx_` belongs to the API token authenticator;
//! - an unknown user may be an LDAP user that has not logged in yet, and an LDAP or
//!   OIDC account must never be authenticated with a local password hash.
//!
//! The chain, not this type, runs the dummy hash when nobody claims the credentials,
//! and applies the login guard around the whole sequence.

#![allow(clippy::missing_errors_doc)] // errors are `AuthError` variants documented on the type

use std::sync::Arc;
use std::time::Instant;

use async_trait::async_trait;
use rustryx_core::error::AuthError;
use rustryx_core::identity::{
    API_TOKEN_PREFIX, AuthContext, AuthMethod, Credentials, Principal, User, UserSource,
};
use rustryx_core::ports::{Authenticator, UserRepo};

use crate::passwords::errmap::store_failure;
use crate::passwords::{Passwords, Verification, is_usable_password};

/// Authenticates local accounts by password.
pub struct LocalPasswordAuthenticator {
    users: Arc<dyn UserRepo>,
    passwords: Arc<Passwords>,
}

impl std::fmt::Debug for LocalPasswordAuthenticator {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LocalPasswordAuthenticator")
            .finish_non_exhaustive()
    }
}

impl LocalPasswordAuthenticator {
    /// Builds the authenticator.
    #[must_use]
    pub fn new(users: Arc<dyn UserRepo>, passwords: Arc<Passwords>) -> Self {
        Self { users, passwords }
    }

    /// Rewrites a hash that no longer meets the target (bcrypt import, raised
    /// argon2id parameters) after a successful login.
    ///
    /// Best effort: a login must not fail because the upgrade did. The write is a
    /// compare and set on the hash that was just verified
    /// (`UserRepo::set_password_hash_if`), so a password changed after this login read its
    /// row is never overwritten with a hash of the OLD password: the swap simply does not
    /// happen and the change wins. The phase 1 code re-read the row first, which narrowed
    /// the window but could not close it.
    async fn rehash(
        &self,
        user: &User,
        verified_hash: &str,
        password: &rustryx_core::Secret,
        deadline: Option<Instant>,
    ) {
        let fresh = match self.passwords.hash_before(password, deadline).await {
            Ok(hash) => hash,
            Err(error) => {
                tracing::debug!(user_id = %user.id, %error, "password rehash skipped");
                return;
            }
        };
        match self
            .users
            .set_password_hash_if(user.id, verified_hash, fresh.expose())
            .await
        {
            Ok(true) => {}
            Ok(false) => {
                tracing::debug!(user_id = %user.id, "password changed during login, rehash dropped");
            }
            Err(error) => {
                tracing::warn!(user_id = %user.id, %error, "could not store the upgraded password hash");
            }
        }
    }
}

#[async_trait]
impl Authenticator for LocalPasswordAuthenticator {
    async fn authenticate(
        &self,
        credentials: &Credentials,
        context: &AuthContext,
    ) -> Result<Option<Principal>, AuthError> {
        let Credentials::Basic { username, password } = credentials else {
            return Ok(None);
        };
        if password.expose().starts_with(API_TOKEN_PREFIX) {
            return Ok(None);
        }
        // Before any lookup or hash: blank credentials are refused outright.
        if username.trim().is_empty() || !is_usable_password(password.expose()) {
            return Err(AuthError::InvalidCredentials);
        }
        let found = self
            .users
            .find_by_username(username)
            .await
            .map_err(|error| store_failure("basic user lookup", error))?;
        let Some(user) = found.filter(|user| user.source == UserSource::Local) else {
            return Ok(None);
        };
        // No stored hash (an account that cannot log in locally) is checked against
        // an impossible one so the response time matches a wrong password.
        let stored = user
            .password_hash
            .as_ref()
            .map(|hash| hash.expose().to_owned())
            .unwrap_or_default();
        let verification = self
            .passwords
            .verify_before(password, &stored, context.deadline)
            .await?;
        let Verification::Valid { needs_rehash } = verification else {
            return Err(AuthError::InvalidCredentials);
        };
        // Disabled is reported only to someone who proved the password. A wrong
        // password gets the same answer for enabled and disabled accounts.
        if !user.enabled {
            return Err(AuthError::AccountDisabled);
        }
        if needs_rehash {
            self.rehash(&user, &stored, password, context.deadline)
                .await;
        }
        let mut principal =
            Principal::new(user.username, UserSource::Local, AuthMethod::BasicLocal);
        principal.user_id = Some(user.id);
        principal.email = user.email;
        principal.display_name = user.display_name;
        Ok(Some(principal))
    }
}

#[cfg(test)]
mod tests {
    use rustryx_core::identity::{NewUser, RoleOrigin, RoleRef};
    use rustryx_core::ids::{RoleId, UserId};
    use rustryx_core::ports::{Page, PageRequest};
    use rustryx_core::{Secret, StoreError};
    use rustryx_testkit::{MemStore, fixtures};
    use time::OffsetDateTime;

    use super::*;
    use crate::passwords::{BlockingGate, GateConfig, PasswordConfig};

    struct Env {
        store: MemStore,
        passwords: Arc<Passwords>,
        auth: LocalPasswordAuthenticator,
    }

    fn env() -> Env {
        let store = MemStore::new();
        let gate = Arc::new(BlockingGate::new(GateConfig::default()));
        let passwords = Arc::new(
            Passwords::new_for_tests(
                PasswordConfig {
                    memory_kib: 8,
                    iterations: 1,
                    parallelism: 1,
                },
                gate,
            )
            .unwrap(),
        );
        let auth = LocalPasswordAuthenticator::new(Arc::new(store.clone()), Arc::clone(&passwords));
        Env {
            store,
            passwords,
            auth,
        }
    }

    async fn add_user(env: &Env, name: &str, source: UserSource, password: Option<&str>) -> User {
        let mut new = fixtures::user(name, source);
        if let Some(password) = password {
            new.password_hash = Some(env.passwords.hash(&Secret::new(password)).await.unwrap());
        }
        UserRepo::create(&env.store, &new).await.unwrap()
    }

    fn basic(user: &str, password: &str) -> Credentials {
        Credentials::Basic {
            username: user.to_owned(),
            password: Secret::new(password),
        }
    }

    async fn login(env: &Env, user: &str, password: &str) -> Result<Option<Principal>, AuthError> {
        env.auth
            .authenticate(&basic(user, password), &AuthContext::default())
            .await
    }

    #[tokio::test]
    async fn local_user_with_the_right_password_gets_a_principal() {
        let env = env();
        let alice = add_user(&env, "alice", UserSource::Local, Some("correct horse")).await;
        let principal = login(&env, "alice", "correct horse")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(principal.user_id, Some(alice.id));
        assert_eq!(principal.username, "alice");
        assert_eq!(principal.source, UserSource::Local);
        assert_eq!(principal.auth_method, AuthMethod::BasicLocal);
        assert_eq!(principal.roles.len() + principal.external_groups.len(), 0);
    }

    #[tokio::test]
    async fn wrong_password_is_invalid_credentials() {
        let env = env();
        add_user(&env, "alice", UserSource::Local, Some("correct horse")).await;
        let error = login(&env, "alice", "wrong horse").await.unwrap_err();
        assert!(matches!(error, AuthError::InvalidCredentials));
    }

    #[tokio::test]
    async fn not_applicable_credentials_are_passed_on() {
        let env = env();
        add_user(&env, "alice", UserSource::Local, Some("correct horse")).await;
        let ctx = AuthContext::default();
        assert!(
            env.auth
                .authenticate(&Credentials::Anonymous, &ctx)
                .await
                .unwrap()
                .is_none()
        );
        let bearer = Credentials::Bearer(Secret::new("anything"));
        assert!(
            env.auth
                .authenticate(&bearer, &ctx)
                .await
                .unwrap()
                .is_none()
        );
        // Unknown user: maybe an LDAP user, so not ours to refuse.
        assert!(login(&env, "nobody", "whatever").await.unwrap().is_none());
        // API token passwords belong to the token authenticator.
        assert!(
            login(&env, "alice", "ryx_not-a-real-token")
                .await
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test]
    async fn must_not_authenticate_ldap_or_oidc_accounts_with_a_local_password() {
        let env = env();
        for (name, source) in [("bob", UserSource::Ldap), ("carol", UserSource::Oidc)] {
            let mut new = fixtures::user(name, source);
            // Even a planted, valid hash must not make an external account local.
            new.password_hash = Some(
                env.passwords
                    .hash(&Secret::new("planted pass"))
                    .await
                    .unwrap(),
            );
            UserRepo::create(&env.store, &new).await.unwrap();
            assert!(
                login(&env, name, "planted pass").await.unwrap().is_none(),
                "{name}"
            );
        }
    }

    #[tokio::test]
    async fn must_not_reach_a_hash_with_an_empty_or_blank_password() {
        let env = env();
        add_user(&env, "alice", UserSource::Local, Some("correct horse")).await;
        for blank in ["", " ", "\t\n ", "\u{a0}"] {
            let error = login(&env, "alice", blank).await.unwrap_err();
            assert!(matches!(error, AuthError::InvalidCredentials), "{blank:?}");
        }
        for blank_user in ["", "  "] {
            let error = login(&env, blank_user, "correct horse").await.unwrap_err();
            assert!(matches!(error, AuthError::InvalidCredentials));
        }
        // An account whose stored hash is empty must not accept an empty password either.
        let mut new = fixtures::user("hollow", UserSource::Local);
        new.password_hash = Some(Secret::new(""));
        UserRepo::create(&env.store, &new).await.unwrap();
        assert!(login(&env, "hollow", "").await.is_err());
        let error = login(&env, "hollow", "anything").await.unwrap_err();
        assert!(matches!(error, AuthError::InvalidCredentials));
    }

    #[tokio::test]
    async fn account_without_a_hash_cannot_log_in_locally() {
        let env = env();
        add_user(&env, "nohash", UserSource::Local, None).await;
        let error = login(&env, "nohash", "anything at all").await.unwrap_err();
        assert!(matches!(error, AuthError::InvalidCredentials));
    }

    #[tokio::test]
    async fn disabled_account_is_reported_only_after_the_password_is_proven() {
        let env = env();
        let user = add_user(&env, "dave", UserSource::Local, Some("correct horse")).await;
        UserRepo::set_enabled(&env.store, user.id, false)
            .await
            .unwrap();
        let wrong = login(&env, "dave", "wrong horse").await.unwrap_err();
        assert!(matches!(wrong, AuthError::InvalidCredentials), "{wrong:?}");
        let right = login(&env, "dave", "correct horse").await.unwrap_err();
        assert!(matches!(right, AuthError::AccountDisabled), "{right:?}");
    }

    #[tokio::test]
    async fn bcrypt_hash_is_accepted_and_rewritten_as_argon2id() {
        let env = env();
        let mut new = fixtures::user("erin", UserSource::Local);
        let bcrypt_hash = bcrypt::hash("imported secret", 4).unwrap();
        new.password_hash = Some(Secret::new(bcrypt_hash.clone()));
        let erin = UserRepo::create(&env.store, &new).await.unwrap();

        login(&env, "erin", "imported secret")
            .await
            .unwrap()
            .unwrap();
        let upgraded = UserRepo::get(&env.store, erin.id).await.unwrap().unwrap();
        let hash = upgraded.password_hash.unwrap();
        assert!(hash.expose().starts_with("$argon2id$"));
        assert_ne!(hash.expose(), bcrypt_hash);
        // The new hash works, and a second login does not rewrite it again.
        login(&env, "erin", "imported secret")
            .await
            .unwrap()
            .unwrap();
        let again = UserRepo::get(&env.store, erin.id).await.unwrap().unwrap();
        assert_eq!(again.password_hash.unwrap().expose(), hash.expose());
    }

    #[tokio::test]
    async fn must_not_upgrade_the_hash_after_a_failed_login() {
        let env = env();
        let mut new = fixtures::user("frank", UserSource::Local);
        let bcrypt_hash = bcrypt::hash("imported secret", 4).unwrap();
        new.password_hash = Some(Secret::new(bcrypt_hash.clone()));
        let frank = UserRepo::create(&env.store, &new).await.unwrap();
        assert!(login(&env, "frank", "wrong").await.is_err());
        let stored = UserRepo::get(&env.store, frank.id).await.unwrap().unwrap();
        assert_eq!(stored.password_hash.unwrap().expose(), bcrypt_hash);
    }

    struct Racy {
        inner: MemStore,
        replace_with: String,
        writes_seen: std::sync::atomic::AtomicUsize,
        /// The compare and set itself fails, as a store outage during the upgrade would.
        fail_the_write: bool,
    }
    #[async_trait]
    impl UserRepo for Racy {
        async fn get(&self, id: UserId) -> Result<Option<User>, StoreError> {
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
            self.writes_seen
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            if self.fail_the_write {
                return Err(StoreError::Unavailable("store went away".into()));
            }
            // Someone changes the password just before the compare and set arrives.
            self.inner.set_password_hash(id, &self.replace_with).await?;
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
    async fn must_not_overwrite_a_password_changed_during_the_login() {
        // A store that swaps the password between the verification and the rehash write, the
        // exact window the phase 1 re-read could not close.
        let env = env();
        let mut new = fixtures::user("gina", UserSource::Local);
        new.password_hash = Some(Secret::new(bcrypt::hash("old secret", 4).unwrap()));
        let gina = UserRepo::create(&env.store, &new).await.unwrap();
        let changed = env
            .passwords
            .hash(&Secret::new("brand new secret"))
            .await
            .unwrap();
        let racy = Arc::new(Racy {
            inner: env.store.clone(),
            replace_with: changed.expose().to_owned(),
            writes_seen: std::sync::atomic::AtomicUsize::new(0),
            fail_the_write: false,
        });
        let auth = LocalPasswordAuthenticator::new(racy.clone(), Arc::clone(&env.passwords));
        auth.authenticate(&basic("gina", "old secret"), &AuthContext::default())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            racy.writes_seen.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "the upgrade was attempted exactly once, as a compare and set"
        );
        let stored = UserRepo::get(&env.store, gina.id).await.unwrap().unwrap();
        assert_eq!(
            stored.password_hash.unwrap().expose(),
            changed.expose(),
            "the concurrently set password must survive the rehash"
        );
    }

    #[tokio::test]
    async fn a_failing_upgrade_write_never_fails_the_login() {
        let env = env();
        let mut new = fixtures::user("hank", UserSource::Local);
        let bcrypt_hash = bcrypt::hash("imported secret", 4).unwrap();
        new.password_hash = Some(Secret::new(bcrypt_hash.clone()));
        let hank = UserRepo::create(&env.store, &new).await.unwrap();
        let failing = Arc::new(Racy {
            inner: env.store.clone(),
            replace_with: String::new(),
            writes_seen: std::sync::atomic::AtomicUsize::new(0),
            fail_the_write: true,
        });
        let auth = LocalPasswordAuthenticator::new(failing.clone(), Arc::clone(&env.passwords));
        let principal = auth
            .authenticate(&basic("hank", "imported secret"), &AuthContext::default())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(principal.user_id, Some(hank.id));
        assert_eq!(
            failing
                .writes_seen
                .load(std::sync::atomic::Ordering::SeqCst),
            1
        );
        let stored = UserRepo::get(&env.store, hank.id).await.unwrap().unwrap();
        assert_eq!(
            stored.password_hash.unwrap().expose(),
            bcrypt_hash,
            "the old hash stays and the next login tries again"
        );
    }

    #[tokio::test]
    async fn the_principal_carries_the_profile_of_the_account() {
        let env = env();
        add_user(&env, "alice", UserSource::Local, Some("correct horse")).await;
        let principal = login(&env, "alice", "correct horse")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(principal.email.as_deref(), Some("alice@example.test"));
        assert_eq!(principal.display_name.as_deref(), Some("alice"));
        assert!(principal.expires_at.is_none() && principal.external_id.is_none());
    }

    #[tokio::test]
    async fn must_not_start_a_hash_after_the_request_deadline() {
        let env = env();
        add_user(&env, "alice", UserSource::Local, Some("correct horse")).await;
        let expired = AuthContext::new().with_deadline(Instant::now());
        let error = env
            .auth
            .authenticate(&basic("alice", "correct horse"), &expired)
            .await
            .unwrap_err();
        assert!(matches!(error, AuthError::Overloaded), "{error:?}");
        let alive =
            AuthContext::new().with_deadline(Instant::now() + std::time::Duration::from_secs(30));
        assert!(
            env.auth
                .authenticate(&basic("alice", "correct horse"), &alive)
                .await
                .unwrap()
                .is_some()
        );
    }

    #[tokio::test]
    async fn store_outage_is_a_503_class_error() {
        let env = env();
        add_user(&env, "alice", UserSource::Local, Some("correct horse")).await;
        env.store.set_unavailable(true);
        let error = login(&env, "alice", "correct horse").await.unwrap_err();
        assert!(
            matches!(error, AuthError::ProviderUnavailable(_)),
            "{error:?}"
        );
    }

    #[tokio::test]
    async fn overloaded_gate_surfaces_as_overloaded_not_as_a_wrong_password() {
        use crate::passwords::{GateClass, LaneLimits};
        let store = MemStore::new();
        let gate = Arc::new(BlockingGate::new(GateConfig {
            password: LaneLimits {
                permits: 1,
                max_waiting: 0,
            },
            general: LaneLimits {
                permits: 1,
                max_waiting: 0,
            },
            permit_wait: std::time::Duration::from_millis(1),
        }));
        let weak = PasswordConfig {
            memory_kib: 8,
            iterations: 1,
            parallelism: 1,
        };
        let passwords = Arc::new(Passwords::new_for_tests(weak, Arc::clone(&gate)).unwrap());
        let mut new = fixtures::user("alice", UserSource::Local);
        new.password_hash = Some(passwords.hash(&Secret::new("correct horse")).await.unwrap());
        UserRepo::create(&store, &new).await.unwrap();
        let auth = LocalPasswordAuthenticator::new(Arc::new(store), passwords);
        let _held = gate.acquire(GateClass::Password).await.unwrap();
        let error = auth
            .authenticate(&basic("alice", "correct horse"), &AuthContext::default())
            .await
            .unwrap_err();
        assert!(matches!(error, AuthError::Overloaded), "{error:?}");
    }

    #[tokio::test]
    async fn debug_output_hides_everything() {
        let env = env();
        assert!(format!("{:?}", env.auth).contains("LocalPasswordAuthenticator"));
    }
}
