//! The table of every configuration key.
//!
//! One entry per leaf key: dotted path, value kind and description. Three things
//! are derived from it so that they cannot drift apart:
//!
//! - the environment variable name (`RUSTRYX_` + path with `.` replaced by `_`,
//!   upper-cased; keys keep their own underscores),
//! - how an environment value is parsed (by [`Kind`]),
//! - the key reference in `docs/config.md` (generated and compared by a test).
//!
//! Tests keep the table equal to the leaves of the serialized default
//! configuration, so adding a field to the schema without a table entry fails.

use std::collections::{BTreeSet, HashMap};

/// Prefix of every Rustryx environment variable.
pub const ENV_PREFIX: &str = "RUSTRYX_";

/// Kind of a leaf value, used to parse environment values and to check YAML types.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// `true` or `false`.
    Bool,
    /// Non-negative integer.
    UInt,
    /// Floating point number.
    Float,
    /// Text.
    Str,
    /// List of text values. In an environment variable: comma separated.
    StrList,
    /// Text that is never printed and may come from a `*_FILE` variable.
    Secret,
    /// One of a fixed set of lowercase words.
    Enum(&'static [&'static str]),
}

/// One leaf key.
#[derive(Debug, Clone, Copy)]
pub struct KeySpec {
    /// Dotted path, for example `auth.oidc.client_id`.
    pub path: &'static str,
    /// Value kind.
    pub kind: Kind,
    /// Short description for the generated reference.
    pub doc: &'static str,
}

impl KeySpec {
    /// Environment variable that overrides this key.
    #[must_use]
    pub fn env_name(&self) -> String {
        env_name_for(self.path)
    }

    /// Environment variable that points at a file holding the value (secrets only).
    #[must_use]
    pub fn env_file_name(&self) -> Option<String> {
        self.is_secret()
            .then(|| format!("{}_FILE", self.env_name()))
    }

    /// True for secret keys.
    #[must_use]
    pub fn is_secret(&self) -> bool {
        self.kind == Kind::Secret
    }
}

/// Environment variable name for a dotted path.
#[must_use]
pub fn env_name_for(path: &str) -> String {
    let mut name = String::with_capacity(ENV_PREFIX.len() + path.len());
    name.push_str(ENV_PREFIX);
    name.extend(path.chars().map(|c| {
        if c == '.' {
            '_'
        } else {
            c.to_ascii_uppercase()
        }
    }));
    name
}

const ENV_ENVIRONMENT: &[&str] = &["production", "development"];
const BLOB_KINDS: &[&str] = &["local", "s3"];
const JIT_MODES: &[&str] = &["jit", "allowlist", "manual"];
const GROUP_STRATEGIES: &[&str] = &["member_of", "member_search"];
const LOCK_BACKENDS: &[&str] = &["postgres", "redis"];
const LOG_FORMATS: &[&str] = &["json", "text"];
const OTLP_PROTOCOLS: &[&str] = &["grpc", "http"];

macro_rules! key {
    ($path:literal, $kind:expr, $doc:literal) => {
        KeySpec {
            path: $path,
            kind: $kind,
            doc: $doc,
        }
    };
}

/// Every leaf key of the configuration, in documentation order.
pub const KEYS: &[KeySpec] = &[
    key!(
        "environment",
        Kind::Enum(ENV_ENVIRONMENT),
        "Deployment environment. `production` refuses default, empty and well-known weak credentials."
    ),
    key!(
        "server.listen_addr",
        Kind::Str,
        "Address of the single HTTP listener (API, formats, probes, metrics)."
    ),
    key!(
        "server.base_url",
        Kind::Str,
        "Public base URL without path. Required when OIDC is enabled (the redirect URI is derived from it)."
    ),
    key!(
        "server.shutdown_timeout_secs",
        Kind::UInt,
        "Drain budget after SIGTERM, in seconds."
    ),
    key!(
        "server.drain_delay_secs",
        Kind::UInt,
        "Seconds to keep serving after readiness flipped to 503, before the listener stops."
    ),
    key!(
        "server.trusted_proxies",
        Kind::StrList,
        "CIDRs of peers allowed to set `X-Forwarded-For`. `/0` is refused."
    ),
    key!(
        "server.cors_allowed_origins",
        Kind::StrList,
        "Exact origins allowed by CORS. Empty sends no CORS headers. `*` is refused."
    ),
    key!(
        "database.url",
        Kind::Secret,
        "PostgreSQL URL (`postgres://...`). Required."
    ),
    key!(
        "database.direct_url",
        Kind::Secret,
        "Non-pooled URL for advisory locks, LISTEN/NOTIFY and migrations. Defaults to `database.url`."
    ),
    key!(
        "database.pool_size",
        Kind::UInt,
        "Connections per node (1 to 500)."
    ),
    key!(
        "database.acquire_timeout_secs",
        Kind::UInt,
        "Wait for a free connection before the request gets 503."
    ),
    key!(
        "database.connect_timeout_secs",
        Kind::UInt,
        "TCP and TLS connect timeout."
    ),
    key!(
        "database.statement_timeout_secs",
        Kind::UInt,
        "`statement_timeout` for API connections."
    ),
    key!(
        "blob.store_name",
        Kind::Str,
        "Name of the default blob store."
    ),
    key!(
        "blob.kind",
        Kind::Enum(BLOB_KINDS),
        "Kind of the default blob store."
    ),
    key!(
        "blob.local_path",
        Kind::Str,
        "Root directory of a local store. Absolute in production."
    ),
    key!(
        "blob.local_min_free_percent",
        Kind::UInt,
        "Free space percent below which a local store rejects writes (0 disables)."
    ),
    key!(
        "blob.s3_endpoint",
        Kind::Str,
        "S3 endpoint URL. Required for `s3`."
    ),
    key!("blob.s3_bucket", Kind::Str, "S3 bucket. Required for `s3`."),
    key!("blob.s3_region", Kind::Str, "S3 region. Required for `s3`."),
    key!(
        "blob.s3_access_key",
        Kind::Secret,
        "S3 access key id. Required for `s3`."
    ),
    key!(
        "blob.s3_secret_key",
        Kind::Secret,
        "S3 secret access key. Required for `s3`."
    ),
    key!(
        "blob.s3_path_style",
        Kind::Bool,
        "Path-style addressing (MinIO, Ceph)."
    ),
    key!(
        "blob.s3_part_size_mb",
        Kind::UInt,
        "Multipart part size in MiB (5 to 5120)."
    ),
    key!(
        "auth.anonymous_enabled",
        Kind::Bool,
        "Global switch for anonymous access. A repository must also allow it."
    ),
    key!(
        "auth.allow_local_role_supplement",
        Kind::Bool,
        "Let manually assigned roles add to roles derived from groups."
    ),
    key!(
        "auth.jit_mode",
        Kind::Enum(JIT_MODES),
        "Account creation policy for identity-provider users."
    ),
    key!(
        "auth.internal_jwt_key",
        Kind::Secret,
        "PEM Ed25519 private key signing internal JWTs. Required in production, same on every node."
    ),
    key!(
        "auth.cookie_key",
        Kind::Secret,
        "Base64 of 32 random bytes sealing the OIDC state cookie. Required in production, same on every node."
    ),
    key!(
        "auth.session.idle_timeout_secs",
        Kind::UInt,
        "UI session idle timeout, at most 1800."
    ),
    key!(
        "auth.session.absolute_timeout_secs",
        Kind::UInt,
        "UI session absolute lifetime, at most 28800."
    ),
    key!(
        "auth.bootstrap_admin.username",
        Kind::Str,
        "Login of the local break-glass administrator."
    ),
    key!(
        "auth.bootstrap_admin.password",
        Kind::Secret,
        "Initial password of that administrator. Required in production, at least 12 characters, not a known weak value."
    ),
    key!(
        "auth.oidc.enabled",
        Kind::Bool,
        "Turn OIDC (Keycloak) login and bearer validation on."
    ),
    key!(
        "auth.oidc.issuer",
        Kind::Str,
        "Issuer URL, compared byte for byte with `iss`. https in production."
    ),
    key!(
        "auth.oidc.public_issuer_url",
        Kind::Str,
        "Issuer URL as seen by browsers when it differs from `issuer`."
    ),
    key!(
        "auth.oidc.client_id",
        Kind::Str,
        "Client id of the confidential UI client, also the required `aud`."
    ),
    key!(
        "auth.oidc.client_secret",
        Kind::Secret,
        "Client secret of the confidential UI client."
    ),
    key!(
        "auth.oidc.admin_groups",
        Kind::StrList,
        "Full group paths (leading `/`) granting `nx-admin` by exact match."
    ),
    key!(
        "auth.oidc.default_role",
        Kind::Str,
        "Role for users without a mapped group. Unset refuses them. `nx-admin` is refused."
    ),
    key!(
        "auth.oidc.groups_claim",
        Kind::Str,
        "Top-level claim holding group paths."
    ),
    key!(
        "auth.oidc.scopes",
        Kind::StrList,
        "Scopes requested at login. Must include `openid`, must not include `offline_access`."
    ),
    key!(
        "auth.oidc.clock_skew_secs",
        Kind::UInt,
        "Clock skew for `exp`, `nbf`, `iat`, at most 60."
    ),
    key!("auth.ldap.enabled", Kind::Bool, "Turn LDAP login on."),
    key!(
        "auth.ldap.url",
        Kind::Str,
        "`ldaps://host[:port]`, or `ldap://host[:port]` with `starttls: true`. Plain `ldap://` fails validation."
    ),
    key!(
        "auth.ldap.starttls",
        Kind::Bool,
        "Upgrade an `ldap://` connection with StartTLS."
    ),
    key!(
        "auth.ldap.bind_dn",
        Kind::Str,
        "Service account DN for the user search."
    ),
    key!(
        "auth.ldap.bind_password",
        Kind::Secret,
        "Service account password."
    ),
    key!(
        "auth.ldap.user_base_dn",
        Kind::Str,
        "Base DN of the user search."
    ),
    key!(
        "auth.ldap.search_filter",
        Kind::Str,
        "User search filter containing `{username}`."
    ),
    key!(
        "auth.ldap.group_strategy",
        Kind::Enum(GROUP_STRATEGIES),
        "How group membership is read."
    ),
    key!(
        "auth.ldap.group_attribute",
        Kind::Str,
        "User attribute listing group DNs (`member_of`)."
    ),
    key!(
        "auth.ldap.group_base_dn",
        Kind::Str,
        "Base DN of the group search (`member_search`)."
    ),
    key!(
        "auth.ldap.group_filter",
        Kind::Str,
        "Group search filter containing `{dn}` (`member_search`)."
    ),
    key!(
        "auth.ldap.admin_groups",
        Kind::StrList,
        "Group DNs granting `nx-admin` by exact match."
    ),
    key!(
        "auth.ldap.ca_file",
        Kind::Str,
        "PEM CA bundle for the server certificate. Must be readable when LDAP is enabled."
    ),
    key!(
        "auth.ldap.connect_timeout_secs",
        Kind::UInt,
        "Connect timeout."
    ),
    key!("auth.ldap.bind_timeout_secs", Kind::UInt, "Bind timeout."),
    key!(
        "auth.ldap.search_timeout_secs",
        Kind::UInt,
        "Search timeout."
    ),
    key!(
        "auth.ldap.groups_cache_ttl_secs",
        Kind::UInt,
        "Lifetime of confirmed group membership, at most 300."
    ),
    key!(
        "auth.ldap.nested_group_depth",
        Kind::UInt,
        "Maximum depth when expanding nested groups."
    ),
    key!(
        "auth.ldap.max_groups",
        Kind::UInt,
        "Maximum groups collected for one user."
    ),
    key!(
        "lock.backend",
        Kind::Enum(LOCK_BACKENDS),
        "Distributed lock backend for singleton jobs. Fixed for the life of the process."
    ),
    key!(
        "lock.redis_url",
        Kind::Secret,
        "Redis URL. Required for the `redis` backend."
    ),
    key!(
        "lock.ttl_secs",
        Kind::UInt,
        "Lock TTL, renewed every TTL/3 (5 to 300)."
    ),
    key!(
        "limits.max_body_mb",
        Kind::UInt,
        "Largest JSON/metadata request body in MiB."
    ),
    key!(
        "limits.max_blob_bytes",
        Kind::UInt,
        "Largest artifact body in bytes; larger uploads get 413."
    ),
    key!(
        "limits.request_timeout_secs",
        Kind::UInt,
        "Ceiling for producing the response head of one request."
    ),
    key!(
        "limits.header_read_timeout_secs",
        Kind::UInt,
        "Time a client may take to send request headers."
    ),
    key!(
        "limits.body_idle_timeout_secs",
        Kind::UInt,
        "Idle time allowed between body chunks."
    ),
    key!(
        "limits.max_connections",
        Kind::UInt,
        "Open connections per node."
    ),
    key!(
        "limits.max_in_flight_requests",
        Kind::UInt,
        "Concurrent requests per node; beyond this requests get 503."
    ),
    key!(
        "limits.blocking_permits",
        Kind::UInt,
        "Permits for blocking work. 0 means 2 x CPUs."
    ),
    key!(
        "limits.password_permits",
        Kind::UInt,
        "Permits for password hashing. 0 means physical cores."
    ),
    key!(
        "limits.permit_wait_ms",
        Kind::UInt,
        "Wait for a blocking permit before the request gets 503."
    ),
    key!(
        "limits.rate_limit.enabled",
        Kind::Bool,
        "Turn the per-client rate limiter on."
    ),
    key!(
        "limits.rate_limit.requests_per_second",
        Kind::UInt,
        "Sustained requests per second per client."
    ),
    key!("limits.rate_limit.burst", Kind::UInt, "Burst size."),
    key!(
        "upstream.allowed_schemes",
        Kind::StrList,
        "Schemes a proxy `remote_url` may use. Must include `https`."
    ),
    key!(
        "upstream.allow_insecure_http_hosts",
        Kind::StrList,
        "Hosts reachable over plain `http`. Needed when `http` is an allowed scheme."
    ),
    key!(
        "upstream.allowed_hosts",
        Kind::StrList,
        "When not empty, only these hosts (exact or `*.example.com`) may be upstreams."
    ),
    key!(
        "upstream.allow_private_cidrs",
        Kind::StrList,
        "Internal CIDRs allowed despite the SSRF filter. Loopback and metadata addresses are refused."
    ),
    key!(
        "upstream.max_redirects",
        Kind::UInt,
        "Redirects followed per request, at most 3."
    ),
    key!(
        "upstream.connect_timeout_secs",
        Kind::UInt,
        "Connect timeout."
    ),
    key!(
        "upstream.metadata_timeout_secs",
        Kind::UInt,
        "Timeout of metadata requests."
    ),
    key!(
        "upstream.idle_timeout_secs",
        Kind::UInt,
        "Idle timeout of a streaming body."
    ),
    key!(
        "upstream.max_attempts",
        Kind::UInt,
        "Attempts for GET and HEAD including the first, at most 3."
    ),
    key!("gc.enabled", Kind::Bool, "Turn blob garbage collection on."),
    key!("gc.interval_secs", Kind::UInt, "Seconds between runs."),
    key!(
        "gc.min_age_secs",
        Kind::UInt,
        "A blob younger than this is never deleted. Must exceed `limits.request_timeout_secs`."
    ),
    key!("gc.batch_size", Kind::UInt, "Blobs examined per batch."),
    key!(
        "observability.log_level",
        Kind::Str,
        "Comma separated `level` or `target=level` directives."
    ),
    key!(
        "observability.log_format",
        Kind::Enum(LOG_FORMATS),
        "Log line format."
    ),
    key!(
        "observability.otlp_endpoint",
        Kind::Str,
        "OTLP collector endpoint. Unset keeps trace export off."
    ),
    key!(
        "observability.otlp_protocol",
        Kind::Enum(OTLP_PROTOCOLS),
        "OTLP transport."
    ),
    key!(
        "observability.otlp_sample_ratio",
        Kind::Float,
        "Head sampling ratio (0 to 1) for traces without a sampled parent."
    ),
    key!(
        "observability.service_name",
        Kind::Str,
        "`service.name` resource attribute."
    ),
    key!(
        "observability.metrics_enabled",
        Kind::Bool,
        "Serve `/metrics`."
    ),
    key!(
        "observability.metrics_public",
        Kind::Bool,
        "Serve `/metrics` without a token."
    ),
    key!(
        "observability.metrics_token",
        Kind::Secret,
        "Bearer token for `/metrics`. Required unless `metrics_public`; at least 16 characters in production."
    ),
];

/// Lookup structure over [`KEYS`].
#[derive(Debug)]
pub struct KeyIndex {
    leaves: HashMap<&'static str, &'static KeySpec>,
    sections: BTreeSet<String>,
}

impl KeyIndex {
    /// Builds the index from the static table.
    #[must_use]
    pub fn new() -> Self {
        let mut leaves = HashMap::with_capacity(KEYS.len());
        let mut sections = BTreeSet::new();
        for spec in KEYS {
            leaves.insert(spec.path, spec);
            for (i, c) in spec.path.char_indices() {
                if c == '.' {
                    sections.insert(spec.path[..i].to_owned());
                }
            }
        }
        Self { leaves, sections }
    }

    /// The leaf spec at this path, if any.
    #[must_use]
    pub fn leaf(&self, path: &str) -> Option<&'static KeySpec> {
        self.leaves.get(path).copied()
    }

    /// True when `path` names a section (a prefix of some leaf).
    #[must_use]
    pub fn is_section(&self, path: &str) -> bool {
        self.sections.contains(path)
    }

    /// Names of the children (leaf or section) directly under `prefix`.
    #[must_use]
    pub fn children(&self, prefix: &str) -> BTreeSet<&'static str> {
        let want = if prefix.is_empty() {
            String::new()
        } else {
            format!("{prefix}.")
        };
        KEYS.iter()
            .filter_map(|spec| spec.path.strip_prefix(want.as_str()))
            .filter_map(|rest| rest.split('.').next())
            .collect()
    }
}

impl Default for KeyIndex {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn env_name_follows_the_single_rule() {
        assert_eq!(env_name_for("database.url"), "RUSTRYX_DATABASE_URL");
        assert_eq!(
            env_name_for("auth.oidc.client_secret"),
            "RUSTRYX_AUTH_OIDC_CLIENT_SECRET"
        );
        assert_eq!(env_name_for("environment"), "RUSTRYX_ENVIRONMENT");
    }

    #[test]
    fn env_names_are_unique() {
        let mut seen = BTreeSet::new();
        for spec in KEYS {
            assert!(seen.insert(spec.env_name()), "collision for {}", spec.path);
        }
    }

    #[test]
    fn paths_are_unique_and_never_both_leaf_and_section() {
        let index = KeyIndex::new();
        let mut seen = BTreeSet::new();
        for spec in KEYS {
            assert!(seen.insert(spec.path), "duplicate {}", spec.path);
            assert!(
                !index.is_section(spec.path),
                "{} is leaf and section",
                spec.path
            );
        }
    }

    #[test]
    fn only_secrets_have_file_variants() {
        for spec in KEYS {
            assert_eq!(spec.env_file_name().is_some(), spec.is_secret());
        }
        let db = KEYS.iter().find(|k| k.path == "database.url").expect("key");
        assert_eq!(
            db.env_file_name().as_deref(),
            Some("RUSTRYX_DATABASE_URL_FILE")
        );
    }

    #[test]
    fn children_lists_direct_names_only() {
        let index = KeyIndex::new();
        let children = index.children("auth.oidc");
        assert!(children.contains("issuer"));
        assert!(!children.contains("oidc"));
        let root = index.children("");
        assert!(root.contains("auth") && root.contains("environment"));
    }

    #[test]
    fn every_key_has_documentation() {
        for spec in KEYS {
            assert!(spec.doc.len() > 8, "{} lacks a description", spec.path);
            assert!(!spec.doc.contains('\u{2014}') && !spec.doc.contains('\u{2013}'));
        }
    }
}
