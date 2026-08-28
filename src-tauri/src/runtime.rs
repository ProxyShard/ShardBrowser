//! Self-bootstrapping runtime: download ShardX browser + Widevine from R2.
//! Emits `runtime:progress` and `runtime:done` events to the Tauri frontend.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use tauri::{Emitter, Window};
use tokio::io::AsyncWriteExt;

const PUB_BASE: &str = "https://pub-e57a7c60f6934eb09a6600bf2fc59cdc.r2.dev";
const LAUNCHER_RELEASE_REPO: &str = "ProxyShard/ShardBrowser";
/// Chromium version baked into the current bundle (used for Mac Framework path).
const CHROMIUM_VERSION: &str = "148.0.7778.216";

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
}

fn load_manifest() -> Manifest {
    let Ok(p) = manifest_path() else {
        return Manifest::default();
    };
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

async fn head_etag(url: &str) -> Result<Option<String>> {
    let resp = reqwest::Client::new().head(url).send().await?;
    Ok(resp
        .headers()
        .get("etag")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.trim_matches('"').to_string()))
}

#[tauri::command]
pub async fn runtime_status() -> Result<RuntimeStatus, String> {
    let spec = host_spec();
    let installed = binary_path().map(|p| p.exists()).unwrap_or(false);
    let m = load_manifest();
    let remote = if let Some(s) = &spec {
        head_etag(&format!("{PUB_BASE}/{}", s.browser.key))
            .await
            .unwrap_or(None)
    } else {
        None
    };
    let update_available = match (&m.browser_etag, &remote) {
        (Some(a), Some(b)) => a != b,
        // Don't flag update when R2 unreachable but binary exists.
        (None, _) => !installed,
        _ => false,
    };
    // Stamp present AND dir has ≥1 .json (catches user-nuked dir).
    let fingerprints_installed = m.fingerprints_etag.is_some()
        && crate::store::fingerprints_dir()
            .map(|d| {
                fs::read_dir(&d)
                    .map(|it| {
                        it.flatten()
                            .any(|e| e.path().extension().and_then(|s| s.to_str()) == Some("json"))
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

    // Skip browser when binary on disk and etag matches remote (unless forced).
    let need_browser = if force || !installed_now {
        true
    } else {
        let remote = head_etag(&format!("{PUB_BASE}/{}", spec.browser.key))
            .await
            .map_err(|e| e.to_string())?;
        local.browser_etag.as_ref() != remote.as_ref()
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

    // Additive fingerprint seed (preserves user edits); skipped when etag matches.
    let fp_etag = install_fingerprints(&window, force, local.fingerprints_etag.as_deref())
        .await
        .map_err(|e| e.to_string())?
        .or(local.fingerprints_etag);

    save_manifest(&Manifest {
        browser_etag: Some(browser_etag),
        widevine_etag,
        fingerprints_etag: fp_etag,
    })
    .map_err(|e| e.to_string())?;

    let _ = window.emit("runtime:done", ());
    runtime_status().await
}

/// Download + seed fingerprint library; `force=true` overwrites, `false` is additive.
async fn install_fingerprints(
    window: &Window,
    force: bool,
    local_etag: Option<&str>,
) -> Result<Option<String>> {
    let url = format!("{PUB_BASE}/{FINGERPRINTS_ARCHIVE_KEY}");

    if !force {
        if let (Some(local), Some(remote)) =
            (local_etag, head_etag(&url).await.ok().flatten().as_deref())
        {
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
    let mut skipped_existing = 0;
    for ent in fs::read_dir(&walk)? {
        let ent = ent?;
        let p = ent.path();
        if p.extension().and_then(|s| s.to_str()) != Some("json") {
            continue;
        }
        let dst = dir.join(p.file_name().unwrap());
        match (dst.exists(), force) {
            (true, false) => {
                skipped_existing += 1;
            }
            (true, true) => {
                fs::copy(&p, &dst)?;
                overwritten += 1;
            }
            (false, _) => {
                fs::copy(&p, &dst)?;
                added += 1;
            }
        }
    }
    let _ = fs::remove_dir_all(&staging);
    eprintln!(
        "[runtime] fingerprints sync: added={added} overwritten={overwritten} kept-existing={skipped_existing}"
    );
    Ok(Some(etag))
}

/// Stream archive → temp file → extract; emits `runtime:progress` events.
async fn download_and_extract(window: &Window, spec: &ArchiveSpec, base: &Path) -> Result<String> {
    let url = format!("{PUB_BASE}/{}", spec.key);
    let mut resp = reqwest::Client::new()
        .get(&url)
        .send()
        .await?
        .error_for_status()?;
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
        let f = fs::File::open(&zip_path)?;
        let mut archive = zip::ZipArchive::new(f)?;
        archive.extract(&dest)?;
        Ok(())
    })
    .await??;

    let _ = fs::remove_file(&tmp);

    // Restore +x on unix (zip may have stripped it).
    #[cfg(unix)]
    if let Ok(bin) = binary_path() {
        if bin.exists() {
            use std::os::unix::fs::PermissionsExt;
            let mut p = fs::metadata(&bin)?.permissions();
            p.set_mode(p.mode() | 0o111);
            let _ = fs::set_permissions(&bin, p);
        }
    }

    Ok(etag)
}

/// Move Widevine to `<Framework>.framework/Versions/<ver>/Libraries/WidevineCdm/`.
#[cfg(target_os = "macos")]
fn place_widevine(base: &Path) -> Result<()> {
    let src = base.join("ShardX-Widevine-Mac-arm64").join("WidevineCdm");
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
                if xn != yn {
                    return xn > yn;
                }
            }
            _ => {
                if x != y {
                    return x > y;
                }
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
        Err(e) => {
            return Ok(LauncherVersionInfo {
                current,
                latest: None,
                update_available: false,
                release_url: None,
            })
            .map_err(|_: String| e.to_string())
        }
    };

    let resp = client
        .get(&url)
        .timeout(std::time::Duration::from_secs(6))
        .send()
        .await;
    let Ok(resp) = resp else {
        return Ok(LauncherVersionInfo {
            current,
            latest: None,
            update_available: false,
            release_url: None,
        });
    };
    if !resp.status().is_success() {
        // 404/403 etc → report unknown rather than scare the user.
        return Ok(LauncherVersionInfo {
            current,
            latest: None,
            update_available: false,
            release_url: None,
        });
    }
    let body: serde_json::Value = match resp.json().await {
        Ok(v) => v,
        Err(_) => {
            return Ok(LauncherVersionInfo {
                current,
                latest: None,
                update_available: false,
                release_url: None,
            })
        }
    };
    let latest = body
        .get("tag_name")
        .and_then(|v| v.as_str())
        .map(String::from);
    let release_url = body
        .get("html_url")
        .and_then(|v| v.as_str())
        .map(String::from);
    let update_available = match &latest {
        Some(l) => is_newer(l, &current),
        None => false,
    };
    Ok(LauncherVersionInfo {
        current,
        latest,
        update_available,
        release_url,
    })
}
