//! E-mail notification subscriptions and deliveries.

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::error::DomainError;
use crate::events::EventKind;
use crate::ids::{EmailDeliveryId, EventId, SubscriptionId, UserId};
use crate::project::ProjectRole;
use crate::webhook::{MAX_WEBHOOK_EVENTS, NotifyScope};

/// Most recipients one subscription may name.
pub const MAX_RECIPIENTS: usize = 20;
/// Longest e-mail address, in characters.
pub const MAX_ADDRESS_CHARS: usize = 254;

/// Who receives a notification.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Recipient {
    /// Syntax checked, at most 254 characters. Not used for any access decision (a user's
    /// e-mail never is).
    Address {
        /// The address.
        address: String,
    },
    /// Members (direct rows) of the subscription's project with that role or above.
    ProjectRole {
        /// The minimum role.
        role: ProjectRole,
    },
    /// For `token.expiring`: the owner or creator of the token.
    TokenOwner,
}

impl Recipient {
    /// An explicit address, syntax checked (see [`validate_address`]).
    ///
    /// # Errors
    /// Returns [`DomainError::Invalid`] when the address is not acceptable.
    pub fn address(address: impl Into<String>) -> Result<Self, DomainError> {
        let address = address.into();
        validate_address(&address)?;
        Ok(Self::Address { address })
    }
}

/// Checks an e-mail address conservatively: 3 to 254 characters, exactly one `@` with a
/// non-empty part on each side, no whitespace and no control character (a CR, LF or NUL would
/// be header injection). Deliverability is the mail relay's business.
///
/// # Errors
/// Returns [`DomainError::Invalid`] when a rule is broken.
pub fn validate_address(address: &str) -> Result<(), DomainError> {
    let length = address.chars().count();
    let clean = address
        .chars()
        .all(|c| !c.is_whitespace() && !c.is_control());
    let mut parts = address.split('@');
    let well_formed = matches!(
        (parts.next(), parts.next(), parts.next()),
        (Some(local), Some(domain), None) if !local.is_empty() && !domain.is_empty()
    );
    if !(3..=MAX_ADDRESS_CHARS).contains(&length) || !clean || !well_formed {
        return Err(DomainError::invalid("e-mail address is not acceptable"));
    }
    Ok(())
}

/// A subscription of recipients to events of a scope.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct NotificationSubscription {
    /// Identifier.
    pub id: SubscriptionId,
    /// Whose events.
    pub scope: NotifyScope,
    /// Includes the internal kinds `task.finished` and `token.expiring`.
    pub events: Vec<EventKind>,
    /// At most 20.
    pub recipients: Vec<Recipient>,
    /// A disabled subscription sends nothing.
    pub enabled: bool,
    /// Optimistic concurrency version.
    pub version: i64,
    /// Creator.
    pub created_by: Option<UserId>,
}

impl NotificationSubscription {
    /// An enabled subscription without creator.
    #[must_use]
    pub fn new(
        id: SubscriptionId,
        scope: NotifyScope,
        events: Vec<EventKind>,
        recipients: Vec<Recipient>,
    ) -> Self {
        Self {
            id,
            scope,
            events,
            recipients,
            enabled: true,
            version: 1,
            created_by: None,
        }
    }

    /// Checks 1 to 32 distinct events, 1 to 20 valid recipients, and that a project role
    /// recipient appears only on a project scoped subscription.
    ///
    /// # Errors
    /// Returns [`DomainError::Invalid`] naming the broken rule.
    pub fn validate(&self) -> Result<(), DomainError> {
        if self.events.is_empty() || self.events.len() > MAX_WEBHOOK_EVENTS {
            return Err(DomainError::invalid("a subscription lists 1 to 32 events"));
        }
        for (index, kind) in self.events.iter().enumerate() {
            if self.events[..index].contains(kind) {
                return Err(DomainError::invalid("a subscription lists each event once"));
            }
        }
        if self.recipients.is_empty() || self.recipients.len() > MAX_RECIPIENTS {
            return Err(DomainError::invalid(
                "a subscription has 1 to 20 recipients",
            ));
        }
        for recipient in &self.recipients {
            match recipient {
                Recipient::Address { address } => validate_address(address)?,
                Recipient::ProjectRole { .. } if !matches!(self.scope, NotifyScope::Project(_)) => {
                    return Err(DomainError::invalid(
                        "project role recipients need a project scoped subscription",
                    ));
                }
                Recipient::ProjectRole { .. } | Recipient::TokenOwner => {}
            }
        }
        Ok(())
    }
}

/// State of an e-mail delivery.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EmailStatus {
    /// Waiting. With no SMTP configured a row stays here, never `Sent`.
    Pending,
    /// A worker holds the lease.
    Sending,
    /// Handed to the relay.
    Sent,
    /// The last attempt failed.
    Failed,
    /// Out of attempts.
    Dead,
    /// Not sent: an identical message went out within 10 minutes.
    Suppressed,
}

/// One e-mail to one address.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmailDelivery {
    /// Identifier.
    pub id: EmailDeliveryId,
    /// The subscription, `None` once it was deleted.
    pub subscription_id: Option<SubscriptionId>,
    /// The event.
    pub event_id: EventId,
    /// The resolved address.
    pub to: String,
    /// State.
    pub status: EmailStatus,
    /// Attempts made so far.
    pub attempts: u8,
    /// When the next attempt is due.
    #[serde(default, with = "crate::rfc3339::option")]
    pub next_attempt_at: Option<OffsetDateTime>,
    /// Short code of the last failure.
    pub last_error: Option<String>,
    /// Creation time.
    #[serde(with = "crate::rfc3339")]
    pub created_at: OffsetDateTime,
    /// When it was sent.
    #[serde(default, with = "crate::rfc3339::option")]
    pub sent_at: Option<OffsetDateTime>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::ProjectId;
    use crate::testing::{assert_wire, at, round_trip_json};

    fn subscription(scope: NotifyScope, recipients: Vec<Recipient>) -> NotificationSubscription {
        NotificationSubscription::new(
            SubscriptionId::generate(),
            scope,
            vec![EventKind::TaskFinished],
            recipients,
        )
    }

    fn address(text: &str) -> Recipient {
        Recipient::Address {
            address: text.into(),
        }
    }

    #[test]
    fn addresses_are_checked_conservatively() {
        for ok in [
            "a@b",
            "ops@example.org",
            "first.last+tag@sub.example.org",
            &format!("{}@x", "a".repeat(252)),
        ] {
            assert!(validate_address(ok).is_ok(), "{ok}");
            assert!(Recipient::address(ok).is_ok(), "{ok}");
        }
        for bad in [
            "",
            "@",
            "a@",
            "@b",
            "ab",
            "a@@b",
            "a@b@c",
            "a b@c",
            "a@b c",
            "a\nb@c",
            "a@b\r\nBcc: x@y",
            "a@b\u{0}",
            &format!("{}@x", "a".repeat(253)),
        ] {
            assert!(validate_address(bad).is_err(), "{bad:?}");
            assert!(Recipient::address(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn must_not_allow_header_injection_through_a_recipient() {
        let injected = "victim@example.org\r\nBcc: attacker@example.org";
        assert!(Recipient::address(injected).is_err());
        let sub = subscription(NotifyScope::System, vec![address(injected)]);
        assert!(sub.validate().is_err());
    }

    #[test]
    fn subscription_limits() {
        let ok = vec![address("a@b")];
        let mut sub = subscription(NotifyScope::System, ok.clone());
        assert!(sub.enabled && sub.version == 1 && sub.validate().is_ok());
        sub.events = Vec::new();
        assert!(sub.validate().is_err(), "needs an event");
        sub.events = vec![EventKind::Push, EventKind::Push];
        assert!(sub.validate().is_err(), "an event twice");
        sub.events = vec![EventKind::TokenExpiring, EventKind::TaskFinished];
        assert!(sub.validate().is_ok(), "internal kinds are allowed here");
        sub.recipients = Vec::new();
        assert!(sub.validate().is_err());
        sub.recipients = (0..20).map(|i| address(&format!("u{i}@x"))).collect();
        assert!(sub.validate().is_ok());
        sub.recipients.push(address("extra@x"));
        assert!(sub.validate().is_err(), "at most 20 recipients");
    }

    #[test]
    fn project_role_recipients_need_a_project_scope() {
        let role = vec![Recipient::ProjectRole {
            role: ProjectRole::Maintainer,
        }];
        assert!(
            subscription(NotifyScope::System, role.clone())
                .validate()
                .is_err()
        );
        assert!(
            subscription(NotifyScope::Project(ProjectId::generate()), role)
                .validate()
                .is_ok()
        );
        assert!(
            subscription(NotifyScope::System, vec![Recipient::TokenOwner])
                .validate()
                .is_ok()
        );
    }

    #[test]
    fn recipient_wire_forms() {
        assert_wire(&address("a@b"), r#"{"type":"address","address":"a@b"}"#);
        assert_wire(&Recipient::TokenOwner, r#"{"type":"token_owner"}"#);
        assert_wire(
            &Recipient::ProjectRole {
                role: ProjectRole::ProjectAdmin,
            },
            r#"{"type":"project_role","role":"project_admin"}"#,
        );
    }

    #[test]
    fn email_statuses_use_snake_case_and_deliveries_rfc3339() {
        for (status, name) in [
            (EmailStatus::Pending, "pending"),
            (EmailStatus::Sending, "sending"),
            (EmailStatus::Sent, "sent"),
            (EmailStatus::Failed, "failed"),
            (EmailStatus::Dead, "dead"),
            (EmailStatus::Suppressed, "suppressed"),
        ] {
            assert_wire(&status, &format!("\"{name}\""));
            match status {
                EmailStatus::Pending
                | EmailStatus::Sending
                | EmailStatus::Sent
                | EmailStatus::Failed
                | EmailStatus::Dead
                | EmailStatus::Suppressed => {}
            }
        }
        let delivery = EmailDelivery {
            id: EmailDeliveryId::generate(),
            subscription_id: None,
            event_id: EventId::generate(),
            to: "a@b".into(),
            status: EmailStatus::Pending,
            attempts: 0,
            next_attempt_at: Some(at()),
            last_error: None,
            created_at: at(),
            sent_at: None,
        };
        let json = round_trip_json(&delivery);
        assert_eq!(json["created_at"], "2026-10-24T10:00:00Z");
        assert_eq!(json["next_attempt_at"], "2026-10-24T10:00:00Z");
        assert!(json["sent_at"].is_null());
    }
}
