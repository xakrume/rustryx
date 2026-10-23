//! The metrics the authorizer emits through the `metrics` facade: decisions, cache events
//! and invalidations. The names and labels are documented in `docs/rbac.md` and are what
//! the dashboards read, so they are pinned here.
//!
//! The recorder is per thread, so every test runs on the default current-thread runtime,
//! where the listener and the spawned loads run on the test thread too.

// Tests may unwrap and expect: a panic is the failure signal there.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::collections::BTreeSet;
use std::sync::Arc;

use common::recorder::Capture;
use common::{
    Env, SlowFirstLoad, anonymous_config, builtin_principal, open_resource, principal, repo,
    resource, selector_grant, view,
};
use rustryx_core::identity::{
    Action, AppArea, Principal, ROLE_NX_ADMIN, ROLE_NX_DEVELOPER, RoleRef,
};
use rustryx_core::ids::{ProjectId, UserId};
use rustryx_core::model::RepoFormat;
use rustryx_core::ports::auth::RbacChecker;
use rustryx_core::ports::{InvalidationBus, InvalidationEvent};
use rustryx_core::project::ProjectPermission;
use rustryx_rbac::Rbac;
use rustryx_rbac::metrics::{
    RBAC_CACHE_EVENTS_TOTAL, RBAC_DECISIONS_TOTAL, RBAC_INVALIDATIONS_TOTAL,
};
use tokio_util::sync::CancellationToken;

fn decisions(capture: &Capture, kind: &str, result: &str) -> u64 {
    capture.get(RBAC_DECISIONS_TOTAL, &[("kind", kind), ("result", result)])
}

fn cache(capture: &Capture, cache: &str, event: &str) -> u64 {
    capture.get(
        RBAC_CACHE_EVENTS_TOTAL,
        &[("cache", cache), ("event", event)],
    )
}

fn invalidations(capture: &Capture, event: &str) -> u64 {
    capture.get(RBAC_INVALIDATIONS_TOTAL, &[("event", event)])
}

#[test]
fn the_documented_names_are_the_ones_emitted() {
    assert_eq!(RBAC_DECISIONS_TOTAL, "rbac_decisions_total");
    assert_eq!(RBAC_CACHE_EVENTS_TOTAL, "rbac_cache_events_total");
    assert_eq!(RBAC_INVALIDATIONS_TOTAL, "rbac_invalidations_total");
}

#[tokio::test]
async fn content_decisions_are_counted_by_result() {
    let capture = Capture::default();
    let _installed = metrics::set_default_local_recorder(&capture);
    let env = Env::new();
    let dev = builtin_principal(ROLE_NX_DEVELOPER);
    let res = resource("r", RepoFormat::Raw, "/x");

    env.rbac.check(&dev, Action::Read, &res).await.unwrap();
    env.rbac.check(&dev, Action::Write, &res).await.unwrap();
    env.rbac.check(&dev, Action::Delete, &res).await.unwrap();
    assert_eq!(decisions(&capture, "content", "allow"), 2);
    assert_eq!(decisions(&capture, "content", "deny"), 1);

    // A decision taken before any store access (anonymous write) is counted as well.
    env.rbac
        .check(&Principal::anonymous(), Action::Write, &res)
        .await
        .unwrap();
    assert_eq!(decisions(&capture, "content", "deny"), 2);

    // A store failure is its own result: the dashboard must tell an outage from a refusal.
    let cold = Env::new();
    cold.store.inner.set_unavailable(true);
    assert!(cold.rbac.check(&dev, Action::Read, &res).await.is_err());
    assert_eq!(decisions(&capture, "content", "error"), 1);
    assert_eq!(capture.total(RBAC_DECISIONS_TOTAL), 5);
}

#[tokio::test]
async fn every_kind_of_question_has_its_own_series() {
    let capture = Capture::default();
    let _installed = metrics::set_default_local_recorder(&capture);
    let env = Env::new();
    let admin = builtin_principal(ROLE_NX_ADMIN);
    let dev = builtin_principal(ROLE_NX_DEVELOPER);

    assert!(env.rbac.is_admin(&admin).await.unwrap());
    assert!(!env.rbac.is_admin(&dev).await.unwrap());
    env.rbac
        .check_app(&admin, AppArea::Settings, Action::Read)
        .await
        .unwrap();
    env.rbac
        .check_app(&dev, AppArea::Settings, Action::Read)
        .await
        .unwrap();
    env.rbac
        .check_project(
            &admin,
            ProjectId::generate(),
            ProjectPermission::ViewProject,
        )
        .await
        .unwrap();
    env.rbac
        .check_project(&dev, ProjectId::generate(), ProjectPermission::ViewProject)
        .await
        .unwrap();

    assert_eq!(decisions(&capture, "admin", "allow"), 1);
    assert_eq!(decisions(&capture, "admin", "deny"), 1);
    assert_eq!(decisions(&capture, "app", "allow"), 1);
    assert_eq!(decisions(&capture, "app", "deny"), 1);
    assert_eq!(decisions(&capture, "project", "allow"), 1);
    assert_eq!(decisions(&capture, "project", "deny"), 1);
    assert_eq!(decisions(&capture, "content", "allow"), 0);
}

#[tokio::test]
async fn is_admin_counts_an_outage_as_an_error() {
    let capture = Capture::default();
    let _installed = metrics::set_default_local_recorder(&capture);
    let env = Env::new();
    env.store.inner.set_unavailable(true);
    assert!(
        env.rbac
            .is_admin(&builtin_principal(ROLE_NX_ADMIN))
            .await
            .is_err()
    );
    assert_eq!(decisions(&capture, "admin", "error"), 1);
}

#[tokio::test]
async fn every_member_of_a_group_is_one_content_decision() {
    let capture = Capture::default();
    let _installed = metrics::set_default_local_recorder(&capture);
    let env = Env::new();
    let reader = env
        .simple_role("reader-of-m1", view("m1", None), &[Action::Read])
        .await;
    let user = principal(&[(reader, "reader-of-m1")]);
    let members = vec![
        repo("m1", RepoFormat::Raw),
        repo("m2", RepoFormat::Raw),
        repo("m3", RepoFormat::Raw),
    ];
    let visible = env
        .rbac
        .filter_visible_members(&user, Action::Read, &members, "/x", None)
        .await
        .unwrap();
    assert_eq!(visible.len(), 1);
    assert_eq!(decisions(&capture, "content", "allow"), 1);
    assert_eq!(decisions(&capture, "content", "deny"), 2);
}

#[tokio::test]
async fn cache_events_tell_hits_from_store_reads() {
    let capture = Capture::default();
    let _installed = metrics::set_default_local_recorder(&capture);
    let env = Env::new();
    let dev = builtin_principal(ROLE_NX_DEVELOPER);
    let res = resource("r", RepoFormat::Raw, "/x");
    for _ in 0..4 {
        env.rbac.check(&dev, Action::Read, &res).await.unwrap();
    }
    // One read of each, then three hits of each.
    assert_eq!(cache(&capture, "privileges", "miss"), 1);
    assert_eq!(cache(&capture, "privileges", "hit"), 3);
    assert_eq!(cache(&capture, "builtin_roles", "miss"), 1);
    assert_eq!(cache(&capture, "builtin_roles", "hit"), 3);
    // The counters agree with what the store saw.
    assert_eq!(env.store.privilege_reads(), 1);
    assert_eq!(env.store.role_name_reads(), 1);
    assert_eq!(cache(&capture, "privileges", "error"), 0);
}

#[tokio::test]
async fn selector_and_program_caches_are_counted_too() {
    let capture = Capture::default();
    let _installed = metrics::set_default_local_recorder(&capture);
    let env = Env::new();
    let selector = env.selector("any", "true").await;
    let role = env
        .simple_role("sel", selector_grant("*", selector), &[Action::Read])
        .await;
    let user = principal(&[(role, "sel")]);
    for i in 0..3 {
        env.rbac
            .check(
                &user,
                Action::Read,
                &resource("r", RepoFormat::Raw, &format!("/x{i}")),
            )
            .await
            .unwrap();
    }
    assert_eq!(cache(&capture, "selectors", "miss"), 1);
    assert_eq!(cache(&capture, "selectors", "hit"), 2);
    assert_eq!(cache(&capture, "programs", "miss"), 1, "compiled once");
}

#[tokio::test]
async fn a_failed_load_is_an_error_event_and_not_a_miss() {
    let capture = Capture::default();
    let _installed = metrics::set_default_local_recorder(&capture);
    let env = Env::new();
    let dev = builtin_principal(ROLE_NX_DEVELOPER);
    let res = resource("r", RepoFormat::Raw, "/x");
    env.store.inner.set_unavailable(true);
    assert!(env.rbac.check(&dev, Action::Read, &res).await.is_err());
    assert!(env.rbac.check(&dev, Action::Read, &res).await.is_err());
    // The role id lookup comes first and fails both times: nothing was cached.
    assert_eq!(cache(&capture, "builtin_roles", "error"), 2);
    assert_eq!(cache(&capture, "builtin_roles", "miss"), 0);
    env.store.inner.set_unavailable(false);
    env.rbac.check(&dev, Action::Read, &res).await.unwrap();
    assert_eq!(cache(&capture, "builtin_roles", "miss"), 1);
}

#[tokio::test]
async fn a_caller_that_waits_for_another_callers_load_counts_as_a_hit() {
    let capture = Capture::default();
    let _installed = metrics::set_default_local_recorder(&capture);
    let env = Env::new();
    let slow = SlowFirstLoad::parking(env.store.clone());
    let (parked, release) = (slow.parked.clone(), slow.release.clone());
    let rbac = Rbac::new(
        Arc::new(slow),
        Arc::new(env.store.clone()),
        &anonymous_config(),
    );
    let dev = builtin_principal(ROLE_NX_DEVELOPER);
    let res = resource("r", RepoFormat::Raw, "/x");

    let first = {
        let (rbac, dev, res) = (rbac.clone(), dev.clone(), res.clone());
        tokio::spawn(async move { rbac.check(&dev, Action::Read, &res).await })
    };
    parked.notified().await;
    let second = {
        let (rbac, dev, res) = (rbac.clone(), dev.clone(), res.clone());
        tokio::spawn(async move { rbac.check(&dev, Action::Read, &res).await })
    };
    // Let the second caller reach the cache and queue behind the parked load.
    for _ in 0..20 {
        tokio::task::yield_now().await;
    }
    release.notify_one();
    first.await.unwrap().unwrap();
    second.await.unwrap().unwrap();
    assert_eq!(cache(&capture, "privileges", "miss"), 1);
    assert_eq!(cache(&capture, "privileges", "hit"), 1);
    assert_eq!(env.store.privilege_reads(), 1, "one store read served both");
}

#[tokio::test]
async fn invalidation_messages_are_counted_by_kind() {
    let capture = Capture::default();
    let _installed = metrics::set_default_local_recorder(&capture);
    let env = Env::new();
    let cancel = CancellationToken::new();
    let listener = env
        .rbac
        .spawn_invalidation_listener(&env.bus, cancel.clone());
    for event in [
        InvalidationEvent::Rbac,
        InvalidationEvent::User(UserId::generate().as_uuid()),
        InvalidationEvent::User(UserId::generate().as_uuid()),
        InvalidationEvent::Reset,
        InvalidationEvent::Projects,
        InvalidationEvent::Config(7),
    ] {
        env.bus.publish(event).await.unwrap();
    }
    for _ in 0..40 {
        tokio::task::yield_now().await;
    }
    assert_eq!(invalidations(&capture, "rbac"), 1);
    assert_eq!(invalidations(&capture, "user"), 2);
    assert_eq!(invalidations(&capture, "reset"), 1);
    assert_eq!(invalidations(&capture, "projects"), 1);
    assert_eq!(invalidations(&capture, "config"), 1);
    assert_eq!(invalidations(&capture, "stream_end"), 0);
    cancel.cancel();
    listener.join().await.unwrap();
    assert_eq!(
        invalidations(&capture, "stream_end"),
        0,
        "a cancelled listener is not a lost bus"
    );
}

#[tokio::test]
async fn a_lost_bus_is_counted_as_a_stream_end_and_a_reset() {
    let capture = Capture::default();
    let _installed = metrics::set_default_local_recorder(&capture);
    let env = Env::new();
    let bus = rustryx_testkit::MemInvalidationBus::new();
    let listener = env
        .rbac
        .spawn_invalidation_listener(&bus, CancellationToken::new());
    drop(bus);
    listener.join().await.unwrap();
    assert_eq!(invalidations(&capture, "stream_end"), 1);
    assert_eq!(invalidations(&capture, "reset"), 1);
}

#[tokio::test]
async fn a_direct_invalidation_is_counted_like_a_bus_message() {
    let capture = Capture::default();
    let _installed = metrics::set_default_local_recorder(&capture);
    let env = Env::new();
    env.rbac.invalidate(&InvalidationEvent::Rbac);
    assert_eq!(invalidations(&capture, "rbac"), 1);
}

#[tokio::test]
async fn must_not_put_unbounded_values_in_labels() {
    // SRE-09: no principal, role, repository, path, selector or group name may become a label
    // value. Drive every path with recognisable names and look at every series.
    let capture = Capture::default();
    let _installed = metrics::set_default_local_recorder(&capture);
    let env = Env::new();
    let selector = env.selector("canary-selector-name", "true").await;
    let role = env
        .simple_role(
            "canary-role-name",
            selector_grant("canary-repository", selector),
            &[Action::Read],
        )
        .await;
    let mut user = principal(&[(role, "canary-role-name")]);
    user.username = "canary-user-name".into();
    user.external_groups = vec!["/canary-group".into()];
    user.roles.push(RoleRef {
        id: rustryx_core::ids::RoleId::generate(),
        name: "canary-forged-role".into(),
    });
    let res = open_resource("canary-repository", RepoFormat::Raw, "/canary/path");
    env.rbac.check(&user, Action::Read, &res).await.unwrap();
    env.rbac.is_admin(&user).await.unwrap();
    env.rbac
        .check_app(&user, AppArea::Users, Action::Read)
        .await
        .unwrap();
    env.rbac
        .check_project(&user, ProjectId::generate(), ProjectPermission::ViewProject)
        .await
        .unwrap();
    env.rbac
        .filter_visible_members(
            &user,
            Action::Read,
            &[repo("canary-repository", RepoFormat::Raw)],
            "/canary/path",
            None,
        )
        .await
        .unwrap();
    env.rbac
        .invalidate(&InvalidationEvent::User(user.user_id.unwrap().as_uuid()));

    let allowed_label_names: BTreeSet<&str> = ["kind", "result", "cache", "event"].into();
    let series = capture.series();
    assert!(series.len() >= 4, "the run produced series: {series:?}");
    for (name, labels) in series {
        assert!(name.starts_with("rbac_"), "{name}");
        for (key, value) in labels {
            assert!(allowed_label_names.contains(key.as_str()), "{name}: {key}");
            assert!(
                !value.contains("canary") && !value.contains('/'),
                "{name}: {key}={value}"
            );
        }
    }
}
