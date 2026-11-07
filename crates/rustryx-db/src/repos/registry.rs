//! Named remote registries. Credentials are sealed. An update rewrites every proxy that
//! points at the registry so it does not keep fetching the old address.

use std::sync::Arc;

use rustryx_core::error::StoreError;
use rustryx_core::ids::{RegistryId, ReplicationRuleId, RepositoryId, TrustedCertificateId};
use rustryx_core::ports::{InvalidationEvent, RegistryDependents, RegistryRepo};
use rustryx_core::registry::{Registry, RegistryAuth, RegistryProvider, RegistryTest};
use serde::{Deserialize, Serialize};
use sqlx::{PgConnection, PgPool};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::error::{backend, map_err};
use crate::sealer::SecretSealer;

/// Registries.
#[derive(Clone)]
pub struct PgRegistryRepo {
    pool: PgPool,
    sealer: Arc<dyn SecretSealer>,
}

impl PgRegistryRepo {
    /// A repo that seals credentials with `sealer`.
    #[must_use]
    pub fn new(pool: PgPool, sealer: Arc<dyn SecretSealer>) -> Self {
        Self { pool, sealer }
    }
}

#[derive(sqlx::FromRow)]
struct Row {
    id: Uuid,
    name: String,
    provider: String,
    url: String,
    auth: serde_json::Value,
    ca_certificate_ids: Vec<Uuid>,
    last_test: Option<serde_json::Value>,
    version: i64,
    created_at: OffsetDateTime,
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum StoredAuth {
    None,
    Basic {
        username: String,
        password_sealed: String,
    },
    Bearer {
        token_sealed: String,
    },
}

#[async_trait::async_trait]
impl RegistryRepo for PgRegistryRepo {
    async fn create(&self, registry: &Registry) -> Result<(), StoreError> {
        registry
            .validate()
            .map_err(|error| StoreError::Conflict(error.to_string()))?;
        let auth = seal_auth(self.sealer.as_ref(), registry.id, &registry.auth)?;
        sqlx::query(
            "INSERT INTO registries (id, name, provider, url, auth, ca_certificate_ids, \
             last_test, version, created_at) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9)",
        )
        .bind(registry.id.as_uuid())
        .bind(&registry.name)
        .bind(provider_label(registry.provider))
        .bind(&registry.url)
        .bind(serde_json::to_value(auth).map_err(|_| backend("create registry", "auth"))?)
        .bind(cert_ids(&registry.ca_certificate_ids))
        .bind(test_value(registry.last_test.as_ref())?)
        .bind(registry.version)
        .bind(registry.created_at)
        .execute(&self.pool)
        .await
        .map_err(|e| map_err("create registry", e))?;
        Ok(())
    }

    async fn get(&self, id: RegistryId) -> Result<Option<Registry>, StoreError> {
        let row: Option<Row> = sqlx::query_as("SELECT * FROM registries WHERE id = $1")
            .bind(id.as_uuid())
            .fetch_optional(&self.pool)
            .await
            .map_err(|e| map_err("read registry", e))?;
        row.map(|row| row.into_registry(self.sealer.as_ref()))
            .transpose()
    }

    async fn list(&self) -> Result<Vec<Registry>, StoreError> {
        let rows: Vec<Row> = sqlx::query_as("SELECT * FROM registries ORDER BY name")
            .fetch_all(&self.pool)
            .await
            .map_err(|e| map_err("list registries", e))?;
        rows.into_iter()
            .map(|row| row.into_registry(self.sealer.as_ref()))
            .collect()
    }

    async fn update(&self, registry: &Registry, expected_version: i64) -> Result<(), StoreError> {
        registry
            .validate()
            .map_err(|error| StoreError::Conflict(error.to_string()))?;
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| map_err("update registry", e))?;
        let stored: Option<OffsetDateTime> =
            sqlx::query_scalar("SELECT created_at FROM registries WHERE id = $1 FOR UPDATE")
                .bind(registry.id.as_uuid())
                .fetch_optional(&mut *tx)
                .await
                .map_err(|e| map_err("update registry", e))?;
        let Some(created_at) = stored else {
            return Err(StoreError::NotFound("registry".to_owned()));
        };
        let auth = seal_auth(self.sealer.as_ref(), registry.id, &registry.auth)?;
        let updated = sqlx::query(
            "UPDATE registries SET name = $2, provider = $3, url = $4, auth = $5, \
             ca_certificate_ids = $6, version = $7 WHERE id = $1 AND version = $8",
        )
        .bind(registry.id.as_uuid())
        .bind(&registry.name)
        .bind(provider_label(registry.provider))
        .bind(&registry.url)
        .bind(serde_json::to_value(&auth).map_err(|_| backend("update registry", "auth"))?)
        .bind(cert_ids(&registry.ca_certificate_ids))
        .bind(expected_version + 1)
        .bind(expected_version)
        .execute(&mut *tx)
        .await
        .map_err(|e| map_err("update registry", e))?;
        if updated.rows_affected() == 0 {
            return Err(StoreError::Conflict(
                "registry was changed by someone else".to_owned(),
            ));
        }
        rewrite_proxies(&mut tx, self.sealer.as_ref(), registry, &auth).await?;
        let _ = created_at;
        super::common::notify(&mut tx, &InvalidationEvent::Reset).await?;
        tx.commit()
            .await
            .map_err(|e| map_err("update registry", e))?;
        Ok(())
    }

    async fn delete(&self, id: RegistryId) -> Result<(), StoreError> {
        let dependents = self.dependents(id).await?;
        if !dependents.proxy_repositories.is_empty() || !dependents.replication_rules.is_empty() {
            return Err(StoreError::Conflict("registry is still in use".to_owned()));
        }
        let deleted = sqlx::query("DELETE FROM registries WHERE id = $1")
            .bind(id.as_uuid())
            .execute(&self.pool)
            .await
            .map_err(|e| map_err("delete registry", e))?;
        if deleted.rows_affected() == 0 {
            return Err(StoreError::NotFound("registry".to_owned()));
        }
        Ok(())
    }

    async fn dependents(&self, id: RegistryId) -> Result<RegistryDependents, StoreError> {
        let proxies: Vec<Uuid> = sqlx::query_scalar(
            "SELECT id FROM repositories \
             WHERE proxy_config->>'registry_id' = $1 ORDER BY id",
        )
        .bind(id.as_uuid().to_string())
        .fetch_all(&self.pool)
        .await
        .map_err(|e| map_err("read registry dependents", e))?;
        let rules: Vec<Uuid> = sqlx::query_scalar(
            "SELECT id FROM replication_rules WHERE registry_id = $1 ORDER BY id",
        )
        .bind(id.as_uuid())
        .fetch_all(&self.pool)
        .await
        .map_err(|e| map_err("read registry dependents", e))?;
        Ok(RegistryDependents {
            proxy_repositories: proxies.into_iter().map(RepositoryId::from_uuid).collect(),
            replication_rules: rules
                .into_iter()
                .map(ReplicationRuleId::from_uuid)
                .collect(),
        })
    }

    async fn record_test(&self, id: RegistryId, test: &RegistryTest) -> Result<(), StoreError> {
        let updated = sqlx::query("UPDATE registries SET last_test = $2 WHERE id = $1")
            .bind(id.as_uuid())
            .bind(test_value(Some(test))?)
            .execute(&self.pool)
            .await
            .map_err(|e| map_err("record registry test", e))?;
        if updated.rows_affected() == 0 {
            return Err(StoreError::NotFound("registry".to_owned()));
        }
        Ok(())
    }
}

impl Row {
    fn into_registry(self, sealer: &dyn SecretSealer) -> Result<Registry, StoreError> {
        let id = RegistryId::from_uuid(self.id);
        let mut registry = Registry::new(
            id,
            self.name,
            parse_provider(&self.provider)?,
            self.url,
            self.created_at,
        );
        registry.auth = open_auth(sealer, id, &self.auth)?;
        registry.ca_certificate_ids = self
            .ca_certificate_ids
            .into_iter()
            .map(TrustedCertificateId::from_uuid)
            .collect();
        registry.last_test = self
            .last_test
            .map(serde_json::from_value)
            .transpose()
            .map_err(|_| backend("read registry", "last test"))?;
        registry.version = self.version;
        Ok(registry)
    }
}

async fn rewrite_proxies(
    tx: &mut PgConnection,
    sealer: &dyn SecretSealer,
    registry: &Registry,
    auth: &StoredAuth,
) -> Result<(), StoreError> {
    let rows: Vec<(Uuid, serde_json::Value)> = sqlx::query_as(
        "SELECT id, proxy_config FROM repositories \
         WHERE proxy_config->>'registry_id' = $1 ORDER BY id FOR UPDATE",
    )
    .bind(registry.id.as_uuid().to_string())
    .fetch_all(&mut *tx)
    .await
    .map_err(|e| map_err("rewrite proxy registries", e))?;
    for (id, mut config) in rows {
        let object = config
            .as_object_mut()
            .ok_or_else(|| backend("rewrite proxy registries", "proxy config"))?;
        object.insert(
            "remote_url".to_owned(),
            serde_json::Value::String(registry.url.clone()),
        );
        match auth {
            StoredAuth::Basic { username, .. } => {
                let RegistryAuth::Basic { password, .. } = &registry.auth else {
                    return Err(backend("rewrite proxy registries", "auth"));
                };
                let sealed =
                    sealer.seal(&proxy_aad(RepositoryId::from_uuid(id)), password.expose())?;
                object.insert(
                    "auth".to_owned(),
                    serde_json::json!({ "username": username, "password_sealed": sealed }),
                );
            }
            StoredAuth::None | StoredAuth::Bearer { .. } => {
                object.remove("auth");
            }
        }
        if registry.ca_certificate_ids.is_empty() {
            object.remove("ca_certificate_ids");
        } else {
            object.insert(
                "ca_certificate_ids".to_owned(),
                serde_json::json!(cert_ids(&registry.ca_certificate_ids)),
            );
        }
        sqlx::query("UPDATE repositories SET proxy_config = $2, updated_at = now() WHERE id = $1")
            .bind(id)
            .bind(&config)
            .execute(&mut *tx)
            .await
            .map_err(|e| map_err("rewrite proxy registries", e))?;
    }
    Ok(())
}

fn seal_auth(
    sealer: &dyn SecretSealer,
    id: RegistryId,
    auth: &RegistryAuth,
) -> Result<StoredAuth, StoreError> {
    let aad = credential_aad(id);
    Ok(match auth {
        RegistryAuth::None => StoredAuth::None,
        RegistryAuth::Basic { username, password } => StoredAuth::Basic {
            username: username.clone(),
            password_sealed: sealer.seal(&aad, password.expose())?,
        },
        RegistryAuth::Bearer { token } => StoredAuth::Bearer {
            token_sealed: sealer.seal(&aad, token.expose())?,
        },
    })
}

fn open_auth(
    sealer: &dyn SecretSealer,
    id: RegistryId,
    value: &serde_json::Value,
) -> Result<RegistryAuth, StoreError> {
    let stored: StoredAuth =
        serde_json::from_value(value.clone()).map_err(|_| backend("read registry", "auth"))?;
    let aad = credential_aad(id);
    Ok(match stored {
        StoredAuth::None => RegistryAuth::None,
        StoredAuth::Basic {
            username,
            password_sealed,
        } => RegistryAuth::Basic {
            username,
            password: sealer.open(&aad, &password_sealed)?,
        },
        StoredAuth::Bearer { token_sealed } => RegistryAuth::Bearer {
            token: sealer.open(&aad, &token_sealed)?,
        },
    })
}

fn credential_aad(id: RegistryId) -> Vec<u8> {
    let mut aad = b"rustryx/registry/credential/v1/".to_vec();
    aad.extend_from_slice(id.as_uuid().as_bytes());
    aad
}

fn proxy_aad(id: RepositoryId) -> Vec<u8> {
    let mut aad = b"rustryx/repository/upstream-password/v1/".to_vec();
    aad.extend_from_slice(id.as_uuid().as_bytes());
    aad
}

fn provider_label(provider: RegistryProvider) -> &'static str {
    match provider {
        RegistryProvider::Rustryx => "rustryx",
        RegistryProvider::Oci => "oci",
    }
}

fn parse_provider(label: &str) -> Result<RegistryProvider, StoreError> {
    match label {
        "rustryx" => Ok(RegistryProvider::Rustryx),
        "oci" => Ok(RegistryProvider::Oci),
        _ => Err(backend("read registry", "unknown provider")),
    }
}

fn cert_ids(ids: &[TrustedCertificateId]) -> Vec<Uuid> {
    ids.iter().map(TrustedCertificateId::as_uuid).collect()
}

fn test_value(test: Option<&RegistryTest>) -> Result<Option<serde_json::Value>, StoreError> {
    test.map(serde_json::to_value)
        .transpose()
        .map_err(|_| backend("write registry", "last test"))
}
