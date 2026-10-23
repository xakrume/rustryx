//! The event catalog and the types of the event outbox.
//!
//! The outbox is one durable, ordered log of "something happened" that webhooks, e-mail,
//! replication, scan on push and any later consumer read. A state changing port method writes
//! its events in the same transaction (`WriteOptions::events`), so an event exists if and only
//! if the change committed. Delivery to the outside world is at least once; the stable event id
//! lets a receiver deduplicate.

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::audit::ActorRef;
use crate::error::StoreError;
use crate::ids::{EventId, ProjectId, RepositoryId};

/// Largest `data` payload of one event, in bytes of its JSON text.
pub const MAX_EVENT_DATA_BYTES: usize = 16 * 1024;

/// The event catalog. Wire names are dotted because the specification fixes them (DSO-32);
/// this is the one documented exception to the snake case rule. A producer cannot invent a
/// kind and a subscription is validated against the list.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[non_exhaustive]
pub enum EventKind {
    /// A component was pushed (one event per component, not per asset).
    #[serde(rename = "push")]
    Push,
    /// A component was pulled. Written only while a webhook subscribes to it.
    #[serde(rename = "pull")]
    Pull,
    /// A component or asset was deleted by a user.
    #[serde(rename = "delete")]
    Delete,
    /// A scan finished.
    #[serde(rename = "scan.completed")]
    ScanCompleted,
    /// A scan failed.
    #[serde(rename = "scan.failed")]
    ScanFailed,
    /// A scan was stopped.
    #[serde(rename = "scan.stopped")]
    ScanStopped,
    /// The pull gate refused a pull.
    #[serde(rename = "pull.blocked")]
    PullBlocked,
    /// A component was quarantined.
    #[serde(rename = "quarantine.entered")]
    QuarantineEntered,
    /// A quarantined component was released.
    #[serde(rename = "quarantine.released")]
    QuarantineReleased,
    /// A write was refused because a quota would be exceeded.
    #[serde(rename = "quota.exceeded")]
    QuotaExceeded,
    /// A quota crossed its warn percentage.
    #[serde(rename = "quota.near_threshold")]
    QuotaNearThreshold,
    /// A replication execution finished.
    #[serde(rename = "replication.finished")]
    ReplicationFinished,
    /// A retention run finished.
    #[serde(rename = "retention.finished")]
    RetentionFinished,
    /// A garbage collection run finished.
    #[serde(rename = "gc.finished")]
    GcFinished,
    /// A promotion was requested.
    #[serde(rename = "promotion.requested")]
    PromotionRequested,
    /// A promotion was approved.
    #[serde(rename = "promotion.approved")]
    PromotionApproved,
    /// A promotion was rejected.
    #[serde(rename = "promotion.rejected")]
    PromotionRejected,
    /// A promotion finished.
    #[serde(rename = "promotion.completed")]
    PromotionCompleted,
    /// Internal, feeds e-mail notifications only.
    #[serde(rename = "task.finished")]
    TaskFinished,
    /// Internal, feeds e-mail notifications only.
    #[serde(rename = "token.expiring")]
    TokenExpiring,
}

impl EventKind {
    /// Every kind, in declaration order.
    pub const ALL: &'static [Self] = &[
        Self::Push,
        Self::Pull,
        Self::Delete,
        Self::ScanCompleted,
        Self::ScanFailed,
        Self::ScanStopped,
        Self::PullBlocked,
        Self::QuarantineEntered,
        Self::QuarantineReleased,
        Self::QuotaExceeded,
        Self::QuotaNearThreshold,
        Self::ReplicationFinished,
        Self::RetentionFinished,
        Self::GcFinished,
        Self::PromotionRequested,
        Self::PromotionApproved,
        Self::PromotionRejected,
        Self::PromotionCompleted,
        Self::TaskFinished,
        Self::TokenExpiring,
    ];

    /// The dotted wire name, identical to the serde name.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Push => "push",
            Self::Pull => "pull",
            Self::Delete => "delete",
            Self::ScanCompleted => "scan.completed",
            Self::ScanFailed => "scan.failed",
            Self::ScanStopped => "scan.stopped",
            Self::PullBlocked => "pull.blocked",
            Self::QuarantineEntered => "quarantine.entered",
            Self::QuarantineReleased => "quarantine.released",
            Self::QuotaExceeded => "quota.exceeded",
            Self::QuotaNearThreshold => "quota.near_threshold",
            Self::ReplicationFinished => "replication.finished",
            Self::RetentionFinished => "retention.finished",
            Self::GcFinished => "gc.finished",
            Self::PromotionRequested => "promotion.requested",
            Self::PromotionApproved => "promotion.approved",
            Self::PromotionRejected => "promotion.rejected",
            Self::PromotionCompleted => "promotion.completed",
            Self::TaskFinished => "task.finished",
            Self::TokenExpiring => "token.expiring",
        }
    }

    /// Parses a wire name; `None` for anything that is not in the catalog.
    #[must_use]
    pub fn parse(label: &str) -> Option<Self> {
        Self::ALL
            .iter()
            .copied()
            .find(|kind| kind.as_str() == label)
    }

    /// Everything except the two internal kinds. A webhook can only subscribe to these.
    #[must_use]
    pub const fn webhook_subscribable(self) -> bool {
        !matches!(self, Self::TaskFinished | Self::TokenExpiring)
    }

    /// `Pull` only: written to the outbox only while some enabled webhook subscribes to it.
    #[must_use]
    pub const fn high_volume(self) -> bool {
        matches!(self, Self::Pull)
    }
}

/// An event as a producer describes it. The database assigns id, sequence and time.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct EventDraft {
    /// What happened.
    pub kind: EventKind,
    /// The project the event belongs to, for scope matching of webhooks and subscriptions.
    pub project_id: Option<ProjectId>,
    /// The repository the event belongs to.
    pub repository_id: Option<RepositoryId>,
    /// Who caused it.
    pub actor: ActorRef,
    /// Request id for log correlation.
    pub request_id: Option<String>,
    /// Payload `data`. No secrets, no client addresses, at most 16 KiB (a larger value is
    /// `StoreError::Invalid`).
    pub data: serde_json::Value,
}

impl EventDraft {
    /// A draft without project, repository or request id.
    #[must_use]
    pub fn new(kind: EventKind, actor: ActorRef, data: serde_json::Value) -> Self {
        Self {
            kind,
            project_id: None,
            repository_id: None,
            actor,
            request_id: None,
            data,
        }
    }

    /// Checks the payload limit that the outbox writer enforces.
    ///
    /// # Errors
    /// Returns [`StoreError::Invalid`] when `data` is larger than 16 KiB.
    pub fn validate(&self) -> Result<(), StoreError> {
        let size = serde_json::to_vec(&self.data).map_or(usize::MAX, |bytes| bytes.len());
        if size > MAX_EVENT_DATA_BYTES {
            return Err(StoreError::Invalid(
                "event data must be at most 16 KiB".to_owned(),
            ));
        }
        Ok(())
    }
}

/// An event as stored and read back by a consumer.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct OutboxEvent {
    /// Stable across delivery attempts; receivers deduplicate on it.
    pub id: EventId,
    /// Sequence value assigned at insert (commit order can differ, see the relay rules).
    pub seq: i64,
    /// When the event was written.
    pub occurred_at: OffsetDateTime,
    /// What happened.
    pub kind: EventKind,
    /// The project the event belongs to.
    pub project_id: Option<ProjectId>,
    /// The repository the event belongs to.
    pub repository_id: Option<RepositoryId>,
    /// Who caused it.
    pub actor: ActorRef,
    /// Request id for log correlation.
    pub request_id: Option<String>,
    /// Payload.
    pub data: serde_json::Value,
}

impl OutboxEvent {
    /// An event without project, repository or request id.
    #[must_use]
    pub fn new(
        id: EventId,
        seq: i64,
        occurred_at: OffsetDateTime,
        kind: EventKind,
        actor: ActorRef,
        data: serde_json::Value,
    ) -> Self {
        Self {
            id,
            seq,
            occurred_at,
            kind,
            project_id: None,
            repository_id: None,
            actor,
            request_id: None,
            data,
        }
    }
}

/// Read position of one consumer: the transaction id and sequence of the last event it
/// handled. Opaque to callers (`tx_id` is the `xid8` of the writing transaction as `u64`).
/// The derived order is `(tx_id, seq)`, the order events are handed out in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub struct OutboxCursor {
    /// Transaction id of the last handled event.
    pub tx_id: u64,
    /// Sequence of the last handled event.
    pub seq: i64,
}

/// A batch of events and the cursor move that goes with handling it.
#[derive(Debug, Clone)]
pub struct OutboxBatch {
    /// Events in `(tx_id, seq)` order.
    pub events: Vec<OutboxEvent>,
    /// The cursor the batch was read from.
    pub from: OutboxCursor,
    /// The cursor after the last event of the batch.
    pub to: OutboxCursor,
}

impl OutboxBatch {
    /// An empty batch at `cursor`: `from == to`.
    #[must_use]
    pub const fn empty(cursor: OutboxCursor) -> Self {
        Self {
            events: Vec::new(),
            from: cursor,
            to: cursor,
        }
    }
}

/// Moves a consumer's cursor from `from` to `to` inside the transaction of the consuming
/// write.
#[derive(Debug, Clone)]
pub struct CursorAdvance {
    /// Consumer name (`webhooks`, `email`, `replication`, `scan`).
    pub consumer: &'static str,
    /// Expected current position (compare and set).
    pub from: OutboxCursor,
    /// New position.
    pub to: OutboxCursor,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::assert_wire;

    /// Position of a kind, with no wildcard arm: adding a variant breaks this function until
    /// `ALL`, the wire name and the subscribability decision are updated.
    fn position(kind: EventKind) -> usize {
        match kind {
            EventKind::Push => 0,
            EventKind::Pull => 1,
            EventKind::Delete => 2,
            EventKind::ScanCompleted => 3,
            EventKind::ScanFailed => 4,
            EventKind::ScanStopped => 5,
            EventKind::PullBlocked => 6,
            EventKind::QuarantineEntered => 7,
            EventKind::QuarantineReleased => 8,
            EventKind::QuotaExceeded => 9,
            EventKind::QuotaNearThreshold => 10,
            EventKind::ReplicationFinished => 11,
            EventKind::RetentionFinished => 12,
            EventKind::GcFinished => 13,
            EventKind::PromotionRequested => 14,
            EventKind::PromotionApproved => 15,
            EventKind::PromotionRejected => 16,
            EventKind::PromotionCompleted => 17,
            EventKind::TaskFinished => 18,
            EventKind::TokenExpiring => 19,
        }
    }

    #[test]
    fn all_lists_every_kind_exactly_once_in_order() {
        assert_eq!(EventKind::ALL.len(), 20);
        for (index, kind) in EventKind::ALL.iter().enumerate() {
            assert_eq!(position(*kind), index, "{kind:?}");
        }
    }

    #[test]
    fn wire_names_are_dotted_and_match_as_str_and_parse() {
        for kind in EventKind::ALL {
            assert_wire(kind, &format!("\"{}\"", kind.as_str()));
            assert_eq!(EventKind::parse(kind.as_str()), Some(*kind));
        }
        assert_wire(&EventKind::QuotaNearThreshold, "\"quota.near_threshold\"");
        assert_wire(&EventKind::ScanCompleted, "\"scan.completed\"");
    }

    #[test]
    fn parse_rejects_anything_outside_the_catalog() {
        for bad in [
            "",
            "Push",
            "scan",
            "scan.completed ",
            "scan_completed",
            "deploy",
            "pull.*",
        ] {
            assert_eq!(EventKind::parse(bad), None, "{bad:?}");
        }
        assert!(serde_json::from_str::<EventKind>("\"deploy\"").is_err());
    }

    #[test]
    fn must_not_let_a_webhook_subscribe_to_the_internal_kinds() {
        for kind in EventKind::ALL {
            let internal = matches!(kind, EventKind::TaskFinished | EventKind::TokenExpiring);
            assert_eq!(kind.webhook_subscribable(), !internal, "{kind:?}");
        }
    }

    #[test]
    fn only_pull_is_high_volume() {
        for kind in EventKind::ALL {
            assert_eq!(kind.high_volume(), *kind == EventKind::Pull, "{kind:?}");
        }
    }

    #[test]
    fn draft_constructor_and_payload_limit() {
        let actor = ActorRef::system("test");
        let mut draft = EventDraft::new(EventKind::Push, actor, serde_json::json!({"k": "v"}));
        assert!(
            draft.project_id.is_none()
                && draft.repository_id.is_none()
                && draft.request_id.is_none()
        );
        assert!(draft.validate().is_ok());
        draft.data = serde_json::json!({"k": "x".repeat(MAX_EVENT_DATA_BYTES)});
        assert!(matches!(draft.validate(), Err(StoreError::Invalid(_))));
        // {"k":"..."} costs 8 bytes of framing, so this payload is exactly at the limit.
        draft.data = serde_json::json!({"k": "x".repeat(MAX_EVENT_DATA_BYTES - 8)});
        assert!(draft.validate().is_ok());
    }

    #[test]
    fn outbox_event_constructor_defaults() {
        let event = OutboxEvent::new(
            EventId::generate(),
            5,
            crate::testing::at(),
            EventKind::Delete,
            ActorRef::system("gc"),
            serde_json::json!({}),
        );
        assert_eq!(event.seq, 5);
        assert!(
            event.project_id.is_none()
                && event.repository_id.is_none()
                && event.request_id.is_none()
        );
    }

    #[test]
    fn cursor_orders_by_transaction_then_sequence() {
        let a = OutboxCursor { tx_id: 1, seq: 9 };
        let b = OutboxCursor { tx_id: 2, seq: 1 };
        let c = OutboxCursor { tx_id: 2, seq: 2 };
        assert!(a < b && b < c);
        assert_eq!(OutboxCursor::default(), OutboxCursor { tx_id: 0, seq: 0 });
    }

    #[test]
    fn an_empty_batch_carries_equal_cursors() {
        let cursor = OutboxCursor { tx_id: 7, seq: 3 };
        let batch = OutboxBatch::empty(cursor);
        assert!(batch.events.is_empty());
        assert_eq!(batch.from, batch.to);
        assert_eq!(batch.from, cursor);
    }
}
