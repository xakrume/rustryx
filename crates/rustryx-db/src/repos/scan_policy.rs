//! Scan policies. One row per scope. A null field inherits from the wider scope.

use rustryx_core::error::StoreError;
use rustryx_core::ids::{ProjectId, RepositoryId, ScannerId, SecurityPolicyId};
use rustryx_core::lifecycle::PolicyScope;
use rustryx_core::ports::{InvalidationEvent, SecurityPolicyRepo};
use rustryx_core::scan::{
    AllowlistMode, PullGatePolicy, ScanPolicy, TrustPolicy,
};
use sqlx::PgPool;
use uuid::Uuid;

use super::common::notify;
use crate::error::{backend, map_err};

/// Scan policies. Trust policies are not stored yet.
#[derive(Clone)]
pub struct PgScanPolicyRepo {
    pool: PgPool,
}

impl PgScanPolicyRepo {
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
    repository_id: Option<Uuid>,
    system: bool,
    auto_scan: Option<bool>,
    scanner_id: Option<Uuid>,
    gate: Option<serde_json::Value>,
    quarantine: Option<bool>,
    allowlist_mode: Option<String>,
    health_check: Option<bool>,
    sbom_on_push: Option<bool>,
    version: i64,
}

#[async_trait::async_trait]
impl SecurityPolicyRepo for PgScanPolicyRepo {
    async fn upsert_scan_policy(
        &self,
        policy: &ScanPolicy,
        expected_version: Option<i64>,
    ) -> Result<(), StoreError> {
        policy
            .validate()
            .map_err(|error| StoreError::Conflict(error.to_string()))?;
        if matches!(policy.scope, PolicyScope::System) && !policy.is_complete() {
            return Err(StoreError::Conflict(
                "the system scan policy must set every field".to_owned(),
            ));
        }
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| map_err("upsert scan policy", e))?;
        let existing = lock_scope(&mut tx, policy.scope).await?;
        let (id, version) = match (existing, expected_version) {
            (Some((id, current)), Some(expected)) => {
                if current != expected {
                    return Err(StoreError::Conflict(format!(
                        "stale version: expected {expected}, found {current}"
                    )));
                }
                (id, current.saturating_add(1))
            }
            (Some(_), None) => {
                return Err(StoreError::Conflict(
                    "the scope already has a policy".to_owned(),
                ));
            }
            (None, Some(_)) => {
                return Err(StoreError::Conflict(
                    "stale version: the scope has no policy".to_owned(),
                ));
            }
            (None, None) => (policy.id.as_uuid(), policy.version),
        };
        let (system, project, repository) = scope_columns(policy.scope);
        let gate = policy
            .gate
            .as_ref()
            .map(serde_json::to_value)
            .transpose()
            .map_err(|_| backend("upsert scan policy", "gate does not serialize"))?;
        let mode = policy.allowlist_mode.map(mode_label);
        if existing.is_some() {
            sqlx::query(
                "UPDATE scan_policies SET auto_scan = $2, scanner_id = $3, gate = $4, \
                 quarantine = $5, allowlist_mode = $6, health_check = $7, sbom_on_push = $8, \
                 version = $9 WHERE id = $1",
            )
            .bind(id)
            .bind(policy.auto_scan)
            .bind(policy.scanner_id.map(|scanner| scanner.as_uuid()))
            .bind(gate)
            .bind(policy.quarantine)
            .bind(mode)
            .bind(policy.health_check)
            .bind(policy.sbom_on_push)
            .bind(version)
            .execute(&mut *tx)
            .await
            .map_err(|e| map_err("upsert scan policy", e))?;
        } else {
            sqlx::query(
                "INSERT INTO scan_policies (id, project_id, repository_id, system, auto_scan, \
                 scanner_id, gate, quarantine, allowlist_mode, health_check, sbom_on_push, \
                 version) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12)",
            )
            .bind(id)
            .bind(project)
            .bind(repository)
            .bind(system)
            .bind(policy.auto_scan)
            .bind(policy.scanner_id.map(|scanner| scanner.as_uuid()))
            .bind(gate)
            .bind(policy.quarantine)
            .bind(mode)
            .bind(policy.health_check)
            .bind(policy.sbom_on_push)
            .bind(version)
            .execute(&mut *tx)
            .await
            .map_err(|e| map_err("upsert scan policy", e))?;
        }
        let revision = bump_revision(&mut tx).await?;
        notify(&mut tx, &InvalidationEvent::Config(revision)).await?;
        tx.commit()
            .await
            .map_err(|e| map_err("upsert scan policy", e))?;
        Ok(())
    }

    async fn get_scan_policy(&self, scope: PolicyScope) -> Result<Option<ScanPolicy>, StoreError> {
        let row = fetch_scope(&self.pool, scope).await?;
        row.map(decode).transpose()
    }

    async fn effective_for(&self, repository: RepositoryId) -> Result<ScanPolicy, StoreError> {
        let project: Option<Uuid> =
            sqlx::query_scalar("SELECT project_id FROM repositories WHERE id = $1")
                .bind(repository.as_uuid())
                .fetch_optional(&self.pool)
                .await
                .map_err(|e| map_err("read repository", e))?
                .ok_or_else(|| StoreError::NotFound(format!("repository {repository} not found")))?;
        let rows: Vec<Row> = sqlx::query_as(
            "SELECT * FROM scan_policies WHERE system OR project_id = $1 OR repository_id = $2",
        )
        .bind(project)
        .bind(repository.as_uuid())
        .fetch_all(&self.pool)
        .await
        .map_err(|e| map_err("read scan policies", e))?;
        let mut policies = Vec::with_capacity(rows.len());
        for row in rows {
            policies.push(decode(row)?);
        }
        let system = policies
            .iter()
            .find(|policy| policy.scope == PolicyScope::System)
            .cloned()
            .unwrap_or_else(|| ScanPolicy::system_default(SecurityPolicyId::generate()));
        let mut result = system;
        if let Some(project_id) = project.map(ProjectId::from_uuid) {
            if let Some(project_policy) = policies
                .iter()
                .find(|policy| policy.scope == PolicyScope::Project(project_id))
            {
                result = merged(result, project_policy);
            }
        }
        if let Some(own) = policies
            .iter()
            .find(|policy| policy.scope == PolicyScope::Repository(repository))
        {
            result = merged(result, own);
        }
        result.scope = PolicyScope::Repository(repository);
        Ok(result)
    }

    async fn upsert_trust_policy(
        &self,
        _policy: &TrustPolicy,
        _expected_version: Option<i64>,
    ) -> Result<(), StoreError> {
        Err(StoreError::Invalid(
            "trust policies are not stored yet".to_owned(),
        ))
    }

    async fn get_trust_policy(
        &self,
        _scope: PolicyScope,
    ) -> Result<Option<TrustPolicy>, StoreError> {
        Err(StoreError::Invalid(
            "trust policies are not stored yet".to_owned(),
        ))
    }

    async fn effective_trust_for(
        &self,
        _repository: RepositoryId,
    ) -> Result<Option<TrustPolicy>, StoreError> {
        Err(StoreError::Invalid(
            "trust policies are not stored yet".to_owned(),
        ))
    }
}

fn merged(base: ScanPolicy, over: &ScanPolicy) -> ScanPolicy {
    let mut merged = base;
    merged.auto_scan = over.auto_scan.or(merged.auto_scan);
    merged.scanner_id = over.scanner_id.or(merged.scanner_id);
    merged.gate = over.gate.or(merged.gate);
    merged.quarantine = over.quarantine.or(merged.quarantine);
    merged.allowlist_mode = over.allowlist_mode.or(merged.allowlist_mode);
    merged.health_check = over.health_check.or(merged.health_check);
    merged.sbom_on_push = over.sbom_on_push.or(merged.sbom_on_push);
    merged.id = over.id;
    merged.version = over.version;
    merged
}

fn scope_columns(scope: PolicyScope) -> (bool, Option<Uuid>, Option<Uuid>) {
    match scope {
        PolicyScope::System => (true, None, None),
        PolicyScope::Project(id) => (false, Some(id.as_uuid()), None),
        PolicyScope::Repository(id) => (false, None, Some(id.as_uuid())),
    }
}

fn mode_label(mode: AllowlistMode) -> &'static str {
    match mode {
        AllowlistMode::System => "system",
        AllowlistMode::Project => "project",
    }
}

async fn lock_scope(
    tx: &mut sqlx::PgConnection,
    scope: PolicyScope,
) -> Result<Option<(Uuid, i64)>, StoreError> {
    let row: Option<(Uuid, i64)> = match scope {
        PolicyScope::System => {
            sqlx::query_as("SELECT id, version FROM scan_policies WHERE system FOR UPDATE")
                .fetch_optional(&mut *tx)
                .await
        }
        PolicyScope::Project(id) => {
            sqlx::query_as(
                "SELECT id, version FROM scan_policies WHERE project_id = $1 FOR UPDATE",
            )
            .bind(id.as_uuid())
            .fetch_optional(&mut *tx)
            .await
        }
        PolicyScope::Repository(id) => {
            sqlx::query_as(
                "SELECT id, version FROM scan_policies WHERE repository_id = $1 FOR UPDATE",
            )
            .bind(id.as_uuid())
            .fetch_optional(&mut *tx)
            .await
        }
    }
    .map_err(|e| map_err("lock scan policy", e))?;
    Ok(row)
}

async fn fetch_scope(pool: &PgPool, scope: PolicyScope) -> Result<Option<Row>, StoreError> {
    let row = match scope {
        PolicyScope::System => {
            sqlx::query_as("SELECT * FROM scan_policies WHERE system")
                .fetch_optional(pool)
                .await
        }
        PolicyScope::Project(id) => {
            sqlx::query_as("SELECT * FROM scan_policies WHERE project_id = $1")
                .bind(id.as_uuid())
                .fetch_optional(pool)
                .await
        }
        PolicyScope::Repository(id) => {
            sqlx::query_as("SELECT * FROM scan_policies WHERE repository_id = $1")
                .bind(id.as_uuid())
                .fetch_optional(pool)
                .await
        }
    }
    .map_err(|e| map_err("read scan policy", e))?;
    Ok(row)
}

async fn bump_revision(tx: &mut sqlx::PgConnection) -> Result<i64, StoreError> {
    let revision: i64 = sqlx::query_scalar(
        "INSERT INTO system_config (key, value) VALUES ('security.revision', '0'::jsonb) \
         ON CONFLICT (key) DO UPDATE SET value = to_jsonb( \
             COALESCE((system_config.value #>> '{}')::bigint, 0) + 1) \
         RETURNING revision",
    )
    .fetch_one(&mut *tx)
    .await
    .map_err(|e| map_err("bump security revision", e))?;
    Ok(revision)
}

fn decode(row: Row) -> Result<ScanPolicy, StoreError> {
    let scope = match (row.system, row.project_id, row.repository_id) {
        (true, None, None) => PolicyScope::System,
        (false, Some(id), None) => PolicyScope::Project(ProjectId::from_uuid(id)),
        (false, None, Some(id)) => PolicyScope::Repository(RepositoryId::from_uuid(id)),
        _ => return Err(backend("read scan policy", "scan policy scope is ambiguous")),
    };
    let gate = row
        .gate
        .map(serde_json::from_value::<PullGatePolicy>)
        .transpose()
        .map_err(|_| backend("read scan policy", "stored gate is not a pull gate"))?;
    let allowlist_mode = match row.allowlist_mode.as_deref() {
        None => None,
        Some("system") => Some(AllowlistMode::System),
        Some("project") => Some(AllowlistMode::Project),
        Some(_) => {
            return Err(backend(
                "read scan policy",
                "stored allowlist mode is unknown",
            ));
        }
    };
    let mut policy = ScanPolicy::new(SecurityPolicyId::from_uuid(row.id), scope);
    policy.auto_scan = row.auto_scan;
    policy.scanner_id = row.scanner_id.map(ScannerId::from_uuid);
    policy.gate = gate;
    policy.quarantine = row.quarantine;
    policy.allowlist_mode = allowlist_mode;
    policy.health_check = row.health_check;
    policy.sbom_on_push = row.sbom_on_push;
    policy.version = row.version;
    Ok(policy)
}
