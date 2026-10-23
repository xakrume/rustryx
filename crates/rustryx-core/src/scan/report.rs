//! Scan reports, findings and the per component security state.

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use super::{
    MAX_FINDING_TITLE_CHARS, MAX_SCAN_ERROR_CHARS, MAX_VULN_ID_LEN, ScanKind, ScannerKind, Severity,
};
use crate::error::DomainError;
use crate::ids::{ComponentId, ScanReportId, ScannerId, UserId};

/// State of a scan report.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ScanStatus {
    /// Waiting for a worker.
    Queued,
    /// A worker runs it.
    Running,
    /// Finished with a result.
    Complete,
    /// The scan failed: not scanned, never clean.
    Failed,
    /// Stopped on request.
    Stopped,
    /// The scanner cannot judge this subject: a state, not clean.
    Unsupported,
}

impl ScanStatus {
    /// True when the report will not change again.
    #[must_use]
    pub const fn is_terminal(self) -> bool {
        !matches!(self, Self::Queued | Self::Running)
    }
}

/// Findings per severity of one report.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ScanSummary {
    /// Findings per severity, index = `Severity as usize`.
    pub by_severity: [u32; 6],
    /// Findings with a fixed version available.
    pub fixable: u32,
    /// All findings.
    pub total: u32,
    /// Findings removed from the verdict by the allowlist at the time the report was stored
    /// (informational).
    pub suppressed: u32,
}

impl ScanSummary {
    /// Number of findings of one severity.
    #[must_use]
    pub fn count(&self, severity: Severity) -> u32 {
        self.by_severity[severity.index()]
    }

    /// The findings that block at `threshold` (see [`Severity::blocks_at`]): the highest
    /// severity among them and how many there are, or `None` when nothing blocks. `Unknown`
    /// findings rank with `Medium`, a real `Medium` wins a tie, and `Negligible` never counts.
    #[must_use]
    pub fn blocking(&self, threshold: Severity) -> Option<(Severity, u32)> {
        let count: u32 = Severity::ALL
            .into_iter()
            .filter(|s| s.blocks_at(threshold))
            .map(|s| self.count(s))
            .fold(0, u32::saturating_add);
        if count == 0 {
            return None;
        }
        let highest = [
            Severity::Critical,
            Severity::High,
            Severity::Medium,
            Severity::Unknown,
            Severity::Low,
        ]
        .into_iter()
        .find(|s| s.blocks_at(threshold) && self.count(*s) > 0)?;
        Some((highest, count))
    }
}

/// What started a scan.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScanTrigger {
    /// A push or a proxy fill.
    Push,
    /// A user asked for it.
    Manual,
    /// A schedule (scan all).
    Schedule,
    /// A rescan against newer advisory data.
    Rescan,
    /// A quarantined component awaiting its verdict.
    Quarantine,
}

/// One scan of one component.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct ScanReport {
    /// Identifier.
    pub id: ScanReportId,
    /// The scanned component.
    pub component_id: ComponentId,
    /// The scanner that ran it, `None` once that scanner was deleted.
    pub scanner_id: Option<ScannerId>,
    /// Kind of that scanner.
    pub scanner_kind: ScannerKind,
    /// Version of the scanner binary or service.
    pub scanner_version: String,
    /// Version of the advisory data the scan used (Trivy DB version, OSV snapshot time).
    pub dataset_version: String,
    /// Vulnerability scan or SBOM.
    pub kind: ScanKind,
    /// State.
    pub status: ScanStatus,
    /// What started it.
    pub trigger: ScanTrigger,
    /// Who asked for it, `None` for an automatic scan.
    pub requested_by: Option<UserId>,
    /// When it was queued.
    #[serde(with = "crate::rfc3339")]
    pub queued_at: OffsetDateTime,
    /// When a worker started it.
    #[serde(default, with = "crate::rfc3339::option")]
    pub started_at: Option<OffsetDateTime>,
    /// When it ended.
    #[serde(default, with = "crate::rfc3339::option")]
    pub finished_at: Option<OffsetDateTime>,
    /// Findings per severity.
    pub summary: ScanSummary,
    /// Short, sanitized, at most 500 characters. Never a command line, a path or a scanner
    /// stack trace.
    pub error: Option<String>,
    /// True for the report that currently decides the verdict of the component.
    pub is_current: bool,
}

impl ScanReport {
    /// A queued report with an empty summary and no error.
    #[must_use]
    pub fn new(
        id: ScanReportId,
        component_id: ComponentId,
        scanner_kind: ScannerKind,
        kind: ScanKind,
        trigger: ScanTrigger,
        queued_at: OffsetDateTime,
    ) -> Self {
        Self {
            id,
            component_id,
            scanner_id: None,
            scanner_kind,
            scanner_version: String::new(),
            dataset_version: String::new(),
            kind,
            status: ScanStatus::Queued,
            trigger,
            requested_by: None,
            queued_at,
            started_at: None,
            finished_at: None,
            summary: ScanSummary::default(),
            error: None,
            is_current: false,
        }
    }

    /// Checks the error text limit.
    ///
    /// # Errors
    /// Returns [`DomainError::Invalid`] when the error exceeds 500 characters.
    pub fn validate(&self) -> Result<(), DomainError> {
        if self
            .error
            .as_ref()
            .is_some_and(|e| e.chars().count() > MAX_SCAN_ERROR_CHARS)
        {
            return Err(DomainError::invalid(
                "scan error text must be at most 500 characters",
            ));
        }
        Ok(())
    }
}

/// Normalizes a vulnerability id for storage and comparison: trimmed and upper cased, so
/// `cve-2024-1234` and `CVE-2024-1234` are one id. The same form is what the allowlist column
/// constraint (`vuln_id = upper(vuln_id)`) requires.
///
/// # Errors
/// Returns [`DomainError::Invalid`] for an empty id, one longer than 64 characters, or one
/// with whitespace or control characters inside.
pub fn normalize_vuln_id(raw: &str) -> Result<String, DomainError> {
    let id = raw.trim();
    if id.is_empty() || id.len() > MAX_VULN_ID_LEN {
        return Err(DomainError::invalid(
            "vulnerability id must be 1 to 64 characters",
        ));
    }
    if !id.chars().all(|c| c.is_ascii_graphic()) {
        return Err(DomainError::invalid(
            "vulnerability id must not contain spaces or control characters",
        ));
    }
    Ok(id.to_ascii_uppercase())
}

/// One finding of a report. Findings exist only for the current report of a component.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Finding {
    /// The report.
    pub report_id: ScanReportId,
    /// Normalized upper case (see [`normalize_vuln_id`]): `CVE-2024-1234`,
    /// `GHSA-XXXX-XXXX-XXXX`, other OSV ids likewise.
    pub vuln_id: String,
    /// Affected package.
    pub package: String,
    /// Affected version.
    pub package_version: String,
    /// First fixed version, when known.
    pub fixed_version: Option<String>,
    /// Severity.
    pub severity: Severity,
    /// CVSS base score.
    pub cvss: Option<f32>,
    /// At most 200 characters.
    pub title: Option<String>,
}

impl Finding {
    /// Checks the id form, the title limit and the score range.
    ///
    /// # Errors
    /// Returns [`DomainError::Invalid`] naming the broken rule.
    pub fn validate(&self) -> Result<(), DomainError> {
        if normalize_vuln_id(&self.vuln_id)? != self.vuln_id {
            return Err(DomainError::invalid("vulnerability id is not normalized"));
        }
        if self
            .title
            .as_ref()
            .is_some_and(|t| t.chars().count() > MAX_FINDING_TITLE_CHARS)
        {
            return Err(DomainError::invalid(
                "finding title must be at most 200 characters",
            ));
        }
        if self
            .cvss
            .is_some_and(|score| !(0.0..=10.0).contains(&score))
        {
            return Err(DomainError::invalid("cvss must be between 0 and 10"));
        }
        Ok(())
    }
}

/// Whether a component is held back.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QuarantineState {
    /// Not quarantined.
    None,
    /// Hidden until a scan passes or someone releases it.
    Quarantined,
    /// Released by a person; stays releasable only by a new quarantine.
    Released,
}

/// One row per component: what the gate, the list views and the Security Hub read without
/// touching findings.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct ComponentSecurityState {
    /// The component.
    pub component_id: ComponentId,
    /// The current report.
    pub report_id: Option<ScanReportId>,
    /// `None` is "not scanned".
    pub scan_status: Option<ScanStatus>,
    /// Findings per severity of the current report.
    pub summary: ScanSummary,
    /// When the current report finished.
    #[serde(default, with = "crate::rfc3339::option")]
    pub scanned_at: Option<OffsetDateTime>,
    /// Quarantine state.
    pub quarantine: QuarantineState,
    /// Who released the quarantine.
    pub released_by: Option<UserId>,
    /// When it was released.
    #[serde(default, with = "crate::rfc3339::option")]
    pub released_at: Option<OffsetDateTime>,
}

impl ComponentSecurityState {
    /// The state of a component that was never scanned and is not quarantined.
    #[must_use]
    pub fn new(component_id: ComponentId) -> Self {
        Self {
            component_id,
            report_id: None,
            scan_status: None,
            summary: ScanSummary::default(),
            scanned_at: None,
            quarantine: QuarantineState::None,
            released_by: None,
            released_at: None,
        }
    }

    /// True only when a complete scan exists. A missing or failed scan is not clean.
    #[must_use]
    pub fn is_scanned(&self) -> bool {
        self.scan_status == Some(ScanStatus::Complete)
    }
}
