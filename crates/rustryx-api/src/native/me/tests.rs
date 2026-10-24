use axum::body::Body;
use http::{Method, StatusCode};
use rustryx_core::identity::{Action, ApiToken, Privilege, PrivilegeGrant, Role, RoleOrigin};
use rustryx_core::ids::{ApiTokenId, PrivilegeId, RoleId};
use rustryx_core::model::RepoFormat;
use rustryx_core::ports::{PrivilegeRepo, RoleRepo, TokenRepo, UserRepo};

use crate::session_support::{Browser, Env, EnvBuilder};
use crate::test_support::{call, request};
use crate::{AuthApiSettings, SESSION_COOKIE, TokenPolicy};

async fn env_with_policy() -> Env {
    let mut builder = EnvBuilder::new();
    builder.api_settings = AuthApiSettings {
        base_url_configured: true,
        token_policy: TokenPolicy {
            default_ttl_days: 30,
            max_ttl_days: 60,
            require_expiry: false,
        },
        ..AuthApiSettings::default()
    };
    builder.build().await
}

#[tokio::test]
async fn an_anonymous_caller_sees_an_empty_identity_and_what_nx_anonymous_may_see() {
    let env = EnvBuilder::new().build().await;
    let reply = Browser::new(&env).get("/api/v1/me").await;
    assert_eq!(reply.status, StatusCode::OK, "works anonymously");
    let json = reply.json();
    assert_eq!(json["authenticated"], false);
    assert!(
        json["user"].is_null() && json["token"].is_null() && json["groups_confirmed_at"].is_null()
    );
    assert_eq!(json["auth_method"], "anonymous");
    assert_eq!(json["roles"].as_array().expect("roles").len(), 0);
    assert_eq!(json["permissions"]["is_admin"], false);
    assert_eq!(json["permissions"]["can"]["create_personal_token"], false);
    assert_eq!(json["system"]["anonymous_enabled"], false);
    assert_eq!(
        json["permissions"]["repository_grants"]
            .as_array()
            .expect("grants")
            .len(),
        0,
        "anonymous access is off"
    );
}

#[tokio::test]
async fn with_the_global_switch_on_the_anonymous_summary_reads_the_anonymous_role() {
    let mut builder = EnvBuilder::new();
    builder.settings = Box::new(|s| s.anonymous_enabled = true);
    let env = builder.build().await;
    let json = Browser::new(&env).get("/api/v1/me").await.json();
    assert_eq!(json["system"]["anonymous_enabled"], true);
    assert!(json["permissions"]["repository_grants"].is_array());
    assert_eq!(
        json["permissions"]["is_admin"], false,
        "anonymous callers are never administrators"
    );
}

#[tokio::test]
async fn a_signed_in_user_sees_account_roles_and_permissions() {
    let env = env_with_policy().await;
    let browser = Browser::new(&env);
    browser.login_alice().await;
    let json = browser.get("/api/v1/me").await.json();
    assert_eq!(json["authenticated"], true);
    assert_eq!(json["user"]["username"], "alice");
    assert_eq!(json["user"]["email"], "alice@example.test");
    assert_eq!(json["user"]["source"], "local");
    assert_eq!(json["user"]["enabled"], true);
    assert_eq!(json["user"]["id"], env.alice.to_string().as_str());
    assert!(json["user"]["issuer"].is_null() && json["user"]["subject"].is_null());
    assert_eq!(json["auth_method"], "internal_jwt");
    assert_eq!(json["roles"][0]["role"]["name"], "nx-developer");
    assert_eq!(json["roles"][0]["origin"], "manual");
    assert_eq!(
        json["roles"][0]["groups"].as_array().expect("groups").len(),
        0
    );
    assert!(json["token"].is_null());
    assert_eq!(json["permissions"]["is_admin"], false);
    assert_eq!(
        json["permissions"]["can"]["create_personal_token"], true,
        "a session is interactive"
    );
    assert_eq!(json["permissions"]["can"]["administer"], false);
    assert_eq!(json["token_policy"]["default_ttl_days"], 30);
    assert_eq!(json["token_policy"]["max_ttl_days"], 60);
    assert_eq!(json["token_policy"]["require_expiry"], false);
    assert_eq!(
        json["system"],
        serde_json::json!({"read_only": false, "anonymous_enabled": false, "base_url_configured": true})
    );
}

#[tokio::test]
async fn an_administrator_sees_every_area_and_the_administration_flags() {
    let env = EnvBuilder::new().build().await;
    let browser = Browser::new(&env);
    browser.login("admin", "break glass 123").await;
    let json = browser.get("/api/v1/me").await.json();
    assert_eq!(json["permissions"]["is_admin"], true);
    let areas = json["permissions"]["admin_areas"]
        .as_array()
        .expect("areas");
    assert_eq!(areas.len(), rustryx_core::identity::AppArea::ALL.len());
    assert!(areas.iter().any(|a| a == "audit") && areas.iter().any(|a| a == "settings"));
    let can = &json["permissions"]["can"];
    assert_eq!(
        (
            can["create_repository"].clone(),
            can["view_audit"].clone(),
            can["administer"].clone()
        ),
        (true.into(), true.into(), true.into())
    );
    assert_eq!(can["upload"], true);
}

#[tokio::test]
async fn repository_grants_come_from_the_roles_of_this_request() {
    let env = EnvBuilder::new().build().await;
    let privileges: &dyn PrivilegeRepo = &*env.store;
    let roles: &dyn RoleRepo = &*env.store;
    let view = Privilege {
        id: PrivilegeId::generate(),
        name: "maven-view".into(),
        description: String::new(),
        builtin: false,
        grant: PrivilegeGrant::RepositoryView {
            format: Some(RepoFormat::Maven2),
            repository: "libs".into(),
        },
        actions: vec![Action::Browse, Action::Read, Action::Write],
    };
    let admin_area = Privilege {
        id: PrivilegeId::generate(),
        name: "app-users".into(),
        description: String::new(),
        builtin: false,
        grant: PrivilegeGrant::Application {
            area: rustryx_core::identity::AppArea::Users,
        },
        actions: vec![Action::Read],
    };
    privileges.create(&view).await.expect("view");
    privileges.create(&admin_area).await.expect("area");
    let role = Role {
        id: RoleId::generate(),
        name: "publishers".into(),
        description: String::new(),
        builtin: false,
        privilege_ids: vec![view.id, admin_area.id],
        nested_role_ids: Vec::new(),
    };
    roles.create(&role).await.expect("role");
    // Alice holds the role in the fake chain.
    env.auth.set_roles(
        "alice",
        vec![rustryx_core::identity::RoleRef {
            id: role.id,
            name: role.name.clone(),
        }],
    );
    let browser = Browser::new(&env);
    browser.login_alice().await;
    let json = browser.get("/api/v1/me").await.json();
    let grants = json["permissions"]["repository_grants"]
        .as_array()
        .expect("grants");
    assert_eq!(
        grants.len(),
        1,
        "an application grant is not a repository grant: {grants:?}"
    );
    assert_eq!(grants[0]["privilege"], "maven-view");
    assert_eq!(grants[0]["repository"], "libs");
    assert_eq!(grants[0]["format"], "maven2");
    assert_eq!(grants[0]["selector"], false);
    assert_eq!(
        json["permissions"]["can"]["upload"], true,
        "write on a repository"
    );
}

#[tokio::test]
async fn an_sso_user_sees_the_groups_that_produced_the_roles_and_their_origin() {
    let mut builder = EnvBuilder::new();
    builder.oidc = true;
    let env = builder.build().await;
    let browser = Browser::new(&env);
    browser
        .get("/api/v1/auth/oidc/callback?code=a&state=b")
        .await;
    let json = browser.get("/api/v1/me").await.json();
    assert_eq!(json["user"]["source"], "oidc");
    assert!(json["user"]["issuer"].is_string() && json["user"]["subject"].is_string());
    assert_eq!(json["groups"][0], "/devs");
    assert_eq!(json["groups_confirmed_at"], "2026-01-01T00:00:00Z");
    assert_eq!(
        json["roles"][0]["origin"], "oidc",
        "no stored row says otherwise: the account source decides"
    );
    assert_eq!(json["roles"][0]["groups"][0], "/devs");
}

#[tokio::test]
async fn a_stored_role_row_decides_the_origin_when_there_is_one() {
    let env = EnvBuilder::new().build().await;
    let users: &dyn UserRepo = &*env.store;
    let developer = rustryx_testkit::builtin_role_id("nx-developer").expect("builtin");
    users
        .replace_roles_for_origin(env.alice, RoleOrigin::Ldap, &[developer])
        .await
        .expect("row");
    let browser = Browser::new(&env);
    browser.login_alice().await;
    let json = browser.get("/api/v1/me").await.json();
    assert_eq!(json["roles"][0]["origin"], "ldap");
}

#[tokio::test]
async fn a_request_authenticated_by_a_token_names_the_token_and_never_shows_it() {
    let env = EnvBuilder::new().build().await;
    let tokens: &dyn TokenRepo = &*env.store;
    // The fake chain gives the token the id of the user: store a record with that id.
    let record = ApiToken {
        id: ApiTokenId::from_uuid(env.alice.as_uuid()),
        user_id: env.alice,
        name: "ci-publisher".into(),
        token_hash: "f".repeat(64),
        display_prefix: "ryx_ab".into(),
        scopes: vec!["repo:*:read".into()],
        expires_at: None,
        revoked_at: None,
        last_used_at: None,
        created_at: env.clock_now(),
    };
    tokens.create(&record).await.expect("token");
    let req = request(Method::GET, "/api/v1/me")
        .header("authorization", "Bearer ryx_alice")
        .body(Body::empty())
        .expect("request");
    let reply = call(&env.router, req).await;
    let json = reply.json();
    assert_eq!(json["auth_method"], "api_token");
    assert_eq!(json["token"]["name"], "ci-publisher");
    assert_eq!(json["token"]["scopes"][0], "repo:*:read");
    assert_eq!(
        json["permissions"]["can"]["create_personal_token"], false,
        "a token cannot mint tokens"
    );
    let text = reply.text();
    assert!(
        !text.contains(&"f".repeat(64))
            && !text.contains("token_hash")
            && !text.contains("ryx_alice")
    );
}

#[tokio::test]
async fn a_token_whose_record_vanished_is_shown_without_a_token_object() {
    let env = EnvBuilder::new().build().await;
    let req = request(Method::GET, "/api/v1/me")
        .header("authorization", "Bearer ryx_alice")
        .body(Body::empty())
        .expect("request");
    let json = call(&env.router, req).await.json();
    assert!(json["token"].is_null());
}

#[tokio::test]
async fn a_dependency_failure_is_a_503_and_a_vanished_account_is_a_401() {
    let env = EnvBuilder::new().build().await;
    let browser = Browser::new(&env);
    browser.login_alice().await;
    let users: &dyn UserRepo = &*env.store;
    users.delete(env.alice).await.expect("delete");
    let reply = browser.get("/api/v1/me").await;
    assert_eq!(
        reply.status,
        StatusCode::UNAUTHORIZED,
        "the account is gone, the session with it"
    );

    let env = EnvBuilder::new().build().await;
    let browser = Browser::new(&env);
    browser.login_alice().await;
    env.store.set_unavailable(true);
    let reply = browser.get("/api/v1/me").await;
    assert_eq!(reply.status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(reply.error_code(), "store_unavailable");
    assert!(browser.cookie(SESSION_COOKIE).is_some());
}

#[tokio::test]
async fn without_a_checker_the_summary_is_empty_and_never_generous() {
    let env = EnvBuilder::new().build().await;
    let browser = Browser::new(&env);
    browser.login("admin", "break glass 123").await;
    let state = crate::AppState::unwired()
        .with_settings(env.state.settings().clone())
        .with_authenticator(env.auth.clone())
        .with_sessions(env.state.sessions().expect("sessions").clone());
    let (router, _) = crate::RouterBuilder::new(state)
        .module(std::sync::Arc::new(crate::NativeAuthModule::new(
            env.deps_for_test(),
        )))
        .build_with_catalog();
    let session = browser.cookie(SESSION_COOKIE).expect("session");
    let req = request(Method::GET, "/api/v1/me")
        .header("cookie", format!("{SESSION_COOKIE}={session}"))
        .body(Body::empty())
        .expect("request");
    let json = call(&router, req).await.json();
    assert_eq!(json["permissions"]["is_admin"], false);
    assert_eq!(
        json["permissions"]["admin_areas"]
            .as_array()
            .expect("areas")
            .len(),
        0
    );
}
