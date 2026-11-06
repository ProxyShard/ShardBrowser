//! The team in use, held in memory and nowhere else. Filled from its server on
//! a switch or the first read, kept current by the live channel, and dropped
//! on a switch away, a withdrawal or a restart.

use std::collections::{BTreeMap, HashMap};
use std::sync::{Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant};

use anyhow::Result;
use futures_util::StreamExt;
use serde::Serialize;
use serde_json::json;

use super::automation::Project;
use super::bookmarks::Bookmark;
use super::client::{TeamClient, TeamError};
use super::extensions::ExtensionEntry;
use super::folders::FolderState;
use super::live::Tell;
use super::perm;
use super::profile::{ProfileMeta, StoredProfile};
use super::proxy::{ProxyEntry, TestSnapshot};
use super::remote::{self, FolderRow, ItemRow, ProfileRow, ProfileWithData, ProxyRow, ProxyWithData};
use super::tags::TagState;

/// Events closer together than this are one refetch.
const COALESCE: Duration = Duration::from_millis(500);
/// How long a read waits for a fill before answering what is held.
const READY_WAIT: Duration = Duration::from_secs(8);
/// A read tries a failed fill again only after this.
const RETRY_AFTER: Duration = Duration::from_secs(10);
/// Proxy logins asked for at once.
const AT_ONCE: usize = 8;

/// Which of the team's lists.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Kinds(u8);

impl Kinds {
    pub const NONE: Kinds = Kinds(0);
    pub const PROFILES: Kinds = Kinds(1);
    pub const PROXIES: Kinds = Kinds(2);
    pub const FOLDERS: Kinds = Kinds(4);
    pub const BOOKMARKS: Kinds = Kinds(8);
    pub const EXTENSIONS: Kinds = Kinds(16);
    pub const AUTOMATION: Kinds = Kinds(32);
    pub const TAGS: Kinds = Kinds(64);
    pub const ALL: Kinds = Kinds(127);

    fn has(self, k: Kinds) -> bool {
        self.0 & k.0 == k.0
    }

    fn with(self, k: Kinds) -> Kinds {
        Kinds(self.0 | k.0)
    }

    /// What a `team-changed` event's `what` makes stale.
    pub fn of(what: &str) -> Kinds {
        match what {
            "profile" | "lease" | "session" => Kinds::PROFILES,
            "proxy" => Kinds::PROXIES,
            // A renamed folder moves its profiles.
            "folder" => Kinds::FOLDERS.with(Kinds::PROFILES),
            "bookmark" => Kinds::BOOKMARKS,
            "extension" | "package" => Kinds::EXTENSIONS,
            "automation" => Kinds::AUTOMATION,
            "tag" => Kinds::TAGS,
            // No trash in a team here; a rename is the live channel's.
            "trash" | "team" => Kinds::NONE,
            _ => Kinds::ALL,
        }
    }
}

/// The `store-changed` topic for each list.
const TOPICS: [(Kinds, &str); 7] = [
    (Kinds::PROFILES, "profiles"),
    (Kinds::PROXIES, "proxies"),
    (Kinds::FOLDERS, "folders"),
    (Kinds::BOOKMARKS, "bookmarks"),
    (Kinds::EXTENSIONS, "extensions"),
    (Kinds::AUTOMATION, "automation"),
    (Kinds::TAGS, "tags"),
];

/// Every topic, for a switch: each page reads again.
pub fn all_topics() -> impl Iterator<Item = &'static str> {
    TOPICS.iter().map(|(_, t)| *t)
}

const ITEMS: [(&str, Kinds); 4] = [
    ("bookmark", Kinds::BOOKMARKS),
    ("extension", Kinds::EXTENSIONS),
    ("automation", Kinds::AUTOMATION),
    ("tag", Kinds::TAGS),
];

/// How the team's lists stand.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Reach {
    /// Nothing has come back yet.
    #[default]
    Loading,
    Ready,
    /// The server did not answer; what is shown is the last read, if any.
    Offline,
    /// The server answered no.
    Refused,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize)]
pub struct TeamStatus {
    pub state: Reach,
    /// Why the last read failed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// Unix seconds of the last good read.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub synced_at: Option<u64>,
}

#[derive(Default)]
struct Snapshot {
    team: String,
    /// A read of every list has come back.
    full: bool,
    failed_at: Option<Instant>,
    status: TeamStatus,
    /// The permission edition the rights were last told under.
    epoch: Option<u64>,
    profiles: Vec<ProfileRow>,
    /// Settings read once, kept while their revision is the row's.
    details: HashMap<String, StoredProfile>,
    proxies: Vec<ProxyRow>,
    /// Logins of the proxies this member may use, by id, with the revision read.
    logins: HashMap<String, (u64, String)>,
    folders: Vec<FolderRow>,
    items: BTreeMap<&'static str, Vec<ItemRow>>,
    /// Proxy tests run here.
    tests: HashMap<String, Vec<TestSnapshot>>,
}

impl Snapshot {
    /// The settings read at the row's revision, while a right still reads them.
    fn detail(&self, row: &ProfileRow) -> Option<&StoredProfile> {
        self.details
            .get(&row.id)
            .filter(|d| d.meta.rev == row.revision && reads_settings(&perm::rights(&self.team, &row.id, &row.folder)))
    }

    /// A proxy's login, while the permission held still hands it out.
    fn login(&self, id: &str) -> Option<&str> {
        self.logins.get(id).filter(|_| perm::may_use_proxy(&self.team, id)).map(|(_, d)| d.as_str())
    }

    fn items(&self, kind: &str) -> &[ItemRow] {
        self.items.get(kind).map(Vec::as_slice).unwrap_or(&[])
    }
}

fn state() -> MutexGuard<'static, Option<Snapshot>> {
    static S: OnceLock<Mutex<Option<Snapshot>>> = OnceLock::new();
    S.get_or_init(Default::default).lock().unwrap_or_else(|e| e.into_inner())
}

/// One fill at a time, so an older answer never lands over a newer one.
fn fill_turn() -> &'static tokio::sync::Mutex<()> {
    static T: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();
    T.get_or_init(|| tokio::sync::Mutex::new(()))
}

/// `f` over what is held for `team`; None when nothing is.
fn with<T>(team: &str, f: impl FnOnce(&Snapshot) -> T) -> Option<T> {
    state().as_ref().filter(|s| s.team == team).map(f)
}

fn now_secs() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

// ---- reads ----

pub fn status(team: &str) -> TeamStatus {
    with(team, |s| s.status.clone()).unwrap_or_default()
}

/// Profiles by name: a team's rows have no order of their own.
pub fn profiles(team: &str) -> Vec<ProfileMeta> {
    let device = crate::team::device_id();
    let mut out = with(team, |s| {
        s.profiles.iter().map(|r| remote::meta(team, r, s.detail(r), &device)).collect::<Vec<_>>()
    })
    .unwrap_or_default();
    out.sort_by_cached_key(|m| (m.name.to_lowercase(), m.id.clone()));
    out
}

pub fn has_profile(team: &str, id: &str) -> bool {
    with(team, |s| s.profiles.iter().any(|r| r.id == id)).unwrap_or(false)
}

/// The settings when read, else what the row alone says.
pub fn profile(team: &str, id: &str) -> Option<StoredProfile> {
    with(team, |s| {
        let row = s.profiles.iter().find(|r| r.id == id)?;
        Some(s.detail(row).cloned().unwrap_or_else(|| remote::stored(row, None)))
    })
    .flatten()
}

/// Oldest change first, as the personal file keeps them: the list shows it reversed.
pub fn proxies(team: &str) -> Vec<ProxyEntry> {
    let mut rows = with(team, |s| {
        s.proxies
            .iter()
            .map(|r| (r.updated_at, r.name.to_lowercase(), remote::proxy(r, s.login(&r.id))))
            .collect::<Vec<_>>()
    })
    .unwrap_or_default();
    rows.sort_by(|a, b| (a.0, &a.1, &a.2.id).cmp(&(b.0, &b.1, &b.2.id)));
    rows.into_iter().map(|(_, _, e)| e).collect()
}

pub fn proxy(team: &str, id: &str) -> Option<ProxyEntry> {
    with(team, |s| {
        let row = s.proxies.iter().find(|r| r.id == id)?;
        Some(remote::proxy(row, s.login(id)))
    })
    .flatten()
}

pub fn folders(team: &str) -> FolderState {
    with(team, |s| remote::folders(&s.folders)).unwrap_or_default()
}

pub fn tags(team: &str) -> TagState {
    with(team, |s| remote::tags(s.items("tag"), &s.profiles)).unwrap_or_default()
}

pub fn bookmarks(team: &str) -> Vec<Bookmark> {
    with(team, |s| remote::bookmarks(s.items("bookmark"))).unwrap_or_default()
}

pub fn extensions(team: &str) -> Vec<ExtensionEntry> {
    with(team, |s| remote::extensions(s.items("extension"))).unwrap_or_default()
}

pub fn projects(team: &str) -> Vec<Project> {
    with(team, |s| remote::projects(s.items("automation"))).unwrap_or_default()
}

pub fn tests(team: &str, proxy_id: &str) -> Vec<TestSnapshot> {
    with(team, |s| s.tests.get(proxy_id).cloned().unwrap_or_default()).unwrap_or_default()
}

/// Keep a test of a team proxy, here only; answers it as kept.
pub fn keep_test(team: &str, proxy_id: &str, snap: TestSnapshot) -> TestSnapshot {
    let mut g = state();
    match g.as_mut().filter(|s| s.team == team && s.proxies.iter().any(|r| r.id == proxy_id)) {
        Some(s) => super::local::proxy::keep_test(s.tests.entry(proxy_id.to_string()).or_default(), snap),
        None => super::local::proxy::keep_test(&mut Vec::new(), snap),
    }
}

/// A team profile's settings, read from the server the first time. Without a
/// right that reads them, the row is all there is.
pub async fn profile_detail(team: &str, id: &str) -> Result<StoredProfile> {
    let found = with(team, |s| {
        let row = s.profiles.iter().find(|r| r.id == id)?;
        Some((row.clone(), s.detail(row).cloned()))
    })
    .flatten();
    let missing = || super::CmdError::Missing(format!("no profile {id} in this team"));
    let Some((row, cached)) = found else { return Err(missing().into()) };
    if let Some(d) = cached {
        return Ok(d);
    }
    if !reads_settings(&perm::rights(team, &row.id, &row.folder)) {
        return Ok(remote::stored(&row, None));
    }
    let client = TeamClient::for_team(team)?;
    let path = format!("/profiles/{}", super::admin::seg(id)?);
    match client.get::<ProfileWithData>(&path).await {
        Ok(got) => {
            let stored = remote::stored(&got.row, Some(&got.data));
            if let Some(s) = state().as_mut().filter(|s| s.team == team) {
                if s.profiles.iter().any(|r| r.id == id && r.revision == got.row.revision) {
                    s.details.insert(id.to_string(), stored.clone());
                }
            }
            Ok(stored)
        }
        // The rules moved since the list was read: the row is what may be known.
        Err(e) if e.status() == Some(403) => Ok(remote::stored(&row, None)),
        Err(e) if e.status() == Some(404) => Err(missing().into()),
        Err(e) => Err(e.into()),
    }
}

/// The server hands a profile's settings to whoever may use it, not to See alone.
fn reads_settings(r: &perm::Rights) -> bool {
    r.start || r.edit || r.cookies || r.take_out
}

// ---- written from here, before the server's news of it comes back ----

pub fn row(team: &str, id: &str) -> Option<ProfileRow> {
    with(team, |s| s.profiles.iter().find(|r| r.id == id).cloned()).flatten()
}

pub fn rows_in_folder(team: &str, folder: &str) -> Vec<ProfileRow> {
    with(team, |s| s.profiles.iter().filter(|r| r.folder == folder).cloned().collect()).unwrap_or_default()
}

pub fn proxy_row(team: &str, id: &str) -> Option<ProxyRow> {
    with(team, |s| s.proxies.iter().find(|r| r.id == id).cloned()).flatten()
}

/// `f` over the team's held lists; nothing when another team's are held.
fn change(team: &str, f: impl FnOnce(&mut Snapshot)) {
    if let Some(s) = state().as_mut().filter(|s| s.team == team) {
        f(s);
    }
}

/// A profile as written, with the settings sent when they were. One written
/// out of this member's sight leaves the list, as the server's will.
pub fn wrote_profile(team: &str, row: ProfileRow, data: Option<&str>) {
    if !perm::rights(team, &row.id, &row.folder).see {
        return dropped_profile(team, &row.id);
    }
    change(team, |s| {
        match data {
            Some(d) => {
                s.details.insert(row.id.clone(), remote::stored(&row, Some(d)));
            }
            None => {
                s.details.remove(&row.id);
            }
        }
        match s.profiles.iter_mut().find(|r| r.id == row.id) {
            Some(slot) => *slot = row,
            None => s.profiles.push(row),
        }
    });
}

pub fn dropped_profile(team: &str, id: &str) {
    change(team, |s| {
        s.profiles.retain(|r| r.id != id);
        s.details.remove(id);
    });
}

pub fn wrote_proxy(team: &str, row: ProxyRow, login: Option<String>) {
    let t = team_model::Target { profile_id: Some(&row.id), folder: None };
    if !perm::allows(team, team_model::Action::See, &t) {
        return dropped_proxy(team, &row.id);
    }
    change(team, |s| {
        match login {
            Some(l) => {
                s.logins.insert(row.id.clone(), (row.revision, l));
            }
            None => {
                s.logins.remove(&row.id);
            }
        }
        match s.proxies.iter_mut().find(|r| r.id == row.id) {
            Some(slot) => *slot = row,
            None => s.proxies.push(row),
        }
    });
}

pub fn dropped_proxy(team: &str, id: &str) {
    change(team, |s| {
        s.proxies.retain(|r| r.id != id);
        s.logins.remove(id);
        s.tests.remove(id);
    });
}

/// A folder kept, or its colour set; None leaves the colour as it is.
pub fn wrote_folder(team: &str, name: &str, color: Option<&str>) {
    change(team, |s| match s.folders.iter_mut().find(|f| f.name == name) {
        Some(f) => {
            if let Some(c) = color {
                f.color = c.to_string();
            }
        }
        None => s.folders.push(FolderRow { name: name.to_string(), color: color.unwrap_or_default().to_string() }),
    });
}

/// A folder renamed: its row and its profiles go with the name.
pub fn renamed_folder(team: &str, old: &str, new: &str) {
    change(team, |s| {
        for f in s.folders.iter_mut().filter(|f| f.name == old) {
            f.name = new.to_string();
        }
        for r in s.profiles.iter_mut().filter(|r| r.folder == old) {
            r.folder = new.to_string();
        }
        s.details.retain(|_, d| d.meta.folder != old);
    });
}

pub fn dropped_folder(team: &str, name: &str) {
    change(team, |s| s.folders.retain(|f| f.name != name));
}

/// One item of a kind as held, for the revision a write goes from.
pub fn item(team: &str, kind: &str, id: &str) -> Option<ItemRow> {
    with(team, |s| s.items(kind).iter().find(|r| r.id == id).cloned()).flatten()
}

/// An item as written from here, in the list's order; out of sight without the team's See.
pub fn wrote_item(team: &str, kind: &str, row: ItemRow) {
    let Some(&(kind, _)) = ITEMS.iter().find(|(k, _)| *k == kind) else { return };
    if !perm::allows(team, team_model::Action::See, &team_model::Target::team()) {
        return dropped_item(team, kind, &row.id);
    }
    change(team, |s| {
        let rows = s.items.entry(kind).or_default();
        match rows.iter_mut().find(|r| r.id == row.id) {
            Some(slot) => *slot = row,
            None => rows.push(row),
        }
        rows.sort_by(|a, b| (&a.name, &a.id).cmp(&(&b.name, &b.id)));
    });
}

pub fn dropped_item(team: &str, kind: &str, id: &str) {
    change(team, |s| {
        if let Some(rows) = s.items.iter_mut().find(|(k, _)| **k == kind).map(|(_, r)| r) {
            rows.retain(|r| r.id != id);
        }
    });
}

// ---- filling ----

/// Drop what is held unless it is `team`'s: the space moved, or the team went.
pub fn keep_only(team: Option<&str>) {
    let mut g = state();
    if g.as_ref().is_some_and(|s| Some(s.team.as_str()) != team) {
        *g = None;
    }
}

/// The team was left or withdrew this member.
pub fn forget(team: &str) {
    let mut g = state();
    if g.as_ref().is_some_and(|s| s.team == team) {
        *g = None;
    }
}

fn needs_fill(team: &str) -> bool {
    match state().as_ref().filter(|s| s.team == team) {
        Some(s) if s.full => false,
        Some(s) => s.failed_at.is_none_or(|t| t.elapsed() >= RETRY_AFTER),
        None => true,
    }
}

/// Before a read of the team in use: fill its lists if none are held, and
/// wait a while for them. Nothing to do in the personal space.
pub async fn ready() {
    let Some(team) = crate::team::active_team() else { return };
    if needs_fill(&team) {
        run_and_wait(team, true, super::live::default_tell()).await;
    }
}

/// Read again what a `team-changed` of `what` would, now: after a write that
/// moved more than its own row.
pub async fn refetch(team: &str, what: &str) {
    let _turn = fill_turn().lock().await;
    refresh_held(team, Kinds::of(what), &super::live::default_tell()).await;
}

/// Read every list of the team now, as on a switch, waiting a while for it.
pub async fn reload(team: &str, tell: Tell) {
    run_and_wait(team.to_string(), false, tell).await;
}

/// The fill goes on past the wait; it tells the windows when it lands.
async fn run_and_wait(team: String, only_if_needed: bool, tell: Tell) {
    let (done, wait) = tokio::sync::oneshot::channel();
    super::live::spawn(async move {
        let _turn = fill_turn().lock().await;
        if !only_if_needed || needs_fill(&team) {
            refresh_held(&team, Kinds::ALL, &tell).await;
        }
        let _ = done.send(());
    });
    let _ = tokio::time::timeout(READY_WAIT, wait).await;
}

struct Pending {
    team: String,
    kinds: Kinds,
}

fn pending() -> MutexGuard<'static, Option<Pending>> {
    static P: OnceLock<Mutex<Option<Pending>>> = OnceLock::new();
    P.get_or_init(Default::default).lock().unwrap_or_else(|e| e.into_inner())
}

/// The server said something of the team changed: refetch it once things settle.
pub fn heard(team: &str, what: &str, tell: &Tell) {
    schedule(team, Kinds::of(what), tell);
}

/// A new permission, or the channel opened again: anything may have moved.
pub fn heard_all(team: &str, tell: &Tell) {
    schedule(team, Kinds::ALL, tell);
}

fn schedule(team: &str, kinds: Kinds, tell: &Tell) {
    if kinds == Kinds::NONE || crate::team::active_team().as_deref() != Some(team) {
        return;
    }
    {
        let mut p = pending();
        if let Some(job) = p.as_mut().filter(|j| j.team == team) {
            job.kinds = job.kinds.with(kinds);
            return;
        }
        *p = Some(Pending { team: team.to_string(), kinds });
    }
    let tell = tell.clone();
    super::live::spawn(async move {
        tokio::time::sleep(COALESCE).await;
        let Some(job) = pending().take() else { return };
        let _turn = fill_turn().lock().await;
        refresh_held(&job.team, job.kinds, &tell).await;
    });
}

/// Why a fill came back empty-handed.
struct Failed {
    offline: bool,
    why: String,
}

impl From<TeamError> for Failed {
    fn from(e: TeamError) -> Self {
        Failed { offline: matches!(e, TeamError::Offline(_)), why: e.to_string() }
    }
}

impl From<anyhow::Error> for Failed {
    fn from(e: anyhow::Error) -> Self {
        Failed { offline: false, why: format!("{e:#}") }
    }
}

#[derive(Default)]
struct Got {
    profiles: Option<Vec<ProfileRow>>,
    proxies: Option<Vec<ProxyRow>>,
    logins: Vec<(String, u64, String)>,
    folders: Option<Vec<FolderRow>>,
    items: Vec<(&'static str, Vec<ItemRow>)>,
}

async fn maybe<T>(on: bool, f: impl std::future::Future<Output = Result<T, TeamError>>) -> Result<Option<T>, TeamError> {
    if on {
        f.await.map(Some)
    } else {
        Ok(None)
    }
}

async fn fetch(team: &str, kinds: Kinds) -> Result<Got, Failed> {
    let client = TeamClient::for_team(team)?;
    // The rights shown and the logins asked for follow the permission.
    if perm::held_epoch(team).is_none() {
        let issued = client.permission().await?;
        perm::accept(team, &issued.permission).map_err(|e| Failed { offline: false, why: e.to_string() })?;
    }
    let c = &client;
    let (profiles, proxies, folders, bookmarks, extensions, automation, tags) = tokio::try_join!(
        maybe(kinds.has(Kinds::PROFILES), c.get::<Vec<ProfileRow>>("/profiles")),
        maybe(kinds.has(Kinds::PROXIES), c.get::<Vec<ProxyRow>>("/proxies")),
        maybe(kinds.has(Kinds::FOLDERS), c.get::<Vec<FolderRow>>("/folders")),
        maybe(kinds.has(Kinds::BOOKMARKS), c.get::<Vec<ItemRow>>("/items/bookmark")),
        maybe(kinds.has(Kinds::EXTENSIONS), c.get::<Vec<ItemRow>>("/items/extension")),
        maybe(kinds.has(Kinds::AUTOMATION), c.get::<Vec<ItemRow>>("/items/automation")),
        maybe(kinds.has(Kinds::TAGS), c.get::<Vec<ItemRow>>("/items/tag")),
    )?;
    let logins = match &proxies {
        Some(rows) => logins(c, team, rows).await,
        None => Vec::new(),
    };
    let items = [("bookmark", bookmarks), ("extension", extensions), ("automation", automation), ("tag", tags)]
        .into_iter()
        .filter_map(|(k, rows)| rows.map(|r| (k, r)))
        .collect();
    Ok(Got { profiles, proxies, logins, folders, items })
}

/// The logins this member may read and has not read at this revision.
async fn logins(c: &TeamClient, team: &str, rows: &[ProxyRow]) -> Vec<(String, u64, String)> {
    let known: HashMap<String, u64> =
        with(team, |s| s.logins.iter().map(|(id, (rev, _))| (id.clone(), *rev)).collect()).unwrap_or_default();
    let wanted: Vec<String> = rows
        .iter()
        .filter(|r| perm::may_use_proxy(team, &r.id) && known.get(&r.id) != Some(&r.revision))
        .map(|r| r.id.clone())
        .collect();
    let got: Vec<Option<(String, u64, String)>> = futures_util::stream::iter(wanted)
        .map(|id| login(c, team, id))
        .buffer_unordered(AT_ONCE)
        .collect()
        .await;
    got.into_iter().flatten().collect()
}

async fn login(c: &TeamClient, team: &str, id: String) -> Option<(String, u64, String)> {
    let path = format!("/proxies/{}", super::admin::seg(&id).ok()?);
    match c.get::<ProxyWithData>(&path).await {
        Ok(p) => Some((p.row.id, p.row.revision, p.data)),
        Err(e) => {
            eprintln!("[launcher] team {team}: proxy {id} not read: {e}");
            None
        }
    }
}

/// Refetch, keep, and tell what changed. The caller holds the fill turn.
async fn refresh_held(team: &str, kinds: Kinds, tell: &Tell) {
    // Only the team in use is kept.
    if crate::team::active_team().as_deref() != Some(team) {
        return;
    }
    let kinds = if with(team, |s| s.full).unwrap_or(false) { kinds } else { Kinds::ALL };
    let got = fetch(team, kinds).await;
    let (changed, status) = keep(team, kinds, got);
    #[cfg(test)]
    FILLS.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    for (k, topic) in TOPICS {
        if changed.has(k) {
            tell("store-changed", json!(topic));
        }
    }
    if let Some(st) = status {
        tell("team-status", serde_json::to_value(st).unwrap_or_default());
    }
}

/// Fills done, for tests that count them.
#[cfg(test)]
static FILLS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// Put what came back in place; answers the lists that changed and the new status, if it moved.
fn keep(team: &str, kinds: Kinds, got: Result<Got, Failed>) -> (Kinds, Option<TeamStatus>) {
    let mut g = state();
    // Left or switched away while it was asked: checked under the lock a forget takes.
    if crate::team::active_team().as_deref() != Some(team) {
        return (Kinds::NONE, None);
    }
    if g.as_ref().is_none_or(|s| s.team != team) {
        *g = Some(Snapshot { team: team.to_string(), ..Default::default() });
    }
    let Some(s) = g.as_mut() else { return (Kinds::NONE, None) };
    let before = s.status.clone();
    let mut changed = Kinds::NONE;
    match got {
        Err(f) => {
            eprintln!("[launcher] team {team}: lists not read: {}", f.why);
            s.failed_at = Some(Instant::now());
            s.status.state = if f.offline { Reach::Offline } else { Reach::Refused };
            s.status.error = Some(f.why);
        }
        Ok(got) => {
            if let Some(rows) = got.profiles {
                if rows != s.profiles {
                    changed = changed.with(Kinds::PROFILES);
                }
                s.profiles = rows;
                let rows = &s.profiles;
                s.details.retain(|id, d| rows.iter().any(|r| &r.id == id && r.revision == d.meta.rev));
            }
            if let Some(rows) = got.proxies {
                if rows != s.proxies {
                    changed = changed.with(Kinds::PROXIES);
                }
                s.proxies = rows;
                let rows = &s.proxies;
                // A login whose proxy moved on, went, or may no longer be used is dropped.
                let before = s.logins.len();
                s.logins.retain(|id, (rev, _)| {
                    rows.iter().any(|r| &r.id == id && r.revision == *rev) && perm::may_use_proxy(team, id)
                });
                if s.logins.len() != before || !got.logins.is_empty() {
                    changed = changed.with(Kinds::PROXIES);
                }
                for (id, rev, data) in got.logins {
                    s.logins.insert(id, (rev, data));
                }
                s.tests.retain(|id, _| rows.iter().any(|r| &r.id == id));
            }
            if let Some(rows) = got.folders {
                if rows != s.folders {
                    changed = changed.with(Kinds::FOLDERS);
                }
                s.folders = rows;
            }
            for (kind, rows) in got.items {
                if s.items.get(kind) != Some(&rows) {
                    let bad = remote::unreadable(kind, &rows);
                    if bad > 0 {
                        eprintln!("[launcher] team {team}: {bad} {kind} item(s) not readable here");
                    }
                    if let Some((_, k)) = ITEMS.iter().find(|(name, _)| *name == kind) {
                        changed = changed.with(*k);
                    }
                    s.items.insert(kind, rows);
                }
            }
            // The rights shown are the permission's: a new edition is news for every row.
            let epoch = perm::held_epoch(team);
            if epoch != s.epoch {
                s.epoch = epoch;
                changed = changed.with(Kinds::PROFILES);
            }
            if kinds == Kinds::ALL {
                s.full = true;
            }
            s.failed_at = None;
            s.status = TeamStatus { state: Reach::Ready, error: None, synced_at: Some(now_secs()) };
        }
    }
    let moved = (s.status.state != before.state || s.status.error != before.error).then(|| s.status.clone());
    (changed, moved)
}

#[cfg(test)]
mod tests {
    use super::super::client::Method;
    use super::*;
    use crate::team::{self, Space};
    use serde_json::Value;
    use shardbrowser_team_server::{Action, Grant, NewMember, Scope, TestServer};
    use std::sync::Arc;

    fn on(scope: Scope, action: Action) -> Grant {
        Grant { scope, action, allow: true }
    }

    fn ads() -> Scope {
        Scope::Folder { name: "Ads".into() }
    }

    /// A team server in-process, its admin's data, and Vera joined from here.
    struct Team {
        // Stopped before its data goes with the root.
        srv: TestServer,
        root: crate::store::TestRoot,
        team_id: String,
        vera: NewMember,
        told: Arc<Mutex<Vec<(String, Value)>>>,
        // Switching waits on launches and data moves, which other tests make.
        _serial: std::sync::MutexGuard<'static, ()>,
    }

    impl Team {
        async fn new(grants: &[Grant]) -> Team {
            let serial = crate::migrate::serial();
            let root = crate::store::test_root("team-cache").with_config();
            let srv = TestServer::start(root.path.join("server")).await.unwrap();
            let vera = srv.add_member("Vera", grants).unwrap();
            team::join(&srv.url, &vera.token).await.unwrap();
            let team_id = srv.server.team_id().to_string();
            keep_only(None);
            Team { srv, root, team_id, vera, told: Default::default(), _serial: serial }
        }

        fn tell(&self) -> Tell {
            let told = self.told.clone();
            Arc::new(move |event: &str, payload| told.lock().unwrap().push((event.to_string(), payload)))
        }

        fn admin(&self) -> TeamClient {
            TeamClient::new(&self.srv.url, &self.srv.admin_token, "admin-device").unwrap()
        }

        async fn put(&self, path: &str, body: Value) {
            self.admin().call(Method::PUT, path, Some(&body)).await.unwrap();
        }

        async fn profile(&self, id: &str, folder: &str, settings: Value) {
            let body = json!({
                "name": format!("name of {id}"), "folder": folder,
                "data": b64(settings), "card": { "notes": format!("notes of {id}"), "tags": ["tag-1"] },
            });
            self.put(&format!("/profiles/{id}"), body).await;
        }

        fn ids(&self) -> Vec<String> {
            let mut ids: Vec<String> = profiles(&self.team_id).into_iter().map(|m| m.id).collect();
            ids.sort();
            ids
        }

        fn topics(&self) -> Vec<String> {
            let told = self.told.lock().unwrap();
            told.iter().filter(|(e, _)| e == "store-changed").filter_map(|(_, p)| p.as_str().map(String::from)).collect()
        }
    }

    fn b64(v: Value) -> String {
        use base64::Engine as _;
        base64::engine::general_purpose::STANDARD.encode(v.to_string())
    }

    #[tokio::test]
    async fn a_member_with_a_grant_on_one_folder_sees_exactly_it() {
        let t = Team::new(&[on(ads(), Action::See), on(ads(), Action::Start)]).await;
        for (name, color) in [("Ads", "red"), ("Shops", "blue")] {
            let body = json!({ "name": name, "color": color });
            t.admin().call(Method::POST, "/admin/folders", Some(&body)).await.unwrap();
        }
        let ua = json!({ "navigator": { "user_agent": "Mozilla/5.0 (Linux; Android 14)" } });
        t.profile("ads-1", "Ads", ua.clone()).await;
        t.profile("ads-2", "Ads", json!({})).await;
        t.profile("shop-1", "Shops", json!({})).await;
        t.profile("loose-1", "", json!({})).await;
        let proxy = json!({ "name": "DE", "kind": "socks5", "country": "DE", "data": b64(json!({ "host": "203.0.113.5", "port": 1080 })) });
        t.put("/proxies/x1", proxy).await;
        t.put("/items/bookmark/b1", json!({ "name": "Mail", "data": b64(json!({ "url": "https://mail.example" })) })).await;
        // Someone has one open elsewhere.
        t.admin().call(Method::POST, "/profiles/ads-2/lease", None).await.unwrap();

        super::super::ready().await;
        assert_eq!(t.ids(), ["ads-1", "ads-2"]);
        let rows = super::super::profile::list_all().unwrap();
        let open = rows.iter().find(|m| m.id == "ads-2").unwrap();
        let on = open.open_on.as_ref().expect("the lease shows");
        assert_eq!((on.device_name.as_str(), on.member_name.as_str(), on.this_device), (team::device_name().as_str(), "Admin", false));
        let r = rows[0].rights.unwrap();
        assert!(r.see && r.start && !r.edit && !r.delete && !r.cookies && !r.take_out, "{r:?}");
        assert_eq!(rows[0].tags.as_deref(), Some(&["tag-1".to_string()][..]));
        assert_eq!(rows[0].notes, "notes of ads-1");

        let f = super::super::folders::list().unwrap();
        assert_eq!(f.registry, ["Ads"]);
        assert_eq!(f.meta["Ads"].color.as_deref(), Some("red"));
        // Proxies and items are the team's: no team-wide See, none of them.
        assert!(super::super::proxy::list().unwrap().is_empty());
        assert!(super::super::bookmarks::list().unwrap().is_empty());
        assert!(super::super::trash::list().unwrap().is_empty());

        // Settings come when asked, as far as Start reaches.
        let got = super::super::profile::fetch("ads-1").await.unwrap();
        assert_eq!((got.meta.id.as_str(), got.meta.folder.as_str()), ("ads-1", "Ads"));
        assert_eq!(got.config["navigator"], ua["navigator"]);
        assert!(profiles(&t.team_id)[0].mobile, "read from the settings now held");
        let err = super::super::profile::fetch("shop-1").await.unwrap_err().to_string();
        assert_eq!(err, "no profile shop-1 in this team");
        assert!(super::super::profile::load_raw("shop-1").is_err());

        // A write needs its right, the team's items one on the whole team; nothing opens.
        use super::super::{CmdError, Door, Ticket};
        let tk = Ticket::now();
        let refused = |e: anyhow::Error| super::super::error::find(&e).cloned();
        let forbidden = |right, on| Some(CmdError::Forbidden(super::super::error::Need { right, on }));
        let mut copy = got.clone();
        assert_eq!(super::super::profile::save_raw(&mut copy).unwrap_err().to_string(), super::super::PERSONAL_ONLY);
        let moved = super::super::profile::set_folder(Door("profile_set_folder"), &tk, "ads-1", "").await;
        assert_eq!(refused(moved.unwrap_err()), forbidden("edit", "profile"));
        let gone = super::super::profile::remove(Door("profile_delete"), &tk, "ads-1").await;
        assert_eq!(refused(gone.unwrap_err()), forbidden("delete", "profile"));
        let entry = super::super::proxy::parse_single("203.0.113.7:1080").unwrap();
        let added = super::super::proxy::upsert(Door("proxy_save"), &tk, entry).await;
        assert_eq!(refused(added.unwrap_err()), forbidden("create", "team"));
        let tag = super::super::tags::Tag { id: String::new(), name: "x".into(), color: "red".into() };
        let made = super::super::tags::save(Door("tag_save"), &tk, tag, None).await;
        assert_eq!(refused(made.unwrap_err()), forbidden("create", "team"));
        let kept = super::super::folders::add(Door("folder_add"), &tk, "New").await;
        assert_eq!(refused(kept.unwrap_err()), forbidden("admin", "team"));
        let made = super::super::automation::create(Door("automation_create"), &tk, "x").await;
        assert_eq!(refused(made.unwrap_err()), forbidden("create", "team"));
        let gone = super::super::bookmarks::delete(Door("bookmark_delete"), &tk, "b1").await;
        assert_eq!(refused(gone.unwrap_err()), Some(CmdError::Missing("no bookmark b1 in this team".into())));
        let err = perm::start("ads-1").err().unwrap().to_string();
        assert_eq!(err, super::super::TEAM_NOT_YET);
        assert!(perm::cookies("someone-elses").is_err());
        drop(t.srv);
    }

    #[tokio::test]
    async fn the_list_dates_each_row_by_the_server_and_a_new_one_keeps_its_own_date() {
        let t = Team::new(&[on(Scope::Team, Action::See), on(ads(), Action::Create)]).await;
        // Seen only: its settings are never read here.
        let body = json!({ "name": "Seen", "folder": "Shops", "data": b64(json!({})), "created_at": 1_700_000_000 });
        t.put("/profiles/seen-1", body).await;
        super::super::ready().await;
        let made = |id: &str| profiles(&t.team_id).into_iter().find(|m| m.id == id).and_then(|m| m.created_at);
        assert_eq!(made("seen-1").as_deref(), Some("@1700000000"));

        // Brought in with the ISO date of older data: the server keeps that date.
        let mut stored = StoredProfile::default();
        stored.meta.folder = "Ads".into();
        stored.meta.created_at = Some("2023-11-14T22:13:20Z".into());
        stored.config.insert("name".into(), json!("Imported"));
        let tk = super::super::Ticket::now();
        let m = super::super::profile::save(super::super::Door("profile_import"), &tk, stored, Default::default()).await.unwrap();
        assert_eq!(m.created_at.as_deref(), Some("@1700000000"));
        let rows: Vec<Value> = t.admin().get("/profiles").await.unwrap();
        let row = rows.iter().find(|r| r["id"] == m.id.as_str()).unwrap();
        assert_eq!(row["created_at"], 1_700_000_000);
        drop(t.srv);
    }

    #[tokio::test]
    async fn a_proxy_login_comes_only_with_a_right_that_uses_it() {
        let t = Team::new(&[on(Scope::Team, Action::See), on(Scope::Profile { id: "x2".into() }, Action::Start)]).await;
        for id in ["x1", "x2"] {
            let body = json!({ "name": id, "kind": "http", "data": b64(json!({ "host": format!("{id}.example"), "port": 8080 })) });
            t.put(&format!("/proxies/{id}"), body).await;
        }
        super::super::ready().await;
        let list = super::super::proxy::list().unwrap();
        let host = |id: &str| list.iter().find(|p| p.id == id).map(|p| p.host.clone()).unwrap();
        assert_eq!((host("x1").as_str(), host("x2").as_str()), ("", "x2.example"));
        assert_eq!(super::super::proxy::get("x2").unwrap().unwrap().port, 8080);
        drop(t.srv);
    }

    #[tokio::test]
    async fn a_team_changed_event_refreshes_within_a_second() {
        let t = Team::new(&[on(Scope::Team, Action::See)]).await;
        t.profile("p-1", "", json!({})).await;
        super::super::ready().await;
        assert_eq!(t.ids(), ["p-1"]);
        // The channel's catch-up on opening is a fill: once it is done, the socket is open.
        let fills = || FILLS.load(std::sync::atomic::Ordering::SeqCst);
        let before = fills();
        super::super::live::follow_with(&t.team_id, t.tell());
        let start = Instant::now();
        while fills() == before {
            assert!(start.elapsed() < Duration::from_secs(5), "the channel did not open");
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        t.told.lock().unwrap().clear();

        // A burst of changes is one refetch.
        let before = fills();
        let start = Instant::now();
        for id in ["p-2", "p-3", "p-4"] {
            t.profile(id, "", json!({})).await;
        }
        let body = json!({ "name": "VIP", "data": b64(json!({ "color": "violet" })) });
        t.put("/items/tag/tag-1", body).await;
        while t.ids().len() < 4 || !t.topics().contains(&"tags".to_string()) {
            assert!(start.elapsed() < Duration::from_secs(1), "not refreshed within a second: {:?}", t.ids());
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert_eq!(t.ids(), ["p-1", "p-2", "p-3", "p-4"]);
        assert_eq!(super::super::tags::list().unwrap().tags[0].name, "VIP");
        tokio::time::sleep(COALESCE * 2).await;
        assert_eq!(fills(), before + 1, "{:?}", t.topics());

        // Rights moved: the rows are news though none of them changed.
        t.told.lock().unwrap().clear();
        let path = format!("/admin/members/{}/grants", t.vera.id);
        let grants = json!({ "grants": [on(Scope::Team, Action::See), on(Scope::Team, Action::Edit)] });
        t.admin().call(Method::PUT, &path, Some(&grants)).await.unwrap();
        let start = Instant::now();
        while !profiles(&t.team_id)[0].rights.unwrap().edit {
            assert!(start.elapsed() < Duration::from_secs(2), "the new rights did not show");
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let start = Instant::now();
        while !t.topics().contains(&"profiles".to_string()) {
            assert!(start.elapsed() < Duration::from_secs(1), "the rows were not told");
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        drop(t.srv);
    }

    /// Every file under `dir` but the server's own that names one of `needles`.
    /// teams.json is the membership: the team's id is expected there, nothing else.
    fn naming(dir: &std::path::Path, skip: &std::path::Path, team_id: &str, needles: &[&str], out: &mut Vec<String>) {
        let Ok(entries) = std::fs::read_dir(dir) else { return };
        for e in entries.flatten() {
            let p = e.path();
            if p.starts_with(skip) {
                continue;
            }
            let name = p.to_string_lossy().to_string();
            let membership = p.file_name().is_some_and(|f| f == "teams.json");
            let named = |text: &str| needles.iter().any(|n| text.contains(n) && !(membership && *n == team_id));
            if named(&name) {
                out.push(name.clone());
            }
            if p.is_dir() {
                naming(&p, skip, team_id, needles, out);
            } else if named(&String::from_utf8_lossy(&std::fs::read(&p).unwrap_or_default())) {
                out.push(name);
            }
        }
    }

    #[tokio::test]
    async fn after_a_restart_nothing_of_the_team_is_on_disk() {
        let all = [Action::See, Action::Start, Action::Edit, Action::Cookies, Action::TakeOut];
        let t = Team::new(&all.map(|a| on(Scope::Team, a))).await;
        let ids = ["team-prof-6f1c2a", "team-prof-91be07"];
        for id in ids {
            t.profile(id, "Secret-folder-4d2", json!({ "navigator": { "user_agent": "UA-team-7c1" } })).await;
        }
        // Nothing listens there, so a test of it fails at once.
        let proxy = json!({ "name": "proxy-name-58e", "kind": "socks5", "data": b64(json!({ "host": "127.0.0.1", "port": 1 })) });
        t.put("/proxies/team-proxy-2f8", proxy).await;
        t.put("/items/bookmark/team-mark-0d1", json!({ "name": "m", "data": b64(json!({ "url": "https://mark-team-77.example" })) })).await;

        // Used as a member would: switched to, listed, opened in the editor, a proxy tested.
        crate::space::switch(Space::Personal).await.unwrap();
        let st = crate::space::switch(Space::Team { team_id: t.team_id.clone() }).await.unwrap();
        assert_eq!(st.team_status.unwrap().state, Reach::Ready);
        assert_eq!(t.ids(), ids);
        for id in ids {
            super::super::profile::fetch(id).await.unwrap();
        }
        let p = super::super::proxy::get("team-proxy-2f8").unwrap().unwrap();
        assert_eq!((p.host.as_str(), p.port), ("127.0.0.1", 1));
        super::super::proxy::full_test(&p).await.unwrap();
        assert_eq!(super::super::proxy::history(&p.id).unwrap().len(), 1);
        assert!(super::super::proxy::latest_test(&p.id).is_some());
        assert_eq!(super::super::bookmarks::list().unwrap().len(), 1);
        let _ = super::super::profile::list_all();
        let _ = super::super::tags::list();

        // A restart: nothing held, the files read again, and the server gone.
        let team_id = t.team_id.clone();
        let needles: Vec<&str> = [team_id.as_str(), "Secret-folder-4d2", "UA-team-7c1", "proxy-name-58e", "team-proxy-2f8", "mark-team-77"]
            .into_iter()
            .chain(ids)
            .collect();
        let server_dir = t.root.path.join("server");
        let Team { srv, root, .. } = t;
        drop(srv);
        keep_only(None);
        perm::forget(&team_id);
        team::drop_cache();

        let mut found = Vec::new();
        let config = crate::store::config_root().unwrap();
        let data = crate::store::data_root().unwrap();
        // The scan finds a team's id where one is.
        let canary = config.join("canary.txt");
        std::fs::write(&canary, ids[0]).unwrap();
        naming(&data, &server_dir, &team_id, &needles, &mut found);
        assert_eq!(found, [canary.to_string_lossy().to_string()]);
        std::fs::remove_file(&canary).unwrap();
        found.clear();
        for dir in [&config, &data] {
            naming(dir, &server_dir, &team_id, &needles, &mut found);
        }
        assert!(found.is_empty(), "the team left files behind: {found:?}");

        // Started again with no server: the team is there, its lists are not.
        assert_eq!(team::active_team(), Some(team_id.clone()));
        super::super::ready().await;
        assert!(super::super::profile::list_all().unwrap().is_empty());
        assert!(super::super::proxy::list().unwrap().is_empty());
        let st = status(&team_id);
        assert_eq!(st.state, Reach::Offline, "{st:?}");
        drop(root);
    }

    #[test]
    fn an_event_names_what_to_refetch() {
        assert_eq!(Kinds::of("lease"), Kinds::PROFILES);
        assert_eq!(Kinds::of("folder"), Kinds::FOLDERS.with(Kinds::PROFILES));
        assert_eq!(Kinds::of("package"), Kinds::EXTENSIONS);
        assert_eq!(Kinds::of("trash"), Kinds::NONE);
        assert_eq!(Kinds::of("something-new"), Kinds::ALL);
        assert!(Kinds::ALL.has(Kinds::TAGS) && !Kinds::PROFILES.has(Kinds::PROXIES));
    }
}
