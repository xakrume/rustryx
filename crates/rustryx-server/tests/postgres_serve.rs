//! The whole node against a real PostgreSQL: start without a schema, migrate while it runs,
//! sign in as the bootstrap administrator, call the API, shut down.
//!
//! The PostgreSQL server is a testcontainers container, so the test needs Docker. It starts the
//! server in process (`run_server` over the components `assemble` builds) rather than as a child
//! process, because that is the same code path as the binary and the test can then read the
//! outcome of the shutdown.
//!
//! One test function on purpose: the steps depend on each other (the readiness flip needs the
//! migration, the login needs the bootstrap task, the shutdown needs the whole run), and a
//! container per step would dominate the run time.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::too_many_lines,
    clippy::field_reassign_with_default,
    missing_docs
)]

use std::net::SocketAddr;
use std::time::{Duration, Instant};

use reqwest::header::{HeaderMap, HeaderValue};
use reqwest::{Client, Method, StatusCode};
use rustryx_config::{AppConfig, ConfigSecret, Environment};
use rustryx_core::ids::{BlobStoreId, RepositoryId};
use rustryx_core::model::{RepoFormat, RepoType, Repository};
use rustryx_core::names::RepoName;
use rustryx_core::ports::RepositoryRepo;
use rustryx_observability::MetricsHandle;
use rustryx_server::serve::{Outcome, run_server};
use rustryx_server::wiring;
use serde_json::{Value, json};
use sqlx::{Connection, PgConnection};
use tokio::sync::oneshot;

mod common;
use common::{Postgres, WAIT};
use tokio_util::sync::CancellationToken;

const ADMIN_PASSWORD: &str = "Correct-Horse-Battery-9";

struct Reply {
    status: StatusCode,
    headers: HeaderMap,
    body: String,
}

impl Reply {
    fn json(&self) -> Value {
        serde_json::from_str(&self.body)
            .unwrap_or_else(|error| panic!("not json ({error}): {}", self.body))
    }

    /// `name=value` of every `Set-Cookie`, for the cookies the server sets.
    fn set_cookies(&self) -> Vec<String> {
        self.headers
            .get_all("set-cookie")
            .iter()
            .filter_map(|value| value.to_str().ok())
            .map(str::to_owned)
            .collect()
    }

    fn cookie_pair(&self, name: &str) -> Option<String> {
        self.set_cookies().into_iter().find_map(|cookie| {
            let pair = cookie.split(';').next()?.trim();
            pair.starts_with(&format!("{name}="))
                .then(|| pair.to_owned())
        })
    }
}

struct Node {
    client: Client,
    base: String,
    origin: String,
}

impl Node {
    async fn send(
        &self,
        method: Method,
        path: &str,
        headers: &[(&str, String)],
        body: Option<Value>,
    ) -> Reply {
        let mut request = self.client.request(method, format!("{}{path}", self.base));
        for (name, value) in headers {
            request = request.header(*name, HeaderValue::from_str(value).expect("header"));
        }
        if let Some(body) = body {
            request = request.json(&body);
        }
        let response = request.send().await.expect("response");
        let status = response.status();
        let headers = response.headers().clone();
        let body = response.text().await.expect("body");
        Reply {
            status,
            headers,
            body,
        }
    }

    async fn get(&self, path: &str, headers: &[(&str, String)]) -> Reply {
        self.send(Method::GET, path, headers, None).await
    }

    async fn login(&self, username: &str, password: &str) -> Reply {
        self.send(
            Method::POST,
            "/api/v1/auth/login",
            &[("origin", self.origin.clone())],
            Some(json!({"username": username, "password": password})),
        )
        .await
    }
}

fn config(database_url: &str, blob_dir: &std::path::Path) -> AppConfig {
    let mut config = AppConfig::default();
    config.environment = Environment::Development;
    config.server.listen_addr = "127.0.0.1:0".parse().unwrap();
    config.server.shutdown_timeout_secs = 10;
    // Plain HTTP to a loopback listener: the HTTPS rule for Basic credentials is switched off,
    // which is what a development node does. The rule itself is tested in rustryx-api.
    config.server.require_https = false;
    config.database.url = Some(ConfigSecret::new(database_url));
    config.database.acquire_timeout_secs = 5;
    config.auth.bootstrap_admin.password = Some(ConfigSecret::new(ADMIN_PASSWORD));
    config.blob.local_path = blob_dir.to_path_buf();
    config.observability.metrics_public = true;
    config
}

async fn until<T, F, Fut>(what: &str, mut attempt: F) -> T
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Option<T>>,
{
    let deadline = Instant::now() + WAIT;
    while Instant::now() < deadline {
        if let Some(found) = attempt().await {
            return found;
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    panic!("timed out waiting for: {what}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn serves_before_the_schema_exists_becomes_ready_after_migrate_and_signs_the_admin_in() {
    let postgres = Postgres::start().await;
    let blob_dir = tempfile::tempdir().unwrap();
    let config = config(&postgres.url(), &blob_dir.path().join("blobs"));
    config.validate().expect("the test configuration is valid");

    let metrics = MetricsHandle::new().unwrap();
    metrics.install_global().unwrap();
    let components = wiring::assemble(&config, &metrics)
        .await
        .expect("assembly needs no running service");
    let stop = CancellationToken::new();
    let (bound_tx, bound_rx) = oneshot::channel::<SocketAddr>();
    let server = tokio::spawn({
        let (config, metrics, stop) = (config.clone(), metrics.clone(), stop.clone());
        async move {
            run_server(
                &config,
                components,
                metrics,
                None,
                stop.cancelled_owned(),
                Some(bound_tx),
            )
            .await
        }
    });
    let addr = tokio::time::timeout(Duration::from_secs(10), bound_rx)
        .await
        .expect("bound in time")
        .expect("address");
    let node = Node {
        client: Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(15))
            .build()
            .unwrap(),
        base: format!("http://{addr}"),
        origin: format!("http://{addr}"),
    };

    // 1. Liveness does not depend on the database; readiness does, and the schema is empty.
    let health = node.get("/healthz", &[]).await;
    assert_eq!(health.status, StatusCode::OK);
    let ready = node.get("/readyz", &[]).await;
    assert_eq!(
        ready.status,
        StatusCode::SERVICE_UNAVAILABLE,
        "must not be ready before migrate: {}",
        ready.body
    );

    // 2. Migrate on the direct URL while the node runs.
    let summary = wiring::migrator()
        .migrate(&config)
        .await
        .expect("migrations apply");
    assert!(summary.contains("applied"), "{summary}");
    until("readyz 200 after migrate", || async {
        (node.get("/readyz", &[]).await.status == StatusCode::OK).then_some(())
    })
    .await;

    // 3. The bootstrap task creates the administrator once the schema is servable. The test
    //    waits for the row instead of retrying the login: every failed login counts towards the
    //    lockout, which is the very rule the product is supposed to enforce.
    let probe = sqlx::PgPool::connect(&postgres.url()).await.unwrap();
    until("the bootstrap administrator row", || async {
        let found: Option<i64> =
            sqlx::query_scalar("SELECT count(*) FROM users WHERE username = 'admin'")
                .fetch_one(&probe)
                .await
                .ok();
        (found == Some(1)).then_some(())
    })
    .await;
    let login = node.login("admin", ADMIN_PASSWORD).await;
    assert_eq!(login.status, StatusCode::OK, "{}", login.body);
    let session_cookie = login
        .cookie_pair("__Host-rustryx_session")
        .expect("session cookie");
    let csrf_cookie = login
        .cookie_pair("__Host-rustryx_csrf")
        .expect("csrf cookie");
    let all_cookies = login.set_cookies().join("\n");
    assert!(
        all_cookies.contains("HttpOnly") && all_cookies.contains("Secure"),
        "{all_cookies}"
    );
    let cookie_header = format!("{session_cookie}; {csrf_cookie}");
    let csrf_token = csrf_cookie.split_once('=').unwrap().1.to_owned();

    // 4. The session reaches the API.
    let me = node
        .get("/api/v1/me", &[("cookie", cookie_header.clone())])
        .await;
    assert_eq!(me.status, StatusCode::OK, "{}", me.body);
    let me_json = me.json();
    assert_eq!(me_json["authenticated"], true);
    assert_eq!(me_json["user"]["username"], "admin");
    assert_eq!(me_json["permissions"]["is_admin"], true);
    assert!(
        !me.body.contains("argon2") && !me.body.contains("password"),
        "must not leak credential material: {}",
        me.body
    );

    // 5. Negative requirements, against the live node.
    let wrong = node.login("admin", "wrong-password-123").await;
    assert_eq!(
        wrong.status,
        StatusCode::UNAUTHORIZED,
        "must not accept a wrong password"
    );
    let foreign_origin = node
        .send(
            Method::POST,
            "/api/v1/auth/login",
            &[("origin", "https://evil.example".to_owned())],
            Some(json!({"username": "admin", "password": ADMIN_PASSWORD})),
        )
        .await;
    assert_eq!(
        foreign_origin.status,
        StatusCode::FORBIDDEN,
        "must not accept a login posted from another origin"
    );
    let no_csrf = node
        .send(
            Method::POST,
            "/api/v1/auth/logout",
            &[
                ("cookie", cookie_header.clone()),
                ("origin", node.origin.clone()),
            ],
            None,
        )
        .await;
    assert_eq!(
        no_csrf.status,
        StatusCode::FORBIDDEN,
        "must not accept a cookie session mutation without the CSRF header: {}",
        no_csrf.body
    );
    assert!(no_csrf.body.contains("csrf_rejected"), "{}", no_csrf.body);

    // 6. Basic credentials reach the same principal (development: plain HTTP is allowed).
    let basic = format!(
        "Basic {}",
        base64_encode(format!("admin:{ADMIN_PASSWORD}").as_bytes())
    );
    let me_basic = node
        .get("/api/v1/me", &[("authorization", basic.clone())])
        .await;
    assert_eq!(me_basic.status, StatusCode::OK, "{}", me_basic.body);

    // 7. The default blob store was seeded, and a repository request reaches the format engine:
    //    with no format registered it answers for itself (500), which proves the route, the
    //    credentials and the lookup are wired.
    let mut conn = PgConnection::connect(&postgres.url()).await.unwrap();
    let stores: i64 = sqlx::query_scalar("SELECT count(*) FROM blob_stores WHERE name = 'default'")
        .fetch_one(&mut conn)
        .await
        .unwrap();
    assert_eq!(stores, 1, "the default blob store is seeded exactly once");
    assert!(
        blob_dir.path().join("blobs").is_dir(),
        "a local store creates its root"
    );
    let store_id: sqlx::types::Uuid =
        sqlx::query_scalar("SELECT id FROM blob_stores WHERE name = 'default'")
            .fetch_one(&mut conn)
            .await
            .unwrap();
    let pool = sqlx::PgPool::connect(&postgres.url()).await.unwrap();
    let key = rustryx_db::KeyEntry::from_bytes("test", [7; 32]).unwrap();
    let sealer = std::sync::Arc::new(rustryx_db::AeadSealer::new(key, Vec::new()).unwrap());
    let repositories = rustryx_db::PgStore::new(pool, sealer).repositories;
    let hosted = Repository::new(
        RepositoryId::generate(),
        RepoName::parse("raw1").unwrap(),
        RepoFormat::Raw,
        RepoType::Hosted,
        BlobStoreId::from_uuid(store_id),
        time::OffsetDateTime::now_utc(),
    );
    RepositoryRepo::create(&repositories, &hosted)
        .await
        .expect("repository row");
    let repo = node
        .get("/repository/raw1/a.txt", &[("authorization", basic)])
        .await;
    assert_eq!(
        repo.status,
        StatusCode::INTERNAL_SERVER_ERROR,
        "{}",
        repo.body
    );
    let missing = node.get("/repository/nothing/a.txt", &[]).await;
    assert_eq!(missing.status, StatusCode::NOT_FOUND);

    // 8. Metrics: the database is up, and requests carry the class and cache labels.
    let scrape = node.get("/metrics", &[]).await;
    assert_eq!(scrape.status, StatusCode::OK);
    assert!(scrape.body.contains("db_up 1"), "{}", scrape.body);
    assert!(
        scrape.body.contains("class=\"api\"") && scrape.body.contains("cache=\"none\""),
        "the request histogram carries class and cache"
    );

    // 9. Logout with the CSRF token ends the session.
    let logout = node
        .send(
            Method::POST,
            "/api/v1/auth/logout",
            &[
                ("cookie", cookie_header.clone()),
                ("origin", node.origin.clone()),
                ("x-csrf-token", csrf_token),
            ],
            None,
        )
        .await;
    assert!(
        logout.status.is_success(),
        "{}: {}",
        logout.status,
        logout.body
    );
    let after = node.get("/api/v1/me", &[("cookie", cookie_header)]).await;
    assert_ne!(
        after.json()["authenticated"],
        true,
        "a revoked session token must not authenticate again"
    );

    // 10. Graceful shutdown: readiness first, then everything joins within the deadline.
    stop.cancel();
    let outcome: Outcome = tokio::time::timeout(Duration::from_secs(30), server)
        .await
        .expect("the server stops in time")
        .expect("no panic")
        .expect("a clean outcome");
    assert!(!outcome.drain.forced, "no connection had to be cut off");
    assert_eq!(
        outcome.background.aborted, 0,
        "every background task stopped by itself"
    );
    postgres.stop().await;
}

/// Standard base64 without a dependency for this one use.
fn base64_encode(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let n = (u32::from(chunk[0]) << 16)
            | (u32::from(*chunk.get(1).unwrap_or(&0)) << 8)
            | u32::from(*chunk.get(2).unwrap_or(&0));
        for (index, shift) in [18, 12, 6, 0].into_iter().enumerate() {
            if index <= chunk.len() {
                out.push(TABLE[((n >> shift) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}
