//! Strict, panic-free parsers for everything a registry client or an upstream registry
//! sends: names, references, digests, media types, manifests, pagination, upload session
//! ids, token scopes and the routes below `/v2/`.
//!
//! Every parser takes `&str` or `&[u8]`, walks it once and returns a typed value or an
//! error; none indexes with data it did not just measure. Each has proptests (round trip
//! and "never panics") and a cargo-fuzz target in `fuzz/fuzz_targets`.

pub mod digest;
pub mod link;
pub mod manifest;
pub mod media_type;
pub mod name;
pub mod reference;
pub mod route;
pub mod scope;
pub mod session;

pub use digest::{Digest, DigestAlgorithm, DigestError};
pub use link::{PageError, PageParams, next_link, parse_next_link};
pub use manifest::{Descriptor, ManifestError, ParsedManifest};
pub use media_type::{Accept, ManifestType};
pub use name::{NameError, RepositoryName, split_full_name};
pub use reference::{Reference, ReferenceError, Tag};
pub use route::{OciRoute, RouteError};
pub use scope::{Action, Resource, Scope, ScopeError, parse_scopes};
pub use session::{SessionIdError, UploadSessionId};
