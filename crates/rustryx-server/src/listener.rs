//! The accept loop and connection draining.
//!
//! `axum::serve` was not used because it cannot set a header-read timeout (a
//! client that trickles request headers would hold a connection for as long as it
//! likes), cannot cap open connections, and has no way to bound the drain. Here:
//!
//! - a connection permit is taken *before* `accept`, so at the cap the loop stops
//!   accepting and further clients wait in the kernel backlog instead of being
//!   accepted and dropped;
//! - `header_read_timeout` bounds the time to receive request headers (HTTP/1);
//! - on stop the listener is closed first, then every connection is told to finish
//!   its current request and close (idle keep-alive connections close at once),
//!   and whatever is still open at the deadline is cut off.
//!
//! TLS is terminated in front of the node (ingress or reverse proxy); the peer
//! address of each connection is placed in the request as `ConnectInfo`, which the
//! HTTP layer combines with `X-Forwarded-For` and its trusted proxy list.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use axum::Extension;
use axum::Router;
use axum::extract::ConnectInfo;
use hyper_util::rt::{TokioExecutor, TokioIo, TokioTimer};
use hyper_util::server::conn::auto::Builder;
use hyper_util::server::graceful::GracefulShutdown;
use hyper_util::service::TowerToHyperService;
use tokio::net::TcpListener;
use tokio::sync::Semaphore;
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;
use tower::Layer;

/// Pause after a failed `accept` (file descriptor exhaustion, for example) so the
/// loop does not spin.
const ACCEPT_ERROR_BACKOFF: Duration = Duration::from_millis(5);
/// How long to wait for connection tasks to notice a forced close.
const FORCE_GRACE: Duration = Duration::from_secs(1);

/// Connection-level settings.
#[derive(Debug, Clone, Copy)]
pub struct ConnectionOptions {
    /// Time a client may take to send request headers.
    pub header_read_timeout: Duration,
    /// Open connections at most.
    pub max_connections: usize,
}

/// What happened while serving and draining.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DrainReport {
    /// Connections accepted over the lifetime of the loop.
    pub accepted: u64,
    /// Connections still open when the drain deadline passed (cut off).
    pub open_at_deadline: usize,
    /// True when the deadline passed before all connections finished.
    pub forced: bool,
}

/// Serves `router` on `listener` until `stop` is cancelled, then drains for at most
/// `drain_deadline`.
pub async fn serve(
    listener: TcpListener,
    router: Router,
    options: ConnectionOptions,
    stop: CancellationToken,
    drain_deadline: Duration,
) -> DrainReport {
    let mut builder = Builder::new(TokioExecutor::new());
    builder
        .http1()
        .timer(TokioTimer::new())
        .header_read_timeout(options.header_read_timeout);
    builder.http2().timer(TokioTimer::new());
    let builder = Arc::new(builder);

    let graceful = GracefulShutdown::new();
    let tracker = TaskTracker::new();
    let force = CancellationToken::new();
    let permits = Arc::new(Semaphore::new(options.max_connections.max(1)));
    let mut accepted: u64 = 0;

    loop {
        let permit = tokio::select! {
            biased;
            () = stop.cancelled() => break,
            permit = permits.clone().acquire_owned() => match permit {
                Ok(permit) => permit,
                Err(_) => break,
            },
        };
        let (stream, peer) = tokio::select! {
            biased;
            () = stop.cancelled() => break,
            result = listener.accept() => match result {
                Ok(pair) => pair,
                Err(error) => {
                    tracing::warn!(error = %error, "accept failed");
                    tokio::time::sleep(ACCEPT_ERROR_BACKOFF).await;
                    continue;
                }
            },
        };
        accepted += 1;
        if let Err(error) = stream.set_nodelay(true) {
            tracing::debug!(error = %error, "cannot set TCP_NODELAY");
        }
        let service = TowerToHyperService::new(
            Extension(ConnectInfo::<SocketAddr>(peer)).layer(router.clone()),
        );
        let connection = builder
            .serve_connection(TokioIo::new(stream), service)
            .into_owned();
        let watched = graceful.watch(connection);
        let force = force.clone();
        tracker.spawn(async move {
            let _permit = permit;
            tokio::select! {
                result = watched => {
                    if let Err(error) = result {
                        tracing::debug!(error = %error, "connection ended with an error");
                    }
                }
                () = force.cancelled() => {}
            }
        });
    }

    // Close the socket first: new clients are refused from now on and a load
    // balancer sees the failure, instead of connecting into a node that is leaving.
    drop(listener);
    tracker.close();
    let drained = tokio::select! {
        () = graceful.shutdown() => true,
        () = tokio::time::sleep(drain_deadline) => false,
    };
    let open_at_deadline = if drained { 0 } else { tracker.len() };
    if !drained {
        force.cancel();
        let _ = tokio::time::timeout(FORCE_GRACE, tracker.wait()).await;
    }
    DrainReport {
        accepted,
        open_at_deadline,
        forced: !drained,
    }
}

#[cfg(test)]
mod tests;
