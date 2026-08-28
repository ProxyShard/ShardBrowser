// Persistent storage layout for the isolated test build:
//   $CONFIG/shardx-launcher-test/
//     profiles/                   <- fingerprint profile JSON files (default)
//     proxies.json                <- saved proxy list
//     user-data/<profile-id>/     <- per-profile Chromium data (default)
//     settings.json               <- test-build settings
//
// A custom profile storage root replaces only the default profiles/ and
// user-data/ parent. All other state remains isolated under this test root.

use anyhow::{bail, Context, Result};
use std::{
    path::PathBuf,
    sync::{OnceLock, RwLock},
};

const APP_STORAGE_DIR: &str = "shardx-launcher-test";

fn profile_root_cell() -> &'static RwLock<Option<PathBuf>> {
    static ROOT: OnceLock<RwLock<Option<PathBuf>>> = OnceLock::new();
    ROOT.get_or_init(|| RwLock::new(None))
}

pub fn config_root() -> Result<PathBuf> {
    let base = dirs::config_dir().context("OS config dir unavailable")?;
    let root = base.join(APP_STORAGE_DIR);
    std::fs::create_dir_all(&root)?;
    Ok(root)
}

pub fn normalize_profile_root(value: Option<&str>) -> Result<Option<PathBuf>> {
    let Some(value) = value.map(str::trim).filter(|value| !value.is_empty()) else {
        return Ok(None);
    };
    let root = PathBuf::from(value);
    if !root.is_absolute() {
        bail!("profile storage directory must be an absolute path");
    }
    Ok(Some(root))
}

pub fn prepare_profile_root(value: Option<&str>) -> Result<Option<PathBuf>> {
    let root = normalize_profile_root(value)?;
    if let Some(root) = &root {
        std::fs::create_dir_all(root.join("profiles"))?;
        std::fs::create_dir_all(root.join("user-data"))?;
    }
    Ok(root)
}

pub fn configure_profile_root(value: Option<&str>) -> Result<()> {
    let root = prepare_profile_root(value)?;
    *profile_root_cell()
        .write()
        .map_err(|_| anyhow::anyhow!("profile storage lock poisoned"))? = root;
    Ok(())
}

fn profile_storage_root() -> Result<PathBuf> {
    let custom = profile_root_cell()
        .read()
        .map_err(|_| anyhow::anyhow!("profile storage lock poisoned"))?
        .clone();
    custom.map_or_else(config_root, Ok)
}

pub fn same_profile_root(left: Option<&str>, right: Option<&str>) -> Result<bool> {
    Ok(normalize_profile_root(left)? == normalize_profile_root(right)?)
}

pub fn profiles_dir() -> Result<PathBuf> {
    let p = profile_storage_root()?.join("profiles");
    std::fs::create_dir_all(&p)?;
    Ok(p)
}

pub fn fingerprints_dir() -> Result<PathBuf> {
    let p = config_root()?.join("fingerprints");
    std::fs::create_dir_all(&p)?;
    Ok(p)
}

/// Cached Widevine CDM, seeded from a host Chrome install (or
/// downloaded from the project's git LFS bucket for end users).  When
/// present, every freshly-created profile's user-data-dir gets a
/// pre-warmed `WidevineCdm/` copy so the browser doesn't sit waiting
/// on the component updater the first time a DRM page (Netflix /
/// Spotify / etc.) loads.
pub fn widevine_cache_dir() -> Result<PathBuf> {
    Ok(config_root()?.join("widevine-cdm"))
}

pub fn user_data_root() -> Result<PathBuf> {
    let p = profile_storage_root()?.join("user-data");
    std::fs::create_dir_all(&p)?;
    Ok(p)
}

pub fn proxies_path() -> Result<PathBuf> {
    Ok(config_root()?.join("proxies.json"))
}

pub fn settings_path() -> Result<PathBuf> {
    Ok(config_root()?.join("settings.json"))
}
