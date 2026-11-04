//! [`QuotaRepo`] on PostgreSQL.

use async_trait::async_trait;
use rustryx_core::StoreError;
use rustryx_core::audit::ActorRef;
use rustryx_core::ids::RepositoryId;
use rustryx_core::ports::{Page, PageRequest, QuotaRepo};
use rustryx_core::quota::{Headroom, QuotaScope, QuotaUsage, validate_warn_percent};
use sqlx::{FromRow, PgPool};
use time::OffsetDateTime;
use uuid::Uuid;

use super::common::corrupt;
use crate::cursor;
use crate::error::map_err;

const PAGE: &str = "quota";

/// Project and repository quotas.
#[derive(Debug, Clone)]
pub struct PgQuotaRepo {
    pool: PgPool,
}

impl PgQuotaRepo {
    /// Creates the repository over `pool`.
    #[must_use]
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[derive(Debug, FromRow)]
struct UsageRow {
    scope_kind: String,
    scope_id: Uuid,
    used_bytes: i64,
    object_count: i64,
    limit_bytes: Option<i64>,
    warn_percent: i16,
    recounted_at: Option<OffsetDateTime>,
}

fn kind_of(scope: QuotaScope) -> (&'static str, Uuid) {
    match scope {
        QuotaScope::Project(id) => ("project", id.as_uuid()),
        QuotaScope::Repository(id) => ("repository", id.as_uuid()),
    }
}

fn scope_of(kind: &str, id: Uuid) -> Result<QuotaScope, StoreError> {
    match kind {
        "project" => Ok(QuotaScope::Project(id.into())),
        "repository" => Ok(QuotaScope::Repository(id.into())),
        _ => Err(corrupt("read quota")),
    }
}

fn as_u64(value: i64) -> Result<u64, StoreError> {
    u64::try_from(value).map_err(|_| corrupt("read quota"))
}

impl UsageRow {
    fn into_usage(self) -> Result<QuotaUsage, StoreError> {
        let mut usage = QuotaUsage::new(scope_of(&self.scope_kind, self.scope_id)?);
        usage.used_bytes = as_u64(self.used_bytes)?;
        usage.object_count = as_u64(self.object_count)?;
        usage.limit_bytes = self.limit_bytes.map(as_u64).transpose()?;
        usage.warn_percent = u8::try_from(self.warn_percent).map_err(|_| corrupt("read quota"))?;
        usage.recounted_at = self.recounted_at;
        Ok(usage)
    }
}

#[async_trait]
impl QuotaRepo for PgQuotaRepo {
    async fn get(&self, scope: QuotaScope) -> Result<Option<QuotaUsage>, StoreError> {
        let (kind, id) = kind_of(scope);
        sqlx::query_as::<_, UsageRow>(
            "SELECT scope_kind, scope_id, used_bytes, object_count, limit_bytes, warn_percent, \
             recounted_at FROM quota_usage WHERE scope_kind = $1 AND scope_id = $2",
        )
        .bind(kind)
        .bind(id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| map_err("read quota", e))?
        .map(UsageRow::into_usage)
        .transpose()
    }

    async fn set_limit(
        &self,
        scope: QuotaScope,
        limit_bytes: Option<u64>,
        warn_percent: u8,
        expected_version: Option<i64>,
        actor: &ActorRef,
    ) -> Result<(), StoreError> {
        validate_warn_percent(warn_percent)
            .map_err(|error| StoreError::Conflict(error.to_string()))?;
        let limit = limit_bytes.map(i64::try_from).transpose().map_err(|_| {
            StoreError::Conflict("limit_bytes does not fit a signed counter".to_owned())
        })?;
        let (kind, id) = kind_of(scope);
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| map_err("set quota", e))?;
        let current: Option<i64> = sqlx::query_scalar(
            "SELECT version FROM quota_usage WHERE scope_kind = $1 AND scope_id = $2",
        )
        .bind(kind)
        .bind(id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(|e| map_err("set quota", e))?;
        match (current, expected_version) {
            (Some(version), Some(expected)) if version != expected => {
                return Err(StoreError::Conflict(
                    "quota was changed by someone else".to_owned(),
                ));
            }
            (Some(_), None) => {
                return Err(StoreError::Conflict(
                    "the scope already has a quota".to_owned(),
                ));
            }
            (None, Some(_)) => {
                return Err(StoreError::Conflict(
                    "stale version: the scope has no quota".to_owned(),
                ));
            }
            _ => {}
        }
        if let QuotaScope::Repository(repo) = scope {
            let updated = sqlx::query("UPDATE repositories SET quota_bytes = $2 WHERE id = $1")
                .bind(repo.as_uuid())
                .bind(limit)
                .execute(&mut *tx)
                .await
                .map_err(|e| map_err("set quota", e))?;
            if updated.rows_affected() == 0 {
                return Err(StoreError::NotFound("repository".to_owned()));
            }
        } else {
            let exists: bool =
                sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM projects WHERE id = $1)")
                    .bind(id)
                    .fetch_one(&mut *tx)
                    .await
                    .map_err(|e| map_err("set quota", e))?;
            if !exists {
                return Err(StoreError::NotFound("project".to_owned()));
            }
            sqlx::query(
                "INSERT INTO quotas (project_id, limit_bytes, warn_percent, version, updated_by) \
                 VALUES ($1, $2, $3, 1, $4) \
                 ON CONFLICT (project_id) DO UPDATE SET limit_bytes = EXCLUDED.limit_bytes, \
                 warn_percent = EXCLUDED.warn_percent, version = quotas.version + 1, \
                 updated_by = EXCLUDED.updated_by, updated_at = now()",
            )
            .bind(id)
            .bind(limit)
            .bind(i16::from(warn_percent))
            .bind(actor.id.map(|user| user.as_uuid()))
            .execute(&mut *tx)
            .await
            .map_err(|e| map_err("set quota", e))?;
        }
        sqlx::query(
            "INSERT INTO quota_usage (scope_kind, scope_id, limit_bytes, warn_percent, version) \
             VALUES ($1, $2, $3, $4, 1) \
             ON CONFLICT (scope_kind, scope_id) DO UPDATE SET limit_bytes = EXCLUDED.limit_bytes, \
             warn_percent = EXCLUDED.warn_percent, version = quota_usage.version + 1",
        )
        .bind(kind)
        .bind(id)
        .bind(limit)
        .bind(i16::from(warn_percent))
        .execute(&mut *tx)
        .await
        .map_err(|e| map_err("set quota", e))?;
        tx.commit().await.map_err(|e| map_err("set quota", e))
    }

    async fn headroom(&self, repository: RepositoryId) -> Result<Headroom, StoreError> {
        let row: Option<(Option<Uuid>, Option<i64>, Option<i64>, Option<i64>, Option<i64>)> =
            sqlx::query_as(
                "SELECT r.project_id, r.quota_bytes, repo_usage.used_bytes, \
                        project_usage.limit_bytes, project_usage.used_bytes \
                   FROM repositories r \
                   LEFT JOIN quota_usage repo_usage \
                     ON repo_usage.scope_kind = 'repository' AND repo_usage.scope_id = r.id \
                   LEFT JOIN quota_usage project_usage \
                     ON project_usage.scope_kind = 'project' AND project_usage.scope_id = r.project_id \
                  WHERE r.id = $1",
            )
            .bind(repository.as_uuid())
            .fetch_optional(&self.pool)
            .await
            .map_err(|e| map_err("read quota headroom", e))?;
        let Some((project_id, repo_limit, repo_used, project_limit, project_used)) = row else {
            return Err(StoreError::NotFound("repository".to_owned()));
        };
        let mut best: Option<(u64, QuotaScope)> = None;
        for (limit, used, scope) in [
            (
                repo_limit,
                repo_used.unwrap_or(0),
                QuotaScope::Repository(repository),
            ),
            (
                project_limit,
                project_used.unwrap_or(0),
                project_id
                    .map(|id| QuotaScope::Project(id.into()))
                    .unwrap_or(QuotaScope::Repository(repository)),
            ),
        ] {
            let Some(limit) = limit else {
                continue;
            };
            if project_id.is_none() && matches!(scope, QuotaScope::Project(_)) {
                continue;
            }
            let limit = as_u64(limit)?;
            let used = as_u64(used)?;
            let room = limit.saturating_sub(used);
            if best.is_none_or(|(bytes, _)| room < bytes) {
                best = Some((room, scope));
            }
        }
        Ok(match best {
            Some((bytes, scope)) => Headroom {
                bytes: Some(bytes),
                binding: Some(scope),
            },
            None => Headroom {
                bytes: None,
                binding: None,
            },
        })
    }

    async fn recount(&self, scope: QuotaScope) -> Result<(u64, u64), StoreError> {
        let (kind, id) = kind_of(scope);
        let before: i64 = sqlx::query_scalar(
            "SELECT COALESCE((SELECT used_bytes FROM quota_usage \
              WHERE scope_kind = $1 AND scope_id = $2), 0)",
        )
        .bind(kind)
        .bind(id)
        .fetch_one(&self.pool)
        .await
        .map_err(|e| map_err("recount quota", e))?;
        let counted: Option<(i64, i64)> = sqlx::query_as(
            "SELECT COALESCE(SUM(size_bytes), 0)::bigint, COALESCE(SUM(object_count), 0)::bigint FROM ( \
               SELECT b.size_bytes, COUNT(a.id) AS object_count \
                 FROM assets a \
                 JOIN blobs b ON b.id = a.blob_id \
                 JOIN repositories r ON r.id = a.repository_id \
                WHERE ($1 = 'repository' AND r.id = $2) \
                   OR ($1 = 'project' AND r.project_id = $2) \
                GROUP BY b.id, b.size_bytes) AS distinct_blobs",
        )
        .bind(kind)
        .bind(id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| map_err("recount quota", e))?;
        let (used, objects) = counted.unwrap_or((0, 0));
        sqlx::query(
            "INSERT INTO quota_usage (scope_kind, scope_id, used_bytes, object_count, recounted_at) \
             VALUES ($1, $2, $3, $4, now()) \
             ON CONFLICT (scope_kind, scope_id) DO UPDATE \
                SET used_bytes = EXCLUDED.used_bytes, object_count = EXCLUDED.object_count, \
                    recounted_at = now()",
        )
        .bind(kind)
        .bind(id)
        .bind(used)
        .bind(objects)
        .execute(&self.pool)
        .await
        .map_err(|e| map_err("recount quota", e))?;
        Ok((as_u64(before)?, as_u64(used)?))
    }

    async fn list(&self, page: &PageRequest) -> Result<Page<QuotaUsage>, StoreError> {
        let (limit, fetch) = cursor::fetch_limit(page);
        let after = match &page.after {
            Some(token) => Some(cursor::decode(PAGE, token)?),
            None => None,
        };
        let (after_kind, after_id) = match after.as_deref().and_then(|key| key.split_once('/')) {
            Some((kind, id)) => (
                Some(kind.to_owned()),
                Some(Uuid::parse_str(id).map_err(|_| cursor::invalid_token())?),
            ),
            None if after.is_none() => (None, None),
            _ => return Err(cursor::invalid_token()),
        };
        let rows = sqlx::query_as::<_, UsageRow>(
            "SELECT scope_kind, scope_id, used_bytes, object_count, limit_bytes, warn_percent, \
             recounted_at FROM quota_usage \
             WHERE $1::text IS NULL OR (scope_kind, scope_id) > ($1, $2) \
             ORDER BY scope_kind, scope_id LIMIT $3",
        )
        .bind(after_kind)
        .bind(after_id)
        .bind(fetch)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| map_err("list quotas", e))?;
        let items = rows
            .into_iter()
            .map(UsageRow::into_usage)
            .collect::<Result<Vec<_>, _>>()?;
        Ok(cursor::finish(items, limit, PAGE, |usage| {
            let (kind, id) = kind_of(usage.scope);
            format!("{kind}/{id}")
        }))
    }
}
