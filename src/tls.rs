//! TLS configuration for the CockroachDB connection.
//!
//! | mode        | behaviour                                              |
//! | ----------- | ------------------------------------------------------ |
//! | disable     | plain TCP                                              |
//! | allow/prefer| TLS when the server offers it, no verification         |
//! | require     | TLS, no certificate verification                       |
//! | verify-ca   | verify the chain, skip only the hostname check         |
//! | verify-full | verify chain and hostname                              |

use std::sync::Arc;

use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::client::WebPkiServerVerifier;
use rustls::crypto::{aws_lc_rs, CryptoProvider};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, ServerName, UnixTime};
use rustls::{
    CertificateError, ClientConfig, DigitallySignedStruct, Error as TlsError, RootCertStore,
    SignatureScheme,
};
use tokio_postgres_rustls::MakeRustlsConnect;

use crate::error::PluginError;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SslMode {
    Disable,
    Prefer,
    Require,
    VerifyCa,
    VerifyFull,
}

impl SslMode {
    /// `None` / empty means the default (`prefer`).
    pub fn parse(value: Option<&str>) -> Result<Self, PluginError> {
        match value.map(|v| v.trim().to_ascii_lowercase()).as_deref() {
            None | Some("") | Some("prefer") | Some("allow") => Ok(Self::Prefer),
            Some("disable") => Ok(Self::Disable),
            Some("require") => Ok(Self::Require),
            Some("verify-ca") => Ok(Self::VerifyCa),
            Some("verify-full") => Ok(Self::VerifyFull),
            Some(other) => Err(PluginError::invalid_params(format!(
                "unsupported ssl_mode '{other}' (expected disable, prefer, require, verify-ca or verify-full)"
            ))),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Disable => "disable",
            Self::Prefer => "prefer",
            Self::Require => "require",
            Self::VerifyCa => "verify-ca",
            Self::VerifyFull => "verify-full",
        }
    }

    pub fn pg_ssl_mode(self) -> tokio_postgres::config::SslMode {
        use tokio_postgres::config::SslMode as M;
        match self {
            Self::Disable => M::Disable,
            Self::Prefer => M::Prefer,
            Self::Require | Self::VerifyCa | Self::VerifyFull => M::Require,
        }
    }
}

/// Accepts any certificate (`prefer`, `require`).
#[derive(Debug)]
struct NoVerify(Arc<CryptoProvider>);

impl ServerCertVerifier for NoVerify {
    fn verify_server_cert(
        &self,
        _: &CertificateDer<'_>,
        _: &[CertificateDer<'_>],
        _: &ServerName<'_>,
        _: &[u8],
        _: UnixTime,
    ) -> Result<ServerCertVerified, TlsError> {
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, TlsError> {
        rustls::crypto::verify_tls12_signature(
            message,
            cert,
            dss,
            &self.0.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, TlsError> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &self.0.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.0.signature_verification_algorithms.supported_schemes()
    }
}

/// Delegates to `inner` but tolerates a hostname mismatch (`verify-ca`).
/// The chain is validated before the name, so a name error means the chain
/// itself was accepted.
#[derive(Debug)]
struct IgnoreHostname(Arc<dyn ServerCertVerifier>);

pub(crate) fn is_name_error(err: &TlsError) -> bool {
    matches!(
        err,
        TlsError::InvalidCertificate(
            CertificateError::NotValidForName | CertificateError::NotValidForNameContext { .. }
        )
    )
}

impl ServerCertVerifier for IgnoreHostname {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        server_name: &ServerName<'_>,
        ocsp: &[u8],
        now: UnixTime,
    ) -> Result<ServerCertVerified, TlsError> {
        match self
            .0
            .verify_server_cert(end_entity, intermediates, server_name, ocsp, now)
        {
            Err(e) if is_name_error(&e) => Ok(ServerCertVerified::assertion()),
            other => other,
        }
    }

    fn verify_tls12_signature(
        &self,
        m: &[u8],
        c: &CertificateDer<'_>,
        d: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, TlsError> {
        self.0.verify_tls12_signature(m, c, d)
    }

    fn verify_tls13_signature(
        &self,
        m: &[u8],
        c: &CertificateDer<'_>,
        d: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, TlsError> {
        self.0.verify_tls13_signature(m, c, d)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.0.supported_verify_schemes()
    }
}

fn read_file(path: &str, what: &str) -> Result<Vec<u8>, PluginError> {
    std::fs::read(path)
        .map_err(|e| PluginError::invalid_params(format!("cannot read {what} '{path}': {e}")))
}

pub fn parse_certs(pem: &[u8], what: &str) -> Result<Vec<CertificateDer<'static>>, PluginError> {
    let certs: Vec<_> = rustls_pemfile::certs(&mut &*pem)
        .collect::<Result<_, _>>()
        .map_err(|e| PluginError::invalid_params(format!("invalid {what} PEM: {e}")))?;
    if certs.is_empty() {
        return Err(PluginError::invalid_params(format!(
            "no certificates found in {what}"
        )));
    }
    Ok(certs)
}

/// Accepts PKCS#8, PKCS#1 (RSA) and SEC1 (EC) PEM keys.
pub fn parse_private_key(pem: &[u8]) -> Result<PrivateKeyDer<'static>, PluginError> {
    rustls_pemfile::private_key(&mut &*pem)
        .map_err(|e| PluginError::invalid_params(format!("invalid client key PEM: {e}")))?
        .ok_or_else(|| PluginError::invalid_params("no private key found in ssl_key"))
}

fn root_store(ca_pem: &[u8]) -> Result<Arc<RootCertStore>, PluginError> {
    let mut roots = RootCertStore::empty();
    for cert in parse_certs(ca_pem, "ssl_ca")? {
        roots
            .add(cert)
            .map_err(|e| PluginError::invalid_params(format!("invalid CA certificate: {e}")))?;
    }
    Ok(Arc::new(roots))
}

/// Build the rustls config. `ca_pem`, `cert_pem` and `key_pem` are file
/// contents (see [`build_connector`] for the path-based entry point).
pub fn build_config(
    mode: SslMode,
    ca_pem: Option<&[u8]>,
    client: Option<(&[u8], &[u8])>,
) -> Result<ClientConfig, PluginError> {
    let provider = Arc::new(aws_lc_rs::default_provider());
    let tls_err = |e: TlsError| PluginError::internal(format!("TLS setup failed: {e}"));

    let verifier: Arc<dyn ServerCertVerifier> = match mode {
        SslMode::Disable | SslMode::Prefer | SslMode::Require => {
            Arc::new(NoVerify(provider.clone()))
        }
        SslMode::VerifyCa | SslMode::VerifyFull => {
            let base: Arc<dyn ServerCertVerifier> = match ca_pem {
                Some(pem) => {
                    WebPkiServerVerifier::builder_with_provider(root_store(pem)?, provider.clone())
                        .build()
                        .map_err(|e| PluginError::internal(format!("TLS setup failed: {e}")))?
                }
                None => Arc::new(
                    rustls_platform_verifier::Verifier::new(provider.clone()).map_err(tls_err)?,
                ),
            };
            if mode == SslMode::VerifyCa {
                Arc::new(IgnoreHostname(base))
            } else {
                base
            }
        }
    };

    let builder = ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(tls_err)?
        .dangerous()
        .with_custom_certificate_verifier(verifier);

    match client {
        Some((cert, key)) => builder
            .with_client_auth_cert(parse_certs(cert, "ssl_cert")?, parse_private_key(key)?)
            .map_err(tls_err),
        None => Ok(builder.with_no_client_auth()),
    }
}

/// Path-based entry point. Always returns a connector; with `disable` it is
/// never used because `tokio_postgres` never starts a TLS handshake.
pub fn build_connector(
    mode: SslMode,
    ca: Option<&str>,
    cert: Option<&str>,
    key: Option<&str>,
) -> Result<MakeRustlsConnect, PluginError> {
    let ca_pem = match (mode, ca) {
        (SslMode::Disable, _) => None,
        (_, Some(p)) => Some(read_file(p, "ssl_ca")?),
        _ => None,
    };
    let client = match (mode, cert, key) {
        (SslMode::Disable, _, _) => None,
        (_, Some(c), Some(k)) => Some((read_file(c, "ssl_cert")?, read_file(k, "ssl_key")?)),
        (_, None, None) => None,
        _ => {
            return Err(PluginError::invalid_params(
                "client certificate authentication needs both ssl_cert and ssl_key",
            ))
        }
    };
    let cfg = build_config(
        mode,
        ca_pem.as_deref(),
        client.as_ref().map(|(c, k)| (c.as_slice(), k.as_slice())),
    )?;
    Ok(MakeRustlsConnect::new(cfg))
}

#[cfg(test)]
#[path = "tls_tests.rs"]
mod tests;
