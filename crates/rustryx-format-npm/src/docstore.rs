//! Where a hosted package's document lives, and how it changes without losing an update.
//!
//! The packument of a hosted package is stored as a sequence of **generations**, each an asset at
//! `<package>/-/doc/<number>.json` (the number zero padded to 20 digits, so path order is numeric
//! order). The current document is the highest generation. A change reads the highest generation
//! `n`, computes the next document and writes generation `n + 1` with a create-only publish.
//!
//! Why not overwrite one asset in place:
//!
//! * the repository write policy `allow_once` refuses to replace any existing path, so an
//!   in-place packument would make the second version of every package unpublishable on such a
//!   repository (the format core has no way to mark a generated document as rewritable);
//! * an overwrite is a blind last-writer-wins: two publishers on two nodes both read generation
//!   `n`, both write, and one of the versions silently disappears from the document while its
//!   tarball stays behind. Here the second creator of generation `n + 1` is refused by the
//!   database (create-only publish, [`FormatError::Conflict`]), re-reads and applies its change
//!   to the document that won. The guarantee is the transaction, not this process.
//!
//! Older generations are deleted after a successful write, on a best effort basis: a refused or
//! failed delete leaves a stale generation behind that readers ignore, because the highest number
//! wins. The cost of the scheme is one asset listing per read, and one extra blob per change until
//! the garbage collector removes the replaced ones.

use std::sync::Arc;

use axum_core::body::Body;
use bytes::Bytes;
use rustryx_core::model::Asset;
use rustryx_core::ports::{BlobKey, PageRequest};
use rustryx_format_core::stream::collect_bounded;
use rustryx_format_core::{FormatContext, FormatError, NormalizedPath, OverwriteRule, UploadSpec};
use sha2::{Digest, Sha256};

use crate::blocking::Blocking;
use crate::name::PackageName;
use crate::packument::{Packument, PackumentError, ParseMode};

/// Attempts at winning the creation of the next generation before the request gives up.
const MAX_ATTEMPTS: usize = 16;

/// Generations listed per page, and pages read at most: far more than ever exist, because old
/// ones are deleted, but a listing must end even if every delete has been refused for a year.
const LIST_PAGE: u32 = 100;
const LIST_PAGES: usize = 50;

/// The current document of a package.
#[derive(Debug, Clone)]
pub struct StoredDocument {
    /// Its generation number.
    pub generation: u64,
    /// The asset that holds it.
    pub asset: Asset,
}

/// Document operations of one package of the hosted repository of a context.
pub struct DocStore<'a> {
    cx: &'a FormatContext,
    package: &'a PackageName,
}

/// Path of generation `generation` of a package.
#[must_use]
pub fn generation_path(package: &PackageName, generation: u64) -> String {
    format!("{package}/-/doc/{generation:020}.json")
}

fn prefix(package: &PackageName) -> String {
    format!("{package}/-/doc/")
}

fn generation_of(package: &PackageName, path: &str) -> Option<u64> {
    let number = path
        .strip_prefix(prefix(package).as_str())?
        .strip_suffix(".json")?;
    if number.len() == 20 && number.bytes().all(|b| b.is_ascii_digit()) {
        number.parse().ok()
    } else {
        None
    }
}

impl<'a> DocStore<'a> {
    /// The store of `package` in the repository of `cx`.
    #[must_use]
    pub fn new(cx: &'a FormatContext, package: &'a PackageName) -> Self {
        Self { cx, package }
    }

    /// Every generation that exists, ascending.
    async fn generations(&self) -> Result<Vec<StoredDocument>, FormatError> {
        let repository = self.cx.repository().id;
        let wanted = prefix(self.package);
        let mut found = Vec::new();
        let mut after = None;
        for _ in 0..LIST_PAGES {
            let page = self
                .cx
                .ports()
                .assets
                .list(
                    repository,
                    &wanted,
                    &PageRequest {
                        limit: LIST_PAGE,
                        after: after.clone(),
                    },
                )
                .await?;
            for asset in page.items {
                if let Some(generation) = generation_of(self.package, &asset.path) {
                    found.push(StoredDocument { generation, asset });
                }
            }
            match page.next {
                Some(next) => after = Some(next),
                None => break,
            }
        }
        found.sort_by_key(|doc| doc.generation);
        Ok(found)
    }

    /// The current document, or `None` when the package has none.
    ///
    /// # Errors
    /// Store errors.
    pub async fn latest(&self) -> Result<Option<StoredDocument>, FormatError> {
        Ok(self.generations().await?.pop())
    }

    /// Reads the bytes of a document, at most `limit` of them.
    ///
    /// # Errors
    /// `Internal` when the blob row is missing or the document is larger than `limit` (a document
    /// this registry wrote is never larger than its own limit), store and blob errors.
    pub async fn read(&self, document: &StoredDocument, limit: u64) -> Result<Bytes, FormatError> {
        if document.asset.size_bytes > limit {
            return Err(FormatError::internal(format!(
                "stored package document of {} bytes exceeds the limit of {limit}",
                document.asset.size_bytes
            )));
        }
        let record = self
            .cx
            .ports()
            .blobs
            .get(document.asset.blob_id)
            .await?
            .ok_or_else(|| FormatError::internal("a package document has no blob record"))?;
        let store = self.cx.store_for_blob(&record).await?;
        let read = store.get(&BlobKey::new(record.key), None).await?;
        collect_bounded(read.stream, limit)
            .await
            .map_err(|error| FormatError::internal(format!("reading a package document: {error}")))
    }

    async fn write(&self, generation: u64, bytes: Vec<u8>) -> Result<(), FormatError> {
        let path = NormalizedPath::parse(&generation_path(self.package, generation))?;
        let mut spec = UploadSpec::new(path);
        spec.content_type = Some("application/json".to_owned());
        spec.content_length = Some(bytes.len() as u64);
        // Create-only is the compare and set of the whole scheme.
        spec.overwrite = OverwriteRule::Never;
        self.cx
            .hosted()
            .upload(spec, Body::from(Bytes::from(bytes)))
            .await
            .map(|_| ())
    }

    /// Checks that the generation just written is still the current one, then deletes the
    /// generations below it. Returns false when a higher generation exists.
    ///
    /// Deleting old generations frees their paths, and a writer that read the document long ago
    /// (a stalled request, another node) would then succeed in creating a generation number that
    /// was already used and pruned: its write would land below the current generation, invisible
    /// to readers, and still be reported as a success. Looking at the listing after the write
    /// closes that: a write that is not the highest generation was superseded, it is removed and
    /// the change is applied again on top of the document that is current.
    ///
    /// Failures of the cleanup are logged and ignored: the highest generation wins whatever is
    /// left behind.
    async fn settle(&self, written: u64) -> Result<bool, FormatError> {
        let all = self.generations().await?;
        let superseded = all.iter().any(|doc| doc.generation > written);
        for old in all
            .into_iter()
            .filter(|doc| doc.generation < written || (superseded && doc.generation == written))
        {
            let Ok(path) = NormalizedPath::parse(&old.asset.path) else {
                continue;
            };
            if let Err(error) = self.cx.hosted().delete(&path).await {
                tracing::warn!(%error, path = %old.asset.path, "could not delete a package document generation");
            }
        }
        Ok(!superseded)
    }

    /// Deletes every generation of the package.
    ///
    /// # Errors
    /// The first refusal of a delete (an immutability rule, a hold).
    pub async fn delete_all(&self) -> Result<(), FormatError> {
        for doc in self.generations().await? {
            let path = NormalizedPath::parse(&doc.asset.path)?;
            match self.cx.hosted().delete(&path).await {
                Ok(_) | Err(FormatError::NotFound) => {}
                Err(error) => return Err(error),
            }
        }
        Ok(())
    }

    /// The current document parsed, with the SHA-256 of its stored bytes, or `None` for a package
    /// that has none.
    ///
    /// # Errors
    /// Store errors, `Internal` for an unreadable stored document, `Unavailable` without a
    /// permit of the blocking pool.
    pub async fn current(
        &self,
        blocking: &Blocking,
        max_bytes: u64,
    ) -> Result<Option<(Packument, String)>, FormatError> {
        let Some(stored) = self.latest().await? else {
            return Ok(None);
        };
        let bytes = self.read(&stored, max_bytes).await?;
        let package = self.package.clone();
        let wait = self.cx.limits().blocking_permit_wait;
        let document = blocking
            .run(wait, move || {
                Packument::from_slice(&bytes, ParseMode::Strict, Some(&package))
                    .map_err(|e| stored_document_error(&e))
            })
            .await??;
        Ok(Some((document, stored.asset.checksums.sha256)))
    }

    /// Applies `change` to the current document and stores the result as the next generation.
    ///
    /// `change` receives the current document (`None` for a package that has none) and returns
    /// the new one, or says it changed nothing; it may be called several times, once per lost
    /// race, so it must be a pure function of its input. Parsing, `change` and serialization run
    /// on the blocking pool.
    ///
    /// # Errors
    /// Whatever `change` returns, store errors, `PayloadTooLarge` when the new document would
    /// exceed `max_bytes`, and `Unavailable` when `MAX_ATTEMPTS` rounds were all lost to other
    /// writers.
    pub async fn update<F>(
        &self,
        blocking: &Blocking,
        max_bytes: u64,
        change: Arc<F>,
    ) -> Result<Updated, FormatError>
    where
        F: Fn(Option<Packument>, bool) -> Result<Change, FormatError> + Send + Sync + 'static,
    {
        let wait = self.cx.limits().blocking_permit_wait;
        // True once an attempt of this call has written a generation that was then superseded.
        // The document that is current may already contain that change (a writer that read the
        // generation before it was superseded built on it), so the change function must treat
        // "already applied" as done, not as an error.
        let mut applied_before = false;
        for _ in 0..MAX_ATTEMPTS {
            let current = self.latest().await?;
            let (generation, bytes, current_digest) = match &current {
                Some(document) => (
                    document.generation,
                    Some(self.read(document, max_bytes).await?),
                    document.asset.checksums.sha256.clone(),
                ),
                None => (0, None, String::new()),
            };
            let change = Arc::clone(&change);
            let package = self.package.clone();
            let (outcome, stored) = blocking
                .run(wait, move || {
                    let parsed = match &bytes {
                        Some(bytes) => Some(
                            Packument::from_slice(bytes, ParseMode::Strict, Some(&package))
                                .map_err(|e| stored_document_error(&e))?,
                        ),
                        None => None,
                    };
                    let outcome = change(parsed, applied_before)?;
                    let stored = match &outcome {
                        Change::Replace(document) => document.to_stored_vec(),
                        Change::Keep(_) => Vec::new(),
                    };
                    Ok::<_, FormatError>((outcome, stored))
                })
                .await??;
            let document = match outcome {
                Change::Keep(document) => {
                    return Ok(Updated {
                        document,
                        digest: current_digest,
                        changed: false,
                    });
                }
                Change::Replace(document) => document,
            };
            if stored.len() as u64 > max_bytes {
                return Err(FormatError::PayloadTooLarge { limit: max_bytes });
            }
            let digest = hex::encode(Sha256::digest(&stored));
            match self.write(generation + 1, stored).await {
                Ok(()) => {
                    if self.settle(generation + 1).await? {
                        return Ok(Updated {
                            document,
                            digest,
                            changed: true,
                        });
                    }
                    applied_before = true;
                    crate::metrics::cas_retry();
                }
                Err(FormatError::Conflict(_)) => crate::metrics::cas_retry(),
                Err(other) => return Err(other),
            }
        }
        Err(FormatError::Unavailable {
            reason: "the package is being changed by too many requests at once, retry",
            retry_after: Some(std::time::Duration::from_secs(1)),
        })
    }
}

/// What a change function decided.
#[derive(Debug)]
pub enum Change {
    /// Store this document as the next generation.
    Replace(Packument),
    /// Nothing to store: the document already is what the request asks for.
    Keep(Packument),
}

/// The result of a change.
#[derive(Debug)]
pub struct Updated {
    /// The document after the change.
    pub document: Packument,
    /// SHA-256 (hex) of its stored bytes, for `_rev`.
    pub digest: String,
    /// False when the change function kept the document.
    pub changed: bool,
}

fn stored_document_error(error: &PackumentError) -> FormatError {
    // The document is ours: failing to read it is a fault of this registry, never of the client.
    FormatError::internal(format!("a stored package document is unreadable: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generation_paths_sort_numerically_and_round_trip() {
        let package = PackageName::parse("@a/b").unwrap();
        let low = generation_path(&package, 9);
        let high = generation_path(&package, 10);
        assert!(low < high, "zero padding keeps path order numeric");
        assert_eq!(generation_of(&package, &low), Some(9));
        assert_eq!(generation_of(&package, &high), Some(10));
        assert_eq!(low, "@a/b/-/doc/00000000000000000009.json");
    }

    #[test]
    fn foreign_paths_are_not_generations() {
        let package = PackageName::parse("a").unwrap();
        let other = PackageName::parse("ab").unwrap();
        for path in [
            "a/-/doc/9.json",
            "a/-/doc/0000000000000000000x.json",
            "a/-/doc/00000000000000000009.txt",
            "a/-/a-1.0.0.tgz",
            "ab/-/doc/00000000000000000009.json",
            "a/-/doc/00000000000000000009.json/x",
        ] {
            assert_eq!(generation_of(&package, path), None, "{path}");
        }
        assert_eq!(
            generation_of(&other, "ab/-/doc/00000000000000000009.json"),
            Some(9)
        );
    }
}
