//! Property tests of the parsers: they never panic, they round trip, normalisation is idempotent,
//! and what authorization sees is what the route reads.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::too_many_lines,
    reason = "test scenarios read best as straight-line code"
)]

use http::Method;
use proptest::prelude::*;
use rustryx_core::model::{RepoFormat, RepoType};
use rustryx_format_core::{FormatHandler, NormalizedPath, RequestHead};
use rustryx_format_pub::PubHandler;
use rustryx_format_pub::archive::{ArchiveError, ArchiveLimits, inspect};
use rustryx_format_pub::document::{ArchiveUrls, PackageDoc};
use rustryx_format_pub::name::PackageName;
use rustryx_format_pub::pubspec::{self, PubspecLimits};
use rustryx_format_pub::route::{Declared, Route, UploadId};
use rustryx_format_pub::testing::{Tar, gzip, package_archive, pubspec_text};
use rustryx_format_pub::version::PubVersion;
use rustryx_testkit::fixtures;
use url::Url;

fn config() -> ProptestConfig {
    ProptestConfig {
        cases: 256,
        ..ProptestConfig::default()
    }
}

fn valid_name() -> impl Strategy<Value = String> {
    "[a-z][a-z0-9_]{0,63}"
}

fn valid_version() -> impl Strategy<Value = String> {
    let number = "(0|[1-9][0-9]{0,6})";
    let ident = "(0|[1-9][0-9]{0,3}|[0-9]{0,3}[a-zA-Z-][0-9a-zA-Z-]{0,6})";
    (
        number,
        number,
        number,
        proptest::option::of(proptest::collection::vec(ident, 1..4)),
        proptest::option::of(proptest::collection::vec("[0-9a-zA-Z-]{1,6}", 1..3)),
    )
        .prop_map(|(major, minor, patch, pre, build)| {
            let mut text = format!("{major}.{minor}.{patch}");
            if let Some(pre) = pre {
                text.push('-');
                text.push_str(&pre.join("."));
            }
            if let Some(build) = build {
                text.push('+');
                text.push_str(&build.join("."));
            }
            text
        })
}

fn names() -> impl Strategy<Value = PackageName> {
    valid_name().prop_map(|n| PackageName::parse(&n).unwrap())
}

fn versions() -> impl Strategy<Value = PubVersion> {
    valid_version().prop_map(|v| PubVersion::parse(&v).unwrap())
}

fn declared() -> impl Strategy<Value = Option<Declared>> {
    proptest::option::of(
        (names(), versions()).prop_map(|(name, version)| Declared { name, version }),
    )
}

fn ids() -> impl Strategy<Value = UploadId> {
    any::<u128>().prop_map(|n| UploadId::new(uuid::Uuid::from_u128(n)))
}

fn route() -> impl Strategy<Value = Route> {
    prop_oneof![
        names().prop_map(Route::List),
        (names(), versions()).prop_map(|(n, v)| Route::Version(n, v)),
        (names(), versions()).prop_map(|(n, v)| Route::Archive(n, v)),
        names().prop_map(Route::Advisories),
        Just(Route::NewUpload),
        (ids(), declared()).prop_map(|(id, declared)| Route::Upload { id, declared }),
        (ids(), declared()).prop_map(|(id, declared)| Route::Finish { id, declared }),
    ]
}

proptest! {
    #![proptest_config(config())]

    #[test]
    fn a_name_never_panics_and_round_trips(raw in ".{0,100}") {
        if let Ok(name) = PackageName::parse(&raw) {
            prop_assert_eq!(name.as_str(), raw.as_str());
            prop_assert_eq!(PackageName::parse(name.as_ref()).unwrap(), name);
        }
    }

    #[test]
    fn every_valid_name_is_accepted_unchanged(raw in valid_name()) {
        let name = PackageName::parse(&raw).unwrap();
        prop_assert_eq!(name.as_str(), raw.as_str());
    }

    #[test]
    fn a_name_with_a_forbidden_character_is_refused(
        head in "[a-z][a-z0-9_]{0,10}",
        bad in "[A-Z.\\-/\\\\ %:@é]",
        tail in "[a-z0-9_]{0,10}",
    ) {
        let raw = format!("{head}{bad}{tail}");
        prop_assert!(PackageName::parse(&raw).is_err(), "{raw}");
    }

    #[test]
    fn a_version_never_panics_and_is_canonical(raw in ".{0,160}") {
        if let Ok(version) = PubVersion::parse(&raw) {
            // Normalisation is the identity on what is accepted, so it is idempotent.
            prop_assert_eq!(version.to_string(), raw.clone());
            prop_assert_eq!(PubVersion::parse(&version.to_string()).unwrap(), version);
        }
    }

    #[test]
    fn every_generated_version_round_trips(raw in valid_version()) {
        let parsed = PubVersion::parse(&raw).unwrap();
        prop_assert_eq!(parsed.to_string(), raw.clone());
        let again = PubVersion::parse(&parsed.to_string()).unwrap();
        prop_assert_eq!(again, parsed);
    }

    #[test]
    fn version_order_is_total_and_consistent_with_equality(a in valid_version(), b in valid_version()) {
        let (a, b) = (PubVersion::parse(&a).unwrap(), PubVersion::parse(&b).unwrap());
        prop_assert_eq!(a.cmp(&b), b.cmp(&a).reverse());
        prop_assert_eq!(a == b, a.cmp(&b) == std::cmp::Ordering::Equal);
    }

    #[test]
    fn routes_round_trip_through_their_paths(route in route()) {
        let path = NormalizedPath::parse(&route.to_path()).unwrap();
        prop_assert_eq!(Route::parse(&path).unwrap(), route);
    }

    #[test]
    fn a_route_never_panics_on_any_path(raw in ".{0,200}") {
        if let Ok(path) = NormalizedPath::parse(&raw) {
            let _ = Route::parse(&path);
        }
    }

    #[test]
    fn the_authorized_path_is_the_path_the_route_reads(raw in "[a-zA-Z0-9_%./-]{0,120}") {
        let repository = fixtures::repository("pub-hosted", RepoFormat::Pub, RepoType::Hosted);
        let head = RequestHead::new(
            Method::GET,
            raw.clone(),
            rustryx_core::identity::Principal::anonymous(),
            Url::parse("https://rustryx.test").unwrap(),
        );
        if let Ok(request) = PubHandler::default().classify(&repository, &head) {
            // What classify returns is the normalised path, once, and the route is read from
            // that very value: it renders back to the same path.
            let expected = NormalizedPath::parse(&raw).unwrap();
            prop_assert_eq!(request.path.as_str(), expected.as_str());
            let route = Route::parse(&request.path).unwrap();
            prop_assert_eq!(route.to_path(), request.path.as_str().to_owned());
            // And no part of the path that was authorized can climb out of the repository.
            prop_assert!(request.path.segments().all(|s| s != ".." && s != "."));
        }
    }

    #[test]
    fn the_archive_reader_never_panics_on_arbitrary_bytes(bytes in proptest::collection::vec(any::<u8>(), 0..2048)) {
        let _ = inspect(bytes.as_slice(), bytes.len() as u64, &ArchiveLimits::default());
        // And on arbitrary bytes that are a valid gzip stream.
        let packed = gzip(&bytes);
        let _ = inspect(packed.as_slice(), packed.len() as u64, &ArchiveLimits::default());
    }

    #[test]
    fn the_archive_reader_never_panics_on_a_damaged_archive(
        flips in proptest::collection::vec((any::<proptest::sample::Index>(), any::<u8>()), 1..8),
        cut in proptest::option::of(any::<proptest::sample::Index>()),
        raw_flips in proptest::collection::vec((any::<proptest::sample::Index>(), any::<u8>()), 0..8),
    ) {
        // Damage the compressed bytes.
        let mut archive = package_archive("demo", "1.0.0");
        for (at, value) in &flips {
            let index = at.index(archive.len());
            archive[index] ^= value | 1;
        }
        if let Some(cut) = cut {
            archive.truncate(cut.index(archive.len() + 1));
        }
        let _ = inspect(archive.as_slice(), archive.len() as u64, &ArchiveLimits::default());
        // Damage the tar underneath a valid gzip, which reaches the tar reader.
        let mut raw = Tar::new()
            .file("pubspec.yaml", pubspec_text("demo", "1.0.0").as_bytes())
            .pax(&[("path", "lib/a.dart")])
            .file("placeholder", b"x")
            .finish();
        for (at, value) in &raw_flips {
            let index = at.index(raw.len());
            raw[index] ^= value | 1;
        }
        let packed = gzip(&raw);
        let _ = inspect(packed.as_slice(), packed.len() as u64, &ArchiveLimits::default());
    }

    #[test]
    fn must_not_accept_any_entry_path_that_climbs_out(
        before in proptest::collection::vec("[a-z]{1,8}", 0..3),
        after in proptest::collection::vec("[a-z]{1,8}", 0..3),
        dots in prop_oneof![Just(".."), Just("."), Just("")],
    ) {
        // A single leading `./` is the ordinary spelling of a relative path and is accepted.
        prop_assume!(!(dots == "." && before.is_empty() && !after.is_empty()));
        let mut parts = before;
        parts.push(dots.to_owned());
        parts.extend(after);
        let name = parts.join("/");
        let archive = Tar::new()
            .file("pubspec.yaml", pubspec_text("demo", "1.0.0").as_bytes())
            .entry(name.as_bytes(), b'0', b"x", b"")
            .finish_gz();
        let outcome = inspect(archive.as_slice(), archive.len() as u64, &ArchiveLimits::default());
        prop_assert_eq!(outcome, Err(ArchiveError::UnsafePath), "{}", name);
    }

    #[test]
    fn must_accept_any_ordinary_relative_entry_path(
        parts in proptest::collection::vec("[a-z][a-z0-9_.-]{0,10}", 1..5),
    ) {
        let name = parts.join("/");
        prop_assume!(!name.eq_ignore_ascii_case("pubspec.yaml") && !parts[0].eq_ignore_ascii_case("pubspec.yaml"));
        let archive = Tar::new()
            .file("pubspec.yaml", pubspec_text("demo", "1.0.0").as_bytes())
            .file(&name, b"x")
            .finish_gz();
        let outcome = inspect(archive.as_slice(), archive.len() as u64, &ArchiveLimits::default());
        prop_assert!(outcome.is_ok(), "{name}: {outcome:?}");
    }

    #[test]
    fn the_manifest_parser_never_panics_on_arbitrary_text(text in ".{0,400}") {
        let _ = pubspec::parse(text.as_bytes(), &PubspecLimits::default());
    }

    #[test]
    fn the_manifest_parser_never_panics_on_yaml_shaped_text(
        lines in proptest::collection::vec(
            prop_oneof![
                "[a-z_]{1,8}: [^\n]{0,20}",
                "  [a-z_]{1,8}: [^\n]{0,20}",
                "- [^\n]{0,20}",
                "[&*!?|>\\[\\]{},#:-][^\n]{0,12}",
            ],
            0..30,
        ),
    ) {
        let text = lines.join("\n");
        let _ = pubspec::parse(text.as_bytes(), &PubspecLimits::default());
    }

    #[test]
    fn a_valid_manifest_round_trips_its_coordinate(name in valid_name(), version in valid_version()) {
        let spec = pubspec::parse(pubspec_text(&name, &version).as_bytes(), &PubspecLimits::default());
        // The generated name may be longer than a manifest allows for other reasons, never shorter.
        let spec = spec.unwrap();
        prop_assert_eq!(spec.name.as_str(), name.as_str());
        prop_assert_eq!(spec.version.to_string(), version);
    }

    #[test]
    fn the_listing_reader_never_panics_on_arbitrary_bytes(bytes in proptest::collection::vec(any::<u8>(), 0..512)) {
        let name = PackageName::parse("demo").unwrap();
        let _ = PackageDoc::parse(&bytes, &name, &ArchiveUrls::Keep, 100);
        let url = |v: &PubVersion| format!("https://r.test/{v}");
        let _ = PackageDoc::parse(&bytes, &name, &ArchiveUrls::Rewrite(&url), 100);
    }

    #[test]
    fn the_listing_reader_never_panics_on_json_shaped_input(
        entries in proptest::collection::vec(
            (proptest::option::of(".{0,12}"), proptest::option::of(".{0,70}"), proptest::option::of(any::<bool>())),
            0..6,
        ),
        name in proptest::option::of("[a-z]{0,6}"),
    ) {
        let versions: Vec<serde_json::Value> = entries
            .into_iter()
            .map(|(version, sha, retracted)| {
                let mut entry = serde_json::json!({ "pubspec": { "name": "demo", "version": version.clone() } });
                entry["version"] = version.map_or(serde_json::Value::Null, Into::into);
                if let Some(sha) = sha { entry["archive_sha256"] = sha.into(); }
                if let Some(flag) = retracted { entry["retracted"] = flag.into(); }
                entry
            })
            .collect();
        let doc = serde_json::json!({ "name": name.unwrap_or_else(|| "demo".to_owned()), "versions": versions });
        let bytes = serde_json::to_vec(&doc).unwrap();
        let package = PackageDoc::parse(
            &bytes,
            &PackageName::parse("demo").unwrap(),
            &ArchiveUrls::Keep,
            100,
        );
        if let Ok(parsed) = package {
            // Whatever survives is sorted, unique and re-readable.
            let again = PackageDoc::parse(&parsed.doc.to_bytes(), &parsed.doc.name, &ArchiveUrls::Keep, 100).unwrap();
            prop_assert_eq!(again.doc, parsed.doc);
        }
    }
}
