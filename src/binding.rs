//! Value binding for INSERT / UPDATE / filters: JSON from the host -> SQL
//! fragment plus an optional typed bind parameter. See
//! `docs/architecture/03-types.md`.
//!
//! Every parameter is pinned to an explicit wire type (`prepare_typed`), so
//! the client never guesses from the SQL. Values whose Rust type would not
//! match the column (DECIMAL, temporal, arrays, enums, ...) are sent as TEXT
//! and converted by the server with `CAST($N AS <type>)`, which keeps
//! precision and lets CockroachDB parse its own literal formats.
//!
//! Cascade, in order (the order matters: enum and DECIMAL run before the
//! generic numeric and text steps):
//! `DEFAULT` -> `NULL` -> BLOB wire format -> enum -> BOOL -> INT2/4/8 ->
//! DECIMAL -> FLOAT4/8 -> UUID -> JSONB -> temporal -> arrays -> other
//! types with a cast -> TEXT.

use bytes::BytesMut;
use serde_json::Value;
use tokio_postgres::types::{to_sql_checked, IsNull, ToSql, Type};

use crate::error::PluginError;
use crate::utils::blob::{decode_blob, resolve_blob_file_ref, DEFAULT_MAX_BLOB_SIZE};

pub type PgParam = Box<dyn ToSql + Sync + Send>;
pub type TypedPgParam = (PgParam, Type);

pub struct BoundValue {
    pub sql: String,
    pub param: Option<TypedPgParam>,
}

impl std::fmt::Debug for BoundValue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BoundValue")
            .field("sql", &self.sql)
            .field("param", &self.param.as_ref().map(|(_, t)| t.clone()))
            .finish()
    }
}

#[derive(Default)]
pub struct BindOptions<'a> {
    /// Canonical column type, e.g. `INT8`, `DECIMAL(10,2)`, `STRING[]`.
    pub column_type: Option<&'a str>,
    /// Schema-qualified, already-quoted enum type (`"public"."mood"`) when
    /// the column is an enum.
    pub enum_type: Option<&'a str>,
    pub allow_default: bool,
    /// Largest file accepted for a `BLOB_FILE_REF`; the host's `maxBlobSize`.
    /// `None` means the default (100 MiB).
    pub max_blob_size: Option<u64>,
}

pub const USE_DEFAULT_SENTINEL: &str = "__USE_DEFAULT__";

/// A UUID sent in its 16-byte binary form.
#[derive(Debug)]
struct RawUuid([u8; 16]);

impl ToSql for RawUuid {
    fn to_sql(
        &self,
        _: &Type,
        out: &mut BytesMut,
    ) -> Result<IsNull, Box<dyn std::error::Error + Sync + Send>> {
        out.extend_from_slice(&self.0);
        Ok(IsNull::No)
    }
    fn accepts(ty: &Type) -> bool {
        *ty == Type::UUID
    }
    to_sql_checked!();
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Bool,
    Int(u8),
    Float(u8),
    Decimal,
    Text,
    Bytes,
    Uuid,
    Json,
    Temporal,
    Array,
    Other,
}

fn kind_of(column_type: &str) -> Kind {
    let upper = column_type.trim().to_ascii_uppercase();
    if upper.ends_with("[]") || upper.starts_with("ARRAY") {
        return Kind::Array;
    }
    let base = upper.split('(').next().unwrap_or("").trim();
    match base {
        "BOOL" | "BOOLEAN" => Kind::Bool,
        "INT2" | "SMALLINT" | "INT16" => Kind::Int(2),
        "INT4" => Kind::Int(4),
        "INT" | "INT8" | "INTEGER" | "BIGINT" | "INT64" | "SERIAL" | "BIGSERIAL" => Kind::Int(8),
        "FLOAT4" | "REAL" => Kind::Float(4),
        "FLOAT" | "FLOAT8" | "DOUBLE PRECISION" | "FLOAT64" => Kind::Float(8),
        "DECIMAL" | "NUMERIC" | "DEC" => Kind::Decimal,
        "STRING" | "TEXT" | "VARCHAR" | "CHAR" | "CHARACTER" | "CHARACTER VARYING" | "NAME"
        | "CITEXT" | "\"CHAR\"" | "BPCHAR" => Kind::Text,
        "BYTES" | "BYTEA" | "BLOB" => Kind::Bytes,
        "UUID" => Kind::Uuid,
        "JSON" | "JSONB" => Kind::Json,
        "DATE"
        | "TIME"
        | "TIMETZ"
        | "TIMESTAMP"
        | "TIMESTAMPTZ"
        | "INTERVAL"
        | "TIME WITH TIME ZONE"
        | "TIME WITHOUT TIME ZONE"
        | "TIMESTAMP WITH TIME ZONE"
        | "TIMESTAMP WITHOUT TIME ZONE" => Kind::Temporal,
        _ => Kind::Other,
    }
}

/// A type name that is safe to put in `CAST($N AS <type>)`.
fn safe_type(column_type: &str) -> Result<String, PluginError> {
    let t = column_type.trim();
    let ok = !t.is_empty()
        && t.chars().all(|c| {
            c.is_ascii_alphanumeric() || matches!(c, ' ' | '_' | '(' | ')' | ',' | '[' | ']' | '.')
        });
    if ok {
        Ok(t.to_string())
    } else {
        Err(PluginError::invalid_params(format!(
            "unsupported column type '{column_type}'"
        )))
    }
}

fn err(msg: impl Into<String>) -> PluginError {
    PluginError::invalid_params(msg)
}

fn ph(idx: usize) -> String {
    format!("${idx}")
}

fn plain(idx: usize, param: PgParam, ty: Type) -> BoundValue {
    BoundValue {
        sql: ph(idx),
        param: Some((param, ty)),
    }
}

/// `CAST($N AS <target>)` with the placeholder pinned to TEXT.
fn cast_text(idx: usize, text: String, target: &str) -> Result<BoundValue, PluginError> {
    Ok(cast_text_unchecked(idx, text, &safe_type(target)?))
}

/// `target` must already be validated (see `safe_type`, `enum_type_checked`).
fn cast_text_unchecked(idx: usize, text: String, target: &str) -> BoundValue {
    BoundValue {
        sql: format!("CAST(${idx} AS {target})"),
        param: Some((Box::new(text), Type::TEXT)),
    }
}

fn null() -> BoundValue {
    BoundValue {
        sql: "NULL".into(),
        param: None,
    }
}

/// Bind a JSON value to a SQL fragment + optional typed parameter.
pub fn bind_value(value: Value, idx: usize, opts: &BindOptions) -> Result<BoundValue, PluginError> {
    // 1. DEFAULT sentinel.
    if let Value::String(s) = &value {
        if opts.allow_default && s == USE_DEFAULT_SENTINEL {
            return Ok(BoundValue {
                sql: "DEFAULT".into(),
                param: None,
            });
        }
    }
    // 2. NULL.
    if value.is_null() {
        return Ok(null());
    }

    let kind = opts.column_type.map(kind_of);

    // 3. BLOB wire format, for BYTES columns (or when the type is unknown).
    //    `BLOB_FILE_REF:` is a file the user picked; an unreadable or oversized
    //    file is an error, never silently stored as the text of its own path.
    if let Value::String(s) = &value {
        if matches!(kind, Some(Kind::Bytes) | None) {
            if s.starts_with("BLOB_FILE_REF:") {
                let max = opts.max_blob_size.unwrap_or(DEFAULT_MAX_BLOB_SIZE);
                let bytes = resolve_blob_file_ref(s, max).map_err(PluginError::invalid_params)?;
                return Ok(plain(idx, Box::new(bytes), Type::BYTEA));
            }
            if let Some(bytes) = decode_blob(s) {
                return Ok(plain(idx, Box::new(bytes), Type::BYTEA));
            }
        }
    }

    // 4. Enum columns bind as text and cast, whatever the JSON shape.
    if let Some(enum_type) = opts.enum_type {
        let text = match &value {
            Value::String(s) => s.clone(),
            Value::Array(_) | Value::Object(_) => return Err(err("enum value must be a string")),
            other => other.to_string(),
        };
        return Ok(cast_text_unchecked(
            idx,
            text,
            enum_type_checked(enum_type)?,
        ));
    }

    match value {
        Value::Bool(b) => bind_bool_value(b, idx, kind, opts),
        Value::Number(n) => bind_number(n, idx, kind, opts),
        Value::String(s) => bind_string(s, idx, kind, opts),
        Value::Array(arr) => bind_array(arr, idx, kind, opts),
        Value::Object(_) => match kind {
            Some(Kind::Json) => Ok(plain(idx, Box::new(value), Type::JSONB)),
            _ => Err(err("cannot bind a JSON object to a non-JSON column")),
        },
        Value::Null => Ok(null()),
    }
}

/// Quoted enum names (`"public"."mood"`) contain quotes, which `safe_type`
/// rejects, so validate them separately: identifiers separated by dots, with
/// `"` doubled inside quotes.
fn enum_type_checked(t: &str) -> Result<&str, PluginError> {
    let mut in_quotes = false;
    let mut chars = t.chars().peekable();
    while let Some(c) = chars.next() {
        match (c, in_quotes) {
            ('"', true) if chars.peek() == Some(&'"') => {
                chars.next();
            }
            ('"', _) => in_quotes = !in_quotes,
            (_, true) => {}
            ('.', false) => {}
            (c, false) if c.is_ascii_alphanumeric() || c == '_' => {}
            _ => return Err(err("invalid enum type name")),
        }
    }
    if in_quotes || t.is_empty() {
        return Err(err("invalid enum type name"));
    }
    Ok(t)
}

fn bind_bool_value(
    b: bool,
    idx: usize,
    kind: Option<Kind>,
    opts: &BindOptions,
) -> Result<BoundValue, PluginError> {
    match kind {
        Some(Kind::Json) => Ok(plain(idx, Box::new(Value::Bool(b)), Type::JSONB)),
        Some(Kind::Text) => Ok(plain(idx, Box::new(b.to_string()), Type::TEXT)),
        Some(Kind::Int(_)) => bind_int(i64::from(b), idx, kind),
        Some(
            Kind::Temporal
            | Kind::Other
            | Kind::Array
            | Kind::Decimal
            | Kind::Float(_)
            | Kind::Uuid
            | Kind::Bytes,
        ) => {
            let target = opts.column_type.unwrap_or("STRING");
            cast_text(idx, b.to_string(), target)
        }
        _ => Ok(plain(idx, Box::new(b), Type::BOOL)),
    }
}

fn bind_int(v: i64, idx: usize, kind: Option<Kind>) -> Result<BoundValue, PluginError> {
    match kind {
        Some(Kind::Int(2)) => {
            let v = i16::try_from(v).map_err(|_| err(format!("{v} is out of range for INT2")))?;
            Ok(plain(idx, Box::new(v), Type::INT2))
        }
        Some(Kind::Int(4)) => {
            let v = i32::try_from(v).map_err(|_| err(format!("{v} is out of range for INT4")))?;
            Ok(plain(idx, Box::new(v), Type::INT4))
        }
        _ => Ok(plain(idx, Box::new(v), Type::INT8)),
    }
}

fn bind_float(v: f64, idx: usize, kind: Option<Kind>) -> BoundValue {
    match kind {
        Some(Kind::Float(4)) => plain(idx, Box::new(v as f32), Type::FLOAT4),
        _ => plain(idx, Box::new(v), Type::FLOAT8),
    }
}

fn bind_number(
    n: serde_json::Number,
    idx: usize,
    kind: Option<Kind>,
    opts: &BindOptions,
) -> Result<BoundValue, PluginError> {
    match kind {
        Some(Kind::Int(_)) => {
            if let Some(i) = n.as_i64() {
                return bind_int(i, idx, kind);
            }
            match n.as_f64() {
                Some(f) if f.fract() == 0.0 && f.abs() < 9.2e18 => bind_int(f as i64, idx, kind),
                _ => Err(err(format!("{n} is not a valid integer"))),
            }
        }
        Some(Kind::Decimal) => cast_text(idx, n.to_string(), opts.column_type.unwrap_or("DECIMAL")),
        Some(Kind::Float(_)) => n
            .as_f64()
            .map(|f| bind_float(f, idx, kind))
            .ok_or_else(|| err("unsupported numeric value")),
        Some(Kind::Bool) => match n.as_i64() {
            Some(0) => Ok(plain(idx, Box::new(false), Type::BOOL)),
            Some(1) => Ok(plain(idx, Box::new(true), Type::BOOL)),
            _ => Err(err(format!("cannot bind {n} as BOOL"))),
        },
        Some(Kind::Text) => Ok(plain(idx, Box::new(n.to_string()), Type::TEXT)),
        Some(Kind::Json) => Ok(plain(idx, Box::new(Value::Number(n)), Type::JSONB)),
        Some(Kind::Temporal | Kind::Other) => {
            cast_text(idx, n.to_string(), opts.column_type.unwrap_or("STRING"))
        }
        Some(Kind::Bytes | Kind::Uuid | Kind::Array) => Err(err(format!(
            "cannot bind a number to a {} column",
            opts.column_type.unwrap_or("?")
        ))),
        None => {
            if let Some(i) = n.as_i64() {
                Ok(plain(idx, Box::new(i), Type::INT8))
            } else {
                n.as_f64()
                    .map(|f| plain(idx, Box::new(f), Type::FLOAT8))
                    .ok_or_else(|| err("unsupported numeric value"))
            }
        }
    }
}

fn parse_bool(s: &str) -> Option<bool> {
    match s.trim().to_ascii_lowercase().as_str() {
        "true" | "t" | "yes" | "y" | "on" | "1" => Some(true),
        "false" | "f" | "no" | "n" | "off" | "0" => Some(false),
        _ => None,
    }
}

/// Plain decimal literal (`-12.5e3`) or one of NaN / Infinity / -Infinity.
pub fn is_decimal_literal(s: &str) -> bool {
    let t = s.trim();
    let lower = t.to_ascii_lowercase();
    if matches!(
        lower.as_str(),
        "nan" | "infinity" | "-infinity" | "+infinity" | "inf" | "-inf"
    ) {
        return true;
    }
    let body = t.strip_prefix(['+', '-']).unwrap_or(t);
    let (mantissa, exp) = match body.split_once(['e', 'E']) {
        Some((m, e)) => (m, Some(e)),
        None => (body, None),
    };
    if let Some(e) = exp {
        let e = e.strip_prefix(['+', '-']).unwrap_or(e);
        if e.is_empty() || !e.bytes().all(|b| b.is_ascii_digit()) {
            return false;
        }
    }
    let (int, frac) = mantissa.split_once('.').unwrap_or((mantissa, ""));
    (!int.is_empty() || !frac.is_empty())
        && int.bytes().all(|b| b.is_ascii_digit())
        && frac.bytes().all(|b| b.is_ascii_digit())
}

fn parse_uuid(s: &str) -> Option<[u8; 16]> {
    let t = s.trim().trim_start_matches('{').trim_end_matches('}');
    let t = t.strip_prefix("urn:uuid:").unwrap_or(t);
    let hex: String = t.chars().filter(|c| *c != '-').collect();
    if hex.len() != 32 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    // Hyphens, when present, must sit at the canonical positions.
    if t.contains('-')
        && (t.len() != 36 || [8, 13, 18, 23].iter().any(|&i| t.as_bytes()[i] != b'-'))
    {
        return None;
    }
    let mut out = [0u8; 16];
    for (i, b) in out.iter_mut().enumerate() {
        *b = u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16).ok()?;
    }
    Some(out)
}

fn decode_hex(s: &str) -> Option<Vec<u8>> {
    let h = s.strip_prefix("\\x")?;
    if h.len() % 2 != 0 || !h.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    (0..h.len() / 2)
        .map(|i| u8::from_str_radix(&h[i * 2..i * 2 + 2], 16).ok())
        .collect()
}

fn bind_string(
    s: String,
    idx: usize,
    kind: Option<Kind>,
    opts: &BindOptions,
) -> Result<BoundValue, PluginError> {
    let target = opts.column_type.unwrap_or("STRING");
    match kind {
        Some(Kind::Bool) => parse_bool(&s)
            .map(|b| plain(idx, Box::new(b), Type::BOOL))
            .ok_or_else(|| err(format!("cannot bind '{s}' as BOOL"))),
        Some(Kind::Int(_)) => {
            // Numeric strings: an INT8 key always round-trips as a string.
            let v: i64 = s
                .trim()
                .parse()
                .map_err(|_| err(format!("cannot bind '{s}' as an integer")))?;
            bind_int(v, idx, kind)
        }
        Some(Kind::Decimal) => {
            if is_decimal_literal(&s) {
                cast_text(idx, s.trim().to_string(), target)
            } else {
                Err(err(format!("cannot bind '{s}' as DECIMAL")))
            }
        }
        Some(Kind::Float(_)) => {
            let f: f64 = s
                .trim()
                .parse()
                .map_err(|_| err(format!("cannot bind '{s}' as a float")))?;
            Ok(bind_float(f, idx, kind))
        }
        Some(Kind::Uuid) => parse_uuid(&s)
            .map(|u| plain(idx, Box::new(RawUuid(u)), Type::UUID))
            .ok_or_else(|| err(format!("'{s}' is not a valid UUID"))),
        Some(Kind::Json) => {
            let v = serde_json::from_str::<Value>(&s).unwrap_or(Value::String(s));
            Ok(plain(idx, Box::new(v), Type::JSONB))
        }
        Some(Kind::Bytes) => {
            let bytes = decode_hex(&s).unwrap_or_else(|| s.into_bytes());
            Ok(plain(idx, Box::new(bytes), Type::BYTEA))
        }
        Some(Kind::Temporal | Kind::Other) => cast_text(idx, s, target),
        Some(Kind::Array) => {
            let t = s.trim();
            if t.starts_with('[') {
                if let Ok(Value::Array(arr)) = serde_json::from_str::<Value>(t) {
                    return cast_text(idx, pg_array_literal(&arr)?, target);
                }
            }
            // Already an array literal: `{1,2,3}`.
            cast_text(idx, s, target)
        }
        Some(Kind::Text) | None => Ok(plain(idx, Box::new(s), Type::TEXT)),
    }
}

fn bind_array(
    arr: Vec<Value>,
    idx: usize,
    kind: Option<Kind>,
    opts: &BindOptions,
) -> Result<BoundValue, PluginError> {
    match kind {
        Some(Kind::Json) => Ok(plain(idx, Box::new(Value::Array(arr)), Type::JSONB)),
        Some(Kind::Array) => cast_text(
            idx,
            pg_array_literal(&arr)?,
            opts.column_type.unwrap_or("STRING[]"),
        ),
        Some(_) => Err(err("cannot bind an array to a non-array column")),
        None => Err(err("binding an array needs the column type")),
    }
}

/// Text form of a one-dimensional array: `{1,"a b",NULL}`. Strings are
/// always quoted, with `\` and `"` escaped; objects become quoted JSON.
pub fn pg_array_literal(arr: &[Value]) -> Result<String, PluginError> {
    let quote = |s: &str| format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""));
    let mut parts = Vec::with_capacity(arr.len());
    for v in arr {
        parts.push(match v {
            Value::Null => "NULL".to_string(),
            Value::Bool(b) => b.to_string(),
            Value::Number(n) => n.to_string(),
            Value::String(s) => quote(s),
            Value::Array(inner) => pg_array_literal(inner)?,
            Value::Object(_) => quote(&v.to_string()),
        });
    }
    Ok(format!("{{{}}}", parts.join(",")))
}

/// Quote a schema-qualified enum type for [`BindOptions::enum_type`].
pub fn quote_enum_type(schema: &str, name: &str) -> String {
    format!(
        "{}.{}",
        crate::utils::identifiers::quote_identifier(schema, '"'),
        crate::utils::identifiers::quote_identifier(name, '"')
    )
}

#[cfg(test)]
#[path = "binding_tests.rs"]
mod tests;
