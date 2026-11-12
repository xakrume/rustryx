//! [`LabelRepo`] on PostgreSQL.

use async_trait::async_trait;
use rustryx_core::StoreError;
use rustryx_core::ids::{ComponentId, LabelId, ProjectId, UserId};
use rustryx_core::label::{Label, LabelName, LabelScope, MAX_LABELS_PER_COMPONENT};
use rustryx_core::ports::{LabelRepo, Page, PageRequest};
use sqlx::{FromRow, PgPool};
use time::OffsetDateTime;
use uuid::Uuid;

use super::common::{corrupt, to_u64};
use crate::cursor;
use crate::error::map_err;

const PAGE: &str = "label";
const LIMIT_TEXT: &str = "a component carries at most 50 labels";

macro_rules! select {
    ($($tail:literal),*) => {
        concat!(
            "SELECT id, project_id, name, color, description, attributes, version, \
             created_at, updated_at FROM labels ",
            $($tail),*
        )
    };
}

/// Labels and the links that apply them to components.
#[derive(Debug, Clone)]
pub struct PgLabelRepo {
    pool: PgPool,
}

impl PgLabelRepo {
    /// Creates the repository over `pool`.
    #[must_use]
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[derive(Debug, FromRow)]
struct Row {
    id: Uuid,
    project_id: Option<Uuid>,
    name: String,
    color: String,
    description: String,
    attributes: serde_json::Value,
    version: i64,
    created_at: OffsetDateTime,
    updated_at: OffsetDateTime,
}

impl Row {
    fn into_label(self) -> Result<Label, StoreError> {
        let scope = match self.project_id {
            None => LabelScope::System,
            Some(id) => LabelScope::Project(ProjectId::from_uuid(id)),
        };
        let name = LabelName::parse(&self.name).map_err(|_| corrupt("read label"))?;
        let mut label = Label::new(LabelId::from_uuid(self.id), scope, name, self.created_at);
        label.color = self.color;
        label.description = self.description;
        label.attributes = self.attributes;
        label.version = self.version;
        label.updated_at = self.updated_at;
        Ok(label)
    }
}

fn missing_label(id: LabelId) -> StoreError {
    StoreError::NotFound(format!("label {id}"))
}

fn missing_component(id: ComponentId) -> StoreError {
    StoreError::NotFound(format!("component {id}"))
}

fn stale(expected: i64, found: i64) -> StoreError {
    StoreError::Conflict(format!("stale version: expected {expected}, found {found}"))
}

fn validated(label: &Label) -> Result<(), StoreError> {
    label
        .validate()
        .map_err(|error| StoreError::Conflict(error.to_string()))
}

fn is_unique(err: &sqlx::Error) -> bool {
    match err {
        sqlx::Error::Database(db) => db.code().as_deref() == Some("23505"),
        _ => false,
    }
}

fn project_id_of(scope: LabelScope) -> Option<Uuid> {
    match scope {
        LabelScope::System => None,
        LabelScope::Project(id) => Some(id.as_uuid()),
    }
}

async fn load(pool: &PgPool, id: LabelId) -> Result<Option<Label>, StoreError> {
    sqlx::query_as::<_, Row>(select!("WHERE id = $1"))
        .bind(id.as_uuid())
        .fetch_optional(pool)
        .await
        .map_err(|e| map_err("read label", e))?
        .map(Row::into_label)
        .transpose()
}

#[async_trait]
impl LabelRepo for PgLabelRepo {
    async fn create(&self, label: &Label) -> Result<(), StoreError> {
        validated(label)?;
        let inserted = sqlx::query(
            "INSERT INTO labels (id, project_id, name, color, description, attributes, version, \
             created_at, updated_at) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)",
        )
        .bind(label.id.as_uuid())
        .bind(project_id_of(label.scope))
        .bind(label.name.as_str())
        .bind(&label.color)
        .bind(&label.description)
        .bind(&label.attributes)
        .bind(label.version)
        .bind(label.created_at)
        .bind(label.updated_at)
        .execute(&self.pool)
        .await;
        match inserted {
            Ok(_) => Ok(()),
            Err(err) => Err(map_err("create label", err)),
        }
    }

    async fn get(&self, id: LabelId) -> Result<Option<Label>, StoreError> {
        load(&self.pool, id).await
    }

    async fn list(&self, scope: LabelScope, page: &PageRequest) -> Result<Page<Label>, StoreError> {
        let (limit, fetch) = cursor::fetch_limit(page);
        let after = match &page.after {
            Some(token) => Some(cursor::decode(PAGE, token)?),
            None => None,
        };
        let rows = match scope {
            LabelScope::System => {
                sqlx::query_as::<_, Row>(select!(
                    "WHERE project_id IS NULL AND ($1::text IS NULL OR name > $1) \
                     ORDER BY name LIMIT $2"
                ))
                .bind(after)
                .bind(fetch)
                .fetch_all(&self.pool)
                .await
            }
            LabelScope::Project(project) => {
                sqlx::query_as::<_, Row>(select!(
                    "WHERE project_id = $1 AND ($2::text IS NULL OR name > $2) \
                     ORDER BY name LIMIT $3"
                ))
                .bind(project.as_uuid())
                .bind(after)
                .bind(fetch)
                .fetch_all(&self.pool)
                .await
            }
        }
        .map_err(|e| map_err("list labels", e))?;
        let items = rows
            .into_iter()
            .map(Row::into_label)
            .collect::<Result<Vec<_>, _>>()?;
        Ok(cursor::finish(items, limit, PAGE, |label| {
            label.name.as_str().to_owned()
        }))
    }

    async fn update(&self, label: &Label, expected_version: i64) -> Result<(), StoreError> {
        validated(label)?;
        let updated = sqlx::query(
            "UPDATE labels SET name = $2, color = $3, description = $4, attributes = $5, \
             version = $6, updated_at = now() WHERE id = $1 AND version = $7",
        )
        .bind(label.id.as_uuid())
        .bind(label.name.as_str())
        .bind(&label.color)
        .bind(&label.description)
        .bind(&label.attributes)
        .bind(expected_version.saturating_add(1))
        .bind(expected_version)
        .execute(&self.pool)
        .await
        .map_err(|e| map_err("update label", e))?;
        if updated.rows_affected() == 1 {
            return Ok(());
        }
        let found: Option<i64> = sqlx::query_scalar("SELECT version FROM labels WHERE id = $1")
            .bind(label.id.as_uuid())
            .fetch_optional(&self.pool)
            .await
            .map_err(|e| map_err("read label", e))?;
        match found {
            None => Err(missing_label(label.id)),
            Some(found) => Err(stale(expected_version, found)),
        }
    }

    async fn delete(&self, id: LabelId) -> Result<u64, StoreError> {
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| map_err("delete label", e))?;
        let exists: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM labels WHERE id = $1)")
            .bind(id.as_uuid())
            .fetch_one(&mut *tx)
            .await
            .map_err(|e| map_err("delete label", e))?;
        if !exists {
            return Err(missing_label(id));
        }
        let links: i64 =
            sqlx::query_scalar("SELECT count(*)::bigint FROM component_labels WHERE label_id = $1")
                .bind(id.as_uuid())
                .fetch_one(&mut *tx)
                .await
                .map_err(|e| map_err("delete label", e))?;
        sqlx::query("DELETE FROM labels WHERE id = $1")
            .bind(id.as_uuid())
            .execute(&mut *tx)
            .await
            .map_err(|e| map_err("delete label", e))?;
        tx.commit().await.map_err(|e| map_err("delete label", e))?;
        to_u64(links, "delete label")
    }

    async fn apply(
        &self,
        component: ComponentId,
        label: LabelId,
        by: Option<UserId>,
    ) -> Result<(), StoreError> {
        if load(&self.pool, label).await?.is_none() {
            return Err(missing_label(label));
        }
        let component_exists: bool =
            sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM components WHERE id = $1)")
                .bind(component.as_uuid())
                .fetch_one(&self.pool)
                .await
                .map_err(|e| map_err("apply label", e))?;
        if !component_exists {
            return Err(missing_component(component));
        }
        let linked: bool = sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM component_labels \
             WHERE component_id = $1 AND label_id = $2)",
        )
        .bind(component.as_uuid())
        .bind(label.as_uuid())
        .fetch_one(&self.pool)
        .await
        .map_err(|e| map_err("apply label", e))?;
        if linked {
            return Ok(());
        }
        let on_component: i64 = sqlx::query_scalar(
            "SELECT count(*)::bigint FROM component_labels WHERE component_id = $1",
        )
        .bind(component.as_uuid())
        .fetch_one(&self.pool)
        .await
        .map_err(|e| map_err("apply label", e))?;
        if usize::try_from(on_component).unwrap_or(usize::MAX) >= MAX_LABELS_PER_COMPONENT {
            return Err(StoreError::Invalid(LIMIT_TEXT.to_owned()));
        }
        let inserted = sqlx::query(
            "INSERT INTO component_labels (component_id, label_id, applied_by) \
             VALUES ($1, $2, $3)",
        )
        .bind(component.as_uuid())
        .bind(label.as_uuid())
        .bind(by.map(|id| id.as_uuid()))
        .execute(&self.pool)
        .await;
        match inserted {
            Ok(_) => Ok(()),
            Err(err) if is_unique(&err) => Ok(()),
            Err(err) => Err(map_err("apply label", err)),
        }
    }

    async fn unapply(&self, component: ComponentId, label: LabelId) -> Result<(), StoreError> {
        sqlx::query("DELETE FROM component_labels WHERE component_id = $1 AND label_id = $2")
            .bind(component.as_uuid())
            .bind(label.as_uuid())
            .execute(&self.pool)
            .await
            .map_err(|e| map_err("unapply label", e))?;
        Ok(())
    }

    async fn labels_of(&self, component: ComponentId) -> Result<Vec<Label>, StoreError> {
        let rows = sqlx::query_as::<_, Row>(select!(
            "JOIN component_labels AS link ON link.label_id = labels.id \
             WHERE link.component_id = $1 ORDER BY labels.name"
        ))
        .bind(component.as_uuid())
        .fetch_all(&self.pool)
        .await
        .map_err(|e| map_err("read labels", e))?;
        rows.into_iter().map(Row::into_label).collect()
    }

    async fn usage(&self, label: LabelId) -> Result<u64, StoreError> {
        let count: i64 =
            sqlx::query_scalar("SELECT count(*)::bigint FROM component_labels WHERE label_id = $1")
                .bind(label.as_uuid())
                .fetch_one(&self.pool)
                .await
                .map_err(|e| map_err("read label usage", e))?;
        to_u64(count, "read label usage")
    }
}
