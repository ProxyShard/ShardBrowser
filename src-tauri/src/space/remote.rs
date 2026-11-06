//! A team's lists as its server sends them, and how each row reads as the
//! launcher's own type. A data blob is the launcher's JSON, base64 on the wire.

use base64::{engine::general_purpose::STANDARD, Engine};
use serde::de::DeserializeOwned;
use serde::Deserialize;
use serde_json::{json, Value};
use team_model::Card;

use super::automation::Project;
use super::bookmarks::Bookmark;
use super::client::{Method, TeamClient, TeamError};
use super::extensions::ExtensionEntry;
use super::folders::{FolderMeta, FolderState};
use super::perm::{self, Create, Delete, Edit, Granted, On};
use super::profile::{claims_mobile, OpenOn, Origin, ProfileMeta, StoredProfile};
use super::proxy::{ProxyEntry, ProxyKind};
use super::tags::{Tag, TagState};

/// A row of `GET /profiles`: never the settings or the session.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct ProfileRow {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub folder: String,
    #[serde(default)]
    pub revision: u64,
    #[serde(default)]
    pub updated_at: u64,
    #[serde(default)]
    pub card: Card,
    #[serde(default)]
    pub session_rev: u64,
    #[serde(default)]
    pub open_on: Option<Lease>,
    /// Unix seconds; None from an older server, or for a row it kept from before.
    #[serde(default)]
    pub created_at: Option<u64>,
}

/// Who has a profile open, and on which device.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct Lease {
    #[serde(default)]
    pub device_id: String,
    #[serde(default)]
    pub device_name: String,
    #[serde(default)]
    pub member_name: String,
    #[serde(default)]
    pub acquired_at: u64,
    #[serde(default)]
    pub expires_at: u64,
}

/// `GET /profiles/:id`: the row and its settings.
#[derive(Deserialize)]
pub struct ProfileWithData {
    #[serde(flatten)]
    pub row: ProfileRow,
    #[serde(default)]
    pub data: String,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct ProxyRow {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub country: String,
    #[serde(default)]
    pub revision: u64,
    #[serde(default)]
    pub updated_at: u64,
}

/// `GET /proxies/:id`: the row and its login.
#[derive(Deserialize)]
pub struct ProxyWithData {
    #[serde(flatten)]
    pub row: ProxyRow,
    #[serde(default)]
    pub data: String,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct FolderRow {
    pub name: String,
    #[serde(default)]
    pub color: String,
}

/// A bookmark, extension, project or tag, with its data.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct ItemRow {
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub revision: u64,
    #[serde(default)]
    pub updated_at: u64,
    #[serde(default)]
    pub package: Option<Package>,
    #[serde(default)]
    pub data: String,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct Package {
    #[serde(default)]
    pub size: u64,
    #[serde(default)]
    pub package_rev: u64,
}

/// A blob's JSON; None when it is empty or not JSON.
fn opened(data: &str) -> Option<Value> {
    let bytes = STANDARD.decode(data.trim()).ok()?;
    if bytes.is_empty() {
        return None;
    }
    serde_json::from_slice(&bytes).ok()
}

fn some_text(s: &str) -> Option<String> {
    Some(s.to_string()).filter(|s| !s.is_empty())
}

fn open_on(lease: &Lease, device_id: &str) -> OpenOn {
    OpenOn {
        device_name: lease.device_name.clone(),
        member_name: lease.member_name.clone(),
        this_device: !device_id.is_empty() && lease.device_id == device_id,
        since: lease.acquired_at,
        until: lease.expires_at,
    }
}

/// A row as the profile list shows it; the settings fill in what only they know.
pub fn meta(team: &str, row: &ProfileRow, detail: Option<&StoredProfile>, device_id: &str) -> ProfileMeta {
    ProfileMeta {
        id: row.id.clone(),
        name: row.name.clone(),
        notes: row.card.notes.clone(),
        proxy_id: row.card.bound_proxy().map(str::to_owned),
        // Last launched is the lease: no count is kept for a team's profile.
        last_launched_at: row.open_on.as_ref().map(|l| format!("@{}", l.acquired_at)),
        // The server's date, else what this device read in the settings.
        created_at: row.created_at.map(|t| format!("@{t}")).or_else(|| detail.and_then(|d| d.meta.created_at.clone())),
        pinned: false,
        folder: row.folder.clone(),
        total_runtime_ms: 0,
        color: some_text(&row.card.color),
        extensions: detail.map(|d| d.meta.extensions.clone()).unwrap_or_default(),
        mobile: detail.is_some_and(|d| claims_mobile(&d.config)),
        android_media: detail.is_some_and(|d| d.meta.android_media),
        origin: Origin::Team,
        rights: Some(perm::rights(team, &row.id, &row.folder)),
        open_on: row.open_on.as_ref().map(|l| open_on(l, device_id)),
        tags: Some(row.card.tags.clone()),
        sync: None,
        protected: false,
    }
}

/// The settings as the editor reads them: the blob when there is one, with
/// the row's own fields over it. One member's marks are never the team's.
pub fn stored(row: &ProfileRow, data: Option<&str>) -> StoredProfile {
    let mut s: StoredProfile = data
        .and_then(opened)
        .and_then(|v| serde_json::from_value(v).ok())
        .unwrap_or_default();
    s.meta.id = row.id.clone();
    s.meta.folder = row.folder.clone();
    s.meta.rev = row.revision;
    s.meta.proxy_id = row.card.bound_proxy().map(str::to_owned);
    s.meta.color = some_text(&row.card.color);
    s.meta.pinned = false;
    s.meta.last_launched_at = row.open_on.as_ref().map(|l| format!("@{}", l.acquired_at));
    s.meta.total_runtime_ms = 0;
    s.meta.inline_proxy = None;
    s.meta.temporary = false;
    s.config.insert("name".into(), Value::String(row.name.clone()));
    s.config.insert("notes".into(), Value::String(row.card.notes.clone()));
    s
}

fn kind(raw: &str) -> Option<ProxyKind> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "socks5" => Some(ProxyKind::Socks5),
        "http" => Some(ProxyKind::Http),
        "https" => Some(ProxyKind::Https),
        _ => None,
    }
}

/// A team proxy; with no login read, it has no address either.
pub fn proxy(row: &ProxyRow, login: Option<&str>) -> ProxyEntry {
    let read: Option<ProxyEntry> = login.and_then(opened).and_then(|mut v| {
        let obj = v.as_object_mut()?;
        for (k, d) in [("name", json!("")), ("kind", json!("socks5")), ("host", json!("")), ("port", json!(0))] {
            obj.entry(k).or_insert(d);
        }
        serde_json::from_value(v).ok()
    });
    let mut e = read.unwrap_or_else(|| ProxyEntry {
        id: String::new(),
        name: String::new(),
        kind: ProxyKind::Socks5,
        host: String::new(),
        port: 0,
        username: String::new(),
        password: String::new(),
        country: String::new(),
        notes: String::new(),
    });
    e.id = row.id.clone();
    e.name = row.name.clone();
    e.country = row.country.clone();
    if let Some(k) = kind(&row.kind) {
        e.kind = k;
    }
    e
}

pub fn folders(rows: &[FolderRow]) -> FolderState {
    FolderState {
        registry: rows.iter().map(|f| f.name.clone()).collect(),
        meta: rows
            .iter()
            .filter(|f| !f.color.is_empty())
            .map(|f| (f.name.clone(), FolderMeta { color: Some(f.color.clone()), icon: "folder".into() }))
            .collect(),
    }
}

/// An item's data as `T`, with `fill` for what it leaves out; the row's id wins.
fn item<T: DeserializeOwned>(row: &ItemRow, fill: Value) -> Option<T> {
    let mut v = opened(&row.data).unwrap_or_else(|| json!({}));
    let obj = v.as_object_mut()?;
    if let Value::Object(fill) = fill {
        for (k, d) in fill {
            obj.entry(k).or_insert(d);
        }
    }
    obj.insert("id".into(), Value::String(row.id.clone()));
    serde_json::from_value(v).ok()
}

/// How many rows of an item kind do not read as the launcher's type.
pub fn unreadable(kind: &str, rows: &[ItemRow]) -> usize {
    let bad = |ok: bool| usize::from(!ok);
    rows.iter()
        .map(|r| match kind {
            "bookmark" => bad(bookmark(r).is_some()),
            "extension" => bad(extension(r).is_some()),
            "automation" => bad(project(r).is_some()),
            "tag" => bad(tag(r).is_some()),
            _ => 0,
        })
        .sum()
}

fn tag(r: &ItemRow) -> Option<Tag> {
    item(r, json!({ "name": r.name, "color": "" }))
}

fn bookmark(r: &ItemRow) -> Option<Bookmark> {
    item(r, json!({ "title": r.name }))
}

fn extension(r: &ItemRow) -> Option<ExtensionEntry> {
    let size = r.package.as_ref().map_or(0, |p| p.size);
    let fill = json!({
        "name": r.name, "version": "", "description": "", "icon": "", "path": "",
        "size_bytes": size, "added_at": format!("@{}", r.updated_at),
    });
    // Its files are not on this computer.
    item(r, fill).map(|e: ExtensionEntry| ExtensionEntry { path: String::new(), ..e })
}

fn project(r: &ItemRow) -> Option<Project> {
    let fill = json!({ "name": r.name, "created_at": r.updated_at, "updated_at": r.updated_at });
    item(r, fill).map(|p: Project| Project { revision: Some(r.revision), ..p })
}

/// The team's tags, and which profile carries which by its card. A tag deleted
/// stays on the cards it was on, and is left out here.
pub fn tags(rows: &[ItemRow], profiles: &[ProfileRow]) -> TagState {
    let tags: Vec<Tag> = rows.iter().filter_map(tag).collect();
    let known = |id: &String| tags.iter().any(|t| &t.id == id);
    let by_profile = profiles
        .iter()
        .map(|p| (p.id.clone(), p.card.tags.iter().filter(|t| known(t)).cloned().collect::<Vec<_>>()))
        .filter(|(_, ids)| !ids.is_empty())
        .collect();
    TagState { tags, by_profile }
}

pub fn bookmarks(rows: &[ItemRow]) -> Vec<Bookmark> {
    rows.iter().filter_map(bookmark).collect()
}

pub fn extensions(rows: &[ItemRow]) -> Vec<ExtensionEntry> {
    rows.iter().filter_map(extension).collect()
}

pub fn projects(rows: &[ItemRow]) -> Vec<Project> {
    rows.iter().filter_map(project).collect()
}

// ---- writing back ----

/// What a profile or proxy write answers: the row's own fields.
#[derive(Debug, Clone, Deserialize)]
pub struct Written {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub folder: String,
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub country: String,
    pub revision: u64,
    #[serde(default)]
    pub updated_at: u64,
    #[serde(default)]
    pub created_at: Option<u64>,
}

/// The settings the team keeps: the launcher's JSON without one member's marks.
pub fn profile_data(stored: &StoredProfile) -> String {
    let mut s = stored.clone();
    s.meta.pinned = false;
    s.meta.last_launched_at = None;
    s.meta.total_runtime_ms = 0;
    s.meta.temporary = false;
    s.meta.inline_proxy = None;
    s.meta.rev = 0;
    STANDARD.encode(serde_json::to_vec(&s).unwrap_or_default())
}

/// The name the server lists; it refuses a blank one.
pub fn name_of(stored: &StoredProfile) -> String {
    let name = stored.config.get("name").and_then(Value::as_str).unwrap_or("").trim();
    if name.is_empty() {
        "(unnamed)".into()
    } else {
        name.to_string()
    }
}

/// What lists show beside the name; the binding lives here.
pub fn card(stored: &StoredProfile, tags: Vec<String>) -> Card {
    Card {
        proxy_id: stored.meta.proxy_id.as_deref().map(str::trim).filter(|p| !p.is_empty()).map(str::to_owned),
        color: stored.meta.color.clone().unwrap_or_default(),
        notes: stored.config.get("notes").and_then(Value::as_str).unwrap_or("").to_string(),
        tags,
    }
}

/// The row after a write from here: the server's fields, the card sent, the rest as it was.
pub fn row_after(w: &Written, card: Option<Card>, before: Option<&ProfileRow>) -> ProfileRow {
    let card = card.or_else(|| before.map(|b| b.card.clone())).unwrap_or_default();
    ProfileRow {
        id: w.id.clone(),
        name: w.name.clone(),
        folder: w.folder.clone(),
        revision: w.revision,
        updated_at: w.updated_at,
        card: Card { proxy_id: card.bound_proxy().map(str::to_owned), ..card },
        session_rev: before.map_or(0, |b| b.session_rev),
        open_on: before.and_then(|b| b.open_on.clone()),
        created_at: w.created_at.or_else(|| before.and_then(|b| b.created_at)),
    }
}

/// A launcher date ("@<unix seconds>", or ISO from older or imported data) as unix seconds.
pub fn stamp_secs(stamp: Option<&str>) -> Option<u64> {
    let s = stamp?.trim();
    let secs = match s.strip_prefix('@') {
        Some(n) => n.parse::<i64>().ok()?,
        None => crate::psapi::parse_ts(&Value::String(s.to_string()))?,
    };
    u64::try_from(secs).ok().filter(|&t| t > 0)
}

/// When a profile was made: the row's date, else the one its settings carry.
pub fn made_at(row: &ProfileRow, data: &str) -> Option<u64> {
    row.created_at.or_else(|| stamp_secs(opened(data)?.get("_meta")?.get("created_at")?.as_str()))
}

pub fn proxy_row_after(w: &Written) -> ProxyRow {
    ProxyRow {
        id: w.id.clone(),
        name: w.name.clone(),
        kind: w.kind.clone(),
        country: w.country.clone(),
        revision: w.revision,
        updated_at: w.updated_at,
    }
}

/// A proxy's login and the rest of it, as the team keeps it.
pub fn proxy_data(entry: &ProxyEntry) -> String {
    STANDARD.encode(serde_json::to_vec(entry).unwrap_or_default())
}

/// The name the server lists for a proxy; it refuses a blank one.
pub fn proxy_name(entry: &ProxyEntry) -> String {
    let name = entry.name.trim();
    if name.is_empty() {
        format!("{}:{}", entry.host, entry.port)
    } else {
        name.to_string()
    }
}

pub struct ProfileFields<'a> {
    pub name: &'a str,
    /// Base64 of the settings.
    pub data: &'a str,
    /// None keeps the card the server has.
    pub card: Option<&'a Card>,
    /// Unix seconds; the server keeps it only for a row with no date yet.
    pub created_at: Option<u64>,
}

fn path_of(kind: &str, id: &str) -> Result<String, TeamError> {
    let seg = super::admin::seg(id).map_err(|e| TeamError::Garbled(e.to_string()))?;
    Ok(format!("/{kind}/{seg}"))
}

/// The profile as the server has it now, settings included.
pub async fn read_profile(c: &TeamClient, id: &str) -> Result<ProfileWithData, TeamError> {
    c.get(&path_of("profiles", id)?).await
}

async fn put_profile(c: &TeamClient, id: &str, folder: &str, f: &ProfileFields<'_>, base: u64) -> Result<Written, TeamError> {
    let mut body = json!({ "name": f.name, "folder": folder, "data": f.data, "base_revision": base });
    if let Some(card) = f.card {
        body["card"] = serde_json::to_value(card).unwrap_or_default();
    }
    if let Some(t) = f.created_at {
        body["created_at"] = t.into();
    }
    c.json(Method::PUT, &path_of("profiles", id)?, Some(&body)).await
}

fn wrong(what: &str) -> TeamError {
    TeamError::Garbled(format!("a pass for something else than {what}"))
}

/// A new profile, in the folder its pass is for; refused if the id is taken.
pub async fn create_profile(c: &TeamClient, pass: &Granted<Create>, id: &str, f: &ProfileFields<'_>) -> Result<Written, TeamError> {
    let On::Folder(folder) = pass.on() else { return Err(wrong("a folder")) };
    put_profile(c, id, folder, f, 0).await
}

/// A profile changed from revision `base`; moved too when `to` is a pass on another folder.
pub async fn edit_profile(
    c: &TeamClient,
    pass: &Granted<Edit>,
    to: Option<&Granted<Create>>,
    f: &ProfileFields<'_>,
    base: u64,
) -> Result<Written, TeamError> {
    let On::Profile { id, folder } = pass.on() else { return Err(wrong("a profile")) };
    let folder = match to.map(|t| t.on()) {
        Some(On::Folder(dest)) => dest,
        Some(_) => return Err(wrong("a folder")),
        None => folder,
    };
    put_profile(c, id, folder, f, base).await
}

/// To the team's trash; refused once the profile moved past `base`.
pub async fn delete_profile(c: &TeamClient, pass: &Granted<Delete>, base: u64) -> Result<(), TeamError> {
    let On::Profile { id, .. } = pass.on() else { return Err(wrong("a profile")) };
    let path = format!("{}?base_revision={base}", path_of("profiles", id)?);
    c.call(Method::DELETE, &path, None).await.map(|_| ())
}

async fn put_proxy(c: &TeamClient, id: &str, e: &ProxyEntry, base: u64) -> Result<Written, TeamError> {
    let kind = serde_json::to_value(&e.kind).ok().and_then(|v| v.as_str().map(str::to_owned)).unwrap_or_default();
    let body = json!({
        "name": proxy_name(e), "kind": kind, "country": e.country, "data": proxy_data(e), "base_revision": base,
    });
    c.json(Method::PUT, &path_of("proxies", id)?, Some(&body)).await
}

/// A new team proxy; refused if the id is taken.
pub async fn create_proxy(c: &TeamClient, pass: &Granted<Create>, id: &str, e: &ProxyEntry) -> Result<Written, TeamError> {
    if pass.on() != &On::Team {
        return Err(wrong("the team"));
    }
    put_proxy(c, id, e, 0).await
}

pub async fn edit_proxy(c: &TeamClient, pass: &Granted<Edit>, e: &ProxyEntry, base: u64) -> Result<Written, TeamError> {
    let On::Proxy(id) = pass.on() else { return Err(wrong("a proxy")) };
    put_proxy(c, id, e, base).await
}

pub async fn delete_proxy(c: &TeamClient, pass: &Granted<Delete>, base: u64) -> Result<(), TeamError> {
    let On::Proxy(id) = pass.on() else { return Err(wrong("a proxy")) };
    let path = format!("{}?base_revision={base}", path_of("proxies", id)?);
    c.call(Method::DELETE, &path, None).await.map(|_| ())
}

// ---- bookmarks, extensions, projects and tags ----

/// What an item write answers.
#[derive(Debug, Clone, Deserialize)]
pub struct ItemWritten {
    pub revision: u64,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub updated_at: u64,
}

/// What a package upload answers.
#[derive(Debug, Clone, Deserialize)]
pub struct PackageWritten {
    pub package_rev: u64,
    #[serde(default)]
    pub size: u64,
}

/// The longest name the server lists.
const ITEM_NAME_MAX: usize = 200;

/// An item's name as the server takes it.
pub fn item_name(s: &str) -> String {
    s.trim().chars().take(ITEM_NAME_MAX).collect()
}

/// The launcher's JSON as an item's data.
pub fn item_data<T: serde::Serialize>(v: &T) -> String {
    STANDARD.encode(serde_json::to_vec(v).unwrap_or_default())
}

fn item_path(kind: &str, id: &str) -> Result<String, TeamError> {
    let seg = super::admin::seg(id).map_err(|e| TeamError::Garbled(e.to_string()))?;
    Ok(format!("/items/{kind}/{seg}"))
}

async fn put_item(c: &TeamClient, kind: &str, id: &str, name: &str, data: &str, base: u64) -> Result<ItemWritten, TeamError> {
    let body = json!({ "name": name, "data": data, "base_revision": base });
    c.json(Method::PUT, &item_path(kind, id)?, Some(&body)).await
}

/// A new item of the team's; refused if the id is taken.
pub async fn create_item(
    c: &TeamClient,
    pass: &Granted<Create>,
    kind: &str,
    id: &str,
    name: &str,
    data: &str,
) -> Result<ItemWritten, TeamError> {
    if pass.on() != &On::Team {
        return Err(wrong("the team"));
    }
    put_item(c, kind, id, name, data, 0).await
}

/// An item changed from revision `base`.
pub async fn edit_item(
    c: &TeamClient,
    pass: &Granted<Edit>,
    kind: &str,
    id: &str,
    name: &str,
    data: &str,
    base: u64,
) -> Result<ItemWritten, TeamError> {
    if pass.on() != &On::Team {
        return Err(wrong("the team"));
    }
    put_item(c, kind, id, name, data, base).await
}

/// To the team's trash; refused once the item moved past `base`.
pub async fn delete_item(c: &TeamClient, pass: &Granted<Delete>, kind: &str, id: &str, base: u64) -> Result<(), TeamError> {
    if pass.on() != &On::Team {
        return Err(wrong("the team"));
    }
    let path = format!("{}?base_revision={base}", item_path(kind, id)?);
    c.call(Method::DELETE, &path, None).await.map(|_| ())
}

/// A new extension's files, from the zip at `from`: only while it has none.
pub async fn put_package(c: &TeamClient, pass: &Granted<Create>, id: &str, from: &std::path::Path) -> Result<PackageWritten, TeamError> {
    if pass.on() != &On::Team {
        return Err(wrong("the team"));
    }
    let path = format!("{}/package?base_package_rev=0", item_path("extension", id)?);
    c.send_file(&path, from).await
}

/// An extension's files, as a zip into `to`; `for_start` asks for them as that
/// profile's start needs them, which Start alone gets.
pub async fn fetch_package(
    c: &TeamClient,
    id: &str,
    for_start: Option<&str>,
    to: &std::path::Path,
    max: u64,
) -> Result<u64, TeamError> {
    let q = for_start.map(|p| format!("?profile={}", query_arg(p))).unwrap_or_default();
    c.fetch(&format!("{}/package{q}", item_path("extension", id)?), to, max).await
}

/// The bookmarks a team profile's start brings: its folder's and those for every profile.
pub async fn bookmarks_for(c: &TeamClient, profile_id: &str) -> Result<Vec<Bookmark>, TeamError> {
    let rows: Vec<ItemRow> = c.get(&format!("/items/bookmark?profile={}", query_arg(profile_id))).await?;
    Ok(bookmarks(&rows))
}

fn query_arg(s: &str) -> String {
    url::form_urlencoded::byte_serialize(s.as_bytes()).collect()
}

/// The row after a write from here: the server's fields, the data sent.
pub fn item_after(id: &str, w: &ItemWritten, data: String, package: Option<Package>) -> ItemRow {
    ItemRow { id: id.to_string(), name: w.name.clone(), revision: w.revision, updated_at: w.updated_at, package, data }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn b64(v: Value) -> String {
        STANDARD.encode(v.to_string())
    }

    fn row(id: &str) -> ProfileRow {
        serde_json::from_value(json!({
            "id": id, "name": "Shop 1", "folder": "Ads", "revision": 4, "updated_at": 10,
            "updated_by": "m1", "session_rev": 2, "session_size": 9,
            "card": { "proxy_id": " ", "color": "#ff0000", "notes": "card notes", "tags": ["t1"] },
            "open_on": {
                "member_id": "m2", "device_id": "dev-b", "device_name": "Vera's Mac",
                "acquired_at": 100, "expires_at": 190, "member_name": "Vera"
            }
        }))
        .unwrap()
    }

    #[test]
    fn a_profile_row_reads_as_the_teams_with_its_lease_and_card() {
        let m = meta("t1", &row("p1"), None, "dev-b");
        assert_eq!(m.origin, Origin::Team);
        assert_eq!((m.name.as_str(), m.folder.as_str(), m.notes.as_str()), ("Shop 1", "Ads", "card notes"));
        assert_eq!(m.proxy_id, None, "a blank binding is none");
        assert_eq!(m.color.as_deref(), Some("#ff0000"));
        assert_eq!(m.tags.as_deref(), Some(&["t1".to_string()][..]));
        let on = m.open_on.unwrap();
        assert_eq!((on.device_name.as_str(), on.member_name.as_str(), on.this_device), ("Vera's Mac", "Vera", true));
        assert_eq!((on.since, on.until), (100, 190));
        assert_eq!(m.last_launched_at.as_deref(), Some("@100"));
        // No permission held: no right shown.
        assert_eq!(m.rights, Some(perm::Rights::default()));
        let shown = serde_json::to_value(meta("t1", &row("p1"), None, "dev-a")).unwrap();
        assert_eq!(shown["origin"], "team");
        assert_eq!(shown["open_on"]["this_device"], false);
        assert_eq!(shown["rights"]["take_out"], false);
    }

    #[test]
    fn the_settings_keep_the_teams_fields_over_the_blob() {
        let blob = b64(json!({
            "_meta": { "id": "other", "folder": "Elsewhere", "pinned": true, "total_runtime_ms": 5, "rev": 1,
                       "temporary": true, "extensions": ["e1"] },
            "name": "old name", "navigator": { "user_agent": "Mozilla/5.0 (Linux; Android 14)" }
        }));
        let s = stored(&row("p1"), Some(&blob));
        assert_eq!((s.meta.id.as_str(), s.meta.folder.as_str(), s.meta.rev), ("p1", "Ads", 4));
        assert!(!s.meta.pinned && !s.meta.temporary && s.meta.total_runtime_ms == 0);
        assert_eq!(s.config["name"], "Shop 1");
        assert_eq!(s.meta.extensions, ["e1"]);
        let m = meta("t1", &row("p1"), Some(&s), "");
        assert!(m.mobile, "read from the settings");
        // Nothing to read: the row alone.
        let bare = stored(&row("p1"), Some(""));
        assert_eq!((bare.meta.id.as_str(), bare.config["notes"].as_str()), ("p1", Some("card notes")));
    }

    #[test]
    fn the_list_takes_the_servers_creation_date_over_the_settings() {
        let blob = b64(json!({ "_meta": { "created_at": "@1600000000" }, "name": "x" }));
        let s = stored(&row("p1"), Some(&blob));
        // An older server's row: what the settings read here say.
        assert_eq!(meta("t1", &row("p1"), Some(&s), "").created_at.as_deref(), Some("@1600000000"));
        assert_eq!(meta("t1", &row("p1"), None, "").created_at, None);
        let mut dated = row("p1");
        dated.created_at = Some(1_700_000_000);
        for detail in [Some(&s), None] {
            assert_eq!(meta("t1", &dated, detail, "").created_at.as_deref(), Some("@1700000000"));
        }
        // What is sent: the row's, else the settings'.
        assert_eq!(made_at(&dated, &blob), Some(1_700_000_000));
        assert_eq!(made_at(&row("p1"), &blob), Some(1_600_000_000));
        assert_eq!(made_at(&row("p1"), ""), None);
        // A write's answer keeps it, or the row's before when an older server says none.
        let w: Written = serde_json::from_value(json!({ "id": "p1", "name": "x", "revision": 5 })).unwrap();
        assert_eq!(row_after(&w, None, Some(&dated)).created_at, Some(1_700_000_000));
        let w: Written = serde_json::from_value(json!({ "id": "p1", "name": "x", "revision": 5, "created_at": 9 })).unwrap();
        assert_eq!(row_after(&w, None, None).created_at, Some(9));
    }

    #[test]
    fn a_launcher_date_reads_as_seconds_whichever_way_it_was_written() {
        assert_eq!(stamp_secs(Some("@1700000000")), Some(1_700_000_000));
        assert_eq!(stamp_secs(Some("2023-11-14T22:13:20Z")), Some(1_700_000_000));
        assert_eq!(stamp_secs(Some("2023-11-15T00:13:20+02:00")), Some(1_700_000_000));
        for junk in [None, Some(""), Some("@"), Some("@-5"), Some("@0"), Some("yesterday")] {
            assert_eq!(stamp_secs(junk), None, "{junk:?}");
        }
    }

    #[test]
    fn items_read_as_the_launchers_own_and_a_broken_one_is_left_out() {
        let r = |id: &str, name: &str, data: Value| ItemRow {
            id: id.into(),
            name: name.into(),
            revision: 1,
            updated_at: 50,
            package: None,
            data: b64(data),
        };
        let marks = [
            r("b1", "Mail", json!({ "url": "https://mail.example", "folder": "Ads" })),
            r("b2", "Broken", json!({ "title": "no address" })),
        ];
        let got = bookmarks(&marks);
        assert_eq!(got.len(), 1);
        assert_eq!((got[0].id.as_str(), got[0].title.as_str(), got[0].folder.as_str()), ("b1", "Mail", "Ads"));
        assert_eq!(unreadable("bookmark", &marks), 1);

        let ext = [r("e1", "Wallet", json!({ "version": "1.2", "path": "/somewhere/else" }))];
        let got = extensions(&ext);
        assert_eq!((got[0].name.as_str(), got[0].version.as_str(), got[0].path.as_str()), ("Wallet", "1.2", ""));

        let tag_rows = [r("t1", "VIP", json!({ "color": "violet" }))];
        let state = tags(&tag_rows, &[row("p1")]);
        assert_eq!(state.tags[0], Tag { id: "t1".into(), name: "VIP".into(), color: "violet".into() });
        assert_eq!(state.by_profile["p1"], ["t1"]);

        let got = projects(&[r("a1", "Warm up", json!({ "notes": "x" }))]);
        assert_eq!((got[0].id.as_str(), got[0].name.as_str(), got[0].created_at), ("a1", "Warm up", 50));
    }

    #[test]
    fn a_proxy_without_its_login_has_no_address() {
        let row = ProxyRow { id: "x1".into(), name: "DE".into(), kind: "http".into(), country: "DE".into(), revision: 3, updated_at: 7 };
        let bare = proxy(&row, None);
        assert_eq!((bare.host.as_str(), bare.port, bare.kind.clone()), ("", 0, ProxyKind::Http));
        let login = b64(json!({ "id": "", "name": "old", "kind": "socks5", "host": "203.0.113.9", "port": 1080, "username": "u", "password": "p" }));
        let full = proxy(&row, Some(&login));
        assert_eq!((full.id.as_str(), full.name.as_str(), full.host.as_str(), full.port), ("x1", "DE", "203.0.113.9", 1080));
        assert_eq!(full.kind, ProxyKind::Http, "the row's kind is the server's");
    }
}
