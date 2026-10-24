//! Audit records for decisions the format makes itself (the pipelines audit uploads, deletes
//! and their own refusals).

use rustryx_core::audit::{AuditEvent, AuditKind};
use rustryx_format_core::{ArtifactRequest, FormatContext};

/// Records an event about `target` for the caller of `req`. A failing sink is logged and
/// swallowed: the decision it describes is already made and cannot be undone here.
pub(crate) async fn record(
    cx: &FormatContext,
    req: &ArtifactRequest,
    kind: AuditKind,
    action: &str,
    outcome: &str,
    detail: serde_json::Value,
) {
    let principal = cx.principal();
    let mut event = AuditEvent::new(kind, principal.username.clone(), action, outcome);
    // `new` stamps the wall clock; the injected clock is the one the rest of the request used.
    event.at = cx.now();
    event.actor_id = principal.user_id;
    event.actor_kind = principal.actor_kind();
    event.client_ip = req.head.client.ip.map(|ip| ip.to_string());
    event.request_id.clone_from(&req.head.client.request_id);
    event.target = Some(format!("{}:{}", cx.repository().name, req.path));
    event.detail = detail;
    event.project_id = cx.repository().project.as_ref().map(|project| project.id);
    if let Err(error) = cx.ports().audit.record(event).await {
        tracing::error!(%error, action, "audit record failed");
    }
}
