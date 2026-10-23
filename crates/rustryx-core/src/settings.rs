//! Platform settings: typed, versioned, audited sections that an operator edits at run time.
//!
//! Operator tunable policy lives in the database as one JSON document per section under the key
//! `settings.<section>` of `system_config`. A section is parsed into one of the typed structs
//! below with `deny_unknown_fields`, validated by code, edited through one generic endpoint and
//! applied by every node within seconds. The security boundary and bootstrap values (SSRF
//! allowlists, the proxy host list) stay in file configuration, because an API administrator
//! must not be able to widen them.
//!
//! Every section has a `Default` that a node uses when its row is missing, so a node without a
//! row behaves like every other node and a missing row is never an error. Every section also
//! reads a document that lacks some fields (`#[serde(default)]` on the container): a field added
//! later takes its default when an older row is read.

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::error::DomainError;
use crate::identity::AppArea;
use crate::ids::UserId;
use crate::notification::{MAX_RECIPIENTS, validate_address};
use crate::quota::validate_warn_percent;
use crate::secret::Secret;

/// Longest hostname, in characters.
pub const MAX_HOST_CHARS: usize = 253;
/// Longest SMTP from name, in characters.
pub const MAX_FROM_NAME_CHARS: usize = 100;
/// Most `no_proxy` entries.
pub const MAX_NO_PROXY_ENTRIES: usize = 100;
/// Most log overrides at once.
pub const MAX_LOG_OVERRIDES: usize = 20;
/// Longest time a log override may last ahead, in hours.
pub const MAX_LOG_OVERRIDE_HOURS: i64 = 24;
/// Most certificates in the managed trust store.
pub const MAX_TRUSTED_CERTIFICATES: usize = 200;

mod certificate;
mod projects;

pub use certificate::{MAX_CERTIFICATE_PEM_BYTES, TrustedCertificate};
pub use projects::{GroupMappingPolicy, ProjectCreation, ProjectsSettings, RobotSettings};

/// A section of the settings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum SettingsSection {
    /// Outbound mail.
    Smtp,
    /// The outbound HTTP proxy.
    OutboundProxy,
    /// The managed trust store switches.
    Trust,
    /// Time boxed log level overrides.
    Logging,
    /// Project creation, group mapping policy and robot token rules.
    Projects,
    /// Quota defaults.
    Quota,
    /// Storage behaviour.
    Storage,
    /// Lifecycle engine limits.
    Lifecycle,
    /// The system wide webhook switch.
    Webhooks,
    /// Notification defaults.
    Notifications,
}

impl SettingsSection {
    /// Every section, in declaration order.
    pub const ALL: &'static [Self] = &[
        Self::Smtp,
        Self::OutboundProxy,
        Self::Trust,
        Self::Logging,
        Self::Projects,
        Self::Quota,
        Self::Storage,
        Self::Lifecycle,
        Self::Webhooks,
        Self::Notifications,
    ];

    /// The `system_config` key, for example `settings.smtp`.
    #[must_use]
    pub const fn key(self) -> &'static str {
        match self {
            Self::Smtp => "settings.smtp",
            Self::OutboundProxy => "settings.outbound_proxy",
            Self::Trust => "settings.trust",
            Self::Logging => "settings.logging",
            Self::Projects => "settings.projects",
            Self::Quota => "settings.quota",
            Self::Storage => "settings.storage",
            Self::Lifecycle => "settings.lifecycle",
            Self::Webhooks => "settings.webhooks",
            Self::Notifications => "settings.notifications",
        }
    }

    /// JSON pointers of the values sealed at rest (purpose `settings.<section>.<field>`).
    #[must_use]
    pub const fn secret_pointers(self) -> &'static [&'static str] {
        match self {
            Self::Smtp | Self::OutboundProxy => &["/password"],
            Self::Trust
            | Self::Logging
            | Self::Projects
            | Self::Quota
            | Self::Storage
            | Self::Lifecycle
            | Self::Webhooks
            | Self::Notifications => &[],
        }
    }

    /// The application area that edits it (`nx-admin` until `ApplicationPrivileges` is open).
    #[must_use]
    pub const fn area(self) -> AppArea {
        match self {
            Self::Smtp
            | Self::OutboundProxy
            | Self::Trust
            | Self::Logging
            | Self::Projects
            | Self::Notifications => AppArea::Settings,
            Self::Quota => AppArea::Quotas,
            Self::Storage => AppArea::Blobstores,
            Self::Lifecycle => AppArea::Tasks,
            Self::Webhooks => AppArea::Webhooks,
        }
    }
}

/// How the SMTP connection is secured. There is no plaintext variant, so there is no switch to
/// forget.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SmtpTls {
    /// TLS from the first byte (port 465).
    Implicit,
    /// Upgrade with STARTTLS (port 587), refused when the server does not offer it.
    Starttls,
}

fn has_forbidden_header_char(text: &str) -> bool {
    text.chars().any(|c| matches!(c, '\r' | '\n' | '\0'))
}

fn validate_host(host: &str) -> Result<(), DomainError> {
    let bad = host.is_empty()
        || host.chars().count() > MAX_HOST_CHARS
        || host
            .chars()
            .any(|c| c.is_whitespace() || c.is_control() || matches!(c, '/' | '@' | '?' | '#'));
    if bad {
        return Err(DomainError::invalid(
            "host must be a name or address of at most 253 characters",
        ));
    }
    Ok(())
}

/// Outbound mail. The server name goes through the `Smtp` egress policy; the certificate is
/// verified against the system roots and the managed trust store, and verification cannot be
/// turned off.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct SmtpSettings {
    /// Mail is sent only while this is true.
    pub enabled: bool,
    /// Relay host name or address.
    pub host: String,
    /// Relay port (the `Smtp` egress policy allows 465 and 587 by default).
    pub port: u16,
    /// Implicit TLS or STARTTLS.
    pub tls: SmtpTls,
    /// Login user, if the relay needs one.
    pub username: Option<String>,
    /// Login password. Sealed at rest; the API shows `password_set`. Redacted in `Debug`.
    pub password: Option<Secret>,
    /// RFC 5322 mailbox address, validated with an address crate by the service; CR, LF and
    /// NUL are refused everywhere.
    pub from_address: String,
    /// Display name of the sender, at most 100 characters.
    pub from_name: String,
    /// Timeout of one SMTP exchange, 1 to 60 seconds, default 10.
    pub timeout_secs: u8,
}

impl Default for SmtpSettings {
    /// Disabled, `smtp` on port 587 with STARTTLS, 10 seconds, nothing else set.
    fn default() -> Self {
        Self {
            enabled: false,
            host: String::new(),
            port: 587,
            tls: SmtpTls::Starttls,
            username: None,
            password: None,
            from_address: String::new(),
            from_name: String::new(),
            timeout_secs: 10,
        }
    }
}

impl SmtpSettings {
    /// Checks the section. Header characters (CR, LF, NUL) are refused in every text field
    /// whether or not the section is enabled; an enabled section also needs a host, a port, a
    /// from address and a timeout of 1 to 60 seconds.
    ///
    /// # Errors
    /// Returns [`DomainError::Invalid`] naming the broken rule.
    pub fn validate(&self) -> Result<(), DomainError> {
        let texts = [
            Some(self.host.as_str()),
            Some(self.from_address.as_str()),
            Some(self.from_name.as_str()),
            self.username.as_deref(),
        ];
        if texts.into_iter().flatten().any(has_forbidden_header_char) {
            return Err(DomainError::invalid(
                "smtp settings must not contain line breaks or NUL",
            ));
        }
        if self.from_name.chars().count() > MAX_FROM_NAME_CHARS {
            return Err(DomainError::invalid(
                "from name must be at most 100 characters",
            ));
        }
        if !(1..=60).contains(&self.timeout_secs) {
            return Err(DomainError::invalid("smtp timeout must be 1 to 60 seconds"));
        }
        if self.enabled {
            validate_host(&self.host)?;
            if self.port == 0 {
                return Err(DomainError::invalid("smtp port must not be 0"));
            }
            validate_address(&self.from_address)?;
        }
        Ok(())
    }
}

/// Which outbound calls use the proxy.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields, default)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "one switch per outbound purpose is the stored shape of the setting"
)]
pub struct ProxyApplication {
    /// Proxy repository upstreams.
    pub upstreams: bool,
    /// Registry endpoints and replication.
    pub registries: bool,
    /// Webhook deliveries.
    pub webhooks: bool,
    /// HTTP scanners and data set downloads.
    pub scanners: bool,
}

/// The only source of an outbound proxy: environment variables stay ignored. Blob store
/// endpoints never use it.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields, default)]
pub struct OutboundProxySettings {
    /// The proxy is used only while this is true.
    pub enabled: bool,
    /// `http://host:port` or `https://host:port`, no user info; the host must be listed in
    /// `egress.proxy.allowed_hosts`.
    pub http_proxy: Option<String>,
    /// Proxy for `https` destinations, same form.
    pub https_proxy: Option<String>,
    /// Exact hosts, `*.suffix` hosts and CIDRs that bypass the proxy; at most 100 entries of
    /// at most 253 characters.
    pub no_proxy: Vec<String>,
    /// Proxy login user.
    pub username: Option<String>,
    /// Proxy login password. Sealed at rest. Redacted in `Debug`.
    pub password: Option<Secret>,
    /// Which calls go through the proxy.
    pub apply_to: ProxyApplication,
}

fn validate_proxy_url(url: &str) -> Result<(), DomainError> {
    let authority = url
        .strip_prefix("http://")
        .or_else(|| url.strip_prefix("https://"))
        .ok_or_else(|| DomainError::invalid("a proxy URL must start with http:// or https://"))?;
    if authority.contains('@') {
        return Err(DomainError::invalid(
            "a proxy URL must not contain user info",
        ));
    }
    if authority.is_empty()
        || authority
            .chars()
            .any(|c| matches!(c, '/' | '?' | '#') || c.is_whitespace() || c.is_control())
    {
        return Err(DomainError::invalid(
            "a proxy URL is a scheme, a host and an optional port",
        ));
    }
    Ok(())
}

impl OutboundProxySettings {
    /// Checks the section: proxy URLs without user info, path, query or fragment, at most 100
    /// `no_proxy` entries of at most 253 characters without whitespace, and for an enabled
    /// section at least one proxy URL. Whether the proxy host is allowed is decided against
    /// `egress.proxy.allowed_hosts` (file configuration), which core does not know.
    ///
    /// # Errors
    /// Returns [`DomainError::Invalid`] naming the broken rule.
    pub fn validate(&self) -> Result<(), DomainError> {
        for url in [&self.http_proxy, &self.https_proxy].into_iter().flatten() {
            validate_proxy_url(url)?;
        }
        if self.enabled && self.http_proxy.is_none() && self.https_proxy.is_none() {
            return Err(DomainError::invalid("an enabled proxy needs a proxy URL"));
        }
        if self.no_proxy.len() > MAX_NO_PROXY_ENTRIES {
            return Err(DomainError::invalid("no_proxy has at most 100 entries"));
        }
        if self.no_proxy.iter().any(|entry| {
            entry.is_empty()
                || entry.chars().count() > MAX_HOST_CHARS
                || entry.chars().any(|c| c.is_whitespace() || c.is_control())
        }) {
            return Err(DomainError::invalid(
                "a no_proxy entry has 1 to 253 characters and no whitespace",
            ));
        }
        if self
            .username
            .as_deref()
            .is_some_and(has_forbidden_header_char)
        {
            return Err(DomainError::invalid(
                "proxy user must not contain line breaks or NUL",
            ));
        }
        Ok(())
    }
}

/// Switches of the managed outbound trust store.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct TrustSettings {
    /// Allow `POST /trusted-certificates/actions/fetch` (read a server's chain for review).
    /// Default false.
    pub allow_fetch: bool,
    /// Keep the platform and webpki roots in the client roots (default true); false makes the
    /// managed store the only trust.
    pub use_system_roots: bool,
}

impl Default for TrustSettings {
    fn default() -> Self {
        Self {
            allow_fetch: false,
            use_system_roots: true,
        }
    }
}

/// Verbosity of a log override.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LogLevel {
    /// Errors only.
    Error,
    /// Warnings and errors.
    Warn,
    /// Normal operation.
    Info,
    /// Debugging detail.
    Debug,
    /// Everything.
    Trace,
}

/// A time boxed log level for one target.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogOverride {
    /// The `tracing` target. Allowed targets are a fixed list in `rustryx-observability`.
    pub target: String,
    /// The level.
    pub level: LogLevel,
    /// When the override ends and the base level returns.
    #[serde(with = "crate::rfc3339")]
    pub expires_at: OffsetDateTime,
}

/// Time boxed, cluster wide log overrides. At most 20 entries, each at most 24 hours ahead
/// (default 1 hour in the API).
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields, default)]
pub struct LoggingSettings {
    /// The overrides.
    pub overrides: Vec<LogOverride>,
}

impl LoggingSettings {
    /// Checks the count, that no target is empty, and that no expiry lies more than 24 hours
    /// after `now`. An expiry in the past is accepted (the entry is simply expired).
    ///
    /// # Errors
    /// Returns [`DomainError::Invalid`] naming the broken rule.
    pub fn validate(&self, now: OffsetDateTime) -> Result<(), DomainError> {
        if self.overrides.len() > MAX_LOG_OVERRIDES {
            return Err(DomainError::invalid("at most 20 log overrides"));
        }
        let limit = now + time::Duration::hours(MAX_LOG_OVERRIDE_HOURS);
        if self.overrides.iter().any(|o| o.target.is_empty()) {
            return Err(DomainError::invalid("a log override needs a target"));
        }
        if self.overrides.iter().any(|o| o.expires_at > limit) {
            return Err(DomainError::invalid(
                "a log override lasts at most 24 hours",
            ));
        }
        Ok(())
    }

    /// The overrides that have not expired at `now`. An override whose expiry equals `now` is
    /// over.
    pub fn active_at(&self, now: OffsetDateTime) -> impl Iterator<Item = &LogOverride> {
        self.overrides.iter().filter(move |o| o.expires_at > now)
    }
}

/// Quota defaults.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct QuotaSettings {
    /// Limit copied into the quota of a new project at creation, not applied retroactively.
    /// `None` is unlimited.
    pub default_project_bytes: Option<u64>,
    /// Default warn edge, 1 to 100.
    pub default_warn_percent: u8,
}

impl Default for QuotaSettings {
    fn default() -> Self {
        Self {
            default_project_bytes: None,
            default_warn_percent: crate::quota::DEFAULT_WARN_PERCENT,
        }
    }
}

impl QuotaSettings {
    /// Checks the warn edge.
    ///
    /// # Errors
    /// Returns [`DomainError::Invalid`] when `default_warn_percent` is outside 1 to 100.
    pub fn validate(&self) -> Result<(), DomainError> {
        validate_warn_percent(self.default_warn_percent)
    }
}

/// Storage behaviour.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct StorageSettings {
    /// Minimum spacing of `last_downloaded_at` writes per asset, in seconds.
    pub last_downloaded_interval_secs: u32,
}

impl Default for StorageSettings {
    fn default() -> Self {
        Self {
            last_downloaded_interval_secs: 3600,
        }
    }
}

/// Lifecycle engine limits.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct LifecycleSettings {
    /// Days a finished dry run keeps its real run unlocked.
    pub dry_run_validity_days: u32,
    /// A run that would delete more than this share of the evaluated components stops
    /// unless an admin confirmed it.
    pub max_delete_ratio: f32,
    /// The brake needs at least this many evaluated components.
    pub mass_delete_min_candidates: u32,
    /// Components deleted per transaction, 50 to 2000.
    pub batch_size: u32,
}

impl Default for LifecycleSettings {
    /// 14 days, ratio 0.9, 20 candidates, batches of 500.
    fn default() -> Self {
        Self {
            dry_run_validity_days: 14,
            max_delete_ratio: 0.9,
            mass_delete_min_candidates: 20,
            batch_size: 500,
        }
    }
}

impl LifecycleSettings {
    /// Checks `0 < max_delete_ratio <= 1` (NaN is refused), a validity of at least one day and
    /// a batch size of 50 to 2000.
    ///
    /// # Errors
    /// Returns [`DomainError::Invalid`] naming the broken rule.
    pub fn validate(&self) -> Result<(), DomainError> {
        if !(self.max_delete_ratio > 0.0 && self.max_delete_ratio <= 1.0) {
            return Err(DomainError::invalid(
                "max_delete_ratio must be greater than 0 and at most 1",
            ));
        }
        if self.dry_run_validity_days == 0 {
            return Err(DomainError::invalid(
                "dry_run_validity_days must be at least 1",
            ));
        }
        if !(50..=2000).contains(&self.batch_size) {
            return Err(DomainError::invalid("batch_size must be 50 to 2000"));
        }
        Ok(())
    }
}

/// The system wide webhook switch.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct WebhooksSettings {
    /// When false no webhook is delivered.
    pub enabled: bool,
}

impl Default for WebhooksSettings {
    fn default() -> Self {
        Self { enabled: true }
    }
}

/// Notification defaults.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields, default)]
pub struct NotificationSettings {
    /// Addresses that receive the e-mail for `task.finished`; at most 20.
    pub task_recipients: Vec<String>,
}

impl NotificationSettings {
    /// Checks at most 20 valid addresses.
    ///
    /// # Errors
    /// Returns [`DomainError::Invalid`] naming the broken rule.
    pub fn validate(&self) -> Result<(), DomainError> {
        if self.task_recipients.len() > MAX_RECIPIENTS {
            return Err(DomainError::invalid("at most 20 task recipients"));
        }
        self.task_recipients
            .iter()
            .try_for_each(|a| validate_address(a))
    }
}

/// The value of `system.read_only`. Read only mode has its own endpoint and a quiesce protocol,
/// but is stored and propagated by the same framework.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields, default)]
pub struct ReadOnlyState {
    /// Writes are refused while this is true.
    pub enabled: bool,
    /// Why, shown to users.
    pub reason: String,
    /// Who switched it on.
    pub set_by: Option<UserId>,
    /// When.
    #[serde(with = "crate::rfc3339::option")]
    pub set_at: Option<OffsetDateTime>,
}

#[cfg(test)]
mod tests;
