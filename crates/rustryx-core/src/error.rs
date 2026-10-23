//! Error types shared by ports and their implementations.
//!
//! Messages here are for logs. The HTTP layer maps variants to stable public
//! codes and never copies `Display` text of `Backend` variants into a response.

use std::time::Duration;

/// Boxed error source kept only for the log chain.
pub type BoxedSource = Box<dyn std::error::Error + Send + Sync + 'static>;

/// Failure of a metadata store (PostgreSQL) port.
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    /// The requested row does not exist.
    #[error("not found: {0}")]
    NotFound(String),
    /// A uniqueness or state constraint was violated.
    #[error("conflict: {0}")]
    Conflict(String),
    /// The store is unreachable or timed out.
    #[error("store unavailable: {0}")]
    Unavailable(String),
    /// Any other backend failure.
    #[error("store backend failure during {context}")]
    Backend {
        /// What the store was doing.
        context: &'static str,
        /// Underlying error, for logs only.
        #[source]
        source: BoxedSource,
    },
}

/// Failure of a blob store port.
#[derive(Debug, thiserror::Error)]
pub enum BlobError {
    /// No object under the key.
    #[error("blob not found")]
    NotFound,
    /// The backend is unreachable or timed out.
    #[error("blob store unavailable: {0}")]
    Unavailable(String),
    /// A requested byte range lies outside the object.
    #[error("range not satisfiable")]
    RangeNotSatisfiable,
    /// The writer was used after commit or abort.
    #[error("blob writer is closed")]
    WriterClosed,
    /// Any other backend failure.
    #[error("blob backend failure during {context}")]
    Backend {
        /// What the store was doing.
        context: &'static str,
        /// Underlying error, for logs only.
        #[source]
        source: BoxedSource,
    },
}

/// Failure while authenticating or resolving groups.
///
/// `InvalidCredentials` deliberately covers "no such user" and "wrong password"
/// so that the response cannot be used to enumerate accounts.
#[derive(Debug, thiserror::Error)]
pub enum AuthError {
    /// Unknown user or wrong secret. Never more specific towards the client.
    #[error("invalid credentials")]
    InvalidCredentials,
    /// Token is malformed, badly signed, wrong issuer or audience. Reason is for logs.
    #[error("invalid token: {0}")]
    InvalidToken(&'static str),
    /// Token is past its `exp`.
    #[error("token expired")]
    TokenExpired,
    /// Token or API token was revoked.
    #[error("token revoked")]
    TokenRevoked,
    /// The account is disabled.
    #[error("account disabled")]
    AccountDisabled,
    /// Too many failed logins; try again after the delay.
    #[error("locked out for {retry_after:?}")]
    LockedOut {
        /// Remaining lockout time.
        retry_after: Duration,
    },
    /// Same username exists in another identity source. No automatic merge.
    #[error("username already used by another identity source")]
    UsernameConflict,
    /// Group lookup failed. Callers must fail closed, not treat it as "no groups".
    #[error("group lookup failed: {0}")]
    GroupLookupFailed(String),
    /// Identity provider (Keycloak, LDAP) is unavailable.
    #[error("identity provider unavailable: {0}")]
    ProviderUnavailable(String),
    /// Required blocking-work permit was not available before the deadline (maps to 503).
    #[error("authentication capacity exhausted")]
    Overloaded,
    /// Login-attempt storage is down; local and LDAP login fail closed.
    #[error("login attempt store unavailable")]
    GuardUnavailable,
    /// Any other failure.
    #[error("auth backend failure during {context}")]
    Backend {
        /// What was being done.
        context: &'static str,
        /// Underlying error, for logs only.
        #[source]
        source: BoxedSource,
    },
}

/// Domain-level error returned by services.
#[derive(Debug, thiserror::Error)]
pub enum DomainError {
    /// Entity does not exist (or is hidden from the caller).
    #[error("{entity} not found")]
    NotFound {
        /// Entity kind, for example `repository`.
        entity: &'static str,
    },
    /// Entity already exists.
    #[error("{0}")]
    AlreadyExists(String),
    /// Request conflicts with current state (write policy, immutability).
    #[error("{0}")]
    Conflict(String),
    /// Input failed validation.
    #[error("invalid input: {0}")]
    Invalid(String),
    /// Authentication is required or failed.
    #[error("authentication required")]
    Unauthenticated,
    /// Authenticated but not allowed.
    #[error("forbidden")]
    Forbidden,
    /// Repository or blob store quota would be exceeded.
    #[error("quota exceeded")]
    QuotaExceeded,
    /// A dependency is down or overloaded (maps to 503).
    #[error("dependency unavailable: {0}")]
    Unavailable(String),
    /// Metadata store failure.
    #[error(transparent)]
    Store(#[from] StoreError),
    /// Blob store failure.
    #[error(transparent)]
    Blob(#[from] BlobError),
    /// Authentication failure.
    #[error(transparent)]
    Auth(#[from] AuthError),
    /// Bug or unexpected state. Message is for logs only.
    #[error("internal error: {0}")]
    Internal(String),
}

impl DomainError {
    /// Shorthand for an invalid-input error.
    #[must_use]
    pub fn invalid(message: impl Into<String>) -> Self {
        Self::Invalid(message.into())
    }
}
