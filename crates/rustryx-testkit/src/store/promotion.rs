//! Promotion targets, requests, decisions and items.

use async_trait::async_trait;
use rustryx_core::error::StoreError;
use rustryx_core::events::EventDraft;
use rustryx_core::ids::{ComponentId, PromotionId, RepositoryId, UserId};
use rustryx_core::ports::{Page, PageRequest, PromotionRepo};
use rustryx_core::promotion::{
    MAX_ITEM_REASON_CHARS, MAX_PROMOTION_COMPONENTS, PromotionDecision, PromotionItem,
    PromotionItemStatus, PromotionRequest, PromotionStatus, PromotionTarget,
};
use time::OffsetDateTime;

use super::outbox::validate_drafts;
use super::paging::paginate;
use super::state::State;
use super::support::bump_version;
use super::{MemStore, conflict, not_found};

/// The queue the executor job of an approved promotion goes to; the dedupe key is
/// `promotion:<id>`, so approving twice cannot run a promotion twice.
const EXECUTOR_QUEUE: &str = "promotion";

impl State {
    fn promotion_mut(&mut self, id: PromotionId) -> Option<&mut PromotionRequest> {
        self.promotions
            .iter_mut()
            .find(|(_, r)| r.id == id)
            .map(|(_, r)| r)
    }

    fn promotion(&self, id: PromotionId) -> Option<&PromotionRequest> {
        self.promotions
            .iter()
            .find(|(_, r)| r.id == id)
            .map(|(_, r)| r)
    }
}

fn newest_first(seq: u64) -> String {
    format!("{:020}", u64::MAX - seq)
}

#[async_trait]
impl PromotionRepo for MemStore {
    async fn set_target(
        &self,
        target: &PromotionTarget,
        expected_version: Option<i64>,
    ) -> Result<(), StoreError> {
        let mut state = self.tables()?;
        target
            .validate()
            .map_err(|error| conflict(error.to_string()))?;
        if !state.repositories.contains_key(&target.repository_id) {
            return Err(conflict(format!(
                "repository {} does not exist",
                target.repository_id
            )));
        }
        let existing = state
            .promotion_targets
            .get(&target.repository_id)
            .map(|t| t.version);
        let mut next = target.clone();
        match (existing, expected_version) {
            (Some(current), Some(expected)) => next.version = bump_version(current, expected)?,
            (Some(_), None) => return Err(conflict("the repository already has a promotion rule")),
            (None, Some(_)) => {
                return Err(conflict(
                    "stale version: the repository has no promotion rule",
                ));
            }
            (None, None) => {}
        }
        state.promotion_targets.insert(next.repository_id, next);
        Ok(())
    }

    async fn get_target(
        &self,
        repository: RepositoryId,
    ) -> Result<Option<PromotionTarget>, StoreError> {
        Ok(self.tables()?.promotion_targets.get(&repository).cloned())
    }

    async fn create(
        &self,
        request: &PromotionRequest,
        items: &[ComponentId],
        events: &[EventDraft],
    ) -> Result<(), StoreError> {
        validate_drafts(events)?;
        let now = self.now();
        let mut state = self.tables()?;
        request
            .validate()
            .map_err(|error| conflict(error.to_string()))?;
        if items.len() > MAX_PROMOTION_COMPONENTS {
            return Err(StoreError::Invalid(
                "a promotion has at most 10000 items".to_owned(),
            ));
        }
        for repository in [request.source_repository_id, request.target_repository_id] {
            if !state.repositories.contains_key(&repository) {
                return Err(conflict(format!("repository {repository} does not exist")));
            }
        }
        if state.promotion(request.id).is_some() {
            return Err(conflict(format!("promotion {} already exists", request.id)));
        }
        let mut seen = std::collections::HashSet::new();
        let rows: Vec<PromotionItem> = items
            .iter()
            .filter(|component| seen.insert(**component))
            .map(|component| PromotionItem {
                promotion_id: request.id,
                component_id: *component,
                status: PromotionItemStatus::Pending,
                reason: None,
            })
            .collect();
        state.run_seq += 1;
        let seq = state.run_seq;
        state.promotions.push((seq, request.clone()));
        state.promotion_items.insert(request.id, rows);
        state.push_events(events, now);
        Ok(())
    }

    async fn get(&self, id: PromotionId) -> Result<Option<PromotionRequest>, StoreError> {
        Ok(self.tables()?.promotion(id).cloned())
    }

    async fn list(
        &self,
        repository: Option<RepositoryId>,
        status: Option<PromotionStatus>,
        page: &PageRequest,
    ) -> Result<Page<PromotionRequest>, StoreError> {
        let rows: Vec<(String, PromotionRequest)> = self
            .tables()?
            .promotions
            .iter()
            .filter(|(_, r)| {
                repository
                    .is_none_or(|id| r.source_repository_id == id || r.target_repository_id == id)
            })
            .filter(|(_, r)| status.is_none_or(|s| r.status == s))
            .map(|(seq, r)| (newest_first(*seq), r.clone()))
            .collect();
        paginate(rows, page)
    }

    async fn decide(
        &self,
        decision: &PromotionDecision,
        events: &[EventDraft],
    ) -> Result<PromotionStatus, StoreError> {
        validate_drafts(events)?;
        let now = self.now();
        let mut state = self.tables()?;
        decision
            .validate()
            .map_err(|error| conflict(error.to_string()))?;
        let request = state
            .promotion(decision.promotion_id)
            .cloned()
            .ok_or_else(|| not_found("promotion", decision.promotion_id))?;
        if request.status != PromotionStatus::PendingApproval || request.is_expired_at(now) {
            return Err(conflict("the promotion is not waiting for a decision"));
        }
        let decisions = state.promotion_decisions.entry(request.id).or_default();
        if decisions.iter().any(|d| d.approver == decision.approver) {
            return Err(conflict("the approver already decided"));
        }
        decisions.push(decision.clone());
        let counted = decisions.iter().filter(|d| d.approve && d.counted).count();
        let outcome = if decision.approve {
            if counted >= usize::from(request.required_approvals) {
                PromotionStatus::Approved
            } else {
                PromotionStatus::PendingApproval
            }
        } else {
            PromotionStatus::Rejected
        };
        if let Some(stored) = state.promotion_mut(request.id) {
            stored.status = outcome;
            if outcome == PromotionStatus::Rejected {
                stored.finished_at = Some(decision.at);
            }
        }
        if outcome == PromotionStatus::Approved {
            // The executor job is queued in the same step, deduplicated by its key.
            let key = format!("promotion:{}", request.id);
            let payload = serde_json::json!({ "promotion_id": request.id });
            state.insert_job(EXECUTOR_QUEUE, Some(&key), &payload, now);
        }
        state.push_events(events, now);
        Ok(outcome)
    }

    async fn decisions(&self, id: PromotionId) -> Result<Vec<PromotionDecision>, StoreError> {
        let state = self.tables()?;
        if state.promotion(id).is_none() {
            return Err(not_found("promotion", id));
        }
        Ok(state
            .promotion_decisions
            .get(&id)
            .cloned()
            .unwrap_or_default())
    }

    async fn cancel(&self, id: PromotionId, by: UserId) -> Result<(), StoreError> {
        let now = self.now();
        let mut state = self.tables()?;
        let request = state
            .promotion_mut(id)
            .ok_or_else(|| not_found("promotion", id))?;
        if request.requested_by != by {
            return Err(conflict("only the requester may cancel a promotion"));
        }
        if request.status != PromotionStatus::PendingApproval {
            return Err(conflict("only a pending promotion can be cancelled"));
        }
        request.status = PromotionStatus::Cancelled;
        request.finished_at = Some(now);
        Ok(())
    }

    async fn items(
        &self,
        id: PromotionId,
        page: &PageRequest,
    ) -> Result<Page<PromotionItem>, StoreError> {
        let state = self.tables()?;
        if state.promotion(id).is_none() {
            return Err(not_found("promotion", id));
        }
        let rows: Vec<(String, PromotionItem)> = state
            .promotion_items
            .get(&id)
            .into_iter()
            .flatten()
            .map(|item| (item.component_id.to_string(), item.clone()))
            .collect();
        drop(state);
        paginate(rows, page)
    }

    async fn mark_running(&self, id: PromotionId, at: OffsetDateTime) -> Result<bool, StoreError> {
        let mut state = self.tables()?;
        let request = state
            .promotion_mut(id)
            .ok_or_else(|| not_found("promotion", id))?;
        // Compare and set on `approved`: two executors cannot both start the same promotion.
        if request.status != PromotionStatus::Approved {
            return Ok(false);
        }
        request.status = PromotionStatus::Running;
        request.started_at = Some(at);
        Ok(true)
    }

    async fn set_item(
        &self,
        id: PromotionId,
        component: ComponentId,
        status: PromotionItemStatus,
        reason: Option<&str>,
    ) -> Result<(), StoreError> {
        let mut state = self.tables()?;
        let item = state
            .promotion_items
            .get_mut(&id)
            .and_then(|items| items.iter_mut().find(|i| i.component_id == component))
            .ok_or_else(|| not_found("promotion item", component))?;
        item.status = status;
        // A reason is a stable code of at most 64 characters, cut like the column would refuse.
        item.reason = reason.map(|r| r.chars().take(MAX_ITEM_REASON_CHARS).collect());
        Ok(())
    }

    async fn finish(
        &self,
        id: PromotionId,
        status: PromotionStatus,
        at: OffsetDateTime,
        events: &[EventDraft],
    ) -> Result<(), StoreError> {
        validate_drafts(events)?;
        let now = self.now();
        let mut state = self.tables()?;
        if !status.is_terminal() {
            return Err(StoreError::Invalid(
                "a promotion ends with a terminal status".to_owned(),
            ));
        }
        let request = state
            .promotion_mut(id)
            .ok_or_else(|| not_found("promotion", id))?;
        // Only an approved or running request can end here; pending ones end through
        // `decide`, `cancel` and `expire`, and a finished one stays as it ended.
        if !matches!(
            request.status,
            PromotionStatus::Approved | PromotionStatus::Running
        ) {
            return Err(conflict("the promotion is not executing"));
        }
        request.status = status;
        request.finished_at = Some(at);
        state.push_events(events, now);
        Ok(())
    }

    async fn expire(&self, now: OffsetDateTime) -> Result<u64, StoreError> {
        let mut state = self.tables()?;
        let mut expired = 0_u64;
        for (_, request) in &mut state.promotions {
            if request.is_expired_at(now) {
                request.status = PromotionStatus::Expired;
                request.finished_at = Some(now);
                expired += 1;
            }
        }
        Ok(expired)
    }
}
