// $CONFIG/shardx-launcher/: settings.json, proxies.json, bookmarks.json, and
// under data_root() — profiles/, user-data/, extensions/, trash/.
//
// data_root() is movable to another disk from Settings; the config files stay
// put, since that is where the new location is recorded.

#[cfg(not(test))]
use anyhow::Context;
use anyhow::Result;
use std::path::{Path, PathBuf};
use std::sync::{OnceLock, RwLock};

pub fn config_root() -> Result<PathBuf> {
    #[cfg(test)]
    let root = test_config_or_panic("config root");
    #[cfg(not(test))]
    let root = dirs::config_dir().context("OS config dir unavailable")?.join("shardx-launcher");
    std::fs::create_dir_all(&root)?;
    Ok(root)
}

/// What stays on this machine: on Windows the config dir roams with the
/// account to other PCs. The same folder as `config_root` on macOS.
pub fn local_root() -> Result<PathBuf> {
    #[cfg(test)]
    let root = test_config_or_panic("local root").join("local");
    #[cfg(not(test))]
    let root = dirs::data_local_dir().context("OS local data dir unavailable")?.join("shardx-launcher");
    std::fs::create_dir_all(&root)?;
    Ok(root)
}

/// A test never reaches the owner's real folders: without a root of its own
/// it fails here, loudly, instead of writing into them.
#[cfg(test)]
pub(crate) fn test_config_or_panic(what: &str) -> PathBuf {
    if let Some(p) = TEST_CONFIG.with(|c| c.borrow().clone()) {
        return p;
    }
    let thread = std::thread::current();
    let msg = format!(
        "a test resolved the real {what} (thread {:?}): take crate::store::test_root(..) first",
        thread.name().unwrap_or("?")
    );
    // Straight to stderr, past the harness's capture, so a helper thread's panic shows too.
    use std::io::Write as _;
    let _ = writeln!(std::io::stderr(), "[store] {msg}");
    panic!("{msg}");
}

fn data_root_cell() -> &'static RwLock<Option<PathBuf>> {
    static CELL: OnceLock<RwLock<Option<PathBuf>>> = OnceLock::new();
    CELL.get_or_init(|| RwLock::new(None))
}

/// Point the heavy directories at `root` (None = back to the config dir).
pub fn set_data_root(root: Option<PathBuf>) {
    if let Ok(mut g) = data_root_cell().write() {
        *g = root;
    }
}

/// Where profiles, user-data, extensions and the trash live.
pub fn data_root() -> Result<PathBuf> {
    let over = data_root_cell().read().ok().and_then(|g| g.clone());
    match over {
        Some(p) => {
            std::fs::create_dir_all(&p)?;
            Ok(p)
        }
        None => config_root(),
    }
}

fn data_sub(name: &str) -> Result<PathBuf> {
    let p = data_root()?.join(name);
    std::fs::create_dir_all(&p)?;
    Ok(p)
}

pub fn profiles_dir() -> Result<PathBuf> {
    data_sub("profiles")
}

pub fn user_data_root() -> Result<PathBuf> {
    data_sub("user-data")
}

/// Where Chromium keeps the disk cache of the browser profile at `udd`. On macOS and Linux a
/// profile under the OS config folder gets it under the OS cache folder, at the same path.
pub fn browser_cache_twin(udd: &Path) -> Option<PathBuf> {
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    if let (Some(config), Some(cache)) = (dirs::config_dir(), dirs::cache_dir()) {
        return udd.strip_prefix(&config).ok().map(|rel| cache.join(rel));
    }
    let _ = udd;
    None
}

/// Removes that cache with the profile: it is not inside the folder the profile is deleted with.
pub fn remove_browser_cache(udd: &Path) {
    if let Some(twin) = browser_cache_twin(udd) {
        let _ = std::fs::remove_dir_all(twin);
    }
}

/// Caches of profiles deleted before their cache went with them (#125). Returns how many.
pub fn sweep_orphan_caches() -> usize {
    let Ok(root) = user_data_root() else { return 0 };
    let Some(twin) = browser_cache_twin(&root) else { return 0 };
    let Ok(entries) = std::fs::read_dir(&twin) else { return 0 };
    let mut n = 0;
    for e in entries.flatten() {
        if e.path().is_dir() && !root.join(e.file_name()).exists() && std::fs::remove_dir_all(e.path()).is_ok() {
            n += 1;
        }
    }
    n
}

/// Unpacked extensions, one directory per id; `--load-extension` points here.
pub fn extensions_dir() -> Result<PathBuf> {
    data_sub("extensions")
}

/// Password-protected profiles' sealed browser data, one `<id>.data` each (#127).
pub fn protected_dir() -> Result<PathBuf> {
    data_sub("protected")
}

/// Deleted profiles, one `<id>.zip` + `<id>.json` manifest each.
pub fn trash_dir() -> Result<PathBuf> {
    data_sub("trash")
}

pub fn fingerprints_dir() -> Result<PathBuf> {
    let p = config_root()?.join("fingerprints");
    std::fs::create_dir_all(&p)?;
    Ok(p)
}

/// Cached Widevine CDM, seeded from a host Chrome install (or downloaded from
/// the project's git LFS bucket).  Every freshly-created profile gets a
/// pre-warmed copy so a DRM page doesn't stall on the component updater.
pub fn widevine_cache_dir() -> Result<PathBuf> {
    Ok(config_root()?.join("widevine-cdm"))
}

pub fn proxies_path() -> Result<PathBuf> {
    Ok(config_root()?.join("proxies.json"))
}

pub fn settings_path() -> Result<PathBuf> {
    Ok(config_root()?.join("settings.json"))
}

/// Folder-scoped bookmarks, merged into each profile's Bookmarks on launch.
pub fn bookmarks_path() -> Result<PathBuf> {
    Ok(config_root()?.join("bookmarks.json"))
}

/// Tags and which profiles carry them.
pub fn tags_path() -> Result<PathBuf> {
    Ok(config_root()?.join("tags.json"))
}

/// Folders kept while empty, and each folder's color and icon.
pub fn folders_path() -> Result<PathBuf> {
    Ok(config_root()?.join("folders.json"))
}

/// Automation projects: one JSON file holding every project's blocks.
pub fn automation_path() -> Result<PathBuf> {
    Ok(config_root()?.join("automation.json"))
}

/// ProxyShard billing-API config (Bearer key). Kept in its own file so the
/// Settings page (which round-trips the whole Settings struct) can never
/// clobber the saved key.
/// What an OS leaves beside our files: macOS's "._name" twins on exFAT and FAT
/// drives, Finder and Explorer metadata. Never data, so every scan skips it.
pub fn is_os_litter(name: &std::ffi::OsStr) -> bool {
    let n = name.to_string_lossy();
    n.starts_with("._")
        || matches!(
            n.as_ref(),
            ".DS_Store" | ".Spotlight-V100" | ".Trashes" | ".fseventsd" | ".TemporaryItems" | "Thumbs.db" | "desktop.ini"
        )
}

pub fn psapi_path() -> Result<PathBuf> {
    Ok(config_root()?.join("psapi.json"))
}

/// A data root of a test's own, its config dir under it on this thread. Tests
/// that move the root take turns through this; all is undone on drop.
#[cfg(test)]
pub struct TestRoot {
    pub path: PathBuf,
    _turn: std::sync::MutexGuard<'static, ()>,
}

#[cfg(test)]
pub fn test_root(tag: &str) -> TestRoot {
    static TURN: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let turn = TURN.lock().unwrap_or_else(|e| e.into_inner());
    let path = std::env::temp_dir().join(format!("shardx-{tag}-{}", uuid::Uuid::new_v4().simple()));
    std::fs::create_dir_all(&path).expect("make the test's data root");
    set_data_root(Some(path.clone()));
    TEST_CONFIG.with(|c| *c.borrow_mut() = Some(path.join("config")));
    TestRoot { path, _turn: turn }
}

#[cfg(test)]
thread_local! {
    /// The config dir of the test running on this thread, when it took one.
    static TEST_CONFIG: std::cell::RefCell<Option<PathBuf>> = const { std::cell::RefCell::new(None) };
}

/// Whether this thread's test keeps its settings under its own root.
#[cfg(test)]
pub fn test_config_set() -> bool {
    TEST_CONFIG.with(|c| c.borrow().is_some())
}

/// This thread's test config dir, for a child process of the test to use.
#[cfg(test)]
pub fn test_config() -> Option<PathBuf> {
    TEST_CONFIG.with(|c| c.borrow().clone())
}

/// A test's child process takes its parent's config dir, on this thread.
#[cfg(test)]
pub fn set_test_config(dir: Option<PathBuf>) {
    TEST_CONFIG.with(|c| *c.borrow_mut() = dir);
}

#[cfg(test)]
impl TestRoot {
    /// Settings-side files under the test's root; test_root does this already,
    /// so this only takes the config back after a test moved it elsewhere.
    pub fn with_config(self) -> Self {
        TEST_CONFIG.with(|c| *c.borrow_mut() = Some(self.path.join("config")));
        self
    }
}

#[cfg(test)]
impl Drop for TestRoot {
    fn drop(&mut self) {
        TEST_CONFIG.with(|c| *c.borrow_mut() = None);
        set_data_root(None);
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

#[cfg(test)]
mod cache_twin_tests {
    use super::browser_cache_twin;

    /// #125: Chromium's own mapping, profile under the config folder -> same path under the cache one.
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn a_profile_in_the_config_folder_has_its_cache_in_the_cache_folder() {
        let udd = dirs::config_dir().unwrap().join("shardx-launcher/user-data/abc");
        assert_eq!(browser_cache_twin(&udd), Some(dirs::cache_dir().unwrap().join("shardx-launcher/user-data/abc")));
    }

    #[test]
    fn a_profile_elsewhere_keeps_its_cache_inside() {
        assert_eq!(browser_cache_twin(std::path::Path::new("/Volumes/Data/shardx/user-data/abc")), None);
    }
}
