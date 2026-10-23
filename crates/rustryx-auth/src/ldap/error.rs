//! Internal error of the LDAP layer and its mapping to `AuthError`.
//!
//! The internal error keeps full detail for logs. The mapping is where detail is
//! dropped: a caller of `Authenticator` or `GroupResolver` learns "unavailable" or
//! "invalid credentials", never a server diagnostic, a DN or a result code.

use std::fmt;

use rustryx_core::AuthError;

use super::transport::{TransportError, result_code};
use crate::telemetry::RequestResult;

/// A network operation that has its own timeout.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Operation {
    Connect,
    Bind,
    Search,
}

impl fmt::Display for Operation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Connect => "connect",
            Self::Bind => "bind",
            Self::Search => "search",
        })
    }
}

/// Failure of an LDAP step.
#[derive(Debug, thiserror::Error)]
pub(crate) enum LdapError {
    /// An operation did not finish within its timeout.
    #[error("ldap {0} timed out")]
    Timeout(Operation),
    /// The transport reported a failure.
    #[error(transparent)]
    Transport(#[from] TransportError),
    /// The service account could not bind: a deployment problem, not a user problem.
    #[error("service account bind failed: {0}")]
    ServiceBind(TransportError),
    /// The pool was shut down while a checkout was waiting.
    #[error("connection pool is closed")]
    PoolClosed,
    /// The circuit breaker is open: recent attempts to reach the directory failed.
    #[error("circuit breaker is open")]
    CircuitOpen,
}

impl LdapError {
    /// How this failure counts in `ldap_requests_total`. A server answer that only says "there
    /// is no such entry" is the directory working, and so is a refused password (that case
    /// never becomes an `LdapError`); a timeout and every transport failure are what
    /// `RustryxLdapErrors` counts.
    pub(crate) fn request_result(&self) -> RequestResult {
        match self {
            Self::Timeout(_) => RequestResult::Timeout,
            Self::Transport(TransportError::Server { code, .. })
                if *code == result_code::NO_SUCH_OBJECT =>
            {
                RequestResult::Ok
            }
            Self::Transport(_) | Self::ServiceBind(_) | Self::PoolClosed | Self::CircuitOpen => {
                RequestResult::Error
            }
        }
    }

    /// The error a caller of `Authenticator` is allowed to see.
    pub(crate) fn into_provider_error(self) -> AuthError {
        let message = match self {
            Self::Timeout(Operation::Connect) => "ldap connect timed out",
            Self::Timeout(Operation::Bind) => "ldap bind timed out",
            Self::Timeout(Operation::Search) => "ldap search timed out",
            Self::Transport(_) | Self::ServiceBind(_) | Self::PoolClosed | Self::CircuitOpen => {
                "ldap directory unavailable"
            }
        };
        AuthError::ProviderUnavailable(message.to_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_error_never_carries_server_text() {
        let secret_detail = "uid=alice,ou=people,dc=corp: 52e invalid credentials";
        for error in [
            LdapError::Transport(TransportError::Server {
                code: 49,
                message: secret_detail.into(),
            }),
            LdapError::ServiceBind(TransportError::Connect(secret_detail.into())),
            LdapError::PoolClosed,
            LdapError::CircuitOpen,
            LdapError::Timeout(Operation::Connect),
            LdapError::Timeout(Operation::Bind),
            LdapError::Timeout(Operation::Search),
        ] {
            let shown = error.into_provider_error().to_string();
            assert!(!shown.contains("alice"), "{shown}");
            assert!(!shown.contains("52e"), "{shown}");
            assert!(
                shown.starts_with("identity provider unavailable"),
                "{shown}"
            );
        }
    }

    #[test]
    fn timeouts_name_the_operation_in_logs() {
        assert_eq!(
            LdapError::Timeout(Operation::Bind).to_string(),
            "ldap bind timed out"
        );
        assert_eq!(Operation::Connect.to_string(), "connect");
        assert_eq!(Operation::Search.to_string(), "search");
    }
}
