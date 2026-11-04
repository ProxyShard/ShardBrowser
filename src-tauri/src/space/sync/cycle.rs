//! One sync, kind by kind, against what was last synced of each thing. A profile
//! changed on both sides is kept twice; anything else takes the server's, and says so.

use std::collections::{BTreeMap, BTreeSet};

use base64::{engine::general_purpose::STANDARD, Engine};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use team_model::Card;

use super::super::client::{Code, Method, TeamClient, TeamError};
use super::super::local;
use super::super::profile::StoredProfile;
use super::super::proxy::ProxyEntry;
use super::super::remote::{self, FolderRow, ItemRow, ItemWritten, ProfileRow, ProfileWithData, ProxyRow, ProxyWithData, Written};
use super::book::{self, Book, Entry, Held, Kinds, Op, Rec, Sess};
use super::{changed, note_failure, rows_changed, run};

mod extension;
#[cfg(test)]
pub(super) use extension::{ENGINE, FETCHED};

/// The largest extension package the server keeps.
const PACKAGE_MAX: u64 = 64 * 1024 * 1024;
/// The space's own document: what syncs, and which data this is.
const DOC: &str = "/me/items/sync/space";

/// One sync at a time; a disconnect waits for it.
pub(super) async fn lock() -> tokio::sync::MutexGuard<'static, ()> {
    static TURN: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
    TURN.lock().await
}

/// Why a sync stopped short.
pub(super) enum Stop {
    Team(TeamError),
    /// Another computer removed the synced data: nothing more goes either way.
    Removed,
    /// What is here could not be read: none of it is taken for deleted.
    Local(String),
}

fn unread(what: &str, e: anyhow::Error) -> Stop {
    Stop::Local(format!("{what} on this computer could not be read: {e:#}"))
}

/// Every synced profile gone from here at once, none through the trash.
const VANISHED: &str = "none of the synced profiles is on this computer any more, so nothing was \
                        deleted from the server: is the data folder where it was?";

impl From<TeamError> for Stop {
    fn from(e: TeamError) -> Self {
        Stop::Team(e)
    }
}

type Net<T> = Result<T, TeamError>;

/// No answer, or a token the server does not take: nothing more is tried now.
fn fatal(e: &TeamError) -> bool {
    matches!(e, TeamError::Offline(_)) || matches!(e.status(), Some(401 | 403))
}

/// A refusal of one thing is logged and left for the next sync; the rest goes on.
fn skip<T>(r: Net<T>, what: &str) -> Result<Option<T>, Stop> {
    match r {
        Ok(v) => Ok(Some(v)),
        Err(e) if fatal(&e) => Err(e.into()),
        Err(e) => {
            eprintln!("[sync] {what}: {e}");
            Ok(None)
        }
    }
}

type Change = Box<dyn FnOnce(&mut Book) + Send>;

/// Changes to the book a phase made, written once it ends however it ends.
#[derive(Default)]
struct Patch(Vec<Change>);

impl Patch {
    fn add(&mut self, f: impl FnOnce(&mut Book) + Send + 'static) {
        self.0.push(Box::new(f));
    }

    fn rec(&mut self, kind: &'static str, id: &str, rec: Option<Rec>) {
        let id = id.to_string();
        self.add(move |b| match rec {
            Some(r) => b.set_rec(kind, &id, r),
            None => b.drop_rec(kind, &id),
        });
    }

    /// Written to the book; `r` is how the phase ended, handed on.
    fn apply<T>(self, r: T) -> T {
        if !self.0.is_empty() {
            let written = book::change(|b| {
                for f in self.0 {
                    f(b);
                }
            });
            if let Err(e) = written {
                eprintln!("[sync] {e:#}");
            }
        }
        r
    }
}

/// What the person was told and which lists moved, said once the sync ends.
#[derive(Default)]
struct Told {
    warnings: Vec<String>,
    topics: BTreeSet<&'static str>,
}

// ---- digests ----

/// Keys in order, so one value always reads the same.
fn canon(v: &Value) -> Value {
    match v {
        Value::Object(m) => {
            let sorted: BTreeMap<&String, Value> = m.iter().map(|(k, v)| (k, canon(v))).collect();
            Value::Object(sorted.into_iter().map(|(k, v)| (k.clone(), v)).collect())
        }
        Value::Array(a) => Value::Array(a.iter().map(canon).collect()),
        v => v.clone(),
    }
}

fn digest(v: &Value) -> String {
    use sha2::{Digest, Sha256};
    let bytes = serde_json::to_vec(&canon(v)).unwrap_or_default();
    Sha256::digest(&bytes).iter().take(16).map(|b| format!("{b:02x}")).collect()
}

fn b64<T: Serialize>(v: &T) -> String {
    STANDARD.encode(serde_json::to_vec(v).unwrap_or_default())
}

fn opened(data: &str) -> Option<Value> {
    let bytes = STANDARD.decode(data.trim()).ok()?;
    serde_json::from_slice(&bytes).ok()
}

fn seg(id: &str) -> Net<String> {
    super::super::admin::seg(id).map_err(|e| TeamError::Garbled(e.to_string()))
}

// ---- the space's document ----

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Doc {
    generation: String,
    kinds: Kinds,
}

async fn read_doc(c: &TeamClient) -> Net<Option<(u64, Doc)>> {
    match c.get::<ItemRow>(DOC).await {
        Ok(row) => Ok(opened(&row.data).and_then(|v| serde_json::from_value(v).ok()).map(|d| (row.revision, d))),
        Err(e) if e.status() == Some(404) => Ok(None),
        Err(e) => Err(e),
    }
}

async fn write_doc(c: &TeamClient, doc: &Doc, base: u64) -> Net<u64> {
    let body = json!({ "name": "sync", "data": b64(doc), "base_revision": base });
    let w: ItemWritten = c.json(Method::PUT, DOC, Some(&body)).await?;
    Ok(w.revision)
}

/// Right after connecting: the space's document read, or made with this
/// computer's choice of what syncs when this is the first.
pub(super) async fn first_contact(c: &TeamClient) -> Net<()> {
    let found = match read_doc(c).await? {
        Some(d) => Some(d),
        None => {
            let doc = Doc { generation: uuid::Uuid::new_v4().simple().to_string(), kinds: book::read().kinds };
            match write_doc(c, &doc, 0).await {
                Ok(rev) => Some((rev, doc)),
                // Another computer made it a moment ago: theirs is the one.
                Err(e) if e.code() == Some(&Code::Stale) => read_doc(c).await?,
                Err(e) => return Err(e),
            }
        }
    };
    if let Some((rev, doc)) = found {
        let _ = book::change(|b| {
            b.generation = doc.generation;
            b.kinds = doc.kinds;
            b.doc_rev = rev;
            b.kinds_dirty = false;
            b.removed = false;
        });
    }
    Ok(())
}

/// The document as it stands: gone or made anew means the data was removed.
async fn doc(c: &TeamClient) -> Result<bool, Stop> {
    let b = book::read();
    if b.generation.is_empty() {
        first_contact(c).await?;
        return Ok(true);
    }
    let Some((rev, there)) = read_doc(c).await? else {
        let _ = book::change(|b| b.removed = true);
        return Err(Stop::Removed);
    };
    if there.generation != b.generation {
        let _ = book::change(|b| b.removed = true);
        return Err(Stop::Removed);
    }
    if b.kinds_dirty {
        let doc = Doc { generation: b.generation.clone(), kinds: b.kinds.clone() };
        match write_doc(c, &doc, b.doc_rev).await {
            Ok(rev) => {
                let sent = b.kinds.clone();
                let _ = book::change(|b| {
                    b.doc_rev = rev;
                    b.kinds_dirty = b.kinds != sent;
                });
                return Ok(false);
            }
            // Changed on another computer meanwhile: theirs stands.
            Err(e) if e.code() == Some(&Code::Stale) => {}
            Err(e) => return Err(e.into()),
        }
    }
    if rev != b.doc_rev || b.kinds_dirty {
        let moved = b.kinds.profiles != there.kinds.profiles;
        let _ = book::change(|b| {
            b.kinds = there.kinds;
            b.doc_rev = rev;
            b.kinds_dirty = false;
        });
        return Ok(moved);
    }
    Ok(false)
}

// ---- one sync ----

/// A sync, start to end, with the status told as it goes.
pub(super) async fn run_once() {
    let _turn = lock().await;
    if crate::team::sync_place().is_none() {
        return;
    }
    run().syncing = true;
    scan();
    changed();
    let result = match TeamClient::for_sync() {
        Ok(c) => cycle(&c).await,
        Err(e) => Err(Stop::Team(TeamError::Garbled(format!("{e:#}")))),
    };
    {
        let mut r = run();
        r.syncing = false;
        if result.is_ok() {
            r.offline = false;
            r.error = None;
        }
    }
    match &result {
        Err(Stop::Team(e)) => note_failure(e),
        Err(Stop::Local(why)) => {
            eprintln!("[sync] {why}");
            let mut r = run();
            r.offline = false;
            r.error = Some(why.clone());
        }
        _ => {}
    }
    scan();
    changed();
}

async fn cycle(c: &TeamClient) -> Result<(), Stop> {
    if book::read().removed {
        return Err(Stop::Removed);
    }
    let mut told = Told::default();
    if doc(c).await? {
        told.topics.insert("profiles");
    }
    let rows: Vec<ProfileRow> = c.get("/me/profiles").await?;
    let result = async {
        // Each phase's changes to the book are written however it ends.
        // Proxies and tags before the profiles that name them; folders after
        // the profiles have moved out of one deleted here.
        let kinds = book::read().kinds;
        let mut p = Patch::default();
        let r = proxies(c, &rows, &mut p, &mut told).await;
        p.apply(r)?;
        if kinds.folders_tags {
            let mut p = Patch::default();
            let r = items(c, ItemKind::Tag, &mut p, &mut told).await;
            p.apply(r)?;
        }
        let mut p = Patch::default();
        let r = profiles(c, &rows, &mut p, &mut told).await;
        p.apply(r)?;
        sessions().await?;
        if kinds.folders_tags {
            let mut p = Patch::default();
            let r = folders(c, &mut p, &mut told).await;
            p.apply(r)?;
        }
        for kind in ITEM_KINDS.into_iter().filter(|k| *k != ItemKind::Tag && k.on(&kinds)) {
            let mut p = Patch::default();
            let r = items(c, kind, &mut p, &mut told).await;
            p.apply(r)?;
        }
        Ok::<(), Stop>(())
    }
    .await;
    let _ = book::change(|b| {
        if result.is_ok() {
            b.last_sync_at = Some(super::now());
        }
    });
    for w in told.warnings {
        #[cfg(test)]
        super::tests::warned(&w);
        crate::notify_warning(w);
    }
    let topics: Vec<&str> = told.topics.into_iter().collect();
    rows_changed(&topics);
    result
}

// ---- profiles ----

/// What of a profile goes to the server, and a digest of it as it is here.
struct Up {
    name: String,
    folder: String,
    data: String,
    card: Card,
    hash: String,
}

/// The settings the server keeps: without what only this computer counts.
fn body_of(stored: &StoredProfile) -> Value {
    let mut s = stored.clone();
    s.meta.pinned = false;
    s.meta.last_launched_at = None;
    s.meta.total_runtime_ms = 0;
    s.meta.temporary = false;
    s.meta.inline_proxy = None;
    s.meta.rev = 0;
    serde_json::to_value(&s).unwrap_or_default()
}

fn up_of(stored: &StoredProfile, tags: Vec<String>) -> Up {
    let body = body_of(stored);
    let name = remote::name_of(stored);
    let folder = stored.meta.folder.clone();
    let card = remote::card(stored, tags);
    let hash = digest(&json!({ "name": name, "folder": folder, "data": body, "card": card }));
    Up { name, folder, data: b64(&body), card, hash }
}

/// This computer's own profiles, temporary ones left out. One whose file is
/// there but unreadable is left out too: [`unreadable`] says which.
fn local_profiles() -> Result<BTreeMap<String, StoredProfile>, Stop> {
    let rows = local::profile::list_all().map_err(|e| unread("the profiles", e))?;
    // A protected profile never syncs; one picked before it was locked is left alone too.
    Ok(rows
        .into_iter()
        .filter(|m| !m.protected)
        .filter_map(|m| local::profile::load_raw(&m.id).ok().map(|s| (m.id, s)))
        .collect())
}

/// Its file is here but did not read: neither changed nor deleted.
fn unreadable(id: &str, here: &BTreeMap<String, StoredProfile>) -> bool {
    !here.contains_key(id) && local::profile::exists(id)
}

/// Two or more synced profiles all gone, none of them through the trash: a
/// missing data folder far more likely than a choice.
fn all_vanished(recs: &BTreeMap<String, Rec>, here: &BTreeMap<String, StoredProfile>) -> bool {
    if recs.len() < 2 || recs.keys().any(|id| here.contains_key(id) || local::profile::exists(id)) {
        return false;
    }
    let trashed: BTreeSet<String> = local::trash::list().unwrap_or_default().into_iter().map(|e| e.id).collect();
    !recs.keys().any(|id| trashed.contains(id))
}

fn tags_of(kinds: &Kinds) -> Result<BTreeMap<String, Vec<String>>, Stop> {
    if !kinds.folders_tags {
        return Ok(BTreeMap::new());
    }
    local::tags::list().map(|t| t.by_profile).map_err(|e| unread("the tags", e))
}

fn up_here(id: &str, kinds: &Kinds) -> Option<Up> {
    let stored = local::profile::load_raw(id).ok()?;
    let mut tags = tags_of(kinds).ok()?;
    Some(up_of(&stored, tags.remove(id).unwrap_or_default()))
}

fn me_path(kind: &str, id: &str) -> Net<String> {
    Ok(format!("/me/{kind}/{}", seg(id)?))
}

async fn put_profile(c: &TeamClient, id: &str, up: &Up, base: u64) -> Net<u64> {
    let body = json!({
        "name": up.name, "folder": up.folder, "data": up.data, "base_revision": base, "card": up.card,
    });
    let w: Written = c.json(Method::PUT, &me_path("profiles", id)?, Some(&body)).await?;
    Ok(w.revision)
}

async fn delete_at(c: &TeamClient, path: &str, base: u64) -> Net<()> {
    c.call(Method::DELETE, &format!("{path}?base_revision={base}"), None).await.map(|_| ())
}

// ---- stop marks ----

const MARK: &str = "stopped.";

/// Beside a profile that left the server because one computer stopped syncing
/// it: the others keep their copy, where a deletion would put it in the trash.
fn stop_mark(id: &str) -> String {
    format!("{MARK}{}", super::super::open::safe(id))
}

async fn stop_marks(c: &TeamClient) -> Net<BTreeSet<String>> {
    let rows: Vec<Named> = c.get("/me/items/sync").await?;
    Ok(rows.into_iter().map(|r| r.id).filter(|id| id.starts_with(MARK)).collect())
}

async fn mark_stopped(c: &TeamClient, id: &str) -> Net<()> {
    let body = json!({ "name": "stopped", "data": b64(&json!({ "profile": id })) });
    c.call(Method::PUT, &me_path("items/sync", &stop_mark(id))?, Some(&body)).await.map(|_| ())
}

async fn unmark_stopped(c: &TeamClient, id: &str) -> Net<()> {
    gone(c.call(Method::DELETE, &me_path("items/sync", &stop_mark(id))?, None).await.map(|_| ()))
}

/// A browser of this computer's runs on the profile's data, or is opening or closing.
fn running_here(id: &str) -> bool {
    crate::process::Tracker::shared().is_running(id) || super::super::open::holds_mine(id)
}

/// The server's version, written over this computer's; how it reads here after.
async fn pull_profile(c: &TeamClient, id: &str, kinds: &Kinds) -> Result<Option<(u64, String)>, Stop> {
    let Some(got) = skip(c.get::<ProfileWithData>(&me_path("profiles", id)?).await, id)? else { return Ok(None) };
    let existing = local::profile::load_raw(id).ok();
    let mut s = remote::stored(&got.row, Some(&got.data));
    // What only this computer counts stays as it was here.
    s.meta.last_launched_at = existing.as_ref().and_then(|e| e.meta.last_launched_at.clone());
    s.meta.rev = existing.as_ref().map_or(1, |e| e.meta.rev);
    s.meta.pinned = existing.as_ref().is_some_and(|e| e.meta.pinned);
    s.meta.total_runtime_ms = existing.as_ref().map_or(0, |e| e.meta.total_runtime_ms);
    let written = local::profile::save_raw(&mut s).and_then(|()| {
        // A save keeps the old folder when the new one is none.
        if existing.is_some() {
            local::profile::set_folder(id, &got.row.folder)?;
        }
        if kinds.folders_tags {
            local::tags::assign(BTreeMap::from([(id.to_string(), got.row.card.tags.clone())]))?;
        }
        Ok(())
    });
    if let Err(e) = written {
        eprintln!("[sync] {id} not written here: {e:#}");
        return Ok(None);
    }
    Ok(up_here(id, kinds).map(|u| (got.row.revision, u.hash)))
}

/// Both versions kept: this computer's goes to a new profile beside it, with
/// its browser data when `with_data`, and the person is told.
pub(super) fn conflict_copy(id: &str, with_data: bool) -> anyhow::Result<String> {
    let mut s = local::profile::load_raw(id)?;
    let name = remote::name_of(&s);
    s.meta.id = String::new();
    s.meta.pinned = false;
    s.config.insert("name".into(), Value::String(format!("{name} (conflict copy)")));
    local::profile::save_raw(&mut s)?;
    let copy = s.meta.id.clone();
    if with_data {
        let root = crate::store::user_data_root()?;
        let (from, to) = (root.join(id), root.join(&copy));
        if from.exists() {
            let _ = std::fs::remove_dir_all(&to);
            std::fs::rename(&from, &to)?;
        }
    }
    if let Some(tags) = local::tags::list()?.by_profile.get(id).cloned() {
        local::tags::assign(BTreeMap::from([(copy.clone(), tags)]))?;
    }
    let c = copy.clone();
    book::change(move |b| {
        b.picked.insert(c.clone());
        b.conflicts.insert(c.clone());
        if with_data {
            b.sessions.insert(c, Sess { rev: 0, dirty: true });
        }
    })?;
    crate::notify_warning(format!(
        "{name} was changed on another computer too, so both versions are kept: \
         the changes made here are in “{name} (conflict copy)”."
    ));
    rows_changed(&["profiles"]);
    Ok(copy)
}

/// Whether the browser data here holds anything worth sending.
fn has_data(id: &str) -> bool {
    crate::store::user_data_root().is_ok_and(|r| r.join(id).join("Default").exists())
}

async fn profiles(c: &TeamClient, rows: &[ProfileRow], p: &mut Patch, told: &mut Told) -> Result<(), Stop> {
    let b = book::read();
    let mine = crate::team::device_id();
    // Who holds what, as the server says now.
    let held: BTreeMap<String, Held> = rows
        .iter()
        .filter_map(|r| {
            let l = r.open_on.as_ref().filter(|l| l.device_id != mine)?;
            Some((r.id.clone(), Held { device_id: l.device_id.clone(), device_name: l.device_name.clone(), expires_at: l.expires_at }))
        })
        .collect();
    if held != b.held {
        told.topics.insert("profiles");
        p.add(move |b| b.held = held);
    }
    let server: BTreeMap<&str, &ProfileRow> = rows.iter().map(|r| (r.id.as_str(), r)).collect();
    // Read after the rows: a mark goes up before its profile leaves.
    let marks = stop_marks(c).await?;
    let here = local_profiles()?;
    let mut tags = tags_of(&b.kinds)?;
    let none = BTreeMap::new();
    let recs = b.records.get("profile").unwrap_or(&none);
    if all_vanished(recs, &here) {
        return Err(Stop::Local(VANISHED.into()));
    }
    let ids: BTreeSet<String> = here.keys().cloned().chain(server.keys().map(|s| s.to_string())).chain(recs.keys().cloned()).collect();
    for id in &ids {
        let id = id.as_str();
        if unreadable(id, &here) {
            eprintln!("[sync] {id}: its file here did not read; left as it is");
            continue;
        }
        let (r, s, l) = (recs.get(id), server.get(id).copied(), here.get(id));
        let on = b.syncs(id);
        let marked = marks.contains(&stop_mark(id));
        let up = l.map(|l| up_of(l, tags.remove(id).unwrap_or_default()));
        let local_changed = matches!((r, &up), (Some(r), Some(u)) if r.hash != u.hash);
        let remote_changed = matches!((r, s), (Some(r), Some(s)) if r.rev != s.revision);
        // Its browser runs on its data here: nothing is moved or deleted under it now.
        let busy = running_here(id);
        match (r, s, &up) {
            (None, Some(_), Some(_)) if busy => eprintln!("[sync] {id}: open here; met after it closes"),
            (Some(_), None, Some(_)) if busy && on && !marked => {
                eprintln!("[sync] {id}: open here; left for a sync after it closes");
            }
            (None, None, Some(u)) if on => {
                if let Some(rev) = skip(put_profile(c, id, u, 0).await, id)? {
                    p.rec("profile", id, Some(Rec::of(rev, u.hash.clone())));
                    // Synced again: the other computers take it up again.
                    if marked {
                        skip(unmark_stopped(c, id).await, id)?;
                    }
                    // What its browser holds goes up too, though it is not opened.
                    if has_data(id) && !b.sessions.contains_key(id) {
                        let id = id.to_string();
                        p.add(move |b| {
                            b.sessions.insert(id, Sess { rev: 0, dirty: true });
                        });
                    }
                }
            }
            (None, Some(_), None) => {
                if let Some((rev, hash)) = pull_profile(c, id, &b.kinds).await? {
                    p.rec("profile", id, Some(Rec::of(rev, hash)));
                    let id = id.to_string();
                    p.add(move |b| {
                        b.picked.insert(id);
                    });
                    told.topics.insert("profiles");
                }
            }
            // The same profile on both sides, met for the first time: one is kept,
            // or both, the browser data here going with this computer's version.
            (None, Some(s), Some(u)) => {
                let theirs = server_hash(c, id, &b.kinds).await?;
                let data_here = has_data(id) && !b.sessions.contains_key(id);
                let with_data = data_here && s.session_rev > 0;
                if theirs.as_deref() == Some(u.hash.as_str()) {
                    p.rec("profile", id, Some(Rec::of(s.revision, u.hash.clone())));
                } else {
                    keep_both(c, id, &b.kinds, p, told, with_data).await?;
                }
                let (id, data_goes) = (id.to_string(), data_here && s.session_rev == 0);
                p.add(move |b| {
                    if data_goes {
                        b.sessions.insert(id.clone(), Sess { rev: 0, dirty: true });
                    }
                    b.picked.insert(id);
                });
            }
            (Some(_), None, None) => p.rec("profile", id, None),
            // Gone from the server: deleted or stopped on another computer.
            (Some(r), None, Some(u)) => {
                if !on {
                    p.rec("profile", id, None);
                } else if marked {
                    // Stopped there: this copy stays here, synced no more.
                    p.rec("profile", id, None);
                    let id = id.to_string();
                    p.add(move |b| {
                        b.picked.remove(&id);
                        b.sessions.remove(&id);
                    });
                    told.topics.insert("profiles");
                } else if r.hash == u.hash {
                    if let Err(e) = local::trash::move_to_trash(id) {
                        eprintln!("[sync] {id}: not moved to the trash: {e:#}");
                        let _ = local::profile::delete(id);
                    }
                    p.rec("profile", id, None);
                    let id = id.to_string();
                    p.add(move |b| {
                        b.picked.remove(&id);
                        b.sessions.remove(&id);
                    });
                    told.topics.insert("profiles");
                } else if let Some(rev) = skip(put_profile(c, id, u, 0).await, id)? {
                    p.rec("profile", id, Some(Rec::of(rev, u.hash.clone())));
                }
            }
            // Deleted here: deleted there, unless it changed there meanwhile.
            (Some(r), Some(_), None) => {
                if remote_changed {
                    if let Some((rev, hash)) = pull_profile(c, id, &b.kinds).await? {
                        p.rec("profile", id, Some(Rec::of(rev, hash)));
                        told.topics.insert("profiles");
                    }
                } else {
                    // A mark left from before would keep it on the others.
                    if marked {
                        skip(unmark_stopped(c, id).await, id)?;
                    }
                    if skip(delete_at(c, &me_path("profiles", id)?, r.rev).await, id)?.is_some() {
                        p.rec("profile", id, None);
                    }
                }
            }
            // Stopped syncing here: it leaves the server with a mark, so the
            // other computers keep their copy, and it stays here.
            (Some(_), Some(s), Some(_)) if !on => {
                // Open here: it runs on, its lease given back and nothing sent.
                super::super::open::let_go_mine(Some(id));
                if skip(mark_stopped(c, id).await, id)?.is_some()
                    && skip(delete_at(c, &me_path("profiles", id)?, s.revision).await, id)?.is_some()
                {
                    p.rec("profile", id, None);
                    let id = id.to_string();
                    p.add(move |b| {
                        b.sessions.remove(&id);
                    });
                }
            }
            (Some(r), Some(_), Some(u)) => match (local_changed, remote_changed) {
                (false, false) => {}
                (true, false) => match put_profile(c, id, u, r.rev).await {
                    Ok(rev) => p.rec("profile", id, Some(Rec::of(rev, u.hash.clone()))),
                    Err(e) if e.code() == Some(&Code::Stale) => keep_both(c, id, &b.kinds, p, told, false).await?,
                    Err(e) => {
                        skip(Err::<(), _>(e), id)?;
                    }
                },
                (false, true) => {
                    if let Some((rev, hash)) = pull_profile(c, id, &b.kinds).await? {
                        p.rec("profile", id, Some(Rec::of(rev, hash)));
                        told.topics.insert("profiles");
                    }
                }
                (true, true) => keep_both(c, id, &b.kinds, p, told, false).await?,
            },
            _ => {}
        }
    }
    Ok(())
}

/// The digest the server's version would have here.
async fn server_hash(c: &TeamClient, id: &str, kinds: &Kinds) -> Result<Option<String>, Stop> {
    let Some(got) = skip(c.get::<ProfileWithData>(&me_path("profiles", id)?).await, id)? else { return Ok(None) };
    let mut s = remote::stored(&got.row, Some(&got.data));
    if let Ok(here) = local::profile::load_raw(id) {
        s.meta.created_at = here.meta.created_at.or(s.meta.created_at);
    }
    let tags = if kinds.folders_tags { got.row.card.tags.clone() } else { Vec::new() };
    Ok(Some(up_of(&s, tags).hash))
}

/// Changed on both sides: this computer's version becomes a conflict copy,
/// and the server's takes the profile's place here.
async fn keep_both(c: &TeamClient, id: &str, kinds: &Kinds, p: &mut Patch, told: &mut Told, with_data: bool) -> Result<(), Stop> {
    if let Err(e) = conflict_copy(id, with_data) {
        eprintln!("[sync] {id}: no conflict copy made, so nothing is written over: {e:#}");
        return Ok(());
    }
    if let Some((rev, hash)) = pull_profile(c, id, kinds).await? {
        p.rec("profile", id, Some(Rec::of(rev, hash)));
    }
    told.topics.insert("profiles");
    Ok(())
}

/// One profile up before it is first opened, its proxy first.
pub(super) async fn push_one(id: &str) -> Net<()> {
    let c = TeamClient::for_sync().map_err(|e| TeamError::Garbled(format!("{e:#}")))?;
    let b = book::read();
    let up = up_here(id, &b.kinds).ok_or_else(|| TeamError::Garbled(format!("no profile {id} here")))?;
    if let Some(x) = up.card.bound_proxy().filter(|x| b.rec("proxy", x).is_none()) {
        if let Ok(Some(e)) = local::proxy::get(x) {
            let rev = put_proxy(&c, &e, 0).await?;
            let (x, hash) = (x.to_string(), proxy_hash(&e));
            let _ = book::change(move |b| b.set_rec("proxy", &x, Rec::of(rev, hash)));
        }
    }
    let rev = put_profile(&c, id, &up, 0).await?;
    let _ = book::change(|b| b.set_rec("profile", id, Rec::of(rev, up.hash.clone())));
    Ok(())
}

// ---- sessions ----

/// Browser data changed here and not sent (a start without the server, a close
/// it did not take): sent now, each under a lease, while its browser is closed.
async fn sessions() -> Result<(), Stop> {
    let b = book::read();
    let Some(place) = crate::team::sync_place() else { return Ok(()) };
    for (id, s) in b.sessions.iter().filter(|(_, s)| s.dirty) {
        let here = local::profile::exists(id);
        if !here || !b.syncs(id) || b.rec("profile", id).is_none() || crate::process::Tracker::shared().is_running(id) {
            continue;
        }
        let Ok(udd) = local::profile::user_data_dir(id) else { continue };
        let name = local::profile::load_raw(id).map(|s| remote::name_of(&s)).unwrap_or_default();
        match super::super::open::upload_own(&place.team_id, id, &name, udd, s.rev).await {
            Ok(outcome) => eprintln!("[sync] {id}: session {outcome:?}"),
            Err(e) => match super::super::error::find(&e) {
                Some(super::super::error::CmdError::Offline(why)) => return Err(TeamError::Offline(why.clone()).into()),
                _ => eprintln!("[sync] {id}: session not sent now: {e:#}"),
            },
        }
    }
    Ok(())
}

// ---- proxies ----

fn proxy_hash(e: &ProxyEntry) -> String {
    digest(&serde_json::to_value(e).unwrap_or_default())
}

async fn put_proxy(c: &TeamClient, e: &ProxyEntry, base: u64) -> Net<u64> {
    let kind = serde_json::to_value(&e.kind).ok().and_then(|v| v.as_str().map(str::to_owned)).unwrap_or_default();
    let body = json!({
        "name": remote::proxy_name(e), "kind": kind, "country": e.country,
        "data": remote::proxy_data(e), "base_revision": base,
    });
    let w: Written = c.json(Method::PUT, &me_path("proxies", &e.id)?, Some(&body)).await?;
    Ok(w.revision)
}

/// The server's proxy, written here; how it reads here after.
async fn pull_proxy(c: &TeamClient, id: &str) -> Result<Option<(u64, String)>, Stop> {
    let Some(got) = skip(c.get::<ProxyWithData>(&me_path("proxies", id)?).await, id)? else { return Ok(None) };
    let e = remote::proxy(&got.row, Some(&got.data));
    if e.host.trim().is_empty() || e.port == 0 {
        return Ok(None);
    }
    match local::proxy::upsert(e) {
        Ok(saved) => Ok(Some((got.row.revision, proxy_hash(&saved)))),
        Err(err) => {
            eprintln!("[sync] proxy {id} not written here: {err:#}");
            Ok(None)
        }
    }
}

/// The proxies that sync: every one under "all", else those a synced profile is bound to.
fn wanted_proxies(b: &Book, rows: &[ProfileRow], here: &BTreeMap<String, StoredProfile>) -> BTreeSet<String> {
    let mut out: BTreeSet<String> = rows.iter().filter_map(|r| r.card.bound_proxy().map(str::to_owned)).collect();
    for (id, s) in here {
        if b.syncs(id) {
            if let Some(x) = s.meta.proxy_id.as_deref().map(str::trim).filter(|x| !x.is_empty()) {
                out.insert(x.to_string());
            }
        }
    }
    out
}

async fn proxies(c: &TeamClient, rows: &[ProfileRow], p: &mut Patch, told: &mut Told) -> Result<(), Stop> {
    let b = book::read();
    let all = b.kinds.proxies == book::Proxies::All;
    let wanted = wanted_proxies(&b, rows, &local_profiles()?);
    let bound_there: BTreeSet<&str> = rows.iter().filter_map(|r| r.card.bound_proxy()).collect();
    let server: BTreeMap<String, ProxyRow> =
        c.get::<Vec<ProxyRow>>("/me/proxies").await?.into_iter().map(|r| (r.id.clone(), r)).collect();
    let here: BTreeMap<String, ProxyEntry> =
        local::proxy::list().map_err(|e| unread("the proxies", e))?.into_iter().map(|e| (e.id.clone(), e)).collect();
    let none = BTreeMap::new();
    let recs = b.records.get("proxy").unwrap_or(&none);
    let ids: BTreeSet<String> = here.keys().chain(server.keys()).chain(recs.keys()).cloned().collect();
    for id in &ids {
        let id = id.as_str();
        let want = all || wanted.contains(id);
        let (r, s, l) = (recs.get(id), server.get(id), here.get(id));
        let lh = l.map(proxy_hash);
        match (r, s, l) {
            (None, None, Some(e)) if want => {
                if let Some(rev) = skip(put_proxy(c, e, 0).await, id)? {
                    p.rec("proxy", id, Some(Rec::of(rev, lh.unwrap_or_default())));
                }
            }
            (None, Some(_), _) if want => {
                let theirs = pull_proxy(c, id).await?;
                if let Some((rev, hash)) = theirs {
                    if lh.as_ref().is_some_and(|h| *h != hash) {
                        told.warnings.push(format!("The proxy {id} was set differently on another computer: that version is kept."));
                    }
                    p.rec("proxy", id, Some(Rec::of(rev, hash)));
                    told.topics.insert("proxies");
                }
            }
            // Gone from the server: this computer keeps its own.
            (Some(_), None, _) => p.rec("proxy", id, None),
            (Some(r), Some(s), None) => {
                if r.rev != s.revision {
                    if let Some((rev, hash)) = pull_proxy(c, id).await? {
                        p.rec("proxy", id, Some(Rec::of(rev, hash)));
                        told.topics.insert("proxies");
                    }
                } else if skip(delete_at(c, &me_path("proxies", id)?, r.rev).await, id)?.is_some() {
                    p.rec("proxy", id, None);
                }
            }
            // Bound to nothing that syncs any more: it leaves the server, unless a profile there needs it.
            (Some(_), Some(s), Some(_)) if !want => {
                if bound_there.contains(id) || skip(delete_at(c, &me_path("proxies", id)?, s.revision).await, id)?.is_some() {
                    p.rec("proxy", id, None);
                }
            }
            (Some(r), Some(s), Some(e)) => {
                let (lc, rc) = (lh.as_deref() != Some(r.hash.as_str()), r.rev != s.revision);
                if rc {
                    if lc {
                        told.warnings.push(format!("The proxy {} was changed on another computer too: that version is kept.", e.name));
                    }
                    if let Some((rev, hash)) = pull_proxy(c, id).await? {
                        p.rec("proxy", id, Some(Rec::of(rev, hash)));
                        told.topics.insert("proxies");
                    }
                } else if lc {
                    if let Some(rev) = skip(put_proxy(c, e, r.rev).await, id)? {
                        p.rec("proxy", id, Some(Rec::of(rev, lh.unwrap_or_default())));
                    }
                }
            }
            _ => {}
        }
    }
    Ok(())
}

// ---- folders ----

fn local_folders() -> Result<BTreeMap<String, String>, Stop> {
    let st = local::folders::list().map_err(|e| unread("the folders", e))?;
    let mut out: BTreeMap<String, String> = st.registry.iter().map(|n| (n.clone(), String::new())).collect();
    for (n, m) in st.meta {
        out.insert(n, m.color.unwrap_or_default());
    }
    Ok(out)
}

fn set_local_color(name: &str, color: &str) -> anyhow::Result<()> {
    local::folders::add(name)?;
    let icon = local::folders::list()?.meta.get(name).map(|m| m.icon.clone()).unwrap_or_else(|| "folder".into());
    let color = Some(color.to_string()).filter(|c| !c.is_empty());
    local::folders::set_meta(name, super::super::folders::FolderMeta { color, icon })
}

async fn folders(c: &TeamClient, p: &mut Patch, told: &mut Told) -> Result<(), Stop> {
    let b = book::read();
    let server: BTreeMap<String, FolderRow> =
        c.get::<Vec<FolderRow>>("/me/folders").await?.into_iter().map(|f| (f.name.clone(), f)).collect();
    let here = local_folders()?;
    let none = BTreeMap::new();
    let recs = b.records.get("folder").unwrap_or(&none);
    let names: BTreeSet<String> = here.keys().chain(server.keys()).chain(recs.keys()).cloned().collect();
    let rec = |color: &str| Some(Rec::of(0, color.to_string()));
    for name in &names {
        let n = name.as_str();
        let path = me_path("folders", n)?;
        match (recs.get(n), server.get(n), here.get(n)) {
            (None, None, Some(color)) => {
                let body = json!({ "name": n, "color": color });
                if skip(c.call(Method::POST, "/me/folders", Some(&body)).await, n)?.is_some() {
                    p.rec("folder", n, rec(color));
                }
            }
            (None, Some(s), here) => {
                if here.is_some_and(|h| *h != s.color) || here.is_none() {
                    if let Err(e) = set_local_color(n, &s.color) {
                        eprintln!("[sync] folder {n}: {e:#}");
                        continue;
                    }
                    told.topics.insert("folders");
                }
                p.rec("folder", n, rec(&s.color));
            }
            (Some(_), None, None) => p.rec("folder", n, None),
            (Some(r), None, Some(color)) => {
                if *color == r.hash {
                    let _ = local::folders::forget(n);
                    told.topics.insert("folders");
                    p.rec("folder", n, None);
                } else {
                    let body = json!({ "name": n, "color": color });
                    if skip(c.call(Method::POST, "/me/folders", Some(&body)).await, n)?.is_some() {
                        p.rec("folder", n, rec(color));
                    }
                }
            }
            (Some(_), Some(_), None) => {
                // A folder its profiles still fill there stays, and stops syncing here.
                let _ = skip(c.call(Method::DELETE, &path, None).await, n)?;
                p.rec("folder", n, None);
            }
            (Some(r), Some(s), Some(color)) => {
                let (lc, rc) = (*color != r.hash, s.color != r.hash);
                if rc {
                    if set_local_color(n, &s.color).is_ok() {
                        told.topics.insert("folders");
                        p.rec("folder", n, rec(&s.color));
                    }
                } else if lc {
                    let body = json!({ "color": color });
                    if skip(c.call(Method::PATCH, &path, Some(&body)).await, n)?.is_some() {
                        p.rec("folder", n, rec(color));
                    }
                }
            }
            _ => {}
        }
    }
    Ok(())
}

// ---- bookmarks, extensions, projects and tags ----

#[derive(Clone, Copy, PartialEq, Eq)]
enum ItemKind {
    Tag,
    Bookmark,
    Automation,
    Extension,
}

const ITEM_KINDS: [ItemKind; 4] = [ItemKind::Tag, ItemKind::Bookmark, ItemKind::Automation, ItemKind::Extension];

impl ItemKind {
    fn key(self) -> &'static str {
        match self {
            ItemKind::Tag => "tag",
            ItemKind::Bookmark => "bookmark",
            ItemKind::Automation => "automation",
            ItemKind::Extension => "extension",
        }
    }

    fn topic(self) -> &'static str {
        match self {
            ItemKind::Tag => "tags",
            ItemKind::Bookmark => "bookmarks",
            ItemKind::Automation => "automation",
            ItemKind::Extension => "extensions",
        }
    }

    fn on(self, k: &Kinds) -> bool {
        match self {
            ItemKind::Tag => k.folders_tags,
            ItemKind::Bookmark => k.bookmarks,
            ItemKind::Automation => k.automation,
            ItemKind::Extension => k.extensions,
        }
    }

    /// What of an item travels and counts: nothing of this computer's alone.
    fn norm(self, mut v: Value) -> Value {
        if let Value::Object(m) = &mut v {
            match self {
                ItemKind::Automation => {
                    m.insert("updated_at".into(), json!(0));
                    m.remove("revision");
                }
                ItemKind::Extension => {
                    m.insert("path".into(), json!(""));
                    m.insert("added_at".into(), json!(""));
                }
                _ => {}
            }
        }
        v
    }

    /// This computer's items: name and what goes up.
    fn here(self) -> Result<BTreeMap<String, (String, Value)>, Stop> {
        fn row<T: Serialize>(id: &str, name: &str, v: &T) -> (String, String, Value) {
            (id.to_string(), name.to_string(), serde_json::to_value(v).unwrap_or_default())
        }
        let rows: anyhow::Result<Vec<(String, String, Value)>> = match self {
            ItemKind::Tag => local::tags::list().map(|t| t.tags.iter().map(|t| row(&t.id, &t.name, t)).collect()),
            ItemKind::Bookmark => local::bookmarks::list().map(|l| l.iter().map(|x| row(&x.id, &x.title, x)).collect()),
            ItemKind::Automation => local::automation::list().map(|l| {
                l.into_iter()
                    .map(|mut x| {
                        // A secret step parameter never leaves this computer.
                        let _ = super::super::secrets::take(&mut x);
                        x.revision = None;
                        row(&x.id, &x.name, &x)
                    })
                    .collect()
            }),
            ItemKind::Extension => local::extensions::list().map(|l| l.iter().map(|x| row(&x.id, &x.name, x)).collect()),
        };
        let rows = rows.map_err(|e| unread(self.topic(), e))?;
        Ok(rows.into_iter().map(|(id, name, v)| (id, (name, self.norm(v)))).collect())
    }

    fn remove_here(self, id: &str) -> anyhow::Result<()> {
        match self {
            ItemKind::Tag => local::tags::delete(id),
            ItemKind::Bookmark => local::bookmarks::delete(id),
            ItemKind::Automation => local::automation::delete(id),
            ItemKind::Extension => local::extensions::delete(id),
        }
    }
}

fn hash_item(kind: ItemKind, v: &Value) -> String {
    digest(&kind.norm(v.clone()))
}

/// The server's item written here; an extension's files only when its package moved.
async fn write_item(c: &TeamClient, kind: ItemKind, row: &ItemRow, r: Option<&Rec>, told: &mut Told) -> Result<bool, Stop> {
    let mut v = opened(&row.data).unwrap_or_else(|| json!({}));
    if let Value::Object(m) = &mut v {
        m.insert("id".into(), json!(row.id));
    }
    let r = match kind {
        ItemKind::Tag => serde_json::from_value(v).map_err(anyhow::Error::from).and_then(|t| local::tags::save(t, None).map(|_| ())),
        ItemKind::Bookmark => serde_json::from_value(v).map_err(anyhow::Error::from).and_then(|x| local::bookmarks::save(x).map(|_| ())),
        ItemKind::Automation => serde_json::from_value::<super::super::automation::Project>(v)
            .map_err(anyhow::Error::from)
            .and_then(|mut x| {
                keep_secrets_here(&mut x);
                local::automation::save(x).map(|_| ())
            }),
        ItemKind::Extension => return Ok(extension::write(c, row, r, told).await? != extension::Wrote::No),
    };
    match r {
        Ok(()) => Ok(true),
        Err(e) => {
            eprintln!("[sync] {} {} not written here: {e:#}", kind.key(), row.id);
            Ok(false)
        }
    }
}

/// The values this computer keeps for a project's secret steps, put back.
fn keep_secrets_here(p: &mut super::super::automation::Project) {
    let Some(mine) = local::automation::list().ok().and_then(|all| all.into_iter().find(|x| x.id == p.id)) else { return };
    for b in &mut p.blocks {
        let Some(old) = mine.blocks.iter().find(|o| o.id == b.id) else { continue };
        let (Value::Object(to), Value::Object(from)) = (&mut b.params, &old.params) else { continue };
        for name in &b.secrets {
            if let Some(v) = from.get(name) {
                to.insert(name.clone(), v.clone());
            }
        }
    }
}

async fn put_item(c: &TeamClient, kind: ItemKind, id: &str, name: &str, v: &Value, base: u64) -> Net<u64> {
    let body = json!({ "name": remote::item_name(name), "data": b64(v), "base_revision": base });
    let w: ItemWritten = c.json(Method::PUT, &me_path(&format!("items/{}", kind.key()), id)?, Some(&body)).await?;
    Ok(w.revision)
}

/// Up as new or changed, over the server's `over` (none: as new there); an
/// extension's files after it when they changed ([`extension::after_put`]).
async fn send_item(c: &TeamClient, kind: ItemKind, id: &str, name: &str, v: &Value, r: Option<&Rec>, over: Option<&ItemRow>) -> Result<Option<Rec>, Stop> {
    let Some(rev) = skip(put_item(c, kind, id, name, v, over.map_or(0, |o| o.revision)).await, id)? else { return Ok(None) };
    let rec = Rec::of(rev, hash_item(kind, v));
    if kind != ItemKind::Extension {
        return Ok(Some(rec));
    }
    extension::after_put(c, id, v, rec, r, over).await.map(Some)
}

/// What was synced of an item just taken from the server's row.
fn pulled(kind: ItemKind, id: &str, row: &ItemRow) -> Rec {
    let ext = kind == ItemKind::Extension;
    Rec {
        rev: row.revision,
        hash: here_hash(kind, id),
        package: extension::package_of(row),
        files: if ext { extension::files_here(id) } else { String::new() },
    }
}

async fn items(c: &TeamClient, kind: ItemKind, p: &mut Patch, told: &mut Told) -> Result<(), Stop> {
    let key = kind.key();
    if kind == ItemKind::Extension && !extension::settled().await {
        return Ok(());
    }
    let b = book::read();
    let server: BTreeMap<String, ItemRow> =
        c.get::<Vec<ItemRow>>(&format!("/me/items/{key}")).await?.into_iter().map(|r| (r.id.clone(), r)).collect();
    let here = kind.here()?;
    let none = BTreeMap::new();
    let recs = b.records.get(key).unwrap_or(&none);
    let ids: BTreeSet<String> = here.keys().chain(server.keys()).chain(recs.keys()).cloned().collect();
    let theirs = |row: &ItemRow| opened(&row.data).map(|v| hash_item(kind, &v)).unwrap_or_default();
    // An extension's package moves without its record when another computer's upload lags.
    let moved = |r: &Rec, row: &ItemRow| r.rev != row.revision || (kind == ItemKind::Extension && r.package != extension::package_of(row));
    for id in &ids {
        let id = id.as_str();
        let (r, s, l) = (recs.get(id), server.get(id), here.get(id));
        // Its folder is here though it does not read now: neither changed nor deleted.
        if l.is_none() && kind == ItemKind::Extension && extension::unlisted(id) {
            eprintln!("[sync] extension {id}: here but unreadable for now; left as it is");
            continue;
        }
        let lh = l.map(|(_, v)| hash_item(kind, v));
        match (r, s, l) {
            (None, None, Some((name, v))) => {
                if let Some(rec) = send_item(c, kind, id, name, v, None, None).await? {
                    p.rec(key, id, Some(rec));
                }
            }
            (None, Some(row), Some((name, v))) if kind == ItemKind::Extension => {
                if let Some(rec) = extension::first(c, id, name, v, row, told).await? {
                    p.rec(key, id, Some(rec));
                }
            }
            (None, Some(row), l) => {
                // Met for the first time: the same on both sides, or the server's.
                let same = lh.as_deref() == Some(theirs(row).as_str());
                // A project of this computer's is kept beside it first.
                let kept = match l {
                    Some((name, _)) if !same && kind == ItemKind::Automation => match keep_project(id, name, told) {
                        Some(()) => true,
                        None => continue,
                    },
                    _ => false,
                };
                if same || write_item(c, kind, row, None, told).await? {
                    if !same {
                        told.topics.insert(kind.topic());
                        if l.is_some() && !kept {
                            told.warnings.push(format!("A {key} was set differently on another computer: that version is kept."));
                        }
                    }
                    p.rec(key, id, Some(pulled(kind, id, row)));
                }
            }
            (Some(_), None, None) => p.rec(key, id, None),
            (Some(r), None, Some((name, v))) => {
                if lh.as_deref() == Some(r.hash.as_str()) {
                    if let Err(e) = kind.remove_here(id) {
                        eprintln!("[sync] {key} {id}: {e:#}");
                    }
                    told.topics.insert(kind.topic());
                    p.rec(key, id, None);
                } else if let Some(rec) = send_item(c, kind, id, name, v, None, None).await? {
                    p.rec(key, id, Some(rec));
                }
            }
            (Some(r), Some(row), None) => {
                if moved(r, row) {
                    if write_item(c, kind, row, Some(r), told).await? {
                        told.topics.insert(kind.topic());
                        p.rec(key, id, Some(pulled(kind, id, row)));
                    }
                } else if skip(delete_at(c, &me_path(&format!("items/{key}"), id)?, r.rev).await, id)?.is_some() {
                    p.rec(key, id, None);
                }
            }
            (Some(r), Some(row), Some((name, v))) => {
                let (lc, rc) = (lh.as_deref() != Some(r.hash.as_str()), moved(r, row));
                if rc && kind == ItemKind::Extension {
                    if let Some(rec) = extension::met(c, id, name, v, r, row, lc, told).await? {
                        p.rec(key, id, Some(rec));
                    }
                } else if rc {
                    let differs = lc && lh.as_deref() != Some(theirs(row).as_str());
                    // A project's two versions are both kept, as a profile's are.
                    if differs && kind == ItemKind::Automation {
                        if keep_project(id, name, told).is_none() {
                            continue;
                        }
                    } else if differs {
                        told.warnings.push(format!("“{name}” was changed on another computer too: that version is kept."));
                    }
                    if write_item(c, kind, row, Some(r), told).await? {
                        told.topics.insert(kind.topic());
                        p.rec(key, id, Some(pulled(kind, id, row)));
                    }
                } else if lc {
                    match send_item(c, kind, id, name, v, Some(r), Some(row)).await {
                        Ok(Some(rec)) => p.rec(key, id, Some(rec)),
                        Ok(None) => {}
                        Err(e) => return Err(e),
                    }
                }
            }
            _ => {}
        }
    }
    Ok(())
}

/// A project changed here and on another computer: this computer's version
/// goes to a new project beside it, before the server's takes its place.
/// None when no copy could be made; nothing is written over then.
fn keep_project(id: &str, name: &str, told: &mut Told) -> Option<()> {
    let made = local::automation::list().and_then(|all| {
        let mine = all.into_iter().find(|p| p.id == id).ok_or_else(|| anyhow::anyhow!("not here"))?;
        let copy = super::super::automation::Project {
            name: format!("{name} (conflict copy)"),
            ..local::automation::copy_of(mine)
        };
        local::automation::save(copy)
    });
    match made {
        Ok(_) => {
            told.warnings.push(format!(
                "“{name}” was changed on another computer too, so both versions are kept: \
                 the changes made here are in “{name} (conflict copy)”."
            ));
            told.topics.insert(ItemKind::Automation.topic());
            Some(())
        }
        Err(e) => {
            eprintln!("[sync] project {id}: no conflict copy made, so nothing is written over: {e:#}");
            None
        }
    }
}

/// The digest of the item as it reads here now.
fn here_hash(kind: ItemKind, id: &str) -> String {
    kind.here().ok().and_then(|h| h.get(id).map(|(_, v)| hash_item(kind, v))).unwrap_or_default()
}

// ---- what waits ----

/// The changes made here the server has not taken, as far as this computer can
/// tell without asking it: kept in `sync-outbox.json`.
pub(super) fn scan() {
    if crate::team::sync_place().is_none() {
        let _ = book::set_outbox(Vec::new());
        return;
    }
    match waiting() {
        Ok(out) => {
            if let Err(e) = book::set_outbox(out) {
                eprintln!("[sync] {e:#}");
            }
        }
        // What could not be read leaves the outbox as it was.
        Err(Stop::Local(why)) => eprintln!("[sync] {why}"),
        Err(_) => {}
    }
}

fn waiting() -> Result<Vec<Entry>, Stop> {
    let b = book::read();
    let mut out = Vec::new();
    let mut put = |kind: &str, id: &str, op: Op| out.push(Entry { kind: kind.into(), id: id.into(), op });
    let none = BTreeMap::new();

    let here = local_profiles()?;
    let mut tags = tags_of(&b.kinds)?;
    let recs = b.records.get("profile").unwrap_or(&none);
    for (id, s) in &here {
        let up = up_of(s, tags.remove(id).unwrap_or_default());
        match recs.get(id) {
            Some(_) if !b.syncs(id) => put("profile", id, Op::Delete),
            Some(r) if r.hash != up.hash => put("profile", id, Op::Put),
            None if b.syncs(id) => put("profile", id, Op::Put),
            _ => {}
        }
    }
    if !all_vanished(recs, &here) {
        for id in recs.keys().filter(|id| !here.contains_key(*id) && !unreadable(id, &here)) {
            put("profile", id, Op::Delete);
        }
    }

    let proxies: BTreeMap<String, ProxyEntry> =
        local::proxy::list().map_err(|e| unread("the proxies", e))?.into_iter().map(|e| (e.id.clone(), e)).collect();
    let wanted = wanted_proxies(&b, &[], &here);
    let all = b.kinds.proxies == book::Proxies::All;
    let recs = b.records.get("proxy").unwrap_or(&none);
    for (id, e) in &proxies {
        match recs.get(id) {
            Some(r) if r.hash != proxy_hash(e) => put("proxy", id, Op::Put),
            None if all || wanted.contains(id) => put("proxy", id, Op::Put),
            _ => {}
        }
    }
    for id in recs.keys().filter(|id| !proxies.contains_key(*id)) {
        put("proxy", id, Op::Delete);
    }

    if b.kinds.folders_tags {
        let here = local_folders()?;
        let recs = b.records.get("folder").unwrap_or(&none);
        for (name, color) in &here {
            if recs.get(name).is_none_or(|r| r.hash != *color) {
                put("folder", name, Op::Put);
            }
        }
        for name in recs.keys().filter(|n| !here.contains_key(*n)) {
            put("folder", name, Op::Delete);
        }
    }
    for kind in ITEM_KINDS.into_iter().filter(|k| k.on(&b.kinds)) {
        let here = kind.here()?;
        let recs = b.records.get(kind.key()).unwrap_or(&none);
        for (id, (_, v)) in &here {
            if recs.get(id).is_none_or(|r| r.hash != hash_item(kind, v)) {
                put(kind.key(), id, Op::Put);
            }
        }
        for id in recs.keys().filter(|id| !here.contains_key(*id) && !(kind == ItemKind::Extension && extension::unlisted(id))) {
            put(kind.key(), id, Op::Delete);
        }
    }
    out.sort();
    Ok(out)
}

// ---- removing the person's data ----

#[derive(Deserialize)]
struct Named {
    #[serde(default)]
    id: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    revision: u64,
}

/// Everything of the person's on the server: the document first, so their other
/// computers stop at once; a profile open on one of them is taken over to go.
pub(super) async fn remove_all(c: &TeamClient) -> Net<()> {
    match c.call(Method::DELETE, DOC, None).await {
        Err(e) if e.status() != Some(404) => return Err(e),
        _ => {}
    }
    for row in c.get::<Vec<Named>>("/me/profiles").await? {
        let path = me_path("profiles", &row.id)?;
        match delete_at(c, &path, row.revision).await {
            Err(e) if e.code() == Some(&Code::Leased) => {
                c.call(Method::POST, &format!("{path}/lease?takeover=true"), None).await?;
                delete_at(c, &path, row.revision).await?;
            }
            Err(e) if e.status() != Some(404) => return Err(e),
            _ => {}
        }
    }
    for row in c.get::<Vec<Named>>("/me/proxies").await? {
        gone(delete_at(c, &me_path("proxies", &row.id)?, row.revision).await)?;
    }
    for row in c.get::<Vec<Named>>("/me/folders").await? {
        gone(c.call(Method::DELETE, &me_path("folders", &row.name)?, None).await.map(|_| ()))?;
    }
    for kind in ITEM_KINDS {
        let key = kind.key();
        for row in c.get::<Vec<Named>>(&format!("/me/items/{key}")).await? {
            gone(delete_at(c, &me_path(&format!("items/{key}"), &row.id)?, row.revision).await)?;
        }
    }
    // The stop marks last.
    for row in c.get::<Vec<Named>>("/me/items/sync").await? {
        gone(c.call(Method::DELETE, &me_path("items/sync", &row.id)?, None).await.map(|_| ()))?;
    }
    Ok(())
}

/// Gone already is gone.
fn gone(r: Net<()>) -> Net<()> {
    match r {
        Err(e) if e.status() == Some(404) => Ok(()),
        r => r,
    }
}
