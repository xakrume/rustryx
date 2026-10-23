//! Internal JWT: the session credential issued after a UI login.
//!
//! Format: `EdDSA` (`Ed25519`) JWT with a mandatory `kid` header and the claims
//! `iss`, `sub` (user id), `tid` (token id), `iat`, `exp`, plus an optional
//! snapshot of the external groups the login was confirmed with (`grp`, `gat`).
//!
//! What this module enforces on every verification:
//!
//! 1. the algorithm is `EdDSA`, nothing else (`none`, `HS*`, `RS*`, `ES*` are refused;
//!    the verification key is chosen by `kid` from the key ring, never taken from
//!    the token, so there is no key confusion with an HMAC secret);
//! 2. the signature is valid for the key named by `kid`;
//! 3. `iss` is the configured issuer;
//! 4. `exp` has not passed, and `exp - iat` does not exceed the 8 hour absolute
//!    session maximum, even for a correctly signed token;
//! 5. the account exists and is enabled;
//! 6. `iat >= users.tokens_valid_after` (revocation of all sessions of a user);
//! 7. `tid` is not on the revocation list (revocation of one session).
//!
//! The 30 minute idle limit and the 8 hour absolute limit of the UI session are
//! enforced by the caller that issues and refreshes tokens: this module honours
//! the `exp` it was asked to sign and refuses to sign a longer one.
//!
//! Database lookups (steps 5 to 7) happen only after the signature is verified,
//! so unauthenticated garbage never costs a query.

#![allow(clippy::missing_errors_doc)] // errors are `AuthError`/`IssueError` variants documented on the types

mod keys;
mod revocation;

use std::sync::Arc;
use std::time::Duration;

use arc_swap::ArcSwap;
use async_trait::async_trait;
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use jsonwebtoken::errors::ErrorKind;
use jsonwebtoken::{Algorithm, Header, Validation, decode, decode_header, encode};
use rustryx_core::Secret;
use rustryx_core::error::AuthError;
use rustryx_core::identity::{
    API_TOKEN_PREFIX, AuthContext, AuthMethod, Credentials, Principal, User,
};
use rustryx_core::ids::UserId;
use rustryx_core::ports::{Authenticator, Clock, UserRepo};
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use uuid::Uuid;

pub use self::keys::{KeyError, KeyRing, MAX_PREVIOUS_KEYS};
pub use self::revocation::{MemRevokedTokens, RevokedTokenStore};
use crate::passwords::errmap::store_failure;

/// Absolute session maximum (security spec: 8 hours). No token is signed or
/// accepted with a longer `exp - iat`.
pub const MAX_SESSION_TTL: Duration = Duration::from_hours(8);
/// Most groups one token may carry. A bigger set would not fit a header budget
/// anyway; the caller must fail the login instead of truncating the groups.
pub const MAX_GROUPS_IN_TOKEN: usize = 128;
/// Default `iss` value.
pub const DEFAULT_ISSUER: &str = "rustryx";
/// Default tolerance for an `iat` that is ahead of this node's clock.
pub const DEFAULT_CLOCK_SKEW: Duration = Duration::from_secs(5);
/// Longest token inspected at all. The HTTP layer caps header size far below this.
const MAX_TOKEN_BYTES: usize = 16 * 1024;
/// Longest `tid`.
const MAX_TID_CHARS: usize = 64;

/// Settings of the internal JWT.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InternalJwtConfig {
    /// Value of `iss`. Only tokens with exactly this issuer are claimed.
    pub issuer: String,
    /// How far ahead of this node's clock an `iat` may be. Needed because the node
    /// that verifies a token is often not the node that signed it. Not applied to `exp`.
    pub clock_skew: Duration,
}

impl Default for InternalJwtConfig {
    fn default() -> Self {
        Self {
            issuer: DEFAULT_ISSUER.to_owned(),
            clock_skew: DEFAULT_CLOCK_SKEW,
        }
    }
}

/// External groups an OIDC login was confirmed with, carried inside the session token.
///
/// The token is the only place that remembers them: there is no Keycloak access
/// token on a UI request, and persisted role rows alone grant nothing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GroupSnapshot {
    /// Full group paths, exactly as the provider sent them.
    pub groups: Vec<String>,
    /// When the provider last confirmed this membership.
    pub confirmed_at: OffsetDateTime,
}

/// What to sign.
#[derive(Debug, Clone)]
pub struct IssueRequest {
    /// The account the session belongs to.
    pub user_id: UserId,
    /// Lifetime of the token. At most [`MAX_SESSION_TTL`].
    pub ttl: Duration,
    /// Group snapshot for OIDC sessions, `None` for other sources.
    pub groups: Option<GroupSnapshot>,
}

/// A freshly signed token.
#[derive(Debug)]
pub struct IssuedJwt {
    /// The compact JWT.
    pub token: Secret,
    /// The token id, needed to revoke this one session.
    pub tid: String,
    /// `iat`.
    pub issued_at: OffsetDateTime,
    /// `exp`.
    pub expires_at: OffsetDateTime,
}

/// Why a token could not be issued.
#[derive(Debug, thiserror::Error)]
pub enum IssueError {
    /// `ttl` is zero or longer than [`MAX_SESSION_TTL`].
    #[error("token lifetime must be positive and at most 8 hours")]
    TtlOutOfRange,
    /// More than [`MAX_GROUPS_IN_TOKEN`] groups.
    #[error("too many groups for one session token")]
    TooManyGroups,
    /// The signer failed.
    #[error("token signing failed")]
    Signing,
}

/// A token that passed every check, with the account it names.
#[derive(Debug, Clone)]
pub struct VerifiedJwt {
    /// The account, as stored now.
    pub user: User,
    /// Token id.
    pub tid: String,
    /// `iat`.
    pub issued_at: OffsetDateTime,
    /// `exp`.
    pub expires_at: OffsetDateTime,
    /// Group snapshot, when the token carries one.
    pub groups: Option<GroupSnapshot>,
}

#[derive(Debug, Serialize, Deserialize)]
struct Claims {
    iss: String,
    sub: String,
    tid: String,
    iat: i64,
    exp: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    grp: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    gat: Option<i64>,
}

fn timestamp(seconds: i64) -> Result<OffsetDateTime, AuthError> {
    OffsetDateTime::from_unix_timestamp(seconds)
        .map_err(|_| AuthError::InvalidToken("timestamp out of range"))
}

fn whole_seconds(duration: Duration) -> i64 {
    let rounded_up = duration.as_secs() + u64::from(duration.subsec_nanos() > 0);
    i64::try_from(rounded_up).unwrap_or(i64::MAX)
}

/// Issues and verifies internal JWTs.
pub struct InternalJwtService {
    ring: ArcSwap<KeyRing>,
    config: InternalJwtConfig,
    clock: Arc<dyn Clock>,
    users: Arc<dyn UserRepo>,
    revoked: Arc<dyn RevokedTokenStore>,
}

impl std::fmt::Debug for InternalJwtService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("InternalJwtService")
            .field("issuer", &self.config.issuer)
            .field("ring", &**self.ring.load())
            .finish_non_exhaustive()
    }
}

impl InternalJwtService {
    /// Builds the service.
    #[must_use]
    pub fn new(
        ring: KeyRing,
        config: InternalJwtConfig,
        clock: Arc<dyn Clock>,
        users: Arc<dyn UserRepo>,
        revoked: Arc<dyn RevokedTokenStore>,
    ) -> Self {
        Self {
            ring: ArcSwap::from_pointee(ring),
            config,
            clock,
            users,
            revoked,
        }
    }

    /// Swaps in a new key ring (rotation without a restart). Tokens signed by a key
    /// that is no longer in the new ring stop verifying at once.
    pub fn replace_ring(&self, ring: KeyRing) {
        self.ring.store(Arc::new(ring));
    }

    /// The configured issuer.
    #[must_use]
    pub fn issuer(&self) -> &str {
        &self.config.issuer
    }

    /// Signs a session token. Synchronous: an Ed25519 signature takes tens of
    /// microseconds, far below the cost of moving it to the blocking pool.
    pub fn issue(&self, request: &IssueRequest) -> Result<IssuedJwt, IssueError> {
        if request.ttl.is_zero() || request.ttl > MAX_SESSION_TTL {
            return Err(IssueError::TtlOutOfRange);
        }
        if request
            .groups
            .as_ref()
            .is_some_and(|snapshot| snapshot.groups.len() > MAX_GROUPS_IN_TOKEN)
        {
            return Err(IssueError::TooManyGroups);
        }
        let now = self.clock.now();
        let iat = now.unix_timestamp();
        let exp = iat.saturating_add(whole_seconds(request.ttl));
        let tid = Uuid::now_v7().to_string();
        let claims = Claims {
            iss: self.config.issuer.clone(),
            sub: request.user_id.to_string(),
            tid: tid.clone(),
            iat,
            exp,
            grp: request.groups.as_ref().map(|s| s.groups.clone()),
            gat: request
                .groups
                .as_ref()
                .map(|s| s.confirmed_at.unix_timestamp()),
        };
        let ring = self.ring.load();
        let mut header = Header::new(Algorithm::EdDSA);
        header.kid = Some(ring.current_kid().to_owned());
        let token = encode(&header, &claims, ring.encoding_key()).map_err(|error| {
            tracing::error!(%error, "internal JWT signing failed");
            IssueError::Signing
        })?;
        Ok(IssuedJwt {
            token: Secret::new(token),
            tid,
            issued_at: timestamp(iat).map_err(|_| IssueError::Signing)?,
            expires_at: timestamp(exp).map_err(|_| IssueError::Signing)?,
        })
    }

    /// Ends one session: its `tid` is refused from now on, until `until` (its `exp`).
    pub async fn revoke(&self, tid: &str, until: OffsetDateTime) -> Result<(), AuthError> {
        self.revoked
            .revoke(tid, until)
            .await
            .map_err(|error| store_failure("jwt revoke", error))
    }

    /// Verifies a token against every rule in the module documentation.
    pub async fn verify(&self, token: &str) -> Result<VerifiedJwt, AuthError> {
        let claims = self.verify_signature(token)?;
        let now = self.clock.now();
        let facts = self.check_claims(&claims, now)?;
        let user = self
            .users
            .get(facts.user_id)
            .await
            .map_err(|error| store_failure("jwt user lookup", error))?
            .ok_or(AuthError::InvalidToken("unknown subject"))?;
        if !user.enabled {
            return Err(AuthError::AccountDisabled);
        }
        if user
            .tokens_valid_after
            .is_some_and(|valid_after| facts.issued_at < valid_after)
        {
            return Err(AuthError::TokenRevoked);
        }
        let revoked = self
            .revoked
            .is_revoked(&claims.tid)
            .await
            .map_err(|error| store_failure("jwt revocation lookup", error))?;
        if revoked {
            return Err(AuthError::TokenRevoked);
        }
        Ok(VerifiedJwt {
            user,
            tid: claims.tid,
            issued_at: facts.issued_at,
            expires_at: facts.expires_at,
            groups: facts.groups,
        })
    }

    /// Signature, algorithm and `kid` checks. Returns the claims of a token that
    /// was signed by one of our keys; nothing else about it is trusted yet.
    fn verify_signature(&self, token: &str) -> Result<Claims, AuthError> {
        if token.len() > MAX_TOKEN_BYTES {
            return Err(AuthError::InvalidToken("token too large"));
        }
        let header =
            decode_header(token).map_err(|_| AuthError::InvalidToken("malformed header"))?;
        if header.alg != Algorithm::EdDSA {
            return Err(AuthError::InvalidToken("algorithm not allowed"));
        }
        let Some(kid) = header.kid.as_deref() else {
            return Err(AuthError::InvalidToken("kid missing"));
        };
        let ring = self.ring.load();
        let Some(key) = ring.decoding_key(kid) else {
            return Err(AuthError::InvalidToken("unknown kid"));
        };
        // Time and issuer are checked by hand in `check_claims`: the library reads
        // the system clock, which would make expiry untestable and ignore `Clock`.
        let mut validation = Validation::new(Algorithm::EdDSA);
        validation.required_spec_claims.clear();
        validation.validate_exp = false;
        validation.validate_nbf = false;
        validation.validate_aud = false;
        validation.leeway = 0;
        decode::<Claims>(token, key, &validation)
            .map(|data| data.claims)
            .map_err(|error| match error.kind() {
                ErrorKind::InvalidSignature => AuthError::InvalidToken("bad signature"),
                ErrorKind::InvalidAlgorithm => AuthError::InvalidToken("algorithm not allowed"),
                _ => AuthError::InvalidToken("invalid claims"),
            })
    }

    fn check_claims(&self, claims: &Claims, now: OffsetDateTime) -> Result<ClaimFacts, AuthError> {
        if claims.iss != self.config.issuer {
            return Err(AuthError::InvalidToken("wrong issuer"));
        }
        let issued_at = timestamp(claims.iat)?;
        let expires_at = timestamp(claims.exp)?;
        if expires_at <= issued_at {
            return Err(AuthError::InvalidToken("exp is not after iat"));
        }
        let lifetime = expires_at - issued_at;
        let max = time::Duration::try_from(MAX_SESSION_TTL).unwrap_or(time::Duration::MAX);
        if lifetime > max {
            return Err(AuthError::InvalidToken(
                "lifetime exceeds the session maximum",
            ));
        }
        let skew = time::Duration::try_from(self.config.clock_skew).unwrap_or(time::Duration::ZERO);
        if issued_at > now.saturating_add(skew) {
            return Err(AuthError::InvalidToken("iat is in the future"));
        }
        // Strict: `exp` is the end of the session, a verifier must not extend it.
        if now >= expires_at {
            return Err(AuthError::TokenExpired);
        }
        let user_id = claims
            .sub
            .parse::<UserId>()
            .map_err(|_| AuthError::InvalidToken("malformed subject"))?;
        if claims.tid.is_empty() || claims.tid.chars().count() > MAX_TID_CHARS {
            return Err(AuthError::InvalidToken("malformed tid"));
        }
        let groups = match (&claims.grp, claims.gat) {
            (None, None) => None,
            (Some(groups), Some(confirmed)) => {
                if groups.len() > MAX_GROUPS_IN_TOKEN {
                    return Err(AuthError::InvalidToken("too many groups"));
                }
                let confirmed_at = timestamp(confirmed)?;
                if confirmed_at > now.saturating_add(skew) {
                    return Err(AuthError::InvalidToken("group snapshot is from the future"));
                }
                Some(GroupSnapshot {
                    groups: groups.clone(),
                    confirmed_at,
                })
            }
            // Groups without a confirmation time (or the reverse) cannot be aged,
            // and an unaged group set must not grant anything.
            _ => return Err(AuthError::InvalidToken("incomplete group snapshot")),
        };
        Ok(ClaimFacts {
            user_id,
            issued_at,
            expires_at,
            groups,
        })
    }
}

struct ClaimFacts {
    user_id: UserId,
    issued_at: OffsetDateTime,
    expires_at: OffsetDateTime,
    groups: Option<GroupSnapshot>,
}

/// What `peek` learns from a token without trusting it.
struct Peek {
    alg: String,
    iss: Option<String>,
}

/// Reads `alg` and `iss` without verifying anything, only to decide which
/// authenticator a bearer token belongs to. `None` for anything that is not a
/// three segment JWT. The result is never used to grant access.
fn peek(token: &str) -> Option<Peek> {
    if token.len() > MAX_TOKEN_BYTES {
        return None;
    }
    let mut parts = token.split('.');
    let (header, payload, _signature) = (parts.next()?, parts.next()?, parts.next()?);
    if parts.next().is_some() {
        return None;
    }
    let header = decode_json(header)?;
    let payload = decode_json(payload)?;
    Some(Peek {
        alg: header.get("alg")?.as_str()?.to_owned(),
        iss: payload
            .get("iss")
            .and_then(|v| v.as_str())
            .map(str::to_owned),
    })
}

fn decode_json(segment: &str) -> Option<serde_json::Value> {
    let bytes = URL_SAFE_NO_PAD.decode(segment).ok()?;
    serde_json::from_slice(&bytes).ok()
}

/// `exp` of a JWT that something else already verified, for sizing a cache entry.
pub(crate) fn unverified_expiry(token: &str) -> Option<OffsetDateTime> {
    let payload = decode_json(token.split('.').nth(1)?)?;
    let seconds = payload.get("exp")?.as_i64()?;
    OffsetDateTime::from_unix_timestamp(seconds).ok()
}

/// `Authenticator` for `Bearer` internal JWTs.
#[derive(Debug, Clone)]
pub struct InternalJwtAuthenticator {
    service: Arc<InternalJwtService>,
}

impl InternalJwtAuthenticator {
    /// Wraps the service.
    #[must_use]
    pub const fn new(service: Arc<InternalJwtService>) -> Self {
        Self { service }
    }
}

#[async_trait]
impl Authenticator for InternalJwtAuthenticator {
    async fn authenticate(
        &self,
        credentials: &Credentials,
        _context: &AuthContext,
    ) -> Result<Option<Principal>, AuthError> {
        let Credentials::Bearer(secret) = credentials else {
            return Ok(None);
        };
        let token = secret.expose();
        if token.starts_with(API_TOKEN_PREFIX) {
            return Ok(None);
        }
        let Some(peeked) = peek(token) else {
            return Ok(None);
        };
        let claims_ours = peeked.iss.as_deref() == Some(self.service.issuer());
        match (peeked.alg == "EdDSA", claims_ours) {
            (true, true) => {}
            // A token that names our issuer but not our algorithm is a forgery attempt
            // (`alg: none`, or HS256 keyed with our public key). Saying "not applicable"
            // would be safe because the next authenticator refuses it too, but a
            // definite refusal does not depend on that.
            (false, true) => return Err(AuthError::InvalidToken("algorithm not allowed")),
            (_, false) => return Ok(None),
        }
        let verified = self.service.verify(token).await?;
        let user = verified.user;
        let (external_groups, groups_confirmed_at) = match verified.groups {
            Some(snapshot) => (snapshot.groups, Some(snapshot.confirmed_at)),
            None => (Vec::new(), None),
        };
        Ok(Some(Principal {
            user_id: Some(user.id),
            username: user.username,
            source: user.source,
            issuer: user.issuer,
            subject: user.subject,
            roles: Vec::new(),
            external_groups,
            groups_confirmed_at,
            token_scopes: None,
            token_id: None,
            auth_method: AuthMethod::InternalJwt,
        }))
    }
}

#[cfg(test)]
mod tests;
