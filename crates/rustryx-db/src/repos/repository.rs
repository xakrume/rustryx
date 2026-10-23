//! [`RepositoryRepo`] on `PostgreSQL`.
//!
//! Upstream passwords of proxy repositories are sealed before they reach the database (see
//! [`crate::sealer`]); `proxy_config` therefore never contains a clear text secret, and the
//! sealed value is bound to the repository id so it cannot be replayed on another row.

use std::str::FromStr;
use std::sync::Arc;

use async_trait::async_trait;
use rustryx_core::StoreError;
use rustryx_core::ids::{
    BlobStoreId, CleanupPolicyId, ProjectId, RegistryId, RepositoryId, RoutingRuleId,
    TrustedCertificateId,
};
use rustryx_core::model::{
    GroupConfig, ProjectRef, ProxyConfig, RepoFormat, RepoType, Repository, UpstreamAuth,
    WritePolicy,
};
use rustryx_core::names::RepoName;
use rustryx_core::ports::RepositoryRepo;
use rustryx_core::project::{ProjectName, ProjectVisibility};
use serde::{Deserialize, Serialize};
use sqlx::{FromRow, PgPool};
use time::OffsetDateTime;
use uuid::Uuid;

use super::common::{corrupt, to_i64, to_u64};
use crate::error::map_err;
use crate::sealer::SecretSealer;

/// The project is joined in so that `Repository::project` (what authorization needs: id, name
/// and visibility) costs no second query. A `LEFT JOIN`: most repositories have no project.
macro_rules! select {
    ($($tail:literal),*) => {
        concat!(
            "SELECT r.id, r.name, r.format, r.repo_type, r.blob_store_id, r.write_policy, \
             r.allow_anonymous, r.online, r.quota_bytes, r.cleanup_policy_ids, \
             r.routing_rule_id, r.proxy_config, r.group_config, r.format_config, \
             r.project_id, p.name AS project_name, p.visibility AS project_visibility, \
             r.created_at, r.updated_at \
             FROM repositories r LEFT JOIN projects p ON p.id = r.project_id ",
            $($tail),*
        )
    };
}

/// Repositories.
#[derive(Debug, Clone)]
pub struct PgRepositoryRepo {
    pool: PgPool,
    sealer: Arc<dyn SecretSealer>,
}

impl PgRepositoryRepo {
    /// Creates the repository over `pool`, sealing upstream passwords with `sealer`.
    #[must_use]
    pub fn new(pool: PgPool, sealer: Arc<dyn SecretSealer>) -> Self {
        Self { pool, sealer }
    }

    /// Re-seals every stored upstream password with the sealer's current key. Run it after
    /// adding a new key as `current` so the old key can be retired (KEK rotation without
    /// downtime: readers keep working throughout, because the old key stays in the ring).
    /// Returns the number of repositories rewritten. A row that was edited concurrently is
    /// skipped (compare-and-set on the old value) and caught by the next run.
    pub async fn reseal_upstream_secrets(&self) -> Result<u64, StoreError> {
        let rows: Vec<(Uuid, serde_json::Value)> = sqlx::query_as(
            "SELECT id, proxy_config FROM repositories \
             WHERE proxy_config -> 'auth' ->> 'password_sealed' IS NOT NULL",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(|e| map_err("list sealed repositories", e))?;
        let mut rewritten = 0_u64;
        for (id, old) in rows {
            let repo_id = RepositoryId::from_uuid(id);
            let proxy = open_proxy(self.sealer.as_ref(), repo_id, old.clone())?;
            let fresh = seal_proxy(self.sealer.as_ref(), repo_id, &proxy)?;
            let result = sqlx::query(
                "UPDATE repositories SET proxy_config = $2 WHERE id = $1 AND proxy_config = $3",
            )
            .bind(id)
            .bind(&fresh)
            .bind(&old)
            .execute(&self.pool)
            .await
            .map_err(|e| map_err("reseal upstream secret", e))?;
            rewritten += result.rows_affected();
        }
        Ok(rewritten)
    }
}

/// What `proxy_config` holds: the domain struct with the password replaced by its envelope.
///
/// The four fields of wave 2 are `default` on read (rows written by phase 1 do not have them)
/// and omitted on write when empty, so a row that uses none of them keeps the phase 1 shape
/// and an N-1 binary (which ignores unknown fields) reads every row. An N-1 binary that
/// rewrites a row drops them; that only matters once an operator has set one, which is gated
/// by the feature level.
#[derive(Serialize, Deserialize)]
struct StoredProxy {
    remote_url: String,
    auth: Option<StoredAuth>,
    content_max_age_secs: u64,
    metadata_max_age_secs: u64,
    negative_cache_ttl_secs: u64,
    max_concurrent_upstream: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    registry_id: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    ca_certificate_ids: Vec<Uuid>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    namespace_allowlist: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    bandwidth_limit_kbps: Option<u32>,
}

#[derive(Serialize, Deserialize)]
struct StoredAuth {
    username: String,
    password_sealed: String,
}

/// Associated data of a sealed upstream password: purpose and repository id. The purpose
/// keeps a value sealed for another column from being accepted here.
fn aad(id: RepositoryId) -> Vec<u8> {
    let mut aad = b"rustryx/repository/upstream-password/v1/".to_vec();
    aad.extend_from_slice(id.as_uuid().as_bytes());
    aad
}

fn seal_proxy(
    sealer: &dyn SecretSealer,
    id: RepositoryId,
    proxy: &ProxyConfig,
) -> Result<serde_json::Value, StoreError> {
    let auth = proxy
        .auth
        .as_ref()
        .map(|a| {
            Ok::<_, StoreError>(StoredAuth {
                username: a.username.clone(),
                password_sealed: sealer.seal(&aad(id), a.password.expose())?,
            })
        })
        .transpose()?;
    let stored = StoredProxy {
        remote_url: proxy.remote_url.clone(),
        auth,
        content_max_age_secs: proxy.content_max_age_secs,
        metadata_max_age_secs: proxy.metadata_max_age_secs,
        negative_cache_ttl_secs: proxy.negative_cache_ttl_secs,
        max_concurrent_upstream: proxy.max_concurrent_upstream,
        registry_id: proxy.registry_id.map(|id| id.as_uuid()),
        ca_certificate_ids: proxy
            .ca_certificate_ids
            .iter()
            .map(TrustedCertificateId::as_uuid)
            .collect(),
        namespace_allowlist: proxy.namespace_allowlist.clone(),
        bandwidth_limit_kbps: proxy.bandwidth_limit_kbps,
    };
    serde_json::to_value(stored).map_err(|_| corrupt("serialize proxy config"))
}

fn open_proxy(
    sealer: &dyn SecretSealer,
    id: RepositoryId,
    value: serde_json::Value,
) -> Result<ProxyConfig, StoreError> {
    let stored: StoredProxy =
        serde_json::from_value(value).map_err(|_| corrupt("read proxy config"))?;
    let auth = stored
        .auth
        .map(|a| {
            Ok::<_, StoreError>(UpstreamAuth {
                username: a.username,
                password: sealer.open(&aad(id), &a.password_sealed)?,
            })
        })
        .transpose()?;
    let mut proxy = ProxyConfig::new(stored.remote_url);
    proxy.auth = auth;
    proxy.content_max_age_secs = stored.content_max_age_secs;
    proxy.metadata_max_age_secs = stored.metadata_max_age_secs;
    proxy.negative_cache_ttl_secs = stored.negative_cache_ttl_secs;
    proxy.max_concurrent_upstream = stored.max_concurrent_upstream;
    // The registry is stored by id only. Resolving `remote_url` and `auth` from it is the job
    // of the registry stream, which owns the `registries` table; until it lands the id is
    // carried unchanged so that no edit loses it.
    proxy.registry_id = stored.registry_id.map(RegistryId::from_uuid);
    proxy.ca_certificate_ids = stored
        .ca_certificate_ids
        .into_iter()
        .map(TrustedCertificateId::from_uuid)
        .collect();
    proxy.namespace_allowlist = stored.namespace_allowlist;
    proxy.bandwidth_limit_kbps = stored.bandwidth_limit_kbps;
    Ok(proxy)
}

fn parse_visibility(label: &str) -> Result<ProjectVisibility, StoreError> {
    match label {
        "private" => Ok(ProjectVisibility::Private),
        "public" => Ok(ProjectVisibility::Public),
        _ => Err(corrupt("read repository")),
    }
}

const fn write_policy_label(policy: WritePolicy) -> &'static str {
    match policy {
        WritePolicy::Allow => "allow",
        WritePolicy::AllowOnce => "allow_once",
        WritePolicy::Deny => "deny",
    }
}

pub(super) fn parse_write_policy(label: &str) -> Result<WritePolicy, StoreError> {
    match label {
        "allow" => Ok(WritePolicy::Allow),
        "allow_once" => Ok(WritePolicy::AllowOnce),
        "deny" => Ok(WritePolicy::Deny),
        _ => Err(corrupt("read repository")),
    }
}

#[derive(Debug, FromRow)]
struct Row {
    id: Uuid,
    name: String,
    format: String,
    repo_type: String,
    blob_store_id: Uuid,
    write_policy: String,
    allow_anonymous: bool,
    online: bool,
    quota_bytes: Option<i64>,
    cleanup_policy_ids: Vec<Uuid>,
    routing_rule_id: Option<Uuid>,
    proxy_config: Option<serde_json::Value>,
    group_config: Option<serde_json::Value>,
    format_config: serde_json::Value,
    project_id: Option<Uuid>,
    project_name: Option<String>,
    project_visibility: Option<String>,
    created_at: OffsetDateTime,
    updated_at: OffsetDateTime,
}

impl Row {
    fn into_repository(self, sealer: &dyn SecretSealer) -> Result<Repository, StoreError> {
        let id = RepositoryId::from_uuid(self.id);
        let proxy = self
            .proxy_config
            .map(|v| open_proxy(sealer, id, v))
            .transpose()?;
        let group = self
            .group_config
            .map(|v| {
                serde_json::from_value::<GroupConfig>(v).map_err(|_| corrupt("read group config"))
            })
            .transpose()?;
        let mut repository = Repository::new(
            id,
            RepoName::parse(&self.name).map_err(|_| corrupt("read repository"))?,
            RepoFormat::from_str(&self.format).map_err(|_| corrupt("read repository"))?,
            RepoType::from_str(&self.repo_type).map_err(|_| corrupt("read repository"))?,
            BlobStoreId::from_uuid(self.blob_store_id),
            self.created_at,
        );
        repository.write_policy = parse_write_policy(&self.write_policy)?;
        repository.allow_anonymous = self.allow_anonymous;
        repository.online = self.online;
        repository.quota_bytes = self
            .quota_bytes
            .map(|q| to_u64(q, "read repository"))
            .transpose()?;
        repository.cleanup_policy_ids = self
            .cleanup_policy_ids
            .into_iter()
            .map(CleanupPolicyId::from_uuid)
            .collect();
        repository.routing_rule_id = self.routing_rule_id.map(RoutingRuleId::from_uuid);
        repository.proxy = proxy;
        repository.group = group;
        repository.format_config = self.format_config;
        repository.project = match (self.project_id, self.project_name, self.project_visibility) {
            (Some(id), Some(name), Some(visibility)) => Some(ProjectRef {
                id: ProjectId::from_uuid(id),
                name: ProjectName::parse(&name).map_err(|_| corrupt("read repository"))?,
                visibility: parse_visibility(&visibility)?,
            }),
            (None, _, _) => None,
            // A project id whose join found no row: the foreign key makes this unreachable.
            (Some(_), _, _) => return Err(corrupt("read repository")),
        };
        repository.updated_at = self.updated_at;
        Ok(repository)
    }
}

/// Values of the mutable columns, prepared once for insert and update.
struct Prepared {
    quota: Option<i64>,
    cleanup: Vec<Uuid>,
    proxy: Option<serde_json::Value>,
    group: Option<serde_json::Value>,
}

impl PgRepositoryRepo {
    fn prepare(&self, repository: &Repository) -> Result<Prepared, StoreError> {
        Ok(Prepared {
            quota: repository.quota_bytes.map(to_i64).transpose()?,
            cleanup: repository
                .cleanup_policy_ids
                .iter()
                .map(CleanupPolicyId::as_uuid)
                .collect(),
            proxy: repository
                .proxy
                .as_ref()
                .map(|p| seal_proxy(self.sealer.as_ref(), repository.id, p))
                .transpose()?,
            group: repository
                .group
                .as_ref()
                .map(serde_json::to_value)
                .transpose()
                .map_err(|_| corrupt("serialize group config"))?,
        })
    }

    fn decode_all(&self, rows: Vec<Row>) -> Result<Vec<Repository>, StoreError> {
        rows.into_iter()
            .map(|r| r.into_repository(self.sealer.as_ref()))
            .collect()
    }
}

#[async_trait]
impl RepositoryRepo for PgRepositoryRepo {
    /// A repository created with a `project` is attached to it at once; the same trigger
    /// sets `allow_anonymous` from the project visibility whatever the struct says.
    async fn create(&self, repository: &Repository) -> Result<(), StoreError> {
        let p = self.prepare(repository)?;
        sqlx::query(
            "INSERT INTO repositories (id, name, format, repo_type, blob_store_id, write_policy, \
             allow_anonymous, online, quota_bytes, cleanup_policy_ids, routing_rule_id, \
             proxy_config, group_config, format_config, created_at, updated_at, project_id) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17)",
        )
        .bind(repository.id.as_uuid())
        .bind(repository.name.as_str())
        .bind(repository.format.as_str())
        .bind(repository.repo_type.as_str())
        .bind(repository.blob_store_id.as_uuid())
        .bind(write_policy_label(repository.write_policy))
        .bind(repository.allow_anonymous)
        .bind(repository.online)
        .bind(p.quota)
        .bind(&p.cleanup)
        .bind(repository.routing_rule_id.map(|id| id.as_uuid()))
        .bind(&p.proxy)
        .bind(&p.group)
        .bind(&repository.format_config)
        .bind(repository.created_at)
        .bind(repository.updated_at)
        .bind(repository.project.as_ref().map(|p| p.id.as_uuid()))
        .execute(&self.pool)
        .await
        .map_err(|e| map_err("create repository", e))?;
        Ok(())
    }

    async fn get(&self, id: RepositoryId) -> Result<Option<Repository>, StoreError> {
        sqlx::query_as::<_, Row>(select!("WHERE r.id = $1"))
            .bind(id.as_uuid())
            .fetch_optional(&self.pool)
            .await
            .map_err(|e| map_err("read repository", e))?
            .map(|r| r.into_repository(self.sealer.as_ref()))
            .transpose()
    }

    async fn get_by_name(&self, name: &RepoName) -> Result<Option<Repository>, StoreError> {
        sqlx::query_as::<_, Row>(select!("WHERE r.name = $1"))
            .bind(name.as_str())
            .fetch_optional(&self.pool)
            .await
            .map_err(|e| map_err("read repository", e))?
            .map(|r| r.into_repository(self.sealer.as_ref()))
            .transpose()
    }

    async fn list(&self) -> Result<Vec<Repository>, StoreError> {
        let rows = sqlx::query_as::<_, Row>(select!("ORDER BY r.name"))
            .fetch_all(&self.pool)
            .await
            .map_err(|e| map_err("list repositories", e))?;
        self.decode_all(rows)
    }

    /// Writes the mutable fields. `project_id` is NOT among them: a repository changes its
    /// project through `ProjectRepo::attach_repository` and `detach_repository`, which also
    /// clean up what depends on the membership (project scoped labels) in the same
    /// transaction. `allow_anonymous` is written as given, but for a repository in a project
    /// the trigger `repositories_project_sync` overrules it with the project visibility, so
    /// no caller (an N-1 binary included) can make a private project's repository public.
    ///
    /// `name`, `format`, `repo_type` and `blob_store_id` identify
    /// what the repository IS (URL space, protocol, where the bytes live): the statement
    /// matches on them, and a caller that changed one gets `Conflict`, not a silent rewrite
    /// that would orphan the content. `NotFound` when the id does not exist.
    async fn update(&self, repository: &Repository) -> Result<(), StoreError> {
        let p = self.prepare(repository)?;
        let result = sqlx::query(
            "UPDATE repositories SET write_policy = $2, allow_anonymous = $3, online = $4, \
             quota_bytes = $5, cleanup_policy_ids = $6, routing_rule_id = $7, proxy_config = $8, \
             group_config = $9, format_config = $10, updated_at = $11 \
             WHERE id = $1 AND name = $12 AND format = $13 AND repo_type = $14 AND blob_store_id = $15",
        )
        .bind(repository.id.as_uuid())
        .bind(write_policy_label(repository.write_policy))
        .bind(repository.allow_anonymous)
        .bind(repository.online)
        .bind(p.quota)
        .bind(&p.cleanup)
        .bind(repository.routing_rule_id.map(|id| id.as_uuid()))
        .bind(&p.proxy)
        .bind(&p.group)
        .bind(&repository.format_config)
        .bind(repository.updated_at)
        .bind(repository.name.as_str())
        .bind(repository.format.as_str())
        .bind(repository.repo_type.as_str())
        .bind(repository.blob_store_id.as_uuid())
        .execute(&self.pool)
        .await
        .map_err(|e| map_err("update repository", e))?;
        if result.rows_affected() > 0 {
            return Ok(());
        }
        let exists: bool =
            sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM repositories WHERE id = $1)")
                .bind(repository.id.as_uuid())
                .fetch_one(&self.pool)
                .await
                .map_err(|e| map_err("update repository", e))?;
        if exists {
            Err(StoreError::Conflict(
                "name, format, type and blob store of a repository cannot change".to_owned(),
            ))
        } else {
            Err(StoreError::NotFound("repository".to_owned()))
        }
    }

    /// Cascades to components and assets. The freed blobs are picked up by GC.
    async fn delete(&self, id: RepositoryId) -> Result<(), StoreError> {
        let result = sqlx::query("DELETE FROM repositories WHERE id = $1")
            .bind(id.as_uuid())
            .execute(&self.pool)
            .await
            .map_err(|e| map_err("delete repository", e))?;
        if result.rows_affected() == 0 {
            return Err(StoreError::NotFound("repository".to_owned()));
        }
        Ok(())
    }
}
