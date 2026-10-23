use super::*;

#[test]
fn parses_basic_uri() {
    let p = parse_connection_uri("cockroachdb://root@localhost:26257/mydb?sslmode=verify-full")
        .unwrap();
    assert_eq!(p.user.as_deref(), Some("root"));
    assert_eq!(p.host.as_deref(), Some("localhost"));
    assert_eq!(p.port, 26257);
    assert_eq!(p.database, "mydb");
    assert_eq!(p.sslmode.as_deref(), Some("verify-full"));
    assert_eq!(p.password, None);
}

#[test]
fn defaults_port_and_database() {
    let p = parse_connection_uri("cockroach://root@example.com").unwrap();
    assert_eq!(p.port, 26257);
    assert_eq!(p.database, "defaultdb");
    let p = parse_connection_uri("postgresql://root@example.com/").unwrap();
    assert_eq!(p.port, 26257);
    assert_eq!(p.database, "defaultdb");
}

#[test]
fn accepts_all_schemes() {
    for s in ["postgresql", "postgres", "cockroachdb", "cockroach"] {
        assert!(parse_connection_uri(&format!("{s}://h")).is_ok(), "{s}");
    }
    assert!(parse_connection_uri("mysql://h").is_err());
    assert!(parse_connection_uri("localhost:26257").is_err());
}

#[test]
fn decodes_userinfo_and_path() {
    let p = parse_connection_uri("postgres://we%40ird:p%3Aa%2Fss@h:1/my%20db").unwrap();
    assert_eq!(p.user.as_deref(), Some("we@ird"));
    assert_eq!(p.password.as_deref(), Some("p:a/ss"));
    assert_eq!(p.database, "my db");
    assert_eq!(p.port, 1);
}

#[test]
fn decodes_cluster_routing_options_once() {
    let p = parse_connection_uri(
        "postgresql://u:pw@free-tier.gcp.cockroachlabs.cloud:26257/defaultdb?sslmode=verify-full&options=--cluster%3Dmy-cluster-1234",
    )
    .unwrap();
    assert_eq!(p.options.as_deref(), Some("--cluster=my-cluster-1234"));
    assert_eq!(p.password.as_deref(), Some("pw"));
}

#[test]
fn reads_tls_paths_and_app_name() {
    let p = parse_connection_uri(
        "cockroachdb://root@h/db?sslrootcert=/ca.crt&sslcert=/c.crt&sslkey=/c.key&application_name=my%20app",
    )
    .unwrap();
    assert_eq!(p.sslrootcert.as_deref(), Some("/ca.crt"));
    assert_eq!(p.sslcert.as_deref(), Some("/c.crt"));
    assert_eq!(p.sslkey.as_deref(), Some("/c.key"));
    assert_eq!(p.application_name.as_deref(), Some("my app"));
}

#[test]
fn user_and_password_query_fallbacks() {
    let p = parse_connection_uri("cockroachdb://h/db?user=bob&password=s3").unwrap();
    assert_eq!(p.user.as_deref(), Some("bob"));
    assert_eq!(p.password.as_deref(), Some("s3"));
    // userinfo wins
    let p = parse_connection_uri("cockroachdb://alice@h/db?user=bob").unwrap();
    assert_eq!(p.user.as_deref(), Some("alice"));
}

#[test]
fn ignores_unknown_params() {
    let p = parse_connection_uri("cockroachdb://root@h/db?foo=bar&sslmode=disable").unwrap();
    assert_eq!(p.sslmode.as_deref(), Some("disable"));
}

#[test]
fn parses_ipv6_hosts() {
    let p = parse_connection_uri("cockroachdb://root@[::1]:26258/db").unwrap();
    assert_eq!(p.host.as_deref(), Some("::1"));
    assert_eq!(p.port, 26258);
    let p = parse_connection_uri("cockroachdb://root@[::1]/db").unwrap();
    assert_eq!(p.port, 26257);
}

#[test]
fn rejects_bad_port() {
    assert!(parse_connection_uri("cockroachdb://root@h:abc/db").is_err());
    assert!(parse_connection_uri("cockroachdb://root@h:99999/db").is_err());
}

#[test]
fn uses_first_host_of_list() {
    let p = parse_connection_uri("cockroachdb://root@a:1,b:2/db").unwrap();
    assert_eq!(p.host.as_deref(), Some("a"));
    assert_eq!(p.port, 1);
}
