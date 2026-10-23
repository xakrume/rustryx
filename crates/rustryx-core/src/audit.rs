//! Audit events.

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::ids::UserId;

/// Category of an audit event.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuditKind {
    /// Successful login (any method).
    LoginSuccess,
    /// Failed login.
    LoginFailure,
    /// Role, privilege, selector, mapping or user change.
    RbacChange,
    /// API token created or revoked.
    TokenChange,
    /// Repository or blob store configuration change.
    ConfigChange,
    /// Artifact upload, overwrite or delete.
    ContentChange,
}

/// One audit record. Must never contain secrets, tokens or passwords.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditEvent {
    /// Event time.
    pub at: OffsetDateTime,
    /// Category.
    pub kind: AuditKind,
    /// Acting user id, when known.
    pub actor_id: Option<UserId>,
    /// Acting user name (or the attempted name for failed logins).
    pub actor_name: String,
    /// Peer address.
    pub client_ip: Option<String>,
    /// Request id.
    pub request_id: Option<String>,
    /// Short action label, for example `role.update`.
    pub action: String,
    /// Target description, for example a repository name or role name.
    pub target: Option<String>,
    /// `success` or `failure`.
    pub outcome: String,
    /// Extra non-secret detail (JSON object).
    pub detail: serde_json::Value,
}
