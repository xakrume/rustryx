//! Routing rules: which request paths a proxy or group repository may serve.
//!
//! [`RuleRoutingGate`] is the real implementation of the [`RoutingGate`] seam. It reads the rule
//! a repository points at (`Repository::routing_rule_id`) through `RoutingRuleRepo` and
//! evaluates it against the path the format normalized in `classify`.
//!
//! ## Semantics (core's definition of `RoutingRule`)
//!
//! * A rule has a mode and 1 to 32 patterns, regular expressions of the `regex` crate (linear
//!   time, no backreferences, compiled size capped at 256 KiB).
//! * A pattern matches when it is found anywhere in the path (`is_match`, unanchored): the
//!   author anchors with `^` and `$` when the whole path is meant. The path has no leading
//!   slash and is already decoded exactly once, so `%2e%2e` tricks never reach a pattern.
//! * `Block`: the request is refused when ANY pattern matches.
//!   `Allow`: the request is refused unless ANY pattern matches. A path that matches nothing
//!   is therefore blocked by an allow list.
//! * A refusal is a 404 for the addressed repository (the content "does not exist here") and a
//!   silently skipped member inside a group. It is checked before the cache lookup, so a rule
//!   added later applies to cached content at once on every request it is seen by.
//!
//! ## Precedence
//!
//! A repository has at most one rule, so inside one repository there is nothing to rank. Across
//! a group the rules are cumulative and **a refusal always wins**: the engine evaluates the
//! rule of the addressed repository first, then the group pipeline evaluates the rule of each
//! member before it asks that member. A path is served by a member only if the group rule AND
//! the member rule let it through. An allow list on the group never widens a member's block
//! list, and the other way round.
//!
//! Only proxy and group repositories are gated. A rule attached to a hosted repository is
//! ignored: hosted content is written by the repository's own users, and a rule there would
//! hide data the caller was just allowed to upload.
//!
//! ## Failure
//!
//! The gate fails closed. A rule that cannot be read answers 503 (the engine) or skips the
//! member (a group), a rule id that no longer resolves or a stored pattern that does not
//! compile is a server error, never "allow".
//!
//! ## Freshness
//!
//! Compiled rules are cached per node for a short time (default 5 seconds, `Duration::ZERO`
//! turns the cache off) so that the rule is not read from the database on every proxy request.
//! A rule change is therefore visible on every node after at most that time; the node that
//! handled the change can call [`RuleRoutingGate::invalidate`] to be immediate.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use moka::future::Cache;
use regex::{RegexSet, RegexSetBuilder};
use rustryx_core::ids::RoutingRuleId;
use rustryx_core::model::{RepoType, Repository};
use rustryx_core::ports::RoutingRuleRepo;
use rustryx_core::routing::{MAX_ROUTING_COMPILED_BYTES, RoutingMode, RoutingRule};

use crate::context::{RoutingDecision, RoutingGate};
use crate::error::FormatError;
use crate::request::ArtifactRequest;

/// How long a compiled rule is reused before it is read again.
pub const DEFAULT_RULE_TTL: Duration = Duration::from_secs(5);

/// Most compiled rules kept per node. Rules are few (one per repository at most).
const CACHE_CAPACITY: u64 = 10_000;

/// A rule ready to evaluate.
struct CompiledRule {
    mode: RoutingMode,
    patterns: RegexSet,
}

impl CompiledRule {
    fn compile(rule: &RoutingRule) -> Result<Self, LoadError> {
        rule.validate()
            .map_err(|error| LoadError::Invalid(error.to_string()))?;
        // The compiled size is capped by the same constant the service checks when a rule is
        // saved, so a rule that was accepted always compiles here, and a row written around the
        // service cannot make a request compile a pathological pattern.
        let patterns = RegexSetBuilder::new(&rule.patterns)
            .size_limit(MAX_ROUTING_COMPILED_BYTES)
            .build()
            .map_err(|error| LoadError::Invalid(error.to_string()))?;
        Ok(Self {
            mode: rule.mode,
            patterns,
        })
    }

    fn decide(&self, path: &str) -> RoutingDecision {
        let matched = self.patterns.is_match(path);
        match (self.mode, matched) {
            (RoutingMode::Block, true) | (RoutingMode::Allow, false) => RoutingDecision::Block,
            // `RoutingMode` is not marked non exhaustive today. If it grows, the compiler stops
            // here, which is the point: an unknown mode must not silently allow.
            (RoutingMode::Block, false) | (RoutingMode::Allow, true) => RoutingDecision::Allow,
        }
    }
}

/// Why a rule could not be loaded. Cloneable because concurrent callers share one failed load;
/// the text goes to the log, the client only learns the class.
#[derive(Debug, Clone, thiserror::Error)]
enum LoadError {
    #[error("the rule store failed: {0}")]
    Store(String),
    #[error("the repository points at a routing rule that does not exist")]
    Missing,
    #[error("the stored routing rule is invalid: {0}")]
    Invalid(String),
}

/// The [`RoutingGate`] that evaluates the routing rule of each proxy and group repository.
#[derive(Clone)]
pub struct RuleRoutingGate {
    rules: Arc<dyn RoutingRuleRepo>,
    cache: Option<Cache<RoutingRuleId, Arc<CompiledRule>>>,
}

impl RuleRoutingGate {
    /// A gate that reuses compiled rules for [`DEFAULT_RULE_TTL`].
    #[must_use]
    pub fn new(rules: Arc<dyn RoutingRuleRepo>) -> Self {
        Self::with_ttl(rules, DEFAULT_RULE_TTL)
    }

    /// A gate that reuses compiled rules for `ttl`. A zero TTL reads the rule on every request.
    #[must_use]
    pub fn with_ttl(rules: Arc<dyn RoutingRuleRepo>, ttl: Duration) -> Self {
        let cache = (!ttl.is_zero()).then(|| {
            Cache::builder()
                .max_capacity(CACHE_CAPACITY)
                .time_to_live(ttl)
                .build()
        });
        Self { rules, cache }
    }

    /// Forgets one rule, for the node that just saved a change to it.
    pub async fn invalidate(&self, id: RoutingRuleId) {
        if let Some(cache) = &self.cache {
            cache.invalidate(&id).await;
        }
    }

    /// Forgets every compiled rule.
    pub async fn invalidate_all(&self) {
        if let Some(cache) = &self.cache {
            cache.invalidate_all();
            cache.run_pending_tasks().await;
        }
    }

    async fn load(&self, id: RoutingRuleId) -> Result<Arc<CompiledRule>, LoadError> {
        let rule = self
            .rules
            .get(id)
            .await
            .map_err(|error| LoadError::Store(error.to_string()))?
            .ok_or(LoadError::Missing)?;
        CompiledRule::compile(&rule).map(Arc::new)
    }

    async fn rule(&self, id: RoutingRuleId) -> Result<Arc<CompiledRule>, FormatError> {
        let loaded = match &self.cache {
            // Only successes are cached, and concurrent misses share one load.
            Some(cache) => cache
                .try_get_with(id, self.load(id))
                .await
                .map_err(|shared| (*shared).clone()),
            None => self.load(id).await,
        };
        loaded.map_err(|error| {
            tracing::warn!(%error, rule = %id, "routing rule could not be evaluated; failing closed");
            match error {
                LoadError::Store(_) => FormatError::Unavailable {
                    reason: "routing rules are unavailable",
                    retry_after: Some(Duration::from_secs(5)),
                },
                LoadError::Missing | LoadError::Invalid(_) => {
                    FormatError::internal("the routing rule of the repository is unusable")
                }
            }
        })
    }
}

impl std::fmt::Debug for RuleRoutingGate {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RuleRoutingGate")
            .field("cached", &self.cache.is_some())
            .finish_non_exhaustive()
    }
}

#[async_trait]
impl RoutingGate for RuleRoutingGate {
    async fn check(
        &self,
        repository: &Repository,
        request: &ArtifactRequest,
    ) -> Result<RoutingDecision, FormatError> {
        if repository.repo_type == RepoType::Hosted {
            return Ok(RoutingDecision::Allow);
        }
        let Some(id) = repository.routing_rule_id else {
            return Ok(RoutingDecision::Allow);
        };
        Ok(self.rule(id).await?.decide(request.path.as_str()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustryx_core::ids::RoutingRuleId;
    use time::OffsetDateTime;

    fn compiled(mode: RoutingMode, patterns: &[&str]) -> Result<CompiledRule, LoadError> {
        CompiledRule::compile(&RoutingRule::new(
            RoutingRuleId::generate(),
            "r",
            mode,
            patterns.iter().map(|p| (*p).to_owned()).collect(),
            OffsetDateTime::UNIX_EPOCH,
        ))
    }

    #[test]
    fn the_decision_matrix_is_block_on_match_and_allow_on_match() {
        use RoutingDecision::{Allow, Block};
        let block = compiled(RoutingMode::Block, &["^a/", "b$"]).unwrap();
        assert_eq!(block.decide("a/x"), Block);
        assert_eq!(block.decide("x/b"), Block, "any pattern is enough");
        assert_eq!(block.decide("x/y"), Allow);
        let allow = compiled(RoutingMode::Allow, &["^a/", "b$"]).unwrap();
        assert_eq!(allow.decide("a/x"), Allow);
        assert_eq!(allow.decide("x/b"), Allow);
        assert_eq!(
            allow.decide("x/y"),
            Block,
            "an allow list refuses what matches nothing"
        );
    }

    #[test]
    fn patterns_match_anywhere_unless_anchored() {
        let block = compiled(RoutingMode::Block, &["snapshot"]).unwrap();
        assert_eq!(
            block.decide("com/x/1.0-snapshot/x.jar"),
            RoutingDecision::Block
        );
        let anchored = compiled(RoutingMode::Block, &["^snapshot$"]).unwrap();
        assert_eq!(
            anchored.decide("com/x/1.0-snapshot/x.jar"),
            RoutingDecision::Allow
        );
    }

    #[test]
    fn a_rule_that_core_would_refuse_to_save_does_not_compile() {
        assert!(matches!(
            compiled(RoutingMode::Block, &[]),
            Err(LoadError::Invalid(_))
        ));
        assert!(matches!(
            compiled(RoutingMode::Block, &[""]),
            Err(LoadError::Invalid(_))
        ));
        assert!(matches!(
            compiled(RoutingMode::Block, &["("]),
            Err(LoadError::Invalid(_))
        ));
        let long = "a".repeat(257);
        assert!(matches!(
            compiled(RoutingMode::Block, &[long.as_str()]),
            Err(LoadError::Invalid(_))
        ));
    }

    #[test]
    fn must_not_compile_a_pattern_beyond_the_size_limit() {
        // Short as text, huge as a program: nested bounded repetition of a unicode class.
        let bomb = r"(?:\w{100}){100}";
        assert!(bomb.len() <= 256);
        assert!(matches!(
            compiled(RoutingMode::Block, &[bomb]),
            Err(LoadError::Invalid(_))
        ));
    }
}
