//! Everything the server keeps, in one SQLite file.
//!
//! Member tokens are stored only as a hash, and the profiles, proxies, cards,
//! items, sessions and packages are sealed with the server's key (see
//! vault.rs), which lives in a file of its own. The meta table does hold the admin secret and the signing key, so the
//! file is kept readable by the server's own account only.
//!
//! Callers see plain data. Sealing happens here, on the way in and out, so no
//! route can forget to do it.

use anyhow::{bail, Context, Result};
use rusqlite::{params, Connection, OptionalExtension};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard};
use std::time::Instant;

use crate::model::{admin_grants, Action, Card, Grant, Member, Scope, Target, TeamProfile, ADMIN_ID};
use crate::vault::Vault;
use crate::view;

/// The team's display name in the meta table.
pub const META_TEAM_NAME: &str = "team_name";

pub struct Store {
    db: Mutex<Connection>,
    vault: Vault,
    /// Bodies being written or read, and by how many: not the collector's yet.
    /// When both are locked, the database is locked first.
    held: Mutex<std::collections::HashMap<i64, u32>>,
    /// How many times the database has been held: what a test counts.
    hold_count: AtomicU64,
    /// Members whose permission a committed change moved, with their new
    /// epoch, not yet told. The routes drain it with [`Store::take_regranted`].
    regranted: Mutex<Vec<(String, u64)>>,
    /// When each member's `last_seen_at` was last written, and under which
    /// token, so a busy launcher writes it once a minute rather than on every request.
    seen: Mutex<std::collections::HashMap<String, (String, u64)>>,
}

/// How often a member's `last_seen_at` is written at most.
const SEEN_EVERY_SECS: u64 = 60;

/// A hold this long is worth a line in the log: every request waits behind it.
const SLOW_HOLD_US: u64 = 500_000;

/// The database, held; a long hold is logged when it is let go.
struct Locked<'a> {
    conn: MutexGuard<'a, Connection>,
    since: Instant,
    store: &'a Store,
}

impl std::ops::Deref for Locked<'_> {
    type Target = Connection;
    fn deref(&self) -> &Connection {
        &self.conn
    }
}

impl std::ops::DerefMut for Locked<'_> {
    fn deref_mut(&mut self) -> &mut Connection {
        &mut self.conn
    }
}

impl Drop for Locked<'_> {
    fn drop(&mut self) {
        let us = u64::try_from(self.since.elapsed().as_micros()).unwrap_or(u64::MAX);
        self.store.hold_count.fetch_add(1, Ordering::Relaxed);
        if us > SLOW_HOLD_US {
            tracing::warn!("the database was held for {} ms", us / 1000);
        }
    }
}

/// One row of the team's own proxy list. Like a profile, the part that can
/// log in — host, port, user, password — is sealed; the rest is what the
/// launcher needs to show a list and what the server needs to address it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TeamProxy {
    pub id: String,
    pub name: String,
    pub kind: String,
    pub country: String,
    pub revision: u64,
    pub updated_at: u64,
    pub updated_by: String,
}

/// The team's own space; a member's own profiles live in [`personal_space`].
pub const TEAM: &str = "team";

pub fn personal_space(member_id: &str) -> String {
    format!("personal:{member_id}")
}

/// A folder of a space, with how many profiles are in it now.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Folder {
    pub name: String,
    pub color: String,
    pub profiles: u64,
}

#[derive(Debug, PartialEq, Eq)]
pub enum FolderRename {
    /// Done; grants naming it were carried over.
    Done,
    Missing,
    NameTaken,
}

#[derive(Debug, PartialEq, Eq)]
pub enum FolderDelete {
    Done,
    Missing,
    NotEmpty(u64),
    /// Only deleted profiles are left, and they are still judged by its rules.
    InTrash(u64),
}

/// Somebody has a profile open, on a device.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Lease {
    pub member_id: String,
    pub device_id: String,
    pub device_name: String,
    pub acquired_at: u64,
    pub expires_at: u64,
}

/// Who is writing. A write with no device — a script on the API — holds no
/// lease, so it can never overwrite a profile that is open somewhere.
#[derive(Debug, Clone, Copy)]
pub struct Writer<'a> {
    pub member_id: &'a str,
    pub device_id: Option<&'a str>,
}

impl Writer<'_> {
    fn holds(&self, lease: &Lease) -> bool {
        lease.member_id == self.member_id && self.device_id == Some(lease.device_id.as_str())
    }
}

/// What a profile write carries. No card keeps the one stored.
#[derive(Debug, Clone, Copy)]
pub struct ProfileFields<'a> {
    pub name: &'a str,
    pub folder: &'a str,
    pub data: &'a [u8],
    pub card: Option<&'a Card>,
    /// When the writer says it was made, unix seconds; kept only by a row that has no date yet.
    pub created_at: Option<i64>,
}

/// How far ahead of the server's clock a writer's creation date may be.
const MADE_AHEAD_SECS: i64 = 24 * 3600;

/// A writer's creation date, if it could be one.
fn made_at(asked: Option<i64>, now: u64) -> Option<u64> {
    asked.filter(|&t| t > 0 && t <= now as i64 + MADE_AHEAD_SECS).map(|t| t as u64)
}

/// What a proxy write carries.
#[derive(Debug, Clone, Copy)]
pub struct ProxyFields<'a> {
    pub name: &'a str,
    pub kind: &'a str,
    pub country: &'a str,
    pub data: &'a [u8],
}

/// What a write or a move finds beside the row, for the caller's rights check.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Found<'a> {
    /// The proxy the profile's card newly binds where it lands.
    pub bound: Option<&'a str>,
    /// New where it lands, but something there already names the id: a rule,
    /// or for a proxy a team profile's binding. Making it is changing that.
    pub named: bool,
}

/// Why a guarded write did not happen. Each is the caller's to handle, so it
/// is an answer, not an error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    /// Another device has it open.
    Leased(Lease),
    /// It changed since the writer read it; their copy is stale.
    Stale { current: u64 },
    /// The id lives in another space than the one written to.
    OtherSpace,
    Missing,
    /// The caller's rights do not cover it (their `authorize` said so).
    Forbidden,
    /// The writer was switched off before the write ran.
    SwitchedOff,
    /// The writer was removed before the write ran.
    Removed,
    /// A session or package move from a device that holds no live lease on it.
    NoLease,
    /// The session moved past the revision the upload was based on.
    SessionStale { current: u64 },
    /// The package moved past the revision the upload was based on.
    PackageStale { current: u64 },
    /// The card binds a proxy that is not in the profile's space, or not the writer's to use.
    BadProxy,
    /// The id waits in the team's trash: restore or purge it first.
    InTrash(Box<TrashItem>),
}

/// What kind of item a trash row holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TrashKind {
    Profile,
    Proxy,
    Bookmark,
    Extension,
    Automation,
    Tag,
}

impl TrashKind {
    /// The same word the live channel uses for the item.
    pub fn as_str(self) -> &'static str {
        match self {
            TrashKind::Profile => "profile",
            TrashKind::Proxy => "proxy",
            TrashKind::Bookmark => "bookmark",
            TrashKind::Extension => "extension",
            TrashKind::Automation => "automation",
            TrashKind::Tag => "tag",
        }
    }

    /// A generic item's kind, by its name; None for anything else.
    pub fn of_item(kind: &str) -> Option<Self> {
        match kind {
            "bookmark" => Some(TrashKind::Bookmark),
            "extension" => Some(TrashKind::Extension),
            "automation" => Some(TrashKind::Automation),
            "tag" => Some(TrashKind::Tag),
            _ => None,
        }
    }

    /// Kept in the items table rather than the trash table.
    pub fn is_item(self) -> bool {
        !matches!(self, TrashKind::Profile | TrashKind::Proxy)
    }
}

/// A team item deleted but not yet gone. Its sealed data waits beside it; the
/// folder is where it was, and its rights are still judged there. A generic
/// item (bookmark, extension…) is in no folder.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct TrashItem {
    pub id: String,
    pub kind: TrashKind,
    pub name: String,
    /// Always "" for a proxy, which is in no folder.
    pub folder: String,
    /// A proxy's own kind (socks5, http…) and country; "" for a profile.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub proxy_kind: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub country: String,
    pub revision: u64,
    pub updated_at: u64,
    pub updated_by: String,
    pub deleted_at: u64,
    pub deleted_by: String,
}

/// An item put back from the trash, as it is now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Restored {
    Profile(TeamProfile),
    Proxy(TeamProxy),
}

fn trash_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<TrashItem> {
    let kind: String = r.get(1)?;
    Ok(TrashItem {
        id: r.get(0)?,
        kind: if kind == "proxy" { TrashKind::Proxy } else { TrashKind::Profile },
        name: r.get(2)?,
        folder: r.get(3)?,
        proxy_kind: r.get(4)?,
        country: r.get(5)?,
        revision: r.get::<_, i64>(6)? as u64,
        updated_at: r.get::<_, i64>(7)? as u64,
        updated_by: r.get(8)?,
        deleted_at: r.get::<_, i64>(9)? as u64,
        deleted_by: r.get(10)?,
    })
}

const TRASH_COLUMNS: &str = "id, kind, name, folder, proxy_kind, country, revision, updated_at,
                             updated_by, deleted_at, deleted_by";

fn trashed(db: &Connection, id: &str) -> Result<Option<TrashItem>> {
    Ok(db
        .query_row(
            &format!("SELECT {TRASH_COLUMNS} FROM trash WHERE id = ?1"),
            params![id],
            trash_row,
        )
        .optional()?)
}

fn trash_before(db: &Connection, cutoff: u64) -> Result<Vec<TrashItem>> {
    let mut q = db.prepare(&format!(
        "SELECT {TRASH_COLUMNS} FROM trash WHERE deleted_at <= ?1 ORDER BY deleted_at, id"
    ))?;
    let rows = q
        .query_map(params![cutoff as i64], trash_row)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

/// Whether any member's rule names this id: a profile's, or a proxy's.
fn named_by_grant(db: &Connection, id: &str) -> Result<bool> {
    Ok(db
        .query_row(
            "SELECT 1 FROM grants WHERE scope_kind = 'profile' AND scope_key = ?1 LIMIT 1",
            params![id],
            |_| Ok(()),
        )
        .optional()?
        .is_some())
}

/// Every live lease on an item, by item id.
fn live_leases_in(db: &Connection) -> Result<Vec<(String, Lease)>> {
    let mut q = db.prepare(
        "SELECT item_id, member_id, device_id, device_name, acquired_at, expires_at
         FROM leases WHERE expires_at > ?1",
    )?;
    let rows = q
        .query_map(params![lease_now() as i64], |r| {
            Ok((
                r.get::<_, String>(0)?,
                Lease {
                    member_id: r.get(1)?,
                    device_id: r.get(2)?,
                    device_name: r.get(3)?,
                    acquired_at: r.get::<_, i64>(4)? as u64,
                    expires_at: r.get::<_, i64>(5)? as u64,
                },
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

/// The items a member holds live leases on, freed with them.
fn held_by(db: &Connection, member_id: &str) -> Result<Vec<String>> {
    let mut q = db.prepare("SELECT item_id FROM leases WHERE member_id = ?1 AND expires_at > ?2")?;
    let rows = q
        .query_map(params![member_id, lease_now() as i64], |r| r.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

fn live_lease(db: &Connection, id: &str) -> Result<Option<Lease>> {
    Ok(db
        .query_row(
            "SELECT member_id, device_id, device_name, acquired_at, expires_at
             FROM leases WHERE item_id = ?1 AND expires_at > ?2",
            params![id, lease_now() as i64],
            |r| {
                Ok(Lease {
                    member_id: r.get(0)?,
                    device_id: r.get(1)?,
                    device_name: r.get(2)?,
                    acquired_at: r.get::<_, i64>(3)? as u64,
                    expires_at: r.get::<_, i64>(4)? as u64,
                })
            },
        )
        .optional()?)
}

/// What [`profile_row`] reads, in its order.
const PROFILE_COLUMNS: &str = "id, name, folder, revision, updated_at, updated_by, created_at";

fn profile_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<TeamProfile> {
    Ok(TeamProfile {
        id: r.get(0)?,
        name: r.get(1)?,
        folder: r.get(2)?,
        revision: r.get::<_, i64>(3)? as u64,
        updated_at: r.get::<_, i64>(4)? as u64,
        updated_by: r.get(5)?,
        created_at: r.get::<_, Option<i64>>(6)?.map(|t| t as u64),
    })
}

fn profile_by_id(db: &Connection, id: &str) -> Result<TeamProfile> {
    let sql = format!("SELECT {PROFILE_COLUMNS} FROM profiles WHERE id = ?1");
    Ok(db.query_row(&sql, params![id], profile_row)?)
}

fn proxy_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<TeamProxy> {
    Ok(TeamProxy {
        id: r.get(0)?,
        name: r.get(1)?,
        kind: r.get(2)?,
        country: r.get(3)?,
        revision: r.get::<_, i64>(4)? as u64,
        updated_at: r.get::<_, i64>(5)? as u64,
        updated_by: r.get(6)?,
    })
}

/// A profile's fields and the space it lives in.
fn profile_with_space(db: &Connection, id: &str) -> Result<Option<(TeamProfile, String)>> {
    Ok(db
        .query_row(
            &format!("SELECT {PROFILE_COLUMNS}, space FROM profiles WHERE id = ?1"),
            params![id],
            |r| Ok((profile_row(r)?, r.get::<_, String>(7)?)),
        )
        .optional()?)
}

fn proxy_with_space(db: &Connection, id: &str) -> Result<Option<(TeamProxy, String)>> {
    Ok(db
        .query_row(
            "SELECT id, name, kind, country, revision, updated_at, updated_by, space
             FROM proxies WHERE id = ?1",
            params![id],
            |r| Ok((proxy_row(r)?, r.get::<_, String>(7)?)),
        )
        .optional()?)
}

/// Whether a member is still on, and their rules, read under a guarded write's
/// own lock: a revoke that commits first is what the write is judged by.
#[derive(Debug, Clone, Default)]
pub struct Standing {
    /// False once the member has been removed: nothing at all, as for a stranger.
    pub known: bool,
    pub active: bool,
    pub grants: Vec<Grant>,
}

const MEMBER_COLUMNS: &str = "id, name, active, created_at, epoch, last_seen_at";

fn member_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<Member> {
    Ok(Member {
        id: r.get(0)?,
        name: r.get(1)?,
        active: r.get::<_, i64>(2)? != 0,
        created_at: r.get::<_, i64>(3)? as u64,
        epoch: r.get::<_, i64>(4)? as u64,
        last_seen_at: r.get::<_, Option<i64>>(5)?.map(|t| t as u64),
    })
}

fn member_in(db: &Connection, id: &str) -> Result<Option<Member>> {
    let sql = format!("SELECT {MEMBER_COLUMNS} FROM members WHERE id = ?1");
    Ok(db.query_row(&sql, params![id], member_row).optional()?)
}

fn members_in(db: &Connection) -> Result<Vec<Member>> {
    let mut q = db.prepare(&format!("SELECT {MEMBER_COLUMNS} FROM members ORDER BY created_at"))?;
    let rows = q.query_map([], member_row)?.collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

fn grants_in(db: &Connection, member_id: &str) -> Result<Vec<Grant>> {
    let mut q = db.prepare(
        "SELECT scope_kind, scope_key, action, allow FROM grants WHERE member_id = ?1",
    )?;
    let rows = q
        .query_map(params![member_id], |r| {
            let kind: String = r.get(0)?;
            let key: String = r.get(1)?;
            let action: String = r.get(2)?;
            let allow: i64 = r.get(3)?;
            Ok((kind, key, action, allow != 0))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut out = Vec::with_capacity(rows.len());
    for (kind, key, action, allow) in rows {
        let scope = match kind.as_str() {
            "team" => Scope::Team,
            "folder" => Scope::Folder { name: key },
            "profile" => Scope::Profile { id: key },
            other => bail!("unknown scope kind in the database: {other}"),
        };
        let action: Action = serde_json::from_value(serde_json::Value::String(action))
            .context("unknown action in the database")?;
        out.push(Grant { scope, action, allow });
    }
    Ok(out)
}

fn standing_in(db: &Connection, member_id: &str) -> Result<Standing> {
    // The admin principal has no row: always on, with every right.
    if member_id == ADMIN_ID {
        return Ok(Standing { known: true, active: true, grants: admin_grants() });
    }
    let active: Option<i64> = db
        .query_row("SELECT active FROM members WHERE id = ?1", params![member_id], |r| r.get(0))
        .optional()?;
    match active {
        // Removed: nothing at all, as for a stranger.
        None => Ok(Standing::default()),
        Some(a) => Ok(Standing { known: true, active: a != 0, grants: grants_in(db, member_id)? }),
    }
}

/// How a rule's scope is kept in the grants table.
fn scope_row(scope: &Scope) -> (&'static str, &str) {
    match scope {
        Scope::Team => ("team", ""),
        Scope::Folder { name } => ("folder", name),
        Scope::Profile { id } => ("profile", id),
    }
}

fn bump_epoch(db: &Connection, member_id: &str) -> Result<u64> {
    db.execute("UPDATE members SET epoch = epoch + 1 WHERE id = ?1", params![member_id])?;
    let epoch: i64 =
        db.query_row("SELECT epoch FROM members WHERE id = ?1", params![member_id], |r| r.get(0))?;
    Ok(epoch as u64)
}

/// What a member's permission carries, and the edition it is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InView {
    pub active: bool,
    pub epoch: u64,
    pub rules: Vec<Grant>,
}

/// Whether rules `g` let their member see what `scope` names, as the lists
/// show it: the folder list, and the profile, proxy and trash lists.
fn seen_in(db: &Connection, g: &[Grant], scope: &Scope) -> Result<bool> {
    match scope {
        Scope::Team => Ok(true),
        // Where a profile in no folder is: there is no name to hide.
        Scope::Folder { name } if name.is_empty() => Ok(true),
        Scope::Folder { name } => sees_folder_in(db, g, name),
        Scope::Profile { id } => sees_id_in(db, g, id),
    }
}

/// Whether a permission carries rules naming `scope`: what the lists show,
/// and a folder they may See or make profiles in by name, made yet or not.
fn sees_named(db: &Connection, g: &[Grant], scope: &Scope) -> Result<bool> {
    match scope {
        Scope::Folder { name } if view::names_folder(g, name) => Ok(true),
        _ => seen_in(db, g, scope),
    }
}

fn ids_where(db: &Connection, sql: &str, key: &str) -> Result<Vec<String>> {
    let mut q = db.prepare_cached(sql)?;
    let rows = q.query_map(params![key], |r| r.get::<_, String>(0))?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

/// GET /folders' rule over a folder that is there, or the trash list showing
/// it as where a deleted profile they may See was.
fn sees_folder_in(db: &Connection, g: &[Grant], name: &str) -> Result<bool> {
    let row = db
        .query_row("SELECT 1 FROM folders WHERE space = 'team' AND name = ?1", params![name], |_| Ok(()))
        .optional()?
        .is_some();
    // Its profiles are read only when a rule and a row do not settle it.
    let by_rule = view::may_see(g, &Target::folder(name));
    let ids = if by_rule && row {
        Vec::new()
    } else {
        ids_where(db, "SELECT id FROM profiles WHERE space = 'team' AND folder = ?1", name)?
    };
    if (row || !ids.is_empty()) && view::sees_folder(g, name, ids.iter().map(String::as_str)) {
        return Ok(true);
    }
    let trashed = ids_where(db, "SELECT id FROM trash WHERE kind = 'profile' AND folder = ?1", name)?;
    Ok(trashed.iter().any(|id| view::may_see(g, &Target::profile(id, name))))
}

/// A team profile or proxy, live or in the trash, that the rules let them See.
fn sees_id_in(db: &Connection, g: &[Grant], id: &str) -> Result<bool> {
    if let Some((p, space)) = profile_with_space(db, id)? {
        return Ok(space == TEAM && view::may_see(g, &Target::profile(id, &p.folder)));
    }
    if let Some((_, space)) = proxy_with_space(db, id)? {
        return Ok(space == TEAM && view::may_see(g, &view::proxy_target(id)));
    }
    Ok(match trashed(db, id)? {
        Some(t) if t.kind == TrashKind::Profile => view::may_see(g, &Target::profile(id, &t.folder)),
        Some(t) if t.kind == TrashKind::Proxy => view::may_see(g, &view::proxy_target(id)),
        _ => false,
    })
}

/// The rules of `g` a permission carries, or of those naming one of `only`:
/// the team's own, and each folder or profile rule whose name they can see.
fn in_view(db: &Connection, g: &[Grant], only: Option<&[Scope]>) -> Result<Vec<Grant>> {
    let mut seen: Vec<(&Scope, bool)> = Vec::new();
    let mut out = Vec::new();
    for r in g {
        if only.is_some_and(|o| !o.contains(&r.scope)) {
            continue;
        }
        let shown = match seen.iter().find(|(s, _)| *s == &r.scope) {
            Some(&(_, b)) => b,
            None => {
                let b = sees_named(db, g, &r.scope)?;
                seen.push((&r.scope, b));
                b
            }
        };
        if shown {
            out.push(r.clone());
        }
    }
    view::sort_rules(&mut out);
    Ok(out)
}

/// What a member's permission carries of a change's scopes, or all of it
/// when `whole`, and which of those scopes their lists show.
#[derive(PartialEq)]
struct Sight {
    seen: Vec<bool>,
    rules: Vec<Grant>,
}

/// Every member's sight of `scopes`; none when the change is outside the team.
fn sights(db: &Connection, scopes: &[Scope], whole: bool) -> Result<Vec<(String, Sight)>> {
    if scopes.is_empty() {
        return Ok(Vec::new());
    }
    let mut q = db.prepare_cached("SELECT id FROM members ORDER BY id")?;
    let members = q.query_map([], |r| r.get::<_, String>(0))?.collect::<rusqlite::Result<Vec<_>>>()?;
    members
        .into_iter()
        .map(|m| {
            let g = grants_in(db, &m)?;
            let seen = scopes.iter().map(|s| seen_in(db, &g, s)).collect::<Result<Vec<_>>>()?;
            let rules = in_view(db, &g, (!whole).then_some(scopes))?;
            Ok((m, Sight { seen, rules }))
        })
        .collect()
}

/// The folder and profile scopes a change touches; the rest of every
/// permission stays as it was. "" names no folder and is always carried.
fn touched(folders: &[&str], ids: &[&str]) -> Vec<Scope> {
    let mut out: Vec<Scope> = Vec::new();
    let named = folders
        .iter()
        .filter(|f| !f.is_empty())
        .map(|f| Scope::Folder { name: (*f).to_owned() })
        .chain(ids.iter().map(|id| Scope::Profile { id: (*id).to_owned() }));
    for s in named {
        if !out.contains(&s) {
            out.push(s);
        }
    }
    out
}

/// What each member sees of a change's scopes before it, to compare after.
struct Watch {
    scopes: Vec<Scope>,
    whole: bool,
    before: Vec<(String, Sight)>,
}

impl Watch {
    fn start(db: &Connection, scopes: Vec<Scope>) -> Result<Self> {
        let before = sights(db, &scopes, false)?;
        Ok(Watch { scopes, whole: false, before })
    }

    /// For a change that rewrites the rules naming `scopes`: the whole
    /// permission is compared, as the new rules may judge other things.
    fn rules(db: &Connection, scopes: Vec<Scope>) -> Result<Self> {
        let before = sights(db, &scopes, true)?;
        Ok(Watch { scopes, whole: true, before })
    }

    /// Move the epoch of each member who now sees any of it otherwise, or
    /// whose permission carries other rules, in the change's own transaction.
    fn finish(self, db: &Connection) -> Result<Vec<(String, u64)>> {
        let after = sights(db, &self.scopes, self.whole)?;
        let mut moved = Vec::new();
        for (m, now) in &after {
            if self.before.iter().find(|(id, _)| id == m).is_none_or(|(_, was)| was != now) {
                moved.push((m.clone(), bump_epoch(db, m)?));
            }
        }
        Ok(moved)
    }
}

/// A lease as taken: `fresh` when it was not this device's a moment ago, so a
/// renewal can be told apart from an open.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Taken {
    pub lease: Lease,
    pub fresh: bool,
}

/// Open a profile on a device, or renew the lease that device already holds.
/// `Err(held)` when another device holds a live one.
fn acquire_in(
    db: &Connection,
    id: &str,
    member_id: &str,
    device_id: &str,
    device_name: &str,
    ttl_secs: u64,
) -> Result<std::result::Result<Taken, Lease>> {
    if let Some(held) = live_lease(db, id)? {
        if !(held.member_id == member_id && held.device_id == device_id) {
            return Ok(Err(held));
        }
    }
    let t = lease_now();
    // Renewing keeps the time it was first opened; a fresh lease starts now.
    let renewing: Option<u64> = db
        .query_row(
            "SELECT acquired_at FROM leases
             WHERE item_id = ?1 AND member_id = ?2 AND device_id = ?3 AND expires_at > ?4",
            params![id, member_id, device_id, t as i64],
            |r| r.get::<_, i64>(0),
        )
        .optional()?
        .map(|v| v as u64);
    let acquired = renewing.unwrap_or(t);
    let lease = Lease {
        member_id: member_id.into(),
        device_id: device_id.into(),
        device_name: device_name.into(),
        acquired_at: acquired,
        expires_at: t + ttl_secs,
    };
    db.execute(
        "INSERT INTO leases(item_id, member_id, device_id, device_name, acquired_at, expires_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)
         ON CONFLICT(item_id) DO UPDATE SET
            member_id = excluded.member_id, device_id = excluded.device_id,
            device_name = excluded.device_name, acquired_at = excluded.acquired_at,
            expires_at = excluded.expires_at",
        params![
            id,
            member_id,
            device_id,
            device_name,
            lease.acquired_at as i64,
            lease.expires_at as i64
        ],
    )?;
    Ok(Ok(Taken { lease, fresh: renewing.is_none() }))
}

/// The database holds the admin secret and the signing key: owner-only, and
/// SQLite gives its -wal and -shm files the same mode.
#[cfg(unix)]
fn keep_private(db: &Path) -> Result<()> {
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .open(db)
        .with_context(|| format!("create {}", db.display()))?;
    // A database made before this was readable by everyone: tighten what is there.
    for suffix in ["", "-wal", "-shm"] {
        let mut p = db.as_os_str().to_owned();
        p.push(suffix);
        let p = std::path::PathBuf::from(p);
        match std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o600)) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e).with_context(|| format!("restrict {}", p.display())),
        }
    }
    Ok(())
}

#[cfg(not(unix))]
fn keep_private(_db: &Path) -> Result<()> {
    // No mode bits; the file inherits the data directory's ACL.
    Ok(())
}

/// Written from a revision the row is no longer at: stale. No base, no check.
/// A row that is not there is at 0, so a base of 0 means "only if new".
fn check_base(base: Option<u64>, current: u64) -> std::result::Result<(), Refusal> {
    match base {
        Some(b) if b != current => Err(Refusal::Stale { current }),
        _ => Ok(()),
    }
}

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// The clock leases run on: the wall clock as this process started, moved on by
/// a clock nobody can set. A wall clock set forward never ends a lease early.
fn lease_now() -> u64 {
    static START: std::sync::OnceLock<(std::time::Duration, std::time::Instant)> = std::sync::OnceLock::new();
    let (wall, at) = START.get_or_init(|| {
        let wall = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default();
        (wall, std::time::Instant::now())
    });
    (*wall + at.elapsed()).as_secs()
}

// ---- blobs: sessions and packages ----
//
// A head row points at a body of sealed chunks, each moved under the lock on
// its own. An upload fills a new body; one nothing points at is collected later.

/// A profile's session, keyed by the profile's id alone (space ''): it moves with the profile.
const SESSION: &str = "session";
/// An extension's package, keyed by the item's space and id.
const PACKAGE: &str = "package";
const EXTENSION: &str = "extension";

/// The bytes in one chunk of a blob; the last may be shorter.
pub const CHUNK: usize = 512 * 1024;

/// Chunks the collector frees per hold of the lock.
const COLLECT_BATCH: i64 = 16;

// What each sealed thing is bound to: a blob moved onto another row does not open.
fn card_aad(id: &str) -> String {
    format!("card:{id}")
}
fn session_aad(id: &str) -> String {
    format!("session:{id}")
}
fn package_aad(space: &str, id: &str) -> String {
    format!("package:{space}:{id}")
}
/// One chunk of one body of a blob, and whether it is the last: chunks do not
/// open out of place, out of order, or with the end cut off.
fn chunk_aad(blob: &str, body: i64, seq: u64, last: bool) -> String {
    format!("{blob}#{body}.{seq}{}", if last { ".end" } else { "" })
}
fn item_aad(space: &str, kind: &str, id: &str) -> String {
    format!("item:{kind}:{space}:{id}")
}

/// A large blob's own revision and size: a session or an extension's package.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlobMeta {
    pub rev: u64,
    pub size: u64,
    pub updated_at: u64,
    pub updated_by: String,
}

/// rev, size, updated_at, updated_by from `at` on; None when the join found none.
fn blob_meta_at(r: &rusqlite::Row<'_>, at: usize) -> rusqlite::Result<Option<BlobMeta>> {
    let Some(rev) = r.get::<_, Option<i64>>(at)? else { return Ok(None) };
    Ok(Some(BlobMeta {
        rev: rev as u64,
        size: r.get::<_, i64>(at + 1)? as u64,
        updated_at: r.get::<_, i64>(at + 2)? as u64,
        updated_by: r.get(at + 3)?,
    }))
}

/// A blob's meta and the body that holds its bytes.
fn blob_at(db: &Connection, kind: &str, space: &str, id: &str) -> Result<Option<(BlobMeta, i64)>> {
    Ok(db
        .query_row(
            "SELECT rev, size, updated_at, updated_by, body FROM blobs
             WHERE kind = ?1 AND space = ?2 AND id = ?3",
            params![kind, space, id],
            |r| {
                let meta = BlobMeta {
                    rev: r.get::<_, i64>(0)? as u64,
                    size: r.get::<_, i64>(1)? as u64,
                    updated_at: r.get::<_, i64>(2)? as u64,
                    updated_by: r.get(3)?,
                };
                Ok((meta, r.get::<_, i64>(4)?))
            },
        )
        .optional()?)
}

/// Point a blob at a finished upload's body, at revision `rev`. The body it
/// pointed at before is left for the collector.
fn head_put(db: &Connection, kind: &str, space: &str, id: &str, rev: u64, by: &str, up: &Upload) -> Result<BlobMeta> {
    let ts = now();
    db.execute(
        "INSERT INTO blobs(kind, space, id, rev, size, updated_at, updated_by, body)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
         ON CONFLICT(kind, space, id) DO UPDATE SET
            rev = excluded.rev, size = excluded.size, updated_at = excluded.updated_at,
            updated_by = excluded.updated_by, body = excluded.body",
        params![kind, space, id, rev as i64, up.size as i64, ts as i64, by, up.body],
    )?;
    Ok(BlobMeta { rev, size: up.size, updated_at: ts, updated_by: by.into() })
}

/// A blob coming in: the new body its chunks go to, what they are sealed to,
/// and how far it has got. The body is held until [`Store::let_go`].
#[derive(Debug)]
pub struct Upload {
    pub body: i64,
    aad: String,
    next: u64,
    pub size: u64,
    done: bool,
}

/// Where a stored blob's bytes are, to be read a chunk at a time. Handed out
/// held; [`Store::let_go`] once read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BodyRef {
    pub body: i64,
    pub size: u64,
    aad: String,
}

impl BodyRef {
    /// How many chunks it is in: an empty blob is one empty chunk.
    pub fn chunks(&self) -> u64 {
        self.size.div_ceil(CHUNK as u64).max(1)
    }
}

/// The aad a stored blob was sealed to, by its kind.
fn blob_aad(kind: &str, space: &str, id: &str) -> Result<String> {
    match kind {
        SESSION => Ok(session_aad(id)),
        PACKAGE => Ok(package_aad(space, id)),
        other => bail!("unknown blob kind in the database: {other}"),
    }
}

fn blob_drop(db: &Connection, kind: &str, space: &str, id: &str) -> Result<()> {
    db.execute(
        "DELETE FROM blobs WHERE kind = ?1 AND space = ?2 AND id = ?3",
        params![kind, space, id],
    )?;
    Ok(())
}

/// A profile as lists show it: its fields, its card and its session's revision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Listed {
    pub profile: TeamProfile,
    pub card: Card,
    pub session: Option<BlobMeta>,
}

const LISTED_COLUMNS: &str = "p.id, p.name, p.folder, p.revision, p.updated_at, p.updated_by,
     p.created_at, p.card, b.rev, b.size, b.updated_at, b.updated_by, p.space";
const LISTED_FROM: &str =
    "profiles p LEFT JOIN blobs b ON b.kind = 'session' AND b.space = '' AND b.id = p.id";

type ListedRow = (TeamProfile, Option<Vec<u8>>, Option<BlobMeta>, String);

fn listed_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<ListedRow> {
    Ok((profile_row(r)?, r.get(7)?, blob_meta_at(r, 8)?, r.get(12)?))
}

/// What a session read finds beside the bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionHead {
    /// None until the first upload.
    pub session: Option<BlobMeta>,
    /// The proxy the profile's card binds, if any.
    pub proxy_id: Option<String>,
    /// That proxy and its data, opened — only when it is in the profile's own space.
    pub proxy: Option<(TeamProxy, Vec<u8>)>,
}

// ---- generic items ----

/// A bookmark, extension, automation or tag, in a space; its data is sealed.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Item {
    pub kind: String,
    pub id: String,
    pub name: String,
    pub revision: u64,
    pub updated_at: u64,
    pub updated_by: String,
}

/// An item with its data opened, and an extension's package if it has one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredItem {
    pub item: Item,
    pub data: Vec<u8>,
    pub package: Option<BlobMeta>,
}

fn item_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<Item> {
    Ok(Item {
        kind: r.get(0)?,
        id: r.get(1)?,
        name: r.get(2)?,
        revision: r.get::<_, i64>(3)? as u64,
        updated_at: r.get::<_, i64>(4)? as u64,
        updated_by: r.get(5)?,
    })
}

/// An item and whether it waits in the trash.
fn item_at(db: &Connection, space: &str, kind: &str, id: &str) -> Result<Option<(Item, bool)>> {
    Ok(db
        .query_row(
            "SELECT kind, id, name, revision, updated_at, updated_by, deleted_at IS NOT NULL
             FROM items WHERE space = ?1 AND kind = ?2 AND id = ?3",
            params![space, kind, id],
            |r| Ok((item_row(r)?, r.get::<_, bool>(6)?)),
        )
        .optional()?)
}

const STORED_SELECT: &str = "SELECT i.kind, i.id, i.name, i.revision, i.updated_at, i.updated_by,
            i.sealed, b.rev, b.size, b.updated_at, b.updated_by
     FROM items i LEFT JOIN blobs b
       ON b.kind = 'package' AND b.space = i.space AND b.id = i.id AND i.kind = 'extension'";

type StoredRow = (Item, Vec<u8>, Option<BlobMeta>);

fn stored_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<StoredRow> {
    Ok((item_row(r)?, r.get(6)?, blob_meta_at(r, 7)?))
}

/// Team items in the trash, deleted at or before `cutoff`.
fn trashed_items(db: &Connection, cutoff: u64) -> Result<Vec<TrashItem>> {
    let mut q = db.prepare(
        "SELECT kind, id, name, revision, updated_at, updated_by, deleted_at, deleted_by FROM items
         WHERE space = 'team' AND deleted_at IS NOT NULL AND deleted_at <= ?1
         ORDER BY deleted_at, kind, id",
    )?;
    let rows = q
        .query_map(params![cutoff.min(i64::MAX as u64) as i64], |r| {
            Ok((item_row(r)?, r.get::<_, i64>(6)? as u64, r.get::<_, String>(7)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    rows.into_iter().map(|(i, at, by)| item_in_trash(i, at, by)).collect()
}

fn item_in_trash(i: Item, deleted_at: u64, deleted_by: String) -> Result<TrashItem> {
    let Some(kind) = TrashKind::of_item(&i.kind) else {
        bail!("unknown item kind in the database: {}", i.kind)
    };
    Ok(TrashItem {
        id: i.id,
        kind,
        name: i.name,
        folder: String::new(),
        proxy_kind: String::new(),
        country: String::new(),
        revision: i.revision,
        updated_at: i.updated_at,
        updated_by: i.updated_by,
        deleted_at,
        deleted_by,
    })
}

fn trashed_item(db: &Connection, kind: &str, id: &str) -> Result<Option<TrashItem>> {
    let row = db
        .query_row(
            "SELECT kind, id, name, revision, updated_at, updated_by, deleted_at, deleted_by FROM items
             WHERE space = 'team' AND kind = ?1 AND id = ?2 AND deleted_at IS NOT NULL",
            params![kind, id],
            |r| Ok((item_row(r)?, r.get::<_, i64>(6)? as u64, r.get::<_, String>(7)?)),
        )
        .optional()?;
    row.map(|(i, at, by)| item_in_trash(i, at, by)).transpose()
}

/// Whether a table has a column; false when there is no such table.
fn has_column(db: &Connection, table: &str, column: &str) -> Result<bool> {
    let n: i64 = db.query_row(
        &format!("SELECT COUNT(*) FROM pragma_table_info('{table}') WHERE name = ?1"),
        params![column],
        |r| r.get(0),
    )?;
    Ok(n > 0)
}

/// A column a newer server added, put on a table an older one made.
fn add_column(db: &Connection, table: &str, column: &str, decl: &str) -> Result<()> {
    if !has_column(db, table, column)? {
        db.execute_batch(&format!("ALTER TABLE {table} ADD COLUMN {column} {decl}"))?;
    }
    Ok(())
}

impl Store {
    pub fn open(path: &Path, vault: Vault) -> Result<Self> {
        keep_private(path)?;
        let db = Connection::open(path).with_context(|| format!("open {}", path.display()))?;
        db.pragma_update(None, "journal_mode", "WAL")?;
        // A large session goes through the WAL; it shrinks back after a checkpoint.
        db.pragma_update(None, "journal_size_limit", 64 * 1024 * 1024)?;
        db.pragma_update(None, "foreign_keys", "ON")?;
        let s = Store::with(db, vault);
        s.migrate()?;
        Ok(s)
    }

    fn with(db: Connection, vault: Vault) -> Self {
        Store {
            db: Mutex::new(db),
            vault,
            held: Mutex::default(),
            hold_count: AtomicU64::new(0),
            regranted: Mutex::default(),
            seen: Mutex::default(),
        }
    }

    fn lock(&self) -> Locked<'_> {
        let conn = self.db.lock().unwrap();
        Locked { conn, since: Instant::now(), store: self }
    }

    /// Test check: how many times the database was held since the last call.
    #[cfg(test)]
    pub fn holds(&self) -> u64 {
        self.hold_count.swap(0, Ordering::Relaxed)
    }

    #[cfg(test)]
    pub fn open_memory() -> Result<Self> {
        let db = Connection::open_in_memory()?;
        db.pragma_update(None, "foreign_keys", "ON")?;
        let s = Store::with(db, Vault::for_tests());
        s.migrate()?;
        Ok(s)
    }

    fn migrate(&self) -> Result<()> {
        let mut db = self.lock();
        // All of it or none: a start that fails here leaves the file as it was.
        let tx = db.transaction()?;
        // An older server kept each blob whole in one row: that table makes way
        // for the chunked one, and its rows move over below.
        let whole = has_column(&tx, "blobs", "envelope")?;
        if whole {
            tx.execute_batch("ALTER TABLE blobs RENAME TO blobs_whole")?;
        }
        tx.execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS meta (
                key   TEXT PRIMARY KEY,
                value TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS members (
                id         TEXT PRIMARY KEY,
                name       TEXT NOT NULL,
                token_hash TEXT NOT NULL UNIQUE,
                active     INTEGER NOT NULL DEFAULT 1,
                created_at INTEGER NOT NULL,
                -- Bumped when this member's grants, or which of the things they
                -- name they can see, change; an older permission reads as stale.
                epoch      INTEGER NOT NULL DEFAULT 1
            );

            CREATE TABLE IF NOT EXISTS grants (
                member_id  TEXT NOT NULL REFERENCES members(id) ON DELETE CASCADE,
                scope_kind TEXT NOT NULL,   -- team | folder | profile
                scope_key  TEXT NOT NULL,   -- '' for team, folder name, or profile id
                action     TEXT NOT NULL,
                allow      INTEGER NOT NULL,
                PRIMARY KEY (member_id, scope_kind, scope_key, action)
            );
            -- Who names a folder or profile: asked on every change to what is visible.
            CREATE INDEX IF NOT EXISTS grants_scope ON grants(scope_kind, scope_key);

            CREATE TABLE IF NOT EXISTS profiles (
                id         TEXT PRIMARY KEY,
                -- 'team', or 'personal:<member id>' for a member's own profiles,
                -- kept here so they can use them from any of their devices.
                space      TEXT NOT NULL DEFAULT 'team',
                name       TEXT NOT NULL,
                folder     TEXT NOT NULL DEFAULT '',
                revision   INTEGER NOT NULL DEFAULT 1,
                updated_at INTEGER NOT NULL,
                updated_by TEXT NOT NULL,
                -- Encrypted at rest with the server's own key, which is kept in
                -- a file of its own beside this database: a copy of the
                -- database alone opens nothing.
                sealed     BLOB NOT NULL
            );

            CREATE TABLE IF NOT EXISTS proxies (
                id         TEXT PRIMARY KEY,
                space      TEXT NOT NULL DEFAULT 'team',
                name       TEXT NOT NULL,
                kind       TEXT NOT NULL,
                country    TEXT NOT NULL DEFAULT '',
                revision   INTEGER NOT NULL DEFAULT 1,
                updated_at INTEGER NOT NULL,
                updated_by TEXT NOT NULL,
                sealed     BLOB NOT NULL
            );

            CREATE INDEX IF NOT EXISTS profiles_folder ON profiles(folder);
            CREATE INDEX IF NOT EXISTS profiles_space ON profiles(space);
            CREATE INDEX IF NOT EXISTS proxies_space ON proxies(space);

            -- Folders are their own rows, not just a name a profile happens to
            -- carry: an empty folder can exist, be granted on before anything
            -- is put in it, and keep its grants when it is renamed.
            CREATE TABLE IF NOT EXISTS folders (
                space      TEXT NOT NULL,
                name       TEXT NOT NULL,
                color      TEXT NOT NULL DEFAULT '',
                created_at INTEGER NOT NULL,
                PRIMARY KEY (space, name)
            );

            -- Who has a profile open right now, and on which device. One row per
            -- profile at most: that is the whole point. A lease runs out on its
            -- own unless the device keeps renewing it, so a crashed launcher or a
            -- closed laptop frees the profile instead of holding it for ever.
            CREATE TABLE IF NOT EXISTS leases (
                item_id     TEXT PRIMARY KEY,
                member_id   TEXT NOT NULL,
                device_id   TEXT NOT NULL,
                device_name TEXT NOT NULL,
                acquired_at INTEGER NOT NULL,
                expires_at  INTEGER NOT NULL
            );

            -- Deleted team items, sealed, until restored, purged or out of days.
            -- The id stays taken meanwhile; `folder` is where it was deleted from.
            CREATE TABLE IF NOT EXISTS trash (
                id          TEXT PRIMARY KEY,
                kind        TEXT NOT NULL,              -- profile | proxy
                name        TEXT NOT NULL,
                folder      TEXT NOT NULL DEFAULT '',
                proxy_kind  TEXT NOT NULL DEFAULT '',
                country     TEXT NOT NULL DEFAULT '',
                revision    INTEGER NOT NULL,
                updated_at  INTEGER NOT NULL,
                updated_by  TEXT NOT NULL,
                deleted_at  INTEGER NOT NULL,
                deleted_by  TEXT NOT NULL,
                sealed      BLOB NOT NULL
            );
            CREATE INDEX IF NOT EXISTS trash_deleted_at ON trash(deleted_at);

            -- Large blobs: a profile's session (space '', it moves with the
            -- profile) and an extension's package (in its item's space). Each
            -- has its own rev; its bytes are the chunks of `body`.
            CREATE TABLE IF NOT EXISTS blobs (
                kind       TEXT NOT NULL,   -- session | package
                space      TEXT NOT NULL,
                id         TEXT NOT NULL,
                rev        INTEGER NOT NULL,
                size       INTEGER NOT NULL,
                updated_at INTEGER NOT NULL,
                updated_by TEXT NOT NULL,
                body       INTEGER NOT NULL,
                PRIMARY KEY (kind, space, id)
            );
            CREATE INDEX IF NOT EXISTS blobs_body ON blobs(body);

            -- One upload's bytes. One no blob points at is being written,
            -- still being read, or left for the collector.
            CREATE TABLE IF NOT EXISTS bodies (
                id         INTEGER PRIMARY KEY AUTOINCREMENT,
                created_at INTEGER NOT NULL
            );

            -- Sealed one by one, each bound to its body, place and whether it is the last.
            CREATE TABLE IF NOT EXISTS chunks (
                body   INTEGER NOT NULL,
                seq    INTEGER NOT NULL,
                sealed BLOB NOT NULL,
                PRIMARY KEY (body, seq)
            );

            -- Bookmarks, extensions, automations and tags, in the team or in a
            -- member's own space. One id per kind per space.
            CREATE TABLE IF NOT EXISTS items (
                space      TEXT NOT NULL,
                kind       TEXT NOT NULL,
                id         TEXT NOT NULL,
                name       TEXT NOT NULL DEFAULT '',
                revision   INTEGER NOT NULL DEFAULT 1,
                updated_at INTEGER NOT NULL,
                updated_by TEXT NOT NULL,
                -- Set while a team item waits in the trash; the row keeps its id taken.
                deleted_at INTEGER,
                deleted_by TEXT,
                sealed     BLOB NOT NULL,
                PRIMARY KEY (space, kind, id)
            );
            CREATE INDEX IF NOT EXISTS items_deleted_at ON items(deleted_at);
            "#,
        )?;
        // A profile's card, sealed: what lists show beside its name.
        add_column(&tx, "profiles", "card", "BLOB")?;
        add_column(&tx, "trash", "card", "BLOB")?;
        // When a profile was made; null for one an older server kept, never guessed.
        add_column(&tx, "profiles", "created_at", "INTEGER")?;
        add_column(&tx, "trash", "created_at", "INTEGER")?;
        // Null until the member first uses their invite.
        add_column(&tx, "members", "last_seen_at", "INTEGER")?;
        // Hashes of re-issued tokens, so their holder hears "replaced", not "removed".
        tx.execute_batch(
            "CREATE TABLE IF NOT EXISTS replaced_tokens (
                token_hash  TEXT PRIMARY KEY,
                member_id   TEXT NOT NULL REFERENCES members(id) ON DELETE CASCADE,
                replaced_at INTEGER NOT NULL
            );",
        )?;
        if whole {
            self.rechunk(&tx)?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Move the blobs an older server kept whole into chunks, in the caller's transaction.
    fn rechunk(&self, tx: &Connection) -> Result<()> {
        type Whole = (String, String, String, i64, i64, String, Vec<u8>, Vec<u8>);
        let rows: Vec<Whole> = {
            let mut q = tx.prepare(
                "SELECT kind, space, id, rev, updated_at, updated_by, envelope, sealed FROM blobs_whole",
            )?;
            let rows = q
                .query_map([], |r| {
                    Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?, r.get(7)?))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            rows
        };
        for (kind, space, id, rev, at, by, envelope, mut bytes) in rows {
            let aad = blob_aad(&kind, &space, &id)?;
            self.vault.open_detached(&aad, &envelope, &mut bytes)?;
            let body = self.write_body_in(tx, &aad, &bytes)?;
            tx.execute(
                "INSERT INTO blobs(kind, space, id, rev, size, updated_at, updated_by, body)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![kind, space, id, rev, bytes.len() as i64, at, by, body],
            )?;
        }
        tx.execute_batch("DROP TABLE blobs_whole")?;
        Ok(())
    }

    /// A whole blob written as a new body in the caller's transaction.
    fn write_body_in(&self, db: &Connection, aad: &str, data: &[u8]) -> Result<i64> {
        db.execute("INSERT INTO bodies(created_at) VALUES (?1)", params![now() as i64])?;
        let body = db.last_insert_rowid();
        let mut pieces: Vec<&[u8]> = data.chunks(CHUNK).collect();
        if pieces.is_empty() {
            pieces.push(&[]);
        }
        let last = pieces.len() - 1;
        for (seq, piece) in pieces.into_iter().enumerate() {
            let sealed = self.vault.seal(&chunk_aad(aad, body, seq as u64, seq == last), piece)?;
            db.execute(
                "INSERT INTO chunks(body, seq, sealed) VALUES (?1, ?2, ?3)",
                params![body, seq as i64, sealed],
            )?;
        }
        Ok(body)
    }

    // ---- meta ----

    pub fn meta_get(&self, key: &str) -> Result<Option<String>> {
        let db = self.lock();
        Ok(db
            .query_row("SELECT value FROM meta WHERE key = ?1", params![key], |r| r.get(0))
            .optional()?)
    }

    /// Several rows in one transaction: all of them are kept, or none.
    pub fn meta_set_all(&self, rows: &[(&str, &str)]) -> Result<()> {
        let mut db = self.lock();
        let tx = db.transaction()?;
        for (key, value) in rows {
            tx.execute(
                "INSERT INTO meta(key, value) VALUES (?1, ?2)
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                params![key, value],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    // ---- members ----

    /// Add a member. `token_hash` is the hash of the token shown to the admin
    /// once; the token itself never reaches this file.
    pub fn add_member(&self, id: &str, name: &str, token_hash: &str) -> Result<Member> {
        if id == ADMIN_ID {
            bail!("{ADMIN_ID} is the admin's own id, not a member's");
        }
        let db = self.lock();
        let created = now();
        db.execute(
            "INSERT INTO members(id, name, token_hash, active, created_at, epoch)
             VALUES (?1, ?2, ?3, 1, ?4, 1)",
            params![id, name, token_hash, created as i64],
        )?;
        Ok(Member {
            id: id.into(),
            name: name.into(),
            active: true,
            created_at: created,
            epoch: 1,
            last_seen_at: None,
        })
    }

    pub fn member_by_token_hash(&self, token_hash: &str) -> Result<Option<Member>> {
        let db = self.lock();
        let sql = format!("SELECT {MEMBER_COLUMNS} FROM members WHERE token_hash = ?1");
        Ok(db.query_row(&sql, params![token_hash], member_row).optional()?)
    }

    /// Every token hash a member may still come with: current ones and re-issued ones.
    pub fn token_hashes(&self) -> Result<Vec<String>> {
        let db = self.lock();
        let mut q = db.prepare("SELECT token_hash FROM members UNION ALL SELECT token_hash FROM replaced_tokens")?;
        let rows = q.query_map([], |r| r.get(0))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Whether this hash was a member's token before the admin re-issued it.
    pub fn token_replaced(&self, token_hash: &str) -> Result<bool> {
        let db = self.lock();
        Ok(db
            .query_row("SELECT 1 FROM replaced_tokens WHERE token_hash = ?1", params![token_hash], |_| Ok(()))
            .optional()?
            .is_some())
    }

    pub fn member(&self, id: &str) -> Result<Option<Member>> {
        member_in(&self.lock(), id)
    }

    pub fn members(&self) -> Result<Vec<Member>> {
        members_in(&self.lock())
    }

    /// Every member with their whole rule table, in one hold.
    pub fn members_with_grants(&self) -> Result<Vec<(Member, Vec<Grant>)>> {
        let db = self.lock();
        let mut out = Vec::new();
        for m in members_in(&db)? {
            let mut g = grants_in(&db, &m.id)?;
            view::sort_rules(&mut g);
            out.push((m, g));
        }
        Ok(out)
    }

    /// Note a request from a member with this token; written at most once a minute each.
    pub fn seen(&self, member_id: &str, token_hash: &str) -> Result<()> {
        self.seen_at(member_id, token_hash, now())
    }

    /// Only while the token is still theirs: a request let in just before a
    /// re-issue does not mark the new invite used.
    fn seen_at(&self, member_id: &str, token_hash: &str, at: u64) -> Result<()> {
        {
            let mut seen = self.seen.lock().unwrap();
            let recent = |(hash, last): &(String, u64)| hash == token_hash && last.abs_diff(at) < SEEN_EVERY_SECS;
            if seen.get(member_id).is_some_and(recent) {
                return Ok(());
            }
            seen.insert(member_id.to_owned(), (token_hash.to_owned(), at));
        }
        self.lock().execute(
            "UPDATE members SET last_seen_at = ?2 WHERE id = ?1 AND token_hash = ?3",
            params![member_id, at as i64, token_hash],
        )?;
        Ok(())
    }

    /// Put a new token in place of a member's: the old hash is kept as replaced,
    /// and they read as not seen until the new one is used. None: no such member.
    pub fn reissue_token(&self, id: &str, token_hash: &str) -> Result<Option<Member>> {
        let mut db = self.lock();
        let tx = db.transaction()?;
        let old: Option<String> = tx
            .query_row("SELECT token_hash FROM members WHERE id = ?1", params![id], |r| r.get(0))
            .optional()?;
        let Some(old) = old else { return Ok(None) };
        tx.execute(
            "INSERT OR REPLACE INTO replaced_tokens(token_hash, member_id, replaced_at) VALUES (?1, ?2, ?3)",
            params![old, id, now() as i64],
        )?;
        tx.execute(
            "UPDATE members SET token_hash = ?2, last_seen_at = NULL WHERE id = ?1",
            params![id, token_hash],
        )?;
        let member = member_in(&tx, id)?;
        tx.commit()?;
        self.seen.lock().unwrap().remove(id);
        Ok(member)
    }

    /// Turn a member off (or back on). Off means: no new permission is issued
    /// and the live channel is cut, whatever their grants still say.
    /// Answers the new epoch and the items whose live leases were freed.
    pub fn set_member_active(&self, id: &str, active: bool) -> Result<(u64, Vec<String>)> {
        let mut db = self.lock();
        let tx = db.transaction()?;
        let n = tx.execute(
            "UPDATE members SET active = ?2, epoch = epoch + 1 WHERE id = ?1",
            params![id, active as i64],
        )?;
        if n == 0 {
            bail!("no such member: {id}");
        }
        let mut freed = Vec::new();
        if !active {
            // As when removed: what they had open is free for others now, not
            // when the lease runs out.
            freed = held_by(&tx, id)?;
            tx.execute("DELETE FROM leases WHERE member_id = ?1", params![id])?;
        }
        let epoch = tx.query_row("SELECT epoch FROM members WHERE id = ?1", params![id], |r| {
            r.get::<_, i64>(0)
        })? as u64;
        tx.commit()?;
        Ok((epoch, freed))
    }

    /// Remove a member for good. Their grants go with them; what they made
    /// does NOT — profiles and proxies belong to the team, have no link to the
    /// member who made them beyond the `updated_by` record, and stay readable
    /// for everyone still entitled to them. Their own space stays on the server too.
    /// None when there was no such member; else the items whose leases were freed.
    pub fn delete_member(&self, id: &str) -> Result<Option<Vec<String>>> {
        let mut db = self.lock();
        let tx = db.transaction()?;
        // Whatever they had open is free again: their launcher is told to close
        // it, and nobody else should wait out the lease of someone who is gone.
        let freed = held_by(&tx, id)?;
        tx.execute("DELETE FROM leases WHERE member_id = ?1", params![id])?;
        let gone = tx.execute("DELETE FROM members WHERE id = ?1", params![id])? > 0;
        tx.commit()?;
        self.seen.lock().unwrap().remove(id);
        Ok(gone.then_some(freed))
    }

    // ---- grants ----

    pub fn grants_of(&self, member_id: &str) -> Result<Vec<Grant>> {
        grants_in(&self.lock(), member_id)
    }

    /// Whether a member is still on, and their rules, in one read.
    pub fn standing(&self, member_id: &str) -> Result<Standing> {
        standing_in(&self.lock(), member_id)
    }

    /// The rules a member's permission carries, with its epoch, read in one
    /// go so a change between the two cannot slip in. None once removed.
    pub fn permission_view(&self, member_id: &str) -> Result<Option<InView>> {
        let db = self.lock();
        let row: Option<(i64, i64)> = db
            .query_row("SELECT active, epoch FROM members WHERE id = ?1", params![member_id], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .optional()?;
        let Some((active, epoch)) = row else { return Ok(None) };
        let rules = in_view(&db, &grants_in(&db, member_id)?, None)?;
        Ok(Some(InView { active: active != 0, epoch: epoch as u64, rules }))
    }

    /// Members whose permission moved since the last call, with their new epoch.
    pub fn take_regranted(&self) -> Vec<(String, u64)> {
        std::mem::take(&mut *self.regranted.lock().unwrap())
    }

    /// Commit, then note who is to be told: in commit order, under the store's lock.
    fn commit_regranted(&self, tx: rusqlite::Transaction<'_>, moved: Vec<(String, u64)>) -> Result<()> {
        tx.commit()?;
        if !moved.is_empty() {
            self.regranted.lock().unwrap().extend(moved);
        }
        Ok(())
    }

    /// Replace a member's whole permission table in one go, and bump their
    /// epoch. One statement per row would let a launcher fetch a half-written
    /// table, so this is a transaction and the epoch moves with it.
    pub fn set_grants(&self, member_id: &str, grants: &[Grant]) -> Result<u64> {
        let mut db = self.lock();
        let tx = db.transaction()?;
        let exists: bool = tx
            .query_row("SELECT 1 FROM members WHERE id = ?1", params![member_id], |_| Ok(true))
            .optional()?
            .unwrap_or(false);
        if !exists {
            bail!("no such member: {member_id}");
        }
        tx.execute("DELETE FROM grants WHERE member_id = ?1", params![member_id])?;
        for g in grants {
            let (kind, key) = scope_row(&g.scope);
            let action = match serde_json::to_value(g.action)? {
                serde_json::Value::String(s) => s,
                other => bail!("an action did not serialise to a name: {other}"),
            };
            // The same rule twice is one rule, and an allow beside a refusal is
            // the refusal, as `decide` reads it.
            tx.execute(
                "INSERT INTO grants(member_id, scope_kind, scope_key, action, allow)
                 VALUES (?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT(member_id, scope_kind, scope_key, action)
                 DO UPDATE SET allow = MIN(allow, excluded.allow)",
                params![member_id, kind, key, action, g.allow as i64],
            )?;
        }
        let epoch = bump_epoch(&tx, member_id)?;
        tx.commit()?;
        Ok(epoch)
    }

    // ---- the team's own profiles ----

    /// Test setup: an unguarded write into the team.
    #[cfg(test)]
    pub fn put_profile(
        &self,
        id: &str,
        name: &str,
        folder: &str,
        data: &[u8],
        by: &str,
    ) -> Result<TeamProfile> {
        let sealed = self.vault.seal(&format!("profile:{id}"), data)?;
        let db = self.lock();
        let ts = now();
        db.execute(
            "INSERT INTO profiles(id, name, folder, revision, updated_at, updated_by, sealed, created_at)
             VALUES (?1, ?2, ?3, 1, ?4, ?5, ?6, ?4)
             ON CONFLICT(id) DO UPDATE SET
                name = excluded.name,
                folder = excluded.folder,
                revision = profiles.revision + 1,
                updated_at = excluded.updated_at,
                updated_by = excluded.updated_by,
                sealed = excluded.sealed",
            params![id, name, folder, ts as i64, by, &sealed],
        )?;
        profile_by_id(&db, id)
    }

    pub fn profiles(&self) -> Result<Vec<TeamProfile>> {
        let db = self.lock();
        let mut q = db.prepare(&format!(
            "SELECT {PROFILE_COLUMNS} FROM profiles WHERE space = 'team' ORDER BY name"
        ))?;
        let rows = q.query_map([], profile_row)?.collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// The profile's data, opened. None when there is no such profile.
    pub fn profile_data(&self, id: &str) -> Result<Option<Vec<u8>>> {
        let sealed: Option<Vec<u8>> = {
            let db = self.lock();
            db.query_row("SELECT sealed FROM profiles WHERE id = ?1", params![id], |r| r.get(0))
                .optional()?
        };
        sealed.map(|b| self.vault.open(&format!("profile:{id}"), &b)).transpose()
    }

    #[cfg(test)]
    pub fn profile_folder(&self, id: &str) -> Result<Option<String>> {
        let db = self.lock();
        Ok(db
            .query_row("SELECT folder FROM profiles WHERE id = ?1", params![id], |r| r.get(0))
            .optional()?)
    }

    // ---- folders ----

    pub fn folders_in(&self, space: &str) -> Result<Vec<Folder>> {
        let db = self.lock();
        let mut q = db.prepare(
            "SELECT f.name, f.color,
                    (SELECT COUNT(*) FROM profiles p WHERE p.space = f.space AND p.folder = f.name)
             FROM folders f WHERE f.space = ?1 ORDER BY f.name",
        )?;
        let rows = q
            .query_map(params![space], |r| {
                Ok(Folder {
                    name: r.get(0)?,
                    color: r.get(1)?,
                    profiles: r.get::<_, i64>(2)? as u64,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// Make a folder. False when one of that name is already there.
    pub fn create_folder(&self, space: &str, name: &str, color: &str) -> Result<bool> {
        let mut db = self.lock();
        let tx = db.transaction()?;
        let watch = Watch::start(&tx, if space == TEAM { touched(&[name], &[]) } else { Vec::new() })?;
        let made = tx.execute(
            "INSERT OR IGNORE INTO folders(space, name, color, created_at) VALUES (?1, ?2, ?3, ?4)",
            params![space, name, color, now() as i64],
        )? > 0;
        let moved = watch.finish(&tx)?;
        self.commit_regranted(tx, moved)?;
        Ok(made)
    }

    pub fn set_folder_color(&self, space: &str, name: &str, color: &str) -> Result<bool> {
        let db = self.lock();
        Ok(db.execute(
            "UPDATE folders SET color = ?3 WHERE space = ?1 AND name = ?2",
            params![space, name, color],
        )? > 0)
    }

    /// Rename a folder, and everything that names it, in one go: its profiles
    /// and — in the team — every grant on it. Done in pieces, a member could be
    /// asked about a folder whose rules still carried the old name, and the
    /// rules would silently stop applying.
    pub fn rename_folder(&self, space: &str, from: &str, to: &str) -> Result<FolderRename> {
        let mut db = self.lock();
        let tx = db.transaction()?;
        let exists: bool = tx
            .query_row(
                "SELECT 1 FROM folders WHERE space = ?1 AND name = ?2",
                params![space, from],
                |_| Ok(true),
            )
            .optional()?
            .unwrap_or(false);
        if !exists {
            return Ok(FolderRename::Missing);
        }
        // Taken by a row, or by profiles that carry the name without one
        // (written before folders had rows), live or in the team's trash.
        let taken: bool = tx
            .query_row(
                "SELECT 1 FROM folders WHERE space = ?1 AND name = ?2
                 UNION ALL SELECT 1 FROM profiles WHERE space = ?1 AND folder = ?2
                 UNION ALL SELECT 1 FROM trash WHERE ?1 = 'team' AND kind = 'profile' AND folder = ?2
                 LIMIT 1",
                params![space, to],
                |_| Ok(true),
            )
            .optional()?
            .unwrap_or(false);
        if taken {
            return Ok(FolderRename::NameTaken);
        }
        // Rules already naming `to` merge in, and may judge its profiles anew.
        let watch = Watch::rules(&tx, if space == TEAM { touched(&[from, to], &[]) } else { Vec::new() })?;
        tx.execute(
            "UPDATE folders SET name = ?3 WHERE space = ?1 AND name = ?2",
            params![space, from, to],
        )?;
        // A new revision: a save made from before the rename is stale now,
        // rather than quietly carrying the profile back under the old name.
        tx.execute(
            "UPDATE profiles SET folder = ?3, revision = revision + 1, updated_at = ?4
             WHERE space = ?1 AND folder = ?2",
            params![space, from, to, now() as i64],
        )?;
        if space == TEAM {
            // What was deleted from the folder is still judged by its rules, so it moves too.
            tx.execute(
                "UPDATE trash SET folder = ?2 WHERE kind = 'profile' AND folder = ?1",
                params![from, to],
            )?;
            // A member may already have a rule naming the new name: the two
            // become one, and a refusal wins, as in `decide`.
            tx.execute(
                "INSERT INTO grants(member_id, scope_kind, scope_key, action, allow)
                 SELECT member_id, 'folder', ?2, action, allow FROM grants
                 WHERE scope_kind = 'folder' AND scope_key = ?1
                 ON CONFLICT(member_id, scope_kind, scope_key, action)
                 DO UPDATE SET allow = MIN(allow, excluded.allow)",
                params![from, to],
            )?;
            tx.execute(
                "DELETE FROM grants WHERE scope_kind = 'folder' AND scope_key = ?1",
                params![from],
            )?;
        }
        // Whoever could see it holds a permission naming the old name.
        let moved = watch.finish(&tx)?;
        self.commit_regranted(tx, moved)?;
        Ok(FolderRename::Done)
    }

    /// Remove an empty folder. One with profiles in it is refused rather than
    /// emptied: where they should go is the person's call, not ours. So is one
    /// whose deleted profiles wait in the trash, judged by its rules. Its grants
    /// go with it — they would otherwise wait for any new folder of that name.
    pub fn delete_folder(&self, space: &str, name: &str) -> Result<FolderDelete> {
        let mut db = self.lock();
        let tx = db.transaction()?;
        let count: i64 = tx.query_row(
            "SELECT COUNT(*) FROM profiles WHERE space = ?1 AND folder = ?2",
            params![space, name],
            |r| r.get(0),
        )?;
        if count > 0 {
            return Ok(FolderDelete::NotEmpty(count as u64));
        }
        if space == TEAM {
            let trashed: i64 = tx.query_row(
                "SELECT COUNT(*) FROM trash WHERE kind = 'profile' AND folder = ?1",
                params![name],
                |r| r.get(0),
            )?;
            if trashed > 0 {
                return Ok(FolderDelete::InTrash(trashed as u64));
            }
        }
        let watch = Watch::start(&tx, if space == TEAM { touched(&[name], &[]) } else { Vec::new() })?;
        let gone = tx.execute(
            "DELETE FROM folders WHERE space = ?1 AND name = ?2",
            params![space, name],
        )? > 0;
        if !gone {
            return Ok(FolderDelete::Missing);
        }
        if space == TEAM {
            tx.execute(
                "DELETE FROM grants WHERE scope_kind = 'folder' AND scope_key = ?1",
                params![name],
            )?;
        }
        let moved = watch.finish(&tx)?;
        self.commit_regranted(tx, moved)?;
        Ok(FolderDelete::Done)
    }

    // ---- spaces, leases, and writes that cannot overwrite each other ----

    #[cfg(test)]
    pub fn profiles_in(&self, space: &str) -> Result<Vec<TeamProfile>> {
        let db = self.lock();
        let mut q = db.prepare(&format!(
            "SELECT {PROFILE_COLUMNS} FROM profiles WHERE space = ?1 ORDER BY name"
        ))?;
        let rows = q
            .query_map(params![space], profile_row)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    pub fn proxies_in(&self, space: &str) -> Result<Vec<TeamProxy>> {
        let db = self.lock();
        let mut q = db.prepare(
            "SELECT id, name, kind, country, revision, updated_at, updated_by FROM proxies
             WHERE space = ?1 ORDER BY name",
        )?;
        let rows = q
            .query_map(params![space], proxy_row)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// A space's profiles as lists show them, by name; `ids` narrows them, and
    /// unknown ones are left out.
    pub fn listed_in(&self, space: &str, ids: Option<&[&str]>) -> Result<Vec<Listed>> {
        if ids.is_some_and(|i| i.is_empty()) {
            return Ok(Vec::new());
        }
        let rows = {
            let db = self.lock();
            let mut sql = format!("SELECT {LISTED_COLUMNS} FROM {LISTED_FROM} WHERE p.space = ?");
            if let Some(ids) = ids {
                sql.push_str(&format!(" AND p.id IN ({})", vec!["?"; ids.len()].join(", ")));
            }
            sql.push_str(" ORDER BY p.name");
            let mut q = db.prepare(&sql)?;
            let args = std::iter::once(&space).chain(ids.unwrap_or_default().iter());
            let rows = q
                .query_map(rusqlite::params_from_iter(args), listed_row)?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            rows
        };
        rows.into_iter().map(|row| self.listed(row)).collect()
    }

    /// One profile as listed, with the space it is in.
    pub fn listed_one(&self, id: &str) -> Result<Option<(Listed, String)>> {
        let row = {
            let db = self.lock();
            db.query_row(
                &format!("SELECT {LISTED_COLUMNS} FROM {LISTED_FROM} WHERE p.id = ?1"),
                params![id],
                listed_row,
            )
            .optional()?
        };
        row.map(|r| {
            let space = r.3.clone();
            Ok((self.listed(r)?, space))
        })
        .transpose()
    }

    fn listed(&self, (profile, card, session, _): ListedRow) -> Result<Listed> {
        Ok(Listed { card: self.open_card(&profile.id, card.as_deref())?, profile, session })
    }

    /// A stored card, opened; none stored is the empty card.
    fn open_card(&self, id: &str, sealed: Option<&[u8]>) -> Result<Card> {
        match sealed {
            None => Ok(Card::default()),
            Some(b) => {
                let plain = self.vault.open(&card_aad(id), b)?;
                serde_json::from_slice(&plain).context("a stored card does not read")
            }
        }
    }

    /// A profile's card as the row has it, under the caller's lock.
    fn card_in(&self, db: &Connection, id: &str) -> Result<Card> {
        let sealed: Option<Vec<u8>> = db
            .query_row("SELECT card FROM profiles WHERE id = ?1", params![id], |r| r.get(0))
            .optional()?
            .flatten();
        self.open_card(id, sealed.as_deref())
    }

    /// Whether the team already names a proxy id: a rule, or the card of a
    /// team profile, live or in the trash, that binds it.
    fn team_names_proxy(&self, db: &Connection, id: &str) -> Result<bool> {
        if named_by_grant(db, id)? {
            return Ok(true);
        }
        let mut q = db.prepare(
            "SELECT id, card FROM profiles WHERE space = 'team' AND card IS NOT NULL
             UNION ALL SELECT id, card FROM trash WHERE kind = 'profile' AND card IS NOT NULL",
        )?;
        let cards = q
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, Vec<u8>>(1)?)))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        for (profile, sealed) in cards {
            if self.open_card(&profile, Some(&sealed))?.bound_proxy() == Some(id) {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// Every live lease, by the item it holds.
    pub fn live_leases(&self) -> Result<Vec<(String, Lease)>> {
        live_leases_in(&self.lock())
    }

    /// One profile's fields and the space it lives in, by id.
    pub fn profile_meta(&self, id: &str) -> Result<Option<(TeamProfile, String)>> {
        profile_with_space(&self.lock(), id)
    }

    pub fn proxy_meta(&self, id: &str) -> Result<Option<(TeamProxy, String)>> {
        proxy_with_space(&self.lock(), id)
    }

    /// The live lease on a profile, if anybody holds one.
    pub fn lease_live(&self, id: &str) -> Result<Option<Lease>> {
        let db = self.lock();
        live_lease(&db, id)
    }

    /// Test setup: a lease with no profile, space or rights behind it.
    #[cfg(test)]
    pub fn lease_acquire(
        &self,
        id: &str,
        member_id: &str,
        device_id: &str,
        device_name: &str,
        ttl_secs: u64,
    ) -> Result<std::result::Result<Lease, Lease>> {
        let mut db = self.lock();
        let tx = db.transaction()?;
        let r = acquire_in(&tx, id, member_id, device_id, device_name, ttl_secs)?;
        tx.commit()?;
        Ok(r.map(|t| t.lease))
    }

    /// Open a profile on a device, or renew that device's lease; busy answers
    /// who has it and where. Space and `authorize` are checked in the same transaction.
    #[allow(clippy::too_many_arguments)]
    pub fn lease_take(
        &self,
        space: &str,
        id: &str,
        member_id: &str,
        device_id: &str,
        device_name: &str,
        ttl_secs: u64,
        authorize: impl FnOnce(&Standing, &TeamProfile) -> std::result::Result<(), Refusal>,
    ) -> Result<std::result::Result<Taken, Refusal>> {
        let mut db = self.lock();
        let tx = db.transaction()?;
        let p = match profile_with_space(&tx, id)? {
            None => return Ok(Err(Refusal::Missing)),
            Some((_, sp)) if sp != space => return Ok(Err(Refusal::OtherSpace)),
            Some((p, _)) => p,
        };
        if let Err(r) = authorize(&standing_in(&tx, member_id)?, &p) {
            return Ok(Err(r));
        }
        let r = acquire_in(&tx, id, member_id, device_id, device_name, ttl_secs)?;
        tx.commit()?;
        Ok(r.map_err(Refusal::Leased))
    }

    /// As [`Store::lease_take`], but a live lease of the same member on another
    /// device is ended first: one of their own devices takes the profile over.
    #[allow(clippy::too_many_arguments)]
    pub fn lease_take_over(
        &self,
        space: &str,
        id: &str,
        member_id: &str,
        device_id: &str,
        device_name: &str,
        ttl_secs: u64,
        authorize: impl FnOnce(&Standing, &TeamProfile) -> std::result::Result<(), Refusal>,
    ) -> Result<std::result::Result<Taken, Refusal>> {
        let mut db = self.lock();
        let tx = db.transaction()?;
        let p = match profile_with_space(&tx, id)? {
            None => return Ok(Err(Refusal::Missing)),
            Some((_, sp)) if sp != space => return Ok(Err(Refusal::OtherSpace)),
            Some((p, _)) => p,
        };
        if let Err(r) = authorize(&standing_in(&tx, member_id)?, &p) {
            return Ok(Err(r));
        }
        tx.execute(
            "DELETE FROM leases WHERE item_id = ?1 AND member_id = ?2 AND device_id <> ?3",
            params![id, member_id, device_id],
        )?;
        let r = acquire_in(&tx, id, member_id, device_id, device_name, ttl_secs)?;
        tx.commit()?;
        Ok(r.map_err(Refusal::Leased))
    }

    /// Leases that ran out unrenewed, taken off the books so each is told once.
    pub fn reap_leases(&self) -> Result<Vec<(String, Lease)>> {
        let mut db = self.lock();
        let tx = db.transaction()?;
        let t = lease_now() as i64;
        let gone = {
            let mut q = tx.prepare(
                "SELECT item_id, member_id, device_id, device_name, acquired_at, expires_at
                 FROM leases WHERE expires_at <= ?1",
            )?;
            let rows = q
                .query_map(params![t], |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        Lease {
                            member_id: r.get(1)?,
                            device_id: r.get(2)?,
                            device_name: r.get(3)?,
                            acquired_at: r.get::<_, i64>(4)? as u64,
                            expires_at: r.get::<_, i64>(5)? as u64,
                        },
                    ))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            rows
        };
        tx.execute("DELETE FROM leases WHERE expires_at <= ?1", params![t])?;
        tx.commit()?;
        Ok(gone)
    }

    /// Keep a live lease alive while its device is busy with the profile, as a
    /// renewal would. False when that device no longer holds one.
    pub fn lease_extend(&self, id: &str, member_id: &str, device_id: &str, ttl_secs: u64) -> Result<bool> {
        let db = self.lock();
        let t = lease_now();
        Ok(db.execute(
            "UPDATE leases SET expires_at = MAX(expires_at, ?5)
             WHERE item_id = ?1 AND member_id = ?2 AND device_id = ?3 AND expires_at > ?4",
            params![id, member_id, device_id, t as i64, (t + ttl_secs) as i64],
        )? > 0)
    }

    /// The server is up again: every lease gets `ttl_secs` from now, so a device
    /// that kept its profile open while the server was down renews it before
    /// anyone else can take it.
    pub fn leases_resume(&self, ttl_secs: u64) -> Result<usize> {
        let db = self.lock();
        let until = (lease_now() + ttl_secs) as i64;
        Ok(db.execute("UPDATE leases SET expires_at = MAX(expires_at, ?1)", params![until])?)
    }

    /// Close: only the device that holds the lease can give it up.
    pub fn lease_release(&self, id: &str, member_id: &str, device_id: &str) -> Result<bool> {
        let db = self.lock();
        Ok(db.execute(
            "DELETE FROM leases WHERE item_id = ?1 AND member_id = ?2 AND device_id = ?3",
            params![id, member_id, device_id],
        )? > 0)
    }

    /// Test setup: a write with no card, judged by `authorize` on the row alone.
    #[cfg(test)]
    #[allow(clippy::too_many_arguments)]
    pub fn put_profile_checked(
        &self,
        space: &str,
        id: &str,
        name: &str,
        folder: &str,
        data: &[u8],
        writer: Writer<'_>,
        base_revision: Option<u64>,
        authorize: impl FnOnce(&Standing, Option<&TeamProfile>) -> std::result::Result<(), Refusal>,
    ) -> Result<std::result::Result<TeamProfile, Refusal>> {
        let fields = ProfileFields { name, folder, data, card: None, created_at: None };
        self.put_profile_with(space, id, &fields, writer, base_revision, |s, e, _| authorize(s, e))
    }

    /// Write a profile into a space, refusing when that would overwrite
    /// someone: another device has it open, or it has changed since the writer
    /// read it (`base_revision`), or the id already lives in another space.
    /// `authorize` is the caller's rights check, handed the writer's standing,
    /// the row as it is here (None for a new one) and what else it [`Found`];
    /// it runs under the store's lock, so no store calls in it. A newly
    /// bound proxy must also be in the profile's own space.
    /// All of it is checked and the write made in one transaction, so nothing
    /// can slip in between. No card keeps the stored one.
    pub fn put_profile_with(
        &self,
        space: &str,
        id: &str,
        fields: &ProfileFields<'_>,
        writer: Writer<'_>,
        base_revision: Option<u64>,
        authorize: impl FnOnce(&Standing, Option<&TeamProfile>, Found<'_>) -> std::result::Result<(), Refusal>,
    ) -> Result<std::result::Result<TeamProfile, Refusal>> {
        let ProfileFields { name, folder, data, card, created_at } = *fields;
        let sealed = self.vault.seal(&format!("profile:{id}"), data)?;
        let sealed_card = card
            .map(|c| self.vault.seal(&card_aad(id), &serde_json::to_vec(c)?))
            .transpose()?;
        let mut db = self.lock();
        let tx = db.transaction()?;
        let existing = match profile_with_space(&tx, id)? {
            Some((_, sp)) if sp != space => return Ok(Err(Refusal::OtherSpace)),
            row => row.map(|(p, _)| p),
        };
        // One id, one item: a rule naming a profile must never reach a proxy,
        // and an id in the trash is kept for the item that may come back.
        if existing.is_none() {
            if proxy_with_space(&tx, id)?.is_some() {
                return Ok(Err(Refusal::OtherSpace));
            }
            if let Some(t) = trashed(&tx, id)? {
                return Ok(Err(Refusal::InTrash(Box::new(t))));
            }
        }
        // A binding the row already has is not asked about again.
        let bound = match card.and_then(Card::bound_proxy) {
            Some(x) if existing.is_some() && self.card_in(&tx, id)?.bound_proxy() == Some(x) => None,
            other => other,
        };
        let named = existing.is_none() && space == TEAM && named_by_grant(&tx, id)?;
        // Rights first: the lease and the revision are only for someone who may write it.
        let found = Found { bound, named };
        if let Err(r) = authorize(&standing_in(&tx, writer.member_id)?, existing.as_ref(), found) {
            return Ok(Err(r));
        }
        if let Some(x) = bound {
            if !matches!(proxy_with_space(&tx, x)?, Some((_, sp)) if sp == space) {
                return Ok(Err(Refusal::BadProxy));
            }
        }
        if let Err(r) = check_base(base_revision, existing.as_ref().map_or(0, |p| p.revision)) {
            return Ok(Err(r));
        }
        if let Some(held) = live_lease(&tx, id)? {
            if !writer.holds(&held) {
                return Ok(Err(Refusal::Leased(held)));
            }
        }
        // Made in the team, or moved to another folder there: who sees what may move.
        let scopes = match &existing {
            _ if space != TEAM => Vec::new(),
            None => touched(&[folder], &[id]),
            Some(p) if p.folder != folder => touched(&[&p.folder, folder], &[id]),
            Some(_) => Vec::new(),
        };
        let watch = Watch::start(&tx, scopes)?;
        if !folder.is_empty() {
            tx.execute(
                "INSERT OR IGNORE INTO folders(space, name, created_at) VALUES (?1, ?2, ?3)",
                params![space, folder, now() as i64],
            )?;
        }
        let ts = now();
        // A new one is dated now unless the writer knows better; an old one keeps its date,
        // and one with none takes only the writer's.
        let made = made_at(created_at, ts).or(existing.is_none().then_some(ts));
        tx.execute(
            "INSERT INTO profiles(id, space, name, folder, revision, updated_at, updated_by, sealed, card, created_at)
             VALUES (?1, ?2, ?3, ?4, 1, ?5, ?6, ?7, ?8, ?9)
             ON CONFLICT(id) DO UPDATE SET
                name = excluded.name, folder = excluded.folder,
                revision = profiles.revision + 1, updated_at = excluded.updated_at,
                updated_by = excluded.updated_by, sealed = excluded.sealed,
                card = COALESCE(excluded.card, profiles.card),
                created_at = COALESCE(profiles.created_at, excluded.created_at)",
            params![id, space, name, folder, ts as i64, writer.member_id, &sealed, sealed_card, made.map(|t| t as i64)],
        )?;
        let row = profile_by_id(&tx, id)?;
        let moved = watch.finish(&tx)?;
        self.commit_regranted(tx, moved)?;
        Ok(Ok(row))
    }

    /// Test setup: a proxy write judged on the row alone.
    #[cfg(test)]
    #[allow(clippy::too_many_arguments)]
    pub fn put_proxy_checked(
        &self,
        space: &str,
        id: &str,
        name: &str,
        kind: &str,
        country: &str,
        data: &[u8],
        writer: Writer<'_>,
        base_revision: Option<u64>,
        authorize: impl FnOnce(&Standing, Option<&TeamProxy>) -> std::result::Result<(), Refusal>,
    ) -> Result<std::result::Result<TeamProxy, Refusal>> {
        let fields = ProxyFields { name, kind, country, data };
        self.put_proxy_with(space, id, &fields, writer, base_revision, |s, e, _| authorize(s, e))
    }

    /// The same guard for proxies, minus the lease: nothing has a proxy open.
    /// A new team proxy is [`Found::named`] when a rule or a team card names its id.
    pub fn put_proxy_with(
        &self,
        space: &str,
        id: &str,
        fields: &ProxyFields<'_>,
        writer: Writer<'_>,
        base_revision: Option<u64>,
        authorize: impl FnOnce(&Standing, Option<&TeamProxy>, Found<'_>) -> std::result::Result<(), Refusal>,
    ) -> Result<std::result::Result<TeamProxy, Refusal>> {
        let ProxyFields { name, kind, country, data } = *fields;
        let sealed = self.vault.seal(&format!("proxy:{id}"), data)?;
        let mut db = self.lock();
        let tx = db.transaction()?;
        let existing = match proxy_with_space(&tx, id)? {
            Some((_, sp)) if sp != space => return Ok(Err(Refusal::OtherSpace)),
            row => row.map(|(x, _)| x),
        };
        if existing.is_none() {
            if profile_with_space(&tx, id)?.is_some() {
                return Ok(Err(Refusal::OtherSpace));
            }
            if let Some(t) = trashed(&tx, id)? {
                return Ok(Err(Refusal::InTrash(Box::new(t))));
            }
        }
        let named = existing.is_none() && space == TEAM && self.team_names_proxy(&tx, id)?;
        let found = Found { bound: None, named };
        if let Err(r) = authorize(&standing_in(&tx, writer.member_id)?, existing.as_ref(), found) {
            return Ok(Err(r));
        }
        if let Err(r) = check_base(base_revision, existing.as_ref().map_or(0, |x| x.revision)) {
            return Ok(Err(r));
        }
        let new_in_team = existing.is_none() && space == TEAM;
        let watch = Watch::start(&tx, if new_in_team { touched(&[], &[id]) } else { Vec::new() })?;
        let ts = now();
        tx.execute(
            "INSERT INTO proxies(id, space, name, kind, country, revision, updated_at, updated_by, sealed)
             VALUES (?1, ?2, ?3, ?4, ?5, 1, ?6, ?7, ?8)
             ON CONFLICT(id) DO UPDATE SET
                name = excluded.name, kind = excluded.kind, country = excluded.country,
                revision = proxies.revision + 1, updated_at = excluded.updated_at,
                updated_by = excluded.updated_by, sealed = excluded.sealed",
            params![id, space, name, kind, country, ts as i64, writer.member_id, &sealed],
        )?;
        let revision: i64 =
            tx.query_row("SELECT revision FROM proxies WHERE id = ?1", params![id], |r| r.get(0))?;
        let moved = watch.finish(&tx)?;
        self.commit_regranted(tx, moved)?;
        Ok(Ok(TeamProxy {
            id: id.into(),
            name: name.into(),
            kind: kind.into(),
            country: country.into(),
            revision: revision as u64,
            updated_at: ts,
            updated_by: writer.member_id.into(),
        }))
    }

    /// Move a profile between spaces — a member's own into the team, or out of
    /// it into theirs. Atomic, and refused while another device has it open:
    /// moving it out from under a running browser would leave that browser
    /// writing somewhere the profile no longer is. `authorize` is judged on the
    /// row as it is here, as for a write. Into the team its binding is judged as
    /// a new one there; out of it, a team binding stays but opens nothing.
    pub fn move_profile(
        &self,
        id: &str,
        from: &str,
        to: &str,
        folder: &str,
        writer: Writer<'_>,
        authorize: impl FnOnce(&Standing, &TeamProfile, Found<'_>) -> std::result::Result<(), Refusal>,
    ) -> Result<std::result::Result<TeamProfile, Refusal>> {
        let mut db = self.lock();
        let tx = db.transaction()?;
        let p = match profile_with_space(&tx, id)? {
            None => return Ok(Err(Refusal::Missing)),
            Some((_, sp)) if sp != from => return Ok(Err(Refusal::OtherSpace)),
            Some((p, _)) => p,
        };
        let card = self.card_in(&tx, id)?;
        let found = if to == TEAM {
            Found { bound: card.bound_proxy(), named: named_by_grant(&tx, id)? }
        } else {
            Found::default()
        };
        if let Err(r) = authorize(&standing_in(&tx, writer.member_id)?, &p, found) {
            return Ok(Err(r));
        }
        if let Some(x) = found.bound {
            if !matches!(proxy_with_space(&tx, x)?, Some((_, sp)) if sp == to) {
                return Ok(Err(Refusal::BadProxy));
            }
        }
        if let Some(held) = live_lease(&tx, id)? {
            if !writer.holds(&held) {
                return Ok(Err(Refusal::Leased(held)));
            }
        }
        let was = if from == TEAM { p.folder.as_str() } else { "" };
        let lands = if to == TEAM { folder } else { "" };
        let in_team = from == TEAM || to == TEAM;
        let watch = Watch::start(&tx, if in_team { touched(&[was, lands], &[id]) } else { Vec::new() })?;
        if !folder.is_empty() {
            tx.execute(
                "INSERT OR IGNORE INTO folders(space, name, created_at) VALUES (?1, ?2, ?3)",
                params![to, folder, now() as i64],
            )?;
        }
        let ts = now();
        tx.execute(
            "UPDATE profiles SET space = ?2, folder = ?3, revision = revision + 1,
                                 updated_at = ?4, updated_by = ?5
             WHERE id = ?1",
            params![id, to, folder, ts as i64, writer.member_id],
        )?;
        let row = profile_by_id(&tx, id)?;
        let moved = watch.finish(&tx)?;
        self.commit_regranted(tx, moved)?;
        Ok(Ok(row))
    }

    /// Move a proxy between spaces. Into the team, [`Found::named`] says the
    /// team already names its id, as for a new one written there.
    pub fn move_proxy(
        &self,
        id: &str,
        from: &str,
        to: &str,
        writer: Writer<'_>,
        authorize: impl FnOnce(&Standing, &TeamProxy, Found<'_>) -> std::result::Result<(), Refusal>,
    ) -> Result<std::result::Result<TeamProxy, Refusal>> {
        let mut db = self.lock();
        let tx = db.transaction()?;
        let x = match proxy_with_space(&tx, id)? {
            None => return Ok(Err(Refusal::Missing)),
            Some((_, sp)) if sp != from => return Ok(Err(Refusal::OtherSpace)),
            Some((x, _)) => x,
        };
        let found = Found { bound: None, named: to == TEAM && self.team_names_proxy(&tx, id)? };
        if let Err(r) = authorize(&standing_in(&tx, writer.member_id)?, &x, found) {
            return Ok(Err(r));
        }
        let in_team = from == TEAM || to == TEAM;
        let watch = Watch::start(&tx, if in_team { touched(&[], &[id]) } else { Vec::new() })?;
        let ts = now();
        tx.execute(
            "UPDATE proxies SET space = ?2, revision = revision + 1, updated_at = ?3, updated_by = ?4
             WHERE id = ?1",
            params![id, to, ts as i64, writer.member_id],
        )?;
        let row = tx.query_row(
            "SELECT id, name, kind, country, revision, updated_at, updated_by FROM proxies WHERE id = ?1",
            params![id],
            proxy_row,
        )?;
        let moved = watch.finish(&tx)?;
        self.commit_regranted(tx, moved)?;
        Ok(Ok(row))
    }

    /// Delete from a space with the same guard as a write: `authorize`, then
    /// `base_revision`, then the lease, all in one transaction with the delete.
    /// From the team it goes to the trash; from a member's own space it is gone.
    pub fn delete_profile_checked(
        &self,
        space: &str,
        id: &str,
        writer: Writer<'_>,
        base_revision: Option<u64>,
        authorize: impl FnOnce(&Standing, &TeamProfile) -> std::result::Result<(), Refusal>,
    ) -> Result<std::result::Result<(), Refusal>> {
        let mut db = self.lock();
        let tx = db.transaction()?;
        let p = match profile_with_space(&tx, id)? {
            None => return Ok(Err(Refusal::Missing)),
            Some((_, sp)) if sp != space => return Ok(Err(Refusal::OtherSpace)),
            Some((p, _)) => p,
        };
        let who = standing_in(&tx, writer.member_id)?;
        if let Err(r) = authorize(&who, &p).and_then(|()| check_base(base_revision, p.revision)) {
            return Ok(Err(r));
        }
        if let Some(held) = live_lease(&tx, id)? {
            if !writer.holds(&held) {
                return Ok(Err(Refusal::Leased(held)));
            }
        }
        let in_team = space == TEAM;
        let watch = Watch::start(&tx, if in_team { touched(&[&p.folder], &[id]) } else { Vec::new() })?;
        if space == TEAM {
            // The session stays under the id the trash keeps, for a restore.
            tx.execute(
                "INSERT INTO trash(id, kind, name, folder, revision, updated_at, updated_by,
                                   deleted_at, deleted_by, sealed, card, created_at)
                 SELECT id, 'profile', name, folder, revision, updated_at, updated_by, ?2, ?3, sealed, card,
                        created_at
                 FROM profiles WHERE id = ?1",
                params![id, now() as i64, writer.member_id],
            )?;
        } else {
            blob_drop(&tx, SESSION, "", id)?;
        }
        tx.execute("DELETE FROM profiles WHERE id = ?1", params![id])?;
        tx.execute("DELETE FROM leases WHERE item_id = ?1", params![id])?;
        let moved = watch.finish(&tx)?;
        self.commit_regranted(tx, moved)?;
        Ok(Ok(()))
    }

    /// The same for a proxy, minus the lease: nothing opens a proxy, and a lease
    /// under the same id would be some profile's.
    pub fn delete_proxy_checked(
        &self,
        space: &str,
        id: &str,
        writer: Writer<'_>,
        base_revision: Option<u64>,
        authorize: impl FnOnce(&Standing, &TeamProxy) -> std::result::Result<(), Refusal>,
    ) -> Result<std::result::Result<(), Refusal>> {
        let mut db = self.lock();
        let tx = db.transaction()?;
        let x = match proxy_with_space(&tx, id)? {
            None => return Ok(Err(Refusal::Missing)),
            Some((_, sp)) if sp != space => return Ok(Err(Refusal::OtherSpace)),
            Some((x, _)) => x,
        };
        let who = standing_in(&tx, writer.member_id)?;
        if let Err(r) = authorize(&who, &x).and_then(|()| check_base(base_revision, x.revision)) {
            return Ok(Err(r));
        }
        let watch = Watch::start(&tx, if space == TEAM { touched(&[], &[id]) } else { Vec::new() })?;
        if space == TEAM {
            tx.execute(
                "INSERT INTO trash(id, kind, name, proxy_kind, country, revision, updated_at,
                                   updated_by, deleted_at, deleted_by, sealed)
                 SELECT id, 'proxy', name, kind, country, revision, updated_at, updated_by, ?2, ?3, sealed
                 FROM proxies WHERE id = ?1",
                params![id, now() as i64, writer.member_id],
            )?;
        }
        tx.execute("DELETE FROM proxies WHERE id = ?1", params![id])?;
        let moved = watch.finish(&tx)?;
        self.commit_regranted(tx, moved)?;
        Ok(Ok(()))
    }

    // ---- the team's trash ----

    /// Everything in the team's trash, the most recently deleted first.
    pub fn trash(&self) -> Result<Vec<TrashItem>> {
        let db = self.lock();
        let mut q = db.prepare(&format!("SELECT {TRASH_COLUMNS} FROM trash"))?;
        let mut rows = q.query_map([], trash_row)?.collect::<rusqlite::Result<Vec<_>>>()?;
        rows.extend(trashed_items(&db, u64::MAX)?);
        rows.sort_by(|a, b| b.deleted_at.cmp(&a.deleted_at).then_with(|| a.id.cmp(&b.id)));
        Ok(rows)
    }

    /// Put a trashed item back where it was, at a new revision so older copies read
    /// as stale. `authorize` is judged on the trash row, under the same lock.
    pub fn restore_from_trash(
        &self,
        id: &str,
        writer: Writer<'_>,
        authorize: impl FnOnce(&Standing, &TrashItem) -> std::result::Result<(), Refusal>,
    ) -> Result<std::result::Result<Restored, Refusal>> {
        let mut db = self.lock();
        let tx = db.transaction()?;
        let Some(t) = trashed(&tx, id)? else {
            return Ok(Err(Refusal::Missing));
        };
        if let Err(r) = authorize(&standing_in(&tx, writer.member_id)?, &t) {
            return Ok(Err(r));
        }
        // Writes keep trashed ids free, so this only guards against an older database.
        if profile_with_space(&tx, id)?.is_some() || proxy_with_space(&tx, id)?.is_some() {
            return Ok(Err(Refusal::OtherSpace));
        }
        let watch = Watch::start(&tx, touched(&[&t.folder], &[id]))?;
        let ts = now();
        let restored = match t.kind {
            TrashKind::Profile => {
                if !t.folder.is_empty() {
                    tx.execute(
                        "INSERT OR IGNORE INTO folders(space, name, created_at) VALUES (?1, ?2, ?3)",
                        params![TEAM, t.folder, ts as i64],
                    )?;
                }
                tx.execute(
                    "INSERT INTO profiles(id, space, name, folder, revision, updated_at, updated_by, sealed, card,
                                          created_at)
                     SELECT id, ?2, name, folder, revision + 1, ?3, ?4, sealed, card, created_at
                     FROM trash WHERE id = ?1",
                    params![id, TEAM, ts as i64, writer.member_id],
                )?;
                Restored::Profile(profile_by_id(&tx, id)?)
            }
            // The trash table holds profiles and proxies only; items wait in their own.
            TrashKind::Bookmark | TrashKind::Extension | TrashKind::Automation | TrashKind::Tag => {
                return Ok(Err(Refusal::Missing))
            }
            TrashKind::Proxy => {
                tx.execute(
                    "INSERT INTO proxies(id, space, name, kind, country, revision, updated_at, updated_by, sealed)
                     SELECT id, ?2, name, proxy_kind, country, revision + 1, ?3, ?4, sealed
                     FROM trash WHERE id = ?1",
                    params![id, TEAM, ts as i64, writer.member_id],
                )?;
                Restored::Proxy(tx.query_row(
                    "SELECT id, name, kind, country, revision, updated_at, updated_by FROM proxies WHERE id = ?1",
                    params![id],
                    proxy_row,
                )?)
            }
        };
        tx.execute("DELETE FROM trash WHERE id = ?1", params![id])?;
        let moved = watch.finish(&tx)?;
        self.commit_regranted(tx, moved)?;
        Ok(Ok(restored))
    }

    /// Remove a trashed item for good, judged like a restore.
    pub fn purge_from_trash(
        &self,
        id: &str,
        member_id: &str,
        authorize: impl FnOnce(&Standing, &TrashItem) -> std::result::Result<(), Refusal>,
    ) -> Result<std::result::Result<TrashItem, Refusal>> {
        let mut db = self.lock();
        let tx = db.transaction()?;
        let Some(t) = trashed(&tx, id)? else {
            return Ok(Err(Refusal::Missing));
        };
        if let Err(r) = authorize(&standing_in(&tx, member_id)?, &t) {
            return Ok(Err(r));
        }
        let watch = Watch::start(&tx, touched(&[&t.folder], &[id]))?;
        tx.execute("DELETE FROM trash WHERE id = ?1", params![id])?;
        if t.kind == TrashKind::Profile {
            blob_drop(&tx, SESSION, "", id)?;
        }
        let moved = watch.finish(&tx)?;
        self.commit_regranted(tx, moved)?;
        Ok(Ok(t))
    }

    /// Purge what was deleted at or before `cutoff`, and answer what went.
    pub fn expire_trash(&self, cutoff: u64) -> Result<Vec<TrashItem>> {
        let mut db = self.lock();
        let tx = db.transaction()?;
        let mut gone = trash_before(&tx, cutoff)?;
        let folders: Vec<&str> = gone.iter().map(|t| t.folder.as_str()).collect();
        let ids: Vec<&str> = gone.iter().map(|t| t.id.as_str()).collect();
        let watch = Watch::start(&tx, touched(&folders, &ids))?;
        let cut = cutoff.min(i64::MAX as u64) as i64;
        tx.execute("DELETE FROM trash WHERE deleted_at <= ?1", params![cut])?;
        gone.extend(trashed_items(&tx, cutoff)?);
        tx.execute(
            "DELETE FROM items WHERE space = 'team' AND deleted_at IS NOT NULL AND deleted_at <= ?1",
            params![cut],
        )?;
        for t in &gone {
            match t.kind {
                TrashKind::Profile => blob_drop(&tx, SESSION, "", &t.id)?,
                TrashKind::Extension => blob_drop(&tx, PACKAGE, TEAM, &t.id)?,
                _ => {}
            }
        }
        let moved = watch.finish(&tx)?;
        self.commit_regranted(tx, moved)?;
        Ok(gone)
    }

    /// Test setup: pretend an item was deleted at `when`.
    #[cfg(test)]
    pub fn backdate_trash(&self, id: &str, when: u64) -> Result<()> {
        let db = self.lock();
        db.execute("UPDATE trash SET deleted_at = ?2 WHERE id = ?1", params![id, when as i64])?;
        db.execute(
            "UPDATE items SET deleted_at = ?2 WHERE id = ?1 AND deleted_at IS NOT NULL",
            params![id, when as i64],
        )?;
        Ok(())
    }

    // ---- the team's own proxies ----

    /// Test setup: an unguarded write into the team.
    #[cfg(test)]
    pub fn put_proxy(
        &self,
        id: &str,
        name: &str,
        kind: &str,
        country: &str,
        data: &[u8],
        by: &str,
    ) -> Result<TeamProxy> {
        let sealed = self.vault.seal(&format!("proxy:{id}"), data)?;
        let db = self.lock();
        let ts = now();
        db.execute(
            "INSERT INTO proxies(id, name, kind, country, revision, updated_at, updated_by, sealed)
             VALUES (?1, ?2, ?3, ?4, 1, ?5, ?6, ?7)
             ON CONFLICT(id) DO UPDATE SET
                name = excluded.name,
                kind = excluded.kind,
                country = excluded.country,
                revision = proxies.revision + 1,
                updated_at = excluded.updated_at,
                updated_by = excluded.updated_by,
                sealed = excluded.sealed",
            params![id, name, kind, country, ts as i64, by, &sealed],
        )?;
        let revision: i64 =
            db.query_row("SELECT revision FROM proxies WHERE id = ?1", params![id], |r| r.get(0))?;
        Ok(TeamProxy {
            id: id.into(),
            name: name.into(),
            kind: kind.into(),
            country: country.into(),
            revision: revision as u64,
            updated_at: ts,
            updated_by: by.into(),
        })
    }

    pub fn proxies(&self) -> Result<Vec<TeamProxy>> {
        let db = self.lock();
        let mut q = db.prepare(
            "SELECT id, name, kind, country, revision, updated_at, updated_by FROM proxies
             WHERE space = 'team' ORDER BY name",
        )?;
        let rows = q
            .query_map([], |r| {
                Ok(TeamProxy {
                    id: r.get(0)?,
                    name: r.get(1)?,
                    kind: r.get(2)?,
                    country: r.get(3)?,
                    revision: r.get::<_, i64>(4)? as u64,
                    updated_at: r.get::<_, i64>(5)? as u64,
                    updated_by: r.get(6)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// The proxy's data, opened. None when there is no such proxy.
    pub fn proxy_data(&self, id: &str) -> Result<Option<Vec<u8>>> {
        let sealed: Option<Vec<u8>> = {
            let db = self.lock();
            db.query_row("SELECT sealed FROM proxies WHERE id = ?1", params![id], |r| r.get(0))
                .optional()?
        };
        sealed.map(|b| self.vault.open(&format!("proxy:{id}"), &b)).transpose()
    }

    // ---- sessions ----

    /// A profile's session, for the device that has it open: what was found
    /// beside it, and its body, held until the caller lets go.
    pub fn session_open(
        &self,
        space: &str,
        id: &str,
        reader: Writer<'_>,
        authorize: impl FnOnce(&Standing, &TeamProfile) -> std::result::Result<(), Refusal>,
    ) -> Result<std::result::Result<(SessionHead, Option<BodyRef>), Refusal>> {
        let mut db = self.lock();
        let tx = db.transaction()?;
        let p = match profile_with_space(&tx, id)? {
            None => return Ok(Err(Refusal::Missing)),
            Some((_, sp)) if sp != space => return Ok(Err(Refusal::OtherSpace)),
            Some((p, _)) => p,
        };
        if let Err(r) = authorize(&standing_in(&tx, reader.member_id)?, &p) {
            return Ok(Err(r));
        }
        match live_lease(&tx, id)? {
            Some(held) if reader.holds(&held) => {}
            Some(held) => return Ok(Err(Refusal::Leased(held))),
            None => return Ok(Err(Refusal::NoLease)),
        }
        // The bound proxy comes with it, but only from the profile's own space.
        let card = self.card_in(&tx, id)?;
        let proxy = match card.bound_proxy() {
            Some(x) => match proxy_with_space(&tx, x)? {
                Some((row, sp)) if sp == space => {
                    let sealed: Vec<u8> =
                        tx.query_row("SELECT sealed FROM proxies WHERE id = ?1", params![x], |r| r.get(0))?;
                    Some((row, self.vault.open(&format!("proxy:{x}"), &sealed)?))
                }
                _ => None,
            },
            None => None,
        };
        let blob = blob_at(&tx, SESSION, "", id)?;
        let found = SessionHead {
            session: blob.as_ref().map(|(m, _)| m.clone()),
            proxy_id: card.bound_proxy().map(str::to_owned),
            proxy,
        };
        let body = blob.map(|(m, body)| BodyRef { body, size: m.size, aad: session_aad(id) });
        if let Some(b) = &body {
            self.hold(b.body);
        }
        Ok(Ok((found, body)))
    }

    /// A new upload of a profile's session.
    pub fn session_upload(&self, id: &str) -> Result<Upload> {
        self.body_begin(session_aad(id))
    }

    /// Make a finished upload the session, for the device holding the lease and
    /// based on `base` (0 before the first). `authorize` is told if it holds it.
    pub fn session_commit(
        &self,
        space: &str,
        id: &str,
        writer: Writer<'_>,
        base: u64,
        up: &Upload,
        authorize: impl FnOnce(&Standing, &TeamProfile, bool) -> std::result::Result<(), Refusal>,
    ) -> Result<std::result::Result<(TeamProfile, BlobMeta), Refusal>> {
        if !up.done || up.aad != session_aad(id) {
            bail!("{id}: not a finished upload of this session");
        }
        let mut db = self.lock();
        let tx = db.transaction()?;
        let p = match profile_with_space(&tx, id)? {
            None => return Ok(Err(Refusal::Missing)),
            Some((_, sp)) if sp != space => return Ok(Err(Refusal::OtherSpace)),
            Some((p, _)) => p,
        };
        let lease = live_lease(&tx, id)?;
        let holds = lease.as_ref().is_some_and(|l| writer.holds(l));
        if let Err(r) = authorize(&standing_in(&tx, writer.member_id)?, &p, holds) {
            return Ok(Err(r));
        }
        if !holds {
            return Ok(Err(lease.map_or(Refusal::NoLease, Refusal::Leased)));
        }
        let current = blob_at(&tx, SESSION, "", id)?.map_or(0, |(m, _)| m.rev);
        if base != current {
            return Ok(Err(Refusal::SessionStale { current }));
        }
        let meta = head_put(&tx, SESSION, "", id, current + 1, writer.member_id, up)?;
        tx.commit()?;
        Ok(Ok((p, meta)))
    }

    /// Test setup: a whole session uploaded and judged in one call.
    #[cfg(test)]
    pub fn session_write(
        &self,
        space: &str,
        id: &str,
        writer: Writer<'_>,
        base: u64,
        data: Vec<u8>,
        authorize: impl FnOnce(&Standing, &TeamProfile, bool) -> std::result::Result<(), Refusal>,
    ) -> Result<std::result::Result<(TeamProfile, BlobMeta), Refusal>> {
        let mut up = self.session_upload(id)?;
        self.upload_all(&mut up, &data)?;
        let r = self.session_commit(space, id, writer, base, &up, authorize);
        self.let_go(up.body);
        r
    }

    /// Test check: `head(found)` and then the session's bytes, in one buffer.
    #[cfg(test)]
    pub fn session_read(
        &self,
        space: &str,
        id: &str,
        reader: Writer<'_>,
        authorize: impl FnOnce(&Standing, &TeamProfile) -> std::result::Result<(), Refusal>,
        head: impl FnOnce(&SessionHead) -> Result<Vec<u8>>,
    ) -> Result<std::result::Result<Vec<u8>, Refusal>> {
        let (found, body) = match self.session_open(space, id, reader, authorize)? {
            Ok(x) => x,
            Err(r) => return Ok(Err(r)),
        };
        let mut buf = head(&found)?;
        if let Some(b) = body {
            let read = self.read_body(&b, &mut buf);
            self.let_go(b.body);
            read?;
        }
        Ok(Ok(buf))
    }

    /// Test check: the session as stored, still sealed, chunk after chunk.
    #[cfg(test)]
    pub fn session_sealed(&self, id: &str) -> Result<Option<Vec<u8>>> {
        let db = self.lock();
        let Some((_, body)) = blob_at(&db, SESSION, "", id)? else { return Ok(None) };
        let mut q = db.prepare("SELECT sealed FROM chunks WHERE body = ?1 ORDER BY seq")?;
        let parts = q.query_map(params![body], |r| r.get::<_, Vec<u8>>(0))?.collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(Some(parts.concat()))
    }

    // ---- bodies: a blob's bytes, a chunk at a time ----

    /// A new body for an upload, held from the start so nothing collects it half-written.
    fn body_begin(&self, aad: String) -> Result<Upload> {
        let db = self.lock();
        db.execute("INSERT INTO bodies(created_at) VALUES (?1)", params![now() as i64])?;
        let body = db.last_insert_rowid();
        self.hold(body);
        Ok(Upload { body, aad, next: 0, size: 0, done: false })
    }

    /// Seal and keep an upload's next chunk: whole [`CHUNK`]s, then the last
    /// with `last` set — at most a CHUNK, and empty only when the blob is.
    pub fn upload_put(&self, up: &mut Upload, plain: &[u8], last: bool) -> Result<()> {
        let fits = if last { plain.len() <= CHUNK && (up.next == 0 || !plain.is_empty()) } else { plain.len() == CHUNK };
        if up.done || !fits {
            bail!("a chunk of {} bytes does not go there", plain.len());
        }
        let sealed = self.vault.seal(&chunk_aad(&up.aad, up.body, up.next, last), plain)?;
        self.lock().execute(
            "INSERT INTO chunks(body, seq, sealed) VALUES (?1, ?2, ?3)",
            params![up.body, up.next as i64, sealed],
        )?;
        up.next += 1;
        up.size += plain.len() as u64;
        up.done = last;
        Ok(())
    }

    /// Test setup: a whole blob into an upload, chunk by chunk.
    #[cfg(test)]
    fn upload_all(&self, up: &mut Upload, data: &[u8]) -> Result<()> {
        let mut rest = data;
        while rest.len() > CHUNK {
            self.upload_put(up, &rest[..CHUNK], false)?;
            rest = &rest[CHUNK..];
        }
        self.upload_put(up, rest, true)
    }

    /// One chunk of a stored body, opened, and held to the length it must have.
    pub fn body_chunk(&self, b: &BodyRef, seq: u64) -> Result<Vec<u8>> {
        let sealed: Vec<u8> = self
            .lock()
            .query_row(
                "SELECT sealed FROM chunks WHERE body = ?1 AND seq = ?2",
                params![b.body, seq as i64],
                |r| r.get(0),
            )
            .optional()?
            .with_context(|| format!("{}: chunk {seq} is missing", b.aad))?;
        let n = b.chunks();
        let last = seq + 1 == n;
        let plain = self.vault.open(&chunk_aad(&b.aad, b.body, seq, last), &sealed)?;
        let want = if last { b.size - (n - 1) * CHUNK as u64 } else { CHUNK as u64 };
        if plain.len() as u64 != want {
            bail!("{}: chunk {seq} is {} bytes, not {want}", b.aad, plain.len());
        }
        Ok(plain)
    }

    /// Test check: a stored body's bytes appended to `buf`.
    #[cfg(test)]
    pub fn read_body(&self, b: &BodyRef, buf: &mut Vec<u8>) -> Result<()> {
        for seq in 0..b.chunks() {
            buf.extend_from_slice(&self.body_chunk(b, seq)?);
        }
        Ok(())
    }

    /// One more holder of a body. Called with the database held, so the
    /// collector, which reads this under the same hold, never sees it free.
    fn hold(&self, body: i64) {
        *self.held.lock().unwrap().entry(body).or_insert(0) += 1;
    }

    /// Done writing or reading a body. One that no blob points at and nobody
    /// holds is then the collector's.
    pub fn let_go(&self, body: i64) {
        let mut held = self.held.lock().unwrap();
        if let Some(n) = held.get_mut(&body) {
            *n -= 1;
            if *n == 0 {
                held.remove(&body);
            }
        }
    }

    /// Free the bodies no blob points at and nobody holds, a few chunks per hold
    /// of the database so nobody waits long. Answers how many chunks went.
    pub fn collect_bodies(&self) -> Result<u64> {
        let mut freed = 0;
        loop {
            let db = self.lock();
            let pick = {
                let held = self.held.lock().unwrap();
                let mut q = db.prepare_cached(
                    "SELECT id FROM bodies b WHERE NOT EXISTS (SELECT 1 FROM blobs h WHERE h.body = b.id)
                     ORDER BY id",
                )?;
                let mut rows = q.query([])?;
                let mut pick = None;
                while let Some(r) = rows.next()? {
                    let body: i64 = r.get(0)?;
                    if !held.contains_key(&body) {
                        pick = Some(body);
                        break;
                    }
                }
                pick
            };
            let Some(body) = pick else { return Ok(freed) };
            let n = db.execute(
                "DELETE FROM chunks WHERE body = ?1
                 AND seq IN (SELECT seq FROM chunks WHERE body = ?1 ORDER BY seq LIMIT ?2)",
                params![body, COLLECT_BATCH],
            )?;
            if n == 0 {
                db.execute("DELETE FROM bodies WHERE id = ?1", params![body])?;
            }
            freed += n as u64;
        }
    }

    /// Test check: how many chunks are stored, whoever's.
    #[cfg(test)]
    pub fn chunk_count(&self) -> u64 {
        self.lock().query_row("SELECT COUNT(*) FROM chunks", [], |r| r.get::<_, i64>(0)).unwrap() as u64
    }

    // ---- generic items ----

    fn stored(&self, space: &str, (item, sealed, package): StoredRow) -> Result<StoredItem> {
        let data = self.vault.open(&item_aad(space, &item.kind, &item.id), &sealed)?;
        Ok(StoredItem { item, data, package })
    }

    /// The live items of one kind in a space, by name.
    pub fn items_in(&self, space: &str, kind: &str) -> Result<Vec<StoredItem>> {
        let rows = {
            let db = self.lock();
            let mut q = db.prepare(&format!(
                "{STORED_SELECT} WHERE i.space = ?1 AND i.kind = ?2 AND i.deleted_at IS NULL
                 ORDER BY i.name, i.id"
            ))?;
            let rows = q
                .query_map(params![space, kind], stored_row)?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            rows
        };
        rows.into_iter().map(|r| self.stored(space, r)).collect()
    }

    /// One live item; None when missing or in the trash.
    pub fn item_get(&self, space: &str, kind: &str, id: &str) -> Result<Option<StoredItem>> {
        let row = {
            let db = self.lock();
            db.query_row(
                &format!(
                    "{STORED_SELECT} WHERE i.space = ?1 AND i.kind = ?2 AND i.id = ?3
                     AND i.deleted_at IS NULL"
                ),
                params![space, kind, id],
                stored_row,
            )
            .optional()?
        };
        row.map(|r| self.stored(space, r)).transpose()
    }

    /// Write an item with the same guard as a profile, minus the lease:
    /// `authorize` on the row as it is here, then `base_revision`. An id in the
    /// trash stays taken.
    #[allow(clippy::too_many_arguments)]
    pub fn put_item_checked(
        &self,
        space: &str,
        kind: &str,
        id: &str,
        name: &str,
        data: &[u8],
        writer: Writer<'_>,
        base_revision: Option<u64>,
        authorize: impl FnOnce(&Standing, Option<&Item>) -> std::result::Result<(), Refusal>,
    ) -> Result<std::result::Result<Item, Refusal>> {
        let sealed = self.vault.seal(&item_aad(space, kind, id), data)?;
        let mut db = self.lock();
        let tx = db.transaction()?;
        let existing = match item_at(&tx, space, kind, id)? {
            Some((_, true)) => {
                let t = trashed_item(&tx, kind, id)?.context("a deleted item missing from the trash")?;
                return Ok(Err(Refusal::InTrash(Box::new(t))));
            }
            row => row.map(|(i, _)| i),
        };
        if let Err(r) = authorize(&standing_in(&tx, writer.member_id)?, existing.as_ref()) {
            return Ok(Err(r));
        }
        if let Err(r) = check_base(base_revision, existing.as_ref().map_or(0, |i| i.revision)) {
            return Ok(Err(r));
        }
        let ts = now();
        tx.execute(
            "INSERT INTO items(space, kind, id, name, revision, updated_at, updated_by, sealed)
             VALUES (?1, ?2, ?3, ?4, 1, ?5, ?6, ?7)
             ON CONFLICT(space, kind, id) DO UPDATE SET
                name = excluded.name, revision = items.revision + 1,
                updated_at = excluded.updated_at, updated_by = excluded.updated_by,
                sealed = excluded.sealed",
            params![space, kind, id, name, ts as i64, writer.member_id, &sealed],
        )?;
        let item = tx.query_row(
            "SELECT kind, id, name, revision, updated_at, updated_by FROM items
             WHERE space = ?1 AND kind = ?2 AND id = ?3",
            params![space, kind, id],
            item_row,
        )?;
        tx.commit()?;
        Ok(Ok(item))
    }

    /// Delete an item, judged like a write. From the team it waits in the
    /// trash; from a member's own space it is gone, with its package.
    pub fn delete_item_checked(
        &self,
        space: &str,
        kind: &str,
        id: &str,
        writer: Writer<'_>,
        base_revision: Option<u64>,
        authorize: impl FnOnce(&Standing, &Item) -> std::result::Result<(), Refusal>,
    ) -> Result<std::result::Result<(), Refusal>> {
        let mut db = self.lock();
        let tx = db.transaction()?;
        let item = match item_at(&tx, space, kind, id)? {
            Some((i, false)) => i,
            _ => return Ok(Err(Refusal::Missing)),
        };
        let who = standing_in(&tx, writer.member_id)?;
        if let Err(r) = authorize(&who, &item).and_then(|()| check_base(base_revision, item.revision)) {
            return Ok(Err(r));
        }
        if space == TEAM {
            tx.execute(
                "UPDATE items SET deleted_at = ?4, deleted_by = ?5
                 WHERE space = ?1 AND kind = ?2 AND id = ?3",
                params![space, kind, id, now() as i64, writer.member_id],
            )?;
        } else {
            tx.execute(
                "DELETE FROM items WHERE space = ?1 AND kind = ?2 AND id = ?3",
                params![space, kind, id],
            )?;
            if kind == EXTENSION {
                blob_drop(&tx, PACKAGE, space, id)?;
            }
        }
        tx.commit()?;
        Ok(Ok(()))
    }

    /// Put a trashed team item back, at a new revision; judged on the trash row.
    pub fn restore_item(
        &self,
        kind: &str,
        id: &str,
        writer: Writer<'_>,
        authorize: impl FnOnce(&Standing, &TrashItem) -> std::result::Result<(), Refusal>,
    ) -> Result<std::result::Result<Item, Refusal>> {
        let mut db = self.lock();
        let tx = db.transaction()?;
        let Some(t) = trashed_item(&tx, kind, id)? else {
            return Ok(Err(Refusal::Missing));
        };
        if let Err(r) = authorize(&standing_in(&tx, writer.member_id)?, &t) {
            return Ok(Err(r));
        }
        tx.execute(
            "UPDATE items SET deleted_at = NULL, deleted_by = NULL, revision = revision + 1,
                              updated_at = ?4, updated_by = ?5
             WHERE space = ?1 AND kind = ?2 AND id = ?3",
            params![TEAM, kind, id, now() as i64, writer.member_id],
        )?;
        let item = tx.query_row(
            "SELECT kind, id, name, revision, updated_at, updated_by FROM items
             WHERE space = ?1 AND kind = ?2 AND id = ?3",
            params![TEAM, kind, id],
            item_row,
        )?;
        tx.commit()?;
        Ok(Ok(item))
    }

    /// Remove a trashed team item for good, with its package; judged like a restore.
    pub fn purge_item(
        &self,
        kind: &str,
        id: &str,
        member_id: &str,
        authorize: impl FnOnce(&Standing, &TrashItem) -> std::result::Result<(), Refusal>,
    ) -> Result<std::result::Result<TrashItem, Refusal>> {
        let mut db = self.lock();
        let tx = db.transaction()?;
        let Some(t) = trashed_item(&tx, kind, id)? else {
            return Ok(Err(Refusal::Missing));
        };
        if let Err(r) = authorize(&standing_in(&tx, member_id)?, &t) {
            return Ok(Err(r));
        }
        tx.execute(
            "DELETE FROM items WHERE space = ?1 AND kind = ?2 AND id = ?3",
            params![TEAM, kind, id],
        )?;
        if kind == EXTENSION {
            blob_drop(&tx, PACKAGE, TEAM, id)?;
        }
        tx.commit()?;
        Ok(Ok(t))
    }

    // ---- an extension's package ----

    /// Where a live extension's package is, held for the caller as for a
    /// session. `authorize` judges the caller.
    pub fn package_open(
        &self,
        space: &str,
        id: &str,
        member_id: &str,
        authorize: impl FnOnce(&Standing) -> std::result::Result<(), Refusal>,
    ) -> Result<std::result::Result<(BlobMeta, BodyRef), Refusal>> {
        let mut db = self.lock();
        let tx = db.transaction()?;
        if !matches!(item_at(&tx, space, EXTENSION, id)?, Some((_, false))) {
            return Ok(Err(Refusal::Missing));
        }
        if let Err(r) = authorize(&standing_in(&tx, member_id)?) {
            return Ok(Err(r));
        }
        let Some((meta, body)) = blob_at(&tx, PACKAGE, space, id)? else {
            return Ok(Err(Refusal::Missing));
        };
        self.hold(body);
        let at = BodyRef { body, size: meta.size, aad: package_aad(space, id) };
        Ok(Ok((meta, at)))
    }

    /// A new upload of an extension's package.
    pub fn package_upload(&self, space: &str, id: &str) -> Result<Upload> {
        self.body_begin(package_aad(space, id))
    }

    /// Make a finished upload a live extension's package. `authorize` is told if
    /// it has one and the item as it is; `base`, given, must match (0 for none).
    pub fn package_commit(
        &self,
        space: &str,
        id: &str,
        writer: Writer<'_>,
        base: Option<u64>,
        up: &Upload,
        authorize: impl FnOnce(&Standing, bool, &Item) -> std::result::Result<(), Refusal>,
    ) -> Result<std::result::Result<BlobMeta, Refusal>> {
        if !up.done || up.aad != package_aad(space, id) {
            bail!("{id}: not a finished upload of this package");
        }
        let mut db = self.lock();
        let tx = db.transaction()?;
        let item = match item_at(&tx, space, EXTENSION, id)? {
            Some((i, false)) => i,
            _ => return Ok(Err(Refusal::Missing)),
        };
        let current = blob_at(&tx, PACKAGE, space, id)?.map(|(m, _)| m.rev);
        if let Err(r) = authorize(&standing_in(&tx, writer.member_id)?, current.is_some(), &item) {
            return Ok(Err(r));
        }
        let current = current.unwrap_or(0);
        if base.is_some_and(|b| b != current) {
            return Ok(Err(Refusal::PackageStale { current }));
        }
        let meta = head_put(&tx, PACKAGE, space, id, current + 1, writer.member_id, up)?;
        tx.commit()?;
        Ok(Ok(meta))
    }

    /// Test check: the package of a live extension, opened.
    #[cfg(test)]
    pub fn package_read(
        &self,
        space: &str,
        id: &str,
        member_id: &str,
        authorize: impl FnOnce(&Standing) -> std::result::Result<(), Refusal>,
    ) -> Result<std::result::Result<(BlobMeta, Vec<u8>), Refusal>> {
        let (meta, at) = match self.package_open(space, id, member_id, authorize)? {
            Ok(x) => x,
            Err(r) => return Ok(Err(r)),
        };
        let mut buf = Vec::new();
        let read = self.read_body(&at, &mut buf);
        self.let_go(at.body);
        read?;
        Ok(Ok((meta, buf)))
    }

    /// Test setup: a whole package uploaded and judged in one call.
    #[cfg(test)]
    pub fn package_write(
        &self,
        space: &str,
        id: &str,
        writer: Writer<'_>,
        base: Option<u64>,
        data: Vec<u8>,
        authorize: impl FnOnce(&Standing, bool, &Item) -> std::result::Result<(), Refusal>,
    ) -> Result<std::result::Result<BlobMeta, Refusal>> {
        let mut up = self.package_upload(space, id)?;
        self.upload_all(&mut up, &data)?;
        let r = self.package_commit(space, id, writer, base, &up, authorize);
        self.let_go(up.body);
        r
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grant(scope: Scope, action: Action, allow: bool) -> Grant {
        Grant { scope, action, allow }
    }

    #[test]
    fn a_new_team_is_empty() {
        let s = Store::open_memory().unwrap();
        assert!(s.profiles().unwrap().is_empty());
        assert!(s.proxies().unwrap().is_empty());
        assert!(s.members().unwrap().is_empty());
    }

    #[test]
    fn a_member_is_stored_without_their_token() {
        let s = Store::open_memory().unwrap();
        let m = s.add_member("m1", "Vera", "hash-of-the-token").unwrap();
        assert!(m.active);
        assert_eq!(m.epoch, 1);
        assert_eq!(s.member_by_token_hash("hash-of-the-token").unwrap().unwrap().id, "m1");
        assert!(s.member_by_token_hash("some-other-hash").unwrap().is_none());
    }

    #[test]
    fn setting_grants_replaces_them_and_moves_the_epoch() {
        let s = Store::open_memory().unwrap();
        s.add_member("m1", "Vera", "h1").unwrap();

        let e1 = s
            .set_grants(
                "m1",
                &[
                    grant(Scope::Team, Action::See, true),
                    grant(Scope::Folder { name: "Ads".into() }, Action::Start, true),
                ],
            )
            .unwrap();
        assert_eq!(e1, 2);
        assert_eq!(s.grants_of("m1").unwrap().len(), 2);

        // Replaced, not added to.
        let e2 = s.set_grants("m1", &[grant(Scope::Team, Action::See, true)]).unwrap();
        assert_eq!(e2, 3);
        let g = s.grants_of("m1").unwrap();
        assert_eq!(g.len(), 1);
        assert_eq!(g[0].action, Action::See);

        // Emptying the table is a real state, not a no-op.
        let e3 = s.set_grants("m1", &[]).unwrap();
        assert_eq!(e3, 4);
        assert!(s.grants_of("m1").unwrap().is_empty());
    }

    #[test]
    fn a_refusal_survives_the_round_trip() {
        let s = Store::open_memory().unwrap();
        s.add_member("m1", "Vera", "h1").unwrap();
        s.set_grants(
            "m1",
            &[
                grant(Scope::Folder { name: "Ads".into() }, Action::Start, true),
                grant(Scope::Profile { id: "p9".into() }, Action::Start, false),
            ],
        )
        .unwrap();
        let g = s.grants_of("m1").unwrap();
        let refused = g.iter().find(|x| !x.allow).expect("the refusal is stored");
        assert_eq!(refused.scope, Scope::Profile { id: "p9".into() });
    }

    #[test]
    fn turning_a_member_off_moves_the_epoch_too() {
        let s = Store::open_memory().unwrap();
        s.add_member("m1", "Vera", "h1").unwrap();
        let (e, freed) = s.set_member_active("m1", false).unwrap();
        assert_eq!(e, 2);
        assert!(freed.is_empty());
        assert!(!s.members().unwrap()[0].active);
        assert!(s.set_member_active("nobody", false).is_err());
    }

    #[test]
    fn a_profile_keeps_its_sealed_bytes_and_counts_its_revisions() {
        let s = Store::open_memory().unwrap();
        let p = s.put_profile("p1", "Etsy shop", "Shops", b"sealed-one", "m1").unwrap();
        assert_eq!(p.revision, 1);
        assert_eq!(s.profile_data("p1").unwrap().unwrap(), b"sealed-one");

        let p = s.put_profile("p1", "Etsy shop", "Shops", b"sealed-two", "m2").unwrap();
        assert_eq!(p.revision, 2);
        assert_eq!(p.updated_by, "m2");
        assert_eq!(s.profile_data("p1").unwrap().unwrap(), b"sealed-two");

        assert_eq!(s.profile_folder("p1").unwrap().unwrap(), "Shops");
        let script = Writer { member_id: "m1", device_id: None };
        assert_eq!(s.delete_profile_checked(TEAM, "p1", script, None, |_, _| Ok(())).unwrap(), Ok(()));
        assert_eq!(
            s.delete_profile_checked(TEAM, "p1", script, None, |_, _| Ok(())).unwrap(),
            Err(Refusal::Missing)
        );
        assert!(s.profile_folder("p1").unwrap().is_none());
    }

    #[test]
    fn profiles_with_no_folder_do_not_invent_one() {
        let s = Store::open_memory().unwrap();
        s.put_profile("p1", "Loose", "", b"x", "m1").unwrap();
        assert_eq!(s.profile_folder("p1").unwrap().unwrap(), "");
    }

    #[test]
    fn proxies_live_in_the_team_the_same_way() {
        let s = Store::open_memory().unwrap();
        let x = s.put_proxy("x1", "DC Tokyo", "socks5", "JP", b"sealed", "m1").unwrap();
        assert_eq!(x.revision, 1);
        assert_eq!(s.proxy_data("x1").unwrap().unwrap(), b"sealed");
        assert_eq!(s.proxies().unwrap().len(), 1);
        assert_eq!(s.delete_proxy_checked(TEAM, "x1", w("m1", "a"), None, |_, _| Ok(())).unwrap(), Ok(()));
        assert!(s.proxies().unwrap().is_empty());
        assert_eq!(
            s.delete_proxy_checked(TEAM, "x1", w("m1", "a"), None, |_, _| Ok(())).unwrap(),
            Err(Refusal::Missing)
        );
    }

    #[test]
    fn grants_go_when_the_member_does() {
        let s = Store::open_memory().unwrap();
        s.add_member("m1", "Vera", "h1").unwrap();
        s.set_grants("m1", &[grant(Scope::Team, Action::See, true)]).unwrap();
        let db = s.lock();
        db.execute("DELETE FROM members WHERE id = 'm1'", []).unwrap();
        drop(db);
        assert!(s.grants_of("m1").unwrap().is_empty());
    }

    // ---- spaces and leases ----

    fn w<'a>(member: &'a str, device: &'a str) -> Writer<'a> {
        Writer { member_id: member, device_id: Some(device) }
    }

    /// No rights check: a member's own space, or a test about something else.
    fn anyone<T>(_: &Standing, _: Option<&T>) -> std::result::Result<(), Refusal> {
        Ok(())
    }

    #[test]
    fn a_members_own_profiles_never_show_up_in_the_team() {
        let s = Store::open_memory().unwrap();
        let mine = personal_space("m1");
        s.put_profile_checked(&mine, "own-1", "My shop", "", b"x", w("m1", "laptop"), None, anyone)
            .unwrap()
            .unwrap();
        s.put_profile_checked(TEAM, "team-1", "Team shop", "", b"y", w("m1", "laptop"), None, anyone)
            .unwrap()
            .unwrap();
        let team: Vec<_> = s.profiles().unwrap().into_iter().map(|p| p.id).collect();
        assert_eq!(team, vec!["team-1".to_string()]);
        let personal: Vec<_> = s.profiles_in(&mine).unwrap().into_iter().map(|p| p.id).collect();
        assert_eq!(personal, vec!["own-1".to_string()]);
        // and nobody else's personal space has it
        assert!(s.profiles_in(&personal_space("m2")).unwrap().is_empty());
    }

    #[test]
    fn a_profile_open_on_one_device_cannot_be_opened_on_another() {
        let s = Store::open_memory().unwrap();
        let first = s.lease_acquire("p1", "m1", "laptop", "Vera's MacBook", 90).unwrap();
        assert!(first.is_ok());

        // Same person, other device: refused, and told where it is open.
        let busy = s.lease_acquire("p1", "m1", "desktop", "Vera's PC", 90).unwrap().unwrap_err();
        assert_eq!(busy.device_name, "Vera's MacBook");
        // Another person: refused too.
        assert!(s.lease_acquire("p1", "m2", "x", "Sam's", 90).unwrap().is_err());

        // The holder renews, and keeps the time it was first opened.
        let renewed = s.lease_acquire("p1", "m1", "laptop", "Vera's MacBook", 90).unwrap().unwrap();
        assert_eq!(renewed.acquired_at, first.unwrap().acquired_at);

        // Only the holder can close it.
        assert!(!s.lease_release("p1", "m1", "desktop").unwrap());
        assert!(s.lease_release("p1", "m1", "laptop").unwrap());
        assert!(s.lease_acquire("p1", "m1", "desktop", "Vera's PC", 90).unwrap().is_ok());
    }

    /// Started again after a long stop: the leases held then are renewable
    /// first by their own devices, and taken by nobody else meanwhile.
    #[test]
    fn a_restart_gives_every_lease_a_full_life_again() {
        let s = Store::open_memory().unwrap();
        s.lease_acquire("p1", "m1", "laptop", "Vera's MacBook", 90).unwrap().unwrap();
        s.lock().execute("UPDATE leases SET expires_at = 1", []).unwrap();
        assert!(s.lease_live("p1").unwrap().is_none(), "run out while the server was down");
        assert_eq!(s.leases_resume(90).unwrap(), 1);
        let held = s.lease_live("p1").unwrap().expect("alive again");
        assert!(held.expires_at >= lease_now() + 89, "{held:?}");
        assert!(s.lease_acquire("p1", "m2", "pc", "Sam's PC", 90).unwrap().is_err());
        assert!(s.lease_acquire("p1", "m1", "laptop", "Vera's MacBook", 90).unwrap().is_ok());
    }

    /// Leases run on a clock the wall clock cannot move once the server runs.
    #[test]
    fn the_lease_clock_moves_with_time_only() {
        let a = lease_now();
        assert!(a.abs_diff(now()) <= 2, "{a} against {}", now());
        std::thread::sleep(std::time::Duration::from_millis(1100));
        let b = lease_now();
        assert!((1..=3).contains(&(b - a)), "{a} then {b}");
    }

    #[test]
    fn a_lease_nobody_renews_frees_the_profile_on_its_own() {
        let s = Store::open_memory().unwrap();
        // A lease that has already run out — a launcher that crashed, a laptop shut.
        s.lease_acquire("p1", "m1", "laptop", "Vera's MacBook", 0).unwrap().unwrap();
        assert!(s.lease_live("p1").unwrap().is_none());
        assert!(s.lease_acquire("p1", "m2", "pc", "Sam's PC", 90).unwrap().is_ok());
    }

    #[test]
    fn nobody_can_write_over_a_profile_open_elsewhere() {
        let s = Store::open_memory().unwrap();
        s.put_profile_checked(TEAM, "p1", "Shop", "", b"v1", w("m1", "laptop"), None, anyone).unwrap().unwrap();
        s.lease_acquire("p1", "m1", "laptop", "Vera's MacBook", 90).unwrap().unwrap();

        // Another device of the same person.
        let r = s.put_profile_checked(TEAM, "p1", "Shop", "", b"v2", w("m1", "desktop"), Some(1), anyone).unwrap();
        assert!(matches!(r, Err(Refusal::Leased(_))));
        // A script with no device at all.
        let script = Writer { member_id: "m1", device_id: None };
        let r = s.put_profile_checked(TEAM, "p1", "Shop", "", b"v2", script, Some(1), anyone).unwrap();
        assert!(matches!(r, Err(Refusal::Leased(_))));
        // Deleting it out from under the browser.
        let r = s.delete_profile_checked(TEAM, "p1", w("m2", "pc"), Some(1), |_, _| Ok(())).unwrap();
        assert!(matches!(r, Err(Refusal::Leased(_))));

        // The device that has it open writes freely.
        let ok = s.put_profile_checked(TEAM, "p1", "Shop", "", b"v2", w("m1", "laptop"), Some(1), anyone).unwrap();
        assert_eq!(ok.unwrap().revision, 2);
        assert_eq!(s.profile_data("p1").unwrap().unwrap(), b"v2");
    }

    #[test]
    fn a_laptop_that_wakes_up_late_cannot_overwrite_newer_work() {
        let s = Store::open_memory().unwrap();
        s.put_profile_checked(TEAM, "p1", "Shop", "", b"v1", w("m1", "laptop"), None, anyone).unwrap().unwrap();
        // The laptop opened it at revision 1, then went to sleep; its lease ran out.
        s.lease_acquire("p1", "m1", "laptop", "Vera's MacBook", 0).unwrap().unwrap();
        // The desktop took it over and saved newer work.
        s.lease_acquire("p1", "m1", "desktop", "Vera's PC", 90).unwrap().unwrap();
        s.put_profile_checked(TEAM, "p1", "Shop", "", b"v2-from-desktop", w("m1", "desktop"), Some(1), anyone)
            .unwrap()
            .unwrap();
        s.lease_release("p1", "m1", "desktop").unwrap();

        // The laptop wakes and tries to save what it had, based on revision 1.
        let r = s
            .put_profile_checked(TEAM, "p1", "Shop", "", b"v1-stale", w("m1", "laptop"), Some(1), anyone)
            .unwrap();
        assert_eq!(r, Err(Refusal::Stale { current: 2 }));
        assert_eq!(s.profile_data("p1").unwrap().unwrap(), b"v2-from-desktop", "the newer work survives");
    }

    #[test]
    fn moving_between_my_space_and_the_team_is_one_step() {
        let s = Store::open_memory().unwrap();
        let mine = personal_space("m1");
        s.put_profile_checked(&mine, "p1", "My shop", "", b"data", w("m1", "laptop"), None, anyone)
            .unwrap()
            .unwrap();

        let moved = s.move_profile("p1", &mine, TEAM, "Shops", w("m1", "laptop"), |_, _, _| Ok(())).unwrap().unwrap();
        assert_eq!(moved.folder, "Shops");
        assert!(s.profiles_in(&mine).unwrap().is_empty());
        assert_eq!(s.profiles().unwrap().len(), 1);
        assert_eq!(s.profile_data("p1").unwrap().unwrap(), b"data", "the data moves with it, untouched");

        // Asking to move it from where it no longer is, is refused.
        assert_eq!(
            s.move_profile("p1", &mine, TEAM, "", w("m1", "laptop"), |_, _, _| Ok(())).unwrap(),
            Err(Refusal::OtherSpace)
        );
        // Open on another device: it stays where it is.
        s.lease_acquire("p1", "m2", "pc", "Sam's PC", 90).unwrap().unwrap();
        let r = s.move_profile("p1", TEAM, &mine, "", w("m1", "laptop"), |_, _, _| Ok(())).unwrap();
        assert!(matches!(r, Err(Refusal::Leased(_))));
        assert_eq!(s.profile_meta("p1").unwrap().unwrap().1, TEAM);
    }

    #[test]
    fn a_write_cannot_jump_an_id_into_another_space() {
        let s = Store::open_memory().unwrap();
        s.put_profile_checked(TEAM, "p1", "Team shop", "", b"t", w("m1", "a"), None, anyone).unwrap().unwrap();
        let r = s
            .put_profile_checked(&personal_space("m1"), "p1", "Mine now", "", b"m", w("m1", "a"), None, anyone)
            .unwrap();
        assert_eq!(r, Err(Refusal::OtherSpace));
        assert_eq!(s.profile_meta("p1").unwrap().unwrap().1, TEAM);
    }

    #[test]
    fn removing_a_member_frees_what_they_had_open() {
        let s = Store::open_memory().unwrap();
        s.add_member("m1", "Vera", "h1").unwrap();
        s.lease_acquire("p1", "m1", "laptop", "Vera's MacBook", 90).unwrap().unwrap();
        s.delete_member("m1").unwrap();
        assert!(s.lease_live("p1").unwrap().is_none());
        assert!(s.lease_acquire("p1", "m2", "pc", "Sam's PC", 90).unwrap().is_ok());
    }

    // ---- the rights check inside the write ----

    /// What a member with Create and no Edit is allowed.
    fn only_new<T>(_: &Standing, existing: Option<&T>) -> std::result::Result<(), Refusal> {
        match existing {
            Some(_) => Err(Refusal::Forbidden),
            None => Ok(()),
        }
    }

    #[test]
    fn the_rights_check_is_handed_the_row_as_the_write_finds_it() {
        let s = Store::open_memory().unwrap();
        s.put_profile_checked(TEAM, "p1", "First", "Ads", b"v1", w("m1", "a"), None, only_new)
            .unwrap()
            .unwrap();
        // The same id again is an overwrite, whatever the caller thought.
        let r = s.put_profile_checked(TEAM, "p1", "Over", "Ads", b"v2", w("m2", "b"), None, only_new).unwrap();
        assert_eq!(r, Err(Refusal::Forbidden));
        assert_eq!(s.profile_data("p1").unwrap().unwrap(), b"v1");

        // It sees the folder the profile is in now, not the one being written.
        let mut seen = None;
        s.put_profile_checked(TEAM, "p1", "Moved", "Shops", b"v3", w("m1", "a"), Some(1), |_, e| {
            seen = e.map(|p| (p.folder.clone(), p.revision));
            Ok(())
        })
        .unwrap()
        .unwrap();
        assert_eq!(seen, Some(("Ads".to_string(), 1)));

        // Proxies the same way.
        s.put_proxy_checked(TEAM, "x1", "DC", "socks5", "JP", b"v1", w("m1", "a"), None, only_new)
            .unwrap()
            .unwrap();
        let r = s
            .put_proxy_checked(TEAM, "x1", "DC", "socks5", "JP", b"v2", w("m2", "b"), None, only_new)
            .unwrap();
        assert_eq!(r, Err(Refusal::Forbidden));
        assert_eq!(s.proxy_data("x1").unwrap().unwrap(), b"v1");
    }

    #[test]
    fn nothing_slips_in_between_the_rights_check_and_the_write() {
        let store = Store::open_memory().unwrap();
        let s = &store;
        let (sam_tx, sam_rx) = std::sync::mpsc::channel();
        std::thread::scope(|sc| {
            let vera = s.put_profile_checked(TEAM, "p1", "Vera's", "", b"vera", w("m1", "a"), None, |who, e| {
                // Sam's create starts while Vera's check is running.
                sc.spawn(move || {
                    let r = s.put_profile_checked(TEAM, "p1", "Sam's", "", b"sam", w("m2", "b"), None, only_new);
                    sam_tx.send(r.unwrap()).unwrap();
                });
                only_new(who, e)
            });
            assert_eq!(vera.unwrap().unwrap().revision, 1);
        });
        // Sam waited for Vera's write and was judged on her row: an overwrite.
        assert_eq!(sam_rx.recv().unwrap(), Err(Refusal::Forbidden));
        assert_eq!(store.profile_data("p1").unwrap().unwrap(), b"vera");
    }

    #[test]
    fn a_refused_caller_learns_nothing_about_the_lease_or_the_revision() {
        let s = Store::open_memory().unwrap();
        s.put_profile_checked(TEAM, "p1", "Shop", "", b"v1", w("m1", "laptop"), None, anyone).unwrap().unwrap();
        s.put_profile_checked(TEAM, "p1", "Shop", "", b"v2", w("m1", "laptop"), Some(1), anyone).unwrap().unwrap();
        s.lease_acquire("p1", "m1", "laptop", "Vera's MacBook", 90).unwrap().unwrap();

        // Both stale and leased, and still only "missing".
        let r = s
            .put_profile_checked(TEAM, "p1", "x", "", b"x", w("m2", "pc"), Some(1), |_, _| Err(Refusal::Missing))
            .unwrap();
        assert_eq!(r, Err(Refusal::Missing));
        let r = s.delete_profile_checked(TEAM, "p1", w("m2", "pc"), Some(1), |_, _| Err(Refusal::Missing)).unwrap();
        assert_eq!(r, Err(Refusal::Missing));
        assert_eq!(s.profile_data("p1").unwrap().unwrap(), b"v2");
    }

    #[test]
    fn a_delete_based_on_an_old_revision_is_refused() {
        let s = Store::open_memory().unwrap();
        let me = w("m1", "a");
        s.put_profile_checked(TEAM, "p1", "Shop", "", b"v1", me, None, anyone).unwrap().unwrap();
        s.put_profile_checked(TEAM, "p1", "Shop", "", b"v2", me, Some(1), anyone).unwrap().unwrap();
        assert_eq!(
            s.delete_profile_checked(TEAM, "p1", me, Some(1), |_, _| Ok(())).unwrap(),
            Err(Refusal::Stale { current: 2 })
        );
        assert!(s.profile_meta("p1").unwrap().is_some());
        assert_eq!(s.delete_profile_checked(TEAM, "p1", me, Some(2), |_, _| Ok(())).unwrap(), Ok(()));

        s.put_proxy_checked(TEAM, "x1", "DC", "socks5", "JP", b"v1", me, None, anyone).unwrap().unwrap();
        s.put_proxy_checked(TEAM, "x1", "DC", "socks5", "JP", b"v2", me, Some(1), anyone).unwrap().unwrap();
        assert_eq!(
            s.delete_proxy_checked(TEAM, "x1", me, Some(1), |_, _| Ok(())).unwrap(),
            Err(Refusal::Stale { current: 2 })
        );
        assert!(s.proxy_meta("x1").unwrap().is_some());
        assert_eq!(s.delete_proxy_checked(TEAM, "x1", me, Some(2), |_, _| Ok(())).unwrap(), Ok(()));
        assert!(s.proxy_meta("x1").unwrap().is_none());
    }

    #[test]
    fn a_proxy_is_deleted_only_from_the_space_it_is_in() {
        let s = Store::open_memory().unwrap();
        let mine = personal_space("m1");
        s.put_proxy_checked(&mine, "x1", "Home", "socks5", "", b"x", w("m1", "a"), None, anyone)
            .unwrap()
            .unwrap();
        for other in [TEAM.to_string(), personal_space("m2")] {
            assert_eq!(
                s.delete_proxy_checked(&other, "x1", w("m1", "a"), None, |_, _| Ok(())).unwrap(),
                Err(Refusal::OtherSpace)
            );
        }
        // A lease under the same id is a profile's: it neither holds the proxy
        // nor goes with it.
        s.lease_acquire("x1", "m2", "pc", "Sam's PC", 90).unwrap().unwrap();
        assert_eq!(s.delete_proxy_checked(&mine, "x1", w("m1", "a"), None, |_, _| Ok(())).unwrap(), Ok(()));
        assert!(s.lease_live("x1").unwrap().is_some());
    }

    // ---- folders ----

    #[test]
    fn an_empty_folder_can_exist_and_be_granted_on() {
        let s = Store::open_memory().unwrap();
        assert!(s.create_folder(TEAM, "Ads", "orange").unwrap());
        assert!(!s.create_folder(TEAM, "Ads", "blue").unwrap(), "one folder per name");
        let f = s.folders_in(TEAM).unwrap();
        assert_eq!(f.len(), 1);
        assert_eq!((f[0].name.as_str(), f[0].color.as_str(), f[0].profiles), ("Ads", "orange", 0));

        s.add_member("m1", "Vera", "h1").unwrap();
        s.set_grants("m1", &[grant(Scope::Folder { name: "Ads".into() }, Action::Create, true)]).unwrap();
        assert_eq!(s.grants_of("m1").unwrap().len(), 1);
    }

    #[test]
    fn renaming_a_folder_carries_its_profiles_and_its_rules_along() {
        let s = Store::open_memory().unwrap();
        s.add_member("m1", "Vera", "h1").unwrap();
        s.add_member("m2", "Sam", "h2").unwrap();
        s.add_member("m3", "Zoe", "h3").unwrap();
        s.put_profile_checked(TEAM, "p1", "Ad acc", "Ads", b"x", w("m1", "a"), None, anyone).unwrap().unwrap();
        s.set_grants(
            "m1",
            &[
                grant(Scope::Folder { name: "Ads".into() }, Action::See, true),
                grant(Scope::Folder { name: "Ads".into() }, Action::Start, true),
                grant(Scope::Folder { name: "Shops".into() }, Action::See, true),
            ],
        )
        .unwrap();
        s.set_grants("m2", &[grant(Scope::Team, Action::See, true)]).unwrap();
        // Zoe's rule hides the folder from her.
        s.set_grants(
            "m3",
            &[grant(Scope::Team, Action::See, true), grant(Scope::Folder { name: "Ads".into() }, Action::See, false)],
        )
        .unwrap();
        let vera_before = s.members().unwrap().into_iter().find(|m| m.id == "m1").unwrap().epoch;
        let sam_before = s.members().unwrap().into_iter().find(|m| m.id == "m2").unwrap().epoch;
        let zoe_before = s.members().unwrap().into_iter().find(|m| m.id == "m3").unwrap().epoch;

        let r = s.rename_folder(TEAM, "Ads", "Advertising").unwrap();
        assert_eq!(r, FolderRename::Done);
        // Vera's rules moved; Sam sees one folder go and another come.
        let told: Vec<String> = s.take_regranted().into_iter().map(|(m, _)| m).collect();
        assert_eq!(told, ["m1", "m2"]);

        // The profile moved with it.
        assert_eq!(s.profile_meta("p1").unwrap().unwrap().0.folder, "Advertising");
        // Vera's rule now names the new folder, and only that rule changed.
        let g = s.grants_of("m1").unwrap();
        assert!(g.contains(&grant(Scope::Folder { name: "Advertising".into() }, Action::Start, true)));
        assert!(g.contains(&grant(Scope::Folder { name: "Shops".into() }, Action::See, true)));
        assert!(!g.iter().any(|x| x.scope == Scope::Folder { name: "Ads".into() }));
        // Their editions moved, so their launchers fetch the new one.
        let after = |id: &str| s.members().unwrap().into_iter().find(|m| m.id == id).unwrap().epoch;
        assert!(after("m1") > vera_before);
        assert!(after("m2") > sam_before);
        // Zoe's rule moved too, but her permission never carried it.
        assert!(s.grants_of("m3").unwrap().contains(&grant(folder("Advertising"), Action::See, false)));
        assert_eq!(after("m3"), zoe_before);
    }

    #[test]
    fn a_rename_onto_a_name_in_use_is_refused() {
        let s = Store::open_memory().unwrap();
        s.create_folder(TEAM, "Ads", "").unwrap();
        s.create_folder(TEAM, "Shops", "").unwrap();
        assert_eq!(s.rename_folder(TEAM, "Ads", "Shops").unwrap(), FolderRename::NameTaken);
        assert_eq!(s.rename_folder(TEAM, "Nope", "X").unwrap(), FolderRename::Missing);
    }

    #[test]
    fn a_folder_with_profiles_in_it_is_not_deleted() {
        let s = Store::open_memory().unwrap();
        s.put_profile_checked(TEAM, "p1", "Ad acc", "Ads", b"x", w("m1", "a"), None, anyone).unwrap().unwrap();
        assert_eq!(s.delete_folder(TEAM, "Ads").unwrap(), FolderDelete::NotEmpty(1));
        assert_eq!(s.folders_in(TEAM).unwrap().len(), 1);

        // Emptied, it goes — and its rules with it.
        s.move_profile("p1", TEAM, TEAM, "", w("m1", "a"), |_, _, _| Ok(())).unwrap().unwrap();
        s.add_member("m1", "Vera", "h1").unwrap();
        s.set_grants("m1", &[grant(Scope::Folder { name: "Ads".into() }, Action::See, true)]).unwrap();
        assert_eq!(s.delete_folder(TEAM, "Ads").unwrap(), FolderDelete::Done);
        assert_eq!(s.take_regranted().into_iter().map(|(m, _)| m).collect::<Vec<_>>(), ["m1"]);
        assert!(s.grants_of("m1").unwrap().is_empty());
    }

    #[test]
    fn writing_a_profile_into_a_new_folder_makes_the_folder() {
        let s = Store::open_memory().unwrap();
        s.put_profile_checked(TEAM, "p1", "x", "Brand new", b"x", w("m1", "a"), None, anyone).unwrap().unwrap();
        assert_eq!(s.folders_in(TEAM).unwrap()[0].name, "Brand new");
    }

    #[test]
    fn my_folders_and_the_teams_do_not_mix() {
        let s = Store::open_memory().unwrap();
        s.create_folder(TEAM, "Ads", "").unwrap();
        s.create_folder(&personal_space("m1"), "Ads", "").unwrap();
        s.add_member("m1", "Vera", "h1").unwrap();
        s.set_grants("m1", &[grant(Scope::Folder { name: "Ads".into() }, Action::See, true)]).unwrap();

        // Renaming my own "Ads" leaves the team's alone, and its rules too.
        s.rename_folder(&personal_space("m1"), "Ads", "Mine").unwrap();
        assert_eq!(s.folders_in(TEAM).unwrap()[0].name, "Ads");
        assert_eq!(s.grants_of("m1").unwrap()[0].scope, Scope::Folder { name: "Ads".into() });
    }

    fn folder(name: &str) -> Scope {
        Scope::Folder { name: name.into() }
    }

    #[test]
    fn a_save_from_before_a_folder_rename_is_stale() {
        let s = Store::open_memory().unwrap();
        s.create_folder(TEAM, "Ads", "").unwrap();
        s.put_profile_checked(TEAM, "p1", "Ad acc", "Ads", b"v1", w("m1", "mac"), None, anyone)
            .unwrap()
            .unwrap();
        s.rename_folder(TEAM, "Ads", "Clients").unwrap();
        let (p, _) = s.profile_meta("p1").unwrap().unwrap();
        assert_eq!((p.folder.as_str(), p.revision), ("Clients", 2));

        // The Mac read it at revision 1, in Ads, and saves that now.
        let r = s
            .put_profile_checked(TEAM, "p1", "Ad acc", "Ads", b"v2", w("m1", "mac"), Some(1), anyone)
            .unwrap();
        assert_eq!(r, Err(Refusal::Stale { current: 2 }));
        assert_eq!(s.profile_meta("p1").unwrap().unwrap().0.folder, "Clients");
        let names: Vec<String> = s.folders_in(TEAM).unwrap().into_iter().map(|f| f.name).collect();
        assert_eq!(names, vec!["Clients".to_string()], "the old name did not come back");

        // The same in a member's own space.
        let mine = personal_space("m1");
        s.put_profile_checked(&mine, "p2", "Own", "Mine", b"v1", w("m1", "mac"), None, anyone)
            .unwrap()
            .unwrap();
        s.rename_folder(&mine, "Mine", "Ours").unwrap();
        assert_eq!(s.profile_meta("p2").unwrap().unwrap().0.revision, 2);
    }

    #[test]
    fn a_move_is_judged_on_the_row_as_the_move_finds_it() {
        let s = Store::open_memory().unwrap();
        let sams = personal_space("m1");
        s.put_profile_checked(TEAM, "p1", "Shop", "Free", b"v1", w("m2", "b"), None, anyone)
            .unwrap()
            .unwrap();
        // Sam read it in Free; before his move runs, Vera files it under Clients.
        s.put_profile_checked(TEAM, "p1", "Shop", "Clients", b"v1", w("m2", "b"), Some(1), anyone)
            .unwrap()
            .unwrap();
        let mut seen = None;
        let r = s
            .move_profile("p1", TEAM, &sams, "", w("m1", "a"), |_, p, _| {
                seen = Some(p.folder.clone());
                if p.folder == "Free" {
                    Ok(())
                } else {
                    Err(Refusal::Forbidden)
                }
            })
            .unwrap();
        assert_eq!(r, Err(Refusal::Forbidden));
        assert_eq!(seen.as_deref(), Some("Clients"));
        assert_eq!(s.profile_meta("p1").unwrap().unwrap().1, TEAM);

        s.put_proxy_checked(TEAM, "x1", "DC", "socks5", "JP", b"v1", w("m2", "b"), None, anyone)
            .unwrap()
            .unwrap();
        let r = s.move_proxy("x1", TEAM, &sams, w("m1", "a"), |_, _, _| Err(Refusal::Forbidden)).unwrap();
        assert_eq!(r, Err(Refusal::Forbidden));
        assert_eq!(s.proxy_meta("x1").unwrap().unwrap().1, TEAM);
    }

    #[test]
    fn a_lease_is_taken_only_on_a_profile_still_there_for_the_taker() {
        let s = Store::open_memory().unwrap();
        let take = |id: &str, ok: bool| {
            s.lease_take(TEAM, id, "m2", "pc", "Sam's PC", 90, |_, _| {
                if ok {
                    Ok(())
                } else {
                    Err(Refusal::Forbidden)
                }
            })
            .unwrap()
        };
        for id in ["p1", "p3"] {
            s.put_profile_checked(TEAM, id, "Shop", "", b"v1", w("m1", "a"), None, anyone)
                .unwrap()
                .unwrap();
        }
        // Vera took p1 into her own space a moment before Sam's lease ran.
        s.move_profile("p1", TEAM, &personal_space("m1"), "", w("m1", "a"), |_, _, _| Ok(()))
            .unwrap()
            .unwrap();
        assert_eq!(take("p1", true), Err(Refusal::OtherSpace));
        // Deleted, or no longer his to start.
        assert_eq!(take("p-gone", true), Err(Refusal::Missing));
        assert_eq!(take("p3", false), Err(Refusal::Forbidden));
        for id in ["p1", "p-gone", "p3"] {
            assert!(s.lease_live(id).unwrap().is_none(), "{id}");
        }
        // Still there and still his: taken.
        assert_eq!(take("p3", true).unwrap().lease.device_name, "Sam's PC");
    }

    #[test]
    fn a_guarded_write_is_judged_by_the_rights_as_they_are_at_the_write() {
        let s = Store::open_memory().unwrap();
        s.add_member("m1", "Vera", "h1").unwrap();
        s.set_grants("m1", &[grant(Scope::Team, Action::Delete, true)]).unwrap();
        s.put_profile_checked(TEAM, "p1", "Shop", "", b"v1", w("m1", "a"), None, anyone)
            .unwrap()
            .unwrap();

        // What the write is handed, read at the write.
        let seen = || {
            let mut got = None;
            let r = s.delete_profile_checked(TEAM, "p1", w("m1", "a"), None, |who, _| {
                got = Some((who.active, who.grants.clone()));
                Err(Refusal::Forbidden)
            });
            assert_eq!(r.unwrap(), Err(Refusal::Forbidden));
            got.unwrap()
        };
        assert_eq!(seen(), (true, vec![grant(Scope::Team, Action::Delete, true)]));
        // The request came in holding Delete; the admin took it away before the write ran.
        s.set_grants("m1", &[]).unwrap();
        assert_eq!(seen(), (true, vec![]));
        // Switched off, and then removed: the write sees that too.
        s.set_member_active("m1", false).unwrap();
        assert_eq!(seen(), (false, vec![]));
        s.delete_member("m1").unwrap();
        let r = s
            .lease_take(TEAM, "p1", "m1", "laptop", "Vera's MacBook", 90, |who, _| {
                if who.active {
                    Ok(())
                } else {
                    Err(Refusal::Forbidden)
                }
            })
            .unwrap();
        assert_eq!(r, Err(Refusal::Forbidden));
        assert!(s.lease_live("p1").unwrap().is_none(), "a removed member holds nothing");
    }

    #[test]
    fn switching_a_member_off_frees_what_they_had_open() {
        let s = Store::open_memory().unwrap();
        s.add_member("m1", "Vera", "h1").unwrap();
        s.lease_acquire("p1", "m1", "laptop", "Vera's MacBook", 90).unwrap().unwrap();
        s.set_member_active("m1", false).unwrap();
        assert!(s.lease_live("p1").unwrap().is_none());
        assert!(s.lease_acquire("p1", "m2", "pc", "Sam's PC", 90).unwrap().is_ok());
        // Back on: nothing comes back, and what Sam took stays his.
        s.set_member_active("m1", true).unwrap();
        assert_eq!(s.lease_live("p1").unwrap().unwrap().member_id, "m2");
    }

    #[test]
    fn a_rename_onto_a_name_rules_already_use_merges_them_and_the_refusal_wins() {
        let s = Store::open_memory().unwrap();
        s.add_member("m2", "Sam", "h2").unwrap();
        s.set_grants(
            "m2",
            &[
                grant(folder("Ads"), Action::See, true),
                grant(folder("Clients"), Action::See, true),
                grant(folder("Ads"), Action::Delete, true),
                grant(folder("Clients"), Action::Delete, false),
                grant(folder("Ads"), Action::Start, false),
                grant(folder("Clients"), Action::Start, true),
            ],
        )
        .unwrap();
        // Only rules name Clients; there is no folder of that name.
        s.create_folder(TEAM, "Ads", "").unwrap();
        s.take_regranted();
        assert_eq!(s.rename_folder(TEAM, "Ads", "Clients").unwrap(), FolderRename::Done);
        assert_eq!(s.take_regranted().into_iter().map(|(m, _)| m).collect::<Vec<_>>(), ["m2"]);
        let g = s.grants_of("m2").unwrap();
        assert_eq!(g.len(), 3, "{g:?}");
        assert!(g.contains(&grant(folder("Clients"), Action::See, true)));
        assert!(g.contains(&grant(folder("Clients"), Action::Delete, false)));
        assert!(g.contains(&grant(folder("Clients"), Action::Start, false)));
    }

    #[test]
    fn a_rename_that_merges_a_refusal_in_moves_the_permission_of_whoever_it_hides_from() {
        let s = Store::open_memory().unwrap();
        s.put_profile_checked(TEAM, "p1", "Ad acc", "Ads", b"x", w("m1", "a"), None, anyone).unwrap().unwrap();
        s.add_member("m1", "Vera", "h1").unwrap();
        // Clients is not there yet; her refusal on it says nothing so far.
        let p1_start = grant(Scope::Profile { id: "p1".into() }, Action::Start, true);
        s.set_grants(
            "m1",
            &[grant(Scope::Team, Action::See, true), grant(folder("Clients"), Action::See, false), p1_start.clone()],
        )
        .unwrap();
        let rules = |s: &Store| s.permission_view("m1").unwrap().unwrap().rules;
        assert!(rules(&s).contains(&p1_start));
        s.take_regranted();

        // Ads becomes Clients, the refusal now hides p1, and the rule naming it goes.
        assert_eq!(s.rename_folder(TEAM, "Ads", "Clients").unwrap(), FolderRename::Done);
        assert_eq!(rules(&s), vec![grant(Scope::Team, Action::See, true)]);
        let epoch = s.permission_view("m1").unwrap().unwrap().epoch;
        assert_eq!(s.take_regranted(), vec![("m1".to_owned(), epoch)]);
    }

    #[test]
    fn a_folder_not_made_yet_is_carried_when_its_own_rules_let_them_see_or_create_there() {
        let s = Store::open_memory().unwrap();
        s.create_folder(TEAM, "Secret", "").unwrap();
        s.add_member("m1", "Ann", "h1").unwrap();
        let (see_q4, make_q4) = (grant(folder("Q4"), Action::See, true), grant(folder("Q4"), Action::Create, true));
        let drop_box = grant(folder("Inbox"), Action::Create, true);
        let hide_plan = grant(folder("Plan"), Action::See, false);
        let hide_secret = grant(folder("Secret"), Action::See, false);
        s.set_grants(
            "m1",
            &[
                see_q4.clone(),
                make_q4.clone(),
                drop_box.clone(),
                grant(Scope::Team, Action::Create, true),
                hide_plan.clone(),
                hide_secret.clone(),
            ],
        )
        .unwrap();
        let rules = s.permission_view("m1").unwrap().unwrap().rules;
        // Q4 and Inbox are not there yet; their own rules let her see or fill them.
        assert!(rules.contains(&see_q4) && rules.contains(&make_q4) && rules.contains(&drop_box), "{rules:?}");
        let q4 = Target::folder("Q4");
        let made = crate::model::decide(&rules, Action::Create, &q4);
        assert_eq!(made, crate::model::Decision::Allowed { by: folder("Q4") });
        // Create for the whole team names no folder: Plan and Secret stay unnamed.
        assert!(!rules.contains(&hide_plan) && !rules.contains(&hide_secret), "{rules:?}");
    }

    #[test]
    fn a_table_naming_one_rule_twice_is_kept_with_the_refusal() {
        let s = Store::open_memory().unwrap();
        s.add_member("m1", "Vera", "h1").unwrap();
        s.set_grants(
            "m1",
            &[
                grant(folder("Ads"), Action::Delete, true),
                grant(folder("Ads"), Action::Delete, false),
                grant(Scope::Team, Action::See, true),
                grant(Scope::Team, Action::See, true),
            ],
        )
        .unwrap();
        let g = s.grants_of("m1").unwrap();
        assert_eq!(g.len(), 2, "{g:?}");
        assert!(g.contains(&grant(folder("Ads"), Action::Delete, false)));
        assert!(g.contains(&grant(Scope::Team, Action::See, true)));
    }

    #[test]
    fn one_id_is_one_item_whether_profile_or_proxy() {
        let s = Store::open_memory().unwrap();
        let mine = personal_space("m1");
        s.put_proxy_checked(TEAM, "X", "DC", "socks5", "", b"x", w("m2", "b"), None, anyone)
            .unwrap()
            .unwrap();
        s.put_profile_checked(TEAM, "P", "Shop", "", b"p", w("m2", "b"), None, anyone)
            .unwrap()
            .unwrap();
        for space in [TEAM.to_string(), mine] {
            let r = s.put_profile_checked(&space, "X", "Mine", "", b"m", w("m1", "a"), None, anyone);
            assert_eq!(r.unwrap(), Err(Refusal::OtherSpace));
            let r = s.put_proxy_checked(&space, "P", "Mine", "socks5", "", b"m", w("m1", "a"), None, anyone);
            assert_eq!(r.unwrap(), Err(Refusal::OtherSpace));
        }
        assert!(s.profile_meta("X").unwrap().is_none());
        assert!(s.proxy_meta("P").unwrap().is_none());
    }

    // ---- the team's trash ----

    fn ids(items: &[TrashItem]) -> Vec<&str> {
        items.iter().map(|t| t.id.as_str()).collect()
    }

    #[test]
    fn a_team_delete_goes_to_the_trash_and_a_personal_one_is_gone() {
        let s = Store::open_memory().unwrap();
        let mine = personal_space("m1");
        let me = w("m1", "a");
        s.put_profile_checked(TEAM, "p1", "Shop", "Ads", b"team", me, None, anyone).unwrap().unwrap();
        s.put_proxy_checked(TEAM, "x1", "DC", "socks5", "JP", b"proxy", me, None, anyone).unwrap().unwrap();
        s.put_profile_checked(&mine, "p2", "Own", "", b"own", me, None, anyone).unwrap().unwrap();
        s.put_proxy_checked(&mine, "x2", "Home", "http", "", b"own", me, None, anyone).unwrap().unwrap();

        for (space, id) in [(TEAM, "p1"), (mine.as_str(), "p2")] {
            assert_eq!(s.delete_profile_checked(space, id, me, None, |_, _| Ok(())).unwrap(), Ok(()));
        }
        for (space, id) in [(TEAM, "x1"), (mine.as_str(), "x2")] {
            assert_eq!(s.delete_proxy_checked(space, id, me, None, |_, _| Ok(())).unwrap(), Ok(()));
        }

        let t = s.trash().unwrap();
        assert_eq!(t.len(), 2, "only the team's: {t:?}");
        let p = t.iter().find(|x| x.id == "p1").unwrap();
        assert_eq!((p.kind, p.folder.as_str(), p.deleted_by.as_str(), p.revision), (TrashKind::Profile, "Ads", "m1", 1));
        let x = t.iter().find(|x| x.id == "x1").unwrap();
        assert_eq!((x.kind, x.proxy_kind.as_str(), x.country.as_str()), (TrashKind::Proxy, "socks5", "JP"));
        // Out of the lists and unreadable as live items.
        assert!(s.profile_meta("p1").unwrap().is_none() && s.profile_data("p1").unwrap().is_none());
        assert!(s.proxy_meta("x1").unwrap().is_none() && s.proxy_data("x1").unwrap().is_none());
    }

    #[test]
    fn restoring_puts_it_back_where_it_was_at_a_new_revision() {
        let s = Store::open_memory().unwrap();
        let me = w("m1", "a");
        s.put_profile_checked(TEAM, "p1", "Shop", "Ads", b"v1", me, None, anyone).unwrap().unwrap();
        s.put_profile_checked(TEAM, "p1", "Shop", "Ads", b"v2", me, Some(1), anyone).unwrap().unwrap();
        s.delete_profile_checked(TEAM, "p1", me, None, |_, _| Ok(())).unwrap().unwrap();
        // Its folder stays while it waits: its rules judge it there.
        assert_eq!(s.delete_folder(TEAM, "Ads").unwrap(), FolderDelete::InTrash(1));
        // An older server could have let the folder go meanwhile.
        s.lock().execute("DELETE FROM folders WHERE name = 'Ads'", []).unwrap();

        // A refusal leaves it in the trash.
        let r = s.restore_from_trash("p1", w("m2", "b"), |_, _| Err(Refusal::Forbidden)).unwrap();
        assert_eq!(r, Err(Refusal::Forbidden));
        assert_eq!(ids(&s.trash().unwrap()), vec!["p1"]);

        let mut judged = None;
        let r = s
            .restore_from_trash("p1", w("m2", "b"), |_, t| {
                judged = Some(t.folder.clone());
                Ok(())
            })
            .unwrap()
            .unwrap();
        assert_eq!(judged.as_deref(), Some("Ads"), "judged where it was");
        let Restored::Profile(p) = r else { panic!("a profile comes back as one") };
        assert_eq!((p.folder.as_str(), p.revision, p.updated_by.as_str()), ("Ads", 3, "m2"));
        assert_eq!(s.profile_data("p1").unwrap().unwrap(), b"v2", "the same bytes");
        assert_eq!(s.profile_meta("p1").unwrap().unwrap().1, TEAM);
        assert_eq!(s.folders_in(TEAM).unwrap()[0].name, "Ads", "its folder is back");
        assert!(s.trash().unwrap().is_empty());
        assert_eq!(s.restore_from_trash("p1", me, |_, _| Ok(())).unwrap(), Err(Refusal::Missing));

        // A proxy the same way.
        s.put_proxy_checked(TEAM, "x1", "DC", "socks5", "JP", b"login", me, None, anyone).unwrap().unwrap();
        s.delete_proxy_checked(TEAM, "x1", me, None, |_, _| Ok(())).unwrap().unwrap();
        let Restored::Proxy(x) = s.restore_from_trash("x1", me, |_, _| Ok(())).unwrap().unwrap() else {
            panic!("a proxy comes back as one")
        };
        assert_eq!((x.kind.as_str(), x.country.as_str(), x.revision), ("socks5", "JP", 2));
        assert_eq!(s.proxy_data("x1").unwrap().unwrap(), b"login");
    }

    #[test]
    fn an_id_in_the_trash_is_still_taken() {
        let s = Store::open_memory().unwrap();
        let me = w("m1", "a");
        s.put_profile_checked(TEAM, "p1", "Shop", "", b"v1", me, None, anyone).unwrap().unwrap();
        s.delete_profile_checked(TEAM, "p1", me, None, |_, _| Ok(())).unwrap().unwrap();

        for space in [TEAM.to_string(), personal_space("m1")] {
            let r = s.put_profile_checked(&space, "p1", "New", "", b"new", me, None, anyone).unwrap();
            assert!(matches!(r, Err(Refusal::InTrash(ref t)) if t.id == "p1"), "{space}: {r:?}");
            let r = s.put_proxy_checked(&space, "p1", "New", "socks5", "", b"new", me, None, anyone).unwrap();
            assert!(matches!(r, Err(Refusal::InTrash(ref t)) if t.id == "p1"), "{space}: {r:?}");
        }
        // Purged, the id is free again.
        let gone = s.purge_from_trash("p1", "m1", |_, _| Ok(())).unwrap().unwrap();
        assert_eq!(gone.id, "p1");
        assert!(s.put_profile_checked(TEAM, "p1", "New", "", b"new", me, None, anyone).unwrap().is_ok());
    }

    #[test]
    fn purging_is_judged_on_the_trash_row() {
        let s = Store::open_memory().unwrap();
        s.add_member("m1", "Vera", "h1").unwrap();
        s.set_grants("m1", &[grant(Scope::Team, Action::Delete, true)]).unwrap();
        s.put_profile_checked(TEAM, "p1", "Shop", "Ads", b"v1", w("m2", "b"), None, anyone).unwrap().unwrap();
        s.delete_profile_checked(TEAM, "p1", w("m2", "b"), None, |_, _| Ok(())).unwrap().unwrap();

        let mut seen = None;
        let r = s
            .purge_from_trash("p1", "m1", |who, t| {
                seen = Some((who.grants.clone(), t.folder.clone()));
                Err(Refusal::Forbidden)
            })
            .unwrap();
        assert_eq!(r, Err(Refusal::Forbidden));
        assert_eq!(seen, Some((vec![grant(Scope::Team, Action::Delete, true)], "Ads".to_string())));
        assert_eq!(ids(&s.trash().unwrap()), vec!["p1"]);
        assert!(s.purge_from_trash("p1", "m1", |_, _| Ok(())).unwrap().is_ok());
        assert_eq!(s.purge_from_trash("p1", "m1", |_, _| Ok(())).unwrap(), Err(Refusal::Missing));
    }

    #[test]
    fn the_trash_lets_go_of_what_ran_out_of_days() {
        let s = Store::open_memory().unwrap();
        let me = w("m1", "a");
        for id in ["old", "new"] {
            s.put_profile_checked(TEAM, id, id, "", b"x", me, None, anyone).unwrap().unwrap();
            s.delete_profile_checked(TEAM, id, me, None, |_, _| Ok(())).unwrap().unwrap();
        }
        s.backdate_trash("old", 100).unwrap();

        assert_eq!(ids(&s.expire_trash(1_000).unwrap()), vec!["old"]);
        assert_eq!(ids(&s.trash().unwrap()), vec!["new"]);
        assert!(s.expire_trash(1_000).unwrap().is_empty(), "nothing twice");
        // The newer one goes once its time comes too.
        assert_eq!(ids(&s.expire_trash(now()).unwrap()), vec!["new"]);
    }

    #[test]
    fn a_folder_rename_carries_the_trash_along() {
        let s = Store::open_memory().unwrap();
        let me = w("m1", "a");
        s.put_profile_checked(TEAM, "p1", "Shop", "Ads", b"x", me, None, anyone).unwrap().unwrap();
        s.put_profile_checked(TEAM, "p2", "Keep", "Ads", b"x", me, None, anyone).unwrap().unwrap();
        s.delete_profile_checked(TEAM, "p1", me, None, |_, _| Ok(())).unwrap().unwrap();
        // The same name in a member's own space is another folder.
        s.create_folder(&personal_space("m1"), "Ads", "").unwrap();
        s.rename_folder(&personal_space("m1"), "Ads", "Mine").unwrap();
        assert_eq!(s.trash().unwrap()[0].folder, "Ads");

        s.rename_folder(TEAM, "Ads", "Clients").unwrap();
        assert_eq!(s.trash().unwrap()[0].folder, "Clients");
    }

    // ---- the admin principal ----

    #[test]
    fn the_admin_is_always_on_with_every_right_and_no_member_takes_its_id() {
        let s = Store::open_memory().unwrap();
        let a = s.standing(ADMIN_ID).unwrap();
        assert!(a.known && a.active);
        assert_eq!(a.grants, admin_grants());
        // A removed member, by contrast, is unknown.
        let gone = s.standing("m-gone").unwrap();
        assert!(!gone.known && !gone.active && gone.grants.is_empty());

        assert!(s.add_member(ADMIN_ID, "Impostor", "h").is_err());
        assert!(s.members().unwrap().is_empty());
    }

    // ---- what lists need ----

    #[test]
    fn a_lease_says_whether_it_was_just_opened() {
        let s = Store::open_memory().unwrap();
        s.put_profile_checked(TEAM, "p1", "Shop", "", b"x", w("m1", "a"), None, anyone).unwrap().unwrap();
        let take = |dev: &str| s.lease_take(TEAM, "p1", "m1", dev, "Mac", 90, |_, _| Ok(())).unwrap();
        assert!(take("mac").unwrap().fresh);
        assert!(!take("mac").unwrap().fresh, "a renewal is not an open");
        assert!(matches!(take("pc"), Err(Refusal::Leased(_))));
        s.lease_release("p1", "m1", "mac").unwrap();
        assert!(take("pc").unwrap().fresh);
        let live = s.live_leases().unwrap();
        assert_eq!(live.len(), 1);
        assert_eq!((live[0].0.as_str(), live[0].1.device_id.as_str()), ("p1", "pc"));
    }

    #[test]
    fn profiles_by_id_come_only_from_the_space_asked() {
        let s = Store::open_memory().unwrap();
        let me = w("m1", "a");
        s.put_profile_checked(TEAM, "p1", "B", "", b"x", me, None, anyone).unwrap().unwrap();
        s.put_profile_checked(TEAM, "p2", "A", "", b"x", me, None, anyone).unwrap().unwrap();
        s.put_profile_checked(&personal_space("m1"), "own", "C", "", b"x", me, None, anyone).unwrap().unwrap();
        let got: Vec<String> = s
            .listed_in(TEAM, Some(&["p1", "own", "nope", "p2"]))
            .unwrap()
            .into_iter()
            .map(|l| l.profile.id)
            .collect();
        assert_eq!(got, vec!["p2".to_string(), "p1".to_string()], "by name, team only");
        assert!(s.listed_in(TEAM, Some(&[])).unwrap().is_empty());
        assert_eq!(s.listed_in(TEAM, None).unwrap().len(), 2);
    }

    #[test]
    fn switching_off_or_removing_says_what_was_freed() {
        let s = Store::open_memory().unwrap();
        s.add_member("m1", "Vera", "h1").unwrap();
        s.lease_acquire("p1", "m1", "mac", "Mac", 90).unwrap().unwrap();
        s.lease_acquire("p2", "m1", "mac", "Mac", 0).unwrap().unwrap();
        let (_, freed) = s.set_member_active("m1", false).unwrap();
        assert_eq!(freed, vec!["p1".to_string()], "only what was still open");
        assert!(s.set_member_active("m1", true).unwrap().1.is_empty());

        s.lease_acquire("p3", "m1", "mac", "Mac", 90).unwrap().unwrap();
        assert_eq!(s.delete_member("m1").unwrap(), Some(vec!["p3".to_string()]));
        assert_eq!(s.delete_member("m1").unwrap(), None);
    }

    #[cfg(unix)]
    #[test]
    fn the_database_is_readable_by_the_server_alone() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("shardx-store-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;

        // One left by an older server, readable by everyone.
        let old = dir.join("old.sqlite");
        std::fs::write(&old, b"").unwrap();
        std::fs::set_permissions(&old, std::fs::Permissions::from_mode(0o644)).unwrap();
        drop(Store::open(&old, Vault::for_tests()).unwrap());
        assert_eq!(mode(&old), 0o600);

        let db = dir.join("team.sqlite");
        let s = Store::open(&db, Vault::for_tests()).unwrap();
        s.meta_set_all(&[("admin_secret", "x")]).unwrap();
        for suffix in ["", "-wal", "-shm"] {
            let p = std::path::PathBuf::from(format!("{}{suffix}", db.display()));
            assert_eq!(mode(&p), 0o600, "{}", p.display());
        }
        drop(s);
        let _ = std::fs::remove_dir_all(&dir);
    }

    // ---- sessions, cards, items and packages ----

    fn sealed_bytes(s: &Store, sql: &str, id: &str) -> Vec<u8> {
        s.lock().query_row(sql, params![id], |r| r.get(0)).unwrap()
    }

    fn contains(hay: &[u8], needle: &[u8]) -> bool {
        hay.windows(needle.len()).any(|w| w == needle)
    }

    /// No rights check: what decides is the lease.
    fn on_lease(_: &Standing, _: &TeamProfile, _: bool) -> std::result::Result<(), Refusal> {
        Ok(())
    }

    fn front(h: &SessionHead) -> Result<Vec<u8>> {
        Ok(format!("rev={};", h.session.as_ref().map_or(0, |m| m.rev)).into_bytes())
    }

    #[test]
    fn a_session_is_its_own_object_moved_only_by_the_device_holding_the_lease() {
        let s = Store::open_memory().unwrap();
        s.put_profile_checked(TEAM, "p1", "Shop", "", b"settings", w("m1", "laptop"), None, anyone).unwrap().unwrap();
        let read = |who: Writer<'_>| s.session_read(TEAM, "p1", who, |_, _| Ok(()), front).unwrap();
        let session = b"cookies: sid=SECRET-SESSION-VALUE".to_vec();

        // Nobody has it open: neither direction moves.
        assert_eq!(read(w("m1", "laptop")), Err(Refusal::NoLease));
        let r = s.session_write(TEAM, "p1", w("m1", "laptop"), 0, session.clone(), on_lease).unwrap();
        assert_eq!(r, Err(Refusal::NoLease));

        s.lease_acquire("p1", "m1", "laptop", "Vera's Mac", 90).unwrap().unwrap();
        // Another device, or a script with none: told who has it.
        for who in [w("m1", "desktop"), Writer { member_id: "m1", device_id: None }] {
            assert!(matches!(read(who), Err(Refusal::Leased(_))));
            let r = s.session_write(TEAM, "p1", who, 0, session.clone(), on_lease).unwrap();
            assert!(matches!(r, Err(Refusal::Leased(l)) if l.device_name == "Vera's Mac"));
        }
        // Nothing uploaded yet reads as revision 0 and no bytes.
        assert_eq!(read(w("m1", "laptop")).unwrap(), b"rev=0;");

        let (_, meta) = s.session_write(TEAM, "p1", w("m1", "laptop"), 0, session.clone(), on_lease).unwrap().unwrap();
        assert_eq!((meta.rev, meta.size, meta.updated_by.as_str()), (1, session.len() as u64, "m1"));
        // Based on what it no longer is: stale, and told what it is now.
        let r = s.session_write(TEAM, "p1", w("m1", "laptop"), 0, b"older".to_vec(), on_lease).unwrap();
        assert_eq!(r, Err(Refusal::SessionStale { current: 1 }));
        let (_, meta) = s.session_write(TEAM, "p1", w("m1", "laptop"), 1, session.clone(), on_lease).unwrap().unwrap();
        assert_eq!(meta.rev, 2);

        let mut want = b"rev=2;".to_vec();
        want.extend_from_slice(&session);
        assert_eq!(read(w("m1", "laptop")).unwrap(), want, "the front, then the bytes as sent");
        // Sealed at rest, bound to its own row.
        let at_rest = s.session_sealed("p1").unwrap().unwrap();
        assert!(!contains(&at_rest, b"SECRET-SESSION-VALUE"));

        // Its revision is not the profile's: a settings save made before is not stale.
        let p = s.put_profile_checked(TEAM, "p1", "Shop", "", b"settings2", w("m1", "laptop"), Some(1), anyone).unwrap().unwrap();
        assert_eq!(p.revision, 2);
        let listed = s.listed_in(TEAM, None).unwrap();
        assert_eq!((listed[0].profile.revision, listed[0].session.as_ref().unwrap().rev), (2, 2));
    }

    #[test]
    fn a_removed_or_switched_off_members_upload_is_refused_even_under_a_live_lease() {
        let s = Store::open_memory().unwrap();
        s.add_member("m1", "Vera", "h1").unwrap();
        s.put_profile_checked(TEAM, "p1", "Shop", "", b"x", w("m1", "laptop"), None, anyone).unwrap().unwrap();
        let judged = |st: &Standing, _: &TeamProfile, _: bool| -> std::result::Result<(), Refusal> {
            match (st.known, st.active) {
                (false, _) => Err(Refusal::Removed),
                (true, false) => Err(Refusal::SwitchedOff),
                _ => Ok(()),
            }
        };

        s.set_member_active("m1", false).unwrap();
        // A lease that outlived the switch-off (taken just before, say).
        s.lease_acquire("p1", "m1", "laptop", "Mac", 90).unwrap().unwrap();
        let r = s.session_write(TEAM, "p1", w("m1", "laptop"), 0, b"x".to_vec(), judged).unwrap();
        assert_eq!(r, Err(Refusal::SwitchedOff));

        s.delete_member("m1").unwrap();
        s.lease_acquire("p1", "m1", "laptop", "Mac", 90).unwrap().unwrap();
        let r = s.session_write(TEAM, "p1", w("m1", "laptop"), 0, b"x".to_vec(), judged).unwrap();
        assert_eq!(r, Err(Refusal::Removed));
        assert!(s.session_sealed("p1").unwrap().is_none(), "nothing was kept");
    }

    #[test]
    fn a_session_waits_in_the_trash_with_its_profile_and_goes_when_the_profile_does() {
        let s = Store::open_memory().unwrap();
        let me = w("m1", "laptop");
        let upload = |space: &str, id: &str| {
            s.lease_acquire(id, "m1", "laptop", "Mac", 90).unwrap().unwrap();
            s.session_write(space, id, me, 0, b"jar".to_vec(), on_lease).unwrap().unwrap();
            s.lease_release(id, "m1", "laptop").unwrap();
        };
        s.put_profile_checked(TEAM, "p1", "Shop", "", b"x", me, None, anyone).unwrap().unwrap();
        upload(TEAM, "p1");

        s.delete_profile_checked(TEAM, "p1", me, None, |_, _| Ok(())).unwrap().unwrap();
        assert!(s.session_sealed("p1").unwrap().is_some(), "kept for a restore");
        s.restore_from_trash("p1", me, |_, _| Ok(())).unwrap().unwrap();
        s.lease_acquire("p1", "m1", "laptop", "Mac", 90).unwrap().unwrap();
        assert_eq!(s.session_read(TEAM, "p1", me, |_, _| Ok(()), |_| Ok(Vec::new())).unwrap().unwrap(), b"jar");
        s.lease_release("p1", "m1", "laptop").unwrap();

        // Purged by hand, or out of days: the session goes with it.
        s.delete_profile_checked(TEAM, "p1", me, None, |_, _| Ok(())).unwrap().unwrap();
        s.purge_from_trash("p1", "m1", |_, _| Ok(())).unwrap().unwrap();
        assert!(s.session_sealed("p1").unwrap().is_none());
        s.put_profile_checked(TEAM, "p2", "Old", "", b"x", me, None, anyone).unwrap().unwrap();
        upload(TEAM, "p2");
        s.delete_profile_checked(TEAM, "p2", me, None, |_, _| Ok(())).unwrap().unwrap();
        s.backdate_trash("p2", 10).unwrap();
        assert_eq!(s.expire_trash(20).unwrap().len(), 1);
        assert!(s.session_sealed("p2").unwrap().is_none());

        // In a member's own space a delete is final, session and all.
        let mine = personal_space("m1");
        s.put_profile_checked(&mine, "own", "Own", "", b"x", me, None, anyone).unwrap().unwrap();
        upload(&mine, "own");
        s.delete_profile_checked(&mine, "own", me, None, |_, _| Ok(())).unwrap().unwrap();
        assert!(s.session_sealed("own").unwrap().is_none());
    }

    #[test]
    fn a_session_moves_with_its_profile_into_the_team() {
        let s = Store::open_memory().unwrap();
        let me = w("m1", "laptop");
        let mine = personal_space("m1");
        s.put_profile_checked(&mine, "p1", "Mine", "", b"x", me, None, anyone).unwrap().unwrap();
        s.lease_acquire("p1", "m1", "laptop", "Mac", 90).unwrap().unwrap();
        s.session_write(&mine, "p1", me, 0, b"jar".to_vec(), on_lease).unwrap().unwrap();
        s.move_profile("p1", &mine, TEAM, "", me, |_, _, _| Ok(())).unwrap().unwrap();

        assert_eq!(s.session_read(&mine, "p1", me, |_, _| Ok(()), front).unwrap(), Err(Refusal::OtherSpace));
        let got = s.session_read(TEAM, "p1", me, |_, _| Ok(()), front).unwrap().unwrap();
        assert_eq!(got, b"rev=1;jar");
    }

    fn card(proxy: Option<&str>, notes: &str) -> Card {
        Card { proxy_id: proxy.map(str::to_owned), color: "#c33".into(), notes: notes.into(), tags: vec!["ads".into()] }
    }

    #[test]
    fn a_card_is_sealed_kept_when_absent_and_binds_only_a_proxy_of_its_own_space() {
        let s = Store::open_memory().unwrap();
        let me = w("m1", "a");
        let mine = personal_space("m1");
        s.put_proxy_checked(TEAM, "x1", "DC", "socks5", "JP", b"login", me, None, anyone).unwrap().unwrap();
        s.put_proxy_checked(&mine, "x-own", "Home", "http", "", b"own", me, None, anyone).unwrap().unwrap();
        let put = |space: &str, id: &str, c: Option<&Card>, seen: &mut Vec<Option<String>>| {
            let fields = ProfileFields { name: "Shop", folder: "", data: b"x", card: c, created_at: None };
            s.put_profile_with(space, id, &fields, me, None, |_, _, found| {
                seen.push(found.bound.map(str::to_owned));
                Ok(())
            })
            .unwrap()
        };
        let mut seen = Vec::new();

        let c1 = card(Some("x1"), "login is in the notes: hunter2");
        put(TEAM, "p1", Some(&c1), &mut seen).unwrap();
        assert_eq!(s.listed_in(TEAM, None).unwrap()[0].card, c1);
        let at_rest = sealed_bytes(&s, "SELECT card FROM profiles WHERE id = ?1", "p1");
        assert!(!contains(&at_rest, b"hunter2"), "notes are sealed at rest");

        // No card on a write keeps the one stored; the same binding is not asked about again.
        put(TEAM, "p1", None, &mut seen).unwrap();
        assert_eq!(s.listed_one("p1").unwrap().unwrap().0.card, c1);
        put(TEAM, "p1", Some(&card(Some("x1"), "new notes")), &mut seen).unwrap();
        assert_eq!(seen, vec![Some("x1".to_string()), None, None]);

        // A proxy from another space, or none at all, is not bound.
        for bad in ["x-own", "x-nope"] {
            let r = put(TEAM, "p1", Some(&card(Some(bad), "")), &mut seen);
            assert_eq!(r, Err(Refusal::BadProxy), "{bad}");
        }
        put(&mine, "own", Some(&card(Some("x-own"), "")), &mut seen).unwrap();
        assert_eq!(put(&mine, "own", Some(&card(Some("x1"), "")), &mut seen), Err(Refusal::BadProxy));

        // A binding kept while its proxy went is still kept; the trash keeps the card too.
        s.delete_proxy_checked(TEAM, "x1", me, None, |_, _| Ok(())).unwrap().unwrap();
        put(TEAM, "p1", Some(&card(Some("x1"), "kept")), &mut seen).unwrap();
        s.delete_profile_checked(TEAM, "p1", me, None, |_, _| Ok(())).unwrap().unwrap();
        s.restore_from_trash("p1", me, |_, _| Ok(())).unwrap().unwrap();
        assert_eq!(s.listed_one("p1").unwrap().unwrap().0.card.notes, "kept");
    }

    #[test]
    fn the_bound_proxy_comes_with_the_session_only_from_the_profiles_own_space() {
        let s = Store::open_memory().unwrap();
        let me = w("m1", "a");
        s.put_proxy_checked(TEAM, "x1", "DC", "socks5", "JP", b"user:pass@host", me, None, anyone).unwrap().unwrap();
        let fields = ProfileFields { name: "Shop", folder: "", data: b"x", card: Some(&card(Some("x1"), "")), created_at: None };
        s.put_profile_with(TEAM, "p1", &fields, me, None, |_, _, _| Ok(())).unwrap().unwrap();
        s.lease_acquire("p1", "m1", "a", "Mac", 90).unwrap().unwrap();

        let mut found = None;
        s.session_read(TEAM, "p1", me, |_, _| Ok(()), |h| {
            found = Some(h.clone());
            Ok(Vec::new())
        })
        .unwrap()
        .unwrap();
        let h = found.unwrap();
        assert_eq!(h.proxy_id.as_deref(), Some("x1"));
        let (x, data) = h.proxy.unwrap();
        assert_eq!((x.id.as_str(), data.as_slice()), ("x1", b"user:pass@host".as_slice()));

        // Taken out of the team, it no longer comes with the team's profile.
        s.move_proxy("x1", TEAM, &personal_space("m1"), me, |_, _, _| Ok(())).unwrap().unwrap();
        s.session_read(TEAM, "p1", me, |_, _| Ok(()), |h| {
            assert_eq!((h.proxy_id.as_deref(), h.proxy.is_none()), (Some("x1"), true));
            Ok(Vec::new())
        })
        .unwrap()
        .unwrap();
    }

    #[test]
    fn an_older_database_gains_the_card_columns_and_reads_an_empty_card() {
        let dir = std::env::temp_dir().join(format!("shardx-store-old-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("team.sqlite");
        {
            let db = Connection::open(&path).unwrap();
            db.execute_batch(
                "CREATE TABLE profiles (id TEXT PRIMARY KEY, space TEXT NOT NULL DEFAULT 'team',
                    name TEXT NOT NULL, folder TEXT NOT NULL DEFAULT '', revision INTEGER NOT NULL DEFAULT 1,
                    updated_at INTEGER NOT NULL, updated_by TEXT NOT NULL, sealed BLOB NOT NULL);
                 INSERT INTO profiles(id, name, updated_at, updated_by, sealed) VALUES ('p1', 'Old', 1, 'm1', x'00');",
            )
            .unwrap();
        }
        let s = Store::open(&path, Vault::for_tests()).unwrap();
        let l = s.listed_in(TEAM, None).unwrap();
        assert_eq!((l[0].profile.name.as_str(), &l[0].card, &l[0].session), ("Old", &Card::default(), &None));
        drop(s);
        // And opening again changes nothing.
        drop(Store::open(&path, Vault::for_tests()).unwrap());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn items_live_per_space_with_revisions_and_wait_in_the_team_trash() {
        let s = Store::open_memory().unwrap();
        let me = w("m1", "a");
        let mine = personal_space("m1");
        let ok = |_: &Standing, _: Option<&Item>| Ok(());
        // One id per kind per space: the team's b1 and mine are two items.
        // The trash knows every kind the routes take.
        for k in crate::model::ITEM_KINDS {
            assert_eq!(TrashKind::of_item(k).map(TrashKind::as_str), Some(k));
        }
        let t = s.put_item_checked(TEAM, "bookmark", "b1", "Docs", b"https://team.example/SECRET", me, None, ok).unwrap().unwrap();
        s.put_item_checked(&mine, "bookmark", "b1", "Mine", b"https://mine.example", me, None, ok).unwrap().unwrap();
        s.put_item_checked(TEAM, "tag", "b1", "A tag", b"{}", me, None, ok).unwrap().unwrap();
        assert_eq!((t.revision, t.kind.as_str()), (1, "bookmark"));

        let team = s.items_in(TEAM, "bookmark").unwrap();
        assert_eq!((team.len(), team[0].data.as_slice()), (1, b"https://team.example/SECRET".as_slice()));
        assert_eq!(s.item_get(&mine, "bookmark", "b1").unwrap().unwrap().item.name, "Mine");
        let at_rest: Vec<u8> = s.lock().query_row(
            "SELECT sealed FROM items WHERE space = 'team' AND kind = 'bookmark' AND id = 'b1'", [], |r| r.get(0)).unwrap();
        assert!(!contains(&at_rest, b"SECRET"));

        // Revisions as for a profile.
        let r = s.put_item_checked(TEAM, "bookmark", "b1", "Docs", b"v2", me, Some(1), ok).unwrap().unwrap();
        assert_eq!(r.revision, 2);
        let r = s.put_item_checked(TEAM, "bookmark", "b1", "Docs", b"v1-late", me, Some(1), ok).unwrap();
        assert_eq!(r, Err(Refusal::Stale { current: 2 }));
        let r = s.delete_item_checked(TEAM, "bookmark", "b1", me, Some(1), |_, _| Ok(())).unwrap();
        assert_eq!(r, Err(Refusal::Stale { current: 2 }));

        // From the team into the trash, where its id stays taken.
        s.delete_item_checked(TEAM, "bookmark", "b1", me, Some(2), |_, _| Ok(())).unwrap().unwrap();
        assert!(s.items_in(TEAM, "bookmark").unwrap().is_empty());
        assert!(s.item_get(TEAM, "bookmark", "b1").unwrap().is_none());
        let trash = s.trash().unwrap();
        assert_eq!((trash.len(), trash[0].kind, trash[0].name.as_str()), (1, TrashKind::Bookmark, "Docs"));
        let r = s.put_item_checked(TEAM, "bookmark", "b1", "New", b"x", me, None, ok).unwrap();
        assert!(matches!(r, Err(Refusal::InTrash(ref t)) if t.kind == TrashKind::Bookmark), "{r:?}");
        assert_eq!(s.restore_item("tag", "b1", me, |_, _| Ok(())).unwrap(), Err(Refusal::Missing), "by kind");
        assert_eq!(s.restore_from_trash("b1", me, |_, _| Ok(())).unwrap(), Err(Refusal::Missing), "not a profile");

        let back = s.restore_item("bookmark", "b1", w("m2", "b"), |_, _| Ok(())).unwrap().unwrap();
        assert_eq!((back.revision, back.updated_by.as_str()), (3, "m2"));
        assert_eq!(s.items_in(TEAM, "bookmark").unwrap()[0].data, b"v2");

        // Purged by hand, then by days.
        s.delete_item_checked(TEAM, "bookmark", "b1", me, None, |_, _| Ok(())).unwrap().unwrap();
        assert_eq!(s.purge_item("bookmark", "b1", "m1", |_, _| Err(Refusal::Forbidden)).unwrap(), Err(Refusal::Forbidden));
        s.purge_item("bookmark", "b1", "m1", |_, _| Ok(())).unwrap().unwrap();
        assert!(s.trash().unwrap().is_empty());
        s.delete_item_checked(TEAM, "tag", "b1", me, None, |_, _| Ok(())).unwrap().unwrap();
        s.backdate_trash("b1", 10).unwrap();
        let gone = s.expire_trash(20).unwrap();
        assert_eq!((gone.len(), gone[0].kind), (1, TrashKind::Tag));
        assert!(s.trash().unwrap().is_empty());

        // Mine: a delete is final.
        s.delete_item_checked(&mine, "bookmark", "b1", me, None, |_, _| Ok(())).unwrap().unwrap();
        assert!(s.item_get(&mine, "bookmark", "b1").unwrap().is_none());
        assert!(s.trash().unwrap().is_empty());
    }

    #[test]
    fn an_extensions_package_is_a_sealed_blob_of_its_own() {
        let s = Store::open_memory().unwrap();
        let me = w("m1", "a");
        let mine = personal_space("m1");
        let ok = |_: &Standing, _: Option<&Item>| Ok(());
        let any = |_: &Standing, _: bool, _: &Item| Ok(());
        s.put_item_checked(TEAM, "extension", "e1", "uBlock", b"{}", me, None, ok).unwrap().unwrap();
        s.put_item_checked(TEAM, "bookmark", "b1", "Docs", b"{}", me, None, ok).unwrap().unwrap();

        // Only a live extension has one.
        for (space, id) in [(TEAM, "b1"), (TEAM, "nope"), (mine.as_str(), "e1")] {
            assert_eq!(s.package_write(space, id, me, None, b"zip".to_vec(), any).unwrap(), Err(Refusal::Missing));
        }
        let mut had = Vec::new();
        let mut seen = |_: &Standing, has: bool, _: &Item| {
            had.push(has);
            Ok(())
        };
        let m = s.package_write(TEAM, "e1", me, Some(0), b"PK-SECRET-1".to_vec(), &mut seen).unwrap().unwrap();
        assert_eq!((m.rev, m.size), (1, 11));
        let r = s.package_write(TEAM, "e1", me, Some(0), b"late".to_vec(), &mut seen).unwrap();
        assert_eq!(r, Err(Refusal::PackageStale { current: 1 }));
        s.package_write(TEAM, "e1", me, None, b"PK-SECRET-2".to_vec(), &mut seen).unwrap().unwrap();
        assert_eq!(had, vec![false, true, true]);

        let (meta, bytes) = s.package_read(TEAM, "e1", "m1", |_| Ok(())).unwrap().unwrap();
        assert_eq!((meta.rev, bytes.as_slice()), (2, b"PK-SECRET-2".as_slice()));
        let at_rest: Vec<u8> = s.lock().query_row(
            "SELECT c.sealed FROM blobs b JOIN chunks c ON c.body = b.body WHERE b.kind = 'package' AND b.id = 'e1'",
            [], |r| r.get(0)).unwrap();
        assert!(!contains(&at_rest, b"SECRET"));
        assert_eq!(s.items_in(TEAM, "extension").unwrap()[0].package.as_ref().unwrap().rev, 2);
        assert!(s.items_in(TEAM, "bookmark").unwrap()[0].package.is_none());

        // It waits in the trash with its extension, and goes when that is purged.
        s.delete_item_checked(TEAM, "extension", "e1", me, None, |_, _| Ok(())).unwrap().unwrap();
        assert_eq!(s.package_read(TEAM, "e1", "m1", |_| Ok(())).unwrap(), Err(Refusal::Missing));
        s.restore_item("extension", "e1", me, |_, _| Ok(())).unwrap().unwrap();
        assert!(s.package_read(TEAM, "e1", "m1", |_| Ok(())).unwrap().is_ok());
        s.delete_item_checked(TEAM, "extension", "e1", me, None, |_, _| Ok(())).unwrap().unwrap();
        s.purge_item("extension", "e1", "m1", |_, _| Ok(())).unwrap().unwrap();
        let left: i64 = s.lock().query_row("SELECT COUNT(*) FROM blobs", [], |r| r.get(0)).unwrap();
        assert_eq!(left, 0);
        s.collect_bodies().unwrap();
        assert_eq!(s.chunk_count(), 0, "and its chunks after");

        // Mine: deleting the extension takes its package along at once.
        s.put_item_checked(&mine, "extension", "e1", "Own", b"{}", me, None, ok).unwrap().unwrap();
        s.package_write(&mine, "e1", me, None, b"own zip".to_vec(), any).unwrap().unwrap();
        s.delete_item_checked(&mine, "extension", "e1", me, None, |_, _| Ok(())).unwrap().unwrap();
        let left: i64 = s.lock().query_row("SELECT COUNT(*) FROM blobs", [], |r| r.get(0)).unwrap();
        assert_eq!(left, 0);
        s.collect_bodies().unwrap();
        assert_eq!(s.chunk_count(), 0);
    }

    #[test]
    fn a_large_session_is_written_and_read_without_holding_the_store_for_it() {
        let dir = std::env::temp_dir().join(format!("shardx-store-big-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let s = Store::open(&dir.join("team.sqlite"), Vault::for_tests()).unwrap();
        let me = w("m1", "a");
        s.put_profile_checked(TEAM, "p1", "Shop", "", b"x", me, None, anyone).unwrap().unwrap();
        s.lease_acquire("p1", "m1", "a", "Mac", 90).unwrap().unwrap();
        let chunks = 24;
        let data: Vec<u8> = (0..chunks * CHUNK).map(|i| (i % 251) as u8).collect();

        // Every other request waits while the store is held: so it is let go
        // after every chunk, never held for the whole session.
        s.holds();
        s.session_write(TEAM, "p1", me, 0, data.clone(), on_lease).unwrap().unwrap();
        let count = s.holds();
        assert!(count > chunks as u64, "writing: {count} holds for {chunks} chunks");
        let got = s.session_read(TEAM, "p1", me, |_, _| Ok(()), |_| Ok(Vec::new())).unwrap().unwrap();
        let count = s.holds();
        assert!(count > chunks as u64, "reading: {count} holds for {chunks} chunks");
        assert!(got == data);
        drop(s);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_body_nothing_points_at_is_freed_later_and_never_under_a_reader() {
        let s = Store::open_memory().unwrap();
        let me = w("m1", "a");
        s.put_profile_checked(TEAM, "p1", "Shop", "", b"x", me, None, anyone).unwrap().unwrap();
        s.lease_acquire("p1", "m1", "a", "Mac", 90).unwrap().unwrap();
        let big = vec![1u8; 40 * CHUNK + 5];
        s.session_write(TEAM, "p1", me, 0, big.clone(), on_lease).unwrap().unwrap();

        // A download begins on it, and a newer upload replaces it meanwhile.
        let (_, body) = s.session_open(TEAM, "p1", me, |_, _| Ok(())).unwrap().unwrap();
        let body = body.unwrap();
        assert_eq!(body.chunks(), 41);
        s.session_write(TEAM, "p1", me, 1, b"new".to_vec(), on_lease).unwrap().unwrap();
        assert_eq!(s.collect_bodies().unwrap(), 0, "still being read");
        let mut got = Vec::new();
        s.read_body(&body, &mut got).unwrap();
        assert!(got == big, "the reader gets what it began on");

        // Let go, it is freed a batch at a time, not in one long hold.
        s.let_go(body.body);
        s.holds();
        assert_eq!(s.collect_bodies().unwrap(), 41);
        assert!(s.holds() >= 41 / COLLECT_BATCH as u64, "a batch per hold");
        assert_eq!(s.chunk_count(), 1, "the live session's is kept");

        // Purged: the session goes from the lists at once, its chunks with the collector.
        s.lease_release("p1", "m1", "a").unwrap();
        s.delete_profile_checked(TEAM, "p1", me, None, |_, _| Ok(())).unwrap().unwrap();
        s.purge_from_trash("p1", "m1", |_, _| Ok(())).unwrap().unwrap();
        assert!(s.session_sealed("p1").unwrap().is_none());
        assert_eq!(s.collect_bodies().unwrap(), 1);
        assert_eq!(s.chunk_count(), 0);

        // An upload still coming in is left alone; cut short, it goes.
        let mut up = s.session_upload("p2").unwrap();
        s.upload_put(&mut up, &vec![0u8; CHUNK], false).unwrap();
        assert!(s.upload_put(&mut up, b"short", false).is_err(), "only whole chunks before the last");
        assert_eq!(s.collect_bodies().unwrap(), 0);
        s.let_go(up.body);
        assert_eq!(s.collect_bodies().unwrap(), 1);
        assert_eq!(s.chunk_count(), 0);
    }

    #[test]
    fn a_body_opens_only_where_and_as_it_was_written() {
        let s = Store::open_memory().unwrap();
        let me = w("m1", "a");
        for id in ["p1", "p2"] {
            s.put_profile_checked(TEAM, id, "Shop", "", b"x", me, None, anyone).unwrap().unwrap();
            s.lease_acquire(id, "m1", "a", "Mac", 90).unwrap().unwrap();
        }
        let data: Vec<u8> = (0..2 * CHUNK + 7).map(|i| (i % 253) as u8).collect();
        s.session_write(TEAM, "p1", me, 0, data.clone(), on_lease).unwrap().unwrap();
        s.session_write(TEAM, "p2", me, 0, data.clone(), on_lease).unwrap().unwrap();
        let open = |id: &str| s.session_open(TEAM, id, me, |_, _| Ok(())).unwrap().unwrap().1.unwrap();
        let (one, two) = (open("p1"), open("p2"));
        let sql = |q: &str| s.lock().execute_batch(q).unwrap();

        // Chunks swapped in place, or one of another session's: neither opens.
        sql(&format!(
            "UPDATE chunks SET seq = 9 WHERE body = {b} AND seq = 0;
             UPDATE chunks SET seq = 0 WHERE body = {b} AND seq = 1;
             UPDATE chunks SET seq = 1 WHERE body = {b} AND seq = 9;",
            b = one.body
        ));
        assert!(s.body_chunk(&one, 0).is_err() && s.body_chunk(&one, 1).is_err());
        assert_eq!(s.body_chunk(&two, 2).unwrap().len(), 7);
        sql(&format!(
            "DELETE FROM chunks WHERE body = {o} AND seq = 2;
             UPDATE chunks SET body = {o} WHERE body = {t} AND seq = 2;",
            o = one.body,
            t = two.body
        ));
        assert!(s.body_chunk(&one, 2).is_err());
        // Its end cut off: what is now last was not sealed as the last.
        let short = BodyRef { size: 2 * CHUNK as u64, ..two.clone() };
        assert!(s.body_chunk(&short, 1).is_err());
        assert_eq!(s.body_chunk(&two, 1).unwrap().len(), CHUNK);
        s.let_go(one.body);
        s.let_go(two.body);
    }

    #[test]
    fn blobs_an_older_server_kept_whole_move_into_chunks() {
        let dir = std::env::temp_dir().join(format!("shardx-store-whole-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let (path, key) = (dir.join("team.sqlite"), dir.join("server.key"));
        let me = w("m1", "a");
        {
            let s = Store::open(&path, Vault::open_or_create(&key).unwrap()).unwrap();
            s.put_profile_checked(TEAM, "p1", "Shop", "", b"x", me, None, anyone).unwrap().unwrap();
            s.put_item_checked(TEAM, "extension", "e1", "uBlock", b"{}", me, None, |_, _| Ok(())).unwrap().unwrap();
        }
        let jar: Vec<u8> = (0..CHUNK + 10).map(|i| (i % 241) as u8).collect();
        {
            // The shape an older server kept them in, one sealed row each.
            let v = Vault::open_or_create(&key).unwrap();
            let db = Connection::open(&path).unwrap();
            db.execute_batch(
                "DROP TABLE blobs; DROP TABLE chunks; DROP TABLE bodies;
                 CREATE TABLE blobs (kind TEXT NOT NULL, space TEXT NOT NULL, id TEXT NOT NULL,
                    rev INTEGER NOT NULL, size INTEGER NOT NULL, updated_at INTEGER NOT NULL,
                    updated_by TEXT NOT NULL, envelope BLOB NOT NULL, sealed BLOB NOT NULL,
                    PRIMARY KEY (kind, space, id));",
            )
            .unwrap();
            for (kind, space, id, aad, plain) in [
                ("session", "", "p1", "session:p1", jar.clone()),
                ("package", TEAM, "e1", "package:team:e1", b"zip".to_vec()),
            ] {
                let mut sealed = plain.clone();
                let env = v.seal_detached(aad, &mut sealed).unwrap();
                db.execute(
                    "INSERT INTO blobs VALUES (?1, ?2, ?3, 3, ?4, 1, 'm1', ?5, ?6)",
                    params![kind, space, id, plain.len() as i64, env, sealed],
                )
                .unwrap();
            }
        }
        // With the wrong key nothing opens, and nothing is changed: the next start tries again.
        assert!(Store::open(&path, Vault::for_tests()).is_err());
        assert!(has_column(&Connection::open(&path).unwrap(), "blobs", "envelope").unwrap());
        let s = Store::open(&path, Vault::open_or_create(&key).unwrap()).unwrap();
        s.lease_acquire("p1", "m1", "a", "Mac", 90).unwrap().unwrap();
        let mut want = b"rev=3;".to_vec();
        want.extend_from_slice(&jar);
        assert!(s.session_read(TEAM, "p1", me, |_, _| Ok(()), front).unwrap().unwrap() == want);
        let (meta, zip) = s.package_read(TEAM, "e1", "m1", |_| Ok(())).unwrap().unwrap();
        assert_eq!((meta.rev, zip.as_slice()), (3, b"zip".as_slice()));
        assert!(!has_column(&s.lock(), "blobs", "envelope").unwrap());
        assert_eq!(s.chunk_count(), 3);
        drop(s);
        // Opened again, nothing moves twice.
        let s = Store::open(&path, Vault::open_or_create(&key).unwrap()).unwrap();
        assert_eq!(s.chunk_count(), 3);
        drop(s);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_name_only_profiles_or_the_trash_still_carry_is_taken() {
        let s = Store::open_memory().unwrap();
        let me = w("m1", "a");
        // Written before folders had rows: names with no row behind them.
        s.put_profile("p-old", "Old", "Old", b"x", "m1").unwrap();
        s.put_profile("p-loose", "Loose", "Loose", b"x", "m1").unwrap();
        s.delete_profile_checked(TEAM, "p-old", me, None, |_, _| Ok(())).unwrap().unwrap();
        s.create_folder(TEAM, "X", "").unwrap();
        for taken in ["Old", "Loose"] {
            assert_eq!(s.rename_folder(TEAM, "X", taken).unwrap(), FolderRename::NameTaken, "{taken}");
        }
        assert!(matches!(s.rename_folder(TEAM, "X", "Fresh").unwrap(), FolderRename::Done));
    }

    #[test]
    fn a_member_is_seen_at_most_once_a_minute_and_not_before_their_first_request() {
        let s = Store::open_memory().unwrap();
        s.add_member("m1", "Vera", "h1").unwrap();
        let seen = |s: &Store| s.member("m1").unwrap().unwrap().last_seen_at;
        assert_eq!(seen(&s), None, "invited, never seen");

        s.seen_at("m1", "h1", 1_000).unwrap();
        assert_eq!(seen(&s), Some(1_000));
        s.holds();
        // Within the minute: not even the database is held.
        s.seen_at("m1", "h1", 1_059).unwrap();
        assert_eq!(s.holds(), 0);
        assert_eq!(seen(&s), Some(1_000));
        s.seen_at("m1", "h1", 1_060).unwrap();
        assert_eq!(seen(&s), Some(1_060));
        // A clock set back far is written too, rather than waited out.
        s.seen_at("m1", "h1", 60).unwrap();
        assert_eq!(seen(&s), Some(60));
        // Someone not there is not an error.
        s.seen_at("m-gone", "h9", 5_000).unwrap();
        assert_eq!(s.members().unwrap()[0].last_seen_at, Some(60));
    }

    #[test]
    fn a_reissued_token_replaces_the_old_one_which_is_remembered_until_the_member_goes() {
        let s = Store::open_memory().unwrap();
        s.add_member("m1", "Vera", "old").unwrap();
        s.set_grants("m1", &[grant(Scope::Team, Action::See, true)]).unwrap();
        s.seen_at("m1", "old", 1_000).unwrap();
        let before = s.member("m1").unwrap().unwrap();

        let m = s.reissue_token("m1", "new").unwrap().unwrap();
        assert_eq!((m.id.as_str(), m.name.as_str(), m.epoch, m.last_seen_at), ("m1", "Vera", before.epoch, None));
        assert!(s.member_by_token_hash("old").unwrap().is_none());
        assert_eq!(s.member_by_token_hash("new").unwrap().unwrap().id, "m1");
        assert!(s.token_replaced("old").unwrap());
        assert!(!s.token_replaced("new").unwrap() && !s.token_replaced("never").unwrap());
        // Their rules stay; and seen again at once, the minute starts afresh.
        assert_eq!(s.grants_of("m1").unwrap().len(), 1);
        s.seen_at("m1", "new", 1_010).unwrap();
        assert_eq!(s.member("m1").unwrap().unwrap().last_seen_at, Some(1_010));

        assert!(s.reissue_token("nobody", "x").unwrap().is_none());
        assert!(s.reissue_token(ADMIN_ID, "x").unwrap().is_none());

        // Removed: the old token is a stranger's again.
        s.delete_member("m1").unwrap().unwrap();
        assert!(!s.token_replaced("old").unwrap());
    }

    #[test]
    fn a_request_let_in_with_the_old_token_does_not_mark_the_new_invite_used() {
        let s = Store::open_memory().unwrap();
        s.add_member("m1", "Vera", "old").unwrap();
        // Let in with the old token; the re-issue lands before it is noted.
        s.reissue_token("m1", "new").unwrap().unwrap();
        s.seen_at("m1", "old", 1_000).unwrap();
        assert_eq!(s.member("m1").unwrap().unwrap().last_seen_at, None, "still only invited");
        // Nor does that note hold back the new token's first one.
        s.seen_at("m1", "new", 1_001).unwrap();
        assert_eq!(s.member("m1").unwrap().unwrap().last_seen_at, Some(1_001));
    }

    #[test]
    fn an_older_database_gains_last_seen_and_reads_its_members_as_never_seen() {
        let dir = std::env::temp_dir().join(format!("shardx-store-seen-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("team.sqlite");
        {
            let db = Connection::open(&path).unwrap();
            db.execute_batch(
                "CREATE TABLE members (id TEXT PRIMARY KEY, name TEXT NOT NULL, token_hash TEXT NOT NULL UNIQUE,
                    active INTEGER NOT NULL DEFAULT 1, created_at INTEGER NOT NULL, epoch INTEGER NOT NULL DEFAULT 1);
                 INSERT INTO members(id, name, token_hash, created_at) VALUES ('m1', 'Vera', 'h1', 1);",
            )
            .unwrap();
        }
        let s = Store::open(&path, Vault::for_tests()).unwrap();
        let m = s.member_by_token_hash("h1").unwrap().unwrap();
        assert_eq!((m.name.as_str(), m.last_seen_at), ("Vera", None));
        s.reissue_token("m1", "h2").unwrap().unwrap();
        assert!(s.token_replaced("h1").unwrap());
        drop(s);
        drop(Store::open(&path, Vault::for_tests()).unwrap());
        let _ = std::fs::remove_dir_all(&dir);
    }

    // ---- when a profile was made ----

    fn dated(created_at: Option<i64>) -> ProfileFields<'static> {
        ProfileFields { name: "Shop", folder: "", data: b"x", card: None, created_at }
    }

    fn put_dated(s: &Store, space: &str, id: &str, at: Option<i64>) -> TeamProfile {
        s.put_profile_with(space, id, &dated(at), w("m1", "a"), None, |_, _, _| Ok(())).unwrap().unwrap()
    }

    const MADE: i64 = 1_700_000_000;

    #[test]
    fn a_new_profile_keeps_its_writers_date_when_plausible_and_is_dated_now_otherwise() {
        let s = Store::open_memory().unwrap();
        assert_eq!(put_dated(&s, TEAM, "p1", Some(MADE)).created_at, Some(MADE as u64));
        // A clock a little ahead is still a clock.
        let soon = now() as i64 + 3600;
        assert_eq!(put_dated(&s, TEAM, "p2", Some(soon)).created_at, Some(soon as u64));
        let before = now();
        for (id, at) in [("p3", None), ("p4", Some(0)), ("p5", Some(-5)), ("p6", Some(now() as i64 + 3 * 86_400))] {
            let got = put_dated(&s, TEAM, id, at).created_at.unwrap();
            assert!((before..=now()).contains(&got), "{id}: {got}");
        }
        // Every read of the row carries it.
        let listed = s.listed_in(TEAM, Some(&["p1"])).unwrap();
        assert_eq!(listed[0].profile.created_at, Some(MADE as u64));
        assert_eq!(s.listed_one("p1").unwrap().unwrap().0.profile.created_at, Some(MADE as u64));
        assert_eq!(s.profile_meta("p1").unwrap().unwrap().0.created_at, Some(MADE as u64));
    }

    #[test]
    fn a_known_date_never_changes_on_an_edit() {
        let s = Store::open_memory().unwrap();
        put_dated(&s, TEAM, "p1", Some(MADE));
        for at in [Some(MADE + 500), None] {
            assert_eq!(put_dated(&s, TEAM, "p1", at).created_at, Some(MADE as u64));
        }
        // The test setup's own write dates a new row too, and keeps it.
        let p = s.put_profile("p2", "Shop", "", b"x", "m1").unwrap();
        assert_eq!(s.put_profile("p2", "Shop", "", b"y", "m1").unwrap().created_at, p.created_at);
    }

    #[test]
    fn a_row_with_no_date_takes_the_first_a_writer_knows_and_no_guess() {
        let s = Store::open_memory().unwrap();
        put_dated(&s, TEAM, "p1", None);
        // As an older server left it.
        s.lock().execute("UPDATE profiles SET created_at = NULL WHERE id = 'p1'", []).unwrap();
        // An older launcher's edit says nothing, and nothing is made up.
        assert_eq!(put_dated(&s, TEAM, "p1", None).created_at, None);
        assert_eq!(put_dated(&s, TEAM, "p1", Some(-1)).created_at, None);
        assert_eq!(put_dated(&s, TEAM, "p1", Some(MADE)).created_at, Some(MADE as u64));
        assert_eq!(put_dated(&s, TEAM, "p1", Some(MADE + 9)).created_at, Some(MADE as u64));
    }

    #[test]
    fn the_date_moves_with_the_profile_and_waits_in_the_trash() {
        let s = Store::open_memory().unwrap();
        let mine = personal_space("m1");
        put_dated(&s, &mine, "p1", Some(MADE));
        let moved = s.move_profile("p1", &mine, TEAM, "Ads", w("m1", "a"), |_, _, _| Ok(())).unwrap().unwrap();
        assert_eq!(moved.created_at, Some(MADE as u64));
        s.delete_profile_checked(TEAM, "p1", w("m1", "a"), None, |_, _| Ok(())).unwrap().unwrap();
        let Restored::Profile(back) = s.restore_from_trash("p1", w("m1", "a"), |_, _| Ok(())).unwrap().unwrap() else {
            panic!("a profile comes back as one")
        };
        assert_eq!((back.created_at, back.revision), (Some(MADE as u64), 3));
        let out = s.move_profile("p1", TEAM, &mine, "", w("m1", "a"), |_, _, _| Ok(())).unwrap().unwrap();
        assert_eq!(out.created_at, Some(MADE as u64));
    }

    #[test]
    fn an_older_database_gains_the_date_columns_and_leaves_its_rows_undated() {
        let dir = std::env::temp_dir().join(format!("shardx-store-made-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("team.sqlite");
        {
            let db = Connection::open(&path).unwrap();
            db.execute_batch(
                "CREATE TABLE profiles (id TEXT PRIMARY KEY, space TEXT NOT NULL DEFAULT 'team',
                    name TEXT NOT NULL, folder TEXT NOT NULL DEFAULT '', revision INTEGER NOT NULL DEFAULT 1,
                    updated_at INTEGER NOT NULL, updated_by TEXT NOT NULL, sealed BLOB NOT NULL, card BLOB);
                 CREATE TABLE trash (id TEXT PRIMARY KEY, kind TEXT NOT NULL, name TEXT NOT NULL,
                    folder TEXT NOT NULL DEFAULT '', proxy_kind TEXT NOT NULL DEFAULT '',
                    country TEXT NOT NULL DEFAULT '', revision INTEGER NOT NULL, updated_at INTEGER NOT NULL,
                    updated_by TEXT NOT NULL, deleted_at INTEGER NOT NULL, deleted_by TEXT NOT NULL,
                    sealed BLOB NOT NULL, card BLOB);
                 INSERT INTO profiles(id, name, updated_at, updated_by, sealed) VALUES ('p1', 'Old', 5, 'm1', x'00');
                 INSERT INTO trash(id, kind, name, revision, updated_at, updated_by, deleted_at, deleted_by, sealed)
                    VALUES ('p2', 'profile', 'Gone', 1, 5, 'm1', 6, 'm1', x'00');",
            )
            .unwrap();
        }
        let s = Store::open(&path, Vault::for_tests()).unwrap();
        // Not guessed from when it was last written.
        assert_eq!(s.listed_in(TEAM, None).unwrap()[0].profile.created_at, None);
        let Restored::Profile(back) = s.restore_from_trash("p2", w("m1", "a"), |_, _| Ok(())).unwrap().unwrap() else {
            panic!("a profile comes back as one")
        };
        assert_eq!(back.created_at, None);
        // The first writer who knows the date fills it in.
        assert_eq!(put_dated(&s, TEAM, "p1", Some(MADE)).created_at, Some(MADE as u64));
        drop(s);
        // And opening again changes nothing.
        let s = Store::open(&path, Vault::for_tests()).unwrap();
        assert_eq!(s.profile_meta("p1").unwrap().unwrap().0.created_at, Some(MADE as u64));
        drop(s);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
