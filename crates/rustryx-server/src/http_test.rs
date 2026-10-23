//! Minimal raw HTTP/1.1 client for in-process tests (no HTTP client dependency).

use std::fmt::Write as _;
use std::net::SocketAddr;
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

/// A parsed response.
#[derive(Debug, Clone)]
pub struct Response {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: String,
}

impl Response {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }
}

/// Parses `HTTP/1.1 200 OK\r\nk: v\r\n\r\nbody` (body taken verbatim, no dechunking).
pub fn parse(raw: &str) -> Option<Response> {
    let (head, body) = raw.split_once("\r\n\r\n")?;
    let mut lines = head.lines();
    let status = lines.next()?.split_whitespace().nth(1)?.parse().ok()?;
    let headers = lines
        .filter_map(|l| l.split_once(':'))
        .map(|(k, v)| (k.trim().to_owned(), v.trim().to_owned()))
        .collect();
    Some(Response {
        status,
        headers,
        body: body.to_owned(),
    })
}

/// One request on a fresh connection, `Connection: close`.
pub async fn get(addr: SocketAddr, path: &str) -> std::io::Result<Response> {
    get_with(addr, path, &[]).await
}

/// Like [`get`] with extra headers.
pub async fn get_with(
    addr: SocketAddr,
    path: &str,
    headers: &[(&str, &str)],
) -> std::io::Result<Response> {
    let mut stream = TcpStream::connect(addr).await?;
    let mut request = format!("GET {path} HTTP/1.1\r\nHost: test\r\nConnection: close\r\n");
    for (k, v) in headers {
        let _ = write!(request, "{k}: {v}\r\n");
    }
    request.push_str("\r\n");
    stream.write_all(request.as_bytes()).await?;
    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).await?;
    parse(&String::from_utf8_lossy(&raw)).ok_or_else(|| std::io::Error::other("no HTTP response"))
}

/// Writes a keep-alive request on an open stream and reads one response with a
/// `Content-Length` body.
pub async fn keep_alive_get(stream: &mut TcpStream, path: &str) -> std::io::Result<Response> {
    let request = format!("GET {path} HTTP/1.1\r\nHost: test\r\n\r\n");
    stream.write_all(request.as_bytes()).await?;
    let mut raw = Vec::new();
    let mut buffer = [0_u8; 1024];
    loop {
        let n = tokio::time::timeout(Duration::from_secs(5), stream.read(&mut buffer))
            .await
            .map_err(|_| std::io::Error::other("read timed out"))??;
        if n == 0 {
            return Err(std::io::Error::other(
                "connection closed before a full response",
            ));
        }
        raw.extend_from_slice(&buffer[..n]);
        let text = String::from_utf8_lossy(&raw).into_owned();
        if let Some(response) = parse(&text) {
            let wanted: usize = response
                .header("content-length")
                .and_then(|v| v.parse().ok())
                .unwrap_or(0);
            if response.body.len() >= wanted {
                return Ok(response);
            }
        }
    }
}
