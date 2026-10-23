//! Scanners, CVE exceptions, scan and trust policies and trust material.
//!
//! Every write to a policy, an exception, trust material and a quarantine change bumps
//! `security.revision` and publishes `InvalidationEvent::Config`, in the same step as the
//! write, as the real schema does in its transaction.

use async_trait::async_trait;
use rustryx_core::error::StoreError;
use rustryx_core::ids::{
    ComponentId, CveAllowlistEntryId, ProjectId, RepositoryId, ScannerId, TrustMaterialId, UserId,
};
use rustryx_core::lifecycle::PolicyScope;
use rustryx_core::ports::{
    CveAllowlistRepo, InvalidationEvent, Page, PageRequest, ScannerRepo, SecurityPolicyRepo,
    TrustRepo,
};
use rustryx_core::scan::{
    AllowlistMode, AllowlistScope, CveAllowlistEntry, ProbeResult, ScanKind, ScanPolicy,
    ScannerRecord, TrustMaterial, TrustMaterialKind, TrustPolicy,
};
use time::OffsetDateTime;

use super::paging::paginate;
use super::state::State;
use super::support::bump_version;
use super::{MemStore, conflict, not_found};

impl State {
    /// The foreign key behind a policy scope: the project or repository must exist.
    fn policy_scope_exists(&self, scope: PolicyScope) -> bool {
        match scope {
            PolicyScope::System => true,
            PolicyScope::Project(id) => self.projects.contains_key(&id),
            PolicyScope::Repository(id) => self.repositories.contains_key(&id),
        }
    }

    /// `uq_scanners_default_vuln`: at most one default among the scanners that can find
    /// vulnerabilities.
    fn default_scanner_clash(&self, scanner: &ScannerRecord) -> bool {
        scanner.is_default
            && scanner.capabilities.contains(&ScanKind::Vulnerability)
            && self.scanners.values().any(|other| {
                other.id != scanner.id
                    && other.is_default
                    && other.capabilities.contains(&ScanKind::Vulnerability)
            })
    }

    fn allowlist_scope_exists(&self, scope: AllowlistScope) -> bool {
        match scope {
            AllowlistScope::System => true,
            AllowlistScope::Project(id) => self.projects.contains_key(&id),
            AllowlistScope::Component(id) => self.components.contains_key(&id),
        }
    }

    fn allowlist_clash(&self, entry: &CveAllowlistEntry) -> bool {
        self.allowlist.values().any(|other| {
            other.id != entry.id && other.scope == entry.scope && other.vuln_id == entry.vuln_id
        })
    }

    /// The policy of exactly one scope, with the scope's own foreign key and uniqueness rules
    /// shared by scan and trust policies: `Some(expected)` replaces the stored row of the
    /// scope and needs its version, `None` only inserts.
    fn upsert_decision(
        existing_version: Option<i64>,
        expected_version: Option<i64>,
    ) -> Result<Option<i64>, StoreError> {
        match (existing_version, expected_version) {
            (Some(current), Some(expected)) => bump_version(current, expected).map(Some),
            (Some(_), None) => Err(conflict("the scope already has a policy")),
            (None, Some(_)) => Err(conflict("stale version: the scope has no policy")),
            (None, None) => Ok(None),
        }
    }
}

#[async_trait]
impl ScannerRepo for MemStore {
    async fn create(&self, scanner: &ScannerRecord) -> Result<(), StoreError> {
        let mut state = self.tables()?;
        scanner
            .validate()
            .map_err(|error| conflict(error.to_string()))?;
        let taken = state
            .scanners
            .values()
            .any(|s| s.id == scanner.id || s.name == scanner.name);
        if taken || state.default_scanner_clash(scanner) {
            return Err(conflict(format!("scanner {} already exists", scanner.name)));
        }
        state.scanners.insert(scanner.id, scanner.clone());
        Ok(())
    }

    async fn get(&self, id: ScannerId) -> Result<Option<ScannerRecord>, StoreError> {
        Ok(self.tables()?.scanners.get(&id).cloned())
    }

    async fn list(&self) -> Result<Vec<ScannerRecord>, StoreError> {
        let mut all: Vec<ScannerRecord> = self.tables()?.scanners.values().cloned().collect();
        all.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(all)
    }

    async fn update(
        &self,
        scanner: &ScannerRecord,
        expected_version: i64,
    ) -> Result<(), StoreError> {
        let mut state = self.tables()?;
        let existing = state
            .scanners
            .get(&scanner.id)
            .ok_or_else(|| not_found("scanner", scanner.id))?;
        let version = bump_version(existing.version, expected_version)?;
        scanner
            .validate()
            .map_err(|error| conflict(error.to_string()))?;
        let mut next = scanner.clone();
        next.version = version;
        // The probe result and the creation time are not form fields: an edit made from a
        // stale read must not erase the last probe.
        next.last_probe.clone_from(&existing.last_probe);
        next.created_at = existing.created_at;
        let name_taken = state
            .scanners
            .values()
            .any(|s| s.id != next.id && s.name == next.name);
        if name_taken || state.default_scanner_clash(&next) {
            return Err(conflict(format!("scanner {} already exists", next.name)));
        }
        state.scanners.insert(next.id, next);
        Ok(())
    }

    async fn delete(&self, id: ScannerId) -> Result<(), StoreError> {
        let mut state = self.tables()?;
        if !state.scanners.contains_key(&id) {
            return Err(not_found("scanner", id));
        }
        if state
            .scan_policies
            .values()
            .any(|policy| policy.scanner_id == Some(id))
        {
            return Err(conflict("scanner is still referenced by a scan policy"));
        }
        state.scanners.remove(&id);
        // `scan_reports.scanner_id` is `ON DELETE SET NULL`: the history stays, unlinked.
        for report in &mut state.scan_reports {
            if report.scanner_id == Some(id) {
                report.scanner_id = None;
            }
        }
        Ok(())
    }

    async fn set_default(&self, id: ScannerId) -> Result<(), StoreError> {
        let mut state = self.tables()?;
        let chosen = state
            .scanners
            .get(&id)
            .cloned()
            .ok_or_else(|| not_found("scanner", id))?;
        // Default of its scan kind and format family: every other scanner that shares a
        // capability and covers an overlapping set of formats (an empty list covers all)
        // gives the flag up in the same step.
        let overlaps = |other: &ScannerRecord| {
            other
                .capabilities
                .iter()
                .any(|c| chosen.capabilities.contains(c))
                && (other.formats.is_empty()
                    || chosen.formats.is_empty()
                    || other.formats.iter().any(|f| chosen.formats.contains(f)))
        };
        for other in state.scanners.values_mut() {
            if other.id != id && overlaps(other) {
                other.is_default = false;
            }
        }
        if let Some(stored) = state.scanners.get_mut(&id) {
            stored.is_default = true;
        }
        Ok(())
    }

    async fn record_probe(&self, id: ScannerId, probe: &ProbeResult) -> Result<(), StoreError> {
        let mut state = self.tables()?;
        let scanner = state
            .scanners
            .get_mut(&id)
            .ok_or_else(|| not_found("scanner", id))?;
        scanner.last_probe = Some(probe.clone());
        Ok(())
    }
}

#[async_trait]
impl CveAllowlistRepo for MemStore {
    async fn create(&self, entry: &CveAllowlistEntry) -> Result<(), StoreError> {
        let now = self.now();
        let mut state = self.tables()?;
        entry
            .validate()
            .map_err(|error| conflict(error.to_string()))?;
        if !state.allowlist_scope_exists(entry.scope) {
            return Err(conflict("the scope of the exception does not exist"));
        }
        if state.allowlist.contains_key(&entry.id) || state.allowlist_clash(entry) {
            return Err(conflict(format!(
                "{} is already allowed in this scope",
                entry.vuln_id
            )));
        }
        state.allowlist.insert(entry.id, entry.clone());
        let revision = state.bump_security_revision(now);
        drop(state);
        self.notify(InvalidationEvent::Config(revision));
        Ok(())
    }

    async fn list(
        &self,
        scope: AllowlistScope,
        include_expired: bool,
        page: &PageRequest,
    ) -> Result<Page<CveAllowlistEntry>, StoreError> {
        let now = self.now();
        let rows: Vec<(String, CveAllowlistEntry)> = self
            .tables()?
            .allowlist
            .values()
            .filter(|entry| entry.scope == scope)
            .filter(|entry| include_expired || entry.is_active_at(now))
            .map(|entry| {
                (
                    format!("{}\u{1f}{}", entry.vuln_id, entry.id),
                    entry.clone(),
                )
            })
            .collect();
        paginate(rows, page)
    }

    async fn update(
        &self,
        entry: &CveAllowlistEntry,
        expected_version: i64,
    ) -> Result<(), StoreError> {
        let now = self.now();
        let mut state = self.tables()?;
        let existing = state
            .allowlist
            .get(&entry.id)
            .ok_or_else(|| not_found("cve exception", entry.id))?;
        let version = bump_version(existing.version, expected_version)?;
        entry
            .validate()
            .map_err(|error| conflict(error.to_string()))?;
        let mut next = entry.clone();
        next.version = version;
        // The scope and the author do not change: moving an exception between scopes would be
        // a new exception with a new audit trail.
        next.scope = existing.scope;
        next.created_by = existing.created_by;
        next.created_at = existing.created_at;
        let expiry_changed = existing.expires_at != next.expires_at;
        if state.allowlist_clash(&next) {
            return Err(conflict(format!(
                "{} is already allowed in this scope",
                next.vuln_id
            )));
        }
        let id = next.id;
        state.allowlist.insert(id, next);
        if expiry_changed {
            // A new expiry date re-arms the one time audit of the expiry.
            state.allowlist_audited.remove(&id);
        }
        let revision = state.bump_security_revision(now);
        drop(state);
        self.notify(InvalidationEvent::Config(revision));
        Ok(())
    }

    async fn delete(&self, id: CveAllowlistEntryId) -> Result<(), StoreError> {
        let now = self.now();
        let mut state = self.tables()?;
        if state.allowlist.remove(&id).is_none() {
            return Err(not_found("cve exception", id));
        }
        state.allowlist_audited.remove(&id);
        let revision = state.bump_security_revision(now);
        drop(state);
        self.notify(InvalidationEvent::Config(revision));
        Ok(())
    }

    async fn copy_system_to_project(
        &self,
        project: ProjectId,
        by: Option<UserId>,
    ) -> Result<u64, StoreError> {
        let now = self.now();
        let mut state = self.tables()?;
        if !state.projects.contains_key(&project) {
            return Err(not_found("project", project));
        }
        // A snapshot of what is in force: an expired entry would be copied dead, and a
        // vulnerability the project already allows keeps its own reason and expiry.
        let mut copies: Vec<CveAllowlistEntry> = state
            .allowlist
            .values()
            .filter(|entry| entry.scope == AllowlistScope::System && entry.is_active_at(now))
            .filter(|entry| {
                !state.allowlist.values().any(|own| {
                    own.scope == AllowlistScope::Project(project) && own.vuln_id == entry.vuln_id
                })
            })
            .map(|entry| {
                let mut copy = CveAllowlistEntry::new(
                    CveAllowlistEntryId::generate(),
                    AllowlistScope::Project(project),
                    entry.vuln_id.clone(),
                    entry.reason.clone(),
                    now,
                );
                copy.created_by = by;
                copy.expires_at = entry.expires_at;
                copy
            })
            .collect();
        copies.sort_by(|a, b| a.vuln_id.cmp(&b.vuln_id));
        let copy_count = u64::try_from(copies.len()).unwrap_or(u64::MAX);
        for copy in copies {
            state.allowlist.insert(copy.id, copy);
        }
        let revision = (copy_count > 0).then(|| state.bump_security_revision(now));
        drop(state);
        if let Some(revision) = revision {
            self.notify(InvalidationEvent::Config(revision));
        }
        Ok(copy_count)
    }

    async fn active_ids(
        &self,
        component: ComponentId,
        mode: AllowlistMode,
    ) -> Result<Vec<String>, StoreError> {
        let now = self.now();
        let state = self.tables()?;
        let Some(stored) = state.components.get(&component) else {
            return Ok(Vec::new());
        };
        let project = state
            .repositories
            .get(&stored.repository_id)
            .and_then(|repository| state.project_of(repository));
        // The component's own entries always apply. The wider list is the project's when the
        // mode says so (and the component has a project), the system's otherwise.
        let wider = match mode {
            AllowlistMode::Project => project.map(AllowlistScope::Project),
            AllowlistMode::System => Some(AllowlistScope::System),
        };
        let mut ids: Vec<String> = state
            .allowlist
            .values()
            .filter(|entry| entry.is_active_at(now))
            .filter(|entry| {
                entry.scope == AllowlistScope::Component(component) || Some(entry.scope) == wider
            })
            .map(|entry| entry.vuln_id.clone())
            .collect();
        ids.sort();
        ids.dedup();
        Ok(ids)
    }

    async fn take_expired_unaudited(
        &self,
        now: OffsetDateTime,
        limit: u32,
    ) -> Result<Vec<CveAllowlistEntry>, StoreError> {
        let mut state = self.tables()?;
        let mut due: Vec<CveAllowlistEntry> = state
            .allowlist
            .values()
            .filter(|entry| entry.expires_at.is_some() && !entry.is_active_at(now))
            .filter(|entry| !state.allowlist_audited.contains(&entry.id))
            .cloned()
            .collect();
        due.sort_by_key(|entry| (entry.expires_at, entry.id));
        due.truncate(usize::try_from(limit).unwrap_or(usize::MAX));
        // Marked in the same call, so a second call never returns the same entry.
        for entry in &due {
            state.allowlist_audited.insert(entry.id);
        }
        Ok(due)
    }
}

/// Field wise merge of scan policies, most specific last: a field that the more specific
/// policy leaves unset keeps the less specific value.
fn merged(base: ScanPolicy, over: &ScanPolicy) -> ScanPolicy {
    let mut merged = base;
    merged.auto_scan = over.auto_scan.or(merged.auto_scan);
    merged.scanner_id = over.scanner_id.or(merged.scanner_id);
    merged.gate = over.gate.or(merged.gate);
    merged.quarantine = over.quarantine.or(merged.quarantine);
    merged.allowlist_mode = over.allowlist_mode.or(merged.allowlist_mode);
    merged.health_check = over.health_check.or(merged.health_check);
    merged.sbom_on_push = over.sbom_on_push.or(merged.sbom_on_push);
    merged.id = over.id;
    merged.version = over.version;
    merged
}

#[async_trait]
impl SecurityPolicyRepo for MemStore {
    async fn upsert_scan_policy(
        &self,
        policy: &ScanPolicy,
        expected_version: Option<i64>,
    ) -> Result<(), StoreError> {
        let now = self.now();
        let mut state = self.tables()?;
        policy
            .validate()
            .map_err(|error| conflict(error.to_string()))?;
        if !state.policy_scope_exists(policy.scope) {
            return Err(conflict("the scope of the policy does not exist"));
        }
        let existing = state
            .scan_policies
            .values()
            .find(|p| p.scope == policy.scope)
            .cloned();
        let bumped =
            State::upsert_decision(existing.as_ref().map(|p| p.version), expected_version)?;
        let mut next = policy.clone();
        if let (Some(version), Some(current)) = (bumped, &existing) {
            next.version = version;
            // The row of a scope keeps its identity: the unique index is on the scope.
            next.id = current.id;
        } else if state.scan_policies.contains_key(&next.id) {
            return Err(conflict(format!("scan policy {} already exists", next.id)));
        }
        state.scan_policies.insert(next.id, next);
        let revision = state.bump_security_revision(now);
        drop(state);
        self.notify(InvalidationEvent::Config(revision));
        Ok(())
    }

    async fn get_scan_policy(&self, scope: PolicyScope) -> Result<Option<ScanPolicy>, StoreError> {
        Ok(self
            .tables()?
            .scan_policies
            .values()
            .find(|p| p.scope == scope)
            .cloned())
    }

    async fn effective_for(&self, repository: RepositoryId) -> Result<ScanPolicy, StoreError> {
        let state = self.tables()?;
        let stored = state
            .repositories
            .get(&repository)
            .ok_or_else(|| not_found("repository", repository))?;
        let find = |scope: PolicyScope| state.scan_policies.values().find(|p| p.scope == scope);
        // A system row is seeded by the real migration. Without one the built-in default
        // applies, so the answer is always complete.
        let system = find(PolicyScope::System).cloned().unwrap_or_else(|| {
            ScanPolicy::system_default(rustryx_core::ids::SecurityPolicyId::generate())
        });
        let mut result = system;
        if let Some(project) = state
            .project_of(stored)
            .and_then(|p| find(PolicyScope::Project(p)))
        {
            result = merged(result, project);
        }
        if let Some(own) = find(PolicyScope::Repository(repository)) {
            result = merged(result, own);
        }
        result.scope = PolicyScope::Repository(repository);
        Ok(result)
    }

    async fn upsert_trust_policy(
        &self,
        policy: &TrustPolicy,
        expected_version: Option<i64>,
    ) -> Result<(), StoreError> {
        let now = self.now();
        let mut state = self.tables()?;
        policy
            .validate()
            .map_err(|error| conflict(error.to_string()))?;
        if !state.policy_scope_exists(policy.scope) {
            return Err(conflict("the scope of the policy does not exist"));
        }
        // The material ids sit inside a JSON column in the real schema, so no foreign key
        // checks them; the repository does, in the same transaction.
        let listed = policy
            .cosign_keys
            .iter()
            .chain(&policy.cosign_roots)
            .chain(&policy.notation_stores);
        if let Some(missing) = listed
            .into_iter()
            .find(|id| !state.trust_materials.contains_key(id))
        {
            return Err(conflict(format!("trust material {missing} does not exist")));
        }
        let existing = state
            .trust_policies
            .values()
            .find(|p| p.scope == policy.scope)
            .cloned();
        let bumped =
            State::upsert_decision(existing.as_ref().map(|p| p.version), expected_version)?;
        let mut next = policy.clone();
        if let (Some(version), Some(current)) = (bumped, &existing) {
            next.version = version;
            next.id = current.id;
        } else if state.trust_policies.contains_key(&next.id) {
            return Err(conflict(format!("trust policy {} already exists", next.id)));
        }
        state.trust_policies.insert(next.id, next);
        let revision = state.bump_security_revision(now);
        drop(state);
        self.notify(InvalidationEvent::Config(revision));
        Ok(())
    }

    async fn get_trust_policy(
        &self,
        scope: PolicyScope,
    ) -> Result<Option<TrustPolicy>, StoreError> {
        Ok(self
            .tables()?
            .trust_policies
            .values()
            .find(|p| p.scope == scope)
            .cloned())
    }

    async fn effective_trust_for(
        &self,
        repository: RepositoryId,
    ) -> Result<Option<TrustPolicy>, StoreError> {
        let state = self.tables()?;
        let stored = state
            .repositories
            .get(&repository)
            .ok_or_else(|| not_found("repository", repository))?;
        let find = |scope: PolicyScope| state.trust_policies.values().find(|p| p.scope == scope);
        // A trust policy is one unit (which signatures, which keys): the most specific scope
        // wins as a whole, there is no field wise merge.
        Ok(find(PolicyScope::Repository(repository))
            .or_else(|| {
                state
                    .project_of(stored)
                    .and_then(|project| find(PolicyScope::Project(project)))
            })
            .or_else(|| find(PolicyScope::System))
            .cloned())
    }
}

#[async_trait]
impl TrustRepo for MemStore {
    async fn create(&self, material: &TrustMaterial) -> Result<(), StoreError> {
        let now = self.now();
        let mut state = self.tables()?;
        material
            .validate()
            .map_err(|error| conflict(error.to_string()))?;
        let taken = state.trust_materials.values().any(|m| {
            m.id == material.id
                || m.name == material.name
                || (m.kind == material.kind && m.fingerprint_sha256 == material.fingerprint_sha256)
        });
        if taken {
            return Err(conflict(format!(
                "trust material {} already exists",
                material.name
            )));
        }
        state.trust_materials.insert(material.id, material.clone());
        let revision = state.bump_security_revision(now);
        drop(state);
        self.notify(InvalidationEvent::Config(revision));
        Ok(())
    }

    async fn get(&self, id: TrustMaterialId) -> Result<Option<TrustMaterial>, StoreError> {
        Ok(self.tables()?.trust_materials.get(&id).cloned())
    }

    async fn list(
        &self,
        kind: Option<TrustMaterialKind>,
    ) -> Result<Vec<TrustMaterial>, StoreError> {
        let mut all: Vec<TrustMaterial> = self
            .tables()?
            .trust_materials
            .values()
            .filter(|m| kind.is_none_or(|k| m.kind == k))
            .cloned()
            .collect();
        all.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(all)
    }

    async fn delete(&self, id: TrustMaterialId) -> Result<(), StoreError> {
        let now = self.now();
        let mut state = self.tables()?;
        if !state.trust_materials.contains_key(&id) {
            return Err(not_found("trust material", id));
        }
        let listed = state.trust_policies.values().any(|policy| {
            policy
                .cosign_keys
                .iter()
                .chain(&policy.cosign_roots)
                .chain(&policy.notation_stores)
                .any(|listed| *listed == id)
        });
        if listed {
            return Err(conflict("trust material is listed by a trust policy"));
        }
        state.trust_materials.remove(&id);
        let revision = state.bump_security_revision(now);
        drop(state);
        self.notify(InvalidationEvent::Config(revision));
        Ok(())
    }
}
