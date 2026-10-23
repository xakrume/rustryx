//! Cleanup and retention policies, lifecycle runs, holds, immutability rules, soft quotas and
//! garbage collection runs.
//!
//! The unit the lifecycle engine works on is a component for every format. For OCI a component
//! is one manifest digest; its tags are rows owned by the OCI crate, and retention evaluates
//! tags but acts on whole components.

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::error::DomainError;
use crate::glob;
use crate::ids::{
    CleanupPolicyId, ComponentId, ImmutabilityRuleId, LifecycleHoldId, LifecycleRunId, ProjectId,
    RepositoryId, RetentionPolicyId, TaskRunId, UserId,
};
use crate::model::RepoFormat;
use crate::tasks::TaskRunStatus;

/// Most globs in one selector.
pub const MAX_SELECTOR_GLOBS: usize = 16;
/// Longest name of a policy, rule or hold target, in characters.
pub const MAX_POLICY_NAME_CHARS: usize = 128;
/// Longest regular expression of a cleanup criterion, in bytes.
pub const MAX_NAME_REGEX_BYTES: usize = 256;
/// Most retention rules in one policy.
pub const MAX_RETENTION_RULES: usize = 15;
/// Longest hold reason, in characters.
pub const MAX_HOLD_REASON_CHARS: usize = 500;
/// Most components a lifecycle run records as items.
pub const MAX_LIFECYCLE_ITEMS_PER_RUN: usize = 100_000;

mod gc;

pub use gc::{
    DEFAULT_GC_WORKERS, GcCounts, GcOptions, GcRun, GcSample, MAX_GC_WORKERS, SoftQuota,
    SoftQuotaKind,
};

/// Where a policy applies. Resolution for scoped policies is repository over project over
/// system.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "scope", content = "id", rename_all = "snake_case")]
pub enum PolicyScope {
    /// The whole installation.
    System,
    /// All repositories of a project.
    Project(ProjectId),
    /// One repository.
    Repository(RepositoryId),
}

/// Whether the globs of a [`NameSelector`] list what is selected or what is not.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SelectorMode {
    /// Select the names that match any glob.
    Matching,
    /// Select every name that matches none of the globs.
    Excluding,
}

/// A glob selector over names (repositories, tags, versions, paths).
///
/// The dialect and its limits are in [`crate::glob`]: at most 16 globs of at most 256
/// characters with at most 8 wildcards each. `Matching` needs at least one glob; `Excluding`
/// may be empty and then selects everything. Rust translates every glob to an anchored regular
/// expression (see [`NameSelector::to_regexes`]) that PostgreSQL evaluates in bounded time; the
/// translation is stored next to the glob.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NameSelector {
    /// Matching or excluding.
    pub mode: SelectorMode,
    /// The globs, at most 16.
    pub globs: Vec<String>,
}

impl NameSelector {
    /// Builds and validates a selector.
    ///
    /// # Errors
    /// Returns [`DomainError::Invalid`] when a rule of the selector or of a glob is broken.
    pub fn new(mode: SelectorMode, globs: Vec<String>) -> Result<Self, DomainError> {
        let selector = Self { mode, globs };
        selector.validate()?;
        Ok(selector)
    }

    /// A `Matching` selector over the given globs.
    ///
    /// # Errors
    /// Returns [`DomainError::Invalid`] when a rule is broken (including an empty list).
    pub fn matching(globs: Vec<String>) -> Result<Self, DomainError> {
        Self::new(SelectorMode::Matching, globs)
    }

    /// An `Excluding` selector over the given globs.
    ///
    /// # Errors
    /// Returns [`DomainError::Invalid`] when a rule is broken.
    pub fn excluding(globs: Vec<String>) -> Result<Self, DomainError> {
        Self::new(SelectorMode::Excluding, globs)
    }

    /// The selector that selects every name: `Excluding` with no glob.
    #[must_use]
    pub const fn everything() -> Self {
        Self {
            mode: SelectorMode::Excluding,
            globs: Vec::new(),
        }
    }

    /// Checks the glob count, `Matching` needing at least one, and every glob.
    ///
    /// # Errors
    /// Returns [`DomainError::Invalid`] naming the broken rule.
    pub fn validate(&self) -> Result<(), DomainError> {
        if self.globs.len() > MAX_SELECTOR_GLOBS {
            return Err(DomainError::invalid("a selector has at most 16 globs"));
        }
        if self.mode == SelectorMode::Matching && self.globs.is_empty() {
            return Err(DomainError::invalid(
                "a matching selector needs at least one glob",
            ));
        }
        self.globs.iter().try_for_each(|g| glob::validate(g))
    }

    /// The anchored regular expression of every glob, in order. Stored in the `*_regexes`
    /// columns next to the globs.
    ///
    /// # Errors
    /// Returns [`DomainError::Invalid`] when the selector is not valid.
    pub fn to_regexes(&self) -> Result<Vec<String>, DomainError> {
        self.validate()?;
        self.globs.iter().map(|g| glob::to_regex(g)).collect()
    }

    /// Applies the mode to a per expression test. `hit` receives each translated regular
    /// expression and says whether the item matches it. `Matching` selects when any
    /// expression hits; `Excluding` selects when none does, so an empty excluding selector
    /// selects everything.
    pub fn selects(&self, hit: impl FnMut(&str) -> bool) -> Result<bool, DomainError> {
        let regexes = self.to_regexes()?;
        let any = regexes.iter().map(String::as_str).any(hit);
        Ok(match self.mode {
            SelectorMode::Matching => any,
            SelectorMode::Excluding => !any,
        })
    }
}

/// What an immutability rule and a retention rule compare against.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ItemKind {
    /// An OCI tag.
    Tag,
    /// A package version.
    Version,
    /// An asset path.
    Path,
}

/// Release or prerelease, as the format adapter decides.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReleaseType {
    /// A release version.
    Release,
    /// A snapshot or prerelease version.
    Prerelease,
}

/// compat style policy: all set criteria must match (AND) for a component to be deleted. At
/// least one criterion is required.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CleanupCriteria {
    /// Delete components published more than this many days ago.
    pub max_age_days: Option<u32>,
    /// Days since the last download; the publish date counts when it was never downloaded.
    pub not_downloaded_days: Option<u32>,
    /// Keep the newest N versions of each package, delete the rest. Version order comes from
    /// the format adapter.
    pub keep_latest_versions: Option<u32>,
    /// Only releases or only prereleases.
    pub release_type: Option<ReleaseType>,
    /// Linear time regex against the component name (OCI: against each tag), at most 256
    /// bytes.
    pub name_regex: Option<String>,
    /// Format specific (Maven snapshot template: minimum count, grace days, remove after
    /// release), validated by the format's `LifecycleAdapter` when the policy is saved.
    #[serde(default)]
    pub extra: serde_json::Value,
}

impl CleanupCriteria {
    /// True when no criterion is set. A `null` or empty object `extra` is not a criterion.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        let extra_empty = match &self.extra {
            serde_json::Value::Null => true,
            serde_json::Value::Object(map) => map.is_empty(),
            _ => false,
        };
        self.max_age_days.is_none()
            && self.not_downloaded_days.is_none()
            && self.keep_latest_versions.is_none()
            && self.release_type.is_none()
            && self.name_regex.is_none()
            && extra_empty
    }

    /// Checks that at least one criterion is set and that the regular expression is at most
    /// 256 bytes. Its syntax is checked by the service with the `regex` crate.
    ///
    /// # Errors
    /// Returns [`DomainError::Invalid`] naming the broken rule.
    pub fn validate(&self) -> Result<(), DomainError> {
        if self.is_empty() {
            return Err(DomainError::invalid(
                "a cleanup policy needs at least one criterion",
            ));
        }
        if self
            .name_regex
            .as_ref()
            .is_some_and(|re| re.len() > MAX_NAME_REGEX_BYTES)
        {
            return Err(DomainError::invalid(
                "the name regex must be at most 256 bytes",
            ));
        }
        Ok(())
    }
}

/// A cleanup policy (compat style, attached to repositories by id).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct CleanupPolicy {
    /// Identifier.
    pub id: CleanupPolicyId,
    /// Unique name.
    pub name: String,
    /// Description.
    pub description: String,
    /// `None` means any format; criteria a format does not support are refused when the policy
    /// is attached.
    pub format: Option<RepoFormat>,
    /// What to delete.
    pub criteria: CleanupCriteria,
    /// Optimistic concurrency version. A dry run is valid only for the version it ran on.
    pub version: i64,
    /// Creation time.
    #[serde(with = "crate::rfc3339")]
    pub created_at: OffsetDateTime,
    /// Last change time.
    #[serde(with = "crate::rfc3339")]
    pub updated_at: OffsetDateTime,
}

impl CleanupPolicy {
    /// A policy for any format with an empty description and version 1.
    #[must_use]
    pub fn new(
        id: CleanupPolicyId,
        name: impl Into<String>,
        criteria: CleanupCriteria,
        now: OffsetDateTime,
    ) -> Self {
        Self {
            id,
            name: name.into(),
            description: String::new(),
            format: None,
            criteria,
            version: 1,
            created_at: now,
            updated_at: now,
        }
    }

    /// Checks name, description (500 characters) and criteria.
    ///
    /// # Errors
    /// Returns [`DomainError::Invalid`] naming the broken rule.
    pub fn validate(&self) -> Result<(), DomainError> {
        validate_name(&self.name, "cleanup policy name")?;
        if self.description.chars().count() > 500 {
            return Err(DomainError::invalid(
                "cleanup policy description must be at most 500 characters",
            ));
        }
        self.criteria.validate()
    }
}

fn validate_name(name: &str, what: &str) -> Result<(), DomainError> {
    if name.is_empty() || name.chars().count() > MAX_POLICY_NAME_CHARS {
        return Err(DomainError::invalid(format!(
            "{what} must be 1 to 128 characters"
        )));
    }
    Ok(())
}

/// Harbor style retention: rules say what to KEEP; items a rule covers and no rule keeps are
/// deleted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "template", rename_all = "snake_case")]
pub enum RetentionTemplate {
    /// Keep the N most recently pushed items.
    KeepLastPushed {
        /// How many to keep.
        count: u32,
    },
    /// Keep the N most recently pulled items.
    KeepLastPulled {
        /// How many to keep.
        count: u32,
    },
    /// Keep items pushed within the last N days.
    KeepPushedWithinDays {
        /// Window in days.
        days: u32,
    },
    /// Keep items pulled within the last N days.
    KeepPulledWithinDays {
        /// Window in days.
        days: u32,
    },
    /// Non OCI formats: newest N versions.
    KeepLatestVersions {
        /// How many to keep.
        count: u32,
    },
}

/// One retention rule.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RetentionRule {
    /// What the rule keeps.
    pub template: RetentionTemplate,
    /// Which repositories the rule covers.
    pub repository: NameSelector,
    /// Tags (OCI), versions or paths, as the format's `LifecycleAdapter::item_kind` says.
    pub item: NameSelector,
    /// OCI only: whether untagged components are covered by this rule.
    pub include_untagged: bool,
}

/// A retention policy. At most one per scope (project or repository), 1 to 15 rules. The
/// schedule is a `task_definitions` row of kind `retention` that the service creates and
/// updates together with the policy.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct RetentionPolicy {
    /// Identifier.
    pub id: RetentionPolicyId,
    /// Project or repository, never `System`.
    pub scope: PolicyScope,
    /// Name.
    pub name: String,
    /// 1 to 15 rules.
    pub rules: Vec<RetentionRule>,
    /// A policy never deletes the last remaining version of a package unless this is true.
    pub allow_delete_last_version: bool,
    /// Optimistic concurrency version.
    pub version: i64,
    /// Creation time.
    #[serde(with = "crate::rfc3339")]
    pub created_at: OffsetDateTime,
    /// Last change time.
    #[serde(with = "crate::rfc3339")]
    pub updated_at: OffsetDateTime,
}

impl RetentionPolicy {
    /// A policy that protects the last version of every package, with version 1.
    #[must_use]
    pub fn new(
        id: RetentionPolicyId,
        scope: PolicyScope,
        name: impl Into<String>,
        rules: Vec<RetentionRule>,
        now: OffsetDateTime,
    ) -> Self {
        Self {
            id,
            scope,
            name: name.into(),
            rules,
            allow_delete_last_version: false,
            version: 1,
            created_at: now,
            updated_at: now,
        }
    }

    /// Checks the scope, the name, the rule count and every selector.
    ///
    /// # Errors
    /// Returns [`DomainError::Invalid`] naming the broken rule.
    pub fn validate(&self) -> Result<(), DomainError> {
        if self.scope == PolicyScope::System {
            return Err(DomainError::invalid(
                "a retention policy belongs to a project or a repository",
            ));
        }
        validate_name(&self.name, "retention policy name")?;
        if self.rules.is_empty() || self.rules.len() > MAX_RETENTION_RULES {
            return Err(DomainError::invalid("a retention policy has 1 to 15 rules"));
        }
        self.rules.iter().try_for_each(|rule| {
            rule.repository.validate()?;
            rule.item.validate()
        })
    }
}

/// Kind of a lifecycle run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LifecycleKind {
    /// A cleanup policy run.
    Cleanup,
    /// A retention policy run.
    Retention,
    /// A label cleanup run.
    LabelCleanup,
}

/// What a run decided for one component.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ItemDecision {
    /// Selected and deleted (or selected, in a dry run).
    Delete,
    /// Selected, but the delete failed.
    DeleteFailed,
    /// Kept: an immutability rule covers it.
    ProtectedImmutable,
    /// Kept: a hold covers it.
    ProtectedHold,
    /// Kept: it is the last remaining version of its package.
    ProtectedLastVersion,
}

/// Counters of a lifecycle run.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LifecycleCounts {
    /// Components evaluated.
    pub evaluated: u64,
    /// Components selected for deletion.
    pub selected: u64,
    /// Selected components kept because of an immutability rule.
    pub protected_immutable: u64,
    /// Selected components kept because of a hold.
    pub protected_hold: u64,
    /// Selected components kept as the last version.
    pub protected_last_version: u64,
    /// Components deleted.
    pub deleted: u64,
    /// Deletions that failed.
    pub failed: u64,
    /// Bytes of the selected components.
    pub bytes_selected: u64,
    /// Bytes of the deleted components.
    pub bytes_deleted: u64,
}

/// One execution of a policy. A dry run stores the plan; a real run stores what it deleted.
/// The rows are the authoritative deletion record (the audit batch row only points at the run
/// id), so a failing audit sink cannot lose it.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct LifecycleRun {
    /// Identifier.
    pub id: LifecycleRunId,
    /// Cleanup, retention or label cleanup.
    pub kind: LifecycleKind,
    /// The cleanup policy, for a cleanup run.
    pub cleanup_policy_id: Option<CleanupPolicyId>,
    /// The retention policy, for a retention run.
    pub retention_policy_id: Option<RetentionPolicyId>,
    /// The repository the run worked on, when it is per repository.
    pub repository_id: Option<RepositoryId>,
    /// A dry run deletes nothing.
    pub dry_run: bool,
    /// Version of the policy this run evaluated. A real run needs a finished dry run of the
    /// same version.
    pub policy_version: i64,
    /// The task run that executes it.
    pub task_run_id: Option<TaskRunId>,
    /// State of the run.
    pub status: TaskRunStatus,
    /// Counters.
    pub counts: LifecycleCounts,
    /// True when more than the per run item cap (100 000) rows were selected; the counts stay
    /// exact.
    pub truncated: bool,
    /// Who asked for the run, `None` for a scheduled run.
    pub requested_by: Option<UserId>,
    /// An `nx-admin` confirmed a run that tripped the mass delete brake.
    pub confirmed_mass_delete: bool,
    /// Start time.
    #[serde(default, with = "crate::rfc3339::option")]
    pub started_at: Option<OffsetDateTime>,
    /// End time.
    #[serde(default, with = "crate::rfc3339::option")]
    pub finished_at: Option<OffsetDateTime>,
}

impl LifecycleRun {
    /// A queued run with zero counters and no brake confirmation.
    #[must_use]
    pub fn new(
        id: LifecycleRunId,
        kind: LifecycleKind,
        dry_run: bool,
        policy_version: i64,
    ) -> Self {
        Self {
            id,
            kind,
            cleanup_policy_id: None,
            retention_policy_id: None,
            repository_id: None,
            dry_run,
            policy_version,
            task_run_id: None,
            status: TaskRunStatus::Queued,
            counts: LifecycleCounts::default(),
            truncated: false,
            requested_by: None,
            confirmed_mass_delete: false,
            started_at: None,
            finished_at: None,
        }
    }
}

/// One component a run selected or protected.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LifecycleItem {
    /// The run.
    pub run_id: LifecycleRunId,
    /// Position inside the run.
    pub seq: u64,
    /// Repository name.
    pub repository: String,
    /// The component (no foreign key: a deleted component stays in the record).
    pub component_id: ComponentId,
    /// Human readable coordinate.
    pub coordinate: String,
    /// OCI tags of the component.
    pub tags: Vec<String>,
    /// Size of the component.
    pub size_bytes: u64,
    /// What the run decided.
    pub decision: ItemDecision,
    /// Stable code: the rule index, the criterion, or the protecting rule or hold id. No free
    /// text from users.
    pub reason: String,
}

/// Kind of a hold.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HoldKind {
    /// Blocks lifecycle jobs, promotion moves, replication deletes and interactive deletes and
    /// overwrites.
    Legal,
    /// A release repository standing hold: blocks lifecycle jobs and promotion moves.
    Release,
    /// Taken automatically on the source of a running promotion, released when it ends.
    Promotion,
}

/// What a hold covers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "id", rename_all = "snake_case")]
pub enum HoldTarget {
    /// One component.
    Component(ComponentId),
    /// Every component of a repository.
    Repository(RepositoryId),
    /// Every component of a project.
    Project(ProjectId),
}

/// A hold: protects its target from deletion.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct LifecycleHold {
    /// Identifier.
    pub id: LifecycleHoldId,
    /// Legal, release or promotion.
    pub kind: HoldKind,
    /// What it covers.
    pub target: HoldTarget,
    /// Required, 1 to 500 characters.
    pub reason: String,
    /// Who created it, `None` for a system made hold.
    pub created_by: Option<UserId>,
    /// Creation time.
    #[serde(with = "crate::rfc3339")]
    pub created_at: OffsetDateTime,
    /// Active while `released_at` is unset and `expires_at` is unset or in the future.
    #[serde(default, with = "crate::rfc3339::option")]
    pub expires_at: Option<OffsetDateTime>,
    /// Release time.
    #[serde(default, with = "crate::rfc3339::option")]
    pub released_at: Option<OffsetDateTime>,
    /// Who released it.
    pub released_by: Option<UserId>,
}

impl LifecycleHold {
    /// An active hold without expiry.
    #[must_use]
    pub fn new(
        id: LifecycleHoldId,
        kind: HoldKind,
        target: HoldTarget,
        reason: impl Into<String>,
        now: OffsetDateTime,
    ) -> Self {
        Self {
            id,
            kind,
            target,
            reason: reason.into(),
            created_by: None,
            created_at: now,
            expires_at: None,
            released_at: None,
            released_by: None,
        }
    }

    /// True while the hold protects its target: not released, and not expired. A hold whose
    /// expiry equals `now` has expired.
    #[must_use]
    pub fn is_active_at(&self, now: OffsetDateTime) -> bool {
        self.released_at.is_none() && self.expires_at.is_none_or(|expiry| expiry > now)
    }

    /// Checks the reason (1 to 500 characters).
    ///
    /// # Errors
    /// Returns [`DomainError::Invalid`] for an empty or oversized reason.
    pub fn validate(&self) -> Result<(), DomainError> {
        let length = self.reason.chars().count();
        if length == 0 || length > MAX_HOLD_REASON_CHARS {
            return Err(DomainError::invalid(
                "a hold reason must be 1 to 500 characters",
            ));
        }
        Ok(())
    }
}

/// A rule that makes matching items unchangeable: no overwrite, delete, retag or replacement
/// by replication or promotion. Matching is SQL evaluable (globs only, not CEL), because the
/// check runs inside the publish transaction.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct ImmutabilityRule {
    /// Identifier.
    pub id: ImmutabilityRuleId,
    /// Project or repository, never `System`. At most 15 rules per scope.
    pub scope: PolicyScope,
    /// Name.
    pub name: String,
    /// What `item` is compared against.
    pub item_kind: ItemKind,
    /// Which repositories of the scope the rule covers.
    pub repository: NameSelector,
    /// Which items it covers.
    pub item: NameSelector,
    /// A disabled rule protects nothing.
    pub enabled: bool,
    /// Optimistic concurrency version.
    pub version: i64,
    /// Creator.
    pub created_by: Option<UserId>,
    /// Creation time.
    #[serde(with = "crate::rfc3339")]
    pub created_at: OffsetDateTime,
    /// Last change time.
    #[serde(with = "crate::rfc3339")]
    pub updated_at: OffsetDateTime,
}

impl ImmutabilityRule {
    /// An enabled rule over every repository of its scope.
    #[must_use]
    pub fn new(
        id: ImmutabilityRuleId,
        scope: PolicyScope,
        name: impl Into<String>,
        item_kind: ItemKind,
        item: NameSelector,
        now: OffsetDateTime,
    ) -> Self {
        Self {
            id,
            scope,
            name: name.into(),
            item_kind,
            repository: NameSelector::everything(),
            item,
            enabled: true,
            version: 1,
            created_by: None,
            created_at: now,
            updated_at: now,
        }
    }

    /// Checks the scope, the name and both selectors.
    ///
    /// # Errors
    /// Returns [`DomainError::Invalid`] naming the broken rule.
    pub fn validate(&self) -> Result<(), DomainError> {
        if self.scope == PolicyScope::System {
            return Err(DomainError::invalid(
                "an immutability rule belongs to a project or a repository",
            ));
        }
        validate_name(&self.name, "immutability rule name")?;
        self.repository.validate()?;
        self.item.validate()
    }
}

#[cfg(test)]
mod tests;
