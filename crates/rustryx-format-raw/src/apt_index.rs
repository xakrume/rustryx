//! Debian archive index: `dists/<suite>/Release` and the `Packages` file it names.
//!
//! `apt` reads the same layout. A sync walks one or more distributions, then the
//! `.deb` files named by `Filename`.

use std::io::Read;

use flate2::read::GzDecoder;

/// Why an index could not be read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IndexError {
    /// The document is not an index the reader accepts.
    Syntax,
    /// A path leaves the repository root.
    Path,
}

/// `Packages` paths from a `Release` file, relative to `dists/<suite>/`.
///
/// `components` and `architectures` empty means every component and architecture.
/// A directory that has both `Packages` and `Packages.gz` keeps only the gzip file.
///
/// # Errors
/// [`IndexError::Path`] when a listed path leaves the distribution directory.
#[must_use]
pub fn package_indexes(
    release: &str,
    components: &[String],
    architectures: &[String],
) -> Result<Vec<String>, IndexError> {
    let mut plain = Vec::new();
    let mut gzip = Vec::new();
    for path in release_paths(release)? {
        let Some(kind) = packages_kind(&path) else {
            continue;
        };
        if !component_allowed(&path, components) || !architecture_allowed(&path, architectures) {
            continue;
        }
        match kind {
            PackagesKind::Plain => plain.push(path),
            PackagesKind::Gzip => gzip.push(path),
        }
    }
    let mut paths = gzip;
    for path in plain {
        let gzip_sibling = format!("{path}.gz");
        if !paths.iter().any(|kept| kept == &gzip_sibling) {
            paths.push(path);
        }
    }
    paths.sort();
    paths.dedup();
    Ok(paths)
}

/// `Filename` values from a plain (not gzip) `Packages` document.
///
/// # Errors
/// [`IndexError::Path`] when a filename leaves the repository root.
pub fn package_paths(packages: &str) -> Result<Vec<String>, IndexError> {
    let mut paths = Vec::new();
    for paragraph in packages.split("\n\n") {
        for line in paragraph.lines() {
            let Some(value) = line.strip_prefix("Filename:") else {
                continue;
            };
            let path = safe_path(value).ok_or(IndexError::Path)?;
            paths.push(path);
        }
    }
    Ok(paths)
}

/// Inflates a gzip body. A body that is not gzip is returned unchanged.
///
/// # Errors
/// The gzip member cannot be read.
pub fn inflate(bytes: &[u8]) -> Result<Vec<u8>, IndexError> {
    if bytes.len() >= 2 && bytes[0] == 0x1f && bytes[1] == 0x8b {
        let mut decoder = GzDecoder::new(bytes);
        let mut out = Vec::new();
        decoder.read_to_end(&mut out).map_err(|_| IndexError::Syntax)?;
        Ok(out)
    } else {
        Ok(bytes.to_vec())
    }
}

/// Name, version, and architecture of a `.deb` file (`name_version_arch.deb`).
#[must_use]
pub fn component_from_path(path: &str) -> Option<rustryx_format_core::ComponentSpec> {
    let file = path.rsplit('/').next().unwrap_or(path);
    let stem = file.strip_suffix(".deb")?;
    let (name_version, arch) = stem.rsplit_once('_')?;
    let (name, version) = name_version.rsplit_once('_')?;
    if name.is_empty() || version.is_empty() || arch.is_empty() {
        return None;
    }
    Some(rustryx_format_core::ComponentSpec {
        group: Some(arch.to_owned()),
        name: name.to_owned(),
        version: Some(version.to_owned()),
    })
}

enum PackagesKind {
    Plain,
    Gzip,
}

fn packages_kind(path: &str) -> Option<PackagesKind> {
    if path.ends_with("/Packages.gz") {
        Some(PackagesKind::Gzip)
    } else if path.ends_with("/Packages") {
        Some(PackagesKind::Plain)
    } else {
        None
    }
}

fn component_allowed(path: &str, components: &[String]) -> bool {
    if components.is_empty() {
        return true;
    }
    let component = path.split('/').next().unwrap_or("");
    components.iter().any(|item| item == component)
}

fn architecture_allowed(path: &str, architectures: &[String]) -> bool {
    if architectures.is_empty() {
        return true;
    }
    architectures.iter().any(|arch| path.contains(&format!("/binary-{arch}/")))
}

fn release_paths(release: &str) -> Result<Vec<String>, IndexError> {
    let mut paths = Vec::new();
    let mut in_files = false;
    for line in release.lines() {
        if line.starts_with("-----") {
            continue;
        }
        if !line.starts_with(' ') {
            in_files = matches!(line.trim(), "SHA256:" | "SHA512:" | "MD5Sum:");
            continue;
        }
        if !in_files {
            continue;
        }
        let mut parts = line.split_whitespace();
        let Some(_hash) = parts.next() else {
            continue;
        };
        let Some(_size) = parts.next() else {
            continue;
        };
        let Some(path) = parts.next() else {
            continue;
        };
        if parts.next().is_some() {
            return Err(IndexError::Syntax);
        }
        paths.push(safe_path(path).ok_or(IndexError::Path)?);
    }
    Ok(paths)
}

fn safe_path(href: &str) -> Option<String> {
    let href = href.trim().trim_start_matches('/');
    if href.is_empty() || href.contains('\\') || href.contains("://") {
        return None;
    }
    if href.split('/').any(|part| part.is_empty() || part == "." || part == "..") {
        return None;
    }
    Some(href.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    const RELEASE: &str = "\
Origin: Debian
SHA256:
 aaaa 10 main/binary-amd64/Packages
 bbbb 4 main/binary-amd64/Packages.gz
 cccc 4 contrib/binary-arm64/Packages.gz
 dddd 4 ../secret
";

    #[test]
    fn a_path_that_escapes_the_repository_is_refused() {
        assert_eq!(package_indexes(RELEASE, &[], &[]), Err(IndexError::Path));
    }

    #[test]
    fn gzip_is_preferred_and_filters_apply() {
        let release = "\
SHA256:
 aaaa 10 main/binary-amd64/Packages
 bbbb 4 main/binary-amd64/Packages.gz
 cccc 4 main/binary-arm64/Packages.gz
 dddd 4 contrib/binary-amd64/Packages.gz
";
        let indexes = package_indexes(
            release,
            &["main".to_owned()],
            &["amd64".to_owned()],
        )
        .unwrap();
        assert_eq!(indexes, vec!["main/binary-amd64/Packages.gz".to_owned()]);
    }

    #[test]
    fn filenames_are_package_paths() {
        let packages = "\
Package: bash\n\
Version: 5.2-1\n\
Filename: pool/main/b/bash/bash_5.2-1_amd64.deb\n\
\n\
Package: bad\n\
Filename: pool/../x.deb\n\
";
        assert_eq!(package_paths(packages), Err(IndexError::Path));
        let packages = "\
Package: bash\n\
Filename: pool/main/b/bash/bash_5.2-1_amd64.deb\n\
";
        assert_eq!(
            package_paths(packages).unwrap(),
            vec!["pool/main/b/bash/bash_5.2-1_amd64.deb".to_owned()]
        );
        let spec = component_from_path("pool/main/b/bash/bash_5.2-1_amd64.deb").unwrap();
        assert_eq!(spec.name, "bash");
        assert_eq!(spec.version.as_deref(), Some("5.2-1"));
        assert_eq!(spec.group.as_deref(), Some("amd64"));
    }
}
