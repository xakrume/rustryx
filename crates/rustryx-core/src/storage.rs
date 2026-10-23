//! Blob store groups, the move ledger, verification runs and store statistics.
//!
//! A group store is a blob store whose `kind` is `Group`. Its members are rows of
//! `blob_store_members`, not JSON, so they have foreign keys. Every blob row names the CONCRETE
//! member store that holds it (`BlobCommit::placement` becomes `BlobRecord::blob_store_id`), so
//! reads never probe members.

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::error::DomainError;
use crate::ids::{BlobId, BlobStoreId, TaskRunId, UserId, VerifyRunId};
use crate::tasks::TaskRunStatus;

/// How a group chooses the member that takes a new blob.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FillPolicy {
    /// Even spread. Implemented as `hash(key) mod n` over the eligible members in position
    /// order, not as a shared counter: it needs no coordination between nodes and gives the
    /// same member for the same key.
    RoundRobin,
    /// The first eligible member in position order.
    WriteToFirst,
}

/// Group configuration (`BlobStoreDef.config` of a group is `{"fill_policy": "round_robin"}`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlobGroupConfig {
    /// How new blobs are placed.
    pub fill_policy: FillPolicy,
}

/// State of a group member.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemberState {
    /// Takes new writes.
    Active,
    /// Takes no new writes; a move task empties it before it is removed.
    Draining,
}

impl MemberState {
    /// True when the member may take a new write (it is `Active`). Health and quota are
    /// checked by the group adapter on top of this.
    #[must_use]
    pub const fn accepts_writes(self) -> bool {
        matches!(self, Self::Active)
    }
}

/// A member of a group store.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlobStoreMember {
    /// The group.
    pub group_id: BlobStoreId,
    /// The member store. A store belongs to at most one group and a group cannot be a member.
    pub member_id: BlobStoreId,
    /// Position inside the group (unique per group); the placement order.
    pub position: u16,
    /// Active or draining.
    pub state: MemberState,
}

/// Ledger of a blob that moved between member stores.
///
/// The old object is deleted only after `cleanup_after`, which is at least twice
/// `limits.request_timeout_secs`, so a reader that resolved the old store before the switch can
/// finish.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlobMove {
    /// The blob.
    pub blob_id: BlobId,
    /// The store the blob left.
    pub from_store: BlobStoreId,
    /// The store the blob now lives in.
    pub to_store: BlobStoreId,
    /// Object key (identical in both stores).
    pub key: String,
    /// True once the old object was deleted.
    pub cleaned: bool,
    /// When the row was switched.
    #[serde(with = "crate::rfc3339")]
    pub moved_at: OffsetDateTime,
    /// Earliest time the old object may be deleted.
    #[serde(with = "crate::rfc3339")]
    pub cleanup_after: OffsetDateTime,
}

impl BlobMove {
    /// Builds a ledger row whose cleanup time respects the delay rule: `cleanup_after` is
    /// `moved_at` plus at least twice the request timeout.
    ///
    /// # Errors
    /// Returns [`DomainError::Invalid`] when `cleanup_delay_secs` is below twice
    /// `request_timeout_secs`.
    pub fn new(
        blob_id: BlobId,
        from_store: BlobStoreId,
        to_store: BlobStoreId,
        key: impl Into<String>,
        moved_at: OffsetDateTime,
        cleanup_delay_secs: u64,
        request_timeout_secs: u64,
    ) -> Result<Self, DomainError> {
        if cleanup_delay_secs < request_timeout_secs.saturating_mul(2) {
            return Err(DomainError::invalid(
                "the cleanup delay must be at least twice the request timeout",
            ));
        }
        let delay = i64::try_from(cleanup_delay_secs)
            .map_err(|_| DomainError::invalid("the cleanup delay is too large"))?;
        Ok(Self {
            blob_id,
            from_store,
            to_store,
            key: key.into(),
            cleaned: false,
            moved_at,
            cleanup_after: moved_at + time::Duration::seconds(delay),
        })
    }
}

/// Whether a verification run only reports or also repairs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VerifyMode {
    /// Changes nothing.
    Report,
    /// Deletes orphan objects, one at a time. Never deletes a row and never touches a missing
    /// object: a missing object is evidence of data loss that needs a restore.
    Repair,
}

/// What a verification found.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VerifyFindingKind {
    /// A blob row whose object is not in the store: data loss, critical.
    Missing,
    /// An object with no row older than the grace age: a leak (also the lost S3 commit
    /// response case).
    Orphan,
    /// The re-read bytes hash to something else than the row says.
    ChecksumMismatch,
    /// The object has another size than the row says.
    SizeMismatch,
}

/// Counters of a verification run.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct VerifyCounts {
    /// Keys the store listed.
    pub listed_objects: u64,
    /// Blob rows compared.
    pub rows_checked: u64,
    /// Rows without an object.
    pub missing: u64,
    /// Objects without a row.
    pub orphans: u64,
    /// Rows whose bytes were re-read and hashed.
    pub checksum_checked: u64,
    /// Re-read rows whose hash differs.
    pub checksum_mismatch: u64,
    /// Rows whose object has another size.
    pub size_mismatch: u64,
    /// Orphans deleted in repair mode.
    pub repaired: u64,
}

/// Default share of rows whose bytes a verification re-reads, in percent.
pub const DEFAULT_CHECKSUM_SAMPLE_PERCENT: f32 = 0.1;

/// One verification run over a blob store.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct VerifyRun {
    /// Identifier.
    pub id: VerifyRunId,
    /// The store under verification.
    pub blob_store_id: BlobStoreId,
    /// Report or repair.
    pub mode: VerifyMode,
    /// State.
    pub status: TaskRunStatus,
    /// Counters.
    pub counts: VerifyCounts,
    /// Share of rows whose bytes are re-read and re-hashed, 0 to 100. Default 0.1.
    pub checksum_sample_percent: f32,
    /// The task run that executes it.
    pub task_run_id: Option<TaskRunId>,
    /// Who asked for it, `None` for the weekly schedule.
    pub requested_by: Option<UserId>,
    /// Start time.
    #[serde(default, with = "crate::rfc3339::option")]
    pub started_at: Option<OffsetDateTime>,
    /// End time.
    #[serde(default, with = "crate::rfc3339::option")]
    pub finished_at: Option<OffsetDateTime>,
}

impl VerifyRun {
    /// A queued run with zero counters and the default sample share.
    #[must_use]
    pub fn new(id: VerifyRunId, blob_store_id: BlobStoreId, mode: VerifyMode) -> Self {
        Self {
            id,
            blob_store_id,
            mode,
            status: TaskRunStatus::Queued,
            counts: VerifyCounts::default(),
            checksum_sample_percent: DEFAULT_CHECKSUM_SAMPLE_PERCENT,
            task_run_id: None,
            requested_by: None,
            started_at: None,
            finished_at: None,
        }
    }

    /// Checks that the sample share is a number between 0 and 100.
    ///
    /// # Errors
    /// Returns [`DomainError::Invalid`] for NaN, a negative value or a value above 100.
    pub fn validate(&self) -> Result<(), DomainError> {
        if (0.0..=100.0).contains(&self.checksum_sample_percent) {
            Ok(())
        } else {
            Err(DomainError::invalid(
                "checksum sample percent must be between 0 and 100",
            ))
        }
    }
}

/// One finding of a verification run (at most 10 000 are stored, the counts stay exact).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VerifyFinding {
    /// The run.
    pub run_id: VerifyRunId,
    /// Position inside the run.
    pub seq: u32,
    /// What was found.
    pub kind: VerifyFindingKind,
    /// The blob row, when there is one.
    pub blob_id: Option<BlobId>,
    /// Object key.
    pub key: String,
    /// Short detail, at most 200 characters, no free text from a remote system.
    pub detail: String,
}

/// Usage of one blob store, refreshed by the `blob_store_stats` job.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlobStoreStats {
    /// The store.
    pub blob_store_id: BlobStoreId,
    /// Sum of the sizes of its blobs.
    pub used_bytes: u64,
    /// Number of blobs.
    pub blob_count: u64,
    /// Free bytes where the store knows them.
    pub free_bytes: Option<u64>,
    /// Total bytes where the store knows them.
    pub total_bytes: Option<u64>,
    /// When the numbers were computed.
    #[serde(with = "crate::rfc3339")]
    pub refreshed_at: OffsetDateTime,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{assert_wire, at, round_trip_json};

    #[test]
    fn fill_policies_and_member_states_use_snake_case() {
        assert_wire(&FillPolicy::RoundRobin, "\"round_robin\"");
        assert_wire(&FillPolicy::WriteToFirst, "\"write_to_first\"");
        assert_wire(&MemberState::Draining, "\"draining\"");
        assert_wire(
            &BlobGroupConfig {
                fill_policy: FillPolicy::RoundRobin,
            },
            r#"{"fill_policy":"round_robin"}"#,
        );
    }

    #[test]
    fn must_not_write_to_a_draining_member() {
        assert!(MemberState::Active.accepts_writes());
        assert!(!MemberState::Draining.accepts_writes());
    }

    #[test]
    fn member_row_wire_form() {
        let member = BlobStoreMember {
            group_id: BlobStoreId::generate(),
            member_id: BlobStoreId::generate(),
            position: 0,
            state: MemberState::Active,
        };
        let json = round_trip_json(&member);
        assert_eq!(json["state"], "active");
        assert_eq!(json["position"], 0);
    }

    #[test]
    fn must_not_accept_a_cleanup_delay_below_twice_the_request_timeout() {
        let (blob, from, to) = (
            BlobId::generate(),
            BlobStoreId::generate(),
            BlobStoreId::generate(),
        );
        assert!(BlobMove::new(blob, from, to, "k", at(), 119, 60).is_err());
        assert!(BlobMove::new(blob, from, to, "k", at(), 0, 60).is_err());
        let moved =
            BlobMove::new(blob, from, to, "k", at(), 120, 60).expect("exactly twice is enough");
        assert_eq!(moved.cleanup_after, at() + time::Duration::seconds(120));
        assert!(!moved.cleaned);
        assert!(
            BlobMove::new(blob, from, to, "k", at(), u64::MAX, 1).is_err(),
            "no overflow"
        );
    }

    #[test]
    fn move_ledger_serializes_rfc3339() {
        let moved = BlobMove::new(
            BlobId::generate(),
            BlobStoreId::generate(),
            BlobStoreId::generate(),
            "k",
            at(),
            600,
            30,
        )
        .expect("valid");
        let json = round_trip_json(&moved);
        assert_eq!(json["moved_at"], "2026-10-24T10:00:00Z");
        assert_eq!(json["cleanup_after"], "2026-10-24T10:10:00Z");
    }

    /// No wildcard arm: a new finding kind needs a wire name and a decision here.
    fn finding_name(kind: VerifyFindingKind) -> &'static str {
        match kind {
            VerifyFindingKind::Missing => "missing",
            VerifyFindingKind::Orphan => "orphan",
            VerifyFindingKind::ChecksumMismatch => "checksum_mismatch",
            VerifyFindingKind::SizeMismatch => "size_mismatch",
        }
    }

    #[test]
    fn verify_enums_match_the_database_checks() {
        for kind in [
            VerifyFindingKind::Missing,
            VerifyFindingKind::Orphan,
            VerifyFindingKind::ChecksumMismatch,
            VerifyFindingKind::SizeMismatch,
        ] {
            assert_wire(&kind, &format!("\"{}\"", finding_name(kind)));
        }
        assert_wire(&VerifyMode::Report, "\"report\"");
        assert_wire(&VerifyMode::Repair, "\"repair\"");
    }

    #[test]
    fn verify_run_constructor_and_sample_validation() {
        let mut run = VerifyRun::new(
            VerifyRunId::generate(),
            BlobStoreId::generate(),
            VerifyMode::Report,
        );
        assert_eq!(run.status, TaskRunStatus::Queued);
        assert!((run.checksum_sample_percent - 0.1).abs() < f32::EPSILON);
        assert!(run.validate().is_ok());
        for bad in [-0.1_f32, 100.1, f32::NAN, f32::INFINITY] {
            run.checksum_sample_percent = bad;
            assert!(run.validate().is_err(), "{bad}");
        }
        for good in [0.0_f32, 100.0, 12.5] {
            run.checksum_sample_percent = good;
            assert!(run.validate().is_ok(), "{good}");
        }
    }

    #[test]
    fn verify_run_wire_form() {
        let mut run = VerifyRun::new(
            VerifyRunId::generate(),
            BlobStoreId::generate(),
            VerifyMode::Repair,
        );
        run.started_at = Some(at());
        let json = round_trip_json(&run);
        assert_eq!(json["mode"], "repair");
        assert_eq!(json["started_at"], "2026-10-24T10:00:00Z");
        assert!(json["finished_at"].is_null());
        let counts = round_trip_json(&VerifyCounts::default());
        assert_eq!(counts["checksum_mismatch"], 0);
    }

    #[test]
    fn stats_serialize_the_refresh_time() {
        let stats = BlobStoreStats {
            blob_store_id: BlobStoreId::generate(),
            used_bytes: 1,
            blob_count: 1,
            free_bytes: None,
            total_bytes: Some(10),
            refreshed_at: at(),
        };
        assert_eq!(
            round_trip_json(&stats)["refreshed_at"],
            "2026-10-24T10:00:00Z"
        );
    }
}
