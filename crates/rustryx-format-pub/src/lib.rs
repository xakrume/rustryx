//! `rustryx-format-pub`: the Dart and Flutter package format.
//!
//! The protocol is the Hosted Pub Repository Specification Version 2,
//! <https://github.com/dart-lang/pub/blob/master/doc/repository-spec-v2.md>. `dart pub` and
//! `flutter pub` use the same client and the same protocol, so one [`PubHandler`] serves both.
//!
//! * [`name`], [`version`]: strict parsers of package names and `SemVer` versions;
//! * [`archive`], [`pubspec`]: reading an untrusted `.tar.gz` and its `pubspec.yaml` under hard
//!   limits, on the blocking pool;
//! * [`route`]: the URL space, parsed once from the normalised path;
//! * [`document`]: the version listing, as the one typed document behind hosted, proxy and
//!   group answers;
//! * [`merge`]: the group merge and the dependency confusion claim;
//! * [`error`]: errors in the shape `dart pub` reads, with the `401` and `403` rules;
//! * [`PubHandler`]: the [`rustryx_format_core::FormatHandler`] that ties it together.
//!
//! `docs/format-pub.md` states what is supported, what is not, and why each decision was made.

// Tests may unwrap, expect and panic: a panic is the failure signal there.
#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::too_many_lines,
        clippy::assert_is_empty,
        clippy::duration_suboptimal_units,
        clippy::unused_self,
        clippy::format_push_string,
        clippy::byte_char_slices,
        clippy::needless_pass_by_value,
        clippy::items_after_statements
    )
)]
// Errors are documented once per type.
#![allow(clippy::missing_errors_doc)]

pub mod accept;
pub mod archive;
mod blobs;
pub mod blocking;
pub mod config;
pub mod document;
pub mod error;
mod handler;
mod hosted;
pub mod merge;
pub mod metrics;
pub mod name;
mod proxy;
pub mod pubspec;
mod response;
pub mod route;
pub mod testing;
mod time_text;
pub mod version;

pub use config::PubConfig;
pub use handler::PubHandler;
pub use merge::{PubClaimer, PubMerger};
pub use name::PackageName;
pub use version::PubVersion;
