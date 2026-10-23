//! Shared engine under all Rustryx format crates (maven, npm, pypi, oci, helm, raw, pub).
//!
//! A format crate implements [`FormatHandler`]: `classify` (path, action, coordinate,
//! before the body is read) and `handle` (the protocol). Everything else a format
//! needs is here, built once and tested once:
//!
//! * [`hosted`]: streaming upload (body, size limit, hashing, blob writer, a single
//!   `AssetRepo::publish_with` commit, abort on error or disconnect), chunked uploads on
//!   resumable stores, streaming download with `ETag`, ranges and conditional requests;
//! * [`proxy`]: cache fill with singleflight, a tee to the first client, bounded
//!   upstream concurrency, timeouts, retries, circuit breaker, negative cache,
//!   serve-stale, checksum verification, and the SSRF guard;
//! * [`group`]: ordered members, per-member authorization, index merging, and
//!   dependency-confusion name claims;
//! * [`policy`]: the hook that immutability, holds and quotas plug into before a body is read;
//! * [`routing`]: allow and block rules per proxy and group repository;
//! * [`downloads`]: batched download counters;
//! * helpers: [`path`] normalization, [`mime`], [`conditional`], [`stream`], [`metrics`].
//!
//! The entry point from the HTTP layer is [`FormatEngine::serve`].
//! `docs/format-core.md` describes the design and how to write a format.

// Errors are documented once per type; repeating `# Errors` on every method of the
// pipelines (which return `FormatError`) adds noise without information.
#![allow(clippy::missing_errors_doc)]
// Tests may unwrap, expect and panic: a panic is the failure signal there.
#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::bool_assert_comparison
    )
)]

pub mod blobio;
pub mod checksum;
pub mod conditional;
pub mod context;
pub mod downloads;
pub mod engine;
pub mod error;
pub mod group;
pub mod handler;
pub mod hosted;
pub mod metrics;
pub mod mime;
pub mod path;
pub mod policy;
pub mod proxy;
pub mod registry;
pub mod request;
pub mod routing;
pub mod standard;
pub mod stream;
pub mod testing;

pub use checksum::{ChecksumSource, ExpectedChecksum, HashAlgo, StandardChecksumHeaders};
pub use context::{
    AllowAllRouting, BlobStoreRegistry, ContentEvent, ContentEvents, FormatContext, Limits,
    NoopEvents, OrphanCause, Ports, RequestMeta, RoutingDecision, RoutingGate,
};
pub use downloads::DownloadBatcher;
pub use engine::{FormatEngine, FormatEngineBuilder};
pub use error::{FormatError, UpstreamFailureKind};
pub use group::{
    GroupIndexMerger, GroupNameClaimer, MemberDocument, MemberRef, MergeInput, MergedDocument,
    NameClaim,
};
pub use handler::{ErrorInfo, FormatHandler};
pub use hosted::{
    CacheStatus, ChunkedSession, ComponentSpec, OverwriteRule, UploadOutcome, UploadSpec,
};
pub use metrics::{CacheLabel, RequestClass, RequestLabels};
pub use path::{NormalizedPath, PathError, PathRules};
pub use policy::{ContentPolicy, DeleteIntent, NoopContentPolicy, WriteIntent};
pub use proxy::{CachedDocument, ProxyFetch, UpstreamPath};
pub use registry::{FormatRegistry, RegistryError};
pub use request::{ArtifactRequest, ClientInfo, ContentKind, RequestHead, action_for_method};
pub use routing::RuleRoutingGate;
pub use standard::classify_path_addressed;
