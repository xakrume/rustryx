//! Hardlink deduplication of a local blob directory.
//!
//! Files with the same size and SHA-256 become one inode. Staging files and dotfiles
//! are left alone. S3 has no hardlinks; this only applies to a directory on one volume.

use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{self, Read};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::key::STAGING_DIR;

/// What one pass did.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct DedupReport {
    /// Regular files seen outside staging.
    pub files: u64,
    /// Files whose bytes were hashed (a size shared by at least two files).
    pub hashed: u64,
    /// Directory entries replaced with a hardlink.
    pub links: u64,
    /// Bytes that a single-link copy no longer occupies.
    pub bytes_saved: u64,
    /// Files that could not be hashed or linked.
    pub errors: u64,
}

/// Replaces duplicate files under `root` with hardlinks to one copy of each.
///
/// # Errors
/// The root cannot be read. A failure on one file is counted in [`DedupReport::errors`]
/// and the walk continues.
pub fn dedup_directory(root: &Path) -> io::Result<DedupReport> {
    let mut files = Vec::new();
    walk(root, &mut files)?;
    let mut report = DedupReport {
        files: u64::try_from(files.len()).unwrap_or(u64::MAX),
        ..DedupReport::default()
    };
    let mut by_size: HashMap<u64, Vec<PathBuf>> = HashMap::new();
    for path in files {
        match path.metadata() {
            Ok(meta) if meta.is_file() => {
                by_size.entry(meta.len()).or_default().push(path);
            }
            Ok(_) => {}
            Err(_) => report.errors = report.errors.saturating_add(1),
        }
    }
    for (size, paths) in by_size {
        if paths.len() < 2 {
            continue;
        }
        let mut by_hash: HashMap<String, Vec<(PathBuf, u64)>> = HashMap::new();
        for path in paths {
            report.hashed = report.hashed.saturating_add(1);
            match hash_file(&path) {
                Ok(digest) => {
                    let nlink = path.metadata().map_or(1, |meta| meta.nlink());
                    by_hash.entry(digest).or_default().push((path, nlink));
                }
                Err(_) => report.errors = report.errors.saturating_add(1),
            }
        }
        for group in by_hash.into_values() {
            if group.len() < 2 {
                continue;
            }
            link_group(size, group, &mut report);
        }
    }
    Ok(report)
}

fn walk(dir: &Path, out: &mut Vec<PathBuf>) -> io::Result<()> {
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name == STAGING_DIR || name.starts_with('.') {
            continue;
        }
        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            walk(&entry.path(), out)?;
        } else if file_type.is_file() {
            out.push(entry.path());
        }
    }
    Ok(())
}

fn hash_file(path: &Path) -> io::Result<String> {
    let mut file = File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0_u8; 64 * 1024];
    loop {
        let read = file.read(&mut buf)?;
        if read == 0 {
            break;
        }
        hasher.update(&buf[..read]);
    }
    Ok(hex::encode(hasher.finalize()))
}

fn link_group(size: u64, mut group: Vec<(PathBuf, u64)>, report: &mut DedupReport) {
    group.sort_by(|left, right| right.1.cmp(&left.1).then_with(|| left.0.cmp(&right.0)));
    let canonical = group[0].0.clone();
    let Ok(canonical_meta) = canonical.metadata() else {
        report.errors = report.errors.saturating_add(1);
        return;
    };
    for (path, _) in group.into_iter().skip(1) {
        let Ok(meta) = path.metadata() else {
            report.errors = report.errors.saturating_add(1);
            continue;
        };
        if meta.dev() == canonical_meta.dev() && meta.ino() == canonical_meta.ino() {
            continue;
        }
        let Some(parent) = path.parent() else {
            report.errors = report.errors.saturating_add(1);
            continue;
        };
        let temp = parent.join(format!(".dedup-{}", uuid_name()));
        if fs::hard_link(&canonical, &temp).is_err() {
            report.errors = report.errors.saturating_add(1);
            let _ = fs::remove_file(&temp);
            continue;
        }
        if fs::rename(&temp, &path).is_err() {
            report.errors = report.errors.saturating_add(1);
            let _ = fs::remove_file(&temp);
            continue;
        }
        report.links = report.links.saturating_add(1);
        if meta.nlink() == 1 {
            report.bytes_saved = report.bytes_saved.saturating_add(size);
        }
    }
}

fn uuid_name() -> String {
    uuid::Uuid::now_v7().simple().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identical_files_share_one_inode_and_the_copy_is_counted() {
        let root = tempfile::tempdir().unwrap();
        fs::write(root.path().join("a.bin"), b"same-bytes").unwrap();
        fs::write(root.path().join("b.bin"), b"same-bytes").unwrap();
        fs::write(root.path().join("c.bin"), b"other").unwrap();
        let report = dedup_directory(root.path()).unwrap();
        assert_eq!(report.links, 1);
        assert_eq!(report.bytes_saved, b"same-bytes".len() as u64);
        let a = fs::metadata(root.path().join("a.bin")).unwrap();
        let b = fs::metadata(root.path().join("b.bin")).unwrap();
        assert_eq!(a.ino(), b.ino());
        assert_ne!(
            a.ino(),
            fs::metadata(root.path().join("c.bin")).unwrap().ino()
        );
        let again = dedup_directory(root.path()).unwrap();
        assert_eq!(again.links, 0);
        assert_eq!(again.bytes_saved, 0);
    }

    #[test]
    fn staging_and_dotfiles_are_not_linked() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir(root.path().join(STAGING_DIR)).unwrap();
        fs::write(root.path().join(STAGING_DIR).join("upload"), b"same-bytes").unwrap();
        fs::write(root.path().join(".hidden"), b"same-bytes").unwrap();
        fs::write(root.path().join("kept.bin"), b"same-bytes").unwrap();
        let report = dedup_directory(root.path()).unwrap();
        assert_eq!(report.files, 1);
        assert_eq!(report.links, 0);
    }
}
