//! Incremental scanner of the `PUT /<package>` body.
//!
//! `npm publish` sends one JSON document whose `_attachments.<file>.data` is the whole tarball as
//! base64. A server that parses that document into memory holds the tarball three times over (the
//! text, the decoded copy, the parsed value) and a 256 MiB package becomes a gigabyte of resident
//! memory. This scanner walks the bytes as they arrive and splits them in two:
//!
//! * the **metadata**: everything that is not the attachment payload, kept in a buffer that has an
//!   explicit limit, with the payload replaced by an empty string, so it is still valid JSON;
//! * the **payload**: the base64 text of the attachment, decoded on the fly into raw bytes that the
//!   caller streams into the blob store. Nothing of it is kept here.
//!
//! The scanner tracks only structure (braces, strings, which key is being read), not values; the
//! caller parses the metadata with `serde_json` afterwards, which is what validates the document.
//! The scanner must never disagree with a conforming JSON parser about where strings begin and
//! end, because that is how a payload could be smuggled into the metadata or the other way round;
//! it is therefore exact about quotes and escapes, and it refuses what it cannot track:
//!
//! * escape sequences in object keys at depth three or less (`"_attachments"` would name
//!   the attachments to a JSON parser and not to this scanner);
//! * duplicate top-level keys, a second attachment, `name`, `_id`, `versions` or `dist-tags`
//!   appearing after `_attachments` (the decisions taken before the payload is read must be the
//!   decisions the final document supports);
//! * an `_attachments` that is not an object of objects, a `data` that is not a string, escapes
//!   inside the payload, padding in the middle of the payload.
//!
//! Format reference: the publish document of the public registry API
//! (<https://github.com/npm/registry/blob/main/docs/REGISTRY-API.md>) and RFC 4648 section 4 for
//! the payload.

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;

/// Limits of a scan.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScanLimits {
    /// Largest metadata part (everything except the payload) in bytes.
    pub max_meta_bytes: usize,
    /// Deepest nesting of objects and arrays.
    pub max_depth: usize,
    /// Longest object key tracked at the top three levels.
    pub max_key_bytes: usize,
}

impl Default for ScanLimits {
    fn default() -> Self {
        Self {
            max_meta_bytes: 16 * 1024 * 1024,
            max_depth: 64,
            max_key_bytes: 1024,
        }
    }
}

/// Why a scan stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ScanError {
    /// The bytes are not the JSON this scanner can follow.
    #[error("malformed JSON: {0}")]
    Malformed(&'static str),
    /// A rule of the publish document is broken.
    #[error("{0}")]
    Rule(&'static str),
    /// The metadata part exceeds the limit.
    #[error("metadata part is larger than the limit of {limit} bytes")]
    MetaTooLarge {
        /// The limit that was crossed.
        limit: usize,
    },
    /// Nesting deeper than the limit.
    #[error("document is nested too deeply")]
    TooDeep,
    /// The payload is not canonical standard base64.
    #[error("the attachment is not valid base64")]
    BadBase64,
    /// The input ended before the document was complete.
    #[error("the document ended before it was complete")]
    Truncated,
}

/// Decoder of standard base64 that accepts its input in arbitrary pieces.
///
/// Complete groups of four characters are decoded in one call per piece (a per group call would
/// make the decoder the bottleneck of a large upload); up to three characters wait for the next
/// piece. Padding is only valid at the very end: once a padded group was seen, any further
/// character is an error, which the one-shot decoder would have caught and a naive streaming one
/// would not.
#[derive(Debug, Default)]
struct Base64Stream {
    carry: [u8; 4],
    carry_len: usize,
    padded: bool,
}

impl Base64Stream {
    fn push(&mut self, input: &[u8], out: &mut Vec<u8>) -> Result<(), ScanError> {
        if input.is_empty() {
            return Ok(());
        }
        if self.padded {
            return Err(ScanError::BadBase64);
        }
        let mut rest = input;
        if self.carry_len > 0 {
            let take = (4 - self.carry_len).min(rest.len());
            let (head, tail) = rest.split_at(take);
            self.carry[self.carry_len..self.carry_len + take].copy_from_slice(head);
            self.carry_len += take;
            rest = tail;
            if self.carry_len < 4 {
                return Ok(());
            }
            let group = self.carry;
            self.carry_len = 0;
            self.decode(&group, out)?;
            if self.padded && !rest.is_empty() {
                return Err(ScanError::BadBase64);
            }
        }
        let whole = rest.len() / 4 * 4;
        let (body, tail) = rest.split_at(whole);
        if !body.is_empty() {
            self.decode(body, out)?;
            if self.padded && !tail.is_empty() {
                return Err(ScanError::BadBase64);
            }
        }
        self.carry[..tail.len()].copy_from_slice(tail);
        self.carry_len = tail.len();
        Ok(())
    }

    fn decode(&mut self, groups: &[u8], out: &mut Vec<u8>) -> Result<(), ScanError> {
        let start = out.len();
        out.resize(start + groups.len() / 4 * 3, 0);
        let written = STANDARD
            .decode_slice(groups, &mut out[start..])
            .map_err(|_| ScanError::BadBase64)?;
        out.truncate(start + written);
        if groups.last() == Some(&b'=') {
            self.padded = true;
        }
        Ok(())
    }

    /// The closing quote was reached: nothing may be left over.
    fn finish(&self) -> Result<(), ScanError> {
        if self.carry_len == 0 {
            Ok(())
        } else {
            Err(ScanError::BadBase64)
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StringKind {
    /// An object key; `capture` is set for the top three levels, whose text is interpreted.
    Key { capture: bool },
    /// Any other string value, copied to the metadata.
    Value,
    /// The attachment payload: decoded, not copied.
    Data,
}

/// Where an object is in its `key : value , key : value` rhythm.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ObjState {
    /// A key (or the closing brace of an empty object) comes next.
    Key,
    /// A colon comes next.
    Colon,
    /// A value comes next.
    Value,
    /// A comma or the closing brace comes next.
    Comma,
}

#[derive(Debug)]
struct Frame {
    object: bool,
    state: ObjState,
    entries: usize,
    key: Option<String>,
}

/// The incremental scanner. Feed it the body piece by piece; read the results as they appear.
#[derive(Debug)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "the flags are independent facts of one scan (document finished, payload started, payload finished, key seen); an enum would have to list their combinations"
)]
pub struct Scanner {
    limits: ScanLimits,
    meta: Vec<u8>,
    frames: Vec<Frame>,
    string: Option<StringKind>,
    string_start: usize,
    escape: bool,
    in_scalar: bool,
    root_done: bool,
    top_keys: Vec<String>,
    attachments_seen: bool,
    attachment_entries: usize,
    data_seen: bool,
    attachment_name: Option<String>,
    data_started: bool,
    data_finished: bool,
    decoder: Base64Stream,
    decoded: Vec<u8>,
}

impl Scanner {
    /// A scanner with the given limits.
    #[must_use]
    pub fn new(limits: ScanLimits) -> Self {
        Self {
            limits,
            meta: Vec::new(),
            frames: Vec::new(),
            string: None,
            string_start: 0,
            escape: false,
            in_scalar: false,
            root_done: false,
            top_keys: Vec::new(),
            attachments_seen: false,
            attachment_entries: 0,
            data_seen: false,
            attachment_name: None,
            data_started: false,
            data_finished: false,
            decoder: Base64Stream::default(),
            decoded: Vec::new(),
        }
    }

    /// Feeds the next piece of the body.
    ///
    /// # Errors
    /// [`ScanError`]; after an error the scanner must not be fed again.
    pub fn feed(&mut self, input: &[u8]) -> Result<(), ScanError> {
        let mut at = 0;
        while at < input.len() {
            at += if let Some(kind) = self.string {
                self.feed_string(kind, &input[at..])?
            } else {
                self.feed_structural(input[at])?;
                1
            };
        }
        Ok(())
    }

    /// Declares the end of the body.
    ///
    /// # Errors
    /// [`ScanError::Truncated`] when the root object is not closed.
    pub fn finish(&self) -> Result<(), ScanError> {
        if self.root_done {
            Ok(())
        } else {
            Err(ScanError::Truncated)
        }
    }

    /// True once the opening quote of the payload has been read.
    #[must_use]
    pub fn data_started(&self) -> bool {
        self.data_started
    }

    /// True once the closing quote of the payload has been read.
    #[must_use]
    pub fn data_finished(&self) -> bool {
        self.data_finished
    }

    /// True when the root object is closed.
    #[must_use]
    pub fn is_complete(&self) -> bool {
        self.root_done
    }

    /// The key of the attachment entry (the file name), once it has been read.
    #[must_use]
    pub fn attachment_name(&self) -> Option<&str> {
        self.attachment_name.as_deref()
    }

    /// The metadata read so far, payload elided.
    #[must_use]
    pub fn meta(&self) -> &[u8] {
        &self.meta
    }

    /// Takes the payload bytes decoded since the last call.
    pub fn take_decoded(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.decoded)
    }

    /// The metadata read so far with the open containers closed, so that it parses as a document.
    /// Meant for the moment the payload starts, to read what came before it.
    #[must_use]
    pub fn head_document(&self) -> Vec<u8> {
        let mut out = self.meta.clone();
        for frame in self.frames.iter().rev() {
            out.push(if frame.object { b'}' } else { b']' });
        }
        out
    }

    fn push_meta(&mut self, bytes: &[u8]) -> Result<(), ScanError> {
        if self.meta.len().saturating_add(bytes.len()) > self.limits.max_meta_bytes {
            return Err(ScanError::MetaTooLarge {
                limit: self.limits.max_meta_bytes,
            });
        }
        self.meta.extend_from_slice(bytes);
        Ok(())
    }

    fn feed_string(&mut self, kind: StringKind, input: &[u8]) -> Result<usize, ScanError> {
        match kind {
            StringKind::Data => self.feed_data(input),
            StringKind::Key { .. } | StringKind::Value => self.feed_text(kind, input),
        }
    }

    fn feed_data(&mut self, input: &[u8]) -> Result<usize, ScanError> {
        match input.iter().position(|b| matches!(b, b'"' | b'\\')) {
            None => {
                self.decoder.push(input, &mut self.decoded)?;
                Ok(input.len())
            }
            Some(at) if input[at] == b'\\' => Err(ScanError::BadBase64),
            Some(at) => {
                self.decoder.push(&input[..at], &mut self.decoded)?;
                self.decoder.finish()?;
                self.string = None;
                self.data_finished = true;
                Ok(at + 1)
            }
        }
    }

    fn feed_text(&mut self, kind: StringKind, input: &[u8]) -> Result<usize, ScanError> {
        let mut at = 0;
        if self.escape {
            // The previous piece ended on a backslash: the first byte is the escaped character.
            self.push_meta(&input[..1])?;
            self.escape = false;
            at = 1;
        }
        while at < input.len() {
            let Some(found) = input[at..].iter().position(|b| matches!(b, b'"' | b'\\')) else {
                self.push_meta(&input[at..])?;
                return Ok(input.len());
            };
            let mark = at + found;
            if input[mark] == b'"' {
                self.push_meta(&input[at..=mark])?;
                self.string = None;
                self.end_string(kind)?;
                return Ok(mark + 1);
            }
            // A backslash: keep it and the character it escapes, which may be a quote.
            if mark + 1 < input.len() {
                self.push_meta(&input[at..mark + 2])?;
                at = mark + 2;
            } else {
                self.push_meta(&input[at..=mark])?;
                self.escape = true;
                return Ok(input.len());
            }
        }
        Ok(input.len())
    }

    fn end_string(&mut self, kind: StringKind) -> Result<(), ScanError> {
        let StringKind::Key { capture } = kind else {
            return Ok(());
        };
        if let Some(frame) = self.frames.last_mut() {
            frame.state = ObjState::Colon;
            frame.entries += 1;
        }
        if !capture {
            return Ok(());
        }
        let raw = &self.meta[self.string_start + 1..self.meta.len() - 1];
        if raw.contains(&b'\\') {
            return Err(ScanError::Rule(
                "escape sequences are not allowed in the keys of a publish document",
            ));
        }
        if raw.len() > self.limits.max_key_bytes {
            return Err(ScanError::Rule("a key of the publish document is too long"));
        }
        let key = String::from_utf8(raw.to_vec())
            .map_err(|_| ScanError::Malformed("a key is not valid UTF-8"))?;
        self.on_key(key)
    }

    fn on_key(&mut self, key: String) -> Result<(), ScanError> {
        let index = self.frames.len() - 1;
        match index {
            0 => {
                if self.top_keys.contains(&key) {
                    return Err(ScanError::Rule("duplicate key in the publish document"));
                }
                if self.attachments_seen
                    && matches!(key.as_str(), "name" | "_id" | "versions" | "dist-tags")
                {
                    return Err(ScanError::Rule(
                        "name, versions and dist-tags must precede _attachments",
                    ));
                }
                if self.top_keys.len() >= 256 {
                    return Err(ScanError::Rule("too many top-level keys"));
                }
                if key == "_attachments" {
                    self.attachments_seen = true;
                }
                self.top_keys.push(key.clone());
            }
            1 if self.in_attachments() => {
                self.attachment_entries += 1;
                if self.attachment_entries > 1 {
                    return Err(ScanError::Rule(
                        "only one attachment is accepted per publish",
                    ));
                }
                self.attachment_name = Some(key.clone());
            }
            2 if self.in_attachments() && key == "data" => {
                if self.data_seen {
                    return Err(ScanError::Rule("duplicate data in the attachment"));
                }
                self.data_seen = true;
            }
            _ => {}
        }
        if let Some(frame) = self.frames.get_mut(index) {
            frame.key = Some(key);
        }
        Ok(())
    }

    fn in_attachments(&self) -> bool {
        self.frames
            .first()
            .and_then(|f| f.key.as_deref())
            .is_some_and(|k| k == "_attachments")
    }

    /// Checks the first byte of a value that belongs to the innermost object frame.
    fn on_value_start(&mut self, byte: u8) -> Result<bool, ScanError> {
        let index = self.frames.len() - 1;
        if !self.in_attachments() {
            return Ok(false);
        }
        match index {
            0 | 1 if byte != b'{' => {
                Err(ScanError::Rule("_attachments must be an object of objects"))
            }
            2 if self.frames[2].key.as_deref() == Some("data") => {
                if byte == b'"' {
                    Ok(true)
                } else {
                    Err(ScanError::Rule("the attachment data must be a string"))
                }
            }
            _ => Ok(false),
        }
    }

    fn feed_structural(&mut self, byte: u8) -> Result<(), ScanError> {
        if self.root_done {
            return if byte.is_ascii_whitespace() {
                Ok(())
            } else {
                Err(ScanError::Malformed("data after the document"))
            };
        }
        match byte {
            b' ' | b'\t' | b'\n' | b'\r' => {
                self.in_scalar = false;
                if self.frames.is_empty() {
                    Ok(())
                } else {
                    self.push_meta(&[byte])
                }
            }
            b'"' => self.open_string(),
            b'{' | b'[' => self.open_container(byte),
            b'}' | b']' => self.close_container(byte),
            b':' => self.colon(),
            b',' => self.comma(),
            _ => self.scalar(byte),
        }
    }

    fn open_string(&mut self) -> Result<(), ScanError> {
        self.in_scalar = false;
        let Some(top) = self.frames.last() else {
            return Err(ScanError::Malformed("the document must be an object"));
        };
        if top.object && top.state == ObjState::Key {
            let capture = self.frames.len() <= 3;
            self.string = Some(StringKind::Key { capture });
            self.string_start = self.meta.len();
            return self.push_meta(b"\"");
        }
        if self.begin_value(b'"')? {
            // Both quotes go to the metadata now, so it stays a valid document whatever the
            // payload is; the content between them is decoded, never stored.
            self.push_meta(b"\"\"")?;
            self.string = Some(StringKind::Data);
            self.data_started = true;
            return Ok(());
        }
        self.string = Some(StringKind::Value);
        self.push_meta(b"\"")
    }

    /// Called with the first byte of every value. Moves the enclosing object on, and says whether
    /// the value is the attachment payload.
    fn begin_value(&mut self, byte: u8) -> Result<bool, ScanError> {
        let Some(top) = self.frames.last_mut() else {
            return Err(ScanError::Malformed("the document must be an object"));
        };
        if !top.object {
            return Ok(false);
        }
        if top.state != ObjState::Value {
            return Err(ScanError::Malformed("unexpected value"));
        }
        top.state = ObjState::Comma;
        self.on_value_start(byte)
    }

    fn open_container(&mut self, byte: u8) -> Result<(), ScanError> {
        self.in_scalar = false;
        if self.frames.is_empty() {
            if byte != b'{' {
                return Err(ScanError::Malformed("the document must be an object"));
            }
        } else {
            if self.frames.len() >= self.limits.max_depth {
                return Err(ScanError::TooDeep);
            }
            self.begin_value(byte)?;
        }
        self.frames.push(Frame {
            object: byte == b'{',
            state: ObjState::Key,
            entries: 0,
            key: None,
        });
        self.push_meta(&[byte])
    }

    fn close_container(&mut self, byte: u8) -> Result<(), ScanError> {
        self.in_scalar = false;
        let Some(frame) = self.frames.pop() else {
            return Err(ScanError::Malformed("unbalanced closing bracket"));
        };
        if frame.object != (byte == b'}') {
            return Err(ScanError::Malformed("mismatched closing bracket"));
        }
        let complete = match frame.state {
            ObjState::Comma => true,
            ObjState::Key => frame.entries == 0,
            ObjState::Colon | ObjState::Value => false,
        };
        if frame.object && !complete {
            return Err(ScanError::Malformed(
                "an object ends in the middle of an entry",
            ));
        }
        self.push_meta(&[byte])?;
        if self.frames.is_empty() {
            self.root_done = true;
        }
        Ok(())
    }

    fn colon(&mut self) -> Result<(), ScanError> {
        self.in_scalar = false;
        match self.frames.last_mut() {
            Some(frame) if frame.object && frame.state == ObjState::Colon => {
                frame.state = ObjState::Value;
            }
            _ => return Err(ScanError::Malformed("unexpected colon")),
        }
        self.push_meta(b":")
    }

    fn comma(&mut self) -> Result<(), ScanError> {
        self.in_scalar = false;
        match self.frames.last_mut() {
            Some(frame) if frame.object => {
                if frame.state != ObjState::Comma {
                    return Err(ScanError::Malformed("unexpected comma"));
                }
                frame.state = ObjState::Key;
            }
            Some(_) => {}
            None => return Err(ScanError::Malformed("unexpected comma")),
        }
        self.push_meta(b",")
    }

    /// A byte of a number, `true`, `false` or `null`. The first byte of one is a value start, the
    /// rest only continue it; whether the token is well formed is for the JSON parser that reads
    /// the metadata afterwards.
    fn scalar(&mut self, byte: u8) -> Result<(), ScanError> {
        if !self.in_scalar {
            self.begin_value(byte)?;
            self.in_scalar = true;
        }
        self.push_meta(&[byte])
    }
}

#[cfg(test)]
mod tests;
