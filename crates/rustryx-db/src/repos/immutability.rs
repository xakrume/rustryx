//! [`ImmutabilityRepo`] on PostgreSQL (HB-24).

use async_trait::async_trait;
use rustryx_core::StoreError;
use rustryx_core::ids::{ImmutabilityRuleId, RepositoryId, UserId};
use rustryx_core::lifecycle::{
    ImmutabilityRule, ItemKind, NameSelector, PolicyScope, SelectorMode,
};
use rustryx_core::ports::{ImmutabilityRepo, Page, PageRequest};
use sqlx::{FromRow, PgPool};
use time::OffsetDateTime;
use uuid::Uuid;

use super::common::corrupt;
use crate::cursor;
use crate::error::map_err;

const KIND: &str = "immutability";

macro_rules! select {
    ($($tail:literal),*) => {
        concat!(
            "SELECT id, project_id, repository_id, name, item_kind, repo_mode, repo_globs, \
             item_mode, item_globs, enabled, version, created_by, created_at, updated_at \
             FROM immutability_rules ",
            $($tail),*
        )
    };
}

/// Immutability rules.
#[derive(Debug, Clone)]
pub struct PgImmutabilityRepo {
    pool: PgPool,
}

impl PgImmutabilityRepo {
    /// Creates the repository over `pool`.
    #[must_use]
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[derive(Debug, FromRow)]
struct Row {
    id: Uuid,
    project_id: Option<Uuid>,
    repository_id: Option<Uuid>,
    name: String,
    item_kind: String,
    repo_mode: String,
    repo_globs: Vec<String>,
    item_mode: String,
    item_globs: Vec<String>,
    enabled: bool,
    version: i64,
    created_by: Option<Uuid>,
    created_at: OffsetDateTime,
    updated_at: OffsetDateTime,
}

fn mode_label(mode: SelectorMode) -> &'static str {
    match mode {
        SelectorMode::Matching => "matching",
        SelectorMode::Excluding => "excluding",
    }
}

fn parse_mode(raw: &str) -> Result<SelectorMode, StoreError> {
    match raw {
        "matching" => Ok(SelectorMode::Matching),
        "excluding" => Ok(SelectorMode::Excluding),
        _ => Err(corrupt("read immutability rule")),
    }
}

fn parse_kind(raw: &str) -> Result<ItemKind, StoreError> {
    match raw {
        "tag" => Ok(ItemKind::Tag),
        "version" => Ok(ItemKind::Version),
        "path" => Ok(ItemKind::Path),
        _ => Err(corrupt("read immutability rule")),
    }
}

fn kind_label(kind: ItemKind) -> &'static str {
    match kind {
        ItemKind::Tag => "tag",
        ItemKind::Version => "version",
        ItemKind::Path => "path",
    }
}

fn selector(mode: &str, globs: Vec<String>) -> Result<NameSelector, StoreError> {
    NameSelector::new(parse_mode(mode)?, globs).map_err(|_| corrupt("read immutability rule"))
}

impl Row {
    fn into_rule(self) -> Result<ImmutabilityRule, StoreError> {
        let scope = match (self.project_id, self.repository_id) {
            (Some(id), None) => PolicyScope::Project(id.into()),
            (None, Some(id)) => PolicyScope::Repository(RepositoryId::from_uuid(id)),
            _ => return Err(corrupt("read immutability rule")),
        };
        let mut rule = ImmutabilityRule::new(
            ImmutabilityRuleId::from_uuid(self.id),
            scope,
            self.name,
            parse_kind(&self.item_kind)?,
            selector(&self.item_mode, self.item_globs)?,
            self.created_at,
        );
        rule.repository = selector(&self.repo_mode, self.repo_globs)?;
        rule.enabled = self.enabled;
        rule.version = self.version;
        rule.created_by = self.created_by.map(UserId::from_uuid);
        rule.updated_at = self.updated_at;
        Ok(rule)
    }
}

struct ScopeIds {
    project_id: Option<Uuid>,
    repository_id: Option<Uuid>,
}

fn scope_ids(scope: PolicyScope) -> Result<ScopeIds, StoreError> {
    match scope {
        PolicyScope::Project(id) => Ok(ScopeIds {
            project_id: Some(id.as_uuid()),
            repository_id: None,
        }),
        PolicyScope::Repository(id) => Ok(ScopeIds {
            project_id: None,
            repository_id: Some(id.as_uuid()),
        }),
        PolicyScope::System => Err(StoreError::Conflict(
            "an immutability rule belongs to a project or a repository".to_owned(),
        )),
    }
}

fn regexes(selector: &NameSelector) -> Result<Vec<String>, StoreError> {
    selector
        .to_regexes()
        .map_err(|error| StoreError::Conflict(error.to_string()))
}

#[async_trait]
impl ImmutabilityRepo for PgImmutabilityRepo {
    async fn create(&self, rule: &ImmutabilityRule) -> Result<(), StoreError> {
        rule.validate()
            .map_err(|error| StoreError::Conflict(error.to_string()))?;
        let scope = scope_ids(rule.scope)?;
        let repo_regexes = regexes(&rule.repository)?;
        let item_regexes = regexes(&rule.item)?;
        sqlx::query(
            "INSERT INTO immutability_rules (id, project_id, repository_id, name, item_kind, \
             repo_mode, repo_globs, repo_regexes, item_mode, item_globs, item_regexes, enabled, \
             version, created_by, created_at, updated_at) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, 1, $13, $14, $15)",
        )
        .bind(rule.id.as_uuid())
        .bind(scope.project_id)
        .bind(scope.repository_id)
        .bind(&rule.name)
        .bind(kind_label(rule.item_kind))
        .bind(mode_label(rule.repository.mode))
        .bind(&rule.repository.globs)
        .bind(&repo_regexes)
        .bind(mode_label(rule.item.mode))
        .bind(&rule.item.globs)
        .bind(&item_regexes)
        .bind(rule.enabled)
        .bind(rule.created_by.map(|id| id.as_uuid()))
        .bind(rule.created_at)
        .bind(rule.updated_at)
        .execute(&self.pool)
        .await
        .map_err(|e| map_err("create immutability rule", e))?;
        Ok(())
    }

    async fn get(&self, id: ImmutabilityRuleId) -> Result<Option<ImmutabilityRule>, StoreError> {
        sqlx::query_as::<_, Row>(select!("WHERE id = $1"))
            .bind(id.as_uuid())
            .fetch_optional(&self.pool)
            .await
            .map_err(|e| map_err("read immutability rule", e))?
            .map(Row::into_rule)
            .transpose()
    }

    async fn list(
        &self,
        scope: PolicyScope,
        page: &PageRequest,
    ) -> Result<Page<ImmutabilityRule>, StoreError> {
        let scope = scope_ids(scope)?;
        let (limit, fetch) = cursor::fetch_limit(page);
        let after = match &page.after {
            Some(token) => Some(
                Uuid::parse_str(&cursor::decode(KIND, token)?)
                    .map_err(|_| cursor::invalid_token())?,
            ),
            None => None,
        };
        let rows = sqlx::query_as::<_, Row>(select!(
            "WHERE project_id IS NOT DISTINCT FROM $1 AND repository_id IS NOT DISTINCT FROM $2 \
             AND ($3::uuid IS NULL OR id > $3) ORDER BY id LIMIT $4"
        ))
        .bind(scope.project_id)
        .bind(scope.repository_id)
        .bind(after)
        .bind(fetch)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| map_err("list immutability rules", e))?;
        let items = rows
            .into_iter()
            .map(Row::into_rule)
            .collect::<Result<Vec<_>, _>>()?;
        Ok(cursor::finish(items, limit, KIND, |rule| {
            rule.id.to_string()
        }))
    }

    async fn update(
        &self,
        rule: &ImmutabilityRule,
        expected_version: i64,
    ) -> Result<(), StoreError> {
        rule.validate()
            .map_err(|error| StoreError::Conflict(error.to_string()))?;
        let repo_regexes = regexes(&rule.repository)?;
        let item_regexes = regexes(&rule.item)?;
        let updated = sqlx::query(
            "UPDATE immutability_rules SET name = $3, item_kind = $4, repo_mode = $5, \
             repo_globs = $6, repo_regexes = $7, item_mode = $8, item_globs = $9, \
             item_regexes = $10, enabled = $11, version = version + 1, updated_at = now() \
             WHERE id = $1 AND version = $2",
        )
        .bind(rule.id.as_uuid())
        .bind(expected_version)
        .bind(&rule.name)
        .bind(kind_label(rule.item_kind))
        .bind(mode_label(rule.repository.mode))
        .bind(&rule.repository.globs)
        .bind(&repo_regexes)
        .bind(mode_label(rule.item.mode))
        .bind(&rule.item.globs)
        .bind(&item_regexes)
        .bind(rule.enabled)
        .execute(&self.pool)
        .await
        .map_err(|e| map_err("update immutability rule", e))?;
        if updated.rows_affected() == 1 {
            return Ok(());
        }
        let exists: bool =
            sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM immutability_rules WHERE id = $1)")
                .bind(rule.id.as_uuid())
                .fetch_one(&self.pool)
                .await
                .map_err(|e| map_err("update immutability rule", e))?;
        Err(if exists {
            StoreError::Conflict("immutability rule was changed by someone else".to_owned())
        } else {
            StoreError::NotFound("immutability rule".to_owned())
        })
    }

    async fn delete(&self, id: ImmutabilityRuleId) -> Result<(), StoreError> {
        let result = sqlx::query("DELETE FROM immutability_rules WHERE id = $1")
            .bind(id.as_uuid())
            .execute(&self.pool)
            .await
            .map_err(|e| map_err("delete immutability rule", e))?;
        if result.rows_affected() == 0 {
            return Err(StoreError::NotFound("immutability rule".to_owned()));
        }
        Ok(())
    }

    async fn covering_rule(
        &self,
        repository: RepositoryId,
        kind: ItemKind,
        item: &str,
    ) -> Result<Option<ImmutabilityRuleId>, StoreError> {
        let exists: bool =
            sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM repositories WHERE id = $1)")
                .bind(repository.as_uuid())
                .fetch_one(&self.pool)
                .await
                .map_err(|e| map_err("read immutability coverage", e))?;
        if !exists {
            return Err(StoreError::NotFound("repository".to_owned()));
        }
        let id: Option<Uuid> = sqlx::query_scalar("SELECT rx_immutable_rule($1, $2, $3)")
            .bind(repository.as_uuid())
            .bind(kind_label(kind))
            .bind(item)
            .fetch_one(&self.pool)
            .await
            .map_err(|e| map_err("read immutability coverage", e))?;
        Ok(id.map(ImmutabilityRuleId::from_uuid))
    }

    async fn preview(
        &self,
        rule: &ImmutabilityRule,
        limit: u32,
    ) -> Result<Vec<String>, StoreError> {
        rule.validate()
            .map_err(|error| StoreError::Conflict(error.to_string()))?;
        let scope = scope_ids(rule.scope)?;
        let repo_regexes = regexes(&rule.repository)?;
        let item_regexes = regexes(&rule.item)?;
        let cap = i64::from(limit.min(100));
        let sql = match rule.item_kind {
            ItemKind::Tag => return Ok(Vec::new()),
            ItemKind::Version => {
                "SELECT DISTINCT components.version FROM repositories \
                 JOIN components ON components.repository_id = repositories.id \
                 WHERE ($1::uuid IS NULL OR repositories.project_id = $1) \
                   AND ($2::uuid IS NULL OR repositories.id = $2) \
                   AND rx_selector_matches($3, $4, repositories.name) \
                   AND components.version IS NOT NULL \
                   AND rx_selector_matches($5, $6, components.version) \
                 ORDER BY 1 LIMIT $7"
            }
            ItemKind::Path => {
                "SELECT DISTINCT assets.path FROM repositories \
                 JOIN assets ON assets.repository_id = repositories.id \
                 WHERE ($1::uuid IS NULL OR repositories.project_id = $1) \
                   AND ($2::uuid IS NULL OR repositories.id = $2) \
                   AND rx_selector_matches($3, $4, repositories.name) \
                   AND rx_selector_matches($5, $6, assets.path) \
                 ORDER BY 1 LIMIT $7"
            }
        };
        let items: Vec<String> = sqlx::query_scalar(sql)
            .bind(scope.project_id)
            .bind(scope.repository_id)
            .bind(mode_label(rule.repository.mode))
            .bind(&repo_regexes)
            .bind(mode_label(rule.item.mode))
            .bind(&item_regexes)
            .bind(cap)
            .fetch_all(&self.pool)
            .await
            .map_err(|e| map_err("preview immutability rule", e))?;
        Ok(items)
    }
}
