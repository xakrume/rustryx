//! API tokens: `ryx_` + 32 random bytes (base64url), stored only as SHA-256 hex.
//!
//! A token is a 256-bit CSPRNG secret, so a plain SHA-256 is enough to store it
//! (no salt or work factor can help against a secret that cannot be guessed) and
//! lets the lookup use an index. A database dump therefore holds nothing that
//! can be replayed, and the prefix lets secret scanners find leaked tokens.
//!
//! The authenticator claims `Bearer ryx_*` and `Basic <any user>:ryx_*`. The user
//! name of a Basic credential is ignored on purpose: clients such as `twine` send
//! a fixed `__token__` there, and the token alone identifies its owner.

#![allow(clippy::missing_errors_doc)] // errors are `AuthError` variants documented on the type

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use moka::future::Cache;
use rustryx_core::Secret;
use rustryx_core::error::AuthError;
use rustryx_core::identity::{
    API_TOKEN_PREFIX, ApiToken, AuthContext, AuthMethod, Credentials, Principal,
};
use rustryx_core::ids::{ApiTokenId, UserId};
use rustryx_core::ports::{Authenticator, Clock, TokenRepo, UserRepo};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;
use time::OffsetDateTime;

use crate::passwords::errmap::store_failure;

/// Entropy of the secret part, in bytes (256 bit, SEC-06).
pub const TOKEN_SECRET_BYTES: usize = 32;
/// Length of the base64url (no padding) encoding of [`TOKEN_SECRET_BYTES`].
const TOKEN_SECRET_CHARS: usize = 43;
/// How many characters of the token are kept for display (`ryx_` + 4).
const DISPLAY_PREFIX_CHARS: usize = API_TOKEN_PREFIX.len() + 4;
/// Default lifetime of a new token (SEC-06: at most 90 days by default).
pub const DEFAULT_TOKEN_TTL: Duration = Duration::from_hours(90 * 24);
/// Longest lifetime `create_token` accepts. An explicit, bounded ceiling keeps
/// "never expires" out of the system.
pub const MAX_TOKEN_TTL: Duration = Duration::from_hours(365 * 24);
/// Longest token name.
const MAX_NAME_CHARS: usize = 128;
/// Default spacing of `last_used_at` writes for one token on one node.
pub const DEFAULT_TOUCH_INTERVAL: Duration = Duration::from_secs(60);
/// Upper bound of tokens tracked for write coalescing; older entries are evicted.
const TOUCH_CACHE_CAPACITY: u64 = 100_000;

/// Generates a token secret: `ryx_` followed by 32 CSPRNG bytes, base64url without padding.
#[must_use]
pub fn generate_token_secret() -> Secret {
    let mut bytes = [0_u8; TOKEN_SECRET_BYTES];
    rand::fill(&mut bytes);
    Secret::new(format!(
        "{API_TOKEN_PREFIX}{}",
        URL_SAFE_NO_PAD.encode(bytes)
    ))
}

/// Lowercase hex SHA-256 of the full token string: the only form that is stored.
#[must_use]
pub fn hash_token(token: &str) -> String {
    hex::encode(Sha256::digest(token.as_bytes()))
}

/// True when `token` has exactly the shape `generate_token_secret` produces.
///
/// Checked before hashing so that garbage never costs a database round trip.
#[must_use]
pub fn is_well_formed_token(token: &str) -> bool {
    let Some(secret) = token.strip_prefix(API_TOKEN_PREFIX) else {
        return false;
    };
    secret.len() == TOKEN_SECRET_CHARS
        && URL_SAFE_NO_PAD
            .decode(secret)
            .is_ok_and(|bytes| bytes.len() == TOKEN_SECRET_BYTES)
}

/// What a new token is for.
#[derive(Debug, Clone)]
pub struct TokenSpec {
    /// Human readable label, 1 to 128 characters.
    pub name: String,
    /// Restriction of actions or repositories. Empty means the owner's rights.
    pub scopes: Vec<String>,
    /// Lifetime from now. Must be positive and at most [`MAX_TOKEN_TTL`].
    pub ttl: Duration,
}

impl TokenSpec {
    /// A spec with the default 90 day lifetime and no scope restriction.
    #[must_use]
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            scopes: Vec::new(),
            ttl: DEFAULT_TOKEN_TTL,
        }
    }
}

/// A freshly created token. The secret exists only here: it is shown once and
/// is not recoverable from the stored record.
#[derive(Debug)]
pub struct IssuedToken {
    /// The full token string to hand to the owner.
    pub secret: Secret,
    /// The stored record (hash and display prefix only).
    pub record: ApiToken,
}

/// Why a token could not be created.
#[derive(Debug, thiserror::Error)]
pub enum TokenCreateError {
    /// The spec is invalid (name or lifetime).
    #[error("invalid token specification: {0}")]
    InvalidSpec(&'static str),
    /// The store refused or failed.
    #[error(transparent)]
    Auth(#[from] AuthError),
}

/// Creates a token for `owner` and stores its hash. Returns the secret once.
pub async fn create_token(
    repo: &dyn TokenRepo,
    clock: &dyn Clock,
    owner: UserId,
    spec: TokenSpec,
) -> Result<IssuedToken, TokenCreateError> {
    let name = spec.name.trim();
    if name.is_empty() || name.chars().count() > MAX_NAME_CHARS {
        return Err(TokenCreateError::InvalidSpec(
            "name must have 1 to 128 characters",
        ));
    }
    if spec.ttl.is_zero() || spec.ttl > MAX_TOKEN_TTL {
        return Err(TokenCreateError::InvalidSpec(
            "ttl must be positive and at most 365 days",
        ));
    }
    let ttl = time::Duration::try_from(spec.ttl)
        .map_err(|_| TokenCreateError::InvalidSpec("ttl out of range"))?;
    let secret = generate_token_secret();
    let now = clock.now();
    let record = ApiToken {
        id: ApiTokenId::generate(),
        user_id: owner,
        name: name.to_owned(),
        token_hash: hash_token(secret.expose()),
        display_prefix: secret.expose().chars().take(DISPLAY_PREFIX_CHARS).collect(),
        scopes: spec.scopes,
        expires_at: Some(now.saturating_add(ttl)),
        revoked_at: None,
        last_used_at: None,
        created_at: now,
    };
    repo.create(&record)
        .await
        .map_err(|error| store_failure("token create", error))?;
    Ok(IssuedToken { secret, record })
}

/// Spaces `last_used_at` writes: one write per token per interval per node.
///
/// Why: a package manager sends hundreds of requests per minute with one token,
/// and a write per request would turn a read path into a write hot spot for a
/// column that only needs minute resolution.
struct TouchGate {
    seen: Cache<ApiTokenId, OffsetDateTime>,
    interval: time::Duration,
}

impl TouchGate {
    fn new(interval: Duration) -> Self {
        Self {
            seen: Cache::builder()
                .max_capacity(TOUCH_CACHE_CAPACITY)
                // Entries older than two intervals can never suppress a write, so
                // letting them expire only frees memory.
                .time_to_live(interval.saturating_mul(2).max(Duration::from_secs(1)))
                .build(),
            interval: time::Duration::try_from(interval).unwrap_or(time::Duration::MAX),
        }
    }

    /// True when the caller should write now. Claiming the slot before the write
    /// also folds concurrent requests of the same token into one write.
    async fn due(&self, id: ApiTokenId, now: OffsetDateTime) -> bool {
        let entry = self.seen.entry(id).or_insert(now).await;
        if entry.is_fresh() {
            return true;
        }
        if now - *entry.value() >= self.interval {
            self.seen.insert(id, now).await;
            return true;
        }
        false
    }
}

/// Authenticates `Bearer ryx_*` and `Basic user:ryx_*`.
pub struct TokenAuthenticator {
    tokens: Arc<dyn TokenRepo>,
    users: Arc<dyn UserRepo>,
    clock: Arc<dyn Clock>,
    touches: TouchGate,
}

impl std::fmt::Debug for TokenAuthenticator {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TokenAuthenticator").finish_non_exhaustive()
    }
}

impl TokenAuthenticator {
    /// Builds the authenticator with the default `last_used_at` write interval.
    #[must_use]
    pub fn new(
        tokens: Arc<dyn TokenRepo>,
        users: Arc<dyn UserRepo>,
        clock: Arc<dyn Clock>,
    ) -> Self {
        Self::with_touch_interval(tokens, users, clock, DEFAULT_TOUCH_INTERVAL)
    }

    /// Builds the authenticator with an explicit `last_used_at` write interval.
    #[must_use]
    pub fn with_touch_interval(
        tokens: Arc<dyn TokenRepo>,
        users: Arc<dyn UserRepo>,
        clock: Arc<dyn Clock>,
        touch_interval: Duration,
    ) -> Self {
        Self {
            tokens,
            users,
            clock,
            touches: TouchGate::new(touch_interval),
        }
    }

    /// The token string of a credential, and whether it arrived through Basic.
    fn candidate(credentials: &Credentials) -> Option<(&str, bool)> {
        match credentials {
            Credentials::Bearer(secret) if secret.expose().starts_with(API_TOKEN_PREFIX) => {
                Some((secret.expose(), false))
            }
            Credentials::Basic { password, .. }
                if password.expose().starts_with(API_TOKEN_PREFIX) =>
            {
                Some((password.expose(), true))
            }
            _ => None,
        }
    }

    fn reject(via_basic: bool, reason: &'static str) -> AuthError {
        if via_basic {
            AuthError::InvalidCredentials
        } else {
            AuthError::InvalidToken(reason)
        }
    }

    /// Finds the PERSONAL token record of `token`.
    ///
    /// Robot tokens and scan tokens share the `ryx_` shape and live in other tables
    /// (`robot_tokens`, `scan_access_tokens`), so a token cannot be classified by its text:
    /// the kind is whichever table knows the hash. Their authenticators come with the streams
    /// that own those tables and produce `AuthMethod::RobotToken` and `AuthMethod::ScanToken`
    /// principals; until then a hash that no table knows is refused here as "unknown API
    /// token", with the same answer for every unknown kind. Nothing falls back to a default
    /// identity, and the chain refuses a robot or scan principal that carries no scopes.
    async fn lookup(&self, token: &str, via_basic: bool) -> Result<ApiToken, AuthError> {
        if !is_well_formed_token(token) {
            return Err(Self::reject(via_basic, "malformed API token"));
        }
        let hash = hash_token(token);
        let record = self
            .tokens
            .find_by_hash(&hash)
            .await
            .map_err(|error| store_failure("token lookup", error))?
            .ok_or_else(|| Self::reject(via_basic, "unknown API token"))?;
        // The repository looked the row up by this hash, so the values are equal by
        // construction. The constant-time comparison is defense in depth against a
        // store that returns a near match (a collation or truncation bug), and it keeps
        // every comparison of a secret-derived value off the variable-time `==`.
        let equal: bool = record.token_hash.as_bytes().ct_eq(hash.as_bytes()).into();
        if !equal {
            return Err(Self::reject(via_basic, "unknown API token"));
        }
        Ok(record)
    }
}

#[async_trait]
impl Authenticator for TokenAuthenticator {
    async fn authenticate(
        &self,
        credentials: &Credentials,
        _context: &AuthContext,
    ) -> Result<Option<Principal>, AuthError> {
        let Some((token, via_basic)) = Self::candidate(credentials) else {
            return Ok(None);
        };
        let record = self.lookup(token, via_basic).await?;
        let now = self.clock.now();
        if record.revoked_at.is_some() {
            return Err(AuthError::TokenRevoked);
        }
        if record.expires_at.is_some_and(|expires| expires <= now) {
            return Err(AuthError::TokenExpired);
        }
        let owner = self
            .users
            .get(record.user_id)
            .await
            .map_err(|error| store_failure("token owner lookup", error))?
            .ok_or(AuthError::InvalidToken("token owner no longer exists"))?;
        if !owner.enabled {
            return Err(AuthError::AccountDisabled);
        }
        if self.touches.due(record.id, now).await {
            // A failed `last_used_at` write must not fail a valid request.
            if let Err(error) = self.tokens.touch_last_used(record.id, now).await {
                tracing::warn!(token_id = %record.id, %error, "could not record API token use");
            }
        }
        let mut principal = Principal::new(owner.username, owner.source, AuthMethod::ApiToken);
        principal.user_id = Some(owner.id);
        principal.issuer = owner.issuer;
        principal.subject = owner.subject;
        principal.token_scopes = Some(record.scopes);
        principal.token_id = Some(record.id);
        principal.email = owner.email;
        principal.display_name = owner.display_name;
        // The credential's own expiry bounds how long a cache may keep this principal.
        principal.expires_at = record.expires_at;
        Ok(Some(principal))
    }
}

#[cfg(test)]
mod tests;
