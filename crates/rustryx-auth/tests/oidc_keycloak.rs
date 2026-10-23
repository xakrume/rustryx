//! OIDC against a real Keycloak started with testcontainers.
//!
//! Gating: the test runs only with `RUSTRYX_IT=1` (a missing Docker or image is then a failure);
//! without it, or when the realm file `deploy/compose/keycloak/realm-rustryx.json` is missing, it
//! prints why it was skipped and passes, so the default `cargo test` never needs Docker.
//!
//! One test drives every step against one container: the realm import dominates the time, and
//! the steps build on each other (token, login, group removal).
//!
//! What it proves that the in-process mock cannot: the discovery document, JWKS (including the
//! encryption key Keycloak publishes next to the signing key), token and ID token of a real
//! Keycloak 26 are accepted by the validator; the `groups` claim carries full paths; the
//! authorization code flow with PKCE and a confidential client works end to end; removing a
//! user from a group removes the group from the next token.
// Tests may unwrap, expect and panic: a panic is the failure signal there.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;

use rcgen::{BasicConstraints, CertificateParams, DnType, IsCa, Issuer, KeyPair};
use reqwest::header::{COOKIE, LOCATION, SET_COOKIE};
use reqwest::{Client, Response, StatusCode};
use rustryx_auth::oidc::{OidcConfig, OidcProvider, UiClientConfig};
use rustryx_core::identity::{AuthContext, AuthMethod, Credentials, UserSource};
use rustryx_core::ports::Authenticator;
use rustryx_core::{AuthError, Secret};
use serde_json::Value;
use testcontainers::core::{IntoContainerPort, WaitFor};
use testcontainers::runners::AsyncRunner;
use testcontainers::{CopyTargetOptions, GenericImage, ImageExt};
use url::Url;

/// The image pinned by `deploy/compose/services.yaml`.
const KEYCLOAK_IMAGE: &str = "quay.io/keycloak/keycloak";
const KEYCLOAK_TAG: &str = "26.8.1";
const REALM: &str = "rustryx";
const REDIRECT_URL: &str = "http://localhost:8081/api/v1/auth/oidc/callback";
const ADMIN_USER: &str = "admin";
const ADMIN_PASSWORD: &str = "admin-test-only";

/// The realm shared with docker compose; `RUSTRYX_KEYCLOAK_REALM` points elsewhere when the
/// crate is built outside the repository layout.
fn realm_file() -> PathBuf {
    std::env::var_os("RUSTRYX_KEYCLOAK_REALM").map_or_else(
        || {
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../../deploy/compose/keycloak/realm-rustryx.json")
        },
        PathBuf::from,
    )
}

/// A throwaway CA and a server certificate for `localhost`.
struct Tls {
    ca: String,
    chain: String,
    key: String,
}

fn make_tls() -> Tls {
    let ca_key = KeyPair::generate().unwrap();
    let mut ca_params = CertificateParams::new(Vec::<String>::new()).unwrap();
    ca_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    ca_params
        .distinguished_name
        .push(DnType::CommonName, "rustryx keycloak it ca");
    let ca = ca_params.self_signed(&ca_key).unwrap();
    let issuer = Issuer::new(ca_params, &ca_key);
    let leaf_key = KeyPair::generate().unwrap();
    let leaf = CertificateParams::new(vec!["localhost".to_owned()])
        .unwrap()
        .signed_by(&leaf_key, &issuer)
        .unwrap();
    Tls {
        ca: ca.pem(),
        chain: format!("{}{}", leaf.pem(), ca.pem()),
        key: leaf_key.serialize_pem(),
    }
}

/// Cookies of the simulated browser: Keycloak keeps the login session in them.
#[derive(Default)]
struct Jar(BTreeMap<String, String>);

impl Jar {
    fn absorb(&mut self, response: &Response) {
        for value in response.headers().get_all(SET_COOKIE) {
            let pair = value.to_str().unwrap().split(';').next().unwrap();
            if let Some((name, value)) = pair.split_once('=') {
                if value.is_empty() {
                    self.0.remove(name);
                } else {
                    self.0.insert(name.to_owned(), value.to_owned());
                }
            }
        }
    }

    fn header(&self) -> String {
        self.0
            .iter()
            .map(|(k, v)| format!("{k}={v}"))
            .collect::<Vec<_>>()
            .join("; ")
    }
}

fn form(pairs: &[(&str, &str)]) -> String {
    let mut serializer = url::form_urlencoded::Serializer::new(String::new());
    for (key, value) in pairs {
        serializer.append_pair(key, value);
    }
    serializer.finish()
}

struct Keycloak {
    http: Client,
    base: String,
}

impl Keycloak {
    fn issuer(&self) -> String {
        format!("{}/realms/{REALM}", self.base)
    }

    async fn post_form(&self, url: &str, pairs: &[(&str, &str)]) -> Value {
        let response = self
            .http
            .post(url)
            .header("content-type", "application/x-www-form-urlencoded")
            .body(form(pairs))
            .send()
            .await
            .unwrap();
        let status = response.status();
        let body: Value = response.json().await.unwrap();
        assert_eq!(status, StatusCode::OK, "{url}: {body}");
        body
    }

    async fn client_credentials_token(&self, secret: &str) -> String {
        let url = format!("{}/protocol/openid-connect/token", self.issuer());
        let body = self
            .post_form(
                &url,
                &[
                    ("grant_type", "client_credentials"),
                    ("client_id", "rustryx-cli"),
                    ("client_secret", secret),
                ],
            )
            .await;
        body["access_token"].as_str().unwrap().to_owned()
    }

    async fn admin_token(&self) -> String {
        let url = format!("{}/realms/master/protocol/openid-connect/token", self.base);
        let body = self
            .post_form(
                &url,
                &[
                    ("grant_type", "password"),
                    ("client_id", "admin-cli"),
                    ("username", ADMIN_USER),
                    ("password", ADMIN_PASSWORD),
                ],
            )
            .await;
        body["access_token"].as_str().unwrap().to_owned()
    }

    async fn admin_get(&self, token: &str, path: &str) -> Value {
        let url = format!("{}/admin/realms/{REALM}{path}", self.base);
        let response = self.http.get(&url).bearer_auth(token).send().await.unwrap();
        assert_eq!(response.status(), StatusCode::OK, "{url}");
        response.json().await.unwrap()
    }

    /// Removes a user from a group, as an operator would in the admin console.
    async fn remove_from_group(&self, username: &str, group_path: &str) {
        let token = self.admin_token().await;
        let users = self
            .admin_get(&token, &format!("/users?username={username}&exact=true"))
            .await;
        let user_id = users[0]["id"].as_str().unwrap().to_owned();
        let group = self
            .admin_get(&token, &format!("/group-by-path{group_path}"))
            .await;
        let group_id = group["id"].as_str().unwrap();
        let url = format!(
            "{}/admin/realms/{REALM}/users/{user_id}/groups/{group_id}",
            self.base
        );
        let response = self
            .http
            .delete(&url)
            .bearer_auth(&token)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NO_CONTENT, "{url}");
    }

    /// Plays the browser: opens the authorization URL, submits the login form and returns the
    /// `(code, state)` Keycloak puts on the redirect to the product.
    async fn browser_login(
        &self,
        authorization_url: &Url,
        username: &str,
        password: &str,
    ) -> (String, String) {
        let mut jar = Jar::default();
        let mut url = authorization_url.to_string();
        let mut page = String::new();
        for _ in 0..5 {
            let response = self
                .http
                .get(&url)
                .header(COOKIE, jar.header())
                .send()
                .await
                .unwrap();
            jar.absorb(&response);
            if response.status().is_redirection() {
                url = response.headers()[LOCATION].to_str().unwrap().to_owned();
                continue;
            }
            assert_eq!(response.status(), StatusCode::OK, "{url}");
            page = response.text().await.unwrap();
            break;
        }
        let form_start = page
            .find("id=\"kc-form-login\"")
            .expect("login form on the page");
        let action = page[form_start..]
            .split("action=\"")
            .nth(1)
            .and_then(|rest| rest.split('"').next())
            .expect("form action")
            .replace("&amp;", "&");
        let response = self
            .http
            .post(&action)
            .header(COOKIE, jar.header())
            .header("content-type", "application/x-www-form-urlencoded")
            .body(form(&[
                ("username", username),
                ("password", password),
                ("credentialId", ""),
            ]))
            .send()
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            StatusCode::FOUND,
            "login of {username} was not accepted"
        );
        let location = response.headers()[LOCATION].to_str().unwrap().to_owned();
        assert!(
            location.starts_with(REDIRECT_URL),
            "unexpected redirect {location}"
        );
        let callback = Url::parse(&location).unwrap();
        let param = |name: &str| {
            let (_, value) = callback
                .query_pairs()
                .find(|(k, _)| k == name)
                .unwrap_or_else(|| panic!("{name} missing on {location}"));
            value.into_owned()
        };
        (param("code"), param("state"))
    }
}

fn client_secret(realm: &Value, client_id: &str) -> String {
    realm["clients"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["clientId"] == client_id)
        .and_then(|c| c["secret"].as_str())
        .unwrap_or_else(|| panic!("no secret for {client_id} in the realm file"))
        .to_owned()
}

async fn authenticate(
    provider: &OidcProvider,
    token: &str,
) -> Result<Option<rustryx_core::identity::Principal>, AuthError> {
    provider
        .bearer_authenticator()
        .authenticate(
            &Credentials::Bearer(Secret::new(token)),
            &AuthContext::default(),
        )
        .await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn oidc_against_real_keycloak() {
    if std::env::var("RUSTRYX_IT").as_deref() != Ok("1") {
        eprintln!("SKIP oidc_against_real_keycloak: set RUSTRYX_IT=1 (needs Docker) to run it");
        return;
    }
    let Ok(realm_bytes) = tokio::fs::read(realm_file()).await else {
        eprintln!(
            "SKIP oidc_against_real_keycloak: {} does not exist yet",
            realm_file().display()
        );
        return;
    };
    let realm: Value = serde_json::from_slice(&realm_bytes).unwrap();
    let tls = make_tls();

    let container = GenericImage::new(KEYCLOAK_IMAGE, KEYCLOAK_TAG)
        .with_exposed_port(8443.tcp())
        .with_wait_for(WaitFor::message_on_stdout("Listening on:"))
        .with_env_var("KC_BOOTSTRAP_ADMIN_USERNAME", ADMIN_USER)
        .with_env_var("KC_BOOTSTRAP_ADMIN_PASSWORD", ADMIN_PASSWORD)
        .with_env_var("KC_HTTPS_CERTIFICATE_FILE", "/opt/keycloak/conf/it/tls.crt")
        .with_env_var(
            "KC_HTTPS_CERTIFICATE_KEY_FILE",
            "/opt/keycloak/conf/it/tls.key",
        )
        .with_copy_to(
            CopyTargetOptions::new("/opt/keycloak/conf/it/tls.crt").with_mode(0o644),
            tls.chain.clone().into_bytes(),
        )
        .with_copy_to(
            CopyTargetOptions::new("/opt/keycloak/conf/it/tls.key").with_mode(0o644),
            tls.key.clone().into_bytes(),
        )
        .with_copy_to(
            CopyTargetOptions::new("/opt/keycloak/data/import/realm-rustryx.json").with_mode(0o644),
            realm_bytes.clone(),
        )
        .with_cmd(["start-dev", "--import-realm"])
        .with_startup_timeout(Duration::from_secs(300))
        .start()
        .await
        .expect("keycloak container (RUSTRYX_IT=1 requires Docker and the image)");
    let port = container.get_host_port_ipv4(8443.tcp()).await.unwrap();

    let http = Client::builder()
        .tls_certs_merge([reqwest::Certificate::from_pem(tls.ca.as_bytes()).unwrap()])
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(30))
        .build()
        .unwrap();
    let keycloak = Keycloak {
        http,
        base: format!("https://localhost:{port}"),
    };

    run_steps(&keycloak, &realm, &tls).await;
    container.rm().await.unwrap();
}

async fn run_steps(keycloak: &Keycloak, realm: &Value, tls: &Tls) {
    let mut config = OidcConfig::new(keycloak.issuer(), "rustryx-ui");
    config.authorized_parties = vec!["rustryx-ui".into(), "rustryx-cli".into()];
    config.extra_root_certs_pem = vec![tls.ca.clone().into_bytes()];
    config.request_timeout = Duration::from_secs(10);
    config.ui = Some(UiClientConfig::new(
        "rustryx-ui",
        Secret::new(client_secret(realm, "rustryx-ui")),
        REDIRECT_URL,
        Secret::new(hex::encode((0u8..32).collect::<Vec<_>>())),
    ));
    let provider = OidcProvider::new(config).expect("valid configuration");
    provider
        .refresh_now()
        .await
        .expect("discovery and JWKS of a real Keycloak");
    let status = provider.status();
    assert!(status.discovery_loaded && status.reachable);
    assert!(status.jwks_keys >= 1, "{status:?}");

    // Bearer: service account of the CLI client (client credentials), in /rustryx/ci/demo.
    let cli_secret = client_secret(realm, "rustryx-cli");
    let token = keycloak.client_credentials_token(&cli_secret).await;
    let principal = authenticate(&provider, &token)
        .await
        .unwrap()
        .expect("a principal");
    assert_eq!(principal.username, "service-account-rustryx-cli");
    assert_eq!(principal.source, UserSource::Oidc);
    assert_eq!(principal.auth_method, AuthMethod::OidcBearer);
    assert_eq!(
        principal.issuer.as_deref(),
        Some(keycloak.issuer().as_str())
    );
    assert_eq!(principal.external_groups, ["/rustryx/ci/demo"]);
    assert!(
        !principal.is_admin(),
        "roles are not decided by the authenticator"
    );

    // The same token with one character of the signature changed is refused.
    let mut forged = token.clone().into_bytes();
    let last = forged.len() - 3;
    forged[last] = if forged[last] == b'A' { b'B' } else { b'A' };
    let forged = String::from_utf8(forged).unwrap();
    assert!(!matches!(
        authenticate(&provider, &forged).await,
        Ok(Some(_))
    ));

    // UI: authorization code + PKCE for three users.
    let flow = provider.login_flow().expect("ui configured");
    for (user, password, expected) in [
        ("alice", "alice-test-only", vec!["/rustryx/admins"]),
        ("bob", "bob-test-only", vec!["/rustryx/readers"]),
        ("dave", "dave-test-only", vec![]),
    ] {
        let start = flow.start().unwrap();
        let (code, state) = keycloak
            .browser_login(&start.authorization_url, user, password)
            .await;
        let cookie = start
            .set_cookie
            .split_once('=')
            .and_then(|(_, rest)| rest.split(';').next())
            .unwrap();
        let identity = flow.complete(&code, &state, cookie).await.unwrap();
        assert_eq!(identity.username, user);
        assert_eq!(identity.issuer, keycloak.issuer());
        assert_eq!(identity.groups, expected, "groups of {user}");
        // The same callback again is refused: state and code are single use.
        assert!(flow.complete(&code, &state, cookie).await.is_err());
    }

    // Group removal shows up in the next token: the claim is gone, so are the groups.
    keycloak
        .remove_from_group("service-account-rustryx-cli", "/rustryx/ci/demo")
        .await;
    let fresh = keycloak.client_credentials_token(&cli_secret).await;
    let principal = authenticate(&provider, &fresh)
        .await
        .unwrap()
        .expect("a principal");
    assert!(
        principal.external_groups.is_empty(),
        "{:?}",
        principal.external_groups
    );
}
