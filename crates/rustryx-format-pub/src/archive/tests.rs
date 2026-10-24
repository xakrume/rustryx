//! Tests of the archive reader: the good path, then every refusal, each named after the property
//! it protects.

use std::io::{self, Read};

use super::*;
use crate::testing::{Tar, gzip, header, package_archive, pubspec_text, seal};

fn limits() -> ArchiveLimits {
    ArchiveLimits::default()
}

fn inspect_bytes(bytes: &[u8]) -> Result<ArchiveReport, ArchiveError> {
    inspect(bytes, bytes.len() as u64, &limits())
}

fn inspect_with(bytes: &[u8], limits: &ArchiveLimits) -> Result<ArchiveReport, ArchiveError> {
    inspect(bytes, bytes.len() as u64, limits)
}

fn manifest() -> Vec<u8> {
    pubspec_text("demo", "1.0.0").into_bytes()
}

fn with_manifest() -> Tar {
    Tar::new().file("pubspec.yaml", &manifest())
}

#[test]
fn reads_a_normal_package() {
    let report = inspect_bytes(&package_archive("demo", "1.0.0")).unwrap();
    assert_eq!(report.pubspec, manifest());
    assert_eq!(report.entries, 5);
    assert!(report.uncompressed_bytes >= 1024);
}

#[test]
fn accepts_a_dot_slash_prefix_and_a_root_directory_entry() {
    let archive = Tar::new()
        .directory("./")
        .file("./pubspec.yaml", &manifest())
        .directory("./lib/")
        .file("./lib/demo.dart", b"x")
        .finish_gz();
    assert_eq!(inspect_bytes(&archive).unwrap().pubspec, manifest());
}

#[test]
fn accepts_long_names_through_pax_and_gnu_extensions() {
    let long = format!("lib/{}.dart", "n".repeat(150));
    let pax = with_manifest()
        .pax(&[("path", &long), ("mtime", "1700000000.5")])
        .entry(b"short", b'0', b"data", b"")
        .finish_gz();
    assert!(inspect_bytes(&pax).is_ok());
    let gnu = with_manifest()
        .gnu_long_name(long.as_bytes())
        .entry(b"short", b'0', b"data", b"")
        .finish_gz();
    assert!(inspect_bytes(&gnu).is_ok());
}

#[test]
fn a_pax_path_names_the_entry_so_it_can_carry_the_manifest() {
    let archive = Tar::new()
        .pax(&[("path", "pubspec.yaml")])
        .entry(b"placeholder", b'0', &manifest(), b"")
        .finish_gz();
    assert_eq!(inspect_bytes(&archive).unwrap().pubspec, manifest());
}

#[test]
fn must_not_accept_an_archive_without_a_root_pubspec() {
    assert_eq!(
        inspect_bytes(&Tar::new().file("lib/a.dart", b"x").finish_gz()),
        Err(ArchiveError::PubspecMissing)
    );
    // A manifest below a directory is not the manifest of the package.
    assert_eq!(
        inspect_bytes(
            &Tar::new()
                .file("demo/pubspec.yaml", &manifest())
                .finish_gz()
        ),
        Err(ArchiveError::PubspecMissing)
    );
    assert_eq!(
        inspect_bytes(&Tar::new().finish_gz()),
        Err(ArchiveError::PubspecMissing)
    );
}

#[test]
fn must_not_accept_two_manifests_or_a_manifest_look_alike() {
    assert_eq!(
        inspect_bytes(
            &with_manifest()
                .file("pubspec.yaml", b"name: other\n")
                .finish_gz()
        ),
        Err(ArchiveError::PubspecDuplicate)
    );
    for name in ["Pubspec.yaml", "PUBSPEC.YAML", "pubspec.YAML"] {
        assert_eq!(
            inspect_bytes(&with_manifest().file(name, b"x").finish_gz()),
            Err(ArchiveError::PubspecInvalid),
            "{name}"
        );
    }
    assert_eq!(
        inspect_bytes(&Tar::new().directory("pubspec.yaml/").finish_gz()),
        Err(ArchiveError::PubspecInvalid)
    );
    assert_eq!(
        inspect_bytes(&with_manifest().file("pubspec.yaml/inner", b"x").finish_gz()),
        Err(ArchiveError::PubspecInvalid)
    );
    // The manifest as a symlink is refused as a link, not read through.
    assert_eq!(
        inspect_bytes(
            &Tar::new()
                .entry(b"pubspec.yaml", b'2', b"", b"elsewhere")
                .finish_gz()
        ),
        Err(ArchiveError::UnsupportedEntry)
    );
}

#[test]
fn must_not_accept_paths_that_leave_the_extraction_directory() {
    for name in [
        "../evil",
        "a/../../evil",
        "a/..",
        "/etc/passwd",
        "a\\b",
        "..\\evil",
        "C:evil",
        "c:/evil",
        "a//b",
        "./../evil",
        "././a",
        "a/./b",
        "a/",
    ] {
        let archive = with_manifest()
            .entry(name.as_bytes(), b'0', b"x", b"")
            .finish_gz();
        assert_eq!(
            inspect_bytes(&archive),
            Err(ArchiveError::UnsafePath),
            "{name:?}"
        );
    }
}

#[test]
fn must_not_accept_unsafe_paths_hidden_in_extension_headers() {
    for (label, archive) in [
        (
            "pax",
            with_manifest()
                .pax(&[("path", "../evil")])
                .entry(b"fine", b'0', b"x", b"")
                .finish_gz(),
        ),
        (
            "gnu long name",
            with_manifest()
                .gnu_long_name(b"../../evil")
                .entry(b"fine", b'0', b"x", b"")
                .finish_gz(),
        ),
        (
            "control character",
            with_manifest()
                .gnu_long_name(b"lib/a\nb")
                .entry(b"fine", b'0', b"x", b"")
                .finish_gz(),
        ),
        (
            "nul",
            with_manifest()
                .gnu_long_name(b"lib/a\0b")
                .entry(b"fine", b'0', b"x", b"")
                .finish_gz(),
        ),
        (
            "not utf-8",
            with_manifest()
                .entry(&[b'l', 0xff, 0xfe], b'0', b"x", b"")
                .finish_gz(),
        ),
    ] {
        assert_eq!(
            inspect_bytes(&archive),
            Err(ArchiveError::UnsafePath),
            "{label}"
        );
    }
}

#[test]
fn must_not_accept_a_path_over_the_length_limits() {
    let long_path = format!("{}/x", "d".repeat(300));
    let archive = with_manifest()
        .gnu_long_name(long_path.as_bytes())
        .entry(b"x", b'0', b"x", b"")
        .finish_gz();
    assert_eq!(inspect_bytes(&archive), Err(ArchiveError::UnsafePath));
    let small = ArchiveLimits {
        max_path_bytes: 16,
        ..limits()
    };
    let archive = with_manifest()
        .file("lib/a_rather_long_name.dart", b"x")
        .finish_gz();
    assert_eq!(
        inspect_with(&archive, &small),
        Err(ArchiveError::UnsafePath)
    );
}

#[test]
fn must_not_accept_links_devices_and_other_kinds() {
    for kind in [b'1', b'2', b'3', b'4', b'6', b'7', b'S', b'g', b'K', b'Z'] {
        let archive = with_manifest()
            .entry(b"x", kind, b"", b"target")
            .finish_gz();
        assert_eq!(
            inspect_bytes(&archive),
            Err(ArchiveError::UnsupportedEntry),
            "kind {}",
            char::from(kind)
        );
    }
}

#[test]
fn must_not_honour_a_pax_size_that_would_make_readers_disagree() {
    let archive = with_manifest()
        .pax(&[("size", "5")])
        .entry(b"x", b'0', b"hello world", b"")
        .finish_gz();
    assert_eq!(inspect_bytes(&archive), Err(ArchiveError::UnsupportedEntry));
}

#[test]
fn must_not_accept_repeated_or_dangling_extension_headers() {
    let two_pax = with_manifest()
        .pax(&[("path", "a")])
        .pax(&[("path", "b")])
        .entry(b"x", b'0', b"x", b"")
        .finish_gz();
    assert_eq!(inspect_bytes(&two_pax), Err(ArchiveError::Malformed));
    let two_long = with_manifest()
        .gnu_long_name(b"a")
        .gnu_long_name(b"b")
        .entry(b"x", b'0', b"x", b"")
        .finish_gz();
    assert_eq!(inspect_bytes(&two_long), Err(ArchiveError::Malformed));
    let dangling = with_manifest().pax(&[("path", "a")]).finish_gz();
    assert!(matches!(
        inspect_bytes(&dangling),
        Err(ArchiveError::Malformed)
    ));
}

#[test]
fn must_not_read_an_oversized_extension_header_into_memory() {
    // Declared size of a GNU long name or a PAX block beyond its cap (and far below the entry
    // limit): refused without reading it.
    let archive = with_manifest()
        .raw_header(&header(b"././@LongLink", b'L', 1 << 20, b""), b"")
        .finish_gz();
    assert_eq!(inspect_bytes(&archive), Err(ArchiveError::Malformed));
    let archive = with_manifest()
        .raw_header(&header(b"PaxHeader", b'x', 1 << 20, b""), b"")
        .finish_gz();
    assert_eq!(inspect_bytes(&archive), Err(ArchiveError::Malformed));
}

#[test]
fn must_not_exceed_the_entry_count_limit() {
    let mut tar = with_manifest();
    for index in 0..10 {
        tar = tar.file(&format!("f{index}"), b"x");
    }
    let small = ArchiveLimits {
        max_entries: 5,
        ..limits()
    };
    assert_eq!(
        inspect_with(&tar.finish_gz(), &small),
        Err(ArchiveError::Limit(LimitKind::Entries))
    );
}

#[test]
fn must_not_exceed_the_entry_size_limit_even_when_the_content_is_missing() {
    // The header claims 1 GiB; nothing follows. The claim alone is refused.
    let archive = with_manifest()
        .raw_header(&header(b"big", b'0', 1 << 30, b""), b"")
        .finish_gz();
    assert_eq!(
        inspect_bytes(&archive),
        Err(ArchiveError::Limit(LimitKind::EntryBytes))
    );
}

#[test]
fn must_not_exceed_the_manifest_size_limit() {
    let big = vec![b'#'; 1024];
    let small = ArchiveLimits {
        max_pubspec_bytes: 512,
        ..limits()
    };
    let archive = Tar::new().file("pubspec.yaml", &big).finish_gz();
    assert_eq!(
        inspect_with(&archive, &small),
        Err(ArchiveError::Limit(LimitKind::PubspecBytes))
    );
}

#[test]
fn must_not_exceed_the_decompressed_size_limit() {
    let archive = with_manifest()
        .file("blob", &vec![7u8; 600_000])
        .finish_gz();
    let small = ArchiveLimits {
        max_uncompressed_bytes: 100_000,
        // Out of the way: this test is about the absolute limit.
        ratio_floor_bytes: u64::MAX,
        ..limits()
    };
    assert_eq!(
        inspect_with(&archive, &small),
        Err(ArchiveError::Limit(LimitKind::Uncompressed))
    );
}

#[test]
fn must_not_expand_a_gzip_bomb() {
    // 64 MiB of zeros compresses to about 64 KiB: a ratio of a thousand. It must be refused
    // by the ratio rule long before the absolute limit, and without holding the data.
    let bomb = with_manifest()
        .file("zeros", &vec![0u8; 64 * 1024 * 1024])
        .finish_gz();
    assert!(
        bomb.len() < 200_000,
        "the fixture is not a bomb: {}",
        bomb.len()
    );
    assert_eq!(
        inspect_bytes(&bomb),
        Err(ArchiveError::Limit(LimitKind::Ratio))
    );
}

#[test]
fn the_ratio_rule_does_not_apply_below_its_floor() {
    // Highly repetitive but small: a legitimate generated file.
    let archive = with_manifest()
        .file("generated", &vec![b'a'; 1024 * 1024])
        .finish_gz();
    assert!(inspect_bytes(&archive).is_ok());
}

#[test]
fn must_not_accept_what_is_not_a_gzip_compressed_tar() {
    assert_eq!(inspect_bytes(b""), Err(ArchiveError::Malformed));
    assert_eq!(
        inspect_bytes(b"not gzip at all"),
        Err(ArchiveError::Malformed)
    );
    // Valid gzip, not a tar.
    assert_eq!(
        inspect_bytes(&gzip(
            b"this is not a tar archive, just text"
                .repeat(100)
                .as_slice()
        )),
        Err(ArchiveError::Malformed)
    );
    // A tar that was not compressed.
    assert_eq!(
        inspect_bytes(&with_manifest().finish()),
        Err(ArchiveError::Malformed)
    );
    // Truncated in the middle of the stream.
    let archive = package_archive("demo", "1.0.0");
    for cut in [archive.len() - 1, archive.len() / 2, 12, 3] {
        assert_eq!(
            inspect_bytes(&archive[..cut]),
            Err(ArchiveError::Malformed),
            "cut at {cut}"
        );
    }
}

#[test]
fn must_not_accept_a_header_with_a_bad_checksum() {
    let mut block = header(b"x", b'0', 0, b"");
    block[148] ^= 0x01;
    let archive = with_manifest().raw_header(&block, b"").finish_gz();
    assert_eq!(inspect_bytes(&archive), Err(ArchiveError::Malformed));
    // And one that was sealed again after tampering is a different header, which is fine.
    let mut block = header(b"x", b'0', 0, b"");
    block[0] = b'y';
    seal(&mut block);
    let archive = with_manifest().raw_header(&block, b"").finish_gz();
    assert!(inspect_bytes(&archive).is_ok());
}

#[test]
fn must_not_accept_data_after_the_end_of_the_archive() {
    // Non-zero bytes after the end marker, inside the gzip stream.
    let mut raw = with_manifest().finish();
    raw.extend_from_slice(&[1u8; 600]);
    assert_eq!(inspect_bytes(&gzip(&raw)), Err(ArchiveError::TrailingData));
    // Zero padding is what tar writers add.
    let mut raw = with_manifest().finish();
    raw.resize(10_240 * 3, 0);
    assert!(inspect_bytes(&gzip(&raw)).is_ok());
    // A second gzip member, or garbage, after the first.
    let mut two = package_archive("demo", "1.0.0");
    two.extend_from_slice(&package_archive("evil", "9.9.9"));
    assert_eq!(inspect_bytes(&two), Err(ArchiveError::TrailingData));
    let mut garbage = package_archive("demo", "1.0.0");
    garbage.extend_from_slice(b"trailing");
    assert_eq!(inspect_bytes(&garbage), Err(ArchiveError::TrailingData));
}

#[test]
fn must_not_accept_a_corrupted_gzip_trailer() {
    let mut archive = package_archive("demo", "1.0.0");
    let last = archive.len() - 1;
    archive[last] ^= 0xff;
    assert_eq!(inspect_bytes(&archive), Err(ArchiveError::Malformed));
}

/// A reader that fails after some bytes, like a blob store that goes away.
struct Failing {
    data: io::Cursor<Vec<u8>>,
    left: usize,
}

impl Read for Failing {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.left == 0 {
            return Err(io::Error::other("store failed"));
        }
        let max = buf.len().min(self.left);
        let n = self.data.read(&mut buf[..max])?;
        self.left -= n;
        Ok(n)
    }
}

#[test]
fn a_failing_source_is_reported_not_panicked_on() {
    let archive = package_archive("demo", "1.0.0");
    let source = Failing {
        data: io::Cursor::new(archive.clone()),
        left: archive.len() / 2,
    };
    assert_eq!(
        inspect(source, archive.len() as u64, &limits()),
        Err(ArchiveError::Malformed)
    );
}

#[test]
fn reasons_are_a_closed_set_and_messages_are_fixed() {
    let all = [
        ArchiveError::Limit(LimitKind::Uncompressed),
        ArchiveError::Limit(LimitKind::Ratio),
        ArchiveError::Limit(LimitKind::Entries),
        ArchiveError::Limit(LimitKind::EntryBytes),
        ArchiveError::Limit(LimitKind::PubspecBytes),
        ArchiveError::Malformed,
        ArchiveError::UnsafePath,
        ArchiveError::UnsupportedEntry,
        ArchiveError::PubspecMissing,
        ArchiveError::PubspecDuplicate,
        ArchiveError::PubspecInvalid,
        ArchiveError::TrailingData,
    ];
    let mut reasons = std::collections::BTreeSet::new();
    for error in all {
        reasons.insert(error.reason());
        assert!(!error.to_string().is_empty());
        assert!(error.to_string().is_ascii());
    }
    // Four limit kinds share the label `limit`; the ratio has its own.
    assert_eq!(reasons.len(), 9);
}
