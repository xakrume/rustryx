//! Tests of the authentication sequence on the fake transport.

use std::time::Duration;

use rustryx_core::AuthError;
use rustryx_core::Secret;
use rustryx_core::identity::{AuthContext, AuthMethod, Credentials, UserSource};
use rustryx_core::ports::{Authenticator, GroupResolver};
use tokio::time::Instant;

use super::config::{LdapConfig, LdapGroupStrategy};
use super::testing::{
    Event, Harness, PEOPLE_DN, SERVICE_DN, ScriptedTransport, assert_empty, assert_not_empty,
    base_config, group_dn, standard_directory, user_dn,
};
use super::transport::{LdapEntry, SearchResponse, TransportError};

fn kinds(events: &[Event]) -> Vec<&'static str> {
    events
        .iter()
        .filter_map(|event| match event {
            Event::Connect { .. } => Some("connect"),
            Event::Bind { .. } => Some("bind"),
            Event::Search { .. } => Some("search"),
            Event::HealthCheck { .. } => Some("health"),
            Event::Close { .. } => Some("close"),
            Event::Dropped { .. } => None,
        })
        .collect()
}

fn invalid_credentials(result: Result<impl std::fmt::Debug, AuthError>) -> String {
    match result {
        Err(error @ AuthError::InvalidCredentials) => error.to_string(),
        other => format!("UNEXPECTED {other:?}"),
    }
}

#[tokio::test]
async fn bearer_and_anonymous_credentials_are_not_applicable() {
    let harness = Harness::standard();
    let context = AuthContext::default();
    let bearer = Credentials::Bearer(Secret::new("token"));
    assert!(
        harness
            .auth
            .authenticate(&bearer, &context)
            .await
            .unwrap()
            .is_none()
    );
    let anonymous = Credentials::Anonymous;
    assert!(
        harness
            .auth
            .authenticate(&anonymous, &context)
            .await
            .unwrap()
            .is_none()
    );
    assert_empty(&harness.fake.events(), "network events");
}

#[tokio::test]
async fn a_disabled_section_handles_nobody_and_never_connects() {
    let config = LdapConfig {
        enabled: false,
        ..base_config()
    };
    let harness = Harness::new(&config, standard_directory());
    assert!(!harness.auth.is_enabled());
    assert!(!harness.service.is_enabled());
    assert!(harness.login("alice", "alice-pw").await.unwrap().is_none());
    assert_empty(&harness.fake.events(), "network events");
    assert!(format!("{:?}", harness.service).contains("enabled: false"));
}

#[tokio::test]
async fn authenticates_a_valid_user_and_fills_the_principal() {
    let harness = Harness::standard();
    assert!(harness.auth.is_enabled());
    let principal = harness.login("alice", "alice-pw").await.unwrap().unwrap();
    assert_eq!(principal.username, "alice");
    assert_eq!(principal.source, UserSource::Ldap);
    assert_eq!(principal.auth_method, AuthMethod::BasicLdap);
    assert!(principal.roles.is_empty(), "roles are resolved later");
    assert!(principal.user_id.is_none());
    assert_eq!(
        principal.external_groups,
        vec![group_dn("devs"), group_dn("admins")],
        "DNs exactly as the directory returned them"
    );
    assert_eq!(principal.groups_confirmed_at, Some(super::testing::T0));
}

#[tokio::test]
async fn takes_the_directory_spelling_of_the_user_name() {
    let harness = Harness::standard();
    let principal = harness.login("ALICE", "alice-pw").await.unwrap().unwrap();
    assert_eq!(principal.username, "alice");
}

#[tokio::test]
async fn must_not_call_the_network_for_an_empty_blank_or_nul_password() {
    let harness = Harness::standard();
    for password in ["", " ", "   ", "\t", " \u{a0} ", "pass\0word", "\0"] {
        let result = harness.login("alice", password).await;
        assert!(
            matches!(result, Err(AuthError::InvalidCredentials)),
            "{password:?}"
        );
    }
    assert!(
        harness.fake.events().is_empty(),
        "an unusable password must not cause any network call: {:?}",
        harness.fake.events()
    );
}

#[tokio::test]
async fn must_not_call_the_network_for_an_unusable_login() {
    let harness = Harness::standard();
    let too_long = "a".repeat(257);
    for login in ["", "   ", "al\nice", "a\0b", too_long.as_str()] {
        let result = harness.login(login, "alice-pw").await;
        assert!(
            matches!(result, Err(AuthError::InvalidCredentials)),
            "{login:?}"
        );
    }
    assert_empty(&harness.fake.events(), "network events");
}

#[tokio::test]
async fn must_not_bind_with_an_empty_password_even_when_the_server_would_accept_it() {
    let harness = Harness::standard();
    for password in ["", " ", "\0"] {
        let _ = harness.login("alice", password).await;
        let _ = harness.login("nobody", password).await;
    }
    let _ = harness.login("alice", "alice-pw").await;
    let binds = harness.fake.binds();
    assert_not_empty(&binds, "binds");
    assert!(
        binds.iter().all(|(_, _, password)| !password.is_empty()),
        "a bind with an empty password reached the wire: {binds:?}"
    );
}

#[tokio::test]
async fn must_not_authenticate_a_wrong_password() {
    let harness = Harness::standard();
    let result = harness.login("alice", "wrong").await;
    assert!(matches!(result, Err(AuthError::InvalidCredentials)));
}

#[tokio::test]
async fn must_not_authenticate_an_unknown_user() {
    let harness = Harness::standard();
    let result = harness.login("mallory", "anything").await;
    assert!(matches!(result, Err(AuthError::InvalidCredentials)));
}

#[tokio::test]
async fn must_not_reveal_whether_the_user_exists() {
    let harness = Harness::standard();
    harness.login("alice", "alice-pw").await.unwrap();

    harness.fake.clear_events();
    let wrong_password = invalid_credentials(harness.login("alice", "wrong").await);
    let wrong_password_shape = kinds(&harness.fake.events());

    harness.fake.clear_events();
    let unknown_user = invalid_credentials(harness.login("mallory", "wrong").await);
    let unknown_user_shape = kinds(&harness.fake.events());

    assert_eq!(
        wrong_password, unknown_user,
        "the client must see the same error text"
    );
    assert_eq!(
        wrong_password_shape, unknown_user_shape,
        "the decoy bind must make the network work identical, or latency gives users away"
    );
}

#[tokio::test]
async fn the_decoy_bind_never_authenticates_anyone() {
    let harness = Harness::standard();
    harness.login("mallory", "anything").await.unwrap_err();
    let binds = harness.fake.binds();
    let decoy = binds
        .iter()
        .find(|(_, dn, _)| dn.starts_with("cn=rustryx-decoy"))
        .expect("a decoy bind is made when the user is unknown");
    assert!(decoy.1.ends_with(PEOPLE_DN));

    // Presenting a password that is valid for somebody else changes nothing.
    let result = harness.login("mallory", "alice-pw").await;
    assert!(matches!(result, Err(AuthError::InvalidCredentials)));
}

#[tokio::test]
async fn must_not_authenticate_when_two_entries_match() {
    let twin = LdapEntry::new("uid=alice,ou=contractors,dc=example,dc=org")
        .with_attribute("uid", ["alice"]);
    let directory = standard_directory().entry(twin);
    let mut config = base_config();
    config.user_base_dn = Some("dc=example,dc=org".into());
    let harness = Harness::new(&config, directory);
    let result = harness.login("alice", "alice-pw").await;
    assert_eq!(invalid_credentials(result), "invalid credentials");
    assert_eq!(harness.service.stats().snapshot().ambiguous_user_matches, 1);
    let real_binds: Vec<_> = harness
        .fake
        .binds()
        .into_iter()
        .filter(|(_, dn, _)| dn.starts_with("uid=alice,"))
        .collect();
    assert!(
        real_binds.is_empty(),
        "nobody may be bound as one of two candidates: {real_binds:?}"
    );
}

#[tokio::test]
async fn must_not_authenticate_when_the_server_cut_the_user_search_short() {
    let directory = standard_directory().entry(
        LdapEntry::new(format!("uid=alice,ou=contractors,{PEOPLE_DN}"))
            .with_attribute("uid", ["alice"]),
    );
    let harness = Harness::new(&base_config(), directory);
    harness.fake.knobs().server_size_limit = Some(1);
    let result = harness.login("alice", "alice-pw").await;
    assert!(matches!(result, Err(AuthError::InvalidCredentials)));
    assert_eq!(harness.service.stats().snapshot().ambiguous_user_matches, 1);
}

#[tokio::test]
async fn the_user_search_is_capped_at_two_entries() {
    let harness = Harness::standard();
    harness.login("alice", "alice-pw").await.unwrap();
    let searches = harness.fake.searches();
    let (_, user_search) = &searches[0];
    assert_eq!(user_search.size_limit, Some(2));
    assert_eq!(user_search.base, PEOPLE_DN);
    assert_eq!(user_search.filter, "(uid=alice)");
}

#[tokio::test]
async fn must_not_reuse_the_user_bind_connection() {
    let harness = Harness::standard();
    for _ in 0..3 {
        harness.login("alice", "alice-pw").await.unwrap();
    }
    let user_connections = harness.fake.connections_bound_as(&user_dn("alice"));
    assert_eq!(user_connections.len(), 3, "one fresh connection per login");
    let mut distinct = user_connections.clone();
    distinct.sort_unstable();
    distinct.dedup();
    assert_eq!(distinct.len(), 3, "a user bind connection was used twice");

    let service_connections = harness.fake.connections_bound_as(SERVICE_DN);
    for conn in &user_connections {
        assert!(!service_connections.contains(conn));
        assert!(
            harness.fake.was_closed(*conn),
            "user bind connection {conn} was not closed"
        );
        let did_other_work = harness.fake.events().iter().any(|event| match event {
            Event::Search { conn: c, .. } | Event::HealthCheck { conn: c } => c == conn,
            _ => false,
        });
        assert!(
            !did_other_work,
            "a user bind connection did something besides the one bind"
        );
        let binds_on_it = harness
            .fake
            .binds()
            .iter()
            .filter(|(c, _, _)| c == conn)
            .count();
        assert_eq!(binds_on_it, 1);
    }
}

#[tokio::test]
async fn must_not_return_a_user_bound_connection_to_the_service_pool() {
    let harness = Harness::standard();
    harness.login("alice", "alice-pw").await.unwrap();
    harness.login("bob", "bob-pw").await.unwrap();
    let active = harness.service.active.as_ref().unwrap();
    assert_eq!(
        active.pool.idle_count(),
        1,
        "only the service connection may be idle"
    );

    let user_connections: Vec<usize> = [user_dn("alice"), user_dn("bob")]
        .iter()
        .flat_map(|dn| harness.fake.connections_bound_as(dn))
        .collect();
    for (conn, _) in harness.fake.searches() {
        assert!(
            !user_connections.contains(&conn),
            "connection {conn} searched after carrying a user bind"
        );
    }
    let service_connections = harness.fake.connections_bound_as(SERVICE_DN);
    assert_eq!(
        service_connections.len(),
        1,
        "the service connection is pooled and reused"
    );
}

#[tokio::test]
async fn special_characters_in_the_login_cannot_change_the_filter() {
    let harness = Harness::standard();
    for login in ["*", "*)(uid=*", "alice)(|(uid=*", "\\", "al*ce", "(alice)"] {
        let result = harness.login(login, "alice-pw").await;
        assert!(
            matches!(result, Err(AuthError::InvalidCredentials)),
            "{login:?} must be a plain unknown user, not a server error or a match: {result:?}"
        );
    }
    let filters: Vec<String> = harness
        .fake
        .searches()
        .into_iter()
        .map(|(_, request)| request.filter)
        .collect();
    assert!(
        filters.contains(&"(uid=\\2a\\29\\28uid=\\2a)".to_owned()),
        "{filters:?}"
    );
    assert!(filters.contains(&"(uid=\\5c)".to_owned()), "{filters:?}");
    assert!(
        filters
            .iter()
            .all(|filter| filter.matches('(').count() == 1),
        "{filters:?}"
    );
}

#[tokio::test]
async fn a_failing_service_bind_is_a_provider_error_without_details() {
    let mut config = base_config();
    config.bind_password = Some(Secret::new("not-the-service-password"));
    let harness = Harness::new(&config, standard_directory());
    let error = harness.login("alice", "alice-pw").await.unwrap_err();
    let shown = error.to_string();
    assert!(matches!(error, AuthError::ProviderUnavailable(_)));
    assert!(
        !shown.contains("49") && !shown.contains(SERVICE_DN),
        "{shown}"
    );
    assert_empty(
        &harness.fake.connections_bound_as(&user_dn("alice")),
        "user binds",
    );
}

#[tokio::test]
async fn an_unreachable_directory_is_a_provider_error() {
    let harness = Harness::standard();
    harness.fake.knobs().fail_connect = true;
    let error = harness.login("alice", "alice-pw").await.unwrap_err();
    assert!(
        matches!(error, AuthError::ProviderUnavailable(_)),
        "{error:?}"
    );
    assert!(!error.to_string().contains("refused"));
}

#[tokio::test]
async fn a_busy_directory_during_the_user_bind_is_not_reported_as_a_wrong_password() {
    let harness = Harness::standard();
    harness.fake.knobs().user_bind_error = Some(TransportError::Server {
        code: 51,
        message: "busy".into(),
    });
    let error = harness.login("alice", "alice-pw").await.unwrap_err();
    assert!(
        matches!(error, AuthError::ProviderUnavailable(_)),
        "{error:?}"
    );
}

#[tokio::test]
async fn a_failing_decoy_bind_does_not_change_the_answer() {
    let harness = Harness::standard();
    harness.fake.knobs().user_bind_error = Some(TransportError::Server {
        code: 51,
        message: "busy".into(),
    });
    let result = harness.login("nobody", "whatever").await;
    assert!(
        matches!(result, Err(AuthError::InvalidCredentials)),
        "the decoy is for timing only; its failure must not leak: {result:?}"
    );
}

#[tokio::test]
async fn account_policy_refusals_are_wrong_password_towards_the_client() {
    for code in [19, 32, 34, 48, 49, 50, 53] {
        let harness = Harness::standard();
        harness.fake.knobs().user_bind_error = Some(TransportError::Server {
            code,
            message: "refused".into(),
        });
        let result = harness.login("alice", "alice-pw").await;
        assert!(
            matches!(result, Err(AuthError::InvalidCredentials)),
            "code {code}: {result:?}"
        );
    }
}

#[tokio::test(start_paused = true)]
async fn must_honour_the_connect_timeout() {
    let harness = Harness::standard();
    harness.fake.knobs().hang_connect = true;
    let started = Instant::now();
    let error = harness.login("alice", "alice-pw").await.unwrap_err();
    assert_eq!(started.elapsed(), Duration::from_secs(5));
    assert_eq!(
        error.to_string(),
        "identity provider unavailable: ldap connect timed out"
    );
}

#[tokio::test(start_paused = true)]
async fn must_honour_the_service_bind_timeout() {
    let harness = Harness::standard();
    harness.fake.knobs().hang_bind = true;
    let started = Instant::now();
    let error = harness.login("alice", "alice-pw").await.unwrap_err();
    assert_eq!(started.elapsed(), Duration::from_secs(5));
    assert_eq!(
        error.to_string(),
        "identity provider unavailable: ldap bind timed out"
    );
}

#[tokio::test(start_paused = true)]
async fn must_honour_the_search_timeout_and_discard_the_connection() {
    let harness = Harness::standard();
    harness.fake.knobs().hang_search = true;
    let started = Instant::now();
    let error = harness.login("alice", "alice-pw").await.unwrap_err();
    assert_eq!(started.elapsed(), Duration::from_secs(5));
    assert_eq!(
        error.to_string(),
        "identity provider unavailable: ldap search timed out"
    );
    let active = harness.service.active.as_ref().unwrap();
    assert_eq!(
        active.pool.idle_count(),
        0,
        "an interrupted connection must not be pooled"
    );
    assert_eq!(harness.service.stats().snapshot().pool_discarded, 1);
}

#[tokio::test(start_paused = true)]
async fn must_honour_the_user_bind_timeout() {
    let harness = Harness::standard();
    harness.fake.knobs().hang_user_bind = true;
    let started = Instant::now();
    let error = harness.login("alice", "alice-pw").await.unwrap_err();
    assert_eq!(started.elapsed(), Duration::from_secs(5));
    assert_eq!(
        error.to_string(),
        "identity provider unavailable: ldap bind timed out"
    );
    let user_connections = harness.fake.connections_bound_as(&user_dn("alice"));
    assert_eq!(user_connections.len(), 1);
    assert!(
        harness.fake.was_closed(user_connections[0]),
        "closed even after a timeout"
    );
}

#[tokio::test(start_paused = true)]
async fn must_honour_the_overall_authentication_deadline() {
    let mut config = base_config();
    config.auth_deadline_secs = 6;
    let harness = Harness::new(&config, standard_directory());
    {
        let mut knobs = harness.fake.knobs();
        knobs.search_delay = Some(Duration::from_secs(4));
        knobs.hang_user_bind = true;
    }
    let started = Instant::now();
    let error = harness.login("alice", "alice-pw").await.unwrap_err();
    assert_eq!(
        started.elapsed(),
        Duration::from_secs(6),
        "the deadline (6 s) must win over search 4 s plus bind timeout 5 s"
    );
    assert_eq!(
        error.to_string(),
        "identity provider unavailable: ldap authentication deadline exceeded"
    );
}

#[tokio::test]
async fn a_failed_group_lookup_does_not_fail_the_login_but_leaves_the_resolver_closed() {
    let mut config = base_config();
    config.group_strategy = LdapGroupStrategy::ReverseMember;
    config.group_base_dn = Some(super::testing::GROUPS_DN.into());
    let harness = Harness::new(&config, standard_directory());
    harness.fake.knobs().search_error_for_base = vec![(super::testing::GROUPS_DN.to_owned(), 52)];
    let principal = harness.login("alice", "alice-pw").await.unwrap().unwrap();
    assert_empty(&principal.external_groups, "groups");
    assert!(principal.groups_confirmed_at.is_none());
    let error = harness
        .resolver
        .external_groups(&principal)
        .await
        .unwrap_err();
    assert!(
        matches!(error, AuthError::GroupLookupFailed(_)),
        "{error:?}"
    );
}

#[tokio::test]
async fn an_entry_without_a_dn_is_treated_as_no_user() {
    let transport = ScriptedTransport::new(vec![Ok(SearchResponse {
        entries: vec![LdapEntry::new("").with_attribute("uid", ["ghost"])],
        size_limit_exceeded: false,
    })]);
    let harness = Harness::scripted(&base_config(), transport);
    let result = harness.login("ghost", "user-pw").await;
    assert!(matches!(result, Err(AuthError::InvalidCredentials)));
}

#[tokio::test]
async fn an_empty_search_result_with_a_limit_flag_is_still_no_user() {
    let transport = ScriptedTransport::new(vec![Ok(SearchResponse {
        entries: Vec::new(),
        size_limit_exceeded: true,
    })]);
    let harness = Harness::scripted(&base_config(), transport);
    let result = harness.login("ghost", "user-pw").await;
    assert!(matches!(result, Err(AuthError::InvalidCredentials)));
}

#[tokio::test]
async fn a_ranged_member_of_attribute_does_not_become_a_partial_group_list() {
    let transport = ScriptedTransport::new(vec![Ok(SearchResponse {
        entries: vec![
            LdapEntry::new(user_dn("alice"))
                .with_attribute("uid", ["alice"])
                .with_attribute("memberOf;range=0-1499", [group_dn("devs")]),
        ],
        size_limit_exceeded: false,
    })]);
    let harness = Harness::scripted(&base_config(), transport);
    let principal = harness.login("alice", "user-pw").await.unwrap().unwrap();
    assert!(
        principal.external_groups.is_empty(),
        "a ranged slice must not be taken as the full list"
    );
    assert!(principal.groups_confirmed_at.is_none());
}

#[tokio::test(start_paused = true)]
async fn the_overall_deadline_covers_the_group_warm_up_too() {
    let mut config = base_config();
    config.auth_deadline_secs = 5;
    let harness = Harness::new(&config, standard_directory());
    harness.fake.knobs().search_delay = Some(Duration::from_secs(2));
    let started = Instant::now();
    let principal = harness.login("alice", "alice-pw").await.unwrap().unwrap();
    assert_eq!(
        started.elapsed(),
        Duration::from_secs(5),
        "user search 2 s, then nested group reads of 2 s each (4 s more): cut at 5 s"
    );
    assert_empty(
        &principal.external_groups,
        "groups claimed without a confirmed lookup",
    );
    assert!(principal.groups_confirmed_at.is_none());
}

proptest::proptest! {
    #![proptest_config(proptest::prelude::ProptestConfig::with_cases(64))]

    #[test]
    fn must_not_let_any_login_break_the_search_filter(
        chars in proptest::collection::vec(
            proptest::prop_oneof![
                proptest::prelude::any::<char>(),
                proptest::sample::select(vec!['(', ')', '*', '\\', '=', '&', '|', '!', '{', '}', ',']),
            ],
            1..24,
        )
    ) {
        let login: String = chars.into_iter().collect();
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let outcome = runtime.block_on(async {
            let harness = Harness::standard();
            harness.login(&login, "alice-pw").await
        });
        // A malformed filter would come back from the strict fake as a server error and
        // surface as ProviderUnavailable. Any login is either a user or nobody.
        proptest::prop_assert!(
            matches!(outcome, Ok(Some(_)) | Err(AuthError::InvalidCredentials)),
            "login {:?} produced {:?}", login, outcome
        );
    }
}
