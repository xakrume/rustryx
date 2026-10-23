//! [`BlobStoreRepo`] on `PostgreSQL`.

use async_trait::async_trait;
use rustryx_core::StoreError;
use rustryx_core::ids::BlobStoreId;
use rustryx_core::model::{BlobStoreDef, BlobStoreKind};
use rustryx_core::ports::{BlobStoreRepo, InvalidationEvent};
use sqlx::{FromRow, PgPool};
use time::OffsetDateTime;
use uuid::Uuid;

use super::common::{corrupt, notify, to_i64, to_u64};
use crate::error::map_err;

/// Static `SELECT` with a literal tail. `concat!` keeps the text `'static`, which is what
/// `sqlx` 0.9 demands of anything that is not explicitly asserted safe.
macro_rules! select {
    ($($tail:literal),*) => {
        concat!(
            "SELECT id, name, kind, config, quota_bytes, created_at FROM blob_stores ",
            $($tail),*
        )
    };
}

/// Blob store definitions.
#[derive(Debug, Clone)]
pub struct PgBlobStoreRepo {
    pool: PgPool,
}

impl PgBlobStoreRepo {
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
    kind: String,
    config: serde_json::Value,
    quota_bytes: Option<i64>,
    created_at: OffsetDateTime,
}

/// The stored text of a kind. The database CHECK still lists `local` and `s3` only (it widens
/// with the storage stream, behind the `BlobGroupsAndAzure` gate), so a store of another kind
/// is refused by the constraint until then. A kind this build does not know is an error, never
/// a default: writing the wrong kind would send reads to the wrong backend.
fn kind_label(kind: BlobStoreKind) -> Result<&'static str, StoreError> {
    match kind {
        BlobStoreKind::Local => Ok("local"),
        BlobStoreKind::S3 => Ok("s3"),
        BlobStoreKind::Azure => Ok("azure"),
        BlobStoreKind::Group => Ok("group"),
        _ => Err(StoreError::Invalid(
            "unsupported blob store kind".to_owned(),
        )),
    }
}

/// Soft quota and group settings have no column yet (they arrive with the storage and the
/// lifecycle streams). Dropping them silently would let a caller believe they were saved, so
/// a definition that carries them is refused.
fn reject_unpersisted(store: &BlobStoreDef) -> Result<(), StoreError> {
    if store.soft_quota.is_some() || store.group.is_some() {
        return Err(StoreError::Invalid(
            "soft quota and group settings are not supported by this schema version".to_owned(),
        ));
    }
    Ok(())
}

impl Row {
    fn into_def(self) -> Result<BlobStoreDef, StoreError> {
        let kind = match self.kind.as_str() {
            "local" => BlobStoreKind::Local,
            "s3" => BlobStoreKind::S3,
            "azure" => BlobStoreKind::Azure,
            "group" => BlobStoreKind::Group,
            _ => return Err(corrupt("read blob store")),
        };
        let mut def = BlobStoreDef::new(
            BlobStoreId::from_uuid(self.id),
            self.name,
            kind,
            self.config,
            self.created_at,
        );
        def.quota_bytes = self
            .quota_bytes
            .map(|q| to_u64(q, "read blob store"))
            .transpose()?;
        Ok(def)
    }
}

#[async_trait]
impl BlobStoreRepo for PgBlobStoreRepo {
    async fn create(&self, store: &BlobStoreDef) -> Result<(), StoreError> {
        reject_unpersisted(store)?;
        let kind = kind_label(store.kind)?;
        let quota = store.quota_bytes.map(to_i64).transpose()?;
        sqlx::query(
            "INSERT INTO blob_stores (id, name, kind, config, quota_bytes, created_at) \
             VALUES ($1, $2, $3, $4, $5, $6)",
        )
        .bind(store.id.as_uuid())
        .bind(&store.name)
        .bind(kind)
        .bind(&store.config)
        .bind(quota)
        .bind(store.created_at)
        .execute(&self.pool)
        .await
        .map_err(|e| map_err("create blob store", e))?;
        Ok(())
    }

    async fn get(&self, id: BlobStoreId) -> Result<Option<BlobStoreDef>, StoreError> {
        sqlx::query_as::<_, Row>(select!("WHERE id = $1"))
            .bind(id.as_uuid())
            .fetch_optional(&self.pool)
            .await
            .map_err(|e| map_err("read blob store", e))?
            .map(Row::into_def)
            .transpose()
    }

    async fn get_by_name(&self, name: &str) -> Result<Option<BlobStoreDef>, StoreError> {
        sqlx::query_as::<_, Row>(select!("WHERE name = $1"))
            .bind(name)
            .fetch_optional(&self.pool)
            .await
            .map_err(|e| map_err("read blob store", e))?
            .map(Row::into_def)
            .transpose()
    }

    async fn list(&self) -> Result<Vec<BlobStoreDef>, StoreError> {
        sqlx::query_as::<_, Row>(select!("ORDER BY name"))
            .fetch_all(&self.pool)
            .await
            .map_err(|e| map_err("list blob stores", e))?
            .into_iter()
            .map(Row::into_def)
            .collect()
    }

    /// Replaces `config` and `quota_bytes`. `name` and `kind` say what the store IS (every blob
    /// key and every repository points at it), so the statement matches on them and a caller
    /// that changed one gets `Conflict` instead of a silent rewrite; `name` also stays out of
    /// the `SET` list on purpose, because a unique index column there turns the row lock into
    /// a KEY lock that blocks the foreign key checks of concurrent blob inserts (db.md
    /// section 6). Other nodes learn of the change through a `Reset` on the bus: the registry
    /// cache must rebuild the store, and there is no finer grained event for it yet.
    async fn update(&self, store: &BlobStoreDef) -> Result<(), StoreError> {
        reject_unpersisted(store)?;
        let kind = kind_label(store.kind)?;
        let quota = store.quota_bytes.map(to_i64).transpose()?;
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| map_err("update blob store", e))?;
        let result = sqlx::query(
            "UPDATE blob_stores SET config = $4, quota_bytes = $5 \
             WHERE id = $1 AND name = $2 AND kind = $3",
        )
        .bind(store.id.as_uuid())
        .bind(&store.name)
        .bind(kind)
        .bind(&store.config)
        .bind(quota)
        .execute(&mut *tx)
        .await
        .map_err(|e| map_err("update blob store", e))?;
        if result.rows_affected() == 0 {
            let exists: bool =
                sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM blob_stores WHERE id = $1)")
                    .bind(store.id.as_uuid())
                    .fetch_one(&mut *tx)
                    .await
                    .map_err(|e| map_err("update blob store", e))?;
            return Err(if exists {
                StoreError::Conflict("name and kind of a blob store cannot change".to_owned())
            } else {
                StoreError::NotFound("blob store".to_owned())
            });
        }
        notify(&mut tx, &InvalidationEvent::Reset).await?;
        tx.commit()
            .await
            .map_err(|e| map_err("update blob store", e))
    }

    /// `Conflict` while a repository or a blob still refers to the store (foreign keys are
    /// `RESTRICT`); `NotFound` when absent.
    async fn delete(&self, id: BlobStoreId) -> Result<(), StoreError> {
        let result = sqlx::query("DELETE FROM blob_stores WHERE id = $1")
            .bind(id.as_uuid())
            .execute(&self.pool)
            .await
            .map_err(|e| map_err("delete blob store", e))?;
        if result.rows_affected() == 0 {
            return Err(StoreError::NotFound("blob store".to_owned()));
        }
        Ok(())
    }
}
