//! Roles, privileges, content selectors and identity provider group mappings.

use std::collections::{HashMap, HashSet};

use async_trait::async_trait;
use rustryx_core::error::StoreError;
use rustryx_core::identity::{
    ContentSelector, IdpGroupMapping, IdpProvider, Privilege, PrivilegeGrant, Role,
};
use rustryx_core::ids::{ContentSelectorId, IdpGroupMappingId, PrivilegeId, RoleId};
use rustryx_core::ports::{
    IdpMappingRepo, InvalidationEvent, PrivilegeRepo, RoleRepo, SelectorRepo,
};

use super::state::State;
use super::{MemStore, conflict, not_found};

/// True when `target` is reachable from `start` by following nested roles.
///
/// Iterative with a visited set, so it terminates even on data that already contains a
/// cycle (which the write paths refuse, but a direct state edit in a test can create).
fn reaches(roles: &HashMap<RoleId, Role>, start: &[RoleId], target: RoleId) -> bool {
    let mut seen = HashSet::new();
    let mut stack = start.to_vec();
    while let Some(id) = stack.pop() {
        if id == target {
            return true;
        }
        if !seen.insert(id) {
            continue;
        }
        if let Some(role) = roles.get(&id) {
            stack.extend(role.nested_role_ids.iter().copied());
        }
    }
    false
}

fn check_role_write(state: &State, role: &Role) -> Result<(), StoreError> {
    if state
        .roles
        .values()
        .any(|r| r.id != role.id && r.name == role.name)
    {
        return Err(conflict(format!("role {} already exists", role.name)));
    }
    // A role is a cycle member exactly when its own id is reachable through the roles
    // it nests. Other roles are walked with their stored nesting; the role being
    // written is matched by id before its stale stored nesting could be expanded.
    if reaches(&state.roles, &role.nested_role_ids, role.id) {
        return Err(conflict(format!("role {} would nest itself", role.name)));
    }
    Ok(())
}

#[async_trait]
impl RoleRepo for MemStore {
    async fn get(&self, id: RoleId) -> Result<Option<Role>, StoreError> {
        Ok(self.tables()?.roles.get(&id).cloned())
    }

    async fn get_by_name(&self, name: &str) -> Result<Option<Role>, StoreError> {
        let state = self.tables()?;
        Ok(state.roles.values().find(|r| r.name == name).cloned())
    }

    async fn list(&self) -> Result<Vec<Role>, StoreError> {
        let mut all: Vec<Role> = self.tables()?.roles.values().cloned().collect();
        all.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(all)
    }

    async fn create(&self, role: &Role) -> Result<(), StoreError> {
        let mut state = self.tables()?;
        if state.roles.contains_key(&role.id) {
            return Err(conflict(format!("role id {} already exists", role.id)));
        }
        check_role_write(&state, role)?;
        state.roles.insert(role.id, role.clone());
        drop(state);
        self.notify(InvalidationEvent::Rbac);
        Ok(())
    }

    async fn update(&self, role: &Role) -> Result<(), StoreError> {
        let mut state = self.tables()?;
        let existing = state
            .roles
            .get(&role.id)
            .ok_or_else(|| not_found("role", role.id))?;
        if existing.builtin {
            return Err(conflict("built-in roles cannot be edited"));
        }
        check_role_write(&state, role)?;
        let mut next = role.clone();
        // The flag is not editable: a caller cannot promote a role to built-in.
        next.builtin = false;
        state.roles.insert(next.id, next);
        drop(state);
        self.notify(InvalidationEvent::Rbac);
        Ok(())
    }

    async fn delete(&self, id: RoleId) -> Result<(), StoreError> {
        let mut state = self.tables()?;
        let role = state.roles.get(&id).ok_or_else(|| not_found("role", id))?;
        if role.builtin {
            return Err(conflict("built-in roles cannot be deleted"));
        }
        state.roles.remove(&id);
        // Dependents of the role go with it, as foreign keys with ON DELETE CASCADE do.
        for assigned in state.user_roles.values_mut() {
            assigned.retain(|(role_id, _)| *role_id != id);
        }
        for other in state.roles.values_mut() {
            other.nested_role_ids.retain(|nested| *nested != id);
        }
        state.mappings.retain(|_, mapping| mapping.role_id != id);
        drop(state);
        self.notify(InvalidationEvent::Rbac);
        Ok(())
    }

    async fn effective_privileges(&self, roles: &[RoleId]) -> Result<Vec<Privilege>, StoreError> {
        let state = self.tables()?;
        let mut visited = HashSet::new();
        let mut pending = roles.to_vec();
        let mut privilege_ids = HashSet::new();
        // Walk with a visited set rather than trusting the data to be acyclic: this is
        // the authorization read path, and it must terminate whatever is stored.
        while let Some(id) = pending.pop() {
            if !visited.insert(id) {
                continue;
            }
            if let Some(role) = state.roles.get(&id) {
                privilege_ids.extend(role.privilege_ids.iter().copied());
                pending.extend(role.nested_role_ids.iter().copied());
            }
        }
        let mut privileges: Vec<Privilege> = privilege_ids
            .iter()
            .filter_map(|id| state.privileges.get(id).cloned())
            .collect();
        privileges.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(privileges)
    }
}

#[async_trait]
impl PrivilegeRepo for MemStore {
    async fn get(&self, id: PrivilegeId) -> Result<Option<Privilege>, StoreError> {
        Ok(self.tables()?.privileges.get(&id).cloned())
    }

    async fn list(&self) -> Result<Vec<Privilege>, StoreError> {
        let mut all: Vec<Privilege> = self.tables()?.privileges.values().cloned().collect();
        all.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(all)
    }

    async fn create(&self, privilege: &Privilege) -> Result<(), StoreError> {
        let mut state = self.tables()?;
        let taken = state
            .privileges
            .values()
            .any(|p| p.id == privilege.id || p.name == privilege.name);
        if taken {
            return Err(conflict(format!(
                "privilege {} already exists",
                privilege.name
            )));
        }
        state.privileges.insert(privilege.id, privilege.clone());
        drop(state);
        self.notify(InvalidationEvent::Rbac);
        Ok(())
    }

    async fn update(&self, privilege: &Privilege) -> Result<(), StoreError> {
        let mut state = self.tables()?;
        let existing = state
            .privileges
            .get(&privilege.id)
            .ok_or_else(|| not_found("privilege", privilege.id))?;
        if existing.builtin {
            return Err(conflict("built-in privileges cannot be edited"));
        }
        let name_taken = state
            .privileges
            .values()
            .any(|p| p.id != privilege.id && p.name == privilege.name);
        if name_taken {
            return Err(conflict(format!(
                "privilege {} already exists",
                privilege.name
            )));
        }
        let mut next = privilege.clone();
        next.builtin = false;
        state.privileges.insert(next.id, next);
        drop(state);
        self.notify(InvalidationEvent::Rbac);
        Ok(())
    }

    async fn delete(&self, id: PrivilegeId) -> Result<(), StoreError> {
        let mut state = self.tables()?;
        let privilege = state
            .privileges
            .get(&id)
            .ok_or_else(|| not_found("privilege", id))?;
        if privilege.builtin {
            return Err(conflict("built-in privileges cannot be deleted"));
        }
        state.privileges.remove(&id);
        for role in state.roles.values_mut() {
            role.privilege_ids.retain(|attached| *attached != id);
        }
        drop(state);
        self.notify(InvalidationEvent::Rbac);
        Ok(())
    }
}

#[async_trait]
impl SelectorRepo for MemStore {
    async fn get(&self, id: ContentSelectorId) -> Result<Option<ContentSelector>, StoreError> {
        Ok(self.tables()?.selectors.get(&id).cloned())
    }

    async fn list(&self) -> Result<Vec<ContentSelector>, StoreError> {
        let mut all: Vec<ContentSelector> = self.tables()?.selectors.values().cloned().collect();
        all.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(all)
    }

    async fn create(&self, selector: &ContentSelector) -> Result<(), StoreError> {
        let mut state = self.tables()?;
        let taken = state
            .selectors
            .values()
            .any(|s| s.id == selector.id || s.name == selector.name);
        if taken {
            return Err(conflict(format!(
                "content selector {} already exists",
                selector.name
            )));
        }
        state.selectors.insert(selector.id, selector.clone());
        drop(state);
        self.notify(InvalidationEvent::Rbac);
        Ok(())
    }

    async fn update(&self, selector: &ContentSelector) -> Result<(), StoreError> {
        let mut state = self.tables()?;
        if !state.selectors.contains_key(&selector.id) {
            return Err(not_found("content selector", selector.id));
        }
        let name_taken = state
            .selectors
            .values()
            .any(|s| s.id != selector.id && s.name == selector.name);
        if name_taken {
            return Err(conflict(format!(
                "content selector {} already exists",
                selector.name
            )));
        }
        state.selectors.insert(selector.id, selector.clone());
        drop(state);
        self.notify(InvalidationEvent::Rbac);
        Ok(())
    }

    async fn delete(&self, id: ContentSelectorId) -> Result<(), StoreError> {
        let mut state = self.tables()?;
        if !state.selectors.contains_key(&id) {
            return Err(not_found("content selector", id));
        }
        let referenced = state.privileges.values().any(|p| {
            matches!(
                &p.grant,
                PrivilegeGrant::RepositoryContentSelector { selector_id, .. } if *selector_id == id
            )
        });
        if referenced {
            return Err(conflict("content selector is referenced by a privilege"));
        }
        state.selectors.remove(&id);
        drop(state);
        self.notify(InvalidationEvent::Rbac);
        Ok(())
    }
}

#[async_trait]
impl IdpMappingRepo for MemStore {
    async fn list(&self, provider: IdpProvider) -> Result<Vec<IdpGroupMapping>, StoreError> {
        let state = self.tables()?;
        let mut mappings: Vec<IdpGroupMapping> = state
            .mappings
            .values()
            .filter(|m| m.provider == provider)
            .cloned()
            .collect();
        mappings.sort_by(|a, b| {
            (a.priority, &a.external_group, a.id).cmp(&(b.priority, &b.external_group, b.id))
        });
        Ok(mappings)
    }

    async fn upsert(&self, mapping: &IdpGroupMapping) -> Result<(), StoreError> {
        let mut state = self.tables()?;
        let existing = state
            .mappings
            .values()
            .find(|m| m.provider == mapping.provider && m.external_group == mapping.external_group)
            .map(|m| m.id);
        // Like `ON CONFLICT (provider, external_group) DO UPDATE`: the row keeps its id and
        // takes the new role and priority. A new row must not reuse another row's id.
        if existing.is_none() && state.mappings.contains_key(&mapping.id) {
            return Err(conflict(format!(
                "mapping id {} already exists",
                mapping.id
            )));
        }
        let mut next = mapping.clone();
        next.id = existing.unwrap_or(mapping.id);
        state.mappings.insert(next.id, next);
        drop(state);
        self.notify(InvalidationEvent::Rbac);
        Ok(())
    }

    async fn delete(&self, id: IdpGroupMappingId) -> Result<(), StoreError> {
        let mut state = self.tables()?;
        if state.mappings.remove(&id).is_none() {
            return Err(not_found("idp group mapping", id));
        }
        drop(state);
        self.notify(InvalidationEvent::Rbac);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::seed::builtin_role_id;

    fn plain_role(name: &str, nested: Vec<RoleId>) -> Role {
        Role {
            id: RoleId::generate(),
            name: name.to_owned(),
            description: String::new(),
            builtin: false,
            privilege_ids: Vec::new(),
            nested_role_ids: nested,
        }
    }

    /// The write paths refuse cycles, so the only way to get one into the tables is to
    /// edit them directly. That is exactly the corrupt-data case the read path guards.
    #[tokio::test]
    async fn effective_privileges_terminates_on_data_that_already_contains_a_cycle() {
        let store = MemStore::with_builtin_roles();
        let developer = builtin_role_id("nx-developer").unwrap();
        let a = plain_role("a", vec![developer]);
        let b = plain_role("b", vec![a.id]);
        {
            let mut tables = store.tables().unwrap();
            let mut cyclic = a.clone();
            cyclic.nested_role_ids.push(b.id);
            tables.roles.insert(cyclic.id, cyclic);
            tables.roles.insert(b.id, b.clone());
        }
        let privileges = store.effective_privileges(&[b.id]).await.unwrap();
        let names: Vec<&str> = privileges.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, ["nx-developer-repository-view"]);
    }

    #[test]
    fn reaches_follows_chains_and_ignores_unknown_ids() {
        let c = plain_role("c", Vec::new());
        let b = plain_role("b", vec![c.id]);
        let a = plain_role("a", vec![b.id]);
        let roles: HashMap<RoleId, Role> = [&a, &b, &c]
            .into_iter()
            .map(|r| (r.id, r.clone()))
            .collect();
        assert!(reaches(&roles, &[a.id], c.id));
        assert!(!reaches(&roles, &[c.id], a.id));
        assert!(!reaches(&roles, &[RoleId::generate()], a.id));
    }
}
