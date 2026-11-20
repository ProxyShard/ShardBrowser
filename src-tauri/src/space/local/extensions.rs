//! Extension library. Name, description and icon are read out of the extension
//! itself, so the grid matches what the browser will load.

use crate::space::error::CmdError;
use crate::store;
use anyhow::{bail, Context, Result};
use base64::Engine;
use serde::{Deserialize, Serialize};
use source::{How, Origin, SourceLocal, SourceRef};
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

// Updates of extensions added from the Web Store: the pure parts, used from the next steps on.
#[allow(dead_code)]
pub(in crate::space) mod chrome_id;
#[allow(dead_code)]
pub(in crate::space) mod crx;
pub(in crate::space) mod widevine;
#[allow(dead_code)]
pub(in crate::space) mod manifest_edit;
pub(in crate::space) mod source;
#[allow(dead_code)]
pub(in crate::space) mod store_link;
pub(in crate::space) mod synced;
// Staging is reached from the update check (step 4) and sync (step 5).
#[allow(dead_code)]
pub(in crate::space) mod swap;
#[cfg(test)]
pub(in crate::space) mod testkit;
#[allow(dead_code)]
pub(in crate::space) mod update2;
pub(in crate::space) mod updates;
pub(in crate::space) mod usage;
#[allow(dead_code)]
pub(in crate::space) mod version;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtensionEntry {
    pub id: String,
    pub name: String,
    pub version: String,
    pub description: String,
    /// Largest icon the manifest declares, as a `data:` URL. Empty when the
    /// extension ships none.
    pub icon: String,
    /// Unpacked directory handed to `--load-extension`.
    pub path: String,
    pub size_bytes: u64,
    /// "@<unix_secs>", same marker profiles use.
    pub added_at: String,
    /// Where it came from, when known; travels with the item.
    #[serde(default, skip_serializing_if = "Option::is_none", deserialize_with = "source::lenient")]
    pub source: Option<SourceRef>,
}

/// A library folder's name: letters, digits, `-` and `_`, so never a dot-named
/// file of the launcher's, a path or a drive (`D:`).
pub(in crate::space) fn valid_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 64 && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

fn dir_for(id: &str) -> Result<PathBuf> {
    if !valid_id(id) {
        anyhow::bail!("invalid extension id");
    }
    Ok(store::extensions_dir()?.join(id))
}

/// `--load-extension` wants the directory holding manifest.json. A .crx often
/// unpacks with everything under one top-level folder; follow that down.
pub(in crate::space) fn manifest_root(dir: &Path) -> PathBuf {
    if dir.join("manifest.json").exists() {
        return dir.to_path_buf();
    }
    if let Ok(rd) = fs::read_dir(dir) {
        let subs: Vec<PathBuf> = rd
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.is_dir())
            .collect();
        if subs.len() == 1 && subs[0].join("manifest.json").exists() {
            return subs[0].clone();
        }
    }
    dir.to_path_buf()
}

pub(in crate::space) fn list() -> Result<Vec<ExtensionEntry>> {
    let root = store::extensions_dir()?;
    let mut out = Vec::new();
    for entry in fs::read_dir(&root)? {
        let entry = entry?;
        let id = entry.file_name().to_string_lossy().to_string();
        // Side files, downloads and stages sit beside the folders.
        if !entry.path().is_dir() || !valid_id(&id) {
            continue;
        }
        if let Ok(e) = entry_in(&root, &id) {
            out.push(e);
        }
    }
    out.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    Ok(out)
}

/// Here though `list()` leaves it out now: its manifest does not read, or a
/// swap has its folder aside. Sync takes it neither for changed nor for deleted.
pub(in crate::space) fn unlisted(lib: &Path, id: &str) -> bool {
    valid_id(id)
        && (manifest_root(&lib.join(id)).join("manifest.json").is_file()
            || swap::Paths::new(lib, id).prev_new().exists()
            || usage::busy(id))
}

/// Directory to hand `--load-extension`, or None when the id is unknown.
pub(in crate::space) fn load_path(id: &str) -> Option<PathBuf> {
    let dir = dir_for(id).ok()?;
    let root = manifest_root(&dir);
    root.join("manifest.json").exists().then_some(root)
}

/// The library's entry, its source and date from the side file.
fn entry_in(lib: &Path, id: &str) -> Result<ExtensionEntry> {
    if !valid_id(id) {
        bail!("invalid extension id");
    }
    let mut e = read_at(&lib.join(id), id)?;
    if let Some(side) = source::read(lib, id) {
        e.source = side.source;
        if let Some(at) = side.added_at {
            e.added_at = at;
        }
    }
    Ok(e)
}

/// The entry for the extension unpacked at `dir`, under `id`.
pub(in crate::space) fn read_at(dir: &Path, id: &str) -> Result<ExtensionEntry> {
    let dir = dir.to_path_buf();
    let root = manifest_root(&dir);
    // As Chrome reads it, so what a profile loads is always listed.
    let manifest = manifest_edit::read_manifest(&root)?;
    let msgs = locale_messages(&root, &manifest);
    let sv = |key: &str| -> String {
        resolve_msg(
            manifest.get(key).and_then(|v| v.as_str()).unwrap_or(""),
            &msgs,
        )
    };
    let added = dir.join(source::ADDED);
    let added_at = fs::read_to_string(&added).ok().map(|s| s.trim().to_string()).unwrap_or_default();
    // The extension's own files: the same size before and after `.added` moves out.
    let marker = fs::metadata(&added).ok().filter(|m| m.is_file()).map_or(0, |m| m.len());
    Ok(ExtensionEntry {
        id: id.to_string(),
        name: {
            let n = sv("name");
            if n.is_empty() { id.to_string() } else { n }
        },
        version: sv("version"),
        description: sv("description"),
        icon: best_icon(&root, &manifest).unwrap_or_default(),
        path: root.display().to_string(),
        size_bytes: dir_size(&dir).saturating_sub(marker),
        added_at,
        source: None,
    })
}

/// `__MSG_key__` placeholders, resolved against the default locale's
/// messages.json — Chrome Web Store extensions name themselves that way, and
/// without this the grid would read "__MSG_appName__".
fn locale_messages(root: &Path, manifest: &serde_json::Value) -> serde_json::Value {
    let Some(loc) = manifest.get("default_locale").and_then(|v| v.as_str()) else {
        return serde_json::Value::Null;
    };
    manifest_edit::read_json(&root.join("_locales").join(loc).join("messages.json")).unwrap_or(serde_json::Value::Null)
}

fn resolve_msg(raw: &str, msgs: &serde_json::Value) -> String {
    let Some(key) = raw.strip_prefix("__MSG_").and_then(|r| r.strip_suffix("__")) else {
        return raw.to_string();
    };
    msgs.get(key)
        .and_then(|v| v.get("message"))
        .and_then(|v| v.as_str())
        .unwrap_or(raw)
        .to_string()
}

/// Largest declared icon as a data: URL. MV2 puts them under `icons`, MV3 the
/// same, and some extensions only carry an action icon — try all three.
fn best_icon(root: &Path, manifest: &serde_json::Value) -> Option<String> {
    fn largest(map: &serde_json::Value, best: &mut Option<(u64, String)>) {
        let Some(obj) = map.as_object() else { return };
        for (k, v) in obj {
            let Some(rel) = v.as_str() else { continue };
            let size: u64 = k.parse().unwrap_or(0);
            if best.as_ref().map(|(s, _)| size > *s).unwrap_or(true) {
                *best = Some((size, rel.to_string()));
            }
        }
    }

    let mut best: Option<(u64, String)> = None;
    if let Some(v) = manifest.get("icons") {
        largest(v, &mut best);
    }
    if best.is_none() {
        for key in ["action", "browser_action", "page_action"] {
            let Some(v) = manifest.get(key).and_then(|a| a.get("default_icon")) else { continue };
            match v.as_str() {
                Some(s) => best = Some((0, s.to_string())),
                None => largest(v, &mut best),
            }
        }
    }
    let (_, rel) = best?;
    let path = root.join(rel.trim_start_matches('/'));
    let bytes = fs::read(&path).ok()?;
    let mime = match path.extension().and_then(|e| e.to_str()).unwrap_or("") {
        "svg" => "image/svg+xml",
        "jpg" | "jpeg" => "image/jpeg",
        "webp" => "image/webp",
        _ => "image/png",
    };
    Some(format!(
        "data:{mime};base64,{}",
        base64::engine::general_purpose::STANDARD.encode(bytes)
    ))
}

fn dir_size(dir: &Path) -> u64 {
    let mut total = 0;
    let Ok(rd) = fs::read_dir(dir) else { return 0 };
    for e in rd.flatten() {
        match e.file_type() {
            Ok(t) if t.is_dir() => total += dir_size(&e.path()),
            Ok(_) => total += e.metadata().map(|m| m.len()).unwrap_or(0),
            Err(_) => {}
        }
    }
    total
}

/// What an install expects of a package; made on the thread that asks, so a
/// test's stand-in store key is the one checked.
#[derive(Debug, Clone)]
pub(in crate::space) struct Want {
    /// The Web Store id the link named: its package must be the store's, for that id.
    pub link: Option<String>,
    publisher: [u8; 32],
}

impl Want {
    /// A file picked here, or one from a direct link.
    pub fn file() -> Want {
        Want { link: None, publisher: crx::publisher() }
    }

    /// What a pasted link names, read as strictly as the front reads it (SEC-08).
    pub fn of_link(raw: &str) -> Want {
        Want { link: store_link::store_id(raw), ..Want::file() }
    }
}

/// Code of the refusal to add what the library already holds.
pub(in crate::space) const ALREADY_IN_LIBRARY: &str = "already_in_library";

/// `already_in_library {"entry_id":…,"installed":…,"separate":…}`: `separate` says
/// whether a copy beside it would get an id of its own.
pub(in crate::space) fn already_in_library(entry_id: &str, installed: &str, separate: bool) -> CmdError {
    let detail = serde_json::json!({ "entry_id": entry_id, "installed": installed, "separate": separate });
    CmdError::Refused { status: 409, code: ALREADY_IN_LIBRARY.into(), message: format!("{ALREADY_IN_LIBRARY} {detail}") }
}

/// What several files came to: each one refused, with its file and why.
#[derive(Debug, Default, Serialize)]
pub struct Imported {
    pub added: Vec<ExtensionEntry>,
    pub refused: Vec<ImportRefused>,
}

#[derive(Debug, Serialize, PartialEq)]
pub struct ImportRefused {
    pub path: String,
    pub error: String,
}

/// Each file in turn: one refused does not stop the rest.
pub async fn import_each<F, Fut>(paths: &[String], mut one: F) -> Imported
where
    F: FnMut(PathBuf) -> Fut,
    Fut: std::future::Future<Output = Result<ExtensionEntry>>,
{
    let mut out = Imported::default();
    for p in paths {
        match one(PathBuf::from(p)).await {
            Ok(e) => out.added.push(e),
            Err(e) => out.refused.push(ImportRefused { path: p.clone(), error: format!("{e:#}") }),
        }
    }
    out
}

fn stamp() -> String {
    let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    format!("@{secs}")
}

/// Import a .crx, a .zip or an already-unpacked folder. Returns the new entry.
#[cfg(test)]
pub(in crate::space) fn import(src: &Path) -> Result<ExtensionEntry> {
    import_as(src, false)
}

/// `separate`: added beside the one the library holds, when its id would differ.
pub(in crate::space) fn import_as(src: &Path, separate: bool) -> Result<ExtensionEntry> {
    let lib = store::extensions_dir()?;
    let id = uuid::Uuid::new_v4().simple().to_string();
    install(&lib, &id, src, &Want::file(), separate)
}

/// Unpacked in a stage beside the library, then moved in whole with its side file.
fn install(lib: &Path, id: &str, src: &Path, want: &Want, separate: bool) -> Result<ExtensionEntry> {
    let stage = lib.join(format!(".{id}.staging"));
    let tree = stage.join("tree");
    let _ = fs::remove_dir_all(&stage);
    let placed = (|| -> Result<()> {
        let origin = unpack_into(src, &tree, want)?;
        // Never a folder the library could not show.
        read_at(&tree, id).context("its manifest.json cannot be read")?;
        refuse_duplicate(lib, origin.source.as_ref(), key_id(&manifest_root(&tree)).as_deref(), separate)?;
        source::write(lib, id, &SourceLocal::installed(origin, stamp()))?;
        fs::rename(&tree, lib.join(id)).context("move the extension into the library")
    })();
    let _ = fs::remove_dir_all(&stage);
    if placed.is_err() {
        let _ = fs::remove_file(source::side_path(lib, id));
    }
    placed?;
    entry_in(lib, id)
}

/// The id the manifest's `key` gives, when it has a readable one.
fn key_id(root: &Path) -> Option<String> {
    match manifest_edit::id_basis(root).ok()? {
        manifest_edit::IdBasis::Key(der) => Some(chrome_id::id_from_key(&der)),
        manifest_edit::IdBasis::Path => None,
    }
}

/// Refused when the library holds the same store item or the same id from a key;
/// the latter even as a separate copy, since Chrome would load only one.
fn refuse_duplicate(lib: &Path, src: Option<&SourceRef>, key: Option<&str>, separate: bool) -> Result<()> {
    if src.is_none() && key.is_none() {
        return Ok(());
    }
    let mut ids: Vec<String> = fs::read_dir(lib)?
        .flatten()
        .filter(|e| e.path().is_dir())
        .map(|e| e.file_name().to_string_lossy().to_string())
        .filter(|n| valid_id(n))
        .collect();
    ids.sort();
    let mut hit: Option<(String, PathBuf, bool)> = None;
    for id in ids {
        let root = manifest_root(&lib.join(&id));
        if !root.join("manifest.json").is_file() {
            continue;
        }
        let same_id = key.is_some() && key_id(&root).as_deref() == key;
        // A folder the launch pass has not seen yet shows its source from its files.
        let theirs = match source::read(lib, &id) {
            Some(s) if s.recovered => s.source,
            _ => source::recover(&root).source,
        };
        let same_item = src.is_some() && theirs.as_ref() == src;
        if (same_id || same_item) && hit.as_ref().is_none_or(|h| !h.2 && same_id) {
            hit = Some((id, root, same_id));
        }
    }
    let Some((entry_id, root, same_id)) = hit else { return Ok(()) };
    if separate && !same_id {
        return Ok(());
    }
    let installed = manifest_edit::read_manifest(&root)
        .ok()
        .and_then(|m| m.get("version").and_then(|v| v.as_str()).map(str::to_string))
        .unwrap_or_default();
    Err(already_in_library(&entry_id, &installed, !same_id).into())
}

/// A .crx, a .zip or a folder unpacked into `dst`, which must then hold an
/// extension; on failure nothing is left there.
pub(in crate::space) fn unpack_into(src: &Path, dst: &Path, want: &Want) -> Result<Origin> {
    fs::create_dir_all(dst)?;
    let res = if src.is_dir() { copy_dir(src, dst).map(|_| None) } else { unpack_file(src, dst, want) };
    let origin = match res {
        Ok(o) => o,
        Err(e) => {
            let _ = fs::remove_dir_all(dst);
            return Err(e);
        }
    };
    let root = manifest_root(dst);
    if !root.join("manifest.json").exists() {
        let _ = fs::remove_dir_all(dst);
        anyhow::bail!("no manifest.json inside — not an extension");
    }
    // A package without a proven header: its source from its files, as at launch.
    Ok(origin.unwrap_or_else(|| source::recover(&root)))
}

/// A .crx or .zip into `dst`; what a verified CRX3 header proved.
fn unpack_file(src: &Path, dst: &Path, want: &Want) -> Result<Option<Origin>> {
    let bytes = fs::read(src).with_context(|| format!("read {}", src.display()))?;
    if bytes.len() >= 8 && bytes[..4] == *b"Cr24" && bytes[4..8] == 3u32.to_le_bytes() {
        let expect = crx::Expect { id: want.link.as_deref(), publisher: want.link.as_ref().map(|_| want.publisher), ..Default::default() };
        match crx::verify(&bytes, &expect) {
            Ok(v) => {
                unzip(&bytes[v.archive_offset..], dst)?;
                let store = want.link.is_some() || v.proof_keys.contains(&want.publisher);
                // Only the store's, and only its signer's: a key in the library then always means a store id.
                if store {
                    manifest_edit::set_key(&manifest_root(dst), &v.developer_key)
                        .context("the Web Store's package names a key other than its signer's, and it could not be put right")?;
                }
                let item = v.verified_contents.as_deref().and_then(crx::verified_item).filter(|i| i.id == v.crx_id);
                return Ok(Some(Origin {
                    source: store.then(|| SourceRef::webstore(&v.crx_id)),
                    how: store.then_some(if want.link.is_some() { How::Link } else { How::CrxHeader }),
                    store_item: item.filter(|_| store),
                    crx_id: Some(v.crx_id),
                }));
            }
            Err(r) if want.link.is_some() => bail!("the Web Store's package failed its check: {r}"),
            // A file picked here unpacks as before, but proves nothing.
            Err(r) => eprintln!("[extensions] {}: {r}; unpacked as a plain archive", src.display()),
        }
    } else if want.link.is_some() {
        bail!("the Web Store did not send a CRX3 package");
    }
    unzip(&bytes[crx_payload_offset(&bytes)?..], dst)?;
    Ok(None)
}

/// Import from a Web Store page, a bare extension id, or a direct .crx / .zip
/// URL. A store page is not the file, so the id is pulled out and the download
/// built from it. `separate` as for `import_as`; a store item already here is
/// refused before the download.
pub(in crate::space) async fn import_url_as(raw: &str, separate: bool) -> Result<ExtensionEntry> {
    let lib = store::extensions_dir()?;
    let want = Want::of_link(raw);
    if let Some(store_id) = &want.link {
        // Its package gets the store's key, so the store's id.
        refuse_duplicate(&lib, Some(&SourceRef::webstore(store_id)), Some(store_id), separate)?;
    }
    let id = uuid::Uuid::new_v4().simple().to_string();
    let part = lib.join(format!(".{id}.part"));
    let got = match download(raw, &part).await {
        Ok(()) => install(&lib, &id, &part, &want, separate),
        Err(e) => Err(e),
    };
    let _ = fs::remove_file(&part);
    got
}

/// The most an extension file may be when downloaded.
const DOWNLOAD_MAX: u64 = 256 << 20;
/// A download that sends nothing for this long is given up.
const STALL: std::time::Duration = std::time::Duration::from_secs(30);

/// The extension file a link names, streamed into `to`.
pub(in crate::space) async fn download(raw: &str, to: &Path) -> Result<()> {
    let url = resolve_download_url(raw)?;
    let ua = format!("Mozilla/5.0 Chrome/{}", crate::runtime::CHROMIUM_VERSION);
    fetch(&url, to, &ua, DOWNLOAD_MAX, &|_| {}).await
}

/// What every Web Store call says while no proxy is chosen for it (#154).
pub(crate) const NO_STORE_PROXY: &str =
    "choose a proxy for the Web Store in Settings: the launcher does not reach the store from this computer's IP";

/// Whether a proxy for the Web Store is chosen in Settings.
pub(crate) fn store_proxy_chosen() -> bool {
    crate::settings::load().ok().and_then(|s| s.store_proxy).is_some_and(|id| !id.trim().is_empty())
}

/// A client for `url` through the proxy chosen for the Web Store (#154), never
/// straight from this computer. A loopback address (tests, a local mirror) goes direct.
fn store_client(url: &str, ua: &str) -> std::result::Result<reqwest::ClientBuilder, String> {
    let builder = reqwest::Client::builder().user_agent(ua);
    let loopback = url::Url::parse(url).ok().and_then(|u| u.host_str().map(str::to_string)).is_some_and(|h| {
        h == "localhost" || h.trim_matches(['[', ']']).parse::<std::net::IpAddr>().is_ok_and(|ip| ip.is_loopback())
    });
    if loopback {
        return Ok(builder.no_proxy());
    }
    let id = crate::settings::load().ok().and_then(|s| s.store_proxy).filter(|id| !id.trim().is_empty());
    let id = id.ok_or_else(|| NO_STORE_PROXY.to_string())?;
    // The space's own list: a team's proxy when one was chosen in a team.
    let entry = crate::space::proxy::get(&id)
        .map_err(|e| e.to_string())?
        .filter(|p| !p.host.is_empty())
        .ok_or_else(|| "the proxy chosen for the Web Store in Settings was deleted; choose another".to_string())?;
    let proxy = super::proxy::reqwest_proxy(&entry).map_err(|e| e.to_string())?;
    Ok(builder.proxy(proxy))
}

/// `url` streamed into `to`: at most `cap` bytes, given up after `STALL` without
/// any; `progress` hears the bytes received so far.
pub(in crate::space) async fn fetch(
    url: &str,
    to: &Path,
    ua: &str,
    cap: u64,
    progress: &(dyn Fn(u64) + Send + Sync),
) -> Result<()> {
    use tokio::io::AsyncWriteExt;
    let client = store_client(url, ua)
        .map_err(|e| anyhow::anyhow!(e))?
        .connect_timeout(std::time::Duration::from_secs(10))
        .build()?;
    let resp = tokio::time::timeout(STALL, client.get(url).send())
        .await
        .map_err(|_| anyhow::anyhow!("fetch {url}: no answer"))?
        .with_context(|| format!("fetch {url}"))?;
    let status = resp.status();
    // The store answers 204 for an id it does not know rather than 404.
    if status == reqwest::StatusCode::NO_CONTENT {
        anyhow::bail!("the Web Store has no extension with that id");
    }
    let mut resp = resp.error_for_status().with_context(|| format!("fetch {url}"))?;
    let too_big = || anyhow::anyhow!("the file is over {} MB", cap >> 20);
    if resp.content_length().is_some_and(|n| n > cap) {
        return Err(too_big());
    }
    let mut file = tokio::fs::File::create(to).await.with_context(|| format!("create {}", to.display()))?;
    let (mut got, mut first) = (0u64, None);
    loop {
        let chunk = tokio::time::timeout(STALL, resp.chunk())
            .await
            .map_err(|_| anyhow::anyhow!("the download stalled"))?
            .with_context(|| format!("fetch {url}"))?;
        let Some(chunk) = chunk else { break };
        got += chunk.len() as u64;
        if got > cap {
            return Err(too_big());
        }
        first = first.or(chunk.first().copied());
        file.write_all(&chunk).await?;
        progress(got);
    }
    file.flush().await?;
    if got < 4 {
        anyhow::bail!("the link returned nothing to unpack");
    }
    // Say so before unzip does: a page that 200s with HTML is the usual mistake.
    if first == Some(b'<') {
        anyhow::bail!("that link is a web page, not an extension file");
    }
    Ok(())
}

/// The most an answer of the update check may be.
const SMALL_MAX: usize = 1 << 20;

/// A small GET to the Web Store (the update check): its body, within `timeout`
/// start to end and `SMALL_MAX` bytes. Through the store's proxy, no cookies.
pub(in crate::space) async fn get_small(url: &str, ua: &str, timeout: std::time::Duration) -> std::result::Result<Vec<u8>, String> {
    let said = |e: reqwest::Error| {
        if e.is_timeout() {
            "the Web Store did not answer in time".to_string()
        } else {
            format!("could not reach the Web Store: {e}")
        }
    };
    let client = store_client(url, ua)?
        .connect_timeout(std::time::Duration::from_secs(10).min(timeout))
        .timeout(timeout)
        .build()
        .map_err(|e| e.to_string())?;
    let mut resp = client.get(url).send().await.map_err(said)?;
    if !resp.status().is_success() {
        return Err(format!("the Web Store answered {}", resp.status()));
    }
    let too_big = || "the Web Store's answer is too big to be one".to_string();
    if resp.content_length().is_some_and(|n| n > SMALL_MAX as u64) {
        return Err(too_big());
    }
    let mut body = Vec::new();
    while let Some(chunk) = resp.chunk().await.map_err(said)? {
        body.extend_from_slice(&chunk);
        if body.len() > SMALL_MAX {
            return Err(too_big());
        }
    }
    Ok(body)
}

/// Where the store's packages come from; a test may serve its own.
fn store_base() -> String {
    #[cfg(test)]
    if let Some(b) = TEST_STORE.with(|c| c.borrow().clone()) {
        return b;
    }
    update2::WEBSTORE_UPDATE.to_string()
}

#[cfg(test)]
thread_local! {
    pub(in crate::space) static TEST_STORE: std::cell::RefCell<Option<String>> = const { std::cell::RefCell::new(None) };
}

/// Web Store address or bare id → the update endpoint that serves the .crx.
/// Anything else is taken as a direct link to the file.
fn resolve_download_url(raw: &str) -> Result<String> {
    let raw = raw.trim();
    if raw.is_empty() {
        anyhow::bail!("no link given");
    }
    if let Some(id) = store_link::store_id(raw) {
        return Ok(format!(
            "{}?response=redirect&acceptformat=crx3&prodversion={}&x=id%3D{}%26uc",
            store_base(),
            crate::runtime::CHROMIUM_VERSION,
            id,
        ));
    }
    if !raw.starts_with("http://") && !raw.starts_with("https://") {
        anyhow::bail!("that is neither a link nor a Web Store id");
    }
    Ok(raw.to_string())
}

/// At launch, in the background: swaps a dead launcher left half done
/// settled, each folder's source recorded (Chrome deletes `_metadata/` on
/// first load), approved updates applied. Launches and sync wait for it.
pub(in crate::space) fn at_launch() {
    let lib = match store::extensions_dir() {
        Ok(d) => d,
        Err(e) => {
            eprintln!("[extensions] not checked at launch: {e:#}");
            return;
        }
    };
    let settling = usage::settling(&lib);
    let job = {
        let lib = lib.clone();
        move || {
            let _done = settling;
            settle_library(&lib);
        }
    };
    if let Err(e) = std::thread::Builder::new().name("extensions-at-launch".into()).spawn(job) {
        eprintln!("[extensions] settled in the foreground: {e}");
        settle_library(&lib);
    }
}

fn settle_library(lib: &Path) {
    let ops = swap::Ops::real();
    let s = swap::sweep(lib, &ops);
    if s != swap::Swept::default() {
        eprintln!(
            "[extensions] swept: {} put back, {} finished, {} staged kept, {} removed",
            s.restored, s.finished, s.kept, s.removed
        );
    }
    let p = source::pass(lib);
    if p != source::Pass::default() {
        eprintln!("[extensions] sources: {} recorded, {} .added moved, {} failed", p.written, p.moved, p.failed);
    }
    usage::report(&usage::retry(&usage::System, lib, &ops), "at launch");
}

/// The folder, its side file and whatever an install or update left beside it.
pub(in crate::space) fn delete(id: &str) -> Result<()> {
    let dir = dir_for(id)?;
    // Never in the middle of a swap of the same folder.
    let _alone = usage::exclusive(id, usage::SWAP_WAIT).context("the extension is being updated; try again in a minute")?;
    if dir.exists() {
        fs::remove_dir_all(&dir)?;
    }
    let Some(lib) = dir.parent() else { return Ok(()) };
    let _ = fs::remove_file(source::side_path(lib, id));
    if let Ok(root) = store::data_root() {
        updates::forget(&root.join(updates::BOOK), id);
    }
    let prefix = format!(".{id}.");
    for e in fs::read_dir(lib)?.flatten() {
        if !e.file_name().to_string_lossy().starts_with(&prefix) {
            continue;
        }
        let _ = if e.path().is_dir() { fs::remove_dir_all(e.path()) } else { fs::remove_file(e.path()) };
    }
    Ok(())
}

/// The most an extension may unpack to, whatever its archive claims.
const UNPACK_MAX: u64 = 1024 * 1024 * 1024;
/// The most files it may hold.
const UNPACK_FILES: usize = 100_000;

/// A .crx is a header followed by a plain zip; strip the header and unzip.
pub(in crate::space) fn unpack_archive(src: &Path, dst: &Path) -> Result<()> {
    let bytes = fs::read(src).with_context(|| format!("read {}", src.display()))?;
    unzip(&bytes[crx_payload_offset(&bytes)?..], dst)
}

fn unzip(archive: &[u8], dst: &Path) -> Result<()> {
    unzip_within(archive, dst, UNPACK_MAX, UNPACK_FILES)
}

/// Each file streamed to disk; held to `max` bytes and `most` files, and to
/// the room here for what the archive says it holds.
fn unzip_within(archive: &[u8], dst: &Path, max: u64, most: usize) -> Result<()> {
    let mut zip = zip::ZipArchive::new(std::io::Cursor::new(archive)).context("not a zip/crx archive")?;
    if zip.len() > most {
        bail!("the archive holds more than {most} files");
    }
    let too_big = || anyhow::anyhow!("the extension unpacks to more than {} MB", max >> 20);
    let said = (0..zip.len()).filter_map(|i| zip.by_index_raw(i).ok().map(|f| f.size())).fold(0u64, u64::saturating_add);
    if said > max {
        return Err(too_big());
    }
    if let Some(free) = crate::runtime::free_bytes(dst).filter(|free| said > *free) {
        bail!("not enough free disk space to unpack it: it needs {} MB, {} MB is free", said.div_ceil(1 << 20), free >> 20);
    }
    let mut total = 0u64;
    for i in 0..zip.len() {
        let f = zip.by_index(i)?;
        let Some(rel) = f.enclosed_name() else { continue };
        let out = dst.join(rel);
        if f.is_dir() {
            fs::create_dir_all(&out)?;
            continue;
        }
        if let Some(parent) = out.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut file = fs::File::create(&out).with_context(|| format!("create {}", out.display()))?;
        // What it really holds, whatever its header said.
        total += std::io::copy(&mut f.take(max - total + 1), &mut file)?;
        if total > max {
            return Err(too_big());
        }
    }
    Ok(())
}

/// The launcher's own, never part of a package: Chrome's `_metadata/` and `.added`.
fn not_packed(rel: &str, name: &std::ffi::OsStr) -> bool {
    rel.is_empty() && (name == "_metadata" || name == source::ADDED)
}

/// The folder's files as a plain zip at `out`, for a team's copy; its size.
pub(in crate::space) fn zip_dir(dir: &Path, out: &Path) -> Result<u64> {
    use std::io::Write as _;
    let file = fs::File::create(out).with_context(|| format!("create {}", out.display()))?;
    let mut zip = zip::ZipWriter::new(std::io::BufWriter::new(file));
    let opts = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
    let mut stack = vec![(dir.to_path_buf(), String::new())];
    while let Some((at, rel)) = stack.pop() {
        let mut entries: Vec<_> = fs::read_dir(&at)?.flatten().collect();
        entries.sort_by_key(|e| e.file_name());
        for e in entries {
            let name = e.file_name();
            if store::is_os_litter(&name) || not_packed(&rel, &name) {
                continue;
            }
            let child = format!("{rel}{}", name.to_string_lossy());
            let t = e.file_type()?;
            if t.is_dir() {
                stack.push((e.path(), format!("{child}/")));
            } else if t.is_file() {
                zip.start_file(child, opts)?;
                std::io::copy(&mut fs::File::open(e.path())?, &mut zip)?;
            }
        }
    }
    let mut w = zip.finish()?;
    w.flush()?;
    w.into_inner().map_err(|e| e.into_error())?.sync_all()?;
    Ok(fs::metadata(out)?.len())
}

/// What sync compares to tell that the files changed: every packed file's path
/// and size, and the version. Chrome's cleanup of `_metadata/` does not count.
pub(in crate::space) fn files_digest(dir: &Path) -> Result<String> {
    use sha2::{Digest, Sha256};
    let root = manifest_root(dir);
    let mut files = Vec::new();
    let mut stack = vec![(root.clone(), String::new())];
    while let Some((at, rel)) = stack.pop() {
        for e in fs::read_dir(&at)?.flatten() {
            let name = e.file_name();
            if store::is_os_litter(&name) || not_packed(&rel, &name) {
                continue;
            }
            let child = format!("{rel}{}", name.to_string_lossy());
            let t = e.file_type()?;
            if t.is_dir() {
                stack.push((e.path(), format!("{child}/")));
            } else if t.is_file() {
                files.push((child, e.metadata()?.len()));
            }
        }
    }
    files.sort();
    let version = manifest_edit::read_manifest(&root)?.get("version").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let mut h = Sha256::new();
    for (path, size) in &files {
        h.update(format!("{path}\0{size}\n"));
    }
    h.update(format!("version\0{version}"));
    Ok(h.finalize().iter().map(|b| format!("{b:02x}")).collect())
}

/// Byte offset of the zip inside a .crx (0 for a plain .zip).
fn crx_payload_offset(bytes: &[u8]) -> Result<usize> {
    if bytes.len() < 16 || &bytes[0..4] != b"Cr24" {
        return Ok(0);
    }
    let u32_at = |i: usize| -> u32 {
        u32::from_le_bytes([bytes[i], bytes[i + 1], bytes[i + 2], bytes[i + 3]])
    };
    let offset = match u32_at(4) {
        2 => 16 + u32_at(8) as usize + u32_at(12) as usize,
        3 => 12 + u32_at(8) as usize,
        v => anyhow::bail!("unsupported CRX version {v}"),
    };
    if offset >= bytes.len() {
        anyhow::bail!("CRX header runs past the end of the file");
    }
    Ok(offset)
}

fn copy_dir(src: &Path, dst: &Path) -> Result<()> {
    fs::create_dir_all(dst)?;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        if store::is_os_litter(&entry.file_name()) {
            continue;
        }
        let to = dst.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir(&entry.path(), &to)?;
        } else {
            fs::copy(entry.path(), &to)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
