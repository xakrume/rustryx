//! The seed corpus of the fuzz targets, kept in sync with the code that builds it, and replayed
//! through the same entry points on stable Rust.
//!
//! `cargo fuzz` needs a nightly toolchain, so it runs in the nightly CI job; this test gives the
//! corpus a second life on every normal test run: every file in `fuzz/corpus/*` (the seeds and
//! whatever the fuzzer found and was committed) must be processed without a panic.
//!
//! To regenerate the seeds after changing them here: `RUSTRYX_WRITE_CORPUS=1 cargo test -p
//! rustryx-format-pub --test corpus`.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test scenarios read best as straight-line code"
)]

use std::fmt::Write;
use std::fs;
use std::path::PathBuf;

use rustryx_format_pub::archive::{ArchiveLimits, inspect};
use rustryx_format_pub::document::{ArchiveUrls, PackageDoc};
use rustryx_format_pub::name::PackageName;
use rustryx_format_pub::pubspec::{PubspecLimits, parse};
use rustryx_format_pub::testing::{Tar, gzip, header, package_archive, pubspec_text};
use rustryx_format_pub::version::PubVersion;

fn corpus_dir(target: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("fuzz")
        .join("corpus")
        .join(target)
}

fn pubspec_seeds() -> Vec<(&'static str, Vec<u8>)> {
    let mut alias_bomb =
        String::from("name: lolz\nversion: 1.0.0\nl0: &l0 [x, x, x, x, x, x, x, x, x, x]\n");
    for level in 1..=9 {
        let refs = vec![format!("*l{}", level - 1); 10].join(", ");
        let _ = writeln!(alias_bomb, "l{level}: &l{level} [{refs}]");
    }
    let deep = format!(
        "name: deep\nversion: 1.0.0\nx: {}{}\n",
        "[".repeat(60),
        "]".repeat(60)
    );
    vec![
        ("plain", pubspec_text("demo", "1.0.0").into_bytes()),
        (
            "rich",
            b"name: rich\nversion: 2.0.0-beta.1+3\nhomepage: https://example.com\nflutter:\n  assets:\n    - a.png\n  uses-material-design: true\ndependencies:\n  foo:\n    version: ^1.0.0\n    hosted: https://example.com\nissue_tracker: ~\nratio: 0.5\n".to_vec(),
        ),
        ("alias", b"name: demo\nversion: 1.0.0\na: &x [1, 2]\nb: *x\n".to_vec()),
        ("alias_bomb", alias_bomb.into_bytes()),
        ("deep_nesting", deep.into_bytes()),
        ("nan", b"name: demo\nversion: 1.0.0\nx: .nan\n".to_vec()),
        ("duplicate_key", b"name: demo\nversion: 1.0.0\nname: other\n".to_vec()),
        ("non_string_key", b"name: demo\nversion: 1.0.0\n1: one\n".to_vec()),
        ("custom_tag", b"name: demo\nversion: 1.0.0\nx: !custom 1\n".to_vec()),
        ("multi_document", b"name: a\nversion: 1.0.0\n---\nname: b\n".to_vec()),
        ("bom", [b"\xef\xbb\xbf".as_slice(), pubspec_text("demo", "1.0.0").as_bytes()].concat()),
        ("unclosed", b"name: [unclosed\n".to_vec()),
        ("not_utf8", vec![0xff, 0xfe, b'n', b'a', b'm', b'e']),
        ("version_float", b"name: demo\nversion: 1.0\n".to_vec()),
    ]
}

type SeedFile = (&'static str, Vec<u8>);

fn archive_seeds() -> Vec<SeedFile> {
    let manifest = pubspec_text("demo", "1.0.0");
    let with_manifest = || Tar::new().file("pubspec.yaml", manifest.as_bytes());
    let long = format!("lib/{}.dart", "n".repeat(150));
    let good = package_archive("demo", "1.0.0");
    let mut seeds = vec![
        ("valid", good),
        (
            "pax_long_name",
            with_manifest()
                .pax(&[("path", long.as_str()), ("mtime", "1700000000.5")])
                .entry(b"short", b'0', b"data", b"")
                .finish_gz(),
        ),
        (
            "gnu_long_name",
            with_manifest()
                .gnu_long_name(long.as_bytes())
                .entry(b"short", b'0', b"data", b"")
                .finish_gz(),
        ),
        (
            "tar_slip",
            with_manifest()
                .entry(b"../../etc/cron.d/x", b'0', b"x", b"")
                .finish_gz(),
        ),
        (
            "absolute",
            with_manifest()
                .entry(b"/etc/passwd", b'0', b"x", b"")
                .finish_gz(),
        ),
        (
            "pax_slip",
            with_manifest()
                .pax(&[("path", "../../evil")])
                .entry(b"ok", b'0', b"x", b"")
                .finish_gz(),
        ),
        (
            "pax_size",
            with_manifest()
                .pax(&[("size", "5")])
                .entry(b"x", b'0', b"hello world", b"")
                .finish_gz(),
        ),
        (
            "symlink",
            with_manifest()
                .entry(b"link", b'2', b"", b"/etc")
                .finish_gz(),
        ),
        (
            "hardlink",
            with_manifest()
                .entry(b"link", b'1', b"", b"pubspec.yaml")
                .finish_gz(),
        ),
    ];
    seeds.extend(archive_damage_seeds());
    seeds
}

fn archive_damage_seeds() -> Vec<SeedFile> {
    let manifest = pubspec_text("demo", "1.0.0");
    let with_manifest = || Tar::new().file("pubspec.yaml", manifest.as_bytes());
    let good = package_archive("demo", "1.0.0");
    let mut bad_checksum = header(b"x", b'0', 0, b"");
    bad_checksum[148] ^= 1;
    let mut trailing = good.clone();
    trailing.extend_from_slice(&good);
    vec![
        (
            "duplicate_manifest",
            with_manifest()
                .file("pubspec.yaml", b"name: other\n")
                .finish_gz(),
        ),
        (
            "manifest_case",
            with_manifest().file("PUBSPEC.yaml", b"x").finish_gz(),
        ),
        (
            "no_manifest",
            Tar::new().file("lib/a.dart", b"x").finish_gz(),
        ),
        (
            "nested_manifest",
            Tar::new()
                .file("demo/pubspec.yaml", manifest.as_bytes())
                .finish_gz(),
        ),
        (
            "huge_claim",
            with_manifest()
                .raw_header(&header(b"big", b'0', 1 << 30, b""), b"")
                .finish_gz(),
        ),
        (
            "huge_extension",
            with_manifest()
                .raw_header(&header(b"x", b'x', 1 << 20, b""), b"")
                .finish_gz(),
        ),
        (
            "bad_header_checksum",
            with_manifest().raw_header(&bad_checksum, b"").finish_gz(),
        ),
        ("truncated", good[..good.len() / 2].to_vec()),
        ("trailing_member", trailing),
        ("not_gzip", b"this is not an archive".to_vec()),
        ("gzip_of_text", gzip(b"just text, not a tar archive")),
        ("raw_tar", with_manifest().finish()),
    ]
}

fn listing_seeds() -> Vec<(&'static str, Vec<u8>)> {
    let entry = |version: &str| {
        serde_json::json!({
            "version": version,
            "archive_url": format!("https://pub.example/api/archives/demo-{version}.tar.gz"),
            "archive_sha256": "a".repeat(64),
            "pubspec": { "name": "demo", "version": version },
        })
    };
    let listing = serde_json::json!({
        "name": "demo",
        "isDiscontinued": true,
        "replacedBy": "other",
        "advisoriesUpdated": "2024-01-01T00:00:00Z",
        "latest": entry("1.1.0"),
        "versions": [entry("1.0.0"), entry("1.1.0")],
    });
    let mut bad = listing.clone();
    bad["versions"][0]["version"] = "01.0.0".into();
    vec![
        ("listing", serde_json::to_vec(&listing).unwrap()),
        ("listing_with_bad_entry", serde_json::to_vec(&bad).unwrap()),
        ("empty_object", b"{}".to_vec()),
        ("array", b"[1,2,3]".to_vec()),
        ("path_route", b"api/packages/demo/versions/1.0.0".to_vec()),
        (
            "path_upload",
            b"api/packages/versions/upload/0199c9a4-7a58-7d3e-8c1a-5f6d2b9e4a10/demo/1.0.0"
                .to_vec(),
        ),
    ]
}

fn seeds() -> Vec<(&'static str, Vec<SeedFile>)> {
    vec![
        ("pubspec", pubspec_seeds()),
        ("gzip_tar", archive_seeds()),
        ("listing", listing_seeds()),
    ]
}

#[test]
fn the_committed_seed_corpus_is_what_the_code_builds() {
    let write = std::env::var_os("RUSTRYX_WRITE_CORPUS").is_some();
    for (target, files) in seeds() {
        let dir = corpus_dir(target);
        if write {
            fs::create_dir_all(&dir).unwrap();
        }
        for (name, bytes) in files {
            let path = dir.join(name);
            if write {
                fs::write(&path, &bytes).unwrap();
            }
            let committed = fs::read(&path).unwrap_or_else(|_| {
                panic!(
                    "{} is missing: run with RUSTRYX_WRITE_CORPUS=1",
                    path.display()
                )
            });
            assert_eq!(
                committed,
                bytes,
                "{} is stale: run with RUSTRYX_WRITE_CORPUS=1",
                path.display()
            );
        }
    }
}

fn each_file(target: &str, mut check: impl FnMut(&str, &[u8])) -> usize {
    let mut count = 0;
    for entry in fs::read_dir(corpus_dir(target)).unwrap() {
        let path = entry.unwrap().path();
        if path.is_file() {
            let bytes = fs::read(&path).unwrap();
            check(&path.display().to_string(), &bytes);
            count += 1;
        }
    }
    count
}

#[test]
fn every_corpus_file_is_processed_without_a_panic() {
    let limits = ArchiveLimits {
        max_uncompressed_bytes: 4 * 1024 * 1024,
        max_entry_bytes: 2 * 1024 * 1024,
        ratio_floor_bytes: 256 * 1024,
        ..ArchiveLimits::default()
    };
    let manifests = each_file("pubspec", |_, bytes| {
        let _ = parse(bytes, &PubspecLimits::default());
    });
    let archives = each_file("gzip_tar", |_, bytes| {
        let _ = inspect(bytes, bytes.len() as u64, &limits);
    });
    let name = PackageName::parse("demo").unwrap();
    let rewrite = |version: &PubVersion| format!("https://r.test/{version}");
    let listings = each_file("listing", |_, bytes| {
        let _ = PackageDoc::parse(bytes, &name, &ArchiveUrls::Keep, 1000);
        let _ = PackageDoc::parse(bytes, &name, &ArchiveUrls::Rewrite(&rewrite), 1000);
    });
    assert!(
        manifests >= 10 && archives >= 15 && listings >= 4,
        "{manifests} {archives} {listings}"
    );
}
