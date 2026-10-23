//! `database` section.

use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::secret::ConfigSecret;

/// PostgreSQL connection settings.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct DatabaseConfig {
    /// Connection URL (`postgres://...`). A secret because it carries the password.
    pub url: Option<ConfigSecret>,
    /// Direct (non-pooled) URL for advisory locks, `LISTEN`/`NOTIFY` and migrations.
    /// Needed when `url` points at a transaction-mode pooler. Defaults to `url`.
    pub direct_url: Option<ConfigSecret>,
    /// Connections per node. Sum over all nodes must stay below the server's
    /// `max_connections`.
    pub pool_size: u32,
    /// How long a request waits for a free connection before it gets 503.
    pub acquire_timeout_secs: u64,
    /// TCP and TLS connect timeout.
    pub connect_timeout_secs: u64,
    /// `statement_timeout` applied to API connections.
    pub statement_timeout_secs: u64,
}

impl Default for DatabaseConfig {
    fn default() -> Self {
        Self {
            url: None,
            direct_url: None,
            pool_size: 20,
            acquire_timeout_secs: 5,
            connect_timeout_secs: 5,
            statement_timeout_secs: 30,
        }
    }
}

impl DatabaseConfig {
    /// Pool acquire timeout as a duration.
    #[must_use]
    pub fn acquire_timeout(&self) -> Duration {
        Duration::from_secs(self.acquire_timeout_secs)
    }

    /// Connect timeout as a duration.
    #[must_use]
    pub fn connect_timeout(&self) -> Duration {
        Duration::from_secs(self.connect_timeout_secs)
    }

    /// Statement timeout as a duration.
    #[must_use]
    pub fn statement_timeout(&self) -> Duration {
        Duration::from_secs(self.statement_timeout_secs)
    }

    /// URL for session-level features: the direct URL when set, else the pooled one.
    #[must_use]
    pub fn effective_direct_url(&self) -> Option<&ConfigSecret> {
        self.direct_url.as_ref().or(self.url.as_ref())
    }
}
