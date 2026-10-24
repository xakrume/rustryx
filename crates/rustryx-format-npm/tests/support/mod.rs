//! Shared helpers of the integration tests: a server with the npm handler, publish documents
//! written the way `npm publish` writes them (key order matters to the registry), and a few
//! small tools.
#![allow(
    dead_code,
    reason = "each integration test binary uses a different subset of the helpers"
)]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::too_many_lines,
    clippy::similar_names,
    clippy::missing_panics_doc,
    clippy::must_use_candidate,
    clippy::needless_pass_by_value,
    clippy::items_after_statements,
    clippy::large_futures,
    reason = "tests: a panic is the failure signal and scenarios read best as straight-line code"
)]

use std::sync::Arc;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use rustryx_core::model::RepoFormat;
use rustryx_format_npm::{NpmConfig, NpmHandler};
use rustryx_testserver::reqwest::{self, Method, RequestBuilder, Response};
use rustryx_testserver::{NewTestRepo, TestServer, TestServerBuilder, TestUser, http_client};
use serde_json::{Value, json};
use sha1::Sha1;
use sha2::{Digest, Sha256, Sha512};

pub const HOSTED: &str = "npm-hosted";

/// A server with the handler registered and a hosted repository, `alice` (developer: write, no
/// delete), `admin` (everything) and `reader` (read only).
pub struct Fixture {
    pub server: TestServer,
    pub http: reqwest::Client,
    pub alice: TestUser,
    pub admin: TestUser,
    pub reader: TestUser,
}

pub fn handler() -> Arc<NpmHandler> {
    Arc::new(NpmHandler::new())
}

pub async fn fixture() -> Fixture {
    fixture_with(TestServer::builder().register(handler())).await
}

pub async fn fixture_with(builder: TestServerBuilder) -> Fixture {
    let server = builder.start().await;
    server
        .add_repository(NewTestRepo::hosted(HOSTED, RepoFormat::Npm))
        .await;
    users(server).await
}

pub async fn users(server: TestServer) -> Fixture {
    let alice = server.add_user("alice", &["nx-developer"]).await;
    let admin = server.add_user("admin", &["nx-admin"]).await;
    // A role that reads everything and writes nothing.
    server
        .add_role_with_privileges(
            "npm-reader",
            &[rustryx_testserver::TestPrivilege::repository_view(
                "*",
                &rustryx_testserver::READ,
            )],
        )
        .await;
    let reader = server.add_user("reader", &["npm-reader"]).await;
    Fixture {
        server,
        http: http_client(),
        alice,
        admin,
        reader,
    }
}

pub fn config_with(adjust: impl FnOnce(&mut NpmConfig)) -> Arc<NpmHandler> {
    let mut config = NpmConfig::default();
    adjust(&mut config);
    Arc::new(NpmHandler::with_config(config))
}

impl Fixture {
    pub fn url(&self, repository: &str, path: &str) -> String {
        format!("{}/{path}", self.server.repository_url(repository))
    }

    pub fn hosted(&self, path: &str) -> String {
        self.url(HOSTED, path)
    }

    pub fn request(&self, method: Method, url: &str, user: Option<&TestUser>) -> RequestBuilder {
        let builder = self.http.request(method, url);
        match user {
            Some(user) => builder.header("authorization", user.bearer_header()),
            None => builder,
        }
    }

    pub async fn get(&self, url: &str, user: Option<&TestUser>) -> Response {
        self.request(Method::GET, url, user).send().await.unwrap()
    }

    pub async fn get_json(&self, url: &str, user: Option<&TestUser>) -> (u16, Value) {
        let response = self.get(url, user).await;
        let status = response.status().as_u16();
        let body = response.bytes().await.unwrap();
        (status, serde_json::from_slice(&body).unwrap_or(Value::Null))
    }

    pub async fn put(
        &self,
        url: &str,
        user: Option<&TestUser>,
        body: impl Into<reqwest::Body>,
    ) -> Response {
        self.request(Method::PUT, url, user)
            .header("content-type", "application/json")
            .body(body)
            .send()
            .await
            .unwrap()
    }

    pub async fn delete(&self, url: &str, user: Option<&TestUser>) -> Response {
        self.request(Method::DELETE, url, user)
            .send()
            .await
            .unwrap()
    }

    /// Publishes to the hosted repository as `user` and returns the status.
    pub async fn publish(&self, user: &TestUser, publish: &Publish) -> u16 {
        let url = self.hosted(&publish.package_path());
        self.put(&url, Some(user), publish.body())
            .await
            .status()
            .as_u16()
    }

    /// The full document of a package of the hosted repository, read as `admin`.
    pub async fn packument(&self, package_path: &str) -> Value {
        let (status, body) = self
            .get_json(&self.hosted(package_path), Some(&self.admin))
            .await;
        assert_eq!(status, 200, "{body}");
        body
    }
}

/// Deterministic bytes that look like nothing in particular.
pub fn bytes_of(seed: u8, len: usize) -> Vec<u8> {
    let mut state = u32::from(seed).wrapping_mul(2_654_435_761).wrapping_add(1);
    (0..len)
        .map(|_| {
            state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            (state >> 24) as u8
        })
        .collect()
}

pub fn sha1_hex(bytes: &[u8]) -> String {
    hex::encode(Sha1::digest(bytes))
}

pub fn sri512(bytes: &[u8]) -> String {
    format!("sha512-{}", STANDARD.encode(Sha512::digest(bytes)))
}

pub fn sri256(bytes: &[u8]) -> String {
    format!("sha256-{}", STANDARD.encode(Sha256::digest(bytes)))
}

/// A publish document in the shape and the key order `npm publish` sends.
#[derive(Clone)]
pub struct Publish {
    pub name: String,
    pub version: String,
    pub tarball: Vec<u8>,
    pub tags: Vec<String>,
    pub extra_manifest: Value,
    pub shasum: Option<String>,
    pub integrity: Option<String>,
    pub attachment: Option<String>,
    pub data: Option<String>,
}

impl Publish {
    pub fn new(name: &str, version: &str) -> Self {
        let seed = u8::try_from(version.len() % 256)
            .unwrap()
            .wrapping_add(u8::try_from(name.len() % 256).unwrap());
        let tarball = bytes_of(seed, 2048);
        Self {
            name: name.to_owned(),
            version: version.to_owned(),
            shasum: Some(sha1_hex(&tarball)),
            integrity: Some(sri512(&tarball)),
            tarball,
            tags: vec!["latest".to_owned()],
            extra_manifest: json!({}),
            attachment: None,
            data: None,
        }
    }

    pub fn tarball(mut self, tarball: Vec<u8>) -> Self {
        self.shasum = Some(sha1_hex(&tarball));
        self.integrity = Some(sri512(&tarball));
        self.tarball = tarball;
        self
    }

    pub fn tags(mut self, tags: &[&str]) -> Self {
        self.tags = tags.iter().map(|t| (*t).to_owned()).collect();
        self
    }

    pub fn manifest(mut self, extra: Value) -> Self {
        self.extra_manifest = extra;
        self
    }

    pub fn shasum(mut self, shasum: Option<String>) -> Self {
        self.shasum = shasum;
        self
    }

    pub fn integrity(mut self, integrity: Option<String>) -> Self {
        self.integrity = integrity;
        self
    }

    pub fn attachment(mut self, name: &str) -> Self {
        self.attachment = Some(name.to_owned());
        self
    }

    /// The raw `data` string instead of the base64 of the tarball.
    pub fn data(mut self, data: &str) -> Self {
        self.data = Some(data.to_owned());
        self
    }

    /// The URL path below the repository: `name` or `@scope%2fname`, as npm writes it.
    pub fn package_path(&self) -> String {
        self.name.replacen('/', "%2f", 1)
    }

    pub fn attachment_name(&self) -> String {
        self.attachment
            .clone()
            .unwrap_or_else(|| format!("{}-{}.tgz", self.name, self.version))
    }

    pub fn manifest_json(&self) -> String {
        let mut manifest = json!({
            "name": self.name,
            "version": self.version,
            "description": format!("{} {}", self.name, self.version),
            "main": "index.js",
            "readme": format!("# {}\n", self.name),
        });
        let mut dist = serde_json::Map::new();
        if let Some(shasum) = &self.shasum {
            dist.insert("shasum".to_owned(), json!(shasum));
        }
        if let Some(integrity) = &self.integrity {
            dist.insert("integrity".to_owned(), json!(integrity));
        }
        dist.insert(
            "tarball".to_owned(),
            json!(format!("http://elsewhere.invalid/{}/-/x.tgz", self.name)),
        );
        manifest["dist"] = Value::Object(dist);
        manifest["_id"] = json!(format!("{}@{}", self.name, self.version));
        if let (Some(base), Some(extra)) =
            (manifest.as_object_mut(), self.extra_manifest.as_object())
        {
            base.extend(extra.clone());
        }
        manifest.to_string()
    }

    /// The document, key by key in the order of `libnpmpublish`.
    pub fn body(&self) -> String {
        let tags: serde_json::Map<String, Value> = self
            .tags
            .iter()
            .map(|t| (t.clone(), json!(self.version)))
            .collect();
        let data = self
            .data
            .clone()
            .unwrap_or_else(|| STANDARD.encode(&self.tarball));
        format!(
            r#"{{"_id":{id},"name":{id},"description":"a package","dist-tags":{tags},"versions":{{{version}:{manifest}}},"access":null,"_attachments":{{{attachment}:{{"content_type":"application/octet-stream","data":"{data}","length":{length}}}}}}}"#,
            id = json!(self.name),
            tags = Value::Object(tags),
            version = json!(self.version),
            manifest = self.manifest_json(),
            attachment = json!(self.attachment_name()),
            length = self.tarball.len(),
        )
    }
}

/// A minimal ustar archive (not compressed) with one file whose name tries to leave the
/// extraction directory. The registry must store it like any other bytes and never open it.
pub fn tar_with_traversal_name() -> Vec<u8> {
    let name = b"../../../../tmp/rustryx-must-not-exist";
    let content = b"payload";
    let mut header = [0u8; 512];
    header[..name.len()].copy_from_slice(name);
    header[100..107].copy_from_slice(b"0000644");
    header[108..115].copy_from_slice(b"0000000");
    header[116..123].copy_from_slice(b"0000000");
    header[124..135].copy_from_slice(format!("{:011o}", content.len()).as_bytes());
    header[136..147].copy_from_slice(b"00000000000");
    header[148..156].copy_from_slice(b"        ");
    header[156] = b'0';
    header[257..262].copy_from_slice(b"ustar");
    header[263..265].copy_from_slice(b"00");
    let checksum: u32 = header.iter().map(|b| u32::from(*b)).sum();
    header[148..155].copy_from_slice(format!("{checksum:06o}\0").as_bytes());
    let mut archive = header.to_vec();
    archive.extend_from_slice(content);
    archive.resize(archive.len() + (512 - content.len()), 0);
    archive.resize(archive.len() + 1024, 0);
    archive
}

/// Resident set size of this process in KiB.
pub fn rss_kib() -> u64 {
    let pid = std::process::id().to_string();
    std::process::Command::new("ps")
        .args(["-o", "rss=", "-p", &pid])
        .output()
        .ok()
        .and_then(|out| String::from_utf8(out.stdout).ok())
        .and_then(|text| text.trim().parse().ok())
        .unwrap_or(0)
}

/// The components the metadata store holds for the hosted repository.
pub async fn component_names(fixture: &Fixture) -> Vec<String> {
    use rustryx_core::ports::{ComponentFilter, ComponentRepo, PageRequest, RepositoryRepo};
    let repositories: Arc<dyn RepositoryRepo> = Arc::new(fixture.server.store().clone());
    let repository = repositories
        .get_by_name(&rustryx_core::names::RepoName::parse(HOSTED).unwrap())
        .await
        .unwrap()
        .unwrap();
    let components: Arc<dyn ComponentRepo> = Arc::new(fixture.server.store().clone());
    let page = components
        .list(
            &ComponentFilter {
                repository_id: Some(repository.id),
                ..ComponentFilter::default()
            },
            &PageRequest {
                limit: 500,
                after: None,
            },
        )
        .await
        .unwrap();
    page.items
        .iter()
        .map(|c| match &c.group_id {
            Some(g) => format!("@{g}/{}@{}", c.name, c.version.clone().unwrap_or_default()),
            None => format!("{}@{}", c.name, c.version.clone().unwrap_or_default()),
        })
        .collect()
}

/// What [`raw`] got back.
pub struct Raw {
    pub status: u16,
    pub head: String,
    pub body: Vec<u8>,
}

/// One HTTP/1.1 request written by hand. The HTTP client of the harness (like every URL parser
/// that follows the WHATWG rules) resolves `..` and `%2e%2e` before it sends anything, so the
/// path traversal vectors have to be put on the wire as they are.
pub async fn raw(
    server: &TestServer,
    method: &str,
    target: &str,
    headers: &[(&str, &str)],
    body: &[u8],
) -> Raw {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let mut stream = tokio::net::TcpStream::connect(("127.0.0.1", server.port()))
        .await
        .unwrap();
    let mut request = format!(
        "{method} {target} HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nConnection: close\r\nContent-Length: {}\r\n",
        server.port(),
        body.len()
    );
    for (name, value) in headers {
        use std::fmt::Write as _;
        write!(request, "{name}: {value}\r\n").unwrap();
    }
    request.push_str("\r\n");
    stream.write_all(request.as_bytes()).await.unwrap();
    stream.write_all(body).await.unwrap();
    let mut response = Vec::new();
    stream.read_to_end(&mut response).await.unwrap();
    let split = response
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .map_or(response.len(), |at| at + 4);
    let head = String::from_utf8_lossy(&response[..split]).into_owned();
    let status = head
        .split_whitespace()
        .nth(1)
        .and_then(|code| code.parse().ok())
        .unwrap_or(0);
    Raw {
        status,
        head,
        body: response[split..].to_vec(),
    }
}
