//! The lifecycle engine's reads and deletions, run history, garbage collection history and
//! the daily download counters.

use std::collections::HashSet;
use std::time::Duration;

use async_trait::async_trait;
use futures::StreamExt;
use futures::stream::BoxStream;
use rustryx_core::error::StoreError;
use rustryx_core::ids::{AssetId, ComponentId, GcRunId, LifecycleRunId};
use rustryx_core::lifecycle::{
    GcCounts, GcRun, GcSample, ItemDecision, LifecycleCounts, LifecycleItem, LifecycleKind,
    LifecycleRun, MAX_LIFECYCLE_ITEMS_PER_RUN,
};
use rustryx_core::model::{Component, WriteOptions};
use rustryx_core::ports::{
    Candidate, DeleteResult, DownloadStatsRepo, GcRunRepo, LifecycleRepo, LifecycleRunRepo,
    LifecycleScope, Page, PageRequest, PolicyRef,
};
use rustryx_core::tasks::{TaskRunStatus, TaskTrigger};
use time::{Date, OffsetDateTime};

use super::outbox::validate_drafts;
use super::paging::paginate;
use super::publish::settle_after_delete;
use super::state::{GcRunRow, State};
use super::{MemStore, conflict, not_found};

/// Most components one `apply_deletions` batch may carry.
const MAX_DELETION_BATCH: usize = 500;
/// Most sample rows a garbage collection run keeps.
const MAX_GC_SAMPLES: usize = 1000;
/// Most days of daily counters a read returns.
const MAX_STATS_DAYS: u32 = 90;

impl State {
    /// Distinct blob bytes the assets of a component hold.
    fn component_bytes(&self, id: ComponentId) -> u64 {
        let mut seen = HashSet::new();
        self.assets
            .values()
            .filter(|asset| asset.component_id == Some(id))
            .filter(|asset| seen.insert(asset.blob_id))
            .fold(0_u64, |sum, asset| sum.saturating_add(asset.size_bytes))
    }

    fn candidate_of(&self, component: &Component) -> Candidate {
        let repository = self.repositories.get(&component.repository_id).map_or_else(
            || component.repository_id.to_string(),
            |r| r.name.as_str().to_owned(),
        );
        let mut candidate = Candidate::new(
            component.id,
            component.repository_id,
            repository,
            component.name.clone(),
            component.created_at,
        );
        candidate.group_id.clone_from(&component.group_id);
        candidate.version.clone_from(&component.version);
        candidate.size_bytes = self.component_bytes(component.id);
        candidate.pulled_at = component.last_downloaded_at;
        candidate
    }

    fn next_run_seq(&mut self) -> u64 {
        self.run_seq += 1;
        self.run_seq
    }
}

/// Key whose ascending order is the engine's order `(repository_id, group_id, name,
/// published_at DESC, id)`. Fixed width pieces and a separator below every printable
/// character keep a string comparison equal to the tuple comparison.
fn candidate_key(candidate: &Candidate) -> String {
    const SHIFT: i128 = 1 << 100;
    let published = u128::try_from(
        candidate
            .published_at
            .unix_timestamp_nanos()
            .saturating_add(SHIFT),
    )
    .unwrap_or(0);
    format!(
        "{}\u{1f}{}\u{1f}{}\u{1f}{:039}\u{1f}{}",
        candidate.repository_id,
        candidate.group_id.as_deref().unwrap_or_default(),
        candidate.name,
        u128::MAX - published,
        candidate.component_id
    )
}

#[async_trait]
impl LifecycleRepo for MemStore {
    async fn candidates(
        &self,
        scope: LifecycleScope,
        page: &PageRequest,
    ) -> Result<Page<Candidate>, StoreError> {
        let state = self.tables()?;
        let repositories: HashSet<_> = match scope {
            LifecycleScope::Repository(id) => {
                if !state.repositories.contains_key(&id) {
                    return Err(not_found("repository", id));
                }
                HashSet::from([id])
            }
            LifecycleScope::Project(id) => {
                if !state.projects.contains_key(&id) {
                    return Err(not_found("project", id));
                }
                state
                    .repositories
                    .values()
                    .filter(|r| state.project_of(r) == Some(id))
                    .map(|r| r.id)
                    .collect()
            }
        };
        let rows: Vec<(String, Candidate)> = state
            .components
            .values()
            .filter(|c| repositories.contains(&c.repository_id))
            .map(|c| state.candidate_of(c))
            .map(|candidate| (candidate_key(&candidate), candidate))
            .collect();
        drop(state);
        paginate(rows, page)
    }

    async fn apply_deletions(
        &self,
        run: LifecycleRunId,
        batch: &[ComponentId],
        options: &WriteOptions,
    ) -> Result<Vec<DeleteResult>, StoreError> {
        if batch.len() > MAX_DELETION_BATCH {
            return Err(StoreError::Invalid(
                "a deletion batch has at most 500 components".to_owned(),
            ));
        }
        validate_drafts(&options.events)?;
        let now = self.now();
        let mut state = self.tables()?;
        if !state.lifecycle_runs.iter().any(|(_, r)| r.id == run) {
            return Err(conflict(format!("lifecycle run {run} does not exist")));
        }
        // `RX007` aborts the whole transaction, a protected component does not.
        state
            .check_writable()
            .map_err(|violation| conflict(violation.to_string()))?;
        let mut results = Vec::with_capacity(batch.len());
        let mut items = Vec::new();
        let first_seq = state
            .lifecycle_items
            .get(&run)
            .map_or(0, |items| u64::try_from(items.len()).unwrap_or(u64::MAX));
        for id in batch {
            let Some(component) = state.components.get(id).cloned() else {
                results.push(DeleteResult::Failed);
                continue;
            };
            let repository = state.repositories.get(&component.repository_id).cloned();
            let candidate = state.candidate_of(&component);
            let verdict = state.guard_component_delete(
                repository.as_ref(),
                &component,
                options.override_immutable,
                now,
            );
            let (result, decision, reason) = match verdict {
                Ok(()) => {
                    state.components.remove(id);
                    state.remove_component_rows(*id);
                    (
                        DeleteResult::Deleted {
                            bytes: candidate.size_bytes,
                        },
                        ItemDecision::Delete,
                        "applied".to_owned(),
                    )
                }
                Err(rustryx_core::PolicyViolation::Immutable { rule }) => (
                    DeleteResult::Skipped(ItemDecision::ProtectedImmutable),
                    ItemDecision::ProtectedImmutable,
                    rule.to_string(),
                ),
                Err(rustryx_core::PolicyViolation::Held { hold }) => (
                    DeleteResult::Skipped(ItemDecision::ProtectedHold),
                    ItemDecision::ProtectedHold,
                    hold.to_string(),
                ),
                // The guards of a component delete raise no other violation.
                Err(_) => (
                    DeleteResult::Failed,
                    ItemDecision::DeleteFailed,
                    "failed".to_owned(),
                ),
            };
            let mut item = LifecycleItem {
                run_id: run,
                seq: first_seq + u64::try_from(items.len()).unwrap_or(u64::MAX),
                repository: candidate.repository.clone(),
                component_id: *id,
                coordinate: coordinate_of(&candidate),
                tags: Vec::new(),
                size_bytes: candidate.size_bytes,
                decision,
                reason,
            };
            item.tags.clone_from(&candidate.tags);
            items.push(item);
            results.push(result);
        }
        state.lifecycle_items.entry(run).or_default().extend(items);
        state.push_events(&options.events, now);
        let scopes: Vec<_> = state
            .repositories
            .values()
            .flat_map(|repository| state.scopes_of(repository))
            .collect();
        settle_after_delete(&mut state, &scopes);
        Ok(results)
    }
}

fn coordinate_of(candidate: &Candidate) -> String {
    let mut text = String::new();
    if let Some(group) = &candidate.group_id {
        text.push_str(group);
        text.push(':');
    }
    text.push_str(&candidate.name);
    if let Some(version) = &candidate.version {
        text.push(':');
        text.push_str(version);
    }
    text
}

fn newest_first_key(seq: u64) -> String {
    format!("{:020}", u64::MAX - seq)
}

fn run_matches(run: &LifecycleRun, kind: Option<LifecycleKind>, policy: Option<PolicyRef>) -> bool {
    kind.is_none_or(|k| run.kind == k)
        && policy.is_none_or(|p| match p {
            PolicyRef::Cleanup(id, repository) => {
                run.cleanup_policy_id == Some(id)
                    && repository.is_none_or(|r| run.repository_id == Some(r))
            }
            PolicyRef::Retention(id) => run.retention_policy_id == Some(id),
        })
}

#[async_trait]
impl LifecycleRunRepo for MemStore {
    async fn create(&self, run: &LifecycleRun) -> Result<(), StoreError> {
        let mut state = self.tables()?;
        if state.lifecycle_runs.iter().any(|(_, r)| r.id == run.id) {
            return Err(conflict(format!("lifecycle run {} already exists", run.id)));
        }
        let seq = state.next_run_seq();
        state.lifecycle_runs.push((seq, run.clone()));
        Ok(())
    }

    async fn update_progress(
        &self,
        id: LifecycleRunId,
        counts: &LifecycleCounts,
    ) -> Result<(), StoreError> {
        let mut state = self.tables()?;
        let (_, run) = state
            .lifecycle_runs
            .iter_mut()
            .find(|(_, r)| r.id == id)
            .ok_or_else(|| not_found("lifecycle run", id))?;
        run.counts = counts.clone();
        Ok(())
    }

    async fn finish(
        &self,
        id: LifecycleRunId,
        status: TaskRunStatus,
        counts: &LifecycleCounts,
        truncated: bool,
        at: OffsetDateTime,
    ) -> Result<(), StoreError> {
        let mut state = self.tables()?;
        let (_, run) = state
            .lifecycle_runs
            .iter_mut()
            .find(|(_, r)| r.id == id)
            .ok_or_else(|| not_found("lifecycle run", id))?;
        run.status = status;
        run.counts = counts.clone();
        run.truncated = truncated;
        run.finished_at = Some(at);
        Ok(())
    }

    async fn get(&self, id: LifecycleRunId) -> Result<Option<LifecycleRun>, StoreError> {
        let state = self.tables()?;
        Ok(state
            .lifecycle_runs
            .iter()
            .find(|(_, r)| r.id == id)
            .map(|(_, r)| r.clone()))
    }

    async fn list(
        &self,
        kind: Option<LifecycleKind>,
        policy: Option<PolicyRef>,
        page: &PageRequest,
    ) -> Result<Page<LifecycleRun>, StoreError> {
        let rows: Vec<(String, LifecycleRun)> = self
            .tables()?
            .lifecycle_runs
            .iter()
            .filter(|(_, run)| run_matches(run, kind, policy))
            .map(|(seq, run)| (newest_first_key(*seq), run.clone()))
            .collect();
        paginate(rows, page)
    }

    async fn save_items(
        &self,
        run: LifecycleRunId,
        items: &[LifecycleItem],
    ) -> Result<bool, StoreError> {
        let mut state = self.tables()?;
        if !state.lifecycle_runs.iter().any(|(_, r)| r.id == run) {
            return Err(not_found("lifecycle run", run));
        }
        let stored = state.lifecycle_items.entry(run).or_default();
        // The primary key `(run, seq)`, checked against a set so that a big batch stays linear.
        let mut seen: HashSet<u64> = stored.iter().map(|existing| existing.seq).collect();
        let mut cut = false;
        for item in items {
            if !seen.insert(item.seq) {
                return Err(conflict(format!(
                    "item {} of the run already exists",
                    item.seq
                )));
            }
            if stored.len() >= MAX_LIFECYCLE_ITEMS_PER_RUN {
                cut = true;
                continue;
            }
            let mut row = item.clone();
            row.run_id = run;
            stored.push(row);
        }
        Ok(cut)
    }

    async fn list_items(
        &self,
        run: LifecycleRunId,
        page: &PageRequest,
    ) -> Result<Page<LifecycleItem>, StoreError> {
        let state = self.tables()?;
        if !state.lifecycle_runs.iter().any(|(_, r)| r.id == run) {
            return Err(not_found("lifecycle run", run));
        }
        let rows: Vec<(String, LifecycleItem)> = state
            .lifecycle_items
            .get(&run)
            .into_iter()
            .flatten()
            .map(|item| (format!("{:020}", item.seq), item.clone()))
            .collect();
        drop(state);
        paginate(rows, page)
    }

    fn stream_items(
        &self,
        run: LifecycleRunId,
    ) -> BoxStream<'static, Result<LifecycleItem, StoreError>> {
        match self.tables() {
            Err(error) => futures::stream::once(async move { Err(error) }).boxed(),
            Ok(state) => {
                let mut items: Vec<LifecycleItem> =
                    state.lifecycle_items.get(&run).cloned().unwrap_or_default();
                items.sort_by_key(|item| item.seq);
                futures::stream::iter(items.into_iter().map(Ok)).boxed()
            }
        }
    }

    async fn last_dry_run(
        &self,
        policy: PolicyRef,
        policy_version: i64,
        max_age: Duration,
    ) -> Result<Option<LifecycleRun>, StoreError> {
        let now = self.now();
        let state = self.tables()?;
        // A dry run that failed or was stopped proves nothing about the policy, so only a
        // succeeded one opens the gate.
        let newest = state
            .lifecycle_runs
            .iter()
            .filter(|(_, run)| run.dry_run && run.policy_version == policy_version)
            .filter(|(_, run)| run.status == TaskRunStatus::Succeeded && run.finished_at.is_some())
            .filter(|(_, run)| run_matches(run, None, Some(policy)))
            .filter(|(_, run)| {
                run.started_at.is_some_and(|started| {
                    Duration::try_from(now - started).is_ok_and(|age| age < max_age)
                })
            })
            .max_by_key(|(seq, _)| *seq);
        Ok(newest.map(|(_, run)| run.clone()))
    }

    async fn purge(&self, finished_before: OffsetDateTime) -> Result<u64, StoreError> {
        let mut state = self.tables()?;
        let doomed: Vec<LifecycleRunId> = state
            .lifecycle_runs
            .iter()
            .filter(|(_, run)| run.finished_at.is_some_and(|at| at < finished_before))
            .map(|(_, run)| run.id)
            .collect();
        state
            .lifecycle_runs
            .retain(|(_, run)| !doomed.contains(&run.id));
        for id in &doomed {
            state.lifecycle_items.remove(id);
        }
        Ok(u64::try_from(doomed.len()).unwrap_or(u64::MAX))
    }
}

#[async_trait]
impl GcRunRepo for MemStore {
    async fn create(&self, run: &GcRun) -> Result<(), StoreError> {
        let now = self.now();
        let mut state = self.tables()?;
        if state.gc_runs.iter().any(|row| row.run.id == run.id) {
            return Err(conflict(format!("gc run {} already exists", run.id)));
        }
        let seq = state.next_run_seq();
        state.gc_runs.push(GcRunRow {
            seq,
            created_at: now,
            run: run.clone(),
        });
        Ok(())
    }

    async fn get(&self, id: GcRunId) -> Result<Option<GcRun>, StoreError> {
        let state = self.tables()?;
        Ok(state
            .gc_runs
            .iter()
            .find(|row| row.run.id == id)
            .map(|row| row.run.clone()))
    }

    async fn list(&self, page: &PageRequest) -> Result<Page<GcRun>, StoreError> {
        let rows: Vec<(String, GcRun)> = self
            .tables()?
            .gc_runs
            .iter()
            .map(|row| (newest_first_key(row.seq), row.run.clone()))
            .collect();
        paginate(rows, page)
    }

    async fn update_progress(
        &self,
        id: GcRunId,
        counts: &GcCounts,
        per_store: &serde_json::Value,
    ) -> Result<(), StoreError> {
        let mut state = self.tables()?;
        let row = state
            .gc_runs
            .iter_mut()
            .find(|row| row.run.id == id)
            .ok_or_else(|| not_found("gc run", id))?;
        row.run.counts = counts.clone();
        row.run.per_store = per_store.clone();
        Ok(())
    }

    async fn finish(
        &self,
        id: GcRunId,
        status: TaskRunStatus,
        at: OffsetDateTime,
    ) -> Result<(), StoreError> {
        let mut state = self.tables()?;
        let row = state
            .gc_runs
            .iter_mut()
            .find(|row| row.run.id == id)
            .ok_or_else(|| not_found("gc run", id))?;
        row.run.status = status;
        row.run.finished_at = Some(at);
        Ok(())
    }

    async fn request_stop(&self, id: GcRunId) -> Result<bool, StoreError> {
        let mut state = self.tables()?;
        let row = state
            .gc_runs
            .iter_mut()
            .find(|row| row.run.id == id)
            .ok_or_else(|| not_found("gc run", id))?;
        // A finished run cannot be stopped; the flag would never be read again.
        if row.run.status.is_terminal() {
            return Ok(false);
        }
        row.run.stop_requested = true;
        Ok(true)
    }

    async fn stop_requested(&self, id: GcRunId) -> Result<bool, StoreError> {
        let state = self.tables()?;
        state
            .gc_runs
            .iter()
            .find(|row| row.run.id == id)
            .map(|row| row.run.stop_requested)
            .ok_or_else(|| not_found("gc run", id))
    }

    async fn save_samples(&self, samples: &[GcSample]) -> Result<(), StoreError> {
        let mut state = self.tables()?;
        for sample in samples {
            if !state.gc_runs.iter().any(|row| row.run.id == sample.run_id) {
                return Err(conflict(format!("gc run {} does not exist", sample.run_id)));
            }
        }
        for sample in samples {
            let stored = state.gc_samples.entry(sample.run_id).or_default();
            // The report is a sample: rows past the cap are dropped, not an error.
            if stored.len() < MAX_GC_SAMPLES {
                stored.push(sample.clone());
            }
        }
        Ok(())
    }

    async fn list_samples(
        &self,
        run: GcRunId,
        page: &PageRequest,
    ) -> Result<Page<GcSample>, StoreError> {
        let state = self.tables()?;
        if !state.gc_runs.iter().any(|row| row.run.id == run) {
            return Err(not_found("gc run", run));
        }
        let rows: Vec<(String, GcSample)> = state
            .gc_samples
            .get(&run)
            .into_iter()
            .flatten()
            .enumerate()
            .map(|(index, sample)| (format!("{index:020}"), sample.clone()))
            .collect();
        drop(state);
        paginate(rows, page)
    }

    async fn last_manual_start(&self) -> Result<Option<OffsetDateTime>, StoreError> {
        let state = self.tables()?;
        // A run that was queued but has not started yet counts from its creation, or two
        // clicks in a row would both pass the one per minute limit.
        Ok(state
            .gc_runs
            .iter()
            .filter(|row| row.run.trigger == TaskTrigger::Manual)
            .map(|row| row.run.started_at.unwrap_or(row.created_at))
            .max())
    }

    async fn purge(&self, finished_before: OffsetDateTime) -> Result<u64, StoreError> {
        let mut state = self.tables()?;
        let doomed: Vec<GcRunId> = state
            .gc_runs
            .iter()
            .filter(|row| row.run.finished_at.is_some_and(|at| at < finished_before))
            .map(|row| row.run.id)
            .collect();
        state.gc_runs.retain(|row| !doomed.contains(&row.run.id));
        for id in &doomed {
            state.gc_samples.remove(id);
        }
        Ok(u64::try_from(doomed.len()).unwrap_or(u64::MAX))
    }
}

#[async_trait]
impl DownloadStatsRepo for MemStore {
    async fn daily(&self, asset: AssetId, days: u32) -> Result<Vec<(Date, u64)>, StoreError> {
        let today = self.now().to_offset(time::UtcOffset::UTC).date();
        let state = self.tables()?;
        let days = days.min(MAX_STATS_DAYS);
        let series = (0..days)
            .rev()
            .map(|back| {
                let day = today
                    .checked_sub(time::Duration::days(i64::from(back)))
                    .unwrap_or(today);
                let count = state.downloads.get(&(asset, day)).copied().unwrap_or(0);
                (day, count)
            })
            .collect();
        Ok(series)
    }
}
