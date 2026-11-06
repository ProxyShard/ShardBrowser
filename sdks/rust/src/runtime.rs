//! Runtime cache: download ShardX engine + Widevine CDM + fingerprint
//! library from the ProxyShard CDN, extract into a per-user cache dir, place
//! Widevine inside the engine bundle, remember etags so subsequent runs are
//! zero-network. Mirrors `src-tauri/src/runtime.rs` in the launcher and the
//! Node/Python SDK `runtime` module.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use tokio::io::AsyncWriteExt;

pub const PUB_BASE: &str = "https://pub-e57a7c60f6934eb09a6600bf2fc59cdc.r2.dev";
pub const CHROMIUM_VERSION: &str = "157.0.8103.39";
/// This SDK's own version, compared against the manifest's `min_sdk_version`.
pub const SDK_VERSION: &str = env!("CARGO_PKG_VERSION");
/// Version manifest (GitHub raw) — one tiny GET yields every archive's current
/// etag, so we never poll R2/S3 (no per-archive HEAD).
pub const MANIFEST_URL: &str =
    "https://raw.githubusercontent.com/ProxyShard/ShardBrowser/main/runtime.json";

const FINGERPRINTS_KEY: &str = "ShardX-Fingerprints.zip";
const FINGERPRINTS_TOP_DIR: &str = "shardx-fingerprints";

/// Download-progress callback: `(label, received_bytes, total_bytes)`.
pub type ProgressCb = Arc<dyn Fn(&str, u64, u64) + Send + Sync>;

/// One downloadable archive on the CDN.
#[derive(Clone, Debug)]
pub struct Archive {
    pub key: String,
    pub label: String,
}

/// Per-host archive set + extracted paths.
#[derive(Clone, Debug)]
pub struct HostSpec {
    pub browser: Archive,
    pub widevine: Option<Archive>,
    pub binary_subpath: Vec<String>,
    pub widevine_subpath: Vec<String>,
}

fn arc(key: &str, label: &str) -> Archive {
    Archive {
        key: key.into(),
        label: label.into(),
    }
}

/// Archives + layout for the current host; errors on unsupported platforms.
#[allow(clippy::needless_return)]
pub fn host_spec() -> Result<HostSpec> {
    let p = |parts: &[&str]| parts.iter().map(|s| s.to_string()).collect::<Vec<_>>();

    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    {
        return Ok(HostSpec {
            browser: arc("ShardX-Mac-arm64.zip", "ShardX browser (macOS arm64)"),
            widevine: Some(arc("ShardX-Widevine-Mac-arm64.zip", "Widevine CDM")),
            binary_subpath: p(&["ShardX-Mac-arm64", "ShardX.app", "Contents", "MacOS", "ShardX"]),
            widevine_subpath: p(&[
                "ShardX-Mac-arm64",
                "ShardX.app",
                "Contents",
                "Frameworks",
                "ShardX Framework.framework",
                "Versions",
                CHROMIUM_VERSION,
                "Libraries",
                "WidevineCdm",
            ]),
        });
    }
    #[cfg(all(target_os = "windows", target_arch = "x86_64"))]
    {
        return Ok(HostSpec {
            browser: arc("ShardX-Windows.zip", "ShardX browser (Windows x64)"),
            widevine: Some(arc("ShardX-Widevine-Win.zip", "Widevine CDM")),
            binary_subpath: p(&["ShardX-Windows", "chrome.exe"]),
            widevine_subpath: p(&["ShardX-Windows", "WidevineCdm"]),
        });
    }
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    {
        return Ok(HostSpec {
            browser: arc("ShardX-Linux.zip", "ShardX browser (Linux x64)"),
            widevine: Some(arc("ShardX-Widevine-Linux.zip", "Widevine CDM")),
            binary_subpath: p(&["ShardX-Linux", "chrome"]),
            widevine_subpath: p(&["ShardX-Linux", "WidevineCdm"]),
        });
    }
    #[allow(unreachable_code)]
    Err(anyhow!(
        "Unsupported host. ShardX ships mac-arm64, win-x64, linux-x64."
    ))
}

/// Default per-user cache dir (mirrors the Node SDK layout, `shardx-sdk`).
#[allow(clippy::needless_return)]
pub fn default_cache_dir() -> PathBuf {
    let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
    #[cfg(target_os = "macos")]
    {
        return home.join("Library").join("Application Support").join("shardx-sdk");
    }
    #[cfg(target_os = "windows")]
    {
        return std::env::var_os("LOCALAPPDATA")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.clone())
            .join("shardx-sdk");
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        return std::env::var_os("XDG_CACHE_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".cache"))
            .join("shardx-sdk");
    }
}

#[derive(Default, Serialize, Deserialize)]
struct Manifest {
    browser_etag: Option<String>,
    widevine_etag: Option<String>,
    fingerprints_etag: Option<String>,
    /// Chromium version of the engine last written to disk — the fallback when
    /// the on-disk version marker can't be read (e.g. Linux).
    #[serde(default)]
    installed_chromium_version: Option<String>,
    /// Build stamp of the archive last extracted; fallback when it ships none.
    #[serde(default)]
    installed_engine_build: Option<String>,
    /// What the manifest said about the installed engine, kept for runs that
    /// cannot take it from the manifest: offline, or skipping an update this
    /// SDK is too old for.
    #[serde(default)]
    installed_grease_brand: Option<String>,
    #[serde(default)]
    installed_grease_version: Option<String>,
    #[serde(default)]
    installed_android_chromium_version: Option<String>,
    #[serde(default)]
    installed_tls: Option<serde_json::Value>,
}

pub struct Runtime {
    pub root: PathBuf,
    pub spec: HostSpec,
    profiles_override: Option<PathBuf>,
    progress: Option<ProgressCb>,
    checked: AtomicBool,
    /// Engine chromium version (manifest-driven; set on install()).
    engine_version: std::sync::Mutex<String>,
    /// GREASE brand/version from the manifest (rotates per release; can't be
    /// derived from the version). `(brand, version)`, set on install().
    grease: std::sync::Mutex<(Option<String>, Option<String>)>,
    /// TLS block from the manifest (signature_algorithms / cipher_suites),
    /// applied to profiles on launch so the JA4 hash keeps matching the Chrome
    /// version the profile claims. Set on install().
    tls: std::sync::Mutex<Option<serde_json::Value>>,
    /// Chrome for Android build of the engine's major, for phone profiles.
    android_version: std::sync::Mutex<Option<String>>,
    /// Where the manifest and the archives come from (tests serve their own).
    manifest_url: String,
    cdn: String,
    hooks: Hooks,
}

/// What the install asks the OS; tests swap in their own.
#[derive(Clone, Copy)]
struct Hooks {
    free_bytes: fn(&Path) -> Option<u64>,
    processes_under: fn(&Path) -> usize,
}

impl Default for Hooks {
    fn default() -> Self {
        Hooks { free_bytes, processes_under }
    }
}

impl Runtime {
    pub fn new(
        cache_dir: Option<PathBuf>,
        profiles_dir: Option<PathBuf>,
        progress: Option<ProgressCb>,
    ) -> Result<Self> {
        let root = cache_dir.unwrap_or_else(default_cache_dir);
        fs::create_dir_all(&root).with_context(|| format!("create cache dir {root:?}"))?;
        Ok(Self {
            root,
            spec: host_spec()?,
            profiles_override: profiles_dir,
            progress,
            checked: AtomicBool::new(false),
            engine_version: std::sync::Mutex::new(CHROMIUM_VERSION.to_string()),
            grease: std::sync::Mutex::new((None, None)),
            tls: std::sync::Mutex::new(None),
            android_version: std::sync::Mutex::new(None),
            manifest_url: MANIFEST_URL.to_string(),
            cdn: PUB_BASE.to_string(),
            hooks: Hooks::default(),
        })
    }

    /// GREASE `(brand, version)` from the manifest (set on install()).
    pub fn grease(&self) -> (Option<String>, Option<String>) {
        self.grease.lock().unwrap().clone()
    }

    /// Chrome for Android build phone profiles report (set on install()).
    pub fn android_chromium_version(&self) -> Option<String> {
        self.android_version.lock().unwrap().clone()
    }

    /// TLS overrides from the manifest (set on install()).
    pub fn tls(&self) -> Option<serde_json::Value> {
        self.tls.lock().unwrap().clone()
    }

    /// Engine chromium version (manifest-driven; set on install()).
    pub fn chromium_version(&self) -> String {
        self.engine_version.lock().unwrap().clone()
    }

    pub fn manifest_path(&self) -> PathBuf {
        self.root.join("manifest.json")
    }

    pub fn binary_path(&self) -> PathBuf {
        let mut p = self.root.clone();
        for seg in &self.spec.binary_subpath {
            p.push(seg);
        }
        p
    }

    pub fn fingerprints_dir(&self) -> PathBuf {
        let d = self.root.join("fingerprints");
        let _ = fs::create_dir_all(&d);
        d
    }

    /// Per-profile user-data-dir root. `<cache>/profiles/` unless overridden.
    pub fn profiles_root(&self) -> PathBuf {
        let d = self
            .profiles_override
            .clone()
            .unwrap_or_else(|| self.root.join("profiles"));
        let _ = fs::create_dir_all(&d);
        d
    }

    pub fn installed(&self) -> bool {
        self.binary_path().exists()
    }

    /// Chromium version of the engine actually on disk — read from the mac
    /// Framework `Versions/<ver>/` dir or the win `<ver>.manifest` file. `None`
    /// on Linux (no on-disk version marker) or when unreadable.
    fn installed_engine_version(&self) -> Option<String> {
        #[cfg(target_os = "macos")]
        {
            let versions = self
                .root
                .join("ShardX-Mac-arm64")
                .join("ShardX.app")
                .join("Contents")
                .join("Frameworks")
                .join("ShardX Framework.framework")
                .join("Versions");
            for ent in fs::read_dir(&versions).ok()?.flatten() {
                let name = ent.file_name().to_string_lossy().into_owned();
                if name != "Current" && name.chars().next().is_some_and(|c| c.is_ascii_digit()) {
                    return Some(name);
                }
            }
            None
        }
        #[cfg(target_os = "windows")]
        {
            // Only accept a `<version>.manifest` whose stem parses as a version,
            // so a stray/leftover manifest can't pin a bogus version.
            for ent in fs::read_dir(self.root.join("ShardX-Windows")).ok()?.flatten() {
                let p = ent.path();
                if p.extension().and_then(|s| s.to_str()) == Some("manifest") {
                    if let Some(stem) = p.file_stem().and_then(|s| s.to_str()) {
                        if stem.contains('.')
                            && stem.starts_with(|c: char| c.is_ascii_digit())
                        {
                            return Some(stem.to_string());
                        }
                    }
                }
            }
            None
        }
        #[cfg(not(any(target_os = "macos", target_os = "windows")))]
        {
            None
        }
    }

    /// Effective installed version. Trusts the version recorded at install time
    /// (authoritative — written only after a successful extract) over re-reading
    /// it off disk, which can carry stale files from a previous version. On-disk
    /// detection is the fallback for legacy installs with no recorded version.
    fn effective_installed_version(&self, local: &Manifest) -> Option<String> {
        local
            .installed_chromium_version
            .clone()
            .or_else(|| self.installed_engine_version())
    }

    /// Build stamp on disk: `<engine>/shardx-build` when the archive ships one,
    /// else what was recorded at install time.
    fn installed_engine_build(&self, local: &Manifest) -> Option<String> {
        self.spec
            .binary_subpath
            .first()
            .map(|dir| self.root.join(dir).join("shardx-build"))
            .and_then(|p| fs::read_to_string(p).ok())
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .or_else(|| local.installed_engine_build.clone())
    }

    /// Chromium version, `engine_build`, or the archive hash — any one of them
    /// means the engine moved. An unknown local value is not a mismatch.
    fn engine_outdated(&self, local: &Manifest, remote: &RemoteManifest) -> bool {
        let version_moved = remote
            .chromium_version
            .as_deref()
            .is_some_and(|rv| self.effective_installed_version(local).as_deref() != Some(rv));
        let differs = |have: Option<String>, want: Option<&str>| match (have, want) {
            (Some(h), Some(w)) => !h.is_empty() && h != w,
            _ => false,
        };
        let build_moved = differs(
            self.installed_engine_build(local),
            remote.engine_build.as_deref(),
        );
        let hash_moved = differs(
            local.browser_etag.clone(),
            remote.archives.get(&self.spec.browser.key).map(|s| s.as_str()),
        );
        version_moved || build_moved || hash_moved
    }

    fn load_manifest(&self) -> Manifest {
        fs::read_to_string(self.manifest_path())
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    fn save_manifest(&self, m: &Manifest) -> Result<()> {
        // Whole or not at all: other processes read it (adopt_installed).
        let path = self.manifest_path();
        let tmp = path.with_file_name("manifest.json.tmp");
        fs::write(&tmp, serde_json::to_string_pretty(m)?)?;
        fs::rename(&tmp, &path)?;
        Ok(())
    }

    fn top(&self) -> Result<String> {
        self.spec
            .binary_subpath
            .first()
            .cloned()
            .ok_or_else(|| anyhow!("no engine folder in this host's layout"))
    }

    /// Ensure the engine + Widevine + fingerprints are present and current.
    /// Cheap no-op after the first successful call in-process unless `force`.
    pub async fn install(&self, force: bool) -> Result<()> {
        if self.checked.load(Ordering::Relaxed) && !force {
            self.adopt_installed();
            return Ok(());
        }
        // One install at a time over this cache, across processes and the SDKs.
        let lock = InstallLock::acquire(&self.root, LOCK_POLL).await?;
        // Another call of this process did it while this one waited.
        if self.checked.load(Ordering::Relaxed) && !force {
            self.adopt_installed();
            return Ok(());
        }
        self.install_locked(force, &lock).await
    }

    /// Another process may have swapped the engine since this one checked: the
    /// version profiles claim follows the engine on disk, from its record.
    fn adopt_installed(&self) {
        let local = self.load_manifest();
        let Some(v) = local.installed_chromium_version.clone() else { return };
        if v == self.chromium_version() {
            return;
        }
        *self.engine_version.lock().unwrap() = v;
        *self.grease.lock().unwrap() = (local.installed_grease_brand, local.installed_grease_version);
        *self.android_version.lock().unwrap() = local.installed_android_chromium_version;
        *self.tls.lock().unwrap() = local.installed_tls;
    }

    fn stay_on_installed(&self, local: &Manifest) {
        *self.engine_version.lock().unwrap() = self
            .effective_installed_version(local)
            .unwrap_or_else(|| CHROMIUM_VERSION.to_string());
        *self.grease.lock().unwrap() = (
            local.installed_grease_brand.clone(),
            local.installed_grease_version.clone(),
        );
        *self.android_version.lock().unwrap() = local.installed_android_chromium_version.clone();
        *self.tls.lock().unwrap() = local.installed_tls.clone();
    }

    async fn install_locked(&self, force: bool, lock: &InstallLock) -> Result<()> {
        let top = self.top()?;
        // A run that died between the two renames of an update left the engine
        // as `.previous`: put back before anything asks whether it is installed.
        recover_engine(&self.root, &top, |a, b| fs::rename(a, b), |p| fs::remove_dir_all(p), RENAME_TRIES);
        sweep_leftovers(&self.root, &top, &self.spec.browser.key, &self.spec.binary_subpath);
        let mut local = self.load_manifest();
        let remote = fetch_manifest(&self.manifest_url).await;
        // An engine this SDK is too old to drive is not installed, but the one
        // already on disk keeps running. Refusing outright stopped every run on
        // an engine that worked yesterday, for an update nobody had asked for.
        // With nothing installed, or a reinstall asked for, there is nothing
        // to keep.
        let too_old = remote
            .min_sdk_version
            .as_deref()
            .is_some_and(|min| version_lt(SDK_VERSION, min));
        if too_old && (force || !self.installed()) {
            anyhow::bail!(
                "this engine build needs shardx {} or newer — you are on {SDK_VERSION}; \
                 upgrade the SDK first",
                remote.min_sdk_version.as_deref().unwrap_or_default()
            );
        }
        // Remember the engine version + grease so launch can normalise profiles.
        // Without the manifest, what is ALREADY installed — not the version
        // this SDK was published with. Falling back to the constant made an
        // offline run record 152 over an engine that is 156, and profiles
        // then declared a version the engine does not have. Skipping an
        // update, the manifest describes an engine that is not the one on
        // disk, so it is not read.
        let offered = if too_old { None } else { Some(&remote) };
        *self.engine_version.lock().unwrap() = offered
            .and_then(|r| r.chromium_version.clone())
            .or_else(|| self.effective_installed_version(&local))
            .unwrap_or_else(|| CHROMIUM_VERSION.to_string());
        *self.grease.lock().unwrap() = (
            offered
                .and_then(|r| r.grease_brand.clone())
                .or_else(|| local.installed_grease_brand.clone()),
            offered
                .and_then(|r| r.grease_version.clone())
                .or_else(|| local.installed_grease_version.clone()),
        );
        *self.tls.lock().unwrap() = offered
            .and_then(|r| r.tls.clone())
            .or_else(|| local.installed_tls.clone());
        *self.android_version.lock().unwrap() = offered
            .and_then(|r| r.android_chromium_version.clone())
            .or_else(|| local.installed_android_chromium_version.clone());
        if too_old {
            eprintln!(
                "[shardx] engine {} needs shardx {} or newer (this is {SDK_VERSION}); staying on \
                 the installed engine {}. Upgrade the SDK to get it.",
                remote.chromium_version.as_deref().unwrap_or("update"),
                remote.min_sdk_version.as_deref().unwrap_or_default(),
                self.chromium_version(),
            );
            self.checked.store(true, Ordering::Relaxed);
            return Ok(());
        }
        // Unknown is never a mismatch, so adopt a stamp once or the first bump
        // never lands.
        if self.installed()
            && remote.engine_build.is_some()
            && self.installed_engine_build(&local).is_none()
        {
            local.installed_engine_build = remote.engine_build.clone();
        }
        // A None manifest (unreachable) forces nothing.
        let need_browser = force || !self.installed() || self.engine_outdated(&local, &remote);
        let mut swapped = false;
        if need_browser {
            // Nothing installed is touched until a complete new tree is
            // unpacked beside it (see install_engine). The old tree still goes
            // whole — a leftover `<old>.manifest` or stale lib pinned the
            // detected version and re-downloaded for ever — but a download, an
            // archive or an unpack that fails must never be what removes it.
            let wanted = remote.archives.get(&self.spec.browser.key).cloned();
            match self.install_engine(wanted.as_deref(), force, Some(lock)).await {
                Ok(etag) => {
                    local.browser_etag = Some(etag);
                    swapped = true;
                }
                // With nothing installed, or a reinstall asked for, there is nothing to stay on.
                Err(e) if force || !self.installed() => return Err(e),
                Err(e) => {
                    // The engine on disk works: stay on it, as for an update this SDK is too old for.
                    self.stay_on_installed(&local);
                    eprintln!(
                        "[shardx] the engine update was not installed: {e:#}. Staying on the installed \
                         engine {}; the update is tried again on the next run.",
                        self.chromium_version()
                    );
                }
            }
        }
        if swapped {
            if remote.engine_build.is_some() {
                local.installed_engine_build = remote.engine_build.clone();
            }
            // Saved at once: the engine on disk is the new one, whatever fails next.
            // The new tree has no CDM yet, so the old one's etag must not say it has.
            local.installed_chromium_version = Some(self.chromium_version());
            record_descriptor(&mut local, &remote);
            local.widevine_etag = None;
            self.save_manifest(&local)?;
        }

        if let Some(wv) = self.spec.widevine.clone() {
            if swapped || local.widevine_etag.is_none() {
                // The CDM is no reason to fail a run whose engine is in: tried again next time.
                match self.download_and_extract(&wv, &self.root).await {
                    Ok(etag) => {
                        self.place_widevine();
                        local.widevine_etag = Some(etag);
                    }
                    Err(e) => eprintln!("[shardx] Widevine CDM not installed ({e:#}); tried again on the next run."),
                }
            }
        }

        let remote_fp = remote.archives.get(FINGERPRINTS_KEY);
        let fp_has_json = fs::read_dir(self.fingerprints_dir())
            .map(|it| {
                it.flatten()
                    .any(|e| e.path().extension().and_then(|s| s.to_str()) == Some("json"))
            })
            .unwrap_or(false);
        let need_fp = force
            || !fp_has_json
            || remote_fp
                .map(|rf| local.fingerprints_etag.as_deref() != Some(rf.as_str()))
                .unwrap_or(false);
        if need_fp {
            self.install_fingerprints().await?;
            if let Some(rf) = remote_fp {
                local.fingerprints_etag = Some(rf.clone());
            }
        }

        // Authoritative: the tree was replaced whole with exactly this version.
        // Recording the known value beats re-reading it off disk. What the
        // manifest says describes the engine on disk only when that is current.
        local.installed_chromium_version = Some(self.chromium_version());
        if !need_browser || swapped {
            record_descriptor(&mut local, &remote);
        }

        self.save_manifest(&local)?;

        #[cfg(unix)]
        fix_unix_exec_bits(&self.root);

        self.checked.store(true, Ordering::Relaxed);
        Ok(())
    }

    /// Brings the engine to the archive the manifest offers (`wanted`: its
    /// etag, when known). A checked tree an earlier run staged is used as it
    /// is; else the archive is downloaded and staged. Returns the etag.
    async fn install_engine(&self, wanted: Option<&str>, force: bool, lock: Option<&InstallLock>) -> Result<String> {
        let top = self.top()?;
        let staging = self.root.join(format!(".{top}.staging"));
        let kept = read_staged(&staging, &top, &self.spec.binary_subpath)
            .filter(|k| !force && wanted.map_or(true, |w| w == k.etag));
        let etag = match kept {
            Some(k) => k.etag,
            None => {
                // An outdated tree goes before the download: its room counts.
                clear(&staging)?;
                let (tmp, etag) = self.download(&self.spec.browser, &self.root, true).await?;
                self.stage_engine(&tmp, &etag).await?;
                etag
            }
        };
        self.swap_staged(lock)?;
        Ok(etag)
    }

    /// Unpacks the engine archive at `archive` into the staging folder beside
    /// the live engine and checks every file the archive lists. Nothing
    /// installed is touched: the tree waits there, noted, until `swap_staged`
    /// moves it in. The archive is removed either way.
    async fn stage_engine(&self, archive: &Path, etag: &str) -> Result<()> {
        let out = self.stage_engine_in(archive, etag).await;
        let _ = fs::remove_file(archive);
        out
    }

    async fn stage_engine_in(&self, archive: &Path, etag: &str) -> Result<()> {
        let key = &self.spec.browser.key;
        let top = self.top()?;
        let staging = self.root.join(format!(".{top}.staging"));
        let entry = self.spec.binary_subpath.join("/");
        // The table of contents sits at the end of the file: exactly what a
        // download cut short is missing.
        let listing = zip_listing(archive).map_err(|e| {
            anyhow!("download {key}: not a readable zip ({e}) — not touching the installed engine")
        })?;
        if !listing.names.contains(&entry) {
            anyhow::bail!("download {key}: no {entry} inside — not touching the installed engine");
        }
        // A dirty staging dir would mix an old engine into the new one.
        clear(&staging)?;
        assert_room(self.hooks.free_bytes, &self.root, listing.unpacked.saturating_add(SPACE_MARGIN))?;
        let unpacked: Result<()> = async {
            Self::extract_archive(archive, &staging).await?;
            let inside = format!("{top}/");
            if let Some(gone) = listing
                .names
                .iter()
                .find(|n| n.starts_with(&inside) && !n.ends_with('/') && fs::symlink_metadata(staging.join(n)).is_err())
            {
                anyhow::bail!("download {key}: unpacked without {gone} — an antivirus may have removed it");
            }
            #[cfg(unix)]
            fix_unix_exec_bits(&staging.join(&top));
            let note = StagedNote { etag: etag.to_string(), files: count_files(&staging.join(&top)) };
            fs::write(staging.join(STAGED_NOTE), serde_json::to_string(&note)?)?;
            Ok(())
        }
        .await;
        if let Err(e) = unpacked {
            let _ = fs::remove_dir_all(&staging);
            return Err(e);
        }
        // Whatever else the archive carries (`__MACOSX` and such) replaces its
        // namesake in the cache root, as extracting over the root used to do.
        for ent in fs::read_dir(&staging)?.flatten() {
            let name = ent.file_name();
            if name == top.as_str() || name == STAGED_NOTE {
                continue;
            }
            let dst = self.root.join(&name);
            match fs::symlink_metadata(&dst) {
                Ok(m) if m.is_dir() => fs::remove_dir_all(&dst)?,
                Ok(_) => fs::remove_file(&dst)?,
                Err(_) => {}
            }
            fs::rename(ent.path(), &dst)?;
        }
        Ok(())
    }

    /// Swaps the staged tree in. Refused while processes run from the engine;
    /// every refusal keeps the staged tree for the next run.
    fn swap_staged(&self, lock: Option<&InstallLock>) -> Result<()> {
        let top = self.top()?;
        let staging = self.root.join(format!(".{top}.staging"));
        let target = self.root.join(&top);
        if read_staged(&staging, &top, &self.spec.binary_subpath).is_none() {
            let _ = fs::remove_dir_all(&staging);
            anyhow::bail!(
                "the unpacked engine update is no longer complete (an antivirus may have removed a \
                 file); it is downloaded again on the next run"
            );
        }
        if lock.is_some_and(|l| !l.held()) {
            anyhow::bail!("another process took the engine install over");
        }
        let busy = (self.hooks.processes_under)(&target);
        if busy > 0 {
            anyhow::bail!(
                "{busy} process{} still running from {}; the update is kept and goes in once the \
                 browsers started from it are closed",
                if busy == 1 { " is" } else { "es are" },
                target.display()
            );
        }
        swap_engine_tree(&self.root, &top, &staging.join(&top), |a, b| fs::rename(a, b), |p| fs::remove_dir_all(p), RENAME_TRIES)?;
        let _ = fs::remove_dir_all(&staging);
        Ok(())
    }

    async fn download_and_extract(&self, archive: &Archive, dest: &Path) -> Result<String> {
        let (tmp, etag) = self.download(archive, dest, false).await?;
        Self::extract_archive(&tmp, dest).await?;
        Ok(etag)
    }

    /// Fetches `archive` beside `dest` without touching what is installed.
    /// `unpacks`: refused before a byte is written when the disk cannot also
    /// hold it unpacked beside what is installed.
    async fn download(&self, archive: &Archive, dest: &Path, unpacks: bool) -> Result<(PathBuf, String)> {
        let url = format!("{}/{}", self.cdn, archive.key);
        fs::create_dir_all(dest)?;
        let tmp = dest.join(format!(".{}.tmp", archive.key));

        let mut resp = reqwest::Client::new()
            .get(&url)
            .send()
            .await?
            .error_for_status()
            .with_context(|| format!("download {}", archive.key))?;
        let total = resp.content_length().unwrap_or(0);
        if unpacks && total > 0 {
            assert_room(
                self.hooks.free_bytes,
                dest,
                total.saturating_mul(1 + UNPACK_GUESS).saturating_add(SPACE_MARGIN),
            )?;
        }
        let etag = resp
            .headers()
            .get("etag")
            .and_then(|v| v.to_str().ok())
            .map(|s| s.trim_matches('"').to_string())
            .unwrap_or_default();

        let mut received: u64 = 0;
        let streamed: Result<()> = async {
            let mut out = tokio::fs::File::create(&tmp).await?;
            while let Some(chunk) = resp.chunk().await? {
                out.write_all(&chunk).await?;
                received += chunk.len() as u64;
                if let Some(cb) = &self.progress {
                    cb(&archive.label, received, total);
                }
            }
            out.flush().await?;
            Ok(())
        }
        .await;
        // A connection cut mid-body can still end as a 200 and a short file.
        if streamed.is_ok() && total > 0 && received < total {
            let _ = fs::remove_file(&tmp);
            anyhow::bail!("download {}: got {received} of {total} bytes", archive.key);
        }
        if let Err(e) = streamed {
            let _ = fs::remove_file(&tmp);
            return Err(e.context(format!("download {}", archive.key)));
        }
        Ok((tmp, etag))
    }

    /// Unpacks what `download` left behind, and removes it.
    async fn extract_archive(tmp: &Path, dest: &Path) -> Result<()> {
        let zip_path = tmp.to_path_buf();
        let dest = dest.to_path_buf();
        tokio::task::spawn_blocking(move || extract_zip(&zip_path, &dest)).await??;
        let _ = fs::remove_file(tmp);
        Ok(())
    }

    async fn install_fingerprints(&self) -> Result<()> {
        let dir = self.fingerprints_dir();
        let staging = dir.join(".staging");
        let _ = fs::remove_dir_all(&staging);
        fs::create_dir_all(&staging)?;
        let archive = Archive {
            key: FINGERPRINTS_KEY.into(),
            label: "Fingerprint library".into(),
        };
        self.download_and_extract(&archive, &staging).await?;

        let src = staging.join(FINGERPRINTS_TOP_DIR);
        let walk = if src.exists() { src } else { staging.clone() };
        for ent in fs::read_dir(&walk)? {
            let ent = ent?;
            let p = ent.path();
            if p.extension().and_then(|s| s.to_str()) != Some("json") {
                continue;
            }
            // Always overwrite bundled templates so engine-version bumps reach
            // existing libraries; user-added files (other names) are untouched.
            let dst = dir.join(p.file_name().unwrap());
            fs::copy(&p, &dst)?;
        }
        let _ = fs::remove_dir_all(&staging);
        Ok(())
    }

    fn place_widevine(&self) {
        let Some(wv) = &self.spec.widevine else { return };
        let wrapper = wv.key.trim_end_matches(".zip");
        let src = self.root.join(wrapper).join("WidevineCdm");
        if !src.exists() {
            return;
        }
        // On macOS the CDM sits inside the framework's versioned folder. That
        // is the version of the engine on disk, not the one this SDK was
        // published with: the constant put it in a folder the engine never
        // reads.
        let on_disk = self
            .installed_engine_version()
            .unwrap_or_else(|| CHROMIUM_VERSION.to_string());
        let mut dst = self.root.clone();
        for seg in &self.spec.widevine_subpath {
            dst.push(if seg == CHROMIUM_VERSION { on_disk.as_str() } else { seg.as_str() });
        }
        if dst.exists() {
            let _ = fs::remove_dir_all(&dst);
        }
        if let Some(parent) = dst.parent() {
            let _ = fs::create_dir_all(parent);
        }
        let _ = fs::rename(&src, &dst);
        let _ = fs::remove_dir_all(self.root.join(wrapper));
    }
}

#[derive(Default)]
struct RemoteManifest {
    archives: std::collections::HashMap<String, String>,
    chromium_version: Option<String>,
    /// GREASE brand/version (rotates per release; travels in the manifest as
    /// data since it can't be derived from the version number).
    grease_brand: Option<String>,
    grease_version: Option<String>,
    /// TLS overrides; only the keys present here are applied to a profile.
    tls: Option<serde_json::Value>,
    /// The Chrome for Android build of the same major, for phone profiles.
    android_chromium_version: Option<String>,
    /// Which build of the engine archive this is, apart from its Chromium
    /// version. Not the manifest-wide `revision`, which also moves for a TLS
    /// or GREASE edit.
    engine_build: Option<String>,
    /// Lowest SDK version this engine build can be driven by.
    min_sdk_version: Option<String>,
}

/// Dotted-numeric compare; non-numeric parts count as 0.
fn version_lt(a: &str, b: &str) -> bool {
    let parts = |s: &str| -> Vec<u32> {
        s.split(['.', '-', '+'])
            .map(|p| p.parse::<u32>().unwrap_or(0))
            .collect()
    };
    let (va, vb) = (parts(a), parts(b));
    for i in 0..va.len().max(vb.len()) {
        let (x, y) = (
            va.get(i).copied().unwrap_or(0),
            vb.get(i).copied().unwrap_or(0),
        );
        if x != y {
            return x < y;
        }
    }
    false
}

/// Fetch the version manifest (GitHub raw) — one request that yields every
/// archive's current etag + the chromium version, replacing per-archive HEADs
/// against R2/S3. Empty/None when unreachable.
async fn fetch_manifest(url: &str) -> RemoteManifest {
    async fn inner(url: &str) -> Option<RemoteManifest> {
        let resp = reqwest::Client::new().get(url).send().await.ok()?;
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
            android_chromium_version: str_field("android_chromium_version"),
            tls: v.get("tls").filter(|t| t.is_object()).cloned(),
            // Written as a number or a string; both mean the same thing.
            engine_build: v.get("engine_build").and_then(|b| match b {
                serde_json::Value::String(s) => Some(s.clone()),
                serde_json::Value::Number(n) => Some(n.to_string()),
                _ => None,
            }),
            min_sdk_version: str_field("min_sdk_version"),
        })
    }
    inner(url).await.unwrap_or_default()
}

// ---- engine swap (#122) ----
// Shared with the Node and Python SDKs, which use the same cache dir: in
// `<cache>`, `.install.lock`, `.<top>.staging/` (+ `staged.json`),
// `.<top>.previous` and the journal `.<top>.swapping` mean the same in all three.

/// Room kept free beyond what the unpacked engine takes.
const SPACE_MARGIN: u64 = 256 << 20;
/// Unpacked bytes per archive byte, guessed before the archive is in.
const UNPACK_GUESS: u64 = 3;
/// A lock this old is taken whoever holds it (a pid the OS gave to another).
const LOCK_MAX_AGE_MS: f64 = 6.0 * 60.0 * 60.0 * 1000.0;
const LOCK_POLL: Duration = Duration::from_millis(500);
const STAGED_NOTE: &str = "staged.json";
/// How often a rename Windows refuses for a moment (an antivirus) is tried.
const RENAME_TRIES: u32 = 6;

fn record_descriptor(local: &mut Manifest, remote: &RemoteManifest) {
    if remote.grease_brand.is_some() {
        local.installed_grease_brand = remote.grease_brand.clone();
    }
    if remote.grease_version.is_some() {
        local.installed_grease_version = remote.grease_version.clone();
    }
    if remote.tls.is_some() {
        local.installed_tls = remote.tls.clone();
    }
    if remote.android_chromium_version.is_some() {
        local.installed_android_chromium_version = remote.android_chromium_version.clone();
    }
}

/// `dir` gone, or an error: a staging dir we cannot clear stops the install
/// while the engine still works.
fn clear(dir: &Path) -> Result<()> {
    let _ = fs::remove_dir_all(dir);
    if fs::symlink_metadata(dir).is_ok() {
        anyhow::bail!("could not clear {}; close what uses it and try again", dir.display());
    }
    Ok(())
}

struct ZipListing {
    /// Entry names, `/`-separated.
    names: Vec<String>,
    /// Bytes they unpack to.
    unpacked: u64,
}

/// The archive's own table of contents.
fn zip_listing(archive: &Path) -> Result<ZipListing> {
    let mut zip = zip::ZipArchive::new(fs::File::open(archive)?)?;
    let mut names = Vec::with_capacity(zip.len());
    let mut unpacked = 0u64;
    for i in 0..zip.len() {
        let f = zip.by_index_raw(i)?;
        unpacked = unpacked.saturating_add(f.size());
        // Zips produced on Windows store backslashes; compare on one spelling.
        let n = f.name().replace('\\', "/");
        names.push(n.strip_prefix("./").map(str::to_string).unwrap_or(n));
    }
    Ok(ZipListing { names, unpacked })
}

/// A file held for now: busy, or Windows' access denied (5) / sharing violation (32).
fn transient(e: &io::Error) -> bool {
    #[cfg(windows)]
    {
        matches!(e.raw_os_error(), Some(5 | 32)) || e.kind() == io::ErrorKind::PermissionDenied
    }
    #[cfg(not(windows))]
    {
        e.raw_os_error() == Some(16) // EBUSY
    }
}

/// `f` again while a file is held for now; any other error at once.
fn retry(tries: u32, f: impl Fn() -> io::Result<()>) -> io::Result<()> {
    let mut pause = Duration::from_millis(100);
    let mut last = f();
    for _ in 1..tries.max(1) {
        match &last {
            Err(e) if transient(e) => {}
            _ => break,
        }
        std::thread::sleep(pause);
        pause = (pause * 2).min(Duration::from_secs(2));
        last = f();
    }
    last
}

/// Files and links under `dir`, links not followed.
fn count_files(dir: &Path) -> u64 {
    let Ok(entries) = fs::read_dir(dir) else { return 0 };
    entries
        .flatten()
        .map(|e| match e.file_type() {
            Ok(t) if t.is_dir() => count_files(&e.path()),
            Ok(_) => 1,
            Err(_) => 0,
        })
        .sum()
}

fn engine_paths(root: &Path, top: &str) -> (PathBuf, PathBuf, PathBuf) {
    (root.join(top), root.join(format!(".{top}.previous")), root.join(format!(".{top}.swapping")))
}

/// On disk before the rename it announces, whole or not at all.
fn write_journal(path: &Path, files: u64) -> io::Result<()> {
    use std::io::Write;
    let tmp = path.with_file_name(format!("{}.tmp", path.file_name().unwrap_or_default().to_string_lossy()));
    let mut f = fs::File::create(&tmp)?;
    f.write_all(serde_json::json!({ "files": files }).to_string().as_bytes())?;
    f.sync_all()?;
    drop(f);
    fs::rename(&tmp, path)
}

fn read_journal(path: &Path) -> Option<u64> {
    let v: serde_json::Value = serde_json::from_str(&fs::read_to_string(path).ok()?).ok()?;
    v.get("files")?.as_u64()
}

/// Puts `<root>/.<top>.previous` back when a run died between the two renames
/// of an update (or could not undo the first) — only as its journal left it,
/// and only whole. Any other copy is a leftover and goes: one a cleanup left
/// half removed never stands in for a missing engine. True when put back.
fn recover_engine(
    root: &Path,
    top: &str,
    rename: impl Fn(&Path, &Path) -> io::Result<()>,
    remove: impl Fn(&Path) -> io::Result<()>,
    tries: u32,
) -> bool {
    let (target, previous, journal) = engine_paths(root, top);
    if fs::symlink_metadata(&previous).is_ok() {
        let moved = read_journal(&journal);
        if fs::symlink_metadata(&target).is_err() && moved == Some(count_files(&previous)) {
            // Still held: kept, with its journal, for the next try.
            if retry(tries, || rename(&previous, &target)).is_err() {
                return false;
            }
            let _ = fs::remove_file(&journal);
            return true;
        }
        let _ = remove(&previous); // else the next install() tries again
    }
    if fs::symlink_metadata(&previous).is_err() {
        let _ = fs::remove_file(&journal);
    }
    false
}

/// Swaps the complete tree at `staged` in for `<root>/<top>`: live → previous,
/// staged → live, the first undone if the second fails. Any failure leaves the
/// engine that was there (or `.previous` with its journal, put back by the
/// next install()).
fn swap_engine_tree(
    root: &Path,
    top: &str,
    staged: &Path,
    rename: impl Fn(&Path, &Path) -> io::Result<()>,
    remove: impl Fn(&Path) -> io::Result<()>,
    tries: u32,
) -> Result<()> {
    let (target, previous, journal) = engine_paths(root, top);
    recover_engine(root, top, &rename, &remove, tries);
    if fs::symlink_metadata(&previous).is_ok() {
        if fs::symlink_metadata(&target).is_ok() {
            anyhow::bail!(
                "a copy of an earlier engine at {} could not be removed; delete it and try again",
                previous.display()
            );
        }
        anyhow::bail!(
            "the previous engine at {} could not be put back yet; close the browsers started from it and try again",
            previous.display()
        );
    }
    let had_live = fs::symlink_metadata(&target).is_ok();
    if had_live {
        write_journal(&journal, count_files(&target))?;
        if let Err(e) = retry(tries, || rename(&target, &previous)) {
            let _ = fs::remove_file(&journal);
            anyhow::bail!(
                "the engine at {} is in use and was not replaced ({e}); close the browsers started from it and try again",
                target.display()
            );
        }
    }
    if let Err(e) = retry(tries, || rename(staged, &target)) {
        if had_live {
            if retry(tries, || rename(&previous, &target)).is_err() {
                // The journal stays: it is what puts the engine back.
                anyhow::bail!(
                    "replacing the engine failed ({e}) and the previous one could not be put back yet; \
                     it is kept at {} and restored by the next install()",
                    previous.display()
                );
            }
            let _ = fs::remove_file(&journal);
        }
        return Err(anyhow!("replacing the engine failed: {e}"));
    }
    let _ = fs::remove_file(&journal);
    // A tree Windows still holds is clutter: the next install() removes it.
    let _ = remove(&previous);
    Ok(())
}

#[derive(Serialize, Deserialize)]
struct StagedNote {
    etag: String,
    files: u64,
}

/// A tree an earlier run unpacked, checked and noted, still whole.
fn read_staged(staging: &Path, top: &str, binary_subpath: &[String]) -> Option<StagedNote> {
    let note: StagedNote = serde_json::from_str(&fs::read_to_string(staging.join(STAGED_NOTE)).ok()?).ok()?;
    let mut bin = staging.to_path_buf();
    bin.extend(binary_subpath);
    (bin.exists() && count_files(&staging.join(top)) == note.files).then_some(note)
}

/// What an install that died left: an unfinished staging folder, a partial
/// download. A checked staged tree stays for the next swap.
fn sweep_leftovers(root: &Path, top: &str, key: &str, binary_subpath: &[String]) {
    let staging = root.join(format!(".{top}.staging"));
    if staging.exists() && read_staged(&staging, top, binary_subpath).is_none() {
        let _ = fs::remove_dir_all(&staging);
    }
    let _ = fs::remove_file(root.join(format!(".{key}.tmp")));
}

fn assert_room(free_bytes: fn(&Path) -> Option<u64>, dir: &Path, need: u64) -> Result<()> {
    match free_bytes(dir) {
        Some(free) if free < need => {
            let mb = |n: u64| n.div_ceil(1 << 20);
            anyhow::bail!(
                "not enough free disk space for the engine update: it needs about {} MB and {} MB is \
                 free in {}; the engine you have was not touched",
                mb(need),
                mb(free),
                dir.display()
            )
        }
        _ => Ok(()),
    }
}

/// Bytes the current user may still write on the disk holding `dir`.
#[cfg(unix)]
fn free_bytes(dir: &Path) -> Option<u64> {
    use std::os::unix::ffi::OsStrExt;
    let c = std::ffi::CString::new(dir.as_os_str().as_bytes()).ok()?;
    // SAFETY: plain data, filled by the call.
    let mut st: libc::statvfs = unsafe { std::mem::zeroed() };
    // SAFETY: a NUL-terminated path and the struct the call fills.
    if unsafe { libc::statvfs(c.as_ptr(), &mut st) } != 0 {
        return None;
    }
    #[allow(clippy::unnecessary_cast)]
    Some((st.f_bavail as u64).saturating_mul(st.f_frsize as u64))
}

#[cfg(windows)]
fn free_bytes(dir: &Path) -> Option<u64> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;
    let wide: Vec<u16> = dir.as_os_str().encode_wide().chain(std::iter::once(0)).collect();
    let mut avail = 0u64;
    // SAFETY: a NUL-terminated path; the totals we do not need are null.
    let ok = unsafe { GetDiskFreeSpaceExW(wide.as_ptr(), &mut avail, std::ptr::null_mut(), std::ptr::null_mut()) };
    (ok != 0).then_some(avail)
}

#[cfg(not(any(unix, windows)))]
fn free_bytes(_dir: &Path) -> Option<u64> {
    None
}

/// How many processes run from `dir`. On macOS and Linux a swap would pull the
/// files from under them; Windows refuses the rename itself.
fn processes_under(dir: &Path) -> usize {
    if !dir.is_dir() {
        return 0;
    }
    // As given and as resolved: a process names the path it was started by.
    let given = if dir.is_absolute() {
        dir.to_path_buf()
    } else {
        std::env::current_dir().map_or_else(|_| dir.to_path_buf(), |c| c.join(dir))
    };
    let mut roots = vec![given];
    if let Ok(real) = dir.canonicalize() {
        roots.push(real);
    }
    let under = |p: &Path| roots.iter().any(|r| p != r && p.starts_with(r));
    #[cfg(target_os = "linux")]
    {
        let Ok(procs) = fs::read_dir("/proc") else { return 0 };
        procs
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().bytes().all(|b| b.is_ascii_digit()))
            .filter_map(|e| fs::read_link(e.path().join("exe")).ok())
            .filter(|exe| {
                // A replaced binary reads "<path> (deleted)": still that folder.
                let s = exe.to_string_lossy();
                under(Path::new(s.strip_suffix(" (deleted)").unwrap_or(&s)))
            })
            .count()
    }
    #[cfg(target_os = "macos")]
    {
        let Ok(out) = std::process::Command::new("ps")
            .args(["-axwwo", "comm="])
            .stdin(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .output()
        else {
            return 0;
        };
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .filter(|l| under(Path::new(l.trim())))
            .count()
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        let _ = under;
        0
    }
}

fn now_ms() -> f64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0.0, |d| d.as_millis() as f64)
}

/// This machine's name as the Node and Python SDKs write it into the lock.
#[cfg(unix)]
fn host() -> String {
    let mut buf = [0u8; 256];
    // SAFETY: the buffer is as long as said; the name is NUL-terminated or truncated.
    if unsafe { libc::gethostname(buf.as_mut_ptr().cast(), buf.len()) } != 0 {
        return String::new();
    }
    let len = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
    String::from_utf8_lossy(&buf[..len]).to_lowercase()
}

#[cfg(windows)]
fn host() -> String {
    use windows_sys::Win32::System::SystemInformation::{ComputerNamePhysicalDnsHostname, GetComputerNameExW};
    let mut buf = [0u16; 256];
    let mut len = buf.len() as u32;
    // SAFETY: `len` is the buffer's length in characters.
    if unsafe { GetComputerNameExW(ComputerNamePhysicalDnsHostname, buf.as_mut_ptr(), &mut len) } == 0 {
        return String::new();
    }
    String::from_utf16_lossy(&buf[..len as usize]).to_lowercase()
}

#[cfg(not(any(unix, windows)))]
fn host() -> String {
    String::new()
}

#[cfg(unix)]
fn pid_alive(pid: i64) -> bool {
    let Ok(pid) = libc::pid_t::try_from(pid) else { return false };
    // SAFETY: signal 0 only asks whether the process is there.
    pid > 0 && (unsafe { libc::kill(pid, 0) } == 0 || io::Error::last_os_error().raw_os_error() == Some(libc::EPERM))
}

#[cfg(windows)]
fn pid_alive(pid: i64) -> bool {
    use windows_sys::Win32::Foundation::{CloseHandle, GetLastError, ERROR_ACCESS_DENIED};
    use windows_sys::Win32::System::Threading::{GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION};
    let Ok(pid) = u32::try_from(pid) else { return false };
    if pid == 0 {
        return false;
    }
    // SAFETY: closed below; limited rights are enough for the exit code.
    let h = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if h.is_null() {
        // SAFETY: reads this thread's last error.
        return unsafe { GetLastError() } == ERROR_ACCESS_DENIED;
    }
    let mut code = 0u32;
    // SAFETY: our own handle and a u32 to fill.
    let ok = unsafe { GetExitCodeProcess(h, &mut code) } != 0;
    // SAFETY: our own handle.
    unsafe { CloseHandle(h) };
    !ok || code == 259 // STILL_ACTIVE
}

#[cfg(not(any(unix, windows)))]
fn pid_alive(_pid: i64) -> bool {
    true
}

#[derive(Default)]
struct LockInfo {
    pid: Option<i64>,
    host: Option<String>,
    token: Option<String>,
    at: Option<f64>,
}

/// One install at a time over a cache dir, across processes and the three
/// SDKs (same file, same rules). A holder that died is taken over. Released
/// when dropped.
struct InstallLock {
    path: PathBuf,
    token: String,
}

impl InstallLock {
    async fn acquire(root: &Path, poll: Duration) -> Result<InstallLock> {
        let path = root.join(".install.lock");
        let token = format!("{:016x}", rand::random::<u64>());
        let mut told = false;
        loop {
            if create_lock(&path, &token)? {
                return Ok(InstallLock { path, token });
            }
            let Some(seen) = read_lock(&path) else { continue }; // released meanwhile
            if lock_is_stale(&path, &seen) {
                steal_lock(&path, &seen, &token);
                continue;
            }
            if !told && seen.pid != Some(i64::from(std::process::id())) {
                eprintln!(
                    "[shardx] waiting for another process (pid {}) to finish installing the engine… \
                     (if none is running, delete {})",
                    seen.pid.map_or("?".to_string(), |p| p.to_string()),
                    path.display()
                );
                told = true;
            }
            tokio::time::sleep(poll + Duration::from_millis(rand::random::<u64>() % 100)).await;
        }
    }

    /// Still ours: nobody took it over as stale.
    fn held(&self) -> bool {
        read_lock(&self.path).is_some_and(|l| l.token.as_deref() == Some(self.token.as_str()))
    }
}

impl Drop for InstallLock {
    fn drop(&mut self) {
        if self.held() {
            let _ = fs::remove_file(&self.path);
        }
    }
}

fn create_lock(path: &Path, token: &str) -> Result<bool> {
    use std::io::Write;
    let mut f = match fs::OpenOptions::new().write(true).create_new(true).open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => return Ok(false),
        // One being removed this instant.
        Err(e) if cfg!(windows) && e.kind() == io::ErrorKind::PermissionDenied => return Ok(false),
        Err(e) => return Err(anyhow!("could not create {}: {e}", path.display())),
    };
    let body = serde_json::json!({
        "pid": std::process::id(),
        "host": host(),
        "token": token,
        "at": now_ms() as u64,
    });
    f.write_all(body.to_string().as_bytes())?;
    f.sync_all()?;
    Ok(true)
}

/// None when there is no lock; empty when it cannot be read as one.
fn read_lock(path: &Path) -> Option<LockInfo> {
    let text = fs::read_to_string(path).ok()?;
    let Ok(serde_json::Value::Object(o)) = serde_json::from_str::<serde_json::Value>(&text) else {
        return Some(LockInfo::default());
    };
    Some(LockInfo {
        pid: o.get("pid").and_then(|p| p.as_i64()),
        host: o.get("host").and_then(|h| h.as_str()).map(str::to_string),
        token: o.get("token").and_then(|t| t.as_str()).map(str::to_string),
        at: o.get("at").and_then(|a| a.as_f64()),
    })
}

fn lock_is_stale(path: &Path, seen: &LockInfo) -> bool {
    let Ok(meta) = fs::metadata(path) else { return false };
    let now = now_ms();
    let mtime = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map_or(now, |d| d.as_millis() as f64);
    // Unreadable: being written this instant, or by a process that died doing so.
    if seen.token.is_none() {
        return now - mtime > 10_000.0;
    }
    if now - seen.at.unwrap_or(mtime) > LOCK_MAX_AGE_MS {
        return true;
    }
    // Another computer's process cannot be asked about.
    if seen.host.as_deref() != Some(host().as_str()) {
        return false;
    }
    !seen.pid.is_some_and(pid_alive)
}

/// Moves the stale lock aside and drops it, unless what moved was a fresh one
/// another process took in the meantime: that one is put back.
fn steal_lock(path: &Path, seen: &LockInfo, token: &str) {
    let aside = path.with_file_name(format!(".install.lock.stale-{token}"));
    if fs::rename(path, &aside).is_err() {
        return;
    }
    if read_lock(&aside).is_some_and(|got| got.token != seen.token) {
        let _ = fs::hard_link(&aside, path); // else a third one has it
    }
    let _ = fs::remove_file(&aside);
}

/// Extract `zip_path` into `dest`. On Unix shell out to system `unzip`
/// (preserves symlinks + exec bits the `zip` crate drops); on Windows use
/// the `zip` crate (no symlinks/exec bits to preserve there).
fn extract_zip(zip_path: &Path, dest: &Path) -> Result<()> {
    fs::create_dir_all(dest)?;
    #[cfg(unix)]
    {
        use std::process::Command;
        let out = Command::new("unzip")
            .arg("-q")
            .arg("-o")
            .arg(zip_path)
            .arg("-d")
            .arg(dest)
            .output()
            .map_err(|e| {
                anyhow!("system `unzip` not found ({e}); install via `apt install unzip` / `brew install unzip`")
            })?;
        let code = out.status.code().unwrap_or(-1);
        if code > 1 {
            anyhow::bail!(
                "unzip failed for {} (exit {}): {}",
                zip_path.display(),
                code,
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
        Ok(())
    }
    #[cfg(not(unix))]
    {
        let f = fs::File::open(zip_path)?;
        let mut archive = zip::ZipArchive::new(f)?;
        archive.extract(dest)?;
        Ok(())
    }
}

/// Add +x to every ELF/Mach-O file under `root` (Windows zip producers drop
/// Unix exec bits, so chrome + its helpers come out non-executable).
#[cfg(unix)]
fn fix_unix_exec_bits(root: &Path) {
    use std::io::Read;
    use std::os::unix::fs::PermissionsExt;
    const MAGIC: &[[u8; 4]] = &[
        [0x7f, b'E', b'L', b'F'],
        [0xfe, 0xed, 0xfa, 0xcf],
        [0xcf, 0xfa, 0xed, 0xfe],
        [0xfe, 0xed, 0xfa, 0xce],
        [0xce, 0xfa, 0xed, 0xfe],
        [0xca, 0xfe, 0xba, 0xbe],
        [0xbe, 0xba, 0xfe, 0xca],
    ];
    fn walk(dir: &Path) {
        let Ok(entries) = fs::read_dir(dir) else { return };
        for ent in entries.flatten() {
            let p = ent.path();
            let Ok(ft) = ent.file_type() else { continue };
            if ft.is_symlink() {
                continue;
            }
            if ft.is_dir() {
                walk(&p);
                continue;
            }
            if !ft.is_file() {
                continue;
            }
            let mut head = [0u8; 4];
            let Ok(mut f) = fs::File::open(&p) else { continue };
            if f.read_exact(&mut head).is_err() {
                continue;
            }
            if !MAGIC.contains(&head) {
                continue;
            }
            if let Ok(meta) = fs::metadata(&p) {
                let mut perm = meta.permissions();
                perm.set_mode(perm.mode() | 0o111);
                let _ = fs::set_permissions(&p, perm);
            }
        }
    }
    walk(root);
}

#[cfg(test)]
mod swap_tests {
    //! Engine updates that fail at each step must leave a working engine (#122).
    use super::*;
    use std::collections::HashMap;
    use std::io::Write;
    use std::sync::Mutex;
    use tokio::io::AsyncReadExt;

    const TOP: &str = "ShardX-Test";

    struct Root(PathBuf);

    impl Drop for Root {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn root() -> Root {
        use std::sync::atomic::AtomicU64;
        static N: AtomicU64 = AtomicU64::new(0);
        let n = N.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("shardx-sdk-swap-{}-{n}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        Root(dir)
    }

    fn tree(dir: &Path, version: &str) {
        fs::create_dir_all(dir).unwrap();
        fs::write(dir.join("chrome"), "engine").unwrap();
        fs::write(dir.join("version.txt"), version).unwrap();
        fs::write(dir.join(format!("only-in-{version}")), "x").unwrap();
    }

    fn version(dir: &Path) -> String {
        fs::read_to_string(dir.join("version.txt")).unwrap_or_default()
    }

    struct Swap {
        live: PathBuf,
        staged: PathBuf,
        previous: PathBuf,
        journal: PathBuf,
    }

    /// The live engine at 156 and a complete 157 staged beside it.
    fn setup(r: &Root) -> Swap {
        let s = Swap {
            live: r.0.join(TOP),
            staged: r.0.join(format!(".{TOP}.staging")).join(TOP),
            previous: r.0.join(format!(".{TOP}.previous")),
            journal: r.0.join(format!(".{TOP}.swapping")),
        };
        tree(&s.live, "156");
        tree(&s.staged, "157");
        s
    }

    fn real_rename(a: &Path, b: &Path) -> io::Result<()> {
        fs::rename(a, b)
    }

    fn real_remove(p: &Path) -> io::Result<()> {
        fs::remove_dir_all(p)
    }

    /// What Windows answers for a folder a running chrome.exe holds.
    fn refusing(bad: Vec<PathBuf>) -> impl Fn(&Path, &Path) -> io::Result<()> {
        move |a, b| {
            if bad.iter().any(|p| p == a) {
                return Err(io::Error::new(io::ErrorKind::PermissionDenied, "in use (os error 32)"));
            }
            fs::rename(a, b)
        }
    }

    #[test]
    fn an_update_replaces_the_engine_whole() {
        let r = root();
        let s = setup(&r);
        swap_engine_tree(&r.0, TOP, &s.staged, real_rename, real_remove, 1).unwrap();
        assert_eq!(version(&s.live), "157");
        assert!(!s.live.join("only-in-156").exists(), "nothing of the old version lingers");
        assert!(!s.previous.exists() && !s.journal.exists());
    }

    #[test]
    fn a_first_rename_refused_leaves_everything() {
        let r = root();
        let s = setup(&r);
        let err = swap_engine_tree(&r.0, TOP, &s.staged, refusing(vec![s.live.clone()]), real_remove, 1).unwrap_err();
        assert!(err.to_string().contains("in use"), "{err}");
        assert_eq!(version(&s.live), "156");
        assert!(s.staged.exists() && !s.journal.exists());
    }

    #[test]
    fn a_second_rename_that_fails_puts_the_engine_back() {
        let r = root();
        let s = setup(&r);
        swap_engine_tree(&r.0, TOP, &s.staged, refusing(vec![s.staged.clone()]), real_remove, 1).unwrap_err();
        assert_eq!(version(&s.live), "156");
        assert!(!s.previous.exists() && !s.journal.exists());
    }

    #[test]
    fn a_rename_refused_for_a_moment_is_tried_again() {
        let r = root();
        let s = setup(&r);
        let refusals = std::sync::atomic::AtomicUsize::new(0);
        let staged = s.staged.clone();
        let rename = |a: &Path, b: &Path| {
            if a == staged && refusals.fetch_add(1, Ordering::SeqCst) < 2 {
                return Err(io::Error::from_raw_os_error(if cfg!(windows) { 32 } else { 16 }));
            }
            fs::rename(a, b)
        };
        swap_engine_tree(&r.0, TOP, &s.staged, rename, real_remove, 4).unwrap();
        assert_eq!(refusals.load(Ordering::SeqCst), 3);
        assert_eq!(version(&s.live), "157");
    }

    #[test]
    fn a_failed_put_back_is_restored_by_the_next_install() {
        let r = root();
        let s = setup(&r);
        let err = swap_engine_tree(&r.0, TOP, &s.staged, refusing(vec![s.staged.clone(), s.previous.clone()]), real_remove, 1)
            .unwrap_err();
        assert!(err.to_string().contains("restored by the next install"), "{err}");
        assert!(!s.live.exists());
        assert!(s.journal.exists(), "the journal is what lets it back");
        assert!(recover_engine(&r.0, TOP, real_rename, real_remove, 1));
        assert_eq!(version(&s.live), "156");
        assert!(!s.journal.exists());
    }

    /// Killed after the first rename: the second never returns, and nothing
    /// after it runs.
    #[test]
    fn a_run_killed_between_the_renames_comes_back_to_the_old_engine() {
        let r = root();
        let s = setup(&r);
        let staged = s.staged.clone();
        let killed = move |a: &Path, b: &Path| {
            if a == staged {
                panic!("killed between the renames");
            }
            fs::rename(a, b)
        };
        let out = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            swap_engine_tree(&r.0, TOP, &s.staged, killed, |_| Ok(()), 1)
        }));
        assert!(out.is_err());
        assert!(!s.live.exists() && s.previous.exists() && s.journal.exists());

        assert!(recover_engine(&r.0, TOP, real_rename, real_remove, 1));
        assert_eq!(version(&s.live), "156");
        assert!(!s.previous.exists() && !s.journal.exists());
        swap_engine_tree(&r.0, TOP, &s.staged, real_rename, real_remove, 1).unwrap();
        assert_eq!(version(&s.live), "157");
    }

    #[test]
    fn a_leftover_previous_copy_is_removed_never_swapped_back() {
        let r = root();
        let s = setup(&r);
        tree(&s.previous, "half-of-155");
        assert!(!recover_engine(&r.0, TOP, real_rename, real_remove, 1));
        assert!(!s.previous.exists());
        tree(&s.previous, "half-of-155");
        swap_engine_tree(&r.0, TOP, &s.staged, refusing(vec![s.staged.clone()]), real_remove, 1).unwrap_err();
        assert_eq!(version(&s.live), "156", "the live engine, not the leftover");
    }

    /// A cleanup cut short after a finished swap: no journal, part of the files.
    #[test]
    fn a_copy_a_cleanup_left_half_removed_is_never_put_back() {
        let r = root();
        let s = setup(&r);
        tree(&s.previous, "155");
        fs::remove_file(s.previous.join("version.txt")).unwrap();
        fs::remove_dir_all(&s.live).unwrap();
        assert!(!recover_engine(&r.0, TOP, real_rename, real_remove, 1));
        assert!(!s.live.exists() && !s.previous.exists());
    }

    #[test]
    fn an_unfinished_swap_whose_previous_tree_lost_a_file_is_not_trusted() {
        let r = root();
        let s = setup(&r);
        swap_engine_tree(&r.0, TOP, &s.staged, refusing(vec![s.staged.clone(), s.previous.clone()]), real_remove, 1)
            .unwrap_err();
        fs::remove_file(s.previous.join("version.txt")).unwrap();
        assert!(!recover_engine(&r.0, TOP, real_rename, real_remove, 1));
        assert!(!s.live.exists());
    }

    #[test]
    fn a_cleanup_windows_refuses_does_not_fail_the_update() {
        let r = root();
        let s = setup(&r);
        let p = s.previous.clone();
        let locked = move |x: &Path| {
            if x == p {
                return Err(io::Error::new(io::ErrorKind::PermissionDenied, "denied"));
            }
            fs::remove_dir_all(x)
        };
        swap_engine_tree(&r.0, TOP, &s.staged, real_rename, locked, 1).unwrap();
        assert_eq!(version(&s.live), "157");
        assert!(s.previous.exists());
        assert!(!recover_engine(&r.0, TOP, real_rename, real_remove, 1));
        assert!(!s.previous.exists(), "removed by the next install()");
    }

    #[test]
    fn a_first_install_has_nothing_to_move_aside() {
        let r = root();
        let s = setup(&r);
        fs::remove_dir_all(&s.live).unwrap();
        swap_engine_tree(&r.0, TOP, &s.staged, real_rename, real_remove, 1).unwrap();
        assert_eq!(version(&s.live), "157");
    }

    #[cfg(unix)]
    #[test]
    fn processes_running_from_the_engine_folder_are_counted() {
        use std::os::unix::fs::PermissionsExt;
        let r = root();
        let dir = r.0.join("engine");
        fs::create_dir_all(&dir).unwrap();
        let bin = dir.join("browser");
        fs::copy("/bin/sleep", &bin).unwrap();
        fs::set_permissions(&bin, fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(processes_under(&dir), 0);
        let mut child = std::process::Command::new(&bin).arg("30").spawn().unwrap();
        let mut n = 0;
        for _ in 0..40 {
            n = processes_under(&dir);
            if n > 0 || child.try_wait().ok().flatten().is_some() {
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        let ran = child.try_wait().ok().flatten().is_none();
        let _ = child.kill();
        let _ = child.wait();
        if !ran {
            eprintln!("skipped: this macOS kills a copied system binary at exec");
            return;
        }
        assert_eq!(n, 1);
        assert_eq!(processes_under(&r.0.join("elsewhere")), 0);
    }

    // ---- the lock between processes ----

    fn write_lock(root: &Path, pid: u64, host: &str, token: &str) {
        let body = serde_json::json!({ "pid": pid, "host": host, "token": token, "at": now_ms() as u64 });
        fs::write(root.join(".install.lock"), body.to_string()).unwrap();
    }

    #[tokio::test]
    async fn a_lock_another_live_process_holds_is_waited_for() {
        let r = root();
        write_lock(&r.0, u64::from(std::process::id()), &host(), "other");
        let root_dir = r.0.clone();
        let waiting = tokio::spawn(async move { InstallLock::acquire(&root_dir, Duration::from_millis(20)).await });
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert!(!waiting.is_finished(), "taken while held");
        fs::remove_file(r.0.join(".install.lock")).unwrap();
        let lock = tokio::time::timeout(Duration::from_secs(5), waiting).await.unwrap().unwrap().unwrap();
        assert!(lock.held());
        drop(lock);
        assert!(!r.0.join(".install.lock").exists(), "released when dropped");
    }

    #[tokio::test]
    async fn the_lock_of_a_process_that_died_is_taken_over() {
        let r = root();
        let mut gone = std::process::Command::new(std::env::current_exe().unwrap())
            .arg("--list")
            .stdout(std::process::Stdio::null())
            .spawn()
            .unwrap();
        let pid = gone.id();
        gone.wait().unwrap();
        write_lock(&r.0, u64::from(pid), &host(), "dead");
        let lock = InstallLock::acquire(&r.0, Duration::from_millis(20)).await.unwrap();
        assert!(lock.held());
        let body: serde_json::Value = serde_json::from_str(&fs::read_to_string(r.0.join(".install.lock")).unwrap()).unwrap();
        assert_eq!(body["pid"].as_u64(), Some(u64::from(std::process::id())));
    }

    #[tokio::test]
    async fn a_holder_whose_lock_was_taken_over_knows_it() {
        let r = root();
        let lock = InstallLock::acquire(&r.0, LOCK_POLL).await.unwrap();
        write_lock(&r.0, 1, "elsewhere", "new");
        assert!(!lock.held());
        drop(lock);
        assert!(r.0.join(".install.lock").exists(), "not ours to remove");
    }

    /// The same four fields the Node and Python SDKs write and read.
    #[tokio::test]
    async fn the_lock_is_written_as_the_other_sdks_write_it() {
        let r = root();
        let lock = InstallLock::acquire(&r.0, LOCK_POLL).await.unwrap();
        let body: serde_json::Value = serde_json::from_str(&fs::read_to_string(r.0.join(".install.lock")).unwrap()).unwrap();
        let keys: Vec<&str> = body.as_object().unwrap().keys().map(String::as_str).collect();
        assert_eq!(keys.len(), 4, "{body}");
        for k in ["pid", "host", "token", "at"] {
            assert!(keys.contains(&k), "{body}");
        }
        assert_eq!(body["token"].as_str().map(str::len), Some(16));
        assert!(!host().is_empty());
        drop(lock);
    }

    // ---- install() end to end, served by a local CDN ----

    type Files = HashMap<String, (String, Vec<u8>)>;

    /// A plain HTTP server answering for `files` by path, slowly when asked to.
    struct Cdn {
        base: String,
        calls: Arc<Mutex<Vec<String>>>,
        files: Arc<Mutex<Files>>,
    }

    async fn cdn(slow: bool) -> Cdn {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let calls = Arc::new(Mutex::new(Vec::new()));
        let files: Arc<Mutex<Files>> = Arc::new(Mutex::new(HashMap::new()));
        let (c, f) = (calls.clone(), files.clone());
        tokio::spawn(async move {
            loop {
                let Ok((mut sock, _)) = listener.accept().await else { return };
                let (c, f) = (c.clone(), f.clone());
                tokio::spawn(async move {
                    let mut req = Vec::new();
                    let mut buf = [0u8; 1024];
                    while !req.windows(4).any(|w| w == b"\r\n\r\n") {
                        match sock.read(&mut buf).await {
                            Ok(0) | Err(_) => return,
                            Ok(n) => req.extend_from_slice(&buf[..n]),
                        }
                    }
                    let path = String::from_utf8_lossy(&req)
                        .split_whitespace()
                        .nth(1)
                        .unwrap_or("/")
                        .trim_start_matches('/')
                        .to_string();
                    c.lock().unwrap().push(path.clone());
                    let found = f.lock().unwrap().get(&path).cloned();
                    let Some((etag, body)) = found else {
                        let _ = sock.write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").await;
                        return;
                    };
                    let head = format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nETag: \"{etag}\"\r\nConnection: close\r\n\r\n",
                        body.len()
                    );
                    if sock.write_all(head.as_bytes()).await.is_err() {
                        return;
                    }
                    let step = if slow { body.len().div_ceil(20).max(1) } else { body.len().max(1) };
                    for part in body.chunks(step) {
                        if slow {
                            tokio::time::sleep(Duration::from_millis(15)).await;
                        }
                        if sock.write_all(part).await.is_err() {
                            return;
                        }
                    }
                    let _ = sock.shutdown().await;
                });
            }
        });
        Cdn { base, calls, files }
    }

    impl Cdn {
        fn serve(&self, path: &str, etag: &str, body: Vec<u8>) {
            self.files.lock().unwrap().insert(path.to_string(), (etag.to_string(), body));
        }
        fn manifest(&self, key: &str, v: &str, more: serde_json::Value) {
            let mut m = serde_json::json!({
                "chromium_version": v,
                "archives": { key: format!("e{v}"), FINGERPRINTS_KEY: "f1" },
            });
            if let (Some(m), Some(more)) = (m.as_object_mut(), more.as_object()) {
                m.extend(more.clone());
            }
            self.serve("runtime.json", "m", m.to_string().into_bytes());
        }
        fn fetched(&self, path: &str) -> bool {
            self.calls.lock().unwrap().iter().any(|c| c == path)
        }
    }

    fn runtime(dir: &Path, cdn: &Cdn) -> Option<Runtime> {
        let mut rt = Runtime::new(Some(dir.to_path_buf()), None, None).ok()?;
        rt.manifest_url = format!("{}/runtime.json", cdn.base);
        rt.cdn = cdn.base.clone();
        Some(rt)
    }

    /// An engine tree for this host at `v`, as archive entries under its folder.
    fn entries(spec: &HostSpec, v: &str, extra: usize) -> Vec<(String, String)> {
        let top = &spec.binary_subpath[0];
        let mut e = vec![
            (spec.binary_subpath.join("/"), format!("engine {v}")),
            (format!("{top}/version.txt"), v.to_string()),
            (format!("{top}/resources.pak"), "R".repeat(256)),
        ];
        if cfg!(target_os = "macos") {
            e.push((
                format!("{top}/ShardX.app/Contents/Frameworks/ShardX Framework.framework/Versions/{v}/ShardX Framework"),
                "fw".into(),
            ));
        }
        if cfg!(windows) {
            e.push((format!("{top}/{v}.manifest"), "<assembly/>".into()));
        }
        for i in 0..extra {
            e.push((format!("{top}/f/{i}.bin"), format!("file {i}")));
        }
        e
    }

    fn zip_of(entries: &[(String, String)]) -> Vec<u8> {
        let mut w = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        let opts = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
        for (name, body) in entries {
            w.start_file(name.as_str(), opts).unwrap();
            w.write_all(body.as_bytes()).unwrap();
        }
        w.finish().unwrap().into_inner()
    }

    /// A cache with engine `v` installed, its record, and a fingerprint library.
    fn installed(dir: &Path, spec: &HostSpec, v: &str) {
        for (name, body) in entries(spec, v, 0) {
            let p = dir.join(name);
            fs::create_dir_all(p.parent().unwrap()).unwrap();
            fs::write(p, body).unwrap();
        }
        fs::write(dir.join(&spec.binary_subpath[0]).join(format!("only-in-{v}")), "x").unwrap();
        fs::create_dir_all(dir.join("fingerprints")).unwrap();
        fs::write(dir.join("fingerprints").join("a.json"), "{}").unwrap();
        let m = serde_json::json!({
            "browser_etag": format!("e{v}"), "widevine_etag": "w1", "fingerprints_etag": "f1",
            "installed_chromium_version": v,
        });
        fs::write(dir.join("manifest.json"), m.to_string()).unwrap();
    }

    fn live_version(rt: &Runtime) -> String {
        version(&rt.root.join(&rt.spec.binary_subpath[0]))
    }

    fn record(rt: &Runtime) -> serde_json::Value {
        serde_json::from_str(&fs::read_to_string(rt.manifest_path()).unwrap()).unwrap()
    }

    fn staging(rt: &Runtime) -> PathBuf {
        rt.root.join(format!(".{}.staging", rt.spec.binary_subpath[0]))
    }

    #[tokio::test]
    async fn an_update_swaps_the_engine_in_and_records_it_at_once() {
        let r = root();
        let cdn = cdn(false).await;
        let Some(rt) = runtime(&r.0, &cdn) else { return };
        installed(&r.0, &rt.spec, "156.0.0.1");
        cdn.manifest(&rt.spec.browser.key, "157.0.0.1", serde_json::json!({}));
        cdn.serve(&rt.spec.browser.key, "e157.0.0.1", zip_of(&entries(&rt.spec, "157.0.0.1", 0)));
        rt.install(false).await.unwrap();
        assert_eq!(live_version(&rt), "157.0.0.1");
        assert!(!rt.root.join(&rt.spec.binary_subpath[0]).join("only-in-156.0.0.1").exists());
        assert_eq!(rt.chromium_version(), "157.0.0.1");
        let m = record(&rt);
        assert_eq!(m["installed_chromium_version"], "157.0.0.1");
        assert_eq!(m["browser_etag"], "e157.0.0.1");
        // The CDM failed (404): no reason to fail the run, and not marked as present.
        assert!(m["widevine_etag"].is_null());
        assert!(!staging(&rt).exists() && !r.0.join(".install.lock").exists());
    }

    async fn check_stays(body: impl FnOnce(&HostSpec) -> Vec<u8>) {
        let r = root();
        let cdn = cdn(false).await;
        let Some(rt) = runtime(&r.0, &cdn) else { return };
        installed(&r.0, &rt.spec, "156.0.0.1");
        cdn.manifest(&rt.spec.browser.key, "157.0.0.1", serde_json::json!({}));
        cdn.serve(&rt.spec.browser.key, "e157.0.0.1", body(&rt.spec));
        rt.install(false).await.unwrap();
        assert_eq!(live_version(&rt), "156.0.0.1");
        assert_eq!(rt.chromium_version(), "156.0.0.1", "profiles keep claiming the engine they run on");
        assert_eq!(record(&rt)["installed_chromium_version"], "156.0.0.1");
        assert!(!staging(&rt).exists());
        assert!(!r.0.join(format!(".{}.tmp", rt.spec.browser.key)).exists());
    }

    #[tokio::test]
    async fn a_damaged_archive_leaves_the_engine_and_the_run_goes_on() {
        check_stays(|_| b"PK\x03\x04 cut short".to_vec()).await;
    }

    #[tokio::test]
    async fn an_archive_without_the_engine_leaves_the_engine() {
        check_stays(|spec| zip_of(&[(format!("{}/readme.txt", spec.binary_subpath[0]), "hi".into())])).await;
    }

    /// A real unpack that fails after writing part of the tree: one entry's
    /// data no longer matches its CRC.
    #[tokio::test]
    async fn an_unpack_that_fails_half_way_leaves_the_engine() {
        check_stays(|spec| {
            let mut b = zip_of(&entries(spec, "157.0.0.1", 0));
            let at = b.windows(8).position(|w| w == b"RRRRRRRR").unwrap();
            b[at] = b'X';
            b
        })
        .await;
    }

    #[tokio::test]
    async fn with_nothing_installed_a_failed_install_is_an_error() {
        let r = root();
        let cdn = cdn(false).await;
        let Some(rt) = runtime(&r.0, &cdn) else { return };
        cdn.manifest(&rt.spec.browser.key, "157.0.0.1", serde_json::json!({}));
        cdn.serve(&rt.spec.browser.key, "e157.0.0.1", b"not a zip".to_vec());
        let err = rt.install(false).await.unwrap_err();
        assert!(format!("{err:#}").contains("not a readable zip"), "{err:#}");
        assert!(!rt.installed() && !r.0.join(".install.lock").exists());
    }

    #[tokio::test]
    async fn not_enough_room_refuses_before_a_byte_is_written() {
        let r = root();
        let cdn = cdn(false).await;
        let Some(mut rt) = runtime(&r.0, &cdn) else { return };
        rt.hooks.free_bytes = |_| Some(1024);
        installed(&r.0, &rt.spec, "156.0.0.1");
        cdn.manifest(&rt.spec.browser.key, "157.0.0.1", serde_json::json!({}));
        cdn.serve(&rt.spec.browser.key, "e157.0.0.1", zip_of(&entries(&rt.spec, "157.0.0.1", 0)));
        rt.install(false).await.unwrap();
        assert_eq!(live_version(&rt), "156.0.0.1");
        assert!(!r.0.join(format!(".{}.tmp", rt.spec.browser.key)).exists());
    }

    #[tokio::test]
    async fn not_enough_room_to_unpack_refuses_before_unpacking() {
        let r = root();
        let cdn = cdn(false).await;
        let Some(mut rt) = runtime(&r.0, &cdn) else { return };
        // Enough for the margin alone, not for the files.
        rt.hooks.free_bytes = |_| Some(SPACE_MARGIN + 10);
        installed(&r.0, &rt.spec, "156.0.0.1");
        let tmp = r.0.join(format!(".{}.tmp", rt.spec.browser.key));
        fs::write(&tmp, zip_of(&entries(&rt.spec, "157.0.0.1", 50))).unwrap();
        let err = rt.stage_engine(&tmp, "e157.0.0.1").await.unwrap_err();
        assert!(err.to_string().contains("not enough free disk space"), "{err}");
        assert!(!staging(&rt).join(&rt.spec.binary_subpath[0]).exists());
        assert!(!tmp.exists());
        assert_eq!(live_version(&rt), "156.0.0.1");
    }

    #[tokio::test]
    async fn running_browsers_refuse_the_swap_and_the_next_run_swaps_without_downloading() {
        let r = root();
        let cdn = cdn(false).await;
        let Some(mut busy) = runtime(&r.0, &cdn) else { return };
        busy.hooks.processes_under = |_| 2;
        installed(&r.0, &busy.spec, "156.0.0.1");
        let key = busy.spec.browser.key.clone();
        cdn.manifest(&key, "157.0.0.1", serde_json::json!({}));
        cdn.serve(&key, "e157.0.0.1", zip_of(&entries(&busy.spec, "157.0.0.1", 0)));
        busy.install(false).await.unwrap();
        assert_eq!(live_version(&busy), "156.0.0.1");
        assert_eq!(busy.chromium_version(), "156.0.0.1");
        assert!(staging(&busy).join(STAGED_NOTE).exists(), "the unpacked update is kept");

        cdn.calls.lock().unwrap().clear();
        let mut next = runtime(&r.0, &cdn).unwrap();
        next.hooks.processes_under = |_| 0;
        next.install(false).await.unwrap();
        assert_eq!(live_version(&next), "157.0.0.1");
        assert!(!cdn.fetched(&key), "not downloaded again");
        assert!(!staging(&next).exists());
    }

    #[tokio::test]
    async fn a_process_that_checked_earlier_follows_an_engine_another_swapped_in() {
        let r = root();
        let cdn = cdn(false).await;
        let Some(early) = runtime(&r.0, &cdn) else { return };
        installed(&r.0, &early.spec, "156.0.0.1");
        let key = early.spec.browser.key.clone();
        cdn.manifest(&key, "156.0.0.1", serde_json::json!({}));
        early.install(false).await.unwrap();
        assert_eq!(early.chromium_version(), "156.0.0.1");

        cdn.manifest(&key, "157.0.0.1", serde_json::json!({ "grease_brand": "Not.A/Brand", "grease_version": "99" }));
        cdn.serve(&key, "e157.0.0.1", zip_of(&entries(&early.spec, "157.0.0.1", 0)));
        runtime(&r.0, &cdn).unwrap().install(false).await.unwrap();
        cdn.calls.lock().unwrap().clear();
        early.install(false).await.unwrap();
        assert!(cdn.calls.lock().unwrap().is_empty(), "no network needed");
        assert_eq!(early.chromium_version(), "157.0.0.1");
        assert_eq!(early.grease().0.as_deref(), Some("Not.A/Brand"));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn two_installs_updating_one_cache_at_once_take_turns() {
        let r = root();
        let cdn = cdn(true).await;
        let (Some(a), Some(b)) = (runtime(&r.0, &cdn), runtime(&r.0, &cdn)) else { return };
        installed(&r.0, &a.spec, "156.0.0.1");
        let key = a.spec.browser.key.clone();
        let files = entries(&a.spec, "157.0.0.1", 3000);
        cdn.manifest(&key, "157.0.0.1", serde_json::json!({}));
        cdn.serve(&key, "e157.0.0.1", zip_of(&files));
        let (ra, rb) = tokio::join!(a.install(false), b.install(false));
        ra.unwrap();
        rb.unwrap();
        let downloads = cdn.calls.lock().unwrap().iter().filter(|c| **c == key).count();
        assert_eq!(downloads, 1, "the second found it done");
        assert_eq!(a.chromium_version(), "157.0.0.1");
        assert_eq!(b.chromium_version(), "157.0.0.1");
        assert_eq!(live_version(&a), "157.0.0.1");
        assert_eq!(count_files(&r.0.join(&a.spec.binary_subpath[0])) as usize, files.len());
        assert!(!r.0.join(".install.lock").exists() && !staging(&a).exists());
    }
}
