//! Scan reports, findings, the per component security state, the Security Hub, exports and
//! advisory data sets.

use std::collections::{HashMap, HashSet};

use async_trait::async_trait;
use rustryx_core::error::StoreError;
use rustryx_core::events::EventDraft;
use rustryx_core::ids::RepositoryId;
use rustryx_core::ids::{BlobId, ComponentId, ExportId, ScanReportId, ScannerDataSetId, UserId};
use rustryx_core::lifecycle::PolicyScope;
use rustryx_core::ports::{
    ExportJob, ExportRepo, FindingFilter, HubFilter, HubRow, InvalidationEvent, Page, PageRequest,
    ScanRepo, ScannerDataRepo, SecurityHubRepo, SecurityStats,
};
use rustryx_core::scan::{
    ComponentSecurityState, Finding, MAX_FINDINGS_PER_REPORT, MAX_SCAN_ERROR_CHARS,
    QuarantineState, ScanKind, ScanReport, ScanStatus, ScanSummary, ScannerDataSet, ScannerKind,
};
use rustryx_core::tasks::TaskRunStatus;
use time::OffsetDateTime;

use super::outbox::validate_drafts;
use super::paging::paginate;
use super::state::State;
use super::{MemStore, conflict, not_found};

/// Reports kept per `(component, kind)`: the current one and the two before it.
const REPORTS_KEPT: usize = 3;
/// Entries in each of the two top lists of the Security Hub.
const TOP_LIST_LEN: usize = 5;

impl State {
    fn report_mut(&mut self, id: ScanReportId) -> Option<&mut ScanReport> {
        self.scan_reports.iter_mut().find(|r| r.id == id)
    }

    /// The current vulnerability report of a component and its findings.
    fn current_findings(&self, component: ComponentId) -> Option<(&ScanReport, &[Finding])> {
        let report = self.scan_reports.iter().find(|r| {
            r.component_id == component && r.kind == ScanKind::Vulnerability && r.is_current
        })?;
        let findings = self.findings.get(&report.id).map_or(&[][..], Vec::as_slice);
        Some((report, findings))
    }

    fn security_state_mut(&mut self, component: ComponentId) -> &mut ComponentSecurityState {
        self.security_states
            .entry(component)
            .or_insert_with(|| ComponentSecurityState::new(component))
    }

    /// Repositories that fall inside a policy scope.
    fn repositories_in(&self, scope: Option<PolicyScope>) -> HashSet<RepositoryId> {
        self.repositories
            .values()
            .filter(|repository| match scope {
                None | Some(PolicyScope::System) => true,
                Some(PolicyScope::Project(id)) => self.project_of(repository) == Some(id),
                Some(PolicyScope::Repository(id)) => repository.id == id,
            })
            .map(|repository| repository.id)
            .collect()
    }
}

fn summarize(findings: &[Finding], allowed: &HashSet<&str>) -> ScanSummary {
    let mut summary = ScanSummary::default();
    for finding in findings {
        if allowed.contains(finding.vuln_id.as_str()) {
            summary.suppressed = summary.suppressed.saturating_add(1);
            continue;
        }
        let slot = &mut summary.by_severity[finding.severity.index()];
        *slot = slot.saturating_add(1);
        summary.total = summary.total.saturating_add(1);
        if finding.fixed_version.is_some() {
            summary.fixable = summary.fixable.saturating_add(1);
        }
    }
    summary
}

fn finding_key(finding: &Finding) -> String {
    // Most severe first, then a stable order inside a severity.
    format!(
        "{}{}\u{1f}{}\u{1f}{}",
        5 - finding.severity.index(),
        finding.vuln_id,
        finding.package,
        finding.package_version
    )
}

#[async_trait]
impl ScanRepo for MemStore {
    async fn start(&self, report: &ScanReport) -> Result<(), StoreError> {
        let mut state = self.tables()?;
        report
            .validate()
            .map_err(|error| conflict(error.to_string()))?;
        if !state.components.contains_key(&report.component_id) {
            return Err(conflict(format!(
                "component {} does not exist",
                report.component_id
            )));
        }
        if state.scan_reports.iter().any(|r| r.id == report.id) {
            return Err(conflict(format!(
                "scan report {} already exists",
                report.id
            )));
        }
        let mut row = report.clone();
        // Whatever the caller wrote, a report starts queued and is never current: only
        // `complete` makes a report current.
        row.status = ScanStatus::Queued;
        row.is_current = false;
        state.scan_reports.push(row);
        Ok(())
    }

    async fn mark_running(&self, id: ScanReportId, at: OffsetDateTime) -> Result<(), StoreError> {
        let mut state = self.tables()?;
        let report = state
            .report_mut(id)
            .ok_or_else(|| not_found("scan report", id))?;
        if report.status != ScanStatus::Queued {
            return Err(conflict("only a queued scan can start running"));
        }
        report.status = ScanStatus::Running;
        report.started_at = Some(at);
        Ok(())
    }

    async fn complete(
        &self,
        id: ScanReportId,
        findings: &[Finding],
        summary: &ScanSummary,
        scanner_version: &str,
        dataset_version: &str,
        at: OffsetDateTime,
        events: &[EventDraft],
    ) -> Result<(), StoreError> {
        validate_drafts(events)?;
        let now = self.now();
        let mut state = self.tables()?;
        let report = state
            .scan_reports
            .iter()
            .find(|r| r.id == id)
            .cloned()
            .ok_or_else(|| not_found("scan report", id))?;
        if report.status.is_terminal() {
            return Err(conflict("the scan already finished"));
        }
        if findings.len() > MAX_FINDINGS_PER_REPORT {
            return Err(StoreError::Invalid(
                "a report holds at most 50000 findings".to_owned(),
            ));
        }
        let mut keys = HashSet::new();
        for finding in findings {
            finding
                .validate()
                .map_err(|error| conflict(error.to_string()))?;
            // The primary key of `findings`.
            let key = (&finding.vuln_id, &finding.package, &finding.package_version);
            if !keys.insert(key) {
                return Err(conflict("duplicate finding in one report"));
            }
        }
        // The replace, in one step: the old current report loses its flag and its findings,
        // the new one takes both, the state follows.
        let previous: Vec<ScanReportId> = state
            .scan_reports
            .iter()
            .filter(|r| {
                r.component_id == report.component_id && r.kind == report.kind && r.is_current
            })
            .map(|r| r.id)
            .collect();
        for old in previous {
            state.findings.remove(&old);
            if let Some(row) = state.report_mut(old) {
                row.is_current = false;
            }
        }
        let stored: Vec<Finding> = findings
            .iter()
            .map(|finding| {
                let mut row = finding.clone();
                row.report_id = id;
                row
            })
            .collect();
        if let Some(row) = state.report_mut(id) {
            row.status = ScanStatus::Complete;
            row.is_current = true;
            row.summary = summary.clone();
            scanner_version.clone_into(&mut row.scanner_version);
            dataset_version.clone_into(&mut row.dataset_version);
            row.finished_at = Some(at);
            row.error = None;
        }
        state.findings.insert(id, stored);
        if report.kind == ScanKind::Vulnerability {
            let security = state.security_state_mut(report.component_id);
            security.report_id = Some(id);
            security.scan_status = Some(ScanStatus::Complete);
            security.summary = summary.clone();
            security.scanned_at = Some(at);
        }
        prune_reports(&mut state, report.component_id, report.kind);
        state.push_events(events, now);
        Ok(())
    }

    async fn fail(
        &self,
        id: ScanReportId,
        status: ScanStatus,
        error: &str,
        at: OffsetDateTime,
        events: &[EventDraft],
    ) -> Result<(), StoreError> {
        validate_drafts(events)?;
        let now = self.now();
        let mut state = self.tables()?;
        if !matches!(
            status,
            ScanStatus::Failed | ScanStatus::Stopped | ScanStatus::Unsupported
        ) {
            return Err(StoreError::Invalid(
                "a scan ends as failed, stopped or unsupported".to_owned(),
            ));
        }
        let report = state
            .report_mut(id)
            .ok_or_else(|| not_found("scan report", id))?;
        if report.status.is_terminal() {
            return Err(conflict("the scan already finished"));
        }
        // The column is limited to 500 characters and a failing scan must still be able to
        // record why, so a longer text is cut, not refused.
        report.status = status;
        report.error = Some(error.chars().take(MAX_SCAN_ERROR_CHARS).collect());
        report.finished_at = Some(at);
        let (component, kind) = (report.component_id, report.kind);
        if kind == ScanKind::Vulnerability {
            // The previous report and its verdict stay; only the status shown changes, so a
            // failed rescan reads as "not scanned" and never as the old clean answer.
            state.security_state_mut(component).scan_status = Some(status);
        }
        state.push_events(events, now);
        Ok(())
    }

    async fn get_report(&self, id: ScanReportId) -> Result<Option<ScanReport>, StoreError> {
        let state = self.tables()?;
        Ok(state.scan_reports.iter().find(|r| r.id == id).cloned())
    }

    async fn state(
        &self,
        component: ComponentId,
    ) -> Result<Option<ComponentSecurityState>, StoreError> {
        Ok(self.tables()?.security_states.get(&component).cloned())
    }

    async fn states(
        &self,
        components: &[ComponentId],
    ) -> Result<Vec<ComponentSecurityState>, StoreError> {
        let state = self.tables()?;
        Ok(components
            .iter()
            .filter_map(|id| state.security_states.get(id).cloned())
            .collect())
    }

    async fn findings(
        &self,
        component: ComponentId,
        filter: &FindingFilter,
        page: &PageRequest,
    ) -> Result<Page<Finding>, StoreError> {
        let state = self.tables()?;
        let rows: Vec<(String, Finding)> = state
            .current_findings(component)
            .map(|(_, findings)| findings)
            .unwrap_or_default()
            .iter()
            .filter(|f| filter.severity_at_least.is_none_or(|min| f.severity >= min))
            .filter(|f| {
                filter
                    .fixable
                    .is_none_or(|want| f.fixed_version.is_some() == want)
            })
            .filter(|f| {
                filter
                    .vuln_id
                    .as_ref()
                    .is_none_or(|id| f.vuln_id.eq_ignore_ascii_case(id))
            })
            .filter(|f| filter.package.as_ref().is_none_or(|p| &f.package == p))
            .map(|f| (finding_key(f), f.clone()))
            .collect();
        drop(state);
        paginate(rows, page)
    }

    async fn effective_summary(
        &self,
        component: ComponentId,
        allowed: &[String],
    ) -> Result<ScanSummary, StoreError> {
        let state = self.tables()?;
        let allowed: HashSet<&str> = allowed.iter().map(String::as_str).collect();
        Ok(state
            .current_findings(component)
            .map_or_else(ScanSummary::default, |(_, findings)| {
                summarize(findings, &allowed)
            }))
    }

    async fn stale_components(
        &self,
        before: OffsetDateTime,
        scope: Option<PolicyScope>,
        limit: u32,
    ) -> Result<Vec<ComponentId>, StoreError> {
        let state = self.tables()?;
        let repositories = state.repositories_in(scope);
        let mut candidates: Vec<_> = state
            .components
            .values()
            .filter(|c| repositories.contains(&c.repository_id))
            .filter(|c| {
                // Never scanned (no state, or only failed attempts) or scanned too long ago.
                state
                    .security_states
                    .get(&c.id)
                    .and_then(|s| s.scanned_at)
                    .is_none_or(|at| at < before)
            })
            .collect();
        candidates.sort_by_key(|c| std::cmp::Reverse((c.created_at, c.id)));
        Ok(candidates
            .into_iter()
            .take(usize::try_from(limit).unwrap_or(usize::MAX))
            .map(|c| c.id)
            .collect())
    }

    async fn set_quarantine(
        &self,
        component: ComponentId,
        quarantine: QuarantineState,
        by: Option<UserId>,
        at: OffsetDateTime,
        events: &[EventDraft],
    ) -> Result<(), StoreError> {
        validate_drafts(events)?;
        let now = self.now();
        let mut state = self.tables()?;
        if !state.components.contains_key(&component) {
            return Err(not_found("component", component));
        }
        let security = state.security_state_mut(component);
        security.quarantine = quarantine;
        if quarantine == QuarantineState::Released {
            security.released_by = by;
            security.released_at = Some(at);
        } else {
            security.released_by = None;
            security.released_at = None;
        }
        state.push_events(events, now);
        let revision = state.bump_security_revision(now);
        drop(state);
        self.notify(InvalidationEvent::Config(revision));
        Ok(())
    }
}

/// Keeps the newest `REPORTS_KEPT` reports per `(component, kind)`; the replace transaction
/// of the real schema deletes the rest, and their findings were deleted before.
fn prune_reports(state: &mut State, component: ComponentId, kind: ScanKind) {
    let ids: Vec<ScanReportId> = state
        .scan_reports
        .iter()
        .filter(|r| r.component_id == component && r.kind == kind)
        .map(|r| r.id)
        .collect();
    if ids.len() <= REPORTS_KEPT {
        return;
    }
    // The current report is always kept, whatever its age, then the newest by queue time.
    let mut ranked: Vec<&ScanReport> = state
        .scan_reports
        .iter()
        .filter(|r| r.component_id == component && r.kind == kind)
        .collect();
    ranked.sort_by_key(|r| std::cmp::Reverse((r.is_current, r.queued_at)));
    let doomed: HashSet<ScanReportId> = ranked
        .into_iter()
        .skip(REPORTS_KEPT)
        .map(|r| r.id)
        .collect();
    state.scan_reports.retain(|r| !doomed.contains(&r.id));
    for id in doomed {
        state.findings.remove(&id);
    }
}

fn highest_index(findings: &[Finding]) -> usize {
    findings
        .iter()
        .map(|f| f.severity.index())
        .max()
        .unwrap_or(0)
}

#[async_trait]
impl SecurityHubRepo for MemStore {
    async fn stats(&self) -> Result<Option<SecurityStats>, StoreError> {
        Ok(self.tables()?.security_stats.clone())
    }

    async fn search(
        &self,
        vuln_id: &str,
        filter: &HubFilter,
        page: &PageRequest,
    ) -> Result<Page<HubRow>, StoreError> {
        let wanted = vuln_id.trim().to_ascii_uppercase();
        if wanted.is_empty() {
            // The index is on the vulnerability id, and an unfiltered scan of every finding
            // is refused rather than served slowly.
            return Err(StoreError::Invalid(
                "a vulnerability id is required".to_owned(),
            ));
        }
        let state = self.tables()?;
        let mut rows: Vec<(String, HubRow)> = Vec::new();
        for report in state
            .scan_reports
            .iter()
            .filter(|r| r.is_current && r.kind == ScanKind::Vulnerability)
        {
            let Some(component) = state.components.get(&report.component_id) else {
                continue;
            };
            let repository = state.repositories.get(&component.repository_id);
            let project = repository.and_then(|r| state.project_of(r));
            if filter.project.is_some_and(|p| Some(p) != project)
                || filter.repository.is_some_and(|r| r != component.repository_id)
                || filter
                    .digest
                    .as_ref()
                    .is_some_and(|d| component.version.as_ref() != Some(d) && &component.name != d)
                // Tags belong to the OCI crate's own table, which the double does not have, so
                // a tag filter can match nothing here.
                || filter.tag.is_some()
            {
                continue;
            }
            for finding in state.findings.get(&report.id).into_iter().flatten() {
                if finding.vuln_id != wanted
                    || filter.severity.is_some_and(|s| s != finding.severity)
                    || filter
                        .package
                        .as_ref()
                        .is_some_and(|p| &finding.package != p)
                    || filter
                        .min_cvss
                        .is_some_and(|min| finding.cvss.is_none_or(|score| score < min))
                    || filter
                        .max_cvss
                        .is_some_and(|max| finding.cvss.is_none_or(|score| score > max))
                {
                    continue;
                }
                let mut coordinate = String::new();
                if let Some(group) = &component.group_id {
                    coordinate.push_str(group);
                    coordinate.push(':');
                }
                coordinate.push_str(&component.name);
                if let Some(version) = &component.version {
                    coordinate.push(':');
                    coordinate.push_str(version);
                }
                let row = HubRow {
                    vuln_id: finding.vuln_id.clone(),
                    severity: finding.severity,
                    cvss: finding.cvss,
                    package: finding.package.clone(),
                    package_version: finding.package_version.clone(),
                    fixed_version: finding.fixed_version.clone(),
                    component_id: component.id,
                    repository: repository.map_or_else(String::new, |r| r.name.as_str().to_owned()),
                    coordinate,
                };
                let key = format!(
                    "{}{}\u{1f}{}\u{1f}{}",
                    5 - row.severity.index(),
                    row.component_id,
                    row.package,
                    row.package_version
                );
                rows.push((key, row));
            }
        }
        drop(state);
        paginate(rows, page)
    }

    async fn refresh_stats(&self, now: OffsetDateTime) -> Result<(), StoreError> {
        let mut state = self.tables()?;
        let mut components_by_severity = [0_u64; 6];
        let mut findings_by_severity = [0_u64; 6];
        let mut per_component: Vec<(ComponentId, u32)> = Vec::new();
        let mut per_vuln: HashMap<String, HashSet<ComponentId>> = HashMap::new();
        for report in state
            .scan_reports
            .iter()
            .filter(|r| r.is_current && r.kind == ScanKind::Vulnerability)
        {
            let findings = state
                .findings
                .get(&report.id)
                .map_or(&[][..], Vec::as_slice);
            components_by_severity[highest_index(findings)] += 1;
            for finding in findings {
                findings_by_severity[finding.severity.index()] += 1;
                per_vuln
                    .entry(finding.vuln_id.clone())
                    .or_default()
                    .insert(report.component_id);
            }
            per_component.push((
                report.component_id,
                u32::try_from(findings.len()).unwrap_or(u32::MAX),
            ));
        }
        per_component.sort_by_key(|(id, count)| (std::cmp::Reverse(*count), *id));
        per_component.truncate(TOP_LIST_LEN);
        let mut top_vulns: Vec<(String, u32)> = per_vuln
            .into_iter()
            .map(|(vuln, components)| (vuln, u32::try_from(components.len()).unwrap_or(u32::MAX)))
            .collect();
        top_vulns.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        top_vulns.truncate(TOP_LIST_LEN);
        state.security_stats = Some(SecurityStats {
            computed_at: now,
            components_by_severity,
            findings_by_severity,
            top_components: per_component,
            top_vulns,
        });
        Ok(())
    }
}

#[async_trait]
impl ExportRepo for MemStore {
    async fn create(&self, job: &ExportJob) -> Result<(), StoreError> {
        let mut state = self.tables()?;
        if state.exports.contains_key(&job.id) {
            return Err(conflict(format!("export {} already exists", job.id)));
        }
        state.exports.insert(job.id, job.clone());
        Ok(())
    }

    async fn get(&self, id: ExportId) -> Result<Option<ExportJob>, StoreError> {
        Ok(self.tables()?.exports.get(&id).cloned())
    }

    async fn finish(
        &self,
        id: ExportId,
        status: TaskRunStatus,
        blob: Option<BlobId>,
    ) -> Result<(), StoreError> {
        let mut state = self.tables()?;
        let job = state
            .exports
            .get_mut(&id)
            .ok_or_else(|| not_found("export", id))?;
        job.status = status;
        job.blob_id = blob;
        Ok(())
    }

    async fn claim_download(
        &self,
        id: ExportId,
        by: Option<UserId>,
        at: OffsetDateTime,
    ) -> Result<bool, StoreError> {
        let mut state = self.tables()?;
        let job = state
            .exports
            .get_mut(&id)
            .ok_or_else(|| not_found("export", id))?;
        // One download, by whoever asked for the export, before it expires.
        let owner_ok = job.requested_by.is_none_or(|owner| by == Some(owner));
        if job.downloaded_at.is_some() || job.expires_at <= at || !owner_ok {
            return Ok(false);
        }
        job.downloaded_at = Some(at);
        Ok(true)
    }

    async fn due_for_purge(
        &self,
        now: OffsetDateTime,
        limit: u32,
    ) -> Result<Vec<ExportJob>, StoreError> {
        let state = self.tables()?;
        let mut due: Vec<ExportJob> = state
            .exports
            .values()
            .filter(|job| job.downloaded_at.is_some() || job.expires_at <= now)
            .cloned()
            .collect();
        due.sort_by_key(|job| (job.created_at, job.id));
        due.truncate(usize::try_from(limit).unwrap_or(usize::MAX));
        Ok(due)
    }

    async fn delete(&self, id: ExportId) -> Result<(), StoreError> {
        let mut state = self.tables()?;
        if state.exports.remove(&id).is_none() {
            return Err(not_found("export", id));
        }
        Ok(())
    }
}

#[async_trait]
impl ScannerDataRepo for MemStore {
    async fn current(
        &self,
        scanner_kind: ScannerKind,
        dataset: &str,
    ) -> Result<Option<ScannerDataSet>, StoreError> {
        let state = self.tables()?;
        Ok(state
            .datasets
            .values()
            .find(|d| d.scanner_kind == scanner_kind && d.dataset == dataset && d.is_current)
            .cloned())
    }

    async fn publish(&self, set: &ScannerDataSet) -> Result<(), StoreError> {
        let mut state = self.tables()?;
        set.validate()
            .map_err(|error| conflict(error.to_string()))?;
        if !state.blobs.contains_key(&set.blob_id) {
            return Err(conflict(format!("blob {} does not exist", set.blob_id)));
        }
        if state.datasets.contains_key(&set.id) {
            return Err(conflict(format!("data set {} already exists", set.id)));
        }
        // One transaction: the previous current version gives the flag up, the new one takes
        // it, so there is never a moment with two (the unique index) or none.
        for other in state.datasets.values_mut() {
            if other.scanner_kind == set.scanner_kind && other.dataset == set.dataset {
                other.is_current = false;
            }
        }
        let mut row = set.clone();
        row.is_current = true;
        state.datasets.insert(row.id, row);
        Ok(())
    }

    async fn prunable(
        &self,
        scanner_kind: ScannerKind,
        dataset: &str,
    ) -> Result<Vec<ScannerDataSet>, StoreError> {
        let state = self.tables()?;
        let mut versions: Vec<&ScannerDataSet> = state
            .datasets
            .values()
            .filter(|d| d.scanner_kind == scanner_kind && d.dataset == dataset)
            .collect();
        // Newest first by fetch time (the current one is the newest published): the newest
        // two stay, the rest can go.
        versions.sort_by_key(|d| std::cmp::Reverse((d.is_current, d.fetched_at, d.id)));
        Ok(versions.into_iter().skip(2).cloned().collect())
    }

    async fn delete(&self, id: ScannerDataSetId) -> Result<(), StoreError> {
        let mut state = self.tables()?;
        if state.datasets.remove(&id).is_none() {
            return Err(not_found("scanner data set", id));
        }
        Ok(())
    }

    async fn create_scan_token(
        &self,
        token_hash: &str,
        scan: ScanReportId,
        repository: RepositoryId,
        expires_at: OffsetDateTime,
    ) -> Result<(), StoreError> {
        let mut state = self.tables()?;
        // The column is a SHA-256 hex digest; a longer or other text means the caller stored
        // the token itself, which must never happen.
        let is_digest = token_hash.len() == 64
            && token_hash
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b));
        if !is_digest {
            return Err(conflict(
                "a scan token is stored as 64 lowercase hex digits",
            ));
        }
        if state.scan_tokens.contains_key(token_hash) {
            return Err(conflict("scan token already exists"));
        }
        state
            .scan_tokens
            .insert(token_hash.to_owned(), (scan, repository, expires_at));
        Ok(())
    }

    async fn find_scan_token(
        &self,
        token_hash: &str,
        now: OffsetDateTime,
    ) -> Result<Option<(ScanReportId, RepositoryId)>, StoreError> {
        let state = self.tables()?;
        Ok(state
            .scan_tokens
            .get(token_hash)
            .filter(|(_, _, expires_at)| *expires_at > now)
            .map(|(scan, repository, _)| (*scan, *repository)))
    }
}
