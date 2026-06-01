use crate::store;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

/// Launcher-side view of a profile (wraps raw FingerprintConfig JSON).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProfileMeta {
    pub id: String,
    pub name: String,
    pub notes: String,
    pub proxy_id: Option<String>,
    pub last_launched_at: Option<String>,
    pub created_at: Option<String>,
    pub pinned: bool,
    pub folder: String,
    /// Accumulated runtime across every launch; UI shows this plus the
    /// current-session uptime when the profile is running.
    #[serde(default)]
    pub total_runtime_ms: u64,
}

/// On-disk `<profiles_dir>/<id>.json`: FingerprintConfig + `_meta` envelope.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct StoredProfile {
    #[serde(rename = "_meta", default)]
    pub meta: StoredMeta,
    /// Verbatim FingerprintConfig payload (round-trip, not parsed).
    #[serde(flatten)]
    pub config: serde_json::Map<String, serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct StoredMeta {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub proxy_id: Option<String>,
    #[serde(default)]
    pub last_launched_at: Option<String>,
    /// "@<unix_secs>" creation marker.
    #[serde(default)]
    pub created_at: Option<String>,
    #[serde(default)]
    pub pinned: bool,
    /// Empty = unfiled (All tab).
    #[serde(default)]
    pub folder: String,
    /// Cumulative engine uptime in milliseconds; bumped by the Tracker
    /// when the child exits.  Persists across launcher restarts.
    #[serde(default)]
    pub total_runtime_ms: u64,
    /// Source library fingerprint id; MUST round-trip — drives the editor GPU select.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gpu_preset_id: Option<String>,
    /// Inline proxy from temporary profile API; not in proxy store.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inline_proxy: Option<crate::proxy::ProxyEntry>,
    /// Hidden from listings; auto-deleted on close.
    #[serde(default, skip_serializing_if = "is_false")]
    pub temporary: bool,
}

fn is_false(b: &bool) -> bool {
    !*b
}

fn path_for(id: &str) -> Result<PathBuf> {
    if id.contains(['/', '\\', '.']) {
        anyhow::bail!("invalid profile id");
    }
    Ok(store::profiles_dir()?.join(format!("{id}.json")))
}

pub fn list_all() -> Result<Vec<ProfileMeta>> {
    let dir = store::profiles_dir()?;
    let mut out = Vec::new();
    for entry in fs::read_dir(&dir)? {
        let entry = entry?;
        if entry.path().extension().and_then(|s| s.to_str()) != Some("json") {
            continue;
        }
        let path = entry.path();
        let body = fs::read_to_string(&path)?;
        let Ok(mut stored): std::result::Result<StoredProfile, _> = serde_json::from_str(&body) else {
            continue;
        };
        // Hide ephemeral profiles.
        if stored.meta.temporary {
            continue;
        }
        // Backfill legacy profiles' created_at from file mtime, then persist.
        if stored.meta.created_at.is_none() {
            let mtime = entry
                .metadata()
                .ok()
                .and_then(|m| m.modified().ok())
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| format!("@{}", d.as_secs()));
            if let Some(ts) = mtime {
                stored.meta.created_at = Some(ts);
                if let Ok(body) = serde_json::to_string_pretty(&stored) {
                    let _ = fs::write(&path, body);
                }
            }
        }
        let name = stored
            .config
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("(unnamed)")
            .to_string();
        let notes = stored
            .config
            .get("notes")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        out.push(ProfileMeta {
            id: stored.meta.id,
            name,
            notes,
            proxy_id: stored.meta.proxy_id,
            last_launched_at: stored.meta.last_launched_at,
            created_at: stored.meta.created_at,
            pinned: stored.meta.pinned,
            folder: stored.meta.folder,
            total_runtime_ms: stored.meta.total_runtime_ms,
        });
    }
    // Pinned first, then newest-first by created_at; name fallback for same-second ties.
    out.sort_by(|a, b| {
        match (a.pinned, b.pinned) {
            (true, false) => return std::cmp::Ordering::Less,
            (false, true) => return std::cmp::Ordering::Greater,
            _ => {}
        }
        match (&b.created_at, &a.created_at) {
            (Some(bv), Some(av)) => bv.cmp(av),
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (None, None) => a.name.cmp(&b.name),
        }
    });
    Ok(out)
}

/// Delete leftover temporary profiles after a crash; returns count.
pub fn purge_temporary() -> Result<usize> {
    let dir = store::profiles_dir()?;
    let mut n = 0;
    for entry in fs::read_dir(&dir)? {
        let entry = entry?;
        if entry.path().extension().and_then(|s| s.to_str()) != Some("json") {
            continue;
        }
        let Ok(body) = fs::read_to_string(entry.path()) else { continue; };
        let Ok(stored): std::result::Result<StoredProfile, _> = serde_json::from_str(&body) else {
            continue;
        };
        if stored.meta.temporary && !stored.meta.id.is_empty() {
            let _ = delete(&stored.meta.id);
            n += 1;
        }
    }
    Ok(n)
}

pub fn load_raw(id: &str) -> Result<StoredProfile> {
    let path = path_for(id)?;
    let body = fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
    let stored: StoredProfile = serde_json::from_str(&body)?;
    Ok(stored)
}

pub fn save_raw(stored: &mut StoredProfile) -> Result<()> {
    let is_new = stored.meta.id.is_empty();
    if is_new {
        stored.meta.id = uuid::Uuid::new_v4().to_string();
    }
    // Carry created_at/pinned/folder/last_launched_at through edits.
    // pinned and folder are owned by set_pin/set_folder respectively.
    if !is_new {
        if let Ok(existing) = load_raw(&stored.meta.id) {
            if stored.meta.created_at.is_none() {
                stored.meta.created_at = existing.meta.created_at;
            }
            stored.meta.pinned = existing.meta.pinned;
            if stored.meta.folder.is_empty() {
                stored.meta.folder = existing.meta.folder;
            }
            if stored.meta.last_launched_at.is_none() {
                stored.meta.last_launched_at = existing.meta.last_launched_at;
            }
            // total_runtime_ms is owned by the Tracker — every save (edit /
            // proxy bind / folder move) carries the existing counter through.
            if stored.meta.total_runtime_ms == 0 {
                stored.meta.total_runtime_ms = existing.meta.total_runtime_ms;
            }
        }
    }
    if stored.meta.created_at.is_none() {
        stored.meta.created_at = Some(chrono_now_iso());
    }
    let path = path_for(&stored.meta.id)?;
    let body = serde_json::to_string_pretty(stored)?;
    fs::write(path, body)?;
    Ok(())
}

pub fn delete(id: &str) -> Result<()> {
    let path = path_for(id)?;
    if path.exists() {
        fs::remove_file(path)?;
    }
    // Also wipe per-profile user-data-dir.
    let udd = store::user_data_root()?.join(id);
    if udd.exists() {
        let _ = fs::remove_dir_all(udd);
    }
    Ok(())
}

/// Add `ms` to the persisted total_runtime_ms counter.  Called by the
/// process Tracker when the engine exits — totals survive launcher restarts.
pub fn add_runtime(id: &str, ms: u64) -> Result<()> {
    let mut p = load_raw(id)?;
    p.meta.total_runtime_ms = p.meta.total_runtime_ms.saturating_add(ms);
    save_raw(&mut p)?;
    Ok(())
}

/// Touch last_launched_at; optionally switch bound proxy.
pub fn touch_launched(id: &str, proxy_id: Option<String>) -> Result<()> {
    let mut p = load_raw(id)?;
    p.meta.last_launched_at = Some(chrono_now_iso());
    if proxy_id.is_some() {
        p.meta.proxy_id = proxy_id;
    }
    save_raw(&mut p)?;
    Ok(())
}

pub fn clone_profile(id: &str) -> Result<ProfileMeta> {
    let mut src = load_raw(id)?;
    let new_id = uuid::Uuid::new_v4().to_string();
    let old_name = src
        .config
        .get("name")
        .and_then(|v| v.as_str())
        .unwrap_or("profile")
        .to_string();
    src.meta.id = new_id.clone();
    src.meta.last_launched_at = None;
    src.meta.created_at = None;
    src.meta.pinned = false;
    src.config
        .insert("name".into(), serde_json::Value::String(format!("{old_name} (copy)")));
    // Re-randomize CPU/RAM/platform_version so the copy doesn't collide on those axes.
    crate::randomize_platform_version(&mut src.config);
    crate::randomize_hardware(&mut src.config);
    save_raw(&mut src)?;
    Ok(ProfileMeta {
        id: src.meta.id,
        name: format!("{old_name} (copy)"),
        notes: src
            .config
            .get("notes")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        proxy_id: src.meta.proxy_id,
        last_launched_at: None,
        created_at: src.meta.created_at,
        pinned: false,
        folder: src.meta.folder,
        total_runtime_ms: 0,
    })
}

/// Flip pin flag.
pub fn set_pin(id: &str, pinned: bool) -> Result<()> {
    let mut p = load_raw(id)?;
    p.meta.pinned = pinned;
    let path = path_for(&p.meta.id)?;
    let body = serde_json::to_string_pretty(&p)?;
    fs::write(path, body)?;
    Ok(())
}

/// Assign folder tag (empty string clears).
pub fn set_folder(id: &str, folder: &str) -> Result<()> {
    let mut p = load_raw(id)?;
    p.meta.folder = folder.trim().to_string();
    let path = path_for(&p.meta.id)?;
    let body = serde_json::to_string_pretty(&p)?;
    fs::write(path, body)?;
    Ok(())
}

/// Retag profiles from folder `old` to `new`; returns count.
pub fn rename_folder(old: &str, new: &str) -> Result<usize> {
    let dir = store::profiles_dir()?;
    let new = new.trim();
    let mut n = 0;
    for entry in fs::read_dir(&dir)? {
        let entry = entry?;
        if entry.path().extension().and_then(|s| s.to_str()) != Some("json") {
            continue;
        }
        let Ok(body) = fs::read_to_string(entry.path()) else { continue; };
        let Ok(mut stored): std::result::Result<StoredProfile, _> = serde_json::from_str(&body)
        else {
            continue;
        };
        if stored.meta.folder == old {
            stored.meta.folder = new.to_string();
            if let Ok(out) = serde_json::to_string_pretty(&stored) {
                let _ = fs::write(entry.path(), out);
            }
            n += 1;
        }
    }
    Ok(n)
}

/// Delete folder; `delete_profiles` true removes, false unfiles. Returns count.
pub fn delete_folder(name: &str, delete_profiles: bool) -> Result<usize> {
    let dir = store::profiles_dir()?;
    let mut n = 0;
    for entry in fs::read_dir(&dir)? {
        let entry = entry?;
        if entry.path().extension().and_then(|s| s.to_str()) != Some("json") {
            continue;
        }
        let Ok(body) = fs::read_to_string(entry.path()) else { continue; };
        let Ok(mut stored): std::result::Result<StoredProfile, _> = serde_json::from_str(&body)
        else {
            continue;
        };
        if stored.meta.folder == name {
            if delete_profiles {
                let _ = delete(&stored.meta.id);
            } else {
                stored.meta.folder = String::new();
                if let Ok(out) = serde_json::to_string_pretty(&stored) {
                    let _ = fs::write(entry.path(), out);
                }
            }
            n += 1;
        }
    }
    Ok(n)
}

/// Per-profile user-data-dir; created on first call.
pub fn user_data_dir(id: &str) -> Result<PathBuf> {
    if id.contains(['/', '\\', '.']) {
        anyhow::bail!("invalid profile id");
    }
    let p = store::user_data_root()?.join(id);
    std::fs::create_dir_all(&p)?;
    Ok(p)
}

fn chrono_now_iso() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let s = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format!("@{s}")
}
