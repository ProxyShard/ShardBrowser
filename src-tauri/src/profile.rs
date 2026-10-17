use crate::store;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, OnceLock};

/// Serialises every read-modify-write of a profile file: two interleaving — a
/// launch touching last_launched_at while the editor saves — lose one change.
fn file_lock() -> MutexGuard<'static, ()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|e| e.into_inner())
}

/// Temp file plus rename: `fs::write` truncates first, and a truncated profile
/// is unparseable, so `list_all` drops it. The retry is for Windows scanners.
fn write_atomic(path: &Path, body: &[u8]) -> Result<()> {
    let tmp = path.with_extension("json.tmp");
    {
        let mut f = fs::File::create(&tmp)
            .with_context(|| format!("create {}", tmp.display()))?;
        f.write_all(body)?;
        // Durable before the rename, or the rename can publish an empty file.
        f.sync_all()?;
    }
    let mut last = None;
    for attempt in 0..4 {
        match fs::rename(&tmp, path) {
            Ok(()) => return Ok(()),
            Err(e) => {
                last = Some(e);
                if attempt < 3 {
                    std::thread::sleep(std::time::Duration::from_millis(50));
                }
            }
        }
    }
    let _ = fs::remove_file(&tmp);
    Err(anyhow::anyhow!(
        "rename {} -> {}: {}",
        tmp.display(),
        path.display(),
        last.map(|e| e.to_string()).unwrap_or_default()
    ))
}

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
    /// Icon accent, `#rrggbb`. None = derived from the name, which is what the
    /// browser does on its own.
    #[serde(default)]
    pub color: Option<String>,
    /// Extension ids from the library, loaded at launch.
    #[serde(default)]
    pub extensions: Vec<String>,
    /// Phone/tablet fingerprint, by the core's own rule. Sync groups must not
    /// mix classes, so the bulk bar reads it.
    #[serde(default)]
    pub mobile: bool,
    /// Whether this profile answers media questions the Android way.
    #[serde(default)]
    pub android_media: bool,
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
    /// Icon accent, `#rrggbb`; absent = derived from the name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
    /// Portable normalized avatar, independent of the original file and profile name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon_png: Option<String>,
    /// Extension ids from the library.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub extensions: Vec<String>,
    /// Answer media questions like a real Android device; phone profiles only.
    /// Blending in and playing video pull apart here, so the operator chooses.
    #[serde(default, skip_serializing_if = "is_false")]
    pub android_media: bool,
    /// Bumped by every write. An editor sends back the number it opened, so a
    /// save landing on top of someone else's is refused instead of silent.
    #[serde(default)]
    pub rev: u64,
}

/// The screen the profile claims, as (width, height).
pub fn claimed_screen(
    config: &serde_json::Map<String, serde_json::Value>,
) -> Option<(i64, i64)> {
    let screen = config.get("screen")?;
    let n = |k: &str| screen.get(k).and_then(|v| v.as_i64()).filter(|v| *v > 0);
    Some((n("width")?, n("height")?))
}

/// Mirrors the core's fingerprint::ProfileClaimsMobile() — same three signals
/// in the same order, so launcher and engine agree on what a profile is.
pub fn claims_mobile(config: &serde_json::Map<String, serde_json::Value>) -> bool {
    let ch = config.get("client_hints");
    if ch.and_then(|c| c.get("mobile")).and_then(|v| v.as_bool()).unwrap_or(false) {
        return true;
    }
    if ch
        .and_then(|c| c.get("platform"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_ascii_lowercase()
        .starts_with("android")
    {
        return true;
    }
    config
        .get("navigator")
        .and_then(|n| n.get("user_agent"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_ascii_lowercase()
        .contains("android")
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
                    let _ = write_atomic(&path, body.as_bytes());
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
            color: stored.meta.color,
            extensions: stored.meta.extensions,
            mobile: claims_mobile(&stored.config),
            android_media: stored.meta.android_media,
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

/// Deterministic non-zero 32-bit seed from the profile id + noise slot (FNV-1a).
/// Same id + slot always yields the same seed (stable fingerprint across
/// launches/edits); different ids yield different seeds (unique per profile).
fn derive_noise_seed(id: &str, slot: &str) -> u32 {
    let s = format!("{id}::{slot}");
    let mut h: u32 = 2166136261;
    for b in s.bytes() {
        h ^= b as u32;
        h = h.wrapping_mul(16777619);
    }
    // 0 is the "derive automatically" sentinel — never hand it back as a value.
    if h == 0 {
        1
    } else {
        h
    }
}

/// Replace every auto-sentinel noise seed (`seed == 0` or absent) with a
/// stable per-profile value derived from the final profile id.  The UI can't
/// know the id at create time, so it sends `seed: 0` for every vector; without
/// this every freshly-created profile would otherwise share one placeholder
/// seed and produce an identical canvas/audio/WebGL fingerprint.
fn fill_noise_seeds(config: &mut serde_json::Map<String, serde_json::Value>, id: &str) {
    let Some(noise) = config.get_mut("noise").and_then(|n| n.as_object_mut()) else {
        return;
    };
    for (slot, block) in noise.iter_mut() {
        let Some(obj) = block.as_object_mut() else {
            continue;
        };
        let needs = obj
            .get("seed")
            .and_then(|v| v.as_u64())
            .map(|n| n == 0)
            .unwrap_or(true);
        if needs {
            obj.insert("seed".into(), serde_json::Value::from(derive_noise_seed(id, slot)));
        }
    }
}

/// Reset every noise seed back to the auto sentinel so the next `save_raw`
/// re-derives them from a fresh id.  Used when cloning so the copy doesn't
/// inherit the source's canvas/audio/WebGL fingerprint.
fn clear_noise_seeds(config: &mut serde_json::Map<String, serde_json::Value>) {
    let Some(noise) = config.get_mut("noise").and_then(|n| n.as_object_mut()) else {
        return;
    };
    for (_, block) in noise.iter_mut() {
        if let Some(obj) = block.as_object_mut() {
            obj.insert("seed".into(), serde_json::Value::from(0u32));
        }
    }
}

/// The `rev` currently on disk, or 0 when there is no such profile yet.
pub fn current_rev(id: &str) -> u64 {
    load_raw(id).map(|p| p.meta.rev).unwrap_or(0)
}

pub fn save_raw(stored: &mut StoredProfile) -> Result<()> {
    let _guard = file_lock();
    save_raw_locked(stored)
}

/// Body of `save_raw` for callers already holding the lock. It reloads the file
/// itself, so a caller that loaded first must hold the lock across both halves.
fn save_raw_locked(stored: &mut StoredProfile) -> Result<()> {
    if let Some(icon) = stored.meta.icon_png.as_deref() {
        crate::profile_icon::decode_avatar(icon)?;
    }
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
            // Only a change to the profile's own content moves the counter.
            // Launching a profile writes last_launched_at, and closing it
            // writes the runtime total — neither is something an open editor
            // is in conflict with, and bumping for them refused the operator's
            // own save with a message blaming an API nobody had called.
            stored.meta.rev = if stored.config == existing.config
                && stored.meta.icon_png == existing.meta.icon_png {
                existing.meta.rev
            } else {
                existing.meta.rev.wrapping_add(1)
            };
        }
    }
    if is_new {
        stored.meta.rev = 1;
    }
    if stored.meta.created_at.is_none() {
        stored.meta.created_at = Some(chrono_now_iso());
    }
    // The id is now final (freshly minted for new profiles, carried through for
    // edits) — derive per-profile noise seeds from it so each profile gets a
    // unique-but-stable fingerprint instead of sharing the UI's placeholder.
    fill_noise_seeds(&mut stored.config, &stored.meta.id);
    let path = path_for(&stored.meta.id)?;
    let body = serde_json::to_string_pretty(stored)?;
    write_atomic(&path, body.as_bytes())?;
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
    let _guard = file_lock();
    let mut p = load_raw(id)?;
    p.meta.total_runtime_ms = p.meta.total_runtime_ms.saturating_add(ms);
    save_raw_locked(&mut p)?;
    Ok(())
}

/// Touch last_launched_at; optionally switch bound proxy.
pub fn touch_launched(id: &str, proxy_id: Option<String>) -> Result<()> {
    let _guard = file_lock();
    let mut p = load_raw(id)?;
    p.meta.last_launched_at = Some(chrono_now_iso());
    if proxy_id.is_some() {
        p.meta.proxy_id = proxy_id;
    }
    save_raw_locked(&mut p)?;
    Ok(())
}

pub fn clone_profile(id: &str) -> Result<ProfileMeta> {
    let _guard = file_lock();
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
    // Same reasoning for the fingerprint noise: drop the source's seeds so
    // save_raw re-derives fresh ones from new_id, giving the copy its own
    // canvas/audio/WebGL fingerprint instead of a clone of the original's.
    clear_noise_seeds(&mut src.config);
    save_raw_locked(&mut src)?;
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
        color: src.meta.color,
        extensions: src.meta.extensions,
        mobile: claims_mobile(&src.config),
        android_media: src.meta.android_media,
    })
}

/// Flip pin flag.
pub fn set_pin(id: &str, pinned: bool) -> Result<()> {
    let _guard = file_lock();
    let mut p = load_raw(id)?;
    p.meta.pinned = pinned;
    let path = path_for(&p.meta.id)?;
    let body = serde_json::to_string_pretty(&p)?;
    write_atomic(&path, body.as_bytes())?;
    Ok(())
}

/// Assign folder tag (empty string clears).
pub fn set_folder(id: &str, folder: &str) -> Result<()> {
    let _guard = file_lock();
    let mut p = load_raw(id)?;
    p.meta.folder = folder.trim().to_string();
    let path = path_for(&p.meta.id)?;
    let body = serde_json::to_string_pretty(&p)?;
    write_atomic(&path, body.as_bytes())?;
    Ok(())
}

/// Retag profiles from folder `old` to `new`; returns count.
pub fn rename_folder(old: &str, new: &str) -> Result<usize> {
    let _guard = file_lock();
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
                let _ = write_atomic(&entry.path(), out.as_bytes());
            }
            n += 1;
        }
    }
    Ok(n)
}

/// Delete folder; `delete_profiles` true removes, false unfiles. Returns count.
pub fn delete_folder(name: &str, delete_profiles: bool) -> Result<usize> {
    let _guard = file_lock();
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
                // Through the trash, like every other delete — a folder wiped
                // by mistake is exactly the case the week is there for.
                if crate::trash::move_to_trash(&stored.meta.id).is_err() {
                    let _ = delete(&stored.meta.id);
                }
            } else {
                stored.meta.folder = String::new();
                if let Ok(out) = serde_json::to_string_pretty(&stored) {
                    let _ = write_atomic(&entry.path(), out.as_bytes());
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Absent from the file when off, so old profiles do not grow a line.
    #[test]
    fn android_media_round_trips_and_stays_absent_when_off() {
        let mut meta = StoredMeta::default();
        assert!(!meta.android_media);
        let off = serde_json::to_string(&meta).unwrap();
        assert!(!off.contains("android_media"), "{off}");

        meta.android_media = true;
        let on = serde_json::to_string(&meta).unwrap();
        assert!(on.contains("\"android_media\":true"), "{on}");

        let back: StoredMeta = serde_json::from_str(&on).unwrap();
        assert!(back.android_media);

        // A profile written before the setting existed reads as off.
        let old: StoredMeta = serde_json::from_str("{}").unwrap();
        assert!(!old.android_media);
    }

    /// The launcher's rule and the core's rule must agree about what a phone is.
    #[test]
    fn claims_mobile_matches_the_cores_three_signals() {
        let m = |json: &str| -> bool {
            let v: serde_json::Value = serde_json::from_str(json).unwrap();
            claims_mobile(v.as_object().unwrap())
        };
        assert!(m(r#"{"client_hints":{"mobile":true}}"#));
        assert!(m(r#"{"client_hints":{"platform":"Android"}}"#));
        assert!(m(r#"{"navigator":{"user_agent":"Mozilla/5.0 (Linux; Android 15) Chrome"}}"#));
        assert!(!m(r#"{"navigator":{"user_agent":"Mozilla/5.0 (Macintosh) Chrome"}}"#));
        // navigator.platform is the field that lies: Chrome for Android says
        // "Linux armv8l" there, and a desktop Linux profile says "Linux x86_64".
        assert!(!m(r#"{"navigator":{"platform":"Linux armv8l"}}"#));
    }
}
