//! DDL generation for the schema-management dialogs: create table, add and
//! alter column, create index, create foreign key, and the statements behind
//! the drop and view operations.
//!
//! Pure: every function builds SQL text from its input and touches no database,
//! so it is unit-tested without a server. Two rules keep generated SQL safe:
//!
//! - Names are always quoted (`"` doubled), and comment text is a quoted
//!   literal (`'` doubled; `standard_conforming_strings` is always on).
//! - The only free-form SQL the host sends is a column's type, a column's
//!   default expression and a view's `SELECT`. Each is checked with the SQL
//!   scanner before it is spliced in, so none of them can end the statement
//!   (`;`), start a comment that swallows what follows (`--`, `/*`), or leave a
//!   quote or parenthesis open.
//!
//! What CockroachDB does differently from PostgreSQL, verified on v24.3 and
//! v26.3 (see `docs/architecture/04-ddl.md`):
//! - the primary key constraint is named `<table>_pkey`;
//! - `SERIAL` means `INT8 DEFAULT unique_rowid()`, and `SERIAL4`/`SMALLSERIAL`
//!   keep the narrower width;
//! - a column cannot be added with `PRIMARY KEY` when the table already has one:
//!   the column is added, then `ALTER PRIMARY KEY` replaces the key;
//! - `ALTER COLUMN TYPE` needs `USING` when the data is converted, and on
//!   v24.3 a conversion that rewrites data also needs
//!   `SET enable_experimental_alter_column_type_general = true`;
//! - `DROP INDEX` must name the table (`schema.table@index`), because index
//!   names are only unique per table;
//! - `CREATE OR REPLACE VIEW` can append columns but not change or drop them.

use std::collections::HashSet;

use serde_json::Value;

use crate::error::PluginError;
use crate::introspection::{qualified, quote_ident};
use crate::sqlscan::{scan, Kind};

/// A column as the host's dialogs describe it.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ColumnDef {
    pub name: String,
    pub data_type: String,
    pub is_nullable: bool,
    pub is_pk: bool,
    pub is_auto_increment: bool,
    pub default_value: Option<String>,
    pub comment: Option<String>,
}

fn invalid(msg: impl Into<String>) -> PluginError {
    PluginError::invalid_params(msg)
}

fn non_empty(v: Option<&Value>) -> Option<String> {
    v.and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

impl ColumnDef {
    pub fn from_value(v: &Value) -> Result<Self, PluginError> {
        let obj = v
            .as_object()
            .ok_or_else(|| invalid("a column definition must be an object"))?;
        let flag =
            |key: &str, default: bool| obj.get(key).and_then(Value::as_bool).unwrap_or(default);
        Ok(Self {
            name: non_empty(obj.get("name")).ok_or_else(|| invalid("a column needs a name"))?,
            data_type: non_empty(obj.get("data_type"))
                .ok_or_else(|| invalid("a column needs a data_type"))?,
            is_nullable: flag("is_nullable", true),
            is_pk: flag("is_pk", false),
            is_auto_increment: flag("is_auto_increment", false),
            default_value: non_empty(obj.get("default_value")),
            comment: non_empty(obj.get("comment")),
        })
    }
}

// ---- validating free-form text ------------------------------------------------

/// A type such as `INT8`, `VARCHAR(255)`, `DECIMAL(10,2)`, `STRING[]` or an
/// enum name (`app.mood`, `"my type"`). Only the characters those need are
/// accepted, with balanced parentheses and quotes.
pub fn check_type(raw: &str) -> Result<String, PluginError> {
    let t = raw.trim();
    let ok_chars = t.chars().all(|c| {
        c.is_ascii_alphanumeric()
            || matches!(c, ' ' | '_' | '(' | ')' | ',' | '[' | ']' | '.' | '"')
    });
    let mut depth = 0i32;
    for c in t.chars() {
        match c {
            '(' => depth += 1,
            ')' => depth -= 1,
            _ => {}
        }
        if depth < 0 {
            break;
        }
    }
    if t.is_empty()
        || t.len() > 200
        || !ok_chars
        || depth != 0
        || !t.matches('"').count().is_multiple_of(2)
    {
        return Err(invalid(format!("'{raw}' is not a valid column type")));
    }
    Ok(t.to_string())
}

/// A default expression (`now()`, `'new'`, `gen_random_uuid()`, `(a + 1)`). It
/// must be one complete expression: no `;`, no comment, nothing left open.
pub fn check_expression(raw: &str, what: &str) -> Result<String, PluginError> {
    let e = raw.trim();
    let scanned = scan(e);
    let mut depth = 0i32;
    let mut bad = e.is_empty() || !scanned.complete;
    for t in &scanned.tokens {
        match t.kind {
            Kind::Semi | Kind::Comment => bad = true,
            Kind::Open => depth += 1,
            Kind::Close => depth -= 1,
            _ => {}
        }
        if depth < 0 {
            bad = true;
        }
    }
    if bad || depth != 0 {
        return Err(invalid(format!(
            "{what} must be a single complete SQL expression (no ';', no comments, quotes and parentheses closed)"
        )));
    }
    Ok(e.to_string())
}

/// A view's `SELECT`. A trailing `;` is dropped; a `;` anywhere else would
/// start a second statement. Comments are fine (nothing follows the text).
pub fn check_definition(raw: &str) -> Result<String, PluginError> {
    let scanned = scan(raw);
    if !scanned.complete {
        return Err(invalid(
            "the view definition has an unterminated string, quoted name or comment",
        ));
    }
    let code: Vec<_> = scanned
        .tokens
        .iter()
        .filter(|t| t.kind != Kind::Comment)
        .collect();
    let mut end = code.len();
    while end > 0 && code[end - 1].kind == Kind::Semi {
        end -= 1;
    }
    if end == 0 {
        return Err(invalid("the view definition is empty"));
    }
    if code[..end].iter().any(|t| t.kind == Kind::Semi) {
        return Err(invalid("the view definition must be a single statement"));
    }
    Ok(raw[..code[end - 1].end].trim().to_string())
}

fn require_name(value: &str, what: &str) -> Result<(), PluginError> {
    if value.trim().is_empty() {
        Err(invalid(format!("{what} must not be empty")))
    } else {
        Ok(())
    }
}

fn quote_literal(text: &str) -> String {
    format!("'{}'", text.replace('\'', "''"))
}

// ---- types ----------------------------------------------------------------------

/// Upper-cased type without its `(..)` modifiers: `varchar(50)` -> `VARCHAR`,
/// `decimal(10,2)[]` -> `DECIMAL[]`.
pub fn base_type(t: &str) -> String {
    let upper = t.trim().to_ascii_uppercase();
    match (upper.find('('), upper.rfind(')')) {
        (Some(open), Some(close)) if close > open => {
            format!("{}{}", upper[..open].trim_end(), &upper[close + 1..])
        }
        _ => upper,
    }
}

/// The `SERIAL` spelling for an integer column that is auto-incrementing, or
/// `None` for any other type (a UUID that defaults to `gen_random_uuid()` is
/// "auto-generated" too, but it is not a serial).
fn serial_type(data_type: &str) -> Option<&'static str> {
    match base_type(data_type).as_str() {
        "INT2" | "SMALLINT" | "SMALLSERIAL" | "SERIAL2" => Some("SMALLSERIAL"),
        "INT4" | "SERIAL4" => Some("SERIAL4"),
        "INT" | "INT8" | "INTEGER" | "BIGINT" | "INT64" | "SERIAL" | "SERIAL8" | "BIGSERIAL" => {
            Some("SERIAL")
        }
        _ => None,
    }
}

/// Types whose values convert to each other implicitly, so no `USING` is
/// needed and nothing is rewritten.
const COMPATIBLE_GROUPS: &[&[&str]] = &[
    &[
        "INT",
        "INT2",
        "INT4",
        "INT8",
        "INT64",
        "INTEGER",
        "SMALLINT",
        "BIGINT",
        "SERIAL",
        "SMALLSERIAL",
        "BIGSERIAL",
    ],
    &[
        "FLOAT",
        "FLOAT4",
        "FLOAT8",
        "REAL",
        "DOUBLE PRECISION",
        "DECIMAL",
        "NUMERIC",
        "DEC",
    ],
    &[
        "STRING",
        "TEXT",
        "VARCHAR",
        "CHAR",
        "CHARACTER",
        "CHARACTER VARYING",
        "NAME",
        "BPCHAR",
    ],
    &[
        "TIMESTAMP",
        "TIMESTAMPTZ",
        "TIMESTAMP WITH TIME ZONE",
        "TIMESTAMP WITHOUT TIME ZONE",
    ],
    &[
        "TIME",
        "TIMETZ",
        "TIME WITH TIME ZONE",
        "TIME WITHOUT TIME ZONE",
    ],
    &["JSON", "JSONB"],
    &["BIT", "VARBIT"],
];

fn same_family(a: &str, b: &str) -> bool {
    a == b
        || COMPATIBLE_GROUPS
            .iter()
            .any(|g| g.contains(&a) && g.contains(&b))
}

// ---- statements ------------------------------------------------------------------

/// `"name" TYPE [NOT NULL] [DEFAULT expr]`. A serial column carries its own
/// default and nullability. A primary key column is always `NOT NULL`.
fn column_definition(col: &ColumnDef) -> Result<String, PluginError> {
    require_name(&col.name, "a column name")?;
    let ty = check_type(&col.data_type)?;
    let serial = if col.is_auto_increment {
        serial_type(&ty)
    } else {
        None
    };
    let mut def = format!("{} {}", quote_ident(&col.name), serial.unwrap_or(&ty));
    if serial.is_none() {
        if !col.is_nullable || col.is_pk {
            def.push_str(" NOT NULL");
        }
        if let Some(d) = &col.default_value {
            def.push_str(&format!(
                " DEFAULT {}",
                check_expression(d, "a default value")?
            ));
        }
    }
    Ok(def)
}

fn comment_statement(schema: &str, table: &str, column: &str, comment: Option<&str>) -> String {
    format!(
        "COMMENT ON COLUMN {}.{} IS {}",
        qualified(schema, table),
        quote_ident(column),
        comment.map_or_else(|| "NULL".to_string(), quote_literal)
    )
}

/// `CREATE TABLE`, then one `COMMENT ON COLUMN` per described column. A table
/// without a primary key gets a warning comment: CockroachDB would add a hidden
/// `rowid` column and use it as the key.
pub fn build_create_table(
    schema: &str,
    table: &str,
    columns: &[ColumnDef],
) -> Result<Vec<String>, PluginError> {
    require_name(table, "the table name")?;
    if columns.is_empty() {
        return Err(invalid("a table needs at least one column"));
    }
    let mut seen = HashSet::new();
    for c in columns {
        require_name(&c.name, "a column name")?;
        if !seen.insert(c.name.as_str()) {
            return Err(invalid(format!(
                "column \"{}\" is defined more than once",
                c.name
            )));
        }
    }

    let mut lines = columns
        .iter()
        .map(column_definition)
        .collect::<Result<Vec<_>, _>>()?;
    let pk: Vec<String> = columns
        .iter()
        .filter(|c| c.is_pk)
        .map(|c| quote_ident(&c.name))
        .collect();
    let has_pk = !pk.is_empty();
    if has_pk {
        lines.push(format!(
            "CONSTRAINT {} PRIMARY KEY ({})",
            quote_ident(&format!("{table}_pkey")),
            pk.join(", ")
        ));
    }

    let mut create = format!(
        "CREATE TABLE {} (\n  {}\n)",
        qualified(schema, table),
        lines.join(",\n  ")
    );
    if !has_pk {
        create = format!(
            "-- Warning: no primary key. CockroachDB will add a hidden rowid column and use it as the key.\n{create}"
        );
    }
    let mut out = vec![create];
    out.extend(columns.iter().filter_map(|c| {
        c.comment
            .as_deref()
            .map(|text| comment_statement(schema, table, &c.name, Some(text)))
    }));
    Ok(out)
}

/// `ALTER TABLE .. ADD COLUMN`. CockroachDB rejects `ADD COLUMN .. PRIMARY KEY`
/// on a table that already has a key, so a primary key column is added first and
/// then `ALTER PRIMARY KEY` makes it part of the key, next to `existing_pk` (the
/// table's current key columns, empty for a table keyed by the hidden `rowid`).
pub fn build_add_column(
    schema: &str,
    table: &str,
    column: &ColumnDef,
    existing_pk: &[String],
) -> Result<Vec<String>, PluginError> {
    require_name(table, "the table name")?;
    let mut out = vec![format!(
        "ALTER TABLE {} ADD COLUMN {}",
        qualified(schema, table),
        column_definition(column)?
    )];
    if column.is_pk {
        let mut key: Vec<String> = existing_pk.to_vec();
        if !key.contains(&column.name) {
            key.push(column.name.clone());
        }
        let cols: Vec<String> = key.iter().map(|c| quote_ident(c)).collect();
        out.push(format!(
            "ALTER TABLE {} ALTER PRIMARY KEY USING COLUMNS ({})",
            qualified(schema, table),
            cols.join(", ")
        ));
    }
    if let Some(text) = &column.comment {
        out.push(comment_statement(schema, table, &column.name, Some(text)));
    }
    Ok(out)
}

fn normalized(t: &str) -> String {
    t.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_ascii_uppercase()
}

/// One statement per change: rename, type, nullability, default, comment. Fails
/// with "No changes detected" when old and new are the same.
pub fn build_alter_column(
    schema: &str,
    table: &str,
    old: &ColumnDef,
    new: &ColumnDef,
) -> Result<Vec<String>, PluginError> {
    require_name(table, "the table name")?;
    require_name(&new.name, "a column name")?;
    if old.is_pk != new.is_pk {
        return Err(invalid(
            "changing whether an existing column is part of the primary key is not supported here; \
             use ALTER PRIMARY KEY in the SQL editor",
        ));
    }
    let target = qualified(schema, table);
    let col = quote_ident(&new.name);
    let mut out = Vec::new();

    if old.name != new.name {
        out.push(format!(
            "ALTER TABLE {target} RENAME COLUMN {} TO {col}",
            quote_ident(&old.name)
        ));
    }

    if normalized(&old.data_type) != normalized(&new.data_type) {
        let new_type = check_type(&new.data_type)?;
        let (old_base, new_base) = (base_type(&old.data_type), base_type(&new_type));
        let alter = format!("ALTER TABLE {target} ALTER COLUMN {col} TYPE {new_type}");
        if same_family(&old_base, &new_base) {
            out.push(alter);
        } else {
            // The data is converted, so the cast is explicit. On v24.3 a conversion that
            // rewrites data also needs a session setting; it cannot be emitted as its own
            // statement because the host runs each statement on its own connection.
            out.push(format!(
                "-- Converts the column's data. On CockroachDB v24.3 this needs `SET enable_experimental_alter_column_type_general = true` in the same session; run both together as one script.\n{alter} USING {col}::{new_type}"
            ));
        }
    }

    if old.is_nullable != new.is_nullable {
        let action = if new.is_nullable {
            "DROP NOT NULL"
        } else {
            "SET NOT NULL"
        };
        out.push(format!("ALTER TABLE {target} ALTER COLUMN {col} {action}"));
    }

    if old.default_value != new.default_value {
        out.push(match &new.default_value {
            Some(d) => format!(
                "ALTER TABLE {target} ALTER COLUMN {col} SET DEFAULT {}",
                check_expression(d, "a default value")?
            ),
            None => format!("ALTER TABLE {target} ALTER COLUMN {col} DROP DEFAULT"),
        });
    }

    if old.comment != new.comment {
        out.push(comment_statement(
            schema,
            table,
            &new.name,
            new.comment.as_deref(),
        ));
    }

    if out.is_empty() {
        return Err(PluginError::internal("No changes detected"));
    }
    Ok(out)
}

pub fn build_create_index(
    schema: &str,
    table: &str,
    index_name: &str,
    columns: &[String],
    unique: bool,
) -> Result<String, PluginError> {
    require_name(table, "the table name")?;
    if columns.is_empty() || columns.iter().any(|c| c.trim().is_empty()) {
        return Err(invalid("an index needs at least one column"));
    }
    let cols: Vec<String> = columns.iter().map(|c| quote_ident(c)).collect();
    let name = if index_name.trim().is_empty() {
        String::new()
    } else {
        format!("{} ", quote_ident(index_name))
    };
    Ok(format!(
        "CREATE {}INDEX {name}ON {} ({})",
        if unique { "UNIQUE " } else { "" },
        qualified(schema, table),
        cols.join(", ")
    ))
}

/// Canonical text of a foreign key action, or `None` when none was given.
pub fn normalize_action(raw: Option<&str>) -> Result<Option<&'static str>, PluginError> {
    let Some(text) = raw.map(str::trim).filter(|s| !s.is_empty()) else {
        return Ok(None);
    };
    match normalized(text).as_str() {
        "NO ACTION" => Ok(Some("NO ACTION")),
        "RESTRICT" => Ok(Some("RESTRICT")),
        "CASCADE" => Ok(Some("CASCADE")),
        "SET NULL" => Ok(Some("SET NULL")),
        "SET DEFAULT" => Ok(Some("SET DEFAULT")),
        _ => Err(invalid(format!(
            "'{text}' is not a foreign key action (NO ACTION, RESTRICT, CASCADE, SET NULL or SET DEFAULT)"
        ))),
    }
}

#[allow(clippy::too_many_arguments)]
pub fn build_create_foreign_key(
    schema: &str,
    table: &str,
    fk_name: &str,
    column: &str,
    ref_table: &str,
    ref_column: &str,
    on_delete: Option<&str>,
    on_update: Option<&str>,
) -> Result<String, PluginError> {
    require_name(table, "the table name")?;
    require_name(fk_name, "the constraint name")?;
    require_name(column, "the column")?;
    require_name(ref_table, "the referenced table")?;
    require_name(ref_column, "the referenced column")?;
    let mut sql = format!(
        "ALTER TABLE {} ADD CONSTRAINT {} FOREIGN KEY ({}) REFERENCES {} ({})",
        qualified(schema, table),
        quote_ident(fk_name),
        quote_ident(column),
        qualified(schema, ref_table),
        quote_ident(ref_column)
    );
    if let Some(action) = normalize_action(on_delete)? {
        sql.push_str(&format!(" ON DELETE {action}"));
    }
    if let Some(action) = normalize_action(on_update)? {
        sql.push_str(&format!(" ON UPDATE {action}"));
    }
    Ok(sql)
}

/// `DROP INDEX schema.table@index`. Index names are only unique per table, so a
/// bare `schema.index` is ambiguous when two tables share the name.
pub fn build_drop_index(schema: &str, table: &str, index: &str) -> Result<String, PluginError> {
    require_name(table, "the table name")?;
    require_name(index, "the index name")?;
    Ok(format!(
        "DROP INDEX {}@{}",
        qualified(schema, table),
        quote_ident(index)
    ))
}

pub fn build_drop_foreign_key(
    schema: &str,
    table: &str,
    fk_name: &str,
) -> Result<String, PluginError> {
    require_name(table, "the table name")?;
    require_name(fk_name, "the constraint name")?;
    Ok(format!(
        "ALTER TABLE {} DROP CONSTRAINT {}",
        qualified(schema, table),
        quote_ident(fk_name)
    ))
}

pub fn build_create_view(
    schema: &str,
    view: &str,
    definition: &str,
) -> Result<String, PluginError> {
    require_name(view, "the view name")?;
    Ok(format!(
        "CREATE VIEW {} AS {}",
        qualified(schema, view),
        check_definition(definition)?
    ))
}

pub fn build_replace_view(
    schema: &str,
    view: &str,
    definition: &str,
) -> Result<String, PluginError> {
    require_name(view, "the view name")?;
    Ok(format!(
        "CREATE OR REPLACE VIEW {} AS {}",
        qualified(schema, view),
        check_definition(definition)?
    ))
}

pub fn build_drop_view(schema: &str, view: &str) -> Result<String, PluginError> {
    require_name(view, "the view name")?;
    Ok(format!("DROP VIEW IF EXISTS {}", qualified(schema, view)))
}

/// SQLSTATE CockroachDB reports when `CREATE OR REPLACE VIEW` would change or
/// drop an existing column (`invalid_table_definition`).
pub const VIEW_REPLACE_INCOMPATIBLE: &str = "42P16";

#[cfg(test)]
#[path = "ddl_tests.rs"]
mod tests;
