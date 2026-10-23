//! Behaviour tests against the in-process HTTPS mock of Keycloak.
//!
//! Every negative requirement of README 5, 8.3 and 8.5 has a test whose name starts with
//! `must_not_`.

// Test code: a failing helper aborts the test with its message.
#![allow(clippy::panic)]

mod bearer;
mod claims;
mod flow;
mod jwks;
mod provider;
