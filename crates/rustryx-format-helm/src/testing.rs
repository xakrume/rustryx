//! Builders of test archives and documents, for the unit tests of this crate and for its
//! integration tests. Nothing here is used on a production path.
//!
//! The tar writer is written by hand (a ustar header and its padding) instead of using
//! `tar::Builder`, because the point is to produce archives a well behaved writer refuses to
//! write: a path with `..`, a link to `/etc/passwd`, a name with a newline.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::missing_panics_doc,
    reason = "test support: a panic fails the calling test with the cause"
)]

use std::io::Write;

use flate2::Compression;
use flate2::write::GzEncoder;

/// A `Chart.yaml` for a chart.
#[must_use]
pub fn chart_yaml(name: &str, version: &str) -> String {
    format!(
        "apiVersion: v2\nname: {name}\nversion: {version}\ndescription: Test chart {name}\ntype: application\nappVersion: \"1.0\"\n"
    )
}

/// A gzip compressed tar with the given `(path, contents)` entries, written as given
/// (no normalization), so that a hostile path can be produced.
#[must_use]
pub fn archive(entries: &[(&str, &[u8])]) -> Vec<u8> {
    let mut raw = Vec::new();
    for (path, data) in entries {
        raw.extend_from_slice(&tar_entry(path, data, b'0', ""));
    }
    raw.extend_from_slice(&[0u8; 1024]);
    gzip(&raw)
}

/// Gzip-compresses `raw`.
#[must_use]
pub fn gzip(raw: &[u8]) -> Vec<u8> {
    let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(raw).unwrap();
    encoder.finish().unwrap()
}

/// One ustar entry of type `kind` (`b'0'` file, `b'5'` directory, `b'2'` symlink).
#[must_use]
pub fn tar_entry(path: &str, data: &[u8], kind: u8, link: &str) -> Vec<u8> {
    let mut header = [0u8; 512];
    let name = path.as_bytes();
    assert!(name.len() <= 100, "test paths fit the ustar name field");
    header[..name.len()].copy_from_slice(name);
    header[100..107].copy_from_slice(b"0000644");
    header[108..115].copy_from_slice(b"0000000");
    header[116..123].copy_from_slice(b"0000000");
    let size = format!("{:011o}", data.len());
    header[124..135].copy_from_slice(size.as_bytes());
    header[136..147].copy_from_slice(b"00000000000");
    header[148..156].copy_from_slice(b"        ");
    header[156] = kind;
    let link = link.as_bytes();
    header[157..157 + link.len()].copy_from_slice(link);
    header[257..263].copy_from_slice(b"ustar\0");
    header[263..265].copy_from_slice(b"00");
    let sum: u32 = header.iter().map(|b| u32::from(*b)).sum();
    let checksum = format!("{sum:06o}\0 ");
    header[148..156].copy_from_slice(checksum.as_bytes());
    let mut out = header.to_vec();
    out.extend_from_slice(data);
    let padding = (512 - data.len() % 512) % 512;
    out.extend(std::iter::repeat_n(0u8, padding));
    out
}

/// A well formed chart archive: `<name>/Chart.yaml`, `values.yaml` and a template.
#[must_use]
pub fn chart(name: &str, version: &str) -> Vec<u8> {
    let yaml = chart_yaml(name, version);
    archive(&[
        (&format!("{name}/Chart.yaml"), yaml.as_bytes()),
        (&format!("{name}/values.yaml"), b"replicas: 1\n"),
        (
            &format!("{name}/templates/deployment.yaml"),
            b"kind: Deployment\n",
        ),
    ])
}
