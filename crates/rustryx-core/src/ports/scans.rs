//! Ports of scanners, scan reports, CVE allowlists, security policies, trust material, the
//! Security Hub, exports and advisory data sets.
//!
//! Contract as in [`crate::ports`]: every method is one statement or one transaction and
//! therefore cancel safe (a cancelled future leaves the old or the new state, never a mix).
//! The scanner contract itself (a trait with a cancellation token) lives in `rustryx-scan`.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::error::StoreError;
use crate::events::EventDraft;
use crate::ids::{
    BlobId, ComponentId, CveAllowlistEntryId, ExportId, ProjectId, RepositoryId, ScanReportId,
    ScannerDataSetId, ScannerId, TrustMaterialId, UserId,
};
use crate::lifecycle::PolicyScope;
use crate::ports::repos::{Page, PageRequest};
use crate::scan::{
    AllowlistMode, AllowlistScope, ComponentSecurityState, CveAllowlistEntry, Finding, ProbeResult,
    QuarantineState, ScanPolicy, ScanReport, ScanStatus, ScanSummary, ScannerDataSet, ScannerKind,
    ScannerRecord, Severity, TrustMaterial, TrustMaterialKind, TrustPolicy,
};
use crate::tasks::TaskRunStatus;

/// Registered scanners.
#[async_trait]
pub trait ScannerRepo: Send + Sync {
    /// Inserts. `Conflict` when the name exists.
    async fn create(&self, scanner: &ScannerRecord) -> Result<(), StoreError>;
    /// Finds by id.
    async fn get(&self, id: ScannerId) -> Result<Option<ScannerRecord>, StoreError>;
    /// Lists all scanners.
    async fn list(&self) -> Result<Vec<ScannerRecord>, StoreError>;
    /// Replaces a scanner. `Conflict` on a stale `expected_version`.
    async fn update(
        &self,
        scanner: &ScannerRecord,
        expected_version: i64,
    ) -> Result<(), StoreError>;
    /// `Conflict` while a scan policy references it.
    async fn delete(&self, id: ScannerId) -> Result<(), StoreError>;
    /// Makes this scanner the default of its scan kind and format family.
    async fn set_default(&self, id: ScannerId) -> Result<(), StoreError>;
    /// Stores the result of a probe.
    async fn record_probe(&self, id: ScannerId, probe: &ProbeResult) -> Result<(), StoreError>;
}

/// Filter of a findings listing.
#[derive(Debug, Clone, Default)]
pub struct FindingFilter {
    /// Only findings at or above this severity.
    pub severity_at_least: Option<Severity>,
    /// Only findings with (or without) a fixed version.
    pub fixable: Option<bool>,
    /// Only this vulnerability id.
    pub vuln_id: Option<String>,
    /// Only this package.
    pub package: Option<String>,
}

/// Reports, findings, state and the verdict inputs.
#[async_trait]
pub trait ScanRepo: Send + Sync {
    /// Creates a queued report row (the queue entry itself is a `JobLeaseRepo` job with the
    /// dedupe key `scan:<component>:<kind>`).
    async fn start(&self, report: &ScanReport) -> Result<(), StoreError>;
    /// Marks the report running.
    async fn mark_running(&self, id: ScanReportId, at: OffsetDateTime) -> Result<(), StoreError>;
    /// One transaction: stores the report as `Complete`, deletes the findings of the previous
    /// current report, inserts the new findings (bounded at 50 000 per report; more is
    /// `Invalid` and the scan is `Failed` with `too_many_findings`), flips `is_current`,
    /// upserts `component_security_state`, keeps the newest 3 reports per
    /// `(component, kind)`, and writes the `scan.completed` outbox event. A new report
    /// therefore replaces the old verdict atomically.
    #[allow(
        clippy::too_many_arguments,
        reason = "the signature is the contract of the design document; a parameter struct would only move the same eight values"
    )]
    async fn complete(
        &self,
        id: ScanReportId,
        findings: &[Finding],
        summary: &ScanSummary,
        scanner_version: &str,
        dataset_version: &str,
        at: OffsetDateTime,
        events: &[EventDraft],
    ) -> Result<(), StoreError>;
    /// Terminal failure or stop. The previous current report and state are untouched, except
    /// the status shown.
    async fn fail(
        &self,
        id: ScanReportId,
        status: ScanStatus,
        error: &str,
        at: OffsetDateTime,
        events: &[EventDraft],
    ) -> Result<(), StoreError>;
    /// Finds a report.
    async fn get_report(&self, id: ScanReportId) -> Result<Option<ScanReport>, StoreError>;
    /// The security state of a component; `None` is "not scanned".
    async fn state(
        &self,
        component: ComponentId,
    ) -> Result<Option<ComponentSecurityState>, StoreError>;
    /// The states of several components.
    async fn states(
        &self,
        components: &[ComponentId],
    ) -> Result<Vec<ComponentSecurityState>, StoreError>;
    /// Findings of the current report of a component.
    async fn findings(
        &self,
        component: ComponentId,
        filter: &FindingFilter,
        page: &PageRequest,
    ) -> Result<Page<Finding>, StoreError>;
    /// Counts by severity of the current report's findings that match none of `allowed` (the
    /// verdict inputs).
    async fn effective_summary(
        &self,
        component: ComponentId,
        allowed: &[String],
    ) -> Result<ScanSummary, StoreError>;
    /// Components that were never scanned, or whose current report is older than `before`,
    /// newest first, for the rescan job.
    async fn stale_components(
        &self,
        before: OffsetDateTime,
        scope: Option<PolicyScope>,
        limit: u32,
    ) -> Result<Vec<ComponentId>, StoreError>;
    /// Quarantine transitions, with the outbox event in the same transaction.
    async fn set_quarantine(
        &self,
        component: ComponentId,
        state: QuarantineState,
        by: Option<UserId>,
        at: OffsetDateTime,
        events: &[EventDraft],
    ) -> Result<(), StoreError>;
}

/// CVE exceptions.
#[async_trait]
pub trait CveAllowlistRepo: Send + Sync {
    /// Inserts. `Conflict` on the same vulnerability in the same scope.
    async fn create(&self, entry: &CveAllowlistEntry) -> Result<(), StoreError>;
    /// Lists the entries of a scope.
    async fn list(
        &self,
        scope: AllowlistScope,
        include_expired: bool,
        page: &PageRequest,
    ) -> Result<Page<CveAllowlistEntry>, StoreError>;
    /// Replaces an entry. `Conflict` on a stale `expected_version`.
    async fn update(
        &self,
        entry: &CveAllowlistEntry,
        expected_version: i64,
    ) -> Result<(), StoreError>;
    /// Deletes an entry.
    async fn delete(&self, id: CveAllowlistEntryId) -> Result<(), StoreError>;
    /// Copies the system list into a project scope (a snapshot, it does not track later system
    /// removals). Returns how many entries were copied.
    async fn copy_system_to_project(
        &self,
        project: ProjectId,
        by: Option<UserId>,
    ) -> Result<u64, StoreError>;
    /// Ids that are active now for a component: its own, its project's when the mode is
    /// `Project`, else the system's; plus the component's. Used to build `effective_summary`.
    /// An expired entry is never included.
    async fn active_ids(
        &self,
        component: ComponentId,
        mode: AllowlistMode,
    ) -> Result<Vec<String>, StoreError>;
    /// Entries that expired and were not yet audited, marked in the same call. For
    /// `allowlist_expiry`.
    async fn take_expired_unaudited(
        &self,
        now: OffsetDateTime,
        limit: u32,
    ) -> Result<Vec<CveAllowlistEntry>, StoreError>;
}

/// Scan and trust policies per scope.
#[async_trait]
pub trait SecurityPolicyRepo: Send + Sync {
    /// Inserts or replaces the scan policy of its scope.
    async fn upsert_scan_policy(
        &self,
        policy: &ScanPolicy,
        expected_version: Option<i64>,
    ) -> Result<(), StoreError>;
    /// The scan policy of exactly this scope.
    async fn get_scan_policy(&self, scope: PolicyScope) -> Result<Option<ScanPolicy>, StoreError>;
    /// Field-wise merge repository over project over system for one repository. Cacheable per
    /// `security.revision`.
    async fn effective_for(&self, repository: RepositoryId) -> Result<ScanPolicy, StoreError>;
    /// Inserts or replaces the trust policy of its scope.
    async fn upsert_trust_policy(
        &self,
        policy: &TrustPolicy,
        expected_version: Option<i64>,
    ) -> Result<(), StoreError>;
    /// The trust policy of exactly this scope.
    async fn get_trust_policy(&self, scope: PolicyScope)
    -> Result<Option<TrustPolicy>, StoreError>;
    /// The trust policy that applies to a repository, if any.
    async fn effective_trust_for(
        &self,
        repository: RepositoryId,
    ) -> Result<Option<TrustPolicy>, StoreError>;
}

/// Trust material (public keys and certificates).
#[async_trait]
pub trait TrustRepo: Send + Sync {
    /// Inserts. `Conflict` on a duplicate name or fingerprint.
    async fn create(&self, material: &TrustMaterial) -> Result<(), StoreError>;
    /// Finds by id.
    async fn get(&self, id: TrustMaterialId) -> Result<Option<TrustMaterial>, StoreError>;
    /// Lists material, optionally of one kind.
    async fn list(&self, kind: Option<TrustMaterialKind>)
    -> Result<Vec<TrustMaterial>, StoreError>;
    /// `Conflict` while a trust policy lists it.
    async fn delete(&self, id: TrustMaterialId) -> Result<(), StoreError>;
}

/// Numbers of the Security Hub, refreshed by the `hub_stats` job.
#[derive(Debug, Clone)]
pub struct SecurityStats {
    /// When the numbers were computed.
    pub computed_at: OffsetDateTime,
    /// Components per highest severity, index = `Severity as usize`.
    pub components_by_severity: [u64; 6],
    /// Findings per severity.
    pub findings_by_severity: [u64; 6],
    /// The components with the most findings.
    pub top_components: Vec<(ComponentId, u32)>,
    /// The vulnerabilities that affect the most components.
    pub top_vulns: Vec<(String, u32)>,
}

/// Filter of a Security Hub search.
#[derive(Debug, Clone, Default)]
pub struct HubFilter {
    /// Only this severity.
    pub severity: Option<Severity>,
    /// Lowest CVSS score.
    pub min_cvss: Option<f32>,
    /// Highest CVSS score.
    pub max_cvss: Option<f32>,
    /// Only this project.
    pub project: Option<ProjectId>,
    /// Only this repository.
    pub repository: Option<RepositoryId>,
    /// Only this digest.
    pub digest: Option<String>,
    /// Only this package.
    pub package: Option<String>,
    /// Only this tag.
    pub tag: Option<String>,
}

/// One row of a Security Hub search.
#[derive(Debug, Clone)]
pub struct HubRow {
    /// The vulnerability.
    pub vuln_id: String,
    /// Its severity.
    pub severity: Severity,
    /// CVSS score.
    pub cvss: Option<f32>,
    /// Affected package.
    pub package: String,
    /// Affected version.
    pub package_version: String,
    /// First fixed version.
    pub fixed_version: Option<String>,
    /// The affected component.
    pub component_id: ComponentId,
    /// Its repository.
    pub repository: String,
    /// Its coordinate.
    pub coordinate: String,
}

/// Security Hub reads. Totals and the two top lists come from `security_stats` (refreshed by
/// `hub_stats`), search from `findings`.
#[async_trait]
pub trait SecurityHubRepo: Send + Sync {
    /// The last computed numbers, `None` before the first refresh.
    async fn stats(&self) -> Result<Option<SecurityStats>, StoreError>;
    /// A CVE id is mandatory (the index is on it, and an unfiltered scan of all findings is
    /// refused with `Invalid`).
    async fn search(
        &self,
        vuln_id: &str,
        filter: &HubFilter,
        page: &PageRequest,
    ) -> Result<Page<HubRow>, StoreError>;
    /// Recomputes the numbers.
    async fn refresh_stats(&self, now: OffsetDateTime) -> Result<(), StoreError>;
}

/// A queued or finished export (CSV and similar). One download is allowed.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct ExportJob {
    /// Identifier.
    pub id: ExportId,
    /// What is exported (for example `vulnerabilities`).
    pub kind: String,
    /// Who asked; only they may download.
    pub requested_by: Option<UserId>,
    /// Parameters of the export.
    pub params: serde_json::Value,
    /// State.
    pub status: TaskRunStatus,
    /// The blob that holds the result.
    pub blob_id: Option<BlobId>,
    /// After this time the export is purged.
    #[serde(with = "crate::rfc3339")]
    pub expires_at: OffsetDateTime,
    /// When the one allowed download happened.
    #[serde(default, with = "crate::rfc3339::option")]
    pub downloaded_at: Option<OffsetDateTime>,
    /// Creation time.
    #[serde(with = "crate::rfc3339")]
    pub created_at: OffsetDateTime,
}

impl ExportJob {
    /// A queued export without result.
    #[must_use]
    pub fn new(
        id: ExportId,
        kind: impl Into<String>,
        params: serde_json::Value,
        expires_at: OffsetDateTime,
        now: OffsetDateTime,
    ) -> Self {
        Self {
            id,
            kind: kind.into(),
            requested_by: None,
            params,
            status: TaskRunStatus::Queued,
            blob_id: None,
            expires_at,
            downloaded_at: None,
            created_at: now,
        }
    }
}

/// Exports.
#[async_trait]
pub trait ExportRepo: Send + Sync {
    /// Inserts a job.
    async fn create(&self, job: &ExportJob) -> Result<(), StoreError>;
    /// Finds by id.
    async fn get(&self, id: ExportId) -> Result<Option<ExportJob>, StoreError>;
    /// Ends a job and records its blob.
    async fn finish(
        &self,
        id: ExportId,
        status: TaskRunStatus,
        blob: Option<BlobId>,
    ) -> Result<(), StoreError>;
    /// Marks the one allowed download. `false` when it was already downloaded or expired.
    async fn claim_download(
        &self,
        id: ExportId,
        by: Option<UserId>,
        at: OffsetDateTime,
    ) -> Result<bool, StoreError>;
    /// Expired or downloaded jobs; the caller deletes their blobs and calls `delete`.
    async fn due_for_purge(
        &self,
        now: OffsetDateTime,
        limit: u32,
    ) -> Result<Vec<ExportJob>, StoreError>;
    /// Deletes a job row.
    async fn delete(&self, id: ExportId) -> Result<(), StoreError>;
}

/// Advisory data sets and scan access tokens.
#[async_trait]
pub trait ScannerDataRepo: Send + Sync {
    /// The current version of a data set.
    async fn current(
        &self,
        scanner_kind: ScannerKind,
        dataset: &str,
    ) -> Result<Option<ScannerDataSet>, StoreError>;
    /// Makes this version current and the previous one not current in one transaction.
    async fn publish(&self, set: &ScannerDataSet) -> Result<(), StoreError>;
    /// Versions older than the newest two, to delete with their blobs.
    async fn prunable(
        &self,
        scanner_kind: ScannerKind,
        dataset: &str,
    ) -> Result<Vec<ScannerDataSet>, StoreError>;
    /// Deletes a data set row.
    async fn delete(&self, id: ScannerDataSetId) -> Result<(), StoreError>;
    /// Stores a short lived scan access token (its SHA-256 hash) for an external image
    /// scanner. The token is bound to one scan and one repository.
    async fn create_scan_token(
        &self,
        token_hash: &str,
        scan: ScanReportId,
        repository: RepositoryId,
        expires_at: OffsetDateTime,
    ) -> Result<(), StoreError>;
    /// Looks a scan token up by hash. An expired token is not returned.
    async fn find_scan_token(
        &self,
        token_hash: &str,
        now: OffsetDateTime,
    ) -> Result<Option<(ScanReportId, RepositoryId)>, StoreError>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{at, round_trip_json};

    #[test]
    fn export_job_constructor_and_wire_form() {
        let mut job = ExportJob::new(
            ExportId::generate(),
            "vulnerabilities",
            serde_json::json!({"project": "p"}),
            at() + time::Duration::hours(24),
            at(),
        );
        assert_eq!(job.status, TaskRunStatus::Queued);
        assert!(job.blob_id.is_none() && job.downloaded_at.is_none() && job.requested_by.is_none());
        job.downloaded_at = Some(at());
        let json = round_trip_json(&job);
        assert_eq!(json["expires_at"], "2026-10-25T10:00:00Z");
        assert_eq!(json["downloaded_at"], "2026-10-24T10:00:00Z");
        assert_eq!(json["created_at"], "2026-10-24T10:00:00Z");
        assert_eq!(json["status"], "queued");
    }

    #[test]
    fn filters_default_to_everything() {
        let findings = FindingFilter::default();
        assert!(findings.severity_at_least.is_none() && findings.fixable.is_none());
        assert!(findings.vuln_id.is_none() && findings.package.is_none());
        let hub = HubFilter::default();
        assert!(hub.severity.is_none() && hub.project.is_none() && hub.repository.is_none());
        assert!(hub.min_cvss.is_none() && hub.max_cvss.is_none() && hub.tag.is_none());
    }
}
