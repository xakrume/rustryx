//! Authorization decisions for anonymous callers, API token scopes and
//! repository visibility (the 404 versus 403 decision).
//!
//! The negative requirements ("must not ...") have their own file,
//! `rbac_must_not.rs`.

// Tests may unwrap and expect: a panic is the failure signal there.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use common::{Env, builtin_principal, open_resource, principal, resource, selector_grant, view};
use rustryx_core::error::AuthError;
use rustryx_core::identity::{
    Action, Principal, ROLE_NX_ADMIN, ROLE_NX_ANONYMOUS, ROLE_NX_DEVELOPER,
};
use rustryx_core::model::RepoFormat;
use rustryx_core::ports::auth::{Decision, RbacChecker, ResourceRef};

async fn allowed(env: &Env, p: &Principal, action: Action, r: &ResourceRef) -> bool {
    env.rbac.check(p, action, r).await.unwrap() == Decision::Allow
}

#[tokio::test]
async fn anonymous_reads_through_nx_anonymous_and_never_writes() {
    let env = Env::new();
    let anonymous = Principal::anonymous();
    let res = open_resource("public", RepoFormat::Raw, "/x");
    assert!(allowed(&env, &anonymous, Action::Browse, &res).await);
    assert!(allowed(&env, &anonymous, Action::Read, &res).await);
    assert!(!allowed(&env, &anonymous, Action::Write, &res).await);
    assert!(!allowed(&env, &anonymous, Action::Delete, &res).await);
    assert!(
        env.rbac
            .can_see_repository(&anonymous, "public", RepoFormat::Raw)
            .await
            .unwrap()
    );
    assert!(!env.rbac.is_admin(&anonymous).await.unwrap());
}

#[tokio::test]
async fn anonymous_gets_nothing_when_the_store_has_no_nx_anonymous_role() {
    let env = Env::bare();
    let res = open_resource("r", RepoFormat::Raw, "/x");
    for action in [Action::Browse, Action::Read, Action::Write, Action::Delete] {
        assert_eq!(
            env.rbac
                .check(&Principal::anonymous(), action, &res)
                .await
                .unwrap(),
            Decision::Deny
        );
    }
    assert!(
        !env.rbac
            .can_see_repository(&Principal::anonymous(), "r", RepoFormat::Raw)
            .await
            .unwrap()
    );
}

#[tokio::test]
async fn anonymous_access_can_be_narrowed_with_a_custom_nx_anonymous_role() {
    // A store that does not seed the built-ins lets the test define nx-anonymous.
    let env = Env::bare();
    let privilege = env
        .privilege("public-read", view("public", None), &[Action::Read])
        .await;
    env.role(ROLE_NX_ANONYMOUS, &[privilege], &[]).await;
    let anonymous = Principal::anonymous();
    assert!(
        allowed(
            &env,
            &anonymous,
            Action::Read,
            &open_resource("public", RepoFormat::Raw, "/x")
        )
        .await
    );
    assert!(
        !allowed(
            &env,
            &anonymous,
            Action::Read,
            &open_resource("private", RepoFormat::Raw, "/x")
        )
        .await
    );
    assert!(
        !allowed(
            &env,
            &anonymous,
            Action::Browse,
            &open_resource("public", RepoFormat::Raw, "/x")
        )
        .await
    );
}

#[tokio::test]
async fn token_scopes_narrow_the_owners_rights() {
    let env = Env::new();
    let role = env
        .simple_role(
            "owner",
            view("*", None),
            &[Action::Browse, Action::Read, Action::Write],
        )
        .await;
    let mut token = principal(&[(role, "owner")]);
    token.token_scopes = Some(vec!["repo:a:read".into(), "repo:b:*".into()]);
    assert!(
        allowed(
            &env,
            &token,
            Action::Read,
            &resource("a", RepoFormat::Raw, "/x")
        )
        .await
    );
    assert!(
        !allowed(
            &env,
            &token,
            Action::Write,
            &resource("a", RepoFormat::Raw, "/x")
        )
        .await
    );
    assert!(
        allowed(
            &env,
            &token,
            Action::Write,
            &resource("b", RepoFormat::Raw, "/x")
        )
        .await
    );
    assert!(
        !allowed(
            &env,
            &token,
            Action::Delete,
            &resource("b", RepoFormat::Raw, "/x")
        )
        .await,
        "owner has no delete"
    );
    assert!(
        !allowed(
            &env,
            &token,
            Action::Read,
            &resource("c", RepoFormat::Raw, "/x")
        )
        .await
    );
}

#[tokio::test]
async fn unrestricted_scope_forms_leave_the_owners_rights_alone() {
    let env = Env::new();
    let role = env
        .simple_role("owner", view("*", None), &[Action::Read])
        .await;
    for scopes in [None, Some(vec![]), Some(vec!["*".to_owned()])] {
        let mut token = principal(&[(role, "owner")]);
        token.token_scopes = scopes;
        assert!(
            allowed(
                &env,
                &token,
                Action::Read,
                &resource("z", RepoFormat::Raw, "/x")
            )
            .await
        );
    }
}

#[tokio::test]
async fn token_scopes_narrow_an_administrator_too() {
    let env = Env::new();
    let mut token = builtin_principal(ROLE_NX_ADMIN);
    token.token_scopes = Some(vec!["repo:ci:write".into()]);
    assert!(
        allowed(
            &env,
            &token,
            Action::Write,
            &resource("ci", RepoFormat::Raw, "/x")
        )
        .await
    );
    assert!(
        !allowed(
            &env,
            &token,
            Action::Read,
            &resource("ci", RepoFormat::Raw, "/x")
        )
        .await
    );
    assert!(
        !allowed(
            &env,
            &token,
            Action::Write,
            &resource("other", RepoFormat::Raw, "/x")
        )
        .await
    );
    assert!(
        !env.rbac.is_admin(&token).await.unwrap(),
        "a restricted token is not an administrator"
    );
    token.token_scopes = Some(vec!["*".into()]);
    assert!(env.rbac.is_admin(&token).await.unwrap());
}

#[tokio::test]
async fn visibility_follows_privileges_formats_and_scopes() {
    let env = Env::new();
    let write_only = env
        .simple_role("drop-box", view("drop", None), &[Action::Write])
        .await;
    let maven = env
        .simple_role(
            "maven",
            view("*", Some(RepoFormat::Maven2)),
            &[Action::Browse],
        )
        .await;
    let drop_user = principal(&[(write_only, "drop-box")]);
    let maven_user = principal(&[(maven, "maven")]);
    assert!(
        env.rbac
            .can_see_repository(&drop_user, "drop", RepoFormat::Raw)
            .await
            .unwrap()
    );
    assert!(
        !env.rbac
            .can_see_repository(&drop_user, "other", RepoFormat::Raw)
            .await
            .unwrap()
    );
    assert!(
        env.rbac
            .can_see_repository(&maven_user, "m", RepoFormat::Maven2)
            .await
            .unwrap()
    );
    assert!(
        !env.rbac
            .can_see_repository(&maven_user, "n", RepoFormat::Npm)
            .await
            .unwrap()
    );
    let mut scoped = maven_user.clone();
    scoped.token_scopes = Some(vec!["repo:other:browse".into()]);
    assert!(
        !env.rbac
            .can_see_repository(&scoped, "m", RepoFormat::Maven2)
            .await
            .unwrap()
    );
    scoped.token_scopes = Some(vec!["repo:m:browse".into()]);
    assert!(
        env.rbac
            .can_see_repository(&scoped, "m", RepoFormat::Maven2)
            .await
            .unwrap()
    );
    let admin = builtin_principal(ROLE_NX_ADMIN);
    assert!(
        env.rbac
            .can_see_repository(&admin, "anything", RepoFormat::Pub)
            .await
            .unwrap()
    );
    let mut scoped_admin = admin;
    scoped_admin.token_scopes = Some(vec!["repo:x:read".into()]);
    assert!(
        !env.rbac
            .can_see_repository(&scoped_admin, "y", RepoFormat::Pub)
            .await
            .unwrap()
    );
}

#[tokio::test]
async fn selector_privileges_make_a_repository_visible_only_while_the_selector_works() {
    let env = Env::new();
    let good = env.selector("good", "path.startsWith(\"/a/\")").await;
    let broken = env.selector("broken", "path.startsWith(").await;
    let good_role = env
        .simple_role("good-role", selector_grant("g", good), &[Action::Read])
        .await;
    let broken_role = env
        .simple_role("broken-role", selector_grant("b", broken), &[Action::Read])
        .await;
    let dangling = rustryx_core::ids::ContentSelectorId::generate();
    let dangling_role = env
        .simple_role(
            "dangling-role",
            selector_grant("d", dangling),
            &[Action::Read],
        )
        .await;
    let user = principal(&[
        (good_role, "good-role"),
        (broken_role, "broken-role"),
        (dangling_role, "dangling-role"),
    ]);
    assert!(
        env.rbac
            .can_see_repository(&user, "g", RepoFormat::Raw)
            .await
            .unwrap()
    );
    assert!(
        !env.rbac
            .can_see_repository(&user, "b", RepoFormat::Raw)
            .await
            .unwrap()
    );
    assert!(
        !env.rbac
            .can_see_repository(&user, "d", RepoFormat::Raw)
            .await
            .unwrap()
    );
}

#[tokio::test]
async fn anonymous_visibility_counts_only_browse_and_read() {
    let env = Env::bare();
    let write_only = env
        .privilege("anon-write", view("w", None), &[Action::Write])
        .await;
    let read = env
        .privilege("anon-read", view("r", None), &[Action::Read])
        .await;
    env.role(ROLE_NX_ANONYMOUS, &[write_only, read], &[]).await;
    let anonymous = Principal::anonymous();
    assert!(
        env.rbac
            .can_see_repository(&anonymous, "r", RepoFormat::Raw)
            .await
            .unwrap()
    );
    assert!(
        !env.rbac
            .can_see_repository(&anonymous, "w", RepoFormat::Raw)
            .await
            .unwrap()
    );
}

#[tokio::test]
async fn the_debug_form_names_the_type_and_shows_no_data() {
    let env = Env::new();
    let admin = builtin_principal(ROLE_NX_ADMIN);
    assert!(env.rbac.is_admin(&admin).await.unwrap());
    let shown = format!("{:?}", env.rbac);
    assert!(shown.starts_with("Rbac"), "{shown}");
    assert!(!shown.contains("privilege"), "{shown}");
}

#[tokio::test]
async fn validate_selector_reports_compile_errors() {
    let env = Env::new();
    assert!(env.rbac.validate_selector("format == \"maven2\"").is_ok());
    assert!(env.rbac.validate_selector("format ==").is_err());
}

#[tokio::test]
async fn a_privilege_naming_an_unknown_selector_matches_nothing() {
    let env = Env::new();
    let ghost = env
        .simple_role(
            "ghost-role",
            selector_grant("*", rustryx_core::ids::ContentSelectorId::generate()),
            &[Action::Read],
        )
        .await;
    let user = principal(&[(ghost, "ghost-role")]);
    assert!(
        !allowed(
            &env,
            &user,
            Action::Read,
            &resource("r", RepoFormat::Raw, "/x")
        )
        .await
    );
}

#[tokio::test]
async fn store_errors_are_reported_as_backend_errors() {
    let env = Env::new();
    let dev = builtin_principal(ROLE_NX_DEVELOPER);
    env.store.inner.set_unavailable(true);
    let err = env
        .rbac
        .check(&dev, Action::Read, &resource("r", RepoFormat::Raw, "/x"))
        .await
        .unwrap_err();
    assert!(matches!(err, AuthError::Backend { .. }), "{err:?}");
    assert!(std::error::Error::source(&err).is_some());
}
