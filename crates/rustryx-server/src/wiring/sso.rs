//! The adapters between the HTTP layer's session ports and `rustryx-auth`.
//!
//! `rustryx-api` knows two ports here: the token service ([`SessionIssuer`]) and the interactive
//! Keycloak login ([`OidcLoginPort`]). They live in the composition root because they are the
//! only code that needs both crates.
//!
//! # What is not here: refreshing an SSO session
//!
//! A browser session started through Keycloak carries a group snapshot that the authentication
//! chain refuses once it is five minutes old. Renewing it needs the Keycloak refresh token, and
//! `OidcLoginFlow::complete` returns only the identity: the refresh token is discarded inside
//! `rustryx-auth`, and the crate has no refresh grant. So no [`SsoSessionRefresher`] exists and
//! an SSO session ends five minutes after the login. The seam is in place (`OidcLogin::with_carrier`
//! for the sealed token, `SessionManager` taking the refresher): the fix belongs in
//! `rustryx-auth` and reaches this file as one more adapter, nothing else changes.
//!
//! [`SsoSessionRefresher`]: rustryx_api::SsoSessionRefresher

use std::sync::Arc;

use async_trait::async_trait;
use rustryx_api::{
    GroupSnapshot, IssuedSession, OidcCallback, OidcCompletion, OidcLogin, OidcLoginPort,
    OidcStart, SessionIssuer, SessionRequest,
};
use rustryx_auth::chain::AuthService;
use rustryx_auth::groups::JitProvisioner;
use rustryx_auth::internal_jwt::{
    GroupSnapshot as JwtGroups, InternalJwtService, IssueError, IssueRequest,
};
use rustryx_auth::oidc::{OidcLoginFlow, OidcProvider, SealedCookie};
use rustryx_core::Secret;
use rustryx_core::error::AuthError;
use rustryx_core::identity::{AuthMethod, Principal, UserSource};
use rustryx_core::ids::UserId;
use rustryx_core::ports::{Clock, InvalidationBus, InvalidationEvent};
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

/// Name of the cookie that carries `return_to` across the Keycloak round trip.
///
/// Not `__Host-` prefixed on purpose: like the flow cookie it is scoped to the callback path,
/// and the prefix requires `Path=/`. The sealing binds the value to this name, so it cannot be
/// replayed as another cookie.
pub const RETURN_COOKIE: &str = "rustryx_oidc_return";

/// Signs session tokens with the internal JWT service and ends them.
///
/// Ending a session is more than writing the revoked id. The chain caches a verified principal
/// for up to five minutes and does not look at the revocation list while it does, so a logout
/// that only wrote the id would leave the token working until the cache entry expired. The
/// adapter therefore also drops the user's cached principals on this node at once and announces
/// it on the invalidation bus for the others (`InvalidationEvent::User`, which the chain of every
/// node already listens for).
pub struct JwtSessionIssuer {
    jwt: Arc<InternalJwtService>,
    chain: Arc<AuthService>,
    bus: Arc<dyn InvalidationBus>,
}

impl JwtSessionIssuer {
    /// An issuer over the token service, the chain whose cache it must clear and the bus.
    #[must_use]
    pub fn new(
        jwt: Arc<InternalJwtService>,
        chain: Arc<AuthService>,
        bus: Arc<dyn InvalidationBus>,
    ) -> Self {
        Self { jwt, chain, bus }
    }
}

fn issue_failure(error: IssueError) -> AuthError {
    // The three variants are programming or configuration faults, not something the user did:
    // a lifetime out of range or too many groups is refused by the caller's own limits first.
    AuthError::Backend {
        context: "issue session token",
        source: Box::new(error),
    }
}

#[async_trait]
impl SessionIssuer for JwtSessionIssuer {
    fn issue(&self, request: &SessionRequest) -> Result<IssuedSession, AuthError> {
        let groups = request.groups.as_ref().map(|snapshot| JwtGroups {
            groups: snapshot.groups.clone(),
            confirmed_at: snapshot.confirmed_at,
        });
        let issued = self
            .jwt
            .issue(&IssueRequest {
                user_id: request.user_id,
                ttl: request.ttl,
                groups,
            })
            .map_err(issue_failure)?;
        Ok(IssuedSession::new(
            issued.token,
            issued.tid,
            issued.issued_at,
            issued.expires_at,
        ))
    }

    async fn revoke(
        &self,
        user_id: UserId,
        tid: &str,
        until: OffsetDateTime,
    ) -> Result<(), AuthError> {
        self.jwt.revoke(tid, until).await?;
        // This node first, so the logout holds here even if the bus is down.
        self.chain.invalidate_user(user_id);
        if let Err(error) = self
            .bus
            .publish(InvalidationEvent::User(user_id.as_uuid()))
            .await
        {
            // The revocation is stored; the other nodes still refuse the token once their cached
            // principal expires (five minutes at most), so the logout is not failed for this.
            tracing::warn!(%error, "could not announce the logout to the other nodes");
        }
        Ok(())
    }
}

/// What the `return_to` cookie holds.
#[derive(Serialize, Deserialize)]
struct ReturnTo {
    path: String,
}

/// The cookies of the login redirect and the account it produces.
pub struct KeycloakLogin {
    provider: Arc<OidcProvider>,
    flow: OidcLoginFlow,
    return_cookie: SealedCookie,
    jit: Arc<JitProvisioner>,
    clock: Arc<dyn Clock>,
}

impl KeycloakLogin {
    /// Builds the adapter. `cookie_key` is the key that seals the flow cookie (`auth.cookie_key`).
    ///
    /// # Errors
    /// The key is not a usable 32 byte key.
    pub fn new(
        provider: Arc<OidcProvider>,
        flow: OidcLoginFlow,
        cookie_key: &Secret,
        callback_path: &str,
        jit: Arc<JitProvisioner>,
        clock: Arc<dyn Clock>,
    ) -> anyhow::Result<Self> {
        let return_cookie = SealedCookie::new(
            cookie_key,
            RETURN_COOKIE,
            std::time::Duration::from_mins(10),
            Arc::clone(&clock),
        )
        .map_err(|error| anyhow::anyhow!("auth.cookie_key: {error}"))?
        .with_path(callback_path);
        Ok(Self {
            provider,
            flow,
            return_cookie,
            jit,
            clock,
        })
    }
}

/// The values of cookie `name` in a `Cookie` header text. Two or more is returned as is: the
/// caller refuses an ambiguous cookie, which is how a cookie planted by a sibling subdomain would
/// otherwise shadow the real one.
fn cookie_values<'a>(header: &'a str, name: &str) -> Vec<&'a str> {
    header
        .split(';')
        .filter_map(|pair| {
            let (key, value) = pair.trim().split_once('=')?;
            (key == name).then_some(value)
        })
        .collect()
}

fn single<'a>(header: &'a str, name: &str) -> Result<Option<&'a str>, AuthError> {
    match cookie_values(header, name).as_slice() {
        [] => Ok(None),
        [one] => Ok(Some(one)),
        _ => Err(AuthError::InvalidToken("ambiguous flow cookie")),
    }
}

/// The principal the account is provisioned from. The groups are the ones Keycloak just
/// confirmed, so they are stamped with the current time.
fn principal_of(
    identity: rustryx_auth::oidc::OidcIdentity,
    confirmed_at: OffsetDateTime,
) -> Principal {
    let mut principal = Principal::new(identity.username, UserSource::Oidc, AuthMethod::OidcBearer);
    principal.issuer = Some(identity.issuer);
    principal.subject = Some(identity.subject);
    principal.external_groups = identity.groups;
    principal.groups_confirmed_at = Some(confirmed_at);
    principal.email = identity.email;
    principal.display_name = identity.display_name;
    principal
}

#[async_trait]
impl OidcLoginPort for KeycloakLogin {
    fn available(&self) -> bool {
        self.provider.status().reachable
    }

    fn start(&self, return_to: &str) -> Result<OidcStart, AuthError> {
        let started = self.flow.start()?;
        let sealed = self
            .return_cookie
            .seal(&ReturnTo {
                path: return_to.to_owned(),
            })
            .map_err(|error| AuthError::Backend {
                context: "seal return_to cookie",
                source: Box::new(error),
            })?;
        Ok(OidcStart::new(
            started.authorization_url.to_string(),
            vec![
                started.set_cookie,
                self.return_cookie.set_cookie_header(&sealed),
            ],
        ))
    }

    async fn complete(&self, callback: &OidcCallback<'_>) -> Result<OidcCompletion, AuthError> {
        if callback.error.is_some() {
            return Err(AuthError::InvalidToken("the provider refused the login"));
        }
        let (Some(code), Some(state)) = (callback.code, callback.state) else {
            return Err(AuthError::InvalidToken("malformed callback"));
        };
        let Some(flow_cookie) = single(callback.cookie_header, self.flow.cookie_name())? else {
            return Err(AuthError::InvalidToken("flow cookie missing"));
        };
        // Missing or unreadable `return_to` only costs the user the page they came from: the
        // flow cookie is what authenticates the callback.
        let return_to = single(callback.cookie_header, RETURN_COOKIE)?
            .and_then(|value| self.return_cookie.open::<ReturnTo>(value).ok())
            .map_or_else(|| "/".to_owned(), |stored| stored.path);

        let identity = self.flow.complete(code, state, flow_cookie).await?;
        let confirmed_at = self.clock.now();
        let groups = identity.groups.clone();
        let user = self
            .jit
            .provision(&principal_of(identity, confirmed_at))
            .await?;
        Ok(OidcCompletion::new(
            OidcLogin::new(
                user.id,
                GroupSnapshot {
                    groups,
                    confirmed_at,
                },
            ),
            return_to,
            self.clear_cookies(),
        ))
    }

    fn clear_cookies(&self) -> Vec<String> {
        vec![
            self.flow.clear_cookie(),
            self.return_cookie.clear_cookie_header(),
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cookie_values_finds_exact_names_only() {
        let header = "a=1; rustryx_oidc_return=xyz; other_rustryx_oidc_return=no; b=2";
        assert_eq!(cookie_values(header, RETURN_COOKIE), ["xyz"]);
        assert!(cookie_values("", RETURN_COOKIE).is_empty());
        assert!(cookie_values("novalue", "novalue").is_empty());
    }

    #[test]
    fn must_not_accept_a_flow_cookie_that_appears_twice() {
        let header = "rustryx_oidc_flow=a; rustryx_oidc_flow=b";
        assert!(matches!(
            single(header, "rustryx_oidc_flow"),
            Err(AuthError::InvalidToken(_))
        ));
        assert_eq!(single("x=1", "rustryx_oidc_flow").unwrap(), None);
        assert_eq!(
            single("rustryx_oidc_flow=a", "rustryx_oidc_flow").unwrap(),
            Some("a")
        );
    }

    #[test]
    fn the_provisioned_principal_carries_the_confirmed_groups_and_identity() {
        let identity = rustryx_auth::oidc::OidcIdentity {
            issuer: "https://kc/realms/r".into(),
            subject: "sub-1".into(),
            username: "alice".into(),
            email: Some("alice@example.com".into()),
            display_name: Some("Alice".into()),
            groups: vec!["/team/a".into()],
        };
        let at = OffsetDateTime::UNIX_EPOCH;
        let principal = principal_of(identity, at);
        assert_eq!(principal.username, "alice");
        assert_eq!(principal.source, UserSource::Oidc);
        assert_eq!(principal.subject.as_deref(), Some("sub-1"));
        assert_eq!(principal.external_groups, ["/team/a"]);
        assert_eq!(principal.groups_confirmed_at, Some(at));
    }
}
