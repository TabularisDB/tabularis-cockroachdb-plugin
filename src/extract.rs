//! Result decoding: PostgreSQL binary wire values -> JSON for the host.
//! See `docs/architecture/03-types.md`.
//!
//! Decoding works on raw bytes keyed by the column's type, so it covers
//! types `tokio-postgres` has no decoder for (DECIMAL, INTERVAL, INET, BIT,
//! TIMETZ, enums, arrays of those, ...). [`decode`] is pure and unit-tested.

use serde_json::{json, Map, Value};
use tokio_postgres::types::{FromSql, Kind, Type};
use tokio_postgres::Row;

use crate::utils::blob::encode_blob;

/// JavaScript's largest exactly representable integer.
pub const MAX_SAFE_INT: i64 = (1 << 53) - 1;

type DecodeResult = Result<Value, String>;

/// Accepts every type and keeps the undecoded bytes.
struct Raw<'a>(&'a [u8]);

impl<'a> FromSql<'a> for Raw<'a> {
    fn from_sql(_: &Type, raw: &'a [u8]) -> Result<Self, Box<dyn std::error::Error + Sync + Send>> {
        Ok(Raw(raw))
    }
    fn accepts(_: &Type) -> bool {
        true
    }
}

/// JSON value of one cell. NULL is `null`; a value that cannot be decoded
/// becomes a string describing the problem instead of failing the row.
pub fn cell_to_json(row: &Row, idx: usize) -> Value {
    let ty = row.columns()[idx].type_();
    match row.try_get::<_, Option<Raw>>(idx) {
        Ok(None) => Value::Null,
        Ok(Some(Raw(bytes))) => decode(ty, bytes)
            .unwrap_or_else(|e| Value::String(format!("<undecodable {}: {e}>", ty.name()))),
        Err(e) => Value::String(format!("<error: {e}>")),
    }
}

/// `INT8` outside the JS safe range is sent as a string, because a JSON
/// number that large loses its low bits in the UI (every `unique_rowid()`
/// value is around 2^60).
pub fn int_to_json(v: i64) -> Value {
    if (-MAX_SAFE_INT..=MAX_SAFE_INT).contains(&v) {
        json!(v)
    } else {
        Value::String(v.to_string())
    }
}

pub fn float_to_json(v: f64) -> Value {
    if v.is_nan() {
        Value::String("NaN".into())
    } else if v.is_infinite() {
        Value::String(if v > 0.0 { "Infinity" } else { "-Infinity" }.into())
    } else {
        json!(v)
    }
}

fn need(raw: &[u8], n: usize, what: &str) -> Result<(), String> {
    if raw.len() < n {
        Err(format!("{what}: expected {n} bytes, got {}", raw.len()))
    } else {
        Ok(())
    }
}

fn be_i16(raw: &[u8], at: usize) -> i16 {
    i16::from_be_bytes([raw[at], raw[at + 1]])
}
fn be_i32(raw: &[u8], at: usize) -> i32 {
    i32::from_be_bytes(raw[at..at + 4].try_into().unwrap())
}
fn be_i64(raw: &[u8], at: usize) -> i64 {
    i64::from_be_bytes(raw[at..at + 8].try_into().unwrap())
}

fn text(raw: &[u8]) -> Value {
    Value::String(String::from_utf8_lossy(raw).into_owned())
}

pub fn hex(raw: &[u8]) -> String {
    raw.iter().map(|b| format!("{b:02x}")).collect()
}

/// Decode one non-NULL value.
pub fn decode(ty: &Type, raw: &[u8]) -> DecodeResult {
    match ty.kind() {
        Kind::Array(inner) => return decode_array(inner, raw),
        Kind::Enum(_) => return Ok(text(raw)),
        _ => {}
    }
    Ok(match *ty {
        Type::BOOL => {
            need(raw, 1, "bool")?;
            Value::Bool(raw[0] != 0)
        }
        Type::INT2 => {
            need(raw, 2, "int2")?;
            json!(be_i16(raw, 0))
        }
        Type::INT4 => {
            need(raw, 4, "int4")?;
            json!(be_i32(raw, 0))
        }
        Type::INT8 => {
            need(raw, 8, "int8")?;
            int_to_json(be_i64(raw, 0))
        }
        Type::OID => {
            need(raw, 4, "oid")?;
            json!(u32::from_be_bytes(raw[..4].try_into().unwrap()))
        }
        Type::FLOAT4 => {
            need(raw, 4, "float4")?;
            let f = f32::from_be_bytes(raw[..4].try_into().unwrap());
            // Shortest f32 text, so 0.1f32 is 0.1 and not 0.10000000149.
            float_to_json(f.to_string().parse::<f64>().unwrap_or(f as f64))
        }
        Type::FLOAT8 => {
            need(raw, 8, "float8")?;
            float_to_json(f64::from_be_bytes(raw[..8].try_into().unwrap()))
        }
        Type::NUMERIC => Value::String(decode_numeric(raw)?),
        Type::BYTEA => Value::String(encode_blob(raw)),
        Type::UUID => {
            need(raw, 16, "uuid")?;
            Value::String(format_uuid(raw))
        }
        Type::JSONB => {
            need(raw, 1, "jsonb")?;
            // First byte is the format version.
            serde_json::from_slice(&raw[1..]).unwrap_or_else(|_| text(&raw[1..]))
        }
        Type::JSON => serde_json::from_slice(raw).unwrap_or_else(|_| text(raw)),
        Type::DATE => {
            need(raw, 4, "date")?;
            Value::String(format_date(be_i32(raw, 0) as i64))
        }
        Type::TIME => {
            need(raw, 8, "time")?;
            Value::String(format_time(be_i64(raw, 0)))
        }
        Type::TIMETZ => {
            need(raw, 12, "timetz")?;
            // The zone is seconds *west* of UTC.
            let offset = -(be_i32(raw, 8) as i64);
            Value::String(format!(
                "{}{}",
                format_time(be_i64(raw, 0)),
                format_offset(offset)
            ))
        }
        Type::TIMESTAMP => {
            need(raw, 8, "timestamp")?;
            Value::String(format_timestamp(be_i64(raw, 0)))
        }
        Type::TIMESTAMPTZ => {
            need(raw, 8, "timestamptz")?;
            // Always UTC on the wire.
            Value::String(format!("{}Z", format_timestamp(be_i64(raw, 0))))
        }
        Type::INTERVAL => {
            need(raw, 16, "interval")?;
            Value::String(format_interval(
                be_i64(raw, 0),
                be_i32(raw, 8),
                be_i32(raw, 12),
            ))
        }
        Type::INET | Type::CIDR => Value::String(decode_inet(raw)?),
        Type::BIT | Type::VARBIT => Value::String(decode_bits(raw)?),
        Type::CHAR => {
            need(raw, 1, "char")?;
            Value::String((raw[0] as char).to_string())
        }
        Type::TEXT | Type::VARCHAR | Type::BPCHAR | Type::NAME | Type::UNKNOWN => text(raw),
        Type::TS_VECTOR => Value::String(decode_tsvector(raw)?),
        Type::TSQUERY => Value::String(decode_tsquery(raw)?),
        _ => match ty.name() {
            // Spatial types arrive as EWKB; the text form is its hex.
            "geometry" | "geography" | "box2d" => Value::String(hex(raw)),
            "vector" => Value::String(decode_vector(raw)?),
            _ => match std::str::from_utf8(raw) {
                Ok(s) => Value::String(s.to_string()),
                Err(_) => Value::String(hex(raw)),
            },
        },
    })
}

/// pgvector binary: u16 dim, u16 unused, then `dim` big-endian f32.
fn decode_vector(raw: &[u8]) -> Result<String, String> {
    need(raw, 4, "vector")?;
    let dim = u16::from_be_bytes([raw[0], raw[1]]) as usize;
    need(raw, 4 + dim * 4, "vector data")?;
    let items: Vec<String> = (0..dim)
        .map(|i| f32::from_be_bytes(raw[4 + i * 4..8 + i * 4].try_into().unwrap()).to_string())
        .collect();
    Ok(format!("[{}]", items.join(",")))
}

fn take_cstr(buf: &mut &[u8]) -> Result<String, String> {
    let end = buf
        .iter()
        .position(|b| *b == 0)
        .ok_or("unterminated lexeme")?;
    let s = String::from_utf8_lossy(&buf[..end]).into_owned();
    *buf = &buf[end + 1..];
    Ok(s)
}

const WEIGHTS: [char; 4] = ['D', 'C', 'B', 'A'];

/// `tsvector` binary: i32 count, then per lexeme `text\0`, u16 position
/// count and u16 entries (top 2 bits weight, low 14 bits position).
fn decode_tsvector(raw: &[u8]) -> Result<String, String> {
    need(raw, 4, "tsvector")?;
    let count = be_i32(raw, 0).max(0);
    let mut buf = &raw[4..];
    let mut out = Vec::new();
    for _ in 0..count {
        let word = take_cstr(&mut buf)?;
        need(buf, 2, "tsvector positions")?;
        let n = u16::from_be_bytes([buf[0], buf[1]]) as usize;
        buf = &buf[2..];
        need(buf, n * 2, "tsvector positions")?;
        let mut item = format!("'{}'", word.replace('\'', "''"));
        for i in 0..n {
            let v = u16::from_be_bytes([buf[i * 2], buf[i * 2 + 1]]);
            item.push(if i == 0 { ':' } else { ',' });
            item.push_str(&(v & 0x3FFF).to_string());
            let w = WEIGHTS[(v >> 14) as usize];
            if w != 'D' {
                item.push(w);
            }
        }
        buf = &buf[n * 2..];
        out.push(item);
    }
    Ok(out.join(" "))
}

/// `tsquery` binary: i32 node count, then a prefix-ordered node tree.
fn decode_tsquery(raw: &[u8]) -> Result<String, String> {
    need(raw, 4, "tsquery")?;
    if be_i32(raw, 0) == 0 {
        return Ok(String::new());
    }
    let mut buf = &raw[4..];
    tsquery_node(&mut buf, 4)
}

fn tsquery_node(buf: &mut &[u8], parent_level: u8) -> Result<String, String> {
    need(buf, 1, "tsquery node")?;
    match buf[0] {
        1 => {
            need(buf, 3, "tsquery operand")?;
            let (weight, prefix) = (buf[1], buf[2] == 1);
            *buf = &buf[3..];
            let mut s = format!("'{}'", take_cstr(buf)?.replace('\'', "''"));
            let w = match weight {
                1 => Some('D'),
                2 => Some('C'),
                4 => Some('B'),
                8 => Some('A'),
                _ => None,
            };
            if prefix || w.is_some() {
                s.push(':');
            }
            if prefix {
                s.push('*');
            }
            if let Some(w) = w {
                s.push(w);
            }
            Ok(s)
        }
        2 => {
            need(buf, 2, "tsquery operator")?;
            let op = buf[1];
            *buf = &buf[2..];
            let (level, sym) = match op {
                1 => return Ok(format!("!{}", tsquery_node(buf, 1)?)),
                2 => (3, "&".to_string()),
                3 => (4, "|".to_string()),
                4 => {
                    need(buf, 2, "tsquery distance")?;
                    let d = be_i16(buf, 0);
                    *buf = &buf[2..];
                    (
                        2,
                        if d == 1 {
                            "<->".to_string()
                        } else {
                            format!("<{d}>")
                        },
                    )
                }
                other => return Err(format!("bad tsquery operator {other}")),
            };
            let right = tsquery_node(buf, level)?;
            let left = tsquery_node(buf, level)?;
            Ok(if parent_level < level {
                format!("({left} {sym} {right})")
            } else {
                format!("{left} {sym} {right}")
            })
        }
        other => Err(format!("bad tsquery node {other}")),
    }
}

fn decode_array(inner: &Type, raw: &[u8]) -> DecodeResult {
    need(raw, 12, "array header")?;
    let ndim = be_i32(raw, 0);
    if ndim == 0 {
        return Ok(Value::Array(vec![]));
    }
    if !(1..=6).contains(&ndim) {
        return Err(format!("unsupported array dimension count {ndim}"));
    }
    let ndim = ndim as usize;
    need(raw, 12 + ndim * 8, "array dimensions")?;
    let dims: Vec<usize> = (0..ndim)
        .map(|d| be_i32(raw, 12 + d * 8).max(0) as usize)
        .collect();
    let mut pos = 12 + ndim * 8;

    fn level(
        inner: &Type,
        raw: &[u8],
        pos: &mut usize,
        dims: &[usize],
    ) -> Result<Vec<Value>, String> {
        let mut out = Vec::with_capacity(dims[0]);
        for _ in 0..dims[0] {
            if dims.len() > 1 {
                out.push(Value::Array(level(inner, raw, pos, &dims[1..])?));
                continue;
            }
            need(raw, *pos + 4, "array element length")?;
            let len = be_i32(raw, *pos);
            *pos += 4;
            if len < 0 {
                out.push(Value::Null);
            } else {
                let len = len as usize;
                need(raw, *pos + len, "array element")?;
                out.push(decode(inner, &raw[*pos..*pos + len])?);
                *pos += len;
            }
        }
        Ok(out)
    }
    Ok(Value::Array(level(inner, raw, &mut pos, &dims)?))
}

/// Binary `numeric`: ndigits, weight, sign, dscale, then base-10000 digits.
fn decode_numeric(raw: &[u8]) -> Result<String, String> {
    need(raw, 8, "numeric")?;
    let ndigits = be_i16(raw, 0).max(0) as usize;
    let weight = be_i16(raw, 2) as i32;
    let sign = u16::from_be_bytes([raw[4], raw[5]]);
    let dscale = be_i16(raw, 6).max(0) as usize;
    match sign {
        0xC000 => return Ok("NaN".into()),
        0xD000 => return Ok("Infinity".into()),
        0xF000 => return Ok("-Infinity".into()),
        0x0000 | 0x4000 => {}
        other => return Err(format!("bad numeric sign {other:#x}")),
    }
    need(raw, 8 + ndigits * 2, "numeric digits")?;
    let digit = |i: i32| -> i32 {
        if i < 0 || i as usize >= ndigits {
            0
        } else {
            be_i16(raw, 8 + i as usize * 2) as i32
        }
    };

    let mut int_part = String::new();
    for i in 0..=weight.max(-1) {
        let d = digit(i);
        if int_part.is_empty() {
            int_part = d.to_string();
        } else {
            int_part.push_str(&format!("{d:04}"));
        }
    }
    if int_part.is_empty() {
        int_part.push('0');
    }

    let mut frac = String::new();
    let mut i = weight + 1;
    while frac.len() < dscale {
        frac.push_str(&format!("{:04}", digit(i)));
        i += 1;
    }
    frac.truncate(dscale);

    let mut out = String::new();
    if sign == 0x4000 {
        out.push('-');
    }
    out.push_str(&int_part);
    if dscale > 0 {
        out.push('.');
        out.push_str(&frac);
    }
    Ok(out)
}

fn format_uuid(raw: &[u8]) -> String {
    let h = hex(&raw[..16]);
    format!(
        "{}-{}-{}-{}-{}",
        &h[..8],
        &h[8..12],
        &h[12..16],
        &h[16..20],
        &h[20..]
    )
}

/// Days since 1970-01-01 -> (year, month, day). Hinnant's civil_from_days.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// Days since 2000-01-01 (the PostgreSQL epoch).
fn format_date(days: i64) -> String {
    if days == i32::MAX as i64 {
        return "infinity".into();
    }
    if days == i32::MIN as i64 {
        return "-infinity".into();
    }
    let (y, m, d) = civil_from_days(days + 10_957);
    if y > 0 {
        format!("{y:04}-{m:02}-{d:02}")
    } else {
        // PostgreSQL has no year 0: 1 BC is astronomical year 0.
        format!("{:04}-{m:02}-{d:02} BC", 1 - y)
    }
}

fn format_time(micros: i64) -> String {
    let secs = micros.div_euclid(1_000_000);
    let frac = micros.rem_euclid(1_000_000);
    let mut s = format!("{:02}:{:02}:{:02}", secs / 3600, secs / 60 % 60, secs % 60);
    if frac != 0 {
        s.push_str(format!(".{frac:06}").trim_end_matches('0'));
    }
    s
}

fn format_timestamp(micros: i64) -> String {
    if micros == i64::MAX {
        return "infinity".into();
    }
    if micros == i64::MIN {
        return "-infinity".into();
    }
    let days = micros.div_euclid(86_400_000_000);
    let rest = micros.rem_euclid(86_400_000_000);
    format!("{}T{}", format_date(days), format_time(rest))
}

fn format_offset(seconds: i64) -> String {
    let sign = if seconds < 0 { '-' } else { '+' };
    let s = seconds.abs();
    if s % 3600 == 0 {
        format!("{sign}{:02}:00", s / 3600)
    } else {
        format!("{sign}{:02}:{:02}", s / 3600, s / 60 % 60)
    }
}

/// PostgreSQL / CockroachDB default interval style: `1 year 2 mons 3 days 04:05:06`.
fn format_interval(micros: i64, days: i32, months: i32) -> String {
    let mut parts: Vec<String> = Vec::new();
    let (years, mons) = (months / 12, months % 12);
    let unit =
        |n: i32, one: &str, many: &str| format!("{n} {}", if n.abs() == 1 { one } else { many });
    if years != 0 {
        parts.push(unit(years, "year", "years"));
    }
    if mons != 0 {
        parts.push(unit(mons, "mon", "mons"));
    }
    if days != 0 {
        parts.push(unit(days, "day", "days"));
    }
    if micros != 0 || parts.is_empty() {
        let sign = if micros < 0 { "-" } else { "" };
        parts.push(format!("{sign}{}", format_time(micros.abs())));
    }
    parts.join(" ")
}

fn decode_inet(raw: &[u8]) -> Result<String, String> {
    need(raw, 4, "inet")?;
    let (family, bits, is_cidr, len) = (raw[0], raw[1], raw[2] != 0, raw[3] as usize);
    need(raw, 4 + len, "inet address")?;
    let (addr, max) = match (family, len) {
        (2, 4) => {
            let a: [u8; 4] = raw[4..8].try_into().unwrap();
            (std::net::IpAddr::from(a).to_string(), 32)
        }
        (3, 16) => {
            let a: [u8; 16] = raw[4..20].try_into().unwrap();
            (std::net::IpAddr::from(a).to_string(), 128)
        }
        _ => return Err(format!("unexpected inet family {family} / length {len}")),
    };
    Ok(if is_cidr || bits != max {
        format!("{addr}/{bits}")
    } else {
        addr
    })
}

fn decode_bits(raw: &[u8]) -> Result<String, String> {
    need(raw, 4, "bit")?;
    let nbits = be_i32(raw, 0).max(0) as usize;
    need(raw, 4 + nbits.div_ceil(8), "bit data")?;
    Ok((0..nbits)
        .map(|i| {
            if raw[4 + i / 8] & (0x80 >> (i % 8)) != 0 {
                '1'
            } else {
                '0'
            }
        })
        .collect())
}

/// Display name of a column type for the host: canonical CockroachDB
/// spelling, `ENUM(<name>)` for enums, arrays as `<element>[]`.
pub fn canonical_type_name(ty: &Type) -> String {
    match ty.kind() {
        Kind::Array(inner) => return format!("{}[]", canonical_type_name(inner)),
        Kind::Enum(_) => return format!("ENUM({})", ty.name()),
        _ => {}
    }
    match *ty {
        Type::BOOL => "BOOL",
        Type::INT2 => "INT2",
        Type::INT4 => "INT4",
        Type::INT8 => "INT8",
        Type::FLOAT4 => "FLOAT4",
        Type::FLOAT8 => "FLOAT8",
        Type::NUMERIC => "DECIMAL",
        Type::TEXT | Type::UNKNOWN => "STRING",
        Type::VARCHAR => "VARCHAR",
        Type::BPCHAR => "CHAR",
        Type::NAME => "NAME",
        Type::CHAR => "\"char\"",
        Type::BYTEA => "BYTES",
        Type::JSON | Type::JSONB => "JSONB",
        Type::DATE => "DATE",
        Type::TIME => "TIME",
        Type::TIMETZ => "TIMETZ",
        Type::TIMESTAMP => "TIMESTAMP",
        Type::TIMESTAMPTZ => "TIMESTAMPTZ",
        Type::INTERVAL => "INTERVAL",
        Type::UUID => "UUID",
        Type::INET => "INET",
        Type::BIT => "BIT",
        Type::VARBIT => "VARBIT",
        Type::OID => "OID",
        _ => return ty.name().to_ascii_uppercase(),
    }
    .to_string()
}

/// Helper for building JSON rows.
pub fn row_to_json(row: &Row) -> Vec<Value> {
    (0..row.len()).map(|i| cell_to_json(row, i)).collect()
}

/// Column name -> value, for handlers that want objects.
pub fn row_to_object(row: &Row) -> Map<String, Value> {
    row.columns()
        .iter()
        .enumerate()
        .map(|(i, c)| (c.name().to_string(), cell_to_json(row, i)))
        .collect()
}

#[cfg(test)]
#[path = "extract_tests.rs"]
mod tests;
