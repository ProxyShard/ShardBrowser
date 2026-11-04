//! Password-protected profiles (#127): locking a profile, opening it for one start, sealing it after.
//! A locked profile's file keeps its name readable and the rest in one sealed block; its browser
//! data is a sealed archive while closed, and `user-data/<id>` exists only while it runs.

use std::fs;
use std::path::PathBuf;

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

use super::profile::{self, StoredMeta, StoredProfile};
use super::seal::{self, DataKey};
use crate::store;

pub const PASSWORD_REQUIRED: &str = "this profile is protected by a password";
pub const TOO_SHORT: &str = "the password is too short";
pub const CLOSE_FIRST: &str = "close the profile first";
pub const NOT_FOR_PROTECTED: &str = "not available for a password-protected profile";
const MIN_LEN: usize = 8;
/// The config key of the sealed block; `name` beside it stays readable.
const SEALED: &str = "sealed";

/// What the lock hides: the fingerprint, the proxy binding, and the rest that names the device.
#[derive(Serialize, Deserialize, Default)]
struct Secret {
    config: serde_json::Map<String, serde_json::Value>,
    proxy_id: Option<String>,
    inline_proxy: Option<super::proxy::ProxyEntry>,
    gpu_preset_id: Option<String>,
    extensions: Vec<String>,
    screen_custom: bool,
    android_media: bool,
}

pub(in crate::space) fn is_protected(stored: &StoredProfile) -> bool {
    stored.meta.lock.is_some()
}

/// Does `stored` carry anything a locked profile must not keep readable?
pub(in crate::space) fn shows_secrets(stored: &StoredProfile) -> bool {
    let m = &stored.meta;
    stored.config.keys().any(|k| k != "name" && k != SEALED)
        || m.proxy_id.is_some()
        || m.inline_proxy.is_some()
        || m.gpu_preset_id.is_some()
        || !m.extensions.is_empty()
        || m.screen_custom
        || m.android_media
}

pub(in crate::space) fn data_path(id: &str) -> Result<PathBuf> {
    Ok(store::protected_dir()?.join(format!("{id}.data")))
}

fn udd_of(id: &str) -> Result<PathBuf> {
    Ok(store::user_data_root()?.join(id))
}

fn check_password(password: &str) -> Result<()> {
    if password.chars().count() < MIN_LEN {
        bail!(TOO_SHORT);
    }
    Ok(())
}

/// Refused while the browser runs: its files are open, and sealing them would catch them half-written.
fn check_closed(id: &str) -> Result<()> {
    if crate::process::Tracker::shared().is_running(id) || crate::reset::lock_is_live(&udd_of(id)?) {
        bail!(CLOSE_FIRST);
    }
    Ok(())
}

/// The file's form of `full`: the name readable, the rest sealed under `key`.
fn stub_of(full: &StoredProfile, key: &DataKey, lock: &seal::Lock) -> Result<StoredProfile> {
    let mut config = full.config.clone();
    let name = config.remove("name");
    let secret = Secret {
        config,
        proxy_id: full.meta.proxy_id.clone(),
        inline_proxy: full.meta.inline_proxy.clone(),
        gpu_preset_id: full.meta.gpu_preset_id.clone(),
        extensions: full.meta.extensions.clone(),
        screen_custom: full.meta.screen_custom,
        android_media: full.meta.android_media,
    };
    let blob = seal::seal_blob(&full.meta.id, key, "profile", &serde_json::to_vec(&secret)?)?;
    let meta = StoredMeta {
        proxy_id: None,
        inline_proxy: None,
        gpu_preset_id: None,
        extensions: Vec::new(),
        screen_custom: false,
        android_media: false,
        lock: Some(lock.clone()),
        ..full.meta.clone()
    };
    let mut out = serde_json::Map::new();
    if let Some(n) = name {
        out.insert("name".into(), n);
    }
    out.insert(SEALED.into(), blob.into());
    Ok(StoredProfile { meta, config: out })
}

/// `stub` opened with `key`: the profile as it was before it was locked, its lock still on it.
fn full_of(stub: &StoredProfile, key: &DataKey) -> Result<StoredProfile> {
    let blob = stub.config.get(SEALED).and_then(|v| v.as_str()).context("profile has no sealed part")?;
    let secret: Secret = serde_json::from_slice(&seal::open_blob(&stub.meta.id, key, "profile", blob)?)?;
    let mut config = secret.config;
    if let Some(n) = stub.config.get("name") {
        config.insert("name".into(), n.clone());
    }
    let meta = StoredMeta {
        proxy_id: secret.proxy_id,
        inline_proxy: secret.inline_proxy,
        gpu_preset_id: secret.gpu_preset_id,
        extensions: secret.extensions,
        screen_custom: secret.screen_custom,
        android_media: secret.android_media,
        ..stub.meta.clone()
    };
    Ok(StoredProfile { meta, config })
}

/// The profile `id` opened with `password`, and the key that seals it again.
pub(in crate::space) fn unlock(id: &str, password: &str) -> Result<(StoredProfile, DataKey)> {
    let stub = profile::load_raw(id)?;
    let lock = stub.meta.lock.clone().context("profile is not protected")?;
    let key = seal::unlock(id, &lock, password)?;
    Ok((full_of(&stub, &key)?, key))
}

/// Lock profile `id` under `password`: data first, then the file, then the plaintext goes.
pub(in crate::space) fn protect(id: &str, password: &str) -> Result<()> {
    check_password(password)?;
    check_closed(id)?;
    let _guard = profile::file_lock();
    let full = profile::load_raw(id)?;
    if is_protected(&full) {
        bail!("this profile is already protected");
    }
    if full.meta.temporary {
        bail!(NOT_FOR_PROTECTED);
    }
    let (lock, key) = seal::new_lock(id, password)?;
    let udd = udd_of(id)?;
    if udd.exists() {
        seal::seal_dir(&udd, &data_path(id)?, id, &key)?;
    }
    profile::write_raw(&stub_of(&full, &key, &lock)?)?;
    wipe(&udd);
    Ok(())
}

/// Take the lock off profile `id`: its data back in `user-data`, its file readable again.
pub(in crate::space) fn unprotect(id: &str, password: &str) -> Result<()> {
    check_closed(id)?;
    let (mut full, key) = unlock(id, password)?;
    let _guard = profile::file_lock();
    open_data(id, &key)?;
    full.meta.lock = None;
    profile::write_raw(&full)?;
    let _ = fs::remove_file(data_path(id)?);
    Ok(())
}

/// The same data key under `new`: nothing else is sealed again.
pub(in crate::space) fn change_password(id: &str, old: &str, new: &str) -> Result<()> {
    check_password(new)?;
    let (_, key) = unlock(id, old)?;
    let _guard = profile::file_lock();
    let mut stub = profile::load_raw(id)?;
    stub.meta.lock = Some(seal::wrap(id, &key, new)?);
    profile::write_raw(&stub)
}

/// Seal an edited `full` again; what others own in the file (pin, folder, counters) is carried over.
pub(in crate::space) fn save_full(full: &StoredProfile, key: &DataKey) -> Result<()> {
    let _guard = profile::file_lock();
    let existing = profile::load_raw(&full.meta.id)?;
    let lock = existing.meta.lock.clone().context("profile is not protected")?;
    let mut next = full.clone();
    next.meta.pinned = existing.meta.pinned;
    next.meta.folder = existing.meta.folder.clone();
    next.meta.created_at = existing.meta.created_at.clone();
    next.meta.last_launched_at = existing.meta.last_launched_at.clone();
    next.meta.total_runtime_ms = existing.meta.total_runtime_ms;
    let before = full_of(&existing, key)?;
    next.meta.rev = if before.config == next.config && before.meta.proxy_id == next.meta.proxy_id {
        existing.meta.rev
    } else {
        existing.meta.rev.wrapping_add(1)
    };
    profile::write_raw(&stub_of(&next, key, &lock)?)
}

/// The browser data of `id`, opened for a start. A folder already there is what a crash left
/// while it ran, newer than the archive, and is used as it is.
pub(in crate::space) fn open_data(id: &str, key: &DataKey) -> Result<PathBuf> {
    let udd = udd_of(id)?;
    if !udd.exists() {
        let sealed = data_path(id)?;
        if sealed.exists() {
            let tmp = udd.with_extension("opening");
            let _ = fs::remove_dir_all(&tmp);
            seal::open_dir(&sealed, &tmp, id, key)?;
            fs::rename(&tmp, &udd).with_context(|| format!("place {}", udd.display()))?;
        } else {
            fs::create_dir_all(&udd)?;
        }
    }
    Ok(udd)
}

/// Seal the browser data of `id` again and remove the plaintext; kept as it is if sealing fails.
pub(in crate::space) fn seal_data(id: &str, key: &DataKey) -> Result<()> {
    let udd = udd_of(id)?;
    if !udd.exists() {
        return Ok(());
    }
    seal::seal_dir(&udd, &data_path(id)?, id, key)?;
    wipe(&udd);
    Ok(())
}

/// Profiles left open by a crash: their plaintext is still on disk until the next unlock.
pub(in crate::space) fn left_open() -> Vec<String> {
    let Ok(dir) = store::profiles_dir() else { return Vec::new() };
    let Ok(entries) = fs::read_dir(dir) else { return Vec::new() };
    entries
        .flatten()
        .filter_map(|e| e.path().file_stem().map(|s| s.to_string_lossy().into_owned()))
        .filter(|id| {
            profile::load_raw(id).is_ok_and(|p| is_protected(&p))
                && udd_of(id).is_ok_and(|u| u.exists())
                && !crate::process::Tracker::shared().is_running(id)
        })
        .collect()
}

fn wipe(udd: &std::path::Path) {
    let _ = fs::remove_dir_all(udd);
    store::remove_browser_cache(udd);
}

/// Remove the sealed data of a profile being deleted for good.
pub(in crate::space) fn forget(id: &str) {
    if let Ok(p) = data_path(id) {
        let _ = fs::remove_file(p);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plain(id: &str) -> StoredProfile {
        let mut p = StoredProfile::default();
        p.meta.id = id.into();
        p.meta.proxy_id = Some("proxy-marker-1".into());
        p.meta.folder = "Work".into();
        p.meta.extensions = vec!["ext-marker-1".into()];
        p.config.insert("name".into(), "Visible".into());
        p.config.insert("notes".into(), "secret notes marker".into());
        p.config.insert("navigator".into(), serde_json::json!({ "platform": "Win32-marker" }));
        p
    }

    #[test]
    fn a_locked_file_keeps_only_the_name_readable() {
        let _root = store::test_root("protect-stub");
        let p = plain("a1");
        profile::write_raw(&p).unwrap();
        let udd = udd_of("a1").unwrap();
        fs::create_dir_all(udd.join("Default")).unwrap();
        fs::write(udd.join("Default/Cookies"), b"cookie jar").unwrap();

        protect("a1", "long enough").unwrap();
        let body = fs::read_to_string(store::profiles_dir().unwrap().join("a1.json")).unwrap();
        for leak in ["proxy-marker-1", "secret notes marker", "Win32-marker", "ext-marker-1"] {
            assert!(!body.contains(leak), "{leak} readable in {body}");
        }
        assert!(body.contains("Visible") && body.contains("Work"));
        assert!(!udd.exists(), "the plaintext data is gone");
        let sealed = fs::read(data_path("a1").unwrap()).unwrap();
        assert!(!sealed.windows(10).any(|w| w == b"cookie jar"));

        let (full, key) = unlock("a1", "long enough").unwrap();
        assert_eq!(full.meta.proxy_id.as_deref(), Some("proxy-marker-1"));
        assert_eq!(full.config.get("notes").unwrap(), "secret notes marker");
        assert_eq!(fs::read(open_data("a1", &key).unwrap().join("Default/Cookies")).unwrap(), b"cookie jar");
        seal_data("a1", &key).unwrap();
        assert!(!udd.exists());
        assert_eq!(unlock("a1", "wrong one!").err().unwrap().to_string(), seal::WRONG_PASSWORD);
    }

    #[test]
    fn unprotect_and_a_new_password_round_trip() {
        let _root = store::test_root("protect-cycle");
        profile::write_raw(&plain("a2")).unwrap();
        assert_eq!(protect("a2", "short").err().unwrap().to_string(), TOO_SHORT);
        protect("a2", "first password").unwrap();
        change_password("a2", "first password", "second password").unwrap();
        assert!(unlock("a2", "first password").is_err());
        unprotect("a2", "second password").unwrap();
        let back = profile::load_raw("a2").unwrap();
        assert!(back.meta.lock.is_none());
        assert_eq!(back.meta.proxy_id.as_deref(), Some("proxy-marker-1"));
        assert_eq!(back.config.get("notes").unwrap(), "secret notes marker");
        assert!(!data_path("a2").unwrap().exists());
    }

    #[test]
    fn an_edit_is_sealed_again_and_the_open_part_is_kept() {
        let _root = store::test_root("protect-edit");
        profile::write_raw(&plain("a3")).unwrap();
        protect("a3", "long enough").unwrap();
        let (mut full, key) = unlock("a3", "long enough").unwrap();
        full.config.insert("notes".into(), "edited".into());
        full.meta.folder = "ignored".into();
        save_full(&full, &key).unwrap();
        let stub = profile::load_raw("a3").unwrap();
        assert_eq!(stub.meta.folder, "Work", "the folder belongs to set_folder");
        assert!(!shows_secrets(&stub));
        let (again, _) = unlock("a3", "long enough").unwrap();
        assert_eq!(again.config.get("notes").unwrap(), "edited");
    }
}
