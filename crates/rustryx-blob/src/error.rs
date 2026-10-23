//! Error construction helpers.
//!
//! `rustryx_core::BlobError` has no variant for "invalid key" or "disk full", so
//! those cases travel as `BlobError::Backend` with a typed source. Callers that need
//! to tell them apart can `downcast_ref::<KeyError>()` on the source (see
//! `docs/blob.md`, section "Core gaps").

use std::future::Future;
use std::time::Duration;

use rustryx_core::BlobError;
use rustryx_core::error::BoxedSource;

use crate::key::KeyError;

/// Wraps any error as `BlobError::Backend`.
pub(crate) fn backend(context: &'static str, source: impl Into<BoxedSource>) -> BlobError {
    BlobError::Backend {
        context,
        source: source.into(),
    }
}

/// A path that does not exist, including the case where a parent component is a
/// regular file (`ENOTDIR`), which a key like `a/b` meets when `a` is a file.
pub(crate) fn is_absent(kind: std::io::ErrorKind) -> bool {
    matches!(
        kind,
        std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
    )
}

/// An I/O error on a path that is expected to exist: a missing file is `NotFound`.
pub(crate) fn io_read(context: &'static str, err: std::io::Error) -> BlobError {
    if is_absent(err.kind()) {
        BlobError::NotFound
    } else {
        backend(context, err)
    }
}

/// An invalid key is a caller bug, not a missing object, so it is never `NotFound`.
pub(crate) fn invalid_key(err: KeyError) -> BlobError {
    backend("invalid blob key", err)
}

/// Runs `future` under a deadline. An expired deadline is `Unavailable`: the
/// contract says that variant covers "unreachable or timed out".
pub(crate) async fn with_deadline<T>(
    what: &'static str,
    deadline: Duration,
    future: impl Future<Output = Result<T, BlobError>>,
) -> Result<T, BlobError> {
    match tokio::time::timeout(deadline, future).await {
        Ok(result) => result,
        Err(_elapsed) => Err(BlobError::Unavailable(format!(
            "{what} timed out after {deadline:?}"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn absent_covers_missing_parent_directories() {
        assert!(is_absent(std::io::ErrorKind::NotFound));
        assert!(is_absent(std::io::ErrorKind::NotADirectory));
        assert!(!is_absent(std::io::ErrorKind::PermissionDenied));
    }

    #[test]
    fn io_read_maps_missing_file_to_not_found() {
        let missing = std::io::Error::from(std::io::ErrorKind::NotFound);
        assert!(matches!(io_read("read", missing), BlobError::NotFound));
        let not_dir = std::io::Error::from(std::io::ErrorKind::NotADirectory);
        assert!(matches!(io_read("read", not_dir), BlobError::NotFound));
        let denied = std::io::Error::from(std::io::ErrorKind::PermissionDenied);
        assert!(matches!(
            io_read("read", denied),
            BlobError::Backend {
                context: "read",
                ..
            }
        ));
    }

    #[test]
    fn invalid_key_keeps_the_typed_source() {
        match invalid_key(KeyError::Empty) {
            BlobError::Backend { context, source } => {
                assert_eq!(context, "invalid blob key");
                assert_eq!(source.downcast_ref::<KeyError>(), Some(&KeyError::Empty));
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[tokio::test]
    async fn deadline_expiry_is_unavailable() {
        let slow = async {
            tokio::time::sleep(Duration::from_secs(30)).await;
            Ok::<_, BlobError>(())
        };
        let result = with_deadline("op", Duration::from_millis(20), slow).await;
        assert!(matches!(result, Err(BlobError::Unavailable(_))));
    }

    #[tokio::test]
    async fn deadline_passes_through_the_inner_result() {
        let fast = async { Ok::<_, BlobError>(7) };
        assert_eq!(
            with_deadline("op", Duration::from_secs(5), fast)
                .await
                .unwrap(),
            7
        );
    }
}
