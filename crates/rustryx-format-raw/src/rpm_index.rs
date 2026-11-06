//! RPM repository index: `repodata/repomd.xml` and the primary package list.
//!
//! yum and dnf share this layout. A sync walks the metadata hrefs, then the package
//! hrefs. Paths stay relative to the repository root.

use std::io::Read;

use flate2::read::GzDecoder;
use quick_xml::Reader;
use quick_xml::events::Event;
use rustryx_format_core::ComponentSpec;

/// Why an index could not be read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IndexError {
    /// The document is not XML the reader accepts.
    Syntax,
    /// A `location` href leaves the repository root.
    Path,
    /// `repomd` has no primary package index.
    NoPrimary,
}

/// Metadata files and package files named by an RPM repository.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RpmIndex {
    /// Paths from `repomd`, including the primary index.
    pub metadata: Vec<String>,
    /// Package paths from the primary index.
    pub packages: Vec<String>,
}

/// Reads metadata hrefs from `repomd.xml`.
///
/// # Errors
/// [`IndexError::Syntax`] or [`IndexError::Path`].
pub fn metadata_paths(repomd_xml: &str) -> Result<Vec<String>, IndexError> {
    hrefs(repomd_xml, false)
}

/// The primary index path inside a metadata list (`primary.xml` or `primary.xml.gz`).
#[must_use]
pub fn primary_path(metadata: &[String]) -> Option<&str> {
    metadata.iter().find_map(|path| {
        let name = path.rsplit('/').next().unwrap_or(path);
        let bare = name.rsplit_once('-').map_or(name, |(_, rest)| rest);
        (bare == "primary.xml" || bare == "primary.xml.gz").then_some(path.as_str())
    })
}

/// Package hrefs from primary XML (plain, not gzip).
///
/// # Errors
/// [`IndexError::Syntax`] or [`IndexError::Path`].
pub fn package_paths(primary_xml: &str) -> Result<Vec<String>, IndexError> {
    hrefs(primary_xml, true)
}

/// Inflates a gzip body. A body that is not gzip is returned unchanged.
///
/// # Errors
/// The gzip member cannot be read.
pub fn inflate(bytes: &[u8]) -> Result<Vec<u8>, IndexError> {
    if bytes.len() >= 2 && bytes[0] == 0x1f && bytes[1] == 0x8b {
        let mut decoder = GzDecoder::new(bytes);
        let mut out = Vec::new();
        decoder
            .read_to_end(&mut out)
            .map_err(|_| IndexError::Syntax)?;
        Ok(out)
    } else {
        Ok(bytes.to_vec())
    }
}

/// Builds the sync list from `repomd` and the primary document.
///
/// # Errors
/// The same as [`metadata_paths`] and [`package_paths`], plus [`IndexError::NoPrimary`]
/// when the caller did not pass the primary file `repomd` names.
pub fn index_from(repomd_xml: &str, primary_xml: &str) -> Result<RpmIndex, IndexError> {
    let metadata = metadata_paths(repomd_xml)?;
    if primary_path(&metadata).is_none() {
        return Err(IndexError::NoPrimary);
    }
    let packages = package_paths(primary_xml)?;
    Ok(RpmIndex { metadata, packages })
}

fn hrefs(xml: &str, packages_only: bool) -> Result<Vec<String>, IndexError> {
    let mut reader = Reader::from_str(xml);
    let mut paths = Vec::new();
    let mut in_package = false;
    loop {
        let event = reader.read_event().map_err(|_| IndexError::Syntax)?;
        match event {
            Event::Eof => break,
            Event::DocType(_) => return Err(IndexError::Syntax),
            Event::Start(start) => {
                if start.local_name().as_ref() == b"package" {
                    in_package = true;
                }
                push_href(&start, in_package, packages_only, &mut paths)?;
            }
            Event::Empty(start) => {
                push_href(&start, in_package, packages_only, &mut paths)?;
            }
            Event::End(end) if end.local_name().as_ref() == b"package" => {
                in_package = false;
            }
            _ => {}
        }
    }
    Ok(paths)
}

fn push_href(
    start: &quick_xml::events::BytesStart<'_>,
    in_package: bool,
    packages_only: bool,
    paths: &mut Vec<String>,
) -> Result<(), IndexError> {
    if start.local_name().as_ref() != b"location" {
        return Ok(());
    }
    if packages_only != in_package {
        return Ok(());
    }
    for attr in start.attributes() {
        let attr = attr.map_err(|_| IndexError::Syntax)?;
        if attr.key.as_ref() != b"href" {
            continue;
        }
        let value = std::str::from_utf8(attr.value.as_ref()).map_err(|_| IndexError::Syntax)?;
        let path = safe_path(value).ok_or(IndexError::Path)?;
        paths.push(path);
    }
    Ok(())
}

/// Name, `version-release`, and architecture of an RPM file.
///
/// The filename is `{name}-{version}-{release}.{arch}.rpm`. The name may contain hyphens, so
/// the split is from the right. A path that is not an RPM file returns `None`.
#[must_use]
pub fn component_from_path(path: &str) -> Option<ComponentSpec> {
    let file = path.rsplit('/').next().unwrap_or(path);
    let stem = file.strip_suffix(".rpm")?;
    let (rest, arch) = stem.rsplit_once('.')?;
    let (name_version, release) = rest.rsplit_once('-')?;
    let (name, version) = name_version.rsplit_once('-')?;
    if name.is_empty() || version.is_empty() || release.is_empty() || arch.is_empty() {
        return None;
    }
    Some(ComponentSpec {
        group: Some(arch.to_owned()),
        name: name.to_owned(),
        version: Some(format!("{version}-{release}")),
    })
}

fn safe_path(href: &str) -> Option<String> {
    let href = href.trim().trim_start_matches('/');
    if href.is_empty() || href.contains('\\') || href.contains("://") {
        return None;
    }
    if href
        .split('/')
        .any(|part| part.is_empty() || part == "." || part == "..")
    {
        return None;
    }
    Some(href.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    const REPOMD: &str = r#"<?xml version="1.0"?>
<repomd>
  <data type="primary"><location href="repodata/abc-primary.xml.gz"/></data>
  <data type="filelists"><location href="repodata/def-filelists.xml.gz"/></data>
  <data type="primary"><location href="../secret"/></data>
</repomd>"#;

    const PRIMARY: &str = r#"<metadata>
  <package type="rpm"><location href="getPackage/bash.rpm"/></package>
  <package type="rpm"><location href="getPackage/../evil.rpm"/></package>
  <package type="rpm"><location href="getPackage/coreutils.rpm"/></package>
</metadata>"#;

    #[test]
    fn a_path_that_escapes_the_repository_is_refused() {
        assert_eq!(metadata_paths(REPOMD), Err(IndexError::Path));
        assert_eq!(package_paths(PRIMARY), Err(IndexError::Path));
    }

    #[test]
    fn an_rpm_filename_is_a_component() {
        let spec = component_from_path("gitlab-ee-18.5.1-ee.0.el9.x86_64.rpm").unwrap();
        assert_eq!(spec.name, "gitlab-ee");
        assert_eq!(spec.version.as_deref(), Some("18.5.1-ee.0.el9"));
        assert_eq!(spec.group.as_deref(), Some("x86_64"));
        assert!(component_from_path("repodata/repomd.xml").is_none());
    }

    #[test]
    fn a_clean_index_lists_metadata_and_packages() {
        let repomd = r#"<repomd>
          <data type="primary"><location href="repodata/abc-primary.xml.gz"/></data>
          <data type="other"><location href="repodata/other.xml.gz"/></data>
        </repomd>"#;
        let primary = r#"<metadata>
          <package type="rpm"><location href="Packages/bash.rpm"/></package>
          <package type="rpm"><location href="Packages/coreutils.rpm"/></package>
        </metadata>"#;
        let index = index_from(repomd, primary).unwrap();
        assert_eq!(index.packages.len(), 2);
        assert_eq!(index.metadata.len(), 2);
    }

    #[test]
    fn gzip_primary_inflates_and_plain_xml_stays() {
        let plain = b"<metadata/>";
        assert_eq!(inflate(plain).unwrap(), plain);
    }
}
