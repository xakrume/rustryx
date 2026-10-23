//! Caching, expiry and invalidation of the authorizer.
//!
//! Time based tests use tokio's paused clock; the caches stamp entries with
//! `tokio::time::Instant`, so `advance` moves them. Invalidation tests run the
//! real listener against the testkit's in-process bus, which the in-memory
//! store also publishes to, exactly as PostgreSQL's `NOTIFY` would.

// Tests may unwrap and expect: a panic is the failure signal there.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::sync::Arc;
use std::time::Duration;

use common::{
    Env, SlowFirstLoad, anonymous_config, builtin_principal, open_resource, principal, resource,
    selector_grant, view,
};
use rustryx_core::identity::{Action, Principal, ROLE_NX_DEVELOPER};
use rustryx_core::ids::{RoleId, UserId};
use rustryx_core::model::RepoFormat;
use rustryx_core::ports::auth::{Decision, RbacChecker, ResourceRef};
use rustryx_core::ports::{InvalidationBus, InvalidationEvent, RoleRepo, SelectorRepo};
use rustryx_rbac::{InvalidationListener, Rbac, RbacConfig};
use rustryx_testkit::MemInvalidationBus;
use time::OffsetDateTime;
use tokio_util::sync::CancellationToken;

async fn allowed(env: &Env, p: &Principal, action: Action, r: &ResourceRef) -> bool {
    env.rbac.check(p, action, r).await.unwrap() == Decision::Allow
}

/// Lets the listener task run. The test runtime is single threaded, so the
/// listener only makes progress while the test yields.
async fn settle() {
    for _ in 0..20 {
        tokio::task::yield_now().await;
    }
}

fn start_listener(env: &Env) -> (InvalidationListener, CancellationToken) {
    let cancel = CancellationToken::new();
    let listener = env
        .rbac
        .spawn_invalidation_listener(&env.bus, cancel.clone());
    (listener, cancel)
}

/// Replaces the privileges of a role in the store (which publishes `Rbac`).
async fn revoke(env: &Env, role_id: RoleId) {
    let mut role = RoleRepo::get(&env.store.inner, role_id)
        .await
        .unwrap()
        .unwrap();
    role.privilege_ids.clear();
    RoleRepo::update(&env.store.inner, &role).await.unwrap();
}

#[tokio::test]
async fn privileges_and_the_admin_role_id_are_read_once_and_then_cached() {
    let env = Env::new();
    let dev = builtin_principal(ROLE_NX_DEVELOPER);
    let res = resource("r", RepoFormat::Raw, "/x");
    for _ in 0..5 {
        assert!(allowed(&env, &dev, Action::Read, &res).await);
    }
    assert_eq!(env.store.privilege_reads(), 1);
    assert_eq!(env.store.role_name_reads(), 1);
    // Another action and another repository are served from the same entry.
    assert!(
        allowed(
            &env,
            &dev,
            Action::Write,
            &resource("other", RepoFormat::Npm, "/y")
        )
        .await
    );
    assert_eq!(env.store.privilege_reads(), 1);
}

#[tokio::test]
async fn the_order_of_roles_does_not_split_the_cache() {
    let env = Env::new();
    let a = env.simple_role("a", view("a", None), &[Action::Read]).await;
    let b = env.simple_role("b", view("b", None), &[Action::Read]).await;
    let first = principal(&[(a, "a"), (b, "b")]);
    let mut second = first.clone();
    second.user_id = first.user_id;
    second.roles.reverse();
    let res = resource("a", RepoFormat::Raw, "/x");
    assert!(allowed(&env, &first, Action::Read, &res).await);
    assert!(allowed(&env, &second, Action::Read, &res).await);
    assert_eq!(env.store.privilege_reads(), 1);
}

#[tokio::test]
async fn anonymous_checks_share_one_cache_entry() {
    let env = Env::new();
    let res = open_resource("r", RepoFormat::Raw, "/x");
    for _ in 0..3 {
        assert!(allowed(&env, &Principal::anonymous(), Action::Read, &res).await);
    }
    assert_eq!(env.store.privilege_reads(), 1);
    assert_eq!(env.store.role_name_reads(), 1);
}

#[tokio::test(start_paused = true)]
async fn entries_are_reloaded_after_the_ttl() {
    let env = Env::new();
    let dev = builtin_principal(ROLE_NX_DEVELOPER);
    let res = resource("r", RepoFormat::Raw, "/x");
    assert!(allowed(&env, &dev, Action::Read, &res).await);
    tokio::time::advance(Duration::from_secs(59)).await;
    assert!(allowed(&env, &dev, Action::Read, &res).await);
    assert_eq!(env.store.privilege_reads(), 1);
    tokio::time::advance(Duration::from_secs(2)).await;
    assert!(allowed(&env, &dev, Action::Read, &res).await);
    assert_eq!(env.store.privilege_reads(), 2);
    assert_eq!(env.store.role_name_reads(), 2);
}

#[tokio::test(start_paused = true)]
async fn a_configured_ttl_shorter_than_the_default_is_honoured() {
    let config = RbacConfig {
        decision_ttl: Duration::from_secs(5),
        ..RbacConfig::default()
    };
    let env = Env::with_config(&config);
    let dev = builtin_principal(ROLE_NX_DEVELOPER);
    let res = resource("r", RepoFormat::Raw, "/x");
    assert!(allowed(&env, &dev, Action::Read, &res).await);
    tokio::time::advance(Duration::from_secs(6)).await;
    assert!(allowed(&env, &dev, Action::Read, &res).await);
    assert_eq!(env.store.privilege_reads(), 2);
}

#[tokio::test]
async fn a_failed_load_is_not_cached() {
    let env = Env::new();
    let dev = builtin_principal(ROLE_NX_DEVELOPER);
    let res = resource("r", RepoFormat::Raw, "/x");
    env.store.inner.set_unavailable(true);
    assert!(env.rbac.check(&dev, Action::Read, &res).await.is_err());
    assert!(env.rbac.check(&dev, Action::Read, &res).await.is_err());
    env.store.inner.set_unavailable(false);
    assert!(
        allowed(&env, &dev, Action::Read, &res).await,
        "no negative caching"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_first_requests_share_one_store_read() {
    let env = Arc::new(Env::new());
    let dev = builtin_principal(ROLE_NX_DEVELOPER);
    let mut tasks = Vec::new();
    for _ in 0..32 {
        let (env, dev) = (env.clone(), dev.clone());
        tasks.push(tokio::spawn(async move {
            allowed(
                &env,
                &dev,
                Action::Read,
                &resource("r", RepoFormat::Raw, "/x"),
            )
            .await
        }));
    }
    for task in tasks {
        assert!(task.await.unwrap());
    }
    assert_eq!(env.store.privilege_reads(), 1);
}

#[tokio::test]
async fn a_bus_message_drops_cached_privileges_before_the_ttl() {
    let env = Env::new();
    let role = env
        .simple_role("temp", view("r", None), &[Action::Read])
        .await;
    let (listener, cancel) = start_listener(&env);
    let user = principal(&[(role, "temp")]);
    let res = resource("r", RepoFormat::Raw, "/x");
    assert!(allowed(&env, &user, Action::Read, &res).await);
    revoke(&env, role).await;
    settle().await;
    assert!(
        !allowed(&env, &user, Action::Read, &res).await,
        "the Rbac event reached this node"
    );
    cancel.cancel();
    listener.join().await.unwrap();
}

#[tokio::test]
async fn without_a_listener_a_change_is_only_seen_after_the_ttl() {
    let env = Env::new();
    let role = env
        .simple_role("temp", view("r", None), &[Action::Read])
        .await;
    let user = principal(&[(role, "temp")]);
    let res = resource("r", RepoFormat::Raw, "/x");
    assert!(allowed(&env, &user, Action::Read, &res).await);
    revoke(&env, role).await;
    settle().await;
    assert!(
        allowed(&env, &user, Action::Read, &res).await,
        "nobody told the cache"
    );
}

#[tokio::test]
async fn a_user_message_drops_only_that_users_entries() {
    let env = Env::new();
    let role = env
        .simple_role("shared", view("r", None), &[Action::Read])
        .await;
    // Subscribe after the setup writes: they publish Rbac messages of their own
    // and a listener that already existed would drop the caches when it got to them.
    let (listener, cancel) = start_listener(&env);
    let alice = principal(&[(role, "shared")]);
    let bob = principal(&[(role, "shared")]);
    assert_ne!(alice.user_id, bob.user_id);
    let res = resource("r", RepoFormat::Raw, "/x");
    assert!(allowed(&env, &alice, Action::Read, &res).await);
    assert!(allowed(&env, &bob, Action::Read, &res).await);
    let before = env.store.privilege_reads();
    env.bus
        .publish(InvalidationEvent::User(alice.user_id.unwrap().as_uuid()))
        .await
        .unwrap();
    settle().await;
    assert!(allowed(&env, &bob, Action::Read, &res).await);
    assert_eq!(env.store.privilege_reads(), before, "bob is untouched");
    assert!(allowed(&env, &alice, Action::Read, &res).await);
    assert_eq!(env.store.privilege_reads(), before + 1, "alice is reloaded");
    cancel.cancel();
    listener.join().await.unwrap();
}

#[tokio::test]
async fn a_message_about_an_unknown_user_changes_nothing() {
    let env = Env::new();
    let (listener, cancel) = start_listener(&env);
    let dev = builtin_principal(ROLE_NX_DEVELOPER);
    let res = resource("r", RepoFormat::Raw, "/x");
    assert!(allowed(&env, &dev, Action::Read, &res).await);
    env.bus
        .publish(InvalidationEvent::User(UserId::generate().as_uuid()))
        .await
        .unwrap();
    settle().await;
    assert!(allowed(&env, &dev, Action::Read, &res).await);
    assert_eq!(env.store.privilege_reads(), 1);
    cancel.cancel();
    listener.join().await.unwrap();
}

#[tokio::test]
async fn project_and_config_messages_leave_the_privilege_caches_alone() {
    // Nothing cached here depends on projects or settings yet; dropping the privileges for
    // them would only turn every project change into a wave of store reads.
    let env = Env::new();
    let (listener, cancel) = start_listener(&env);
    let dev = builtin_principal(ROLE_NX_DEVELOPER);
    let res = resource("r", RepoFormat::Raw, "/x");
    assert!(allowed(&env, &dev, Action::Read, &res).await);
    for event in [InvalidationEvent::Projects, InvalidationEvent::Config(42)] {
        env.bus.publish(event).await.unwrap();
    }
    settle().await;
    env.rbac.invalidate(&InvalidationEvent::Projects);
    env.rbac.invalidate(&InvalidationEvent::Config(43));
    assert!(allowed(&env, &dev, Action::Read, &res).await);
    assert_eq!(env.store.privilege_reads(), 1);
    assert_eq!(env.store.role_name_reads(), 1);
    cancel.cancel();
    listener.join().await.unwrap();
}

#[tokio::test]
async fn the_privilege_caches_still_drop_for_the_messages_that_concern_them() {
    // The counterpart of the test above: the new arms did not swallow the old ones.
    let env = Env::new();
    let dev = builtin_principal(ROLE_NX_DEVELOPER);
    let res = resource("r", RepoFormat::Raw, "/x");
    assert!(allowed(&env, &dev, Action::Read, &res).await);
    env.rbac.invalidate(&InvalidationEvent::Rbac);
    assert!(allowed(&env, &dev, Action::Read, &res).await);
    assert_eq!(env.store.privilege_reads(), 2);
    env.rbac.invalidate(&InvalidationEvent::Reset);
    assert!(allowed(&env, &dev, Action::Read, &res).await);
    assert_eq!(env.store.privilege_reads(), 3);
}

#[tokio::test]
async fn reset_drops_everything() {
    let env = Env::new();
    let selector = env.selector("any", "true").await;
    let role = env
        .simple_role("sel", selector_grant("*", selector), &[Action::Read])
        .await;
    let (listener, cancel) = start_listener(&env);
    let user = principal(&[(role, "sel")]);
    let res = resource("r", RepoFormat::Raw, "/x");
    assert!(allowed(&env, &user, Action::Read, &res).await);
    assert!(allowed(&env, &user, Action::Read, &res).await);
    assert_eq!(
        (env.store.privilege_reads(), env.store.selector_reads()),
        (1, 1)
    );
    env.bus.publish(InvalidationEvent::Reset).await.unwrap();
    settle().await;
    assert!(allowed(&env, &user, Action::Read, &res).await);
    assert_eq!(
        (env.store.privilege_reads(), env.store.selector_reads()),
        (2, 2)
    );
    assert_eq!(env.store.role_name_reads(), 2);
    cancel.cancel();
    listener.join().await.unwrap();
}

#[tokio::test]
async fn a_lagging_subscriber_is_reset_and_therefore_cannot_miss_a_change() {
    // The bus turns lag into Reset; the listener must treat it like any event.
    let env = Env::new();
    let small_bus = MemInvalidationBus::with_capacity(1);
    let cancel = CancellationToken::new();
    let listener = env
        .rbac
        .spawn_invalidation_listener(&small_bus, cancel.clone());
    let role = env
        .simple_role("temp", view("r", None), &[Action::Read])
        .await;
    let user = principal(&[(role, "temp")]);
    let res = resource("r", RepoFormat::Raw, "/x");
    assert!(allowed(&env, &user, Action::Read, &res).await);
    revoke(&env, role).await;
    for _ in 0..10 {
        small_bus.emit(InvalidationEvent::User(UserId::generate().as_uuid()));
    }
    settle().await;
    assert!(!allowed(&env, &user, Action::Read, &res).await);
    cancel.cancel();
    listener.join().await.unwrap();
}

#[tokio::test]
async fn an_edited_selector_takes_effect_after_the_message() {
    let env = Env::new();
    let selector_id = env.selector("docs", "path.startsWith(\"/docs/\")").await;
    let role = env
        .simple_role("sel", selector_grant("*", selector_id), &[Action::Read])
        .await;
    let (listener, cancel) = start_listener(&env);
    let user = principal(&[(role, "sel")]);
    let docs = resource("r", RepoFormat::Raw, "/docs/a");
    let site = resource("r", RepoFormat::Raw, "/site/a");
    assert!(allowed(&env, &user, Action::Read, &docs).await);
    assert!(!allowed(&env, &user, Action::Read, &site).await);

    let mut edited = SelectorRepo::get(&env.store.inner, selector_id)
        .await
        .unwrap()
        .unwrap();
    edited.expression = "path.startsWith(\"/site/\")".into();
    edited.updated_at = OffsetDateTime::now_utc() + time::Duration::seconds(1);
    SelectorRepo::update(&env.store.inner, &edited)
        .await
        .unwrap();
    assert!(
        allowed(&env, &user, Action::Read, &docs).await,
        "cached until told or expired"
    );
    settle().await;
    assert!(!allowed(&env, &user, Action::Read, &docs).await);
    assert!(allowed(&env, &user, Action::Read, &site).await);
    cancel.cancel();
    listener.join().await.unwrap();
}

#[tokio::test]
async fn an_edit_that_forgot_to_move_updated_at_is_still_compiled_afresh() {
    // The compiled program is cached by (id, updated_at). If a writer changes the
    // expression and leaves updated_at alone, the stale program must not be used.
    let env = Env::new();
    let selector_id = env.selector("docs", "path.startsWith(\"/docs/\")").await;
    let role = env
        .simple_role("sel", selector_grant("*", selector_id), &[Action::Read])
        .await;
    let (listener, cancel) = start_listener(&env);
    let user = principal(&[(role, "sel")]);
    let docs = resource("r", RepoFormat::Raw, "/docs/a");
    assert!(allowed(&env, &user, Action::Read, &docs).await);
    let mut edited = SelectorRepo::get(&env.store.inner, selector_id)
        .await
        .unwrap()
        .unwrap();
    edited.expression = "false".into();
    SelectorRepo::update(&env.store.inner, &edited)
        .await
        .unwrap();
    settle().await;
    assert!(!allowed(&env, &user, Action::Read, &docs).await);
    cancel.cancel();
    listener.join().await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn an_edited_selector_is_seen_after_the_ttl_without_any_message() {
    let env = Env::new();
    let selector_id = env.selector("docs", "path.startsWith(\"/docs/\")").await;
    let role = env
        .simple_role("sel", selector_grant("*", selector_id), &[Action::Read])
        .await;
    let user = principal(&[(role, "sel")]);
    let docs = resource("r", RepoFormat::Raw, "/docs/a");
    assert!(allowed(&env, &user, Action::Read, &docs).await);
    let mut edited = SelectorRepo::get(&env.store.inner, selector_id)
        .await
        .unwrap()
        .unwrap();
    edited.expression = "false".into();
    edited.updated_at = OffsetDateTime::now_utc() + time::Duration::seconds(1);
    SelectorRepo::update(&env.store.inner, &edited)
        .await
        .unwrap();
    tokio::time::advance(Duration::from_secs(61)).await;
    assert!(!allowed(&env, &user, Action::Read, &docs).await);
}

#[tokio::test]
async fn a_load_that_straddles_an_invalidation_cannot_poison_the_cache() {
    let env = Env::new();
    let slow = SlowFirstLoad::parking(env.store.clone());
    let (parked, release) = (slow.parked.clone(), slow.release.clone());
    let rbac = Rbac::new(
        Arc::new(slow),
        Arc::new(env.store.clone()),
        &anonymous_config(),
    );

    let role = env
        .simple_role("temp", view("r", None), &[Action::Read])
        .await;
    let user = principal(&[(role, "temp")]);
    let res = resource("r", RepoFormat::Raw, "/x");

    let in_flight = {
        let (rbac, user, res) = (rbac.clone(), user.clone(), res.clone());
        tokio::spawn(async move { rbac.check(&user, Action::Read, &res).await })
    };
    parked.notified().await;
    // The load has read the old privileges. Now the role changes and every node
    // is told, then the slow load finishes and stores what it read.
    revoke(&env, role).await;
    rbac.invalidate(&InvalidationEvent::Rbac);
    release.notify_one();
    assert_eq!(
        in_flight.await.unwrap().unwrap(),
        Decision::Allow,
        "it answers from what it read"
    );
    // The stale result must not be what the next request sees.
    assert_eq!(
        rbac.check(&user, Action::Read, &res).await.unwrap(),
        Decision::Deny
    );
}

#[tokio::test]
async fn direct_invalidation_without_a_bus_works_too() {
    let env = Env::new();
    let role = env
        .simple_role("temp", view("r", None), &[Action::Read])
        .await;
    let user = principal(&[(role, "temp")]);
    let res = resource("r", RepoFormat::Raw, "/x");
    assert!(allowed(&env, &user, Action::Read, &res).await);
    revoke(&env, role).await;
    env.rbac.invalidate(&InvalidationEvent::Rbac);
    assert!(!allowed(&env, &user, Action::Read, &res).await);
}

#[tokio::test]
async fn an_event_published_right_after_spawning_is_not_missed() {
    let env = Env::new();
    let role = env
        .simple_role("temp", view("r", None), &[Action::Read])
        .await;
    let user = principal(&[(role, "temp")]);
    let res = resource("r", RepoFormat::Raw, "/x");
    assert!(allowed(&env, &user, Action::Read, &res).await);
    revoke(&env, role).await;
    // Spawn first, publish immediately, and only then let the task run: the
    // subscription was taken inside spawn_invalidation_listener, not on first poll.
    let (listener, cancel) = start_listener(&env);
    env.bus.publish(InvalidationEvent::Rbac).await.unwrap();
    settle().await;
    assert!(!allowed(&env, &user, Action::Read, &res).await);
    cancel.cancel();
    listener.join().await.unwrap();
}

#[tokio::test]
async fn the_listener_ends_when_cancelled_and_handles_repeated_cancellation() {
    let env = Env::new();
    let (listener, cancel) = start_listener(&env);
    settle().await;
    assert!(!listener.is_finished());
    cancel.cancel();
    cancel.cancel();
    listener.join().await.unwrap();
}

#[tokio::test]
async fn the_listener_ends_and_clears_the_caches_when_the_bus_goes_away() {
    let env = Env::new();
    let bus = MemInvalidationBus::new();
    let cancel = CancellationToken::new();
    let listener = env.rbac.spawn_invalidation_listener(&bus, cancel);
    let role = env
        .simple_role("temp", view("r", None), &[Action::Read])
        .await;
    let user = principal(&[(role, "temp")]);
    let res = resource("r", RepoFormat::Raw, "/x");
    assert!(allowed(&env, &user, Action::Read, &res).await);
    revoke(&env, role).await;
    drop(bus);
    listener.join().await.unwrap();
    assert!(
        !allowed(&env, &user, Action::Read, &res).await,
        "the closing stream resets the caches"
    );
}

#[tokio::test]
async fn dropping_the_handle_detaches_a_listener_that_still_obeys_cancellation() {
    let env = Env::new();
    let (listener, cancel) = start_listener(&env);
    drop(listener);
    let role = env
        .simple_role("temp", view("r", None), &[Action::Read])
        .await;
    let user = principal(&[(role, "temp")]);
    let res = resource("r", RepoFormat::Raw, "/x");
    assert!(allowed(&env, &user, Action::Read, &res).await);
    revoke(&env, role).await;
    settle().await;
    assert!(!allowed(&env, &user, Action::Read, &res).await);
    cancel.cancel();
    settle().await;
    // After cancellation the listener is gone, so the TTL is the only limit again.
    let role2 = env
        .simple_role("temp2", view("r2", None), &[Action::Read])
        .await;
    let user2 = principal(&[(role2, "temp2")]);
    let res2 = resource("r2", RepoFormat::Raw, "/x");
    assert!(allowed(&env, &user2, Action::Read, &res2).await);
    revoke(&env, role2).await;
    settle().await;
    assert!(allowed(&env, &user2, Action::Read, &res2).await);
}

#[tokio::test]
async fn selector_rows_are_cached_between_checks() {
    let env = Env::new();
    let selector_id = env.selector("any", "true").await;
    let role = env
        .simple_role("sel", selector_grant("*", selector_id), &[Action::Read])
        .await;
    let user = principal(&[(role, "sel")]);
    for i in 0..4 {
        let res = resource("r", RepoFormat::Raw, &format!("/x{i}"));
        assert!(allowed(&env, &user, Action::Read, &res).await);
    }
    assert_eq!(env.store.selector_reads(), 1);
}

#[tokio::test]
async fn a_selector_that_does_not_compile_is_cached_as_unusable() {
    let env = Env::new();
    let selector_id = env.selector("broken", "path ==").await;
    let role = env
        .simple_role("sel", selector_grant("*", selector_id), &[Action::Read])
        .await;
    let user = principal(&[(role, "sel")]);
    for _ in 0..3 {
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
    assert_eq!(env.store.selector_reads(), 1);
}
