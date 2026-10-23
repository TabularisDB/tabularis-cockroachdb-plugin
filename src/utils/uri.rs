//! Connection-string parsing for `postgresql://`, `postgres://`,
//! `cockroachdb://` and `cockroach://`.

use percent_encoding::percent_decode_str;

use crate::error::PluginError;

pub const DEFAULT_PORT: u16 = 26257;
pub const DEFAULT_DATABASE: &str = "defaultdb";

const SCHEMES: [&str; 4] = ["postgresql", "postgres", "cockroachdb", "cockroach"];

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UriParts {
    pub user: Option<String>,
    pub password: Option<String>,
    pub host: Option<String>,
    pub port: u16,
    pub database: String,
    pub sslmode: Option<String>,
    pub sslrootcert: Option<String>,
    pub sslcert: Option<String>,
    pub sslkey: Option<String>,
    pub options: Option<String>,
    pub application_name: Option<String>,
}

fn decode(s: &str) -> String {
    percent_decode_str(s).decode_utf8_lossy().into_owned()
}

fn non_empty(s: String) -> Option<String> {
    (!s.is_empty()).then_some(s)
}

pub fn parse_connection_uri(uri: &str) -> Result<UriParts, PluginError> {
    let uri = uri.trim();
    let (scheme, rest) = uri
        .split_once("://")
        .ok_or_else(|| PluginError::invalid_params("connection string must contain '://'"))?;
    if !SCHEMES.contains(&scheme.to_ascii_lowercase().as_str()) {
        return Err(PluginError::invalid_params(format!(
            "unsupported connection string scheme '{scheme}'"
        )));
    }

    let rest = rest.split_once('#').map_or(rest, |(r, _)| r);
    let (authority_path, query) = rest.split_once('?').unwrap_or((rest, ""));
    let (authority, path) = match authority_path.find('/') {
        Some(i) => (&authority_path[..i], &authority_path[i + 1..]),
        None => (authority_path, ""),
    };

    let mut parts = UriParts {
        port: DEFAULT_PORT,
        ..Default::default()
    };

    // userinfo is everything before the last '@'.
    let hostport = match authority.rsplit_once('@') {
        Some((userinfo, hp)) => {
            match userinfo.split_once(':') {
                Some((u, p)) => {
                    parts.user = non_empty(decode(u));
                    parts.password = Some(decode(p));
                }
                None => parts.user = non_empty(decode(userinfo)),
            }
            hp
        }
        None => authority,
    };

    // Only the first host of a multi-host list is used.
    let hostport = hostport.split(',').next().unwrap_or("");
    let (host, port) = split_host_port(hostport)?;
    parts.host = non_empty(host);
    if let Some(p) = port {
        parts.port = p;
    }

    let db = decode(path.split('/').next().unwrap_or(""));
    parts.database = if db.is_empty() {
        DEFAULT_DATABASE.to_string()
    } else {
        db
    };

    for pair in query.split('&').filter(|p| !p.is_empty()) {
        let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
        let v = decode(v);
        match decode(k).as_str() {
            "sslmode" => parts.sslmode = non_empty(v),
            "sslrootcert" => parts.sslrootcert = non_empty(v),
            "sslcert" => parts.sslcert = non_empty(v),
            "sslkey" => parts.sslkey = non_empty(v),
            "options" => parts.options = non_empty(v),
            "application_name" => parts.application_name = non_empty(v),
            "user" if parts.user.is_none() => parts.user = non_empty(v),
            "password" if parts.password.is_none() => parts.password = Some(v),
            _ => {}
        }
    }

    Ok(parts)
}

fn split_host_port(hostport: &str) -> Result<(String, Option<u16>), PluginError> {
    let bad_port = |p: &str| PluginError::invalid_params(format!("invalid port '{p}'"));
    if let Some(stripped) = hostport.strip_prefix('[') {
        // IPv6 literal: [::1]:26257
        let (host, after) = stripped
            .split_once(']')
            .ok_or_else(|| PluginError::invalid_params("unterminated IPv6 host"))?;
        let port = match after.strip_prefix(':') {
            Some(p) if !p.is_empty() => Some(p.parse().map_err(|_| bad_port(p))?),
            _ => None,
        };
        return Ok((host.to_string(), port));
    }
    match hostport.rsplit_once(':') {
        Some((h, p)) if !p.is_empty() => Ok((decode(h), Some(p.parse().map_err(|_| bad_port(p))?))),
        Some((h, _)) => Ok((decode(h), None)),
        None => Ok((decode(hostport), None)),
    }
}

#[cfg(test)]
#[path = "uri_tests.rs"]
mod tests;
