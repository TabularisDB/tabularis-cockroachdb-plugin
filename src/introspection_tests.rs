use super::*;

fn idx(name: &str, seq: i64, col: &str) -> IndexRow {
    IndexRow {
        name: name.into(),
        non_unique: false,
        seq,
        column: col.into(),
        definition: col.into(),
        storing: false,
        implicit: false,
    }
}

#[test]
fn pk_join_includes_table_name() {
    for f in [TableFilter::One, TableFilter::Any, TableFilter::All] {
        let sql = columns_sql(f);
        assert!(sql.contains("AND tc.table_name = kcu.table_name"), "{f:?}");
        assert!(sql.contains("pk.table_name = c.table_name"), "{f:?}");
    }
}

#[test]
fn columns_use_requested_schema_and_hide_hidden() {
    let sql = columns_sql(TableFilter::One);
    assert!(sql.contains("c.table_schema = $1") && sql.contains("tc.table_schema = $1"));
    assert!(sql.contains("c.is_hidden = 'NO'"));
    assert!(!sql.contains("'public'"));
}

#[test]
fn batch_form_selects_and_orders_by_table() {
    let sql = columns_sql(TableFilter::Any);
    assert!(sql.starts_with("SELECT c.table_name"));
    assert!(sql.contains("ORDER BY c.table_name, c.ordinal_position"));
    assert!(sql.contains("c.table_name = ANY($2)") && sql.contains("tc.table_name = ANY($2)"));
    assert!(!columns_sql(TableFilter::All).contains("$2"));
}

#[test]
fn fk_query_pairs_columns_by_position() {
    let sql = foreign_keys_sql(TableFilter::One);
    assert!(sql.contains("unnest(c.conkey, c.confkey) WITH ORDINALITY"));
    assert!(sql.contains("a.attnum = k.col") && sql.contains("ra.attnum = k.ref_col"));
    assert!(sql.contains("ns.nspname = $1 AND cl.relname = $2"));
    assert!(foreign_keys_sql(TableFilter::Any).contains("cl.relname = ANY($2)"));
}

#[test]
fn fk_actions() {
    assert_eq!(fk_action("a"), "NO ACTION");
    assert_eq!(fk_action("r"), "RESTRICT");
    assert_eq!(fk_action("c"), "CASCADE");
    assert_eq!(fk_action("n"), "SET NULL");
    assert_eq!(fk_action("d"), "SET DEFAULT");
}

#[test]
fn composite_fk_keeps_one_entry_per_column() {
    let rows = [
        FkRow {
            name: "fk".into(),
            position: 1,
            column: "a".into(),
            ref_column: "x".into(),
            ref_table: "t".into(),
            ..Default::default()
        },
        FkRow {
            name: "fk".into(),
            position: 2,
            column: "b".into(),
            ref_column: "y".into(),
            ref_table: "t".into(),
            ..Default::default()
        },
    ];
    let out: Vec<_> = rows.iter().map(fk_json).collect();
    assert_eq!(
        (
            out[0]["column_name"].as_str(),
            out[0]["ref_column"].as_str()
        ),
        (Some("a"), Some("x"))
    );
    assert_eq!(
        (
            out[1]["column_name"].as_str(),
            out[1]["ref_column"].as_str()
        ),
        (Some("b"), Some("y"))
    );
    assert_eq!(out[1]["seq_in_fk"], 2);
    assert_eq!(out[0]["name"], out[1]["name"]);
}

#[test]
fn base_type_strips_modifiers() {
    assert_eq!(base_type_name("DECIMAL(10,2)"), "DECIMAL");
    assert_eq!(base_type_name("VARCHAR(50)"), "VARCHAR");
    assert_eq!(base_type_name("VARCHAR(50)[]"), "VARCHAR[]");
    assert_eq!(base_type_name("INT8"), "INT8");
    assert_eq!(base_type_name("STRING[]"), "STRING[]");
}

#[test]
fn auto_increment_detection() {
    assert!(is_auto_increment(Some("unique_rowid()"), false));
    assert!(is_auto_increment(Some("nextval('s'::REGCLASS)"), false));
    assert!(is_auto_increment(Some("gen_random_uuid()"), false));
    assert!(is_auto_increment(None, true));
    assert!(!is_auto_increment(Some("'x'::STRING"), false));
    assert!(!is_auto_increment(None, false));
}

#[test]
fn column_json_shapes() {
    let r = ColumnRow {
        name: "price".into(),
        sql_type: "DECIMAL(10,2)".into(),
        data_type: "numeric".into(),
        nullable: "NO".into(),
        num_precision: Some(10),
        num_scale: Some(2),
        is_pk: true,
        comment: Some(String::new()),
        ..Default::default()
    };
    let j = column_json(&r);
    assert_eq!(j["data_type"], "DECIMAL");
    assert_eq!(j["is_nullable"], false);
    assert_eq!(j["is_pk"], true);
    assert_eq!(j["numeric_precision"], 10);
    assert!(j["comment"].is_null());

    let e = ColumnRow {
        data_type: "USER-DEFINED".into(),
        udt_name: "mood".into(),
        sql_type: "mood".into(),
        nullable: "YES".into(),
        ..Default::default()
    };
    assert_eq!(column_json(&e)["data_type"], "ENUM(mood)");
    assert_eq!(column_json(&e)["is_nullable"], true);
}

fn cols(entries: &[Value], index: &str) -> Vec<String> {
    entries
        .iter()
        .filter(|e| e["name"] == index)
        .map(|e| e["column_name"].as_str().unwrap().to_string())
        .collect()
}

#[test]
fn one_entry_per_key_column_in_key_order() {
    let rows = vec![idx("b", 2, "y"), idx("a", 1, "x"), idx("b", 1, "z")];
    let out = index_entries(rows, None);
    assert_eq!(out.len(), 3);
    assert_eq!(cols(&out, "b"), ["z", "y"]);
    assert_eq!(cols(&out, "a"), ["x"]);
    let b: Vec<_> = out.iter().filter(|e| e["name"] == "b").collect();
    assert_eq!(
        (b[0]["seq_in_index"].as_i64(), b[1]["seq_in_index"].as_i64()),
        (Some(1), Some(2))
    );
}

#[test]
fn entries_match_the_host_index_shape() {
    // The host deserializes `name`, `column_name`, `is_unique`, `is_primary`,
    // `seq_in_index` (and optional `is_expression`); anything else is ignored,
    // but a missing or mistyped one fails the whole table.
    let e = &index_entries(vec![idx("i", 1, "c")], None)[0];
    assert!(e["name"].is_string() && e["column_name"].is_string());
    assert!(e["is_unique"].is_boolean() && e["is_primary"].is_boolean());
    assert!(e["seq_in_index"].is_i64() && e["is_expression"].is_boolean());
    assert!(e.get("columns").is_none());
}

#[test]
fn skips_implicit_storing_and_shard_columns() {
    let mut implicit = idx("i", 2, "id");
    implicit.implicit = true;
    let mut storing = idx("i", 3, "extra");
    storing.storing = true;
    let shard = idx("i", 0, "crdb_internal_email_shard_16");
    let out = index_entries(vec![idx("i", 1, "email"), implicit, storing, shard], None);
    assert_eq!(cols(&out, "i"), ["email"]);
    assert_eq!(out[0]["seq_in_index"], 1);
}

#[test]
fn expression_indexes_show_the_expression() {
    let mut e = idx("idx_expr", 1, "crdb_internal_idx_expr");
    e.definition = "(lower(email))".into();
    let out = index_entries(vec![e], None);
    assert_eq!(out[0]["column_name"], "(lower(email))");
    assert_eq!(out[0]["is_expression"], true);
}

#[test]
fn reports_uniqueness() {
    let mut r = idx("i", 1, "c");
    r.non_unique = true;
    let out = index_entries(vec![r, idx("u", 1, "c")], None);
    assert_eq!(
        out.iter().find(|e| e["name"] == "i").unwrap()["is_unique"],
        false
    );
    assert_eq!(
        out.iter().find(|e| e["name"] == "u").unwrap()["is_unique"],
        true
    );
}

#[test]
fn primary_index_detection() {
    assert!(is_primary_index("users_pkey", Some("users_pkey")));
    assert!(!is_primary_index("users_pkey", Some("other_pkey")));
    assert!(is_primary_index("primary", None));
    assert!(is_primary_index("t_pkey", None));
    assert!(!is_primary_index("idx_email", None));
}

#[test]
fn show_tables_quotes_identifiers() {
    let sql = show_tables_sql("my\"db", "s");
    assert!(sql.contains("\"my\"\"db\".\"s\""));
    assert_eq!(qualified("a", "b\"c"), "\"a\".\"b\"\"c\"");
}

#[test]
fn comment_joins_pin_the_catalog() {
    assert!(TABLE_COMMENTS_SQL.contains("d.classoid = 'pg_catalog.pg_class'::REGCLASS"));
    assert!(columns_sql(TableFilter::One).contains("d.classoid = 'pg_catalog.pg_class'::REGCLASS"));
}

#[test]
fn integer_bit_width_is_not_a_precision() {
    let r = ColumnRow {
        sql_type: "INT8".into(),
        data_type: "bigint".into(),
        nullable: "NO".into(),
        num_precision: Some(64),
        num_scale: Some(0),
        ..Default::default()
    };
    let j = column_json(&r);
    assert!(j["numeric_precision"].is_null() && j["numeric_scale"].is_null());
}
