//! Publish step 3: `GET api/packages/versions/finish/<id>`.
//!
//! This is where an upload becomes a published version, and where the archive is read for the
//! first time. The order is the order of the checks that must hold before anything is visible:
//!
//! 1. the staged archive exists, and the caller is the one who uploaded it;
//! 2. the archive is read **from the blob store, on the blocking pool, behind a permit**, after
//!    it was streamed there, never buffered whole: limits, paths, links, manifest (see
//!    [`crate::archive`] and [`crate::pubspec`]);
//! 3. the name and version of the manifest equal the coordinate the publisher declared, if it
//!    declared one;
//! 4. the caller is authorized to write exactly that coordinate (the engine authorized the step
//!    itself, but a pub client names the package only inside the archive, so the decision about
//!    the package is made here, again, through the same authorizer);
//! 5. the policy hook, then **create only** publication of the archive path: a version is
//!    immutable, so a second publication of it is refused (409) and the database decides it, on
//!    any node;
//! 6. the version document, then removal of the staging copy.
//!
//! The visible effect is the pair of assets at the end of step 5 and 6. If the document cannot
//! be written the archive asset is removed again, so a version is either listed and servable or
//! not there; the one window that remains is a crash between the two writes, which leaves an
//! archive that is not listed and refuses a republication until an administrator deletes it. That
//! failure direction is chosen on purpose: an unlisted archive is harmless to clients, a listed
//! version without an archive is a broken install.
//!
//! The whole step runs as a task tracked by the engine, so a publisher that disconnects while it
//! runs cannot leave it half done.

use std::sync::Arc;

use axum_core::body::Body;
use http::{Response, StatusCode};
use rustryx_core::audit::{ActorRef, AuditEvent, AuditKind};
use rustryx_core::error::{AuthError, MutationError, PolicyViolation};
use rustryx_core::identity::Action;
use rustryx_core::model::{
    Asset, NewAssetLink, NewComponent, PublishMode, RepoFormat, WriteOptions,
};
use rustryx_core::ports::auth::Coordinate;
use rustryx_core::ports::{Decision, ResourceRef};
use rustryx_format_core::{
    ArtifactRequest, ComponentSpec, ContentEvent, FormatContext, FormatError, NormalizedPath,
    OverwriteRule, UploadSpec, WriteIntent,
};

use super::publish::discard_staged;
use crate::archive::{ArchiveError, inspect};
use crate::blobs;
use crate::blocking::BlockingPool;
use crate::config::PubConfig;
use crate::document::{VersionEntry, success_json};
use crate::metrics;
use crate::pubspec::{self, Pubspec, PubspecError};
use crate::response;
use crate::route::{Declared, Route, archive_path, version_path};

/// Content type of a stored archive.
const ARCHIVE_CONTENT_TYPE: &str = "application/gzip";

/// Why the archive of a staged upload was refused.
enum Refusal {
    Archive(ArchiveError),
    Pubspec(PubspecError),
}

/// Runs the step as a tracked task, so that it is not cancelled with the request.
pub(crate) async fn run(
    cx: &FormatContext,
    cfg: &Arc<PubConfig>,
    pool: &BlockingPool,
    req: &ArtifactRequest,
) -> Result<Response<Body>, FormatError> {
    let job = Job {
        cx: cx.clone(),
        cfg: Arc::clone(cfg),
        pool: pool.clone(),
        req: req.clone(),
    };
    match cx.tasks().spawn(job.run()).await {
        Ok(result) => result,
        Err(error) => Err(FormatError::internal(format!(
            "the pub finalize task failed: {error}"
        ))),
    }
}

struct Job {
    cx: FormatContext,
    cfg: Arc<PubConfig>,
    pool: BlockingPool,
    req: ArtifactRequest,
}

impl Job {
    async fn run(self) -> Result<Response<Body>, FormatError> {
        let Route::Finish { id, declared } = Route::parse(&self.req.path)? else {
            return Err(FormatError::internal("finalize called for another route"));
        };
        // The staging copy lives at the path of the upload step of this publication.
        let staged_path = NormalizedPath::parse(
            &Route::Upload {
                id,
                declared: declared.clone(),
            }
            .to_path(),
        )?;
        let outcome = self.publish(declared.as_ref(), &staged_path).await;
        match &outcome {
            Ok(_) => metrics::publish("ok"),
            Err(failure) => metrics::publish(failure.metric_reason()),
        }
        match outcome {
            Ok(message) => Ok(response::json(StatusCode::OK, success_json(&message))),
            Err(failure) => {
                // A staging copy that cannot become a version is garbage and goes. The two
                // refusals that say nothing about the copy do not touch it: an id that names
                // nothing, and an id that belongs to someone else's upload.
                if failure.discard {
                    discard_staged(&self.cx, &staged_path).await;
                }
                Err(failure.error)
            }
        }
    }

    async fn publish(
        &self,
        declared: Option<&Declared>,
        staged_path: &NormalizedPath,
    ) -> Result<String, Failure> {
        let cx = &self.cx;
        let repository = cx.repository();
        let staged = self.staged_asset(staged_path).await?;

        let pubspec = self.read_manifest(&staged).await?;
        if declared.is_some_and(|d| d.name != pubspec.name || d.version != pubspec.version) {
            return Err(Failure::new(
                "declared_mismatch",
                FormatError::bad_request(
                    "pubspec.yaml does not match the package and version that were declared",
                ),
            ));
        }
        let name = pubspec.name.clone();
        let version = pubspec.version.clone();
        let path = NormalizedPath::parse(&archive_path(&name, &version))
            .map_err(|e| Failure::new("internal", e.into()))?;

        self.authorize(&path, &name, &version).await?;

        let component = ComponentSpec {
            group: None,
            name: name.to_string(),
            version: Some(version.to_string()),
        };
        let existing = cx
            .ports()
            .assets
            .get_by_path(repository.id, path.as_str())
            .await
            .map_err(|e| Failure::new("store", e.into()))?;
        let mut intent = WriteIntent::new(repository, cx.principal(), &path);
        intent.existing = existing.as_ref();
        intent.component = Some(&component);
        if let Err(error) = cx.ports().policy.check_write(&intent).await {
            return Err(Failure::new("policy", error.into()));
        }
        if existing.is_some() {
            return Err(already_exists(name.as_str(), &version.to_string()));
        }

        let archive = self.link_archive(&staged, &path, &component).await?;
        let sha256 = archive.checksums.sha256.clone();
        if let Err(failure) = self.store_version_document(&pubspec, &sha256).await {
            // Do not leave a version that is servable but not listed.
            if let Err(error) = cx.hosted().delete(&path).await {
                tracing::error!(%error, path = %path, "could not remove an archive after its version document failed");
            }
            return Err(failure);
        }
        self.announce(&archive).await;
        discard_staged(cx, staged_path).await;
        Ok(format!("Successfully uploaded {name} version {version}."))
    }

    /// The staged archive, if it exists and belongs to the caller.
    async fn staged_asset(&self, path: &NormalizedPath) -> Result<Asset, Failure> {
        let cx = &self.cx;
        let staged = cx
            .ports()
            .assets
            .get_by_path(cx.repository().id, path.as_str())
            .await
            .map_err(|e| Failure::new("store", e.into()))?
            // Unknown, finished and reaped uploads look the same.
            .ok_or_else(|| Failure::keep_staged("unknown_upload", FormatError::NotFound))?;
        // The id is a capability of whoever started the upload. Another writer of the same
        // repository must not be able to complete (or poison) it.
        if staged.created_by != cx.principal().user_id {
            return Err(Failure::keep_staged(
                "foreign_upload",
                FormatError::Forbidden,
            ));
        }
        Ok(staged)
    }

    /// Reads the staged archive on the blocking pool and returns its manifest.
    async fn read_manifest(&self, staged: &Asset) -> Result<Pubspec, Failure> {
        let cx = &self.cx;
        let (_, read) = blobs::open(cx, staged)
            .await
            .map_err(|e| Failure::new("store", e))?;
        let compressed_len = staged.size_bytes;
        let cfg = Arc::clone(&self.cfg);
        let result: Result<Pubspec, Refusal> = self
            .pool
            .run_with_reader(read.stream, FormatError::Blob, move |reader| {
                let report =
                    inspect(reader, compressed_len, &cfg.archive).map_err(Refusal::Archive)?;
                pubspec::parse(&report.pubspec, &cfg.pubspec).map_err(Refusal::Pubspec)
            })
            .await
            .map_err(|e| Failure::new("blocking", e))?;
        result.map_err(Failure::from_refusal)
    }

    /// Authorizes the write of this coordinate through the same authorizer the engine used for
    /// the step. Fails closed.
    async fn authorize(
        &self,
        path: &NormalizedPath,
        name: &crate::name::PackageName,
        version: &crate::version::PubVersion,
    ) -> Result<(), Failure> {
        let cx = &self.cx;
        let resource = ResourceRef::for_repository(cx.repository(), path.as_str()).with_coordinate(
            Coordinate::new(None, Some(name.to_string()), Some(version.to_string())),
        );
        match cx
            .ports()
            .rbac
            .check(cx.principal(), Action::Write, &resource)
            .await
        {
            Ok(Decision::Allow) => Ok(()),
            Ok(Decision::Deny) => Err(Failure::new("forbidden", FormatError::Forbidden)),
            Err(AuthError::Overloaded) => Err(Failure::new(
                "overloaded",
                FormatError::Unavailable {
                    reason: "authorization capacity exhausted, retry",
                    retry_after: Some(std::time::Duration::from_secs(1)),
                },
            )),
            Err(error) => {
                tracing::warn!(%error, "authorization of a pub publication failed closed");
                Err(Failure::new("forbidden", FormatError::Forbidden))
            }
        }
    }

    /// Publishes the archive path as a link to the staged blob, create only.
    async fn link_archive(
        &self,
        staged: &Asset,
        path: &NormalizedPath,
        component: &ComponentSpec,
    ) -> Result<Asset, Failure> {
        let cx = &self.cx;
        let repository = cx.repository();
        let mut new = NewComponent::new(repository.id, RepoFormat::Pub, &component.name);
        new.version.clone_from(&component.version);
        new.created_by = cx.principal().user_id;
        let component_row = cx
            .ports()
            .components
            .find_or_create(&new)
            .await
            .map_err(|e| Failure::new("store", e.into()))?;
        let mut link = NewAssetLink::new(
            repository.id,
            path.as_str(),
            ARCHIVE_CONTENT_TYPE,
            staged.blob_id,
        );
        link.component_id = Some(component_row.id);
        link.created_by = cx.principal().user_id;
        let mut options = WriteOptions::new()
            .with_actor(ActorRef::of(cx.principal()))
            .with_mode(PublishMode::CreateOnly);
        if let Some(request_id) = cx.request_id() {
            options = options.with_request_id(request_id);
        }
        match cx.ports().assets.link(&link, &options).await {
            Ok(outcome) => Ok(outcome.asset),
            Err(MutationError::Violation(PolicyViolation::AlreadyExists)) => Err(already_exists(
                &component.name,
                component.version.as_deref().unwrap_or_default(),
            )),
            Err(error) => Err(Failure::new("policy", error.into())),
        }
    }

    /// Stores the document that the listing is built from.
    async fn store_version_document(&self, pubspec: &Pubspec, sha256: &str) -> Result<(), Failure> {
        let cx = &self.cx;
        let entry = VersionEntry {
            version: pubspec.version.clone(),
            retracted: false,
            archive_url: String::new(),
            archive_sha256: Some(sha256.to_owned()),
            pubspec: pubspec.json.clone(),
            published: crate::time_text::rfc3339(cx.now()),
        };
        let bytes = serde_json::to_vec(&entry.to_stored_json())
            .map_err(|e| Failure::new("internal", FormatError::internal(e.to_string())))?;
        let path = NormalizedPath::parse(&version_path(&pubspec.name, &pubspec.version))
            .map_err(|e| Failure::new("internal", e.into()))?;
        let mut spec = UploadSpec::new(path);
        spec.content_type = Some("application/json".to_owned());
        spec.content_length = Some(bytes.len() as u64);
        spec.component = Some(ComponentSpec {
            group: None,
            name: pubspec.name.to_string(),
            version: Some(pubspec.version.to_string()),
        });
        spec.overwrite = OverwriteRule::Never;
        cx.hosted()
            .upload(spec, Body::from(bytes))
            .await
            .map(|_| ())
            .map_err(|e| match e {
                FormatError::Conflict(_) => {
                    already_exists(pubspec.name.as_str(), &pubspec.version.to_string())
                }
                other => Failure::new("document", other),
            })
    }

    /// Event and audit record of the publication of the archive. The pipeline did both for the
    /// staging copy and for the version document; the archive path is published here.
    async fn announce(&self, archive: &Asset) {
        let cx = &self.cx;
        cx.ports().events.emit(ContentEvent::AssetPublished {
            repository: cx.repository().id,
            asset: archive.clone(),
            replaced: false,
        });
        let mut event = AuditEvent::new(
            AuditKind::ContentChange,
            cx.principal().username.clone(),
            "asset.publish",
            "success",
        );
        event.at = cx.now();
        event.actor_id = cx.principal().user_id;
        event.actor_kind = cx.principal().actor_kind();
        event.request_id = cx.request_id().map(str::to_owned);
        event.target = Some(format!("{}:{}", cx.repository().name, archive.path));
        event.project_id = cx.repository().project.as_ref().map(|project| project.id);
        event.detail = serde_json::json!({
            "size_bytes": archive.size_bytes,
            "sha256": archive.checksums.sha256,
            "replaced": false,
        });
        if let Err(error) = cx.ports().audit.record(event).await {
            // The publication is committed; a failing sink must not undo it.
            tracing::error!(%error, "audit record for a pub publication failed");
        }
    }
}

/// A failed step: the error for the client and a bounded reason for the metric.
struct Failure {
    reason: &'static str,
    error: FormatError,
    /// Whether the staging copy is removed after this failure.
    discard: bool,
}

impl Failure {
    fn new(reason: &'static str, error: FormatError) -> Self {
        Self {
            reason,
            error,
            discard: true,
        }
    }

    /// A failure that must leave the staged upload alone.
    fn keep_staged(reason: &'static str, error: FormatError) -> Self {
        Self {
            discard: false,
            ..Self::new(reason, error)
        }
    }

    fn metric_reason(&self) -> &'static str {
        self.reason
    }

    fn from_refusal(refusal: Refusal) -> Self {
        match refusal {
            Refusal::Archive(error) => {
                let client = match error {
                    // The content is readable and unacceptable: bombs and oversized parts.
                    ArchiveError::Limit(kind) => FormatError::unprocessable(kind.message()),
                    other => FormatError::bad_request(other.to_string()),
                };
                Self::new(error.reason(), client)
            }
            Refusal::Pubspec(error) => {
                let client = match &error {
                    PubspecError::TooLarge
                    | PubspecError::Complexity
                    | PubspecError::Depth
                    | PubspecError::StringTooLong => FormatError::unprocessable(error.to_string()),
                    _ => FormatError::bad_request(error.to_string()),
                };
                Self::new(error.reason(), client)
            }
        }
    }
}

fn already_exists(name: &str, version: &str) -> Failure {
    Failure::new(
        "version_exists",
        FormatError::conflict(format!(
            "version {version} of package {name} already exists and cannot be replaced"
        )),
    )
}
