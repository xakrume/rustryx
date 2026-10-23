//! Authorization code flow: URL, sealed cookie, callback checks, ID token validation.

use std::collections::HashMap;
use std::sync::Arc;

use rustryx_core::AuthError;
use serde_json::{Value, json};
use url::Url;

use crate::oidc::test_support::{
    CodeGrant, ManualClock, MockKeycloak, REDIRECT_URL, TestEnv, TokenMode, UI_CLIENT_ID,
    UI_CLIENT_SECRET, id_claims, keys, mint, now, ui_config, with,
};
use crate::oidc::{OidcIdentity, OidcLoginFlow};

/// A named way to spoil an ID token and the reason the flow must give for refusing it.
type IdTokenCase<'a> = (&'a str, Box<dyn Fn(&Started) -> String + 'a>, &'static str);

struct Started {
    params: HashMap<String, String>,
    url: Url,
    cookie: String,
    set_cookie: String,
}

async fn flow_env() -> TestEnv {
    let env = TestEnv::start_with(|c| c.ui = Some(ui_config())).await;
    env.provider.refresh_now().await.unwrap();
    env
}

fn flow(env: &TestEnv) -> OidcLoginFlow {
    env.provider.login_flow().unwrap()
}

fn begin(env: &TestEnv) -> Started {
    let start = flow(env).start().unwrap();
    let params: HashMap<String, String> =
        start.authorization_url.query_pairs().into_owned().collect();
    let cookie = start
        .set_cookie
        .split_once('=')
        .and_then(|(_, rest)| rest.split(';').next())
        .unwrap()
        .to_owned();
    Started {
        params,
        url: start.authorization_url,
        cookie,
        set_cookie: start.set_cookie,
    }
}

fn grant_for(env: &TestEnv, started: &Started, code: &str, id_token: String) {
    env.mock.grant(
        code,
        CodeGrant {
            code_challenge: started.params["code_challenge"].clone(),
            id_token,
            scope: "openid profile email".to_owned(),
        },
    );
}

fn good_id_token(env: &TestEnv, started: &Started) -> String {
    keys()
        .rsa
        .sign(&id_claims(&env.issuer(), &started.params["nonce"]))
}

async fn finish(env: &TestEnv, started: &Started, code: &str) -> Result<OidcIdentity, AuthError> {
    flow(env)
        .complete(code, &started.params["state"], &started.cookie)
        .await
}

fn assert_invalid(result: Result<OidcIdentity, AuthError>, reason: &'static str) {
    match result {
        Err(AuthError::InvalidToken(actual)) => assert_eq!(actual, reason),
        other => panic!("expected InvalidToken({reason}), got {other:?}"),
    }
}

#[tokio::test]
async fn start_builds_an_authorization_url_with_pkce_state_and_nonce() {
    let env = flow_env().await;
    let started = begin(&env);
    assert!(
        started
            .url
            .path()
            .ends_with("/protocol/openid-connect/auth")
    );
    let p = &started.params;
    assert_eq!(p["response_type"], "code");
    assert_eq!(p["client_id"], UI_CLIENT_ID);
    assert_eq!(p["redirect_uri"], REDIRECT_URL);
    assert_eq!(p["code_challenge_method"], "S256");
    assert_eq!(p["code_challenge"].len(), 43);
    assert!(
        p["state"].len() >= 22 && p["nonce"].len() >= 22,
        "at least 128 bits each"
    );
    let scopes: Vec<&str> = p["scope"].split(' ').collect();
    assert!(scopes.contains(&"openid") && scopes.contains(&"profile") && scopes.contains(&"email"));
    assert!(!scopes.contains(&"offline_access"));
    for forbidden in ["client_secret", "code_verifier", "password", "grant_type"] {
        assert!(!p.contains_key(forbidden), "{forbidden}");
    }
    assert!(!started.url.as_str().contains(UI_CLIENT_SECRET));
    assert_ne!(p["state"], p["nonce"]);
}

#[tokio::test]
async fn requested_scopes_follow_the_configuration_and_never_include_offline_access() {
    let mut ui = ui_config();
    ui.scopes = vec!["openid".into(), "email".into(), "groups".into()];
    let env = TestEnv::start_with(|c| c.ui = Some(ui)).await;
    env.provider.refresh_now().await.unwrap();
    let started = begin(&env);
    let mut scopes: Vec<&str> = started.params["scope"].split(' ').collect();
    scopes.sort_unstable();
    // `openid` once (the client adds it), no `profile` because the operator did not ask for it.
    assert_eq!(scopes, ["email", "groups", "openid"]);
}

#[tokio::test]
async fn start_issues_a_fresh_state_nonce_challenge_and_cookie_every_time() {
    let env = flow_env().await;
    let (a, b) = (begin(&env), begin(&env));
    for key in ["state", "nonce", "code_challenge"] {
        assert_ne!(a.params[key], b.params[key], "{key}");
    }
    assert_ne!(a.cookie, b.cookie);
}

#[tokio::test]
async fn flow_cookie_carries_the_security_attributes_and_hides_the_flow_secrets() {
    let env = flow_env().await;
    let started = begin(&env);
    let header = &started.set_cookie;
    assert!(header.starts_with("rustryx_oidc_flow=v1."));
    assert!(header.ends_with("; Max-Age=600; Path=/oidc/callback; HttpOnly; Secure; SameSite=Lax"));
    for secret in [&started.params["state"], &started.params["nonce"]] {
        assert!(
            !header.contains(secret.as_str()),
            "sealed, not merely encoded"
        );
    }
    assert!(flow(&env).clear_cookie().contains("Max-Age=0"));
    assert_eq!(flow(&env).cookie_name(), "rustryx_oidc_flow");
}

#[tokio::test]
async fn callback_exchanges_the_code_and_returns_the_identity() {
    let env = flow_env().await;
    let started = begin(&env);
    grant_for(&env, &started, "code-1", good_id_token(&env, &started));
    let identity = finish(&env, &started, "code-1").await.unwrap();
    assert_eq!(identity.username, "alice");
    assert_eq!(identity.subject, "5a0b1c2d-user-1");
    assert_eq!(identity.issuer, env.issuer());
    assert_eq!(identity.email.as_deref(), Some("alice@example.test"));
    assert_eq!(identity.groups, ["/rustryx/admins"]);
    // What the token endpoint saw: confidential client over Basic, PKCE verifier, exact redirect.
    let requests = env.mock.token_requests();
    assert_eq!(requests.len(), 1);
    assert!(requests[0].basic_auth_ok);
    let form = &requests[0].form;
    assert_eq!(form["grant_type"], "authorization_code");
    assert_eq!(form["redirect_uri"], REDIRECT_URL);
    assert!(form["code_verifier"].len() >= 43);
    assert!(!form.contains_key("client_secret"));
}

#[tokio::test]
async fn must_not_accept_state_mismatch() {
    let env = flow_env().await;
    let started = begin(&env);
    grant_for(&env, &started, "code-1", good_id_token(&env, &started));
    for state in ["", "forged", &format!("{}x", started.params["state"])] {
        let result = flow(&env).complete("code-1", state, &started.cookie).await;
        assert_invalid(result, "state mismatch");
    }
    // The state of another login attempt does not fit this browser's cookie.
    let other = begin(&env);
    let result = flow(&env)
        .complete("code-1", &other.params["state"], &started.cookie)
        .await;
    assert_invalid(result, "state mismatch");
    assert_eq!(env.mock.hits().token(), 0, "nothing may reach Keycloak");
}

#[tokio::test]
async fn must_not_accept_tampered_or_foreign_cookies() {
    let env = flow_env().await;
    let started = begin(&env);
    grant_for(&env, &started, "code-1", good_id_token(&env, &started));
    let mut flipped = started.cookie.clone().into_bytes();
    let middle = flipped.len() / 2;
    flipped[middle] = if flipped[middle] == b'A' { b'B' } else { b'A' };
    // Cutting characters off the end can break the canonical base64 form as well as the tag,
    // so both reasons are fine there: what matters is that no flow state comes out.
    let cases: [(String, &[&str]); 6] = [
        (
            String::from_utf8(flipped).unwrap(),
            &["flow cookie invalid"],
        ),
        (
            started.cookie[..started.cookie.len() - 4].to_owned(),
            &["flow cookie invalid", "flow cookie malformed"],
        ),
        (String::new(), &["flow cookie malformed"]),
        ("v1.AAAA".to_owned(), &["flow cookie malformed"]),
        ("not-a-cookie".to_owned(), &["flow cookie malformed"]),
        (
            format!("v2.{}", &started.cookie[3..]),
            &["flow cookie malformed"],
        ),
    ];
    for (cookie, reasons) in cases {
        let result = flow(&env)
            .complete("code-1", &started.params["state"], &cookie)
            .await;
        match result {
            Err(AuthError::InvalidToken(reason)) => assert!(reasons.contains(&reason), "{reason}"),
            other => panic!("expected a cookie error, got {other:?}"),
        }
    }
    assert_eq!(env.mock.hits().token(), 0);
}

#[tokio::test]
async fn must_not_accept_a_cookie_sealed_under_another_key_or_name() {
    let env = flow_env().await;
    let started = begin(&env);
    let mut other_ui = ui_config();
    other_ui.cookie_key = rustryx_core::Secret::new(hex::encode((9u8..41).collect::<Vec<_>>()));
    let other_key = TestEnv::start_with(|c| c.ui = Some(other_ui)).await;
    other_key.provider.refresh_now().await.unwrap();
    let foreign = begin(&other_key);
    let result = flow(&env)
        .complete("c", &foreign.params["state"], &foreign.cookie)
        .await;
    assert_invalid(result, "flow cookie invalid");
    let mut renamed_ui = ui_config();
    renamed_ui.cookie_name = "another_cookie".into();
    let renamed = TestEnv::start_with(|c| c.ui = Some(renamed_ui)).await;
    renamed.provider.refresh_now().await.unwrap();
    let result = flow(&renamed)
        .complete("c", &started.params["state"], &started.cookie)
        .await;
    assert_invalid(result, "flow cookie invalid");
}

#[tokio::test]
async fn must_not_accept_an_expired_cookie() {
    let clock = ManualClock::new(now());
    let env = TestEnv::with_mock(
        MockKeycloak::start().await,
        |c| c.ui = Some(ui_config()),
        clock.clone(),
    );
    env.provider.refresh_now().await.unwrap();
    let started = begin(&env);
    grant_for(&env, &started, "code-1", good_id_token(&env, &started));
    clock.advance(601);
    assert_invalid(
        finish(&env, &started, "code-1").await,
        "flow cookie expired",
    );
    assert_eq!(env.mock.hits().token(), 0);
}

#[tokio::test]
async fn cookie_is_still_valid_just_before_its_deadline() {
    let clock = ManualClock::new(now());
    let env = TestEnv::with_mock(
        MockKeycloak::start().await,
        |c| c.ui = Some(ui_config()),
        clock.clone(),
    );
    env.provider.refresh_now().await.unwrap();
    let started = begin(&env);
    grant_for(&env, &started, "code-1", good_id_token(&env, &started));
    clock.advance(599);
    assert!(finish(&env, &started, "code-1").await.is_ok());
}

#[tokio::test]
async fn must_not_accept_a_replayed_callback() {
    let env = flow_env().await;
    let started = begin(&env);
    grant_for(&env, &started, "code-1", good_id_token(&env, &started));
    assert!(finish(&env, &started, "code-1").await.is_ok());
    grant_for(&env, &started, "code-2", good_id_token(&env, &started));
    assert_invalid(finish(&env, &started, "code-2").await, "state already used");
    assert_eq!(
        env.mock.hits().token(),
        1,
        "the replay never reaches the token endpoint"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_callbacks_with_one_cookie_let_exactly_one_through() {
    let env = Arc::new(flow_env().await);
    let started = Arc::new(begin(&env));
    grant_for(&env, &started, "code-1", good_id_token(&env, &started));
    let mut tasks = Vec::new();
    for _ in 0..10 {
        let (env, started) = (Arc::clone(&env), Arc::clone(&started));
        tasks.push(tokio::spawn(async move {
            finish(&env, &started, "code-1").await.is_ok()
        }));
    }
    let mut accepted = 0;
    for task in tasks {
        accepted += usize::from(task.await.unwrap());
    }
    assert_eq!(accepted, 1);
    assert_eq!(env.mock.hits().token(), 1);
}

/// Every way an ID token can be wrong that the flow must catch.
fn spoiled_id_tokens(issuer: &str) -> Vec<IdTokenCase<'_>> {
    let nonce_of = |s: &Started| s.params["nonce"].clone();
    let sign = move |s: &Started, key: &str, value: Value| {
        keys()
            .rsa
            .sign(&with(id_claims(issuer, &nonce_of(s)), key, value))
    };
    vec![
        (
            "wrong nonce",
            Box::new(move |_| keys().rsa.sign(&id_claims(issuer, "another-nonce"))),
            "nonce mismatch",
        ),
        (
            "no nonce",
            Box::new(move |_| {
                keys()
                    .rsa
                    .sign(&with(id_claims(issuer, "x"), "nonce", Value::Null))
            }),
            "nonce mismatch",
        ),
        (
            "wrong audience",
            Box::new(move |s| sign(s, "aud", json!("other-client"))),
            "audience mismatch",
        ),
        (
            "wrong issuer",
            Box::new(move |s| sign(s, "iss", json!("https://evil.example/realms/test"))),
            "issuer mismatch",
        ),
        (
            "expired",
            Box::new(move |s| sign(s, "exp", json!(now() - 3600))),
            "token expired",
        ),
        (
            "access token as id token",
            Box::new(move |s| sign(s, "typ", json!("Bearer"))),
            "unexpected token type",
        ),
        (
            "foreign azp",
            Box::new(move |s| sign(s, "azp", json!("other"))),
            "authorized party mismatch",
        ),
        (
            "attacker signature",
            Box::new(move |s| {
                keys()
                    .attacker_rsa
                    .sign(&id_claims(issuer, &s.params["nonce"]))
            }),
            "signature verification failed",
        ),
        (
            "alg none",
            Box::new(move |s| {
                mint(
                    &json!({"alg": "none", "kid": "rsa-1"}),
                    &id_claims(issuer, &s.params["nonce"]),
                    None,
                )
            }),
            "algorithm not allowed",
        ),
    ]
}

#[tokio::test]
async fn must_not_accept_a_forged_or_misdirected_id_token() {
    let env = flow_env().await;
    let issuer = env.issuer();
    for (name, make, reason) in spoiled_id_tokens(&issuer) {
        let started = begin(&env);
        grant_for(&env, &started, "code-x", make(&started));
        let result = finish(&env, &started, "code-x").await;
        match result {
            Err(AuthError::InvalidToken(actual)) => assert_eq!(actual, reason, "{name}"),
            Err(AuthError::TokenExpired) if reason == "token expired" => {}
            other => panic!("{name}: expected {reason}, got {other:?}"),
        }
    }
}

#[tokio::test]
async fn an_opaque_id_token_is_an_unusable_provider_response() {
    let env = flow_env().await;
    let started = begin(&env);
    grant_for(&env, &started, "code-1", "opaque".to_owned());
    let result = finish(&env, &started, "code-1").await;
    assert!(
        matches!(result, Err(AuthError::ProviderUnavailable(_))),
        "{result:?}"
    );
}

#[tokio::test]
async fn must_not_accept_offline_scope_in_the_token_response() {
    let env = flow_env().await;
    let started = begin(&env);
    env.mock.grant(
        "code-1",
        CodeGrant {
            code_challenge: started.params["code_challenge"].clone(),
            id_token: good_id_token(&env, &started),
            scope: "openid offline_access".to_owned(),
        },
    );
    assert_invalid(
        finish(&env, &started, "code-1").await,
        "offline tokens are not accepted",
    );
}

#[tokio::test]
async fn token_endpoint_refusal_is_a_credentials_error() {
    let env = flow_env().await;
    let started = begin(&env);
    // No grant registered: Keycloak answers invalid_grant.
    let result = finish(&env, &started, "unknown-code").await;
    assert!(
        matches!(result, Err(AuthError::InvalidCredentials)),
        "{result:?}"
    );
}

#[tokio::test]
async fn must_not_exchange_a_code_with_a_wrong_pkce_verifier() {
    let env = flow_env().await;
    let started = begin(&env);
    env.mock.grant(
        "code-1",
        CodeGrant {
            code_challenge: "not-the-challenge-of-this-login".to_owned(),
            id_token: good_id_token(&env, &started),
            scope: "openid".to_owned(),
        },
    );
    let result = finish(&env, &started, "code-1").await;
    assert!(
        matches!(result, Err(AuthError::InvalidCredentials)),
        "{result:?}"
    );
}

#[tokio::test]
async fn keycloak_failures_at_callback_are_503_not_400() {
    for mode in [TokenMode::ServerError, TokenMode::Garbage] {
        let env = flow_env().await;
        let started = begin(&env);
        env.mock.set_token_mode(mode);
        let result = finish(&env, &started, "code-1").await;
        assert!(
            matches!(result, Err(AuthError::ProviderUnavailable(_))),
            "{result:?}"
        );
    }
    let env = flow_env().await;
    let started = begin(&env);
    env.mock.kill();
    let result = finish(&env, &started, "code-1").await;
    assert!(
        matches!(result, Err(AuthError::ProviderUnavailable(_))),
        "{result:?}"
    );
    assert!(!env.provider.status().reachable);
}

#[tokio::test]
async fn new_logins_get_503_while_keycloak_is_unreachable_and_nothing_else_breaks() {
    let env = flow_env().await;
    let bearer = env.access_token(&keys().rsa);
    env.mock.set_down(true);
    assert!(env.provider.refresh_now().await.is_err());
    let result = flow(&env).start();
    assert!(
        matches!(result, Err(AuthError::ProviderUnavailable(_))),
        "{result:?}"
    );
    // Bearer tokens keep working from the cached JWKS.
    assert!(env.authenticate(&bearer).await.unwrap().is_some());
    // Recovery: the next successful contact lets logins through again.
    env.mock.set_down(false);
    env.provider.refresh_now().await.unwrap();
    assert!(flow(&env).start().is_ok());
}

#[tokio::test]
async fn login_cannot_start_before_discovery_succeeded() {
    let env = TestEnv::start_with(|c| c.ui = Some(ui_config())).await;
    env.mock.kill();
    let result = flow(&env).start();
    assert!(
        matches!(result, Err(AuthError::ProviderUnavailable(_))),
        "{result:?}"
    );
}

#[tokio::test]
async fn must_not_accept_malformed_callback_parameters() {
    let env = flow_env().await;
    let started = begin(&env);
    let flow = flow(&env);
    for (code, state) in [("", "s"), (&"c".repeat(5000), "s"), ("c", &"s".repeat(600))] {
        let result = flow.complete(code, state, &started.cookie).await;
        assert!(matches!(
            result,
            Err(AuthError::InvalidToken("malformed callback"))
        ));
    }
}

#[tokio::test]
async fn no_login_flow_without_ui_configuration() {
    let env = TestEnv::start().await;
    assert!(env.provider.login_flow().is_none());
}

#[tokio::test]
async fn flow_debug_output_hides_secrets() {
    let env = flow_env().await;
    let shown = format!("{:?} {:?}", flow(&env), env.provider);
    assert!(!shown.contains(UI_CLIENT_SECRET));
    assert!(!shown.contains("000102"));
    assert!(shown.contains("REDACTED"));
}
