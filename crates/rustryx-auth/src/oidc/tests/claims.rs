//! Group handling end to end: from the token claim to the principal and the resolver.

use rustryx_core::AuthError;
use rustryx_core::identity::{IdpProvider, ROLE_NX_ADMIN, UserSource};
use rustryx_core::ports::GroupResolver;
use serde_json::{Value, json};

use crate::oidc::test_support::{TestEnv, access_claims, assert_empty, keys, with};

async fn principal_with(env: &TestEnv, groups: Value) -> rustryx_core::identity::Principal {
    let claims = with(access_claims(&env.issuer()), "groups", groups);
    env.authenticate(&keys().rsa.sign(&claims))
        .await
        .unwrap()
        .unwrap()
}

#[tokio::test]
async fn groups_claim_is_carried_byte_for_byte() {
    let env = TestEnv::start_warm().await;
    let exact = [
        "/rustryx/admins",
        "rustryx/admins",
        "/Rustryx/Admins",
        "/rustryx/admins ",
        "/rustryx/admins/",
        "/rustryx/team-payments/publishers",
    ];
    let principal = principal_with(&env, json!(exact)).await;
    assert_eq!(principal.external_groups, exact);
}

#[tokio::test]
async fn absent_null_and_empty_claim_give_no_groups() {
    let env = TestEnv::start_warm().await;
    for groups in [Value::Null, json!([]), json!("/rustryx/admins")] {
        let principal = principal_with(&env, groups).await;
        assert_empty(&principal.external_groups);
        let resolved = crate::oidc::OidcGroupResolver::new()
            .external_groups(&principal)
            .await
            .unwrap();
        assert_empty(&resolved.groups);
    }
}

#[tokio::test]
async fn must_not_derive_groups_from_realm_or_client_roles() {
    let env = TestEnv::start_warm().await;
    let claims = with(
        with(access_claims(&env.issuer()), "groups", Value::Null),
        "roles",
        json!(["nx-admin"]),
    );
    // access_claims already carries realm_access and resource_access with nx-admin.
    let principal = env
        .authenticate(&keys().rsa.sign(&claims))
        .await
        .unwrap()
        .unwrap();
    assert_empty(&principal.external_groups);
    assert_empty(&principal.roles);
    assert!(!principal.has_role(ROLE_NX_ADMIN));
}

#[tokio::test]
async fn caps_group_count_and_length_and_counts_it() {
    let env = TestEnv::start_warm().await;
    let mut groups: Vec<Value> = (0..300).map(|i| json!(format!("/g/{i}"))).collect();
    groups.push(json!(format!("/{}", "x".repeat(600))));
    let principal = principal_with(&env, Value::Array(groups)).await;
    assert_eq!(principal.external_groups.len(), 256);
    assert_eq!(principal.external_groups[255], "/g/255");
    let status = env.provider.status();
    assert_eq!(status.groups_truncated, 45);
    principal_with(&env, json!(["/ok", 5, null, ""])).await;
    assert_eq!(env.provider.status().groups_malformed, 3);
}

#[tokio::test]
async fn resolver_returns_the_groups_of_this_requests_token() {
    let env = TestEnv::start_warm().await;
    let principal = principal_with(&env, json!(["/rustryx/readers"])).await;
    let resolved = env
        .provider
        .group_resolver()
        .external_groups(&principal)
        .await
        .unwrap();
    assert_eq!(resolved.provider, IdpProvider::Oidc);
    assert_eq!(resolved.groups, ["/rustryx/readers"]);
}

#[tokio::test]
async fn must_not_resolve_groups_for_a_principal_of_another_source() {
    let env = TestEnv::start_warm().await;
    let mut principal = rustryx_core::identity::Principal::anonymous();
    principal.source = UserSource::Ldap;
    principal.external_groups = vec!["cn=admins,dc=example".into()];
    let result = env
        .provider
        .group_resolver()
        .external_groups(&principal)
        .await;
    assert!(matches!(result, Err(AuthError::GroupLookupFailed(_))));
}

#[tokio::test]
async fn reads_the_configured_groups_claim_and_nothing_else() {
    let env = TestEnv::start_with(|c| c.groups_claim = "grp".into()).await;
    let claims = with(
        with(access_claims(&env.issuer()), "grp", json!(["/team/a"])),
        "groups",
        json!(["/rustryx/admins"]),
    );
    let principal = env
        .authenticate(&keys().rsa.sign(&claims))
        .await
        .unwrap()
        .unwrap();
    // `groups` is still in the token, but it is not the configured claim.
    assert_eq!(principal.external_groups, ["/team/a"]);
}
