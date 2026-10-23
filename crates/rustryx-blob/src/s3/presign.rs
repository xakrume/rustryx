//! `presign_get` for S3: a time limited direct download URL (`BLOB-16`).
//!
//! The URL is a bearer capability, so three rules hold here and are pinned by tests:
//!
//! * its lifetime never exceeds the configured `presign.ttl_secs`, and that is capped at
//!   [`MAX_PRESIGN_TTL_SECS`] (900 s) whatever a definition says or a caller asks for;
//! * the `response-content-type` and `response-content-disposition` overrides are folded
//!   INTO the signature (`Signer::signed_url_opts`), so the client cannot change them and
//!   the `attachment` disposition that the caller demands for active content survives;
//! * the URL is never written to a log or an error: it travels only inside the returned
//!   [`Secret`], and the errors of this module name no URL.
//!
//! No `HEAD` is made: the caller already knows the asset is committed (design 7.d), and a
//! URL for a key that vanished answers 404 at the client, which then retries through the
//! node.

use std::time::Duration;

use object_store::signer::{Method, SignedUrlOptions};
use rustryx_core::ports::{BlobKey, PresignOptions, PresignedUrl};
use rustryx_core::{BlobError, Secret};
use time::OffsetDateTime;

use super::{S3BlobStore, store_error};
use crate::config::MAX_PRESIGN_TTL_SECS;
use crate::error::with_deadline;

/// A shorter URL is useless: it would expire before the client opens it.
const MIN_TTL: Duration = Duration::from_secs(1);

/// Longest response header override accepted. Real values (`application/octet-stream`,
/// `attachment; filename="x.tgz"`) are far shorter; the bound keeps a URL a sane length.
const MAX_OVERRIDE_BYTES: usize = 1024;

impl S3BlobStore {
    pub(super) async fn presign(
        &self,
        key: &BlobKey,
        ttl: Duration,
        options: &PresignOptions,
    ) -> Result<Option<PresignedUrl>, BlobError> {
        let settings = self.backend.settings.presign;
        let Some(signer) = self.backend.signer.as_ref().filter(|_| settings.enabled) else {
            return Ok(None);
        };
        let path = self.object_path(key)?;

        // Clamped in two steps so that `clamp` never sees a minimum above its maximum, even
        // for settings built in code (the JSON path is validated to 1..=900 already).
        let cap = settings.ttl.clamp(
            MIN_TTL,
            Duration::from_secs(u64::from(MAX_PRESIGN_TTL_SECS)),
        );
        let lifetime = ttl.clamp(MIN_TTL, cap);

        let mut overrides = SignedUrlOptions::default();
        for (name, value) in [
            (
                "response-content-type",
                options.response_content_type.as_deref(),
            ),
            (
                "response-content-disposition",
                options.response_content_disposition.as_deref(),
            ),
        ] {
            if let Some(value) = value {
                check_override(value)?;
                overrides = overrides.with_query([(name, value)]);
            }
        }

        let url = with_deadline("s3 presign", self.deadline(), async {
            signer
                .signed_url_opts(Method::GET, &path, lifetime, &overrides)
                .await
                .map_err(|e| store_error("presign object", e))
        })
        .await?;
        Ok(Some(PresignedUrl {
            url: Secret::new(url.as_str()),
            expires_at: OffsetDateTime::now_utc() + lifetime,
        }))
    }
}

/// A response header override goes into a signed query string and, at the client, into a
/// response header. Control characters (CR and LF above all) have no business in either,
/// so a value with one is refused instead of being escaped and trusted.
fn check_override(value: &str) -> Result<(), BlobError> {
    if value.len() > MAX_OVERRIDE_BYTES || value.chars().any(char::is_control) {
        return Err(BlobError::Unsupported(
            "presign with an oversized or control character response header",
        ));
    }
    Ok(())
}
