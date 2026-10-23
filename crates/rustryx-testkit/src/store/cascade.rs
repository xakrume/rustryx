//! Row removal that follows a foreign key `ON DELETE CASCADE`.
//!
//! Kept in one place so that the tables that hang off a repository, a component or a project
//! are listed once. Every delete path of the store calls these after its guards passed.

use rustryx_core::ids::{AssetId, ComponentId, ProjectId, RepositoryId};

use super::state::State;

impl State {
    /// Removes a component's label links and its assets.
    pub(crate) fn remove_component_rows(&mut self, id: ComponentId) {
        self.component_labels
            .retain(|(component, _), _| *component != id);
        let doomed: Vec<AssetId> = self
            .assets
            .values()
            .filter(|a| a.component_id == Some(id))
            .map(|a| a.id)
            .collect();
        for asset_id in doomed {
            self.remove_asset(asset_id);
        }
        self.holds
            .retain(|_, hold| hold.target != rustryx_core::lifecycle::HoldTarget::Component(id));
        // Scan results and exceptions hang off the component (`ON DELETE CASCADE`).
        self.security_states.remove(&id);
        let reports: Vec<_> = self
            .scan_reports
            .iter()
            .filter(|report| report.component_id == id)
            .map(|report| report.id)
            .collect();
        self.scan_reports.retain(|report| report.component_id != id);
        for report in reports {
            self.findings.remove(&report);
        }
        self.allowlist
            .retain(|_, entry| entry.scope != rustryx_core::scan::AllowlistScope::Component(id));
    }

    /// Removes a repository's components, assets and the rows scoped to the repository.
    /// Blob rows stay: the port leaves them for garbage collection.
    pub(crate) fn remove_repository_rows(&mut self, id: RepositoryId) {
        let components: Vec<ComponentId> = self
            .components
            .values()
            .filter(|c| c.repository_id == id)
            .map(|c| c.id)
            .collect();
        for component in components {
            self.components.remove(&component);
            self.remove_component_rows(component);
        }
        let assets: Vec<AssetId> = self
            .assets
            .values()
            .filter(|a| a.repository_id == id)
            .map(|a| a.id)
            .collect();
        for asset in assets {
            self.remove_asset(asset);
        }
        self.repository_warn.remove(&id);
        let scope = rustryx_core::quota::QuotaScope::Repository(id);
        self.quota_warned.remove(&scope);
        self.quota_recounted.remove(&scope);
        self.immutability_rules
            .retain(|_, rule| rule.scope != rustryx_core::lifecycle::PolicyScope::Repository(id));
        self.holds
            .retain(|_, hold| hold.target != rustryx_core::lifecycle::HoldTarget::Repository(id));
        self.remove_policies_of(rustryx_core::lifecycle::PolicyScope::Repository(id));
        self.remove_notifiers_of(rustryx_core::webhook::NotifyScope::Repository(id));
        self.promotion_targets.remove(&id);
    }

    /// Removes the policies scoped to a project or repository: retention, scan and trust.
    fn remove_policies_of(&mut self, scope: rustryx_core::lifecycle::PolicyScope) {
        self.retention_policies
            .retain(|_, policy| policy.scope != scope);
        self.scan_policies.retain(|_, policy| policy.scope != scope);
        self.trust_policies
            .retain(|_, policy| policy.scope != scope);
    }

    /// Removes the webhooks, with their deliveries, and the subscriptions of one scope.
    fn remove_notifiers_of(&mut self, scope: rustryx_core::webhook::NotifyScope) {
        let hooks: Vec<_> = self
            .webhooks
            .values()
            .filter(|hook| hook.scope == scope)
            .map(|hook| hook.id)
            .collect();
        for hook in hooks {
            self.webhooks.remove(&hook);
            self.webhook_deliveries
                .retain(|row| row.delivery.webhook_id != hook);
        }
        self.subscriptions
            .retain(|_, subscription| subscription.scope != scope);
    }

    /// Removes the rows scoped to a project. The caller checked that no repository is
    /// attached, because that foreign key is `RESTRICT`.
    pub(crate) fn remove_project_rows(&mut self, id: ProjectId) {
        self.members.retain(|_, member| member.project_id != id);
        self.robots.retain(|_, robot| robot.project_id != Some(id));
        let doomed_labels: Vec<_> = self
            .labels
            .values()
            .filter(|label| label.scope == rustryx_core::label::LabelScope::Project(id))
            .map(|label| label.id)
            .collect();
        for label in doomed_labels {
            self.labels.remove(&label);
            self.component_labels.retain(|(_, link), _| *link != label);
        }
        self.project_quotas.remove(&id);
        let scope = rustryx_core::quota::QuotaScope::Project(id);
        self.quota_warned.remove(&scope);
        self.quota_recounted.remove(&scope);
        self.immutability_rules
            .retain(|_, rule| rule.scope != rustryx_core::lifecycle::PolicyScope::Project(id));
        self.holds
            .retain(|_, hold| hold.target != rustryx_core::lifecycle::HoldTarget::Project(id));
        self.remove_policies_of(rustryx_core::lifecycle::PolicyScope::Project(id));
        self.remove_notifiers_of(rustryx_core::webhook::NotifyScope::Project(id));
        self.allowlist
            .retain(|_, entry| entry.scope != rustryx_core::scan::AllowlistScope::Project(id));
    }
}
