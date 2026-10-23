# 08 · Capabilities

The manifest (`.tabularium`) declares what the finished driver supports. This
table lists, for each flag, the RPC methods that must work before the flag is
honest. During the alpha some of them are still stubs.

## Capability flags

| Flag | Value | Requires | Phase |
| --- | --- | --- | --- |
| `schemas` | `true` | `get_schemas`, schema-aware metadata calls | 2 |
| `views` | `true` | `get_views`, `get_view_definition`, `get_view_columns`, `create_view`, `alter_view`, `drop_view` | 2 / 5 |
| `materialized_views` | `true` | `get_materialized_views`, `get_materialized_view_columns`, `get_materialized_view_definition`, `refresh_materialized_view` | 7 |
| `routines` | `true` | `get_routines`, `get_routine_parameters`, `get_routine_definition` (v22.2+) | 7 |
| `routine_management` | `false` | `build_routine_call_sql`, `routine_create_template`, `drop_routine`. Flip when done. | 7 |
| `triggers` | `false` | Triggers exist from v24.3. Flip after `get_triggers`, `get_trigger_definition`, `create_trigger`, `drop_trigger` are done and version-gated. | later |
| `connection_string` | `true` | URI parsing, see [01](./01-connection.md) | 1 |
| `connection_uri_schemes` | `cockroachdb`, `cockroach` | | 1 |
| `identifier_quote` | `"` | | |
| `sql_dialect` | `postgres` | Lexing matches: `'` strings, `"` identifiers, `$$` quoting, nested comments | |
| `supports_ssl` | `true` | TLS modes, CA and client certificates | 1 |
| `explain` | `true` | `explain_query`, see [06](./06-explain.md) | 6 |
| `readonly` | `false` | `insert_record`, `update_record`, `delete_record` | 4 |
| `manage_tables` | `true` | `get_create_table_sql`, `get_add_column_sql`, drops | 5 |
| `alter_column` | `true` | `get_alter_column_sql` | 5 |
| `alter_primary_key` | `true` | `ALTER PRIMARY KEY`, see [04](./04-ddl.md) | 5 |
| `create_foreign_keys` | `true` | `get_create_foreign_key_sql`, `drop_foreign_key` | 5 |
| `column_comments` | `true` | Column comments read in `get_columns`, written with `COMMENT ON COLUMN` | 2 / 5 |
| `serial_type` | `SERIAL` | Normalized by the server to `INT8 DEFAULT unique_rowid()` | |
| `auto_increment_keyword` | empty | | |
| `inline_pk` | `false` | Primary key as a table constraint | |
| `table_query_templates` | `false` | Flip if CockroachDB-specific SELECT/UPDATE/DELETE templates are added (`AS OF SYSTEM TIME`, `UPSERT`) | later |
| `drop_table_sql` | `false` | Flip after `get_drop_table_sql` returns `DROP TABLE ... CASCADE` | later |

Not declared, so the host keeps its defaults: `file_based`, `folder_based`,
`in_memory`, `no_connection_required`, `single_database` (all false),
`connection_uri` (false: the decomposed fields are enough; the driver reads
`options` from `connection_uri` when it is present).

## Version-dependent capabilities

Static flags describe the newest supported release. Some features do not
exist on older ones (routines before v22.2, procedures before v23.2, `VECTOR`
before v24.2). Two ways to handle them:

1. Return a clear error from the RPC on an older server ("functions require
   CockroachDB v22.2 or later; connected to v22.1.x").
2. Opt in to `connection_metadata` and return per-connection `capabilities`
   and `data_types` from `get_connection_metadata`, based on the parsed
   version. The host then hides what the connected cluster does not support.
   This is the better option once the basics work; see the host's
   `CONNECTION_METADATA.md`.

## Plugin settings

| Key | Type | Default | Used by |
| --- | --- | --- | --- |
| `poolMaxSize` | number | `10` | Pool size per profile and database |
| `retryMaxAttempts` | number | `3` | Retries of implicit transactions on `40001`; `0` disables |
| `applicationName` | string | `tabularis` | `application_name` startup parameter |

Settings arrive in the `initialize` call. Read them once and apply them to
pools created afterwards.

## Connection form

- `default_port`: `26257`, `default_username`: `root`.
- `connection_fields.database.placeholder`: `defaultdb`.
- CockroachDB Cloud Basic needs the cluster routing option. Phase 1 supports
  it through `options` in a pasted connection string. Phase 7 can add a
  "Cluster" input through a UI extension (`connection-modal.extra_fields`
  slot), sent as `params.extra.cluster`.

## Host features this driver does not offer

- Database size and server uptime (no `pg_database_size`,
  `pg_postmaster_start_time`).
- Disabling foreign key checks for a session.
- Server-side query progress.

These are host features that have no CockroachDB equivalent. Return an
explicit "not supported" error if the host calls a related method, not an
empty result.

## Minimum host version

`min_runtime_version` is `0.29.0`, the first release with `column_comments`
and `connection_fields` in the manifest. Raise it when the driver starts
depending on a newer host feature (for example a raw EXPLAIN parser bundle).
