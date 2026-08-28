// Tracker for launched ShardX child processes; keyed by profile_id.

use anyhow::Result;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tokio::process::Child;
use uuid::Uuid;

pub struct Tracker {
    inner: Arc<Mutex<HashMap<String, ChildEntry>>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ProcessState {
    Running,
    Stopping,
}

struct ChildEntry {
    pid: u32,
    killer: tokio::sync::mpsc::Sender<()>,
    /// Unique launch generation; stale monitor tasks must not remove newer entries.
    generation: Uuid,
    state: ProcessState,
    /// Set once DevToolsActivePort is read; None for UI launches.
    cdp: Option<CdpInfo>,
}

/// CDP endpoint for an API-launched profile.
#[derive(Debug, Clone, serde::Serialize)]
pub struct CdpInfo {
    pub port: u16,
    pub http_url: String,
    /// ws://127.0.0.1:<port>/devtools/browser/<id> for Puppeteer/Playwright.
    pub web_socket_debugger_url: String,
}

impl Tracker {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Register a spawned child and monitor it. Only one live child may own a profile id.
    /// On a duplicate, returns the unregistered child so the caller can terminate it.
    pub fn try_track(
        &self,
        profile_id: String,
        mut child: Child,
        temporary: bool,
    ) -> std::result::Result<u32, Child> {
        let pid = child.id().unwrap_or(0);
        let (tx, mut rx) = tokio::sync::mpsc::channel::<()>(1);
        let generation = Uuid::new_v4();

        {
            let mut g = self.inner.lock().unwrap();
            if g.contains_key(&profile_id) {
                return Err(child);
            }
            g.insert(
                profile_id.clone(),
                ChildEntry {
                    pid,
                    killer: tx,
                    generation,
                    state: ProcessState::Running,
                    cdp: None,
                },
            );
        }
        let entries = Arc::clone(&self.inner);

        // Graceful shutdown (SIGTERM / taskkill WM_CLOSE) → 5s → hard kill.
        // Graceful path flushes session state so next launch skips the restore prompt.
        tokio::spawn(async move {
            tokio::select! {
                _ = child.wait() => {}
                _ = rx.recv() => {
                    #[cfg(unix)]
                    {
                        if let Some(p) = child.id() {
                            // SAFETY: libc::kill on a child pid we own.
                            unsafe { libc::kill(p as libc::pid_t, libc::SIGTERM); }
                        }
                    }
                    #[cfg(windows)]
                    {
                        if let Some(p) = child.id() {
                            // taskkill /PID without /F posts WM_CLOSE for clean shutdown.
                            let _ = std::process::Command::new("taskkill")
                                .args(["/PID", &p.to_string()])
                                .stdout(std::process::Stdio::null())
                                .stderr(std::process::Stdio::null())
                                .status();
                        }
                    }
                    let graceful = tokio::time::timeout(
                        std::time::Duration::from_secs(5),
                        child.wait(),
                    ).await;
                    if graceful.is_err() {
                        let _ = child.kill().await;
                        let _ = child.wait().await;
                    }
                }
            }
            let owns_profile = Self::remove_generation(&entries, &profile_id, generation);
            // Tear down temporary profile (config + udd) only when this monitor
            // still owns the profile. A stale monitor must not delete a newer run.
            if temporary && owns_profile {
                match crate::profile::delete(&profile_id) {
                    Ok(()) => {
                        eprintln!("[launcher] temporary profile {profile_id} deleted on close")
                    }
                    Err(e) => {
                        eprintln!("[launcher] temporary profile {profile_id} cleanup failed: {e}")
                    }
                }
            }
        });

        Ok(pid)
    }

    fn remove_generation(
        entries: &Mutex<HashMap<String, ChildEntry>>,
        profile_id: &str,
        generation: Uuid,
    ) -> bool {
        let Ok(mut entries) = entries.lock() else {
            return false;
        };
        if entries.get(profile_id).map(|entry| entry.generation) != Some(generation) {
            return false;
        }
        entries.remove(profile_id);
        true
    }

    /// Attach CDP to a tracked profile. Returns false if the process already exited.
    pub fn set_cdp(&self, profile_id: &str, cdp: CdpInfo) -> bool {
        let Ok(mut entries) = self.inner.lock() else {
            return false;
        };
        let Some(entry) = entries.get_mut(profile_id) else {
            return false;
        };
        entry.cdp = Some(cdp);
        true
    }

    /// CDP endpoint when the profile was launched with remote debugging.
    pub fn cdp(&self, profile_id: &str) -> Option<CdpInfo> {
        self.inner.lock().ok()?.get(profile_id)?.cdp.clone()
    }

    pub fn running(&self) -> Vec<RunningProfile> {
        let g = self.inner.lock().unwrap();
        g.iter()
            .map(|(id, e)| RunningProfile {
                profile_id: id.clone(),
                pid: e.pid,
                cdp: e.cdp.clone(),
            })
            .collect()
    }

    pub async fn kill(&self, profile_id: &str) -> Result<bool> {
        let killer = {
            let mut entries = self.inner.lock().unwrap();
            let Some(entry) = entries.get_mut(profile_id) else {
                return Ok(false);
            };
            if entry.state == ProcessState::Stopping {
                return Ok(true);
            }
            entry.state = ProcessState::Stopping;
            entry.killer.clone()
        };

        // Non-blocking notification keeps repeated/concurrent stop requests idempotent.
        // A disconnected receiver means the monitor is already completing cleanup.
        let _ = killer.try_send(());
        Ok(true)
    }

    /// Wait until the monitor removes a profile entry, bounded by `timeout`.
    pub async fn wait_until_stopped(&self, profile_id: &str, timeout: std::time::Duration) -> bool {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            let tracked = self
                .inner
                .lock()
                .map(|entries| entries.contains_key(profile_id))
                .unwrap_or(false);
            if !tracked {
                return true;
            }
            if tokio::time::Instant::now() >= deadline {
                return false;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    }

    pub fn shared() -> &'static Tracker {
        static INSTANCE: std::sync::OnceLock<Tracker> = std::sync::OnceLock::new();
        INSTANCE.get_or_init(Tracker::new)
    }
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct RunningProfile {
    pub profile_id: String,
    pub pid: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cdp: Option<CdpInfo>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::process::Command;

    #[cfg(unix)]
    async fn test_child() -> Child {
        Command::new("sh")
            .args(["-c", "sleep 1"])
            .spawn()
            .expect("spawn test child")
    }

    #[cfg(windows)]
    async fn test_child() -> Child {
        Command::new("cmd")
            .args(["/C", "timeout /T 1 /NOBREAK >NUL"])
            .spawn()
            .expect("spawn test child")
    }

    #[test]
    fn new_tracker_starts_empty() {
        let tracker = Tracker::new();
        assert!(tracker.running().is_empty());
    }

    #[test]
    fn stale_generation_cannot_remove_newer_entry() {
        let entries = Mutex::new(HashMap::new());
        let old_generation = Uuid::new_v4();
        let new_generation = Uuid::new_v4();
        let (old_tx, _) = tokio::sync::mpsc::channel::<()>(1);
        let (new_tx, _) = tokio::sync::mpsc::channel::<()>(1);

        entries.lock().unwrap().insert(
            "profile".to_string(),
            ChildEntry {
                pid: 1,
                killer: new_tx,
                generation: new_generation,
                state: ProcessState::Running,
                cdp: None,
            },
        );

        assert!(!Tracker::remove_generation(
            &entries,
            "profile",
            old_generation
        ));
        assert_eq!(entries.lock().unwrap().get("profile").unwrap().pid, 1);

        // The old generation's sender is intentionally kept alive to model a stale monitor.
        drop(old_tx);
        assert!(Tracker::remove_generation(
            &entries,
            "profile",
            new_generation
        ));
        assert!(entries.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn duplicate_profile_is_rejected_without_replacing_owner() {
        let tracker = Tracker::new();
        let first_pid = tracker
            .try_track("profile".to_string(), test_child().await, false)
            .expect("first child should be tracked");
        let duplicate = tracker.try_track("profile".to_string(), test_child().await, false);

        let duplicate_child = duplicate.expect_err("duplicate profile must be rejected");
        assert_eq!(tracker.running().len(), 1);
        assert_eq!(tracker.running()[0].pid, first_pid);

        let mut duplicate_child = duplicate_child;
        let _ = duplicate_child.kill().await;
        let _ = duplicate_child.wait().await;
        assert!(tracker.kill("profile").await.unwrap());
    }

    #[tokio::test]
    async fn natural_exit_removes_tracked_entry() {
        let tracker = Tracker::new();
        tracker
            .try_track("profile".to_string(), test_child().await, false)
            .expect("child should be tracked");

        for _ in 0..100 {
            if tracker.running().is_empty() {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        panic!("naturally exited child did not clean up");
    }

    #[tokio::test]
    async fn stop_is_idempotent_and_cleanup_is_eventual() {
        let tracker = Tracker::new();
        tracker
            .try_track("profile".to_string(), test_child().await, false)
            .expect("child should be tracked");

        assert!(tracker.kill("profile").await.unwrap());
        assert!(tracker.kill("profile").await.unwrap());

        assert!(
            tracker
                .wait_until_stopped("profile", std::time::Duration::from_secs(7))
                .await,
            "tracked child did not clean up"
        );
    }
}
