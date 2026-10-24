use axum::body::Body;
use http::{Method, StatusCode};
use rustryx_core::audit::AuditKind;

use crate::session_support::{Browser, EnvBuilder, ORIGIN};
use crate::test_support::{call, request};
use crate::{AuthApiSettings, CSRF_COOKIE, SESSION_COOKIE};

fn set_cookies(reply: &crate::test_support::Reply) -> Vec<String> {
    reply
        .headers
        .get_all("set-cookie")
        .iter()
        .filter_map(|v| v.to_str().ok().map(str::to_owned))
        .collect()
}

// ---- providers -------------------------------------------------------------------------

#[tokio::test]
async fn the_login_page_is_told_what_is_on() {
    let env = EnvBuilder::new().build().await;
    let reply = Browser::new(&env).get("/api/v1/auth/providers").await;
    assert_eq!(reply.status, StatusCode::OK);
    let json = reply.json();
    assert_eq!(json["local"]["enabled"], true);
    assert_eq!(json["ldap"]["enabled"], false);
    assert_eq!(json["oidc"]["enabled"], false);
    assert_eq!(json["oidc"]["available"], false);
    assert!(json["oidc"]["login_path"].is_null() && json["oidc"]["display_name"].is_null());
}

#[tokio::test]
async fn the_sso_button_follows_the_availability_of_the_provider() {
    let mut builder = EnvBuilder::new();
    builder.oidc = true;
    builder.api_settings = AuthApiSettings {
        ldap_enabled: true,
        oidc_display_name: Some("Corporate SSO".into()),
        ..AuthApiSettings::default()
    };
    let env = builder.build().await;
    let browser = Browser::new(&env);
    let json = browser.get("/api/v1/auth/providers").await.json();
    assert_eq!(json["ldap"]["enabled"], true);
    assert_eq!(json["oidc"]["enabled"], true);
    assert_eq!(json["oidc"]["available"], true);
    assert_eq!(json["oidc"]["display_name"], "Corporate SSO");
    assert_eq!(json["oidc"]["login_path"], "/api/v1/auth/oidc/login");
    *env.oidc
        .as_ref()
        .expect("port")
        .available
        .lock()
        .expect("lock") = false;
    let down = browser.get("/api/v1/auth/providers").await.json();
    assert_eq!(down["oidc"]["enabled"], true);
    assert_eq!(down["oidc"]["available"], false, "the local form stays");
}

// ---- login -----------------------------------------------------------------------------

#[tokio::test]
async fn a_local_login_sets_the_session_and_csrf_cookies_and_returns_the_session() {
    let env = EnvBuilder::new().build().await;
    let browser = Browser::new(&env);
    let reply = browser.login("alice", "correct horse").await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.text());
    assert_eq!(reply.header("cache-control"), Some("no-store"));
    let cookies = set_cookies(&reply);
    assert!(
        cookies
            .iter()
            .any(|c| c.starts_with("__Host-rustryx_session=") && c.contains("HttpOnly"))
    );
    assert!(
        cookies
            .iter()
            .any(|c| c.starts_with("__Host-rustryx_csrf=") && !c.contains("HttpOnly"))
    );
    let json = reply.json();
    assert_eq!(json["authenticated"], true);
    assert_eq!(json["user"]["username"], "alice");
    assert_eq!(json["user"]["source"], "local");
    assert_eq!(json["auth_method"], "internal_jwt");
    assert_eq!(
        json["csrf_token"],
        browser.cookie(CSRF_COOKIE).expect("csrf").as_str()
    );
    assert_eq!(json["idle_timeout_secs"], 1800);
    assert_eq!(json["absolute_timeout_secs"], 28800);
    assert_eq!(json["issued_at"], "2026-01-01T00:00:00Z");
    assert_eq!(json["idle_expires_at"], "2026-01-01T00:30:00Z");
    assert_eq!(json["absolute_expires_at"], "2026-01-01T08:00:00Z");
    assert!(
        !reply.text().contains("correct horse")
            && !reply
                .text()
                .contains(&browser.cookie(SESSION_COOKIE).expect("session")),
        "neither the password nor the token is in the body"
    );
}

#[tokio::test]
async fn a_login_is_audited_with_the_actor_and_never_the_password() {
    let env = EnvBuilder::new().build().await;
    let browser = Browser::new(&env);
    browser.login_alice().await;
    let _ = browser.login("alice", "wrong password").await;
    let events = env.audit.events();
    assert_eq!(events.len(), 2);
    assert_eq!(events[0].kind, AuditKind::LoginSuccess);
    assert_eq!(events[0].action, "auth.login");
    assert_eq!(events[0].outcome, "success");
    assert_eq!(events[0].actor_name, "alice");
    assert_eq!(events[0].actor_id, Some(env.alice));
    assert_eq!(events[1].kind, AuditKind::LoginFailure);
    assert_eq!(events[1].outcome, "failure");
    assert_eq!(events[1].actor_name, "alice");
    for event in &events {
        let text = serde_json::to_string(event).expect("json");
        assert!(
            !text.contains("wrong password") && !text.contains("correct horse"),
            "{text}"
        );
    }
}

#[tokio::test]
async fn wrong_password_and_unknown_user_answer_alike() {
    let env = EnvBuilder::new().build().await;
    let browser = Browser::new(&env);
    let wrong = browser.login("alice", "nope").await;
    let unknown = browser.login("nobody", "nope").await;
    for reply in [&wrong, &unknown] {
        assert_eq!(reply.status, StatusCode::UNAUTHORIZED);
        assert_eq!(reply.error_code(), "invalid_credentials");
        assert!(reply.header("www-authenticate").is_some());
        assert!(
            reply.headers.get_all("set-cookie").iter().next().is_none(),
            "no cookie on a refusal"
        );
    }
    assert_eq!(
        wrong.json()["error"]["message"],
        unknown.json()["error"]["message"]
    );
}

#[tokio::test]
async fn must_not_open_a_session_with_an_api_token_as_the_password() {
    let env = EnvBuilder::new().build().await;
    let browser = Browser::new(&env);
    // `ryx_alice` is a valid token in the fake chain: a token must not become a UI session.
    let reply = browser.login("alice", "ryx_alice").await;
    assert_eq!(reply.status, StatusCode::UNAUTHORIZED);
    assert_eq!(reply.error_code(), "invalid_credentials");
    assert!(browser.cookie(SESSION_COOKIE).is_none());
    assert_eq!(
        env.audit.events().last().expect("event").kind,
        AuditKind::LoginFailure
    );
}

#[tokio::test]
async fn must_not_accept_a_login_from_a_foreign_or_missing_origin() {
    let env = EnvBuilder::new().build().await;
    for origin in [Some("https://evil.example.test"), Some("null"), None] {
        let mut builder =
            request(Method::POST, "/api/v1/auth/login").header("content-type", "application/json");
        if let Some(origin) = origin {
            builder = builder.header("origin", origin);
        }
        let body = r#"{"username":"alice","password":"correct horse"}"#;
        let reply = call(
            &env.router,
            builder.body(Body::from(body)).expect("request"),
        )
        .await;
        assert_eq!(reply.status, StatusCode::FORBIDDEN, "origin {origin:?}");
        assert_eq!(reply.error_code(), "csrf_rejected");
        assert!(reply.headers.get_all("set-cookie").iter().next().is_none());
    }
    assert!(
        env.audit.events().is_empty(),
        "no credential was even looked at"
    );
}

#[tokio::test]
async fn login_bodies_are_validated_like_every_other_body() {
    let env = EnvBuilder::new().build().await;
    let post = |content_type: Option<&str>, body: &str| {
        let mut builder = request(Method::POST, "/api/v1/auth/login").header("origin", ORIGIN);
        if let Some(ct) = content_type {
            builder = builder.header("content-type", ct);
        }
        builder.body(Body::from(body.to_owned())).expect("request")
    };
    let missing_field = call(
        &env.router,
        post(Some("application/json"), r#"{"username":"alice"}"#),
    )
    .await;
    assert_eq!(
        (missing_field.status, missing_field.error_code().as_str()),
        (StatusCode::BAD_REQUEST, "invalid_json")
    );
    let unknown_field = call(
        &env.router,
        post(
            Some("application/json"),
            r#"{"username":"a","password":"b","remember":true}"#,
        ),
    )
    .await;
    assert_eq!(unknown_field.error_code(), "invalid_json");
    let not_json = call(&env.router, post(Some("application/json"), "nope")).await;
    assert_eq!(not_json.error_code(), "invalid_json");
    let wrong_type = call(&env.router, post(Some("text/plain"), "{}")).await;
    assert_eq!(wrong_type.status, StatusCode::UNSUPPORTED_MEDIA_TYPE);
}

#[tokio::test]
async fn lockout_provider_outage_and_overload_keep_their_own_answers() {
    for (error, status, code) in [
        (
            (|| rustryx_core::AuthError::LockedOut {
                retry_after: std::time::Duration::from_secs(60),
            }) as fn() -> rustryx_core::AuthError,
            StatusCode::TOO_MANY_REQUESTS,
            "locked_out",
        ),
        (
            || rustryx_core::AuthError::GuardUnavailable,
            StatusCode::SERVICE_UNAVAILABLE,
            "login_unavailable",
        ),
        (
            || rustryx_core::AuthError::Overloaded,
            StatusCode::SERVICE_UNAVAILABLE,
            "overloaded",
        ),
    ] {
        let env = EnvBuilder::new().build().await;
        let failing = crate::test_support::StubAuth::failing(error);
        let state = env.state.clone().with_authenticator(failing);
        let (router, _) = crate::RouterBuilder::new(state)
            .module(std::sync::Arc::new(crate::NativeAuthModule::new(
                env.deps_for_test(),
            )))
            .build_with_catalog();
        let req = request(Method::POST, "/api/v1/auth/login")
            .header("origin", ORIGIN)
            .header("content-type", "application/json")
            .body(Body::from(r#"{"username":"alice","password":"x"}"#))
            .expect("request");
        let reply = call(&router, req).await;
        assert_eq!((reply.status, reply.error_code().as_str()), (status, code));
        assert!(reply.header("retry-after").is_some());
    }
}

#[tokio::test]
async fn a_login_without_an_authenticator_or_session_manager_is_a_503() {
    let env = EnvBuilder::new().build().await;
    let post = || {
        request(Method::POST, "/api/v1/auth/login")
            .header("origin", ORIGIN)
            .header("content-type", "application/json")
            .body(Body::from(
                r#"{"username":"alice","password":"correct horse"}"#,
            ))
            .expect("request")
    };
    let no_sessions = crate::AppState::unwired()
        .with_settings(env.state.settings().clone())
        .with_authenticator(env.auth.clone());
    let (router, _) = crate::RouterBuilder::new(no_sessions)
        .module(std::sync::Arc::new(crate::NativeAuthModule::new(
            env.deps_for_test(),
        )))
        .build_with_catalog();
    let reply = call(&router, post()).await;
    assert_eq!(
        (reply.status, reply.error_code().as_str()),
        (
            StatusCode::SERVICE_UNAVAILABLE,
            "authentication_unavailable"
        )
    );
}

#[tokio::test]
async fn a_chain_that_returns_the_wrong_kind_of_principal_does_not_open_a_session() {
    let env = EnvBuilder::new().build().await;
    // A chain that answers a Basic credential with an API token principal.
    let confused = crate::test_support::StubAuth::always(crate::test_support::principal(
        "alice",
        rustryx_core::identity::AuthMethod::ApiToken,
        "r",
    ));
    let state = env.state.clone().with_authenticator(confused);
    let (router, _) = crate::RouterBuilder::new(state)
        .module(std::sync::Arc::new(crate::NativeAuthModule::new(
            env.deps_for_test(),
        )))
        .build_with_catalog();
    let req = request(Method::POST, "/api/v1/auth/login")
        .header("origin", ORIGIN)
        .header("content-type", "application/json")
        .body(Body::from(r#"{"username":"alice","password":"x"}"#))
        .expect("request");
    let reply = call(&router, req).await;
    assert_eq!(reply.status, StatusCode::UNAUTHORIZED);
    assert!(reply.headers.get_all("set-cookie").iter().next().is_none());
}

#[tokio::test]
async fn a_failing_audit_sink_fails_the_login_closed_but_not_the_refusal() {
    let env = EnvBuilder::new().build().await;
    let failing: std::sync::Arc<dyn rustryx_core::ports::AuditSink> =
        std::sync::Arc::new(FailingAudit);
    let mut deps = env.deps_for_test();
    deps.audit = failing;
    let (router, _) = crate::RouterBuilder::new(env.state.clone())
        .module(std::sync::Arc::new(crate::NativeAuthModule::new(deps)))
        .build_with_catalog();
    let login = |password: &str| {
        request(Method::POST, "/api/v1/auth/login")
            .header("origin", ORIGIN)
            .header("content-type", "application/json")
            .body(Body::from(format!(
                r#"{{"username":"alice","password":"{password}"}}"#
            )))
            .expect("request")
    };
    let ok = call(&router, login("correct horse")).await;
    assert_eq!(
        ok.status,
        StatusCode::SERVICE_UNAVAILABLE,
        "no audit row, no session"
    );
    assert_eq!(ok.error_code(), "store_unavailable");
    assert!(ok.headers.get_all("set-cookie").iter().next().is_none());
    let refused = call(&router, login("wrong")).await;
    assert_eq!(
        refused.status,
        StatusCode::UNAUTHORIZED,
        "the refusal is still the refusal"
    );
}

struct FailingAudit;

#[async_trait::async_trait]
impl rustryx_core::ports::AuditSink for FailingAudit {
    async fn record(
        &self,
        _event: rustryx_core::audit::AuditEvent,
    ) -> Result<(), rustryx_core::StoreError> {
        Err(rustryx_core::StoreError::Unavailable("audit".into()))
    }
}

#[tokio::test]
async fn a_second_login_with_a_session_cookie_still_in_the_jar_needs_no_csrf_token() {
    let env = EnvBuilder::new().build().await;
    let browser = Browser::new(&env);
    browser.login_alice().await;
    let first = browser.cookie(SESSION_COOKIE).expect("session");
    // The old cookie goes along with the form post; it is not the credential of this request.
    let reply = browser.login("admin", "break glass 123").await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.text());
    assert_ne!(browser.cookie(SESSION_COOKIE).expect("session"), first);
    assert_eq!(browser.get("/api/whoami").await.text(), "admin");
}

#[tokio::test]
async fn login_attempts_are_counted_by_method_and_result() {
    let env = EnvBuilder::new().build().await;
    let browser = Browser::new(&env);
    browser.login_alice().await;
    let _ = browser.login("alice", "wrong").await;
    let text = crate::test_support::get_uri(&env.router, "/metrics")
        .await
        .text();
    assert!(
        text.contains(r#"auth_attempts_total{method="local",result="ok"} 1"#),
        "{text}"
    );
    assert!(
        text.contains(r#"auth_attempts_total{method="unknown",result="denied"} 1"#),
        "{text}"
    );
}

// ---- logout ----------------------------------------------------------------------------

#[tokio::test]
async fn logout_revokes_the_token_clears_the_cookies_and_leaves_an_audit_row() {
    let env = EnvBuilder::new().build().await;
    let browser = Browser::new(&env);
    browser.login_alice().await;
    let tid = env.auth.issued().last().cloned().expect("a token");
    let stolen = browser.cookie(SESSION_COOKIE).expect("session");
    let reply = browser.post("/api/v1/auth/logout", None).await;
    assert_eq!(reply.status, StatusCode::OK);
    assert!(reply.json()["redirect_url"].is_null());
    let cookies = set_cookies(&reply);
    for name in [SESSION_COOKIE, CSRF_COOKIE, "__Host-rustryx_sso"] {
        assert!(
            cookies
                .iter()
                .any(|c| c.starts_with(&format!("{name}=;")) && c.contains("Max-Age=0")),
            "{name} not cleared: {cookies:?}"
        );
    }
    assert!(env.auth.is_revoked(&tid));
    assert_eq!(browser.get("/api/whoami").await.text(), "anonymous");
    // A copied cookie is useless after the logout.
    let copied = request(Method::GET, "/api/whoami")
        .header("cookie", format!("{SESSION_COOKIE}={stolen}"))
        .body(Body::empty())
        .expect("request");
    assert_eq!(call(&env.router, copied).await.text(), "anonymous");
    let last = env.audit.events().pop().expect("event");
    assert_eq!(
        (last.action.as_str(), last.outcome.as_str()),
        ("auth.logout", "success")
    );
    assert_eq!(last.actor_name, "alice");
}

#[tokio::test]
async fn logout_is_idempotent_and_needs_no_session() {
    let env = EnvBuilder::new().build().await;
    let browser = Browser::new(&env);
    let reply = browser.post("/api/v1/auth/logout", None).await;
    assert_eq!(reply.status, StatusCode::OK);
    assert!(set_cookies(&reply).iter().any(|c| c.contains("Max-Age=0")));
    assert!(
        env.audit.events().is_empty(),
        "nothing to audit without a session"
    );
    let again = browser.post("/api/v1/auth/logout", None).await;
    assert_eq!(again.status, StatusCode::OK);
}

#[tokio::test]
async fn must_not_log_out_through_a_forged_cross_site_request() {
    let env = EnvBuilder::new().build().await;
    let browser = Browser::new(&env);
    browser.login_alice().await;
    let reply = browser
        .send(Method::POST, "/api/v1/auth/logout", None, false)
        .await;
    assert_eq!(
        (reply.status, reply.error_code().as_str()),
        (StatusCode::FORBIDDEN, "csrf_rejected")
    );
    assert_eq!(
        browser.get("/api/whoami").await.text(),
        "alice",
        "the session is untouched"
    );
}

#[tokio::test]
async fn a_failing_revocation_still_clears_the_cookies() {
    let env = EnvBuilder::new().build().await;
    let browser = Browser::new(&env);
    browser.login_alice().await;
    *env.auth.fail_revoke.lock().expect("lock") = true;
    let reply = browser.post("/api/v1/auth/logout", None).await;
    assert_eq!(reply.status, StatusCode::OK, "the user asked to leave");
    assert!(browser.cookie(SESSION_COOKIE).is_none());
}
