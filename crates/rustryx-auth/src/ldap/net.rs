//! `ldap3` backed transport: the only place that touches the network.
//!
//! Only the async API is used (`LdapConnAsync`), the connection driver is a Tokio task
//! whose handle the connection owns, and the TLS policy is fixed by the rules in
//! README 8.4:
//! - `ldaps://`, or `ldap://` upgraded with `StartTLS`, nothing else is accepted here
//!   even if a caller bypasses configuration validation;
//! - the certificate chain and the host name are verified against the roots built in
//!   `tls.rs`, and `set_no_tls_verify` is never called.

use std::collections::HashMap;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use ldap3::{
    Ldap, LdapConnAsync, LdapConnSettings, LdapError, ResultEntry, Scope, SearchEntry,
    SearchOptions, SearchResult,
};
use rustls::ClientConfig;
use tokio::task::JoinHandle;
use url::Url;

use super::config::{Endpoint, LdapSettings};
#[cfg(test)]
use super::config::{LdapConfigError, parse_endpoint};
use super::tls::{self, TlsSetupError};
use super::transport::{
    LdapConnection, LdapEntry, LdapTransport, SearchRequest, SearchResponse, SearchScope,
    TransportError,
};

/// LDAP result code `sizeLimitExceeded`.
const SIZE_LIMIT_EXCEEDED: u32 = 4;
/// Probe used as the health check: reading the root DSE needs no particular rights.
const ROOT_DSE_FILTER: &str = "(objectClass=*)";

/// Opens connections to one directory.
pub(crate) struct NetTransport {
    url: Url,
    start_tls: bool,
    tls: Arc<ClientConfig>,
    connect_timeout: Duration,
}

impl NetTransport {
    /// Builds the transport from validated settings, loading the trust material.
    pub(crate) async fn from_settings(settings: &LdapSettings) -> Result<Self, TlsSetupError> {
        let tls = tls::client_config(settings.ca_file.as_deref()).await?;
        Ok(Self::with_tls(
            &settings.endpoint,
            tls,
            settings.timeouts.connect,
        ))
    }

    fn with_tls(endpoint: &Endpoint, tls: Arc<ClientConfig>, connect_timeout: Duration) -> Self {
        Self {
            url: endpoint.url.clone(),
            start_tls: endpoint.start_tls,
            tls,
            connect_timeout,
        }
    }

    /// Builds a transport from a raw URL and an already built TLS configuration.
    ///
    /// The transport rule is enforced again here (defence in depth): this constructor
    /// is the last gate before a socket, and a plaintext target must not get past it
    /// because some caller skipped `LdapConfig::validate`.
    #[cfg(test)]
    pub(crate) fn new(
        url: &str,
        start_tls: bool,
        tls: Arc<ClientConfig>,
        connect_timeout: Duration,
    ) -> Result<Self, LdapConfigError> {
        let endpoint = parse_endpoint(url, start_tls)?;
        Ok(Self::with_tls(&endpoint, tls, connect_timeout))
    }
}

#[async_trait]
impl LdapTransport for NetTransport {
    async fn connect(&self) -> Result<Box<dyn LdapConnection>, TransportError> {
        let settings = LdapConnSettings::new()
            .set_config(Arc::clone(&self.tls))
            .set_starttls(self.start_tls)
            .set_conn_timeout(self.connect_timeout);
        let (driver, ldap) = LdapConnAsync::from_url_with_settings(settings, &self.url)
            .await
            .map_err(|error| TransportError::Connect(error.to_string()))?;
        let driver = tokio::spawn(async move {
            if let Err(error) = driver.drive().await {
                tracing::debug!(%error, "ldap connection driver stopped with an error");
            }
        });
        Ok(Box::new(NetConnection { ldap, driver }))
    }
}

/// One live connection. The driver task is aborted when the connection is dropped, so
/// every discarded connection closes its socket promptly.
struct NetConnection {
    ldap: Ldap,
    driver: JoinHandle<()>,
}

impl Drop for NetConnection {
    fn drop(&mut self) {
        self.driver.abort();
    }
}

#[async_trait]
impl LdapConnection for NetConnection {
    async fn simple_bind(&mut self, dn: &str, password: &str) -> Result<(), TransportError> {
        // The callers refuse these inputs first. This is the last line: a simple bind
        // with an empty password is an unauthenticated bind, and an empty name with a
        // password is an anonymous one; both are answered with success by many servers.
        if dn.is_empty() || password.is_empty() {
            return Err(TransportError::Protocol(
                "refusing a simple bind with an empty DN or an empty password".to_owned(),
            ));
        }
        let result = self
            .ldap
            .simple_bind(dn, password)
            .await
            .map_err(wire_error)?;
        status(result.rc, result.text)
    }

    async fn search(&mut self, request: &SearchRequest) -> Result<SearchResponse, TransportError> {
        if let Some(limit) = request.size_limit {
            self.ldap.with_search_options(
                SearchOptions::new().sizelimit(i32::try_from(limit).unwrap_or(i32::MAX)),
            );
        }
        let attributes: Vec<&str> = request.attributes.iter().map(String::as_str).collect();
        let SearchResult(entries, result) = self
            .ldap
            .search(
                &request.base,
                scope(request.scope),
                &request.filter,
                attributes,
            )
            .await
            .map_err(wire_error)?;
        let size_limit_exceeded = match result.rc {
            0 => false,
            SIZE_LIMIT_EXCEEDED => true,
            code => status(code, result.text).map(|()| false)?,
        };
        let entries = entries
            .into_iter()
            .filter(|entry| !entry.is_ref())
            .map(convert_entry)
            .collect::<Result<Vec<_>, _>>()?;
        Ok(SearchResponse {
            entries,
            size_limit_exceeded,
        })
    }

    async fn is_healthy(&mut self) -> bool {
        if self.ldap.is_closed() || self.driver.is_finished() {
            return false;
        }
        let probe = SearchRequest {
            base: String::new(),
            scope: SearchScope::Base,
            filter: ROOT_DSE_FILTER.to_owned(),
            attributes: vec!["1.1".to_owned()],
            size_limit: Some(1),
        };
        // Any answer, even a refusal, proves the peer is alive and the stream is in sync.
        matches!(
            self.search(&probe).await,
            Ok(_) | Err(TransportError::Server { .. })
        )
    }

    async fn close(mut self: Box<Self>) {
        // The driver is aborted by `Drop` right after the unbind has been sent.
        let _ = self.ldap.unbind().await;
    }
}

fn status(code: u32, message: String) -> Result<(), TransportError> {
    if code == 0 {
        Ok(())
    } else {
        Err(TransportError::Server { code, message })
    }
}

const fn scope(scope: SearchScope) -> Scope {
    match scope {
        SearchScope::Base => Scope::Base,
        SearchScope::OneLevel => Scope::OneLevel,
        SearchScope::Subtree => Scope::Subtree,
    }
}

fn wire_error(error: LdapError) -> TransportError {
    match error {
        LdapError::OpSend { .. }
        | LdapError::ResultRecv { .. }
        | LdapError::EndOfStream
        | LdapError::IdScrubSend { .. }
        | LdapError::MiscSend { .. } => TransportError::Closed,
        other => TransportError::Protocol(other.to_string()),
    }
}

fn convert_entry(entry: ResultEntry) -> Result<LdapEntry, TransportError> {
    // `SearchEntry::construct` panics on a malformed entry (a DN that is not UTF-8, a
    // broken structure). The bytes come from a server that passed TLS verification but
    // can still be buggy or compromised, and a panic must fail this lookup, not the
    // request that triggered it.
    let parsed = catch_unwind(AssertUnwindSafe(|| SearchEntry::construct(entry)))
        .map_err(|_| TransportError::Protocol("malformed search entry".to_owned()))?;
    let attributes: HashMap<String, Vec<String>> = parsed.attrs;
    let mut attributes: Vec<(String, Vec<String>)> = attributes.into_iter().collect();
    attributes.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(LdapEntry {
        dn: parsed.dn,
        attributes,
    })
}
