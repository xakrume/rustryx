//! [`RoutingRuleRepo`] on `PostgreSQL`.

use async_trait::async_trait;
use rustryx_core::StoreError;
use rustryx_core::ids::{RepositoryId, RoutingRuleId};
use rustryx_core::ports::RoutingRuleRepo;
use rustryx_core::routing::{RoutingMode, RoutingRule};
use sqlx::{FromRow, PgPool};
use time::OffsetDateTime;
use uuid::Uuid;

use super::common::corrupt;
use crate::error::map_err;

macro_rules! select {
    ($($tail:literal),*) => {
        concat!(
            "SELECT id, name, mode, patterns, description, version, created_at, updated_at \
             FROM routing_rules ",
            $($tail),*
        )
    };
}

/// Routing rules.
#[derive(Debug, Clone)]
pub struct PgRoutingRuleRepo {
    pool: PgPool,
}

impl PgRoutingRuleRepo {
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
    mode: String,
    patterns: Vec<String>,
    description: String,
    version: i64,
    created_at: OffsetDateTime,
    updated_at: OffsetDateTime,
}

const fn mode_label(mode: RoutingMode) -> &'static str {
    match mode {
        RoutingMode::Allow => "allow",
        RoutingMode::Block => "block",
    }
}

impl Row {
    fn into_rule(self) -> Result<RoutingRule, StoreError> {
        let mode = match self.mode.as_str() {
            "allow" => RoutingMode::Allow,
            "block" => RoutingMode::Block,
            _ => return Err(corrupt("read routing rule")),
        };
        let mut rule = RoutingRule::new(
            RoutingRuleId::from_uuid(self.id),
            self.name,
            mode,
            self.patterns,
            self.created_at,
        );
        rule.description = self.description;
        rule.version = self.version;
        rule.updated_at = self.updated_at;
        Ok(rule)
    }
}

#[async_trait]
impl RoutingRuleRepo for PgRoutingRuleRepo {
    /// The version of a new row is 1 whatever the struct says: the counter belongs to the
    /// store, a caller that sent another value would otherwise make the first `update` fail
    /// its compare and set for no visible reason. `Conflict` on a duplicate name or a pattern
    /// list outside the shape limits.
    async fn create(&self, rule: &RoutingRule) -> Result<(), StoreError> {
        sqlx::query(
            "INSERT INTO routing_rules (id, name, mode, patterns, description, created_at, \
             updated_at) VALUES ($1, $2, $3, $4, $5, $6, $7)",
        )
        .bind(rule.id.as_uuid())
        .bind(&rule.name)
        .bind(mode_label(rule.mode))
        .bind(&rule.patterns)
        .bind(&rule.description)
        .bind(rule.created_at)
        .bind(rule.updated_at)
        .execute(&self.pool)
        .await
        .map_err(|e| map_err("create routing rule", e))?;
        Ok(())
    }

    async fn get(&self, id: RoutingRuleId) -> Result<Option<RoutingRule>, StoreError> {
        sqlx::query_as::<_, Row>(select!("WHERE id = $1"))
            .bind(id.as_uuid())
            .fetch_optional(&self.pool)
            .await
            .map_err(|e| map_err("read routing rule", e))?
            .map(Row::into_rule)
            .transpose()
    }

    async fn list(&self) -> Result<Vec<RoutingRule>, StoreError> {
        sqlx::query_as::<_, Row>(select!("ORDER BY name"))
            .fetch_all(&self.pool)
            .await
            .map_err(|e| map_err("list routing rules", e))?
            .into_iter()
            .map(Row::into_rule)
            .collect()
    }

    /// Compare and set on the version, then `version + 1`. `Conflict` on a stale
    /// `expected_version`, `NotFound` when the rule does not exist (told apart by a read
    /// after a miss, so the success path is one statement).
    ///
    /// `name` stays out of the `SET` list unless it changes: a unique index column there
    /// takes the row lock as a KEY update, which blocks the foreign key checks of concurrent
    /// repository updates that point at the rule (db.md section 6).
    async fn update(&self, rule: &RoutingRule, expected_version: i64) -> Result<(), StoreError> {
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| map_err("update routing rule", e))?;
        let current_name: Option<String> = sqlx::query_scalar(
            "UPDATE routing_rules SET mode = $3, patterns = $4, description = $5, \
             version = version + 1, updated_at = now() \
             WHERE id = $1 AND version = $2 RETURNING name",
        )
        .bind(rule.id.as_uuid())
        .bind(expected_version)
        .bind(mode_label(rule.mode))
        .bind(&rule.patterns)
        .bind(&rule.description)
        .fetch_optional(&mut *tx)
        .await
        .map_err(|e| map_err("update routing rule", e))?;
        let Some(current_name) = current_name else {
            let exists: bool =
                sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM routing_rules WHERE id = $1)")
                    .bind(rule.id.as_uuid())
                    .fetch_one(&mut *tx)
                    .await
                    .map_err(|e| map_err("update routing rule", e))?;
            return Err(if exists {
                StoreError::Conflict("routing rule was changed by someone else".to_owned())
            } else {
                StoreError::NotFound("routing rule".to_owned())
            });
        };
        if current_name != rule.name {
            sqlx::query("UPDATE routing_rules SET name = $2 WHERE id = $1")
                .bind(rule.id.as_uuid())
                .bind(&rule.name)
                .execute(&mut *tx)
                .await
                .map_err(|e| map_err("rename routing rule", e))?;
        }
        tx.commit()
            .await
            .map_err(|e| map_err("update routing rule", e))
    }

    /// `Conflict` while a repository uses the rule (the foreign key is `RESTRICT`, so the
    /// check and the delete are one atomic statement and a repository that attaches the rule
    /// concurrently cannot be missed); `NotFound` when absent.
    async fn delete(&self, id: RoutingRuleId) -> Result<(), StoreError> {
        let result = sqlx::query("DELETE FROM routing_rules WHERE id = $1")
            .bind(id.as_uuid())
            .execute(&self.pool)
            .await
            .map_err(|e| map_err("delete routing rule", e))?;
        if result.rows_affected() == 0 {
            return Err(StoreError::NotFound("routing rule".to_owned()));
        }
        Ok(())
    }

    async fn assigned_to(&self, id: RoutingRuleId) -> Result<Vec<RepositoryId>, StoreError> {
        let ids: Vec<Uuid> = sqlx::query_scalar(
            "SELECT id FROM repositories WHERE routing_rule_id = $1 ORDER BY name",
        )
        .bind(id.as_uuid())
        .fetch_all(&self.pool)
        .await
        .map_err(|e| map_err("list routing rule users", e))?;
        Ok(ids.into_iter().map(RepositoryId::from_uuid).collect())
    }
}
