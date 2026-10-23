//! Test PKI: a throwaway CA and server certificates for the TLS tests.

use std::sync::Arc;

use rcgen::{
    BasicConstraints, Certificate, CertificateParams, DnType, ExtendedKeyUsagePurpose, IsCa,
    Issuer, KeyPair, KeyUsagePurpose,
};
use rustls::ServerConfig;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use time::{Duration, OffsetDateTime};

/// A certificate authority that exists for one test.
pub(crate) struct TestPki {
    ca: Certificate,
    issuer: Issuer<'static, KeyPair>,
}

/// A server certificate with its key, in the encodings tests need.
pub(crate) struct ServerIdentity {
    pub(crate) cert_der: CertificateDer<'static>,
    pub(crate) key_der: Vec<u8>,
    pub(crate) cert_pem: String,
    pub(crate) key_pem: String,
}

impl TestPki {
    pub(crate) fn new() -> Self {
        let mut params = CertificateParams::new(Vec::<String>::new()).unwrap();
        params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        set_validity(&mut params);
        params
            .distinguished_name
            .push(DnType::CommonName, "Rustryx LDAP test CA");
        params.key_usages = vec![
            KeyUsagePurpose::DigitalSignature,
            KeyUsagePurpose::KeyCertSign,
            KeyUsagePurpose::CrlSign,
        ];
        let key = KeyPair::generate().unwrap();
        let ca = params.self_signed(&key).unwrap();
        Self {
            ca,
            issuer: Issuer::new(params, key),
        }
    }

    pub(crate) fn ca_pem(&self) -> String {
        self.ca.pem()
    }

    /// A server certificate valid for `names` (DNS names or IP addresses), signed by this CA.
    pub(crate) fn server(&self, names: &[&str]) -> ServerIdentity {
        let key = KeyPair::generate().unwrap();
        let params = leaf_params(names);
        let cert = params.signed_by(&key, &self.issuer).unwrap();
        identity(&cert, &key)
    }
}

/// A self-signed server certificate that no CA vouches for.
pub(crate) fn self_signed_server(names: &[&str]) -> ServerIdentity {
    let key = KeyPair::generate().unwrap();
    let cert = leaf_params(names).self_signed(&key).unwrap();
    identity(&cert, &key)
}

fn leaf_params(names: &[&str]) -> CertificateParams {
    let names: Vec<String> = names.iter().map(|name| (*name).to_owned()).collect();
    let mut params = CertificateParams::new(names).unwrap();
    params
        .distinguished_name
        .push(DnType::CommonName, "ldap test server");
    params.key_usages = vec![KeyUsagePurpose::DigitalSignature];
    params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
    set_validity(&mut params);
    params
}

/// Ten years from a day ago: valid now, and for any external tool that inspects the dates.
fn set_validity(params: &mut CertificateParams) {
    let now = OffsetDateTime::now_utc();
    params.not_before = now - Duration::days(1);
    params.not_after = now + Duration::days(3650);
}

fn identity(cert: &Certificate, key: &KeyPair) -> ServerIdentity {
    ServerIdentity {
        cert_der: cert.der().clone(),
        key_der: key.serialize_der(),
        cert_pem: cert.pem(),
        key_pem: key.serialize_pem(),
    }
}

impl ServerIdentity {
    /// TLS server configuration presenting this identity.
    pub(crate) fn server_config(&self) -> Arc<ServerConfig> {
        let provider = Arc::new(rustls::crypto::aws_lc_rs::default_provider());
        let key = PrivateKeyDer::from(PrivatePkcs8KeyDer::from(self.key_der.clone()));
        let config = ServerConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_no_client_auth()
            .with_single_cert(vec![self.cert_der.clone()], key)
            .unwrap();
        Arc::new(config)
    }
}
