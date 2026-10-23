//! Serde helper that writes `time::OffsetDateTime` as RFC 3339 text.
//!
//! The workspace enables the `time` feature `serde` only, and with that feature alone an
//! `OffsetDateTime` serializes as the crate's tuple form (`[2026, 297, 10, 0, 0, 0, 0, 0, 0]`),
//! which no JSON client and no `OpenAPI` schema can read. Every timestamp field of a core entity
//! therefore carries `#[serde(with = "crate::rfc3339")]`, or `#[serde(default, with =
//! "crate::rfc3339::option")]` for an optional one. The `default` is required: serde does not
//! fill a missing field of a `with` field with `None` on its own.
//!
//! Downstream crates (`rustryx-api` DTOs, `rustryx-db` JSON columns) use the same path, so one
//! wire format exists and nobody has to enable a `time` feature of their own:
//! `#[serde(with = "rustryx_core::rfc3339")]`.

pub use time::serde::rfc3339::{deserialize, serialize};

/// The same helper for `Option<OffsetDateTime>` (`null` and an absent field are `None`).
pub mod option {
    pub use time::serde::rfc3339::option::{deserialize, serialize};
}

#[cfg(test)]
mod tests {
    use serde::{Deserialize, Serialize};
    use time::OffsetDateTime;
    use time::macros::datetime;

    #[derive(Debug, PartialEq, Serialize, Deserialize)]
    struct Probe {
        #[serde(with = "super")]
        at: OffsetDateTime,
        #[serde(default, with = "super::option")]
        maybe: Option<OffsetDateTime>,
    }

    #[test]
    fn writes_rfc3339_text_not_the_tuple_form() {
        let probe = Probe {
            at: datetime!(2026-10-24 10:00:00 UTC),
            maybe: Some(datetime!(2026-10-24 10:00:01.5 UTC)),
        };
        let json = serde_json::to_value(&probe).expect("serialize");
        assert_eq!(json["at"], "2026-10-24T10:00:00Z");
        assert_eq!(json["maybe"], "2026-10-24T10:00:01.5Z");
    }

    #[test]
    fn round_trips_and_accepts_an_absent_optional_field() {
        let back: Probe = serde_json::from_str(r#"{"at":"2026-10-24T10:00:00Z"}"#).expect("parse");
        assert_eq!(back.at, datetime!(2026-10-24 10:00:00 UTC));
        assert_eq!(back.maybe, None);
        let again: Probe =
            serde_json::from_str(&serde_json::to_string(&back).expect("serialize")).expect("parse");
        assert_eq!(again, back);
    }

    #[test]
    fn rejects_the_tuple_form_and_free_text() {
        assert!(serde_json::from_str::<Probe>(r#"{"at":[2026,297,10,0,0,0,0,0,0]}"#).is_err());
        assert!(serde_json::from_str::<Probe>(r#"{"at":"yesterday"}"#).is_err());
    }

    /// Every source file that defines serializable types holding timestamps.
    const SOURCES: &[(&str, &str)] = &[
        ("audit.rs", include_str!("audit.rs")),
        ("identity.rs", include_str!("identity.rs")),
        ("label.rs", include_str!("label.rs")),
        ("lifecycle.rs", include_str!("lifecycle.rs")),
        ("lifecycle/gc.rs", include_str!("lifecycle/gc.rs")),
        ("model.rs", include_str!("model.rs")),
        ("node.rs", include_str!("node.rs")),
        ("notification.rs", include_str!("notification.rs")),
        ("ports/repos.rs", include_str!("ports/repos.rs")),
        ("ports/scans.rs", include_str!("ports/scans.rs")),
        ("promotion.rs", include_str!("promotion.rs")),
        ("quota.rs", include_str!("quota.rs")),
        ("registry.rs", include_str!("registry.rs")),
        ("replication.rs", include_str!("replication.rs")),
        ("robot.rs", include_str!("robot.rs")),
        ("routing.rs", include_str!("routing.rs")),
        ("scan/policy.rs", include_str!("scan/policy.rs")),
        ("scan/report.rs", include_str!("scan/report.rs")),
        ("scan/scanner.rs", include_str!("scan/scanner.rs")),
        ("scan/trust.rs", include_str!("scan/trust.rs")),
        ("settings.rs", include_str!("settings.rs")),
        (
            "settings/certificate.rs",
            include_str!("settings/certificate.rs"),
        ),
        ("status.rs", include_str!("status.rs")),
        ("storage.rs", include_str!("storage.rs")),
        ("tasks.rs", include_str!("tasks.rs")),
        ("webhook.rs", include_str!("webhook.rs")),
        ("project.rs", include_str!("project.rs")),
    ];

    /// Scans the text for fields of type `OffsetDateTime` or `Option<OffsetDateTime>` inside
    /// types that derive `Serialize`, and reports the ones whose attributes do not name the
    /// helper. Returns the number of fields checked and the offenders.
    fn timestamp_fields_without_the_helper(file: &str, text: &str) -> (usize, Vec<String>) {
        let lines: Vec<&str> = text.lines().collect();
        let mut serializing = false;
        let mut checked = 0;
        let mut offenders = Vec::new();
        for (index, raw) in lines.iter().enumerate() {
            let starts_type = ["pub struct ", "pub enum ", "struct ", "enum "]
                .iter()
                .any(|prefix| raw.starts_with(prefix));
            if starts_type {
                let mut above = index;
                serializing = false;
                while above > 0 {
                    let previous = lines[above - 1].trim();
                    if !(previous.starts_with("///") || previous.starts_with("#[")) {
                        break;
                    }
                    if previous.starts_with("#[derive(") && previous.contains("Serialize") {
                        serializing = true;
                    }
                    above -= 1;
                }
                continue;
            }
            if *raw == "}" || raw.starts_with("impl") {
                serializing = false;
            }
            if !serializing {
                continue;
            }
            let line = raw.trim();
            let Some((name, ty)) = line.split_once(": ") else {
                continue;
            };
            let name = name.strip_prefix("pub ").unwrap_or(name);
            let is_field =
                !name.is_empty() && name.chars().all(|c| c.is_alphanumeric() || c == '_');
            let is_timestamp =
                ty.starts_with("OffsetDateTime") || ty.starts_with("Option<OffsetDateTime>");
            if !(is_field && is_timestamp) {
                continue;
            }
            checked += 1;
            let mut above = index;
            let mut helped = false;
            while above > 0 {
                let previous = lines[above - 1].trim();
                if !(previous.starts_with("///") || previous.starts_with("#[")) {
                    break;
                }
                helped |= previous.starts_with("#[serde(") && previous.contains("rfc3339");
                above -= 1;
            }
            if !helped {
                offenders.push(format!("{file}:{}: {name}", index + 1));
            }
        }
        (checked, offenders)
    }

    #[test]
    fn must_not_serialize_a_timestamp_without_the_rfc3339_helper() {
        let mut checked = 0;
        let mut offenders = Vec::new();
        for (file, text) in SOURCES {
            let (count, missing) = timestamp_fields_without_the_helper(file, text);
            checked += count;
            offenders.extend(missing);
        }
        assert!(
            offenders.is_empty(),
            "timestamps that serialize as tuples: {offenders:#?}"
        );
        assert!(
            checked >= 60,
            "the scan must actually look at the fields (saw {checked})"
        );
    }

    #[test]
    fn the_timestamp_scan_flags_a_missing_helper() {
        let broken = "\
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Probe {
    pub at: OffsetDateTime,
    #[serde(with = \"crate::rfc3339\")]
    pub fine: OffsetDateTime,
    /// A doc line between the attribute and the field is fine.
    #[serde(default, with = \"crate::rfc3339::option\")]
    pub maybe: Option<OffsetDateTime>,
    pub lost: Option<OffsetDateTime>,
}

#[derive(Debug, Clone)]
pub struct NotSerialized {
    pub at: OffsetDateTime,
}
";
        let (checked, offenders) = timestamp_fields_without_the_helper("probe.rs", broken);
        assert_eq!(checked, 4);
        assert_eq!(offenders, ["probe.rs:3: at", "probe.rs:9: lost"]);
    }
}
