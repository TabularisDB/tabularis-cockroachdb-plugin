//! Schema metadata: databases, schemas, tables, columns, indexes, FKs,
//! views, DDL. SQL and row shaping live in `crate::introspection`.
//!
//! Routines, schema snapshot and the other phase-7 methods are still stubs.
//!
//! Full reference: https://github.com/TabularisDB/tabularis/blob/main/plugins/PLUGIN_GUIDE.md#5-required-methods

use std::collections::{BTreeMap, HashMap};

use deadpool_postgres::Object;
use serde_json::{json, Value};

use crate::client::{self, PoolKind};
use crate::error::PluginError;
use crate::introspection::{self as intro, ColumnRow, FkRow, IndexRow, RelKind, TableFilter};
use crate::models::{inner_params, ConnectionParams};
use crate::rpc::{ok_response, result_response};

/// A checked-out metadata connection plus the resolved schema.
struct Ctx {
    client: Object,
    database: String,
    schema: String,
}

async fn ctx(params: &Value) -> Result<Ctx, PluginError> {
    let conn = ConnectionParams::from_value(inner_params(params));
    let entry = client::get_pool(&conn, PoolKind::Metadata)?;
    let client = entry.pool.get().await.map_err(client::pool_error)?;
    let schema = match params
        .get("schema")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
    {
        Some(s) => s.to_string(),
        None => entry
            .server_info()
            .map(|i| i.current_schema)
            .unwrap_or_else(|| "public".to_string()),
    };
    Ok(Ctx {
        client,
        database: entry.database.clone(),
        schema,
    })
}

fn str_param<'a>(params: &'a Value, keys: &[&str]) -> Result<&'a str, PluginError> {
    keys.iter()
        .find_map(|k| params.get(*k).and_then(Value::as_str))
        .filter(|s| !s.is_empty())
        .ok_or_else(|| PluginError::invalid_params(format!("missing parameter '{}'", keys[0])))
}

fn tables_param(params: &Value) -> Option<Vec<String>> {
    params.get("tables").and_then(Value::as_array).map(|a| {
        a.iter()
            .filter_map(Value::as_str)
            .map(str::to_string)
            .collect()
    })
}

/// Names of tables or views in the schema, with the row estimate and
/// locality from `SHOW TABLES`. Falls back to `information_schema` when
/// `SHOW TABLES` is unavailable to the role.
async fn list_relations(
    c: &Ctx,
    kind: RelKind,
) -> Result<Vec<(String, Option<i64>, Option<String>)>, PluginError> {
    let sql = intro::show_tables_sql(&c.database, &c.schema);
    match c.client.query(&sql, &[&kind.show_type()]).await {
        Ok(rows) => Ok(rows
            .iter()
            .map(|r| (r.get(0), r.get(2), r.get(3)))
            .collect()),
        Err(_) => {
            let rows = c
                .client
                .query(
                    intro::INFO_SCHEMA_TABLES_SQL,
                    &[&c.schema, &kind.info_schema_type()],
                )
                .await?;
            Ok(rows.iter().map(|r| (r.get(0), None, None)).collect())
        }
    }
}

async fn table_comments(c: &Ctx) -> HashMap<String, String> {
    match c
        .client
        .query(intro::TABLE_COMMENTS_SQL, &[&c.schema])
        .await
    {
        Ok(rows) => rows
            .iter()
            .filter_map(|r| {
                let d: Option<String> = r.get(1);
                d.filter(|d| !d.is_empty()).map(|d| (r.get(0), d))
            })
            .collect(),
        Err(_) => HashMap::new(),
    }
}

pub fn get_databases(id: Value, params: &Value) -> Value {
    result_response(
        id,
        client::block_on(async {
            let c = ctx(params).await?;
            let rows = c.client.query(intro::DATABASES_SQL, &[]).await?;
            Ok(Value::Array(
                rows.iter().map(|r| json!(r.get::<_, String>(0))).collect(),
            ))
        }),
    )
}

pub fn get_schemas(id: Value, params: &Value) -> Value {
    result_response(
        id,
        client::block_on(async {
            let c = ctx(params).await?;
            let rows = c.client.query(intro::SCHEMAS_SQL, &[]).await?;
            Ok(Value::Array(
                rows.iter().map(|r| json!(r.get::<_, String>(0))).collect(),
            ))
        }),
    )
}

pub fn get_tables(id: Value, params: &Value) -> Value {
    result_response(
        id,
        client::block_on(async {
            let c = ctx(params).await?;
            let comments = table_comments(&c).await;
            let tables = list_relations(&c, RelKind::Table).await?;
            Ok(Value::Array(
                tables
                    .into_iter()
                    .map(|(name, estimate, locality)| {
                        json!({
                            "comment": comments.get(&name),
                            "name": name,
                            "schema": c.schema,
                            "estimated_row_count": estimate,
                            "locality": locality,
                        })
                    })
                    .collect(),
            ))
        }),
    )
}

async fn fetch_columns(
    c: &Ctx,
    filter: TableFilter,
    tables: &[String],
    table: &str,
) -> Result<Vec<ColumnRow>, PluginError> {
    let sql = intro::columns_sql(filter);
    let rows = match filter {
        TableFilter::One => c.client.query(&sql, &[&c.schema, &table]).await?,
        TableFilter::Any => c.client.query(&sql, &[&c.schema, &tables]).await?,
        TableFilter::All => c.client.query(&sql, &[&c.schema]).await?,
    };
    Ok(rows
        .iter()
        .map(|r| ColumnRow {
            table: r.get(0),
            name: r.get(1),
            sql_type: r.get(2),
            data_type: r.get(3),
            udt_name: r.get(4),
            nullable: r.get(5),
            default: r.get(6),
            is_identity: r.get(7),
            is_generated: r.get(8),
            generation_expression: r.get(9),
            char_len: r.get(10),
            num_precision: r.get(11),
            num_scale: r.get(12),
            comment: r.get(13),
            is_pk: r.get(14),
        })
        .collect())
}

async fn columns_of(params: &Value, keys: &[&str]) -> Result<Value, PluginError> {
    let table = str_param(params, keys)?;
    let c = ctx(params).await?;
    let rows = fetch_columns(&c, TableFilter::One, &[], table).await?;
    Ok(Value::Array(rows.iter().map(intro::column_json).collect()))
}

pub fn get_columns(id: Value, params: &Value) -> Value {
    result_response(id, client::block_on(columns_of(params, &["table"])))
}

pub fn get_view_columns(id: Value, params: &Value) -> Value {
    result_response(id, client::block_on(columns_of(params, &["view", "table"])))
}

pub fn get_all_columns_batch(id: Value, params: &Value) -> Value {
    result_response(
        id,
        client::block_on(async {
            let c = ctx(params).await?;
            let tables = tables_param(params);
            let rows = match &tables {
                Some(t) => fetch_columns(&c, TableFilter::Any, t, "").await?,
                None => fetch_columns(&c, TableFilter::All, &[], "").await?,
            };
            let mut out: BTreeMap<String, Vec<Value>> = BTreeMap::new();
            for t in tables.iter().flatten() {
                out.entry(t.clone()).or_default();
            }
            for r in &rows {
                out.entry(r.table.clone())
                    .or_default()
                    .push(intro::column_json(r));
            }
            Ok(json!(out))
        }),
    )
}

async fn fetch_fks(
    c: &Ctx,
    filter: TableFilter,
    tables: &[String],
    table: &str,
) -> Result<Vec<FkRow>, PluginError> {
    let sql = intro::foreign_keys_sql(filter);
    let rows = match filter {
        TableFilter::One => c.client.query(&sql, &[&c.schema, &table]).await?,
        TableFilter::Any => c.client.query(&sql, &[&c.schema, &tables]).await?,
        TableFilter::All => c.client.query(&sql, &[&c.schema]).await?,
    };
    Ok(rows
        .iter()
        .map(|r| FkRow {
            table: r.get(0),
            name: r.get(1),
            ref_schema: r.get(2),
            ref_table: r.get(3),
            on_update: r.get(4),
            on_delete: r.get(5),
            position: r.get(6),
            column: r.get(7),
            ref_column: r.get(8),
        })
        .collect())
}

pub fn get_foreign_keys(id: Value, params: &Value) -> Value {
    result_response(
        id,
        client::block_on(async {
            let table = str_param(params, &["table"])?;
            let c = ctx(params).await?;
            let rows = fetch_fks(&c, TableFilter::One, &[], table).await?;
            Ok(Value::Array(rows.iter().map(intro::fk_json).collect()))
        }),
    )
}

pub fn get_all_foreign_keys_batch(id: Value, params: &Value) -> Value {
    result_response(
        id,
        client::block_on(async {
            let c = ctx(params).await?;
            let tables = tables_param(params);
            let rows = match &tables {
                Some(t) => fetch_fks(&c, TableFilter::Any, t, "").await?,
                None => fetch_fks(&c, TableFilter::All, &[], "").await?,
            };
            let mut out: BTreeMap<String, Vec<Value>> = BTreeMap::new();
            for t in tables.iter().flatten() {
                out.entry(t.clone()).or_default();
            }
            for r in &rows {
                out.entry(r.table.clone())
                    .or_default()
                    .push(intro::fk_json(r));
            }
            Ok(json!(out))
        }),
    )
}

pub fn get_indexes(id: Value, params: &Value) -> Value {
    result_response(
        id,
        client::block_on(async {
            let table = str_param(params, &["table"])?;
            let c = ctx(params).await?;
            let pk: Option<String> = c
                .client
                .query_opt(intro::PK_NAME_SQL, &[&c.schema, &table])
                .await?
                .map(|r| r.get(0));
            let sql = format!(
                "SELECT index_name::STRING, non_unique, seq_in_index::INT8, column_name::STRING, \
                        storing, implicit, visible \
                 FROM [SHOW INDEXES FROM {}]",
                intro::qualified(&c.schema, table)
            );
            let rows = c.client.query(&sql, &[]).await?;
            let index_rows = rows
                .iter()
                .map(|r| IndexRow {
                    name: r.get(0),
                    non_unique: r.get(1),
                    seq: r.get(2),
                    column: r.get(3),
                    storing: r.get(4),
                    implicit: r.get(5),
                    visible: r.get(6),
                })
                .collect();
            Ok(Value::Array(intro::group_indexes(
                index_rows,
                pk.as_deref(),
            )))
        }),
    )
}

pub fn get_views(id: Value, params: &Value) -> Value {
    result_response(
        id,
        client::block_on(async {
            let c = ctx(params).await?;
            let views = list_relations(&c, RelKind::View).await?;
            Ok(Value::Array(
                views
                    .into_iter()
                    .map(|(name, _, _)| json!({ "name": name, "schema": c.schema }))
                    .collect(),
            ))
        }),
    )
}

pub fn get_view_definition(id: Value, params: &Value) -> Value {
    result_response(
        id,
        client::block_on(async {
            let view = str_param(params, &["view", "table"])?;
            let c = ctx(params).await?;
            let body: Option<String> = c
                .client
                .query_opt(intro::VIEW_DEFINITION_SQL, &[&c.schema, &view])
                .await?
                .and_then(|r| r.get(0));
            match body.filter(|b| !b.is_empty()) {
                Some(b) => Ok(Value::String(b)),
                None => show_create(&c, "VIEW", view).await,
            }
        }),
    )
}

async fn show_create(c: &Ctx, kind: &str, name: &str) -> Result<Value, PluginError> {
    let sql = format!(
        "SELECT create_statement::STRING FROM [SHOW CREATE {kind} {}]",
        intro::qualified(&c.schema, name)
    );
    let row = c.client.query_one(&sql, &[]).await?;
    Ok(Value::String(row.get(0)))
}

pub fn get_table_ddl(id: Value, params: &Value) -> Value {
    result_response(
        id,
        client::block_on(async {
            let table = str_param(params, &["table"])?;
            let c = ctx(params).await?;
            show_create(&c, "TABLE", table).await
        }),
    )
}

pub fn get_routines(id: Value, _params: &Value) -> Value {
    ok_response(id, json!([]))
}

pub fn get_routine_parameters(id: Value, _params: &Value) -> Value {
    ok_response(id, json!([]))
}

pub fn get_routine_definition(id: Value, _params: &Value) -> Value {
    ok_response(id, Value::String(String::new()))
}

pub fn get_schema_snapshot(id: Value, _params: &Value) -> Value {
    // Used for the ER diagram. Return
    // [{ name, columns: [...], foreign_keys: [...] }].
    ok_response(id, json!([]))
}
