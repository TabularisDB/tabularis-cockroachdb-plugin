//! Driver connection layer: connection resolution, pooling, session
//! bootstrap and version detection.
//!
//! The JSON-RPC loop is synchronous, so a single process-wide Tokio runtime
//! drives the async database calls through [`block_on`].

use std::collections::hash_map::DefaultHasher;
use std::collections::HashMap;
use std::future::Future;
use std::hash::{Hash, Hasher};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use deadpool_postgres::{Hook, HookError, Manager, ManagerConfig, Pool, RecyclingMethod, Runtime};
use tokio::runtime::Runtime as TokioRuntime;

use crate::error::{describe_pg_error, friendly_message, PluginError};
use crate::models::ConnectionParams;
use crate::settings::{self, Settings};
use crate::tls::{self, SslMode};
use crate::utils::uri::{parse_connection_uri, DEFAULT_DATABASE, DEFAULT_PORT};
use crate::version::{is_cockroachdb, parse_crdb_version, CrdbVersion};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

fn runtime() -> &'static TokioRuntime {
    static RT: OnceLock<TokioRuntime> = OnceLock::new();
    RT.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .expect("failed to start the Tokio runtime")
    })
}

pub fn block_on<F: Future>(fut: F) -> F::Output {
    runtime().block_on(fut)
}

/// Which workload a pool serves. They get distinct `application_name`
/// suffixes so sessions show up separately in `SHOW SESSIONS`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PoolKind {
    Query,
    Metadata,
}

/// Effective connection settings: decomposed host fields merged with the raw
/// connection string (URI values win for TLS paths, `options`, and
/// `application_name`; decomposed fields win for host/port/database/user).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Connection {
    pub host: String,
    pub port: u16,
    pub database: String,
    pub user: String,
    pub password: String,
    pub ssl_mode: SslMode,
    pub ssl_ca: Option<String>,
    pub ssl_cert: Option<String>,
    pub ssl_key: Option<String>,
    pub options: Option<String>,
    pub application_name: String,
}

fn non_empty(s: &Option<String>) -> Option<String> {
    s.as_ref()
        .map(|v| v.trim())
        .filter(|v| !v.is_empty())
        .map(str::to_string)
}

impl Connection {
    pub fn resolve(params: &ConnectionParams, settings: &Settings) -> Result<Self, PluginError> {
        let uri = match non_empty(&params.connection_uri) {
            Some(u) => Some(parse_connection_uri(&u)?),
            None => None,
        };
        let uri = uri.unwrap_or_default();

        let host = non_empty(&params.host)
            .or_else(|| uri.host.clone())
            .ok_or_else(|| PluginError::invalid_params("host is required"))?;
        let port = params
            .port
            .or(if uri.host.is_some() {
                Some(uri.port)
            } else {
                None
            })
            .unwrap_or(DEFAULT_PORT);
        let database = non_empty(&params.database)
            .or_else(|| non_empty(&Some(uri.database.clone())))
            .unwrap_or_else(|| DEFAULT_DATABASE.to_string());
        let user = non_empty(&params.username)
            .or_else(|| uri.user.clone())
            .unwrap_or_else(|| "root".to_string());
        let password = params
            .password
            .clone()
            .filter(|p| !p.is_empty())
            .or_else(|| uri.password.clone())
            .unwrap_or_default();

        let ssl_mode = SslMode::parse(uri.sslmode.as_deref().or(params.ssl_mode.as_deref()))?;

        let cluster = params
            .extra
            .get("cluster")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .map(|c| format!("--cluster={c}"));

        Ok(Self {
            host,
            port,
            database,
            user,
            password,
            ssl_mode,
            ssl_ca: uri
                .sslrootcert
                .clone()
                .or_else(|| non_empty(&params.ssl_ca)),
            ssl_cert: uri.sslcert.clone().or_else(|| non_empty(&params.ssl_cert)),
            ssl_key: uri.sslkey.clone().or_else(|| non_empty(&params.ssl_key)),
            options: uri.options.clone().or(cluster),
            application_name: uri
                .application_name
                .clone()
                .unwrap_or_else(|| settings.application_name.clone()),
        })
    }

    fn app_name(&self, kind: PoolKind) -> String {
        match kind {
            PoolKind::Query => self.application_name.clone(),
            PoolKind::Metadata => format!("{} metadata", self.application_name),
        }
    }

    /// Cache key: every field that changes which server, identity, TLS
    /// material, or cluster a connection reaches. The password is hashed so
    /// it is not kept in clear text as a map key.
    pub fn pool_key(&self, kind: PoolKind, pool_size: usize) -> String {
        let mut h = DefaultHasher::new();
        self.password.hash(&mut h);
        format!(
            "{}:{}|{}|{}|pw{:x}|{}|{:?}|{:?}|{:?}|{:?}|{}|{:?}|{}",
            self.host,
            self.port,
            self.database,
            self.user,
            h.finish(),
            self.ssl_mode.as_str(),
            self.ssl_ca,
            self.ssl_cert,
            self.ssl_key,
            self.options,
            self.application_name,
            kind,
            pool_size,
        )
    }

    pub fn pg_config(&self, kind: PoolKind) -> tokio_postgres::Config {
        let mut cfg = tokio_postgres::Config::new();
        cfg.host(&self.host)
            .port(self.port)
            .dbname(&self.database)
            .user(&self.user)
            .application_name(self.app_name(kind))
            .ssl_mode(self.ssl_mode.pg_ssl_mode())
            .connect_timeout(CONNECT_TIMEOUT)
            .keepalives(true)
            .keepalives_idle(Duration::from_secs(60))
            .keepalives_interval(Duration::from_secs(10))
            .keepalives_retries(3);
        if !self.password.is_empty() {
            cfg.password(&self.password);
        }
        if let Some(o) = &self.options {
            cfg.options(o);
        }
        cfg
    }

    fn connector(&self) -> Result<tokio_postgres_rustls::MakeRustlsConnect, PluginError> {
        tls::build_connector(
            self.ssl_mode,
            self.ssl_ca.as_deref(),
            self.ssl_cert.as_deref(),
            self.ssl_key.as_deref(),
        )
    }
}

/// What the bootstrap learned about the server.
#[derive(Debug, Clone)]
pub struct ServerInfo {
    pub version_string: String,
    pub version: CrdbVersion,
    pub current_schema: String,
}

/// Session bootstrap, run once per new physical connection (startup
/// parameters are already applied by the connection config).
pub async fn bootstrap(
    client: &tokio_postgres::Client,
    schema: Option<&str>,
) -> Result<ServerInfo, PluginError> {
    let row = client.query_one("SELECT version()", &[]).await?;
    let version_string: String = row.get(0);
    if !is_cockroachdb(&version_string) {
        let found = version_string
            .split(" on ")
            .next()
            .unwrap_or(&version_string)
            .to_string();
        return Err(PluginError::internal(format!(
            "The server is not CockroachDB (found: {found}). Use the PostgreSQL driver for this connection"
        )));
    }
    let version = parse_crdb_version(&version_string).ok_or_else(|| {
        PluginError::internal(format!(
            "Could not parse the CockroachDB version from: {version_string}"
        ))
    })?;

    if let Some(s) = schema.filter(|s| !s.is_empty()) {
        let sql = format!(
            "SET search_path TO {}",
            crate::utils::identifiers::quote_identifier(s, '"')
        );
        client.batch_execute(&sql).await?;
    }

    let current_schema = resolve_current_schema(client).await;
    Ok(ServerInfo {
        version_string,
        version,
        current_schema,
    })
}

async fn resolve_current_schema(client: &tokio_postgres::Client) -> String {
    for sql in [
        "SELECT current_schema()",
        "SELECT (current_schemas(false))[1]",
    ] {
        if let Ok(row) = client.query_one(sql, &[]).await {
            if let Ok(Some(s)) = row.try_get::<_, Option<String>>(0) {
                if !s.is_empty() {
                    return s;
                }
            }
        }
    }
    "public".to_string()
}

/// A pool plus the server info captured when its first connection came up.
pub struct PoolEntry {
    pub pool: Pool,
    info: Arc<Mutex<Option<ServerInfo>>>,
}

impl PoolEntry {
    pub fn server_info(&self) -> Option<ServerInfo> {
        self.info.lock().ok().and_then(|g| g.clone())
    }
}

fn pools() -> &'static Mutex<HashMap<String, Arc<PoolEntry>>> {
    static POOLS: OnceLock<Mutex<HashMap<String, Arc<PoolEntry>>>> = OnceLock::new();
    POOLS.get_or_init(Default::default)
}

fn build_pool(conn: &Connection, kind: PoolKind, size: usize) -> Result<PoolEntry, PluginError> {
    let manager = Manager::from_config(
        conn.pg_config(kind),
        conn.connector()?,
        ManagerConfig {
            recycling_method: RecyclingMethod::Fast,
        },
    );
    let info: Arc<Mutex<Option<ServerInfo>>> = Arc::new(Mutex::new(None));
    let hook_info = info.clone();
    let pool = Pool::builder(manager)
        .max_size(size)
        .runtime(Runtime::Tokio1)
        .post_create(Hook::async_fn(move |client, _| {
            let hook_info = hook_info.clone();
            Box::pin(async move {
                let server = bootstrap(client, None)
                    .await
                    .map_err(|e| HookError::message(e.message))?;
                if let Ok(mut g) = hook_info.lock() {
                    *g = Some(server);
                }
                Ok(())
            })
        }))
        .build()
        .map_err(|e| PluginError::internal(format!("failed to build pool: {e}")))?;
    Ok(PoolEntry { pool, info })
}

/// Get (or create) the pool for these connection params.
pub fn get_pool(params: &ConnectionParams, kind: PoolKind) -> Result<Arc<PoolEntry>, PluginError> {
    let settings = settings::get();
    let conn = Connection::resolve(params, &settings)?;
    let key = conn.pool_key(kind, settings.pool_max_size);
    let mut map = pools()
        .lock()
        .map_err(|_| PluginError::internal("pool registry poisoned"))?;
    if let Some(entry) = map.get(&key) {
        return Ok(entry.clone());
    }
    // Pool construction needs a runtime context for deadpool's timers.
    let _guard = runtime().enter();
    let entry = Arc::new(build_pool(&conn, kind, settings.pool_max_size)?);
    map.insert(key, entry.clone());
    Ok(entry)
}

pub fn pool_error(err: deadpool_postgres::PoolError) -> PluginError {
    use deadpool_postgres::PoolError as E;
    match err {
        E::Backend(e) => PluginError::internal(describe_pg_error(&e)),
        E::PostCreateHook(HookError::Message(m)) => PluginError::internal(friendly_message(&m)),
        E::PostCreateHook(HookError::Backend(e)) => PluginError::internal(describe_pg_error(&e)),
        E::Timeout(_) => PluginError::internal("Timed out waiting for a database connection"),
        other => PluginError::internal(friendly_message(&other.to_string())),
    }
}

/// `test_connection`: connect outside the pool, bootstrap, report the version.
pub async fn test_connection(
    params: &ConnectionParams,
    schema: Option<&str>,
) -> Result<ServerInfo, PluginError> {
    let conn = Connection::resolve(params, &settings::get())?;
    let tls = conn.connector()?;
    let (client, connection) = conn
        .pg_config(PoolKind::Query)
        .connect(tls)
        .await
        .map_err(PluginError::from)?;
    let driver = tokio::spawn(async move {
        let _ = connection.await;
    });
    let result = bootstrap(&client, schema).await;
    drop(client);
    let _ = driver.await;
    result
}

/// `ping`: `SELECT 1` on a pooled connection.
pub async fn ping(params: &ConnectionParams) -> Result<(), PluginError> {
    let entry = get_pool(params, PoolKind::Query)?;
    let client = entry.pool.get().await.map_err(pool_error)?;
    client.simple_query("SELECT 1").await?;
    Ok(())
}

#[cfg(test)]
#[path = "client_tests.rs"]
mod tests;
