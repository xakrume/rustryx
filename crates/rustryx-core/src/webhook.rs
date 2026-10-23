//! Webhooks and their deliveries.
//!
//! A webhook receives the events of its scope as signed HTTP requests. The signing secret is
//! mandatory (a webhook without one cannot exist), sealed at rest and shown once, at creation
//! or rotation; afterwards the API returns a `SecretStatus`.

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::error::DomainError;
use crate::events::EventKind;
use crate::ids::{EventId, ProjectId, RepositoryId, UserId, WebhookDeliveryId, WebhookId};
use crate::secret::Secret;

/// Most events one webhook may subscribe to.
pub const MAX_WEBHOOK_EVENTS: usize = 32;
/// Most webhooks one scope may hold (enforced by the repository).
pub const MAX_WEBHOOKS_PER_SCOPE: usize = 20;
/// Longest webhook URL, in characters.
pub const MAX_WEBHOOK_URL_CHARS: usize = 2048;
/// Default total attempts per delivery.
pub const DEFAULT_RETRY_MAX: u8 = 5;
/// Default timeout of one attempt, in seconds.
pub const DEFAULT_TIMEOUT_SECS: u8 = 10;
/// Longest stored delivery error code, in characters.
pub const MAX_DELIVERY_ERROR_CHARS: usize = 64;
/// Longest stored response excerpt, in characters.
pub const MAX_RESPONSE_EXCERPT_CHARS: usize = 1024;

/// The body format of a delivery.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WebhookKind {
    /// The default JSON envelope.
    Http,
    /// `CloudEvents` structured mode.
    CloudEvents,
}

/// Whose events a webhook or a notification subscription receives.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", content = "id", rename_all = "snake_case")]
pub enum NotifyScope {
    /// Every event of the installation.
    System,
    /// Events of one project.
    Project(ProjectId),
    /// Events of one repository.
    Repository(RepositoryId),
}

/// A webhook.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct Webhook {
    /// Identifier.
    pub id: WebhookId,
    /// Whose events it receives. A project scoped webhook never receives another project's
    /// events.
    pub scope: NotifyScope,
    /// Display name.
    pub name: String,
    /// Body format.
    pub kind: WebhookKind,
    /// `https`, no user info, no fragment; checked against the `Webhook` egress policy on
    /// create, update and on every delivery.
    pub url: String,
    /// The HMAC signing secret. Mandatory. Redacted in `Debug`.
    pub secret: Secret,
    /// Optional value of an `Authorization` header sent with every delivery. Redacted in
    /// `Debug`.
    pub auth_header: Option<Secret>,
    /// 1 to 32 subscribable kinds. A webhook cannot subscribe to the internal kinds.
    pub events: Vec<EventKind>,
    /// A disabled webhook receives nothing.
    pub enabled: bool,
    /// Set after `webhooks.suspend_after_failures` (default 100) consecutive dead deliveries.
    /// A suspended webhook gets no new deliveries until re-enabled.
    #[serde(default, with = "crate::rfc3339::option")]
    pub suspended_at: Option<OffsetDateTime>,
    /// Dead deliveries in a row.
    pub consecutive_failures: u32,
    /// Total attempts per delivery, 1 to 10, default 5.
    pub retry_max: u8,
    /// Per attempt total timeout, 1 to 30 seconds, default 10.
    pub timeout_secs: u8,
    /// Optimistic concurrency version.
    pub version: i64,
    /// Creator.
    pub created_by: Option<UserId>,
    /// Creation time.
    #[serde(with = "crate::rfc3339")]
    pub created_at: OffsetDateTime,
}

impl Webhook {
    /// An enabled JSON webhook with 5 attempts and a 10 second timeout. Assign `kind`,
    /// `auth_header`, `retry_max` and `timeout_secs` afterwards if they differ.
    #[must_use]
    pub fn new(
        id: WebhookId,
        scope: NotifyScope,
        name: impl Into<String>,
        url: impl Into<String>,
        secret: Secret,
        events: Vec<EventKind>,
        now: OffsetDateTime,
    ) -> Self {
        Self {
            id,
            scope,
            name: name.into(),
            kind: WebhookKind::Http,
            url: url.into(),
            secret,
            auth_header: None,
            events,
            enabled: true,
            suspended_at: None,
            consecutive_failures: 0,
            retry_max: DEFAULT_RETRY_MAX,
            timeout_secs: DEFAULT_TIMEOUT_SECS,
            version: 1,
            created_by: None,
            created_at: now,
        }
    }

    /// Checks the rules of the entity: a secret that is not empty, a name of 1 to 128
    /// characters, a URL of at most 2048 characters that starts with `https://` (the egress
    /// policy decides the rest), 1 to 32 distinct subscribable kinds, 1 to 10 attempts and a
    /// timeout of 1 to 30 seconds.
    ///
    /// # Errors
    /// Returns [`DomainError::Invalid`] naming the broken rule.
    pub fn validate(&self) -> Result<(), DomainError> {
        if self.secret.is_empty() {
            return Err(DomainError::invalid("a webhook needs a signing secret"));
        }
        if self.name.is_empty() || self.name.chars().count() > 128 {
            return Err(DomainError::invalid(
                "webhook name must be 1 to 128 characters",
            ));
        }
        if self.url.chars().count() > MAX_WEBHOOK_URL_CHARS || !self.url.starts_with("https://") {
            return Err(DomainError::invalid(
                "webhook URL must be an https URL of at most 2048 characters",
            ));
        }
        if self.events.is_empty() || self.events.len() > MAX_WEBHOOK_EVENTS {
            return Err(DomainError::invalid(
                "a webhook subscribes to 1 to 32 events",
            ));
        }
        if self.events.iter().any(|kind| !kind.webhook_subscribable()) {
            return Err(DomainError::invalid(
                "a webhook cannot subscribe to internal events",
            ));
        }
        for (index, kind) in self.events.iter().enumerate() {
            if self.events[..index].contains(kind) {
                return Err(DomainError::invalid("a webhook lists each event once"));
            }
        }
        if !(1..=10).contains(&self.retry_max) {
            return Err(DomainError::invalid("retry_max must be 1 to 10"));
        }
        if !(1..=30).contains(&self.timeout_secs) {
            return Err(DomainError::invalid("timeout_secs must be 1 to 30"));
        }
        Ok(())
    }

    /// True while the webhook may receive deliveries: enabled and not suspended.
    #[must_use]
    pub fn is_deliverable(&self) -> bool {
        self.enabled && self.suspended_at.is_none()
    }

    /// True when the webhook subscribes to `kind`.
    #[must_use]
    pub fn subscribes_to(&self, kind: EventKind) -> bool {
        self.events.contains(&kind)
    }
}

/// State of a webhook delivery.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeliveryStatus {
    /// Waiting for its next attempt.
    Pending,
    /// A worker holds the lease.
    Delivering,
    /// A 2xx answer.
    Succeeded,
    /// The last attempt failed.
    Failed,
    /// Out of attempts.
    Dead,
}

/// One delivery of one event to one webhook.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct WebhookDelivery {
    /// Identifier, stable across attempts (`X-Rustryx-Delivery`).
    pub id: WebhookDeliveryId,
    /// The webhook.
    pub webhook_id: WebhookId,
    /// The event.
    pub event_id: EventId,
    /// The kind of the event.
    pub event_kind: EventKind,
    /// Attempts made so far.
    pub attempt: u8,
    /// State.
    pub status: DeliveryStatus,
    /// When the next attempt is due.
    #[serde(default, with = "crate::rfc3339::option")]
    pub next_attempt_at: Option<OffsetDateTime>,
    /// HTTP status of the last attempt.
    pub last_status_code: Option<u16>,
    /// Latency of the last attempt.
    pub last_latency_ms: Option<u32>,
    /// Stable code (`timeout`, `connect`, `tls`, `blocked_by_policy`, `http_status`,
    /// `redirect_refused`), never a remote message.
    pub last_error: Option<String>,
    /// First 1 KiB of the response body, control characters removed. Never includes request
    /// headers.
    pub response_excerpt: Option<String>,
    /// Creation time.
    #[serde(with = "crate::rfc3339")]
    pub created_at: OffsetDateTime,
    /// When it succeeded.
    #[serde(default, with = "crate::rfc3339::option")]
    pub delivered_at: Option<OffsetDateTime>,
}

impl WebhookDelivery {
    /// A pending delivery that is due at once, with no attempt made.
    #[must_use]
    pub fn new(
        id: WebhookDeliveryId,
        webhook_id: WebhookId,
        event_id: EventId,
        event_kind: EventKind,
        now: OffsetDateTime,
    ) -> Self {
        Self {
            id,
            webhook_id,
            event_id,
            event_kind,
            attempt: 0,
            status: DeliveryStatus::Pending,
            next_attempt_at: Some(now),
            last_status_code: None,
            last_latency_ms: None,
            last_error: None,
            response_excerpt: None,
            created_at: now,
            delivered_at: None,
        }
    }

    /// Checks the stored text limits: error code at most 64 characters, excerpt at most 1024
    /// and without control characters.
    ///
    /// # Errors
    /// Returns [`DomainError::Invalid`] naming the broken rule.
    pub fn validate(&self) -> Result<(), DomainError> {
        if self
            .last_error
            .as_ref()
            .is_some_and(|e| e.chars().count() > MAX_DELIVERY_ERROR_CHARS)
        {
            return Err(DomainError::invalid(
                "a delivery error must be a code of at most 64 characters",
            ));
        }
        if let Some(excerpt) = &self.response_excerpt
            && (excerpt.chars().count() > MAX_RESPONSE_EXCERPT_CHARS
                || excerpt.chars().any(char::is_control))
        {
            return Err(DomainError::invalid(
                "a response excerpt has at most 1024 characters and no control characters",
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{assert_wire, at, round_trip_json};

    fn webhook() -> Webhook {
        Webhook::new(
            WebhookId::generate(),
            NotifyScope::System,
            "ci hook",
            "https://hooks.example.org/rustryx",
            Secret::new("canary-signing-secret-0123456789"),
            vec![EventKind::Push, EventKind::ScanCompleted],
            at(),
        )
    }

    #[test]
    fn constructor_sets_the_documented_defaults() {
        let hook = webhook();
        assert_eq!(hook.kind, WebhookKind::Http);
        assert!(hook.enabled && hook.suspended_at.is_none() && hook.is_deliverable());
        assert_eq!(
            (
                hook.retry_max,
                hook.timeout_secs,
                hook.consecutive_failures,
                hook.version
            ),
            (5, 10, 0, 1)
        );
        assert!(hook.auth_header.is_none());
        assert!(hook.validate().is_ok());
    }

    #[test]
    fn must_not_leak_the_signing_secret_or_the_auth_header_through_debug() {
        let mut hook = webhook();
        hook.auth_header = Some(Secret::new("Bearer canary-header-value"));
        let rendered = format!("{hook:?}");
        assert!(!rendered.contains("canary"), "{rendered}");
        assert!(rendered.contains("REDACTED"));
    }

    #[test]
    fn must_not_store_a_webhook_without_a_secret() {
        let mut hook = webhook();
        hook.secret = Secret::new("");
        assert!(hook.validate().is_err());
    }

    #[test]
    fn must_not_subscribe_to_internal_events_nor_exceed_the_event_limits() {
        for internal in [EventKind::TaskFinished, EventKind::TokenExpiring] {
            let mut hook = webhook();
            hook.events = vec![EventKind::Push, internal];
            assert!(hook.validate().is_err(), "{internal:?}");
        }
        let mut hook = webhook();
        hook.events = Vec::new();
        assert!(hook.validate().is_err(), "at least one event");
        hook.events = EventKind::ALL
            .iter()
            .copied()
            .filter(|k| k.webhook_subscribable())
            .collect();
        assert!(hook.events.len() <= MAX_WEBHOOK_EVENTS && hook.validate().is_ok());
        hook.events = (0..33).map(|_| EventKind::Push).collect();
        assert!(hook.validate().is_err(), "more than 32 entries");
        hook.events = vec![EventKind::Push, EventKind::Push];
        assert!(hook.validate().is_err(), "an event twice");
    }

    #[test]
    fn retry_timeout_url_and_name_limits() {
        for bad in [0u8, 11] {
            let mut hook = webhook();
            hook.retry_max = bad;
            assert!(hook.validate().is_err(), "retry {bad}");
        }
        for bad in [0u8, 31] {
            let mut hook = webhook();
            hook.timeout_secs = bad;
            assert!(hook.validate().is_err(), "timeout {bad}");
        }
        for good in [(1u8, 1u8), (10, 30)] {
            let mut hook = webhook();
            (hook.retry_max, hook.timeout_secs) = good;
            assert!(hook.validate().is_ok(), "{good:?}");
        }
        let mut hook = webhook();
        hook.url = "http://hooks.example.org".into();
        assert!(hook.validate().is_err(), "https only");
        hook.url = format!("https://h.example.org/{}", "a".repeat(2048));
        assert!(hook.validate().is_err(), "URL length");
        hook.url = "https://h.example.org".into();
        hook.name = String::new();
        assert!(hook.validate().is_err());
    }

    #[test]
    fn suspended_or_disabled_webhooks_are_not_deliverable() {
        let mut hook = webhook();
        hook.suspended_at = Some(at());
        assert!(!hook.is_deliverable());
        hook.suspended_at = None;
        hook.enabled = false;
        assert!(!hook.is_deliverable());
        assert!(hook.subscribes_to(EventKind::Push) && !hook.subscribes_to(EventKind::Delete));
    }

    #[test]
    fn scope_and_kind_wire_forms() {
        assert_wire(&NotifyScope::System, r#"{"kind":"system"}"#);
        let project = ProjectId::generate();
        assert_wire(
            &NotifyScope::Project(project),
            &format!(r#"{{"kind":"project","id":"{project}"}}"#),
        );
        assert_wire(&WebhookKind::CloudEvents, "\"cloud_events\"");
        assert_wire(&WebhookKind::Http, "\"http\"");
    }

    #[test]
    fn webhook_wire_form_uses_dotted_event_names_and_rfc3339() {
        let mut hook = webhook();
        hook.suspended_at = Some(at());
        let json = round_trip_json(&hook);
        assert_eq!(
            json["events"],
            serde_json::json!(["push", "scan.completed"])
        );
        assert_eq!(json["created_at"], "2026-10-24T10:00:00Z");
        assert_eq!(json["suspended_at"], "2026-10-24T10:00:00Z");
        assert_eq!(json["scope"]["kind"], "system");
    }

    #[test]
    fn delivery_statuses_match_the_database_check() {
        for (status, name) in [
            (DeliveryStatus::Pending, "pending"),
            (DeliveryStatus::Delivering, "delivering"),
            (DeliveryStatus::Succeeded, "succeeded"),
            (DeliveryStatus::Failed, "failed"),
            (DeliveryStatus::Dead, "dead"),
        ] {
            assert_wire(&status, &format!("\"{name}\""));
            match status {
                DeliveryStatus::Pending
                | DeliveryStatus::Delivering
                | DeliveryStatus::Succeeded
                | DeliveryStatus::Failed
                | DeliveryStatus::Dead => {}
            }
        }
    }

    #[test]
    fn delivery_constructor_and_text_limits() {
        let mut delivery = WebhookDelivery::new(
            WebhookDeliveryId::generate(),
            WebhookId::generate(),
            EventId::generate(),
            EventKind::Push,
            at(),
        );
        assert_eq!(
            (delivery.attempt, delivery.status),
            (0, DeliveryStatus::Pending)
        );
        assert_eq!(delivery.next_attempt_at, Some(at()), "due at once");
        assert!(delivery.validate().is_ok());
        delivery.last_error = Some("e".repeat(65));
        assert!(delivery.validate().is_err());
        delivery.last_error = Some("timeout".into());
        delivery.response_excerpt = Some("x".repeat(1024));
        assert!(delivery.validate().is_ok());
        delivery.response_excerpt = Some("x".repeat(1025));
        assert!(delivery.validate().is_err());
        delivery.response_excerpt = Some("bad\u{1b}[31m".into());
        assert!(
            delivery.validate().is_err(),
            "control characters are removed before storing"
        );
        delivery.response_excerpt = None;
        delivery.delivered_at = Some(at());
        let json = round_trip_json(&delivery);
        assert_eq!(json["created_at"], "2026-10-24T10:00:00Z");
        assert_eq!(json["event_kind"], "push");
    }
}
