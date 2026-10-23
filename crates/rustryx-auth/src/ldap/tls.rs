//! TLS client configuration for LDAPS and `StartTLS`.
//!
//! Certificate chain AND hostname are verified by `rustls`' default verifier against
//! the root store built here. There is no code path that installs a different
//! verifier, so "no switch to turn verification off" is a property of this module
//! (`rustls::ClientConfig::dangerous()` is never called) and of `net.rs` (which never
//! calls `LdapConnSettings::set_no_tls_verify`).

use std::path::{Path, PathBuf};
use std::sync::Arc;

use rustls::pki_types::CertificateDer;
use rustls::pki_types::pem::PemObject;
use rustls::{ClientConfig, RootCertStore};

/// Why the trust material could not be set up.
#[derive(Debug, thiserror::Error)]
pub enum TlsSetupError {
    /// The CA file could not be read.
    #[error("cannot read ldap.ca_file {}: {source}", path.display())]
    ReadCaFile {
        /// Path from the configuration.
        path: PathBuf,
        /// I/O error.
        #[source]
        source: std::io::Error,
    },
    /// The CA file is not valid PEM, or holds a certificate that cannot be a trust anchor.
    #[error("ldap.ca_file {} holds an invalid certificate", path.display())]
    InvalidCaFile {
        /// Path from the configuration.
        path: PathBuf,
    },
    /// The CA file has no certificate in it.
    #[error("ldap.ca_file {} contains no certificates", path.display())]
    EmptyCaFile {
        /// Path from the configuration.
        path: PathBuf,
    },
    /// No `ca_file` was given and the platform trust store is empty or unreadable.
    #[error("the platform trust store has no usable certificates: set ldap.ca_file")]
    NoPlatformRoots,
    /// The TLS provider rejected the configuration.
    #[error("TLS setup failed: {0}")]
    Provider(#[from] rustls::Error),
}

/// Builds the client configuration.
///
/// With a `ca_file`, ONLY the certificates in it are trusted: a deployment that pins
/// its directory to an internal CA must not also trust every public CA. Without one,
/// the platform trust store is used.
pub(crate) async fn client_config(
    ca_file: Option<&Path>,
) -> Result<Arc<ClientConfig>, TlsSetupError> {
    let roots = match ca_file {
        Some(path) => {
            let pem = tokio::fs::read(path)
                .await
                .map_err(|source| TlsSetupError::ReadCaFile {
                    path: path.to_path_buf(),
                    source,
                })?;
            roots_from_pem(&pem, path)?
        }
        None => platform_roots().await?,
    };
    config_from_roots(roots)
}

pub(crate) fn roots_from_pem(pem: &[u8], path: &Path) -> Result<RootCertStore, TlsSetupError> {
    let mut roots = RootCertStore::empty();
    let mut count = 0usize;
    for certificate in CertificateDer::pem_slice_iter(pem) {
        let certificate = certificate.map_err(|_| TlsSetupError::InvalidCaFile {
            path: path.to_path_buf(),
        })?;
        roots
            .add(certificate)
            .map_err(|_| TlsSetupError::InvalidCaFile {
                path: path.to_path_buf(),
            })?;
        count += 1;
    }
    if count == 0 {
        return Err(TlsSetupError::EmptyCaFile {
            path: path.to_path_buf(),
        });
    }
    Ok(roots)
}

async fn platform_roots() -> Result<RootCertStore, TlsSetupError> {
    // One-time startup work, not on a request path: `rustls-native-certs` only has a
    // blocking API, and `ldap3` would otherwise do the same blocking read lazily inside
    // the first login.
    let loaded = tokio::task::spawn_blocking(rustls_native_certs::load_native_certs)
        .await
        .map_err(|_| TlsSetupError::NoPlatformRoots)?;
    let mut roots = RootCertStore::empty();
    let (added, ignored) = roots.add_parsable_certificates(loaded.certs);
    if !loaded.errors.is_empty() || ignored > 0 {
        tracing::warn!(
            errors = loaded.errors.len(),
            ignored,
            added,
            "some platform trust store entries could not be loaded"
        );
    }
    if added == 0 {
        return Err(TlsSetupError::NoPlatformRoots);
    }
    Ok(roots)
}

pub(crate) fn config_from_roots(roots: RootCertStore) -> Result<Arc<ClientConfig>, TlsSetupError> {
    // The provider is named explicitly. `ClientConfig::builder()` picks the process
    // default provider and panics when both `ring` and `aws-lc-rs` are compiled in,
    // which is the case in this workspace (`openidconnect` pulls in `ring` through
    // `reqwest`). `ldap3` is built with `aws-lc-rs`, so that is the one used here.
    let provider = Arc::new(rustls::crypto::aws_lc_rs::default_provider());
    let config = ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()?
        .with_root_certificates(roots)
        .with_no_client_auth();
    Ok(Arc::new(config))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ldap::testing_tls::TestPki;

    #[tokio::test]
    async fn loads_every_certificate_of_the_ca_file() {
        let pki = TestPki::new();
        let other = TestPki::new();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ca.pem");
        std::fs::write(&path, format!("{}{}", pki.ca_pem(), other.ca_pem())).unwrap();
        let config = client_config(Some(&path)).await;
        assert!(config.is_ok());
        let roots = roots_from_pem(&std::fs::read(&path).unwrap(), &path).unwrap();
        assert_eq!(roots.len(), 2);
    }

    #[tokio::test]
    async fn must_not_start_when_the_ca_file_is_missing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("absent.pem");
        let error = client_config(Some(&path)).await.unwrap_err();
        assert!(matches!(error, TlsSetupError::ReadCaFile { .. }), "{error}");
        assert!(error.to_string().contains("absent.pem"));
    }

    #[tokio::test]
    async fn must_not_start_when_the_ca_file_holds_no_certificate() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("empty.pem");
        std::fs::write(&path, "not a certificate\n").unwrap();
        let error = client_config(Some(&path)).await.unwrap_err();
        assert!(
            matches!(error, TlsSetupError::EmptyCaFile { .. }),
            "{error}"
        );
        std::fs::write(&path, "").unwrap();
        assert!(matches!(
            client_config(Some(&path)).await.unwrap_err(),
            TlsSetupError::EmptyCaFile { .. }
        ));
    }

    #[tokio::test]
    async fn must_not_start_when_the_ca_file_is_corrupt() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bad.pem");
        std::fs::write(
            &path,
            "-----BEGIN CERTIFICATE-----\n!!!not base64!!!\n-----END CERTIFICATE-----\n",
        )
        .unwrap();
        let error = client_config(Some(&path)).await.unwrap_err();
        assert!(
            matches!(error, TlsSetupError::InvalidCaFile { .. }),
            "{error}"
        );

        std::fs::write(
            &path,
            "-----BEGIN CERTIFICATE-----\nAAAA\n-----END CERTIFICATE-----\n",
        )
        .unwrap();
        assert!(matches!(
            client_config(Some(&path)).await.unwrap_err(),
            TlsSetupError::InvalidCaFile { .. }
        ));
    }

    #[test]
    fn error_messages_are_actionable() {
        let error = TlsSetupError::NoPlatformRoots;
        assert!(error.to_string().contains("ldap.ca_file"));
    }
}
