//! Who is calling: the authentication chain and everything it is made of.
//!
//! The pieces come from `rustryx-auth`; this file maps the configuration onto them and fixes the
//! order. Two constraints from the requirements shape it:
//!
//! - **Start without the identity providers.** Keycloak discovery runs in a background task and
//!   LDAP connects on the first login, so an unreachable provider never stops the node. Only a
//!   configuration that cannot work (a bad URL, an unreadable `ca_file`) does.
//! - **Nothing weaker than production when a key is missing.** Production validation requires
//!   the signing and cookie keys. Development may omit them; then they are generated for this
//!   process, so sessions end at a restart instead of being protected by a key anyone can read in
//!   the source.

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, anyhow};
use rustryx_auth::basic::LocalPasswordAuthenticator;
use rustryx_auth::chain::{AuthService, AuthServiceConfig, AuthServiceDeps, ExternalIdp};
use rustryx_auth::groups::{
    JitConfig, JitMode, JitProvisioner, ProviderPolicy, RoleMapper, RoleMapperConfig,
};
use rustryx_auth::internal_jwt::{
    InternalJwtAuthenticator, InternalJwtConfig, InternalJwtService, KeyRing,
};
use rustryx_auth::ldap::{
    LdapAuthenticator, LdapConfig as AuthLdapConfig, LdapGroupResolver, LdapGroupStrategy,
    LdapService,
};
use rustryx_auth::login_guard::{GuardPolicy, LoginGuard};
use rustryx_auth::oidc::{OidcConfig, OidcProvider, UiClientConfig};
use rustryx_auth::passwords::{BlockingGate, GateConfig, LaneLimits, PasswordConfig, Passwords};
use rustryx_auth::tokens::TokenAuthenticator;
use rustryx_config::{AppConfig, GroupStrategy, LdapConfig};
use rustryx_core::Secret;
use rustryx_core::ports::Clock;

use super::sso::KeycloakLogin;
use super::store::Repos;
use crate::supervisor::BackgroundTask;

/// Path of the OIDC callback below the public base URL. The redirect URI registered in Keycloak
/// is `server.base_url` plus this.
pub const OIDC_CALLBACK_PATH: &str = "/api/v1/auth/oidc/callback";

/// The authentication side of the node.
pub struct Identity {
    /// The chain the HTTP layer authenticates with.
    pub service: Arc<AuthService>,
    /// Password hashing, shared with the bootstrap administrator.
    pub passwords: Arc<Passwords>,
    /// The token service behind the cookie session.
    pub jwt: Arc<InternalJwtService>,
    /// The interactive Keycloak login, when OIDC is on and the UI client is configured.
    pub keycloak: Option<Arc<KeycloakLogin>>,
    /// Whether the LDAP provider is part of the chain.
    pub ldap_enabled: bool,
    /// Keycloak discovery, kept running under the supervisor.
    pub background: Vec<BackgroundTask>,
}

/// The key that seals the OIDC flow cookies and derives the CSRF key.
///
#[must_use]
pub fn cookie_key(config: &AppConfig) -> Secret {
    if let Some(key) = config.auth.cookie_key.as_ref() {
        return key.to_secret();
    }
    tracing::warn!(
        "auth.cookie_key is not set: a key for this process is generated, so OIDC logins in \
         flight and CSRF tokens do not survive a restart (development only)"
    );
    let mut bytes = [0_u8; 32];
    rand::fill(&mut bytes);
    Secret::new(hex_encode(&bytes))
}

fn hex_encode(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes
        .iter()
        .fold(String::with_capacity(bytes.len() * 2), |mut out, b| {
            // Writing to a String cannot fail.
            let _ = write!(out, "{b:02x}");
            out
        })
}

/// The signing ring of the internal JWT.
fn key_ring(config: &AppConfig) -> anyhow::Result<KeyRing> {
    let ring = config
        .auth
        .jwt_ring()
        .map_err(|error| anyhow!("auth.internal_jwt_previous_keys: {error}"))?;
    let Some(ring) = ring else {
        tracing::warn!(
            "auth.internal_jwt_key is not set: sessions are signed with a key generated for this \
             process and end at a restart (development only)"
        );
        return KeyRing::generate("ephemeral").map_err(|error| anyhow!("signing key: {error}"));
    };
    KeyRing::from_pem(&ring.current_id, &ring.current, &ring.previous)
        .map_err(|error| anyhow!("auth.internal_jwt_key: {error}"))
}

/// Limits of the blocking lanes. A password hash saturates a core, so the lane is as wide as the
/// physical cores unless the operator says otherwise (`limits.password_permits`).
fn gate(config: &AppConfig) -> BlockingGate {
    let limits = &config.limits;
    let password = limits.effective_password_permits();
    let general = limits.effective_blocking_permits();
    BlockingGate::new(GateConfig {
        password: LaneLimits {
            permits: password,
            max_waiting: password,
        },
        general: LaneLimits {
            permits: general,
            max_waiting: general,
        },
        permit_wait: limits.permit_wait(),
    })
}

fn jit_mode(config: &AppConfig) -> JitMode {
    match config.auth.jit_mode {
        rustryx_config::JitMode::Jit => JitMode::Jit,
        rustryx_config::JitMode::Allowlist => JitMode::Allowlist,
        rustryx_config::JitMode::Manual => JitMode::Manual,
    }
}

fn role_mapper_config(config: &AppConfig) -> RoleMapperConfig {
    let auth = &config.auth;
    RoleMapperConfig {
        oidc: ProviderPolicy {
            admin_groups: auth.oidc.admin_groups.clone(),
            default_role: auth.oidc.default_role.clone(),
            auto_map_group_name: false,
        },
        ldap: ProviderPolicy {
            admin_groups: auth.ldap.admin_groups.clone(),
            default_role: None,
            auto_map_group_name: false,
        },
        allow_local_role_supplement: auth.allow_local_role_supplement,
        ..RoleMapperConfig::default()
    }
}

/// The LDAP section in the shape `rustryx-auth` reads (it must not depend on the config crate).
#[must_use]
pub fn ldap_config(ldap: &LdapConfig) -> AuthLdapConfig {
    AuthLdapConfig {
        enabled: ldap.enabled,
        url: ldap.url.clone(),
        starttls: ldap.starttls,
        ca_file: ldap.ca_file.clone(),
        bind_dn: ldap.bind_dn.clone(),
        bind_password: ldap
            .bind_password
            .as_ref()
            .map(rustryx_config::ConfigSecret::to_secret),
        user_base_dn: ldap.user_base_dn.clone(),
        search_filter: ldap.search_filter.clone(),
        group_strategy: match ldap.group_strategy {
            GroupStrategy::MemberOf => LdapGroupStrategy::MemberOf,
            GroupStrategy::MemberSearch => LdapGroupStrategy::ReverseMember,
        },
        group_attribute: ldap.group_attribute.clone(),
        group_base_dn: ldap.group_base_dn.clone(),
        group_filter: ldap.group_filter.clone(),
        // Nested groups walk the same relation as the direct search.
        nested_group_filter: ldap.group_filter.clone(),
        nested_groups: ldap.nested_group_depth > 0,
        nested_group_depth: ldap.nested_group_depth.max(1),
        max_groups: ldap.max_groups,
        groups_cache_ttl_secs: ldap.groups_cache_ttl_secs,
        admin_groups: ldap.admin_groups.clone(),
        connect_timeout_secs: ldap.connect_timeout_secs,
        bind_timeout_secs: ldap.bind_timeout_secs,
        search_timeout_secs: ldap.search_timeout_secs,
        ..AuthLdapConfig::default()
    }
}

/// The Keycloak settings, with the UI client when the base URL and the client secret allow one.
///
/// # Errors
/// OIDC is enabled without its issuer or client id (validation refuses that first).
pub fn oidc_config(config: &AppConfig, cookie_key: &Secret) -> anyhow::Result<OidcConfig> {
    let oidc = &config.auth.oidc;
    let issuer = oidc.issuer.clone().context("auth.oidc.issuer is not set")?;
    let client_id = oidc
        .client_id
        .clone()
        .context("auth.oidc.client_id is not set")?;
    let mut settings = OidcConfig::new(issuer, client_id.clone());
    settings
        .public_issuer_url
        .clone_from(&oidc.public_issuer_url);
    settings.authorized_parties = oidc.effective_authorized_parties();
    settings.clock_skew = Duration::from_secs(oidc.clock_skew_secs);
    settings.groups_claim.clone_from(&oidc.groups_claim);
    if let (Some(secret), Some(base_url)) = (
        oidc.client_secret.as_ref(),
        config.server.base_url.as_deref(),
    ) {
        let redirect = format!("{}{OIDC_CALLBACK_PATH}", base_url.trim_end_matches('/'));
        let mut ui =
            UiClientConfig::new(client_id, secret.to_secret(), redirect, cookie_key.clone());
        // `openid` is added by the protocol code itself.
        ui.scopes = oidc
            .scopes
            .iter()
            .filter(|scope| scope.as_str() != "openid")
            .cloned()
            .collect();
        settings.ui = Some(ui);
    } else {
        tracing::warn!(
            "auth.oidc is enabled without auth.oidc.client_secret or server.base_url: Keycloak \
             bearer tokens are accepted but the browser login is off"
        );
    }
    Ok(settings)
}

struct OidcParts {
    external: ExternalIdp,
    keycloak: Option<Arc<KeycloakLogin>>,
    background: BackgroundTask,
}

fn build_oidc(
    config: &AppConfig,
    cookie_key: &Secret,
    jit: &Arc<JitProvisioner>,
    clock: &Arc<dyn Clock>,
) -> anyhow::Result<OidcParts> {
    let settings = oidc_config(config, cookie_key)?;
    let ui_enabled = settings.ui.is_some();
    let provider = Arc::new(
        OidcProvider::with_clock(settings, Arc::clone(clock))
            .map_err(|error| anyhow!("auth.oidc: {error}"))?,
    );
    let external = ExternalIdp {
        authenticator: Arc::new(provider.bearer_authenticator()),
        resolver: Arc::new(provider.group_resolver()),
    };
    let keycloak = match provider.login_flow() {
        Some(flow) if ui_enabled => Some(Arc::new(KeycloakLogin::new(
            Arc::clone(&provider),
            flow,
            cookie_key,
            OIDC_CALLBACK_PATH,
            Arc::clone(jit),
            Arc::clone(clock),
        )?)),
        _ => None,
    };
    let discovery = Arc::clone(&provider);
    let background = BackgroundTask::new("oidc_discovery", move |cancel| {
        let provider = Arc::clone(&discovery);
        async move {
            // The provider owns its refresh loop and ends it when `cancel` fires. A panic inside
            // surfaces here as a join error, and returning makes the supervisor restart it.
            if let Err(error) = provider.spawn_background(cancel).await {
                tracing::error!(%error, "oidc discovery task failed");
            }
        }
    });
    Ok(OidcParts {
        external,
        keycloak,
        background,
    })
}

async fn build_ldap(config: &AppConfig, clock: &Arc<dyn Clock>) -> anyhow::Result<ExternalIdp> {
    let service = LdapService::new(&ldap_config(&config.auth.ldap), Arc::clone(clock))
        .await
        .map_err(|error| anyhow!("auth.ldap: {error}"))?;
    Ok(ExternalIdp {
        authenticator: Arc::new(LdapAuthenticator::new(Arc::clone(&service))),
        resolver: Arc::new(LdapGroupResolver::new(service)),
    })
}

/// Hashes at startup cost one hash time (the dummy hash); the blocking pool keeps it off the
/// async workers.
async fn build_passwords(config: &AppConfig) -> anyhow::Result<Arc<Passwords>> {
    let gate = Arc::new(gate(config));
    let passwords =
        tokio::task::spawn_blocking(move || Passwords::new(PasswordConfig::recommended(), gate))
            .await
            .context("password hashing setup was cancelled")?
            .map_err(|error| anyhow!("password hashing: {error}"))?;
    Ok(Arc::new(passwords))
}

/// Builds the chain over `repos`.
///
/// # Errors
/// A mandatory part cannot be built: a malformed key ring, an invalid Keycloak or LDAP
/// configuration, the password hasher.
pub async fn build(
    config: &AppConfig,
    repos: &Repos,
    cookie_key: &Secret,
) -> anyhow::Result<Identity> {
    let clock = Arc::clone(&repos.clock);
    let passwords = build_passwords(config).await?;
    let jwt = Arc::new(InternalJwtService::new(
        key_ring(config)?,
        InternalJwtConfig::default(),
        Arc::clone(&clock),
        Arc::clone(&repos.users),
        Arc::clone(&repos.revoked_tokens),
    ));
    let jit = Arc::new(JitProvisioner::new(
        Arc::clone(&repos.users),
        Arc::clone(&clock),
        JitConfig {
            mode: jit_mode(config),
            allowlist: config.auth.jit_allowlist.iter().cloned().collect(),
            ..JitConfig::default()
        },
    ));
    let mapper = Arc::new(RoleMapper::new(
        Arc::clone(&repos.idp_mappings),
        Arc::clone(&repos.roles),
        role_mapper_config(config),
    ));
    let guard = Arc::new(LoginGuard::new(
        Arc::clone(&repos.login_attempts),
        Arc::clone(&clock),
        GuardPolicy::default(),
    ));

    let mut background = Vec::new();
    let mut keycloak = None;
    let oidc = if config.auth.oidc.enabled {
        let parts = build_oidc(config, cookie_key, &jit, &clock)?;
        keycloak = parts.keycloak;
        background.push(parts.background);
        Some(parts.external)
    } else {
        None
    };
    let ldap_enabled = config.auth.ldap.enabled;
    let ldap = if ldap_enabled {
        Some(build_ldap(config, &clock).await?)
    } else {
        None
    };

    let deps = AuthServiceDeps {
        users: Arc::clone(&repos.users),
        clock: Arc::clone(&clock),
        local_basic: Arc::new(LocalPasswordAuthenticator::new(
            Arc::clone(&repos.users),
            Arc::clone(&passwords),
        )),
        api_token: Arc::new(TokenAuthenticator::new(
            Arc::clone(&repos.tokens),
            Arc::clone(&repos.users),
            Arc::clone(&clock),
        )),
        internal_jwt: Arc::new(InternalJwtAuthenticator::new(Arc::clone(&jwt))),
        oidc,
        ldap,
        guard,
        passwords: Arc::clone(&passwords),
        mapper,
        jit,
    };
    let service = Arc::new(AuthService::new(
        deps,
        AuthServiceConfig {
            // The HTTP layer counts every attempt itself (`auth_attempts_total`); the chain
            // counting too would double every series.
            record_attempt_metrics: false,
            breakglass_username: Some(config.auth.bootstrap_admin.username.clone()),
            ..AuthServiceConfig::default()
        },
    ));
    Ok(Identity {
        service,
        passwords,
        jwt,
        keycloak,
        ldap_enabled,
        background,
    })
}

#[cfg(test)]
mod tests {
    use rustryx_config::ConfigSecret;

    use super::*;

    fn oidc_on() -> AppConfig {
        let mut config = AppConfig::default();
        let oidc = &mut config.auth.oidc;
        oidc.enabled = true;
        oidc.issuer = Some("https://kc.example/realms/r".into());
        oidc.client_id = Some("rustryx-ui".into());
        oidc.client_secret = Some(ConfigSecret::new("s3cret"));
        oidc.authorized_parties = vec!["rustryx-cli".into()];
        config.server.base_url = Some("https://repo.example/".into());
        config
    }

    #[test]
    fn the_redirect_uri_is_the_base_url_plus_the_callback_path() {
        let ui = oidc_config(&oidc_on(), &Secret::new("k"))
            .unwrap()
            .ui
            .expect("ui client");
        assert_eq!(
            ui.redirect_url,
            "https://repo.example/api/v1/auth/oidc/callback"
        );
        assert!(!ui.scopes.iter().any(|s| s == "openid"));
    }

    #[test]
    fn the_authorized_parties_are_the_product_client_plus_the_listed_ones() {
        let settings = oidc_config(&oidc_on(), &Secret::new("k")).unwrap();
        assert_eq!(settings.audience, "rustryx-ui");
        assert_eq!(settings.authorized_parties, ["rustryx-ui", "rustryx-cli"]);
    }

    #[test]
    fn without_a_base_url_or_secret_there_is_no_browser_login() {
        let mut config = oidc_on();
        config.server.base_url = None;
        assert!(
            oidc_config(&config, &Secret::new("k"))
                .unwrap()
                .ui
                .is_none()
        );
        let mut config = oidc_on();
        config.auth.oidc.client_secret = None;
        assert!(
            oidc_config(&config, &Secret::new("k"))
                .unwrap()
                .ui
                .is_none()
        );
    }

    #[test]
    fn must_not_build_keycloak_settings_without_issuer_or_client() {
        let mut config = oidc_on();
        config.auth.oidc.issuer = None;
        assert!(oidc_config(&config, &Secret::new("k")).is_err());
        let mut config = oidc_on();
        config.auth.oidc.client_id = None;
        assert!(oidc_config(&config, &Secret::new("k")).is_err());
    }

    #[test]
    fn ldap_settings_follow_the_config_section() {
        let mut ldap = LdapConfig::default();
        ldap.enabled = true;
        ldap.url = Some("ldaps://ldap.example".into());
        ldap.bind_password = Some(ConfigSecret::new("pw"));
        ldap.bind_dn = Some("cn=svc,dc=example,dc=com".into());
        ldap.user_base_dn = Some("ou=people,dc=example,dc=com".into());
        ldap.group_base_dn = Some("ou=groups,dc=example,dc=com".into());
        ldap.group_strategy = GroupStrategy::MemberSearch;
        ldap.nested_group_depth = 0;
        ldap.groups_cache_ttl_secs = 45;
        let mapped = ldap_config(&ldap);
        assert!(mapped.enabled);
        assert_eq!(mapped.group_strategy, LdapGroupStrategy::ReverseMember);
        assert!(!mapped.nested_groups);
        assert_eq!(mapped.nested_group_depth, 1);
        assert_eq!(mapped.groups_cache_ttl_secs, 45);
        assert_eq!(mapped.bind_password.as_ref().unwrap().expose(), "pw");
        mapped.validate().expect("a complete section validates");
    }

    #[test]
    fn the_jit_mode_maps_one_to_one() {
        let mut config = AppConfig::default();
        for (from, to) in [
            (rustryx_config::JitMode::Jit, JitMode::Jit),
            (rustryx_config::JitMode::Allowlist, JitMode::Allowlist),
            (rustryx_config::JitMode::Manual, JitMode::Manual),
        ] {
            config.auth.jit_mode = from;
            assert_eq!(jit_mode(&config), to);
        }
    }

    #[test]
    fn only_the_admin_groups_of_a_provider_reach_that_provider_policy() {
        let mut config = AppConfig::default();
        config.auth.oidc.admin_groups = vec!["/admins".into()];
        config.auth.oidc.default_role = Some("viewer".into());
        config.auth.ldap.admin_groups = vec!["cn=ops,dc=x".into()];
        let mapper = role_mapper_config(&config);
        assert_eq!(mapper.oidc.admin_groups, ["/admins"]);
        assert_eq!(mapper.oidc.default_role.as_deref(), Some("viewer"));
        assert_eq!(mapper.ldap.admin_groups, ["cn=ops,dc=x"]);
        assert_eq!(mapper.ldap.default_role, None);
        assert!(!mapper.oidc.auto_map_group_name && !mapper.ldap.auto_map_group_name);
        assert!(!mapper.allow_local_role_supplement);
    }

    #[test]
    fn a_missing_cookie_key_is_generated_and_a_configured_one_is_used() {
        let mut config = AppConfig::default();
        let generated = cookie_key(&config);
        assert_eq!(generated.expose().len(), 64);
        assert_ne!(generated.expose(), cookie_key(&config).expose());
        config.auth.cookie_key = Some(ConfigSecret::new("configured"));
        assert_eq!(cookie_key(&config).expose(), "configured");
    }

    #[test]
    fn a_generated_signing_key_ring_builds_and_a_garbage_one_is_refused() {
        let mut config = AppConfig::default();
        assert!(key_ring(&config).is_ok(), "development generates a key");
        config.auth.internal_jwt_key = Some(ConfigSecret::new("not a pem"));
        let error = key_ring(&config).expect_err("not a key");
        assert!(!format!("{error:#}").contains("not a pem"));
    }
}
