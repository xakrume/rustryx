//! Provider wiring: configuration errors, split horizon, status.

use std::sync::Arc;

use rustryx_core::AuthError;
use rustryx_core::Secret;
use rustryx_core::ports::SystemClock;

use crate::oidc::test_support::{MockKeycloak, TestEnv, keys, ui_config};
use crate::oidc::{OidcConfig, OidcInitError, OidcProvider};

#[test]
fn new_rejects_an_invalid_configuration_without_touching_the_network() {
    let mut config = OidcConfig::new("http://kc.example/realms/r", "account");
    config.clock_skew = std::time::Duration::from_secs(120);
    let error = OidcProvider::new(config).unwrap_err();
    match error {
        OidcInitError::Config(config) => assert_eq!(config.problems().len(), 3),
        other => panic!("unexpected {other:?}"),
    }
}

#[test]
fn new_rejects_an_unreadable_extra_ca() {
    let mut config = OidcConfig::new("https://kc.example/realms/r", "rustryx");
    config.extra_root_certs_pem =
        vec![b"-----BEGIN CERTIFICATE-----\n!!!\n-----END CERTIFICATE-----\n".to_vec()];
    assert!(matches!(
        OidcProvider::new(config),
        Err(OidcInitError::Http(_))
    ));
}

#[test]
fn new_accepts_a_valid_configuration_and_does_not_need_a_runtime() {
    let mut config = OidcConfig::new("https://kc.example/realms/r", "rustryx");
    config.ui = Some(ui_config());
    let provider = OidcProvider::with_clock(config, Arc::new(SystemClock)).unwrap();
    let status = provider.status();
    assert!(!status.discovery_loaded && !status.reachable);
    assert_eq!((status.jwks_keys, status.jwks_age), (0, None));
    assert!(provider.login_flow().is_some());
    assert!(format!("{provider:?}").contains("https://kc.example/realms/r"));
}

#[test]
fn new_reports_a_bad_flow_key() {
    let mut config = OidcConfig::new("https://kc.example/realms/r", "rustryx");
    let mut ui = ui_config();
    ui.cookie_key = Secret::new("zz");
    config.ui = Some(ui);
    assert!(matches!(
        OidcProvider::new(config),
        Err(OidcInitError::Config(_))
    ));
}

#[tokio::test]
async fn split_horizon_uses_the_public_issuer_for_tokens_and_the_internal_host_for_calls() {
    let mock = MockKeycloak::start_with_issuer(Some("https://sso.example.test")).await;
    let env = TestEnv::with_mock(mock, |c| c.ui = Some(ui_config()), Arc::new(SystemClock));
    // Discovery advertises endpoints on the public host, which is not reachable from here:
    // the node must rewrite them to the internal realm URL.
    env.provider.refresh_now().await.unwrap();
    assert_eq!(env.mock.hits().discovery(), 1);
    assert_eq!(env.mock.hits().jwks(), 1);
    assert_eq!(env.issuer(), "https://sso.example.test/realms/test");
    let token = env.access_token(&keys().rsa);
    let principal = env.authenticate(&token).await.unwrap().unwrap();
    assert_eq!(
        principal.issuer.as_deref(),
        Some("https://sso.example.test/realms/test")
    );
    // The browser is sent to the public host.
    let start = env.provider.login_flow().unwrap().start().unwrap();
    assert_eq!(start.authorization_url.host_str(), Some("sso.example.test"));
    // A token naming the internal URL as issuer is not accepted: `iss` is the public value only.
    let internal = keys().rsa.sign(&crate::oidc::test_support::access_claims(
        &env.mock.realm_url(),
    ));
    assert!(!matches!(env.authenticate(&internal).await, Ok(Some(_))));
}

#[tokio::test]
async fn status_counts_what_happened() {
    let env = TestEnv::start_warm().await;
    let status = env.provider.status();
    assert!(status.discovery_loaded && status.reachable);
    assert_eq!(
        (
            status.jwks_keys,
            status.jwks_refresh_ok,
            status.jwks_refresh_err
        ),
        (2, 1, 0)
    );
    assert!(status.discovery_last_success.is_some());
    assert_eq!(status.discovery_failures, 0);
    let result = env.authenticate("a.b.c").await;
    assert!(matches!(result, Ok(None)));
    let denied = env.provider.bearer_authenticator().validate("x.y.z").await;
    assert!(matches!(denied, Err(AuthError::InvalidToken(_))));
}
