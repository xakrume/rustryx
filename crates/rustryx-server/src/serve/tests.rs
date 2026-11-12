//! `run_server` in process: startup, shutdown order, supervision.

use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use axum::routing::get;
use rustryx_api::{Requires, RouteModule, RouteSet};
use rustryx_config::{AppConfig, ConfigSecret, Environment};
use rustryx_observability::MetricsHandle;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{Notify, oneshot};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use super::*;
use crate::http_test;
use crate::supervisor::BackgroundTask;
use crate::wiring::Components;

fn dev_config() -> AppConfig {
    let mut config = AppConfig::default();
    config.environment = Environment::Development;
    config.server.listen_addr = "127.0.0.1:0".parse().expect("addr");
    config.server.shutdown_timeout_secs = 5;
    config.observability.metrics_public = true;
    config
}

struct GateModule {
    gate: Arc<Notify>,
    entered: Arc<Notify>,
}

impl RouteModule for GateModule {
    fn name(&self) -> &'static str {
        "gate"
    }

    fn mount_api(&self, routes: RouteSet) -> RouteSet {
        let gate = self.gate.clone();
        let entered = self.entered.clone();
        routes.route(
            "/api/gate",
            Requires::Public,
            get(move || {
                let gate = gate.clone();
                let entered = entered.clone();
                async move {
                    entered.notify_one();
                    gate.notified().await;
                    "released"
                }
            }),
        )
    }
}

struct Running {
    addr: SocketAddr,
    trigger: CancellationToken,
    task: JoinHandle<anyhow::Result<Outcome>>,
    gate: Arc<Notify>,
    entered: Arc<Notify>,
}

async fn start(config: AppConfig, mut components: Components) -> Running {
    let gate = Arc::new(Notify::new());
    let entered = Arc::new(Notify::new());
    components.modules.push(Arc::new(GateModule {
        gate: gate.clone(),
        entered: entered.clone(),
    }));
    let trigger = CancellationToken::new();
    let (tx, rx) = oneshot::channel();
    let metrics = MetricsHandle::new().expect("metrics");
    let shutdown = trigger.clone().cancelled_owned();
    let task = tokio::spawn(async move {
        run_server(&config, components, metrics, None, shutdown, Some(tx)).await
    });
    let addr = tokio::time::timeout(Duration::from_secs(5), rx)
        .await
        .expect("bound in time")
        .expect("address sent");
    Running {
        addr,
        trigger,
        task,
        gate,
        entered,
    }
}

impl Running {
    async fn stop(self) -> Outcome {
        self.trigger.cancel();
        tokio::time::timeout(Duration::from_secs(10), self.task)
            .await
            .expect("server stops")
            .expect("no panic")
            .expect("clean outcome")
    }
}

#[tokio::test]
async fn starts_with_nothing_wired_and_answers_probes() {
    let server = start(dev_config(), Components::not_wired()).await;
    let health = http_test::get(server.addr, "/healthz")
        .await
        .expect("healthz");
    assert_eq!(
        (health.status, health.body.as_str()),
        (200, r#"{"status":"ok"}"#)
    );

    let ready = http_test::get(server.addr, "/readyz")
        .await
        .expect("readyz");
    assert_eq!(
        ready.status, 503,
        "no database is wired, so the node is not ready"
    );
    assert!(
        ready.body.contains(r#""status":"not_ready""#),
        "{}",
        ready.body
    );

    // Browser paths fall through to the embedded UI. A reserved API prefix stays 404.
    let missing = http_test::get(server.addr, "/api/v1/nothing")
        .await
        .expect("404");
    assert_eq!(missing.status, 404);
    assert!(missing.body.contains("route_not_found"));
    assert!(missing.header("x-request-id").is_some());
    let _ = server.stop().await;
}

#[tokio::test]
async fn metrics_follow_the_configured_access_mode() {
    let server = start(dev_config(), Components::not_wired()).await;
    let _ = http_test::get(server.addr, "/healthz")
        .await
        .expect("healthz");
    let metrics = http_test::get(server.addr, "/metrics")
        .await
        .expect("metrics");
    assert_eq!(metrics.status, 200);
    assert!(
        metrics.body.contains("http_requests_total"),
        "{}",
        metrics.body
    );
    let _ = server.stop().await;

    let mut guarded = dev_config();
    guarded.observability.metrics_public = false;
    guarded.observability.metrics_token = Some(ConfigSecret::new("scrape-token-0123456789"));
    let server = start(guarded, Components::not_wired()).await;
    assert_eq!(
        http_test::get(server.addr, "/metrics")
            .await
            .expect("metrics")
            .status,
        401
    );
    let authorised = http_test::get_with(
        server.addr,
        "/metrics",
        &[("Authorization", "Bearer scrape-token-0123456789")],
    )
    .await
    .expect("metrics");
    assert_eq!(authorised.status, 200);
    let _ = server.stop().await;

    let mut off = dev_config();
    off.observability.metrics_enabled = false;
    let server = start(off, Components::not_wired()).await;
    assert_eq!(
        http_test::get(server.addr, "/metrics")
            .await
            .expect("metrics")
            .status,
        404
    );
    let _ = server.stop().await;
}

#[tokio::test]
async fn must_not_require_keycloak_to_start() {
    let mut config = dev_config();
    config.auth.oidc.enabled = true;
    config.auth.oidc.issuer = Some("http://127.0.0.1:1/realms/unreachable".into());
    config.auth.oidc.client_id = Some("rustryx-ui".into());
    config.auth.oidc.client_secret = Some(ConfigSecret::new("secret"));
    config.server.base_url = Some("http://localhost:8081".into());
    let server = start(config, Components::not_wired()).await;
    let health = http_test::get(server.addr, "/healthz")
        .await
        .expect("healthz");
    assert_eq!(
        health.status, 200,
        "an unreachable identity provider must not block the node"
    );
    let _ = server.stop().await;
}

#[tokio::test]
async fn readiness_flips_first_then_the_listener_closes() {
    let mut config = dev_config();
    config.server.drain_delay_secs = 1;
    let server = start(config, Components::not_wired()).await;
    let addr = server.addr;
    let before = http_test::get(addr, "/readyz").await.expect("readyz");
    assert!(
        before.body.contains(r#"{"name":"draining","status":"ok"}"#),
        "{}",
        before.body
    );

    server.trigger.cancel();
    tokio::time::sleep(Duration::from_millis(250)).await;
    let during = http_test::get(addr, "/readyz")
        .await
        .expect("still accepting during the drain delay");
    assert_eq!(during.status, 503);
    assert!(
        during
            .body
            .contains(r#"{"name":"draining","status":"fail"}"#),
        "{}",
        during.body
    );
    let alive = http_test::get(addr, "/healthz")
        .await
        .expect("liveness stays up");
    assert_eq!(alive.status, 200);

    let outcome = tokio::time::timeout(Duration::from_secs(10), server.task)
        .await
        .expect("stops")
        .expect("join")
        .expect("outcome");
    assert!(!outcome.drain.forced);
    assert!(
        TcpStream::connect(addr).await.is_err(),
        "the listener is closed after the drain"
    );
}

#[tokio::test]
async fn an_in_flight_request_completes_during_shutdown() {
    let server = start(dev_config(), Components::not_wired()).await;
    let addr = server.addr;
    let client = tokio::spawn(async move { http_test::get(addr, "/api/gate").await });
    server.entered.notified().await;
    server.trigger.cancel();
    tokio::time::sleep(Duration::from_millis(100)).await;
    server.gate.notify_one();
    let response = client.await.expect("join").expect("the request finished");
    assert_eq!((response.status, response.body.as_str()), (200, "released"));
    let outcome = tokio::time::timeout(Duration::from_secs(10), server.task)
        .await
        .expect("stops")
        .expect("join")
        .expect("outcome");
    assert!(!outcome.drain.forced);
}

#[tokio::test]
async fn requests_that_outlive_the_deadline_are_cut_off_and_reported() {
    let mut config = dev_config();
    config.server.shutdown_timeout_secs = 1;
    let server = start(config, Components::not_wired()).await;
    let addr = server.addr;
    let client = tokio::spawn(async move { http_test::get(addr, "/api/gate").await });
    server.entered.notified().await;
    let started = std::time::Instant::now();
    let outcome = server.stop().await;
    assert!(outcome.drain.forced);
    assert_eq!(outcome.drain.open_at_deadline, 1);
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "bounded by the deadline"
    );
    assert!(client.await.expect("join").is_err());
}

#[tokio::test]
async fn background_tasks_are_cancelled_when_shutdown_begins() {
    let stopped = Arc::new(AtomicBool::new(false));
    let flag = stopped.clone();
    let mut components = Components::not_wired();
    components
        .background
        .push(BackgroundTask::new("probe_task", move |cancel| {
            let flag = flag.clone();
            async move {
                cancel.cancelled().await;
                flag.store(true, Ordering::SeqCst);
            }
        }));
    let server = start(dev_config(), components).await;
    assert!(!stopped.load(Ordering::SeqCst));
    let outcome = server.stop().await;
    assert!(
        stopped.load(Ordering::SeqCst),
        "the task saw the cancellation"
    );
    assert_eq!(
        outcome.background.stopped, 2,
        "the task and the metrics upkeep"
    );
    assert_eq!(outcome.background.aborted, 0);
}

#[tokio::test]
async fn a_port_that_is_taken_fails_the_start_with_a_clear_message() {
    let taken = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let mut config = dev_config();
    config.server.listen_addr = taken.local_addr().expect("addr");
    let metrics = MetricsHandle::new().expect("metrics");
    let result = run_server(
        &config,
        Components::not_wired(),
        metrics,
        None,
        std::future::pending::<()>(),
        None,
    )
    .await;
    let error = result.expect_err("bind must fail");
    assert!(
        error.to_string().contains("cannot listen on 127.0.0.1:"),
        "{error:#}"
    );
}

#[test]
fn the_runtime_is_multi_threaded() {
    let runtime = build_runtime().expect("runtime");
    let flavor = runtime.block_on(async { tokio::runtime::Handle::current().runtime_flavor() });
    assert_eq!(flavor, tokio::runtime::RuntimeFlavor::MultiThread);
}
