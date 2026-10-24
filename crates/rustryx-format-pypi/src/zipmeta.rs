//! Reading `*.dist-info/METADATA` out of a wheel without reading the wheel.
//!
//! A wheel is a ZIP archive and can be gigabytes. Nothing here loads it: the central directory is
//! found from the end of the file with a handful of ranged reads, and only the one entry that
//! holds the metadata is read and inflated. The stages are pure functions over byte slices, so
//! they can be tested and fuzzed without I/O; [`read_metadata`] strings them together over a
//! [`RangeRead`] source (a blob store in the server, a slice in tests).
//!
//! Format reference: the ZIP application note (PKWARE `APPNOTE.TXT`) for the end of central
//! directory record, the ZIP64 records, central directory and local file headers, and the wheel
//! format (<https://packaging.python.org/en/latest/specifications/binary-distribution-format/>)
//! for where the metadata lives.
//!
//! Safety properties: every offset and length is checked (`checked_add`, `get`), every size is
//! bounded before memory is reserved, the inflated size is limited while inflating (a zip bomb
//! stops at the limit, not after), entries are never written anywhere (no extraction, so
//! no zip slip), encrypted entries are refused, and the entry's CRC-32 must match.

use async_trait::async_trait;
use bytes::Bytes;

/// Largest inflated `METADATA` accepted. The long description of a project is part of it.
pub const MAX_METADATA_BYTES: u64 = 8 * 1024 * 1024;
/// Largest compressed `METADATA` read.
pub const MAX_COMPRESSED_METADATA_BYTES: u64 = 8 * 1024 * 1024;
/// How much of the end of the file is read to find the end of central directory record: the
/// record (22 bytes), the longest comment (65 535), the ZIP64 locator and record (76) and some
/// extensible data.
pub const TAIL_WINDOW: u64 = 128 * 1024;
/// Largest central directory read. About 16 MiB holds 100 000 entries with realistic names.
pub const MAX_CENTRAL_DIRECTORY_BYTES: u64 = 16 * 1024 * 1024;
/// Most entries walked.
pub const MAX_ENTRIES: u64 = 200_000;

const EOCD_SIGNATURE: [u8; 4] = *b"PK\x05\x06";
const EOCD_LEN: usize = 22;
const ZIP64_LOCATOR_SIGNATURE: [u8; 4] = *b"PK\x06\x07";
const ZIP64_LOCATOR_LEN: usize = 20;
const ZIP64_EOCD_SIGNATURE: [u8; 4] = *b"PK\x06\x06";
const ZIP64_EOCD_LEN: usize = 56;
const CENTRAL_SIGNATURE: [u8; 4] = *b"PK\x01\x02";
const CENTRAL_LEN: usize = 46;
const LOCAL_SIGNATURE: [u8; 4] = *b"PK\x03\x04";
/// Length of the fixed part of a local file header.
pub const LOCAL_HEADER_LEN: usize = 30;
const ZIP64_EXTRA_ID: u16 = 1;
const METHOD_STORED: u16 = 0;
const METHOD_DEFLATED: u16 = 8;

/// Why the metadata could not be read.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ZipError {
    /// The file is too short to be a ZIP archive.
    #[error("file is too short to be a zip archive")]
    TooShort,
    /// No end of central directory record at the end of the file.
    #[error("not a zip archive")]
    NotAZip,
    /// A record is malformed or points outside the file.
    #[error("malformed zip structure: {0}")]
    Malformed(&'static str),
    /// A size or count is over its bound.
    #[error("zip structure too large: {0}")]
    TooLarge(&'static str),
    /// The archive has no `*.dist-info/METADATA`.
    #[error("wheel has no .dist-info/METADATA")]
    NoMetadata,
    /// More than one `*.dist-info/METADATA`.
    #[error("wheel has more than one .dist-info/METADATA")]
    MultipleMetadata,
    /// The metadata entry is encrypted.
    #[error("metadata entry is encrypted")]
    Encrypted,
    /// A compression method other than stored and deflate.
    #[error("unsupported compression method {0}")]
    UnsupportedMethod(u16),
    /// The inflated data does not match the sizes or the checksum of the entry.
    #[error("metadata entry is corrupt")]
    Corrupt,
    /// Reading the file failed (storage problem, not a problem of the archive).
    #[error("reading the file failed: {0}")]
    Io(String),
}

/// A source of byte ranges of one file.
#[async_trait]
pub trait RangeRead: Send + Sync {
    /// Reads exactly `len` bytes at `offset`. The caller has checked that the range lies inside
    /// the file and that `len` is within its own bound.
    async fn read_range(&self, offset: u64, len: u64) -> Result<Bytes, ZipError>;
}

/// A [`RangeRead`] over bytes in memory, for tests and fuzzing.
#[derive(Debug, Clone, Copy)]
pub struct SliceSource<'a>(pub &'a [u8]);

#[async_trait]
impl RangeRead for SliceSource<'_> {
    async fn read_range(&self, offset: u64, len: u64) -> Result<Bytes, ZipError> {
        let start = usize::try_from(offset).map_err(|_| ZipError::Malformed("offset"))?;
        let len = usize::try_from(len).map_err(|_| ZipError::Malformed("length"))?;
        let end = start.checked_add(len).ok_or(ZipError::Malformed("range"))?;
        self.0
            .get(start..end)
            .map(Bytes::copy_from_slice)
            .ok_or_else(|| ZipError::Io("range outside the file".to_owned()))
    }
}

/// Where the central directory is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CentralDirectory {
    /// Absolute offset of the first central directory header.
    pub offset: u64,
    /// Size in bytes.
    pub size: u64,
    /// Number of entries.
    pub entries: u64,
}

/// One central directory entry, with the sizes and the offset already resolved from ZIP64
/// extra fields.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ZipEntry {
    /// Path inside the archive.
    pub name: String,
    /// General purpose bit flags.
    pub flags: u16,
    /// Compression method.
    pub method: u16,
    /// CRC-32 of the inflated data.
    pub crc32: u32,
    /// Compressed size.
    pub compressed_size: u64,
    /// Inflated size.
    pub uncompressed_size: u64,
    /// Absolute offset of the local file header.
    pub local_header_offset: u64,
}

fn le16(bytes: &[u8], at: usize) -> Option<u16> {
    let end = at.checked_add(2)?;
    Some(u16::from_le_bytes(bytes.get(at..end)?.try_into().ok()?))
}

fn le32(bytes: &[u8], at: usize) -> Option<u32> {
    let end = at.checked_add(4)?;
    Some(u32::from_le_bytes(bytes.get(at..end)?.try_into().ok()?))
}

fn le64(bytes: &[u8], at: usize) -> Option<u64> {
    let end = at.checked_add(8)?;
    Some(u64::from_le_bytes(bytes.get(at..end)?.try_into().ok()?))
}

fn malformed(what: &'static str) -> ZipError {
    ZipError::Malformed(what)
}

/// Finds the central directory from the last bytes of the file.
///
/// `tail` is the file's last `tail.len()` bytes, starting at absolute offset `tail_start`;
/// `total_len` is the size of the whole file. A ZIP64 archive is recognized by the marker values
/// in the classic record and by the locator in front of it.
///
/// # Errors
/// [`ZipError`] when there is no valid record, or the directory lies outside the file or over a
/// bound.
pub fn find_central_directory(
    tail: &[u8],
    tail_start: u64,
    total_len: u64,
) -> Result<CentralDirectory, ZipError> {
    if total_len < EOCD_LEN as u64 {
        return Err(ZipError::TooShort);
    }
    let record_at = find_eocd(tail).ok_or(ZipError::NotAZip)?;
    let record = tail
        .get(record_at..)
        .ok_or_else(|| malformed("end record"))?;
    let disk = le16(record, 4).ok_or_else(|| malformed("end record"))?;
    let cd_disk = le16(record, 6).ok_or_else(|| malformed("end record"))?;
    if disk != 0 || cd_disk != 0 {
        return Err(malformed("multi-disk archive"));
    }
    let entries = le16(record, 10).ok_or_else(|| malformed("end record"))?;
    let size = le32(record, 12).ok_or_else(|| malformed("end record"))?;
    let offset = le32(record, 16).ok_or_else(|| malformed("end record"))?;
    let eocd_absolute = tail_start
        .checked_add(record_at as u64)
        .ok_or_else(|| malformed("end record"))?;

    let needs_zip64 = entries == u16::MAX || size == u32::MAX || offset == u32::MAX;
    let directory = if needs_zip64 {
        zip64_directory(tail, tail_start, record_at)?
    } else {
        CentralDirectory {
            offset: u64::from(offset),
            size: u64::from(size),
            entries: u64::from(entries),
        }
    };
    check_directory(&directory, eocd_absolute, total_len)?;
    Ok(directory)
}

/// Index of the end of central directory record: the last signature whose comment length leads
/// exactly to the end of the file.
fn find_eocd(tail: &[u8]) -> Option<usize> {
    let last_start = tail.len().checked_sub(EOCD_LEN)?;
    (0..=last_start).rev().find(|&at| {
        tail.get(at..at + 4) == Some(&EOCD_SIGNATURE[..])
            && le16(tail, at + 20)
                .is_some_and(|comment| at + EOCD_LEN + usize::from(comment) == tail.len())
    })
}

fn zip64_directory(
    tail: &[u8],
    tail_start: u64,
    record_at: usize,
) -> Result<CentralDirectory, ZipError> {
    let locator_at = record_at
        .checked_sub(ZIP64_LOCATOR_LEN)
        .ok_or_else(|| malformed("zip64 locator"))?;
    let locator = tail
        .get(locator_at..)
        .ok_or_else(|| malformed("zip64 locator"))?;
    if locator.get(..4) != Some(&ZIP64_LOCATOR_SIGNATURE[..]) {
        return Err(malformed("zip64 locator"));
    }
    let record_offset = le64(locator, 8).ok_or_else(|| malformed("zip64 locator"))?;
    let relative = record_offset
        .checked_sub(tail_start)
        .and_then(|r| usize::try_from(r).ok())
        .ok_or_else(|| malformed("zip64 record outside the read window"))?;
    let record = tail
        .get(relative..)
        .ok_or_else(|| malformed("zip64 record"))?;
    if record.len() < ZIP64_EOCD_LEN || record.get(..4) != Some(&ZIP64_EOCD_SIGNATURE[..]) {
        return Err(malformed("zip64 record"));
    }
    let disk = le32(record, 16).ok_or_else(|| malformed("zip64 record"))?;
    let cd_disk = le32(record, 20).ok_or_else(|| malformed("zip64 record"))?;
    if disk != 0 || cd_disk != 0 {
        return Err(malformed("multi-disk archive"));
    }
    Ok(CentralDirectory {
        entries: le64(record, 32).ok_or_else(|| malformed("zip64 record"))?,
        size: le64(record, 40).ok_or_else(|| malformed("zip64 record"))?,
        offset: le64(record, 48).ok_or_else(|| malformed("zip64 record"))?,
    })
}

fn check_directory(
    directory: &CentralDirectory,
    eocd_absolute: u64,
    total_len: u64,
) -> Result<(), ZipError> {
    if directory.size > MAX_CENTRAL_DIRECTORY_BYTES {
        return Err(ZipError::TooLarge("central directory"));
    }
    if directory.entries > MAX_ENTRIES {
        return Err(ZipError::TooLarge("entry count"));
    }
    let end = directory
        .offset
        .checked_add(directory.size)
        .ok_or_else(|| malformed("central directory range"))?;
    if end > eocd_absolute || end > total_len {
        return Err(malformed("central directory outside the file"));
    }
    let needed = directory
        .entries
        .checked_mul(CENTRAL_LEN as u64)
        .ok_or_else(|| malformed("entry count"))?;
    if needed > directory.size {
        return Err(malformed("entry count does not fit the directory"));
    }
    Ok(())
}

/// True for `<something>.dist-info/METADATA`, with exactly one directory level.
fn is_metadata_path(name: &str) -> bool {
    name.strip_suffix("/METADATA").is_some_and(|dir| {
        !dir.contains('/') && dir.len() > ".dist-info".len() && dir.ends_with(".dist-info")
    })
}

/// Walks the central directory and returns the entry of the wheel's `METADATA`.
///
/// # Errors
/// [`ZipError::NoMetadata`], [`ZipError::MultipleMetadata`] or a malformed record.
pub fn find_metadata_entry(directory: &[u8], entries: u64) -> Result<ZipEntry, ZipError> {
    let mut position = 0usize;
    let mut found: Option<ZipEntry> = None;
    for _ in 0..entries {
        let header = directory
            .get(position..)
            .filter(|rest| rest.len() >= CENTRAL_LEN)
            .ok_or_else(|| malformed("central directory header"))?;
        if header.get(..4) != Some(&CENTRAL_SIGNATURE[..]) {
            return Err(malformed("central directory signature"));
        }
        let name_len = usize::from(le16(header, 28).ok_or_else(|| malformed("header"))?);
        let extra_len = usize::from(le16(header, 30).ok_or_else(|| malformed("header"))?);
        let comment_len = usize::from(le16(header, 32).ok_or_else(|| malformed("header"))?);
        let total = CENTRAL_LEN
            .checked_add(name_len)
            .and_then(|n| n.checked_add(extra_len))
            .and_then(|n| n.checked_add(comment_len))
            .ok_or_else(|| malformed("header length"))?;
        if header.len() < total {
            return Err(malformed("central directory header is cut off"));
        }
        let name_bytes = header
            .get(CENTRAL_LEN..CENTRAL_LEN + name_len)
            .ok_or_else(|| malformed("name"))?;
        // A name that is not UTF-8 cannot be `*.dist-info/METADATA`, which is ASCII.
        if let Ok(name) = std::str::from_utf8(name_bytes)
            && is_metadata_path(name)
        {
            let extra = header
                .get(CENTRAL_LEN + name_len..CENTRAL_LEN + name_len + extra_len)
                .ok_or_else(|| malformed("extra field"))?;
            let entry = read_entry(header, name, extra)?;
            if found.replace(entry).is_some() {
                return Err(ZipError::MultipleMetadata);
            }
        }
        position += total;
    }
    found.ok_or(ZipError::NoMetadata)
}

fn read_entry(header: &[u8], name: &str, extra: &[u8]) -> Result<ZipEntry, ZipError> {
    let flags = le16(header, 8).ok_or_else(|| malformed("header"))?;
    let method = le16(header, 10).ok_or_else(|| malformed("header"))?;
    let crc32 = le32(header, 16).ok_or_else(|| malformed("header"))?;
    let compressed = le32(header, 20).ok_or_else(|| malformed("header"))?;
    let uncompressed = le32(header, 24).ok_or_else(|| malformed("header"))?;
    let offset = le32(header, 42).ok_or_else(|| malformed("header"))?;

    let mut zip64 = zip64_extra(extra);
    let mut next = || zip64.next().ok_or_else(|| malformed("zip64 extra field"));
    let uncompressed_size = if uncompressed == u32::MAX {
        next()?
    } else {
        u64::from(uncompressed)
    };
    let compressed_size = if compressed == u32::MAX {
        next()?
    } else {
        u64::from(compressed)
    };
    let local_header_offset = if offset == u32::MAX {
        next()?
    } else {
        u64::from(offset)
    };
    Ok(ZipEntry {
        name: name.to_owned(),
        flags,
        method,
        crc32,
        compressed_size,
        uncompressed_size,
        local_header_offset,
    })
}

/// The 64-bit values of the ZIP64 extra field, in the order the format defines (uncompressed
/// size, compressed size, local header offset); the caller takes the ones whose classic field
/// holds the marker.
fn zip64_extra(extra: &[u8]) -> impl Iterator<Item = u64> + '_ {
    let mut at = 0usize;
    let mut values: Option<&[u8]> = None;
    while let (Some(id), Some(size)) = (le16(extra, at), le16(extra, at + 2)) {
        let start = at + 4;
        let end = start + usize::from(size);
        let Some(data) = extra.get(start..end) else {
            break;
        };
        if id == ZIP64_EXTRA_ID {
            values = Some(data);
            break;
        }
        at = end;
    }
    let data = values.unwrap_or_default();
    (0..data.len() / 8).filter_map(move |i| le64(data, i * 8))
}

/// Length of a local file header including its name and extra field, from its first 30 bytes.
/// The entry's data starts right after it.
///
/// # Errors
/// [`ZipError::Malformed`] when the signature is wrong.
pub fn local_header_len(header: &[u8]) -> Result<u64, ZipError> {
    if header.get(..4) != Some(&LOCAL_SIGNATURE[..]) {
        return Err(malformed("local header signature"));
    }
    let name = le16(header, 26).ok_or_else(|| malformed("local header"))?;
    let extra = le16(header, 28).ok_or_else(|| malformed("local header"))?;
    Ok(LOCAL_HEADER_LEN as u64 + u64::from(name) + u64::from(extra))
}

/// Bounds of the entry that are checked before anything is read.
fn check_entry(entry: &ZipEntry) -> Result<(), ZipError> {
    if entry.flags & 0x0041 != 0 {
        return Err(ZipError::Encrypted);
    }
    if entry.method != METHOD_STORED && entry.method != METHOD_DEFLATED {
        return Err(ZipError::UnsupportedMethod(entry.method));
    }
    if entry.uncompressed_size > MAX_METADATA_BYTES
        || entry.compressed_size > MAX_COMPRESSED_METADATA_BYTES
    {
        return Err(ZipError::TooLarge("METADATA"));
    }
    if entry.method == METHOD_STORED && entry.compressed_size != entry.uncompressed_size {
        return Err(ZipError::Corrupt);
    }
    Ok(())
}

/// Inflates (or copies) the entry's data and verifies its size and CRC-32. The inflater is told
/// the declared size as its limit, so a stream that expands further is refused when it crosses
/// that line.
///
/// # Errors
/// [`ZipError::Corrupt`] on any mismatch or inflate failure, [`ZipError::UnsupportedMethod`],
/// [`ZipError::Encrypted`] or [`ZipError::TooLarge`] from the entry's own fields.
pub fn decode_entry(entry: &ZipEntry, data: &[u8]) -> Result<Vec<u8>, ZipError> {
    check_entry(entry)?;
    let limit =
        usize::try_from(entry.uncompressed_size).map_err(|_| ZipError::TooLarge("METADATA"))?;
    let output = if entry.method == METHOD_STORED {
        data.to_vec()
    } else {
        miniz_oxide::inflate::decompress_to_vec_with_limit(data, limit)
            .map_err(|_| ZipError::Corrupt)?
    };
    if output.len() != limit || crc32(&output) != entry.crc32 {
        return Err(ZipError::Corrupt);
    }
    Ok(output)
}

const CRC_TABLE: [u32; 256] = {
    let mut table = [0u32; 256];
    let mut n = 0usize;
    while n < 256 {
        #[allow(
            clippy::cast_possible_truncation,
            reason = "n stays below 256, so the conversion to u32 cannot truncate"
        )]
        let mut c = n as u32;
        let mut k = 0;
        while k < 8 {
            c = if c & 1 == 1 {
                0xEDB8_8320 ^ (c >> 1)
            } else {
                c >> 1
            };
            k += 1;
        }
        table[n] = c;
        n += 1;
    }
    table
};

/// CRC-32 (IEEE) as ZIP uses it.
#[must_use]
pub fn crc32(data: &[u8]) -> u32 {
    !data.iter().fold(!0u32, |crc, byte| {
        let low = u8::try_from(crc & 0xFF).unwrap_or(0);
        let index = usize::from(low ^ byte);
        CRC_TABLE.get(index).copied().unwrap_or(0) ^ (crc >> 8)
    })
}

/// Reads and returns the wheel's `METADATA` file from `source`, a file of `total_len` bytes.
///
/// Four ranged reads at most: the tail, the central directory, the local header and the entry
/// data. Cancel safe (nothing is held between reads).
///
/// # Errors
/// [`ZipError`]; [`ZipError::Io`] means the read failed, every other variant means the archive is
/// not a usable wheel.
pub async fn read_metadata<R: RangeRead + ?Sized>(
    source: &R,
    total_len: u64,
) -> Result<Vec<u8>, ZipError> {
    if total_len < EOCD_LEN as u64 {
        return Err(ZipError::TooShort);
    }
    let tail_len = total_len.min(TAIL_WINDOW);
    let tail_start = total_len - tail_len;
    let tail = source.read_range(tail_start, tail_len).await?;
    let directory = find_central_directory(&tail, tail_start, total_len)?;

    let directory_bytes = source.read_range(directory.offset, directory.size).await?;
    let entry = find_metadata_entry(&directory_bytes, directory.entries)?;
    check_entry(&entry)?;

    let header_end = entry
        .local_header_offset
        .checked_add(LOCAL_HEADER_LEN as u64)
        .filter(|end| *end <= total_len)
        .ok_or_else(|| malformed("local header outside the file"))?;
    let header = source
        .read_range(entry.local_header_offset, LOCAL_HEADER_LEN as u64)
        .await?;
    let data_start = entry
        .local_header_offset
        .checked_add(local_header_len(&header)?)
        .filter(|start| *start >= header_end)
        .ok_or_else(|| malformed("local header"))?;
    let data_end = data_start
        .checked_add(entry.compressed_size)
        .filter(|end| *end <= directory.offset)
        .ok_or_else(|| malformed("entry data outside the file"))?;
    let data = source.read_range(data_start, data_end - data_start).await?;
    decode_entry(&entry, &data)
}

#[cfg(test)]
pub(crate) mod testing {
    //! A tiny writer of ZIP archives for tests: stored or deflated entries, optional ZIP64 end
    //! records, so that every branch of the reader has an input.

    use super::crc32;

    pub(crate) struct Entry<'a> {
        pub name: &'a str,
        pub data: &'a [u8],
        pub deflate: bool,
        pub flags: u16,
    }

    impl<'a> Entry<'a> {
        pub(crate) fn stored(name: &'a str, data: &'a [u8]) -> Self {
            Self {
                name,
                data,
                deflate: false,
                flags: 0,
            }
        }

        pub(crate) fn deflated(name: &'a str, data: &'a [u8]) -> Self {
            Self {
                name,
                data,
                deflate: true,
                flags: 0,
            }
        }
    }

    fn put16(out: &mut Vec<u8>, v: u16) {
        out.extend_from_slice(&v.to_le_bytes());
    }
    fn put32(out: &mut Vec<u8>, v: u32) {
        out.extend_from_slice(&v.to_le_bytes());
    }
    fn put64(out: &mut Vec<u8>, v: u64) {
        out.extend_from_slice(&v.to_le_bytes());
    }

    /// Builds an archive. `zip64` writes the ZIP64 end records and the marker values.
    pub(crate) fn build(entries: &[Entry<'_>], comment: &[u8], zip64: bool) -> Vec<u8> {
        let mut out = Vec::new();
        let mut central = Vec::new();
        for entry in entries {
            let offset = out.len() as u32;
            let payload = if entry.deflate {
                miniz_oxide::deflate::compress_to_vec(entry.data, 6)
            } else {
                entry.data.to_vec()
            };
            let crc = crc32(entry.data);
            let method: u16 = if entry.deflate { 8 } else { 0 };
            out.extend_from_slice(b"PK\x03\x04");
            put16(&mut out, 20);
            put16(&mut out, entry.flags);
            put16(&mut out, method);
            put32(&mut out, 0);
            put32(&mut out, crc);
            put32(&mut out, payload.len() as u32);
            put32(&mut out, entry.data.len() as u32);
            put16(&mut out, entry.name.len() as u16);
            put16(&mut out, 4);
            out.extend_from_slice(entry.name.as_bytes());
            out.extend_from_slice(&[0xCA, 0xFE, 0x00, 0x00]);
            out.extend_from_slice(&payload);

            central.extend_from_slice(b"PK\x01\x02");
            put16(&mut central, 20);
            put16(&mut central, 20);
            put16(&mut central, entry.flags);
            put16(&mut central, method);
            put32(&mut central, 0);
            put32(&mut central, crc);
            put32(&mut central, payload.len() as u32);
            put32(&mut central, entry.data.len() as u32);
            put16(&mut central, entry.name.len() as u16);
            put16(&mut central, 0);
            put16(&mut central, 0);
            put16(&mut central, 0);
            put16(&mut central, 0);
            put32(&mut central, 0);
            put32(&mut central, offset);
            central.extend_from_slice(entry.name.as_bytes());
        }
        let cd_offset = out.len() as u64;
        out.extend_from_slice(&central);
        let cd_size = central.len() as u64;
        if zip64 {
            let record_at = out.len() as u64;
            out.extend_from_slice(b"PK\x06\x06");
            put64(&mut out, 44);
            put16(&mut out, 45);
            put16(&mut out, 45);
            put32(&mut out, 0);
            put32(&mut out, 0);
            put64(&mut out, entries.len() as u64);
            put64(&mut out, entries.len() as u64);
            put64(&mut out, cd_size);
            put64(&mut out, cd_offset);
            out.extend_from_slice(b"PK\x06\x07");
            put32(&mut out, 0);
            put64(&mut out, record_at);
            put32(&mut out, 1);
        }
        out.extend_from_slice(b"PK\x05\x06");
        put16(&mut out, 0);
        put16(&mut out, 0);
        if zip64 {
            put16(&mut out, u16::MAX);
            put16(&mut out, u16::MAX);
            put32(&mut out, u32::MAX);
            put32(&mut out, u32::MAX);
        } else {
            put16(&mut out, entries.len() as u16);
            put16(&mut out, entries.len() as u16);
            put32(&mut out, cd_size as u32);
            put32(&mut out, cd_offset as u32);
        }
        put16(&mut out, comment.len() as u16);
        out.extend_from_slice(comment);
        out
    }
}

#[cfg(test)]
mod tests {
    use super::testing::{Entry, build};
    use super::*;
    use proptest::prelude::*;

    const META: &[u8] = b"Metadata-Version: 2.1\nName: foo\nVersion: 1.0\n";

    fn run(bytes: &[u8]) -> Result<Vec<u8>, ZipError> {
        futures::executor::block_on(read_metadata(&SliceSource(bytes), bytes.len() as u64))
    }

    #[test]
    fn crc32_matches_the_known_check_value() {
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
        assert_eq!(crc32(b""), 0);
    }

    #[test]
    fn reads_deflated_and_stored_metadata() {
        for deflate in [true, false] {
            let entry = if deflate {
                Entry::deflated("foo-1.0.dist-info/METADATA", META)
            } else {
                Entry::stored("foo-1.0.dist-info/METADATA", META)
            };
            let zip = build(
                &[
                    Entry::stored("foo/__init__.py", b"x = 1\n"),
                    entry,
                    Entry::stored("foo-1.0.dist-info/RECORD", b"r"),
                ],
                b"",
                false,
            );
            assert_eq!(run(&zip).unwrap(), META);
        }
    }

    #[test]
    fn reads_zip64_archives_and_archives_with_a_comment() {
        let entries = [Entry::deflated("foo-1.0.dist-info/METADATA", META)];
        assert_eq!(run(&build(&entries, b"", true)).unwrap(), META);
        assert_eq!(
            run(&build(&entries, b"a comment with PK\x05\x06 inside", false)).unwrap(),
            META
        );
    }

    #[test]
    fn metadata_far_from_the_end_is_found() {
        // Repaired wheels keep large `.libs` directories after the dist-info: the read must not
        // depend on the metadata being near the end.
        let big = vec![7u8; 600 * 1024];
        let zip = build(
            &[
                Entry::stored("foo-1.0.dist-info/METADATA", META),
                Entry::stored("foo.libs/libbig.so", &big),
            ],
            b"",
            false,
        );
        assert_eq!(run(&zip).unwrap(), META);
    }

    #[test]
    fn refuses_archives_without_usable_metadata() {
        let none = build(&[Entry::stored("foo/__init__.py", b"")], b"", false);
        assert_eq!(run(&none), Err(ZipError::NoMetadata));
        let nested = build(&[Entry::stored("a/b.dist-info/METADATA", META)], b"", false);
        assert_eq!(run(&nested), Err(ZipError::NoMetadata));
        let bare = build(&[Entry::stored(".dist-info/METADATA", META)], b"", false);
        assert_eq!(run(&bare), Err(ZipError::NoMetadata));
        let twice = build(
            &[
                Entry::stored("a-1.dist-info/METADATA", META),
                Entry::stored("b-1.dist-info/METADATA", META),
            ],
            b"",
            false,
        );
        assert_eq!(run(&twice), Err(ZipError::MultipleMetadata));
    }

    #[test]
    fn refuses_encrypted_and_unsupported_entries() {
        let mut encrypted = Entry::stored("a-1.dist-info/METADATA", META);
        encrypted.flags = 1;
        assert_eq!(
            run(&build(&[encrypted], b"", false)),
            Err(ZipError::Encrypted)
        );
    }

    #[test]
    fn refuses_a_corrupt_entry() {
        let mut zip = build(&[Entry::stored("a-1.dist-info/METADATA", META)], b"", false);
        // Flip a byte of the stored data: the CRC no longer matches.
        let at = zip.windows(META.len()).position(|w| w == META).unwrap();
        zip[at + 3] ^= 0xff;
        assert_eq!(run(&zip), Err(ZipError::Corrupt));
    }

    #[test]
    fn must_not_inflate_past_the_declared_size() {
        // A deflate stream that expands to far more than the entry declares: the inflater stops
        // at the declared limit and the entry is refused.
        let bomb = vec![0u8; 4 * 1024 * 1024];
        let mut zip = build(
            &[Entry::deflated("a-1.dist-info/METADATA", &bomb)],
            b"",
            false,
        );
        // Lower the declared uncompressed size in the central directory (and the local header
        // is not consulted for sizes) to 1 KiB.
        let cd = zip.windows(4).rposition(|w| w == b"PK\x01\x02").unwrap();
        zip[cd + 24..cd + 28].copy_from_slice(&1024u32.to_le_bytes());
        assert_eq!(run(&zip), Err(ZipError::Corrupt));
    }

    #[test]
    fn must_not_read_a_metadata_entry_over_the_size_bound() {
        let over = vec![b'a'; (MAX_METADATA_BYTES + 1) as usize];
        let zip = build(
            &[Entry::deflated("a-1.dist-info/METADATA", &over)],
            b"",
            false,
        );
        assert_eq!(run(&zip), Err(ZipError::TooLarge("METADATA")));
    }

    #[test]
    fn refuses_broken_structure() {
        assert_eq!(run(b""), Err(ZipError::TooShort));
        assert_eq!(run(b"short"), Err(ZipError::TooShort));
        assert_eq!(run(&[0u8; 100]), Err(ZipError::NotAZip));
        let good = build(&[Entry::stored("a-1.dist-info/METADATA", META)], b"", false);
        // Every truncation of a valid archive is an error, never a panic or a wrong answer.
        for len in 0..good.len() {
            let result = run(&good[..len]);
            assert!(result.is_err(), "truncated at {len}");
        }
        // A central directory that claims more entries than fit.
        let mut lying = good.clone();
        let eocd = lying.windows(4).rposition(|w| w == b"PK\x05\x06").unwrap();
        lying[eocd + 10..eocd + 12].copy_from_slice(&100u16.to_le_bytes());
        assert!(run(&lying).is_err());
        // A directory offset that points past the file.
        let mut far = good.clone();
        let eocd = far.windows(4).rposition(|w| w == b"PK\x05\x06").unwrap();
        far[eocd + 16..eocd + 20].copy_from_slice(&0x7fff_ffffu32.to_le_bytes());
        assert!(run(&far).is_err());
        // Multi-disk archives are refused.
        let mut disks = good;
        let eocd = disks.windows(4).rposition(|w| w == b"PK\x05\x06").unwrap();
        disks[eocd + 4] = 1;
        assert_eq!(run(&disks), Err(ZipError::Malformed("multi-disk archive")));
    }

    #[test]
    fn zip64_marker_without_records_is_refused() {
        let mut zip = build(&[Entry::stored("a-1.dist-info/METADATA", META)], b"", false);
        let eocd = zip.windows(4).rposition(|w| w == b"PK\x05\x06").unwrap();
        zip[eocd + 16..eocd + 20].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(run(&zip).is_err());
    }

    #[test]
    fn io_failures_are_reported_as_io() {
        struct Failing;
        #[async_trait]
        impl RangeRead for Failing {
            async fn read_range(&self, _: u64, _: u64) -> Result<Bytes, ZipError> {
                Err(ZipError::Io("store down".into()))
            }
        }
        let result = futures::executor::block_on(read_metadata(&Failing, 1000));
        assert_eq!(result, Err(ZipError::Io("store down".into())));
    }

    proptest! {
        #[test]
        fn read_never_panics_on_arbitrary_bytes(bytes in proptest::collection::vec(any::<u8>(), 0..600)) {
            let _ = run(&bytes);
        }

        #[test]
        fn read_never_panics_when_a_valid_archive_is_damaged(
            index in 0usize..400, value in any::<u8>(), cut in 0usize..400,
        ) {
            let mut zip = build(&[Entry::deflated("a-1.dist-info/METADATA", META), Entry::stored("a/b.py", b"1")], b"c", false);
            if let Some(byte) = zip.get_mut(index) {
                *byte = value;
            }
            zip.truncate(zip.len().saturating_sub(cut % 8));
            let _ = run(&zip);
        }

        #[test]
        fn metadata_round_trips_through_an_archive(data in proptest::collection::vec(any::<u8>(), 0..2000), deflate in any::<bool>()) {
            let entry = if deflate { Entry::deflated("p-1.dist-info/METADATA", &data) } else { Entry::stored("p-1.dist-info/METADATA", &data) };
            let zip = build(&[entry], b"", false);
            prop_assert_eq!(run(&zip).unwrap(), data);
        }
    }
}
