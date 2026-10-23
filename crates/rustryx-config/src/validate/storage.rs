//! Rules for `database`, `blob`, `lock` and `gc`.

use super::Report;
use crate::AppConfig;
use crate::schema::{BlobKind, LockBackendKind};
use crate::secret::ConfigSecret;

pub(super) fn database(config: &AppConfig, report: &mut Report) {
    let db = &config.database;
    report.range("database.pool_size", u64::from(db.pool_size), 1, 500);
    report.range(
        "database.acquire_timeout_secs",
        db.acquire_timeout_secs,
        1,
        300,
    );
    report.range(
        "database.connect_timeout_secs",
        db.connect_timeout_secs,
        1,
        300,
    );
    report.range(
        "database.statement_timeout_secs",
        db.statement_timeout_secs,
        1,
        3600,
    );

    if let Some(secret) = report.require_secret("database.url", db.url.as_ref()) {
        postgres_url(config, "database.url", secret, report);
    }
    if let Some(secret) = db.direct_url.as_ref() {
        if secret.is_blank() {
            report.err("database.direct_url", "must not be blank when set");
        } else {
            postgres_url(config, "database.direct_url", secret, report);
        }
    }
}

fn postgres_url(config: &AppConfig, key: &str, secret: &ConfigSecret, report: &mut Report) {
    let Some(url) = report.url(key, secret.expose()) else {
        return;
    };
    if !matches!(url.scheme(), "postgres" | "postgresql") {
        report.err(key, "must use the postgres:// or postgresql:// scheme");
    }
    let host_param = url.query_pairs().any(|(k, v)| k == "host" && !v.is_empty());
    if url.host_str().is_none_or(str::is_empty) && !host_param {
        report.err(key, "must name a host");
    }
    let sslmode = url
        .query_pairs()
        .find(|(k, _)| k == "sslmode")
        .map(|(_, v)| v.into_owned());
    let verified = matches!(sslmode.as_deref(), Some("verify-ca" | "verify-full"));
    if config.is_production() && !verified {
        report.warn(
            key,
            "production traffic to PostgreSQL should use sslmode=verify-full (or verify-ca)",
        );
    }
}

pub(super) fn blob(config: &AppConfig, report: &mut Report) {
    let blob = &config.blob;
    let name_ok = !blob.store_name.is_empty()
        && blob.store_name.len() <= 64
        && blob
            .store_name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'));
    if !name_ok {
        report.err(
            "blob.store_name",
            "must be 1 to 64 characters of letters, digits, '.', '_' or '-'",
        );
    }
    report.range(
        "blob.local_min_free_percent",
        u64::from(blob.local_min_free_percent),
        0,
        90,
    );
    report.range("blob.s3_part_size_mb", blob.s3_part_size_mb, 5, 5120);

    match blob.kind {
        BlobKind::Local => {
            if blob.local_path.as_os_str().is_empty() {
                report.err("blob.local_path", "must not be empty");
            } else if config.is_production() && !blob.local_path.is_absolute() {
                report.err("blob.local_path", "must be an absolute path in production");
            }
        }
        BlobKind::S3 => s3(config, report),
    }
}

fn s3(config: &AppConfig, report: &mut Report) {
    let blob = &config.blob;
    if let Some(endpoint) = report.require_text("blob.s3_endpoint", blob.s3_endpoint.as_deref())
        && let Some(url) = report.url("blob.s3_endpoint", &endpoint)
    {
        if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
            report.err("blob.s3_endpoint", "must be an http(s) URL with a host");
        } else if config.is_production() && url.scheme() != "https" {
            report.warn("blob.s3_endpoint", "an http endpoint is used in production");
        }
    }
    report.require_text("blob.s3_bucket", blob.s3_bucket.as_deref());
    report.require_text("blob.s3_region", blob.s3_region.as_deref());
    report.require_secret("blob.s3_access_key", blob.s3_access_key.as_ref());
    report.require_secret("blob.s3_secret_key", blob.s3_secret_key.as_ref());
}

pub(super) fn lock(config: &AppConfig, report: &mut Report) {
    let lock = &config.lock;
    report.range("lock.ttl_secs", lock.ttl_secs, 5, 300);
    match lock.backend {
        LockBackendKind::Postgres => {
            if lock.redis_url.is_some() {
                report.warn(
                    "lock.redis_url",
                    "is ignored because lock.backend is postgres",
                );
            }
        }
        LockBackendKind::Redis => {
            let Some(secret) = report.require_secret("lock.redis_url", lock.redis_url.as_ref())
            else {
                return;
            };
            match report.url("lock.redis_url", secret.expose()) {
                Some(url) if matches!(url.scheme(), "redis" | "rediss") => {
                    if config.is_production() && url.scheme() == "redis" {
                        report.warn(
                            "lock.redis_url",
                            "production traffic to Redis should use rediss://",
                        );
                    }
                }
                Some(_) => report.err(
                    "lock.redis_url",
                    "must use the redis:// or rediss:// scheme",
                ),
                None => {}
            }
        }
    }
}

pub(super) fn gc(config: &AppConfig, report: &mut Report) {
    let gc = &config.gc;
    if !gc.enabled {
        return;
    }
    report.range("gc.interval_secs", gc.interval_secs, 60, 30 * 86_400);
    report.range("gc.batch_size", u64::from(gc.batch_size), 1, 100_000);
    // Invariant: an upload may run for up to `limits.request_timeout_secs` before its
    // blob is referenced by an asset row. The collector deletes unreferenced blobs
    // older than `min_age_secs`, so a smaller value could delete a blob that is
    // still being written. The same two settings are read here to hold them together.
    if gc.min_age_secs <= config.limits.request_timeout_secs {
        report.err(
            "gc.min_age_secs",
            "must be greater than limits.request_timeout_secs, otherwise the blob of an upload in progress could be collected",
        );
    }
}
