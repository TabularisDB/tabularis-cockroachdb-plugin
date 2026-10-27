//! SQL for row editing (`insert_record`, `update_record`, `delete_record`) and
//! for reading one BLOB cell. Pure: it builds a statement and its bound
//! parameters and touches no database, so it is unit-tested without a server.
//!
//! Every value goes through `binding::bind_value`, with the column's real type
//! (from `information_schema`), so an `INT8` key that arrived as a string is
//! bound as an integer, a `DECIMAL` keeps its digits, an enum is cast, and so
//! on. Identifiers are quoted; nothing from the host is spliced into SQL as
//! text except through `bind_value`'s validated casts.
//!
//! Safety rules that live here:
//! - `update_record` and `delete_record` refuse an empty row key, so a request
//!   can never touch every row.
//! - A column named in the request must exist in the table (hidden columns such
//!   as `rowid` count), so a typo is reported instead of being guessed at.
//! - A `NULL` key value becomes `IS NULL`, never `= NULL`.

use std::collections::HashMap;

use serde_json::{Map, Value};
use tokio_postgres::types::{ToSql, Type};

use crate::binding::{bind_value, BindOptions, PgParam};
use crate::error::PluginError;
use crate::introspection::{qualified, quote_ident};

/// What the binder needs to know about a column.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ColumnInfo {
    /// Canonical type, e.g. `INT8`, `DECIMAL(10,2)`, `STRING[]`.
    pub sql_type: String,
    /// Quoted `"schema"."name"` when the column is an enum.
    pub enum_type: Option<String>,
}

pub type Columns = HashMap<String, ColumnInfo>;

/// A statement with its parameters pinned to wire types, ready for
/// `prepare_typed` and `execute`.
pub struct Statement {
    pub sql: String,
    pub types: Vec<Type>,
    pub params: Vec<PgParam>,
}

impl std::fmt::Debug for Statement {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Statement")
            .field("sql", &self.sql)
            .field("types", &self.types)
            .finish()
    }
}

impl Statement {
    pub fn param_refs(&self) -> Vec<&(dyn ToSql + Sync)> {
        self.params
            .iter()
            .map(|p| &**p as &(dyn ToSql + Sync))
            .collect()
    }
}

/// Hands out `$n` placeholders in order and collects the parameters. A value
/// that binds nothing (`NULL`, `DEFAULT`) does not consume a number, so the
/// numbering always matches the parameter list.
struct Binder<'a> {
    columns: &'a Columns,
    max_blob_size: Option<u64>,
    next: usize,
    types: Vec<Type>,
    params: Vec<PgParam>,
}

impl<'a> Binder<'a> {
    fn new(columns: &'a Columns, max_blob_size: Option<u64>) -> Self {
        Self {
            columns,
            max_blob_size,
            next: 1,
            types: Vec::new(),
            params: Vec::new(),
        }
    }

    /// SQL fragment for `value` going into `column`.
    fn bind(
        &mut self,
        column: &str,
        value: Value,
        allow_default: bool,
    ) -> Result<String, PluginError> {
        let info = self.columns.get(column);
        let options = BindOptions {
            column_type: info.map(|c| c.sql_type.as_str()),
            enum_type: info.and_then(|c| c.enum_type.as_deref()),
            allow_default,
            max_blob_size: self.max_blob_size,
        };
        let bound = bind_value(value, self.next, &options).map_err(|e| {
            PluginError::invalid_params(format!("column \"{column}\": {}", e.message))
        })?;
        if let Some((param, ty)) = bound.param {
            self.params.push(param);
            self.types.push(ty);
            self.next += 1;
        }
        Ok(bound.sql)
    }

    fn finish(self, sql: String) -> Statement {
        Statement {
            sql,
            types: self.types,
            params: self.params,
        }
    }
}

fn require_column(
    columns: &Columns,
    schema: &str,
    table: &str,
    column: &str,
) -> Result<(), PluginError> {
    if columns.contains_key(column) {
        Ok(())
    } else {
        Err(PluginError::invalid_params(format!(
            "column \"{column}\" does not exist in {schema}.{table}"
        )))
    }
}

/// `"a" = $1 AND "b" IS NULL`, keys in sorted order so the SQL is stable.
fn pk_predicate(
    binder: &mut Binder,
    pk_map: &Map<String, Value>,
    schema: &str,
    table: &str,
) -> Result<String, PluginError> {
    if pk_map.is_empty() {
        return Err(PluginError::invalid_params(
            "pk_map must not be empty: refusing to match every row",
        ));
    }
    let mut keys: Vec<&String> = pk_map.keys().collect();
    keys.sort();
    let mut parts = Vec::with_capacity(keys.len());
    for key in keys {
        require_column(binder.columns, schema, table, key)?;
        let value = &pk_map[key];
        if value.is_null() {
            parts.push(format!("{} IS NULL", quote_ident(key)));
        } else {
            let sql = binder.bind(key, value.clone(), false)?;
            parts.push(format!("{} = {sql}", quote_ident(key)));
        }
    }
    Ok(parts.join(" AND "))
}

/// `INSERT INTO s.t (cols) VALUES (...)`. A column the user left out is not in
/// `data`, so the server default (`unique_rowid()`, `gen_random_uuid()`,
/// `now()`) applies. No columns at all inserts `DEFAULT VALUES`.
pub fn build_insert(
    schema: &str,
    table: &str,
    data: &Map<String, Value>,
    columns: &Columns,
    max_blob_size: Option<u64>,
) -> Result<Statement, PluginError> {
    let target = qualified(schema, table);
    if data.is_empty() {
        return Ok(Binder::new(columns, max_blob_size)
            .finish(format!("INSERT INTO {target} DEFAULT VALUES")));
    }
    let mut binder = Binder::new(columns, max_blob_size);
    let mut names = Vec::with_capacity(data.len());
    let mut values = Vec::with_capacity(data.len());
    for (column, value) in data {
        require_column(columns, schema, table, column)?;
        names.push(quote_ident(column));
        values.push(binder.bind(column, value.clone(), true)?);
    }
    let sql = format!(
        "INSERT INTO {target} ({}) VALUES ({})",
        names.join(", "),
        values.join(", ")
    );
    Ok(binder.finish(sql))
}

/// `UPDATE s.t SET "col" = ... WHERE <row key>`.
pub fn build_update(
    schema: &str,
    table: &str,
    pk_map: &Map<String, Value>,
    col_name: &str,
    new_val: Value,
    columns: &Columns,
    max_blob_size: Option<u64>,
) -> Result<Statement, PluginError> {
    require_column(columns, schema, table, col_name)?;
    let mut binder = Binder::new(columns, max_blob_size);
    // The SET value is bound first, so it is `$1` whenever it binds anything.
    let set = binder.bind(col_name, new_val, true)?;
    let predicate = pk_predicate(&mut binder, pk_map, schema, table)?;
    let sql = format!(
        "UPDATE {} SET {} = {set} WHERE {predicate}",
        qualified(schema, table),
        quote_ident(col_name)
    );
    Ok(binder.finish(sql))
}

/// `DELETE FROM s.t WHERE <row key>`.
pub fn build_delete(
    schema: &str,
    table: &str,
    pk_map: &Map<String, Value>,
    columns: &Columns,
) -> Result<Statement, PluginError> {
    let mut binder = Binder::new(columns, None);
    let predicate = pk_predicate(&mut binder, pk_map, schema, table)?;
    let sql = format!("DELETE FROM {} WHERE {predicate}", qualified(schema, table));
    Ok(binder.finish(sql))
}

/// `SELECT "col" FROM s.t WHERE <row key>`: one cell, never the whole row.
pub fn build_blob_select(
    schema: &str,
    table: &str,
    col_name: &str,
    pk_map: &Map<String, Value>,
    columns: &Columns,
) -> Result<Statement, PluginError> {
    require_column(columns, schema, table, col_name)?;
    let mut binder = Binder::new(columns, None);
    let predicate = pk_predicate(&mut binder, pk_map, schema, table)?;
    let sql = format!(
        "SELECT {} FROM {} WHERE {predicate}",
        quote_ident(col_name),
        qualified(schema, table)
    );
    Ok(binder.finish(sql))
}

/// Sanity-check a path before a database round trip that a bad path would
/// waste: empty, a directory, or in a folder that does not exist. Not a
/// security boundary; the path comes from the host's save dialog.
pub fn validate_writable_path(file_path: &str) -> Result<(), PluginError> {
    if file_path.trim().is_empty() {
        return Err(PluginError::invalid_params("file_path must not be empty"));
    }
    let path = std::path::Path::new(file_path);
    if path.is_dir() {
        return Err(PluginError::invalid_params(format!(
            "file_path '{file_path}' is a directory, not a file"
        )));
    }
    match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() && !parent.is_dir() => {
            Err(PluginError::invalid_params(format!(
                "file_path '{file_path}': parent directory '{}' does not exist",
                parent.display()
            )))
        }
        _ => Ok(()),
    }
}

#[cfg(test)]
#[path = "crud_tests.rs"]
mod tests;
