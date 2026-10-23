//! Status checks: what the system status page and the status API report.
//!
//! Each stream registers [`StatusCheckProvider`]s and the API runs them all. A check that
//! cannot be evaluated is `Unknown`, never `Ok`.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::error::DomainError;

/// Longest check message, in characters.
pub const MAX_CHECK_MESSAGE_CHARS: usize = 200;

/// How serious a failing check is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckSeverity {
    /// For information.
    Info,
    /// Needs attention.
    Warning,
    /// An operator is paged on `Critical` and `Failing`.
    Critical,
}

/// The result of one check.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckState {
    /// Checked and fine.
    Ok,
    /// Checked and not fine.
    Failing,
    /// Could not be checked (timeout, error, missing input).
    Unknown,
}

/// One line of the status page.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StatusCheck {
    /// Stable id from the check catalog (`blob_store_unhealthy`, ...).
    pub id: String,
    /// How serious it is when failing.
    pub severity: CheckSeverity,
    /// Ok, failing or unknown.
    pub state: CheckState,
    /// One line, no secrets, no addresses of internal systems, at most 200 characters.
    pub message: String,
    /// Since when the state holds.
    #[serde(default, with = "crate::rfc3339::option")]
    pub since: Option<OffsetDateTime>,
    /// A blob store, scanner or registry name when the check is about one.
    pub subject: Option<String>,
}

impl StatusCheck {
    /// A check without `since` and `subject`.
    #[must_use]
    pub fn new(
        id: impl Into<String>,
        severity: CheckSeverity,
        state: CheckState,
        message: impl Into<String>,
    ) -> Self {
        Self {
            id: id.into(),
            severity,
            state,
            message: message.into(),
            since: None,
            subject: None,
        }
    }

    /// The result of a check that could not be evaluated: `Unknown`, never `Ok`.
    #[must_use]
    pub fn unknown(
        id: impl Into<String>,
        severity: CheckSeverity,
        message: impl Into<String>,
    ) -> Self {
        Self::new(id, severity, CheckState::Unknown, message)
    }

    /// True when an operator should be paged: failing with `Critical` severity.
    #[must_use]
    pub fn pages_an_operator(&self) -> bool {
        self.state == CheckState::Failing && self.severity == CheckSeverity::Critical
    }

    /// Checks the id and the message: a non empty id, a message of at most 200 characters on
    /// one line.
    ///
    /// # Errors
    /// Returns [`DomainError::Invalid`] naming the broken rule.
    pub fn validate(&self) -> Result<(), DomainError> {
        if self.id.is_empty() {
            return Err(DomainError::invalid("a status check needs an id"));
        }
        if self.message.chars().count() > MAX_CHECK_MESSAGE_CHARS
            || self.message.chars().any(char::is_control)
        {
            return Err(DomainError::invalid(
                "a check message is one line of at most 200 characters",
            ));
        }
        Ok(())
    }
}

/// A source of status checks. Each stream registers providers; the API runs them all.
#[async_trait]
pub trait StatusCheckProvider: Send + Sync {
    /// Name of the provider, for logs and metrics.
    fn name(&self) -> &'static str;

    /// Cancel safe, read only, bounded: the runner gives it 5 seconds and reports `Unknown`
    /// on a timeout. A provider never reports `Ok` for something it could not check.
    async fn run(&self, now: OffsetDateTime) -> Vec<StatusCheck>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{assert_wire, at, round_trip_json};

    #[test]
    fn severities_are_ordered_and_snake_case() {
        assert!(
            CheckSeverity::Info < CheckSeverity::Warning
                && CheckSeverity::Warning < CheckSeverity::Critical
        );
        assert_wire(&CheckSeverity::Critical, "\"critical\"");
        assert_wire(&CheckState::Unknown, "\"unknown\"");
        assert_wire(&CheckState::Failing, "\"failing\"");
        match CheckState::Ok {
            CheckState::Ok | CheckState::Failing | CheckState::Unknown => {}
        }
    }

    #[test]
    fn must_not_report_ok_for_an_unchecked_item() {
        let check = StatusCheck::unknown("scanner_down", CheckSeverity::Warning, "probe timed out");
        assert_eq!(check.state, CheckState::Unknown);
        assert_ne!(check.state, CheckState::Ok);
        assert!(!check.pages_an_operator(), "unknown does not page");
    }

    #[test]
    fn only_failing_critical_checks_page_an_operator() {
        for severity in [
            CheckSeverity::Info,
            CheckSeverity::Warning,
            CheckSeverity::Critical,
        ] {
            for state in [CheckState::Ok, CheckState::Failing, CheckState::Unknown] {
                let check = StatusCheck::new("x", severity, state, "m");
                let pages = severity == CheckSeverity::Critical && state == CheckState::Failing;
                assert_eq!(check.pages_an_operator(), pages, "{severity:?} {state:?}");
            }
        }
    }

    #[test]
    fn message_and_id_rules() {
        let mut check = StatusCheck::new(
            "outbox_lag",
            CheckSeverity::Critical,
            CheckState::Failing,
            "m".repeat(200),
        );
        assert!(check.validate().is_ok());
        check.message = "m".repeat(201);
        assert!(check.validate().is_err());
        check.message = "two\nlines".into();
        assert!(check.validate().is_err());
        check.message = "fine".into();
        check.id.clear();
        assert!(check.validate().is_err());
    }

    #[test]
    fn wire_form_carries_rfc3339_since() {
        let mut check = StatusCheck::new(
            "read_only_mode",
            CheckSeverity::Warning,
            CheckState::Failing,
            "enabled",
        );
        check.since = Some(at());
        check.subject = Some("default".into());
        let json = round_trip_json(&check);
        assert_eq!(json["since"], "2026-10-24T10:00:00Z");
        assert_eq!(json["subject"], "default");
        assert_eq!(json["state"], "failing");
    }

    struct Fixed;

    #[async_trait]
    impl StatusCheckProvider for Fixed {
        fn name(&self) -> &'static str {
            "fixed"
        }
        async fn run(&self, now: OffsetDateTime) -> Vec<StatusCheck> {
            let mut check =
                StatusCheck::new("fixed_check", CheckSeverity::Info, CheckState::Ok, "fine");
            check.since = Some(now);
            vec![check]
        }
    }

    #[test]
    fn the_provider_trait_is_object_safe() {
        let provider: Box<dyn StatusCheckProvider> = Box::new(Fixed);
        assert_eq!(provider.name(), "fixed");
    }
}
