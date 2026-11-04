//! Writes to a team: the door's rule, a pass from `perm`, the server with the
//! revision the change was made from, then the copy held here.

use std::collections::BTreeMap;

use serde_json::json;

use super::cache;
use super::client::{Code, Method, TeamClient, TeamError};
use super::doors::{self, Ask, Door};
use super::error::{CmdError, Doing, Need};
use super::perm::{self, Create, Delete, Edit, On};
use super::profile::{ProfileMeta, StoredProfile};
use super::proxy::ProxyEntry;
use super::remote::{self, ProfileFields, ProfileRow, ProfileWithData};
use super::Ticket;

/// What a new profile comes with, for its door's conditions.
#[derive(Debug, Default)]
pub struct Made<'a> {
    pub proxy_id: Option<&'a str>,
    pub inline_proxy: bool,
    pub extensions: bool,
}

/// How a profile save was asked for.
#[derive(Debug, Default, Clone)]
pub struct SaveOpts {
    /// The revision the editor opened; refused as stale once the profile moved past it.
    pub sent_rev: Option<u64>,
    /// Where it goes; "" is no folder. None keeps it where it is; a new one takes the settings' own.
    pub move_to: Option<String>,
    /// A proxy line came with it and was stored for it.
    pub inline_proxy: bool,
    /// A protected profile's password (#127): an edit of its sealed part needs it.
    pub password: Option<String>,
}

fn need(right: &'static str, on: &'static str) -> Need {
    Need { right, on }
}

fn missing(what: &str, id: &str) -> CmdError {
    CmdError::Missing(format!("no {what} {id} in this team"))
}

pub(super) fn client(team: &str) -> Result<TeamClient, CmdError> {
    TeamClient::for_team(team).map_err(|e| CmdError::Refused {
        status: 403,
        code: "no_access".into(),
        message: format!("{e:#}"),
    })
}

fn doing<'a>(need: Need, what: &'static str, id: &'a str, had: Option<u64>) -> Doing<'a> {
    Doing { need, what, id, had }
}

fn device() -> String {
    crate::team::device_id()
}

fn row_of(team: &str, id: &str) -> Result<ProfileRow, CmdError> {
    cache::row(team, id).ok_or_else(|| missing("profile", id))
}

/// The profile as its server has it now; reading the settings is a use of it.
async fn fresh(c: &TeamClient, id: &str) -> Result<ProfileWithData, CmdError> {
    remote::read_profile(c, id).await.map_err(|e| CmdError::from_team(e, &doing(need("edit", "profile"), "profile", id, None)))
}

/// A team profile's settings to change, as the server has them now.
pub async fn read_for_edit(team: &str, id: &str) -> Result<StoredProfile, CmdError> {
    row_of(team, id)?;
    let now = fresh(&client(team)?, id).await?;
    Ok(remote::stored(&now.row, Some(&now.data)))
}

/// What the list shows for a row just written, with its settings.
fn shown(team: &str, row: &ProfileRow, data: Option<&str>) -> ProfileMeta {
    let detail = data.map(|d| remote::stored(row, Some(d)));
    remote::meta(team, row, detail.as_ref(), &device())
}

fn bound(stored: &StoredProfile) -> Option<&str> {
    stored.meta.proxy_id.as_deref().map(str::trim).filter(|p| !p.is_empty())
}

// ---- profiles ----

/// A new profile's door, asked before anything made for it is stored.
pub fn check_create(door: Door, team: &str, folder: &str, made: &Made<'_>) -> Result<(), CmdError> {
    let folder = folder.trim();
    let ask = Ask {
        new: true,
        made: Some(("", folder)),
        folder: Some(folder),
        binds_proxy: made.proxy_id.map(str::trim).filter(|p| !p.is_empty()),
        inline_proxy: made.inline_proxy,
        changes_extensions: made.extensions,
        ..Default::default()
    };
    doors::clear(door, team, &ask)
}

/// An edit's door, asked before anything made for it is stored.
pub fn check_edit(door: Door, team: &str, id: &str, inline_proxy: bool) -> Result<(), CmdError> {
    let row = row_of(team, id)?;
    doors::clear(door, team, &Ask { profile: Some((id, &row.folder)), inline_proxy, ..Default::default() })
}

pub async fn save_profile(door: Door, t: &Ticket, team: &str, stored: StoredProfile, opts: SaveOpts) -> Result<ProfileMeta, CmdError> {
    if stored.meta.id.is_empty() {
        return create_profile(door, t, team, stored, opts).await;
    }
    let id = stored.meta.id.clone();
    let row = row_of(team, &id)?;
    let c = client(team)?;
    let now = fresh(&c, &id).await?;
    let before = remote::stored(&now.row, Some(&now.data));
    // Only an asked move moves it: the settings' own folder may be one read before someone else's move.
    let dest = opts.move_to.as_deref().map_or_else(|| now.row.folder.clone(), |f| f.trim().to_string());
    let moves = dest != now.row.folder;
    let binds = bound(&stored).filter(|b| Some(*b) != now.row.card.bound_proxy());
    let ask = Ask {
        profile: Some((&id, &row.folder)),
        folder: moves.then_some(dest.as_str()),
        moves_folder: moves,
        binds_proxy: binds,
        inline_proxy: opts.inline_proxy,
        changes_extensions: stored.meta.extensions != before.meta.extensions,
        ..Default::default()
    };
    doors::clear(door, team, &ask)?;
    let edit = perm::team_write::<Edit>(team, On::Profile { id: id.clone(), folder: now.row.folder.clone() })?;
    let to = if moves { Some(perm::team_write::<Create>(team, On::Folder(dest.clone()))?) } else { None };
    let card = remote::card(&stored, now.row.card.tags.clone());
    let data = remote::profile_data(&stored);
    let name = remote::name_of(&stored);
    let base = opts.sent_rev.unwrap_or(now.row.revision);
    let fields = ProfileFields { name: &name, data: &data, card: Some(&card) };
    t.commit().await?;
    let w = remote::edit_profile(&c, &edit, to.as_ref(), &fields, base)
        .await
        .map_err(|e| CmdError::from_team(e, &doing(need("edit", "profile"), "profile", &id, Some(base))))?;
    let row = remote::row_after(&w, Some(card), Some(&now.row));
    cache::wrote_profile(team, row.clone(), Some(&data));
    Ok(shown(team, &row, Some(&data)))
}

async fn create_profile(door: Door, t: &Ticket, team: &str, mut stored: StoredProfile, opts: SaveOpts) -> Result<ProfileMeta, CmdError> {
    let id = uuid::Uuid::new_v4().to_string();
    let folder = opts.move_to.as_deref().unwrap_or(&stored.meta.folder).trim().to_string();
    let ask = Ask {
        new: true,
        made: Some((&id, &folder)),
        folder: Some(&folder),
        binds_proxy: bound(&stored),
        inline_proxy: opts.inline_proxy,
        changes_extensions: !stored.meta.extensions.is_empty(),
        ..Default::default()
    };
    doors::clear(door, team, &ask)?;
    let create = perm::team_write::<Create>(team, On::Folder(folder.clone()))?;
    stored.meta.id = id.clone();
    stored.meta.folder = folder;
    super::local::profile::prepare_new(&mut stored);
    put_new(t, team, &create, &id, &stored, Vec::new()).await
}

/// A new profile out to the server, under the pass for its folder.
async fn put_new(
    t: &Ticket,
    team: &str,
    create: &perm::Granted<Create>,
    id: &str,
    stored: &StoredProfile,
    tags: Vec<String>,
) -> Result<ProfileMeta, CmdError> {
    let c = client(team)?;
    let card = remote::card(stored, tags);
    let data = remote::profile_data(stored);
    let name = remote::name_of(stored);
    let fields = ProfileFields { name: &name, data: &data, card: Some(&card) };
    t.commit().await?;
    let w = remote::create_profile(&c, create, id, &fields)
        .await
        .map_err(|e| CmdError::from_team(e, &doing(need("create", "folder"), "profile", id, None)))?;
    let row = remote::row_after(&w, Some(card), None);
    cache::wrote_profile(team, row.clone(), Some(&data));
    Ok(shown(team, &row, Some(&data)))
}

/// A copy in the same folder, with a fingerprint of its own.
pub async fn clone_profile(door: Door, t: &Ticket, team: &str, id: &str) -> Result<ProfileMeta, CmdError> {
    let row = row_of(team, id)?;
    let ask = Ask {
        profile: Some((id, &row.folder)),
        folder: Some(&row.folder),
        new: true,
        binds_proxy: row.card.bound_proxy(),
        ..Default::default()
    };
    doors::clear(door, team, &ask)?;
    let c = client(team)?;
    let now = remote::read_profile(&c, id)
        .await
        .map_err(|e| CmdError::from_team(e, &doing(need("start", "profile"), "profile", id, None)))?;
    let mut copy = remote::stored(&now.row, Some(&now.data));
    super::local::profile::make_copy(&mut copy);
    let new_id = copy.meta.id.clone();
    let create = perm::team_write::<Create>(team, On::Folder(now.row.folder.clone()))?;
    copy.meta.folder = now.row.folder.clone();
    super::local::profile::prepare_new(&mut copy);
    put_new(t, team, &create, &new_id, &copy, now.row.card.tags.clone()).await
}

/// To the team's trash, where it waits 30 days.
pub async fn remove_profile(door: Door, t: &Ticket, team: &str, id: &str) -> Result<(), CmdError> {
    let row = row_of(team, id)?;
    doors::clear(door, team, &Ask { profile: Some((id, &row.folder)), ..Default::default() })?;
    drop_profile(t, team, &row).await
}

async fn drop_profile(t: &Ticket, team: &str, row: &ProfileRow) -> Result<(), CmdError> {
    let del = perm::team_write::<Delete>(team, On::Profile { id: row.id.clone(), folder: row.folder.clone() })?;
    let c = client(team)?;
    t.commit().await?;
    remote::delete_profile(&c, &del, row.revision)
        .await
        .map_err(|e| CmdError::from_team(e, &doing(need("delete", "profile"), "profile", &row.id, Some(row.revision))))?;
    cache::dropped_profile(team, &row.id);
    Ok(())
}

/// Write the profile as the server has it now, changed by `change`, from that revision.
async fn rewrite(
    t: &Ticket,
    team: &str,
    id: &str,
    dest: Option<&str>,
    change: impl FnOnce(&mut team_model::Card),
) -> Result<(), CmdError> {
    let c = client(team)?;
    let now = fresh(&c, id).await?;
    let edit = perm::team_write::<Edit>(team, On::Profile { id: id.to_string(), folder: now.row.folder.clone() })?;
    let to = match dest.filter(|d| *d != now.row.folder) {
        Some(d) => Some(perm::team_write::<Create>(team, On::Folder(d.to_string()))?),
        None => None,
    };
    let mut card = now.row.card.clone();
    change(&mut card);
    let fields = ProfileFields { name: &now.row.name, data: &now.data, card: Some(&card) };
    let base = now.row.revision;
    t.commit().await?;
    let w = remote::edit_profile(&c, &edit, to.as_ref(), &fields, base)
        .await
        .map_err(|e| CmdError::from_team(e, &doing(need("edit", "profile"), "profile", id, Some(base))))?;
    cache::wrote_profile(team, remote::row_after(&w, Some(card), Some(&now.row)), Some(&now.data));
    Ok(())
}

pub async fn set_folder(door: Door, t: &Ticket, team: &str, id: &str, folder: &str) -> Result<(), CmdError> {
    let row = row_of(team, id)?;
    let dest = folder.trim();
    let ask = Ask { profile: Some((id, &row.folder)), folder: Some(dest), moves_folder: true, ..Default::default() };
    doors::clear(door, team, &ask)?;
    if dest == row.folder {
        return Ok(());
    }
    rewrite(t, team, id, Some(dest), |_| {}).await
}

pub async fn bind_proxy(door: Door, t: &Ticket, team: &str, id: &str, proxy_id: Option<String>) -> Result<(), CmdError> {
    let row = row_of(team, id)?;
    let to = proxy_id.as_deref().map(str::trim).filter(|p| !p.is_empty()).map(str::to_owned);
    let ask = Ask {
        profile: Some((id, &row.folder)),
        binds_proxy: to.as_deref().filter(|x| Some(*x) != row.card.bound_proxy()),
        ..Default::default()
    };
    doors::clear(door, team, &ask)?;
    rewrite(t, team, id, None, |card| card.proxy_id = to).await
}

/// Each listed profile gets exactly these tags; one already so is left alone.
pub async fn assign_tags(door: Door, t: &Ticket, team: &str, by_profile: BTreeMap<String, Vec<String>>) -> Result<(), CmdError> {
    for (id, tags) in by_profile {
        let row = row_of(team, &id)?;
        doors::clear(door, team, &Ask { profile: Some((&id, &row.folder)), ..Default::default() })?;
        if row.card.tags == tags {
            continue;
        }
        rewrite(t, team, &id, None, |card| card.tags = tags).await?;
    }
    Ok(())
}

// ---- folders: the admin's ----

fn admin_path(name: &str) -> Result<String, CmdError> {
    let seg = super::admin::seg(name).map_err(|_| CmdError::Missing(format!("no folder {name} in this team")))?;
    Ok(format!("/admin/folders/{seg}"))
}

fn folder_err(e: TeamError, name: &str) -> CmdError {
    CmdError::from_team(e, &doing(need("admin", "team"), "folder", name, None))
}

/// The folder's profiles go with the new name.
pub async fn rename_folder(door: Door, t: &Ticket, team: &str, old: &str, new: &str) -> Result<usize, CmdError> {
    doors::clear(door, team, &Ask::default())?;
    let new = new.trim();
    let rows = cache::rows_in_folder(team, old);
    let c = client(team)?;
    t.commit().await?;
    match c.call(Method::PATCH, &admin_path(old)?, Some(&json!({ "name": new }))).await {
        Ok(_) => {
            cache::renamed_folder(team, old, new);
            // The server moved each profile, at a new revision.
            cache::refetch(team, "folder").await;
        }
        // A folder only its profiles name: they move one by one.
        Err(e) if e.code() == Some(&Code::NotFound) => {
            for row in &rows {
                rewrite(t, team, &row.id, Some(new), |_| {}).await?;
            }
        }
        Err(e) => return Err(folder_err(e, old)),
    }
    Ok(rows.len())
}

/// The folder goes once its profiles are in the trash, or in no folder.
pub async fn delete_folder(door: Door, t: &Ticket, team: &str, name: &str, delete_profiles: bool) -> Result<usize, CmdError> {
    doors::clear(door, team, &Ask::default())?;
    // Each profile goes from the revision it is at now.
    cache::refetch(team, "profile").await;
    let rows = cache::rows_in_folder(team, name);
    for row in &rows {
        let ask = Ask { profile: Some((&row.id, &row.folder)), deletes_profiles: delete_profiles, ..Default::default() };
        doors::clear(door, team, &ask)?;
        if delete_profiles {
            drop_profile(t, team, row).await?;
        } else {
            rewrite(t, team, &row.id, Some(""), |_| {}).await?;
        }
    }
    let c = client(team)?;
    t.commit().await?;
    match c.call(Method::DELETE, &admin_path(name)?, None).await {
        Ok(_) => cache::dropped_folder(team, name),
        Err(e) if e.code() == Some(&Code::NotFound) => cache::dropped_folder(team, name),
        // Profiles deleted from it wait in the team's trash, judged by its rules:
        // the folder stays, empty, until they leave it.
        Err(e) if e.code() == Some(&Code::Conflict) && delete_profiles && !rows.is_empty() => {}
        Err(e) => return Err(folder_err(e, name)),
    }
    Ok(rows.len())
}

/// Kept while empty; one already there is fine.
pub async fn add_folder(door: Door, t: &Ticket, team: &str, name: &str) -> Result<(), CmdError> {
    doors::clear(door, team, &Ask::default())?;
    let c = client(team)?;
    t.commit().await?;
    match c.call(Method::POST, "/admin/folders", Some(&json!({ "name": name.trim(), "color": "" }))).await {
        Ok(_) => {}
        Err(e) if e.code() == Some(&Code::Conflict) => {}
        Err(e) => return Err(folder_err(e, name)),
    }
    cache::wrote_folder(team, name.trim(), None);
    Ok(())
}

/// A folder only its profiles name gets a row of its own for its colour.
pub async fn set_folder_color(door: Door, t: &Ticket, team: &str, name: &str, color: &str) -> Result<(), CmdError> {
    doors::clear(door, team, &Ask::default())?;
    let c = client(team)?;
    t.commit().await?;
    match c.call(Method::PATCH, &admin_path(name)?, Some(&json!({ "color": color }))).await {
        Ok(_) => {}
        Err(e) if e.code() == Some(&Code::NotFound) => {
            let body = json!({ "name": name, "color": color });
            c.call(Method::POST, "/admin/folders", Some(&body)).await.map_err(|e| folder_err(e, name))?;
        }
        Err(e) => return Err(folder_err(e, name)),
    }
    cache::wrote_folder(team, name, Some(color));
    Ok(())
}

// ---- proxies ----

pub async fn save_proxy(door: Door, t: &Ticket, team: &str, mut entry: ProxyEntry) -> Result<ProxyEntry, CmdError> {
    let existing = Some(entry.id.as_str()).filter(|id| !id.is_empty()).and_then(|id| cache::proxy_row(team, id));
    let c = client(team)?;
    let w = match existing {
        Some(row) => {
            doors::clear(door, team, &Ask { proxy: Some(&row.id), ..Default::default() })?;
            let edit = perm::team_write::<Edit>(team, On::Proxy(row.id.clone()))?;
            t.commit().await?;
            remote::edit_proxy(&c, &edit, &entry, row.revision)
                .await
                .map_err(|e| CmdError::from_team(e, &doing(need("edit", "proxy"), "proxy", &row.id, Some(row.revision))))?
        }
        None => {
            if entry.id.is_empty() {
                entry.id = uuid::Uuid::new_v4().to_string();
            }
            doors::clear(door, team, &Ask { new: true, ..Default::default() })?;
            let create = perm::team_write::<Create>(team, On::Team)?;
            t.commit().await?;
            remote::create_proxy(&c, &create, &entry.id, &entry)
                .await
                .map_err(|e| CmdError::from_team(e, &doing(need("create", "team"), "proxy", &entry.id, None)))?
        }
    };
    entry.name = w.name.clone();
    cache::wrote_proxy(team, remote::proxy_row_after(&w), Some(remote::proxy_data(&entry)));
    Ok(entry)
}

/// An identical team proxy this member can read is used as it is.
pub async fn save_proxy_dedup(door: Door, t: &Ticket, team: &str, mut entry: ProxyEntry) -> Result<ProxyEntry, CmdError> {
    if let Some(same) = cache::proxies(team).into_iter().find(|p| p.same_proxy(&entry)) {
        return Ok(same);
    }
    entry.id.clear();
    save_proxy(door, t, team, entry).await
}

/// A new password for the profile's own proxy changes that proxy.
pub async fn save_proxy_for_binding(
    door: Door,
    t: &Ticket,
    team: &str,
    entry: ProxyEntry,
    bound_id: Option<&str>,
) -> Result<ProxyEntry, CmdError> {
    let held = cache::proxies(team);
    if let Some(same) = held.iter().find(|p| p.same_proxy(&entry)) {
        return Ok(same.clone());
    }
    let own = bound_id.filter(|b| !b.is_empty()).and_then(|b| {
        held.into_iter().find(|p| {
            p.id == b && p.kind == entry.kind && p.host == entry.host && p.port == entry.port && p.username == entry.username
        })
    });
    match own {
        Some(mut p) => {
            p.password = entry.password;
            save_proxy(door, t, team, p).await
        }
        None => save_proxy_dedup(door, t, team, entry).await,
    }
}

/// Each one the team has no identical copy of; answers how many went in.
pub async fn save_proxies(door: Door, t: &Ticket, team: &str, entries: Vec<ProxyEntry>) -> Result<usize, CmdError> {
    doors::clear(door, team, &Ask { new: true, ..Default::default() })?;
    let mut held = cache::proxies(team);
    let mut added = 0;
    for mut e in entries {
        if held.iter().any(|p| p.same_proxy(&e)) {
            continue;
        }
        e.id.clear();
        held.push(save_proxy(door, t, team, e).await?);
        added += 1;
    }
    Ok(added)
}

/// To the team's trash.
pub async fn delete_proxy(door: Door, t: &Ticket, team: &str, id: &str) -> Result<(), CmdError> {
    let row = cache::proxy_row(team, id).ok_or_else(|| missing("proxy", id))?;
    doors::clear(door, team, &Ask { proxy: Some(id), ..Default::default() })?;
    let del = perm::team_write::<Delete>(team, On::Proxy(id.to_string()))?;
    let c = client(team)?;
    t.commit().await?;
    remote::delete_proxy(&c, &del, row.revision)
        .await
        .map_err(|e| CmdError::from_team(e, &doing(need("delete", "proxy"), "proxy", id, Some(row.revision))))?;
    cache::dropped_proxy(team, id);
    Ok(())
}
