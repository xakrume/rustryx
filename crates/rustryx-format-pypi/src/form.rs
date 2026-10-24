//! The form of the legacy upload API: which fields are read, what they must look like.
//!
//! Specification: the legacy upload API as documented at <https://docs.pypi.org/api/upload/>:
//! a `multipart/form-data` `POST` with `:action=file_upload`, `protocol_version=1`, the
//! metadata fields (`name`, `version`, `metadata_version`, `filetype`, `pyversion`,
//! `requires_python`), at least one digest (`md5_digest`, `sha256_digest`,
//! `blake2_256_digest`) and the file in the part named `content`.
//!
//! Only a fixed set of fields is retained, each bounded; every other field (`description`,
//! `classifiers`, a `gpg_signature` part, ...) is counted against a budget and dropped, so a
//! form cannot make the server hold what it does not use. A field that appears twice is an error:
//! two values for `name` would let a client present one to authorization and another to storage.

use std::collections::BTreeMap;
use std::fmt::Display;

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use bytes::Bytes;
use futures::Stream;

use crate::filename::DistKind;
use crate::multipart::{MultipartError, MultipartReader, PartHead};
use crate::name::ProjectName;
use crate::version::Version;

/// The only fields kept.
const RETAINED_FIELDS: [&str; 11] = [
    ":action",
    "protocol_version",
    "name",
    "version",
    "filetype",
    "pyversion",
    "metadata_version",
    "requires_python",
    "md5_digest",
    "sha256_digest",
    "blake2_256_digest",
];
/// Longest value of a retained field.
pub const MAX_FIELD_BYTES: usize = 4096;
/// Total size of all dropped fields before the file.
pub const MAX_DROPPED_FORM_BYTES: u64 = 16 * 1024 * 1024;
/// The name of the part that holds the distribution.
pub const CONTENT_FIELD: &str = "content";

/// Why the form was refused. The text is shown to the client, so it names the field and never
/// echoes its value.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum FormError {
    /// The multipart framing is broken or the body ended early.
    #[error("{0}")]
    Multipart(MultipartError),
    /// The size budget for the fields is exhausted or a field is over its bound.
    #[error("upload form fields are too large")]
    TooLarge,
    /// A field that is read appears more than once.
    #[error("form field {0} is repeated")]
    Repeated(String),
    /// A retained field is not valid UTF-8.
    #[error("form field {0} is not valid UTF-8")]
    NotUtf8(String),
    /// The body has no `content` part with a file name.
    #[error("the form has no content file")]
    NoContent,
    /// `:action` is not `file_upload`.
    #[error("unsupported :action, only file_upload is accepted")]
    UnsupportedAction,
    /// `protocol_version` is present and not `1`.
    #[error("unsupported protocol_version")]
    UnsupportedProtocol,
    /// A required field is missing.
    #[error("form field {0} is required")]
    Missing(&'static str),
    /// `name` is not a valid project name.
    #[error("form field name is not a valid project name")]
    BadName,
    /// `version` is not a PEP 440 version.
    #[error("form field version is not a valid version")]
    BadVersion,
    /// `filetype` is neither `bdist_wheel` nor `sdist`.
    #[error("unsupported filetype, only bdist_wheel and sdist are accepted")]
    BadFiletype,
    /// A digest field is not a digest of the right form.
    #[error("form field {0} is not a valid digest")]
    BadDigest(&'static str),
    /// `requires_python` or `metadata_version` has an unusable value.
    #[error("form field {0} has an invalid value")]
    BadValue(&'static str),
}

/// Digests the client declared. A digest that was sent is verified against the bytes; sending
/// none is allowed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DeclaredDigests {
    /// `sha256_digest` (hex).
    pub sha256: Option<[u8; 32]>,
    /// `md5_digest` (hex, or URL-safe base64 without padding as the `PyPI` documentation says).
    pub md5: Option<[u8; 16]>,
    /// `blake2_256_digest` (hex).
    pub blake2_256: Option<[u8; 32]>,
}

/// The typed fields of an upload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UploadFields {
    /// The project name, normalized.
    pub name: ProjectName,
    /// The version.
    pub version: Version,
    /// Wheel or sdist.
    pub filetype: DistKind,
    /// `requires_python`, as sent, when present and not empty.
    pub requires_python: Option<String>,
    /// `metadata_version`, when present.
    pub metadata_version: Option<String>,
    /// The declared digests.
    pub digests: DeclaredDigests,
}

/// The retained raw fields, by name.
pub type RawFields = BTreeMap<String, String>;

/// What precedes the file in the form.
pub struct FormPrefix<S> {
    /// The retained fields that came before the file.
    pub fields: RawFields,
    /// The header of the `content` part.
    pub content: PartHead,
    /// The reader, positioned at the start of the file's bytes.
    pub reader: MultipartReader<S>,
}

/// Reads the form up to the start of the file.
///
/// The fields that decide where the file goes must come before it (the file is streamed and its
/// destination has to be known before its first byte); every client of the legacy API writes
/// the file part last. A form whose file part comes first reaches the end of the stream
/// without a second part and is [`FormError::NoContent`] for the fields, which are then missing.
///
/// # Errors
/// [`FormError`].
pub async fn read_prefix<S, E>(mut reader: MultipartReader<S>) -> Result<FormPrefix<S>, FormError>
where
    S: Stream<Item = Result<Bytes, E>> + Unpin,
    E: Display,
{
    let mut fields = RawFields::new();
    let mut budget = MAX_DROPPED_FORM_BYTES;
    while let Some(head) = reader.next_part().await.map_err(FormError::Multipart)? {
        if head.name == CONTENT_FIELD {
            if head.filename.is_none() {
                return Err(FormError::NoContent);
            }
            return Ok(FormPrefix {
                fields,
                content: head,
                reader,
            });
        }
        if RETAINED_FIELDS.contains(&head.name.as_str()) {
            let value = read_field(&mut reader, &head.name).await?;
            if fields.insert(head.name.clone(), value).is_some() {
                return Err(FormError::Repeated(head.name));
            }
        } else {
            let dropped = reader
                .skip_body(budget)
                .await
                .map_err(|error| match error {
                    MultipartError::Malformed("part is too large") => FormError::TooLarge,
                    other => FormError::Multipart(other),
                })?;
            budget = budget.saturating_sub(dropped);
        }
    }
    Err(FormError::NoContent)
}

async fn read_field<S, E>(reader: &mut MultipartReader<S>, name: &str) -> Result<String, FormError>
where
    S: Stream<Item = Result<Bytes, E>> + Unpin,
    E: Display,
{
    let mut value = Vec::new();
    while let Some(chunk) = reader.next_chunk().await.map_err(FormError::Multipart)? {
        if value.len().saturating_add(chunk.len()) > MAX_FIELD_BYTES {
            return Err(FormError::TooLarge);
        }
        value.extend_from_slice(&chunk);
    }
    String::from_utf8(value).map_err(|_| FormError::NotUtf8(name.to_owned()))
}

impl UploadFields {
    /// Validates the retained fields.
    ///
    /// # Errors
    /// [`FormError`] for the first field that is missing or invalid.
    pub fn from_raw(raw: &RawFields) -> Result<Self, FormError> {
        let get = |key: &str| raw.get(key).map(|v| v.trim());
        match get(":action") {
            Some("file_upload") => {}
            _ => return Err(FormError::UnsupportedAction),
        }
        if get("protocol_version").is_some_and(|v| v != "1") {
            return Err(FormError::UnsupportedProtocol);
        }
        let name = ProjectName::parse(get("name").ok_or(FormError::Missing("name"))?)
            .map_err(|_| FormError::BadName)?;
        let version = Version::parse(get("version").ok_or(FormError::Missing("version"))?)
            .map_err(|_| FormError::BadVersion)?;
        let filetype = match get("filetype").ok_or(FormError::Missing("filetype"))? {
            "bdist_wheel" => DistKind::Wheel,
            "sdist" => DistKind::Sdist,
            _ => return Err(FormError::BadFiletype),
        };
        let requires_python = match get("requires_python") {
            None | Some("") => None,
            Some(value) if is_specifier_text(value) => Some(value.to_owned()),
            Some(_) => return Err(FormError::BadValue("requires_python")),
        };
        let metadata_version = match get("metadata_version") {
            None | Some("") => None,
            Some(value) if is_short_token(value) => Some(value.to_owned()),
            Some(_) => return Err(FormError::BadValue("metadata_version")),
        };
        Ok(Self {
            name,
            version,
            filetype,
            requires_python,
            metadata_version,
            digests: DeclaredDigests {
                sha256: digest(get("sha256_digest"), "sha256_digest", decode_hex::<32>)?,
                md5: digest(get("md5_digest"), "md5_digest", decode_md5)?,
                blake2_256: digest(
                    get("blake2_256_digest"),
                    "blake2_256_digest",
                    decode_hex::<32>,
                )?,
            },
        })
    }
}

fn digest<const N: usize>(
    text: Option<&str>,
    field: &'static str,
    decode: fn(&str) -> Option<[u8; N]>,
) -> Result<Option<[u8; N]>, FormError> {
    match text {
        None | Some("") => Ok(None),
        Some(value) => decode(value).map(Some).ok_or(FormError::BadDigest(field)),
    }
}

fn decode_hex<const N: usize>(text: &str) -> Option<[u8; N]> {
    if text.len() != N * 2 {
        return None;
    }
    let mut out = [0u8; N];
    hex::decode_to_slice(text, &mut out).ok()?;
    Some(out)
}

fn decode_md5(text: &str) -> Option<[u8; 16]> {
    if text.len() == 32 {
        return decode_hex::<16>(text);
    }
    // The documentation of the upload API says URL-safe base64 without padding (22 characters).
    let bytes = URL_SAFE_NO_PAD.decode(text).ok()?;
    bytes.try_into().ok()
}

fn is_specifier_text(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 256
        && value.bytes().all(|b| b.is_ascii_graphic() || b == b' ')
}

fn is_short_token(value: &str) -> bool {
    value.len() <= 16 && value.bytes().all(|b| b.is_ascii_digit() || b == b'.')
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::stream;
    use proptest::prelude::*;
    use std::time::Duration;

    fn block_on<F: std::future::Future>(future: F) -> F::Output {
        tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .unwrap()
            .block_on(future)
    }

    fn body(parts: &[(&str, Option<&str>, &[u8])]) -> Vec<u8> {
        let mut out = Vec::new();
        for (name, filename, data) in parts {
            out.extend_from_slice(b"--B\r\n");
            match filename {
                Some(file) => out.extend_from_slice(format!("Content-Disposition: form-data; name=\"{name}\"; filename=\"{file}\"\r\n\r\n").as_bytes()),
                None => out.extend_from_slice(format!("Content-Disposition: form-data; name=\"{name}\"\r\n\r\n").as_bytes()),
            }
            out.extend_from_slice(data);
            out.extend_from_slice(b"\r\n");
        }
        out.extend_from_slice(b"--B--\r\n");
        out
    }

    fn prefix(bytes: &[u8]) -> Result<RawFields, FormError> {
        let chunks: Vec<Result<Bytes, std::io::Error>> = bytes
            .chunks(13)
            .map(|c| Ok(Bytes::copy_from_slice(c)))
            .collect();
        let reader = MultipartReader::new(stream::iter(chunks), "B", Duration::from_secs(5));
        block_on(read_prefix(reader)).map(|p| p.fields)
    }

    fn raw(pairs: &[(&str, &str)]) -> RawFields {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect()
    }

    fn good() -> Vec<(&'static str, &'static str)> {
        vec![
            (":action", "file_upload"),
            ("protocol_version", "1"),
            ("name", "Foo_Bar"),
            ("version", "1.0"),
            ("filetype", "bdist_wheel"),
            ("pyversion", "py3"),
            ("metadata_version", "2.1"),
            ("requires_python", ">=3.8"),
        ]
    }

    #[test]
    fn collects_retained_fields_and_stops_at_the_file() {
        let mut parts: Vec<(&str, Option<&str>, &[u8])> = good()
            .into_iter()
            .map(|(k, v)| (k, None, v.as_bytes()))
            .collect();
        let description = vec![b'x'; 100_000];
        parts.push(("description", None, description.as_slice()));
        parts.push(("gpg_signature", Some("a.asc"), b"sig"));
        parts.push(("content", Some("foo_bar-1.0-py3-none-any.whl"), b"PK..."));
        parts.push(("after", None, b"never read"));
        let fields = prefix(&body(&parts)).unwrap();
        assert_eq!(fields.get("name").map(String::as_str), Some("Foo_Bar"));
        assert!(!fields.contains_key("description"));
        assert!(!fields.contains_key("gpg_signature"));
        assert!(!fields.contains_key("after"));
        let typed = UploadFields::from_raw(&fields).unwrap();
        assert_eq!(typed.name.as_str(), "foo-bar");
        assert_eq!(typed.version.to_string(), "1.0");
        assert_eq!(typed.filetype, DistKind::Wheel);
        assert_eq!(typed.requires_python.as_deref(), Some(">=3.8"));
        assert_eq!(typed.metadata_version.as_deref(), Some("2.1"));
        assert_eq!(typed.digests, DeclaredDigests::default());
    }

    #[test]
    fn refuses_repeated_oversized_and_misplaced_parts() {
        let dup = body(&[
            ("name", None, b"a"),
            ("name", None, b"b"),
            ("content", Some("f"), b"x"),
        ]);
        assert_eq!(prefix(&dup).err(), Some(FormError::Repeated("name".into())));
        let big = vec![b'a'; MAX_FIELD_BYTES + 1];
        let oversized = body(&[("name", None, &big), ("content", Some("f"), b"x")]);
        assert_eq!(prefix(&oversized).err(), Some(FormError::TooLarge));
        let no_content = body(&[("name", None, b"a")]);
        assert_eq!(prefix(&no_content).err(), Some(FormError::NoContent));
        let content_not_a_file = body(&[("content", None, b"a")]);
        assert_eq!(
            prefix(&content_not_a_file).err(),
            Some(FormError::NoContent)
        );
        let bad_utf8 = body(&[("name", None, &[0xff, 0xfe]), ("content", Some("f"), b"x")]);
        assert_eq!(
            prefix(&bad_utf8).err(),
            Some(FormError::NotUtf8("name".into()))
        );
    }

    #[test]
    fn must_not_read_the_file_before_the_fields_that_place_it() {
        // File first: the stream has no fields before it, so the upload cannot be placed.
        let file_first = body(&[("content", Some("f.whl"), b"x"), ("name", None, b"a")]);
        let prefix = prefix(&file_first).unwrap();
        assert!(UploadFields::from_raw(&prefix).is_err());
    }

    #[test]
    fn must_not_keep_unbounded_ignored_fields() {
        let chunk = vec![b'z'; 1024 * 1024];
        let mut parts: Vec<(&str, Option<&str>, &[u8])> = Vec::new();
        for _ in 0..17 {
            parts.push(("description", None, &chunk));
        }
        parts.push(("content", Some("f"), b"x"));
        // 17 MiB of ignored data in one form: the budget of 16 MiB is exhausted.
        let result = prefix(&body(&parts));
        assert_eq!(result.err(), Some(FormError::TooLarge));
    }

    #[test]
    fn validates_the_typed_fields() {
        let mut fields = raw(&good());
        assert!(UploadFields::from_raw(&fields).is_ok());
        for (key, bad, error) in [
            (":action", "submit", FormError::UnsupportedAction),
            ("protocol_version", "2", FormError::UnsupportedProtocol),
            ("name", "-bad-", FormError::BadName),
            ("version", "not-a-version", FormError::BadVersion),
            ("filetype", "bdist_egg", FormError::BadFiletype),
            (
                "requires_python",
                "\u{1}",
                FormError::BadValue("requires_python"),
            ),
            (
                "metadata_version",
                "x1",
                FormError::BadValue("metadata_version"),
            ),
        ] {
            let mut copy = fields.clone();
            copy.insert(key.to_owned(), bad.to_owned());
            assert_eq!(UploadFields::from_raw(&copy), Err(error), "{key}");
        }
        for key in [":action", "name", "version", "filetype"] {
            let mut copy = fields.clone();
            copy.remove(key);
            assert!(UploadFields::from_raw(&copy).is_err(), "{key} is required");
        }
        fields.insert("requires_python".into(), String::new());
        assert_eq!(
            UploadFields::from_raw(&fields).unwrap().requires_python,
            None
        );
    }

    #[test]
    fn decodes_the_declared_digests() {
        let hex64 = "ab".repeat(32);
        let mut fields = raw(&good());
        fields.insert("sha256_digest".into(), hex64.to_uppercase());
        fields.insert("blake2_256_digest".into(), hex64.clone());
        fields.insert("md5_digest".into(), "cd".repeat(16));
        let typed = UploadFields::from_raw(&fields).unwrap();
        assert_eq!(typed.digests.sha256, Some([0xab; 32]));
        assert_eq!(typed.digests.blake2_256, Some([0xab; 32]));
        assert_eq!(typed.digests.md5, Some([0xcd; 16]));
        // The documentation's URL-safe base64 form of an MD5.
        let b64 = URL_SAFE_NO_PAD.encode([0xcd; 16]);
        assert_eq!(b64.len(), 22);
        fields.insert("md5_digest".into(), b64);
        assert_eq!(
            UploadFields::from_raw(&fields).unwrap().digests.md5,
            Some([0xcd; 16])
        );
        for (key, bad) in [
            ("sha256_digest", "abc"),
            ("sha256_digest", &"zz".repeat(32)),
            ("md5_digest", "short"),
            ("md5_digest", &"!".repeat(22)),
            ("blake2_256_digest", &"ab".repeat(31)),
        ] {
            let mut copy = raw(&good());
            copy.insert(key.to_owned(), bad.to_owned());
            assert!(
                matches!(UploadFields::from_raw(&copy), Err(FormError::BadDigest(_))),
                "{key}"
            );
        }
    }

    proptest! {
        #[test]
        fn from_raw_never_panics(
            name in "\\PC{0,40}", version in "\\PC{0,40}", filetype in "\\PC{0,12}",
            sha in "\\PC{0,70}", md5 in "\\PC{0,40}", action in "\\PC{0,14}",
        ) {
            let fields = raw(&[(":action", &action), ("name", &name), ("version", &version), ("filetype", &filetype), ("sha256_digest", &sha), ("md5_digest", &md5)]);
            let _ = UploadFields::from_raw(&fields);
        }

        #[test]
        fn read_prefix_never_panics(bytes in proptest::collection::vec(any::<u8>(), 0..300)) {
            let _ = prefix(&bytes);
        }

        #[test]
        fn hex_digests_round_trip(bytes in proptest::array::uniform32(any::<u8>())) {
            let mut fields = raw(&good());
            fields.insert("sha256_digest".into(), hex::encode(bytes));
            prop_assert_eq!(UploadFields::from_raw(&fields).unwrap().digests.sha256, Some(bytes));
        }
    }
}
