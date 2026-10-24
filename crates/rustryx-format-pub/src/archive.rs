//! Reading the gzip-compressed tar of a package without trusting it.
//!
//! [`inspect`] is a synchronous function over a [`Read`]: it runs on the blocking pool, behind a
//! permit, and is fed from the blob store after the archive has been streamed there (the archive
//! is never held whole). It reads the **entire** archive, not just up to `pubspec.yaml`, because
//! the client extracts all of it: every entry is checked, and every decompressed byte is counted.
//!
//! What it refuses, and which mechanism forces the choice:
//!
//! * **Decompression bombs.** The decoder is wrapped in a reader that counts decompressed bytes
//!   and fails the read itself when the total passes `max_uncompressed_bytes`, or when the
//!   expansion ratio against the stored size passes `max_ratio` (checked from `ratio_floor_bytes`
//!   on, so a small package of highly repetitive text is not mistaken for a bomb). The limit is
//!   enforced inside `read`, so no parser above it can be talked into allocating past it.
//! * **Memory amplification through tar extension headers.** The `tar` crate reads GNU long names
//!   and PAX records into memory with a size taken from the header. The crate is therefore driven
//!   in raw mode and those two header kinds are read here, with small fixed caps.
//! * **Tar slip.** Every entry path (after PAX or GNU long name resolution) must be relative,
//!   valid UTF-8, free of `..` and `.` segments, backslashes, control characters, empty segments
//!   and a drive prefix. The clients extract to disk, so this check protects them even though the
//!   server never writes an archive path anywhere.
//! * **Links and devices.** Only regular files and directories are accepted. A symlink or a hard
//!   link in a package is how an extractor is made to write outside its directory.
//! * **Parser differentials on the manifest.** A PAX `size` record would make this reader and an
//!   extractor disagree about where the next entry starts, so it is refused. A second
//!   `pubspec.yaml`, a `pubspec.yaml` that is not a regular file, and a case variant of the name
//!   (on a case-insensitive file system it would overwrite the validated manifest) are refused.
//! * **Hidden payloads.** Data after the end-of-archive marker must be zero padding, and nothing
//!   may follow the gzip member.

use std::cell::Cell;
use std::io::{self, BufRead, BufReader, Read};
use std::rc::Rc;

use flate2::bufread::GzDecoder;
use tar::{Archive, EntryType, PaxExtensions};

/// The manifest file name. It is read from the root of the archive only.
pub const PUBSPEC_NAME: &str = "pubspec.yaml";

/// Longest GNU long name entry accepted, in bytes.
const MAX_LONG_NAME_BYTES: u64 = 4096;
/// Largest PAX extension record block accepted, in bytes.
const MAX_PAX_BYTES: u64 = 64 * 1024;
/// Longest single path segment, in bytes (the limit of common file systems).
const MAX_SEGMENT_BYTES: usize = 255;
/// Size of the buffer that reads the compressed side.
const COMPRESSED_BUFFER_BYTES: usize = 64 * 1024;

/// Hard limits of [`inspect`]. The defaults follow the limits of the public registry (100 MiB
/// compressed, 256 MiB extracted) and the ratio of SEC-33.
#[derive(Debug, Clone)]
pub struct ArchiveLimits {
    /// Most decompressed bytes of the whole archive, headers and padding included.
    pub max_uncompressed_bytes: u64,
    /// Most tar headers (entries and extension headers).
    pub max_entries: usize,
    /// Largest single entry.
    pub max_entry_bytes: u64,
    /// Largest `pubspec.yaml`.
    pub max_pubspec_bytes: u64,
    /// Longest entry path, in bytes.
    pub max_path_bytes: usize,
    /// Largest allowed decompressed / compressed ratio.
    pub max_ratio: u64,
    /// The ratio is only enforced once this many bytes were decompressed.
    pub ratio_floor_bytes: u64,
}

impl Default for ArchiveLimits {
    fn default() -> Self {
        Self {
            max_uncompressed_bytes: 256 * 1024 * 1024,
            max_entries: 50_000,
            max_entry_bytes: 128 * 1024 * 1024,
            max_pubspec_bytes: 256 * 1024,
            max_path_bytes: 1024,
            max_ratio: 100,
            ratio_floor_bytes: 4 * 1024 * 1024,
        }
    }
}

/// Which limit an archive broke.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LimitKind {
    /// Too many decompressed bytes.
    Uncompressed,
    /// Decompressed size is too large for the compressed size (a bomb).
    Ratio,
    /// Too many entries.
    Entries,
    /// One entry is too large.
    EntryBytes,
    /// `pubspec.yaml` is too large.
    PubspecBytes,
}

impl LimitKind {
    /// Client-safe sentence that names the limit.
    #[must_use]
    pub const fn message(self) -> &'static str {
        match self {
            Self::Uncompressed => "the archive is larger than the extracted size limit",
            Self::Ratio => "the archive expands too much for its compressed size",
            Self::Entries => "the archive has more entries than the limit",
            Self::EntryBytes => "the archive contains a file larger than the limit",
            Self::PubspecBytes => "pubspec.yaml is larger than the limit",
        }
    }
}

/// Why an archive was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ArchiveError {
    /// A limit was exceeded. Reported as 422: the content is well formed enough to read, and
    /// semantically unacceptable.
    #[error("{}", .0.message())]
    Limit(LimitKind),
    /// Not a gzip stream, not a tar, a header with a bad checksum, or a truncated stream.
    #[error("the archive is not a valid gzip-compressed tar file")]
    Malformed,
    /// An entry path could leave the extraction directory or is not portable.
    #[error("the archive contains an unsafe file path")]
    UnsafePath,
    /// A link, a device, a sparse file, a global header or another entry kind that a package
    /// must not contain.
    #[error("the archive contains an entry that is not a regular file or a directory")]
    UnsupportedEntry,
    /// No `pubspec.yaml` at the root.
    #[error("pubspec.yaml is missing from the root of the archive")]
    PubspecMissing,
    /// Two entries named `pubspec.yaml`.
    #[error("the archive contains more than one pubspec.yaml")]
    PubspecDuplicate,
    /// An entry that is named like the manifest but is not the manifest.
    #[error("pubspec.yaml must be a regular file named exactly pubspec.yaml at the root")]
    PubspecInvalid,
    /// Bytes after the end of the archive or after the gzip stream.
    #[error("the archive has data after its end")]
    TrailingData,
}

impl ArchiveError {
    /// A short, bounded label for metrics.
    #[must_use]
    pub const fn reason(self) -> &'static str {
        match self {
            Self::Limit(LimitKind::Ratio) => "bomb_ratio",
            Self::Limit(_) => "limit",
            Self::Malformed => "malformed",
            Self::UnsafePath => "unsafe_path",
            Self::UnsupportedEntry => "unsupported_entry",
            Self::PubspecMissing => "pubspec_missing",
            Self::PubspecDuplicate => "pubspec_duplicate",
            Self::PubspecInvalid => "pubspec_invalid",
            Self::TrailingData => "trailing_data",
        }
    }
}

/// What [`inspect`] found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArchiveReport {
    /// The bytes of the root `pubspec.yaml`.
    pub pubspec: Vec<u8>,
    /// Number of tar headers read.
    pub entries: usize,
    /// Decompressed size of the whole archive.
    pub uncompressed_bytes: u64,
}

/// Decompressed byte counter that fails the read when a limit is crossed.
struct Limited<'a, R> {
    inner: R,
    read: u64,
    compressed_len: u64,
    limits: &'a ArchiveLimits,
    tripped: Rc<Cell<Option<LimitKind>>>,
}

impl<R> Limited<'_, R> {
    fn trip(&self, kind: LimitKind) -> io::Error {
        self.tripped.set(Some(kind));
        io::Error::other("archive limit exceeded")
    }
}

impl<R: Read> Read for Limited<'_, R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = self.inner.read(buf)?;
        self.read = self.read.saturating_add(n as u64);
        if self.read > self.limits.max_uncompressed_bytes {
            return Err(self.trip(LimitKind::Uncompressed));
        }
        // Integer division rounds down, so the ratio is only "over" when it really is.
        if self.read > self.limits.ratio_floor_bytes
            && self.read / self.compressed_len.max(1) > self.limits.max_ratio
        {
            return Err(self.trip(LimitKind::Ratio));
        }
        Ok(n)
    }
}

/// Reads a whole package archive from `source` (`compressed_len` bytes of gzip) and returns its
/// manifest, or the reason it is refused.
///
/// The function never panics on any input, which the fuzz target `gzip_tar` and the property
/// tests check, and it holds at most the manifest and one extension header in memory.
///
/// # Errors
/// An [`ArchiveError`]; see the module documentation for the rules.
pub fn inspect<R: Read>(
    source: R,
    compressed_len: u64,
    limits: &ArchiveLimits,
) -> Result<ArchiveReport, ArchiveError> {
    let tripped = Rc::new(Cell::new(None));
    let decoder = GzDecoder::new(BufReader::with_capacity(COMPRESSED_BUFFER_BYTES, source));
    let limited = Limited {
        inner: decoder,
        read: 0,
        compressed_len,
        limits,
        tripped: Rc::clone(&tripped),
    };
    let mut archive = Archive::new(limited);
    let io_error = |error: io::Error| classify_io(&error, &tripped);
    let scan = scan_entries(&mut archive, limits, &io_error)?;

    let mut limited = archive.into_inner();
    drain_padding(&mut limited, &io_error)?;
    let uncompressed_bytes = limited.read;
    // The decoder has verified the gzip trailer by reaching its end. Whatever is left in the
    // buffered reader below it is data after the member: a second member or garbage.
    let mut compressed = limited.inner.into_inner();
    let rest = compressed.fill_buf().map_err(io_error)?;
    if !rest.is_empty() {
        return Err(ArchiveError::TrailingData);
    }
    let pubspec = scan.pubspec.ok_or(ArchiveError::PubspecMissing)?;
    Ok(ArchiveReport {
        pubspec,
        entries: scan.entries,
        uncompressed_bytes,
    })
}

fn classify_io(_error: &io::Error, tripped: &Cell<Option<LimitKind>>) -> ArchiveError {
    match tripped.get() {
        Some(kind) => ArchiveError::Limit(kind),
        None => ArchiveError::Malformed,
    }
}

/// What the entry loop collects.
struct Scan {
    entries: usize,
    pubspec: Option<Vec<u8>>,
}

/// Records of a PAX extended header that this reader acts on.
#[derive(Default)]
struct Pax {
    path: Option<Vec<u8>>,
}

fn scan_entries<R: Read>(
    archive: &mut Archive<R>,
    limits: &ArchiveLimits,
    io_error: &dyn Fn(io::Error) -> ArchiveError,
) -> Result<Scan, ArchiveError> {
    // Raw mode: the crate must not read long names and PAX blocks by itself (see module docs).
    let entries = archive.entries().map_err(io_error)?.raw(true);
    let mut scan = Scan {
        entries: 0,
        pubspec: None,
    };
    let mut long_name: Option<Vec<u8>> = None;
    let mut pax: Option<Pax> = None;
    for entry in entries {
        let mut entry = entry.map_err(io_error)?;
        scan.entries += 1;
        if scan.entries > limits.max_entries {
            return Err(ArchiveError::Limit(LimitKind::Entries));
        }
        let size = entry.size();
        if size > limits.max_entry_bytes {
            return Err(ArchiveError::Limit(LimitKind::EntryBytes));
        }
        let kind = entry.header().entry_type();
        match kind {
            EntryType::GNULongName => {
                if long_name.is_some() {
                    return Err(ArchiveError::Malformed);
                }
                let mut name = read_extension(&mut entry, size, MAX_LONG_NAME_BYTES, io_error)?;
                // The name is stored with its terminating NUL.
                if name.last() == Some(&0) {
                    name.pop();
                }
                long_name = Some(name);
            }
            EntryType::XHeader => {
                if pax.is_some() {
                    return Err(ArchiveError::Malformed);
                }
                let block = read_extension(&mut entry, size, MAX_PAX_BYTES, io_error)?;
                pax = Some(parse_pax(&block)?);
            }
            EntryType::Regular | EntryType::Directory => {
                let pending_pax = pax.take();
                let name = match pending_pax
                    .and_then(|p| p.path)
                    .or_else(|| long_name.take())
                {
                    Some(name) => name,
                    None => entry.header().path_bytes().into_owned(),
                };
                long_name = None;
                let directory = kind == EntryType::Directory;
                let path = entry_path(&name, directory, limits)?;
                if let Some(path) = path {
                    visit_data_entry(&mut scan, &mut entry, &path, kind, size, limits, io_error)?;
                }
            }
            // Links, devices, fifos, sparse files, global PAX headers, GNU long links and
            // anything unknown.
            _ => return Err(ArchiveError::UnsupportedEntry),
        }
    }
    if long_name.is_some() || pax.is_some() {
        // An extension header that describes an entry that never came.
        return Err(ArchiveError::Malformed);
    }
    Ok(scan)
}

fn visit_data_entry<R: Read>(
    scan: &mut Scan,
    entry: &mut R,
    path: &str,
    kind: EntryType,
    size: u64,
    limits: &ArchiveLimits,
    io_error: &dyn Fn(io::Error) -> ArchiveError,
) -> Result<(), ArchiveError> {
    let first = path.split('/').next().unwrap_or_default();
    if first.eq_ignore_ascii_case(PUBSPEC_NAME) {
        // Only the exact root file is the manifest. A case variant or something below a path
        // that starts with the manifest name could replace or shadow it after extraction.
        if path != PUBSPEC_NAME || kind != EntryType::Regular {
            return Err(ArchiveError::PubspecInvalid);
        }
        if scan.pubspec.is_some() {
            return Err(ArchiveError::PubspecDuplicate);
        }
        if size > limits.max_pubspec_bytes {
            return Err(ArchiveError::Limit(LimitKind::PubspecBytes));
        }
        let mut bytes = Vec::new();
        entry.read_to_end(&mut bytes).map_err(io_error)?;
        scan.pubspec = Some(bytes);
    }
    Ok(())
}

/// Reads an extension header's data into memory under a hard cap.
fn read_extension<R: Read>(
    entry: &mut R,
    size: u64,
    cap: u64,
    io_error: &dyn Fn(io::Error) -> ArchiveError,
) -> Result<Vec<u8>, ArchiveError> {
    if size > cap {
        return Err(ArchiveError::Malformed);
    }
    let mut bytes = Vec::new();
    entry.read_to_end(&mut bytes).map_err(io_error)?;
    Ok(bytes)
}

/// Reads the records of a PAX block. `path` is honoured; `size` is refused because honouring it
/// is exactly what makes two readers disagree about the next entry; `linkpath` belongs to links,
/// which are refused anyway. Everything else (times, owners) does not matter to the server.
fn parse_pax(block: &[u8]) -> Result<Pax, ArchiveError> {
    let mut pax = Pax::default();
    for record in PaxExtensions::new(block) {
        let record = record.map_err(|_| ArchiveError::Malformed)?;
        match record.key_bytes() {
            b"path" => pax.path = Some(record.value_bytes().to_vec()),
            b"size" => return Err(ArchiveError::UnsupportedEntry),
            _ => {}
        }
    }
    Ok(pax)
}

/// Checks and normalises an entry path. `Ok(None)` is the root directory entry (`./`), which
/// carries no file.
fn entry_path(
    raw: &[u8],
    directory: bool,
    limits: &ArchiveLimits,
) -> Result<Option<String>, ArchiveError> {
    if raw.is_empty() || raw.len() > limits.max_path_bytes {
        return Err(ArchiveError::UnsafePath);
    }
    let text = std::str::from_utf8(raw).map_err(|_| ArchiveError::UnsafePath)?;
    let text = text.strip_prefix("./").unwrap_or(text);
    let text = if directory {
        text.strip_suffix('/').unwrap_or(text)
    } else {
        text
    };
    if directory && (text.is_empty() || text == ".") {
        return Ok(None);
    }
    if text.is_empty() || text.starts_with('/') {
        return Err(ArchiveError::UnsafePath);
    }
    for (index, segment) in text.split('/').enumerate() {
        if !segment_is_safe(segment, index == 0) {
            return Err(ArchiveError::UnsafePath);
        }
    }
    Ok(Some(text.to_owned()))
}

fn segment_is_safe(segment: &str, first: bool) -> bool {
    if segment.is_empty() || segment.len() > MAX_SEGMENT_BYTES {
        return false;
    }
    if segment == "." || segment == ".." {
        return false;
    }
    if segment.chars().any(|c| c == '\\' || c.is_control()) {
        return false;
    }
    // `C:` or `C:foo` as the first segment is an absolute or drive relative path on Windows.
    let mut chars = segment.chars();
    let drive =
        matches!((chars.next(), chars.next()), (Some(a), Some(':')) if a.is_ascii_alphabetic());
    !(first && drive)
}

/// After the end-of-archive marker only zero padding may follow. Reading it also drives the
/// decoder to the end of the gzip member, which is where the CRC is verified.
fn drain_padding<R: Read>(
    reader: &mut R,
    io_error: &dyn Fn(io::Error) -> ArchiveError,
) -> Result<(), ArchiveError> {
    let mut buffer = [0u8; 8192];
    loop {
        let n = reader.read(&mut buffer).map_err(io_error)?;
        let Some(read) = buffer.get(..n).filter(|read| !read.is_empty()) else {
            return Ok(());
        };
        if read.iter().any(|&b| b != 0) {
            return Err(ArchiveError::TrailingData);
        }
    }
}

#[cfg(test)]
mod tests;
