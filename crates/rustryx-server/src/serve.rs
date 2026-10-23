//! `rustryx serve`: runtime, startup order and graceful shutdown.
//!
//! Startup order, each step failing the process before the next one runs:
//!
//! 1. the configuration was loaded and validated by the caller, before anything here;
//! 2. the tokio runtime is built (multi-thread);
//! 3. logging, and tracing when configured, are initialised;
//! 4. stop-signal handlers are installed;
//! 5. components are assembled (never runs migrations, never waits for Keycloak,
//!    LDAP, Redis or S3);
//! 6. the listener is bound and readiness may report "listening".
//!
//! Shutdown order (SIGTERM or SIGINT):
//!
//! 1. readiness flips to 503 (liveness stays 200) so the load balancer stops
//!    sending new requests;
//! 2. the listener keeps accepting for `server.drain_delay_secs`, for balancers
//!    that need time to notice;
//! 3. the listener closes, in-flight requests finish for up to
//!    `server.shutdown_timeout_secs`, the rest are cut off;
//! 4. at the same moment the background-task token is cancelled and the tasks get the
//!    same deadline, after which stragglers are aborted;
//! 5. logs and traces are flushed and the process exits with code 0.

use std::future::Future;
use std::net::SocketAddr;
use std::time::Duration;

use anyhow::Context;
use rustryx_api::{Lifecycle, RouterBuilder};
use rustryx_config::AppConfig;
use rustryx_observability::{DroppedLines, MetricsHandle};
use tokio::net::TcpListener;
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;

use crate::listener::{self, DrainReport};
use crate::signals::StopSignals;
use crate::supervisor::{BackgroundTask, StopReport, Supervisor};
use crate::wiring::{self, Components};

/// Blocking threads (archive unpacking, password hashing). Bounded so that a burst
/// cannot spawn threads without limit; the semaphores in the request path are what
/// really limit concurrency.
const MAX_BLOCKING_THREADS: usize = 64;
/// Interval of the metrics upkeep task.
const METRICS_UPKEEP_INTERVAL: Duration = Duration::from_secs(10);
/// How long the runtime may take to wind down after the server stopped.
const RUNTIME_SHUTDOWN: Duration = Duration::from_secs(5);

/// What a finished server run reports.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Outcome {
    /// Connection draining.
    pub drain: DrainReport,
    /// Background task shutdown.
    pub background: StopReport,
}

/// Builds the multi-thread runtime.
///
/// # Errors
/// The runtime could not be created.
pub fn build_runtime() -> std::io::Result<tokio::runtime::Runtime> {
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .thread_name("rustryx-worker")
        .max_blocking_threads(MAX_BLOCKING_THREADS)
        .build()
}

/// Runs the server until SIGTERM or SIGINT. Called with a validated configuration.
///
/// # Errors
/// Startup failed (logging, signals, assembly, bind) or shutdown reported a failure.
pub fn run(config: &AppConfig, warnings: &[String]) -> anyhow::Result<()> {
    let runtime = build_runtime().context("cannot build the tokio runtime")?;
    let enter = runtime.enter();
    // OTLP over gRPC needs the runtime context, which `enter` provides.
    let guard = rustryx_observability::init(&wiring::observability_options(config))
        .context("cannot initialise logging and tracing")?;
    let metrics = MetricsHandle::new().context("cannot create the metrics recorder")?;
    metrics
        .install_global()
        .context("cannot install the metrics recorder")?;

    tracing::info!(
        version = env!("CARGO_PKG_VERSION"),
        environment = ?config.environment,
        tracing = guard.tracing_enabled(),
        "rustryx starting"
    );
    for warning in warnings {
        tracing::warn!(%warning, "configuration warning");
    }

    let dropped = guard.dropped_lines();
    let result = runtime.block_on(async {
        let mut signals = StopSignals::install().context("cannot install signal handlers")?;
        let components = wiring::assemble(config, &metrics)
            .await
            .context("cannot assemble the server components")?;
        let shutdown = async move {
            let reason = signals.wait().await;
            tracing::info!(?reason, "stop signal received");
        };
        run_server(config, components, metrics, dropped, shutdown, None).await
    });
    drop(enter);
    runtime.shutdown_timeout(RUNTIME_SHUTDOWN);
    if let Err(error) = guard.shutdown() {
        // Logging is gone by now; standard error is all that is left.
        eprintln!("rustryx: flushing telemetry failed: {error}");
    }
    result.map(|outcome| {
        tracing::debug!(?outcome, "server stopped");
    })
}

/// Serves `components` until `shutdown` resolves, then drains.
///
/// `on_bound` receives the bound address as soon as the listener is open (tests use
/// port 0 and need to learn it).
///
/// # Errors
/// The listener could not be bound.
pub async fn run_server(
    config: &AppConfig,
    mut components: Components,
    metrics: MetricsHandle,
    dropped: Option<DroppedLines>,
    shutdown: impl Future<Output = ()>,
    on_bound: Option<oneshot::Sender<SocketAddr>>,
) -> anyhow::Result<Outcome> {
    let background = std::mem::take(&mut components.background);
    let lifecycle = Lifecycle::new();
    let state = wiring::app_state(config, &components, lifecycle.clone(), metrics.clone());
    let mut builder = RouterBuilder::new(state);
    for module in &components.modules {
        builder = builder.module(module.clone());
    }
    let router = builder.build();

    let addr = config.server.listen_addr;
    let tcp = TcpListener::bind(addr)
        .await
        .with_context(|| format!("cannot listen on {addr}"))?;
    let bound = tcp.local_addr().context("cannot read the bound address")?;
    lifecycle.mark_listening();
    tracing::info!(addr = %bound, "listening");
    if let Some(sender) = on_bound {
        // A dropped receiver only means nobody is waiting for the address.
        let _ = sender.send(bound);
    }

    let background_cancel = CancellationToken::new();
    let mut supervisor = Supervisor::new(background_cancel, Some(metrics.clone()));
    for task in background {
        supervisor.spawn(task);
    }
    supervisor.spawn(metrics_upkeep(metrics, dropped));

    let stop = CancellationToken::new();
    let deadline = config.server.shutdown_timeout();
    let options = wiring::connection_options(config);
    let serving = listener::serve(tcp, router, options, stop.clone(), deadline);
    // `serving` and `controller` run side by side under `join!`. The accept loop must
    // keep being polled while the controller waits out the drain delay: awaiting the
    // delay inside a `select!` arm would stop polling the loop and leave connections
    // that arrive during the delay unanswered.
    let controller = async {
        shutdown.await;
        lifecycle.start_draining();
        tracing::info!(
            drain_delay_secs = config.server.drain_delay_secs,
            shutdown_timeout_secs = deadline.as_secs(),
            "draining: readiness is now 503"
        );
        tokio::time::sleep(config.server.drain_delay()).await;
        stop.cancel();
        supervisor.shutdown(deadline).await
    };
    let (drain, background) = tokio::join!(serving, controller);
    if drain.forced {
        tracing::warn!(
            open_connections = drain.open_at_deadline,
            "shutdown deadline passed, open connections were cut off"
        );
    }
    tracing::info!(
        accepted_connections = drain.accepted,
        stopped_tasks = background.stopped,
        aborted_tasks = background.aborted,
        "shutdown complete"
    );
    Ok(Outcome { drain, background })
}

/// Keeps the Prometheus recorder tidy and mirrors the log writer's drop counter.
fn metrics_upkeep(metrics: MetricsHandle, dropped: Option<DroppedLines>) -> BackgroundTask {
    BackgroundTask::new("metrics_upkeep", move |cancel| {
        let metrics = metrics.clone();
        let dropped = dropped.clone();
        async move {
            let mut ticker = tokio::time::interval(METRICS_UPKEEP_INTERVAL);
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                tokio::select! {
                    () = cancel.cancelled() => break,
                    _ = ticker.tick() => {
                        metrics.run_upkeep_once();
                        if let Some(dropped) = &dropped {
                            metrics.set_dropped_log_lines(dropped.get());
                        }
                    }
                }
            }
        }
    })
}

#[cfg(test)]
mod tests;
