use super::*;

fn i16b(v: i16) -> [u8; 2] {
    v.to_be_bytes()
}

fn numeric(ndigits: i16, weight: i16, sign: u16, dscale: i16, digits: &[i16]) -> Vec<u8> {
    let mut b = Vec::new();
    b.extend(i16b(ndigits));
    b.extend(i16b(weight));
    b.extend(sign.to_be_bytes());
    b.extend(i16b(dscale));
    for d in digits {
        b.extend(i16b(*d));
    }
    b
}

fn dec(raw: &[u8]) -> Value {
    decode(&Type::NUMERIC, raw).unwrap()
}

#[test]
fn int8_is_a_number_inside_the_safe_range_and_a_string_outside() {
    let enc = |v: i64| decode(&Type::INT8, &v.to_be_bytes()).unwrap();
    assert_eq!(enc(42), json!(42));
    assert_eq!(enc(-MAX_SAFE_INT), json!(-MAX_SAFE_INT));
    assert_eq!(enc(MAX_SAFE_INT), json!(MAX_SAFE_INT));
    assert_eq!(enc(MAX_SAFE_INT + 1), json!("9007199254740992"));
    // A typical unique_rowid() value.
    assert_eq!(enc(1_152_921_504_606_846_977), json!("1152921504606846977"));
    assert_eq!(enc(i64::MIN), json!("-9223372036854775808"));
}

#[test]
fn small_ints_and_bool() {
    assert_eq!(
        decode(&Type::INT2, &(-5i16).to_be_bytes()).unwrap(),
        json!(-5)
    );
    assert_eq!(
        decode(&Type::INT4, &70_000i32.to_be_bytes()).unwrap(),
        json!(70_000)
    );
    assert_eq!(decode(&Type::BOOL, &[1]).unwrap(), json!(true));
    assert_eq!(decode(&Type::BOOL, &[0]).unwrap(), json!(false));
    assert!(decode(&Type::INT8, &[0, 1]).is_err());
}

#[test]
fn floats_keep_specials_as_strings() {
    let f8 = |v: f64| decode(&Type::FLOAT8, &v.to_be_bytes()).unwrap();
    assert_eq!(f8(1.5), json!(1.5));
    assert_eq!(f8(f64::NAN), json!("NaN"));
    assert_eq!(f8(f64::INFINITY), json!("Infinity"));
    assert_eq!(f8(f64::NEG_INFINITY), json!("-Infinity"));
    assert_eq!(
        decode(&Type::FLOAT4, &0.1f32.to_be_bytes()).unwrap(),
        json!(0.1)
    );
}

#[test]
fn decimal_is_always_an_exact_string() {
    assert_eq!(dec(&numeric(2, 0, 0, 2, &[123, 4500])), json!("123.45"));
    assert_eq!(dec(&numeric(2, 0, 0, 2, &[1, 5000])), json!("1.50"));
    assert_eq!(dec(&numeric(1, -1, 0x4000, 4, &[12])), json!("-0.0012"));
    assert_eq!(dec(&numeric(1, 1, 0, 0, &[1])), json!("10000"));
    assert_eq!(dec(&numeric(0, 0, 0, 0, &[])), json!("0"));
    assert_eq!(dec(&numeric(0, 0, 0, 3, &[])), json!("0.000"));
    assert_eq!(
        dec(&numeric(4, 3, 0, 0, &[1, 2345, 6789, 1])),
        json!("1234567890001")
    );
    assert_eq!(dec(&numeric(0, 0, 0xC000, 0, &[])), json!("NaN"));
    assert_eq!(dec(&numeric(0, 0, 0xD000, 0, &[])), json!("Infinity"));
    assert_eq!(dec(&numeric(0, 0, 0xF000, 0, &[])), json!("-Infinity"));
    assert!(decode(&Type::NUMERIC, &numeric(0, 0, 0x1234, 0, &[])).is_err());
}

#[test]
fn text_and_json() {
    assert_eq!(
        decode(&Type::TEXT, "héllo".as_bytes()).unwrap(),
        json!("héllo")
    );
    let mut jsonb = vec![1u8];
    jsonb.extend(br#"{"a":[1,2]}"#);
    assert_eq!(decode(&Type::JSONB, &jsonb).unwrap(), json!({"a": [1, 2]}));
    assert_eq!(decode(&Type::JSON, b"[true]").unwrap(), json!([true]));
}

#[test]
fn uuid_is_lowercase_hyphenated() {
    let raw = [
        0xDE, 0xAD, 0xBE, 0xEF, 0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0A,
        0x0B,
    ];
    assert_eq!(
        decode(&Type::UUID, &raw).unwrap(),
        json!("deadbeef-0001-0203-0405-060708090a0b")
    );
}

#[test]
fn bytes_use_the_blob_wire_format() {
    let v = decode(&Type::BYTEA, &[1, 2, 3]).unwrap();
    assert!(v
        .as_str()
        .unwrap()
        .starts_with("BLOB:3:application/octet-stream:"));
}

#[test]
fn dates_and_timestamps_are_iso() {
    let d = |days: i32| decode(&Type::DATE, &days.to_be_bytes()).unwrap();
    assert_eq!(d(0), json!("2000-01-01"));
    assert_eq!(d(-1), json!("1999-12-31"));
    assert_eq!(d(8825), json!("2024-02-29"));
    assert_eq!(d(-11122), json!("1969-07-20"));
    assert_eq!(d(i32::MAX), json!("infinity"));
    let ts = 762_957_045_500_000i64.to_be_bytes();
    assert_eq!(
        decode(&Type::TIMESTAMP, &ts).unwrap(),
        json!("2024-03-05T12:30:45.5")
    );
    assert_eq!(
        decode(&Type::TIMESTAMPTZ, &ts).unwrap(),
        json!("2024-03-05T12:30:45.5Z")
    );
    assert_eq!(
        decode(&Type::TIMESTAMP, &0i64.to_be_bytes()).unwrap(),
        json!("2000-01-01T00:00:00")
    );
    assert_eq!(
        decode(&Type::TIMESTAMP, &(-1i64).to_be_bytes()).unwrap(),
        json!("1999-12-31T23:59:59.999999")
    );
}

#[test]
fn time_and_timetz() {
    let t = (3_723_000_000i64 + 250_000).to_be_bytes();
    assert_eq!(decode(&Type::TIME, &t).unwrap(), json!("01:02:03.25"));
    let mut tz = t.to_vec();
    tz.extend((-19_800i32).to_be_bytes()); // 5.5 hours east of UTC
    assert_eq!(
        decode(&Type::TIMETZ, &tz).unwrap(),
        json!("01:02:03.25+05:30")
    );
    let mut west = 0i64.to_be_bytes().to_vec();
    west.extend(18_000i32.to_be_bytes());
    assert_eq!(
        decode(&Type::TIMETZ, &west).unwrap(),
        json!("00:00:00-05:00")
    );
}

fn interval(micros: i64, days: i32, months: i32) -> Value {
    let mut b = micros.to_be_bytes().to_vec();
    b.extend(days.to_be_bytes());
    b.extend(months.to_be_bytes());
    decode(&Type::INTERVAL, &b).unwrap()
}

#[test]
fn intervals_use_the_postgres_text_form() {
    assert_eq!(
        interval(3_723_000_000, 3, 14),
        json!("1 year 2 mons 3 days 01:02:03")
    );
    assert_eq!(interval(7_200_000_000, 1, 0), json!("1 day 02:00:00"));
    assert_eq!(interval(0, 0, 0), json!("00:00:00"));
    assert_eq!(interval(0, 2, 0), json!("2 days"));
    assert_eq!(interval(-3_600_000_000, 0, 0), json!("-01:00:00"));
    assert_eq!(interval(0, 0, -1), json!("-1 mon"));
}

#[test]
fn inet_and_cidr() {
    assert_eq!(
        decode(&Type::INET, &[2, 32, 0, 4, 192, 168, 1, 1]).unwrap(),
        json!("192.168.1.1")
    );
    assert_eq!(
        decode(&Type::INET, &[2, 24, 0, 4, 10, 0, 0, 0]).unwrap(),
        json!("10.0.0.0/24")
    );
    assert_eq!(
        decode(&Type::CIDR, &[2, 8, 1, 4, 10, 0, 0, 0]).unwrap(),
        json!("10.0.0.0/8")
    );
    let mut v6 = vec![3, 128, 0, 16];
    v6.extend([0u8; 15]);
    v6.push(1);
    assert_eq!(decode(&Type::INET, &v6).unwrap(), json!("::1"));
    assert!(decode(&Type::INET, &[9, 0, 0, 0]).is_err());
}

#[test]
fn bit_strings() {
    let mut raw = 5i32.to_be_bytes().to_vec();
    raw.push(0b1011_0000);
    assert_eq!(decode(&Type::VARBIT, &raw).unwrap(), json!("10110"));
    assert_eq!(decode(&Type::BIT, &0i32.to_be_bytes()).unwrap(), json!(""));
}

/// One-dimensional array of `elems` (`None` = NULL).
fn array(elem_oid: u32, elems: &[Option<Vec<u8>>]) -> Vec<u8> {
    let mut b = 1i32.to_be_bytes().to_vec();
    b.extend(1i32.to_be_bytes()); // has-nulls flag (value ignored)
    b.extend(elem_oid.to_be_bytes());
    b.extend((elems.len() as i32).to_be_bytes());
    b.extend(1i32.to_be_bytes());
    for e in elems {
        match e {
            Some(v) => {
                b.extend((v.len() as i32).to_be_bytes());
                b.extend(v);
            }
            None => b.extend((-1i32).to_be_bytes()),
        }
    }
    b
}

#[test]
fn arrays_decode_elements_and_nulls() {
    let raw = array(
        20,
        &[
            Some(1i64.to_be_bytes().to_vec()),
            None,
            Some((1i64 << 60).to_be_bytes().to_vec()),
        ],
    );
    assert_eq!(
        decode(&Type::INT8_ARRAY, &raw).unwrap(),
        json!([1, null, "1152921504606846976"])
    );
    let text = array(25, &[Some(b"a".to_vec()), Some(b"b c".to_vec())]);
    assert_eq!(
        decode(&Type::TEXT_ARRAY, &text).unwrap(),
        json!(["a", "b c"])
    );
}

#[test]
fn empty_and_truncated_arrays() {
    let mut empty = 0i32.to_be_bytes().to_vec();
    empty.extend([0u8; 8]);
    assert_eq!(decode(&Type::TEXT_ARRAY, &empty).unwrap(), json!([]));
    let mut cut = array(25, &[Some(b"abc".to_vec())]);
    cut.truncate(cut.len() - 2);
    assert!(decode(&Type::TEXT_ARRAY, &cut).is_err());
}

#[test]
fn canonical_names() {
    assert_eq!(canonical_type_name(&Type::INT8), "INT8");
    assert_eq!(canonical_type_name(&Type::NUMERIC), "DECIMAL");
    assert_eq!(canonical_type_name(&Type::TEXT), "STRING");
    assert_eq!(canonical_type_name(&Type::TEXT_ARRAY), "STRING[]");
    assert_eq!(canonical_type_name(&Type::BYTEA), "BYTES");
    assert_eq!(canonical_type_name(&Type::JSON), "JSONB");
}

#[test]
fn unknown_types_fall_back_to_text_or_hex() {
    let t = Type::new("geometry".into(), 99_999, Kind::Simple, "public".into());
    assert_eq!(decode(&t, &[0x01, 0xAB]).unwrap(), json!("01ab"));
    let other = Type::new("tsvector".into(), 99_998, Kind::Simple, "pg_catalog".into());
    assert_eq!(decode(&other, b"'a':1").unwrap(), json!("'a':1"));
    assert_eq!(decode(&other, &[0xFF, 0xFE]).unwrap(), json!("fffe"));
}

#[test]
fn enums_are_labels() {
    let t = Type::new(
        "mood".into(),
        99_997,
        Kind::Enum(vec!["sad".into(), "ok".into()]),
        "public".into(),
    );
    assert_eq!(decode(&t, b"sad").unwrap(), json!("sad"));
    assert_eq!(canonical_type_name(&t), "ENUM(mood)");
}

// ---- live: real CockroachDB wire encodings (set COCKROACH_HOST) ----------

fn live_rows(sql: &str) -> Option<Vec<Vec<Value>>> {
    let host = std::env::var("COCKROACH_HOST").ok()?;
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
        let rows = client.query(sql, &[]).await.unwrap();
        Some(rows.iter().map(row_to_json).collect())
    })
}

#[test]
fn live_scalar_types() {
    let Some(rows) = live_rows(
        "SELECT 42::INT2, 70000::INT4, 1152921504606846977::INT8, 1.5::FLOAT8, 0.1::FLOAT4, \
                123.450::DECIMAL(10,3), 'x'::STRING, true, '\\x0102'::BYTES, \
                'deadbeef-0001-0203-0405-060708090a0b'::UUID, '{\"a\":[1]}'::JSONB, \
                '2024-02-29'::DATE, '12:30:45.5'::TIME, '2024-03-05 12:30:45.5'::TIMESTAMP, \
                '2024-03-05 12:30:45.5+00'::TIMESTAMPTZ, '1 year 2 mons 3 days 01:02:03'::INTERVAL, \
                '10.0.0.0/24'::INET, B'10110'::VARBIT, 'NaN'::FLOAT8, 'Infinity'::DECIMAL, 123456789012345678901234567890.123::DECIMAL",
    ) else {
        return;
    };
    let r = &rows[0];
    assert_eq!(r[0], json!(42));
    assert_eq!(r[1], json!(70000));
    assert_eq!(r[2], json!("1152921504606846977"));
    assert_eq!(r[3], json!(1.5));
    assert_eq!(r[4], json!(0.1));
    assert_eq!(r[5], json!("123.450"));
    assert_eq!(r[6], json!("x"));
    assert_eq!(r[7], json!(true));
    assert!(r[8].as_str().unwrap().starts_with("BLOB:2:"));
    assert_eq!(r[9], json!("deadbeef-0001-0203-0405-060708090a0b"));
    assert_eq!(r[10], json!({"a": [1]}));
    assert_eq!(r[11], json!("2024-02-29"));
    assert_eq!(r[12], json!("12:30:45.5"));
    assert_eq!(r[13], json!("2024-03-05T12:30:45.5"));
    assert_eq!(r[14], json!("2024-03-05T12:30:45.5Z"));
    assert_eq!(r[15], json!("1 year 2 mons 3 days 01:02:03"));
    assert_eq!(r[16], json!("10.0.0.0/24"));
    assert_eq!(r[17], json!("10110"));
    assert_eq!(r[18], json!("NaN"));
    assert_eq!(r[19], json!("Infinity"));
    assert_eq!(r[20], json!("123456789012345678901234567890.123"));
}

#[test]
fn live_arrays_and_nulls() {
    let Some(rows) = live_rows(
        "SELECT ARRAY[1,2,NULL]::INT8[], ARRAY['a','b']::STRING[], ARRAY[1152921504606846977]::INT8[], \
                ARRAY[1.5,2.25]::DECIMAL[], '{}'::INT4[], NULL::INT8",
    ) else {
        return;
    };
    let r = &rows[0];
    assert_eq!(r[0], json!([1, 2, null]));
    assert_eq!(r[1], json!(["a", "b"]));
    assert_eq!(r[2], json!(["1152921504606846977"]));
    assert_eq!(r[3], json!(["1.5", "2.25"]));
    assert_eq!(r[4], json!([]));
    assert!(r[5].is_null());
}

#[test]
fn live_extension_like_types() {
    let Some(rows) = live_rows(
        "SELECT '12:30:45.5+05:30'::TIMETZ, 'POINT(1 2)'::GEOMETRY, '[1,2.5,3]'::VECTOR, \
                to_tsvector('simple', 'fat cat fat'), to_tsquery('simple', 'a & (b | c) & !d'), \
                ARRAY['{\"x\":1}'::JSONB], 'a'::\"char\", 'ab'::CHAR(4), '::1'::INET, 5::OID, \
                ARRAY['2024-01-01'::DATE], ARRAY['1 day'::INTERVAL]",
    ) else {
        return;
    };
    let r = &rows[0];
    assert_eq!(r[0], json!("12:30:45.5+05:30"));
    assert_eq!(r[1], json!("0101000000000000000000f03f0000000000000040"));
    assert_eq!(r[2], json!("[1,2.5,3]"));
    assert_eq!(r[3], json!("'cat':2 'fat':1,3"));
    assert_eq!(r[4], json!("'a' & ('b' | 'c') & !'d'"));
    assert_eq!(r[5], json!([{"x": 1}]));
    assert_eq!(r[6], json!("a"));
    assert_eq!(r[7], json!("ab  "));
    assert_eq!(r[8], json!("::1"));
    assert_eq!(r[9], json!(5));
    assert_eq!(r[10], json!(["2024-01-01"]));
    assert_eq!(r[11], json!(["1 day"]));
}

#[test]
fn live_enum_and_enum_array() {
    let host = match std::env::var("COCKROACH_HOST") {
        Ok(h) => h,
        Err(_) => return,
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
        client
            .batch_execute(
                "DROP TYPE IF EXISTS live_mood; CREATE TYPE live_mood AS ENUM ('sad','ok')",
            )
            .await
            .unwrap();
        let rows = client
            .query(
                "SELECT 'ok'::live_mood, ARRAY['sad','ok']::live_mood[]",
                &[],
            )
            .await
            .unwrap();
        let r = row_to_json(&rows[0]);
        assert_eq!(r[0], json!("ok"));
        assert_eq!(r[1], json!(["sad", "ok"]));
        let cols = rows[0].columns();
        assert_eq!(canonical_type_name(cols[0].type_()), "ENUM(live_mood)");
        assert_eq!(canonical_type_name(cols[1].type_()), "ENUM(live_mood)[]");
        client.batch_execute("DROP TYPE live_mood").await.unwrap();
    });
}
