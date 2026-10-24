//! `PyPI` format for Rustryx: the simple repository API (PEP 503, 691, 592, 658, 700, 714),
//! the legacy upload API, hosted, proxy and group repositories.
//!
//! The protocol parsers (names, versions, file names, core metadata, ZIP) live in their own
//! modules and are written to never panic on hostile input; `docs/format-pypi.md` lists the
//! protocol surface, the deviations and the decisions.

// Errors are documented once per type; the handler functions return `FormatError`.
#![allow(clippy::missing_errors_doc)]
// Tests may unwrap, expect and panic: a panic is the failure signal there.
#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::too_many_lines,
        clippy::format_push_string,
        clippy::cast_possible_truncation
    )
)]

mod blocking;
mod error;
pub mod filename;
pub mod form;
mod group;
mod handler;
mod hosted;
mod jsonapi;
pub mod metadata;
pub mod metrics;
pub mod multipart;
pub mod name;
pub mod negotiate;
pub mod page;
mod pages;
mod proxy;
mod route;
pub mod sidecar;
pub mod simple_html;
pub mod simple_json;
mod upload;
pub mod version;
pub mod zipmeta;

pub use handler::{PypiConfig, PypiHandler};
