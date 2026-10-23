//! Promotion of components between repositories, with counted approvals.
//!
//! A promotion is a request, approvals that count only from people who are neither the
//! requester nor an author of a selected component, checks that are evaluated again at
//! execution time, and a hold on the source while it runs.

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::error::DomainError;
use crate::ids::{ComponentId, LabelId, PromotionId, RepositoryId, UserId};
use crate::scan::Severity;

/// Most components one request may select.
pub const MAX_PROMOTION_COMPONENTS: usize = 10_000;
/// Days a pending request stays open.
pub const PROMOTION_REQUEST_TTL_DAYS: i64 = 14;
/// Longest approval comment, in characters.
pub const MAX_DECISION_COMMENT_CHARS: usize = 1000;
/// Longest stored item reason code, in characters.
pub const MAX_ITEM_REASON_CHARS: usize = 64;

/// Copy keeps the source, move deletes it after the target committed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PromotionMode {
    /// Copy.
    Copy,
    /// Move.
    Move,
}

/// What a request promotes.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "by", rename_all = "snake_case")]
pub enum PromotionSelector {
    /// An explicit list, at most 10 000 per request.
    Components {
        /// The components.
        ids: Vec<ComponentId>,
    },
    /// Every component of the source that carries the label.
    Label {
        /// The label.
        label_id: LabelId,
    },
}

impl PromotionSelector {
    /// Checks the component list: 1 to 10 000 entries.
    ///
    /// # Errors
    /// Returns [`DomainError::Invalid`] for an empty or oversized list.
    pub fn validate(&self) -> Result<(), DomainError> {
        match self {
            Self::Components { ids } if ids.is_empty() || ids.len() > MAX_PROMOTION_COMPONENTS => {
                Err(DomainError::invalid(
                    "a promotion selects 1 to 10000 components",
                ))
            }
            Self::Components { .. } | Self::Label { .. } => Ok(()),
        }
    }
}

/// A check that must hold for a component to be promoted. Evaluated again at execution.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "check", rename_all = "snake_case")]
pub enum PromotionCheck {
    /// The scan has no finding at or above the threshold.
    ScanBelowThreshold {
        /// The threshold.
        threshold: Severity,
    },
    /// A signature verifies against the target's trust policy.
    SignatureValid,
    /// An SBOM exists for the component.
    SbomPresent,
}

/// State of a promotion request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum PromotionStatus {
    /// Waiting for counted approvals.
    PendingApproval,
    /// Approved, waiting for the executor.
    Approved,
    /// Rejected by an approver.
    Rejected,
    /// The executor runs it.
    Running,
    /// Every item is done or deliberately skipped.
    Succeeded,
    /// Some items failed.
    PartiallyFailed,
    /// Every item failed.
    Failed,
    /// Cancelled by the requester while pending.
    Cancelled,
    /// Not decided within 14 days.
    Expired,
}

impl PromotionStatus {
    /// True when the request will not change again.
    #[must_use]
    pub const fn is_terminal(self) -> bool {
        !matches!(self, Self::PendingApproval | Self::Approved | Self::Running)
    }
}

/// A release repository's promotion rule. A target without a row accepts direct copies from
/// users with read on the source and write on the target; a row with `require_approval`
/// refuses them (`PolicyViolation::ApprovalRequired`).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct PromotionTarget {
    /// The target repository.
    pub repository_id: RepositoryId,
    /// Direct copies are refused when true.
    pub require_approval: bool,
    /// At least 1 when `require_approval`.
    pub required_approvals: u8,
    /// Checks every promoted component must pass.
    pub checks: Vec<PromotionCheck>,
    /// Optimistic concurrency version.
    pub version: i64,
}

impl PromotionTarget {
    /// A target that requires no approval and no check (one approval would be needed once
    /// `require_approval` is switched on).
    #[must_use]
    pub fn new(repository_id: RepositoryId) -> Self {
        Self {
            repository_id,
            require_approval: false,
            required_approvals: 1,
            checks: Vec::new(),
            version: 1,
        }
    }

    /// Checks that an approval requirement needs at least one approval.
    ///
    /// # Errors
    /// Returns [`DomainError::Invalid`] when `require_approval` is set and
    /// `required_approvals` is 0.
    pub fn validate(&self) -> Result<(), DomainError> {
        if self.require_approval && self.required_approvals == 0 {
            return Err(DomainError::invalid(
                "a target that requires approval needs at least one approval",
            ));
        }
        Ok(())
    }
}

/// A promotion request.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct PromotionRequest {
    /// Identifier.
    pub id: PromotionId,
    /// Copy or move.
    pub mode: PromotionMode,
    /// Where the components come from.
    pub source_repository_id: RepositoryId,
    /// Where they go. Different from the source.
    pub target_repository_id: RepositoryId,
    /// What is promoted.
    pub selector: PromotionSelector,
    /// Who asked.
    pub requested_by: UserId,
    /// When.
    #[serde(with = "crate::rfc3339")]
    pub requested_at: OffsetDateTime,
    /// State.
    pub status: PromotionStatus,
    /// Counted approvals needed, snapshotted from the target at request time.
    pub required_approvals: u8,
    /// Checks snapshotted from the target at request time.
    pub checks: Vec<PromotionCheck>,
    /// A pending request expires after 14 days.
    #[serde(with = "crate::rfc3339")]
    pub expires_at: OffsetDateTime,
    /// When the executor started.
    #[serde(default, with = "crate::rfc3339::option")]
    pub started_at: Option<OffsetDateTime>,
    /// When it ended.
    #[serde(default, with = "crate::rfc3339::option")]
    pub finished_at: Option<OffsetDateTime>,
}

impl PromotionRequest {
    /// A pending request that expires 14 days after `now`, with no required approval and no
    /// check (assign what the target's `PromotionTarget` says).
    #[must_use]
    pub fn new(
        id: PromotionId,
        mode: PromotionMode,
        source_repository_id: RepositoryId,
        target_repository_id: RepositoryId,
        selector: PromotionSelector,
        requested_by: UserId,
        now: OffsetDateTime,
    ) -> Self {
        Self {
            id,
            mode,
            source_repository_id,
            target_repository_id,
            selector,
            requested_by,
            requested_at: now,
            status: PromotionStatus::PendingApproval,
            required_approvals: 0,
            checks: Vec::new(),
            expires_at: now + time::Duration::days(PROMOTION_REQUEST_TTL_DAYS),
            started_at: None,
            finished_at: None,
        }
    }

    /// Checks that source and target differ and that the selector is acceptable.
    ///
    /// # Errors
    /// Returns [`DomainError::Invalid`] naming the broken rule.
    pub fn validate(&self) -> Result<(), DomainError> {
        if self.source_repository_id == self.target_repository_id {
            return Err(DomainError::invalid(
                "source and target repository must differ",
            ));
        }
        self.selector.validate()
    }

    /// True when a pending request is past its expiry. A request that is not pending never
    /// expires.
    #[must_use]
    pub fn is_expired_at(&self, now: OffsetDateTime) -> bool {
        self.status == PromotionStatus::PendingApproval && self.expires_at <= now
    }
}

/// One approver's decision.
///
/// A decision counts only if the approver is not the requester and not the author of any
/// selected component (the author is `components.created_by`). A decision that does not count
/// is stored with `counted = false` and shown, so nothing is hidden.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PromotionDecision {
    /// The request.
    pub promotion_id: PromotionId,
    /// The approver.
    pub approver: UserId,
    /// Approve or reject.
    pub approve: bool,
    /// At most 1000 characters.
    pub comment: String,
    /// Whether it counts toward `required_approvals`.
    pub counted: bool,
    /// When it was made.
    #[serde(with = "crate::rfc3339")]
    pub at: OffsetDateTime,
}

impl PromotionDecision {
    /// Whether a decision by `approver` counts: the approver is neither the requester nor an
    /// author of one of the selected components.
    #[must_use]
    pub fn counts(approver: UserId, requester: UserId, component_authors: &[UserId]) -> bool {
        approver != requester && !component_authors.contains(&approver)
    }

    /// Checks the comment limit.
    ///
    /// # Errors
    /// Returns [`DomainError::Invalid`] when the comment exceeds 1000 characters.
    pub fn validate(&self) -> Result<(), DomainError> {
        if self.comment.chars().count() > MAX_DECISION_COMMENT_CHARS {
            return Err(DomainError::invalid(
                "a comment has at most 1000 characters",
            ));
        }
        Ok(())
    }
}

/// State of one component of a promotion.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PromotionItemStatus {
    /// Not handled yet.
    Pending,
    /// Promoted (or already present with the same digest).
    Done,
    /// Not promoted on purpose.
    Skipped,
    /// The attempt failed.
    Failed,
}

/// One component of a promotion.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PromotionItem {
    /// The request.
    pub promotion_id: PromotionId,
    /// The component.
    pub component_id: ComponentId,
    /// State.
    pub status: PromotionItemStatus,
    /// `immutable`, `held`, `quota`, `check_failed`, `target_exists`, ... never free text, at
    /// most 64 characters.
    pub reason: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{assert_wire, at, round_trip_json};

    fn request() -> PromotionRequest {
        PromotionRequest::new(
            PromotionId::generate(),
            PromotionMode::Copy,
            RepositoryId::generate(),
            RepositoryId::generate(),
            PromotionSelector::Components {
                ids: vec![ComponentId::generate()],
            },
            UserId::generate(),
            at(),
        )
    }

    #[test]
    fn request_constructor_defaults() {
        let request = request();
        assert_eq!(request.status, PromotionStatus::PendingApproval);
        assert_eq!(request.expires_at, at() + time::Duration::days(14));
        assert_eq!((request.required_approvals, request.checks.len()), (0, 0));
        assert!(request.started_at.is_none() && request.finished_at.is_none());
        assert!(request.validate().is_ok());
    }

    #[test]
    fn source_and_target_must_differ() {
        let mut request = request();
        request.target_repository_id = request.source_repository_id;
        assert!(request.validate().is_err());
    }

    #[test]
    fn selector_limits() {
        let ids = |n: usize| PromotionSelector::Components {
            ids: (0..n).map(|_| ComponentId::generate()).collect(),
        };
        assert!(ids(0).validate().is_err());
        assert!(ids(1).validate().is_ok());
        assert!(ids(MAX_PROMOTION_COMPONENTS).validate().is_ok());
        assert!(ids(MAX_PROMOTION_COMPONENTS + 1).validate().is_err());
        assert!(
            PromotionSelector::Label {
                label_id: LabelId::generate()
            }
            .validate()
            .is_ok()
        );
    }

    #[test]
    fn only_a_pending_request_expires_and_expiry_is_inclusive() {
        let mut request = request();
        assert!(!request.is_expired_at(at()));
        assert!(
            !request.is_expired_at(at() + time::Duration::days(14) - time::Duration::seconds(1))
        );
        assert!(request.is_expired_at(at() + time::Duration::days(14)));
        request.status = PromotionStatus::Running;
        assert!(
            !request.is_expired_at(at() + time::Duration::days(30)),
            "a running promotion is not cut short"
        );
    }

    #[test]
    fn must_not_count_the_requesters_approval_nor_an_authors() {
        let requester = UserId::generate();
        let author = UserId::generate();
        let other = UserId::generate();
        assert!(!PromotionDecision::counts(requester, requester, &[]));
        assert!(!PromotionDecision::counts(
            author,
            requester,
            &[other, author]
        ));
        assert!(PromotionDecision::counts(other, requester, &[author]));
        assert!(PromotionDecision::counts(other, requester, &[]));
    }

    /// No wildcard arm: a new status has to decide whether it ends a request.
    fn expected(status: PromotionStatus) -> (&'static str, bool) {
        match status {
            PromotionStatus::PendingApproval => ("pending_approval", false),
            PromotionStatus::Approved => ("approved", false),
            PromotionStatus::Rejected => ("rejected", true),
            PromotionStatus::Running => ("running", false),
            PromotionStatus::Succeeded => ("succeeded", true),
            PromotionStatus::PartiallyFailed => ("partially_failed", true),
            PromotionStatus::Failed => ("failed", true),
            PromotionStatus::Cancelled => ("cancelled", true),
            PromotionStatus::Expired => ("expired", true),
        }
    }

    #[test]
    fn promotion_statuses_are_pinned() {
        for status in [
            PromotionStatus::PendingApproval,
            PromotionStatus::Approved,
            PromotionStatus::Rejected,
            PromotionStatus::Running,
            PromotionStatus::Succeeded,
            PromotionStatus::PartiallyFailed,
            PromotionStatus::Failed,
            PromotionStatus::Cancelled,
            PromotionStatus::Expired,
        ] {
            let (name, terminal) = expected(status);
            assert_wire(&status, &format!("\"{name}\""));
            assert_eq!(status.is_terminal(), terminal, "{status:?}");
        }
    }

    #[test]
    fn target_constructor_and_approval_rule() {
        let mut target = PromotionTarget::new(RepositoryId::generate());
        assert!(
            !target.require_approval && target.required_approvals == 1 && target.checks.is_empty()
        );
        assert!(target.validate().is_ok());
        target.require_approval = true;
        assert!(target.validate().is_ok());
        target.required_approvals = 0;
        assert!(target.validate().is_err());
        target.require_approval = false;
        assert!(
            target.validate().is_ok(),
            "zero is harmless when nothing is required"
        );
    }

    #[test]
    fn checks_selectors_and_modes_use_tagged_snake_case() {
        assert_wire(&PromotionMode::Move, "\"move\"");
        assert_wire(
            &PromotionCheck::SignatureValid,
            r#"{"check":"signature_valid"}"#,
        );
        assert_wire(
            &PromotionCheck::ScanBelowThreshold {
                threshold: Severity::High,
            },
            r#"{"check":"scan_below_threshold","threshold":"high"}"#,
        );
        let label = LabelId::generate();
        let json =
            serde_json::to_value(PromotionSelector::Label { label_id: label }).expect("json");
        assert_eq!(json["by"], "label");
        assert_eq!(json["label_id"], label.to_string());
    }

    #[test]
    fn request_and_decision_serialize_rfc3339() {
        let json = round_trip_json(&request());
        assert_eq!(json["requested_at"], "2026-10-24T10:00:00Z");
        assert_eq!(json["expires_at"], "2026-11-07T10:00:00Z");
        assert_eq!(json["status"], "pending_approval");
        let decision = PromotionDecision {
            promotion_id: PromotionId::generate(),
            approver: UserId::generate(),
            approve: true,
            comment: "c".repeat(1000),
            counted: true,
            at: at(),
        };
        assert!(decision.validate().is_ok());
        assert_eq!(round_trip_json(&decision)["at"], "2026-10-24T10:00:00Z");
        let mut long = decision;
        long.comment.push('c');
        assert!(long.validate().is_err());
    }

    #[test]
    fn item_statuses_use_snake_case() {
        for (status, name) in [
            (PromotionItemStatus::Pending, "pending"),
            (PromotionItemStatus::Done, "done"),
            (PromotionItemStatus::Skipped, "skipped"),
            (PromotionItemStatus::Failed, "failed"),
        ] {
            assert_wire(&status, &format!("\"{name}\""));
        }
        let item = PromotionItem {
            promotion_id: PromotionId::generate(),
            component_id: ComponentId::generate(),
            status: PromotionItemStatus::Skipped,
            reason: Some("immutable".into()),
        };
        assert_eq!(round_trip_json(&item)["reason"], "immutable");
    }
}
