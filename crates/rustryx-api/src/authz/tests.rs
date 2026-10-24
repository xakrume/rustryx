use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::extract::State;
use axum::routing::{get, post};
use http::{Method, StatusCode};
use rustryx_core::identity::{Action, AppArea, AuthMethod};
use rustryx_core::ports::Decision;

use super::*;
use crate::session_support::{Browser, EnvBuilder};
use crate::test_support::{
    StubAuth, StubRbac, call, get_request, get_uri, principal, ready_state, request,
};
use crate::{RouteModule, RouteSet, RouterBuilder, resource_for_tests};

fn bad_rbac() -> StubRbac {
    StubRbac(Err(|| {
        rustryx_core::AuthError::GroupLookupFailed("down".into())
    }))
}

// ---- the authorize family ------------------------------------------------------------

#[tokio::test]
async fn authorize_hands_out_a_proof_that_names_what_was_decided() {
    let state = ready_state().with_rbac(Arc::new(StubRbac(Ok(Decision::Allow))));
    let alice = principal("alice", AuthMethod::BasicLocal, "r");
    let proof = state
        .authorize(&alice, Action::Write, &resource_for_tests())
        .await
        .expect("allowed");
    assert_eq!(proof.principal().username, "alice");
    assert_eq!(proof.grant().action, Action::Write);
    assert_eq!(proof.grant().repository, "maven-central");
}

#[tokio::test]
async fn must_not_authorize_without_a_checker_for_any_kind() {
    let state = ready_state();
    let alice = principal("alice", AuthMethod::InternalJwt, "r");
    let status = |error: ApiError| error.status();
    assert_eq!(
        status(
            state
                .authorize(&alice, Action::Read, &resource_for_tests())
                .await
                .expect_err("no checker")
        ),
        StatusCode::SERVICE_UNAVAILABLE
    );
    assert_eq!(
        status(state.authorize_admin(&alice).await.expect_err("no checker")),
        StatusCode::SERVICE_UNAVAILABLE
    );
    assert_eq!(
        status(
            state
                .authorize_app(&alice, AppArea::Users, Action::Read)
                .await
                .expect_err("no checker")
        ),
        StatusCode::SERVICE_UNAVAILABLE
    );
    assert_eq!(
        status(
            state
                .authorize_project(
                    &alice,
                    rustryx_core::ids::ProjectId::generate(),
                    rustryx_core::project::ProjectPermission::ViewProject
                )
                .await
                .expect_err("no checker")
        ),
        StatusCode::SERVICE_UNAVAILABLE
    );
    // These two need no checker.
    assert!(state.authorize_authenticated(&alice).is_ok());
    assert!(state.authorize_interactive(&alice).is_ok());
}

#[tokio::test]
async fn must_not_let_an_anonymous_caller_through_any_kind() {
    let state = ready_state().with_rbac(Arc::new(StubRbac(Ok(Decision::Allow))));
    let anonymous = Principal::anonymous();
    assert_eq!(
        state
            .authorize_authenticated(&anonymous)
            .expect_err("anonymous")
            .status(),
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        state
            .authorize_interactive(&anonymous)
            .expect_err("anonymous")
            .status(),
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        state
            .authorize_admin(&anonymous)
            .await
            .expect_err("anonymous")
            .status(),
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        state
            .authorize_app(&anonymous, AppArea::Settings, Action::Read)
            .await
            .expect_err("anonymous")
            .status(),
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        state
            .authorize_project(
                &anonymous,
                rustryx_core::ids::ProjectId::generate(),
                rustryx_core::project::ProjectPermission::ViewProject
            )
            .await
            .expect_err("anonymous")
            .status(),
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn must_not_treat_a_forged_admin_role_name_as_administration() {
    // The default `is_admin` of a checker is false: a principal that only CLAIMS the role name
    // gets nothing from `authorize_admin` unless the checker confirms it.
    let state = ready_state().with_rbac(Arc::new(StubRbac(Ok(Decision::Allow))));
    let forged = principal("mallory", AuthMethod::InternalJwt, "nx-admin");
    assert_eq!(
        state
            .authorize_admin(&forged)
            .await
            .expect_err("not confirmed")
            .status(),
        StatusCode::FORBIDDEN
    );
    assert!(
        forged.has_role("nx-admin"),
        "the name is there, the proof is not"
    );
}

#[tokio::test]
async fn interactive_credentials_are_a_whitelist() {
    let state = ready_state();
    for (method, allowed) in [
        (AuthMethod::InternalJwt, true),
        (AuthMethod::OidcBearer, true),
        (AuthMethod::ApiToken, false),
        (AuthMethod::RobotToken, false),
        (AuthMethod::ScanToken, false),
        (AuthMethod::BasicLocal, false),
        (AuthMethod::BasicLdap, false),
    ] {
        let verdict = state.authorize_interactive(&principal("u", method, "r"));
        assert_eq!(verdict.is_ok(), allowed, "{method:?}");
        if !allowed {
            assert_eq!(
                verdict.expect_err("refused").status(),
                StatusCode::FORBIDDEN
            );
        }
    }
}

#[tokio::test]
async fn a_checker_that_fails_closes_every_kind() {
    let state = ready_state().with_rbac(Arc::new(bad_rbac()));
    let alice = principal("alice", AuthMethod::InternalJwt, "r");
    // `is_admin` of the stub is the provided default (false), so only `check` can fail.
    assert!(
        state
            .authorize(&alice, Action::Read, &resource_for_tests())
            .await
            .is_err()
    );
    assert!(state.authorize_admin(&alice).await.is_err());
}

#[test]
fn proofs_describe_themselves() {
    let proof = Authorized::new(Principal::anonymous(), AdminGrant);
    assert_eq!(*proof.grant(), AdminGrant);
    assert!(proof.principal().is_anonymous());
    let copy = proof.clone();
    assert!(format!("{copy:?}").contains("AdminGrant"));
}

// ---- the route layer -------------------------------------------------------------------

/// Routes that exercise every requirement, and handlers that forget or remember to ask.
struct Probe;

async fn forgetful() -> &'static str {
    "served without asking"
}

async fn asks(
    State(state): State<AppState>,
    crate::RequestPrincipal(who): crate::RequestPrincipal,
) -> Result<&'static str, ApiError> {
    state
        .authorize(&who, Action::Read, &resource_for_tests())
        .await?;
    Ok("asked")
}

async fn not_found_without_asking() -> ApiError {
    ApiError::new(
        StatusCode::NOT_FOUND,
        "repository_not_found",
        "Repository not found",
    )
}

async fn admin_only(proof: Authorized<AdminGrant>) -> String {
    proof.principal().username.clone()
}

async fn area(proof: Authorized<AppGrant>) -> String {
    format!("{}:{:?}", proof.grant().area.as_str(), proof.grant().action)
}

async fn interactive(proof: Authorized<InteractiveGrant>) -> String {
    proof.principal().username.clone()
}

async fn authenticated(proof: Authorized<AuthenticatedGrant>) -> String {
    proof.principal().username.clone()
}

async fn takes_a_proof_the_route_never_made(_proof: Authorized<AdminGrant>) -> &'static str {
    "unreachable"
}

impl RouteModule for Probe {
    fn name(&self) -> &'static str {
        "probe"
    }

    fn mount_api(&self, routes: RouteSet) -> RouteSet {
        routes
            .route("/p/forgetful", Requires::Repo(Action::Read), get(forgetful))
            .route("/p/asks", Requires::Repo(Action::Read), get(asks))
            .route(
                "/p/missing",
                Requires::Repo(Action::Read),
                get(not_found_without_asking),
            )
            .route(
                "/p/project",
                Requires::Project(rustryx_core::project::ProjectPermission::ViewProject),
                get(forgetful),
            )
            .route("/p/admin", Requires::Admin, get(admin_only))
            .route(
                "/p/area",
                Requires::App(AppArea::Users, Action::Write),
                post(area),
            )
            .route("/p/interactive", Requires::Interactive, get(interactive))
            .route(
                "/p/authenticated",
                Requires::Authenticated,
                get(authenticated),
            )
            .route("/p/admin-no-extractor", Requires::Admin, get(forgetful))
            .route(
                "/p/wrong-proof",
                Requires::Authenticated,
                get(takes_a_proof_the_route_never_made),
            )
            .route("/p/public", Requires::Public, get(forgetful))
    }
}

fn probe_router(state: AppState) -> Router {
    RouterBuilder::new(state).module(Arc::new(Probe)).build()
}

fn with_basic(uri: &str, method: Method, user: &str) -> http::Request<Body> {
    request(method, uri)
        .header(
            "authorization",
            format!("Basic {}", base64_of(&format!("{user}:pw"))),
        )
        .body(Body::empty())
        .expect("request")
}

fn base64_of(text: &str) -> String {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.encode(text)
}

/// Authenticator that knows `root` (an admin by role id) and `eve` (nobody).
fn state_with_people() -> AppState {
    let auth = StubAuth::new(|credentials, _| match credentials {
        rustryx_core::identity::Credentials::Basic { username, .. } if username == "root" => {
            Ok(Some(principal("root", AuthMethod::InternalJwt, "nx-admin")))
        }
        rustryx_core::identity::Credentials::Basic { username, .. } if username == "token" => Ok(
            Some(principal("token", AuthMethod::ApiToken, "nx-developer")),
        ),
        rustryx_core::identity::Credentials::Basic { username, .. } => Ok(Some(principal(
            username,
            AuthMethod::InternalJwt,
            "nx-developer",
        ))),
        _ => Err(rustryx_core::AuthError::InvalidCredentials),
    });
    let rbac = crate::session_support::AdminByName;
    crate::test_support::state_with_settings(|_| {})
        .with_authenticator(auth)
        .with_rbac(Arc::new(rbac))
}

#[tokio::test]
async fn must_not_serve_a_handler_that_never_asked_whether_the_caller_may() {
    let router = probe_router(state_with_people());
    let reply = call(&router, with_basic("/p/forgetful", Method::GET, "root")).await;
    assert_eq!(reply.status, StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(reply.error_code(), "authorization_not_enforced");
    assert!(
        !reply.text().contains("served without asking"),
        "the answer of the forgetful handler never reaches the client"
    );
    // Same for a project route: the handler has to ask `authorize_project`.
    let project = call(&router, with_basic("/p/project", Method::GET, "root")).await;
    assert_eq!(project.error_code(), "authorization_not_enforced");
}

#[tokio::test]
async fn a_handler_that_asked_is_served_and_one_that_refuses_is_not_overruled() {
    let router = probe_router(state_with_people());
    let served = call(&router, with_basic("/p/asks", Method::GET, "root")).await;
    assert_eq!(
        (served.status, served.text().as_str()),
        (StatusCode::OK, "asked")
    );
    // A 404 serves no content, the backstop leaves it alone.
    let missing = call(&router, with_basic("/p/missing", Method::GET, "root")).await;
    assert_eq!(missing.error_code(), "repository_not_found");
}

#[tokio::test]
async fn admin_routes_are_enforced_before_the_handler_runs() {
    let router = probe_router(state_with_people());
    assert_eq!(
        get_uri(&router, "/p/admin").await.status,
        StatusCode::UNAUTHORIZED
    );
    let developer = call(&router, with_basic("/p/admin", Method::GET, "dev")).await;
    assert_eq!(developer.status, StatusCode::FORBIDDEN);
    let admin = call(&router, with_basic("/p/admin", Method::GET, "root")).await;
    assert_eq!(
        (admin.status, admin.text().as_str()),
        (StatusCode::OK, "root")
    );
    // The handler that ignores the proof is protected all the same.
    let ignoring = call(
        &router,
        with_basic("/p/admin-no-extractor", Method::GET, "dev"),
    )
    .await;
    assert_eq!(ignoring.status, StatusCode::FORBIDDEN);
    let allowed = call(
        &router,
        with_basic("/p/admin-no-extractor", Method::GET, "root"),
    )
    .await;
    assert_eq!(
        allowed.status,
        StatusCode::OK,
        "the up front check counts as asking"
    );
}

#[tokio::test]
async fn application_routes_ask_the_checker_for_their_area() {
    let router = probe_router(state_with_people());
    assert_eq!(
        call(&router, with_basic("/p/area", Method::POST, "dev"))
            .await
            .status,
        StatusCode::FORBIDDEN
    );
    let reply = call(&router, with_basic("/p/area", Method::POST, "root")).await;
    assert_eq!(reply.text(), "users:Write");
}

#[tokio::test]
async fn interactive_routes_refuse_api_tokens_and_anonymous_callers() {
    let router = probe_router(state_with_people());
    assert_eq!(
        get_uri(&router, "/p/interactive").await.status,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        call(&router, with_basic("/p/interactive", Method::GET, "token"))
            .await
            .status,
        StatusCode::FORBIDDEN,
        "a leaked token cannot mint tokens"
    );
    let ok = call(&router, with_basic("/p/interactive", Method::GET, "dev")).await;
    assert_eq!(ok.text(), "dev");
    // Authenticated accepts every credential kind.
    let any = call(
        &router,
        with_basic("/p/authenticated", Method::GET, "token"),
    )
    .await;
    assert_eq!(any.text(), "token");
    assert_eq!(
        get_uri(&router, "/p/authenticated").await.status,
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn a_proof_extractor_on_a_route_declared_without_it_fails_the_request() {
    let router = probe_router(state_with_people());
    let reply = call(&router, with_basic("/p/wrong-proof", Method::GET, "root")).await;
    assert_eq!(reply.status, StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(reply.error_code(), "internal_error");
}

#[tokio::test]
async fn public_routes_carry_no_layer() {
    let router = probe_router(state_with_people());
    let reply = get_uri(&router, "/p/public").await;
    assert_eq!(reply.text(), "served without asking");
}

#[tokio::test]
async fn a_route_layer_without_the_authentication_middleware_fails_closed() {
    // A set that was never wrapped by `authenticate` has no principal.
    let state = state_with_people();
    let (router, _) = {
        let set = RouteSet::new(state.clone());
        let set = Probe.mount_api(set);
        let (router, declared) = set.into_parts();
        (router.with_state(state), declared)
    };
    let reply = call(&router, get_request("/p/admin")).await;
    assert_eq!(reply.status, StatusCode::INTERNAL_SERVER_ERROR);
}

#[tokio::test]
async fn the_catalogue_lists_every_declared_route_and_a_test_can_walk_it() {
    let (_, catalog) = RouterBuilder::new(state_with_people())
        .module(Arc::new(Probe))
        .build_with_catalog();
    assert_eq!(catalog.api.len(), 11);
    assert_eq!(catalog.artifacts.len(), 0);
    assert!(
        catalog
            .all()
            .any(|d| d.path == "/p/admin" && d.requires == Requires::Admin)
    );
}

/// The walk of the scan test: every declared route that is not public must refuse an anonymous
/// caller, whatever the method. Substitutes a sample for each `{param}`.
async fn assert_anonymous_is_refused(
    router: &Router,
    catalog: &crate::RouteCatalog,
    skip: &[&str],
) {
    for decl in catalog.all() {
        if matches!(decl.requires, Requires::Public | Requires::Delegated(_))
            || skip.contains(&decl.path.as_str())
        {
            continue;
        }
        let path = decl
            .path
            .split('/')
            .map(|segment| {
                if segment.starts_with('{') {
                    "sample"
                } else {
                    segment
                }
            })
            .collect::<Vec<_>>()
            .join("/");
        let mut refused = false;
        for method in [
            Method::GET,
            Method::POST,
            Method::PUT,
            Method::PATCH,
            Method::DELETE,
        ] {
            let reply = call(
                router,
                request(method.clone(), &path)
                    .body(Body::empty())
                    .expect("request"),
            )
            .await;
            assert!(
                !(reply.status.is_success() || reply.status.is_redirection()),
                "{method} {path} ({:?}) served an anonymous caller: {}",
                decl.requires,
                reply.status
            );
            // Either the caller was asked for credentials, or the backstop withheld the answer
            // of a handler that never asked.
            refused |= reply.status == StatusCode::UNAUTHORIZED
                || reply.text().contains("authorization_not_enforced");
        }
        assert!(
            refused,
            "{path} ({:?}) never refused an anonymous caller",
            decl.requires
        );
    }
}

#[tokio::test]
async fn must_not_serve_an_anonymous_caller_on_any_declared_route_that_needs_more() {
    let (router, catalog) = RouterBuilder::new(state_with_people())
        .module(Arc::new(Probe))
        .build_with_catalog();
    // `/p/missing` is the fixture of a handler that answers 404 before asking: it serves nothing
    // (no 2xx), and is exactly what the walk cannot call refused. The test above covers it.
    assert_anonymous_is_refused(&router, &catalog, &["/p/missing"]).await;
}

#[tokio::test]
async fn must_not_serve_an_anonymous_caller_on_any_native_auth_route_that_needs_more() {
    let env = EnvBuilder::new().build().await;
    assert!(
        env.catalog.api.len() >= 9,
        "the native module declared its routes"
    );
    assert_anonymous_is_refused(&env.router, &env.catalog, &[]).await;
    // The same router answers a signed-in caller on the interactive routes.
    let browser = Browser::new(&env);
    browser.login_alice().await;
    let csrf = browser.get("/api/v1/auth/csrf").await;
    assert_eq!(csrf.status, StatusCode::OK);
}
