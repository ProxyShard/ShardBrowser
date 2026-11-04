//! A profile kept on a server, opened on this device: its lease taken and kept
//! alive, its session downloaded and unpacked, and when its browser closes the
//! session sent back and the copy here wiped. Nothing here asks which space is
//! in use, so a personal profile synced through `/me` opens the same way.
//! A team's profile opens in `team-open/<team>/<id>/`, outside the user data.
//! A synced personal profile opens on its own browser data, which stays: only
//! its marker and archive live in `team-open/<team>.me/<id>/`.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use std::time::Duration;

use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::sync::Notify;

use super::client::{Code, Method, SessionFront, TeamClient, TeamError};
use super::error::{CmdError, Doing, Need};
use super::perm::{self, Cookies, Granted, Start};
use super::profile::StoredProfile;
use super::proxy::ProxyEntry;
use super::{cache, pack, remote};

/// A lease lasts 90 s on the server; it is renewed well inside that.
const RENEW_EVERY: Duration = Duration::from_secs(30);
/// How long before the team's permission runs out the person is told.
const WARN_BEFORE: Duration = Duration::from_secs(120);
/// The largest session a server keeps, less room for the archive's own bytes.
const BUDGET: u64 = 252 * 1024 * 1024;
/// How long a quit waits for open profiles to be saved.
pub const QUIT_WAIT: Duration = Duration::from_secs(120);
/// Why a start finds the profile still busy here.
pub const BUSY: &str = "this profile is still opening or being saved here: start it again in a moment";
/// Why Delete all data and a data move wait: an open copy lives under the data folder.
pub const STILL_OPEN: &str = "A team profile is open or being saved here. Close it, wait until it is saved, then try again.";

/// How long a reaper tries to send a session back once the browser is closed;
/// what it could not send is kept for the next start.
const REAP_UPLOAD: Duration = Duration::from_secs(20);
/// A session the server did not take is sent again after this, doubling up to the last.
const RESEND_FIRST: Duration = Duration::from_secs(2);
const RESEND_LAST: Duration = Duration::from_secs(30);
/// How long a close against a deadline waits to give the lease back.
const GIVE_BACK_WITHIN: Duration = Duration::from_secs(3);

/// Where a session lives on its server.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Home {
    pub team: String,
    /// The member's own space there, under `/me`.
    #[serde(default)]
    pub mine: bool,
}

impl Home {
    pub fn team(team: &str) -> Home {
        Home { team: team.to_string(), mine: false }
    }

    /// The person's own space on that server, which sync follows.
    pub fn mine(team: &str) -> Home {
        Home { team: team.to_string(), mine: true }
    }

    fn path(&self, id: &str, what: &str) -> Result<String, TeamError> {
        let seg = super::admin::seg(id).map_err(|e| TeamError::Garbled(e.to_string()))?;
        Ok(format!("{}/profiles/{seg}/{what}", if self.mine { "/me" } else { "" }))
    }

    fn client(&self) -> Result<TeamClient, CmdError> {
        let c = if self.mine {
            // Sync moved to another server meanwhile: this space is not reached any more.
            TeamClient::for_sync().and_then(|c| match c.team() == Some(self.team.as_str()) {
                true => Ok(c),
                false => Err(anyhow::anyhow!(super::sync::NOT_CONNECTED)),
            })
        } else {
            TeamClient::for_team(&self.team)
        };
        c.map_err(|e| CmdError::Refused { status: 403, code: "no_access".into(), message: format!("{e:#}") })
    }

    /// Its folder under `team-open`.
    fn folder(&self) -> String {
        if self.mine {
            format!("{}.me", safe(&self.team))
        } else {
            safe(&self.team)
        }
    }
}

/// How an open profile closes; a later ask never softens an earlier one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum End {
    /// Its session goes back to the server.
    Upload,
    /// Left on disk for the team's new invite, which sends it back.
    Keep,
    /// Not sent: wiped as it is.
    Discard,
}

/// What became of a session when its browser closed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    Saved(u64),
    /// Saved from another device meanwhile: the server's copy stays, this one goes.
    Stale,
    NotSaved(String),
    /// Not sent in time, the server out of reach: the copy is kept here, and
    /// the next start sends it.
    Unsent(String),
    Discarded,
    Kept,
}

/// The marker beside an open copy: what a sweep needs after a crash.
#[derive(Debug, Serialize, Deserialize)]
struct Marker {
    home: Home,
    id: String,
    #[serde(default)]
    name: String,
    /// The session_rev downloaded; None until it was.
    #[serde(default)]
    base: Option<u64>,
    /// This opening: a copy made again in the same folder is another one.
    #[serde(default)]
    nonce: String,
    /// A synced personal profile's own browser data, which is never wiped.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    own: Option<PathBuf>,
    /// Its browser as spawned, for a reaper when the launcher dies before the browser writes its lock.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    browser: Option<crate::process::Spawned>,
}

const MARKER: &str = "open.json";
const ARCHIVE: &str = "session.zip";
/// Locked by whoever closes a copy whose launcher died: its reaper, or a sweep.
const REAPING: &str = "reaping";

/// One profile open, or opening, or closing, on this device.
pub struct Session {
    home: Home,
    id: String,
    name: String,
    nonce: String,
    /// Holds the browser data, the marker and an archive in transit.
    work: PathBuf,
    /// A synced personal profile's own browser data, used in place of `work/udd`.
    own: Option<PathBuf>,
    state: Mutex<State>,
    nudge: Notify,
    /// Held across a renewal, so none lands after the lease is given back.
    renewing: tokio::sync::Mutex<()>,
}

#[derive(Default)]
struct State {
    base: Option<u64>,
    /// The lease is this device's, as far as is known here.
    leased: bool,
    end: Option<End>,
    /// Its browser is tracked: a close is asked of the tracker.
    tracked: bool,
    stopped: bool,
    warned: bool,
    dir: Option<Materialized>,
    /// Held while this process closes a copy whose launcher died.
    reaping: Option<fs::File>,
    /// Another device took the lease while it was open here.
    lost: bool,
    /// What ends its browser with this launcher, where the OS has one.
    tie: Option<crate::process::Tie>,
    /// Its browser while it runs.
    browser: Option<crate::process::Spawned>,
    /// Its reaper, which holds the browser's job too (Windows).
    reaper: Option<super::reap::Reaper>,
}

fn open_now() -> MutexGuard<'static, Vec<Arc<Session>>> {
    static OPEN: OnceLock<Mutex<Vec<Arc<Session>>>> = OnceLock::new();
    OPEN.get_or_init(Default::default).lock().unwrap_or_else(|e| e.into_inner())
}

fn find(home: &Home, id: &str) -> Option<Arc<Session>> {
    open_now().iter().find(|s| s.home == *home && s.id == id).cloned()
}

fn unregister(s: &Arc<Session>) {
    open_now().retain(|o| !Arc::ptr_eq(o, s));
}

/// A profile open here that a quit closes, for the quit dialog.
#[derive(Debug, Clone, Serialize)]
pub struct Listed {
    pub team_id: String,
    pub profile_id: String,
    pub name: String,
    /// The person's own synced profile; `team_id` is then the sync server's.
    pub mine: bool,
}

/// The team and synced profiles open on this device, opening ones and closing ones too.
pub fn listed() -> Vec<Listed> {
    open_now()
        .iter()
        .map(|s| Listed { team_id: s.home.team.clone(), profile_id: s.id.clone(), name: s.name.clone(), mine: s.home.mine })
        .collect()
}

/// Whether the person's synced profile `id` is open, opening or closing here.
pub fn holds_mine(id: &str) -> bool {
    open_now().iter().any(|s| s.home.mine && s.id == id)
}

pub fn any_open() -> bool {
    !open_now().is_empty()
}

/// Refused while anything is open here: what is open lives under the data folder.
pub fn refuse_while_open() -> Result<(), String> {
    if any_open() {
        return Err(STILL_OPEN.into());
    }
    Ok(())
}

/// Ask a profile open here to close, as `end` says.
pub fn close(home: &Home, id: &str, end: End) {
    if let Some(s) = find(home, id) {
        s.ask(end);
    }
}

/// Ask every profile of a team open here to close; never the person's own synced ones.
pub fn close_team(team: &str, end: End) {
    let all: Vec<_> = open_now().iter().filter(|s| s.home.team == team && !s.home.mine).cloned().collect();
    for s in all {
        s.ask(end);
    }
}

/// Close a team's profiles with upload and wait for them, up to `limit`.
pub async fn close_team_and_wait(team: &str, limit: Duration) {
    close_team(team, End::Upload);
    let gone = || !open_now().iter().any(|s| s.home.team == team && !s.home.mine);
    wait(gone, limit).await;
}

/// Before the launcher exits: every profile open here closes and is sent back,
/// for `limit` at most. What is left then is wiped unsent.
pub async fn close_all(limit: Duration) {
    let all: Vec<_> = open_now().clone();
    for s in &all {
        s.ask(End::Upload);
    }
    wait(|| open_now().is_empty(), limit).await;
    let left: Vec<_> = open_now().clone();
    for s in left {
        // A whole team copy still to be sent stays for the next start's sweep.
        let whole = {
            let st = s.state();
            st.base.is_some() && st.end != Some(End::Discard)
        };
        if s.own.is_none() && whole {
            eprintln!("[launcher] {}: not saved in time; kept here and sent at the next start", s.id);
            s.stop_watch().await;
            s.take_copy().keep();
            unregister(&s);
            continue;
        }
        eprintln!("[launcher] {}: not saved in time, wiped as it was", s.id);
        s.ask(End::Discard);
        s.stop_watch().await;
        let copy = s.take_copy();
        let _ = tokio::task::spawn_blocking(move || copy.wipe()).await;
        unregister(&s);
        // A synced profile's own data stays, and goes up at the next sync.
        if s.own.is_some() {
            super::sync::session_closed(&s.id, &Outcome::NotSaved("not saved in time".into()), false);
        }
    }
}

async fn wait(done: impl Fn() -> bool, limit: Duration) {
    let until = tokio::time::Instant::now() + limit;
    while !done() && tokio::time::Instant::now() < until {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// The passes a team profile's start needs, judged on the server's row now:
/// Start, and Cookies too when debugging is on.
pub async fn passes(team: &str, id: &str, cdp: bool) -> Result<(Granted<Start>, Option<Granted<Cookies>>)> {
    let client = Home::team(team).client()?;
    // Just after the launcher started, its live channel may not have brought one yet.
    if perm::held_epoch(team).is_none() {
        let issued = client.permission().await.map_err(|e| from_team(e.into(), id))?;
        perm::accept(team, &issued.permission).map_err(|e| anyhow::anyhow!("the team's permission did not check out: {e}"))?;
    }
    let mut seen = looked(&client, team, id).await?;
    // A profile made a moment ago moves the rules: looked up again under the
    // permission that arrived, so a start right after a create is not refused.
    if perm::moved_since(team, &seen) {
        seen = looked(&client, team, id).await?;
    }
    let start = perm::team_pass::<Start>(team, &seen)?;
    let cookies = if cdp { Some(perm::team_pass::<Cookies>(team, &seen)?) } else { None };
    Ok((start, cookies))
}

/// The profile as the team's rows show it to this member, under the rules held now.
async fn looked(c: &super::client::TeamClient, team: &str, id: &str) -> Result<perm::Seen> {
    let seen = perm::look(c, team, &[id]).await.map_err(|e| from_team(e, id))?;
    seen.into_iter()
        .find(|s| s.id() == id)
        .ok_or_else(|| CmdError::Missing(format!("no profile {id} in this team")).into())
}

fn doing(id: &str) -> Doing<'_> {
    Doing { need: Need { right: "start", on: "profile" }, what: "profile", id, had: None }
}

fn from_team(e: anyhow::Error, id: &str) -> anyhow::Error {
    match e.downcast::<TeamError>() {
        Ok(t) => CmdError::from_team(t, &doing(id)).into(),
        Err(e) => e,
    }
}

/// What a team profile starts with: its settings, the proxy it is bound to,
/// its browser data, and the session to hand to the tracker.
pub struct Opened {
    pub stored: StoredProfile,
    pub proxy: Option<ProxyEntry>,
    pub udd: PathBuf,
    /// Where its extensions are unpacked, wiped with the copy.
    pub ext: PathBuf,
    pub hand: Handoff,
}

/// Open a team profile on this device: lease, session, unpacked copy.
pub async fn open_team(team: &str, start: &Granted<Start>, cdp: bool) -> Result<Opened> {
    let id = start.profile_id();
    let stored = cache::profile_detail(team, id).await?;
    let session = Session::claim(Home::team(team), id, &remote::name_of(&stored), cdp, None)?;
    // From here a start that stops short gives the lease back and wipes the copy.
    let hand = Handoff(Some(session.clone()));
    let front = session.begin().await?;
    let proxy = bound_proxy(id, &front)?;
    session.unpack(front.size).await?;
    Ok(Opened { stored, proxy, udd: session.udd(), ext: session.work.join("ext"), hand })
}

/// A personal profile synced through `/me`, opened on its own browser data at
/// `udd`: the lease taken and kept alive. Answers the server's session revision;
/// the caller then takes the server's session or keeps its own.
pub async fn open_own(team: &str, id: &str, name: &str, udd: PathBuf, cdp: bool) -> Result<(Handoff, u64)> {
    let session = Session::claim(Home::mine(team), id, name, cdp, Some(udd))?;
    let hand = Handoff(Some(session.clone()));
    session.take_lease().await?;
    let rev = session.server_rev().await?;
    Ok((hand, rev))
}

/// A synced profile's session sent up while nothing has it open: its own data
/// as made from `base`, under a lease taken for the upload. How it went is told
/// to sync as a close is.
pub async fn upload_own(team: &str, id: &str, name: &str, udd: PathBuf, base: u64) -> Result<Outcome> {
    let session = Session::claim(Home::mine(team), id, name, false, Some(udd))?;
    let hand = Handoff(Some(session.clone()));
    session.take_lease().await?;
    session.state().base = Some(base);
    Ok(hand.finish().await)
}

/// Synced personal profiles open here (all, or the one `only`) stop syncing:
/// the lease goes back now, and the browser, left running, closes with nothing sent.
pub fn let_go_mine(only: Option<&str>) {
    let all: Vec<_> = open_now().iter().filter(|s| s.home.mine && only.is_none_or(|id| s.id == id)).cloned().collect();
    for s in all {
        s.unsync();
        super::live::spawn(async move {
            s.stop_watch().await;
            s.give_back().await;
        });
    }
}

/// The proxy the server sent with the session. Bound to one that is not there
/// is an error: without it the profile would go out on this computer's IP.
fn bound_proxy(id: &str, front: &SessionFront) -> Result<Option<ProxyEntry>> {
    let Some(bound) = front.proxy_id.as_deref().map(str::trim).filter(|p| !p.is_empty()) else {
        return Ok(None);
    };
    let sent = front.proxy.clone().and_then(|v| serde_json::from_value::<remote::ProxyWithData>(v).ok());
    match sent.map(|p| remote::proxy(&p.row, Some(&p.data))) {
        Some(e) if !e.host.trim().is_empty() && e.port != 0 => Ok(Some(e)),
        _ => bail!(
            "profile {id} is bound to a proxy that no longer exists ({bound}) \
             — bind another proxy, or none, then start it again"
        ),
    }
}

/// An opened session until its browser is tracked: a start that stops short
/// drops it, and that gives the lease back and wipes the copy.
pub struct Handoff(Option<Arc<Session>>);

impl Handoff {
    pub fn session(&self) -> Option<Arc<Session>> {
        self.0.clone()
    }

    /// A synced profile: the server's session, unpacked over its own data.
    pub async fn take_servers(&self) -> Result<()> {
        let Some(s) = &self.0 else { bail!("the profile is not open") };
        let path = s.home.path(&s.id, "session").map_err(|e| s.refusal(e))?;
        let c = s.home.client()?;
        let front = c.download(&path, &s.archive()).await.map_err(|e| s.refusal(e))?;
        let (zip, udd, incoming) = (s.archive(), s.udd(), s.work.join("incoming"));
        let size = front.size;
        tokio::task::spawn_blocking(move || -> Result<()> {
            remove_all(&incoming)?;
            fs::create_dir_all(&incoming)?;
            if size > 0 {
                pack::unpack(&zip, &incoming)?;
            }
            let _ = fs::remove_file(&zip);
            // Whole, it takes the place of what was here.
            remove_all(&udd)?;
            fs::rename(&incoming, &udd)?;
            Ok(())
        })
        .await??;
        s.state().base = Some(front.session_rev);
        s.mark()
    }

    /// A synced profile: its own data stays as it is, made from revision `base`.
    pub fn keep_mine(&self, base: u64) -> Result<()> {
        let Some(s) = &self.0 else { bail!("the profile is not open") };
        s.state().base = Some(base);
        s.mark()
    }

    /// Its browser closed: the session goes back as it was asked to.
    pub async fn closed(self) {
        self.finish().await;
    }

    async fn finish(mut self) -> Outcome {
        match self.0.take() {
            Some(s) => s.finish().await,
            None => Outcome::Discarded,
        }
    }
}

impl Drop for Handoff {
    fn drop(&mut self) {
        if let Some(s) = self.0.take() {
            s.abandon();
        }
    }
}

impl Session {
    /// Take the profile's place on this device: its folder made fresh, marked,
    /// watched by a reaper, registered.
    fn claim(home: Home, id: &str, name: &str, cdp: bool, own: Option<PathBuf>) -> Result<Arc<Session>> {
        let work = root()?.join(home.folder()).join(safe(id));
        let mut all = open_now();
        if all.iter().any(|s| s.home == home && s.id == id) {
            bail!("{BUSY}");
        }
        // A reaper still closing what a launcher that died left in this folder.
        if work.join(REAPING).exists() && take_reaping(&work).is_none() {
            bail!("{BUSY}");
        }
        // A copy left unsent: it is sent first, never wiped by this opening.
        if let Some(m) = read_marker(&work).filter(|m| m.home == home && m.id == id) {
            if let Some(lock) = take_reaping(&work) {
                let left = Session::adopt(m, work, lock);
                all.push(left.clone());
                drop(all);
                super::live::spawn(async move {
                    left.close_orphan(None).await;
                });
            }
            bail!("{BUSY}");
        }
        let dir = Materialized::fresh(work.clone())?;
        let s = Arc::new(Session {
            home,
            id: id.to_string(),
            name: name.to_string(),
            nonce: uuid::Uuid::new_v4().simple().to_string(),
            work,
            own,
            state: Mutex::new(State { dir: Some(dir), ..Default::default() }),
            nudge: Notify::new(),
            renewing: tokio::sync::Mutex::new(()),
        });
        s.mark()?;
        // Without one, a launcher that is killed leaves the browser up and nobody renewing its lease.
        let reaper = super::reap::spawn(&s.work).map_err(|e| anyhow::anyhow!("the profile's guard process did not start: {e}"))?;
        s.state().reaper = Some(reaper);
        all.push(s.clone());
        drop(all);
        if !s.home.mine {
            super::live::opened(&s.home.team, super::live::Open { id: s.id.clone(), cdp });
        }
        Ok(s)
    }

    /// A copy a dead launcher left, taken over to be closed under its lock.
    fn adopt(m: Marker, work: PathBuf, reaping: fs::File) -> Arc<Session> {
        let dir = Materialized { path: Some(work.clone()) };
        // It held the lease when it died, maybe still, and its browser ran.
        let state = State {
            base: m.base,
            leased: true,
            tracked: true,
            dir: Some(dir),
            reaping: Some(reaping),
            browser: m.browser,
            ..Default::default()
        };
        Arc::new(Session {
            name: if m.name.is_empty() { m.id.clone() } else { m.name },
            home: m.home,
            id: m.id,
            nonce: m.nonce,
            work,
            own: m.own,
            state: Mutex::new(state),
            nudge: Notify::new(),
            renewing: tokio::sync::Mutex::new(()),
        })
    }

    fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn udd(&self) -> PathBuf {
        self.own.clone().unwrap_or_else(|| self.work.join("udd"))
    }

    fn archive(&self) -> PathBuf {
        self.work.join(ARCHIVE)
    }

    /// Written whole or not at all: its reaper reads it at any moment. Once the
    /// copy is taken to be wiped or kept, nothing more is written there.
    fn mark(&self) -> Result<()> {
        let st = self.state();
        if st.dir.is_none() {
            return Ok(());
        }
        let m = Marker {
            home: self.home.clone(),
            id: self.id.clone(),
            name: self.name.clone(),
            base: st.base,
            nonce: self.nonce.clone(),
            own: self.own.clone(),
            browser: st.browser,
        };
        let tmp = self.work.join("open.json.tmp");
        fs::write(&tmp, serde_json::to_vec(&m)?)?;
        fs::rename(&tmp, self.work.join(MARKER))?;
        Ok(())
    }

    /// The copy, its lock and its tie, taken to be wiped or kept.
    fn take_copy(&self) -> Copy {
        let mut st = self.state();
        Copy { _tie: st.tie.take(), dir: st.dir.take(), _reaping: st.reaping.take() }
    }

    fn refusal(&self, e: TeamError) -> anyhow::Error {
        CmdError::from_team(e, &doing(&self.id)).into()
    }

    /// The lease, kept alive from here on.
    async fn take_lease(self: &Arc<Self>) -> Result<()> {
        let c = self.home.client()?;
        let lease = self.home.path(&self.id, "lease").map_err(|e| self.refusal(e))?;
        c.call(Method::POST, &lease, None).await.map_err(|e| self.refusal(e))?;
        self.state().leased = true;
        let watcher = self.clone();
        super::live::spawn(watcher.watch());
        Ok(())
    }

    /// The session revision the server holds now.
    async fn server_rev(&self) -> Result<u64> {
        #[derive(Deserialize)]
        struct Row {
            #[serde(default)]
            session_rev: u64,
        }
        let c = self.home.client()?;
        let path = format!("{}/profiles/{}", if self.home.mine { "/me" } else { "" }, super::admin::seg(&self.id)?);
        let row: Row = c.get(&path).await.map_err(|e| self.refusal(e))?;
        Ok(row.session_rev)
    }

    /// The lease, then the session into the folder; renewals start with the lease.
    async fn begin(self: &Arc<Self>) -> Result<SessionFront> {
        self.take_lease().await?;
        let c = self.home.client()?;
        let path = self.home.path(&self.id, "session").map_err(|e| self.refusal(e))?;
        let front = c.download(&path, &self.archive()).await.map_err(|e| self.refusal(e))?;
        self.state().base = Some(front.session_rev);
        Ok(front)
    }

    async fn unpack(&self, size: u64) -> Result<()> {
        let (zip, udd) = (self.archive(), self.udd());
        tokio::task::spawn_blocking(move || -> Result<()> {
            fs::create_dir_all(&udd)?;
            // Nothing uploaded yet: a fresh profile.
            if size > 0 {
                pack::unpack(&zip, &udd)?;
            }
            let _ = fs::remove_file(&zip);
            Ok(())
        })
        .await??;
        // Only a whole copy is ever sent back after a crash: marked once it is whole.
        self.mark()
    }

    /// The last look before its browser starts: a close asked meanwhile stops it.
    pub fn still_wanted(&self) -> Result<()> {
        if self.state().end.is_some() {
            bail!("the profile was closed before its browser started");
        }
        Ok(())
    }

    /// Its browser, `pid`, is tracked now; a close asked before that is carried
    /// out. `tie` is kept until the browser is gone.
    pub fn spawned(self: &Arc<Self>, tie: crate::process::Tie, pid: u32) {
        let asked = {
            let mut st = self.state();
            st.tracked = true;
            if let Some(r) = st.reaper.as_ref() {
                r.hold(&tie);
            }
            st.tie = Some(tie);
            st.browser = crate::process::Spawned::of(pid);
            st.end.is_some()
        };
        // Named for its reaper before its lock is: a launcher that dies now leaves it no orphan.
        if let Err(e) = self.mark() {
            eprintln!("[launcher] {}: its browser not marked: {e:#}", self.id);
        }
        if asked {
            self.kill();
        }
    }

    /// Ask it to close; true the first time it is asked.
    fn ask(self: &Arc<Self>, end: End) -> bool {
        let (first, tracked) = {
            let mut st = self.state();
            let first = st.end.is_none();
            st.end = Some(st.end.map_or(end, |was| was.max(end)));
            (first, st.tracked)
        };
        if tracked {
            self.kill();
        }
        first
    }

    fn kill(&self) {
        let id = self.id.clone();
        super::live::spawn(async move {
            let _ = crate::process::Tracker::shared().kill(&id).await;
        });
    }

    async fn renew(&self) -> Result<(), TeamError> {
        let c = self.home.client().map_err(|e| TeamError::Garbled(e.to_string()))?;
        c.call(Method::POST, &self.home.path(&self.id, "lease")?, None).await.map(|_| ())
    }

    /// The team's permission: a personal space has none to run out.
    fn time_left(&self) -> Option<Duration> {
        if self.home.mine {
            None
        } else {
            perm::time_left(&self.home.team)
        }
    }

    /// Renew the lease, and close the profile once the team takes it back,
    /// another device holds it, or the permission runs out.
    async fn watch(self: Arc<Self>) {
        loop {
            let wait = self.next_look();
            tokio::select! {
                _ = tokio::time::sleep(wait) => {}
                _ = self.nudge.notified() => {}
            }
            let turn = self.renewing.lock().await;
            if self.state().stopped {
                return;
            }
            let renewed = self.renew().await;
            // Let go meanwhile: what the server says now is not this one's.
            if self.state().stopped {
                return;
            }
            match renewed {
                Ok(()) => {}
                Err(e) if e.code() == Some(&Code::Leased) => self.lost_lease(&e),
                // Gone from the person's own space: it runs on here, unsynced.
                Err(e) if self.own.is_some() && matches!(e.code(), Some(Code::Forbidden | Code::NotFound)) => {
                    eprintln!("[sync] {}: {e}; it is not synced any more", self.id);
                    self.unsync();
                    self.state().leased = false;
                    return;
                }
                Err(e) if matches!(e.code(), Some(Code::Forbidden | Code::NotFound)) => {
                    if self.ask(End::Upload) {
                        eprintln!("[launcher] {}: the team took it back ({e}); closing", self.id);
                    }
                }
                Err(e) => eprintln!("[launcher] {}: lease not renewed: {e}", self.id),
            }
            drop(turn);
            self.look_at_permission();
        }
    }

    fn next_look(&self) -> Duration {
        let (closing, warned) = {
            let st = self.state();
            (st.end.is_some(), st.warned)
        };
        match self.time_left() {
            _ if closing => RENEW_EVERY,
            Some(left) if !warned && left > WARN_BEFORE => RENEW_EVERY.min(left - WARN_BEFORE),
            Some(left) => RENEW_EVERY.min(left),
            None => RENEW_EVERY,
        }
    }

    fn look_at_permission(self: &Arc<Self>) {
        let Some(left) = self.time_left() else { return };
        if left.is_zero() {
            if self.ask(End::Upload) {
                eprintln!("[launcher] {}: the team's permission ran out; closing", self.id);
            }
        } else if left <= WARN_BEFORE && !std::mem::replace(&mut self.state().warned, true) {
            let text = match perm::clock_ahead(&self.home.team) {
                Some(lead) => format!(
                    "This computer's clock is {} min ahead of the team server's, so the team's permission \
                     runs short: set the clock right, or {} may close when the permission runs out.",
                    (lead.as_secs() + 30) / 60,
                    self.name
                ),
                None => format!(
                    "The team server has not been reachable for a while: {} closes in {} min unless it answers.",
                    self.name,
                    left.as_secs().div_ceil(60)
                ),
            };
            crate::notify_warning(text);
        }
    }

    fn lost_lease(self: &Arc<Self>, e: &TeamError) {
        {
            let mut st = self.state();
            st.leased = false;
            st.lost = true;
        }
        // A synced profile's own data stays, and comes back as a conflict copy.
        if self.own.is_some() {
            if self.ask(End::Discard) {
                eprintln!("[launcher] {}: {e}; closing it here", self.id);
            }
            return;
        }
        if self.ask(End::Discard) {
            crate::notify_warning(format!(
                "{}: {e} while this computer could not reach the team, so it was closed here \
                 and the changes made here were not kept.",
                self.name
            ));
        }
    }

    /// A synced profile that stops syncing while open: no renewal, nothing sent
    /// at its close, and its browser left running.
    fn unsync(&self) {
        let mut st = self.state();
        st.end = Some(st.end.map_or(End::Discard, |was| was.max(End::Discard)));
        st.stopped = true;
    }

    /// No renewal from here on; one under way lands first.
    async fn stop_watch(&self) {
        self.state().stopped = true;
        self.nudge.notify_one();
        let _turn = self.renewing.lock().await;
    }

    /// The launch stopped short of its browser: give it all back.
    fn abandon(self: Arc<Self>) {
        self.ask(End::Discard);
        super::live::spawn(async move {
            self.finish().await;
        });
    }

    /// Its browser is gone: send the session back as asked, give the lease
    /// back, and wipe the copy.
    async fn finish(self: Arc<Self>) -> Outcome {
        self.finish_by(None).await
    }

    /// As `finish`, with the upload and the lease's return cut off at `by`.
    async fn finish_by(self: Arc<Self>, by: Option<tokio::time::Instant>) -> Outcome {
        let end = {
            let mut st = self.state();
            // Gone by now: nothing marked from here names it.
            st.browser = None;
            st.end.unwrap_or(End::Upload)
        };
        let outcome = match end {
            End::Upload => self.send_back(by).await,
            End::Keep => Outcome::Kept,
            End::Discard => Outcome::Discarded,
        };
        // Kept: the lease stays, so nobody opens the older session meanwhile.
        let keep = end == End::Keep || matches!(outcome, Outcome::Unsent(_));
        self.stop_watch().await;
        match (keep, by) {
            (true, _) => {}
            (_, None) => self.give_back().await,
            (_, Some(_)) => {
                let _ = tokio::time::timeout(GIVE_BACK_WITHIN, self.give_back()).await;
            }
        }
        let (ran, lost) = {
            let st = self.state();
            (st.tracked, st.lost)
        };
        let copy = self.take_copy();
        if keep {
            copy.keep();
        } else {
            let _ = tokio::task::spawn_blocking(move || copy.wipe()).await;
        }
        unregister(&self);
        if !self.home.mine {
            super::live::closed(&self.home.team, &self.id);
        }
        // Taken over while its browser ran: what was done here was not sent.
        if self.own.is_some() {
            let outcome = if lost && ran { Outcome::Stale } else { outcome.clone() };
            super::sync::session_closed(&self.id, &outcome, ran);
        }
        self.tell(&outcome);
        outcome
    }

    /// Pack the copy and send it, made from the revision it was opened at.
    /// The packing, a local job, runs to its end; the sending is tried again
    /// while the server does not take it, and stops at `by`.
    async fn send_back(self: &Arc<Self>, by: Option<tokio::time::Instant>) -> Outcome {
        let Some(base) = self.state().base else {
            return Outcome::NotSaved("it had not finished opening".into());
        };
        let (udd, zip) = (self.udd(), self.archive());
        let packed = tokio::task::spawn_blocking(move || pack::pack(&udd, &zip, BUDGET)).await;
        let packed = match packed {
            Ok(Ok(p)) => p,
            Ok(Err(e)) => return Outcome::NotSaved(format!("{e:#}")),
            Err(e) => return Outcome::NotSaved(e.to_string()),
        };
        eprintln!("[launcher] {}: session packed, {} KB", self.id, packed.size / 1024);
        if !packed.left_out.is_empty() {
            crate::notify_warning(format!(
                "{}: its session was over 256 MB, so the storage of {} site(s) was left out.",
                self.name,
                packed.left_out.len()
            ));
        }
        // Asked meanwhile not to send it: it is not sent.
        if self.state().end == Some(End::Discard) {
            return Outcome::Discarded;
        }
        const LATE: &str = "the team server did not take it in time";
        if by.is_some_and(|by| tokio::time::Instant::now() >= by) {
            return self.unsent(LATE.into());
        }
        let mut wait = RESEND_FIRST;
        let mut warned = false;
        let outcome = loop {
            // Asked not to send it, or another device has it now.
            if self.state().end == Some(End::Discard) {
                break Outcome::Discarded;
            }
            let sent = self.send_once(base);
            let got = match by {
                Some(by) => tokio::time::timeout_at(by, sent).await.unwrap_or_else(|_| Err(LATE.into())),
                None => sent.await,
            };
            let why = match got {
                Ok(o) => break o,
                Err(why) => why,
            };
            // A synced profile's own data stays here, and goes up at the next sync.
            if self.own.is_some() {
                break Outcome::NotSaved(why);
            }
            if by.is_some_and(|by| tokio::time::Instant::now() + wait >= by) {
                break Outcome::Unsent(why);
            }
            if by.is_none() && !std::mem::replace(&mut warned, true) {
                crate::notify_warning(format!(
                    "{} closed, but the team server did not take its session: it is kept here and sent again until the server takes it ({why}).",
                    self.name
                ));
            }
            eprintln!("[launcher] {}: session not sent ({why}); again in {} s", self.id, wait.as_secs());
            tokio::time::sleep(wait).await;
            wait = (wait * 2).min(RESEND_LAST);
            // Run out on the server meanwhile: taken again, unless another device has it now.
            if let Err(e) = self.renew().await {
                if e.code() == Some(&Code::Leased) {
                    self.lost_lease(&e);
                }
            }
        };
        if let Outcome::Saved(rev) = outcome {
            self.state().base = Some(rev);
            // A launcher that dies before the copy is wiped sends it from there, not stale.
            if let Err(e) = self.mark() {
                eprintln!("[launcher] {}: not marked as saved: {e:#}", self.id);
            }
        }
        let _ = fs::remove_file(self.archive());
        outcome
    }

    /// One upload of the packed session; `Err` when it is worth sending again.
    async fn send_once(&self, base: u64) -> Result<Outcome, String> {
        let c = match self.home.client() {
            Ok(c) => c,
            Err(e) => return Ok(Outcome::NotSaved(e.to_string())),
        };
        let path = match self.home.path(&self.id, "session") {
            Ok(p) => format!("{p}?base_session_rev={base}"),
            Err(e) => return Ok(Outcome::NotSaved(e.to_string())),
        };
        match c.upload(&path, &self.archive()).await {
            Ok(info) => Ok(Outcome::Saved(info.session_rev)),
            Err(e) if e.code() == Some(&Code::Stale) => Ok(Outcome::Stale),
            // Another of the person's devices took it: theirs stays on the server.
            Err(e) if self.own.is_some() && e.code() == Some(&Code::Leased) => Ok(Outcome::Stale),
            Err(e) if resend(&e) => Err(e.to_string()),
            Err(e) => Ok(Outcome::NotSaved(e.to_string())),
        }
    }

    /// Not sent, and not to be sent from here any more.
    fn unsent(&self, why: String) -> Outcome {
        if self.own.is_some() {
            Outcome::NotSaved(why)
        } else {
            Outcome::Unsent(why)
        }
    }

    async fn give_back(&self) {
        if !std::mem::take(&mut self.state().leased) {
            return;
        }
        let (Ok(c), Ok(path)) = (self.home.client(), self.home.path(&self.id, "lease")) else {
            return;
        };
        if let Err(e) = c.call(Method::DELETE, &path, None).await {
            eprintln!("[launcher] {}: lease not given back: {e}", self.id);
        }
    }

    fn tell(&self, outcome: &Outcome) {
        #[cfg(test)]
        tests::record(&self.id, outcome.clone());
        // Sync says what became of a personal profile's session.
        if self.own.is_some() {
            return;
        }
        let (word, warning) = match outcome {
            Outcome::Saved(_) => ("saved", None),
            Outcome::Stale => (
                "stale",
                Some(format!(
                    "{} was saved from another device while it was open here, so the changes made here were not kept.",
                    self.name
                )),
            ),
            Outcome::NotSaved(why) => {
                ("not_saved", Some(format!("{} closed, and its session was not saved to the team: {why}", self.name)))
            }
            Outcome::Unsent(why) => {
                let why = format!("{why}; it is kept on this computer and sent when the launcher starts next");
                ("not_saved", Some(format!("{} closed, and its session was not saved to the team: {why}", self.name)))
            }
            Outcome::Discarded => ("discarded", None),
            Outcome::Kept => ("kept", None),
        };
        if let Some(w) = warning {
            crate::notify_warning(w);
        }
        let tell = super::live::default_tell();
        let payload = json!({ "team_id": self.home.team, "profile_id": self.id, "name": self.name, "outcome": word });
        tell("team-session", payload);
        tell("store-changed", json!("profiles"));
    }
}

/// No answer, the lease run out, or a gateway between: the same upload may go through later.
fn resend(e: &TeamError) -> bool {
    match e {
        TeamError::Offline(_) => true,
        TeamError::Refused(r) => r.code == Code::NoLease || matches!(r.status, 502..=504),
        TeamError::Garbled(_) => false,
    }
}

// ---- the copy on disk ----

/// Where open copies live: under the data root, beside `user-data`, never in it.
fn root() -> Result<PathBuf> {
    Ok(crate::store::data_root()?.join("team-open"))
}

/// An id as a folder name: as it is when plain, else a digest of it.
pub(super) fn safe(s: &str) -> String {
    let plain = !s.is_empty() && s.len() <= 100 && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_');
    if plain {
        return s.to_string();
    }
    use sha2::{Digest, Sha256};
    Sha256::digest(s.as_bytes()).iter().take(16).map(|b| format!("{b:02x}")).collect()
}

/// A folder wiped when it goes, and its parents once they are empty.
pub struct Materialized {
    path: Option<PathBuf>,
}

impl Materialized {
    fn fresh(path: PathBuf) -> Result<Materialized> {
        remove_all(&path)?;
        // Another copy's wipe may prune a parent meanwhile: made again then.
        let mut made = fs::create_dir_all(&path);
        for _ in 0..3 {
            if made.is_ok() {
                break;
            }
            made = fs::create_dir_all(&path);
        }
        made?;
        Ok(Materialized { path: Some(path) })
    }

    /// Left on disk, for a sweep to send back later.
    fn keep(mut self) {
        self.path = None;
    }
}

impl Drop for Materialized {
    fn drop(&mut self) {
        if let Some(p) = self.path.take() {
            wipe(&p);
        }
    }
}

fn remove_all(dir: &Path) -> std::io::Result<()> {
    match fs::remove_dir_all(dir) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        r => r,
    }
}

/// A copy as it is let go. Dropped in field order: the browser's tie (what is
/// left of it ends), then the folder, then the reaping lock, held to the end.
struct Copy {
    _tie: Option<crate::process::Tie>,
    dir: Option<Materialized>,
    _reaping: Option<fs::File>,
}

impl Copy {
    /// Unmarked before its lock goes, so no sweep or reaper takes it up again.
    fn wipe(self) {
        if let Some(p) = self.dir.as_ref().and_then(|d| d.path.as_ref()) {
            let _ = fs::remove_file(p.join(MARKER));
        }
    }

    fn keep(mut self) {
        if let Some(d) = self.dir.take() {
            d.keep();
        }
    }
}

/// Remove `dir`; what cannot go now (a Windows lock) is tried again every minute.
/// Unmarked first: a copy half gone is never sent back.
fn wipe(dir: &Path) {
    crate::store::remove_browser_cache(dir);
    let _ = fs::remove_file(dir.join(MARKER));
    if let Some(left) = clear(dir) {
        retry_later(left);
    }
}

/// Moved aside under a name no copy takes, then removed, so a copy made again
/// at `dir` meanwhile is never touched. Answers what is left of it.
fn clear(dir: &Path) -> Option<PathBuf> {
    let aside = dir.with_file_name(format!(".gone-{}", uuid::Uuid::new_v4().simple()));
    let aside_now = dir.file_name().is_some_and(|n| n.to_string_lossy().starts_with(".gone-"));
    let inside = !aside_now && root().is_ok_and(|r| dir.starts_with(&r) && dir != r);
    let at = if inside && fs::rename(dir, &aside).is_ok() { aside } else { dir.to_path_buf() };
    match remove_all(&at) {
        Ok(()) => {
            prune(&at);
            None
        }
        Err(e) => {
            eprintln!("[launcher] {} not wiped yet: {e}", at.display());
            Some(at)
        }
    }
}

/// The empty folders above `dir`, up to and including `team-open`.
fn prune(dir: &Path) {
    let Ok(root) = root() else { return };
    let mut at = dir.parent();
    while let Some(d) = at.filter(|d| d.starts_with(&root)) {
        if fs::remove_dir(d).is_err() {
            break;
        }
        at = d.parent();
    }
}

/// What a wipe could not remove yet.
fn left() -> MutexGuard<'static, Vec<PathBuf>> {
    static LEFT: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());
    LEFT.lock().unwrap_or_else(|e| e.into_inner())
}

fn retry_later(dir: PathBuf) {
    static RUNNING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    left().push(dir);
    // A reaper tries again in its own loop; a test, by hand.
    if cfg!(test) || super::reap::in_reaper() || RUNNING.swap(true, std::sync::atomic::Ordering::SeqCst) {
        return;
    }
    super::live::spawn(async {
        loop {
            tokio::time::sleep(Duration::from_secs(60)).await;
            let _ = tokio::task::spawn_blocking(retry_left).await;
        }
    });
}

/// What a wipe left, tried again; true once nothing is left. A folder that
/// holds a copy again is that copy's: looked at again later.
pub(super) fn retry_left() -> bool {
    let dirs = std::mem::take(&mut *left());
    for d in dirs {
        // Held across the look and the removal: no copy is made here meanwhile.
        let open = open_now();
        let rest = if taken(&d, &open) { Some(d) } else { clear(&d) };
        drop(open);
        if let Some(d) = rest {
            left().push(d);
        }
    }
    left().is_empty()
}

/// A copy lives at or under `dir`: marked, or open in this process.
fn taken(dir: &Path, open: &[Arc<Session>]) -> bool {
    dir.join(MARKER).exists() || open.iter().any(|s| s.work.starts_with(dir))
}

/// Delete all data: whatever is left in `team-open` goes, sent nowhere.
pub fn wipe_left() {
    if let Ok(root) = root() {
        wipe(&root);
    }
}

// ---- after a crash ----

/// What a launcher that died left behind, at start: each copy's browser is
/// closed, its session sent back if the lease can be had again and nobody saved
/// since, and the copy wiped. Run before anything can start a profile.
pub fn sweep() {
    sweep_where(|_| true, true);
}

/// The copies kept for a team's new invite, once it is pasted.
pub fn sweep_team(team: &str) {
    sweep_where(|h| h.team == team, false);
}

fn sweep_where(pick: impl Fn(&Home) -> bool, everything: bool) {
    let Ok(root) = root() else { return };
    let Ok(homes) = fs::read_dir(&root) else { return };
    let mut orphans = Vec::new();
    let mut reaped = Vec::new();
    for h in homes.flatten() {
        let Ok(copies) = fs::read_dir(h.path()) else {
            if everything {
                let _ = fs::remove_file(h.path());
            }
            continue;
        };
        for c in copies.flatten() {
            let dir = c.path();
            match read_marker(&dir) {
                Some(m) if pick(&m.home) => {
                    // A reaper on it closes it; nothing here does it twice.
                    if find(&m.home, &m.id).is_none() {
                        match take_reaping(&dir) {
                            Some(lock) => orphans.push(Session::adopt(m, dir, lock)),
                            None => reaped.push(dir),
                        }
                    }
                }
                Some(_) => {}
                None if everything => {
                    if c.file_type().is_ok_and(|t| t.is_dir()) {
                        wipe(&dir);
                    } else {
                        let _ = fs::remove_file(&dir);
                    }
                }
                None => {}
            }
        }
        let _ = fs::remove_dir(h.path());
    }
    let _ = fs::remove_dir(&root);
    for s in orphans {
        open_now().push(s.clone());
        super::live::spawn(async move {
            s.close_orphan(None).await;
        });
    }
    for dir in reaped {
        super::live::spawn(after_its_reaper(dir));
    }
}

/// A copy its reaper is closing: what the reaper leaves unsent is sent from here.
async fn after_its_reaper(dir: PathBuf) {
    let until = tokio::time::Instant::now() + REAP_UPLOAD + Duration::from_secs(60);
    while tokio::time::Instant::now() < until {
        tokio::time::sleep(Duration::from_secs(2)).await;
        if read_marker(&dir).is_none() {
            return;
        }
        let Some(lock) = take_reaping(&dir) else { continue };
        let taken = {
            let mut open = open_now();
            read_marker(&dir).filter(|m| find_in(&open, &m.home, &m.id).is_none()).map(|m| {
                let s = Session::adopt(m, dir, lock);
                open.push(s.clone());
                s
            })
        };
        if let Some(s) = taken {
            s.close_orphan(None).await;
        }
        return;
    }
}

fn find_in(open: &[Arc<Session>], home: &Home, id: &str) -> Option<Arc<Session>> {
    open.iter().find(|s| s.home == *home && s.id == id).cloned()
}

impl Session {
    /// A copy whose launcher died: its browser closed, its session sent back if
    /// the lease can be had again and nobody saved since, then wiped. With a
    /// `limit`, the sending stops that long after the browser closed.
    async fn close_orphan(self: Arc<Self>, limit: Option<Duration>) -> Outcome {
        let spawned = self.state().browser;
        crate::process::end_browser(&self.udd(), spawned).await;
        // Kept for the team's new invite: neither sent nor wiped until it is pasted.
        if self.waits_for_invite() {
            self.ask(End::Keep);
            return self.finish_by(None).await;
        }
        let by = limit.map(|l| tokio::time::Instant::now() + l);
        if self.state().base.is_none() {
            self.ask(End::Discard);
        } else {
            let renewed = match by {
                Some(by) => tokio::time::timeout_at(by, self.renew()).await.ok(),
                None => Some(self.renew().await),
            };
            // Held by another device now: what is here is not sent over it.
            if let Some(Err(e)) = &renewed {
                if e.code() == Some(&Code::Leased) {
                    eprintln!("[launcher] {}: {e}; not sent", self.id);
                    {
                        let mut st = self.state();
                        st.leased = false;
                        st.lost = true;
                    }
                    self.ask(End::Discard);
                }
            }
        }
        self.finish_by(by).await
    }
}

impl Session {
    /// Its team re-issued the token: the copy waits for the new invite.
    fn waits_for_invite(&self) -> bool {
        !self.home.mine && crate::team::link(&self.home.team).ok().flatten().is_some_and(|l| l.needs_invite && !l.revoked)
    }
}

fn read_marker(dir: &Path) -> Option<Marker> {
    fs::read(dir.join(MARKER)).ok().and_then(|b| serde_json::from_slice(&b).ok())
}

/// The lock on a copy whose launcher died; None while someone else closes it.
fn take_reaping(dir: &Path) -> Option<fs::File> {
    let f = fs::OpenOptions::new().create(true).truncate(false).write(true).open(dir.join(REAPING)).ok()?;
    match f.try_lock() {
        Ok(()) => Some(f),
        Err(fs::TryLockError::WouldBlock) => None,
        // A disk that keeps no locks: closed here all the same.
        Err(fs::TryLockError::Error(e)) => {
            eprintln!("[launcher] {}: not locked ({e})", dir.display());
            Some(f)
        }
    }
}

// ---- the reaper's side ----

/// The opening the copy at `dir` belongs to; None when there is none.
pub(super) fn nonce_at(dir: &Path) -> Option<String> {
    read_marker(dir).map(|m| m.nonce)
}

/// Whether that opening's copy is still there: gone, or made again, it is not.
pub(super) fn still_there(dir: &Path, nonce: &str) -> bool {
    match fs::read(dir.join(MARKER)) {
        Err(e) => e.kind() != std::io::ErrorKind::NotFound,
        // Unreadable for a moment: looked at again.
        Ok(b) => serde_json::from_slice::<Marker>(&b).map_or(true, |m| m.nonce == nonce),
    }
}

/// The launcher that opened the copy at `dir` died: it is closed as a sweep
/// would close it, the upload cut off at 20 s. False when it was not this
/// reaper's to close (opened again since, or a sweep is on it).
pub(super) async fn reap(dir: PathBuf, nonce: &str) -> bool {
    let Some(m) = read_marker(&dir).filter(|m| m.nonce == nonce) else { return false };
    let Some(lock) = take_reaping(&dir) else { return false };
    let s = Session::adopt(m, dir, lock);
    let outcome = s.clone().close_orphan(Some(REAP_UPLOAD)).await;
    eprintln!("[reaper] {}: {outcome:?}", s.id);
    true
}

/// Whether a profile of `home` is open, opening or closing here.
#[cfg(test)]
pub(crate) fn open_here(home: &Home, id: &str) -> bool {
    find(home, id).is_some()
}

/// The lease of a profile open here is renewed now, as at its next turn.
#[cfg(test)]
pub(crate) fn renew_now(home: &Home, id: &str) {
    if let Some(s) = find(home, id) {
        s.nudge.notify_one();
    }
}

#[cfg(test)]
mod tests;
