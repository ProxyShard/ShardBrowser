//! Profile = a fingerprint JSON + a per-launch working dir. Wraps the
//! bundled fingerprint library and lets callers override fields before
//! launch.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{anyhow, Context, Result};
use serde_json::{Map, Value};

use crate::runtime::Runtime;

#[derive(Clone, Debug)]
pub struct Profile {
    pub id: String,
    pub config: Value,
}

impl Profile {
    /// Wrap a config object. `id` defaults to `config.name` or `"anonymous"`.
    pub fn new(config: Value, id: Option<String>) -> Self {
        let id = id
            .or_else(|| {
                config
                    .get("name")
                    .and_then(|v| v.as_str())
                    .map(String::from)
            })
            .unwrap_or_else(|| "anonymous".to_string());
        Self { config, id }
    }

    pub fn from_file(path: &Path) -> Result<Self> {
        let text = fs::read_to_string(path).with_context(|| format!("read {path:?}"))?;
        let cfg: Value = serde_json::from_str(&text).with_context(|| format!("parse {path:?}"))?;
        let id = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("anonymous")
            .to_string();
        Ok(Self::new(cfg, Some(id)))
    }

    /// Shallow merge: object values merge one level deep, scalars replace.
    pub fn with_override(&self, overrides: Value) -> Profile {
        let mut out = self.config.clone();
        if let (Some(out_obj), Some(ov)) = (out.as_object_mut(), overrides.as_object()) {
            for (k, v) in ov {
                match (out_obj.get_mut(k), v) {
                    (Some(Value::Object(dst)), Value::Object(src)) => {
                        for (sk, sv) in src {
                            dst.insert(sk.clone(), sv.clone());
                        }
                    }
                    _ => {
                        out_obj.insert(k.clone(), v.clone());
                    }
                }
            }
        }
        let id = overrides
            .get("name")
            .and_then(|v| v.as_str())
            .map(String::from)
            .unwrap_or_else(|| self.id.clone());
        Profile::new(out, Some(id))
    }

    pub fn platform(&self) -> String {
        self.config
            .get("navigator")
            .and_then(|n| n.get("platform"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string()
    }

    pub fn has_webgpu(&self) -> bool {
        self.config
            .get("webgpu")
            .and_then(|w| w.get("limits"))
            .and_then(|l| l.as_object())
            .map(|m: &Map<String, Value>| !m.is_empty())
            .unwrap_or(false)
    }
}

/// The bundled fingerprint library (JSON files under the cache dir).
pub struct FingerprintLibrary {
    runtime: Arc<Runtime>,
}

impl FingerprintLibrary {
    pub fn new(runtime: Arc<Runtime>) -> Self {
        Self { runtime }
    }

    pub fn ids(&self) -> Vec<String> {
        let mut ids: Vec<String> = fs::read_dir(self.runtime.fingerprints_dir())
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|e| {
                let p = e.path();
                if p.extension().and_then(|s| s.to_str()) == Some("json") {
                    p.file_stem().and_then(|s| s.to_str()).map(String::from)
                } else {
                    None
                }
            })
            .collect();
        ids.sort();
        ids
    }

    /// Ids whose `navigator.platform` contains `platform` (case-insensitive).
    pub fn filter(&self, platform: Option<&str>) -> Vec<String> {
        let needle = platform.map(|p| p.to_lowercase());
        self.ids()
            .into_iter()
            .filter(|id| match &needle {
                None => true,
                Some(n) => self
                    .load(id)
                    .map(|p| p.platform().to_lowercase().contains(n))
                    .unwrap_or(false),
            })
            .collect()
    }

    pub fn load(&self, fingerprint_id: &str) -> Result<Profile> {
        let path = self
            .runtime
            .fingerprints_dir()
            .join(format!("{fingerprint_id}.json"));
        if !path.exists() {
            let sample = self.ids().into_iter().take(10).collect::<Vec<_>>().join(", ");
            return Err(anyhow!(
                "Fingerprint '{fingerprint_id}' not found. Available: {sample}…"
            ));
        }
        Profile::from_file(&path)
    }
}

/// Normalise a profile config's spoofed Chrome version to `chromium_version`
/// (e.g. "149.0.7827.103") so it always matches the running engine — bumps
/// `navigator.user_agent` (Chrome/<major>.0.0.0) and the version fields in
/// `client_hints`: brand_version / brand_full_version / chrome_build /
/// chrome_patch (derived from the version) plus, when supplied, grease_brand /
/// grease_version / grease_full_version (GREASE rotates per release, so it can't
/// be derived — it comes from the manifest). Leaves platform_version,
/// architecture, etc. intact. SDK equivalent of the launcher's post-update
/// profile migration.
pub fn apply_engine_version(
    config: &mut Value,
    chromium_version: &str,
    grease_brand: Option<&str>,
    grease_version: Option<&str>,
) {
    let parts: Vec<&str> = chromium_version.split('.').collect();
    if parts.len() != 4 {
        return;
    }
    let major = parts[0];
    let build = parts[2].parse::<i64>().ok();
    let patch = parts[3].parse::<i64>().ok();

    if let Some(ua) = config
        .pointer("/navigator/user_agent")
        .and_then(|v| v.as_str())
        .map(String::from)
    {
        if let Some(idx) = ua.find("Chrome/") {
            let rest = &ua[idx + 7..];
            let end = rest.find(' ').unwrap_or(rest.len());
            let new_ua = format!("{}Chrome/{}.0.0.0{}", &ua[..idx], major, &rest[end..]);
            if let Some(slot) = config.pointer_mut("/navigator/user_agent") {
                *slot = Value::String(new_ua);
            }
        }
    }
    if let Some(ch) = config.get_mut("client_hints").and_then(|v| v.as_object_mut()) {
        ch.insert("brand_version".into(), serde_json::json!(major));
        ch.insert("brand_full_version".into(), serde_json::json!(chromium_version));
        if let Some(b) = build {
            ch.insert("chrome_build".into(), serde_json::json!(b));
        }
        if let Some(p) = patch {
            ch.insert("chrome_patch".into(), serde_json::json!(p));
        }
        if let Some(gb) = grease_brand {
            ch.insert("grease_brand".into(), serde_json::json!(gb));
        }
        if let Some(gv) = grease_version {
            ch.insert("grease_version".into(), serde_json::json!(gv));
            ch.insert("grease_full_version".into(), serde_json::json!(format!("{gv}.0.0.0")));
        }
    }
}

/// Per-profile state dir (cookies / IndexedDB / cache), preserved across
/// launches. Defaults to `<profiles_root>/<id>/`.
pub fn user_data_dir(runtime: &Runtime, profile_id: &str, base: Option<&Path>) -> Result<PathBuf> {
    let root = base.map(PathBuf::from).unwrap_or_else(|| runtime.profiles_root());
    let d = root.join(profile_id);
    fs::create_dir_all(&d)?;
    Ok(d)
}
