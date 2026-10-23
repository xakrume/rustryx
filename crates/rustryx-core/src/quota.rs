//! Storage quotas of projects and repositories.
//!
//! Usage counts the DISTINCT blobs a scope references: a blob shared by several assets of one
//! scope counts once, and garbage collection does not reduce usage (a deleted asset does).

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::error::DomainError;
use crate::ids::{ProjectId, RepositoryId};

/// Default edge of the `quota.near_threshold` event, in percent of the limit.
pub const DEFAULT_WARN_PERCENT: u8 = 80;

/// What a quota applies to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", content = "id", rename_all = "snake_case")]
pub enum QuotaScope {
    /// All repositories of a project.
    Project(ProjectId),
    /// One repository.
    Repository(RepositoryId),
}

/// A hard limit. `limit_bytes: None` is unlimited (the "minus one" of the reference product).
///
/// A project limit is stored in `quotas`; a repository limit stays in
/// `repositories.quota_bytes`, so the phase 1 column keeps its meaning.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct Quota {
    /// What the limit applies to.
    pub scope: QuotaScope,
    /// Maximum bytes of distinct blobs, `None` for unlimited.
    pub limit_bytes: Option<u64>,
    /// Edge for the `quota.near_threshold` event, 1 to 100.
    pub warn_percent: u8,
    /// Optimistic concurrency version.
    pub version: i64,
}

impl Quota {
    /// A quota with the default warn edge ([`DEFAULT_WARN_PERCENT`]) and version 1.
    #[must_use]
    pub fn new(scope: QuotaScope, limit_bytes: Option<u64>) -> Self {
        Self {
            scope,
            limit_bytes,
            warn_percent: DEFAULT_WARN_PERCENT,
            version: 1,
        }
    }

    /// Checks that `warn_percent` lies in 1 to 100.
    ///
    /// # Errors
    /// Returns [`DomainError::Invalid`] for 0 and for values above 100.
    pub fn validate(&self) -> Result<(), DomainError> {
        validate_warn_percent(self.warn_percent)
    }
}

/// Checks a `warn_percent` value (the `quotas.warn_percent` column accepts 1 to 100).
///
/// # Errors
/// Returns [`DomainError::Invalid`] for 0 and for values above 100.
pub fn validate_warn_percent(percent: u8) -> Result<(), DomainError> {
    if (1..=100).contains(&percent) {
        Ok(())
    } else {
        Err(DomainError::invalid("warn_percent must be 1 to 100"))
    }
}

/// Distinct blobs referenced by the scope, with the limit that applies.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct QuotaUsage {
    /// What the usage belongs to.
    pub scope: QuotaScope,
    /// Bytes of distinct blobs the scope references.
    pub used_bytes: u64,
    /// Number of assets (objects) of the scope.
    pub object_count: u64,
    /// Hard limit, `None` for unlimited.
    pub limit_bytes: Option<u64>,
    /// Edge for the `quota.near_threshold` event.
    pub warn_percent: u8,
    /// When the counter was last recomputed from the assets, `None` before the first recount.
    #[serde(default, with = "crate::rfc3339::option")]
    pub recounted_at: Option<OffsetDateTime>,
}

impl QuotaUsage {
    /// An empty counter with no limit and the default warn edge.
    #[must_use]
    pub fn new(scope: QuotaScope) -> Self {
        Self {
            scope,
            used_bytes: 0,
            object_count: 0,
            limit_bytes: None,
            warn_percent: DEFAULT_WARN_PERCENT,
            recounted_at: None,
        }
    }

    /// True when `extra` more bytes still fit under the limit (always true without a limit).
    /// The authority is the conditional update inside the publish transaction; this is the
    /// same arithmetic for pre checks and tests.
    #[must_use]
    pub fn fits(&self, extra: u64) -> bool {
        self.limit_bytes.is_none_or(|limit| {
            self.used_bytes
                .checked_add(extra)
                .is_some_and(|total| total <= limit)
        })
    }
}

/// The refusal that a publish returns when a limit would be exceeded.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuotaExceeded {
    /// The scope whose limit was hit.
    pub scope: QuotaScope,
    /// The limit.
    pub limit_bytes: u64,
    /// Usage before the write.
    pub used_bytes: u64,
    /// Bytes the refused write would have added.
    pub requested_bytes: u64,
}

/// Room left for a new write, the smallest over the scopes that apply to a repository.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Headroom {
    /// Bytes still available, `None` when no scope has a limit.
    pub bytes: Option<u64>,
    /// The scope that sets the smallest room, `None` when `bytes` is `None`.
    pub binding: Option<QuotaScope>,
}

impl Headroom {
    /// No limit applies.
    pub const UNLIMITED: Self = Self {
        bytes: None,
        binding: None,
    };

    /// True when a write of `declared_bytes` fits. A declared length that does not fit is
    /// answered 507 before the body is read.
    #[must_use]
    pub fn admits(&self, declared_bytes: u64) -> bool {
        self.bytes.is_none_or(|room| declared_bytes <= room)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{assert_wire, at, round_trip_json};

    #[test]
    fn scope_wire_form_is_adjacently_tagged() {
        let id = ProjectId::generate();
        assert_wire(
            &QuotaScope::Project(id),
            &format!(r#"{{"kind":"project","id":"{id}"}}"#),
        );
        let repo = RepositoryId::generate();
        assert_wire(
            &QuotaScope::Repository(repo),
            &format!(r#"{{"kind":"repository","id":"{repo}"}}"#),
        );
    }

    #[test]
    fn constructor_sets_documented_defaults() {
        let quota = Quota::new(QuotaScope::Project(ProjectId::generate()), Some(10));
        assert_eq!(quota.warn_percent, 80);
        assert_eq!(quota.version, 1);
        assert!(quota.validate().is_ok());
        let usage = QuotaUsage::new(quota.scope);
        assert_eq!((usage.used_bytes, usage.object_count), (0, 0));
        assert_eq!(usage.limit_bytes, None);
        assert_eq!(usage.warn_percent, 80);
        assert!(usage.recounted_at.is_none());
    }

    #[test]
    fn warn_percent_must_lie_in_one_to_hundred() {
        for bad in [0u8, 101, 255] {
            assert!(validate_warn_percent(bad).is_err(), "{bad}");
        }
        for good in [1u8, 80, 100] {
            assert!(validate_warn_percent(good).is_ok(), "{good}");
        }
    }

    #[test]
    fn usage_serializes_the_recount_time_as_rfc3339() {
        let mut usage = QuotaUsage::new(QuotaScope::Repository(RepositoryId::generate()));
        usage.recounted_at = Some(at());
        let json = round_trip_json(&usage);
        assert_eq!(json["recounted_at"], "2026-10-24T10:00:00Z");
        let absent: QuotaUsage = serde_json::from_value(serde_json::json!({
            "scope": {"kind": "repository", "id": RepositoryId::generate()},
            "used_bytes": 1, "object_count": 1, "limit_bytes": null, "warn_percent": 80
        }))
        .expect("an absent recounted_at is None");
        assert!(absent.recounted_at.is_none());
    }

    #[test]
    fn fits_is_exact_at_the_limit_and_safe_against_overflow() {
        let mut usage = QuotaUsage::new(QuotaScope::Project(ProjectId::generate()));
        assert!(usage.fits(u64::MAX), "no limit never refuses");
        usage.limit_bytes = Some(100);
        usage.used_bytes = 60;
        assert!(usage.fits(40));
        assert!(!usage.fits(41));
        usage.used_bytes = u64::MAX;
        assert!(!usage.fits(1), "overflow is a refusal, not a wrap");
    }

    #[test]
    fn headroom_admits_up_to_the_remaining_bytes() {
        assert!(Headroom::UNLIMITED.admits(u64::MAX));
        let room = Headroom {
            bytes: Some(10),
            binding: Some(QuotaScope::Repository(RepositoryId::generate())),
        };
        assert!(room.admits(10));
        assert!(!room.admits(11));
    }
}
