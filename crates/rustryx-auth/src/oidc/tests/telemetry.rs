//! `oidc_requests_total` and `oidc_jwks_age_seconds`: the series the Keycloak alerts read.

use std::sync::Arc;
use std::time::Duration;

use rustryx_core::ports::SystemClock;
use serde_json::json;
use tokio_util::sync::CancellationToken;

use crate::oidc::test_support::{AUDIENCE, TestEnv, access_claims, keys};
use crate::oidc::{OidcConfig, OidcProvider};
use crate::telemetry::recording::Capture;

const DISCOVERY_OK: &str = r#"oidc_requests_total{op="discovery",result="ok"}"#;
const DISCOVERY_ERROR: &str = r#"oidc_requests_total{op="discovery",result="error"}"#;
const DISCOVERY_TIMEOUT: &str = r#"oidc_requests_total{op="discovery",result="timeout"}"#;
const JWKS_OK: &str = r#"oidc_requests_total{op="jwks",result="ok"}"#;
const JWKS_ERROR: &str = r#"oidc_requests_total{op="jwks",result="error"}"#;
const AGE: &str = "oidc_jwks_age_seconds";

#[tokio::test]
async fn a_successful_refresh_counts_both_fetches_and_resets_the_age() {
    let capture = Capture::default();
    let _guard = capture.install();
    let env = TestEnv::start().await;
    env.provider.refresh_now().await.unwrap();
    assert_eq!(capture.counter(DISCOVERY_OK), 1);
    assert_eq!(capture.counter(JWKS_OK), 1);
    assert_eq!(capture.gauge(AGE), Some(0.0));
    assert_eq!(capture.counter(DISCOVERY_ERROR), 0);
    assert_eq!(capture.counter(JWKS_ERROR), 0);
}

#[tokio::test]
async fn a_failed_discovery_is_counted_as_an_error() {
    let capture = Capture::default();
    let _guard = capture.install();
    let env = TestEnv::start().await;
    env.mock.set_down(true);
    env.provider.refresh_now().await.unwrap_err();
    assert_eq!(capture.counter(DISCOVERY_ERROR), 1);
    assert_eq!(capture.counter(DISCOVERY_OK), 0);
    assert_eq!(capture.counter(JWKS_OK), 0);
    env.mock.set_down(false);
    env.provider.refresh_now().await.unwrap();
    assert_eq!(capture.counter(DISCOVERY_OK), 1);
}

#[tokio::test]
async fn a_failed_jwks_fetch_is_counted_as_an_error_and_leaves_the_age_alone() {
    let capture = Capture::default();
    let _guard = capture.install();
    let env =
        TestEnv::start_with(|c| c.jwks_min_refresh_interval = Duration::from_millis(300)).await;
    env.provider.refresh_now().await.unwrap();
    assert_eq!(capture.gauge(AGE), Some(0.0));
    tokio::time::sleep(Duration::from_millis(350)).await;
    env.mock.set_down(true);
    // An unknown `kid` makes the node ask Keycloak for the keys, which fails.
    let unknown = keys().attacker_ec.sign_with_header(
        json!({"kid": "never-published"}),
        &access_claims(&env.issuer()),
    );
    let _ = env.authenticate(&unknown).await;
    assert_eq!(capture.counter(JWKS_ERROR), 1);
    assert_eq!(capture.counter(JWKS_OK), 1);
    assert_eq!(
        capture.gauge(AGE),
        Some(0.0),
        "a failure does not reset the age"
    );
}

#[tokio::test]
async fn the_age_keeps_growing_while_keycloak_is_unreachable_from_the_start() {
    let capture = Capture::default();
    let _guard = capture.install();
    let env = TestEnv::start_with(|c| c.discovery_retry = Duration::from_secs(1)).await;
    env.mock.kill();
    let shutdown = CancellationToken::new();
    let task = env.provider.spawn_background(shutdown.clone());
    tokio::time::sleep(Duration::from_millis(1300)).await;
    shutdown.cancel();
    task.await.unwrap();
    // With no key set ever fetched the age counts from the start of the node: without a series
    // the stale alert could never fire on exactly the node that cannot reach Keycloak.
    let age = capture.gauge(AGE).expect("the gauge exists");
    assert!(age >= 1.0, "{age}");
    assert!(capture.counter(DISCOVERY_ERROR) >= 2);
    assert_eq!(capture.counter(JWKS_OK), 0);
}

#[tokio::test]
async fn the_background_loop_publishes_the_age_of_a_key_set_that_works() {
    let capture = Capture::default();
    let _guard = capture.install();
    let env = TestEnv::start_with(|c| c.discovery_retry = Duration::from_secs(1)).await;
    env.provider.refresh_now().await.unwrap();
    let shutdown = CancellationToken::new();
    let task = env.provider.spawn_background(shutdown.clone());
    tokio::time::sleep(Duration::from_millis(100)).await;
    shutdown.cancel();
    task.await.unwrap();
    let age = capture.gauge(AGE).expect("the gauge exists");
    assert!((0.0..5.0).contains(&age), "{age}");
}

#[tokio::test]
async fn a_request_that_times_out_is_counted_as_a_timeout() {
    // A server that accepts the connection and never speaks: the TLS handshake hangs until
    // the request timeout, which is what a Keycloak behind a dead firewall rule looks like.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let holder = tokio::spawn(async move {
        let mut held = Vec::new();
        while let Ok((stream, _)) = listener.accept().await {
            held.push(stream);
        }
    });
    let capture = Capture::default();
    let _guard = capture.install();
    let mut config = OidcConfig::new(format!("https://127.0.0.1:{port}/realms/test"), AUDIENCE);
    config.connect_timeout = Duration::from_millis(200);
    config.request_timeout = Duration::from_millis(400);
    let provider = OidcProvider::new_for_tests(config, Arc::new(SystemClock)).unwrap();
    provider.refresh_now().await.unwrap_err();
    assert_eq!(capture.counter(DISCOVERY_TIMEOUT), 1);
    assert_eq!(capture.counter(DISCOVERY_ERROR), 0);
    holder.abort();
}
