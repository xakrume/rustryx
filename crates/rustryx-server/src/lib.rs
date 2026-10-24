//! The Rustryx server: command line, runtime, listener and graceful shutdown.
//!
//! This crate is the composition root. Everything else is a library that knows
//! nothing about processes; here the pieces are loaded, wired and run. What is
//! wired and what is not is listed in `docs/server.md` and concentrated in [`wiring`].
//!
//! `anyhow` is used here and nowhere else in the workspace: errors in this crate
//! end at `main`, where they are printed and turned into an exit code.

// Tests may unwrap, expect and panic: a panic is the failure signal there. They also
// tweak a field or two of a default configuration, where struct-update syntax would
// bury what the test is about.
#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::field_reassign_with_default,
        clippy::assert_is_empty
    )
)]

pub mod cli;
pub mod commands;
pub mod listener;
pub mod migrate;
pub mod serve;
pub mod shutdown;
pub mod signals;
pub mod supervisor;
pub mod wiring;

#[cfg(test)]
mod http_test;

pub use cli::{Cli, Command};
pub use commands::{CliError, execute};
