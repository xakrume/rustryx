//! Classic Helm chart repositories (`index.yaml` and `charts/*.tgz`).
//!
//! [`HelmHandler`] is the [`rustryx_format_core::FormatHandler`] for [`rustryx_core::model::RepoFormat::Helm`].
//! The chart archive rules live in [`chart`]. Behaviour that the tests pin down is written in
//! `docs/format-helm.md`.
#![allow(clippy::missing_errors_doc)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

mod blocking;
pub mod chart;
mod group;
mod handler;
mod index;
pub mod model;
pub mod names;
mod response;
mod route;
pub mod testing;
pub mod yaml;

pub use handler::HelmHandler;
