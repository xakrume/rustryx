//! Schema migrations and the schema version check.
//!
//! Only `rustryx migrate` calls [`migrate`]; `serve` never does (CONTRIBUTING rule 7), it
//! only calls [`schema_status`] to decide about readiness.

use std::collections::HashSet;
use std::fmt;
use std::time::Duration;

use rustryx_core::{Secret, StoreError};
use sqlx::migrate::{MigrateError, Migrator};
use sqlx::{Connection, PgConnection, PgPool};

use crate::error::{backend, map_err};
use crate::pool::parse_options;

/// Migrations embedded at compile time from `crates/rustryx-db/migrations`.
static MIGRATOR: Migrator = sqlx::migrate!("./migrations");

/// Session advisory lock key of the migrator ("RUSTRYX\0"). `sqlx` takes its own lock
/// (derived from the database name) as well; this one is ours so that the report of what
/// was applied is computed while no other migrator can run, whatever `sqlx` does inside.
const MIGRATE_LOCK_KEY: i64 = 0x5255_5354_5259_5800;
/// How long a second migrator waits for the first one.
const LOCK_WAIT: Duration = Duration::from_secs(300);
/// Connect timeout of the dedicated migration connection.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// One migration applied by this run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppliedMigration {
    /// Numeric version (the file prefix).
    pub version: i64,
    /// Description (the file name after the prefix).
    pub description: String,
}

/// Outcome of [`migrate`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MigrateReport {
    /// Migrations applied by THIS call, in order. Empty when the schema was current, for
    /// example for the caller that lost a race against a concurrent migrator.
    pub applied: Vec<AppliedMigration>,
    /// Schema version after the call.
    pub schema_version: i64,
}

/// Newest schema version this binary knows.
#[must_use]
pub fn expected_schema_version() -> i64 {
    MIGRATOR.iter().map(|m| m.version).max().unwrap_or(0)
}

/// How many migration files this binary embeds (may be less than
/// [`expected_schema_version`] when version numbers have gaps).
#[must_use]
pub fn embedded_migration_count() -> usize {
    MIGRATOR.iter().count()
}

/// Relation between the schema in the database and the binary.
///
/// The "N-1" of the expand/contract scheme is the BINARY: during a rolling update the
/// migrate job has already applied schema N while pods of release N-1 still run. Those pods
/// stay ready ([`SchemaStatus::OlderCompatible`]) as long as the newer migrations only
/// expanded the schema (`schema_contract.min_binary_schema <= expected`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SchemaStatus {
    /// Schema equals the version this binary expects.
    Current,
    /// The schema is newer than this binary, but only by expand migrations: this binary
    /// (release N-1) still works. Ready.
    OlderCompatible {
        /// Newest version applied in the database.
        applied: i64,
        /// Version this binary expects.
        expected: i64,
    },
    /// The schema is older than this binary expects: `rustryx migrate` has not run (or
    /// failed). Not ready, because queries would hit missing tables or columns.
    Behind {
        /// Newest version applied in the database (0 for an empty database).
        applied: i64,
        /// Version this binary expects.
        expected: i64,
    },
    /// The schema is newer and a contract migration removed something this binary needs.
    /// Not ready.
    Ahead {
        /// Newest version applied in the database.
        applied: i64,
        /// Version this binary expects.
        expected: i64,
    },
}

impl SchemaStatus {
    /// Whether `/readyz` may report this node ready as far as the schema is concerned.
    #[must_use]
    pub const fn is_serving(self) -> bool {
        matches!(self, Self::Current | Self::OlderCompatible { .. })
    }

    /// Stable label for health bodies and metrics.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Current => "current",
            Self::OlderCompatible { .. } => "older_compatible",
            Self::Behind { .. } => "behind",
            Self::Ahead { .. } => "ahead",
        }
    }
}

impl fmt::Display for SchemaStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Pure classification, kept apart from the queries so every branch is unit tested.
pub(crate) fn classify(applied: i64, expected: i64, min_binary_schema: i64) -> SchemaStatus {
    if applied == expected {
        SchemaStatus::Current
    } else if applied < expected {
        SchemaStatus::Behind { applied, expected }
    } else if expected >= min_binary_schema {
        SchemaStatus::OlderCompatible { applied, expected }
    } else {
        SchemaStatus::Ahead { applied, expected }
    }
}

/// Compares the schema in the database with the version this binary expects.
///
/// Costs two tiny queries; `/readyz` callers cache the result for a couple of seconds.
pub async fn schema_status(pool: &PgPool) -> Result<SchemaStatus, StoreError> {
    let expected = expected_schema_version();
    let table_exists: bool =
        sqlx::query_scalar("SELECT to_regclass('_sqlx_migrations') IS NOT NULL")
            .fetch_one(pool)
            .await
            .map_err(|e| map_err("check migration table", e))?;
    if !table_exists {
        return Ok(SchemaStatus::Behind {
            applied: 0,
            expected,
        });
    }
    // A row with success = false is a migration that started and did not finish (only
    // possible for a `-- no-transaction` file). The schema is then at the version BEFORE it,
    // whatever newer rows say, so the node stays not-ready until an operator repairs it.
    let applied: i64 = sqlx::query_scalar(
        "SELECT CASE WHEN EXISTS (SELECT 1 FROM _sqlx_migrations WHERE NOT success) \
         THEN (SELECT MIN(version) - 1 FROM _sqlx_migrations WHERE NOT success) \
         ELSE COALESCE((SELECT MAX(version) FROM _sqlx_migrations WHERE success), 0) END",
    )
    .fetch_one(pool)
    .await
    .map_err(|e| map_err("read applied schema version", e))?;
    if applied <= expected {
        return Ok(classify(applied, expected, 1));
    }
    // Only a schema newer than the binary needs the contract row, and it exists then
    // because migration 1 creates it.
    let min_binary: i64 = sqlx::query_scalar("SELECT min_binary_schema FROM schema_contract")
        .fetch_one(pool)
        .await
        .map_err(|e| map_err("read schema contract", e))?;
    Ok(classify(applied, expected, min_binary))
}

/// Applies pending migrations over a DIRECT connection and returns what was applied.
///
/// `url` must point at the server, not at a transaction-mode pooler: the session advisory
/// lock below lives on the connection and a pooler would hand the next statement to another
/// server session. Concurrent callers are serialised by `pg_advisory_lock`; the one that
/// waits finds nothing left to do and reports an empty `applied`.
pub async fn migrate(url: &Secret) -> Result<MigrateReport, StoreError> {
    let options = parse_options(url)?.application_name("rustryx-migrate");
    let mut conn = tokio::time::timeout(CONNECT_TIMEOUT, PgConnection::connect_with(&options))
        .await
        .map_err(|_| StoreError::Unavailable("database connect timed out".to_owned()))?
        .map_err(|e| map_err("connect for migration", e))?;
    let result = run_locked(&mut conn).await;
    // Closing the session releases the advisory lock even if the unlock statement could
    // not run (broken connection, cancelled future).
    let _ = conn.close().await;
    result
}

async fn run_locked(conn: &mut PgConnection) -> Result<MigrateReport, StoreError> {
    tokio::time::timeout(
        LOCK_WAIT,
        sqlx::query("SELECT pg_advisory_lock($1)")
            .bind(MIGRATE_LOCK_KEY)
            .execute(&mut *conn),
    )
    .await
    .map_err(|_| StoreError::Unavailable("timed out waiting for another migration".to_owned()))?
    .map_err(|e| map_err("take migration lock", e))?;

    let report = apply(conn).await;

    let unlock = sqlx::query("SELECT pg_advisory_unlock($1)")
        .bind(MIGRATE_LOCK_KEY)
        .execute(&mut *conn)
        .await;
    if let Err(err) = unlock {
        tracing::warn!(error = %err, "could not release the migration lock; closing the connection releases it");
    }
    report
}

async fn apply(conn: &mut PgConnection) -> Result<MigrateReport, StoreError> {
    let before = applied_versions(conn).await?;
    // `run_direct` instead of `Migrator::run`: `run` takes `impl Acquire`, and a future that
    // holds `&mut PgConnection` through it is not provably `Send` ("implementation of
    // `Acquire` is not general enough", a rustc limitation sqlx itself documents on
    // `run_direct`). A `Send` future matters because callers spawn it. The method is
    // `#[doc(hidden)]`, so the sqlx version is pinned by Cargo.lock and the migrate tests
    // exercise this exact call.
    MIGRATOR
        .run_direct(None, &mut *conn, false)
        .await
        .map_err(map_migrate_error)?;
    let applied: Vec<AppliedMigration> = MIGRATOR
        .iter()
        .filter(|m| !m.migration_type.is_down_migration() && !before.contains(&m.version))
        .map(|m| AppliedMigration {
            version: m.version,
            description: m.description.to_string(),
        })
        .collect();
    Ok(MigrateReport {
        applied,
        schema_version: expected_schema_version(),
    })
}

async fn applied_versions(conn: &mut PgConnection) -> Result<HashSet<i64>, StoreError> {
    let exists: bool = sqlx::query_scalar("SELECT to_regclass('_sqlx_migrations') IS NOT NULL")
        .fetch_one(&mut *conn)
        .await
        .map_err(|e| map_err("check migration table", e))?;
    if !exists {
        return Ok(HashSet::new());
    }
    let versions: Vec<i64> =
        sqlx::query_scalar("SELECT version FROM _sqlx_migrations WHERE success")
            .fetch_all(&mut *conn)
            .await
            .map_err(|e| map_err("read applied migrations", e))?;
    Ok(versions.into_iter().collect())
}

fn map_migrate_error(err: MigrateError) -> StoreError {
    match err {
        MigrateError::Execute(inner) => map_err("apply migration", inner),
        MigrateError::VersionMissing(_) => backend(
            "apply migrations",
            "the database holds a migration this binary does not know (binary older than schema)",
        ),
        MigrateError::VersionMismatch(_) => backend(
            "apply migrations",
            "an applied migration file was edited after it ran",
        ),
        MigrateError::Dirty(_) => backend("apply migrations", "a previous migration failed midway"),
        _ => backend("apply migrations", "migration failed"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_covers_every_relation() {
        assert_eq!(classify(6, 6, 1), SchemaStatus::Current);
        assert_eq!(
            classify(5, 6, 1),
            SchemaStatus::Behind {
                applied: 5,
                expected: 6
            }
        );
        assert_eq!(
            classify(0, 6, 1),
            SchemaStatus::Behind {
                applied: 0,
                expected: 6
            }
        );
        assert_eq!(
            classify(7, 6, 1),
            SchemaStatus::OlderCompatible {
                applied: 7,
                expected: 6
            }
        );
        assert_eq!(
            classify(7, 6, 6),
            SchemaStatus::OlderCompatible {
                applied: 7,
                expected: 6
            }
        );
        assert_eq!(
            classify(8, 6, 7),
            SchemaStatus::Ahead {
                applied: 8,
                expected: 6
            }
        );
    }

    #[test]
    fn only_current_and_older_compatible_serve() {
        assert!(SchemaStatus::Current.is_serving());
        assert!(
            SchemaStatus::OlderCompatible {
                applied: 2,
                expected: 1
            }
            .is_serving()
        );
        assert!(
            !SchemaStatus::Behind {
                applied: 0,
                expected: 1
            }
            .is_serving()
        );
        assert!(
            !SchemaStatus::Ahead {
                applied: 3,
                expected: 1
            }
            .is_serving()
        );
    }

    /// Versions are strictly increasing from 1 but NOT contiguous: the streams of wave 2 own
    /// number ranges (0020 to 0139, design section 2.7), and `sqlx` applies every embedded
    /// version that is not recorded yet, so a gap is harmless. A duplicate or an unordered
    /// pair would not be.
    #[test]
    fn embedded_migrations_are_ordered_unique_and_forward_only() {
        let versions: Vec<i64> = MIGRATOR.iter().map(|m| m.version).collect();
        assert_eq!(versions.first(), Some(&1));
        assert!(
            versions.windows(2).all(|pair| pair[0] < pair[1]),
            "{versions:?}"
        );
        assert!(
            MIGRATOR
                .iter()
                .all(|m| !m.migration_type.is_down_migration())
        );
        assert_eq!(expected_schema_version(), *versions.last().unwrap());
    }
}
