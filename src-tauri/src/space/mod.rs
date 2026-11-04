//! Every read and write of profiles, proxies, bookmarks, extensions, projects,
//! tags, folders, the trash and cookies goes through here, in the space the
//! launcher works in. The personal space is this machine's files in `local`;
//! a team is read from `cache` and written through `write` and `items`, door by door.
//! A write carries a [`Ticket`]: it goes to the space it was asked in, or nowhere.

pub mod admin;
pub mod cache;
pub mod client;
pub mod doors;
pub mod error;
mod items;
pub mod live;
mod local;
pub mod open;
mod pack;
pub mod perm;
pub mod reap;
mod remote;
pub mod secrets;
pub mod sync;
mod write;

use anyhow::{bail, Result};
use std::sync::atomic::{AtomicBool, Ordering};

pub use doors::Door;
pub use error::CmdError;

pub use cache::ready;

/// A write only this machine's own profiles take: a run's profile steps, the launcher's own fixes.
pub const PERSONAL_ONLY: &str = "not offered in a team space yet: this writes only your own profiles";
/// A door the team space does not have.
pub const NOT_IN_TEAM: &str = "not offered in a team space";
/// A door a team's profiles do not have yet: cookies kept apart from a start, temporary ones.
pub const TEAM_NOT_YET: &str = "not offered for a team's profiles yet";
/// Why a switch is refused.
pub const SWITCH_WHILE_STARTING: &str = "a profile is starting: switch spaces once it has started";
pub const SWITCH_WHILE_MOVING: &str = "ShardBrowser is moving or resetting its data: switch spaces when that finishes";
/// Why a launch is refused while the space changes.
pub const LAUNCH_WHILE_SWITCHING: &str = "the launcher is switching spaces: start the profile again in a moment";

/// The team the doors work in now; None in the personal space.
fn team_now() -> Option<String> {
    crate::team::active_team()
}

/// Writes go to this machine's files only in the personal space.
fn personal_only() -> Result<()> {
    if team_now().is_some() {
        return Err(CmdError::NotOffered(PERSONAL_ONLY).into());
    }
    Ok(())
}

/// The space a write was asked in, by its edition. The write goes to that
/// space or nowhere: once the launcher has moved on, it is refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ticket {
    epoch: u64,
    team: Option<String>,
}

impl Ticket {
    /// The space in use now.
    pub fn now() -> Ticket {
        Ticket { epoch: crate::team::space_epoch(), team: team_now() }
    }

    pub fn epoch(&self) -> u64 {
        self.epoch
    }

    /// The team the write goes to; None for this machine's own files.
    pub fn team(&self) -> Option<&str> {
        self.team.as_deref()
    }

    /// Still the space in use: asked again right before anything is written.
    pub fn still(&self) -> Result<(), CmdError> {
        if switching() || crate::team::space_epoch() != self.epoch || team_now() != self.team {
            return Err(CmdError::SpaceChanged);
        }
        // A write to the person's own files is on its way: sync goes once it settles.
        if self.team.is_none() {
            sync::touched();
        }
        Ok(())
    }

    /// The last look before a team write goes out.
    pub async fn commit(&self) -> Result<(), CmdError> {
        #[cfg(test)]
        hold::reached().await;
        self.still()
    }

    /// A write to this machine's own files, while the personal space is in use.
    fn here(&self, team_refusal: &'static str) -> Result<()> {
        if self.team.is_some() {
            return Err(CmdError::NotOffered(team_refusal).into());
        }
        Ok(self.still()?)
    }
}

/// A ticket for a write asked under edition `asked`, when the caller says.
pub fn ticket(asked: Option<u64>) -> Result<Ticket, CmdError> {
    let t = Ticket::now();
    if asked.is_some_and(|e| e != t.epoch) {
        return Err(CmdError::SpaceChanged);
    }
    Ok(t)
}

/// A write from the window without its space's edition, refused in a team.
pub const NO_EPOCH: &str = "a write in a team space has to name the space it was asked in (epoch): nothing was written";

/// The window's ticket: in a team it must say which edition the write was asked under.
pub fn window_ticket(asked: Option<u64>) -> Result<Ticket, CmdError> {
    let t = ticket(asked)?;
    if asked.is_none() && t.team.is_some() {
        return Err(CmdError::Refused { status: 400, code: "space_unknown".into(), message: NO_EPOCH.into() });
    }
    Ok(t)
}

/// A test holds a team write right before it goes out.
#[cfg(test)]
pub(crate) mod hold {
    use std::sync::atomic::{AtomicBool, Ordering};
    use tokio::sync::Notify;

    static ARMED: AtomicBool = AtomicBool::new(false);
    static REACHED: Notify = Notify::const_new();
    static GO: Notify = Notify::const_new();

    /// The next write waits at its last look until [`release`].
    pub fn arm() {
        ARMED.store(true, Ordering::SeqCst);
    }

    pub async fn wait_reached() {
        REACHED.notified().await;
    }

    pub fn release() {
        GO.notify_one();
    }

    pub(super) async fn reached() {
        if ARMED.swap(false, Ordering::SeqCst) {
            REACHED.notify_one();
            GO.notified().await;
        }
    }
}

static SWITCHING: AtomicBool = AtomicBool::new(false);

/// Set while the space changes. A launch counts itself before it looks, and a
/// switch sets this before it counts launches, so one of them always sees the other.
pub fn switching() -> bool {
    SWITCHING.load(Ordering::SeqCst)
}

/// Held across a switch; refused while a launch or a data move is under way.
struct Gate;

impl Gate {
    fn take() -> Result<Gate> {
        if SWITCHING.swap(true, Ordering::SeqCst) {
            bail!("the space is already being switched");
        }
        let gate = Gate;
        if crate::migrate::in_progress() || crate::reset::active() {
            bail!("{SWITCH_WHILE_MOVING}");
        }
        if crate::launch::in_flight() > 0 {
            bail!("{SWITCH_WHILE_STARTING}");
        }
        Ok(gate)
    }
}

impl Drop for Gate {
    fn drop(&mut self) {
        SWITCHING.store(false, Ordering::SeqCst);
    }
}

/// Move the launcher to a space and read it; every page is told to read again.
pub async fn switch(space: crate::team::Space) -> Result<crate::team::SpaceState> {
    {
        let _gate = Gate::take()?;
        crate::team::set_space(space)?;
    }
    settle().await;
    crate::team::state()
}

/// Join a team and move into it, as a switch; its live channel starts over.
pub async fn join(server: &str, token: &str) -> Result<crate::team::SpaceState> {
    let st = {
        let _gate = Gate::take()?;
        crate::team::join(server, token).await?
    };
    if let crate::team::Space::Team { team_id } = &st.space {
        live::rejoined(team_id);
        // Copies kept when the old token was replaced go back with the new one.
        open::sweep_team(team_id);
        sync::heard_from(team_id);
    }
    settle().await;
    crate::team::state()
}

/// After the space moved: only the team in use is held, and it is read now.
pub async fn settle() {
    let team = team_now();
    cache::keep_only(team.as_deref());
    let tell = live::default_tell();
    if let Some(team) = &team {
        cache::reload(team, tell.clone()).await;
    }
    for topic in cache::all_topics() {
        tell("store-changed", serde_json::json!(topic));
    }
}

/// Delete all data: every team forgotten here, its channel and permission, what
/// is left in `team-open`, and the secret values kept for its projects. None of
/// it goes into the backup.
pub fn forget_teams() -> Result<()> {
    let teams = crate::team::forget_all()?;
    live::follow_joined();
    for t in &teams {
        perm::forget(t);
        cache::forget(t);
    }
    open::wipe_left();
    sync::forget_local()?;
    secrets::forget_all()
}

/// The running browsers of the space in use; a team profile's CDP address only
/// with Cookies on it now, since CDP reads every cookie.
pub fn running() -> Vec<crate::process::RunningProfile> {
    let mut all = crate::process::Tracker::shared().running();
    all.retain(|r| profile::in_active_space(&r.profile_id));
    for r in &mut all {
        if r.cdp.is_some() && !may_debug(&r.profile_id) {
            r.cdp = None;
        }
    }
    all
}

/// The CDP pass of a running profile, judged again at each use: in the space
/// in use, and with Cookies on it in a team.
#[cfg_attr(not(feature = "automation"), allow(dead_code))]
pub fn cdp_pass(id: &str) -> Result<crate::process::CdpInfo, String> {
    if !profile::in_active_space(id) {
        return Err(format!("no profile {id} in the space in use"));
    }
    if !may_debug(id) {
        return Err(CmdError::Forbidden(error::Need { right: "cookies", on: "profile" }).to_string());
    }
    crate::process::Tracker::shared()
        .cdp(id)
        .ok_or_else(|| "that profile is not running with automation on".to_string())
}

fn may_debug(id: &str) -> bool {
    let Some(team) = team_now() else { return true };
    cache::row(&team, id).is_some_and(|r| {
        perm::allows(&team, team_model::Action::Cookies, &team_model::Target::profile(id, &r.folder))
    })
}

pub mod profile {
    use super::local::profile as local;
    use super::{cache, personal_only, team_now, write, CmdError, Door, Ticket, NOT_IN_TEAM};
    use anyhow::Result;
    use std::path::PathBuf;

    pub use super::write::{Made, SaveOpts};
    pub use local::{claimed_screen, claims_mobile, names_a_window, OpenOn, Origin, ProfileMeta, StoredProfile};

    fn missing_here(id: &str) -> anyhow::Error {
        CmdError::Missing(format!("no profile {id} in your own space")).into()
    }

    fn missing_in_team(id: &str) -> anyhow::Error {
        CmdError::Missing(format!("no profile {id} in this team")).into()
    }

    /// A protected profile's browser data, open for one start: sealed again when this goes.
    pub struct Unsealed {
        id: String,
        key: Option<super::local::seal::DataKey>,
    }

    /// How many protected profiles have their data open now: quitting waits for them to seal.
    static UNSEALED: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

    /// Protected profiles whose data a crash left open: sealed again at their next start.
    pub fn left_open() -> Vec<String> {
        super::local::protect::left_open()
    }

    pub fn any_unsealed() -> bool {
        UNSEALED.load(std::sync::atomic::Ordering::SeqCst) > 0
    }

    /// Quitting: close the protected profiles running here, then wait for their data to seal.
    pub async fn close_unsealed(limit: std::time::Duration) {
        let tracker = crate::process::Tracker::shared();
        for r in tracker.running() {
            if is_protected(&r.profile_id) {
                let _ = tracker.kill(&r.profile_id).await;
            }
        }
        let until = std::time::Instant::now() + limit;
        while any_unsealed() && std::time::Instant::now() < until {
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        }
        if any_unsealed() {
            eprintln!("[launcher] a protected profile did not close in time: sealed at its next start");
        }
    }

    impl Unsealed {
        /// An edit made for this start (an engine migration), sealed with the rest.
        pub fn save(&self, full: &StoredProfile) -> Result<()> {
            super::local::protect::save_full(full, self.key.as_ref().expect("open"))
        }

        /// Seal now, and say whether it worked; dropping does the same and only logs.
        pub fn seal(mut self) -> Result<()> {
            let key = self.key.take().expect("sealed once");
            let out = super::local::protect::seal_data(&self.id, &key);
            UNSEALED.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
            out
        }
    }

    impl Drop for Unsealed {
        fn drop(&mut self) {
            if let Some(key) = self.key.take() {
                if let Err(e) = super::local::protect::seal_data(&self.id, &key) {
                    eprintln!("[launcher] {}: browser data left unsealed: {e:#}", self.id);
                }
                UNSEALED.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
            }
        }
    }

    pub fn list_all() -> Result<Vec<ProfileMeta>> {
        match team_now() {
            Some(team) => Ok(cache::profiles(&team)),
            None => {
                let mut rows = local::list_all()?;
                super::sync::decorate(&mut rows);
                Ok(rows)
            }
        }
    }

    /// Whether the profile is one of the space in use: a door acts on no other.
    pub fn in_active_space(id: &str) -> bool {
        match team_now() {
            Some(team) => cache::has_profile(&team, id),
            None => local::exists(id),
        }
    }

    /// A team profile's settings as far as they were read; [`fetch`] reads them.
    pub fn load_raw(id: &str) -> Result<StoredProfile> {
        match team_now() {
            Some(team) => cache::profile(&team, id).ok_or_else(|| missing_in_team(id)),
            None if !local::exists(id) => Err(missing_here(id)),
            None => local::load_raw(id),
        }
    }

    /// The profile's settings, a team's read from its server the first time.
    pub async fn fetch(id: &str) -> Result<StoredProfile> {
        match team_now() {
            Some(team) => cache::profile_detail(&team, id).await,
            None if !local::exists(id) => Err(missing_here(id)),
            None => local::load_raw(id),
        }
    }

    /// The settings to change and save whole: a team's as its server has them now.
    pub async fn fetch_for_edit(t: &Ticket, id: &str) -> Result<StoredProfile> {
        match t.team() {
            Some(team) => Ok(write::read_for_edit(team, id).await?),
            None if !local::exists(id) => Err(missing_here(id)),
            None => local::load_raw(id),
        }
    }

    /// A new profile's door, asked before anything made for it (a proxy) is stored.
    pub fn check_create(door: Door, t: &Ticket, folder: &str, made: &Made<'_>) -> Result<()> {
        match t.team() {
            Some(team) => Ok(write::check_create(door, team, folder, made)?),
            None => Ok(()),
        }
    }

    /// The same for an edit of a profile.
    pub fn check_edit(door: Door, t: &Ticket, id: &str, inline_proxy: bool) -> Result<()> {
        match t.team() {
            Some(team) => Ok(write::check_edit(door, team, id, inline_proxy)?),
            None => Ok(()),
        }
    }

    /// Saves a profile built whole (the editor, the API); new when its id is empty.
    pub async fn save(door: Door, t: &Ticket, stored: StoredProfile, opts: SaveOpts) -> Result<ProfileMeta> {
        match t.team() {
            Some(team) => Ok(write::save_profile(door, t, team, stored, opts).await?),
            None => {
                t.still()?;
                save_here(stored, opts)
            }
        }
    }

    /// This machine's own save. An editor's rev is checked against the file:
    /// a save on top of a change made elsewhere would revert it.
    pub fn save_here(mut stored: StoredProfile, opts: SaveOpts) -> Result<ProfileMeta> {
        personal_only()?;
        // A lock is bound to its profile's id and data: a copy or an import of one opens nothing.
        if stored.meta.id.is_empty() && stored.meta.lock.is_some() {
            anyhow::bail!(NOT_FOR_PROTECTED);
        }
        if !stored.meta.id.is_empty() {
            if let Some(sent) = opts.sent_rev {
                let on_disk = local::current_rev(&stored.meta.id);
                if sent != on_disk {
                    let stale = CmdError::Stale { what: "profile", now: Some(on_disk), had: Some(sent) };
                    return Err(anyhow::Error::new(stale).context(format!(
                        "This profile changed after you opened it (rev {on_disk}, you have {sent}) \
                         — probably through the API or another window. Reopen it and apply your \
                         changes to the current version."
                    )));
                }
            }
        }
        match opts.password.as_deref() {
            // An edit of a protected profile: sealed again under its key, nothing written readable.
            Some(pw) if local::load_raw(&stored.meta.id).is_ok_and(|p| p.meta.lock.is_some()) => {
                let (_, key) = super::local::protect::unlock(&stored.meta.id, pw)?;
                super::local::protect::save_full(&stored, &key)?;
                stored = local::load_raw(&stored.meta.id)?;
            }
            _ => local::save_raw(&mut stored)?,
        }
        // save_raw keeps the folder when it is blank; a move to no folder is set apart.
        if let Some(f) = opts.move_to {
            local::set_folder(&stored.meta.id, &f)?;
            stored.meta.folder = f.trim().to_string();
        }
        super::sync::touched();
        let mut row = [meta_of(&stored)];
        super::sync::decorate(&mut row);
        let [row] = row;
        Ok(row)
    }

    /// The row a personal profile's settings make.
    fn meta_of(stored: &StoredProfile) -> ProfileMeta {
        let text = |k: &str, or: &str| stored.config.get(k).and_then(|v| v.as_str()).unwrap_or(or).to_string();
        ProfileMeta {
            id: stored.meta.id.clone(),
            name: text("name", "(unnamed)"),
            notes: text("notes", ""),
            proxy_id: stored.meta.proxy_id.clone(),
            last_launched_at: stored.meta.last_launched_at.clone(),
            created_at: stored.meta.created_at.clone(),
            pinned: stored.meta.pinned,
            folder: stored.meta.folder.clone(),
            total_runtime_ms: stored.meta.total_runtime_ms,
            color: stored.meta.color.clone(),
            extensions: stored.meta.extensions.clone(),
            mobile: claims_mobile(&stored.config),
            android_media: stored.meta.android_media,
            origin: Origin::Personal,
            rights: None,
            open_on: None,
            tags: None,
            sync: None,
            protected: stored.meta.lock.is_some(),
        }
    }

    /// This machine's own write, for the launcher's own fixes and a run's steps.
    pub use super::local::protect::{NOT_FOR_PROTECTED, PASSWORD_REQUIRED};
    /// Sync sends a profile readable: it is turned off before the profile is locked.
    pub const SYNCED_FIRST: &str = "turn off sync for this profile first";

    pub fn is_protected(id: &str) -> bool {
        team_now().is_none() && local::load_raw(id).is_ok_and(|p| p.meta.lock.is_some())
    }

    /// Lock profile `id` under `password` (#127).
    pub fn protect(id: &str, password: &str) -> Result<ProfileMeta> {
        personal_only()?;
        if super::sync::picked(id) {
            anyhow::bail!(SYNCED_FIRST);
        }
        super::local::protect::protect(id, password)?;
        super::sync::touched();
        Ok(meta_of(&local::load_raw(id)?))
    }

    pub fn unprotect(id: &str, password: &str) -> Result<ProfileMeta> {
        personal_only()?;
        super::local::protect::unprotect(id, password)?;
        super::sync::touched();
        Ok(meta_of(&local::load_raw(id)?))
    }

    pub fn change_password(id: &str, old: &str, new: &str) -> Result<()> {
        personal_only()?;
        super::local::protect::change_password(id, old, new)
    }

    /// The whole of protected profile `id`, for the editor; nothing of it is written.
    pub fn unlock(id: &str, password: &str) -> Result<StoredProfile> {
        personal_only()?;
        Ok(super::local::protect::unlock(id, password)?.0)
    }

    /// Protected profile `id` opened for one start: the whole profile, its browser data
    /// folder, and the guard that seals that folder again.
    pub fn open_protected(id: &str, password: &str) -> Result<(StoredProfile, PathBuf, Unsealed)> {
        personal_only()?;
        let (full, key) = super::local::protect::unlock(id, password)?;
        let udd = super::local::protect::open_data(id, &key)?;
        UNSEALED.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Ok((full, udd, Unsealed { id: id.to_string(), key: Some(key) }))
    }

    pub fn save_raw(stored: &mut StoredProfile) -> Result<()> {
        personal_only()?;
        local::save_raw(stored)
    }

    pub async fn clone_profile(door: Door, t: &Ticket, id: &str) -> Result<ProfileMeta> {
        match t.team() {
            Some(team) => Ok(write::clone_profile(door, t, team, id).await?),
            None => {
                t.still()?;
                if !local::exists(id) {
                    return Err(missing_here(id));
                }
                local::clone_profile(id)
            }
        }
    }

    /// Into the trash: this machine's for a week, or the team's on its server.
    pub async fn remove(door: Door, t: &Ticket, id: &str) -> Result<()> {
        match t.team() {
            Some(team) => Ok(write::remove_profile(door, t, team, id).await?),
            None => {
                t.still()?;
                if !local::exists(id) {
                    return Err(missing_here(id));
                }
                super::local::trash::move_to_trash(id).map(|_| ())
            }
        }
    }

    /// Removes the profile and its browser data at once, past the trash.
    // Only a run's step does this.
    #[cfg_attr(not(feature = "automation"), allow(dead_code))]
    pub fn delete(id: &str) -> Result<()> {
        personal_only()?;
        local::delete(id)
    }

    /// A temporary profile's browser closed: it goes, whichever space is in use.
    pub fn forget_temporary(id: &str) -> Result<()> {
        local::delete(id)
    }

    /// A pin is this member's own mark, and none is kept for a team's profile yet.
    pub fn set_pin(t: &Ticket, id: &str, pinned: bool) -> Result<()> {
        t.here(NOT_IN_TEAM)?;
        local::set_pin(id, pinned)
    }

    pub async fn set_folder(door: Door, t: &Ticket, id: &str, folder: &str) -> Result<()> {
        match t.team() {
            Some(team) => Ok(write::set_folder(door, t, team, id, folder).await?),
            None => {
                t.still()?;
                if !local::exists(id) {
                    return Err(missing_here(id));
                }
                local::set_folder(id, folder)
            }
        }
    }

    pub async fn bind_proxy(door: Door, t: &Ticket, id: &str, proxy_id: Option<String>) -> Result<()> {
        match t.team() {
            Some(team) => Ok(write::bind_proxy(door, t, team, id, proxy_id).await?),
            None => {
                t.still()?;
                let mut p = load_raw(id)?;
                p.meta.proxy_id = proxy_id;
                local::save_raw(&mut p)
            }
        }
    }

    /// The folder's kept entry and its look follow the name. The profiles have
    /// moved by then, so a folders.json that fails is logged, not returned.
    pub async fn rename_folder(door: Door, t: &Ticket, old: &str, new: &str) -> Result<usize> {
        if let Some(team) = t.team() {
            return Ok(write::rename_folder(door, t, team, old, new).await?);
        }
        t.still()?;
        let n = local::rename_folder(old, new)?;
        if let Err(e) = super::local::folders::rename(old, new) {
            eprintln!("[launcher] folder {old} renamed, its kept entry and look not: {e:#}");
        }
        Ok(n)
    }

    /// The folder's kept entry and its look go with it, as with a rename.
    pub async fn delete_folder(door: Door, t: &Ticket, name: &str, delete_profiles: bool) -> Result<usize> {
        if let Some(team) = t.team() {
            return Ok(write::delete_folder(door, t, team, name, delete_profiles).await?);
        }
        t.still()?;
        let n = local::delete_folder(name, delete_profiles)?;
        if let Err(e) = super::local::folders::forget(name) {
            eprintln!("[launcher] folder {name} deleted, its kept entry and look not: {e:#}");
        }
        Ok(n)
    }

    // This machine's own profiles, which only it starts: never a team's.

    pub fn fill_missing_window(id: &str, window: serde_json::Value) -> Result<bool> {
        local::fill_missing_window(id, window)
    }

    pub fn touch_launched(id: &str, proxy_id: Option<String>) -> Result<()> {
        local::touch_launched(id, proxy_id)
    }

    pub fn add_runtime(id: &str, ms: u64) -> Result<()> {
        local::add_runtime(id, ms)
    }

    pub fn user_data_dir(id: &str) -> Result<PathBuf> {
        local::user_data_dir(id)
    }

    pub fn purge_temporary() -> Result<usize> {
        local::purge_temporary()
    }

    /// Held by a reset while it moves the profiles aside.
    pub fn file_lock() -> std::sync::MutexGuard<'static, ()> {
        local::file_lock()
    }
}

pub mod proxy {
    use super::local::proxy as local;
    use super::{cache, team_now, write, CmdError, Door, Ticket};
    use anyhow::Result;

    pub use local::{
        can_send_udp_directly, country_to_locale, country_to_timezone, geo_check, geo_check_via,
        parse_bulk, parse_single, probe, probe_udp, GeoInfo, ProxyEntry, ProxyKind, TestSnapshot,
    };

    /// A team's proxies carry an address only where this member may use them.
    pub fn list() -> Result<Vec<ProxyEntry>> {
        match team_now() {
            Some(team) => Ok(cache::proxies(&team)),
            None => local::list(),
        }
    }

    pub fn get(id: &str) -> Result<Option<ProxyEntry>> {
        match team_now() {
            Some(team) => Ok(cache::proxy(&team, id)),
            None => local::get(id),
        }
    }

    /// Adds the proxy, or changes the one with its id.
    pub async fn upsert(door: Door, t: &Ticket, entry: ProxyEntry) -> Result<ProxyEntry> {
        match t.team() {
            Some(team) => Ok(write::save_proxy(door, t, team, entry).await?),
            None => {
                t.still()?;
                local::upsert(entry)
            }
        }
    }

    /// Adds the proxy unless an identical one is kept: that one is answered.
    pub async fn upsert_dedup(door: Door, t: &Ticket, entry: ProxyEntry) -> Result<ProxyEntry> {
        match t.team() {
            Some(team) => Ok(write::save_proxy_dedup(door, t, team, entry).await?),
            None => {
                t.still()?;
                local::upsert_dedup(entry)
            }
        }
    }

    /// As [`upsert_dedup`], but a new password for the profile's own proxy changes it.
    pub async fn upsert_for_binding(door: Door, t: &Ticket, entry: ProxyEntry, bound_id: Option<&str>) -> Result<ProxyEntry> {
        match t.team() {
            Some(team) => Ok(write::save_proxy_for_binding(door, t, team, entry, bound_id).await?),
            None => {
                t.still()?;
                local::upsert_for_binding(entry, bound_id)
            }
        }
    }

    /// Adds each proxy not kept yet; answers how many went in.
    pub async fn bulk_save(door: Door, t: &Ticket, entries: Vec<ProxyEntry>) -> Result<usize> {
        match t.team() {
            Some(team) => Ok(write::save_proxies(door, t, team, entries).await?),
            None => {
                t.still()?;
                local::bulk_save(entries)
            }
        }
    }

    pub async fn delete(door: Door, t: &Ticket, id: &str) -> Result<()> {
        match t.team() {
            Some(team) => Ok(write::delete_proxy(door, t, team, id).await?),
            None => {
                t.still()?;
                if local::get(id)?.is_none() {
                    return Err(CmdError::Missing(format!("no proxy {id} in your own space")).into());
                }
                local::delete(id)
            }
        }
    }

    /// Tests the proxy and keeps the result in its history; a team's only in memory.
    pub async fn full_test(entry: &ProxyEntry) -> Result<TestSnapshot> {
        match team_now() {
            Some(team) => Ok(cache::keep_test(&team, &entry.id, local::measure(entry).await)),
            None => local::full_test(entry).await,
        }
    }

    pub fn history(id: &str) -> Result<Vec<TestSnapshot>> {
        match team_now() {
            Some(team) => Ok(cache::tests(&team, id)),
            None => local::history(id),
        }
    }

    pub fn latest_test(id: &str) -> Option<TestSnapshot> {
        match team_now() {
            Some(team) => cache::tests(&team, id).pop(),
            None => local::latest_test(id),
        }
    }
}

pub mod tags {
    use super::local::tags as local;
    use super::{cache, items, team_now, write, Door, Ticket, NOT_IN_TEAM};
    use anyhow::Result;
    use std::collections::BTreeMap;

    pub use local::{Tag, TagState};

    pub fn list() -> Result<TagState> {
        match team_now() {
            Some(team) => Ok(cache::tags(&team)),
            None => local::list(),
        }
    }

    /// Adds the tag or changes the one with its id; a new one goes at `at` when
    /// given. A team's are listed by name, so `at` means nothing there.
    pub async fn save(door: Door, t: &Ticket, tag: Tag, at: Option<usize>) -> Result<Tag> {
        match t.team() {
            Some(team) => items::save_tag(door, t, team, tag).await,
            None => {
                t.still()?;
                local::save(tag, at)
            }
        }
    }

    /// Deletes the tag and takes it off every profile.
    pub async fn delete(door: Door, t: &Ticket, id: &str) -> Result<()> {
        match t.team() {
            Some(team) => items::delete_tag(door, t, team, id).await,
            None => {
                t.still()?;
                local::delete(id)
            }
        }
    }

    /// Gives each listed profile exactly these tags; an empty list clears it.
    pub async fn assign(door: Door, t: &Ticket, by_profile: BTreeMap<String, Vec<String>>) -> Result<()> {
        match t.team() {
            Some(team) => Ok(write::assign_tags(door, t, team, by_profile).await?),
            None => {
                t.still()?;
                local::assign(by_profile)
            }
        }
    }

    /// The interface's old copy, taken once: false when tags were saved here already.
    pub fn import(t: &Ticket, state: TagState) -> Result<bool> {
        t.here(NOT_IN_TEAM)?;
        local::import(state)
    }
}

pub mod folders {
    use super::local::folders as local;
    use super::{cache, team_now, write, Door, Ticket, NOT_IN_TEAM};
    use anyhow::Result;

    pub use local::{FolderMeta, FolderState};

    /// A team's folders are the ones this member may know of.
    pub fn list() -> Result<FolderState> {
        match team_now() {
            Some(team) => Ok(cache::folders(&team)),
            None => local::list(),
        }
    }

    /// Keeps the folder while no profile is in it; in a team, the admin makes it there.
    pub async fn add(door: Door, t: &Ticket, name: &str) -> Result<()> {
        match t.team() {
            Some(team) => Ok(write::add_folder(door, t, team, name).await?),
            None => {
                t.still()?;
                local::add(name)
            }
        }
    }

    /// A team folder's look is its colour alone.
    pub async fn set_meta(door: Door, t: &Ticket, name: &str, meta: FolderMeta) -> Result<()> {
        match t.team() {
            Some(team) => Ok(write::set_folder_color(door, t, team, name, meta.color.as_deref().unwrap_or("")).await?),
            None => {
                t.still()?;
                local::set_meta(name, meta)
            }
        }
    }

    /// The interface's old copy, taken once: false when folders were saved here already.
    pub fn import(t: &Ticket, state: FolderState) -> Result<bool> {
        t.here(NOT_IN_TEAM)?;
        local::import(state)
    }
}

pub mod bookmarks {
    use super::local::bookmarks as local;
    use super::{cache, items, team_now, Door, Ticket};
    use anyhow::Result;
    use std::path::Path;

    pub use local::Bookmark;

    pub fn list() -> Result<Vec<Bookmark>> {
        match team_now() {
            Some(team) => Ok(cache::bookmarks(&team)),
            None => local::list(),
        }
    }

    pub async fn save(door: Door, t: &Ticket, b: Bookmark) -> Result<Bookmark> {
        match t.team() {
            Some(team) => items::save_bookmark(door, t, team, b).await,
            None => {
                t.still()?;
                local::save(b)
            }
        }
    }

    /// In a team, one not on its list is not there.
    pub async fn delete(door: Door, t: &Ticket, id: &str) -> Result<()> {
        match t.team() {
            Some(team) => items::delete_bookmark(door, t, team, id).await,
            None => {
                t.still()?;
                local::delete(id)
            }
        }
    }

    /// A team's server takes one at a time: each in turn, past a failure, and
    /// the first failure is the answer.
    pub async fn delete_many(door: Door, t: &Ticket, ids: &[String]) -> Result<()> {
        let Some(team) = t.team() else {
            t.still()?;
            return local::delete_many(ids);
        };
        let mut first = None;
        for id in ids {
            if let Err(e) = items::delete_bookmark(door, t, team, id).await {
                first.get_or_insert(e);
            }
        }
        first.map_or(Ok(()), Err)
    }

    /// This machine's own, for a personal profile's launch.
    pub fn mine() -> Result<Vec<Bookmark>> {
        local::list()
    }

    /// A team's, for its profile's launch: the team's list, or, for a member
    /// it is hidden from, what the profile's start brings.
    pub async fn for_start(team: &str, profile_id: &str) -> Vec<Bookmark> {
        if super::perm::allows(team, team_model::Action::See, &team_model::Target::team()) {
            return cache::bookmarks(team);
        }
        items::bookmarks_for(team, profile_id).await
    }

    /// Writes the bookmarks of `all` the folder takes into a profile's browser data.
    pub fn apply_to_profile(udd: &Path, folder: &str, all: &[Bookmark]) -> Result<usize> {
        local::apply_to_profile(udd, folder, all)
    }
}

pub mod extensions {
    use super::local::extensions as local;
    use super::{cache, items, team_now, CmdError, Door, Ticket};
    use anyhow::Result;
    use local::updates::Env;
    use std::path::{Path, PathBuf};

    pub use local::updates::{Done as UpdateDone, ExtensionUpdate, Linked as LinkDone, News as UpdateNews};
    pub use local::{import_each, ExtensionEntry, Imported};

    /// Updates of a team's extensions come later (design v2).
    const TEAM_UPDATES_NOT_YET: &str = "not offered for a team's extensions yet";

    pub fn list() -> Result<Vec<ExtensionEntry>> {
        match team_now() {
            Some(team) => Ok(cache::extensions(&team)),
            None => local::list(),
        }
    }

    /// In a team the files go up with it and are not kept in this machine's library.
    pub async fn import(door: Door, t: &Ticket, src: &Path) -> Result<ExtensionEntry> {
        import_as(door, t, src, false).await
    }

    /// One the space holds already is refused (`already_in_library`); `separate`
    /// adds it beside, in this machine's library, when its id would differ.
    pub async fn import_as(door: Door, t: &Ticket, src: &Path, separate: bool) -> Result<ExtensionEntry> {
        match t.team() {
            Some(team) => items::import_extension(door, t, team, src).await,
            None => {
                t.still()?;
                local::import_as(src, separate)
            }
        }
    }

    pub async fn import_url(door: Door, t: &Ticket, raw: &str) -> Result<ExtensionEntry> {
        import_url_as(door, t, raw, false).await
    }

    /// A Web Store link is read as strictly as the front reads it (SEC-08).
    pub async fn import_url_as(door: Door, t: &Ticket, raw: &str, separate: bool) -> Result<ExtensionEntry> {
        match t.team() {
            Some(team) => items::import_extension_url(door, t, team, raw).await,
            None => {
                t.still()?;
                let got = local::import_url_as(raw, separate).await?;
                // The download is slow: a switch made meanwhile is told, the files kept here.
                t.still()?;
                Ok(got)
            }
        }
    }

    /// In a team, one not on its list is not there.
    pub async fn delete(door: Door, t: &Ticket, id: &str) -> Result<()> {
        match t.team() {
            Some(team) => items::delete_extension(door, t, team, id).await,
            None => {
                t.still()?;
                // It may wait out a swap of the folder: off the async threads.
                let id = id.to_string();
                tokio::task::spawn_blocking(move || local::delete(&id)).await?
            }
        }
    }

    /// Where this machine keeps an extension's files, for a personal profile's launch.
    pub fn load_path(id: &str) -> Option<PathBuf> {
        local::load_path(id)
    }

    /// At launch, before any profile can start: half-done swaps settled, where
    /// each extension came from, approved updates applied.
    pub fn at_launch() {
        local::at_launch()
    }

    /// Before a personal profile starts: a swap of its extensions is waited
    /// out, and none starts until its browser is tracked (the hold drops).
    pub async fn begin_launch(profile_id: &str) -> Result<local::usage::Hold> {
        let ids = super::local::profile::load_raw(profile_id)?.meta.extensions;
        local::usage::begin_launch(profile_id, &ids).await
    }

    /// A browser closed: updates that waited for it may go in.
    pub fn profile_closed(profile_id: &str) {
        local::usage::profile_closed(profile_id)
    }

    /// A team profile's extensions, unpacked into its open copy at `into`;
    /// `profile` is its id and name.
    pub async fn unpack_for_team(team: &str, profile: (&str, &str), ids: &[String], into: &Path) -> Vec<PathBuf> {
        items::unpack_for_profile(team, profile, ids, into).await
    }

    /// Each library extension's update; none in a team yet.
    pub fn updates(engine: Option<String>) -> Result<Vec<ExtensionUpdate>> {
        if team_now().is_some() {
            return Ok(Vec::new());
        }
        Ok(local::updates::all(&Env::real(engine)?))
    }

    /// The Web Store asked about this machine's linked extensions.
    pub async fn check_updates(engine: Option<String>) -> Result<UpdateNews, String> {
        let env = Env::real(engine).map_err(|e| e.to_string())?;
        local::updates::check(&env).await
    }

    /// When the store was last asked, unix seconds; 0 never.
    pub fn updates_checked_at() -> u64 {
        Env::real(None).map_or(0, |env| local::updates::load(&env.book).last_check_at)
    }

    pub async fn update(door: Door, t: &Ticket, id: &str, confirmed: bool, engine: Option<String>) -> Result<UpdateDone> {
        match t.team() {
            Some(team) => {
                super::doors::clear(door, team, &Default::default())?;
                Err(CmdError::NotOffered(TEAM_UPDATES_NOT_YET).into())
            }
            None => {
                t.still()?;
                local::updates::update(&Env::real(engine)?, id, confirmed).await
            }
        }
    }

    /// Links a library extension to its Web Store item, by its key or its files.
    pub async fn link_source(door: Door, t: &Ticket, id: &str, link: &str, confirmed: bool, engine: Option<String>) -> Result<LinkDone> {
        match t.team() {
            Some(team) => {
                super::doors::clear(door, team, &Default::default())?;
                Err(CmdError::NotOffered(TEAM_UPDATES_NOT_YET).into())
            }
            None => {
                t.still()?;
                local::updates::link(&Env::real(engine)?, id, link, confirmed).await
            }
        }
    }

    pub fn cancel_update(id: &str) -> Result<()> {
        local::updates::cancel(&Env::real(None)?, id)
    }

    pub fn skip_update(id: &str, version: &str) -> Result<()> {
        local::updates::skip(&Env::real(None)?, id, version)
    }

    /// Approved updates nobody holds any more, put in.
    pub fn retry_waiting() {
        match Env::real(None) {
            Ok(env) => local::updates::retry_waiting(&env, "while waiting"),
            Err(e) => eprintln!("[extensions] waiting updates not tried: {e:#}"),
        }
    }
}

pub mod automation {
    use super::local::automation as local;
    use super::{cache, items, team_now, CmdError, Door, Ticket};
    use anyhow::{Context, Result};

    #[cfg(feature = "automation")]
    pub use local::{Block, Branch};
    pub use local::{Bundle, Project};
    // Only the runner's tests build settings by hand.
    #[cfg(all(test, feature = "automation"))]
    pub use local::RunSettings;

    /// A team's come with this machine's own secret values in them.
    pub fn list() -> Result<Vec<Project>> {
        match team_now() {
            Some(team) => Ok(items::projects(&team)),
            None => local::list(),
        }
    }

    pub async fn create(door: Door, t: &Ticket, name: &str) -> Result<Project> {
        match t.team() {
            Some(team) => items::create_project(door, t, team, name).await,
            None => {
                t.still()?;
                local::create(name)
            }
        }
    }

    /// In a team, a secret value is kept on this machine and sent blank.
    pub async fn save(door: Door, t: &Ticket, project: Project) -> Result<Project> {
        match t.team() {
            Some(team) => items::save_project(door, t, team, project).await,
            None => {
                t.still()?;
                local::save(project)
            }
        }
    }

    pub async fn duplicate(door: Door, t: &Ticket, id: &str) -> Result<Project> {
        match t.team() {
            Some(team) => items::duplicate_project(door, t, team, id).await,
            None => {
                t.still()?;
                local::duplicate(id)
            }
        }
    }

    pub async fn delete(door: Door, t: &Ticket, id: &str) -> Result<()> {
        match t.team() {
            Some(team) => items::delete_project(door, t, team, id).await,
            None => {
                t.still()?;
                local::delete(id)
            }
        }
    }

    /// Taking a team's project out needs the team's TakeOut.
    pub fn export(id: &str) -> Result<Bundle> {
        let Some(team) = team_now() else { return local::export(id) };
        let project = cache::projects(&team)
            .into_iter()
            .find(|p| p.id == id)
            .ok_or_else(|| CmdError::Missing(format!("no project {id} in this team")))?;
        if !super::perm::allows(&team, team_model::Action::TakeOut, &team_model::Target::team()) {
            let need = super::error::Need { right: "take_out", on: "team" };
            return Err(CmdError::Forbidden(need)).context("not allowed in this team");
        }
        Ok(local::bundle(project))
    }

    pub async fn import(door: Door, t: &Ticket, bundle: Bundle) -> Result<Project> {
        match t.team() {
            Some(team) => items::import_project(door, t, team, bundle).await,
            None => {
                t.still()?;
                local::import(bundle)
            }
        }
    }
}

pub mod trash {
    use super::local::trash as local;
    use super::profile::ProfileMeta;
    use super::{team_now, Ticket, NOT_IN_TEAM};
    use anyhow::Result;

    pub use local::TrashEntry;

    /// This machine's trash; a team shows none here.
    pub fn list() -> Result<Vec<TrashEntry>> {
        match team_now() {
            Some(_) => Ok(Vec::new()),
            None => local::list(),
        }
    }

    pub fn restore(t: &Ticket, id: &str) -> Result<ProfileMeta> {
        t.here(NOT_IN_TEAM)?;
        local::restore(id)
    }

    pub fn purge(t: &Ticket, id: &str) -> Result<()> {
        t.here(NOT_IN_TEAM)?;
        local::purge(id)
    }

    /// This machine's housekeeping, whichever space is in use.
    pub fn purge_expired() -> Result<usize> {
        local::purge_expired()
    }
}

pub mod cookies {
    use super::local::cookies as local;
    use super::perm::{Cookies, Granted};
    use anyhow::Result;

    pub use local::Cookie;

    pub fn export(pass: &Granted<Cookies>) -> Result<Vec<Cookie>> {
        let udd = super::profile::user_data_dir(pass.profile_id())?;
        local::export(&udd)
    }

    /// The profile must be stopped: a running browser writes over the import.
    pub fn import(pass: &Granted<Cookies>, cookies: &[Cookie]) -> Result<usize> {
        let udd = super::profile::user_data_dir(pass.profile_id())?;
        local::import(&udd, cookies)
    }
}

#[cfg(test)]
mod tests {
    use super::{folders, profile, Door};
    use crate::team::Space;
    use std::fs;

    #[tokio::test]
    async fn a_switch_and_a_launch_never_overlap() {
        let _serial = crate::migrate::serial();
        let _root = crate::store::test_root("switch-gate").with_config();
        let launching = crate::launch::Launching::start();
        let starting = super::switch(Space::Personal).await.unwrap_err().to_string();
        drop(launching);
        assert_eq!(starting, super::SWITCH_WHILE_STARTING);
        assert!(crate::migrate::hold());
        let moving = super::switch(Space::Personal).await.unwrap_err().to_string();
        crate::migrate::release();
        assert_eq!(moving, super::SWITCH_WHILE_MOVING);

        // A launch that comes while the space changes stops before it reads anything.
        let gate = super::Gate::take().unwrap();
        assert!(super::switching() && super::Gate::take().is_err());
        let launch = crate::launch::launch_profile("any", false, false).await;
        drop(gate);
        assert_eq!(launch.err().map(|e| e.to_string()).as_deref(), Some(super::LAUNCH_WHILE_SWITCHING));
        assert!(!super::switching());
        assert_eq!(super::switch(Space::Personal).await.unwrap().space, Space::Personal);
    }

    fn put_profile(id: &str, folder: &str) {
        let body = serde_json::json!({ "_meta": { "id": id, "folder": folder } });
        fs::write(crate::store::profiles_dir().unwrap().join(format!("{id}.json")), body.to_string()).unwrap();
    }
    fn folder_of(id: &str) -> String {
        profile::load_raw(id).unwrap().meta.folder
    }
    fn here() -> super::Ticket {
        super::Ticket::now()
    }
    fn red() -> folders::FolderMeta {
        folders::FolderMeta { color: Some("red".into()), icon: "cart".into() }
    }

    #[tokio::test]
    async fn a_folder_rename_or_delete_takes_its_kept_entry_and_look_along() {
        let _root = crate::store::test_root("folder-facade").with_config();
        put_profile("fa1", "Ads");
        put_profile("fa2", "Ads");
        put_profile("fa3", "Other");
        folders::add(Door("folder_add"), &here(), "Ads").await.unwrap();
        folders::set_meta(Door("folder_meta_set"), &here(), "Ads", red()).await.unwrap();

        assert_eq!(profile::rename_folder(Door("folder_rename"), &here(), "Ads", "Shops").await.unwrap(), 2);
        assert_eq!(folder_of("fa1"), "Shops");
        let s = folders::list().unwrap();
        assert_eq!(s.registry, ["Shops"]);
        assert_eq!(s.meta.get("Shops"), Some(&red()));
        assert!(!s.meta.contains_key("Ads"));

        // A folder only its profiles held stays unkept.
        assert_eq!(profile::rename_folder(Door("folder_rename"), &here(), "Other", "Else").await.unwrap(), 1);
        assert_eq!(folders::list().unwrap().registry, ["Shops"]);

        assert_eq!(profile::delete_folder(Door("folder_delete"), &here(), "Shops", false).await.unwrap(), 2);
        assert_eq!(folder_of("fa2"), "");
        assert_eq!(folders::list().unwrap(), folders::FolderState::default());
    }

    #[tokio::test]
    async fn a_broken_folders_file_never_fails_a_folder_rename_or_delete() {
        let _root = crate::store::test_root("folder-broken").with_config();
        put_profile("fb1", "Ads");
        put_profile("fb2", "Shops");
        let path = crate::store::folders_path().unwrap();
        for body in ["{not json", r#"{"version": 2, "registry": ["Ads"]}"#] {
            fs::write(&path, body).unwrap();
            assert_eq!(profile::rename_folder(Door("folder_rename"), &here(), "Ads", "Ads2").await.unwrap(), 1);
            assert_eq!(folder_of("fb1"), "Ads2");
            assert_eq!(profile::rename_folder(Door("folder_rename"), &here(), "Ads2", "Ads").await.unwrap(), 1);
            assert_eq!(fs::read_to_string(&path).unwrap(), body, "left as it was");
        }
        assert_eq!(profile::delete_folder(Door("folder_delete"), &here(), "Shops", false).await.unwrap(), 1);
        assert_eq!(folder_of("fb2"), "");
    }
}
