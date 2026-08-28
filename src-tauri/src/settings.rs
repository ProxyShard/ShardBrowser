use crate::store;
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::fs;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Settings {
    /// Optional absolute parent directory for profile JSON and Chromium user-data.
    /// Empty or missing keeps the platform-default profile location.
    #[serde(default)]
    pub profile_storage_path: Option<String>,
    /// Absolute path to the ShardX executable.
    pub browser_path: Option<String>,
    /// Theme: "dark" (default) or "light".
    #[serde(default = "default_theme")]
    pub theme: String,
    /// Geo-IP checker provider used by the proxy "Test" button.
    /// One of "ip-api.com" | "ipapi.co" | "ipwho.is".
    #[serde(default)]
    pub geo_checker: Option<String>,
    /// "fingerprint" (use the screen from the bound fingerprint) or
    /// "real" (let ShardX use the host's real screen).
    #[serde(default)]
    pub screen_resolution_mode: Option<String>,

    // ---- Local automation HTTP API (axum + JWT bearer) ----
    /// Whether the local API server listens on 127.0.0.1:`api_port`.
    #[serde(default = "default_api_enabled")]
    pub api_enabled: bool,
    /// Port the API binds on 127.0.0.1.
    #[serde(default = "default_api_port")]
    pub api_port: u16,
    /// HS256 signing key for API JWTs.  Auto-generated on first run
    /// (see `ensure_secret`); rotating it invalidates issued tokens.
    #[serde(default)]
    pub api_secret: String,
}

fn default_theme() -> String {
    "dark".into()
}

fn default_api_enabled() -> bool {
    true
}

fn default_api_port() -> u16 {
    41325
}

pub fn load() -> Result<Settings> {
    let path = store::settings_path()?;
    if !path.exists() {
        return Ok(Settings {
            profile_storage_path: None,
            browser_path: None,
            theme: default_theme(),
            geo_checker: Some("ip-api.com".into()),
            screen_resolution_mode: Some("fingerprint".into()),
            api_enabled: default_api_enabled(),
            api_port: default_api_port(),
            api_secret: String::new(),
        });
    }
    let body = fs::read_to_string(&path)?;
    Ok(serde_json::from_str(&body).unwrap_or_default())
}

/// Load settings, generating + persisting the API JWT secret if it's
/// still empty.  Call once at startup before the server reads it.
pub fn ensure_secret() -> Result<Settings> {
    let mut s = load()?;
    store::configure_profile_root(s.profile_storage_path.as_deref())?;
    if s.api_secret.is_empty() {
        s.api_secret = format!(
            "{}{}",
            uuid::Uuid::new_v4().simple(),
            uuid::Uuid::new_v4().simple()
        );
        save(&s)?;
    }
    Ok(s)
}

pub fn save(s: &Settings) -> Result<()> {
    store::normalize_profile_root(s.profile_storage_path.as_deref())?;
    let body = serde_json::to_string_pretty(s)?;
    fs::write(store::settings_path()?, body)?;
    Ok(())
}
