//! Delete all data (Settings > Storage): the launcher back to a first start,
//! with nothing deleted. Every entry the app owns moves into a new backup
//! folder, `shardx-launcher-backup-<date-time>/`, on the same disk as the
//! entry: one beside the config folder, and one beside a moved data folder
//! that sits on another disk, both under the same name. Every move is then
//! one atomic rename; a rename that would cross disks is a failure, and
//! nothing is ever copied. The layout is kept, so it can be put back by hand.
//! The browser engine and the bundled fingerprint library stay.
//!
//! Plan first, then exactly that plan. `app_reset_plan` reads what would
//! move and counts it; its id is a digest of every path and count.
//! `app_reset` builds the plan again, before and after stopping everything,
//! and refuses when the digest differs. It also refuses while a launch is
//! under way (launch.rs counts them).
//!
//! What moves is an explicit list of the names the app writes, matched case
//! and all. Every root is canonicalized and checked first: `/`, the top of
//! any disk, the home folder (or a folder holding it), a top-level system
//! folder, a folder other apps share, or one without the app's marker files
//! refuses the reset. So does a browser that still runs, from this run or an
//! earlier one. A link moves as a link; what it points at is never walked.
//!
//! In a moved data folder only what has the app's shape moves (profile and
//! extension ids), and a folder goes only once it is empty.
//!
//! All or nothing, and written down before it starts. Each backup gets a
//! README.txt and a RECOVERY.txt (every planned move, then each one done or
//! undone), and the config folder gets `reset-in-progress.json`, the journal.
//! A failure undoes every move from what is on disk. The journal goes only
//! after full success or a full undo; while it is there nothing starts, and
//! the next start offers to undo or finish the reset. A README is never
//! removed from a backup that still holds anything.

use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::ffi::OsStr;
use std::fs;
use std::io::{ErrorKind, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

/// The config folder's own files and folders (`store::config_root`).
const CONFIG_ENTRIES: &[&str] = &[
    "proxies.json",
    "proxies-history.json",
    "bookmarks.json",
    "tags.json",
    "tags.json.tmp",
    "folders.json",
    "folders.json.tmp",
    "automation.json",
    "automation.json.tmp",
    "psapi.json",
    "host-gl-caps.json",
    "automation-modules",
    "automation-runs",
];

/// Moved last: `settings.json` is the config folder's marker and holds where
/// moved data lives, so a reset cut short still finds everything.
const CONFIG_MARKERS: &[&str] = &["settings.json.bad", "settings.json"];

/// The data folders (`store::data_root`; the config folder until moved, and
/// swept there too after a move). `profiles`, the data marker, last.
const DATA_ENTRIES: &[&str] = &["user-data", "extensions", "trash", "profiles", "protected"];

/// In the engine's folder (`runtime::runtime_dir`): window icons drawn from
/// profile names. The engine beside them stays.
const RUNTIME_ENTRIES: &[&str] = &["profile-icons"];

const CONFIG_NAME: &str = "shardx-launcher";
const CONFIG_MARKER: &str = "settings.json";
const DATA_MARKER: &str = "profiles";

/// A backup: `<parent>/shardx-launcher-backup-<stamp>[-n]/`.
const BACKUP_PREFIX: &str = "shardx-launcher-backup-";
/// Inside it, where a moved data folder's entries go.
const MOVED_DATA: &str = "moved-data";
const README: &str = "README.txt";
const RECOVERY: &str = "RECOVERY.txt";
/// The journal, in the config folder, from before the first move until the
/// reset finished or was undone.
const JOURNAL: &str = "reset-in-progress.json";

const STILL_RUNNING: &str = "A profile is still running. Close it and try again.";
const RUN_STILL_GOING: &str = "An automation run is still going. Stop it and try again.";
const STARTING: &str = "A profile is starting. Wait for it, then try again.";
/// Shown when Quit or closing the window is refused mid-reset.
pub const WAIT: &str = "Wait for Delete all data to finish.";
/// The frontend asks for a new plan on this one, word for word.
pub const CHANGED: &str = "Your data changed. Check again.";

/// Where the app keeps things, as this run sees it.
pub struct Layout {
    /// `store::config_root`.
    pub config: PathBuf,
    /// `store::data_root`: the config folder unless the data was moved.
    pub data: PathBuf,
    /// `runtime::runtime_dir`, where the engine lives.
    pub runtime: Option<PathBuf>,
    pub home: Option<PathBuf>,
    /// Folders other apps use too (Documents, Application Support…): never a
    /// root, nor any folder that holds one.
    pub shared: Vec<PathBuf>,
    /// Whether two folders are on one disk (a rename between them works).
    pub same_disk: fn(&Path, &Path) -> bool,
}

impl Layout {
    pub fn live() -> Result<Self> {
        let home = dirs::home_dir();
        let mut shared: Vec<PathBuf> = [
            dirs::config_dir(),
            dirs::config_local_dir(),
            dirs::data_dir(),
            dirs::data_local_dir(),
            dirs::cache_dir(),
            dirs::preference_dir(),
            dirs::state_dir(),
            dirs::document_dir(),
            dirs::desktop_dir(),
            dirs::download_dir(),
            dirs::picture_dir(),
            dirs::video_dir(),
            dirs::audio_dir(),
            dirs::public_dir(),
            dirs::template_dir(),
            dirs::executable_dir(),
            dirs::font_dir(),
        ]
        .into_iter()
        .flatten()
        .collect();
        // Linux without user-dirs.dirs names none of these.
        if let Some(h) = &home {
            shared.extend(["Documents", "Desktop", "Downloads"].map(|n| h.join(n)));
        }
        if cfg!(target_os = "macos") {
            shared.push(PathBuf::from("/Users/Shared"));
        }
        Ok(Self {
            config: crate::store::config_root()?,
            data: crate::store::data_root()?,
            runtime: crate::runtime::runtime_dir().ok(),
            home,
            shared,
            same_disk,
        })
    }
}

/// One disk is one device id. Unknown is "not the same": a move then refuses.
#[cfg(unix)]
fn same_disk(a: &Path, b: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    match (fs::metadata(a), fs::metadata(b)) {
        (Ok(x), Ok(y)) => x.dev() == y.dev(),
        _ => false,
    }
}

/// A canonical Windows path starts with its volume (`\\?\C:`, a share).
#[cfg(not(unix))]
fn same_disk(a: &Path, b: &Path) -> bool {
    a.components().next() == b.components().next()
}

/// One thing a reset does, in order.
#[derive(Debug, Clone, PartialEq)]
enum Step {
    /// An app-owned entry, to `rel` inside backup number `spot`.
    Move { from: PathBuf, spot: usize, rel: PathBuf },
    /// A moved data folder's own folder, emptied by the moves before it.
    DropEmpty(PathBuf),
}

/// What a reset moves, counted the way the person counts it.
#[derive(Debug, Default, Clone, PartialEq, Serialize)]
pub struct Counts {
    pub profiles: usize,
    pub proxies: usize,
    pub fingerprints: usize,
    pub extensions: usize,
    pub bookmarks: usize,
    pub projects: usize,
}

/// The checked roots and every step, built from the disk each time.
#[derive(Debug, PartialEq)]
pub struct Plan {
    config: PathBuf,
    moved: Option<PathBuf>,
    runtime: Option<PathBuf>,
    /// The folders backups go in: beside the config folder first, then beside
    /// a moved data folder on another disk.
    spots: Vec<PathBuf>,
    steps: Vec<Step>,
    counts: Counts,
}

impl Plan {
    fn roots(&self) -> Vec<PathBuf> {
        std::iter::once(self.config.clone()).chain(self.moved.clone()).chain(self.runtime.clone()).collect()
    }

    /// Every path and count, so any change shows. Sizes stay out: a running
    /// browser grows its folder all the time.
    fn digest(&self) -> String {
        let mut h = Sha256::new();
        for s in &self.spots {
            h.update(format!("spot\0{}\n", s.display()));
        }
        for s in &self.steps {
            match s {
                Step::Move { from, spot, rel } => h.update(format!("move\0{}\0{spot}\0{}\n", from.display(), rel.display())),
                Step::DropEmpty(p) => h.update(format!("drop\0{}\n", p.display())),
            }
        }
        h.update(serde_json::to_string(&self.counts).unwrap_or_default());
        h.finalize()[..12].iter().map(|b| format!("{b:02x}")).collect()
    }

    fn backups(&self, name: &str) -> Vec<PathBuf> {
        self.spots.iter().map(|s| s.join(name)).collect()
    }

    /// The journal for these backups: every step with its full paths.
    fn journal(&self, backups: &[PathBuf]) -> Journal {
        Journal {
            plan: self.digest(),
            config: self.config.clone(),
            roots: self.roots(),
            backups: backups.to_vec(),
            steps: self
                .steps
                .iter()
                .map(|s| match s {
                    Step::Move { from, spot, rel } => JStep::Move { from: from.clone(), to: backups[*spot].join(rel) },
                    Step::DropEmpty(d) => JStep::Drop { dir: d.clone() },
                })
                .collect(),
        }
    }
}

/// What the confirm dialog shows.
#[derive(Debug, Serialize)]
pub struct PlanView {
    /// `<stamp>.<digest>`: `app_reset` takes it back.
    pub id: String,
    /// Where the backups will be (one, or two when a data folder is on
    /// another disk); a free name is taken at the time.
    pub backups: Vec<String>,
    pub counts: Counts,
    pub total_bytes: u64,
    /// Every entry that moves, as it is now.
    pub entries: Vec<String>,
}

pub fn plan(l: &Layout) -> Result<Plan> {
    if l.config.file_name() != Some(OsStr::new(CONFIG_NAME)) {
        bail!("Nothing was moved: {} is not ShardBrowser’s settings folder.", l.config.display());
    }
    let config = safe_root(&l.config, l)?;
    if config.file_name() != Some(OsStr::new(CONFIG_NAME)) || !is_file(&config, CONFIG_MARKER) {
        bail!("Nothing was moved: {} doesn’t look like ShardBrowser’s settings folder.", config.display());
    }
    let home_spot = config.parent().context("the settings folder has no folder above it")?.to_path_buf();
    let data = safe_root(&l.data, l)?;
    let moved = if data == config {
        None
    } else {
        if config.starts_with(&data) {
            bail!("Nothing was moved: the data folder {} holds other folders too.", data.display());
        }
        if exact_dir(&data, DATA_MARKER).is_none() {
            bail!("Nothing was moved: {} doesn’t look like ShardBrowser’s data folder.", data.display());
        }
        Some(data)
    };
    let mut spots = vec![home_spot.clone()];
    // A data folder on another disk gets its own backup beside it, on its
    // disk (it is no mount point, so its parent is on that disk too).
    let moved_spot = match &moved {
        Some(m) if !(l.same_disk)(m, &home_spot) => {
            spots.push(m.parent().context("the data folder has no folder above it")?.to_path_buf());
            1
        }
        _ => 0,
    };
    // The icons are a cache: a runtime folder that is not where the app puts
    // it, or not on the config's disk, keeps them rather than stop the reset.
    let runtime = l
        .runtime
        .as_deref()
        .filter(|r| r.exists())
        .and_then(|r| safe_root(r, l).ok())
        .filter(|r| {
            r.file_name() == Some(OsStr::new("runtime"))
                && r.parent().and_then(Path::file_name) == Some(OsStr::new(CONFIG_NAME))
                && (l.same_disk)(r, &home_spot)
        });

    let mut steps = Vec::new();
    let mut counts = Counts::default();
    if let Some(m) = &moved {
        for folder in DATA_ENTRIES {
            let Some(dir) = exact_dir(m, folder) else { continue };
            let mut ours = Vec::new();
            let mut others = false;
            for e in fs::read_dir(&dir).with_context(|| format!("Nothing was moved: {} can’t be read", dir.display()))? {
                let e = e?;
                let kind = e.file_type()?;
                if app_named(folder, &e.file_name(), kind.is_dir()) {
                    ours.push(e.file_name());
                } else {
                    others = true;
                }
            }
            ours.sort();
            for name in ours {
                match *folder {
                    "profiles" if !name.to_string_lossy().ends_with(".tmp") => counts.profiles += 1,
                    "extensions" => counts.extensions += 1,
                    _ => {}
                }
                steps.push(Step::Move { from: dir.join(&name), spot: moved_spot, rel: Path::new(MOVED_DATA).join(folder).join(&name) });
            }
            if !others {
                steps.push(Step::DropEmpty(dir));
            }
        }
    }
    for name in DATA_ENTRIES {
        if let Some((from, kind)) = exact(&config, name) {
            if kind.is_dir() {
                match *name {
                    "profiles" => counts.profiles += count_in(&from, |n, k| !k.is_dir() && n.ends_with(".json")),
                    "extensions" => counts.extensions += count_in(&from, |_, k| k.is_dir()),
                    _ => {}
                }
            }
            steps.push(Step::Move { from, spot: 0, rel: PathBuf::from(name) });
        }
    }
    if let Some(rt) = &runtime {
        for name in RUNTIME_ENTRIES {
            if let Some((from, _)) = exact(rt, name) {
                steps.push(Step::Move { from, spot: 0, rel: Path::new("runtime").join(name) });
            }
        }
    }
    if let Some(dir) = exact_dir(&config, "fingerprints") {
        let mut ours = Vec::new();
        for e in fs::read_dir(&dir)?.flatten() {
            let Ok(kind) = e.file_type() else { continue };
            let name = e.file_name();
            let json = Path::new(&name).extension() == Some(OsStr::new("json"));
            // The app never links a fingerprint in: a link is the user's add.
            let added = json && (kind.is_symlink() || (kind.is_file() && imported_file(&e.path())));
            if added {
                counts.fingerprints += 1;
            }
            if added || name == OsStr::new(".staging") {
                ours.push(name);
            }
        }
        ours.sort();
        for name in ours {
            steps.push(Step::Move { from: dir.join(&name), spot: 0, rel: Path::new("fingerprints").join(&name) });
        }
    }
    for name in CONFIG_ENTRIES.iter().chain(CONFIG_MARKERS) {
        if let Some((from, _)) = exact(&config, name) {
            match *name {
                "proxies.json" => counts.proxies = count_list(&from, "proxies"),
                "bookmarks.json" => counts.bookmarks = count_list(&from, "bookmarks"),
                "automation.json" => counts.projects = count_list(&from, "projects"),
                _ => {}
            }
            steps.push(Step::Move { from, spot: 0, rel: PathBuf::from(name) });
        }
    }
    Ok(Plan { config, moved, runtime, spots, steps, counts })
}

impl Plan {
    /// For the person; walks every folder for its size, so only here.
    pub fn view(&self, stamp: &str) -> PlanView {
        let name = format!("{BACKUP_PREFIX}{stamp}");
        let moves = || self.steps.iter().filter_map(|s| if let Step::Move { from, .. } = s { Some(from) } else { None });
        PlanView {
            id: format!("{stamp}.{}", self.digest()),
            backups: self.backups(&name).iter().map(|b| b.display().to_string()).collect(),
            counts: self.counts.clone(),
            total_bytes: moves().map(|p| size_of(p)).sum(),
            entries: moves().map(|p| p.display().to_string()).collect(),
        }
    }
}

/// The stamp a plan id carries, when it is one: `YYYY-MM-DD-HHMMSS`, digits
/// and dashes only, so it can never steer the backup's name elsewhere.
fn stamp_of(id: &str) -> Option<(&str, &str)> {
    let (stamp, digest) = id.split_once('.')?;
    let shape = stamp.len() == 17
        && stamp.char_indices().all(|(i, c)| if [4, 7, 10].contains(&i) { c == '-' } else { c.is_ascii_digit() });
    (shape && !digest.is_empty()).then_some((stamp, digest))
}

/// The first profile folder a browser still holds, from this run or an
/// earlier one (the tracker knows only this run's browsers).
fn running_profile(plan: &Plan) -> Option<PathBuf> {
    for root in std::iter::once(&plan.config).chain(plan.moved.iter()) {
        let Some(dir) = exact_dir(root, "user-data") else { continue };
        let Ok(entries) = fs::read_dir(&dir) else { continue };
        for e in entries.flatten() {
            if e.file_type().is_ok_and(|t| t.is_dir()) && lock_is_live(&e.path()) {
                return Some(e.path());
            }
        }
    }
    None
}

/// Whether a browser holds `udd`, read the way Chromium reads it: a
/// `SingletonLock` link to "<host>-<pid>". A malformed link is stale, as
/// Chromium treats it. The host is not trusted alone (macOS renames the
/// computer with the network), so a live pid counts on any host, as
/// Chromium's own check of the process does.
#[cfg(unix)]
pub(crate) fn lock_is_live(udd: &Path) -> bool {
    let Ok(target) = fs::read_link(udd.join("SingletonLock")) else { return false };
    let Some((host, pid)) = target.to_str().and_then(|t| t.rsplit_once('-')) else { return false };
    if host.is_empty() {
        return false;
    }
    // A lock naming another machine is not ours to read a pid out of. A
    // profile carried over from another computer, or restored from a backup,
    // keeps that machine's SingletonLock, and its pid can easily exist here as
    // something else entirely. While this answer only decided whether to show
    // "Running" the cost of being wrong was a stuck row; Stop now acts on it,
    // so being wrong would mean signalling a stranger's process. An unknown
    // local name leaves the old behaviour alone rather than calling every lock
    // dead.
    if this_host().is_some_and(|here| here != host) {
        return false;
    }
    let Ok(pid) = pid.parse::<i32>() else { return false };
    pid_alive(pid)
}

/// This machine's name, the way Chromium writes it into `SingletonLock`.
#[cfg(unix)]
pub(crate) fn this_host() -> Option<String> {
    let mut buf = [0 as libc::c_char; 256];
    // SAFETY: gethostname writes at most buf.len() bytes and NUL-terminates
    // whenever it returns 0; the buffer outlives the CStr borrowed from it.
    if unsafe { libc::gethostname(buf.as_mut_ptr(), buf.len()) } != 0 {
        return None;
    }
    unsafe { std::ffi::CStr::from_ptr(buf.as_ptr()) }
        .to_str()
        .ok()
        .map(str::to_owned)
}

#[cfg(unix)]
fn pid_alive(pid: i32) -> bool {
    if pid <= 0 {
        return false;
    }
    // SAFETY: signal 0 delivers nothing; it only asks whether the pid exists.
    let rc = unsafe { libc::kill(pid as libc::pid_t, 0) };
    rc == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

/// Chromium on Windows keeps `lockfile` open, shared for reading only, while
/// the profile runs (and deletes it on close): an open that shares nothing
/// then fails with a sharing violation (32).
#[cfg(windows)]
pub(crate) fn lock_is_live(udd: &Path) -> bool {
    use std::os::windows::fs::OpenOptionsExt;
    let lock = udd.join("lockfile");
    if fs::symlink_metadata(&lock).is_err() {
        return false;
    }
    match fs::OpenOptions::new().read(true).share_mode(0).open(&lock) {
        Ok(_) => false,
        Err(e) => e.raw_os_error() == Some(32),
    }
}

#[cfg(not(any(unix, windows)))]
pub(crate) fn lock_is_live(_udd: &Path) -> bool {
    false
}

/// The journal, as `reset-in-progress.json` holds it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Journal {
    /// The plan's digest.
    plan: String,
    config: PathBuf,
    /// Where the moves come from (canonical).
    roots: Vec<PathBuf>,
    backups: Vec<PathBuf>,
    steps: Vec<JStep>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum JStep {
    Move { from: PathBuf, to: PathBuf },
    Drop { dir: PathBuf },
}

fn exists(p: &Path) -> bool {
    fs::symlink_metadata(p).is_ok()
}

/// Writes a file that must survive a crash: to the side, flushed, renamed
/// into place, and its folder flushed.
fn write_durable(path: &Path, body: &[u8]) -> Result<()> {
    let tmp = path.with_extension("tmp");
    {
        let mut f = fs::File::create(&tmp).with_context(|| format!("write {}", tmp.display()))?;
        f.write_all(body)?;
        f.sync_all()?;
    }
    fs::rename(&tmp, path)?;
    sync_dir(path.parent().unwrap_or(path));
    Ok(())
}

/// A new file, never over one, flushed.
fn write_new_durable(path: &Path, body: &[u8]) -> Result<()> {
    let mut f = fs::OpenOptions::new().write(true).create_new(true).open(path).with_context(|| format!("write {}", path.display()))?;
    f.write_all(body)?;
    f.sync_all()?;
    Ok(())
}

fn sync_dir(dir: &Path) {
    #[cfg(unix)]
    if let Ok(d) = fs::File::open(dir) {
        let _ = d.sync_all();
    }
    #[cfg(not(unix))]
    let _ = dir;
}

/// One line in every backup's RECOVERY.txt: what happened to which item.
fn note(backups: &[PathBuf], what: &str, from: &Path, to: &Path) {
    for b in backups {
        if let Ok(mut f) = fs::OpenOptions::new().append(true).open(b.join(RECOVERY)) {
            let _ = writeln!(f, "{what}: {}  <-  {}", to.display(), from.display());
        }
    }
}

fn readme(j: &Journal, stamp: &str, this: &Path) -> String {
    let mut s = format!(
        "ShardBrowser moved this data aside when you chose Delete all data ({stamp}). Nothing in it was deleted.\n\n\
         RECOVERY.txt lists every item: where it is in this folder, then where it came from, and whether it was moved or put back.\n\
         To put the data back: quit ShardBrowser, move each item that is here back where it came from, and start ShardBrowser again.\n\
         An item listed but missing here was never moved: leave the original where it is.\n\
         After a restart ShardBrowser makes a new, empty settings.json, profiles and user-data: replace those with the ones here.\n"
    );
    for other in j.backups.iter().filter(|b| b.as_path() != this) {
        s.push_str(&format!("\nThe rest of this reset is in {}.\n", other.display()));
    }
    s.push_str("\nDelete this folder when you no longer need it.\n");
    s
}

fn recovery(j: &Journal, stamp: &str) -> String {
    let mut s = format!(
        "ShardBrowser Delete all data, {stamp}. Each line: what happened, where the item is in the backup, and where it came from.\n\
         To put an item back by hand, move it from the first path to the second.\n\n"
    );
    for step in &j.steps {
        if let JStep::Move { from, to } = step {
            s.push_str(&format!("planned: {}  <-  {}\n", to.display(), from.display()));
        }
    }
    s
}

/// New backup folders, one per spot, all with one name: the planned one,
/// else "-2", "-3"… An existing folder is never reused or written into.
fn fresh_backups(spots: &[PathBuf], stamp: &str) -> Result<Vec<PathBuf>> {
    for n in 1..1000 {
        let name = if n == 1 { format!("{BACKUP_PREFIX}{stamp}") } else { format!("{BACKUP_PREFIX}{stamp}-{n}") };
        let mut made = Vec::new();
        let mut taken = false;
        for spot in spots {
            let p = spot.join(&name);
            match fs::create_dir(&p) {
                Ok(()) => made.push(p),
                Err(e) if e.kind() == ErrorKind::AlreadyExists => {
                    taken = true;
                    break;
                }
                Err(e) => {
                    for m in &made {
                        let _ = fs::remove_dir(m);
                    }
                    return Err(anyhow!(e).context(format!("Nothing was moved: {} can’t be made", p.display())));
                }
            }
        }
        if !taken {
            return Ok(made);
        }
        // Made here a moment ago, and empty.
        for m in &made {
            let _ = fs::remove_dir(m);
        }
    }
    bail!("Nothing was moved: there are too many backups beside {}.", spots[0].display())
}

/// Moves one entry by rename; never over anything. A link moves as a link.
/// Across disks a rename fails, and so does the move: nothing is copied.
fn move_entry(from: &Path, to: &Path) -> Result<()> {
    if exists(to) {
        bail!("{} is already there", to.display());
    }
    match fs::rename(from, to) {
        Ok(()) => Ok(()),
        Err(e) if crosses_devices(&e) => bail!("{} and {} are on different disks", from.display(), to.display()),
        Err(e) => Err(e.into()),
    }
}

fn crosses_devices(e: &std::io::Error) -> bool {
    #[cfg(unix)]
    let raw = Some(libc::EXDEV);
    // ERROR_NOT_SAME_DEVICE
    #[cfg(windows)]
    let raw = Some(17);
    #[cfg(not(any(unix, windows)))]
    let raw: Option<i32> = None;
    e.kind() == ErrorKind::CrossesDevices || (raw.is_some() && e.raw_os_error() == raw)
}

/// Whether `p` is a real folder holding nothing but empty folders.
fn only_empty_dirs(p: &Path) -> bool {
    fs::symlink_metadata(p).is_ok_and(|m| m.is_dir())
        && fs::read_dir(p).is_ok_and(|it| it.flatten().all(|e| e.file_type().is_ok_and(|t| t.is_dir()) && only_empty_dirs(&e.path())))
}

/// Removes a tree of empty folders, deepest first; anything else stays.
fn remove_empty_dirs(p: &Path) {
    if !fs::symlink_metadata(p).is_ok_and(|m| m.is_dir()) {
        return;
    }
    for e in fs::read_dir(p).into_iter().flatten().flatten() {
        if e.file_type().is_ok_and(|t| t.is_dir()) {
            remove_empty_dirs(&e.path());
        }
    }
    let _ = fs::remove_dir(p);
}

/// A folder the app made again where an entry was (a list read makes an
/// empty `profiles/`) is in the way only while it holds nothing.
fn clear_if_empty(p: &Path) {
    if only_empty_dirs(p) {
        remove_empty_dirs(p);
    }
}

fn apply(j: &Journal, step: &JStep) -> Result<()> {
    match step {
        JStep::Move { from, to } => {
            if let Some(parent) = to.parent() {
                fs::create_dir_all(parent).with_context(|| format!("make {}", parent.display()))?;
            }
            move_entry(from, to).with_context(|| format!("{} couldn’t be moved", from.display()))?;
            note(&j.backups, "moved", from, to);
        }
        // Only while still empty: a file that arrived since keeps its folder.
        JStep::Drop { dir } => {
            let _ = fs::remove_dir(dir);
        }
    }
    Ok(())
}

/// Puts back, newest first, every move the disk shows was made: the item is
/// in the backup and its place is free. Returns what could not go back.
fn undo_steps(j: &Journal) -> Vec<String> {
    let mut stuck = Vec::new();
    for step in j.steps.iter().rev() {
        match step {
            JStep::Move { from, to } => {
                if !exists(to) {
                    continue;
                }
                clear_if_empty(from);
                let back = if exists(from) {
                    Err(anyhow!("something new is at {}", from.display()))
                } else {
                    move_entry(to, from)
                };
                match back {
                    Ok(()) => note(&j.backups, "put back", from, to),
                    Err(e) => stuck.push(format!("{}: {e:#}", to.display())),
                }
            }
            JStep::Drop { dir } => {
                if !exists(dir) {
                    if let Err(e) = fs::create_dir(dir) {
                        stuck.push(format!("{}: {e}", dir.display()));
                    }
                }
            }
        }
    }
    stuck
}

/// Makes every move the disk shows was not made yet. Returns what could not.
fn finish_steps(j: &Journal) -> Vec<String> {
    let mut stuck = Vec::new();
    for step in &j.steps {
        match step {
            JStep::Move { from, to } => {
                if !exists(from) {
                    continue;
                }
                if exists(to) {
                    clear_if_empty(from);
                    if exists(from) {
                        stuck.push(format!("{}: it is in {} and something new is at its place", from.display(), to.display()));
                    }
                    continue;
                }
                if let Err(e) = apply(j, step) {
                    stuck.push(format!("{e:#}"));
                }
            }
            JStep::Drop { dir } => {
                let _ = fs::remove_dir(dir);
            }
        }
    }
    stuck
}

/// After a full undo: each backup holds only its two notes and empty
/// folders, so it goes. One that holds anything else stays, notes and all.
fn clear_backups(j: &Journal) {
    for b in &j.backups {
        let only_notes = fs::read_dir(b).is_ok_and(|it| {
            it.flatten().all(|e| {
                let n = e.file_name();
                n == OsStr::new(README) || n == OsStr::new(RECOVERY) || only_empty_dirs(&e.path())
            })
        });
        if only_notes {
            for e in fs::read_dir(b).into_iter().flatten().flatten() {
                remove_empty_dirs(&e.path());
            }
            let _ = fs::remove_file(b.join(README));
            let _ = fs::remove_file(b.join(RECOVERY));
            let _ = fs::remove_dir(b);
        }
    }
}

fn journal_path(config: &Path) -> PathBuf {
    config.join(JOURNAL)
}

/// Drops the journal: the reset finished or was undone in full.
fn close_journal(config: &Path) {
    if let Err(e) = fs::remove_file(journal_path(config)) {
        eprintln!("[launcher] reset: the journal could not be removed: {e}");
    }
    sync_dir(config);
    crate::migrate::set_reset_pending(false);
}

/// Opens the backups and the journal before the first move: the backups with
/// their README and RECOVERY, then the journal, each flushed to disk.
fn start(plan: &Plan, stamp: &str) -> Result<Journal> {
    let backups = fresh_backups(&plan.spots, stamp)?;
    let j = plan.journal(&backups);
    let written = backups.iter().try_for_each(|b| {
        write_new_durable(&b.join(README), readme(&j, stamp, b).as_bytes())?;
        write_new_durable(&b.join(RECOVERY), recovery(&j, stamp).as_bytes())?;
        sync_dir(b);
        sync_dir(b.parent().unwrap_or(b));
        Ok::<(), anyhow::Error>(())
    });
    let journaled = written.and_then(|()| write_durable(&journal_path(&plan.config), &serde_json::to_vec_pretty(&j)?));
    if let Err(e) = journaled {
        let _ = fs::remove_file(journal_path(&plan.config));
        clear_backups(&j);
        return Err(e.context("Nothing was moved: the backup folder could not be prepared"));
    }
    crate::migrate::set_reset_pending(true);
    Ok(j)
}

/// Runs the plan into fresh backups and returns them. `before` runs ahead of
/// each step (tests make one fail there). On any failure every move the disk
/// shows was made is undone; the backups and the journal go only when that
/// put everything back.
fn execute(plan: &Plan, stamp: &str, before: &mut dyn FnMut(&JStep) -> Result<()>) -> Result<Vec<PathBuf>> {
    // A browser's last write (its run time) waits for this, then finds no
    // profile to write to: it never brings one back.
    let _profiles = crate::space::profile::file_lock();
    let j = start(plan, stamp)?;
    let mut failed = None;
    for step in &j.steps {
        if let Err(e) = before(step).and_then(|()| apply(&j, step)) {
            failed = Some(e);
            break;
        }
    }
    let Some(e) = failed else {
        close_journal(&plan.config);
        return Ok(j.backups);
    };
    let all = || j.backups.iter().map(|b| b.display().to_string()).collect::<Vec<_>>().join(" and ");
    // An item on a disk that went away can't be seen, and must not be taken
    // for one that never moved: nothing is put back, and the journal stays.
    if let Some(gone) = unreachable(&j).first() {
        bail!(
            "Some data is still in {}: {e:#}. Connect the disk with {}; when ShardBrowser starts again, it offers to undo or finish the reset.",
            all(),
            gone.display()
        );
    }
    let stuck = undo_steps(&j);
    if stuck.is_empty() && unreachable(&j).is_empty() {
        clear_backups(&j);
        close_journal(&plan.config);
        bail!("Nothing was moved: {e:#}. Everything is back where it was.");
    }
    let stuck = if stuck.is_empty() { offline(&j) } else { stuck.join("; ") };
    bail!(
        "Some data is still in {}: {e:#}. It could not all be put back ({stuck}). Move the items listed in RECOVERY.txt back where they came from.",
        all()
    );
}

/// Every folder the journal names that can't be reached now: a backup or a
/// root that is gone, or (unix) a backup no longer on the disk of the root
/// it sits beside. Undo and Finish read the disk, so with one of these away
/// an item there would look as if it never moved.
fn unreachable(j: &Journal) -> Vec<PathBuf> {
    let real_dir = |p: &Path| fs::symlink_metadata(p).is_ok_and(|m| m.is_dir());
    let mut gone: Vec<PathBuf> = j.roots.iter().chain(&j.backups).filter(|p| !real_dir(p)).cloned().collect();
    if gone.is_empty() {
        for b in &j.backups {
            let beside = j.roots.iter().find(|r| r.parent() == b.parent());
            if beside.is_some_and(|r| !same_disk(b, r)) {
                gone.push(b.clone());
            }
        }
    }
    gone
}

fn offline(j: &Journal) -> String {
    match unreachable(j).first() {
        Some(p) => format!("Connect the disk with {}, then try again.", p.display()),
        None => String::new(),
    }
}

/// Undo and Finish start only with every folder of the journal in reach.
fn refuse_offline(j: &Journal) -> Result<()> {
    let why = offline(j);
    if !why.is_empty() {
        bail!("{why}");
    }
    Ok(())
}

/// The journal of a reset that did not finish, checked against this app: its
/// config folder, backups named and placed as a reset makes them, every move
/// from inside one of its roots into one of its backups.
fn read_journal(config: &Path) -> Result<Option<Journal>> {
    let path = journal_path(config);
    if !exists(&path) {
        return Ok(None);
    }
    let unreadable = || format!("ShardBrowser can’t read its notes on the reset ({})", path.display());
    let j: Journal = serde_json::from_slice(&fs::read(&path).with_context(unreadable)?).with_context(unreadable)?;
    let trusted = j.config == config
        && j.roots.first() == Some(&j.config)
        && !j.backups.is_empty()
        && j.backups.iter().all(|b| {
            b.file_name().and_then(OsStr::to_str).is_some_and(|n| n.starts_with(BACKUP_PREFIX))
                && b.parent().is_some_and(|p| j.roots.iter().any(|r| r.parent() == Some(p)))
        })
        && j.steps.iter().all(|s| match s {
            JStep::Move { from, to } => {
                from.parent().is_some_and(|p| j.roots.iter().any(|r| p.starts_with(r)))
                    && j.backups.iter().any(|b| to.starts_with(b) && to != b)
                    && !from.components().chain(to.components()).any(|c| c == Component::ParentDir)
            }
            JStep::Drop { dir } => j.roots.iter().any(|r| dir.starts_with(r) && dir != r),
        });
    if !trusted {
        bail!("ShardBrowser can’t trust its notes on the reset ({}). Nothing was changed.", path.display());
    }
    Ok(Some(j))
}

/// Puts back everything a reset that did not finish had moved, then drops
/// its journal and the emptied backups.
pub fn undo_pending(config: &Path) -> Result<()> {
    let Some(j) = read_journal(config)? else { return Ok(()) };
    refuse_offline(&j)?;
    let stuck = undo_steps(&j);
    // A disk that went away meanwhile: what was on it was skipped, not put back.
    refuse_offline(&j)?;
    if !stuck.is_empty() {
        bail!("Some data could not be put back: {}. The rest is listed in RECOVERY.txt.", stuck.join("; "));
    }
    clear_backups(&j);
    close_journal(config);
    Ok(())
}

/// Makes the moves a reset that did not finish had not made yet, then drops
/// its journal. The backups stay, as after any reset.
pub fn finish_pending(config: &Path) -> Result<Vec<PathBuf>> {
    let Some(j) = read_journal(config)? else { return Ok(Vec::new()) };
    refuse_offline(&j)?;
    let stuck = finish_steps(&j);
    refuse_offline(&j)?;
    if !stuck.is_empty() {
        bail!("Some data could not be moved: {}. It is listed in RECOVERY.txt.", stuck.join("; "));
    }
    close_journal(config);
    Ok(j.backups)
}

/// What the "A reset didn’t finish" screen shows.
#[derive(Debug, Serialize)]
pub struct PendingView {
    /// The journal itself, always: the person can find it, and set it aside.
    pub journal: String,
    pub backups: Vec<String>,
    pub moved: usize,
    pub total: usize,
    /// Folders of the journal out of reach (a disk not connected): Undo and
    /// Finish refuse until they are back.
    pub missing: Vec<String>,
    /// Set when the journal can't be read or trusted: Undo and Finish are
    /// not offered, setting the notes aside is.
    pub error: Option<String>,
}

fn pending_view(config: &Path) -> Option<PendingView> {
    let journal = journal_path(config).display().to_string();
    match read_journal(config) {
        Ok(None) => None,
        Ok(Some(j)) => {
            let moves: Vec<_> = j.steps.iter().filter_map(|s| if let JStep::Move { to, .. } = s { Some(to) } else { None }).collect();
            Some(PendingView {
                journal,
                backups: j.backups.iter().map(|b| b.display().to_string()).collect(),
                moved: moves.iter().filter(|t| exists(t)).count(),
                total: moves.len(),
                missing: unreachable(&j).iter().map(|p| p.display().to_string()).collect(),
                error: None,
            })
        }
        Err(e) => Some(PendingView { journal, backups: Vec::new(), moved: 0, total: 0, missing: Vec::new(), error: Some(format!("{e:#}")) }),
    }
}

/// "Set these notes aside": the journal is renamed, never deleted
/// (`reset-in-progress.set-aside-<stamp>[-n].json`), and the app unlocks.
/// What is still in a backup stays there, listed in its RECOVERY.txt.
fn set_aside(config: &Path, stamp: &str) -> Result<Option<PathBuf>> {
    let path = journal_path(config);
    if !exists(&path) {
        crate::migrate::set_reset_pending(false);
        return Ok(None);
    }
    for n in 1..1000 {
        let name = if n == 1 { format!("reset-in-progress.set-aside-{stamp}.json") } else { format!("reset-in-progress.set-aside-{stamp}-{n}.json") };
        let to = config.join(name);
        if exists(&to) {
            continue;
        }
        fs::rename(&path, &to).with_context(|| format!("{} couldn’t be renamed", path.display()))?;
        sync_dir(config);
        crate::migrate::set_reset_pending(false);
        return Ok(Some(to));
    }
    bail!("{} couldn’t be renamed: too many set-aside notes beside it", path.display())
}

/// At start: whether a reset did not finish. While its journal is there the
/// app stays locked (no launches, no local API, no runs, no data move).
pub fn check_pending() -> bool {
    let found = crate::store::config_root()
        .ok()
        .and_then(|c| fs::canonicalize(c).ok())
        .is_some_and(|c| exists(&journal_path(&c)));
    if found {
        crate::migrate::set_reset_pending(true);
        eprintln!("[launcher] a reset did not finish; the app stays locked until it is undone or finished");
    }
    found
}

/// The newest backup a reset left beside the config folder, if any. Names
/// with "-n" sort by n.
pub fn latest_backup() -> Option<PathBuf> {
    let config = fs::canonicalize(crate::store::config_root().ok()?).ok()?;
    let parent = config.parent()?;
    fs::read_dir(parent)
        .ok()?
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .filter_map(|e| {
            let name = e.file_name().to_str()?.strip_prefix(BACKUP_PREFIX)?.to_string();
            let (stamp, n) = match name.get(17..) {
                Some(rest) if !rest.is_empty() => (name[..17].to_string(), rest.trim_start_matches('-').parse::<u32>().ok()?),
                _ => (name.clone(), 1),
            };
            Some(((stamp, n), e.path()))
        })
        .max_by(|a, b| a.0.cmp(&b.0))
        .map(|(_, p)| p)
}

/// This computer's local time as `YYYY-MM-DD-HHMMSS` (UTC where the local
/// zone can't be read).
fn stamp_now() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    #[cfg(unix)]
    {
        let t = secs as libc::time_t;
        // SAFETY: localtime_r fills `tm` from `t` and keeps no pointer.
        let mut tm: libc::tm = unsafe { std::mem::zeroed() };
        if !unsafe { libc::localtime_r(&t, &mut tm) }.is_null() {
            return format!(
                "{:04}-{:02}-{:02}-{:02}{:02}{:02}",
                tm.tm_year + 1900, tm.tm_mon + 1, tm.tm_mday, tm.tm_hour, tm.tm_min, tm.tm_sec
            );
        }
    }
    utc_stamp(secs)
}

fn utc_stamp(secs: i64) -> String {
    let (days, rem) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    // Days to civil date (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + if m <= 2 { 1 } else { 0 };
    format!("{y:04}-{m:02}-{d:02}-{:02}{:02}{:02}", rem / 3600, rem % 3600 / 60, rem % 60)
}

fn imported_file(p: &Path) -> bool {
    fs::read_to_string(p)
        .ok()
        .and_then(|t| serde_json::from_str::<Value>(&t).ok())
        .is_some_and(|v| imported(&v))
}

/// The library keeps what came with it: a bundled entry is a bare
/// fingerprint (or marked `builtin`), while `fingerprints::import` wraps
/// what the user adds with an `id` and a `payload`.
fn imported(v: &Value) -> bool {
    v.get("payload").is_some_and(Value::is_object)
        && v.get("id").is_some_and(Value::is_string)
        && v.get("builtin") != Some(&Value::Bool(true))
}

/// What the app itself names inside each data folder: profile ids
/// (`Uuid::new_v4`, profile.rs) and extension ids (`Uuid::simple`, or a Web
/// Store id), with the suffixes the app writes.
fn app_named(folder: &str, name: &OsStr, is_dir: bool) -> bool {
    let Some(name) = name.to_str() else { return false };
    let uuid_with = |suffix: &str| name.strip_suffix(suffix).is_some_and(is_uuid);
    match folder {
        "profiles" => !is_dir && (uuid_with(".json") || uuid_with(".json.tmp")),
        "user-data" => is_dir && (is_uuid(name) || uuid_with(".opening")),
        "extensions" => is_dir && name.len() == 32 && name.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'p').contains(&b)),
        "trash" => !is_dir && (uuid_with(".zip") || uuid_with(".json")),
        "protected" => !is_dir && (uuid_with(".data") || uuid_with(".sealing")),
        _ => false,
    }
}

fn is_uuid(s: &str) -> bool {
    s.len() == 36 && uuid::Uuid::try_parse(s).is_ok()
}

fn count_in(dir: &Path, keep: impl Fn(&str, fs::FileType) -> bool) -> usize {
    fs::read_dir(dir)
        .map(|it| {
            it.flatten()
                .filter(|e| e.file_type().is_ok_and(|k| keep(&e.file_name().to_string_lossy(), k)))
                .count()
        })
        .unwrap_or(0)
}

/// The length of the list `key` in a JSON file (`{"proxies": [...]}`).
fn count_list(p: &Path, key: &str) -> usize {
    fs::read_to_string(p)
        .ok()
        .and_then(|t| serde_json::from_str::<Value>(&t).ok())
        .and_then(|v| v.get(key).and_then(Value::as_array).map(Vec::len))
        .unwrap_or(0)
}

/// Bytes in an entry, links not followed.
fn size_of(p: &Path) -> u64 {
    let Ok(meta) = fs::symlink_metadata(p) else { return 0 };
    if meta.file_type().is_symlink() {
        return 0;
    }
    if !meta.is_dir() {
        return meta.len();
    }
    fs::read_dir(p).map(|it| it.flatten().map(|e| size_of(&e.path())).sum()).unwrap_or(0)
}

/// The canonical root, or why it must not be one.
fn safe_root(path: &Path, l: &Layout) -> Result<PathBuf> {
    let root = fs::canonicalize(path)
        .with_context(|| format!("Nothing was moved: {} can’t be read", path.display()))?;
    if !root.is_dir() {
        bail!("Nothing was moved: {} is not a folder.", root.display());
    }
    let depth = root.components().filter(|c| matches!(c, Component::Normal(_))).count();
    if depth == 0 || is_mount_point(&root) {
        bail!("Nothing was moved: {} is the top of a disk.", root.display());
    }
    if depth == 1 && top_level_is_system(&root) {
        bail!("Nothing was moved: {} is a system folder.", root.display());
    }
    let home = l
        .home
        .as_deref()
        .and_then(|h| fs::canonicalize(h).ok())
        .context("Nothing was moved: ShardBrowser can’t find your home folder.")?;
    if home.starts_with(&root) {
        bail!("Nothing was moved: {} is your home folder or holds it.", root.display());
    }
    if l.shared.iter().filter_map(|s| fs::canonicalize(s).ok()).any(|s| s.starts_with(&root)) {
        bail!("Nothing was moved: other apps keep their files in {} too.", root.display());
    }
    Ok(root)
}

/// A folder another disk is mounted on (`/Volumes/T7`, `/media/u/T7`,
/// `/mnt/x`): its device differs from its parent's. Unknown counts as one.
#[cfg(unix)]
fn is_mount_point(root: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    let Some(parent) = root.parent() else { return true };
    match (fs::metadata(root), fs::metadata(parent)) {
        (Ok(a), Ok(b)) => a.dev() != b.dev(),
        _ => true,
    }
}

/// A drive's root (`D:\`) has no folder above it and is refused already.
#[cfg(not(unix))]
fn is_mount_point(_root: &Path) -> bool {
    false
}

/// Only an administrator makes a folder at the top of a Unix disk.
#[cfg(not(windows))]
fn top_level_is_system(_root: &Path) -> bool {
    true
}

/// On Windows a folder like `D:\ShardBrowser` is the user's own; these are not.
#[cfg(windows)]
fn top_level_is_system(root: &Path) -> bool {
    const SYSTEM: &[&str] = &[
        "windows", "program files", "program files (x86)", "programdata", "users",
        "documents and settings", "recovery", "perflogs", "$recycle.bin",
        "system volume information", "boot", "msocache",
    ];
    root.file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| SYSTEM.contains(&n.to_ascii_lowercase().as_str()))
}

/// The entry in `root` named exactly `name`, case and all, with its own type
/// (a link is not followed). On a disk that ignores case, `Profiles` is some
/// other app's folder, never the launcher's `profiles`.
fn exact(root: &Path, name: &str) -> Option<(PathBuf, fs::FileType)> {
    let entry = fs::read_dir(root).ok()?.flatten().find(|e| e.file_name() == OsStr::new(name))?;
    Some((entry.path(), entry.file_type().ok()?))
}

fn is_file(root: &Path, name: &str) -> bool {
    exact(root, name).is_some_and(|(_, t)| t.is_file())
}

/// A real folder (not a link) named exactly `name` in `root`.
fn exact_dir(root: &Path, name: &str) -> Option<PathBuf> {
    exact(root, name).filter(|(_, t)| t.is_dir()).map(|(p, _)| p)
}

/// Ends every run and every browser the way Stop does, and waits for them to
/// close (at most 10 s; a browser gets 5 s to close before it is killed).
/// Anything still going then stops the reset, with nothing moved.
async fn stop_everything() -> Result<()> {
    use std::collections::HashSet;
    use std::time::{Duration, Instant};

    let tracker = crate::process::Tracker::shared();
    let mut stopped: HashSet<String> = HashSet::new();
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        #[cfg(feature = "automation")]
        let runs = {
            let live: Vec<_> = crate::runner::all().into_iter().filter(|r| r.running).collect();
            for r in &live {
                crate::runner::stop(&r.project_id);
            }
            live.len()
        };
        #[cfg(not(feature = "automation"))]
        let runs = 0usize;
        // A run that was mid-start can add a browser after the first pass;
        // each one is told once (the kill channel holds one message).
        let running = tracker.running();
        for p in &running {
            if stopped.insert(p.profile_id.clone()) {
                let _ = tracker.kill(&p.profile_id).await;
            }
        }
        if running.is_empty() && runs == 0 {
            return Ok(());
        }
        if Instant::now() >= deadline {
            if !running.is_empty() {
                bail!(STILL_RUNNING);
            }
            bail!(RUN_STILL_GOING);
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// Set while a reset (or the undo or finish of one) works on the disk:
/// Quit and closing the window are refused meanwhile (lib.rs).
static ACTIVE: AtomicBool = AtomicBool::new(false);

pub fn active() -> bool {
    ACTIVE.load(Ordering::SeqCst)
}

/// The lock a data move takes, held for the whole reset: nothing starts,
/// runs or moves meanwhile. Released on every way out but success, where it
/// stays held until the restart. Quit is refused while it is held here.
struct Held;

impl Held {
    fn take() -> Option<Self> {
        if !crate::migrate::hold() {
            return None;
        }
        ACTIVE.store(true, Ordering::SeqCst);
        Some(Held)
    }

    /// Success: the moves are done and the journal is gone, so Quit is fine
    /// again; the lock stays until the restart.
    fn keep(self) {
        ACTIVE.store(false, Ordering::SeqCst);
        std::mem::forget(self);
    }
}

impl Drop for Held {
    fn drop(&mut self) {
        ACTIVE.store(false, Ordering::SeqCst);
        crate::migrate::release();
    }
}

/// While a launch is between its first step and its browser being tracked,
/// a reset would not see that browser: refuse. Read after the lock is taken
/// (launch.rs counts before it checks the lock).
fn refuse_while_starting() -> Result<(), String> {
    if crate::launch::in_flight() > 0 {
        return Err(STARTING.into());
    }
    Ok(())
}

/// Disk work off the main thread and off the async workers.
async fn blocking<T: Send + 'static>(f: impl FnOnce() -> Result<T> + Send + 'static) -> Result<T, String> {
    tauri::async_runtime::spawn_blocking(f)
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| format!("{e:#}"))
}

fn current_plan() -> Result<Plan> {
    Layout::live().and_then(|l| plan(&l))
}

fn live_config() -> Result<PathBuf> {
    Ok(fs::canonicalize(crate::store::config_root()?)?)
}

/// What Delete all data would move, counted. Reads only.
#[tauri::command]
pub async fn app_reset_plan() -> Result<PlanView, String> {
    crate::space::open::refuse_while_open()?;
    blocking(|| Ok(current_plan()?.view(&stamp_now()))).await
}

/// The teams go with the data: a reset that kept a token would keep the access.
/// Nothing of them goes into the backup.
fn forget_teams() {
    if let Err(e) = crate::space::forget_teams() {
        eprintln!("[launcher] reset: the teams were not forgotten: {e:#}");
        crate::notify_warning(format!("The team connections could not be removed: {e:#}"));
    }
}

#[derive(Debug, Serialize)]
pub struct ResetDone {
    pub backups: Vec<String>,
}

fn done(backups: Vec<PathBuf>) -> ResetDone {
    ResetDone { backups: backups.iter().map(|b| b.display().to_string()).collect() }
}

/// Settings > Storage > Delete all data, for the plan the person confirmed.
/// Stops everything and moves the user's data into the backups (engine and
/// bundled library kept). Ok names the backups; the app then waits for
/// `app_restart`, with the lock still held. An Err says why: "Your data
/// changed. Check again." when the plan no longer matches, "Nothing was
/// moved: …" (or a profile running or starting) when it refused or undid
/// itself, "Some data is still in …" in the one case it could not undo,
/// where the journal stays and the next start offers to undo or finish.
#[tauri::command]
pub async fn app_reset(plan_id: String) -> Result<ResetDone, String> {
    let Some((stamp, digest)) = stamp_of(&plan_id).map(|(s, d)| (s.to_string(), d.to_string())) else {
        return Err(CHANGED.into());
    };
    if crate::migrate::reset_pending() {
        return Err("Nothing was moved: a reset didn’t finish. Restart ShardBrowser to undo or finish it.".into());
    }
    let Some(held) = Held::take() else {
        return Err("Nothing was moved: your data is being moved. Try again when that finishes.".into());
    };
    refuse_while_starting()?;
    // A team profile's copy lives under the data folder, with its lease and upload.
    crate::space::open::refuse_while_open()?;
    if blocking(current_plan).await?.digest() != digest {
        return Err(CHANGED.into());
    }
    stop_everything().await.map_err(|e| format!("{e:#}"))?;
    // As it is now that everything stopped: still the plan that was shown.
    let checked = blocking(current_plan).await?;
    if checked.digest() != digest {
        return Err(CHANGED.into());
    }
    if let Some(dir) = running_profile(&checked) {
        eprintln!("[launcher] reset refused: a browser holds {}", dir.display());
        return Err(STILL_RUNNING.into());
    }
    refuse_while_starting()?;
    match blocking(move || execute(&checked, &stamp, &mut |_| Ok(()))).await {
        Ok(backups) => {
            forget_teams();
            crate::store::set_data_root(None);
            eprintln!("[launcher] all data moved to {backups:?}");
            if let Ok(mut last) = LAST_BACKUPS.lock() {
                *last = backups.clone();
            }
            // Held until the restart: nothing starts in an emptied app.
            held.keep();
            Ok(done(backups))
        }
        Err(e) => {
            if e.starts_with("Some data is still in") {
                for kind in ["profiles", "proxies", "extensions", "bookmarks", "trash", "automation"] {
                    crate::notify_store_changed(kind);
                }
            }
            Err(e)
        }
    }
}

/// The backup folders the last finished reset made: the only places
/// `app_reset_keep_storage` writes to.
static LAST_BACKUPS: std::sync::Mutex<Vec<PathBuf>> = std::sync::Mutex::new(Vec::new());

/// The file the window's own storage goes into, beside the moved data.
const WEBVIEW_STORAGE: &str = "webview-storage.json";

/// What the window keeps in its own storage (Tags, Views, folder colours and
/// order, list layouts) after a finished reset, written into the backup it
/// made. The window clears that storage for the fresh start; without this it
/// would be the one thing a reset that says "Nothing is deleted" did delete.
/// Only a folder the last reset made is accepted, and an existing file is
/// never overwritten.
#[tauri::command]
pub async fn app_reset_keep_storage(
    backup: String,
    items: serde_json::Map<String, Value>,
) -> Result<(), String> {
    let target = PathBuf::from(&backup);
    let known = LAST_BACKUPS
        .lock()
        .map(|l| l.iter().any(|b| *b == target))
        .unwrap_or(false);
    if !known {
        return Err("not a backup this reset made".into());
    }
    let body = serde_json::json!({
        "about": "ShardBrowser's window storage as it was before Delete all data: \
                  Tags, Views, folder colours and order, list layouts. Each item \
                  is one localStorage key and its value.",
        "items": items,
    });
    let bytes = serde_json::to_vec_pretty(&body).map_err(|e| e.to_string())?;
    blocking(move || write_new_durable(&target.join(WEBVIEW_STORAGE), &bytes)).await
}

/// A reset that did not finish, for the screen the app opens on; None when
/// there is none.
#[tauri::command]
pub async fn app_reset_pending() -> Result<Option<PendingView>, String> {
    blocking(|| Ok(pending_view(&live_config()?))).await
}

/// "Undo the reset": every item back where it came from.
#[tauri::command]
pub async fn app_reset_undo() -> Result<(), String> {
    let Some(_held) = Held::take() else {
        return Err("Your data is being moved. Try again when that finishes.".into());
    };
    blocking(|| undo_pending(&live_config()?)).await
}

/// "Finish the reset": the moves that were left, then a first start.
#[tauri::command]
pub async fn app_reset_finish() -> Result<ResetDone, String> {
    let Some(held) = Held::take() else {
        return Err("Your data is being moved. Try again when that finishes.".into());
    };
    let backups = blocking(|| finish_pending(&live_config()?)).await?;
    forget_teams();
    held.keep();
    Ok(done(backups))
}

/// "Set these notes aside" on the start screen: the journal renamed, the
/// app unlocked; the frontend restarts next.
#[tauri::command]
pub async fn app_reset_set_aside() -> Result<(), String> {
    let Some(_held) = Held::take() else {
        return Err("Your data is being moved. Try again when that finishes.".into());
    };
    blocking(|| set_aside(&live_config()?, &stamp_now()).map(|_| ())).await
}

/// The newest backup Delete all data left, for Settings > Storage.
#[tauri::command]
pub fn app_reset_backup() -> Option<String> {
    latest_backup().map(|p| p.display().to_string())
}

/// Async, so it runs off the main thread: the restart then goes through the
/// exit events, and the single-instance lock is let go before the relaunch.
#[tauri::command]
pub async fn app_restart(app: tauri::AppHandle) {
    app.restart()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const P1: &str = "1b4e28ba-2fa1-41d2-883f-0016d3cca427";
    const P2: &str = "6fa459ea-ee8a-4ca4-894e-db77e160355e";
    const P9: &str = "9a7b3c1d-5e2f-4a6b-8c9d-0e1f2a3b4c5d";
    const EXT: &str = "0123456789abcdef0123456789abcdef";
    const STAMP: &str = "2026-10-08-153000";

    /// The reset flips the app-wide "unfinished reset" flag: tests that run
    /// one take turns with the tests that read it, and leave it off.
    struct Serial(#[allow(dead_code)] std::sync::MutexGuard<'static, ()>);
    impl Drop for Serial {
        fn drop(&mut self) {
            crate::migrate::set_reset_pending(false);
        }
    }
    fn serial() -> Serial {
        let g = crate::migrate::serial();
        crate::migrate::set_reset_pending(false);
        Serial(g)
    }

    struct Fx {
        base: PathBuf,
        home: PathBuf,
        config: PathBuf,
        runtime: PathBuf,
    }

    impl Drop for Fx {
        fn drop(&mut self) {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                fn open_up(p: &Path) {
                    if fs::symlink_metadata(p).is_ok_and(|m| m.is_dir()) {
                        let _ = fs::set_permissions(p, fs::Permissions::from_mode(0o755));
                        for e in fs::read_dir(p).into_iter().flatten().flatten() {
                            open_up(&e.path());
                        }
                    }
                }
                open_up(&self.base);
            }
            let _ = fs::remove_dir_all(&self.base);
        }
    }

    fn put(path: &Path, body: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, body).unwrap();
    }

    /// A home folder with a used install in it: every kind of user data, the
    /// engine, the bundled library, the Widevine cache and one foreign file.
    fn install() -> Fx {
        let base = std::env::temp_dir().join(format!("shardx-reset-{}", uuid::Uuid::new_v4().simple()));
        let home = base.join("home");
        let config = home.join("Library").join(CONFIG_NAME);
        let runtime = config.join("runtime");
        put(&config.join("settings.json"), r#"{"theme":"dark"}"#);
        put(&config.join("proxies.json"), r#"{"proxies":[{},{}]}"#);
        put(&config.join("bookmarks.json"), r#"{"bookmarks":[{}]}"#);
        put(&config.join("automation.json"), r#"{"projects":[{},{},{}]}"#);
        for name in ["proxies-history.json", "psapi.json", "host-gl-caps.json", "tags.json", "folders.json"] {
            put(&config.join(name), "{}");
        }
        put(&config.join("automation-modules/m.wasm"), "wasm");
        put(&config.join("automation-modules/data/m/rows.txt"), "row");
        put(&config.join("automation-runs/1-p.log"), "log");
        put(&config.join(format!("profiles/{P1}.json")), r#"{"_meta":{}}"#);
        put(&config.join(format!("user-data/{P1}/Default/Cookies")), "cookies");
        put(&config.join(format!("extensions/{EXT}/manifest.json")), "{}");
        put(&config.join(format!("trash/{P2}.zip")), "zip");
        let bare = json!({ "name": "bundled", "navigator": { "platform": "MacIntel" } });
        put(&config.join("fingerprints/bundled.json"), &bare.to_string());
        let shipped = json!({ "id": "shipped", "label": "Shipped", "builtin": true, "payload": {} });
        put(&config.join("fingerprints/shipped.json"), &shipped.to_string());
        let mine = json!({ "id": "mine", "label": "Mine", "builtin": false, "payload": { "name": "mine" } });
        put(&config.join("fingerprints/mine.json"), &mine.to_string());
        put(&config.join("fingerprints/.staging/part.json"), "{}");
        put(&config.join("widevine-cdm/manifest.json"), "{}");
        put(&runtime.join("ShardX-Mac-arm64/ShardX.app/Contents/MacOS/ShardX"), "engine");
        put(&runtime.join("manifest.json"), "{}");
        put(&runtime.join("profile-icons/0a.png"), "png");
        put(&config.join("notes.txt"), "not the app's");
        Fx { base, home, config, runtime }
    }

    fn layout(fx: &Fx) -> Layout {
        Layout {
            config: fx.config.clone(),
            data: fx.config.clone(),
            runtime: Some(fx.runtime.clone()),
            home: Some(fx.home.clone()),
            shared: vec![fx.home.join("Documents")],
            same_disk,
        }
    }

    /// As `app_reset` runs it, the tracker and the restart aside.
    fn reset(l: &Layout) -> Result<Vec<PathBuf>> {
        let p = plan(l)?;
        if running_profile(&p).is_some() {
            bail!(STILL_RUNNING);
        }
        execute(&p, STAMP, &mut |_| Ok(()))
    }

    fn canon(p: &Path) -> PathBuf {
        fs::canonicalize(p).unwrap()
    }

    /// Every file under `p`, relative, with its bytes: what a tree holds.
    fn tree(p: &Path) -> Vec<(PathBuf, Vec<u8>)> {
        fn walk(root: &Path, at: &Path, out: &mut Vec<(PathBuf, Vec<u8>)>) {
            for e in fs::read_dir(at).unwrap().flatten() {
                let t = e.file_type().unwrap();
                if t.is_dir() {
                    walk(root, &e.path(), out);
                } else if t.is_file() {
                    out.push((e.path().strip_prefix(root).unwrap().to_path_buf(), fs::read(e.path()).unwrap()));
                }
            }
        }
        let mut out = Vec::new();
        walk(p, p, &mut out);
        out.sort();
        out
    }

    /// A data folder moved among the user's own files.
    fn moved(fx: &Fx) -> PathBuf {
        let m = fx.base.join("Volumes/T7/ShardData");
        put(&m.join(format!("profiles/{P9}.json")), "{}");
        put(&m.join(format!("profiles/{P9}.json.tmp")), "{}");
        put(&m.join(format!("user-data/{P9}/Default/Cookies")), "cookies");
        put(&m.join(format!("extensions/{EXT}/manifest.json")), "{}");
        put(&m.join(format!("trash/{P2}.zip")), "zip");
        put(&m.join(format!("trash/{P2}.json")), "{}");
        put(&m.join("holiday.jpg"), "the user's own");
        m
    }

    /// Anything under a "Volumes/T7" folder counts as another disk.
    fn t7_is_another_disk(a: &Path, b: &Path) -> bool {
        let on_t7 = |p: &Path| p.to_string_lossy().contains("/Volumes/T7");
        on_t7(a) == on_t7(b)
    }

    fn backups_beside(parent: &Path) -> Vec<PathBuf> {
        let mut v: Vec<_> = fs::read_dir(parent).unwrap().flatten()
            .filter(|e| e.file_name().to_str().is_some_and(|n| n.starts_with(BACKUP_PREFIX)))
            .map(|e| e.path())
            .collect();
        v.sort();
        v
    }

    #[test]
    fn nothing_is_deleted_everything_is_in_the_backup_and_the_app_is_clean() {
        let _s = serial();
        let fx = install();
        let before = tree(&fx.config);
        let backups = reset(&layout(&fx)).unwrap();
        let backup = &backups[0];
        assert_eq!(backups.len(), 1);
        assert_eq!(*backup, canon(fx.config.parent().unwrap()).join(format!("{BACKUP_PREFIX}{STAMP}")));
        let c = &fx.config;
        for gone in [
            "settings.json", "proxies.json", "proxies-history.json", "bookmarks.json",
            "tags.json", "folders.json",
            "automation.json", "psapi.json", "host-gl-caps.json", "automation-modules",
            "automation-runs", "profiles", "user-data", "extensions", "trash",
            "fingerprints/mine.json", "fingerprints/.staging", "runtime/profile-icons",
        ] {
            assert!(!c.join(gone).exists(), "{gone} should have left");
            assert!(backup.join(gone).exists(), "{gone} should be in the backup");
        }
        for kept in [
            "runtime/ShardX-Mac-arm64/ShardX.app/Contents/MacOS/ShardX", "runtime/manifest.json",
            "fingerprints/bundled.json", "fingerprints/shipped.json", "widevine-cdm/manifest.json",
            "notes.txt",
        ] {
            assert!(c.join(kept).exists(), "{kept} should stay");
        }
        // Nothing deleted: every file is in one of the two, byte for byte.
        let mut after = tree(&fx.config);
        after.extend(tree(backup).into_iter().filter(|(p, _)| p != Path::new(README) && p != Path::new(RECOVERY)));
        after.sort();
        assert_eq!(before, after);
        // The notes stay with the backup; the journal is gone; the app is open.
        let recovery = fs::read_to_string(backup.join(RECOVERY)).unwrap();
        assert!(recovery.contains("planned: ") && recovery.contains("moved: "), "{recovery}");
        assert!(backup.join(README).exists());
        assert!(!c.join(JOURNAL).exists());
        assert!(!crate::migrate::reset_pending());
    }

    #[test]
    fn a_plan_counts_what_the_person_counts() {
        let _s = serial();
        let fx = install();
        let p = plan(&layout(&fx)).unwrap();
        assert_eq!(p.counts, Counts { profiles: 1, proxies: 2, fingerprints: 1, extensions: 1, bookmarks: 1, projects: 3 });
        let v = p.view(STAMP);
        assert!(v.id.starts_with(&format!("{STAMP}.")));
        assert_eq!(v.backups.len(), 1);
        assert!(v.backups[0].ends_with(&format!("{BACKUP_PREFIX}{STAMP}")));
        assert!(v.total_bytes > 0);
        assert!(v.entries.iter().any(|e| e.ends_with("settings.json")));
        // Reading it moved nothing.
        assert!(fx.config.join("settings.json").exists());
    }

    /// A plan that no longer matches the disk is refused, and so is an id
    /// that could steer the backup's name.
    #[test]
    fn a_plan_that_changed_is_refused() {
        let _s = serial();
        let fx = install();
        let shown = plan(&layout(&fx)).unwrap().digest();
        put(&fx.config.join(format!("profiles/{P2}.json")), "{}");
        let now = plan(&layout(&fx)).unwrap();
        assert_ne!(now.digest(), shown);
        assert_eq!(now.counts.profiles, 2);
        // A browser growing its folder is not a change.
        put(&fx.config.join(format!("user-data/{P1}/Default/History")), "more");
        assert_eq!(plan(&layout(&fx)).unwrap().digest(), now.digest());

        for bad in ["../x.abc", "2026-10-08-15300.abc", "2026-10-08/153000.abc", "2026-10-08-153000.", "nonsense"] {
            assert!(stamp_of(bad).is_none(), "{bad}");
        }
        assert_eq!(stamp_of("2026-10-08-153000.ab12"), Some((STAMP, "ab12")));
    }

    #[test]
    fn a_backup_name_that_exists_gets_a_new_name_and_is_never_written_into() {
        let _s = serial();
        let fx = install();
        let parent = canon(fx.config.parent().unwrap());
        let taken = parent.join(format!("{BACKUP_PREFIX}{STAMP}"));
        put(&taken.join("someone.txt"), "already here");
        let backup = reset(&layout(&fx)).unwrap().remove(0);
        assert_eq!(backup, parent.join(format!("{BACKUP_PREFIX}{STAMP}-2")));
        assert_eq!(tree(&taken), vec![(PathBuf::from("someone.txt"), b"already here".to_vec())]);
        assert!(backup.join("settings.json").exists());
    }

    /// I-2: a data folder on another disk gets its own backup on that disk,
    /// under the same name, so every move stays a rename.
    #[test]
    fn a_data_folder_on_another_disk_gets_its_own_backup() {
        let _s = serial();
        let fx = install();
        let m = moved(&fx);
        let l = Layout { data: m.clone(), same_disk: t7_is_another_disk, ..layout(&fx) };
        let p = plan(&l).unwrap();
        assert_eq!(p.spots, vec![canon(fx.config.parent().unwrap()), canon(m.parent().unwrap())]);
        let backups = reset(&l).unwrap();
        assert_eq!(backups.len(), 2);
        assert_eq!(backups[0].file_name(), backups[1].file_name());
        assert_eq!(backups[1], canon(&fx.base.join("Volumes/T7")).join(format!("{BACKUP_PREFIX}{STAMP}")));
        assert!(backups[1].join(format!("{MOVED_DATA}/profiles/{P9}.json")).exists());
        assert!(backups[1].join(format!("{MOVED_DATA}/user-data/{P9}/Default/Cookies")).exists());
        assert!(!backups[0].join(MOVED_DATA).exists());
        assert!(backups[0].join("settings.json").exists());
        // Each says where the other part is.
        assert!(fs::read_to_string(backups[1].join(README)).unwrap().contains(&backups[0].display().to_string()));
        // On one disk, one backup.
        let fx = install();
        let m = moved(&fx);
        let p = plan(&Layout { data: m, ..layout(&fx) }).unwrap();
        assert_eq!(p.spots.len(), 1);
    }

    /// I-2: no copying anywhere; a rename across disks is a failure.
    #[test]
    fn a_move_across_disks_fails_and_nothing_is_copied() {
        #[cfg(unix)]
        assert!(crosses_devices(&std::io::Error::from_raw_os_error(libc::EXDEV)));
        assert!(!crosses_devices(&std::io::Error::from(ErrorKind::PermissionDenied)));
        let src = include_str!("reset.rs");
        for gone in [format!("fs::{}(", "copy"), format!("fn copy_{}", "tree"), format!("copy_then_{}", "remove")] {
            assert!(!src.contains(&gone), "{gone} is still in reset.rs");
        }
    }

    /// A move that fails midway puts every entry back, and the backup, its
    /// notes and the journal go.
    #[test]
    fn a_move_that_fails_midway_rolls_back_fully() {
        let _s = serial();
        let fx = install();
        let m = moved(&fx);
        let before = (tree(&fx.config), tree(&m));
        let l = Layout { data: m.clone(), ..layout(&fx) };
        let p = plan(&l).unwrap();
        let mut n = 0;
        let e = execute(&p, STAMP, &mut |step| {
            n += 1;
            match step {
                JStep::Move { to, .. } if to.ends_with("automation-modules") => bail!("the disk went away"),
                _ => Ok(()),
            }
        })
        .unwrap_err()
        .to_string();
        assert!(n > 10, "{n} steps ran first");
        assert!(e.starts_with("Nothing was moved"), "{e}");
        assert_eq!((tree(&fx.config), tree(&m)), before);
        assert!(m.join("profiles").is_dir() && m.join("user-data").is_dir());
        assert!(backups_beside(&canon(fx.config.parent().unwrap())).is_empty(), "the emptied backup goes");
        assert!(!fx.config.join(JOURNAL).exists());
        assert!(!crate::migrate::reset_pending());
    }

    /// The same for a real failure: a folder the move can't take from.
    #[cfg(unix)]
    #[test]
    fn a_folder_that_refuses_the_move_rolls_everything_back() {
        use std::os::unix::fs::PermissionsExt;
        let _s = serial();
        let fx = install();
        let m = moved(&fx);
        let before = (tree(&fx.config), tree(&m));
        fs::set_permissions(m.join("trash"), fs::Permissions::from_mode(0o555)).unwrap();
        let l = Layout { data: m.clone(), ..layout(&fx) };
        let e = reset(&l).unwrap_err().to_string();
        assert!(e.starts_with("Nothing was moved"), "{e}");
        fs::set_permissions(m.join("trash"), fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!((tree(&fx.config), tree(&m)), before);
        // With the folder writable again, the retry goes through.
        reset(&l).unwrap();
        assert!(!fx.config.join("settings.json").exists());
    }

    /// I-3: when the undo itself fails, the backup keeps its README and
    /// RECOVERY.txt, the journal stays, and the error names the backup.
    #[test]
    fn a_failed_undo_keeps_the_notes_and_the_journal() {
        let _s = serial();
        let fx = install();
        let p = plan(&layout(&fx)).unwrap();
        let config = fx.config.clone();
        let e = execute(&p, STAMP, &mut |step| match step {
            JStep::Move { to, .. } if to.ends_with("automation-modules") => {
                // Something new where proxies.json was: it can't go back.
                put(&config.join("proxies.json"), "{\"proxies\":[]}");
                bail!("the disk went away")
            }
            _ => Ok(()),
        })
        .unwrap_err()
        .to_string();
        let backup = canon(fx.config.parent().unwrap()).join(format!("{BACKUP_PREFIX}{STAMP}"));
        assert!(e.starts_with(&format!("Some data is still in {}", backup.display())), "{e}");
        assert!(backup.join(README).exists());
        let recovery = fs::read_to_string(backup.join(RECOVERY)).unwrap();
        assert!(recovery.contains("put back: ") && recovery.contains("proxies.json"), "{recovery}");
        assert!(backup.join("proxies.json").exists(), "the stuck item stays in the backup");
        assert!(fx.config.join(format!("profiles/{P1}.json")).exists(), "the rest went back");
        assert!(fx.config.join(JOURNAL).exists());
        assert!(crate::migrate::reset_pending());
        assert!(pending_view(&canon(&fx.config)).is_some_and(|v| v.error.is_none() && v.moved == 1));
    }

    /// A reset stopped partway (a crash, a forced quit): the journal is there
    /// and some moves were made. Returns the config and the journal.
    fn half_done(fx: &Fx, l: &Layout) -> (PathBuf, Journal) {
        let p = plan(l).unwrap();
        let j = start(&p, STAMP).unwrap();
        let moves: Vec<_> = j.steps.iter().filter(|s| matches!(s, JStep::Move { .. })).cloned().collect();
        for step in &moves[..moves.len() / 2] {
            apply(&j, step).unwrap();
        }
        // The next start made empty folders again before anything was asked.
        fs::create_dir_all(fx.config.join("profiles")).ok();
        (canon(&fx.config), j)
    }

    /// I-4: Undo the reset puts every item back from a half-done journal.
    #[test]
    fn undo_puts_a_half_done_reset_back() {
        let _s = serial();
        let fx = install();
        let m = moved(&fx);
        let before = (tree(&fx.config), tree(&m));
        let l = Layout { data: m.clone(), same_disk: t7_is_another_disk, ..layout(&fx) };
        let (config, j) = half_done(&fx, &l);
        assert!(read_journal(&config).unwrap().is_some_and(|r| r == j));
        let v = pending_view(&config).unwrap();
        assert!(v.moved > 0 && v.moved < v.total && v.backups.len() == 2, "{v:?}");
        undo_pending(&config).unwrap();
        assert_eq!((tree(&fx.config), tree(&m)), before);
        assert!(!config.join(JOURNAL).exists());
        for b in &j.backups {
            assert!(!b.exists(), "{} should go", b.display());
        }
        assert!(!crate::migrate::reset_pending());
    }

    /// I-4: Finish the reset makes the moves that were left.
    #[test]
    fn finish_completes_a_half_done_reset() {
        let _s = serial();
        let fx = install();
        let m = moved(&fx);
        let l = Layout { data: m.clone(), same_disk: t7_is_another_disk, ..layout(&fx) };
        let (config, j) = half_done(&fx, &l);
        let backups = finish_pending(&config).unwrap();
        assert_eq!(backups, j.backups);
        for gone in ["settings.json", "proxies.json", "profiles", "user-data"] {
            assert!(!fx.config.join(gone).exists(), "{gone} should have left");
            assert!(backups[0].join(gone).exists(), "{gone} should be in the backup");
        }
        assert!(backups[1].join(format!("{MOVED_DATA}/profiles/{P9}.json")).exists());
        assert!(m.join("holiday.jpg").exists());
        assert!(!config.join(JOURNAL).exists());
        assert!(backups[0].join(README).exists());
    }

    /// I-A: with the disk of a backup away, Undo and Finish move nothing and
    /// keep the journal; once it is back, Undo puts everything back.
    #[test]
    fn undo_and_finish_wait_for_a_disk_that_is_not_there() {
        let _s = serial();
        let fx = install();
        let m = moved(&fx);
        let before = (tree(&fx.config), tree(&m));
        let l = Layout { data: m.clone(), same_disk: t7_is_another_disk, ..layout(&fx) };
        let (config, j) = half_done(&fx, &l);
        let away = j.backups[1].with_file_name("unplugged");
        fs::rename(&j.backups[1], &away).unwrap();
        let held = (tree(&fx.config), tree(&j.backups[0]));
        for e in [undo_pending(&config).unwrap_err(), finish_pending(&config).map(|_| ()).unwrap_err()] {
            assert_eq!(e.to_string(), format!("Connect the disk with {}, then try again.", j.backups[1].display()));
        }
        assert_eq!((tree(&fx.config), tree(&j.backups[0])), held, "nothing moved");
        assert!(config.join(JOURNAL).exists());
        let v = pending_view(&config).unwrap();
        assert_eq!(v.missing, vec![j.backups[1].display().to_string()]);
        assert!(v.journal.ends_with(JOURNAL));

        fs::rename(&away, &j.backups[1]).unwrap();
        undo_pending(&config).unwrap();
        assert_eq!((tree(&fx.config), tree(&m)), before);
        assert!(!config.join(JOURNAL).exists());
    }

    /// I-A: a disk that goes away during the move: nothing is put back (its
    /// items would look as if they never moved), the notes and the journal
    /// stay, and the error says which disk to connect.
    #[test]
    fn a_disk_lost_during_the_move_keeps_everything_for_later() {
        let _s = serial();
        let fx = install();
        let m = moved(&fx);
        let before = (tree(&fx.config), tree(&m));
        let l = Layout { data: m.clone(), same_disk: t7_is_another_disk, ..layout(&fx) };
        let p = plan(&l).unwrap();
        let t7 = canon(&fx.base.join("Volumes/T7")).join(format!("{BACKUP_PREFIX}{STAMP}"));
        let away = t7.with_file_name("unplugged");
        let e = execute(&p, STAMP, &mut |step| match step {
            JStep::Move { to, .. } if to.ends_with("automation-modules") => {
                fs::rename(&t7, &away).unwrap();
                bail!("the disk went away")
            }
            _ => Ok(()),
        })
        .unwrap_err()
        .to_string();
        assert!(e.starts_with("Some data is still in"), "{e}");
        assert!(e.contains(&format!("Connect the disk with {}", t7.display())), "{e}");
        let config = canon(&fx.config);
        let home_backup = canon(fx.config.parent().unwrap()).join(format!("{BACKUP_PREFIX}{STAMP}"));
        assert!(home_backup.join("profiles").exists(), "nothing was put back");
        assert!(home_backup.join(README).exists() && home_backup.join(RECOVERY).exists());
        assert!(config.join(JOURNAL).exists());
        assert!(crate::migrate::reset_pending());

        // The disk is back: the next start's Undo restores everything.
        fs::rename(&away, &t7).unwrap();
        undo_pending(&config).unwrap();
        assert_eq!((tree(&fx.config), tree(&m)), before);
    }

    /// I-B: notes that can't be read name their file, and can be set aside
    /// under a new name (never deleted), which unlocks the app.
    #[test]
    fn unreadable_notes_can_be_set_aside_and_are_kept() {
        let _s = serial();
        let fx = install();
        let config = canon(&fx.config);
        fs::write(config.join(JOURNAL), b"{ not json").unwrap();
        crate::migrate::set_reset_pending(true);
        let v = pending_view(&config).unwrap();
        let e = v.error.unwrap();
        assert!(e.contains(&config.join(JOURNAL).display().to_string()), "{e}");
        assert_eq!(v.journal, config.join(JOURNAL).display().to_string());
        assert!(undo_pending(&config).is_err() && finish_pending(&config).is_err());

        let kept = set_aside(&config, STAMP).unwrap().unwrap();
        assert_eq!(kept, config.join(format!("reset-in-progress.set-aside-{STAMP}.json")));
        assert_eq!(fs::read(&kept).unwrap(), b"{ not json");
        assert!(!config.join(JOURNAL).exists());
        assert!(!crate::migrate::reset_pending());
        assert!(pending_view(&config).is_none());
        // A second set-aside never writes over the first.
        fs::write(config.join(JOURNAL), b"again").unwrap();
        let second = set_aside(&config, STAMP).unwrap().unwrap();
        assert_eq!(second, config.join(format!("reset-in-progress.set-aside-{STAMP}-2.json")));
        assert_eq!(fs::read(&kept).unwrap(), b"{ not json");
    }

    /// A journal that points outside the app is not followed.
    #[test]
    fn a_journal_that_points_elsewhere_is_refused() {
        let _s = serial();
        let fx = install();
        let (config, mut j) = half_done(&fx, &layout(&fx));
        j.steps.push(JStep::Move { from: fx.home.join("Documents/thesis.pdf"), to: j.backups[0].join("thesis.pdf") });
        fs::write(config.join(JOURNAL), serde_json::to_vec(&j).unwrap()).unwrap();
        assert!(undo_pending(&config).is_err());
        assert!(finish_pending(&config).is_err());
        assert!(pending_view(&config).is_some_and(|v| v.error.is_some()));
    }

    #[test]
    fn refuses_a_folder_without_the_markers_and_moves_nothing() {
        let _s = serial();
        let fx = install();
        fs::remove_file(fx.config.join("settings.json")).unwrap();
        assert!(reset(&layout(&fx)).is_err());
        assert!(fx.config.join(format!("profiles/{P1}.json")).exists());

        // A moved data folder with no profiles in it is not the app's.
        let fx = install();
        let elsewhere = fx.base.join("elsewhere");
        put(&elsewhere.join(format!("user-data/{P9}/x")), "someone else's");
        let l = Layout { data: elsewhere.clone(), ..layout(&fx) };
        assert!(reset(&l).is_err());
        assert!(elsewhere.join(format!("user-data/{P9}/x")).exists());
        assert!(fx.config.join("settings.json").exists());

        // A settings folder under another name is not the app's either.
        let fx = install();
        let other = fx.home.join("Library/other-app");
        put(&other.join("settings.json"), "{}");
        put(&other.join("profiles/a.json"), "{}");
        let l = Layout { config: other.clone(), data: other.clone(), ..layout(&fx) };
        assert!(reset(&l).is_err());
        assert!(other.join("profiles/a.json").exists());
    }

    #[test]
    fn refuses_home_the_disk_and_shared_folders() {
        let _s = serial();
        let fx = install();
        // The data folder may be moved anywhere; these never count.
        for bad in [fx.home.clone(), fx.base.clone(), PathBuf::from("/"), fx.home.join("Documents")] {
            // Marked as a data folder, so only the place itself is refused
            // (never written to at the top of the disk).
            if bad != Path::new("/") {
                fs::create_dir_all(bad.join("profiles")).unwrap();
            }
            let l = Layout { data: bad.clone(), ..layout(&fx) };
            assert!(reset(&l).is_err(), "{} should be refused", bad.display());
        }
        assert!(fx.config.join("settings.json").exists());
        assert!(fx.config.join(format!("profiles/{P1}.json")).exists());

        // A folder holding a shared one.
        fs::create_dir_all(fx.base.join("other/Documents")).unwrap();
        fs::create_dir_all(fx.base.join("other/profiles")).unwrap();
        let l = Layout { shared: vec![fx.base.join("other/Documents")], data: fx.base.join("other"), ..layout(&fx) };
        assert!(reset(&l).is_err());

        // A home folder that can't be found fails closed.
        let l = Layout { home: Some(fx.base.join("no-such-home")), ..layout(&fx) };
        assert!(reset(&l).is_err());
        assert!(fx.config.join("settings.json").exists());
    }

    #[cfg(unix)]
    #[test]
    fn refuses_a_top_level_folder() {
        let _s = serial();
        let fx = install();
        let l = Layout { data: PathBuf::from("/usr"), ..layout(&fx) };
        assert!(reset(&l).is_err());
    }

    /// A disk mounted on a folder (here one of the Mac's own system volumes,
    /// as `/Volumes/T7` would be) is the top of that disk, however deep.
    #[cfg(target_os = "macos")]
    #[test]
    fn refuses_a_folder_a_disk_is_mounted_on() {
        let _s = serial();
        let mount = ["/System/Volumes/VM", "/System/Volumes/Preboot"]
            .into_iter()
            .map(Path::new)
            .find(|p| p.exists())
            .expect("a macOS system volume");
        assert!(is_mount_point(mount));
        let fx = install();
        assert!(!is_mount_point(&canon(&fx.config)));
        let l = Layout { data: mount.to_path_buf(), ..layout(&fx) };
        let e = reset(&l).unwrap_err().to_string();
        assert!(e.contains("top of a disk"), "{e}");
    }

    /// A moved data folder gives up what has the app's shape, and keeps
    /// everything else; its folders go only once empty.
    #[test]
    fn a_moved_data_folder_keeps_the_users_own_files() {
        let _s = serial();
        let fx = install();
        let m = moved(&fx);
        put(&m.join("profiles/notes.txt"), "the user's");
        put(&m.join("extensions/mine.txt"), "the user's");
        put(&m.join("extensions/my-extension/manifest.json"), "the user's");
        put(&m.join("trash/keep.txt"), "the user's");
        let l = Layout { data: m.clone(), ..layout(&fx) };
        let backup = reset(&l).unwrap().remove(0);
        for gone in [
            format!("profiles/{P9}.json"), format!("profiles/{P9}.json.tmp"), format!("user-data/{P9}"),
            format!("extensions/{EXT}"), format!("trash/{P2}.zip"), format!("trash/{P2}.json"),
        ] {
            assert!(!m.join(&gone).exists(), "{gone} should have left");
            assert!(backup.join(MOVED_DATA).join(&gone).exists(), "{gone} should be in the backup");
        }
        assert!(!m.join("user-data").exists());
        for kept in ["profiles/notes.txt", "extensions/mine.txt", "extensions/my-extension/manifest.json", "trash/keep.txt", "holiday.jpg"] {
            assert!(m.join(kept).exists(), "{kept} should stay");
        }
        assert!(!fx.config.join("settings.json").exists());
    }

    /// Another app's "Profiles" (the case differs) is not the marker, and a
    /// "Trash" or "Extensions" of the user's is never touched.
    #[test]
    fn a_folder_named_like_the_apps_in_another_case_is_untouched() {
        let _s = serial();
        let fx = install();
        let other = fx.base.join("Mozilla/Firefox");
        put(&other.join(format!("Profiles/{P9}.json")), "firefox's");
        put(&other.join(format!("Extensions/{EXT}/x")), "firefox's");
        let l = Layout { data: other.clone(), ..layout(&fx) };
        let e = reset(&l).unwrap_err().to_string();
        assert!(e.contains("doesn’t look like ShardBrowser’s data folder"), "{e}");
        assert!(other.join(format!("Profiles/{P9}.json")).exists());
        assert!(other.join(format!("Extensions/{EXT}/x")).exists());
        assert!(fx.config.join("settings.json").exists());

        let work = fx.base.join("Dropbox/Work");
        put(&work.join(format!("profiles/{P9}.json")), "{}");
        put(&work.join(format!("Trash/{P2}.zip")), "the user's");
        put(&work.join(format!("Extensions/{EXT}/x")), "the user's");
        let l = Layout { data: work.clone(), ..layout(&fx) };
        let backup = reset(&l).unwrap().remove(0);
        assert!(backup.join(format!("{MOVED_DATA}/profiles/{P9}.json")).exists());
        assert!(work.join(format!("Trash/{P2}.zip")).exists());
        assert!(work.join(format!("Extensions/{EXT}/x")).exists());
        assert!(!backup.join(format!("{MOVED_DATA}/trash")).exists());
    }

    /// A browser from any run that still holds a profile stops the reset
    /// before anything moves; a stale lock does not.
    /// A profile copied from another computer keeps that machine's
    /// SingletonLock. Its pid means nothing here, and Stop acts on this
    /// answer now, so a foreign host must read as "not running".
    #[cfg(unix)]
    #[test]
    fn a_lock_from_another_machine_is_not_ours() {
        use std::os::unix::fs::symlink;
        let dir = std::env::temp_dir().join(format!("shardx-foreign-lock-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        // Our own live pid, so only the host can decide the answer.
        symlink(format!("not-this-machine-{}", std::process::id()), dir.join("SingletonLock")).unwrap();
        assert!(!lock_is_live(&dir));
        let host = this_host().unwrap();
        fs::remove_file(dir.join("SingletonLock")).unwrap();
        symlink(format!("{host}-{}", std::process::id()), dir.join("SingletonLock")).unwrap();
        assert!(lock_is_live(&dir));
        let _ = fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn a_browser_still_holding_a_profile_stops_the_reset() {
        use std::os::unix::fs::symlink;
        let _s = serial();
        let fx = install();
        let lock = fx.config.join(format!("user-data/{P1}/SingletonLock"));
        // The host this machine writes: a lock naming another one is not
        // ours and is ignored on purpose (a profile copied from elsewhere).
        let host = this_host().unwrap();
        symlink(format!("{host}-{}", std::process::id()), &lock).unwrap();
        let e = reset(&layout(&fx)).unwrap_err().to_string();
        assert_eq!(e, STILL_RUNNING);
        assert!(fx.config.join("proxies.json").exists());

        let mut child = std::process::Command::new("true").spawn().unwrap();
        let dead = child.id();
        child.wait().unwrap();
        fs::remove_file(&lock).unwrap();
        symlink(format!("some-host-{dead}"), &lock).unwrap();
        reset(&layout(&fx)).unwrap();
        assert!(!fx.config.join("user-data").exists());
    }

    /// I-1: a launch under way (counted before its network checks) refuses
    /// the reset.
    #[test]
    fn a_launch_under_way_refuses_the_reset() {
        let _s = serial();
        assert!(refuse_while_starting().is_ok());
        let launching = crate::launch::Launching::start();
        assert_eq!(refuse_while_starting(), Err(STARTING.to_string()));
        drop(launching);
        assert!(refuse_while_starting().is_ok());
    }

    /// A browser's last write (its run time) waits for the profiles to move;
    /// a write that got in first moves with them.
    #[test]
    fn a_late_profile_write_waits_for_the_move() {
        let _s = serial();
        let fx = install();
        let profiles = fx.config.join("profiles");
        let held = crate::space::profile::file_lock();
        let (tx, rx) = std::sync::mpsc::channel();
        let l = layout(&fx);
        let worker = std::thread::spawn(move || {
            tx.send(()).unwrap();
            reset(&l)
        });
        rx.recv().unwrap();
        std::thread::sleep(std::time::Duration::from_millis(200));
        put(&profiles.join(format!("{P2}.json")), "{}");
        drop(held);
        let backup = worker.join().unwrap().unwrap().remove(0);
        assert!(!profiles.exists());
        assert!(backup.join(format!("profiles/{P2}.json")).exists());
    }

    #[cfg(unix)]
    #[test]
    fn a_link_moves_as_a_link_and_is_never_followed() {
        use std::os::unix::fs::symlink;
        let _s = serial();
        let fx = install();
        let outside = fx.base.join("outside");
        put(&outside.join("keep.txt"), "not the app's");
        fs::remove_dir_all(fx.config.join("extensions")).unwrap();
        symlink(&outside, fx.config.join("extensions")).unwrap();
        symlink(&outside, fx.config.join(format!("user-data/{P1}/Default/linked"))).unwrap();
        symlink(outside.join("keep.txt"), fx.config.join("fingerprints/linked.json")).unwrap();
        let backup = reset(&layout(&fx)).unwrap().remove(0);
        assert!(outside.join("keep.txt").exists(), "the link's target stays");
        assert!(fs::symlink_metadata(fx.config.join("extensions")).is_err());
        assert!(fs::symlink_metadata(backup.join("extensions")).unwrap().file_type().is_symlink());
        assert!(fs::symlink_metadata(backup.join(format!("user-data/{P1}/Default/linked"))).unwrap().file_type().is_symlink());
        assert!(fs::symlink_metadata(backup.join("fingerprints/linked.json")).unwrap().file_type().is_symlink());
    }

    #[cfg(unix)]
    #[test]
    fn a_settings_folder_reached_through_a_link_is_checked_where_it_really_is() {
        use std::os::unix::fs::symlink;
        let _s = serial();
        let fx = install();
        let dir = fx.base.join("cfg");
        fs::create_dir_all(&dir).unwrap();
        put(&fx.home.join("settings.json"), "{}");
        fs::create_dir_all(fx.home.join("profiles")).unwrap();
        let link = dir.join(CONFIG_NAME);
        symlink(&fx.home, &link).unwrap();
        let l = Layout { config: link.clone(), data: link, ..layout(&fx) };
        assert!(reset(&l).is_err());
        assert!(fx.home.join("profiles").exists());
    }

    #[test]
    fn only_the_apps_own_names_have_its_shape() {
        let ok = |f, n: &str, d| app_named(f, OsStr::new(n), d);
        assert!(ok("profiles", &format!("{P1}.json"), false));
        assert!(!ok("profiles", "notes.json", false));
        assert!(!ok("profiles", &format!("{P1}.json"), true));
        assert!(ok("user-data", P1, true));
        assert!(!ok("user-data", "Default", true));
        assert!(ok("extensions", EXT, true));
        assert!(ok("extensions", "acmacodkjbdgmoleebolmdjonilkdbch", true));
        assert!(!ok("extensions", "my-extension", true));
        assert!(ok("trash", &format!("{P1}.zip"), false));
        assert!(!ok("trash", "keep.txt", false));
        assert!(!ok("Trash", &format!("{P1}.zip"), false));
    }

    #[test]
    fn a_stamp_reads_as_a_date() {
        assert_eq!(utc_stamp(0), "1970-01-01-000000");
        assert_eq!(utc_stamp(1_791_469_800), "2026-10-08-143000");
        let s = stamp_now();
        assert!(stamp_of(&format!("{s}.x")).is_some(), "{s}");
    }
}
