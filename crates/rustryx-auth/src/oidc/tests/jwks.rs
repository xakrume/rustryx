//! JWKS cache: discovery failure at start, refresh limits, rotation, outage, TLS.

use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use rustryx_core::AuthError;
use serde_json::json;
use tokio_util::sync::CancellationToken;

use crate::oidc::test_support::{MockKeycloak, TestEnv, access_claims, keys, now, with};

const FLOOD: usize = 200;

fn flood_token(env: &TestEnv, n: usize) -> String {
    // A key the realm never published, under a fresh kid per request.
    let attacker = &keys().attacker_ec;
    attacker.sign_with_header(
        json!({"kid": format!("random-{n}")}),
        &access_claims(&env.issuer()),
    )
}

/// Tokens are minted before the clock starts so that signing time does not eat the window.
fn flood_tokens(env: &TestEnv) -> Vec<String> {
    (0..FLOOD).map(|n| flood_token(env, n)).collect()
}

async fn flood(env: &Arc<TestEnv>, tokens: &[String]) -> Vec<Result<bool, String>> {
    let mut tasks = Vec::new();
    for token in tokens {
        let env = Arc::clone(env);
        let token = token.clone();
        tasks.push(tokio::spawn(async move {
            match env.authenticate(&token).await {
                Ok(principal) => Ok(principal.is_some()),
                Err(error) => Err(error.to_string()),
            }
        }));
    }
    let mut results = Vec::new();
    for task in tasks {
        results.push(task.await.unwrap());
    }
    results
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn must_not_refresh_jwks_more_than_once_per_window_for_unknown_kid() {
    let env = Arc::new(TestEnv::start_warm().await);
    assert_eq!(env.mock.hits().jwks(), 1);
    let results = flood(&env, &flood_tokens(&env)).await;
    assert!(
        results
            .iter()
            .all(|r| r == &Err("invalid token: signing key unknown".to_owned()))
    );
    // The warm-up fetch took the window: 200 unknown kids add nothing.
    assert_eq!(env.mock.hits().jwks(), 1);
    assert_eq!(env.provider.status().jwks_forced_throttled, FLOOD as u64);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn must_not_refresh_more_than_once_per_window_under_concurrent_unknown_kids() {
    // A short window makes the later rounds observable; production floors it at 30 s.
    let env = Arc::new(
        TestEnv::start_with(|c| c.jwks_min_refresh_interval = Duration::from_millis(2000)).await,
    );
    env.provider.refresh_now().await.unwrap();
    env.mock.state.jwks_delay_ms.store(100, Ordering::SeqCst);
    let tokens = flood_tokens(&env);
    let before = env.mock.hits().jwks();
    tokio::time::sleep(Duration::from_millis(2100)).await;
    flood(&env, &tokens).await;
    assert_eq!(
        env.mock.hits().jwks() - before,
        1,
        "one refresh for 200 unknown kids"
    );
    flood(&env, &tokens).await;
    assert_eq!(
        env.mock.hits().jwks() - before,
        1,
        "second flood is inside the window"
    );
    tokio::time::sleep(Duration::from_millis(2100)).await;
    flood(&env, &tokens).await;
    assert_eq!(
        env.mock.hits().jwks() - before,
        2,
        "the window reopened exactly once"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_requests_with_a_rotated_key_share_one_refresh() {
    let env = Arc::new(
        TestEnv::start_with(|c| c.jwks_min_refresh_interval = Duration::from_millis(300)).await,
    );
    env.provider.refresh_now().await.unwrap();
    tokio::time::sleep(Duration::from_millis(350)).await;
    env.mock
        .set_jwks(&[&keys().rsa, &keys().rsa_rotated, &keys().ec]);
    env.mock.state.jwks_delay_ms.store(150, Ordering::SeqCst);
    let before = env.mock.hits().jwks();
    let tokens: Vec<String> = (0..20)
        .map(|_| env.access_token(&keys().rsa_rotated))
        .collect();
    let mut tasks = Vec::new();
    for token in tokens {
        let env = Arc::clone(&env);
        tasks.push(tokio::spawn(async move {
            env.authenticate(&token).await.map(|p| p.is_some())
        }));
    }
    for task in tasks {
        assert!(
            task.await.unwrap().unwrap(),
            "joiners must see the rotated key"
        );
    }
    assert_eq!(env.mock.hits().jwks() - before, 1);
}

#[tokio::test]
async fn must_not_accept_a_key_that_vanished_from_the_last_jwks() {
    let env =
        TestEnv::start_with(|c| c.jwks_min_refresh_interval = Duration::from_millis(300)).await;
    env.provider.refresh_now().await.unwrap();
    let old_token = env.access_token(&keys().rsa);
    assert!(env.authenticate(&old_token).await.unwrap().is_some());
    // Rotation: the realm drops the old RSA key and publishes a new one.
    env.mock.set_jwks(&[&keys().rsa_rotated, &keys().ec]);
    env.provider.refresh_now().await.unwrap();
    let result = env.authenticate(&old_token).await;
    assert!(
        matches!(result, Err(AuthError::InvalidToken("signing key unknown"))),
        "{result:?}"
    );
    assert!(
        env.authenticate(&env.access_token(&keys().rsa_rotated))
            .await
            .unwrap()
            .is_some()
    );
    assert_eq!(env.provider.status().jwks_keys, 2);
}

#[tokio::test]
async fn keycloak_outage_uses_cached_jwks_but_still_checks_expiry() {
    let env = TestEnv::start_warm().await;
    let fresh = env.access_token(&keys().rsa);
    let expired = keys().rsa.sign(&with(
        access_claims(&env.issuer()),
        "exp",
        json!(now() - 600),
    ));
    let wrong_audience = keys().rsa.sign(&with(
        access_claims(&env.issuer()),
        "aud",
        json!(["account"]),
    ));
    env.mock.kill();
    // Cached keys keep unexpired tokens working.
    assert!(env.authenticate(&fresh).await.unwrap().is_some());
    // exp, aud and signature are still enforced on every request.
    assert!(matches!(
        env.authenticate(&expired).await,
        Err(AuthError::TokenExpired)
    ));
    assert!(matches!(
        env.authenticate(&wrong_audience).await,
        Err(AuthError::InvalidToken("audience mismatch"))
    ));
    let forged = keys().attacker_rsa.sign(&access_claims(&env.issuer()));
    assert!(matches!(
        env.authenticate(&forged).await,
        Err(AuthError::InvalidToken("signature verification failed"))
    ));
}

#[tokio::test]
async fn unknown_kid_during_outage_is_a_401_not_a_503() {
    let env =
        TestEnv::start_with(|c| c.jwks_min_refresh_interval = Duration::from_millis(300)).await;
    env.provider.refresh_now().await.unwrap();
    tokio::time::sleep(Duration::from_millis(350)).await;
    env.mock.set_down(true);
    let result = env.authenticate(&flood_token(&env, 1)).await;
    assert!(
        matches!(result, Err(AuthError::InvalidToken("signing key unknown"))),
        "{result:?}"
    );
    // The failed attempt is counted but did not drop the keys.
    let status = env.provider.status();
    assert!(
        status.jwks_refresh_err >= 1 && status.jwks_keys == 2,
        "{status:?}"
    );
    assert!(!status.reachable);
    assert!(
        env.authenticate(&env.access_token(&keys().rsa))
            .await
            .unwrap()
            .is_some()
    );
}

#[tokio::test]
async fn startup_without_keycloak_is_a_state_not_an_error() {
    let env = TestEnv::start().await;
    env.mock.kill();
    let status = env.provider.status();
    assert!(!status.discovery_loaded && !status.reachable && status.jwks_age.is_none());
    assert!(env.provider.refresh_now().await.is_err());
    let status = env.provider.status();
    assert_eq!(status.discovery_failures, 1);
    assert!(!status.discovery_loaded);
    assert!(matches!(
        env.authenticate(&env.access_token(&keys().rsa)).await,
        Err(AuthError::ProviderUnavailable(_))
    ));
}

#[tokio::test]
async fn background_task_recovers_when_keycloak_appears() {
    let mock = MockKeycloak::start().await;
    mock.set_down(true);
    let env = TestEnv::with_mock(
        mock,
        |c| {
            c.discovery_retry = Duration::from_secs(1);
        },
        Arc::new(rustryx_core::ports::SystemClock),
    );
    let shutdown = CancellationToken::new();
    let task = env.provider.spawn_background(shutdown.clone());
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(!env.provider.status().discovery_loaded);
    env.mock.set_down(false);
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while env.provider.status().jwks_keys == 0 {
        assert!(
            tokio::time::Instant::now() < deadline,
            "background refresh never succeeded"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let status = env.provider.status();
    assert!(status.discovery_loaded && status.reachable && status.jwks_age.is_some());
    assert!(status.discovery_last_success.is_some());
    shutdown.cancel();
    task.await.unwrap();
}

#[tokio::test]
async fn reachable_recovers_after_a_failed_forced_refresh() {
    let env = TestEnv::start_with(|c| {
        c.jwks_min_refresh_interval = Duration::from_millis(300);
        c.discovery_retry = Duration::from_secs(1);
    })
    .await;
    env.provider.refresh_now().await.unwrap();
    let shutdown = CancellationToken::new();
    let task = env.provider.spawn_background(shutdown.clone());
    tokio::time::sleep(Duration::from_millis(350)).await;
    env.mock.set_down(true);
    // An unknown kid makes the node try the JWKS, which fails and marks Keycloak unreachable.
    let _ = env.authenticate(&flood_token(&env, 1)).await;
    assert!(!env.provider.status().reachable);
    env.mock.set_down(false);
    let deadline = tokio::time::Instant::now() + Duration::from_secs(6);
    while !env.provider.status().reachable {
        assert!(
            tokio::time::Instant::now() < deadline,
            "reachable never recovered"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    shutdown.cancel();
    task.await.unwrap();
}

#[tokio::test]
async fn background_task_refreshes_jwks_after_its_ttl() {
    let env = TestEnv::start_with(|c| c.jwks_ttl = Duration::from_millis(300)).await;
    let shutdown = CancellationToken::new();
    let task = env.provider.spawn_background(shutdown.clone());
    let deadline = tokio::time::Instant::now() + Duration::from_secs(6);
    while env.mock.hits().jwks() < 3 {
        assert!(
            tokio::time::Instant::now() < deadline,
            "jwks was not refreshed repeatedly"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    shutdown.cancel();
    task.await.unwrap();
}

#[tokio::test]
async fn cache_control_max_age_wins_over_the_default_ttl_and_is_floored() {
    // Default TTL 300 ms would refresh constantly; the header says 1 s, which is floored to 30 s.
    let env = TestEnv::start_with(|c| c.jwks_ttl = Duration::from_millis(300)).await;
    env.mock
        .set_jwks_header("cache-control", "public, max-age=1");
    let shutdown = CancellationToken::new();
    let task = env.provider.spawn_background(shutdown.clone());
    tokio::time::sleep(Duration::from_millis(1500)).await;
    assert_eq!(
        env.mock.hits().jwks(),
        1,
        "TTL from Cache-Control, not from the default"
    );
    shutdown.cancel();
    task.await.unwrap();
}

#[tokio::test]
async fn must_not_trust_jwks_from_an_untrusted_certificate() {
    // No extra root: the mock's private CA is unknown to the platform verifier.
    let env = TestEnv::start_with(|c| c.extra_root_certs_pem.clear()).await;
    assert!(env.provider.refresh_now().await.is_err());
    assert_eq!(
        env.mock.hits().discovery(),
        0,
        "the TLS handshake must fail before any request"
    );
    let result = env.authenticate(&env.access_token(&keys().rsa)).await;
    assert!(
        matches!(result, Err(AuthError::ProviderUnavailable(_))),
        "{result:?}"
    );
    assert_eq!(env.provider.status().jwks_keys, 0);
}

#[tokio::test]
async fn must_not_accept_a_jwks_uri_over_plain_http() {
    let env = TestEnv::start().await;
    env.mock
        .state
        .discovery_http_jwks
        .store(true, Ordering::SeqCst);
    let error = env.provider.refresh_now().await.unwrap_err();
    assert!(error.contains("jwks_uri must be https"), "{error}");
    assert!(!env.provider.status().discovery_loaded);
}

#[tokio::test]
async fn must_not_accept_a_discovery_document_for_another_issuer() {
    // The provider expects a public issuer that the mock does not announce.
    let env = TestEnv::start_with(|c| {
        c.public_issuer_url = Some("https://sso.example.test/realms/other".into());
    })
    .await;
    let error = env.provider.refresh_now().await.unwrap_err();
    assert!(error.contains("issuer mismatch"), "{error}");
}

#[tokio::test]
async fn must_not_read_an_oversized_jwks_and_keeps_the_previous_keys() {
    let env = TestEnv::start_warm().await;
    env.mock
        .state
        .jwks_padding
        .store(2 * 1024 * 1024, Ordering::SeqCst);
    let error = env.provider.refresh_now().await.unwrap_err();
    assert!(error.contains("exceeds"), "{error}");
    assert_eq!(env.provider.status().jwks_keys, 2);
}

#[tokio::test]
async fn jwks_with_no_usable_key_keeps_the_previous_keys() {
    let env = TestEnv::start_warm().await;
    *env.mock.state.jwks.lock().unwrap() =
        json!({"keys": [{"kty": "oct", "kid": "h", "k": "c2VjcmV0"}]});
    assert!(env.provider.refresh_now().await.is_err());
    assert_eq!(env.provider.status().jwks_keys, 2);
    assert!(
        env.authenticate(&env.access_token(&keys().rsa))
            .await
            .unwrap()
            .is_some()
    );
}
