//! Tests of the service connection pool, driven through the public flows.

use std::time::Duration;

use rustryx_core::AuthError;

use super::config::LdapConfig;
use super::testing::{Harness, SERVICE_DN, assert_empty, base_config, standard_directory};
use super::transport::TransportError;

fn pool_of(harness: &Harness) -> &std::sync::Arc<super::pool::ServicePool> {
    &harness.service.active.as_ref().unwrap().pool
}

#[tokio::test]
async fn an_idle_service_connection_is_reused() {
    let harness = Harness::standard();
    for _ in 0..3 {
        harness.login("alice", "alice-pw").await.unwrap();
    }
    assert_eq!(harness.fake.connections_bound_as(SERVICE_DN).len(), 1);
    assert_eq!(pool_of(&harness).idle_count(), 1);
}

#[tokio::test]
async fn the_health_check_runs_on_every_checkout_of_an_idle_connection() {
    let harness = Harness::standard();
    harness.login("alice", "alice-pw").await.unwrap();
    harness.fake.clear_events();
    harness.login("alice", "alice-pw").await.unwrap();
    let checks = harness
        .fake
        .events()
        .iter()
        .filter(|event| matches!(event, super::testing::Event::HealthCheck { .. }))
        .count();
    assert_eq!(checks, 1);
}

#[tokio::test]
async fn an_unhealthy_idle_connection_is_replaced() {
    let harness = Harness::standard();
    harness.login("alice", "alice-pw").await.unwrap();
    harness.fake.kill_existing_connections();
    harness.login("alice", "alice-pw").await.unwrap();
    assert_eq!(harness.fake.connections_bound_as(SERVICE_DN).len(), 2);
    assert_eq!(
        harness.service.stats().snapshot().pool_unhealthy_evictions,
        1
    );
}

#[tokio::test(start_paused = true)]
async fn a_hung_health_check_counts_as_unhealthy() {
    let harness = Harness::standard();
    harness.login("alice", "alice-pw").await.unwrap();
    harness.fake.knobs().hang_health_check = true;
    harness.login("alice", "alice-pw").await.unwrap();
    assert_eq!(harness.fake.connections_bound_as(SERVICE_DN).len(), 2);
}

#[tokio::test(start_paused = true)]
async fn a_connection_past_its_max_lifetime_is_retired_without_being_checked() {
    let harness = Harness::standard();
    harness.login("alice", "alice-pw").await.unwrap();
    tokio::time::advance(Duration::from_secs(301)).await;
    harness.fake.clear_events();
    harness.login("alice", "alice-pw").await.unwrap();
    assert_eq!(
        harness.fake.connections_bound_as(SERVICE_DN).len(),
        1,
        "a new service connection"
    );
    let health_checks = harness
        .fake
        .events()
        .iter()
        .filter(|event| matches!(event, super::testing::Event::HealthCheck { .. }))
        .count();
    assert_eq!(
        health_checks, 0,
        "an expired connection is dropped, not probed"
    );
}

#[tokio::test(start_paused = true)]
async fn a_connection_that_ages_out_while_in_use_is_not_returned() {
    let mut config = base_config();
    config.pool_max_lifetime_secs = 2;
    let harness = Harness::new(&config, standard_directory());
    harness.fake.knobs().search_delay = Some(Duration::from_secs(3));
    harness.login("alice", "alice-pw").await.unwrap();
    assert_eq!(pool_of(&harness).idle_count(), 0);
}

#[tokio::test]
async fn the_pool_size_bounds_the_number_of_service_connections() {
    let mut config = base_config();
    config.pool_size = 1;
    let harness = Harness::new(&config, standard_directory());
    harness.fake.knobs().search_delay = Some(Duration::from_millis(20));
    let (a, b, c) = tokio::join!(
        harness.login("alice", "alice-pw"),
        harness.login("bob", "bob-pw"),
        harness.login("alice", "alice-pw"),
    );
    assert!(a.unwrap().is_some() && b.unwrap().is_some() && c.unwrap().is_some());
    assert_eq!(
        harness.fake.connections_bound_as(SERVICE_DN).len(),
        1,
        "three concurrent logins on a pool of one must share one service connection"
    );
}

#[tokio::test]
async fn the_pool_grows_up_to_its_size_under_concurrency() {
    let mut config = base_config();
    config.pool_size = 3;
    let harness = Harness::new(&config, standard_directory());
    harness.fake.knobs().search_delay = Some(Duration::from_millis(20));
    let (a, b, c) = tokio::join!(
        harness.login("alice", "alice-pw"),
        harness.login("bob", "bob-pw"),
        harness.login("alice", "alice-pw"),
    );
    assert!(a.unwrap().is_some() && b.unwrap().is_some() && c.unwrap().is_some());
    assert_eq!(harness.fake.connections_bound_as(SERVICE_DN).len(), 3);
    assert_eq!(pool_of(&harness).idle_count(), 3);
}

#[tokio::test]
async fn a_server_error_answer_keeps_the_connection() {
    let harness = Harness::standard();
    harness.login("alice", "alice-pw").await.unwrap();
    harness.fake.knobs().search_error = Some(TransportError::Server {
        code: 50,
        message: "denied".into(),
    });
    let error = harness.login("alice", "alice-pw").await.unwrap_err();
    assert!(matches!(error, AuthError::ProviderUnavailable(_)));
    assert_eq!(
        pool_of(&harness).idle_count(),
        1,
        "a refusal leaves the stream in sync"
    );
    assert_eq!(harness.service.stats().snapshot().pool_discarded, 0);
}

#[tokio::test]
async fn a_transport_error_discards_the_connection() {
    let harness = Harness::standard();
    harness.login("alice", "alice-pw").await.unwrap();
    harness.fake.knobs().search_error = Some(TransportError::Closed);
    harness.login("alice", "alice-pw").await.unwrap_err();
    assert_eq!(pool_of(&harness).idle_count(), 0);
    assert_eq!(harness.service.stats().snapshot().pool_discarded, 1);
}

#[tokio::test]
async fn failures_do_not_leak_pool_slots() {
    let mut config = base_config();
    config.pool_size = 2;
    let harness = Harness::new(&config, standard_directory());
    harness.fake.knobs().search_error = Some(TransportError::Closed);
    for _ in 0..10 {
        harness.login("alice", "alice-pw").await.unwrap_err();
    }
    harness.fake.knobs().search_error = None;
    assert!(harness.login("alice", "alice-pw").await.unwrap().is_some());
}

#[tokio::test]
async fn a_failed_connect_does_not_leak_a_pool_slot() {
    let mut config = base_config();
    config.pool_size = 1;
    let harness = Harness::new(&config, standard_directory());
    harness.fake.knobs().fail_connect = true;
    for _ in 0..3 {
        harness.login("alice", "alice-pw").await.unwrap_err();
    }
    harness.fake.knobs().fail_connect = false;
    assert!(harness.login("alice", "alice-pw").await.unwrap().is_some());
}

#[test]
fn the_pool_config_default_is_small_and_bounded() {
    let config = LdapConfig::default();
    assert_eq!(config.pool_size, 4);
    assert_eq!(config.pool_max_lifetime_secs, 300);
}

#[tokio::test]
async fn must_not_open_more_user_bind_connections_than_the_configured_cap() {
    let mut config = base_config();
    config.max_concurrent_user_binds = 2;
    config.pool_size = 8;
    let harness = Harness::new(&config, standard_directory());
    harness.fake.knobs().user_bind_delay = Some(Duration::from_millis(30));
    let results =
        futures::future::join_all((0..8).map(|_| harness.login("alice", "alice-pw"))).await;
    assert!(results.into_iter().all(|result| result.unwrap().is_some()));
    assert_eq!(
        harness.fake.max_concurrent_user_binds(),
        2,
        "eight concurrent logins on a cap of two"
    );
}

#[tokio::test]
async fn the_decoy_bind_counts_against_the_user_bind_cap_too() {
    let mut config = base_config();
    config.max_concurrent_user_binds = 1;
    let harness = Harness::new(&config, standard_directory());
    harness.fake.knobs().user_bind_delay = Some(Duration::from_millis(30));
    let results =
        futures::future::join_all((0..4).map(|_| harness.login("nobody", "whatever"))).await;
    assert!(results.into_iter().all(|result| result.is_err()));
    assert_eq!(harness.fake.max_concurrent_user_binds(), 1);
}

#[tokio::test(start_paused = true)]
async fn must_not_keep_connecting_when_the_directory_has_been_failing() {
    let harness = Harness::standard();
    harness.fake.knobs().fail_connect = true;
    for _ in 0..5 {
        let error = harness.login("alice", "alice-pw").await.unwrap_err();
        assert!(matches!(error, AuthError::ProviderUnavailable(_)));
    }
    assert_eq!(harness.service.stats().snapshot().breaker_trips, 1);

    harness.fake.knobs().fail_connect = false;
    harness.fake.clear_events();
    let started = tokio::time::Instant::now();
    let error = harness.login("alice", "alice-pw").await.unwrap_err();
    assert!(
        matches!(error, AuthError::ProviderUnavailable(_)),
        "{error:?}"
    );
    assert_eq!(
        started.elapsed(),
        Duration::ZERO,
        "refused at once, no timeout spent"
    );
    assert_empty(
        &harness.fake.events(),
        "the open breaker must not touch the network",
    );
    assert_eq!(harness.service.stats().snapshot().breaker_rejections, 1);
}

#[tokio::test(start_paused = true)]
async fn the_breaker_lets_a_trial_through_after_the_cooldown_and_closes_on_success() {
    let harness = Harness::standard();
    harness.fake.knobs().fail_connect = true;
    for _ in 0..5 {
        harness.login("alice", "alice-pw").await.unwrap_err();
    }
    harness.fake.knobs().fail_connect = false;
    tokio::time::advance(Duration::from_secs(10)).await;
    assert!(
        harness.login("alice", "alice-pw").await.unwrap().is_some(),
        "the trial"
    );
    assert!(
        harness.login("bob", "bob-pw").await.unwrap().is_some(),
        "closed again"
    );
    assert_eq!(harness.service.stats().snapshot().breaker_trips, 1);
}

#[tokio::test(start_paused = true)]
async fn a_failed_trial_opens_the_breaker_again() {
    let harness = Harness::standard();
    harness.fake.knobs().fail_connect = true;
    for _ in 0..5 {
        harness.login("alice", "alice-pw").await.unwrap_err();
    }
    tokio::time::advance(Duration::from_secs(10)).await;
    harness.login("alice", "alice-pw").await.unwrap_err();
    assert_eq!(harness.service.stats().snapshot().breaker_trips, 2);
    harness.fake.knobs().fail_connect = false;
    harness.fake.clear_events();
    harness.login("alice", "alice-pw").await.unwrap_err();
    assert_empty(&harness.fake.events(), "open again: still no network");
}

#[tokio::test(start_paused = true)]
async fn must_not_hammer_the_directory_with_a_wrong_service_password() {
    let mut config = base_config();
    config.bind_password = Some(rustryx_core::Secret::new("not-the-service-password"));
    let harness = Harness::new(&config, standard_directory());
    for _ in 0..20 {
        harness.login("alice", "alice-pw").await.unwrap_err();
    }
    let service_binds = harness.fake.connections_bound_as(SERVICE_DN).len();
    assert_eq!(
        service_binds, 5,
        "five attempts open the breaker, the other fifteen logins never reached the directory"
    );
}

#[tokio::test(start_paused = true)]
async fn an_idle_healthy_connection_keeps_working_while_the_breaker_is_open() {
    let harness = Harness::standard();
    harness.login("alice", "alice-pw").await.unwrap();
    // Failures elsewhere (new connections refused) must not take down the warm one.
    harness.fake.knobs().fail_connect = true;
    for _ in 0..5 {
        // Each of these needs a user bind connection and fails at connect, which is not
        // the service pool: the pooled connection stays usable.
        let _ = harness.login("alice", "wrong").await;
    }
    harness.fake.knobs().fail_connect = false;
    assert!(harness.login("alice", "alice-pw").await.unwrap().is_some());
}
