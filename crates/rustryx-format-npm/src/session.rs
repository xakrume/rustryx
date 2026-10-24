//! The routes that concern the caller and not the content: `ping`, `whoami`, the legacy login and
//! logout, and the audit endpoints.
//!
//! ## Login
//!
//! `npm login` with `--auth-type=legacy` (and scripts that do the same with `curl`) `PUT`s a user
//! document to `/-/user/org.couchdb.user:<name>` and expects a token back
//! (<https://github.com/npm/registry/blob/main/docs/user/authentication.md>). This registry does
//! not own passwords: the HTTP layer authenticates the request (Basic with a local or LDAP
//! account) before it reaches the format, so the handler only answers when the request was
//! already authenticated **with a password** as the user named in the URL. The password in the
//! body is never read for a decision and never logged. A caller that arrives with a token (an
//! API token, an OIDC bearer token) is refused: a token must not be able to mint more tokens, and
//! a Keycloak user has no password here, so Keycloak users create an API token in the UI and put
//! it into `.npmrc` as `_authToken`.
//!
//! Issuing the token is the composition root's job (the format has no token repository): give the
//! handler a [`TokenIssuer`]. Without one both routes answer `501`.
//!
//! ## Audit
//!
//! `npm audit` (and the audit that `npm install` runs) asks the registry for advisories. This
//! registry has none to give, and an error would make `npm install` print a warning or fail with
//! `--audit-level`; an empty result is the honest answer ("nothing known"), documented as such.

use async_trait::async_trait;
use axum_core::body::Body;
use http::{Response, StatusCode};
use rustryx_core::identity::{AuthMethod, Principal};
use rustryx_format_core::stream::read_bounded;
use rustryx_format_core::{ArtifactRequest, FormatContext, FormatError};
use serde_json::{Value, json};

use crate::handler::NpmHandler;
use crate::metrics;
use crate::respond;

/// Largest login document read. A real one is about 150 bytes.
const MAX_LOGIN_BODY: u64 = 16 * 1024;

/// Mints and revokes the API tokens that the legacy login and logout routes hand out.
///
/// Implemented by the composition root over the token repository and the auth crate.
#[async_trait]
pub trait TokenIssuer: Send + Sync {
    /// Issues a new API token for `principal` and returns its plaintext, which is shown once.
    ///
    /// # Errors
    /// Any [`FormatError`]; the handler passes it on.
    async fn issue(&self, principal: &Principal) -> Result<String, FormatError>;

    /// Revokes `token` if it belongs to `principal`.
    ///
    /// # Errors
    /// Any [`FormatError`]; `NotFound` for a token that is not the principal's.
    async fn revoke(&self, principal: &Principal, token: &str) -> Result<(), FormatError>;
}

/// The answer to the audit routes: no advisories. The shape depends on the route generation:
/// the bulk endpoint of npm 7 and later answers a map by package name, the two older ones a
/// report with a summary.
#[must_use]
pub fn audit_answer(req: &ArtifactRequest) -> Response<Body> {
    let value = if req.path.as_str().ends_with("advisories/bulk") {
        json!({})
    } else {
        json!({
            "actions": [],
            "advisories": {},
            "muted": [],
            "metadata": {
                "vulnerabilities": { "info": 0, "low": 0, "moderate": 0, "high": 0, "critical": 0 },
                "dependencies": 0,
                "devDependencies": 0,
                "optionalDependencies": 0,
                "totalDependencies": 0,
            },
        })
    };
    respond::json(StatusCode::OK, &value)
}

fn password_authenticated(principal: &Principal) -> bool {
    matches!(
        principal.auth_method,
        AuthMethod::BasicLocal | AuthMethod::BasicLdap
    )
}

fn not_configured() -> Response<Body> {
    respond::json(
        StatusCode::NOT_IMPLEMENTED,
        &json!({
            "error": "this registry does not issue tokens through npm login; create an API token and set it as _authToken",
            "code": "not_implemented",
        }),
    )
}

impl NpmHandler {
    /// `GET /-/whoami`.
    ///
    /// # Errors
    /// `Unauthenticated` for an anonymous caller: `npm whoami` must fail, not print `anonymous`.
    pub(crate) fn whoami(cx: &FormatContext) -> Result<Response<Body>, FormatError> {
        let principal = cx.principal();
        if principal.is_anonymous() {
            return Err(FormatError::Unauthenticated);
        }
        Ok(respond::json(
            StatusCode::OK,
            &json!({ "username": principal.username }),
        ))
    }

    /// `PUT /-/user/org.couchdb.user:<user>`.
    pub(crate) async fn login(
        &self,
        cx: &FormatContext,
        user: &str,
        body: Body,
    ) -> Result<Response<Body>, FormatError> {
        let principal = cx.principal();
        if principal.is_anonymous() {
            metrics::session("login", "unauthenticated");
            return Err(FormatError::Unauthenticated);
        }
        if !password_authenticated(principal) || principal.username != user {
            metrics::session("login", "refused");
            return Err(FormatError::Forbidden);
        }
        let document = read_bounded(body, MAX_LOGIN_BODY)
            .await
            .map_err(|_| FormatError::bad_request("the login document is missing or too large"))?;
        // The document is checked for shape and name only; its `password` field is ignored: the
        // request was authenticated by the HTTP layer before it got here.
        let value: Value = serde_json::from_slice(&document)
            .map_err(|_| FormatError::bad_request("the login document is not valid JSON"))?;
        let named = value.get("name").and_then(Value::as_str);
        if value.as_object().is_none() || named.is_some_and(|name| name != user) {
            return Err(FormatError::bad_request(
                "the login document does not describe this user",
            ));
        }
        let Some(issuer) = &self.tokens else {
            metrics::session("login", "not_configured");
            return Ok(not_configured());
        };
        let token = issuer.issue(principal).await?;
        metrics::session("login", "ok");
        Ok(respond::json(
            StatusCode::CREATED,
            &json!({
                "ok": true,
                "id": format!("org.couchdb.user:{user}"),
                // A vestige of CouchDB that clients ignore.
                "rev": "_we_dont_use_revs_any_more",
                "token": token,
            }),
        ))
    }

    /// `DELETE /-/user/token/<token>`.
    pub(crate) async fn logout(
        &self,
        cx: &FormatContext,
        token: &str,
    ) -> Result<Response<Body>, FormatError> {
        let principal = cx.principal();
        if principal.is_anonymous() {
            return Err(FormatError::Unauthenticated);
        }
        let Some(issuer) = &self.tokens else {
            metrics::session("logout", "not_configured");
            return Ok(not_configured());
        };
        issuer.revoke(principal, token).await?;
        metrics::session("logout", "ok");
        Ok(respond::ok(StatusCode::OK))
    }
}
