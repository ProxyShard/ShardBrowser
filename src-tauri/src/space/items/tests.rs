//! The team's bookmarks, extensions, projects and tags, written from here to a
//! real server in-process and read back as another device would.

use super::super::client::{Method, TeamClient};
use super::super::error::{find, Need};
use super::super::{automation, bookmarks, cache, extensions, tags, CmdError, Door, Ticket};
use super::*;
use crate::team;
use base64::Engine as _;
use serde_json::{json, Value};
use shardbrowser_team_server::{Action, Grant, Scope, TestServer};

fn team_wide(actions: &[Action]) -> Vec<Grant> {
    actions.iter().map(|a| Grant { scope: Scope::Team, action: *a, allow: true }).collect()
}

fn writer() -> Vec<Grant> {
    team_wide(&[Action::See, Action::Create, Action::Edit, Action::Delete])
}

/// A team server in-process, Vera joined from here, her lists read.
struct Team {
    // Stopped before its data goes with the root.
    srv: TestServer,
    root: crate::store::TestRoot,
    team_id: String,
    vera_id: String,
    _serial: std::sync::MutexGuard<'static, ()>,
}

impl Team {
    async fn new(grants: &[Grant]) -> Team {
        let serial = crate::migrate::serial();
        let root = crate::store::test_root("team-items").with_config();
        let srv = TestServer::start(root.path.join("server")).await.unwrap();
        let vera = srv.add_member("Vera", grants).unwrap();
        team::join(&srv.url, &vera.token).await.unwrap();
        let team_id = srv.server.team_id().to_string();
        cache::keep_only(None);
        super::super::ready().await;
        Team { srv, root, team_id, vera_id: vera.id, _serial: serial }
    }

    fn admin(&self) -> TeamClient {
        TeamClient::new(&self.srv.url, &self.srv.admin_token, "admin-device").unwrap()
    }

    /// The item as the server keeps it, data opened; None when it is not there.
    async fn stored(&self, kind: &str, id: &str) -> Option<(Value, Value)> {
        let (_, body) = self.admin().call(Method::GET, &format!("/items/{kind}/{id}"), None).await.ok()?;
        let row: Value = serde_json::from_slice(&body).unwrap();
        let raw = base64::engine::general_purpose::STANDARD.decode(row["data"].as_str().unwrap()).unwrap();
        Some((row, serde_json::from_slice(&raw).unwrap()))
    }

    /// Everything of a kind the server has, rows and data as sent.
    async fn all(&self, kind: &str) -> Value {
        let (_, body) = self.admin().call(Method::GET, &format!("/items/{kind}"), None).await.unwrap();
        serde_json::from_slice(&body).unwrap()
    }

    /// Read again from the server, as after a restart or on another device.
    async fn read_again(&self) {
        cache::keep_only(None);
        super::super::ready().await;
    }

    async fn set_grants(&self, grants: &[Grant]) {
        let path = format!("/admin/members/{}/grants", self.vera_id);
        self.admin().call(Method::PUT, &path, Some(&json!({ "grants": grants }))).await.unwrap();
        let issued = TeamClient::for_team(&self.team_id).unwrap().permission().await.unwrap();
        perm::accept(&self.team_id, &issued.permission).unwrap();
    }
}

fn refusal(e: anyhow::Error) -> Option<CmdError> {
    find(&e).cloned()
}

fn forbidden(right: &'static str) -> Option<CmdError> {
    Some(CmdError::Forbidden(Need { right, on: "team" }))
}

fn mark(id: &str, title: &str, url: &str, folder: &str) -> bookmarks::Bookmark {
    bookmarks::Bookmark { id: id.into(), title: title.into(), url: url.into(), folder: folder.into(), root: String::new(), path: Vec::new() }
}

#[tokio::test]
async fn bookmarks_and_tags_go_to_the_team_and_come_back() {
    let t = Team::new(&writer()).await;
    let tk = Ticket::now();

    // A bookmark: new, changed, read back from the server, deleted.
    let b = bookmarks::save(Door("bookmark_save"), &tk, mark("", " Mail ", "mail.example", "Ads")).await.unwrap();
    let (row, data) = t.stored("bookmark", &b.id).await.unwrap();
    assert_eq!((row["name"].as_str(), row["revision"].as_u64()), (Some("Mail"), Some(1)));
    assert_eq!(data, json!({ "id": b.id, "title": "Mail", "url": "https://mail.example", "folder": "Ads" }));
    bookmarks::save(Door("bookmark_save"), &tk, mark(&b.id, "Inbox", "https://mail.example", "")).await.unwrap();
    assert_eq!(t.stored("bookmark", &b.id).await.unwrap().0["revision"], 2);
    t.read_again().await;
    let back = bookmarks::list().unwrap();
    assert_eq!(back.len(), 1);
    assert_eq!((back[0].id.as_str(), back[0].title.as_str(), back[0].folder.as_str()), (b.id.as_str(), "Inbox", ""));
    assert!(!crate::store::bookmarks_path().unwrap().exists(), "nothing in this machine's own bookmarks");
    bookmarks::delete(Door("bookmark_delete"), &Ticket::now(), &b.id).await.unwrap();
    assert!(t.stored("bookmark", &b.id).await.is_none());
    assert!(bookmarks::list().unwrap().is_empty());
    // Several at once, in one call.
    let tk = Ticket::now();
    let x = bookmarks::save(Door("bookmark_save"), &tk, mark("", "X", "x.example", "")).await.unwrap();
    let y = bookmarks::save(Door("bookmark_save"), &tk, mark("", "Y", "y.example", "")).await.unwrap();
    bookmarks::delete_many(Door("bookmark_delete_many"), &Ticket::now(), &[x.id.clone(), y.id.clone()]).await.unwrap();
    assert!(t.stored("bookmark", &x.id).await.is_none() && t.stored("bookmark", &y.id).await.is_none());
    assert!(bookmarks::list().unwrap().is_empty());

    // Tags: by name, one name once, a deleted one off every profile that had it.
    let tk = Ticket::now();
    let tag = |id: &str, name: &str| tags::Tag { id: id.into(), name: name.into(), color: "violet".into() };
    let vip = tags::save(Door("tag_save"), &tk, tag("", "VIP"), Some(0)).await.unwrap();
    let warm = tags::save(Door("tag_save"), &tk, tag("", "Warm-up"), None).await.unwrap();
    assert!(vip.id.starts_with("t-"));
    let clash = tags::save(Door("tag_save"), &tk, tag("", "vip"), None).await.unwrap_err();
    assert!(clash.to_string().contains("already exists"), "{clash}");
    assert_eq!(t.all("tag").await.as_array().unwrap().len(), 2, "the clash wrote nothing");
    tags::save(Door("tag_save"), &tk, tags::Tag { color: "red".into(), ..tag(&vip.id, "Gold") }, None).await.unwrap();
    assert_eq!(t.stored("tag", &vip.id).await.unwrap().1, json!({ "id": vip.id, "name": "Gold", "color": "red" }));

    let body = json!({ "name": "Shop", "folder": "", "data": "", "card": { "tags": [vip.id, warm.id] } });
    t.admin().call(Method::PUT, "/profiles/p1", Some(&body)).await.unwrap();
    t.read_again().await;
    let held = tags::list().unwrap();
    let names: Vec<&str> = held.tags.iter().map(|t| t.name.as_str()).collect();
    assert_eq!(names, ["Gold", "Warm-up"]);
    assert_eq!(held.by_profile["p1"], [vip.id.clone(), warm.id.clone()]);
    tags::delete(Door("tag_delete"), &Ticket::now(), &vip.id).await.unwrap();
    assert!(t.stored("tag", &vip.id).await.is_none());
    let held = tags::list().unwrap();
    assert_eq!((held.tags.len(), &held.by_profile["p1"]), (1, &vec![warm.id.clone()]));

    // Put back under its old id, it comes back under a new one: the old waits in the trash.
    let again = tags::save(Door("tag_save"), &Ticket::now(), tag(&vip.id, "Gold"), None).await.unwrap();
    assert_ne!(again.id, vip.id);
    assert!(t.stored("tag", &again.id).await.is_some());
    assert!(!crate::store::tags_path().unwrap().exists(), "nothing in this machine's own tags");
    drop(t.srv);
}

#[tokio::test]
async fn projects_go_to_the_team_with_their_revisions() {
    let t = Team::new(&writer()).await;
    let tk = Ticket::now();
    let made = automation::create(Door("automation_create"), &tk, " Warm up ").await.unwrap();
    assert_eq!((made.name.as_str(), made.revision), ("Warm up", Some(1)));
    let (row, data) = t.stored("automation", &made.id).await.unwrap();
    assert_eq!(row["name"], "Warm up");
    assert!(data.get("revision").is_none(), "the revision is the server's, not the data's");

    let mut p = made.clone();
    p.notes = "first".into();
    p.blocks = serde_json::from_value(json!([
        { "id": "a", "kind": "goto", "params": { "url": "https://a.example" } },
        { "id": "a", "kind": "wait", "params": { "seconds": 2 } }
    ]))
    .unwrap();
    p.run.profiles = vec!["this-machines-profile".into()];
    let saved = automation::save(Door("automation_save"), &tk, p.clone()).await.unwrap();
    assert_eq!(saved.revision, Some(2));
    let (_, data) = t.stored("automation", &made.id).await.unwrap();
    assert_eq!(data["notes"], "first");
    assert_eq!(data["run"]["profiles"], json!([]), "this machine's ids stay here");
    let ids: Vec<&str> = data["blocks"].as_array().unwrap().iter().map(|b| b["id"].as_str().unwrap()).collect();
    assert!(ids[0] == "a" && ids[1] != "a", "{ids:?}");

    // Saved elsewhere meanwhile: a save from the revision read is stale and writes nothing.
    let mut theirs = data.clone();
    theirs["notes"] = json!("theirs");
    let body = json!({ "name": "Warm up", "data": base64::engine::general_purpose::STANDARD.encode(theirs.to_string()), "base_revision": 2 });
    t.admin().call(Method::PUT, &format!("/items/automation/{}", made.id), Some(&body)).await.unwrap();
    p.notes = "mine".into();
    p.revision = saved.revision;
    let stale = automation::save(Door("automation_save"), &tk, p).await.unwrap_err();
    assert!(matches!(refusal(stale), Some(CmdError::Stale { what: "project", now: Some(3), had: Some(2) })));
    t.read_again().await;
    let back = automation::list().unwrap();
    assert_eq!((back[0].notes.as_str(), back[0].revision, back[0].blocks.len()), ("theirs", Some(3), 2));

    let copy = automation::duplicate(Door("automation_duplicate"), &Ticket::now(), &made.id).await.unwrap();
    assert_eq!(t.stored("automation", &copy.id).await.unwrap().0["name"], "Warm up copy");
    let bundle = automation::export(&made.id);
    assert!(refusal(bundle.unwrap_err()).is_some_and(|e| e.code() == "forbidden"), "no TakeOut");
    let mut bundle = crate::space::local::automation::bundle(back[0].clone());
    bundle.project.name = "Imported".into();
    let imported = automation::import(Door("automation_import"), &Ticket::now(), bundle).await.unwrap();
    assert_ne!(imported.id, made.id);
    assert_eq!(t.stored("automation", &imported.id).await.unwrap().0["name"], "Imported");

    for id in [&made.id, &copy.id, &imported.id] {
        automation::delete(Door("automation_delete"), &Ticket::now(), id).await.unwrap();
    }
    assert_eq!(t.all("automation").await, json!([]));
    assert!(automation::list().unwrap().is_empty());
    assert!(!crate::store::automation_path().unwrap().exists(), "nothing in this machine's own projects");
    drop(t.srv);
}

/// An extension's folder: a manifest naming it, a script and an icon.
pub(crate) fn an_extension(dir: &std::path::Path) -> std::path::PathBuf {
    let ext = dir.join("wallet");
    std::fs::create_dir_all(ext.join("js")).unwrap();
    let manifest = json!({ "manifest_version": 3, "name": "Wallet", "version": "1.2.3", "description": "keys", "icons": { "16": "i.png" } });
    std::fs::write(ext.join("manifest.json"), manifest.to_string()).unwrap();
    std::fs::write(ext.join("js/bg.js"), "console.log('bg')").unwrap();
    std::fs::write(ext.join("i.png"), [0x89, b'P', b'N', b'G']).unwrap();
    ext
}

/// The files of a zip, by name.
fn unzip(bytes: &[u8]) -> std::collections::BTreeMap<String, Vec<u8>> {
    use std::io::Read as _;
    let mut z = zip::ZipArchive::new(std::io::Cursor::new(bytes)).unwrap();
    (0..z.len())
        .map(|i| {
            let mut f = z.by_index(i).unwrap();
            let mut buf = Vec::new();
            f.read_to_end(&mut buf).unwrap();
            (f.name().to_string(), buf)
        })
        .collect()
}

#[tokio::test]
async fn an_extension_goes_up_with_its_files_and_is_not_kept_here() {
    let t = Team::new(&writer()).await;
    let src = an_extension(&t.root.path);
    let e = extensions::import(Door("extension_import"), &Ticket::now(), &src).await.unwrap();
    assert_eq!((e.name.as_str(), e.version.as_str(), e.path.as_str()), ("Wallet", "1.2.3", ""));
    assert!(e.icon.starts_with("data:image/png;base64,"), "{}", e.icon);
    let (row, data) = t.stored("extension", &e.id).await.unwrap();
    assert_eq!((row["name"].as_str(), row["package"]["package_rev"].as_u64()), (Some("Wallet"), Some(1)));
    assert_eq!((data["version"].as_str(), data["path"].as_str()), (Some("1.2.3"), Some("")));

    let (_, zip) = t.admin().call(Method::GET, &format!("/items/extension/{}/package", e.id), None).await.unwrap();
    let files = unzip(&zip);
    assert_eq!(files.keys().map(String::as_str).collect::<Vec<_>>(), ["i.png", "js/bg.js", "manifest.json"]);
    assert_eq!(files["js/bg.js"], b"console.log('bg')");
    let library = crate::store::extensions_dir().unwrap();
    assert_eq!(std::fs::read_dir(&library).unwrap().count(), 0, "nothing in this machine's library");

    t.read_again().await;
    let listed = extensions::list().unwrap();
    assert_eq!((listed.len(), listed[0].id.as_str(), listed[0].name.as_str()), (1, e.id.as_str(), "Wallet"));
    assert_eq!(listed[0].size_bytes, e.size_bytes);

    // Not an extension: refused before anything is sent.
    let empty = t.root.path.join("empty");
    std::fs::create_dir_all(&empty).unwrap();
    let err = extensions::import(Door("extension_import"), &Ticket::now(), &empty).await.unwrap_err();
    assert!(err.to_string().contains("not an extension"), "{err}");
    assert_eq!(t.all("extension").await.as_array().unwrap().len(), 1);

    extensions::delete(Door("extension_delete"), &Ticket::now(), &e.id).await.unwrap();
    assert!(t.stored("extension", &e.id).await.is_none());
    assert!(extensions::list().unwrap().is_empty());
    drop(t.srv);
}

/// A store package goes up with where it came from and the store's key, so
/// every member's copy has the store's id; the same item is not added twice.
#[tokio::test]
async fn a_store_extension_goes_up_with_its_source_and_key_once() {
    use local::extensions::testkit::{crx3, stand_in, zip, Key};
    let t = Team::new(&writer()).await;
    let (dev, store) = (Key::new(), Key::new());
    let _store = stand_in(&store);
    let manifest = r#"{"name":"Wallet","version":"1.0","manifest_version":3}"#;
    let pkg = t.root.path.join("wallet.crx");
    std::fs::write(&pkg, crx3(&dev, Some(&store), &zip(&[("manifest.json", manifest.as_bytes())]))).unwrap();

    let e = extensions::import(Door("extension_import"), &Ticket::now(), &pkg).await.unwrap();
    let (_, data) = t.stored("extension", &e.id).await.unwrap();
    assert_eq!(data["source"], json!({ "kind": "webstore", "id": dev.id() }));
    let (_, package) = t.admin().call(Method::GET, &format!("/items/extension/{}/package", e.id), None).await.unwrap();
    let shipped: Value = serde_json::from_slice(&unzip(&package)["manifest.json"]).unwrap();
    assert_eq!(shipped["key"], base64::engine::general_purpose::STANDARD.encode(&dev.spki));

    let again = extensions::import(Door("extension_import"), &Ticket::now(), &pkg).await.unwrap_err();
    let refused = refusal(again).expect("a refusal");
    assert_eq!(refused.code(), "already_in_library");
    assert!(refused.to_string().ends_with(&format!(r#"{{"entry_id":"{}","installed":"1.0","separate":false}}"#, e.id)), "{refused}");
    assert_eq!(t.all("extension").await.as_array().unwrap().len(), 1);
    drop(t.srv);
}

#[tokio::test]
async fn each_write_asks_its_right_on_the_whole_team() {
    let t = Team::new(&team_wide(&[Action::See])).await;
    let data = json!({ "url": "https://m.example", "title": "Mail" }).to_string();
    let body = json!({ "name": "Mail", "data": base64::engine::general_purpose::STANDARD.encode(data) });
    t.admin().call(Method::PUT, "/items/bookmark/b1", Some(&body)).await.unwrap();
    t.read_again().await;
    let tk = Ticket::now();

    let new = bookmarks::save(Door("bookmark_save"), &tk, mark("", "x", "x.example", "")).await;
    assert_eq!(refusal(new.unwrap_err()), forbidden("create"));
    let edit = bookmarks::save(Door("bookmark_save"), &tk, mark("b1", "y", "y.example", "")).await;
    assert_eq!(refusal(edit.unwrap_err()), forbidden("edit"));
    let gone = bookmarks::delete(Door("bookmark_delete"), &tk, "b1").await;
    assert_eq!(refusal(gone.unwrap_err()), forbidden("delete"));
    let gone = bookmarks::delete_many(Door("bookmark_delete_many"), &tk, &["b1".to_string()]).await;
    assert_eq!(refusal(gone.unwrap_err()), forbidden("delete"));
    let src = an_extension(&t.root.path);
    let ext = extensions::import(Door("extension_import"), &tk, &src).await;
    assert_eq!(refusal(ext.unwrap_err()), forbidden("create"));
    let p = automation::create(Door("automation_create"), &tk, "x").await;
    assert_eq!(refusal(p.unwrap_err()), forbidden("create"));
    assert_eq!(t.stored("bookmark", "b1").await.unwrap().0["revision"], 1, "nothing changed");
    assert_eq!(t.all("extension").await, json!([]));

    // Create alone makes, and changes nothing made before.
    t.set_grants(&team_wide(&[Action::See, Action::Create])).await;
    bookmarks::save(Door("bookmark_save"), &tk, mark("", "x", "x.example", "")).await.unwrap();
    let edit = bookmarks::save(Door("bookmark_save"), &tk, mark("b1", "y", "y.example", "")).await;
    assert_eq!(refusal(edit.unwrap_err()), forbidden("edit"));
    // Without See the team's items are not there at all.
    t.set_grants(&team_wide(&[Action::Create])).await;
    t.read_again().await;
    let gone = bookmarks::delete(Door("bookmark_delete"), &Ticket::now(), "b1").await;
    assert!(matches!(refusal(gone.unwrap_err()), Some(CmdError::Missing(_))));
    drop(t.srv);
}

/// A project whose typing step keeps a password.
fn with_a_password(p: &mut automation::Project, password: &str) {
    p.blocks = serde_json::from_value(json!([
        { "id": "login", "kind": "type", "label": "Password",
          "params": { "selector": "#pw", "text": password }, "secrets": ["text"] },
        { "id": "go", "kind": "goto", "params": { "url": "https://shop.example" } }
    ]))
    .unwrap();
}

fn text_of(p: &automation::Project) -> &str {
    p.blocks[0].params["text"].as_str().unwrap()
}

/// Every file under `dir` whose bytes hold `needle`.
fn holding(dir: &std::path::Path, needle: &[u8], out: &mut Vec<std::path::PathBuf>) {
    for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let p = e.path();
        if p.is_dir() {
            holding(&p, needle, out);
        } else if std::fs::read(&p).is_ok_and(|b| b.windows(needle.len()).any(|w| w == needle)) {
            out.push(p);
        }
    }
}

#[tokio::test]
async fn a_secret_value_stays_on_this_machine_and_reaches_the_server_blank() {
    let t = Team::new(&writer()).await;
    let secret = "hunter2-secret-77";
    let tk = Ticket::now();
    let mut p = automation::create(Door("automation_create"), &tk, "Login").await.unwrap();
    with_a_password(&mut p, secret);
    let saved = automation::save(Door("automation_save"), &tk, p).await.unwrap();
    assert_eq!(text_of(&saved), secret, "the editor keeps what it showed");
    let copy = automation::duplicate(Door("automation_duplicate"), &tk, &saved.id).await.unwrap();
    assert_eq!(text_of(&copy), secret);
    let mut bundle = crate::space::local::automation::bundle(saved.clone());
    bundle.project.blocks[0].params["text"] = json!(secret);
    let imported = automation::import(Door("automation_import"), &tk, bundle).await.unwrap();

    // What the server stored: each step there, the value blank.
    for id in [&saved.id, &copy.id, &imported.id] {
        let (_, data) = t.stored("automation", id).await.unwrap();
        assert_eq!(data["blocks"][0]["params"], json!({ "selector": "#pw", "text": "" }), "{id}");
        assert_eq!(data["blocks"][0]["secrets"], json!(["text"]));
        assert!(!data.to_string().contains(secret));
    }
    let mut found = Vec::new();
    holding(&t.root.path.join("server"), secret.as_bytes(), &mut found);
    assert!(found.is_empty(), "the server keeps it: {found:?}");

    // Read again from the server, the value comes from this machine, for the editor and a run.
    t.read_again().await;
    let listed = automation::list().unwrap();
    assert_eq!(listed.len(), 3);
    assert!(listed.iter().all(|p| text_of(p) == secret));
    let kept = super::super::secrets::kept_file(&t.team_id);
    assert!(kept.starts_with(crate::store::local_root().unwrap()), "{}", kept.display());
    assert!(String::from_utf8(std::fs::read(&kept).unwrap()).unwrap().contains(secret));
    // A bundle taken out of the team carries none.
    let bundle = crate::space::local::automation::bundle(listed[0].clone());
    assert_eq!(bundle.project.blocks[0].params["text"], "");

    // Blank, it is forgotten here; unmarked, it goes up as any other value.
    let mut p = listed.into_iter().find(|p| p.id == saved.id).unwrap();
    p.blocks[0].params["text"] = json!("");
    automation::save(Door("automation_save"), &Ticket::now(), p).await.unwrap();
    let mut back = automation::list().unwrap().into_iter().find(|p| p.id == saved.id).unwrap();
    assert_eq!(text_of(&back), "");
    back.blocks[0].secrets.clear();
    back.blocks[0].params["text"] = json!("not-secret-1");
    automation::save(Door("automation_save"), &Ticket::now(), back).await.unwrap();
    assert_eq!(t.stored("automation", &saved.id).await.unwrap().1["blocks"][0]["params"]["text"], "not-secret-1");

    // Deleted, its values go; the team forgotten, every value goes.
    automation::delete(Door("automation_delete"), &Ticket::now(), &copy.id).await.unwrap();
    assert!(!String::from_utf8(std::fs::read(&kept).unwrap()).unwrap().contains(&copy.id));
    super::super::forget_teams().unwrap();
    assert!(!kept.exists());
    drop(t.srv);
}
