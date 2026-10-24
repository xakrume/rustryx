//! Tests of the blocking pool and the stream to reader bridge.

use std::io::Read;
use std::time::Duration;

use futures::stream;

use super::*;

fn pool(permits: usize) -> BlockingPool {
    BlockingPool::new(permits, Duration::from_millis(100))
}

fn chunks(parts: &[&'static [u8]]) -> impl Stream<Item = Result<Bytes, std::io::Error>> + Unpin {
    stream::iter(
        parts
            .iter()
            .map(|part| Ok(Bytes::from_static(part)))
            .collect::<Vec<_>>(),
    )
}

#[tokio::test]
async fn runs_work_on_the_blocking_pool() {
    let answer = pool(1).run(|| 6 * 7).await.unwrap();
    assert_eq!(answer, 42);
}

#[tokio::test(start_paused = true)]
async fn must_not_queue_without_a_permit() {
    let pool = pool(1);
    let held = pool.acquire().await.unwrap();
    let refused = pool.acquire().await.unwrap_err();
    match refused {
        FormatError::Unavailable { retry_after, .. } => {
            assert_eq!(retry_after, Some(Duration::from_secs(1)));
        }
        other => panic!("{other:?}"),
    }
    // The work was never spawned: `run` refuses in the same way.
    assert!(matches!(
        pool.run(|| ()).await,
        Err(FormatError::Unavailable { .. })
    ));
    drop(held);
    assert!(pool.acquire().await.is_ok());
}

#[tokio::test]
async fn a_zero_permit_pool_still_has_one() {
    assert!(
        BlockingPool::new(0, Duration::from_millis(10))
            .run(|| ())
            .await
            .is_ok()
    );
}

#[tokio::test]
async fn a_panicking_task_is_an_internal_error() {
    let outcome = pool(1).run(|| -> u8 { panic!("a parser bug") }).await;
    assert!(matches!(outcome, Err(FormatError::Internal(_))));
}

#[tokio::test]
async fn the_reader_sees_the_chunks_in_order_and_then_the_end() {
    let all = pool(1)
        .run_with_reader(
            chunks(&[b"hello ", b"", b"wor", b"ld"]),
            |_| FormatError::internal("stream"),
            |mut reader| {
                let mut out = Vec::new();
                reader.read_to_end(&mut out).map(|_| out)
            },
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(all, b"hello world");
}

#[tokio::test]
async fn a_small_destination_buffer_reads_a_chunk_in_pieces() {
    let pieces = pool(1)
        .run_with_reader(
            chunks(&[b"abcdefgh"]),
            |_| FormatError::internal("stream"),
            |mut reader| {
                let mut pieces = Vec::new();
                let mut buf = [0u8; 3];
                let mut empty = [0u8; 0];
                assert_eq!(reader.read(&mut empty).unwrap(), 0);
                loop {
                    let n = reader.read(&mut buf).unwrap();
                    if n == 0 {
                        return pieces;
                    }
                    pieces.push(buf[..n].to_vec());
                }
            },
        )
        .await
        .unwrap();
    assert_eq!(pieces, [b"abc".to_vec(), b"def".to_vec(), b"gh".to_vec()]);
}

#[tokio::test]
async fn a_reader_that_stops_early_does_not_hang_the_pump() {
    // An endless source and a reader that wants three bytes: the pump must notice that nobody
    // listens any more and return.
    let endless = stream::repeat_with(|| Ok::<_, std::io::Error>(Bytes::from_static(b"xxxx")));
    let endless = Box::pin(endless);
    let result = tokio::time::timeout(
        Duration::from_secs(10),
        pool(1).run_with_reader(
            endless,
            |_| FormatError::internal("stream"),
            |mut reader| {
                let mut three = [0u8; 3];
                reader.read_exact(&mut three).map(|()| three)
            },
        ),
    )
    .await
    .expect("the pump did not stop")
    .unwrap()
    .unwrap();
    assert_eq!(&result, b"xxx");
}

#[tokio::test]
async fn must_report_a_failing_source_as_the_source_not_as_a_bad_archive() {
    let broken = stream::iter(vec![
        Ok(Bytes::from_static(b"partial")),
        Err(std::io::Error::other("store went away")),
    ]);
    let outcome = pool(1)
        .run_with_reader(
            broken,
            |_| FormatError::Unavailable {
                reason: "blob store",
                retry_after: None,
            },
            |mut reader| {
                let mut out = Vec::new();
                // What the reader makes of the truncated input is not the answer.
                reader.read_to_end(&mut out).map(|_| out)
            },
        )
        .await;
    assert!(matches!(
        outcome,
        Err(FormatError::Unavailable {
            reason: "blob store",
            ..
        })
    ));
}

#[tokio::test]
async fn a_dropped_caller_lets_the_blocking_task_finish() {
    // The blocking side waits for bytes; the caller goes away while the stream is pending.
    // Dropping the pump drops the sender, the reader sees the end and returns.
    let (done_tx, done_rx) = std::sync::mpsc::channel();
    let pending = stream::pending::<Result<Bytes, std::io::Error>>();
    let pool = pool(1);
    let call = pool.run_with_reader(
        Box::pin(pending),
        |_| FormatError::internal("stream"),
        move |mut reader| {
            let mut out = Vec::new();
            let result = reader.read_to_end(&mut out);
            done_tx.send(result.is_ok()).unwrap();
        },
    );
    let _ = tokio::time::timeout(Duration::from_millis(50), call).await;
    let finished =
        tokio::task::spawn_blocking(move || done_rx.recv_timeout(Duration::from_secs(10)))
            .await
            .unwrap();
    assert_eq!(finished, Ok(true));
}
