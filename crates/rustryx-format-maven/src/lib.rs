//! Rustryx crate `rustryx-format-maven`.

#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::too_many_lines,
        clippy::items_after_statements
    )
)]

mod audit;
mod blocking;
pub mod checksum;
pub mod config;
#[doc(hidden)]
pub mod fuzzing;
mod group;
mod handler;
mod hosted;
pub mod layout;
pub mod metadata;
pub mod metrics;
mod proxy;
mod request;
mod response;
pub mod version;

pub use config::{MavenConfig, VersionPolicy};
pub use handler::MavenHandler;
