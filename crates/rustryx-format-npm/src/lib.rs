//! The npm registry format of Rustryx.
//!
//! [`NpmHandler`] implements the format core's `FormatHandler` for hosted, proxy and group
//! repositories of format `npm`. Register one instance with the format registry:
//!
//! ```ignore
//! registry.register(Arc::new(NpmHandler::new().with_token_issuer(issuer)))?;
//! ```
//!
//! What is here, bottom up:
//!
//! * strict, panic-free parsers: [`name`] (package names with scopes), [`version`] (`SemVer` and
//!   dist-tag names), [`disttags`], [`packument`] (the package document, full and abbreviated),
//!   [`publish`] (the publish document), [`scan`] (the incremental scanner that splits a publish
//!   body into metadata and a decoded tarball stream) and [`route`] (the URL space);
//! * [`attach`]: the tarball of a publish as a byte stream for the hosted upload pipeline, with
//!   the SHA-512 check;
//! * [`docstore`]: how a hosted package document changes without losing an update;
//! * the handler itself and its three repository types, [`group`] (merge and name claims) and
//!   [`session`] (ping, whoami, login, audit).
//!
//! The protocol surface, the deviations from the public registry and the decisions behind them
//! are in `docs/format-npm.md`.

// Errors are documented once per type; a `# Errors` section on every function that returns the
// format core's `FormatError` would repeat the same sentence.
#![allow(clippy::missing_errors_doc)]
// Tests may unwrap, expect and panic: a panic is the failure signal there.
#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::too_many_lines,
        clippy::similar_names
    )
)]

pub mod attach;
pub mod blocking;
pub mod disttags;
pub mod docstore;
pub mod group;
pub mod handler;
mod hosted;
mod hosted_write;
pub mod metrics;
pub mod name;
pub mod packument;
mod proxy;
pub mod publish;
mod respond;
pub mod route;
pub mod scan;
pub mod search;
pub mod session;
mod unpublish;
pub mod variant;
pub mod version;
mod view;

pub use handler::{NpmConfig, NpmHandler};
pub use name::{NameError, PackageName};
pub use packument::{Packument, PackumentError, ParseMode};
pub use session::TokenIssuer;
pub use variant::Variant;
