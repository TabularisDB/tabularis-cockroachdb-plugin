use bytes::BytesMut;
use serde_json::json;

use super::*;

fn opts(t: &str) -> BindOptions<'_> {
    BindOptions {
        column_type: Some(t),
        ..Default::default()
    }
}

fn bind(v: Value, t: &str) -> BoundValue {
    bind_value(v, 1, &opts(t)).unwrap()
}

fn ty(b: &BoundValue) -> Type {
    b.param.as_ref().unwrap().1.clone()
}

/// The bytes that would go on the wire for the bound parameter.
fn wire(b: &BoundValue) -> Vec<u8> {
    let (p, t) = b.param.as_ref().unwrap();
    let mut out = BytesMut::new();
    p.to_sql_checked(t, &mut out).unwrap();
    out.to_vec()
}

#[test]
fn default_sentinel_binds_nothing() {
    let o = BindOptions {
        column_type: Some("INT8"),
        allow_default: true,
        ..Default::default()
    };
    let b = bind_value(json!("__USE_DEFAULT__"), 1, &o).unwrap();
    assert_eq!(b.sql, "DEFAULT");
    assert!(b.param.is_none());
    // Without allow_default it is just text.
    assert!(bind_value(json!("__USE_DEFAULT__"), 1, &opts("STRING"))
        .unwrap()
        .param
        .is_some());
}

#[test]
fn null_binds_nothing() {
    let b = bind(Value::Null, "INT8");
    assert_eq!(b.sql, "NULL");
    assert!(b.param.is_none());
}

#[test]
fn int8_accepts_numbers_and_numeric_strings() {
    let n = bind(json!(42), "INT8");
    assert_eq!((n.sql.as_str(), ty(&n)), ("$1", Type::INT8));
    assert_eq!(wire(&n), 42i64.to_be_bytes());
    // A key that was sent as a string comes back as a string.
    let s = bind(json!("1152921504606846977"), "INT8");
    assert_eq!(ty(&s), Type::INT8);
    assert_eq!(wire(&s), 1_152_921_504_606_846_977i64.to_be_bytes());
    assert!(bind_value(json!("12x"), 1, &opts("INT8")).is_err());
    assert!(bind_value(json!(1.5), 1, &opts("INT8")).is_err());
    assert_eq!(wire(&bind(json!(5.0), "INT8")), 5i64.to_be_bytes());
}

#[test]
fn narrow_ints_are_pinned_and_range_checked() {
    assert_eq!(ty(&bind(json!(7), "INT2")), Type::INT2);
    assert_eq!(ty(&bind(json!("7"), "INT4")), Type::INT4);
    assert_eq!(wire(&bind(json!(7), "INT4")), 7i32.to_be_bytes());
    assert!(bind_value(json!(40_000), 1, &opts("INT2")).is_err());
    assert!(bind_value(json!(3_000_000_000i64), 1, &opts("INT4")).is_err());
}

#[test]
fn decimal_keeps_precision_through_a_cast() {
    for v in [json!("123456789012345678901234567890.123"), json!(12.5)] {
        let b = bind(v, "DECIMAL(10,2)");
        assert_eq!(b.sql, "CAST($1 AS DECIMAL(10,2))");
        assert_eq!(ty(&b), Type::TEXT);
    }
    assert_eq!(wire(&bind(json!("-0.001"), "DECIMAL")), b"-0.001");
    for ok in [
        "NaN",
        "Infinity",
        "-Infinity",
        "1e10",
        "-1.5E-3",
        ".5",
        "5.",
    ] {
        assert!(bind_value(json!(ok), 1, &opts("DECIMAL")).is_ok(), "{ok}");
    }
    for bad in ["abc", "1.2.3", "1e", "", "--1", "1; DROP TABLE x"] {
        assert!(
            bind_value(json!(bad), 1, &opts("DECIMAL")).is_err(),
            "{bad}"
        );
    }
}

#[test]
fn floats() {
    assert_eq!(ty(&bind(json!(1.5), "FLOAT8")), Type::FLOAT8);
    assert_eq!(ty(&bind(json!(1.5), "FLOAT4")), Type::FLOAT4);
    assert_eq!(wire(&bind(json!(1.5), "FLOAT8")), 1.5f64.to_be_bytes());
    let nan = wire(&bind(json!("NaN"), "FLOAT8"));
    assert!(f64::from_be_bytes(nan.try_into().unwrap()).is_nan());
    let inf = wire(&bind(json!("-Infinity"), "FLOAT8"));
    assert_eq!(
        f64::from_be_bytes(inf.try_into().unwrap()),
        f64::NEG_INFINITY
    );
    assert!(bind_value(json!("x"), 1, &opts("FLOAT8")).is_err());
}

#[test]
fn booleans() {
    for (v, expect) in [
        (json!(true), 1u8),
        (json!("t"), 1),
        (json!("1"), 1),
        (json!("false"), 0),
        (json!("0"), 0),
        (json!(0), 0),
    ] {
        let b = bind(v, "BOOL");
        assert_eq!(ty(&b), Type::BOOL);
        assert_eq!(wire(&b), [expect]);
    }
    assert!(bind_value(json!("maybe"), 1, &opts("BOOL")).is_err());
    assert!(bind_value(json!(2), 1, &opts("BOOL")).is_err());
}

#[test]
fn bytes_use_the_blob_wire_format() {
    let wire_str = crate::utils::blob::encode_blob_full(&[1, 2, 3]);
    let b = bind(json!(wire_str), "BYTES");
    assert_eq!(ty(&b), Type::BYTEA);
    assert_eq!(wire(&b), [1, 2, 3]);
    assert_eq!(wire(&bind(json!("\\x0aff"), "BYTES")), [0x0a, 0xff]);
    assert_eq!(wire(&bind(json!("plain"), "BYTES")), b"plain");
    // Unknown column type: still recognised.
    assert_eq!(
        ty(&bind_value(json!(wire_str), 1, &BindOptions::default()).unwrap()),
        Type::BYTEA
    );
}

#[test]
fn enum_binds_as_text_with_a_cast() {
    let o = BindOptions {
        column_type: Some("STRING"),
        enum_type: Some("\"public\".\"mood\""),
        ..Default::default()
    };
    let b = bind_value(json!("happy"), 2, &o).unwrap();
    assert_eq!(b.sql, "CAST($2 AS \"public\".\"mood\")");
    assert_eq!(ty(&b), Type::TEXT);
    // A label that looks like a UUID or an array is still just a label.
    assert_eq!(
        bind_value(json!("[1,2]"), 1, &o).unwrap().sql,
        "CAST($1 AS \"public\".\"mood\")"
    );
    assert!(bind_value(json!(["a"]), 1, &o).is_err());
}

#[test]
fn enum_type_name_is_validated() {
    let bad = BindOptions {
        enum_type: Some("mood); DROP TABLE x; --"),
        ..Default::default()
    };
    assert!(bind_value(json!("a"), 1, &bad).is_err());
    assert_eq!(quote_enum_type("my\"s", "m"), "\"my\"\"s\".\"m\"");
    let ok = BindOptions {
        enum_type: Some(&quote_enum_type("my\"s", "m")),
        ..Default::default()
    };
    assert!(bind_value(json!("a"), 1, &ok).is_ok());
}

#[test]
fn uuid_is_validated_and_sent_as_16_bytes() {
    let b = bind(json!("DEADBEEF-0001-0203-0405-060708090A0B"), "UUID");
    assert_eq!(ty(&b), Type::UUID);
    assert_eq!(
        wire(&b),
        [0xDE, 0xAD, 0xBE, 0xEF, 0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 0x0A, 0x0B]
    );
    assert_eq!(
        wire(&bind(json!("deadbeef000102030405060708090a0b"), "UUID")).len(),
        16
    );
    for bad in [
        "not-a-uuid",
        "deadbeef-0001-0203-0405-060708090a0",
        "deadbeef0001-0203-0405-060708090a0b0",
        "",
    ] {
        assert!(bind_value(json!(bad), 1, &opts("UUID")).is_err(), "{bad}");
    }
}

#[test]
fn jsonb_binds_values_and_parses_json_strings() {
    let obj = bind(json!({"a": 1}), "JSONB");
    assert_eq!(ty(&obj), Type::JSONB);
    assert_eq!(wire(&obj), b"\x01{\"a\":1}");
    assert_eq!(wire(&bind(json!("{\"a\":1}"), "JSONB")), b"\x01{\"a\":1}");
    assert_eq!(wire(&bind(json!("not json"), "JSONB")), b"\x01\"not json\"");
    assert_eq!(wire(&bind(json!([1, 2]), "JSONB")), b"\x01[1,2]");
    assert_eq!(wire(&bind(json!(5), "JSONB")), b"\x015");
    assert!(bind_value(json!({"a": 1}), 1, &opts("STRING")).is_err());
}

#[test]
fn temporal_and_special_types_cast_the_column_type() {
    let t = bind(json!("2024-03-05 12:30:45+02"), "TIMESTAMPTZ");
    assert_eq!(t.sql, "CAST($1 AS TIMESTAMPTZ)");
    assert_eq!(ty(&t), Type::TEXT);
    assert_eq!(bind(json!("1 day"), "INTERVAL").sql, "CAST($1 AS INTERVAL)");
    assert_eq!(bind(json!("10.0.0.0/8"), "INET").sql, "CAST($1 AS INET)");
    assert_eq!(bind(json!("101"), "BIT(3)").sql, "CAST($1 AS BIT(3))");
    assert_eq!(
        bind(json!("[1,2]"), "VECTOR(2)").sql,
        "CAST($1 AS VECTOR(2))"
    );
    assert_eq!(
        bind(json!("POINT(1 2)"), "GEOMETRY").sql,
        "CAST($1 AS GEOMETRY)"
    );
}

#[test]
fn arrays_become_literals_with_a_cast() {
    let b = bind(json!([1, null, 3]), "INT8[]");
    assert_eq!(b.sql, "CAST($1 AS INT8[])");
    assert_eq!(wire(&b), b"{1,NULL,3}");
    assert_eq!(
        wire(&bind(json!(["a b", "q\"x", "b\\s"]), "STRING[]")),
        br#"{"a b","q\"x","b\\s"}"#
    );
    // A JSON array held in a string, and a ready-made literal.
    assert_eq!(wire(&bind(json!("[1,2]"), "INT8[]")), b"{1,2}");
    assert_eq!(wire(&bind(json!("{1,2}"), "INT8[]")), b"{1,2}");
    assert_eq!(wire(&bind(json!([]), "INT8[]")), b"{}");
    assert_eq!(
        wire(&bind(json!([{"x": 1}]), "JSONB[]")),
        br#"{"{\"x\":1}"}"#
    );
    assert!(bind_value(json!([1]), 1, &opts("INT8")).is_err());
    assert!(bind_value(json!([1]), 1, &BindOptions::default()).is_err());
}

#[test]
fn text_fallback() {
    let b = bind(json!("hello"), "STRING");
    assert_eq!((b.sql.as_str(), ty(&b)), ("$1", Type::TEXT));
    let u = bind_value(json!("hello"), 3, &BindOptions::default()).unwrap();
    assert_eq!(u.sql, "$3");
    assert_eq!(wire(&bind(json!(12), "STRING")), b"12");
    assert_eq!(wire(&bind(json!(true), "STRING")), b"true");
}

#[test]
fn unknown_type_infers_from_json() {
    let i = bind_value(json!(7), 1, &BindOptions::default()).unwrap();
    assert_eq!(ty(&i), Type::INT8);
    let f = bind_value(json!(7.5), 1, &BindOptions::default()).unwrap();
    assert_eq!(ty(&f), Type::FLOAT8);
    let b = bind_value(json!(true), 1, &BindOptions::default()).unwrap();
    assert_eq!(ty(&b), Type::BOOL);
}

#[test]
fn cast_targets_cannot_inject_sql() {
    assert!(bind_value(json!("x"), 1, &opts("INET) ; DROP TABLE t; --")).is_err());
    assert!(bind_value(json!("x"), 1, &opts("A\"B")).is_err());
}

#[test]
fn array_literal_escaping() {
    assert_eq!(
        pg_array_literal(&[json!(true), json!("x")]).unwrap(),
        r#"{true,"x"}"#
    );
    assert_eq!(
        pg_array_literal(&[json!([1, 2]), json!([3])]).unwrap(),
        "{{1,2},{3}}"
    );
}

// ---- live: bind against a real server (set COCKROACH_HOST) -------------

#[test]
fn live_round_trip() {
    let Ok(host) = std::env::var("COCKROACH_HOST") else {
        return;
    };
    let port: u16 = std::env::var("COCKROACH_PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(26257);
    crate::client::block_on(async {
        let (client, conn) = tokio_postgres::Config::new()
            .host(&host)
            .port(port)
            .user("root")
            .dbname("defaultdb")
            .connect(tokio_postgres::NoTls)
            .await
            .unwrap();
        tokio::spawn(conn);
        client.batch_execute(
            "DROP TABLE IF EXISTS live_bind; DROP TYPE IF EXISTS live_bind_mood;
             CREATE TYPE live_bind_mood AS ENUM ('sad','ok');
             CREATE TABLE live_bind (id INT8 PRIMARY KEY, i2 INT2, i4 INT4, d DECIMAL(30,3), f FLOAT8, b BOOL,
               s STRING, y BYTES, u UUID, j JSONB, ts TIMESTAMPTZ, iv INTERVAL, ip INET, a INT8[], sa STRING[], m live_bind_mood)",
        ).await.unwrap();

        let cols = [
            ("id", "INT8", json!("1152921504606846977")),
            ("i2", "INT2", json!(7)),
            ("i4", "INT4", json!("70000")),
            (
                "d",
                "DECIMAL(30,3)",
                json!("123456789012345678901234567.890"),
            ),
            ("f", "FLOAT8", json!("Infinity")),
            ("b", "BOOL", json!("t")),
            ("s", "STRING", json!("it's")),
            (
                "y",
                "BYTES",
                json!(crate::utils::blob::encode_blob_full(&[0, 1, 255])),
            ),
            ("u", "UUID", json!("DEADBEEF-0001-0203-0405-060708090A0B")),
            ("j", "JSONB", json!({"k": [1, 2]})),
            ("ts", "TIMESTAMPTZ", json!("2024-03-05T12:30:45.5Z")),
            ("iv", "INTERVAL", json!("1 day 02:00:00")),
            ("ip", "INET", json!("10.0.0.0/24")),
            ("a", "INT8[]", json!([1, null, 3])),
            ("sa", "STRING[]", json!(["a b", "q\"x", "b\\s"])),
            ("m", "STRING", json!("ok")),
        ];
        let mut names = vec![];
        let mut sqls = vec![];
        let mut types = vec![];
        let mut params: Vec<PgParam> = vec![];
        let mut n = 0;
        for (name, ty, val) in cols {
            let o = BindOptions {
                column_type: Some(ty),
                enum_type: (name == "m").then_some("\"public\".\"live_bind_mood\""),
                ..Default::default()
            };
            n += 1;
            let b = bind_value(val, n, &o).unwrap();
            names.push(name);
            sqls.push(b.sql);
            if let Some((p, t)) = b.param {
                params.push(p);
                types.push(t);
            } else {
                n -= 1;
            }
        }
        let sql = format!(
            "INSERT INTO live_bind ({}) VALUES ({})",
            names.join(","),
            sqls.join(",")
        );
        let stmt = client.prepare_typed(&sql, &types).await.unwrap();
        let refs: Vec<&(dyn ToSql + Sync)> =
            params.iter().map(|p| &**p as &(dyn ToSql + Sync)).collect();
        client.execute(&stmt, &refs).await.unwrap();

        let rows = client.query("SELECT * FROM live_bind", &[]).await.unwrap();
        let r = crate::extract::row_to_json(&rows[0]);
        assert_eq!(r[0], json!("1152921504606846977"));
        assert_eq!(r[1], json!(7));
        assert_eq!(r[2], json!(70000));
        assert_eq!(r[3], json!("123456789012345678901234567.890"));
        assert_eq!(r[4], json!("Infinity"));
        assert_eq!(r[5], json!(true));
        assert_eq!(r[6], json!("it's"));
        assert!(r[7].as_str().unwrap().starts_with("BLOB:3:"));
        assert_eq!(r[8], json!("deadbeef-0001-0203-0405-060708090a0b"));
        assert_eq!(r[9], json!({"k": [1, 2]}));
        assert_eq!(r[10], json!("2024-03-05T12:30:45.5Z"));
        assert_eq!(r[11], json!("1 day 02:00:00"));
        assert_eq!(r[12], json!("10.0.0.0/24"));
        assert_eq!(r[13], json!([1, null, 3]));
        assert_eq!(r[14], json!(["a b", "q\"x", "b\\s"]));
        assert_eq!(r[15], json!("ok"));

        // A string INT8 key finds the right row again.
        let b = bind_value(json!("1152921504606846977"), 1, &opts("INT8")).unwrap();
        let (p, t) = b.param.unwrap();
        let stmt = client
            .prepare_typed("SELECT count(*) FROM live_bind WHERE id = $1", &[t])
            .await
            .unwrap();
        let hit = client.query_one(&stmt, &[&*p]).await.unwrap();
        assert_eq!(hit.get::<_, i64>(0), 1);

        client
            .batch_execute("DROP TABLE live_bind; DROP TYPE live_bind_mood")
            .await
            .unwrap();
    });
}
