use bytes::BytesMut;
use serde_json::{json, Map};

use super::*;

fn cols(spec: &[(&str, &str)]) -> Columns {
    spec.iter()
        .map(|(n, t)| {
            (
                n.to_string(),
                ColumnInfo {
                    sql_type: t.to_string(),
                    enum_type: None,
                },
            )
        })
        .collect()
}

fn obj(v: Value) -> Map<String, Value> {
    v.as_object().unwrap().clone()
}

/// The bytes that would go on the wire for parameter `i`.
fn wire(s: &Statement, i: usize) -> Vec<u8> {
    let mut out = BytesMut::new();
    s.params[i].to_sql_checked(&s.types[i], &mut out).unwrap();
    out.to_vec()
}

fn users() -> Columns {
    cols(&[
        ("id", "INT8"),
        ("name", "STRING"),
        ("price", "DECIMAL(10,2)"),
        ("active", "BOOL"),
        ("rowid", "INT8"),
    ])
}

// ---- insert ---------------------------------------------------------------

#[test]
fn insert_lists_only_the_given_columns() {
    let s = build_insert("app", "users", &obj(json!({"name": "Ann"})), &users(), None).unwrap();
    assert_eq!(s.sql, r#"INSERT INTO "app"."users" ("name") VALUES ($1)"#);
    assert_eq!(s.types, [Type::TEXT]);
    assert_eq!(wire(&s, 0), b"Ann");
}

#[test]
fn insert_with_no_columns_uses_default_values() {
    let s = build_insert("app", "users", &Map::new(), &users(), None).unwrap();
    assert_eq!(s.sql, r#"INSERT INTO "app"."users" DEFAULT VALUES"#);
    assert!(s.params.is_empty());
}

#[test]
fn insert_numbers_placeholders_across_values_that_bind_nothing() {
    // NULL and DEFAULT take no parameter, so the next value is still $2.
    let data = obj(json!({"active": true, "id": null, "name": "__USE_DEFAULT__", "price": "9.50"}));
    let s = build_insert("app", "users", &data, &users(), None).unwrap();
    let placeholders: Vec<String> = (1..=s.params.len()).map(|n| format!("${n}")).collect();
    for p in &placeholders {
        assert_eq!(
            s.sql.matches(p.as_str()).count(),
            1,
            "{p} must appear once in {}",
            s.sql
        );
    }
    assert!(s.sql.contains("NULL") && s.sql.contains("DEFAULT"));
    assert_eq!(s.params.len(), s.types.len());
    assert_eq!(
        s.params.len(),
        2,
        "only `active` and `price` bind a parameter"
    );
}

#[test]
fn insert_keeps_decimal_digits_and_big_int_keys_exact() {
    let data = obj(json!({"id": "1152921504606846977", "price": "123456789012345678.90"}));
    let s = build_insert("s", "t", &data, &users(), None).unwrap();
    let i = s.sql.find("\"id\"").unwrap() < s.sql.find("\"price\"").unwrap();
    assert!(i, "{}", s.sql);
    assert_eq!(wire(&s, 0), 1_152_921_504_606_846_977i64.to_be_bytes());
    assert!(s.sql.contains("CAST($2 AS DECIMAL(10,2))"), "{}", s.sql);
    assert_eq!(wire(&s, 1), b"123456789012345678.90");
}

#[test]
fn insert_rejects_a_column_that_does_not_exist() {
    let err =
        build_insert("app", "users", &obj(json!({"nme": "Ann"})), &users(), None).unwrap_err();
    assert!(
        err.message
            .contains("column \"nme\" does not exist in app.users"),
        "{}",
        err.message
    );
}

#[test]
fn insert_quotes_hostile_identifiers() {
    let mut c = users();
    c.insert(
        "we\"ird".into(),
        ColumnInfo {
            sql_type: "STRING".into(),
            enum_type: None,
        },
    );
    let s = build_insert(
        "sch\"ema",
        "ta\"ble",
        &obj(json!({"we\"ird": "x"})),
        &c,
        None,
    )
    .unwrap();
    assert_eq!(
        s.sql,
        r#"INSERT INTO "sch""ema"."ta""ble" ("we""ird") VALUES ($1)"#
    );
}

#[test]
fn insert_casts_enums_and_binds_blobs_from_the_wire_format() {
    let mut c = cols(&[("m", "STRING"), ("data", "BYTES")]);
    c.get_mut("m").unwrap().enum_type = Some("\"app\".\"mood\"".into());
    let blob = crate::utils::blob::encode_blob_full(&[1, 2, 3]);
    let s = build_insert(
        "app",
        "t",
        &obj(json!({"m": "happy", "data": blob})),
        &c,
        None,
    )
    .unwrap();
    assert!(
        s.sql.contains("CAST($2 AS \"app\".\"mood\")")
            || s.sql.contains("CAST($1 AS \"app\".\"mood\")"),
        "{}",
        s.sql
    );
    assert!(s.types.contains(&Type::BYTEA) && s.types.contains(&Type::TEXT));
}

#[test]
fn a_bad_value_names_its_column() {
    let err = build_insert("s", "t", &obj(json!({"active": "maybe"})), &users(), None).unwrap_err();
    assert!(err.message.contains("column \"active\""), "{}", err.message);
}

// ---- update ---------------------------------------------------------------

#[test]
fn update_sets_one_column_where_the_key_matches() {
    let s = build_update(
        "app",
        "users",
        &obj(json!({"id": 42})),
        "name",
        json!("Robert"),
        &users(),
        None,
    )
    .unwrap();
    assert_eq!(
        s.sql,
        r#"UPDATE "app"."users" SET "name" = $1 WHERE "id" = $2"#
    );
    assert_eq!(s.types, [Type::TEXT, Type::INT8]);
    assert_eq!(wire(&s, 0), b"Robert");
    assert_eq!(wire(&s, 1), 42i64.to_be_bytes());
}

#[test]
fn update_to_null_or_default_shifts_the_key_placeholders() {
    let s = build_update(
        "s",
        "t",
        &obj(json!({"id": 7})),
        "name",
        Value::Null,
        &users(),
        None,
    )
    .unwrap();
    assert_eq!(s.sql, r#"UPDATE "s"."t" SET "name" = NULL WHERE "id" = $1"#);
    let s = build_update(
        "s",
        "t",
        &obj(json!({"id": 7})),
        "name",
        json!("__USE_DEFAULT__"),
        &users(),
        None,
    )
    .unwrap();
    assert_eq!(
        s.sql,
        r#"UPDATE "s"."t" SET "name" = DEFAULT WHERE "id" = $1"#
    );
    assert_eq!(s.params.len(), 1);
}

#[test]
fn update_with_a_composite_key_matches_every_key_column() {
    let c = cols(&[("a", "INT8"), ("b", "STRING"), ("v", "STRING")]);
    let s = build_update(
        "s",
        "t",
        &obj(json!({"b": "x", "a": 1})),
        "v",
        json!("new"),
        &c,
        None,
    )
    .unwrap();
    // Keys are sorted, so the SQL is stable whatever order the host sent them in.
    assert_eq!(
        s.sql,
        r#"UPDATE "s"."t" SET "v" = $1 WHERE "a" = $2 AND "b" = $3"#
    );
    assert_eq!(s.params.len(), 3);
}

#[test]
fn a_null_key_value_is_is_null_not_equals_null() {
    let c = cols(&[("a", "INT8"), ("b", "STRING"), ("v", "STRING")]);
    let s = build_delete("s", "t", &obj(json!({"a": 1, "b": null})), &c).unwrap();
    assert_eq!(
        s.sql,
        r#"DELETE FROM "s"."t" WHERE "a" = $1 AND "b" IS NULL"#
    );
    assert_eq!(s.params.len(), 1);
}

#[test]
fn an_int8_key_sent_as_a_string_is_bound_exactly() {
    let s = build_update(
        "s",
        "t",
        &obj(json!({"id": "1152921504606846977"})),
        "name",
        json!("x"),
        &users(),
        None,
    )
    .unwrap();
    assert_eq!(s.types[1], Type::INT8);
    assert_eq!(wire(&s, 1), 1_152_921_504_606_846_977i64.to_be_bytes());
}

#[test]
fn the_hidden_rowid_can_be_the_key() {
    let s = build_update(
        "s",
        "t",
        &obj(json!({"rowid": "999"})),
        "name",
        json!("x"),
        &users(),
        None,
    )
    .unwrap();
    assert_eq!(
        s.sql,
        r#"UPDATE "s"."t" SET "name" = $1 WHERE "rowid" = $2"#
    );
}

#[test]
fn update_and_delete_refuse_an_empty_row_key() {
    let e = build_update("s", "t", &Map::new(), "name", json!("x"), &users(), None).unwrap_err();
    assert!(
        e.message.contains("pk_map must not be empty"),
        "{}",
        e.message
    );
    let e = build_delete("s", "t", &Map::new(), &users()).unwrap_err();
    assert!(
        e.message.contains("refusing to match every row"),
        "{}",
        e.message
    );
}

#[test]
fn update_rejects_unknown_columns_in_the_key_and_in_set() {
    let e = build_update(
        "s",
        "t",
        &obj(json!({"idd": 1})),
        "name",
        json!("x"),
        &users(),
        None,
    )
    .unwrap_err();
    assert!(e.message.contains("\"idd\" does not exist"));
    let e = build_update(
        "s",
        "t",
        &obj(json!({"id": 1})),
        "nope",
        json!("x"),
        &users(),
        None,
    )
    .unwrap_err();
    assert!(e.message.contains("\"nope\" does not exist"));
}

#[test]
fn update_with_the_default_sentinel_in_a_key_is_just_text() {
    // The sentinel only means DEFAULT for a value being set, never for a key.
    let c = cols(&[("k", "STRING"), ("v", "STRING")]);
    let s = build_update(
        "s",
        "t",
        &obj(json!({"k": "__USE_DEFAULT__"})),
        "v",
        json!("x"),
        &c,
        None,
    )
    .unwrap();
    assert_eq!(s.sql, r#"UPDATE "s"."t" SET "v" = $1 WHERE "k" = $2"#);
    assert_eq!(wire(&s, 1), b"__USE_DEFAULT__");
}

// ---- delete and blob select --------------------------------------------------

#[test]
fn delete_matches_the_row_key() {
    let s = build_delete("app", "users", &obj(json!({"id": 42})), &users()).unwrap();
    assert_eq!(s.sql, r#"DELETE FROM "app"."users" WHERE "id" = $1"#);
    assert_eq!(wire(&s, 0), 42i64.to_be_bytes());
}

#[test]
fn blob_select_reads_one_cell_by_key() {
    let c = cols(&[("id", "INT8"), ("data", "BYTES")]);
    let s = build_blob_select("app", "docs", "data", &obj(json!({"id": 7})), &c).unwrap();
    assert_eq!(s.sql, r#"SELECT "data" FROM "app"."docs" WHERE "id" = $1"#);
    assert!(build_blob_select("app", "docs", "nope", &obj(json!({"id": 7})), &c).is_err());
    assert!(build_blob_select("app", "docs", "data", &Map::new(), &c).is_err());
}

// ---- BLOB_FILE_REF and sizes ---------------------------------------------------

#[test]
fn a_picked_file_is_read_and_bound_as_bytes() {
    let dir = std::env::temp_dir().join(format!("crud-ref-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("pic.bin");
    std::fs::write(&file, [5u8, 6, 7, 8]).unwrap();
    let c = cols(&[("id", "INT8"), ("data", "BYTES")]);
    let value = format!(
        "BLOB_FILE_REF:4:application/octet-stream:{}",
        file.display()
    );
    let s = build_update(
        "s",
        "t",
        &obj(json!({"id": 1})),
        "data",
        json!(value),
        &c,
        Some(1024),
    )
    .unwrap();
    assert_eq!(s.types[0], Type::BYTEA);
    assert_eq!(wire(&s, 0), [5, 6, 7, 8]);
    // Over the host's cap: an error naming the column, not the path stored as text.
    let e = build_update(
        "s",
        "t",
        &obj(json!({"id": 1})),
        "data",
        json!(value),
        &c,
        Some(2),
    )
    .unwrap_err();
    assert!(
        e.message.contains("column \"data\"") && e.message.contains("exceeds"),
        "{}",
        e.message
    );
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn a_missing_picked_file_is_an_error_not_a_stored_path() {
    let c = cols(&[("id", "INT8"), ("data", "BYTES")]);
    let e = build_insert(
        "s",
        "t",
        &obj(json!({"data": "BLOB_FILE_REF:1:x/y:/no/such/file"})),
        &c,
        None,
    )
    .unwrap_err();
    assert!(e.message.contains("cannot read"), "{}", e.message);
}

// ---- paths ---------------------------------------------------------------------

#[test]
fn writable_path_checks() {
    assert!(validate_writable_path("").is_err());
    assert!(validate_writable_path("   ").is_err());
    assert!(
        validate_writable_path(&std::env::temp_dir().display().to_string()).is_err(),
        "a directory"
    );
    assert!(validate_writable_path("/no/such/dir/file.bin").is_err());
    assert!(
        validate_writable_path(&std::env::temp_dir().join("ok.bin").display().to_string()).is_ok()
    );
    assert!(validate_writable_path("relative.bin").is_ok());
}
