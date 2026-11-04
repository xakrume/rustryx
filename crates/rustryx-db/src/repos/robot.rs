//! [`RobotTokenRepo`] on PostgreSQL.

use std::time::Duration;

use async_trait::async_trait;
use rustryx_core::StoreError;
use rustryx_core::ids::{ProjectId, RobotTokenId, UserId};
use rustryx_core::ports::{Page, PageRequest, RobotFilter, RobotState, RobotTokenRepo};
use rustryx_core::robot::RobotToken;
use sqlx::{FromRow, PgPool};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::cursor;
use crate::error::map_err;

const PAGE: &str = "robot";

macro_rules! select {
    ($($tail:literal),*) => {
        concat!(
            "SELECT id, name, short_name, project_id, scopes, token_hash, display_prefix, \
             expires_at, disabled_at, revoked_at, last_used_at, rotated_at, created_by, \
             created_at, version FROM robot_tokens ",
            $($tail),*
        )
    };
}

/// Robot tokens. The secret is never stored, only its hash.
#[derive(Debug, Clone)]
pub struct PgRobotTokenRepo {
    pool: PgPool,
}

impl PgRobotTokenRepo {
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
    short_name: String,
    project_id: Option<Uuid>,
    scopes: Vec<String>,
    token_hash: String,
    display_prefix: String,
    expires_at: Option<OffsetDateTime>,
    disabled_at: Option<OffsetDateTime>,
    revoked_at: Option<OffsetDateTime>,
    last_used_at: Option<OffsetDateTime>,
    rotated_at: Option<OffsetDateTime>,
    created_by: Option<Uuid>,
    created_at: OffsetDateTime,
    version: i64,
}

impl Row {
    fn into_token(self) -> RobotToken {
        let mut token = RobotToken::new(
            RobotTokenId::from_uuid(self.id),
            self.name,
            self.short_name,
            self.scopes,
            self.token_hash,
            self.display_prefix,
            self.created_at,
        );
        token.project_id = self.project_id.map(ProjectId::from_uuid);
        token.expires_at = self.expires_at;
        token.disabled_at = self.disabled_at;
        token.revoked_at = self.revoked_at;
        token.last_used_at = self.last_used_at;
        token.rotated_at = self.rotated_at;
        token.created_by = self.created_by.map(UserId::from_uuid);
        token.version = self.version;
        token
    }
}

fn missing(id: RobotTokenId) -> StoreError {
    StoreError::NotFound(format!("robot token {id}"))
}

fn stale(expected: i64, found: i64) -> StoreError {
    StoreError::Conflict(format!("stale version: expected {expected}, found {found}"))
}

fn is_unique(err: &sqlx::Error) -> bool {
    match err {
        sqlx::Error::Database(db) => db.code().as_deref() == Some("23505"),
        _ => false,
    }
}

fn horizon(now: OffsetDateTime, within: Duration) -> OffsetDateTime {
    let delta = time::Duration::try_from(within).unwrap_or(time::Duration::MAX);
    now.saturating_add(delta)
}

/// Most final state first: revoked wins over disabled and expired.
fn state_flags(state: Option<RobotState>) -> (bool, bool, bool, bool) {
    match state {
        None => (false, false, false, false),
        Some(RobotState::Active) => (true, false, false, false),
        Some(RobotState::Disabled) => (false, true, false, false),
        Some(RobotState::Expired) => (false, false, true, false),
        Some(RobotState::Revoked) => (false, false, false, true),
    }
}

async fn require_project(pool: &PgPool, project: ProjectId) -> Result<(), StoreError> {
    let exists: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM projects WHERE id = $1)")
        .bind(project.as_uuid())
        .fetch_one(pool)
        .await
        .map_err(|e| map_err("create robot", e))?;
    if exists {
        Ok(())
    } else {
        Err(StoreError::Conflict(format!(
            "project {project} does not exist"
        )))
    }
}

async fn check_version(pool: &PgPool, id: RobotTokenId, expected: i64) -> Result<(), StoreError> {
    let found: Option<i64> = sqlx::query_scalar("SELECT version FROM robot_tokens WHERE id = $1")
        .bind(id.as_uuid())
        .fetch_optional(pool)
        .await
        .map_err(|e| map_err("read robot", e))?;
    match found {
        None => Err(missing(id)),
        Some(found) if found != expected => Err(stale(expected, found)),
        Some(_) => Ok(()),
    }
}

async fn insert_robot(pool: &PgPool, token: &RobotToken) -> Result<(), StoreError> {
    let inserted = sqlx::query(
        "INSERT INTO robot_tokens (id, name, short_name, project_id, scopes, token_hash, \
         display_prefix, expires_at, disabled_at, revoked_at, last_used_at, rotated_at, \
         created_by, created_at, version) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15)",
    )
    .bind(token.id.as_uuid())
    .bind(&token.name)
    .bind(&token.short_name)
    .bind(token.project_id.map(|id| id.as_uuid()))
    .bind(&token.scopes)
    .bind(&token.token_hash)
    .bind(&token.display_prefix)
    .bind(token.expires_at)
    .bind(token.disabled_at)
    .bind(token.revoked_at)
    .bind(token.last_used_at)
    .bind(token.rotated_at)
    .bind(token.created_by.map(|id| id.as_uuid()))
    .bind(token.created_at)
    .bind(token.version)
    .execute(pool)
    .await;
    match inserted {
        Ok(_) => Ok(()),
        Err(err) if is_unique(&err) => Err(StoreError::Conflict(
            "a robot with that name or secret already exists".to_owned(),
        )),
        Err(err) => Err(map_err("create robot", err)),
    }
}

#[async_trait]
impl RobotTokenRepo for PgRobotTokenRepo {
    async fn create(&self, token: &RobotToken) -> Result<(), StoreError> {
        if let Some(project) = token.project_id {
            require_project(&self.pool, project).await?;
        }
        insert_robot(&self.pool, token).await
    }

    async fn find_by_hash(&self, token_hash: &str) -> Result<Option<RobotToken>, StoreError> {
        Ok(sqlx::query_as::<_, Row>(select!("WHERE token_hash = $1"))
            .bind(token_hash)
            .fetch_optional(&self.pool)
            .await
            .map_err(|e| map_err("read robot", e))?
            .map(Row::into_token))
    }

    async fn get(&self, id: RobotTokenId) -> Result<Option<RobotToken>, StoreError> {
        Ok(sqlx::query_as::<_, Row>(select!("WHERE id = $1"))
            .bind(id.as_uuid())
            .fetch_optional(&self.pool)
            .await
            .map_err(|e| map_err("read robot", e))?
            .map(Row::into_token))
    }

    async fn list(
        &self,
        filter: &RobotFilter,
        page: &PageRequest,
    ) -> Result<Page<RobotToken>, StoreError> {
        list_robots(&self.pool, filter, page).await
    }

    async fn rotate(
        &self,
        id: RobotTokenId,
        new_hash: &str,
        new_prefix: &str,
        at: OffsetDateTime,
        expected_version: i64,
    ) -> Result<(), StoreError> {
        rotate_robot(&self.pool, id, new_hash, new_prefix, at, expected_version).await
    }

    async fn set_disabled(
        &self,
        id: RobotTokenId,
        disabled: bool,
        at: OffsetDateTime,
        expected_version: i64,
    ) -> Result<(), StoreError> {
        let at = disabled.then_some(at);
        let updated = sqlx::query(
            "UPDATE robot_tokens SET disabled_at = $2, version = version + 1 \
             WHERE id = $1 AND version = $3",
        )
        .bind(id.as_uuid())
        .bind(at)
        .bind(expected_version)
        .execute(&self.pool)
        .await
        .map_err(|e| map_err("disable robot", e))?;
        if updated.rows_affected() == 1 {
            Ok(())
        } else {
            check_version(&self.pool, id, expected_version).await
        }
    }

    async fn revoke(&self, id: RobotTokenId, at: OffsetDateTime) -> Result<(), StoreError> {
        let updated = sqlx::query(
            "UPDATE robot_tokens SET revoked_at = COALESCE(revoked_at, $2) WHERE id = $1",
        )
        .bind(id.as_uuid())
        .bind(at)
        .execute(&self.pool)
        .await
        .map_err(|e| map_err("revoke robot", e))?;
        if updated.rows_affected() == 1 {
            Ok(())
        } else {
            Err(missing(id))
        }
    }

    async fn delete(&self, id: RobotTokenId) -> Result<(), StoreError> {
        let deleted = sqlx::query("DELETE FROM robot_tokens WHERE id = $1")
            .bind(id.as_uuid())
            .execute(&self.pool)
            .await
            .map_err(|e| map_err("delete robot", e))?;
        if deleted.rows_affected() == 1 {
            Ok(())
        } else {
            Err(missing(id))
        }
    }

    async fn touch_last_used(
        &self,
        id: RobotTokenId,
        at: OffsetDateTime,
    ) -> Result<(), StoreError> {
        sqlx::query(
            "UPDATE robot_tokens SET last_used_at = CASE \
                 WHEN last_used_at IS NULL OR last_used_at < $2 THEN $2 \
                 ELSE last_used_at END \
             WHERE id = $1",
        )
        .bind(id.as_uuid())
        .bind(at)
        .execute(&self.pool)
        .await
        .map_err(|e| map_err("touch robot", e))?;
        Ok(())
    }

    async fn purge_expired(&self, before: OffsetDateTime) -> Result<u64, StoreError> {
        let deleted = sqlx::query(
            "DELETE FROM robot_tokens \
             WHERE (expires_at IS NOT NULL AND expires_at < $1) \
                OR (revoked_at IS NOT NULL AND revoked_at < $1)",
        )
        .bind(before)
        .execute(&self.pool)
        .await
        .map_err(|e| map_err("purge robots", e))?;
        Ok(deleted.rows_affected())
    }

    async fn list_expiring(
        &self,
        within: Duration,
        now: OffsetDateTime,
        limit: u32,
    ) -> Result<Vec<RobotToken>, StoreError> {
        let rows = sqlx::query_as::<_, Row>(select!(
            "WHERE revoked_at IS NULL AND expiry_notice_at IS NULL \
             AND expires_at > $1 AND expires_at <= $2 \
             ORDER BY expires_at, id LIMIT $3"
        ))
        .bind(now)
        .bind(horizon(now, within))
        .bind(i64::from(limit))
        .fetch_all(&self.pool)
        .await
        .map_err(|e| map_err("list expiring robots", e))?;
        Ok(rows.into_iter().map(Row::into_token).collect())
    }

    async fn mark_expiry_notified(
        &self,
        id: RobotTokenId,
        at: OffsetDateTime,
    ) -> Result<(), StoreError> {
        sqlx::query(
            "UPDATE robot_tokens SET expiry_notice_at = COALESCE(expiry_notice_at, $2) \
             WHERE id = $1",
        )
        .bind(id.as_uuid())
        .bind(at)
        .execute(&self.pool)
        .await
        .map_err(|e| map_err("mark robot expiry", e))?;
        Ok(())
    }
}

async fn list_robots(
    pool: &PgPool,
    filter: &RobotFilter,
    page: &PageRequest,
) -> Result<Page<RobotToken>, StoreError> {
    let (limit, fetch) = cursor::fetch_limit(page);
    let after = match &page.after {
        Some(token) => Some(cursor::decode(PAGE, token)?),
        None => None,
    };
    let (active, disabled, expired, revoked) = state_flags(filter.state);
    let rows = sqlx::query_as::<_, Row>(select!(
        "WHERE ($1::uuid IS NULL OR project_id = $1) \
         AND ($2::bool = false OR project_id IS NULL) \
         AND ($3::bool = false OR (revoked_at IS NULL AND disabled_at IS NULL \
              AND (expires_at IS NULL OR expires_at > now()))) \
         AND ($4::bool = false OR (revoked_at IS NULL AND disabled_at IS NOT NULL)) \
         AND ($5::bool = false OR (revoked_at IS NULL AND disabled_at IS NULL \
              AND expires_at IS NOT NULL AND expires_at <= now())) \
         AND ($6::bool = false OR revoked_at IS NOT NULL) \
         AND ($7::text IS NULL OR name > $7) \
         ORDER BY name LIMIT $8"
    ))
    .bind(filter.project.map(|id| id.as_uuid()))
    .bind(filter.system_only)
    .bind(active)
    .bind(disabled)
    .bind(expired)
    .bind(revoked)
    .bind(after)
    .bind(fetch)
    .fetch_all(pool)
    .await
    .map_err(|e| map_err("list robots", e))?;
    let items: Vec<RobotToken> = rows.into_iter().map(Row::into_token).collect();
    Ok(cursor::finish(items, limit, PAGE, |token| {
        token.name.clone()
    }))
}

async fn rotate_robot(
    pool: &PgPool,
    id: RobotTokenId,
    new_hash: &str,
    new_prefix: &str,
    at: OffsetDateTime,
    expected_version: i64,
) -> Result<(), StoreError> {
    let updated = sqlx::query(
        "UPDATE robot_tokens SET token_hash = $2, display_prefix = $3, rotated_at = $4, \
         version = version + 1 WHERE id = $1 AND version = $5",
    )
    .bind(id.as_uuid())
    .bind(new_hash)
    .bind(new_prefix)
    .bind(at)
    .bind(expected_version)
    .execute(pool)
    .await;
    match updated {
        Ok(done) if done.rows_affected() == 1 => Ok(()),
        Ok(_) => check_version(pool, id, expected_version).await,
        Err(err) if is_unique(&err) => Err(StoreError::Conflict(
            "a robot with that secret already exists".to_owned(),
        )),
        Err(err) => Err(map_err("rotate robot", err)),
    }
}
