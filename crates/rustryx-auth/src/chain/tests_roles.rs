//! Chain tests for group resolution and role assignment: what an external identity
//! may and may not end up with.

use std::time::Duration;

use rustryx_core::error::AuthError;
use rustryx_core::identity::{IdpProvider, RoleOrigin, UserSource};
use rustryx_core::ports::UserRepo;
use rustryx_testkit::fixtures;

use super::testing::{
    ADMIN_DN, ADMIN_PATH, Answer, Options, harness, harness_with, names, oidc_token,
};
use crate::groups::{JitConfig, JitMode, ProviderPolicy, RoleMapperConfig};

const READERS_DN: &str = "cn=readers,ou=groups,dc=example,dc=test";

fn mapper_with(default_role: Option<&str>, supplement: bool) -> RoleMapperConfig {
    RoleMapperConfig {
        oidc: ProviderPolicy {
            admin_groups: vec![ADMIN_PATH.into()],
            default_role: default_role.map(str::to_owned),
            auto_map_group_name: false,
        },
        ldap: ProviderPolicy {
            admin_groups: vec![ADMIN_DN.into()],
            default_role: None,
            auto_map_group_name: false,
        },
        allow_local_role_supplement: supplement,
        ..RoleMapperConfig::default()
    }
}

async fn stored_origins(h: &super::testing::Harness, name: &str) -> Vec<(String, RoleOrigin)> {
    let user = UserRepo::find_by_username(&h.store, name)
        .await
        .unwrap()
        .unwrap();
    UserRepo::roles_of(&h.store, user.id)
        .await
        .unwrap()
        .into_iter()
        .map(|(role, origin)| (role.name, origin))
        .collect()
}

#[tokio::test]
async fn must_not_grant_nx_admin_through_near_miss_groups_in_a_bearer_token() {
    let h = harness();
    for (index, near_miss) in [
        "/rustryx/admins2",
        "rustryx/admins",
        "/Rustryx/Admins",
        "/rustryx/admins/",
        "/rustryx/admins/sub",
        " /rustryx/admins",
    ]
    .into_iter()
    .enumerate()
    {
        let token = oidc_token(
            &format!("mallory{index}"),
            &format!("sub-{index}"),
            &[near_miss],
            h.exp_in(300 + i64::try_from(index).unwrap()),
        );
        let error = h.bearer(&token).await.unwrap_err();
        assert!(
            matches!(error, AuthError::GroupLookupFailed(_)),
            "{near_miss}: {error:?}"
        );
    }
    let admin = h
        .bearer(&oidc_token(
            "root-sso",
            "sub-root",
            &[ADMIN_PATH],
            h.exp_in(300),
        ))
        .await
        .unwrap();
    assert!(admin.is_admin());
    assert_eq!(admin.external_groups, [ADMIN_PATH]);
}

#[tokio::test]
async fn must_not_treat_realm_or_client_roles_as_groups() {
    // Only the `groups` claim feeds the mapper: a token that names roles elsewhere is
    // just a token with no groups.
    let h = harness();
    let mut parts: Vec<String> = oidc_token("mallory", "sub-m", &[], h.exp_in(300))
        .split('.')
        .map(str::to_owned)
        .collect();
    {
        use base64::Engine;
        let engine = base64::engine::general_purpose::URL_SAFE_NO_PAD;
        let mut claims: serde_json::Value =
            serde_json::from_slice(&engine.decode(&parts[1]).unwrap()).unwrap();
        claims["realm_access"] = serde_json::json!({"roles": ["nx-admin", "admin"]});
        claims["resource_access"] = serde_json::json!({"rustryx-ui": {"roles": ["nx-admin"]}});
        parts[1] = engine.encode(serde_json::to_vec(&claims).unwrap());
    }
    let error = h.bearer(&parts.join(".")).await.unwrap_err();
    assert!(
        matches!(error, AuthError::GroupLookupFailed(_)),
        "{error:?}"
    );
}

#[tokio::test]
async fn empty_groups_remove_oidc_roles_for_that_request_and_do_not_fall_back_to_rows() {
    let h = harness();
    let readers = h.add_role("readers").await;
    h.map_group(IdpProvider::Oidc, "/rustryx/readers", &readers)
        .await;
    let with_group = oidc_token("carol", "sub-c", &["/rustryx/readers"], h.exp_in(300));
    assert_eq!(names(&h.bearer(&with_group).await.unwrap()), ["readers"]);
    assert_eq!(
        stored_origins(&h, "carol").await,
        [("readers".to_owned(), RoleOrigin::Oidc)]
    );

    // Removed from the group in Keycloak: the new token carries no groups.
    h.clock.advance(Duration::from_secs(1));
    let without = oidc_token("carol", "sub-c", &[], h.exp_in(300));
    let error = h.bearer(&without).await.unwrap_err();
    assert!(
        matches!(error, AuthError::GroupLookupFailed(_)),
        "the stored `readers` row must not be read back as a grant: {error:?}"
    );
}

#[tokio::test]
async fn default_role_replaces_the_denial_when_configured() {
    let h = harness_with(Options {
        mapper: mapper_with(Some("guests"), false),
        ..Options::default()
    });
    let guests = h.add_role("guests").await;
    let principal = h
        .bearer(&oidc_token("carol", "sub-c", &["/unmapped"], h.exp_in(300)))
        .await
        .unwrap();
    assert_eq!(principal.roles, [guests]);
    assert_eq!(principal.external_groups.len(), 0);
    let empty = h
        .bearer(&oidc_token("dave", "sub-d", &[], h.exp_in(300)))
        .await
        .unwrap();
    assert_eq!(names(&empty), ["guests"]);
    assert_eq!(h.mapper.unknown_group_count(), 1);
}

#[tokio::test]
async fn must_not_read_persisted_oidc_rows_as_roles() {
    let h = harness();
    let carol = UserRepo::create(&h.store, &fixtures::user("carol", UserSource::Oidc))
        .await
        .unwrap();
    let admin = rustryx_testkit::builtin_role_id("nx-admin").unwrap();
    UserRepo::replace_roles_for_origin(&h.store, carol.id, RoleOrigin::Oidc, &[admin])
        .await
        .unwrap();
    let error = h
        .bearer(&oidc_token(
            "carol",
            "sub-carol",
            &["/unmapped"],
            h.exp_in(300),
        ))
        .await
        .unwrap_err();
    assert!(
        matches!(error, AuthError::GroupLookupFailed(_)),
        "{error:?}"
    );
    // The denial also cleared the stale row.
    assert_eq!(stored_origins(&h, "carol").await.len(), 0);
}

#[tokio::test]
async fn persisted_rows_are_rewritten_at_most_every_five_minutes() {
    let h = harness();
    let readers = h.add_role("readers").await;
    let writers = h.add_role("writers").await;
    h.map_group(IdpProvider::Oidc, "/g/readers", &readers).await;
    h.map_group(IdpProvider::Oidc, "/g/writers", &writers).await;
    let first = oidc_token("carol", "sub-c", &["/g/readers"], h.exp_in(10));
    h.bearer(&first).await.unwrap();
    h.clock.advance(Duration::from_secs(20));
    let second = oidc_token("carol", "sub-c", &["/g/writers"], h.exp_in(10));
    let principal = h.bearer(&second).await.unwrap();
    assert_eq!(
        names(&principal),
        ["writers"],
        "the request itself uses the current claims"
    );
    assert_eq!(
        stored_origins(&h, "carol").await,
        [("readers".to_owned(), RoleOrigin::Oidc)],
        "the table is rewritten at most once per interval"
    );
    h.clock.advance(Duration::from_secs(300));
    let third = oidc_token("carol", "sub-c", &["/g/writers"], h.exp_in(10));
    h.bearer(&third).await.unwrap();
    assert_eq!(
        stored_origins(&h, "carol").await,
        [("writers".to_owned(), RoleOrigin::Oidc)]
    );
}

#[tokio::test]
async fn manual_roles_supplement_external_ones_only_when_enabled_and_never_with_admin() {
    for supplement in [false, true] {
        let h = harness_with(Options {
            mapper: mapper_with(None, supplement),
            ..Options::default()
        });
        let readers = h.add_role("readers").await;
        let extra = h.add_role("extra").await;
        h.map_group(IdpProvider::Oidc, "/g/readers", &readers).await;
        let carol = UserRepo::create(&h.store, &fixtures::user("carol", UserSource::Oidc))
            .await
            .unwrap();
        let admin = rustryx_testkit::builtin_role_id("nx-admin").unwrap();
        UserRepo::replace_roles_for_origin(
            &h.store,
            carol.id,
            RoleOrigin::Manual,
            &[extra.id, admin],
        )
        .await
        .unwrap();
        let token = oidc_token("carol", "sub-carol", &["/g/readers"], h.exp_in(300));
        let principal = h.bearer(&token).await.unwrap();
        let expected: &[&str] = if supplement {
            &["extra", "readers"]
        } else {
            &["readers"]
        };
        assert_eq!(names(&principal), expected, "supplement = {supplement}");
        assert!(!principal.is_admin());
        // Persisting the external rows leaves the manual ones alone.
        let origins = stored_origins(&h, "carol").await;
        assert!(origins.contains(&("extra".to_owned(), RoleOrigin::Manual)));
        assert!(origins.contains(&("readers".to_owned(), RoleOrigin::Oidc)));
    }
}

#[tokio::test]
async fn ldap_roles_follow_the_directory_on_every_login() {
    let h = harness();
    h.add_ldap_user("bob", "ldap secret");
    let readers = h.add_role("readers").await;
    h.map_group(IdpProvider::Ldap, READERS_DN, &readers).await;
    h.ldap_resolver.set(Answer::Groups(vec![READERS_DN.into()]));
    assert_eq!(
        names(&h.basic("bob", "ldap secret").await.unwrap()),
        ["readers"]
    );

    // Removed from the group: no stickiness.
    h.ldap_resolver
        .set(Answer::Groups(vec!["cn=other,ou=groups".into()]));
    let error = h.basic("bob", "ldap secret").await.unwrap_err();
    assert!(
        matches!(error, AuthError::GroupLookupFailed(_)),
        "{error:?}"
    );

    h.ldap_resolver.set(Answer::Groups(vec![ADMIN_DN.into()]));
    assert!(h.basic("bob", "ldap secret").await.unwrap().is_admin());
}

#[tokio::test]
async fn must_not_grant_nx_admin_for_near_miss_ldap_dns() {
    let h = harness();
    h.add_ldap_user("bob", "ldap secret");
    for near_miss in [
        "CN=rustryx-admins,ou=groups,dc=example,dc=test",
        "cn=rustryx-admins, ou=groups,dc=example,dc=test",
        "cn=rustryx-admins,ou=groups,dc=example,dc=test,dc=evil",
    ] {
        h.ldap_resolver.set(Answer::Groups(vec![near_miss.into()]));
        let error = h.basic("bob", "ldap secret").await.unwrap_err();
        assert!(
            matches!(error, AuthError::GroupLookupFailed(_)),
            "{near_miss}: {error:?}"
        );
    }
}

#[tokio::test]
async fn must_not_turn_a_failed_ldap_lookup_into_an_empty_group_list() {
    let h = harness();
    h.add_ldap_user("bob", "ldap secret");
    h.ldap_resolver.set(Answer::Fail);
    let error = h.basic("bob", "ldap secret").await.unwrap_err();
    assert!(
        matches!(error, AuthError::GroupLookupFailed(_)),
        "{error:?}"
    );

    // A default role must not paper over a failure either.
    let with_default = harness_with(Options {
        mapper: RoleMapperConfig {
            ldap: ProviderPolicy {
                default_role: Some("guests".into()),
                ..ProviderPolicy::default()
            },
            ..RoleMapperConfig::default()
        },
        ..Options::default()
    });
    with_default.add_role("guests").await;
    with_default.add_ldap_user("bob", "ldap secret");
    with_default.ldap_resolver.set(Answer::Fail);
    let error = with_default.basic("bob", "ldap secret").await.unwrap_err();
    assert!(
        matches!(error, AuthError::GroupLookupFailed(_)),
        "{error:?}"
    );
}

#[tokio::test]
async fn an_empty_ldap_group_list_is_a_valid_answer_and_is_denied_by_the_mapping() {
    let h = harness();
    h.add_ldap_user("bob", "ldap secret");
    h.ldap_resolver.set(Answer::Groups(Vec::new()));
    let error = h.basic("bob", "ldap secret").await.unwrap_err();
    assert!(matches!(error, AuthError::GroupLookupFailed(msg) if msg.contains("access denied")));
}

#[tokio::test]
async fn ldap_roles_survive_a_directory_outage_for_five_minutes_and_not_longer() {
    let h = harness();
    h.add_ldap_user("bob", "ldap secret");
    let readers = h.add_role("readers").await;
    h.map_group(IdpProvider::Ldap, READERS_DN, &readers).await;
    h.ldap_resolver.set(Answer::Groups(vec![READERS_DN.into()]));
    let confirmed = h.basic("bob", "ldap secret").await.unwrap();
    let confirmed_at = confirmed.groups_confirmed_at.unwrap();

    h.ldap_resolver.set(Answer::Fail);
    h.clock.advance(Duration::from_secs(300));
    let reused = h.basic("bob", "ldap secret").await.unwrap();
    assert_eq!(names(&reused), ["readers"]);
    assert_eq!(
        reused.groups_confirmed_at,
        Some(confirmed_at),
        "the age is the original one"
    );

    h.clock.advance(Duration::from_secs(1));
    let error = h.basic("bob", "ldap secret").await.unwrap_err();
    assert!(
        matches!(error, AuthError::GroupLookupFailed(_)),
        "{error:?}"
    );

    // Once the directory answers again the clock starts over.
    h.ldap_resolver.set(Answer::Groups(vec![READERS_DN.into()]));
    assert_eq!(
        names(&h.basic("bob", "ldap secret").await.unwrap()),
        ["readers"]
    );
}

#[tokio::test]
async fn ldap_overload_is_not_disguised_as_a_group_failure() {
    struct Overloaded;
    #[async_trait::async_trait]
    impl rustryx_core::ports::GroupResolver for Overloaded {
        async fn external_groups(
            &self,
            _principal: &rustryx_core::identity::Principal,
        ) -> Result<rustryx_core::ports::ExternalGroups, AuthError> {
            Err(AuthError::Overloaded)
        }
    }
    let h = harness();
    h.add_ldap_user("bob", "ldap secret");
    // Rebuild a service whose LDAP resolver reports overload.
    let service = {
        let mut deps = super::testing::deps_for(&h);
        deps.ldap = Some(super::ExternalIdp {
            authenticator: h.ldap_auth.clone(),
            resolver: std::sync::Arc::new(Overloaded),
        });
        super::AuthService::new(deps, super::AuthServiceConfig::default())
    };
    let context = rustryx_core::identity::AuthContext::default();
    let error = rustryx_core::ports::Authenticator::authenticate(
        &service,
        &super::testing::basic("bob", "ldap secret"),
        &context,
    )
    .await
    .unwrap_err();
    assert!(matches!(error, AuthError::Overloaded), "{error:?}");
}

#[tokio::test]
async fn missing_ldap_configuration_fails_closed_for_ldap_accounts() {
    let h = harness_with(Options {
        with_ldap: false,
        ..Options::default()
    });
    let bob = UserRepo::create(&h.store, &fixtures::user("bob", UserSource::Ldap))
        .await
        .unwrap();
    let issued = crate::tokens::create_token(
        &h.store,
        &h.clock,
        bob.id,
        crate::tokens::TokenSpec::new("ci"),
    )
    .await
    .unwrap();
    let error = h
        .authenticate(&rustryx_core::identity::Credentials::Bearer(issued.secret))
        .await
        .unwrap_err();
    assert!(
        matches!(error, AuthError::GroupLookupFailed(_)),
        "{error:?}"
    );
}

#[tokio::test]
async fn must_not_accept_a_resolver_that_answers_for_the_wrong_provider() {
    let h = harness();
    h.oidc_resolver
        .set(Answer::WrongProvider(vec![ADMIN_PATH.into()]));
    let error = h
        .bearer(&oidc_token("carol", "sub-c", &[ADMIN_PATH], h.exp_in(300)))
        .await
        .unwrap_err();
    assert!(
        matches!(
            error,
            AuthError::Backend {
                context: "group resolution",
                ..
            }
        ),
        "{error:?}"
    );

    h.add_ldap_user("bob", "ldap secret");
    h.ldap_resolver
        .set(Answer::WrongProvider(vec![ADMIN_DN.into()]));
    let error = h.basic("bob", "ldap secret").await.unwrap_err();
    assert!(
        matches!(
            error,
            AuthError::Backend {
                context: "group resolution",
                ..
            }
        ),
        "{error:?}"
    );
}

#[tokio::test]
async fn manual_provisioning_mode_refuses_unknown_external_identities() {
    let h = harness_with(Options {
        jit: JitConfig {
            mode: JitMode::Manual,
            ..JitConfig::default()
        },
        ..Options::default()
    });
    let error = h
        .bearer(&oidc_token("carol", "sub-c", &[ADMIN_PATH], h.exp_in(300)))
        .await
        .unwrap_err();
    assert!(matches!(error, AuthError::AccountDisabled), "{error:?}");
    h.add_ldap_user("bob", "ldap secret");
    let error = h.basic("bob", "ldap secret").await.unwrap_err();
    assert!(matches!(error, AuthError::AccountDisabled), "{error:?}");
}

#[tokio::test]
async fn disabled_external_account_is_refused() {
    let h = harness();
    let token = oidc_token("carol", "sub-c", &[ADMIN_PATH], h.exp_in(30));
    let principal = h.bearer(&token).await.unwrap();
    UserRepo::set_enabled(&h.store, principal.user_id.unwrap(), false)
        .await
        .unwrap();
    h.service.invalidate_all();
    let error = h.bearer(&token).await.unwrap_err();
    assert!(matches!(error, AuthError::AccountDisabled), "{error:?}");
}

#[tokio::test]
async fn store_outage_during_mapping_is_a_service_error_not_a_denial() {
    let h = harness();
    let token = oidc_token("carol", "sub-c", &[ADMIN_PATH], h.exp_in(300));
    h.store.set_unavailable(true);
    let error = h.bearer(&token).await.unwrap_err();
    assert!(
        matches!(error, AuthError::ProviderUnavailable(_)),
        "{error:?}"
    );
}
