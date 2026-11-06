//! Trust material and the policy of a scope that names it.

use rustryx_core::error::StoreError;
use rustryx_core::ids::{RepositoryId, TrustMaterialId, UserId};
use rustryx_core::lifecycle::PolicyScope;
use rustryx_core::ports::{InvalidationEvent, TrustRepo};
use rustryx_core::scan::{
    CosignIdentity, GateMode, NotationLevel, RequiredSignatures, TrustMaterial, TrustMaterialKind,
    TrustPolicy,
};
use sqlx::PgPool;
use sqlx::types::Json;
use time::OffsetDateTime;
use uuid::Uuid;

use super::common::notify;
use crate::error::{backend, map_err};

/// Public keys and certificates a trust policy can name.
#[derive(Clone)]
pub struct PgTrustRepo {
    pool: PgPool,
}

impl PgTrustRepo {
    /// A repo on `pool`.
    #[must_use]
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[async_trait::async_trait]
impl TrustRepo for PgTrustRepo {
    async fn create(&self, material: &TrustMaterial) -> Result<(), StoreError> {
        material
            .validate()
            .map_err(|error| StoreError::Conflict(error.to_string()))?;
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| map_err("create trust material", e))?;
        sqlx::query(
            "INSERT INTO trust_material (id, kind, name, pem, fingerprint_sha256, not_after, \
             created_by, created_at) VALUES ($1,$2,$3,$4,$5,$6,$7,$8)",
        )
        .bind(material.id.as_uuid())
        .bind(kind_label(material.kind))
        .bind(&material.name)
        .bind(&material.pem)
        .bind(&material.fingerprint_sha256)
        .bind(material.not_after)
        .bind(material.created_by.map(|id| id.as_uuid()))
        .bind(material.created_at)
        .execute(&mut *tx)
        .await
        .map_err(|e| map_err("create trust material", e))?;
        let revision = bump_revision(&mut tx).await?;
        notify(&mut tx, &InvalidationEvent::Config(revision)).await?;
        tx.commit()
            .await
            .map_err(|e| map_err("create trust material", e))
    }

    async fn get(&self, id: TrustMaterialId) -> Result<Option<TrustMaterial>, StoreError> {
        let row: Option<MaterialRow> = sqlx::query_as("SELECT * FROM trust_material WHERE id = $1")
            .bind(id.as_uuid())
            .fetch_optional(&self.pool)
            .await
            .map_err(|e| map_err("read trust material", e))?;
        row.map(decode_material).transpose()
    }

    async fn list(
        &self,
        kind: Option<TrustMaterialKind>,
    ) -> Result<Vec<TrustMaterial>, StoreError> {
        let rows: Vec<MaterialRow> = sqlx::query_as(
            "SELECT * FROM trust_material WHERE ($1::text IS NULL OR kind = $1) ORDER BY name, id",
        )
        .bind(kind.map(kind_label))
        .fetch_all(&self.pool)
        .await
        .map_err(|e| map_err("list trust material", e))?;
        rows.into_iter().map(decode_material).collect()
    }

    async fn delete(&self, id: TrustMaterialId) -> Result<(), StoreError> {
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| map_err("delete trust material", e))?;
        let listed: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM trust_policies \
             WHERE $1 = ANY(cosign_key_ids) OR $1 = ANY(cosign_root_ids) \
                OR $1 = ANY(notation_store_ids))",
        )
        .bind(id.as_uuid())
        .fetch_one(&mut *tx)
        .await
        .map_err(|e| map_err("delete trust material", e))?;
        if listed {
            return Err(StoreError::Conflict(
                "trust material is listed by a trust policy".to_owned(),
            ));
        }
        let removed = sqlx::query("DELETE FROM trust_material WHERE id = $1")
            .bind(id.as_uuid())
            .execute(&mut *tx)
            .await
            .map_err(|e| map_err("delete trust material", e))?;
        if removed.rows_affected() == 0 {
            return Err(StoreError::NotFound(format!("trust material {id}")));
        }
        let revision = bump_revision(&mut tx).await?;
        notify(&mut tx, &InvalidationEvent::Config(revision)).await?;
        tx.commit()
            .await
            .map_err(|e| map_err("delete trust material", e))
    }
}

/// Inserts or replaces the trust policy of one scope.
pub(crate) async fn upsert_policy(
    pool: &PgPool,
    policy: &TrustPolicy,
    expected_version: Option<i64>,
) -> Result<(), StoreError> {
    policy
        .validate()
        .map_err(|error| StoreError::Conflict(error.to_string()))?;
    let mut tx = pool
        .begin()
        .await
        .map_err(|e| map_err("upsert trust policy", e))?;
    if !scope_exists(&mut tx, policy.scope).await? {
        return Err(StoreError::Conflict(
            "the scope of the policy does not exist".to_owned(),
        ));
    }
    ensure_material(&mut tx, policy).await?;
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
        (None, None) => {
            let taken: bool =
                sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM trust_policies WHERE id = $1)")
                    .bind(policy.id.as_uuid())
                    .fetch_one(&mut *tx)
                    .await
                    .map_err(|e| map_err("upsert trust policy", e))?;
            if taken {
                return Err(StoreError::Conflict(format!(
                    "trust policy {} already exists",
                    policy.id
                )));
            }
            (policy.id.as_uuid(), policy.version)
        }
    };
    let (system, project, repository) = scope_columns(policy.scope);
    let identities = Json(&policy.cosign_identities);
    let keys = uuids(&policy.cosign_keys);
    let roots = uuids(&policy.cosign_roots);
    let stores = uuids(&policy.notation_stores);
    if existing.is_some() {
        sqlx::query(
            "UPDATE trust_policies SET required = $2, mode = $3, cosign_key_ids = $4, \
             cosign_root_ids = $5, cosign_identities = $6, cosign_require_tlog = $7, \
             notation_store_ids = $8, notation_identities = $9, notation_level = $10, \
             verify_children = $11, version = $12 WHERE id = $1",
        )
        .bind(id)
        .bind(required_label(policy.required))
        .bind(mode_label(policy.mode))
        .bind(&keys)
        .bind(&roots)
        .bind(identities)
        .bind(policy.cosign_require_transparency_log)
        .bind(&stores)
        .bind(&policy.notation_identities)
        .bind(level_label(policy.notation_level))
        .bind(policy.verify_children)
        .bind(version)
        .execute(&mut *tx)
        .await
        .map_err(|e| map_err("upsert trust policy", e))?;
    } else {
        sqlx::query(
            "INSERT INTO trust_policies (id, project_id, repository_id, system, required, mode, \
             cosign_key_ids, cosign_root_ids, cosign_identities, cosign_require_tlog, \
             notation_store_ids, notation_identities, notation_level, verify_children, version) \
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15)",
        )
        .bind(id)
        .bind(project)
        .bind(repository)
        .bind(system)
        .bind(required_label(policy.required))
        .bind(mode_label(policy.mode))
        .bind(&keys)
        .bind(&roots)
        .bind(identities)
        .bind(policy.cosign_require_transparency_log)
        .bind(&stores)
        .bind(&policy.notation_identities)
        .bind(level_label(policy.notation_level))
        .bind(policy.verify_children)
        .bind(version)
        .execute(&mut *tx)
        .await
        .map_err(|e| map_err("upsert trust policy", e))?;
    }
    let revision = bump_revision(&mut tx).await?;
    notify(&mut tx, &InvalidationEvent::Config(revision)).await?;
    tx.commit()
        .await
        .map_err(|e| map_err("upsert trust policy", e))
}

/// The policy stored for exactly this scope.
pub(crate) async fn get_policy(
    pool: &PgPool,
    scope: PolicyScope,
) -> Result<Option<TrustPolicy>, StoreError> {
    let row = fetch_scope(pool, scope).await?;
    row.map(decode_policy).transpose()
}

/// Repository policy, else the project policy, else the system policy.
pub(crate) async fn effective_policy(
    pool: &PgPool,
    repository: RepositoryId,
) -> Result<Option<TrustPolicy>, StoreError> {
    let project: Option<Uuid> =
        sqlx::query_scalar("SELECT project_id FROM repositories WHERE id = $1")
            .bind(repository.as_uuid())
            .fetch_optional(pool)
            .await
            .map_err(|e| map_err("read repository", e))?
            .ok_or_else(|| StoreError::NotFound(format!("repository {repository} not found")))?;
    let rows: Vec<PolicyRow> = sqlx::query_as(
        "SELECT * FROM trust_policies \
         WHERE repository_id = $1 OR project_id IS NOT DISTINCT FROM $2 OR system",
    )
    .bind(repository.as_uuid())
    .bind(project)
    .fetch_all(pool)
    .await
    .map_err(|e| map_err("read trust policies", e))?;
    let policies = rows
        .into_iter()
        .map(decode_policy)
        .collect::<Result<Vec<_>, _>>()?;
    if let Some(own) = policies
        .iter()
        .find(|policy| policy.scope == PolicyScope::Repository(repository))
    {
        return Ok(Some(own.clone()));
    }
    if let Some(project_id) = project.map(rustryx_core::ids::ProjectId::from_uuid)
        && let Some(project_policy) = policies
            .iter()
            .find(|policy| policy.scope == PolicyScope::Project(project_id))
    {
        return Ok(Some(project_policy.clone()));
    }
    Ok(policies
        .into_iter()
        .find(|policy| policy.scope == PolicyScope::System))
}

#[derive(sqlx::FromRow)]
struct MaterialRow {
    id: Uuid,
    kind: String,
    name: String,
    pem: String,
    fingerprint_sha256: String,
    not_after: Option<OffsetDateTime>,
    created_by: Option<Uuid>,
    created_at: OffsetDateTime,
}

#[derive(sqlx::FromRow)]
struct PolicyRow {
    id: Uuid,
    project_id: Option<Uuid>,
    repository_id: Option<Uuid>,
    system: bool,
    required: String,
    mode: String,
    cosign_key_ids: Vec<Uuid>,
    cosign_root_ids: Vec<Uuid>,
    cosign_identities: Json<Vec<CosignIdentity>>,
    cosign_require_tlog: bool,
    notation_store_ids: Vec<Uuid>,
    notation_identities: Vec<String>,
    notation_level: String,
    verify_children: bool,
    version: i64,
}

fn decode_material(row: MaterialRow) -> Result<TrustMaterial, StoreError> {
    let mut material = TrustMaterial::new(
        TrustMaterialId::from_uuid(row.id),
        parse_kind(&row.kind)?,
        row.name,
        row.pem,
        row.fingerprint_sha256,
        row.created_at,
    );
    material.not_after = row.not_after;
    material.created_by = row.created_by.map(UserId::from_uuid);
    Ok(material)
}

fn decode_policy(row: PolicyRow) -> Result<TrustPolicy, StoreError> {
    let scope = match (row.system, row.project_id, row.repository_id) {
        (true, None, None) => PolicyScope::System,
        (false, Some(id), None) => {
            PolicyScope::Project(rustryx_core::ids::ProjectId::from_uuid(id))
        }
        (false, None, Some(id)) => PolicyScope::Repository(RepositoryId::from_uuid(id)),
        _ => {
            return Err(backend(
                "read trust policy",
                "trust policy scope is ambiguous",
            ));
        }
    };
    let mut policy = TrustPolicy::new(rustryx_core::ids::TrustPolicyId::from_uuid(row.id), scope);
    policy.required = parse_required(&row.required)?;
    policy.mode = parse_mode(&row.mode)?;
    policy.cosign_keys = row
        .cosign_key_ids
        .into_iter()
        .map(TrustMaterialId::from_uuid)
        .collect();
    policy.cosign_roots = row
        .cosign_root_ids
        .into_iter()
        .map(TrustMaterialId::from_uuid)
        .collect();
    policy.cosign_identities = row.cosign_identities.0;
    policy.cosign_require_transparency_log = row.cosign_require_tlog;
    policy.notation_stores = row
        .notation_store_ids
        .into_iter()
        .map(TrustMaterialId::from_uuid)
        .collect();
    policy.notation_identities = row.notation_identities;
    policy.notation_level = parse_level(&row.notation_level)?;
    policy.verify_children = row.verify_children;
    policy.version = row.version;
    Ok(policy)
}

async fn scope_exists(tx: &mut sqlx::PgConnection, scope: PolicyScope) -> Result<bool, StoreError> {
    let exists = match scope {
        PolicyScope::System => true,
        PolicyScope::Project(id) => {
            sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM projects WHERE id = $1)")
                .bind(id.as_uuid())
                .fetch_one(&mut *tx)
                .await
                .map_err(|e| map_err("read trust scope", e))?
        }
        PolicyScope::Repository(id) => {
            sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM repositories WHERE id = $1)")
                .bind(id.as_uuid())
                .fetch_one(&mut *tx)
                .await
                .map_err(|e| map_err("read trust scope", e))?
        }
    };
    Ok(exists)
}

async fn ensure_material(
    tx: &mut sqlx::PgConnection,
    policy: &TrustPolicy,
) -> Result<(), StoreError> {
    let listed: Vec<Uuid> = policy
        .cosign_keys
        .iter()
        .chain(&policy.cosign_roots)
        .chain(&policy.notation_stores)
        .map(rustryx_core::ids::TrustMaterialId::as_uuid)
        .collect();
    if listed.is_empty() {
        return Ok(());
    }
    let missing: Option<Uuid> = sqlx::query_scalar(
        "SELECT listed FROM unnest($1::uuid[]) AS listed \
         WHERE NOT EXISTS (SELECT 1 FROM trust_material WHERE id = listed) LIMIT 1",
    )
    .bind(&listed)
    .fetch_optional(&mut *tx)
    .await
    .map_err(|e| map_err("read trust material", e))?;
    if let Some(id) = missing {
        return Err(StoreError::Conflict(format!(
            "trust material {} does not exist",
            TrustMaterialId::from_uuid(id)
        )));
    }
    Ok(())
}

async fn lock_scope(
    tx: &mut sqlx::PgConnection,
    scope: PolicyScope,
) -> Result<Option<(Uuid, i64)>, StoreError> {
    let row = match scope {
        PolicyScope::System => {
            sqlx::query_as("SELECT id, version FROM trust_policies WHERE system FOR UPDATE")
                .fetch_optional(&mut *tx)
                .await
        }
        PolicyScope::Project(id) => {
            sqlx::query_as(
                "SELECT id, version FROM trust_policies WHERE project_id = $1 FOR UPDATE",
            )
            .bind(id.as_uuid())
            .fetch_optional(&mut *tx)
            .await
        }
        PolicyScope::Repository(id) => {
            sqlx::query_as(
                "SELECT id, version FROM trust_policies WHERE repository_id = $1 FOR UPDATE",
            )
            .bind(id.as_uuid())
            .fetch_optional(&mut *tx)
            .await
        }
    }
    .map_err(|e| map_err("lock trust policy", e))?;
    Ok(row)
}

async fn fetch_scope(pool: &PgPool, scope: PolicyScope) -> Result<Option<PolicyRow>, StoreError> {
    let row = match scope {
        PolicyScope::System => {
            sqlx::query_as("SELECT * FROM trust_policies WHERE system")
                .fetch_optional(pool)
                .await
        }
        PolicyScope::Project(id) => {
            sqlx::query_as("SELECT * FROM trust_policies WHERE project_id = $1")
                .bind(id.as_uuid())
                .fetch_optional(pool)
                .await
        }
        PolicyScope::Repository(id) => {
            sqlx::query_as("SELECT * FROM trust_policies WHERE repository_id = $1")
                .bind(id.as_uuid())
                .fetch_optional(pool)
                .await
        }
    }
    .map_err(|e| map_err("read trust policy", e))?;
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

fn uuids(ids: &[TrustMaterialId]) -> Vec<Uuid> {
    ids.iter()
        .map(rustryx_core::ids::TrustMaterialId::as_uuid)
        .collect()
}

fn scope_columns(scope: PolicyScope) -> (bool, Option<Uuid>, Option<Uuid>) {
    match scope {
        PolicyScope::System => (true, None, None),
        PolicyScope::Project(id) => (false, Some(id.as_uuid()), None),
        PolicyScope::Repository(id) => (false, None, Some(id.as_uuid())),
    }
}

fn kind_label(kind: TrustMaterialKind) -> &'static str {
    match kind {
        TrustMaterialKind::CosignPublicKey => "cosign_public_key",
        TrustMaterialKind::CosignRoot => "cosign_root",
        TrustMaterialKind::NotationCa => "notation_ca",
    }
}

fn parse_kind(raw: &str) -> Result<TrustMaterialKind, StoreError> {
    match raw {
        "cosign_public_key" => Ok(TrustMaterialKind::CosignPublicKey),
        "cosign_root" => Ok(TrustMaterialKind::CosignRoot),
        "notation_ca" => Ok(TrustMaterialKind::NotationCa),
        _ => Err(backend(
            "read trust material",
            "unknown trust material kind",
        )),
    }
}

fn required_label(required: RequiredSignatures) -> &'static str {
    match required {
        RequiredSignatures::None => "none",
        RequiredSignatures::Cosign => "cosign",
        RequiredSignatures::Notation => "notation",
        RequiredSignatures::Both => "both",
    }
}

fn parse_required(raw: &str) -> Result<RequiredSignatures, StoreError> {
    match raw {
        "none" => Ok(RequiredSignatures::None),
        "cosign" => Ok(RequiredSignatures::Cosign),
        "notation" => Ok(RequiredSignatures::Notation),
        "both" => Ok(RequiredSignatures::Both),
        _ => Err(backend(
            "read trust policy",
            "unknown signature requirement",
        )),
    }
}

fn mode_label(mode: GateMode) -> &'static str {
    match mode {
        GateMode::Enforce => "enforce",
        GateMode::AuditOnly => "audit_only",
    }
}

fn parse_mode(raw: &str) -> Result<GateMode, StoreError> {
    match raw {
        "enforce" => Ok(GateMode::Enforce),
        "audit_only" => Ok(GateMode::AuditOnly),
        _ => Err(backend("read trust policy", "unknown gate mode")),
    }
}

fn level_label(level: NotationLevel) -> &'static str {
    match level {
        NotationLevel::Strict => "strict",
        NotationLevel::Permissive => "permissive",
        NotationLevel::Audit => "audit",
    }
}

fn parse_level(raw: &str) -> Result<NotationLevel, StoreError> {
    match raw {
        "strict" => Ok(NotationLevel::Strict),
        "permissive" => Ok(NotationLevel::Permissive),
        "audit" => Ok(NotationLevel::Audit),
        _ => Err(backend("read trust policy", "unknown notation level")),
    }
}
