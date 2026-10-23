//! Built-in roles and their privileges.
//!
//! Ids are constants, not generated, for two reasons. A principal built by
//! `fixtures::principal_with_roles(&["nx-admin"])` then carries the same role id as the
//! seeded role, so `RoleRepo::effective_privileges` resolves it. And a failing test
//! prints the same ids on every run.

use rustryx_core::identity::{
    Action, Privilege, PrivilegeGrant, ROLE_NX_ADMIN, ROLE_NX_ANONYMOUS, ROLE_NX_DEVELOPER, Role,
};
use rustryx_core::ids::{PrivilegeId, RoleId};
use uuid::Uuid;

use super::state::State;

const ROLE_ADMIN: RoleId =
    RoleId::from_uuid(Uuid::from_u128(0x0192_6f00_0000_7000_8000_0000_0000_0001));
const ROLE_DEVELOPER: RoleId =
    RoleId::from_uuid(Uuid::from_u128(0x0192_6f00_0000_7000_8000_0000_0000_0002));
const ROLE_ANONYMOUS: RoleId =
    RoleId::from_uuid(Uuid::from_u128(0x0192_6f00_0000_7000_8000_0000_0000_0003));
const PRIVILEGE_ADMIN: PrivilegeId =
    PrivilegeId::from_uuid(Uuid::from_u128(0x0192_6f00_0000_7000_8000_0000_0000_0011));
const PRIVILEGE_DEVELOPER: PrivilegeId =
    PrivilegeId::from_uuid(Uuid::from_u128(0x0192_6f00_0000_7000_8000_0000_0000_0012));
const PRIVILEGE_ANONYMOUS: PrivilegeId =
    PrivilegeId::from_uuid(Uuid::from_u128(0x0192_6f00_0000_7000_8000_0000_0000_0013));

/// Id of a built-in role as seeded by `MemStore::with_builtin_roles`, or `None` for any
/// other name.
#[must_use]
pub fn builtin_role_id(name: &str) -> Option<RoleId> {
    match name {
        ROLE_NX_ADMIN => Some(ROLE_ADMIN),
        ROLE_NX_DEVELOPER => Some(ROLE_DEVELOPER),
        ROLE_NX_ANONYMOUS => Some(ROLE_ANONYMOUS),
        _ => None,
    }
}

fn privilege(
    id: PrivilegeId,
    name: &str,
    description: &str,
    grant: PrivilegeGrant,
    actions: &[Action],
) -> Privilege {
    Privilege {
        id,
        name: name.to_owned(),
        description: description.to_owned(),
        builtin: true,
        grant,
        actions: actions.to_vec(),
    }
}

fn role(id: RoleId, name: &str, description: &str, privilege_id: PrivilegeId) -> Role {
    Role {
        id,
        name: name.to_owned(),
        description: description.to_owned(),
        builtin: true,
        privilege_ids: vec![privilege_id],
        nested_role_ids: Vec::new(),
    }
}

fn any_repository() -> PrivilegeGrant {
    PrivilegeGrant::RepositoryView {
        format: None,
        repository: "*".to_owned(),
    }
}

pub(super) fn insert_builtin_roles(state: &mut State) {
    let privileges = [
        privilege(
            PRIVILEGE_ADMIN,
            "nx-admin-all",
            "Every action on everything, including administration",
            PrivilegeGrant::Admin,
            &[Action::Browse, Action::Read, Action::Write, Action::Delete],
        ),
        privilege(
            PRIVILEGE_DEVELOPER,
            "nx-developer-repository-view",
            "Browse, read and write on every repository",
            any_repository(),
            &[Action::Browse, Action::Read, Action::Write],
        ),
        privilege(
            PRIVILEGE_ANONYMOUS,
            "nx-anonymous-repository-view",
            "Browse and read on every repository",
            any_repository(),
            &[Action::Browse, Action::Read],
        ),
    ];
    for privilege in privileges {
        state.privileges.insert(privilege.id, privilege);
    }
    let roles = [
        role(ROLE_ADMIN, ROLE_NX_ADMIN, "Administrator", PRIVILEGE_ADMIN),
        role(
            ROLE_DEVELOPER,
            ROLE_NX_DEVELOPER,
            "Read and write on repositories",
            PRIVILEGE_DEVELOPER,
        ),
        role(
            ROLE_ANONYMOUS,
            ROLE_NX_ANONYMOUS,
            "Unauthenticated callers",
            PRIVILEGE_ANONYMOUS,
        ),
    ];
    for role in roles {
        state.roles.insert(role.id, role);
    }
}
