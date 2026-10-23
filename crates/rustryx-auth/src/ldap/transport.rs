//! The seam between the LDAP rules and the network.
//!
//! Everything this module declares is deliberately small: connect, simple bind,
//! search, probe, close. The rules (input checks, one entry per user search, user
//! bind on a throwaway connection, group walk limits, timeouts) live above the seam
//! and are therefore testable with a fake that never opens a socket.
//!
//! Timeouts are NOT the transport's job. The callers wrap every call in
//! `tokio::time::timeout`, so a transport that hangs is cut off no matter how it
//! hangs, and the test fake can prove it by never returning.

use async_trait::async_trait;

/// Scope of a search.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchScope {
    /// Only the base object.
    Base,
    /// Direct children of the base object.
    OneLevel,
    /// The base object and everything below it.
    Subtree,
}

/// One search request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchRequest {
    /// Search base DN.
    pub base: String,
    /// Search scope.
    pub scope: SearchScope,
    /// Filter in RFC 4515 string form. Built only by `FilterTemplate::render`.
    pub filter: String,
    /// Attributes to return. `["1.1"]` asks for no attributes, only DNs.
    pub attributes: Vec<String>,
    /// Maximum number of entries the server should return.
    pub size_limit: Option<u32>,
}

/// One entry of a search result.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct LdapEntry {
    /// Entry DN exactly as the server returned it.
    pub dn: String,
    /// Attributes as returned. Names keep the server's spelling; use [`LdapEntry::values`]
    /// for a case insensitive lookup.
    pub attributes: Vec<(String, Vec<String>)>,
}

/// What an attribute lookup found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AttributeValues<'a> {
    /// The attribute is absent from the entry.
    Absent,
    /// The complete value list.
    Complete(Vec<&'a str>),
    /// The server returned a ranged slice (`name;range=0-1499`): the list is
    /// incomplete and must not be mistaken for the whole membership.
    Ranged,
}

impl LdapEntry {
    /// Builds an entry with no attributes.
    #[must_use]
    pub fn new(dn: impl Into<String>) -> Self {
        Self {
            dn: dn.into(),
            attributes: Vec::new(),
        }
    }

    /// Returns the entry with one more attribute.
    #[must_use]
    pub fn with_attribute<I, S>(mut self, name: &str, values: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.attributes.push((
            name.to_owned(),
            values.into_iter().map(Into::into).collect(),
        ));
        self
    }

    /// Values of `name`, matched case insensitively as LDAP attribute names are.
    ///
    /// A ranged variant (`member;range=0-1499`) yields [`AttributeValues::Ranged`]
    /// because Active Directory silently splits large multi valued attributes that
    /// way and a caller that took the first slice as the whole list would grant
    /// fewer roles without any error.
    #[must_use]
    pub fn values(&self, name: &str) -> AttributeValues<'_> {
        let mut found: Option<Vec<&str>> = None;
        for (attribute, values) in &self.attributes {
            let (base, options) = attribute
                .split_once(';')
                .map_or((attribute.as_str(), ""), |(b, o)| (b, o));
            if !base.eq_ignore_ascii_case(name) {
                continue;
            }
            if options.to_ascii_lowercase().starts_with("range=") {
                return AttributeValues::Ranged;
            }
            found
                .get_or_insert_with(Vec::new)
                .extend(values.iter().map(String::as_str));
        }
        found.map_or(AttributeValues::Absent, AttributeValues::Complete)
    }

    /// First non empty value of `name`, if the attribute is present and complete.
    #[must_use]
    pub fn first_value(&self, name: &str) -> Option<&str> {
        match self.values(name) {
            AttributeValues::Complete(values) => values.into_iter().find(|value| !value.is_empty()),
            AttributeValues::Absent | AttributeValues::Ranged => None,
        }
    }
}

/// Result of a search that completed with a usable result code.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SearchResponse {
    /// Entries returned.
    pub entries: Vec<LdapEntry>,
    /// The server stopped at a size limit (result code 4): there may be more entries.
    pub size_limit_exceeded: bool,
}

/// Failure of a transport operation. Messages are for logs; they can carry server
/// diagnostics and DNs, so they never travel into an `AuthError` shown to a client.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TransportError {
    /// The connection could not be established, or TLS failed (certificate or
    /// hostname verification included).
    #[error("connect failed: {0}")]
    Connect(String),
    /// The connection is gone.
    #[error("connection closed")]
    Closed,
    /// The peer or the local codec violated the protocol, or a request was refused
    /// locally before it reached the wire.
    #[error("ldap protocol error: {0}")]
    Protocol(String),
    /// The server answered with a non-success result code.
    #[error("ldap result code {code}: {message}")]
    Server {
        /// LDAP result code (RFC 4511 appendix A.1).
        code: u32,
        /// Diagnostic text from the server.
        message: String,
    },
}

impl TransportError {
    /// The LDAP result code, when the server answered with one.
    #[must_use]
    pub const fn result_code(&self) -> Option<u32> {
        match self {
            Self::Server { code, .. } => Some(*code),
            Self::Connect(_) | Self::Closed | Self::Protocol(_) => None,
        }
    }
}

/// LDAP result codes the rules above the seam care about.
pub(crate) mod result_code {
    /// `noSuchObject`.
    pub(crate) const NO_SUCH_OBJECT: u32 = 32;
}

/// One open LDAP connection.
///
/// A connection is owned by exactly one task at a time. The fake and the real
/// implementation both treat dropping the box as "close without unbind".
#[async_trait]
pub trait LdapConnection: Send {
    /// Simple bind. Cancel safe only in the sense that a cancelled bind leaves the
    /// connection in an unknown state, so the caller discards it.
    ///
    /// Implementations refuse an empty DN or an empty password themselves: a simple
    /// bind with an empty password is an unauthenticated bind that many servers
    /// answer with success.
    async fn simple_bind(&mut self, dn: &str, password: &str) -> Result<(), TransportError>;

    /// Runs one search. A non-success result code other than size limit exceeded is
    /// an `Err(TransportError::Server)`.
    async fn search(&mut self, request: &SearchRequest) -> Result<SearchResponse, TransportError>;

    /// True when the connection still answers. Used as the health check on pool
    /// checkout: any answer from the server counts, only silence or a dead socket
    /// is "unhealthy".
    async fn is_healthy(&mut self) -> bool;

    /// Unbinds and closes the connection.
    async fn close(self: Box<Self>);
}

/// Factory of connections: the TLS policy lives here.
#[async_trait]
pub trait LdapTransport: Send + Sync {
    /// Opens a new, unbound connection. Cancel safe: dropping the future closes
    /// whatever was half open.
    async fn connect(&self) -> Result<Box<dyn LdapConnection>, TransportError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry() -> LdapEntry {
        LdapEntry::new("uid=a,dc=x")
            .with_attribute("memberOf", ["cn=g1,dc=x", "cn=g2,dc=x"])
            .with_attribute("uid", ["a"])
            .with_attribute("empty", [""])
    }

    #[test]
    fn attribute_lookup_ignores_case() {
        let e = entry();
        assert_eq!(
            e.values("MEMBEROF"),
            AttributeValues::Complete(vec!["cn=g1,dc=x", "cn=g2,dc=x"])
        );
        assert_eq!(e.values("missing"), AttributeValues::Absent);
        assert_eq!(e.first_value("UID"), Some("a"));
        assert_eq!(e.first_value("empty"), None);
        assert_eq!(e.first_value("missing"), None);
    }

    #[test]
    fn must_not_treat_a_ranged_attribute_as_the_complete_list() {
        let e =
            LdapEntry::new("uid=a,dc=x").with_attribute("memberOf;range=0-1499", ["cn=g1,dc=x"]);
        assert_eq!(e.values("memberOf"), AttributeValues::Ranged);
        assert_eq!(e.first_value("memberOf"), None);
    }

    #[test]
    fn attribute_options_other_than_range_are_merged() {
        let e = LdapEntry::new("uid=a,dc=x")
            .with_attribute("cn", ["a"])
            .with_attribute("cn;lang-ru", ["b"]);
        assert_eq!(e.values("cn"), AttributeValues::Complete(vec!["a", "b"]));
    }

    #[test]
    fn result_code_is_only_present_for_server_errors() {
        let server = TransportError::Server {
            code: 49,
            message: "x".into(),
        };
        assert_eq!(server.result_code(), Some(49));
        assert_eq!(TransportError::Closed.result_code(), None);
        assert_eq!(TransportError::Connect("x".into()).result_code(), None);
        assert_eq!(TransportError::Protocol("x".into()).result_code(), None);
        assert!(server.to_string().contains("49"));
    }
}
