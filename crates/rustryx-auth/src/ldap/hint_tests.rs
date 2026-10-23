//! The user DN as a hint for the group refresh (`Principal::external_id`), the confirmation
//! time the resolver reports, the request deadline and the `ldap_requests_total` series.

use std::time::Duration;

use rustryx_core::AuthError;
use rustryx_core::identity::{AuthContext, IdpProvider, Principal, UserSource};
use rustryx_core::ports::{Authenticator, GroupResolver};
use time::Duration as TimeDuration;
use tokio::time::Instant;

use super::config::{LdapConfig, LdapGroupStrategy};
use super::testing::{
    GROUPS_DN, Harness, PEOPLE_DN, T0, base_config, basic, group_dn, ldap_principal,
    standard_directory, user_dn,
};
use super::transport::{LdapEntry, SearchScope, TransportError};
use crate::telemetry::recording::Capture;

fn secs(n: i64) -> TimeDuration {
    TimeDuration::seconds(n)
}

fn alice_groups() -> Vec<String> {
    vec![group_dn("devs"), group_dn("admins")]
}

/// The user searches of the wire log: the ones that look for a user, not for groups.
fn user_searches(harness: &Harness) -> Vec<(SearchScope, String)> {
    harness
        .fake
        .searches()
        .into_iter()
        .filter(|(_, request)| request.filter.starts_with("(uid="))
        .map(|(_, request)| (request.scope, request.base))
        .collect()
}

#[tokio::test]
async fn the_login_principal_carries_the_user_dn_as_its_external_id() {
    let harness = Harness::standard();
    let principal = harness.login("alice", "alice-pw").await.unwrap().unwrap();
    assert_eq!(
        principal.external_id.as_deref(),
        Some(user_dn("alice").as_str())
    );
    assert_eq!(principal.groups_confirmed_at, Some(T0));
}

#[tokio::test]
async fn a_refresh_reads_the_entry_at_the_remembered_dn_instead_of_searching_for_the_user() {
    let harness = Harness::standard();
    harness.login("alice", "alice-pw").await.unwrap();
    harness.clock.advance(secs(61));
    harness.fake.clear_events();

    let external = harness
        .resolver
        .external_groups(&ldap_principal("alice"))
        .await
        .unwrap();
    assert_eq!(external.groups, alice_groups());
    assert_eq!(external.confirmed_at, T0 + secs(61), "a fresh answer");
    assert_eq!(
        user_searches(&harness),
        vec![(SearchScope::Base, user_dn("alice"))],
        "one read of the known entry and no subtree search"
    );
}

#[tokio::test]
async fn the_dn_of_the_principal_is_used_when_nothing_is_remembered() {
    let harness = Harness::standard();
    let mut principal = ldap_principal("alice");
    principal.external_id = Some(user_dn("alice"));
    let external = harness.resolver.external_groups(&principal).await.unwrap();
    assert_eq!(external.groups, alice_groups());
    assert_eq!(
        user_searches(&harness),
        vec![(SearchScope::Base, user_dn("alice"))]
    );
}

#[tokio::test]
async fn must_not_let_a_dn_hint_return_the_groups_of_another_user() {
    // bob is only in `admins`, alice in both. A hint that points at bob's entry, asked for
    // alice, meets the user filter `(uid=alice)` there and finds nothing: the hint decides
    // where to look, never who the user is.
    let harness = Harness::standard();
    let mut forged = ldap_principal("alice");
    forged.external_id = Some(user_dn("bob"));
    let external = harness.resolver.external_groups(&forged).await.unwrap();
    assert_eq!(external.groups, alice_groups());
    assert_eq!(
        user_searches(&harness),
        vec![
            (SearchScope::Base, user_dn("bob")),
            (SearchScope::Subtree, PEOPLE_DN.to_owned()),
        ],
        "the hint was tried, refused by the filter, and the subtree search decided"
    );

    // And the other way round: nothing of bob's leaks into the cache entry of alice.
    let again = harness
        .resolver
        .external_groups(&ldap_principal("bob"))
        .await
        .unwrap();
    assert_eq!(again.groups, vec![group_dn("admins")]);
}

#[tokio::test]
async fn a_hint_to_an_entry_that_does_not_exist_falls_back_to_the_subtree_search() {
    let harness = Harness::standard();
    let mut principal = ldap_principal("alice");
    principal.external_id = Some(user_dn("nobody"));
    let external = harness.resolver.external_groups(&principal).await.unwrap();
    assert_eq!(external.groups, alice_groups());
}

#[tokio::test]
async fn must_not_keep_granting_groups_once_the_filter_no_longer_matches_the_entry() {
    // The directory marks the account as no longer eligible (here: the attribute the filter
    // matches is gone), exactly what a disabled account looks like to the user filter.
    let harness = Harness::standard();
    harness.login("alice", "alice-pw").await.unwrap();
    harness.fake.update_directory(|directory| {
        directory.remove_entry(&user_dn("alice"));
        *directory = directory
            .clone()
            .entry(LdapEntry::new(user_dn("alice")).with_attribute("uid", ["alice-disabled"]));
    });
    harness.clock.advance(secs(61));
    let error = harness
        .resolver
        .external_groups(&ldap_principal("alice"))
        .await
        .unwrap_err();
    assert!(
        matches!(error, AuthError::GroupLookupFailed(_)),
        "{error:?}"
    );
    assert!(error.to_string().contains("no longer in the directory"));
}

#[tokio::test]
async fn a_moved_user_is_found_again_and_the_new_dn_is_remembered() {
    let harness = Harness::standard();
    harness.login("alice", "alice-pw").await.unwrap();
    let moved = format!("uid=alice,ou=staff,{PEOPLE_DN}");
    harness.fake.update_directory(|directory| {
        directory.remove_entry(&user_dn("alice"));
        *directory = directory
            .clone()
            .entry(LdapEntry::new(moved.clone()).with_attribute("uid", ["alice"]));
    });
    harness.clock.advance(secs(61));
    harness.fake.clear_events();
    harness
        .resolver
        .external_groups(&ldap_principal("alice"))
        .await
        .unwrap();
    assert_eq!(
        user_searches(&harness),
        vec![
            (SearchScope::Base, user_dn("alice")),
            (SearchScope::Subtree, PEOPLE_DN.to_owned()),
        ]
    );

    harness.clock.advance(secs(61));
    harness.fake.clear_events();
    harness
        .resolver
        .external_groups(&ldap_principal("alice"))
        .await
        .unwrap();
    assert_eq!(
        user_searches(&harness),
        vec![(SearchScope::Base, moved)],
        "the next refresh goes straight to the new entry"
    );
}

#[tokio::test]
async fn a_failing_read_of_the_remembered_entry_is_a_failed_lookup_not_a_gone_user() {
    let harness = Harness::standard();
    harness.login("alice", "alice-pw").await.unwrap();
    harness.clock.advance(secs(61));
    // Anything but "no such object" on the base read is the directory failing.
    harness
        .fake
        .knobs()
        .search_error_for_base
        .push((user_dn("alice"), 51));
    let error = harness
        .resolver
        .external_groups(&ldap_principal("alice"))
        .await
        .unwrap_err();
    assert!(
        matches!(error, AuthError::GroupLookupFailed(_)),
        "{error:?}"
    );
    assert!(
        !error.to_string().contains("no longer in the directory"),
        "{error}"
    );
}

#[tokio::test]
async fn reverse_member_refreshes_use_the_hint_too() {
    let mut config = base_config();
    config.group_strategy = LdapGroupStrategy::ReverseMember;
    config.group_base_dn = Some(GROUPS_DN.into());
    let harness = Harness::new(&config, standard_directory());
    harness.login("alice", "alice-pw").await.unwrap();
    harness.clock.advance(secs(61));
    harness.fake.clear_events();
    let external = harness
        .resolver
        .external_groups(&ldap_principal("alice"))
        .await
        .unwrap();
    assert_eq!(external.groups.len(), 2);
    assert_eq!(
        user_searches(&harness),
        vec![(SearchScope::Base, user_dn("alice"))]
    );
}

#[tokio::test]
async fn the_confirmation_time_is_the_directorys_answer_also_for_a_cached_set() {
    let harness = Harness::standard();
    let first = harness
        .resolver
        .external_groups(&ldap_principal("alice"))
        .await
        .unwrap();
    assert_eq!(first.confirmed_at, T0);
    harness.clock.advance(secs(30));
    let cached = harness
        .resolver
        .external_groups(&ldap_principal("alice"))
        .await
        .unwrap();
    assert_eq!(cached.groups, first.groups);
    assert_eq!(
        cached.confirmed_at, T0,
        "a cached set is as old as the directory's answer, not as new as the request"
    );
}

#[tokio::test]
async fn a_stale_set_is_reported_with_the_time_of_the_last_search() {
    let mut config = base_config();
    config.groups_stale_on_error = true;
    let harness = Harness::new(&config, standard_directory());
    harness
        .resolver
        .external_groups(&ldap_principal("alice"))
        .await
        .unwrap();
    harness.clock.advance(secs(120));
    harness.fake.knobs().search_error = Some(TransportError::Connect("down".into()));
    harness.fake.knobs().fail_connect = true;
    let stale = harness
        .resolver
        .external_groups(&ldap_principal("alice"))
        .await
        .unwrap();
    assert_eq!(stale.confirmed_at, T0, "the chain can age it exactly");
}

#[tokio::test]
async fn a_principal_of_another_source_has_no_ldap_groups_as_of_now() {
    let harness = Harness::standard();
    harness.clock.advance(secs(7));
    let local = Principal::new(
        "alice",
        UserSource::Local,
        rustryx_core::identity::AuthMethod::BasicLocal,
    );
    let external = harness.resolver.external_groups(&local).await.unwrap();
    assert_eq!(external.provider, IdpProvider::Ldap);
    assert_eq!(external.groups, Vec::<String>::new());
    assert_eq!(external.confirmed_at, T0 + secs(7));
}

#[tokio::test(start_paused = true)]
async fn must_not_outlive_the_request_deadline() {
    let mut config: LdapConfig = base_config();
    config.auth_deadline_secs = 10;
    let harness = Harness::new(&config, standard_directory());
    harness.fake.knobs().user_bind_delay = Some(Duration::from_secs(8));
    let started = Instant::now();
    let context =
        AuthContext::new().with_deadline(std::time::Instant::now() + Duration::from_secs(2));
    let error = harness
        .auth
        .authenticate(&basic("alice", "alice-pw"), &context)
        .await
        .unwrap_err();
    // The deadline is a std instant taken a moment after the paused clock froze, so the
    // elapsed virtual time is two seconds plus the real microseconds in between.
    let elapsed = started.elapsed();
    assert!(
        elapsed >= Duration::from_secs(2) && elapsed < Duration::from_millis(2100),
        "the request deadline is shorter than the module's own and wins: {elapsed:?}"
    );
    assert_eq!(
        error.to_string(),
        "identity provider unavailable: ldap authentication deadline exceeded"
    );

    // A deadline further away than the module's own bounds changes nothing: here the bind
    // timeout of five seconds ends the attempt.
    harness.fake.knobs().user_bind_delay = Some(Duration::from_secs(30));
    let started = Instant::now();
    let context =
        AuthContext::new().with_deadline(std::time::Instant::now() + Duration::from_secs(600));
    harness
        .auth
        .authenticate(&basic("alice", "alice-pw"), &context)
        .await
        .unwrap_err();
    assert_eq!(started.elapsed(), Duration::from_secs(5));
}

#[tokio::test]
async fn requests_to_the_directory_are_counted_by_result() {
    let capture = Capture::default();
    let _guard = capture.install();
    let harness = Harness::standard();

    harness.login("alice", "alice-pw").await.unwrap();
    let ok = capture.counter(r#"ldap_requests_total{result="ok"}"#);
    // Connection open, user search, user bind, group lookup: at least these four.
    assert!(ok >= 3, "{ok}");

    // A refused password is the directory working, not an error.
    harness.login("alice", "wrong").await.unwrap_err();
    assert!(capture.counter(r#"ldap_requests_total{result="ok"}"#) > ok);
    assert_eq!(capture.counter(r#"ldap_requests_total{result="error"}"#), 0);
    assert_eq!(
        capture.counter(r#"ldap_requests_total{result="timeout"}"#),
        0
    );
}

#[tokio::test]
async fn directory_failures_are_counted_as_errors() {
    let capture = Capture::default();
    let _guard = capture.install();
    let harness = Harness::standard();
    harness.fake.knobs().fail_connect = true;
    harness.login("alice", "alice-pw").await.unwrap_err();
    assert!(capture.counter(r#"ldap_requests_total{result="error"}"#) >= 1);
    assert_eq!(capture.counter(r#"ldap_requests_total{result="ok"}"#), 0);

    // A failing search on a healthy connection is an error too, and "no such object" is not.
    let capture = Capture::default();
    let _guard = capture.install();
    let harness = Harness::standard();
    harness.fake.knobs().search_error = Some(TransportError::Server {
        code: 52,
        message: "unavailable".into(),
    });
    harness.login("alice", "alice-pw").await.unwrap_err();
    assert_eq!(capture.counter(r#"ldap_requests_total{result="error"}"#), 1);
    harness.fake.knobs().search_error = Some(TransportError::Server {
        code: 32,
        message: "no such object".into(),
    });
    harness.login("alice", "alice-pw").await.unwrap_err();
    assert_eq!(
        capture.counter(r#"ldap_requests_total{result="error"}"#),
        1,
        "noSuchObject is an answer"
    );
}

#[tokio::test(start_paused = true)]
async fn timeouts_are_counted_as_timeouts() {
    let capture = Capture::default();
    let _guard = capture.install();
    let harness = Harness::standard();
    harness.fake.knobs().hang_search = true;
    harness.login("alice", "alice-pw").await.unwrap_err();
    assert_eq!(
        capture.counter(r#"ldap_requests_total{result="timeout"}"#),
        1
    );

    let harness = Harness::standard();
    harness.fake.knobs().hang_user_bind = true;
    harness.login("alice", "alice-pw").await.unwrap_err();
    assert_eq!(
        capture.counter(r#"ldap_requests_total{result="timeout"}"#),
        2
    );
}

#[tokio::test]
async fn the_directory_is_not_asked_for_a_user_the_input_checks_refuse() {
    let capture = Capture::default();
    let _guard = capture.install();
    let harness = Harness::standard();
    harness.login("alice", "").await.unwrap_err();
    assert!(
        capture.counter_names().is_empty(),
        "{:?}",
        capture.counter_names()
    );
    assert_eq!(harness.fake.events().len(), 0);
}
