//! The guards that PostgreSQL runs as triggers (`RX004` immutable, `RX005` held, `RX006`
//! quota, `RX007` read only), as functions over the tables.
//!
//! Every state changing method of the store calls them under the same lock that makes the
//! change, so a refusal leaves nothing behind, exactly like the rolled back transaction. They
//! apply to the phase 1 methods as well: those flatten a violation to `StoreError::Conflict`,
//! which is what an N-1 writer sees against the real database.

use regex::Regex;
use rustryx_core::error::PolicyViolation;
use rustryx_core::ids::{ComponentId, ImmutabilityRuleId, LifecycleHoldId, ProjectId};
use rustryx_core::lifecycle::{
    HoldKind, HoldTarget, ImmutabilityRule, ItemKind, NameSelector, PolicyScope,
};
use rustryx_core::model::{Asset, Component, Repository, WritePolicy};
use rustryx_core::quota::{QuotaExceeded, QuotaScope};
use time::OffsetDateTime;

use super::state::State;

/// The `system_config` key of read only mode.
pub(crate) const READ_ONLY_KEY: &str = "system.read_only";

impl State {
    /// `rx_assert_writable()`: true while `system.read_only` has `enabled = true`. A row that
    /// does not parse counts as off, like a missing row (the database function reads the
    /// `enabled` member and nothing else).
    pub(crate) fn is_read_only(&self) -> bool {
        self.config
            .get(READ_ONLY_KEY)
            .and_then(|row| row.value.get("enabled"))
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false)
    }

    /// `RX007`.
    pub(crate) fn check_writable(&self) -> Result<(), PolicyViolation> {
        if self.is_read_only() {
            Err(PolicyViolation::ReadOnly)
        } else {
            Ok(())
        }
    }

    /// The project a repository belongs to, from the project table and not from the copy in
    /// the repository row, so a project that was deleted or reattached is seen as it is.
    pub(crate) fn project_of(&self, repository: &Repository) -> Option<ProjectId> {
        repository
            .project
            .as_ref()
            .map(|project| project.id)
            .filter(|id| self.projects.contains_key(id))
    }

    /// `rx_immutable_rule`: the first enabled rule of the repository's scope or its project's
    /// scope with that item kind whose repository selector matches the repository name and
    /// whose item selector matches `item`. Rules are tried in id order, so the answer is
    /// stable (the database says "first" without fixing an order, so tests must not depend on
    /// which of two covering rules is named).
    pub(crate) fn immutable_rule(
        &self,
        repository: &Repository,
        kind: ItemKind,
        item: &str,
    ) -> Option<ImmutabilityRuleId> {
        let project = self.project_of(repository);
        let mut rules: Vec<&ImmutabilityRule> = self
            .immutability_rules
            .values()
            .filter(|rule| rule.enabled && rule.item_kind == kind)
            .filter(|rule| match rule.scope {
                PolicyScope::Repository(id) => id == repository.id,
                PolicyScope::Project(id) => Some(id) == project,
                PolicyScope::System => false,
            })
            .collect();
        rules.sort_by_key(|rule| rule.id);
        rules
            .into_iter()
            .find(|rule| rule_covers(rule, repository.name.as_str(), item))
            .map(|rule| rule.id)
    }

    /// `rx_active_legal_hold`: an active `legal` hold on the component, its repository or its
    /// project. Release and promotion holds stop lifecycle jobs and promotion moves, which
    /// check them through `LifecycleHoldRepo`, but never the SQL guard.
    pub(crate) fn legal_hold(
        &self,
        repository: &Repository,
        component: Option<ComponentId>,
        now: OffsetDateTime,
    ) -> Option<LifecycleHoldId> {
        let project = self.project_of(repository);
        let mut holds: Vec<_> = self
            .holds
            .values()
            .filter(|hold| hold.kind == HoldKind::Legal && hold.is_active_at(now))
            .filter(|hold| match hold.target {
                HoldTarget::Component(id) => Some(id) == component,
                HoldTarget::Repository(id) => id == repository.id,
                HoldTarget::Project(id) => Some(id) == project,
            })
            .collect();
        holds.sort_by_key(|hold| hold.id);
        holds.first().map(|hold| hold.id)
    }

    /// `assets_guard`, for a delete or for an overwrite with different content: immutability
    /// by path and by the version of the asset's component (skipped by the override), then a
    /// legal hold (never skipped).
    pub(crate) fn guard_asset_change(
        &self,
        repository: &Repository,
        asset: &Asset,
        override_immutable: bool,
        now: OffsetDateTime,
    ) -> Result<(), PolicyViolation> {
        let component = asset
            .component_id
            .and_then(|id| self.components.get(&id).map(|c| (id, c)));
        if !override_immutable {
            let by_path = self.immutable_rule(repository, ItemKind::Path, &asset.path);
            let by_version = component
                .and_then(|(_, c)| c.version.as_deref())
                .and_then(|version| self.immutable_rule(repository, ItemKind::Version, version));
            if let Some(rule) = by_path.or(by_version) {
                return Err(PolicyViolation::Immutable { rule });
            }
        }
        match self.legal_hold(repository, component.map(|(id, _)| id), now) {
            Some(hold) => Err(PolicyViolation::Held { hold }),
            None => Ok(()),
        }
    }

    /// `components_guard`, then `assets_guard` for every asset the delete cascades to.
    pub(crate) fn guard_component_delete(
        &self,
        repository: Option<&Repository>,
        component: &Component,
        override_immutable: bool,
        now: OffsetDateTime,
    ) -> Result<(), PolicyViolation> {
        // A component whose repository row is gone (the double does not force the row to
        // exist) has no scope, so no rule or hold can name it.
        let Some(repository) = repository else {
            return Ok(());
        };
        if !override_immutable
            && let Some(rule) = component
                .version
                .as_deref()
                .and_then(|version| self.immutable_rule(repository, ItemKind::Version, version))
        {
            return Err(PolicyViolation::Immutable { rule });
        }
        if let Some(hold) = self.legal_hold(repository, Some(component.id), now) {
            return Err(PolicyViolation::Held { hold });
        }
        let assets: Vec<&Asset> = self
            .assets
            .values()
            .filter(|asset| asset.component_id == Some(component.id))
            .collect();
        assets.into_iter().try_for_each(|asset| {
            self.guard_asset_change(repository, asset, override_immutable, now)
        })
    }

    /// The write policy of a hosted repository. Proxy and group repositories are written by
    /// the system (cache fills), for which the setting has no meaning, so it is not applied
    /// to them.
    pub(crate) fn check_write_policy(
        repository: &Repository,
        existing: Option<&Asset>,
    ) -> Result<(), PolicyViolation> {
        if repository.repo_type != rustryx_core::model::RepoType::Hosted {
            return Ok(());
        }
        match repository.write_policy {
            WritePolicy::Allow => Ok(()),
            WritePolicy::AllowOnce if existing.is_none() => Ok(()),
            other => Err(PolicyViolation::WritePolicy(other)),
        }
    }

    /// Distinct blob bytes and the number of assets a scope references.
    pub(crate) fn scope_usage(&self, scope: QuotaScope) -> (u64, u64) {
        let mut blobs = std::collections::HashSet::new();
        let mut bytes = 0_u64;
        let mut count = 0_u64;
        for asset in self.assets.values().filter(|a| self.in_scope(a, scope)) {
            count += 1;
            if blobs.insert(asset.blob_id) {
                bytes = bytes.saturating_add(asset.size_bytes);
            }
        }
        (bytes, count)
    }

    /// True when the asset belongs to the scope.
    pub(crate) fn in_scope(&self, asset: &Asset, scope: QuotaScope) -> bool {
        match scope {
            QuotaScope::Repository(id) => asset.repository_id == id,
            QuotaScope::Project(id) => self
                .repositories
                .get(&asset.repository_id)
                .is_some_and(|repository| self.project_of(repository) == Some(id)),
        }
    }

    /// The hard limit of a scope, `None` when unlimited.
    pub(crate) fn scope_limit(&self, scope: QuotaScope) -> Option<u64> {
        match scope {
            QuotaScope::Repository(id) => self.repositories.get(&id).and_then(|r| r.quota_bytes),
            QuotaScope::Project(id) => self
                .project_quotas
                .get(&id)
                .and_then(|quota| quota.limit_bytes),
        }
    }

    /// Edge of the `quota.near_threshold` event for a scope, in percent of the limit.
    pub(crate) fn warn_percent(&self, scope: QuotaScope) -> u8 {
        let configured = match scope {
            QuotaScope::Project(id) => self.project_quotas.get(&id).map(|q| q.warn_percent),
            QuotaScope::Repository(id) => self.repository_warn.get(&id).copied(),
        };
        configured.unwrap_or(rustryx_core::quota::DEFAULT_WARN_PERCENT)
    }

    /// True when a repository that uses blob store `repository_store` can serve a blob that
    /// lives in `blob_store`: the same store, or a group store that lists it as a member.
    pub(crate) fn store_serves(
        &self,
        repository_store: rustryx_core::ids::BlobStoreId,
        blob_store: rustryx_core::ids::BlobStoreId,
    ) -> bool {
        repository_store == blob_store
            || self
                .store_members
                .iter()
                .any(|m| m.group_id == repository_store && m.member_id == blob_store)
    }

    /// The scopes that apply to a repository, in the order the quota statements run:
    /// repository, then project.
    pub(crate) fn scopes_of(&self, repository: &Repository) -> Vec<QuotaScope> {
        let mut scopes = vec![QuotaScope::Repository(repository.id)];
        scopes.extend(self.project_of(repository).map(QuotaScope::Project));
        scopes
    }

    /// Bytes a write adds to a scope, minus what it frees: `+size` when no other asset of the
    /// scope references the incoming blob, `- old size` when the replaced blob keeps no
    /// reference in the scope. `replaced` is the asset the write overwrites, which stops
    /// counting as a referrer.
    pub(crate) fn quota_delta(
        &self,
        scope: QuotaScope,
        incoming: &rustryx_core::model::BlobRecord,
        replaced: Option<&Asset>,
    ) -> i128 {
        let others = |blob| {
            self.assets
                .values()
                .filter(|a| Some(a.id) != replaced.map(|r| r.id))
                .any(|a| a.blob_id == blob && self.in_scope(a, scope))
        };
        let plus = if others(incoming.id) {
            0
        } else {
            i128::from(incoming.size_bytes)
        };
        let minus = replaced
            .filter(|old| self.in_scope(old, scope) && old.blob_id != incoming.id)
            .filter(|old| !others(old.blob_id))
            .map_or(0, |old| i128::from(old.size_bytes));
        plus - minus
    }

    /// `RX006`: refuses a write whose delta would push a limited scope over its limit.
    pub(crate) fn check_quota(
        &self,
        repository: &Repository,
        incoming: &rustryx_core::model::BlobRecord,
        replaced: Option<&Asset>,
    ) -> Result<(), PolicyViolation> {
        for scope in self.scopes_of(repository) {
            let Some(limit) = self.scope_limit(scope) else {
                continue;
            };
            let delta = self.quota_delta(scope, incoming, replaced);
            let (used, _) = self.scope_usage(scope);
            if delta > 0 && i128::from(used) + delta > i128::from(limit) {
                return Err(PolicyViolation::QuotaExceeded(QuotaExceeded {
                    scope,
                    limit_bytes: limit,
                    used_bytes: used,
                    requested_bytes: incoming.size_bytes,
                }));
            }
        }
        Ok(())
    }
}

/// True when the rule's repository selector matches `repository` and its item selector matches
/// `item`. The glob is the regular expression of `rustryx_core::glob`, the text PostgreSQL
/// stores next to it, so both engines evaluate one definition. A rule whose selectors cannot be
/// evaluated covers the item: a guard that cannot decide fails closed.
fn rule_covers(rule: &ImmutabilityRule, repository: &str, item: &str) -> bool {
    let repository_hit = rule
        .repository
        .selects(|pattern| matches(pattern, repository))
        .unwrap_or(true);
    let item_hit = rule
        .item
        .selects(|pattern| matches(pattern, item))
        .unwrap_or(true);
    repository_hit && item_hit
}

/// Evaluates one stored regular expression, answering "matches" when it does not compile.
fn matches(pattern: &str, text: &str) -> bool {
    match Regex::new(pattern) {
        Ok(re) => re.is_match(text),
        Err(_) => true,
    }
}

/// Evaluates a selector against one text with the regular expressions of
/// `rustryx_core::glob`, the way the database does. An invalid selector selects, because a
/// guard that cannot decide fails closed.
pub(crate) fn selector_selects(selector: &NameSelector, text: &str) -> bool {
    selector
        .selects(|pattern| matches(pattern, text))
        .unwrap_or(true)
}
