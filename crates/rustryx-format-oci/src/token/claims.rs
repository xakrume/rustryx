//! The claims of a registry token and the traits that sign and verify them.
//!
//! The service decides WHAT a token says ([`RegistryTokenClaims`]); how it is signed is the
//! server's: it implements [`TokenIssuer`] and [`TokenVerifier`] over its internal JWT key
//! ring. [`crate::token::HmacTokenCodec`] is a complete implementation for tests and for
//! deployments without a key ring.

use time::OffsetDateTime;

use rustryx_core::ids::UserId;

use crate::parse::scope::Action;

/// What a grant names.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum GrantResource {
    /// An image, by its docker name `<rustryx-repo>/<image path>`.
    Repository(String),
    /// The registry catalog.
    Catalog,
}

/// One entry of the `access` claim.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccessGrant {
    /// The resource.
    pub resource: GrantResource,
    /// The actions granted on it, possibly none: a token that says "nothing" is a valid
    /// answer to a request for something the caller may not have.
    pub actions: Vec<Action>,
}

/// The claims of a registry bearer token.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegistryTokenClaims {
    /// Who issued it (the service name).
    pub issuer: String,
    /// The account name; `anonymous` for an anonymous caller.
    pub subject: String,
    /// The account id, when the caller has one.
    pub subject_id: Option<UserId>,
    /// The service it is meant for.
    pub audience: String,
    /// When it was issued.
    pub issued_at: OffsetDateTime,
    /// Not valid before.
    pub not_before: OffsetDateTime,
    /// Not valid from.
    pub expires_at: OffsetDateTime,
    /// A unique id, for logs and revocation.
    pub token_id: String,
    /// What the holder may do. It can only be narrower than what RBAC allows at request
    /// time, never wider: RBAC is asked again for every request.
    pub access: Vec<AccessGrant>,
}

impl RegistryTokenClaims {
    /// True when the token carries `action` on the docker name `name`
    /// (`<rustryx-repo>/<image path>`). The caller applies this on top of RBAC to refuse a
    /// request the token was not issued for, with a challenge that asks for the right scope.
    #[must_use]
    pub fn permits(&self, name: &str, action: Action) -> bool {
        self.access.iter().any(|grant| match &grant.resource {
            GrantResource::Repository(granted) => {
                granted == name && grant.actions.contains(&action)
            }
            GrantResource::Catalog => false,
        })
    }

    /// True when the token carries the catalog grant.
    #[must_use]
    pub fn permits_catalog(&self) -> bool {
        self.access
            .iter()
            .any(|grant| grant.resource == GrantResource::Catalog && !grant.actions.is_empty())
    }
}

/// Why a token was not issued or not accepted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum TokenError {
    /// Not three base64url parts, or the parts do not decode to the expected shape.
    #[error("malformed token")]
    Malformed,
    /// The header names an algorithm other than the one this verifier uses (`none`
    /// included).
    #[error("unsupported token algorithm")]
    UnsupportedAlgorithm,
    /// The signature does not match.
    #[error("bad token signature")]
    BadSignature,
    /// `exp` has passed.
    #[error("token expired")]
    Expired,
    /// `nbf` or `iat` lies in the future.
    #[error("token not valid yet")]
    NotYetValid,
    /// Issued for another service.
    #[error("token audience mismatch")]
    WrongAudience,
    /// Issued by another issuer.
    #[error("token issuer mismatch")]
    WrongIssuer,
    /// The signing key is too short or missing.
    #[error("signing key is not usable")]
    KeyUnusable,
    /// The token could not be produced.
    #[error("token could not be issued")]
    IssueFailed,
}

/// Signs the claims of a token.
pub trait TokenIssuer: Send + Sync {
    /// Produces the compact token text. Must not fail for valid claims; must not embed
    /// anything but the claims.
    ///
    /// # Errors
    /// [`TokenError::IssueFailed`] or [`TokenError::KeyUnusable`].
    fn issue(&self, claims: &RegistryTokenClaims) -> Result<String, TokenError>;
}

/// Verifies a token and returns its claims.
pub trait TokenVerifier: Send + Sync {
    /// Checks signature, algorithm, audience, issuer and the time window against `now`.
    ///
    /// # Errors
    /// The matching [`TokenError`]. An implementation must not return claims for a token it
    /// did not fully verify.
    fn verify(&self, token: &str, now: OffsetDateTime) -> Result<RegistryTokenClaims, TokenError>;
}
