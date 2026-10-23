//! The pull gate, scan policies, CVE exceptions and the gate decision.

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use super::{MAX_ALLOWLIST_REASON_CHARS, Severity, normalize_vuln_id};
use crate::error::DomainError;
use crate::ids::{
    ComponentId, CveAllowlistEntryId, ProjectId, ScannerId, SecurityPolicyId, UserId,
};
use crate::lifecycle::PolicyScope;

/// Whether a gate decision is enforced or only recorded.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GateMode {
    /// A refusal is returned to the client.
    Enforce,
    /// The pull proceeds; the audit row and the event are written.
    AuditOnly,
}

/// The pull gate settings. A threshold of `None` switches the gate off. `Negligible` (and
/// `Unknown`) are rejected as a threshold because they would block everything.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PullGatePolicy {
    /// Lowest severity that blocks, `None` for no gate.
    pub threshold: Option<Severity>,
    /// Enforce or audit only.
    pub mode: GateMode,
    /// In `Enforce`: may a component without a complete scan be pulled? Default false (fail
    /// closed).
    pub allow_unscanned: bool,
    /// Treat a report older than this many days as not scanned (`None`: never).
    pub max_report_age_days: Option<u32>,
}

impl Default for PullGatePolicy {
    /// The seeded system gate: threshold `High`, `AuditOnly`, unscanned components are not
    /// allowed (relevant once the mode is `Enforce`), reports never go stale.
    fn default() -> Self {
        Self {
            threshold: Some(Severity::High),
            mode: GateMode::AuditOnly,
            allow_unscanned: false,
            max_report_age_days: None,
        }
    }
}

impl PullGatePolicy {
    /// Checks the threshold.
    ///
    /// # Errors
    /// Returns [`DomainError::Invalid`] for `Negligible` and `Unknown` thresholds.
    pub fn validate(&self) -> Result<(), DomainError> {
        if matches!(
            self.threshold,
            Some(Severity::Negligible | Severity::Unknown)
        ) {
            return Err(DomainError::invalid(
                "the gate threshold must be low, medium, high or critical",
            ));
        }
        Ok(())
    }
}

/// Whose CVE exceptions count for a project.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AllowlistMode {
    /// The system list applies.
    System,
    /// The project's own list applies.
    Project,
}

/// The settings of the scanning feature at one scope. A `None` field inherits (repository over
/// project over system); the system row is seeded and always complete (see
/// [`ScanPolicy::system_default`]).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct ScanPolicy {
    /// Identifier.
    pub id: SecurityPolicyId,
    /// System, project or repository.
    pub scope: PolicyScope,
    /// Scan automatically on push.
    pub auto_scan: Option<bool>,
    /// The scanner to use.
    pub scanner_id: Option<ScannerId>,
    /// The pull gate.
    pub gate: Option<PullGatePolicy>,
    /// Quarantine new proxy components until a scan passes.
    pub quarantine: Option<bool>,
    /// Whose allowlist applies.
    pub allowlist_mode: Option<AllowlistMode>,
    /// Aggregate repository health from stored findings.
    pub health_check: Option<bool>,
    /// Produce an SBOM on push.
    pub sbom_on_push: Option<bool>,
    /// Optimistic concurrency version.
    pub version: i64,
}

impl ScanPolicy {
    /// A policy that inherits everything.
    #[must_use]
    pub fn new(id: SecurityPolicyId, scope: PolicyScope) -> Self {
        Self {
            id,
            scope,
            auto_scan: None,
            scanner_id: None,
            gate: None,
            quarantine: None,
            allowlist_mode: None,
            health_check: None,
            sbom_on_push: None,
            version: 1,
        }
    }

    /// The seeded system row: `auto_scan` true, gate `High` and `AuditOnly`, quarantine off,
    /// allowlist mode `System`, health check off, SBOM on push off. No scanner is chosen.
    #[must_use]
    pub fn system_default(id: SecurityPolicyId) -> Self {
        Self {
            auto_scan: Some(true),
            gate: Some(PullGatePolicy::default()),
            quarantine: Some(false),
            allowlist_mode: Some(AllowlistMode::System),
            health_check: Some(false),
            sbom_on_push: Some(false),
            ..Self::new(id, PolicyScope::System)
        }
    }

    /// True when no field is left to inherit: what the system row must be.
    #[must_use]
    pub fn is_complete(&self) -> bool {
        self.auto_scan.is_some()
            && self.gate.is_some()
            && self.quarantine.is_some()
            && self.allowlist_mode.is_some()
            && self.health_check.is_some()
            && self.sbom_on_push.is_some()
    }

    /// Checks the gate.
    ///
    /// # Errors
    /// Returns [`DomainError::Invalid`] for an invalid gate threshold.
    pub fn validate(&self) -> Result<(), DomainError> {
        self.gate.as_ref().map_or(Ok(()), PullGatePolicy::validate)
    }
}

/// Where a CVE exception applies.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "scope", content = "id", rename_all = "snake_case")]
pub enum AllowlistScope {
    /// Every component.
    System,
    /// The components of a project.
    Project(ProjectId),
    /// One component.
    Component(ComponentId),
}

/// A CVE exception with a recorded reason, author and optional expiry. An expired entry stops
/// counting at once; the expiry itself is audited by `allowlist_expiry`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct CveAllowlistEntry {
    /// Identifier.
    pub id: CveAllowlistEntryId,
    /// Where it applies. A scope never applies outside itself.
    pub scope: AllowlistScope,
    /// Normalized upper case vulnerability id.
    pub vuln_id: String,
    /// Required, 1 to 500 characters.
    pub reason: String,
    /// Who created it.
    pub created_by: Option<UserId>,
    /// Creation time.
    #[serde(with = "crate::rfc3339")]
    pub created_at: OffsetDateTime,
    /// After this time the entry no longer counts.
    #[serde(default, with = "crate::rfc3339::option")]
    pub expires_at: Option<OffsetDateTime>,
    /// Optimistic concurrency version.
    pub version: i64,
}

impl CveAllowlistEntry {
    /// An entry without expiry. `vuln_id` should already be normalized (see
    /// [`normalize_vuln_id`]); [`CveAllowlistEntry::validate`] checks it.
    #[must_use]
    pub fn new(
        id: CveAllowlistEntryId,
        scope: AllowlistScope,
        vuln_id: impl Into<String>,
        reason: impl Into<String>,
        now: OffsetDateTime,
    ) -> Self {
        Self {
            id,
            scope,
            vuln_id: vuln_id.into(),
            reason: reason.into(),
            created_by: None,
            created_at: now,
            expires_at: None,
            version: 1,
        }
    }

    /// True while the entry counts: an expiry equal to `now` has already expired.
    #[must_use]
    pub fn is_active_at(&self, now: OffsetDateTime) -> bool {
        self.expires_at.is_none_or(|expiry| expiry > now)
    }

    /// Checks the id form and the reason (1 to 500 characters).
    ///
    /// # Errors
    /// Returns [`DomainError::Invalid`] naming the broken rule.
    pub fn validate(&self) -> Result<(), DomainError> {
        if normalize_vuln_id(&self.vuln_id)? != self.vuln_id {
            return Err(DomainError::invalid("vulnerability id is not normalized"));
        }
        let reason = self.reason.chars().count();
        if reason == 0 || reason > MAX_ALLOWLIST_REASON_CHARS {
            return Err(DomainError::invalid(
                "an exception needs a reason of 1 to 500 characters",
            ));
        }
        Ok(())
    }
}

/// Why the gate decided as it did.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "reason", rename_all = "snake_case")]
pub enum GateReason {
    /// Findings at or above the threshold.
    AboveThreshold {
        /// The highest blocking severity.
        highest: Severity,
        /// How many findings block.
        count: u32,
        /// The threshold that was applied.
        threshold: Severity,
    },
    /// No scan exists.
    NotScanned,
    /// The last scan failed.
    ScanFailed,
    /// The report is older than `max_report_age_days`.
    ScanStale,
    /// The component is quarantined.
    Quarantined,
    /// A required signature is missing.
    SignatureMissing,
    /// A signature does not verify.
    SignatureInvalid,
    /// The verdict could not be computed (store down). Mapped to 503, never to an allow in
    /// `Enforce`.
    Error,
}

/// The gate's answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "decision", rename_all = "snake_case")]
pub enum GateDecision {
    /// The pull may proceed.
    Allow,
    /// `AuditOnly` mode: the pull proceeds, the audit row and the event are written.
    AllowAudited {
        /// What would have blocked it.
        reason: GateReason,
    },
    /// The pull is refused.
    Deny {
        /// Why.
        reason: GateReason,
    },
}
