//! The group confirmation window (README 8.5: at most five minutes).
//!
//! Every age decision is made from the time the PROVIDER confirmed the groups
//! (`ExternalGroups::confirmed_at`), never from the time the chain received the answer. The
//! tests drive the chain with a scripted resolver that serves sets of any age, which is what
//! a 60 second resolver cache does in production, and prove that the layers cannot add up.

use std::time::Duration;

use rustryx_core::error::AuthError;
use rustryx_core::identity::IdpProvider;

use super::testing::{Answer, Harness, harness, names, oidc_token};

const READERS_DN: &str = "cn=readers,ou=groups,dc=example,dc=test";
const WINDOW: Duration = Duration::from_mins(5);

fn secs(n: u64) -> Duration {
    Duration::from_secs(n)
}

async fn ldap_env() -> Harness {
    let h = harness();
    h.add_ldap_user("bob", "ldap secret");
    let readers = h.add_role("readers").await;
    h.map_group(IdpProvider::Ldap, READERS_DN, &readers).await;
    h
}

fn confirmed(age: Duration) -> Answer {
    Answer::GroupsConfirmed {
        groups: vec![READERS_DN.into()],
        age,
    }
}

#[tokio::test]
async fn the_principal_carries_the_time_the_directory_confirmed_its_groups() {
    let h = ldap_env().await;
    h.ldap_resolver.set(confirmed(secs(40)));
    let principal = h.basic("bob", "ldap secret").await.unwrap();
    assert_eq!(
        principal.groups_confirmed_at,
        Some(h.clock_now() - time::Duration::seconds(40)),
        "the chain must not stamp its own `now` on an answer the directory gave earlier"
    );
    assert_eq!(names(&principal), ["readers"]);
}

#[tokio::test]
async fn must_not_let_the_resolver_cache_and_the_chain_grace_add_up_beyond_five_minutes() {
    let h = ldap_env().await;
    // The resolver serves a set the directory confirmed 4 minutes ago.
    h.ldap_resolver.set(confirmed(Duration::from_mins(4)));
    h.basic("bob", "ldap secret").await.unwrap();

    // The directory is down from now on. The set is 4:00 old and may be reused for one more
    // minute, counted from the DIRECTORY's answer: 4:59 and 5:00 are inside the window.
    h.ldap_resolver.set(Answer::Fail);
    h.clock.advance(secs(59));
    assert_eq!(
        names(&h.basic("bob", "ldap secret").await.unwrap()),
        ["readers"]
    );
    h.clock.advance(secs(1));
    assert_eq!(
        names(&h.basic("bob", "ldap secret").await.unwrap()),
        ["readers"],
        "at most five minutes"
    );

    // 5:01 since the directory spoke: refused, although the chain itself only learned the set
    // a minute ago. With the old "stamp now on receipt" accounting this request still passed.
    h.clock.advance(secs(1));
    let error = h.basic("bob", "ldap secret").await.unwrap_err();
    assert!(
        matches!(error, AuthError::GroupLookupFailed(_)),
        "{error:?}"
    );
}

#[tokio::test]
async fn must_not_serve_an_ldap_answer_older_than_five_minutes_from_a_working_resolver() {
    for (age, served) in [
        (secs(299), true),
        (WINDOW, true),
        (WINDOW + secs(1), false),
        (Duration::from_hours(1), false),
    ] {
        let h = ldap_env().await;
        h.ldap_resolver.set(confirmed(age));
        let outcome = h.basic("bob", "ldap secret").await;
        assert_eq!(outcome.is_ok(), served, "age {age:?}: {outcome:?}");
        if !served {
            let error = outcome.unwrap_err();
            assert!(
                matches!(error, AuthError::GroupLookupFailed(_)),
                "{error:?}"
            );
        }
    }
}

#[tokio::test]
async fn must_not_accept_a_confirmation_from_the_future_beyond_the_clock_skew() {
    for (ahead, served) in [(secs(0), true), (secs(5), true), (secs(6), false)] {
        let h = ldap_env().await;
        h.ldap_resolver.set(Answer::GroupsFromTheFuture {
            groups: vec![READERS_DN.into()],
            ahead,
        });
        let outcome = h.basic("bob", "ldap secret").await;
        assert_eq!(outcome.is_ok(), served, "ahead {ahead:?}: {outcome:?}");
    }
}

#[tokio::test]
async fn an_older_answer_never_replaces_a_newer_remembered_one() {
    let h = ldap_env().await;
    // Confirmed at -10 s.
    h.ldap_resolver.set(confirmed(secs(10)));
    h.basic("bob", "ldap secret").await.unwrap();
    // Twenty seconds later the resolver serves a set confirmed at -40 s (older).
    h.clock.advance(secs(20));
    h.ldap_resolver.set(confirmed(secs(60)));
    h.basic("bob", "ldap secret").await.unwrap();

    // At t = 280 s the newer set (-10 s) is 290 s old and usable; the older one (-40 s) would
    // be 320 s old and refused. The directory is down, so the remembered set decides.
    h.clock.advance(secs(260));
    h.ldap_resolver.set(Answer::Fail);
    assert_eq!(
        names(&h.basic("bob", "ldap secret").await.unwrap()),
        ["readers"],
        "the newest confirmation must be the one that is remembered"
    );
}

#[tokio::test]
async fn an_overloaded_resolver_is_reported_not_papered_over_by_an_earlier_result() {
    let h = ldap_env().await;
    h.ldap_resolver.set(confirmed(secs(1)));
    h.basic("bob", "ldap secret").await.unwrap();
    h.ldap_resolver.set(Answer::Overloaded);
    let error = h.basic("bob", "ldap secret").await.unwrap_err();
    assert!(matches!(error, AuthError::Overloaded), "{error:?}");
}

#[tokio::test]
async fn must_not_cache_a_session_principal_of_an_ldap_user_beyond_the_directory_window() {
    let h = ldap_env().await;
    h.ldap_resolver.set(confirmed(secs(60)));
    let user = h.basic("bob", "ldap secret").await.unwrap();
    let user = rustryx_core::ports::UserRepo::get(&h.store, user.user_id.unwrap())
        .await
        .unwrap()
        .unwrap();
    let token = h.issue_jwt(&user, Duration::from_hours(1));
    h.ldap_resolver.set(confirmed(Duration::from_mins(4)));
    let calls = h.ldap_resolver.calls();
    h.bearer(&token).await.unwrap();
    assert_eq!(h.ldap_resolver.calls(), calls + 1);

    // The set was confirmed 4:00 ago, so the cached principal may live one more minute,
    // not the five minutes of a fresh entry.
    h.clock.advance(secs(30));
    h.bearer(&token).await.unwrap();
    assert_eq!(
        h.ldap_resolver.calls(),
        calls + 1,
        "still served from memory"
    );
    h.clock.advance(secs(31));
    h.bearer(&token).await.unwrap();
    assert_eq!(
        h.ldap_resolver.calls(),
        calls + 2,
        "the entry ended with the confirmation window, so the resolver was asked again"
    );
}

#[tokio::test]
async fn must_not_take_oidc_groups_older_than_the_window_from_the_resolver() {
    let h = harness();
    let admin_group = "/rustryx/admins".to_owned();
    h.oidc_resolver.set(Answer::GroupsConfirmed {
        groups: vec![admin_group.clone()],
        age: WINDOW + secs(1),
    });
    let token = oidc_token("carol", "sub-c", &[], h.exp_in(300));
    let error = h.bearer(&token).await.unwrap_err();
    assert!(
        matches!(error, AuthError::GroupLookupFailed(_)),
        "{error:?}"
    );

    h.oidc_resolver.set(Answer::GroupsConfirmed {
        groups: vec![admin_group],
        age: secs(30),
    });
    let principal = h.bearer(&token).await.unwrap();
    assert_eq!(
        principal.groups_confirmed_at,
        Some(h.clock_now() - time::Duration::seconds(30)),
        "the resolver's confirmation time, not the receipt time"
    );
}
