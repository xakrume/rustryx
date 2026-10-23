//! [`IdpMappingRepo`] on `PostgreSQL`.

use async_trait::async_trait;
use rustryx_core::StoreError;
use rustryx_core::identity::{IdpGroupMapping, IdpProvider};
use rustryx_core::ids::{IdpGroupMappingId, RoleId};
use rustryx_core::ports::{IdpMappingRepo, InvalidationEvent};
use sqlx::{FromRow, PgPool};
use uuid::Uuid;

use super::common::{corrupt, notify};
use crate::error::map_err;

/// Group to role mappings.
#[derive(Debug, Clone)]
pub struct PgIdpMappingRepo {
    pool: PgPool,
}

impl PgIdpMappingRepo {
    /// Creates the repository over `pool`.
    #[must_use]
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

const fn provider_label(provider: IdpProvider) -> &'static str {
    match provider {
        IdpProvider::Oidc => "oidc",
        IdpProvider::Ldap => "ldap",
    }
}

#[derive(Debug, FromRow)]
struct Row {
    id: Uuid,
    provider: String,
    external_group: String,
    role_id: Uuid,
    priority: i32,
}

impl Row {
    fn into_mapping(self) -> Result<IdpGroupMapping, StoreError> {
        let provider = match self.provider.as_str() {
            "oidc" => IdpProvider::Oidc,
            "ldap" => IdpProvider::Ldap,
            _ => return Err(corrupt("read group mapping")),
        };
        Ok(IdpGroupMapping {
            id: IdpGroupMappingId::from_uuid(self.id),
            provider,
            external_group: self.external_group,
            role_id: RoleId::from_uuid(self.role_id),
            priority: self.priority,
        })
    }
}

#[async_trait]
impl IdpMappingRepo for PgIdpMappingRepo {
    async fn list(&self, provider: IdpProvider) -> Result<Vec<IdpGroupMapping>, StoreError> {
        sqlx::query_as::<_, Row>(
            "SELECT id, provider, external_group, role_id, priority FROM idp_group_mappings \
             WHERE provider = $1 ORDER BY priority, external_group",
        )
        .bind(provider_label(provider))
        .fetch_all(&self.pool)
        .await
        .map_err(|e| map_err("list group mappings", e))?
        .into_iter()
        .map(Row::into_mapping)
        .collect()
    }

    /// Replaces the whole row of an existing `(provider, external_group)`, id included, so
    /// that a later `delete(mapping.id)` with the id the caller just wrote finds it.
    /// `Conflict` when the role does not exist.
    async fn upsert(&self, mapping: &IdpGroupMapping) -> Result<(), StoreError> {
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| map_err("upsert group mapping", e))?;
        sqlx::query(
            "INSERT INTO idp_group_mappings (id, provider, external_group, role_id, priority) \
             VALUES ($1, $2, $3, $4, $5) \
             ON CONFLICT ON CONSTRAINT uq_idp_mapping_group DO UPDATE SET \
                 id = EXCLUDED.id, role_id = EXCLUDED.role_id, priority = EXCLUDED.priority",
        )
        .bind(mapping.id.as_uuid())
        .bind(provider_label(mapping.provider))
        .bind(&mapping.external_group)
        .bind(mapping.role_id.as_uuid())
        .bind(mapping.priority)
        .execute(&mut *tx)
        .await
        .map_err(|e| map_err("upsert group mapping", e))?;
        notify(&mut tx, &InvalidationEvent::Rbac).await?;
        tx.commit()
            .await
            .map_err(|e| map_err("upsert group mapping", e))
    }

    async fn delete(&self, id: IdpGroupMappingId) -> Result<(), StoreError> {
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| map_err("delete group mapping", e))?;
        let result = sqlx::query("DELETE FROM idp_group_mappings WHERE id = $1")
            .bind(id.as_uuid())
            .execute(&mut *tx)
            .await
            .map_err(|e| map_err("delete group mapping", e))?;
        if result.rows_affected() == 0 {
            return Err(StoreError::NotFound("group mapping".to_owned()));
        }
        notify(&mut tx, &InvalidationEvent::Rbac).await?;
        tx.commit()
            .await
            .map_err(|e| map_err("delete group mapping", e))
    }
}
