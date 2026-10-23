//! Audit sink that keeps events in memory.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use rustryx_core::audit::AuditEvent;
use rustryx_core::error::StoreError;
use rustryx_core::ports::AuditSink;

use crate::support::lock;

/// [`AuditSink`] that appends to a shared vector, in arrival order.
#[derive(Debug, Clone, Default)]
pub struct MemAuditSink {
    events: Arc<Mutex<Vec<AuditEvent>>>,
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
        lock(&self.events).clone()
    }

    /// Forgets recorded events, for tests with several phases.
    pub fn clear(&self) {
        lock(&self.events).clear();
    }
}

#[async_trait]
impl AuditSink for MemAuditSink {
    async fn record(&self, event: AuditEvent) -> Result<(), StoreError> {
        lock(&self.events).push(event);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use rustryx_core::audit::AuditKind;
    use time::OffsetDateTime;

    use super::*;

    fn event(action: &str) -> AuditEvent {
        AuditEvent {
            at: OffsetDateTime::UNIX_EPOCH,
            kind: AuditKind::RbacChange,
            actor_id: None,
            actor_name: "tester".to_owned(),
            client_ip: None,
            request_id: None,
            action: action.to_owned(),
            target: None,
            outcome: "success".to_owned(),
            detail: serde_json::Value::Null,
        }
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
