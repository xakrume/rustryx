//! Mapping of lower-level failures onto [`AuthError`].
//!
//! Lives next to the password code because that is the lowest module of the
//! crate that every other module of this part already depends on.

use rustryx_core::error::{AuthError, BoxedSource, StoreError};

/// Maps a metadata store failure to the auth error the HTTP layer can act on.
///
/// `StoreError::Unavailable` becomes [`AuthError::ProviderUnavailable`] because
/// that is the only variant that maps to 503: a database outage must not look
/// like a bug (500) or like bad credentials (401). Every other store error is a
/// `Backend` failure whose source stays in the log chain only.
pub(crate) fn store_failure(context: &'static str, error: StoreError) -> AuthError {
    match error {
        StoreError::Unavailable(detail) => {
            tracing::warn!(context, detail = %detail, "metadata store unavailable");
            AuthError::ProviderUnavailable(format!("metadata store unavailable during {context}"))
        }
        other => AuthError::Backend {
            context,
            source: Box::new(other),
        },
    }
}

/// Wraps an unexpected condition (a bug or a broken invariant) as a `Backend` error.
pub(crate) fn backend_failure(context: &'static str, detail: impl Into<BoxedSource>) -> AuthError {
    AuthError::Backend {
        context,
        source: detail.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unavailable_store_maps_to_provider_unavailable() {
        let mapped = store_failure("probe", StoreError::Unavailable("down".into()));
        assert!(matches!(mapped, AuthError::ProviderUnavailable(_)));
    }

    #[test]
    fn other_store_errors_map_to_backend_with_context() {
        let mapped = store_failure("probe", StoreError::Conflict("dup".into()));
        assert!(matches!(
            mapped,
            AuthError::Backend {
                context: "probe",
                ..
            }
        ));
    }

    #[test]
    fn backend_failure_keeps_context() {
        let mapped = backend_failure("ctx", "broken invariant");
        assert!(matches!(mapped, AuthError::Backend { context: "ctx", .. }));
    }
}
