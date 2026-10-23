//! The delegation and project questions of Core revision 2: `holds_whole_repository` (the
//! test for handing an action to a robot token) and `visible_projects` (the filter for the
//! project list). The administrator, application area and project permission questions are in
//! `rbac_management.rs`; the rules are in `docs/rbac.md`.
//!
//! Both methods fail closed: a content selector is never the whole repository, groups never
//! travel without their provider, and a restricted token, a robot or a lookalike
//! administrator never widens anything.

// Tests may unwrap and expect: a panic is the failure signal there.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use common::{
    Env, builtin_principal, external, principal, repo, robot, selector_grant, view, with_scopes,
};
use rustryx_core::error::AuthError;
use rustryx_core::identity::{
    Action, AuthMethod, IdpProvider, Principal, PrivilegeGrant, ROLE_NX_ADMIN, ROLE_NX_ANONYMOUS,
    ROLE_NX_DEVELOPER, RoleRef, UserSource,
};
use rustryx_core::ids::RoleId;
use rustryx_core::model::RepoFormat;
use rustryx_core::ports::auth::{Decision, ProjectVisibilityFilter, RbacChecker};

const ACTIONS: [Action; 4] = [Action::Browse, Action::Read, Action::Write, Action::Delete];

fn role_ref(id: RoleId, name: &str) -> RoleRef {
    RoleRef {
        id,
        name: name.to_owned(),
    }
}

// ---- holds_whole_repository ----

#[tokio::test]
async fn whole_repository_rights_follow_plain_repository_privileges() {
    let env = Env::new();
    let dev = builtin_principal(ROLE_NX_DEVELOPER);
    let admin = builtin_principal(ROLE_NX_ADMIN);
    let target = repo("releases", RepoFormat::Maven2);
    let holds = |p: &Principal, action| {
        let (env, target) = (&env, &target);
        let p = p.clone();
        async move {
            env.rbac
                .holds_whole_repository(&p, target, action)
                .await
                .unwrap()
        }
    };
    for action in ACTIONS {
        assert!(holds(&admin, action).await, "admin {action:?}");
    }
    for action in [Action::Browse, Action::Read, Action::Write] {
        assert!(holds(&dev, action).await, "developer {action:?}");
    }
    assert!(!holds(&dev, Action::Delete).await);
    assert!(!holds(&principal(&[]), Action::Read).await);
}

#[tokio::test]
async fn whole_repository_respects_the_name_and_the_format_of_the_grant() {
    let env = Env::new();
    let only_a = env
        .simple_role("only-a", view("a", None), &[Action::Read])
        .await;
    let maven_only = env
        .simple_role(
            "maven-only",
            view("*", Some(RepoFormat::Maven2)),
            &[Action::Read],
        )
        .await;
    let user = principal(&[(only_a, "only-a"), (maven_only, "maven-only")]);
    let holds = |name: &str, format| {
        let target = repo(name, format);
        let (env, user) = (&env, &user);
        async move {
            env.rbac
                .holds_whole_repository(user, &target, Action::Read)
                .await
                .unwrap()
        }
    };
    assert!(holds("a", RepoFormat::Raw).await, "by name");
    assert!(holds("b", RepoFormat::Maven2).await, "by format");
    assert!(!holds("b", RepoFormat::Raw).await);
    assert!(
        !holds("a-more", RepoFormat::Npm).await,
        "no prefix matching"
    );
}

#[tokio::test]
async fn an_admin_privilege_holds_every_repository() {
    let env = Env::new();
    let role = env
        .simple_role("god-privilege", PrivilegeGrant::Admin, &[Action::Write])
        .await;
    let user = principal(&[(role, "god-privilege")]);
    let target = repo("any", RepoFormat::Pub);
    assert!(
        env.rbac
            .holds_whole_repository(&user, &target, Action::Write)
            .await
            .unwrap()
    );
    assert!(
        !env.rbac
            .holds_whole_repository(&user, &target, Action::Read)
            .await
            .unwrap(),
        "only the listed actions"
    );
}

#[tokio::test]
async fn must_not_delegate_a_selector_only_privilege() {
    // A robot would carry the action without the selector, i.e. on the whole repository.
    let env = Env::new();
    let selector = env.selector("everything", "true").await;
    let role = env
        .simple_role("selector-only", selector_grant("r", selector), &ACTIONS)
        .await;
    let user = principal(&[(role, "selector-only")]);
    let target = repo("r", RepoFormat::Raw);
    for action in ACTIONS {
        assert_eq!(
            env.rbac
                .check(&user, action, &common::resource("r", RepoFormat::Raw, "/x"))
                .await
                .unwrap(),
            Decision::Allow,
            "the selector does allow content ({action:?})"
        );
        assert!(
            !env.rbac
                .holds_whole_repository(&user, &target, action)
                .await
                .unwrap(),
            "{action:?}"
        );
    }
}

#[tokio::test]
async fn must_not_hold_a_whole_repository_beyond_the_token_scopes() {
    let env = Env::new();
    let scoped_dev = with_scopes(
        builtin_principal(ROLE_NX_DEVELOPER),
        &["repo:a:read", "repo:b:*"],
    );
    let scoped_admin = with_scopes(builtin_principal(ROLE_NX_ADMIN), &["repo:a:read"]);
    let a = repo("a", RepoFormat::Raw);
    let b = repo("b", RepoFormat::Raw);
    let c = repo("c", RepoFormat::Raw);
    let holds = |p: &Principal, target: &rustryx_core::model::Repository, action| {
        let env = &env;
        let p = p.clone();
        let target = target.clone();
        async move {
            env.rbac
                .holds_whole_repository(&p, &target, action)
                .await
                .unwrap()
        }
    };
    assert!(holds(&scoped_dev, &a, Action::Read).await);
    assert!(!holds(&scoped_dev, &a, Action::Write).await);
    assert!(holds(&scoped_dev, &b, Action::Write).await);
    assert!(!holds(&scoped_dev, &c, Action::Read).await);
    assert!(holds(&scoped_admin, &a, Action::Read).await);
    assert!(!holds(&scoped_admin, &a, Action::Write).await);
    assert!(!holds(&scoped_admin, &b, Action::Read).await);
}

#[tokio::test]
async fn must_not_let_anonymous_or_robots_delegate_anything() {
    let env = Env::bare();
    let privilege = env
        .privilege("anon-everything", view("*", None), &ACTIONS)
        .await;
    let nx_anonymous = env.role(ROLE_NX_ANONYMOUS, &[privilege], &[]).await;
    let target = repo("r", RepoFormat::Raw);
    let mut forged = Principal::anonymous();
    forged.roles = vec![role_ref(nx_anonymous, ROLE_NX_ANONYMOUS)];
    let robots = [
        robot(
            AuthMethod::RobotToken,
            &[role_ref(nx_anonymous, ROLE_NX_ANONYMOUS)],
            &["repo:r:*"],
        ),
        robot(AuthMethod::ScanToken, &[], &["repo:r:read"]),
    ];
    for subject in [&Principal::anonymous(), &forged, &robots[0], &robots[1]] {
        for action in ACTIONS {
            assert!(
                !env.rbac
                    .holds_whole_repository(subject, &target, action)
                    .await
                    .unwrap()
            );
        }
    }
    assert_eq!(env.store.privilege_reads() + env.store.role_name_reads(), 0);
}

#[tokio::test]
async fn must_not_hold_a_whole_repository_by_a_lookalike_admin_role() {
    let env = Env::new();
    let lookalike = principal(&[(RoleId::generate(), ROLE_NX_ADMIN)]);
    let target = repo("r", RepoFormat::Raw);
    for action in ACTIONS {
        assert!(
            !env.rbac
                .holds_whole_repository(&lookalike, &target, action)
                .await
                .unwrap()
        );
    }
}

#[tokio::test]
async fn must_not_report_a_whole_repository_on_store_error() {
    let env = Env::new();
    let dev = builtin_principal(ROLE_NX_DEVELOPER);
    env.store.inner.set_unavailable(true);
    let result = env
        .rbac
        .holds_whole_repository(&dev, &repo("r", RepoFormat::Raw), Action::Read)
        .await;
    assert!(
        matches!(result, Err(AuthError::Backend { .. })),
        "{result:?}"
    );
}

// ---- visible_projects ----

#[tokio::test]
async fn an_administrator_sees_all_projects() {
    let env = Env::new();
    assert_eq!(
        env.rbac
            .visible_projects(&builtin_principal(ROLE_NX_ADMIN))
            .await
            .unwrap(),
        ProjectVisibilityFilter::All
    );
}

#[tokio::test]
async fn a_user_sees_public_projects_plus_the_ones_where_the_account_has_a_row() {
    let env = Env::new();
    let dev = builtin_principal(ROLE_NX_DEVELOPER);
    let filter = env.rbac.visible_projects(&dev).await.unwrap();
    assert_eq!(
        filter,
        ProjectVisibilityFilter::Member {
            user_id: dev.user_id,
            provider: None,
            groups: Vec::new()
        }
    );
}

#[tokio::test]
async fn external_groups_travel_unmodified_with_their_provider() {
    let env = Env::new();
    let groups = [
        " /Teams/Payments",
        "/teams/payments ",
        "/Teams/Payments/Admins",
    ];
    let oidc = external(UserSource::Oidc, &[], &groups);
    assert_eq!(
        env.rbac.visible_projects(&oidc).await.unwrap(),
        ProjectVisibilityFilter::Member {
            user_id: oidc.user_id,
            provider: Some(IdpProvider::Oidc),
            groups: groups.iter().map(|g| (*g).to_owned()).collect(),
        }
    );
    let dns = ["CN=Payments,OU=Groups,DC=example,DC=test"];
    let ldap = external(UserSource::Ldap, &[], &dns);
    assert_eq!(
        env.rbac.visible_projects(&ldap).await.unwrap(),
        ProjectVisibilityFilter::Member {
            user_id: ldap.user_id,
            provider: Some(IdpProvider::Ldap),
            groups: dns.iter().map(|g| (*g).to_owned()).collect(),
        }
    );
}

#[tokio::test]
async fn an_external_token_without_groups_keeps_its_provider_and_no_groups() {
    // A token of an OIDC user carries no groups: direct memberships only.
    let env = Env::new();
    let mut token = external(UserSource::Oidc, &[], &[]);
    token.auth_method = AuthMethod::ApiToken;
    token.token_scopes = Some(vec!["*".into()]);
    assert_eq!(
        env.rbac.visible_projects(&token).await.unwrap(),
        ProjectVisibilityFilter::Member {
            user_id: token.user_id,
            provider: Some(IdpProvider::Oidc),
            groups: Vec::new()
        }
    );
}

#[tokio::test]
async fn must_not_pass_groups_without_the_provider_they_came_from() {
    // A local account has no external groups. If a bug left some on the principal they must
    // not reach the SQL filter, which matches `(provider, group)` pairs.
    let env = Env::new();
    let mut local = builtin_principal(ROLE_NX_DEVELOPER);
    local.external_groups = vec!["/Teams/Payments".into()];
    assert_eq!(
        env.rbac.visible_projects(&local).await.unwrap(),
        ProjectVisibilityFilter::Member {
            user_id: local.user_id,
            provider: None,
            groups: Vec::new()
        }
    );
}

#[tokio::test]
async fn must_not_show_every_project_to_a_lookalike_or_restricted_administrator() {
    let env = Env::new();
    let lookalike = principal(&[(RoleId::generate(), ROLE_NX_ADMIN)]);
    assert!(matches!(
        env.rbac.visible_projects(&lookalike).await.unwrap(),
        ProjectVisibilityFilter::Member { .. }
    ));
    for scopes in [&["repo:*:*"][..], &["repo:a:read"], &["garbage"]] {
        let token = with_scopes(builtin_principal(ROLE_NX_ADMIN), scopes);
        assert_eq!(
            env.rbac.visible_projects(&token).await.unwrap(),
            ProjectVisibilityFilter::PublicOnly,
            "{scopes:?}"
        );
    }
    let user_token = with_scopes(builtin_principal(ROLE_NX_DEVELOPER), &["repo:a:read"]);
    assert_eq!(
        env.rbac.visible_projects(&user_token).await.unwrap(),
        ProjectVisibilityFilter::PublicOnly,
        "a restricted token holds no project permission"
    );
}

#[tokio::test]
async fn must_not_show_private_projects_to_robots() {
    let env = Env::new();
    for method in [AuthMethod::RobotToken, AuthMethod::ScanToken] {
        let robot = robot(method, &builtin_principal(ROLE_NX_ADMIN).roles, &["*"]);
        assert_eq!(
            env.rbac.visible_projects(&robot).await.unwrap(),
            ProjectVisibilityFilter::PublicOnly,
            "{method:?}"
        );
    }
}

#[tokio::test]
async fn anonymous_sees_public_projects_only_while_anonymous_access_is_on() {
    let env = Env::new();
    assert_eq!(
        env.rbac
            .visible_projects(&Principal::anonymous())
            .await
            .unwrap(),
        ProjectVisibilityFilter::PublicOnly
    );
    env.rbac.anonymous_switch().set(false);
    let error = env
        .rbac
        .visible_projects(&Principal::anonymous())
        .await
        .unwrap_err();
    assert!(matches!(error, AuthError::AccessDenied(_)), "{error:?}");
    assert!(error.to_string().contains("anonymous"));
}

#[tokio::test]
async fn must_not_widen_the_project_filter_for_anonymous_with_forged_roles() {
    let env = Env::new();
    let mut forged = Principal::anonymous();
    forged.roles = builtin_principal(ROLE_NX_ADMIN).roles;
    assert_eq!(
        env.rbac.visible_projects(&forged).await.unwrap(),
        ProjectVisibilityFilter::PublicOnly
    );
}

#[tokio::test]
async fn must_not_return_a_project_filter_on_store_error() {
    let env = Env::new();
    env.store.inner.set_unavailable(true);
    for subject in [
        builtin_principal(ROLE_NX_ADMIN),
        builtin_principal(ROLE_NX_DEVELOPER),
    ] {
        let result = env.rbac.visible_projects(&subject).await;
        assert!(
            matches!(result, Err(AuthError::Backend { .. })),
            "{result:?}"
        );
    }
}
