//! Builders of package archives and multipart bodies, for the tests of this crate, for the seed
//! corpus of the fuzz targets and for the real client tests.
//!
//! The tar writer is hand rolled on purpose. The `tar` crate refuses to write a path with `..`,
//! an absolute path, a bad checksum or a header it considers inconsistent, and those are exactly
//! the archives the readers must be shown. Here every byte of a header can be chosen.
//!
//! Nothing in this module is used on a request path.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::missing_panics_doc,
    clippy::assert_is_empty,
    reason = "test support: a failure to build a fixture is a failure of the test that asked for it"
)]

use std::io::Write;

use flate2::Compression;
use flate2::write::GzEncoder;
use sha2::{Digest, Sha256};

/// Size of a tar block.
const BLOCK: usize = 512;

/// The multipart boundary the builders use.
pub const BOUNDARY: &str = "----rustryx-test-boundary";

/// A well formed `pubspec.yaml` for a package.
#[must_use]
pub fn pubspec_text(name: &str, version: &str) -> String {
    format!(
        "name: {name}\nversion: {version}\ndescription: A package for the tests of Rustryx.\n\
         environment:\n  sdk: ^3.0.0\ndependencies:\n  collection: ^1.17.0\n"
    )
}

/// Lowercase hex SHA-256.
#[must_use]
pub fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

/// Gzip with the default level.
#[must_use]
pub fn gzip(bytes: &[u8]) -> Vec<u8> {
    let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(bytes).unwrap();
    encoder.finish().unwrap()
}

/// A small, valid package archive: manifest, a library file, a readme and a changelog.
#[must_use]
pub fn package_archive(name: &str, version: &str) -> Vec<u8> {
    Tar::new()
        .file("pubspec.yaml", pubspec_text(name, version).as_bytes())
        .directory("lib/")
        .file(
            &format!("lib/{name}.dart"),
            format!("library {name};\n\nint answer() => 42;\n").as_bytes(),
        )
        .file("README.md", format!("# {name}\n").as_bytes())
        .file(
            "CHANGELOG.md",
            format!("## {version}\n\n- Initial release.\n").as_bytes(),
        )
        .finish_gz()
}

/// A `multipart/form-data` body with text fields and one file part named `file`.
#[must_use]
pub fn multipart(fields: &[(&str, &str)], file: &[u8]) -> Vec<u8> {
    let mut body = Vec::new();
    for (name, value) in fields {
        body.extend_from_slice(
            format!(
                "--{BOUNDARY}\r\nContent-Disposition: form-data; name=\"{name}\"\r\n\
                 Content-Type: text/plain; charset=utf-8\r\n\r\n{value}\r\n"
            )
            .as_bytes(),
        );
    }
    body.extend_from_slice(
        format!(
            "--{BOUNDARY}\r\nContent-Type: application/octet-stream\r\n\
             Content-Disposition: form-data; name=\"file\"; filename=\"package.tar.gz\"\r\n\r\n"
        )
        .as_bytes(),
    );
    body.extend_from_slice(file);
    body.extend_from_slice(format!("\r\n--{BOUNDARY}--\r\n").as_bytes());
    body
}

/// The `Content-Type` header value that goes with [`multipart`].
#[must_use]
pub fn multipart_content_type() -> String {
    format!("multipart/form-data; boundary={BOUNDARY}")
}

/// One 512 byte ustar header with a valid checksum. `name` is truncated to the 100 byte name
/// field, `link` to the 100 byte link field; both may hold any bytes.
#[must_use]
pub fn header(name: &[u8], kind: u8, size: u64, link: &[u8]) -> [u8; BLOCK] {
    let mut block = [0u8; BLOCK];
    put(&mut block, 0, &name[..name.len().min(100)]);
    put(&mut block, 100, b"0000644\0");
    put(&mut block, 108, b"0000000\0");
    put(&mut block, 116, b"0000000\0");
    put(&mut block, 124, format!("{size:011o}\0").as_bytes());
    put(&mut block, 136, b"00000000000\0");
    put(&mut block, 156, &[kind]);
    put(&mut block, 157, &link[..link.len().min(100)]);
    put(&mut block, 257, b"ustar\0");
    put(&mut block, 263, b"00");
    seal(&mut block);
    block
}

/// Writes the checksum field of a header.
pub fn seal(block: &mut [u8; BLOCK]) {
    put(block, 148, b"        ");
    let sum: u32 = block.iter().map(|&b| u32::from(b)).sum();
    put(block, 148, format!("{sum:06o}\0 ").as_bytes());
}

fn put(block: &mut [u8; BLOCK], at: usize, bytes: &[u8]) {
    block[at..at + bytes.len()].copy_from_slice(bytes);
}

/// A PAX record: `<length> <key>=<value>\n`, where the length counts itself.
#[must_use]
pub fn pax_record(key: &str, value: &str) -> Vec<u8> {
    let body_len = key.len() + value.len() + 3; // space, equals sign, newline
    let mut total = body_len + 1;
    // The decimal length is part of the length: iterate until it is stable.
    loop {
        let digits = total.to_string().len();
        if body_len + digits == total {
            break;
        }
        total = body_len + digits;
    }
    format!("{total} {key}={value}\n").into_bytes()
}

/// A tar archive under construction.
#[derive(Debug, Default, Clone)]
pub struct Tar {
    bytes: Vec<u8>,
}

impl Tar {
    /// An empty archive.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// A regular file.
    #[must_use]
    pub fn file(self, name: &str, data: &[u8]) -> Self {
        self.entry(name.as_bytes(), b'0', data, b"")
    }

    /// A directory entry.
    #[must_use]
    pub fn directory(self, name: &str) -> Self {
        self.entry(name.as_bytes(), b'5', b"", b"")
    }

    /// An entry of any kind, with raw name and link bytes.
    #[must_use]
    pub fn entry(mut self, name: &[u8], kind: u8, data: &[u8], link: &[u8]) -> Self {
        self.bytes
            .extend_from_slice(&header(name, kind, data.len() as u64, link));
        self.push_data(data);
        self
    }

    /// A PAX extended header that applies to the next entry.
    #[must_use]
    pub fn pax(self, records: &[(&str, &str)]) -> Self {
        let data: Vec<u8> = records
            .iter()
            .flat_map(|(key, value)| pax_record(key, value))
            .collect();
        self.entry(b"PaxHeader/x", b'x', &data, b"")
    }

    /// A GNU long name entry that applies to the next entry.
    #[must_use]
    pub fn gnu_long_name(self, name: &[u8]) -> Self {
        let mut data = name.to_vec();
        data.push(0);
        self.entry(b"././@LongLink", b'L', &data, b"")
    }

    /// A header block as given (for a header with a bad checksum or odd fields), followed by
    /// `data` padded to a block.
    #[must_use]
    pub fn raw_header(mut self, block: &[u8; BLOCK], data: &[u8]) -> Self {
        self.bytes.extend_from_slice(block);
        self.push_data(data);
        self
    }

    fn push_data(&mut self, data: &[u8]) {
        self.bytes.extend_from_slice(data);
        let padding = (BLOCK - data.len() % BLOCK) % BLOCK;
        self.bytes.resize(self.bytes.len() + padding, 0);
    }

    /// The archive with its end marker (two zero blocks), not compressed.
    #[must_use]
    pub fn finish(mut self) -> Vec<u8> {
        self.bytes.resize(self.bytes.len() + 2 * BLOCK, 0);
        self.bytes
    }

    /// The archive with its end marker, compressed.
    #[must_use]
    pub fn finish_gz(self) -> Vec<u8> {
        gzip(&self.finish())
    }

    /// The bytes so far, without an end marker.
    #[must_use]
    pub fn into_bytes(self) -> Vec<u8> {
        self.bytes
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pax_record_lengths_count_themselves() {
        for (key, value) in [("path", "a"), ("path", &"x".repeat(90)), ("size", "1")] {
            let record = pax_record(key, value);
            let text = String::from_utf8(record.clone()).unwrap();
            let declared: usize = text.split(' ').next().unwrap().parse().unwrap();
            assert_eq!(declared, record.len(), "{text}");
        }
    }

    #[test]
    fn the_hand_rolled_tar_is_readable_by_the_tar_crate() {
        let bytes = Tar::new()
            .file("a.txt", b"hello")
            .directory("d/")
            .file("d/b.txt", &[7u8; 700])
            .finish();
        let mut archive = tar::Archive::new(bytes.as_slice());
        let names: Vec<String> = archive
            .entries()
            .unwrap()
            .map(|e| e.unwrap().path().unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, ["a.txt", "d/", "d/b.txt"]);
    }

    #[test]
    fn multipart_has_fields_then_the_file() {
        let body = multipart(&[("a", "1")], b"DATA");
        let text = String::from_utf8(body).unwrap();
        assert!(text.contains("name=\"a\"\r\n"));
        assert!(text.find("name=\"a\"").unwrap() < text.find("name=\"file\"").unwrap());
        assert!(text.ends_with(&format!("DATA\r\n--{BOUNDARY}--\r\n")));
        assert!(multipart_content_type().contains(BOUNDARY));
        assert_eq!(sha256_hex(b"").len(), 64);
        assert_eq!(pubspec_text("a", "1.0.0").lines().next(), Some("name: a"));
        assert!(!package_archive("a", "1.0.0").is_empty());
    }
}
