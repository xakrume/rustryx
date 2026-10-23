//! Blob store groups, the move ledger, verification runs and store statistics.

use std::collections::HashSet;

use async_trait::async_trait;
use rustryx_core::error::StoreError;
use rustryx_core::ids::{BlobId, BlobStoreId, VerifyRunId};
use rustryx_core::model::{BlobRecord, BlobStoreDef, BlobStoreKind};
use rustryx_core::ports::{
    BlobMoveRepo, BlobStoreMemberRepo, BlobStoreStatsRepo, InvalidationEvent, Page, PageRequest,
    VerifyRunRepo,
};
use rustryx_core::storage::{
    BlobMove, BlobStoreMember, BlobStoreStats, MemberState, VerifyCounts, VerifyFinding, VerifyRun,
};
use rustryx_core::tasks::TaskRunStatus;
use time::OffsetDateTime;

use super::paging::paginate;
use super::state::State;
use super::{MemStore, conflict, not_found};

/// Most findings a verification run stores; the counters stay exact.
const MAX_VERIFY_FINDINGS: usize = 10_000;

impl State {
    fn store_kind(&self, id: BlobStoreId) -> Option<BlobStoreKind> {
        self.blob_stores.get(&id).map(|store| store.kind)
    }

    fn members_of(&self, group: BlobStoreId) -> Vec<&BlobStoreMember> {
        let mut members: Vec<&BlobStoreMember> = self
            .store_members
            .iter()
            .filter(|m| m.group_id == group)
            .collect();
        members.sort_by_key(|m| (m.position, m.member_id));
        members
    }
}

#[async_trait]
impl BlobStoreMemberRepo for MemStore {
    async fn members(&self, group: BlobStoreId) -> Result<Vec<BlobStoreMember>, StoreError> {
        let state = self.tables()?;
        Ok(state.members_of(group).into_iter().cloned().collect())
    }

    async fn add_member(
        &self,
        group: BlobStoreId,
        member: BlobStoreId,
        position: u16,
    ) -> Result<(), StoreError> {
        let mut state = self.tables()?;
        // The `members_shape` trigger: the group is of kind `group`, the member is not, no
        // store joins itself, and a store belongs to at most one group.
        match (state.store_kind(group), state.store_kind(member)) {
            (Some(BlobStoreKind::Group), Some(kind)) if kind != BlobStoreKind::Group => {}
            (Some(_), Some(_)) => {
                return Err(conflict(
                    "a group holds plain stores and groups do not nest",
                ));
            }
            _ => return Err(conflict("the group and the member must exist")),
        }
        if group == member {
            return Err(conflict("a store cannot be a member of itself"));
        }
        if state.store_members.iter().any(|m| m.member_id == member) {
            return Err(conflict("the store already belongs to a group"));
        }
        state.store_members.push(BlobStoreMember {
            group_id: group,
            member_id: member,
            position,
            state: MemberState::Active,
        });
        drop(state);
        self.notify(InvalidationEvent::Reset);
        Ok(())
    }

    async fn set_state(
        &self,
        group: BlobStoreId,
        member: BlobStoreId,
        state: MemberState,
    ) -> Result<(), StoreError> {
        let mut tables = self.tables()?;
        let row = tables
            .store_members
            .iter_mut()
            .find(|m| m.group_id == group && m.member_id == member)
            .ok_or_else(|| not_found("blob store member", member))?;
        row.state = state;
        drop(tables);
        self.notify(InvalidationEvent::Reset);
        Ok(())
    }

    async fn remove_member(
        &self,
        group: BlobStoreId,
        member: BlobStoreId,
    ) -> Result<(), StoreError> {
        let mut state = self.tables()?;
        if !state
            .store_members
            .iter()
            .any(|m| m.group_id == group && m.member_id == member)
        {
            return Err(not_found("blob store member", member));
        }
        // Blob rows name the concrete member, so a member that still holds data cannot leave.
        if state.blobs.values().any(|b| b.blob_store_id == member) {
            return Err(conflict("the member still holds blobs"));
        }
        let remaining = state.members_of(group).len().saturating_sub(1);
        let in_use = state
            .repositories
            .values()
            .any(|r| r.blob_store_id == group);
        if remaining == 0 && in_use {
            return Err(conflict("a group used by repositories needs a member"));
        }
        state
            .store_members
            .retain(|m| !(m.group_id == group && m.member_id == member));
        drop(state);
        self.notify(InvalidationEvent::Reset);
        Ok(())
    }

    async fn convert_to_group(
        &self,
        existing: BlobStoreId,
        group: &BlobStoreDef,
    ) -> Result<(), StoreError> {
        let mut state = self.tables()?;
        let kind = state
            .store_kind(existing)
            .ok_or_else(|| not_found("blob store", existing))?;
        if kind == BlobStoreKind::Group || group.kind != BlobStoreKind::Group {
            return Err(conflict("a plain store is converted into a group store"));
        }
        if state.store_members.iter().any(|m| m.member_id == existing) {
            return Err(conflict("the store already belongs to a group"));
        }
        let taken = state
            .blob_stores
            .values()
            .any(|s| s.id == group.id || s.name == group.name);
        if taken {
            return Err(conflict(format!(
                "blob store {} already exists",
                group.name
            )));
        }
        // One step: the group row, the first member and the repointed repositories. Blob rows
        // are not touched, so it is safe at any time.
        state.blob_stores.insert(group.id, group.clone());
        state.store_members.push(BlobStoreMember {
            group_id: group.id,
            member_id: existing,
            position: 0,
            state: MemberState::Active,
        });
        for repository in state.repositories.values_mut() {
            if repository.blob_store_id == existing {
                repository.blob_store_id = group.id;
            }
        }
        drop(state);
        self.notify(InvalidationEvent::Reset);
        Ok(())
    }

    async fn group_of(&self, member: BlobStoreId) -> Result<Option<BlobStoreId>, StoreError> {
        let state = self.tables()?;
        Ok(state
            .store_members
            .iter()
            .find(|m| m.member_id == member)
            .map(|m| m.group_id))
    }
}

#[async_trait]
impl BlobMoveRepo for MemStore {
    async fn record(&self, mv: &BlobMove) -> Result<(), StoreError> {
        let mut state = self.tables()?;
        // Keyed by `(blob, from store)`: a second move inside the cleanup window leaves the
        // first old object tracked under its own key.
        state
            .blob_moves
            .insert((mv.blob_id, mv.from_store), mv.clone());
        Ok(())
    }

    async fn due_for_cleanup(
        &self,
        now: OffsetDateTime,
        limit: u32,
    ) -> Result<Vec<BlobMove>, StoreError> {
        let state = self.tables()?;
        let mut due: Vec<BlobMove> = state
            .blob_moves
            .values()
            .filter(|mv| !mv.cleaned && mv.cleanup_after <= now)
            .cloned()
            .collect();
        due.sort_by_key(|mv| (mv.cleanup_after, mv.blob_id));
        due.truncate(usize::try_from(limit).unwrap_or(usize::MAX));
        Ok(due)
    }

    async fn mark_cleaned(&self, blob: BlobId, from: BlobStoreId) -> Result<(), StoreError> {
        let mut state = self.tables()?;
        // Idempotent: the cleanup job may repeat after a crash between the object delete and
        // this mark.
        if let Some(mv) = state.blob_moves.get_mut(&(blob, from)) {
            mv.cleaned = true;
        }
        Ok(())
    }
}

fn run_key(seq: u64) -> String {
    format!("{:020}", u64::MAX - seq)
}

#[async_trait]
impl VerifyRunRepo for MemStore {
    async fn create(&self, run: &VerifyRun) -> Result<(), StoreError> {
        let mut state = self.tables()?;
        run.validate()
            .map_err(|error| conflict(error.to_string()))?;
        if state.verify_runs.iter().any(|(_, r)| r.id == run.id) {
            return Err(conflict(format!("verify run {} already exists", run.id)));
        }
        state.run_seq += 1;
        let seq = state.run_seq;
        state.verify_runs.push((seq, run.clone()));
        Ok(())
    }

    async fn get(&self, id: VerifyRunId) -> Result<Option<VerifyRun>, StoreError> {
        let state = self.tables()?;
        Ok(state
            .verify_runs
            .iter()
            .find(|(_, r)| r.id == id)
            .map(|(_, r)| r.clone()))
    }

    async fn list(
        &self,
        store: Option<BlobStoreId>,
        page: &PageRequest,
    ) -> Result<Page<VerifyRun>, StoreError> {
        let rows: Vec<(String, VerifyRun)> = self
            .tables()?
            .verify_runs
            .iter()
            .filter(|(_, run)| store.is_none_or(|s| run.blob_store_id == s))
            .map(|(seq, run)| (run_key(*seq), run.clone()))
            .collect();
        paginate(rows, page)
    }

    async fn update(&self, id: VerifyRunId, counts: &VerifyCounts) -> Result<(), StoreError> {
        let mut state = self.tables()?;
        let (_, run) = state
            .verify_runs
            .iter_mut()
            .find(|(_, r)| r.id == id)
            .ok_or_else(|| not_found("verify run", id))?;
        run.counts = counts.clone();
        Ok(())
    }

    async fn finish(
        &self,
        id: VerifyRunId,
        status: TaskRunStatus,
        counts: &VerifyCounts,
        at: OffsetDateTime,
    ) -> Result<(), StoreError> {
        let mut state = self.tables()?;
        let (_, run) = state
            .verify_runs
            .iter_mut()
            .find(|(_, r)| r.id == id)
            .ok_or_else(|| not_found("verify run", id))?;
        run.status = status;
        run.counts = counts.clone();
        run.finished_at = Some(at);
        Ok(())
    }

    async fn stage_listing(
        &self,
        run: VerifyRunId,
        objects: &[(String, u64, OffsetDateTime)],
    ) -> Result<(), StoreError> {
        let mut state = self.tables()?;
        if !state.verify_runs.iter().any(|(_, r)| r.id == run) {
            return Err(conflict(format!("verify run {run} does not exist")));
        }
        let listing = state.verify_listing.entry(run).or_default();
        // Idempotent per `(run, key)`: a repeated page overwrites the same entries.
        for (key, size, modified) in objects {
            listing.insert(key.clone(), (*size, *modified));
        }
        Ok(())
    }

    async fn missing(
        &self,
        run: VerifyRunId,
        store: BlobStoreId,
        after: Option<BlobId>,
        limit: u32,
    ) -> Result<Vec<BlobRecord>, StoreError> {
        let state = self.tables()?;
        let listing = state.verify_listing.get(&run);
        let mut rows: Vec<BlobRecord> = state
            .blobs
            .values()
            .filter(|b| b.blob_store_id == store)
            .filter(|b| after.is_none_or(|a| b.id > a))
            .filter(|b| listing.is_none_or(|l| !l.contains_key(&b.key)))
            .cloned()
            .collect();
        rows.sort_by_key(|b| b.id);
        rows.truncate(usize::try_from(limit).unwrap_or(usize::MAX));
        Ok(rows)
    }

    async fn orphans(
        &self,
        run: VerifyRunId,
        store: BlobStoreId,
        grace_before: OffsetDateTime,
        after: Option<String>,
        limit: u32,
    ) -> Result<Vec<String>, StoreError> {
        let state = self.tables()?;
        let known: HashSet<&str> = state
            .blobs
            .values()
            .filter(|b| b.blob_store_id == store)
            .map(|b| b.key.as_str())
            .collect();
        let keys = state
            .verify_listing
            .get(&run)
            .into_iter()
            .flatten()
            .filter(|(key, _)| after.as_ref().is_none_or(|a| *key > a))
            .filter(|(key, _)| !known.contains(key.as_str()))
            // Young objects may belong to a publish that has not committed its row yet.
            .filter(|(_, (_, modified))| *modified < grace_before)
            .map(|(key, _)| key.clone())
            .take(usize::try_from(limit).unwrap_or(usize::MAX))
            .collect();
        Ok(keys)
    }

    async fn size_mismatches(
        &self,
        run: VerifyRunId,
        store: BlobStoreId,
        after: Option<BlobId>,
        limit: u32,
    ) -> Result<Vec<(BlobRecord, u64)>, StoreError> {
        let state = self.tables()?;
        let Some(listing) = state.verify_listing.get(&run) else {
            return Ok(Vec::new());
        };
        let mut rows: Vec<(BlobRecord, u64)> = state
            .blobs
            .values()
            .filter(|b| b.blob_store_id == store)
            .filter(|b| after.is_none_or(|a| b.id > a))
            .filter_map(|b| {
                let (size, _) = listing.get(&b.key)?;
                (*size != b.size_bytes).then(|| (b.clone(), *size))
            })
            .collect();
        rows.sort_by_key(|(blob, _)| blob.id);
        rows.truncate(usize::try_from(limit).unwrap_or(usize::MAX));
        Ok(rows)
    }

    async fn save_findings(&self, findings: &[VerifyFinding]) -> Result<(), StoreError> {
        let mut state = self.tables()?;
        for finding in findings {
            if !state
                .verify_runs
                .iter()
                .any(|(_, r)| r.id == finding.run_id)
            {
                return Err(conflict(format!(
                    "verify run {} does not exist",
                    finding.run_id
                )));
            }
        }
        for finding in findings {
            let stored = state.verify_findings.entry(finding.run_id).or_default();
            if stored.iter().any(|f| f.seq == finding.seq) {
                return Err(conflict(format!("finding {} already exists", finding.seq)));
            }
            // Past the cap rows are dropped; the counters of the run stay exact.
            if stored.len() < MAX_VERIFY_FINDINGS {
                stored.push(finding.clone());
            }
        }
        Ok(())
    }

    async fn list_findings(
        &self,
        run: VerifyRunId,
        page: &PageRequest,
    ) -> Result<Page<VerifyFinding>, StoreError> {
        let state = self.tables()?;
        if !state.verify_runs.iter().any(|(_, r)| r.id == run) {
            return Err(not_found("verify run", run));
        }
        let rows: Vec<(String, VerifyFinding)> = state
            .verify_findings
            .get(&run)
            .into_iter()
            .flatten()
            .map(|f| (format!("{:010}", f.seq), f.clone()))
            .collect();
        drop(state);
        paginate(rows, page)
    }

    async fn drop_listing(&self, run: VerifyRunId) -> Result<(), StoreError> {
        self.tables()?.verify_listing.remove(&run);
        Ok(())
    }
}

#[async_trait]
impl BlobStoreStatsRepo for MemStore {
    async fn upsert(&self, stats: &BlobStoreStats) -> Result<(), StoreError> {
        self.tables()?
            .store_stats
            .insert(stats.blob_store_id, stats.clone());
        Ok(())
    }

    async fn get(&self, store: BlobStoreId) -> Result<Option<BlobStoreStats>, StoreError> {
        Ok(self.tables()?.store_stats.get(&store).cloned())
    }

    async fn list(&self) -> Result<Vec<BlobStoreStats>, StoreError> {
        let mut all: Vec<BlobStoreStats> = self.tables()?.store_stats.values().cloned().collect();
        all.sort_by_key(|s| s.blob_store_id);
        Ok(all)
    }
}
