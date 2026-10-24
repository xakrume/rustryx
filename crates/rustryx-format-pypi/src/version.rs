//! PEP 440 versions: parsing with normalization, canonical text and ordering.
//!
//! Specification: <https://packaging.python.org/en/latest/specifications/version-specifiers/>
//! (PEP 440). The accepted spellings and the ordering follow the "Normalization" and "Version
//! ordering" sections; nothing here is copied from another implementation.
//!
//! The parser is strict in one place where the reference regular expression is lenient: a
//! separator is only consumed when the token it separates follows (`1.0a-` is refused, `1.0a-1`
//! is `1.0a1`). Every input is bounded (length, number of segments, size of a number), and the
//! parser never indexes a slice, so arbitrary bytes cannot make it panic.

use std::cmp::Ordering;
use std::fmt;

/// Longest version text accepted.
pub const MAX_VERSION_LEN: usize = 128;
/// Most release segments (`1.2.3` has three).
const MAX_RELEASE_SEGMENTS: usize = 32;
/// Most segments of the local label.
const MAX_LOCAL_SEGMENTS: usize = 32;

/// Why a version was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum VersionError {
    /// Empty text.
    #[error("empty version")]
    Empty,
    /// Longer than [`MAX_VERSION_LEN`], or more segments than allowed.
    #[error("version too long")]
    TooLong,
    /// Not a PEP 440 version.
    #[error("not a valid PEP 440 version")]
    Invalid,
    /// A number does not fit in 64 bits.
    #[error("version number too large")]
    NumberTooLarge,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum PreKind {
    Alpha,
    Beta,
    Rc,
}

impl PreKind {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Alpha => "a",
            Self::Beta => "b",
            Self::Rc => "rc",
        }
    }
}

/// A run of ASCII digits without leading zeros, compared as a number of any size.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Digits(String);

impl Digits {
    fn new(text: &str) -> Self {
        let trimmed = text.trim_start_matches('0');
        Self(if trimmed.is_empty() { "0" } else { trimmed }.to_owned())
    }
}

impl Ord for Digits {
    fn cmp(&self, other: &Self) -> Ordering {
        self.0
            .len()
            .cmp(&other.0.len())
            .then_with(|| self.0.cmp(&other.0))
    }
}

impl PartialOrd for Digits {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// One dot separated piece of the local label. A numeric piece sorts above a textual one
/// (variant order), textual pieces compare lexicographically.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
enum LocalSegment {
    Text(String),
    Number(Digits),
}

impl fmt::Display for LocalSegment {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Text(text) | Self::Number(Digits(text)) => f.write_str(text),
        }
    }
}

/// A parsed PEP 440 version. `Display` prints the canonical form, which parses back to an equal
/// value.
#[derive(Debug, Clone)]
pub struct Version {
    epoch: u64,
    release: Vec<u64>,
    pre: Option<(PreKind, u64)>,
    post: Option<u64>,
    dev: Option<u64>,
    local: Vec<LocalSegment>,
}

/// Sort key of the pre-release slot: a development release of a final release sorts before every
/// pre-release of it, a final release after them.
#[derive(Debug, PartialEq, Eq, PartialOrd, Ord)]
enum PreKey {
    DevOnly,
    Pre(PreKind, u64),
    Final,
}

#[derive(Debug, PartialEq, Eq, PartialOrd, Ord)]
enum DevKey {
    Dev(u64),
    None,
}

impl Version {
    /// Parses and normalizes `raw` (surrounding whitespace, a leading `v` and any case are
    /// accepted).
    ///
    /// # Errors
    /// [`VersionError`] when `raw` is not a PEP 440 version or exceeds a bound.
    pub fn parse(raw: &str) -> Result<Self, VersionError> {
        let trimmed = raw.trim_matches(|c: char| c.is_ascii_whitespace());
        if trimmed.is_empty() {
            return Err(VersionError::Empty);
        }
        if trimmed.len() > MAX_VERSION_LEN {
            return Err(VersionError::TooLong);
        }
        if !trimmed.is_ascii() {
            return Err(VersionError::Invalid);
        }
        let lowered = trimmed.to_ascii_lowercase();
        let mut cursor = Cursor::new(lowered.as_bytes());
        let version = parse_version(&mut cursor)?;
        if cursor.rest().is_empty() {
            Ok(version)
        } else {
            Err(VersionError::Invalid)
        }
    }

    /// True for a pre-release or a development release (pip skips those unless asked).
    #[must_use]
    pub fn is_prerelease(&self) -> bool {
        self.pre.is_some() || self.dev.is_some()
    }

    fn trimmed_release(&self) -> &[u64] {
        let end = self
            .release
            .iter()
            .rposition(|n| *n != 0)
            .map_or(0, |last| last + 1);
        self.release.get(..end).unwrap_or(&[])
    }

    fn pre_key(&self) -> PreKey {
        match (self.pre, self.post, self.dev) {
            (Some((kind, n)), _, _) => PreKey::Pre(kind, n),
            (None, None, Some(_)) => PreKey::DevOnly,
            (None, _, _) => PreKey::Final,
        }
    }

    fn dev_key(&self) -> DevKey {
        self.dev.map_or(DevKey::None, DevKey::Dev)
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.epoch != 0 {
            write!(f, "{}!", self.epoch)?;
        }
        let mut first = true;
        for segment in &self.release {
            if !first {
                f.write_str(".")?;
            }
            first = false;
            write!(f, "{segment}")?;
        }
        if let Some((kind, n)) = self.pre {
            write!(f, "{}{n}", kind.as_str())?;
        }
        if let Some(n) = self.post {
            write!(f, ".post{n}")?;
        }
        if let Some(n) = self.dev {
            write!(f, ".dev{n}")?;
        }
        if !self.local.is_empty() {
            f.write_str("+")?;
            for (i, segment) in self.local.iter().enumerate() {
                if i > 0 {
                    f.write_str(".")?;
                }
                write!(f, "{segment}")?;
            }
        }
        Ok(())
    }
}

impl Ord for Version {
    fn cmp(&self, other: &Self) -> Ordering {
        self.epoch
            .cmp(&other.epoch)
            .then_with(|| self.trimmed_release().cmp(other.trimmed_release()))
            .then_with(|| self.pre_key().cmp(&other.pre_key()))
            // No post release sorts before any post release.
            .then_with(|| self.post.cmp(&other.post))
            .then_with(|| self.dev_key().cmp(&other.dev_key()))
            // No local label sorts before any local label; a label that is a prefix of another
            // sorts before it (slice comparison).
            .then_with(|| self.local.cmp(&other.local))
    }
}

impl PartialOrd for Version {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl PartialEq for Version {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}

impl Eq for Version {}

/// Canonical text of a version, or the error when it is not one.
///
/// # Errors
/// See [`Version::parse`].
pub fn normalize_version(raw: &str) -> Result<String, VersionError> {
    Version::parse(raw).map(|version| version.to_string())
}

struct Cursor<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> Cursor<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, at: 0 }
    }

    fn rest(&self) -> &'a [u8] {
        self.bytes.get(self.at..).unwrap_or(&[])
    }

    fn peek_at(&self, offset: usize) -> Option<u8> {
        self.bytes.get(self.at.checked_add(offset)?).copied()
    }

    fn eat_if(&mut self, wanted: u8) -> bool {
        if self.peek_at(0) == Some(wanted) {
            self.at += 1;
            true
        } else {
            false
        }
    }

    fn eat_literal(&mut self, literal: &str) -> bool {
        if self.rest().starts_with(literal.as_bytes()) {
            self.at += literal.len();
            true
        } else {
            false
        }
    }

    fn next_is_digit(&self, offset: usize) -> bool {
        self.peek_at(offset).is_some_and(|b| b.is_ascii_digit())
    }

    /// Consumes one or more digits and returns them as text.
    fn digits(&mut self) -> Option<&'a str> {
        let rest = self.rest();
        let len = rest.iter().take_while(|b| b.is_ascii_digit()).count();
        if len == 0 {
            return None;
        }
        let digits = rest.get(..len)?;
        self.at += len;
        std::str::from_utf8(digits).ok()
    }

    /// Consumes a separator (`.`, `-`, `_`) and reports whether one was there.
    fn separator(&mut self) -> bool {
        match self.peek_at(0) {
            Some(b'.' | b'-' | b'_') => {
                self.at += 1;
                true
            }
            _ => false,
        }
    }
}

fn parse_number(text: &str) -> Result<u64, VersionError> {
    text.parse::<u64>()
        .map_err(|_| VersionError::NumberTooLarge)
}

fn parse_version(cursor: &mut Cursor<'_>) -> Result<Version, VersionError> {
    cursor.eat_if(b'v');
    let epoch = parse_epoch(cursor)?;
    let release = parse_release(cursor)?;
    let pre = parse_pre(cursor)?;
    let post = parse_post(cursor)?;
    let dev = parse_dev(cursor)?;
    let local = parse_local(cursor)?;
    Ok(Version {
        epoch,
        release,
        pre,
        post,
        dev,
        local,
    })
}

fn parse_epoch(cursor: &mut Cursor<'_>) -> Result<u64, VersionError> {
    let saved = cursor.at;
    if let Some(digits) = cursor.digits() {
        if cursor.eat_if(b'!') {
            return parse_number(digits);
        }
        cursor.at = saved;
    }
    Ok(0)
}

fn parse_release(cursor: &mut Cursor<'_>) -> Result<Vec<u64>, VersionError> {
    let first = cursor.digits().ok_or(VersionError::Invalid)?;
    let mut release = vec![parse_number(first)?];
    while cursor.peek_at(0) == Some(b'.') && cursor.next_is_digit(1) {
        cursor.at += 1;
        let digits = cursor.digits().ok_or(VersionError::Invalid)?;
        release.push(parse_number(digits)?);
        if release.len() > MAX_RELEASE_SEGMENTS {
            return Err(VersionError::TooLong);
        }
    }
    Ok(release)
}

/// The optional number after a label: a separator is consumed only when a digit follows it.
fn parse_label_number(cursor: &mut Cursor<'_>) -> Result<u64, VersionError> {
    let saved = cursor.at;
    cursor.separator();
    if let Some(digits) = cursor.digits() {
        parse_number(digits)
    } else {
        cursor.at = saved;
        Ok(0)
    }
}

fn parse_pre(cursor: &mut Cursor<'_>) -> Result<Option<(PreKind, u64)>, VersionError> {
    let saved = cursor.at;
    cursor.separator();
    // Spelling order of the grammar: alpha|a|beta|b|preview|pre|c|rc.
    let kind = if cursor.eat_literal("alpha") || cursor.eat_literal("a") {
        PreKind::Alpha
    } else if cursor.eat_literal("beta") || cursor.eat_literal("b") {
        PreKind::Beta
    } else if cursor.eat_literal("preview")
        || cursor.eat_literal("pre")
        || cursor.eat_literal("rc")
        || cursor.eat_literal("c")
    {
        PreKind::Rc
    } else {
        cursor.at = saved;
        return Ok(None);
    };
    Ok(Some((kind, parse_label_number(cursor)?)))
}

fn parse_post(cursor: &mut Cursor<'_>) -> Result<Option<u64>, VersionError> {
    // Implicit post release: `1.0-1`.
    if cursor.peek_at(0) == Some(b'-') && cursor.next_is_digit(1) {
        cursor.at += 1;
        let digits = cursor.digits().ok_or(VersionError::Invalid)?;
        return parse_number(digits).map(Some);
    }
    let saved = cursor.at;
    cursor.separator();
    if cursor.eat_literal("post") || cursor.eat_literal("rev") || cursor.eat_literal("r") {
        return Ok(Some(parse_label_number(cursor)?));
    }
    cursor.at = saved;
    Ok(None)
}

fn parse_dev(cursor: &mut Cursor<'_>) -> Result<Option<u64>, VersionError> {
    let saved = cursor.at;
    cursor.separator();
    if cursor.eat_literal("dev") {
        return Ok(Some(parse_label_number(cursor)?));
    }
    cursor.at = saved;
    Ok(None)
}

fn parse_local(cursor: &mut Cursor<'_>) -> Result<Vec<LocalSegment>, VersionError> {
    if !cursor.eat_if(b'+') {
        return Ok(Vec::new());
    }
    let mut segments = Vec::new();
    loop {
        let rest = cursor.rest();
        let len = rest
            .iter()
            .take_while(|b| b.is_ascii_alphanumeric())
            .count();
        let text = rest
            .get(..len)
            .and_then(|bytes| std::str::from_utf8(bytes).ok())
            .filter(|text| !text.is_empty())
            .ok_or(VersionError::Invalid)?;
        cursor.at += len;
        segments.push(if text.bytes().all(|b| b.is_ascii_digit()) {
            LocalSegment::Number(Digits::new(text))
        } else {
            LocalSegment::Text(text.to_owned())
        });
        if segments.len() > MAX_LOCAL_SEGMENTS {
            return Err(VersionError::TooLong);
        }
        // A separator belongs to the label only when another segment follows it.
        let continues = matches!(cursor.peek_at(0), Some(b'.' | b'-' | b'_'))
            && cursor.peek_at(1).is_some_and(|b| b.is_ascii_alphanumeric());
        if !continues {
            break;
        }
        cursor.at += 1;
    }
    Ok(segments)
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn v(text: &str) -> Version {
        Version::parse(text).unwrap()
    }

    #[test]
    fn normalizes_the_spellings_of_the_specification() {
        for (raw, canonical) in [
            ("1.1RC1", "1.1rc1"),
            ("v1.0", "1.0"),
            ("  1.0  ", "1.0"),
            ("1.0.0", "1.0.0"),
            ("00.09000", "0.9000"),
            ("1.2a", "1.2a0"),
            ("1.2.alpha.3", "1.2a3"),
            ("1.2-beta-4", "1.2b4"),
            ("1.2_c_5", "1.2rc5"),
            ("1.2pre6", "1.2rc6"),
            ("1.2preview", "1.2rc0"),
            ("1.2.post", "1.2.post0"),
            ("1.2-r7", "1.2.post7"),
            ("1.2rev8", "1.2.post8"),
            ("1.0-1", "1.0.post1"),
            ("1.2.dev", "1.2.dev0"),
            ("1.2-dev3", "1.2.dev3"),
            ("1.2_DEV_3", "1.2.dev3"),
            ("1.0+ubuntu-1", "1.0+ubuntu.1"),
            ("1.0+UBUNTU_01", "1.0+ubuntu.1"),
            ("0!1.0", "1.0"),
            ("2!1.0", "2!1.0"),
            ("1.0a1.post2.dev3+x.7", "1.0a1.post2.dev3+x.7"),
            ("1.0.a-1", "1.0a1"),
        ] {
            assert_eq!(normalize_version(raw).unwrap(), canonical, "{raw}");
        }
    }

    #[test]
    fn refuses_what_is_not_a_version() {
        for raw in [
            "",
            "   ",
            "abc",
            "1.",
            "1..0",
            ".1",
            "1.0-",
            "1.0a-",
            "1.0+",
            "1.0+a..b",
            "1.0+a-",
            "1.0 1",
            "1.0.post1.post2",
            "1.0.dev1.dev2",
            "1.0!1",
            "!1.0",
            "1.0\u{e9}",
            "1.0/../..",
            "1.0\0",
        ] {
            assert!(Version::parse(raw).is_err(), "{raw:?} must be refused");
        }
        assert_eq!(
            Version::parse(&"1".repeat(MAX_VERSION_LEN + 1)),
            Err(VersionError::TooLong)
        );
        assert_eq!(
            Version::parse("99999999999999999999999"),
            Err(VersionError::NumberTooLarge)
        );
        let many = vec!["1"; MAX_RELEASE_SEGMENTS + 2].join(".");
        assert_eq!(Version::parse(&many), Err(VersionError::TooLong));
    }

    #[test]
    fn orders_like_the_specification_example() {
        let ascending = [
            "1.0.dev456",
            "1.0a1",
            "1.0a2.dev456",
            "1.0a12.dev456",
            "1.0a12",
            "1.0b1.dev456",
            "1.0b2",
            "1.0b2.post345.dev456",
            "1.0b2.post345",
            "1.0rc1.dev456",
            "1.0rc1",
            "1.0",
            "1.0+abc.5",
            "1.0+abc.7",
            "1.0+5",
            "1.0.post456.dev34",
            "1.0.post456",
            "1.0.15",
            "1.1.dev1",
        ];
        for pair in ascending.windows(2) {
            let (low, high) = (v(pair[0]), v(pair[1]));
            assert!(low < high, "{} must sort before {}", pair[0], pair[1]);
            assert!(high > low);
        }
    }

    #[test]
    fn trailing_zeros_and_epochs() {
        assert_eq!(v("1.0"), v("1"));
        assert_eq!(v("1.0.0"), v("1.0"));
        assert!(v("1!0.1") > v("99.0"));
        assert!(v("1.0") < v("1.0.1"));
        assert_eq!(v("0"), v("0.0.0"));
    }

    #[test]
    fn local_labels_compare_by_segment() {
        assert!(v("1.0+abc") < v("1.0+abc.1"));
        assert!(v("1.0+abc") < v("1.0+1"));
        assert!(v("1.0+9") < v("1.0+10"));
        assert_eq!(v("1.0+Abc"), v("1.0+abc"));
        assert!(v("1.0") < v("1.0+a"));
        assert!(v("1.0+123456789012345678901234567890") < v("1.0+123456789012345678901234567891"));
    }

    #[test]
    fn prerelease_detection() {
        assert!(v("1.0a1").is_prerelease());
        assert!(v("1.0.dev1").is_prerelease());
        assert!(v("1.0rc1").is_prerelease());
        assert!(!v("1.0").is_prerelease());
        assert!(!v("1.0.post1").is_prerelease());
    }

    #[test]
    fn equal_values_are_not_ordered_apart() {
        assert_eq!(v("1.0").cmp(&v("1.0.0")), Ordering::Equal);
        assert_eq!(v("1.0").partial_cmp(&v("1.0.1")), Some(Ordering::Less));
    }

    fn arb_canonical() -> impl Strategy<Value = String> {
        (
            proptest::option::of(1u64..5),
            proptest::collection::vec(0u64..1000, 1..5),
            proptest::option::of((0usize..3, 0u64..50)),
            proptest::option::of(0u64..50),
            proptest::option::of(0u64..50),
            proptest::option::of(proptest::collection::vec("[a-z0-9]{1,6}", 1..4)),
        )
            .prop_map(|(epoch, release, pre, post, dev, local)| {
                let mut text = String::new();
                if let Some(epoch) = epoch {
                    text.push_str(&format!("{epoch}!"));
                }
                let release: Vec<String> = release.iter().map(u64::to_string).collect();
                text.push_str(&release.join("."));
                if let Some((kind, n)) = pre {
                    text.push_str(&format!("{}{n}", ["a", "b", "rc"][kind]));
                }
                if let Some(n) = post {
                    text.push_str(&format!(".post{n}"));
                }
                if let Some(n) = dev {
                    text.push_str(&format!(".dev{n}"));
                }
                if let Some(local) = local {
                    text.push('+');
                    text.push_str(&local.join("."));
                }
                text
            })
    }

    proptest! {
        #[test]
        fn canonical_text_round_trips(text in arb_canonical()) {
            let parsed = Version::parse(&text).unwrap();
            // Leading zeros of a local number are dropped, so compare after one more pass.
            let canonical = parsed.to_string();
            prop_assert_eq!(Version::parse(&canonical).unwrap().to_string(), canonical.clone());
            prop_assert_eq!(Version::parse(&canonical).unwrap(), parsed);
        }

        #[test]
        fn normalization_is_idempotent(
            text in arb_canonical(),
            upper in any::<bool>(),
            sep in prop::sample::select(vec!["", ".", "-", "_"]),
        ) {
            let mut spelled = text.replace("rc", &format!("{sep}c")).replace(".post", &format!("{sep}rev")).replace(".dev", &format!("{sep}dev"));
            if upper {
                spelled = spelled.to_ascii_uppercase();
            }
            if let Ok(once) = normalize_version(&spelled) {
                prop_assert_eq!(normalize_version(&once).unwrap(), once);
            }
        }

        #[test]
        fn ordering_is_total_and_consistent(a in arb_canonical(), b in arb_canonical(), c in arb_canonical()) {
            let (a, b, c) = (v(&a), v(&b), v(&c));
            prop_assert_eq!(a.cmp(&b), b.cmp(&a).reverse());
            if a <= b && b <= c {
                prop_assert!(a <= c);
            }
            prop_assert_eq!(a.cmp(&a), Ordering::Equal);
        }

        #[test]
        fn parse_never_panics(raw in "\\PC{0,200}") {
            let _ = Version::parse(&raw);
        }

        #[test]
        fn parse_never_panics_on_version_like_text(raw in "[0-9a-zA-Z.!+_ -]{0,60}") {
            let _ = Version::parse(&raw);
        }
    }
}
