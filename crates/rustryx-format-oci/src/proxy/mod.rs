//! Proxy repositories: an upstream registry behind the pipelines of format-core.
//!
//! * manifests by digest and blobs are content addressed, so a cached copy is served for ever
//!   and a fetched one is verified against its digest BEFORE it is served or cached;
//! * a manifest by tag is cached for the repository's metadata TTL and revalidated with the
//!   upstream's own validators; the digest the upstream announces in `Docker-Content-Digest`
//!   is checked against the bytes;
//! * tags lists and the catalog are not cached: they are asked live, bounded, with the
//!   pagination of the client;
//! * what the upstream answers with `401` or `403` is never passed on: it is a `502`.

mod auth;
mod blob;
mod challenge;
mod lists;
mod manifest;
mod names;
mod passthrough;

pub(crate) use auth::{UpstreamAuth, UpstreamTokens};
pub(crate) use blob::read as read_blob;
pub(crate) use lists::{referrers, tags};
pub(crate) use manifest::read as read_manifest;

use rustryx_format_core::{FormatError, UpstreamFailureKind};

use crate::ops::Op;

/// True when `error` says the upstream refused a bearer token this node sent, in which case
/// the cached token (it expired early, or was revoked) is forgotten and the caller asks for a
/// fresh one and tries once more. Written as a loop at each call site instead of a closure
/// helper because rustc cannot prove `Send` through a higher ranked async closure here.
pub(crate) async fn token_was_refused(
    op: &Op<'_>,
    upstream_name: &str,
    auth: &UpstreamAuth,
    error: &FormatError,
) -> bool {
    let refused = matches!(auth, UpstreamAuth::Bearer(_))
        && matches!(
            error,
            FormatError::Upstream {
                kind: UpstreamFailureKind::Rejected,
                ..
            }
        );
    if refused {
        op.shared
            .tokens
            .forget(op.cx.repository().id, upstream_name)
            .await;
    }
    refused
}

/// True when the proxy pipeline can carry this request. A bearer token and the repository's
/// stored Basic credentials cannot go together through it: the pipeline puts the Basic header
/// on every call it makes, over any `Authorization` the format sets (format-core gap, see
/// `docs/format-oci.md`). That one combination is served by [`passthrough`], uncached.
pub(crate) fn pipeline_can_carry(op: &Op<'_>, auth: &UpstreamAuth) -> bool {
    match auth {
        UpstreamAuth::Direct => true,
        UpstreamAuth::Bearer(_) => op
            .cx
            .repository()
            .proxy
            .as_ref()
            .is_none_or(|config| config.auth.is_none()),
    }
}
