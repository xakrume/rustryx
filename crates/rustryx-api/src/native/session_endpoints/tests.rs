use axum::body::Body;
use http::{Method, StatusCode};

use crate::session_support::{Browser, EnvBuilder, advance};
use crate::test_support::{call, request};
use crate::{CSRF_COOKIE, SESSION_COOKIE};

const MINUTE: std::time::Duration = std::time::Duration::from_secs(60);

fn set_cookies(reply: &crate::test_support::Reply) -> Vec<String> {
    reply
        .headers
        .get_all("set-cookie")
        .iter()
        .filter_map(|v| v.to_str().ok().map(str::to_owned))
        .collect()
}

// ---- GET /auth/session -----------------------------------------------------------------

#[tokio::test]
async fn an_anonymous_caller_is_told_there_is_no_session_and_what_the_limits_are() {
    let env = EnvBuilder::new().build().await;
    let reply = Browser::new(&env).get("/api/v1/auth/session").await;
    assert_eq!(
        reply.status,
        StatusCode::OK,
        "never a 401, the SPA asks this to learn the state"
    );
    let json = reply.json();
    assert_eq!(json["authenticated"], false);
    for null in [
        "user",
        "auth_method",
        "csrf_token",
        "issued_at",
        "idle_expires_at",
        "absolute_expires_at",
    ] {
        assert!(json[null].is_null(), "{null} is present and null");
    }
    assert_eq!(json["idle_timeout_secs"], 1800);
    assert_eq!(json["absolute_timeout_secs"], 28800);
}

#[tokio::test]
async fn a_cookie_session_reports_its_user_its_times_and_its_csrf_token() {
    let env = EnvBuilder::new().build().await;
    let browser = Browser::new(&env);
    browser.login_alice().await;
    let reply = browser.get("/api/v1/auth/session").await;
    let json = reply.json();
    assert_eq!(json["authenticated"], true);
    assert_eq!(json["user"]["username"], "alice");
    assert_eq!(json["auth_method"], "internal_jwt");
    assert_eq!(
        json["csrf_token"],
        browser.cookie(CSRF_COOKIE).expect("csrf").as_str()
    );
    assert_eq!(json["absolute_expires_at"], "2026-01-01T08:00:00Z");
    assert!(
        set_cookies(&reply).is_empty(),
        "the browser already holds the CSRF cookie"
    );
}

#[tokio::test]
async fn reading_the_session_does_not_extend_it_but_restores_a_missing_csrf_cookie() {
    let env = EnvBuilder::new().build().await;
    let browser = Browser::new(&env);
    browser.login_alice().await;
    let before = browser.get("/api/v1/auth/session").await.json();
    advance(&env, MINUTE * 10);
    let after = browser.get("/api/v1/auth/session").await.json();
    assert_eq!(
        before["idle_expires_at"], after["idle_expires_at"],
        "the idle end did not move"
    );
    let token = browser.cookie(CSRF_COOKIE).expect("csrf");
    // The browser lost the CSRF cookie (cleared by the user): the next read hands it back.
    let session = browser.cookie(SESSION_COOKIE).expect("session");
    let req = request(Method::GET, "/api/v1/auth/session")
        .header("cookie", format!("{SESSION_COOKIE}={session}"))
        .body(Body::empty())
        .expect("request");
    let reply = call(&env.router, req).await;
    assert!(
        set_cookies(&reply)
            .iter()
            .any(|c| c.starts_with(&format!("{CSRF_COOKIE}={token};"))),
        "{:?}",
        set_cookies(&reply)
    );
}

#[tokio::test]
async fn a_bearer_caller_is_authenticated_and_has_no_csrf_token() {
    let env = EnvBuilder::new().build().await;
    let req = request(Method::GET, "/api/v1/auth/session")
        .header("authorization", "Bearer ryx_alice")
        .body(Body::empty())
        .expect("request");
    let json = call(&env.router, req).await.json();
    assert_eq!(json["authenticated"], true);
    assert_eq!(json["auth_method"], "api_token");
    assert!(json["csrf_token"].is_null());
    assert!(json["issued_at"].is_null() && json["idle_expires_at"].is_null());
}

#[tokio::test]
async fn an_expired_session_reads_as_unauthenticated() {
    let env = EnvBuilder::new().build().await;
    let browser = Browser::new(&env);
    browser.login_alice().await;
    advance(&env, MINUTE * 31);
    let json = browser.get("/api/v1/auth/session").await.json();
    assert_eq!(json["authenticated"], false);
}

// ---- POST /auth/session/refresh --------------------------------------------------------

#[tokio::test]
async fn refreshing_restarts_the_idle_timer_and_never_moves_the_absolute_end() {
    let env = EnvBuilder::new().build().await;
    let browser = Browser::new(&env);
    let login = browser.login_alice().await.json();
    advance(&env, MINUTE * 20);
    let old_session = browser.cookie(SESSION_COOKIE).expect("session");
    let reply = browser.post("/api/v1/auth/session/refresh", None).await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.text());
    let json = reply.json();
    assert_eq!(
        json["absolute_expires_at"], login["absolute_expires_at"],
        "never moves"
    );
    assert_eq!(json["idle_expires_at"], "2026-01-01T00:50:00Z");
    assert_eq!(json["issued_at"], "2026-01-01T00:20:00Z");
    assert_ne!(
        browser.cookie(SESSION_COOKIE).expect("session"),
        old_session
    );
    assert_eq!(
        json["csrf_token"],
        browser.cookie(CSRF_COOKIE).expect("csrf").as_str()
    );
    // Immediately usable with the new pair.
    let echo = browser
        .send(
            Method::POST,
            "/api/echo",
            Some(serde_json::json!({"name": "x"})),
            true,
        )
        .await;
    assert_eq!(echo.status, StatusCode::OK);
}

#[tokio::test]
async fn must_not_refresh_without_a_csrf_token_for_an_api_token_or_an_expired_session() {
    let env = EnvBuilder::new().build().await;
    let browser = Browser::new(&env);
    browser.login_alice().await;
    let forged = browser
        .send(Method::POST, "/api/v1/auth/session/refresh", None, false)
        .await;
    assert_eq!(
        (forged.status, forged.error_code().as_str()),
        (StatusCode::FORBIDDEN, "csrf_rejected")
    );

    let token = request(Method::POST, "/api/v1/auth/session/refresh")
        .header("authorization", "Bearer ryx_alice")
        .body(Body::empty())
        .expect("request");
    let reply = call(&env.router, token).await;
    assert_eq!(
        reply.status,
        StatusCode::FORBIDDEN,
        "a leaked token cannot make itself permanent"
    );

    advance(&env, MINUTE * 31);
    let late = browser.post("/api/v1/auth/session/refresh", None).await;
    assert_eq!(
        late.status,
        StatusCode::UNAUTHORIZED,
        "an ended session cannot be revived"
    );
    assert_eq!(late.error_code(), "token_expired");
    let anonymous = Browser::new(&env)
        .post("/api/v1/auth/session/refresh", None)
        .await;
    assert_eq!(anonymous.status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn a_bearer_session_has_nothing_to_refresh_and_is_told_its_state() {
    let mut builder = EnvBuilder::new();
    builder.oidc = true;
    let env = builder.build().await;
    // An internal JWT presented as a bearer: interactive, but there is no cookie to renew.
    let browser = Browser::new(&env);
    browser.login_alice().await;
    let jwt = browser.cookie(SESSION_COOKIE).expect("session");
    let req = request(Method::POST, "/api/v1/auth/session/refresh")
        .header("authorization", format!("Bearer {jwt}"))
        .body(Body::empty())
        .expect("request");
    let reply = call(&env.router, req).await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.text());
    assert!(set_cookies(&reply).is_empty(), "no cookie was touched");
    assert!(reply.json()["csrf_token"].is_null());
    assert_eq!(reply.json()["authenticated"], true);
}

#[tokio::test]
async fn a_failing_token_service_fails_the_refresh_and_keeps_the_old_cookie() {
    let env = EnvBuilder::new().build().await;
    let browser = Browser::new(&env);
    browser.login_alice().await;
    let before = browser.cookie(SESSION_COOKIE).expect("session");
    *env.auth.fail_issue.lock().expect("lock") = true;
    let reply = browser.post("/api/v1/auth/session/refresh", None).await;
    assert_eq!(reply.status, StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(browser.cookie(SESSION_COOKIE).expect("session"), before);
}

// ---- GET /auth/csrf --------------------------------------------------------------------

#[tokio::test]
async fn the_csrf_endpoint_returns_the_token_of_the_session_and_sets_the_cookie() {
    let env = EnvBuilder::new().build().await;
    let browser = Browser::new(&env);
    browser.login_alice().await;
    let expected = browser.cookie(CSRF_COOKIE).expect("csrf");
    let reply = browser.get("/api/v1/auth/csrf").await;
    assert_eq!(reply.status, StatusCode::OK);
    assert_eq!(reply.json()["csrf_token"], expected.as_str());
    assert!(
        set_cookies(&reply)
            .iter()
            .any(|c| c.starts_with(&format!("{CSRF_COOKIE}=")))
    );
}

#[tokio::test]
async fn recovery_after_csrf_rejected_works_with_the_documented_two_calls() {
    let env = EnvBuilder::new().build().await;
    let browser = Browser::new(&env);
    browser.login_alice().await;
    // The page lost its token (a reload that kept no state) and the first try is rejected.
    let first = browser
        .send(
            Method::POST,
            "/api/echo",
            Some(serde_json::json!({"name": "x"})),
            false,
        )
        .await;
    assert_eq!(first.error_code(), "csrf_rejected");
    let token = browser.get("/api/v1/auth/csrf").await.json()["csrf_token"]
        .as_str()
        .expect("token")
        .to_owned();
    let session = browser.cookie(SESSION_COOKIE).expect("session");
    let retry = request(Method::POST, "/api/echo")
        .header(
            "cookie",
            format!("{SESSION_COOKIE}={session}; {CSRF_COOKIE}={token}"),
        )
        .header("x-csrf-token", &token)
        .header("origin", crate::session_support::ORIGIN)
        .header("content-type", "application/json")
        .body(Body::from(r#"{"name":"x"}"#))
        .expect("request");
    assert_eq!(call(&env.router, retry).await.status, StatusCode::OK);
}

#[tokio::test]
async fn must_not_hand_a_csrf_token_to_anonymous_callers_tokens_or_bearer_sessions() {
    let env = EnvBuilder::new().build().await;
    let anonymous = Browser::new(&env).get("/api/v1/auth/csrf").await;
    assert_eq!(anonymous.status, StatusCode::UNAUTHORIZED);
    let token = request(Method::GET, "/api/v1/auth/csrf")
        .header("authorization", "Bearer ryx_alice")
        .body(Body::empty())
        .expect("request");
    assert_eq!(call(&env.router, token).await.status, StatusCode::FORBIDDEN);
    // An internal JWT as a bearer is interactive but has no cookie session and no CSRF token.
    let browser = Browser::new(&env);
    browser.login_alice().await;
    let jwt = browser.cookie(SESSION_COOKIE).expect("session");
    let bearer = request(Method::GET, "/api/v1/auth/csrf")
        .header("authorization", format!("Bearer {jwt}"))
        .body(Body::empty())
        .expect("request");
    assert_eq!(
        call(&env.router, bearer).await.status,
        StatusCode::FORBIDDEN
    );
}

#[tokio::test]
async fn the_session_endpoints_need_a_session_manager() {
    let env = EnvBuilder::new().build().await;
    let state = crate::AppState::unwired().with_settings(env.state.settings().clone());
    let (router, _) = crate::RouterBuilder::new(state)
        .module(std::sync::Arc::new(crate::NativeAuthModule::new(
            env.deps_for_test(),
        )))
        .build_with_catalog();
    let reply = call(
        &router,
        crate::test_support::get_request("/api/v1/auth/session"),
    )
    .await;
    assert_eq!(
        (reply.status, reply.error_code().as_str()),
        (
            StatusCode::SERVICE_UNAVAILABLE,
            "authentication_unavailable"
        )
    );
}
