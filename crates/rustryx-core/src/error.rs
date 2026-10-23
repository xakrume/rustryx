//! Error types shared by ports and their implementations.
//!
//! Messages here are for logs. The HTTP layer maps variants to stable public
//! codes and never copies `Display` text of `Backend` variants into a response.
//!
//! Every enum is `#[non_exhaustive]`: a variant added later is not a breaking change, so
//! consumers keep a wildcard arm that maps to the 500 class (an unknown failure is never an
//! allow and never a 404).

use std::time::Duration;

use crate::ids::{ImmutabilityRuleId, LifecycleHoldId};
use crate::model::WritePolicy;
use crate::quota::QuotaExceeded;

/// Boxed error source kept only for the log chain.
pub type BoxedSource = Box<dyn std::error::Error + Send + Sync + 'static>;

/// Failure of a metadata store (PostgreSQL) port.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum StoreError {
    /// The requested row does not exist.
    #[error("not found: {0}")]
    NotFound(String),
    /// A uniqueness or state constraint was violated.
    #[error("conflict: {0}")]
    Conflict(String),
    /// Malformed input such as a bad page token or an out of range number. Maps to 400, never to
    /// a conflict.
    #[error("invalid input: {0}")]
    Invalid(String),
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
#[non_exhaustive]
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
    /// Caller bug: the key could escape the store root. It must not read as `NotFound`.
    #[error("invalid blob key: {0}")]
    InvalidKey(&'static str),
    /// Local disk is full or below the free space floor. Maps to 507.
    #[error("insufficient storage")]
    InsufficientStorage,
    /// No upload slot or similar capacity. Maps to 503 with `Retry-After`.
    #[error("blob store overloaded")]
    Overloaded {
        /// How long the caller should wait, when the store can tell.
        retry_after: Option<Duration>,
    },
    /// The store cannot do this (list keys, presign, resumable upload).
    #[error("operation not supported by this blob store: {0}")]
    Unsupported(&'static str),
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
#[non_exhaustive]
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
    /// No mapped group and no default role, or the identity may not be provisioned. Maps to 403.
    #[error("access denied: {0}")]
    AccessDenied(&'static str),
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

/// What an upstream (proxy target, registry, scanner endpoint, webhook target) did wrong.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum UpstreamError {
    /// The upstream did not answer in time. Maps to 504.
    #[error("upstream timed out")]
    Timeout,
    /// The connection could not be established.
    #[error("upstream connection failed")]
    Connect,
    /// The TLS handshake or the certificate check failed.
    #[error("upstream tls failure")]
    Tls,
    /// The upstream answered with this status code.
    #[error("upstream answered {0}")]
    Status(u16),
    /// The response body exceeded its cap.
    #[error("upstream body too large")]
    TooLarge,
    /// The egress policy (SSRF filter) refused the target. The reason is a fixed label.
    #[error("target refused by egress policy: {0}")]
    Blocked(&'static str),
    /// The upstream broke the protocol.
    #[error("upstream protocol error")]
    Protocol,
    /// The circuit breaker of the upstream is open.
    #[error("upstream circuit open")]
    CircuitOpen,
}

impl UpstreamError {
    /// 504 for `Timeout`, 502 for everything else.
    #[must_use]
    pub const fn http_status(&self) -> u16 {
        match self {
            Self::Timeout => 504,
            _ => 502,
        }
    }
}

/// A policy said no. These are expected outcomes, not store failures.
///
/// The SQL guards raise SQLSTATE `RX004` (immutable), `RX005` (held), `RX006` (quota exceeded),
/// `RX007` (read only) and `RX008` (scope violation); `rustryx-db` maps them to this type for
/// the typed port methods and to `StoreError::Conflict` for the phase 1 methods.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum PolicyViolation {
    /// An immutability rule covers the item.
    #[error("item is immutable")]
    Immutable {
        /// The rule that covers it.
        rule: ImmutabilityRuleId,
    },
    /// A hold covers the item.
    #[error("item is under a hold")]
    Held {
        /// The hold that covers it.
        hold: LifecycleHoldId,
    },
    /// A quota would be exceeded by the write.
    #[error("quota exceeded")]
    QuotaExceeded(QuotaExceeded),
    /// `Deny`, or `AllowOnce` on an existing path.
    #[error("write policy forbids this write")]
    WritePolicy(WritePolicy),
    /// `PublishMode::CreateOnly` found the path taken.
    #[error("path already exists")]
    AlreadyExists,
    /// The system is in read only mode.
    #[error("read only mode")]
    ReadOnly,
    /// The target repository requires an approved promotion request.
    #[error("promotion approval required")]
    ApprovalRequired,
}

impl PolicyViolation {
    /// Stable label of the violation for metrics and logs (bounded cardinality).
    ///
    /// It is NOT the API error code: `rustryx-api` maps the variants to the codes of the API
    /// contract (api-conventions section 5.2), where for example read only mode is
    /// `read_only_mode` and an immutable tag is `immutable_tag`.
    #[must_use]
    pub const fn label(&self) -> &'static str {
        match self {
            Self::Immutable { .. } => "immutable",
            Self::Held { .. } => "held",
            Self::QuotaExceeded(_) => "quota_exceeded",
            Self::WritePolicy(_) => "write_policy",
            Self::AlreadyExists => "already_exists",
            Self::ReadOnly => "read_only",
            Self::ApprovalRequired => "approval_required",
        }
    }

    /// HTTP status class of the violation: 507 for a quota, 503 for read only mode (a retry
    /// after the mode ends can succeed), 409 for everything else.
    #[must_use]
    pub const fn http_status(&self) -> u16 {
        match self {
            Self::QuotaExceeded(_) => 507,
            Self::ReadOnly => 503,
            _ => 409,
        }
    }
}

/// Error of a port method that enforces policies inside its transaction.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum MutationError {
    /// A policy refused the write. Nothing was changed.
    #[error(transparent)]
    Violation(#[from] PolicyViolation),
    /// The store failed.
    #[error(transparent)]
    Store(#[from] StoreError),
}

impl MutationError {
    /// Flattens the error for callers of the phase 1 methods: a violation becomes
    /// `StoreError::Conflict` (as the SQL guards report it to an N-1 writer), a store error is
    /// returned unchanged.
    #[must_use]
    pub fn into_store_error(self) -> StoreError {
        match self {
            Self::Violation(violation) => StoreError::Conflict(violation.to_string()),
            Self::Store(error) => error,
        }
    }
}

/// Domain-level error returned by services.
///
/// The messages of `Invalid`, `Conflict` and `AlreadyExists` reach clients verbatim: a service
/// puts nothing in them that it would not print on a public page (no SQL text, no paths, no
/// hostnames, no secrets). `rustryx-api` has a lint test over every service message.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum DomainError {
    /// Entity does not exist (or is hidden from the caller).
    #[error("{entity} not found")]
    NotFound {
        /// Entity kind, for example `repository`.
        entity: &'static str,
    },
    /// Entity already exists. The message is public.
    #[error("{0}")]
    AlreadyExists(String),
    /// Request conflicts with current state (write policy, immutability). The message is public.
    #[error("{0}")]
    Conflict(String),
    /// Input failed validation. The message is public.
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
    /// An upstream failed (502, or 504 for a timeout).
    #[error(transparent)]
    Upstream(#[from] UpstreamError),
    /// A policy refused the operation. A `QuotaExceeded` inside keeps mapping to 507 and
    /// `ReadOnly` to 503 `read_only`, the rest to 409 (see [`PolicyViolation::http_status`]).
    #[error(transparent)]
    Violation(#[from] PolicyViolation),
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

impl From<MutationError> for DomainError {
    fn from(error: MutationError) -> Self {
        match error {
            MutationError::Violation(violation) => Self::Violation(violation),
            MutationError::Store(store) => Self::Store(store),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::error::Error as _;

    use super::*;
    use crate::ids::ProjectId;
    use crate::quota::QuotaScope;

    fn quota() -> QuotaExceeded {
        QuotaExceeded {
            scope: QuotaScope::Project(ProjectId::generate()),
            limit_bytes: 10,
            used_bytes: 9,
            requested_bytes: 5,
        }
    }

    fn all_violations() -> Vec<PolicyViolation> {
        vec![
            PolicyViolation::Immutable {
                rule: ImmutabilityRuleId::generate(),
            },
            PolicyViolation::Held {
                hold: LifecycleHoldId::generate(),
            },
            PolicyViolation::QuotaExceeded(quota()),
            PolicyViolation::WritePolicy(WritePolicy::AllowOnce),
            PolicyViolation::AlreadyExists,
            PolicyViolation::ReadOnly,
            PolicyViolation::ApprovalRequired,
        ]
    }

    /// Fails to compile when a variant is added without a decision on its label and status.
    fn expected(violation: &PolicyViolation) -> (&'static str, u16) {
        match violation {
            PolicyViolation::Immutable { .. } => ("immutable", 409),
            PolicyViolation::Held { .. } => ("held", 409),
            PolicyViolation::QuotaExceeded(_) => ("quota_exceeded", 507),
            PolicyViolation::WritePolicy(_) => ("write_policy", 409),
            PolicyViolation::AlreadyExists => ("already_exists", 409),
            PolicyViolation::ReadOnly => ("read_only", 503),
            PolicyViolation::ApprovalRequired => ("approval_required", 409),
        }
    }

    #[test]
    fn violation_labels_and_statuses_are_pinned() {
        let all = all_violations();
        assert_eq!(all.len(), 7, "a new variant needs a row here");
        for violation in &all {
            let (code, status) = expected(violation);
            assert_eq!(violation.label(), code);
            assert_eq!(violation.http_status(), status, "{violation}");
        }
    }

    #[test]
    fn upstream_timeout_is_504_and_everything_else_502() {
        assert_eq!(UpstreamError::Timeout.http_status(), 504);
        for other in [
            UpstreamError::Connect,
            UpstreamError::Tls,
            UpstreamError::Status(500),
            UpstreamError::TooLarge,
            UpstreamError::Blocked("loopback"),
            UpstreamError::Protocol,
            UpstreamError::CircuitOpen,
        ] {
            assert_eq!(other.http_status(), 502, "{other}");
        }
    }

    #[test]
    fn mutation_error_flattens_a_violation_into_a_conflict() {
        let flattened = MutationError::Violation(PolicyViolation::ReadOnly).into_store_error();
        assert!(matches!(&flattened, StoreError::Conflict(m) if m == "read only mode"));
        let kept = MutationError::Store(StoreError::Unavailable("down".into())).into_store_error();
        assert!(matches!(kept, StoreError::Unavailable(_)));
    }

    #[test]
    fn mutation_error_converts_into_the_domain_error_without_loss() {
        let violation: DomainError =
            MutationError::Violation(PolicyViolation::QuotaExceeded(quota())).into();
        assert!(matches!(
            violation,
            DomainError::Violation(PolicyViolation::QuotaExceeded(_))
        ));
        let store: DomainError = MutationError::Store(StoreError::NotFound("x".into())).into();
        assert!(matches!(store, DomainError::Store(StoreError::NotFound(_))));
    }

    #[test]
    fn question_mark_converts_every_source_into_the_domain_error() {
        fn upstream() -> Result<(), DomainError> {
            Err(UpstreamError::Timeout)?
        }
        fn violation() -> Result<(), DomainError> {
            Err(PolicyViolation::AlreadyExists)?
        }
        assert!(matches!(upstream(), Err(DomainError::Upstream(_))));
        assert!(matches!(violation(), Err(DomainError::Violation(_))));
    }

    #[test]
    fn invalid_page_tokens_have_their_own_variant() {
        let invalid = StoreError::Invalid("page token".into());
        assert_eq!(invalid.to_string(), "invalid input: page token");
        assert!(!matches!(invalid, StoreError::Conflict(_)));
    }

    #[test]
    fn blob_errors_display_without_internals() {
        assert_eq!(
            BlobError::InvalidKey("parent segment").to_string(),
            "invalid blob key: parent segment"
        );
        assert_eq!(
            BlobError::InsufficientStorage.to_string(),
            "insufficient storage"
        );
        let overloaded = BlobError::Overloaded {
            retry_after: Some(Duration::from_secs(3)),
        };
        assert_eq!(overloaded.to_string(), "blob store overloaded");
        assert_eq!(
            BlobError::Unsupported("list_keys").to_string(),
            "operation not supported by this blob store: list_keys"
        );
    }

    #[test]
    fn auth_access_denied_carries_a_fixed_reason() {
        let denied = AuthError::AccessDenied("no mapped group");
        assert_eq!(denied.to_string(), "access denied: no mapped group");
    }

    #[test]
    fn backend_errors_keep_the_source_for_the_log_chain_only() {
        let source: BoxedSource = "password=hunter2 refused".into();
        let error = StoreError::Backend {
            context: "insert",
            source,
        };
        assert_eq!(error.to_string(), "store backend failure during insert");
        assert!(error.source().is_some());
    }

    #[test]
    fn domain_invalid_shorthand_prefixes_the_message() {
        assert_eq!(
            DomainError::invalid("name too long").to_string(),
            "invalid input: name too long"
        );
    }
}
