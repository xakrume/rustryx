//! Tests of account provisioning: identity rules first, persistence second.

use std::time::Duration;

use rustryx_core::identity::{AuthMethod, Role};
use rustryx_core::ids::RoleId;
use rustryx_testkit::{FixedClock, MemStore, fixtures};

use super::*;

const ISSUER: &str = "https://idp.example.test/realms/rustryx";

struct Env {
    store: MemStore,
    clock: FixedClock,
    jit: JitProvisioner,
}

fn env_with(config: JitConfig) -> Env {
    let store = MemStore::with_builtin_roles();
    let clock = FixedClock::default();
    let jit = JitProvisioner::new(Arc::new(store.clone()), Arc::new(clock.clone()), config);
    Env { store, clock, jit }
}

fn env() -> Env {
    env_with(JitConfig::default())
}

fn oidc(username: &str, issuer: &str, subject: &str) -> Principal {
    let mut principal = Principal::new(username, UserSource::Oidc, AuthMethod::OidcBearer);
    principal.issuer = Some(issuer.to_owned());
    principal.subject = Some(subject.to_owned());
    principal
}

fn ldap(username: &str) -> Principal {
    Principal::new(username, UserSource::Ldap, AuthMethod::BasicLdap)
}

async fn user_count(store: &MemStore) -> usize {
    UserRepo::list(store, &rustryx_core::ports::PageRequest::default())
        .await
        .unwrap()
        .items
        .len()
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
    rustryx_core::ports::RoleRepo::create(store, &role)
        .await
        .unwrap();
    RoleRef {
        id: role.id,
        name: role.name,
    }
}

#[tokio::test]
async fn jit_creates_an_oidc_account_once_and_finds_it_by_issuer_and_subject() {
    let env = env();
    let first = env
        .jit
        .provision(&oidc("carol", ISSUER, "sub-1"))
        .await
        .unwrap();
    assert_eq!(first.source, UserSource::Oidc);
    assert_eq!(first.issuer.as_deref(), Some(ISSUER));
    assert_eq!(first.subject.as_deref(), Some("sub-1"));
    assert!(first.password_hash.is_none());
    let second = env
        .jit
        .provision(&oidc("carol", ISSUER, "sub-1"))
        .await
        .unwrap();
    assert_eq!(second.id, first.id);
    // A renamed account in the IdP is still the same account here.
    let renamed = env
        .jit
        .provision(&oidc("carol-renamed", ISSUER, "sub-1"))
        .await
        .unwrap();
    assert_eq!(renamed.id, first.id);
    assert_eq!(renamed.username, "carol");
    assert_eq!(user_count(&env.store).await, 1);
}

#[tokio::test]
async fn must_not_link_an_oidc_login_to_an_existing_username() {
    let env = env();
    let local_admin = UserRepo::create(&env.store, &fixtures::user("root", UserSource::Local))
        .await
        .unwrap();
    let error = env
        .jit
        .provision(&oidc("root", ISSUER, "sub-evil"))
        .await
        .unwrap_err();
    assert!(matches!(error, AuthError::UsernameConflict), "{error:?}");
    assert_eq!(
        user_count(&env.store).await,
        1,
        "no account was created or merged"
    );
    let untouched = UserRepo::get(&env.store, local_admin.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(untouched.source, UserSource::Local);
    assert!(untouched.subject.is_none());
}

#[tokio::test]
async fn must_not_link_an_oidc_login_to_a_local_account_by_email() {
    // The local administrator and the new SSO identity share an e-mail address. The
    // address is informational: it must neither select an account nor be copied
    // into the new one.
    let env = env();
    let mut new = fixtures::user("root", UserSource::Local);
    new.email = Some("shared@example.test".to_owned());
    let root = UserRepo::create(&env.store, &new).await.unwrap();
    let sso = env
        .jit
        .provision(&oidc("root-sso", ISSUER, "sub-root"))
        .await
        .unwrap();
    assert_ne!(sso.id, root.id);
    assert_eq!(sso.source, UserSource::Oidc);
    assert_eq!(sso.email, None);
    let root_after = UserRepo::get(&env.store, root.id).await.unwrap().unwrap();
    assert_eq!(root_after.source, UserSource::Local);
    assert_eq!(root_after.subject, None);
    assert_eq!(user_count(&env.store).await, 2);
}

#[tokio::test]
async fn must_not_link_by_username_between_oidc_identities() {
    let env = env();
    let alice = env
        .jit
        .provision(&oidc("alice", ISSUER, "sub-1"))
        .await
        .unwrap();
    // Same name, other subject: a clash, not alice.
    let error = env
        .jit
        .provision(&oidc("alice", ISSUER, "sub-2"))
        .await
        .unwrap_err();
    assert!(matches!(error, AuthError::UsernameConflict), "{error:?}");
    // Same name and subject but another issuer: another realm, another identity.
    let error = env
        .jit
        .provision(&oidc(
            "alice",
            "https://other.example.test/realms/x",
            "sub-1",
        ))
        .await
        .unwrap_err();
    assert!(matches!(error, AuthError::UsernameConflict), "{error:?}");
    // Same subject in another issuer with a free name: a new, separate account.
    let other = env
        .jit
        .provision(&oidc(
            "alice2",
            "https://other.example.test/realms/x",
            "sub-1",
        ))
        .await
        .unwrap();
    assert_ne!(other.id, alice.id);
    assert_eq!(user_count(&env.store).await, 2);
}

#[tokio::test]
async fn must_not_take_over_accounts_of_other_sources_by_name() {
    let env = env();
    UserRepo::create(&env.store, &fixtures::user("dave", UserSource::Ldap))
        .await
        .unwrap();
    let error = env
        .jit
        .provision(&oidc("dave", ISSUER, "sub-d"))
        .await
        .unwrap_err();
    assert!(matches!(error, AuthError::UsernameConflict), "{error:?}");
    UserRepo::create(&env.store, &fixtures::user("erin", UserSource::Local))
        .await
        .unwrap();
    let error = env.jit.provision(&ldap("erin")).await.unwrap_err();
    assert!(matches!(error, AuthError::UsernameConflict), "{error:?}");
    UserRepo::create(&env.store, &fixtures::user("frank", UserSource::Oidc))
        .await
        .unwrap();
    let error = env.jit.provision(&ldap("frank")).await.unwrap_err();
    assert!(matches!(error, AuthError::UsernameConflict), "{error:?}");
}

#[tokio::test]
async fn concurrent_first_logins_of_one_identity_yield_one_account() {
    let env = Arc::new(env());
    let tasks: Vec<_> = (0..8)
        .map(|_| {
            let env = Arc::clone(&env);
            tokio::spawn(async move { env.jit.provision(&oidc("grace", ISSUER, "sub-g")).await })
        })
        .collect();
    let mut ids = HashSet::new();
    for task in tasks {
        ids.insert(task.await.unwrap().unwrap().id);
    }
    assert_eq!(ids.len(), 1);
    assert_eq!(user_count(&env.store).await, 1);
}

#[tokio::test]
async fn manual_mode_never_creates_accounts() {
    let env = env_with(JitConfig {
        mode: JitMode::Manual,
        ..JitConfig::default()
    });
    let error = env
        .jit
        .provision(&oidc("heidi", ISSUER, "sub-h"))
        .await
        .unwrap_err();
    assert!(matches!(error, AuthError::AccessDenied(_)), "{error:?}");
    let error = env.jit.provision(&ldap("heidi")).await.unwrap_err();
    assert!(matches!(error, AuthError::AccessDenied(_)), "{error:?}");
    assert_eq!(user_count(&env.store).await, 0);

    // A pre-provisioned identity still logs in.
    let existing = UserRepo::create(&env.store, &fixtures::user("ivan", UserSource::Oidc))
        .await
        .unwrap();
    let found = env
        .jit
        .provision(&oidc("ivan", ISSUER_OF_FIXTURES, "sub-ivan"))
        .await
        .unwrap();
    assert_eq!(found.id, existing.id);
}

const ISSUER_OF_FIXTURES: &str = fixtures::OIDC_ISSUER;

#[tokio::test]
async fn allowlist_mode_creates_only_listed_usernames_exactly() {
    let env = env_with(JitConfig {
        mode: JitMode::Allowlist,
        allowlist: HashSet::from(["judy".to_owned()]),
        ..JitConfig::default()
    });
    for name in ["Judy", "judy ", "jud", "judy2"] {
        let error = env
            .jit
            .provision(&oidc(name, ISSUER, name))
            .await
            .unwrap_err();
        assert!(
            matches!(error, AuthError::AccessDenied(_)),
            "{name:?}: {error:?}"
        );
    }
    assert_eq!(user_count(&env.store).await, 0);
    env.jit
        .provision(&oidc("judy", ISSUER, "sub-j"))
        .await
        .unwrap();
    assert_eq!(user_count(&env.store).await, 1);
}

#[tokio::test]
async fn a_new_account_stores_the_email_and_the_name_the_principal_carries() {
    let env = env();
    let mut principal = oidc("nora", ISSUER, "sub-n");
    principal.email = Some("nora@example.test".to_owned());
    principal.display_name = Some("Nora Quinn".to_owned());
    let user = env.jit.provision(&principal).await.unwrap();
    assert_eq!(user.email.as_deref(), Some("nora@example.test"));
    assert_eq!(user.display_name.as_deref(), Some("Nora Quinn"));

    let mut directory_user = ldap("omar");
    directory_user.email = Some("omar@example.test".to_owned());
    let user = env.jit.provision(&directory_user).await.unwrap();
    assert_eq!(user.email.as_deref(), Some("omar@example.test"));
    assert!(user.display_name.is_none());
}

#[tokio::test]
async fn must_not_change_an_existing_account_from_the_claims_of_a_later_login() {
    // The profile of an existing account is not rewritten at login: the e-mail of a token is
    // informational and must never become a way to edit an account.
    let env = env();
    let mut first = oidc("pia", ISSUER, "sub-p");
    first.email = Some("pia@example.test".to_owned());
    let created = env.jit.provision(&first).await.unwrap();
    let mut later = oidc("pia", ISSUER, "sub-p");
    later.email = Some("someone.else@example.test".to_owned());
    let again = env.jit.provision(&later).await.unwrap();
    assert_eq!(again.id, created.id);
    assert_eq!(again.email.as_deref(), Some("pia@example.test"));
}

#[tokio::test]
async fn must_not_admit_a_disabled_account() {
    let env = env();
    let user = env
        .jit
        .provision(&oidc("ken", ISSUER, "sub-k"))
        .await
        .unwrap();
    UserRepo::set_enabled(&env.store, user.id, false)
        .await
        .unwrap();
    let error = env
        .jit
        .provision(&oidc("ken", ISSUER, "sub-k"))
        .await
        .unwrap_err();
    assert!(matches!(error, AuthError::AccountDisabled), "{error:?}");

    let ldap_user = env.jit.provision(&ldap("leo")).await.unwrap();
    UserRepo::set_enabled(&env.store, ldap_user.id, false)
        .await
        .unwrap();
    let error = env.jit.provision(&ldap("leo")).await.unwrap_err();
    assert!(matches!(error, AuthError::AccountDisabled), "{error:?}");
}

#[tokio::test]
async fn rejects_incomplete_or_malformed_identities() {
    let env = env();
    let mut no_issuer = oidc("mia", ISSUER, "sub-m");
    no_issuer.issuer = None;
    let mut empty_subject = oidc("mia", ISSUER, "");
    empty_subject.subject = Some(String::new());
    for principal in [
        no_issuer,
        empty_subject,
        oidc("", ISSUER, "s"),
        oidc("   ", ISSUER, "s"),
        oidc("bad\nname", ISSUER, "s"),
        oidc(&"x".repeat(257), ISSUER, "s"),
        ldap(""),
        ldap("tab\tname"),
    ] {
        let error = env.jit.provision(&principal).await.unwrap_err();
        assert!(
            matches!(error, AuthError::InvalidToken(_)),
            "{principal:?}: {error:?}"
        );
    }
    assert_eq!(user_count(&env.store).await, 0);
}

#[tokio::test]
async fn ldap_accounts_are_created_once_by_username() {
    let env = env();
    let first = env.jit.provision(&ldap("nina")).await.unwrap();
    assert_eq!(first.source, UserSource::Ldap);
    assert!(first.issuer.is_none() && first.subject.is_none());
    let second = env.jit.provision(&ldap("nina")).await.unwrap();
    assert_eq!(second.id, first.id);
}

#[tokio::test]
async fn local_principals_are_never_provisioned() {
    let env = env();
    let mut local = oidc("oscar", ISSUER, "s");
    local.source = UserSource::Local;
    let error = env.jit.provision(&local).await.unwrap_err();
    assert!(
        matches!(
            error,
            AuthError::Backend {
                context: "jit provisioning",
                ..
            }
        ),
        "{error:?}"
    );
}

#[tokio::test]
async fn store_outage_is_reported_not_swallowed() {
    let env = env();
    env.store.set_unavailable(true);
    for principal in [oidc("pat", ISSUER, "s"), ldap("pat")] {
        let error = env.jit.provision(&principal).await.unwrap_err();
        assert!(
            matches!(error, AuthError::ProviderUnavailable(_)),
            "{error:?}"
        );
    }
}

async fn stored(env: &Env, user: UserId) -> Vec<(String, RoleOrigin)> {
    UserRepo::roles_of(&env.store, user)
        .await
        .unwrap()
        .into_iter()
        .map(|(role, origin)| (role.name, origin))
        .collect()
}

#[tokio::test]
async fn external_roles_are_persisted_at_most_once_per_interval() {
    let env = env();
    let user = env
        .jit
        .provision(&oidc("quinn", ISSUER, "s"))
        .await
        .unwrap();
    let first = add_role(&env.store, "first").await;
    let second = add_role(&env.store, "second").await;

    env.jit
        .persist_external_roles(user.id, IdpProvider::Oidc, std::slice::from_ref(&first))
        .await;
    assert_eq!(
        stored(&env, user.id).await,
        [("first".to_owned(), RoleOrigin::Oidc)]
    );

    env.clock.advance(Duration::from_secs(299));
    env.jit
        .persist_external_roles(user.id, IdpProvider::Oidc, std::slice::from_ref(&second))
        .await;
    assert_eq!(
        stored(&env, user.id).await,
        [("first".to_owned(), RoleOrigin::Oidc)],
        "inside the interval the table is not rewritten"
    );

    env.clock.advance(Duration::from_secs(1));
    env.jit
        .persist_external_roles(user.id, IdpProvider::Oidc, &[second])
        .await;
    assert_eq!(
        stored(&env, user.id).await,
        [("second".to_owned(), RoleOrigin::Oidc)]
    );
}

#[tokio::test]
async fn persisting_replaces_only_its_own_origin() {
    let env = env();
    let user = env.jit.provision(&oidc("rita", ISSUER, "s")).await.unwrap();
    let manual = add_role(&env.store, "manual-role").await;
    let ldap_role = add_role(&env.store, "ldap-role").await;
    let oidc_role = add_role(&env.store, "oidc-role").await;
    UserRepo::replace_roles_for_origin(&env.store, user.id, RoleOrigin::Manual, &[manual.id])
        .await
        .unwrap();
    UserRepo::replace_roles_for_origin(&env.store, user.id, RoleOrigin::Ldap, &[ldap_role.id])
        .await
        .unwrap();
    env.jit
        .persist_external_roles(user.id, IdpProvider::Oidc, &[oidc_role])
        .await;
    let rows = stored(&env, user.id).await;
    assert!(rows.contains(&("manual-role".to_owned(), RoleOrigin::Manual)));
    assert!(rows.contains(&("ldap-role".to_owned(), RoleOrigin::Ldap)));
    assert!(rows.contains(&("oidc-role".to_owned(), RoleOrigin::Oidc)));
    let manual_only = env.jit.manual_roles(user.id).await.unwrap();
    assert_eq!(manual_only, [manual]);
}

#[tokio::test]
async fn an_empty_role_set_removes_the_rows_of_that_origin() {
    let env = env();
    let user = env.jit.provision(&oidc("sam", ISSUER, "s")).await.unwrap();
    let role = add_role(&env.store, "temp").await;
    env.jit
        .persist_external_roles(user.id, IdpProvider::Oidc, &[role])
        .await;
    assert_eq!(stored(&env, user.id).await.len(), 1);
    env.clock.advance(Duration::from_secs(300));
    env.jit
        .persist_external_roles(user.id, IdpProvider::Oidc, &[])
        .await;
    assert_eq!(stored(&env, user.id).await.len(), 0);
}

#[tokio::test]
async fn a_failed_persist_does_not_panic_and_is_retried_at_once() {
    let env = env();
    let user = env.jit.provision(&oidc("tina", ISSUER, "s")).await.unwrap();
    let role = add_role(&env.store, "late").await;
    env.store.set_unavailable(true);
    env.jit
        .persist_external_roles(user.id, IdpProvider::Oidc, std::slice::from_ref(&role))
        .await;
    env.store.set_unavailable(false);
    // No clock advance: the failed attempt must not have consumed the interval.
    env.jit
        .persist_external_roles(user.id, IdpProvider::Oidc, &[role])
        .await;
    assert_eq!(
        stored(&env, user.id).await,
        [("late".to_owned(), RoleOrigin::Oidc)]
    );
}

#[test]
fn debug_shows_mode_only() {
    let rendered = format!("{:?}", env().jit);
    assert!(rendered.contains("Jit"));
}
