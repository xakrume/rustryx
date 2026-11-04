//! [`RetentionPolicyRepo`] on PostgreSQL.

use async_trait::async_trait;
use rustryx_core::StoreError;
use rustryx_core::ids::RetentionPolicyId;
use rustryx_core::lifecycle::{PolicyScope, RetentionPolicy, RetentionRule};
use rustryx_core::ports::RetentionPolicyRepo;
use sqlx::types::Json;
use sqlx::{FromRow, PgPool};
use time::OffsetDateTime;
use uuid::Uuid;

use super::common::corrupt;
use crate::error::map_err;

/// Retention policies.
#[derive(Debug, Clone)]
pub struct PgRetentionPolicyRepo {
    pool: PgPool,
}

impl PgRetentionPolicyRepo {
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
    repository_id: Option<Uuid>,
    name: String,
    rules: Json<Vec<RetentionRule>>,
    allow_delete_last_version: bool,
    version: i64,
    created_at: OffsetDateTime,
    updated_at: OffsetDateTime,
}

struct ScopeIds {
    project_id: Option<Uuid>,
    repository_id: Option<Uuid>,
}

fn scope_ids(scope: PolicyScope) -> Result<ScopeIds, StoreError> {
    match scope {
        PolicyScope::Project(id) => Ok(ScopeIds {
            project_id: Some(id.as_uuid()),
            repository_id: None,
        }),
        PolicyScope::Repository(id) => Ok(ScopeIds {
            project_id: None,
            repository_id: Some(id.as_uuid()),
        }),
        PolicyScope::System => Err(StoreError::Conflict(
            "a retention policy belongs to a project or a repository".to_owned(),
        )),
    }
}

impl Row {
    fn into_policy(self) -> Result<RetentionPolicy, StoreError> {
        let scope = match (self.project_id, self.repository_id) {
            (Some(id), None) => PolicyScope::Project(id.into()),
            (None, Some(id)) => PolicyScope::Repository(id.into()),
            _ => return Err(corrupt("read retention policy")),
        };
        let mut policy = RetentionPolicy::new(
            RetentionPolicyId::from_uuid(self.id),
            scope,
            self.name,
            self.rules.0,
            self.created_at,
        );
        policy.allow_delete_last_version = self.allow_delete_last_version;
        policy.version = self.version;
        policy.updated_at = self.updated_at;
        Ok(policy)
    }
}

#[async_trait]
impl RetentionPolicyRepo for PgRetentionPolicyRepo {
    async fn upsert(
        &self,
        policy: &RetentionPolicy,
        expected_version: Option<i64>,
    ) -> Result<(), StoreError> {
        policy
            .validate()
            .map_err(|error| StoreError::Conflict(error.to_string()))?;
        let scope = scope_ids(policy.scope)?;
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| map_err("save retention policy", e))?;
        let existing: Option<(Uuid, i64, OffsetDateTime)> = sqlx::query_as(
            "SELECT id, version, created_at FROM retention_policies \
             WHERE project_id IS NOT DISTINCT FROM $1 AND repository_id IS NOT DISTINCT FROM $2",
        )
        .bind(scope.project_id)
        .bind(scope.repository_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(|e| map_err("save retention policy", e))?;
        let rules = Json(&policy.rules);
        match (existing, expected_version) {
            (Some((id, version, created_at)), Some(expected)) => {
                if version != expected {
                    return Err(StoreError::Conflict(
                        "retention policy was changed by someone else".to_owned(),
                    ));
                }
                sqlx::query(
                    "UPDATE retention_policies SET name = $2, rules = $3, \
                     allow_delete_last_version = $4, version = version + 1, updated_at = now() \
                     WHERE id = $1",
                )
                .bind(id)
                .bind(&policy.name)
                .bind(rules)
                .bind(policy.allow_delete_last_version)
                .execute(&mut *tx)
                .await
                .map_err(|e| map_err("save retention policy", e))?;
                let _ = created_at;
            }
            (Some(_), None) => {
                return Err(StoreError::Conflict(
                    "the scope already has a retention policy".to_owned(),
                ));
            }
            (None, Some(_)) => {
                return Err(StoreError::Conflict(
                    "stale version: the scope has no retention policy".to_owned(),
                ));
            }
            (None, None) => {
                sqlx::query(
                    "INSERT INTO retention_policies (id, project_id, repository_id, name, rules, \
                     allow_delete_last_version, version, created_at, updated_at) \
                     VALUES ($1, $2, $3, $4, $5, $6, 1, $7, $8)",
                )
                .bind(policy.id.as_uuid())
                .bind(scope.project_id)
                .bind(scope.repository_id)
                .bind(&policy.name)
                .bind(rules)
                .bind(policy.allow_delete_last_version)
                .bind(policy.created_at)
                .bind(policy.updated_at)
                .execute(&mut *tx)
                .await
                .map_err(|e| map_err("save retention policy", e))?;
            }
        }
        tx.commit()
            .await
            .map_err(|e| map_err("save retention policy", e))
    }

    async fn get(&self, id: RetentionPolicyId) -> Result<Option<RetentionPolicy>, StoreError> {
        sqlx::query_as::<_, Row>(
            "SELECT id, project_id, repository_id, name, rules, allow_delete_last_version, \
             version, created_at, updated_at FROM retention_policies WHERE id = $1",
        )
        .bind(id.as_uuid())
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| map_err("read retention policy", e))?
        .map(Row::into_policy)
        .transpose()
    }

    async fn get_for_scope(
        &self,
        scope: PolicyScope,
    ) -> Result<Option<RetentionPolicy>, StoreError> {
        let scope = scope_ids(scope)?;
        sqlx::query_as::<_, Row>(
            "SELECT id, project_id, repository_id, name, rules, allow_delete_last_version, \
             version, created_at, updated_at FROM retention_policies \
             WHERE project_id IS NOT DISTINCT FROM $1 AND repository_id IS NOT DISTINCT FROM $2",
        )
        .bind(scope.project_id)
        .bind(scope.repository_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| map_err("read retention policy", e))?
        .map(Row::into_policy)
        .transpose()
    }

    async fn delete(&self, id: RetentionPolicyId) -> Result<(), StoreError> {
        let result = sqlx::query("DELETE FROM retention_policies WHERE id = $1")
            .bind(id.as_uuid())
            .execute(&self.pool)
            .await
            .map_err(|e| map_err("delete retention policy", e))?;
        if result.rows_affected() == 0 {
            return Err(StoreError::NotFound("retention policy".to_owned()));
        }
        Ok(())
    }
}
