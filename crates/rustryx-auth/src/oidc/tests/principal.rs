//! What the bearer authenticator and the resolver hand to the chain: the profile and expiry
//! of the principal, the confirmation time of the groups, and the `azp` allow list.

use rustryx_core::AuthError;
use rustryx_core::identity::{AuthMethod, IdpProvider, Principal, UserSource};
use rustryx_core::ports::GroupResolver;
use serde_json::{Value, json};

use crate::oidc::OidcConfig;
use crate::oidc::OidcGroupResolver;
use crate::oidc::test_support::{TestEnv, access_claims, keys, with};

type AuthResult = Result<Option<Principal>, AuthError>;

fn sign(claims: &Value) -> String {
    keys().rsa.sign(claims)
}

fn assert_azp_refused(result: &AuthResult) {
    assert!(
        matches!(
            result,
            Err(AuthError::InvalidToken("authorized party mismatch"))
        ),
        "{result:?}"
    );
}

#[tokio::test]
async fn the_principal_carries_the_email_the_name_and_the_expiry_of_the_token() {
    let env = TestEnv::start_warm().await;
    let claims = with(access_claims(&env.issuer()), "name", json!("Alice Liddell"));
    let expected_exp = claims["exp"].as_i64().unwrap();
    let principal = env.authenticate(&sign(&claims)).await.unwrap().unwrap();
    assert_eq!(principal.email.as_deref(), Some("alice@example.test"));
    assert_eq!(principal.display_name.as_deref(), Some("Alice Liddell"));
    assert_eq!(
        principal
            .expires_at
            .map(time::OffsetDateTime::unix_timestamp),
        Some(expected_exp),
        "the principal cache is bounded by `exp`, which the principal now carries"
    );
    assert!(principal.external_id.is_none());
    assert_eq!(principal.source, UserSource::Oidc);
    assert_eq!(principal.auth_method, AuthMethod::OidcBearer);
}

#[tokio::test]
async fn a_token_without_email_or_name_gives_a_principal_without_them() {
    let env = TestEnv::start_warm().await;
    let claims = with(access_claims(&env.issuer()), "email", Value::Null);
    let principal = env.authenticate(&sign(&claims)).await.unwrap().unwrap();
    assert!(principal.email.is_none() && principal.display_name.is_none());
    let empty_name = with(access_claims(&env.issuer()), "name", json!(""));
    let principal = env.authenticate(&sign(&empty_name)).await.unwrap().unwrap();
    assert!(principal.display_name.is_none(), "an empty name is no name");
}

#[tokio::test]
async fn must_not_accept_a_name_with_control_characters() {
    let env = TestEnv::start_warm().await;
    let claims = with(access_claims(&env.issuer()), "name", json!("Al\u{0}ice\n"));
    let result = env.authenticate(&sign(&claims)).await;
    assert!(!matches!(result, Ok(Some(_))), "{result:?}");
}

#[tokio::test]
async fn must_not_read_the_groups_from_the_name_claim() {
    // `name` has a meaning of its own now, so it can no longer be configured as the groups claim.
    let mut config = OidcConfig::new("https://kc.example/realms/r", "rustryx");
    config.groups_claim = "name".into();
    assert!(config.validate().is_err());
}

#[tokio::test]
async fn the_resolver_reports_when_the_authenticator_confirmed_the_groups() {
    let env = TestEnv::start_warm().await;
    let principal = env
        .authenticate(&sign(&access_claims(&env.issuer())))
        .await
        .unwrap()
        .unwrap();
    let external = OidcGroupResolver::new()
        .external_groups(&principal)
        .await
        .unwrap();
    assert_eq!(external.provider, IdpProvider::Oidc);
    assert_eq!(external.groups, principal.external_groups);
    assert_eq!(
        Some(external.confirmed_at),
        principal.groups_confirmed_at,
        "the confirmation of the verified token, not a time the resolver makes up"
    );
}

#[tokio::test]
async fn must_not_stamp_a_confirmation_on_groups_that_were_never_confirmed() {
    let env = TestEnv::start_warm().await;
    let mut principal = env
        .authenticate(&sign(&access_claims(&env.issuer())))
        .await
        .unwrap()
        .unwrap();
    principal.groups_confirmed_at = None;
    let error = OidcGroupResolver::new()
        .external_groups(&principal)
        .await
        .unwrap_err();
    assert!(
        matches!(error, AuthError::GroupLookupFailed(_)),
        "{error:?}"
    );
}

fn with_parties(parties: &[&str]) -> impl FnOnce(&mut OidcConfig) {
    let parties: Vec<String> = parties.iter().map(|p| (*p).to_owned()).collect();
    move |config| config.authorized_parties = parties
}

#[tokio::test]
async fn every_client_of_the_allow_list_is_accepted() {
    let env = TestEnv::start_with(with_parties(&["rustryx", "rustryx-cli"])).await;
    env.provider.refresh_now().await.unwrap();
    for azp in ["rustryx", "rustryx-cli"] {
        let claims = with(access_claims(&env.issuer()), "azp", json!(azp));
        assert!(
            env.authenticate(&sign(&claims)).await.unwrap().is_some(),
            "{azp}"
        );
    }
}

#[tokio::test]
async fn must_not_accept_an_authorized_party_outside_the_allow_list() {
    let env = TestEnv::start_with(with_parties(&["rustryx", "rustryx-cli"])).await;
    env.provider.refresh_now().await.unwrap();
    for azp in [
        "rustryx-evil",
        "RUSTRYX",
        "rustryx-cli ",
        " rustryx",
        "rustryx-",
        "rustryx-ui",
        "account",
        "*",
        "",
    ] {
        let claims = with(access_claims(&env.issuer()), "azp", json!(azp));
        assert_azp_refused(&env.authenticate(&sign(&claims)).await);
    }
}

#[tokio::test]
async fn the_default_allow_list_is_only_the_product_client() {
    let env = TestEnv::start_warm().await;
    let cli = with(access_claims(&env.issuer()), "azp", json!("rustryx-cli"));
    assert_azp_refused(&env.authenticate(&sign(&cli)).await);
    let own = with(access_claims(&env.issuer()), "azp", json!("rustryx"));
    assert!(env.authenticate(&sign(&own)).await.unwrap().is_some());
}

#[test]
fn must_not_accept_a_malformed_allow_list() {
    for (parties, ok) in [
        (vec![], true),
        (vec!["rustryx"], true),
        (vec!["rustryx", "rustryx-cli"], true),
        (vec![""], false),
        (vec!["  "], false),
        (vec!["rustryx cli"], false),
        (vec!["rustryx\tcli"], false),
        (vec!["*"], false),
        (vec!["rustryx-*"], false),
    ] {
        let mut config = OidcConfig::new("https://kc.example/realms/r", "rustryx");
        config.authorized_parties = parties.iter().map(|p| (*p).to_owned()).collect();
        assert_eq!(config.validate().is_ok(), ok, "{parties:?}");
    }
}

#[test]
fn the_allow_list_is_what_the_validator_compares_against() {
    let mut config = OidcConfig::new("https://kc.example/realms/r", "rustryx");
    assert_eq!(config.azp_allowed(), ["rustryx"]);
    config.authorized_parties = vec!["rustryx".into(), "rustryx-cli".into()];
    assert_eq!(config.azp_allowed(), ["rustryx", "rustryx-cli"]);
}
