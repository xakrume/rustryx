//! Strict, panic free parser of `maven-metadata.xml`.

use quick_xml::Reader;
use quick_xml::events::Event;
use rustryx_format_core::FormatError;

use super::{
    MAX_PLUGINS, MAX_SNAPSHOT_VERSIONS, MAX_VERSIONS, Metadata, Plugin, Snapshot, SnapshotVersion,
    Versioning,
};

/// Deepest element nesting accepted. The model is five levels deep; the slack is for
/// unknown elements that are skipped.
const MAX_DEPTH: usize = 12;
/// Longest text of one element. A version, a name or a timestamp is far shorter.
const MAX_TEXT: usize = 4096;
/// Longest token (version, prefix, artifactId, classifier, extension).
const MAX_TOKEN: usize = 256;

/// Why a document was refused. Rendered to the client by [`From<MetadataError> for FormatError`].
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum MetadataError {
    /// The bytes are not UTF-8.
    #[error("the document is not UTF-8")]
    NotUtf8,
    /// More bytes than the cap.
    #[error("the document is larger than {limit} bytes")]
    TooLarge {
        /// The cap that was exceeded.
        limit: u64,
    },
    /// Not well formed XML.
    #[error("the document is not well formed XML")]
    Syntax,
    /// A DOCTYPE declaration. Refused: it is how entities are defined.
    #[error("a document type declaration is not allowed")]
    DtdRefused,
    /// An entity other than the five predefined ones and numeric references.
    #[error("an entity reference is not allowed")]
    UnknownEntity,
    /// The root element is not `metadata`, or there is none.
    #[error("the root element is not metadata")]
    BadRoot,
    /// Elements nested deeper than the limit.
    #[error("the document is nested too deeply")]
    TooDeep,
    /// More entries than the limit.
    #[error("too many {0} entries")]
    TooMany(&'static str),
    /// The text of one element is longer than the limit.
    #[error("an element text is too long")]
    TextTooLong,
    /// A value that is not valid for its element.
    #[error("invalid value for {0}")]
    BadValue(&'static str),
}

impl From<MetadataError> for FormatError {
    fn from(error: MetadataError) -> Self {
        match error {
            MetadataError::TooLarge { limit } => Self::PayloadTooLarge { limit },
            other => Self::unprocessable(format!("maven-metadata.xml is not acceptable: {other}")),
        }
    }
}

fn is_token(text: &str) -> bool {
    !text.is_empty()
        && text.len() <= MAX_TOKEN
        && text.chars().all(|c| {
            !c.is_control()
                && !c.is_whitespace()
                && !matches!(c, '/' | '\\' | '<' | '>' | '&' | '"' | '\'')
        })
}

fn token(text: &str, what: &'static str) -> Result<String, MetadataError> {
    if is_token(text) {
        Ok(text.to_owned())
    } else {
        Err(MetadataError::BadValue(what))
    }
}

/// An empty element (`<release></release>`) is an absent value.
fn optional_token(text: &str, what: &'static str) -> Result<Option<String>, MetadataError> {
    if text.is_empty() {
        Ok(None)
    } else {
        token(text, what).map(Some)
    }
}

fn compact_time(text: &str, what: &'static str) -> Result<Option<String>, MetadataError> {
    if text.is_empty() {
        return Ok(None);
    }
    if text.len() == 14 && text.bytes().all(|b| b.is_ascii_digit()) {
        Ok(Some(text.to_owned()))
    } else {
        Err(MetadataError::BadValue(what))
    }
}

fn snapshot_timestamp(text: &str) -> Result<Option<String>, MetadataError> {
    if text.is_empty() {
        return Ok(None);
    }
    let bytes = text.as_bytes();
    let shape_ok = bytes.len() == 15
        && bytes.get(8) == Some(&b'.')
        && bytes
            .iter()
            .enumerate()
            .all(|(i, b)| i == 8 || b.is_ascii_digit());
    if shape_ok {
        Ok(Some(text.to_owned()))
    } else {
        Err(MetadataError::BadValue("timestamp"))
    }
}

fn plain_text(text: &str, what: &'static str) -> Result<Option<String>, MetadataError> {
    if text.is_empty() {
        return Ok(None);
    }
    if text.chars().any(char::is_control) {
        return Err(MetadataError::BadValue(what));
    }
    Ok(Some(text.to_owned()))
}

/// Parses a document.
///
/// Unknown elements are skipped (the model grows), unknown or malformed *known* values are
/// errors. Repeated singular elements keep the last one, as the reference reader does.
///
/// # Errors
/// A [`MetadataError`]; the document is refused as a whole.
pub fn parse_metadata(bytes: &[u8]) -> Result<Metadata, MetadataError> {
    let text = std::str::from_utf8(bytes).map_err(|_| MetadataError::NotUtf8)?;
    let mut reader = Reader::from_str(text);
    let mut state = State::default();
    loop {
        let event = reader.read_event().map_err(|_| MetadataError::Syntax)?;
        match event {
            Event::Eof => break,
            Event::DocType(_) => return Err(MetadataError::DtdRefused),
            Event::Start(start) => {
                let name = element_name(start.local_name().as_ref())?;
                state.open(name)?;
            }
            Event::Empty(start) => {
                let name = element_name(start.local_name().as_ref())?;
                state.open(name)?;
                state.close()?;
            }
            Event::End(_) => state.close()?,
            Event::Text(content) => {
                let decoded = content.decode().map_err(|_| MetadataError::Syntax)?;
                state.push_text(&decoded)?;
            }
            Event::CData(content) => {
                let bytes = content.into_inner();
                let decoded = std::str::from_utf8(&bytes).map_err(|_| MetadataError::NotUtf8)?;
                state.push_text(decoded)?;
            }
            Event::GeneralRef(reference) => {
                let name = reference.decode().map_err(|_| MetadataError::Syntax)?;
                let resolved = match name.as_ref() {
                    "lt" => '<',
                    "gt" => '>',
                    "amp" => '&',
                    "quot" => '"',
                    "apos" => '\'',
                    _ => reference
                        .resolve_char_ref()
                        .map_err(|_| MetadataError::UnknownEntity)?
                        .ok_or(MetadataError::UnknownEntity)?,
                };
                let mut buffer = [0u8; 4];
                state.push_text(resolved.encode_utf8(&mut buffer))?;
            }
            Event::Decl(_) | Event::PI(_) | Event::Comment(_) => {}
        }
    }
    state.finish()
}

fn element_name(raw: &[u8]) -> Result<String, MetadataError> {
    std::str::from_utf8(raw)
        .map(str::to_owned)
        .map_err(|_| MetadataError::Syntax)
}

#[derive(Default)]
struct State {
    path: Vec<String>,
    text: String,
    root_closed: bool,
    document: Metadata,
    versioning: Versioning,
    has_versioning: bool,
    snapshot: Snapshot,
    has_snapshot: bool,
    snapshot_version: SnapshotVersion,
    plugin: Plugin,
}

impl State {
    fn open(&mut self, name: String) -> Result<(), MetadataError> {
        if self.path.is_empty() && (name != "metadata" || self.root_closed) {
            return Err(MetadataError::BadRoot);
        }
        if self.path.len() >= MAX_DEPTH {
            return Err(MetadataError::TooDeep);
        }
        self.path.push(name);
        self.text.clear();
        let rel: Vec<&str> = self.path.iter().skip(1).map(String::as_str).collect();
        match rel.as_slice() {
            ["versioning"] => self.has_versioning = true,
            ["versioning", "snapshot"] => self.has_snapshot = true,
            ["versioning", "snapshotVersions", "snapshotVersion"] => {
                if self.versioning.snapshot_versions.len() >= MAX_SNAPSHOT_VERSIONS {
                    return Err(MetadataError::TooMany("snapshotVersion"));
                }
                self.snapshot_version = SnapshotVersion::default();
            }
            ["plugins", "plugin"] => {
                if self.document.plugins.len() >= MAX_PLUGINS {
                    return Err(MetadataError::TooMany("plugin"));
                }
                self.plugin = Plugin::default();
            }
            _ => {}
        }
        Ok(())
    }

    fn push_text(&mut self, piece: &str) -> Result<(), MetadataError> {
        if self.path.is_empty() {
            // Outside the root only whitespace is well formed; the reader reports the rest.
            return if piece.trim().is_empty() {
                Ok(())
            } else {
                Err(MetadataError::Syntax)
            };
        }
        if self.text.len() + piece.len() > MAX_TEXT {
            return Err(MetadataError::TextTooLong);
        }
        self.text.push_str(piece);
        Ok(())
    }

    fn close(&mut self) -> Result<(), MetadataError> {
        let text = std::mem::take(&mut self.text);
        let value = text.trim();
        // Borrows only `self.path`, so the arms may assign the other fields.
        let rel: Vec<&str> = self.path.iter().skip(1).map(String::as_str).collect();
        match rel.as_slice() {
            ["groupId"] => self.document.group_id = optional_token(value, "groupId")?,
            ["artifactId"] => self.document.artifact_id = optional_token(value, "artifactId")?,
            ["version"] => self.document.version = optional_token(value, "version")?,
            ["versioning", "latest"] => self.versioning.latest = optional_token(value, "latest")?,
            ["versioning", "release"] => {
                self.versioning.release = optional_token(value, "release")?;
            }
            ["versioning", "lastUpdated"] => {
                self.versioning.last_updated = compact_time(value, "lastUpdated")?;
            }
            ["versioning", "versions", "version"] => {
                if self.versioning.versions.len() >= MAX_VERSIONS {
                    return Err(MetadataError::TooMany("version"));
                }
                self.versioning.versions.push(token(value, "version")?);
            }
            ["versioning", "snapshot", "timestamp"] => {
                self.snapshot.timestamp = snapshot_timestamp(value)?;
            }
            ["versioning", "snapshot", "buildNumber"] => {
                self.snapshot.build_number = if value.is_empty() {
                    None
                } else {
                    Some(
                        value
                            .parse()
                            .map_err(|_| MetadataError::BadValue("buildNumber"))?,
                    )
                };
            }
            ["versioning", "snapshot"] => {
                self.versioning.snapshot = Some(std::mem::take(&mut self.snapshot));
            }
            [
                "versioning",
                "snapshotVersions",
                "snapshotVersion",
                "classifier",
            ] => {
                self.snapshot_version.classifier = optional_token(value, "classifier")?;
            }
            [
                "versioning",
                "snapshotVersions",
                "snapshotVersion",
                "extension",
            ] => {
                self.snapshot_version.extension = token(value, "extension")?;
            }
            ["versioning", "snapshotVersions", "snapshotVersion", "value"] => {
                self.snapshot_version.value = token(value, "value")?;
            }
            [
                "versioning",
                "snapshotVersions",
                "snapshotVersion",
                "updated",
            ] => {
                self.snapshot_version.updated = compact_time(value, "updated")?;
            }
            ["versioning", "snapshotVersions", "snapshotVersion"] => {
                let entry = std::mem::take(&mut self.snapshot_version);
                // An entry without an extension or a value cannot name a file.
                if entry.extension.is_empty() || entry.value.is_empty() {
                    return Err(MetadataError::BadValue("snapshotVersion"));
                }
                self.versioning.snapshot_versions.push(entry);
            }
            ["plugins", "plugin", "name"] => self.plugin.name = plain_text(value, "name")?,
            ["plugins", "plugin", "prefix"] => self.plugin.prefix = token(value, "prefix")?,
            ["plugins", "plugin", "artifactId"] => {
                self.plugin.artifact_id = token(value, "artifactId")?;
            }
            ["plugins", "plugin"] => {
                let entry = std::mem::take(&mut self.plugin);
                if entry.prefix.is_empty() || entry.artifact_id.is_empty() {
                    return Err(MetadataError::BadValue("plugin"));
                }
                self.document.plugins.push(entry);
            }
            _ => {}
        }
        self.path.pop();
        if self.path.is_empty() {
            self.root_closed = true;
        }
        Ok(())
    }

    fn finish(mut self) -> Result<Metadata, MetadataError> {
        if !self.root_closed || !self.path.is_empty() {
            return Err(MetadataError::BadRoot);
        }
        if self.has_versioning {
            if self.has_snapshot && self.versioning.snapshot.is_none() {
                self.versioning.snapshot = Some(Snapshot::default());
            }
            self.document.versioning = Some(self.versioning);
        }
        Ok(self.document)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fmt::Write as _;

    const FULL: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<metadata modelVersion="1.1.0">
  <groupId>org.acme</groupId>
  <artifactId>lib</artifactId>
  <version>1.0-SNAPSHOT</version>
  <versioning>
    <latest>1.1</latest>
    <release>1.0</release>
    <snapshot>
      <timestamp>20240102.030405</timestamp>
      <buildNumber>3</buildNumber>
    </snapshot>
    <versions>
      <version>1.0</version>
      <version>1.1</version>
    </versions>
    <lastUpdated>20240102030405</lastUpdated>
    <snapshotVersions>
      <snapshotVersion>
        <classifier>sources</classifier>
        <extension>jar</extension>
        <value>1.0-20240102.030405-3</value>
        <updated>20240102030405</updated>
      </snapshotVersion>
      <snapshotVersion>
        <extension>pom</extension>
        <value>1.0-20240102.030405-3</value>
      </snapshotVersion>
    </snapshotVersions>
  </versioning>
  <plugins>
    <plugin>
      <name>Acme &amp; Co Plugin</name>
      <prefix>acme</prefix>
      <artifactId>acme-maven-plugin</artifactId>
    </plugin>
  </plugins>
</metadata>"#;

    #[test]
    fn parses_every_element_of_the_model() {
        let m = parse_metadata(FULL.as_bytes()).unwrap();
        assert_eq!(m.group_id.as_deref(), Some("org.acme"));
        assert_eq!(m.artifact_id.as_deref(), Some("lib"));
        assert_eq!(m.version.as_deref(), Some("1.0-SNAPSHOT"));
        let v = m.versioning.unwrap();
        assert_eq!(v.latest.as_deref(), Some("1.1"));
        assert_eq!(v.release.as_deref(), Some("1.0"));
        assert_eq!(v.versions, ["1.0", "1.1"]);
        assert_eq!(v.last_updated.as_deref(), Some("20240102030405"));
        let snapshot = v.snapshot.unwrap();
        assert_eq!(snapshot.timestamp.as_deref(), Some("20240102.030405"));
        assert_eq!(snapshot.build_number, Some(3));
        assert_eq!(v.snapshot_versions.len(), 2);
        assert_eq!(
            v.snapshot_versions[0].classifier.as_deref(),
            Some("sources")
        );
        assert_eq!(v.snapshot_versions[1].classifier, None);
        assert_eq!(m.plugins[0].name.as_deref(), Some("Acme & Co Plugin"));
        assert_eq!(m.plugins[0].prefix, "acme");
    }

    #[test]
    fn tolerates_unknown_elements_comments_cdata_and_empty_values() {
        let xml = "<metadata><!-- c --><groupId><![CDATA[org.acme]]></groupId><extra><deep/></extra>\
            <versioning><release></release><latest/><versions/></versioning></metadata>";
        let m = parse_metadata(xml.as_bytes()).unwrap();
        assert_eq!(m.group_id.as_deref(), Some("org.acme"));
        let v = m.versioning.unwrap();
        assert_eq!(v.release, None);
        assert_eq!(v.latest, None);
        assert_eq!(v.versions, Vec::<String>::new());
    }

    #[test]
    fn a_namespace_prefix_does_not_hide_the_elements() {
        let xml = "<m:metadata xmlns:m=\"urn:x\"><m:groupId>g</m:groupId></m:metadata>";
        let m = parse_metadata(xml.as_bytes());
        // The root local name is `metadata`, so the prefixed document is read.
        assert_eq!(m.unwrap().group_id.as_deref(), Some("g"));
    }

    #[test]
    fn must_not_resolve_entities_defined_in_a_doctype() {
        let xxe = "<?xml version=\"1.0\"?><!DOCTYPE metadata [<!ENTITY xxe SYSTEM \"file:///etc/passwd\">]>\
            <metadata><groupId>&xxe;</groupId></metadata>";
        assert_eq!(
            parse_metadata(xxe.as_bytes()),
            Err(MetadataError::DtdRefused)
        );
    }

    #[test]
    fn must_not_expand_a_billion_laughs_document() {
        let mut bomb = String::from("<?xml version=\"1.0\"?><!DOCTYPE lolz [<!ENTITY lol \"lol\">");
        for level in 1..=9 {
            let previous = if level == 1 {
                "lol".to_owned()
            } else {
                format!("lol{}", level - 1)
            };
            let _ = write!(
                bomb,
                "<!ENTITY lol{level} \"{}\">",
                format!("&{previous};").repeat(10)
            );
        }
        bomb.push_str("]><metadata><groupId>&lol9;</groupId></metadata>");
        assert_eq!(
            parse_metadata(bomb.as_bytes()),
            Err(MetadataError::DtdRefused)
        );
    }

    #[test]
    fn must_not_accept_an_undeclared_entity() {
        let xml = "<metadata><groupId>&unknown;</groupId></metadata>";
        assert_eq!(
            parse_metadata(xml.as_bytes()),
            Err(MetadataError::UnknownEntity)
        );
    }

    #[test]
    fn predefined_entities_and_character_references_resolve() {
        let xml = "<metadata><plugins><plugin><name>a&lt;b&#x26;&#65;&quot;&apos;&gt;</name>\
            <prefix>p</prefix><artifactId>a</artifactId></plugin></plugins></metadata>";
        let m = parse_metadata(xml.as_bytes()).unwrap();
        assert_eq!(m.plugins[0].name.as_deref(), Some("a<b&A\"'>"));
    }

    #[test]
    fn rejects_structural_problems() {
        let cases: [(&str, MetadataError); 8] = [
            ("", MetadataError::BadRoot),
            ("<other/>", MetadataError::BadRoot),
            ("<metadata><groupId>g</metadata>", MetadataError::Syntax),
            (
                "<metadata></metadata><metadata></metadata>",
                MetadataError::BadRoot,
            ),
            ("<metadata>", MetadataError::BadRoot),
            ("text<metadata/>", MetadataError::Syntax),
            (
                "<metadata><a><b><c><d><e><f><g><h><i><j><k><l><m/></l></k></j></i></h></g></f></e></d></c></b></a></metadata>",
                MetadataError::TooDeep,
            ),
            (
                "<metadata><groupId>a b</groupId></metadata>",
                MetadataError::BadValue("groupId"),
            ),
        ];
        for (xml, expected) in cases {
            assert_eq!(parse_metadata(xml.as_bytes()), Err(expected), "{xml}");
        }
    }

    #[test]
    fn rejects_bad_values() {
        for xml in [
            "<metadata><versioning><lastUpdated>2024</lastUpdated></versioning></metadata>",
            "<metadata><versioning><snapshot><timestamp>x</timestamp></snapshot></versioning></metadata>",
            "<metadata><versioning><snapshot><buildNumber>-1</buildNumber></snapshot></versioning></metadata>",
            "<metadata><versioning><versions><version>a/b</version></versions></versioning></metadata>",
            "<metadata><versioning><snapshotVersions><snapshotVersion><extension>jar</extension></snapshotVersion></snapshotVersions></versioning></metadata>",
            "<metadata><plugins><plugin><prefix>p</prefix></plugin></plugins></metadata>",
        ] {
            assert!(
                matches!(
                    parse_metadata(xml.as_bytes()),
                    Err(MetadataError::BadValue(_))
                ),
                "{xml}"
            );
        }
    }

    #[test]
    fn rejects_invalid_utf8_and_oversized_text() {
        assert_eq!(
            parse_metadata(&[0xff, 0xfe, b'<']),
            Err(MetadataError::NotUtf8)
        );
        let long = format!(
            "<metadata><groupId>{}</groupId></metadata>",
            "a".repeat(5000)
        );
        assert_eq!(
            parse_metadata(long.as_bytes()),
            Err(MetadataError::TextTooLong)
        );
    }

    #[test]
    fn caps_the_number_of_versions() {
        let mut xml = String::from("<metadata><versioning><versions>");
        for i in 0..=MAX_VERSIONS {
            let _ = write!(xml, "<version>{i}</version>");
        }
        xml.push_str("</versions></versioning></metadata>");
        assert_eq!(
            parse_metadata(xml.as_bytes()),
            Err(MetadataError::TooMany("version"))
        );
    }

    #[test]
    fn error_conversion_keeps_the_limit_and_hides_nothing_internal() {
        let too_large: FormatError = MetadataError::TooLarge { limit: 7 }.into();
        assert!(matches!(
            too_large,
            FormatError::PayloadTooLarge { limit: 7 }
        ));
        let other: FormatError = MetadataError::DtdRefused.into();
        assert!(matches!(other, FormatError::Unprocessable(_)));
    }
}
