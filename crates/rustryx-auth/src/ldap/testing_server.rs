//! A very small LDAP server for tests of the real transport.
//!
//! It speaks just enough BER to answer bind, search, unbind and the `StartTLS` extended
//! operation, over plain TCP, LDAPS or `StartTLS`. Behaviour is scripted with closures and
//! every request is recorded, so a test can assert what actually crossed the wire (for
//! example that no bind with an empty password was ever sent) and what the TLS
//! handshake did with an untrusted certificate.

use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use bytes::BytesMut;
use ldap3::asn1::{
    ASNTag, Enumerated, Integer, OctetString, PL, Sequence, Set, StructureTag, Tag, TagClass,
    parse_tag, write,
};
use rustls::ServerConfig;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::task::{JoinHandle, JoinSet};
use tokio_rustls::TlsAcceptor;

const OP_BIND_REQUEST: u64 = 0;
const OP_BIND_RESPONSE: u64 = 1;
const OP_UNBIND_REQUEST: u64 = 2;
const OP_SEARCH_REQUEST: u64 = 3;
const OP_SEARCH_ENTRY: u64 = 4;
const OP_SEARCH_DONE: u64 = 5;
const OP_EXTENDED_REQUEST: u64 = 23;
const OP_EXTENDED_RESPONSE: u64 = 24;
const START_TLS_OID: &[u8] = b"1.3.6.1.4.1.1466.20037";

/// How the server secures connections.
#[derive(Clone)]
pub(crate) enum Security {
    /// TLS from the first byte (`ldaps://`).
    Ldaps(Arc<ServerConfig>),
    /// Plain until the client sends `StartTLS`.
    StartTls(Arc<ServerConfig>),
    /// Answers `StartTLS` with this result code and stays in plain text.
    StartTlsRefused(u32),
}

/// A search as it arrived on the wire.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct WireSearch {
    pub(crate) base: String,
    pub(crate) scope: i64,
    pub(crate) size_limit: i64,
    /// `(attr=value)` or `(attr=*)` for the two filter shapes the tests send.
    pub(crate) filter: String,
    pub(crate) attributes: Vec<String>,
}

/// An entry the server returns.
#[derive(Debug, Clone)]
pub(crate) struct WireEntry {
    pub(crate) dn: String,
    pub(crate) attributes: Vec<(String, Vec<String>)>,
}

/// What the server answers to a search.
pub(crate) enum SearchReply {
    Entries {
        entries: Vec<WireEntry>,
        rc: u32,
    },
    /// An entry whose DN is not UTF-8, to prove the client survives a malformed server.
    MalformedEntry,
    /// Drop the connection without answering.
    Disconnect,
}

pub(crate) type BindHandler = Arc<dyn Fn(&str, &str) -> u32 + Send + Sync>;
pub(crate) type SearchHandler = Arc<dyn Fn(&WireSearch) -> SearchReply + Send + Sync>;

/// Everything the server saw.
#[derive(Default)]
pub(crate) struct ServerLog {
    pub(crate) binds: Mutex<Vec<(String, String)>>,
    pub(crate) searches: Mutex<Vec<WireSearch>>,
    pub(crate) connections: AtomicUsize,
    pub(crate) handshake_failures: AtomicUsize,
    pub(crate) start_tls_requests: AtomicUsize,
    pub(crate) unbinds: AtomicUsize,
}

struct Context {
    security: Security,
    bind: BindHandler,
    search: SearchHandler,
    log: Arc<ServerLog>,
}

/// A running test server. Dropping it stops it and closes its connections.
pub(crate) struct MiniLdap {
    addr: SocketAddr,
    pub(crate) log: Arc<ServerLog>,
    task: JoinHandle<()>,
}

impl MiniLdap {
    pub(crate) async fn start(
        security: Security,
        bind: BindHandler,
        search: SearchHandler,
    ) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let log = Arc::new(ServerLog::default());
        let context = Arc::new(Context {
            security,
            bind,
            search,
            log: Arc::clone(&log),
        });
        let task = tokio::spawn(async move {
            // The set owns the per connection tasks: aborting this task drops it, which
            // aborts every connection.
            let mut connections = JoinSet::new();
            while let Ok((tcp, _)) = listener.accept().await {
                context.log.connections.fetch_add(1, Ordering::SeqCst);
                connections.spawn(connection(tcp, Arc::clone(&context)));
            }
        });
        Self { addr, log, task }
    }

    pub(crate) fn port(&self) -> u16 {
        self.addr.port()
    }

    pub(crate) fn binds(&self) -> Vec<(String, String)> {
        self.log.binds.lock().unwrap().clone()
    }

    pub(crate) fn searches(&self) -> Vec<WireSearch> {
        self.log.searches.lock().unwrap().clone()
    }
}

impl Drop for MiniLdap {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// Convenience: a server that accepts the password `good` for every DN and answers every
/// search with `entries`.
pub(crate) async fn simple_server(security: Security, entries: Vec<WireEntry>) -> MiniLdap {
    MiniLdap::start(
        security,
        Arc::new(|_, password| if password == "good" { 0 } else { 49 }),
        Arc::new(move |_| SearchReply::Entries {
            entries: entries.clone(),
            rc: 0,
        }),
    )
    .await
}

async fn connection(tcp: TcpStream, context: Arc<Context>) {
    match context.security.clone() {
        Security::StartTlsRefused(_) => {
            serve(tcp, &context, false).await;
        }
        Security::Ldaps(config) => {
            if let Some(tls) = handshake(tcp, config, &context).await {
                serve(tls, &context, false).await;
            }
        }
        Security::StartTls(config) => {
            if let Some(tcp) = serve(tcp, &context, true).await
                && let Some(tls) = handshake(tcp, config, &context).await
            {
                serve(tls, &context, false).await;
            }
        }
    }
}

async fn handshake(
    tcp: TcpStream,
    config: Arc<ServerConfig>,
    context: &Context,
) -> Option<tokio_rustls::server::TlsStream<TcpStream>> {
    let Ok(tls) = TlsAcceptor::from(config).accept(tcp).await else {
        context
            .log
            .handshake_failures
            .fetch_add(1, Ordering::SeqCst);
        return None;
    };
    Some(tls)
}

enum Flow {
    Continue,
    Close,
    Upgrade,
}

/// Serves one connection. Returns the stream when the client asked for `StartTLS` and the
/// server agreed, so the caller can run the handshake on it.
async fn serve<S>(mut stream: S, context: &Context, allow_start_tls: bool) -> Option<S>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let mut buffer: Vec<u8> = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        loop {
            let (consumed, tag) = match parse_tag(&buffer) {
                Ok((rest, tag)) => (buffer.len() - rest.len(), tag),
                Err(error) if error.is_incomplete() => break,
                Err(_) => return None,
            };
            buffer.drain(..consumed);
            let (id, request) = decode(&tag)?;
            match respond(&mut stream, context, id, request, allow_start_tls).await {
                Flow::Continue => {}
                Flow::Close => return None,
                Flow::Upgrade => return Some(stream),
            }
        }
        let read = stream.read(&mut chunk).await.ok()?;
        if read == 0 {
            return None;
        }
        buffer.extend_from_slice(&chunk[..read]);
    }
}

enum Request {
    Bind { dn: String, password: String },
    Search(WireSearch),
    Unbind,
    StartTls,
    Other,
}

async fn respond<S>(
    stream: &mut S,
    context: &Context,
    id: i64,
    request: Request,
    allow_start_tls: bool,
) -> Flow
where
    S: AsyncWrite + Unpin,
{
    let reply: Vec<u8> = match request {
        Request::Unbind => {
            context.log.unbinds.fetch_add(1, Ordering::SeqCst);
            return Flow::Close;
        }
        Request::Other => return Flow::Close,
        Request::Bind { dn, password } => {
            let rc = (context.bind)(&dn, &password);
            context.log.binds.lock().unwrap().push((dn, password));
            message(id, result_op(OP_BIND_RESPONSE, rc, ""))
        }
        Request::StartTls => {
            context
                .log
                .start_tls_requests
                .fetch_add(1, Ordering::SeqCst);
            let (rc, upgrade) = match (&context.security, allow_start_tls) {
                (Security::StartTls(_), true) => (0, true),
                (Security::StartTlsRefused(rc), _) => (*rc, false),
                _ => (2, false),
            };
            let bytes = message(id, result_op(OP_EXTENDED_RESPONSE, rc, ""));
            if stream.write_all(&bytes).await.is_err() {
                return Flow::Close;
            }
            return if upgrade {
                Flow::Upgrade
            } else {
                Flow::Continue
            };
        }
        Request::Search(search) => {
            let reply = (context.search)(&search);
            context.log.searches.lock().unwrap().push(search);
            match reply {
                SearchReply::Disconnect => return Flow::Close,
                SearchReply::MalformedEntry => {
                    let mut bytes = message(id, malformed_entry());
                    bytes.extend(message(id, result_op(OP_SEARCH_DONE, 0, "")));
                    bytes
                }
                SearchReply::Entries { entries, rc } => {
                    let mut bytes = Vec::new();
                    for entry in &entries {
                        bytes.extend(message(id, entry_op(entry)));
                    }
                    bytes.extend(message(id, result_op(OP_SEARCH_DONE, rc, "")));
                    bytes
                }
            }
        }
    };
    if stream.write_all(&reply).await.is_err() {
        return Flow::Close;
    }
    Flow::Continue
}

fn decode(message: &StructureTag) -> Option<(i64, Request)> {
    let PL::C(parts) = &message.payload else {
        return None;
    };
    let id = integer(primitive(parts.first()?)?);
    let op = parts.get(1)?;
    let request = match op.id {
        OP_BIND_REQUEST => {
            let PL::C(fields) = &op.payload else {
                return None;
            };
            Request::Bind {
                dn: text(primitive(fields.get(1)?)?),
                password: text(primitive(fields.get(2)?)?),
            }
        }
        OP_UNBIND_REQUEST => Request::Unbind,
        OP_SEARCH_REQUEST => Request::Search(decode_search(op)?),
        OP_EXTENDED_REQUEST => {
            let PL::C(fields) = &op.payload else {
                return None;
            };
            if primitive(fields.first()?)? == START_TLS_OID {
                Request::StartTls
            } else {
                Request::Other
            }
        }
        _ => Request::Other,
    };
    Some((id, request))
}

fn decode_search(op: &StructureTag) -> Option<WireSearch> {
    let PL::C(fields) = &op.payload else {
        return None;
    };
    let attributes = match &fields.get(7)?.payload {
        PL::C(items) => items
            .iter()
            .filter_map(|item| primitive(item).map(text))
            .collect(),
        PL::P(_) => Vec::new(),
    };
    Some(WireSearch {
        base: text(primitive(fields.first()?)?),
        scope: integer(primitive(fields.get(1)?)?),
        size_limit: integer(primitive(fields.get(3)?)?),
        filter: describe_filter(fields.get(6)?),
        attributes,
    })
}

fn describe_filter(filter: &StructureTag) -> String {
    match (&filter.payload, filter.id) {
        (PL::C(parts), 3) => match (
            parts.first().and_then(primitive),
            parts.get(1).and_then(primitive),
        ) {
            (Some(attribute), Some(value)) => format!("({}={})", text(attribute), text(value)),
            _ => "(?)".to_owned(),
        },
        (PL::P(attribute), 7) => format!("({}=*)", text(attribute)),
        _ => "(?)".to_owned(),
    }
}

fn primitive(tag: &StructureTag) -> Option<&[u8]> {
    match &tag.payload {
        PL::P(bytes) => Some(bytes),
        PL::C(_) => None,
    }
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

fn integer(bytes: &[u8]) -> i64 {
    let negative = bytes.first().is_some_and(|byte| byte & 0x80 != 0);
    bytes
        .iter()
        .fold(if negative { -1 } else { 0 }, |acc, byte| {
            (acc << 8) | i64::from(*byte)
        })
}

fn message(id: i64, op: Tag) -> Vec<u8> {
    let envelope = Tag::Sequence(Sequence {
        inner: vec![
            Tag::Integer(Integer {
                inner: id,
                ..Default::default()
            }),
            op,
        ],
        ..Default::default()
    });
    let mut buffer = BytesMut::new();
    write::encode_into(&mut buffer, envelope.into_structure()).unwrap();
    buffer.to_vec()
}

fn octets(bytes: &[u8]) -> Tag {
    Tag::OctetString(OctetString {
        inner: bytes.to_vec(),
        ..Default::default()
    })
}

fn result_op(op_id: u64, rc: u32, diagnostic: &str) -> Tag {
    Tag::Sequence(Sequence {
        id: op_id,
        class: TagClass::Application,
        inner: vec![
            Tag::Enumerated(Enumerated {
                inner: i64::from(rc),
                ..Default::default()
            }),
            octets(b""),
            octets(diagnostic.as_bytes()),
        ],
    })
}

fn entry_op(entry: &WireEntry) -> Tag {
    let attributes = entry
        .attributes
        .iter()
        .map(|(name, values)| {
            Tag::Sequence(Sequence {
                inner: vec![
                    octets(name.as_bytes()),
                    Tag::Set(Set {
                        inner: values
                            .iter()
                            .map(|value| octets(value.as_bytes()))
                            .collect(),
                        ..Default::default()
                    }),
                ],
                ..Default::default()
            })
        })
        .collect();
    Tag::Sequence(Sequence {
        id: OP_SEARCH_ENTRY,
        class: TagClass::Application,
        inner: vec![
            octets(entry.dn.as_bytes()),
            Tag::Sequence(Sequence {
                inner: attributes,
                ..Default::default()
            }),
        ],
    })
}

fn malformed_entry() -> Tag {
    Tag::Sequence(Sequence {
        id: OP_SEARCH_ENTRY,
        class: TagClass::Application,
        inner: vec![
            octets(&[0xff, 0xfe, 0xfd]),
            Tag::Sequence(Sequence {
                inner: Vec::new(),
                ..Default::default()
            }),
        ],
    })
}
