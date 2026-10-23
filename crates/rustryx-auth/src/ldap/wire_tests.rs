//! Tests of the real `ldap3` transport against an in-process LDAP server over real TLS.
//!
//! These run in the default `cargo test`: they need no Docker, and they are where the
//! TLS rules (certificate AND host name verified, `StartTLS` never silently skipped, no
//! plaintext) are proven on the wire rather than assumed.

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use rustls::ClientConfig;
use rustryx_core::AuthError;
use rustryx_core::ports::{Authenticator, GroupResolver, SystemClock};

use super::config::{LdapConfig, LdapConfigError};
use super::net::NetTransport;
use super::service::{LdapInitError, LdapService};
use super::testing::{
    PEOPLE_DN, SERVICE_DN, SERVICE_PASSWORD, assert_empty, base_config, basic, group_dn, user_dn,
};
use super::testing_server::{
    MiniLdap, SearchReply, Security, WireEntry, WireSearch, simple_server,
};
use super::testing_tls::{TestPki, self_signed_server};
use super::tls::{self, TlsSetupError};
use super::transport::{LdapConnection, LdapTransport, SearchRequest, SearchScope, TransportError};

const TIMEOUT: Duration = Duration::from_secs(5);

fn client_tls(ca_pem: &str) -> Arc<ClientConfig> {
    let roots = tls::roots_from_pem(ca_pem.as_bytes(), Path::new("test-ca.pem")).unwrap();
    tls::config_from_roots(roots).unwrap()
}

fn alice() -> WireEntry {
    WireEntry {
        dn: user_dn("alice"),
        attributes: vec![
            ("uid".to_owned(), vec!["alice".to_owned()]),
            ("memberOf".to_owned(), vec![group_dn("devs")]),
        ],
    }
}

fn transport(
    server: &MiniLdap,
    scheme: &str,
    start_tls: bool,
    tls: Arc<ClientConfig>,
) -> NetTransport {
    NetTransport::new(
        &format!("{scheme}://127.0.0.1:{}", server.port()),
        start_tls,
        tls,
        TIMEOUT,
    )
    .unwrap()
}

async fn connect(transport: &NetTransport) -> Result<Box<dyn LdapConnection>, TransportError> {
    transport.connect().await
}

fn user_search() -> SearchRequest {
    SearchRequest {
        base: PEOPLE_DN.to_owned(),
        scope: SearchScope::Subtree,
        filter: "(uid=alice)".to_owned(),
        attributes: vec!["uid".to_owned(), "memberOf".to_owned()],
        size_limit: Some(2),
    }
}

async fn wait_for(condition: impl Fn() -> bool) {
    for _ in 0..100 {
        if condition() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

#[tokio::test]
async fn ldaps_with_a_trusted_ca_connects_binds_and_searches() {
    let pki = TestPki::new();
    let server = simple_server(
        Security::Ldaps(pki.server(&["127.0.0.1"]).server_config()),
        vec![alice()],
    )
    .await;
    let transport = transport(&server, "ldaps", false, client_tls(&pki.ca_pem()));
    let mut conn = connect(&transport).await.unwrap();

    conn.simple_bind(&user_dn("alice"), "good").await.unwrap();
    let response = conn.search(&user_search()).await.unwrap();
    assert!(!response.size_limit_exceeded);
    assert_eq!(response.entries.len(), 1);
    let entry = &response.entries[0];
    assert_eq!(entry.dn, user_dn("alice"));
    assert_eq!(entry.first_value("uid"), Some("alice"));
    assert_eq!(
        entry.first_value("MEMBEROF"),
        Some(group_dn("devs").as_str())
    );

    let searches = server.searches();
    assert_eq!(
        searches,
        vec![WireSearch {
            base: PEOPLE_DN.to_owned(),
            scope: 2,
            size_limit: 2,
            filter: "(uid=alice)".to_owned(),
            attributes: vec!["uid".to_owned(), "memberOf".to_owned()],
        }]
    );
    conn.close().await;
    wait_for(|| server.log.unbinds.load(Ordering::SeqCst) == 1).await;
    assert_eq!(
        server.log.unbinds.load(Ordering::SeqCst),
        1,
        "close sends an unbind"
    );
}

#[tokio::test]
async fn must_not_connect_when_the_certificate_is_not_trusted() {
    let server_pki = TestPki::new();
    let other_pki = TestPki::new();
    let server = simple_server(
        Security::Ldaps(server_pki.server(&["127.0.0.1"]).server_config()),
        vec![alice()],
    )
    .await;
    let transport = transport(&server, "ldaps", false, client_tls(&other_pki.ca_pem()));
    let error = connect(&transport)
        .await
        .err()
        .expect("an untrusted certificate must fail");
    assert!(matches!(error, TransportError::Connect(_)), "{error:?}");
    wait_for(|| server.log.handshake_failures.load(Ordering::SeqCst) >= 1).await;
    assert_empty(&server.binds(), "binds");
}

#[tokio::test]
async fn must_not_connect_to_a_self_signed_certificate_nobody_vouches_for() {
    let pki = TestPki::new();
    let server = simple_server(
        Security::Ldaps(self_signed_server(&["127.0.0.1"]).server_config()),
        vec![alice()],
    )
    .await;
    let transport = transport(&server, "ldaps", false, client_tls(&pki.ca_pem()));
    let error = connect(&transport)
        .await
        .err()
        .expect("a self signed certificate must fail");
    assert!(matches!(error, TransportError::Connect(_)), "{error:?}");
}

#[tokio::test]
async fn must_not_connect_when_the_host_name_does_not_match_the_certificate() {
    let pki = TestPki::new();
    let server = simple_server(
        Security::Ldaps(pki.server(&["ldap.other.example"]).server_config()),
        vec![alice()],
    )
    .await;
    let transport = transport(&server, "ldaps", false, client_tls(&pki.ca_pem()));
    let error = connect(&transport)
        .await
        .err()
        .expect("a certificate for another host name must fail even though the CA is trusted");
    assert!(matches!(error, TransportError::Connect(_)), "{error:?}");
}

#[tokio::test]
async fn starttls_upgrades_the_connection_before_any_bind() {
    let pki = TestPki::new();
    let server = simple_server(
        Security::StartTls(pki.server(&["127.0.0.1"]).server_config()),
        vec![alice()],
    )
    .await;
    let transport = transport(&server, "ldap", true, client_tls(&pki.ca_pem()));
    let mut conn = connect(&transport).await.unwrap();
    assert_eq!(server.log.start_tls_requests.load(Ordering::SeqCst), 1);
    conn.simple_bind(&user_dn("alice"), "good").await.unwrap();
    assert_eq!(
        server.binds().len(),
        1,
        "the bind went through the TLS session"
    );
    assert_eq!(server.log.handshake_failures.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn must_not_continue_in_plaintext_when_starttls_is_refused() {
    let server = simple_server(Security::StartTlsRefused(2), vec![alice()]).await;
    let pki = TestPki::new();
    let transport = transport(&server, "ldap", true, client_tls(&pki.ca_pem()));
    let error = connect(&transport)
        .await
        .err()
        .expect("a refused StartTLS must fail the connect");
    assert!(matches!(error, TransportError::Connect(_)), "{error:?}");
    assert!(
        server.binds().is_empty(),
        "nothing may be sent after a refused StartTLS"
    );
    assert_empty(&server.searches(), "searches");
}

#[tokio::test]
async fn must_not_start_tls_when_the_certificate_after_starttls_is_not_trusted() {
    let server_pki = TestPki::new();
    let other_pki = TestPki::new();
    let server = simple_server(
        Security::StartTls(server_pki.server(&["127.0.0.1"]).server_config()),
        vec![alice()],
    )
    .await;
    let transport = transport(&server, "ldap", true, client_tls(&other_pki.ca_pem()));
    assert!(connect(&transport).await.is_err());
    assert_empty(&server.binds(), "binds");
}

#[test]
fn must_not_build_a_plaintext_transport() {
    let pki = TestPki::new();
    let tls = client_tls(&pki.ca_pem());
    let plain = NetTransport::new("ldap://127.0.0.1:389", false, Arc::clone(&tls), TIMEOUT);
    assert_eq!(plain.err(), Some(LdapConfigError::PlaintextNotAllowed));
    let both = NetTransport::new("ldaps://127.0.0.1:636", true, Arc::clone(&tls), TIMEOUT);
    assert_eq!(both.err(), Some(LdapConfigError::StartTlsWithLdaps));
    let other = NetTransport::new("http://127.0.0.1", false, tls, TIMEOUT);
    assert!(matches!(
        other.err(),
        Some(LdapConfigError::UnsupportedScheme(_))
    ));
}

#[tokio::test]
async fn must_not_send_a_bind_with_an_empty_password_or_an_empty_dn() {
    let pki = TestPki::new();
    let server = simple_server(
        Security::Ldaps(pki.server(&["127.0.0.1"]).server_config()),
        vec![alice()],
    )
    .await;
    let transport = transport(&server, "ldaps", false, client_tls(&pki.ca_pem()));
    let mut conn = connect(&transport).await.unwrap();
    let no_password = conn.simple_bind(&user_dn("alice"), "").await.unwrap_err();
    assert!(
        matches!(no_password, TransportError::Protocol(_)),
        "{no_password:?}"
    );
    let no_dn = conn.simple_bind("", "good").await.unwrap_err();
    assert!(matches!(no_dn, TransportError::Protocol(_)), "{no_dn:?}");
    assert!(
        server.binds().is_empty(),
        "nothing reached the wire: {:?}",
        server.binds()
    );
}

#[tokio::test]
async fn a_refused_bind_carries_the_result_code() {
    let pki = TestPki::new();
    let server = simple_server(
        Security::Ldaps(pki.server(&["127.0.0.1"]).server_config()),
        vec![alice()],
    )
    .await;
    let transport = transport(&server, "ldaps", false, client_tls(&pki.ca_pem()));
    let mut conn = connect(&transport).await.unwrap();
    let error = conn
        .simple_bind(&user_dn("alice"), "wrong")
        .await
        .unwrap_err();
    assert_eq!(error.result_code(), Some(49));
}

#[tokio::test]
async fn search_result_codes_are_mapped() {
    let pki = TestPki::new();
    let server = MiniLdap::start(
        Security::Ldaps(pki.server(&["127.0.0.1"]).server_config()),
        Arc::new(|_, _| 0),
        Arc::new(|search: &WireSearch| match search.base.as_str() {
            "limit" => SearchReply::Entries {
                entries: vec![alice()],
                rc: 4,
            },
            "denied" => SearchReply::Entries {
                entries: Vec::new(),
                rc: 50,
            },
            _ => SearchReply::Entries {
                entries: vec![alice()],
                rc: 0,
            },
        }),
    )
    .await;
    let transport = transport(&server, "ldaps", false, client_tls(&pki.ca_pem()));
    let mut conn = connect(&transport).await.unwrap();

    let mut request = user_search();
    request.base = "limit".into();
    let limited = conn.search(&request).await.unwrap();
    assert!(limited.size_limit_exceeded);
    assert_eq!(limited.entries.len(), 1);

    request.base = "denied".into();
    let denied = conn.search(&request).await.unwrap_err();
    assert_eq!(denied.result_code(), Some(50));

    request.base = "ok".into();
    request.size_limit = None;
    let ok = conn.search(&request).await.unwrap();
    assert!(!ok.size_limit_exceeded);
    assert!(
        conn.is_healthy().await,
        "a refusal still proves the connection is alive"
    );
}

#[tokio::test]
async fn every_search_scope_reaches_the_wire() {
    let pki = TestPki::new();
    let server = simple_server(
        Security::Ldaps(pki.server(&["127.0.0.1"]).server_config()),
        vec![alice()],
    )
    .await;
    let transport = transport(&server, "ldaps", false, client_tls(&pki.ca_pem()));
    let mut conn = connect(&transport).await.unwrap();
    for (scope, wire) in [
        (SearchScope::Base, 0),
        (SearchScope::OneLevel, 1),
        (SearchScope::Subtree, 2),
    ] {
        let mut request = user_search();
        request.scope = scope;
        conn.search(&request).await.unwrap();
        assert_eq!(server.searches().last().unwrap().scope, wire, "{scope:?}");
    }
}

#[tokio::test]
async fn a_malformed_entry_fails_the_search_instead_of_panicking() {
    let pki = TestPki::new();
    let server = MiniLdap::start(
        Security::Ldaps(pki.server(&["127.0.0.1"]).server_config()),
        Arc::new(|_, _| 0),
        Arc::new(|_| SearchReply::MalformedEntry),
    )
    .await;
    let transport = transport(&server, "ldaps", false, client_tls(&pki.ca_pem()));
    let mut conn = connect(&transport).await.unwrap();
    let error = conn.search(&user_search()).await.unwrap_err();
    assert!(
        matches!(error, TransportError::Protocol(ref m) if m.contains("malformed")),
        "{error:?}"
    );
}

#[tokio::test]
async fn a_dropped_connection_is_reported_as_unhealthy() {
    let pki = TestPki::new();
    let server = MiniLdap::start(
        Security::Ldaps(pki.server(&["127.0.0.1"]).server_config()),
        Arc::new(|_, _| 0),
        Arc::new(|search: &WireSearch| {
            if search.base == "hang-up" {
                SearchReply::Disconnect
            } else {
                SearchReply::Entries {
                    entries: Vec::new(),
                    rc: 0,
                }
            }
        }),
    )
    .await;
    let transport = transport(&server, "ldaps", false, client_tls(&pki.ca_pem()));
    let mut conn = connect(&transport).await.unwrap();
    assert!(conn.is_healthy().await);
    let mut request = user_search();
    request.base = "hang-up".into();
    assert!(conn.search(&request).await.is_err());
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(!conn.is_healthy().await);
    let again = conn.search(&user_search()).await;
    assert!(
        again.is_err(),
        "a dead connection must fail, not return an empty result"
    );
}

#[tokio::test]
async fn connecting_to_a_closed_port_fails_cleanly() {
    let pki = TestPki::new();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    let transport = NetTransport::new(
        &format!("ldaps://127.0.0.1:{port}"),
        false,
        client_tls(&pki.ca_pem()),
        TIMEOUT,
    )
    .unwrap();
    let error = connect(&transport)
        .await
        .err()
        .expect("nothing is listening");
    assert!(matches!(error, TransportError::Connect(_)), "{error:?}");
}

fn service_config(ca_file: &Path, port: u16) -> LdapConfig {
    let mut config = base_config();
    config.url = Some(format!("ldaps://127.0.0.1:{port}"));
    config.ca_file = Some(ca_file.to_path_buf());
    config.connect_timeout_secs = 5;
    config
}

fn directory_server_handlers() -> (
    super::testing_server::BindHandler,
    super::testing_server::SearchHandler,
) {
    let bind: super::testing_server::BindHandler = Arc::new(|dn, password| {
        let service = dn == SERVICE_DN && password == SERVICE_PASSWORD;
        let alice = dn == user_dn("alice") && password == "alice-pw";
        if service || alice { 0 } else { 49 }
    });
    let search: super::testing_server::SearchHandler = Arc::new(|search: &WireSearch| {
        let entries = if search.filter == "(uid=alice)" {
            vec![alice()]
        } else {
            Vec::new()
        };
        SearchReply::Entries { entries, rc: 0 }
    });
    (bind, search)
}

#[tokio::test]
async fn the_whole_stack_authenticates_over_ldaps_with_a_custom_ca_file() {
    let pki = TestPki::new();
    let dir = tempfile::tempdir().unwrap();
    let ca_file = dir.path().join("ca.pem");
    std::fs::write(&ca_file, pki.ca_pem()).unwrap();
    let (bind, search) = directory_server_handlers();
    let server = MiniLdap::start(
        Security::Ldaps(pki.server(&["127.0.0.1"]).server_config()),
        bind,
        search,
    )
    .await;
    let config = service_config(&ca_file, server.port());
    let service = LdapService::new(&config, Arc::new(SystemClock))
        .await
        .unwrap();
    assert!(service.is_enabled());
    let authenticator = super::authenticator::LdapAuthenticator::new(Arc::clone(&service));
    let resolver = super::resolver::LdapGroupResolver::new(Arc::clone(&service));

    let principal = authenticator
        .authenticate(
            &basic("alice", "alice-pw"),
            &rustryx_core::identity::AuthContext::default(),
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(principal.username, "alice");
    assert_eq!(principal.external_groups, vec![group_dn("devs")]);

    let binds = server.binds();
    assert_eq!(
        binds[0].0, SERVICE_DN,
        "the service account binds first, on the pooled connection"
    );
    assert_eq!(binds[1], (user_dn("alice"), "alice-pw".to_owned()));
    assert_eq!(
        server.log.connections.load(Ordering::SeqCst),
        2,
        "one pooled service connection and one throwaway user bind connection"
    );
    let groups = resolver.external_groups(&principal).await.unwrap();
    assert_eq!(groups.groups, vec![group_dn("devs")]);

    for (login, password) in [("alice", "wrong"), ("mallory", "alice-pw")] {
        let result = authenticator
            .authenticate(
                &basic(login, password),
                &rustryx_core::identity::AuthContext::default(),
            )
            .await;
        assert!(
            matches!(result, Err(AuthError::InvalidCredentials)),
            "{login}: {result:?}"
        );
    }
    assert!(
        server
            .binds()
            .iter()
            .all(|(_, password)| !password.is_empty()),
        "no empty password bind may reach the wire"
    );
}

#[tokio::test]
async fn must_not_authenticate_through_a_server_whose_certificate_the_ca_file_does_not_cover() {
    let pki = TestPki::new();
    let other = TestPki::new();
    let dir = tempfile::tempdir().unwrap();
    let ca_file = dir.path().join("ca.pem");
    std::fs::write(&ca_file, other.ca_pem()).unwrap();
    let (bind, search) = directory_server_handlers();
    let server = MiniLdap::start(
        Security::Ldaps(pki.server(&["127.0.0.1"]).server_config()),
        bind,
        search,
    )
    .await;
    let service = LdapService::new(
        &service_config(&ca_file, server.port()),
        Arc::new(SystemClock),
    )
    .await
    .unwrap();
    let authenticator = super::authenticator::LdapAuthenticator::new(service);
    let result = authenticator
        .authenticate(
            &basic("alice", "alice-pw"),
            &rustryx_core::identity::AuthContext::default(),
        )
        .await;
    assert!(
        matches!(result, Err(AuthError::ProviderUnavailable(_))),
        "{result:?}"
    );
    assert!(
        server.binds().is_empty(),
        "the service password must not be sent to an unverified server"
    );
}

#[tokio::test]
async fn the_service_refuses_to_start_on_a_plaintext_url_or_a_bad_ca_file() {
    let plain = LdapConfig {
        url: Some("ldap://127.0.0.1:389".into()),
        ..base_config()
    };
    let error = LdapService::new(&plain, Arc::new(SystemClock))
        .await
        .unwrap_err();
    assert!(
        matches!(
            error,
            LdapInitError::Config(LdapConfigError::PlaintextNotAllowed)
        ),
        "{error}"
    );

    let dir = tempfile::tempdir().unwrap();
    let missing = LdapConfig {
        ca_file: Some(dir.path().join("absent.pem")),
        ..base_config()
    };
    let error = LdapService::new(&missing, Arc::new(SystemClock))
        .await
        .unwrap_err();
    assert!(
        matches!(error, LdapInitError::Tls(TlsSetupError::ReadCaFile { .. })),
        "{error}"
    );

    let disabled = LdapConfig {
        enabled: false,
        ..base_config()
    };
    let service = LdapService::new(&disabled, Arc::new(SystemClock))
        .await
        .unwrap();
    assert!(!service.is_enabled());
}

#[tokio::test]
async fn the_platform_trust_store_is_used_when_no_ca_file_is_given() {
    // Whether this machine has a usable trust store is not up to the test: both outcomes
    // are valid, a panic or a hang is not.
    let result = tls::client_config(None).await;
    assert!(matches!(
        result,
        Ok(_) | Err(TlsSetupError::NoPlatformRoots)
    ));
}
