//! System ports shared by several streams: the event outbox, task definitions and runs, the node
//! registry, the settings store and the managed trust store.
//!
//! Contract as in [`crate::ports`]: every method is one statement or one transaction and
//! therefore cancel safe (a cancelled future leaves the old or the new state, never a mix).

use std::time::Duration;

use async_trait::async_trait;
use time::OffsetDateTime;

use crate::error::StoreError;
use crate::events::{CursorAdvance, EventDraft, OutboxBatch};
use crate::ids::{
    RegistryId, RepositoryId, ScannerId, TaskDefinitionId, TaskRunId, TrustedCertificateId, UserId,
};
use crate::node::NodeInfo;
use crate::ports::repos::{Page, PageRequest};
use crate::settings::TrustedCertificate;
use crate::tasks::{
    Heartbeat, NewTaskRun, Reaped, TaskDefinition, TaskLogLine, TaskOutcome, TaskRun,
};

/// The event outbox.
///
/// How the relay stays correct: a sequence value is allocated at insert, but commit order can
/// differ from sequence order, so a plain `seq > cursor` read can skip a row that commits late.
/// A batch is therefore read with `WHERE (tx_id, seq) > (cursor) AND tx_id <
/// pg_snapshot_xmin(pg_current_snapshot())`: rows below that horizon belong to finished
/// transactions, so the set of rows below the horizon never gains a smaller key.
#[async_trait]
pub trait OutboxRepo: Send + Sync {
    /// Writes events in their own transaction. For events that have no state change to ride on
    /// (pull, a rejection). An event over 16 KiB is `StoreError::Invalid`.
    async fn enqueue(&self, events: &[EventDraft]) -> Result<(), StoreError>;
    /// Events after the consumer's cursor whose writing transaction has finished (see above),
    /// in `(tx_id, seq)` order, at most `limit`. An empty batch carries `from == to`. Cancel
    /// safe, read only.
    async fn read_batch(&self, consumer: &str, limit: u32) -> Result<OutboxBatch, StoreError>;
    /// Moves the cursor with compare and set on `from`. `false` means another instance moved
    /// it first: the caller drops its batch.
    async fn advance(&self, advance: &CursorAdvance) -> Result<bool, StoreError>;
    /// Age of the oldest event the consumer has not handled, zero when there is none. Feeds
    /// `outbox_lag_seconds`.
    async fn lag(&self, consumer: &str) -> Result<Duration, StoreError>;
    /// Drops whole partitions older than `older_than` and returns how many. Never drops past
    /// the slowest cursor.
    async fn purge(&self, older_than: OffsetDateTime) -> Result<u64, StoreError>;
}

/// Task definitions (what runs and when).
#[async_trait]
pub trait TaskDefinitionRepo: Send + Sync {
    /// Inserts. `Conflict` when the name exists.
    async fn create(&self, definition: &TaskDefinition) -> Result<(), StoreError>;
    /// Finds by id.
    async fn get(&self, id: TaskDefinitionId) -> Result<Option<TaskDefinition>, StoreError>;
    /// Lists the definitions, optionally of one kind.
    async fn list(&self, kind: Option<&str>) -> Result<Vec<TaskDefinition>, StoreError>;
    /// Replaces schedule, enabled, notify and (for non system tasks) name and params.
    /// `Conflict` on a stale version.
    async fn update(
        &self,
        definition: &TaskDefinition,
        expected_version: i64,
    ) -> Result<(), StoreError>;
    /// `Conflict` for a system task.
    async fn delete(&self, id: TaskDefinitionId) -> Result<(), StoreError>;
    /// Inserts the missing system definitions; existing rows (edited schedules) are left
    /// alone. Idempotent. Returns how many were inserted.
    async fn seed(&self, definitions: &[TaskDefinition]) -> Result<u64, StoreError>;
    /// Enabled definitions with `next_run_at <= now`, oldest first.
    async fn due(&self, now: OffsetDateTime, limit: u32)
    -> Result<Vec<TaskDefinition>, StoreError>;
    /// Compare and set of `next_run_at`: exactly one node wins a due time, so a schedule fires
    /// once without a lock.
    async fn advance(
        &self,
        id: TaskDefinitionId,
        expected: Option<OffsetDateTime>,
        next: Option<OffsetDateTime>,
    ) -> Result<bool, StoreError>;
}

/// Task runs and their logs.
#[async_trait]
pub trait TaskRunRepo: Send + Sync {
    /// `None` when a queued or running run of the same `(kind, scope_key)` exists (the unique
    /// index).
    async fn enqueue(&self, run: &NewTaskRun) -> Result<Option<TaskRun>, StoreError>;
    /// Claims the oldest queued run of one of `kinds` (`SKIP LOCKED`), sets lease and node.
    async fn claim(
        &self,
        node_id: &str,
        kinds: &[&str],
        lease: Duration,
    ) -> Result<Option<TaskRun>, StoreError>;
    /// Puts a claimed run back (the singleton lock was not free), available again after
    /// `delay`.
    async fn release(
        &self,
        id: TaskRunId,
        node_id: &str,
        delay: Duration,
    ) -> Result<bool, StoreError>;
    /// Extends the lease and reports whether a stop was requested.
    async fn heartbeat(
        &self,
        id: TaskRunId,
        node_id: &str,
        lease: Duration,
    ) -> Result<Heartbeat, StoreError>;
    /// Terminal state. Also writes the `task.finished` outbox event when the definition asks
    /// for it, in the same transaction. `false` when the lease was lost.
    async fn finish(
        &self,
        id: TaskRunId,
        node_id: &str,
        outcome: &TaskOutcome,
    ) -> Result<bool, StoreError>;
    /// Sets `stop_requested_at`; the runner sees it at the next heartbeat. `false` for a
    /// finished run.
    async fn request_stop(&self, id: TaskRunId) -> Result<bool, StoreError>;
    /// Finds by id.
    async fn get(&self, id: TaskRunId) -> Result<Option<TaskRun>, StoreError>;
    /// Lists runs, newest first, optionally of one definition.
    async fn list(
        &self,
        task: Option<TaskDefinitionId>,
        page: &PageRequest,
    ) -> Result<Page<TaskRun>, StoreError>;
    /// Appends lines. The per run cap is 5000 lines; later lines are dropped and one
    /// truncation line is kept.
    async fn append_log(&self, id: TaskRunId, lines: &[TaskLogLine]) -> Result<(), StoreError>;
    /// Reads the log after `after_seq`.
    async fn read_log(
        &self,
        id: TaskRunId,
        after_seq: i32,
        limit: u32,
    ) -> Result<Vec<TaskLogLine>, StoreError>;
    /// Runs whose lease expired: resumable kinds go back to `queued` (at most 3 attempts),
    /// others become `abandoned`.
    async fn reap_expired(
        &self,
        now: OffsetDateTime,
        resumable_kinds: &[&str],
    ) -> Result<Reaped, StoreError>;
    /// Removes finished runs before the cutoff; returns how many.
    async fn purge(&self, finished_before: OffsetDateTime) -> Result<u64, StoreError>;
}

/// The node registry and feature levels.
#[async_trait]
pub trait NodeRegistryRepo: Send + Sync {
    /// Upsert of the node's own row, every 30 s (`node_heartbeat`).
    async fn heartbeat(&self, node: &NodeInfo) -> Result<(), StoreError>;
    /// Nodes seen within `max_age` (90 s). A stopped node drops out by itself; there is no
    /// deregistration to forget.
    async fn list_live(&self, max_age: Duration) -> Result<Vec<NodeInfo>, StoreError>;
}

/// One row of `system_config`.
///
/// `Debug` prints the key and the revision but never the value: a row read with
/// `SecretView::Reveal` holds the plain text of sealed fields (an SMTP or proxy password).
#[derive(Clone)]
pub struct ConfigEntry {
    /// The key (`settings.smtp`, `system.read_only`, ...).
    pub key: String,
    /// The JSON value. With `SecretView::Mask` a sealed field is removed and listed under
    /// `"_sealed"`.
    pub value: serde_json::Value,
    /// Revision assigned from `system_config_revision_seq` at the last write.
    pub revision: i64,
    /// Last write time.
    pub updated_at: OffsetDateTime,
    /// Who wrote it.
    pub updated_by: Option<UserId>,
}

impl std::fmt::Debug for ConfigEntry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ConfigEntry")
            .field("key", &self.key)
            .field("revision", &self.revision)
            .field("updated_at", &self.updated_at)
            .field("updated_by", &self.updated_by)
            .finish_non_exhaustive()
    }
}

/// Whether sealed fields come back as plain values (runtime consumers) or removed (API
/// responses).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SecretView {
    /// Remove sealed fields.
    Mask,
    /// Return them unsealed.
    Reveal,
}

/// The settings store. Typed access for consumers is a crate level handle in `rustryx-api`, so
/// no component parses JSON.
#[async_trait]
pub trait SystemConfigRepo: Send + Sync {
    /// With `Mask`, a sealed field is removed from the value and its pointer is listed in the
    /// entry's value under `"_sealed": ["/password"]`, so the API can say `password_set: true`
    /// without the value.
    async fn get(&self, key: &str, view: SecretView) -> Result<Option<ConfigEntry>, StoreError>;
    /// Entries whose key starts with `prefix`.
    async fn list_prefix(
        &self,
        prefix: &str,
        view: SecretView,
    ) -> Result<Vec<ConfigEntry>, StoreError>;
    /// Writes `value`, sealing the strings at the JSON pointers in `seal` (purpose
    /// `settings.<key>`, row = the key), assigns the next revision from
    /// `system_config_revision_seq`, and publishes `InvalidationEvent::Config(revision)`. With
    /// `expected_revision`, `Conflict` on a stale write. A sealed field that is absent in
    /// `value` keeps its stored value; a JSON `null` clears it (so a form can leave the
    /// password blank).
    async fn put(
        &self,
        key: &str,
        value: &serde_json::Value,
        seal: &[&str],
        expected_revision: Option<i64>,
        actor: Option<UserId>,
    ) -> Result<ConfigEntry, StoreError>;
    /// Deletes a key.
    async fn delete(&self, key: &str, actor: Option<UserId>) -> Result<(), StoreError>;
    /// Highest revision of any `settings.*` or `system.*` row: the revision a node compares
    /// with.
    async fn current_revision(&self) -> Result<i64, StoreError>;
}

/// What still depends on a trusted certificate.
#[derive(Debug, Clone)]
pub struct CertificateDependents {
    /// Proxy repositories that list it.
    pub repositories: Vec<RepositoryId>,
    /// Registries that list it.
    pub registries: Vec<RegistryId>,
    /// Scanners that list it.
    pub scanners: Vec<ScannerId>,
}

/// The managed outbound trust store.
#[async_trait]
pub trait TrustedCertificateRepo: Send + Sync {
    /// Inserts. `Conflict` on a duplicate fingerprint or name.
    async fn create(&self, certificate: &TrustedCertificate) -> Result<(), StoreError>;
    /// Finds by id.
    async fn get(&self, id: TrustedCertificateId)
    -> Result<Option<TrustedCertificate>, StoreError>;
    /// Lists all certificates.
    async fn list(&self) -> Result<Vec<TrustedCertificate>, StoreError>;
    /// `Conflict` while a proxy repository, registry or scanner lists it; `dependents` names
    /// them.
    async fn delete(&self, id: TrustedCertificateId) -> Result<(), StoreError>;
    /// What still depends on the certificate.
    async fn dependents(
        &self,
        id: TrustedCertificateId,
    ) -> Result<CertificateDependents, StoreError>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::at;

    #[test]
    fn must_not_leak_a_revealed_setting_through_debug() {
        let entry = ConfigEntry {
            key: "settings.smtp".to_owned(),
            value: serde_json::json!({"password": "canary-smtp-password"}),
            revision: 7,
            updated_at: at(),
            updated_by: None,
        };
        let rendered = format!("{entry:?}");
        assert!(!rendered.contains("canary"), "{rendered}");
        assert!(
            rendered.contains("settings.smtp") && rendered.contains('7'),
            "{rendered}"
        );
    }

    #[test]
    fn secret_views_differ() {
        assert_ne!(SecretView::Mask, SecretView::Reveal);
    }
}
