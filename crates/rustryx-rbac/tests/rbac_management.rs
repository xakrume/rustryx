//! The questions of Core revision 2 that are not about one piece of content: `is_admin`,
//! `check_app`, `check_project`, `holds_whole_repository` and `visible_projects`, plus how
//! robot and scan tokens are treated. The rules are in `docs/rbac.md`.
//!
//! Every provided method of `RbacChecker` fails closed, and each section below has its
//! `must_not_*` tests: forged names, restricted tokens, robots, anonymous callers, a store
//! that is down.

// Tests may unwrap and expect: a panic is the failure signal there.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::sync::Arc;

use common::{Env, builtin_principal, builtin_ref, principal, repo, robot, view, with_scopes};
use rustryx_core::error::AuthError;
use rustryx_core::identity::{
    Action, AppArea, AuthMethod, Principal, PrivilegeGrant, ROLE_NX_ADMIN, ROLE_NX_ANONYMOUS,
    ROLE_NX_DEVELOPER, RoleRef,
};
use rustryx_core::ids::{ProjectId, RoleId};
use rustryx_core::model::RepoFormat;
use rustryx_core::ports::auth::{Decision, ProjectVisibilityFilter, RbacChecker};
use rustryx_core::project::ProjectPermission;

const ACTIONS: [Action; 4] = [Action::Browse, Action::Read, Action::Write, Action::Delete];
const APP_ACTIONS: [Action; 3] = [Action::Read, Action::Write, Action::Delete];

fn application(area: AppArea) -> PrivilegeGrant {
    PrivilegeGrant::Application { area }
}

fn role_ref(id: RoleId, name: &str) -> RoleRef {
    RoleRef {
        id,
        name: name.to_owned(),
    }
}

// ---- is_admin ----

#[tokio::test]
async fn is_admin_through_the_trait_object_matches_the_role_id_and_the_token() {
    let env = Env::new();
    let checker: Arc<dyn RbacChecker> = Arc::new(env.rbac.clone());
    assert!(
        checker
            .is_admin(&builtin_principal(ROLE_NX_ADMIN))
            .await
            .unwrap()
    );
    assert!(
        !checker
            .is_admin(&builtin_principal(ROLE_NX_DEVELOPER))
            .await
            .unwrap()
    );
    assert!(!checker.is_admin(&Principal::anonymous()).await.unwrap());
    for scopes in [&["repo:*:*"][..], &["repo:a:read"], &["garbage"]] {
        let token = with_scopes(builtin_principal(ROLE_NX_ADMIN), scopes);
        assert!(!checker.is_admin(&token).await.unwrap(), "{scopes:?}");
    }
    for scopes in [&["*"][..], &[]] {
        let token = with_scopes(builtin_principal(ROLE_NX_ADMIN), scopes);
        assert!(checker.is_admin(&token).await.unwrap(), "{scopes:?}");
    }
}

#[tokio::test]
async fn must_not_let_a_robot_or_scan_token_be_an_administrator() {
    let env = Env::new();
    let admin_roles = builtin_principal(ROLE_NX_ADMIN).roles;
    for method in [AuthMethod::RobotToken, AuthMethod::ScanToken] {
        // A robot has no roles; one that carries the real nx-admin reference is a bug of an
        // authenticator, and the authorizer must not turn it into an administrator.
        for scopes in [&["*"][..], &["repo:*:*"], &[]] {
            let forged = robot(method, &admin_roles, scopes);
            assert!(
                !env.rbac.is_admin(&forged).await.unwrap(),
                "{method:?} {scopes:?}"
            );
            for action in ACTIONS {
                assert_eq!(
                    env.rbac
                        .check(
                            &forged,
                            action,
                            &common::open_resource("r", RepoFormat::Raw, "/x")
                        )
                        .await
                        .unwrap(),
                    Decision::Deny,
                    "{method:?} {action:?}"
                );
            }
            assert!(
                !env.rbac
                    .can_see_repository(&forged, "r", RepoFormat::Raw)
                    .await
                    .unwrap()
            );
        }
    }
}

#[tokio::test]
async fn must_not_give_a_robot_the_privileges_of_the_roles_it_carries() {
    // The same applies to ordinary roles: a robot is not evidence of any role.
    let env = Env::new();
    let developer = builtin_ref(ROLE_NX_DEVELOPER);
    let robot = robot(AuthMethod::RobotToken, &[developer], &["*"]);
    for action in ACTIONS {
        assert_eq!(
            env.rbac
                .check(
                    &robot,
                    action,
                    &common::resource("r", RepoFormat::Raw, "/x")
                )
                .await
                .unwrap(),
            Decision::Deny,
            "{action:?}"
        );
    }
    assert_eq!(
        env.store.privilege_reads(),
        0,
        "the roles were never looked up"
    );
}

// ---- check_app ----

#[tokio::test]
async fn an_administrator_passes_every_area_and_action() {
    let env = Env::new();
    let admin = builtin_principal(ROLE_NX_ADMIN);
    for area in AppArea::ALL {
        for action in APP_ACTIONS {
            assert_eq!(
                env.rbac.check_app(&admin, *area, action).await.unwrap(),
                Decision::Allow,
                "{area:?} {action:?}"
            );
        }
    }
}

#[tokio::test]
async fn an_application_privilege_grants_exactly_its_area_and_actions() {
    let env = Env::new();
    let role = env
        .simple_role(
            "settings-reader",
            application(AppArea::Settings),
            &[Action::Read],
        )
        .await;
    let user = principal(&[(role, "settings-reader")]);
    for area in AppArea::ALL {
        for action in APP_ACTIONS {
            let expected = if *area == AppArea::Settings && action == Action::Read {
                Decision::Allow
            } else {
                Decision::Deny
            };
            assert_eq!(
                env.rbac.check_app(&user, *area, action).await.unwrap(),
                expected,
                "{area:?} {action:?}"
            );
        }
    }
}

#[tokio::test]
async fn several_application_privileges_combine() {
    let env = Env::new();
    let settings = env
        .privilege("settings", application(AppArea::Settings), &[Action::Read])
        .await;
    let tokens = env
        .privilege(
            "tokens",
            application(AppArea::Tokens),
            &[Action::Read, Action::Write],
        )
        .await;
    let role = env.role("ops", &[settings, tokens], &[]).await;
    let user = principal(&[(role, "ops")]);
    let allowed = |area, action| {
        let (env, user) = (&env, &user);
        async move { env.rbac.check_app(user, area, action).await.unwrap() == Decision::Allow }
    };
    assert!(allowed(AppArea::Settings, Action::Read).await);
    assert!(allowed(AppArea::Tokens, Action::Write).await);
    assert!(!allowed(AppArea::Tokens, Action::Delete).await);
    assert!(!allowed(AppArea::Users, Action::Read).await);
}

#[tokio::test]
async fn an_application_privilege_never_grants_content() {
    let env = Env::new();
    let role = env
        .simple_role("tokens-admin", application(AppArea::Tokens), &ACTIONS)
        .await;
    let user = principal(&[(role, "tokens-admin")]);
    for action in ACTIONS {
        assert_eq!(
            env.rbac
                .check(&user, action, &common::resource("r", RepoFormat::Raw, "/x"))
                .await
                .unwrap(),
            Decision::Deny,
            "{action:?}"
        );
    }
    assert!(
        !env.rbac
            .can_see_repository(&user, "r", RepoFormat::Raw)
            .await
            .unwrap()
    );
    assert!(
        !env.rbac
            .holds_whole_repository(&user, &repo("r", RepoFormat::Raw), Action::Read)
            .await
            .unwrap()
    );
}

#[tokio::test]
async fn must_not_pass_check_app_with_a_content_privilege() {
    let env = Env::new();
    let dev = builtin_principal(ROLE_NX_DEVELOPER);
    for area in AppArea::ALL {
        for action in APP_ACTIONS {
            assert_eq!(
                env.rbac.check_app(&dev, *area, action).await.unwrap(),
                Decision::Deny,
                "{area:?} {action:?}"
            );
        }
    }
}

#[tokio::test]
async fn must_not_pass_check_app_with_the_admin_privilege_of_another_role() {
    // The `Admin` grant means every action on all content (and `check` honours that), but
    // administration is gated by the role id of nx-admin or an explicit area grant, so no
    // second route to administrator rights exists.
    let env = Env::new();
    let role = env
        .simple_role("god-privilege", PrivilegeGrant::Admin, &ACTIONS)
        .await;
    let user = principal(&[(role, "god-privilege")]);
    assert_eq!(
        env.rbac
            .check(
                &user,
                Action::Delete,
                &common::resource("r", RepoFormat::Raw, "/x")
            )
            .await
            .unwrap(),
        Decision::Allow,
        "content is everything the grant promises"
    );
    for area in AppArea::ALL {
        for action in APP_ACTIONS {
            assert_eq!(
                env.rbac.check_app(&user, *area, action).await.unwrap(),
                Decision::Deny,
                "{area:?} {action:?}"
            );
        }
    }
    assert!(!env.rbac.is_admin(&user).await.unwrap());
}

#[tokio::test]
async fn must_not_match_an_application_privilege_by_role_name() {
    let env = Env::new();
    let real = env
        .simple_role("area-admin", application(AppArea::Users), &[Action::Write])
        .await;
    // A reference with the right name and an unknown id gets nothing; the real role under any
    // name gets the privilege.
    let forged = principal(&[(RoleId::generate(), "area-admin")]);
    assert_eq!(
        env.rbac
            .check_app(&forged, AppArea::Users, Action::Write)
            .await
            .unwrap(),
        Decision::Deny
    );
    let renamed = principal(&[(real, "something-else")]);
    assert_eq!(
        env.rbac
            .check_app(&renamed, AppArea::Users, Action::Write)
            .await
            .unwrap(),
        Decision::Allow
    );
    // And a role named like nx-admin with another id is not an administrator in any area.
    let lookalike = principal(&[(RoleId::generate(), ROLE_NX_ADMIN)]);
    for area in AppArea::ALL {
        assert_eq!(
            env.rbac
                .check_app(&lookalike, *area, Action::Read)
                .await
                .unwrap(),
            Decision::Deny
        );
    }
}

#[tokio::test]
async fn must_not_give_a_restricted_token_application_privileges() {
    // The scope grammar describes content. Reading it as a statement about administration
    // would widen the token, so a restricted token holds no application privilege, an
    // administrator's included.
    let env = Env::new();
    let role = env
        .simple_role(
            "settings-admin",
            application(AppArea::Settings),
            &[Action::Read, Action::Write],
        )
        .await;
    let owner = principal(&[(role, "settings-admin")]);
    for scopes in [&["repo:*:*"][..], &["repo:a:read"], &["garbage"]] {
        for subject in [
            with_scopes(owner.clone(), scopes),
            with_scopes(builtin_principal(ROLE_NX_ADMIN), scopes),
        ] {
            assert_eq!(
                env.rbac
                    .check_app(&subject, AppArea::Settings, Action::Read)
                    .await
                    .unwrap(),
                Decision::Deny,
                "{scopes:?}"
            );
        }
    }
    // The unrestricted forms leave the owner's rights alone.
    for scopes in [&["*"][..], &[]] {
        assert_eq!(
            env.rbac
                .check_app(
                    &with_scopes(owner.clone(), scopes),
                    AppArea::Settings,
                    Action::Write
                )
                .await
                .unwrap(),
            Decision::Allow,
            "{scopes:?}"
        );
    }
}

#[tokio::test]
async fn must_not_give_anonymous_or_robots_application_privileges() {
    // nx-anonymous is misconfigured with an application privilege, and a robot carries the
    // administrator reference: neither gets an area.
    let env = Env::bare();
    let privilege = env
        .privilege(
            "anon-settings",
            application(AppArea::Settings),
            &APP_ACTIONS,
        )
        .await;
    let nx_anonymous = env.role(ROLE_NX_ANONYMOUS, &[privilege], &[]).await;
    let anonymous = Principal::anonymous();
    let mut forged = Principal::anonymous();
    forged.roles = vec![role_ref(nx_anonymous, ROLE_NX_ANONYMOUS)];
    let carrying_the_role = robot(
        AuthMethod::RobotToken,
        &[role_ref(nx_anonymous, ROLE_NX_ANONYMOUS)],
        &["*"],
    );
    for subject in [&anonymous, &forged, &carrying_the_role] {
        for action in APP_ACTIONS {
            assert_eq!(
                env.rbac
                    .check_app(subject, AppArea::Settings, action)
                    .await
                    .unwrap(),
                Decision::Deny
            );
        }
    }
    assert_eq!(
        env.store.privilege_reads() + env.store.role_name_reads(),
        0,
        "refused before any store access"
    );
}

#[tokio::test]
async fn must_not_allow_check_app_on_store_error() {
    let env = Env::new();
    let role = env
        .simple_role("app", application(AppArea::Users), &[Action::Read])
        .await;
    let owner = principal(&[(role, "app")]);
    let admin = builtin_principal(ROLE_NX_ADMIN);
    env.store.inner.set_unavailable(true);
    for subject in [&owner, &admin] {
        let result = env
            .rbac
            .check_app(subject, AppArea::Users, Action::Read)
            .await;
        assert!(
            matches!(result, Err(AuthError::Backend { .. })),
            "{result:?}"
        );
    }
}

// ---- check_project ----

#[tokio::test]
async fn an_administrator_holds_every_project_permission_everywhere() {
    let env = Env::new();
    let admin = builtin_principal(ROLE_NX_ADMIN);
    for permission in ProjectPermission::ALL {
        assert_eq!(
            env.rbac
                .check_project(&admin, ProjectId::generate(), *permission)
                .await
                .unwrap(),
            Decision::Allow,
            "{permission:?}"
        );
    }
}

#[tokio::test]
async fn must_not_grant_a_project_permission_without_the_membership_rules() {
    // Until the membership caches of wave 2 stream B exist, only nx-admin passes. Everyone
    // else is refused, whatever content or application privileges they hold.
    let env = Env::new();
    let everything = env.privilege("everything", view("*", None), &ACTIONS).await;
    let app = env
        .privilege("app", application(AppArea::Settings), &APP_ACTIONS)
        .await;
    let god = env.privilege("god", PrivilegeGrant::Admin, &ACTIONS).await;
    let role = env.role("powerful", &[everything, app, god], &[]).await;
    let subjects = [
        builtin_principal(ROLE_NX_DEVELOPER),
        principal(&[(role, "powerful")]),
        principal(&[(RoleId::generate(), ROLE_NX_ADMIN)]),
        Principal::anonymous(),
        robot(
            AuthMethod::RobotToken,
            &builtin_principal(ROLE_NX_ADMIN).roles,
            &["*"],
        ),
        robot(AuthMethod::ScanToken, &[], &["*"]),
    ];
    for subject in &subjects {
        for permission in ProjectPermission::ALL {
            assert_eq!(
                env.rbac
                    .check_project(subject, ProjectId::generate(), *permission)
                    .await
                    .unwrap(),
                Decision::Deny,
                "{:?} {permission:?}",
                subject.auth_method
            );
        }
    }
}

#[tokio::test]
async fn must_not_give_a_restricted_token_project_permissions() {
    let env = Env::new();
    for scopes in [&["repo:*:*"][..], &["repo:a:read"], &["garbage"]] {
        let token = with_scopes(builtin_principal(ROLE_NX_ADMIN), scopes);
        assert_eq!(
            env.rbac
                .check_project(
                    &token,
                    ProjectId::generate(),
                    ProjectPermission::ViewProject
                )
                .await
                .unwrap(),
            Decision::Deny,
            "{scopes:?}"
        );
    }
    let unrestricted = with_scopes(builtin_principal(ROLE_NX_ADMIN), &["*"]);
    assert_eq!(
        env.rbac
            .check_project(
                &unrestricted,
                ProjectId::generate(),
                ProjectPermission::ManageSettings
            )
            .await
            .unwrap(),
        Decision::Allow
    );
}

#[tokio::test]
async fn must_not_allow_check_project_on_store_error() {
    let env = Env::new();
    let admin = builtin_principal(ROLE_NX_ADMIN);
    env.store.inner.set_unavailable(true);
    let result = env
        .rbac
        .check_project(
            &admin,
            ProjectId::generate(),
            ProjectPermission::ViewProject,
        )
        .await;
    assert!(
        matches!(result, Err(AuthError::Backend { .. })),
        "{result:?}"
    );
}

#[tokio::test]
async fn the_trait_defaults_are_not_used_by_this_authorizer() {
    // `Rbac` overrides every provided method: `is_admin` and `check_app` answer true for an
    // administrator, which the fail closed defaults never do.
    let env = Env::new();
    let checker: Arc<dyn RbacChecker> = Arc::new(env.rbac.clone());
    let admin = builtin_principal(ROLE_NX_ADMIN);
    assert!(checker.is_admin(&admin).await.unwrap());
    assert_eq!(
        checker
            .check_app(&admin, AppArea::System, Action::Write)
            .await
            .unwrap(),
        Decision::Allow
    );
    assert_eq!(
        checker.visible_projects(&admin).await.unwrap(),
        ProjectVisibilityFilter::All
    );
}
