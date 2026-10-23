//! Audit events and the description of who acted.

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::identity::Principal;
use crate::ids::{ProjectId, UserId};

/// Category of an audit event. The database column is plain text, so a new variant needs no
/// migration, but the text mapping of `rustryx-db` must accept it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
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
    /// Project, member, label or visibility change.
    ProjectChange,
    /// Retention, immutability, quota, allowlist, trust, gate or hold change.
    PolicyChange,
    /// Pull blocked, immutable reject, quota reject, SSRF reject, quarantine.
    SecurityDecision,
    /// Batch deletions of cleanup, retention, promotion and garbage collection.
    LifecycleAction,
    /// Settings sections, read only mode, trust store, log levels.
    SettingsChange,
    /// Webhook, registry, replication rule or scanner change.
    IntegrationChange,
    /// Promotion request, approval or execution.
    PromotionChange,
}

impl AuditKind {
    /// Every kind, in declaration order.
    pub const ALL: &'static [Self] = &[
        Self::LoginSuccess,
        Self::LoginFailure,
        Self::RbacChange,
        Self::TokenChange,
        Self::ConfigChange,
        Self::ContentChange,
        Self::ProjectChange,
        Self::PolicyChange,
        Self::SecurityDecision,
        Self::LifecycleAction,
        Self::SettingsChange,
        Self::IntegrationChange,
        Self::PromotionChange,
    ];

    /// The snake case text stored in the database and written to JSON.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::LoginSuccess => "login_success",
            Self::LoginFailure => "login_failure",
            Self::RbacChange => "rbac_change",
            Self::TokenChange => "token_change",
            Self::ConfigChange => "config_change",
            Self::ContentChange => "content_change",
            Self::ProjectChange => "project_change",
            Self::PolicyChange => "policy_change",
            Self::SecurityDecision => "security_decision",
            Self::LifecycleAction => "lifecycle_action",
            Self::SettingsChange => "settings_change",
            Self::IntegrationChange => "integration_change",
            Self::PromotionChange => "promotion_change",
        }
    }

    /// Parses the stored text; `None` for a value this build does not know.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        Self::ALL.iter().copied().find(|kind| kind.as_str() == text)
    }
}

/// The kind of actor behind an event.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ActorKind {
    /// A person (default).
    #[default]
    User,
    /// A personal API token.
    Token,
    /// A robot token or a scan token.
    Robot,
    /// An unauthenticated caller.
    Anonymous,
    /// Rustryx itself (a scheduled task, the replication worker, the promotion executor).
    System,
}

impl ActorKind {
    /// The snake case text stored in the database.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Token => "token",
            Self::Robot => "robot",
            Self::Anonymous => "anonymous",
            Self::System => "system",
        }
    }

    /// Parses the stored text.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        [
            Self::User,
            Self::Token,
            Self::Robot,
            Self::Anonymous,
            Self::System,
        ]
        .into_iter()
        .find(|kind| kind.as_str() == text)
    }
}

/// Who acted, for events and audit rows.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActorRef {
    /// Account id, when there is one.
    pub id: Option<UserId>,
    /// Account name, or the name of the system component.
    pub name: String,
    /// Kind of actor.
    pub kind: ActorKind,
}

impl ActorRef {
    /// A system actor such as `system:replication:<rule id>` or `task:blob_gc`.
    #[must_use]
    pub fn system(name: impl Into<String>) -> Self {
        Self {
            id: None,
            name: name.into(),
            kind: ActorKind::System,
        }
    }

    /// The actor behind a principal; the kind follows [`Principal::actor_kind`].
    #[must_use]
    pub fn of(principal: &Principal) -> Self {
        Self {
            id: principal.user_id,
            name: principal.username.clone(),
            kind: principal.actor_kind(),
        }
    }
}

/// One audit record. Must never contain secrets, tokens or passwords.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct AuditEvent {
    /// Event time.
    #[serde(with = "crate::rfc3339")]
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
    /// Actor type. Defaults to a user.
    #[serde(default)]
    pub actor_kind: ActorKind,
    /// The project the event is scoped to, for the project log view. Indexed.
    #[serde(default)]
    pub project_id: Option<ProjectId>,
}

impl AuditEvent {
    /// An event stamped with the current time, from a user, with no id, address, request id,
    /// target, project or detail. Override `at` when a `Clock` is injected.
    #[must_use]
    pub fn new(
        kind: AuditKind,
        actor_name: impl Into<String>,
        action: impl Into<String>,
        outcome: impl Into<String>,
    ) -> Self {
        Self {
            at: OffsetDateTime::now_utc(),
            kind,
            actor_id: None,
            actor_name: actor_name.into(),
            client_ip: None,
            request_id: None,
            action: action.into(),
            target: None,
            outcome: outcome.into(),
            detail: serde_json::json!({}),
            actor_kind: ActorKind::User,
            project_id: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::{AuthMethod, UserSource};
    use crate::testing::{assert_wire, at, round_trip_json};

    /// No wildcard arm: a new kind has to be listed here, in `ALL` and in `as_str`.
    fn position(kind: AuditKind) -> usize {
        match kind {
            AuditKind::LoginSuccess => 0,
            AuditKind::LoginFailure => 1,
            AuditKind::RbacChange => 2,
            AuditKind::TokenChange => 3,
            AuditKind::ConfigChange => 4,
            AuditKind::ContentChange => 5,
            AuditKind::ProjectChange => 6,
            AuditKind::PolicyChange => 7,
            AuditKind::SecurityDecision => 8,
            AuditKind::LifecycleAction => 9,
            AuditKind::SettingsChange => 10,
            AuditKind::IntegrationChange => 11,
            AuditKind::PromotionChange => 12,
        }
    }

    #[test]
    fn kinds_are_complete_and_their_text_matches_serde() {
        assert_eq!(AuditKind::ALL.len(), 13);
        for (index, kind) in AuditKind::ALL.iter().enumerate() {
            assert_eq!(position(*kind), index, "{kind:?}");
            assert_wire(kind, &format!("\"{}\"", kind.as_str()));
            assert_eq!(AuditKind::parse(kind.as_str()), Some(*kind));
        }
        assert_eq!(AuditKind::parse("login"), None);
        assert_eq!(AuditKind::parse(""), None);
    }

    #[test]
    fn actor_kind_text_round_trips() {
        for kind in [
            ActorKind::User,
            ActorKind::Token,
            ActorKind::Robot,
            ActorKind::Anonymous,
            ActorKind::System,
        ] {
            assert_eq!(ActorKind::parse(kind.as_str()), Some(kind));
            assert_wire(&kind, &format!("\"{}\"", kind.as_str()));
        }
        assert_eq!(ActorKind::parse("bot"), None);
        assert_eq!(ActorKind::default(), ActorKind::User);
    }

    #[test]
    fn constructor_sets_documented_defaults() {
        let before = OffsetDateTime::now_utc();
        let event = AuditEvent::new(AuditKind::ContentChange, "alice", "asset.delete", "success");
        assert!(event.at >= before);
        assert_eq!(event.actor_kind, ActorKind::User);
        assert!(event.project_id.is_none() && event.actor_id.is_none());
        assert!(event.client_ip.is_none() && event.request_id.is_none() && event.target.is_none());
        assert!(
            event
                .detail
                .as_object()
                .is_some_and(serde_json::Map::is_empty)
        );
        assert_eq!(event.outcome, "success");
    }

    #[test]
    fn event_serializes_rfc3339_and_reads_rows_without_the_new_columns() {
        let mut event = AuditEvent::new(AuditKind::PolicyChange, "bob", "quota.update", "success");
        event.at = at();
        event.actor_kind = ActorKind::Robot;
        event.project_id = Some(ProjectId::generate());
        let json = round_trip_json(&event);
        assert_eq!(json["at"], "2026-10-24T10:00:00Z");
        assert_eq!(json["actor_kind"], "robot");
        assert_eq!(json["kind"], "policy_change");

        let mut old = json;
        let map = old.as_object_mut().expect("object");
        map.remove("actor_kind");
        map.remove("project_id");
        let back: AuditEvent = serde_json::from_value(old).expect("N-1 row parses");
        assert_eq!(back.actor_kind, ActorKind::User);
        assert!(back.project_id.is_none());
    }

    #[test]
    fn actor_ref_of_follows_the_principal() {
        let anonymous = ActorRef::of(&Principal::anonymous());
        assert_eq!(
            (anonymous.kind, anonymous.name.as_str()),
            (ActorKind::Anonymous, "anonymous")
        );
        assert!(anonymous.id.is_none());

        let mut user = Principal::new("alice", UserSource::Local, AuthMethod::BasicLocal);
        user.user_id = Some(UserId::generate());
        let actor = ActorRef::of(&user);
        assert_eq!((actor.id, actor.kind), (user.user_id, ActorKind::User));

        let robot = Principal::new("robot+ci", UserSource::Local, AuthMethod::RobotToken);
        assert_eq!(ActorRef::of(&robot).kind, ActorKind::Robot);
    }

    #[test]
    fn system_actor_has_no_account() {
        let actor = ActorRef::system("task:blob_gc");
        assert_eq!(actor.kind, ActorKind::System);
        assert!(actor.id.is_none());
        assert_wire(
            &actor,
            r#"{"id":null,"name":"task:blob_gc","kind":"system"}"#,
        );
    }
}
