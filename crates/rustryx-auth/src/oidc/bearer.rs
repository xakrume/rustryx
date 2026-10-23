//! Bearer authentication of Keycloak access tokens and the group resolver that reads the
//! groups from the same token.
//!
//! Roles are not decided here. The principal carries the `groups` claim of this very
//! request in `external_groups`; the shared mapping group -> role lives elsewhere.

use std::sync::Arc;

use async_trait::async_trait;
use rustryx_core::AuthError;
use rustryx_core::identity::{
    AuthContext, AuthMethod, Credentials, IdpProvider, Principal, UserSource,
};
use rustryx_core::ports::{Authenticator, Clock, ExternalGroups, GroupResolver};
use time::OffsetDateTime;

use super::claims::OidcIdentity;
use super::validator::{TokenError, TokenValidator, VerifiedToken};

/// A verified access token: the identity plus the instant the token stops being valid.
///
/// [`Principal`] now carries the e-mail, the display name and the expiry itself, so the chain
/// no longer needs this type. It stays for a caller that wants the whole identity (issuer,
/// subject and groups together) without going through the chain.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedBearer {
    /// Identity from the token.
    pub identity: OidcIdentity,
    /// `exp` of the token. A cache keyed by the token must not outlive this.
    pub expires_at: OffsetDateTime,
}

/// Authenticates `Authorization: Bearer <Keycloak access token>`.
#[derive(Clone)]
pub struct OidcBearerAuthenticator {
    validator: Arc<TokenValidator>,
    clock: Arc<dyn Clock>,
}

impl std::fmt::Debug for OidcBearerAuthenticator {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OidcBearerAuthenticator")
            .finish_non_exhaustive()
    }
}

impl OidcBearerAuthenticator {
    pub(crate) fn new(validator: Arc<TokenValidator>, clock: Arc<dyn Clock>) -> Self {
        Self { validator, clock }
    }

    /// Fully validates an access token and returns its identity. A token that is not an OIDC
    /// access token at all is an error here, unlike in [`Authenticator::authenticate`].
    pub async fn validate(&self, token: &str) -> Result<VerifiedBearer, AuthError> {
        let VerifiedToken {
            identity,
            expires_at,
        } = self.validator.validate_access(token).await?;
        let expires_at = OffsetDateTime::from_unix_timestamp(expires_at)
            .map_err(|_| AuthError::InvalidToken(TokenError::Claims.reason()))?;
        Ok(VerifiedBearer {
            identity,
            expires_at,
        })
    }

    fn principal(&self, identity: OidcIdentity, expires_at: OffsetDateTime) -> Principal {
        let mut principal =
            Principal::new(identity.username, UserSource::Oidc, AuthMethod::OidcBearer);
        principal.issuer = Some(identity.issuer);
        principal.subject = Some(identity.subject);
        principal.external_groups = identity.groups;
        // The groups come from a token verified a moment ago, so they are confirmed now.
        principal.groups_confirmed_at = Some(self.clock.now());
        // Both are informational: JIT stores them on a new account, nothing decides on them.
        principal.email = identity.email;
        principal.display_name = identity.display_name;
        // The token's `exp` bounds how long a cache may keep this principal.
        principal.expires_at = Some(expires_at);
        principal
    }
}

#[async_trait]
impl Authenticator for OidcBearerAuthenticator {
    /// Cancel safe: the only await points are the JWKS cache (whose refresh runs detached)
    /// and nothing is written.
    async fn authenticate(
        &self,
        credentials: &Credentials,
        _context: &AuthContext,
    ) -> Result<Option<Principal>, AuthError> {
        let Credentials::Bearer(token) = credentials else {
            return Ok(None);
        };
        match self.validator.validate_access(token.expose()).await {
            Ok(verified) => {
                let expires_at = OffsetDateTime::from_unix_timestamp(verified.expires_at)
                    .map_err(|_| AuthError::InvalidToken(TokenError::Claims.reason()))?;
                Ok(Some(self.principal(verified.identity, expires_at)))
            }
            // Not a JWT of ours (API token, internal JWT, other issuer): let the chain go on.
            Err(TokenError::NotApplicable) => Ok(None),
            Err(error) => {
                tracing::debug!(reason = error.reason(), "oidc bearer token rejected");
                Err(error.into())
            }
        }
    }
}

/// Returns the groups of the current request, taken from the token claim in the principal.
#[derive(Debug, Clone, Copy, Default)]
pub struct OidcGroupResolver;

impl OidcGroupResolver {
    /// A resolver. It is stateless: the principal already carries what the token said.
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

#[async_trait]
impl GroupResolver for OidcGroupResolver {
    /// Cancel safe: no I/O.
    async fn external_groups(&self, principal: &Principal) -> Result<ExternalGroups, AuthError> {
        if principal.source != UserSource::Oidc {
            // Fail closed: answering "no groups" for someone else's principal would silently
            // strip their roles, answering with their groups would be wrong provider data.
            return Err(AuthError::GroupLookupFailed(
                "principal does not come from oidc".to_owned(),
            ));
        }
        // The groups are the ones of the token this request presented, so the confirmation
        // time is the one the authenticator stamped when it verified that token. A principal
        // without one was not built from a verified token, and its groups cannot be aged:
        // fail closed instead of stamping "now" on them.
        let Some(confirmed_at) = principal.groups_confirmed_at else {
            return Err(AuthError::GroupLookupFailed(
                "principal carries no group confirmation".to_owned(),
            ));
        };
        Ok(ExternalGroups::new(
            IdpProvider::Oidc,
            principal.external_groups.clone(),
            confirmed_at,
        ))
    }
}
