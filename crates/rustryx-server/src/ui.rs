//! Web UI embedded from `ui/dist`.
//!
//! `npm run build` in `ui/` must run before this crate is compiled. The files
//! are part of the binary, so the version reported by the API is the version
//! of the interface as well.

use include_dir::{Dir, include_dir};

use rustryx_api::WebUi;

static DIST: Dir<'static> = include_dir!("$CARGO_MANIFEST_DIR/../../ui/dist");

/// The production UI build compiled into this binary.
pub struct EmbeddedUi;

impl WebUi for EmbeddedUi {
    fn asset(&self, path: &str) -> Option<&'static [u8]> {
        DIST.get_file(path).map(include_dir::File::contents)
    }
}
