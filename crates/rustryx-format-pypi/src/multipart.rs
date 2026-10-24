//! A streaming `multipart/form-data` reader for the legacy upload API.
//!
//! Specifications: RFC 7578 (`multipart/form-data`) and RFC 2046 section 5.1 (boundaries).
//!
//! Why this is not a general multipart crate: the distribution is the last, huge part and has to
//! reach the blob writer as a stream, never as a buffer; the parts before it are small text
//! fields. The reader therefore works as a pull parser over any chunk stream: [`MultipartReader::next_part`]
//! yields the header of the next part, and [`MultipartReader::next_chunk`] yields the body of
//! the current part chunk by chunk. At most one chunk plus the length of a delimiter is held
//! (the delimiter `CRLF--boundary` may be split across two chunks, so the last bytes of the
//! buffer are kept back until it is clear they are not the start of one).
//!
//! Strictness: the body must start with the boundary, every part needs a header block of at most
//! [`MAX_PART_HEADER_BYTES`], a boundary that is followed by anything but `--` or a line end is
//! an error, and a stream that ends inside a part is [`MultipartError::UnexpectedEof`], which is
//! what keeps a truncated upload from being published: the end of a part is only ever the
//! delimiter, never the end of the connection.

use std::fmt::Display;
use std::time::Duration;

use bytes::{Buf, Bytes, BytesMut};
use futures::{Stream, StreamExt};

/// Largest header block of one part.
pub const MAX_PART_HEADER_BYTES: usize = 16 * 1024;
/// Longest boundary RFC 2046 allows.
const MAX_BOUNDARY_LEN: usize = 70;

/// Why the body could not be read as multipart.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum MultipartError {
    /// The structure is wrong (missing boundary, bad header, junk after a boundary).
    #[error("malformed multipart body: {0}")]
    Malformed(&'static str),
    /// A part header is over its bound.
    #[error("multipart part header too large")]
    HeaderTooLarge,
    /// The stream ended before the closing delimiter.
    #[error("multipart body ended early")]
    UnexpectedEof,
    /// The client stopped sending.
    #[error("multipart body stalled")]
    Stalled,
    /// Reading the underlying stream failed.
    #[error("reading the multipart body failed: {0}")]
    Read(String),
}

/// The header of one part.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PartHead {
    /// The form field name (`name` parameter of `Content-Disposition`).
    pub name: String,
    /// The file name, when the part is a file.
    pub filename: Option<String>,
    /// The part's `Content-Type`, when it has one.
    pub content_type: Option<String>,
}

/// The `boundary` of a `multipart/form-data` content type.
///
/// # Errors
/// [`MultipartError::Malformed`] when the type is not `multipart/form-data` or the boundary is
/// missing, empty, longer than 70 characters or uses characters RFC 2046 does not allow.
pub fn boundary_from_content_type(value: &str) -> Result<String, MultipartError> {
    let mut parts = value.split(';');
    let media_type = parts.next().unwrap_or_default().trim();
    if !media_type.eq_ignore_ascii_case("multipart/form-data") {
        return Err(MultipartError::Malformed(
            "content type is not multipart/form-data",
        ));
    }
    for part in parts {
        let Some((key, raw)) = part.split_once('=') else {
            continue;
        };
        if !key.trim().eq_ignore_ascii_case("boundary") {
            continue;
        }
        let raw = raw.trim();
        let boundary = raw
            .strip_prefix('"')
            .and_then(|rest| rest.strip_suffix('"'))
            .unwrap_or(raw);
        let allowed = |b: u8| b.is_ascii_alphanumeric() || b" '()+_,-./:=?".contains(&b);
        if boundary.is_empty()
            || boundary.len() > MAX_BOUNDARY_LEN
            || !boundary.bytes().all(allowed)
            || boundary.ends_with(' ')
        {
            return Err(MultipartError::Malformed("invalid boundary"));
        }
        return Ok(boundary.to_owned());
    }
    Err(MultipartError::Malformed("boundary is missing"))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    /// Nothing read yet: the first boundary is expected.
    Start,
    /// A boundary was consumed; a part header or the closing `--` follows.
    AfterBoundary,
    /// Inside the body of a part.
    InBody,
    /// The delimiter ended the body; the next call of `next_chunk` answers `None`.
    BodyEnded,
    /// The closing delimiter was read.
    Done,
    /// A failure was reported; every later call fails again.
    Failed,
}

/// Pull parser over a chunk stream. See the module docs.
pub struct MultipartReader<S> {
    stream: S,
    idle: Duration,
    buffer: BytesMut,
    first_boundary: Vec<u8>,
    delimiter: Vec<u8>,
    state: State,
    ended: bool,
}

impl<S, E> MultipartReader<S>
where
    S: Stream<Item = Result<Bytes, E>> + Unpin,
    E: Display,
{
    /// A reader over `stream` for a body with `boundary`. `idle` bounds the wait for each chunk.
    #[must_use]
    pub fn new(stream: S, boundary: &str, idle: Duration) -> Self {
        let mut first_boundary = b"--".to_vec();
        first_boundary.extend_from_slice(boundary.as_bytes());
        let mut delimiter = b"\r\n".to_vec();
        delimiter.extend_from_slice(&first_boundary);
        Self {
            stream,
            idle,
            buffer: BytesMut::new(),
            first_boundary,
            delimiter,
            state: State::Start,
            ended: false,
        }
    }

    async fn fill(&mut self) -> Result<(), MultipartError> {
        if self.ended {
            return Err(MultipartError::UnexpectedEof);
        }
        let next = tokio::time::timeout(self.idle, self.stream.next())
            .await
            .map_err(|_| MultipartError::Stalled)?;
        match next {
            Some(Ok(chunk)) => {
                self.buffer.extend_from_slice(&chunk);
                Ok(())
            }
            Some(Err(error)) => Err(MultipartError::Read(error.to_string())),
            None => {
                self.ended = true;
                Err(MultipartError::UnexpectedEof)
            }
        }
    }

    async fn fill_to(&mut self, len: usize) -> Result<(), MultipartError> {
        while self.buffer.len() < len {
            self.fill().await?;
        }
        Ok(())
    }

    /// Moves to the next part and returns its header, or `None` after the closing delimiter.
    /// The body of the previous part must have been read to its end (`next_chunk` returned
    /// `None`) or discarded with [`MultipartReader::skip_body`].
    pub async fn next_part(&mut self) -> Result<Option<PartHead>, MultipartError> {
        let result = self.next_part_inner().await;
        if result.is_err() {
            self.state = State::Failed;
        }
        result
    }

    async fn next_part_inner(&mut self) -> Result<Option<PartHead>, MultipartError> {
        match self.state {
            State::Start => {
                let len = self.first_boundary.len();
                self.fill_to(len).await?;
                if !self.buffer.starts_with(&self.first_boundary) {
                    return Err(MultipartError::Malformed(
                        "body does not start with the boundary",
                    ));
                }
                self.buffer.advance(len);
                self.state = State::AfterBoundary;
            }
            State::AfterBoundary => {}
            State::Done => return Ok(None),
            State::InBody | State::BodyEnded => {
                return Err(MultipartError::Malformed(
                    "previous part was not read to its end",
                ));
            }
            State::Failed => return Err(MultipartError::Malformed("reader failed earlier")),
        }
        if self.after_boundary().await? {
            self.state = State::Done;
            return Ok(None);
        }
        let head = self.read_part_head().await?;
        self.state = State::InBody;
        Ok(Some(head))
    }

    /// Reads what follows a boundary: `--` closes the body (true), optional padding and a line
    /// end open a part (false).
    async fn after_boundary(&mut self) -> Result<bool, MultipartError> {
        self.fill_to(2).await?;
        if self.buffer.starts_with(b"--") {
            self.buffer.advance(2);
            return Ok(true);
        }
        // Transport padding (spaces, tabs) is allowed before the line end.
        loop {
            let padding = self
                .buffer
                .iter()
                .take_while(|b| matches!(**b, b' ' | b'\t'))
                .count();
            self.fill_to(padding + 2).await?;
            if self.buffer.get(padding..padding + 2) == Some(b"\r\n") {
                self.buffer.advance(padding + 2);
                return Ok(false);
            }
            let grown = self
                .buffer
                .iter()
                .take_while(|b| matches!(**b, b' ' | b'\t'))
                .count();
            if grown == padding {
                return Err(MultipartError::Malformed("junk after the boundary"));
            }
        }
    }

    async fn read_part_head(&mut self) -> Result<PartHead, MultipartError> {
        loop {
            if let Some(end) = find(&self.buffer, b"\r\n\r\n") {
                if end > MAX_PART_HEADER_BYTES {
                    return Err(MultipartError::HeaderTooLarge);
                }
                let block = self.buffer.split_to(end).freeze();
                self.buffer.advance(4);
                return parse_part_head(&block);
            }
            if self.buffer.len() > MAX_PART_HEADER_BYTES {
                return Err(MultipartError::HeaderTooLarge);
            }
            self.fill().await?;
        }
    }

    /// The next piece of the current part's body, `None` once its delimiter was reached.
    pub async fn next_chunk(&mut self) -> Result<Option<Bytes>, MultipartError> {
        let result = self.next_chunk_inner().await;
        if result.is_err() {
            self.state = State::Failed;
        }
        result
    }

    async fn next_chunk_inner(&mut self) -> Result<Option<Bytes>, MultipartError> {
        match self.state {
            State::InBody => {}
            State::BodyEnded => {
                self.state = State::AfterBoundary;
                return Ok(None);
            }
            State::Failed => return Err(MultipartError::Malformed("reader failed earlier")),
            State::Start | State::AfterBoundary | State::Done => {
                return Err(MultipartError::Malformed("no part is open"));
            }
        }
        loop {
            if let Some(at) = find(&self.buffer, &self.delimiter) {
                let chunk = self.buffer.split_to(at).freeze();
                self.buffer.advance(self.delimiter.len());
                if chunk.is_empty() {
                    self.state = State::AfterBoundary;
                    return Ok(None);
                }
                self.state = State::BodyEnded;
                return Ok(Some(chunk));
            }
            // Without a delimiter in the buffer, everything but a possible partial delimiter at
            // its end belongs to the body.
            let safe = self
                .buffer
                .len()
                .saturating_sub(self.delimiter.len().saturating_sub(1));
            if safe > 0 {
                return Ok(Some(self.buffer.split_to(safe).freeze()));
            }
            self.fill().await?;
        }
    }

    /// Reads the current part's body to its end and drops it. Returns the number of bytes
    /// dropped, so that a caller can bound what a part may cost.
    pub async fn skip_body(&mut self, limit: u64) -> Result<u64, MultipartError> {
        let mut total = 0u64;
        while let Some(chunk) = self.next_chunk().await? {
            total = total.saturating_add(chunk.len() as u64);
            if total > limit {
                self.state = State::Failed;
                return Err(MultipartError::Malformed("part is too large"));
            }
        }
        Ok(total)
    }
}

/// Index of the first occurrence of `needle` in `hay`.
fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    let first = *needle.first()?;
    let mut from = 0usize;
    loop {
        let rest = hay.get(from..)?;
        let at = from + rest.iter().position(|b| *b == first)?;
        if hay.get(at..at.checked_add(needle.len())?) == Some(needle) {
            return Some(at);
        }
        from = at + 1;
    }
}

fn parse_part_head(block: &[u8]) -> Result<PartHead, MultipartError> {
    let text = std::str::from_utf8(block)
        .map_err(|_| MultipartError::Malformed("part header is not UTF-8"))?;
    let mut disposition: Option<&str> = None;
    let mut content_type: Option<String> = None;
    for line in text.split("\r\n") {
        let (name, value) = line.split_once(':').ok_or(MultipartError::Malformed(
            "part header line without a colon",
        ))?;
        if name.eq_ignore_ascii_case("content-disposition") {
            if disposition.replace(value.trim()).is_some() {
                return Err(MultipartError::Malformed("repeated Content-Disposition"));
            }
        } else if name.eq_ignore_ascii_case("content-type") {
            content_type = Some(value.trim().to_owned());
        }
    }
    let disposition = disposition.ok_or(MultipartError::Malformed(
        "part without Content-Disposition",
    ))?;
    let params = parse_disposition(disposition)?;
    let name = params
        .iter()
        .find(|(key, _)| key == "name")
        .map(|(_, value)| value.clone())
        .ok_or(MultipartError::Malformed("part without a name"))?;
    let filename = params
        .iter()
        .find(|(key, _)| key == "filename")
        .map(|(_, value)| value.clone());
    Ok(PartHead {
        name,
        filename,
        content_type,
    })
}

/// Parameters of `form-data; name="x"; filename="y"`, keys lowercased, quoted strings unescaped.
fn parse_disposition(value: &str) -> Result<Vec<(String, String)>, MultipartError> {
    let mut chars = value.chars().peekable();
    let mut kind = String::new();
    while let Some(c) = chars.next_if(|c| *c != ';') {
        kind.push(c);
    }
    if !kind.trim().eq_ignore_ascii_case("form-data") {
        return Err(MultipartError::Malformed("part is not form-data"));
    }
    let mut params = Vec::new();
    while chars.next_if_eq(&';').is_some() {
        while chars.next_if(|c| *c == ' ' || *c == '\t').is_some() {}
        let mut key = String::new();
        while let Some(c) = chars.next_if(|c| *c != '=' && *c != ';') {
            key.push(c);
        }
        if chars.next_if_eq(&'=').is_none() {
            // A parameter without a value carries no information this reader uses.
            continue;
        }
        let mut text = String::new();
        if chars.next_if_eq(&'"').is_some() {
            loop {
                match chars.next() {
                    Some('"') => break,
                    Some('\\') => text.push(
                        chars
                            .next()
                            .ok_or(MultipartError::Malformed("unterminated quoted string"))?,
                    ),
                    Some(c) => text.push(c),
                    None => return Err(MultipartError::Malformed("unterminated quoted string")),
                }
            }
        } else {
            while let Some(c) = chars.next_if(|c| *c != ';') {
                text.push(c);
            }
            let trimmed = text.trim().to_owned();
            text = trimmed;
        }
        params.push((key.trim().to_ascii_lowercase(), text));
    }
    Ok(params)
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::stream;
    use proptest::prelude::*;

    /// The reader's idle timeout needs a timer, so the futures run on a small tokio runtime.
    fn block_on<F: std::future::Future>(future: F) -> F::Output {
        tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .unwrap()
            .block_on(future)
    }

    type Chunks = stream::Iter<std::vec::IntoIter<Result<Bytes, std::io::Error>>>;

    fn reader(body: &[u8], split: usize, boundary: &str) -> MultipartReader<Chunks> {
        let chunks: Vec<_> = body
            .chunks(split.max(1))
            .map(|c| Ok(Bytes::copy_from_slice(c)))
            .collect();
        MultipartReader::new(stream::iter(chunks), boundary, Duration::from_secs(5))
    }

    fn form(parts: &[(&str, Option<&str>, &[u8])]) -> Vec<u8> {
        let mut out = Vec::new();
        for (name, filename, data) in parts {
            out.extend_from_slice(b"--XyZ\r\n");
            match filename {
                Some(file) => out.extend_from_slice(
                    format!("Content-Disposition: form-data; name=\"{name}\"; filename=\"{file}\"\r\nContent-Type: application/octet-stream\r\n\r\n").as_bytes(),
                ),
                None => out.extend_from_slice(
                    format!("Content-Disposition: form-data; name=\"{name}\"\r\n\r\n").as_bytes(),
                ),
            }
            out.extend_from_slice(data);
            out.extend_from_slice(b"\r\n");
        }
        out.extend_from_slice(b"--XyZ--\r\n");
        out
    }

    async fn collect(
        r: &mut MultipartReader<Chunks>,
    ) -> Result<Vec<(PartHead, Vec<u8>)>, MultipartError> {
        let mut parts = Vec::new();
        while let Some(head) = r.next_part().await? {
            let mut body = Vec::new();
            while let Some(chunk) = r.next_chunk().await? {
                body.extend_from_slice(&chunk);
            }
            parts.push((head, body));
        }
        Ok(parts)
    }

    #[test]
    fn boundary_is_read_from_the_content_type() {
        assert_eq!(
            boundary_from_content_type("multipart/form-data; boundary=abc").unwrap(),
            "abc"
        );
        assert_eq!(
            boundary_from_content_type("Multipart/Form-Data;charset=x; Boundary=\"a b'c\"")
                .unwrap(),
            "a b'c"
        );
        for bad in [
            "text/plain; boundary=abc",
            "multipart/form-data",
            "multipart/form-data; boundary=",
            "multipart/form-data; boundary=\"\"",
            "multipart/form-data; boundary=a\r\nb",
            "multipart/form-data; boundary=a%b",
            "multipart/form-data; boundary=\"a \"",
        ] {
            assert!(boundary_from_content_type(bad).is_err(), "{bad:?}");
        }
        let long = format!("multipart/form-data; boundary={}", "a".repeat(71));
        assert!(boundary_from_content_type(&long).is_err());
    }

    #[test]
    fn reads_fields_and_a_file_in_order() {
        let body = form(&[
            ("name", None, b"foo"),
            (
                "content",
                Some("foo-1.0.tar.gz"),
                b"\x00\r\n--Xy\r\nbinary\xff",
            ),
            ("late", None, b"x"),
        ]);
        let parts = block_on(collect(&mut reader(&body, 7, "XyZ"))).unwrap();
        assert_eq!(parts.len(), 3);
        assert_eq!(parts[0].0.name, "name");
        assert_eq!(parts[0].0.filename, None);
        assert_eq!(parts[0].1, b"foo");
        assert_eq!(parts[1].0.filename.as_deref(), Some("foo-1.0.tar.gz"));
        assert_eq!(
            parts[1].0.content_type.as_deref(),
            Some("application/octet-stream")
        );
        assert_eq!(parts[1].1, b"\x00\r\n--Xy\r\nbinary\xff");
        assert_eq!(parts[2].1, b"x");
    }

    #[test]
    fn an_empty_part_and_a_form_without_parts() {
        let body = form(&[("a", None, b""), ("b", None, b"1")]);
        let parts = block_on(collect(&mut reader(&body, 3, "XyZ"))).unwrap();
        assert_eq!(parts[0].1, b"");
        assert_eq!(parts[1].1, b"1");
        let parts = block_on(collect(&mut reader(b"--XyZ--\r\n", 4, "XyZ"))).unwrap();
        assert_eq!(parts.len(), 0);
    }

    #[test]
    fn quoted_names_and_padding_are_understood() {
        let body = b"--XyZ \t\r\nContent-Disposition: form-data; name=\"a\\\"b\"; filename=\"c;d.txt\"\r\n\r\nv\r\n--XyZ--";
        let parts = block_on(collect(&mut reader(body, 5, "XyZ"))).unwrap();
        assert_eq!(parts[0].0.name, "a\"b");
        assert_eq!(parts[0].0.filename.as_deref(), Some("c;d.txt"));
    }

    #[test]
    fn refuses_malformed_bodies() {
        let cases: [(&[u8], &str); 9] = [
            (b"nonsense", "start"),
            (b"--XyZx\r\n", "junk after boundary"),
            (b"--XyZ\r\nContent-Disposition: form-data\r\n\r\nv\r\n--XyZ--", "no name"),
            (b"--XyZ\r\nContent-Type: a/b\r\n\r\nv\r\n--XyZ--", "no disposition"),
            (b"--XyZ\r\nContent-Disposition: attachment; name=\"a\"\r\n\r\nv\r\n--XyZ--", "not form-data"),
            (b"--XyZ\r\nbad line\r\n\r\nv\r\n--XyZ--", "header without colon"),
            (b"--XyZ\r\nContent-Disposition: form-data; name=\"a\r\n\r\nv", "unterminated quote"),
            (b"--XyZ\r\nContent-Disposition: form-data; name=\"a\"\r\nContent-Disposition: form-data; name=\"b\"\r\n\r\nv\r\n--XyZ--", "repeated disposition"),
            (b"--XyZ\r\nContent-Disposition: form-data; name=\"a\"\r\n\r\nv\r\n--XyZjunk", "junk after the delimiter"),
        ];
        for (body, why) in cases {
            assert!(
                block_on(collect(&mut reader(body, 6, "XyZ"))).is_err(),
                "{why}"
            );
        }
    }

    #[test]
    fn must_not_treat_a_truncated_body_as_complete() {
        let body = form(&[("content", Some("a.whl"), &[7u8; 5000])]);
        // Cut anywhere inside the file part or its headers: never `Ok`.
        for cut in (1..body.len() - 12).step_by(97) {
            let result = block_on(collect(&mut reader(&body[..cut], 11, "XyZ")));
            assert_eq!(
                result.err(),
                Some(MultipartError::UnexpectedEof),
                "cut at {cut}"
            );
        }
    }

    #[test]
    fn must_not_accept_an_oversized_part_header() {
        let mut body = b"--XyZ\r\nContent-Disposition: form-data; name=\"a\"\r\nX-Pad: ".to_vec();
        body.extend(std::iter::repeat_n(b'a', MAX_PART_HEADER_BYTES + 10));
        body.extend_from_slice(b"\r\n\r\nv\r\n--XyZ--");
        let result = block_on(collect(&mut reader(&body, 1000, "XyZ")));
        assert_eq!(result.err(), Some(MultipartError::HeaderTooLarge));
    }

    #[test]
    fn a_read_error_and_a_stall_are_reported() {
        let chunks: Vec<Result<Bytes, std::io::Error>> = vec![
            Ok(Bytes::from_static(
                b"--XyZ\r\nContent-Disposition: form-data; name=\"a\"\r\n\r\nab",
            )),
            Err(std::io::Error::other("boom")),
        ];
        let mut r = MultipartReader::new(stream::iter(chunks), "XyZ", Duration::from_secs(1));
        let result = block_on(collect_raw(&mut r));
        assert!(matches!(result, Err(MultipartError::Read(text)) if text.contains("boom")));
        // After a failure the reader stays failed.
        assert!(block_on(r.next_chunk()).is_err());
        assert!(block_on(r.next_part()).is_err());
    }

    async fn collect_raw<S: Stream<Item = Result<Bytes, std::io::Error>> + Unpin>(
        r: &mut MultipartReader<S>,
    ) -> Result<Vec<u8>, MultipartError> {
        let mut body = Vec::new();
        while r.next_part().await?.is_some() {
            while let Some(chunk) = r.next_chunk().await? {
                body.extend_from_slice(&chunk);
            }
        }
        Ok(body)
    }

    #[tokio::test(start_paused = true)]
    async fn must_not_wait_forever_for_a_stalled_client() {
        let pending = stream::pending::<Result<Bytes, std::io::Error>>();
        let mut r = MultipartReader::new(pending, "XyZ", Duration::from_secs(30));
        assert_eq!(r.next_part().await.err(), Some(MultipartError::Stalled));
    }

    #[test]
    fn skip_body_counts_and_bounds() {
        let body = form(&[("a", None, &[1u8; 300]), ("b", None, b"z")]);
        let mut r = reader(&body, 50, "XyZ");
        block_on(async {
            r.next_part().await.unwrap().unwrap();
            assert_eq!(r.skip_body(1000).await.unwrap(), 300);
            r.next_part().await.unwrap().unwrap();
            assert_eq!(
                r.skip_body(0).await.err(),
                Some(MultipartError::Malformed("part is too large"))
            );
        });
    }

    #[test]
    fn calling_in_the_wrong_order_is_an_error() {
        let body = form(&[("a", None, b"1")]);
        let mut r = reader(&body, 50, "XyZ");
        block_on(async {
            assert!(r.next_chunk().await.is_err());
        });
        let mut r = reader(&body, 50, "XyZ");
        block_on(async {
            r.next_part().await.unwrap().unwrap();
            assert!(r.next_part().await.is_err());
        });
    }

    proptest! {
        #[test]
        fn chunking_does_not_change_the_result(
            data in proptest::collection::vec(any::<u8>(), 0..400),
            split in 1usize..64,
        ) {
            // The content may contain anything but the delimiter itself.
            prop_assume!(find(&data, b"\r\n--XyZ").is_none());
            let body = form(&[("name", None, b"foo"), ("content", Some("f.whl"), &data)]);
            let whole = block_on(collect(&mut reader(&body, body.len(), "XyZ"))).unwrap();
            let pieces = block_on(collect(&mut reader(&body, split, "XyZ"))).unwrap();
            prop_assert_eq!(&whole, &pieces);
            prop_assert_eq!(&whole[1].1, &data);
        }

        #[test]
        fn never_panics_on_arbitrary_bytes(bytes in proptest::collection::vec(any::<u8>(), 0..300), split in 1usize..40) {
            let _ = block_on(collect(&mut reader(&bytes, split, "XyZ")));
        }

        #[test]
        fn never_panics_on_form_like_bytes(text in "(--XyZ(--)?\r?\n?(Content-Disposition: form-data; name=\"[a-z]{0,4}\"(; filename=\"[a-z.]{0,5}\")?\r?\n?){0,2}\r?\n?[ -~]{0,10}){0,4}", split in 1usize..20) {
            let _ = block_on(collect(&mut reader(text.as_bytes(), split, "XyZ")));
        }

        #[test]
        fn boundary_parser_never_panics(text in "\\PC{0,120}") {
            let _ = boundary_from_content_type(&text);
        }
    }
}
