//! Moving the data root. Copy, verify, then delete — a rename cannot cross
//! disks. `launch` checks the lock, so the UI is not the only guard.

use crate::{settings, store};
use anyhow::{Context, Result};
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use tauri::Emitter;

static MIGRATING: AtomicBool = AtomicBool::new(false);
/// A Delete all data that has not finished: its journal
/// (`reset-in-progress.json`) is in the config folder (reset.rs).
static RESET_PENDING: AtomicBool = AtomicBool::new(false);

/// True while the data is being moved or reset, or while a reset that did
/// not finish waits to be undone or finished: nothing starts, runs, writes
/// through the local API, or moves meanwhile.
pub fn in_progress() -> bool {
    MIGRATING.load(Ordering::SeqCst) || RESET_PENDING.load(Ordering::SeqCst)
}

pub fn set_reset_pending(on: bool) {
    RESET_PENDING.store(on, Ordering::SeqCst);
}

pub fn reset_pending() -> bool {
    RESET_PENDING.load(Ordering::SeqCst)
}

/// Delete all data (reset.rs) takes the same lock, so nothing starts, runs
/// or moves while it moves the data aside. False when a move (or a reset)
/// holds it.
pub fn hold() -> bool {
    !MIGRATING.swap(true, Ordering::SeqCst)
}

pub fn release() {
    MIGRATING.store(false, Ordering::SeqCst);
}

/// Tests that hold the lock, or that a held lock would change (the local
/// API's answers), take turns through this.
#[cfg(test)]
pub fn serial() -> std::sync::MutexGuard<'static, ()> {
    static M: std::sync::Mutex<()> = std::sync::Mutex::new(());
    M.lock().unwrap_or_else(|e| e.into_inner())
}

/// What the progress bar reads.
#[derive(Debug, Clone, Serialize)]
pub struct Progress {
    /// "scan" | "copy" | "verify" | "cleanup" | "done"
    pub phase: &'static str,
    pub done: u64,
    pub total: u64,
    pub percent: u8,
    /// File currently being moved, relative to the root.
    pub current: String,
}

/// Directories that move; everything else in the config dir stays where the
/// launcher can always find it. A team copy kept for a new invite moves too,
/// or no sweep would find it again.
const MOVED: &[&str] = &["profiles", "user-data", "extensions", "trash", "team-open", "protected"];

fn emit(app: &tauri::AppHandle, p: Progress) {
    let _ = app.emit("data-migration", p);
}

/// Move the data root to `dst`. Returns the number of files moved.
pub fn run(app: &tauri::AppHandle, dst: &Path) -> Result<u64> {
    run_with(dst, &|p| emit(app, p))
}

/// The move, telling its progress to `emit`.
pub(crate) fn run_with(dst: &Path, emit: &dyn Fn(Progress)) -> Result<u64> {
    if reset_pending() {
        anyhow::bail!("a reset didn’t finish: restart ShardBrowser to undo or finish it first");
    }
    if MIGRATING.swap(true, Ordering::SeqCst) {
        anyhow::bail!("a migration is already running");
    }
    // Looked at under the lock: a launch counts itself before it looks at the lock.
    let out = refuse_while_busy().and_then(|()| do_run(dst, emit));
    MIGRATING.store(false, Ordering::SeqCst);
    if out.is_err() {
        emit(Progress { phase: "done", done: 0, total: 0, percent: 100, current: String::new() });
    }
    out
}

/// A profile starting, or a team profile open here (its copy is under the data folder).
fn refuse_while_busy() -> Result<()> {
    if crate::launch::in_flight() > 0 {
        anyhow::bail!("a profile is starting: try again once it has started");
    }
    // Its data is open under the data folder until the browser closes and it is sealed.
    if crate::space::profile::any_unsealed() {
        anyhow::bail!("a protected profile is running: close it, then try again");
    }
    crate::space::open::refuse_while_open().map_err(anyhow::Error::msg)
}

fn do_run(dst: &Path, emit: &dyn Fn(Progress)) -> Result<u64> {
    let src = store::data_root()?;
    let dst = dst.to_path_buf();
    if dst == src {
        anyhow::bail!("that is already where the data lives");
    }
    if dst.starts_with(&src) {
        anyhow::bail!("the new folder cannot be inside the current one");
    }
    std::fs::create_dir_all(&dst).with_context(|| format!("create {}", dst.display()))?;
    writable(&dst)?;

    emit(Progress { phase: "scan", done: 0, total: 0, percent: 0, current: String::new() });
    let mut files: Vec<(PathBuf, PathBuf)> = Vec::new();
    for name in MOVED {
        let from = src.join(name);
        if from.exists() {
            collect(&from, &dst.join(name), &mut files)?;
        }
    }
    let total = files.len() as u64;
    if total == 0 {
        finish(&dst)?;
        emit(Progress { phase: "done", done: 0, total: 0, percent: 100, current: String::new() });
        return Ok(0);
    }

    let mut done = 0u64;
    for (from, to) in &files {
        if let Some(parent) = to.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::copy(from, to)
            .with_context(|| format!("copy {} → {}", from.display(), to.display()))?;
        done += 1;
        // One event per file is noise on a profile with ten thousand cache
        // entries; the bar only needs to move.
        if done % 25 == 0 || done == total {
            emit(Progress {
                phase: "copy",
                done,
                total,
                percent: ((done * 100) / total) as u8,
                current: from
                    .strip_prefix(&src)
                    .unwrap_or(from)
                    .display()
                    .to_string(),
            });
        }
    }

    emit(Progress { phase: "verify", done, total, percent: 100, current: String::new() });
    for (from, to) in &files {
        let a = std::fs::metadata(from).map(|m| m.len()).unwrap_or(0);
        let b = std::fs::metadata(to).map(|m| m.len()).unwrap_or(u64::MAX);
        if a != b {
            anyhow::bail!("{} did not copy cleanly — nothing was deleted", from.display());
        }
    }

    // Only now is the old copy expendable.
    emit(Progress { phase: "cleanup", done, total, percent: 100, current: String::new() });
    finish(&dst)?;
    for name in MOVED {
        let old = src.join(name);
        if old.exists() {
            let _ = std::fs::remove_dir_all(&old);
        }
    }

    emit(Progress { phase: "done", done, total, percent: 100, current: String::new() });
    Ok(done)
}

/// Persist the new root and point the process at it.
fn finish(dst: &Path) -> Result<()> {
    let mut s = settings::load()?;
    s.data_root = Some(dst.display().to_string());
    settings::save(&s)?;
    store::set_data_root(Some(dst.to_path_buf()));
    Ok(())
}

/// Fail before copying rather than half-way through: an external disk mounted
/// read-only looks like an ordinary directory until something is written.
fn writable(dir: &Path) -> Result<()> {
    let probe = dir.join(".shardx-write-test");
    std::fs::write(&probe, b"ok")
        .with_context(|| format!("{} is not writable", dir.display()))?;
    let _ = std::fs::remove_file(&probe);
    Ok(())
}

fn collect(src: &Path, dst: &Path, out: &mut Vec<(PathBuf, PathBuf)>) -> Result<()> {
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        // The drive's own litter is not ours to carry back and forth.
        if crate::store::is_os_litter(&entry.file_name()) {
            continue;
        }
        let from = entry.path();
        let to = dst.join(entry.file_name());
        match entry.file_type() {
            Ok(t) if t.is_dir() => collect(&from, &to, out)?,
            // Symlinks are followed by copy(); a profile dir has none anyway.
            Ok(_) => out.push((from, to)),
            Err(_) => {}
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    /// I-4: an unfinished reset closes every door the lock closes.
    #[test]
    fn an_unfinished_reset_counts_as_the_lock() {
        let _serial = super::serial();
        super::set_reset_pending(true);
        let locked = super::in_progress();
        super::set_reset_pending(false);
        assert!(locked);
        assert!(!super::in_progress());
    }
}
