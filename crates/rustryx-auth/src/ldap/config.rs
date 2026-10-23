//! LDAP configuration and its validation.
//!
//! [`LdapConfig`] is the serde shape. It is flat and uses the same field names as the
//! `auth.ldap` section of `rustryx-config` (this crate must not depend on that one), so
//! the composition root maps it field by field. [`LdapConfig::validate`] is what stops the
//! process from starting with a bad configuration, and `settings()` turns a valid one into
//! `LdapSettings`, where URLs, filter templates and durations are already parsed so
//! nothing downstream can meet an invalid value.
//!
//! There is no plaintext switch: `deny_unknown_fields` makes a key such as
//! `allow_plaintext` a startup error instead of a silently ignored line.

use std::path::PathBuf;
use std::time::Duration;

use rustryx_core::Secret;
use serde::Deserialize;
use url::Url;

use super::filter::{FilterTemplate, FilterTemplateError, Placeholder};

/// How group membership is discovered.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LdapGroupStrategy {
    /// Read the membership attribute of the user entry (`memberOf`).
    MemberOf,
    /// Search the group tree for entries that list the user (`member`, `memberUid`).
    /// `member_search` is the name `rustryx-config` uses for the same strategy.
    #[serde(alias = "member_search")]
    ReverseMember,
}

/// LDAP and Active Directory configuration (`auth.ldap`).
///
/// Debug output redacts `bind_password` (it is a [`Secret`]). This type implements
/// `Deserialize` only: nothing here is meant to be written back to disk.
// A configuration section: every flag is an independent switch in the file format
// (`enabled`, `starttls`, ...), and the YAML shape is shared with `rustryx-config`, so
// folding them into enums would change the file, not just the code.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct LdapConfig {
    /// Master switch. A disabled section is not connected to and not required to be
    /// complete, but a plaintext URL in it is still rejected.
    pub enabled: bool,
    /// `ldaps://host[:636]`, or `ldap://host[:389]` together with `starttls: true`.
    pub url: Option<String>,
    /// Upgrade an `ldap://` connection with `StartTLS`. Must stay false for `ldaps://`.
    #[serde(alias = "start_tls")]
    pub starttls: bool,
    /// PEM file with the CA certificate(s) to trust. When set, ONLY these CAs are
    /// trusted; when absent the platform trust store is used.
    pub ca_file: Option<PathBuf>,
    /// DN of the service account that searches for users and groups.
    pub bind_dn: Option<String>,
    /// Password of the service account. Read it from an environment variable or a
    /// mounted file in the configuration layer, never from the repository.
    pub bind_password: Option<Secret>,
    /// Search base for users.
    pub user_base_dn: Option<String>,
    /// User search filter. `{username}` is the escaped login.
    pub search_filter: String,
    /// Attribute holding the canonical user name (`uid`, `sAMAccountName`).
    pub username_attribute: String,
    /// Longest accepted login name in bytes.
    pub max_login_length: usize,
    /// Group discovery strategy.
    pub group_strategy: LdapGroupStrategy,
    /// Attribute of the user (and, for nesting, of a group) listing its groups.
    pub group_attribute: String,
    /// Base of the group search. Required for `reverse_member`.
    pub group_base_dn: Option<String>,
    /// Group search filter for `reverse_member`. `{dn}` is the user DN, `{username}`
    /// the canonical user name: `(member={dn})` for `member`, `(memberUid={username})`
    /// for POSIX groups.
    pub group_filter: String,
    /// Filter finding the parents of a group when `reverse_member` walks nested
    /// groups. Only `{dn}` is available.
    pub nested_group_filter: String,
    /// Whether to follow groups that are members of groups.
    pub nested_groups: bool,
    /// Depth of the walk: 1 means only the groups the user belongs to directly.
    pub nested_group_depth: u32,
    /// Upper bound on the number of groups returned for one user.
    pub max_groups: u32,
    /// Seconds a successful lookup is served without asking the directory again.
    /// At most 300: confirmed membership never outlives five minutes. With 300 the
    /// last confirmed set is never served after the TTL, so an outage fails closed at
    /// once; a smaller value leaves a window in which it is.
    pub groups_cache_ttl_secs: u64,
    /// When the directory cannot be reached, serve the last confirmed set while it is at
    /// most five minutes old, measured from the last successful search. Default `false`:
    /// a failed lookup is an error at once. Turn it on only when the caller does NOT apply
    /// a grace period of its own (the authentication chain does), because two layers each
    /// granting five minutes add up to ten.
    pub groups_stale_on_error: bool,
    /// Group DNs that grant `nx-admin`. Accepted so that one `auth.ldap` section can be
    /// deserialized into this struct, but NOT used here: the role mapper compares the
    /// returned DNs with it by exact equality.
    pub admin_groups: Vec<String>,
    /// TCP connect plus TLS handshake plus `StartTLS`, seconds.
    pub connect_timeout_secs: u64,
    /// One bind, seconds.
    pub bind_timeout_secs: u64,
    /// One search, seconds.
    pub search_timeout_secs: u64,
    /// The whole authentication (pool checkout, user search, user bind), seconds.
    pub auth_deadline_secs: u64,
    /// The whole group lookup, nested searches included, seconds.
    pub group_deadline_secs: u64,
    /// Maximum number of simultaneously open service account connections.
    pub pool_size: usize,
    /// Seconds after which a service connection is retired even if healthy.
    pub pool_max_lifetime_secs: u64,
    /// Maximum number of user bind connections open at once. Each login opens its own
    /// connection, so without a cap a login storm would become a connection storm.
    pub max_concurrent_user_binds: usize,
}

impl Default for LdapConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            url: None,
            starttls: false,
            ca_file: None,
            bind_dn: None,
            bind_password: None,
            user_base_dn: None,
            search_filter: "(uid={username})".to_owned(),
            username_attribute: "uid".to_owned(),
            max_login_length: 256,
            group_strategy: LdapGroupStrategy::MemberOf,
            group_attribute: "memberOf".to_owned(),
            group_base_dn: None,
            group_filter: "(member={dn})".to_owned(),
            nested_group_filter: "(member={dn})".to_owned(),
            nested_groups: true,
            nested_group_depth: 5,
            max_groups: 1000,
            groups_cache_ttl_secs: 60,
            groups_stale_on_error: false,
            admin_groups: Vec::new(),
            connect_timeout_secs: 5,
            bind_timeout_secs: 5,
            search_timeout_secs: 5,
            auth_deadline_secs: 10,
            group_deadline_secs: 10,
            pool_size: 4,
            pool_max_lifetime_secs: 300,
            max_concurrent_user_binds: 16,
        }
    }
}

/// Why a configuration was rejected. Messages never contain secrets: a URL that
/// carries credentials is reported without being echoed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum LdapConfigError {
    /// The URL does not parse.
    #[error("ldap.url is not a valid URL")]
    InvalidUrl,
    /// The scheme is neither `ldap` nor `ldaps`.
    #[error("ldap.url scheme `{0}` is not supported: use ldaps:// or ldap:// with starttls")]
    UnsupportedScheme(String),
    /// `ldap://` without `StartTLS`.
    #[error("plaintext LDAP is not allowed: use ldaps://, or ldap:// together with starttls: true")]
    PlaintextNotAllowed,
    /// `ldaps://` with `starttls: true`.
    #[error("ldap.starttls must be false for an ldaps:// url (the connection is already TLS)")]
    StartTlsWithLdaps,
    /// The URL carries credentials, a path, a query or a fragment.
    #[error("ldap.url must be scheme://host[:port] only (no credentials, path, query or fragment)")]
    UnexpectedUrlParts,
    /// The URL has no host, so there is no name to verify the certificate against.
    #[error("ldap.url has no host")]
    MissingHost,
    /// A required field is empty.
    #[error("ldap.{0} must not be empty")]
    Empty(&'static str),
    /// A field has an unusable value.
    #[error("ldap.{field}: {reason}")]
    Invalid {
        /// Field name.
        field: &'static str,
        /// What is wrong with it.
        reason: &'static str,
    },
    /// A filter template is unusable.
    #[error("ldap.{field}: {source}")]
    Filter {
        /// Field name.
        field: &'static str,
        /// Template problem.
        #[source]
        source: FilterTemplateError,
    },
}

/// Parsed transport target.
#[derive(Debug, Clone)]
pub(crate) struct Endpoint {
    pub(crate) url: Url,
    pub(crate) start_tls: bool,
}

/// Timeouts as durations.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Timeouts {
    pub(crate) connect: Duration,
    pub(crate) bind: Duration,
    pub(crate) search: Duration,
    pub(crate) auth_deadline: Duration,
    pub(crate) group_deadline: Duration,
}

/// Group settings with parsed templates.
#[derive(Debug, Clone)]
pub(crate) struct GroupSettings {
    pub(crate) strategy: LdapGroupStrategy,
    pub(crate) member_of_attribute: String,
    pub(crate) group_base_dn: String,
    pub(crate) group_filter: FilterTemplate,
    pub(crate) nested_group_filter: FilterTemplate,
    pub(crate) nested: bool,
    pub(crate) max_depth: u32,
    pub(crate) max_groups: usize,
    pub(crate) cache_ttl: Duration,
    pub(crate) stale_on_error: bool,
}

/// A fully validated configuration.
#[derive(Debug, Clone)]
pub(crate) struct LdapSettings {
    pub(crate) endpoint: Endpoint,
    pub(crate) ca_file: Option<PathBuf>,
    pub(crate) bind_dn: String,
    pub(crate) bind_password: Secret,
    pub(crate) user_base_dn: String,
    pub(crate) user_filter: FilterTemplate,
    pub(crate) username_attribute: String,
    pub(crate) max_login_length: usize,
    pub(crate) groups: GroupSettings,
    pub(crate) timeouts: Timeouts,
    pub(crate) pool_size: usize,
    pub(crate) pool_max_lifetime: Duration,
    pub(crate) max_user_binds: usize,
}

/// Confirmed group membership is never used for longer than this.
pub(crate) const MAX_GROUP_STALENESS: Duration = Duration::from_mins(5);

const MAX_TIMEOUT_SECS: u64 = 300;
const MAX_DEADLINE_SECS: u64 = 600;

impl LdapConfig {
    /// Checks the configuration. Call it at startup: an `Err` must stop the process.
    ///
    /// A disabled section only has its URL checked (if there is one), so a plaintext
    /// URL cannot sit in the file waiting for someone to flip `enabled`.
    pub fn validate(&self) -> Result<(), LdapConfigError> {
        if self.enabled {
            return self.settings().map(|_| ());
        }
        match self.url.as_deref() {
            Some(url) if !url.trim().is_empty() => parse_endpoint(url, self.starttls).map(|_| ()),
            _ => Ok(()),
        }
    }

    /// Full validation, whatever `enabled` says.
    pub(crate) fn settings(&self) -> Result<LdapSettings, LdapConfigError> {
        let endpoint = parse_endpoint(self.url.as_deref().unwrap_or_default(), self.starttls)?;
        let bind_password = self
            .bind_password
            .clone()
            .ok_or(LdapConfigError::Empty("bind_password"))?;
        // An empty service password would turn the service bind into an unauthenticated
        // bind that many servers accept, after which every search runs as anonymous.
        if bind_password.expose().trim().is_empty() {
            return Err(LdapConfigError::Empty("bind_password"));
        }
        let bind_dn = require_dn("bind_dn", self.bind_dn.as_deref())?;
        let user_base_dn = require_dn("user_base_dn", self.user_base_dn.as_deref())?;
        require_attribute("username_attribute", &self.username_attribute)?;
        let user_filter = parse_template(
            "search_filter",
            &self.search_filter,
            &[Placeholder::Username],
            &[Placeholder::Username],
        )?;
        let groups = self.group_settings()?;
        let timeouts = self.timeouts()?;
        check_range("max_login_length", self.max_login_length as u64, 1, 1024)?;
        check_range("pool_size", self.pool_size as u64, 1, 64)?;
        check_range(
            "pool_max_lifetime_secs",
            self.pool_max_lifetime_secs,
            1,
            86_400,
        )?;
        check_range(
            "max_concurrent_user_binds",
            self.max_concurrent_user_binds as u64,
            1,
            256,
        )?;
        if self
            .ca_file
            .as_ref()
            .is_some_and(|path| path.as_os_str().is_empty())
        {
            return Err(LdapConfigError::Empty("ca_file"));
        }
        Ok(LdapSettings {
            endpoint,
            ca_file: self.ca_file.clone(),
            bind_dn,
            bind_password,
            user_base_dn,
            user_filter,
            username_attribute: self.username_attribute.clone(),
            max_login_length: self.max_login_length,
            groups,
            timeouts,
            pool_size: self.pool_size,
            pool_max_lifetime: Duration::from_secs(self.pool_max_lifetime_secs),
            max_user_binds: self.max_concurrent_user_binds,
        })
    }

    fn group_settings(&self) -> Result<GroupSettings, LdapConfigError> {
        require_attribute("group_attribute", &self.group_attribute)?;
        let group_base_dn = if self.group_strategy == LdapGroupStrategy::ReverseMember {
            require_dn("group_base_dn", self.group_base_dn.as_deref())?
        } else {
            self.group_base_dn.clone().unwrap_or_default()
        };
        check_range(
            "nested_group_depth",
            u64::from(self.nested_group_depth),
            1,
            16,
        )?;
        check_range("max_groups", u64::from(self.max_groups), 1, 10_000)?;
        check_range(
            "groups_cache_ttl_secs",
            self.groups_cache_ttl_secs,
            0,
            MAX_GROUP_STALENESS.as_secs(),
        )?;
        Ok(GroupSettings {
            strategy: self.group_strategy,
            member_of_attribute: self.group_attribute.clone(),
            group_base_dn,
            // `(member={dn})` uses the DN, `(memberUid={username})` the login name.
            group_filter: parse_template(
                "group_filter",
                &self.group_filter,
                &[Placeholder::Username, Placeholder::Dn],
                &[Placeholder::Username, Placeholder::Dn],
            )?,
            nested_group_filter: parse_template(
                "nested_group_filter",
                &self.nested_group_filter,
                &[Placeholder::Dn],
                &[Placeholder::Dn],
            )?,
            nested: self.nested_groups,
            max_depth: self.nested_group_depth,
            max_groups: usize::try_from(self.max_groups).unwrap_or(usize::MAX),
            cache_ttl: Duration::from_secs(self.groups_cache_ttl_secs),
            stale_on_error: self.groups_stale_on_error,
        })
    }

    fn timeouts(&self) -> Result<Timeouts, LdapConfigError> {
        check_range(
            "connect_timeout_secs",
            self.connect_timeout_secs,
            1,
            MAX_TIMEOUT_SECS,
        )?;
        check_range(
            "bind_timeout_secs",
            self.bind_timeout_secs,
            1,
            MAX_TIMEOUT_SECS,
        )?;
        check_range(
            "search_timeout_secs",
            self.search_timeout_secs,
            1,
            MAX_TIMEOUT_SECS,
        )?;
        check_range(
            "auth_deadline_secs",
            self.auth_deadline_secs,
            1,
            MAX_DEADLINE_SECS,
        )?;
        check_range(
            "group_deadline_secs",
            self.group_deadline_secs,
            1,
            MAX_DEADLINE_SECS,
        )?;
        Ok(Timeouts {
            connect: Duration::from_secs(self.connect_timeout_secs),
            bind: Duration::from_secs(self.bind_timeout_secs),
            search: Duration::from_secs(self.search_timeout_secs),
            auth_deadline: Duration::from_secs(self.auth_deadline_secs),
            group_deadline: Duration::from_secs(self.group_deadline_secs),
        })
    }
}

/// The transport rule of README 8.4: LDAPS, or `StartTLS` on `ldap://`, nothing else.
pub(crate) fn parse_endpoint(raw: &str, start_tls: bool) -> Result<Endpoint, LdapConfigError> {
    if raw.trim().is_empty() {
        return Err(LdapConfigError::Empty("url"));
    }
    let url = Url::parse(raw.trim()).map_err(|_| LdapConfigError::InvalidUrl)?;
    match url.scheme() {
        "ldaps" if start_tls => return Err(LdapConfigError::StartTlsWithLdaps),
        "ldaps" => {}
        "ldap" if start_tls => {}
        "ldap" => return Err(LdapConfigError::PlaintextNotAllowed),
        other => return Err(LdapConfigError::UnsupportedScheme(other.to_owned())),
    }
    if !url.username().is_empty()
        || url.password().is_some()
        || !matches!(url.path(), "" | "/")
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(LdapConfigError::UnexpectedUrlParts);
    }
    if url.host_str().is_none_or(str::is_empty) {
        return Err(LdapConfigError::MissingHost);
    }
    Ok(Endpoint { url, start_tls })
}

fn require_dn(field: &'static str, value: Option<&str>) -> Result<String, LdapConfigError> {
    let value = value.unwrap_or_default();
    if value.trim().is_empty() {
        return Err(LdapConfigError::Empty(field));
    }
    if value.contains('\0') {
        return Err(LdapConfigError::Invalid {
            field,
            reason: "contains a NUL byte",
        });
    }
    Ok(value.to_owned())
}

/// RFC 4512 attribute description: a name (letters, digits, hyphen) or a numeric OID.
fn require_attribute(field: &'static str, value: &str) -> Result<(), LdapConfigError> {
    if value.is_empty() {
        return Err(LdapConfigError::Empty(field));
    }
    let valid = value
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || ch == '-' || ch == '.')
        && !value.starts_with(['-', '.']);
    if valid {
        Ok(())
    } else {
        Err(LdapConfigError::Invalid {
            field,
            reason: "is not a valid attribute name",
        })
    }
}

fn parse_template(
    field: &'static str,
    text: &str,
    allowed: &[Placeholder],
    required: &[Placeholder],
) -> Result<FilterTemplate, LdapConfigError> {
    FilterTemplate::parse(text, allowed, required)
        .map_err(|source| LdapConfigError::Filter { field, source })
}

fn check_range(field: &'static str, value: u64, min: u64, max: u64) -> Result<(), LdapConfigError> {
    if (min..=max).contains(&value) {
        Ok(())
    } else {
        Err(LdapConfigError::Invalid {
            field,
            reason: "is out of the allowed range",
        })
    }
}
