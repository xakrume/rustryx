//! [`CleanupPolicyRepo`] on PostgreSQL.

use std::str::FromStr;

use async_trait::async_trait;
use rustryx_core::StoreError;
use rustryx_core::ids::{CleanupPolicyId, RepositoryId};
use rustryx_core::lifecycle::{CleanupCriteria, CleanupPolicy};
use rustryx_core::model::RepoFormat;
use rustryx_core::ports::CleanupPolicyRepo;
use sqlx::types::Json;
use sqlx::{FromRow, PgPool};
use time::OffsetDateTime;
use uuid::Uuid;

use super::common::corrupt;
use crate::error::map_err;

/// Cleanup policies.
#[derive(Debug, Clone)]
pub struct PgCleanupPolicyRepo {
    pool: PgPool,
}

impl PgCleanupPolicyRepo {
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
    format: Option<String>,
    criteria: Json<CleanupCriteria>,
    version: i64,
    created_at: OffsetDateTime,
    updated_at: OffsetDateTime,
}

fn validated(policy: &CleanupPolicy) -> Result<(), StoreError> {
    policy
        .validate()
        .map_err(|error| StoreError::Conflict(error.to_string()))
}

fn format_label(format: Option<RepoFormat>) -> Option<&'static str> {
    format.map(RepoFormat::as_str)
}

impl Row {
    fn into_policy(self) -> Result<CleanupPolicy, StoreError> {
        let format = self
            .format
            .as_deref()
            .map(RepoFormat::from_str)
            .transpose()
            .map_err(|_| corrupt("read cleanup policy"))?;
        let mut policy = CleanupPolicy::new(
            CleanupPolicyId::from_uuid(self.id),
            self.name,
            self.criteria.0,
            self.created_at,
        );
        policy.description = self.description;
        policy.format = format;
        policy.version = self.version;
        policy.updated_at = self.updated_at;
        Ok(policy)
    }
}

#[async_trait]
impl CleanupPolicyRepo for PgCleanupPolicyRepo {
    async fn create(&self, policy: &CleanupPolicy) -> Result<(), StoreError> {
        validated(policy)?;
        let taken: bool = sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM cleanup_policies WHERE id = $1 OR name = $2)",
        )
        .bind(policy.id.as_uuid())
        .bind(&policy.name)
        .fetch_one(&self.pool)
        .await
        .map_err(|e| map_err("create cleanup policy", e))?;
        if taken {
            return Err(StoreError::Conflict(format!(
                "cleanup policy {} already exists",
                policy.name
            )));
        }
        sqlx::query(
            "INSERT INTO cleanup_policies (id, name, description, format, criteria, version, \
             created_at, updated_at) VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
        )
        .bind(policy.id.as_uuid())
        .bind(&policy.name)
        .bind(&policy.description)
        .bind(format_label(policy.format))
        .bind(Json(&policy.criteria))
        .bind(policy.version)
        .bind(policy.created_at)
        .bind(policy.updated_at)
        .execute(&self.pool)
        .await
        .map_err(|e| map_err("create cleanup policy", e))?;
        Ok(())
    }

    async fn get(&self, id: CleanupPolicyId) -> Result<Option<CleanupPolicy>, StoreError> {
        sqlx::query_as::<_, Row>(
            "SELECT id, name, description, format, criteria, version, created_at, updated_at \
             FROM cleanup_policies WHERE id = $1",
        )
        .bind(id.as_uuid())
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| map_err("read cleanup policy", e))?
        .map(Row::into_policy)
        .transpose()
    }

    async fn list(&self) -> Result<Vec<CleanupPolicy>, StoreError> {
        let rows = sqlx::query_as::<_, Row>(
            "SELECT id, name, description, format, criteria, version, created_at, updated_at \
             FROM cleanup_policies ORDER BY name",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(|e| map_err("list cleanup policies", e))?;
        rows.into_iter().map(Row::into_policy).collect()
    }

    async fn update(
        &self,
        policy: &CleanupPolicy,
        expected_version: i64,
    ) -> Result<(), StoreError> {
        validated(policy)?;
        let next_version = expected_version.checked_add(1).ok_or_else(|| {
            StoreError::Conflict("cleanup policy version is out of range".to_owned())
        })?;
        let updated = sqlx::query(
            "UPDATE cleanup_policies SET name = $2, description = $3, format = $4, criteria = $5, \
             version = $6, updated_at = now() WHERE id = $1 AND version = $7",
        )
        .bind(policy.id.as_uuid())
        .bind(&policy.name)
        .bind(&policy.description)
        .bind(format_label(policy.format))
        .bind(Json(&policy.criteria))
        .bind(next_version)
        .bind(expected_version)
        .execute(&self.pool)
        .await
        .map_err(|e| map_err("update cleanup policy", e))?;
        if updated.rows_affected() == 1 {
            return Ok(());
        }
        let found: Option<i64> =
            sqlx::query_scalar("SELECT version FROM cleanup_policies WHERE id = $1")
                .bind(policy.id.as_uuid())
                .fetch_optional(&self.pool)
                .await
                .map_err(|e| map_err("read cleanup policy", e))?;
        match found {
            None => Err(StoreError::NotFound("cleanup policy".to_owned())),
            Some(_) => Err(StoreError::Conflict(
                "cleanup policy was changed by someone else".to_owned(),
            )),
        }
    }

    async fn delete(&self, id: CleanupPolicyId) -> Result<(), StoreError> {
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| map_err("delete cleanup policy", e))?;
        let exists: bool =
            sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM cleanup_policies WHERE id = $1)")
                .bind(id.as_uuid())
                .fetch_one(&mut *tx)
                .await
                .map_err(|e| map_err("delete cleanup policy", e))?;
        if !exists {
            return Err(StoreError::NotFound("cleanup policy".to_owned()));
        }
        let attached: bool = sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM repositories WHERE $1 = ANY(cleanup_policy_ids))",
        )
        .bind(id.as_uuid())
        .fetch_one(&mut *tx)
        .await
        .map_err(|e| map_err("delete cleanup policy", e))?;
        if attached {
            return Err(StoreError::Conflict(
                "cleanup policy is still attached to a repository".to_owned(),
            ));
        }
        sqlx::query("DELETE FROM cleanup_policies WHERE id = $1")
            .bind(id.as_uuid())
            .execute(&mut *tx)
            .await
            .map_err(|e| map_err("delete cleanup policy", e))?;
        tx.commit()
            .await
            .map_err(|e| map_err("delete cleanup policy", e))
    }

    async fn repositories_using(
        &self,
        id: CleanupPolicyId,
    ) -> Result<Vec<RepositoryId>, StoreError> {
        let ids: Vec<Uuid> = sqlx::query_scalar(
            "SELECT id FROM repositories WHERE $1 = ANY(cleanup_policy_ids) ORDER BY id",
        )
        .bind(id.as_uuid())
        .fetch_all(&self.pool)
        .await
        .map_err(|e| map_err("list repositories using cleanup policy", e))?;
        Ok(ids.into_iter().map(RepositoryId::from_uuid).collect())
    }
}
