//! Authorization decisions from roles, grants and content selectors.
//!
//! Anonymous callers, token scopes and visibility are in `rbac_access.rs`; the
//! negative requirements ("must not ...") are in `rbac_must_not.rs`.

// Tests may unwrap and expect: a panic is the failure signal there.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use common::{
    Env, SpyStore, builtin_principal, principal, resource, resource_with_coordinate,
    selector_grant, view,
};
use rustryx_core::error::StoreError;
use rustryx_core::identity::Principal;
use rustryx_core::identity::{Action, Privilege, ROLE_NX_ADMIN, ROLE_NX_DEVELOPER, Role};
use rustryx_core::ids::{PrivilegeId, RoleId};
use rustryx_core::model::RepoFormat;
use rustryx_core::ports::RoleRepo;
use rustryx_core::ports::auth::{Decision, RbacChecker, ResourceRef};
use rustryx_rbac::{Rbac, RbacConfig};
use std::sync::Arc;

async fn decide(env: &Env, p: &Principal, action: Action, r: &ResourceRef) -> Decision {
    env.rbac.check(p, action, r).await.unwrap()
}

async fn allowed(env: &Env, p: &Principal, action: Action, r: &ResourceRef) -> bool {
    decide(env, p, action, r).await == Decision::Allow
}

#[tokio::test]
async fn admin_may_do_everything_everywhere() {
    let env = Env::new();
    let admin = builtin_principal(ROLE_NX_ADMIN);
    for action in [Action::Browse, Action::Read, Action::Write, Action::Delete] {
        for format in RepoFormat::ALL {
            assert!(allowed(&env, &admin, action, &resource("any", format, "/x")).await);
        }
    }
    assert!(env.rbac.is_admin(&admin).await.unwrap());
}

#[tokio::test]
async fn developer_reads_and_writes_but_cannot_delete() {
    let env = Env::new();
    let dev = builtin_principal(ROLE_NX_DEVELOPER);
    let res = resource("maven-releases", RepoFormat::Maven2, "/a/b");
    assert!(allowed(&env, &dev, Action::Browse, &res).await);
    assert!(allowed(&env, &dev, Action::Read, &res).await);
    assert!(allowed(&env, &dev, Action::Write, &res).await);
    assert!(!allowed(&env, &dev, Action::Delete, &res).await);
    assert!(!env.rbac.is_admin(&dev).await.unwrap());
}

#[tokio::test]
async fn authenticated_user_without_roles_has_no_access() {
    let env = Env::new();
    let nobody = principal(&[]);
    let res = resource("r", RepoFormat::Raw, "/x");
    for action in [Action::Browse, Action::Read, Action::Write, Action::Delete] {
        assert!(!allowed(&env, &nobody, action, &res).await);
    }
    assert!(
        !env.rbac
            .can_see_repository(&nobody, "r", RepoFormat::Raw)
            .await
            .unwrap()
    );
}

#[tokio::test]
async fn roles_without_known_ids_grant_nothing() {
    let env = Env::new();
    let ghost = principal(&[(RoleId::generate(), "ghost")]);
    assert!(
        !allowed(
            &env,
            &ghost,
            Action::Read,
            &resource("r", RepoFormat::Raw, "/x")
        )
        .await
    );
}

#[tokio::test]
async fn repository_grant_matches_exact_name_or_star() {
    let env = Env::new();
    let only_a = env
        .simple_role("only-a", view("a", None), &[Action::Read])
        .await;
    let all = env
        .simple_role("all", view("*", None), &[Action::Read])
        .await;
    let user_a = principal(&[(only_a, "only-a")]);
    let user_all = principal(&[(all, "all")]);
    let in_a = resource("a", RepoFormat::Raw, "/x");
    let in_b = resource("b", RepoFormat::Raw, "/x");
    let in_prefix = resource("a-more", RepoFormat::Raw, "/x");
    assert!(allowed(&env, &user_a, Action::Read, &in_a).await);
    assert!(!allowed(&env, &user_a, Action::Read, &in_b).await);
    assert!(
        !allowed(&env, &user_a, Action::Read, &in_prefix).await,
        "no prefix matching"
    );
    assert!(allowed(&env, &user_all, Action::Read, &in_a).await);
    assert!(allowed(&env, &user_all, Action::Read, &in_b).await);
}

#[tokio::test]
async fn format_restriction_applies() {
    let env = Env::new();
    let role = env
        .simple_role(
            "maven-only",
            view("*", Some(RepoFormat::Maven2)),
            &[Action::Read],
        )
        .await;
    let user = principal(&[(role, "maven-only")]);
    assert!(
        allowed(
            &env,
            &user,
            Action::Read,
            &resource("m", RepoFormat::Maven2, "/x")
        )
        .await
    );
    assert!(
        !allowed(
            &env,
            &user,
            Action::Read,
            &resource("n", RepoFormat::Npm, "/x")
        )
        .await
    );
}

#[tokio::test]
async fn actions_are_exact_there_is_no_implication() {
    let env = Env::new();
    let role = env
        .simple_role("read-only", view("*", None), &[Action::Read])
        .await;
    let user = principal(&[(role, "read-only")]);
    let res = resource("r", RepoFormat::Raw, "/x");
    assert!(allowed(&env, &user, Action::Read, &res).await);
    assert!(!allowed(&env, &user, Action::Browse, &res).await);
    assert!(!allowed(&env, &user, Action::Write, &res).await);
    assert!(!allowed(&env, &user, Action::Delete, &res).await);
}

#[tokio::test]
async fn one_matching_privilege_among_many_is_enough() {
    let env = Env::new();
    let narrow = env
        .privilege("narrow", view("a", None), &[Action::Read])
        .await;
    let other = env
        .privilege("other", view("b", None), &[Action::Write])
        .await;
    let wide = env
        .privilege("wide", view("*", None), &[Action::Delete])
        .await;
    let role = env.role("mixed", &[narrow, other, wide], &[]).await;
    let user = principal(&[(role, "mixed")]);
    assert!(
        allowed(
            &env,
            &user,
            Action::Read,
            &resource("a", RepoFormat::Raw, "/x")
        )
        .await
    );
    assert!(
        allowed(
            &env,
            &user,
            Action::Write,
            &resource("b", RepoFormat::Raw, "/x")
        )
        .await
    );
    assert!(
        allowed(
            &env,
            &user,
            Action::Delete,
            &resource("z", RepoFormat::Raw, "/x")
        )
        .await
    );
    assert!(
        !allowed(
            &env,
            &user,
            Action::Write,
            &resource("a", RepoFormat::Raw, "/x")
        )
        .await
    );
}

#[tokio::test]
async fn several_roles_combine_their_privileges() {
    let env = Env::new();
    let reader = env
        .simple_role("reader", view("a", None), &[Action::Read])
        .await;
    let writer = env
        .simple_role("writer", view("a", None), &[Action::Write])
        .await;
    let user = principal(&[(reader, "reader"), (writer, "writer"), (reader, "reader")]);
    let res = resource("a", RepoFormat::Raw, "/x");
    assert!(allowed(&env, &user, Action::Read, &res).await);
    assert!(allowed(&env, &user, Action::Write, &res).await);
}

#[tokio::test]
async fn nested_roles_inherit_privileges_through_a_chain() {
    let env = Env::new();
    let leaf = env
        .simple_role("leaf", view("deep", None), &[Action::Read])
        .await;
    let mut parent = leaf;
    for level in 0..5 {
        parent = env.role(&format!("level-{level}"), &[], &[parent]).await;
    }
    let user = principal(&[(parent, "level-4")]);
    assert!(
        allowed(
            &env,
            &user,
            Action::Read,
            &resource("deep", RepoFormat::Raw, "/x")
        )
        .await
    );
    assert!(
        !allowed(
            &env,
            &user,
            Action::Write,
            &resource("deep", RepoFormat::Raw, "/x")
        )
        .await
    );
}

#[tokio::test]
async fn diamond_nesting_counts_each_privilege_once_without_trouble() {
    let env = Env::new();
    let base = env
        .simple_role("base", view("d", None), &[Action::Read])
        .await;
    let left = env.role("left", &[], &[base]).await;
    let right = env.role("right", &[], &[base]).await;
    let top = env.role("top", &[], &[left, right]).await;
    let user = principal(&[(top, "top")]);
    assert!(
        allowed(
            &env,
            &user,
            Action::Read,
            &resource("d", RepoFormat::Raw, "/x")
        )
        .await
    );
}

/// Store double that answers `effective_privileges` the way a buggy or
/// cyclic expansion could: the same privilege many times over.
struct DuplicatingRoles {
    inner: SpyStore,
    privilege: Privilege,
}

#[async_trait::async_trait]
impl RoleRepo for DuplicatingRoles {
    async fn get(&self, id: RoleId) -> Result<Option<Role>, StoreError> {
        RoleRepo::get(&self.inner, id).await
    }
    async fn get_by_name(&self, name: &str) -> Result<Option<Role>, StoreError> {
        self.inner.get_by_name(name).await
    }
    async fn list(&self) -> Result<Vec<Role>, StoreError> {
        RoleRepo::list(&self.inner).await
    }
    async fn create(&self, role: &Role) -> Result<(), StoreError> {
        RoleRepo::create(&self.inner, role).await
    }
    async fn update(&self, role: &Role) -> Result<(), StoreError> {
        RoleRepo::update(&self.inner, role).await
    }
    async fn delete(&self, id: RoleId) -> Result<(), StoreError> {
        RoleRepo::delete(&self.inner, id).await
    }
    async fn effective_privileges(&self, _roles: &[RoleId]) -> Result<Vec<Privilege>, StoreError> {
        Ok(vec![self.privilege.clone(); 500])
    }
}

#[tokio::test]
async fn repeated_privileges_from_a_cyclic_expansion_change_nothing() {
    let env = Env::new();
    let privilege = Privilege {
        id: PrivilegeId::generate(),
        name: "dup".into(),
        description: String::new(),
        builtin: false,
        grant: view("a", None),
        actions: vec![Action::Read],
    };
    let roles = Arc::new(DuplicatingRoles {
        inner: env.store.clone(),
        privilege,
    });
    let rbac = Rbac::new(roles, Arc::new(env.store.clone()), &RbacConfig::default());
    let user = principal(&[(RoleId::generate(), "cyclic")]);
    assert_eq!(
        rbac.check(&user, Action::Read, &resource("a", RepoFormat::Raw, "/x"))
            .await
            .unwrap(),
        Decision::Allow
    );
    assert_eq!(
        rbac.check(&user, Action::Write, &resource("a", RepoFormat::Raw, "/x"))
            .await
            .unwrap(),
        Decision::Deny
    );
}

#[tokio::test]
async fn the_store_refuses_cycles_so_expansion_can_rely_on_acyclic_roles() {
    // Cycle safety of `effective_privileges` itself belongs to the store; this
    // pins the part rbac relies on: the write path rejects a cycle.
    let env = Env::new();
    let a = env.role("cycle-a", &[], &[]).await;
    let b = env.role("cycle-b", &[], &[a]).await;
    let looped = Role {
        id: a,
        name: "cycle-a".into(),
        description: String::new(),
        builtin: false,
        privilege_ids: Vec::new(),
        nested_role_ids: vec![b],
    };
    let err = RoleRepo::update(&env.store.inner, &looped)
        .await
        .unwrap_err();
    assert!(matches!(err, StoreError::Conflict(_)));
}

#[tokio::test]
async fn selector_privilege_allows_only_matching_paths() {
    let env = Env::new();
    let selector = env
        .selector(
            "example-org",
            "format == \"maven2\" && path.startsWith(\"/org/example/\")",
        )
        .await;
    let role = env
        .simple_role("example", selector_grant("*", selector), &[Action::Read])
        .await;
    let user = principal(&[(role, "example")]);
    let inside = resource(
        "maven-releases",
        RepoFormat::Maven2,
        "org/example/lib/1.0/lib-1.0.jar",
    );
    let leading_slash = resource(
        "maven-releases",
        RepoFormat::Maven2,
        "/org/example/lib/1.0/lib-1.0.jar",
    );
    let outside = resource("maven-releases", RepoFormat::Maven2, "org/other/lib.jar");
    let wrong_format = resource("npm", RepoFormat::Npm, "org/example/lib");
    assert!(allowed(&env, &user, Action::Read, &inside).await);
    assert!(allowed(&env, &user, Action::Read, &leading_slash).await);
    assert!(!allowed(&env, &user, Action::Read, &outside).await);
    assert!(!allowed(&env, &user, Action::Read, &wrong_format).await);
    assert!(
        !allowed(&env, &user, Action::Write, &inside).await,
        "only the listed actions"
    );
}

#[tokio::test]
async fn selector_privilege_is_limited_to_its_repository() {
    let env = Env::new();
    let selector = env.selector("everything", "true").await;
    let role = env
        .simple_role("sel-a", selector_grant("repo-a", selector), &[Action::Read])
        .await;
    let user = principal(&[(role, "sel-a")]);
    assert!(
        allowed(
            &env,
            &user,
            Action::Read,
            &resource("repo-a", RepoFormat::Raw, "/x")
        )
        .await
    );
    assert!(
        !allowed(
            &env,
            &user,
            Action::Read,
            &resource("repo-b", RepoFormat::Raw, "/x")
        )
        .await
    );
}

#[tokio::test]
async fn selector_sees_repository_and_coordinate() {
    let env = Env::new();
    let selector = env
        .selector(
            "acme-libs",
            "repository == \"maven-releases\" && (coordinate.groupId == \"com.acme\" || coordinate.artifactId.startsWith(\"lib-\"))",
        )
        .await;
    let role = env
        .simple_role("acme", selector_grant("*", selector), &[Action::Read])
        .await;
    let user = principal(&[(role, "acme")]);
    let by_group = resource_with_coordinate(
        "maven-releases",
        RepoFormat::Maven2,
        "/p",
        Some("com.acme"),
        Some("app"),
        Some("1"),
    );
    let by_name = resource_with_coordinate(
        "maven-releases",
        RepoFormat::Maven2,
        "/p",
        Some("org.x"),
        Some("lib-core"),
        Some("1"),
    );
    let neither = resource_with_coordinate(
        "maven-releases",
        RepoFormat::Maven2,
        "/p",
        Some("org.x"),
        Some("app"),
        Some("1"),
    );
    let other_repo = resource_with_coordinate(
        "maven-snapshots",
        RepoFormat::Maven2,
        "/p",
        Some("com.acme"),
        Some("app"),
        Some("1"),
    );
    assert!(allowed(&env, &user, Action::Read, &by_group).await);
    assert!(allowed(&env, &user, Action::Read, &by_name).await);
    assert!(!allowed(&env, &user, Action::Read, &neither).await);
    assert!(!allowed(&env, &user, Action::Read, &other_repo).await);
}

#[tokio::test]
async fn selector_privilege_and_plain_privilege_combine() {
    let env = Env::new();
    let selector = env.selector("docs", "path.startsWith(\"/docs/\")").await;
    let narrow = env
        .privilege("docs-read", selector_grant("r", selector), &[Action::Read])
        .await;
    let wide = env
        .privilege("r-browse", view("r", None), &[Action::Browse])
        .await;
    let role = env.role("mixed", &[narrow, wide], &[]).await;
    let user = principal(&[(role, "mixed")]);
    assert!(
        allowed(
            &env,
            &user,
            Action::Browse,
            &resource("r", RepoFormat::Raw, "/anything")
        )
        .await
    );
    assert!(
        allowed(
            &env,
            &user,
            Action::Read,
            &resource("r", RepoFormat::Raw, "/docs/a")
        )
        .await
    );
    assert!(
        !allowed(
            &env,
            &user,
            Action::Read,
            &resource("r", RepoFormat::Raw, "/other/a")
        )
        .await
    );
}
