use super::*;

#[test]
fn parses_release_builds() {
    let v = parse_crdb_version(
        "CockroachDB CCL v24.3.4 (x86_64-pc-linux-gnu, built 2025/01/15 10:00:00, go1.22.8)",
    );
    assert_eq!(v, Some(CrdbVersion::new(24, 3, 4)));
    let v =
        parse_crdb_version("CockroachDB OSS v23.1.30 (aarch64-unknown-linux-gnu, built x, go1.21)");
    assert_eq!(v, Some(CrdbVersion::new(23, 1, 30)));
}

#[test]
fn parses_prerelease_builds() {
    let v = parse_crdb_version("CockroachDB CCL v25.2.0-beta.1 (x86_64-apple-darwin, built x)");
    assert_eq!(v, Some(CrdbVersion::new(25, 2, 0)));
    let v = parse_crdb_version("CockroachDB CCL v25.3.0-alpha.00000000 (x, built x)");
    assert_eq!(v, Some(CrdbVersion::new(25, 3, 0)));
}

#[test]
fn rejects_other_servers() {
    assert_eq!(
        parse_crdb_version("PostgreSQL 16.2 on x86_64-pc-linux-gnu, compiled by gcc"),
        None
    );
    assert!(!is_cockroachdb("PostgreSQL 16.2"));
    assert_eq!(parse_crdb_version(""), None);
    assert_eq!(parse_crdb_version("CockroachDB CCL (no version)"), None);
}

#[test]
fn versions_are_ordered() {
    assert!(CrdbVersion::new(24, 3, 0) > CrdbVersion::new(24, 2, 9));
    assert!(CrdbVersion::new(25, 1, 0) > CrdbVersion::new(24, 3, 9));
}

#[test]
fn feature_gates() {
    let v = CrdbVersion::new(24, 3, 4);
    assert!(v.supports_udf() && v.supports_procedures() && v.supports_triggers());
    assert!(!v.supports_vector_index());
    assert!(!CrdbVersion::new(22, 1, 0).supports_udf());
    assert!(CrdbVersion::new(23, 2, 0).supports_procedures());
    assert!(!CrdbVersion::new(23, 1, 9).supports_procedures());
}

#[test]
fn display_is_stable() {
    assert_eq!(CrdbVersion::new(24, 3, 4).to_string(), "v24.3.4");
}
