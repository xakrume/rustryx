//! [`BlobStoreMemberRepo`] on PostgreSQL.

use async_trait::async_trait;
use rustryx_core::StoreError;
use rustryx_core::ids::BlobStoreId;
use rustryx_core::model::{BlobStoreDef, BlobStoreKind};
use rustryx_core::ports::{BlobStoreMemberRepo, InvalidationEvent};
use rustryx_core::storage::{BlobStoreMember, MemberState};
use sqlx::{FromRow, PgConnection, PgPool};
use uuid::Uuid;

use super::common::{corrupt, to_i64};
use crate::error::map_err;

/// Members of group blob stores.
#[derive(Debug, Clone)]
pub struct PgBlobStoreMemberRepo {
    pool: PgPool,
}

impl PgBlobStoreMemberRepo {
    /// Creates the repository over `pool`.
    #[must_use]
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[derive(Debug, FromRow)]
struct Row {
    group_id: Uuid,
    member_id: Uuid,
    position: i16,
    state: String,
}

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

fn state_label(state: MemberState) -> &'static str {
    match state {
        MemberState::Active => "active",
        MemberState::Draining => "draining",
    }
}

fn parse_state(raw: &str) -> Result<MemberState, StoreError> {
    match raw {
        "active" => Ok(MemberState::Active),
        "draining" => Ok(MemberState::Draining),
        _ => Err(corrupt("read blob store member")),
    }
}

fn position_column(position: u16) -> Result<i16, StoreError> {
    i16::try_from(position)
        .map_err(|_| StoreError::Invalid("member position is out of range".to_owned()))
}

/// Soft quota and the structured group field have no column. Dropping them silently would
/// let a caller believe they were saved, same as [`super::blob_store`].
fn reject_unpersisted(store: &BlobStoreDef) -> Result<(), StoreError> {
    if store.soft_quota.is_some() || store.group.is_some() {
        return Err(StoreError::Invalid(
            "soft quota and group settings are not supported by this schema version".to_owned(),
        ));
    }
    Ok(())
}

impl Row {
    fn into_member(self) -> Result<BlobStoreMember, StoreError> {
        Ok(BlobStoreMember {
            group_id: BlobStoreId::from_uuid(self.group_id),
            member_id: BlobStoreId::from_uuid(self.member_id),
            position: u16::try_from(self.position)
                .map_err(|_| corrupt("read blob store member"))?,
            state: parse_state(&self.state)?,
        })
    }
}

async fn begin<'a>(
    pool: &'a PgPool,
    context: &'static str,
) -> Result<sqlx::Transaction<'a, sqlx::Postgres>, StoreError> {
    pool.begin().await.map_err(|e| map_err(context, e))
}

async fn finish(
    tx: sqlx::Transaction<'_, sqlx::Postgres>,
    context: &'static str,
) -> Result<(), StoreError> {
    tx.commit().await.map_err(|e| map_err(context, e))
}

async fn guard_conversion(
    tx: &mut PgConnection,
    existing: BlobStoreId,
    group: &BlobStoreDef,
) -> Result<(), StoreError> {
    let kind: Option<String> =
        sqlx::query_scalar("SELECT kind FROM blob_stores WHERE id = $1 FOR UPDATE")
            .bind(existing.as_uuid())
            .fetch_optional(&mut *tx)
            .await
            .map_err(|e| map_err("convert blob store to group", e))?;
    let Some(kind) = kind else {
        return Err(StoreError::NotFound("blob store".to_owned()));
    };
    if kind == "group" || group.kind != BlobStoreKind::Group {
        return Err(StoreError::Conflict(
            "a plain store is converted into a group store".to_owned(),
        ));
    }
    let already: bool =
        sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM blob_store_members WHERE member_id = $1)")
            .bind(existing.as_uuid())
            .fetch_one(&mut *tx)
            .await
            .map_err(|e| map_err("convert blob store to group", e))?;
    if already {
        return Err(StoreError::Conflict(
            "the store already belongs to a group".to_owned(),
        ));
    }
    let taken: bool =
        sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM blob_stores WHERE id = $1 OR name = $2)")
            .bind(group.id.as_uuid())
            .bind(&group.name)
            .fetch_one(&mut *tx)
            .await
            .map_err(|e| map_err("convert blob store to group", e))?;
    if taken {
        return Err(StoreError::Conflict(format!(
            "blob store {} already exists",
            group.name
        )));
    }
    reject_unpersisted(group)?;
    Ok(())
}

#[async_trait]
impl BlobStoreMemberRepo for PgBlobStoreMemberRepo {
    async fn members(&self, group: BlobStoreId) -> Result<Vec<BlobStoreMember>, StoreError> {
        sqlx::query_as::<_, Row>(
            "SELECT group_id, member_id, position, state FROM blob_store_members \
             WHERE group_id = $1 ORDER BY position, member_id",
        )
        .bind(group.as_uuid())
        .fetch_all(&self.pool)
        .await
        .map_err(|e| map_err("list blob store members", e))?
        .into_iter()
        .map(Row::into_member)
        .collect()
    }

    async fn add_member(
        &self,
        group: BlobStoreId,
        member: BlobStoreId,
        position: u16,
    ) -> Result<(), StoreError> {
        let position = position_column(position)?;
        let mut tx = begin(&self.pool, "add blob store member").await?;
        sqlx::query(
            "INSERT INTO blob_store_members (group_id, member_id, position, state) \
             VALUES ($1, $2, $3, 'active')",
        )
        .bind(group.as_uuid())
        .bind(member.as_uuid())
        .bind(position)
        .execute(&mut *tx)
        .await
        .map_err(|e| map_err("add blob store member", e))?;
        super::common::notify(&mut tx, &InvalidationEvent::Reset).await?;
        finish(tx, "add blob store member").await
    }

    async fn set_state(
        &self,
        group: BlobStoreId,
        member: BlobStoreId,
        state: MemberState,
    ) -> Result<(), StoreError> {
        let mut tx = begin(&self.pool, "set blob store member state").await?;
        let updated = sqlx::query(
            "UPDATE blob_store_members SET state = $3 WHERE group_id = $1 AND member_id = $2",
        )
        .bind(group.as_uuid())
        .bind(member.as_uuid())
        .bind(state_label(state))
        .execute(&mut *tx)
        .await
        .map_err(|e| map_err("set blob store member state", e))?;
        if updated.rows_affected() == 0 {
            return Err(StoreError::NotFound("blob store member".to_owned()));
        }
        super::common::notify(&mut tx, &InvalidationEvent::Reset).await?;
        finish(tx, "set blob store member state").await
    }

    async fn remove_member(
        &self,
        group: BlobStoreId,
        member: BlobStoreId,
    ) -> Result<(), StoreError> {
        let mut tx = begin(&self.pool, "remove blob store member").await?;
        let ids: Vec<Uuid> = sqlx::query_scalar(
            "SELECT member_id FROM blob_store_members WHERE group_id = $1 FOR UPDATE",
        )
        .bind(group.as_uuid())
        .fetch_all(&mut *tx)
        .await
        .map_err(|e| map_err("remove blob store member", e))?;
        if !ids.contains(&member.as_uuid()) {
            return Err(StoreError::NotFound("blob store member".to_owned()));
        }
        let holds: bool =
            sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM blobs WHERE blob_store_id = $1)")
                .bind(member.as_uuid())
                .fetch_one(&mut *tx)
                .await
                .map_err(|e| map_err("remove blob store member", e))?;
        if holds {
            return Err(StoreError::Conflict(
                "the member still holds blobs".to_owned(),
            ));
        }
        let used: bool = sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM repositories WHERE blob_store_id = $1)",
        )
        .bind(group.as_uuid())
        .fetch_one(&mut *tx)
        .await
        .map_err(|e| map_err("remove blob store member", e))?;
        if ids.len() == 1 && used {
            return Err(StoreError::Conflict(
                "a group used by repositories needs a member".to_owned(),
            ));
        }
        sqlx::query("DELETE FROM blob_store_members WHERE group_id = $1 AND member_id = $2")
            .bind(group.as_uuid())
            .bind(member.as_uuid())
            .execute(&mut *tx)
            .await
            .map_err(|e| map_err("remove blob store member", e))?;
        super::common::notify(&mut tx, &InvalidationEvent::Reset).await?;
        finish(tx, "remove blob store member").await
    }

    async fn convert_to_group(
        &self,
        existing: BlobStoreId,
        group: &BlobStoreDef,
    ) -> Result<(), StoreError> {
        let kind = kind_label(group.kind)?;
        let quota = group.quota_bytes.map(to_i64).transpose()?;
        let mut tx = begin(&self.pool, "convert blob store to group").await?;
        guard_conversion(&mut tx, existing, group).await?;
        sqlx::query(
            "INSERT INTO blob_stores (id, name, kind, config, quota_bytes, created_at) \
             VALUES ($1, $2, $3, $4, $5, $6)",
        )
        .bind(group.id.as_uuid())
        .bind(&group.name)
        .bind(kind)
        .bind(&group.config)
        .bind(quota)
        .bind(group.created_at)
        .execute(&mut *tx)
        .await
        .map_err(|e| map_err("convert blob store to group", e))?;
        sqlx::query(
            "INSERT INTO blob_store_members (group_id, member_id, position, state) \
             VALUES ($1, $2, 0, 'active')",
        )
        .bind(group.id.as_uuid())
        .bind(existing.as_uuid())
        .execute(&mut *tx)
        .await
        .map_err(|e| map_err("convert blob store to group", e))?;
        sqlx::query("UPDATE repositories SET blob_store_id = $1 WHERE blob_store_id = $2")
            .bind(group.id.as_uuid())
            .bind(existing.as_uuid())
            .execute(&mut *tx)
            .await
            .map_err(|e| map_err("convert blob store to group", e))?;
        super::common::notify(&mut tx, &InvalidationEvent::Reset).await?;
        finish(tx, "convert blob store to group").await
    }

    async fn group_of(&self, member: BlobStoreId) -> Result<Option<BlobStoreId>, StoreError> {
        let group: Option<Uuid> =
            sqlx::query_scalar("SELECT group_id FROM blob_store_members WHERE member_id = $1")
                .bind(member.as_uuid())
                .fetch_optional(&self.pool)
                .await
                .map_err(|e| map_err("read blob store group", e))?;
        Ok(group.map(BlobStoreId::from_uuid))
    }
}
