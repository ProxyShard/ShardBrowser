//! Deleted profiles, kept seven days. Only the files carrying the account are
//! archived — the rest of a Chromium profile is cache, rebuilt on next launch.

use super::profile;
use crate::store;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

pub const RETENTION_DAYS: u64 = 7;
const RETENTION_SECS: u64 = RETENTION_DAYS * 24 * 60 * 60;

/// Files and directories under the profile's user-data dir worth keeping: the
/// session, the logins and the site storage. Everything else is cache.
/// Where a protected profile's sealed data sits in the archive.
const SEALED_DATA: &str = "sealed.data";

const KEEP: &[&str] = &[
    "Local State",
    "Default/Cookies",
    "Default/Login Data",
    "Default/Login Data For Account",
    "Default/Web Data",
    "Default/Preferences",
    "Default/Secure Preferences",
    "Default/History",
    "Default/Bookmarks",
    "Default/Favicons",
    "Default/Local Storage",
    "Default/IndexedDB",
    "Default/Local Extension Settings",
    "Default/Extension State",
    // The rest of an extension's state, as pack.rs keeps it: a restored profile
    // does not reinstall its extensions, so nothing would rebuild these.
    "Default/Sync Extension Settings",
    "Default/Extension Rules",
    "Default/Extension Scripts",
    "Default/DNR Extension Rules",
    // Files of the profile's own Web Store extensions: their records come back in Secure Preferences.
    "Default/Extensions",
];

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrashEntry {
    pub id: String,
    pub name: String,
    pub folder: String,
    /// Unix seconds.
    pub deleted_at: u64,
    /// Unix seconds; `purge_expired` removes anything past it.
    pub expires_at: u64,
    pub size_bytes: u64,
}

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn paths_for(id: &str) -> Result<(PathBuf, PathBuf)> {
    if id.is_empty() || id.contains(['/', '\\']) {
        anyhow::bail!("invalid profile id");
    }
    let dir = store::trash_dir()?;
    Ok((dir.join(format!("{id}.zip")), dir.join(format!("{id}.json"))))
}

/// Archive the profile, then delete the original. The archive holds the
/// profile JSON verbatim plus the kept user-data files under `user-data/`.
pub(in crate::space) fn move_to_trash(id: &str) -> Result<TrashEntry> {
    let stored = profile::load_raw(id)?;
    let (zip_path, meta_path) = paths_for(id)?;

    let file = fs::File::create(&zip_path).with_context(|| format!("create {}", zip_path.display()))?;
    let mut zip = zip::ZipWriter::new(file);
    let opts = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);

    zip.start_file("profile.json", opts)?;
    zip.write_all(serde_json::to_string_pretty(&stored)?.as_bytes())?;

    // A protected profile's data is its sealed archive; it goes in as it is, still sealed.
    let sealed = super::protect::data_path(id)?;
    if sealed.is_file() {
        add_file(&mut zip, &sealed, SEALED_DATA, opts)?;
    }
    let udd = store::user_data_root()?.join(id);
    if udd.exists() {
        for rel in KEEP {
            let src = udd.join(rel);
            if src.is_dir() {
                add_dir(&mut zip, &src, &format!("user-data/{rel}"), opts)?;
            } else if src.is_file() {
                add_file(&mut zip, &src, &format!("user-data/{rel}"), opts)?;
            }
        }
    }
    zip.finish()?;

    let name = stored
        .config
        .get("name")
        .and_then(|v| v.as_str())
        .unwrap_or("(unnamed)")
        .to_string();
    let deleted_at = now();
    let entry = TrashEntry {
        id: id.to_string(),
        name,
        folder: stored.meta.folder.clone(),
        deleted_at,
        expires_at: deleted_at + RETENTION_SECS,
        size_bytes: fs::metadata(&zip_path).map(|m| m.len()).unwrap_or(0),
    };
    fs::write(&meta_path, serde_json::to_string_pretty(&entry)?)?;

    profile::delete(id)?;
    Ok(entry)
}

fn add_file<W: Write + std::io::Seek>(
    zip: &mut zip::ZipWriter<W>,
    src: &Path,
    name: &str,
    opts: zip::write::SimpleFileOptions,
) -> Result<()> {
    // A profile that is still running holds locks on some of these; a file we
    // cannot read is one the operator loses, not a reason to lose the rest.
    let Ok(bytes) = fs::read(src) else { return Ok(()) };
    zip.start_file(name, opts)?;
    zip.write_all(&bytes)?;
    Ok(())
}

fn add_dir<W: Write + std::io::Seek>(
    zip: &mut zip::ZipWriter<W>,
    src: &Path,
    prefix: &str,
    opts: zip::write::SimpleFileOptions,
) -> Result<()> {
    let Ok(rd) = fs::read_dir(src) else { return Ok(()) };
    for entry in rd.flatten() {
        if store::is_os_litter(&entry.file_name()) {
            continue;
        }
        let name = entry.file_name().to_string_lossy().to_string();
        let child = format!("{prefix}/{name}");
        match entry.file_type() {
            Ok(t) if t.is_dir() => add_dir(zip, &entry.path(), &child, opts)?,
            Ok(_) => add_file(zip, &entry.path(), &child, opts)?,
            Err(_) => {}
        }
    }
    Ok(())
}

pub(in crate::space) fn list() -> Result<Vec<TrashEntry>> {
    let dir = store::trash_dir()?;
    let mut out = Vec::new();
    for entry in fs::read_dir(&dir)? {
        let entry = entry?;
        let path = entry.path();
        if store::is_os_litter(&entry.file_name()) || path.extension().and_then(|s| s.to_str()) != Some("json") {
            continue;
        }
        let Ok(body) = fs::read_to_string(&path) else { continue };
        if let Ok(e) = serde_json::from_str::<TrashEntry>(&body) {
            out.push(e);
        }
    }
    out.sort_by(|a, b| b.deleted_at.cmp(&a.deleted_at));
    Ok(out)
}

/// Unpack back into place. The profile keeps its old id, so anything that
/// referenced it — a script, a bookmark folder — still points at it.
pub(in crate::space) fn restore(id: &str) -> Result<profile::ProfileMeta> {
    let (zip_path, meta_path) = paths_for(id)?;
    let file = fs::File::open(&zip_path).with_context(|| format!("open {}", zip_path.display()))?;
    let mut zip = zip::ZipArchive::new(file)?;

    let udd = store::user_data_root()?.join(id);
    let mut stored: Option<profile::StoredProfile> = None;
    for i in 0..zip.len() {
        let mut f = zip.by_index(i)?;
        let Some(rel) = f.enclosed_name() else { continue };
        let rel_str = rel.to_string_lossy().replace('\\', "/");
        if f.is_dir() {
            continue;
        }
        let mut buf = Vec::with_capacity(f.size() as usize);
        f.read_to_end(&mut buf)?;
        if rel_str == "profile.json" {
            stored = Some(serde_json::from_slice(&buf)?);
            continue;
        }
        if rel_str == SEALED_DATA {
            fs::write(super::protect::data_path(id)?, buf)?;
            continue;
        }
        let Some(sub) = rel_str.strip_prefix("user-data/") else { continue };
        let out = udd.join(sub);
        if let Some(parent) = out.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(out, buf)?;
    }
    let mut stored = stored.context("archive has no profile.json")?;
    // save_raw would mint a new id for an empty one; the archive always has it.
    profile::save_raw(&mut stored)?;

    let _ = fs::remove_file(&zip_path);
    let _ = fs::remove_file(&meta_path);

    Ok(profile::ProfileMeta {
        id: stored.meta.id.clone(),
        name: stored
            .config
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("(unnamed)")
            .to_string(),
        notes: stored
            .config
            .get("notes")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        proxy_id: stored.meta.proxy_id.clone(),
        last_launched_at: stored.meta.last_launched_at.clone(),
        created_at: stored.meta.created_at.clone(),
        pinned: stored.meta.pinned,
        folder: stored.meta.folder.clone(),
        total_runtime_ms: stored.meta.total_runtime_ms,
        color: stored.meta.color.clone(),
        extensions: stored.meta.extensions.clone(),
        mobile: profile::claims_mobile(&stored.config),
        android_media: false,
        origin: profile::Origin::Personal,
        rights: None,
        open_on: None,
        tags: None,
        sync: None,
        protected: stored.meta.lock.is_some(),
    })
}

/// Delete one archive for good.
pub(in crate::space) fn purge(id: &str) -> Result<()> {
    let (zip_path, meta_path) = paths_for(id)?;
    let _ = fs::remove_file(zip_path);
    let _ = fs::remove_file(meta_path);
    Ok(())
}

/// Drop everything past its seven days; returns how many went. Run at startup.
pub(in crate::space) fn purge_expired() -> Result<usize> {
    let now = now();
    let mut n = 0;
    for e in list()? {
        if e.expires_at <= now {
            purge(&e.id)?;
            n += 1;
        }
    }
    Ok(n)
}
