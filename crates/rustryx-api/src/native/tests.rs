use crate::session_support::EnvBuilder;
use crate::{AuthApiSettings, Requires, TokenPolicy};

#[tokio::test]
async fn the_module_declares_every_route_with_the_requirement_of_the_contract() {
    let env = EnvBuilder::new().build().await;
    let declared = |path: &str| {
        env.catalog
            .api
            .iter()
            .filter(|d| d.path == path)
            .map(|d| d.requires)
            .collect::<Vec<_>>()
    };
    for path in [
        "/api/v1/auth/providers",
        "/api/v1/auth/login",
        "/api/v1/auth/oidc/login",
        "/api/v1/auth/oidc/callback",
        "/api/v1/auth/logout",
        "/api/v1/auth/session",
        "/api/v1/me",
    ] {
        assert_eq!(declared(path), [Requires::Public], "{path}");
    }
    for path in ["/api/v1/auth/session/refresh", "/api/v1/auth/csrf"] {
        assert_eq!(
            declared(path),
            [Requires::Interactive],
            "{path}: tokens are refused"
        );
    }
    assert!(
        env.catalog
            .artifacts
            .iter()
            .all(|d| !matches!(d.requires, Requires::Public))
    );
}

#[test]
fn defaults_follow_the_requirements() {
    let policy = TokenPolicy::default();
    assert_eq!(
        (
            policy.default_ttl_days,
            policy.max_ttl_days,
            policy.require_expiry
        ),
        (90, 365, true)
    );
    let settings = AuthApiSettings::default();
    assert!(!settings.ldap_enabled && !settings.base_url_configured);
    assert!(settings.oidc_display_name.is_none());
}

#[tokio::test]
async fn the_module_has_a_name_for_the_logs() {
    use crate::RouteModule;
    let env = EnvBuilder::new().build().await;
    let module = crate::NativeAuthModule::new(env.deps_for_test());
    assert_eq!(module.name(), "native-auth");
}
