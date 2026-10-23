use super::*;

#[test]
fn parses_ssl_modes() {
    assert_eq!(SslMode::parse(None).unwrap(), SslMode::Prefer);
    assert_eq!(SslMode::parse(Some("")).unwrap(), SslMode::Prefer);
    assert_eq!(SslMode::parse(Some("allow")).unwrap(), SslMode::Prefer);
    assert_eq!(SslMode::parse(Some("Disable")).unwrap(), SslMode::Disable);
    assert_eq!(SslMode::parse(Some("require")).unwrap(), SslMode::Require);
    assert_eq!(
        SslMode::parse(Some("verify-ca")).unwrap(),
        SslMode::VerifyCa
    );
    assert_eq!(
        SslMode::parse(Some("verify-full")).unwrap(),
        SslMode::VerifyFull
    );
    assert!(SslMode::parse(Some("bogus")).is_err());
}

#[test]
fn name_errors_are_recognized() {
    assert!(is_name_error(&TlsError::InvalidCertificate(
        CertificateError::NotValidForName
    )));
    assert!(!is_name_error(&TlsError::InvalidCertificate(
        CertificateError::UnknownIssuer
    )));
    assert!(!is_name_error(&TlsError::InvalidCertificate(
        CertificateError::Expired
    )));
}

#[test]
fn unverified_modes_build_without_files() {
    for m in [SslMode::Disable, SslMode::Prefer, SslMode::Require] {
        assert!(build_config(m, None, None).is_ok());
    }
}

#[test]
fn verify_modes_reject_garbage_ca() {
    assert!(build_config(SslMode::VerifyFull, Some(b"not pem"), None).is_err());
}

#[test]
fn client_cert_needs_both_files() {
    assert!(build_connector(SslMode::Require, None, Some("/x.crt"), None).is_err());
    assert!(build_connector(SslMode::Require, None, None, Some("/x.key")).is_err());
}

#[test]
fn missing_ca_file_is_reported() {
    let err = build_connector(SslMode::VerifyFull, Some("/no/such/ca.crt"), None, None)
        .err()
        .unwrap();
    assert!(err.message.contains("ssl_ca"));
}

#[test]
fn rejects_non_key_pem() {
    assert!(parse_private_key(b"nothing here").is_err());
}
