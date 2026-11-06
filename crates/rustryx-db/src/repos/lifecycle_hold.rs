//! [`LifecycleHoldRepo`] on PostgreSQL.

use async_trait::async_trait;
use rustryx_core::StoreError;
use rustryx_core::ids::{ComponentId, LifecycleHoldId, ProjectId, RepositoryId, UserId};
use rustryx_core::lifecycle::{HoldKind, HoldTarget, LifecycleHold};
use rustryx_core::ports::{LifecycleHoldRepo, Page, PageRequest};
use sqlx::{FromRow, PgPool};
use time::OffsetDateTime;
use uuid::Uuid;

use super::common::corrupt;
use crate::cursor;
use crate::error::map_err;

const KIND: &str = "lifecycle_hold";

macro_rules! select {
    ($($tail:literal),*) => {
        concat!(
            "SELECT id, kind, component_id, repository_id, project_id, reason, created_by, \
             created_at, expires_at, released_at, released_by FROM lifecycle_holds ",
            $($tail),*
        )
    };
}

/// Lifecycle holds.
#[derive(Debug, Clone)]
pub struct PgLifecycleHoldRepo {
    pool: PgPool,
}

impl PgLifecycleHoldRepo {
    /// Creates the repository over `pool`.
    #[must_use]
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[derive(Debug, FromRow)]
struct Row {
    id: Uuid,
    kind: String,
    component_id: Option<Uuid>,
    repository_id: Option<Uuid>,
    project_id: Option<Uuid>,
    reason: String,
    created_by: Option<Uuid>,
    created_at: OffsetDateTime,
    expires_at: Option<OffsetDateTime>,
    released_at: Option<OffsetDateTime>,
    released_by: Option<Uuid>,
}

fn kind_label(kind: HoldKind) -> &'static str {
    match kind {
        HoldKind::Legal => "legal",
        HoldKind::Release => "release",
        HoldKind::Promotion => "promotion",
    }
}

fn parse_kind(raw: &str) -> Result<HoldKind, StoreError> {
    match raw {
        "legal" => Ok(HoldKind::Legal),
        "release" => Ok(HoldKind::Release),
        "promotion" => Ok(HoldKind::Promotion),
        _ => Err(corrupt("read lifecycle hold")),
    }
}

struct TargetIds {
    component_id: Option<Uuid>,
    repository_id: Option<Uuid>,
    project_id: Option<Uuid>,
}

fn target_ids(target: HoldTarget) -> TargetIds {
    match target {
        HoldTarget::Component(id) => TargetIds {
            component_id: Some(id.as_uuid()),
            repository_id: None,
            project_id: None,
        },
        HoldTarget::Repository(id) => TargetIds {
            component_id: None,
            repository_id: Some(id.as_uuid()),
            project_id: None,
        },
        HoldTarget::Project(id) => TargetIds {
            component_id: None,
            repository_id: None,
            project_id: Some(id.as_uuid()),
        },
    }
}

impl Row {
    fn into_hold(self) -> Result<LifecycleHold, StoreError> {
        let target = match (self.component_id, self.repository_id, self.project_id) {
            (Some(id), None, None) => HoldTarget::Component(ComponentId::from_uuid(id)),
            (None, Some(id), None) => HoldTarget::Repository(RepositoryId::from_uuid(id)),
            (None, None, Some(id)) => HoldTarget::Project(ProjectId::from_uuid(id)),
            _ => return Err(corrupt("read lifecycle hold")),
        };
        let mut hold = LifecycleHold::new(
            LifecycleHoldId::from_uuid(self.id),
            parse_kind(&self.kind)?,
            target,
            self.reason,
            self.created_at,
        );
        hold.created_by = self.created_by.map(UserId::from_uuid);
        hold.expires_at = self.expires_at;
        hold.released_at = self.released_at;
        hold.released_by = self.released_by.map(UserId::from_uuid);
        Ok(hold)
    }
}

#[async_trait]
impl LifecycleHoldRepo for PgLifecycleHoldRepo {
    async fn create(&self, hold: &LifecycleHold) -> Result<(), StoreError> {
        hold.validate()
            .map_err(|error| StoreError::Conflict(error.to_string()))?;
        let target = target_ids(hold.target);
        sqlx::query(
            "INSERT INTO lifecycle_holds (id, kind, component_id, repository_id, project_id, \
             reason, created_by, created_at, expires_at, released_at, released_by) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)",
        )
        .bind(hold.id.as_uuid())
        .bind(kind_label(hold.kind))
        .bind(target.component_id)
        .bind(target.repository_id)
        .bind(target.project_id)
        .bind(&hold.reason)
        .bind(hold.created_by.map(|id| id.as_uuid()))
        .bind(hold.created_at)
        .bind(hold.expires_at)
        .bind(hold.released_at)
        .bind(hold.released_by.map(|id| id.as_uuid()))
        .execute(&self.pool)
        .await
        .map_err(|e| map_err("create lifecycle hold", e))?;
        Ok(())
    }

    async fn get(&self, id: LifecycleHoldId) -> Result<Option<LifecycleHold>, StoreError> {
        sqlx::query_as::<_, Row>(select!("WHERE id = $1"))
            .bind(id.as_uuid())
            .fetch_optional(&self.pool)
            .await
            .map_err(|e| map_err("read lifecycle hold", e))?
            .map(Row::into_hold)
            .transpose()
    }

    async fn release(
        &self,
        id: LifecycleHoldId,
        by: Option<UserId>,
        at: OffsetDateTime,
    ) -> Result<(), StoreError> {
        let updated = sqlx::query(
            "UPDATE lifecycle_holds SET released_at = $2, released_by = $3 \
             WHERE id = $1 AND released_at IS NULL",
        )
        .bind(id.as_uuid())
        .bind(at)
        .bind(by.map(|user| user.as_uuid()))
        .execute(&self.pool)
        .await
        .map_err(|e| map_err("release lifecycle hold", e))?;
        if updated.rows_affected() == 1 {
            return Ok(());
        }
        let exists: bool =
            sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM lifecycle_holds WHERE id = $1)")
                .bind(id.as_uuid())
                .fetch_one(&self.pool)
                .await
                .map_err(|e| map_err("release lifecycle hold", e))?;
        if exists {
            return Ok(());
        }
        Err(StoreError::NotFound("lifecycle hold".to_owned()))
    }

    async fn list(
        &self,
        target: Option<HoldTarget>,
        active_only: bool,
        page: &PageRequest,
    ) -> Result<Page<LifecycleHold>, StoreError> {
        let ids = target.map_or(
            TargetIds {
                component_id: None,
                repository_id: None,
                project_id: None,
            },
            target_ids,
        );
        let (limit, fetch) = cursor::fetch_limit(page);
        let after = match &page.after {
            Some(token) => Some(
                Uuid::parse_str(&cursor::decode(KIND, token)?)
                    .map_err(|_| cursor::invalid_token())?,
            ),
            None => None,
        };
        let rows = sqlx::query_as::<_, Row>(select!(
            "WHERE ($1::uuid IS NULL OR component_id = $1) \
             AND ($2::uuid IS NULL OR repository_id = $2) \
             AND ($3::uuid IS NULL OR project_id = $3) \
             AND ($4::bool = false OR ( \
                 released_at IS NULL AND (expires_at IS NULL OR expires_at > now()))) \
             AND ($5::uuid IS NULL OR id > $5) ORDER BY id LIMIT $6"
        ))
        .bind(ids.component_id)
        .bind(ids.repository_id)
        .bind(ids.project_id)
        .bind(active_only)
        .bind(after)
        .bind(fetch)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| map_err("list lifecycle holds", e))?;
        let items = rows
            .into_iter()
            .map(Row::into_hold)
            .collect::<Result<Vec<_>, _>>()?;
        Ok(cursor::finish(items, limit, KIND, |hold| {
            hold.id.to_string()
        }))
    }

    async fn active_for(&self, component: ComponentId) -> Result<Vec<LifecycleHold>, StoreError> {
        let rows = sqlx::query_as::<_, Row>(select!(
            "WHERE released_at IS NULL AND (expires_at IS NULL OR expires_at > now()) \
             AND EXISTS ( \
                 SELECT 1 FROM components \
                 JOIN repositories ON repositories.id = components.repository_id \
                 WHERE components.id = $1 \
                   AND (lifecycle_holds.component_id = components.id \
                     OR lifecycle_holds.repository_id = components.repository_id \
                     OR lifecycle_holds.project_id = repositories.project_id)) \
             ORDER BY id"
        ))
        .bind(component.as_uuid())
        .fetch_all(&self.pool)
        .await
        .map_err(|e| map_err("read active lifecycle holds", e))?;
        rows.into_iter().map(Row::into_hold).collect()
    }
}
