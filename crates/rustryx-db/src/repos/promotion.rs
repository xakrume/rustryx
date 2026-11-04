//! Promotion targets, requests and the approvals that release them.

use rustryx_core::error::StoreError;
use rustryx_core::events::EventDraft;
use rustryx_core::ids::{ComponentId, PromotionId, RepositoryId, UserId};
use rustryx_core::ports::{Page, PageRequest, PromotionRepo};
use rustryx_core::promotion::{
    MAX_PROMOTION_COMPONENTS, PromotionCheck, PromotionDecision, PromotionItem, PromotionItemStatus,
    PromotionMode, PromotionRequest, PromotionSelector, PromotionStatus, PromotionTarget,
};
use sqlx::PgPool;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::error::{backend, map_err};
use crate::outbox;

/// Promotion.
#[derive(Clone)]
pub struct PgPromotionRepo {
    pool: PgPool,
}

impl PgPromotionRepo {
    /// A repo on `pool`.
    #[must_use]
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[derive(sqlx::FromRow)]
struct TargetRow {
    repository_id: Uuid,
    require_approval: bool,
    required_approvals: i16,
    checks: serde_json::Value,
    version: i64,
}

#[derive(sqlx::FromRow)]
struct RequestRow {
    id: Uuid,
    mode: String,
    source_repository_id: Uuid,
    target_repository_id: Uuid,
    selector: serde_json::Value,
    requested_by: Uuid,
    requested_at: OffsetDateTime,
    status: String,
    required_approvals: i16,
    checks: serde_json::Value,
    expires_at: OffsetDateTime,
    started_at: Option<OffsetDateTime>,
    finished_at: Option<OffsetDateTime>,
}

#[async_trait::async_trait]
impl PromotionRepo for PgPromotionRepo {
    async fn set_target(
        &self,
        target: &PromotionTarget,
        expected_version: Option<i64>,
    ) -> Result<(), StoreError> {
        target
            .validate()
            .map_err(|error| StoreError::Conflict(error.to_string()))?;
        let current: Option<i64> =
            sqlx::query_scalar("SELECT version FROM promotion_targets WHERE repository_id = $1")
                .bind(target.repository_id.as_uuid())
                .fetch_optional(&self.pool)
                .await
                .map_err(|e| map_err("read promotion target", e))?;
        let version = match (current, expected_version) {
            (Some(current), Some(expected)) if current == expected => current.saturating_add(1),
            (Some(current), Some(expected)) => {
                return Err(StoreError::Conflict(format!(
                    "stale version: expected {expected}, found {current}"
                )));
            }
            (Some(_), None) => {
                return Err(StoreError::Conflict(
                    "the repository already has a promotion rule".to_owned(),
                ));
            }
            (None, Some(_)) => {
                return Err(StoreError::Conflict(
                    "stale version: the repository has no promotion rule".to_owned(),
                ));
            }
            (None, None) => target.version,
        };
        let checks = serde_json::to_value(&target.checks)
            .map_err(|_| backend("write promotion target", "checks"))?;
        if current.is_some() {
            sqlx::query(
                "UPDATE promotion_targets SET require_approval = $2, required_approvals = $3, \
                 checks = $4, version = $5 WHERE repository_id = $1",
            )
            .bind(target.repository_id.as_uuid())
            .bind(target.require_approval)
            .bind(i16::from(target.required_approvals))
            .bind(checks)
            .bind(version)
            .execute(&self.pool)
            .await
            .map_err(|e| map_err("update promotion target", e))?;
        } else {
            sqlx::query(
                "INSERT INTO promotion_targets (repository_id, require_approval, \
                 required_approvals, checks, version) VALUES ($1,$2,$3,$4,$5)",
            )
            .bind(target.repository_id.as_uuid())
            .bind(target.require_approval)
            .bind(i16::from(target.required_approvals))
            .bind(checks)
            .bind(version)
            .execute(&self.pool)
            .await
            .map_err(|e| map_err("create promotion target", e))?;
        }
        Ok(())
    }

    async fn get_target(
        &self,
        repository: RepositoryId,
    ) -> Result<Option<PromotionTarget>, StoreError> {
        let row: Option<TargetRow> =
            sqlx::query_as("SELECT * FROM promotion_targets WHERE repository_id = $1")
                .bind(repository.as_uuid())
                .fetch_optional(&self.pool)
                .await
                .map_err(|e| map_err("read promotion target", e))?;
        row.map(decode_target).transpose()
    }

    async fn create(
        &self,
        request: &PromotionRequest,
        items: &[ComponentId],
        events: &[EventDraft],
    ) -> Result<(), StoreError> {
        request
            .validate()
            .map_err(|error| StoreError::Conflict(error.to_string()))?;
        if items.len() > MAX_PROMOTION_COMPONENTS {
            return Err(StoreError::Invalid(
                "a promotion has at most 10000 items".to_owned(),
            ));
        }
        let mut tx = self.pool.begin().await.map_err(|e| map_err("create promotion", e))?;
        sqlx::query(
            "INSERT INTO promotion_requests (id, mode, source_repository_id, target_repository_id, \
             selector, requested_by, requested_at, status, required_approvals, checks, expires_at, \
             started_at, finished_at) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13)",
        )
        .bind(request.id.as_uuid())
        .bind(mode_label(request.mode))
        .bind(request.source_repository_id.as_uuid())
        .bind(request.target_repository_id.as_uuid())
        .bind(serde_json::to_value(&request.selector).map_err(|_| backend("create promotion", "selector"))?)
        .bind(request.requested_by.as_uuid())
        .bind(request.requested_at)
        .bind(status_label(request.status)?)
        .bind(i16::from(request.required_approvals))
        .bind(serde_json::to_value(&request.checks).map_err(|_| backend("create promotion", "checks"))?)
        .bind(request.expires_at)
        .bind(request.started_at)
        .bind(request.finished_at)
        .execute(&mut *tx)
        .await
        .map_err(|e| map_err("create promotion", e))?;
        let mut seen = std::collections::HashSet::new();
        for component in items {
            if !seen.insert(*component) {
                continue;
            }
            sqlx::query(
                "INSERT INTO promotion_items (promotion_id, component_id, status) VALUES ($1,$2,'pending')",
            )
            .bind(request.id.as_uuid())
            .bind(component.as_uuid())
            .execute(&mut *tx)
            .await
            .map_err(|e| map_err("create promotion item", e))?;
        }
        outbox::insert_events(&mut tx, events).await?;
        tx.commit().await.map_err(|e| map_err("create promotion", e))
    }

    async fn get(&self, id: PromotionId) -> Result<Option<PromotionRequest>, StoreError> {
        let row: Option<RequestRow> = sqlx::query_as("SELECT * FROM promotion_requests WHERE id = $1")
            .bind(id.as_uuid())
            .fetch_optional(&self.pool)
            .await
            .map_err(|e| map_err("read promotion", e))?;
        row.map(decode_request).transpose()
    }

    async fn list(
        &self,
        repository: Option<RepositoryId>,
        status: Option<PromotionStatus>,
        page: &PageRequest,
    ) -> Result<Page<PromotionRequest>, StoreError> {
        let limit = page.limit.clamp(1, 100);
        let rows: Vec<RequestRow> = sqlx::query_as(
            "SELECT * FROM promotion_requests \
             WHERE ($1::uuid IS NULL OR source_repository_id = $1 OR target_repository_id = $1) \
               AND ($2::text IS NULL OR status = $2) \
             ORDER BY requested_at DESC, id DESC LIMIT $3",
        )
        .bind(repository.map(|id| id.as_uuid()))
        .bind(status.map(status_label).transpose()?)
        .bind(i64::from(limit))
        .fetch_all(&self.pool)
        .await
        .map_err(|e| map_err("list promotions", e))?;
        let items = rows.into_iter().map(decode_request).collect::<Result<Vec<_>, _>>()?;
        Ok(Page { items, next: None })
    }

    async fn decide(
        &self,
        decision: &PromotionDecision,
        events: &[EventDraft],
    ) -> Result<PromotionStatus, StoreError> {
        decision
            .validate()
            .map_err(|error| StoreError::Conflict(error.to_string()))?;
        let mut tx = self.pool.begin().await.map_err(|e| map_err("decide promotion", e))?;
        let row: Option<(String, i16, OffsetDateTime)> = sqlx::query_as(
            "SELECT status, required_approvals, expires_at FROM promotion_requests \
             WHERE id = $1 FOR UPDATE",
        )
        .bind(decision.promotion_id.as_uuid())
        .fetch_optional(&mut *tx)
        .await
        .map_err(|e| map_err("read promotion", e))?;
        let Some((status, required, expires_at)) = row else {
            return Err(StoreError::NotFound("promotion".to_owned()));
        };
        if status != "pending_approval" || expires_at <= OffsetDateTime::now_utc() {
            return Err(StoreError::Conflict(
                "the promotion is not waiting for a decision".to_owned(),
            ));
        }
        sqlx::query(
            "INSERT INTO promotion_decisions (promotion_id, approver, approve, comment, counted, at) \
             VALUES ($1,$2,$3,$4,$5,$6)",
        )
        .bind(decision.promotion_id.as_uuid())
        .bind(decision.approver.as_uuid())
        .bind(decision.approve)
        .bind(&decision.comment)
        .bind(decision.counted)
        .bind(decision.at)
        .execute(&mut *tx)
        .await
        .map_err(|e| map_err("record promotion decision", e))?;
        let counted: i64 = sqlx::query_scalar(
            "SELECT count(*)::bigint FROM promotion_decisions \
             WHERE promotion_id = $1 AND approve AND counted",
        )
        .bind(decision.promotion_id.as_uuid())
        .fetch_one(&mut *tx)
        .await
        .map_err(|e| map_err("count promotion decisions", e))?;
        let outcome = if decision.approve {
            if counted >= i64::from(required) {
                PromotionStatus::Approved
            } else {
                PromotionStatus::PendingApproval
            }
        } else {
            PromotionStatus::Rejected
        };
        if outcome == PromotionStatus::Rejected {
            sqlx::query(
                "UPDATE promotion_requests SET status = 'rejected', finished_at = $2 WHERE id = $1",
            )
            .bind(decision.promotion_id.as_uuid())
            .bind(decision.at)
            .execute(&mut *tx)
            .await
            .map_err(|e| map_err("reject promotion", e))?;
        } else if outcome == PromotionStatus::Approved {
            sqlx::query("UPDATE promotion_requests SET status = 'approved' WHERE id = $1")
                .bind(decision.promotion_id.as_uuid())
                .execute(&mut *tx)
                .await
                .map_err(|e| map_err("approve promotion", e))?;
            let key = format!("promotion:{}", decision.promotion_id);
            sqlx::query(
                "INSERT INTO job_leases (id, queue, dedupe_key, payload, available_at) \
                 VALUES ($1, 'promotion', $2, $3, now()) \
                 ON CONFLICT (queue, dedupe_key) WHERE dedupe_key IS NOT NULL \
                     AND state IN ('queued', 'running') DO NOTHING",
            )
            .bind(Uuid::now_v7())
            .bind(key)
            .bind(serde_json::json!({ "promotion_id": decision.promotion_id }))
            .execute(&mut *tx)
            .await
            .map_err(|e| map_err("enqueue promotion", e))?;
        }
        outbox::insert_events(&mut tx, events).await?;
        tx.commit().await.map_err(|e| map_err("decide promotion", e))?;
        Ok(outcome)
    }

    async fn decisions(&self, id: PromotionId) -> Result<Vec<PromotionDecision>, StoreError> {
        let exists: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM promotion_requests WHERE id = $1)")
            .bind(id.as_uuid())
            .fetch_one(&self.pool)
            .await
            .map_err(|e| map_err("read promotion", e))?;
        if !exists {
            return Err(StoreError::NotFound("promotion".to_owned()));
        }
        let rows: Vec<(Uuid, bool, String, bool, OffsetDateTime)> = sqlx::query_as(
            "SELECT approver, approve, comment, counted, at FROM promotion_decisions \
             WHERE promotion_id = $1 ORDER BY at",
        )
        .bind(id.as_uuid())
        .fetch_all(&self.pool)
        .await
        .map_err(|e| map_err("list promotion decisions", e))?;
        Ok(rows
            .into_iter()
            .map(|(approver, approve, comment, counted, at)| PromotionDecision {
                promotion_id: id,
                approver: UserId::from_uuid(approver),
                approve,
                comment,
                counted,
                at,
            })
            .collect())
    }

    async fn cancel(&self, id: PromotionId, by: UserId) -> Result<(), StoreError> {
        let row: Option<(Uuid, String)> = sqlx::query_as(
            "SELECT requested_by, status FROM promotion_requests WHERE id = $1",
        )
        .bind(id.as_uuid())
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| map_err("read promotion", e))?;
        let Some((requester, status)) = row else {
            return Err(StoreError::NotFound("promotion".to_owned()));
        };
        if requester != by.as_uuid() {
            return Err(StoreError::Conflict(
                "only the requester may cancel a promotion".to_owned(),
            ));
        }
        if status != "pending_approval" {
            return Err(StoreError::Conflict(
                "only a pending promotion can be cancelled".to_owned(),
            ));
        }
        sqlx::query(
            "UPDATE promotion_requests SET status = 'cancelled', finished_at = now() WHERE id = $1",
        )
        .bind(id.as_uuid())
        .execute(&self.pool)
        .await
        .map_err(|e| map_err("cancel promotion", e))?;
        Ok(())
    }

    async fn items(
        &self,
        id: PromotionId,
        page: &PageRequest,
    ) -> Result<Page<PromotionItem>, StoreError> {
        let exists: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM promotion_requests WHERE id = $1)")
            .bind(id.as_uuid())
            .fetch_one(&self.pool)
            .await
            .map_err(|e| map_err("read promotion", e))?;
        if !exists {
            return Err(StoreError::NotFound("promotion".to_owned()));
        }
        let limit = page.limit.clamp(1, 100);
        let rows: Vec<(Uuid, String, Option<String>)> = sqlx::query_as(
            "SELECT component_id, status, reason FROM promotion_items \
             WHERE promotion_id = $1 ORDER BY component_id LIMIT $2",
        )
        .bind(id.as_uuid())
        .bind(i64::from(limit))
        .fetch_all(&self.pool)
        .await
        .map_err(|e| map_err("list promotion items", e))?;
        let items = rows
            .into_iter()
            .map(|(component, status, reason)| {
                Ok(PromotionItem {
                    promotion_id: id,
                    component_id: ComponentId::from_uuid(component),
                    status: decode_item_status(&status)?,
                    reason,
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Page { items, next: None })
    }

    async fn mark_running(&self, id: PromotionId, at: OffsetDateTime) -> Result<bool, StoreError> {
        let updated = sqlx::query(
            "UPDATE promotion_requests SET status = 'running', started_at = $2 \
             WHERE id = $1 AND status = 'approved'",
        )
        .bind(id.as_uuid())
        .bind(at)
        .execute(&self.pool)
        .await
        .map_err(|e| map_err("start promotion", e))?;
        if updated.rows_affected() == 1 {
            return Ok(true);
        }
        let exists: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM promotion_requests WHERE id = $1)")
            .bind(id.as_uuid())
            .fetch_one(&self.pool)
            .await
            .map_err(|e| map_err("read promotion", e))?;
        if exists {
            Ok(false)
        } else {
            Err(StoreError::NotFound("promotion".to_owned()))
        }
    }

    async fn set_item(
        &self,
        id: PromotionId,
        component: ComponentId,
        status: PromotionItemStatus,
        reason: Option<&str>,
    ) -> Result<(), StoreError> {
        let updated = sqlx::query(
            "UPDATE promotion_items SET status = $3, reason = $4 \
             WHERE promotion_id = $1 AND component_id = $2",
        )
        .bind(id.as_uuid())
        .bind(component.as_uuid())
        .bind(item_status_label(status))
        .bind(reason.map(|text| text.chars().take(64).collect::<String>()))
        .execute(&self.pool)
        .await
        .map_err(|e| map_err("update promotion item", e))?;
        if updated.rows_affected() == 0 {
            return Err(StoreError::NotFound("promotion item".to_owned()));
        }
        Ok(())
    }

    async fn finish(
        &self,
        id: PromotionId,
        status: PromotionStatus,
        at: OffsetDateTime,
        events: &[EventDraft],
    ) -> Result<(), StoreError> {
        if !status.is_terminal() {
            return Err(StoreError::Invalid(
                "a promotion ends with a terminal status".to_owned(),
            ));
        }
        let mut tx = self.pool.begin().await.map_err(|e| map_err("finish promotion", e))?;
        let updated = sqlx::query(
            "UPDATE promotion_requests SET status = $2, finished_at = $3 \
             WHERE id = $1 AND status IN ('approved', 'running')",
        )
        .bind(id.as_uuid())
        .bind(status_label(status)?)
        .bind(at)
        .execute(&mut *tx)
        .await
        .map_err(|e| map_err("finish promotion", e))?;
        if updated.rows_affected() == 0 {
            let exists: bool =
                sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM promotion_requests WHERE id = $1)")
                    .bind(id.as_uuid())
                    .fetch_one(&mut *tx)
                    .await
                    .map_err(|e| map_err("read promotion", e))?;
            return Err(if exists {
                StoreError::Conflict("the promotion is not executing".to_owned())
            } else {
                StoreError::NotFound("promotion".to_owned())
            });
        }
        outbox::insert_events(&mut tx, events).await?;
        tx.commit().await.map_err(|e| map_err("finish promotion", e))
    }

    async fn expire(&self, now: OffsetDateTime) -> Result<u64, StoreError> {
        let updated = sqlx::query(
            "UPDATE promotion_requests SET status = 'expired', finished_at = $1 \
             WHERE status = 'pending_approval' AND expires_at <= $1",
        )
        .bind(now)
        .execute(&self.pool)
        .await
        .map_err(|e| map_err("expire promotions", e))?;
        Ok(updated.rows_affected())
    }
}

fn decode_target(row: TargetRow) -> Result<PromotionTarget, StoreError> {
    let mut target = PromotionTarget::new(RepositoryId::from_uuid(row.repository_id));
    target.require_approval = row.require_approval;
    target.required_approvals = u8::try_from(row.required_approvals).unwrap_or(u8::MAX);
    target.checks = serde_json::from_value::<Vec<PromotionCheck>>(row.checks)
        .map_err(|_| backend("read promotion target", "checks are not checks"))?;
    target.version = row.version;
    Ok(target)
}

fn decode_request(row: RequestRow) -> Result<PromotionRequest, StoreError> {
    let selector = serde_json::from_value::<PromotionSelector>(row.selector)
        .map_err(|_| backend("read promotion", "selector is not a selector"))?;
    let mut request = PromotionRequest::new(
        PromotionId::from_uuid(row.id),
        decode_mode(&row.mode)?,
        RepositoryId::from_uuid(row.source_repository_id),
        RepositoryId::from_uuid(row.target_repository_id),
        selector,
        UserId::from_uuid(row.requested_by),
        row.requested_at,
    );
    request.status = decode_status(&row.status)?;
    request.required_approvals = u8::try_from(row.required_approvals).unwrap_or(u8::MAX);
    request.checks = serde_json::from_value(row.checks)
        .map_err(|_| backend("read promotion", "checks are not checks"))?;
    request.expires_at = row.expires_at;
    request.started_at = row.started_at;
    request.finished_at = row.finished_at;
    Ok(request)
}

fn mode_label(mode: PromotionMode) -> &'static str {
    match mode {
        PromotionMode::Copy => "copy",
        PromotionMode::Move => "move",
    }
}

fn decode_mode(label: &str) -> Result<PromotionMode, StoreError> {
    Ok(match label {
        "copy" => PromotionMode::Copy,
        "move" => PromotionMode::Move,
        _ => return Err(backend("read promotion", "mode is unknown")),
    })
}

fn status_label(status: PromotionStatus) -> Result<&'static str, StoreError> {
    Ok(match status {
        PromotionStatus::PendingApproval => "pending_approval",
        PromotionStatus::Approved => "approved",
        PromotionStatus::Rejected => "rejected",
        PromotionStatus::Running => "running",
        PromotionStatus::Succeeded => "succeeded",
        PromotionStatus::PartiallyFailed => "partially_failed",
        PromotionStatus::Failed => "failed",
        PromotionStatus::Cancelled => "cancelled",
        PromotionStatus::Expired => "expired",
        _ => return Err(backend("write promotion", "status is unknown")),
    })
}

fn decode_status(label: &str) -> Result<PromotionStatus, StoreError> {
    Ok(match label {
        "pending_approval" => PromotionStatus::PendingApproval,
        "approved" => PromotionStatus::Approved,
        "rejected" => PromotionStatus::Rejected,
        "running" => PromotionStatus::Running,
        "succeeded" => PromotionStatus::Succeeded,
        "partially_failed" => PromotionStatus::PartiallyFailed,
        "failed" => PromotionStatus::Failed,
        "cancelled" => PromotionStatus::Cancelled,
        "expired" => PromotionStatus::Expired,
        _ => return Err(backend("read promotion", "status is unknown")),
    })
}

fn item_status_label(status: PromotionItemStatus) -> &'static str {
    match status {
        PromotionItemStatus::Pending => "pending",
        PromotionItemStatus::Done => "done",
        PromotionItemStatus::Skipped => "skipped",
        PromotionItemStatus::Failed => "failed",
    }
}

fn decode_item_status(label: &str) -> Result<PromotionItemStatus, StoreError> {
    Ok(match label {
        "pending" => PromotionItemStatus::Pending,
        "done" => PromotionItemStatus::Done,
        "skipped" => PromotionItemStatus::Skipped,
        "failed" => PromotionItemStatus::Failed,
        _ => return Err(backend("read promotion item", "status is unknown")),
    })
}
