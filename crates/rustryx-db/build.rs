//! Re-runs the build when a migration file is added or edited: `sqlx::migrate!` embeds
//! the directory at compile time, and cargo does not track files it was not told about,
//! so without this a new migration would be silently missing from the binary.

fn main() {
    println!("cargo:rerun-if-changed=migrations");
}
