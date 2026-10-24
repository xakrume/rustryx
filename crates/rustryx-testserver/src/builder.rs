//! [`TestServer`] and its builder: the real `FormatEngine` over the in-memory ports, the real
//! `Rbac`, the harness authenticator and an HTTP listener on an ephemeral port.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::sync::atomic::AtomicU64;
use std::time::Duration;

use rustryx_core::ports::{
    BlobStore, PrivilegeRepo, RepositoryRepo, RoleRepo, RoutingRuleRepo, SelectorRepo, SystemClock,
    UserRepo,
};
use rustryx_format_core::proxy::executor::ExecutorConfig;
use rustryx_format_core::proxy::ssrf::UpstreamUrlPolicy;
use rustryx_format_core::proxy::upstream::UpstreamClient;
use rustryx_format_core::{
    FormatEngine, FormatHandler, FormatRegistry, Limits, Ports, RuleRoutingGate,
};
use rustryx_rbac::{AnonymousSwitch, InvalidationListener, Rbac, RbacConfig};
use rustryx_testkit::{MemAuditSink, MemBlobStore, MemInvalidationBus, MemStore};
use tokio::net::TcpListener;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use url::Url;

use crate::auth::CredentialBook;
use crate::error::HarnessError;
use crate::policy::loopback_friendly_policy;
use crate::serve::{AppState, router};
use crate::store::SingleStoreRegistry;

/// The host name a container uses to reach the machine that runs the tests.
const DOCKER_HOST_NAME: &str = "host.docker.internal";

/// How long `shutdown` waits for open connections before it cuts them.
const SHUTDOWN_GRACE: Duration = Duration::from_secs(5);

/// Lifetime of a registry token unless the test says otherwise.
const DEFAULT_OCI_TOKEN_TTL: Duration = Duration::from_secs(300);

/// Customization of the ports before the engine is built.
type PortsHook = Box<dyn FnOnce(&mut Ports) + Send>;

/// Builds a [`TestServer`].
///
/// Defaults: bound to `127.0.0.1`, anonymous access OFF (like a fresh `Rbac`), an upstream
/// policy that allows loopback, the default [`Limits`], a `MemBlobStore`.
#[must_use]
pub struct TestServerBuilder {
    handlers: Vec<Arc<dyn FormatHandler>>,
    bind_all: bool,
    anonymous_enabled: bool,
    upstream_policy: UpstreamUrlPolicy,
    limits: Limits,
    blob_store: Option<Arc<dyn BlobStore>>,
    upstream_client: Option<Arc<dyn UpstreamClient>>,
    executor: Option<ExecutorConfig>,
    oci_token_ttl: Duration,
    ports_hook: Option<PortsHook>,
}

impl Default for TestServerBuilder {
    fn default() -> Self {
        Self {
            handlers: Vec::new(),
            bind_all: false,
            anonymous_enabled: false,
            upstream_policy: loopback_friendly_policy(),
            limits: Limits::default(),
            blob_store: None,
            upstream_client: None,
            executor: None,
            oci_token_ttl: DEFAULT_OCI_TOKEN_TTL,
            ports_hook: None,
        }
    }
}

impl TestServerBuilder {
    /// Registers a format handler. A handler that declares several formats (the OCI one
    /// declares `docker` and `oci`) is registered for all of them. A duplicate format makes
    /// `start` panic with the registry's message.
    pub fn register(mut self, handler: Arc<dyn FormatHandler>) -> Self {
        self.handlers.push(handler);
        self
    }

    /// `false` (default) binds `127.0.0.1`; `true` binds `0.0.0.0`, so a Docker container
    /// reaches the server as `host.docker.internal` (see [`TestServer::host_base_url`]).
    pub fn bind_all(mut self, bind_all: bool) -> Self {
        self.bind_all = bind_all;
        self
    }

    /// The initial position of the global anonymous switch. A repository must also carry
    /// `allow_anonymous` (CONTRIBUTING rule 4).
    pub fn anonymous_enabled(mut self, enabled: bool) -> Self {
        self.anonymous_enabled = enabled;
        self
    }

    /// Replaces the SSRF policy for upstream URLs. The default allows loopback over http (see
    /// [`loopback_friendly_policy`]); production is much stricter, so a test of the guard
    /// itself passes its own.
    pub fn upstream_policy(mut self, policy: UpstreamUrlPolicy) -> Self {
        self.upstream_policy = policy;
        self
    }

    /// Replaces the engine limits.
    pub fn limits(mut self, limits: Limits) -> Self {
        self.limits = limits;
        self
    }

    /// Stores blobs in `store` instead of the `MemBlobStore`. [`TestServer::blobs`] then
    /// returns an empty `MemBlobStore` that nothing writes to. Used for bodies that must not
    /// be kept in memory (see `SyntheticBlobStore`) and for failure injection.
    pub fn blob_store(mut self, store: Arc<dyn BlobStore>) -> Self {
        self.blob_store = Some(store);
        self
    }

    /// Replaces the HTTP client that proxy repositories use, for example with a
    /// `FakeUpstream`. The URL policy is still applied to every URL and redirect.
    pub fn upstream_client(mut self, client: Arc<dyn UpstreamClient>) -> Self {
        self.upstream_client = Some(client);
        self
    }

    /// Replaces the timeouts, retry policy and breaker of upstream calls. The defaults retry
    /// with real back-off, which makes a failing-upstream test take seconds.
    pub fn executor_config(mut self, config: ExecutorConfig) -> Self {
        self.executor = Some(config);
        self
    }

    /// Lifetime of the tokens `/v2/token` issues (default 5 minutes).
    pub fn oci_token_ttl(mut self, ttl: Duration) -> Self {
        self.oci_token_ttl = ttl;
        self
    }

    /// Adjusts the ports before the engine is built: the policy hook (immutability, quota
    /// headroom), the event sink, the routing gate.
    pub fn customize_ports(mut self, hook: impl FnOnce(&mut Ports) + Send + 'static) -> Self {
        self.ports_hook = Some(Box::new(hook));
        self
    }

    /// Starts the server.
    ///
    /// # Panics
    /// When a handler cannot be registered, the engine cannot be built or the listener cannot
    /// be bound: a test with a broken harness has nothing to assert. [`Self::try_start`]
    /// returns the error instead.
    #[allow(
        clippy::expect_used,
        reason = "test harness: a harness that cannot start must fail the calling test at once, with the cause"
    )]
    pub async fn start(self) -> TestServer {
        self.try_start()
            .await
            .expect("the test server could not start")
    }

    /// Starts the server, returning the reason when it cannot.
    ///
    /// # Errors
    /// [`HarnessError::Registry`], [`HarnessError::Engine`] or [`HarnessError::Bind`].
    pub async fn try_start(self) -> Result<TestServer, HarnessError> {
        let mut registry = FormatRegistry::new();
        for handler in &self.handlers {
            registry
                .register(Arc::clone(handler))
                .map_err(|error| HarnessError::Registry(error.to_string()))?;
        }
        let bus = MemInvalidationBus::new();
        let store = MemStore::with_builtin_roles()
            .with_default_blob_store()
            .with_invalidation_bus(bus.clone());
        let blobs = MemBlobStore::new();
        let backing: Arc<dyn BlobStore> =
            self.blob_store.unwrap_or_else(|| Arc::new(blobs.clone()));

        let rbac = Rbac::new(
            Arc::new(store.clone()),
            Arc::new(store.clone()),
            &RbacConfig {
                anonymous_enabled: self.anonymous_enabled,
                ..RbacConfig::default()
            },
        );
        let shutdown = CancellationToken::new();
        // The listener keeps the privilege cache coherent when a test changes roles after the
        // first request; without it a change would be invisible for the cache TTL (60 s).
        let listener_task = rbac.spawn_invalidation_listener(&bus, shutdown.child_token());

        let mut ports = Ports::from_store(
            Arc::new(store.clone()),
            Arc::new(rbac.clone()),
            Arc::new(store.audit_sink()),
            Arc::new(SystemClock),
            Arc::new(SingleStoreRegistry(backing)),
        );
        let rules: Arc<dyn RoutingRuleRepo> = Arc::new(store.clone());
        ports.routing = Arc::new(RuleRoutingGate::new(rules));
        if let Some(hook) = self.ports_hook {
            hook(&mut ports);
        }
        let mut engine = FormatEngine::builder(ports, registry)
            .limits(self.limits)
            .url_policy(self.upstream_policy)
            .shutdown_token(shutdown.child_token());
        if let Some(client) = self.upstream_client {
            engine = engine.upstream_client(client);
        }
        if let Some(config) = self.executor {
            engine = engine.executor_config(config);
        }
        let engine = engine
            .build()
            .map_err(|error| HarnessError::Engine(error.to_string()))?;

        let ip = if self.bind_all {
            IpAddr::V4(Ipv4Addr::UNSPECIFIED)
        } else {
            IpAddr::V4(Ipv4Addr::LOCALHOST)
        };
        let listener = TcpListener::bind(SocketAddr::new(ip, 0)).await?;
        let addr = listener.local_addr()?;
        let fallback_origin = Url::parse(&format!("http://127.0.0.1:{}", addr.port()))
            .map_err(|error| HarnessError::Engine(error.to_string()))?;

        let users: Arc<dyn UserRepo> = Arc::new(store.clone());
        let auth = Arc::new(CredentialBook::new(Arc::clone(&users), self.oci_token_ttl));
        let state = AppState {
            engine: engine.clone(),
            repositories: Arc::new(store.clone()),
            auth: Arc::clone(&auth),
            fallback_origin,
            counter: Arc::new(AtomicU64::new(0)),
        };
        let app = router(state).into_make_service_with_connect_info::<SocketAddr>();
        let stop = shutdown.clone();
        let serve_task = tokio::spawn(async move {
            let served = axum::serve(listener, app)
                .with_graceful_shutdown(async move { stop.cancelled().await })
                .await;
            if let Err(error) = served {
                tracing::warn!(%error, "the test server stopped with an error");
            }
        });

        Ok(TestServer {
            addr,
            bind_all: self.bind_all,
            engine,
            blobs,
            rbac_switch: rbac.anonymous_switch(),
            rbac,
            auth,
            repositories: Arc::new(store.clone()),
            users,
            roles: Arc::new(store.clone()),
            privileges: Arc::new(store.clone()),
            selectors: Arc::new(store.clone()),
            audit: store.audit_sink(),
            store,
            shutdown,
            serve_task: Some(serve_task),
            listener_task: Some(listener_task),
        })
    }
}

/// A running in-process HTTP server that mounts the real [`FormatEngine`].
///
/// Dropping it stops the listener (without waiting); [`TestServer::shutdown`] also drains the
/// engine's background tasks.
pub struct TestServer {
    addr: SocketAddr,
    bind_all: bool,
    pub(crate) engine: FormatEngine,
    pub(crate) store: MemStore,
    pub(crate) blobs: MemBlobStore,
    pub(crate) audit: MemAuditSink,
    pub(crate) rbac: Rbac,
    rbac_switch: AnonymousSwitch,
    pub(crate) auth: Arc<CredentialBook>,
    pub(crate) repositories: Arc<dyn RepositoryRepo>,
    pub(crate) users: Arc<dyn UserRepo>,
    pub(crate) roles: Arc<dyn RoleRepo>,
    pub(crate) privileges: Arc<dyn PrivilegeRepo>,
    pub(crate) selectors: Arc<dyn SelectorRepo>,
    shutdown: CancellationToken,
    serve_task: Option<JoinHandle<()>>,
    listener_task: Option<InvalidationListener>,
}

impl TestServer {
    /// A builder with the defaults described on [`TestServerBuilder`].
    pub fn builder() -> TestServerBuilder {
        TestServerBuilder::default()
    }

    /// `http://127.0.0.1:PORT`, how the machine that runs the tests reaches the server.
    #[must_use]
    pub fn base_url(&self) -> String {
        format!("http://127.0.0.1:{}", self.addr.port())
    }

    /// The base URL a Docker container uses: `http://host.docker.internal:PORT` when the
    /// server was started with `bind_all(true)`, otherwise [`TestServer::base_url`] (a
    /// container cannot reach a loopback-only listener).
    #[must_use]
    pub fn host_base_url(&self) -> String {
        if self.bind_all {
            format!("http://{DOCKER_HOST_NAME}:{}", self.addr.port())
        } else {
            self.base_url()
        }
    }

    /// `<base>/repository/<name>`, no trailing slash.
    #[must_use]
    pub fn repository_url(&self, name: &str) -> String {
        format!("{}/repository/{name}", self.base_url())
    }

    /// Like [`TestServer::repository_url`] for a container: built on `host_base_url`.
    #[must_use]
    pub fn host_repository_url(&self, name: &str) -> String {
        format!("{}/repository/{name}", self.host_base_url())
    }

    /// `http://localhost:PORT/repository/<name>`: the URL for pub inside the Docker container
    /// of `Tool::DartDocker`. pub refuses to send a token to a plain http host other than
    /// `localhost`, and the runner maps `localhost` to the host inside the container. Needs
    /// `bind_all(true)`.
    #[must_use]
    pub fn localhost_repository_url(&self, name: &str) -> String {
        format!("http://localhost:{}/repository/{name}", self.addr.port())
    }

    /// The port the server listens on.
    #[must_use]
    pub fn port(&self) -> u16 {
        self.addr.port()
    }

    /// The in-memory metadata store (all ports). Go through a trait object for calls that
    /// share names across ports (see `docs/testkit.md` section 2).
    #[must_use]
    pub fn store(&self) -> &MemStore {
        &self.store
    }

    /// The blob store that takes the bytes (unless `blob_store` replaced it).
    #[must_use]
    pub fn blobs(&self) -> &MemBlobStore {
        &self.blobs
    }

    /// The format engine, for calling it without HTTP (`engine().context(..)`) and for
    /// `fills_in_flight()` / `running_tasks()` in proxy tests.
    #[must_use]
    pub fn engine(&self) -> &FormatEngine {
        &self.engine
    }

    /// The audit log the engine writes to.
    #[must_use]
    pub fn audit(&self) -> &MemAuditSink {
        &self.audit
    }

    /// The authorizer, to ask it questions the HTTP surface cannot (`validate_selector`).
    #[must_use]
    pub fn rbac(&self) -> &Rbac {
        &self.rbac
    }

    /// Turns the global anonymous switch on or off while the server runs. The very next
    /// anonymous request sees it.
    pub fn set_anonymous_enabled(&self, enabled: bool) {
        self.rbac_switch.set(enabled);
    }

    /// Waits until the engine has no background task and no cache fill in flight, so a test
    /// can assert on the cache after a response without racing the fill.
    pub async fn settle(&self) {
        for _ in 0..500 {
            if self.engine.fills_in_flight() == 0 && self.engine.running_tasks() == 0 {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        tracing::warn!(
            fills = self.engine.fills_in_flight(),
            tasks = self.engine.running_tasks(),
            "the engine did not settle within five seconds"
        );
    }

    /// Stops the listener, cancels cache fills and waits for the engine's tracked tasks.
    pub async fn shutdown(mut self) {
        self.shutdown.cancel();
        if let Some(task) = self.serve_task.take()
            && tokio::time::timeout(SHUTDOWN_GRACE, task).await.is_err()
        {
            tracing::warn!("open connections did not close within the grace period");
        }
        self.engine.drain().await;
        if let Some(listener) = self.listener_task.take()
            && let Err(error) = listener.join().await
        {
            tracing::warn!(%error, "the rbac invalidation listener ended abnormally");
        }
    }
}

impl Drop for TestServer {
    fn drop(&mut self) {
        // A test that forgot `shutdown().await` must not leave a listener behind. Cancelling is
        // enough for the tasks; the abort covers a connection that never closes.
        self.shutdown.cancel();
        if let Some(task) = &self.serve_task {
            task.abort();
        }
    }
}
