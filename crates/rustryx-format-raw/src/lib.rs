//! The `raw` repository format: arbitrary files under `/repository/<name>/...`.
//!
//! There is no public protocol specification for plain file repositories, so the behaviour is
//! defined here and in `docs/format-raw.md`:
//!
//! * `PUT <path>` stores a file, `GET` and `HEAD` read it (ranges, conditional requests,
//!   checksum headers come from the shared hosted pipeline), `DELETE` removes it;
//! * a path is either a file or a directory prefix, never both, and the trailing slash decides
//!   what a request addresses (see [`target`]);
//! * `GET <dir>/` lists a directory as HTML or, with `Accept: application/json`, as paged JSON
//!   ([`listing`]), filtered by what the caller may read;
//! * `<file>.sha1`, `<file>.sha256` and `<file>.md5` are computed from the stored digests when
//!   no file of that name exists ([`sidecar`]);
//! * content a browser would execute (html, svg, xml, scripts) is served as an attachment
//!   unless the repository opts in to inline rendering ([`config`]);
//! * a group answers with the first member that has the path, a proxy caches by path.
//!
//! The crate is a [`rustryx_format_core::FormatHandler`]; the composition root registers
//! [`RawHandler`] with one line.

// Errors are documented once on `FormatError`; repeating them on every helper adds noise.
#![allow(clippy::missing_errors_doc)]
// Tests may unwrap, expect and panic: a panic is the failure signal there.
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

pub mod accept;
pub mod apk_index;
pub mod apt_index;
pub mod config;
mod disposition;
mod handler;
pub mod listing;
pub mod rpm_index;
pub mod sidecar;
pub mod target;
mod write;

pub use config::{ContentDisposition, RawConfig};
pub use handler::RawHandler;
pub use target::{Target, parse_target};
