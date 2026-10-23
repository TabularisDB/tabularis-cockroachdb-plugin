//! Catalog SQL builders and pure row shaping for the metadata handlers.
//! See `docs/architecture/02-introspection.md`.
//!
//! Everything here is free of I/O so it can be unit-tested. Numeric and
//! boolean-ish catalog columns are cast in SQL (`::INT8`, `::STRING`) so the
//! typed `tokio-postgres` decoders always see the types they expect.

use serde_json::{json, Value};

/// Schemas never shown in the sidebar. `defaultdb` and `postgres` are
/// databases, not schemas, and are never hidden.
pub const HIDDEN_SCHEMAS: [&str; 4] = [
    "crdb_internal",
    "information_schema",
    "pg_catalog",
    "pg_extension",
];

pub const DATABASES_SQL: &str = "SELECT database_name::STRING FROM [SHOW DATABASES] \
     WHERE database_name <> 'system' ORDER BY database_name";

pub const SCHEMAS_SQL: &str = "SELECT schema_name::STRING FROM information_schema.schemata \
     WHERE schema_name NOT IN ('crdb_internal', 'information_schema', 'pg_catalog', 'pg_extension') \
       AND schema_name NOT LIKE 'pg!_temp!_%' ESCAPE '!' \
     ORDER BY schema_name";

/// Table comments for a whole schema (`$1`). `pg_description` also holds the
/// comments of built-in functions, whose OIDs collide with table OIDs, so the
/// catalog (`classoid`) must be pinned.
pub const TABLE_COMMENTS_SQL: &str = "SELECT c.relname::STRING, d.description::STRING \
     FROM pg_catalog.pg_class c \
     JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace \
     JOIN pg_catalog.pg_description d ON d.objoid = c.oid AND d.objsubid = 0 \
            AND d.classoid = 'pg_catalog.pg_class'::REGCLASS \
     WHERE n.nspname = $1";

/// Name of a table's primary key constraint (`$1` schema, `$2` table).
pub const PK_NAME_SQL: &str =
    "SELECT constraint_name::STRING FROM information_schema.table_constraints \
     WHERE constraint_type = 'PRIMARY KEY' AND table_schema = $1 AND table_name = $2";

pub const VIEW_DEFINITION_SQL: &str =
    "SELECT view_definition::STRING FROM information_schema.views \
     WHERE table_schema = $1 AND table_name = $2";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RelKind {
    Table,
    View,
}

impl RelKind {
    /// `type` value in `SHOW TABLES`.
    pub fn show_type(self) -> &'static str {
        match self {
            Self::Table => "table",
            Self::View => "view",
        }
    }

    /// `table_type` value in `information_schema.tables` (fallback source).
    pub fn info_schema_type(self) -> &'static str {
        match self {
            Self::Table => "BASE TABLE",
            Self::View => "VIEW",
        }
    }
}

/// `SHOW ... FROM` statements cannot bind, so identifiers are quoted.
pub fn quote_ident(name: &str) -> String {
    crate::utils::identifiers::quote_identifier(name, '"')
}

pub fn qualified(schema: &str, name: &str) -> String {
    format!("{}.{}", quote_ident(schema), quote_ident(name))
}

pub fn show_tables_sql(database: &str, schema: &str) -> String {
    format!(
        "SELECT table_name::STRING, type::STRING, estimated_row_count::INT8, locality::STRING \
         FROM [SHOW TABLES FROM {}.{}] WHERE type = $1 ORDER BY table_name",
        quote_ident(database),
        quote_ident(schema)
    )
}

pub const INFO_SCHEMA_TABLES_SQL: &str =
    "SELECT table_name::STRING FROM information_schema.tables \
     WHERE table_schema = $1 AND table_type = $2 ORDER BY table_name";

/// Which tables a column / foreign-key query covers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TableFilter {
    /// `$2` is one table name.
    One,
    /// `$2` is an array of table names.
    Any,
    /// Every table in the schema.
    All,
}

impl TableFilter {
    fn predicate(self, column: &str) -> String {
        match self {
            Self::One => format!("{column} = $2"),
            Self::Any => format!("{column} = ANY($2)"),
            Self::All => "TRUE".to_string(),
        }
    }
}

/// Column query. Result columns, in order:
/// 0 table_name, 1 name, 2 crdb_sql_type, 3 data_type, 4 udt_name,
/// 5 is_nullable, 6 default, 7 is_identity, 8 is_generated,
/// 9 generation_expression, 10 char length, 11 numeric precision,
/// 12 numeric scale, 13 comment, 14 is_pk.
///
/// The primary-key sub-select joins `key_column_usage` on **table name as
/// well as constraint name**: every primary key is called `<table>_pkey`, so
/// without the table predicate each table's key marks columns of every other
/// table with a same-named column.
pub fn columns_sql(filter: TableFilter) -> String {
    format!(
        "SELECT c.table_name::STRING, c.column_name::STRING, c.crdb_sql_type::STRING, \
                c.data_type::STRING, c.udt_name::STRING, c.is_nullable::STRING, \
                c.column_default::STRING, c.is_identity::STRING, c.is_generated::STRING, \
                c.generation_expression::STRING, c.character_maximum_length::INT8, \
                c.numeric_precision::INT8, c.numeric_scale::INT8, d.description::STRING, \
                (pk.column_name IS NOT NULL) AS is_pk \
         FROM information_schema.columns c \
         LEFT JOIN pg_catalog.pg_namespace n ON n.nspname = c.table_schema \
         LEFT JOIN pg_catalog.pg_class cls ON cls.relname = c.table_name AND cls.relnamespace = n.oid \
         LEFT JOIN pg_catalog.pg_description d ON d.objoid = cls.oid AND d.objsubid = c.ordinal_position \
                AND d.classoid = 'pg_catalog.pg_class'::REGCLASS \
         LEFT JOIN ( \
             SELECT kcu.table_name, kcu.column_name \
             FROM information_schema.table_constraints tc \
             JOIN information_schema.key_column_usage kcu \
               ON tc.constraint_name = kcu.constraint_name \
              AND tc.table_schema = kcu.table_schema \
              AND tc.table_name = kcu.table_name \
             WHERE tc.constraint_type = 'PRIMARY KEY' \
               AND tc.table_schema = $1 \
               AND {pk_filter} \
         ) pk ON pk.column_name = c.column_name AND pk.table_name = c.table_name \
         WHERE c.table_schema = $1 AND {col_filter} AND c.is_hidden = 'NO' \
         ORDER BY c.table_name, c.ordinal_position",
        pk_filter = filter.predicate("tc.table_name"),
        col_filter = filter.predicate("c.table_name"),
    )
}

/// Foreign-key query. Result columns, in order:
/// 0 table, 1 constraint, 2 ref_schema, 3 ref_table, 4 update action,
/// 5 delete action, 6 position, 7 column, 8 ref column.
///
/// `conkey` and `confkey` are unnested together so column `i` pairs with
/// referenced column `i` (two independent joins would produce a cross
/// product for composite keys).
pub fn foreign_keys_sql(filter: TableFilter) -> String {
    format!(
        "SELECT cl.relname::STRING, c.conname::STRING, ref_ns.nspname::STRING, ref_cl.relname::STRING, \
                c.confupdtype::STRING, c.confdeltype::STRING, k.ord::INT8, \
                a.attname::STRING, ra.attname::STRING \
         FROM pg_catalog.pg_constraint c \
         JOIN pg_catalog.pg_class cl ON cl.oid = c.conrelid \
         JOIN pg_catalog.pg_namespace ns ON ns.oid = cl.relnamespace \
         JOIN pg_catalog.pg_class ref_cl ON ref_cl.oid = c.confrelid \
         JOIN pg_catalog.pg_namespace ref_ns ON ref_ns.oid = ref_cl.relnamespace \
         CROSS JOIN LATERAL unnest(c.conkey, c.confkey) WITH ORDINALITY AS k(col, ref_col, ord) \
         JOIN pg_catalog.pg_attribute a ON a.attrelid = c.conrelid AND a.attnum = k.col \
         JOIN pg_catalog.pg_attribute ra ON ra.attrelid = c.confrelid AND ra.attnum = k.ref_col \
         WHERE c.contype = 'f' AND ns.nspname = $1 AND {} \
         ORDER BY cl.relname, c.conname, k.ord",
        filter.predicate("cl.relname")
    )
}

pub fn fk_action(code: &str) -> &'static str {
    match code {
        "r" => "RESTRICT",
        "c" => "CASCADE",
        "n" => "SET NULL",
        "d" => "SET DEFAULT",
        _ => "NO ACTION",
    }
}

pub fn is_yes(s: &str) -> bool {
    s.eq_ignore_ascii_case("YES")
}

/// A defaulted value the server generates: `unique_rowid()`, a sequence, a
/// random UUID, or an identity column.
pub fn is_auto_increment(default: Option<&str>, is_identity: bool) -> bool {
    if is_identity {
        return true;
    }
    default.is_some_and(|d| {
        let d = d.trim_start();
        d.starts_with("unique_rowid(")
            || d.starts_with("nextval(")
            || d.starts_with("gen_random_uuid(")
    })
}

/// `DECIMAL(10,2)` -> `DECIMAL`, `VARCHAR(50)[]` -> `VARCHAR[]`. The length
/// and precision travel in their own fields; leaving them in the name makes
/// the host emit them twice.
pub fn base_type_name(sql_type: &str) -> String {
    match (sql_type.find('('), sql_type.rfind(')')) {
        (Some(open), Some(close)) if close > open => {
            format!("{}{}", &sql_type[..open], &sql_type[close + 1..])
        }
        _ => sql_type.to_string(),
    }
}

/// Raw row of the column query.
#[derive(Debug, Clone, Default)]
pub struct ColumnRow {
    pub table: String,
    pub name: String,
    pub sql_type: String,
    pub data_type: String,
    pub udt_name: String,
    pub nullable: String,
    pub default: Option<String>,
    pub is_identity: String,
    pub is_generated: String,
    pub generation_expression: Option<String>,
    pub char_len: Option<i64>,
    pub num_precision: Option<i64>,
    pub num_scale: Option<i64>,
    pub comment: Option<String>,
    pub is_pk: bool,
}

pub fn column_json(r: &ColumnRow) -> Value {
    let data_type = if r.data_type.eq_ignore_ascii_case("USER-DEFINED") {
        format!("ENUM({})", r.udt_name)
    } else {
        base_type_name(&r.sql_type)
    };
    // Precision and scale are only modifiers on DECIMAL; integer and float
    // columns report their bit width there, which must not reach the DDL.
    let keep_precision = data_type.to_ascii_uppercase().starts_with("DECIMAL");
    let generated = is_yes(&r.is_generated) || r.is_generated.eq_ignore_ascii_case("ALWAYS");
    json!({
        "name": r.name,
        "data_type": data_type,
        "is_nullable": is_yes(&r.nullable),
        "default_value": r.default,
        "is_pk": r.is_pk,
        "is_auto_increment": is_auto_increment(r.default.as_deref(), is_yes(&r.is_identity)),
        "character_maximum_length": r.char_len,
        "numeric_precision": r.num_precision.filter(|_| keep_precision),
        "numeric_scale": r.num_scale.filter(|_| keep_precision),
        "comment": r.comment.as_deref().filter(|c| !c.is_empty()),
        "is_generated": generated,
        "generation_expression": r.generation_expression.as_deref().filter(|g| !g.is_empty()),
    })
}

/// Raw row of the foreign-key query.
#[derive(Debug, Clone, Default)]
pub struct FkRow {
    pub table: String,
    pub name: String,
    pub ref_schema: String,
    pub ref_table: String,
    pub on_update: String,
    pub on_delete: String,
    pub position: i64,
    pub column: String,
    pub ref_column: String,
}

pub fn fk_json(r: &FkRow) -> Value {
    json!({
        "name": r.name,
        "column_name": r.column,
        "ref_schema": r.ref_schema,
        "ref_table": r.ref_table,
        "ref_column": r.ref_column,
        "on_update": fk_action(&r.on_update),
        "on_delete": fk_action(&r.on_delete),
        "seq_in_fk": r.position,
    })
}

/// Raw row of `SHOW INDEXES`.
#[derive(Debug, Clone, Default)]
pub struct IndexRow {
    pub name: String,
    pub non_unique: bool,
    pub seq: i64,
    pub column: String,
    pub storing: bool,
    pub implicit: bool,
    pub visible: bool,
}

/// True when `index_name` is the primary key index. Prefer the constraint
/// name looked up from `information_schema`; fall back to the naming
/// convention (`<table>_pkey`, or `primary` on old clusters).
pub fn is_primary_index(index_name: &str, pk_constraint: Option<&str>) -> bool {
    match pk_constraint {
        Some(pk) => index_name == pk,
        None => index_name == "primary" || index_name.ends_with("_pkey"),
    }
}

/// Group `SHOW INDEXES` rows (one per column) into one entry per index.
/// Implicit columns CockroachDB added to the key are skipped; `STORING`
/// columns are reported separately from the key columns.
pub fn group_indexes(mut rows: Vec<IndexRow>, pk_constraint: Option<&str>) -> Vec<Value> {
    rows.sort_by_key(|r| r.seq);
    let mut order: Vec<String> = Vec::new();
    let mut groups: std::collections::HashMap<String, (Vec<String>, Vec<String>, bool, bool)> =
        Default::default();
    for r in rows {
        let entry = groups.entry(r.name.clone()).or_insert_with(|| {
            order.push(r.name.clone());
            (Vec::new(), Vec::new(), !r.non_unique, r.visible)
        });
        if r.implicit {
            continue;
        }
        if r.storing {
            entry.1.push(r.column);
        } else {
            entry.0.push(r.column);
        }
    }
    order
        .into_iter()
        .map(|name| {
            let (columns, mut included, unique, visible) = groups.remove(&name).unwrap_or_default();
            let primary = is_primary_index(&name, pk_constraint);
            if primary {
                // The primary index stores every other column; not useful to show.
                included.clear();
            }
            json!({
                "index_name": name,
                "columns": columns,
                "included_columns": included,
                "is_unique": unique,
                "is_primary": primary,
                "is_visible": visible,
            })
        })
        .collect()
}

#[cfg(test)]
#[path = "introspection_tests.rs"]
mod tests;
