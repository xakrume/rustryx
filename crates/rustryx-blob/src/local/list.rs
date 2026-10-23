//! Directory walk behind `LocalBlobStore::list_keys`.
//!
//! Layout: `<root>/<b0>/<b1>/<key>`, so the walk visits the shard directories in name
//! order (`00` to `ff`, twice), and inside a leaf directory the keys in component order.
//! That order is stable between calls, which is what a cursor needs, but it is NOT key
//! order: the shard is a hash of the key (docs/blob.md, decision B1).
//!
//! The cursor is the location (`<b0>/<b1>/<key>`) of the last key of the previous page,
//! so a key deleted between two pages does not break the walk: the next page starts at the
//! first name greater than the cursor.
//!
//! Bounds, because a store root on a shared volume is not trusted to be tidy:
//!
//! * a page has at most `MAX_LIST_PAGE` keys, whatever `limit` asks for;
//! * a leaf directory is read for at most [`MAX_LEAF_ENTRIES`] names and descended at most
//!   [`MAX_DEPTH`] levels (a valid key has at most 256 components);
//! * symlinks and special files are never followed or reported, so a planted link cannot
//!   steer the walk out of the root or into a loop;
//! * names that are not a valid key of their shard are skipped and counted in one log line.

use std::cmp::Ordering;
use std::path::{Path, PathBuf};

use rustryx_core::BlobError;
use rustryx_core::ports::{BlobKey, BlobMeta, KeyPage};
use tokio::fs;

use super::blob_meta;
use crate::error::{backend, is_absent};
use crate::key::{MAX_LIST_PAGE, key_of_location};

/// Deepest directory level below a leaf that the walk enters.
const MAX_DEPTH: usize = 256;

/// Most names read from one leaf directory.
const MAX_LEAF_ENTRIES: usize = 100_000;

pub(super) async fn list_keys(
    root: &Path,
    after: Option<&str>,
    limit: u32,
) -> Result<KeyPage, BlobError> {
    let want = limit.clamp(1, MAX_LIST_PAGE) as usize;
    let cursor = after.map(Cursor::parse).transpose()?;
    let mut found: Vec<(String, BlobMeta)> = Vec::new();
    let mut skipped = 0_usize;

    'walk: for first in shard_dirs(root, true).await? {
        if cursor.as_ref().is_some_and(|c| first < c.first) {
            continue;
        }
        let first_dir = root.join(&first);
        for second in shard_dirs(&first_dir, false).await? {
            let resuming_here = cursor
                .as_ref()
                .filter(|c| c.first == first && c.second == second);
            if cursor
                .as_ref()
                .is_some_and(|c| c.first == first && second < c.second)
            {
                continue;
            }
            let leaf = first_dir.join(&second);
            let after_key = resuming_here.map(|c| c.key.as_str());
            // One more than the page: its existence is what makes `next` honest.
            skipped += read_leaf(&leaf, (&first, &second), after_key, want + 1, &mut found).await?;
            if found.len() > want {
                break 'walk;
            }
        }
    }
    if skipped > 0 {
        tracing::warn!(
            skipped,
            "list_keys skipped names that are not blobs of their shard"
        );
    }

    let next = if found.len() > want {
        found.truncate(want);
        found.last().map(|(location, _)| location.clone())
    } else {
        None
    };
    let keys = found
        .into_iter()
        .filter_map(|(location, meta)| {
            key_of_location(&location).map(|key| (BlobKey::new(key), meta))
        })
        .collect();
    Ok(KeyPage { keys, next })
}

/// A parsed cursor: the location of the last key already returned.
struct Cursor {
    first: String,
    second: String,
    key: String,
}

impl Cursor {
    fn parse(text: &str) -> Result<Self, BlobError> {
        // A cursor is client-held, so it is validated like any other key before it is used
        // to decide where a directory walk resumes.
        let key = key_of_location(text).ok_or(BlobError::InvalidKey("invalid list cursor"))?;
        let mut parts = text.splitn(3, '/');
        let first = parts.next().unwrap_or_default().to_owned();
        let second = parts.next().unwrap_or_default().to_owned();
        Ok(Self {
            first,
            second,
            key: key.to_owned(),
        })
    }
}

fn is_shard_name(name: &str) -> bool {
    name.len() == 2 && name.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// Names of the shard directories directly below `dir`, in order. A shard directory that
/// vanished is empty; anything else (`_staging`, stray files, symlinks) is not a shard.
///
/// The root itself is `required`: a missing root is not an empty store. Answering with an
/// empty page would let `verify_blobs` conclude that every blob row has lost its object, or
/// the reverse for a sweep, when the truth is that the volume is gone.
async fn shard_dirs(dir: &Path, required: bool) -> Result<Vec<String>, BlobError> {
    let mut entries = match fs::read_dir(dir).await {
        Ok(entries) => entries,
        Err(err) if is_absent(err.kind()) && required => {
            return Err(BlobError::Unavailable("blob store root is missing".into()));
        }
        Err(err) if is_absent(err.kind()) => return Ok(Vec::new()),
        Err(err) => return Err(backend("list shard directory", err)),
    };
    let mut names = Vec::new();
    while let Some(entry) = entries
        .next_entry()
        .await
        .map_err(|e| backend("list shard directory", e))?
    {
        let Ok(name) = entry.file_name().into_string() else {
            continue;
        };
        if !is_shard_name(&name) {
            continue;
        }
        // `file_type` does not follow symlinks, so a link named like a shard is skipped.
        if entry.file_type().await.is_ok_and(|kind| kind.is_dir()) {
            names.push(name);
        }
    }
    names.sort_unstable();
    Ok(names)
}

/// Reads one leaf directory and appends keys that sort after `after_key` until `found`
/// holds `capacity` entries. Returns how many names were skipped.
async fn read_leaf(
    leaf: &Path,
    shard: (&str, &str),
    after_key: Option<&str>,
    capacity: usize,
    found: &mut Vec<(String, BlobMeta)>,
) -> Result<usize, BlobError> {
    let (mut names, mut skipped) = leaf_names(leaf).await?;
    names.sort_by(|a, b| a.split('/').cmp(b.split('/')));
    for name in names {
        if after_key.is_some_and(|last| name.split('/').cmp(last.split('/')) != Ordering::Greater) {
            continue;
        }
        let location = format!("{}/{}/{name}", shard.0, shard.1);
        if key_of_location(&location).is_none() {
            skipped += 1;
            continue;
        }
        let path: PathBuf = leaf.join(&name);
        // `symlink_metadata`: a link planted under a key name is skipped, never followed.
        match fs::symlink_metadata(&path).await {
            Ok(meta) if meta.is_file() => {
                found.push((location, blob_meta(meta.len(), meta.modified())));
            }
            Ok(_) => skipped += 1,
            // Deleted since the directory was read.
            Err(err) if is_absent(err.kind()) => {}
            Err(err) => return Err(backend("stat blob", err)),
        }
        if found.len() >= capacity {
            break;
        }
    }
    Ok(skipped)
}

/// Relative names of the regular files below a leaf directory (nested keys such as
/// `_uploads/docker/<id>` live in subdirectories), plus the number of names skipped.
async fn leaf_names(leaf: &Path) -> Result<(Vec<String>, usize), BlobError> {
    let mut names = Vec::new();
    let mut skipped = 0_usize;
    let mut pending = vec![(leaf.to_path_buf(), String::new(), 0_usize)];
    while let Some((dir, prefix, depth)) = pending.pop() {
        let mut entries = match fs::read_dir(&dir).await {
            Ok(entries) => entries,
            Err(err) if is_absent(err.kind()) => continue,
            Err(err) => return Err(backend("list blob directory", err)),
        };
        while let Some(entry) = entries
            .next_entry()
            .await
            .map_err(|e| backend("list blob directory", e))?
        {
            let Ok(name) = entry.file_name().into_string() else {
                skipped += 1;
                continue;
            };
            let Ok(kind) = entry.file_type().await else {
                continue;
            };
            let relative = if prefix.is_empty() {
                name
            } else {
                format!("{prefix}/{name}")
            };
            if kind.is_dir() && depth < MAX_DEPTH {
                pending.push((entry.path(), relative, depth + 1));
            } else if kind.is_file() {
                names.push(relative);
                if names.len() >= MAX_LEAF_ENTRIES {
                    return Ok((names, skipped + 1));
                }
            } else {
                skipped += 1;
            }
        }
    }
    Ok((names, skipped))
}

#[cfg(test)]
mod tests;
