//! Request bodies and queries of the native authentication endpoints.

use serde::Deserialize;

/// `LoginRequest`. No `Debug` on purpose: the derived one would print the password.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct LoginRequest {
    pub username: String,
    pub password: String,
}

/// The query of `GET /auth/oidc/login`.
#[derive(Debug, Deserialize)]
pub(crate) struct OidcLoginQuery {
    pub return_to: Option<String>,
}

/// The query of `GET /auth/oidc/callback`. Unknown parameters are ignored here (an identity
/// provider may add some), unlike the strict queries of the other endpoints.
#[derive(Debug, Deserialize)]
pub(crate) struct OidcCallbackQuery {
    pub code: Option<String>,
    pub state: Option<String>,
    pub error: Option<String>,
}
