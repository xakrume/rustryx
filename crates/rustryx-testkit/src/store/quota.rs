//! Quotas of projects and repositories.
//!
//! Usage is computed from the assets on every call (distinct blobs per scope), so the counter
//! the real schema keeps in `quota_usage` can never drift here. `recount` therefore only
//! stamps `recounted_at` and reports `(used, used)`.

use async_trait::async_trait;
use rustryx_core::audit::ActorRef;
use rustryx_core::error::StoreError;
use rustryx_core::ids::RepositoryId;
use rustryx_core::ports::{Page, PageRequest, QuotaRepo};
use rustryx_core::quota::{Headroom, Quota, QuotaScope, QuotaUsage, validate_warn_percent};

use super::paging::paginate;
use super::state::State;
use super::support::bump_version;
use super::{MemStore, not_found};

impl State {
    /// Whether a scope owns a counter row: a limit was set, or it was recounted. A scope with
    /// neither has "no row, no limit" and `get` answers `None`.
    fn has_counter(&self, scope: QuotaScope) -> bool {
        let exists = match scope {
            QuotaScope::Project(id) => self.projects.contains_key(&id),
            QuotaScope::Repository(id) => self.repositories.contains_key(&id),
        };
        let configured = match scope {
            QuotaScope::Project(id) => self.project_quotas.contains_key(&id),
            QuotaScope::Repository(id) => {
                self.repositories
                    .get(&id)
                    .is_some_and(|r| r.quota_bytes.is_some())
                    || self.repository_warn.contains_key(&id)
            }
        };
        exists && (configured || self.quota_recounted.contains_key(&scope))
    }

    fn usage_of(&self, scope: QuotaScope) -> QuotaUsage {
        let (used_bytes, object_count) = self.scope_usage(scope);
        let mut usage = QuotaUsage::new(scope);
        usage.used_bytes = used_bytes;
        usage.object_count = object_count;
        usage.limit_bytes = self.scope_limit(scope);
        usage.warn_percent = self.warn_percent(scope);
        usage.recounted_at = self.quota_recounted.get(&scope).copied();
        usage
    }
}

fn sort_key(scope: QuotaScope) -> String {
    match scope {
        QuotaScope::Project(id) => format!("p:{id}"),
        QuotaScope::Repository(id) => format!("r:{id}"),
    }
}

#[async_trait]
impl QuotaRepo for MemStore {
    async fn get(&self, scope: QuotaScope) -> Result<Option<QuotaUsage>, StoreError> {
        let state = self.tables()?;
        Ok(state.has_counter(scope).then(|| state.usage_of(scope)))
    }

    async fn set_limit(
        &self,
        scope: QuotaScope,
        limit_bytes: Option<u64>,
        warn_percent: u8,
        expected_version: Option<i64>,
        _actor: &ActorRef,
    ) -> Result<(), StoreError> {
        let mut state = self.tables()?;
        validate_warn_percent(warn_percent)
            .map_err(|error| StoreError::Invalid(error.to_string()))?;
        match scope {
            QuotaScope::Project(id) => {
                if !state.projects.contains_key(&id) {
                    return Err(not_found("project", id));
                }
                // A project without a quota row stands at version 0, so the first write is
                // accepted against `Some(0)` and against no expectation at all.
                let current = state.project_quotas.get(&id).map_or(0, |q| q.version);
                let version = match expected_version {
                    Some(expected) => bump_version(current, expected)?,
                    None => current.saturating_add(1),
                };
                let mut quota = Quota::new(scope, limit_bytes);
                quota.warn_percent = warn_percent;
                quota.version = version;
                state.project_quotas.insert(id, quota);
            }
            QuotaScope::Repository(id) => {
                // The repository limit is the phase 1 column `repositories.quota_bytes`, which
                // has no version, so `expected_version` has nothing to be compared with.
                let repository = state
                    .repositories
                    .get_mut(&id)
                    .ok_or_else(|| not_found("repository", id))?;
                repository.quota_bytes = limit_bytes;
                state.repository_warn.insert(id, warn_percent);
            }
        }
        Ok(())
    }

    async fn headroom(&self, repository: RepositoryId) -> Result<Headroom, StoreError> {
        let state = self.tables()?;
        let stored = state
            .repositories
            .get(&repository)
            .ok_or_else(|| not_found("repository", repository))?;
        let tightest = state
            .scopes_of(stored)
            .into_iter()
            .filter_map(|scope| {
                let limit = state.scope_limit(scope)?;
                let (used, _) = state.scope_usage(scope);
                Some((limit.saturating_sub(used), scope))
            })
            .min_by_key(|(room, _)| *room);
        Ok(
            tightest.map_or(Headroom::UNLIMITED, |(room, scope)| Headroom {
                bytes: Some(room),
                binding: Some(scope),
            }),
        )
    }

    async fn recount(&self, scope: QuotaScope) -> Result<(u64, u64), StoreError> {
        let now = self.now();
        let mut state = self.tables()?;
        let exists = match scope {
            QuotaScope::Project(id) => state.projects.contains_key(&id),
            QuotaScope::Repository(id) => state.repositories.contains_key(&id),
        };
        if !exists {
            return Err(StoreError::NotFound(format!(
                "quota scope {}",
                sort_key(scope)
            )));
        }
        let (used, _) = state.scope_usage(scope);
        state.quota_recounted.insert(scope, now);
        Ok((used, used))
    }

    async fn list(&self, page: &PageRequest) -> Result<Page<QuotaUsage>, StoreError> {
        let state = self.tables()?;
        let mut scopes: Vec<QuotaScope> = state
            .projects
            .keys()
            .map(|id| QuotaScope::Project(*id))
            .chain(
                state
                    .repositories
                    .keys()
                    .map(|id| QuotaScope::Repository(*id)),
            )
            .filter(|scope| state.has_counter(*scope))
            .collect();
        scopes.sort_by_key(|scope| sort_key(*scope));
        let rows = scopes
            .into_iter()
            .map(|scope| (sort_key(scope), state.usage_of(scope)))
            .collect();
        drop(state);
        paginate(rows, page)
    }
}
