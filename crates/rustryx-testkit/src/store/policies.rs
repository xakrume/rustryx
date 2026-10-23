//! Immutability rules, holds, cleanup policies and retention policies.

use std::collections::BTreeSet;

use async_trait::async_trait;
use rustryx_core::error::StoreError;
use rustryx_core::ids::{
    CleanupPolicyId, ComponentId, ImmutabilityRuleId, LifecycleHoldId, RepositoryId,
    RetentionPolicyId, UserId,
};
use rustryx_core::lifecycle::{
    CleanupPolicy, HoldTarget, ImmutabilityRule, ItemKind, LifecycleHold, PolicyScope,
    RetentionPolicy,
};
use rustryx_core::ports::{
    CleanupPolicyRepo, ImmutabilityRepo, LifecycleHoldRepo, Page, PageRequest, RetentionPolicyRepo,
};
use time::OffsetDateTime;

use super::guards::selector_selects;
use super::paging::paginate;
use super::state::State;
use super::support::bump_version;
use super::{MemStore, conflict, not_found};

/// Most immutability rules of one scope (the `BEFORE INSERT` trigger of the real schema).
const MAX_RULES_PER_SCOPE: usize = 15;

impl State {
    /// The foreign key behind a policy scope: the project or repository must exist.
    fn check_scope_exists(&self, scope: PolicyScope) -> Result<(), StoreError> {
        let exists = match scope {
            PolicyScope::System => true,
            PolicyScope::Project(id) => self.projects.contains_key(&id),
            PolicyScope::Repository(id) => self.repositories.contains_key(&id),
        };
        if exists {
            Ok(())
        } else {
            Err(conflict("the scope of the policy does not exist"))
        }
    }
}

#[async_trait]
impl ImmutabilityRepo for MemStore {
    async fn create(&self, rule: &ImmutabilityRule) -> Result<(), StoreError> {
        let mut state = self.tables()?;
        // The CHECK constraints of the table, reported as conflicts like every constraint.
        rule.validate()
            .map_err(|error| conflict(error.to_string()))?;
        state.check_scope_exists(rule.scope)?;
        if state.immutability_rules.contains_key(&rule.id) {
            return Err(conflict(format!("rule {} already exists", rule.id)));
        }
        let in_scope = state
            .immutability_rules
            .values()
            .filter(|existing| existing.scope == rule.scope)
            .count();
        if in_scope >= MAX_RULES_PER_SCOPE {
            return Err(conflict("a scope has at most 15 immutability rules"));
        }
        state.immutability_rules.insert(rule.id, rule.clone());
        Ok(())
    }

    async fn get(&self, id: ImmutabilityRuleId) -> Result<Option<ImmutabilityRule>, StoreError> {
        Ok(self.tables()?.immutability_rules.get(&id).cloned())
    }

    async fn list(
        &self,
        scope: PolicyScope,
        page: &PageRequest,
    ) -> Result<Page<ImmutabilityRule>, StoreError> {
        let rows: Vec<(String, ImmutabilityRule)> = self
            .tables()?
            .immutability_rules
            .values()
            .filter(|rule| rule.scope == scope)
            .map(|rule| (rule.id.to_string(), rule.clone()))
            .collect();
        paginate(rows, page)
    }

    async fn update(
        &self,
        rule: &ImmutabilityRule,
        expected_version: i64,
    ) -> Result<(), StoreError> {
        let now = self.now();
        let mut state = self.tables()?;
        let existing = state
            .immutability_rules
            .get(&rule.id)
            .ok_or_else(|| not_found("immutability rule", rule.id))?;
        let version = bump_version(existing.version, expected_version)?;
        let mut next = rule.clone();
        // A rule never changes scope: that would make items of another project changeable
        // or frozen without a rule being created or deleted there. The stored scope is put
        // in before the check, so a caller's stale or forged scope is not what is validated.
        next.scope = existing.scope;
        next.validate()
            .map_err(|error| conflict(error.to_string()))?;
        next.version = version;
        next.created_at = existing.created_at;
        next.created_by = existing.created_by;
        next.updated_at = now;
        state.immutability_rules.insert(next.id, next);
        Ok(())
    }

    async fn delete(&self, id: ImmutabilityRuleId) -> Result<(), StoreError> {
        let mut state = self.tables()?;
        if state.immutability_rules.remove(&id).is_none() {
            return Err(not_found("immutability rule", id));
        }
        Ok(())
    }

    async fn covering_rule(
        &self,
        repository: RepositoryId,
        kind: ItemKind,
        item: &str,
    ) -> Result<Option<ImmutabilityRuleId>, StoreError> {
        let state = self.tables()?;
        let stored = state
            .repositories
            .get(&repository)
            .ok_or_else(|| not_found("repository", repository))?;
        Ok(state.immutable_rule(stored, kind, item))
    }

    async fn preview(
        &self,
        rule: &ImmutabilityRule,
        limit: u32,
    ) -> Result<Vec<String>, StoreError> {
        let state = self.tables()?;
        let mut items = BTreeSet::new();
        for repository in state.repositories.values() {
            let in_scope = match rule.scope {
                PolicyScope::System => true,
                PolicyScope::Project(id) => state.project_of(repository) == Some(id),
                PolicyScope::Repository(id) => repository.id == id,
            };
            if !in_scope || !selector_selects(&rule.repository, repository.name.as_str()) {
                continue;
            }
            // Tags belong to the OCI crate's own table, which the double does not have, so a
            // tag rule previews nothing here.
            let candidates: Vec<&str> = match rule.item_kind {
                ItemKind::Path => state
                    .assets
                    .values()
                    .filter(|a| a.repository_id == repository.id)
                    .map(|a| a.path.as_str())
                    .collect(),
                ItemKind::Version => state
                    .components
                    .values()
                    .filter(|c| c.repository_id == repository.id)
                    .filter_map(|c| c.version.as_deref())
                    .collect(),
                ItemKind::Tag => Vec::new(),
            };
            items.extend(
                candidates
                    .into_iter()
                    .filter(|item| selector_selects(&rule.item, item))
                    .map(str::to_owned),
            );
        }
        Ok(items
            .into_iter()
            .take(usize::try_from(limit).unwrap_or(usize::MAX))
            .collect())
    }
}

#[async_trait]
impl LifecycleHoldRepo for MemStore {
    async fn create(&self, hold: &LifecycleHold) -> Result<(), StoreError> {
        let mut state = self.tables()?;
        hold.validate()
            .map_err(|error| conflict(error.to_string()))?;
        let target_exists = match hold.target {
            HoldTarget::Component(id) => state.components.contains_key(&id),
            HoldTarget::Repository(id) => state.repositories.contains_key(&id),
            HoldTarget::Project(id) => state.projects.contains_key(&id),
        };
        if !target_exists {
            return Err(conflict("the target of the hold does not exist"));
        }
        if state.holds.contains_key(&hold.id) {
            return Err(conflict(format!("hold {} already exists", hold.id)));
        }
        state.holds.insert(hold.id, hold.clone());
        Ok(())
    }

    async fn get(&self, id: LifecycleHoldId) -> Result<Option<LifecycleHold>, StoreError> {
        Ok(self.tables()?.holds.get(&id).cloned())
    }

    async fn release(
        &self,
        id: LifecycleHoldId,
        by: Option<UserId>,
        at: OffsetDateTime,
    ) -> Result<(), StoreError> {
        let mut state = self.tables()?;
        let hold = state
            .holds
            .get_mut(&id)
            .ok_or_else(|| not_found("hold", id))?;
        // Idempotent: the first release stays, so the record says who ended the hold and when.
        if hold.released_at.is_none() {
            hold.released_at = Some(at);
            hold.released_by = by;
        }
        Ok(())
    }

    async fn list(
        &self,
        target: Option<HoldTarget>,
        active_only: bool,
        page: &PageRequest,
    ) -> Result<Page<LifecycleHold>, StoreError> {
        let now = self.now();
        let rows: Vec<(String, LifecycleHold)> = self
            .tables()?
            .holds
            .values()
            .filter(|hold| target.is_none_or(|t| hold.target == t))
            .filter(|hold| !active_only || hold.is_active_at(now))
            .map(|hold| (hold.id.to_string(), hold.clone()))
            .collect();
        paginate(rows, page)
    }

    async fn active_for(&self, component: ComponentId) -> Result<Vec<LifecycleHold>, StoreError> {
        let now = self.now();
        let state = self.tables()?;
        let Some(stored) = state.components.get(&component) else {
            return Ok(Vec::new());
        };
        let repository = state.repositories.get(&stored.repository_id);
        let project = repository.and_then(|r| state.project_of(r));
        let mut holds: Vec<LifecycleHold> = state
            .holds
            .values()
            .filter(|hold| hold.is_active_at(now))
            .filter(|hold| match hold.target {
                HoldTarget::Component(id) => id == component,
                HoldTarget::Repository(id) => id == stored.repository_id,
                HoldTarget::Project(id) => Some(id) == project,
            })
            .cloned()
            .collect();
        holds.sort_by_key(|hold| hold.id);
        Ok(holds)
    }
}

#[async_trait]
impl CleanupPolicyRepo for MemStore {
    async fn create(&self, policy: &CleanupPolicy) -> Result<(), StoreError> {
        let mut state = self.tables()?;
        policy
            .validate()
            .map_err(|error| conflict(error.to_string()))?;
        let taken = state
            .cleanup_policies
            .values()
            .any(|p| p.id == policy.id || p.name == policy.name);
        if taken {
            return Err(conflict(format!(
                "cleanup policy {} already exists",
                policy.name
            )));
        }
        state.cleanup_policies.insert(policy.id, policy.clone());
        Ok(())
    }

    async fn get(&self, id: CleanupPolicyId) -> Result<Option<CleanupPolicy>, StoreError> {
        Ok(self.tables()?.cleanup_policies.get(&id).cloned())
    }

    async fn list(&self) -> Result<Vec<CleanupPolicy>, StoreError> {
        let mut all: Vec<CleanupPolicy> =
            self.tables()?.cleanup_policies.values().cloned().collect();
        all.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(all)
    }

    async fn update(
        &self,
        policy: &CleanupPolicy,
        expected_version: i64,
    ) -> Result<(), StoreError> {
        let now = self.now();
        let mut state = self.tables()?;
        let existing = state
            .cleanup_policies
            .get(&policy.id)
            .ok_or_else(|| not_found("cleanup policy", policy.id))?;
        let version = bump_version(existing.version, expected_version)?;
        policy
            .validate()
            .map_err(|error| conflict(error.to_string()))?;
        let name_taken = state
            .cleanup_policies
            .values()
            .any(|p| p.id != policy.id && p.name == policy.name);
        if name_taken {
            return Err(conflict(format!(
                "cleanup policy {} already exists",
                policy.name
            )));
        }
        let mut next = policy.clone();
        // A dry run is valid only for the version it ran on, so every edit moves the version.
        next.version = version;
        next.created_at = existing.created_at;
        next.updated_at = now;
        state.cleanup_policies.insert(next.id, next);
        Ok(())
    }

    async fn delete(&self, id: CleanupPolicyId) -> Result<(), StoreError> {
        let mut state = self.tables()?;
        if !state.cleanup_policies.contains_key(&id) {
            return Err(not_found("cleanup policy", id));
        }
        // The id array of a repository has no foreign key, so the check is the port's job.
        if state
            .repositories
            .values()
            .any(|r| r.cleanup_policy_ids.contains(&id))
        {
            return Err(conflict("cleanup policy is still attached to a repository"));
        }
        state.cleanup_policies.remove(&id);
        Ok(())
    }

    async fn repositories_using(
        &self,
        id: CleanupPolicyId,
    ) -> Result<Vec<RepositoryId>, StoreError> {
        let state = self.tables()?;
        let mut repositories: Vec<RepositoryId> = state
            .repositories
            .values()
            .filter(|r| r.cleanup_policy_ids.contains(&id))
            .map(|r| r.id)
            .collect();
        repositories.sort();
        Ok(repositories)
    }
}

#[async_trait]
impl RetentionPolicyRepo for MemStore {
    async fn upsert(
        &self,
        policy: &RetentionPolicy,
        expected_version: Option<i64>,
    ) -> Result<(), StoreError> {
        let now = self.now();
        let mut state = self.tables()?;
        policy
            .validate()
            .map_err(|error| conflict(error.to_string()))?;
        state.check_scope_exists(policy.scope)?;
        let existing = state
            .retention_policies
            .values()
            .find(|p| p.scope == policy.scope)
            .cloned();
        let mut next = policy.clone();
        match (existing, expected_version) {
            (Some(current), Some(expected)) => {
                next.version = bump_version(current.version, expected)?;
                // The row of a scope keeps its identity: the unique index is on the scope.
                next.id = current.id;
                next.created_at = current.created_at;
                next.updated_at = now;
            }
            (Some(_), None) => {
                return Err(conflict("the scope already has a retention policy"));
            }
            (None, Some(_)) => {
                return Err(conflict("stale version: the scope has no retention policy"));
            }
            (None, None) => {
                if state.retention_policies.contains_key(&policy.id) {
                    return Err(conflict(format!(
                        "retention policy {} already exists",
                        policy.id
                    )));
                }
            }
        }
        state.retention_policies.insert(next.id, next);
        Ok(())
    }

    async fn get(&self, id: RetentionPolicyId) -> Result<Option<RetentionPolicy>, StoreError> {
        Ok(self.tables()?.retention_policies.get(&id).cloned())
    }

    async fn get_for_scope(
        &self,
        scope: PolicyScope,
    ) -> Result<Option<RetentionPolicy>, StoreError> {
        Ok(self
            .tables()?
            .retention_policies
            .values()
            .find(|p| p.scope == scope)
            .cloned())
    }

    async fn delete(&self, id: RetentionPolicyId) -> Result<(), StoreError> {
        let mut state = self.tables()?;
        if state.retention_policies.remove(&id).is_none() {
            return Err(not_found("retention policy", id));
        }
        Ok(())
    }
}
