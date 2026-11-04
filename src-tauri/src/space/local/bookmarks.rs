//! Folder-scoped bookmarks, written into the profile's `Bookmarks` just before
//! launch, each where Chrome had it. Every node written carries our marker, so a
//! later start changes only those and the operator's own stay as they are.

use crate::store;
use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::fs;
use std::path::Path;

/// On every node this launcher writes: Chrome keeps a node's `meta_info` through
/// its own saves (bookmark_codec.cc), so the next start knows what is ours.
const OWNED_KEY: &str = "shardx_owned";
/// On a bookmark node: the launcher bookmark it shows, so a start reuses its node.
const ID_KEY: &str = "shardx_id";
/// Earlier builds put every bookmark in one "ShardBrowser" folder with this marker.
const OLD_FOLDER_KEY: &str = "shardx_managed";
/// That folder's name before the rename to ShardBrowser, written without the marker.
const LEGACY_FOLDER: &str = "ShardX";
/// Chrome's roots in its order, with the names it writes when it makes a file.
const ROOTS: [(&str, &str); 3] = [("bookmark_bar", "Bookmarks bar"), ("other", "Other bookmarks"), ("synced", "Mobile bookmarks")];

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Bookmark {
    #[serde(default)]
    pub id: String,
    pub title: String,
    pub url: String,
    /// Launcher folder this belongs to; empty means every profile.
    #[serde(default)]
    pub folder: String,
    /// Chrome's root it goes in: "other", "synced", or empty for the bookmarks bar.
    /// Left out when empty, as `path` is, so older items hash the same.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub root: String,
    /// Bookmark folders it sits in inside that root, outermost first.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub path: Vec<String>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct BookmarkStore {
    #[serde(default)]
    bookmarks: Vec<Bookmark>,
}

fn load_store() -> Result<BookmarkStore> {
    let path = store::bookmarks_path()?;
    if !path.exists() {
        return Ok(BookmarkStore::default());
    }
    Ok(serde_json::from_str(&fs::read_to_string(path)?).unwrap_or_default())
}

fn save_store(s: &BookmarkStore) -> Result<()> {
    fs::write(store::bookmarks_path()?, serde_json::to_string_pretty(s)?)?;
    Ok(())
}

pub(in crate::space) fn list() -> Result<Vec<Bookmark>> {
    Ok(load_store()?.bookmarks)
}

/// Insert or update by id; a blank id mints one.
pub(in crate::space) fn save(b: Bookmark) -> Result<Bookmark> {
    let mut b = cleaned(b)?;
    let mut s = load_store()?;
    if b.id.is_empty() {
        b.id = uuid::Uuid::new_v4().to_string();
        s.bookmarks.push(b.clone());
    } else if let Some(slot) = s.bookmarks.iter_mut().find(|x| x.id == b.id) {
        *slot = b.clone();
    } else {
        s.bookmarks.push(b.clone());
    }
    save_store(&s)?;
    Ok(b)
}

/// Trimmed, the address made whole, the title the address when blank.
pub(in crate::space) fn cleaned(mut b: Bookmark) -> Result<Bookmark> {
    b.title = b.title.trim().to_string();
    b.url = normalise_url(&b.url);
    b.folder = b.folder.trim().to_string();
    b.root = match b.root.trim().to_ascii_lowercase().as_str() {
        "" | "bookmark_bar" => String::new(),
        "other" => "other".into(),
        "synced" | "mobile" => "synced".into(),
        other => anyhow::bail!("a bookmark's root is bookmark_bar, other or synced, not {other:?}"),
    };
    b.path = b.path.iter().map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect();
    if b.url.is_empty() {
        anyhow::bail!("a bookmark needs a URL");
    }
    if is_bookmarklet(&b.url) && b.url["javascript:".len()..].trim().is_empty() {
        anyhow::bail!("a bookmarklet needs its script after javascript:");
    }
    if b.title.is_empty() {
        b.title = b.url.clone();
    }
    Ok(b)
}

pub(in crate::space) fn delete(id: &str) -> Result<()> {
    let mut s = load_store()?;
    s.bookmarks.retain(|b| b.id != id);
    save_store(&s)
}

/// Several at once: one read and one write of the store, however many.
pub(in crate::space) fn delete_many(ids: &[String]) -> Result<()> {
    let gone: std::collections::HashSet<&str> = ids.iter().map(String::as_str).collect();
    let mut s = load_store()?;
    s.bookmarks.retain(|b| !gone.contains(b.id.as_str()));
    save_store(&s)
}

fn normalise_url(raw: &str) -> String {
    let t = raw.trim();
    // A bookmarklet is kept as written, as Chrome keeps it.
    if t.is_empty() || t.contains("://") || is_bookmarklet(t) {
        return t.to_string();
    }
    format!("https://{t}")
}

/// `javascript:…`, in any case: a bookmark that runs its script on the open page.
fn is_bookmarklet(url: &str) -> bool {
    url.get(..11).is_some_and(|s| s.eq_ignore_ascii_case("javascript:"))
}

/// Chromium timestamps are microseconds since 1601-01-01.
fn chrome_now() -> String {
    let unix = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    ((unix + 11_644_473_600) * 1_000_000).to_string()
}

fn empty_tree() -> Value {
    let ts = chrome_now();
    let root = |id: &str, name: &str| {
        json!({
            "children": [], "date_added": ts, "date_modified": ts,
            "guid": uuid::Uuid::new_v4().to_string(),
            "id": id, "name": name, "type": "folder"
        })
    };
    json!({
        "checksum": "",
        "roots": {
            "bookmark_bar": root("1", "Bookmarks bar"),
            "other": root("2", "Other bookmarks"),
            "synced": root("3", "Mobile bookmarks"),
        },
        "version": 1
    })
}

fn max_id(node: &Value, cur: &mut u64) {
    if let Some(id) = node.get("id").and_then(|v| v.as_str()).and_then(|s| s.parse::<u64>().ok()) {
        *cur = (*cur).max(id);
    }
    if let Some(children) = node.get("children").and_then(|v| v.as_array()) {
        for c in children {
            max_id(c, cur);
        }
    }
    // The file's top holds `roots`, not `children`: without this every id began at 1.
    if let Some(roots) = node.get("roots").and_then(|v| v.as_object()) {
        for r in roots.values() {
            max_id(r, cur);
        }
    }
}

/// Rewrite `<udd>/Default/Bookmarks` so it carries exactly the bookmarks of
/// `all` this launcher folder defines, each in its root and folders, leaving
/// every node the operator added where it is.
pub(in crate::space) fn apply_to_profile(udd: &Path, folder: &str, all: &[Bookmark]) -> Result<usize> {
    let wanted: Vec<&Bookmark> = all.iter().filter(|b| b.folder.is_empty() || b.folder == folder).collect();

    let default_dir = udd.join("Default");
    let path = default_dir.join("Bookmarks");
    let existing = fs::read_to_string(&path)
        .ok()
        .and_then(|s| serde_json::from_str::<Value>(&s).ok());

    // Nothing to add and nothing of ours to clean out — leave the file alone
    // rather than rewriting a profile that never had managed bookmarks.
    if wanted.is_empty() && existing.is_none() {
        return Ok(0);
    }
    let mut tree = existing.unwrap_or_else(empty_tree);
    if !tree.get("roots").is_some_and(Value::is_object) {
        return Ok(0);
    }

    let mut next_id = 0u64;
    max_id(&tree, &mut next_id);
    let mut reuse = HashMap::new();
    collect_ours(&tree["roots"], &mut reuse);
    let mut cx = Layout { reuse, next_id, ts: chrome_now(), had_ours: false };

    for (key, name) in ROOTS {
        let root = &mut tree["roots"][key];
        if !root.is_object() {
            cx.next_id += 1;
            *root = json!({ "children": [], "date_added": cx.ts, "date_modified": cx.ts, "id": cx.next_id.to_string(), "name": name, "type": "folder" });
        }
        let kids = match root.get_mut("children").map(Value::take) {
            Some(Value::Array(kids)) => kids,
            _ => Vec::new(),
        };
        let want = wanted_tree(wanted.iter().copied().filter(|b| root_of(b) == key));
        root["children"] = Value::Array(cx.merge(kids, &want, key == "bookmark_bar"));
    }
    if wanted.is_empty() && !cx.had_ours {
        return Ok(0);
    }

    // Chrome no longer checks these on load (bookmark_codec.cc) and writes fresh
    // ones on its next save; blank, they claim nothing about the new tree.
    tree["checksum"] = json!("");
    if let Some(m) = tree.as_object_mut() {
        m.remove("checksum_sha256");
    }

    fs::create_dir_all(&default_dir)?;
    fs::write(&path, serde_json::to_string(&tree)?)?;
    Ok(wanted.len())
}

fn root_of(b: &Bookmark) -> &'static str {
    match b.root.as_str() {
        "other" => "other",
        "synced" => "synced",
        _ => "bookmark_bar",
    }
}

/// What a root should hold of ours: bookmarks in the list's order, each folder
/// where its first bookmark is, so an import keeps Chrome's order.
enum Want<'a> {
    Mark(&'a Bookmark),
    Folder(&'a str, Vec<Want<'a>>),
}

fn wanted_tree<'a>(marks: impl Iterator<Item = &'a Bookmark>) -> Vec<Want<'a>> {
    let mut top = Vec::new();
    for b in marks {
        let mut kids = &mut top;
        for name in &b.path {
            let at = match kids.iter().position(|w| matches!(w, Want::Folder(n, _) if *n == name.as_str())) {
                Some(i) => i,
                None => {
                    kids.push(Want::Folder(name, Vec::new()));
                    kids.len() - 1
                }
            };
            let Want::Folder(_, inner) = &mut kids[at] else { unreachable!("just matched a folder") };
            kids = inner;
        }
        kids.push(Want::Mark(b));
    }
    top
}

fn owned(n: &Value) -> bool {
    n["meta_info"].get(OWNED_KEY).is_some()
}

/// The single folder earlier builds wrote; the bare legacy name counts only where they put it.
fn old_folder(n: &Value, bar_top: bool) -> bool {
    n["type"] == "folder"
        && !owned(n)
        && (n["meta_info"].get(OLD_FOLDER_KEY).is_some() || (bar_top && n["name"] == LEGACY_FOLDER))
}

/// Our bookmark nodes by launcher id, wherever the operator moved them, to keep their ids and dates.
fn collect_ours(n: &Value, out: &mut HashMap<String, Value>) {
    if n["type"] == "url" && owned(n) {
        if let Some(id) = n["meta_info"][ID_KEY].as_str().filter(|id| !id.is_empty()) {
            out.entry(id.to_string()).or_insert_with(|| n.clone());
        }
    }
    match n {
        Value::Object(m) if m.contains_key("children") => {
            n["children"].as_array().into_iter().flatten().for_each(|c| collect_ours(c, out));
        }
        // The file's `roots`: an object of Chrome's root folders.
        Value::Object(m) => m.values().for_each(|c| collect_ours(c, out)),
        _ => {}
    }
}

/// One slot of a folder's children as they were: one of ours, or a node to keep in place.
enum Slot {
    Ours,
    Keep(Value),
    /// A folder of ours, by its place in `Layout::merge`'s list of them.
    Folder(usize),
}

struct Layout {
    reuse: HashMap<String, Value>,
    next_id: u64,
    ts: String,
    had_ours: bool,
}

impl Layout {
    /// A folder's new children: ours as `want` says, in the slots ours had, and the
    /// operator's nodes where they were. The first time, ours go first.
    fn merge(&mut self, existing: Vec<Value>, want: &[Want], bar_top: bool) -> Vec<Value> {
        let mut seq = Vec::new();
        let mut folders: Vec<Option<(String, Value)>> = Vec::new();
        for mut n in existing {
            if old_folder(&n, bar_top) {
                // Its bookmarks come back in their own places; the folder goes.
                self.had_ours = true;
                seq.push(Slot::Ours);
            } else if owned(&n) {
                self.had_ours = true;
                if n["type"] == "folder" {
                    folders.push(Some((n["name"].as_str().unwrap_or_default().to_string(), n)));
                    seq.push(Slot::Folder(folders.len() - 1));
                } else {
                    // Ours: put back from `reuse` where the list wants it.
                    seq.push(Slot::Ours);
                }
            } else {
                // The operator's, kept; a folder of theirs may hold a bookmark of ours they moved.
                if let Some(Value::Array(kids)) = n.get_mut("children").map(Value::take) {
                    n["children"] = Value::Array(self.merge(kids, &[], false));
                }
                seq.push(Slot::Keep(n));
            }
        }

        let mut ours = Vec::with_capacity(want.len());
        for w in want {
            match w {
                Want::Mark(b) => ours.push(self.mark_node(b)),
                Want::Folder(name, inner) => {
                    let found = folders.iter_mut().find(|f| f.as_ref().is_some_and(|(n, _)| n == name));
                    let mut node = match found.and_then(Option::take) {
                        Some((_, node)) => node,
                        None => self.folder_node(name),
                    };
                    let kids = match node.get_mut("children").map(Value::take) {
                        Some(Value::Array(kids)) => kids,
                        _ => Vec::new(),
                    };
                    node["children"] = Value::Array(self.merge(kids, inner, false));
                    ours.push(node);
                }
            }
        }

        // A folder of ours no longer wanted stays only while it holds the operator's own.
        let seq: Vec<Slot> = seq
            .into_iter()
            .map(|s| match s {
                Slot::Folder(i) => match folders[i].take() {
                    None => Slot::Ours,
                    Some((_, mut f)) => {
                        let kids = match f.get_mut("children").map(Value::take) {
                            Some(Value::Array(kids)) => self.merge(kids, &[], false),
                            _ => Vec::new(),
                        };
                        if kids.is_empty() {
                            Slot::Ours
                        } else {
                            f["children"] = Value::Array(kids);
                            Slot::Keep(f)
                        }
                    }
                },
                s => s,
            })
            .collect();

        let slots = seq.iter().filter(|s| matches!(s, Slot::Ours)).count();
        let mut ours = ours.into_iter();
        let mut out: Vec<Value> = if slots == 0 { ours.by_ref().collect() } else { Vec::new() };
        let mut seen = 0;
        for s in seq {
            match s {
                Slot::Keep(n) => out.push(n),
                Slot::Ours => {
                    seen += 1;
                    // The last slot of ours takes the rest, so new ones follow the ones before.
                    if seen < slots {
                        out.extend(ours.next());
                    } else {
                        out.extend(ours.by_ref());
                    }
                }
                Slot::Folder(_) => {}
            }
        }
        out
    }

    fn mark_node(&mut self, b: &Bookmark) -> Value {
        let reused = (!b.id.is_empty()).then(|| self.reuse.remove(&b.id)).flatten();
        let mut n = reused.unwrap_or_else(|| {
            self.next_id += 1;
            json!({ "date_added": self.ts, "date_last_used": "0", "guid": uuid::Uuid::new_v4().to_string(), "id": self.next_id.to_string() })
        });
        n["name"] = json!(b.title);
        n["type"] = json!("url");
        n["url"] = json!(b.url);
        mark_owned(&mut n, Some(&b.id));
        n
    }

    fn folder_node(&mut self, name: &str) -> Value {
        self.next_id += 1;
        let mut n = json!({
            "children": [], "date_added": self.ts, "date_last_used": "0", "date_modified": self.ts,
            "guid": uuid::Uuid::new_v4().to_string(), "id": self.next_id.to_string(), "name": name, "type": "folder",
        });
        mark_owned(&mut n, None);
        n
    }
}

/// Our marker in the node's `meta_info`, beside whatever else Chrome keeps there.
fn mark_owned(n: &mut Value, id: Option<&str>) {
    if !n["meta_info"].is_object() {
        n["meta_info"] = json!({});
    }
    n["meta_info"][OWNED_KEY] = json!("1");
    if let Some(id) = id.filter(|id| !id.is_empty()) {
        n["meta_info"][ID_KEY] = json!(id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mark(title: &str, folder: &str, path: &[&str]) -> Bookmark {
        Bookmark {
            id: String::new(),
            title: title.into(),
            url: format!("https://{}.example/", title.to_lowercase()),
            folder: folder.into(),
            root: String::new(),
            path: path.iter().map(|s| s.to_string()).collect(),
        }
    }

    /// A launcher bookmark as the store has it: its own id, a root and folders.
    fn placed(title: &str, root: &str, path: &[&str]) -> Bookmark {
        Bookmark { id: format!("id-{title}"), root: root.into(), ..mark(title, "", path) }
    }

    /// A node's shape: a bookmark's name, or a folder's name and what it holds.
    fn shape(n: &Value) -> Value {
        match n["children"].as_array() {
            Some(kids) => {
                let name = n["name"].as_str().unwrap_or_default().to_string();
                Value::Object([(name, Value::Array(kids.iter().map(shape).collect()))].into_iter().collect())
            }
            None => n["name"].clone(),
        }
    }

    fn kids(tree: &Value, root: &str) -> Value {
        Value::Array(tree["roots"][root]["children"].as_array().unwrap().iter().map(shape).collect())
    }

    fn udd() -> std::path::PathBuf {
        std::env::temp_dir().join(format!("shardx-bookmarks-{}", uuid::Uuid::new_v4().simple()))
    }

    fn read(udd: &Path) -> Value {
        serde_json::from_str(&fs::read_to_string(udd.join("Default/Bookmarks")).unwrap()).unwrap()
    }

    fn write(udd: &Path, tree: &Value) {
        fs::create_dir_all(udd.join("Default")).unwrap();
        fs::write(udd.join("Default/Bookmarks"), tree.to_string()).unwrap();
    }

    /// The first node named `name` anywhere under `n`.
    fn find<'a>(n: &'a Value, name: &str) -> Option<&'a Value> {
        if n["name"] == name {
            return Some(n);
        }
        let kids = n["children"].as_array().map(|k| k.iter().collect::<Vec<_>>());
        let kids = kids.unwrap_or_else(|| n.as_object().map(|m| m.values().collect()).unwrap_or_default());
        kids.into_iter().find_map(|c| find(c, name))
    }

    fn ids(n: &Value, out: &mut Vec<String>) {
        if let Some(id) = n["id"].as_str() {
            out.push(id.to_string());
        }
        match n["children"].as_array() {
            Some(kids) => kids.iter().for_each(|c| ids(c, out)),
            None => n.as_object().into_iter().flat_map(|m| m.values()).filter(|v| v.is_object()).for_each(|c| ids(c, out)),
        }
    }

    fn operator(name: &str) -> Value {
        json!({ "date_added": "1", "guid": uuid::Uuid::new_v4().to_string(), "id": "0", "name": name, "type": "url", "url": format!("https://{}.example/", name.to_lowercase()) })
    }

    fn operator_folder(name: &str, children: Vec<Value>) -> Value {
        json!({ "children": children, "date_added": "1", "date_modified": "1", "guid": uuid::Uuid::new_v4().to_string(), "id": "0", "name": name, "type": "folder" })
    }

    /// A profile file with the operator's own nodes, ids numbered as Chrome would.
    fn profile(bar: Vec<Value>, other: Vec<Value>) -> Value {
        let mut tree = empty_tree();
        tree["roots"]["bookmark_bar"]["children"] = Value::Array(bar);
        tree["roots"]["other"]["children"] = Value::Array(other);
        tree["checksum"] = json!("stale");
        tree["checksum_sha256"] = json!("stale");
        let mut n = 10;
        fn number(v: &mut Value, n: &mut u64) {
            if v["id"] == "0" {
                *n += 1;
                v["id"] = json!(n.to_string());
            }
            if let Some(kids) = v.get_mut("children").and_then(Value::as_array_mut) {
                kids.iter_mut().for_each(|c| number(c, n));
            }
        }
        for r in ["bookmark_bar", "other"] {
            number(&mut tree["roots"][r], &mut n);
        }
        tree
    }

    #[test]
    fn several_go_in_one_write_and_the_rest_stay() {
        let _root = crate::store::test_root("bookmarks-many");
        let keep = save(mark("Keep", "", &[])).unwrap();
        let a = save(mark("A", "", &[])).unwrap();
        let b = save(mark("B", "Ads", &[])).unwrap();
        delete_many(&[a.id, b.id, "not-there".into()]).unwrap();
        let left: Vec<String> = list().unwrap().into_iter().map(|x| x.id).collect();
        assert_eq!(left, [keep.id]);
    }

    #[test]
    fn a_path_and_a_root_are_cleaned_and_empty_ones_are_not_written() {
        let b = cleaned(Bookmark { path: vec![" Work ".into(), "  ".into(), "Docs".into()], root: " Other ".into(), ..mark("A", "", &[]) }).unwrap();
        assert_eq!((b.root.as_str(), b.path.clone()), ("other", vec!["Work".to_string(), "Docs".to_string()]));
        assert_eq!(cleaned(Bookmark { root: "bookmark_bar".into(), ..mark("A", "", &[]) }).unwrap().root, "");
        assert_eq!(cleaned(Bookmark { root: "mobile".into(), ..mark("A", "", &[]) }).unwrap().root, "synced");
        assert!(cleaned(Bookmark { root: "desktop".into(), ..mark("A", "", &[]) }).is_err());
        // A bookmark on the bar without folders serializes as before, so its sync digest stays.
        let plain = serde_json::to_value(mark("A", "Ads", &[])).unwrap();
        assert_eq!(plain, json!({ "id": "", "title": "A", "url": "https://a.example/", "folder": "Ads" }));
    }

    #[test]
    fn a_bookmarklet_is_kept_as_written_and_a_bare_one_is_refused() {
        let js = cleaned(Bookmark { url: " javascript:alert(document.title) ".into(), title: "".into(), ..mark("A", "", &[]) }).unwrap();
        assert_eq!(js.url, "javascript:alert(document.title)");
        assert_eq!(js.title, js.url);
        assert_eq!(cleaned(Bookmark { url: "JavaScript:void(0)".into(), ..mark("A", "", &[]) }).unwrap().url, "JavaScript:void(0)");
        assert!(cleaned(Bookmark { url: "javascript:  ".into(), ..mark("A", "", &[]) }).is_err());
        // Anything else without a scheme is still a site.
        assert_eq!(normalise_url("javascript.info/intro"), "https://javascript.info/intro");
        assert_eq!(normalise_url("localhost:8080"), "https://localhost:8080");
    }

    #[test]
    fn each_bookmark_goes_in_its_chrome_root_and_folders_in_the_lists_order() {
        let udd = udd();
        let all = [
            placed("A", "", &[]),
            placed("B", "", &["Work"]),
            placed("C", "", &["Work", "Deep"]),
            placed("Run", "", &["Work"]),
            placed("D", "", &[]),
            // The same address in another root is a bookmark of its own.
            Bookmark { title: "B again".into(), ..placed("B2", "other", &[]) },
            placed("E", "other", &["Shop"]),
            placed("Phone", "synced", &[]),
            Bookmark { folder: "Shop".into(), ..placed("Not here", "", &[]) },
        ];
        let all: Vec<Bookmark> = all.into_iter().map(|b| if b.title == "Run" { Bookmark { url: "javascript:alert(1)".into(), ..b } } else { b }).collect();
        assert_eq!(apply_to_profile(&udd, "Ads", &all).unwrap(), 8);
        let tree = read(&udd);
        assert_eq!(kids(&tree, "bookmark_bar"), json!(["A", { "Work": ["B", { "Deep": ["C"] }, "Run"] }, "D"]));
        assert_eq!(kids(&tree, "other"), json!(["B again", { "Shop": ["E"] }]));
        assert_eq!(kids(&tree, "synced"), json!(["Phone"]));
        assert_eq!(find(&tree["roots"], "Run").unwrap()["url"], "javascript:alert(1)");
        // Every node written is marked, a bookmark with its launcher id.
        assert_eq!(find(&tree["roots"], "Work").unwrap()["meta_info"], json!({ OWNED_KEY: "1" }));
        assert_eq!(find(&tree["roots"], "C").unwrap()["meta_info"], json!({ OWNED_KEY: "1", ID_KEY: "id-C" }));
        let mut all_ids = Vec::new();
        ids(&tree["roots"], &mut all_ids);
        let n = all_ids.len();
        all_ids.sort();
        all_ids.dedup();
        assert_eq!(all_ids.len(), n, "ids stay unique");
        let _ = fs::remove_dir_all(&udd);
    }

    #[test]
    fn a_later_start_moves_and_removes_only_ours_and_leaves_the_operators_where_they_are() {
        let udd = udd();
        write(&udd, &profile(vec![operator("Mine first"), operator_folder("My stuff", vec![operator("Mine deep")])], vec![operator("Mine other")]));
        let first = [placed("A", "", &[]), placed("B", "", &["Work"]), placed("C", "", &["Work"]), placed("D", "", &[])];
        apply_to_profile(&udd, "", &first).unwrap();
        let mut tree = read(&udd);
        // The first time, ours go first; the operator's follow as they were.
        assert_eq!(kids(&tree, "bookmark_bar"), json!(["A", { "Work": ["B", "C"] }, "D", "Mine first", { "My stuff": ["Mine deep"] }]));
        assert_eq!(kids(&tree, "other"), json!(["Mine other"]));
        assert_eq!((tree["checksum"].as_str(), tree.get("checksum_sha256")), (Some(""), None));
        let guid_of = |t: &Value, name: &str| find(&t["roots"], name).unwrap()["guid"].clone();
        let a_guid = guid_of(&tree, "A");
        let b_guid = guid_of(&tree, "B");

        // In the profile: one of the operator's between ours, one inside our Work, and
        // our D dragged into the operator's folder.
        let bar = tree["roots"]["bookmark_bar"]["children"].as_array_mut().unwrap();
        bar.insert(1, operator("Mine between"));
        let d = bar.remove(3);
        let work = bar.iter_mut().find(|n| n["name"] == "Work").unwrap();
        work["children"].as_array_mut().unwrap().push(operator("Mine in Work"));
        let mine = bar.iter_mut().find(|n| n["name"] == "My stuff").unwrap();
        mine["children"].as_array_mut().unwrap().push(d);
        write(&udd, &tree);

        // The launcher's list changes: A renamed, B moves to Other bookmarks, C goes, E comes.
        let next = [
            Bookmark { title: "A renamed".into(), ..placed("A", "", &[]) },
            placed("D", "", &[]),
            placed("E", "", &[]),
            placed("B", "other", &["Moved"]),
        ];
        apply_to_profile(&udd, "", &next).unwrap();
        let tree = read(&udd);
        // Ours fill the place ours still had (A's); Work holds the operator's own, so it stays
        // where it was with only theirs; D is back out of their folder.
        assert_eq!(
            kids(&tree, "bookmark_bar"),
            json!(["A renamed", "D", "E", "Mine between", { "Work": ["Mine in Work"] }, "Mine first", { "My stuff": ["Mine deep"] }])
        );
        assert_eq!(kids(&tree, "other"), json!([{ "Moved": ["B"] }, "Mine other"]));
        // A bookmark kept or moved keeps its node: Chrome sees the same bookmark.
        assert_eq!(guid_of(&tree, "A renamed"), a_guid);
        assert_eq!(guid_of(&tree, "B"), b_guid);
        assert_eq!(find(&tree["roots"], "Mine first").unwrap().get("meta_info"), None, "the operator's own are not marked");

        // Nothing of ours wanted: ours go, the operator's stay; Work stays while it holds theirs, Moved goes.
        apply_to_profile(&udd, "", &[]).unwrap();
        let tree = read(&udd);
        assert_eq!(kids(&tree, "bookmark_bar"), json!(["Mine between", { "Work": ["Mine in Work"] }, "Mine first", { "My stuff": ["Mine deep"] }]));
        assert_eq!(kids(&tree, "other"), json!(["Mine other"]));
        let _ = fs::remove_dir_all(&udd);
    }

    #[test]
    fn the_old_shardbrowser_folder_gives_way_to_chromes_places() {
        let udd = udd();
        let old = json!({
            "children": [operator("Old A"), operator_folder("Old sub", vec![operator("Old B")])],
            "date_added": "1", "date_modified": "1", "guid": uuid::Uuid::new_v4().to_string(), "id": "0",
            "name": "ShardBrowser", "type": "folder", "meta_info": { OLD_FOLDER_KEY: "1" },
        });
        let legacy = operator_folder(LEGACY_FOLDER, vec![operator("Older")]);
        // The operator's own folder named like ours, not at the bar's top, stays.
        let theirs = operator_folder("Kept", vec![operator_folder(LEGACY_FOLDER, vec![operator("Theirs")])]);
        write(&udd, &profile(vec![operator("Mine first"), old, legacy, theirs], vec![]));
        let all = [placed("A", "", &[]), placed("B", "", &["Work"]), placed("E", "other", &[])];
        apply_to_profile(&udd, "", &all).unwrap();
        let tree = read(&udd);
        // Ours take the old folder's place on the bar; what it held is gone with it.
        assert_eq!(kids(&tree, "bookmark_bar"), json!(["Mine first", "A", { "Work": ["B"] }, { "Kept": [{ "ShardX": ["Theirs"] }] }]));
        assert_eq!(kids(&tree, "other"), json!(["E"]));
        // A profile whose old folder is all there was of ours: the folder still goes.
        let udd2 = self::udd();
        write(&udd2, &profile(vec![operator("Mine")], vec![]));
        let mut t = read(&udd2);
        t["roots"]["bookmark_bar"]["children"].as_array_mut().unwrap().insert(0, json!({ "children": [], "id": "99", "name": "ShardBrowser", "type": "folder", "meta_info": { OLD_FOLDER_KEY: "1" } }));
        write(&udd2, &t);
        apply_to_profile(&udd2, "", &[]).unwrap();
        assert_eq!(kids(&read(&udd2), "bookmark_bar"), json!(["Mine"]));
        // A profile that never had ours and gets none is not rewritten.
        let udd3 = self::udd();
        write(&udd3, &json!({ "roots": { "bookmark_bar": { "children": [], "name": "x", "type": "folder" } }, "version": 1, "checksum": "kept" }));
        apply_to_profile(&udd3, "", &[]).unwrap();
        assert_eq!(read(&udd3)["checksum"], "kept");
        for d in [udd, udd2, udd3] {
            let _ = fs::remove_dir_all(d);
        }
    }
}
