//! Plugin settings declared in `.tabularium`, delivered by `initialize`.
//! Read once and applied to pools created afterwards.

use std::sync::RwLock;

use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Settings {
    pub pool_max_size: usize,
    pub retry_max_attempts: u32,
    pub application_name: String,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            pool_max_size: 10,
            retry_max_attempts: 3,
            application_name: "tabularis".to_string(),
        }
    }
}

impl Settings {
    /// Accepts either the `initialize` params (with a `settings` object) or
    /// the settings object itself. Missing or invalid values keep defaults.
    pub fn from_value(value: &Value) -> Self {
        let obj = value.get("settings").unwrap_or(value);
        let mut s = Self::default();
        if let Some(n) = obj.get("poolMaxSize").and_then(Value::as_u64) {
            if n >= 1 {
                s.pool_max_size = n.min(1000) as usize;
            }
        }
        if let Some(n) = obj.get("retryMaxAttempts").and_then(Value::as_u64) {
            s.retry_max_attempts = n.min(100) as u32;
        }
        if let Some(name) = obj.get("applicationName").and_then(Value::as_str) {
            if !name.trim().is_empty() {
                s.application_name = name.trim().to_string();
            }
        }
        s
    }
}

static SETTINGS: RwLock<Option<Settings>> = RwLock::new(None);

pub fn set(settings: Settings) {
    if let Ok(mut g) = SETTINGS.write() {
        *g = Some(settings);
    }
}

pub fn get() -> Settings {
    SETTINGS
        .read()
        .ok()
        .and_then(|g| g.clone())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn defaults_when_missing() {
        assert_eq!(Settings::from_value(&json!({})), Settings::default());
        assert_eq!(Settings::from_value(&Value::Null), Settings::default());
    }

    #[test]
    fn reads_nested_and_flat() {
        let v =
            json!({"settings": {"poolMaxSize": 4, "retryMaxAttempts": 0, "applicationName": "x"}});
        let s = Settings::from_value(&v);
        assert_eq!(
            (
                s.pool_max_size,
                s.retry_max_attempts,
                s.application_name.as_str()
            ),
            (4, 0, "x")
        );
        assert_eq!(Settings::from_value(&v["settings"]), s);
    }

    #[test]
    fn rejects_invalid_values() {
        let s = Settings::from_value(&json!({"poolMaxSize": 0, "applicationName": "  "}));
        assert_eq!(s, Settings::default());
    }
}
