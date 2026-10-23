//! The negative requirements of authorization (CONTRIBUTING rule 8): every
//! "never" and "must not" has a test here whose name starts with `must_not_`.

// Tests may unwrap and expect: a panic is the failure signal there.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::time::Duration;

use common::{
    Env, builtin_principal, open_repo, open_resource, principal, repo, resource,
    resource_with_coordinate, selector_grant, view,
};
use rustryx_core::error::AuthError;
use rustryx_core::identity::{
    Action, Principal, PrivilegeGrant, ROLE_NX_ADMIN, ROLE_NX_ANONYMOUS, ROLE_NX_DEVELOPER,
};
use rustryx_core::ids::RoleId;
use rustryx_core::model::{RepoFormat, Repository};
use rustryx_core::ports::RoleRepo;
use rustryx_core::ports::auth::{Decision, RbacChecker, ResourceRef};
use rustryx_rbac::RbacConfig;

const ACTIONS: [Action; 4] = [Action::Browse, Action::Read, Action::Write, Action::Delete];

async fn allowed(env: &Env, p: &Principal, action: Action, r: &ResourceRef) -> bool {
    env.rbac.check(p, action, r).await.unwrap() == Decision::Allow
}

fn names<'a>(visible: &[&'a Repository]) -> Vec<&'a str> {
    visible.iter().map(|m| m.name.as_str()).collect()
}

fn not_allowed<T: std::fmt::Debug>(result: &Result<Decision, T>) -> bool {
    !matches!(result, Ok(Decision::Allow))
}

#[tokio::test]
async fn must_not_allow_anonymous_write() {
    // The most permissive nx-anonymous a misconfiguration can produce: every
    // action on every repository, plus the Admin privilege.
    let env = Env::bare();
    let everything = env
        .privilege("anon-everything", view("*", None), &ACTIONS)
        .await;
    let admin = env
        .privilege("anon-admin", PrivilegeGrant::Admin, &ACTIONS)
        .await;
    env.role(ROLE_NX_ANONYMOUS, &[everything, admin], &[]).await;
    let res = open_resource("any", RepoFormat::Raw, "/x");
    let anonymous = Principal::anonymous();
    assert!(
        allowed(&env, &anonymous, Action::Read, &res).await,
        "read is what the role grants"
    );
    assert!(!allowed(&env, &anonymous, Action::Write, &res).await);
    assert!(!allowed(&env, &anonymous, Action::Delete, &res).await);

    // An anonymous principal that carries forged roles is still only nx-anonymous.
    let mut forged = Principal::anonymous();
    forged.roles = builtin_principal(ROLE_NX_ADMIN).roles;
    assert!(!allowed(&env, &forged, Action::Write, &res).await);
    assert!(!allowed(&env, &forged, Action::Delete, &res).await);
    assert!(!env.rbac.is_admin(&forged).await.unwrap());
}

#[tokio::test]
async fn must_not_allow_anonymous_write_even_when_the_store_is_down() {
    let env = Env::new();
    env.store.inner.set_unavailable(true);
    let res = open_resource("any", RepoFormat::Raw, "/x");
    for action in [Action::Write, Action::Delete] {
        // A definite Deny, decided before any store access: not an error.
        assert_eq!(
            env.rbac
                .check(&Principal::anonymous(), action, &res)
                .await
                .unwrap(),
            Decision::Deny
        );
    }
    assert_eq!(env.store.privilege_reads() + env.store.role_name_reads(), 0);
}

#[tokio::test]
async fn must_not_grant_anonymous_the_admin_privilege() {
    let env = Env::bare();
    let admin = env
        .privilege("anon-admin", PrivilegeGrant::Admin, &ACTIONS)
        .await;
    env.role(ROLE_NX_ANONYMOUS, &[admin], &[]).await;
    let anonymous = Principal::anonymous();
    for action in ACTIONS {
        assert!(
            !allowed(
                &env,
                &anonymous,
                action,
                &open_resource("r", RepoFormat::Raw, "/x")
            )
            .await
        );
    }
    assert!(
        !env.rbac
            .can_see_repository(&anonymous, "r", RepoFormat::Raw)
            .await
            .unwrap()
    );
}

#[tokio::test]
async fn must_not_fall_back_to_nx_anonymous_for_an_authenticated_user_without_roles() {
    // README 8.5: a user with no mapped group is refused, not treated as anonymous.
    let env = Env::new();
    let nobody = principal(&[]);
    let res = open_resource("public", RepoFormat::Raw, "/x");
    assert!(
        allowed(&env, &Principal::anonymous(), Action::Read, &res).await,
        "anonymous can read"
    );
    assert!(!allowed(&env, &nobody, Action::Read, &res).await);
    assert!(!allowed(&env, &nobody, Action::Browse, &res).await);
}

#[tokio::test]
async fn must_not_extend_rights_via_token_scope() {
    let env = Env::new();
    let reader = env
        .simple_role("reader-of-a", view("a", None), &[Action::Read])
        .await;
    let a = resource("a", RepoFormat::Raw, "/x");
    let b = resource("b", RepoFormat::Raw, "/x");

    for scopes in [
        vec!["repo:b:read"],
        vec!["repo:*:write"],
        vec!["repo:*:*"],
        vec!["repo:a:write", "repo:a:delete"],
        vec!["repo:b:*", "repo:a:browse"],
    ] {
        let mut token = principal(&[(reader, "reader-of-a")]);
        token.token_scopes = Some(scopes.iter().map(|s| (*s).to_owned()).collect());
        // What the owner cannot do stays denied whatever the scopes say.
        assert!(!allowed(&env, &token, Action::Read, &b).await, "{scopes:?}");
        assert!(
            !allowed(&env, &token, Action::Write, &a).await,
            "{scopes:?}"
        );
        assert!(
            !allowed(&env, &token, Action::Write, &b).await,
            "{scopes:?}"
        );
        assert!(
            !allowed(&env, &token, Action::Delete, &a).await,
            "{scopes:?}"
        );
        assert!(!env.rbac.is_admin(&token).await.unwrap(), "{scopes:?}");
    }

    // A token of a user without any privilege gains nothing from `*`.
    let mut nobody = principal(&[]);
    nobody.token_scopes = Some(vec!["*".into(), "repo:*:*".into()]);
    for action in ACTIONS {
        assert!(!allowed(&env, &nobody, action, &a).await);
    }
}

#[tokio::test]
async fn must_not_let_a_malformed_scope_widen_a_token() {
    let env = Env::new();
    let reader = env
        .simple_role("reader", view("a", None), &[Action::Read])
        .await;
    let res = resource("a", RepoFormat::Raw, "/x");
    for scopes in [
        vec!["garbage"],
        vec!["repo:a:READ"],
        vec!["repo:a"],
        vec!["repo::read"],
        vec!["**"],
        vec![" *"],
        vec![""],
    ] {
        let mut token = principal(&[(reader, "reader")]);
        token.token_scopes = Some(scopes.iter().map(|s| (*s).to_owned()).collect());
        assert!(
            !allowed(&env, &token, Action::Read, &res).await,
            "malformed scope {scopes:?} must narrow to nothing, not fall back to the owner's rights"
        );
    }
}

#[tokio::test]
async fn must_not_allow_on_selector_error() {
    let env = Env::new();
    let broken_source = env.selector("broken", "path.startsWith(").await;
    let needs_coordinate = env
        .selector("needs-coordinate", "!(coordinate.groupId == \"evil\")")
        .await;
    let huge_input = env.selector("any-path", "true").await;
    let dangling = rustryx_core::ids::ContentSelectorId::generate();
    let mut roles = Vec::new();
    for (name, selector) in [
        ("broken", broken_source),
        ("needs-coordinate", needs_coordinate),
        ("any-path", huge_input),
        ("dangling", dangling),
    ] {
        let role = env
            .simple_role(name, selector_grant("*", selector), &[Action::Read])
            .await;
        roles.push((role, name));
    }
    let broken = principal(&[roles[0]]);
    let unknown = principal(&[roles[1]]);
    let too_big = principal(&[roles[2]]);
    let ghost = principal(&[roles[3]]);

    // 1. A selector that does not compile matches nothing.
    assert!(
        !allowed(
            &env,
            &broken,
            Action::Read,
            &resource("r", RepoFormat::Raw, "/x")
        )
        .await
    );
    // 2. An evaluation error (the coordinate is not known) is not a match, and a
    //    negated comparison on unknown data is not a match either.
    assert!(
        !allowed(
            &env,
            &unknown,
            Action::Read,
            &resource("r", RepoFormat::Maven2, "/x")
        )
        .await
    );
    let known =
        resource_with_coordinate("r", RepoFormat::Maven2, "/x", Some("good"), Some("n"), None);
    assert!(
        allowed(&env, &unknown, Action::Read, &known).await,
        "with the data it can match"
    );
    // 3. An input over the limit is an evaluation error.
    let long_path = format!("/{}", "a".repeat(9000));
    assert!(
        !allowed(
            &env,
            &too_big,
            Action::Read,
            &resource("r", RepoFormat::Raw, &long_path)
        )
        .await
    );
    // 4. A privilege that points at a selector that does not exist.
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
async fn must_not_let_a_selector_error_block_an_unrelated_grant() {
    // Fail closed means the broken privilege contributes nothing, not that the
    // user loses everything else they were granted.
    let env = Env::new();
    let broken = env.selector("broken", "path ==").await;
    let broken_privilege = env
        .privilege(
            "broken-privilege",
            selector_grant("*", broken),
            &[Action::Read],
        )
        .await;
    let plain = env
        .privilege("plain", view("a", None), &[Action::Read])
        .await;
    let role = env.role("both", &[broken_privilege, plain], &[]).await;
    let user = principal(&[(role, "both")]);
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
        !allowed(
            &env,
            &user,
            Action::Read,
            &resource("b", RepoFormat::Raw, "/x")
        )
        .await
    );
}

#[tokio::test]
async fn must_not_match_a_selector_on_a_path_that_is_not_canonical() {
    let env = Env::new();
    let selector = env
        .selector("public-only", "path.startsWith(\"/public/\")")
        .await;
    let role = env
        .simple_role("public", selector_grant("*", selector), &[Action::Read])
        .await;
    let user = principal(&[(role, "public")]);
    assert!(
        allowed(
            &env,
            &user,
            Action::Read,
            &resource("r", RepoFormat::Raw, "public/a.txt")
        )
        .await
    );
    for path in [
        "public/../secret/a.txt",
        "/public/../secret/a.txt",
        "public/./a.txt",
        "public//a.txt",
        "public/a\0.txt",
        "public/a\n.txt",
        "public/..",
    ] {
        assert!(
            !allowed(
                &env,
                &user,
                Action::Read,
                &resource("r", RepoFormat::Raw, path)
            )
            .await,
            "{path:?}"
        );
    }
}

#[tokio::test]
async fn must_not_allow_on_store_error() {
    let env = Env::new();
    let dev = builtin_principal(ROLE_NX_DEVELOPER);
    let admin = builtin_principal(ROLE_NX_ADMIN);
    // Open, so that the anonymous caller gets past its gates and really reaches the store.
    let res = open_resource("r", RepoFormat::Raw, "/x");

    // Cold caches and a store that is down: every kind of principal is refused.
    env.store.inner.set_unavailable(true);
    for p in [
        &dev,
        &admin,
        &Principal::anonymous(),
        &principal(&[(RoleId::generate(), "x")]),
    ] {
        for action in [Action::Browse, Action::Read] {
            let result = env.rbac.check(p, action, &res).await;
            assert!(not_allowed(&result), "{p:?} {action:?}: {result:?}");
        }
        let visible = env.rbac.can_see_repository(p, "r", RepoFormat::Raw).await;
        assert!(!matches!(visible, Ok(true)), "{visible:?}");
    }
    let admin_check = env.rbac.is_admin(&admin).await;
    assert!(!matches!(admin_check, Ok(true)), "{admin_check:?}");
    env.store.inner.set_unavailable(false);
    assert!(
        allowed(&env, &dev, Action::Read, &res).await,
        "recovers when the store does"
    );
}

#[tokio::test]
async fn must_not_allow_on_selector_store_error() {
    let env = Env::new();
    let selector = env.selector("any", "true").await;
    let role = env
        .simple_role("sel", selector_grant("*", selector), &[Action::Read])
        .await;
    let user = principal(&[(role, "sel")]);
    env.store
        .fail_selectors
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let res = resource("r", RepoFormat::Raw, "/x");
    let result = env.rbac.check(&user, Action::Read, &res).await;
    assert!(
        matches!(result, Err(AuthError::Backend { .. })),
        "{result:?}"
    );
    let visible = env
        .rbac
        .can_see_repository(&user, "r", RepoFormat::Raw)
        .await;
    assert!(
        matches!(visible, Err(AuthError::Backend { .. })),
        "{visible:?}"
    );
}

#[tokio::test(start_paused = true)]
async fn must_not_allow_on_store_error_with_an_expired_cached_allow() {
    // A cached Allow may be served until its TTL; the first request after the
    // TTL needs the store, and when the store is down that is an error.
    let env = Env::new();
    let dev = builtin_principal(ROLE_NX_DEVELOPER);
    let res = resource("r", RepoFormat::Raw, "/x");
    assert!(allowed(&env, &dev, Action::Read, &res).await);
    env.store.inner.set_unavailable(true);
    tokio::time::advance(Duration::from_secs(30)).await;
    assert!(
        allowed(&env, &dev, Action::Read, &res).await,
        "within the TTL the cache answers"
    );
    tokio::time::advance(Duration::from_secs(31)).await;
    let result = env.rbac.check(&dev, Action::Read, &res).await;
    assert!(not_allowed(&result), "{result:?}");
}

#[tokio::test(start_paused = true)]
async fn must_not_serve_privileges_past_the_ttl() {
    let env = Env::new();
    let role_id = env
        .simple_role("temp", view("r", None), &[Action::Read])
        .await;
    let user = principal(&[(role_id, "temp")]);
    let res = resource("r", RepoFormat::Raw, "/x");
    assert!(allowed(&env, &user, Action::Read, &res).await);
    // Revoke without telling the cache (no bus message): the TTL is the only limit.
    let mut role = RoleRepo::get(&env.store.inner, role_id)
        .await
        .unwrap()
        .unwrap();
    role.privilege_ids.clear();
    RoleRepo::update(&env.store.inner, &role).await.unwrap();
    tokio::time::advance(Duration::from_secs(59)).await;
    assert!(
        allowed(&env, &user, Action::Read, &res).await,
        "still inside the 60 s TTL"
    );
    tokio::time::advance(Duration::from_secs(2)).await;
    assert!(
        !allowed(&env, &user, Action::Read, &res).await,
        "revoked right is gone after the TTL"
    );
}

#[tokio::test(start_paused = true)]
async fn must_not_keep_a_cache_entry_longer_than_five_minutes_whatever_is_configured() {
    let config = RbacConfig {
        decision_ttl: Duration::from_hours(24),
        ..RbacConfig::default()
    };
    let env = Env::with_config(&config);
    let role_id = env
        .simple_role("temp", view("r", None), &[Action::Read])
        .await;
    let user = principal(&[(role_id, "temp")]);
    let res = resource("r", RepoFormat::Raw, "/x");
    assert!(allowed(&env, &user, Action::Read, &res).await);
    let mut role = RoleRepo::get(&env.store.inner, role_id)
        .await
        .unwrap()
        .unwrap();
    role.privilege_ids.clear();
    RoleRepo::update(&env.store.inner, &role).await.unwrap();
    tokio::time::advance(Duration::from_secs(299)).await;
    assert!(allowed(&env, &user, Action::Read, &res).await);
    tokio::time::advance(Duration::from_secs(2)).await;
    assert!(!allowed(&env, &user, Action::Read, &res).await);
}

#[tokio::test]
async fn must_not_grant_admin_from_role_name_lookalike() {
    let env = Env::new();
    let res = resource("r", RepoFormat::Raw, "/x");
    for name in [
        "nx-admin ",
        " nx-admin",
        "NX-ADMIN",
        "Nx-Admin",
        "nx-admin-ish",
        "nx-administrator",
        "nx_admin",
        "admin",
        "nx-admin\u{200b}",
        "nx\u{2010}admin",
        "",
    ] {
        // Whatever a role like this is called, it is a different role.
        let lookalike = env
            .simple_role(name, view("only-here", None), &[Action::Read])
            .await;
        let user = principal(&[(lookalike, name)]);
        assert!(!allowed(&env, &user, Action::Write, &res).await, "{name:?}");
        assert!(
            !allowed(&env, &user, Action::Delete, &res).await,
            "{name:?}"
        );
        assert!(!allowed(&env, &user, Action::Read, &res).await, "{name:?}");
        assert!(!env.rbac.is_admin(&user).await.unwrap(), "{name:?}");
    }
}

#[tokio::test]
async fn must_not_grant_admin_to_a_role_ref_that_only_claims_the_name() {
    // The trap behind ADR 7: an identity provider group called "nx-admin" mapped
    // by name to a role reference. The name matches exactly, the id is not the
    // id of the real nx-admin role.
    let env = Env::new();
    let res = resource("r", RepoFormat::Raw, "/x");
    let forged = principal(&[(RoleId::generate(), ROLE_NX_ADMIN)]);
    assert!(
        forged.has_role(ROLE_NX_ADMIN),
        "the name based check of the principal is fooled, which is why nothing consults it"
    );
    for action in ACTIONS {
        assert!(!allowed(&env, &forged, action, &res).await);
    }
    assert!(!env.rbac.is_admin(&forged).await.unwrap());
    assert!(
        !env.rbac
            .can_see_repository(&forged, "r", RepoFormat::Raw)
            .await
            .unwrap()
    );

    // Claiming the name with the id of another real role gives that role's rights only.
    let developer_id = builtin_principal(ROLE_NX_DEVELOPER).roles[0].id;
    let mixed = principal(&[(developer_id, ROLE_NX_ADMIN)]);
    assert!(
        allowed(&env, &mixed, Action::Write, &res).await,
        "developer rights"
    );
    assert!(
        !allowed(&env, &mixed, Action::Delete, &res).await,
        "but no admin rights"
    );
    assert!(!env.rbac.is_admin(&mixed).await.unwrap());

    // The real one is admin whatever name it is carried under.
    let real_id = builtin_principal(ROLE_NX_ADMIN).roles[0].id;
    let real = principal(&[(real_id, "renamed-locally")]);
    assert!(allowed(&env, &real, Action::Delete, &res).await);
    assert!(env.rbac.is_admin(&real).await.unwrap());
}

#[tokio::test]
async fn must_not_leak_data_through_group_without_member_rights() {
    let env = Env::new();
    let reader = env
        .simple_role("reader-of-m1", view("m1", None), &[Action::Read])
        .await;
    let user = principal(&[(reader, "reader-of-m1")]);
    let members: Vec<Repository> = ["m1", "m2", "m3"]
        .iter()
        .map(|n| repo(n, RepoFormat::Maven2))
        .collect();
    let visible = env
        .rbac
        .filter_visible_members(&user, Action::Read, &members, "/a/b.jar", None)
        .await
        .unwrap();
    assert_eq!(names(&visible), ["m1"]);

    // Rights on the group itself are not rights on its members.
    let group_reader = env
        .simple_role("reader-of-group", view("the-group", None), &[Action::Read])
        .await;
    let group_user = principal(&[(group_reader, "reader-of-group")]);
    assert!(
        allowed(
            &env,
            &group_user,
            Action::Read,
            &resource("the-group", RepoFormat::Maven2, "/a")
        )
        .await
    );
    let visible = env
        .rbac
        .filter_visible_members(&group_user, Action::Read, &members, "/a/b.jar", None)
        .await
        .unwrap();
    assert_eq!(visible.len(), 0);

    // Nothing at all for anonymous write, and nothing for a user without roles.
    for p in [&Principal::anonymous(), &principal(&[])] {
        let visible = env
            .rbac
            .filter_visible_members(p, Action::Write, &members, "/a", None)
            .await
            .unwrap();
        assert_eq!(visible.len(), 0);
    }
}

#[tokio::test]
async fn must_not_leak_data_through_group_when_the_member_selector_does_not_match() {
    let env = Env::new();
    let selector = env
        .selector("only-open", "path.startsWith(\"/open/\")")
        .await;
    let role = env
        .simple_role(
            "open-reader",
            selector_grant("m2", selector),
            &[Action::Read],
        )
        .await;
    let user = principal(&[(role, "open-reader")]);
    let members = vec![repo("m1", RepoFormat::Raw), repo("m2", RepoFormat::Raw)];
    let open = env
        .rbac
        .filter_visible_members(&user, Action::Read, &members, "/open/a", None)
        .await
        .unwrap();
    assert_eq!(names(&open), ["m2"]);
    let closed = env
        .rbac
        .filter_visible_members(&user, Action::Read, &members, "/closed/a", None)
        .await
        .unwrap();
    assert_eq!(closed.len(), 0);
}

#[tokio::test]
async fn must_not_return_group_members_when_the_store_fails() {
    let env = Env::new();
    let dev = builtin_principal(ROLE_NX_DEVELOPER);
    let members = vec![open_repo("m1", RepoFormat::Raw)];
    env.store.inner.set_unavailable(true);
    let result = env
        .rbac
        .filter_visible_members(&dev, Action::Read, &members, "/x", None)
        .await;
    assert!(
        matches!(result, Err(AuthError::Backend { .. })),
        "{result:?}"
    );
}

#[tokio::test]
async fn must_not_return_group_members_when_a_selector_lookup_fails_midway() {
    // The first member is decided from a plain privilege, the second needs the selector
    // store, which is down: the whole call fails instead of returning the members decided so
    // far, because a caller that serves from a partial list would serve from a wrong one.
    let env = Env::new();
    let selector = env.selector("any", "true").await;
    let plain = env
        .privilege("plain", view("m1", None), &[Action::Read])
        .await;
    let narrow = env
        .privilege("narrow", selector_grant("m2", selector), &[Action::Read])
        .await;
    let role = env.role("both", &[plain, narrow], &[]).await;
    let user = principal(&[(role, "both")]);
    let members = vec![repo("m1", RepoFormat::Raw), repo("m2", RepoFormat::Raw)];
    env.store
        .fail_selectors
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let result = env
        .rbac
        .filter_visible_members(&user, Action::Read, &members, "/x", None)
        .await;
    assert!(
        matches!(result, Err(AuthError::Backend { .. })),
        "{result:?}"
    );
}

#[tokio::test]
async fn must_not_let_one_users_cached_privileges_serve_another_users_roles() {
    // The privilege cache is keyed by the exact role set, so two users whose
    // role sets differ never share an entry, even when they are checked in a row.
    let env = Env::new();
    let read_a = env
        .simple_role("read-a", view("a", None), &[Action::Read])
        .await;
    let read_b = env
        .simple_role("read-b", view("b", None), &[Action::Read])
        .await;
    let user_a = principal(&[(read_a, "read-a")]);
    let user_b = principal(&[(read_b, "read-b")]);
    let in_a = resource("a", RepoFormat::Raw, "/x");
    let in_b = resource("b", RepoFormat::Raw, "/x");
    assert!(allowed(&env, &user_a, Action::Read, &in_a).await);
    assert!(!allowed(&env, &user_b, Action::Read, &in_a).await);
    assert!(allowed(&env, &user_b, Action::Read, &in_b).await);
    assert!(!allowed(&env, &user_a, Action::Read, &in_b).await);
    // The same user id carrying a smaller role set later (a group was removed) is
    // evaluated with the smaller set at once, not with the earlier larger one.
    let mut shrunk = user_a.clone();
    shrunk.roles.clear();
    assert!(!allowed(&env, &shrunk, Action::Read, &in_a).await);
}
