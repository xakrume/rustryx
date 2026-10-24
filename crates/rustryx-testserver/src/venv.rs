//! The Python virtual environment `pip` and `twine` run in.
//!
//! One venv is shared by every test process of the machine, under the system temp directory
//! (`RUSTRYX_IT_VENV` overrides the path): creating it costs seconds and `twine` has to come
//! from `PyPI`, and with several crates testing in parallel a venv per process would download it
//! again each time and leave a copy behind (the disks of the build machines are tight). A
//! directory used as a lock (`mkdir` is atomic) makes the first process create it while the
//! others wait.
//!
//! Tests must not install into the shared venv: use `pip install --target <dir>` or a venv of
//! their own, otherwise two tests that publish different content under one name race.

use std::path::{Path, PathBuf};
use std::time::Duration;

use tokio::sync::OnceCell;

use crate::tools::{Command, run_command};

/// Marker written when the venv is complete, so a half-created one is rebuilt.
const READY_MARKER: &str = ".rustryx-ready";
/// How long to wait for another process that holds the creation lock.
const LOCK_WAIT: Duration = Duration::from_secs(600);
/// A lock older than this belongs to a process that died.
const LOCK_STALE: Duration = Duration::from_mins(15);
const CREATE_TIMEOUT: Duration = Duration::from_secs(180);
const INSTALL_TIMEOUT: Duration = Duration::from_secs(300);

static VENV: OnceCell<Result<PathBuf, String>> = OnceCell::const_new();
static TWINE: OnceCell<Result<PathBuf, String>> = OnceCell::const_new();

/// Directory of the shared venv.
fn venv_dir() -> PathBuf {
    std::env::var_os("RUSTRYX_IT_VENV").map_or_else(
        || std::env::temp_dir().join("rustryx-testserver-venv"),
        PathBuf::from,
    )
}

/// `<venv>/bin/<name>`.
fn bin(dir: &Path, name: &str) -> PathBuf {
    dir.join("bin").join(name)
}

/// The shared venv, created on first use. `Err` carries the reason a test prints as SKIP.
pub(crate) async fn shared_venv() -> Result<&'static Path, &'static str> {
    VENV.get_or_init(create_venv)
        .await
        .as_ref()
        .map(PathBuf::as_path)
        .map_err(String::as_str)
}

/// The `pip` of the shared venv.
pub(crate) async fn pip_bin() -> Result<PathBuf, String> {
    let dir = shared_venv().await.map_err(str::to_owned)?;
    Ok(bin(dir, "pip"))
}

/// The `twine` of the shared venv, installed from `PyPI` when missing. `Err` when `PyPI` cannot
/// be reached: the caller skips.
pub(crate) async fn twine_bin() -> Result<PathBuf, String> {
    TWINE
        .get_or_init(|| async {
            let dir = shared_venv().await.map_err(str::to_owned)?;
            let twine = bin(dir, "twine");
            if tokio::fs::metadata(&twine).await.is_ok() {
                return Ok(twine);
            }
            let _lock = DirLock::acquire(&lock_path(dir)).await?;
            if tokio::fs::metadata(&twine).await.is_ok() {
                return Ok(twine);
            }
            let output = run_command(
                Command::new(bin(dir, "pip"))
                    .args(["install", "--disable-pip-version-check", "--quiet", "twine"])
                    .timeout(INSTALL_TIMEOUT),
            )
            .await;
            if output.success() && tokio::fs::metadata(&twine).await.is_ok() {
                Ok(twine)
            } else {
                Err(
                    "twine is not installed and could not be installed from PyPI (offline?)"
                        .to_owned(),
                )
            }
        })
        .await
        .clone()
}

fn lock_path(dir: &Path) -> PathBuf {
    let mut name = dir.as_os_str().to_owned();
    name.push(".lock");
    PathBuf::from(name)
}

async fn is_ready(dir: &Path) -> bool {
    tokio::fs::metadata(dir.join(READY_MARKER)).await.is_ok()
        && tokio::fs::metadata(bin(dir, "pip")).await.is_ok()
}

async fn create_venv() -> Result<PathBuf, String> {
    let dir = venv_dir();
    if is_ready(&dir).await {
        return Ok(dir);
    }
    let _lock = DirLock::acquire(&lock_path(&dir)).await?;
    if is_ready(&dir).await {
        return Ok(dir);
    }
    // A venv without the marker is a leftover of a process that was killed half way.
    let _ = tokio::fs::remove_dir_all(&dir).await;
    let output = run_command(
        Command::new("python3")
            .args(["-m", "venv"])
            .arg(dir.as_os_str().to_string_lossy())
            .timeout(CREATE_TIMEOUT),
    )
    .await;
    if !output.success() {
        return Err(format!(
            "python3 -m venv failed: {}",
            output.stderr.lines().last().unwrap_or("no output")
        ));
    }
    tokio::fs::write(dir.join(READY_MARKER), b"ok")
        .await
        .map_err(|error| format!("cannot mark the venv ready: {error}"))?;
    Ok(dir)
}

/// A directory used as a cross-process lock. Released on drop.
struct DirLock {
    path: PathBuf,
}

impl DirLock {
    async fn acquire(path: &Path) -> Result<Self, String> {
        let started = std::time::Instant::now();
        loop {
            match tokio::fs::create_dir(path).await {
                Ok(()) => {
                    return Ok(Self {
                        path: path.to_owned(),
                    });
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                    if lock_is_stale(path).await {
                        let _ = tokio::fs::remove_dir(path).await;
                        continue;
                    }
                    if started.elapsed() > LOCK_WAIT {
                        return Err(
                            "timed out waiting for another process to create the venv".to_owned()
                        );
                    }
                    tokio::time::sleep(Duration::from_millis(500)).await;
                }
                Err(error) => return Err(format!("cannot take the venv lock: {error}")),
            }
        }
    }
}

async fn lock_is_stale(path: &Path) -> bool {
    let modified = tokio::fs::metadata(path)
        .await
        .and_then(|meta| meta.modified());
    modified
        .ok()
        .and_then(|at| at.elapsed().ok())
        .is_some_and(|age| age > LOCK_STALE)
}

impl Drop for DirLock {
    fn drop(&mut self) {
        // `Drop` cannot await, and removing one empty directory does not block in any way
        // that matters; a leaked lock would stall the next process for `LOCK_STALE`.
        let _ = std::fs::remove_dir(&self.path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_derive_from_the_venv_directory() {
        let dir = PathBuf::from("/tmp/venv-x");
        assert_eq!(bin(&dir, "pip"), PathBuf::from("/tmp/venv-x/bin/pip"));
        assert_eq!(lock_path(&dir), PathBuf::from("/tmp/venv-x.lock"));
    }

    #[tokio::test]
    async fn the_lock_is_exclusive_and_released_on_drop() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("venv.lock");
        let first = DirLock::acquire(&path).await.unwrap();
        assert!(path.is_dir());

        // A second process waits until the first one is done.
        let waiting = {
            let path = path.clone();
            tokio::spawn(async move { DirLock::acquire(&path).await.map(drop) })
        };
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(!waiting.is_finished(), "the lock must be held");
        drop(first);
        waiting.await.unwrap().unwrap();
        assert!(!path.exists(), "the lock directory is removed on drop");
    }

    #[tokio::test]
    async fn a_lock_left_by_a_dead_process_is_taken_over() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("venv.lock");
        tokio::fs::create_dir(&path).await.unwrap();
        let old = std::time::SystemTime::now() - Duration::from_secs(3600);
        std::fs::File::open(&path)
            .unwrap()
            .set_modified(old)
            .unwrap();
        assert!(lock_is_stale(&path).await);
        let lock = DirLock::acquire(&path).await.unwrap();
        drop(lock);
        assert!(!lock_is_stale(&path).await, "a missing lock is not stale");
    }

    #[tokio::test]
    async fn a_venv_without_the_ready_marker_is_not_ready() {
        let root = tempfile::tempdir().unwrap();
        tokio::fs::create_dir_all(root.path().join("bin"))
            .await
            .unwrap();
        tokio::fs::write(root.path().join("bin/pip"), b"")
            .await
            .unwrap();
        assert!(!is_ready(root.path()).await);
        tokio::fs::write(root.path().join(READY_MARKER), b"ok")
            .await
            .unwrap();
        assert!(is_ready(root.path()).await);
    }
}
