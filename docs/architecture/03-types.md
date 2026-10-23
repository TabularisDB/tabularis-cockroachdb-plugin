# 03 · Types

## Canonical names

CockroachDB normalizes PostgreSQL aliases when it stores a column type.
`SHOW CREATE TABLE` and `information_schema.columns.crdb_sql_type` always
return the canonical name. The column editor (manifest `data_types`) offers
canonical names only. The host's generic types are mapped by `type_mappings`.

| Canonical | Accepted aliases | Notes |
| --- | --- | --- |
| `INT8` | `INT`, `INTEGER`, `BIGINT`, `INT64` | `INT` is 8 bytes unless the session sets `default_int_size = 4` |
| `INT4` | `INT`/`INTEGER` when `default_int_size = 4` | |
| `INT2` | `SMALLINT` | |
| `FLOAT8` | `FLOAT`, `DOUBLE PRECISION`, `FLOAT64` | |
| `FLOAT4` | `REAL` | |
| `DECIMAL(p,s)` | `NUMERIC`, `DEC` | Arbitrary precision; also accepts `NaN` and `±Infinity` |
| `STRING` | `TEXT`, `CHARACTER VARYING` without length | |
| `VARCHAR(n)` | `STRING(n)`, `CHARACTER VARYING(n)` | Shown as `VARCHAR(n)` |
| `CHAR(n)` | `CHARACTER(n)` | |
| `BOOL` | `BOOLEAN` | |
| `BYTES` | `BYTEA`, `BLOB` | |
| `JSONB` | `JSON` | Always binary JSON; there is no separate text `JSON` |
| `TIMESTAMP`, `TIMESTAMPTZ`, `DATE`, `TIME`, `TIMETZ`, `INTERVAL` | `TIMESTAMP WITHOUT/WITH TIME ZONE` | |
| `UUID`, `INET`, `BIT(n)`, `VARBIT(n)`, `OID` | | No `CIDR`, no `MACADDR`, no `MONEY` |
| `GEOMETRY`, `GEOGRAPHY` | `GEOMETRY(POINT, 4326)` etc. | Spatial, built in |
| `TSVECTOR`, `TSQUERY` | | v23.1+ |
| `VECTOR(n)` | | v24.2+, pgvector-compatible |
| `<type>[]` | `ARRAY` | One dimension only; no arrays of arrays, no `JSONB[]` before v23.1 |
| `SERIAL`, `BIGSERIAL`, `SMALLSERIAL` | | Not stored. Become `INT8 DEFAULT unique_rowid()` with the default `serial_normalization = rowid` |

Gate the version-dependent ones in the editor through connection metadata
(`connection_metadata`, see [08](./08-capabilities.md)), or reject them with
the server's error.

## Decoding results

Use the binary protocol through `tokio-postgres` typed rows, and fall back to
text for OIDs without a Rust decoder. CockroachDB uses PostgreSQL's OIDs for
built-in types.

| Type (OID) | JSON value sent to the host |
| --- | --- |
| `INT2` (21), `INT4` (23) | number |
| `INT8` (20) | number when within ±(2^53 − 1), **string otherwise** |
| `FLOAT4` (700), `FLOAT8` (701) | number; `NaN`, `Infinity`, `-Infinity` as strings |
| `DECIMAL` (1700) | string, never a float |
| `BOOL` (16) | `true` / `false` |
| `STRING` (25), `VARCHAR` (1043), `CHAR` (1042), `NAME` (19) | string |
| `BYTES` (17) | the host's BLOB wire format (as the PostgreSQL plugin does) |
| `UUID` (2950) | string, lowercase hyphenated |
| `JSONB` (3802) | the JSON value itself, not a string holding JSON |
| `DATE`, `TIME`, `TIMETZ`, `TIMESTAMP`, `TIMESTAMPTZ` | ISO 8601 strings; keep the offset for `TZ` types |
| `INTERVAL` (1186) | CockroachDB text form (`1 day 02:00:00`) |
| `INET` (869) | string |
| `BIT` / `VARBIT` | string of `0`/`1` |
| Arrays | JSON arrays of the element mapping; `NULL` elements as `null` |
| Enums | string label |
| `GEOMETRY` / `GEOGRAPHY` | text form from the server (hex EWKB); a WKT/GeoJSON view is a UI concern |
| `VECTOR`, `TSVECTOR`, `TSQUERY`, `OID`, unknown | text form |

**`INT8` precision is the important one.** `unique_rowid()` values are around
2^60 and every `SERIAL` column produces them. A JSON number that large loses
its low bits in the JavaScript UI, so editing or deleting a row by such a key
would target the wrong row. Send `INT8` values outside the safe range as
strings, and accept them back as strings (see binding below).

User-defined types (enums) have OIDs above the built-in range. Resolve their
names once per connection and cache them:

```sql
SELECT t.oid, n.nspname, t.typname, t.typtype
FROM pg_catalog.pg_type t
JOIN pg_catalog.pg_namespace n ON n.oid = t.typnamespace
WHERE t.oid = ANY($1)
```

`typtype = 'e'` marks an enum. Report the column type as `ENUM(<name>)` and
an array of enums as `ENUM(<name>)[]`, so the editor can offer the labels.

## Binding parameters (insert, update, filters)

Values arrive from the host as JSON. Bind them with a strict ordered cascade,
as the PostgreSQL plugin's `binding.rs` does, with these CockroachDB
differences:

1. `DEFAULT` sentinel: emit `DEFAULT`, bind nothing.
2. `NULL`.
3. BLOB wire format for `BYTES` columns.
4. Enum columns: bind as text and cast: `$1::<schema>.<enum>`.
5. `BOOL`: JSON booleans, and the strings `true`/`false`/`t`/`f`/`1`/`0`.
6. `INT2`/`INT4`/`INT8`: JSON numbers **and numeric strings**. An `INT8` key
   always comes back as a string when it was sent as one.
7. `DECIMAL`: bind as text and cast `$1::DECIMAL`, to keep precision.
8. `FLOAT4`/`FLOAT8`: numbers, plus `NaN`/`Infinity` strings.
9. Temporal types: bind as text and cast to the column type. CockroachDB
   parses the ISO forms.
10. `UUID`: validate the shape, bind as `uuid::Uuid`.
11. `JSONB`: bind the JSON value as `serde_json::Value`. A string that holds
    JSON is parsed first.
12. Arrays: JSON arrays, bound as text in array-literal form and cast to
    `<element>[]`.
13. `INET`, `BIT`, `INTERVAL`, spatial, `VECTOR`: text with a cast to the
    column type.
14. Fallback: text.

The order matters: the enum and `DECIMAL` steps must run before the generic
numeric and text ones. Unit-test the cascade without a server, the way the
PostgreSQL plugin's `binding_tests.rs` does.

## Type families and import mapping

The manifest's `type_mappings` cover generic types the host infers on paste
and import:

| Generic | CockroachDB |
| --- | --- |
| `DATETIME` | `TIMESTAMP` |
| `JSON` | `JSONB` |
| `TEXT` | `STRING` |
| `BLOB` | `BYTES` |

Integers map to `INT8`, floats to `FLOAT8`, booleans to `BOOL`. These are
already the host's defaults for PostgreSQL-family drivers; add them to
`type_mappings` if the host's defaults turn out to differ.
