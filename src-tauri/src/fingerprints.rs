// User-managed Fingerprint Library.
//
// Each entry is a full FingerprintConfig JSON stored under
// `$CONFIG/shardx-launcher/fingerprints/<id>.json`.  The GPU select in
// the profile editor pulls its options from here — i.e. the user can
// curate which GPUs/devices show up by importing their own JSON files
// (or by deleting bundled ones).
//
// On first run we seed the directory with the bundled gpu presets so
// the launcher is usable out of the box.

use crate::store;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::fs;
use std::path::PathBuf;

/// One row in the library UI; also what the profile editor uses to
/// populate the GPU select.  `payload` is the verbatim FingerprintConfig
/// (without the `_meta` envelope launcher profiles use).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LibraryEntry {
    pub id: String,
    pub label: String,
    pub platform: String,
    pub chrome: String,
    pub gpu: String,
    pub tag_color: String,
    /// True for the bundled starter set — UI marks them so the user
    /// knows they can re-seed by deleting/reimporting.
    #[serde(default)]
    pub builtin: bool,
    /// Full FingerprintConfig JSON.  Stored inline so the frontend can
    /// preview it and the profile editor can read derived fields
    /// (screen, platform, webgl) without a second round-trip.
    pub payload: Value,
}

fn safe_id(id: &str) -> Result<String> {
    if id.is_empty() || id.contains(['/', '\\']) || id.contains("..") {
        anyhow::bail!("invalid fingerprint id");
    }
    Ok(id.to_string())
}

fn path_for(id: &str) -> Result<PathBuf> {
    let id = safe_id(id)?;
    Ok(store::fingerprints_dir()?.join(format!("{id}.json")))
}

/// Seed the library with the bundled GPU presets on the first call
fn tag_color_for(platform: &str) -> String {
    match platform {
        "macOS" => "#8b5cf6".into(),
        "Windows" => "#5dade2".into(),
        "Linux" => "#4ade80".into(),
        _ => "#a78bfa".into(),
    }
}

pub fn list_all() -> Result<Vec<LibraryEntry>> {
    // Pure filesystem read.  Everything in
    //   $CONFIG/shardx-launcher/fingerprints/*.json
    // becomes a library entry, no matter how it got there — UI
    // imports, drag-and-drop, or the user dumping files in by hand.
    // No bundled set, no compile-time tables, no "builtin" concept.
    let dir = store::fingerprints_dir()?;
    let mut out = Vec::new();
    for entry in fs::read_dir(&dir)? {
        let entry = entry?;
        if entry.path().extension().and_then(|s| s.to_str()) != Some("json") {
            continue;
        }
        let body = fs::read_to_string(entry.path())?;
        if let Ok(e) = serde_json::from_str::<LibraryEntry>(&body) {
            out.push(e);
        } else if let Ok(payload) = serde_json::from_str::<Value>(&body) {
            // Bare FingerprintConfig (no LibraryEntry wrapper) — wrap on
            // the fly so user-imported files that came straight from
            // ShardX still show up.
            let id = entry
                .path()
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("imported")
                .to_string();
            out.push(wrap_payload(&id, &payload));
        }
    }
    out.sort_by(|a, b| a.label.cmp(&b.label));
    Ok(out)
}

/// Pull a label / platform / chrome / GPU description out of a raw
/// FingerprintConfig.  Used both at import time and when listing
/// bare-JSON files in the library dir.
fn wrap_payload(id: &str, p: &Value) -> LibraryEntry {
    let label = p
        .get("name")
        .and_then(|v| v.as_str())
        .unwrap_or(id)
        .to_string();
    let platform = p
        .get("navigator")
        .and_then(|n| n.get("platform"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let chrome = p
        .get("client_hints")
        .and_then(|c| c.get("brand_version"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let gpu = p
        .get("webgl")
        .and_then(|w| w.get("renderer"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    LibraryEntry {
        id: id.to_string(),
        label,
        platform: platform.clone(),
        chrome,
        gpu,
        tag_color: tag_color_for(&platform),
        builtin: false,
        payload: p.clone(),
    }
}

pub fn get(id: &str) -> Result<Option<LibraryEntry>> {
    Ok(list_all()?.into_iter().find(|e| e.id == id))
}

/// Import a raw FingerprintConfig JSON.  Accepts the user's text and
/// returns the saved entry.  If `id` is empty a slug is derived from
/// `payload.name` (or a UUID if no name).
pub fn import(json_text: &str, id_hint: Option<String>) -> Result<LibraryEntry> {
    let payload: Value =
        serde_json::from_str(json_text).context("not a valid JSON FingerprintConfig")?;
    let raw_id = id_hint.filter(|s| !s.trim().is_empty()).unwrap_or_else(|| {
        payload
            .get("name")
            .and_then(|v| v.as_str())
            .map(slugify)
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string())
    });
    let id = ensure_unique_id(&raw_id)?;
    let entry = wrap_payload(&id, &payload);
    let path = path_for(&id)?;
    fs::write(path, serde_json::to_string_pretty(&entry)?)?;
    Ok(entry)
}

fn slugify(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
        } else if c == ' ' || c == '_' || c == '-' || c == '.' {
            out.push('-');
        }
    }
    let trimmed = out.trim_matches('-').to_string();
    if trimmed.is_empty() {
        uuid::Uuid::new_v4().to_string()
    } else {
        trimmed
    }
}

fn ensure_unique_id(base: &str) -> Result<String> {
    let dir = store::fingerprints_dir()?;
    if !dir.join(format!("{base}.json")).exists() {
        return Ok(base.into());
    }
    for n in 2..1000 {
        let cand = format!("{base}-{n}");
        if !dir.join(format!("{cand}.json")).exists() {
            return Ok(cand);
        }
    }
    Ok(format!("{base}-{}", uuid::Uuid::new_v4()))
}

pub fn delete(id: &str) -> Result<()> {
    let path = path_for(id)?;
    if path.exists() {
        fs::remove_file(path)?;
    }
    Ok(())
}
