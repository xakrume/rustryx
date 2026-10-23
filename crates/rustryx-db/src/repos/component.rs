//! [`ComponentRepo`] on `PostgreSQL`.

use std::str::FromStr;

use async_trait::async_trait;
use rustryx_core::StoreError;
use rustryx_core::error::MutationError;
use rustryx_core::ids::{ComponentId, RepositoryId, UserId};
use rustryx_core::model::{Component, NewComponent, RepoFormat, WriteOptions};
use rustryx_core::ports::{ComponentFilter, ComponentRepo, Page, PageRequest};
use sqlx::{FromRow, PgPool, Postgres, QueryBuilder};
use time::OffsetDateTime;
use uuid::Uuid;

use super::common::{corrupt, escape_like};
use super::write;
use crate::cursor;
use crate::error::{map_err, map_mutation_err};

const KIND: &str = "component";

const COLUMNS: &str = "id, repository_id, format, group_id, name, version, created_by, \
                       last_downloaded_at, created_at, updated_at";

/// Components.
#[derive(Debug, Clone)]
pub struct PgComponentRepo {
    pool: PgPool,
}

impl PgComponentRepo {
    /// Creates the repository over `pool`.
    #[must_use]
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[derive(Debug, FromRow)]
struct Row {
    id: Uuid,
    repository_id: Uuid,
    format: String,
    group_id: Option<String>,
    name: String,
    version: Option<String>,
    created_by: Option<Uuid>,
    last_downloaded_at: Option<OffsetDateTime>,
    created_at: OffsetDateTime,
    updated_at: OffsetDateTime,
}

impl Row {
    fn into_component(self) -> Result<Component, StoreError> {
        let mut component = Component::new(
            ComponentId::from_uuid(self.id),
            RepositoryId::from_uuid(self.repository_id),
            RepoFormat::from_str(&self.format).map_err(|_| corrupt("read component"))?,
            self.name,
            self.created_at,
        );
        component.group_id = self.group_id;
        component.version = self.version;
        component.created_by = self.created_by.map(UserId::from_uuid);
        component.last_downloaded_at = self.last_downloaded_at;
        component.updated_at = self.updated_at;
        Ok(component)
    }
}

/// `col = $n` or `col IS NULL`: `IS NOT DISTINCT FROM` would be NULL-safe too, but the planner
/// cannot use the coordinate index for it.
fn push_nullable_eq(builder: &mut QueryBuilder<Postgres>, column: &str, value: Option<&str>) {
    builder.push(" AND ").push(column);
    match value {
        Some(v) => {
            builder.push(" = ").push_bind(v.to_owned());
        }
        None => {
            builder.push(" IS NULL");
        }
    }
}

impl PgComponentRepo {
    async fn find_row(&self, new: &NewComponent) -> Result<Option<Component>, StoreError> {
        let mut builder = QueryBuilder::<Postgres>::new("SELECT ");
        builder
            .push(COLUMNS)
            .push(" FROM components WHERE repository_id = ");
        builder.push_bind(new.repository_id.as_uuid());
        builder
            .push(" AND format = ")
            .push_bind(new.format.as_str());
        push_nullable_eq(&mut builder, "group_id", new.group_id.as_deref());
        builder.push(" AND name = ").push_bind(new.name.clone());
        push_nullable_eq(&mut builder, "version", new.version.as_deref());
        builder
            .build_query_as::<Row>()
            .fetch_optional(&self.pool)
            .await
            .map_err(|e| map_err("find component", e))?
            .map(Row::into_component)
            .transpose()
    }
}

#[async_trait]
impl ComponentRepo for PgComponentRepo {
    /// Read first, insert second: the common case (the component exists) is one indexed read
    /// and no write. `ON CONFLICT DO NOTHING` plus a re-read makes concurrent callers for one
    /// coordinate converge on the same row without the dead tuple a no-op `DO UPDATE` leaves
    /// on every hit.
    async fn find_or_create(&self, new: &NewComponent) -> Result<Component, StoreError> {
        for _ in 0..3 {
            if let Some(existing) = self.find_row(new).await? {
                return Ok(existing);
            }
            let inserted = sqlx::query_as::<_, Row>(
                "INSERT INTO components (id, repository_id, format, group_id, name, version, \
                 created_by) VALUES ($1, $2, $3, $4, $5, $6, $7) \
                 ON CONFLICT ON CONSTRAINT uq_components_coordinate DO NOTHING \
                 RETURNING id, repository_id, format, group_id, name, version, created_by, \
                 last_downloaded_at, created_at, updated_at",
            )
            .bind(ComponentId::generate().as_uuid())
            .bind(new.repository_id.as_uuid())
            .bind(new.format.as_str())
            .bind(&new.group_id)
            .bind(&new.name)
            .bind(&new.version)
            .bind(new.created_by.map(|id| id.as_uuid()))
            .fetch_optional(&self.pool)
            .await
            .map_err(|e| map_err("create component", e))?;
            if let Some(row) = inserted {
                return row.into_component();
            }
            // Lost the race: the winner's row is committed now, the next read sees it.
        }
        Err(StoreError::Unavailable(
            "component create kept losing a race, retry".to_owned(),
        ))
    }

    async fn get(&self, id: ComponentId) -> Result<Option<Component>, StoreError> {
        sqlx::query_as::<_, Row>(
            "SELECT id, repository_id, format, group_id, name, version, created_by, \
             last_downloaded_at, created_at, updated_at FROM components WHERE id = $1",
        )
        .bind(id.as_uuid())
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| map_err("read component", e))?
        .map(Row::into_component)
        .transpose()
    }

    async fn find(&self, new: &NewComponent) -> Result<Option<Component>, StoreError> {
        self.find_row(new).await
    }

    /// Ordered by id (`UUIDv7`, so by creation time) because that is the one order a keyset
    /// token can follow whatever filter is active; relevance ranking cannot be paged by
    /// keyset. `query` matches words of group, name and version (full text search over
    /// the generated `search` column) or a substring of the name (trigram index), so both
    /// `guava` and `guav` find `guava`.
    async fn list(
        &self,
        filter: &ComponentFilter,
        page: &PageRequest,
    ) -> Result<Page<Component>, StoreError> {
        let (limit, fetch) = cursor::fetch_limit(page);
        let mut builder = QueryBuilder::<Postgres>::new("SELECT ");
        builder.push(COLUMNS).push(" FROM components WHERE TRUE");
        if let Some(repository_id) = filter.repository_id {
            builder
                .push(" AND repository_id = ")
                .push_bind(repository_id.as_uuid());
        }
        if let Some(group_id) = &filter.group_id {
            builder.push(" AND group_id = ").push_bind(group_id.clone());
        }
        if let Some(name) = &filter.name {
            builder.push(" AND name = ").push_bind(name.clone());
        }
        if let Some(version) = &filter.version {
            builder.push(" AND version = ").push_bind(version.clone());
        }
        if let Some(query) = filter
            .query
            .as_deref()
            .map(str::trim)
            .filter(|q| !q.is_empty())
        {
            builder.push(" AND (search @@ websearch_to_tsquery('simple', regexp_replace(");
            builder.push_bind(query.to_owned());
            builder.push(", '[^[:alnum:]]+', ' ', 'g')) OR name ILIKE ");
            builder.push_bind(format!("%{}%", escape_like(query)));
            builder.push(" ESCAPE '\\')");
        }
        if let Some(after) = &page.after {
            let key = cursor::decode(KIND, after)?;
            let after_id = Uuid::parse_str(&key).map_err(|_| cursor::invalid_token())?;
            builder.push(" AND id > ").push_bind(after_id);
        }
        builder.push(" ORDER BY id LIMIT ").push_bind(fetch);
        let rows = builder
            .build_query_as::<Row>()
            .fetch_all(&self.pool)
            .await
            .map_err(|e| map_err("list components", e))?;
        let items = rows
            .into_iter()
            .map(Row::into_component)
            .collect::<Result<Vec<_>, _>>()?;
        Ok(cursor::finish(items, limit, KIND, |c| c.id.to_string()))
    }

    /// The phase 1 delete is `delete_checked` with no options, so a guard (immutable, held,
    /// read only) refuses it exactly as it refuses the typed call, as a `Conflict`.
    async fn delete(&self, id: ComponentId) -> Result<(), StoreError> {
        self.delete_checked(id, &WriteOptions::new())
            .await
            .map_err(MutationError::into_store_error)
    }

    /// Cascades to the component's assets (the guards on `assets` fire for each of them, so a
    /// protected asset keeps its component). `NotFound` when absent. The events ride in the
    /// same transaction: a refused delete writes none.
    async fn delete_checked(
        &self,
        id: ComponentId,
        options: &WriteOptions,
    ) -> Result<(), MutationError> {
        const CONTEXT: &str = "delete component";
        let mut tx = write::begin(&self.pool, options, CONTEXT).await?;
        let deleted = sqlx::query("DELETE FROM components WHERE id = $1")
            .bind(id.as_uuid())
            .execute(&mut *tx)
            .await
            .map_err(|e| map_mutation_err(CONTEXT, e))?;
        if deleted.rows_affected() == 0 {
            return Err(StoreError::NotFound("component".to_owned()).into());
        }
        write::commit(tx, options, CONTEXT).await
    }
}
