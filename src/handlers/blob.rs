//! BLOB cells: `save_blob_to_file` and `fetch_blob_as_data_url`.
//!
//! Both read a single `BYTES` cell chosen by the row key, never the whole row.
//! The preview format is the host's `BLOB:<size>:<mime>:<base64>`.

use serde_json::Value;

use crate::crud::{self, Statement};
use crate::error::PluginError;
use crate::handlers::crud::{pk_map_param, prepare, Target};
use crate::rpc::result_response;
use crate::utils::blob::encode_blob_full;

/// The bytes of one cell, or an error saying why there are none.
async fn fetch_cell(params: &Value) -> Result<Vec<u8>, PluginError> {
    let target = Target::from_params(params)?;
    let col_name = params
        .get("col_name")
        .and_then(Value::as_str)
        .filter(|c| !c.is_empty())
        .ok_or_else(|| PluginError::invalid_params("missing parameter 'col_name'"))?;
    let pk_map = pk_map_param(params)?;

    let prep = prepare(&target).await?;
    let stmt: Statement = crud::build_blob_select(
        &prep.schema,
        &target.table,
        col_name,
        &pk_map,
        &prep.columns,
    )?;
    let prepared = prep.client.prepare_typed(&stmt.sql, &stmt.types).await?;
    let rows = prep.client.query(&prepared, &stmt.param_refs()).await?;

    let row = rows.first().ok_or_else(|| {
        PluginError::internal("row not found; it may have been changed or deleted")
    })?;
    if rows.len() > 1 {
        return Err(PluginError::internal(format!(
            "the row key matches {} rows, so it does not identify a single row",
            rows.len()
        )));
    }
    let column_type = row.columns()[0].type_().clone();
    row.try_get::<_, Option<Vec<u8>>>(0)
        .map_err(|_| {
            PluginError::invalid_params(format!(
                "column \"{col_name}\" is {}, not BYTES",
                crate::extract::canonical_type_name(&column_type)
            ))
        })?
        .ok_or_else(|| PluginError::internal(format!("column \"{col_name}\" is NULL for this row")))
}

pub async fn save_blob_to_file(id: Value, params: &Value) -> Value {
    result_response(id, save(params).await)
}

async fn save(params: &Value) -> Result<Value, PluginError> {
    let file_path = params
        .get("file_path")
        .and_then(Value::as_str)
        .unwrap_or("");
    // Fail on a bad path before spending a database round trip.
    crud::validate_writable_path(file_path)?;
    let bytes = fetch_cell(params).await?;
    std::fs::write(file_path, bytes)
        .map_err(|e| PluginError::internal(format!("cannot write '{file_path}': {e}")))?;
    Ok(Value::Null)
}

pub async fn fetch_blob_as_data_url(id: Value, params: &Value) -> Value {
    result_response(
        id,
        fetch_cell(params)
            .await
            .map(|bytes| Value::String(encode_blob_full(&bytes))),
    )
}
