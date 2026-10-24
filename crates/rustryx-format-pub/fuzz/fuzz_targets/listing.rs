//! Fuzz target: a version listing from an upstream or from a group member, and the URL space.
//!
//! Reads the bytes as a listing with each way of producing `archive_url`, and as a request path.
#![no_main]

use libfuzzer_sys::fuzz_target;
use rustryx_format_core::NormalizedPath;
use rustryx_format_pub::document::{ArchiveUrls, PackageDoc};
use rustryx_format_pub::name::PackageName;
use rustryx_format_pub::route::Route;
use rustryx_format_pub::version::PubVersion;

fuzz_target!(|data: &[u8]| {
    if let Ok(name) = PackageName::parse("demo") {
        let rewrite = |version: &PubVersion| format!("https://r.test/{version}");
        let _ = PackageDoc::parse(data, &name, &ArchiveUrls::Keep, 1000);
        if let Ok(parsed) = PackageDoc::parse(data, &name, &ArchiveUrls::Rewrite(&rewrite), 1000) {
            // What was accepted must be re-readable.
            let again = PackageDoc::parse(&parsed.doc.to_bytes(), &name, &ArchiveUrls::Keep, 1000);
            assert!(again.is_ok());
        }
    }
    if let Ok(text) = std::str::from_utf8(data)
        && let Ok(path) = NormalizedPath::parse(text)
        && let Ok(route) = Route::parse(&path)
    {
        // A route renders back to the path it was read from.
        let again = NormalizedPath::parse(&route.to_path()).map(|p| Route::parse(&p));
        assert_eq!(again, Ok(Ok(route)));
    }
});
