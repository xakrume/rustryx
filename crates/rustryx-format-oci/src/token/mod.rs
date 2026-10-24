//! The token service of the registry: scope strings in, signed short lived bearer tokens out,
//! plus the verification of those tokens on `/v2/*`.
//!
//! The flow a registry client follows (Docker token authentication,
//! <https://distribution.github.io/distribution/spec/auth/token/>):
//!
//! 1. a request without a usable token gets `401` with
//!    `WWW-Authenticate: Bearer realm=".../v2/token",service="...",scope="..."`
//!    ([`bearer_challenge`]);
//! 2. the client calls the token endpoint with its Basic credentials (or none) and the scope
//!    ([`token_response`]), which answers with a token that carries what RBAC allows;
//! 3. the client retries with `Authorization: Bearer <token>`, which the server verifies
//!    ([`OciTokenService::verify`]) and turns into a principal; the engine then authorizes the
//!    request again through RBAC, so the token can only narrow access.

mod claims;
mod codec;
mod endpoint;
mod service;

pub use crate::parse::scope::Action;
pub use claims::{
    AccessGrant, GrantResource, RegistryTokenClaims, TokenError, TokenIssuer, TokenVerifier,
};
pub use codec::{HmacTokenCodec, MIN_KEY_LEN};
pub use endpoint::{bearer_challenge, challenge_scope, ping_challenge, ping_ok, token_response};
pub use service::{
    IssuedToken, OciTokenService, TokenRequest, TokenRequestError, TokenServiceConfig,
};
