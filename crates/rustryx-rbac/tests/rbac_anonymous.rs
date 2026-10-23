//! The two anonymous gates inside `RbacChecker::check` (CONTRIBUTING rule 4, core gap 13):
//! an anonymous caller needs the repository's own flag (`ResourceRef::allow_anonymous`) AND
//! the global switch. Neither gate depends on the roles, the caches or the store.

// Tests may unwrap and expect: a panic is the failure signal there.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::sync::Arc;

use common::{Env, builtin_principal, open_resource, resource, view};
use rustryx_core::identity::{
    Action, Principal, PrivilegeGrant, ROLE_NX_ANONYMOUS, ROLE_NX_DEVELOPER,
};
use rustryx_core::model::RepoFormat;
use rustryx_core::ports::auth::{Decision, RbacChecker, ResourceRef};
use rustryx_rbac::{Rbac, RbacConfig};

const ACTIONS: [Action; 4] = [Action::Browse, Action::Read, Action::Write, Action::Delete];

fn config(switch: bool) -> RbacConfig {
    RbacConfig {
        anonymous_enabled: switch,
        ..RbacConfig::default()
    }
}

async fn decide(env: &Env, p: &Principal, action: Action, r: &ResourceRef) -> Decision {
    env.rbac.check(p, action, r).await.unwrap()
}

#[tokio::test]
async fn must_not_allow_anonymous_without_both_switches() {
    // The built-in nx-anonymous grants browse and read on everything, so only the gates stand
    // between an anonymous caller and every repository.
    for (switch, flag, expected) in [
        (true, true, Decision::Allow),
        (true, false, Decision::Deny),
        (false, true, Decision::Deny),
        (false, false, Decision::Deny),
    ] {
        let env = Env::with_config(&config(switch));
        let mut res = resource("public", RepoFormat::Raw, "/x");
        res.allow_anonymous = flag;
        for action in [Action::Browse, Action::Read] {
            assert_eq!(
                decide(&env, &Principal::anonymous(), action, &res).await,
                expected,
                "switch {switch}, flag {flag}, {action:?}"
            );
        }
        for action in [Action::Write, Action::Delete] {
            assert_eq!(
                decide(&env, &Principal::anonymous(), action, &res).await,
                Decision::Deny,
                "switch {switch}, flag {flag}, {action:?}: anonymous never writes"
            );
        }
    }
}

#[tokio::test]
async fn must_not_reach_the_store_for_an_anonymous_caller_behind_a_closed_gate() {
    // A closed gate is decided from the request alone: an anonymous flood against a closed
    // repository must not cost a single store read, and a store outage must not turn the
    // refusal into an error.
    let env = Env::with_config(&config(false));
    env.store.inner.set_unavailable(true);
    for action in ACTIONS {
        assert_eq!(
            decide(
                &env,
                &Principal::anonymous(),
                action,
                &open_resource("r", RepoFormat::Raw, "/x")
            )
            .await,
            Decision::Deny
        );
    }
    assert_eq!(env.store.privilege_reads() + env.store.role_name_reads(), 0);
    let env = Env::new();
    env.store.inner.set_unavailable(true);
    for action in ACTIONS {
        assert_eq!(
            decide(
                &env,
                &Principal::anonymous(),
                action,
                &resource("r", RepoFormat::Raw, "/x")
            )
            .await,
            Decision::Deny,
            "the repository flag is closed"
        );
    }
    assert_eq!(env.store.privilege_reads() + env.store.role_name_reads(), 0);
}

#[tokio::test]
async fn must_not_allow_anonymous_with_the_default_config() {
    // `RbacConfig::default()` is what a composition root gets when it forgets the setting.
    let store = common::SpyStore::new(rustryx_testkit::MemStore::with_builtin_roles());
    let rbac = Rbac::new(
        Arc::new(store.clone()),
        Arc::new(store),
        &RbacConfig::default(),
    );
    assert!(!rbac.anonymous_switch().is_on());
    for action in ACTIONS {
        assert_eq!(
            rbac.check(
                &Principal::anonymous(),
                action,
                &open_resource("r", RepoFormat::Raw, "/x")
            )
            .await
            .unwrap(),
            Decision::Deny
        );
    }
}

#[tokio::test]
async fn the_switch_handle_takes_effect_on_the_next_request() {
    let env = Env::with_config(&config(false));
    let switch = env.rbac.anonymous_switch();
    let anonymous = Principal::anonymous();
    let res = open_resource("public", RepoFormat::Raw, "/x");
    assert_eq!(
        decide(&env, &anonymous, Action::Read, &res).await,
        Decision::Deny
    );
    switch.set(true);
    assert_eq!(
        decide(&env, &anonymous, Action::Read, &res).await,
        Decision::Allow
    );
    // The privileges of nx-anonymous are cached now; turning the switch off must still win.
    switch.set(false);
    assert_eq!(
        decide(&env, &anonymous, Action::Read, &res).await,
        Decision::Deny
    );
    assert!(
        !env.rbac
            .can_see_repository(&anonymous, "public", RepoFormat::Raw)
            .await
            .unwrap()
    );
    switch.set(true);
    assert!(
        env.rbac
            .can_see_repository(&anonymous, "public", RepoFormat::Raw)
            .await
            .unwrap()
    );
}

#[tokio::test]
async fn clones_of_the_authorizer_and_of_the_handle_share_one_switch() {
    let env = Env::with_config(&config(false));
    let clone = env.rbac.clone();
    let handle = clone.anonymous_switch();
    let other_handle = env.rbac.anonymous_switch();
    handle.set(true);
    assert!(other_handle.is_on());
    assert!(env.rbac.anonymous_switch().is_on());
    let res = open_resource("public", RepoFormat::Raw, "/x");
    assert_eq!(
        env.rbac
            .check(&Principal::anonymous(), Action::Read, &res)
            .await
            .unwrap(),
        Decision::Allow
    );
}

#[tokio::test]
async fn the_gates_do_not_apply_to_authenticated_callers() {
    // Closed repository, switch off: a user with rights still gets in. The gates are about
    // callers that did not identify themselves.
    let env = Env::with_config(&config(false));
    let dev = builtin_principal(ROLE_NX_DEVELOPER);
    let res = resource("private", RepoFormat::Raw, "/x");
    assert!(!res.allow_anonymous);
    for action in [Action::Browse, Action::Read, Action::Write] {
        assert_eq!(decide(&env, &dev, action, &res).await, Decision::Allow);
    }
}

#[tokio::test]
async fn must_not_let_the_gates_depend_on_a_cached_decision() {
    // The same anonymous principal, the same cached nx-anonymous privileges, two resources
    // that differ only in the flag.
    let env = Env::new();
    let anonymous = Principal::anonymous();
    let open = open_resource("r", RepoFormat::Raw, "/x");
    let closed = resource("r", RepoFormat::Raw, "/x");
    for _ in 0..3 {
        assert_eq!(
            decide(&env, &anonymous, Action::Read, &open).await,
            Decision::Allow
        );
        assert_eq!(
            decide(&env, &anonymous, Action::Read, &closed).await,
            Decision::Deny
        );
    }
}

#[tokio::test]
async fn must_not_let_a_forged_role_or_an_admin_privilege_open_a_closed_gate() {
    // nx-anonymous with every privilege, the Admin one included, and a principal that carries
    // forged admin roles: still refused behind a closed gate.
    let env = Env::bare_with_config(&config(true));
    let everything = env
        .privilege("anon-everything", view("*", None), &ACTIONS)
        .await;
    let admin = env
        .privilege("anon-admin", PrivilegeGrant::Admin, &ACTIONS)
        .await;
    env.role(ROLE_NX_ANONYMOUS, &[everything, admin], &[]).await;
    let mut forged = Principal::anonymous();
    forged.roles = builtin_principal("nx-admin").roles;
    let closed = resource("r", RepoFormat::Raw, "/x");
    for action in ACTIONS {
        assert_eq!(decide(&env, &forged, action, &closed).await, Decision::Deny);
    }
    for action in ACTIONS {
        assert_eq!(
            decide(&env, &Principal::anonymous(), action, &closed).await,
            Decision::Deny
        );
    }
    // The same role set is not wider than the gate for the repository that opted in either:
    // write and delete stay refused.
    let open = open_resource("r", RepoFormat::Raw, "/x");
    assert_eq!(
        decide(&env, &forged, Action::Read, &open).await,
        Decision::Allow
    );
    for action in [Action::Write, Action::Delete] {
        assert_eq!(decide(&env, &forged, action, &open).await, Decision::Deny);
    }
}

#[tokio::test]
async fn anonymous_visibility_follows_the_switch_and_the_privileges() {
    let env = Env::with_config(&config(true));
    let anonymous = Principal::anonymous();
    // The signature carries no repository flag, so with the switch on the privileges decide.
    assert!(
        env.rbac
            .can_see_repository(&anonymous, "anything", RepoFormat::Raw)
            .await
            .unwrap()
    );
    let off = Env::with_config(&config(false));
    off.store.inner.set_unavailable(true);
    assert!(
        !off.rbac
            .can_see_repository(&anonymous, "anything", RepoFormat::Raw)
            .await
            .unwrap(),
        "switch off: nothing is visible, and no store access is needed to say so"
    );
    assert_eq!(off.store.role_name_reads(), 0);
}
