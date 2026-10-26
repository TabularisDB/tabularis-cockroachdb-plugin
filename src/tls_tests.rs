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

// ---- PEM parsing (rustls-pki-types `PemObject`, replacing rustls-pemfile) ----
//
// Certificates and keys are generated per test with `rcgen`, so no key
// material is committed. `rcgen` writes PKCS#8 keys; the PKCS#1 form that
// `cockroach cert create-client` writes is parsed by the same `PrivateKeyDer`
// code and was checked against a real secure cluster.

/// A fresh self-signed certificate and its PKCS#8 key, both as PEM.
fn generated() -> (Vec<u8>, Vec<u8>) {
    let key = rcgen::KeyPair::generate().unwrap();
    let cert = rcgen::CertificateParams::new(vec!["pem-test".into()])
        .unwrap()
        .self_signed(&key)
        .unwrap();
    (cert.pem().into_bytes(), key.serialize_pem().into_bytes())
}

#[test]
fn parses_a_single_certificate() {
    let (cert, _) = generated();
    let certs = parse_certs(&cert, "ssl_cert").unwrap();
    assert_eq!(certs.len(), 1);
    assert!(!certs[0].is_empty());
}

#[test]
fn parses_a_certificate_chain_in_order() {
    let (a, _) = generated();
    let (b, _) = generated();
    let bundle = [a.as_slice(), b"\n", b.as_slice()].concat();
    let certs = parse_certs(&bundle, "ssl_ca").unwrap();
    assert_eq!(certs.len(), 2);
    assert_ne!(certs[0].as_ref(), certs[1].as_ref());
    assert_eq!(certs[0].as_ref(), parse_certs(&a, "x").unwrap()[0].as_ref());
    assert_eq!(certs[1].as_ref(), parse_certs(&b, "x").unwrap()[0].as_ref());
}

#[test]
fn certificate_parsing_skips_a_key_in_the_same_file() {
    // Client cert and key are often kept in one PEM file.
    let (cert, key) = generated();
    let combined = [cert.as_slice(), b"\n", key.as_slice()].concat();
    assert_eq!(parse_certs(&combined, "ssl_cert").unwrap().len(), 1);
}

#[test]
fn rejects_files_with_no_certificate() {
    let (_, key) = generated();
    let err = parse_certs(&key, "ssl_ca").unwrap_err();
    assert!(
        err.message.contains("no certificates found in ssl_ca"),
        "{}",
        err.message
    );
    assert!(parse_certs(b"", "ssl_ca").is_err());
    assert!(parse_certs(b"not pem at all", "ssl_ca").is_err());
}

#[test]
fn rejects_a_corrupt_certificate_block() {
    let bad = b"-----BEGIN CERTIFICATE-----\n!!!not base64!!!\n-----END CERTIFICATE-----\n";
    let err = parse_certs(bad, "ssl_cert").unwrap_err();
    assert!(
        err.message.contains("invalid ssl_cert PEM"),
        "{}",
        err.message
    );
}

#[test]
fn parses_a_pkcs8_private_key() {
    let (_, key) = generated();
    assert!(matches!(
        parse_private_key(&key).unwrap(),
        PrivateKeyDer::Pkcs8(_)
    ));
}

#[test]
fn finds_the_key_in_a_combined_file() {
    let (cert, key) = generated();
    let combined = [cert.as_slice(), b"\n", key.as_slice()].concat();
    assert!(matches!(
        parse_private_key(&combined).unwrap(),
        PrivateKeyDer::Pkcs8(_)
    ));
}

#[test]
fn private_key_errors_are_distinct() {
    let (cert, _) = generated();
    let none = parse_private_key(&cert).unwrap_err();
    assert!(
        none.message.contains("no private key found in ssl_key"),
        "{}",
        none.message
    );
    assert!(parse_private_key(b"").is_err());
    let bad = b"-----BEGIN PRIVATE KEY-----\n!!!!\n-----END PRIVATE KEY-----\n";
    assert!(parse_private_key(bad)
        .unwrap_err()
        .message
        .contains("invalid client key PEM"));
}

#[test]
fn a_parsed_certificate_and_key_build_a_client_config() {
    let (cert, key) = generated();
    let cfg = build_config(SslMode::Require, None, Some((&cert, &key)));
    assert!(cfg.is_ok(), "{:?}", cfg.err().map(|e| e.message));
}

#[test]
fn a_ca_bundle_builds_a_verifying_config() {
    let (a, _) = generated();
    let (b, _) = generated();
    let bundle = [a.as_slice(), b"\n", b.as_slice()].concat();
    assert!(build_config(SslMode::VerifyFull, Some(&bundle), None).is_ok());
    assert!(build_config(SslMode::VerifyCa, Some(&bundle), None).is_ok());
}
