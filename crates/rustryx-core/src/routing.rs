//! Routing rules of proxy repositories: which request paths may be fetched from the upstream.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::error::{DomainError, StoreError};
use crate::ids::{RepositoryId, RoutingRuleId};

/// Most patterns in one rule.
pub const MAX_ROUTING_PATTERNS: usize = 32;
/// Longest pattern, in bytes.
pub const MAX_ROUTING_PATTERN_BYTES: usize = 256;
/// Largest compiled size of a pattern, in bytes (checked by the service with the `regex` crate
/// when the rule is saved; core has no regex dependency).
pub const MAX_ROUTING_COMPILED_BYTES: usize = 256 * 1024;

/// What a matching path does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RoutingMode {
    /// Only matching paths may be fetched.
    Allow,
    /// Matching paths may not be fetched.
    Block,
}

/// A routing rule.
///
/// Patterns are regular expressions of the `regex` crate (linear time, no backreferences)
/// matched against the path AFTER the format's `classify` normalization. The rule is checked
/// before the cache lookup, so a new block rule applies to cached content at once. Limits at
/// save time: at most 32 patterns, each at most 256 bytes, compiled size at most 256 KiB.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct RoutingRule {
    /// Identifier.
    pub id: RoutingRuleId,
    /// Unique name.
    pub name: String,
    /// Allow or block.
    pub mode: RoutingMode,
    /// The patterns, 1 to 32.
    pub patterns: Vec<String>,
    /// Free text.
    pub description: String,
    /// Optimistic concurrency version.
    pub version: i64,
    /// Creation time.
    #[serde(with = "crate::rfc3339")]
    pub created_at: OffsetDateTime,
    /// Last change time.
    #[serde(with = "crate::rfc3339")]
    pub updated_at: OffsetDateTime,
}

impl RoutingRule {
    /// A rule with an empty description and version 1.
    #[must_use]
    pub fn new(
        id: RoutingRuleId,
        name: impl Into<String>,
        mode: RoutingMode,
        patterns: Vec<String>,
        now: OffsetDateTime,
    ) -> Self {
        Self {
            id,
            name: name.into(),
            mode,
            patterns,
            description: String::new(),
            version: 1,
            created_at: now,
            updated_at: now,
        }
    }

    /// Checks the shape limits: a name of 1 to 128 characters and 1 to 32 non empty patterns
    /// of at most 256 bytes. A rule with no pattern would match nothing and is refused. The
    /// compiled size is checked by the service, which has the regex engine.
    ///
    /// # Errors
    /// Returns [`DomainError::Invalid`] naming the broken rule.
    pub fn validate(&self) -> Result<(), DomainError> {
        if self.name.is_empty() || self.name.chars().count() > 128 {
            return Err(DomainError::invalid(
                "routing rule name must be 1 to 128 characters",
            ));
        }
        if self.patterns.is_empty() || self.patterns.len() > MAX_ROUTING_PATTERNS {
            return Err(DomainError::invalid("a routing rule has 1 to 32 patterns"));
        }
        if self
            .patterns
            .iter()
            .any(|p| p.is_empty() || p.len() > MAX_ROUTING_PATTERN_BYTES)
        {
            return Err(DomainError::invalid("a routing pattern has 1 to 256 bytes"));
        }
        Ok(())
    }
}

/// Routing rules of proxy repositories.
#[async_trait]
pub trait RoutingRuleRepo: Send + Sync {
    /// Inserts. `Conflict` when the name exists.
    async fn create(&self, rule: &RoutingRule) -> Result<(), StoreError>;
    /// Finds by id.
    async fn get(&self, id: RoutingRuleId) -> Result<Option<RoutingRule>, StoreError>;
    /// Lists all rules ordered by name.
    async fn list(&self) -> Result<Vec<RoutingRule>, StoreError>;
    /// Replaces a rule. `Conflict` on a stale `expected_version`.
    async fn update(&self, rule: &RoutingRule, expected_version: i64) -> Result<(), StoreError>;
    /// `Conflict` while a repository uses the rule; the repositories are in `assigned_to`.
    async fn delete(&self, id: RoutingRuleId) -> Result<(), StoreError>;
    /// The repositories that use the rule.
    async fn assigned_to(&self, id: RoutingRuleId) -> Result<Vec<RepositoryId>, StoreError>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{assert_wire, at, round_trip_json};

    fn rule(patterns: Vec<String>) -> RoutingRule {
        RoutingRule::new(
            RoutingRuleId::generate(),
            "block snapshots",
            RoutingMode::Block,
            patterns,
            at(),
        )
    }

    #[test]
    fn constructor_defaults_and_wire_form() {
        let rule = rule(vec!["^com/acme/.*-SNAPSHOT/".into()]);
        assert_eq!(rule.version, 1);
        assert_eq!(rule.description, "");
        assert!(rule.validate().is_ok());
        assert_wire(&RoutingMode::Allow, "\"allow\"");
        let json = round_trip_json(&rule);
        assert_eq!(json["mode"], "block");
        assert_eq!(json["created_at"], "2026-10-24T10:00:00Z");
    }

    #[test]
    fn pattern_limits_are_exact() {
        assert!(
            rule(Vec::new()).validate().is_err(),
            "a rule without pattern matches nothing"
        );
        let many: Vec<String> = (0..32).map(|i| format!("^p{i}$")).collect();
        assert!(rule(many).validate().is_ok());
        let too_many: Vec<String> = (0..33).map(|i| format!("^p{i}$")).collect();
        assert!(rule(too_many).validate().is_err());
        assert!(rule(vec!["a".repeat(256)]).validate().is_ok());
        assert!(rule(vec!["a".repeat(257)]).validate().is_err());
        assert!(rule(vec![String::new()]).validate().is_err());
        assert!(
            rule(vec!["é".repeat(129)]).validate().is_err(),
            "the limit counts bytes"
        );
    }

    #[test]
    fn name_limits() {
        let mut r = rule(vec!["a".into()]);
        r.name = String::new();
        assert!(r.validate().is_err());
        r.name = "n".repeat(129);
        assert!(r.validate().is_err());
    }
}
