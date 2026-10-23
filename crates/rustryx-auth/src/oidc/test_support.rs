//! Test fixtures: key material generated at test time, a hand-moved clock and an in-process
//! HTTPS mock of Keycloak (discovery, JWKS, token endpoint) that counts every request.
//!
//! Keys are generated, never committed: a private key in the repository would trip the
//! secret-detection gate, and a fixture key could be mistaken for a real one.

// Test code: a failing helper must abort the test with its message.
#![allow(clippy::panic)]

use std::collections::HashMap;
use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use base64::Engine;
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::body::Incoming;
use hyper::service::service_fn;
use hyper::{Method, Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use jsonwebtoken::{Algorithm, EncodingKey};
use rcgen::{
    BasicConstraints, CertificateParams, DnType, IsCa, Issuer, KeyPair, PKCS_ECDSA_P256_SHA256,
    PKCS_RSA_SHA256, RsaKeySize,
};
use rustls::pki_types::{PrivateKeyDer, PrivatePkcs8KeyDer};
use rustryx_core::Secret;
use rustryx_core::ports::Clock;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use time::OffsetDateTime;
use tokio::net::TcpListener;
use tokio_rustls::TlsAcceptor;
use tokio_util::sync::CancellationToken;

use super::{OidcConfig, OidcProvider, UiClientConfig};

pub(crate) const AUDIENCE: &str = "rustryx";
pub(crate) const UI_CLIENT_ID: &str = "rustryx-ui";
pub(crate) const UI_CLIENT_SECRET: &str = "ui-client-secret-for-tests";
pub(crate) const REDIRECT_URL: &str = "https://ryx.example.test/oidc/callback";
const REALM_PATH: &str = "/realms/test";

/// Clock the tests move by hand.
pub(crate) struct ManualClock(AtomicI64);

impl ManualClock {
    pub(crate) fn new(unix: i64) -> Arc<Self> {
        Arc::new(Self(AtomicI64::new(unix)))
    }

    pub(crate) fn advance(&self, secs: i64) {
        self.0.fetch_add(secs, Ordering::SeqCst);
    }
}

impl Clock for ManualClock {
    fn now(&self) -> OffsetDateTime {
        OffsetDateTime::from_unix_timestamp(self.0.load(Ordering::SeqCst)).unwrap()
    }
}

// ---------------------------------------------------------------------------------------------
// Keys and tokens
// ---------------------------------------------------------------------------------------------

/// A signing key with its public JWK.
pub(crate) struct TestKey {
    pub(crate) kid: String,
    pub(crate) alg: Algorithm,
    pub(crate) encoding: EncodingKey,
    pub(crate) jwk: Value,
    /// PEM of the public key: what a key confusion attacker would use as HMAC secret.
    pub(crate) public_pem: String,
}

pub(crate) struct TestKeys {
    pub(crate) rsa: TestKey,
    pub(crate) rsa_rotated: TestKey,
    pub(crate) ec: TestKey,
    /// Different key material under the `kid` of `rsa`.
    pub(crate) attacker_rsa: TestKey,
    pub(crate) attacker_ec: TestKey,
}

pub(crate) fn keys() -> &'static TestKeys {
    static KEYS: OnceLock<TestKeys> = OnceLock::new();
    KEYS.get_or_init(|| TestKeys {
        rsa: TestKey::rsa("rsa-1"),
        rsa_rotated: TestKey::rsa("rsa-2"),
        ec: TestKey::ec("ec-1"),
        attacker_rsa: TestKey::rsa("rsa-1"),
        attacker_ec: TestKey::ec("attacker-ec"),
    })
}

impl TestKey {
    fn rsa(kid: &str) -> Self {
        let pair = KeyPair::generate_rsa_for(&PKCS_RSA_SHA256, RsaKeySize::_2048).unwrap();
        let (n, e) = parse_rsa_public(pair.public_key_raw());
        let jwk = json!({"kty": "RSA", "kid": kid, "use": "sig", "alg": "RS256",
            "n": URL_SAFE_NO_PAD.encode(n), "e": URL_SAFE_NO_PAD.encode(e)});
        Self {
            kid: kid.to_owned(),
            alg: Algorithm::RS256,
            encoding: EncodingKey::from_rsa_pem(pair.serialize_pem().as_bytes()).unwrap(),
            jwk,
            public_pem: pair.public_key_pem(),
        }
    }

    fn ec(kid: &str) -> Self {
        let pair = KeyPair::generate_for(&PKCS_ECDSA_P256_SHA256).unwrap();
        let point = pair.public_key_raw();
        assert_eq!(point.len(), 65, "uncompressed P-256 point");
        let jwk = json!({"kty": "EC", "kid": kid, "use": "sig", "alg": "ES256", "crv": "P-256",
            "x": URL_SAFE_NO_PAD.encode(&point[1..33]), "y": URL_SAFE_NO_PAD.encode(&point[33..65])});
        Self {
            kid: kid.to_owned(),
            alg: Algorithm::ES256,
            encoding: EncodingKey::from_ec_pem(pair.serialize_pem().as_bytes()).unwrap(),
            jwk,
            public_pem: pair.public_key_pem(),
        }
    }

    fn alg_name(&self) -> &'static str {
        if self.alg == Algorithm::RS256 {
            "RS256"
        } else {
            "ES256"
        }
    }

    /// A correctly signed token with the standard header.
    pub(crate) fn sign(&self, claims: &Value) -> String {
        self.sign_with_header(json!({}), claims)
    }

    /// Signs with `extra` merged into the standard header (extra keys win, so tests can
    /// override `alg`, `kid` or add `jku`, `x5u`, `jwk`).
    pub(crate) fn sign_with_header(&self, extra: Value, claims: &Value) -> String {
        let mut header = json!({"alg": self.alg_name(), "typ": "JWT", "kid": self.kid});
        merge(&mut header, extra);
        mint(&header, claims, Some((&self.encoding, self.alg)))
    }
}

fn merge(target: &mut Value, extra: Value) {
    if let (Some(target), Value::Object(extra)) = (target.as_object_mut(), extra) {
        for (key, value) in extra {
            if value.is_null() {
                target.remove(&key);
            } else {
                target.insert(key, value);
            }
        }
    }
}

/// Assembles a compact JWT from any header, so tests can build tokens no library would sign.
pub(crate) fn mint(
    header: &Value,
    claims: &Value,
    signer: Option<(&EncodingKey, Algorithm)>,
) -> String {
    let encode = |value: &Value| URL_SAFE_NO_PAD.encode(serde_json::to_vec(value).unwrap());
    let signing_input = format!("{}.{}", encode(header), encode(claims));
    let signature = match signer {
        Some((key, alg)) => jsonwebtoken::crypto::sign(signing_input.as_bytes(), key, alg).unwrap(),
        None => String::new(),
    };
    format!("{signing_input}.{signature}")
}

/// Minimal DER reader for `RSAPublicKey ::= SEQUENCE { modulus INTEGER, publicExponent INTEGER }`.
fn parse_rsa_public(der: &[u8]) -> (Vec<u8>, Vec<u8>) {
    fn tlv(data: &[u8]) -> (u8, &[u8], &[u8]) {
        let tag = data[0];
        let (len, header) = if data[1] & 0x80 == 0 {
            (usize::from(data[1]), 2)
        } else {
            let count = usize::from(data[1] & 0x7f);
            let len = data[2..2 + count]
                .iter()
                .fold(0usize, |acc, b| (acc << 8) | usize::from(*b));
            (len, 2 + count)
        };
        (tag, &data[header..header + len], &data[header + len..])
    }
    let (tag, sequence, _) = tlv(der);
    assert_eq!(tag, 0x30);
    let (_, modulus, rest) = tlv(sequence);
    let (_, exponent, _) = tlv(rest);
    let strip = |int: &[u8]| {
        int.iter()
            .skip_while(|b| **b == 0)
            .copied()
            .collect::<Vec<u8>>()
    };
    (strip(modulus), strip(exponent))
}

pub(crate) fn now() -> i64 {
    i64::try_from(jsonwebtoken::get_current_timestamp()).unwrap()
}

/// Claims of a valid Keycloak access token for the product client.
pub(crate) fn access_claims(issuer: &str) -> Value {
    json!({
        "iss": issuer,
        "aud": [AUDIENCE],
        "sub": "5a0b1c2d-user-1",
        "preferred_username": "alice",
        "email": "alice@example.test",
        "typ": "Bearer",
        "azp": AUDIENCE,
        "scope": "openid profile email",
        "iat": now(),
        "exp": now() + 300,
        "groups": ["/rustryx/admins", "/rustryx/team-payments/publishers"],
        "realm_access": {"roles": ["offline_access", "nx-admin"]},
        "resource_access": {"rustryx": {"roles": ["nx-admin"]}},
    })
}

/// Claims of a valid ID token of the UI client.
pub(crate) fn id_claims(issuer: &str, nonce: &str) -> Value {
    json!({
        "iss": issuer,
        "aud": UI_CLIENT_ID,
        "sub": "5a0b1c2d-user-1",
        "preferred_username": "alice",
        "email": "alice@example.test",
        "typ": "ID",
        "azp": UI_CLIENT_ID,
        "nonce": nonce,
        "iat": now(),
        "exp": now() + 300,
        "auth_time": now(),
        "groups": ["/rustryx/admins"],
    })
}

/// Fails the test when a list that must be empty is not, and shows what was in it.
pub(crate) fn assert_empty<T: std::fmt::Debug>(items: &[T]) {
    assert!(items.is_empty(), "expected no items, got {items:?}");
}

/// Sets (or, with `Value::Null`, removes) one claim.
pub(crate) fn with(mut claims: Value, key: &str, value: Value) -> Value {
    if let Some(map) = claims.as_object_mut() {
        if value.is_null() {
            map.remove(key);
        } else {
            map.insert(key.to_owned(), value);
        }
    }
    claims
}

// ---------------------------------------------------------------------------------------------
// Mock Keycloak
// ---------------------------------------------------------------------------------------------

/// Per-route request counters.
#[derive(Default)]
pub(crate) struct Hits {
    pub(crate) discovery: AtomicUsize,
    pub(crate) jwks: AtomicUsize,
    pub(crate) token: AtomicUsize,
    /// Requests to the path an attacker would name in a `jku` or `x5u` header.
    pub(crate) attacker: AtomicUsize,
}

impl Hits {
    pub(crate) fn jwks(&self) -> usize {
        self.jwks.load(Ordering::SeqCst)
    }

    pub(crate) fn discovery(&self) -> usize {
        self.discovery.load(Ordering::SeqCst)
    }

    pub(crate) fn token(&self) -> usize {
        self.token.load(Ordering::SeqCst)
    }

    pub(crate) fn attacker(&self) -> usize {
        self.attacker.load(Ordering::SeqCst)
    }
}

/// What the token endpoint hands out for one authorization code.
#[derive(Clone)]
pub(crate) struct CodeGrant {
    pub(crate) code_challenge: String,
    pub(crate) id_token: String,
    pub(crate) scope: String,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum TokenMode {
    Normal,
    ServerError,
    Garbage,
}

/// A request that reached the token endpoint, for assertions.
#[derive(Clone, Debug)]
pub(crate) struct TokenRequest {
    pub(crate) form: HashMap<String, String>,
    pub(crate) basic_auth_ok: bool,
}

pub(crate) struct MockState {
    /// Value of `issuer` in the discovery document.
    pub(crate) issuer: String,
    /// Origin that endpoints in the discovery document point to.
    pub(crate) endpoint_origin: String,
    pub(crate) hits: Hits,
    pub(crate) jwks: Mutex<Value>,
    pub(crate) jwks_headers: Mutex<Vec<(&'static str, String)>>,
    pub(crate) jwks_delay_ms: AtomicU64,
    /// Bytes of padding added to the JWKS body, to exercise the size cap.
    pub(crate) jwks_padding: AtomicUsize,
    pub(crate) down: AtomicBool,
    pub(crate) token_mode: Mutex<TokenMode>,
    pub(crate) codes: Mutex<HashMap<String, CodeGrant>>,
    pub(crate) token_requests: Mutex<Vec<TokenRequest>>,
    pub(crate) discovery_http_jwks: AtomicBool,
}

/// In-process HTTPS server standing in for one Keycloak realm.
pub(crate) struct MockKeycloak {
    pub(crate) state: Arc<MockState>,
    pub(crate) addr: SocketAddr,
    pub(crate) ca_pem: Vec<u8>,
    cancel: CancellationToken,
}

impl Drop for MockKeycloak {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

impl MockKeycloak {
    pub(crate) async fn start() -> Self {
        Self::start_with_issuer(None).await
    }

    /// `public_origin` makes the discovery document name that origin as issuer and endpoint
    /// host (split horizon): it is not reachable from the test, `internal` is.
    pub(crate) async fn start_with_issuer(public_origin: Option<&str>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let origin = format!("https://localhost:{}", addr.port());
        let (ca_pem, acceptor) = tls_material();
        let advertised = public_origin.map_or_else(|| origin.clone(), str::to_owned);
        let state = Arc::new(MockState {
            issuer: format!("{advertised}{REALM_PATH}"),
            endpoint_origin: advertised,
            hits: Hits::default(),
            jwks: Mutex::new(json!({"keys": [keys().rsa.jwk.clone(), keys().ec.jwk.clone()]})),
            jwks_headers: Mutex::new(Vec::new()),
            jwks_delay_ms: AtomicU64::new(0),
            jwks_padding: AtomicUsize::new(0),
            down: AtomicBool::new(false),
            token_mode: Mutex::new(TokenMode::Normal),
            codes: Mutex::new(HashMap::new()),
            token_requests: Mutex::new(Vec::new()),
            discovery_http_jwks: AtomicBool::new(false),
        });
        let cancel = CancellationToken::new();
        tokio::spawn(accept_loop(
            listener,
            acceptor,
            Arc::clone(&state),
            cancel.clone(),
        ));
        Self {
            state,
            addr,
            ca_pem,
            cancel,
        }
    }

    /// Realm URL the server under test reaches (internal issuer).
    pub(crate) fn realm_url(&self) -> String {
        format!("https://localhost:{}{REALM_PATH}", self.addr.port())
    }

    pub(crate) fn hits(&self) -> &Hits {
        &self.state.hits
    }

    pub(crate) fn set_jwks(&self, keys: &[&TestKey]) {
        let jwks = json!({"keys": keys.iter().map(|k| k.jwk.clone()).collect::<Vec<_>>()});
        *self.state.jwks.lock().unwrap() = jwks;
    }

    pub(crate) fn set_jwks_header(&self, name: &'static str, value: &str) {
        self.state
            .jwks_headers
            .lock()
            .unwrap()
            .push((name, value.to_owned()));
    }

    pub(crate) fn set_down(&self, down: bool) {
        self.state.down.store(down, Ordering::SeqCst);
    }

    /// Kills the listener and every open connection: the next request is a connection error.
    pub(crate) fn kill(&self) {
        self.cancel.cancel();
    }

    pub(crate) fn set_token_mode(&self, mode: TokenMode) {
        *self.state.token_mode.lock().unwrap() = mode;
    }

    pub(crate) fn grant(&self, code: &str, grant: CodeGrant) {
        self.state
            .codes
            .lock()
            .unwrap()
            .insert(code.to_owned(), grant);
    }

    pub(crate) fn token_requests(&self) -> Vec<TokenRequest> {
        self.state.token_requests.lock().unwrap().clone()
    }

    pub(crate) fn issuer(&self) -> &str {
        &self.state.issuer
    }
}

/// A CA and a server certificate for `localhost`, as the CA PEM and a ready acceptor.
fn tls_material() -> (Vec<u8>, TlsAcceptor) {
    let ca_key = KeyPair::generate().unwrap();
    let mut ca_params = CertificateParams::new(Vec::<String>::new()).unwrap();
    ca_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    ca_params
        .distinguished_name
        .push(DnType::CommonName, "rustryx oidc test ca");
    let ca_cert = ca_params.self_signed(&ca_key).unwrap();
    let issuer = Issuer::new(ca_params, &ca_key);
    let leaf_key = KeyPair::generate().unwrap();
    let leaf = CertificateParams::new(vec!["localhost".to_owned()])
        .unwrap()
        .signed_by(&leaf_key, &issuer)
        .unwrap();
    let provider = Arc::new(rustls::crypto::aws_lc_rs::default_provider());
    let config = rustls::ServerConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(
            vec![leaf.der().clone()],
            PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(leaf_key.serialize_der())),
        )
        .unwrap();
    (
        ca_cert.pem().into_bytes(),
        TlsAcceptor::from(Arc::new(config)),
    )
}

async fn accept_loop(
    listener: TcpListener,
    acceptor: TlsAcceptor,
    state: Arc<MockState>,
    cancel: CancellationToken,
) {
    loop {
        let accepted = tokio::select! {
            () = cancel.cancelled() => return,
            accepted = listener.accept() => accepted,
        };
        let Ok((tcp, _)) = accepted else { continue };
        let (acceptor, state, cancel) = (acceptor.clone(), Arc::clone(&state), cancel.clone());
        tokio::spawn(async move {
            let Ok(tls) = acceptor.accept(tcp).await else {
                return;
            };
            let service = service_fn(move |request| handle(Arc::clone(&state), request));
            let connection = hyper::server::conn::http1::Builder::new()
                .serve_connection(TokioIo::new(tls), service);
            tokio::select! {
                () = cancel.cancelled() => {}
                _ = connection => {}
            }
        });
    }
}

fn respond(status: StatusCode, content_type: &str, body: String) -> Response<Full<Bytes>> {
    Response::builder()
        .status(status)
        .header("content-type", content_type)
        .body(Full::new(Bytes::from(body)))
        .unwrap()
}

async fn handle(
    state: Arc<MockState>,
    request: Request<Incoming>,
) -> Result<Response<Full<Bytes>>, Infallible> {
    let path = request.uri().path().to_owned();
    if state.down.load(Ordering::SeqCst) {
        return Ok(respond(
            StatusCode::SERVICE_UNAVAILABLE,
            "text/plain",
            "down".into(),
        ));
    }
    let realm = REALM_PATH;
    let response = match (request.method().clone(), path.as_str()) {
        (Method::GET, p) if p == format!("{realm}/.well-known/openid-configuration") => {
            state.hits.discovery.fetch_add(1, Ordering::SeqCst);
            respond(
                StatusCode::OK,
                "application/json",
                discovery_document(&state).to_string(),
            )
        }
        (Method::GET, p) if p == format!("{realm}/protocol/openid-connect/certs") => {
            state.hits.jwks.fetch_add(1, Ordering::SeqCst);
            let delay = state.jwks_delay_ms.load(Ordering::SeqCst);
            if delay > 0 {
                tokio::time::sleep(Duration::from_millis(delay)).await;
            }
            let mut body = state.jwks.lock().unwrap().clone();
            let padding = state.jwks_padding.load(Ordering::SeqCst);
            if padding > 0 {
                body["padding"] = Value::String("x".repeat(padding));
            }
            let mut response = respond(StatusCode::OK, "application/json", body.to_string());
            for (name, value) in state.jwks_headers.lock().unwrap().iter() {
                response.headers_mut().insert(*name, value.parse().unwrap());
            }
            response
        }
        (Method::GET, "/attacker/jwks") => {
            state.hits.attacker.fetch_add(1, Ordering::SeqCst);
            respond(
                StatusCode::OK,
                "application/json",
                json!({"keys": []}).to_string(),
            )
        }
        (Method::POST, p) if p == format!("{realm}/protocol/openid-connect/token") => {
            token_endpoint(&state, request).await
        }
        _ => respond(StatusCode::NOT_FOUND, "text/plain", "not found".into()),
    };
    Ok(response)
}

fn discovery_document(state: &MockState) -> Value {
    let base = format!("{}{REALM_PATH}", state.endpoint_origin);
    let jwks_uri = format!("{base}/protocol/openid-connect/certs");
    let jwks_uri = if state.discovery_http_jwks.load(Ordering::SeqCst) {
        jwks_uri.replacen("https://", "http://", 1)
    } else {
        jwks_uri
    };
    json!({
        "issuer": state.issuer,
        "authorization_endpoint": format!("{base}/protocol/openid-connect/auth"),
        "token_endpoint": format!("{base}/protocol/openid-connect/token"),
        "jwks_uri": jwks_uri,
        "userinfo_endpoint": format!("{base}/protocol/openid-connect/userinfo"),
        "response_types_supported": ["code", "id_token", "token id_token"],
        "subject_types_supported": ["public", "pairwise"],
        "id_token_signing_alg_values_supported": ["RS256", "ES256"],
        "code_challenge_methods_supported": ["plain", "S256"],
        "token_endpoint_auth_methods_supported": ["client_secret_basic", "client_secret_post"],
    })
}

async fn token_endpoint(state: &MockState, request: Request<Incoming>) -> Response<Full<Bytes>> {
    state.hits.token.fetch_add(1, Ordering::SeqCst);
    let basic = request
        .headers()
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);
    let body = request.into_body().collect().await.unwrap().to_bytes();
    let form: HashMap<String, String> = url::form_urlencoded::parse(&body).into_owned().collect();
    let expected = format!(
        "Basic {}",
        STANDARD.encode(format!("{UI_CLIENT_ID}:{UI_CLIENT_SECRET}"))
    );
    let basic_auth_ok = basic.as_deref() == Some(expected.as_str());
    state.token_requests.lock().unwrap().push(TokenRequest {
        form: form.clone(),
        basic_auth_ok,
    });
    match *state.token_mode.lock().unwrap() {
        TokenMode::ServerError => {
            return respond(
                StatusCode::INTERNAL_SERVER_ERROR,
                "text/plain",
                "boom".into(),
            );
        }
        TokenMode::Garbage => {
            return respond(StatusCode::OK, "application/json", "{not json".into());
        }
        TokenMode::Normal => {}
    }
    let invalid_grant = || {
        respond(
            StatusCode::BAD_REQUEST,
            "application/json",
            json!({"error": "invalid_grant", "error_description": "bad code"}).to_string(),
        )
    };
    if !basic_auth_ok
        || form.get("grant_type").map(String::as_str) != Some("authorization_code")
        || form.get("redirect_uri").map(String::as_str) != Some(REDIRECT_URL)
    {
        return invalid_grant();
    }
    let grant = form
        .get("code")
        .and_then(|code| state.codes.lock().unwrap().remove(code));
    let Some(grant) = grant else {
        return invalid_grant();
    };
    let verifier = form.get("code_verifier").map_or("", String::as_str);
    let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
    if challenge != grant.code_challenge {
        return invalid_grant();
    }
    respond(
        StatusCode::OK,
        "application/json",
        json!({
            "access_token": "opaque-access-token",
            "token_type": "Bearer",
            "expires_in": 300,
            "refresh_token": "opaque-refresh-token",
            "id_token": grant.id_token,
            "scope": grant.scope,
        })
        .to_string(),
    )
}

// ---------------------------------------------------------------------------------------------
// Environment
// ---------------------------------------------------------------------------------------------

/// A mock realm plus a provider configured to trust it.
pub(crate) struct TestEnv {
    pub(crate) mock: MockKeycloak,
    pub(crate) provider: OidcProvider,
}

pub(crate) fn ui_config() -> UiClientConfig {
    UiClientConfig::new(
        UI_CLIENT_ID,
        Secret::new(UI_CLIENT_SECRET),
        REDIRECT_URL,
        Secret::new(hex::encode((0u8..32).collect::<Vec<_>>())),
    )
}

impl TestEnv {
    /// Provider built, Keycloak not contacted yet.
    pub(crate) async fn start() -> Self {
        Self::start_with(|_| {}).await
    }

    pub(crate) async fn start_with(adjust: impl FnOnce(&mut OidcConfig)) -> Self {
        let mock = MockKeycloak::start().await;
        Self::with_mock(mock, adjust, Arc::new(rustryx_core::ports::SystemClock))
    }

    pub(crate) fn with_mock(
        mock: MockKeycloak,
        adjust: impl FnOnce(&mut OidcConfig),
        clock: Arc<dyn Clock>,
    ) -> Self {
        let mut config = OidcConfig::new(mock.realm_url(), AUDIENCE);
        if mock.issuer() != mock.realm_url() {
            config.public_issuer_url = Some(mock.issuer().to_owned());
        }
        config.extra_root_certs_pem = vec![mock.ca_pem.clone()];
        config.connect_timeout = Duration::from_secs(2);
        config.request_timeout = Duration::from_secs(4);
        adjust(&mut config);
        let provider = OidcProvider::new_for_tests(config, clock).unwrap();
        Self { mock, provider }
    }

    /// Provider that already completed discovery and the first JWKS fetch.
    pub(crate) async fn start_warm() -> Self {
        let env = Self::start().await;
        env.provider.refresh_now().await.unwrap();
        env
    }

    pub(crate) fn issuer(&self) -> String {
        self.mock.issuer().to_owned()
    }

    pub(crate) fn access_token(&self, key: &TestKey) -> String {
        key.sign(&access_claims(&self.issuer()))
    }

    pub(crate) async fn authenticate(
        &self,
        token: &str,
    ) -> Result<Option<rustryx_core::identity::Principal>, rustryx_core::AuthError> {
        use rustryx_core::identity::{AuthContext, Credentials};
        use rustryx_core::ports::Authenticator;
        self.provider
            .bearer_authenticator()
            .authenticate(
                &Credentials::Bearer(Secret::new(token)),
                &AuthContext::default(),
            )
            .await
    }
}
