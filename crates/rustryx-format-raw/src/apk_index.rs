//! Alpine index: `APKINDEX.tar.gz` lists packages as `P:` and `V:` lines.
//!
//! The package file is `<arch>/<name>-<version>.apk`.

use std::io::Read;

use flate2::read::MultiGzDecoder;

/// Why an index could not be read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IndexError {
    /// The archive or the index text is not readable.
    Syntax,
    /// A package name or version is not a safe path segment.
    Path,
}

/// Package paths `<name>-<version>.apk` from the `APKINDEX` text.
///
/// The caller prefixes the architecture directory.
///
/// # Errors
/// [`IndexError::Path`] when a name or version is not a single safe segment.
pub fn package_filenames(index: &str) -> Result<Vec<String>, IndexError> {
    let mut paths = Vec::new();
    for paragraph in index.split("\n\n") {
        let mut name = None;
        let mut version = None;
        for line in paragraph.lines() {
            if let Some(value) = line.strip_prefix("P:") {
                name = Some(value.trim());
            } else if let Some(value) = line.strip_prefix("V:") {
                version = Some(value.trim());
            }
        }
        let (Some(name), Some(version)) = (name, version) else {
            continue;
        };
        if !safe_segment(name) || !safe_segment(version) {
            return Err(IndexError::Path);
        }
        paths.push(format!("{name}-{version}.apk"));
    }
    Ok(paths)
}

/// Reads the `APKINDEX` member of a gzip-compressed tar archive.
///
/// # Errors
/// The gzip member or the tar archive cannot be read, or it has no `APKINDEX`.
pub fn apkindex_text(bytes: &[u8]) -> Result<String, IndexError> {
    let inflated = inflate(bytes)?;
    let member = tar_member(&inflated, "APKINDEX").ok_or(IndexError::Syntax)?;
    String::from_utf8(member).map_err(|_| IndexError::Syntax)
}

/// Name and `version` of an `.apk` file. The architecture is the parent directory.
#[must_use]
pub fn component_from_path(path: &str) -> Option<rustryx_format_core::ComponentSpec> {
    let (dir, file) = path.rsplit_once('/').unwrap_or(("", path));
    let stem = file.strip_suffix(".apk")?;
    let revision = stem.rfind("-r")?;
    let revision_digits = &stem[revision + 2..];
    if revision_digits.is_empty() || !revision_digits.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let before = &stem[..revision];
    let dash = before.rfind('-')?;
    let name = &before[..dash];
    let version = &stem[dash + 1..];
    if name.is_empty() || version.is_empty() {
        return None;
    }
    let arch = if dir.is_empty() {
        None
    } else {
        Some(dir.rsplit('/').next().unwrap_or(dir).to_owned())
    };
    Some(rustryx_format_core::ComponentSpec {
        group: arch,
        name: name.to_owned(),
        version: Some(version.to_owned()),
    })
}

fn inflate(bytes: &[u8]) -> Result<Vec<u8>, IndexError> {
    if bytes.len() < 2 || bytes[0] != 0x1f || bytes[1] != 0x8b {
        return Ok(bytes.to_vec());
    }
    // Alpine appends the signature as another gzip member. `MultiGzDecoder` joins them.
    let mut decoder = MultiGzDecoder::new(bytes);
    let mut out = Vec::new();
    decoder.read_to_end(&mut out).map_err(|_| IndexError::Syntax)?;
    if out.is_empty() {
        return Err(IndexError::Syntax);
    }
    Ok(out)
}

fn safe_segment(value: &str) -> bool {
    !value.is_empty()
        && !value.contains('/')
        && !value.contains('\\')
        && !value.contains("://")
        && value != "."
        && value != ".."
}

/// A ustar member by exact name. Checksums are not verified: a damaged archive fails
/// when the text is not a package index.
fn tar_member(archive: &[u8], wanted: &str) -> Option<Vec<u8>> {
    let mut offset = 0;
    while offset + 512 <= archive.len() {
        let header = &archive[offset..offset + 512];
        offset += 512;
        if header.iter().all(|byte| *byte == 0) {
            return None;
        }
        let name = header_text(&header[..100]);
        let size = header_octal(&header[124..136])?;
        let typeflag = header[156];
        let data_end = offset.checked_add(size)?;
        if data_end > archive.len() {
            return None;
        }
        let data = archive[offset..data_end].to_vec();
        offset = data_end.div_ceil(512) * 512;
        if (typeflag == b'0' || typeflag == 0) && (name == wanted || name.ends_with(&format!("/{wanted}")))
        {
            return Some(data);
        }
    }
    None
}

fn header_text(bytes: &[u8]) -> String {
    let end = bytes.iter().position(|byte| *byte == 0).unwrap_or(bytes.len());
    String::from_utf8_lossy(&bytes[..end]).trim().to_owned()
}

fn header_octal(bytes: &[u8]) -> Option<usize> {
    let text = header_text(bytes);
    if text.is_empty() {
        return Some(0);
    }
    usize::from_str_radix(text.trim(), 8).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use flate2::Compression;
    use flate2::write::GzEncoder;
    use std::io::Write;

    #[test]
    fn an_index_lists_package_files() {
        let index = "C:Q1abc\nP:musl\nV:1.2.5-r0\nA:x86_64\n\nP:bad/name\nV:1\n";
        assert_eq!(package_filenames(index), Err(IndexError::Path));
        let index = "C:Q1abc\nP:musl\nV:1.2.5-r0\n\nP:busybox\nV:1.36.1-r29\n";
        assert_eq!(
            package_filenames(index).unwrap(),
            vec![
                "musl-1.2.5-r0.apk".to_owned(),
                "busybox-1.36.1-r29.apk".to_owned(),
            ]
        );
        let spec = component_from_path("x86_64/busybox-1.36.1-r29.apk").unwrap();
        assert_eq!(spec.name, "busybox");
        assert_eq!(spec.version.as_deref(), Some("1.36.1-r29"));
        assert_eq!(spec.group.as_deref(), Some("x86_64"));
    }

    #[test]
    fn a_tar_gz_yields_the_apkindex_member() {
        let text = b"P:musl\nV:1.2.5-r0\n";
        let archive = gzip_tar("APKINDEX", text);
        assert_eq!(apkindex_text(&archive).unwrap(), "P:musl\nV:1.2.5-r0\n");
    }

    fn gzip_tar(name: &str, body: &[u8]) -> Vec<u8> {
        let mut header = vec![0_u8; 512];
        header[..name.len()].copy_from_slice(name.as_bytes());
        let size = format!("{:o}", body.len());
        header[124..124 + size.len()].copy_from_slice(size.as_bytes());
        header[156] = b'0';
        for byte in &mut header[148..156] {
            *byte = b' ';
        }
        let sum: u32 = header.iter().map(|byte| u32::from(*byte)).sum();
        let checksum = format!("{sum:06o}\0 ");
        header[148..156].copy_from_slice(checksum.as_bytes());
        let mut raw = header;
        raw.extend_from_slice(body);
        while raw.len() % 512 != 0 {
            raw.push(0);
        }
        raw.extend(std::iter::repeat_n(0_u8, 1024));
        let mut encoder = GzEncoder::new(Vec::new(), Compression::fast());
        encoder.write_all(&raw).unwrap();
        encoder.finish().unwrap()
    }
}
