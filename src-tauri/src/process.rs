// Tracker for launched ShardX child processes; keyed by profile_id.

use anyhow::Result;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};
use tokio::process::Child;

mod close;
mod since;
mod tie;
pub use tie::{spawn as spawn_tied, Tie};

/// A browser as it was started: its pid, and when that process began, so a
/// later process that takes the pid is never taken for it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Spawned {
    pub pid: u32,
    pub since_ms: u64,
}

impl Spawned {
    /// `pid` as it runs now; None where its start cannot be read.
    pub fn of(pid: u32) -> Option<Spawned> {
        since::started_at(pid).map(|t| Spawned { pid, since_ms: ms(t) })
    }

    /// Still that process.
    fn running(&self) -> bool {
        since::started_at(self.pid).is_some_and(|t| ms(t).abs_diff(self.since_ms) <= 1000)
    }
}

fn ms(t: std::time::SystemTime) -> u64 {
    t.duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64)
}

pub struct Tracker {
    inner: Mutex<HashMap<String, ChildEntry>>,
}

/// What a Stop asks for, and where its answer goes: `true` only once a browser
/// has really been told to close. Stop must not report success when there was
/// nothing left for it to end.
type StopAck = tokio::sync::oneshot::Sender<bool>;

struct ChildEntry {
    pid: u32,
    /// This entry and no later one for the profile: its watch removes only its own.
    gen: u64,
    killer: tokio::sync::mpsc::Sender<StopAck>,
    /// Set once DevToolsActivePort is read; None for UI launches.
    cdp: Option<CdpInfo>,
    /// Process start; serialised as elapsed ms in RunningProfile.
    started_at: Instant,
    /// Library extensions it started with: their folders are not swapped under it.
    extensions: Vec<String>,
}

/// What is done once a browser has closed for good.
pub enum ExitHook {
    /// A profile kept in this machine's own files: its run time is added up.
    Local,
    /// A temporary profile: it goes, its browser data with it.
    Temporary,
    /// A profile kept on a server: its session goes back, then the copy here is wiped.
    Remote(crate::space::open::Handoff),
    /// A personal profile synced to the person's server: kept here as `Local`,
    /// and its session goes back under the lease it was opened with.
    Synced(crate::space::open::Handoff),
    /// A password-protected profile (#127): kept here as `Local`, its browser data sealed again.
    Sealed(crate::space::profile::Unsealed),
}

impl ExitHook {
    /// The lease-held session a server keeps, for a team or a synced profile.
    pub fn session(&self) -> Option<std::sync::Arc<crate::space::open::Session>> {
        match self {
            ExitHook::Remote(h) | ExitHook::Synced(h) => h.session(),
            _ => None,
        }
    }

    /// Kept in this machine's own files.
    pub fn is_local(&self) -> bool {
        matches!(self, ExitHook::Local | ExitHook::Synced(_))
    }
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
            inner: Mutex::new(HashMap::new()),
        }
    }

    /// Take a spawned child + monitor it; entry removed on exit/kill. `udd` is
    /// the folder the browser was started on, watched as given after the child
    /// exits; `extensions` the library ids it was started with.
    pub fn track(
        self: &'static Self,
        profile_id: String,
        mut child: Child,
        udd: PathBuf,
        exit: ExitHook,
        extensions: Vec<String>,
    ) -> u32 {
        static GEN: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let pid = child.id().unwrap_or(0);
        let gen = GEN.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let (tx, mut rx) = tokio::sync::mpsc::channel::<StopAck>(1);

        {
            let mut g = self.inner.lock().unwrap();
            let previous = g.insert(
                profile_id.clone(),
                ChildEntry { pid, gen, killer: tx, cdp: None, started_at: Instant::now(), extensions },
            );
            // Replacing the entry drops the old killer and orphans that browser.
            // `launch` refuses a second launch up front; this is the backstop.
            if let Some(prev) = previous {
                eprintln!(
                    "[tracker] {profile_id} was already tracked as pid {}; stopping it",
                    prev.pid
                );
                // Nobody waits on this one's answer.
                let (ack, _) = tokio::sync::oneshot::channel();
                let _ = prev.killer.try_send(ack);
            }
        }

        // Stop is read for as long as the entry lives — first by `watch_child`
        // and then by `watch_lock`. The profile is offered a Stop button the
        // whole time, so a channel nobody reads is a button that does nothing.
        let started_at = Instant::now();
        tokio::spawn(async move {
            watch_child(&profile_id, &udd, &mut child, &mut rx).await;
            // The process we started is gone — which is not the same as the
            // browser being gone.
            //
            // On Windows a launcher running as administrator makes Chromium
            // restart itself without those rights, and the process we hold
            // exits within a second while the real browser carries on. The
            // same shape appears when a browser from a previous run of the
            // launcher is still up: the new process hands its command line to
            // that one through Chromium's singleton and exits. Either way the
            // entry was dropped here, the profile went back to Idle with its
            // window open, Stop was no longer offered, and starting it again
            // just repeated the handover.
            //
            // The profile's own lock says which it is, and it is the same
            // answer Chromium itself reads. While it is held the profile is
            // running, whoever owns it, so the entry stays and the status
            // stays honest; `after_exit` waits for the browser that is really
            // there rather than for the shell that launched it.
            watch_lock(&profile_id, &udd, &mut rx).await;
            if let Ok(mut g) = Self::shared().inner.lock() {
                if g.get(&profile_id).is_some_and(|e| e.gen == gen) {
                    g.remove(&profile_id);
                }
            }
            match exit {
                ExitHook::Remote(hand) => hand.closed().await,
                ExitHook::Synced(hand) => {
                    after_exit(&profile_id, &ExitHook::Local, started_at.elapsed().as_millis() as u64);
                    hand.closed().await;
                }
                ExitHook::Sealed(open) => {
                    after_exit(&profile_id, &ExitHook::Local, started_at.elapsed().as_millis() as u64);
                    let id = profile_id.clone();
                    match tokio::task::spawn_blocking(move || open.seal()).await {
                        Ok(Ok(())) => {}
                        Ok(Err(e)) => eprintln!("[launcher] {id}: browser data left unsealed: {e:#}"),
                        Err(e) => eprintln!("[launcher] {id}: sealing stopped: {e}"),
                    }
                }
                local => {
                    after_exit(&profile_id, &local, started_at.elapsed().as_millis() as u64);
                }
            }
        });

        pid
    }

    /// Attach CDP to a tracked profile; no-op if the profile already exited.
    pub fn set_cdp(&self, profile_id: &str, cdp: CdpInfo) {
        if let Ok(mut g) = self.inner.lock() {
            if let Some(e) = g.get_mut(profile_id) {
                e.cdp = Some(cdp);
            }
        }
    }

    /// CDP endpoint when the profile was launched with remote debugging.
    /// Whether a child is live for this profile.
    pub fn is_running(&self, profile_id: &str) -> bool {
        self.inner
            .lock()
            .map(|g| g.contains_key(profile_id))
            .unwrap_or(false)
    }

    pub fn cdp(&self, profile_id: &str) -> Option<CdpInfo> {
        self.inner.lock().ok()?.get(profile_id)?.cdp.clone()
    }

    /// Profiles running with library extension `lib_id`.
    pub fn users_of(&self, lib_id: &str) -> Vec<String> {
        let g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let mut out: Vec<String> =
            g.iter().filter(|(_, e)| e.extensions.iter().any(|x| x == lib_id)).map(|(id, _)| id.clone()).collect();
        out.sort();
        out
    }

    pub fn running(&self) -> Vec<RunningProfile> {
        let g = self.inner.lock().unwrap();
        g.iter()
            .map(|(id, e)| RunningProfile {
                profile_id: id.clone(),
                pid: e.pid,
                cdp: e.cdp.clone(),
                uptime_ms: e.started_at.elapsed().as_millis() as u64,
            })
            .collect()
    }

    /// Stop. `false` means nothing was stopped: no such profile, or the
    /// browser holding it could not be reached. Returns as soon as a browser
    /// has been asked to close, not when its window is gone.
    pub async fn kill(&self, profile_id: &str) -> Result<bool> {
        let killer = {
            let g = self.inner.lock().unwrap();
            g.get(profile_id).map(|e| e.killer.clone())
        };
        let Some(k) = killer else { return Ok(false) };
        let (ack, answer) = tokio::sync::oneshot::channel();
        // try_send, never send: a caller must not be left hanging on a
        // monitor that has stopped reading.
        if k.try_send(ack).is_err() {
            return Ok(false);
        }
        Ok(answer.await.unwrap_or(false))
    }

    pub fn shared() -> &'static Tracker {
        static INSTANCE: std::sync::OnceLock<Tracker> = std::sync::OnceLock::new();
        INSTANCE.get_or_init(Tracker::new)
    }
}

/// How long a browser asked to quit has to do it.
const QUIT_WAIT: Duration = Duration::from_secs(5);
/// Then, still up (a page asking to stay), it is asked the ways not tried yet (unix
/// with none left: SIGTERM), and this long after that taken the hard way.
const END_WAIT: Duration = Duration::from_secs(3);

/// The browser socket of `profile_id`: as tracked, else as its folder says.
fn cdp_of(profile_id: &str, udd: &Path) -> Option<String> {
    Tracker::shared().cdp(profile_id).map(|c| c.web_socket_debugger_url).or_else(|| close::cdp_in(udd))
}

/// Our own child, until it is gone. Stop asks it to quit (see `close`), which
/// saves its data so the next launch skips the restore prompt; past QUIT_WAIT
/// it is asked the ways not tried yet, and END_WAIT after that taken the hard way.
async fn watch_child(profile_id: &str, udd: &Path, child: &mut Child, rx: &mut tokio::sync::mpsc::Receiver<StopAck>) {
    let mut asked: Option<tokio::time::Instant> = None;
    // When the second ask was done: CDP may take seconds over it, and END_WAIT is the browser's.
    let mut ended: Option<tokio::time::Instant> = None;
    let mut closing: Option<close::Stop> = None;
    let mut listening = true;
    loop {
        // Armed only once a Stop has been asked for; until then this branch
        // must never fire.
        let due = match (asked, ended) {
            (_, Some(at)) => Some(at + END_WAIT),
            (Some(at), None) => Some(at + QUIT_WAIT),
            (None, None) => None,
        };
        let step = async move {
            match due {
                Some(at) => tokio::time::sleep_until(at).await,
                None => std::future::pending::<()>().await,
            }
        };
        tokio::select! {
            _ = child.wait() => return,
            _ = step => {
                if ended.is_none() {
                    if let Some(s) = closing.as_mut().filter(|_| child.id().is_some()) {
                        s.end().await;
                    }
                    ended = Some(tokio::time::Instant::now());
                    continue;
                }
                let _ = child.kill().await;
                let _ = child.wait().await;
                return;
            }
            stop = rx.recv(), if listening => {
                let Some(ack) = stop else {
                    // Nobody can ask any more. See the child out, and still
                    // take it the hard way if a stop was already asked for.
                    listening = false;
                    continue;
                };
                // Our own child: it ends either way, so this really is a stop, answered
                // before the ask that CDP may hold up.
                let _ = ack.send(true);
                // Asked once: Chromium takes a second signal as a kill.
                if asked.is_none() {
                    if let Some(p) = child.id() {
                        let mut s = close::Stop::new(Some(p), Some(profile_id), cdp_of(profile_id, udd));
                        s.ask().await;
                        closing = Some(s);
                    }
                    asked = Some(tokio::time::Instant::now());
                }
            }
        }
    }
}

/// A browser Stop asked to close: when, and when it was asked again past QUIT_WAIT.
struct Asked {
    stop: close::Stop,
    at: Instant,
    ended: Option<Instant>,
    forced: bool,
}

/// Until the profile's lock is free. Stop is still offered while a browser we
/// did not spawn holds it, so it is still answered here — and what it has to
/// end is that browser, since the process we spawned is already gone.
async fn watch_lock(profile_id: &str, udd: &Path, rx: &mut tokio::sync::mpsc::Receiver<StopAck>) {
    let mut waited = Duration::ZERO;
    let step = if cfg!(test) { Duration::from_millis(100) } else { Duration::from_secs(2) };
    let mut asked: Option<Asked> = None;
    let mut listening = true;
    while crate::reset::lock_is_live(udd) {
        // Asked and still there: asked again, then taken the hard way, as our own
        // child would be (this is read once per pass).
        if let Some(a) = asked.as_mut() {
            match a.ended {
                None if a.at.elapsed() >= QUIT_WAIT => {
                    a.stop.end().await;
                    a.ended = Some(Instant::now());
                    // The lock looked at again first: it may have closed meanwhile.
                    continue;
                }
                Some(at) if !a.forced && at.elapsed() >= END_WAIT => {
                    a.forced = true;
                    // Only while its lock still names it: a pid freed meanwhile may be another process's.
                    if let Some(pid) = a.stop.pid().filter(|p| lock_holder_pid(udd) == Some(*p)) {
                        force_close(pid);
                    }
                }
                _ => {}
            }
        }
        tokio::select! {
            _ = tokio::time::sleep(step) => {
                waited += step;
                // A lock nobody releases must not pin a profile to Running
                // for the rest of the session.
                if waited >= Duration::from_secs(12 * 60 * 60) {
                    return;
                }
            }
            stop = rx.recv(), if listening => {
                let Some(ack) = stop else { listening = false; continue };
                let pid = lock_holder_pid(udd);
                // Already asked: a second signal would end it unsaved.
                let reached = if asked.as_ref().is_some_and(|a| a.stop.pid() == pid) {
                    true
                } else {
                    // No pid (Windows could not read the command lines): the bus and CDP still reach it.
                    let mut s = close::Stop::new(pid, Some(profile_id), cdp_of(profile_id, udd));
                    // Windows found it by its command line: one no route reaches is still forced past END_WAIT.
                    let took = s.ask().await || (cfg!(windows) && pid.is_some());
                    if took {
                        asked = Some(Asked { stop: s, at: Instant::now(), ended: None, forced: false });
                    }
                    took
                };
                // Nothing found to ask, or it would not take the ask: say so
                // rather than report a stop that did not happen.
                if !reached {
                    eprintln!("[tracker] {profile_id} is held by a browser this launcher cannot reach; Stop did nothing");
                }
                let _ = ack.send(reached);
            }
        }
    }
}

/// Ask the browser at `pid` to close itself, the way a user quitting it would:
/// its cookies and session are written out, so nothing set just before is lost.
/// `false` if the ask could not be delivered at all.
fn request_close(pid: u32) -> bool {
    #[cfg(unix)]
    {
        // SAFETY: a signal and nothing more. SIGINT is Chromium's ordinary quit;
        // SIGTERM is "the session is ending" and exits without saving cookies.
        return unsafe { libc::kill(pid as libc::pid_t, libc::SIGINT) } == 0;
    }
    #[cfg(windows)]
    {
        return close::close_windows(pid);
    }
    #[allow(unreachable_code)]
    false
}

/// A browser still up past QUIT_WAIT (a page that asks to stay) with no other route
/// left to ask: SIGTERM, as a logout would. Windows has only the routes.
#[cfg(unix)]
fn end_session(pid: u32) {
    // SAFETY: as in request_close.
    unsafe {
        libc::kill(pid as libc::pid_t, libc::SIGTERM);
    }
}

#[cfg(not(unix))]
fn end_session(_pid: u32) {}

/// Last resort for a browser that would not close when asked.
fn force_close(pid: u32) {
    #[cfg(unix)]
    {
        // SAFETY: as in request_close.
        unsafe { libc::kill(pid as libc::pid_t, libc::SIGKILL) };
    }
    #[cfg(windows)]
    {
        // /T too: a browser that ignored WM_CLOSE leaves its children behind.
        taskkill(&["/F", "/T", "/PID", &pid.to_string()]);
    }
}

#[cfg(windows)]
fn taskkill(args: &[&str]) -> bool {
    use std::os::windows::process::CommandExt;
    std::process::Command::new("taskkill")
        .args(args)
        // 0x08000000 = CREATE_NO_WINDOW — suppress the console flash.
        .creation_flags(0x08000000)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// The browser holding `udd`, for a Stop that has no child of ours left to ask.
///
/// On unix the lock names it: the `SingletonLock` link to "<host>-<pid>", read
/// the way reset.rs reads it to decide the lock is live, so both agree on which
/// process that is. Windows keeps no pid anywhere — `lockfile` is only held
/// open — so the browser is found by the one thing that names this profile,
/// the `--user-data-dir` on its command line. Chromium's own children carry
/// that switch too; the browser is the one without a `--type=`.
///
/// A lock a crash or a reboot left names a pid another process may have now:
/// only one that was running when the lock was written is taken for its browser.
#[cfg(unix)]
fn lock_holder_pid(udd: &Path) -> Option<u32> {
    let lock = udd.join("SingletonLock");
    let target = std::fs::read_link(&lock).ok()?;
    let (host, pid) = target.to_str()?.rsplit_once('-')?;
    if host.is_empty() {
        return None;
    }
    let pid = pid.parse::<u32>().ok().filter(|p| *p > 0)?;
    if runs_on(pid, udd) {
        return Some(pid);
    }
    let written = std::fs::symlink_metadata(&lock).and_then(|m| m.modified()).ok()?;
    // Slack for Linux, which counts a start in clock ticks from a boot time in whole seconds.
    let since = since::started_at(pid)?;
    (since <= written + Duration::from_secs(2)).then_some(pid)
}

/// Linux: its command line names `udd`, as the launcher starts it. A clock set
/// forward after the lock was written moves a start read there past the lock.
#[cfg(target_os = "linux")]
fn runs_on(pid: u32, udd: &Path) -> bool {
    let Ok(raw) = std::fs::read(format!("/proc/{pid}/cmdline")) else { return false };
    let want = format!("--user-data-dir={}", udd.display());
    raw.split(|b| *b == 0).any(|a| a == want.as_bytes())
}

#[cfg(all(unix, not(target_os = "linux")))]
fn runs_on(_pid: u32, _udd: &Path) -> bool {
    false
}

#[cfg(windows)]
fn lock_holder_pid(udd: &Path) -> Option<u32> {
    use std::os::windows::process::CommandExt;
    // Single quotes do not interpolate in PowerShell; a quote inside the path
    // is doubled so it stays part of the string.
    let quoted = udd.to_string_lossy().replace('\'', "''");
    // -match is case-insensitive and takes both shapes the switch is written
    // in: the whole argument quoted, or only its value. The trailing boundary
    // keeps one profile's folder from matching another's that starts with it.
    let script = format!(
        "$u=[regex]::Escape('{quoted}'); \
         Get-CimInstance Win32_Process | Where-Object {{ \
         $_.CommandLine -match ('--user-data-dir=\"?' + $u + '\"?($|\\s)') \
         -and $_.CommandLine -notmatch '--type=' }} | \
         ForEach-Object {{ $_.ProcessId }}"
    );
    let out = std::process::Command::new("powershell")
        .args(["-NoProfile", "-NonInteractive", "-Command", &script])
        .creation_flags(0x08000000)
        .output()
        .ok()?;
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| l.trim().parse::<u32>().ok())
        .find(|p| *p > 0)
}

#[cfg(not(any(unix, windows)))]
fn lock_holder_pid(_udd: &Path) -> Option<u32> {
    None
}

/// The bookkeeping after a browser closes, as its `ExitHook` says. Skipped
/// while the data is being moved or reset (the lock in migrate.rs): a write
/// then would land in a folder that is on its way out, or bring a removed
/// profile back. A temporary profile left behind is swept at the next start
/// (`purge_temporary`). Returns whether it ran.
fn after_exit(profile_id: &str, exit: &ExitHook, elapsed_ms: u64) -> bool {
    if crate::migrate::in_progress() {
        eprintln!("[launcher] {profile_id} closed while the data is being moved or reset; nothing written");
        return false;
    }
    match exit {
        // Bump the persisted total runtime.
        ExitHook::Local => {
            if let Err(e) = crate::space::profile::add_runtime(profile_id, elapsed_ms) {
                eprintln!("[launcher] add_runtime({profile_id}) failed: {e}");
            }
        }
        // Tear down temporary profile (config + udd) on close.
        ExitHook::Temporary => match crate::space::profile::forget_temporary(profile_id) {
            Ok(()) => eprintln!("[launcher] temporary profile {profile_id} deleted on close"),
            Err(e) => eprintln!("[launcher] temporary profile {profile_id} cleanup failed: {e}"),
        },
        // Sent back by the tracker itself, which waits for it.
        ExitHook::Remote(_) | ExitHook::Synced(_) | ExitHook::Sealed(_) => {}
    }
    // An extension update that waited for this browser may go in now.
    crate::space::extensions::profile_closed(profile_id);
    true
}

/// A browser a dead launcher left on `udd`, found by its lock, or as `spawned`
/// when it died before the browser wrote one: asked to close, then taken the hard way.
pub(crate) async fn end_browser(udd: &Path, spawned: Option<Spawned>) {
    let at = udd.to_path_buf();
    let Ok(Some((pid, since))) = tokio::task::spawn_blocking(move || browser_on(&at, spawned)).await else { return };
    // Gone once that process is; where its start cannot be read, once its lock is free.
    let gone = |udd: &Path| match since {
        Some(s) => !s.running(),
        None => !crate::reset::lock_is_live(udd),
    };
    // Still that process, not one that took its pid since: where its start cannot be read, its lock's.
    let still = |udd: &Path| match since {
        Some(s) => s.running(),
        None => lock_holder_pid(udd) == Some(pid),
    };
    // No bus: the launcher it was on is gone.
    let mut stop = close::Stop::new(Some(pid), None, close::cdp_in(udd));
    stop.ask().await;
    let asked = Instant::now();
    let mut ended: Option<Instant> = None;
    let mut forced: Option<Instant> = None;
    while forced.is_none_or(|f| f.elapsed() < Duration::from_secs(5)) {
        if gone(udd) {
            return;
        }
        match ended {
            None if asked.elapsed() >= QUIT_WAIT => {
                stop.end().await;
                ended = Some(Instant::now());
                continue;
            }
            Some(at) if forced.is_none() && at.elapsed() >= END_WAIT => {
                if still(udd) {
                    force_close(pid);
                }
                forced = Some(Instant::now());
            }
            _ => {}
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    eprintln!("[launcher] the browser on {} would not close", udd.display());
}

/// The browser on `udd`, and its start where that can be read.
fn browser_on(udd: &Path, spawned: Option<Spawned>) -> Option<(u32, Option<Spawned>)> {
    if crate::reset::lock_is_live(udd) {
        if let Some(pid) = lock_holder_pid(udd) {
            return Some((pid, Spawned::of(pid)));
        }
    }
    spawned.filter(Spawned::running).map(|s| (s.pid, Some(s)))
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct RunningProfile {
    pub profile_id: String,
    pub pid: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cdp: Option<CdpInfo>,
    /// Milliseconds since the engine was spawned; frontend formats as
    /// "1h 23m" / "12m 30s" / "45s".
    pub uptime_ms: u64,
}

#[cfg(test)]
mod tests {
    /// I3: while a reset holds the lock, a browser's exit writes nothing.
    #[test]
    fn a_close_during_a_reset_writes_nothing() {
        let _serial = crate::migrate::serial();
        assert!(crate::migrate::hold());
        let ran = super::after_exit("not-a-profile", &super::ExitHook::Local, 5);
        crate::migrate::release();
        assert!(!ran);
    }

    /// A Stop while our own child is up: it is asked to close, the answer is
    /// that it was, and the watch ends with the child.
    #[cfg(unix)]
    #[tokio::test]
    async fn stop_ends_our_own_child() {
        let mut child = tokio::process::Command::new("sleep").arg("60").spawn().unwrap();
        let pid = child.id().unwrap();
        let (tx, mut rx) = tokio::sync::mpsc::channel::<super::StopAck>(1);
        let press = async {
            let (ack, answer) = tokio::sync::oneshot::channel();
            tx.try_send(ack).unwrap();
            assert!(answer.await.unwrap(), "our own child is always a real stop");
        };
        let watch = super::watch_child("p-test", std::path::Path::new("/nonexistent"), &mut child, &mut rx);
        tokio::time::timeout(std::time::Duration::from_secs(20), async {
            tokio::join!(watch, press)
        })
        .await
        .expect("the watch must end once the child is gone");
        assert!(!pid_alive(pid), "pid {pid} outlived the stop");
    }

    /// The #109 state: our own process is gone and a browser we did not spawn
    /// holds the lock. Stop still has to reach that browser and still has to
    /// say truthfully whether it stopped anything.
    #[cfg(unix)]
    #[tokio::test]
    async fn stop_ends_the_browser_holding_the_lock() {
        use std::os::unix::fs::symlink;
        let udd = tmp_udd();

        // A process that is nobody's child, the way a browser left from an
        // earlier launcher run is: `sh` spawns it and exits. Its own output
        // goes nowhere, or it would hold this pipe open for the full minute.
        let out = std::process::Command::new("sh")
            .args(["-c", "set -m; sleep 60 </dev/null >/dev/null 2>&1 & echo $!"])
            .output()
            .unwrap();
        let pid: u32 = String::from_utf8_lossy(&out.stdout).trim().parse().unwrap();
        // The host this machine writes: a lock naming another one is not
        // ours and is ignored on purpose (a profile copied from elsewhere).
        let host = crate::reset::this_host().unwrap();
        symlink(format!("{host}-{pid}"), udd.join("SingletonLock")).unwrap();
        assert!(crate::reset::lock_is_live(&udd));

        let (tx, mut rx) = tokio::sync::mpsc::channel::<super::StopAck>(1);
        let mut watch = std::pin::pin!(super::watch_lock("p-test", &udd, &mut rx));

        // #109: while the lock is held the profile is still running, so the
        // watch must not end just because our own process did.
        tokio::select! {
            _ = watch.as_mut() => panic!("the watch ended with the lock still held"),
            _ = tokio::time::sleep(std::time::Duration::from_millis(300)) => {}
        }

        let press = async {
            let (ack, answer) = tokio::sync::oneshot::channel();
            tx.try_send(ack).unwrap();
            assert!(answer.await.unwrap(), "Stop reached nothing while the lock was held");
        };
        tokio::time::timeout(std::time::Duration::from_secs(20), async {
            tokio::join!(watch.as_mut(), press)
        })
        .await
        .expect("the watch must end once the lock is free");
        assert!(!pid_alive(pid), "pid {pid} kept running after Stop");
        std::fs::remove_dir_all(&udd).unwrap();
    }

    /// A stop that reached nothing must not come back as a stop.
    #[cfg(unix)]
    #[test]
    fn a_browser_that_is_gone_cannot_be_closed() {
        let mut c = std::process::Command::new("true").spawn().unwrap();
        let dead = c.id();
        c.wait().unwrap();
        assert!(!super::request_close(dead));
    }

    #[cfg(unix)]
    fn pid_alive(pid: u32) -> bool {
        // SAFETY: signal 0 delivers nothing; it only asks whether the pid is there.
        unsafe { libc::kill(pid as libc::pid_t, 0) == 0 }
    }

    #[cfg(unix)]
    fn tmp_udd() -> std::path::PathBuf {
        let p = std::env::temp_dir().join(format!("shardx-stop-{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    /// After the child exits the watch reads the folder it was started on,
    /// and makes nothing under the data root for it.
    #[cfg(unix)]
    #[tokio::test]
    async fn the_watch_after_exit_uses_the_folder_it_was_given() {
        // Held to the end: the root looked at below is the one the watch saw.
        let root = crate::store::test_root("watch");
        let id = format!("watch-{}", uuid::Uuid::new_v4().simple());
        let udd = tmp_udd();
        let child = tokio::process::Command::new("true").spawn().unwrap();
        super::Tracker::shared().track(id.clone(), child, udd.clone(), super::ExitHook::Local, Vec::new());
        tokio::time::timeout(std::time::Duration::from_secs(20), async {
            while super::Tracker::shared().is_running(&id) {
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("the entry goes once the child and its lock are gone");
        let under_root = root.path.join("user-data").join(&id);
        assert!(!under_root.exists(), "{} was made", under_root.display());
        std::fs::remove_dir_all(&udd).unwrap();
    }

    /// Stop after our own process is gone has to find the browser that is
    /// really there, and the lock is where unix writes its pid.
    #[cfg(unix)]
    #[test]
    fn the_lock_names_the_browser_to_stop() {
        use std::os::unix::fs::symlink;
        let udd = tmp_udd();
        let lock = udd.join("SingletonLock");
        let browser = stray();

        // No lock at all, then the shape Chromium writes (a host with dashes
        // of its own), then three that say nothing usable.
        assert_eq!(super::lock_holder_pid(&udd), None);
        symlink(format!("some-host-{browser}"), &lock).unwrap();
        assert_eq!(super::lock_holder_pid(&udd), Some(browser));
        for bad in [format!("-{browser}"), "some-host-nope".into(), browser.to_string()] {
            std::fs::remove_file(&lock).unwrap();
            symlink(&bad, &lock).unwrap();
            assert_eq!(super::lock_holder_pid(&udd), None, "{bad}");
        }
        end(browser);
        std::fs::remove_dir_all(&udd).unwrap();
    }

    /// A lock left by a crash or a power cut, its pid taken since by another
    /// process: that process is not the browser, and is never signalled.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_stale_lock_never_ends_the_process_that_took_its_pid() {
        use std::os::unix::fs::symlink;
        let udd = tmp_udd();
        let lock = udd.join("SingletonLock");
        let stranger = stray();
        let host = crate::reset::this_host().unwrap();
        symlink(format!("{host}-{stranger}"), &lock).unwrap();
        // Written an hour before that process began, as before a reboot.
        let path = std::ffi::CString::new(lock.as_os_str().as_encoded_bytes()).unwrap();
        let then = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() - 3600;
        let tv = libc::timeval { tv_sec: then as _, tv_usec: 0 };
        // SAFETY: a NUL-terminated path and two times.
        assert_eq!(unsafe { libc::lutimes(path.as_ptr(), [tv, tv].as_ptr()) }, 0);
        assert!(crate::reset::lock_is_live(&udd), "its pid is alive");
        assert_eq!(super::lock_holder_pid(&udd), None);

        super::end_browser(&udd, None).await;
        assert!(pid_alive(stranger), "the process that took the pid was signalled");
        // Its start known from when it was spawned: still not that one.
        let earlier = super::Spawned { pid: stranger, since_ms: then * 1000 };
        super::end_browser(&udd, Some(earlier)).await;
        assert!(pid_alive(stranger));
        end(stranger);
        std::fs::remove_dir_all(&udd).unwrap();
    }

    /// A launcher that died before its browser wrote a lock: the browser is
    /// found as it was spawned, and closed.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_browser_with_no_lock_yet_is_found_as_it_was_spawned() {
        let udd = tmp_udd();
        let browser = stray();
        let spawned = super::Spawned::of(browser).expect("its start");
        super::end_browser(&udd, Some(spawned)).await;
        // Ended; gone once whoever adopted it reaps it.
        for _ in 0..100 {
            if !pid_alive(browser) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert!(!pid_alive(browser), "pid {browser} kept running");
        std::fs::remove_dir_all(&udd).unwrap();
    }

    /// Stop asks with SIGINT, Chromium's own quit that writes its cookies out,
    /// and only once: a second signal would end it unsaved. One that stays up
    /// (a page asking to stay) is ended with SIGTERM, then killed.
    #[cfg(unix)]
    #[tokio::test]
    async fn stop_asks_once_and_with_sigint() {
        let udd = tmp_udd();
        let got = udd.join("signals");
        let script = format!(
            "trap 'echo INT >> {g}' INT; trap 'echo TERM >> {g}' TERM; while :; do sleep 0.05; done",
            g = got.display()
        );
        let mut child = tokio::process::Command::new("sh").args(["-c", &script]).spawn().unwrap();
        // Its traps are set once it has run a beat.
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        let (tx, mut rx) = tokio::sync::mpsc::channel::<super::StopAck>(1);
        let press = async {
            for _ in 0..2 {
                let (ack, answer) = tokio::sync::oneshot::channel();
                tx.send(ack).await.unwrap();
                assert!(answer.await.unwrap());
                tokio::time::sleep(std::time::Duration::from_millis(300)).await;
            }
        };
        tokio::time::timeout(std::time::Duration::from_secs(20), async {
            tokio::join!(super::watch_child("p-test", &udd, &mut child, &mut rx), press)
        })
        .await
        .expect("taken the hard way 8 s after the ask");
        assert_eq!(std::fs::read_to_string(&got).unwrap(), "INT\nTERM\n");
        std::fs::remove_dir_all(&udd).unwrap();
    }

    /// Two entries for one profile: the first one's watch, ending, leaves the
    /// second in place, so its browser can still be stopped.
    #[cfg(unix)]
    #[tokio::test]
    async fn the_end_of_a_replaced_entry_leaves_the_new_one() {
        let _root = crate::store::test_root("replaced");
        let id = format!("twice-{}", uuid::Uuid::new_v4().simple());
        let (a, b) = (tmp_udd(), tmp_udd());
        let t = super::Tracker::shared();
        let first = tokio::process::Command::new("sleep").arg("60").spawn().unwrap();
        let first_pid = first.id().unwrap();
        t.track(id.clone(), first, a.clone(), super::ExitHook::Temporary, Vec::new());
        let second = tokio::process::Command::new("sleep").arg("60").spawn().unwrap();
        let second_pid = second.id().unwrap();
        t.track(id.clone(), second, b.clone(), super::ExitHook::Temporary, Vec::new());
        for _ in 0..100 {
            if !pid_alive(first_pid) {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        assert!(!pid_alive(first_pid), "the one replaced was stopped");
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        assert!(t.is_running(&id), "the first watch took the second entry with it");
        assert!(t.kill(&id).await.unwrap(), "the second browser could not be stopped");
        for _ in 0..100 {
            if !t.is_running(&id) {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        assert!(!t.is_running(&id) && !pid_alive(second_pid));
        std::fs::remove_dir_all(&a).unwrap();
        std::fs::remove_dir_all(&b).unwrap();
    }

    /// The lock moved on to another process after Stop asked: the pid asked is never forced,
    /// as it may be another process's by then.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_pid_the_lock_no_longer_names_is_never_forced() {
        use std::os::unix::fs::symlink;
        let udd = tmp_udd();
        let lock = udd.join("SingletonLock");
        let host = crate::reset::this_host().unwrap();
        // Both ignore SIGINT and SIGTERM, as a browser a page holds does.
        let (asked, next) = (stubborn(), stubborn());
        symlink(format!("{host}-{asked}"), &lock).unwrap();
        let (tx, mut rx) = tokio::sync::mpsc::channel::<super::StopAck>(1);
        let mut watch = std::pin::pin!(super::watch_lock("p-moved", &udd, &mut rx));
        let script = async {
            let (ack, answer) = tokio::sync::oneshot::channel();
            tx.try_send(ack).unwrap();
            assert!(answer.await.unwrap());
            std::fs::remove_file(&lock).unwrap();
            symlink(format!("{host}-{next}"), &lock).unwrap();
            tokio::time::sleep(super::QUIT_WAIT + super::END_WAIT + std::time::Duration::from_secs(2)).await;
        };
        tokio::select! {
            _ = watch.as_mut() => panic!("the watch ended with the lock still held"),
            _ = script => {}
        }
        assert!(pid_alive(asked), "pid {asked} was forced after the lock named {next}");
        end(next);
        tokio::time::timeout(std::time::Duration::from_secs(10), watch).await.expect("the watch ends once the lock is free");
        end(asked);
        std::fs::remove_dir_all(&udd).unwrap();
    }

    /// A process that ignores SIGINT and SIGTERM, nobody's child here.
    #[cfg(unix)]
    fn stubborn() -> u32 {
        let out = std::process::Command::new("sh")
            .args(["-c", "set -m; sh -c \"trap '' INT TERM; while :; do sleep 0.05; done\" </dev/null >/dev/null 2>&1 & echo $!"])
            .output()
            .unwrap();
        let pid = String::from_utf8_lossy(&out.stdout).trim().parse().unwrap();
        // Its traps are set once it has run a beat.
        std::thread::sleep(std::time::Duration::from_millis(200));
        pid
    }

    /// A process nobody here is the parent of, as a browser a dead launcher left.
    #[cfg(unix)]
    fn stray() -> u32 {
        let out = std::process::Command::new("sh")
            .args(["-c", "set -m; sleep 60 </dev/null >/dev/null 2>&1 & echo $!"])
            .output()
            .unwrap();
        String::from_utf8_lossy(&out.stdout).trim().parse().unwrap()
    }

    #[cfg(unix)]
    fn end(pid: u32) {
        // SAFETY: a signal to the test's own stray process.
        unsafe { libc::kill(pid as libc::pid_t, libc::SIGKILL) };
    }
}
