//! CVE exceptions. One vulnerability id per scope. An expired row stops counting at once.

use rustryx_core::error::StoreError;
use rustryx_core::ids::{ComponentId, CveAllowlistEntryId, ProjectId, UserId};
use rustryx_core::ports::{CveAllowlistRepo, InvalidationEvent, Page, PageRequest};
use rustryx_core::scan::{AllowlistMode, AllowlistScope, CveAllowlistEntry};
use sqlx::PgPool;
use time::OffsetDateTime;
use uuid::Uuid;

use super::common::notify;
use crate::error::{backend, map_err};

/// CVE exceptions.
#[derive(Clone)]
pub struct PgCveAllowlistRepo {
    pool: PgPool,
}

impl PgCveAllowlistRepo {
    /// A repo on `pool`.
    #[must_use]
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[derive(sqlx::FromRow)]
struct Row {
    id: Uuid,
    project_id: Option<Uuid>,
    component_id: Option<Uuid>,
    system: bool,
    vuln_id: String,
    reason: String,
    created_by: Option<Uuid>,
    created_at: OffsetDateTime,
    expires_at: Option<OffsetDateTime>,
    version: i64,
}

#[async_trait::async_trait]
impl CveAllowlistRepo for PgCveAllowlistRepo {
    async fn create(&self, entry: &CveAllowlistEntry) -> Result<(), StoreError> {
        entry
            .validate()
            .map_err(|error| StoreError::Conflict(error.to_string()))?;
        scope_exists(&self.pool, entry.scope).await?;
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| map_err("create cve exception", e))?;
        insert_entry(&mut tx, entry).await?;
        let revision = bump_revision(&mut tx).await?;
        notify(&mut tx, &InvalidationEvent::Config(revision)).await?;
        tx.commit()
            .await
            .map_err(|e| map_err("create cve exception", e))
    }

    async fn list(
        &self,
        scope: AllowlistScope,
        include_expired: bool,
        page: &PageRequest,
    ) -> Result<Page<CveAllowlistEntry>, StoreError> {
        let (system, project, component) = scope_columns(scope);
        let limit = page.limit.clamp(1, 100);
        let rows: Vec<Row> = sqlx::query_as(
            "SELECT id, project_id, component_id, system, vuln_id, reason, created_by, \
             created_at, expires_at, version FROM cve_allowlist \
             WHERE system = $1 AND project_id IS NOT DISTINCT FROM $2 \
               AND component_id IS NOT DISTINCT FROM $3 \
               AND ($4 OR expires_at IS NULL OR expires_at > now()) \
             ORDER BY vuln_id, id LIMIT $5",
        )
        .bind(system)
        .bind(project)
        .bind(component)
        .bind(include_expired)
        .bind(i64::from(limit))
        .fetch_all(&self.pool)
        .await
        .map_err(|e| map_err("list cve exceptions", e))?;
        let items = rows
            .into_iter()
            .map(decode)
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Page { items, next: None })
    }

    async fn update(
        &self,
        entry: &CveAllowlistEntry,
        expected_version: i64,
    ) -> Result<(), StoreError> {
        entry
            .validate()
            .map_err(|error| StoreError::Conflict(error.to_string()))?;
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| map_err("update cve exception", e))?;
        let current: Option<i64> =
            sqlx::query_scalar("SELECT version FROM cve_allowlist WHERE id = $1 FOR UPDATE")
                .bind(entry.id.as_uuid())
                .fetch_optional(&mut *tx)
                .await
                .map_err(|e| map_err("read cve exception", e))?;
        let Some(version) = current else {
            return Err(StoreError::NotFound("cve exception".to_owned()));
        };
        if version != expected_version {
            return Err(StoreError::Conflict(format!(
                "stale version: expected {expected_version}, found {version}"
            )));
        }
        sqlx::query(
            "UPDATE cve_allowlist SET vuln_id = $2, reason = $3, expires_at = $4, version = $5, \
             expiry_audited_at = CASE WHEN expires_at IS DISTINCT FROM $4 THEN NULL \
                                      ELSE expiry_audited_at END \
             WHERE id = $1",
        )
        .bind(entry.id.as_uuid())
        .bind(&entry.vuln_id)
        .bind(&entry.reason)
        .bind(entry.expires_at)
        .bind(expected_version + 1)
        .execute(&mut *tx)
        .await
        .map_err(|e| map_err("update cve exception", e))?;
        let revision = bump_revision(&mut tx).await?;
        notify(&mut tx, &InvalidationEvent::Config(revision)).await?;
        tx.commit()
            .await
            .map_err(|e| map_err("update cve exception", e))
    }

    async fn delete(&self, id: CveAllowlistEntryId) -> Result<(), StoreError> {
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| map_err("delete cve exception", e))?;
        let deleted = sqlx::query("DELETE FROM cve_allowlist WHERE id = $1")
            .bind(id.as_uuid())
            .execute(&mut *tx)
            .await
            .map_err(|e| map_err("delete cve exception", e))?;
        if deleted.rows_affected() == 0 {
            return Err(StoreError::NotFound("cve exception".to_owned()));
        }
        let revision = bump_revision(&mut tx).await?;
        notify(&mut tx, &InvalidationEvent::Config(revision)).await?;
        tx.commit()
            .await
            .map_err(|e| map_err("delete cve exception", e))
    }

    async fn copy_system_to_project(
        &self,
        project: ProjectId,
        by: Option<UserId>,
    ) -> Result<u64, StoreError> {
        let exists: bool =
            sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM projects WHERE id = $1)")
                .bind(project.as_uuid())
                .fetch_one(&self.pool)
                .await
                .map_err(|e| map_err("read project", e))?;
        if !exists {
            return Err(StoreError::NotFound("project".to_owned()));
        }
        let rows: Vec<(String, String, Option<OffsetDateTime>)> = sqlx::query_as(
            "SELECT vuln_id, reason, expires_at FROM cve_allowlist \
             WHERE system AND (expires_at IS NULL OR expires_at > now()) \
               AND NOT EXISTS (SELECT 1 FROM cve_allowlist own \
                               WHERE own.project_id = $1 AND own.vuln_id = cve_allowlist.vuln_id) \
             ORDER BY vuln_id",
        )
        .bind(project.as_uuid())
        .fetch_all(&self.pool)
        .await
        .map_err(|e| map_err("copy cve exceptions", e))?;
        if rows.is_empty() {
            return Ok(0);
        }
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| map_err("copy cve exceptions", e))?;
        let now = OffsetDateTime::now_utc();
        for (vuln_id, reason, expires_at) in &rows {
            let mut copy = CveAllowlistEntry::new(
                CveAllowlistEntryId::generate(),
                AllowlistScope::Project(project),
                vuln_id.clone(),
                reason.clone(),
                now,
            );
            copy.created_by = by;
            copy.expires_at = *expires_at;
            insert_entry(&mut tx, &copy).await?;
        }
        let revision = bump_revision(&mut tx).await?;
        notify(&mut tx, &InvalidationEvent::Config(revision)).await?;
        tx.commit()
            .await
            .map_err(|e| map_err("copy cve exceptions", e))?;
        Ok(u64::try_from(rows.len()).unwrap_or(u64::MAX))
    }

    async fn active_ids(
        &self,
        component: ComponentId,
        mode: AllowlistMode,
    ) -> Result<Vec<String>, StoreError> {
        let project: Option<Option<Uuid>> = sqlx::query_scalar(
            "SELECT r.project_id FROM components c \
             JOIN repositories r ON r.id = c.repository_id WHERE c.id = $1",
        )
        .bind(component.as_uuid())
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| map_err("read component", e))?;
        let Some(project) = project else {
            return Ok(Vec::new());
        };
        let wider_project = matches!(mode, AllowlistMode::Project);
        let ids: Vec<String> = sqlx::query_scalar(
            "SELECT DISTINCT vuln_id FROM cve_allowlist \
             WHERE (expires_at IS NULL OR expires_at > now()) \
               AND (component_id = $1 \
                    OR ($2 AND $3::uuid IS NOT NULL AND project_id = $3) \
                    OR (NOT $2 AND system)) \
             ORDER BY vuln_id",
        )
        .bind(component.as_uuid())
        .bind(wider_project)
        .bind(project)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| map_err("read cve exceptions", e))?;
        Ok(ids)
    }

    async fn take_expired_unaudited(
        &self,
        now: OffsetDateTime,
        limit: u32,
    ) -> Result<Vec<CveAllowlistEntry>, StoreError> {
        let rows: Vec<Row> = sqlx::query_as(
            "WITH due AS ( \
                SELECT id FROM cve_allowlist \
                WHERE expires_at IS NOT NULL AND expires_at <= $1 AND expiry_audited_at IS NULL \
                ORDER BY expires_at, id LIMIT $2 FOR UPDATE SKIP LOCKED) \
             UPDATE cve_allowlist SET expiry_audited_at = $1 \
             WHERE id IN (SELECT id FROM due) \
             RETURNING id, project_id, component_id, system, vuln_id, reason, created_by, \
                       created_at, expires_at, version",
        )
        .bind(now)
        .bind(i64::from(limit))
        .fetch_all(&self.pool)
        .await
        .map_err(|e| map_err("audit expired cve exceptions", e))?;
        rows.into_iter().map(decode).collect()
    }
}

async fn insert_entry(
    tx: &mut sqlx::PgConnection,
    entry: &CveAllowlistEntry,
) -> Result<(), StoreError> {
    let (system, project, component) = scope_columns(entry.scope);
    sqlx::query(
        "INSERT INTO cve_allowlist (id, project_id, component_id, system, vuln_id, reason, \
         created_by, created_at, expires_at, version) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10)",
    )
    .bind(entry.id.as_uuid())
    .bind(project)
    .bind(component)
    .bind(system)
    .bind(&entry.vuln_id)
    .bind(&entry.reason)
    .bind(entry.created_by.map(|id| id.as_uuid()))
    .bind(entry.created_at)
    .bind(entry.expires_at)
    .bind(entry.version)
    .execute(&mut *tx)
    .await
    .map_err(|e| map_err("create cve exception", e))?;
    Ok(())
}

async fn scope_exists(pool: &PgPool, scope: AllowlistScope) -> Result<(), StoreError> {
    let exists = match scope {
        AllowlistScope::System => return Ok(()),
        AllowlistScope::Project(id) => {
            sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM projects WHERE id = $1)")
                .bind(id.as_uuid())
                .fetch_one(pool)
                .await
        }
        AllowlistScope::Component(id) => {
            sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM components WHERE id = $1)")
                .bind(id.as_uuid())
                .fetch_one(pool)
                .await
        }
    }
    .map_err(|e| map_err("read cve exception scope", e))?;
    if exists {
        Ok(())
    } else {
        Err(StoreError::Conflict(
            "the scope of the exception does not exist".to_owned(),
        ))
    }
}

fn scope_columns(scope: AllowlistScope) -> (bool, Option<Uuid>, Option<Uuid>) {
    match scope {
        AllowlistScope::System => (true, None, None),
        AllowlistScope::Project(id) => (false, Some(id.as_uuid()), None),
        AllowlistScope::Component(id) => (false, None, Some(id.as_uuid())),
    }
}

fn decode(row: Row) -> Result<CveAllowlistEntry, StoreError> {
    let scope = match (row.system, row.project_id, row.component_id) {
        (true, None, None) => AllowlistScope::System,
        (false, Some(id), None) => AllowlistScope::Project(ProjectId::from_uuid(id)),
        (false, None, Some(id)) => AllowlistScope::Component(ComponentId::from_uuid(id)),
        _ => return Err(backend("read cve exception", "scope is ambiguous")),
    };
    let mut entry = CveAllowlistEntry::new(
        CveAllowlistEntryId::from_uuid(row.id),
        scope,
        row.vuln_id,
        row.reason,
        row.created_at,
    );
    entry.created_by = row.created_by.map(UserId::from_uuid);
    entry.expires_at = row.expires_at;
    entry.version = row.version;
    Ok(entry)
}

async fn bump_revision(tx: &mut sqlx::PgConnection) -> Result<i64, StoreError> {
    sqlx::query_scalar(
        "INSERT INTO system_config (key, value) VALUES ('security.revision', '0'::jsonb) \
         ON CONFLICT (key) DO UPDATE SET value = to_jsonb( \
             COALESCE((system_config.value #>> '{}')::bigint, 0) + 1) \
         RETURNING revision",
    )
    .fetch_one(&mut *tx)
    .await
    .map_err(|e| map_err("bump security revision", e))
}
