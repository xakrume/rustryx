//! Credential extraction and the authentication middleware.

use std::net::SocketAddr;
use std::sync::Arc;

use axum::extract::ConnectInfo;
use base64::Engine;
use base64::engine::general_purpose::{STANDARD, STANDARD_NO_PAD};
use http::{HeaderMap, HeaderValue, Method, StatusCode};
use rustryx_core::AuthError;
use rustryx_core::identity::{AuthMethod, Credentials};
use rustryx_core::ports::Decision;

use super::*;
use crate::Challenge;
use crate::test_support::{
    StubAuth, StubRbac, call, get_request, get_uri, principal, ready_state, request, test_router,
};

fn headers(value: &str) -> HeaderMap {
    let mut h = HeaderMap::new();
    h.insert(AUTHORIZATION, HeaderValue::from_str(value).expect("header"));
    h
}

fn basic_header(user: &str, password: &str) -> String {
    format!("Basic {}", STANDARD.encode(format!("{user}:{password}")))
}

fn with_auth(uri: &str, value: &str) -> axum::http::Request<axum::body::Body> {
    request(Method::GET, uri)
        .header("authorization", value)
        .body(axum::body::Body::empty())
        .expect("request")
}

// ---- parsing --------------------------------------------------------------

#[test]
fn no_header_is_anonymous() {
    assert!(matches!(
        credentials_from_headers(&HeaderMap::new()),
        Ok(Credentials::Anonymous)
    ));
}

#[test]
fn basic_credentials_are_decoded_with_or_without_padding() {
    for encoded in [
        STANDARD.encode("alice:s3cret"),
        STANDARD_NO_PAD.encode("alice:s3cret"),
    ] {
        let parsed =
            credentials_from_headers(&headers(&format!("Basic {encoded}"))).expect("parses");
        match parsed {
            Credentials::Basic { username, password } => {
                assert_eq!(username, "alice");
                assert_eq!(password.expose(), "s3cret");
            }
            other => panic!("unexpected {other:?}"),
        }
    }
}

#[test]
fn password_may_contain_colons_and_unicode() {
    let parsed =
        credentials_from_headers(&headers(&basic_header("bob", "p:a:ss\u{e9}"))).expect("parses");
    match parsed {
        Credentials::Basic { password, .. } => assert_eq!(password.expose(), "p:a:ss\u{e9}"),
        other => panic!("unexpected {other:?}"),
    }
}

#[test]
fn scheme_is_case_insensitive_and_bearer_tokens_are_kept_verbatim() {
    for value in [
        "bearer ryx_abc.DEF-1_2~3+4/5=",
        "BEARER ryx_abc.DEF-1_2~3+4/5=",
    ] {
        match credentials_from_headers(&headers(value)).expect("parses") {
            Credentials::Bearer(token) => assert_eq!(token.expose(), "ryx_abc.DEF-1_2~3+4/5="),
            other => panic!("unexpected {other:?}"),
        }
    }
    assert!(matches!(
        credentials_from_headers(&headers(&basic_header("u", "p").replace("Basic", "bAsIc"))),
        Ok(Credentials::Basic { .. })
    ));
}

#[test]
fn must_not_accept_unusable_authorization_headers() {
    let nul = format!("Basic {}", STANDARD.encode("user:pa\0ss"));
    let blank = basic_header("user", "   ");
    let empty = basic_header("user", "");
    let no_colon = format!("Basic {}", STANDARD.encode("justausername"));
    let not_utf8 = format!("Basic {}", STANDARD.encode([0xff_u8, b':', 0xfe]));
    let long_user = basic_header(&"u".repeat(300), "pw");
    let long_pass = basic_header("user", &"p".repeat(5000));
    let huge = format!("Bearer {}", "a".repeat(17 * 1024));
    for bad in [
        "Digest username=\"x\"",
        "Negotiate abc",
        "Token abc",
        "Basic",
        "Basic ",
        "Basic !!!not-base64!!!",
        "Bearer",
        "Bearer ",
        "Bearer has space",
        "Bearer bad\u{7f}char",
        "Bearer semi;colon",
        "Bearer quote\"",
        "justonetoken",
        nul.as_str(),
        blank.as_str(),
        empty.as_str(),
        no_colon.as_str(),
        not_utf8.as_str(),
        long_user.as_str(),
        long_pass.as_str(),
        huge.as_str(),
    ] {
        let Ok(value) = HeaderValue::from_str(bad) else {
            continue;
        };
        let mut h = HeaderMap::new();
        h.insert(AUTHORIZATION, value);
        assert!(
            matches!(
                credentials_from_headers(&h),
                Err(AuthError::InvalidCredentials)
            ),
            "{bad:?} must be refused"
        );
    }
}

#[test]
fn must_not_accept_non_ascii_header_bytes() {
    let mut h = HeaderMap::new();
    h.insert(
        AUTHORIZATION,
        HeaderValue::from_bytes(b"Bearer \xff\xfe").expect("opaque"),
    );
    assert!(credentials_from_headers(&h).is_err());
}

#[test]
fn must_not_accept_several_authorization_headers() {
    let mut h = HeaderMap::new();
    h.append(AUTHORIZATION, HeaderValue::from_static("Bearer first"));
    h.append(AUTHORIZATION, HeaderValue::from_static("Bearer second"));
    assert!(credentials_from_headers(&h).is_err());
}

// ---- middleware -----------------------------------------------------------

#[tokio::test]
async fn anonymous_requests_reach_the_handler_without_calling_the_authenticator() {
    let auth = StubAuth::always(principal("never", AuthMethod::BasicLocal, "r"));
    let router = test_router(ready_state().with_authenticator(auth.clone()));
    let reply = get_uri(&router, "/api/whoami").await;
    assert_eq!(reply.status, StatusCode::OK);
    assert_eq!(reply.text(), "anonymous");
    assert!(auth.seen().is_empty());
}

#[tokio::test]
async fn basic_credentials_authenticate_and_the_principal_reaches_the_handler() {
    let auth = StubAuth::new(|credentials, _| match credentials {
        Credentials::Basic { username, password }
            if username == "alice" && password.expose() == "pw" =>
        {
            Ok(Some(principal(
                "alice",
                AuthMethod::BasicLocal,
                "nx-developer",
            )))
        }
        _ => Err(AuthError::InvalidCredentials),
    });
    let router = test_router(ready_state().with_authenticator(auth.clone()));
    let reply = call(
        &router,
        with_auth("/api/whoami", &basic_header("alice", "pw")),
    )
    .await;
    assert_eq!(reply.status, StatusCode::OK);
    assert_eq!(reply.text(), "alice");
    assert_eq!(auth.seen().len(), 1);
}

#[tokio::test]
async fn bearer_tokens_are_passed_to_the_authenticator() {
    let auth = StubAuth::new(|credentials, _| match credentials {
        Credentials::Bearer(token) if token.expose() == "ryx_token123" => Ok(Some(principal(
            "ci-bot",
            AuthMethod::ApiToken,
            "nx-developer",
        ))),
        _ => Err(AuthError::InvalidToken("unknown")),
    });
    let router = test_router(ready_state().with_authenticator(auth));
    let reply = call(&router, with_auth("/api/whoami", "Bearer ryx_token123")).await;
    assert_eq!(reply.text(), "ci-bot");
    let reply = call(&router, with_auth("/api/whoami", "Bearer ryx_other")).await;
    assert_eq!(reply.status, StatusCode::UNAUTHORIZED);
    assert_eq!(reply.error_code(), "invalid_token");
}

#[tokio::test]
async fn invalid_credentials_are_401_with_a_challenge_and_the_handler_does_not_run() {
    let auth = StubAuth::failing(|| AuthError::InvalidCredentials);
    let router = test_router(ready_state().with_authenticator(auth));
    let reply = call(
        &router,
        with_auth("/api/whoami", &basic_header("alice", "wrong")),
    )
    .await;
    assert_eq!(reply.status, StatusCode::UNAUTHORIZED);
    assert_eq!(reply.error_code(), "invalid_credentials");
    assert!(
        reply
            .header("www-authenticate")
            .expect("challenge")
            .starts_with("Basic realm=")
    );
    assert!(
        !reply.text().contains("alice"),
        "the account name is not echoed"
    );
}

#[tokio::test]
async fn a_chain_that_recognises_nothing_is_401() {
    let auth = StubAuth::new(|_, _| Ok(None));
    let router = test_router(ready_state().with_authenticator(auth));
    let reply = call(&router, with_auth("/api/whoami", "Bearer some.token.value")).await;
    assert_eq!(reply.status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn provider_outage_overload_and_lockout_are_not_401() {
    for (make, status, code) in [
        (
            (|| AuthError::ProviderUnavailable("keycloak".into())) as fn() -> AuthError,
            StatusCode::SERVICE_UNAVAILABLE,
            "identity_provider_unavailable",
        ),
        (
            || AuthError::Overloaded,
            StatusCode::SERVICE_UNAVAILABLE,
            "overloaded",
        ),
        (
            || AuthError::LockedOut {
                retry_after: std::time::Duration::from_secs(60),
            },
            StatusCode::TOO_MANY_REQUESTS,
            "locked_out",
        ),
        (
            || AuthError::GuardUnavailable,
            StatusCode::SERVICE_UNAVAILABLE,
            "login_unavailable",
        ),
    ] {
        let router = test_router(ready_state().with_authenticator(StubAuth::failing(make)));
        let reply = call(&router, with_auth("/api/whoami", &basic_header("a", "b"))).await;
        assert_eq!((reply.status, reply.error_code().as_str()), (status, code));
        assert!(reply.header("retry-after").is_some());
    }
}

#[tokio::test]
async fn must_not_accept_credentials_when_no_authenticator_is_wired() {
    let router = test_router(ready_state());
    let reply = call(
        &router,
        with_auth("/api/whoami", &basic_header("admin", "admin123")),
    )
    .await;
    assert_eq!(reply.status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(reply.error_code(), "authentication_unavailable");
    let anonymous = get_uri(&router, "/api/whoami").await;
    assert_eq!(anonymous.status, StatusCode::OK);
}

#[tokio::test]
async fn must_not_accept_a_token_in_the_query_string() {
    let auth = StubAuth::always(principal("victim", AuthMethod::ApiToken, "nx-admin"));
    let router = test_router(ready_state().with_authenticator(auth.clone()));
    for uri in [
        "/api/whoami?access_token=ryx_secret123",
        "/api/whoami?token=ryx_secret123",
        "/api/whoami?password=x&username=admin",
    ] {
        let reply = get_uri(&router, uri).await;
        assert_eq!(reply.text(), "anonymous", "{uri}");
    }
    assert!(
        auth.seen().is_empty(),
        "no credential was extracted from a URL"
    );
}

#[tokio::test]
async fn must_not_treat_an_unknown_scheme_as_anonymous() {
    let auth = StubAuth::always(principal("x", AuthMethod::BasicLocal, "r"));
    let router = test_router(ready_state().with_authenticator(auth.clone()));
    let reply = call(
        &router,
        with_auth("/api/whoami", "Digest username=\"admin\""),
    )
    .await;
    assert_eq!(reply.status, StatusCode::UNAUTHORIZED);
    assert!(auth.seen().is_empty());
}

#[tokio::test]
async fn authenticator_receives_client_ip_and_request_id() {
    let auth = StubAuth::always(principal("alice", AuthMethod::BasicLocal, "r"));
    let mut state = ready_state().with_authenticator(auth.clone());
    let mut settings = state.settings().clone();
    settings.trusted_proxies = vec!["10.0.0.0/8".parse().expect("cidr")];
    state = state.with_settings(settings);
    let router = test_router(state);

    let mut req = request(Method::GET, "/api/whoami")
        .header("authorization", basic_header("alice", "pw"))
        .header("x-forwarded-for", "198.51.100.7")
        .header("x-request-id", "req-abc-1")
        .body(axum::body::Body::empty())
        .expect("request");
    req.extensions_mut()
        .insert(ConnectInfo(SocketAddr::from(([10, 0, 0, 5], 5000))));
    let reply = call(&router, req).await;
    assert_eq!(reply.status, StatusCode::OK);
    let seen = auth.seen();
    assert_eq!(
        seen[0].1.client_ip,
        Some("198.51.100.7".parse().expect("ip"))
    );
    assert_eq!(seen[0].1.request_id.as_deref(), Some("req-abc-1"));
}

#[tokio::test]
async fn must_not_trust_forwarded_for_when_the_peer_is_not_a_proxy() {
    let auth = StubAuth::always(principal("alice", AuthMethod::BasicLocal, "r"));
    let router = test_router(ready_state().with_authenticator(auth.clone()));
    let mut req = request(Method::GET, "/api/whoami")
        .header("authorization", basic_header("alice", "pw"))
        .header("x-forwarded-for", "1.2.3.4")
        .body(axum::body::Body::empty())
        .expect("request");
    req.extensions_mut()
        .insert(ConnectInfo(SocketAddr::from(([203, 0, 113, 9], 5000))));
    let _ = call(&router, req).await;
    assert_eq!(
        auth.seen()[0].1.client_ip,
        Some("203.0.113.9".parse().expect("ip"))
    );
}

#[tokio::test]
async fn challenge_selector_decides_the_challenge_per_request() {
    let selector: crate::ChallengeSelector = Arc::new(|parts| {
        if parts.uri.path().starts_with("/repository/") {
            Challenge::bearer("pub")
        } else {
            Challenge::basic("rustryx")
        }
    });
    let auth = StubAuth::failing(|| AuthError::InvalidCredentials);
    let router = test_router(
        ready_state()
            .with_authenticator(auth)
            .with_challenge_selector(selector),
    );
    let pub_reply = call(
        &router,
        with_auth("/repository/pub-hosted/api/packages/x", "Bearer badtoken"),
    )
    .await;
    assert_eq!(pub_reply.status, StatusCode::UNAUTHORIZED);
    assert_eq!(
        pub_reply.header("www-authenticate"),
        Some("Bearer realm=\"pub\"")
    );
    let api_reply = call(&router, with_auth("/api/whoami", "Bearer badtoken")).await;
    assert!(
        api_reply
            .header("www-authenticate")
            .expect("challenge")
            .starts_with("Basic")
    );
}

#[tokio::test]
async fn authenticated_extractor_rejects_anonymous_callers() {
    let auth = StubAuth::always(principal("alice", AuthMethod::BasicLocal, "r"));
    let router = test_router(ready_state().with_authenticator(auth));
    let anonymous = get_uri(&router, "/api/me").await;
    assert_eq!(anonymous.status, StatusCode::UNAUTHORIZED);
    assert!(anonymous.header("www-authenticate").is_some());
    let named = call(&router, with_auth("/api/me", &basic_header("alice", "pw"))).await;
    assert_eq!(named.text(), "alice");
}

// ---- authorize ------------------------------------------------------------

fn authz_router(
    rbac: Option<StubRbac>,
    anonymous_enabled: bool,
    authenticated: bool,
) -> axum::Router {
    let mut state =
        crate::test_support::state_with_settings(|s| s.anonymous_enabled = anonymous_enabled);
    if let Some(rbac) = rbac {
        state = state.with_rbac(Arc::new(rbac));
    }
    if authenticated {
        state = state.with_authenticator(StubAuth::always(principal(
            "alice",
            AuthMethod::BasicLocal,
            "r",
        )));
    }
    test_router(state)
}

async fn authz_status(router: &axum::Router, authenticated: bool) -> StatusCode {
    let req = if authenticated {
        with_auth("/api/authz", &basic_header("alice", "pw"))
    } else {
        get_request("/api/authz")
    };
    call(router, req).await.status
}

#[tokio::test]
async fn must_not_authorize_without_a_checker() {
    let router = authz_router(None, true, true);
    assert_eq!(
        authz_status(&router, true).await,
        StatusCode::SERVICE_UNAVAILABLE
    );
}

#[tokio::test]
async fn must_not_let_anonymous_callers_through_while_the_global_switch_is_off() {
    let router = authz_router(Some(StubRbac(Ok(Decision::Allow))), false, false);
    assert_eq!(authz_status(&router, false).await, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn authorize_maps_allow_deny_and_errors() {
    let allow = authz_router(Some(StubRbac(Ok(Decision::Allow))), true, true);
    assert_eq!(authz_status(&allow, true).await, StatusCode::OK);
    assert_eq!(
        authz_status(&allow, false).await,
        StatusCode::OK,
        "anonymous allowed when enabled and permitted"
    );

    let deny = authz_router(Some(StubRbac(Ok(Decision::Deny))), true, true);
    assert_eq!(authz_status(&deny, true).await, StatusCode::FORBIDDEN);
    assert_eq!(
        authz_status(&deny, false).await,
        StatusCode::UNAUTHORIZED,
        "anonymous is asked to authenticate"
    );

    let failing = authz_router(
        Some(StubRbac(Err(|| {
            AuthError::GroupLookupFailed("ldap down".into())
        }))),
        true,
        true,
    );
    assert_eq!(
        authz_status(&failing, true).await,
        StatusCode::FORBIDDEN,
        "fail closed"
    );
}

// ---- metrics --------------------------------------------------------------

#[tokio::test]
async fn authentication_outcomes_are_counted_by_method_and_result() {
    let auth = StubAuth::new(|credentials, _| match credentials {
        Credentials::Basic { username, .. } if username == "alice" => {
            Ok(Some(principal("alice", AuthMethod::BasicLocal, "r")))
        }
        Credentials::Basic { username, .. } if username == "down" => {
            Err(AuthError::ProviderUnavailable("x".into()))
        }
        _ => Err(AuthError::InvalidCredentials),
    });
    let router = test_router(ready_state().with_authenticator(auth));
    let _ = call(
        &router,
        with_auth("/api/whoami", &basic_header("alice", "pw")),
    )
    .await;
    let _ = call(
        &router,
        with_auth("/api/whoami", &basic_header("mallory", "pw")),
    )
    .await;
    let _ = call(
        &router,
        with_auth("/api/whoami", &basic_header("down", "pw")),
    )
    .await;
    let _ = call(&router, with_auth("/api/whoami", "Digest x")).await;
    let _ = get_uri(&router, "/api/whoami").await;
    let metrics = get_uri(&router, "/metrics").await.text();
    assert!(
        metrics.contains(r#"auth_attempts_total{method="local",result="ok"} 1"#),
        "{metrics}"
    );
    assert!(
        metrics.contains(r#"auth_attempts_total{method="unknown",result="denied"} 2"#),
        "{metrics}"
    );
    assert!(
        metrics.contains(r#"auth_attempts_total{method="unknown",result="error"} 1"#),
        "{metrics}"
    );
    assert!(
        !metrics.contains("anonymous"),
        "anonymous requests are not attempts"
    );
}
