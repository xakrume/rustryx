//! The transaction frame of the typed, policy enforcing write methods.
//!
//! `publish_with`, `link`, `delete_by_path_checked` and `delete_checked` all run as: validate
//! the options, open a transaction, apply the transaction local settings the SQL guards read,
//! do the work, write the outbox events, commit. Keeping the frame in one place is what makes
//! "a refusal leaves nothing behind" true for every method: the work happens between `begin`
//! and `commit`, and any `Err` before `commit` drops the transaction, which rolls it back.

use rustryx_core::error::MutationError;
use rustryx_core::model::WriteOptions;
use sqlx::{PgPool, Postgres, Transaction};

use crate::error::map_err;
use crate::outbox;

/// Name of the transaction local setting the SQL guards honour as "an administrator driven run
/// may bypass immutability".
pub(crate) const OVERRIDE_IMMUTABLE_SETTING: &str = "rustryx.override_immutable";

/// Validates `options`, opens the transaction and applies the settings.
///
/// The override setting is written only when the option is set, and with `is_local = true`
/// (`set_config(.., true)`): it dies with the transaction, so a pooled connection can never
/// carry the bypass into the next request. Writing `'off'` for the default would be harmless
/// but would add a round trip to every publish.
pub(crate) async fn begin(
    pool: &PgPool,
    options: &WriteOptions,
    context: &'static str,
) -> Result<Transaction<'static, Postgres>, MutationError> {
    for event in &options.events {
        event.validate()?;
    }
    let mut tx = pool.begin().await.map_err(|e| map_err(context, e))?;
    if options.override_immutable {
        sqlx::query("SELECT set_config($1, 'on', true)")
            .bind(OVERRIDE_IMMUTABLE_SETTING)
            .execute(&mut *tx)
            .await
            .map_err(|e| map_err(context, e))?;
    }
    Ok(tx)
}

/// Writes the events of `options` in the open transaction and commits it.
pub(crate) async fn commit(
    mut tx: Transaction<'static, Postgres>,
    options: &WriteOptions,
    context: &'static str,
) -> Result<(), MutationError> {
    outbox::insert_events(&mut tx, &options.events).await?;
    tx.commit().await.map_err(|e| map_err(context, e))?;
    Ok(())
}
