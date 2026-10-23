//! The accept loop against real sockets.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::extract::ConnectInfo;
use axum::routing::get;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Notify;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use super::*;
use crate::http_test;

struct Harness {
    addr: SocketAddr,
    stop: CancellationToken,
    task: JoinHandle<DrainReport>,
    gate: Arc<Notify>,
    entered: Arc<Notify>,
}

fn router(gate: Arc<Notify>, entered: Arc<Notify>) -> Router {
    Router::new()
        .route("/ok", get(|| async { "ok" }))
        .route(
            "/addr",
            get(|ConnectInfo(addr): ConnectInfo<SocketAddr>| async move { addr.to_string() }),
        )
        .route(
            "/gate",
            get(move || {
                let gate = gate.clone();
                let entered = entered.clone();
                async move {
                    entered.notify_one();
                    gate.notified().await;
                    "done"
                }
            }),
        )
}

async fn start(options: ConnectionOptions, deadline: Duration) -> Harness {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("addr");
    let stop = CancellationToken::new();
    let gate = Arc::new(Notify::new());
    let entered = Arc::new(Notify::new());
    let task = tokio::spawn(serve(
        listener,
        router(gate.clone(), entered.clone()),
        options,
        stop.clone(),
        deadline,
    ));
    Harness {
        addr,
        stop,
        task,
        gate,
        entered,
    }
}

fn options() -> ConnectionOptions {
    ConnectionOptions {
        header_read_timeout: Duration::from_secs(5),
        max_connections: 100,
    }
}

impl Harness {
    async fn finish(self) -> DrainReport {
        self.stop.cancel();
        tokio::time::timeout(Duration::from_secs(10), self.task)
            .await
            .expect("serve returns")
            .expect("no panic")
    }
}

#[tokio::test]
async fn serves_requests_and_exposes_the_peer_address() {
    let h = start(options(), Duration::from_secs(2)).await;
    let ok = http_test::get(h.addr, "/ok").await.expect("response");
    assert_eq!((ok.status, ok.body.as_str()), (200, "ok"));
    let addr = http_test::get(h.addr, "/addr").await.expect("response");
    assert!(addr.body.starts_with("127.0.0.1:"), "{}", addr.body);
    let report = h.finish().await;
    assert_eq!(
        report,
        DrainReport {
            accepted: 2,
            open_at_deadline: 0,
            forced: false
        }
    );
}

#[tokio::test]
async fn must_not_hold_a_connection_open_for_a_client_that_trickles_headers() {
    let h = start(
        ConnectionOptions {
            header_read_timeout: Duration::from_millis(200),
            ..options()
        },
        Duration::from_secs(2),
    )
    .await;
    let mut stream = TcpStream::connect(h.addr).await.expect("connect");
    stream
        .write_all(b"GET /ok HTTP/1.1\r\nHost: test\r\nX-Slow: ")
        .await
        .expect("partial headers");
    let mut buffer = Vec::new();
    let closed =
        tokio::time::timeout(Duration::from_secs(3), stream.read_to_end(&mut buffer)).await;
    assert!(
        closed.is_ok(),
        "the server must give up on the half-sent request"
    );
    let text = String::from_utf8_lossy(&buffer);
    assert!(
        text.is_empty() || text.starts_with("HTTP/1.1 408"),
        "{text}"
    );
    let report = h.finish().await;
    assert!(!report.forced);
}

#[tokio::test]
async fn at_the_connection_cap_further_clients_wait_instead_of_being_dropped() {
    let h = start(
        ConnectionOptions {
            max_connections: 1,
            ..options()
        },
        Duration::from_secs(2),
    )
    .await;
    let mut first = TcpStream::connect(h.addr).await.expect("connect");
    let response = http_test::keep_alive_get(&mut first, "/ok")
        .await
        .expect("first served");
    assert_eq!(response.status, 200);

    let mut second = TcpStream::connect(h.addr)
        .await
        .expect("kernel accepts into the backlog");
    second
        .write_all(b"GET /ok HTTP/1.1\r\nHost: test\r\nConnection: close\r\n\r\n")
        .await
        .expect("write");
    let mut buffer = [0_u8; 256];
    let early = tokio::time::timeout(Duration::from_millis(300), second.read(&mut buffer)).await;
    assert!(early.is_err(), "no answer while the only slot is taken");

    drop(first);
    let late = tokio::time::timeout(Duration::from_secs(3), second.read(&mut buffer)).await;
    let n = late.expect("served after the slot frees").expect("read");
    assert!(String::from_utf8_lossy(&buffer[..n]).starts_with("HTTP/1.1 200"));
    let _ = h.finish().await;
}

#[tokio::test]
async fn in_flight_requests_finish_during_a_graceful_stop_and_new_ones_are_refused() {
    let h = start(options(), Duration::from_secs(5)).await;
    let addr = h.addr;
    let client = tokio::spawn(async move { http_test::get(addr, "/gate").await });
    h.entered.notified().await;
    h.stop.cancel();

    let mut refused = false;
    for _ in 0..200 {
        if TcpStream::connect(addr).await.is_err() {
            refused = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(refused, "the listener must close when the stop starts");

    h.gate.notify_one();
    let response = client
        .await
        .expect("join")
        .expect("the in-flight request completes");
    assert_eq!((response.status, response.body.as_str()), (200, "done"));
    let report = tokio::time::timeout(Duration::from_secs(5), h.task)
        .await
        .expect("returns")
        .expect("join");
    assert_eq!(
        report,
        DrainReport {
            accepted: 1,
            open_at_deadline: 0,
            forced: false
        }
    );
}

#[tokio::test]
async fn connections_still_open_at_the_deadline_are_cut_off() {
    let h = start(options(), Duration::from_millis(200)).await;
    let addr = h.addr;
    let client = tokio::spawn(async move { http_test::get(addr, "/gate").await });
    h.entered.notified().await;
    h.stop.cancel();
    let report = tokio::time::timeout(Duration::from_secs(5), h.task)
        .await
        .expect("returns")
        .expect("join");
    assert!(report.forced);
    assert_eq!(report.open_at_deadline, 1);
    let outcome = client.await.expect("join");
    assert!(
        outcome.is_err(),
        "the client sees the connection cut, not a response"
    );
}

#[tokio::test]
async fn idle_keep_alive_connections_are_closed_at_once_when_stopping() {
    let h = start(options(), Duration::from_secs(5)).await;
    let mut stream = TcpStream::connect(h.addr).await.expect("connect");
    let response = http_test::keep_alive_get(&mut stream, "/ok")
        .await
        .expect("served");
    assert_eq!(response.status, 200);
    h.stop.cancel();
    let mut buffer = [0_u8; 16];
    let closed = tokio::time::timeout(Duration::from_secs(2), stream.read(&mut buffer)).await;
    assert!(
        matches!(closed, Ok(Ok(0) | Err(_))),
        "idle connection must close, got {closed:?}"
    );
    let report = tokio::time::timeout(Duration::from_secs(5), h.task)
        .await
        .expect("returns")
        .expect("join");
    assert!(
        !report.forced,
        "nothing in flight, so no waiting for the deadline"
    );
}

#[tokio::test]
async fn stopping_an_idle_server_returns_immediately() {
    let h = start(options(), Duration::from_secs(30)).await;
    let started = std::time::Instant::now();
    let report = h.finish().await;
    assert!(started.elapsed() < Duration::from_secs(2));
    assert_eq!(report.accepted, 0);
}
