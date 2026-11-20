//! Two computers of one person against a real server in-process. Each has its
//! own settings and data folders; a test acts as one of them at a time.

use std::fs;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use shardbrowser_team_server::TestServer;

use super::super::client::{Method, TeamClient};
use super::super::local;
use super::super::open::{self, Home};
use super::super::profile::StoredProfile;
use super::book::{self, Op};
use super::*;
use crate::team;

static HEARD: Mutex<Vec<u64>> = Mutex::new(Vec::new());

pub(super) fn heard(st: &SyncState) {
    HEARD.lock().unwrap_or_else(|e| e.into_inner()).push(st.rev);
}

static WARNED: Mutex<Vec<String>> = Mutex::new(Vec::new());

pub(super) fn warned(w: &str) {
    WARNED.lock().unwrap_or_else(|e| e.into_inner()).push(w.to_string());
}

/// One computer: its settings, its data, its name.
struct Device {
    config: PathBuf,
    data: PathBuf,
    name: &'static str,
}

struct Two {
    // Stopped before its data goes with the root.
    srv: TestServer,
    root: crate::store::TestRoot,
    token: String,
    a: Device,
    b: Device,
    engine: PathBuf,
    _serial: std::sync::MutexGuard<'static, ()>,
}

/// Wait for `check`, failing past `within`.
async fn until(what: &str, within: Duration, mut check: impl FnMut() -> bool) {
    let start = Instant::now();
    while !check() {
        assert!(start.elapsed() < within, "{what}: not within {within:?}");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

impl Two {
    /// A server, Vera on it, and her two computers; A connected with her token.
    async fn new() -> Two {
        let serial = crate::migrate::serial();
        let root = crate::store::test_root("sync-two");
        let srv = TestServer::start(root.path.join("server")).await.unwrap();
        let vera = srv.add_member("Vera", &[]).unwrap();
        let dev = |tag: &str, name| Device { config: root.path.join(tag).join("config"), data: root.path.join(tag).join("data"), name };
        let (a, b) = (dev("a", "Vera's Mac"), dev("b", "Vera's PC"));
        let engine = root.path.join("engine.sh");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let host = crate::reset::this_host().unwrap();
            let script = format!(
                "#!/bin/sh\nfor a in \"$@\"; do case \"$a\" in --user-data-dir=*) udd=\"${{a#--user-data-dir=}}\";; esac; done\n\
                 echo \"$udd\" >> '{}'\nln -s '{host}-'$$ \"$udd/SingletonLock\"\nexec sleep 600\n",
                root.path.join("started").display()
            );
            fs::write(&engine, script).unwrap();
            fs::set_permissions(&engine, fs::Permissions::from_mode(0o755)).unwrap();
        }
        let two = Two { srv, root, token: vera.token, a, b, engine, _serial: serial };
        for d in [&two.a, &two.b] {
            two.be(d);
            team::set_device_name(d.name).unwrap();
        }
        two.be(&two.a);
        connect(Some(two.srv.url.clone()), Some(two.token.clone()), None).await.unwrap();
        two
    }

    /// Act as this computer from here on.
    fn be(&self, d: &Device) {
        crate::store::set_test_config(Some(d.config.clone()));
        crate::store::set_data_root(Some(d.data.clone()));
    }

    /// B joins with the invite A gives.
    async fn join_b(&self) {
        self.be(&self.a);
        let invite = invite().unwrap();
        let (server, token) = invite.split_once('#').unwrap();
        assert_eq!((server, token), (self.srv.url.as_str(), self.token.as_str()), "the same server and token");
        self.be(&self.b);
        connect(Some(server.into()), Some(token.into()), None).await.unwrap();
    }

    /// A profile of this computer's, synced: its id.
    fn profile(&self, name: &str) -> String {
        let mut s: StoredProfile = serde_json::from_value(json!({ "name": name, "notes": "" })).unwrap();
        crate::space::profile::save_raw(&mut s).unwrap();
        profile_set(&[s.meta.id.clone()], true).unwrap();
        s.meta.id
    }

    fn set_notes(&self, id: &str, notes: &str) {
        let mut s = local::profile::load_raw(id).unwrap();
        s.config.insert("notes".into(), Value::String(notes.into()));
        crate::space::profile::save_raw(&mut s).unwrap();
    }

    fn notes(&self, id: &str) -> String {
        local::profile::load_raw(id).unwrap().config["notes"].as_str().unwrap_or_default().to_string()
    }

    fn row(&self, id: &str) -> ProfileMeta {
        crate::space::profile::list_all().unwrap().into_iter().find(|r| r.id == id).unwrap()
    }

    /// The server as Vera's own script sees her space.
    fn script(&self) -> TeamClient {
        TeamClient::new(&self.srv.url, &self.token, "script").unwrap()
    }

    async fn on_server(&self, what: &str) -> Vec<Value> {
        let (_, body) = self.script().call(Method::GET, &format!("/me/{what}"), None).await.unwrap();
        serde_json::from_slice(&body).unwrap()
    }

    /// Started, and up: the fake browser has its lock.
    async fn launch(&self, id: &str) -> anyhow::Result<crate::launch::LaunchOutcome> {
        let before = self.started();
        let out = crate::launch::launch_with(self.engine.clone(), id, false).await?;
        until("the browser up", Duration::from_secs(30), || self.started() > before).await;
        Ok(out)
    }

    fn started(&self) -> usize {
        fs::read_to_string(self.root.path.join("started")).unwrap_or_default().lines().count()
    }

    fn home(&self) -> Home {
        Home::mine(self.srv.server.team_id())
    }

    /// Stopped, and its close run to the end.
    async fn stop(&self, id: &str) {
        assert!(crate::process::Tracker::shared().kill(id).await.unwrap(), "nothing to stop");
        self.closed(id).await;
    }

    async fn closed(&self, id: &str) {
        let home = self.home();
        until("the close", Duration::from_secs(20), || {
            !open::open_here(&home, id) && !crate::process::Tracker::shared().is_running(id)
        })
        .await;
    }

    fn udd(&self, id: &str) -> PathBuf {
        crate::store::user_data_root().unwrap().join(id)
    }

    fn offline(&self) {
        team::set_sync_server("http://127.0.0.1:9");
    }

    fn online(&self) {
        team::set_sync_server(&self.srv.url);
    }
}

fn a_cookie(domain: &str, name: &str, value: &str) -> local::cookies::Cookie {
    serde_json::from_value(json!({ "domain": domain, "name": name, "value": value, "expires": 4_000_000_000.0 })).unwrap()
}

#[cfg(unix)]
#[tokio::test]
async fn two_computers_share_a_profile_and_take_turns_with_it() {
    let t = Two::new().await;
    let x = t.profile("Shop");
    cycle::run_once().await;
    assert_eq!(t.on_server("profiles").await[0]["id"], x.as_str());
    assert_eq!(t.row(&x).sync, Some(ProfileSync { on: true, open_on: None, seen_at: None, conflict: None }));

    t.join_b().await;
    cycle::run_once().await;
    assert_eq!(local::profile::load_raw(&x).unwrap().config["name"], "Shop", "B has it");
    assert!(t.row(&x).sync.unwrap().on);

    // Open on A: B is refused, by every door, in the interface's own words.
    t.be(&t.a);
    t.launch(&x).await.unwrap();
    let quit_closes: Vec<_> = open::listed().into_iter().map(|l| (l.profile_id, l.mine)).collect();
    assert_eq!(quit_closes, [(x.clone(), true)], "the quit dialog names it");
    t.be(&t.b);
    let open_on_a = "this profile is open on Vera's Mac: close it there first";
    assert_eq!(t.launch(&x).await.err().unwrap().to_string(), open_on_a);
    #[cfg(feature = "automation")]
    {
        let err = crate::launch::launch_for_run(&crate::space::Ticket::now(), &x, 0, "").await.err().unwrap();
        assert_eq!(err.to_string(), open_on_a);
    }
    let shown = t.row(&x).sync.unwrap();
    assert_eq!(shown.open_on.as_deref(), Some("Vera's Mac"));
    assert!(shown.seen_at.is_some_and(|s| s.abs_diff(now()) < 10));
    let stop = profile_set(std::slice::from_ref(&x), false).unwrap_err().to_string();
    assert_eq!(stop, "this profile is open on Vera's Mac: stop syncing it there");

    // A closes it: its session goes up, and B opens it with the cookies.
    t.be(&t.a);
    local::cookies::import(&t.udd(&x), &[a_cookie(".shop.example", "sid", "s3cret")]).unwrap();
    t.stop(&x).await;
    let row = t.on_server("profiles").await.remove(0);
    assert_eq!((row["session_rev"].as_u64(), row["open_on"].is_null()), (Some(1), true), "sent, and the lease back");
    assert!(t.udd(&x).join("Default").exists(), "A keeps its own data");
    t.be(&t.b);
    t.launch(&x).await.unwrap();
    let jar = local::cookies::export(&t.udd(&x)).unwrap();
    assert!(jar.iter().any(|c| c.name == "sid" && c.value == "s3cret"), "{jar:?}");
    t.stop(&x).await;
    assert_eq!(t.on_server("profiles").await[0]["session_rev"], 2);
    assert!(t.udd(&x).join("Default").exists(), "B keeps it too");

    // Every change was told, each with a newer edition.
    let revs = HEARD.lock().unwrap().clone();
    assert!(revs.windows(2).all(|w| w[0] < w[1]), "{revs:?}");
}

#[tokio::test]
async fn an_edit_made_offline_waits_in_the_outbox_and_goes_when_the_server_answers() {
    let t = Two::new().await;
    let x = t.profile("Shop");
    cycle::run_once().await;
    assert_eq!(state().status.pending, 0);

    t.offline();
    t.set_notes(&x, "written on the train");
    cycle::run_once().await;
    let st = state();
    assert_eq!((st.status.state, st.status.pending), ("offline", 1));
    let waiting = book::outbox().entries;
    assert_eq!((waiting[0].kind.as_str(), waiting[0].id.as_str(), waiting[0].op), ("profile", x.as_str(), Op::Put));
    let kept = fs::read_to_string(crate::store::local_root().unwrap().join("sync-outbox.json")).unwrap();
    assert!(kept.contains(&x), "{kept}");
    assert_eq!(sync_now().await.unwrap_err().to_string(), "the sync server at 127.0.0.1:9 did not answer");

    t.online();
    cycle::run_once().await;
    let st = state();
    assert_eq!((st.status.state, st.status.pending), ("idle", 0));
    assert!(st.status.last_sync_at.is_some());
    assert_eq!(t.on_server("profiles").await[0]["card"]["notes"], "written on the train");
    assert!(book::outbox().entries.is_empty());
}

/// Sync now, refused offline, tells what waits as it is now.
#[tokio::test]
async fn sync_now_offline_counts_what_waits_at_once() {
    let t = Two::new().await;
    let x = t.profile("Shop");
    cycle::run_once().await;
    assert_eq!(state().status.pending, 0);
    t.offline();
    t.set_notes(&x, "written on the train");
    assert!(sync_now().await.is_err());
    let st = state();
    assert_eq!((st.status.state, st.status.pending), ("offline", 1));
}

#[tokio::test]
async fn a_profile_changed_on_both_computers_is_kept_twice() {
    let t = Two::new().await;
    let x = t.profile("Shop");
    cycle::run_once().await;
    t.join_b().await;
    cycle::run_once().await;

    // B changes it without the server; A changes it and sends it.
    t.offline();
    t.set_notes(&x, "from B");
    t.be(&t.a);
    t.set_notes(&x, "from A");
    cycle::run_once().await;

    t.be(&t.b);
    t.online();
    cycle::run_once().await;
    assert_eq!(t.notes(&x), "from A", "the server's stays the profile");
    let rows = crate::space::profile::list_all().unwrap();
    let copy = rows.iter().find(|r| r.name == "Shop (conflict copy)").expect("a conflict copy");
    assert_eq!(t.notes(&copy.id), "from B", "what was done here is kept");
    assert_eq!(copy.sync.as_ref().unwrap().conflict, Some(true));
    assert_eq!(t.row(&x).sync.unwrap().conflict, None);

    // The copy syncs like any other, and A gets it.
    cycle::run_once().await;
    assert_eq!(t.on_server("profiles").await.len(), 2);
    t.be(&t.a);
    cycle::run_once().await;
    assert_eq!(t.notes(&copy.id), "from B");
    assert_eq!(t.notes(&x), "from A");
}

#[cfg(unix)]
#[tokio::test]
async fn a_profile_taken_over_closes_where_it_was_and_comes_back_there_as_a_conflict_copy() {
    let t = Two::new().await;
    let x = t.profile("Shop");
    cycle::run_once().await;
    t.join_b().await;
    cycle::run_once().await;

    t.be(&t.a);
    t.launch(&x).await.unwrap();
    local::cookies::import(&t.udd(&x), &[a_cookie(".shop.example", "sid", "only-on-a")]).unwrap();

    t.be(&t.b);
    cycle::run_once().await;
    let taken = profile_take(&x).await.unwrap();
    assert_eq!(taken.sync.unwrap().open_on, None, "B holds it now");

    // A hears it at its next renewal: closed, and what it had kept as a copy.
    t.be(&t.a);
    open::renew_now(&t.home(), &x);
    t.closed(&x).await;
    let rows = crate::space::profile::list_all().unwrap();
    let copy = rows.iter().find(|r| r.name == "Shop (conflict copy)").expect("a conflict copy");
    let jar = local::cookies::export(&t.udd(&copy.id)).unwrap();
    assert!(jar.iter().any(|c| c.value == "only-on-a"), "its data went with it");
    assert!(!t.udd(&x).join("Default").exists(), "the profile takes the server's next time");
    assert_eq!(copy.sync.as_ref().unwrap().conflict, Some(true));

    // B starts it at once: its lease.
    t.be(&t.b);
    t.launch(&x).await.unwrap();
    t.stop(&x).await;
}

#[tokio::test]
async fn disconnecting_with_removal_empties_the_server_and_keeps_everything_here() {
    let t = Two::new().await;
    let proxy: super::super::proxy::ProxyEntry = serde_json::from_value(
        json!({ "id": "x1", "name": "DE", "kind": "http", "host": "203.0.113.9", "port": 8080, "username": "u", "password": "p" }),
    )
    .unwrap();
    local::proxy::upsert(proxy).unwrap();
    let x = t.profile("Shop");
    let mut s = local::profile::load_raw(&x).unwrap();
    s.meta.proxy_id = Some("x1".into());
    crate::space::profile::save_raw(&mut s).unwrap();
    local::bookmarks::save(serde_json::from_value(json!({ "title": "Mail", "url": "https://mail.example" })).unwrap()).unwrap();
    cycle::run_once().await;
    assert_eq!(t.on_server("proxies").await.len(), 1, "the bound proxy went with it");
    assert_eq!(t.on_server("items/bookmark").await.len(), 1);
    t.join_b().await;
    cycle::run_once().await;
    assert!(local::proxy::get("x1").unwrap().is_some_and(|p| p.password == "p"), "B has the proxy with its login");

    t.be(&t.a);
    let st = disconnect(true).await.unwrap();
    assert_eq!(st.connection, None);
    for what in ["profiles", "proxies", "items/bookmark", "items/sync"] {
        assert!(t.on_server(what).await.is_empty(), "{what} left on the server");
    }
    assert!(local::profile::exists(&x) && local::proxy::get("x1").unwrap().is_some(), "A keeps its own");
    assert_eq!(t.row(&x).sync, None, "no sync shown while disconnected");
    assert_eq!(sync_now().await.unwrap_err().to_string(), NOT_CONNECTED);

    // B is told, keeps its copies, and sends nothing back.
    t.be(&t.b);
    cycle::run_once().await;
    let st = state();
    assert_eq!((st.status.state, st.status.error.as_deref()), ("error", Some(REMOVED)));
    assert_eq!(sync_now().await.unwrap_err().to_string(), REMOVED);
    assert!(local::profile::exists(&x));
    assert!(t.on_server("profiles").await.is_empty());
}

#[tokio::test]
async fn what_syncs_is_the_same_on_both_and_secrets_stay_where_they_were_typed() {
    let t = Two::new().await;
    assert_eq!(state().kinds, Kinds::default());
    let kinds = Kinds { profiles: book::Profiles::All, proxies: book::Proxies::All, ..Kinds::default() };
    set_kinds(kinds.clone()).unwrap();
    let x = {
        let mut s: StoredProfile = serde_json::from_value(json!({ "name": "Not picked" })).unwrap();
        crate::space::profile::save_raw(&mut s).unwrap();
        s.meta.id
    };
    let tag = local::tags::save(local::tags::Tag { id: String::new(), name: "VIP".into(), color: "violet".into() }, None).unwrap();
    local::tags::assign(std::collections::BTreeMap::from([(x.clone(), vec![tag.id.clone()])])).unwrap();
    // A profile template (#145) goes with the folders and tags it names.
    let settings = json!({ "webrtc": "tcp_only" }).as_object().cloned().unwrap();
    let tpl = local::templates::save(local::templates::ProfileTemplate { name: "Ads base".into(), settings, tags: vec![tag.id.clone()], ..Default::default() }).unwrap();
    let mut p = local::automation::new_project("Warm up");
    p.blocks = vec![serde_json::from_value(json!({
        "id": "b1", "kind": "type", "params": { "text": "hunter2", "selector": "#pw" }, "secrets": ["text"]
    }))
    .unwrap()];
    let p = local::automation::save(p).unwrap();
    cycle::run_once().await;
    assert_eq!(t.on_server("profiles").await.len(), 1, "every profile under all");

    t.join_b().await;
    cycle::run_once().await;
    assert_eq!(state().kinds, kinds, "B follows the space's choice");
    assert!(local::profile::exists(&x));
    assert_eq!(local::tags::list().unwrap().by_profile[&x], std::slice::from_ref(&tag.id));
    let got = local::templates::list().unwrap();
    assert_eq!((got.len(), got[0].id.as_str(), got[0].settings["webrtc"].as_str()), (1, tpl.id.as_str(), Some("tcp_only")));
    assert_eq!(got[0].tags, [tag.id.clone()]);
    let there = local::automation::list().unwrap().into_iter().find(|q| q.id == p.id).unwrap();
    assert_eq!(there.blocks[0].params["text"], "", "a secret does not leave A");
    assert_eq!(there.blocks[0].params["selector"], "#pw");

    // B types its own value: it stays on B, and A keeps its own.
    let mut mine = there.clone();
    mine.blocks[0].params["text"] = json!("b-secret");
    mine.name = "Warm up 2".into();
    local::automation::save(mine).unwrap();
    cycle::run_once().await;
    let server = serde_json::to_string(&t.on_server("items/automation").await).unwrap();
    assert!(!server.contains("b-secret") && !server.contains("hunter2"), "{server}");
    t.be(&t.a);
    cycle::run_once().await;
    let back = local::automation::list().unwrap().into_iter().find(|q| q.id == p.id).unwrap();
    assert_eq!((back.name.as_str(), back.blocks[0].params["text"].as_str()), ("Warm up 2", Some("hunter2")));
}

#[tokio::test]
async fn the_doors_answer_in_the_interfaces_words() {
    let t = Two::new().await;
    let again = connect(Some(t.srv.url.clone()), Some(t.token.clone()), None).await.unwrap_err();
    assert_eq!(again.to_string(), ALREADY_CONNECTED);
    assert_eq!(set_device_name(&"x".repeat(65)).unwrap_err().to_string(), DEVICE_NAME);
    assert_eq!(set_device_name("  ").unwrap_err().to_string(), DEVICE_NAME);
    let st = set_device_name("Vera's MacBook").unwrap();
    assert_eq!(st.connection.unwrap().device_name, "Vera's MacBook");

    t.be(&t.b);
    assert_eq!(state().connection, None);
    assert_eq!(invite().unwrap_err().to_string(), NOT_CONNECTED);
    assert_eq!(profile_set(&[], true).unwrap_err().to_string(), NOT_CONNECTED);
    let err = connect(None, None, Some("no-such-team".into())).await.unwrap_err();
    assert_eq!(err.to_string(), NOT_MEMBER);
    let err = connect(Some("http://127.0.0.1:9".into()), Some("stm_x".into()), None).await.unwrap_err();
    assert_eq!(err.to_string(), "the sync server at 127.0.0.1:9 did not answer");

    // A team joined here: sync through it, with the same member's token.
    team::join(&t.srv.url, &t.token).await.unwrap();
    team::set_space(team::Space::Personal).unwrap();
    let st = connect(None, None, Some(t.srv.server.team_id().to_string())).await.unwrap();
    let c = st.connection.unwrap();
    assert_eq!((c.server.as_str(), c.own, c.device_name.as_str()), (t.srv.url.as_str(), false, "Vera's PC"));
    assert_eq!(invite().unwrap(), format!("{}#{}", t.srv.url, t.token));
    // Its rows are a team's while that space is in use: no sync set there.
    team::set_space(team::Space::Team { team_id: t.srv.server.team_id().to_string() }).unwrap();
    assert_eq!(profile_set(&[], true).unwrap_err().to_string(), crate::space::NOT_IN_TEAM);
    team::set_space(team::Space::Personal).unwrap();
}

#[cfg(unix)]
#[tokio::test]
async fn a_profile_that_starts_syncing_sends_what_its_browser_holds_without_being_opened() {
    let t = Two::new().await;
    let mut s: StoredProfile = serde_json::from_value(json!({ "name": "Old shop" })).unwrap();
    crate::space::profile::save_raw(&mut s).unwrap();
    let x = s.meta.id;
    fs::create_dir_all(t.udd(&x).join("Default")).unwrap();
    local::cookies::import(&t.udd(&x), &[a_cookie(".old.example", "sid", "kept-for-years")]).unwrap();
    profile_set(std::slice::from_ref(&x), true).unwrap();
    cycle::run_once().await;
    assert_eq!(t.on_server("profiles").await[0]["session_rev"], 1, "sent under a lease of its own");
    assert!(t.on_server("profiles").await[0]["open_on"].is_null(), "and the lease given back");
    assert_eq!(state().status.pending, 0);

    t.join_b().await;
    cycle::run_once().await;
    t.launch(&x).await.unwrap();
    let jar = local::cookies::export(&t.udd(&x)).unwrap();
    assert!(jar.iter().any(|c| c.value == "kept-for-years"), "{jar:?}");
    t.stop(&x).await;
}

#[tokio::test]
async fn an_extension_goes_across_with_its_files() {
    let t = Two::new().await;
    let src = t.root.path.join("ext-src");
    fs::create_dir_all(&src).unwrap();
    fs::write(src.join("manifest.json"), r#"{"manifest_version":3,"name":"Wallet","version":"1.2"}"#).unwrap();
    fs::write(src.join("background.js"), b"// wallet").unwrap();
    let e = local::extensions::import(&src).unwrap();
    cycle::run_once().await;
    let there = t.on_server("items/extension").await;
    assert_eq!((there[0]["id"].as_str(), there[0]["package"].is_null()), (Some(e.id.as_str()), false));

    t.join_b().await;
    cycle::run_once().await;
    let got = local::extensions::list().unwrap();
    assert_eq!((got[0].id.as_str(), got[0].name.as_str(), got[0].version.as_str()), (e.id.as_str(), "Wallet", "1.2"));
    let root = local::extensions::load_path(&e.id).expect("its files are here");
    assert_eq!(fs::read(root.join("background.js")).unwrap(), b"// wallet");
    assert_eq!(state().status.pending, 0, "nothing goes back");

    // Deleted on B: gone from the server, then from A.
    local::extensions::delete(&e.id).unwrap();
    cycle::run_once().await;
    assert!(t.on_server("items/extension").await.is_empty());
    t.be(&t.a);
    cycle::run_once().await;
    assert!(local::extensions::list().unwrap().is_empty());
}

#[tokio::test]
async fn a_profile_stopped_on_one_computer_stays_on_the_other_and_a_deleted_one_goes_to_its_trash() {
    let t = Two::new().await;
    let (x, y) = (t.profile("Shop"), t.profile("Mail"));
    cycle::run_once().await;
    t.join_b().await;
    cycle::run_once().await;
    assert!(t.row(&x).sync.unwrap().on && t.row(&y).sync.unwrap().on);

    // Stopped on A: it leaves the server, and each keeps its copy, synced no more.
    t.be(&t.a);
    profile_set(std::slice::from_ref(&x), false).unwrap();
    cycle::run_once().await;
    let ids: Vec<Value> = t.on_server("profiles").await.into_iter().map(|r| r["id"].clone()).collect();
    assert_eq!(ids, vec![json!(y)]);
    assert!(!t.row(&x).sync.unwrap().on);
    t.be(&t.b);
    cycle::run_once().await;
    assert!(local::profile::exists(&x), "B keeps its copy");
    assert!(!t.row(&x).sync.unwrap().on, "and it syncs no more there");
    assert_eq!(state().status.pending, 0, "nothing waits to go back");

    // Deleted on A: into B's trash.
    t.be(&t.a);
    local::trash::move_to_trash(&y).unwrap();
    cycle::run_once().await;
    assert!(t.on_server("profiles").await.is_empty());
    t.be(&t.b);
    cycle::run_once().await;
    assert!(!local::profile::exists(&y));
    assert!(local::trash::list().unwrap().iter().any(|e| e.id == y), "in B's trash");

    // Picked again on B: back on the server, its mark gone, and A takes it up.
    profile_set(std::slice::from_ref(&x), true).unwrap();
    cycle::run_once().await;
    let marks: Vec<Value> = t.on_server("items/sync").await.into_iter().map(|r| r["id"].clone()).collect();
    assert_eq!(marks, vec![json!("space")], "only the space's document is left");
    t.be(&t.a);
    cycle::run_once().await;
    assert!(t.row(&x).sync.unwrap().on);
    let names: Vec<String> = crate::space::profile::list_all().unwrap().into_iter().map(|r| r.name).collect();
    assert!(names.iter().all(|n| !n.contains("conflict")), "the same on both, so no copy: {names:?}");
}

#[tokio::test]
async fn what_cannot_be_read_here_is_never_taken_for_deleted() {
    let t = Two::new().await;
    let (x, y) = (t.profile("Shop"), t.profile("Mail"));
    cycle::run_once().await;
    assert_eq!(t.on_server("profiles").await.len(), 2);

    // One file garbled: that one is left alone.
    let file = crate::store::profiles_dir().unwrap().join(format!("{x}.json"));
    let kept = fs::read(&file).unwrap();
    fs::write(&file, b"{ half a").unwrap();
    cycle::run_once().await;
    assert_eq!(t.on_server("profiles").await.len(), 2, "not deleted from the server");
    assert!(book::outbox().entries.is_empty(), "nothing waits: {:?}", book::outbox().entries);
    fs::write(&file, kept).unwrap();

    // The whole data folder gone, made again empty: nothing is deleted, and sync says why.
    fs::remove_dir_all(crate::store::data_root().unwrap()).unwrap();
    cycle::run_once().await;
    assert_eq!(t.on_server("profiles").await.len(), 2);
    let st = state();
    assert_eq!(st.status.state, "error");
    assert!(st.status.error.unwrap().contains("none of the synced profiles"));
    assert!(!local::profile::exists(&y));
}

#[tokio::test]
async fn a_person_running_their_own_server_syncs_with_its_admin_token() {
    let t = Two::new().await;
    t.be(&t.b);
    let st = connect(Some(t.srv.url.clone()), Some(t.srv.admin_token.clone()), None).await.unwrap();
    let c = st.connection.unwrap();
    assert_eq!((c.own, c.device_name.as_str()), (true, "Vera's PC"));
    assert_eq!(invite().unwrap(), format!("{}#{}", t.srv.url, t.srv.admin_token));
    let x = t.profile("Own");
    cycle::run_once().await;
    let admin = TeamClient::new(&t.srv.url, &t.srv.admin_token, "script").unwrap();
    let (_, body) = admin.call(Method::GET, "/me/profiles", None).await.unwrap();
    let rows: Vec<Value> = serde_json::from_slice(&body).unwrap();
    assert_eq!(rows[0]["id"], x.as_str(), "in the admin's own space");
    assert!(t.on_server("profiles").await.is_empty(), "not in Vera's");
}

#[cfg(unix)]
#[tokio::test]
async fn a_profile_open_here_that_stops_syncing_runs_on_with_nothing_sent() {
    let t = Two::new().await;
    let (x, y) = (t.profile("Shop"), t.profile("Mail"));
    cycle::run_once().await;
    let home = t.home();
    let held = |rows: &[Value], id: &str| rows.iter().find(|r| r["id"] == id).map(|r| !r["open_on"].is_null());
    let running = |id: &str| crate::process::Tracker::shared().is_running(id);

    // Stopped here while open: its lease goes back with it, and its browser stays up.
    t.launch(&x).await.unwrap();
    assert_eq!(held(&t.on_server("profiles").await, &x), Some(true));
    profile_set(std::slice::from_ref(&x), false).unwrap();
    cycle::run_once().await;
    assert_eq!(held(&t.on_server("profiles").await, &x), None, "gone from the server");
    open::renew_now(&home, &x);
    tokio::time::sleep(Duration::from_secs(1)).await;
    assert!(running(&x), "still running");

    // Removed from the server under it: its next renewal finds nothing, and closes nothing.
    t.launch(&y).await.unwrap();
    cycle::remove_all(&TeamClient::for_sync().unwrap()).await.unwrap();
    open::renew_now(&home, &y);
    tokio::time::sleep(Duration::from_secs(1)).await;
    assert!(running(&y), "still running");

    t.stop(&x).await;
    t.stop(&y).await;
    assert!(t.on_server("profiles").await.is_empty(), "nothing sent at the close");
    assert!(local::profile::exists(&x) && local::profile::exists(&y));
}

#[tokio::test]
async fn a_project_changed_on_both_computers_is_kept_twice() {
    let t = Two::new().await;
    let p = local::automation::save(local::automation::new_project("Warm up")).unwrap();
    cycle::run_once().await;
    t.join_b().await;
    cycle::run_once().await;
    let edit = |name: &str, note: &str| {
        let mut q = local::automation::list().unwrap().into_iter().find(|q| q.id == p.id).unwrap();
        q.name = name.into();
        q.notes = note.into();
        local::automation::save(q).unwrap();
    };

    // An hour's work on B without the server; A changes it and sends it.
    t.offline();
    edit("Warm up", "B's hour of work");
    t.be(&t.a);
    edit("Warm up", "A's delay");
    cycle::run_once().await;

    t.be(&t.b);
    t.online();
    cycle::run_once().await;
    let all = local::automation::list().unwrap();
    let theirs = all.iter().find(|q| q.id == p.id).unwrap();
    assert_eq!(theirs.notes, "A's delay", "the server's takes its place");
    let copy = all.iter().find(|q| q.name == "Warm up (conflict copy)").expect("a conflict copy");
    assert_eq!(copy.notes, "B's hour of work", "what was done here is kept");

    // The copy syncs like any other, and A gets it.
    cycle::run_once().await;
    assert_eq!(t.on_server("items/automation").await.len(), 2);
    t.be(&t.a);
    cycle::run_once().await;
    let notes: Vec<String> = local::automation::list().unwrap().into_iter().map(|q| q.notes).collect();
    assert!(notes.contains(&"B's hour of work".to_string()) && notes.contains(&"A's delay".to_string()), "{notes:?}");
}

#[cfg(unix)]
#[tokio::test]
async fn a_profile_deleted_elsewhere_while_it_runs_here_goes_to_the_trash_only_once_closed() {
    let t = Two::new().await;
    let x = t.profile("Shop");
    cycle::run_once().await;
    t.join_b().await;
    cycle::run_once().await;

    // Started on B while the server did not answer: no lease, so A may delete it.
    t.offline();
    t.launch(&x).await.unwrap();
    t.online();
    let marker = t.udd(&x).join("Default/Preferences");
    fs::create_dir_all(marker.parent().unwrap()).unwrap();
    fs::write(&marker, b"in use").unwrap();
    t.be(&t.a);
    local::trash::move_to_trash(&x).unwrap();
    cycle::run_once().await;
    assert!(t.on_server("profiles").await.is_empty());

    t.be(&t.b);
    cycle::run_once().await;
    assert!(local::profile::exists(&x), "not deleted under its browser");
    assert_eq!(fs::read(&marker).unwrap(), b"in use");
    t.stop(&x).await;
    cycle::run_once().await;
    assert!(!local::profile::exists(&x));
    assert!(local::trash::list().unwrap().iter().any(|e| e.id == x), "in B's trash once closed");
}

// ---- extensions through the swap (design §3.5) ----

mod extensions {
    use super::*;
    use crate::space::local::extensions::chrome_id::chrome_id;
    use crate::space::local::extensions::source::{self, How, SourceRef};
    use crate::space::local::extensions::testkit::{manifest, Key};
    use crate::space::local::extensions::{swap, updates};
    use base64::Engine;
    use std::sync::atomic::Ordering;

    const SID: &str = "nkbihfbeogaeaoehlefnkodbefgpgknn";

    /// A wallet at `version` added to this computer's library from a folder.
    fn wallet(t: &Two, version: &str) -> String {
        let src = t.root.path.join(format!("src-{}", uuid::Uuid::new_v4().simple()));
        fs::create_dir_all(&src).unwrap();
        fs::write(src.join("manifest.json"), manifest(version, json!({}))).unwrap();
        fs::write(src.join("background.js"), format!("// {version}")).unwrap();
        local::extensions::import(&src).unwrap().id
    }

    fn lib() -> PathBuf {
        crate::store::extensions_dir().unwrap()
    }

    /// New files at the same path, as an update from the Web Store leaves them.
    fn bump(id: &str, version: &str, extra: Value) {
        let root = local::extensions::load_path(id).unwrap();
        fs::write(root.join("manifest.json"), manifest(version, extra)).unwrap();
        fs::write(root.join("background.js"), format!("// {version}")).unwrap();
    }

    fn here(id: &str) -> local::extensions::ExtensionEntry {
        local::extensions::list().unwrap().into_iter().find(|e| e.id == id).expect("in the library")
    }

    fn rec(id: &str) -> book::Rec {
        book::read().rec("extension", id).cloned().expect("in the book")
    }

    fn fetched() -> usize {
        cycle::FETCHED.load(Ordering::SeqCst)
    }

    fn warnings() -> Vec<String> {
        WARNED.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    fn data(row: &Value) -> Value {
        let b = base64::engine::general_purpose::STANDARD.decode(row["data"].as_str().unwrap()).unwrap();
        serde_json::from_slice(&b).unwrap()
    }

    impl Two {
        async fn ext_row(&self, id: &str) -> Value {
            self.on_server("items/extension").await.into_iter().find(|r| r["id"] == id).expect("on the server")
        }

        async fn package_rev(&self, id: &str) -> u64 {
            self.ext_row(id).await["package"]["package_rev"].as_u64().unwrap()
        }

        /// A wallet on A, synced, and on B too.
        async fn wallet_on_both(&self, version: &str) -> String {
            self.be(&self.a);
            let id = wallet(self, version);
            cycle::run_once().await;
            self.join_b().await;
            cycle::run_once().await;
            assert_eq!(here(&id).version, version, "B has it");
            id
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn an_update_waits_for_a_profile_running_with_it_and_is_downloaded_once() {
        let t = Two::new().await;
        let id = t.wallet_on_both("1.0").await;
        let mut s: StoredProfile = serde_json::from_value(json!({ "name": "Shop" })).unwrap();
        s.meta.extensions = vec![id.clone()];
        crate::space::profile::save_raw(&mut s).unwrap();
        let shop = s.meta.id;
        t.launch(&shop).await.unwrap();

        t.be(&t.a);
        bump(&id, "1.1", json!({}));
        cycle::run_once().await;
        assert_eq!(t.package_rev(&id).await, 2);

        t.be(&t.b);
        let (f0, booked) = (fetched(), rec(&id));
        cycle::run_once().await;
        assert_eq!(fetched(), f0 + 1);
        assert_eq!(here(&id).version, "1.0", "not swapped under its running browser");
        assert_eq!(rec(&id), booked, "the book waits");
        let staged = swap::staged(&lib(), &id).expect("staged");
        assert_eq!((staged.origin, staged.to.as_str()), (swap::Origin::Sync(2), "1.1"));
        cycle::run_once().await;
        assert_eq!(fetched(), f0 + 1, "not downloaded a second time");

        let chrome_before = chrome_id(&local::extensions::load_path(&id).unwrap()).unwrap();
        t.stop(&shop).await;
        cycle::run_once().await;
        assert_eq!(fetched(), f0 + 1);
        assert_eq!(here(&id).version, "1.1", "in once it closed");
        assert_eq!(chrome_id(&local::extensions::load_path(&id).unwrap()).unwrap(), chrome_before);
        assert_eq!(rec(&id).package, 2);
        assert_eq!(source::read(&lib(), &id).unwrap().applied_package_rev, Some(2));
        assert_eq!(state().status.pending, 0, "nothing goes back");
    }

    #[tokio::test]
    async fn a_newer_version_here_is_kept_and_goes_up_over_an_older_one() {
        let t = Two::new().await;
        let id = t.wallet_on_both("1.0").await;
        // B updates without the server; A changes its files at the old version.
        t.offline();
        bump(&id, "2.0", json!({}));
        t.be(&t.a);
        fs::write(local::extensions::load_path(&id).unwrap().join("extra.js"), b"// A's").unwrap();
        cycle::run_once().await;
        assert_eq!(t.package_rev(&id).await, 2);

        t.be(&t.b);
        t.online();
        let told = warnings().len();
        cycle::run_once().await;
        assert_eq!(here(&id).version, "2.0", "B keeps its newer one");
        let row = t.ext_row(&id).await;
        assert_eq!((data(&row)["version"].as_str(), row["package"]["package_rev"].as_u64()), (Some("2.0"), Some(3)));
        let said = warnings()[told..].join("\n");
        assert!(said.contains("“Wallet” is newer on this computer (2.0) than on another one (1.0)"), "{said}");

        t.be(&t.a);
        cycle::run_once().await;
        assert_eq!(here(&id).version, "2.0", "and A takes it");
    }

    #[tokio::test]
    async fn a_link_goes_across_as_the_record_alone_and_a_record_without_it_unlinks_nothing() {
        let t = Two::new().await;
        let id = t.wallet_on_both("1.0").await;
        let f0 = fetched();
        t.be(&t.a);
        let mut side = source::read(&lib(), &id).unwrap();
        (side.source, side.how) = (Some(SourceRef::webstore(SID)), Some(How::User));
        source::write(&lib(), &id, &side).unwrap();
        cycle::run_once().await;
        let row = t.ext_row(&id).await;
        assert_eq!(row["package"]["package_rev"], 1, "only the record went up");
        assert_eq!(data(&row)["source"], json!({ "kind": "webstore", "id": SID }));

        t.be(&t.b);
        cycle::run_once().await;
        assert_eq!(fetched(), f0, "B downloads nothing");
        assert_eq!(here(&id).source, Some(SourceRef::webstore(SID)));
        assert_eq!(state().status.pending, 0);

        // An older launcher writes the record back without a source.
        let mut old = data(&row);
        old.as_object_mut().unwrap().remove("source");
        let body = json!({
            "name": "Wallet",
            "data": base64::engine::general_purpose::STANDARD.encode(old.to_string()),
            "base_revision": row["revision"],
        });
        t.script().call(Method::PUT, &format!("/me/items/extension/{id}"), Some(&body)).await.unwrap();
        cycle::run_once().await;
        assert_eq!(here(&id).source, Some(SourceRef::webstore(SID)), "kept on B");
        assert_eq!(fetched(), f0);
    }

    #[tokio::test]
    async fn a_link_made_here_survives_an_update_from_the_other_computer() {
        let t = Two::new().await;
        let id = t.wallet_on_both("1.0").await;
        let mut side = source::read(&lib(), &id).unwrap();
        (side.source, side.how) = (Some(SourceRef::webstore(SID)), Some(How::User));
        source::write(&lib(), &id, &side).unwrap();
        t.be(&t.a);
        bump(&id, "1.1", json!({}));
        cycle::run_once().await;

        t.be(&t.b);
        let told = warnings().len();
        cycle::run_once().await;
        assert_eq!((here(&id).version.as_str(), here(&id).source), ("1.1", Some(SourceRef::webstore(SID))));
        assert_eq!(data(&t.ext_row(&id).await)["source"], json!({ "kind": "webstore", "id": SID }), "the link went up");
        assert_eq!(warnings().len(), told, "nothing of B's was lost");
        assert_eq!(state().status.pending, 0);
        t.be(&t.a);
        cycle::run_once().await;
        assert_eq!(here(&id).source, Some(SourceRef::webstore(SID)), "A has it");
    }

    #[tokio::test]
    async fn meeting_the_server_anew_downloads_only_what_differs() {
        let t = Two::new().await;
        let id = t.wallet_on_both("1.0").await;
        t.be(&t.a);
        bump(&id, "1.1", json!({}));
        cycle::run_once().await;

        // B's book starts over: the same id here, an older version.
        t.be(&t.b);
        let again = || async {
            disconnect(false).await.unwrap();
            connect(Some(t.srv.url.clone()), Some(t.token.clone()), None).await.unwrap();
            cycle::run_once().await;
        };
        let f0 = fetched();
        again().await;
        assert_eq!((fetched(), here(&id).version.as_str()), (f0 + 1, "1.1"));
        assert_eq!(source::read(&lib(), &id).unwrap().applied_package_rev, Some(2));
        again().await;
        assert_eq!(fetched(), f0 + 1, "the same files: nothing downloaded");
        assert_eq!(state().status.pending, 0);
    }

    #[tokio::test]
    async fn a_nested_folder_stays_nested_with_the_same_id() {
        let t = Two::new().await;
        let id = t.wallet_on_both("1.0").await;
        let dir = lib().join(&id);
        fs::create_dir_all(dir.join("wallet")).unwrap();
        for name in ["manifest.json", "background.js"] {
            fs::rename(dir.join(name), dir.join("wallet").join(name)).unwrap();
        }
        let before = chrome_id(&dir.join("wallet")).unwrap();
        cycle::run_once().await;

        t.be(&t.a);
        bump(&id, "1.1", json!({}));
        cycle::run_once().await;
        t.be(&t.b);
        cycle::run_once().await;
        assert_eq!(here(&id).version, "1.1");
        assert!(dir.join("wallet").join("manifest.json").is_file() && !dir.join("manifest.json").exists(), "still nested");
        assert_eq!(chrome_id(&local::extensions::load_path(&id).unwrap()).unwrap(), before);
    }

    #[tokio::test]
    async fn chromes_own_metadata_sends_the_record_alone() {
        let t = Two::new().await;
        let id = t.wallet_on_both("1.0").await;
        let f0 = fetched();
        // Chrome writes its rule index into B's folder, then tidies it away.
        let meta = lib().join(&id).join("_metadata");
        fs::create_dir_all(meta.join("generated_indexed_rulesets")).unwrap();
        fs::write(meta.join("generated_indexed_rulesets").join("_ruleset1"), vec![7u8; 4096]).unwrap();
        let rev = t.ext_row(&id).await["revision"].as_u64().unwrap();
        cycle::run_once().await;
        let row = t.ext_row(&id).await;
        assert!(row["revision"].as_u64().unwrap() > rev, "the record went up");
        assert_eq!(row["package"]["package_rev"], 1, "and not the files");
        fs::remove_dir_all(&meta).unwrap();
        cycle::run_once().await;
        assert_eq!(t.package_rev(&id).await, 1);

        t.be(&t.a);
        cycle::run_once().await;
        assert_eq!(fetched(), f0, "A downloads nothing");
        assert_eq!(state().status.pending, 0);
    }

    #[tokio::test]
    async fn a_book_from_before_files_were_kept_sends_no_package_at_the_same_version() {
        let t = Two::new().await;
        let id = wallet(&t, "1.0");
        cycle::run_once().await;
        assert!(!rec(&id).files.is_empty());
        book::change(|b| b.records.get_mut("extension").unwrap().get_mut(&id).unwrap().files.clear()).unwrap();
        fs::create_dir_all(lib().join(&id).join("_metadata")).unwrap();
        fs::write(lib().join(&id).join("_metadata").join("computed_hashes.json"), b"{}").unwrap();
        cycle::run_once().await;
        assert_eq!(t.package_rev(&id).await, 1, "the same version: no package");
        assert_eq!(rec(&id).files, local::extensions::files_digest(&lib().join(&id)).unwrap(), "now kept");

        bump(&id, "1.1", json!({}));
        cycle::run_once().await;
        assert_eq!(t.package_rev(&id).await, 2, "changed files go");
    }

    #[tokio::test]
    async fn one_that_would_change_its_id_here_is_kept_out_once_and_said_once() {
        let t = Two::new().await;
        let id = t.wallet_on_both("1.0").await;
        t.be(&t.a);
        let key = base64::engine::general_purpose::STANDARD.encode(Key::new().spki);
        bump(&id, "1.1", json!({ "key": key }));
        cycle::run_once().await;

        t.be(&t.b);
        let (f0, told) = (fetched(), warnings().len());
        cycle::run_once().await;
        assert_eq!(fetched(), f0 + 1);
        assert_eq!(here(&id).version, "1.0", "B keeps its own");
        let said = warnings()[told..].to_vec();
        assert_eq!(said, ["“Wallet” from another computer would change its ID here, so the version on this computer is kept."]);
        let book = updates::load(&crate::store::data_root().unwrap().join(updates::BOOK));
        assert_eq!(book.items[&format!("lib:{id}")].status, updates::Status::IdConflict);
        assert_eq!(rec(&id).package, 2, "the record is taken");

        cycle::run_once().await;
        assert_eq!((fetched(), warnings().len()), (f0 + 1, told + 1), "neither fetched nor said again");
        assert_eq!(state().status.pending, 0);
    }

    #[tokio::test]
    async fn one_for_a_newer_engine_is_not_downloaded_again_and_goes_in_with_the_engine() {
        let t = Two::new().await;
        *cycle::ENGINE.lock().unwrap() = Some("120.0.6099.5".into());
        let id = t.wallet_on_both("1.0").await;
        t.be(&t.a);
        bump(&id, "1.1", json!({ "minimum_chrome_version": "130" }));
        cycle::run_once().await;

        t.be(&t.b);
        let (f0, booked) = (fetched(), rec(&id));
        cycle::run_once().await;
        assert_eq!(swap::staged(&lib(), &id).and_then(|s| s.needs_engine).as_deref(), Some("130"));
        cycle::run_once().await;
        assert_eq!((fetched(), here(&id).version.as_str()), (f0 + 1, "1.0"), "fetched once, not in");
        assert_eq!(rec(&id), booked);

        *cycle::ENGINE.lock().unwrap() = Some("131.0.6778.2".into());
        cycle::run_once().await;
        *cycle::ENGINE.lock().unwrap() = None;
        assert_eq!((fetched(), here(&id).version.as_str()), (f0 + 1, "1.1"));
        assert_eq!(rec(&id).package, 2);
    }

    /// B's folder aside for a sync (the fallback's renames, a put back that
    /// failed): neither the server nor A loses it, nor is a delete waiting.
    #[tokio::test]
    async fn a_folder_aside_for_a_sync_is_never_taken_for_deleted() {
        let t = Two::new().await;
        let id = t.wallet_on_both("1.0").await;
        t.be(&t.b);
        let p = swap::Paths::new(&lib(), &id);
        fs::rename(p.live(), p.prev_new()).unwrap();
        cycle::run_once().await;
        cycle::scan();
        assert!(book::outbox().entries.iter().all(|e| e.id != id), "no delete waits: {:?}", book::outbox().entries);
        fs::rename(p.prev_new(), p.live()).unwrap();
        assert!(t.on_server("items/extension").await.iter().any(|r| r["id"] == id.as_str()), "still on the server");
        t.be(&t.a);
        cycle::run_once().await;
        assert_eq!(here(&id).version, "1.0", "still on A");
    }

    /// A manifest Chrome reads (a BOM here) is listed, so synced as it is,
    /// and one that does not read at all is left alone, not deleted.
    #[tokio::test]
    async fn a_manifest_that_reads_otherwise_here_deletes_nothing() {
        let t = Two::new().await;
        let id = t.wallet_on_both("1.0").await;
        t.be(&t.b);
        let root = local::extensions::load_path(&id).unwrap();
        fs::write(root.join("manifest.json"), format!("\u{feff}{}", manifest("1.1", json!({})))).unwrap();
        cycle::run_once().await;
        assert_eq!(data(&t.ext_row(&id).await)["version"], "1.1", "the update goes across");
        t.be(&t.a);
        cycle::run_once().await;
        assert_eq!(here(&id).version, "1.1");

        t.be(&t.b);
        fs::write(root.join("manifest.json"), b"{ half a").unwrap();
        cycle::run_once().await;
        assert_eq!(data(&t.ext_row(&id).await)["version"], "1.1", "left on the server");
        t.be(&t.a);
        cycle::run_once().await;
        assert_eq!(here(&id).version, "1.1", "and on A");
    }
}
