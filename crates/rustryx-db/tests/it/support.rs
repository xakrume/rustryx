//! Shared PostgreSQL container and one isolated database per test.
//!
//! Why a dedicated runtime: every `#[tokio::test]` owns its own runtime that ends with the
//! test. The container handle and its Docker client are created once and must outlive the
//! first test, so they live on a runtime owned by a dedicated thread for the whole process.

use std::sync::OnceLock;

use rustryx_core::Secret;
use sqlx::{Connection, PgConnection};
use testcontainers::core::{IntoContainerPort, WaitFor};
use testcontainers::runners::AsyncRunner;
use testcontainers::{ContainerAsync, GenericImage, ImageExt};
use tokio::runtime::Handle;
use tokio::sync::{OnceCell, Semaphore};

const TEMPLATE_DB: &str = "rustryx_template";
const PASSWORD: &str = "postgres";

static HANDLE: OnceLock<Handle> = OnceLock::new();
static SERVER: OnceCell<Server> = OnceCell::const_new();
/// `CREATE DATABASE .. TEMPLATE` refuses to run while another session is connected to the
/// template, and two copies in flight make that check flaky; copies are cheap, so they are
/// serialised.
static CREATE_PERMIT: Semaphore = Semaphore::const_new(1);

fn handle() -> &'static Handle {
    HANDLE
        .get()
        .expect("support runtime is started by server()")
}

/// Runs a future on the long-lived support runtime and returns its output.
pub async fn on_support_rt<T: Send + 'static>(
    fut: impl std::future::Future<Output = T> + Send + 'static,
) -> T {
    handle().spawn(fut).await.expect("support task")
}

/// Sealer with a fixed, test-only key.
pub fn test_sealer() -> std::sync::Arc<dyn rustryx_db::SecretSealer> {
    let key = rustryx_db::KeyEntry::from_bytes("test", [42; 32]).expect("key");
    std::sync::Arc::new(rustryx_db::AeadSealer::new(key, vec![]).expect("sealer"))
}

pub struct Server {
    // Held so the container lives as long as the process.
    _container: ContainerAsync<GenericImage>,
    host: String,
    port: u16,
}

impl Server {
    /// Admin connection URL of `database`.
    pub fn url_for(&self, database: &str) -> Secret {
        Secret::new(format!(
            "postgres://postgres:{PASSWORD}@{}:{}/{database}",
            self.host, self.port
        ))
    }
}

/// Seconds the container lives at most. testcontainers 0.28 has no reaper process: a
/// container is removed when its handle is dropped, and the handle here is a static that
/// never drops, so without a limit every test run would leave one server behind. The
/// container ends itself after this time and Docker removes it (auto remove). Override with
/// `RUSTRYX_DB_TEST_CONTAINER_TTL_SECS`; keep it above the longest test run.
fn container_ttl_secs() -> u64 {
    std::env::var("RUSTRYX_DB_TEST_CONTAINER_TTL_SECS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(1200)
}

async fn start_server() -> Server {
    // `(sleep N; kill 1) &` then `exec`: after the exec chain of the image entrypoint,
    // PID 1 is postgres itself, and SIGTERM makes it shut down cleanly.
    let script = format!(
        "(sleep {}; kill 1) & exec docker-entrypoint.sh postgres \
         -c fsync=off -c synchronous_commit=off -c full_page_writes=off -c max_connections=400",
        container_ttl_secs()
    );
    let container = GenericImage::new("postgres", "16-alpine")
        .with_entrypoint("sh")
        .with_exposed_port(5432.tcp())
        .with_wait_for(WaitFor::message_on_stderr(
            "database system is ready to accept connections",
        ))
        .with_env_var("POSTGRES_PASSWORD", PASSWORD)
        .with_label("rustryx.test", "rustryx-db")
        .with_cmd(["-c".to_owned(), script])
        .with_host_config_modifier(|host| host.auto_remove = Some(true))
        .start()
        .await
        .expect("start postgres container (is Docker running?)");
    let host = container
        .get_host()
        .await
        .expect("container host")
        .to_string();
    let host = if host == "localhost" {
        "127.0.0.1".to_owned()
    } else {
        host
    };
    let port = container
        .get_host_port_ipv4(5432.tcp())
        .await
        .expect("mapped port");
    let server = Server {
        _container: container,
        host,
        port,
    };
    wait_until_ready(&server).await;
    build_template(&server).await;
    server
}

/// The image logs "ready to accept connections" twice: once for the temporary server of
/// the init scripts, once for the real one. Waiting for the log line alone races the
/// restart, so readiness is confirmed by connecting.
async fn wait_until_ready(server: &Server) {
    let url = server.url_for("postgres");
    for _ in 0..100 {
        if let Ok(mut conn) = PgConnection::connect(url.expose()).await
            && sqlx::query("SELECT 1").execute(&mut conn).await.is_ok()
        {
            let _ = conn.close().await;
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    }
    panic!("postgres did not become ready");
}

async fn build_template(server: &Server) {
    let mut admin = PgConnection::connect(server.url_for("postgres").expose())
        .await
        .expect("admin connection");
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "CREATE DATABASE {TEMPLATE_DB}"
    )))
    .execute(&mut admin)
    .await
    .expect("create template database");
    let _ = admin.close().await;
    rustryx_db::migrate(&server.url_for(TEMPLATE_DB))
        .await
        .expect("migrate template database");
}

/// Starts the support runtime on its own thread and the container on that runtime. A plain
/// thread with `block_on` is used because the container futures borrow the handle in a way
/// that cannot be proven `Send` for `Handle::spawn`; the thread then parks the runtime on a
/// never-ready future so it outlives every test.
pub async fn server() -> &'static Server {
    SERVER
        .get_or_init(|| async {
            let (tx, rx) = tokio::sync::oneshot::channel();
            std::thread::Builder::new()
                .name("db-test-support".to_owned())
                .spawn(move || {
                    let rt = tokio::runtime::Builder::new_multi_thread()
                        .worker_threads(2)
                        .enable_all()
                        .build()
                        .expect("support runtime");
                    let _ = HANDLE.set(rt.handle().clone());
                    rt.block_on(async move {
                        let server = start_server().await;
                        let _ = tx.send(server);
                        std::future::pending::<()>().await;
                    });
                })
                .expect("spawn support thread");
            rx.await.expect("support thread started the server")
        })
        .await
}

/// An isolated database, dropped (forcibly) when the value goes away.
pub struct TestDb {
    pub name: String,
    /// Direct URL of this database.
    pub url: Secret,
    server: &'static Server,
}

impl TestDb {
    /// A copy of the migrated template.
    pub async fn new() -> Self {
        Self::create(Some(TEMPLATE_DB)).await
    }

    /// An empty database with no schema at all.
    pub async fn empty() -> Self {
        Self::create(None).await
    }

    async fn create(template: Option<&'static str>) -> Self {
        let server = server().await;
        let name = format!("t_{}", uuid::Uuid::now_v7().simple());
        let admin_url = server.url_for("postgres");
        let create_name = name.clone();
        let permit = CREATE_PERMIT.acquire().await.expect("semaphore open");
        on_support_rt(async move {
            let mut admin = PgConnection::connect(admin_url.expose())
                .await
                .expect("admin");
            let statement = match template {
                Some(t) => format!("CREATE DATABASE \"{create_name}\" TEMPLATE {t}"),
                None => format!("CREATE DATABASE \"{create_name}\""),
            };
            sqlx::query(sqlx::AssertSqlSafe(statement))
                .execute(&mut admin)
                .await
                .expect("create test database");
            let _ = admin.close().await;
        })
        .await;
        drop(permit);
        Self {
            url: server.url_for(&name),
            name,
            server,
        }
    }

    /// The store bundle over a fresh pool, sealing with a fixed test key.
    pub async fn store(&self) -> rustryx_db::PgStore {
        rustryx_db::PgStore::new(self.pool().await, test_sealer())
    }

    /// Pool with the default settings of the crate.
    pub async fn pool(&self) -> sqlx::PgPool {
        let mut config = rustryx_db::PgConfig::new(self.url.clone());
        config.max_connections = 12;
        rustryx_db::connect(&config).await.expect("pool")
    }
}

impl Drop for TestDb {
    fn drop(&mut self) {
        let admin_url = self.server.url_for("postgres");
        let name = self.name.clone();
        // Fire and forget on the support runtime: Drop cannot await, and FORCE terminates
        // the connections of a pool that is still open.
        handle().spawn(async move {
            if let Ok(mut admin) = PgConnection::connect(admin_url.expose()).await {
                let _ = sqlx::query(sqlx::AssertSqlSafe(format!(
                    "DROP DATABASE IF EXISTS \"{name}\" WITH (FORCE)"
                )))
                .execute(&mut admin)
                .await;
                let _ = admin.close().await;
            }
        });
    }
}

/// A direct `LISTEN` session on the RBAC channel of this database.
pub async fn listen_rbac(db: &TestDb) -> sqlx::postgres::PgListener {
    let mut listener = sqlx::postgres::PgListener::connect(db.url.expose())
        .await
        .expect("listener");
    listener
        .listen(rustryx_db::RBAC_CHANNEL)
        .await
        .expect("listen");
    listener
}

/// Next payload within `wait`, or `None` when nothing arrived.
pub async fn next_payload(
    listener: &mut sqlx::postgres::PgListener,
    wait: std::time::Duration,
) -> Option<String> {
    match tokio::time::timeout(wait, listener.recv()).await {
        Ok(Ok(n)) => Some(n.payload().to_owned()),
        _ => None,
    }
}

/// SQLSTATE of a failed raw statement.
pub fn sqlstate(err: &sqlx::Error) -> String {
    err.as_database_error()
        .and_then(sqlx::error::DatabaseError::code)
        .map(std::borrow::Cow::into_owned)
        .unwrap_or_default()
}
