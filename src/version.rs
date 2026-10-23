//! CockroachDB release parsing and version gates.
//!
//! `server_version` / `server_version_num` always report PostgreSQL 13.0.0, so
//! the only reliable source is the text returned by `SELECT version()`.

use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CrdbVersion {
    pub major: u32,
    pub minor: u32,
    pub patch: u32,
}

impl CrdbVersion {
    pub const fn new(major: u32, minor: u32, patch: u32) -> Self {
        Self {
            major,
            minor,
            patch,
        }
    }

    pub const fn at_least(&self, major: u32, minor: u32) -> bool {
        self.major > major || (self.major == major && self.minor >= minor)
    }

    /// User-defined functions and `aclexplode` / schema privileges.
    pub const fn supports_udf(&self) -> bool {
        self.at_least(22, 2)
    }

    /// Stored procedures (`CALL`).
    pub const fn supports_procedures(&self) -> bool {
        self.at_least(23, 2)
    }

    /// `READ COMMITTED` isolation (also needs a cluster setting).
    pub const fn supports_read_committed(&self) -> bool {
        self.at_least(23, 2)
    }

    pub const fn supports_vector_type(&self) -> bool {
        self.at_least(24, 2)
    }

    pub const fn supports_triggers(&self) -> bool {
        self.at_least(24, 3)
    }

    pub const fn supports_vector_index(&self) -> bool {
        self.at_least(25, 2)
    }
}

impl fmt::Display for CrdbVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "v{}.{}.{}", self.major, self.minor, self.patch)
    }
}

/// True when a `version()` string comes from CockroachDB.
pub fn is_cockroachdb(version_text: &str) -> bool {
    version_text.trim_start().starts_with("CockroachDB")
}

/// Parse the release out of `SELECT version()`, e.g.
/// `CockroachDB CCL v24.3.4 (x86_64-pc-linux-gnu, built ..., go1.22.8)`.
/// Returns `None` for other servers or unparsable text. Pre-release suffixes
/// (`v25.1.0-beta.1`) are ignored.
pub fn parse_crdb_version(version_text: &str) -> Option<CrdbVersion> {
    if !is_cockroachdb(version_text) {
        return None;
    }
    version_text.split_whitespace().find_map(parse_token)
}

fn parse_token(token: &str) -> Option<CrdbVersion> {
    let rest = token.strip_prefix('v')?;
    let mut parts = rest.splitn(3, '.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next()?.parse().ok()?;
    let patch_raw = parts.next()?;
    let digits: String = patch_raw.chars().take_while(char::is_ascii_digit).collect();
    let patch = digits.parse().ok()?;
    Some(CrdbVersion::new(major, minor, patch))
}

#[cfg(test)]
#[path = "version_tests.rs"]
mod tests;
