//! Labels and their links to components.

use async_trait::async_trait;
use rustryx_core::error::StoreError;
use rustryx_core::ids::{ComponentId, LabelId, UserId};
use rustryx_core::label::{ComponentLabel, Label, LabelScope, MAX_LABELS_PER_COMPONENT};
use rustryx_core::ports::{LabelRepo, Page, PageRequest};

use super::paging::paginate;
use super::state::State;
use super::support::bump_version;
use super::{MemStore, conflict, not_found};

impl State {
    fn label_name_taken(&self, label: &Label) -> bool {
        self.labels
            .values()
            .any(|l| l.id != label.id && l.scope == label.scope && l.name == label.name)
    }

    /// The scope guard of the real schema (`RX008`): a project label sits only on components of
    /// a repository of that project. System labels sit anywhere.
    fn check_label_scope(&self, component: ComponentId, label: &Label) -> Result<(), StoreError> {
        let LabelScope::Project(project) = label.scope else {
            return Ok(());
        };
        let in_project = self
            .components
            .get(&component)
            .and_then(|c| self.repositories.get(&c.repository_id))
            .is_some_and(|repository| self.project_of(repository) == Some(project));
        if in_project {
            Ok(())
        } else {
            Err(conflict(
                "a project label can only be applied to a component of that project",
            ))
        }
    }
}

#[async_trait]
impl LabelRepo for MemStore {
    async fn create(&self, label: &Label) -> Result<(), StoreError> {
        let mut state = self.tables()?;
        // The CHECK constraints of the table, reported as conflicts like every constraint.
        label
            .validate()
            .map_err(|error| conflict(error.to_string()))?;
        if let LabelScope::Project(project) = label.scope
            && !state.projects.contains_key(&project)
        {
            return Err(conflict(format!("project {project} does not exist")));
        }
        if state.labels.contains_key(&label.id) || state.label_name_taken(label) {
            return Err(conflict(format!("label {} already exists", label.name)));
        }
        state.labels.insert(label.id, label.clone());
        Ok(())
    }

    async fn get(&self, id: LabelId) -> Result<Option<Label>, StoreError> {
        Ok(self.tables()?.labels.get(&id).cloned())
    }

    async fn list(&self, scope: LabelScope, page: &PageRequest) -> Result<Page<Label>, StoreError> {
        let rows: Vec<(String, Label)> = self
            .tables()?
            .labels
            .values()
            .filter(|label| label.scope == scope)
            .map(|label| (label.name.as_str().to_owned(), label.clone()))
            .collect();
        paginate(rows, page)
    }

    async fn update(&self, label: &Label, expected_version: i64) -> Result<(), StoreError> {
        let now = self.now();
        let mut state = self.tables()?;
        let existing = state
            .labels
            .get(&label.id)
            .ok_or_else(|| not_found("label", label.id))?;
        let version = bump_version(existing.version, expected_version)?;
        label
            .validate()
            .map_err(|error| conflict(error.to_string()))?;
        let mut next = label.clone();
        // The scope is not editable: moving a label between projects would carry its links
        // across the scope guard.
        next.scope = existing.scope;
        next.version = version;
        next.created_at = existing.created_at;
        next.updated_at = now;
        if state.label_name_taken(&next) {
            return Err(conflict(format!("label {} already exists", next.name)));
        }
        state.labels.insert(next.id, next);
        Ok(())
    }

    async fn delete(&self, id: LabelId) -> Result<u64, StoreError> {
        let mut state = self.tables()?;
        if state.labels.remove(&id).is_none() {
            return Err(not_found("label", id));
        }
        let before = state.component_labels.len();
        state.component_labels.retain(|(_, label), _| *label != id);
        Ok(u64::try_from(before - state.component_labels.len()).unwrap_or(u64::MAX))
    }

    async fn apply(
        &self,
        component: ComponentId,
        label: LabelId,
        by: Option<UserId>,
    ) -> Result<(), StoreError> {
        let now = self.now();
        let mut state = self.tables()?;
        let stored = state
            .labels
            .get(&label)
            .cloned()
            .ok_or_else(|| not_found("label", label))?;
        if !state.components.contains_key(&component) {
            return Err(not_found("component", component));
        }
        // Idempotent: an existing link is left as it is, with its first applier and time.
        if state.component_labels.contains_key(&(component, label)) {
            return Ok(());
        }
        state.check_label_scope(component, &stored)?;
        let on_component = state
            .component_labels
            .keys()
            .filter(|(c, _)| *c == component)
            .count();
        if on_component >= MAX_LABELS_PER_COMPONENT {
            return Err(StoreError::Invalid(
                "a component carries at most 50 labels".to_owned(),
            ));
        }
        state.component_labels.insert(
            (component, label),
            ComponentLabel {
                component_id: component,
                label_id: label,
                applied_by: by,
                applied_at: now,
            },
        );
        Ok(())
    }

    async fn unapply(&self, component: ComponentId, label: LabelId) -> Result<(), StoreError> {
        self.tables()?.component_labels.remove(&(component, label));
        Ok(())
    }

    async fn labels_of(&self, component: ComponentId) -> Result<Vec<Label>, StoreError> {
        let state = self.tables()?;
        let mut labels: Vec<Label> = state
            .component_labels
            .keys()
            .filter(|(c, _)| *c == component)
            .filter_map(|(_, label)| state.labels.get(label).cloned())
            .collect();
        labels.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(labels)
    }

    async fn usage(&self, label: LabelId) -> Result<u64, StoreError> {
        let state = self.tables()?;
        let count = state
            .component_labels
            .keys()
            .filter(|(_, l)| *l == label)
            .count();
        Ok(u64::try_from(count).unwrap_or(u64::MAX))
    }
}
