//! Refuses to compile the server when the UI build is missing.

use std::path::Path;
use std::process::exit;

fn main() {
    println!("cargo:rerun-if-changed=../../ui/dist");
    let index = Path::new("../../ui/dist/index.html");
    if !index.is_file() {
        eprintln!(
            "ui/dist/index.html is missing. From ui/, run `npm ci && npm run build` before cargo build."
        );
        exit(1);
    }
}
