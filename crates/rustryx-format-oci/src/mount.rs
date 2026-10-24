//! Cross repository blob mount: `POST <name>/blobs/uploads/?mount=<digest>&from=<repo>`.
//!
//! A mount shares the blob of another image with this one (`AssetRepo::link`: no bytes are
//! copied, the quota counts the blob once). It is only allowed when the caller may READ the
//! source: without that check a writer who knows a digest could pull any image's layers
//! into a repository of their own and read them from there.
//!
//! Every reason not to mount is the same answer: the caller gets `None` and the handler
//! starts an ordinary upload session (202), which the specification allows. A caller learns
//! nothing about whether the source exists, whether the blob exists or whether it may read
//! it.
//!
//! Only hosted repositories are sources. A proxy source would need the routing rules and
//! the namespace allow list of that proxy to be consulted again, and a cached copy is not
//! the proxy's to hand out to a repository that is not behind those rules.

use axum_core::body::Body;
use rustryx_core::error::{AuthError, MutationError, StoreError};
use rustryx_core::identity::Action;
use rustryx_core::model::{NewAssetLink, RepoFormat, RepoType};
use rustryx_core::ports::{Coordinate, Decision, ResourceRef};
use rustryx_format_core::{FormatError, WriteIntent};
use serde_json::json;

use crate::blobs;
use crate::error::OciError;
use crate::ops::Op;
use crate::parse::digest::{Digest, DigestAlgorithm};
use crate::parse::media_type::OCTET_STREAM;
use crate::parse::name::{RepositoryName, split_full_name};
use crate::store;

/// Tries to mount; `Ok(None)` means "not mounted, carry on with a normal upload".
pub(crate) async fn try_mount(
    op: &Op<'_>,
    name: &RepositoryName,
    mount: &str,
    from: &str,
) -> Result<Option<http::Response<Body>>, OciError> {
    let Ok(digest) = Digest::parse(mount) else {
        return Ok(None);
    };
    if digest.algorithm() != DigestAlgorithm::Sha256 {
        return Ok(None);
    }
    let Ok((source_name, source_image)) = split_full_name(from) else {
        return Ok(None);
    };
    let target_path = store::blob_path(name, &digest);
    if store::get_asset(op.cx, &target_path).await?.is_some() {
        return Ok(Some(blobs::created(op, name, &digest)));
    }
    let source = op
        .cx
        .ports()
        .repositories
        .get_by_name(&source_name)
        .await
        .map_err(FormatError::from)?;
    let Some(source) = source.filter(|repo| {
        matches!(repo.format, RepoFormat::Docker | RepoFormat::Oci)
            && repo.repo_type == RepoType::Hosted
    }) else {
        return Ok(None);
    };
    let source_path = store::blob_path(&source_image, &digest);
    let resource =
        ResourceRef::for_repository(&source, source_path.clone()).with_coordinate(Coordinate::new(
            Some(source_image.as_str().to_owned()),
            Some(source_image.leaf().to_owned()),
            Some(digest.to_string()),
        ));
    match op
        .cx
        .ports()
        .rbac
        .check(op.cx.principal(), Action::Read, &resource)
        .await
    {
        Ok(Decision::Allow) => {}
        Ok(Decision::Deny) => return Ok(None),
        Err(AuthError::Overloaded) => {
            return Err(FormatError::Unavailable {
                reason: "authorization capacity exhausted, retry",
                retry_after: Some(std::time::Duration::from_secs(1)),
            }
            .into());
        }
        Err(error) => {
            // A lookup that failed is not a yes: nothing is mounted, the upload goes on.
            tracing::warn!(%error, "mount source authorization failed closed");
            return Ok(None);
        }
    }
    let Some(source_asset) = op
        .cx
        .ports()
        .assets
        .get_by_path(source.id, &source_path)
        .await
        .map_err(FormatError::from)?
    else {
        return Ok(None);
    };
    let path = Op::path(&target_path)?;
    let intent = WriteIntent::new(op.cx.repository(), op.cx.principal(), &path);
    op.cx
        .ports()
        .policy
        .check_write(&intent)
        .await
        .map_err(FormatError::from)?;
    let mut link = NewAssetLink::new(
        op.cx.repository().id,
        target_path.clone(),
        OCTET_STREAM,
        source_asset.blob_id,
    );
    link.created_by = op.cx.principal().user_id;
    match op.cx.ports().assets.link(&link, &op.write_options()).await {
        Ok(_) => {
            crate::audit::record(
                op.cx,
                "oci.blob.mount",
                format!("{}:{target_path}", op.cx.repository().name),
                json!({ "from": from, "digest": digest.to_string() }),
            )
            .await;
            Ok(Some(blobs::created(op, name, &digest)))
        }
        // The two repositories use different blob stores: the bytes must be copied, which
        // is what the fallback upload does.
        Err(MutationError::Store(StoreError::Conflict(_))) => Ok(None),
        Err(error) => Err(FormatError::from(error).into()),
    }
}

#[cfg(test)]
mod tests;
