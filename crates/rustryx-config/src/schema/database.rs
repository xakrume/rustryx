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
    /// AES-256 key (64 hexadecimal characters) that seals secrets stored in the database
    /// (upstream passwords of proxy repositories). Shared by all nodes.
    pub sealing_key: Option<ConfigSecret>,
    /// Id written into every sealed value so that the right key is found after a rotation.
    pub sealing_key_id: String,
    /// Retired sealing keys that still open old values, as `id:hex` entries separated by
    /// commas, spaces or new lines.
    pub sealing_previous_keys: Option<ConfigSecret>,
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
            sealing_key: None,
            sealing_key_id: "k1".to_owned(),
            sealing_previous_keys: None,
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
