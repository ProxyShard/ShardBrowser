//! Opening and closing team profiles against a real server in-process, with
//! fake browsers: the guarantees table of the plan, row by row.

use super::super::client::{Code, Method, TeamClient};
use super::super::local::cookies;
use super::*;
use crate::team;
use serde_json::Value;
use shardbrowser_team_server::{Action, Grant, NewMember, Scope, TestServer};
use std::time::Instant;
use super::super::reap::tests::{reaper_of, spawn_reapers};

static OUTCOMES: Mutex<Vec<(String, Outcome)>> = Mutex::new(Vec::new());

pub(super) fn record(id: &str, outcome: Outcome) {
    OUTCOMES.lock().unwrap_or_else(|e| e.into_inner()).push((id.to_string(), outcome));
}

/// How each close of this profile ended, in order.
fn outcomes(id: &str) -> Vec<Outcome> {
    let all = OUTCOMES.lock().unwrap_or_else(|e| e.into_inner());
    all.iter().filter(|(i, _)| i == id).map(|(_, o)| o.clone()).collect()
}

fn on(scope: Scope, action: Action) -> Grant {
    Grant { scope, action, allow: true }
}

fn team_wide(actions: &[Action]) -> Vec<Grant> {
    actions.iter().map(|a| on(Scope::Team, *a)).collect()
}

fn everything() -> Vec<Grant> {
    team_wide(&[Action::See, Action::Start, Action::Edit, Action::Create, Action::Delete, Action::Cookies])
}

fn b64(bytes: &[u8]) -> String {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

/// Wait for `check`, failing past `within`.
async fn until(what: &str, within: Duration, mut check: impl FnMut() -> bool) -> Duration {
    let start = Instant::now();
    while !check() {
        assert!(start.elapsed() < within, "{what}: not within {within:?}");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    start.elapsed()
}

#[cfg(unix)]
fn alive(pid: u32) -> bool {
    // SAFETY: signal 0 delivers nothing; it only asks whether the pid is there.
    unsafe { libc::kill(pid as libc::pid_t, 0) == 0 }
}

/// A process nobody here is the parent of, as a browser a dead launcher left.
#[cfg(unix)]
fn stray_sleep() -> u32 {
    let out = std::process::Command::new("sh")
        .args(["-c", "set -m; sleep 60 </dev/null >/dev/null 2>&1 & echo $!"])
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout).trim().parse().unwrap()
}

/// A team server in-process, Vera joined from here, and a fake browser engine.
struct Team {
    // Stopped before its data goes with the root.
    srv: TestServer,
    root: crate::store::TestRoot,
    team_id: String,
    vera: NewMember,
    engine: PathBuf,
    _serial: std::sync::MutexGuard<'static, ()>,
}

impl Team {
    async fn new(grants: &[Grant]) -> Team {
        let serial = crate::migrate::serial();
        let root = crate::store::test_root("team-open").with_config();
        let srv = TestServer::start(root.path.join("server")).await.unwrap();
        let vera = srv.add_member("Vera", grants).unwrap();
        team::join(&srv.url, &vera.token).await.unwrap();
        let team_id = srv.server.team_id().to_string();
        cache::keep_only(None);
        let engine = root.path.join("engine.sh");
        // A browser as the tracker sees one: its lock names it, and SIGINT ends it.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let host = crate::reset::this_host().unwrap();
            let started = root.path.join("started");
            // Its command line is written before it counts as started. With
            // debugging asked for, it names a port nothing listens on.
            let script = format!(
                "#!/bin/sh\nfor a in \"$@\"; do case \"$a\" in --user-data-dir=*) udd=\"${{a#--user-data-dir=}}\";; \
                 --remote-debugging-port=*) cdp=1;; esac; done\n\
                 printf '%s\\n' \"$*\" >> '{}'\necho \"$udd\" >> '{}'\n\
                 [ -n \"$cdp\" ] && printf '9\\n/devtools/browser/x\\n' > \"$udd/DevToolsActivePort\"\n\
                 ln -s '{host}-'$$ \"$udd/SingletonLock\"\nexec sleep 600\n",
                root.path.join("argv").display(),
                started.display()
            );
            fs::write(&engine, script).unwrap();
            fs::set_permissions(&engine, fs::Permissions::from_mode(0o755)).unwrap();
        }
        Team { srv, root, team_id, vera, engine, _serial: serial }
    }

    fn admin(&self) -> TeamClient {
        TeamClient::new(&self.srv.url, &self.srv.admin_token, "admin-device").unwrap()
    }

    /// Vera on her other computer.
    fn pc(&self) -> TeamClient {
        TeamClient::new(&self.srv.url, &self.vera.token, "vera-pc").unwrap()
    }

    /// A new team profile in `folder`, with `card`; its id.
    async fn profile_with(&self, folder: &str, card: Value) -> String {
        let id = format!("p-{}", uuid::Uuid::new_v4().simple());
        let body = json!({ "name": format!("Shop {}", &id[2..8]), "folder": folder, "data": b64(b"{}"), "card": card });
        self.admin().call(Method::PUT, &format!("/profiles/{id}"), Some(&body)).await.unwrap();
        id
    }

    async fn profile(&self) -> String {
        self.profile_with("Ads", json!({})).await
    }

    /// The team's lists read again, as a switch reads them.
    async fn read(&self) {
        cache::reload(&self.team_id, super::super::live::default_tell()).await;
    }

    /// Started, and up: the fake browser has its lock, as a real one would.
    async fn launch(&self, id: &str) -> Result<crate::launch::LaunchOutcome> {
        let before = self.started().len();
        let out = crate::launch::launch_with(self.engine.clone(), id, false).await?;
        until("the browser up", Duration::from_secs(30), || self.started().len() > before).await;
        Ok(out)
    }

    fn copy(&self, id: &str) -> PathBuf {
        self.root.path.join("team-open").join(&self.team_id).join(id)
    }

    fn udd(&self, id: &str) -> PathBuf {
        self.copy(id).join("udd")
    }

    /// The folders the fake browser was started on.
    fn started(&self) -> Vec<String> {
        fs::read_to_string(self.root.path.join("started")).unwrap_or_default().lines().map(String::from).collect()
    }

    /// Each start's command line, as one line.
    fn argv(&self) -> Vec<String> {
        fs::read_to_string(self.root.path.join("argv")).unwrap_or_default().lines().map(String::from).collect()
    }

    async fn row(&self, id: &str) -> Value {
        let (_, body) = self.admin().call(Method::GET, &format!("/profiles/{id}"), None).await.unwrap();
        serde_json::from_slice(&body).unwrap()
    }

    async fn session_rev(&self, id: &str) -> u64 {
        self.row(id).await["session_rev"].as_u64().unwrap()
    }

    fn is_open(&self, id: &str) -> bool {
        find(&Home::team(&self.team_id), id).is_some()
    }

    /// Until its close has run to the end.
    async fn closed(&self, id: &str) {
        until("the close", Duration::from_secs(20), || !self.is_open(id)).await;
    }

    async fn stop(&self, id: &str) {
        assert!(crate::process::Tracker::shared().kill(id).await.unwrap(), "nothing to stop");
        self.closed(id).await;
    }

    fn nudge(&self, id: &str) {
        find(&Home::team(&self.team_id), id).unwrap().nudge.notify_one();
    }

    /// As if this device slept past its lease.
    fn lease_runs_out(&self, id: &str) {
        let db = rusqlite::Connection::open(self.srv.server.db_path()).unwrap();
        db.busy_timeout(Duration::from_secs(5)).unwrap();
        db.execute("UPDATE leases SET expires_at = 1 WHERE item_id = ?1", [id]).unwrap();
    }

    fn lease_path(id: &str) -> String {
        format!("/profiles/{id}/lease")
    }

    async fn set_grants(&self, grants: &[Grant]) {
        let path = format!("/admin/members/{}/grants", self.vera.id);
        self.admin().call(Method::PUT, &path, Some(&json!({ "grants": grants }))).await.unwrap();
    }

    fn follow(&self) {
        super::super::live::follow_with(&self.team_id, Arc::new(|_: &str, _| {}));
    }

    fn browser(&self, id: &str) -> u32 {
        crate::process::Tracker::shared().running().into_iter().find(|r| r.profile_id == id).unwrap().pid
    }

    fn assert_nothing_here(&self, id: &str) {
        assert!(!self.copy(id).exists(), "{} is still there", self.copy(id).display());
        assert!(!self.is_open(id));
        assert!(!crate::process::Tracker::shared().is_running(id));
    }
}

fn a_cookie(domain: &str, name: &str, value: &str) -> cookies::Cookie {
    serde_json::from_value(json!({ "domain": domain, "name": name, "value": value, "expires": 4_000_000_000.0 })).unwrap()
}

#[cfg(unix)]
#[tokio::test]
async fn a_closed_profile_is_sent_back_and_nothing_of_it_stays() {
    let t = Team::new(&everything()).await;
    let id = t.profile().await;
    t.read().await;
    t.launch(&id).await.unwrap();
    let udd = t.udd(&id);
    assert_eq!(t.started(), [udd.display().to_string()], "started on the copy in team-open");
    assert!(!crate::store::user_data_root().unwrap().join(&id).exists(), "nothing under user-data");
    assert_eq!(t.row(&id).await["open_on"]["device_id"], team::device_id());
    // Marked with what it was made from only once the copy is whole.
    let m: Marker = serde_json::from_slice(&fs::read(t.copy(&id).join(MARKER)).unwrap()).unwrap();
    assert_eq!((m.id.as_str(), m.base), (id.as_str(), Some(0)));
    assert_eq!(m.browser.map(|b| b.pid), Some(t.browser(&id)), "its browser named for a reaper");

    // The browser keeps a site's storage and a cookie.
    let store = udd.join("Default/Local Storage/leveldb");
    fs::create_dir_all(&store).unwrap();
    fs::write(store.join("000003.log"), b"remembered").unwrap();
    fs::create_dir_all(udd.join("Default/Cache")).unwrap();
    fs::write(udd.join("Default/Cache/data_1"), b"cache").unwrap();
    cookies::import(&udd, &[a_cookie(".shop.example", "sid", "s3cret")]).unwrap();

    t.stop(&id).await;
    assert_eq!(outcomes(&id), [Outcome::Saved(1)]);
    t.assert_nothing_here(&id);
    assert!(!t.root.path.join("team-open").exists(), "team-open itself goes once empty");
    let row = t.row(&id).await;
    assert_eq!((row["session_rev"].as_u64(), row["open_on"].is_null()), (Some(1), true));

    // Opened again, it has what it had; caches stayed behind.
    t.launch(&id).await.unwrap();
    assert_eq!(fs::read(store.join("000003.log")).unwrap(), b"remembered");
    assert!(!udd.join("Default/Cache").exists());
    let jar = cookies::export(&udd).unwrap();
    assert_eq!(jar.iter().map(|c| (c.name.as_str(), c.value.as_str())).collect::<Vec<_>>(), [("sid", "s3cret")]);
    t.stop(&id).await;
    assert_eq!(outcomes(&id), [Outcome::Saved(1), Outcome::Saved(2)]);
    t.assert_nothing_here(&id);
}

#[cfg(unix)]
#[tokio::test]
async fn a_second_device_is_told_where_the_profile_is_open() {
    let t = Team::new(&everything()).await;
    let id = t.profile().await;
    t.read().await;
    t.launch(&id).await.unwrap();

    let err = t.pc().call(Method::POST, &Team::lease_path(&id), None).await.unwrap_err();
    assert_eq!(err.code(), Some(&Code::Leased));
    assert_eq!(err.to_string(), format!("open on {}", team::device_name()));
    // Its settings cannot be saved from there either.
    let body = json!({ "name": "x", "folder": "Ads", "data": "", "base_revision": 1 });
    let err = t.pc().call(Method::PUT, &format!("/profiles/{id}"), Some(&body)).await.unwrap_err();
    assert_eq!(err.code(), Some(&Code::Leased));
    t.stop(&id).await;

    // The other way round: open on the PC, refused here before anything is made.
    t.pc().call(Method::POST, &Team::lease_path(&id), None).await.unwrap();
    let err = t.launch(&id).await.err().unwrap();
    assert_eq!(err.to_string(), format!("this profile is open on {}: close it there first", team::device_name()));
    assert_eq!(super::super::error::find(&err).map(|e| e.code()), Some("leased"));
    t.closed(&id).await;
    t.assert_nothing_here(&id);
    assert_eq!(t.started().len(), 1, "no second browser");
    assert_eq!(t.row(&id).await["open_on"]["device_id"], "vera-pc", "the PC keeps it");
}

#[cfg(unix)]
#[tokio::test]
async fn saving_its_settings_while_it_is_open_does_not_stale_its_session() {
    let t = Team::new(&everything()).await;
    let id = t.profile().await;
    t.read().await;
    t.launch(&id).await.unwrap();
    let tk = super::super::Ticket::now();
    let mut stored = super::super::profile::fetch_for_edit(&tk, &id).await.unwrap();
    stored.config.insert("notes".into(), json!("edited while open"));
    let door = super::super::Door("profile_save");
    super::super::profile::save(door, &tk, stored, Default::default()).await.unwrap();
    let row = t.row(&id).await;
    assert_eq!((row["revision"].as_u64(), row["session_rev"].as_u64()), (Some(2), Some(0)));
    t.stop(&id).await;
    assert_eq!(outcomes(&id), [Outcome::Saved(1)]);
    assert_eq!(t.row(&id).await["card"]["notes"], "edited while open");
}

#[cfg(unix)]
#[tokio::test]
async fn a_laptop_that_wakes_up_late_keeps_the_newer_work_and_drops_its_own() {
    let t = Team::new(&everything()).await;
    // Free when it wakes: the lease is taken again, and the newer session wins.
    let id = t.profile().await;
    t.read().await;
    t.launch(&id).await.unwrap();
    t.lease_runs_out(&id);
    let pc = t.pc();
    pc.call(Method::POST, &Team::lease_path(&id), None).await.unwrap();
    let zip = t.root.path.join("from-the-pc.zip");
    fs::write(&zip, b"the PC's session").unwrap();
    let saved = pc.upload(&format!("/profiles/{id}/session?base_session_rev=0"), &zip).await.unwrap();
    assert_eq!(saved.session_rev, 1);
    pc.call(Method::DELETE, &Team::lease_path(&id), None).await.unwrap();
    t.nudge(&id);
    let here = team::device_id();
    let mut taken = false;
    for _ in 0..200 {
        if t.row(&id).await["open_on"]["device_id"] == here.as_str() {
            taken = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(taken, "the lease was not taken again");
    t.stop(&id).await;
    assert_eq!(outcomes(&id), [Outcome::Stale]);
    assert_eq!(t.session_rev(&id).await, 1, "the PC's work stays");
    t.assert_nothing_here(&id);
    assert!(t.row(&id).await["open_on"].is_null());

    // Still held by the PC when it wakes: closed unsent at once.
    let id = t.profile().await;
    t.read().await;
    t.launch(&id).await.unwrap();
    let pid = t.browser(&id);
    t.lease_runs_out(&id);
    pc.call(Method::POST, &Team::lease_path(&id), None).await.unwrap();
    t.nudge(&id);
    t.closed(&id).await;
    assert_eq!(outcomes(&id), [Outcome::Discarded]);
    assert!(!alive(pid), "the browser is closed");
    t.assert_nothing_here(&id);
    let row = t.row(&id).await;
    assert_eq!((row["session_rev"].as_u64(), row["open_on"]["device_id"].as_str()), (Some(0), Some("vera-pc")));
}

#[cfg(unix)]
#[tokio::test]
async fn quitting_sends_the_open_team_profiles_back_and_wipes_them_first() {
    let t = Team::new(&everything()).await;
    let ids = [t.profile().await, t.profile().await];
    t.read().await;
    let mut pids = Vec::new();
    for id in &ids {
        t.launch(id).await.unwrap();
        pids.push(t.browser(id));
    }
    assert_eq!(listed().len(), 2);
    assert!(listed().iter().all(|l| !l.mine && l.team_id == t.team_id));
    close_all(Duration::from_secs(20)).await;
    for (id, pid) in ids.iter().zip(pids) {
        assert!(!alive(pid));
        t.assert_nothing_here(id);
        assert_eq!(outcomes(id), [Outcome::Saved(1)]);
        assert_eq!(t.session_rev(id).await, 1);
    }
    assert!(!any_open());
}

/// A copy a dead launcher left: marker, browser data, and a browser still up.
#[cfg(unix)]
fn left_behind(t: &Team, id: &str, base: Option<u64>) -> u32 {
    let dir = t.copy(id);
    let store = dir.join("udd/Default/Local Storage/leveldb");
    fs::create_dir_all(&store).unwrap();
    fs::write(store.join("000003.log"), id.as_bytes()).unwrap();
    let m = Marker { home: Home::team(&t.team_id), id: id.to_string(), name: id.to_string(), base, nonce: format!("n-{id}"), own: None, browser: None };
    fs::write(dir.join(MARKER), serde_json::to_vec(&m).unwrap()).unwrap();
    let pid = stray_sleep();
    let host = crate::reset::this_host().unwrap();
    std::os::unix::fs::symlink(format!("{host}-{pid}"), dir.join("udd/SingletonLock")).unwrap();
    pid
}

#[cfg(unix)]
#[tokio::test]
async fn the_sweep_closes_and_wipes_what_a_dead_launcher_left() {
    let t = Team::new(&everything()).await;
    let me = TeamClient::for_team(&t.team_id).unwrap();
    // Nobody saved since: sent back. Held when the launcher died, as it would be.
    let kept = t.profile().await;
    me.call(Method::POST, &Team::lease_path(&kept), None).await.unwrap();
    // Saved from the PC meanwhile: not sent over it.
    let newer = t.profile().await;
    let pc = t.pc();
    pc.call(Method::POST, &Team::lease_path(&newer), None).await.unwrap();
    let zip = t.root.path.join("pc.zip");
    fs::write(&zip, b"newer").unwrap();
    pc.upload(&format!("/profiles/{newer}/session?base_session_rev=0"), &zip).await.unwrap();
    pc.call(Method::DELETE, &Team::lease_path(&newer), None).await.unwrap();
    // Died mid-download: nothing to send.
    let half = t.profile().await;

    let pids = [left_behind(&t, &kept, Some(0)), left_behind(&t, &newer, Some(0)), left_behind(&t, &half, None)];
    let junk = t.root.path.join("team-open").join(&t.team_id).join("no-marker");
    fs::create_dir_all(junk.join("udd")).unwrap();

    sweep();
    for id in [&kept, &newer, &half] {
        t.closed(id).await;
        t.assert_nothing_here(id);
    }
    for pid in pids {
        until("the browser closed", Duration::from_secs(10), || !alive(pid)).await;
    }
    assert!(!t.root.path.join("team-open").exists(), "everything there went");
    assert_eq!(outcomes(&kept), [Outcome::Saved(1)]);
    assert_eq!(outcomes(&newer), [Outcome::Stale]);
    assert_eq!(outcomes(&half), [Outcome::Discarded]);
    assert_eq!((t.session_rev(&kept).await, t.session_rev(&newer).await), (1, 1));
    for id in [&kept, &newer, &half] {
        assert!(t.row(id).await["open_on"].is_null(), "the lease of {id} went back");
    }
}

#[cfg(unix)]
#[tokio::test]
async fn taking_start_back_closes_the_profile_within_two_seconds_and_sends_it_back() {
    let t = Team::new(&team_wide(&[Action::See, Action::Start])).await;
    let id = t.profile().await;
    t.read().await;
    t.follow();
    until("the permission", Duration::from_secs(5), || perm::held_epoch(&t.team_id).is_some()).await;
    t.launch(&id).await.unwrap();
    let pid = t.browser(&id);

    t.set_grants(&team_wide(&[Action::See])).await;
    let took = until("the browser closed", Duration::from_secs(2), || !alive(pid)).await;
    assert!(took < Duration::from_secs(2), "{took:?}");
    t.closed(&id).await;
    assert_eq!(outcomes(&id), [Outcome::Saved(1)], "she worked on it rightfully");
    t.assert_nothing_here(&id);
}

#[cfg(unix)]
#[tokio::test]
async fn a_removed_member_loses_the_open_copy_unsent_and_a_plain_refusal_closes_nothing() {
    let t = Team::new(&team_wide(&[Action::See, Action::Start])).await;
    let id = t.profile().await;
    t.read().await;
    t.follow();
    until("the permission", Duration::from_secs(5), || perm::held_epoch(&t.team_id).is_some()).await;
    t.launch(&id).await.unwrap();
    let pid = t.browser(&id);

    // No Create: refused, and that is all it is.
    let me = TeamClient::for_team(&t.team_id).unwrap();
    let body = json!({ "name": "New", "data": "" });
    let err = me.call(Method::PUT, "/profiles/p-new", Some(&body)).await.unwrap_err();
    assert_eq!(err.code(), Some(&Code::Forbidden));
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(alive(pid) && t.is_open(&id) && t.udd(&id).is_dir());

    t.admin().call(Method::DELETE, &format!("/admin/members/{}", t.vera.id), None).await.unwrap();
    until("the copy wiped", Duration::from_secs(5), || !t.copy(&id).exists()).await;
    t.closed(&id).await;
    assert!(!alive(pid));
    assert_eq!(outcomes(&id), [Outcome::Discarded]);
    assert_eq!(t.session_rev(&id).await, 0, "nothing sent");
    assert!(team::link(&t.team_id).unwrap().unwrap().revoked);
}

#[cfg(unix)]
#[tokio::test]
async fn an_open_profile_closes_when_the_permission_runs_out() {
    let t = Team::new(&everything()).await;
    let id = t.profile().await;
    t.read().await;
    t.launch(&id).await.unwrap();
    let pid = t.browser(&id);
    let window = perm::time_left(&t.team_id).unwrap();

    // Asleep most of the window: told it is about to close, still open.
    perm::slept(&t.team_id, window - Duration::from_secs(60));
    t.nudge(&id);
    let s = find(&Home::team(&t.team_id), &id).unwrap();
    until("the warning", Duration::from_secs(5), || s.state().warned).await;
    assert!(alive(pid) && s.state().end.is_none());

    // Past it: closed, sent back while the server still answers, wiped.
    perm::slept(&t.team_id, Duration::from_secs(120));
    t.nudge(&id);
    drop(s);
    t.closed(&id).await;
    assert!(!alive(pid));
    assert_eq!(outcomes(&id), [Outcome::Saved(1)]);
    t.assert_nothing_here(&id);
}

/// Just after the launcher started, before its live channel brought the
/// team's permission and its lists were read: a start gets both rather than being refused.
#[cfg(unix)]
#[tokio::test]
async fn a_start_right_after_the_launcher_started_waits_for_the_team() {
    let t = Team::new(&everything()).await;
    let id = t.profile().await;
    perm::forget(&t.team_id);
    cache::keep_only(None);
    t.launch(&id).await.unwrap();
    assert!(perm::held_epoch(&t.team_id).is_some());
    t.stop(&id).await;
    assert_eq!(outcomes(&id), [Outcome::Saved(1)]);
}

#[cfg(unix)]
#[tokio::test]
async fn with_the_server_gone_a_team_profile_does_not_start() {
    let t = Team::new(&everything()).await;
    let id = t.profile().await;
    t.read().await;
    team::set_server(&t.team_id, "http://127.0.0.1:1");
    let err = t.launch(&id).await.err().unwrap();
    assert_eq!(super::super::error::find(&err).map(|e| e.code()), Some("offline"), "{err:#}");
    t.assert_nothing_here(&id);
    assert!(t.started().is_empty());
}

#[cfg(unix)]
#[tokio::test]
async fn the_bound_proxy_comes_with_the_session_and_one_that_is_gone_stops_the_start() {
    // Start in the folder and nothing on the proxy: its login comes only with the session.
    let ads = |action| on(Scope::Folder { name: "Ads".into() }, action);
    let t = Team::new(&[ads(Action::See), ads(Action::Start)]).await;
    let login = json!({ "name": "DE", "kind": "http", "host": "203.0.113.9", "port": 3128, "username": "u", "password": "p" });
    let proxy = json!({ "name": "DE", "kind": "http", "country": "DE", "data": b64(login.to_string().as_bytes()) });
    t.admin().call(Method::PUT, "/proxies/x1", Some(&proxy)).await.unwrap();
    let id = t.profile_with("Ads", json!({ "proxy_id": "x1" })).await;
    t.read().await;
    assert!(super::super::proxy::get("x1").unwrap().is_none_or(|p| p.host.is_empty()), "not hers to see");

    let (start, _) = passes(&t.team_id, &id, false).await.unwrap();
    let opened = open_team(&t.team_id, &start, false).await.unwrap();
    let p = opened.proxy.as_ref().unwrap();
    assert_eq!((p.host.as_str(), p.port, p.username.as_str()), ("203.0.113.9", 3128, "u"));
    // A start that stops short gives it all back.
    drop(opened);
    t.closed(&id).await;
    t.assert_nothing_here(&id);
    assert_eq!(outcomes(&id), [Outcome::Discarded]);
    assert!(t.row(&id).await["open_on"].is_null());

    t.admin().call(Method::DELETE, "/proxies/x1?base_revision=1", None).await.unwrap();
    let err = t.launch(&id).await.err().unwrap().to_string();
    assert!(err.contains("no longer exists (x1)"), "{err}");
    t.closed(&id).await;
    t.assert_nothing_here(&id);
    assert!(t.started().is_empty(), "the browser was started");
    assert!(t.row(&id).await["open_on"].is_null());
}

/// The most this process has held at once, in bytes.
#[cfg(unix)]
fn peak_rss() -> u64 {
    // SAFETY: getrusage fills the struct it is given.
    let mut u: libc::rusage = unsafe { std::mem::zeroed() };
    unsafe { libc::getrusage(libc::RUSAGE_SELF, &mut u) };
    let n = u.ru_maxrss as u64;
    if cfg!(target_os = "macos") {
        n
    } else {
        n * 1024
    }
}

fn same_file(a: &Path, b: &Path) -> bool {
    use std::io::Read;
    let (mut a, mut b) = (fs::File::open(a).unwrap(), fs::File::open(b).unwrap());
    let (mut x, mut y) = (vec![0u8; 1 << 20], vec![0u8; 1 << 20]);
    loop {
        let n = a.read(&mut x).unwrap();
        let mut m = 0;
        while m < n {
            let k = b.read(&mut y[m..n]).unwrap();
            if k == 0 {
                return false;
            }
            m += k;
        }
        if x[..n] != y[..n] {
            return false;
        }
        if n == 0 {
            return b.read(&mut y).unwrap() == 0;
        }
    }
}

#[cfg(unix)]
#[tokio::test]
async fn a_200_mb_session_goes_and_comes_back_without_being_held_in_memory() {
    use std::io::Write;
    let t = Team::new(&everything()).await;
    let id = t.profile().await;
    let src = t.root.path.join("big/udd");
    let blob = src.join("Default/IndexedDB/https_big.example_0.indexeddb.blob/1/00/1");
    fs::create_dir_all(blob.parent().unwrap()).unwrap();
    {
        // 200 MB that does not compress, written a megabyte at a time.
        let mut f = std::io::BufWriter::new(fs::File::create(&blob).unwrap());
        let mut block = vec![0u8; 1 << 20];
        let mut x: u64 = 0x9E37_79B9_7F4A_7C15;
        for _ in 0..200 {
            for b in block.chunks_mut(8) {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                b.copy_from_slice(&x.to_le_bytes()[..b.len()]);
            }
            f.write_all(&block).unwrap();
        }
    }
    fs::write(src.join("Local State"), b"{}").unwrap();
    let before = peak_rss();

    let (out, src2) = (t.root.path.join("big/session.zip"), src.clone());
    let packed = tokio::task::spawn_blocking(move || pack::pack(&src2, &out, BUDGET)).await.unwrap().unwrap();
    assert!(packed.size > 200 << 20, "{}", packed.size);
    let me = TeamClient::for_team(&t.team_id).unwrap();
    me.call(Method::POST, &Team::lease_path(&id), None).await.unwrap();
    let path = format!("/profiles/{id}/session");
    let up = t.root.path.join("big/session.zip");
    assert_eq!(me.upload(&format!("{path}?base_session_rev=0"), &up).await.unwrap().session_rev, 1);
    fs::remove_file(&up).unwrap();
    let down = t.root.path.join("big/back.zip");
    let front = me.download(&path, &down).await.unwrap();
    assert_eq!((front.session_rev, front.size), (1, packed.size));
    let dst = t.root.path.join("big/udd2");
    let (down2, dst2) = (down.clone(), dst.clone());
    tokio::task::spawn_blocking(move || pack::unpack(&down2, &dst2)).await.unwrap().unwrap();

    let grew = peak_rss().saturating_sub(before);
    assert!(same_file(&blob, &dst.join("Default/IndexedDB/https_big.example_0.indexeddb.blob/1/00/1")));
    assert!(grew < 100 << 20, "the round trip held {} MB at once", grew >> 20);
}

#[cfg(unix)]
#[tokio::test]
async fn debugging_a_team_profile_needs_its_cookies_right() {
    let t = Team::new(&team_wide(&[Action::See, Action::Start])).await;
    let id = t.profile().await;
    t.read().await;
    let err = crate::launch::launch_with(t.engine.clone(), &id, true).await.err().unwrap();
    assert_eq!(err.to_string(), "not allowed in this team: needs cookies on the profile");
    t.assert_nothing_here(&id);
    assert!(t.row(&id).await["open_on"].is_null(), "no lease was taken");
    assert!(t.started().is_empty());
}

#[cfg(unix)]
#[tokio::test]
async fn a_replaced_token_keeps_the_copy_until_the_new_invite_sends_it_back() {
    let t = Team::new(&everything()).await;
    let id = t.profile().await;
    t.read().await;
    t.follow();
    until("the permission", Duration::from_secs(5), || perm::held_epoch(&t.team_id).is_some()).await;
    t.launch(&id).await.unwrap();
    let pid = t.browser(&id);
    let store = t.udd(&id).join("Default/Local Storage/leveldb");
    fs::create_dir_all(&store).unwrap();
    fs::write(store.join("000003.log"), b"worked on").unwrap();

    let path = format!("/admin/members/{}/token", t.vera.id);
    let (_, body) = t.admin().call(Method::POST, &path, None).await.unwrap();
    let token = serde_json::from_slice::<Value>(&body).unwrap()["token"].as_str().unwrap().to_string();
    t.closed(&id).await;
    assert!(!alive(pid));
    assert_eq!(outcomes(&id), [Outcome::Kept]);
    assert!(t.copy(&id).join(MARKER).is_file(), "kept for the new invite");

    // The launcher quits or dies before the invite comes: its reaper, then the
    // next start's sweep, leave the copy for the invite.
    let nonce = nonce_at(&t.copy(&id)).unwrap();
    assert!(reap(t.copy(&id), &nonce).await);
    assert!(retry_left());
    sweep();
    until("the sweep done", Duration::from_secs(10), || outcomes(&id).len() == 3).await;
    t.closed(&id).await;
    assert_eq!(outcomes(&id), [Outcome::Kept, Outcome::Kept, Outcome::Kept]);
    assert_eq!(fs::read(store.join("000003.log")).unwrap(), b"worked on", "kept whole");
    assert!(t.copy(&id).join(MARKER).is_file());
    assert_eq!(t.session_rev(&id).await, 0, "nothing sent without the invite");

    // The new invite: the same member again, and what she did goes back.
    super::super::join(&t.srv.url, &token).await.unwrap();
    until("sent back", Duration::from_secs(10), || outcomes(&id).len() == 4).await;
    assert_eq!(outcomes(&id), [Outcome::Kept, Outcome::Kept, Outcome::Kept, Outcome::Saved(1)]);
    t.closed(&id).await;
    t.assert_nothing_here(&id);
    assert!(t.row(&id).await["open_on"].is_null());
}

#[cfg(unix)]
#[tokio::test]
async fn delete_all_data_and_a_data_move_wait_for_an_open_team_profile_and_the_reset_forgets_the_teams() {
    let t = Team::new(&everything()).await;
    let id = t.profile().await;
    t.read().await;
    t.launch(&id).await.unwrap();

    // Refused while its copy is here, before anything is read or moved.
    assert_eq!(crate::reset::app_reset_plan().await.unwrap_err(), STILL_OPEN);
    assert_eq!(crate::reset::app_reset("2026-10-08-153000.0".into()).await.unwrap_err(), STILL_OPEN);
    let elsewhere = t.root.path.join("moved");
    let err = crate::migrate::run_with(&elsewhere, &|_| {}).unwrap_err();
    assert_eq!(err.to_string(), STILL_OPEN);
    assert!(!crate::migrate::in_progress(), "the lock went back");
    assert!(!elsewhere.exists());
    assert!(t.is_open(&id) && t.udd(&id).is_dir());
    t.stop(&id).await;
    assert_eq!(outcomes(&id), [Outcome::Saved(1)]);

    // What the reset does once its moves are done: the teams go, and a copy left
    // for a new invite too; none of it is in the backup.
    let kept = t.copy("p-kept");
    fs::create_dir_all(kept.join("udd")).unwrap();
    let m = Marker { home: Home::team(&t.team_id), id: "p-kept".into(), name: String::new(), base: Some(0), nonce: String::new(), own: None, browser: None };
    fs::write(kept.join(MARKER), serde_json::to_vec(&m).unwrap()).unwrap();
    super::super::forget_teams().unwrap();
    assert!(!crate::store::local_root().unwrap().join("teams.json").exists(), "the token went");
    assert!(!team::has_token(&t.team_id));
    assert!(!t.root.path.join("team-open").exists(), "nothing of the team stays");
    assert!(perm::held_epoch(&t.team_id).is_none());
    assert_eq!(team::active_team(), None);
    assert!(team::state().unwrap().teams.is_empty());
}

#[cfg(unix)]
#[tokio::test]
async fn a_team_profiles_debugging_address_shows_only_with_cookies_and_only_in_its_space() {
    let t = Team::new(&everything()).await;
    let id = t.profile().await;
    t.read().await;
    t.follow();
    until("the permission", Duration::from_secs(5), || perm::held_epoch(&t.team_id).is_some()).await;
    t.launch(&id).await.unwrap();
    let ws = "ws://127.0.0.1:9/devtools/browser/x";
    let info = crate::process::CdpInfo { port: 9, http_url: "http://127.0.0.1:9".into(), web_socket_debugger_url: ws.into() };
    crate::process::Tracker::shared().set_cdp(&id, info);
    let shown = || super::super::running().into_iter().find(|r| r.profile_id == id).map(|r| r.cdp.is_some());
    assert_eq!(shown(), Some(true));
    assert_eq!(super::super::cdp_pass(&id).map(|c| c.web_socket_debugger_url).as_deref(), Ok(ws));

    // Cookies taken back, Start kept: still open (started without debugging), the address hidden.
    let before = perm::held_epoch(&t.team_id);
    t.set_grants(&team_wide(&[Action::See, Action::Start])).await;
    until("the new permission", Duration::from_secs(5), || perm::held_epoch(&t.team_id) != before).await;
    assert_eq!(shown(), Some(false));
    assert_eq!(super::super::cdp_pass(&id).unwrap_err(), "not allowed in this team: needs cookies on the profile");
    assert!(t.is_open(&id));

    // In the personal space the team's browser is neither listed nor reachable.
    team::set_space(team::Space::Personal).unwrap();
    assert_eq!(shown(), None);
    assert!(super::super::cdp_pass(&id).unwrap_err().starts_with("no profile"));
    t.stop(&id).await;
    t.assert_nothing_here(&id);
}

// ---- the reaper ----

/// What a test launcher of its own process is to open, as JSON.
const LAUNCHER: &str = "SHARDX_TEST_LAUNCHER";

/// A launcher in a process of its own, to be killed: it opens one team profile
/// with its reaper and a fake browser, says so, and waits.
#[test]
#[ignore = "started by a_killed_launcher_leaves_no_browser_and_no_copy_behind"]
fn as_launcher() {
    let Ok(job) = std::env::var(LAUNCHER) else { return };
    let job: Value = serde_json::from_str(&job).unwrap();
    let s = |k: &str| job[k].as_str().unwrap().to_string();
    let root = PathBuf::from(s("root"));
    crate::store::set_test_config(Some(s("config").into()));
    crate::store::set_data_root(Some(root.clone()));
    spawn_reapers(true);
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    rt.block_on(async {
        let (team, id) = (s("team"), s("id"));
        cache::reload(&team, super::super::live::default_tell()).await;
        crate::launch::launch_with(PathBuf::from(s("engine")), &id, false).await.unwrap();
        let copy = root.join("team-open").join(&team).join(&id);
        until("the browser up", Duration::from_secs(30), || crate::reset::lock_is_live(&copy.join("udd"))).await;
        let reaper = reaper_of(&copy).expect("a reaper for the copy");
        let browser = crate::process::Tracker::shared().running().into_iter().find(|r| r.profile_id == id).unwrap().pid;
        println!("READY {reaper} {browser}");
        std::future::pending::<()>().await;
    });
}

#[cfg(unix)]
#[tokio::test]
async fn a_killed_launcher_leaves_no_browser_and_no_copy_behind() {
    use tokio::io::AsyncBufReadExt;
    let t = Team::new(&everything()).await;
    let id = t.profile().await;
    let job = json!({
        "config": crate::store::test_config().unwrap(), "root": t.root.path, "team": t.team_id,
        "id": id, "engine": t.engine,
    });
    let mut launcher = tokio::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "--ignored", "--nocapture", "--test-threads=1", "-q", "space::open::tests::as_launcher"])
        .env(LAUNCHER, job.to_string())
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let launcher_pid = launcher.id().unwrap();
    let mut lines = tokio::io::BufReader::new(launcher.stdout.take().unwrap()).lines();
    let ready = tokio::time::timeout(Duration::from_secs(60), async {
        while let Some(l) = lines.next_line().await.unwrap() {
            if let Some(r) = l.strip_prefix("READY ") {
                return r.to_string();
            }
        }
        panic!("the launcher ended before its profile was open");
    })
    .await
    .expect("the launcher did not open its profile");
    let pids: Vec<u32> = ready.split(' ').map(|p| p.parse().unwrap()).collect();
    let (reaper, browser) = (pids[0], pids[1]);
    let copy = t.copy(&id);
    assert!(copy.join(MARKER).is_file() && alive(reaper) && alive(browser));
    assert_eq!(t.row(&id).await["open_on"]["device_id"], team::device_id());

    // The reaper's own command line: which launcher, which copy, and no token.
    let ps = std::process::Command::new("ps").args(["-ww", "-o", "command=", "-p", &reaper.to_string()]).output().unwrap();
    let line = String::from_utf8_lossy(&ps.stdout).trim().to_string();
    assert!(line.ends_with(&format!(" --reap {launcher_pid} {}", copy.display())), "{line}");
    assert!(!line.contains(&t.vera.token), "{line}");

    // SAFETY: a signal to our own child.
    assert_eq!(unsafe { libc::kill(launcher_pid as libc::pid_t, libc::SIGKILL) }, 0);
    let took = until("the browser and the copy gone", Duration::from_secs(25), || !alive(browser) && !copy.exists()).await;
    eprintln!("[test] reaped in {took:?}");
    until("the reaper gone", Duration::from_secs(5), || !alive(reaper)).await;
    assert_eq!(t.session_rev(&id).await, 1, "sent back before it was wiped");
    assert!(t.row(&id).await["open_on"].is_null(), "the lease went back");
    assert!(!t.root.path.join("team-open").exists());
    let _ = launcher.wait().await;
}

#[cfg(unix)]
#[tokio::test]
async fn a_normal_close_leaves_no_reaper_behind() {
    let t = Team::new(&everything()).await;
    let id = t.profile().await;
    t.read().await;
    spawn_reapers(true);
    t.launch(&id).await.unwrap();
    let reaper = reaper_of(&t.copy(&id)).expect("a reaper for the copy");
    assert!(alive(reaper));
    // Still watching while the launcher lives.
    tokio::time::sleep(Duration::from_millis(1200)).await;
    assert!(alive(reaper) && t.copy(&id).is_dir());
    t.stop(&id).await;
    assert_eq!(outcomes(&id), [Outcome::Saved(1)]);
    t.assert_nothing_here(&id);
    until("the reaper gone", Duration::from_secs(5), || !alive(reaper)).await;
}

#[cfg(unix)]
#[tokio::test]
async fn a_copy_being_reaped_is_left_to_its_reaper_and_an_old_reaper_leaves_a_new_copy() {
    let t = Team::new(&everything()).await;
    let id = t.profile().await;
    TeamClient::for_team(&t.team_id).unwrap().call(Method::POST, &Team::lease_path(&id), None).await.unwrap();
    let pid = left_behind(&t, &id, Some(0));
    let copy = t.copy(&id);

    // A reaper holds it: the sweep at start leaves it alone, and so does a second reaper.
    let held = take_reaping(&copy).unwrap();
    sweep();
    assert!(!t.is_open(&id));
    assert!(!reap(copy.clone(), &format!("n-{id}")).await);
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(alive(pid) && copy.join(MARKER).is_file());
    drop(held);

    // The reaper of an earlier opening of the same profile leaves this one.
    assert!(!reap(copy.clone(), "n-an-earlier-opening").await);
    assert!(alive(pid) && copy.join(MARKER).is_file());

    assert!(reap(copy.clone(), &format!("n-{id}")).await);
    assert!(!alive(pid));
    t.assert_nothing_here(&id);
    assert_eq!(outcomes(&id), [Outcome::Saved(1)]);
    assert!(t.row(&id).await["open_on"].is_null());
}

#[cfg(unix)]
#[tokio::test]
async fn a_reaper_stops_sending_at_its_limit_and_keeps_the_copy_for_the_next_start() {
    let t = Team::new(&everything()).await;
    let id = t.profile().await;
    let pid = left_behind(&t, &id, Some(0));
    TeamClient::for_team(&t.team_id).unwrap().call(Method::POST, &Team::lease_path(&id), None).await.unwrap();
    // A server that takes the connection and never answers.
    let silent = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    team::set_server(&t.team_id, &format!("http://{}", silent.local_addr().unwrap()));
    let copy = t.copy(&id);
    let s = Session::adopt(read_marker(&copy).unwrap(), copy.clone(), take_reaping(&copy).unwrap());
    let started = Instant::now();
    let outcome = s.close_orphan(Some(Duration::from_secs(1))).await;
    let took = started.elapsed();
    assert_eq!(outcome, Outcome::Unsent("the team server did not take it in time".into()));
    assert!(took < Duration::from_secs(1) + Duration::from_secs(3), "{took:?}");
    assert!(!alive(pid));
    assert!(read_marker(&copy).is_some_and(|m| m.base == Some(0)), "kept, marked as it was");
    assert!(copy.join("udd/Default/Local Storage/leveldb/000003.log").exists());
    assert!(!t.is_open(&id));
    drop(silent);

    // The next start sends it.
    team::set_server(&t.team_id, &t.srv.url);
    sweep();
    t.closed(&id).await;
    assert_eq!(outcomes(&id), [Outcome::Unsent("the team server did not take it in time".into()), Outcome::Saved(1)]);
    t.assert_nothing_here(&id);
    assert!(t.row(&id).await["open_on"].is_null());
}

/// The link breaks as the session goes up: it is sent again once the server
/// answers, the lease is kept meanwhile and given back after, and nothing is lost.
#[cfg(unix)]
#[tokio::test]
async fn a_session_the_server_did_not_take_is_sent_again() {
    let t = Team::new(&everything()).await;
    let id = t.profile().await;
    t.read().await;
    t.launch(&id).await.unwrap();
    let store = t.udd(&id).join("Default/Local Storage/leveldb");
    fs::create_dir_all(&store).unwrap();
    fs::write(store.join("000003.log"), b"made here").unwrap();
    let closed = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    team::set_server(&t.team_id, &format!("http://{}", closed.local_addr().unwrap()));
    drop(closed);
    assert!(crate::process::Tracker::shared().kill(&id).await.unwrap());
    until("the browser closed", Duration::from_secs(10), || !crate::process::Tracker::shared().is_running(&id)).await;
    tokio::time::sleep(Duration::from_secs(3)).await;
    assert!(t.is_open(&id), "still being saved");
    assert!(outcomes(&id).is_empty());
    assert_eq!(t.row(&id).await["open_on"]["device_id"], team::device_id(), "the lease is still this device's");

    team::set_server(&t.team_id, &t.srv.url);
    let busy = t.launch(&id).await.err().map(|e| e.to_string());
    assert_eq!(busy.as_deref(), Some(BUSY));
    until("sent again", Duration::from_secs(40), || !t.is_open(&id)).await;
    assert_eq!(outcomes(&id), [Outcome::Saved(1)]);
    t.assert_nothing_here(&id);
    assert!(t.row(&id).await["open_on"].is_null(), "the lease went back");
    t.launch(&id).await.unwrap();
    assert_eq!(fs::read(store.join("000003.log")).unwrap(), b"made here");
    t.stop(&id).await;
}

/// A quit that ran out of time keeps the copy, and the next start sends it.
#[cfg(unix)]
#[tokio::test]
async fn a_quit_out_of_time_keeps_the_copy_for_the_next_start() {
    let t = Team::new(&everything()).await;
    let id = t.profile().await;
    t.read().await;
    t.launch(&id).await.unwrap();
    let silent = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    team::set_server(&t.team_id, &format!("http://{}", silent.local_addr().unwrap()));
    close_all(Duration::from_secs(2)).await;
    assert!(!any_open());
    let copy = t.copy(&id);
    assert!(read_marker(&copy).is_some_and(|m| m.base == Some(0)), "kept, marked");
    assert!(t.udd(&id).exists());

    team::set_server(&t.team_id, &t.srv.url);
    sweep();
    t.closed(&id).await;
    assert_eq!(outcomes(&id).last(), Some(&Outcome::Saved(1)));
    t.assert_nothing_here(&id);
    drop(silent);
}

/// A copy left unsent is sent before the profile opens here again, never wiped by it.
#[cfg(unix)]
#[tokio::test]
async fn a_start_sends_a_copy_left_unsent_first() {
    let t = Team::new(&everything()).await;
    let id = t.profile().await;
    t.read().await;
    let pid = left_behind(&t, &id, Some(0));
    let busy = t.launch(&id).await.err().map(|e| e.to_string());
    assert_eq!(busy.as_deref(), Some(BUSY));
    t.closed(&id).await;
    assert!(!alive(pid));
    assert_eq!(outcomes(&id), [Outcome::Saved(1)]);
    t.launch(&id).await.unwrap();
    assert_eq!(fs::read(t.udd(&id).join("Default/Local Storage/leveldb/000003.log")).unwrap(), id.as_bytes());
    t.stop(&id).await;
}

/// A team profile whose settings carry these extensions; its id.
async fn profile_with_extensions(t: &Team, folder: &str, ext: &[&str]) -> String {
    let id = format!("p-{}", uuid::Uuid::new_v4().simple());
    let data = json!({ "_meta": { "extensions": ext } }).to_string();
    let body = json!({ "name": "Shop", "folder": folder, "data": b64(data.as_bytes()), "card": {} });
    t.admin().call(Method::PUT, &format!("/profiles/{id}"), Some(&body)).await.unwrap();
    id
}

/// Every file under `dir` but those under `skip` named `name`.
fn named(dir: &Path, skip: &[PathBuf], name: &str, out: &mut Vec<PathBuf>) {
    for e in fs::read_dir(dir).into_iter().flatten().flatten() {
        let p = e.path();
        if skip.iter().any(|s| p.starts_with(s)) {
            continue;
        }
        if p.is_dir() {
            named(&p, skip, name, out);
        } else if p.file_name().is_some_and(|n| n == name) {
            out.push(p);
        }
    }
}

#[cfg(unix)]
#[tokio::test]
async fn a_team_extension_is_unpacked_into_the_open_copy_and_goes_with_it() {
    use super::super::{bookmarks, extensions, Door, Ticket};
    let t = Team::new(&everything()).await;
    t.read().await;
    let src_dir = t.root.path.join("src");
    let src = super::super::items::tests::an_extension(&src_dir);
    let e = extensions::import(Door("extension_import"), &Ticket::now(), &src).await.unwrap();
    // The team's bookmark for the folder goes in; this machine's own does not.
    let mark = bookmarks::Bookmark { id: String::new(), title: "Team mail".into(), url: "https://team-mail.example".into(), folder: "Ads".into(), root: String::new(), path: Vec::new() };
    bookmarks::save(Door("bookmark_save"), &Ticket::now(), mark).await.unwrap();
    let mine = json!({ "bookmarks": [{ "id": "m1", "title": "Mine", "url": "https://mine.example", "folder": "" }] });
    fs::write(crate::store::bookmarks_path().unwrap(), mine.to_string()).unwrap();
    let id = profile_with_extensions(&t, "Ads", &[&e.id, "no-such-extension"]).await;
    t.read().await;

    t.launch(&id).await.unwrap();
    let ext = t.copy(&id).join("ext").join(&e.id);
    assert_eq!(fs::read_to_string(ext.join("js/bg.js")).unwrap(), "console.log('bg')");
    let argv = t.argv().pop().unwrap();
    assert!(argv.contains(&format!("--load-extension={}", ext.display())), "{argv}");
    assert!(!argv.contains("no-such-extension"), "one it cannot read is left out: {argv}");
    let marks = fs::read_to_string(t.udd(&id).join("Default/Bookmarks")).unwrap();
    assert!(marks.contains("team-mail.example") && !marks.contains("mine.example"), "{marks}");
    let library = crate::store::extensions_dir().unwrap();
    assert_eq!(fs::read_dir(&library).unwrap().count(), 0, "never in this machine's library");

    t.stop(&id).await;
    assert_eq!(outcomes(&id), [Outcome::Saved(1)]);
    t.assert_nothing_here(&id);
    assert!(!ext.exists());
    let mut left = Vec::new();
    named(&t.root.path, &[src_dir, t.root.path.join("server")], "bg.js", &mut left);
    assert!(left.is_empty(), "its files stayed: {left:?}");
}

/// A member with rights on the profile's folder only: the team's lists are
/// hidden from them, yet the profile starts with its extensions and its folder's bookmarks.
#[cfg(unix)]
#[tokio::test]
async fn a_folder_member_starts_a_profile_with_its_extensions_and_bookmarks() {
    use super::super::{bookmarks, extensions, Door, Ticket};
    let t = Team::new(&everything()).await;
    t.read().await;
    let src_dir = t.root.path.join("src");
    let src = super::super::items::tests::an_extension(&src_dir);
    let e = extensions::import(Door("extension_import"), &Ticket::now(), &src).await.unwrap();
    for (title, folder) in [("ads.example", "Ads"), ("other.example", "Other"), ("all.example", "")] {
        let mark = bookmarks::Bookmark { id: String::new(), title: title.into(), url: format!("https://{title}"), folder: folder.into(), root: String::new(), path: Vec::new() };
        bookmarks::save(Door("bookmark_save"), &Ticket::now(), mark).await.unwrap();
    }
    let id = profile_with_extensions(&t, "Ads", &[&e.id]).await;
    let ads = |action| on(Scope::Folder { name: "Ads".into() }, action);
    t.set_grants(&[ads(Action::See), ads(Action::Start)]).await;
    perm::forget(&t.team_id);
    t.read().await;
    assert!(extensions::list().unwrap().is_empty() && bookmarks::list().unwrap().is_empty(), "the team's lists are hidden");

    t.launch(&id).await.unwrap();
    let ext = t.copy(&id).join("ext").join(&e.id);
    assert_eq!(fs::read_to_string(ext.join("js/bg.js")).unwrap(), "console.log('bg')");
    assert!(t.argv().pop().unwrap().contains(&format!("--load-extension={}", ext.display())));
    let marks = fs::read_to_string(t.udd(&id).join("Default/Bookmarks")).unwrap();
    assert!(marks.contains("ads.example") && marks.contains("all.example"), "{marks}");
    assert!(!marks.contains("other.example"), "{marks}");
    t.stop(&id).await;
    t.assert_nothing_here(&id);
}

#[cfg(all(unix, feature = "automation"))]
#[tokio::test]
async fn a_run_starts_a_team_profile_only_with_start_and_cookies() {
    let t = Team::new(&team_wide(&[Action::See, Action::Start])).await;
    let id = t.profile().await;
    let mut s = crate::settings::load().unwrap();
    s.browser_path = Some(t.engine.display().to_string());
    crate::settings::save(&s).unwrap();
    let project = json!({
        "id": "run-1", "name": "Warm up", "created_at": 1, "updated_at": 1,
        "blocks": [
            { "id": "use", "kind": "profile.use", "params": { "id": id } },
            { "id": "go", "kind": "goto", "params": { "url": "https://shop.example" }, "y": 1.0 }
        ]
    });
    let body = json!({ "name": "Warm up", "data": b64(project.to_string().as_bytes()) });
    t.admin().call(Method::PUT, "/items/automation/run-1", Some(&body)).await.unwrap();
    t.read().await;

    async fn run_to_its_end() -> Vec<String> {
        crate::runner::start("run-1").await.unwrap();
        until("the run's end", Duration::from_secs(60), || crate::runner::status("run-1").is_some_and(|s| !s.running)).await;
        crate::runner::status("run-1").unwrap().log
    }
    let log = run_to_its_end().await;
    assert!(log.iter().any(|l| l.contains("needs cookies on the profile")), "{log:?}");
    assert!(t.started().is_empty(), "no browser");
    assert!(t.row(&id).await["open_on"].is_null(), "no lease taken");
    t.assert_nothing_here(&id);

    // With Cookies too it starts, debugging on; the port answers nothing, so the run closes it.
    t.set_grants(&team_wide(&[Action::See, Action::Start, Action::Cookies])).await;
    let issued = TeamClient::for_team(&t.team_id).unwrap().permission().await.unwrap();
    perm::accept(&t.team_id, &issued.permission).unwrap();
    let log = run_to_its_end().await;
    assert_eq!(t.started(), [t.udd(&id).display().to_string()], "{log:?}");
    assert!(t.argv()[0].contains("--remote-debugging-port=0"), "{:?}", t.argv());
    t.closed(&id).await;
    t.assert_nothing_here(&id);
    assert_eq!(outcomes(&id), [Outcome::Saved(1)]);

    // A run keeps to the space it started in: once the launcher moved, it starts nothing.
    let started_in = super::super::Ticket::now();
    crate::space::switch(team::Space::Personal).await.unwrap();
    crate::space::switch(team::Space::Team { team_id: t.team_id.clone() }).await.unwrap();
    let err = crate::launch::launch_for_run(&started_in, &id, 0, "").await.err().unwrap();
    assert_eq!(err.to_string(), super::super::error::SPACE_CHANGED);
    assert_eq!(t.started().len(), 1);
}

/// A wipe a lock held up is tried again on what it moved aside, and never on
/// the folder a copy was made in again meanwhile.
#[cfg(unix)]
#[tokio::test]
async fn a_wipe_tried_again_never_touches_a_copy_made_again_in_its_folder() {
    use std::os::unix::fs::PermissionsExt;
    let t = Team::new(&everything()).await;
    let dir = t.copy("p-again");
    let held = dir.join("udd/held");
    fs::create_dir_all(&held).unwrap();
    fs::write(held.join("f"), b"x").unwrap();
    // As a Windows lock would: what is in it cannot go yet.
    fs::set_permissions(&held, fs::Permissions::from_mode(0o500)).unwrap();
    wipe(&dir);
    assert!(!dir.exists(), "moved aside at once");
    let aside: Vec<PathBuf> = fs::read_dir(dir.parent().unwrap()).unwrap().flatten().map(|e| e.path()).collect();
    assert_eq!(aside.len(), 1, "{aside:?}");

    // A copy made again in the same folder: the next try leaves it be.
    fs::create_dir_all(dir.join("udd")).unwrap();
    fs::write(dir.join(MARKER), b"{}").unwrap();
    fs::write(dir.join("udd/Preferences"), b"live").unwrap();
    assert!(!retry_left(), "still held");
    fs::set_permissions(aside[0].join("udd/held"), fs::Permissions::from_mode(0o700)).unwrap();
    assert!(retry_left());
    assert!(!aside[0].exists());
    assert_eq!(fs::read(dir.join("udd/Preferences")).unwrap(), b"live");

    // Left where it was (it could not be moved): tried only while no copy is there.
    left().push(dir.clone());
    assert!(!retry_left(), "a marked copy is left alone");
    assert!(dir.join("udd/Preferences").is_file());
    fs::remove_file(dir.join(MARKER)).unwrap();
    assert!(retry_left());
    assert!(!dir.exists());
}

/// Sent back, then the launcher dies before the copy is wiped: its reaper
/// sends it again from the revision it was saved at, not as stale.
#[cfg(unix)]
#[tokio::test]
async fn a_copy_saved_just_before_the_launcher_died_is_not_taken_for_stale() {
    let t = Team::new(&everything()).await;
    let id = t.profile().await;
    t.read().await;
    let (start, _) = passes(&t.team_id, &id, false).await.unwrap();
    let mut opened = open_team(&t.team_id, &start, false).await.unwrap();
    let s = opened.hand.0.take().unwrap();
    assert_eq!(s.send_back(None).await, Outcome::Saved(1));
    assert_eq!(read_marker(&t.copy(&id)).unwrap().base, Some(1), "marked as saved");

    // The launcher dies here, before the lease goes back and the copy is wiped.
    assert!(reap(t.copy(&id), &s.nonce).await);
    assert_eq!(outcomes(&id), [Outcome::Saved(2)]);
    assert_eq!(t.session_rev(&id).await, 2);
    assert!(t.row(&id).await["open_on"].is_null(), "the lease went back");
    s.stop_watch().await;
    unregister(&s);
    // Its watcher holds the last handle: it ends while the test's root is still here.
    let watched = std::sync::Arc::downgrade(&s);
    drop(s);
    until("the watcher let go", Duration::from_secs(10), || watched.strong_count() == 0).await;
    t.assert_nothing_here(&id);
}
