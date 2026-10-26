use crate::space::perm::{self, Cookies, Granted, Start};
use crate::space::{bookmarks, extensions, profile, proxy};
use crate::{
    process::{self, ExitHook, Tracker},
    settings, store,
};
use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use std::process::Stdio;

/// Launch result: OS pid plus CDP endpoint when remote-debugging is on.
pub struct LaunchOutcome {
    pub pid: u32,
    pub cdp: Option<process::CdpInfo>,
    /// Why `cdp` is empty, so a client knows to poll rather than guess.
    pub cdp_error: Option<String>,
}

/// Resolve the ShardX executable from settings, runtime cache, or dev guess.
pub fn resolve_binary() -> Result<PathBuf> {
    if let Some(p) = settings::load()?.browser_path {
        let pb = PathBuf::from(p);
        if pb.exists() {
            return Ok(pb);
        }
    }
    if let Ok(pb) = crate::runtime::binary_path() {
        if pb.exists() {
            return Ok(pb);
        }
    }
    #[cfg(target_os = "macos")]
    let guess = "/Applications/ShardX.app/Contents/MacOS/ShardX";
    #[cfg(target_os = "windows")]
    let guess = "C:\\Program Files\\ShardX\\ShardX.exe";
    #[cfg(target_os = "linux")]
    let guess = "/opt/shardx/shardx";
    let pb = PathBuf::from(guess);
    if pb.exists() {
        return Ok(pb);
    }
    anyhow::bail!("ShardBrowser browser engine not installed yet — open Settings to download, or configure Browser path manually")
}

/// Why a launch is refused while the data lock (migrate.rs) is held.
pub const LOCKED: &str = "ShardBrowser is moving or resetting its data. Try again when that finishes.";
/// Why a launch that was under way stops short of starting the browser.
pub const CANCELLED: &str = "launch cancelled: ShardBrowser is moving or resetting its data.";

/// Launches between their first step and their browser being tracked.
static LAUNCHING: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// Counts one launch from its first step to its return. Taken before the lock
/// is checked, and the reset takes the lock before it reads the count (both
/// SeqCst): a launch the reset does not see is one that then sees the lock.
pub struct Launching(());

impl Launching {
    pub fn start() -> Self {
        LAUNCHING.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Launching(())
    }
}

impl Drop for Launching {
    fn drop(&mut self) {
        LAUNCHING.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
    }
}

/// How many launches are under way (reset.rs refuses while any is).
pub fn in_flight() -> usize {
    LAUNCHING.load(std::sync::atomic::Ordering::SeqCst)
}

/// The last look before the browser starts: a launch that waited on the
/// network while the lock was taken stops here, as cancelled; so does one the
/// person cancelled (`launch_cancel`).
fn may_spawn(profile_id: &str) -> Result<()> {
    if crate::migrate::in_progress() {
        anyhow::bail!("{CANCELLED}");
    }
    check_cancel(profile_id)
}

/// Why a launch the person cancelled ends: the UI tells it apart from a failure.
pub const CANCELLED_BY_USER: &str = "launch cancelled";

/// Profiles whose launch was cancelled while it was still on its way.
static CANCELS: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());

/// Cancel a launch that has not started its browser yet: the slow network
/// steps finish their call, and the launch stops at the next look, at the
/// latest just before the browser would start. A launch that already started
/// its browser is past cancelling; the UI stops that one.
pub fn cancel(profile_id: &str) {
    if let Ok(mut c) = CANCELS.lock() {
        if !c.iter().any(|p| p == profile_id) {
            c.push(profile_id.to_string());
        }
    }
}

fn check_cancel(profile_id: &str) -> Result<()> {
    let hit = CANCELS
        .lock()
        .map(|c| c.iter().any(|p| p == profile_id))
        .unwrap_or(false);
    if hit {
        anyhow::bail!("{CANCELLED_BY_USER}");
    }
    Ok(())
}

/// Clears a profile's cancel when its launch starts and when it ends, however
/// it ends: a cancel is for the launch that was on its way, never for the next
/// one (a cancel that arrives just after a launch returned would otherwise
/// stop the following one).
struct CancelScope(String);

impl CancelScope {
    fn start(profile_id: &str) -> Self {
        if let Ok(mut c) = CANCELS.lock() {
            c.retain(|p| p != profile_id);
        }
        CancelScope(profile_id.to_string())
    }
}

impl Drop for CancelScope {
    fn drop(&mut self) {
        if let Ok(mut c) = CANCELS.lock() {
            c.retain(|p| *p != self.0);
        }
    }
}

/// Profiles with a start under way here.
static STARTING: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());

/// A profile's one start, from the look at the tracker until its browser is tracked.
struct Starting(String);

impl Starting {
    fn claim(profile_id: &str) -> Result<Starting> {
        let mut starting = STARTING.lock().unwrap_or_else(|e| e.into_inner());
        if Tracker::shared().is_running(profile_id) {
            anyhow::bail!("profile {profile_id} is already running");
        }
        if starting.iter().any(|p| p == profile_id) {
            anyhow::bail!("{}", crate::space::open::BUSY);
        }
        starting.push(profile_id.to_string());
        Ok(Starting(profile_id.to_string()))
    }
}

impl Drop for Starting {
    fn drop(&mut self) {
        STARTING.lock().unwrap_or_else(|e| e.into_inner()).retain(|p| *p != self.0);
    }
}

/// Give a profile a `window` block when it claims a screen but names no window.
///
/// The engine turns `window.outer_*` into `--window-size`; with nothing to turn,
/// Chromium sizes the window from the real monitor's work area and the result
/// has no relation to the screen the profile claims. Shape follows the
/// library's own desktop entries: the window fills the work area one pixel
/// short, and the viewport is 87 px shorter again for the browser's chrome.
/// Returns the block it derived, so the caller can also put it in the profile.
fn fill_window_from_screen(
    raw: &mut serde_json::Map<String, serde_json::Value>,
) -> Option<serde_json::Value> {
    let scr = raw.get("screen").and_then(|v| v.as_object())?;
    let w = scr.get("width").and_then(|v| v.as_i64()).unwrap_or(0);
    let h = scr.get("height").and_then(|v| v.as_i64()).unwrap_or(0);
    if w <= 0 || h <= 0 {
        return None;
    }
    let avail_w = scr.get("avail_width").and_then(|v| v.as_i64()).unwrap_or(w).max(1);
    let avail_h = scr.get("avail_height").and_then(|v| v.as_i64()).unwrap_or(h).max(1);

    // Only when it is missing or says nothing: a profile that names its own
    // window was captured that way and is left exactly as it is.
    if profile::names_a_window(raw) {
        return None;
    }

    let outer_h = (avail_h - 1).max(1);
    // Start from the window block the profile already has, if any: it names no
    // size (that is why we are here), but it may carry settings of its own —
    // `keep_focus` among them — and replacing the block whole would drop them
    // on this first start.
    let mut win = raw
        .get("window")
        .and_then(|v| v.as_object())
        .cloned()
        .unwrap_or_default();
    win.insert("outer_width".into(), serde_json::Value::from(avail_w));
    win.insert("outer_height".into(), serde_json::Value::from(outer_h));
    win.insert("inner_width".into(), serde_json::Value::from(avail_w));
    win.insert("inner_height".into(), serde_json::Value::from((outer_h - 87).max(1)));
    let win = serde_json::Value::Object(win);
    raw.insert("window".into(), win.clone());
    eprintln!(
        "[launcher] display: profile named no window — derived {avail_w}x{outer_h} \
         from its claimed {w}x{h} screen"
    );
    Some(win)
}

/// This computer's monitor: width and height in CSS px, and its pixel ratio.
pub type HostScreen = (i64, i64, f64);

/// Whether this launch reports the host's screen, the global switch being
/// "real". A profile whose resolution the operator picked is left out, or the
/// switch would report the host's screen over exactly the one they asked for.
/// A phone is left out too: its screen is the handset's, and this computer's
/// monitor over it is the one reading no real phone could give — a 2560x1440
/// phone is a profile that says it is a phone and is not one.
fn wants_real_screen(
    mode: Option<&str>,
    picked_by_hand: bool,
    config: &serde_json::Map<String, serde_json::Value>,
) -> bool {
    mode == Some("real") && !picked_by_hand && !profile::claims_mobile(config)
}

/// A new Windows or Linux profile claims this computer's screen (`host`, in CSS
/// px, with its pixel ratio), keeping the fingerprint's taskbar insets; macOS,
/// phones, a screen picked by hand (`_meta.screen_custom`) and a profile that
/// already exists (an id) keep their own.
pub fn claim_host_screen(
    raw: &mut serde_json::Map<String, serde_json::Value>,
    host: Option<HostScreen>,
) -> bool {
    let Some((w, h, dpr)) = host.filter(|&(w, h, _)| w > 0 && h > 0) else { return false };
    let meta = raw.get("_meta");
    let by_hand = meta
        .and_then(|m| m.get("screen_custom"))
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    // Changing a live profile's screen would change its fingerprint.
    let exists = meta.and_then(|m| m.get("id")).and_then(|v| v.as_str()).is_some_and(|id| !id.is_empty());
    if exists {
        return false;
    }
    let platform = raw
        .get("navigator")
        .and_then(|n| n.get("platform"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    let desktop = platform.starts_with("win")
        || (platform.starts_with("linux") && !platform.starts_with("linux arm"));
    if by_hand || !desktop || profile::claims_mobile(raw) {
        return false;
    }
    let Some(scr) = raw.get_mut("screen").and_then(|v| v.as_object_mut()) else { return false };
    let n = |k: &str| scr.get(k).and_then(|v| v.as_i64());
    let (fw, fh) = (n("width").unwrap_or(0), n("height").unwrap_or(0));
    if fw <= 0 || fh <= 0 {
        return false;
    }
    let inset_w = (fw - n("avail_width").unwrap_or(fw)).max(0);
    let inset_h = (fh - n("avail_height").unwrap_or(fh)).max(0);
    scr.insert("width".into(), w.into());
    scr.insert("height".into(), h.into());
    scr.insert("avail_width".into(), (w - inset_w).max(1).into());
    scr.insert("avail_height".into(), (h - inset_h).max(1).into());
    // The size is in CSS px at this monitor's scale: the template's ratio with it
    // would make a panel no machine has (1536x864 at 1, for a 1920x1080 at 125%).
    if dpr > 0.0 {
        scr.insert("device_pixel_ratio".into(), dpr.into());
    }
    // The template's window was cut for its own screen: the size goes, and the
    // launch's rule derives one that fits this one (other window keys stay).
    if let Some(win) = raw.get_mut("window").and_then(|v| v.as_object_mut()) {
        for k in ["outer_width", "outer_height", "inner_width", "inner_height"] {
            win.remove(k);
        }
    }
    fill_window_from_screen(raw);
    true
}

/// What a space hands a launch: the profile as it is kept, where its browser
/// data lives, the proxy it goes out through, and what is done when it closes.
pub struct LaunchInput {
    profile_id: String,
    stored: profile::StoredProfile,
    udd: PathBuf,
    proxy: Option<proxy::ProxyEntry>,
    extensions: Vec<PathBuf>,
    /// The folder they live in: a new engine keeps unloaded what it holds that is not passed.
    library: Option<PathBuf>,
    /// Their library ids, held against an update while the browser runs.
    ext_ids: Vec<String>,
    /// The space's bookmarks; the profile's folder picks its own.
    bookmarks: Vec<bookmarks::Bookmark>,
    exit: ExitHook,
    /// Debugging is on only with a Cookies pass: CDP reads every cookie.
    cdp: bool,
    /// A team's profile: this machine's own switches must not reopen what the
    /// team keeps shut.
    team: bool,
}

/// Gathers what starting this profile needs from the space it lives in.
pub fn launch_input(start: &Granted<Start>, cdp: Option<&Granted<Cookies>>) -> Result<LaunchInput> {
    let profile_id = start.profile_id();
    if cdp.is_some_and(|c| c.profile_id() != profile_id) {
        anyhow::bail!("the debugging pass was given for another profile");
    }
    let stored = profile::load_raw(profile_id)?;
    let udd = profile::user_data_dir(profile_id)?;

    // A browser left over from an earlier run of the launcher holds the
    // profile's lock but is in nobody's tracker. Starting another one does
    // not open a second window: Chromium's singleton hands the command line
    // to the browser already there and the new process exits at once, so the
    // profile looked like it refused to start while its window sat open on
    // the desktop. Say what is actually happening instead.
    if crate::reset::lock_is_live(&udd) {
        anyhow::bail!(
            "profile {profile_id} is already open in a browser this launcher did not start \
             — close that window, then start it again"
        );
    }

    let proxy = bound_proxy(&stored, profile_id, proxy::get)?;
    // Extensions from the library; an id the library no longer has is skipped.
    let (ext_ids, extensions) = stored
        .meta
        .extensions
        .iter()
        .filter_map(|id| extensions::load_path(id).map(|p| (id.clone(), p)))
        .unzip();
    let exit = if stored.meta.temporary { ExitHook::Temporary } else { ExitHook::Local };
    let bookmarks = bookmarks::mine().unwrap_or_else(|e| {
        eprintln!("[launcher] bookmarks skipped: {e}");
        Vec::new()
    });
    Ok(LaunchInput {
        profile_id: profile_id.to_string(),
        stored,
        udd,
        proxy,
        extensions,
        library: crate::store::extensions_dir().ok().filter(|d| d.is_dir()),
        ext_ids,
        bookmarks,
        exit,
        cdp: cdp.is_some(),
        team: false,
    })
}

/// The proxy by its id, else the one written on the profile. An id that finds
/// nothing is an error: without it the profile goes out on this computer's IP.
fn bound_proxy(
    stored: &profile::StoredProfile,
    profile_id: &str,
    get: impl FnOnce(&str) -> Result<Option<proxy::ProxyEntry>>,
) -> Result<Option<proxy::ProxyEntry>> {
    let Some(id) = stored.meta.proxy_id.as_deref().filter(|id| !id.trim().is_empty()) else {
        return Ok(stored.meta.inline_proxy.clone());
    };
    // The cause goes in the message: the UI shows only its outermost line.
    let found = get(id).map_err(|e| {
        anyhow::anyhow!("could not read the proxy profile {profile_id} is bound to ({id}): {e:#}")
    })?;
    match found {
        Some(p) => Ok(Some(p)),
        None => anyhow::bail!(
            "profile {profile_id} is bound to a proxy that no longer exists ({id}) \
             — bind another proxy, or none, then start it again"
        ),
    }
}

/// Machine switches a team's profile does not take: each would open a door
/// the team keeps shut (debugging, the data folder, the proxy, extensions).
fn team_extra_args(args: Vec<String>) -> Vec<String> {
    args.into_iter().filter(|a| !opens_a_shut_door(a)).collect()
}

fn opens_a_shut_door(arg: &str) -> bool {
    // Chromium trims each argument first, then reads "--x", "-x" and, on
    // Windows, "/x"; names case-blind there.
    let arg = arg.trim();
    let Some(body) = arg
        .strip_prefix("--")
        .or_else(|| arg.strip_prefix('-'))
        .or_else(|| arg.strip_prefix('/'))
    else {
        return false;
    };
    let name = body.split('=').next().unwrap_or("").to_ascii_lowercase();
    name.starts_with("remote-debugging-")
        || name.starts_with("proxy-")
        || matches!(
            name.as_str(),
            "user-data-dir"
                | "no-proxy-server"
                | "load-extension"
                | "disable-extensions-except"
                | "fingerprint-profile"
        )
}

pub async fn launch_profile(
    profile_id: &str,
    enable_cdp: bool,
    headless: bool,
) -> Result<LaunchOutcome> {
    launch_profile_synced(profile_id, enable_cdp, headless, None, 0, "").await
}

/// How the browser is started, beyond what its profile says.
struct LaunchOpts<'a> {
    enable_cdp: bool,
    headless: bool,
    sync_group: Option<&'a str>,
    bus_port: u16,
    bus_token: &'a str,
    /// The space a run started in: the start goes there or nowhere.
    space: Option<&'a crate::space::Ticket>,
}

/// A run's start: with debugging, in the space the run started in. In a team
/// that asks Start and Cookies of the profile.
#[cfg_attr(not(feature = "automation"), allow(dead_code))]
pub async fn launch_for_run(
    space: &crate::space::Ticket,
    profile_id: &str,
    bus_port: u16,
    bus_token: &str,
) -> Result<LaunchOutcome> {
    let opts = LaunchOpts { enable_cdp: true, headless: false, sync_group: None, bus_port, bus_token, space: Some(space) };
    launch_using(resolve_binary, profile_id, &opts).await
}

/// As `launch_profile`, but joins the browser to a synchronisation group:
/// every profile launched under the same `sync_group` mirrors input.
pub async fn launch_profile_synced(
    profile_id: &str,
    enable_cdp: bool,
    headless: bool,
    sync_group: Option<&str>,
    bus_port: u16,
    bus_token: &str,
) -> Result<LaunchOutcome> {
    let opts = LaunchOpts { enable_cdp, headless, sync_group, bus_port, bus_token, space: None };
    launch_using(resolve_binary, profile_id, &opts).await
}

/// The launch, with the engine found by `binary` (tests hand in their own).
async fn launch_using(
    binary: impl FnOnce() -> Result<PathBuf>,
    profile_id: &str,
    opts: &LaunchOpts<'_>,
) -> Result<LaunchOutcome> {
    // Counted from here to the return, before the slow network checks.
    let _launching = Launching::start();
    let _cancel_scope = CancelScope::start(profile_id);
    // Every door (UI, local API, a run's own starts) comes through here:
    // nothing starts while the data is being moved or reset.
    if crate::migrate::in_progress() {
        anyhow::bail!("{LOCKED}");
    }
    // Counted above before this look, so a switch either waits for this launch or is seen here.
    if crate::space::switching() {
        anyhow::bail!("{}", crate::space::LAUNCH_WHILE_SWITCHING);
    }
    // Same order against an engine update: its swap sees this launch, or this sees the swap.
    if crate::runtime::swapping() {
        anyhow::bail!("{}", crate::runtime::UPDATING);
    }
    // Asked after the count above, so the space cannot move under it from here.
    if let Some(t) = opts.space {
        t.still()?;
    }
    // Open on another of the person's computers: refused before anything here is read.
    if crate::team::active_team().is_none() {
        crate::space::sync::refuse_if_open_elsewhere(profile_id).await?;
    }
    // One browser per profile: two children sharing a user-data dir corrupt each other's
    // state, and the second displaces the first in the tracker, leaving it unstoppable.
    // Held to the return, so a second start meanwhile is refused too.
    let _one = Starting::claim(profile_id)?;
    // The passes first: a profile this door may not start is not there, engine or not.
    let mut _held = None;
    let (bin, input) = match crate::team::active_team() {
        Some(team) => {
            // Just after the launcher started its lists may not be read yet: a start from any door waits for them.
            crate::space::ready().await;
            let (start, cookies) = crate::space::open::passes(&team, profile_id, opts.enable_cdp).await?;
            let bin = binary()?;
            // Asked of the server: a lock or a cancel that came meanwhile stops it here.
            may_spawn(profile_id)?;
            (bin, team_input(&team, &start, cookies.as_ref()).await?)
        }
        None => {
            let start = perm::start(profile_id)?;
            let cookies = if opts.enable_cdp { Some(perm::cookies(profile_id)?) } else { None };
            let bin = binary()?;
            // An extension swap is waited out; none starts until the browser is tracked.
            _held = Some(extensions::begin_launch(profile_id).await?);
            let mut input = launch_input(&start, cookies.as_ref())?;
            // Synced: under the lease of the person's space, its session brought up to date.
            if let Some(hand) = crate::space::sync::open_for_start(profile_id, &input.udd, opts.enable_cdp).await? {
                input.exit = ExitHook::Synced(hand);
                may_spawn(profile_id)?;
            }
            (bin, input)
        }
    };
    start_browser(&bin, input, opts).await
}

/// A team profile: its session downloaded under a lease into a folder of its
/// own, the proxy the server sent with it, and the team's extensions and
/// bookmarks, never this machine's.
async fn team_input(team: &str, start: &Granted<Start>, cdp: Option<&Granted<Cookies>>) -> Result<LaunchInput> {
    if cdp.is_some_and(|c| c.profile_id() != start.profile_id()) {
        anyhow::bail!("the debugging pass was given for another profile");
    }
    let opened = crate::space::open::open_team(team, start, cdp.is_some()).await?;
    let id = start.profile_id();
    let name = opened.stored.config.get("name").and_then(|v| v.as_str()).filter(|n| !n.trim().is_empty()).unwrap_or(id);
    let extensions = extensions::unpack_for_team(team, (id, name), &opened.stored.meta.extensions, &opened.ext).await;
    Ok(LaunchInput {
        profile_id: start.profile_id().to_string(),
        stored: opened.stored,
        udd: opened.udd,
        proxy: opened.proxy,
        extensions,
        library: Some(opened.ext).filter(|d| d.is_dir()),
        // Unpacked into the team copy, not the library.
        ext_ids: Vec::new(),
        bookmarks: bookmarks::for_start(team, id).await,
        exit: ExitHook::Remote(opened.hand),
        cdp: cdp.is_some(),
        team: true,
    })
}

/// A launch with an engine of the test's own.
#[cfg(test)]
pub(crate) async fn launch_with(engine: PathBuf, profile_id: &str, enable_cdp: bool) -> Result<LaunchOutcome> {
    let opts = LaunchOpts { enable_cdp, headless: false, sync_group: None, bus_port: 0, bus_token: "", space: None };
    launch_using(|| Ok(engine), profile_id, &opts).await
}

/// What the command line is made from once the slow steps are done.
struct Ready<'a> {
    fp_file: &'a Path,
    icon: Option<&'a Path>,
    /// The profile as the engine reads it: no `_meta`, "auto" resolved.
    raw: &'a serde_json::Map<String, serde_json::Value>,
    proxy_udp_ok: bool,
    proxy_public_ip: Option<&'a str>,
    use_real_screen: bool,
    /// Linux with an X display, where windows are pinned to X11.
    x11: bool,
    /// The engine loads --load-extension under Enhanced Safe Browsing too (patch 0092).
    ext_alone: bool,
}

async fn start_browser(bin: &Path, input: LaunchInput, opts: &LaunchOpts<'_>) -> Result<LaunchOutcome> {
    // Read off the engine binary while the proxy is probed; once per engine version.
    let ext_alone = (!input.extensions.is_empty() || input.library.is_some()).then(|| {
        let bin = bin.to_path_buf();
        tokio::task::spawn_blocking(move || crate::runtime::engine_loads_extensions_alone(&bin))
    });
    // Live UDP probe; QUIC/WebRTC gating uses current capability not stale cache.
    let proxy_udp_ok = if let Some(p) = input.proxy.as_ref() {
        if matches!(p.kind, proxy::ProxyKind::Socks5) {
            match proxy::probe_udp(p).await {
                Ok(ms) => {
                    eprintln!("[launcher] UDP relay OK ({ms} ms) for proxy {}", p.host);
                    true
                }
                Err(e) => {
                    // A failed probe does not mean the proxy has no relay. Some
                    // VPNs pass UDP for one application and drop it for another,
                    // and the launcher can be on the losing side while the
                    // browser it starts is not — measured on a Mac where the
                    // browser got STUN replies and a plain binary got none.
                    // Treating that as "no UDP" took QUIC and proxied WebRTC
                    // away from a browser that could have used both.
                    if !proxy::can_send_udp_directly().await {
                        crate::notify_warning(
                            "This computer would not let the launcher send UDP, so \
                             whether the proxy relays it could not be checked. The \
                             browser is started with UDP left on — if it is a VPN \
                             doing this, the browser may well be allowed where the \
                             launcher is not.",
                        );
                        eprintln!(
                            "[launcher] UDP probe failed for proxy {} ({e}), and this \
                             process cannot send UDP at all — leaving UDP enabled",
                            p.host
                        );
                        true
                    } else {
                        let cached = proxy::latest_test(&p.id).and_then(|s| s.udp_ms).is_some();
                        eprintln!(
                            "[launcher] UDP probe failed for proxy {} ({e}) while this \
                             process CAN send UDP, so the proxy has no relay; cached={cached}",
                            p.host
                        );
                        cached
                    }
                }
            }
        } else {
            false
        }
    } else {
        false
    };

    // "Real screen" is a global switch, and it exists for desktop profiles.
    let s = settings::load()?;
    let use_real_screen = wants_real_screen(
        s.screen_resolution_mode.as_deref(),
        input.stored.meta.screen_custom,
        &input.stored.config,
    );

    // Strip `_meta` wrapper and resolve "auto" sentinels before serialising.
    let mut raw = engine_config(&input.stored);
    resolve_auto_fields(&mut raw, input.proxy.as_ref()).await;
    // Profiles saved before the library carried the block would otherwise offer
    // this computer's voices from an Android handset. Filled here rather than at
    // save so a profile that already exists is fixed on its next start.
    crate::ensure_phone_voices(&mut raw);
    // The proxy probe and the geo lookup are the slow part: a cancel made
    // while they ran stops here, before anything is written.
    check_cancel(&input.profile_id)?;

    // A window to go with the claimed screen. Without one the engine passes no
    // --window-size, and Chromium falls back to the REAL monitor's work area —
    // a profile claiming 1920x1080 then opens in a window sized for the host
    // and sitting outside its own claimed screen. Profiles saved before the
    // editor wrote this block are missing it, so it is derived here.
    if let Some(win) = fill_window_from_screen(&mut raw) {
        // And kept: the block follows from the profile's own screen, nothing
        // about this machine, so writing it back fixes the profile for good
        // instead of re-deriving it on every start. Only a profile kept in
        // this machine's own files is written: temporary profiles are deleted
        // on close. A failure here only costs the correction; this launch
        // already has its window.
        if input.exit.is_local() && !crate::migrate::in_progress() {
            match profile::fill_missing_window(&input.profile_id, win) {
                Ok(true) => eprintln!("[launcher] display: wrote the window block into the profile"),
                Ok(false) => {}
                Err(e) => eprintln!("[launcher] display: could not store the window block: {e}"),
            }
        }
    }

    // Whenever the profile's own screen is the one being used, it still has to
    // fit this monitor: the editor offers only sizes the display can hold, but
    // the profile may since have moved to a smaller machine, and a window
    // cannot be bigger than the screen it opens on. A display that can hold the
    // claim leaves it untouched, so this costs a hand-picked resolution nothing.
    if !use_real_screen {
        if let Some(w) = crate::main_window() {
            crate::clamp_screen_to_real_display(&w, &mut raw);
        }
    }
    let json = serde_json::to_string(&raw).context("serialize profile")?;

    // Pass fingerprint by file path — inline JSON overflows Windows' 32767-char CreateProcess limit.
    let fp_file = input.udd.join("fingerprint.json");
    std::fs::write(&fp_file, &json).context("write fingerprint.json")?;

    // Keep whatever CDM an engine has already fetched for itself, then hand it
    // to this profile. The first profile to open a DRM page downloads one; every
    // profile after that starts with it in place.
    if let Err(e) = harvest_widevine(&input.udd) {
        eprintln!("[launcher] widevine harvest skipped: {e}");
    }
    if let Err(e) = install_widevine(&input.udd) {
        eprintln!("[launcher] widevine pre-warm skipped: {e}");
    }

    // Per-profile window icon. A failure here is cosmetic, never fatal.
    let color = input.stored.meta.color.clone().filter(|c| !c.trim().is_empty());
    // Tests do not draw into the real runtime folder.
    let icon = match crate::runtime::runtime_dir().and_then(|dir| {
        if cfg!(test) {
            anyhow::bail!("no icons in tests");
        }
        // Display name lives in the config, not in _meta; id as fallback.
        let name = input
            .stored
            .config
            .get("name")
            .and_then(|v| v.as_str())
            .filter(|s| !s.trim().is_empty())
            .unwrap_or(&input.profile_id);
        crate::profile_icon::ensure_icon(&dir, name, color.as_deref())
    }) {
        Ok(path) => Some(path),
        Err(e) => {
            eprintln!("[launcher] profile icon unavailable: {e:#}");
            None
        }
    };

    // Folder bookmarks, written before the browser reads the file: the space's
    // own, so this machine's are never written into a team's profile.
    let bookmarked = bookmarks::apply_to_profile(&input.udd, &input.stored.meta.folder, &input.bookmarks);
    match bookmarked {
        Ok(n) if n > 0 => eprintln!("[launcher] {n} folder bookmark(s) applied"),
        Ok(_) => {}
        Err(e) => eprintln!("[launcher] bookmarks skipped: {e}"),
    }

    // Live geo for ICE-candidate spoofing, cached snapshot as fallback.
    let proxy_public_ip: Option<String> = if let Some(p) = input.proxy.as_ref() {
        match proxy::geo_check(p, None).await {
            Ok(g) if !g.ip.is_empty() => Some(g.ip),
            _ => proxy::latest_test(&p.id)
                .map(|s| s.ip)
                .filter(|ip| !ip.is_empty()),
        }
    } else {
        None
    };
    let ext_alone = match ext_alone {
        Some(asked) => asked.await.unwrap_or(false),
        None => false,
    };

    let args = command_line(
        &input,
        &Ready {
            fp_file: &fp_file,
            icon: icon.as_deref(),
            raw: &raw,
            proxy_udp_ok,
            proxy_public_ip: proxy_public_ip.as_deref(),
            use_real_screen,
            x11: cfg!(target_os = "linux") && std::env::var_os("DISPLAY").is_some(),
            ext_alone,
        },
        opts,
        &s,
    );
    let LaunchInput { profile_id, udd, exit, cdp: enable_cdp, ext_ids, .. } = input;
    let remote = exit.session();
    // A team's profile has no launch count of its own here.
    let touch = !matches!(exit, ExitHook::Remote(_));

    let mut cmd = tokio::process::Command::new(bin);
    cmd.args(&args);
    // CDP: port=0 makes Chrome pick free port and write DevToolsActivePort.
    if enable_cdp {
        let _ = std::fs::remove_file(udd.join("DevToolsActivePort"));
    }
    cmd.stdout(Stdio::null()).stderr(Stdio::null());
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        // 0x08000000 = CREATE_NO_WINDOW — suppress the brief console flash
        // when a Tauri GUI app spawns the engine binary.
        cmd.creation_flags(0x08000000);
    }
    may_spawn(&profile_id)?;
    if let Some(s) = &remote {
        s.still_wanted()?;
    }
    // A profile kept on a server ends with the launcher where the OS allows;
    // its reaper (started when it was opened) closes it everywhere.
    let (child, tie) = match &remote {
        Some(_) => process::spawn_tied(cmd).await.context("start the ShardBrowser engine")?,
        None => (cmd.spawn().context("start the ShardBrowser engine")?, process::Tie::default()),
    };
    let pid = Tracker::shared().track(profile_id.clone(), child, udd.clone(), exit, ext_ids);

    if let Some(s) = &remote {
        s.spawned(tie, pid);
    }
    if touch {
        profile::touch_launched(&profile_id, None)?;
    }

    let mut cdp_error = None;
    let cdp = if enable_cdp {
        match read_devtools_endpoint(&udd).await {
            Some(c) => {
                eprintln!("[launcher] CDP ready for {profile_id}: {}", c.web_socket_debugger_url);
                Tracker::shared().set_cdp(&profile_id, c.clone());
                Some(c)
            }
            None => {
                let msg = format!(
                    "the browser did not report a debugging port within {}s; \
                     read DevToolsActivePort in the profile's user-data dir, \
                     or ask this endpoint again",
                    CDP_WAIT.as_secs()
                );
                eprintln!("[launcher] CDP: {msg}");
                cdp_error = Some(msg);
                None
            }
        }
    } else {
        None
    };

    Ok(LaunchOutcome { pid, cdp, cdp_error })
}

/// The profile as the engine reads it, without `_meta`. `ui_language` goes only as the
/// opt-in "profile" (#81); absent, the engine keeps this computer's language.
fn engine_config(stored: &profile::StoredProfile) -> serde_json::Map<String, serde_json::Value> {
    let mut raw = stored.config.clone();
    raw.remove("_meta");
    if raw.get("ui_language").and_then(|v| v.as_str()) != Some("profile") {
        raw.remove("ui_language");
    }
    raw
}

/// The engine's command line, in the order the switches have always gone.
fn command_line(
    input: &LaunchInput,
    ready: &Ready,
    opts: &LaunchOpts,
    s: &settings::Settings,
) -> Vec<String> {
    let mut args: Vec<String> = Vec::new();
    args.push(format!("--fingerprint-profile={}", ready.fp_file.display()));
    args.push(format!("--user-data-dir={}", input.udd.display()));
    if let Some(icon) = ready.icon {
        args.push(format!("--shardx-profile-icon={}", icon.display()));
    }
    // Same accent behind the profile-name pill in the omnibox, so the icon and
    // the window never disagree about a profile's colour. Left off for "auto":
    // the pill then keeps the toolbar colour, which is the browser's own
    // default and follows the theme.
    if let Some(c) = input.stored.meta.color.as_deref().filter(|c| !c.trim().is_empty()) {
        args.push(format!("--shardx-profile-pill-color={c}"));
    }
    args.push("--no-first-run".into());

    // An older engine drops --load-extension under Enhanced Safe Browsing and needs the same
    // list as --disable-extensions-except, which also hides the profile's own extensions.
    if !input.extensions.is_empty() {
        let joined = input
            .extensions
            .iter()
            .map(|p| p.display().to_string())
            .collect::<Vec<_>>()
            .join(",");
        // A team's profile keeps the list: adding an extension there takes the Cookies right.
        if !ready.ext_alone || input.team {
            args.push(format!("--disable-extensions-except={joined}"));
        }
        args.push(format!("--load-extension={joined}"));
    }
    // A new engine keeps unloaded what the library holds but this start does not pass.
    if let Some(lib) = input.library.as_ref().filter(|_| ready.ext_alone) {
        args.push(format!("--shardx-extension-library={}", lib.display()));
    }

    // Disable WebGPU when profile omits `webgpu` (matches real Linux Chrome).
    let webgpu_present = ready.raw.get("webgpu").map(|v| !v.is_null()).unwrap_or(false);
    if !webgpu_present {
        args.push("--disable-features=WebGPU".into());
    }

    // Interactive launches: suppress the crash bubble. Restoring the session is
    // the browser's own "On startup" setting — forcing it here overrode it.
    if !opts.headless && !input.cdp {
        args.push("--hide-crash-restore-bubble".into());
    }

    if let Some(p) = input.proxy.as_ref() {
        args.push(format!("--proxy-server={}", p.to_proxy_server_arg()));

        // QUIC: enable only when proxy UDP relay verified; rely on Alt-Svc upgrade path.
        if ready.proxy_udp_ok {
            args.push("--enable-quic".into());
            eprintln!(
                "[launcher] QUIC enabled (Alt-Svc upgrade path): proxy {} UDP relay verified",
                p.host
            );
        } else {
            args.push("--disable-quic".into());
            eprintln!("[launcher] QUIC disabled: proxy {} has no working UDP relay", p.host);
        }
    }

    // WebRTC IP policy: block / tcp_only / auto (auto = relay if UDP, else tcp_only).
    let webrtc_mode = ready.raw.get("webrtc").and_then(|v| v.as_str()).unwrap_or("auto");
    match webrtc_mode {
        "block" => {
            args.push("--force-webrtc-ip-handling-policy=disable_non_proxied_udp".into());
            args.push("--shardx-webrtc-policy=block".into());
            eprintln!("[launcher] WebRTC blocked (servers stripped, relay-only, UDP off)");
        }
        "tcp_only" => {
            args.push("--force-webrtc-ip-handling-policy=disable_non_proxied_udp".into());
            args.push("--shardx-webrtc-policy=tcp_only".into());
            if let Some(ip) = ready.proxy_public_ip {
                args.push(format!("--shardx-webrtc-public-ip={ip}"));
            }
            eprintln!("[launcher] WebRTC: TCP-only (servers stripped, mDNS host only, UDP off)");
        }
        _ => {
            if input.proxy.is_none() {
                // No proxy bound — let WebRTC use the host network natively
                // (real IP shows in ICE candidates, which is what the user wants
                // when they explicitly didn't bind a proxy).
                eprintln!("[launcher] WebRTC auto -> native (no proxy bound)");
            } else if !ready.proxy_udp_ok {
                args.push("--force-webrtc-ip-handling-policy=disable_non_proxied_udp".into());
                args.push("--shardx-webrtc-policy=tcp_only".into());
                if let Some(ip) = ready.proxy_public_ip {
                    args.push(format!("--shardx-webrtc-public-ip={ip}"));
                }
                eprintln!("[launcher] WebRTC auto -> TCP-only (no proxied UDP available)");
            } else {
                eprintln!("[launcher] WebRTC auto -> through proxy UDP relay");
            }
        }
    }

    // Screen resolution mode: presence-only switch to use host monitor. The
    // setting is there for macOS profiles; a profile whose resolution the
    // operator chose is left alone, or the switch would wipe exactly the
    // screen they asked for.
    if ready.use_real_screen {
        args.push("--shardx-real-screen".into());
    }

    // The bus is the process's link to the launcher, not the group's — the page
    // helper reports over it on launches that belong to no group at all.
    if opts.bus_port != 0 {
        args.push(format!("--shardx-bus=127.0.0.1:{}", opts.bus_port));
        args.push(format!("--shardx-bus-token={}", opts.bus_token));
        args.push(format!("--shardx-sync-profile={}", input.profile_id));
    }
    if let Some(group) = opts.sync_group {
        args.push(format!("--shardx-sync-group={group}"));
    }
    if s.helper_enabled {
        // In a group too: the fill travels as a command, not as input, so each
        // window fills with its own generated person.
        args.push("--shardx-helper".into());
    }
    if s.camera_enabled {
        // No value — the picture is chosen in the running browser. Without the
        // switch the machine's own camera answers.
        args.push("--shardx-camera".into());
    }

    if input.cdp {
        args.push("--remote-debugging-port=0".into());
        args.push("--remote-allow-origins=*".into());
        // Gates the Motion domain, which every automated click and keystroke goes
        // through. Free when unused: it is absent from Schema.getDomains and /json/protocol.
        args.push("--shardx-automation".into());
    }

    if opts.headless {
        args.push("--headless=new".into());
    }

    let extra = settings::parse_extra_args(&s.extra_args);
    let extra = if input.team { team_extra_args(extra) } else { extra };

    // Pin X11 (XWayland under Wayland): a Wayland client may not place its own windows,
    // so SetBounds moves nothing. Not without DISPLAY — forcing x11 then opens no window.
    if ready.x11 && !extra.iter().any(|a| a.starts_with("--ozone-platform")) {
        args.push("--ozone-platform=x11".into());
    }

    // Answer media questions the Android way. Gated on the profile claiming a phone
    // as well as on the setting: the flag does nothing in the engine on a desktop one.
    if input.stored.meta.android_media && profile::claims_mobile(&input.stored.config) {
        args.push("--shardx-android-media".into());
    }

    // Operator's own switches, last so they win a repeat.
    args.extend(extra);
    args
}

/// How long a launch waits for the browser to publish its debugging port. Six
/// seconds was not enough for a cold start on Windows with a large profile.
const CDP_WAIT: std::time::Duration = std::time::Duration::from_secs(30);

/// Poll `<udd>/DevToolsActivePort`; line 1 = port, line 2 = ws path.
async fn read_devtools_endpoint(udd: &Path) -> Option<process::CdpInfo> {
    let file = udd.join("DevToolsActivePort");
    let deadline = std::time::Instant::now() + CDP_WAIT;
    while std::time::Instant::now() < deadline {
        if let Ok(txt) = std::fs::read_to_string(&file) {
            let mut lines = txt.lines();
            if let (Some(port_s), Some(path)) = (lines.next(), lines.next()) {
                if let Ok(port) = port_s.trim().parse::<u16>() {
                    return Some(process::CdpInfo {
                        port,
                        http_url: format!("http://127.0.0.1:{port}"),
                        web_socket_debugger_url: format!(
                            "ws://127.0.0.1:{port}{}",
                            path.trim()
                        ),
                    });
                }
            }
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    None
}

/// Resolve "auto" sentinels in profile JSON; with proxy: live → cached → country tag → host warn.
async fn resolve_auto_fields(
    cfg: &mut serde_json::Map<String, serde_json::Value>,
    proxy_opt: Option<&proxy::ProxyEntry>,
) {
    let want_tz_auto = cfg.get("timezone").and_then(|v| v.as_str()) == Some("auto");
    let want_lang_auto = cfg
        .get("navigator")
        .and_then(|n| n.get("language"))
        .and_then(|v| v.as_str())
        == Some("auto");
    let want_geo_auto = matches!(
        cfg.get("geolocation").and_then(|g| g.get("mode")).and_then(|v| v.as_str()),
        Some("auto")
    );

    if !(want_tz_auto || want_lang_auto || want_geo_auto) {
        return;
    }

    eprintln!(
        "[launcher] resolving auto fields (tz={} lang={} geo={} proxy={})",
        want_tz_auto,
        want_lang_auto,
        want_geo_auto,
        proxy_opt.map(|p| format!("{}:{}", p.host, p.port)).unwrap_or_else(|| "(direct)".into()),
    );

    // ---- geo source ----
    let mut source = "";
    let geo: Option<proxy::GeoInfo> = match proxy_opt {
        Some(p) => {
            match proxy::geo_check_via(Some(p), None).await {
                Ok(g) => { source = "proxy-live"; Some(g) }
                Err(e) => {
                    eprintln!("[launcher] proxy geo failed: {e} — falling back to cached snapshot");
                    if let Some(snap) = proxy::latest_test(&p.id) {
                        if !snap.country_code.is_empty() || !snap.timezone.is_empty() {
                            source = "cached-snapshot";
                            Some(proxy::GeoInfo {
                                ip: snap.ip,
                                country: snap.country,
                                country_code: snap.country_code,
                                region: snap.region,
                                city: snap.city,
                                isp: snap.isp,
                                timezone: snap.timezone,
                                latitude: snap.latitude,
                                longitude: snap.longitude,
                                provider: snap.provider,
                            })
                        } else { None }
                    } else { None }
                    .or_else(|| {
                        if !p.country.is_empty() {
                            source = "country-tag";
                            Some(proxy::GeoInfo {
                                ip: String::new(),
                                country: String::new(),
                                country_code: p.country.clone(),
                                region: String::new(),
                                city: String::new(),
                                isp: String::new(),
                                timezone: String::new(),
                                latitude: 0.0,
                                longitude: 0.0,
                                provider: String::new(),
                            })
                        } else { None }
                    })
                }
            }
        }
        None => {
            match proxy::geo_check_via(None, None).await {
                Ok(g) => { source = "direct-live"; Some(g) }
                Err(e) => {
                    eprintln!("[launcher] direct geo failed: {e} — falling back to host TZ/locale");
                    None
                }
            }
        }
    };

    let host_warn = || {
        if proxy_opt.is_some() {
            crate::notify_warning(
                "Could not read the proxy's location. Rather than hand a page \
                 this computer's timezone, the profile starts on UTC — which \
                 few people really keep. Test the proxy again, or set the \
                 timezone on the profile yourself.",
            );
        }
    };

    // ---- concrete tz/locale/lat/lng ----
    let (resolved_tz, resolved_locale, resolved_lat, resolved_lng) = match geo {
        Some(ref g) => {
            let tz = if !g.timezone.is_empty() {
                g.timezone.clone()
            } else {
                proxy::country_to_timezone(&g.country_code).to_string()
            };
            let locale = proxy::country_to_locale(&g.country_code).to_string();
            let lat = if g.latitude != 0.0 { Some(g.latitude) } else { None };
            let lng = if g.longitude != 0.0 { Some(g.longitude) } else { None };
            (tz, locale, lat, lng)
        }
        None => {
            host_warn();
            if proxy_opt.is_some() {
                // A profile behind a proxy must never answer with this
                // computer's clock. Before, the Windows branch had no way to
                // read the host zone and landed on UTC by accident; now that
                // it can read it, handing it over would be a real leak of
                // where the operator is. UTC is wrong too, but it is not
                // anybody's address.
                ("UTC".into(), "en-US".into(), None, None)
            } else {
                (
                    host_timezone().unwrap_or_else(|| "UTC".into()),
                    host_locale().unwrap_or_else(|| "en-US".into()),
                    None,
                    None,
                )
            }
        }
    };

    eprintln!(
        "[launcher] resolved tz={resolved_tz} locale={resolved_locale} (source={source})"
    );

    if want_tz_auto {
        cfg.insert("timezone".into(), serde_json::Value::String(resolved_tz.clone()));
    }

    if want_lang_auto {
        let base = resolved_locale.split('-').next().unwrap_or(&resolved_locale).to_string();
        let accept = if resolved_locale == "en-US" {
            "en-US,en;q=0.9".to_string()
        } else {
            format!("{resolved_locale},{base};q=0.9,en-US;q=0.8,en;q=0.7")
        };
        let languages = if resolved_locale == "en-US" {
            vec![
                serde_json::Value::String("en-US".into()),
                serde_json::Value::String("en".into()),
            ]
        } else {
            vec![
                serde_json::Value::String(resolved_locale.clone()),
                serde_json::Value::String(base),
                serde_json::Value::String("en-US".into()),
                serde_json::Value::String("en".into()),
            ]
        };
        if let Some(nav) = cfg.get_mut("navigator").and_then(|v| v.as_object_mut()) {
            nav.insert("language".into(), serde_json::Value::String(resolved_locale.clone()));
            nav.insert("accept_language".into(), serde_json::Value::String(accept));
            nav.insert("languages".into(), serde_json::Value::Array(languages));
        }
        // Always overwrite icu_locale so it matches resolved navigator.language.
        cfg.insert("icu_locale".into(), serde_json::Value::String(resolved_locale));
    }

    if want_geo_auto {
        if let (Some(lat), Some(lng)) = (resolved_lat, resolved_lng) {
            cfg.insert(
                "geolocation".into(),
                serde_json::json!({
                    "mode": "manual",
                    "latitude": lat,
                    "longitude": lng,
                    "accuracy": 50.0,
                }),
            );
        } else {
            cfg.remove("geolocation");
        }
    }
}

/// Version string as numbers, so "4.10.2891.0" sorts above "4.9.9999.0".
fn cdm_version(v: &str) -> Vec<u64> {
    v.split('.').map(|p| p.parse().unwrap_or(0)).collect()
}

fn cdm_version_of(dir: &Path) -> Option<String> {
    let text = std::fs::read_to_string(dir.join("manifest.json")).ok()?;
    let manifest: serde_json::Value = serde_json::from_str(&text).ok()?;
    manifest.get("version")?.as_str().map(String::from)
}

/// Take the CDM the engine downloaded for itself into the cache, so the next
/// profile does not have to download its own. The engine's component updater is
/// the only thing that writes `<udd>/WidevineCdm/<version>/`, and it keeps that
/// copy current — which is why the CDM is not shipped from the CDN at all: its
/// version would have to be chased there forever.
fn harvest_widevine(udd: &Path) -> Result<()> {
    let root = udd.join("WidevineCdm");
    if !root.exists() {
        return Ok(());
    }
    let mut newest: Option<(Vec<u64>, String, PathBuf)> = None;
    for entry in std::fs::read_dir(&root)?.flatten() {
        let p = entry.path();
        if !p.is_dir() {
            continue;
        }
        let Some(version) = cdm_version_of(&p) else { continue };
        let parsed = cdm_version(&version);
        if newest.as_ref().is_none_or(|(best, _, _)| parsed > *best) {
            newest = Some((parsed, version, p));
        }
    }
    let Some((found, version, src)) = newest else { return Ok(()) };

    let cache = store::widevine_cache_dir()?;
    if let Some(have) = cdm_version_of(&cache) {
        if cdm_version(&have) >= found {
            return Ok(());
        }
    }
    // Into a sibling first: a half-copied cache is worse than none, because
    // install_widevine reads it without knowing it is unfinished.
    let staging = cache.with_extension("incoming");
    let _ = std::fs::remove_dir_all(&staging);
    copy_dir_recursive(&src, &staging)?;
    let _ = std::fs::remove_dir_all(&cache);
    std::fs::rename(&staging, &cache)?;
    eprintln!("[launcher] widevine cached from a profile: {version}");
    Ok(())
}

/// Copy cached Widevine CDM into `<udd>/WidevineCdm/<version>/` (versioned layout
/// required by Chromium's DefaultComponentInstaller). No-op if cache absent.
fn install_widevine(udd: &Path) -> Result<()> {
    let src = store::widevine_cache_dir()?;
    if !src.exists() {
        anyhow::bail!("cache dir absent ({})", src.display());
    }
    let manifest_path = src.join("manifest.json");
    if !manifest_path.exists() {
        anyhow::bail!("cache missing manifest.json — re-seed from a real Chrome");
    }
    let manifest_text = std::fs::read_to_string(&manifest_path)?;
    let manifest: serde_json::Value = serde_json::from_str(&manifest_text)
        .context("parse widevine manifest.json")?;
    let version = manifest
        .get("version")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("widevine manifest missing `version`"))?;

    let widevine_root = udd.join("WidevineCdm");
    let versioned = widevine_root.join(version);
    if versioned.exists() {
        return Ok(());
    }
    // Clean up any stale flat layout from older launcher versions.
    let flat_manifest = widevine_root.join("manifest.json");
    if flat_manifest.exists() {
        for stray in ["manifest.json", "LICENSE", "_platform_specific"] {
            let p = widevine_root.join(stray);
            if p.is_dir() {
                let _ = std::fs::remove_dir_all(&p);
            } else if p.exists() {
                let _ = std::fs::remove_file(&p);
            }
        }
    }
    copy_dir_recursive(&src, &versioned).with_context(|| {
        format!("copy {} → {}", src.display(), versioned.display())
    })?;
    // Chromium reads this single-line marker on startup.
    std::fs::write(
        widevine_root.join("latest-component-updated-version"),
        version,
    )?;
    eprintln!("[launcher] widevine pre-warmed: {}", versioned.display());
    Ok(())
}

fn copy_dir_recursive(src: &Path, dst: &Path) -> Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let from = entry.path();
        let to = dst.join(entry.file_name());
        let ty = entry.file_type()?;
        if ty.is_dir() {
            copy_dir_recursive(&from, &to)?;
        } else if ty.is_symlink() {
            // Resolve symlinks so dst tree stays portable across hosts.
            let target = std::fs::read_link(&from)?;
            let resolved = if target.is_absolute() { target } else { from.parent().unwrap().join(target) };
            if resolved.is_dir() {
                copy_dir_recursive(&resolved, &to)?;
            } else {
                std::fs::copy(&resolved, &to)?;
            }
        } else {
            std::fs::copy(&from, &to)?;
        }
    }
    Ok(())
}

/// Read host TZ from /etc/localtime symlink, fall back to $TZ.
fn host_timezone() -> Option<String> {
    if let Ok(target) = std::fs::read_link("/etc/localtime") {
        let path = target.to_string_lossy().into_owned();
        for prefix in ["/usr/share/zoneinfo/", "/var/db/timezone/zoneinfo/"] {
            if let Some(tz) = path.strip_prefix(prefix) {
                return Some(tz.to_string());
            }
        }
    }
    if let Ok(tz) = std::env::var("TZ") {
        if !tz.is_empty() {
            return Some(tz);
        }
    }
    // Windows has neither of the above, and answering "UTC" there put every
    // profile on a clock almost nobody really keeps.
    iana_time_zone::get_timezone().ok().filter(|s| !s.is_empty())
}

/// Extract BCP-47 locale from $LANG/$LC_ALL ("en_US.UTF-8" → "en-US").
fn host_locale() -> Option<String> {
    for var in ["LANG", "LC_ALL", "LC_MESSAGES"] {
        if let Ok(v) = std::env::var(var) {
            let stripped = v.split('.').next().unwrap_or("").replace('_', "-");
            if stripped.contains('-') {
                return Some(stripped);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::fill_window_from_screen;
    use serde_json::json;

    /// I4: the local API and a run start profiles through here, not through
    /// the UI's own check, so the lock is checked here.
    #[tokio::test]
    async fn nothing_starts_while_the_data_lock_is_held() {
        let _serial = crate::migrate::serial();
        assert!(crate::migrate::hold());
        let plain = super::launch_profile("any", true, false).await;
        let synced = super::launch_profile_synced("any", false, false, Some("g"), 0, "").await;
        crate::migrate::release();
        for r in [plain, synced] {
            assert_eq!(r.err().map(|e| e.to_string()).as_deref(), Some(super::LOCKED));
        }
    }

    /// I-1: a launch counts from its first step, and stops short of the spawn
    /// when the lock was taken while it waited on the network.
    #[test]
    fn a_cancel_stops_only_the_launch_it_was_made_for() {
        let id = "cancel-test-profile";
        let scope = super::CancelScope::start(id);
        assert!(super::check_cancel(id).is_ok());
        super::cancel(id);
        assert_eq!(super::check_cancel(id).err().map(|e| e.to_string()).as_deref(), Some(super::CANCELLED_BY_USER));
        drop(scope);
        // Late, after that launch returned: the next launch starts clean.
        super::cancel(id);
        let _next = super::CancelScope::start(id);
        assert!(super::check_cancel(id).is_ok());
    }

    #[test]
    fn a_launch_under_way_is_counted_and_stops_before_the_spawn() {
        let _serial = crate::migrate::serial();
        let before = super::in_flight();
        let launching = super::Launching::start();
        assert_eq!(super::in_flight(), before + 1);
        assert!(super::may_spawn("not-a-profile").is_ok());
        assert!(crate::migrate::hold());
        let stopped = super::may_spawn("not-a-profile").err().map(|e| e.to_string());
        crate::migrate::release();
        drop(launching);
        assert_eq!(stopped.as_deref(), Some(super::CANCELLED));
        assert_eq!(super::in_flight(), before);
    }

    fn obj(v: serde_json::Value) -> serde_json::Map<String, serde_json::Value> {
        v.as_object().unwrap().clone()
    }

    #[test]
    fn derives_a_window_from_the_claimed_screen() {
        let mut raw = obj(json!({
            "screen": { "width": 1920, "height": 1080,
                        "avail_width": 1920, "avail_height": 1032 }
        }));
        fill_window_from_screen(&mut raw);
        let w = raw["window"].as_object().unwrap();
        assert_eq!(w["outer_width"], 1920);
        assert_eq!(w["outer_height"], 1031);
        assert_eq!(w["inner_width"], 1920);
        assert_eq!(w["inner_height"], 944);
    }

    #[test]
    fn leaves_a_profile_that_names_its_own_window() {
        let mut raw = obj(json!({
            "screen": { "width": 1920, "height": 1080,
                        "avail_width": 1920, "avail_height": 1032 },
            "window": { "outer_width": 1280, "outer_height": 800,
                        "inner_width": 1280, "inner_height": 713 }
        }));
        fill_window_from_screen(&mut raw);
        let w = raw["window"].as_object().unwrap();
        assert_eq!(w["outer_width"], 1280, "a captured window must survive");
        assert_eq!(w["inner_height"], 713);
    }

    #[test]
    fn falls_back_to_the_full_screen_when_avail_is_absent() {
        let mut raw = obj(json!({ "screen": { "width": 1366, "height": 768 } }));
        fill_window_from_screen(&mut raw);
        let w = raw["window"].as_object().unwrap();
        assert_eq!(w["outer_width"], 1366);
        assert_eq!(w["outer_height"], 767);
    }

    #[test]
    fn does_nothing_without_a_screen() {
        let mut raw = obj(json!({ "name": "no screen here" }));
        fill_window_from_screen(&mut raw);
        assert!(raw.get("window").is_none());
    }

    #[test]
    fn an_empty_window_block_is_still_filled() {
        let mut raw = obj(json!({
            "screen": { "width": 1600, "height": 900, "avail_width": 1600, "avail_height": 860 },
            "window": {}
        }));
        fill_window_from_screen(&mut raw);
        let w = raw["window"].as_object().unwrap();
        assert_eq!(w["outer_width"], 1600);
        assert_eq!(w["outer_height"], 859);
    }

    // ---- A new Windows/Linux profile takes this computer's screen ----

    use super::{claim_host_screen, wants_real_screen};

    const WIN_UA: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36";
    const LINUX_UA: &str = "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36";
    const MAC_UA: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36";
    const PHONE_UA: &str = "Mozilla/5.0 (Linux; Android 15; Pixel 8) AppleWebKit/537.36";

    /// A library-shaped template: 1920x1080 with a 48 px taskbar, its own window.
    fn template(platform: &str, ua: &str) -> serde_json::Map<String, serde_json::Value> {
        obj(json!({
            "_meta": { "id": "" },
            "navigator": { "platform": platform, "user_agent": ua },
            "screen": { "width": 1920, "height": 1080, "avail_width": 1920,
                        "avail_height": 1032, "device_pixel_ratio": 1 },
            "window": { "outer_width": 1920, "outer_height": 1031, "inner_width": 1920,
                        "inner_height": 944, "keep_focus": true }
        }))
    }

    fn assert_window_fits(raw: &serde_json::Map<String, serde_json::Value>) {
        let (s, w) = (&raw["screen"], &raw["window"]);
        assert!(w["outer_width"].as_i64() <= s["avail_width"].as_i64(), "{raw:?}");
        assert!(w["outer_height"].as_i64() <= s["avail_height"].as_i64(), "{raw:?}");
    }

    #[test]
    fn a_windows_template_takes_the_host_screen_and_keeps_its_taskbar() {
        let mut raw = template("Windows", WIN_UA);
        assert!(claim_host_screen(&mut raw, Some((2560, 1440, 1.5))));
        let s = raw["screen"].as_object().unwrap();
        assert_eq!((s["width"].as_i64(), s["height"].as_i64()), (Some(2560), Some(1440)));
        assert_eq!(s["avail_height"], 1392, "the template's 48 px taskbar stays");
        assert_eq!(s["device_pixel_ratio"].as_f64(), Some(1.5), "the host's scale goes with its size");
        let w = raw["window"].as_object().unwrap();
        assert_eq!((w["outer_width"].as_i64(), w["outer_height"].as_i64()), (Some(2560), Some(1391)));
        assert_eq!(w["inner_height"], 1304);
        assert_eq!(w["keep_focus"], true, "the window's own settings stay");
        assert_window_fits(&raw);
        // The launch keeps that window: it already fits the screen.
        assert!(fill_window_from_screen(&mut raw).is_none());
    }

    #[test]
    fn a_linux_template_on_a_smaller_host_gets_a_window_that_fits() {
        let mut raw = template("Linux", LINUX_UA);
        assert!(claim_host_screen(&mut raw, Some((1440, 900, 2.0))));
        assert_eq!(raw["screen"]["width"], 1440);
        assert_eq!(raw["screen"]["device_pixel_ratio"].as_f64(), Some(2.0));
        assert_eq!(raw["screen"]["avail_height"], 852);
        assert_eq!(raw["window"]["outer_width"], 1440);
        assert_window_fits(&raw);
    }

    #[test]
    fn mac_and_phone_templates_keep_the_fingerprint_screen() {
        let cases = [
            ("macOS", MAC_UA), ("MacIntel", MAC_UA), ("Android", PHONE_UA),
            ("Linux armv8l", PHONE_UA),
            // A phone whose platform reads plain "Linux" is still a phone.
            ("Linux", PHONE_UA),
        ];
        for (platform, ua) in cases {
            let mut raw = template(platform, ua);
            let before = raw.clone();
            assert!(!claim_host_screen(&mut raw, Some((2560, 1440, 1.5))), "{platform}");
            assert_eq!(raw, before, "{platform}");
        }
    }

    /// The global "real screen" switch is for desktops. A phone given this
    /// computer's monitor says it is a handset and then reports a screen no
    /// handset has — the pair deviceinfo.me flags first.
    #[test]
    fn the_real_screen_switch_leaves_a_phone_its_own_screen() {
        let phone = template("Android", PHONE_UA);
        assert!(!wants_real_screen(Some("real"), false, &phone));
        // A handset whose platform says plain "Linux" is still a phone.
        let linux_phone = template("Linux", PHONE_UA);
        assert!(!wants_real_screen(Some("real"), false, &linux_phone));
        // A desktop still takes the host screen, and a hand-picked resolution
        // still wins over the switch.
        let desktop = template("Windows", WIN_UA);
        assert!(wants_real_screen(Some("real"), false, &desktop));
        assert!(!wants_real_screen(Some("real"), true, &desktop));
        assert!(!wants_real_screen(Some("fingerprint"), false, &desktop));
    }

    #[test]
    fn a_screen_picked_by_hand_an_existing_profile_or_an_unknown_host_keeps_the_screen() {
        let mut raw = template("Windows", WIN_UA);
        raw["_meta"]["screen_custom"] = json!(true);
        let before = raw.clone();
        assert!(!claim_host_screen(&mut raw, Some((2560, 1440, 1.5))));
        assert_eq!(raw, before);
        let mut raw = template("Windows", WIN_UA);
        raw["_meta"]["id"] = json!("p1");
        let before = raw.clone();
        assert!(!claim_host_screen(&mut raw, Some((2560, 1440, 1.5))), "a live profile keeps its screen");
        assert_eq!(raw, before);
        let mut raw = template("Windows", WIN_UA);
        let before = raw.clone();
        assert!(!claim_host_screen(&mut raw, None));
        assert_eq!(raw, before);
    }

    /// The create paths outside the form (API, MCP, a run, templates) start from
    /// `library_profile`; saving a profile that exists never claims anything.
    #[test]
    fn create_paths_claim_the_host_screen_and_existing_profiles_keep_theirs() {
        let _root = crate::store::test_root("host-screen").with_config();
        let dir = crate::store::fingerprints_dir().unwrap();
        let lib = [("t-win", "Windows", WIN_UA), ("t-linux", "Linux", LINUX_UA),
                   ("t-mac", "macOS", MAC_UA), ("t-phone", "Android", PHONE_UA)];
        for (id, platform, ua) in lib {
            let mut payload = template(platform, ua);
            payload.remove("_meta");
            std::fs::write(dir.join(format!("{id}.json")), serde_json::Value::Object(payload).to_string()).unwrap();
        }
        let host = Some((2560, 1440, 1.5));
        for id in ["t-win", "t-linux"] {
            // From a template, and as the API, MCP and a run get it (enriched, with noise).
            let made = [crate::library_profile(id, host).unwrap(), crate::fingerprint_config_on(None, id, host).unwrap()];
            for cfg in made {
                assert_eq!(cfg["screen"]["width"], 2560, "{id}");
                assert_eq!(cfg["screen"]["height"], 1440, "{id}");
                assert_eq!(cfg["screen"]["device_pixel_ratio"].as_f64(), Some(1.5), "{id}");
                assert!(cfg["_meta"].get("screen_custom").is_none(), "a default is not a pick by hand");
                assert_window_fits(&cfg);
            }
        }
        for id in ["t-mac", "t-phone"] {
            assert_eq!(crate::library_profile(id, host).unwrap()["screen"]["width"], 1920, "{id}");
            assert_eq!(crate::fingerprint_config_on(None, id, host).unwrap()["screen"]["width"], 1920, "{id}");
        }
        // No monitor to ask (as here): the fingerprint's own screen.
        assert_eq!(crate::build_fingerprint_config(None, "t-win").unwrap()["screen"]["width"], 1920);

        // A new profile with a screen picked by hand is saved with it, marked.
        let mut picked = template("Windows", WIN_UA);
        picked["_meta"]["screen_custom"] = json!(true);
        picked["screen"]["width"] = json!(1366);
        let (stored, _) = crate::profile_payload(None, json!(picked), true).unwrap();
        assert!(stored.meta.screen_custom);
        assert_eq!(stored.config["screen"]["width"], 1366);
        // A profile that exists keeps its screen, even handed to the claim itself.
        let mut existing = crate::library_profile("t-win", None).unwrap();
        existing["_meta"]["id"] = json!("p1");
        let before = existing.clone();
        assert!(!claim_host_screen(&mut existing, host));
        assert_eq!(existing, before);
    }

    // ---- The command line and what goes into it ----

    use super::{command_line, LaunchInput, LaunchOpts, Ready};
    use crate::process::ExitHook;
    use std::path::PathBuf;

    fn input(stored: serde_json::Value, proxy: Option<serde_json::Value>) -> LaunchInput {
        let stored: crate::space::profile::StoredProfile = serde_json::from_value(stored).unwrap();
        LaunchInput {
            profile_id: stored.meta.id.clone(),
            udd: PathBuf::from(format!("/data/user-data/{}", stored.meta.id)),
            proxy: proxy.map(|p| serde_json::from_value(p).unwrap()),
            extensions: stored.meta.extensions.iter().map(|id| PathBuf::from(format!("/data/extensions/{id}"))).collect(),
            library: Some(PathBuf::from("/data/extensions")),
            ext_ids: Vec::new(),
            bookmarks: Vec::new(),
            exit: if stored.meta.temporary { ExitHook::Temporary } else { ExitHook::Local },
            cdp: false,
            team: false,
            stored,
        }
    }

    fn settings() -> crate::settings::Settings {
        crate::settings::Settings { helper_enabled: true, camera_enabled: true, ..Default::default() }
    }

    /// As launched on an engine from before patch 0092's second part.
    fn argv(input: &LaunchInput, opts: &LaunchOpts, s: &crate::settings::Settings) -> Vec<String> {
        argv_on(false, input, opts, s)
    }

    fn argv_on(ext_alone: bool, input: &LaunchInput, opts: &LaunchOpts, s: &crate::settings::Settings) -> Vec<String> {
        let mut raw = input.stored.config.clone();
        raw.remove("_meta");
        // Spelled out rather than joined, so the snapshot reads the same on Windows.
        let fp = PathBuf::from(format!("{}/fingerprint.json", input.udd.display()));
        let icon = PathBuf::from(format!("/runtime/profile-icons/{}.png", input.profile_id));
        let ready = Ready {
            fp_file: &fp,
            icon: Some(&icon),
            raw: &raw,
            proxy_udp_ok: false,
            proxy_public_ip: None,
            use_real_screen: false,
            x11: false,
            ext_alone,
        };
        command_line(input, &ready, opts, s)
    }

    const UI: LaunchOpts = LaunchOpts { enable_cdp: false, headless: false, sync_group: None, bus_port: 4242, bus_token: "tok", space: None };

    /// What the engine was started with before the launch was split up, for
    /// an ordinary profile started from the list. It must not move.
    #[test]
    fn a_plain_profile_starts_with_the_same_command_line() {
        let p = input(
            json!({ "_meta": { "id": "p1", "rev": 1, "color": "#3366ff" }, "name": "Plain",
                    "screen": { "width": 1920, "height": 1080 } }),
            None,
        );
        assert_eq!(
            argv(&p, &UI, &settings()),
            [
                "--fingerprint-profile=/data/user-data/p1/fingerprint.json",
                "--user-data-dir=/data/user-data/p1",
                "--shardx-profile-icon=/runtime/profile-icons/p1.png",
                "--shardx-profile-pill-color=#3366ff",
                "--no-first-run",
                "--disable-features=WebGPU",
                "--hide-crash-restore-bubble",
                "--shardx-bus=127.0.0.1:4242",
                "--shardx-bus-token=tok",
                "--shardx-sync-profile=p1",
                "--shardx-helper",
                "--shardx-camera",
            ]
        );
    }

    /// The same for an automated start behind a proxy, with everything else on.
    #[test]
    fn an_automated_start_keeps_its_command_line() {
        let mut p = input(
            json!({ "_meta": { "id": "p2", "rev": 1, "extensions": ["e1", "e2"], "android_media": true },
                    "name": "Phone", "webrtc": "tcp_only", "webgpu": { "vendor": "x" },
                    "client_hints": { "mobile": true } }),
            Some(json!({ "id": "x1", "name": "x", "kind": "http", "host": "127.0.0.1", "port": 9,
                         "username": "u s", "password": "p@ss" })),
        );
        p.cdp = true;
        let opts = LaunchOpts { enable_cdp: true, headless: true, sync_group: Some("grp"), bus_port: 4243, bus_token: "tok2", space: None };
        let s = crate::settings::Settings { extra_args: "--lang=de".into(), ..settings() };
        assert_eq!(
            argv(&p, &opts, &s),
            [
                "--fingerprint-profile=/data/user-data/p2/fingerprint.json",
                "--user-data-dir=/data/user-data/p2",
                "--shardx-profile-icon=/runtime/profile-icons/p2.png",
                "--no-first-run",
                "--disable-extensions-except=/data/extensions/e1,/data/extensions/e2",
                "--load-extension=/data/extensions/e1,/data/extensions/e2",
                "--proxy-server=http://u%20s:p%40ss@127.0.0.1:9",
                "--disable-quic",
                "--force-webrtc-ip-handling-policy=disable_non_proxied_udp",
                "--shardx-webrtc-policy=tcp_only",
                "--shardx-bus=127.0.0.1:4243",
                "--shardx-bus-token=tok2",
                "--shardx-sync-profile=p2",
                "--shardx-sync-group=grp",
                "--shardx-helper",
                "--shardx-camera",
                "--remote-debugging-port=0",
                "--remote-allow-origins=*",
                "--shardx-automation",
                "--headless=new",
                "--shardx-android-media",
                "--lang=de",
            ]
        );
    }

    /// Asked of the engine binary: a new engine gets no except-list (it hides the profile's
    /// own Web Store extensions), an old one keeps it, and nothing else on the line moves.
    #[test]
    fn only_an_old_engine_gets_the_except_list() {
        let p = input(json!({ "_meta": { "id": "p5", "rev": 1, "extensions": ["e1", "e2"] } }), None);
        let except = "--disable-extensions-except=/data/extensions/e1,/data/extensions/e2";
        let dir = std::env::temp_dir().join(format!("shardx-launch-ext-alone-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let engine = |name: &str, code: &[u8]| {
            let bin = dir.join(name).join("chrome");
            std::fs::create_dir_all(bin.parent().unwrap()).unwrap();
            std::fs::write(&bin, [&[0u8; 64][..], code, &[0u8; 64][..]].concat()).unwrap();
            crate::runtime::engine_loads_extensions_alone(&bin)
        };
        let old = engine("old", b"shardx_command_line_version");
        let new = engine("new", b"shardx-engine:load-extension-past-gates");
        let _ = std::fs::remove_dir_all(&dir);
        assert!(!old && new);

        let on_old = argv_on(old, &p, &UI, &settings());
        let on_new = argv_on(new, &p, &UI, &settings());
        assert!(on_old.iter().any(|a| a == except), "{on_old:?}");
        assert!(!on_old.iter().any(|a| a.starts_with("--shardx-extension-library")), "{on_old:?}");
        assert!(!on_new.iter().any(|a| a.starts_with("--disable-extensions")), "{on_new:?}");
        // The except-list gives way to the library, right after the extensions.
        let mut want: Vec<String> = on_old.into_iter().filter(|a| a != except).collect();
        let at = want.iter().position(|a| a == "--load-extension=/data/extensions/e1,/data/extensions/e2").unwrap();
        want.insert(at + 1, "--shardx-extension-library=/data/extensions".into());
        assert_eq!(on_new, want);
    }

    /// A team's profile keeps the except-list on a new engine too: there an extension of
    /// one's own would come in past the Cookies right.
    #[test]
    fn a_team_profile_keeps_the_except_list_on_a_new_engine() {
        let mut p = input(json!({ "_meta": { "id": "p6", "rev": 1, "extensions": ["e1"] } }), None);
        p.team = true;
        let on_new = argv_on(true, &p, &UI, &settings());
        for want in [
            "--disable-extensions-except=/data/extensions/e1",
            "--load-extension=/data/extensions/e1",
            "--shardx-extension-library=/data/extensions",
        ] {
            assert!(on_new.iter().any(|a| a == want), "{want} missing from {on_new:?}");
        }
    }

    /// With nothing to load, a new engine still gets the library: what an old engine left
    /// there stays unloaded. An old engine, or no library, gets nothing.
    #[test]
    fn a_new_engine_gets_the_library_even_with_no_extensions() {
        let mut p = input(json!({ "_meta": { "id": "p7", "rev": 1 } }), None);
        let ext = |argv: Vec<String>| -> Vec<String> {
            argv.into_iter().filter(|a| a.contains("extension")).collect()
        };
        assert_eq!(ext(argv_on(true, &p, &UI, &settings())), ["--shardx-extension-library=/data/extensions"]);
        assert!(ext(argv_on(false, &p, &UI, &settings())).is_empty());
        p.library = None;
        assert!(ext(argv_on(true, &p, &UI, &settings())).is_empty());
    }

    /// Debugging follows the Cookies pass the input was made with, not the ask.
    #[test]
    fn no_debugging_port_without_a_cookies_pass() {
        let p = input(json!({ "_meta": { "id": "p3" } }), None);
        let asked = LaunchOpts { enable_cdp: true, ..UI };
        assert!(!argv(&p, &asked, &settings()).iter().any(|a| a.starts_with("--remote-debugging")));
    }

    #[test]
    fn a_team_profile_drops_the_switches_that_open_shut_doors() {
        let given = [
            "--remote-debugging-port=9222",
            "--remote-debugging-pipe",
            "-remote-debugging-address=0.0.0.0",
            "--REMOTE-DEBUGGING-PORT=1",
            "/remote-debugging-port=2",
            "--user-data-dir=/tmp/elsewhere",
            "--proxy-server=socks5://1.2.3.4:1080",
            "--proxy-bypass-list=*",
            "--proxy-pac-url=http://x/p.pac",
            "--no-proxy-server",
            "--load-extension=/tmp/ext",
            "--disable-extensions-except=/tmp/ext",
            "--fingerprint-profile=/tmp/fp.json",
            " --remote-debugging-port=9223",
            "\t--user-data-dir=/tmp/padded\n",
            "\u{3000}/proxy-server=http://1.2.3.4:8080",
            "--lang=de",
            "--disable-gpu",
            "--user-data-directory=kept",
            "--ozone-platform=wayland",
        ];
        let kept = super::team_extra_args(given.iter().map(|s| s.to_string()).collect());
        assert_eq!(kept, ["--lang=de", "--disable-gpu", "--user-data-directory=kept", "--ozone-platform=wayland"]);

        // Applied only to a team's profile; this machine's own keep every switch.
        let s = crate::settings::Settings { extra_args: "--remote-debugging-port=9222 --lang=de".into(), ..settings() };
        let mut p = input(json!({ "_meta": { "id": "p4" } }), None);
        assert!(argv(&p, &UI, &s).contains(&"--remote-debugging-port=9222".to_string()));
        p.team = true;
        let team = argv(&p, &UI, &s);
        assert!(!team.iter().any(|a| a.contains("9222")), "{team:?}");
        assert_eq!(team.last().map(String::as_str), Some("--lang=de"));
    }

    #[test]
    fn a_bound_proxy_that_is_gone_is_an_error_not_no_proxy() {
        let stored = |meta: serde_json::Value| -> crate::space::profile::StoredProfile {
            serde_json::from_value(json!({ "_meta": meta })).unwrap()
        };
        let entry: crate::space::proxy::ProxyEntry = serde_json::from_value(
            json!({ "id": "x1", "name": "x", "kind": "socks5", "host": "10.0.0.1", "port": 1080 }),
        )
        .unwrap();
        let inline = json!({ "id": "", "name": "i", "kind": "http", "host": "10.0.0.2", "port": 8080 });

        let gone = super::bound_proxy(&stored(json!({ "proxy_id": "x9" })), "p", |_| Ok(None));
        let why = gone.err().map(|e| e.to_string()).unwrap_or_default();
        assert!(why.contains("no longer exists") && why.contains("x9"), "{why}");
        // A store that cannot be read is no answer either.
        let unread = super::bound_proxy(&stored(json!({ "proxy_id": "x1" })), "p", |_| Err(anyhow::anyhow!("disk")));
        let why = unread.err().map(|e| e.to_string()).unwrap_or_default();
        assert!(why.contains("x1") && why.contains("disk"), "{why}");
        // Bound by id even when something is also written on the profile.
        let both = stored(json!({ "proxy_id": "x9", "inline_proxy": inline }));
        assert!(super::bound_proxy(&both, "p", |_| Ok(None)).is_err());

        let found = super::bound_proxy(&stored(json!({ "proxy_id": "x1" })), "p", |id| {
            assert_eq!(id, "x1");
            Ok(Some(entry.clone()))
        });
        assert_eq!(found.unwrap().map(|p| p.host).as_deref(), Some("10.0.0.1"));
        // No binding, or a blank one: the profile's own line, if any. The store is not asked.
        for meta in [json!({}), json!({ "proxy_id": "" }), json!({ "proxy_id": "  " })] {
            let none = super::bound_proxy(&stored(meta), "p", |_| panic!("nothing is bound"));
            assert!(none.unwrap().is_none());
        }
        let quick = super::bound_proxy(&stored(json!({ "inline_proxy": inline })), "p", |_| panic!("nothing is bound"));
        assert_eq!(quick.unwrap().map(|p| p.host).as_deref(), Some("10.0.0.2"));
    }

    /// The whole launch, short of a real engine: a profile whose proxy is gone
    /// is refused before anything is started.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_profile_whose_proxy_is_gone_does_not_start() {
        use std::os::unix::fs::PermissionsExt;
        let _serial = crate::migrate::serial();
        let root = crate::store::test_root("gone-proxy");
        let id = format!("gone-{}", uuid::Uuid::new_v4().simple());
        let body = json!({ "_meta": { "id": id, "rev": 1, "proxy_id": format!("px-{id}") }, "name": "Gone" });
        std::fs::write(crate::store::profiles_dir().unwrap().join(format!("{id}.json")), body.to_string()).unwrap();

        // An engine that leaves a mark if it is ever started.
        let marker = root.path.join("started");
        let engine = root.path.join("engine.sh");
        std::fs::write(&engine, format!("#!/bin/sh\ntouch '{}'\n", marker.display())).unwrap();
        std::fs::set_permissions(&engine, std::fs::Permissions::from_mode(0o755)).unwrap();

        for enable_cdp in [false, true] {
            let opts = LaunchOpts { enable_cdp, ..UI };
            let out = super::launch_using(|| Ok(engine.clone()), &id, &opts).await;
            let why = out.err().map(|e| e.to_string()).unwrap_or_default();
            assert!(why.contains("no longer exists"), "{why}");
        }
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        assert!(!marker.exists(), "the engine was started");
        assert!(!crate::process::Tracker::shared().is_running(&id));
    }

    /// A start while another of the same profile is under way is refused, and
    /// one after it finds the browser running: never a second browser.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_second_start_while_one_is_under_way_is_refused() {
        use std::os::unix::fs::PermissionsExt;
        let _serial = crate::migrate::serial();
        let root = crate::store::test_root("start-twice");
        let id = format!("twice-{}", uuid::Uuid::new_v4().simple());
        let body = json!({ "_meta": { "id": id, "rev": 1 }, "name": "Twice" });
        std::fs::write(crate::store::profiles_dir().unwrap().join(format!("{id}.json")), body.to_string()).unwrap();
        let started = root.path.join("started");
        let engine = root.path.join("engine.sh");
        std::fs::write(&engine, format!("#!/bin/sh\necho $$ >> '{}'\nexec sleep 60\n", started.display())).unwrap();
        std::fs::set_permissions(&engine, std::fs::Permissions::from_mode(0o755)).unwrap();
        let count = || std::fs::read_to_string(&started).unwrap_or_default().lines().count();

        let held = super::Starting::claim(&id).unwrap();
        let busy = super::launch_using(|| Ok(engine.clone()), &id, &UI).await.err().map(|e| e.to_string());
        assert_eq!(busy.as_deref(), Some(crate::space::open::BUSY));
        drop(held);

        // Two at once: one browser, whichever way they interleave.
        let (a, b) = tokio::join!(
            super::launch_using(|| Ok(engine.clone()), &id, &UI),
            super::launch_using(|| Ok(engine.clone()), &id, &UI)
        );
        assert!(a.is_ok() != b.is_ok(), "{:?} / {:?}", a.as_ref().map(|o| o.pid), b.as_ref().map(|o| o.pid));
        let again = super::launch_using(|| Ok(engine.clone()), &id, &UI).await.err().map(|e| e.to_string());
        assert!(again.unwrap_or_default().contains("is already running"));
        for _ in 0..100 {
            if count() > 0 {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        assert_eq!(count(), 1, "one browser");
        assert!(crate::process::Tracker::shared().kill(&id).await.unwrap());
        for _ in 0..200 {
            if !crate::process::Tracker::shared().is_running(&id) {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        assert!(!crate::process::Tracker::shared().is_running(&id));
    }

    /// The input a profile in this machine's own files gets, and the passes it takes.
    #[test]
    fn the_input_follows_the_passes_and_the_profile() {
        let _root = crate::store::test_root("launch-input");
        let dir = crate::store::profiles_dir().unwrap();
        std::fs::write(dir.join("li-temp.json"), json!({ "_meta": { "id": "li-temp", "temporary": true } }).to_string()).unwrap();
        std::fs::write(dir.join("li-kept.json"), json!({ "_meta": { "id": "li-kept" } }).to_string()).unwrap();
        let perm = crate::space::perm::start;
        let cookies = crate::space::perm::cookies;

        let plain = super::launch_input(&perm("li-kept").unwrap(), None).unwrap();
        assert!(!plain.cdp && !plain.team && plain.proxy.is_none());
        assert!(matches!(plain.exit, ExitHook::Local));
        assert_eq!(plain.udd, crate::store::user_data_root().unwrap().join("li-kept"));

        let automated = super::launch_input(&perm("li-temp").unwrap(), Some(&cookies("li-temp").unwrap())).unwrap();
        assert!(automated.cdp);
        assert!(matches!(automated.exit, ExitHook::Temporary));

        // A pass for one profile opens nothing on another.
        let crossed = super::launch_input(&perm("li-kept").unwrap(), Some(&cookies("li-temp").unwrap()));
        assert!(crossed.is_err());
    }

    /// From the profile file to the engine's JSON: the browser's menus follow the
    /// profile's language only when it says "profile" (#81); otherwise nothing is written.
    #[test]
    fn ui_language_reaches_the_engine_only_when_chosen() {
        let _root = crate::store::test_root("launch-ui-language");
        let dir = crate::store::profiles_dir().unwrap();
        for (id, ui) in [("ul-old", None), ("ul-on", Some("profile")), ("ul-sys", Some("system")), ("ul-odd", Some("Profile"))] {
            let mut p = json!({ "_meta": { "id": id }, "name": id });
            if let Some(ui) = ui {
                p["ui_language"] = json!(ui);
            }
            std::fs::write(dir.join(format!("{id}.json")), p.to_string()).unwrap();
        }
        let engine = |id: &str| {
            let input = super::launch_input(&crate::space::perm::start(id).unwrap(), None).unwrap();
            serde_json::to_value(super::engine_config(&input.stored)).unwrap()
        };

        // An old profile: no field, so the engine keeps this computer's language.
        let old = engine("ul-old");
        assert_eq!(old.get("ui_language"), None, "{old}");
        assert_eq!(old.get("_meta"), None, "{old}");
        assert_eq!(old["name"], json!("ul-old"));

        assert_eq!(engine("ul-on")["ui_language"], json!("profile"));
        // "system" is the default and anything else is not the opt-in: left out.
        assert_eq!(engine("ul-sys").get("ui_language"), None);
        assert_eq!(engine("ul-odd").get("ui_language"), None);

        // And a save keeps it: the field rides in the config, not in `_meta`.
        let stored = crate::space::profile::load_raw("ul-on").unwrap();
        let back: crate::space::profile::StoredProfile =
            serde_json::from_str(&serde_json::to_string(&stored).unwrap()).unwrap();
        assert_eq!(back.config.get("ui_language"), Some(&json!("profile")));
    }
}
