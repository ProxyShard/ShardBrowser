//! Self-bootstrapping runtime: download the ShardBrowser engine + Widevine from R2.
//! Emits `runtime:progress` and `runtime:done` events to the Tauri frontend.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::OnceLock;
use tauri::{Emitter, Window};
use tokio::io::AsyncWriteExt;

mod caps;
mod os;
mod swap;

// A launch asks the engine binary whether it still needs --disable-extensions-except.
pub(crate) use caps::loads_extensions_alone as engine_loads_extensions_alone;
// Extension updates measure the same disk before unpacking.
pub(crate) use os::free_bytes;

const PUB_BASE: &str = "https://pub-e57a7c60f6934eb09a6600bf2fc59cdc.r2.dev";
/// Version manifest (GitHub raw) — one tiny GET yields every archive's current
/// etag, so install/status checks never poll R2/S3 per-archive.
const MANIFEST_URL: &str =
    "https://raw.githubusercontent.com/ProxyShard/ShardBrowser/main/runtime.json";
/// Whole-request budget for that GET. The window's first paint waits on
/// `runtime_status`, so being offline has to mean a quick "no manifest" instead
/// of a request the OS lets hang — a network that accepts the connection and
/// never answers, a captive portal, or dead DNS left the window empty (#114).
/// Kept short on purpose: giving up early only costs this launch its update
/// check, while waiting costs the operator a launcher that looks stuck.
const MANIFEST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(4);
const LAUNCHER_RELEASE_REPO: &str = "ProxyShard/ShardBrowser";
/// Chromium version baked into the current bundle (used for Mac Framework path).
pub const CHROMIUM_VERSION: &str = "156.0.8078.48";

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct ArchiveSpec {
    pub key: String,
    pub label: String,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct PlatformSpec {
    pub browser: ArchiveSpec,
}

/// Archives required for this host; None on unsupported platforms.
pub fn host_spec() -> Option<PlatformSpec> {
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    return Some(PlatformSpec {
        browser: ArchiveSpec {
            key: "ShardX-Mac-arm64.zip".into(),
            label: "ShardBrowser engine (macOS arm64)".into(),
        },
    });
    #[cfg(all(target_os = "windows", target_arch = "x86_64"))]
    return Some(PlatformSpec {
        browser: ArchiveSpec {
            key: "ShardX-Windows.zip".into(),
            label: "ShardBrowser engine (Windows x64)".into(),
        },
    });
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    return Some(PlatformSpec {
        browser: ArchiveSpec {
            key: "ShardX-Linux.zip".into(),
            label: "ShardBrowser engine (Linux x64)".into(),
        },
    });
    #[allow(unreachable_code)]
    None
}

/// Runtime dir under the platform data dir; kept outside the launcher bundle.
pub fn runtime_dir() -> Result<PathBuf> {
    // A test gets an empty one under its own root, never the installed engine.
    #[cfg(test)]
    return Ok(crate::store::test_config_or_panic("runtime dir").join("runtime"));
    #[allow(unreachable_code)]
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

/// Top-level dir (under runtime_dir) the engine archive extracts into. Replaced
/// whole on an update (swap.rs), so no file of the previous version lingers.
fn engine_root_dir() -> &'static str {
    #[cfg(target_os = "macos")]
    return "ShardX-Mac-arm64";
    #[cfg(target_os = "windows")]
    return "ShardX-Windows";
    #[cfg(target_os = "linux")]
    return "ShardX-Linux";
    #[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
    return "ShardX-Engine";
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
    /// Build stamp of the archive currently extracted; the fallback for
    /// archives that carry no `shardx-build` file.
    #[serde(default)]
    installed_engine_build: Option<String>,
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
    let layout = swap::Layout::host(runtime_dir().ok()?, "");
    swap::version_in(&layout.live(), layout.marker)
}

/// The engine folder beside its staging folder and backup (swap.rs).
fn host_layout() -> Option<swap::Layout> {
    Some(swap::Layout::host(runtime_dir().ok()?, &host_spec()?.browser.key))
}

/// Read-held while the GPU probe runs the engine, write-held while an install
/// replaces it: neither starts while the other is under way.
fn engine_gate() -> &'static tokio::sync::RwLock<()> {
    static G: OnceLock<tokio::sync::RwLock<()>> = OnceLock::new();
    G.get_or_init(|| tokio::sync::RwLock::new(()))
}

/// One install (or start-up sweep) at a time over the staging and backup folders.
fn install_lock() -> &'static tokio::sync::Mutex<()> {
    static L: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();
    L.get_or_init(|| tokio::sync::Mutex::new(()))
}

/// The engine may be run now; `None` while an install has it.
pub fn engine_use() -> Option<tokio::sync::RwLockReadGuard<'static, ()>> {
    engine_gate().try_read().ok()
}

/// Set for the moment the engine folder is renamed; launches are refused then.
static SWAPPING: AtomicBool = AtomicBool::new(false);

pub fn swapping() -> bool {
    SWAPPING.load(Ordering::SeqCst)
}

pub const UPDATING: &str = "The ShardBrowser engine is being updated. Try again in a moment.";

/// Profiles started by this launcher run the bundled engine (no Browser path set).
fn bundled_engine_in_use() -> bool {
    crate::settings::load()
        .ok()
        .and_then(|s| s.browser_path)
        .is_none_or(|p| !Path::new(&p).exists())
}

/// Before the window asks anything: the engine put back if the launcher died
/// between the two renames of an update, or its record saved if it died just
/// after; leftovers removed in the background.
pub fn recover_at_start() {
    let Some(layout) = host_layout() else { return };
    // A status call that got the lock first does the same.
    if let Ok(_one) = install_lock().try_lock() {
        recover_swap(&layout);
    }
    tauri::async_runtime::spawn(async move {
        let _one = install_lock().lock().await;
        let _ = tokio::task::spawn_blocking(move || swap::sweep(&layout, &swap::Ops::real(&layout))).await;
    });
}

/// What a swap cut short left, settled; held under `install_lock`.
fn recover_swap(layout: &swap::Layout) {
    if swap::restore(layout, &swap::Ops::real(layout)) {
        eprintln!("[runtime] an interrupted engine update was undone; the previous engine is back");
    }
    // Stopped between the swap and its record: the record the journal carries
    // is saved now, or the engine on disk would be downloaded again.
    let Some(record) = swap::finished(layout) else { return };
    let mut m = load_manifest();
    apply_record(&mut m, &record);
    if save_manifest(&m).is_ok() {
        swap::settle(layout);
        eprintln!("[runtime] the record of an engine update that was cut short is saved");
    }
}

/// The engine a swap put in, as the manifest keeps it.
fn apply_record(m: &mut Manifest, r: &swap::Record) {
    m.browser_etag = Some(r.etag.clone());
    if r.version.is_some() {
        m.installed_chromium_version = r.version.clone();
    }
    m.installed_engine_build = r.build.clone();
}

/// Why the last engine install did not finish, for the window to show.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct InstallError {
    /// download | space | extract | verify | busy | swap
    pub stage: String,
    pub message: String,
    /// Profiles running on the engine (stage `busy`), else 0.
    pub running_browsers: u32,
    /// Profile starts under way here (stage `busy`), else 0.
    #[serde(default)]
    pub starting: u32,
    /// The previous engine is installed and works.
    pub engine_kept: bool,
    /// Archive etag (else the version) the attempt was for.
    pub target: Option<String>,
    pub at_ms: u64,
}

fn install_error_path() -> Result<PathBuf> {
    Ok(runtime_dir()?.join("install-error.json"))
}

fn load_install_error() -> Option<InstallError> {
    serde_json::from_str(&fs::read_to_string(install_error_path().ok()?).ok()?).ok()
}

fn save_install_error(e: &InstallError) {
    if let Ok(p) = install_error_path() {
        let _ = fs::write(p, serde_json::to_string_pretty(e).unwrap_or_default());
    }
}

fn clear_install_error() {
    if let Ok(p) = install_error_path() {
        let _ = fs::remove_file(p);
    }
}

/// The `runtime:failed` payload: the stop, and whether its update is unpacked here already.
#[derive(Serialize, Clone)]
struct Failed<'a> {
    #[serde(flatten)]
    err: &'a InstallError,
    update_staged: bool,
}

#[cfg(test)]
mod install_error_tests {
    use super::*;

    #[test]
    fn a_record_from_before_reads_no_start_and_the_event_says_what_is_staged() {
        let old = r#"{"stage":"busy","message":"m","running_browsers":2,"engine_kept":true,"target":"e1","at_ms":1}"#;
        let err: InstallError = serde_json::from_str(old).unwrap();
        assert_eq!((err.running_browsers, err.starting), (2, 0));
        let starting = InstallError { starting: 1, running_browsers: 0, ..err };
        let told = serde_json::to_value(Failed { err: &starting, update_staged: true }).unwrap();
        assert_eq!(told["stage"], "busy");
        assert_eq!((told["starting"].as_u64(), told["running_browsers"].as_u64()), (Some(1), Some(0)));
        assert_eq!(told["update_staged"], true);
    }
}

#[cfg(test)]
mod gate_tests {
    use std::time::Duration;

    /// No GPU probe starts while an install holds the engine, and an install
    /// waits for a probe already running.
    #[tokio::test]
    async fn a_probe_and_an_install_never_overlap() {
        let install = super::engine_gate().write().await;
        assert!(super::engine_use().is_none(), "a probe started during an install");
        drop(install);

        let probe = super::engine_use().expect("no install under way");
        let waited = tokio::time::timeout(Duration::from_millis(200), super::engine_gate().write()).await;
        assert!(waited.is_err(), "the install did not wait for the probe");
        drop(probe);
        let install = tokio::time::timeout(Duration::from_secs(5), super::engine_gate().write()).await;
        assert!(install.is_ok(), "the install did not get the engine once the probe ended");
    }
}

/// Effective installed engine version. Trusts the version recorded at install
/// time (authoritative — written only after a successful download+extract) over
/// re-reading it off disk, whose layout varies per-OS and can carry stale files
/// from a previous version (a leftover `<old>.manifest` made Windows re-download
/// forever). On-disk detection is the fallback for legacy installs that predate
/// `installed_chromium_version`.
/// Build stamp on disk: `<engine>/shardx-build` when the archive ships one,
/// else what was recorded at install time.
fn installed_engine_build(local: &Manifest) -> Option<String> {
    let from_disk = runtime_dir()
        .ok()
        .map(|base| base.join(engine_root_dir()).join("shardx-build"))
        .and_then(|p| fs::read_to_string(p).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    from_disk.or_else(|| local.installed_engine_build.clone())
}

/// The engine build on disk. The GPU-caps probe keys its cache on it: a
/// Chromium bump can move ANGLE, and a stale extension list misjudges profiles.
pub fn engine_version() -> Option<String> {
    effective_installed_version(&load_manifest())
}

fn effective_installed_version(local: &Manifest) -> Option<String> {
    local
        .installed_chromium_version
        .clone()
        .or_else(installed_engine_version)
}

fn load_manifest() -> Manifest {
    let Ok(p) = manifest_path() else { return Manifest::default() };
    fs::read_to_string(p)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn save_manifest(m: &Manifest) -> Result<()> {
    use std::io::Write;
    let p = manifest_path()?;
    fs::create_dir_all(p.parent().unwrap())?;
    // Whole or not at all: a record cut short reads as no engine version.
    let tmp = p.with_extension("json.tmp");
    let mut f = fs::File::create(&tmp)?;
    f.write_all(serde_json::to_string_pretty(m)?.as_bytes())?;
    f.sync_all()?;
    drop(f);
    fs::rename(&tmp, &p)?;
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
    /// The published engine wants a newer launcher than this one. No engine
    /// update is offered while this is true — update the app first.
    pub needs_newer_launcher: bool,
    /// What the manifest asks for, when it asks for anything.
    pub min_launcher_version: Option<String>,
    pub launcher_version: String,
    /// Why the last install did not finish, while it still matters: an update
    /// is still due, or nothing is installed. `None` otherwise.
    pub install_error: Option<InstallError>,
    /// The update is downloaded, unpacked and checked; only the swap is left.
    pub update_staged: bool,
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
    /// TLS ClientHello shape for this engine build — currently
    /// `signature_algorithms` / `cipher_suites`.  Travels as data for the same
    /// reason GREASE does: it changes with the Chromium major (152 added the
    /// ML-DSA post-quantum signature schemes 0x0904-0x0906) and cannot be
    /// derived from the version number.  A profile still advertising the
    /// previous release's list under the new user agent is a JA4 mismatch a
    /// detector reads straight out of `ja4_r`.
    tls: Option<serde_json::Value>,
    /// Which build of the engine archive this is, apart from its Chromium
    /// version. Not the manifest-wide `revision`, which also moves for a TLS
    /// or GREASE edit — those want a migration, not a fresh download.
    engine_build: Option<String>,
    /// Lowest launcher version this engine build can be driven by; absent means
    /// any. An older launcher gets a browser it cannot configure.
    min_launcher_version: Option<String>,
}

/// Fetch the version manifest (GitHub raw) — one request yielding every
/// archive's current etag + the chromium version, so install/status never poll
/// R2/S3 per-archive. Empty when unreachable, refusing, or out of budget — all
/// three answer "no manifest", never an error, because `runtime_status` renders
/// the interface behind this call.
async fn fetch_manifest() -> RemoteManifest {
    fetch_manifest_at(MANIFEST_URL, MANIFEST_TIMEOUT).await
}

/// The fetch itself, address and budget passed in so a test can aim it at a
/// server that never answers.
async fn fetch_manifest_at(url: &str, budget: std::time::Duration) -> RemoteManifest {
    async fn inner(url: &str, budget: std::time::Duration) -> Option<RemoteManifest> {
        // One budget for connect + headers + body: reqwest hands the deadline on
        // to the response, so a server that stalls mid-answer is bounded too.
        let resp = reqwest::Client::new().get(url).timeout(budget).send().await.ok()?;
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
            tls: v.get("tls").filter(|t| t.is_object()).cloned(),
            // Written as a number or a string; both mean the same thing.
            engine_build: v.get("engine_build").and_then(|b| match b {
                serde_json::Value::String(s) => Some(s.clone()),
                serde_json::Value::Number(n) => Some(n.to_string()),
                _ => None,
            }),
            min_launcher_version: str_field("min_launcher_version"),
        })
    }
    inner(url, budget).await.unwrap_or_default()
}

#[cfg(test)]
mod manifest_fetch_tests {
    /// #114: the first paint waits on this GET, so a server that accepts the
    /// connection and then says nothing has to end as "no manifest" within the
    /// budget — that silence, not a refused connection, is what hung the window.
    #[tokio::test]
    async fn a_server_that_never_answers_ends_as_no_manifest() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let held = tokio::spawn(async move {
            let _conn = listener.accept().await;
            std::future::pending::<()>().await;
        });

        let budget = std::time::Duration::from_millis(300);
        let started = std::time::Instant::now();
        let m = super::fetch_manifest_at(&format!("http://{addr}/runtime.json"), budget).await;
        let waited = started.elapsed();
        held.abort();

        assert!(m.archives.is_empty(), "a silent server must not yield archives");
        assert!(m.chromium_version.is_none());
        assert!(waited < budget * 10, "the fetch waited {waited:?} on a silent server");
    }
}

#[cfg(test)]
mod migration_signature_tests {
    use super::{migration_signature, RemoteManifest};

    fn manifest(fp_etag: &str) -> RemoteManifest {
        let mut m = RemoteManifest {
            chromium_version: Some("156.0.8078.48".into()),
            grease_brand: Some("Not:A-Brand".into()),
            grease_version: Some("8".into()),
            ..Default::default()
        };
        m.archives
            .insert(super::FINGERPRINTS_ARCHIVE_KEY.into(), fp_etag.into());
        m
    }

    #[test]
    fn a_reseeded_fingerprint_library_remigrates() {
        // The bundle's Android templates ship without GREASE, and the seed copies
        // them over the migrated copies. The etag moving is the only sign the
        // library is stale again, so it has to be part of the signature.
        let before = migration_signature("156.0.8078.48", &manifest("aaa"));
        let after = migration_signature("156.0.8078.48", &manifest("bbb"));
        assert_ne!(before, after, "a re-seed must not be read as already migrated");
        assert_eq!(
            migration_signature("156.0.8078.48", &manifest("aaa")),
            before,
            "the same inputs must answer the same signature"
        );
    }
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
    tls: Option<&serde_json::Value>,
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

        // TLS: overwrite only the keys the manifest actually carries, so a
        // profile's own `shuffle_extensions` (or any field we don't ship) is
        // preserved.  A profile with no `tls` block at all is left alone —
        // absent means "use the engine default", not "stale".
        if let (Some(want_tls), Some(cfg_tls)) = (
            tls.and_then(|t| t.as_object()),
            cfg.get_mut("tls").and_then(|v| v.as_object_mut()),
        ) {
            for (k, want) in want_tls {
                if cfg_tls.get(k) != Some(want) {
                    cfg_tls.insert(k.clone(), want.clone());
                    changed = true;
                }
            }
        }

        if add_apple_webgpu_features(&mut cfg) {
            changed = true;
        }
        if add_chrome_brand(&mut cfg) {
            changed = true;
        }
        if let Some(obj) = cfg.as_object_mut() {
            if fix_phone_webgl_angle(obj) {
                changed = true;
            }
            // A handset's screen has no window chrome, and its touchscreen
            // takes five fingers.
            if fix_phone_screen_avail(obj) {
                changed = true;
            }
            if fix_phone_touch_points(obj) {
                changed = true;
            }
        }

        if changed {
            fs::write(&p, serde_json::to_string_pretty(&cfg)?)?;
            n += 1;
        }
    }
    Ok(n)
}

/// The brand Chrome lists in `userAgentData.brands` and `Sec-CH-UA`, Android included.
const CHROME_BRAND: &str = "Google Chrome";

/// Names the brand on a phone profile that names none: Android templates lacked it, and the
/// brand list then held only "Chromium" and GREASE. A brand the profile names stays.
fn add_chrome_brand(cfg: &mut serde_json::Value) -> bool {
    if !cfg.as_object().is_some_and(crate::space::profile::claims_mobile) {
        return false;
    }
    let Some(ch) = cfg.get_mut("client_hints").and_then(|v| v.as_object_mut()) else {
        return false;
    };
    if ch.get("brand").and_then(|v| v.as_str()).is_some_and(|b| !b.is_empty()) {
        return false;
    }
    ch.insert("brand".into(), CHROME_BRAND.into());
    true
}

/// WebGPU features Chrome 156 reports on Apple M-series that older templates lack.
const APPLE_WEBGPU_ADDED: &[&str] = &["texture-compression-unaligned"];

/// Adds `APPLE_WEBGPU_ADDED` to an Apple M-series `webgpu.features`; touches nothing else.
/// An absent or empty list means "the host's own", so it stays as it is.
fn add_apple_webgpu_features(cfg: &mut serde_json::Value) -> bool {
    let apple_m = cfg
        .pointer("/webgl/renderer")
        .and_then(|v| v.as_str())
        .and_then(|r| r.find("Apple M").map(|i| &r[i + 7..]))
        .is_some_and(|rest| rest.starts_with(|c: char| c.is_ascii_digit()));
    let apple_adapter = cfg.pointer("/webgpu/vendor").and_then(|v| v.as_str()) == Some("apple");
    if !apple_m || !apple_adapter {
        return false;
    }
    let Some(list) = cfg.pointer_mut("/webgpu/features").and_then(|v| v.as_array_mut()) else {
        return false;
    };
    if list.is_empty() {
        return false;
    }
    let mut changed = false;
    for &feat in APPLE_WEBGPU_ADDED {
        if list.iter().any(|v| v.as_str() == Some(feat)) {
            continue;
        }
        let at = list
            .iter()
            .position(|v| v.as_str().is_some_and(|s| s > feat))
            .unwrap_or(list.len());
        list.insert(at, serde_json::Value::String(feat.to_string()));
        changed = true;
    }
    changed
}

/// A phone's WebGL strings in the form ANGLE answers them.
///
/// Chromium on Android renders WebGL through ANGLE and nothing else: with the
/// passthrough command decoder — the default — `GetAllowedGLImplementations()`
/// in `ui/gl/init/gl_factory_android.cc` offers ANGLE's GLES and Vulkan backends
/// and no direct GLES2, and ANGLE's `Context::initRendererString` always builds
/// `ANGLE (vendor, renderer, version)`. So a handset never answers a bare
/// `Qualcomm` / `Adreno (TM) 619`; that is the shape `glGetString` takes when
/// the page talks to the driver itself.
///
/// 16 of the bundle's 50 Android templates carry the bare pair, and five of the
/// GPU models they name — Mali-G57, Mali-G77 MC9, Mali-G68, Adreno 619, Adreno
/// 750 — appear in the same library in ANGLE form under the same Chrome
/// version, which no two devices of one model can do. Those 16 predate ANGLE's
/// Android default, and the engine passes whatever string a profile names
/// straight through to `UNMASKED_VENDOR_WEBGL`, so the pair reaches the page.
///
/// Only a bare pair is rewritten. A string already naming ANGLE, one from
/// another vendor, or a GPU this cannot place is left exactly as it is.
fn fix_phone_webgl_angle(cfg: &mut serde_json::Map<String, serde_json::Value>) -> bool {
    if !crate::space::profile::claims_mobile(cfg) {
        return false;
    }
    let Some(webgl) = cfg.get_mut("webgl").and_then(|v| v.as_object_mut()) else {
        return false;
    };
    let Some(renderer) = webgl.get("renderer").and_then(|v| v.as_str()).map(str::to_string) else {
        return false;
    };
    let renderer = renderer.trim();
    if renderer.is_empty() || renderer.starts_with("ANGLE (") {
        return false;
    }
    // The backend vendor is the GPU's family, which is the name ANGLE puts in
    // the first slot. A renderer it cannot place is left alone rather than
    // guessed at.
    let family = if renderer.starts_with("Mali") {
        "ARM"
    } else if renderer.starts_with("Adreno") {
        "Qualcomm"
    } else {
        return false;
    };
    webgl.insert(
        "vendor".into(),
        serde_json::Value::String(format!("Google Inc. ({family})")),
    );
    webgl.insert(
        "renderer".into(),
        serde_json::Value::String(format!("ANGLE ({family}, {renderer}, OpenGL ES 3.2)")),
    );
    true
}

#[cfg(test)]
mod phone_webgl_angle_tests {
    use super::fix_phone_webgl_angle;
    use serde_json::json;

    fn phone(vendor: &str, renderer: &str) -> serde_json::Map<String, serde_json::Value> {
        json!({
            "navigator": { "platform": "Android" },
            "client_hints": { "mobile": true, "platform": "Android" },
            "webgl": { "vendor": vendor, "renderer": renderer, "extensions": ["EXT_sRGB"] },
        })
        .as_object()
        .unwrap()
        .clone()
    }

    #[test]
    fn a_bare_qualcomm_pair_is_given_angles_shape() {
        let mut p = phone("Qualcomm", "Adreno (TM) 619");
        assert!(fix_phone_webgl_angle(&mut p));
        assert_eq!(p["webgl"]["vendor"], "Google Inc. (Qualcomm)");
        assert_eq!(
            p["webgl"]["renderer"],
            "ANGLE (Qualcomm, Adreno (TM) 619, OpenGL ES 3.2)"
        );
        // The rest of the block is the profile's own.
        assert_eq!(p["webgl"]["extensions"][0], "EXT_sRGB");
        assert!(!fix_phone_webgl_angle(&mut p), "once");
    }

    #[test]
    fn a_bare_mali_pair_is_given_angles_shape() {
        let mut p = phone("ARM", "Mali-G57");
        assert!(fix_phone_webgl_angle(&mut p));
        assert_eq!(p["webgl"]["vendor"], "Google Inc. (ARM)");
        assert_eq!(
            p["webgl"]["renderer"],
            "ANGLE (ARM, Mali-G57, OpenGL ES 3.2)"
        );
    }

    #[test]
    fn a_pair_already_naming_angle_is_kept() {
        let mut p = phone(
            "Google Inc. (Qualcomm)",
            "ANGLE (Qualcomm, Adreno (TM) 750, OpenGL ES 3.2)",
        );
        let before = p.clone();
        assert!(!fix_phone_webgl_angle(&mut p));
        assert_eq!(p, before);
    }

    #[test]
    fn a_gpu_that_cannot_be_placed_is_left_alone() {
        // Apple and NVIDIA handset strings are not a thing, and inventing an
        // ANGLE prefix for one would be a claim this cannot back.
        let mut p = phone("Apple", "Apple GPU");
        let before = p.clone();
        assert!(!fix_phone_webgl_angle(&mut p));
        assert_eq!(p, before);
    }

    #[test]
    fn a_desktop_profile_keeps_its_bare_string() {
        // The bare shape is only wrong where ANGLE is the only backend, which
        // is Android. A desktop that names one is not this fix's to touch.
        let mut p = json!({
            "navigator": { "platform": "Linux" },
            "client_hints": { "mobile": false },
            "webgl": { "vendor": "X.Org", "renderer": "AMD CEDAR (DRM 2.49.0)" },
        })
        .as_object()
        .unwrap()
        .clone();
        let before = p.clone();
        assert!(!fix_phone_webgl_angle(&mut p));
        assert_eq!(p, before);
    }

    #[test]
    fn a_phone_with_no_webgl_block_is_left_alone() {
        let mut p = json!({ "client_hints": { "mobile": true } })
            .as_object()
            .unwrap()
            .clone();
        assert!(!fix_phone_webgl_angle(&mut p));
    }
}

/// A handset's screen has no window chrome, so its available area is the whole
/// screen.
///
/// `screen.availWidth`/`availHeight` come from `Screen::GetRect(available=true)`,
/// which answers `screen_info.available_rect`; `DisplayUtil::DisplayToScreenInfo`
/// fills that from the display's work area. On Android the work area is only
/// ever the display's real one behind `kAndroidUseCorrectDisplayWorkArea` —
/// `DisplayAndroidManager::DoUpdateDisplay` otherwise calls
/// `set_work_area(bounds)` — and `ui/android/ui_android_features.cc` declares
/// that feature `FEATURE_DISABLED_BY_DEFAULT`. So Chrome on Android answers
/// `availHeight == height`, `availTop == 0`, `availLeft == 0` on every device,
/// whatever the status bar is doing.
///
/// 8 of the bundle's 50 Android templates carry a desktop-shaped inset instead
/// (avail_height 37..84 px short, avail_top 32..40), and the engine passes the
/// pair through unchanged. The 42 that claim the full screen are already right
/// and are left exactly as they are; so is a field the template does not carry,
/// which is the engine's own default rather than a claim.
fn fix_phone_screen_avail(cfg: &mut serde_json::Map<String, serde_json::Value>) -> bool {
    if !crate::space::profile::claims_mobile(cfg) {
        return false;
    }
    let Some(scr) = cfg.get_mut("screen").and_then(|v| v.as_object_mut()) else {
        return false;
    };
    let (Some(w), Some(h)) = (
        scr.get("width").and_then(|v| v.as_i64()),
        scr.get("height").and_then(|v| v.as_i64()),
    ) else {
        return false;
    };
    if w <= 0 || h <= 0 {
        return false;
    }
    let mut changed = false;
    for (key, want) in [
        ("avail_width", w),
        ("avail_height", h),
        ("avail_top", 0),
        ("avail_left", 0),
    ] {
        match scr.get(key).and_then(|v| v.as_i64()) {
            None => continue,
            Some(got) if got == want => continue,
            Some(_) => {}
        }
        scr.insert(key.into(), serde_json::Value::from(want));
        changed = true;
    }
    changed
}

/// The least a phone's touchscreen reports.
///
/// `navigator.maxTouchPoints` is the digitizer's maximum, and every handset
/// Chrome runs on reports five or more — five is the platform's own baseline.
/// One of the bundle's 50 Android templates (`android-realme-c63`) carries 2,
/// and the engine passes the value through verbatim, so the profile answers a
/// two-finger touchscreen: a device no Android phone has been in a decade.
const PHONE_MIN_TOUCH_POINTS: i64 = 5;

fn fix_phone_touch_points(cfg: &mut serde_json::Map<String, serde_json::Value>) -> bool {
    if !crate::space::profile::claims_mobile(cfg) {
        return false;
    }
    let Some(nav) = cfg.get_mut("navigator").and_then(|v| v.as_object_mut()) else {
        return false;
    };
    let Some(got) = nav.get("max_touch_points").and_then(|v| v.as_i64()) else {
        return false;
    };
    if got >= PHONE_MIN_TOUCH_POINTS {
        return false;
    }
    nav.insert(
        "max_touch_points".into(),
        serde_json::Value::from(PHONE_MIN_TOUCH_POINTS),
    );
    true
}

#[cfg(test)]
mod phone_screen_avail_tests {
    use super::{fix_phone_screen_avail, fix_phone_touch_points};
    use serde_json::{json, Value};

    fn phone(screen: Value) -> serde_json::Map<String, Value> {
        json!({
            "navigator": { "platform": "Android", "max_touch_points": 5 },
            "client_hints": { "mobile": true, "platform": "Android" },
            "screen": screen,
        })
        .as_object()
        .unwrap()
        .clone()
    }

    fn full() -> Value {
        json!({ "width": 360, "height": 800, "avail_width": 360, "avail_height": 800,
                "avail_top": 0, "avail_left": 0, "device_pixel_ratio": 2.0 })
    }

    #[test]
    fn a_status_bar_inset_is_removed() {
        // android-samsung-galaxy-s24-ultra, as it shipped.
        let mut p = phone(json!({ "width": 412, "height": 892, "avail_width": 412,
            "avail_height": 808, "avail_top": 36, "avail_left": 0,
            "device_pixel_ratio": 2.625 }));
        assert!(fix_phone_screen_avail(&mut p));
        assert_eq!(p["screen"]["avail_width"], 412);
        assert_eq!(p["screen"]["avail_height"], 892);
        assert_eq!(p["screen"]["avail_top"], 0);
        assert_eq!(p["screen"]["avail_left"], 0);
        // Everything else is the template's own.
        assert_eq!(p["screen"]["device_pixel_ratio"], 2.625);
        assert!(!fix_phone_screen_avail(&mut p), "once");
    }

    #[test]
    fn a_phone_that_already_claims_the_whole_screen_is_untouched() {
        let mut p = phone(full());
        let before = p.clone();
        assert!(!fix_phone_screen_avail(&mut p));
        assert_eq!(p, before);
    }

    #[test]
    fn a_field_the_template_does_not_carry_stays_absent() {
        let mut p = phone(json!({ "width": 360, "height": 800 }));
        assert!(!fix_phone_screen_avail(&mut p));
        assert!(p["screen"].get("avail_height").is_none());
    }

    #[test]
    fn a_desktop_work_area_is_left_alone() {
        // A Mac or Windows template's avail_* is its menu bar and taskbar, and
        // the browser really does report the inset there.
        let mut p = json!({
            "navigator": { "platform": "macOS" },
            "client_hints": { "mobile": false, "platform": "macOS" },
            "screen": { "width": 1440, "height": 900, "avail_width": 1440,
                        "avail_height": 875, "avail_top": 25, "avail_left": 0 },
        })
        .as_object()
        .unwrap()
        .clone();
        let before = p.clone();
        assert!(!fix_phone_screen_avail(&mut p));
        assert_eq!(p, before);
    }

    #[test]
    fn a_two_point_touchscreen_is_raised_to_five() {
        // android-realme-c63, as it shipped.
        let mut p = json!({
            "navigator": { "platform": "Android", "max_touch_points": 2 },
            "client_hints": { "mobile": true, "platform": "Android" },
        })
        .as_object()
        .unwrap()
        .clone();
        assert!(fix_phone_touch_points(&mut p));
        assert_eq!(p["navigator"]["max_touch_points"], 5);
        assert!(!fix_phone_touch_points(&mut p), "once");
    }

    #[test]
    fn a_ten_point_touchscreen_is_kept() {
        let mut p = json!({
            "navigator": { "platform": "Android", "max_touch_points": 10 },
            "client_hints": { "mobile": true, "platform": "Android" },
        })
        .as_object()
        .unwrap()
        .clone();
        let before = p.clone();
        assert!(!fix_phone_touch_points(&mut p));
        assert_eq!(p, before);
    }

    #[test]
    fn a_desktop_and_a_missing_value_are_left_alone() {
        let mut desktop = json!({
            "navigator": { "platform": "Windows", "max_touch_points": 2 },
            "client_hints": { "mobile": false, "platform": "Windows" },
        })
        .as_object()
        .unwrap()
        .clone();
        let before = desktop.clone();
        assert!(!fix_phone_touch_points(&mut desktop));
        assert_eq!(desktop, before);

        let mut bare = json!({
            "navigator": { "platform": "Android" },
            "client_hints": { "mobile": true, "platform": "Android" },
        })
        .as_object()
        .unwrap()
        .clone();
        assert!(!fix_phone_touch_points(&mut bare));
        assert!(bare["navigator"].get("max_touch_points").is_none());
    }
}

#[cfg(test)]
mod chrome_brand_tests {
    use super::add_chrome_brand;
    use serde_json::json;

    #[test]
    fn a_phone_profile_without_a_brand_gets_chromes() {
        let mut cfg = json!({ "navigator": { "platform": "Android" },
            "client_hints": { "mobile": true, "brand_version": "156" } });
        assert!(add_chrome_brand(&mut cfg));
        assert_eq!(cfg["client_hints"]["brand"], "Google Chrome");
        assert!(!add_chrome_brand(&mut cfg), "once");
    }

    #[test]
    fn a_named_brand_and_a_desktop_profile_are_left_alone() {
        let mut named = json!({ "client_hints": { "mobile": true, "brand": "Microsoft Edge" } });
        let mut desktop = json!({ "navigator": { "user_agent": "Mozilla/5.0 (Windows NT 10.0; Win64; x64)" },
            "client_hints": { "mobile": false } });
        let (a, b) = (named.clone(), desktop.clone());
        assert!(!add_chrome_brand(&mut named));
        assert!(!add_chrome_brand(&mut desktop));
        assert_eq!((named, desktop), (a, b));
    }
}

#[cfg(test)]
mod apple_webgpu_tests {
    use super::{add_apple_webgpu_features, migrate_dir_to};
    use serde_json::json;

    const M1: &str = "ANGLE (Apple, ANGLE Metal Renderer: Apple M1, Unspecified Version)";

    fn mac(renderer: &str, vendor: &str, features: serde_json::Value) -> serde_json::Value {
        json!({
            "name": "mac-m1-air13",
            "navigator": { "platform": "macOS" },
            "webgl": { "renderer": renderer, "extensions": ["ANGLE_instanced_arrays"] },
            "webgpu": { "vendor": vendor, "architecture": "metal-3", "limits": { "maxBindGroups": 4 }, "features": features },
        })
    }

    #[test]
    fn an_apple_m_list_gains_the_feature_in_place_and_only_once() {
        let mut cfg = mac(M1, "apple", json!(["shader-f16", "texture-compression-etc2", "texture-formats-tier1", "timestamp-query"]));
        let before = cfg.clone();
        assert!(add_apple_webgpu_features(&mut cfg));
        assert_eq!(
            cfg["webgpu"]["features"],
            json!(["shader-f16", "texture-compression-etc2", "texture-compression-unaligned", "texture-formats-tier1", "timestamp-query"])
        );
        let mut rest = cfg.clone();
        rest["webgpu"]["features"] = before["webgpu"]["features"].clone();
        assert_eq!(rest, before, "only webgpu.features may change");

        let once = cfg.clone();
        assert!(!add_apple_webgpu_features(&mut cfg), "a second pass must change nothing");
        assert_eq!(cfg, once);
    }

    #[test]
    fn other_gpus_and_host_lists_are_left_alone() {
        let feats = json!(["shader-f16", "texture-compression-bc"]);
        let intel_mac = mac("ANGLE (Intel, ANGLE Metal Renderer: Intel(R) UHD Graphics 630, Unspecified Version)", "intel", feats.clone());
        let windows = json!({ "webgl": { "renderer": "ANGLE (NVIDIA, NVIDIA GeForce RTX 3070 Direct3D11 vs_5_0 ps_5_0, D3D11)" }, "webgpu": { "vendor": "nvidia", "features": feats.clone() } });
        let mismatched = mac(M1, "intel", feats.clone());
        let apple_gpu_not_m = mac("ANGLE (Apple, ANGLE Metal Renderer: Apple GPU, Unspecified Version)", "apple", feats.clone());
        let no_list = json!({ "webgl": { "renderer": M1 }, "webgpu": { "vendor": "apple" } });
        let empty_list = mac(M1, "apple", json!([]));
        for mut cfg in [intel_mac, windows, mismatched, apple_gpu_not_m, no_list, empty_list] {
            let before = cfg.clone();
            assert!(!add_apple_webgpu_features(&mut cfg));
            assert_eq!(cfg, before);
        }
    }

    #[test]
    fn the_migration_writes_it_once() {
        let dir = std::env::temp_dir().join(format!("shardx-apple-webgpu-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("p.json");
        std::fs::write(&file, mac(M1, "apple", json!(["texture-compression-etc2", "timestamp-query"])).to_string()).unwrap();

        assert_eq!(migrate_dir_to(&dir, "156.0.8078.48", None, None, None).unwrap(), 1);
        let got: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
        assert_eq!(got["webgpu"]["features"], json!(["texture-compression-etc2", "texture-compression-unaligned", "timestamp-query"]));
        assert_eq!(migrate_dir_to(&dir, "156.0.8078.48", None, None, None).unwrap(), 0, "a second run must write nothing");
        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[cfg(test)]
mod android_grease_tests {
    use super::migrate_dir_to;
    use serde_json::json;

    /// The bundle's Android templates, in the shape they ship: brand present,
    /// no GREASE at all, and the phone-only fields that must survive.
    fn phone() -> serde_json::Value {
        json!({
            "name": "android-samsung-galaxy-s24-ultra",
            "navigator": {
                "user_agent": "Mozilla/5.0 (Linux; Android 10; K) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/156.0.0.0 Mobile Safari/537.36",
                "platform": "Android", "platform_value": "Linux armv81", "max_touch_points": 5,
            },
            "client_hints": {
                "brand": "Google Chrome", "brand_version": "156", "brand_full_version": "156.0.8078.48",
                "platform_version": "16.0.0", "model": "SM-S928B", "architecture": "", "bitness": "", "mobile": true,
            },
            "screen": { "width": 412, "height": 892, "device_pixel_ratio": 2.625 },
        })
    }

    #[test]
    fn a_phone_template_gains_grease_and_keeps_its_own_fields() {
        // Without GREASE the engine drops "Google Chrome" from userAgentData.brands,
        // leaving a two-entry list no Chrome reports.
        let dir = std::env::temp_dir().join(format!("shardx-android-grease-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("android-samsung-galaxy-s24-ultra.json");
        std::fs::write(&file, phone().to_string()).unwrap();

        let n = migrate_dir_to(&dir, "156.0.8078.48", Some("Not:A-Brand"), Some("8"), None).unwrap();
        assert_eq!(n, 1);

        let got: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
        let ch = &got["client_hints"];
        assert_eq!(ch["grease_brand"], "Not:A-Brand");
        assert_eq!(ch["grease_version"], "8");
        assert_eq!(ch["grease_full_version"], "8.0.0.0");
        // The phone's own fields are untouched — an empty architecture stays empty,
        // because Chromium's Android branch really does report one.
        assert_eq!(ch["brand"], "Google Chrome");
        assert_eq!(ch["model"], "SM-S928B");
        assert_eq!(ch["architecture"], "");
        assert_eq!(ch["bitness"], "");
        assert_eq!(ch["platform_version"], "16.0.0");
        assert_eq!(got["navigator"]["max_touch_points"], 5);
        assert_eq!(got["screen"]["device_pixel_ratio"], 2.625);

        assert_eq!(
            migrate_dir_to(&dir, "156.0.8078.48", Some("Not:A-Brand"), Some("8"), None).unwrap(),
            0,
            "a second run must write nothing"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_manifest_without_grease_leaves_the_list_alone() {
        // GREASE rotates per release and travels as data; absent means the
        // manifest did not say, so nothing is invented.
        let dir = std::env::temp_dir().join(format!("shardx-android-nogrease-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("android-samsung-galaxy-s24-ultra.json");
        std::fs::write(&file, phone().to_string()).unwrap();

        migrate_dir_to(&dir, "156.0.8078.48", None, None, None).unwrap();
        let got: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
        assert!(got["client_hints"].get("grease_brand").is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }
}

/// A handset's `Sec-CH-UA-Platform-Version` is always `N.0.0` — Chromium reports
/// the release number alone, never a point release. A phone carrying anything
/// else was built while `randomize_platform_version` had no Android pool and
/// every platform fell through to the template's own value, which for a phone
/// preset taken from a Mac template was a macOS version ("15.6.1"). Such a
/// profile answers `platform: Android` with an iOS-shaped version, a pair no
/// handset reports, and the engine passes it through unchanged.
fn android_shaped_version(v: &str) -> bool {
    let mut parts = v.split('.');
    let major = parts.next().and_then(|s| s.parse::<u32>().ok());
    matches!((major, parts.next(), parts.next(), parts.next()),
             (Some(m), Some("0"), Some("0"), None) if m >= 10)
}

/// Correct a phone's platform version to its template's own. The version is not
/// an operator's choice — nothing in the form edits it, and `bbee2a5` made a new
/// phone draw an Android release — so a phone carrying a desktop-shaped one is a
/// profile from before that fix, and is answered from its template. A version
/// already Android-shaped is left alone: it may be one this profile drew.
fn fix_phone_platform_version(
    cfg: &mut serde_json::Map<String, serde_json::Value>,
    src: &serde_json::Value,
) -> bool {
    if !crate::space::profile::claims_mobile(cfg) {
        return false;
    }
    let Some(want) = src
        .pointer("/client_hints/platform_version")
        .and_then(|v| v.as_str())
        .filter(|s| android_shaped_version(s))
    else {
        return false;
    };
    let got = cfg
        .get("client_hints")
        .and_then(|c| c.get("platform_version"))
        .and_then(|v| v.as_str());
    if got.is_some_and(android_shaped_version) {
        return false;
    }
    if let Some(ch) = cfg.get_mut("client_hints").and_then(|v| v.as_object_mut()) {
        ch.insert("platform_version".into(), serde_json::Value::from(want));
    }
    if let Some(nav) = cfg.get_mut("navigator").and_then(|v| v.as_object_mut()) {
        if nav.contains_key("platform_version") {
            nav.insert("platform_version".into(), serde_json::Value::from(want));
        }
    }
    true
}

/// Fields a profile was created before the library had. A profile keeps its own
/// copy of the fingerprint, so updating the library never reaches it; these are
/// copied across by name, and only where the profile carries none of its own. A
/// fingerprint the operator brought in themselves has no library entry under
/// that name and is left exactly as it is.
fn adopt_library_fields(dir: &Path) -> Result<usize> {
    let mut lib: std::collections::HashMap<String, serde_json::Value> =
        std::collections::HashMap::new();
    for e in crate::fingerprints::list_all().unwrap_or_default() {
        // By library id first: it survives the operator renaming the profile,
        // which the name does not.
        lib.insert(e.id, e.payload.clone());
        lib.entry(e.label).or_insert(e.payload);
    }
    if lib.is_empty() {
        return Ok(0);
    }
    let mut n = 0usize;
    for ent in fs::read_dir(dir)?.flatten() {
        let p = ent.path();
        if p.extension().and_then(|s| s.to_str()) != Some("json") {
            continue;
        }
        let Ok(text) = fs::read_to_string(&p) else { continue };
        let Ok(mut cfg) = serde_json::from_str::<serde_json::Value>(&text) else { continue };
        if !adopt_into(&mut cfg, &lib) {
            continue;
        }
        if fs::write(&p, serde_json::to_string_pretty(&cfg).unwrap_or(text)).is_ok() {
            n += 1;
        }
    }
    Ok(n)
}

/// The per-profile half of `adopt_library_fields`, so a test can hand it a
/// library of its own instead of the machine's. Answers whether it changed
/// anything.
fn adopt_into(
    cfg: &mut serde_json::Value,
    lib: &std::collections::HashMap<String, serde_json::Value>,
) -> bool {
    let key = cfg
        .pointer("/_meta/gpu_preset_id")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .or_else(|| cfg.get("name").and_then(|v| v.as_str()))
        .map(String::from);
    let Some(src) = key.as_deref().and_then(|k| lib.get(k)) else { return false };
    let mut changed = false;
    for key in ["fonts", "fonts_local"] {
        if cfg.get(key).is_none() {
            if let Some(v) = src.get(key) {
                cfg[key] = v.clone();
                changed = true;
            }
        }
    }
    // features live inside the webgpu block the profile already has; a
    // profile without that block claims a machine with no adapter.
    if cfg.pointer("/webgpu/features").is_none() {
        if let Some(f) = src.pointer("/webgpu/features") {
            if let Some(w) = cfg.get_mut("webgpu").and_then(|v| v.as_object_mut()) {
                w.insert("features".into(), f.clone());
                changed = true;
            }
        }
    }
    // A phone keeps the version it was built with; one drawn before the
    // Android pool existed is the template's, which may be a Mac's.
    if let Some(obj) = cfg.as_object_mut() {
        changed |= fix_phone_platform_version(obj, src);
        // And one drawn from a template from before ANGLE was Android's only
        // WebGL backend carries the bare driver pair the engine would hand to
        // the page.
        changed |= fix_phone_webgl_angle(obj);
        // A saved profile carries its template's screen and touchscreen values,
        // so a profile from a defective template needs the same correction.
        changed |= fix_phone_screen_avail(obj);
        changed |= fix_phone_touch_points(obj);
    }
    changed
}

#[cfg(test)]
mod phone_platform_version_tests {
    use super::{adopt_into, android_shaped_version};
    use serde_json::json;
    use std::collections::HashMap;

    fn lib() -> HashMap<String, serde_json::Value> {
        let mut m = HashMap::new();
        m.insert(
            "android-vivo-y19s-5g".to_string(),
            json!({
                "name": "android-vivo-y19s-5g",
                "navigator": { "platform": "Android", "platform_value": "Linux armv81" },
                "client_hints": { "mobile": true, "platform": "Android", "platform_version": "16.0.0" },
            }),
        );
        m.insert(
            "mac-m3-air13".to_string(),
            json!({
                "name": "mac-m3-air13",
                "navigator": { "platform": "macOS", "platform_version": "15.6.1" },
                "client_hints": { "mobile": false, "platform": "macOS", "platform_version": "15.6.1" },
            }),
        );
        m
    }

    fn phone(preset: &str, pv: &str) -> serde_json::Value {
        json!({
            "name": "shop phone",
            "_meta": { "gpu_preset_id": preset },
            "navigator": { "platform": "Android", "platform_version": pv },
            "client_hints": { "mobile": true, "platform": "Android", "platform_version": pv },
        })
    }

    #[test]
    fn only_a_release_shaped_version_is_android() {
        for v in ["13.0.0", "14.0.0", "15.0.0", "16.0.0"] {
            assert!(android_shaped_version(v), "{v} is what a handset reports");
        }
        // The macOS pool, and an Android point release Chromium never reports.
        for v in ["15.6.1", "14.7.2", "26.1", "15.4", "15.1.0"] {
            assert!(!android_shaped_version(v), "{v} is no Android release");
        }
    }

    #[test]
    fn a_phone_keeps_the_version_it_drew() {
        // It may be one this profile drew, and the template's is only one of
        // the several an Android release pool offers.
        let mut cfg = phone("android-vivo-y19s-5g", "14.0.0");
        assert!(!adopt_into(&mut cfg, &lib()));
        assert_eq!(cfg["client_hints"]["platform_version"], "14.0.0");
    }

    #[test]
    fn a_phone_from_before_the_android_pool_gains_its_template_version() {
        // 15.6.1 is the macOS pool's, so this phone answers an iOS-shaped
        // version — the pair no handset reports, and the one the engine
        // passes through untouched.
        let mut cfg = phone("android-vivo-y19s-5g", "15.6.1");
        assert!(adopt_into(&mut cfg, &lib()));
        assert_eq!(cfg["client_hints"]["platform_version"], "16.0.0");
        assert_eq!(cfg["navigator"]["platform_version"], "16.0.0");
        // Everything else the profile owns is untouched.
        assert_eq!(cfg["_meta"]["gpu_preset_id"], "android-vivo-y19s-5g");
        assert_eq!(cfg["name"], "shop phone");
    }

    #[test]
    fn a_desktop_profile_keeps_its_version() {
        // 15.6.1 is a real macOS release, so it is right where it stands.
        let mut cfg = json!({
            "name": "desk",
            "_meta": { "gpu_preset_id": "mac-m3-air13" },
            "navigator": { "platform": "macOS", "platform_version": "15.6.1" },
            "client_hints": { "mobile": false, "platform": "macOS", "platform_version": "15.6.1" },
        });
        assert!(!adopt_into(&mut cfg, &lib()));
        assert_eq!(cfg["client_hints"]["platform_version"], "15.6.1");
    }

    #[test]
    fn a_phone_with_no_library_entry_is_left_alone() {
        // The operator's own capture: no template says what its version is.
        let mut cfg = phone("my own capture", "15.6.1");
        assert!(!adopt_into(&mut cfg, &lib()));
        assert_eq!(cfg["client_hints"]["platform_version"], "15.6.1");
    }
}

#[cfg(test)]
mod adopt_tests {
    use serde_json::json;

    /// The rule the copy must keep: a profile answers to a library entry by id,
    /// and a fingerprint the operator brought in themselves answers to nothing.
    fn pick<'a>(
        cfg: &serde_json::Value,
        lib: &'a std::collections::HashMap<String, serde_json::Value>,
    ) -> Option<&'a serde_json::Value> {
        cfg.pointer("/_meta/gpu_preset_id")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .or_else(|| cfg.get("name").and_then(|v| v.as_str()))
            .and_then(|k| lib.get(k))
    }

    #[test]
    fn adopt_matches_by_preset_id_then_name() {
        let mut lib = std::collections::HashMap::new();
        lib.insert("android-oppo-a6-5g".to_string(), json!({"fonts": [["sans-serif"]]}));
        let renamed = json!({"name": "shop 4", "_meta": {"gpu_preset_id": "android-oppo-a6-5g"}});
        assert!(pick(&renamed, &lib).is_some(), "a renamed profile still matches by id");
        let by_name = json!({"name": "android-oppo-a6-5g"});
        assert!(pick(&by_name, &lib).is_some(), "an older profile matches by name");
    }

    #[test]
    fn adopt_leaves_a_profile_with_no_library_entry_alone() {
        let lib = std::collections::HashMap::new();
        let own = json!({"name": "my own capture", "_meta": {"gpu_preset_id": ""}});
        assert!(pick(&own, &lib).is_none());
    }
}

/// Migrate both the saved profiles AND the fingerprint library (bundled +
/// user-added) to `chromium_version`. Bundled templates are already at the new
/// version after the seed; user-added fingerprints get their UA + client_hints
/// bumped here (their custom fields are preserved).
fn migrate_all_to(
    chromium_version: &str,
    grease_brand: Option<&str>,
    grease_version: Option<&str>,
    tls: Option<&serde_json::Value>,
) -> usize {
    let mut n = 0;
    if let Ok(d) = crate::store::profiles_dir() {
        n += migrate_dir_to(&d, chromium_version, grease_brand, grease_version, tls).unwrap_or(0);
        n += adopt_library_fields(&d).unwrap_or(0);
    }
    if let Ok(d) = crate::store::fingerprints_dir() {
        n += migrate_dir_to(&d, chromium_version, grease_brand, grease_version, tls).unwrap_or(0);
    }
    n
}

/// Raised when `migrate_dir_to` learns a new fix, so a launcher update reruns it once
/// even on a manifest it has already applied.
const MIGRATION_REV: u32 = 7;

/// What a migration was applied for; the install path and the start share it.
///
/// The fingerprint archive's etag belongs in here: `install_fingerprints` copies the
/// archive verbatim, so a re-seed overwrites migrated templates with whatever the
/// bundle ships. The bundle's 50 Android templates carry no GREASE at all, and a
/// phone without it loses "Google Chrome" from `userAgentData.brands` — a brand
/// list only "Chromium" and the GREASE placeholder answers, which no Chrome does.
/// Without the etag in the signature that re-seed left the library unmigrated until
/// some unrelated manifest field happened to change.
fn migration_signature(target: &str, m: &RemoteManifest) -> String {
    format!(
        "{target}|{}|{}|{}|{}|{}|m{MIGRATION_REV}",
        m.grease_brand.as_deref().unwrap_or(""),
        m.grease_version.as_deref().unwrap_or(""),
        m.tls.as_ref().map(|t| t.to_string()).unwrap_or_default(),
        m.engine_build.as_deref().unwrap_or(""),
        m.archives.get(FINGERPRINTS_ARCHIVE_KEY).map(|s| s.as_str()).unwrap_or(""),
    )
}

/// Startup hook: migrate saved profiles + the fingerprint library (bundled +
/// user-added) to the manifest's engine descriptor when not already done. One
/// GitHub-manifest GET (never S3). Guarded by `migration_signature`, so a change
/// to any manifest field it covers, or to `MIGRATION_REV`, re-triggers migration
/// even for users already on the current version. Also covers users whose
/// engine auto-updated via the etag path without an explicit install.
pub async fn ensure_profiles_migrated() {
    let m = fetch_manifest().await;
    let Some(target) = m.chromium_version.clone() else { return };
    // TLS is part of the signature: without it a manifest that changes only
    // the ClientHello shape would be a no-op for anyone already on this
    // version, and their profiles would keep the previous release's JA4.
    // engine_build is in the signature too: a release that keeps the Chromium
    // version but changes what the engine does would otherwise migrate nobody.
    let sig = migration_signature(&target, &m);
    let mut local = load_manifest();
    if local.applied_signature.as_deref() == Some(sig.as_str()) {
        return;
    }
    // An engine still on another version (an update not in yet, or one that
    // failed): its profiles keep its version; the install migrates after the swap.
    let on_disk = binary_path().is_ok_and(|p| p.exists());
    if on_disk && effective_installed_version(&local).is_some_and(|v| v != target) {
        return;
    }
    let n = migrate_all_to(
        &target,
        m.grease_brand.as_deref(),
        m.grease_version.as_deref(),
        m.tls.as_ref(),
    );
    if n > 0 {
        eprintln!("[runtime] migrated {n} profile/fingerprint file(s) to {sig}");
    }
    local.applied_chromium_version = Some(target);
    local.applied_signature = Some(sig);
    let _ = save_manifest(&local);
}

/// Dotted-numeric comparison. Non-numeric parts compare as 0, which is what
/// makes "2.0.2-rc1" sort with "2.0.2" rather than below every release.
fn version_lt(a: &str, b: &str) -> bool {
    let part = |s: &str| -> Vec<u32> {
        s.split(['.', '-', '+'])
            .map(|p| p.parse::<u32>().unwrap_or(0))
            .collect()
    };
    let (va, vb) = (part(a), part(b));
    for i in 0..va.len().max(vb.len()) {
        let (x, y) = (va.get(i).copied().unwrap_or(0), vb.get(i).copied().unwrap_or(0));
        if x != y {
            return x < y;
        }
    }
    false
}

/// This launcher's own version, as published.
fn launcher_version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

/// Whether this launcher is new enough for the engine build the manifest
/// describes.
fn engine_needs_newer_launcher(m: &RemoteManifest) -> bool {
    m.min_launcher_version
        .as_deref()
        .is_some_and(|min| version_lt(launcher_version(), min))
}

/// Whether the published engine differs from the one on disk: Chromium version,
/// `engine_build`, or the archive hash. Any one of them is enough, and an
/// unknown local value is not a mismatch.
fn engine_outdated(local: &Manifest, remote: &RemoteManifest, browser_key: &str) -> bool {
    let version_moved = remote
        .chromium_version
        .as_deref()
        .is_some_and(|rv| effective_installed_version(local).as_deref() != Some(rv));
    let differs = |have: Option<String>, want: Option<&str>| match (have, want) {
        (Some(h), Some(w)) => !h.is_empty() && h != w,
        _ => false,
    };
    let build_moved = differs(installed_engine_build(local), remote.engine_build.as_deref());
    let hash_moved = differs(
        local.browser_etag.clone(),
        remote.archives.get(browser_key).map(|s| s.as_str()),
    );
    version_moved || build_moved || hash_moved
}

#[tauri::command]
pub async fn runtime_status() -> Result<RuntimeStatus, String> {
    let spec = host_spec();
    let layout = host_layout();
    // An update that could not put the previous engine back: tried again here,
    // off the async threads, unless an install (or the start's own try) has it.
    if let (Some(l), Ok(one)) = (layout.clone(), install_lock().try_lock()) {
        let _ = tokio::task::spawn_blocking(move || {
            let _one = one;
            recover_swap(&l);
        })
        .await;
    }
    let installed = binary_path().map(|p| p.exists()).unwrap_or(false);
    let mut m = load_manifest();
    let manifest = fetch_manifest().await;
    let remote = spec
        .as_ref()
        .and_then(|s| manifest.archives.get(&s.browser.key).cloned());
    // Manifest unreachable → everything reads None and we assume up to date.
    let needs_newer_launcher = engine_needs_newer_launcher(&manifest);

    // An engine installed before build stamps has none recorded, and unknown is
    // never a mismatch — so adopt one once, or the first bump never lands.
    if installed && manifest.engine_build.is_some() && installed_engine_build(&m).is_none() {
        let mut adopt = m.clone();
        adopt.installed_engine_build = manifest.engine_build.clone();
        if save_manifest(&adopt).is_ok() {
            m = adopt;
        }
    }

    let update_available = installed
        && !needs_newer_launcher
        && spec
            .as_ref()
            .is_some_and(|s| engine_outdated(&m, &manifest, &s.browser.key));
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
    // A failure matters while its update is still due or nothing is installed.
    let install_error = load_install_error().filter(|_| update_available || !installed);
    let update_staged = update_available
        && layout
            .as_ref()
            .and_then(swap::staged_note)
            .is_some_and(|s| remote.as_deref().is_none_or(|r| r == s.etag));

    Ok(RuntimeStatus {
        installed,
        binary_path: if installed { binary_path().ok() } else { None },
        installed_browser_etag: m.browser_etag,
        remote_browser_etag: remote,
        update_available,
        spec,
        fingerprints_installed,
        needs_newer_launcher,
        min_launcher_version: manifest.min_launcher_version,
        launcher_version: launcher_version().to_string(),
        install_error,
        update_staged,
    })
}

#[tauri::command]
pub async fn runtime_install(window: Window, force: bool) -> Result<RuntimeStatus, String> {
    let spec = host_spec().ok_or("Host platform has no published ShardBrowser engine archive")?;
    let base = runtime_dir().map_err(|e| e.to_string())?;
    fs::create_dir_all(&base).map_err(|e| e.to_string())?;
    let layout = swap::Layout::host(base, &spec.browser.key);

    let manifest = fetch_manifest().await;

    // Refused rather than installed: this build could not configure it.
    if engine_needs_newer_launcher(&manifest) {
        return Err(format!(
            "This engine build needs ShardBrowser Launcher {} or newer — you are on {}. Update the launcher first.",
            manifest.min_launcher_version.as_deref().unwrap_or("?"),
            launcher_version(),
        ));
    }

    let one = install_lock().lock().await;
    // The engine as it was, should an earlier update have been cut short, and
    // the record read after that.
    let l = layout.clone();
    let _ = blocking(move || recover_swap(&l)).await;
    let mut local = load_manifest();
    let installed_now = binary_path().map(|p| p.exists()).unwrap_or(false);
    // Manifest unreachable → don't force a re-download.
    let need_browser =
        force || !installed_now || engine_outdated(&local, &manifest, &spec.browser.key);
    let target_ver = manifest
        .chromium_version
        .clone()
        .unwrap_or_else(|| CHROMIUM_VERSION.to_string());
    if need_browser {
        match install_engine(&window, &spec.browser, &layout, &manifest, force).await {
            Ok(record) => {
                // Saved at once: the engine on disk is the new one, whatever fails next.
                apply_record(&mut local, &record);
                match save_manifest(&local) {
                    Ok(()) => swap::settle(&layout),
                    // The journal keeps it for the next start.
                    Err(e) => eprintln!("[runtime] the engine is in, but its record could not be saved yet: {e}"),
                }
                clear_install_error();
            }
            Err(f) => {
                drop(one);
                return engine_failed(&window, f, &manifest, &spec.browser.key, &layout).await;
            }
        }
    }
    // The previous tree goes in the background, and a staged one nothing needs
    // (not knowable offline); what stays (a Windows lock) is swept at the next start.
    let reachable = manifest.chromium_version.is_some() || !manifest.archives.is_empty();
    let stale = !need_browser && reachable && layout.staging().exists();
    if need_browser || stale {
        let l = layout.clone();
        tauri::async_runtime::spawn(async move {
            let _one = install_lock().lock().await;
            let _ = tokio::task::spawn_blocking(move || {
                if !swap::cleanup(&l, &swap::Ops::real(&l)) {
                    eprintln!("[runtime] an earlier engine could not be removed yet; it goes at the next start");
                }
            })
            .await;
        });
    }
    drop(one);

    // The CDM is no longer fetched here. The engine's own component updater
    // downloads it into the profile's user-data-dir, which is the only place
    // Chromium reads it from: BUNDLE_WIDEVINE_CDM is off in our build, so the
    // copy this used to place inside the bundle was never opened. The launcher
    // now keeps the first one an engine downloads and seeds the rest from it
    // (launch::harvest_widevine), which also means the CDN never has to carry
    // a CDM version again.
    let widevine_etag: Option<String> = local.widevine_etag.clone();

    // Fingerprint seed: overwrites bundled templates, leaves user-added files;
    // skipped when the etag matches. User-added FP get version-migrated below.
    let fp_remote = manifest.archives.get(FINGERPRINTS_ARCHIVE_KEY).map(|s| s.as_str());
    let fp_etag = install_fingerprints(&window, force, local.fingerprints_etag.as_deref(), fp_remote)
        .await
        .map_err(|e| e.to_string())?
        .or(local.fingerprints_etag.clone());

    // Migrate already-created profiles AND the fingerprint library (incl.
    // user-added) to the new engine descriptor (UA + client_hints incl. grease).
    // Runs only when the version-or-grease signature changed since last time.
    let sig = migration_signature(&target_ver, &manifest);
    if local.applied_signature.as_deref() != Some(sig.as_str()) {
        let n = migrate_all_to(
            &target_ver,
            manifest.grease_brand.as_deref(),
            manifest.grease_version.as_deref(),
            manifest.tls.as_ref(),
        );
        if n > 0 {
            eprintln!("[runtime] migrated {n} profile/fingerprint file(s) to {sig}");
        }
    }

    save_manifest(&Manifest {
        browser_etag: local.browser_etag.clone(),
        widevine_etag,
        fingerprints_etag: fp_etag,
        applied_chromium_version: Some(target_ver.clone()),
        applied_signature: Some(sig),
        // Recorded at the swap, never re-read off disk (a leftover
        // `<old>.manifest` once kept the version "stuck"); an engine from
        // before the record adopts the manifest's.
        installed_chromium_version: local.installed_chromium_version.clone().or(Some(target_ver)),
        installed_engine_build: local.installed_engine_build.clone(),
    })
    .map_err(|e| e.to_string())?;

    let _ = window.emit("runtime:done", ());
    runtime_status().await
}

/// The install stopped short. With the previous engine still there the window
/// opens on it and the reason rides in the status; with none, it is an error.
async fn engine_failed(
    window: &Window,
    f: swap::Failure,
    manifest: &RemoteManifest,
    key: &str,
    layout: &swap::Layout,
) -> Result<RuntimeStatus, String> {
    let kept = binary_path().is_ok_and(|p| p.exists());
    let err = InstallError {
        stage: f.stage.name().to_string(),
        message: f.message,
        running_browsers: f.browsers,
        starting: f.starting,
        engine_kept: kept,
        target: manifest.archives.get(key).cloned().or_else(|| manifest.chromium_version.clone()),
        at_ms: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_millis() as u64),
    };
    eprintln!("[runtime] engine install stopped at {}: {}", err.stage, err.message);
    save_install_error(&err);
    let etag = manifest.archives.get(key);
    let update_staged = swap::staged_note(layout).is_some_and(|s| etag.is_none_or(|e| *e == s.etag));
    let _ = window.emit("runtime:failed", Failed { err: &err, update_staged });
    if kept {
        runtime_status().await
    } else {
        Err(err.message)
    }
}

/// Runs `f` off the async threads; a step that panicked is a failure.
async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> T + Send + 'static,
) -> Result<T, swap::Failure> {
    tokio::task::spawn_blocking(f).await.map_err(|e| {
        swap::Failure::new(swap::Stage::Extract, format!("The engine install stopped: {e}"))
    })
}

/// Clears `SWAPPING` however the swap ends.
struct Swapping;

impl Swapping {
    fn on() -> Swapping {
        SWAPPING.store(true, Ordering::SeqCst);
        Swapping
    }
}

impl Drop for Swapping {
    fn drop(&mut self) {
        SWAPPING.store(false, Ordering::SeqCst);
    }
}

/// The engine archive downloaded (or kept from an earlier try), unpacked beside
/// the live engine, checked and swapped in (#122). Nothing installed is touched
/// before the swap, and the swap puts the previous engine back if it fails.
async fn install_engine(
    window: &Window,
    spec: &ArchiveSpec,
    layout: &swap::Layout,
    manifest: &RemoteManifest,
    force: bool,
) -> Result<swap::Record, swap::Failure> {
    let wanted = manifest.archives.get(&spec.key).cloned();
    // Kept from an earlier try only while it is what the manifest offers now.
    let fits = move |etag: &str| !force && wanted.as_deref().is_none_or(|w| w == etag);
    // A GPU probe under way finishes first; none starts until this is done.
    let _gate = engine_gate().write().await;
    let l = layout.clone();
    let kept = blocking(move || swap::staged(&l)).await.ok().flatten();
    let staged = match kept.filter(|s| fits(&s.etag)) {
        Some(s) => {
            progress(window, spec, "extract", 0, 0, 100);
            s
        }
        None => {
            // An unfinished or outdated tree goes before the download: its room counts.
            let l = layout.clone();
            blocking(move || swap::drop_staging(&l, &swap::Ops::real(&l))).await??;
            let l = layout.clone();
            let dl = match blocking(move || swap::downloaded(&l)).await.ok().flatten().filter(|d| fits(&d.etag)) {
                Some(d) => d,
                None => download_engine(window, spec, layout).await?,
            };
            progress(window, spec, "extract", dl.size, dl.size, 100);
            let l = layout.clone();
            blocking(move || swap::stage(&l, &dl, &swap::Ops::real(&l))).await??
        }
    };
    // The manifest's version, as profiles are migrated to it; the tree's own
    // marker when the manifest cannot be read.
    let record = swap::Record {
        etag: staged.etag.clone(),
        version: manifest
            .chromium_version
            .clone()
            .or(staged.version)
            .or_else(|| Some(CHROMIUM_VERSION.to_string())),
        build: manifest.engine_build.clone(),
    };
    // Launches are refused while the folder moves; one under way refuses the swap.
    let _swapping = Swapping::on();
    let (l, r) = (layout.clone(), record.clone());
    blocking(move || swap::swap(&l, &swap::Ops::real(&l), &r)).await??;
    Ok(record)
}

/// Streams the engine archive into `<key>.part` and keeps it as the download.
/// Refused before a byte is written when the disk cannot hold it unpacked too.
async fn download_engine(
    window: &Window,
    spec: &ArchiveSpec,
    layout: &swap::Layout,
) -> Result<swap::Downloaded, swap::Failure> {
    swap::drop_download(layout);
    let ops = swap::Ops::real(layout);
    let room = |total: u64| {
        let need = total.saturating_mul(1 + swap::UNPACK_GUESS).saturating_add(swap::SPACE_MARGIN);
        swap::room(&ops, &layout.base, need)
    };
    let etag = match fetch(window, spec, &layout.part(), room).await {
        Ok(etag) => etag,
        Err(e) => {
            let _ = fs::remove_file(layout.part());
            return Err(match e.downcast::<swap::Failure>() {
                Ok(f) => f,
                Err(e) => swap::Failure::new(
                    swap::Stage::Download,
                    format!("Downloading the engine failed ({e:#}). The engine you have was not touched."),
                ),
            });
        }
    };
    swap::keep_download(layout, &etag).map_err(|e| {
        swap::Failure::new(swap::Stage::Download, format!("Could not keep the downloaded engine: {e}"))
    })
}

fn progress(window: &Window, spec: &ArchiveSpec, phase: &str, received: u64, total: u64, percent: u64) {
    let _ = window.emit(
        "runtime:progress",
        serde_json::json!({
            "label": spec.label,
            "phase": phase,
            "received": received,
            "total": total,
            "percent": percent,
        }),
    );
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
    let zip = staging.join(format!("{}.part", spec.key));
    let etag = fetch(window, &spec, &zip, |_| Ok(())).await?;
    progress(window, &spec, "extract", 0, 0, 100);
    let (z, s) = (zip.clone(), staging.clone());
    tokio::task::spawn_blocking(move || swap::unzip(&z, &s)).await??;
    let _ = fs::remove_file(&zip);

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

/// Streams `spec` into `part`, emitting `runtime:progress`. `room` vets the
/// size the server announces before a byte is written. Returns the etag.
async fn fetch(
    window: &Window,
    spec: &ArchiveSpec,
    part: &Path,
    room: impl Fn(u64) -> Result<(), swap::Failure>,
) -> Result<String> {
    let url = format!("{PUB_BASE}/{}", spec.key);
    let mut resp = reqwest::Client::new().get(&url).send().await?.error_for_status()?;
    let total = resp.content_length().unwrap_or(0);
    if total > 0 {
        room(total)?;
    }
    let etag = resp
        .headers()
        .get("etag")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.trim_matches('"').to_string())
        .unwrap_or_default();

    let mut out = tokio::fs::File::create(part).await?;
    let mut received: u64 = 0;
    let mut last_pct: u64 = u64::MAX;
    while let Some(chunk) = resp.chunk().await? {
        out.write_all(&chunk).await?;
        received += chunk.len() as u64;
        // Emit once per integer percent.
        let pct = if total > 0 { received * 100 / total } else { 0 };
        if pct != last_pct {
            last_pct = pct;
            progress(window, spec, "download", received, total, pct);
        }
    }
    out.flush().await?;
    out.sync_all().await?;
    // A connection cut mid-body still ends as a 200: the announced length says so.
    if total > 0 && received != total {
        anyhow::bail!("the download stopped at {received} of {total} bytes");
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
