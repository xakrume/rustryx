//! The state changing transactions of assets: `publish_with`, `link` and `delete_by_path_checked`.
//!
//! All three run in the frame of [`super::write`] (one transaction, outbox events inside it,
//! rollback on any refusal) and share one rule about locks, which is what keeps them from
//! deadlocking with each other and with garbage collection:
//!
//! 1. the repository row, `FOR SHARE` (write policy and quota are read from it; an edit of the
//!    repository waits for the publishes in flight, publishes do not wait for each other),
//! 2. the asset row of the path, `FOR UPDATE`,
//! 3. the blob row, `FOR NO KEY UPDATE` (only `link`: a new blob of `publish_with` is
//!    invisible to everybody else until commit).
//!
//! The `assets_blob_ref` trigger takes the blob rows of an asset switch in the same order, and
//! the GC sweep ([`super::blob`]) takes a blob row only, so no cycle exists.

use rustryx_core::StoreError;
use rustryx_core::error::{MutationError, PolicyViolation};
use rustryx_core::ids::{AssetId, BlobId, RepositoryId};
use rustryx_core::model::{
    BlobRecord, NewAsset, NewAssetLink, PublishMode, PublishOutcome, WriteOptions, WritePolicy,
};
use rustryx_core::quota::{QuotaExceeded, QuotaScope};
use sqlx::{PgConnection, PgPool};
use uuid::Uuid;

use super::common::to_i64;
use super::repository::parse_write_policy;
use super::rows::{AssetRow, BlobRow, asset_select};
use super::write;
use crate::error::map_mutation_err;

/// Upper bound of attempts when a concurrent publisher keeps winning the insert of a new path.
const PUBLISH_ATTEMPTS: u32 = 5;

/// What the repository row says about writing to it.
struct RepositoryGuard {
    write_policy: WritePolicy,
    quota_bytes: Option<i64>,
    blob_store_id: Uuid,
}

/// The asset to create or switch, and the blob it points at.
struct Switch<'a> {
    repository_id: RepositoryId,
    component_id: Option<Uuid>,
    path: &'a str,
    content_type: &'a str,
    created_by: Option<Uuid>,
    blob_id: Uuid,
    blob_size: i64,
}

/// An asset row that was created or switched.
struct Switched {
    asset_id: Uuid,
    replaced_blob: Option<Uuid>,
}

/// Locks the repository row and applies the checks that do not depend on the path.
///
/// A repository with a quota also takes a transaction advisory lock named after it: the quota
/// check below is a read of the usage followed by a write, and two publishes that both read
/// the old usage would both pass. The lock is taken only for repositories that have a quota,
/// so the common publish does not serialize. (Project quotas and counters come with the
/// lifecycle stream, which replaces the sum below by a conditional counter update.)
async fn lock_repository(
    conn: &mut PgConnection,
    repository_id: RepositoryId,
) -> Result<RepositoryGuard, MutationError> {
    let row: Option<(String, Option<i64>, Uuid)> = sqlx::query_as(
        "SELECT write_policy, quota_bytes, blob_store_id FROM repositories \
         WHERE id = $1 FOR SHARE",
    )
    .bind(repository_id.as_uuid())
    .fetch_optional(&mut *conn)
    .await
    .map_err(|e| map_mutation_err("lock repository", e))?;
    let Some((policy, quota_bytes, blob_store_id)) = row else {
        return Err(StoreError::NotFound("repository".to_owned()).into());
    };
    if quota_bytes.is_some() {
        sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 0))")
            .bind(format!("rustryx.repository_quota:{repository_id}"))
            .execute(&mut *conn)
            .await
            .map_err(|e| map_mutation_err("lock repository quota", e))?;
    }
    Ok(RepositoryGuard {
        write_policy: parse_write_policy(&policy)?,
        quota_bytes,
        blob_store_id,
    })
}

/// The refusal of the write policy and the mode for a path that does or does not exist yet.
///
/// `Deny` refuses everything, `PublishMode::CreateOnly` refuses an existing path (a proxy cache
/// fill never replaces what was published meanwhile, across nodes: the existing row is read
/// under its lock), `AllowOnce` refuses an existing path too. `CreateOnly` is checked first
/// because it is the more specific answer for the one caller that sets it.
fn check_policy(
    policy: WritePolicy,
    mode: PublishMode,
    path_exists: bool,
) -> Result<(), PolicyViolation> {
    if policy == WritePolicy::Deny {
        return Err(PolicyViolation::WritePolicy(WritePolicy::Deny));
    }
    if path_exists {
        if mode == PublishMode::CreateOnly {
            return Err(PolicyViolation::AlreadyExists);
        }
        if policy == WritePolicy::AllowOnce {
            return Err(PolicyViolation::WritePolicy(WritePolicy::AllowOnce));
        }
    }
    Ok(())
}

async fn insert_blob(conn: &mut PgConnection, blob: &BlobRecord) -> Result<(), MutationError> {
    sqlx::query(
        "INSERT INTO blobs (id, blob_store_id, key, size_bytes, sha1, sha256, md5, created_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
    )
    .bind(blob.id.as_uuid())
    .bind(blob.blob_store_id.as_uuid())
    .bind(&blob.key)
    .bind(to_i64(blob.size_bytes)?)
    .bind(&blob.checksums.sha1)
    .bind(&blob.checksums.sha256)
    .bind(&blob.checksums.md5)
    .bind(blob.created_at)
    .execute(conn)
    .await
    .map_err(|e| map_mutation_err("insert blob", e))?;
    Ok(())
}

/// Locks the blob row of a `link` and returns `(store, size)`, or `NotFound` when the row is
/// gone (a sweep got there first). `FOR NO KEY UPDATE` conflicts with the sweep's `FOR UPDATE`
/// and with another link of the same blob, and not with the foreign key checks of unrelated
/// inserts. A share lock would deadlock two links of one orphaned blob: both would hold it and
/// both would then need the row lock of the `assets_blob_ref` trigger.
async fn lock_blob(conn: &mut PgConnection, blob_id: BlobId) -> Result<(Uuid, i64), MutationError> {
    let row: Option<(Uuid, i64)> = sqlx::query_as(
        "SELECT blob_store_id, size_bytes FROM blobs WHERE id = $1 FOR NO KEY UPDATE",
    )
    .bind(blob_id.as_uuid())
    .fetch_optional(conn)
    .await
    .map_err(|e| map_mutation_err("lock blob", e))?;
    row.ok_or_else(|| StoreError::NotFound("blob".to_owned()).into())
}

/// Points the asset at the blob, creating it when the path is new.
///
/// `SELECT .. FOR UPDATE` first, then `UPDATE` or `INSERT .. ON CONFLICT DO NOTHING`:
/// - a concurrent publisher of the same path blocks on the row lock and, when it wakes, reads
///   the row the winner committed, so each publisher reports exactly the blob IT displaced
///   (a single `INSERT .. ON CONFLICT DO UPDATE` cannot return the old value), and the policy
///   check runs against the committed state, which is what makes `allow_once` and
///   `CreateOnly` hold across nodes;
/// - for a new path nothing can be locked, so two publishers may both reach the insert; the
///   loser's `DO NOTHING` returns no row and the loop retries into the update branch.
///
/// `lock_link_blob` makes the loop take the blob row lock after the asset row lock (order in
/// the module documentation) and refuse a blob of another store.
async fn switch_asset(
    conn: &mut PgConnection,
    guard: &RepositoryGuard,
    spec: &Switch<'_>,
    mode: PublishMode,
    lock_link_blob: Option<BlobId>,
) -> Result<Switched, MutationError> {
    for _ in 0..PUBLISH_ATTEMPTS {
        let existing: Option<(Uuid, Uuid)> = sqlx::query_as(
            "SELECT id, blob_id FROM assets WHERE repository_id = $1 AND path = $2 FOR UPDATE",
        )
        .bind(spec.repository_id.as_uuid())
        .bind(spec.path)
        .fetch_optional(&mut *conn)
        .await
        .map_err(|e| map_mutation_err("lock asset", e))?;
        check_policy(guard.write_policy, mode, existing.is_some())?;
        if let Some(blob_id) = lock_link_blob {
            let (store, _) = lock_blob(conn, blob_id).await?;
            if store != guard.blob_store_id {
                return Err(StoreError::Conflict(
                    "the blob is in another blob store than the repository".to_owned(),
                )
                .into());
            }
        }
        if let Some((id, old_blob)) = existing {
            sqlx::query(
                "UPDATE assets SET blob_id = $2, content_type = $3, component_id = $4, \
                 created_by = $5, updated_at = now() WHERE id = $1",
            )
            .bind(id)
            .bind(spec.blob_id)
            .bind(spec.content_type)
            .bind(spec.component_id)
            .bind(spec.created_by)
            .execute(&mut *conn)
            .await
            .map_err(|e| map_mutation_err("switch asset blob", e))?;
            return Ok(Switched {
                asset_id: id,
                replaced_blob: Some(old_blob),
            });
        }
        let inserted: Option<Uuid> = sqlx::query_scalar(
            "INSERT INTO assets (id, repository_id, component_id, path, blob_id, content_type, \
             created_by) VALUES ($1, $2, $3, $4, $5, $6, $7) \
             ON CONFLICT ON CONSTRAINT uq_assets_repo_path DO NOTHING RETURNING id",
        )
        .bind(AssetId::generate().as_uuid())
        .bind(spec.repository_id.as_uuid())
        .bind(spec.component_id)
        .bind(spec.path)
        .bind(spec.blob_id)
        .bind(spec.content_type)
        .bind(spec.created_by)
        .fetch_optional(&mut *conn)
        .await
        .map_err(|e| map_mutation_err("insert asset", e))?;
        if let Some(asset_id) = inserted {
            return Ok(Switched {
                asset_id,
                replaced_blob: None,
            });
        }
    }
    Err(StoreError::Unavailable("publish kept losing a race, retry".to_owned()).into())
}

/// Whether some asset of the repository, other than `except`, still points at the blob.
async fn referenced_in_repository(
    conn: &mut PgConnection,
    repository_id: RepositoryId,
    blob_id: Uuid,
    except: Option<Uuid>,
) -> Result<bool, MutationError> {
    let found: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM assets WHERE repository_id = $1 AND blob_id = $2 \
         AND ($3::uuid IS NULL OR id <> $3))",
    )
    .bind(repository_id.as_uuid())
    .bind(blob_id)
    .bind(except)
    .fetch_one(conn)
    .await
    .map_err(|e| map_mutation_err("check blob references", e))?;
    Ok(found)
}

/// The quota of the repository, checked on the ACTUAL size after the switch and inside the
/// transaction that makes the asset visible (the pre check on the declared length is advisory
/// and lives elsewhere).
///
/// Usage is the sum over DISTINCT blobs of the repository (a blob shared by two assets
/// counts once). The delta of this write is `+size` when no other asset of the repository
/// holds the new blob, minus the old blob's size when the replaced blob has no holder left.
/// A write that does not grow the repository (`delta <= 0`) is never refused, so an
/// overquota repository can still be shrunk and an idempotent re-publish never fails. This is
/// the phase 1 sum the design keeps as the authority until the counters of the lifecycle
/// stream are ready; the sum is a scan of the repository, which the advisory lock of
/// [`lock_repository`] makes exact and which only repositories WITH a quota pay.
async fn enforce_repository_quota(
    conn: &mut PgConnection,
    repository_id: RepositoryId,
    quota: i64,
    spec: &Switch<'_>,
    switched: &Switched,
) -> Result<(), MutationError> {
    let mut delta = 0_i64;
    if !referenced_in_repository(conn, repository_id, spec.blob_id, Some(switched.asset_id)).await?
    {
        delta += spec.blob_size;
    }
    if let Some(old) = switched.replaced_blob
        && old != spec.blob_id
        && !referenced_in_repository(conn, repository_id, old, None).await?
    {
        let old_size: i64 = sqlx::query_scalar("SELECT size_bytes FROM blobs WHERE id = $1")
            .bind(old)
            .fetch_one(&mut *conn)
            .await
            .map_err(|e| map_mutation_err("read replaced blob size", e))?;
        delta -= old_size;
    }
    if delta <= 0 {
        return Ok(());
    }
    let used_after: i64 = sqlx::query_scalar(
        "SELECT COALESCE(SUM(b.size_bytes), 0)::bigint FROM blobs b \
         WHERE b.id IN (SELECT a.blob_id FROM assets a WHERE a.repository_id = $1)",
    )
    .bind(repository_id.as_uuid())
    .fetch_one(&mut *conn)
    .await
    .map_err(|e| map_mutation_err("sum repository usage", e))?;
    if used_after <= quota {
        return Ok(());
    }
    Err(PolicyViolation::QuotaExceeded(QuotaExceeded {
        scope: QuotaScope::Repository(repository_id),
        limit_bytes: u64::try_from(quota).unwrap_or(0),
        used_bytes: u64::try_from(used_after - delta).unwrap_or(0),
        requested_bytes: u64::try_from(delta).unwrap_or(0),
    })
    .into())
}

/// The outcome of a switch: the asset as it is now, and the displaced blob only when it lost
/// its last reference (the same rule as `delete_by_path`). With `link` two assets can share a
/// blob, and returning a still referenced blob as "to be garbage collected" would invite a
/// caller to delete an object that another asset serves. GC re-checks anyway, this keeps the
/// hint honest.
async fn outcome(
    conn: &mut PgConnection,
    switched: &Switched,
    new_blob: Uuid,
) -> Result<PublishOutcome, MutationError> {
    let asset = sqlx::query_as::<_, AssetRow>(asset_select!("WHERE a.id = $1"))
        .bind(switched.asset_id)
        .fetch_one(&mut *conn)
        .await
        .map_err(|e| map_mutation_err("read published asset", e))?
        .into_asset()?;
    let replaced = match switched.replaced_blob {
        Some(old) if old != new_blob => freed_blob(conn, old).await?,
        _ => None,
    };
    let created = switched.replaced_blob.is_none();
    Ok(PublishOutcome::new(asset, replaced, created))
}

/// The blob row when no asset references it any more.
async fn freed_blob(
    conn: &mut PgConnection,
    blob_id: Uuid,
) -> Result<Option<BlobRecord>, MutationError> {
    let record = sqlx::query_as::<_, BlobRow>(
        "SELECT b.id, b.blob_store_id, b.key, b.size_bytes, b.sha1, b.sha256, b.md5, b.created_at \
         FROM blobs b WHERE b.id = $1 AND NOT EXISTS (SELECT 1 FROM assets a WHERE a.blob_id = b.id)",
    )
    .bind(blob_id)
    .fetch_optional(conn)
    .await
    .map_err(|e| map_mutation_err("read freed blob", e))?
    .map(BlobRow::into_record)
    .transpose()?;
    Ok(record)
}

/// `AssetRepo::publish_with`.
pub(super) async fn publish(
    pool: &PgPool,
    new: &NewAsset,
    options: &WriteOptions,
) -> Result<PublishOutcome, MutationError> {
    const CONTEXT: &str = "publish asset";
    let blob_size = to_i64(new.blob.size_bytes)?;
    let mut tx = write::begin(pool, options, CONTEXT).await?;
    let guard = lock_repository(&mut tx, new.repository_id).await?;
    // Refused before any write when the policy forbids every write; the existing path checks
    // run inside `switch_asset` under the asset lock.
    check_policy(guard.write_policy, PublishMode::Upsert, false)?;
    insert_blob(&mut tx, &new.blob).await?;
    let spec = Switch {
        repository_id: new.repository_id,
        component_id: new.component_id.map(|c| c.as_uuid()),
        path: &new.path,
        content_type: &new.content_type,
        created_by: new.created_by.map(|u| u.as_uuid()),
        blob_id: new.blob.id.as_uuid(),
        blob_size,
    };
    let switched = switch_asset(&mut tx, &guard, &spec, options.mode, None).await?;
    if let Some(quota) = guard.quota_bytes {
        enforce_repository_quota(&mut tx, new.repository_id, quota, &spec, &switched).await?;
    }
    let result = outcome(&mut tx, &switched, spec.blob_id).await?;
    write::commit(tx, options, CONTEXT).await?;
    Ok(result)
}

/// `AssetRepo::link`.
pub(super) async fn link(
    pool: &PgPool,
    link: &NewAssetLink,
    options: &WriteOptions,
) -> Result<PublishOutcome, MutationError> {
    const CONTEXT: &str = "link asset";
    let mut tx = write::begin(pool, options, CONTEXT).await?;
    let guard = lock_repository(&mut tx, link.repository_id).await?;
    check_policy(guard.write_policy, PublishMode::Upsert, false)?;
    // The size is read under the blob lock inside the loop; `Switch` needs it for the quota
    // delta, so a first, unlocked read supplies it and the locked read in `switch_asset`
    // is the one that decides existence. A blob row is immutable, so the size cannot differ.
    let size: Option<i64> = sqlx::query_scalar("SELECT size_bytes FROM blobs WHERE id = $1")
        .bind(link.blob_id.as_uuid())
        .fetch_optional(&mut *tx)
        .await
        .map_err(|e| map_mutation_err("read blob", e))?;
    let Some(blob_size) = size else {
        return Err(StoreError::NotFound("blob".to_owned()).into());
    };
    let spec = Switch {
        repository_id: link.repository_id,
        component_id: link.component_id.map(|c| c.as_uuid()),
        path: &link.path,
        content_type: &link.content_type,
        created_by: link.created_by.map(|u| u.as_uuid()),
        blob_id: link.blob_id.as_uuid(),
        blob_size,
    };
    let switched = switch_asset(&mut tx, &guard, &spec, options.mode, Some(link.blob_id)).await?;
    if let Some(quota) = guard.quota_bytes {
        enforce_repository_quota(&mut tx, link.repository_id, quota, &spec, &switched).await?;
    }
    let result = outcome(&mut tx, &switched, spec.blob_id).await?;
    write::commit(tx, options, CONTEXT).await?;
    Ok(result)
}

/// `AssetRepo::delete_by_path_checked`: the blob that lost its last reference, `None` for a
/// missing path (idempotent, nothing is written, no event either).
pub(super) async fn delete_by_path(
    pool: &PgPool,
    repository_id: RepositoryId,
    path: &str,
    options: &WriteOptions,
) -> Result<Option<BlobRecord>, MutationError> {
    const CONTEXT: &str = "delete asset";
    let mut tx = write::begin(pool, options, CONTEXT).await?;
    let deleted: Option<Uuid> = sqlx::query_scalar(
        "DELETE FROM assets WHERE repository_id = $1 AND path = $2 RETURNING blob_id",
    )
    .bind(repository_id.as_uuid())
    .bind(path)
    .fetch_optional(&mut *tx)
    .await
    .map_err(|e| map_mutation_err("delete asset", e))?;
    let Some(blob_id) = deleted else {
        return Ok(None);
    };
    let freed = freed_blob(&mut tx, blob_id).await?;
    write::commit(tx, options, CONTEXT).await?;
    Ok(freed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn policy_matrix() {
        use PolicyViolation::{AlreadyExists, WritePolicy as Wp};
        let upsert = PublishMode::Upsert;
        let create = PublishMode::CreateOnly;
        // Allow: only create only refuses, and only an existing path.
        assert_eq!(check_policy(WritePolicy::Allow, upsert, true), Ok(()));
        assert_eq!(check_policy(WritePolicy::Allow, upsert, false), Ok(()));
        assert_eq!(
            check_policy(WritePolicy::Allow, create, true),
            Err(AlreadyExists)
        );
        assert_eq!(check_policy(WritePolicy::Allow, create, false), Ok(()));
        // AllowOnce: an existing path is refused in both modes, CreateOnly names itself.
        assert_eq!(
            check_policy(WritePolicy::AllowOnce, upsert, true),
            Err(Wp(WritePolicy::AllowOnce))
        );
        assert_eq!(
            check_policy(WritePolicy::AllowOnce, create, true),
            Err(AlreadyExists)
        );
        assert_eq!(check_policy(WritePolicy::AllowOnce, upsert, false), Ok(()));
        // Deny refuses everything.
        for mode in [upsert, create] {
            for exists in [true, false] {
                assert_eq!(
                    check_policy(WritePolicy::Deny, mode, exists),
                    Err(Wp(WritePolicy::Deny))
                );
            }
        }
    }
}
