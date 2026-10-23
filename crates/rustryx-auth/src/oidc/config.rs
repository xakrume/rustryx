//! OIDC configuration and its validation.
//!
//! Validation rejects out-of-range values instead of clamping them: a node that
//! silently runs with a different skew or refresh window than the operator wrote
//! is harder to reason about than one that refuses to start (fail fast, all
//! problems listed at once).

use std::fmt;
use std::time::Duration;

use rustryx_core::Secret;
use url::Url;

use super::cookie::parse_key_32;

/// Largest accepted clock skew for `exp`, `nbf` and `iat`. README 8.3: not configurable higher.
pub const MAX_CLOCK_SKEW: Duration = Duration::from_secs(60);
/// Smallest interval between two forced JWKS refreshes (unknown `kid`) per issuer.
pub const MIN_JWKS_REFRESH_INTERVAL: Duration = Duration::from_secs(30);
/// Upper bound of the JWKS cache lifetime, also applied to `Cache-Control: max-age`.
pub const MAX_JWKS_TTL: Duration = Duration::from_secs(3600);
/// Lower bound of the JWKS cache lifetime: keeps the background refresher from hammering Keycloak.
pub const MIN_JWKS_TTL: Duration = Duration::from_secs(30);
/// Largest lifetime of the sealed `state`/`nonce` cookie (SEC-01: at most 10 minutes).
pub const MAX_FLOW_TTL: Duration = Duration::from_secs(600);
/// Default cap on the number of `groups` taken from one token.
pub const DEFAULT_MAX_GROUPS: usize = 256;
/// Default cap on the byte length of one group path.
pub const DEFAULT_MAX_GROUP_BYTES: usize = 512;
/// Default cap on the byte length of a presented JWT.
pub const DEFAULT_MAX_TOKEN_BYTES: usize = 32 * 1024;
/// Default name of the cookie that carries the sealed `state`, `nonce` and PKCE verifier.
pub const DEFAULT_FLOW_COOKIE_NAME: &str = "rustryx_oidc_flow";
/// Default name of the claim that carries the group paths.
pub const DEFAULT_GROUPS_CLAIM: &str = "groups";
/// Claims this module reads for another purpose, and the role claims that never carry groups.
/// None of them may be configured as the groups claim.
const RESERVED_CLAIMS: &[&str] = &[
    "iss",
    "sub",
    "aud",
    "exp",
    "nbf",
    "iat",
    "azp",
    "typ",
    "scope",
    "nonce",
    "preferred_username",
    "email",
    "name",
    "realm_access",
    "resource_access",
    "roles",
];

/// Scope that would mint an offline token. It is never requested and never accepted.
pub(crate) const SCOPE_OFFLINE_ACCESS: &str = "offline_access";
/// The single audience Keycloak adds by default. It must never authorize on its own.
const AUDIENCE_ACCOUNT: &str = "account";

/// Settings of the interactive login (authorization code + PKCE) for the UI client.
#[derive(Debug, Clone)]
pub struct UiClientConfig {
    /// Client id of the confidential UI client (`rustryx-ui`).
    pub client_id: String,
    /// Client secret. Public clients and password grant are not supported at all.
    pub client_secret: Secret,
    /// Redirect URI registered in Keycloak for this client, an exact value.
    pub redirect_url: String,
    /// 32-byte AES-256-GCM key for the flow cookie, as 64 hex chars or base64.
    pub cookie_key: Secret,
    /// Cookie name. Also bound into the AEAD tag so a value cannot move between cookies.
    pub cookie_name: String,
    /// Lifetime of the sealed flow cookie, at most [`MAX_FLOW_TTL`].
    pub flow_ttl: Duration,
    /// Scopes requested besides the implicit `openid` (default `profile` and `email`). A
    /// scope that makes Keycloak add the `groups` claim goes here too. `offline_access` is refused.
    pub scopes: Vec<String>,
}

impl UiClientConfig {
    /// Settings with defaults for everything except the four required values.
    #[must_use]
    pub fn new(
        client_id: impl Into<String>,
        client_secret: Secret,
        redirect_url: impl Into<String>,
        cookie_key: Secret,
    ) -> Self {
        Self {
            client_id: client_id.into(),
            client_secret,
            redirect_url: redirect_url.into(),
            cookie_key,
            cookie_name: DEFAULT_FLOW_COOKIE_NAME.to_owned(),
            flow_ttl: MAX_FLOW_TTL,
            scopes: vec!["profile".to_owned(), "email".to_owned()],
        }
    }
}

/// Settings of the OIDC integration for one issuer.
#[derive(Debug, Clone)]
pub struct OidcConfig {
    /// Realm URL the server reaches Keycloak at (discovery is fetched from here). HTTPS only.
    pub issuer_url: String,
    /// Realm URL as browsers and tokens see it (split horizon). When set, `iss` must equal this
    /// value, and endpoints under the realm URL are rewritten between the two hosts.
    pub public_issuer_url: Option<String>,
    /// Client id of the product, which `aud` must contain. The single value `account` is refused.
    pub audience: String,
    /// The allow list of `azp` values: the clients whose access tokens this service accepts.
    /// A token without `azp` is not checked against it (Keycloak always sets the claim).
    ///
    /// The list is the WHOLE list, compared byte for byte: an operator who adds `rustryx-cli`
    /// next to the product client writes both. Empty means only [`Self::audience`], which is
    /// the default of `auth.oidc.authorized_parties` in `rustryx-config` (`[client_id]`), so a
    /// second client is always an explicit entry and never implied. Entries are client ids:
    /// no empty value, no whitespace, no wildcard (a `*` would be compared as the literal
    /// text `*` and match nothing, which is a misconfiguration worth refusing at startup).
    pub authorized_parties: Vec<String>,
    /// Skew for `exp`, `nbf` and `iat`, at most [`MAX_CLOCK_SKEW`].
    pub clock_skew: Duration,
    /// Cache lifetime of the JWKS when the response carries no usable `Cache-Control`.
    pub jwks_ttl: Duration,
    /// Minimum spacing of JWKS fetches triggered by an unknown `kid`, at least 30 s.
    pub jwks_min_refresh_interval: Duration,
    /// Period of the background discovery refresh once discovery succeeded.
    pub discovery_refresh: Duration,
    /// Period of discovery retries while it has not succeeded yet.
    pub discovery_retry: Duration,
    /// TCP connect timeout of outgoing calls to Keycloak.
    pub connect_timeout: Duration,
    /// Total timeout of one outgoing call to Keycloak.
    pub request_timeout: Duration,
    /// Top-level claim that carries the group paths.
    pub groups_claim: String,
    /// Cap on the number of `groups` taken from one token.
    pub max_groups: usize,
    /// Cap on the byte length of one group path.
    pub max_group_bytes: usize,
    /// Cap on the byte length of a presented JWT.
    pub max_token_bytes: usize,
    /// Extra trust anchors (PEM) for a corporate CA, merged with the platform roots.
    pub extra_root_certs_pem: Vec<Vec<u8>>,
    /// Interactive login. `None` disables the UI code flow, bearer validation still works.
    pub ui: Option<UiClientConfig>,
}

impl OidcConfig {
    /// Settings with the normative defaults for one issuer and one product client id.
    #[must_use]
    pub fn new(issuer_url: impl Into<String>, audience: impl Into<String>) -> Self {
        Self {
            issuer_url: issuer_url.into(),
            public_issuer_url: None,
            audience: audience.into(),
            authorized_parties: Vec::new(),
            clock_skew: MAX_CLOCK_SKEW,
            jwks_ttl: Duration::from_secs(600),
            jwks_min_refresh_interval: MIN_JWKS_REFRESH_INTERVAL,
            discovery_refresh: Duration::from_secs(3600),
            discovery_retry: Duration::from_secs(15),
            connect_timeout: Duration::from_secs(3),
            request_timeout: Duration::from_secs(5),
            groups_claim: DEFAULT_GROUPS_CLAIM.to_owned(),
            max_groups: DEFAULT_MAX_GROUPS,
            max_group_bytes: DEFAULT_MAX_GROUP_BYTES,
            max_token_bytes: DEFAULT_MAX_TOKEN_BYTES,
            extra_root_certs_pem: Vec::new(),
            ui: None,
        }
    }

    /// The exact string `iss` must equal: the public issuer when configured.
    #[must_use]
    pub fn expected_issuer(&self) -> &str {
        self.public_issuer_url
            .as_deref()
            .unwrap_or(&self.issuer_url)
    }

    /// Values accepted for `azp`.
    #[must_use]
    pub(crate) fn azp_allowed(&self) -> Vec<String> {
        if self.authorized_parties.is_empty() {
            vec![self.audience.clone()]
        } else {
            self.authorized_parties.clone()
        }
    }

    /// Checks every setting and lists all problems at once.
    pub fn validate(&self) -> Result<(), OidcConfigError> {
        let mut problems = Vec::new();
        check_https_url("issuer_url", &self.issuer_url, &mut problems);
        if let Some(public) = &self.public_issuer_url {
            check_https_url("public_issuer_url", public, &mut problems);
        }
        if self.audience.trim().is_empty() {
            problems.push("audience must not be empty".to_owned());
        } else if self.audience == AUDIENCE_ACCOUNT {
            problems.push(
                "audience must be the product client id, the single value `account` never authorizes"
                    .to_owned(),
            );
        }
        self.validate_authorized_parties(&mut problems);
        self.validate_groups_claim(&mut problems);
        self.validate_durations(&mut problems);
        if self.max_groups == 0 || self.max_group_bytes == 0 || self.max_token_bytes == 0 {
            problems.push(
                "max_groups, max_group_bytes and max_token_bytes must be positive".to_owned(),
            );
        }
        if let Some(ui) = &self.ui {
            validate_ui(ui, &mut problems);
        }
        if problems.is_empty() {
            Ok(())
        } else {
            Err(OidcConfigError { problems })
        }
    }

    fn validate_authorized_parties(&self, problems: &mut Vec<String>) {
        if self.authorized_parties.iter().any(|p| p.trim().is_empty()) {
            problems.push("authorized_parties must not contain empty values".to_owned());
        }
        if self
            .authorized_parties
            .iter()
            .any(|p| !p.trim().is_empty() && p.contains(char::is_whitespace))
        {
            problems.push(
                "authorized_parties entries are client ids and must not contain whitespace"
                    .to_owned(),
            );
        }
        if self.authorized_parties.iter().any(|p| p.contains('*')) {
            problems.push("authorized_parties does not support wildcards".to_owned());
        }
    }

    fn validate_groups_claim(&self, problems: &mut Vec<String>) {
        let claim = self.groups_claim.as_str();
        if claim.trim().is_empty() || claim.contains('.') {
            problems.push("groups_claim must name a top-level claim".to_owned());
        } else if RESERVED_CLAIMS.contains(&claim) {
            problems.push(format!(
                "groups_claim must not be `{claim}`: that claim has another meaning and role claims never carry groups"
            ));
        }
    }

    fn validate_durations(&self, problems: &mut Vec<String>) {
        if self.clock_skew > MAX_CLOCK_SKEW {
            problems.push("clock_skew must not exceed 60s".to_owned());
        }
        if self.jwks_ttl < MIN_JWKS_TTL || self.jwks_ttl > MAX_JWKS_TTL {
            problems.push("jwks_ttl must be between 30s and 1h".to_owned());
        }
        if self.jwks_min_refresh_interval < MIN_JWKS_REFRESH_INTERVAL {
            problems.push("jwks_min_refresh_interval must be at least 30s".to_owned());
        }
        if self.discovery_refresh < MIN_JWKS_TTL {
            problems.push("discovery_refresh must be at least 30s".to_owned());
        }
        if self.discovery_retry < Duration::from_secs(1) {
            problems.push("discovery_retry must be at least 1s".to_owned());
        }
        if self.connect_timeout.is_zero() || self.request_timeout < self.connect_timeout {
            problems.push(
                "timeouts must be positive and request_timeout >= connect_timeout".to_owned(),
            );
        }
    }
}

fn validate_ui(ui: &UiClientConfig, problems: &mut Vec<String>) {
    if ui.client_id.trim().is_empty() {
        problems.push("ui.client_id must not be empty".to_owned());
    }
    if ui.client_secret.expose().trim().is_empty() {
        problems.push(
            "ui.client_secret must not be empty: public clients are not supported".to_owned(),
        );
    }
    match Url::parse(&ui.redirect_url) {
        Ok(url) if redirect_scheme_ok(&url) && !ui.redirect_url.contains('*') => {}
        _ => problems.push(
            "ui.redirect_url must be an exact https URL (http only for loopback), no wildcard"
                .to_owned(),
        ),
    }
    if parse_key_32(&ui.cookie_key).is_err() {
        problems.push("ui.cookie_key must be 32 bytes as 64 hex chars or base64".to_owned());
    }
    if ui.cookie_name.is_empty() || !ui.cookie_name.bytes().all(is_cookie_name_byte) {
        problems.push("ui.cookie_name must be a non-empty RFC 6265 token".to_owned());
    }
    if ui.flow_ttl.is_zero() || ui.flow_ttl > MAX_FLOW_TTL {
        problems.push("ui.flow_ttl must be between 1s and 10m".to_owned());
    }
    for scope in &ui.scopes {
        if scope.is_empty() || scope.contains(char::is_whitespace) {
            problems.push("ui.scopes must not contain empty or spaced values".to_owned());
        } else if scope == SCOPE_OFFLINE_ACCESS {
            problems.push("ui.scopes must not request offline_access".to_owned());
        }
    }
}

fn is_cookie_name_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.')
}

fn redirect_scheme_ok(url: &Url) -> bool {
    match url.scheme() {
        "https" => true,
        "http" => matches!(url.host_str(), Some("localhost" | "127.0.0.1" | "[::1]")),
        _ => false,
    }
}

/// Keycloak is only ever contacted over TLS: the JWKS decides who may sign tokens, so it must
/// come from an authenticated channel (README 8.3).
fn check_https_url(field: &str, value: &str, problems: &mut Vec<String>) {
    match Url::parse(value) {
        Ok(url) if url.scheme() == "https" && url.host_str().is_some() => {
            if url.query().is_some() || url.fragment().is_some() {
                problems.push(format!("{field} must not carry a query or fragment"));
            }
        }
        _ => problems.push(format!("{field} must be an https URL")),
    }
}

/// All problems found in an [`OidcConfig`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OidcConfigError {
    problems: Vec<String>,
}

impl OidcConfigError {
    /// The individual problems, in the order they were found.
    #[must_use]
    pub fn problems(&self) -> &[String] {
        &self.problems
    }
}

impl fmt::Display for OidcConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "invalid oidc configuration: {}",
            self.problems.join("; ")
        )
    }
}

impl std::error::Error for OidcConfigError {}

#[cfg(test)]
mod tests {
    use super::*;

    fn good() -> OidcConfig {
        OidcConfig::new("https://kc.example/realms/r", "rustryx")
    }

    fn ui() -> UiClientConfig {
        UiClientConfig::new(
            "rustryx-ui",
            Secret::new("s3cret"),
            "https://ryx.example/oidc/callback",
            Secret::new(hex::encode((0u8..32).collect::<Vec<_>>())),
        )
    }

    #[test]
    fn default_config_is_valid() {
        assert!(good().validate().is_ok());
        let mut with_ui = good();
        with_ui.ui = Some(ui());
        assert!(with_ui.validate().is_ok());
    }

    #[test]
    fn expected_issuer_prefers_public_url() {
        let mut cfg = good();
        assert_eq!(cfg.expected_issuer(), "https://kc.example/realms/r");
        cfg.public_issuer_url = Some("https://sso.example/realms/r".into());
        assert_eq!(cfg.expected_issuer(), "https://sso.example/realms/r");
    }

    #[test]
    fn azp_defaults_to_audience() {
        let mut cfg = good();
        assert_eq!(cfg.azp_allowed(), vec!["rustryx".to_owned()]);
        cfg.authorized_parties = vec!["rustryx-cli".into()];
        assert_eq!(cfg.azp_allowed(), vec!["rustryx-cli".to_owned()]);
    }

    #[test]
    fn must_not_accept_plain_http_issuer() {
        let mut cfg = good();
        cfg.issuer_url = "http://kc.example/realms/r".into();
        cfg.public_issuer_url = Some("http://sso.example/realms/r".into());
        let err = cfg.validate().unwrap_err();
        assert_eq!(err.problems().len(), 2);
        assert!(err.to_string().contains("issuer_url must be an https URL"));
    }

    #[test]
    fn must_not_accept_account_as_audience() {
        let mut cfg = good();
        cfg.audience = "account".into();
        assert!(cfg.validate().is_err());
        cfg.audience = "  ".into();
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn groups_claim_must_name_a_free_top_level_claim() {
        for (claim, ok) in [
            ("groups", true),
            ("grp", true),
            ("", false),
            ("  ", false),
            ("a.b", false),
            ("realm_access", false),
            ("resource_access", false),
            ("roles", false),
            ("sub", false),
            ("scope", false),
        ] {
            let mut cfg = good();
            cfg.groups_claim = claim.into();
            assert_eq!(cfg.validate().is_ok(), ok, "{claim:?}");
        }
    }

    #[test]
    fn must_not_accept_skew_above_sixty_seconds() {
        let mut cfg = good();
        cfg.clock_skew = Duration::from_secs(61);
        assert!(cfg.validate().is_err());
        cfg.clock_skew = Duration::from_secs(60);
        assert!(cfg.validate().is_ok());
    }

    #[test]
    fn must_not_accept_refresh_window_below_thirty_seconds() {
        let mut cfg = good();
        cfg.jwks_min_refresh_interval = Duration::from_secs(29);
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn rejects_out_of_range_durations_and_caps() {
        let mut cfg = good();
        cfg.jwks_ttl = Duration::from_secs(3601);
        cfg.discovery_refresh = Duration::from_secs(1);
        cfg.discovery_retry = Duration::ZERO;
        cfg.connect_timeout = Duration::from_secs(9);
        cfg.max_groups = 0;
        cfg.issuer_url = "https://kc.example/realms/r?x=1".into();
        cfg.authorized_parties = vec![String::new()];
        let err = cfg.validate().unwrap_err();
        assert!(err.problems().len() >= 6, "{err}");
    }

    #[test]
    fn must_not_accept_public_client_or_offline_scope_for_ui() {
        let mut bad = ui();
        bad.client_secret = Secret::new("  ");
        bad.scopes = vec!["offline_access".into(), "two words".into(), String::new()];
        bad.flow_ttl = Duration::from_secs(601);
        bad.cookie_name = "bad name;".into();
        bad.cookie_key = Secret::new("short");
        bad.redirect_url = "https://ryx.example/*".into();
        let mut cfg = good();
        cfg.ui = Some(bad);
        let err = cfg.validate().unwrap_err();
        assert!(err.problems().len() >= 7, "{err}");
    }

    #[test]
    fn redirect_url_allows_http_only_for_loopback() {
        for (url, ok) in [
            ("http://localhost:8080/cb", true),
            ("http://127.0.0.1:8080/cb", true),
            ("http://[::1]:8080/cb", true),
            ("http://ryx.example/cb", false),
            ("ftp://localhost/cb", false),
            ("not a url", false),
        ] {
            let mut u = ui();
            u.redirect_url = url.into();
            let mut cfg = good();
            cfg.ui = Some(u);
            assert_eq!(cfg.validate().is_ok(), ok, "{url}");
        }
    }

    #[test]
    fn debug_never_shows_secrets() {
        let mut cfg = good();
        cfg.ui = Some(ui());
        let shown = format!("{cfg:?}");
        assert!(!shown.contains("s3cret"));
        assert!(!shown.contains(&hex::encode((0u8..32).collect::<Vec<_>>())));
        assert!(shown.contains("REDACTED"));
    }
}
