//! Connection settings.

use std::time::Duration;

use rustryx_core::Secret;

/// Default pool size per node. The sum over all nodes plus a reserve must stay below the
/// server's `max_connections` (SRE-27).
pub const DEFAULT_MAX_CONNECTIONS: u32 = 20;
/// Default wait for a free pooled connection before the caller gets `Unavailable` (503).
pub const DEFAULT_ACQUIRE_TIMEOUT: Duration = Duration::from_secs(5);
/// Default `statement_timeout` for pooled (API) connections.
pub const DEFAULT_STATEMENT_TIMEOUT: Duration = Duration::from_secs(30);

/// Settings of the pooled connection. Deliberately independent of `rustryx-config`: the
/// composition root maps its configuration onto this struct.
///
/// `Debug` never prints the URL (it may carry the password), because `url` is a [`Secret`].
#[derive(Debug, Clone)]
pub struct PgConfig {
    /// `postgres://` URL of the pooler or the server. Redacted in `Debug`.
    pub url: Secret,
    /// Upper bound of pooled connections.
    pub max_connections: u32,
    /// How long a caller waits for a pooled connection.
    pub acquire_timeout: Duration,
    /// Server side `statement_timeout` set on every pooled connection. `None` leaves the
    /// server default.
    pub statement_timeout: Option<Duration>,
    /// Shown in `pg_stat_activity`, so operators can tell nodes and tools apart.
    pub application_name: String,
}

impl PgConfig {
    /// Settings with the documented defaults for `url`.
    #[must_use]
    pub fn new(url: Secret) -> Self {
        Self {
            url,
            max_connections: DEFAULT_MAX_CONNECTIONS,
            acquire_timeout: DEFAULT_ACQUIRE_TIMEOUT,
            statement_timeout: Some(DEFAULT_STATEMENT_TIMEOUT),
            application_name: "rustryx".to_owned(),
        }
    }
}
