use serde_json::json;

use super::*;

fn col(name: &str, ty: &str) -> ColumnDef {
    ColumnDef {
        name: name.into(),
        data_type: ty.into(),
        is_nullable: true,
        ..Default::default()
    }
}

fn pk(mut c: ColumnDef) -> ColumnDef {
    c.is_pk = true;
    c.is_nullable = false;
    c
}

fn err(r: Result<impl std::fmt::Debug, PluginError>) -> String {
    r.unwrap_err().message
}

// ---- ColumnDef parsing -------------------------------------------------------------

#[test]
fn parses_the_hosts_column_definition() {
    let c = ColumnDef::from_value(&json!({
        "name": "email", "data_type": "VARCHAR(255)", "is_nullable": false, "is_pk": false,
        "is_auto_increment": false, "default_value": "'x'", "comment": "Primary contact"
    }))
    .unwrap();
    assert_eq!(
        (c.name.as_str(), c.data_type.as_str(), c.is_nullable),
        ("email", "VARCHAR(255)", false)
    );
    assert_eq!(
        (c.default_value.as_deref(), c.comment.as_deref()),
        (Some("'x'"), Some("Primary contact"))
    );
}

#[test]
fn blank_default_and_comment_mean_none() {
    let c = ColumnDef::from_value(
        &json!({"name": "a", "data_type": "INT8", "default_value": "  ", "comment": ""}),
    )
    .unwrap();
    assert_eq!((c.default_value, c.comment), (None, None));
    assert!(c.is_nullable, "nullable unless said otherwise");
}

#[test]
fn rejects_a_column_without_a_name_or_type() {
    assert!(ColumnDef::from_value(&json!({"data_type": "INT8"})).is_err());
    assert!(ColumnDef::from_value(&json!({"name": "a"})).is_err());
    assert!(ColumnDef::from_value(&json!({"name": " ", "data_type": "INT8"})).is_err());
    assert!(ColumnDef::from_value(&json!("a")).is_err());
}

// ---- validating free text -----------------------------------------------------------

#[test]
fn accepts_the_types_the_editor_offers() {
    for t in [
        "INT8",
        "VARCHAR(255)",
        "DECIMAL(10,2)",
        "STRING[]",
        "TIMESTAMPTZ",
        "app.mood",
        "\"my type\"",
        "DOUBLE PRECISION",
        "BIT(8)",
        "  INT4 ",
    ] {
        assert!(check_type(t).is_ok(), "{t}");
    }
}

#[test]
fn rejects_types_that_could_carry_sql() {
    for t in [
        "",
        "   ",
        "INT8); DROP TABLE x; --",
        "INT8;",
        "INT8 -- c",
        "VARCHAR(5",
        "INT8)",
        "\"open",
        "A'B",
        "INT8/*",
        &"X".repeat(300),
    ] {
        assert!(check_type(t).is_err(), "{t:?}");
    }
}

#[test]
fn accepts_ordinary_default_expressions() {
    for e in [
        "now()",
        "'new'",
        "gen_random_uuid()",
        "unique_rowid()",
        "0",
        "true",
        "current_timestamp()",
        "(a + 1)",
        "'it''s'",
        "ARRAY[1,2]",
        "'a;b'",
        "'-- not a comment'",
    ] {
        assert!(check_expression(e, "a default").is_ok(), "{e}");
    }
}

#[test]
fn rejects_default_expressions_that_break_out() {
    for e in [
        "",
        "  ",
        "1; DROP TABLE t",
        "1 -- c",
        "1 /* c */",
        "'open",
        "(1",
        "1)",
        "now() ; x",
        "$$ open",
    ] {
        assert!(check_expression(e, "a default").is_err(), "{e:?}");
    }
}

#[test]
fn a_view_definition_is_one_statement() {
    assert_eq!(check_definition("SELECT 1").unwrap(), "SELECT 1");
    assert_eq!(check_definition("  SELECT 1 ;; ").unwrap(), "SELECT 1");
    assert_eq!(check_definition("SELECT 1 -- note\n").unwrap(), "SELECT 1");
    assert_eq!(
        check_definition("SELECT ';' FROM t").unwrap(),
        "SELECT ';' FROM t"
    );
    assert_eq!(
        check_definition("SELECT 1 /* c */ + 2").unwrap(),
        "SELECT 1 /* c */ + 2"
    );
    for bad in [
        "",
        "   ",
        ";",
        "-- only a comment",
        "SELECT 1; DROP TABLE t",
        "SELECT 'open",
        "SELECT 1 /* open",
    ] {
        assert!(check_definition(bad).is_err(), "{bad:?}");
    }
}

// ---- create table --------------------------------------------------------------------

#[test]
fn create_table_names_the_primary_key_constraint_after_the_table() {
    let mut id = pk(col("id", "UUID"));
    id.default_value = Some("gen_random_uuid()".into());
    let mut name = col("name", "STRING");
    name.is_nullable = false;
    let mut created = col("created_at", "TIMESTAMPTZ");
    created.is_nullable = false;
    created.default_value = Some("now()".into());
    let sql = build_create_table("app", "users", &[id, name, created]).unwrap();
    assert_eq!(
        sql,
        ["CREATE TABLE \"app\".\"users\" (\n  \"id\" UUID NOT NULL DEFAULT gen_random_uuid(),\n  \"name\" STRING NOT NULL,\n  \"created_at\" TIMESTAMPTZ NOT NULL DEFAULT now(),\n  CONSTRAINT \"users_pkey\" PRIMARY KEY (\"id\")\n)"]
    );
}

#[test]
fn create_table_supports_a_composite_key() {
    let sql = build_create_table(
        "s",
        "t",
        &[
            pk(col("a", "INT8")),
            pk(col("b", "STRING")),
            col("v", "STRING"),
        ],
    )
    .unwrap();
    assert!(
        sql[0].contains("CONSTRAINT \"t_pkey\" PRIMARY KEY (\"a\", \"b\")"),
        "{}",
        sql[0]
    );
}

#[test]
fn create_table_without_a_key_warns_in_a_comment_but_still_generates() {
    let sql = build_create_table("s", "t", &[col("a", "INT8")]).unwrap();
    assert!(
        sql[0].starts_with("-- Warning: no primary key"),
        "{}",
        sql[0]
    );
    assert!(sql[0].contains("CREATE TABLE \"s\".\"t\" ("));
    assert!(!sql[0].contains("PRIMARY KEY"));
}

#[test]
fn create_table_turns_auto_increment_integers_into_serials_of_the_right_width() {
    let mut a = pk(col("a", "INT8"));
    a.is_auto_increment = true;
    let mut b = col("b", "INT4");
    b.is_auto_increment = true;
    let mut c = col("c", "INT2");
    c.is_auto_increment = true;
    let sql = build_create_table("s", "t", &[a, b, c]).unwrap()[0].clone();
    assert!(sql.contains("\"a\" SERIAL,"), "{sql}");
    assert!(sql.contains("\"b\" SERIAL4,"), "{sql}");
    assert!(sql.contains("\"c\" SMALLSERIAL"), "{sql}");
    assert!(
        !sql.contains("DEFAULT"),
        "a serial carries its own default: {sql}"
    );
}

#[test]
fn an_auto_generated_non_integer_keeps_its_declared_default() {
    // get_columns reports a gen_random_uuid() column as auto-increment; that must not become a serial.
    let mut id = pk(col("id", "UUID"));
    id.is_auto_increment = true;
    id.default_value = Some("gen_random_uuid()".into());
    let sql = build_create_table("s", "t", &[id]).unwrap()[0].clone();
    assert!(
        sql.contains("\"id\" UUID NOT NULL DEFAULT gen_random_uuid()"),
        "{sql}"
    );
}

#[test]
fn create_table_writes_column_comments_as_separate_statements() {
    let mut a = pk(col("a", "INT8"));
    a.comment = Some("the key".into());
    let mut b = col("b", "STRING");
    b.comment = Some("it's \"quoted\"".into());
    let sql = build_create_table("s", "t", &[a, b, col("c", "STRING")]).unwrap();
    assert_eq!(sql.len(), 3);
    assert_eq!(sql[1], "COMMENT ON COLUMN \"s\".\"t\".\"a\" IS 'the key'");
    assert_eq!(
        sql[2],
        "COMMENT ON COLUMN \"s\".\"t\".\"b\" IS 'it''s \"quoted\"'"
    );
}

#[test]
fn create_table_quotes_hostile_names() {
    let sql = build_create_table("sch\"ema", "ta\"ble", &[pk(col("co\"l", "INT8"))]).unwrap();
    assert!(
        sql[0].contains("CREATE TABLE \"sch\"\"ema\".\"ta\"\"ble\""),
        "{}",
        sql[0]
    );
    assert!(sql[0].contains("\"co\"\"l\" INT8 NOT NULL"));
    assert!(sql[0].contains("CONSTRAINT \"ta\"\"ble_pkey\""));
}

#[test]
fn create_table_rejects_bad_input() {
    assert!(err(build_create_table("s", "t", &[])).contains("at least one column"));
    assert!(err(build_create_table("s", " ", &[col("a", "INT8")])).contains("table name"));
    assert!(err(build_create_table(
        "s",
        "t",
        &[col("a", "INT8"), col("a", "STRING")]
    ))
    .contains("more than once"));
    assert!(build_create_table("s", "t", &[col("a", "INT8); DROP TABLE x; --")]).is_err());
    let mut d = col("a", "INT8");
    d.default_value = Some("1; DROP TABLE x".into());
    assert!(build_create_table("s", "t", &[d]).is_err());
}

// ---- add column ----------------------------------------------------------------------

#[test]
fn add_column_basic() {
    let mut c = col("age", "INT8");
    c.is_nullable = false;
    c.default_value = Some("0".into());
    assert_eq!(
        build_add_column("s", "t", &c, &[]).unwrap(),
        ["ALTER TABLE \"s\".\"t\" ADD COLUMN \"age\" INT8 NOT NULL DEFAULT 0"]
    );
    assert_eq!(
        build_add_column("s", "t", &col("note", "STRING"), &[]).unwrap(),
        ["ALTER TABLE \"s\".\"t\" ADD COLUMN \"note\" STRING"]
    );
}

#[test]
fn add_column_serial_and_comment() {
    let mut c = col("n", "INT8");
    c.is_auto_increment = true;
    c.comment = Some("counter".into());
    let sql = build_add_column("s", "t", &c, &[]).unwrap();
    assert_eq!(sql[0], "ALTER TABLE \"s\".\"t\" ADD COLUMN \"n\" SERIAL");
    assert_eq!(sql[1], "COMMENT ON COLUMN \"s\".\"t\".\"n\" IS 'counter'");
}

#[test]
fn a_new_primary_key_column_is_added_then_the_key_is_altered() {
    // ADD COLUMN .. PRIMARY KEY fails when the table already has a key.
    let sql = build_add_column("s", "t", &pk(col("tenant", "INT8")), &["id".into()]).unwrap();
    assert_eq!(
        sql[0],
        "ALTER TABLE \"s\".\"t\" ADD COLUMN \"tenant\" INT8 NOT NULL"
    );
    assert_eq!(
        sql[1],
        "ALTER TABLE \"s\".\"t\" ALTER PRIMARY KEY USING COLUMNS (\"id\", \"tenant\")"
    );
    // A table keyed by the hidden rowid has no declared key to extend.
    let sql = build_add_column("s", "t", &pk(col("id", "INT8")), &[]).unwrap();
    assert_eq!(
        sql[1],
        "ALTER TABLE \"s\".\"t\" ALTER PRIMARY KEY USING COLUMNS (\"id\")"
    );
}

#[test]
fn add_column_rejects_bad_input() {
    assert!(build_add_column("s", "", &col("a", "INT8"), &[]).is_err());
    assert!(build_add_column("s", "t", &col("a", "INT8;"), &[]).is_err());
    let mut c = col("a", "INT8");
    c.default_value = Some("1 --".into());
    assert!(build_add_column("s", "t", &c, &[]).is_err());
}

// ---- alter column --------------------------------------------------------------------

#[test]
fn alter_column_emits_one_statement_per_change_in_a_stable_order() {
    let old = col("old_name", "VARCHAR(50)");
    let mut new = col("new_name", "VARCHAR(100)");
    new.is_nullable = false;
    new.default_value = Some("'x'".into());
    new.comment = Some("renamed".into());
    let sql = build_alter_column("s", "t", &old, &new).unwrap();
    assert_eq!(
        sql,
        [
            "ALTER TABLE \"s\".\"t\" RENAME COLUMN \"old_name\" TO \"new_name\"",
            "ALTER TABLE \"s\".\"t\" ALTER COLUMN \"new_name\" TYPE VARCHAR(100)",
            "ALTER TABLE \"s\".\"t\" ALTER COLUMN \"new_name\" SET NOT NULL",
            "ALTER TABLE \"s\".\"t\" ALTER COLUMN \"new_name\" SET DEFAULT 'x'",
            "COMMENT ON COLUMN \"s\".\"t\".\"new_name\" IS 'renamed'",
        ]
    );
}

#[test]
fn alter_column_drops_what_was_cleared() {
    let mut old = col("a", "STRING");
    old.is_nullable = false;
    old.default_value = Some("'x'".into());
    old.comment = Some("was set".into());
    let new = col("a", "STRING");
    let sql = build_alter_column("s", "t", &old, &new).unwrap();
    assert_eq!(
        sql,
        [
            "ALTER TABLE \"s\".\"t\" ALTER COLUMN \"a\" DROP NOT NULL",
            "ALTER TABLE \"s\".\"t\" ALTER COLUMN \"a\" DROP DEFAULT",
            "COMMENT ON COLUMN \"s\".\"t\".\"a\" IS NULL",
        ]
    );
}

#[test]
fn alter_column_with_no_change_is_reported() {
    let c = col("a", "INT8");
    assert_eq!(
        err(build_alter_column("s", "t", &c, &c.clone())),
        "No changes detected"
    );
    // Whitespace and case in a type are not a change.
    let mut other = c.clone();
    other.data_type = " int8 ".into();
    assert!(build_alter_column("s", "t", &c, &other).is_err());
}

#[test]
fn compatible_type_changes_need_no_using() {
    for (from, to) in [
        ("VARCHAR(50)", "VARCHAR(100)"),
        ("INT4", "INT8"),
        ("DECIMAL(10,2)", "DECIMAL(12,2)"),
        ("TIMESTAMP", "TIMESTAMPTZ"),
        ("JSON", "JSONB"),
        ("STRING", "VARCHAR(20)"),
        ("FLOAT4", "DECIMAL"),
    ] {
        let sql = build_alter_column("s", "t", &col("a", from), &col("a", to)).unwrap();
        assert_eq!(sql.len(), 1, "{from}->{to}");
        assert!(
            !sql[0].contains("USING") && !sql[0].starts_with("--"),
            "{from}->{to}: {}",
            sql[0]
        );
    }
}

#[test]
fn converting_type_changes_are_explicit_and_explained() {
    for (from, to) in [
        ("STRING", "INT8"),
        ("INT8", "STRING"),
        ("STRING", "UUID"),
        ("TIMESTAMP", "DATE"),
    ] {
        let sql = build_alter_column("s", "t", &col("a", from), &col("a", to)).unwrap();
        assert_eq!(sql.len(), 1);
        assert!(
            sql[0].contains(&format!("ALTER COLUMN \"a\" TYPE {to} USING \"a\"::{to}")),
            "{}",
            sql[0]
        );
        assert!(
            sql[0].starts_with("-- Converts the column's data"),
            "{}",
            sql[0]
        );
        assert!(
            sql[0].contains("enable_experimental_alter_column_type_general"),
            "{}",
            sql[0]
        );
    }
}

#[test]
fn the_using_cast_keeps_the_type_modifiers() {
    let sql =
        build_alter_column("s", "t", &col("a", "STRING"), &col("a", "DECIMAL(10,2)")).unwrap();
    assert!(
        sql[0].contains("TYPE DECIMAL(10,2) USING \"a\"::DECIMAL(10,2)"),
        "{}",
        sql[0]
    );
}

#[test]
fn a_rename_is_applied_before_the_other_changes_use_the_new_name() {
    let sql = build_alter_column("s", "t", &col("a", "INT8"), &{
        let mut n = col("b", "INT8");
        n.is_nullable = false;
        n
    })
    .unwrap();
    assert!(sql[0].contains("RENAME COLUMN \"a\" TO \"b\""));
    assert!(
        sql[1].contains("ALTER COLUMN \"b\" SET NOT NULL"),
        "{}",
        sql[1]
    );
}

#[test]
fn alter_column_refuses_to_change_primary_key_membership() {
    let old = col("a", "INT8");
    let new = pk(col("a", "INT8"));
    assert!(err(build_alter_column("s", "t", &old, &new)).contains("ALTER PRIMARY KEY"));
}

#[test]
fn alter_column_rejects_bad_input() {
    assert!(build_alter_column("s", "t", &col("a", "INT8"), &col("a", "INT8; --")).is_err());
    let mut n = col("a", "INT8");
    n.default_value = Some("1; DROP TABLE t".into());
    assert!(build_alter_column("s", "t", &col("a", "INT8"), &n).is_err());
}

#[test]
fn type_helpers() {
    assert_eq!(base_type("varchar(50)"), "VARCHAR");
    assert_eq!(base_type("decimal(10,2)[]"), "DECIMAL[]");
    assert_eq!(base_type(" INT8 "), "INT8");
    assert_eq!(serial_type("INT8"), Some("SERIAL"));
    assert_eq!(serial_type("int4"), Some("SERIAL4"));
    assert_eq!(serial_type("SMALLINT"), Some("SMALLSERIAL"));
    assert_eq!(serial_type("UUID"), None);
    assert_eq!(serial_type("STRING"), None);
}

// ---- indexes and foreign keys ----------------------------------------------------------

#[test]
fn create_index() {
    let cols = ["email".to_string(), "name".to_string()];
    assert_eq!(
        build_create_index("s", "t", "idx_email", &cols, true).unwrap(),
        "CREATE UNIQUE INDEX \"idx_email\" ON \"s\".\"t\" (\"email\", \"name\")"
    );
    assert_eq!(
        build_create_index("s", "t", "i", &cols[..1], false).unwrap(),
        "CREATE INDEX \"i\" ON \"s\".\"t\" (\"email\")"
    );
    // An unnamed index lets the server choose a name.
    assert_eq!(
        build_create_index("s", "t", "", &cols[..1], false).unwrap(),
        "CREATE INDEX ON \"s\".\"t\" (\"email\")"
    );
    assert!(build_create_index("s", "t", "i", &[], false).is_err());
    assert!(build_create_index("s", "t", "i", &["".to_string()], false).is_err());
}

#[test]
fn create_foreign_key() {
    assert_eq!(
        build_create_foreign_key("s", "orders", "fk_user", "user_id", "users", "id", Some("CASCADE"), Some("no action")).unwrap(),
        "ALTER TABLE \"s\".\"orders\" ADD CONSTRAINT \"fk_user\" FOREIGN KEY (\"user_id\") REFERENCES \"s\".\"users\" (\"id\") ON DELETE CASCADE ON UPDATE NO ACTION"
    );
    assert_eq!(
        build_create_foreign_key("s", "o", "fk", "u", "users", "id", None, Some("")).unwrap(),
        "ALTER TABLE \"s\".\"o\" ADD CONSTRAINT \"fk\" FOREIGN KEY (\"u\") REFERENCES \"s\".\"users\" (\"id\")"
    );
}

#[test]
fn foreign_key_actions_are_checked() {
    for ok in [
        "NO ACTION",
        "restrict",
        " Cascade ",
        "SET NULL",
        "set  default",
    ] {
        assert!(normalize_action(Some(ok)).unwrap().is_some(), "{ok}");
    }
    assert_eq!(normalize_action(None).unwrap(), None);
    for bad in [
        "CASCADE; DROP TABLE t",
        "DELETE",
        "SET",
        "ON DELETE CASCADE",
    ] {
        assert!(normalize_action(Some(bad)).is_err(), "{bad}");
    }
    assert!(build_create_foreign_key(
        "s",
        "o",
        "fk",
        "u",
        "users",
        "id",
        Some("CASCADE; --"),
        None
    )
    .is_err());
    for (t, n, c, rt, rc) in [
        ("", "fk", "c", "r", "x"),
        ("t", "", "c", "r", "x"),
        ("t", "fk", "", "r", "x"),
        ("t", "fk", "c", "", "x"),
        ("t", "fk", "c", "r", ""),
    ] {
        assert!(build_create_foreign_key("s", t, n, c, rt, rc, None, None).is_err());
    }
}

#[test]
fn drop_index_names_the_table_because_index_names_are_per_table() {
    assert_eq!(
        build_drop_index("s", "t", "idx").unwrap(),
        "DROP INDEX \"s\".\"t\"@\"idx\""
    );
    assert_eq!(
        build_drop_index("s\"x", "t", "i\"d").unwrap(),
        "DROP INDEX \"s\"\"x\".\"t\"@\"i\"\"d\""
    );
    assert!(build_drop_index("s", "", "idx").is_err());
    assert!(build_drop_index("s", "t", "").is_err());
}

#[test]
fn drop_foreign_key() {
    assert_eq!(
        build_drop_foreign_key("s", "t", "fk").unwrap(),
        "ALTER TABLE \"s\".\"t\" DROP CONSTRAINT \"fk\""
    );
    assert!(build_drop_foreign_key("s", "t", " ").is_err());
}

// ---- views ----------------------------------------------------------------------------

#[test]
fn view_statements() {
    assert_eq!(
        build_create_view("s", "v", "SELECT 1;").unwrap(),
        "CREATE VIEW \"s\".\"v\" AS SELECT 1"
    );
    assert_eq!(
        build_replace_view("s", "v", "SELECT 1").unwrap(),
        "CREATE OR REPLACE VIEW \"s\".\"v\" AS SELECT 1"
    );
    assert_eq!(
        build_drop_view("s", "v").unwrap(),
        "DROP VIEW IF EXISTS \"s\".\"v\""
    );
    assert!(build_create_view("s", "", "SELECT 1").is_err());
    assert!(build_create_view("s", "v", "SELECT 1; DROP TABLE t").is_err());
    assert!(build_replace_view("s", "v", "").is_err());
    assert!(build_drop_view("s", " ").is_err());
}
