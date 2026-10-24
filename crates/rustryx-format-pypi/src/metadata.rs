//! Core metadata (`METADATA`, `PKG-INFO`): the header block and the fields this repository uses.
//!
//! Specification: <https://packaging.python.org/en/latest/specifications/core-metadata/>. The file
//! is an RFC 822 style header block (`Field-Name: value`, continuation lines start with a space
//! or a tab) followed by an optional blank line and a body, which holds the long description in
//! metadata 2.1 and later. Only the header block is read; the body is never decoded.
//!
//! The reader is strict: a header line without a colon, a continuation without a header, a
//! repeated `Name`, `Version`, `Metadata-Version` or `Requires-Python`, a name or version that
//! is not valid, and invalid UTF-8 in the header block are all errors. The input size is the
//! caller's bound (it comes from the archive entry that was inflated under a limit).

use crate::name::ProjectName;
use crate::version::Version;

/// Most header lines read. Real files have tens; the cap bounds the work on hostile input.
const MAX_HEADER_LINES: usize = 20_000;
/// Longest `Requires-Python` value kept; it is written into index pages.
const MAX_REQUIRES_PYTHON_LEN: usize = 256;

/// Why a metadata file was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum MetadataError {
    /// The header block is not valid UTF-8.
    #[error("metadata is not valid UTF-8")]
    NotUtf8,
    /// A header line is malformed, or there are too many.
    #[error("malformed metadata header")]
    Malformed,
    /// A required field is missing.
    #[error("metadata field {0} is missing")]
    Missing(&'static str),
    /// A field that must appear once appears more than once.
    #[error("metadata field {0} is repeated")]
    Repeated(&'static str),
    /// `Metadata-Version` is not `major.minor`.
    #[error("invalid Metadata-Version")]
    BadMetadataVersion,
    /// `Name` is not a valid project name.
    #[error("invalid project name in metadata")]
    BadName,
    /// `Version` is not a PEP 440 version.
    #[error("invalid version in metadata")]
    BadVersion,
    /// `Requires-Python` is empty, too long or has characters that cannot appear in a specifier.
    #[error("invalid Requires-Python in metadata")]
    BadRequiresPython,
}

/// The fields of a core metadata file that the repository reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CoreMetadata {
    /// `Metadata-Version`, for example `2.1`.
    pub metadata_version: String,
    /// `Name`, normalized.
    pub name: ProjectName,
    /// `Version`.
    pub version: Version,
    /// `Requires-Python`, as written.
    pub requires_python: Option<String>,
}

impl CoreMetadata {
    /// Reads the header block of `bytes`.
    ///
    /// # Errors
    /// [`MetadataError`] for the first problem found.
    pub fn parse(bytes: &[u8]) -> Result<Self, MetadataError> {
        let header_end = header_block_end(bytes);
        let block = bytes.get(..header_end).unwrap_or_default();
        let text = std::str::from_utf8(block).map_err(|_| MetadataError::NotUtf8)?;
        let fields = read_fields(text)?;

        let metadata_version = single(&fields, "metadata-version", "Metadata-Version")?
            .ok_or(MetadataError::Missing("Metadata-Version"))?;
        if !is_metadata_version(metadata_version) {
            return Err(MetadataError::BadMetadataVersion);
        }
        let name = single(&fields, "name", "Name")?.ok_or(MetadataError::Missing("Name"))?;
        let name = ProjectName::parse(name).map_err(|_| MetadataError::BadName)?;
        let version =
            single(&fields, "version", "Version")?.ok_or(MetadataError::Missing("Version"))?;
        let version = Version::parse(version).map_err(|_| MetadataError::BadVersion)?;
        let requires_python = single(&fields, "requires-python", "Requires-Python")?
            .map(validate_requires_python)
            .transpose()?;
        Ok(Self {
            metadata_version: metadata_version.to_owned(),
            name,
            version,
            requires_python,
        })
    }
}

/// Offset of the first blank line, or the whole input when there is none: the header block ends
/// there and everything after it is the body.
fn header_block_end(bytes: &[u8]) -> usize {
    let mut start = 0;
    while start <= bytes.len() {
        let rest = bytes.get(start..).unwrap_or_default();
        let line_len = rest.iter().position(|b| *b == b'\n').unwrap_or(rest.len());
        let line = rest.get(..line_len).unwrap_or_default();
        let line = line.strip_suffix(b"\r").unwrap_or(line);
        if line.is_empty() {
            return start;
        }
        if line_len == rest.len() {
            return bytes.len();
        }
        start += line_len + 1;
    }
    bytes.len()
}

fn read_fields(text: &str) -> Result<Vec<(String, String)>, MetadataError> {
    let mut fields: Vec<(String, String)> = Vec::new();
    for line in text.split('\n') {
        let line = line.strip_suffix('\r').unwrap_or(line);
        if line.is_empty() {
            continue;
        }
        if fields.len() >= MAX_HEADER_LINES {
            return Err(MetadataError::Malformed);
        }
        if line.starts_with([' ', '\t']) {
            let last = fields.last_mut().ok_or(MetadataError::Malformed)?;
            // RFC 822 unfolding: the line break goes, the whitespace stays one space.
            last.1.push(' ');
            last.1.push_str(line.trim_start());
            continue;
        }
        let (name, value) = line.split_once(':').ok_or(MetadataError::Malformed)?;
        if name.is_empty() || !name.bytes().all(|b| b.is_ascii_graphic()) {
            return Err(MetadataError::Malformed);
        }
        fields.push((name.to_ascii_lowercase(), value.trim().to_owned()));
    }
    Ok(fields)
}

/// The value of a field that may appear at most once.
fn single<'a>(
    fields: &'a [(String, String)],
    lowered: &str,
    shown: &'static str,
) -> Result<Option<&'a str>, MetadataError> {
    let mut found = fields
        .iter()
        .filter(|(name, _)| name == lowered)
        .map(|(_, value)| value.as_str());
    let first = found.next();
    if found.next().is_some() {
        return Err(MetadataError::Repeated(shown));
    }
    Ok(first)
}

fn is_metadata_version(text: &str) -> bool {
    text.split_once('.').is_some_and(|(major, minor)| {
        !major.is_empty()
            && !minor.is_empty()
            && major.len() <= 3
            && minor.len() <= 3
            && major
                .bytes()
                .chain(minor.bytes())
                .all(|b| b.is_ascii_digit())
    })
}

fn validate_requires_python(value: &str) -> Result<String, MetadataError> {
    let printable = value.bytes().all(|b| b.is_ascii_graphic() || b == b' ');
    if value.is_empty() || value.len() > MAX_REQUIRES_PYTHON_LEN || !printable {
        return Err(MetadataError::BadRequiresPython);
    }
    Ok(value.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    const SAMPLE: &str = "Metadata-Version: 2.1\nName: Foo_Bar\nVersion: 1.0.0\nSummary: x\nRequires-Python: >=3.8\nClassifier: A\nClassifier: B\n\nLong description\nName: ignored\n";

    #[test]
    fn reads_the_fields_in_use() {
        let meta = CoreMetadata::parse(SAMPLE.as_bytes()).unwrap();
        assert_eq!(meta.metadata_version, "2.1");
        assert_eq!(meta.name.as_str(), "foo-bar");
        assert_eq!(meta.version.to_string(), "1.0.0");
        assert_eq!(meta.requires_python.as_deref(), Some(">=3.8"));
    }

    #[test]
    fn the_body_is_not_read() {
        // A second `Name:` in the body is description text, and bytes that are not UTF-8 there
        // do not matter.
        let mut text = SAMPLE.as_bytes().to_vec();
        text.extend_from_slice(&[0xff, 0xfe, b'\n']);
        assert!(CoreMetadata::parse(&text).is_ok());
    }

    #[test]
    fn accepts_crlf_and_folded_lines() {
        let text = "Metadata-Version: 1.2\r\nName: a\r\nVersion: 2\r\nRequires-Python: >=3.8,\r\n  <4\r\n\r\nbody";
        let meta = CoreMetadata::parse(text.as_bytes()).unwrap();
        assert_eq!(meta.requires_python.as_deref(), Some(">=3.8, <4"));
    }

    #[test]
    fn a_header_block_without_a_body_is_fine() {
        let meta = CoreMetadata::parse(b"Metadata-Version: 2.4\nName: a\nVersion: 1").unwrap();
        assert_eq!(meta.requires_python, None);
    }

    #[test]
    fn refuses_what_it_cannot_trust() {
        let cases: [(&str, MetadataError); 14] = [
            ("", MetadataError::Missing("Metadata-Version")),
            (
                "Name: a\nVersion: 1\n",
                MetadataError::Missing("Metadata-Version"),
            ),
            (
                "Metadata-Version: 2.1\nVersion: 1\n",
                MetadataError::Missing("Name"),
            ),
            (
                "Metadata-Version: 2.1\nName: a\n",
                MetadataError::Missing("Version"),
            ),
            (
                "Metadata-Version: 2\nName: a\nVersion: 1\n",
                MetadataError::BadMetadataVersion,
            ),
            (
                "Metadata-Version: x.y\nName: a\nVersion: 1\n",
                MetadataError::BadMetadataVersion,
            ),
            (
                "Metadata-Version: 2.1\nName: -a\nVersion: 1\n",
                MetadataError::BadName,
            ),
            (
                "Metadata-Version: 2.1\nName: a\nVersion: one\n",
                MetadataError::BadVersion,
            ),
            (
                "Metadata-Version: 2.1\nName: a\nName: b\nVersion: 1\n",
                MetadataError::Repeated("Name"),
            ),
            (
                "Metadata-Version: 2.1\nName: a\nVersion: 1\nVersion: 2\n",
                MetadataError::Repeated("Version"),
            ),
            (
                "Metadata-Version: 2.1\nName: a\nVersion: 1\nRequires-Python: \n",
                MetadataError::BadRequiresPython,
            ),
            (
                "Metadata-Version: 2.1\nName a\nVersion: 1\n",
                MetadataError::Malformed,
            ),
            (" folded first\nName: a\n", MetadataError::Malformed),
            (
                "Metadata-Version: 2.1\nName: a\nVersion: 1\nRequires-Python: >=3\u{1}\n",
                MetadataError::BadRequiresPython,
            ),
        ];
        for (text, error) in cases {
            assert_eq!(CoreMetadata::parse(text.as_bytes()), Err(error), "{text:?}");
        }
        assert_eq!(
            CoreMetadata::parse(&[b'N', b':', 0xff, b'\n']),
            Err(MetadataError::NotUtf8)
        );
        let long = format!(
            "Metadata-Version: 2.1\nName: a\nVersion: 1\nRequires-Python: {}\n",
            ">".repeat(MAX_REQUIRES_PYTHON_LEN + 1)
        );
        assert_eq!(
            CoreMetadata::parse(long.as_bytes()),
            Err(MetadataError::BadRequiresPython)
        );
    }

    #[test]
    fn too_many_header_lines_are_refused() {
        let mut text = String::from("Metadata-Version: 2.1\nName: a\nVersion: 1\n");
        for i in 0..=MAX_HEADER_LINES {
            text.push_str(&format!("X-{i}: v\n"));
        }
        assert_eq!(
            CoreMetadata::parse(text.as_bytes()),
            Err(MetadataError::Malformed)
        );
    }

    proptest! {
        #[test]
        fn parse_never_panics(bytes in proptest::collection::vec(any::<u8>(), 0..400)) {
            let _ = CoreMetadata::parse(&bytes);
        }

        #[test]
        fn parse_never_panics_on_header_like_text(text in "([A-Za-z-]{0,12}: ?[ -~]{0,20}\r?\n?){0,12}") {
            let _ = CoreMetadata::parse(text.as_bytes());
        }

        #[test]
        fn name_and_version_round_trip(
            name in "[A-Za-z][A-Za-z0-9._-]{0,20}[A-Za-z0-9]",
            major in 0u32..50, minor in 0u32..50,
        ) {
            let text = format!("Metadata-Version: 2.1\nName: {name}\nVersion: {major}.{minor}\n");
            let meta = CoreMetadata::parse(text.as_bytes()).unwrap();
            prop_assert_eq!(meta.name.as_str(), crate::name::normalize(&name));
            prop_assert_eq!(meta.version.to_string(), format!("{major}.{minor}"));
        }
    }
}
