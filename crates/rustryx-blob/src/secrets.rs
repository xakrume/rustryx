//! Where blob store secrets come from.
//!
//! A `BlobStoreDef` row holds only *references* to secrets (a name), never the
//! secrets themselves (CONTRIBUTING rule 5: secrets come from environment variables
//! or mounted files). The composition root passes a [`SecretResolver`] to the
//! registry, which resolves the names when it builds a store.

use std::collections::HashMap;

use async_trait::async_trait;
use rustryx_core::Secret;

/// A secret could not be produced for a reference.
///
/// The reference (a variable or file name) is safe to log; the secret never
/// appears in these messages.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SecretError {
    /// Nothing is configured under that reference.
    #[error("secret `{0}` is not set")]
    NotFound(String),
    /// The backing source (file system, secret manager) failed.
    #[error("secret `{reference}` could not be read: {reason}")]
    Unavailable {
        /// The reference that was being resolved.
        reference: String,
        /// Why, without secret material.
        reason: String,
    },
}

/// Resolves a secret reference to its value.
#[async_trait]
pub trait SecretResolver: Send + Sync + 'static {
    /// Returns the secret for `reference`. Cancel safe.
    async fn resolve(&self, reference: &str) -> Result<Secret, SecretError>;
}

/// Reads secrets from environment variables, with the `*_FILE` convention used for
/// Kubernetes Secret mounts and Vault Agent sinks (SRE-35).
///
/// `resolve("RUSTRYX_S3_KEY")` first looks at `RUSTRYX_S3_KEY_FILE` and reads that
/// file (one trailing newline is stripped), then at `RUSTRYX_S3_KEY`. The file wins
/// because a mounted secret rotates without restarting the process environment.
#[derive(Debug, Clone, Copy, Default)]
pub struct EnvSecretResolver;

#[async_trait]
impl SecretResolver for EnvSecretResolver {
    async fn resolve(&self, reference: &str) -> Result<Secret, SecretError> {
        resolve_from(|name| std::env::var(name).ok(), reference).await
    }
}

/// The resolution logic with the variable lookup injected, so tests can exercise the
/// file branch without mutating the process environment.
async fn resolve_from(
    lookup: impl Fn(&str) -> Option<String>,
    reference: &str,
) -> Result<Secret, SecretError> {
    if let Some(path) = lookup(&format!("{reference}_FILE")) {
        let content =
            tokio::fs::read_to_string(&path)
                .await
                .map_err(|err| SecretError::Unavailable {
                    reference: reference.to_owned(),
                    reason: err.kind().to_string(),
                })?;
        let value = content.strip_suffix('\n').unwrap_or(&content);
        return Ok(Secret::new(value.strip_suffix('\r').unwrap_or(value)));
    }
    lookup(reference)
        .map(Secret::new)
        .ok_or_else(|| SecretError::NotFound(reference.to_owned()))
}

/// A fixed in-memory table. For tests and for wiring that already holds the values.
#[derive(Debug, Clone, Default)]
pub struct MapSecretResolver {
    values: HashMap<String, String>,
}

impl MapSecretResolver {
    /// Builds a resolver from `(reference, value)` pairs.
    #[must_use]
    pub fn new<K, V>(pairs: impl IntoIterator<Item = (K, V)>) -> Self
    where
        K: Into<String>,
        V: Into<String>,
    {
        Self {
            values: pairs
                .into_iter()
                .map(|(k, v)| (k.into(), v.into()))
                .collect(),
        }
    }
}

#[async_trait]
impl SecretResolver for MapSecretResolver {
    async fn resolve(&self, reference: &str) -> Result<Secret, SecretError> {
        self.values
            .get(reference)
            .map(|value| Secret::new(value.clone()))
            .ok_or_else(|| SecretError::NotFound(reference.to_owned()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn map_resolver_returns_known_and_rejects_unknown() {
        let resolver = MapSecretResolver::new([("a", "1")]);
        assert_eq!(resolver.resolve("a").await.unwrap().expose(), "1");
        assert_eq!(
            resolver.resolve("b").await.unwrap_err(),
            SecretError::NotFound("b".into())
        );
    }

    #[tokio::test]
    async fn env_resolver_reports_unset_variables_by_name_only() {
        let err = EnvSecretResolver
            .resolve("RUSTRYX_TEST_SURELY_UNSET_7F3A")
            .await
            .unwrap_err();
        assert_eq!(
            err,
            SecretError::NotFound("RUSTRYX_TEST_SURELY_UNSET_7F3A".into())
        );
    }

    #[tokio::test]
    async fn env_resolver_reads_the_real_environment() {
        // PATH is set in every environment the tests run in; only the wiring of the
        // real lookup is checked here, the logic is covered through `resolve_from`.
        assert!(!EnvSecretResolver.resolve("PATH").await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn file_reference_wins_and_strips_one_trailing_newline() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("key");
        tokio::fs::write(&path, "from-file\n").await.unwrap();
        let path_text = path.to_str().unwrap().to_owned();
        let lookup = move |name: &str| match name {
            "KEY_FILE" => Some(path_text.clone()),
            "KEY" => Some("from-env".to_owned()),
            _ => None,
        };
        assert_eq!(
            resolve_from(lookup, "KEY").await.unwrap().expose(),
            "from-file"
        );
    }

    #[tokio::test]
    async fn plain_variable_is_used_without_a_file_reference() {
        let lookup = |name: &str| (name == "KEY").then(|| "from-env".to_owned());
        assert_eq!(
            resolve_from(lookup, "KEY").await.unwrap().expose(),
            "from-env"
        );
    }

    #[tokio::test]
    async fn unreadable_file_is_unavailable_and_names_only_the_reference() {
        let lookup = |name: &str| (name == "KEY_FILE").then(|| "/nonexistent/secret".to_owned());
        let err = resolve_from(lookup, "KEY").await.unwrap_err();
        let SecretError::Unavailable { reference, reason } = err else {
            unreachable!("expected Unavailable");
        };
        assert_eq!(reference, "KEY");
        assert!(!reason.contains("/nonexistent"));
    }

    #[tokio::test]
    async fn must_not_leak_the_secret_in_debug_output() {
        let secret = MapSecretResolver::new([("k", "s3cr3t-value")])
            .resolve("k")
            .await
            .unwrap();
        assert!(!format!("{secret:?}").contains("s3cr3t-value"));
    }
}
