//! Distribution file names: wheels and source distributions.
//!
//! Specifications:
//! * Wheel: <https://packaging.python.org/en/latest/specifications/binary-distribution-format/>
//!   (PEP 427): `{distribution}-{version}(-{build tag})?-{python tag}-{abi tag}-{platform tag}.whl`.
//! * Source distribution: <https://packaging.python.org/en/latest/specifications/source-distribution-format/>:
//!   `{name}-{version}.tar.gz`. The legacy `.zip` form that `setup.py sdist` can still write is
//!   accepted as well, as the legacy upload API accepts it.
//!
//! The parser only accepts file names that this repository can serve safely: ASCII, at most one
//! path segment, no control characters. The name and the version it extracts are validated
//! with [`ProjectName`] and [`Version`], so that a file name which parses always yields the
//! coordinate (normalized name, canonical version) a component is stored under.

use crate::name::ProjectName;
use crate::version::Version;

/// Longest file name accepted: one path segment of the storage layer.
pub const MAX_FILENAME_LEN: usize = 255;

/// Why a file name was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum FilenameError {
    /// Empty or longer than [`MAX_FILENAME_LEN`].
    #[error("file name has an invalid length")]
    Length,
    /// A character outside the allowed set (path separators, control characters, non-ASCII).
    #[error("file name contains a forbidden character")]
    ForbiddenChar,
    /// Neither `.whl`, `.tar.gz` nor `.zip`.
    #[error("unsupported distribution type")]
    UnsupportedType,
    /// The wheel name does not have 5 or 6 dash separated components, or a tag is malformed.
    #[error("malformed wheel file name")]
    MalformedWheel,
    /// The sdist name has no `-version` part.
    #[error("malformed source distribution file name")]
    MalformedSdist,
    /// The project name is not valid.
    #[error("invalid project name in file name")]
    InvalidName,
    /// The version is not a PEP 440 version.
    #[error("invalid version in file name")]
    InvalidVersion,
}

/// What kind of distribution a file is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DistKind {
    /// A wheel (`bdist_wheel` in the legacy upload API).
    Wheel,
    /// A source distribution (`sdist`).
    Sdist,
}

impl DistKind {
    /// The `filetype` value of the legacy upload API.
    #[must_use]
    pub const fn filetype(self) -> &'static str {
        match self {
            Self::Wheel => "bdist_wheel",
            Self::Sdist => "sdist",
        }
    }
}

/// A parsed distribution file name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DistFilename {
    kind: DistKind,
    project: ProjectName,
    version: Version,
    text: String,
    build: Option<String>,
    python_tag: Option<String>,
}

impl DistFilename {
    /// Parses a file name.
    ///
    /// # Errors
    /// [`FilenameError`] describing the first rule violated.
    pub fn parse(text: &str) -> Result<Self, FilenameError> {
        if text.is_empty() || text.len() > MAX_FILENAME_LEN {
            return Err(FilenameError::Length);
        }
        if !text
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-' | b'+'))
        {
            return Err(FilenameError::ForbiddenChar);
        }
        if let Some(stem) = text.strip_suffix(".whl") {
            parse_wheel(text, stem)
        } else if let Some(stem) = text
            .strip_suffix(".tar.gz")
            .or_else(|| text.strip_suffix(".zip"))
        {
            parse_sdist(text, stem)
        } else {
            Err(FilenameError::UnsupportedType)
        }
    }

    /// Wheel or sdist.
    #[must_use]
    pub fn kind(&self) -> DistKind {
        self.kind
    }

    /// The normalized project name.
    #[must_use]
    pub fn project(&self) -> &ProjectName {
        &self.project
    }

    /// The version.
    #[must_use]
    pub fn version(&self) -> &Version {
        &self.version
    }

    /// The file name as parsed.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.text
    }

    /// The python tag of a wheel (`py3`, `cp311`, `py2.py3`), `None` for an sdist.
    #[must_use]
    pub fn python_tag(&self) -> Option<&str> {
        self.python_tag.as_deref()
    }

    /// The build tag of a wheel, if it has one.
    #[must_use]
    pub fn build_tag(&self) -> Option<&str> {
        self.build.as_deref()
    }

    /// The file extension: `whl`, `tar.gz` or `zip`.
    ///
    /// The suffixes are case sensitive on purpose: only the lowercase spellings parse, and
    /// `Path::extension` would see `gz` for `.tar.gz`.
    #[allow(clippy::case_sensitive_file_extension_comparisons)]
    #[must_use]
    pub fn extension(&self) -> &'static str {
        if self.text.ends_with(".whl") {
            "whl"
        } else if self.text.ends_with(".tar.gz") {
            "tar.gz"
        } else {
            "zip"
        }
    }
}

fn parse_wheel(text: &str, stem: &str) -> Result<DistFilename, FilenameError> {
    let parts: Vec<&str> = stem.split('-').collect();
    let (name, version, build, tags) = match parts.as_slice() {
        [name, version, python, abi, platform] => (name, version, None, [python, abi, platform]),
        [name, version, build, python, abi, platform] => {
            // The build tag must start with a digit (it breaks ties between equal versions).
            if !build.bytes().next().is_some_and(|b| b.is_ascii_digit()) {
                return Err(FilenameError::MalformedWheel);
            }
            (
                name,
                version,
                Some((*build).to_owned()),
                [python, abi, platform],
            )
        }
        _ => return Err(FilenameError::MalformedWheel),
    };
    if tags.iter().any(|tag| !is_tag(tag)) || build.as_deref().is_some_and(|b| !is_tag(b)) {
        return Err(FilenameError::MalformedWheel);
    }
    // The escaped name never contains `-`; `.` and capitals are tolerated by consumers.
    if name.contains('+') {
        return Err(FilenameError::InvalidName);
    }
    let project = ProjectName::parse(name).map_err(|_| FilenameError::InvalidName)?;
    let version = Version::parse(version).map_err(|_| FilenameError::InvalidVersion)?;
    let python_tag = Some((*tags[0]).to_owned());
    Ok(DistFilename {
        kind: DistKind::Wheel,
        project,
        version,
        text: text.to_owned(),
        build,
        python_tag,
    })
}

fn is_tag(tag: &str) -> bool {
    !tag.is_empty()
        && tag
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_'))
}

fn parse_sdist(text: &str, stem: &str) -> Result<DistFilename, FilenameError> {
    // The version is what follows the last dash: names may contain dashes in legacy files, a
    // canonical version never does.
    let (name, version) = stem.rsplit_once('-').ok_or(FilenameError::MalformedSdist)?;
    if name.contains('+') {
        return Err(FilenameError::InvalidName);
    }
    let project = ProjectName::parse(name).map_err(|_| FilenameError::InvalidName)?;
    let version = Version::parse(version).map_err(|_| FilenameError::InvalidVersion)?;
    Ok(DistFilename {
        kind: DistKind::Sdist,
        project,
        version,
        text: text.to_owned(),
        build: None,
        python_tag: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn parses_wheels() {
        let wheel = DistFilename::parse("Foo_Bar-1.0.0-py3-none-any.whl").unwrap();
        assert_eq!(wheel.kind(), DistKind::Wheel);
        assert_eq!(wheel.project().as_str(), "foo-bar");
        assert_eq!(wheel.version().to_string(), "1.0.0");
        assert_eq!(wheel.build_tag(), None);
        assert_eq!(wheel.python_tag(), Some("py3"));
        assert_eq!(wheel.extension(), "whl");
        assert_eq!(wheel.kind().filetype(), "bdist_wheel");

        let built = DistFilename::parse(
            "numpy-1.26.4-1-cp311-cp311-manylinux2014_x86_64.manylinux_2_17_x86_64.whl",
        )
        .unwrap();
        assert_eq!(built.build_tag(), Some("1"));
        let local = DistFilename::parse("torch-2.0.0+cpu-cp311-cp311-linux_x86_64.whl").unwrap();
        assert_eq!(local.version().to_string(), "2.0.0+cpu");
    }

    #[test]
    fn parses_sdists() {
        let sdist = DistFilename::parse("foo-bar-1.0.tar.gz").unwrap();
        assert_eq!(sdist.kind(), DistKind::Sdist);
        assert_eq!(sdist.project().as_str(), "foo-bar");
        assert_eq!(sdist.version().to_string(), "1.0");
        assert_eq!(sdist.extension(), "tar.gz");
        assert_eq!(sdist.python_tag(), None);
        assert_eq!(sdist.kind().filetype(), "sdist");
        let legacy = DistFilename::parse("Foo-1.0rc1.zip").unwrap();
        assert_eq!(legacy.extension(), "zip");
        assert_eq!(legacy.version().to_string(), "1.0rc1");
        assert_eq!(legacy.as_str(), "Foo-1.0rc1.zip");
    }

    #[test]
    fn refuses_what_is_not_a_distribution_of_this_repository() {
        for (text, error) in [
            ("", FilenameError::Length),
            ("foo-1.0.egg", FilenameError::UnsupportedType),
            ("foo-1.0.tar.bz2", FilenameError::UnsupportedType),
            ("foo-1.0.exe", FilenameError::UnsupportedType),
            ("foo-1.0.whl", FilenameError::MalformedWheel),
            ("foo-1.0-py3-none.whl", FilenameError::MalformedWheel),
            ("foo-1.0-x-py3-none-any.whl", FilenameError::MalformedWheel),
            ("foo-1.0--py3-none-any.whl", FilenameError::MalformedWheel),
            (
                "foo-1.0-py3-none-any-extra-more.whl",
                FilenameError::MalformedWheel,
            ),
            ("foo-1.0.tar.gz.whl", FilenameError::MalformedWheel),
            ("foo.tar.gz", FilenameError::MalformedSdist),
            ("-1.0.tar.gz", FilenameError::InvalidName),
            ("foo-bar.tar.gz", FilenameError::InvalidVersion),
            (
                "foo-1.0-py3-none-any.whl/../x",
                FilenameError::ForbiddenChar,
            ),
            ("fo o-1.0.tar.gz", FilenameError::ForbiddenChar),
            ("fo\\o-1.0.tar.gz", FilenameError::ForbiddenChar),
            ("fo%2fo-1.0.tar.gz", FilenameError::ForbiddenChar),
            ("f\u{e9}o-1.0.tar.gz", FilenameError::ForbiddenChar),
            ("fo\0o-1.0.tar.gz", FilenameError::ForbiddenChar),
            ("foo+x-1.0.tar.gz", FilenameError::InvalidName),
            ("foo+x-1.0-py3-none-any.whl", FilenameError::InvalidName),
            ("foo-1.0-py3-n+e-any.whl", FilenameError::MalformedWheel),
        ] {
            assert_eq!(DistFilename::parse(text), Err(error), "{text:?}");
        }
        let long = format!("{}-1.0.tar.gz", "a".repeat(MAX_FILENAME_LEN));
        assert_eq!(DistFilename::parse(&long), Err(FilenameError::Length));
    }

    proptest! {
        #[test]
        fn wheel_names_round_trip(
            name in "[A-Za-z][A-Za-z0-9]{0,8}(_[A-Za-z0-9]{1,6}){0,2}",
            major in 0u32..100, minor in 0u32..100,
            tag in "(py2|py3|cp311|py2\\.py3)",
            plat in "(any|linux_x86_64|macosx_10_9_x86_64)",
        ) {
            let text = format!("{name}-{major}.{minor}-{tag}-none-{plat}.whl");
            let parsed = DistFilename::parse(&text).unwrap();
            prop_assert_eq!(parsed.as_str(), text.as_str());
            prop_assert_eq!(parsed.version().to_string(), format!("{major}.{minor}"));
            prop_assert_eq!(parsed.project().as_str(), crate::name::normalize(&name));
            // Reparsing the name part by its normalized spelling gives the same project.
            let again = format!("{}-{major}.{minor}-{tag}-none-{plat}.whl", parsed.project().wheel_escaped());
            let reparsed = DistFilename::parse(&again).unwrap();
            prop_assert_eq!(reparsed.project(), parsed.project());
        }

        #[test]
        fn sdist_names_round_trip(
            name in "[a-z][a-z0-9]{0,8}(-[a-z0-9]{1,6}){0,2}",
            major in 0u32..100, minor in 0u32..100,
            ext in "(tar\\.gz|zip)",
        ) {
            let text = format!("{name}-{major}.{minor}.{ext}");
            let parsed = DistFilename::parse(&text).unwrap();
            prop_assert_eq!(parsed.project().as_str(), name.as_str());
            prop_assert_eq!(parsed.version().to_string(), format!("{major}.{minor}"));
            prop_assert_eq!(parsed.extension(), ext.as_str());
        }

        #[test]
        fn parse_never_panics(text in "\\PC{0,300}") {
            let _ = DistFilename::parse(&text);
        }

        #[test]
        fn parse_never_panics_on_filename_like_text(text in "[A-Za-z0-9._+-]{0,80}(\\.whl|\\.tar\\.gz|\\.zip)?") {
            let _ = DistFilename::parse(&text);
        }
    }
}
