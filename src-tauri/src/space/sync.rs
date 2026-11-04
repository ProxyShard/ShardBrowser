//! Personal sync: the person's own data, kept here as always, mirrored to their space
//! on a team server (`/me`). A second computer uses the same token; the server tells devices apart.

mod book;
mod cycle;

use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};
use serde_json::json;

pub use book::Kinds;
use book::{Book, Sess};

use super::client::{Method, TeamClient, TeamError};
use super::error::CmdError;
use super::open::{self, Handoff, Outcome};
use super::profile::ProfileMeta;

// The backend's own words, as the interface knows them (SYNC_REFUSALS).
pub const ALREADY_CONNECTED: &str = "sync is already connected: disconnect first";
pub const NOT_CONNECTED: &str = "sync is not connected";
pub const NOT_MEMBER: &str = "not a member of that team";
pub const REMOVED: &str = "your synced data was removed from this server";
pub const DEVICE_NAME: &str = "a device name is 1 to 64 characters";

/// The server did not answer, by its host as an address bar shows it.
pub fn no_answer(server: &str) -> String {
    format!("the sync server at {} did not answer", host_of(server))
}

/// Turning sync off is refused while another computer has the profile open.
pub fn leased_stop(device: &str) -> String {
    format!("this profile is open on {device}: stop syncing it there")
}

fn host_of(server: &str) -> String {
    match url::Url::parse(server) {
        Ok(u) => match (u.host_str(), u.port()) {
            (Some(h), Some(p)) => format!("{h}:{p}"),
            (Some(h), None) => h.to_string(),
            _ => server.to_string(),
        },
        Err(_) => server.to_string(),
    }
}

pub(super) fn now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// A lease lasts this long on the server unless renewed.
const LEASE_TTL: u64 = 90;

// ---- what the interface is told ----

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SyncConnection {
    pub server: String,
    #[serde(rename = "teamId")]
    pub team_id: String,
    pub own: bool,
    pub device_name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SyncStatus {
    /// idle, syncing, offline or error.
    pub state: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_sync_at: Option<u64>,
    pub pending: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SyncState {
    pub rev: u64,
    pub connection: Option<SyncConnection>,
    pub kinds: Kinds,
    pub status: SyncStatus,
}

/// A personal row's sync, while sync is connected.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProfileSync {
    pub on: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub open_on: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seen_at: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conflict: Option<bool>,
}

/// Up by one on every change: the interface drops an older answer or event.
static REV: AtomicU64 = AtomicU64::new(1);

/// How the last try went, and whether one runs now.
#[derive(Default)]
struct Run {
    syncing: bool,
    offline: bool,
    error: Option<String>,
}

fn run() -> std::sync::MutexGuard<'static, Run> {
    static RUN: std::sync::Mutex<Run> = std::sync::Mutex::new(Run { syncing: false, offline: false, error: None });
    RUN.lock().unwrap_or_else(|e| e.into_inner())
}

/// Sync's state as the interface shows it, at the edition it holds now.
pub fn state() -> SyncState {
    state_at(REV.load(Ordering::SeqCst))
}

fn state_at(rev: u64) -> SyncState {
    let b = book::read();
    let Some(place) = crate::team::sync_place() else {
        let status = SyncStatus { state: "idle", last_sync_at: None, pending: 0, error: None };
        return SyncState { rev, connection: None, kinds: Kinds::default(), status };
    };
    let connection = Some(SyncConnection {
        server: place.server,
        team_id: place.team_id,
        own: place.own,
        device_name: crate::team::device_name(),
    });
    let r = run();
    let error = if b.removed { Some(REMOVED.to_string()) } else { r.error.clone() };
    let state = match () {
        _ if r.syncing => "syncing",
        _ if error.is_some() => "error",
        _ if r.offline => "offline",
        _ => "idle",
    };
    let status = SyncStatus { state, last_sync_at: b.last_sync_at, pending: pending(&b), error: error.filter(|_| state == "error") };
    SyncState { rev, connection, kinds: b.kinds.clone(), status }
}

/// What the server has not taken yet: the outbox, sessions and the kinds.
fn pending(b: &Book) -> usize {
    book::outbox().entries.len() + b.sessions.values().filter(|s| s.dirty).count() + usize::from(b.kinds_dirty)
}

/// Something changed: the next edition, told to every window.
fn changed() -> SyncState {
    // One at a time, so the editions go out in order.
    static TELLING: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _one = TELLING.lock().unwrap_or_else(|e| e.into_inner());
    let st = state_at(REV.fetch_add(1, Ordering::SeqCst) + 1);
    #[cfg(test)]
    tests::heard(&st);
    let tell = super::live::default_tell();
    tell("sync-changed", serde_json::to_value(&st).unwrap_or_default());
    st
}

fn rows_changed(topics: &[&str]) {
    let tell = super::live::default_tell();
    for t in topics {
        tell("store-changed", json!(t));
    }
}

fn connected() -> Result<crate::team::SyncPlace> {
    crate::team::sync_place().ok_or_else(|| anyhow::anyhow!(NOT_CONNECTED))
}

fn client() -> Result<TeamClient> {
    TeamClient::for_sync()
}

// ---- the commands ----

/// Connect to an invite's server and token, or to a team joined here.
pub async fn connect(server: Option<String>, token: Option<String>, team_id: Option<String>) -> Result<SyncState> {
    if crate::team::sync_place().is_some() {
        bail!(ALREADY_CONNECTED);
    }
    match (team_id, server, token) {
        (Some(team), _, _) => crate::team::sync_with_team(&team)?,
        (None, Some(server), Some(token)) => crate::team::sync_with_server(&server, &token).await?,
        _ => bail!("give the invite's server and token, or a team"),
    };
    book::reset(&Book::default())?;
    *run() = Run::default();
    // The space's document first: what syncs, and whose data this is.
    if let Ok(c) = client() {
        if let Err(e) = cycle::first_contact(&c).await {
            note_failure(&e);
        }
    }
    let st = changed();
    rows_changed(&["profiles"]);
    nudge();
    Ok(st)
}

/// Disconnect; with `remove`, the person's data on the server goes first.
/// What is here always stays. Changes not sent yet are dropped.
pub async fn disconnect(remove: bool) -> Result<SyncState> {
    let place = connected()?;
    let _turn = cycle::lock().await;
    if remove {
        let c = client()?;
        if let Err(e) = cycle::remove_all(&c).await {
            return Err(match e {
                TeamError::Offline(_) => anyhow::anyhow!(no_answer(&place.server)),
                e => anyhow::anyhow!("{e}"),
            });
        }
    }
    open::let_go_mine(None);
    crate::team::sync_forget()?;
    book::forget()?;
    *run() = Run::default();
    let st = changed();
    rows_changed(&["profiles"]);
    Ok(st)
}

/// What syncs, the same on every computer of the person's: goes up at the next sync.
pub fn set_kinds(kinds: Kinds) -> Result<SyncState> {
    connected()?;
    let was = book::change(|b| {
        let was = b.kinds.profiles;
        if b.kinds != kinds {
            b.kinds = kinds.clone();
            b.kinds_dirty = true;
        }
        was
    })?;
    cycle::scan();
    let st = changed();
    if was != kinds.profiles {
        rows_changed(&["profiles"]);
    }
    nudge();
    Ok(st)
}

/// Sync now: answered at once as "syncing", the end told as `sync-changed`.
pub async fn sync_now() -> Result<SyncState> {
    let place = connected()?;
    if book::read().removed {
        bail!(REMOVED);
    }
    let c = client()?;
    if let Err(TeamError::Offline(_)) = c.call(Method::GET, "/health", None).await {
        run().offline = true;
        // What waits is counted now, not at the cycle's next turn.
        cycle::scan();
        changed();
        bail!(no_answer(&place.server));
    }
    run().syncing = true;
    let st = changed();
    super::live::spawn(async {
        cycle::run_once().await;
    });
    Ok(st)
}

/// What another of the person's computers pastes: this server and this token.
pub fn invite() -> Result<String> {
    connected()?;
    crate::team::sync_invite()
}

pub fn set_device_name(name: &str) -> Result<SyncState> {
    connected()?;
    crate::team::set_device_name(name).map_err(|_| anyhow::anyhow!(DEVICE_NAME))?;
    Ok(changed())
}

/// Turn sync on or off for these profiles of this computer's.
pub fn profile_set(ids: &[String], on: bool) -> Result<Vec<ProfileMeta>> {
    connected()?;
    personal_space()?;
    for id in ids {
        if !super::local::profile::exists(id) {
            bail!("no profile {id} in your own space");
        }
        // A protected profile's data leaves this computer only sealed, and sync sends it readable.
        if on && super::local::profile::load_raw(id).is_ok_and(|p| p.meta.lock.is_some()) {
            bail!(super::local::protect::NOT_FOR_PROTECTED);
        }
    }
    let b = book::read();
    if !on {
        // Another computer runs it: stopping here would let both run it.
        if let Some(h) = ids.iter().find_map(|id| b.held_by(id)) {
            bail!(leased_stop(&h.device_name));
        }
    }
    book::change(|b| {
        for id in ids {
            if on {
                b.picked.insert(id.clone());
            } else {
                b.picked.remove(id);
                if b.kinds.profiles == book::Profiles::Selected {
                    b.conflicts.remove(id);
                }
            }
        }
    })?;
    cycle::scan();
    changed();
    rows_changed(&["profiles"]);
    nudge();
    rows(ids)
}

/// Take the profile from the computer holding it, which closes it at its next
/// renewal; what it had not sent comes back there as a conflict copy.
pub async fn profile_take(id: &str) -> Result<ProfileMeta> {
    let place = connected()?;
    personal_space()?;
    if !super::local::profile::exists(id) {
        bail!("no profile {id} in your own space");
    }
    let c = client()?;
    let path = format!("/me/profiles/{}/lease?takeover=true", super::admin::seg(id)?);
    match c.call(Method::POST, &path, None).await {
        Ok(_) => {}
        Err(TeamError::Offline(_)) => bail!(no_answer(&place.server)),
        Err(e) if e.status() == Some(404) => bail!("profile {id} is not on the sync server yet"),
        Err(e) => bail!("{e}"),
    }
    book::change(|b| {
        b.held.remove(id);
    })?;
    changed();
    rows_changed(&["profiles"]);
    rows(&[id.to_string()])?.pop().ok_or_else(|| anyhow::anyhow!("no profile {id} in your own space"))
}

fn personal_space() -> Result<()> {
    if crate::team::active_team().is_some() {
        return Err(CmdError::NotOffered(super::NOT_IN_TEAM).into());
    }
    Ok(())
}

/// These profiles' rows, with their sync.
fn rows(ids: &[String]) -> Result<Vec<ProfileMeta>> {
    let mut all = super::local::profile::list_all()?;
    all.retain(|m| ids.contains(&m.id));
    decorate(&mut all);
    Ok(all)
}

/// Each personal row's sync, while sync is connected.
pub fn decorate(rows: &mut [ProfileMeta]) {
    if crate::team::sync_place().is_none() {
        return;
    }
    let b = book::read();
    let mine = crate::team::device_id();
    for r in rows {
        r.sync = Some(profile_sync(&b, &r.id, &mine));
    }
}

fn profile_sync(b: &Book, id: &str, mine: &str) -> ProfileSync {
    if !b.syncs(id) {
        return ProfileSync { on: false, open_on: None, seen_at: None, conflict: None };
    }
    let held = b.held_by(id).filter(|h| h.device_id != mine);
    ProfileSync {
        on: true,
        open_on: held.map(|h| h.device_name.clone()),
        seen_at: held.map(|h| h.expires_at.saturating_sub(LEASE_TTL)),
        conflict: b.conflicts.contains(id).then_some(true),
    }
}

// ---- starting a synced profile ----

/// Picked to sync from this computer: a profile is not protected while it is (#127).
pub fn picked(id: &str) -> bool {
    let b = book::read();
    !b.removed && b.syncs(id)
}

/// Whether this profile syncs from here now; its book when it does.
fn synced(id: &str) -> Option<(crate::team::SyncPlace, Book)> {
    let place = crate::team::sync_place()?;
    let b = book::read();
    if b.removed || !b.syncs(id) {
        return None;
    }
    let stored = super::local::profile::load_raw(id).ok()?;
    (!stored.meta.temporary).then_some((place, b))
}

/// Refused while another of the person's computers has the profile open, by
/// what the server says now, or what it said last when it does not answer.
pub async fn refuse_if_open_elsewhere(id: &str) -> Result<()> {
    let Some((_, b)) = synced(id) else { return Ok(()) };
    let mine = crate::team::device_id();
    let last_heard = || match b.held_by(id).filter(|h| h.device_id != mine) {
        Some(h) => Err(CmdError::Leased(h.device_name.clone()).into()),
        None => Ok(()),
    };
    // Not answering lately: no wait for it at every start.
    let Ok(c) = client() else { return last_heard() };
    if run().offline {
        return last_heard();
    }
    #[derive(Deserialize)]
    struct Row {
        #[serde(default)]
        open_on: Option<super::remote::Lease>,
    }
    let path = format!("/me/profiles/{}", super::admin::seg(id)?);
    let held = match c.get::<Row>(&path).await {
        Ok(row) => {
            let held = row.open_on.filter(|l| l.device_id != mine).map(|l| book::Held {
                device_id: l.device_id,
                device_name: l.device_name,
                expires_at: l.expires_at,
            });
            let _ = book::change(|b| {
                match &held {
                    Some(h) => b.held.insert(id.to_string(), h.clone()),
                    None => b.held.remove(id),
                };
            });
            held
        }
        // Not on the server yet, or no answer: as last heard.
        Err(e) => {
            if matches!(e, TeamError::Offline(_)) {
                run().offline = true;
            }
            b.held_by(id).filter(|h| h.device_id != mine).cloned()
        }
    };
    match held {
        Some(h) => Err(CmdError::Leased(h.device_name).into()),
        None => Ok(()),
    }
}

/// A synced profile about to start: its lease taken, and its session brought up
/// to date or kept. None for one that does not sync, and for one started while
/// the server does not answer (its session goes up later).
pub async fn open_for_start(id: &str, udd: &Path, cdp: bool) -> Result<Option<Handoff>> {
    let Some((place, b)) = synced(id) else { return Ok(None) };
    if run().offline {
        return started_unsent(id, "it did not answer lately");
    }
    // Not on the server yet: it goes up first, or the lease has nothing to hold.
    if b.rec("profile", id).is_none() {
        if let Err(e) = cycle::push_one(id).await {
            eprintln!("[sync] {id}: not sent before its start: {e}");
        }
    }
    let name = super::local::profile::load_raw(id).map(|s| super::remote::name_of(&s)).unwrap_or_default();
    let (hand, server) = match open::open_own(&place.team_id, id, &name, udd.to_path_buf(), cdp).await {
        Ok(got) => got,
        // Open on another computer, or still closing here: refused.
        Err(e) if matches!(super::error::find(&e), Some(CmdError::Leased(_))) || e.to_string() == open::BUSY => {
            return Err(e)
        }
        // The person's own profile starts all the same; its session goes up later.
        Err(e) => {
            if matches!(super::error::find(&e), Some(CmdError::Offline(_))) {
                run().offline = true;
            }
            return started_unsent(id, &format!("{e:#}"));
        }
    };
    let here = book::read().sessions.get(id).copied();
    let has_data = udd.join("Default").exists();
    let took = match here {
        Some(Sess { rev, dirty: false }) if server <= rev => false,
        Some(Sess { dirty: false, .. }) => true,
        Some(Sess { rev, dirty: true }) if server == rev => false,
        None if server == 0 || !has_data => server > 0,
        // Changed here unsent while another computer sent its own: both kept.
        Some(Sess { dirty: true, .. }) | None => {
            cycle::conflict_copy(id, true)?;
            true
        }
    };
    if took {
        hand.take_servers().await?;
    } else {
        hand.keep_mine(server)?;
    }
    let _ = book::change(|b| {
        let dirty = !took && b.sessions.get(id).is_some_and(|s| s.dirty);
        b.sessions.insert(id.to_string(), Sess { rev: server, dirty });
    });
    Ok(Some(hand))
}

/// Started without the server: what it does goes up at the next sync.
fn started_unsent(id: &str, e: &str) -> Result<Option<Handoff>> {
    eprintln!("[sync] {id}: started without the sync server ({e})");
    let _ = book::change(|b| {
        let s = b.sessions.entry(id.to_string()).or_default();
        s.dirty = true;
    });
    Ok(None)
}

/// A synced profile's browser closed, or its session was sent while it was
/// closed: kept as sent, kept here for later, or kept as a conflict copy.
pub fn session_closed(id: &str, outcome: &Outcome, ran: bool) {
    if crate::team::sync_place().is_none() {
        return;
    }
    let r = match outcome {
        Outcome::Saved(rev) => book::change(|b| {
            b.sessions.insert(id.to_string(), Sess { rev: *rev, dirty: false });
        }),
        // Sent from another computer meanwhile, or taken from this one: both kept.
        Outcome::Stale if ran || book::read().sessions.get(id).is_some_and(|s| s.dirty) => {
            match cycle::conflict_copy(id, true) {
                Ok(_) => book::change(|b| {
                    b.sessions.remove(id);
                }),
                Err(e) => Err(e),
            }
        }
        Outcome::Stale | Outcome::Discarded | Outcome::Kept => Ok(()),
        Outcome::NotSaved(why) | Outcome::Unsent(why) => {
            eprintln!("[sync] {id}: session not sent ({why}); it goes at the next sync");
            book::change(|b| {
                b.sessions.entry(id.to_string()).or_default().dirty = true;
            })
        }
    };
    if let Err(e) = r {
        eprintln!("[sync] {id}: {e:#}");
    }
    if !super::reap::in_reaper() {
        changed();
        // A browser closed: sync goes soon. Not after sync's own upload, or one
        // the server keeps refusing would go round without end.
        if ran {
            nudge();
        }
    }
}

// ---- running ----

/// Set when sync reads again in the background; tests run it by hand.
static BACKGROUND: AtomicBool = AtomicBool::new(false);
static WAKE: tokio::sync::Notify = tokio::sync::Notify::const_new();
/// How long after a change it goes, so a burst of them goes as one.
const SETTLE: Duration = Duration::from_millis(1500);
/// How often it looks when nothing tells it to.
const EVERY: Duration = Duration::from_secs(60);
const RETRY: Duration = Duration::from_secs(15);

/// At start: sync runs in the background from here on.
pub fn start() {
    if BACKGROUND.swap(true, Ordering::SeqCst) {
        return;
    }
    // The first one soon after the start.
    WAKE.notify_one();
    super::live::spawn(async {
        loop {
            // Without an answer lately, asked again sooner.
            let every = if run().offline { RETRY } else { EVERY };
            let _ = tokio::time::timeout(every, WAKE.notified()).await;
            tokio::time::sleep(SETTLE).await;
            if crate::team::sync_place().is_some() && !super::reap::in_reaper() {
                cycle::run_once().await;
            }
        }
    });
}

/// Something of the person's changed here or there: sync soon.
pub fn nudge() {
    if BACKGROUND.load(Ordering::SeqCst) {
        WAKE.notify_one();
    }
}

/// The person's own space on `team` changed, as its live channel says.
pub fn heard_from(team: &str) {
    if crate::team::sync_place().is_some_and(|p| p.team_id == team) {
        nudge();
    }
}

/// A change written here, in the personal space: the outbox counts it, and sync goes soon.
pub fn touched() {
    if BACKGROUND.load(Ordering::SeqCst) && crate::team::sync_place().is_some() {
        nudge();
    }
}

fn note_failure(e: &TeamError) {
    let mut r = run();
    match e {
        TeamError::Offline(_) => {
            r.offline = true;
            r.error = None;
        }
        // In the words the interface reads a lost token by.
        e if e.code() == Some(&super::client::Code::Unauthorized) => {
            r.offline = false;
            r.error = Some("the server does not know this token".into());
        }
        e => {
            r.offline = false;
            r.error = Some(e.to_string());
        }
    }
}

/// Delete all data: what sync keeps here goes, the token with `teams.json`.
pub fn forget_local() -> Result<()> {
    book::forget()?;
    *run() = Run::default();
    Ok(())
}

#[cfg(test)]
mod tests;
