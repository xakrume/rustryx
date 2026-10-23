//! Ports of blob store groups, the move ledger, verification runs and store statistics.
//!
//! Contract as in [`crate::ports`]: every method is one statement or one transaction and
//! therefore cancel safe (a cancelled future leaves the old or the new state, never a mix).
//! The additions to `BlobRepo` that this stream needs
//! (`move_blob`, `list_page_by_store`) are methods of that trait in [`crate::ports::repos`], next
//! to `insert`, `list_unreferenced_in` and `delete_if_unreferenced`.

use async_trait::async_trait;
use time::OffsetDateTime;

use crate::error::StoreError;
use crate::ids::{BlobId, BlobStoreId, VerifyRunId};
use crate::model::{BlobRecord, BlobStoreDef};
use crate::ports::repos::{Page, PageRequest};
use crate::storage::{
    BlobMove, BlobStoreMember, BlobStoreStats, MemberState, VerifyCounts, VerifyFinding, VerifyRun,
};
use crate::tasks::TaskRunStatus;

/// Members of group stores.
#[async_trait]
pub trait BlobStoreMemberRepo: Send + Sync {
    /// The members of a group in position order.
    async fn members(&self, group: BlobStoreId) -> Result<Vec<BlobStoreMember>, StoreError>;
    /// `Conflict` when the store is a group, is already a member of a group, or equals the
    /// group (no nesting, no sharing).
    async fn add_member(
        &self,
        group: BlobStoreId,
        member: BlobStoreId,
        position: u16,
    ) -> Result<(), StoreError>;
    /// Sets active or draining.
    async fn set_state(
        &self,
        group: BlobStoreId,
        member: BlobStoreId,
        state: MemberState,
    ) -> Result<(), StoreError>;
    /// `Conflict` while any blob row still names the member, or when it would leave a group
    /// that repositories use without members.
    async fn remove_member(
        &self,
        group: BlobStoreId,
        member: BlobStoreId,
    ) -> Result<(), StoreError>;
    /// One transaction: inserts `group` (kind `Group`), makes `existing` its first member and
    /// repoints every repository from `existing` to the group. Blob rows are not touched, so
    /// it is safe at any time. Publishes a bus event.
    async fn convert_to_group(
        &self,
        existing: BlobStoreId,
        group: &BlobStoreDef,
    ) -> Result<(), StoreError>;
    /// The group a store belongs to, if any.
    async fn group_of(&self, member: BlobStoreId) -> Result<Option<BlobStoreId>, StoreError>;
}

/// The ledger of blobs that moved between member stores.
#[async_trait]
pub trait BlobMoveRepo: Send + Sync {
    /// Upserts the ledger row of a switched blob (key `(blob_id, from_store)`, so two moves
    /// inside the cleanup window do not lose track of the first old object).
    async fn record(&self, mv: &BlobMove) -> Result<(), StoreError>;
    /// Rows whose old object may be deleted now.
    async fn due_for_cleanup(
        &self,
        now: OffsetDateTime,
        limit: u32,
    ) -> Result<Vec<BlobMove>, StoreError>;
    /// Marks the old object as deleted.
    async fn mark_cleaned(&self, blob: BlobId, from: BlobStoreId) -> Result<(), StoreError>;
}

/// Verification runs. The staged listing is scratch data of one run in an unlogged table: it
/// is lost on a crash (the run is started again) and it runs on the primary.
#[async_trait]
pub trait VerifyRunRepo: Send + Sync {
    /// Inserts a run.
    async fn create(&self, run: &VerifyRun) -> Result<(), StoreError>;
    /// Finds by id.
    async fn get(&self, id: VerifyRunId) -> Result<Option<VerifyRun>, StoreError>;
    /// Lists runs, newest first, optionally of one store.
    async fn list(
        &self,
        store: Option<BlobStoreId>,
        page: &PageRequest,
    ) -> Result<Page<VerifyRun>, StoreError>;
    /// Updates the counters of a running run.
    async fn update(&self, id: VerifyRunId, counts: &VerifyCounts) -> Result<(), StoreError>;
    /// Ends a run.
    async fn finish(
        &self,
        id: VerifyRunId,
        status: TaskRunStatus,
        counts: &VerifyCounts,
        at: OffsetDateTime,
    ) -> Result<(), StoreError>;
    /// Stages the keys listed from the store as `(key, size, modified)`. Idempotent per
    /// `(run, key)`.
    async fn stage_listing(
        &self,
        run: VerifyRunId,
        objects: &[(String, u64, OffsetDateTime)],
    ) -> Result<(), StoreError>;
    /// Anti join of `blobs` of the store against the staged listing: rows with no object.
    async fn missing(
        &self,
        run: VerifyRunId,
        store: BlobStoreId,
        after: Option<BlobId>,
        limit: u32,
    ) -> Result<Vec<BlobRecord>, StoreError>;
    /// Anti join of the staged listing against `blobs`: objects with no row and older than
    /// `grace_before`.
    async fn orphans(
        &self,
        run: VerifyRunId,
        store: BlobStoreId,
        grace_before: OffsetDateTime,
        after: Option<String>,
        limit: u32,
    ) -> Result<Vec<String>, StoreError>;
    /// Rows whose object has another size, with the size the store reported.
    async fn size_mismatches(
        &self,
        run: VerifyRunId,
        store: BlobStoreId,
        after: Option<BlobId>,
        limit: u32,
    ) -> Result<Vec<(BlobRecord, u64)>, StoreError>;
    /// Stores findings, at most 10 000 per run (the counts stay exact).
    async fn save_findings(&self, findings: &[VerifyFinding]) -> Result<(), StoreError>;
    /// Lists the findings of a run.
    async fn list_findings(
        &self,
        run: VerifyRunId,
        page: &PageRequest,
    ) -> Result<Page<VerifyFinding>, StoreError>;
    /// Drops the staged listing of a run.
    async fn drop_listing(&self, run: VerifyRunId) -> Result<(), StoreError>;
}

/// Store statistics, written by the `blob_store_stats` job.
#[async_trait]
pub trait BlobStoreStatsRepo: Send + Sync {
    /// Inserts or replaces the statistics of a store.
    async fn upsert(&self, stats: &BlobStoreStats) -> Result<(), StoreError>;
    /// The statistics of a store, `None` before the first refresh.
    async fn get(&self, store: BlobStoreId) -> Result<Option<BlobStoreStats>, StoreError>;
    /// All statistics.
    async fn list(&self) -> Result<Vec<BlobStoreStats>, StoreError>;
}
