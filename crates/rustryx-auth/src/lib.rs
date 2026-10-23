//! Authentication for Rustryx: local accounts, API tokens, internal JWT,
//! Keycloak (OIDC) and LDAP, plus group resolution.
//!
//! This crate answers "who is calling" and "which external groups do they have".
//! It never decides what they may do: that is `rustryx-rbac`.

// Tests may unwrap and expect: a panic is the failure signal there.
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

pub mod basic;
pub mod chain;
pub mod groups;
pub mod internal_jwt;
pub mod ldap;
pub mod login_guard;
pub mod oidc;
pub mod passwords;
pub mod tokens;
