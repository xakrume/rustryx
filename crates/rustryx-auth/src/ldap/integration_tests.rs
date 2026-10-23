//! Integration test against a real `OpenLDAP` in Docker (README 8.6: one LDIF, shared
//! with `docker compose`).
//!
//! Off by default: it needs Docker and takes a while. Run it with
//!
//! ```text
//! RUSTRYX_IT=1 cargo test -p rustryx-auth --lib ldap::integration_tests -- --nocapture
//! ```
//!
//! The image is built from `deploy/compose/ldap` (the same Dockerfile compose uses), the
//! data is `deploy/compose/ldap/seed.ldif`, and TLS uses a CA and a server certificate
//! generated here with `rcgen` and copied into the container, so the client verifies a
//! real chain and host name. If the fixture files are missing the test says so and skips.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use ldap3::{LdapConnAsync, LdapConnSettings, Mod};
use rustls::ClientConfig;
use rustryx_core::AuthError;
use rustryx_core::Secret;
use rustryx_core::identity::{AuthContext, Principal};
use rustryx_core::ports::{Authenticator, Clock, GroupResolver};
use testcontainers::core::{IntoContainerPort, WaitFor};
use testcontainers::runners::{AsyncBuilder, AsyncRunner};
use testcontainers::{GenericBuildableImage, ImageExt};
use time::Duration as TimeDuration;

use super::authenticator::LdapAuthenticator;
use super::config::{LdapConfig, LdapGroupStrategy};
use super::resolver::{GroupFreshness, LdapGroupResolver};
use super::service::LdapService;
use super::testing::{FakeClock, T0, basic, ldap_principal};
use super::testing_tls::TestPki;
use super::tls;

const ADMIN_DN: &str = "cn=admin,dc=rustryx,dc=test";
const ADMIN_PASSWORD: &str = "it-admin-password";
const PEOPLE: &str = "ou=people,dc=rustryx,dc=test";
const GROUPS: &str = "ou=groups,dc=rustryx,dc=test";
const SERVICE_DN: &str = "uid=svc-rustryx,ou=services,dc=rustryx,dc=test";
const SPECIAL_LOGIN: &str = "star*(paren)\\back,comma+plus";

fn group(cn: &str) -> String {
    format!("cn={cn},{GROUPS}")
}

fn fixture_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../deploy/compose/ldap")
}

/// A running `OpenLDAP` and the client side material for it.
struct Fixture {
    ldaps_port: u16,
    starttls_port: u16,
    ca_file: PathBuf,
    ca_pem: String,
    _dir: tempfile::TempDir,
}

impl Fixture {
    fn config(&self) -> LdapConfig {
        LdapConfig {
            enabled: true,
            url: Some(format!("ldaps://127.0.0.1:{}", self.ldaps_port)),
            ca_file: Some(self.ca_file.clone()),
            bind_dn: Some(SERVICE_DN.into()),
            bind_password: Some(Secret::new("svc-rustryx-test-only")),
            user_base_dn: Some(PEOPLE.into()),
            ..LdapConfig::default()
        }
    }

    async fn stack(&self, config: &LdapConfig, clock: &Arc<FakeClock>) -> Stack {
        let service = LdapService::new(config, Arc::clone(clock) as Arc<dyn Clock>)
            .await
            .unwrap();
        Stack {
            auth: LdapAuthenticator::new(Arc::clone(&service)),
            resolver: LdapGroupResolver::new(Arc::clone(&service)),
            service,
        }
    }

    async fn admin_remove_member(&self, group_dn: &str, member_dn: &str) {
        let roots = tls::roots_from_pem(self.ca_pem.as_bytes(), Path::new("it-ca.pem")).unwrap();
        let tls: Arc<ClientConfig> = tls::config_from_roots(roots).unwrap();
        let settings = LdapConnSettings::new().set_config(tls);
        let url = format!("ldaps://127.0.0.1:{}", self.ldaps_port);
        let (driver, mut ldap) = LdapConnAsync::with_settings(settings, &url).await.unwrap();
        let driver = tokio::spawn(async move {
            let _ = driver.drive().await;
        });
        ldap.simple_bind(ADMIN_DN, ADMIN_PASSWORD)
            .await
            .unwrap()
            .success()
            .unwrap();
        ldap.modify(
            group_dn,
            vec![Mod::Delete("member", HashSet::from([member_dn]))],
        )
        .await
        .unwrap()
        .success()
        .unwrap();
        ldap.unbind().await.unwrap();
        driver.abort();
    }
}

struct Stack {
    service: Arc<LdapService>,
    auth: LdapAuthenticator,
    resolver: LdapGroupResolver,
}

impl Stack {
    async fn login(&self, user: &str, password: &str) -> Result<Option<Principal>, AuthError> {
        self.auth
            .authenticate(&basic(user, password), &AuthContext::default())
            .await
    }

    async fn groups(&self, user: &str) -> Result<Vec<String>, AuthError> {
        self.resolver
            .external_groups(&ldap_principal(user))
            .await
            .map(|external| external.groups)
    }
}

fn sorted(mut values: Vec<String>) -> Vec<String> {
    values.sort();
    values
}

#[tokio::test(flavor = "multi_thread")]
async fn openldap_with_the_shared_fixture() {
    if std::env::var("RUSTRYX_IT").as_deref() != Ok("1") {
        eprintln!("SKIPPED ldap integration test: set RUSTRYX_IT=1 to run it (needs Docker)");
        return;
    }
    let dir = fixture_dir();
    let seed = dir.join("seed.ldif");
    for required in [
        "Dockerfile",
        "entrypoint.sh",
        "gen-certs.sh",
        "slapd.conf.tmpl",
        "seed.ldif",
    ] {
        if !dir.join(required).exists() {
            eprintln!(
                "SKIPPED ldap integration test: {} is missing (deploy/compose/ldap is written by the devops agent)",
                dir.join(required).display()
            );
            return;
        }
    }

    let pki = TestPki::new();
    let identity = pki.server(&["localhost", "127.0.0.1", "ldap"]);
    let ca_pem = pki.ca_pem();

    let image = GenericBuildableImage::new("rustryx-ldap-fixture", "it")
        .with_dockerfile(dir.join("Dockerfile"))
        .with_file(dir.join("slapd.conf.tmpl"), "slapd.conf.tmpl")
        .with_file(dir.join("entrypoint.sh"), "entrypoint.sh")
        .with_file(dir.join("gen-certs.sh"), "gen-certs.sh")
        .build_image()
        .await
        .expect("building the OpenLDAP fixture image");

    let container = image
        .with_exposed_port(636.tcp())
        .with_exposed_port(389.tcp())
        .with_wait_for(WaitFor::message_on_stderr("slapd starting"))
        .with_startup_timeout(Duration::from_secs(180))
        .with_env_var("LDAP_ADMIN_PASSWORD", ADMIN_PASSWORD)
        .with_copy_to("/certs/ca.crt", ca_pem.clone().into_bytes())
        .with_copy_to("/certs/server.crt", identity.cert_pem.clone().into_bytes())
        .with_copy_to("/certs/server.key", identity.key_pem.clone().into_bytes())
        .with_copy_to("/seed/seed.ldif", seed.as_path())
        .start()
        .await
        .expect("starting the OpenLDAP fixture");

    let work = tempfile::tempdir().unwrap();
    let ca_file = work.path().join("ca.pem");
    std::fs::write(&ca_file, &ca_pem).unwrap();
    let fixture = Fixture {
        ldaps_port: container.get_host_port_ipv4(636.tcp()).await.unwrap(),
        starttls_port: container.get_host_port_ipv4(389.tcp()).await.unwrap(),
        ca_file,
        ca_pem,
        _dir: work,
    };
    let clock = FakeClock::new(T0);

    scenario_login_and_groups(&fixture, &clock).await;
    scenario_escaping_against_a_real_server(&fixture, &clock).await;
    scenario_nested_groups_and_cycles(&fixture, &clock).await;
    scenario_look_alike_admin_groups(&fixture, &clock).await;
    scenario_reverse_member(&fixture, &clock).await;
    scenario_starttls(&fixture, &clock).await;
    scenario_untrusted_ca(&fixture, &clock).await;
    scenario_membership_change(&fixture, &clock).await;
    scenario_outage_and_the_five_minute_rule(&fixture, &container).await;

    container.rm().await.expect("removing the OpenLDAP fixture");
}

async fn scenario_login_and_groups(fixture: &Fixture, clock: &Arc<FakeClock>) {
    let stack = fixture.stack(&fixture.config(), clock).await;
    let alice = stack
        .login("alice", "alice-test-only")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(alice.username, "alice", "login");
    assert_eq!(
        alice.external_groups,
        vec![group("rustryx-admins")],
        "alice groups"
    );

    let upper = stack
        .login("ALICE", "alice-test-only")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(upper.username, "alice", "the directory spelling is used");

    for (user, password) in [
        ("alice", "wrong"),
        ("alice", "bob-test-only"),
        ("nobody", "alice-test-only"),
        ("alice", ""),
        ("alice", "   "),
        ("alice", "alice-test-only\0"),
    ] {
        let result = stack.login(user, password).await;
        assert!(
            matches!(result, Err(AuthError::InvalidCredentials)),
            "{user}/{password:?}: {result:?}"
        );
    }
    let stats = stack.service.stats().snapshot();
    assert_eq!(stats.ambiguous_user_matches, 0);
    println!("ok   login, wrong password, unknown user, empty/blank/NUL password");
}

async fn scenario_escaping_against_a_real_server(fixture: &Fixture, clock: &Arc<FakeClock>) {
    let stack = fixture.stack(&fixture.config(), clock).await;
    let special = stack
        .login(SPECIAL_LOGIN, "special-test-only")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(special.username, SPECIAL_LOGIN);
    assert_eq!(
        special.external_groups,
        vec![group("readers")],
        "special user groups"
    );

    for attack in ["*", "*)(uid=*", "alice)(|(uid=*", "a*", "\\2a"] {
        let result = stack.login(attack, "alice-test-only").await;
        assert!(
            matches!(result, Err(AuthError::InvalidCredentials)),
            "{attack:?} must not match anybody: {result:?}"
        );
    }
    let wildcard_groups = stack.groups("*").await;
    assert!(
        matches!(wildcard_groups, Err(AuthError::GroupLookupFailed(_))),
        "{wildcard_groups:?}"
    );
    println!("ok   RFC 4515 escaping: special login works, injection strings match nobody");
}

async fn scenario_nested_groups_and_cycles(fixture: &Fixture, clock: &Arc<FakeClock>) {
    let stack = fixture.stack(&fixture.config(), clock).await;
    let carol = stack.groups("carol").await.unwrap();
    assert_eq!(
        sorted(carol.clone()),
        sorted(
            (1..=4)
                .map(|level| group(&format!("chain-l{level}")))
                .collect()
        ),
        "carol: four nested levels with the default depth"
    );
    assert_eq!(carol[0], group("chain-l1"), "nearest group first");

    let dave = stack.groups("dave").await.unwrap();
    assert_eq!(
        sorted(dave),
        sorted(vec![group("cycle-a"), group("cycle-b")]),
        "dave: the cycle terminates with each group once"
    );

    let mut shallow = fixture.config();
    shallow.nested_group_depth = 2;
    let shallow_stack = fixture.stack(&shallow, clock).await;
    assert_eq!(
        shallow_stack.groups("carol").await.unwrap(),
        vec![group("chain-l1"), group("chain-l2")],
        "depth limit 2"
    );
    assert_eq!(
        shallow_stack
            .service
            .stats()
            .snapshot()
            .group_depth_limit_hits,
        1
    );

    let mut capped = fixture.config();
    capped.max_groups = 3;
    let capped_stack = fixture.stack(&capped, clock).await;
    assert_eq!(
        capped_stack.groups("carol").await.unwrap().len(),
        3,
        "group count limit 3"
    );
    assert_eq!(
        capped_stack
            .service
            .stats()
            .snapshot()
            .group_size_limit_hits,
        1
    );
    println!("ok   nested chain, cycle, depth limit, group count limit");
}

async fn scenario_look_alike_admin_groups(fixture: &Fixture, clock: &Arc<FakeClock>) {
    let stack = fixture.stack(&fixture.config(), clock).await;
    let erin = stack.groups("erin").await.unwrap();
    assert_eq!(
        sorted(erin.clone()),
        sorted(vec![
            "cn=rustryx-admins,ou=decoys,dc=rustryx,dc=test".to_owned(),
            group("team-x-admins"),
        ])
    );
    assert!(
        !erin.contains(&group("rustryx-admins")),
        "a look-alike is a different DN and must not equal the admin group"
    );
    println!("ok   look-alike groups come back as their own DNs, unmodified");
}

async fn scenario_reverse_member(fixture: &Fixture, clock: &Arc<FakeClock>) {
    let mut config = fixture.config();
    config.group_strategy = LdapGroupStrategy::ReverseMember;
    config.group_base_dn = Some(GROUPS.into());
    let stack = fixture.stack(&config, clock).await;
    assert_eq!(
        sorted(stack.groups("carol").await.unwrap()),
        sorted(
            (1..=4)
                .map(|level| group(&format!("chain-l{level}")))
                .collect()
        )
    );
    assert_eq!(
        stack.groups("alice").await.unwrap(),
        vec![group("rustryx-admins")]
    );
    assert_eq!(
        sorted(stack.groups("dave").await.unwrap()),
        sorted(vec![group("cycle-a"), group("cycle-b")])
    );
    assert_eq!(
        stack.groups(SPECIAL_LOGIN).await.unwrap(),
        vec![group("readers")],
        "the user DN with RFC 4514 escapes goes through the group filter intact"
    );
    println!("ok   reverse_member strategy, nested, cycle, escaped user DN in the group filter");
}

async fn scenario_starttls(fixture: &Fixture, clock: &Arc<FakeClock>) {
    let mut config = fixture.config();
    config.url = Some(format!("ldap://127.0.0.1:{}", fixture.starttls_port));
    config.starttls = true;
    let stack = fixture.stack(&config, clock).await;
    let alice = stack
        .login("alice", "alice-test-only")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(alice.external_groups, vec![group("rustryx-admins")]);
    println!("ok   StartTLS on the plain port");
}

async fn scenario_untrusted_ca(fixture: &Fixture, clock: &Arc<FakeClock>) {
    let other = TestPki::new();
    let dir = tempfile::tempdir().unwrap();
    let wrong_ca = dir.path().join("wrong-ca.pem");
    std::fs::write(&wrong_ca, other.ca_pem()).unwrap();
    let mut config = fixture.config();
    config.ca_file = Some(wrong_ca);
    let stack = fixture.stack(&config, clock).await;
    let result = stack.login("alice", "alice-test-only").await;
    assert!(
        matches!(result, Err(AuthError::ProviderUnavailable(_))),
        "a server whose certificate the CA file does not cover must be refused: {result:?}"
    );
    println!("ok   a certificate that the configured CA does not vouch for is refused");
}

async fn scenario_membership_change(fixture: &Fixture, clock: &Arc<FakeClock>) {
    let mut config = fixture.config();
    config.groups_cache_ttl_secs = 60;
    let stack = fixture.stack(&config, clock).await;
    let bob = ldap_principal("bob");
    let before = stack
        .resolver
        .resolve_detailed(&bob)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(before.groups, vec![group("readers")]);

    fixture
        .admin_remove_member(&group("readers"), &format!("uid=bob,{PEOPLE}"))
        .await;

    clock.advance(TimeDuration::seconds(30));
    let cached = stack
        .resolver
        .resolve_detailed(&bob)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(cached.freshness, GroupFreshness::Cached);
    assert_eq!(
        cached.groups,
        vec![group("readers")],
        "within the cache TTL"
    );

    clock.advance(TimeDuration::seconds(31));
    let after = stack
        .resolver
        .resolve_detailed(&bob)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(after.freshness, GroupFreshness::Fresh);
    assert!(
        after.groups.is_empty(),
        "bob left readers: {:?}",
        after.groups
    );
    println!("ok   removing a user from a group takes effect after the cache TTL");
}

async fn scenario_outage_and_the_five_minute_rule(
    fixture: &Fixture,
    container: &testcontainers::ContainerAsync<testcontainers::GenericImage>,
) {
    // A clock of their own: earlier scenarios moved the shared one.
    let clock = FakeClock::new(T0);
    let strict = fixture.stack(&fixture.config(), &clock).await;
    let mut stale_config = fixture.config();
    stale_config.groups_stale_on_error = true;
    let stale = fixture.stack(&stale_config, &clock).await;

    let strict_alice = strict
        .login("alice", "alice-test-only")
        .await
        .unwrap()
        .unwrap();
    let stale_alice = stale
        .login("alice", "alice-test-only")
        .await
        .unwrap()
        .unwrap();
    let confirmed_at = stale_alice.groups_confirmed_at.unwrap();

    container.stop().await.expect("stopping the directory");

    clock.advance(TimeDuration::seconds(30));
    let cached = strict
        .resolver
        .resolve_detailed(&strict_alice)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        cached.freshness,
        GroupFreshness::Cached,
        "inside the TTL the directory is not asked"
    );

    clock.advance(TimeDuration::seconds(31));
    let refused = strict.resolver.external_groups(&strict_alice).await;
    assert!(
        matches!(refused, Err(AuthError::GroupLookupFailed(_))),
        "default mode: a failed lookup after the TTL is an error at once: {refused:?}"
    );

    let served = stale
        .resolver
        .resolve_detailed(&stale_alice)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(served.freshness, GroupFreshness::StaleWithinWindow);
    assert_eq!(
        served.confirmed_at, confirmed_at,
        "the time of the last search, not now"
    );
    assert_eq!(served.groups, vec![group("rustryx-admins")]);

    let login = stale.login("alice", "alice-test-only").await;
    assert!(
        matches!(login, Err(AuthError::ProviderUnavailable(_))),
        "{login:?}"
    );

    clock.set(confirmed_at + TimeDuration::seconds(300));
    assert!(
        stale.resolver.resolve_detailed(&stale_alice).await.is_ok(),
        "exactly five minutes"
    );
    clock.set(confirmed_at + TimeDuration::seconds(301));
    let expired = stale.resolver.external_groups(&stale_alice).await;
    assert!(
        matches!(expired, Err(AuthError::GroupLookupFailed(_))),
        "stale mode: five minutes after the last successful search the roles must go: {expired:?}"
    );
    let stats = stale.service.stats().snapshot();
    assert!(
        stats.groups_served_stale >= 1 && stats.groups_expired >= 1,
        "{stats:?}"
    );
    println!(
        "ok   directory outage: fail closed by default, bounded stale mode, never past 5 minutes"
    );
}
