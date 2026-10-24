//! Audit rows of sign-ins and sign-outs.

use std::net::IpAddr;

use rustryx_core::audit::{ActorKind, AuditEvent, AuditKind};
use rustryx_core::ids::UserId;

use super::AuthApi;
use crate::error::ApiError;
use crate::request_id::current_request_id;

/// Longest attempted name written to the audit log: it is attacker supplied text.
const MAX_ACTOR_CHARS: usize = 256;

/// What an audit row is about.
pub(super) struct Entry<'a> {
    pub kind: AuditKind,
    pub action: &'static str,
    pub success: bool,
    pub actor_name: &'a str,
    pub actor_id: Option<UserId>,
    pub client_ip: Option<IpAddr>,
    pub detail: serde_json::Value,
}

fn clean(name: &str) -> String {
    name.chars()
        .filter(|c| !c.is_control())
        .take(MAX_ACTOR_CHARS)
        .collect()
}

impl AuthApi {
    /// Writes one row. A failure of the sink is returned: a successful sign-in without its audit
    /// row must not happen (the caller fails the request), while the row of a refusal is best
    /// effort (the caller logs and answers the refusal it was going to answer).
    pub(super) async fn record(&self, entry: Entry<'_>) -> Result<(), ApiError> {
        let outcome = if entry.success { "success" } else { "failure" };
        let mut event = AuditEvent::new(entry.kind, clean(entry.actor_name), entry.action, outcome);
        event.at = self.clock.now();
        event.actor_id = entry.actor_id;
        event.actor_kind = ActorKind::User;
        event.client_ip = entry.client_ip.map(|ip| ip.to_string());
        event.request_id = current_request_id().map(|id| id.as_str().to_owned());
        event.detail = entry.detail;
        self.audit.record(event).await.map_err(ApiError::from)
    }

    /// Best effort: logs a failed write and goes on.
    pub(super) async fn record_best_effort(&self, entry: Entry<'_>) {
        let action = entry.action;
        if let Err(error) = self.record(entry).await {
            tracing::error!(%error, action, "audit row could not be written");
        }
    }
}
