//! Writes to a team's bookmarks, extensions, projects, tags and profile templates: the door's
//! rule, a pass on the team from `perm`, the server from the revision held,
//! then the copy held here. An extension's files go up as a package of their
//! own, and come down only into an open team profile's copy.

use std::path::{Path, PathBuf};

use anyhow::{bail, Result};

use super::automation::{Bundle, Project};
use super::bookmarks::Bookmark;
use super::client::TeamError;
use super::doors::{self, Ask, Door};
use super::error::{CmdError, Doing, Need};
use super::extensions::ExtensionEntry;
use super::local;
use super::local::extensions::source::SourceRef;
use super::perm::{self, Create, Delete, Edit, On};
use super::remote::{self, ItemRow, Package};
use super::local::templates::{ProfileTemplate, TemplateState};
use super::tags::Tag;
use super::write::client;
use super::{cache, secrets, Ticket};

/// The largest extension package a team keeps.
pub const PACKAGE_MAX: u64 = 64 * 1024 * 1024;

fn missing(what: &str, id: &str) -> CmdError {
    CmdError::Missing(format!("no {what} {id} in this team"))
}

fn refused(e: TeamError, right: &'static str, what: &'static str, id: &str, had: Option<u64>) -> CmdError {
    CmdError::from_team(e, &Doing { need: Need { right, on: "team" }, what, id, had })
}

/// The item `id` of `kind` as `data`: made when none is held, else changed from
/// `base` (the revision held when not given).
#[allow(clippy::too_many_arguments)]
async fn put(
    door: Door,
    t: &Ticket,
    team: &str,
    kind: &str,
    what: &'static str,
    id: &str,
    name: &str,
    data: String,
    base: Option<u64>,
) -> Result<ItemRow, CmdError> {
    let held = cache::item(team, kind, id);
    doors::clear(door, team, &Ask { new: held.is_none(), ..Default::default() })?;
    let c = client(team)?;
    let name = remote::item_name(name);
    let w = match &held {
        None => {
            let pass = perm::team_write::<Create>(team, On::Team)?;
            t.commit().await?;
            remote::create_item(&c, &pass, kind, id, &name, &data)
                .await
                .map_err(|e| refused(e, "create", what, id, None))?
        }
        Some(row) => {
            let base = base.unwrap_or(row.revision);
            let pass = perm::team_write::<Edit>(team, On::Team)?;
            t.commit().await?;
            remote::edit_item(&c, &pass, kind, id, &name, &data, base)
                .await
                .map_err(|e| refused(e, "edit", what, id, Some(base)))?
        }
    };
    let row = remote::item_after(id, &w, data, held.and_then(|h| h.package));
    cache::wrote_item(team, kind, row.clone());
    Ok(row)
}

/// To the team's trash, from the revision held.
async fn remove(door: Door, t: &Ticket, team: &str, kind: &str, what: &'static str, id: &str) -> Result<(), CmdError> {
    let row = cache::item(team, kind, id).ok_or_else(|| missing(what, id))?;
    doors::clear(door, team, &Ask::default())?;
    let pass = perm::team_write::<Delete>(team, On::Team)?;
    let c = client(team)?;
    t.commit().await?;
    remote::delete_item(&c, &pass, kind, id, row.revision)
        .await
        .map_err(|e| refused(e, "delete", what, id, Some(row.revision)))?;
    cache::dropped_item(team, kind, id);
    Ok(())
}

fn in_trash(e: &CmdError) -> bool {
    matches!(e, CmdError::Refused { code, .. } if code == "in_trash")
}

// ---- bookmarks ----

pub async fn save_bookmark(door: Door, t: &Ticket, team: &str, b: Bookmark) -> Result<Bookmark> {
    let mut b = local::bookmarks::cleaned(b)?;
    if b.id.is_empty() {
        b.id = uuid::Uuid::new_v4().to_string();
    }
    put(door, t, team, "bookmark", "bookmark", &b.id, &b.title, remote::item_data(&b), None).await?;
    Ok(b)
}

pub async fn delete_bookmark(door: Door, t: &Ticket, team: &str, id: &str) -> Result<()> {
    Ok(remove(door, t, team, "bookmark", "bookmark", id).await?)
}

// ---- tags: the team's list is by name ----

/// A deleted tag's id stays taken in the team's trash: one put back gets a new id.
pub async fn save_tag(door: Door, t: &Ticket, team: &str, tag: Tag) -> Result<Tag> {
    let mut held = cache::tags(team);
    let tag = held.upsert(tag, None)?;
    match put(door, t, team, "tag", "tag", &tag.id, &tag.name, remote::item_data(&tag), None).await {
        Ok(_) => Ok(tag),
        Err(e) if in_trash(&e) && cache::item(team, "tag", &tag.id).is_none() => {
            let tag = Tag { id: format!("t-{}", &uuid::Uuid::new_v4().simple().to_string()[..8]), ..tag };
            put(door, t, team, "tag", "tag", &tag.id, &tag.name, remote::item_data(&tag), None).await?;
            Ok(tag)
        }
        Err(e) => Err(e.into()),
    }
}

/// The tag goes; the profiles that carried it show it no more.
pub async fn delete_tag(door: Door, t: &Ticket, team: &str, id: &str) -> Result<()> {
    Ok(remove(door, t, team, "tag", "tag", id).await?)
}

// ---- profile templates: by name, one of them the team's default ----

async fn put_template(door: Door, t: &Ticket, team: &str, tpl: &ProfileTemplate) -> Result<ItemRow, CmdError> {
    put(door, t, team, "template", "profile template", &tpl.id, &tpl.name, remote::item_data(tpl), None).await
}

/// A deleted template's id stays taken in the team's trash: one put back gets a new id.
/// Made the default, it takes the mark from the others this member may change.
pub async fn save_template(door: Door, t: &Ticket, team: &str, tpl: ProfileTemplate) -> Result<ProfileTemplate> {
    let mut held = TemplateState { templates: cache::templates(team) };
    let tpl = held.upsert(tpl)?;
    let tpl = match put_template(door, t, team, &tpl).await {
        Ok(_) => tpl,
        Err(e) if in_trash(&e) && cache::item(team, "template", &tpl.id).is_none() => {
            let tpl = ProfileTemplate { id: format!("pt-{}", &uuid::Uuid::new_v4().simple().to_string()[..8]), ..tpl };
            put_template(door, t, team, &tpl).await?;
            tpl
        }
        Err(e) => return Err(e.into()),
    };
    if tpl.is_default {
        for other in cache::templates(team).into_iter().filter(|o| o.is_default && o.id != tpl.id) {
            let other = ProfileTemplate { is_default: false, ..other };
            if let Err(e) = put_template(door, t, team, &other).await {
                eprintln!("[launcher] template {} kept its default mark: {e}", other.id);
            }
        }
    }
    Ok(tpl)
}

pub async fn delete_template(door: Door, t: &Ticket, team: &str, id: &str) -> Result<()> {
    Ok(remove(door, t, team, "template", "profile template", id).await?)
}

// ---- automation projects: their secret values stay on this machine ----

/// The team's projects with this machine's secret values in them.
pub fn projects(team: &str) -> Vec<Project> {
    let mut all = cache::projects(team);
    secrets::fill(team, &mut all);
    all
}

async fn put_project(door: Door, t: &Ticket, team: &str, mut p: Project, base: Option<u64>) -> Result<Project> {
    p.updated_at = local::automation::stamp();
    p.revision = None;
    p.run.profiles.clear();
    local::automation::unique_block_ids(&mut p);
    let shown = p.clone();
    let values = secrets::take(&mut p);
    let id = p.id.clone();
    let row = put(door, t, team, "automation", "project", &id, &p.name, remote::item_data(&p), base).await?;
    secrets::keep(team, &id, values)?;
    Ok(Project { revision: Some(row.revision), ..shown })
}

pub async fn create_project(door: Door, t: &Ticket, team: &str, name: &str) -> Result<Project> {
    put_project(door, t, team, local::automation::new_project(name), None).await
}

/// From the revision the project was read at, when it says.
pub async fn save_project(door: Door, t: &Ticket, team: &str, p: Project) -> Result<Project> {
    if cache::item(team, "automation", &p.id).is_none() {
        return Err(missing("project", &p.id).into());
    }
    let base = p.revision;
    put_project(door, t, team, p, base).await
}

pub async fn duplicate_project(door: Door, t: &Ticket, team: &str, id: &str) -> Result<Project> {
    let src = projects(team).into_iter().find(|p| p.id == id).ok_or_else(|| missing("project", id))?;
    put_project(door, t, team, local::automation::copy_of(src), None).await
}

/// The door first: modules are installed only for an import that may happen.
pub async fn import_project(door: Door, t: &Ticket, team: &str, bundle: Bundle) -> Result<Project> {
    doors::clear(door, team, &Ask { new: true, ..Default::default() })?;
    let p = local::automation::unbundle(bundle)?;
    put_project(door, t, team, p, None).await
}

pub async fn delete_project(door: Door, t: &Ticket, team: &str, id: &str) -> Result<()> {
    remove(door, t, team, "automation", "project", id).await?;
    secrets::drop_project(team, id);
    Ok(())
}

// ---- extensions ----

/// A folder of its own while an import is made, gone with it.
struct Stage(PathBuf);

impl Stage {
    fn new() -> Result<Stage> {
        let p = std::env::temp_dir().join(format!("shardx-team-ext-{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir_all(&p)?;
        Ok(Stage(p))
    }
}

impl Drop for Stage {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// Unpacked and read in a stage, sent up as an item and its package; never
/// kept in this machine's extension library.
pub async fn import_extension(door: Door, t: &Ticket, team: &str, src: &Path) -> Result<ExtensionEntry> {
    import_extension_as(door, t, team, src, local::extensions::Want::file()).await
}

/// A store item the team holds already is not added again: the copy would
/// carry the store's key, so the same id.
fn refuse_same_item(team: &str, src: Option<&SourceRef>) -> Result<()> {
    let Some(src) = src else { return Ok(()) };
    match cache::extensions(team).into_iter().find(|e| e.source.as_ref() == Some(src)) {
        Some(e) => Err(local::extensions::already_in_library(&e.id, &e.version, false).into()),
        None => Ok(()),
    }
}

async fn import_extension_as(door: Door, t: &Ticket, team: &str, src: &Path, want: local::extensions::Want) -> Result<ExtensionEntry> {
    doors::clear(door, team, &Ask { new: true, ..Default::default() })?;
    perm::team_write::<Create>(team, On::Team)?;
    let stage = Stage::new()?;
    let id = uuid::Uuid::new_v4().simple().to_string();
    let (from, files, zip, at) = (src.to_path_buf(), stage.0.join("files"), stage.0.join("package.zip"), id.clone());
    let (mut entry, size) = tokio::task::spawn_blocking(move || -> Result<(ExtensionEntry, u64)> {
        let origin = local::extensions::unpack_into(&from, &files, &want)?;
        let mut entry = local::extensions::read_at(&files, &at)?;
        // Where it came from goes up with the item; how it was learned stays here.
        entry.source = origin.source;
        let size = local::extensions::zip_dir(&local::extensions::manifest_root(&files), &zip)?;
        Ok((entry, size))
    })
    .await??;
    refuse_same_item(team, entry.source.as_ref())?;
    if size > PACKAGE_MAX {
        bail!("this extension is {} MB packed; a team keeps extensions up to {} MB", size >> 20, PACKAGE_MAX >> 20);
    }
    entry.path = String::new();
    entry.added_at = format!("@{}", now());
    let row = put(door, t, team, "extension", "extension", &id, &entry.name, remote::item_data(&entry), None).await?;
    let c = client(team)?;
    let pass = perm::team_write::<Create>(team, On::Team)?;
    match remote::put_package(&c, &pass, &id, &stage.0.join("package.zip")).await {
        Ok(w) => {
            let package = Some(Package { size: w.size, package_rev: w.package_rev });
            cache::wrote_item(team, "extension", ItemRow { package, ..row });
            Ok(entry)
        }
        Err(e) => {
            // Without its files it is of no use: taken back where the rights allow.
            if let Ok(del) = perm::team_write::<Delete>(team, On::Team) {
                if remote::delete_item(&c, &del, "extension", &id, row.revision).await.is_ok() {
                    cache::dropped_item(team, "extension", &id);
                }
            }
            Err(refused(e, "create", "extension", &id, None).into())
        }
    }
}

/// Fetched into a stage, then imported as a file; a store link's package must be the store's.
pub async fn import_extension_url(door: Door, t: &Ticket, team: &str, url: &str) -> Result<ExtensionEntry> {
    doors::clear(door, team, &Ask { new: true, ..Default::default() })?;
    perm::team_write::<Create>(team, On::Team)?;
    let want = local::extensions::Want::of_link(url);
    if let Some(id) = &want.link {
        refuse_same_item(team, Some(&SourceRef::webstore(id)))?;
    }
    let stage = Stage::new()?;
    let file = stage.0.join("download.crx");
    local::extensions::download(url, &file).await?;
    import_extension_as(door, t, team, &file, want).await
}

pub async fn delete_extension(door: Door, t: &Ticket, team: &str, id: &str) -> Result<()> {
    Ok(remove(door, t, team, "extension", "extension", id).await?)
}

/// A team profile's extensions, unpacked under `into` in its open copy. One the
/// team's list here does not show is asked for as the profile's start needs it;
/// one that cannot be had is left out, and the person is told.
pub async fn unpack_for_profile(team: &str, profile: (&str, &str), ids: &[String], into: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if ids.is_empty() {
        return out;
    }
    let (profile_id, name) = profile;
    let mut left = 0;
    match client(team) {
        Ok(c) => {
            for id in ids {
                let (for_start, files) = match cache::item(team, "extension", id) {
                    Some(row) => (None, row.package.is_some()),
                    None => (Some(profile_id), true),
                };
                let got = match files {
                    true => fetch_one(&c, id, for_start, into).await,
                    false => Err(anyhow::anyhow!("it has no files")),
                };
                match got {
                    Ok(root) => out.push(root),
                    Err(e) => {
                        eprintln!("[launcher] extension {id} left out: {e:#}");
                        left += 1;
                    }
                }
            }
        }
        Err(e) => {
            eprintln!("[launcher] extensions left out: {e}");
            left = ids.len();
        }
    }
    if left > 0 {
        crate::notify_warning(format!(
            "{name} started without {left} of its extensions: the team server did not give them to this launcher."
        ));
    }
    out
}

/// A team profile's bookmarks as its start brings them, for a member the
/// team's own list is hidden from.
pub async fn bookmarks_for(team: &str, profile_id: &str) -> Vec<Bookmark> {
    let got = match client(team) {
        Ok(c) => remote::bookmarks_for(&c, profile_id).await.map_err(|e| e.to_string()),
        Err(e) => Err(e.to_string()),
    };
    got.unwrap_or_else(|e| {
        eprintln!("[launcher] bookmarks of {profile_id} not read: {e}");
        Vec::new()
    })
}

async fn fetch_one(c: &super::client::TeamClient, id: &str, for_start: Option<&str>, into: &Path) -> Result<PathBuf> {
    let name = super::open::safe(id);
    let (dir, zip) = (into.join(&name), into.join(format!("{name}.zip")));
    std::fs::create_dir_all(into)?;
    remote::fetch_package(c, id, for_start, &zip, PACKAGE_MAX).await?;
    tokio::task::spawn_blocking(move || -> Result<PathBuf> {
        let unpacked = local::extensions::unpack_archive(&zip, &dir);
        let _ = std::fs::remove_file(&zip);
        unpacked?;
        let root = local::extensions::manifest_root(&dir);
        if !root.join("manifest.json").exists() {
            bail!("its files hold no manifest.json");
        }
        Ok(root)
    })
    .await?
}

#[cfg(test)]
pub(super) mod tests;
