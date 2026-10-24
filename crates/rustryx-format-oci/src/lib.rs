//! Rustryx OCI Distribution engine: one handler for the `docker` and `oci` repository
//! labels, which also serves Helm charts pushed with `helm push oci://`.
//!
//! See `docs/format-oci.md` for the supported surface, the storage model and what the
//! server crate has to mount.

#![allow(
    clippy::missing_errors_doc,
    reason = "errors are documented once per type; the handlers return OciError whose codes are listed in docs/format-oci.md"
)]
#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::bool_assert_comparison,
        clippy::assert_is_empty
    )
)]

mod audit;
mod blobs;
pub mod catalog;
mod classify;
pub mod config;
pub mod error;
mod group;
mod handler;
pub mod headers;
mod listing;
mod manifests;
mod mount;
mod ops;
pub mod parse;
mod proxy;
pub mod store;
pub mod token;
mod uploads;

pub use catalog::{CatalogPage, CatalogService, catalog_response};
pub use config::{OciConfig, RepositoryOptions};
pub use error::{OciError, OciErrorCode};
pub use handler::OciHandler;
pub use token::{
    HmacTokenCodec, IssuedToken, OciTokenService, RegistryTokenClaims, TokenError, TokenIssuer,
    TokenRequest, TokenRequestError, TokenServiceConfig, TokenVerifier, bearer_challenge,
    challenge_scope, ping_challenge, ping_ok, token_response,
};
