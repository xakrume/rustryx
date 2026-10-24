//! `auth` section: sessions, bootstrap admin, OIDC and LDAP.

use std::path::PathBuf;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::secret::ConfigSecret;

/// How users that authenticate through an identity provider get an account.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum JitMode {
    /// Account is created at first login.
    Jit,
    /// Account is created at first login only if pre-registered by an administrator.
    Allowlist,
    /// Accounts are created by administrators only.
    Manual,
}

/// How LDAP group membership is read.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GroupStrategy {
    /// Read the `group_attribute` (usually `memberOf`) of the user entry.
    MemberOf,
    /// Search `group_base_dn` with `group_filter` for groups listing the user.
    MemberSearch,
}

/// UI session limits. Values are upper bounds fixed by the requirements.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct SessionConfig {
    /// Idle timeout in seconds (at most 1800).
    pub idle_timeout_secs: u64,
    /// Absolute lifetime in seconds (at most 28800).
    pub absolute_timeout_secs: u64,
}

impl Default for SessionConfig {
    fn default() -> Self {
        Self {
            idle_timeout_secs: 1800,
            absolute_timeout_secs: 28800,
        }
    }
}

/// Local break-glass administrator created by the bootstrap step.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct BootstrapAdminConfig {
    /// Login name of the administrator.
    pub username: String,
    /// Initial password. Required in production; never has a default.
    pub password: Option<ConfigSecret>,
}

impl Default for BootstrapAdminConfig {
    fn default() -> Self {
        Self {
            username: "admin".to_owned(),
            password: None,
        }
    }
}

/// Keycloak (OIDC) settings.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct OidcConfig {
    /// Turns OIDC login and bearer validation on.
    pub enabled: bool,
    /// Issuer URL, compared byte for byte with the `iss` claim.
    pub issuer: Option<String>,
    /// Issuer URL as seen by browsers, when it differs from `issuer` (split horizon).
    pub public_issuer_url: Option<String>,
    /// Client id of the confidential UI client. Also the required `aud` value.
    pub client_id: Option<String>,
    /// Client secret of the confidential UI client.
    pub client_secret: Option<ConfigSecret>,
    /// Full Keycloak group paths that grant `nx-admin` by exact match.
    pub admin_groups: Vec<String>,
    /// Role given to users without a mapped group. Unset means such users are refused.
    pub default_role: Option<String>,
    /// Top-level claim holding group paths.
    pub groups_claim: String,
    /// Scopes requested at login. `offline_access` is refused.
    pub scopes: Vec<String>,
    /// Clock skew tolerated for `exp`, `nbf` and `iat` (at most 60).
    pub clock_skew_secs: u64,
    /// Values accepted for the `azp` claim of access tokens besides the product client id.
    /// Empty means only `client_id`. A token of another client (the CLI) is accepted only
    /// when that client is listed here: the second client is an explicit allowlist entry,
    /// never implicit.
    pub authorized_parties: Vec<String>,
}

impl OidcConfig {
    /// The `azp` values to accept: `client_id` (when set) first, then the listed parties,
    /// without duplicates. The product client is always included so that a list naming only
    /// the CLI cannot lock the UI out.
    #[must_use]
    pub fn effective_authorized_parties(&self) -> Vec<String> {
        let mut parties: Vec<String> = Vec::with_capacity(self.authorized_parties.len() + 1);
        if let Some(client) = self.client_id.as_deref().filter(|c| !c.trim().is_empty()) {
            parties.push(client.to_owned());
        }
        for party in &self.authorized_parties {
            if !parties.contains(party) {
                parties.push(party.clone());
            }
        }
        parties
    }
}

impl Default for OidcConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            issuer: None,
            public_issuer_url: None,
            client_id: None,
            client_secret: None,
            admin_groups: Vec::new(),
            default_role: None,
            groups_claim: "groups".to_owned(),
            scopes: vec![
                "openid".to_owned(),
                "profile".to_owned(),
                "email".to_owned(),
            ],
            clock_skew_secs: 60,
            authorized_parties: Vec::new(),
        }
    }
}

/// LDAP / Active Directory settings. Only `ldaps://` or `ldap://` with `starttls`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct LdapConfig {
    /// Turns LDAP login on.
    pub enabled: bool,
    /// Server URL: `ldaps://host:636`, or `ldap://host:389` together with `starttls`.
    pub url: Option<String>,
    /// Upgrade an `ldap://` connection with `StartTLS`. There is no plaintext mode.
    pub starttls: bool,
    /// Service account DN used for the user search.
    pub bind_dn: Option<String>,
    /// Service account password.
    pub bind_password: Option<ConfigSecret>,
    /// Base DN of the user search.
    pub user_base_dn: Option<String>,
    /// User search filter. `{username}` is replaced by the RFC 4515 escaped login.
    pub search_filter: String,
    /// How group membership is read.
    pub group_strategy: GroupStrategy,
    /// User entry attribute listing group DNs (`member_of` strategy).
    pub group_attribute: String,
    /// Base DN of the group search (`member_search` strategy).
    pub group_base_dn: Option<String>,
    /// Group search filter. `{dn}` is replaced by the RFC 4515 escaped user DN.
    pub group_filter: String,
    /// Group DNs that grant `nx-admin` by exact match.
    pub admin_groups: Vec<String>,
    /// PEM bundle of the CA that signed the server certificate. When set, the LDAP client
    /// trusts ONLY the certificates in this file (the system roots are not consulted), so
    /// the bundle must hold the whole chain the directory presents. Unset means the
    /// system roots.
    pub ca_file: Option<PathBuf>,
    /// Connect timeout.
    pub connect_timeout_secs: u64,
    /// Bind timeout.
    pub bind_timeout_secs: u64,
    /// Search timeout.
    pub search_timeout_secs: u64,
    /// How long a confirmed group lookup is reused (at most 300; 60 by default because the
    /// authentication chain accepts a group confirmation for five minutes in total, and a
    /// cache that ate that window would leave removed members their access).
    pub groups_cache_ttl_secs: u64,
    /// Maximum depth when expanding nested groups.
    pub nested_group_depth: u32,
    /// Maximum number of groups collected for one user.
    pub max_groups: u32,
}

impl Default for LdapConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            url: None,
            starttls: false,
            bind_dn: None,
            bind_password: None,
            user_base_dn: None,
            search_filter: "(uid={username})".to_owned(),
            group_strategy: GroupStrategy::MemberOf,
            group_attribute: "memberOf".to_owned(),
            group_base_dn: None,
            group_filter: "(member={dn})".to_owned(),
            admin_groups: Vec::new(),
            ca_file: None,
            connect_timeout_secs: 5,
            bind_timeout_secs: 5,
            search_timeout_secs: 5,
            groups_cache_ttl_secs: 60,
            nested_group_depth: 5,
            max_groups: 1000,
        }
    }
}

impl LdapConfig {
    /// Connect timeout as a duration.
    #[must_use]
    pub fn connect_timeout(&self) -> Duration {
        Duration::from_secs(self.connect_timeout_secs)
    }

    /// Bind timeout as a duration.
    #[must_use]
    pub fn bind_timeout(&self) -> Duration {
        Duration::from_secs(self.bind_timeout_secs)
    }

    /// Search timeout as a duration.
    #[must_use]
    pub fn search_timeout(&self) -> Duration {
        Duration::from_secs(self.search_timeout_secs)
    }

    /// Group cache lifetime as a duration.
    #[must_use]
    pub fn groups_cache_ttl(&self) -> Duration {
        Duration::from_secs(self.groups_cache_ttl_secs)
    }
}

/// Authentication settings.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct AuthConfig {
    /// Global switch for anonymous access. A repository must also allow it.
    pub anonymous_enabled: bool,
    /// Let roles assigned by an administrator add to roles derived from groups.
    pub allow_local_role_supplement: bool,
    /// Account creation policy for identity-provider users.
    pub jit_mode: JitMode,
    /// Usernames that may get an account when `jit_mode` is `allowlist`. Exact, case sensitive.
    pub jit_allowlist: Vec<String>,
    /// PEM private key (Ed25519) that signs internal JWTs. Shared by all nodes.
    pub internal_jwt_key: Option<ConfigSecret>,
    /// `kid` written into the tokens signed with `internal_jwt_key`.
    pub internal_jwt_key_id: String,
    /// Retired signing keys that still verify tokens, for a rotation without forced
    /// re-login: `kid: <id>` lines each followed by one PEM block (public keys preferred).
    pub internal_jwt_previous_keys: Option<ConfigSecret>,
    /// Base64 of 32 random bytes sealing the OIDC state cookie (AES-256-GCM).
    /// Shared by all nodes.
    pub cookie_key: Option<ConfigSecret>,
    /// UI session limits.
    pub session: SessionConfig,
    /// Local break-glass administrator.
    pub bootstrap_admin: BootstrapAdminConfig,
    /// Keycloak.
    pub oidc: OidcConfig,
    /// LDAP.
    pub ldap: LdapConfig,
}

impl Default for AuthConfig {
    fn default() -> Self {
        Self {
            anonymous_enabled: false,
            allow_local_role_supplement: false,
            jit_mode: JitMode::Jit,
            jit_allowlist: Vec::new(),
            internal_jwt_key: None,
            internal_jwt_key_id: "k1".to_owned(),
            internal_jwt_previous_keys: None,
            cookie_key: None,
            session: SessionConfig::default(),
            bootstrap_admin: BootstrapAdminConfig::default(),
            oidc: OidcConfig::default(),
            ldap: LdapConfig::default(),
        }
    }
}
