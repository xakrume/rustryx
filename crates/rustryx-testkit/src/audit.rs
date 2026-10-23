//! Audit sink and audit read side that keep events in memory.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use futures::StreamExt;
use futures::stream::BoxStream;
use rustryx_core::audit::AuditEvent;
use rustryx_core::error::StoreError;
use rustryx_core::ports::{
    AuditQuery, AuditReadRepo, AuditSink, Page, PageRequest, StoredAuditEvent,
};
use uuid::Uuid;

use crate::store::paginate;
use crate::support::lock;

/// [`AuditSink`] and [`AuditReadRepo`] over one shared vector, in arrival order.
///
/// Writing and reading are one object on purpose: `PgAuditSink` and the audit read side both
/// work on the `audit_events` table, so a test that records through the sink and reads
/// through the port sees what a deployment sees. `MemStore::audit_sink` hands out the sink
/// the store reads from.
#[derive(Debug, Clone, Default)]
pub struct MemAuditSink {
    events: Arc<Mutex<Vec<StoredAuditEvent>>>,
}

impl MemAuditSink {
    /// Creates an empty sink.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Snapshot of everything recorded so far.
    #[must_use]
    pub fn events(&self) -> Vec<AuditEvent> {
        lock(&self.events)
            .iter()
            .map(|stored| stored.event.clone())
            .collect()
    }

    /// Forgets recorded events, for tests with several phases.
    pub fn clear(&self) {
        lock(&self.events).clear();
    }

    fn matching(&self, query: &AuditQuery) -> Vec<StoredAuditEvent> {
        lock(&self.events)
            .iter()
            .filter(|stored| matches_query(&stored.event, query))
            .cloned()
            .collect()
    }

    /// The read side without the availability switch of the store, shared by both owners.
    pub(crate) fn query_page(
        &self,
        query: &AuditQuery,
        page: &PageRequest,
    ) -> Result<Page<StoredAuditEvent>, StoreError> {
        let rows = self
            .matching(query)
            .into_iter()
            .map(|stored| (newest_first_key(&stored), stored))
            .collect();
        paginate(rows, page)
    }

    /// Oldest first snapshot for the export stream.
    pub(crate) fn export_rows(&self, query: &AuditQuery) -> Vec<StoredAuditEvent> {
        let mut rows = self.matching(query);
        rows.sort_by_key(|stored| (stored.event.at, stored.id));
        rows
    }
}

/// Every set field of the query must match; `from` is inclusive and `to` exclusive, an empty
/// `kinds` matches all kinds.
fn matches_query(event: &AuditEvent, query: &AuditQuery) -> bool {
    query.from.is_none_or(|from| event.at >= from)
        && query.to.is_none_or(|to| event.at < to)
        && (query.kinds.is_empty() || query.kinds.contains(&event.kind))
        && query
            .actor_id
            .is_none_or(|actor| event.actor_id == Some(actor))
        && query
            .actor_name
            .as_ref()
            .is_none_or(|name| &event.actor_name == name)
        && query
            .action_prefix
            .as_ref()
            .is_none_or(|prefix| event.action.starts_with(prefix.as_str()))
        && query
            .target
            .as_ref()
            .is_none_or(|target| event.target.as_ref() == Some(target))
        && query
            .project_id
            .is_none_or(|project| event.project_id == Some(project))
        && query
            .outcome
            .as_ref()
            .is_none_or(|outcome| &event.outcome == outcome)
}

/// Sort key whose ascending order is `(event_time, id)` descending, the order of the port.
/// The time is shifted into the positive range and inverted, so a plain string comparison
/// of fixed width keys does the work and the page token (the last key) stays valid when rows
/// arrive between two requests.
fn newest_first_key(stored: &StoredAuditEvent) -> String {
    const SHIFT: i128 = 1 << 100;
    let nanos = stored.event.at.unix_timestamp_nanos();
    let shifted = u128::try_from(nanos.saturating_add(SHIFT)).unwrap_or(0);
    format!(
        "{:039}{:039}",
        u128::MAX - shifted,
        u128::MAX - stored.id.as_u128()
    )
}

#[async_trait]
impl AuditSink for MemAuditSink {
    async fn record(&self, event: AuditEvent) -> Result<(), StoreError> {
        lock(&self.events).push(StoredAuditEvent {
            id: Uuid::now_v7(),
            event,
        });
        Ok(())
    }
}

#[async_trait]
impl AuditReadRepo for MemAuditSink {
    async fn query(
        &self,
        query: &AuditQuery,
        page: &PageRequest,
    ) -> Result<Page<StoredAuditEvent>, StoreError> {
        self.query_page(query, page)
    }

    fn export(
        &self,
        query: &AuditQuery,
    ) -> BoxStream<'static, Result<StoredAuditEvent, StoreError>> {
        futures::stream::iter(self.export_rows(query).into_iter().map(Ok)).boxed()
    }
}

#[cfg(test)]
mod tests {
    use rustryx_core::audit::AuditKind;
    use time::OffsetDateTime;

    use super::*;

    fn event(action: &str) -> AuditEvent {
        let mut event = AuditEvent::new(AuditKind::RbacChange, "tester", action, "success");
        event.at = OffsetDateTime::UNIX_EPOCH;
        event
    }

    #[tokio::test]
    async fn records_in_order_and_clones_share_the_log() {
        let sink = MemAuditSink::new();
        let alias = sink.clone();
        sink.record(event("first")).await.unwrap();
        alias.record(event("second")).await.unwrap();
        let actions: Vec<String> = sink.events().into_iter().map(|e| e.action).collect();
        assert_eq!(actions, ["first", "second"]);
        sink.clear();
        assert!(alias.events().is_empty());
    }
}
