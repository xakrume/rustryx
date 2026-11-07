//! Integration tests against a real PostgreSQL 16 started with testcontainers.
//!
//! One binary on purpose: all modules share one container (see `support`), because
//! starting a container per test file would dominate the run time.

// Tests unwrap and expect: a panic is the failure signal. Test functions are long because
// they set up real rows, and the crate-level lints for production code do not fit here.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::too_many_lines,
    clippy::similar_names,
    clippy::assert_is_empty,
    clippy::collapsible_if,
    missing_docs
)]

mod fixtures;
mod foundation_tests;
mod guard_tests;
mod immutability_tests;
mod lifecycle_hold_tests;
mod lifecycle_run_tests;
mod registry_tests;
mod replication_tests;
mod scan_policy_tests;
mod scanner_tests;
mod support;
mod webhook_delivery_tests;
mod webhook_tests;

mod allowlist_tests;
mod asset_tests;
mod audit_read_tests;
mod audit_tests;
mod blob_group_tests;
mod bootstrap_tests;
mod bus_tests;
mod cleanup_tests;
mod closed_pool_tests;
mod component_tests;
mod downloads_tests;
mod error_tests;
mod gc_tests;
mod label_tests;
mod lease_tests;
mod link_gc_tests;
mod lock_tests;
mod login_tests;
mod migrate_tests;
mod notification_tests;
mod outbox_tests;
mod privilege_tests;
mod project_member_tests;
mod promotion_tests;
mod publish_with_tests;
mod quota_tests;
mod rbac_tests;
mod repository_tests;
mod repository_wave2_tests;
mod retention_tests;
mod revoked_tests;
mod robot_tests;
mod routing_tests;
mod system_config_tests;
mod token_tests;
mod trust_tests;
mod user_tests;
