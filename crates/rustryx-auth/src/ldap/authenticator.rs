//! Bind authentication against LDAP / Active Directory.
//!
//! Sequence for `Credentials::Basic`:
//! 1. refuse an unusable login or password locally, with no network call
//!    (empty, blank and NUL passwords never reach a bind);
//! 2. search for the user on a pooled, service-bound connection (exactly one entry);
//! 3. bind as the DN found, on a NEW connection that is closed afterwards and never
//!    pooled;
//! 4. on success, look the groups up once so the following `GroupResolver` call is a
//!    cache hit.
//!
//! Steps 1 to 3 run under one deadline. Whatever goes wrong in them that is the user's
//! doing (no such user, ambiguous user, wrong password, unusable input) is the same
//! `AuthError::InvalidCredentials`, and the decoy bind in step 3 spends the same
//! network time, so neither the error nor the latency tells an attacker which accounts
//! exist.

use std::sync::Arc;

use async_trait::async_trait;
use rustryx_core::AuthError;
use rustryx_core::identity::{AuthContext, AuthMethod, Credentials, Principal, UserSource};
use rustryx_core::ports::Authenticator;
use tokio::time::{Instant, timeout_at};

use super::error::LdapError;
use super::input::{check_login, check_password};
use super::service::{Active, BindVerdict, LdapService};
use super::user::{DirectoryUser, UserLookup};

/// Why verification did not produce a user.
enum VerifyError {
    /// The user's doing: indistinguishable towards the client.
    Rejected,
    /// The directory failed.
    Directory(LdapError),
}

impl From<LdapError> for VerifyError {
    fn from(error: LdapError) -> Self {
        Self::Directory(error)
    }
}

/// `Authenticator` for LDAP bind logins.
#[derive(Debug)]
pub struct LdapAuthenticator {
    service: Arc<LdapService>,
}

impl LdapAuthenticator {
    /// Creates an authenticator on the shared service.
    #[must_use]
    pub const fn new(service: Arc<LdapService>) -> Self {
        Self { service }
    }

    /// True when LDAP login is configured. The authentication chain decides per user
    /// whether LDAP or the local store handles them, and skips this authenticator when
    /// it answers false (it would answer `Ok(None)` for everyone anyway).
    #[must_use]
    pub fn is_enabled(&self) -> bool {
        self.service.is_enabled()
    }
}

impl Active {
    async fn verify(&self, login: &str, password: &str) -> Result<DirectoryUser, VerifyError> {
        // The pooled connection is released at the end of this block, before the
        // (slow) user bind, so a login does not hold a pool slot while it waits.
        let lookup = {
            let mut conn = self.pool.checkout().await?;
            self.find_user(&mut conn, login).await?
        };
        match lookup {
            UserLookup::Found(user) => match self.bind_as_user(&user.dn, password).await? {
                BindVerdict::Accepted => Ok(user),
                BindVerdict::Rejected => Err(VerifyError::Rejected),
            },
            UserLookup::NotFound => {
                self.decoy_bind(password).await;
                Err(VerifyError::Rejected)
            }
            UserLookup::Ambiguous => {
                tracing::warn!("ldap user search matched more than one entry; login refused");
                self.decoy_bind(password).await;
                Err(VerifyError::Rejected)
            }
        }
    }
}

#[async_trait]
impl Authenticator for LdapAuthenticator {
    /// Cancel safe: a cancelled login leaves the pool consistent (an interrupted
    /// connection is discarded) and the user bind connection is dropped, which closes it.
    ///
    /// `Ok(None)` for anything but Basic credentials, and for everything when LDAP is
    /// disabled. The returned principal has no roles; `external_groups` and
    /// `groups_confirmed_at` are filled when the directory could be asked for groups,
    /// and left empty otherwise (the resolver then fails closed).
    async fn authenticate(
        &self,
        credentials: &Credentials,
        context: &AuthContext,
    ) -> Result<Option<Principal>, AuthError> {
        let Credentials::Basic { username, password } = credentials else {
            return Ok(None);
        };
        let Some(active) = self.service.active.as_ref() else {
            return Ok(None);
        };
        let request_id = context.request_id.as_deref().unwrap_or("-");

        let password = password.expose();
        if let Err(rejection) = check_login(username, active.settings.max_login_length)
            .and_then(|()| check_password(password))
        {
            tracing::debug!(
                request_id,
                reason = rejection.reason(),
                "ldap login refused before any network call"
            );
            return Err(AuthError::InvalidCredentials);
        }

        // One deadline for the whole call, group warm-up included: `authenticate` never
        // takes longer than `auth_deadline_secs`, whatever the directory does.
        let deadline = Instant::now() + active.settings.timeouts.auth_deadline;
        let verified = timeout_at(deadline, active.verify(username, password)).await;
        let user = match verified {
            Ok(Ok(user)) => user,
            Ok(Err(VerifyError::Rejected)) => {
                tracing::debug!(request_id, "ldap login rejected");
                return Err(AuthError::InvalidCredentials);
            }
            Ok(Err(VerifyError::Directory(error))) => {
                tracing::warn!(request_id, %error, "ldap authentication failed: directory error");
                return Err(error.into_provider_error());
            }
            Err(_) => {
                tracing::warn!(request_id, "ldap authentication exceeded its deadline");
                return Err(AuthError::ProviderUnavailable(
                    "ldap authentication deadline exceeded".to_owned(),
                ));
            }
        };

        // The credentials are valid at this point. If the groups cannot be had in the time
        // that is left, the login still succeeds with none: they are not claimed, and the
        // resolver (which fails closed) is asked again on the next step.
        let warm_up = timeout_at(
            deadline,
            active.groups_for_user(&user.username, Some(&user)),
        )
        .await;
        let (external_groups, groups_confirmed_at) = match warm_up {
            Ok(Ok(resolved)) => (resolved.groups, Some(resolved.confirmed_at)),
            Ok(Err(error)) => {
                tracing::warn!(request_id, %error, "ldap groups unavailable at login; the resolver fails closed");
                (Vec::new(), None)
            }
            Err(_) => {
                tracing::warn!(
                    request_id,
                    "ldap group lookup at login ran into the authentication deadline"
                );
                (Vec::new(), None)
            }
        };
        Ok(Some(Principal {
            user_id: None,
            username: user.username,
            source: UserSource::Ldap,
            issuer: None,
            subject: None,
            roles: Vec::new(),
            external_groups,
            groups_confirmed_at,
            token_scopes: None,
            token_id: None,
            auth_method: AuthMethod::BasicLdap,
        }))
    }
}
