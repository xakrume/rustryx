//! Audit records of registry changes that the hosted pipeline does not make itself: moving a
//! tag, linking a mounted blob, recording a referrer, deleting a manifest.
//!
//! The pipeline audits every upload and delete it performs (`asset.publish`,
//! `asset.delete`); these are the operations that go straight to the ports.

use rustryx_core::audit::{AuditEvent, AuditKind};
use rustryx_format_core::FormatContext;
use serde_json::Value;

/// Records one content change. A failing sink is logged and never undoes the change: it is
/// already committed.
pub(crate) async fn record(cx: &FormatContext, action: &str, target: String, detail: Value) {
    let principal = cx.principal();
    let mut event = AuditEvent::new(
        AuditKind::ContentChange,
        principal.username.clone(),
        action,
        "success",
    );
    event.at = cx.now();
    event.actor_id = principal.user_id;
    event.actor_kind = principal.actor_kind();
    event.request_id = cx.request_id().map(str::to_owned);
    event.target = Some(target);
    event.detail = detail;
    event.project_id = cx.repository().project.as_ref().map(|project| project.id);
    if let Err(error) = cx.ports().audit.record(event).await {
        tracing::error!(%error, action, "audit record for a registry change failed");
    }
}
