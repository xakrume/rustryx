//! Tests of the group to role mapping, with emphasis on what must never be granted.

use proptest::prelude::*;
use rustryx_core::identity::{IdpGroupMapping, ROLE_NX_DEVELOPER, Role};
use rustryx_core::ids::{IdpGroupMappingId, RoleId};
use rustryx_testkit::MemStore;

use super::*;

const ADMIN_PATH: &str = "/rustryx/admins";
const ADMIN_DN: &str = "cn=rustryx-admins,ou=groups,dc=example,dc=test";

struct Env {
    store: MemStore,
    mapper: RoleMapper,
}

fn policy(admin: &str, default_role: Option<&str>) -> ProviderPolicy {
    ProviderPolicy {
        admin_groups: vec![admin.to_owned()],
        default_role: default_role.map(str::to_owned),
        auto_map_group_name: false,
    }
}

fn config() -> RoleMapperConfig {
    RoleMapperConfig {
        oidc: policy(ADMIN_PATH, None),
        ldap: policy(ADMIN_DN, None),
        ..RoleMapperConfig::default()
    }
}

fn mapper_over(store: &MemStore, config: RoleMapperConfig) -> RoleMapper {
    RoleMapper::new(Arc::new(store.clone()), Arc::new(store.clone()), config)
}

async fn add_role(store: &MemStore, name: &str) -> RoleRef {
    let role = Role {
        id: RoleId::generate(),
        name: name.to_owned(),
        description: String::new(),
        builtin: false,
        privilege_ids: Vec::new(),
        nested_role_ids: Vec::new(),
    };
    RoleRepo::create(store, &role).await.unwrap();
    RoleRef {
        id: role.id,
        name: role.name,
    }
}

async fn map_group(
    store: &MemStore,
    provider: IdpProvider,
    group: &str,
    role: &RoleRef,
    priority: i32,
) {
    let mapping = IdpGroupMapping {
        id: IdpGroupMappingId::generate(),
        provider,
        external_group: group.to_owned(),
        role_id: role.id,
        priority,
    };
    IdpMappingRepo::upsert(store, &mapping).await.unwrap();
}

fn env_with(config: RoleMapperConfig) -> Env {
    let store = MemStore::with_builtin_roles();
    let mapper = mapper_over(&store, config);
    Env { store, mapper }
}

fn env() -> Env {
    env_with(config())
}

fn groups(list: &[&str]) -> Vec<String> {
    list.iter().map(|g| (*g).to_owned()).collect()
}

async fn granted(env: &Env, provider: IdpProvider, list: &[&str]) -> ExternalRoles {
    match env
        .mapper
        .map_groups(provider, &groups(list))
        .await
        .unwrap()
    {
        MappingOutcome::Granted(roles) => roles,
        MappingOutcome::Denied => unreachable!("expected a grant for {list:?}"),
    }
}

fn names(roles: &ExternalRoles) -> Vec<&str> {
    roles.roles.iter().map(|r| r.name.as_str()).collect()
}

async fn outcome(env: &Env, provider: IdpProvider, list: &[&str]) -> MappingOutcome {
    env.mapper
        .map_groups(provider, &groups(list))
        .await
        .unwrap()
}

#[tokio::test]
async fn explicit_mapping_grants_its_role_and_unknown_groups_are_counted() {
    let env = env();
    let readers = add_role(&env.store, "readers").await;
    map_group(
        &env.store,
        IdpProvider::Oidc,
        "/rustryx/readers",
        &readers,
        10,
    )
    .await;
    let roles = granted(
        &env,
        IdpProvider::Oidc,
        &["/rustryx/readers", "/other/team", "/x"],
    )
    .await;
    assert_eq!(names(&roles), ["readers"]);
    assert_eq!(roles.matched_groups, ["/rustryx/readers"]);
    assert_eq!(env.mapper.unknown_group_count(), 2);
}

#[tokio::test]
async fn admin_group_grants_nx_admin_first() {
    let env = env();
    let readers = add_role(&env.store, "readers").await;
    map_group(
        &env.store,
        IdpProvider::Oidc,
        "/rustryx/readers",
        &readers,
        1,
    )
    .await;
    let roles = granted(&env, IdpProvider::Oidc, &["/rustryx/readers", ADMIN_PATH]).await;
    assert_eq!(names(&roles), [ROLE_NX_ADMIN, "readers"]);
    assert_eq!(roles.matched_groups, ["/rustryx/readers", ADMIN_PATH]);
}

#[tokio::test]
async fn must_not_grant_nx_admin_for_near_miss_oidc_paths() {
    let env = env();
    for near_miss in [
        "/rustryx/admins2",
        "/rustryx/admin",
        "rustryx/admins",
        "/rustryx/admins/",
        "//rustryx/admins",
        "/Rustryx/Admins",
        "/RUSTRYX/ADMINS",
        " /rustryx/admins",
        "/rustryx/admins ",
        "/rustryx/admins\n",
        "/rustryx/admins\0",
        "/rustryx/admins/sub",
        "/x/rustryx/admins",
        "x/rustryx/admins",
        "admins",
        "/rustryx",
        "/rustryx/admins#",
        "\u{ff0f}rustryx\u{ff0f}admins",
        "",
    ] {
        let result = outcome(&env, IdpProvider::Oidc, &[near_miss]).await;
        assert_eq!(result, MappingOutcome::Denied, "{near_miss:?}");
    }
    let roles = granted(&env, IdpProvider::Oidc, &[ADMIN_PATH]).await;
    assert_eq!(names(&roles), [ROLE_NX_ADMIN]);
}

#[tokio::test]
async fn must_not_grant_nx_admin_for_near_miss_ldap_dns() {
    let env = env();
    for near_miss in [
        "CN=rustryx-admins,ou=groups,dc=example,dc=test",
        "cn=rustryx-admins, ou=groups,dc=example,dc=test",
        "cn=rustryx-admins,ou=groups,dc=example,dc=test ",
        "cn=rustryx-admins,ou=groups,dc=example",
        "x,cn=rustryx-admins,ou=groups,dc=example,dc=test",
        "cn=rustryx-admins,ou=groups,dc=example,dc=test,dc=evil",
        "cn=rustryx-admins2,ou=groups,dc=example,dc=test",
    ] {
        let result = outcome(&env, IdpProvider::Ldap, &[near_miss]).await;
        assert_eq!(result, MappingOutcome::Denied, "{near_miss:?}");
    }
    let roles = granted(&env, IdpProvider::Ldap, &[ADMIN_DN]).await;
    assert_eq!(names(&roles), [ROLE_NX_ADMIN]);
}

#[tokio::test]
async fn must_not_cross_providers_for_admin_groups() {
    let env = env();
    assert_eq!(
        outcome(&env, IdpProvider::Ldap, &[ADMIN_PATH]).await,
        MappingOutcome::Denied
    );
    assert_eq!(
        outcome(&env, IdpProvider::Oidc, &[ADMIN_DN]).await,
        MappingOutcome::Denied
    );
}

#[tokio::test]
async fn must_not_let_an_empty_admin_entry_match_an_empty_group() {
    let mut cfg = config();
    cfg.oidc.admin_groups = vec![String::new()];
    let env = env_with(cfg);
    assert_eq!(
        outcome(&env, IdpProvider::Oidc, &[""]).await,
        MappingOutcome::Denied
    );
}

#[tokio::test]
async fn must_not_grant_nx_admin_through_a_mapping_row() {
    let env = env();
    let admin = RoleRef {
        id: rustryx_testkit::builtin_role_id(ROLE_NX_ADMIN).unwrap(),
        name: ROLE_NX_ADMIN.to_owned(),
    };
    map_group(&env.store, IdpProvider::Oidc, "/sneaky/group", &admin, 0).await;
    let result = outcome(&env, IdpProvider::Oidc, &["/sneaky/group"]).await;
    assert_eq!(result, MappingOutcome::Denied);
}

#[tokio::test]
async fn auto_mapping_is_off_by_default() {
    let env = env();
    let result = outcome(&env, IdpProvider::Oidc, &["/team/nx-developer"]).await;
    assert_eq!(result, MappingOutcome::Denied);
}

#[tokio::test]
async fn auto_mapping_grants_matching_roles_but_never_nx_admin_or_anonymous() {
    let mut cfg = config();
    cfg.oidc.auto_map_group_name = true;
    cfg.ldap.auto_map_group_name = true;
    let env = env_with(cfg);
    let team = add_role(&env.store, "team-x").await;
    let roles = granted(&env, IdpProvider::Oidc, &["/a/b/team-x", "/a/b/team-x/"]).await;
    assert_eq!(roles.roles, std::slice::from_ref(&team));
    let roles = granted(
        &env,
        IdpProvider::Ldap,
        &["cn=team-x,ou=groups,dc=example,dc=test"],
    )
    .await;
    assert_eq!(roles.roles, [team]);
    let developer = granted(&env, IdpProvider::Oidc, &["/x/nx-developer"]).await;
    assert_eq!(names(&developer), [ROLE_NX_DEVELOPER]);
    for group in ["/x/nx-admin", "/nx-admin", "/x/nx-anonymous"] {
        assert_eq!(
            outcome(&env, IdpProvider::Oidc, &[group]).await,
            MappingOutcome::Denied,
            "{group}"
        );
    }
    for dn in ["cn=nx-admin,ou=groups,dc=x", "CN=nx-anonymous,ou=g,dc=x"] {
        assert_eq!(
            outcome(&env, IdpProvider::Ldap, &[dn]).await,
            MappingOutcome::Denied,
            "{dn}"
        );
    }
    // Case differs from the role name: no match.
    assert_eq!(
        outcome(&env, IdpProvider::Oidc, &["/a/Team-X"]).await,
        MappingOutcome::Denied
    );
    // Escaped or multi-valued RDNs are skipped, not guessed at.
    for dn in [
        "cn=team\\2dx,ou=g",
        "cn=team-x+uid=1,ou=g",
        "ou=g,cn=team-x",
    ] {
        assert_eq!(
            outcome(&env, IdpProvider::Ldap, &[dn]).await,
            MappingOutcome::Denied,
            "{dn}"
        );
    }
}

#[tokio::test]
async fn default_role_applies_only_when_nothing_matched() {
    let mut cfg = config();
    cfg.oidc.default_role = Some("guests".to_owned());
    let env = env_with(cfg);
    let guests = add_role(&env.store, "guests").await;
    let readers = add_role(&env.store, "readers").await;
    map_group(
        &env.store,
        IdpProvider::Oidc,
        "/rustryx/readers",
        &readers,
        0,
    )
    .await;

    let fallback = granted(&env, IdpProvider::Oidc, &["/unmapped"]).await;
    assert_eq!(fallback.roles, std::slice::from_ref(&guests));
    assert_eq!(fallback.matched_groups.len(), 0);
    let empty = granted(&env, IdpProvider::Oidc, &[]).await;
    assert_eq!(empty.roles, [guests]);
    let matched = granted(&env, IdpProvider::Oidc, &["/rustryx/readers", "/unmapped"]).await;
    assert_eq!(
        names(&matched),
        ["readers"],
        "no default role next to a matched group"
    );
    // LDAP has no default role of its own: the OIDC setting does not leak across.
    assert_eq!(
        outcome(&env, IdpProvider::Ldap, &["x"]).await,
        MappingOutcome::Denied
    );
}

#[tokio::test]
async fn must_deny_instead_of_falling_back_to_anything_implicit() {
    let env = env();
    assert_eq!(
        outcome(&env, IdpProvider::Oidc, &[]).await,
        MappingOutcome::Denied
    );
    assert_eq!(
        outcome(&env, IdpProvider::Oidc, &["/unmapped"]).await,
        MappingOutcome::Denied
    );
}

#[tokio::test]
async fn must_not_use_a_privileged_missing_or_anonymous_default_role() {
    for default in [ROLE_NX_ADMIN, ROLE_NX_ANONYMOUS, "does-not-exist"] {
        let mut cfg = config();
        cfg.oidc.default_role = Some(default.to_owned());
        let env = env_with(cfg);
        let result = outcome(&env, IdpProvider::Oidc, &["/unmapped"]).await;
        assert_eq!(result, MappingOutcome::Denied, "default_role = {default}");
    }
}

#[tokio::test]
async fn roles_are_deduplicated_and_follow_priority() {
    let env = env();
    let low = add_role(&env.store, "low").await;
    let high = add_role(&env.store, "high").await;
    map_group(&env.store, IdpProvider::Oidc, "/g/a", &low, 20).await;
    map_group(&env.store, IdpProvider::Oidc, "/g/b", &high, 10).await;
    map_group(&env.store, IdpProvider::Oidc, "/g/c", &high, 30).await;
    let roles = granted(&env, IdpProvider::Oidc, &["/g/c", "/g/a", "/g/b"]).await;
    assert_eq!(names(&roles), ["high", "low"]);
    assert_eq!(
        roles.matched_groups,
        ["/g/c", "/g/a", "/g/b"],
        "input order is kept"
    );
}

#[tokio::test]
async fn mapping_rows_of_the_other_provider_do_not_apply() {
    let env = env();
    let readers = add_role(&env.store, "readers").await;
    map_group(
        &env.store,
        IdpProvider::Ldap,
        "/rustryx/readers",
        &readers,
        0,
    )
    .await;
    assert_eq!(
        outcome(&env, IdpProvider::Oidc, &["/rustryx/readers"]).await,
        MappingOutcome::Denied
    );
}

#[tokio::test]
async fn mapping_to_a_deleted_role_is_skipped() {
    let env = env();
    let gone = add_role(&env.store, "gone").await;
    map_group(&env.store, IdpProvider::Oidc, "/g/x", &gone, 0).await;
    RoleRepo::delete(&env.store, gone.id).await.unwrap();
    assert_eq!(
        outcome(&env, IdpProvider::Oidc, &["/g/x"]).await,
        MappingOutcome::Denied
    );
}

#[tokio::test]
async fn tables_are_cached_until_invalidated() {
    let env = env();
    let readers = add_role(&env.store, "readers").await;
    assert_eq!(
        outcome(&env, IdpProvider::Oidc, &["/g/new"]).await,
        MappingOutcome::Denied
    );
    map_group(&env.store, IdpProvider::Oidc, "/g/new", &readers, 0).await;
    assert_eq!(
        outcome(&env, IdpProvider::Oidc, &["/g/new"]).await,
        MappingOutcome::Denied,
        "the loaded table is reused inside its TTL"
    );
    env.mapper.invalidate();
    assert_eq!(
        names(&granted(&env, IdpProvider::Oidc, &["/g/new"]).await),
        ["readers"]
    );
}

#[tokio::test]
async fn must_not_report_a_store_outage_as_denied_or_as_no_groups() {
    let env = env();
    env.store.set_unavailable(true);
    let error = env
        .mapper
        .map_groups(IdpProvider::Oidc, &groups(&[ADMIN_PATH]))
        .await
        .unwrap_err();
    assert!(
        matches!(error, AuthError::ProviderUnavailable(_)),
        "{error:?}"
    );
}

#[tokio::test]
async fn must_refuse_an_absurd_number_of_groups_instead_of_truncating() {
    let env = env();
    let many: Vec<String> = (0..=MAX_EXTERNAL_GROUPS)
        .map(|i| format!("/g/{i}"))
        .collect();
    let error = env
        .mapper
        .map_groups(IdpProvider::Oidc, &many)
        .await
        .unwrap_err();
    assert!(
        matches!(error, AuthError::GroupLookupFailed(_)),
        "{error:?}"
    );
}

#[tokio::test]
async fn missing_builtin_admin_role_fails_closed() {
    let store = MemStore::new();
    let mapper = mapper_over(&store, config());
    let error = mapper
        .map_groups(IdpProvider::Oidc, &groups(&[ADMIN_PATH]))
        .await
        .unwrap_err();
    assert!(
        matches!(
            error,
            AuthError::Backend {
                context: "role mapping",
                ..
            }
        ),
        "{error:?}"
    );
}

fn role_ref(name: &str) -> RoleRef {
    RoleRef {
        id: RoleId::generate(),
        name: name.to_owned(),
    }
}

#[tokio::test]
async fn manual_roles_are_ignored_unless_supplement_is_on() {
    let env = env();
    let external = vec![role_ref("readers")];
    let manual = vec![role_ref("extra")];
    assert_eq!(env.mapper.compose(&external, &manual), external);
    assert!(!env.mapper.allows_local_role_supplement());

    let supplement = env_with(RoleMapperConfig {
        allow_local_role_supplement: true,
        ..config()
    });
    assert!(supplement.mapper.allows_local_role_supplement());
    let combined = supplement.mapper.compose(&external, &manual);
    assert_eq!(combined.len(), 2);
    assert_eq!(combined[1].name, "extra");
}

#[tokio::test]
async fn must_not_supplement_nx_admin_from_manual_rows_for_external_users() {
    let supplement = env_with(RoleMapperConfig {
        allow_local_role_supplement: true,
        ..config()
    });
    let external = vec![role_ref("readers")];
    let manual = vec![role_ref(ROLE_NX_ADMIN), role_ref("extra")];
    let combined = supplement.mapper.compose(&external, &manual);
    assert!(combined.iter().all(|r| r.name != ROLE_NX_ADMIN));
    assert_eq!(combined.len(), 2);
}

#[tokio::test]
async fn compose_removes_duplicates() {
    let supplement = env_with(RoleMapperConfig {
        allow_local_role_supplement: true,
        ..config()
    });
    let shared = role_ref("readers");
    let combined = supplement
        .mapper
        .compose(std::slice::from_ref(&shared), std::slice::from_ref(&shared));
    assert_eq!(combined, [shared]);
}

#[test]
fn group_names_for_auto_mapping() {
    assert_eq!(group_name(IdpProvider::Oidc, "/a/b/c"), Some("c"));
    assert_eq!(group_name(IdpProvider::Oidc, "/a/b/c/"), Some("c"));
    assert_eq!(group_name(IdpProvider::Oidc, "c"), Some("c"));
    assert_eq!(group_name(IdpProvider::Oidc, "/"), None);
    assert_eq!(group_name(IdpProvider::Oidc, ""), None);
    assert_eq!(
        group_name(IdpProvider::Ldap, "cn=Foo,ou=g,dc=x"),
        Some("Foo")
    );
    assert_eq!(group_name(IdpProvider::Ldap, "cn=,ou=g"), None);
    assert_eq!(group_name(IdpProvider::Ldap, "no-equals"), None);
    assert_eq!(group_name(IdpProvider::Ldap, "cn=a\\,b,ou=g"), None);
    assert_eq!(group_name(IdpProvider::Ldap, "cn=a+uid=b,ou=g"), None);
}

#[test]
fn debug_output_shows_settings_not_tables() {
    let store = MemStore::new();
    let rendered = format!("{:?}", mapper_over(&store, config()));
    assert!(rendered.contains("allow_local_role_supplement"));
    assert!(!rendered.contains(ADMIN_PATH));
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(48))]

    #[test]
    fn nx_admin_is_granted_exactly_when_the_admin_group_is_present(
        noise in proptest::collection::vec("[/a-dmins2A-D \\n]{0,24}", 0..8),
        include_exact in any::<bool>(),
    ) {
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        runtime.block_on(async {
            let env = env();
            let mut input = noise.clone();
            if include_exact {
                input.push(ADMIN_PATH.to_owned());
            }
            let exact_present = input.iter().any(|g| g == ADMIN_PATH);
            let result = env.mapper.map_groups(IdpProvider::Oidc, &input).await.unwrap();
            let has_admin = matches!(
                &result,
                MappingOutcome::Granted(r) if r.roles.iter().any(|role| role.name == ROLE_NX_ADMIN)
            );
            prop_assert_eq!(has_admin, exact_present);
            Ok(())
        })?;
    }
}
