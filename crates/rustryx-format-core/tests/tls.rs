//! TLS to the upstream is always verified (SEC-26, SEC-27): a self-signed server is refused.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "tests: a panic is the failure signal"
)]
#![allow(
    clippy::too_many_lines,
    clippy::items_after_statements,
    clippy::duration_suboptimal_units,
    clippy::case_sensitive_file_extension_comparisons,
    clippy::needless_pass_by_value,
    clippy::unused_self,
    clippy::similar_names,
    clippy::match_wild_err_arm,
    clippy::manual_assert,
    clippy::semicolon_if_nothing_returned,
    clippy::match_single_binding,
    clippy::large_futures,
    reason = "test scenarios read best as straight-line code with local helpers"
)]

mod support;

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use http::StatusCode;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use rustryx_format_core::proxy::ssrf::{HostAllowlist, UpstreamUrlPolicy};
use rustryx_format_core::proxy::upstream::ReqwestClientConfig;
use support::{World, body_text, principal};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio_rustls::TlsAcceptor;

struct TlsServer {
    port: u16,
    completed: Arc<AtomicUsize>,
    rejected: Arc<AtomicUsize>,
}

/// An HTTPS server with a freshly generated self-signed certificate for `localhost`.
async fn self_signed_server() -> TlsServer {
    let certified = rcgen::generate_simple_self_signed(vec!["localhost".to_owned()]).unwrap();
    let cert = CertificateDer::from(certified.cert.der().to_vec());
    let key = PrivateKeyDer::from(PrivatePkcs8KeyDer::from(
        certified.signing_key.serialize_der(),
    ));
    let provider = Arc::new(rustls::crypto::aws_lc_rs::default_provider());
    let config = rustls::ServerConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(vec![cert], key)
        .unwrap();
    let acceptor = TlsAcceptor::from(Arc::new(config));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let completed = Arc::new(AtomicUsize::new(0));
    let rejected = Arc::new(AtomicUsize::new(0));
    let (done, refused) = (Arc::clone(&completed), Arc::clone(&rejected));
    tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                return;
            };
            let acceptor = acceptor.clone();
            let (done, refused) = (Arc::clone(&done), Arc::clone(&refused));
            tokio::spawn(async move {
                match acceptor.accept(stream).await {
                    Ok(mut tls) => {
                        done.fetch_add(1, Ordering::SeqCst);
                        let mut buffer = [0u8; 1024];
                        let _ = tls.read(&mut buffer).await;
                        let _ = tls
                            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 6\r\n\r\ntls ok")
                            .await;
                        let _ = tls.shutdown().await;
                    }
                    Err(_) => {
                        refused.fetch_add(1, Ordering::SeqCst);
                    }
                }
            });
        }
    });
    TlsServer {
        port,
        completed,
        rejected,
    }
}

#[tokio::test]
async fn must_not_accept_a_self_signed_upstream_certificate() {
    let server = self_signed_server().await;
    // Everything about the destination is allowed; only the certificate is wrong.
    let policy = UpstreamUrlPolicy {
        hosts: HostAllowlist::AnyPublic,
        allow_cidrs: vec!["127.0.0.0/8".parse().unwrap(), "::1/128".parse().unwrap()],
        ..UpstreamUrlPolicy::default()
    };
    let world = World::real_http(policy, ReqwestClientConfig::default());
    let repo = world
        .proxy_at("tls", &format!("https://localhost:{}/", server.port))
        .await;
    world.allow_all("alice", &[&repo]);
    let response = world.get(&repo, "x.bin", &principal("alice")).await;
    assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
    assert_eq!(body_text(response).await, "RAW-ERROR upstream_unreachable");
    assert_eq!(
        server.completed.load(Ordering::SeqCst),
        0,
        "no handshake may complete with an untrusted certificate"
    );
    assert!(
        server.rejected.load(Ordering::SeqCst) >= 1,
        "the client must have reached the TLS handshake and aborted it"
    );
    assert_eq!(world.blobs.len(), 0);
}
