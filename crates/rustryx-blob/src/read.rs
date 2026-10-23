//! Read side helpers shared by both stores: range resolution and chunk capping.

use bytes::Bytes;
use futures::stream::{self, BoxStream, StreamExt};
use rustryx_core::BlobError;
use rustryx_core::ports::ByteRange;

/// Largest chunk a read stream yields. Together with the pull-based stream this is
/// the whole per-download buffer (the target is 256 KiB or less per download).
pub const READ_CHUNK_BYTES: usize = 64 * 1024;

/// Turns an optional inclusive [`ByteRange`] into `(start, length)` for an object of
/// `total` bytes, with HTTP range semantics: an end past the last byte is clamped,
/// a start past the last byte (or an empty object with a range) is unsatisfiable.
pub(crate) fn resolve_range(total: u64, range: Option<ByteRange>) -> Result<(u64, u64), BlobError> {
    let Some(range) = range else {
        return Ok((0, total));
    };
    if range.start >= total {
        return Err(BlobError::RangeNotSatisfiable);
    }
    let last = match range.end {
        Some(end) if end < range.start => return Err(BlobError::RangeNotSatisfiable),
        Some(end) => end.min(total - 1),
        None => total - 1,
    };
    Ok((range.start, last - range.start + 1))
}

/// Splits chunks larger than `max` into zero-copy slices. The HTTP client of the S3
/// store may hand out chunks of a megabyte or more; the contract here is a bounded
/// chunk, so the bound is enforced instead of assumed. Polling stays pull-based:
/// the next upstream chunk is requested only after all slices of the previous one
/// were consumed.
pub(crate) fn cap_chunks(
    source: BoxStream<'static, Result<Bytes, BlobError>>,
    max: usize,
) -> BoxStream<'static, Result<Bytes, BlobError>> {
    source
        .flat_map(move |item| match item {
            Ok(chunk) if chunk.len() > max => {
                let slices: Vec<Result<Bytes, BlobError>> = chunk
                    .chunks(max)
                    .map(|slice| Ok(chunk.slice_ref(slice)))
                    .collect();
                stream::iter(slices).boxed()
            }
            other => stream::once(async move { other }).boxed(),
        })
        .boxed()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn range(start: u64, end: Option<u64>) -> ByteRange {
        ByteRange { start, end }
    }

    #[test]
    fn no_range_selects_everything() {
        assert_eq!(resolve_range(10, None).unwrap(), (0, 10));
        assert_eq!(resolve_range(0, None).unwrap(), (0, 0));
    }

    #[test]
    fn bounded_and_open_ended_ranges() {
        assert_eq!(resolve_range(10, Some(range(2, Some(4)))).unwrap(), (2, 3));
        assert_eq!(resolve_range(10, Some(range(0, Some(0)))).unwrap(), (0, 1));
        assert_eq!(resolve_range(10, Some(range(7, None))).unwrap(), (7, 3));
        assert_eq!(resolve_range(10, Some(range(9, None))).unwrap(), (9, 1));
    }

    #[test]
    fn end_past_the_last_byte_is_clamped() {
        assert_eq!(
            resolve_range(10, Some(range(5, Some(999)))).unwrap(),
            (5, 5)
        );
    }

    #[test]
    fn unsatisfiable_ranges_are_rejected() {
        for bad in [
            Some(range(10, None)),
            Some(range(11, Some(12))),
            Some(range(5, Some(4))),
        ] {
            assert!(matches!(
                resolve_range(10, bad),
                Err(BlobError::RangeNotSatisfiable)
            ));
        }
        assert!(matches!(
            resolve_range(0, Some(range(0, None))),
            Err(BlobError::RangeNotSatisfiable)
        ));
    }

    #[tokio::test]
    async fn oversized_chunks_are_split_without_copying() {
        let big = Bytes::from(vec![1_u8; 300]);
        let small = Bytes::from_static(b"tail");
        let source = stream::iter(vec![Ok(big.clone()), Ok(small)]).boxed();
        let out: Vec<_> = cap_chunks(source, 128).collect().await;
        let lens: Vec<usize> = out.iter().map(|c| c.as_ref().unwrap().len()).collect();
        assert_eq!(lens, vec![128, 128, 44, 4]);
        // Same allocation: slices point into the original buffer.
        let first = out[0].as_ref().unwrap();
        assert_eq!(first.as_ptr(), big.as_ptr());
    }

    #[tokio::test]
    async fn errors_pass_through_the_cap() {
        let source = stream::iter(vec![Err(BlobError::NotFound)]).boxed();
        let out: Vec<_> = cap_chunks(source, 8).collect().await;
        assert!(matches!(out[0], Err(BlobError::NotFound)));
    }
}
