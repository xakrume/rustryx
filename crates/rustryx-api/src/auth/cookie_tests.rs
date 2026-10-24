//! The cookie session as a credential, and HTTP Basic over plain HTTP.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::extract::ConnectInfo;
use base64::Engine;
use http::{Method, StatusCode};
use rustryx_core::identity::{AuthMethod, Credentials};

use crate::session_support::{Browser, EnvBuilder, ORIGIN, advance};
use crate::test_support::{StubAuth, call, principal, ready_state, request, test_router};
use crate::{CSRF_COOKIE, SESSION_COOKIE};

const MINUTE: Duration = Duration::from_secs(60);

fn set_cookies(reply: &crate::test_support::Reply) -> Vec<String> {
    reply
        .headers
        .get_all("set-cookie")
        .iter()
        .filter_map(|v| v.to_str().ok().map(str::to_owned))
        .collect()
}

#[tokio::test]
async fn a_session_cookie_authenticates_a_safe_request() {
    let env = EnvBuilder::new().build().await;
    let browser = Browser::new(&env);
    browser.login_alice().await;
    let reply = browser.get("/api/whoami").await;
    assert_eq!(
        (reply.status, reply.text().as_str()),
        (StatusCode::OK, "alice")
    );
    let me = browser.get("/api/me").await;
    assert_eq!(
        me.text(),
        "alice",
        "the extractor that demands a caller accepts the cookie"
    );
}

#[tokio::test]
async fn without_a_cookie_the_request_is_anonymous() {
    let env = EnvBuilder::new().build().await;
    let reply = Browser::new(&env).get("/api/whoami").await;
    assert_eq!(reply.text(), "anonymous");
}

// ---- CSRF and Origin ---------------------------------------------------------------------

#[tokio::test]
async fn must_not_run_an_unsafe_request_authenticated_by_the_cookie_without_the_csrf_header() {
    let env = EnvBuilder::new().build().await;
    let browser = Browser::new(&env);
    browser.login_alice().await;
    let body = Some(serde_json::json!({"name": "x"}));
    let without = browser
        .send(Method::POST, "/api/echo", body.clone(), false)
        .await;
    assert_eq!(without.status, StatusCode::FORBIDDEN);
    assert_eq!(without.error_code(), "csrf_rejected");
    let with = browser.send(Method::POST, "/api/echo", body, true).await;
    assert_eq!(with.status, StatusCode::OK, "{}", with.text());
}

#[tokio::test]
async fn must_not_accept_a_wrong_or_foreign_csrf_token() {
    let env = EnvBuilder::new().build().await;
    let browser = Browser::new(&env);
    browser.login_alice().await;
    let bearer_of_other = {
        let other = Browser::new(&env);
        other.login_alice().await;
        other.cookie(CSRF_COOKIE).expect("csrf")
    };
    for token in ["", "garbage", bearer_of_other.as_str()] {
        let session = browser.cookie(SESSION_COOKIE).expect("session");
        let csrf = browser.cookie(CSRF_COOKIE).expect("csrf");
        let req = request(Method::POST, "/api/echo")
            .header(
                "cookie",
                format!("{SESSION_COOKIE}={session}; {CSRF_COOKIE}={csrf}"),
            )
            .header("x-csrf-token", token)
            .header("origin", ORIGIN)
            .header("content-type", "application/json")
            .body(Body::from(r#"{"name":"x"}"#))
            .expect("request");
        let reply = call(&env.router, req).await;
        assert_eq!(reply.error_code(), "csrf_rejected", "token {token:?}");
    }
}

#[tokio::test]
async fn must_not_accept_a_csrf_header_without_the_matching_cookie() {
    let env = EnvBuilder::new().build().await;
    let browser = Browser::new(&env);
    browser.login_alice().await;
    let session = browser.cookie(SESSION_COOKIE).expect("session");
    let csrf = browser.cookie(CSRF_COOKIE).expect("csrf");
    // The page knows the token but the browser did not send the cookie (Strict, cross site).
    let req = request(Method::POST, "/api/echo")
        .header("cookie", format!("{SESSION_COOKIE}={session}"))
        .header("x-csrf-token", csrf)
        .header("origin", ORIGIN)
        .header("content-type", "application/json")
        .body(Body::from(r#"{"name":"x"}"#))
        .expect("request");
    assert_eq!(call(&env.router, req).await.error_code(), "csrf_rejected");
}

#[tokio::test]
async fn must_not_accept_a_foreign_or_missing_origin_on_a_cookie_session_request() {
    let env = EnvBuilder::new().build().await;
    let browser = Browser::new(&env);
    browser.login_alice().await;
    let session = browser.cookie(SESSION_COOKIE).expect("session");
    let csrf = browser.cookie(CSRF_COOKIE).expect("csrf");
    for origin in [
        Some("https://evil.example.test"),
        Some("null"),
        Some("http://repo.example.test"),
        None,
    ] {
        let mut builder = request(Method::POST, "/api/echo")
            .header(
                "cookie",
                format!("{SESSION_COOKIE}={session}; {CSRF_COOKIE}={csrf}"),
            )
            .header("x-csrf-token", &csrf)
            .header("content-type", "application/json");
        if let Some(origin) = origin {
            builder = builder.header("origin", origin);
        }
        let reply = call(
            &env.router,
            builder
                .body(Body::from(r#"{"name":"x"}"#))
                .expect("request"),
        )
        .await;
        assert_eq!(reply.status, StatusCode::FORBIDDEN, "origin {origin:?}");
        assert_eq!(reply.error_code(), "csrf_rejected");
    }
}

#[tokio::test]
async fn safe_methods_need_no_csrf_token() {
    let env = EnvBuilder::new().build().await;
    let browser = Browser::new(&env);
    browser.login_alice().await;
    for method in [Method::GET, Method::HEAD] {
        let reply = browser
            .send(method.clone(), "/api/whoami", None, false)
            .await;
        assert_eq!(reply.status, StatusCode::OK, "{method}");
    }
}

#[tokio::test]
async fn a_bearer_credential_is_not_subject_to_csrf_and_wins_over_a_cookie() {
    let env = EnvBuilder::new().build().await;
    let browser = Browser::new(&env);
    browser.login_alice().await;
    let session = browser.cookie(SESSION_COOKIE).expect("session");
    let req = request(Method::POST, "/api/echo")
        .header("cookie", format!("{SESSION_COOKIE}={session}"))
        .header("authorization", "Bearer ryx_admin")
        .header("content-type", "application/json")
        .body(Body::from(r#"{"name":"x"}"#))
        .expect("request");
    let reply = call(&env.router, req).await;
    assert_eq!(
        reply.status,
        StatusCode::OK,
        "no CSRF token, no Origin: the cookie was not the credential"
    );
    let who = request(Method::GET, "/api/whoami")
        .header("cookie", format!("{SESSION_COOKIE}={session}"))
        .header("authorization", "Bearer ryx_admin")
        .body(Body::empty())
        .expect("request");
    assert_eq!(
        call(&env.router, who).await.text(),
        "admin",
        "the header is the credential, the cookie is ignored"
    );
}

// ---- idle and absolute limits ---------------------------------------------------------------

#[tokio::test]
async fn must_not_accept_a_session_that_was_idle_for_more_than_thirty_minutes() {
    let env = EnvBuilder::new().build().await;
    let browser = Browser::new(&env);
    browser.login_alice().await;
    advance(&env, MINUTE * 29);
    assert_eq!(browser.get("/api/whoami").await.text(), "alice");
    advance(&env, MINUTE * 31);
    let reply = browser.get("/api/whoami").await;
    assert_eq!(
        reply.text(),
        "anonymous",
        "the session ended: the request is anonymous"
    );
    let me = browser.get("/api/me").await;
    assert_eq!(me.status, StatusCode::UNAUTHORIZED);
    assert_eq!(
        me.error_code(),
        "token_expired",
        "and a handler that needs a caller says why"
    );
}

#[tokio::test]
async fn activity_keeps_the_session_alive_through_a_transparent_reissue() {
    let env = EnvBuilder::new().build().await;
    let browser = Browser::new(&env);
    browser.login_alice().await;
    let first = browser.cookie(SESSION_COOKIE).expect("session");
    // Ten minutes in: more than half of the idle time is left, nothing is reissued.
    advance(&env, MINUTE * 10);
    let quiet = browser.get("/api/whoami").await;
    assert_eq!(set_cookies(&quiet).len(), 0);
    // Twenty minutes in: less than half is left, the response carries a new cookie pair.
    advance(&env, MINUTE * 10);
    let reply = browser.get("/api/whoami").await;
    assert_eq!(reply.text(), "alice");
    let cookies = set_cookies(&reply);
    assert!(
        cookies
            .iter()
            .any(|c| c.starts_with("__Host-rustryx_session=")),
        "{cookies:?}"
    );
    assert!(
        cookies
            .iter()
            .any(|c| c.starts_with("__Host-rustryx_csrf="))
    );
    assert_ne!(browser.cookie(SESSION_COOKIE).expect("session"), first);
    // 45 minutes after login, 25 after the reissue: still alive although the first token's idle
    // time is long gone.
    advance(&env, MINUTE * 25);
    assert_eq!(browser.get("/api/whoami").await.text(), "alice");
}

#[tokio::test]
async fn must_not_let_activity_extend_the_session_past_eight_hours() {
    let env = EnvBuilder::new().build().await;
    let browser = Browser::new(&env);
    browser.login_alice().await;
    let mut last_alive = 0;
    for minute in (10..=540).step_by(10) {
        advance(&env, MINUTE * 10);
        if browser.get("/api/whoami").await.text() == "alice" {
            last_alive = minute;
        } else {
            break;
        }
    }
    assert!(
        (470..=480).contains(&last_alive),
        "the session ended at eight hours, not {last_alive} minutes"
    );
    assert_eq!(browser.get("/api/whoami").await.text(), "anonymous");
}

#[tokio::test]
async fn a_reissue_keeps_the_csrf_pair_consistent_for_the_next_unsafe_request() {
    let env = EnvBuilder::new().build().await;
    let browser = Browser::new(&env);
    browser.login_alice().await;
    advance(&env, MINUTE * 20);
    let _ = browser.get("/api/whoami").await; // reissues
    let reply = browser
        .send(
            Method::POST,
            "/api/echo",
            Some(serde_json::json!({"name": "x"})),
            true,
        )
        .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.text());
}

#[tokio::test]
async fn no_reissue_rides_on_a_response_that_sets_session_cookies_itself() {
    let env = EnvBuilder::new().build().await;
    let browser = Browser::new(&env);
    browser.login_alice().await;
    advance(&env, MINUTE * 20);
    let logout = browser.post("/api/v1/auth/logout", None).await;
    let cookies = set_cookies(&logout);
    let session_cookies: Vec<_> = cookies
        .iter()
        .filter(|c| c.starts_with("__Host-rustryx_session="))
        .collect();
    assert_eq!(
        session_cookies.len(),
        1,
        "only the clearing cookie: {cookies:?}"
    );
    assert!(session_cookies[0].contains("Max-Age=0"));
}

// ---- revocation, outages, stale cookies ------------------------------------------------------

#[tokio::test]
async fn must_not_accept_a_revoked_session() {
    let env = EnvBuilder::new().build().await;
    let browser = Browser::new(&env);
    browser.login_alice().await;
    let tid = env.auth.issued().last().cloned().expect("a token");
    env.auth.revoke_for_test(&tid);
    assert_eq!(browser.get("/api/whoami").await.text(), "anonymous");
}

#[tokio::test]
async fn a_dependency_failure_while_verifying_a_session_is_503_and_never_anonymous() {
    let env = EnvBuilder::new().build().await;
    let browser = Browser::new(&env);
    browser.login_alice().await;
    *env.auth.chain_unavailable.lock().expect("lock") = true;
    let reply = browser.get("/api/whoami").await;
    assert_eq!(reply.status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(reply.error_code(), "identity_provider_unavailable");
}

#[tokio::test]
async fn a_stale_cookie_does_not_stop_the_user_from_signing_in_again() {
    let env = EnvBuilder::new().build().await;
    let browser = Browser::new(&env);
    browser.login_alice().await;
    advance(&env, MINUTE * 60);
    // The expired cookie is still in the jar and goes along with the login request.
    let reply = browser.login("alice", "correct horse").await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.text());
    assert_eq!(browser.get("/api/whoami").await.text(), "alice");
}

#[tokio::test]
async fn must_not_accept_ambiguous_malformed_or_forged_session_cookies() {
    let env = EnvBuilder::new().build().await;
    let browser = Browser::new(&env);
    browser.login_alice().await;
    let good = browser.cookie(SESSION_COOKIE).expect("session");
    for cookie in [
        format!("{SESSION_COOKIE}={good}; {SESSION_COOKIE}={good}"),
        format!("{SESSION_COOKIE}=not-a-token"),
        format!("{SESSION_COOKIE}="),
        // A real looking token for an account nobody has.
        format!(
            "{SESSION_COOKIE}={}",
            crate::session::claims::forge(&serde_json::json!({
                "sub": rustryx_core::ids::UserId::generate().to_string(),
                "tid": "x", "iat": env.clock_unix(), "exp": env.clock_unix() + 1000,
            }))
        ),
    ] {
        let req = request(Method::GET, "/api/whoami")
            .header("cookie", cookie.clone())
            .body(Body::empty())
            .expect("request");
        assert_eq!(call(&env.router, req).await.text(), "anonymous", "{cookie}");
    }
}

#[tokio::test]
async fn a_session_cookie_means_nothing_when_no_session_manager_is_wired() {
    let state = ready_state().with_authenticator(StubAuth::always(principal(
        "x",
        AuthMethod::InternalJwt,
        "r",
    )));
    let router = test_router(state);
    let req = request(Method::GET, "/api/whoami")
        .header("cookie", format!("{SESSION_COOKIE}=a.b.c"))
        .body(Body::empty())
        .expect("request");
    assert_eq!(call(&router, req).await.text(), "anonymous");
}

#[tokio::test]
async fn a_cookie_session_without_an_authenticator_is_a_503() {
    let env = EnvBuilder::new().build().await;
    let browser = Browser::new(&env);
    browser.login_alice().await;
    let state = ready_state().with_sessions(env.state.sessions().expect("sessions").clone());
    let router = test_router(state);
    let session = browser.cookie(SESSION_COOKIE).expect("session");
    let req = request(Method::GET, "/api/whoami")
        .header("cookie", format!("{SESSION_COOKIE}={session}"))
        .body(Body::empty())
        .expect("request");
    let reply = call(&router, req).await;
    assert_eq!(reply.status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(reply.error_code(), "authentication_unavailable");
}

#[tokio::test]
async fn an_api_token_presented_as_a_cookie_is_not_a_session() {
    let env = EnvBuilder::new().build().await;
    let req = request(Method::GET, "/api/whoami")
        .header("cookie", format!("{SESSION_COOKIE}=ryx_admin"))
        .body(Body::empty())
        .expect("request");
    assert_eq!(call(&env.router, req).await.text(), "anonymous");
}

// ---- HTTP Basic over plain HTTP ----------------------------------------------------------------

fn basic(user: &str, password: &str) -> String {
    format!(
        "Basic {}",
        base64::engine::general_purpose::STANDARD.encode(format!("{user}:{password}"))
    )
}

fn guarded(trusted: &[&str], require_https: bool) -> (axum::Router, Arc<StubAuth>) {
    let auth = StubAuth::new(|credentials, _| match credentials {
        Credentials::Basic { username, .. } => {
            Ok(Some(principal(username, AuthMethod::BasicLocal, "r")))
        }
        _ => Ok(Some(principal("bearer", AuthMethod::ApiToken, "r"))),
    });
    let nets = trusted.iter().map(|n| n.parse().expect("cidr")).collect();
    let state = crate::test_support::state_with_settings(|s| {
        s.require_https = require_https;
        s.trusted_proxies = nets;
    })
    .with_authenticator(auth.clone());
    (test_router(state), auth)
}

fn from_peer(peer: [u8; 4], headers: &[(&str, String)]) -> http::Request<Body> {
    let mut builder = request(Method::GET, "/api/whoami");
    for (name, value) in headers {
        builder = builder.header(*name, value);
    }
    let mut req = builder.body(Body::empty()).expect("request");
    req.extensions_mut()
        .insert(ConnectInfo(SocketAddr::from((peer, 5000))));
    req
}

#[tokio::test]
async fn must_not_accept_basic_credentials_over_plain_http() {
    let (router, auth) = guarded(&["10.0.0.0/8"], true);
    // Directly, no proxy: plain HTTP.
    let direct = call(
        &router,
        from_peer([203, 0, 113, 9], &[("authorization", basic("alice", "pw"))]),
    )
    .await;
    assert_eq!(direct.status, StatusCode::FORBIDDEN);
    assert_eq!(direct.error_code(), "https_required");
    // Behind the trusted proxy, but the proxy says plain http.
    let plain = call(
        &router,
        from_peer(
            [10, 0, 0, 5],
            &[
                ("authorization", basic("alice", "pw")),
                ("x-forwarded-proto", "http".into()),
            ],
        ),
    )
    .await;
    assert_eq!(plain.error_code(), "https_required");
    assert!(auth.seen().is_empty(), "no authenticator saw the password");
}

#[tokio::test]
async fn must_not_believe_an_untrusted_peer_that_claims_to_be_https() {
    let (router, auth) = guarded(&["10.0.0.0/8"], true);
    let lying = call(
        &router,
        from_peer(
            [203, 0, 113, 9],
            &[
                ("authorization", basic("alice", "pw")),
                ("x-forwarded-proto", "https".into()),
            ],
        ),
    )
    .await;
    assert_eq!(lying.error_code(), "https_required");
    assert!(auth.seen().is_empty());
}

#[tokio::test]
async fn basic_credentials_pass_when_a_trusted_proxy_says_https() {
    let (router, auth) = guarded(&["10.0.0.0/8"], true);
    let secure = call(
        &router,
        from_peer(
            [10, 0, 0, 5],
            &[
                ("authorization", basic("alice", "pw")),
                ("x-forwarded-proto", "https".into()),
            ],
        ),
    )
    .await;
    assert_eq!(
        (secure.status, secure.text().as_str()),
        (StatusCode::OK, "alice")
    );
    assert_eq!(auth.seen().len(), 1);
    let rfc = call(
        &router,
        from_peer(
            [10, 0, 0, 5],
            &[
                ("authorization", basic("alice", "pw")),
                ("forwarded", "for=1.2.3.4;proto=https".into()),
            ],
        ),
    )
    .await;
    assert_eq!(rfc.status, StatusCode::OK);
}

#[tokio::test]
async fn the_guard_can_be_switched_off_for_local_work_and_never_touches_bearer_tokens() {
    let (off, _) = guarded(&[], false);
    let reply = call(
        &off,
        from_peer([127, 0, 0, 1], &[("authorization", basic("alice", "pw"))]),
    )
    .await;
    assert_eq!(reply.status, StatusCode::OK);
    let (on, _) = guarded(&[], true);
    let bearer = call(
        &on,
        from_peer(
            [127, 0, 0, 1],
            &[("authorization", "Bearer ryx_token123".into())],
        ),
    )
    .await;
    assert_eq!(
        bearer.status,
        StatusCode::OK,
        "tokens are not Basic credentials"
    );
    let anonymous = call(&on, from_peer([127, 0, 0, 1], &[])).await;
    assert_eq!(anonymous.text(), "anonymous");
}

#[tokio::test]
async fn a_refused_basic_attempt_is_counted_and_not_charged_as_a_guess() {
    let (router, _) = guarded(&["10.0.0.0/8"], true);
    for _ in 0..3 {
        let _ = call(
            &router,
            from_peer([203, 0, 113, 9], &[("authorization", basic("alice", "pw"))]),
        )
        .await;
    }
    let metrics = crate::test_support::get_uri(&router, "/metrics")
        .await
        .text();
    assert!(
        metrics.contains(r#"auth_attempts_total{method="unknown",result="denied"} 3"#),
        "{metrics}"
    );
    // 403 is not a 401: the per address guess limiter did not charge these.
    let healthy = call(
        &router,
        from_peer([203, 0, 113, 9], &[("authorization", basic("alice", "pw"))]),
    )
    .await;
    assert_eq!(healthy.status, StatusCode::FORBIDDEN, "still 403, not 429");
}
