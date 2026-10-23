//! Chain tests for Basic credentials: local passwords, LDAP, API tokens, the guard
//! and the dummy hash.

use std::net::{IpAddr, Ipv4Addr};
use std::time::Duration;

use rustryx_core::Secret;
use rustryx_core::error::AuthError;
use rustryx_core::identity::{AuthMethod, Credentials, UserSource};
use rustryx_core::ports::{LoginAttemptRepo, LoginKey, UserRepo};
use rustryx_testkit::fixtures;

use super::testing::{Options, basic, harness, harness_with, names};
use crate::passwords::{GateClass, GateConfig, LaneLimits};
use crate::tokens::{TokenSpec, create_token};

const IP: IpAddr = IpAddr::V4(Ipv4Addr::new(203, 0, 113, 9));

fn held_gate() -> GateConfig {
    GateConfig {
        password: LaneLimits {
            permits: 1,
            max_waiting: 0,
        },
        general: LaneLimits {
            permits: 1,
            max_waiting: 0,
        },
        permit_wait: Duration::from_millis(1),
    }
}

async fn failures(h: &super::testing::Harness, account: &str, ip: Option<IpAddr>) -> u32 {
    let key = LoginKey {
        account: account.to_owned(),
        ip,
    };
    h.guard_store.state(&key).await.unwrap().failures
}

#[tokio::test]
async fn anonymous_credentials_yield_the_anonymous_principal() {
    let h = harness();
    let principal = h.authenticate(&Credentials::Anonymous).await.unwrap();
    assert!(principal.is_anonymous());
    assert_eq!(principal.roles.len(), 0);
}

#[tokio::test]
async fn local_login_returns_manual_roles() {
    let h = harness();
    let alice = h.add_local_user("alice", "correct horse").await;
    let developer = h.add_role("team-dev").await;
    h.grant_manual(&alice, &developer).await;
    let principal = h.basic("alice", "correct horse").await.unwrap();
    assert_eq!(principal.user_id, Some(alice.id));
    assert_eq!(principal.auth_method, AuthMethod::BasicLocal);
    assert_eq!(names(&principal), ["team-dev"]);
    assert_eq!(principal.external_groups.len(), 0);
}

#[tokio::test]
async fn wrong_password_fails_and_is_counted_per_account_and_per_pair() {
    let h = harness();
    h.add_local_user("alice", "correct horse").await;
    let error = h
        .authenticate_from(&basic("alice", "nope nope"), Some(IP))
        .await
        .unwrap_err();
    assert!(matches!(error, AuthError::InvalidCredentials));
    assert_eq!(failures(&h, "alice", None).await, 1);
    assert_eq!(failures(&h, "alice", Some(IP)).await, 1);
}

#[tokio::test]
async fn unknown_account_fails_like_a_wrong_password_and_is_counted_too() {
    let h = harness();
    let error = h.basic("ghost", "whatever").await.unwrap_err();
    assert!(matches!(error, AuthError::InvalidCredentials));
    assert_eq!(failures(&h, "ghost", None).await, 1);
}

#[tokio::test]
async fn must_pay_the_hash_cost_for_an_unknown_account() {
    // With the only password permit held, anything that needs a hash is Overloaded.
    // An unknown account reaching Overloaded proves the dummy verification runs, under
    // the same semaphore as a real one.
    let h = harness_with(Options {
        gate: Some(held_gate()),
        // With LDAP configured an unknown name is the directory's to refuse, and the
        // bind costs what the dummy hash would; this test is about the local path.
        with_ldap: false,
        ..Options::default()
    });
    h.add_local_user("alice", "correct horse").await;
    let _held = h.gate.acquire(GateClass::Password).await.unwrap();
    for (user, password) in [("ghost", "whatever"), ("alice", "wrong horse")] {
        let error = h.basic(user, password).await.unwrap_err();
        assert!(matches!(error, AuthError::Overloaded), "{user}: {error:?}");
    }
    assert_eq!(
        failures(&h, "ghost", None).await,
        0,
        "an overload is not a failed guess"
    );
}

#[tokio::test]
async fn must_not_reach_a_hash_with_a_blank_password() {
    let h = harness_with(Options {
        gate: Some(held_gate()),
        ..Options::default()
    });
    h.add_local_user("alice", "correct horse").await;
    let _held = h.gate.acquire(GateClass::Password).await.unwrap();
    for blank in ["", " ", "\t \n"] {
        let error = h.basic("alice", blank).await.unwrap_err();
        assert!(
            matches!(error, AuthError::InvalidCredentials),
            "{blank:?}: {error:?}"
        );
        let error = h.basic("ghost", blank).await.unwrap_err();
        assert!(
            matches!(error, AuthError::InvalidCredentials),
            "{blank:?}: {error:?}"
        );
    }
    let error = h.basic("", "correct horse").await.unwrap_err();
    assert!(matches!(error, AuthError::InvalidCredentials));
    assert_eq!(
        h.ldap_auth.calls(),
        0,
        "a blank credential must not reach LDAP either"
    );
}

#[tokio::test]
async fn five_failures_lock_even_the_right_password_and_unknown_names_alike() {
    let h = harness();
    h.add_local_user("alice", "correct horse").await;
    for name in ["alice", "ghost"] {
        for _ in 0..5 {
            let error = h.basic(name, "wrong").await.unwrap_err();
            assert!(matches!(error, AuthError::InvalidCredentials));
        }
    }
    let known = h.basic("alice", "correct horse").await.unwrap_err();
    let unknown = h.basic("ghost", "correct horse").await.unwrap_err();
    for error in [known, unknown] {
        let AuthError::LockedOut { retry_after } = error else {
            unreachable!("expected a lock, got {error:?}");
        };
        assert_eq!(retry_after, Duration::from_secs(60));
    }
    h.clock.advance(Duration::from_secs(61));
    h.basic("alice", "correct horse").await.unwrap();
}

#[tokio::test]
async fn success_clears_the_counters() {
    let h = harness();
    h.add_local_user("alice", "correct horse").await;
    for _ in 0..3 {
        h.basic("alice", "wrong").await.unwrap_err();
    }
    h.basic("alice", "correct horse").await.unwrap();
    assert_eq!(failures(&h, "alice", None).await, 0);
    for _ in 0..4 {
        h.basic("alice", "wrong").await.unwrap_err();
    }
    h.basic("alice", "correct horse").await.unwrap();
}

#[tokio::test]
async fn must_not_let_case_changes_dodge_the_lockout() {
    let h = harness();
    h.add_local_user("alice", "correct horse").await;
    for spelling in ["alice", "ALICE", "Alice", "aLICE", "alICE"] {
        h.basic(spelling, "wrong").await.unwrap_err();
    }
    let error = h.basic("alice", "correct horse").await.unwrap_err();
    assert!(matches!(error, AuthError::LockedOut { .. }), "{error:?}");
}

#[tokio::test]
async fn must_fail_closed_when_the_counter_store_is_down() {
    let h = harness_with(Options {
        separate_guard_store: true,
        ..Options::default()
    });
    h.add_local_user("alice", "correct horse").await;
    h.basic("alice", "correct horse").await.unwrap();
    h.guard_store.set_unavailable(true);
    let right = h.basic("alice", "correct horse").await.unwrap_err();
    assert!(matches!(right, AuthError::GuardUnavailable), "{right:?}");
    let wrong = h.basic("alice", "wrong").await.unwrap_err();
    assert!(matches!(wrong, AuthError::GuardUnavailable), "{wrong:?}");
    // LDAP password logins fail closed too.
    h.add_ldap_user("bob", "ldap secret");
    let ldap = h.basic("bob", "ldap secret").await.unwrap_err();
    assert!(matches!(ldap, AuthError::GuardUnavailable), "{ldap:?}");
    assert_eq!(h.ldap_auth.calls(), 0, "the directory is not even asked");
    h.guard_store.set_unavailable(false);
    h.basic("alice", "correct horse").await.unwrap();
}

#[tokio::test]
async fn overload_and_provider_outage_are_not_counted_as_failures() {
    let h = harness();
    h.add_local_user("alice", "correct horse").await;
    h.store.set_unavailable(true);
    let error = h.basic("alice", "correct horse").await.unwrap_err();
    assert!(matches!(
        error,
        AuthError::GuardUnavailable | AuthError::ProviderUnavailable(_)
    ));
    h.store.set_unavailable(false);
    assert_eq!(failures(&h, "alice", None).await, 0);
}

#[tokio::test]
async fn disabled_account_with_the_right_password_is_reported_and_not_counted() {
    let h = harness();
    let user = h.add_local_user("alice", "correct horse").await;
    UserRepo::set_enabled(&h.store, user.id, false)
        .await
        .unwrap();
    let error = h.basic("alice", "correct horse").await.unwrap_err();
    assert!(matches!(error, AuthError::AccountDisabled), "{error:?}");
    assert_eq!(failures(&h, "alice", None).await, 0);
}

#[tokio::test]
async fn ldap_login_provisions_the_account_and_maps_groups() {
    let h = harness();
    h.add_ldap_user("bob", "ldap secret");
    let readers = h.add_role("readers").await;
    h.map_group(
        rustryx_core::identity::IdpProvider::Ldap,
        "cn=readers,ou=groups,dc=example,dc=test",
        &readers,
    )
    .await;
    h.ldap_resolver.set(super::testing::Answer::Groups(vec![
        "cn=readers,ou=groups,dc=example,dc=test".into(),
    ]));
    let principal = h.basic("bob", "ldap secret").await.unwrap();
    assert_eq!(principal.source, UserSource::Ldap);
    assert_eq!(principal.auth_method, AuthMethod::BasicLdap);
    assert_eq!(names(&principal), ["readers"]);
    assert_eq!(
        principal.external_groups,
        ["cn=readers,ou=groups,dc=example,dc=test"]
    );
    let stored = UserRepo::find_by_username(&h.store, "bob")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stored.source, UserSource::Ldap);
    assert_eq!(principal.user_id, Some(stored.id));
    assert!(principal.groups_confirmed_at.is_some());
}

#[tokio::test]
async fn wrong_ldap_password_is_counted_like_any_other() {
    let h = harness();
    h.add_ldap_user("bob", "ldap secret");
    for _ in 0..5 {
        let error = h.basic("bob", "wrong").await.unwrap_err();
        assert!(matches!(error, AuthError::InvalidCredentials));
    }
    let error = h.basic("bob", "ldap secret").await.unwrap_err();
    assert!(matches!(error, AuthError::LockedOut { .. }), "{error:?}");
}

#[tokio::test]
async fn must_not_try_ldap_after_a_local_account_refused_the_password() {
    let h = harness();
    h.add_local_user("alice", "local secret").await;
    h.add_ldap_user("alice", "ldap secret");
    let wrong_local = h.basic("alice", "ldap secret").await.unwrap_err();
    assert!(matches!(wrong_local, AuthError::InvalidCredentials));
    assert_eq!(
        h.ldap_auth.calls(),
        0,
        "the local account decides, LDAP is not consulted"
    );
}

#[tokio::test]
async fn account_without_any_authenticator_falls_back_to_the_dummy_and_fails() {
    let h = harness_with(Options {
        with_ldap: false,
        ..Options::default()
    });
    let error = h.basic("ghost", "whatever").await.unwrap_err();
    assert!(matches!(error, AuthError::InvalidCredentials));
    assert_eq!(failures(&h, "ghost", None).await, 1);
}

#[tokio::test]
async fn api_token_as_basic_password_authenticates_without_the_guard() {
    let h = harness();
    let alice = h.add_local_user("alice", "correct horse").await;
    let issued = create_token(&h.store, &h.clock, alice.id, TokenSpec::new("ci"))
        .await
        .unwrap();
    let principal = h
        .authenticate(&Credentials::Basic {
            username: "__token__".into(),
            password: issued.secret.clone(),
        })
        .await
        .unwrap();
    assert_eq!(principal.auth_method, AuthMethod::ApiToken);
    assert_eq!(principal.token_id, Some(issued.record.id));
    assert_eq!(principal.user_id, Some(alice.id));
}

#[tokio::test]
async fn must_not_count_bad_token_attempts_against_the_account() {
    let h = harness();
    h.add_local_user("alice", "correct horse").await;
    let bogus = crate::tokens::generate_token_secret();
    for _ in 0..12 {
        let error = h
            .authenticate(&Credentials::Basic {
                username: "alice".into(),
                password: bogus.clone(),
            })
            .await
            .unwrap_err();
        assert!(matches!(error, AuthError::InvalidCredentials), "{error:?}");
    }
    assert_eq!(failures(&h, "alice", None).await, 0);
    h.basic("alice", "correct horse").await.unwrap();
}

#[tokio::test]
async fn must_not_treat_a_ryx_prefixed_password_as_a_local_password() {
    let h = harness();
    // A local password that happens to start with the token prefix is not honoured on
    // Basic: the prefix is the token namespace.
    h.add_local_user("alice", "ryx_looks-like-a-token").await;
    let error = h
        .basic("alice", "ryx_looks-like-a-token")
        .await
        .unwrap_err();
    assert!(matches!(error, AuthError::InvalidCredentials), "{error:?}");
}

#[tokio::test]
async fn external_account_cannot_log_in_with_a_planted_local_hash() {
    let h = harness_with(Options {
        with_ldap: false,
        ..Options::default()
    });
    let mut new = fixtures::user("carol", UserSource::Oidc);
    new.password_hash = Some(
        h.passwords
            .hash(&Secret::new("planted pass"))
            .await
            .unwrap(),
    );
    UserRepo::create(&h.store, &new).await.unwrap();
    let error = h.basic("carol", "planted pass").await.unwrap_err();
    assert!(matches!(error, AuthError::InvalidCredentials), "{error:?}");
}
