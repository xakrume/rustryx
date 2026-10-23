//! `Rbac::filter_visible_members`: authorization of group repositories is per
//! member (SEC-13). The refusals that must never turn into data are in
//! `rbac_must_not.rs`; this file covers the ordinary behaviour.

// Tests may unwrap and expect: a panic is the failure signal there.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::borrow::Borrow;
use std::sync::Arc;

use common::{
    Env, builtin_principal, coordinate, open_repo, principal, repo, selector_grant, view,
};
use rustryx_core::identity::{Action, Principal, ROLE_NX_ADMIN, ROLE_NX_DEVELOPER};
use rustryx_core::model::{RepoFormat, Repository};

fn members(names: &[&str]) -> Vec<Repository> {
    names.iter().map(|n| repo(n, RepoFormat::Raw)).collect()
}

fn names<'a, R: Borrow<Repository>>(visible: &[&'a R]) -> Vec<&'a str> {
    visible
        .iter()
        .map(|member| {
            let repository: &'a Repository = (*member).borrow();
            repository.name.as_str()
        })
        .collect()
}

#[tokio::test]
async fn members_keep_the_resolution_order_of_the_group() {
    let env = Env::new();
    let privilege_a = env
        .privilege("a", view("zeta", None), &[Action::Read])
        .await;
    let privilege_b = env
        .privilege("b", view("alpha", None), &[Action::Read])
        .await;
    let privilege_c = env.privilege("c", view("mid", None), &[Action::Read]).await;
    let role = env
        .role("three", &[privilege_a, privilege_b, privilege_c], &[])
        .await;
    let user = principal(&[(role, "three")]);
    let group = members(&["zeta", "hidden", "alpha", "mid"]);
    let visible = env
        .rbac
        .filter_visible_members(&user, Action::Read, &group, "/x", None)
        .await
        .unwrap();
    assert_eq!(names(&visible), ["zeta", "alpha", "mid"]);
}

#[tokio::test]
async fn administrators_and_wildcard_readers_see_every_member() {
    let env = Env::new();
    let group: Vec<Repository> = ["m1", "m2", "m3"]
        .iter()
        .map(|n| repo(n, RepoFormat::Npm))
        .collect();
    for user in [
        builtin_principal(ROLE_NX_ADMIN),
        builtin_principal(ROLE_NX_DEVELOPER),
    ] {
        let visible = env
            .rbac
            .filter_visible_members(&user, Action::Read, &group, "/pkg", None)
            .await
            .unwrap();
        assert_eq!(visible.len(), 3);
    }
}

#[tokio::test]
async fn the_action_is_part_of_the_member_decision() {
    let env = Env::new();
    let role = env
        .simple_role("reader", view("*", None), &[Action::Read])
        .await;
    let user = principal(&[(role, "reader")]);
    let group = members(&["m1", "m2"]);
    let read = env
        .rbac
        .filter_visible_members(&user, Action::Read, &group, "/x", None)
        .await
        .unwrap();
    let write = env
        .rbac
        .filter_visible_members(&user, Action::Write, &group, "/x", None)
        .await
        .unwrap();
    assert_eq!(read.len(), 2);
    assert_eq!(write.len(), 0);
}

#[tokio::test]
async fn the_format_of_each_member_decides_not_the_format_of_the_group() {
    let env = Env::new();
    let role = env
        .simple_role(
            "maven-reader",
            view("*", Some(RepoFormat::Maven2)),
            &[Action::Read],
        )
        .await;
    let user = principal(&[(role, "maven-reader")]);
    // A mixed group is a data bug, but the answer must follow the member, so the format
    // restriction of a grant can never be satisfied by the group's own format.
    let group = vec![
        repo("maven-member", RepoFormat::Maven2),
        repo("npm-member", RepoFormat::Npm),
    ];
    let visible = env
        .rbac
        .filter_visible_members(&user, Action::Read, &group, "/x", None)
        .await
        .unwrap();
    assert_eq!(names(&visible), ["maven-member"]);
}

#[tokio::test]
async fn token_scopes_limit_the_members_too() {
    let env = Env::new();
    let mut token = builtin_principal(ROLE_NX_DEVELOPER);
    token.token_scopes = Some(vec!["repo:m2:read".into()]);
    let group = members(&["m1", "m2"]);
    let visible = env
        .rbac
        .filter_visible_members(&token, Action::Read, &group, "/x", None)
        .await
        .unwrap();
    assert_eq!(names(&visible), ["m2"]);
}

#[tokio::test]
async fn selectors_see_the_coordinate_of_the_requested_component() {
    let env = Env::new();
    let selector = env
        .selector("acme", "coordinate.group == \"com.acme\"")
        .await;
    let role = env
        .simple_role(
            "acme-reader",
            selector_grant("*", selector),
            &[Action::Read],
        )
        .await;
    let user = principal(&[(role, "acme-reader")]);
    let group = members(&["m1", "m2"]);
    let acme = coordinate(Some("com.acme"), Some("lib"), Some("1"));
    let other = coordinate(Some("org.other"), Some("lib"), Some("1"));
    let hit = env
        .rbac
        .filter_visible_members(&user, Action::Read, &group, "/p", Some(&acme))
        .await
        .unwrap();
    let miss = env
        .rbac
        .filter_visible_members(&user, Action::Read, &group, "/p", Some(&other))
        .await
        .unwrap();
    let unknown = env
        .rbac
        .filter_visible_members(&user, Action::Read, &group, "/p", None)
        .await
        .unwrap();
    assert_eq!(hit.len(), 2);
    assert_eq!(miss.len(), 0);
    assert_eq!(unknown.len(), 0);
}

#[tokio::test]
async fn an_empty_group_has_no_visible_members_and_costs_no_privilege_lookup_for_nobody() {
    let env = Env::new();
    let user = principal(&[]);
    let group: [Repository; 0] = [];
    let visible = env
        .rbac
        .filter_visible_members(&user, Action::Read, &group, "/x", None)
        .await
        .unwrap();
    assert_eq!(visible.len(), 0);
    assert_eq!(env.store.privilege_reads(), 0);
}

#[tokio::test]
async fn all_members_are_decided_with_one_privilege_lookup() {
    let env = Env::new();
    let user = builtin_principal(ROLE_NX_DEVELOPER);
    let group = members(&["m1", "m2", "m3", "m4", "m5"]);
    let visible = env
        .rbac
        .filter_visible_members(&user, Action::Read, &group, "/x", None)
        .await
        .unwrap();
    assert_eq!(visible.len(), 5);
    assert_eq!(env.store.privilege_reads(), 1);
}

#[tokio::test]
async fn shared_and_borrowed_member_slices_work_like_owned_ones() {
    let env = Env::new();
    let user = builtin_principal(ROLE_NX_DEVELOPER);
    let shared: Vec<Arc<Repository>> = members(&["m1", "m2"]).into_iter().map(Arc::new).collect();
    let visible = env
        .rbac
        .filter_visible_members(&user, Action::Read, &shared, "/x", None)
        .await
        .unwrap();
    assert_eq!(visible.len(), 2);
    assert!(
        Arc::ptr_eq(visible[0], &shared[0]),
        "the members themselves come back"
    );

    let owned = members(&["m1", "m2"]);
    let borrowed: Vec<&Repository> = owned.iter().collect();
    let visible = env
        .rbac
        .filter_visible_members(&user, Action::Read, &borrowed, "/x", None)
        .await
        .unwrap();
    assert_eq!(visible.len(), 2);
}

#[tokio::test]
async fn an_anonymous_caller_sees_exactly_the_members_that_opted_in() {
    let env = Env::new();
    let group = vec![
        open_repo("open-a", RepoFormat::Raw),
        repo("closed", RepoFormat::Raw),
        open_repo("open-b", RepoFormat::Raw),
    ];
    let visible = env
        .rbac
        .filter_visible_members(&Principal::anonymous(), Action::Read, &group, "/x", None)
        .await
        .unwrap();
    assert_eq!(names(&visible), ["open-a", "open-b"]);
    // An authenticated reader is not subject to the anonymous flag.
    let dev = builtin_principal(ROLE_NX_DEVELOPER);
    let visible = env
        .rbac
        .filter_visible_members(&dev, Action::Read, &group, "/x", None)
        .await
        .unwrap();
    assert_eq!(visible.len(), 3);
}
