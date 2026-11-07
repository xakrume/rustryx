//! Registered scanners. HTTP credentials are sealed. The last probe survives an edit.

use std::sync::Arc;

use rustryx_core::error::StoreError;
use rustryx_core::ids::{ScannerId, TrustedCertificateId};
use rustryx_core::model::RepoFormat;
use rustryx_core::ports::ScannerRepo;
use rustryx_core::scan::{ProbeResult, ScanKind, ScannerAuth, ScannerKind, ScannerRecord};
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::error::{backend, map_err};
use crate::sealer::SecretSealer;

/// Scanners.
#[derive(Clone)]
pub struct PgScannerRepo {
    pool: PgPool,
    sealer: Arc<dyn SecretSealer>,
}

impl PgScannerRepo {
    /// A repo that seals HTTP credentials with `sealer`.
    #[must_use]
    pub fn new(pool: PgPool, sealer: Arc<dyn SecretSealer>) -> Self {
        Self { pool, sealer }
    }
}

#[derive(sqlx::FromRow)]
struct Row {
    id: Uuid,
    name: String,
    kind: String,
    endpoint: Option<String>,
    auth: serde_json::Value,
    ca_certificate_ids: Vec<Uuid>,
    capabilities: Vec<String>,
    formats: Vec<String>,
    is_default: bool,
    enabled: bool,
    last_probe: Option<serde_json::Value>,
    version: i64,
    created_at: OffsetDateTime,
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum StoredAuth {
    None,
    Basic {
        username: String,
        password_sealed: String,
    },
    Bearer {
        token_sealed: String,
    },
    ApiKey {
        header: String,
        value_sealed: String,
    },
}

#[async_trait::async_trait]
impl ScannerRepo for PgScannerRepo {
    async fn create(&self, scanner: &ScannerRecord) -> Result<(), StoreError> {
        scanner
            .validate()
            .map_err(|error| StoreError::Conflict(error.to_string()))?;
        sqlx::query(
            "INSERT INTO scanners (id, name, kind, endpoint, auth, ca_certificate_ids, \
             capabilities, formats, is_default, enabled, last_probe, version, created_at) \
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13)",
        )
        .bind(scanner.id.as_uuid())
        .bind(&scanner.name)
        .bind(kind_label(scanner.kind))
        .bind(&scanner.endpoint)
        .bind(auth_value(self.sealer.as_ref(), scanner)?)
        .bind(cert_ids(&scanner.ca_certificate_ids))
        .bind(kind_names(&scanner.capabilities)?)
        .bind(format_names(&scanner.formats))
        .bind(scanner.is_default)
        .bind(scanner.enabled)
        .bind(json_of(scanner.last_probe.as_ref())?)
        .bind(scanner.version)
        .bind(scanner.created_at)
        .execute(&self.pool)
        .await
        .map_err(|e| map_err("create scanner", e))?;
        Ok(())
    }

    async fn get(&self, id: ScannerId) -> Result<Option<ScannerRecord>, StoreError> {
        let row: Option<Row> = sqlx::query_as("SELECT * FROM scanners WHERE id = $1")
            .bind(id.as_uuid())
            .fetch_optional(&self.pool)
            .await
            .map_err(|e| map_err("read scanner", e))?;
        row.map(|row| row.into_record(self.sealer.as_ref()))
            .transpose()
    }

    async fn list(&self) -> Result<Vec<ScannerRecord>, StoreError> {
        let rows: Vec<Row> = sqlx::query_as("SELECT * FROM scanners ORDER BY name")
            .fetch_all(&self.pool)
            .await
            .map_err(|e| map_err("list scanners", e))?;
        rows.into_iter()
            .map(|row| row.into_record(self.sealer.as_ref()))
            .collect()
    }

    async fn update(
        &self,
        scanner: &ScannerRecord,
        expected_version: i64,
    ) -> Result<(), StoreError> {
        scanner
            .validate()
            .map_err(|error| StoreError::Conflict(error.to_string()))?;
        let updated = sqlx::query(
            "UPDATE scanners SET name = $2, kind = $3, endpoint = $4, auth = $5, \
             ca_certificate_ids = $6, capabilities = $7, formats = $8, is_default = $9, \
             enabled = $10, version = $11 WHERE id = $1 AND version = $12",
        )
        .bind(scanner.id.as_uuid())
        .bind(&scanner.name)
        .bind(kind_label(scanner.kind))
        .bind(&scanner.endpoint)
        .bind(auth_value(self.sealer.as_ref(), scanner)?)
        .bind(cert_ids(&scanner.ca_certificate_ids))
        .bind(kind_names(&scanner.capabilities)?)
        .bind(format_names(&scanner.formats))
        .bind(scanner.is_default)
        .bind(scanner.enabled)
        .bind(expected_version + 1)
        .bind(expected_version)
        .execute(&self.pool)
        .await
        .map_err(|e| map_err("update scanner", e))?;
        if updated.rows_affected() == 0 {
            return missing_or_stale(&self.pool, scanner.id, "update scanner").await;
        }
        Ok(())
    }

    async fn delete(&self, id: ScannerId) -> Result<(), StoreError> {
        let deleted = sqlx::query("DELETE FROM scanners WHERE id = $1")
            .bind(id.as_uuid())
            .execute(&self.pool)
            .await
            .map_err(|e| map_err("delete scanner", e))?;
        if deleted.rows_affected() == 0 {
            return Err(StoreError::NotFound("scanner".to_owned()));
        }
        Ok(())
    }

    async fn set_default(&self, id: ScannerId) -> Result<(), StoreError> {
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| map_err("set default scanner", e))?;
        let chosen: Option<(Vec<String>, Vec<String>)> =
            sqlx::query_as("SELECT capabilities, formats FROM scanners WHERE id = $1 FOR UPDATE")
                .bind(id.as_uuid())
                .fetch_optional(&mut *tx)
                .await
                .map_err(|e| map_err("set default scanner", e))?;
        let Some((capabilities, formats)) = chosen else {
            return Err(StoreError::NotFound("scanner".to_owned()));
        };
        sqlx::query(
            "UPDATE scanners SET is_default = false \
             WHERE id <> $1 AND is_default \
               AND capabilities && $2 \
               AND (cardinality(formats) = 0 OR cardinality($3::text[]) = 0 OR formats && $3)",
        )
        .bind(id.as_uuid())
        .bind(&capabilities)
        .bind(&formats)
        .execute(&mut *tx)
        .await
        .map_err(|e| map_err("set default scanner", e))?;
        sqlx::query("UPDATE scanners SET is_default = true WHERE id = $1")
            .bind(id.as_uuid())
            .execute(&mut *tx)
            .await
            .map_err(|e| map_err("set default scanner", e))?;
        tx.commit()
            .await
            .map_err(|e| map_err("set default scanner", e))?;
        Ok(())
    }

    async fn record_probe(&self, id: ScannerId, probe: &ProbeResult) -> Result<(), StoreError> {
        let updated = sqlx::query("UPDATE scanners SET last_probe = $2 WHERE id = $1")
            .bind(id.as_uuid())
            .bind(json_of(Some(probe))?)
            .execute(&self.pool)
            .await
            .map_err(|e| map_err("record scanner probe", e))?;
        if updated.rows_affected() == 0 {
            return Err(StoreError::NotFound("scanner".to_owned()));
        }
        Ok(())
    }
}

impl Row {
    fn into_record(self, sealer: &dyn SecretSealer) -> Result<ScannerRecord, StoreError> {
        let id = ScannerId::from_uuid(self.id);
        let mut record =
            ScannerRecord::new(id, self.name, parse_kind(&self.kind)?, self.created_at);
        record.endpoint = self.endpoint;
        record.auth = open_auth(sealer, id, &self.auth)?;
        record.ca_certificate_ids = self
            .ca_certificate_ids
            .into_iter()
            .map(TrustedCertificateId::from_uuid)
            .collect();
        record.capabilities = self
            .capabilities
            .iter()
            .map(|name| parse_scan_kind(name))
            .collect::<Result<Vec<_>, _>>()?;
        record.formats = self
            .formats
            .iter()
            .map(|name| parse_format(name))
            .collect::<Result<Vec<_>, _>>()?;
        record.is_default = self.is_default;
        record.enabled = self.enabled;
        record.last_probe = self
            .last_probe
            .map(serde_json::from_value)
            .transpose()
            .map_err(|_| backend("read scanner", "last probe"))?;
        record.version = self.version;
        Ok(record)
    }
}

async fn missing_or_stale(
    pool: &PgPool,
    id: ScannerId,
    context: &'static str,
) -> Result<(), StoreError> {
    let exists: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM scanners WHERE id = $1)")
        .bind(id.as_uuid())
        .fetch_one(pool)
        .await
        .map_err(|e| map_err(context, e))?;
    if exists {
        Err(StoreError::Conflict(
            "scanner was changed by someone else".to_owned(),
        ))
    } else {
        Err(StoreError::NotFound("scanner".to_owned()))
    }
}

fn kind_label(kind: ScannerKind) -> &'static str {
    match kind {
        ScannerKind::Trivy => "trivy",
        ScannerKind::Osv => "osv",
        ScannerKind::Http => "http",
    }
}

fn parse_kind(label: &str) -> Result<ScannerKind, StoreError> {
    match label {
        "trivy" => Ok(ScannerKind::Trivy),
        "osv" => Ok(ScannerKind::Osv),
        "http" => Ok(ScannerKind::Http),
        _ => Err(backend("read scanner", "unknown kind")),
    }
}

fn kind_names(kinds: &[ScanKind]) -> Result<Vec<String>, StoreError> {
    kinds.iter().copied().map(scan_kind_name).collect()
}

fn scan_kind_name(kind: ScanKind) -> Result<String, StoreError> {
    match serde_json::to_value(kind).map_err(|_| backend("write scanner", "capability"))? {
        serde_json::Value::String(name) => Ok(name),
        _ => Err(backend("write scanner", "capability")),
    }
}

fn parse_scan_kind(name: &str) -> Result<ScanKind, StoreError> {
    serde_json::from_value(serde_json::Value::String(name.to_owned()))
        .map_err(|_| backend("read scanner", "unknown capability"))
}

fn format_names(formats: &[RepoFormat]) -> Vec<String> {
    formats
        .iter()
        .map(|format| format.as_str().to_owned())
        .collect()
}

fn parse_format(name: &str) -> Result<RepoFormat, StoreError> {
    RepoFormat::ALL
        .iter()
        .copied()
        .find(|format| format.as_str() == name)
        .ok_or_else(|| backend("read scanner", "unknown format"))
}

fn cert_ids(ids: &[TrustedCertificateId]) -> Vec<Uuid> {
    ids.iter().map(TrustedCertificateId::as_uuid).collect()
}

fn json_of<T: Serialize>(value: Option<&T>) -> Result<Option<serde_json::Value>, StoreError> {
    value
        .map(serde_json::to_value)
        .transpose()
        .map_err(|_| backend("write scanner", "json"))
}

fn auth_value(
    sealer: &dyn SecretSealer,
    scanner: &ScannerRecord,
) -> Result<serde_json::Value, StoreError> {
    let aad = credential_aad(scanner.id);
    let stored = match &scanner.auth {
        ScannerAuth::None => StoredAuth::None,
        ScannerAuth::Basic { username, password } => StoredAuth::Basic {
            username: username.clone(),
            password_sealed: sealer.seal(&aad, password.expose())?,
        },
        ScannerAuth::Bearer { token } => StoredAuth::Bearer {
            token_sealed: sealer.seal(&aad, token.expose())?,
        },
        ScannerAuth::ApiKey { header, value } => StoredAuth::ApiKey {
            header: header.clone(),
            value_sealed: sealer.seal(&aad, value.expose())?,
        },
    };
    serde_json::to_value(stored).map_err(|_| backend("write scanner", "auth"))
}

fn open_auth(
    sealer: &dyn SecretSealer,
    id: ScannerId,
    value: &serde_json::Value,
) -> Result<ScannerAuth, StoreError> {
    let stored: StoredAuth =
        serde_json::from_value(value.clone()).map_err(|_| backend("read scanner", "auth"))?;
    let aad = credential_aad(id);
    Ok(match stored {
        StoredAuth::None => ScannerAuth::None,
        StoredAuth::Basic {
            username,
            password_sealed,
        } => ScannerAuth::Basic {
            username,
            password: sealer.open(&aad, &password_sealed)?,
        },
        StoredAuth::Bearer { token_sealed } => ScannerAuth::Bearer {
            token: sealer.open(&aad, &token_sealed)?,
        },
        StoredAuth::ApiKey {
            header,
            value_sealed,
        } => ScannerAuth::ApiKey {
            header,
            value: sealer.open(&aad, &value_sealed)?,
        },
    })
}

fn credential_aad(id: ScannerId) -> Vec<u8> {
    let mut aad = b"rustryx/scanner/credential/v1/".to_vec();
    aad.extend_from_slice(id.as_uuid().as_bytes());
    aad
}
