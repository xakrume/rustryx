use http::{Method, StatusCode};
use rustryx_core::AuthError;
use rustryx_core::audit::AuditKind;

use super::*;
use crate::session_support::{Browser, EnvBuilder};
use crate::{CSRF_COOKIE, SESSION_COOKIE};

fn set_cookies(reply: &crate::test_support::Reply) -> Vec<String> {
    reply
        .headers
        .get_all("set-cookie")
        .iter()
        .filter_map(|v| v.to_str().ok().map(str::to_owned))
        .collect()
}

async fn sso_env() -> crate::session_support::Env {
    let mut builder = EnvBuilder::new();
    builder.oidc = true;
    builder.build().await
}

// ---- return_to -------------------------------------------------------------------------

#[test]
fn only_a_plain_absolute_path_survives_as_return_to() {
    for ok in [
        "/",
        "/repositories",
        "/repositories/maven?x=1#top",
        "/a/b/c",
    ] {
        assert_eq!(sanitize_return_to(Some(ok)), ok);
    }
    assert_eq!(sanitize_return_to(None), "/");
    for open_redirect in [
        "//evil.example.test",
        "//evil.example.test/path",
        "/\\evil.example.test",
        "https://evil.example.test/",
        "http://evil.example.test",
        "evil.example.test",
        "",
        "javascript:alert(1)",
        "/ok\r\nSet-Cookie: x=y",
        "/ok\u{0}",
        "/tab\tinside",
        "/non-ascii-\u{e9}",
    ] {
        assert_eq!(
            sanitize_return_to(Some(open_redirect)),
            "/",
            "{open_redirect:?}"
        );
    }
    let long = format!("/{}", "a".repeat(MAX_RETURN_TO_BYTES));
    assert_eq!(sanitize_return_to(Some(&long)), "/");
    let limit = format!("/{}", "a".repeat(MAX_RETURN_TO_BYTES - 1));
    assert_eq!(sanitize_return_to(Some(&limit)), limit);
}

// ---- start -----------------------------------------------------------------------------

#[tokio::test]
async fn starting_a_login_redirects_to_keycloak_with_the_flow_cookie() {
    let env = sso_env().await;
    let browser = Browser::new(&env);
    let reply = browser
        .get("/api/v1/auth/oidc/login?return_to=/repositories")
        .await;
    assert_eq!(reply.status, StatusCode::FOUND);
    assert_eq!(
        reply.header("location"),
        Some("https://sso.example.test/auth?state=abc")
    );
    assert!(
        set_cookies(&reply)
            .iter()
            .any(|c| c.starts_with("rustryx_oidc_flow=sealed"))
    );
    assert_eq!(
        *env.oidc
            .as_ref()
            .expect("port")
            .return_to
            .lock()
            .expect("lock"),
        "/repositories"
    );
}

#[tokio::test]
async fn an_open_redirect_target_is_replaced_before_the_adapter_sees_it() {
    let env = sso_env().await;
    let browser = Browser::new(&env);
    let _ = browser
        .get("/api/v1/auth/oidc/login?return_to=//evil.example.test")
        .await;
    assert_eq!(
        *env.oidc
            .as_ref()
            .expect("port")
            .return_to
            .lock()
            .expect("lock"),
        "/"
    );
}

#[tokio::test]
async fn without_oidc_the_login_and_callback_are_404() {
    let env = EnvBuilder::new().build().await;
    let browser = Browser::new(&env);
    for uri in [
        "/api/v1/auth/oidc/login",
        "/api/v1/auth/oidc/callback?code=a&state=b",
    ] {
        let reply = browser.get(uri).await;
        assert_eq!(
            (reply.status, reply.error_code().as_str()),
            (StatusCode::NOT_FOUND, "not_found"),
            "{uri}"
        );
    }
}

#[tokio::test]
async fn while_keycloak_is_unreachable_a_login_is_503_and_nothing_else_is_affected() {
    let env = sso_env().await;
    let browser = Browser::new(&env);
    *env.oidc
        .as_ref()
        .expect("port")
        .available
        .lock()
        .expect("lock") = false;
    let reply = browser.get("/api/v1/auth/oidc/login").await;
    assert_eq!(
        (reply.status, reply.error_code().as_str()),
        (
            StatusCode::SERVICE_UNAVAILABLE,
            "identity_provider_unavailable"
        )
    );
    assert!(reply.header("retry-after").is_some());
    let local = browser.login_alice().await;
    assert_eq!(local.status, StatusCode::OK, "local users keep working");
}

// ---- callback --------------------------------------------------------------------------

#[tokio::test]
async fn a_good_callback_issues_the_session_and_redirects_to_the_stored_path() {
    let env = sso_env().await;
    let browser = Browser::new(&env);
    browser
        .get("/api/v1/auth/oidc/login?return_to=/repositories")
        .await;
    browser.set_cookie("rustryx_oidc_flow", "sealed");
    let reply = browser
        .get("/api/v1/auth/oidc/callback?code=the-code&state=the-state")
        .await;
    assert_eq!(reply.status, StatusCode::FOUND, "{}", reply.text());
    assert_eq!(reply.header("location"), Some("/repositories"));
    let cookies = set_cookies(&reply);
    assert!(
        cookies
            .iter()
            .any(|c| c.starts_with("__Host-rustryx_session=") && c.contains("HttpOnly"))
    );
    assert!(
        cookies
            .iter()
            .any(|c| c.starts_with("__Host-rustryx_csrf="))
    );
    assert!(
        cookies
            .iter()
            .any(|c| c.starts_with("rustryx_oidc_flow=;") && c.contains("Max-Age=0")),
        "the flow cookie is spent"
    );
    // The adapter got the query and the raw Cookie header, nothing more.
    let seen = env
        .oidc
        .as_ref()
        .expect("port")
        .seen
        .lock()
        .expect("lock")
        .clone();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].0.as_deref(), Some("the-code"));
    assert_eq!(seen[0].1.as_deref(), Some("the-state"));
    assert!(seen[0].3.contains("rustryx_oidc_flow=sealed"));
    // The session works, and carries the groups of the login.
    let me = browser.get("/api/v1/me").await;
    assert_eq!(me.json()["user"]["username"], "sso-user");
    assert_eq!(me.json()["groups"][0], "/devs");
    let event = env.audit.events().pop().expect("event");
    assert_eq!(
        (event.kind, event.action.as_str(), event.outcome.as_str()),
        (AuditKind::LoginSuccess, "auth.oidc_login", "success")
    );
    assert_eq!(event.actor_name, "sso-user");
}

#[tokio::test]
async fn a_callback_may_not_redirect_anywhere_but_a_local_path() {
    let env = sso_env().await;
    let browser = Browser::new(&env);
    // An adapter (or a tampered cookie) that hands back a hostile target is overruled.
    *env.oidc
        .as_ref()
        .expect("port")
        .return_to
        .lock()
        .expect("lock") = "//evil.example.test".to_owned();
    let reply = browser
        .get("/api/v1/auth/oidc/callback?code=a&state=b")
        .await;
    assert_eq!(reply.status, StatusCode::FOUND);
    assert_eq!(reply.header("location"), Some("/"));
}

#[tokio::test]
async fn callback_failures_map_to_the_codes_of_the_contract() {
    for (error, status, code) in [
        (
            (|| AuthError::InvalidToken("state mismatch")) as fn() -> AuthError,
            StatusCode::BAD_REQUEST,
            "oidc_callback_invalid",
        ),
        (
            || AuthError::InvalidCredentials,
            StatusCode::UNAUTHORIZED,
            "invalid_credentials",
        ),
        (
            || AuthError::ProviderUnavailable("keycloak".into()),
            StatusCode::SERVICE_UNAVAILABLE,
            "identity_provider_unavailable",
        ),
        (
            || AuthError::UsernameConflict,
            StatusCode::CONFLICT,
            "username_conflict",
        ),
        (
            || AuthError::AccessDenied("no mapped group"),
            StatusCode::FORBIDDEN,
            "forbidden",
        ),
    ] {
        let env = sso_env().await;
        *env.oidc
            .as_ref()
            .expect("port")
            .next_error
            .lock()
            .expect("lock") = Some(error);
        let browser = Browser::new(&env);
        let reply = browser
            .get("/api/v1/auth/oidc/callback?code=a&state=b")
            .await;
        assert_eq!((reply.status, reply.error_code().as_str()), (status, code));
        let cookies = set_cookies(&reply);
        assert!(
            cookies.iter().any(|c| c.starts_with("rustryx_oidc_flow=;")),
            "the flow cookie is cleared on failure too"
        );
        assert!(
            !cookies
                .iter()
                .any(|c| c.starts_with("__Host-rustryx_session=")),
            "no session on failure"
        );
        let last = env.audit.events().pop().expect("event");
        assert_eq!(
            (last.kind, last.outcome.as_str()),
            (AuditKind::LoginFailure, "failure")
        );
    }
}

#[tokio::test]
async fn a_browser_gets_a_redirect_to_the_login_page_and_a_script_gets_the_envelope() {
    let env = sso_env().await;
    *env.oidc
        .as_ref()
        .expect("port")
        .next_error
        .lock()
        .expect("lock") = Some(|| AuthError::InvalidToken("flow cookie expired"));
    let html = crate::test_support::call(
        &env.router,
        crate::test_support::request(Method::GET, "/api/v1/auth/oidc/callback?code=a&state=b")
            .header(
                "accept",
                "text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8",
            )
            .header("x-request-id", "req-42")
            .body(axum::body::Body::empty())
            .expect("request"),
    )
    .await;
    assert_eq!(html.status, StatusCode::FOUND);
    assert_eq!(
        html.header("location"),
        Some("/login?error=oidc_callback_invalid&request_id=req-42")
    );
    assert!(
        set_cookies(&html)
            .iter()
            .any(|c| c.starts_with("rustryx_oidc_flow=;"))
    );

    *env.oidc
        .as_ref()
        .expect("port")
        .next_error
        .lock()
        .expect("lock") = Some(|| AuthError::InvalidToken("flow cookie expired"));
    let json = crate::test_support::call(
        &env.router,
        crate::test_support::request(Method::GET, "/api/v1/auth/oidc/callback?code=a&state=b")
            .header("accept", "application/json, text/html;q=0.5")
            .body(axum::body::Body::empty())
            .expect("request"),
    )
    .await;
    assert_eq!(json.status, StatusCode::BAD_REQUEST);
    assert_eq!(json.error_code(), "oidc_callback_invalid");
}

#[tokio::test]
async fn an_error_answer_of_keycloak_is_a_failed_login_not_a_session() {
    let env = sso_env().await;
    *env.oidc
        .as_ref()
        .expect("port")
        .next_error
        .lock()
        .expect("lock") = Some(|| AuthError::InvalidCredentials);
    let browser = Browser::new(&env);
    let reply = browser
        .get("/api/v1/auth/oidc/callback?error=access_denied&error_description=user+said+no")
        .await;
    assert_eq!(reply.status, StatusCode::UNAUTHORIZED);
    let seen = env
        .oidc
        .as_ref()
        .expect("port")
        .seen
        .lock()
        .expect("lock")
        .clone();
    assert_eq!(
        seen[0].2.as_deref(),
        Some("access_denied"),
        "the adapter is told Keycloak refused"
    );
    assert!(
        !reply.text().contains("user said no"),
        "the provider's text is not shown"
    );
}

#[tokio::test]
async fn must_not_hand_out_a_session_for_an_identity_the_chain_refuses() {
    // The login succeeded at Keycloak, but the chain says the groups map to nothing.
    let env = sso_env().await;
    let refusing = crate::test_support::StubAuth::failing(|| {
        AuthError::GroupLookupFailed("no mapped group and no default role".into())
    });
    let state = env.state.clone().with_authenticator(refusing);
    let (router, _) = crate::RouterBuilder::new(state)
        .module(std::sync::Arc::new(crate::NativeAuthModule::new(
            env.deps_for_test(),
        )))
        .build_with_catalog();
    let reply = crate::test_support::call(
        &router,
        crate::test_support::get_request("/api/v1/auth/oidc/callback?code=a&state=b"),
    )
    .await;
    assert_eq!(
        (reply.status, reply.error_code().as_str()),
        (StatusCode::FORBIDDEN, "forbidden")
    );
    assert!(
        !set_cookies(&reply)
            .iter()
            .any(|c| c.starts_with("__Host-rustryx_session="))
    );
    assert!(
        !reply.text().contains("mapped"),
        "the reason stays in the log"
    );
}

#[tokio::test]
async fn the_wrong_kind_of_principal_from_the_chain_is_not_a_session_either() {
    let env = sso_env().await;
    let confused = crate::test_support::StubAuth::always(crate::test_support::principal(
        "x",
        rustryx_core::identity::AuthMethod::ApiToken,
        "r",
    ));
    let state = env.state.clone().with_authenticator(confused);
    let (router, _) = crate::RouterBuilder::new(state)
        .module(std::sync::Arc::new(crate::NativeAuthModule::new(
            env.deps_for_test(),
        )))
        .build_with_catalog();
    let reply = crate::test_support::call(
        &router,
        crate::test_support::get_request("/api/v1/auth/oidc/callback?code=a&state=b"),
    )
    .await;
    assert_eq!(reply.status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn an_sso_session_ends_after_five_minutes_without_a_refresher_and_goes_on_with_one() {
    use crate::session_support::FakeRefresher;
    // Without a refresher.
    let env = sso_env().await;
    let browser = Browser::new(&env);
    browser
        .get("/api/v1/auth/oidc/callback?code=a&state=b")
        .await;
    assert_eq!(browser.get("/api/whoami").await.text(), "sso-user");
    crate::session_support::advance(&env, std::time::Duration::from_mins(4));
    assert_eq!(browser.get("/api/whoami").await.text(), "sso-user");
    crate::session_support::advance(&env, std::time::Duration::from_mins(2));
    assert_eq!(
        browser.get("/api/whoami").await.text(),
        "anonymous",
        "the snapshot is older than five minutes"
    );

    // With one, and a carrier the adapter handed out at login.
    let mut builder = EnvBuilder::new();
    builder.oidc = true;
    builder.carrier = Some("carrier-1".into());
    let clock_for_refresher = rustryx_testkit::FixedClock::default();
    let refresher = FakeRefresher::new(clock_for_refresher.clone(), &["/devs"]);
    builder.refresher = Some(refresher.clone());
    let env = builder.build().await;
    // The refresher reads the harness clock: share it.
    let refresher = FakeRefresher::new(env.clock.clone(), &["/devs"]);
    let sessions = crate::session::SessionManager::new(
        crate::SessionSettings::default(),
        env.auth.clone(),
        std::sync::Arc::new(env.clock.clone()),
        crate::CsrfKey::from_cookie_key(&rustryx_core::secret::Secret::new({
            use base64::Engine;
            let mut bytes = [3_u8; 32];
            bytes[0] = 9;
            base64::engine::general_purpose::STANDARD.encode(bytes)
        }))
        .expect("key"),
        Some(refresher.clone()),
    );
    let state = env
        .state
        .clone()
        .with_sessions(std::sync::Arc::new(sessions));
    let (router, _) = crate::RouterBuilder::new(state)
        .module(std::sync::Arc::new(crate::NativeAuthModule::new(
            env.deps_for_test(),
        )))
        .module(crate::test_support::TestModule::new())
        .build_with_catalog();
    let mut env = env;
    env.router = router;
    let browser = Browser::new(&env);
    browser
        .get("/api/v1/auth/oidc/callback?code=a&state=b")
        .await;
    assert!(
        browser.cookie("__Host-rustryx_sso").is_some(),
        "the carrier cookie is set at login"
    );
    for _ in 0..6 {
        crate::session_support::advance(&env, std::time::Duration::from_mins(3));
        assert_eq!(browser.get("/api/whoami").await.text(), "sso-user");
    }
    assert!(
        refresher.calls.load(std::sync::atomic::Ordering::SeqCst) >= 6,
        "the groups were confirmed again and again"
    );
    assert!(browser.cookie(SESSION_COOKIE).is_some() && browser.cookie(CSRF_COOKIE).is_some());
}
