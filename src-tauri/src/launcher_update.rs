//! The launcher updating itself (#161): the release's `latest.json`, signed with
//! the project's updater key, names the new version; the window asks, then this
//! downloads the package, closes what quitting would close and installs it.
//!
//! Only an installed copy updates itself: the .app, the Windows installers, the
//! AppImage and the Linux packages carry their bundle type. The portable .exe
//! carries none; installing over it would leave a second, installed copy, so it
//! is offered the release page instead (`installable: false`), as is any copy
//! whose release has no `latest.json`.

use std::sync::Mutex;

use serde::Serialize;
use tauri::{AppHandle, Emitter};
use tauri_plugin_updater::{Update, UpdaterExt};

/// What the window offers: a version, and whether this copy can install it.
#[derive(Serialize, Clone, Debug)]
pub struct LauncherOffer {
    pub current: String,
    pub version: String,
    pub installable: bool,
    /// The release page, for a copy that cannot install the update itself.
    pub release_url: Option<String>,
}

/// The update the last check found, and its package once downloaded.
static FOUND: Mutex<Option<Update>> = Mutex::new(None);
static PACKAGE: Mutex<Option<(String, Vec<u8>)>> = Mutex::new(None);

/// This copy can replace itself: a release build that came in a bundle.
fn installable() -> bool {
    !cfg!(debug_assertions) && tauri::utils::platform::bundle_type().is_some()
}

#[tauri::command]
pub async fn launcher_update_offer(app: AppHandle) -> Result<Option<LauncherOffer>, String> {
    let current = app.package_info().version.to_string();
    if installable() {
        match app.updater().map_err(|e| e.to_string())?.check().await {
            Ok(Some(update)) => {
                let version = update.version.clone();
                *FOUND.lock().unwrap() = Some(update);
                return Ok(Some(LauncherOffer { current, version, installable: true, release_url: None }));
            }
            Ok(None) => return Ok(None),
            // No latest.json in the release, or GitHub unreachable: the release list still says.
            Err(e) => eprintln!("[launcher] update check: {e}"),
        }
    }
    let info = crate::runtime::launcher_update_check(app).await?;
    Ok(match (info.update_available, info.latest) {
        (true, Some(latest)) => Some(LauncherOffer {
            current: info.current,
            version: latest.trim_start_matches('v').to_string(),
            installable: false,
            release_url: info.release_url,
        }),
        _ => None,
    })
}

#[derive(Serialize, Clone)]
struct Progress {
    received: u64,
    total: Option<u64>,
}

/// Downloads the package the last check found and checks its signature;
/// progress goes to the window as `launcher-update://progress`.
#[tauri::command]
pub async fn launcher_update_download(app: AppHandle) -> Result<String, String> {
    let update = FOUND.lock().unwrap().clone().ok_or("No update was found to download")?;
    if let Some((version, _)) = PACKAGE.lock().unwrap().as_ref() {
        if *version == update.version {
            return Ok(update.version);
        }
    }
    let mut received = 0u64;
    let bytes = update
        .download(
            |chunk, total| {
                received += chunk as u64;
                let _ = app.emit("launcher-update://progress", Progress { received, total });
            },
            || {},
        )
        .await
        .map_err(|e| e.to_string())?;
    *PACKAGE.lock().unwrap() = Some((update.version.clone(), bytes));
    Ok(update.version)
}

/// Installs the downloaded package and starts the new version. The window has
/// stopped the running profiles and asked about unsaved changes; this closes
/// what quitting closes (team and synced profiles, protected profiles' data).
/// On Windows the installer takes over and ends this process itself.
#[tauri::command]
pub async fn launcher_update_apply(app: AppHandle) -> Result<(), String> {
    if crate::reset::active() {
        return Err(crate::reset::WAIT.to_string());
    }
    let update = FOUND.lock().unwrap().clone().ok_or("No update was found to install")?;
    let bytes = match PACKAGE.lock().unwrap().take() {
        Some((version, bytes)) if version == update.version => bytes,
        _ => return Err("The update is not downloaded yet".into()),
    };
    crate::space::open::close_all(crate::space::open::QUIT_WAIT).await;
    crate::space::profile::close_unsealed(crate::space::open::QUIT_WAIT).await;
    if let Err(e) = update.install(&bytes) {
        // Kept, so Try again does not download it twice.
        *PACKAGE.lock().unwrap() = Some((update.version.clone(), bytes));
        return Err(e.to_string());
    }
    app.restart()
}
