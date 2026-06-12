//! Self-bootstrapping runtime: download ShardX browser + Widevine from R2.
//! Emits `runtime:progress` and `runtime:done` events to the Tauri frontend.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use tauri::{Emitter, Window};
use tokio::io::AsyncWriteExt;

const PUB_BASE: &str = "https://pub-e57a7c60f6934eb09a6600bf2fc59cdc.r2.dev";
/// Version manifest (GitHub raw) — one tiny GET yields every archive's current
/// etag, so install/status checks never poll R2/S3 per-archive.
const MANIFEST_URL: &str =
    "https://raw.githubusercontent.com/ProxyShard/ShardBrowser/main/runtime.json";
const LAUNCHER_RELEASE_REPO: &str = "ProxyShard/ShardBrowser";
/// Chromium version baked into the current bundle (used for Mac Framework path).
const CHROMIUM_VERSION: &str = "149.0.7827.103";

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct ArchiveSpec {
    pub key: String,
    pub label: String,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct PlatformSpec {
    pub browser: ArchiveSpec,
    pub widevine: Option<ArchiveSpec>,
}

/// Archives required for this host; None on unsupported platforms.
pub fn host_spec() -> Option<PlatformSpec> {
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    return Some(PlatformSpec {
        browser: ArchiveSpec {
            key: "ShardX-Mac-arm64.zip".into(),
            label: "ShardX browser (macOS arm64)".into(),
        },
        widevine: Some(ArchiveSpec {
            key: "ShardX-Widevine-Mac-arm64.zip".into(),
            label: "Widevine CDM".into(),
        }),
    });
    #[cfg(all(target_os = "windows", target_arch = "x86_64"))]
    return Some(PlatformSpec {
        browser: ArchiveSpec {
            key: "ShardX-Windows.zip".into(),
            label: "ShardX browser (Windows x64)".into(),
        },
        widevine: Some(ArchiveSpec {
            key: "ShardX-Widevine-Win.zip".into(),
            label: "Widevine CDM".into(),
        }),
    });
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    return Some(PlatformSpec {
        browser: ArchiveSpec {
            key: "ShardX-Linux.zip".into(),
            label: "ShardX browser (Linux x64)".into(),
        },
        widevine: Some(ArchiveSpec {
            key: "ShardX-Widevine-Linux.zip".into(),
            label: "Widevine CDM".into(),
        }),
    });
    #[allow(unreachable_code)]
    None
}

/// Runtime dir under the platform data dir; kept outside the launcher bundle.
pub fn runtime_dir() -> Result<PathBuf> {
    Ok(dirs::data_dir()
        .context("platform data dir not available")?
        .join("shardx-launcher")
        .join("runtime"))
}

/// Path to the chrome binary inside the extracted runtime.
pub fn binary_path() -> Result<PathBuf> {
    let base = runtime_dir()?;
    #[cfg(target_os = "macos")]
    return Ok(base
        .join("ShardX-Mac-arm64")
        .join("ShardX.app")
        .join("Contents")
        .join("MacOS")
        .join("ShardX"));
    #[cfg(target_os = "windows")]
    return Ok(base.join("ShardX-Windows").join("chrome.exe"));
    #[cfg(target_os = "linux")]
    return Ok(base.join("ShardX-Linux").join("chrome"));
}

fn manifest_path() -> Result<PathBuf> {
    Ok(runtime_dir()?.join("manifest.json"))
}

// Bundled fingerprint library (cross-platform); seeds fingerprints dir on first run.
const FINGERPRINTS_ARCHIVE_KEY: &str = "ShardX-Fingerprints.zip";
const FINGERPRINTS_TOP_DIR: &str = "shardx-fingerprints";

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
struct Manifest {
    browser_etag: Option<String>,
    widevine_etag: Option<String>,
    fingerprints_etag: Option<String>,
    /// Chromium version the *already-created* profiles were last migrated to.
    /// Lets us bump saved profiles' UA + client_hints when the engine updates,
    /// independent of the fingerprint-library seed.
    #[serde(default)]
    applied_chromium_version: Option<String>,
    /// Signature (`<version>|<grease_brand>|<grease_version>`) of the engine
    /// descriptor the profiles/fingerprints were last migrated against. Migration
    /// re-runs whenever this changes — so adding grease (or any future field) to
    /// the manifest auto-triggers a re-migration even for users already on the
    /// current `applied_chromium_version`. No bump-the-constant ceremony.
    #[serde(default)]
    applied_signature: Option<String>,
    /// Chromium version of the engine binary currently extracted on disk.
    /// The engine update is detected by comparing THIS to the manifest's
    /// `chromium_version` — robust where the etag check failed (e.g. a user who
    /// updated the app but whose stored etag already matched).
    #[serde(default)]
    installed_chromium_version: Option<String>,
}

/// Chromium version of the engine actually on disk (ground truth), read from
/// the bundle layout: the macOS Framework `Versions/<ver>/` dir, the Windows
/// `<ver>.manifest` file. Returns `None` on Linux (no on-disk marker) — callers
/// fall back to the stored `installed_chromium_version`.
fn installed_engine_version() -> Option<String> {
    let base = runtime_dir().ok()?;
    #[cfg(target_os = "macos")]
    {
        let versions = base
            .join("ShardX-Mac-arm64")
            .join("ShardX.app")
            .join("Contents")
            .join("Frameworks")
            .join("ShardX Framework.framework")
            .join("Versions");
        for ent in fs::read_dir(&versions).ok()?.flatten() {
            let name = ent.file_name().to_string_lossy().to_string();
            if name != "Current" && name.chars().next().is_some_and(|c| c.is_ascii_digit()) {
                return Some(name);
            }
        }
        None
    }
    #[cfg(target_os = "windows")]
    {
        let dir = base.join("ShardX-Windows");
        for ent in fs::read_dir(&dir).ok()?.flatten() {
            let p = ent.path();
            if p.extension().and_then(|s| s.to_str()) == Some("manifest") {
                return p.file_stem().and_then(|s| s.to_str()).map(String::from);
            }
        }
        None
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        let _ = base;
        None
    }
}

/// Effective installed engine version: the on-disk version if readable, else
/// the version we last recorded in the manifest.
fn effective_installed_version(local: &Manifest) -> Option<String> {
    installed_engine_version().or_else(|| local.installed_chromium_version.clone())
}

fn load_manifest() -> Manifest {
    let Ok(p) = manifest_path() else { return Manifest::default() };
    fs::read_to_string(p)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn save_manifest(m: &Manifest) -> Result<()> {
    let p = manifest_path()?;
    fs::create_dir_all(p.parent().unwrap())?;
    fs::write(p, serde_json::to_string_pretty(m)?)?;
    Ok(())
}

#[derive(Serialize, Clone, Debug)]
pub struct RuntimeStatus {
    pub installed: bool,
    pub binary_path: Option<PathBuf>,
    pub installed_browser_etag: Option<String>,
    pub remote_browser_etag: Option<String>,
    pub update_available: bool,
    pub spec: Option<PlatformSpec>,
    /// True once the fingerprint library bundle has been extracted.
    pub fingerprints_installed: bool,
}

#[derive(Default)]
struct RemoteManifest {
    archives: std::collections::HashMap<String, String>,
    chromium_version: Option<String>,
    /// GREASE brand/version the engine emits in `sec-ch-ua`. Not derivable from
    /// the version number (it rotates per major release), so it travels in the
    /// manifest as data — migration writes it into every profile/fingerprint.
    grease_brand: Option<String>,
    grease_version: Option<String>,
}

/// Fetch the version manifest (GitHub raw) — one request yielding every
/// archive's current etag + the chromium version, so install/status never poll
/// R2/S3 per-archive. Empty/None when unreachable.
async fn fetch_manifest() -> RemoteManifest {
    async fn inner() -> Option<RemoteManifest> {
        let resp = reqwest::Client::new().get(MANIFEST_URL).send().await.ok()?;
        if !resp.status().is_success() {
            return None;
        }
        let v: serde_json::Value = resp.json().await.ok()?;
        let archives = v
            .get("archives")
            .and_then(|a| a.as_object())
            .map(|o| {
                o.iter()
                    .filter_map(|(k, val)| val.as_str().map(|s| (k.clone(), s.to_string())))
                    .collect()
            })
            .unwrap_or_default();
        let str_field = |k: &str| v.get(k).and_then(|s| s.as_str()).map(String::from);
        Some(RemoteManifest {
            archives,
            chromium_version: str_field("chromium_version"),
            grease_brand: str_field("grease_brand"),
            grease_version: str_field("grease_version"),
        })
    }
    inner().await.unwrap_or_default()
}

/// Migrate every `*.json` in `dir` to a new engine descriptor: bump
/// `navigator.user_agent` (Chrome/<major>.0.0.0) and the version fields in
/// `client_hints` — `brand_version` / `brand_full_version` / `chrome_build` /
/// `chrome_patch` (derived from the version), plus `grease_brand` /
/// `grease_version` / `grease_full_version` (from the manifest, since GREASE
/// can't be derived from the version number). Leaves platform_version,
/// architecture, webgl, etc. intact. Returns the number of files changed.
fn migrate_dir_to(
    dir: &Path,
    chromium_version: &str,
    grease_brand: Option<&str>,
    grease_version: Option<&str>,
) -> Result<usize> {
    let parts: Vec<&str> = chromium_version.split('.').collect();
    if parts.len() != 4 {
        return Ok(0);
    }
    let major = parts[0];
    let build: i64 = parts[2].parse().unwrap_or(0);
    let patch: i64 = parts[3].parse().unwrap_or(0);

    let mut n = 0usize;
    for ent in fs::read_dir(dir)?.flatten() {
        let p = ent.path();
        if p.extension().and_then(|s| s.to_str()) != Some("json") {
            continue;
        }
        let Ok(text) = fs::read_to_string(&p) else { continue };
        let Ok(mut cfg) = serde_json::from_str::<serde_json::Value>(&text) else { continue };
        let mut changed = false;

        // navigator.user_agent: replace the Chrome/<ver> token with major.0.0.0.
        if let Some(ua) = cfg
            .pointer("/navigator/user_agent")
            .and_then(|v| v.as_str())
            .map(String::from)
        {
            if let Some(idx) = ua.find("Chrome/") {
                let rest = &ua[idx + 7..];
                let end = rest.find(' ').unwrap_or(rest.len());
                let new_ua = format!("{}Chrome/{}.0.0.0{}", &ua[..idx], major, &rest[end..]);
                if new_ua != ua {
                    if let Some(slot) = cfg.pointer_mut("/navigator/user_agent") {
                        *slot = serde_json::Value::String(new_ua);
                        changed = true;
                    }
                }
            }
        }

        if let Some(ch) = cfg.get_mut("client_hints").and_then(|v| v.as_object_mut()) {
            let mut wants: Vec<(&str, serde_json::Value)> = vec![
                ("brand_version", serde_json::json!(major)),
                ("brand_full_version", serde_json::json!(chromium_version)),
                ("chrome_build", serde_json::json!(build)),
                ("chrome_patch", serde_json::json!(patch)),
            ];
            // GREASE — only when the manifest carries it (rotates per release).
            if let Some(gb) = grease_brand {
                wants.push(("grease_brand", serde_json::json!(gb)));
            }
            if let Some(gv) = grease_version {
                wants.push(("grease_version", serde_json::json!(gv)));
                wants.push(("grease_full_version", serde_json::json!(format!("{gv}.0.0.0"))));
            }
            for (k, want) in wants {
                if ch.get(k) != Some(&want) {
                    ch.insert(k.to_string(), want);
                    changed = true;
                }
            }
        }

        if changed {
            fs::write(&p, serde_json::to_string_pretty(&cfg)?)?;
            n += 1;
        }
    }
    Ok(n)
}

/// Migrate both the saved profiles AND the fingerprint library (bundled +
/// user-added) to `chromium_version`. Bundled templates are already at the new
/// version after the seed; user-added fingerprints get their UA + client_hints
/// bumped here (their custom fields are preserved).
fn migrate_all_to(
    chromium_version: &str,
    grease_brand: Option<&str>,
    grease_version: Option<&str>,
) -> usize {
    let mut n = 0;
    if let Ok(d) = crate::store::profiles_dir() {
        n += migrate_dir_to(&d, chromium_version, grease_brand, grease_version).unwrap_or(0);
    }
    if let Ok(d) = crate::store::fingerprints_dir() {
        n += migrate_dir_to(&d, chromium_version, grease_brand, grease_version).unwrap_or(0);
    }
    n
}

/// Startup hook: migrate saved profiles + the fingerprint library (bundled +
/// user-added) to the manifest's engine descriptor when not already done. One
/// GitHub-manifest GET (never S3). Guarded by a signature of
/// `<version>|<grease_brand>|<grease_version>`, so a change to the grease (or any
/// future manifest field) re-triggers migration even for users already on the
/// current version — no version bump or constant needed. Also covers users
/// whose engine auto-updated via the etag path without an explicit install.
pub async fn ensure_profiles_migrated() {
    let m = fetch_manifest().await;
    let Some(target) = m.chromium_version.clone() else { return };
    let sig = format!(
        "{target}|{}|{}",
        m.grease_brand.as_deref().unwrap_or(""),
        m.grease_version.as_deref().unwrap_or(""),
    );
    let mut local = load_manifest();
    if local.applied_signature.as_deref() == Some(sig.as_str()) {
        return;
    }
    let n = migrate_all_to(&target, m.grease_brand.as_deref(), m.grease_version.as_deref());
    if n > 0 {
        eprintln!("[runtime] migrated {n} profile/fingerprint file(s) to {sig}");
    }
    local.applied_chromium_version = Some(target);
    local.applied_signature = Some(sig);
    let _ = save_manifest(&local);
}

#[tauri::command]
pub async fn runtime_status() -> Result<RuntimeStatus, String> {
    let spec = host_spec();
    let installed = binary_path().map(|p| p.exists()).unwrap_or(false);
    let m = load_manifest();
    let manifest = fetch_manifest().await;
    let remote = spec
        .as_ref()
        .and_then(|s| manifest.archives.get(&s.browser.key).cloned());
    // Update is detected by VERSION (engine on disk vs manifest's
    // chromium_version), not by etag — robust for users whose stored etag
    // already matched but whose binary never actually updated. Manifest
    // unreachable (chromium_version None) → assume up to date.
    let update_available = installed
        && manifest
            .chromium_version
            .as_deref()
            .is_some_and(|rv| effective_installed_version(&m).as_deref() != Some(rv));
    // Stamp present AND dir has ≥1 .json (catches user-nuked dir).
    let fingerprints_installed = m.fingerprints_etag.is_some()
        && crate::store::fingerprints_dir()
            .map(|d| {
                fs::read_dir(&d)
                    .map(|it| {
                        it.flatten().any(|e| {
                            e.path().extension().and_then(|s| s.to_str()) == Some("json")
                        })
                    })
                    .unwrap_or(false)
            })
            .unwrap_or(false);

    Ok(RuntimeStatus {
        installed,
        binary_path: if installed { binary_path().ok() } else { None },
        installed_browser_etag: m.browser_etag,
        remote_browser_etag: remote,
        update_available,
        spec,
        fingerprints_installed,
    })
}

#[tauri::command]
pub async fn runtime_install(window: Window, force: bool) -> Result<RuntimeStatus, String> {
    let spec = host_spec().ok_or("Host platform has no published ShardX archive")?;
    let base = runtime_dir().map_err(|e| e.to_string())?;
    fs::create_dir_all(&base).map_err(|e| e.to_string())?;

    let installed_now = binary_path().map(|p| p.exists()).unwrap_or(false);
    let local = load_manifest();
    let manifest = fetch_manifest().await;

    // Re-download the engine when its on-disk version differs from the
    // manifest's chromium_version (or when missing / forced). VERSION-based,
    // not etag — so the update actually fires for already-installed users on a
    // bump. Manifest unreachable (None) → don't force a re-download.
    let need_browser = if force || !installed_now {
        true
    } else {
        match &manifest.chromium_version {
            Some(rv) => effective_installed_version(&local).as_deref() != Some(rv.as_str()),
            None => false,
        }
    };
    let browser_etag = if need_browser {
        download_and_extract(&window, &spec.browser, &base)
            .await
            .map_err(|e| e.to_string())?
    } else {
        local.browser_etag.clone().unwrap_or_default()
    };

    let widevine_etag = if let Some(wv) = &spec.widevine {
        // Re-download Widevine only when browser changed or manifest lacks a stamp.
        if need_browser || local.widevine_etag.is_none() {
            let etag = download_and_extract(&window, wv, &base)
                .await
                .map_err(|e| e.to_string())?;
            place_widevine(&base).map_err(|e| e.to_string())?;
            Some(etag)
        } else {
            local.widevine_etag.clone()
        }
    } else {
        None
    };

    // Fingerprint seed: overwrites bundled templates, leaves user-added files;
    // skipped when the etag matches. User-added FP get version-migrated below.
    let fp_remote = manifest.archives.get(FINGERPRINTS_ARCHIVE_KEY).map(|s| s.as_str());
    let fp_etag = install_fingerprints(&window, force, local.fingerprints_etag.as_deref(), fp_remote)
        .await
        .map_err(|e| e.to_string())?
        .or(local.fingerprints_etag);

    // Migrate already-created profiles AND the fingerprint library (incl.
    // user-added) to the new engine descriptor (UA + client_hints incl. grease).
    // Runs only when the version-or-grease signature changed since last time.
    let target_ver = manifest
        .chromium_version
        .clone()
        .unwrap_or_else(|| CHROMIUM_VERSION.to_string());
    let sig = format!(
        "{target_ver}|{}|{}",
        manifest.grease_brand.as_deref().unwrap_or(""),
        manifest.grease_version.as_deref().unwrap_or(""),
    );
    if local.applied_signature.as_deref() != Some(sig.as_str()) {
        let n = migrate_all_to(
            &target_ver,
            manifest.grease_brand.as_deref(),
            manifest.grease_version.as_deref(),
        );
        if n > 0 {
            eprintln!("[runtime] migrated {n} profile/fingerprint file(s) to {sig}");
        }
    }

    save_manifest(&Manifest {
        browser_etag: Some(browser_etag),
        widevine_etag,
        fingerprints_etag: fp_etag,
        applied_chromium_version: Some(target_ver.clone()),
        applied_signature: Some(sig),
        // Record the version now on disk (ground truth where readable, else the
        // version we just installed) so the next launch's version check is exact.
        installed_chromium_version: installed_engine_version().or(Some(target_ver)),
    })
    .map_err(|e| e.to_string())?;

    let _ = window.emit("runtime:done", ());
    runtime_status().await
}

/// Download + seed fingerprint library. Bundled templates are always
/// overwritten (so version bumps propagate); user-added files are left in place.
async fn install_fingerprints(
    window: &Window,
    force: bool,
    local_etag: Option<&str>,
    remote_etag: Option<&str>,
) -> Result<Option<String>> {
    if !force {
        if let (Some(local), Some(remote)) = (local_etag, remote_etag) {
            if local == remote {
                return Ok(None);
            }
        }
    }

    let dir = crate::store::fingerprints_dir()?;
    let spec = ArchiveSpec {
        key: FINGERPRINTS_ARCHIVE_KEY.into(),
        label: "Fingerprint library".into(),
    };
    // Stage outside fingerprints_dir to keep the zip wrapper dir out of the library.
    let staging = dir.join(".staging");
    let _ = fs::remove_dir_all(&staging);
    fs::create_dir_all(&staging)?;
    let etag = download_and_extract(window, &spec, &staging).await?;

    let src = staging.join(FINGERPRINTS_TOP_DIR);
    let walk = if src.exists() { src } else { staging.clone() };
    let mut added = 0;
    let mut overwritten = 0;
    for ent in fs::read_dir(&walk)? {
        let ent = ent?;
        let p = ent.path();
        if p.extension().and_then(|s| s.to_str()) != Some("json") {
            continue;
        }
        // Always overwrite bundled templates so engine-version bumps reach
        // existing libraries. User-added fingerprints (names not in the bundle)
        // are never iterated here, so they stay untouched.
        let dst = dir.join(p.file_name().unwrap());
        let existed = dst.exists();
        fs::copy(&p, &dst)?;
        if existed { overwritten += 1; } else { added += 1; }
    }
    let _ = fs::remove_dir_all(&staging);
    eprintln!("[runtime] fingerprints sync: added={added} overwritten={overwritten}");
    Ok(Some(etag))
}

/// Stream archive → temp file → extract; emits `runtime:progress` events.
async fn download_and_extract(window: &Window, spec: &ArchiveSpec, base: &Path) -> Result<String> {
    let url = format!("{PUB_BASE}/{}", spec.key);
    let mut resp = reqwest::Client::new().get(&url).send().await?.error_for_status()?;
    let total = resp.content_length().unwrap_or(0);
    let etag = resp
        .headers()
        .get("etag")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.trim_matches('"').to_string())
        .unwrap_or_default();

    let tmp = base.join(format!("{}.tmp", spec.key));
    {
        let mut out = tokio::fs::File::create(&tmp).await?;
        let mut received: u64 = 0;
        let mut last_pct: u64 = u64::MAX;
        while let Some(chunk) = resp.chunk().await? {
            out.write_all(&chunk).await?;
            received += chunk.len() as u64;
            // Emit once per integer percent.
            let pct = if total > 0 { received * 100 / total } else { 0 };
            if pct != last_pct {
                last_pct = pct;
                let _ = window.emit(
                    "runtime:progress",
                    serde_json::json!({
                        "label": spec.label,
                        "phase": "download",
                        "received": received,
                        "total": total,
                        "percent": pct,
                    }),
                );
            }
        }
        out.flush().await?;
    }

    let _ = window.emit(
        "runtime:progress",
        serde_json::json!({
            "label": spec.label,
            "phase": "extract",
            "received": total,
            "total": total,
            "percent": 100,
        }),
    );

    let zip_path = tmp.clone();
    let dest = base.to_path_buf();
    tokio::task::spawn_blocking(move || -> Result<()> {
        // On macOS / Linux shell out to the system `unzip`: the Rust `zip`
        // crate's `extract()` does not restore symlinks (rewrites them as
        // text files) or +x bits, and Linux archives that store entries
        // out-of-order vs. their parent dirs trip its file-create with
        // ENOENT ("os error 2") before the parent dir entry is processed.
        // `unzip` handles all three correctly.
        #[cfg(unix)]
        {
            use std::process::Command;
            fs::create_dir_all(&dest)?;
            let out = Command::new("unzip")
                .arg("-q")
                .arg("-o")
                .arg(&zip_path)
                .arg("-d")
                .arg(&dest)
                .output()
                .map_err(|e| anyhow::anyhow!(
                    "system `unzip` not found ({e}); install with `apt install unzip` / `brew install unzip`"
                ))?;
            // unzip exit codes: 0 = clean, 1 = warnings (e.g. archives
            // zipped on Windows have backslashes; extraction still
            // completes correctly), 2+ = real fatal errors per unzip(1).
            let code = out.status.code().unwrap_or(-1);
            if code > 1 {
                let stderr = String::from_utf8_lossy(&out.stderr);
                anyhow::bail!(
                    "unzip failed for {} (exit {}): {}",
                    zip_path.display(),
                    code,
                    stderr.trim()
                );
            }
            return Ok(());
        }
        #[cfg(not(unix))]
        {
            let f = fs::File::open(&zip_path)?;
            let mut archive = zip::ZipArchive::new(f)?;
            archive.extract(&dest)?;
            Ok(())
        }
    })
    .await??;

    let _ = fs::remove_file(&tmp);

    // Linux/mac archives produced on Windows lose every Unix exec bit;
    // restore +x on every ELF/Mach-O file under the runtime tree (not
    // just the main binary — chrome spawns chrome_crashpad_handler,
    // chrome_sandbox, etc., and they all need the exec bit).
    #[cfg(unix)]
    {
        if let Ok(root) = runtime_dir() {
            fix_unix_exec_bits(&root);
        }
    }

    Ok(etag)
}

/// First-4-bytes magic check; matches ELF + every Mach-O flavour.
#[cfg(unix)]
fn fix_unix_exec_bits(root: &Path) {
    use std::io::Read;
    use std::os::unix::fs::PermissionsExt;
    const MAGIC: &[[u8; 4]] = &[
        [0x7f, b'E', b'L', b'F'],                              // ELF
        [0xfe, 0xed, 0xfa, 0xcf], [0xcf, 0xfa, 0xed, 0xfe],   // Mach-O 64 BE/LE
        [0xfe, 0xed, 0xfa, 0xce], [0xce, 0xfa, 0xed, 0xfe],   // Mach-O 32 BE/LE
        [0xca, 0xfe, 0xba, 0xbe], [0xbe, 0xba, 0xfe, 0xca],   // Mach-O universal
    ];
    fn walk(dir: &Path, magic: &[[u8; 4]]) {
        let Ok(entries) = fs::read_dir(dir) else { return };
        for ent in entries.flatten() {
            let p = ent.path();
            let Ok(ft) = ent.file_type() else { continue };
            if ft.is_symlink() { continue; }
            if ft.is_dir() { walk(&p, magic); continue; }
            if !ft.is_file() { continue; }
            let mut head = [0u8; 4];
            let Ok(mut f) = fs::File::open(&p) else { continue };
            if f.read_exact(&mut head).is_err() { continue; }
            if !magic.iter().any(|m| *m == head) { continue; }
            if let Ok(meta) = fs::metadata(&p) {
                let mut perm = meta.permissions();
                perm.set_mode(perm.mode() | 0o111);
                let _ = fs::set_permissions(&p, perm);
            }
        }
    }
    walk(root, MAGIC);
}

/// Move Widevine to `<Framework>.framework/Versions/<ver>/Libraries/WidevineCdm/`.
#[cfg(target_os = "macos")]
fn place_widevine(base: &Path) -> Result<()> {
    let src = base
        .join("ShardX-Widevine-Mac-arm64")
        .join("WidevineCdm");
    if !src.exists() {
        return Ok(());
    }
    let dst = base
        .join("ShardX-Mac-arm64")
        .join("ShardX.app")
        .join("Contents")
        .join("Frameworks")
        .join("ShardX Framework.framework")
        .join("Versions")
        .join(CHROMIUM_VERSION)
        .join("Libraries")
        .join("WidevineCdm");
    if dst.exists() {
        let _ = fs::remove_dir_all(&dst);
    }
    fs::create_dir_all(dst.parent().context("widevine parent")?)?;
    fs::rename(&src, &dst)?;
    let _ = fs::remove_dir(base.join("ShardX-Widevine-Mac-arm64"));
    Ok(())
}

/// Windows flat layout: WidevineCdm/ sits beside chrome.exe.
#[cfg(target_os = "windows")]
fn place_widevine(base: &Path) -> Result<()> {
    let src = base.join("ShardX-Widevine-Win").join("WidevineCdm");
    if !src.exists() {
        return Ok(());
    }
    let dst = base.join("ShardX-Windows").join("WidevineCdm");
    if dst.exists() {
        let _ = fs::remove_dir_all(&dst);
    }
    fs::rename(&src, &dst)?;
    let _ = fs::remove_dir(base.join("ShardX-Widevine-Win"));
    Ok(())
}

/// Linux: WidevineCdm/ next to chrome binary (flat layout).
#[cfg(target_os = "linux")]
fn place_widevine(base: &Path) -> Result<()> {
    let src = base.join("ShardX-Widevine-Linux").join("WidevineCdm");
    if !src.exists() {
        return Ok(());
    }
    let dst = base.join("ShardX-Linux").join("WidevineCdm");
    if dst.exists() {
        let _ = fs::remove_dir_all(&dst);
    }
    fs::rename(&src, &dst)?;
    let _ = fs::remove_dir(base.join("ShardX-Widevine-Linux"));
    Ok(())
}

// ---- launcher self-update check ----

#[derive(Serialize, Clone, Debug)]
pub struct LauncherVersionInfo {
    pub current: String,
    pub latest: Option<String>,
    pub update_available: bool,
    pub release_url: Option<String>,
}

fn norm_ver(v: &str) -> &str {
    v.strip_prefix('v').unwrap_or(v)
}

/// Best-effort SemVer compare, lex fallback per component.
fn is_newer(latest: &str, current: &str) -> bool {
    let a: Vec<_> = norm_ver(latest).split('.').collect();
    let b: Vec<_> = norm_ver(current).split('.').collect();
    for i in 0..a.len().max(b.len()) {
        let x = a.get(i).copied().unwrap_or("0");
        let y = b.get(i).copied().unwrap_or("0");
        match (x.parse::<u64>(), y.parse::<u64>()) {
            (Ok(xn), Ok(yn)) => {
                if xn != yn { return xn > yn; }
            }
            _ => {
                if x != y { return x > y; }
            }
        }
    }
    false
}

#[tauri::command]
pub async fn launcher_update_check(app: tauri::AppHandle) -> Result<LauncherVersionInfo, String> {
    let current = app.package_info().version.to_string();

    let url = format!("https://api.github.com/repos/{LAUNCHER_RELEASE_REPO}/releases/latest");
    let client = match reqwest::Client::builder()
        .user_agent(format!("shardx-launcher/{current}"))
        .build()
    {
        Ok(c) => c,
        Err(e) => return Ok(LauncherVersionInfo {
            current, latest: None, update_available: false, release_url: None,
        }).map_err(|_: String| e.to_string()),
    };

    let resp = client
        .get(&url)
        .timeout(std::time::Duration::from_secs(6))
        .send()
        .await;
    let Ok(resp) = resp else {
        return Ok(LauncherVersionInfo {
            current, latest: None, update_available: false, release_url: None,
        });
    };
    if !resp.status().is_success() {
        // 404/403 etc → report unknown rather than scare the user.
        return Ok(LauncherVersionInfo {
            current, latest: None, update_available: false, release_url: None,
        });
    }
    let body: serde_json::Value = match resp.json().await {
        Ok(v) => v,
        Err(_) => return Ok(LauncherVersionInfo {
            current, latest: None, update_available: false, release_url: None,
        }),
    };
    let latest = body.get("tag_name").and_then(|v| v.as_str()).map(String::from);
    let release_url = body.get("html_url").and_then(|v| v.as_str()).map(String::from);
    let update_available = match &latest {
        Some(l) => is_newer(l, &current),
        None => false,
    };
    Ok(LauncherVersionInfo { current, latest, update_available, release_url })
}
