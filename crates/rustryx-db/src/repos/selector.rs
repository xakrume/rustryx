//! [`SelectorRepo`] on `PostgreSQL`.

use async_trait::async_trait;
use rustryx_core::StoreError;
use rustryx_core::identity::ContentSelector;
use rustryx_core::ids::ContentSelectorId;
use rustryx_core::ports::{InvalidationEvent, SelectorRepo};
use sqlx::{FromRow, PgPool};
use time::OffsetDateTime;
use uuid::Uuid;

use super::common::notify;
use crate::error::map_err;

macro_rules! select {
    ($($tail:literal),*) => {
        concat!(
            "SELECT id, name, description, expression, updated_at FROM content_selectors ",
            $($tail),*
        )
    };
}

/// Content selectors.
#[derive(Debug, Clone)]
pub struct PgSelectorRepo {
    pool: PgPool,
}

impl PgSelectorRepo {
    /// Creates the repository over `pool`.
    #[must_use]
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[derive(Debug, FromRow)]
struct Row {
    id: Uuid,
    name: String,
    description: String,
    expression: String,
    updated_at: OffsetDateTime,
}

impl From<Row> for ContentSelector {
    fn from(row: Row) -> Self {
        Self {
            id: ContentSelectorId::from_uuid(row.id),
            name: row.name,
            description: row.description,
            expression: row.expression,
            updated_at: row.updated_at,
        }
    }
}

#[async_trait]
impl SelectorRepo for PgSelectorRepo {
    async fn get(&self, id: ContentSelectorId) -> Result<Option<ContentSelector>, StoreError> {
        Ok(sqlx::query_as::<_, Row>(select!("WHERE id = $1"))
            .bind(id.as_uuid())
            .fetch_optional(&self.pool)
            .await
            .map_err(|e| map_err("read selector", e))?
            .map(ContentSelector::from))
    }

    async fn list(&self) -> Result<Vec<ContentSelector>, StoreError> {
        Ok(sqlx::query_as::<_, Row>(select!("ORDER BY name"))
            .fetch_all(&self.pool)
            .await
            .map_err(|e| map_err("list selectors", e))?
            .into_iter()
            .map(ContentSelector::from)
            .collect())
    }

    async fn create(&self, selector: &ContentSelector) -> Result<(), StoreError> {
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| map_err("create selector", e))?;
        sqlx::query(
            "INSERT INTO content_selectors (id, name, description, expression, updated_at) \
             VALUES ($1, $2, $3, $4, $5)",
        )
        .bind(selector.id.as_uuid())
        .bind(&selector.name)
        .bind(&selector.description)
        .bind(&selector.expression)
        .bind(selector.updated_at)
        .execute(&mut *tx)
        .await
        .map_err(|e| map_err("create selector", e))?;
        notify(&mut tx, &InvalidationEvent::Rbac).await?;
        tx.commit().await.map_err(|e| map_err("create selector", e))
    }

    /// `updated_at` is part of the compiled-program cache key, so it must change on every
    /// edit. The stored value is the caller's time, but never less than one microsecond after
    /// the previous one: a caller whose clock repeated a value (or went back) would otherwise
    /// leave the old compiled program cached under an unchanged key.
    async fn update(&self, selector: &ContentSelector) -> Result<(), StoreError> {
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| map_err("update selector", e))?;
        // `name` stays out of the SET list unless it changes (see the note in `role.rs`).
        let current_name: Option<String> = sqlx::query_scalar(
            "UPDATE content_selectors SET description = $2, expression = $3, \
             updated_at = GREATEST($4, updated_at + interval '1 microsecond') \
             WHERE id = $1 RETURNING name",
        )
        .bind(selector.id.as_uuid())
        .bind(&selector.description)
        .bind(&selector.expression)
        .bind(selector.updated_at)
        .fetch_optional(&mut *tx)
        .await
        .map_err(|e| map_err("update selector", e))?;
        let Some(current_name) = current_name else {
            return Err(StoreError::NotFound("content selector".to_owned()));
        };
        if current_name != selector.name {
            sqlx::query("UPDATE content_selectors SET name = $2 WHERE id = $1")
                .bind(selector.id.as_uuid())
                .bind(&selector.name)
                .execute(&mut *tx)
                .await
                .map_err(|e| map_err("rename selector", e))?;
        }
        notify(&mut tx, &InvalidationEvent::Rbac).await?;
        tx.commit().await.map_err(|e| map_err("update selector", e))
    }

    /// `Conflict` while a privilege references the selector (foreign key `RESTRICT`).
    async fn delete(&self, id: ContentSelectorId) -> Result<(), StoreError> {
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| map_err("delete selector", e))?;
        let result = sqlx::query("DELETE FROM content_selectors WHERE id = $1")
            .bind(id.as_uuid())
            .execute(&mut *tx)
            .await
            .map_err(|e| map_err("delete selector", e))?;
        if result.rows_affected() == 0 {
            return Err(StoreError::NotFound("content selector".to_owned()));
        }
        notify(&mut tx, &InvalidationEvent::Rbac).await?;
        tx.commit().await.map_err(|e| map_err("delete selector", e))
    }
}
