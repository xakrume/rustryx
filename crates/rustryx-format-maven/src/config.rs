//! Settings: per repository (`format_config`) and per handler.

use std::time::Duration;

use rustryx_core::model::Repository;
use rustryx_format_core::FormatError;

use crate::version::is_snapshot;

/// Which versions a repository takes (hosted) or serves (proxy).
///
/// It is the `version_policy` key of `format_config`: `"release"`, `"snapshot"` or
/// `"mixed"` (the default). A hosted repository refuses a write of the other kind with a
/// 400, a proxy answers 404 for it without asking the upstream.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum VersionPolicy {
    /// Only release versions.
    Release,
    /// Only snapshot versions.
    Snapshot,
    /// Both.
    #[default]
    Mixed,
}

impl VersionPolicy {
    /// Reads the policy from a repository. A missing key (or no `format_config` object) is
    /// `Mixed`; an unknown value is a configuration fault and fails closed instead of being
    /// guessed, because a typo must not turn a release repository into a mixed one.
    ///
    /// # Errors
    /// [`FormatError::Internal`] when the value is not one of the three words.
    pub fn of(repository: &Repository) -> Result<Self, FormatError> {
        let value = match &repository.format_config {
            serde_json::Value::Null => return Ok(Self::Mixed),
            serde_json::Value::Object(map) => map.get("version_policy"),
            _ => return Err(FormatError::internal("format_config is not an object")),
        };
        match value {
            None | Some(serde_json::Value::Null) => Ok(Self::Mixed),
            Some(serde_json::Value::String(word)) => match word.to_ascii_lowercase().as_str() {
                "release" => Ok(Self::Release),
                "snapshot" => Ok(Self::Snapshot),
                "mixed" => Ok(Self::Mixed),
                _ => Err(FormatError::internal(
                    "unknown version_policy in format_config",
                )),
            },
            Some(_) => Err(FormatError::internal("version_policy is not a string")),
        }
    }

    /// True when a version directory name is allowed.
    #[must_use]
    pub fn allows(self, version: &str) -> bool {
        match self {
            Self::Release => !is_snapshot(version),
            Self::Snapshot => is_snapshot(version),
            Self::Mixed => true,
        }
    }

    /// The client text of a refusal.
    #[must_use]
    pub const fn refusal(self) -> &'static str {
        match self {
            Self::Release => "this repository accepts only release versions",
            Self::Snapshot => "this repository accepts only snapshot versions",
            Self::Mixed => "version not accepted",
        }
    }
}

/// Settings of the handler (not of a repository).
#[derive(Debug, Clone)]
pub struct MavenConfig {
    /// Largest `maven-metadata.xml` accepted in an upload (SEC-33: 10 MiB). Documents that
    /// come from an upstream are capped by `Limits::max_document_bytes`, and a group merge
    /// by `Limits::group_merge_cap_bytes`.
    pub max_metadata_bytes: u64,
    /// Permits for parsing metadata on the blocking pool. The permit is taken before the
    /// spawn, so a flood of uploads cannot queue unbounded work.
    pub blocking_permits: usize,
    /// How long to wait for a permit before answering 503 with `Retry-After`.
    pub permit_wait: Duration,
}

impl Default for MavenConfig {
    fn default() -> Self {
        Self {
            max_metadata_bytes: 10 * 1024 * 1024,
            blocking_permits: 8,
            permit_wait: Duration::from_secs(5),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustryx_core::ids::{BlobStoreId, RepositoryId};
    use rustryx_core::model::{RepoFormat, RepoType};

    fn repository(config: serde_json::Value) -> Repository {
        let mut repository = Repository::new(
            RepositoryId::generate(),
            rustryx_core::names::RepoName::parse("m").unwrap(),
            RepoFormat::Maven2,
            RepoType::Hosted,
            BlobStoreId::generate(),
            time::OffsetDateTime::UNIX_EPOCH,
        );
        repository.format_config = config;
        repository
    }

    #[test]
    fn reads_the_three_policies_case_insensitively() {
        for (word, policy) in [
            ("release", VersionPolicy::Release),
            ("SNAPSHOT", VersionPolicy::Snapshot),
            ("Mixed", VersionPolicy::Mixed),
        ] {
            let repository = repository(serde_json::json!({ "version_policy": word }));
            assert_eq!(VersionPolicy::of(&repository).unwrap(), policy);
        }
    }

    #[test]
    fn missing_policy_is_mixed() {
        for config in [
            serde_json::json!({}),
            serde_json::Value::Null,
            serde_json::json!({"version_policy": null}),
        ] {
            assert_eq!(
                VersionPolicy::of(&repository(config)).unwrap(),
                VersionPolicy::Mixed
            );
        }
    }

    #[test]
    fn must_not_guess_an_unknown_policy() {
        for config in [
            serde_json::json!({"version_policy": "relase"}),
            serde_json::json!({"version_policy": 3}),
            serde_json::json!(["release"]),
        ] {
            assert!(VersionPolicy::of(&repository(config)).is_err());
        }
    }

    #[test]
    fn allows_by_kind() {
        assert!(VersionPolicy::Release.allows("1.0"));
        assert!(!VersionPolicy::Release.allows("1.0-SNAPSHOT"));
        assert!(VersionPolicy::Snapshot.allows("1.0-SNAPSHOT"));
        assert!(!VersionPolicy::Snapshot.allows("1.0"));
        assert!(VersionPolicy::Mixed.allows("1.0") && VersionPolicy::Mixed.allows("1.0-SNAPSHOT"));
        assert!(VersionPolicy::Release.refusal().contains("release"));
        assert!(VersionPolicy::Snapshot.refusal().contains("snapshot"));
    }
}
