//! A temporary project directory for a client to run in.

use std::path::{Component, Path, PathBuf};

use tempfile::TempDir;

/// A temporary directory that a client runs in. It is deleted on drop.
///
/// Write the files the client needs (`pom.xml`, `package.json`, `settings.xml`, `.npmrc`,
/// `pubspec.yaml`) with [`TempProject::write`], then pass [`TempProject::path`] as the working
/// directory of `run`.
#[derive(Debug)]
pub struct TempProject {
    dir: TempDir,
}

impl Default for TempProject {
    fn default() -> Self {
        Self::new()
    }
}

impl TempProject {
    /// Creates an empty project directory.
    ///
    /// # Panics
    /// When the system temp directory cannot be written: no test can run then.
    #[must_use]
    #[allow(
        clippy::expect_used,
        reason = "test harness: without a temp directory nothing can run, the test must fail with the cause"
    )]
    pub fn new() -> Self {
        Self {
            dir: TempDir::with_prefix("rustryx-it-").expect("cannot create a temporary directory"),
        }
    }

    /// The directory.
    #[must_use]
    pub fn path(&self) -> &Path {
        self.dir.path()
    }

    /// `path()` joined with a relative path (not checked, not created).
    #[must_use]
    pub fn join(&self, relative: &str) -> PathBuf {
        self.dir.path().join(relative)
    }

    /// Writes `contents` to `relative` (parent directories are created) and returns the
    /// absolute path.
    ///
    /// # Panics
    /// When `relative` is absolute or climbs out of the project with `..`, or the file cannot
    /// be written.
    #[allow(
        clippy::panic,
        reason = "test harness: a path that escapes the project is a bug in the calling test"
    )]
    pub async fn write(&self, relative: &str, contents: impl AsRef<[u8]>) -> PathBuf {
        let inside = Path::new(relative)
            .components()
            .all(|part| matches!(part, Component::Normal(_) | Component::CurDir));
        assert!(
            inside,
            "TempProject::write: {relative:?} must stay inside the project"
        );
        let target = self.dir.path().join(relative);
        if let Some(parent) = target.parent()
            && let Err(error) = tokio::fs::create_dir_all(parent).await
        {
            panic!("cannot create {}: {error}", parent.display());
        }
        if let Err(error) = tokio::fs::write(&target, contents.as_ref()).await {
            panic!("cannot write {}: {error}", target.display());
        }
        target
    }

    /// Reads a file of the project.
    ///
    /// # Errors
    /// The I/O error of the read.
    pub async fn read(&self, relative: &str) -> std::io::Result<Vec<u8>> {
        tokio::fs::read(self.dir.path().join(relative)).await
    }

    /// Environment that keeps a client away from the user's own configuration and caches:
    /// `HOME`, the XDG directories, the npm, pip, pub and Helm state all point inside the
    /// project. Not for `mvn`, which needs the user's `~/.m2` to resolve its plugins offline:
    /// pass `-s settings.xml` and `-Dmaven.repo.local` to it instead.
    #[must_use]
    pub fn isolated_env(&self) -> Vec<(String, String)> {
        let root = self.dir.path();
        let at = |relative: &str| root.join(relative).to_string_lossy().into_owned();
        vec![
            ("HOME".into(), at(".home")),
            ("XDG_CACHE_HOME".into(), at(".cache")),
            ("XDG_CONFIG_HOME".into(), at(".config")),
            ("XDG_DATA_HOME".into(), at(".data")),
            ("NPM_CONFIG_CACHE".into(), at(".npm-cache")),
            ("PIP_CACHE_DIR".into(), at(".pip-cache")),
            ("PUB_CACHE".into(), at(".pub-cache")),
        ]
    }

    /// [`TempProject::isolated_env`] as the borrowed pairs `run` takes.
    #[must_use]
    pub fn env_pairs(env: &[(String, String)]) -> Vec<(&str, &str)> {
        env.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect()
    }
}
