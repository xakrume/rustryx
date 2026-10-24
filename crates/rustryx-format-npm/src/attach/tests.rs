use std::io;

use axum_core::body::Body;
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use bytes::Bytes;
use futures::{StreamExt, stream};
use sha2::{Digest, Sha512};

use super::*;
use crate::scan::ScanLimits;

fn publish_document(payload: &[u8]) -> String {
    format!(
        concat!(
            r#"{{"name":"p","versions":{{"1.0.0":{{"name":"p","version":"1.0.0"}}}},"#,
            r#""_attachments":{{"p-1.0.0.tgz":{{"content_type":"application/octet-stream","#,
            r#""data":"{}","length":{}}}}}}}"#
        ),
        STANDARD.encode(payload),
        payload.len()
    )
}

/// Reads the head the way the handler does and returns the stream that continues it.
fn stream_of(
    document: &str,
    step: usize,
    expected: Option<[u8; 64]>,
) -> (AttachmentStream, OutcomeSlot) {
    let mut scanner = Scanner::new(ScanLimits::default());
    let pieces: Vec<Bytes> = document
        .as_bytes()
        .chunks(step)
        .map(Bytes::copy_from_slice)
        .collect();
    let mut head_pieces = 0;
    for piece in &pieces {
        scanner.feed(piece).unwrap();
        head_pieces += 1;
        if scanner.data_started() {
            break;
        }
    }
    let rest: Vec<io::Result<Bytes>> = pieces[head_pieces..].iter().cloned().map(Ok).collect();
    let source = Body::from_stream(stream::iter(rest)).into_data_stream();
    let slot = OutcomeSlot::new();
    (
        AttachmentStream::new(source, scanner, expected, slot.clone()),
        slot,
    )
}

async fn drain(mut stream: AttachmentStream) -> (Vec<u8>, Option<io::Error>) {
    let mut out = Vec::new();
    while let Some(item) = stream.next().await {
        match item {
            Ok(chunk) => out.extend_from_slice(&chunk),
            Err(error) => return (out, Some(error)),
        }
    }
    (out, None)
}

/// Replaces one character of the payload, `offset` characters after its opening quote, with one
/// that is not base64. The offset is far enough in that the head phase (which stops at the first
/// piece that contains the start of the payload) has already ended.
fn corrupt_payload(document: &str, offset: usize) -> String {
    let start = document.find("\"data\":\"").unwrap() + "\"data\":\"".len();
    let mut bytes = document.as_bytes().to_vec();
    bytes[start + offset] = b'*';
    String::from_utf8(bytes).unwrap()
}

fn sha512(bytes: &[u8]) -> [u8; 64] {
    Sha512::digest(bytes).into()
}

#[tokio::test]
async fn yields_the_decoded_payload_and_reports_digest_and_metadata() {
    let payload: Vec<u8> = (0..50_000u32)
        .map(|n| u8::try_from(n * 7 % 256).unwrap())
        .collect();
    let document = publish_document(&payload);
    for step in [1usize, 5, 777, 100_000] {
        let (stream, slot) = stream_of(&document, step, Some(sha512(&payload)));
        let (decoded, error) = drain(stream).await;
        assert!(error.is_none(), "step {step}");
        assert_eq!(decoded, payload, "step {step}");
        let completed = slot.take_completed().unwrap();
        assert_eq!(completed.sha512, sha512(&payload));
        assert_eq!(completed.size, payload.len() as u64);
        let meta: serde_json::Value = serde_json::from_slice(&completed.meta).unwrap();
        assert_eq!(meta["_attachments"]["p-1.0.0.tgz"]["data"], "");
        assert!(slot.take_failure().is_none());
    }
}

#[tokio::test]
async fn must_not_end_normally_when_the_sha512_does_not_match() {
    let payload = b"the real tarball".to_vec();
    let document = publish_document(&payload);
    let (stream, slot) = stream_of(&document, 64, Some(sha512(b"another tarball")));
    let (decoded, error) = drain(stream).await;
    assert!(
        error.is_some(),
        "the pipeline must see an error, not an end"
    );
    assert_eq!(
        decoded, payload,
        "the bytes were offered; the pipeline discards them on the error"
    );
    assert_eq!(slot.take_failure(), Some(StreamFailure::Integrity));
    assert!(slot.take_completed().is_none());
}

#[tokio::test]
async fn without_a_declared_digest_any_payload_completes() {
    let (stream, slot) = stream_of(&publish_document(b"abc"), 9, None);
    let (decoded, error) = drain(stream).await;
    assert!(error.is_none());
    assert_eq!(decoded, b"abc");
    assert!(slot.take_completed().is_some());
}

#[tokio::test]
async fn must_not_end_normally_when_the_document_is_cut_off() {
    let document = publish_document(&[1u8; 3000]);
    for cut in [document.len() - 1, document.len() - 20, document.len() / 2] {
        let (stream, slot) = stream_of(&document[..cut], 128, None);
        let (_, error) = drain(stream).await;
        assert!(error.is_some(), "cut {cut}");
        assert!(matches!(
            slot.take_failure(),
            Some(StreamFailure::Scan(
                ScanError::Truncated | ScanError::BadBase64
            ))
        ));
        assert!(slot.take_completed().is_none());
    }
}

#[tokio::test]
async fn must_not_end_normally_with_garbage_after_the_document() {
    let mut document = publish_document(b"abc");
    document.push_str(" trailing");
    let (stream, slot) = stream_of(&document, 50, None);
    let (_, error) = drain(stream).await;
    assert!(error.is_some());
    assert!(matches!(
        slot.take_failure(),
        Some(StreamFailure::Scan(ScanError::Malformed(_)))
    ));
}

#[tokio::test]
async fn must_not_end_normally_when_the_payload_is_not_base64() {
    let document = corrupt_payload(&publish_document(&[3u8; 600]), 400);
    let (stream, slot) = stream_of(&document, 40, None);
    let (_, error) = drain(stream).await;
    assert!(error.is_some());
    assert_eq!(
        slot.take_failure(),
        Some(StreamFailure::Scan(ScanError::BadBase64))
    );
}

#[tokio::test]
async fn a_body_that_breaks_is_a_read_failure() {
    let document = publish_document(&[9u8; 500]);
    let mut scanner = Scanner::new(ScanLimits::default());
    let split = document.find("\"data\":\"").unwrap() + 20;
    scanner.feed(&document.as_bytes()[..split]).unwrap();
    assert!(scanner.data_started());
    let source = Body::from_stream(stream::iter(vec![
        Ok(Bytes::copy_from_slice(
            &document.as_bytes()[split..split + 100],
        )),
        Err(io::Error::other("connection reset")),
    ]))
    .into_data_stream();
    let slot = OutcomeSlot::new();
    let (decoded, error) = drain(AttachmentStream::new(source, scanner, None, slot.clone())).await;
    assert!(error.is_some());
    assert!(
        !decoded.is_empty(),
        "what arrived before the break was passed on"
    );
    assert_eq!(slot.take_failure(), Some(StreamFailure::Read));
}

#[tokio::test]
async fn the_stream_is_finished_after_an_error() {
    let (mut stream, _) = stream_of(
        &corrupt_payload(&publish_document(&[3u8; 600]), 400),
        30,
        None,
    );
    let mut errors = 0;
    while let Some(item) = stream.next().await {
        errors += usize::from(item.is_err());
    }
    assert_eq!(errors, 1);
    assert!(stream.next().await.is_none());
}
