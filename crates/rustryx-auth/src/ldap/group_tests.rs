//! Tests of group discovery, the walk limits and the five minute rule.

use std::time::Duration;

use rustryx_core::AuthError;
use rustryx_core::identity::{IdpProvider, UserSource};
use rustryx_core::ports::GroupResolver;
use time::Duration as TimeDuration;
use tokio::time::Instant;

use super::config::{LdapConfig, LdapGroupStrategy};
use super::resolver::GroupFreshness;
use super::testing::{
    Directory, GROUPS_DN, Harness, ScriptedTransport, T0, assert_empty, assert_not_empty,
    base_config, group_dn, ldap_principal, standard_directory, user_dn,
};
use super::transport::{LdapEntry, SearchResponse, TransportError};

fn reverse_config() -> LdapConfig {
    let mut config = base_config();
    config.group_strategy = LdapGroupStrategy::ReverseMember;
    config.group_base_dn = Some(GROUPS_DN.into());
    config
}

/// alice is in `g0`, `g0` is in `g1`, and so on up to `g{n-1}`.
fn chain(n: usize) -> Directory {
    let mut directory = Directory::new()
        .user("alice", "alice-pw")
        .group("g0", &[&user_dn("alice")]);
    for level in 1..n {
        directory = directory.group(
            &format!("g{level}"),
            &[&group_dn(&format!("g{}", level - 1))],
        );
    }
    directory
}

fn chain_names(n: usize) -> Vec<String> {
    (0..n).map(|level| group_dn(&format!("g{level}"))).collect()
}

async fn groups_of(harness: &Harness, user: &str) -> Result<Vec<String>, AuthError> {
    harness
        .resolver
        .external_groups(&ldap_principal(user))
        .await
        .map(|external| external.groups)
}

/// Stale serving is opt in; the tests of its limits run with it on.
fn stale_config() -> LdapConfig {
    let mut config = base_config();
    config.groups_stale_on_error = true;
    config
}

fn stale_harness() -> Harness {
    Harness::new(&stale_config(), standard_directory())
}

fn secs(n: i64) -> TimeDuration {
    TimeDuration::seconds(n)
}

#[tokio::test]
async fn member_of_returns_the_groups_of_the_user_as_ldap_provider() {
    let harness = Harness::standard();
    let external = harness
        .resolver
        .external_groups(&ldap_principal("alice"))
        .await
        .unwrap();
    assert_eq!(external.provider, IdpProvider::Ldap);
    assert_eq!(external.groups, vec![group_dn("devs"), group_dn("admins")]);
    let bob = groups_of(&harness, "bob").await.unwrap();
    assert_eq!(bob, vec![group_dn("admins")]);
}

#[tokio::test]
async fn must_not_modify_the_group_dns() {
    let odd = "CN=Admins , OU=Groups,DC=Example,DC=Org";
    let directory = Directory::new()
        .user("alice", "pw")
        .entry(LdapEntry::new(odd).with_attribute("member", [user_dn("alice")]));
    let harness = Harness::new(&base_config(), directory);
    let groups = groups_of(&harness, "alice").await.unwrap();
    assert_eq!(
        groups,
        vec![odd.to_owned()],
        "case, spacing and order are the admin_groups match key"
    );
}

#[tokio::test]
async fn reverse_member_searches_the_group_tree_for_the_user_dn() {
    let harness = Harness::new(&reverse_config(), standard_directory());
    let groups = groups_of(&harness, "alice").await.unwrap();
    assert_eq!(groups, vec![group_dn("devs"), group_dn("admins")]);
    let (_, request) = harness
        .fake
        .searches()
        .into_iter()
        .find(|(_, request)| request.base == GROUPS_DN)
        .unwrap();
    assert_eq!(request.filter, format!("(member={})", user_dn("alice")));
    assert_eq!(request.attributes, vec!["1.1".to_owned()]);
    assert_eq!(
        request.size_limit,
        Some(1001),
        "max_groups (1000) plus one, to detect the cap"
    );
}

#[tokio::test]
async fn reverse_member_supports_member_uid_groups() {
    let mut config = reverse_config();
    config.group_filter = "(memberUid={username})".into();
    config.nested_groups = false;
    let directory = Directory::new()
        .user("alice", "pw")
        .posix_group("staff", &["alice", "carol"])
        .posix_group("ops", &["carol"]);
    let harness = Harness::new(&config, directory);
    assert_eq!(
        groups_of(&harness, "alice").await.unwrap(),
        vec![group_dn("staff")]
    );
}

#[tokio::test]
async fn must_not_let_special_characters_change_a_group_filter() {
    let weird_dn = "uid=a*)(member=*,ou=people,dc=example,dc=org";
    let directory = Directory::new()
        .entry(LdapEntry::new(weird_dn).with_attribute("uid", ["weird"]))
        .group("devs", &[weird_dn]);
    let harness_weird = Harness::new(&reverse_config(), directory);
    let groups = groups_of(&harness_weird, "weird").await.unwrap();
    assert_eq!(groups, vec![group_dn("devs")]);
    let filters: Vec<String> = harness_weird
        .fake
        .searches()
        .into_iter()
        .map(|(_, request)| request.filter)
        .collect();
    assert!(
        filters
            .iter()
            .any(|f| f == "(member=uid=a\\2a\\29\\28member=\\2a,ou=people,dc=example,dc=org)"),
        "{filters:?}"
    );
}

#[tokio::test]
async fn nested_groups_are_followed_with_member_of() {
    let directory = chain(3).group("devs", &[&user_dn("alice")]);
    let harness = Harness::new(&base_config(), directory);
    let groups = groups_of(&harness, "alice").await.unwrap();
    assert_eq!(groups.len(), 4);
    for expected in chain_names(3).into_iter().chain([group_dn("devs")]) {
        assert!(
            groups.contains(&expected),
            "{expected} missing from {groups:?}"
        );
    }
}

#[tokio::test]
async fn nested_groups_are_followed_with_reverse_member() {
    let harness = Harness::new(&reverse_config(), chain(4));
    let groups = groups_of(&harness, "alice").await.unwrap();
    assert_eq!(
        groups,
        chain_names(4),
        "breadth first order: nearest group first"
    );
}

#[tokio::test]
async fn must_not_loop_forever_on_a_group_cycle() {
    for config in [base_config(), reverse_config()] {
        let directory = Directory::new()
            .user("alice", "pw")
            .group("a", &[&user_dn("alice"), &group_dn("b")])
            .group("b", &[&group_dn("a")]);
        let harness = Harness::new(&config, directory);
        let groups = groups_of(&harness, "alice").await.unwrap();
        assert_eq!(groups.len(), 2, "{groups:?}");
        assert!(groups.contains(&group_dn("a")) && groups.contains(&group_dn("b")));
        assert!(
            harness.fake.searches().len() <= 6,
            "the walk kept searching: {:?}",
            harness.fake.searches().len()
        );
    }
}

#[tokio::test]
async fn must_not_loop_on_a_cycle_when_the_directory_spells_dns_differently() {
    let directory = Directory::new()
        .without_memberof_overlay()
        .entry(
            LdapEntry::new(user_dn("alice"))
                .with_attribute("uid", ["alice"])
                .with_attribute("memberOf", [group_dn("a")]),
        )
        .entry(
            LdapEntry::new(group_dn("a"))
                .with_attribute("memberOf", ["CN=B,OU=Groups,DC=Example,DC=Org"]),
        )
        .entry(
            LdapEntry::new(group_dn("b"))
                .with_attribute("memberOf", ["CN=A, OU=GROUPS,DC=EXAMPLE,DC=ORG"]),
        );
    let harness = Harness::new(&base_config(), directory);
    let groups = groups_of(&harness, "alice").await.unwrap();
    assert_eq!(
        groups,
        vec![group_dn("a"), "CN=B,OU=Groups,DC=Example,DC=Org".to_owned()],
        "each group once, first spelling kept"
    );
}

#[tokio::test]
async fn the_walk_stops_at_the_depth_limit_and_counts_it() {
    let mut config = base_config();
    config.nested_group_depth = 3;
    let harness = Harness::new(&config, chain(8));
    let groups = groups_of(&harness, "alice").await.unwrap();
    assert_eq!(groups, chain_names(3));
    assert_eq!(harness.service.stats().snapshot().group_depth_limit_hits, 1);
}

#[tokio::test]
async fn depth_one_means_direct_groups_only() {
    let mut config = reverse_config();
    config.nested_group_depth = 1;
    let harness = Harness::new(&config, chain(4));
    assert_eq!(
        groups_of(&harness, "alice").await.unwrap(),
        vec![group_dn("g0")]
    );
    assert_eq!(harness.service.stats().snapshot().group_depth_limit_hits, 1);
}

#[tokio::test]
async fn a_walk_that_ends_before_the_limit_is_not_counted() {
    let mut config = base_config();
    config.nested_group_depth = 5;
    let harness = Harness::new(&config, chain(3));
    assert_eq!(groups_of(&harness, "alice").await.unwrap(), chain_names(3));
    assert_eq!(harness.service.stats().snapshot().group_depth_limit_hits, 0);
}

#[tokio::test]
async fn nesting_can_be_switched_off() {
    let mut config = base_config();
    config.nested_groups = false;
    let harness = Harness::new(&config, chain(4));
    assert_eq!(
        groups_of(&harness, "alice").await.unwrap(),
        vec![group_dn("g0")]
    );
    assert_eq!(harness.service.stats().snapshot().group_depth_limit_hits, 0);
}

#[tokio::test]
async fn the_walk_stops_at_the_group_count_limit_and_counts_it() {
    for config in [base_config(), reverse_config()] {
        let mut config = config;
        config.max_groups = 3;
        let mut directory = Directory::new().user("alice", "pw");
        for index in 0..10 {
            directory = directory.group(&format!("g{index}"), &[&user_dn("alice")]);
        }
        let harness = Harness::new(&config, directory);
        let groups = groups_of(&harness, "alice").await.unwrap();
        assert_eq!(groups.len(), 3, "{groups:?}");
        assert_eq!(harness.service.stats().snapshot().group_size_limit_hits, 1);
    }
}

#[tokio::test]
async fn the_group_count_limit_also_stops_nested_expansion() {
    let mut config = base_config();
    config.max_groups = 2;
    let harness = Harness::new(&config, chain(6));
    let groups = groups_of(&harness, "alice").await.unwrap();
    assert_eq!(groups, chain_names(2));
    assert_eq!(harness.service.stats().snapshot().group_size_limit_hits, 1);

    let harness = Harness::new(&config, chain(2));
    let groups = groups_of(&harness, "alice").await.unwrap();
    assert_eq!(groups, chain_names(2));
    assert_eq!(
        harness.service.stats().snapshot().group_size_limit_hits,
        0,
        "a tree that fits exactly into the cap did not hit it"
    );
}

#[tokio::test]
async fn must_not_turn_a_failed_group_search_into_an_empty_list() {
    let harness = Harness::new(&reverse_config(), standard_directory());
    harness.fake.knobs().search_error_for_base = vec![(GROUPS_DN.to_owned(), 52)];
    let result = groups_of(&harness, "alice").await;
    assert!(
        matches!(result, Err(AuthError::GroupLookupFailed(_))),
        "a failed lookup must be an error, never Ok(empty): {result:?}"
    );

    let harness = Harness::standard();
    harness.fake.knobs().search_error = Some(TransportError::Closed);
    let result = groups_of(&harness, "alice").await;
    assert!(
        matches!(result, Err(AuthError::GroupLookupFailed(_))),
        "{result:?}"
    );
}

#[tokio::test(start_paused = true)]
async fn must_not_turn_a_timed_out_group_search_into_an_empty_list() {
    let harness = Harness::new(&reverse_config(), standard_directory());
    harness.login("alice", "alice-pw").await.unwrap();
    harness.clock.advance(secs(400));
    harness.fake.knobs().hang_search = true;
    let started = Instant::now();
    let result = groups_of(&harness, "alice").await;
    assert_eq!(
        started.elapsed(),
        Duration::from_secs(5),
        "the search timeout, not the deadline"
    );
    let error = result.unwrap_err();
    assert!(
        matches!(error, AuthError::GroupLookupFailed(ref message) if message == "ldap group lookup failed"),
        "{error:?}"
    );
}

#[tokio::test(start_paused = true)]
async fn must_honour_the_group_lookup_deadline() {
    let mut config = base_config();
    config.group_deadline_secs = 6;
    let harness = Harness::new(&config, chain(8));
    harness.fake.knobs().search_delay = Some(Duration::from_secs(4));
    let started = Instant::now();
    let result = groups_of(&harness, "alice").await;
    assert_eq!(started.elapsed(), Duration::from_secs(6));
    let error = result.unwrap_err();
    assert!(
        matches!(error, AuthError::GroupLookupFailed(ref message) if message == "ldap group lookup timed out"),
        "{error:?}"
    );
}

#[tokio::test]
async fn must_not_accept_a_group_list_cut_by_the_servers_own_size_limit() {
    let harness = Harness::new(&reverse_config(), standard_directory());
    harness.fake.knobs().server_size_limit = Some(1);
    let result = groups_of(&harness, "alice").await;
    assert!(
        matches!(result, Err(AuthError::GroupLookupFailed(_))),
        "{result:?}"
    );
}

#[tokio::test]
async fn a_member_of_value_pointing_at_a_deleted_group_has_no_parents() {
    let directory = Directory::new()
        .without_memberof_overlay()
        .entry(
            LdapEntry::new(user_dn("alice"))
                .with_attribute("uid", ["alice"])
                .with_attribute("memberOf", [group_dn("ghost"), group_dn("devs")]),
        )
        .entry(LdapEntry::new(group_dn("devs")));
    let harness = Harness::new(&base_config(), directory);
    let groups = groups_of(&harness, "alice").await.unwrap();
    assert_eq!(groups, vec![group_dn("ghost"), group_dn("devs")]);
}

#[tokio::test]
async fn any_other_failure_reading_a_nested_group_fails_the_lookup() {
    let directory = chain(3);
    let harness = Harness::new(&base_config(), directory);
    harness.fake.knobs().search_error_for_base = vec![(group_dn("g1"), 50)];
    let result = groups_of(&harness, "alice").await;
    assert!(
        matches!(result, Err(AuthError::GroupLookupFailed(_))),
        "{result:?}"
    );
}

#[tokio::test]
async fn must_not_treat_a_ranged_member_of_slice_as_the_complete_list() {
    let transport = ScriptedTransport::new(vec![Ok(SearchResponse {
        entries: vec![
            LdapEntry::new(user_dn("alice"))
                .with_attribute("uid", ["alice"])
                .with_attribute("memberOf;range=0-1499", [group_dn("devs")]),
        ],
        size_limit_exceeded: false,
    })]);
    let harness = Harness::scripted(&base_config(), transport);
    let result = groups_of(&harness, "alice").await;
    assert!(
        matches!(result, Err(AuthError::GroupLookupFailed(_))),
        "{result:?}"
    );
}

#[tokio::test]
async fn a_group_read_that_returns_no_entry_has_no_parents() {
    let transport = ScriptedTransport::new(vec![
        Ok(SearchResponse {
            entries: vec![
                LdapEntry::new(user_dn("alice"))
                    .with_attribute("uid", ["alice"])
                    .with_attribute("memberOf", [group_dn("devs")]),
            ],
            size_limit_exceeded: false,
        }),
        Ok(SearchResponse::default()),
    ]);
    let harness = Harness::scripted(&base_config(), transport);
    assert_eq!(
        groups_of(&harness, "alice").await.unwrap(),
        vec![group_dn("devs")]
    );
}

#[tokio::test]
async fn a_ranged_attribute_on_a_nested_group_fails_the_lookup() {
    let transport = ScriptedTransport::new(vec![
        Ok(SearchResponse {
            entries: vec![
                LdapEntry::new(user_dn("alice"))
                    .with_attribute("uid", ["alice"])
                    .with_attribute("memberOf", [group_dn("devs")]),
            ],
            size_limit_exceeded: false,
        }),
        Ok(SearchResponse {
            entries: vec![
                LdapEntry::new(group_dn("devs"))
                    .with_attribute("memberOf;range=0-1", [group_dn("x")]),
            ],
            size_limit_exceeded: false,
        }),
    ]);
    let harness = Harness::scripted(&base_config(), transport);
    assert!(matches!(
        groups_of(&harness, "alice").await,
        Err(AuthError::GroupLookupFailed(_))
    ));
}

#[tokio::test]
async fn a_fresh_cache_answers_without_touching_the_directory() {
    let harness = Harness::standard();
    let first = harness
        .resolver
        .resolve_detailed(&ldap_principal("alice"))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(first.freshness, GroupFreshness::Fresh);
    assert_eq!(first.confirmed_at, T0);
    harness.fake.clear_events();
    harness.clock.advance(secs(59));
    let second = harness
        .resolver
        .resolve_detailed(&ldap_principal("alice"))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(second.freshness, GroupFreshness::Cached);
    assert_eq!(second.confirmed_at, T0);
    assert_eq!(second.groups, first.groups);
    assert!(
        harness.fake.events().is_empty(),
        "{:?}",
        harness.fake.events()
    );
}

#[tokio::test]
async fn login_warms_the_cache_so_the_first_resolve_is_free() {
    let harness = Harness::standard();
    let principal = harness.login("alice", "alice-pw").await.unwrap().unwrap();
    harness.fake.clear_events();
    let resolved = harness
        .resolver
        .resolve_detailed(&principal)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(resolved.freshness, GroupFreshness::Cached);
    assert_eq!(resolved.groups, principal.external_groups);
    assert_empty(&harness.fake.events(), "network events");
}

#[tokio::test]
async fn removing_a_user_from_a_group_takes_effect_after_the_cache_ttl() {
    let harness = Harness::standard();
    let before = groups_of(&harness, "alice").await.unwrap();
    assert!(before.contains(&group_dn("admins")));
    harness
        .fake
        .update_directory(|d| d.remove_member(&group_dn("admins"), &user_dn("alice")));

    harness.clock.advance(secs(30));
    assert_eq!(
        groups_of(&harness, "alice").await.unwrap(),
        before,
        "still within the TTL"
    );

    harness.clock.advance(secs(31));
    let after = groups_of(&harness, "alice").await.unwrap();
    assert_eq!(after, vec![group_dn("devs")]);
}

#[tokio::test]
async fn must_not_serve_groups_older_than_five_minutes_when_the_directory_is_down() {
    let harness = stale_harness();
    let principal = harness.login("alice", "alice-pw").await.unwrap().unwrap();
    harness.fake.knobs().search_error = Some(TransportError::Closed);

    harness.clock.advance(secs(61));
    let stale = harness
        .resolver
        .resolve_detailed(&principal)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stale.freshness, GroupFreshness::StaleWithinWindow);
    assert_eq!(
        stale.confirmed_at, T0,
        "the timestamp of the last SEARCH, never now"
    );
    assert_eq!(stale.groups, principal.external_groups);

    harness.clock.set(T0 + secs(300));
    assert!(
        harness.resolver.resolve_detailed(&principal).await.is_ok(),
        "exactly five minutes is still inside the window"
    );

    harness.clock.set(T0 + secs(301));
    let result = harness.resolver.external_groups(&principal).await;
    assert!(
        matches!(result, Err(AuthError::GroupLookupFailed(_))),
        "301 s after the last successful search the roles must go: {result:?}"
    );
    let stats = harness.service.stats().snapshot();
    assert_eq!(stats.groups_expired, 1);
    assert_eq!(stats.groups_served_stale, 2);
    assert_eq!(stats.group_lookup_failures, 3);
}

#[tokio::test]
async fn must_not_extend_the_window_by_serving_a_stale_answer() {
    let harness = stale_harness();
    let principal = harness.login("alice", "alice-pw").await.unwrap().unwrap();
    harness.fake.knobs().search_error = Some(TransportError::Closed);
    harness.clock.set(T0 + secs(200));
    let stale = harness
        .resolver
        .resolve_detailed(&principal)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stale.freshness, GroupFreshness::StaleWithinWindow);
    harness.clock.set(T0 + secs(301));
    let result = harness.resolver.external_groups(&principal).await;
    assert!(
        matches!(result, Err(AuthError::GroupLookupFailed(_))),
        "{result:?}"
    );
}

#[tokio::test]
async fn must_not_trust_the_principals_own_confirmation_time() {
    let harness = stale_harness();
    let mut principal = ldap_principal("alice");
    principal.groups_confirmed_at = Some(T0);
    principal.external_groups = vec![group_dn("admins")];
    harness.fake.knobs().search_error = Some(TransportError::Closed);
    let result = harness.resolver.external_groups(&principal).await;
    assert!(
        matches!(result, Err(AuthError::GroupLookupFailed(_))),
        "a caller supplied timestamp must not be able to create grace: {result:?}"
    );
}

#[tokio::test]
async fn a_timestamp_in_the_future_after_a_clock_step_is_not_trusted() {
    let harness = stale_harness();
    let principal = harness.login("alice", "alice-pw").await.unwrap().unwrap();
    harness.clock.set(T0 - secs(3600));
    harness.fake.knobs().search_error = Some(TransportError::Closed);
    let result = harness.resolver.external_groups(&principal).await;
    assert!(
        matches!(result, Err(AuthError::GroupLookupFailed(_))),
        "{result:?}"
    );
}

#[tokio::test]
async fn the_directory_recovering_replaces_the_stale_set() {
    let harness = stale_harness();
    let principal = harness.login("alice", "alice-pw").await.unwrap().unwrap();
    harness.fake.knobs().search_error = Some(TransportError::Closed);
    harness.clock.advance(secs(100));
    assert_eq!(
        harness
            .resolver
            .resolve_detailed(&principal)
            .await
            .unwrap()
            .unwrap()
            .freshness,
        GroupFreshness::StaleWithinWindow
    );
    harness.fake.knobs().search_error = None;
    let fresh = harness
        .resolver
        .resolve_detailed(&principal)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(fresh.freshness, GroupFreshness::Fresh);
    assert_eq!(fresh.confirmed_at, T0 + secs(100));
}

#[tokio::test]
async fn must_not_fall_back_to_stale_groups_when_the_user_is_gone() {
    let harness = stale_harness();
    let principal = harness.login("alice", "alice-pw").await.unwrap().unwrap();
    harness
        .fake
        .update_directory(|d| d.remove_entry(&user_dn("alice")));
    harness.clock.advance(secs(61));
    let result = harness.resolver.external_groups(&principal).await;
    assert!(
        matches!(result, Err(AuthError::GroupLookupFailed(ref m)) if m.contains("no longer")),
        "{result:?}"
    );

    harness.fake.knobs().search_error = Some(TransportError::Closed);
    harness.clock.advance(secs(10));
    let result = harness.resolver.external_groups(&principal).await;
    assert!(
        matches!(result, Err(AuthError::GroupLookupFailed(_))),
        "the cache entry of a deleted user must be gone, an outage must not resurrect it: {result:?}"
    );
}

#[tokio::test]
async fn an_ambiguous_user_at_refresh_is_treated_like_a_missing_one() {
    let directory = standard_directory().entry(
        LdapEntry::new(format!(
            "uid=alice,ou=contractors,{}",
            super::testing::PEOPLE_DN
        ))
        .with_attribute("uid", ["alice"]),
    );
    let harness = Harness::new(&base_config(), directory);
    let result = groups_of(&harness, "alice").await;
    assert!(
        matches!(result, Err(AuthError::GroupLookupFailed(_))),
        "{result:?}"
    );
}

#[tokio::test]
async fn special_characters_in_a_principal_name_cannot_change_the_refresh_filter() {
    let harness = Harness::standard();
    let result = groups_of(&harness, "*)(uid=*").await;
    assert!(
        matches!(result, Err(AuthError::GroupLookupFailed(ref m)) if m.contains("no longer")),
        "an unknown user, not a malformed filter error: {result:?}"
    );
}

#[tokio::test]
async fn a_principal_of_another_source_has_no_ldap_groups_and_costs_no_network_call() {
    let harness = Harness::standard();
    let mut principal = ldap_principal("alice");
    principal.source = UserSource::Local;
    let external = harness.resolver.external_groups(&principal).await.unwrap();
    assert_eq!(external.provider, IdpProvider::Ldap);
    assert_empty(&external.groups, "groups");
    assert!(
        harness
            .resolver
            .resolve_detailed(&principal)
            .await
            .unwrap()
            .is_none()
    );
    assert_empty(&harness.fake.events(), "network events");
}

#[tokio::test]
async fn an_ldap_principal_cannot_be_resolved_when_ldap_is_disabled() {
    let config = LdapConfig {
        enabled: false,
        ..base_config()
    };
    let harness = Harness::new(&config, standard_directory());
    let result = harness
        .resolver
        .external_groups(&ldap_principal("alice"))
        .await;
    assert!(
        matches!(result, Err(AuthError::GroupLookupFailed(_))),
        "{result:?}"
    );
    assert!(format!("{:?}", harness.resolver).contains("LdapGroupResolver"));
}

#[tokio::test]
async fn a_zero_cache_ttl_asks_the_directory_every_time_but_keeps_the_fallback() {
    let mut config = stale_config();
    config.groups_cache_ttl_secs = 0;
    let harness = Harness::new(&config, standard_directory());
    harness
        .resolver
        .resolve_detailed(&ldap_principal("alice"))
        .await
        .unwrap();
    harness.fake.clear_events();
    let again = harness
        .resolver
        .resolve_detailed(&ldap_principal("alice"))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(again.freshness, GroupFreshness::Fresh);
    assert_not_empty(&harness.fake.events(), "network events");
    harness.fake.knobs().search_error = Some(TransportError::Closed);
    let stale = harness
        .resolver
        .resolve_detailed(&ldap_principal("alice"))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stale.freshness, GroupFreshness::StaleWithinWindow);
}

#[tokio::test]
async fn by_default_a_failed_lookup_fails_closed_even_when_a_younger_set_exists() {
    let harness = Harness::standard();
    let principal = harness.login("alice", "alice-pw").await.unwrap().unwrap();
    harness.fake.knobs().search_error = Some(TransportError::Closed);

    harness.clock.advance(secs(30));
    let cached = harness
        .resolver
        .resolve_detailed(&principal)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        cached.freshness,
        GroupFreshness::Cached,
        "inside the TTL the directory is not asked"
    );

    harness.clock.advance(secs(31));
    let result = harness.resolver.external_groups(&principal).await;
    assert!(
        matches!(result, Err(AuthError::GroupLookupFailed(_))),
        "the grace period belongs to the caller; this layer must not add one: {result:?}"
    );
    let stats = harness.service.stats().snapshot();
    assert_eq!(stats.group_lookup_failures, 1);
    assert_eq!(stats.groups_served_stale, 0);
}

#[tokio::test]
async fn the_stale_mode_is_off_in_the_default_configuration() {
    assert!(!LdapConfig::default().groups_stale_on_error);
    assert!(!base_config().groups_stale_on_error);
}
