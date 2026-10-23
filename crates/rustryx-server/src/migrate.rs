//! The migration port of the server.
//!
//! The database crate does not exist yet. The `migrate` subcommand therefore goes
//! through this small trait, and the composition root decides what implements it
//! ([`crate::wiring::migrator`]). Until then [`NotWiredMigrator`] fails with a
//! message that says so, rather than the command silently doing nothing.

use async_trait::async_trait;
use rustryx_config::AppConfig;

/// Applies the database schema. One process at a time (the implementation takes a
/// PostgreSQL advisory lock on a direct connection), forward-only, expand/contract.
#[async_trait]
pub trait Migrator: Send + Sync {
    /// Runs all pending migrations and returns a short human readable summary.
    ///
    /// Cancel safety: not cancel safe mid-migration by design; each migration is
    /// one transaction, so cancellation leaves the previous version intact.
    async fn migrate(&self, config: &AppConfig) -> anyhow::Result<String>;
}

/// Placeholder until the database crate is wired.
#[derive(Debug, Clone, Copy, Default)]
pub struct NotWiredMigrator;

#[async_trait]
impl Migrator for NotWiredMigrator {
    async fn migrate(&self, _config: &AppConfig) -> anyhow::Result<String> {
        anyhow::bail!(
            "migrations are not wired into this build: the database crate has not been \
             integrated into the composition root yet (see docs/server.md, section \
             \"What the lead must wire\")"
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn the_placeholder_says_clearly_that_it_is_not_wired() {
        let error = NotWiredMigrator
            .migrate(&AppConfig::default())
            .await
            .expect_err("must fail");
        let text = error.to_string();
        assert!(text.contains("not wired"), "{text}");
        assert!(text.contains("docs/server.md"), "{text}");
    }
}
