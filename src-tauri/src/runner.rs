//! Runs a project: N browsers in parallel, each walking the block list.
//!
//! Every page action goes through the Motion domain. Nothing here dispatches a
//! synthetic event and nothing executes script in the page — that is the whole
//! point of the feature, and the reason the block set is deliberately small.

#![cfg(feature = "automation")]

use crate::space::{automation, profile, proxy};
use crate::{cdp, launch, migrate, process, store, wasm};
use anyhow::{anyhow, Context, Result};
use serde::Serialize;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Serialize)]
pub struct WorkerState {
    pub profile_id: String,
    pub profile_name: String,
    /// 1-based; 0 before the first pass starts.
    pub pass: u32,
    pub step: u32,
    pub steps_total: u32,
    /// "starting" | "running" | "done" | "failed" | "stopped"
    pub status: String,
    pub note: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct RunState {
    pub project_id: String,
    pub project_name: String,
    pub started_at: u64,
    pub running: bool,
    pub workers: Vec<WorkerState>,
    pub log: Vec<String>,
}

struct Run {
    state: Mutex<RunState>,
    /// Shared, not a bare flag: a WASM module runs on a blocking thread and
    /// asks `should_stop()`, so the operator's Stop has to reach it there.
    stop: Arc<AtomicBool>,
    /// Interception rules for this project, mounted per profile on first attach.
    rules: Vec<serde_json::Value>,
    /// Every saved project, as they stood when the run started. A sub-flow
    /// resolves out of this rather than re-reading the file per call, and
    /// editing a project mid-run cannot change a call already in flight.
    projects: Vec<automation::Project>,
    /// The space the run started in: its profile steps and starts go there or nowhere.
    space: crate::space::Ticket,
}

fn runs() -> &'static Mutex<HashMap<String, Arc<Run>>> {
    static R: OnceLock<Mutex<HashMap<String, Arc<Run>>>> = OnceLock::new();
    R.get_or_init(|| Mutex::new(HashMap::new()))
}

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

pub fn status(project_id: &str) -> Option<RunState> {
    let g = runs().lock().ok()?;
    let r = g.get(project_id)?;
    r.state.lock().ok().map(|s| s.clone())
}

/// Every run currently going, for the fleet window.
pub fn all() -> Vec<RunState> {
    let Ok(g) = runs().lock() else { return Vec::new() };
    g.values()
        .filter_map(|r| r.state.lock().ok().map(|s| s.clone()))
        .collect()
}

pub fn stop(project_id: &str) {
    if let Ok(g) = runs().lock() {
        if let Some(r) = g.get(project_id) {
            r.stop.store(true, Ordering::Relaxed);
        }
    }
}

/// Where a run's log lives. One file per run, named so they sort by time.
fn log_path(project_id: &str, started_at: u64) -> Option<std::path::PathBuf> {
    let dir = store::config_root().ok()?.join("automation-runs");
    std::fs::create_dir_all(&dir).ok()?;
    Some(dir.join(format!("{started_at}-{project_id}.log")))
}

/// Keeps the newest `keep` run logs and deletes the rest. Called once when a
/// run starts, so a machine left running for months does not fill up.
fn prune_logs(keep: usize) {
    let Ok(root) = store::config_root() else { return };
    let dir = root.join("automation-runs");
    let Ok(entries) = std::fs::read_dir(&dir) else { return };
    let mut files: Vec<_> = entries
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().and_then(|x| x.to_str()) == Some("log"))
        .collect();
    if files.len() <= keep {
        return;
    }
    files.sort_by_key(|e| e.file_name());
    let cut = files.len() - keep;
    for f in files.into_iter().take(cut) {
        let _ = std::fs::remove_file(f.path());
    }
}

impl Run {
    fn log(&self, line: impl Into<String>) {
        let line = line.into();
        let stamped = format!("[{}] {line}", now());
        let mut path = None;
        if let Ok(mut s) = self.state.lock() {
            s.log.push(stamped.clone());
            // In memory the log is a tail, on disk it is the whole thing: the
            // window only ever shows the last screenful, and a run that failed
            // three hours in is exactly the one whose beginning matters.
            if s.log.len() > 2000 {
                s.log.drain(0..500);
            }
            path = log_path(&s.project_id, s.started_at);
        }
        if let Some(p) = path {
            use std::io::Write;
            if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(p) {
                let _ = writeln!(f, "{stamped}");
            }
        }
    }

    fn worker<F: FnOnce(&mut WorkerState)>(&self, idx: usize, f: F) {
        if let Ok(mut s) = self.state.lock() {
            if let Some(w) = s.workers.get_mut(idx) {
                f(w);
            }
        }
    }
}

/// Substitutes {{name}} from the worker's own variables.
fn expand(text: &str, vars: &HashMap<String, String>) -> String {
    let mut out = text.to_string();
    for (k, v) in vars {
        out = out.replace(&format!("{{{{{k}}}}}"), v);
    }
    out
}

fn param<'a>(params: &'a Value, name: &str) -> Option<&'a str> {
    params.get(name).and_then(|v| v.as_str()).filter(|s| !s.is_empty())
}

/// A number a step asks for, with `{{variables}}` resolved first.
///
/// A step writes its numbers as numbers, but the operator can also type
/// `{{delay}}` into one — that is how `var.random` feeds a wait. Without the
/// expansion the string stayed a string, parsing it failed, and the step
/// silently fell back to its default: `var.random` into `wait` did nothing at
/// all, and every `timeout` behaved the same way.
fn param_f64(params: &Value, name: &str, vars: &HashMap<String, String>) -> Option<f64> {
    params.get(name).and_then(|v| match v {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => expand(s, vars).trim().parse().ok(),
        _ => None,
    })
}

/// Viewport centre of the element a selector matches.
async fn center_of(profile: &str, selector: &str) -> Result<(f64, f64)> {
    // Through the piercing resolver, so a selector recorded inside a shadow
    // root still finds its element on replay.
    let node = *cdp::query_piercing(profile, selector)
        .await?
        .first()
        .ok_or_else(|| anyhow!("no element matches {selector}"))?;

    let box_model = cdp::page_call(profile, "DOM.getBoxModel", json!({ "nodeId": node })).await?;
    let quad = box_model
        .get("model")
        .and_then(|m| m.get("content"))
        .and_then(|c| c.as_array())
        .ok_or_else(|| anyhow!("{selector} has no box"))?;
    if quad.len() < 8 {
        return Err(anyhow!("{selector} has no box"));
    }
    let n = |i: usize| quad[i].as_f64().unwrap_or(0.0);
    // The content quad is four corners; the centre is the mean of the x's and
    // the y's, which is right for a rotated element too.
    let x = (n(0) + n(2) + n(4) + n(6)) / 4.0;
    let y = (n(1) + n(3) + n(5) + n(7)) / 4.0;
    Ok((x, y))
}

/// The visible page's size, from the browser. A finger's stroke is bounded by
/// the glass, so a long scroll has to know how much glass there is.
async fn viewport_size(profile: &str) -> (f64, f64) {
    if let Ok(m) = cdp::page_call(profile, "Page.getLayoutMetrics", json!({})).await {
        let v = m.get("cssVisualViewport").or_else(|| m.get("visualViewport"));
        if let Some(v) = v {
            let w = v.get("clientWidth").and_then(|x| x.as_f64()).unwrap_or(0.0);
            let h = v.get("clientHeight").and_then(|x| x.as_f64()).unwrap_or(0.0);
            if w > 0.0 && h > 0.0 {
                return (w, h);
            }
        }
    }
    (800.0, 800.0)
}

/// Middle of the visible page, from the browser rather than a guess.
async fn viewport_center(profile: &str) -> (f64, f64) {
    if let Ok(m) = cdp::page_call(profile, "Page.getLayoutMetrics", json!({})).await {
        let v = m.get("cssVisualViewport").or_else(|| m.get("visualViewport"));
        if let Some(v) = v {
            let w = v.get("clientWidth").and_then(|x| x.as_f64()).unwrap_or(0.0);
            let h = v.get("clientHeight").and_then(|x| x.as_f64()).unwrap_or(0.0);
            if w > 0.0 && h > 0.0 {
                return (w / 2.0, h / 2.0);
            }
        }
    }
    (400.0, 400.0)
}

/// How long an element is given to turn up before the step gives in.
const DEFAULT_WAIT_S: f64 = 5.0;

/// The element's centre, waiting for it to appear.
///
/// Every step that names an element goes through here. Asking once and failing
/// is wrong on any page worth automating: the click that follows a navigation
/// arrives while the page is still building itself, and the element it wants
/// is a few hundred milliseconds away.
async fn center_waiting(profile: &str, selector: &str, timeout_s: f64) -> Result<(f64, f64)> {
    let deadline = Instant::now() + Duration::from_secs_f64(timeout_s.clamp(0.0, 600.0));
    loop {
        match center_of(profile, selector).await {
            Ok(p) => return Ok(p),
            Err(e) => {
                // Reported at the moment of giving up, so the message says what
                // was actually wrong and not merely that the wait ran out.
                if Instant::now() >= deadline {
                    return Err(anyhow!(
                        "{selector} did not turn up within {}s ({e})",
                        tidy(timeout_s)
                    ));
                }
            }
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

/// The wait a step asks for, or the default.
fn wait_for(p: &Value, vars: &HashMap<String, String>) -> f64 {
    param_f64(p, "timeout", vars).filter(|v| *v >= 0.0).unwrap_or(DEFAULT_WAIT_S)
}

async fn exists(profile: &str, selector: &str) -> bool {
    center_of(profile, selector).await.is_ok()
}

async fn click_at(profile: &str, x: f64, y: f64, button: &str) -> Result<()> {
    cdp::page_call(profile, "Motion.createPointer", json!({ "x": x, "y": y })).await?;
    cdp::page_call(profile, "Motion.glideTo", json!({ "x": x, "y": y })).await?;
    let clicks = if button == "double" { 2 } else { 1 };
    let b = if button == "right" { "right" } else { "left" };
    cdp::page_call(profile, "Motion.tap", json!({ "button": b, "clickCount": clicks })).await?;
    Ok(())
}

/// One frame of a chain of calls: a module block, or a saved project called as
/// a sub-flow.
///
/// Named rather than counted so a loop can be reported as the loop it is. A
/// module that calls itself reaches the depth bound in four hops and would
/// otherwise always be told the wrong thing.
#[derive(Clone, PartialEq)]
pub enum Frame {
    Module(String),
    Flow(String),
}

impl Frame {
    fn name(&self) -> String {
        match self {
            Frame::Module(id) => format!("module \"{id}\""),
            Frame::Flow(id) => format!("flow \"{id}\""),
        }
    }
}

/// What a worker's whole chain of calls shares.
///
/// One per worker, reborrowed down every nested frame. Not a process global:
/// the recursion spans several blocking threads and several WASM stores at
/// once, and two workers must never share a counter.
pub struct CallCtx {
    depth: u8,
    stack: Vec<Frame>,
    /// Actions the current chain has spent. Reset when a chain starts, not per
    /// pass: a project that calls a module block ten times gets ten budgets,
    /// exactly as it does today, while a module that nests cannot multiply its
    /// own by calling through another one.
    actions: u32,
    /// Every temporary profile this worker made. Cleanup used to look only at
    /// whatever was FINALLY bound, so a graph that made two orphaned the first.
    temps: Vec<String>,
    /// Variables a module wrote.
    ///
    /// Refusing a module `script.run` is not enough on its own: it can write a
    /// variable instead and wait for a block the OPERATOR wrote to interpolate
    /// it — `script.run source="{{payload}}"` runs the module's text without
    /// the module ever asking for the block. So where a value came from travels
    /// with its name, and the three steps that turn a value into running code
    /// refuse one that came from a module.
    tainted: std::collections::HashSet<String>,
}

/// What `enter` took away, put back by `leave`. A value rather than a guard:
/// a guard would borrow the context that the callee also needs.
#[derive(Debug)]
struct Saved {
    depth: u8,
    stack_len: usize,
}

/// Four frames of nesting. Deep enough for a module that calls a helper that
/// calls a flow; shallow enough that a runaway is reported rather than felt.
const MAX_CALL_DEPTH: u8 = 4;
/// What one chain may ask the runner to do. The same 5000 a single module has
/// always had — now shared by everything it calls, so nesting cannot multiply it.
const MAX_CHAIN_ACTIONS: u32 = 5000;

impl CallCtx {
    fn new() -> Self {
        Self {
            depth: 0,
            stack: Vec::new(),
            actions: 0,
            temps: Vec::new(),
            tainted: std::collections::HashSet::new(),
        }
    }

    fn enter(&mut self, frame: Frame) -> Result<Saved> {
        if let Some(at) = self.stack.iter().position(|f| *f == frame) {
            let mut chain: Vec<String> = self.stack[at..].iter().map(Frame::name).collect();
            chain.push(frame.name());
            return Err(anyhow!("that call goes round in a circle: {}", chain.join(" → ")));
        }
        if self.depth >= MAX_CALL_DEPTH {
            let chain: Vec<String> = self.stack.iter().map(Frame::name).collect();
            return Err(anyhow!(
                "a call may be {MAX_CALL_DEPTH} deep and this is one more: {} → {}",
                chain.join(" → "),
                frame.name()
            ));
        }
        let saved = Saved { depth: self.depth, stack_len: self.stack.len() };
        if self.depth == 0 {
            self.actions = 0;
        }
        self.depth += 1;
        self.stack.push(frame);
        Ok(saved)
    }

    /// The module this work is being done for, if any.
    ///
    /// Any frame, not only the top one: a module decides when to call a flow
    /// and what to pass into it, so a flow doing `goto {{url}}` is the module's
    /// reach with one step in between.
    fn on_behalf_of(&self) -> Option<&str> {
        self.stack.iter().find_map(|f| match f {
            Frame::Module(id) => Some(id.as_str()),
            Frame::Flow(_) => None,
        })
    }

    /// Whether the frame currently on top may call `target`.
    ///
    /// At depth 0 there is no caller: the operator placed the block themselves,
    /// and the whole point of the block library is that they may. Deeper, the
    /// decision belongs to what the operator granted the CALLING module — a
    /// module that was never granted anything calls nothing, which is what
    /// makes this safe to add to a contract that already shipped.
    fn may_call(&self, target: &Frame) -> Result<()> {
        let Some(Frame::Module(caller)) = self.stack.last() else {
            // Either depth 0, or the caller is a flow — operator-authored
            // blocks, which are trusted exactly as they are when run directly.
            return Ok(());
        };
        let grant = wasm::grant_for(caller);
        let (allowed, what) = match target {
            Frame::Module(id) => (grant.call_modules.iter().any(|m| m == id), format!("module \"{id}\"")),
            // "*" is a module that asked to run any project and was granted
            // exactly that by the operator (Modules: "run any of your projects").
            Frame::Flow(id) => (
                grant.call_flows.iter().any(|f| f == id || f == "*"),
                "that flow".to_string(),
            ),
        };
        if allowed {
            return Ok(());
        }
        Err(anyhow!(
            "\"{caller}\" was not given permission to call {what} — grant it in Modules"
        ))
    }

    fn leave(&mut self, saved: Saved) {
        self.depth = saved.depth;
        self.stack.truncate(saved.stack_len);
    }

    /// Charges one action to the whole chain.
    fn charge(&mut self) -> Result<()> {
        self.actions += 1;
        if self.actions > MAX_CHAIN_ACTIONS {
            return Err(anyhow!("this chain of calls asked for more than {MAX_CHAIN_ACTIONS} actions"));
        }
        Ok(())
    }

    /// Marks what a module wrote, so a later block cannot run it as code.
    fn mark_from_module(&mut self, names: impl IntoIterator<Item = String>) {
        if self.on_behalf_of().is_some() {
            self.tainted.extend(names);
        }
    }

    /// Refuses a step that would turn a module's text into running code.
    ///
    /// The raw parameters are searched, not the expanded ones: by the time a
    /// value is substituted it is indistinguishable from one the operator typed,
    /// which is the whole point of the attack.
    fn refuse_laundering(&self, block: &automation::Block) -> Result<()> {
        const SINKS: [&str; 3] = ["script.run", "traffic.editResponse", "traffic.fulfill"];
        if self.tainted.is_empty() || !SINKS.contains(&block.kind.as_str()) {
            return Ok(());
        }
        let raw = block.params.to_string();
        for name in &self.tainted {
            if raw.contains(&format!("{{{{{name}}}}}")) {
                return Err(anyhow!(
                    "\"{}\" would run the value of \"{name}\", and a module wrote that — \
                     a module's text is never code",
                    block.kind
                ));
            }
        }
        Ok(())
    }

    fn remember_temp(&mut self, id: &str) {
        if !id.is_empty() && !self.temps.iter().any(|t| t == id) {
            self.temps.push(id.to_string());
        }
    }
}

/// A profile the graph made, and whether it goes away at the end.
#[derive(Default, Clone)]
pub struct Bound {
    pub id: String,
    pub temporary: bool,
    /// Phone/tablet fingerprint. Decided once, when the profile is bound: the
    /// core refuses a pointer on such a profile, so every step that would have
    /// used one has to reach for a finger instead.
    pub mobile: bool,
}

/// Whether a profile id claims a handset, by the same rule the core uses.
/// Brings the window forward before a touch. The core refuses a gesture aimed
/// at a background window, and the failure looks like the gesture simply
/// missing.
async fn touch_front(profile: &str) {
    let _ = cdp::page_call(profile, "Page.bringToFront", json!({})).await;
}

fn bound_is_mobile(id: &str) -> bool {
    crate::space::profile::load_raw(id)
        .map(|st| crate::space::profile::claims_mobile(&st.config))
        .unwrap_or(false)
}

/// Makes a profile from a random fingerprint of the wanted platform, exactly
/// the way the temporary-profile API does — same enrichment, same noise
/// defaults — so a profile a project made is not a lesser one.
fn make_profile(
    name: &str,
    folder: &str,
    platform: Option<&str>,
    inline_proxy: Option<&str>,
    temporary: bool,
) -> Result<String> {
    let all = crate::fingerprints::list_all()?;
    if all.is_empty() {
        return Err(anyhow!("the fingerprint library is empty"));
    }
    let want = platform
        .map(|p| p.trim().to_lowercase())
        .unwrap_or_else(crate::host_platform);
    let pool: Vec<_> = all
        .iter()
        .filter(|e| e.platform.eq_ignore_ascii_case(&want))
        .cloned()
        .collect();
    let pool = if pool.is_empty() { all } else { pool };
    let idx = (uuid::Uuid::new_v4().as_bytes()[0] as usize) % pool.len();
    let fid = pool[idx].id.clone();

    let mut cfg = crate::build_fingerprint_config(None, &fid).map_err(|e| anyhow!(e))?;
    cfg.remove("_meta");
    if !name.is_empty() {
        cfg.insert("name".into(), json!(name));
    }
    crate::ensure_default_noise(&mut cfg);
    crate::ensure_phone_voices(&mut cfg);

    let mut meta = json!({ "id": "", "folder": folder, "temporary": temporary });
    if let Some(pstr) = inline_proxy {
        // Deliberately inline: a proxy a project generated belongs to that
        // profile, not to the operator's proxy library, which is theirs to
        // curate and would otherwise fill up with single-use entries.
        let entry = proxy::parse_single(pstr)
            .ok_or_else(|| anyhow!("cannot read proxy \"{pstr}\""))?;
        meta["inline_proxy"] = serde_json::to_value(entry)?;
    }
    cfg.insert("_meta".into(), meta);

    // A run writes only this machine's own profiles; in a team this is refused.
    let pm = crate::save_profile_here(None, Value::Object(cfg), false).map_err(|e| anyhow!(e))?;
    Ok(pm.id)
}

/// Arithmetic on variables. Values that are not numbers are an error rather
/// than a silent zero — a total that quietly counted a failed read as nothing
/// is worse than one that stopped.
fn arithmetic(op: &str, a: f64, b: f64) -> Result<f64> {
    Ok(match op {
        "+" | "sum" | "add" => a + b,
        "-" | "sub" => a - b,
        "*" | "mul" => a * b,
        "/" | "div" => {
            if b == 0.0 {
                return Err(anyhow!("cannot divide by zero"));
            }
            a / b
        }
        "%" | "mod" => {
            if b == 0.0 {
                return Err(anyhow!("cannot take a remainder by zero"));
            }
            a % b
        }
        "min" => a.min(b),
        "max" => a.max(b),
        other => return Err(anyhow!("unknown operation \"{other}\"")),
    })
}

/// Variable names are ASCII on purpose. `{{имя}}` and `{{name}}` look alike in
/// a list and substitute differently, and a project shared between people is
/// read by someone whose keyboard may not make the first one at all.
fn check_var_name(name: &str) -> Result<String> {
    let name = name.trim();
    if name.is_empty() {
        return Err(anyhow!("a variable needs a name"));
    }
    if !name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '.')
    {
        return Err(anyhow!(
            "\"{name}\" — a variable name may only use English letters, digits, _ and ."
        ));
    }
    Ok(name.to_string())
}

fn as_number(vars: &HashMap<String, String>, raw: &str) -> Result<f64> {
    let expanded = expand(raw, vars);
    expanded
        .trim()
        .parse::<f64>()
        .map_err(|_| anyhow!("\"{expanded}\" is not a number"))
}

/// Trims a float that came out whole, so a count reads 3 and not 3.
fn tidy(n: f64) -> String {
    if n.fract() == 0.0 && n.abs() < 1e15 {
        format!("{}", n as i64)
    } else {
        format!("{n}")
    }
}

/// Brings up the bound profile's browser if it is not already connected.
///
/// Lazily, because the graph decides which profile there is: a project that
/// starts by making a temporary profile has nothing to launch until that block
/// has run.
async fn ensure_browser(bound: &Bound, run: &Run) -> Result<()> {
    if cdp::is_attached(&bound.id) {
        return Ok(());
    }
    if process::Tracker::shared().cdp(&bound.id).is_none() {
        let (port, token) = bus_details().await?;
        launch::launch_for_run(&run.space, &bound.id, port, &token).await?;
    }
    let info = process::Tracker::shared()
        .cdp(&bound.id)
        .ok_or_else(|| anyhow!("the browser started without a debugging port"))?;
    cdp::attach(bound.id.clone(), info.web_socket_debugger_url, |_| {}).await?;
    run.log(format!("browser up for {}", bound.id));
    // Mount the project's interception rules before anything navigates, so the
    // profile's very first request already sees them. Profile-scoped (no
    // targetId). A failure here is logged, not fatal — the run can still work
    // without interception.
    if !run.rules.is_empty() {
        match serde_json::to_string(&run.rules) {
            Ok(rules_json) => {
                if let Err(e) = cdp::page_call(
                    &bound.id,
                    "Traffic.mount",
                    json!({ "rules": rules_json }),
                )
                .await
                {
                    run.log(format!("interception rules did not mount: {e}"));
                } else {
                    run.log(format!("{} interception rule(s) mounted", run.rules.len()));
                }
            }
            Err(e) => run.log(format!("interception rules are not serialisable: {e}")),
        }
    }
    Ok(())
}

/// The sync bus the launcher already runs; a worker needs its details to start
/// a browser the same way the UI does.
async fn bus_details() -> Result<(u16, String)> {
    let b = crate::bus().await.map_err(|e| anyhow!(e))?;
    Ok((b.port, b.token.clone()))
}

/// What a block did. `EndPass` ends the rest of this pass; `Else` is a
/// condition that came out false — it takes the block's "when it fails" branch
/// without being an error.
#[derive(Debug)]
enum Flow {
    Next,
    EndPass,
    Else,
}

/// Compares two values for an `if` block. Numbers are compared as numbers when
/// both parse; otherwise as text. `b` is ignored for the empty/not-empty tests.
fn cond_holds(a: &str, op: &str, b: &str) -> bool {
    let nums = a.trim().parse::<f64>().ok().zip(b.trim().parse::<f64>().ok());
    match op {
        "=" | "==" | "equals" => a == b,
        "≠" | "!=" | "not equals" => a != b,
        "contains" => a.contains(b),
        "not contains" => !a.contains(b),
        "starts with" => a.starts_with(b),
        "ends with" => a.ends_with(b),
        "is empty" => a.trim().is_empty(),
        "is not empty" => !a.trim().is_empty(),
        ">" => nums.map(|(x, y)| x > y).unwrap_or(false),
        ">=" => nums.map(|(x, y)| x >= y).unwrap_or(false),
        "<" => nums.map(|(x, y)| x < y).unwrap_or(false),
        "<=" => nums.map(|(x, y)| x <= y).unwrap_or(false),
        _ => false,
    }
}

/// Every bit of text under a node, in document order.
///
/// Walked here rather than asked of the page, because asking means running a
/// function in it and this feature's whole point is that it never does. Script
/// and style bodies are skipped: they are text to the DOM and not to a person.
fn gather_text(node: &Value, out: &mut String) {
    let name = node
        .get("nodeName")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_ascii_uppercase();
    if matches!(name.as_str(), "SCRIPT" | "STYLE" | "NOSCRIPT" | "TEMPLATE") {
        return;
    }
    if node.get("nodeType").and_then(|t| t.as_i64()) == Some(3) {
        if let Some(v) = node.get("nodeValue").and_then(|v| v.as_str()) {
            out.push_str(v);
            out.push(' ');
        }
    }
    for key in ["children", "shadowRoots", "contentDocument"] {
        match node.get(key) {
            Some(Value::Array(kids)) => {
                for kid in kids {
                    gather_text(kid, out);
                }
            }
            Some(one @ Value::Object(_)) => gather_text(one, out),
            _ => {}
        }
    }
}

/// The text under the first element a selector matches.
async fn element_text(profile: &str, selector: &str) -> Result<String> {
    let node = *cdp::query_piercing(profile, selector)
        .await?
        .first()
        .ok_or_else(|| anyhow!("no element matches {selector}"))?;
    let described = cdp::page_call(
        profile,
        "DOM.describeNode",
        json!({ "nodeId": node, "depth": -1, "pierce": true }),
    )
    .await?;
    let mut text = String::new();
    if let Some(node) = described.get("node") {
        gather_text(node, &mut text);
    }
    Ok(text.split_whitespace().collect::<Vec<_>>().join(" "))
}

/// Where the page is now. Empty when the browser will not say.
async fn current_url(profile: &str) -> String {
    let Ok(hist) = cdp::page_call(profile, "Page.getNavigationHistory", json!({})).await else {
        return String::new();
    };
    let idx = hist.get("currentIndex").and_then(|v| v.as_i64()).unwrap_or(0);
    hist.get("entries")
        .and_then(|e| e.as_array())
        .and_then(|e| e.get(idx.max(0) as usize))
        .and_then(|e| e.get("url"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string()
}

/// Waits for a navigation to settle. Primarily the load event — but an instant
/// synthetic response (a `fulfill` rule) can fire it before the watcher gets to
/// poll, so also finish when document.readyState reaches "complete" at a URL
/// that is no longer `before`. Returns on either, or when the window closes (a
/// slow page still gets the whole timeout).
async fn await_load(
    profile: &str,
    wait: Option<cdp::EventWait>,
    before: &str,
    timeout: Duration,
) {
    let pid = profile.to_string();
    let was = before.to_string();
    let poll = async move {
        loop {
            tokio::time::sleep(Duration::from_millis(150)).await;
            let rs = cdp::page_call(
                &pid,
                "Runtime.evaluate",
                json!({ "expression": "document.readyState", "returnByValue": true }),
            )
            .await
            .ok()
            .and_then(|v| {
                v.get("result")
                    .and_then(|r| r.get("value"))
                    .and_then(|s| s.as_str().map(str::to_string))
            });
            if rs.as_deref() == Some("complete") && current_url(&pid).await != was {
                return;
            }
        }
    };
    match wait {
        Some(w) => {
            tokio::select! {
                _ = w.until("Page.loadEventFired", timeout) => {}
                _ = poll => {}
                _ = tokio::time::sleep(timeout) => {}
            }
        }
        None => {
            let _ = tokio::time::timeout(timeout, poll).await;
        }
    }
}

/// "Name: value" lines → the header-op array the core's Traffic rules take.
fn traffic_headers(text: &str) -> Vec<Value> {
    text.lines()
        .filter_map(|l| {
            let l = l.trim();
            if l.is_empty() {
                return None;
            }
            let (n, v) = l.split_once(':')?;
            Some(json!({ "name": n.trim(), "value": v.trim() }))
        })
        .collect()
}

/// The match half of a Traffic rule from a block's url/method/resource params.
fn traffic_match(p: &Value, vars: &HashMap<String, String>) -> Value {
    let mut m = serde_json::Map::new();
    for key in ["url", "host", "method", "resource"] {
        if let Some(raw) = param(p, key) {
            let v = expand(raw, vars);
            let v = v.trim();
            if !v.is_empty() && v != "any" {
                m.insert(key.to_string(), json!(v));
            }
        }
    }
    Value::Object(m)
}

/// Builds one Traffic rule ({match, action}) from a traffic.* block's params.
fn build_traffic_rule(kind: &str, p: &Value, vars: &HashMap<String, String>) -> Result<Value> {
    let g = |k: &str| param(p, k).map(|s| expand(s, vars)).unwrap_or_default();
    let mut a = serde_json::Map::new();
    match kind {
        "traffic.block" => {
            a.insert("type".into(), json!("block"));
            let reason = g("reason");
            if !reason.trim().is_empty() {
                a.insert("blockReason".into(), json!(reason.trim()));
            }
        }
        "traffic.redirect" => {
            let to = g("to");
            if to.trim().is_empty() {
                return Err(anyhow!("redirect needs a target address"));
            }
            a.insert("type".into(), json!("redirect"));
            a.insert("setUrl".into(), json!(to.trim()));
        }
        "traffic.setHeaders" => {
            a.insert("type".into(), json!("modify"));
            let hs = traffic_headers(&g("headers"));
            if !hs.is_empty() {
                a.insert("setHeaders".into(), json!(hs));
            }
            let body = g("setBody");
            if !body.is_empty() {
                a.insert("setBody".into(), json!(body));
            }
            let method = g("setMethod");
            if !method.trim().is_empty() {
                a.insert("setMethod".into(), json!(method.trim()));
            }
        }
        "traffic.editResponse" => {
            a.insert("type".into(), json!("modify"));
            if let Some(n) = param_f64(p, "status", vars) {
                a.insert("setStatus".into(), json!(n as i64));
            }
            let hs = traffic_headers(&g("responseHeaders"));
            if !hs.is_empty() {
                a.insert("setResponseHeaders".into(), json!(hs));
            }
            let body = g("responseBody");
            if !body.is_empty() {
                a.insert("setResponseBody".into(), json!(body));
            }
        }
        "traffic.fulfill" => {
            a.insert("type".into(), json!("fulfill"));
            if let Some(n) = param_f64(p, "status", vars) {
                a.insert("status".into(), json!(n as i64));
            }
            let hs = traffic_headers(&g("responseHeaders"));
            if !hs.is_empty() {
                a.insert("responseHeaders".into(), json!(hs));
            }
            let body = g("responseBody");
            if !body.is_empty() {
                a.insert("responseBody".into(), json!(body));
            }
        }
        _ => return Err(anyhow!("unknown traffic rule kind {kind}")),
    }
    Ok(json!({ "match": traffic_match(p, vars), "action": Value::Object(a) }))
}


/// How a sub-flow ended.
enum WalkEnd {
    /// It reached the end, or a step said Stop after succeeding, or the pass
    /// was ended from inside. All three mean "the flow finished".
    Ran,
    /// A step failed and its "when it fails" edge was Stop, so the failure is
    /// the flow's. The CALLER's own edge then decides what that means.
    Failed(anyhow::Error),
    /// The operator stopped the run while the flow was running.
    Stopped,
}

/// Walks a saved project's blocks as a sub-flow.
///
/// Deliberately not the worker's own loop: that one owns the worker's status,
/// its pass counter and its retry table, and a sub-flow borrowing them would
/// mark the CALLER's worker failed for a failure the caller may well handle.
/// The branch rules are the same; the bookkeeping is not.
async fn walk_flow(
    steps: &[automation::Block],
    entry: usize,
    bound: &mut Bound,
    vars: &mut HashMap<String, String>,
    run: &Run,
    ctx: &mut CallCtx,
) -> WalkEnd {
    let mut at = entry;
    let mut retries: HashMap<usize, u32> = HashMap::new();
    let mut hops: u32 = 0;

    while at < steps.len() {
        if run.stop.load(Ordering::Relaxed) {
            return WalkEnd::Stopped;
        }
        hops += 1;
        if hops > 10_000 {
            return WalkEnd::Failed(anyhow!("this flow jumped 10000 times without finishing"));
        }

        let block = &steps[at];
        let outcome = Box::pin(run_block(bound, block, vars, run, ctx)).await;
        let branch = match &outcome {
            Ok(Flow::Next) => block.on_done.clone(),
            Ok(Flow::EndPass) => automation::Branch::EndPass,
            Ok(Flow::Else) => block.on_fail.clone(),
            Err(e) => {
                run.log(format!("flow step {} — {e}", at + 1));
                block.on_fail.clone()
            }
        };

        match branch {
            automation::Branch::Next => {
                retries.remove(&at);
                at += 1;
            }
            automation::Branch::Stop => {
                return match outcome {
                    Err(e) => WalkEnd::Failed(e),
                    // Stop after a step that worked is how a flow says "done",
                    // not how it says "something went wrong".
                    _ => WalkEnd::Ran,
                };
            }
            automation::Branch::EndPass => return WalkEnd::Ran,
            automation::Branch::Goto(target) => {
                retries.remove(&at);
                match steps.iter().position(|b| b.id == target) {
                    Some(next) => at = next,
                    None => at += 1,
                }
            }
            automation::Branch::Retry(times) => {
                let used = retries.entry(at).or_insert(0);
                if *used < times {
                    *used += 1;
                    tokio::time::sleep(Duration::from_millis(600)).await;
                } else {
                    retries.remove(&at);
                    at += 1;
                }
            }
        }
    }
    WalkEnd::Ran
}

/// Reads a "name=value, other={{var}}" list into pairs, expanded.
fn name_value_list(raw: &str, vars: &HashMap<String, String>) -> Vec<(String, String)> {
    raw.split(|c| c == ',' || c == '\n')
        .filter_map(|part| part.split_once('='))
        .map(|(k, v)| (k.trim().to_string(), expand(v.trim(), vars)))
        .filter(|(k, _)| !k.is_empty())
        .collect()
}

fn name_list(raw: &str) -> Vec<String> {
    raw.split(|c| c == ',' || c == '\n')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}


/// Finds the project a `flow.call` names.
///
/// By id first and by name second, and never silently: an id is not portable
/// because import mints a fresh one, and a name is not unique because nothing
/// stops two projects sharing one. Both are stored so a bundle that travelled
/// still resolves, and an ambiguous or missing name is an error rather than a
/// guess — the wrong guess here runs somebody else's graph against a live
/// profile.
fn resolve_flow(run: &Run, id: &str, name: &str) -> Result<automation::Project> {
    if !id.is_empty() {
        if let Some(p) = run.projects.iter().find(|p| p.id == id) {
            return Ok(p.clone());
        }
    }
    if !name.is_empty() {
        let mut hits = run.projects.iter().filter(|p| p.name == name);
        match (hits.next(), hits.next()) {
            (Some(p), None) => return Ok(p.clone()),
            (Some(_), Some(_)) => {
                return Err(anyhow!(
                    "more than one project is called \"{name}\" — the call cannot tell which"
                ))
            }
            _ => {}
        }
    }
    Err(anyhow!(
        "the flow this step calls is not on this machine{}",
        if name.is_empty() { String::new() } else { format!(" (\"{name}\")") }
    ))
}

/// Runs a resolved project as a sub-flow. The frame is already entered.
async fn call_flow(
    bound: &mut Bound,
    vars: &mut HashMap<String, String>,
    run: &Run,
    ctx: &mut CallCtx,
    callee: &automation::Project,
    p: &Value,
) -> Result<Flow> {
    let mut steps: Vec<automation::Block> =
        callee.blocks.iter().filter(|b| b.enabled).cloned().collect();
    if steps.is_empty() {
        return Err(anyhow!("\"{}\" has no steps to run", callee.name));
    }
    // Same order the project runs in on its own: down the visual stack, by
    // column then height.
    steps.sort_by(|a, b| {
        let ca = (a.x / 40.0).round() as i64;
        let cb = (b.x / 40.0).round() as i64;
        ca.cmp(&cb).then(a.y.partial_cmp(&b.y).unwrap_or(std::cmp::Ordering::Equal))
    });

    // Where to start. A named entry is a block id — usually a flow.entry
    // marker, but any block will do. An entry that no longer exists is an
    // error, never a silent fall back to the top: running somebody's whole
    // graph when they asked for one piece of it is the worst available answer.
    let wanted_entry = expand(param(p, "entry").unwrap_or(""), vars);
    let entry = if wanted_entry.is_empty() {
        steps
            .iter()
            .position(|b| b.id == callee.run.start)
            .unwrap_or(0)
    } else {
        steps
            .iter()
            .position(|b| {
                b.id == wanted_entry
                    || (b.kind == "flow.entry"
                        && param(&b.params, "name").map(|n| n == wanted_entry).unwrap_or(false))
            })
            .ok_or_else(|| {
                anyhow!("\"{}\" has no entry called \"{wanted_entry}\"", callee.name)
            })?
    };

    // Variables. Shared by default, because they are the run's and a sub-flow
    // is a piece of the same run. Naming anything in "in" switches to a private
    // map: the flow then sees only what was handed to it, and only the names in
    // "out" come back. That is the shape a caller wants when the flow is a
    // reusable piece rather than an inlined section.
    let given = name_value_list(param(p, "in").unwrap_or(""), vars);
    let wanted_out = name_list(param(p, "out").unwrap_or(""));
    let isolated = !given.is_empty() || !wanted_out.is_empty();

    let mut inner_vars = if isolated {
        let mut m: HashMap<String, String> = HashMap::new();
        // The two the runner itself sets travel with the worker, not with the
        // graph, so a flow that logs "pass 3" is not lying.
        for k in ["thread", "pass"] {
            if let Some(v) = vars.get(k) {
                m.insert(k.to_string(), v.clone());
            }
        }
        for (k, v) in given {
            m.insert(check_var_name(&k)?, v);
        }
        m
    } else {
        vars.clone()
    };

    run.log(format!(
        "entering flow \"{}\"{}",
        callee.name,
        if wanted_entry.is_empty() { String::new() } else { format!(" at {wanted_entry}") }
    ));
    let end = Box::pin(walk_flow(&steps, entry, bound, &mut inner_vars, run, ctx)).await;

    // The outcome first. A flow that failed has no promises to keep, and
    // checking them here would report "finished without setting X" over the top
    // of the reason it actually stopped.
    match end {
        WalkEnd::Failed(e) => return Err(e),
        // The operator stopped the run; ending this pass is how that travels
        // back up through however many frames are above.
        WalkEnd::Stopped => return Ok(Flow::EndPass),
        WalkEnd::Ran => {}
    }

    if isolated {
        for name in wanted_out {
            match inner_vars.get(&name) {
                Some(v) => {
                    vars.insert(check_var_name(&name)?, v.clone());
                }
                // Silence here would read as "the flow returned an empty
                // string", which is a different and much harder bug to find.
                None => {
                    return Err(anyhow!(
                        "\"{}\" finished without setting \"{name}\"",
                        callee.name
                    ))
                }
            }
        }
    } else {
        *vars = inner_vars;
    }

    run.log(format!("left flow \"{}\"", callee.name));
    Ok(Flow::Next)
}

async fn run_block(
    bound: &mut Bound,
    block: &automation::Block,
    vars: &mut HashMap<String, String>,
    run: &Run,
    ctx: &mut CallCtx,
) -> Result<Flow> {
    let p = &block.params;

    // Before anything is interpreted: when this work is a module's, the places
    // where the block vocabulary is itself an escape are refused. One check,
    // here, because every page action in this file is inside this function or a
    // helper it calls — see modguard.
    if let Some(module) = ctx.on_behalf_of() {
        let data_dir = wasm::data_dir(module)?;
        crate::modguard::check(module, &block.kind, p, vars, &data_dir)?;
    }
    ctx.refuse_laundering(block)?;

    // A profile or proxy step reads and writes the space the run started in, or none.
    if block.kind.starts_with("profile.") || block.kind.starts_with("proxy.") {
        run.space.still()?;
    }

    // Profile, proxy, variable and file blocks touch no page, so they run
    // before the "is a browser attached" check below.
    match block.kind.as_str() {
        "profile.create" | "profile.temp" => {
            let temporary = block.kind == "profile.temp";
            let name = expand(param(p, "name").unwrap_or(""), vars);
            let folder = expand(
                param(p, "folder").unwrap_or(if temporary { "" } else { "Automation" }),
                vars,
            );
            let platform = param(p, "platform").map(|s| expand(s, vars));
            let inline = param(p, "proxy").map(|s| expand(s, vars));
            let id = make_profile(
                &name,
                &folder,
                platform.as_deref(),
                inline.as_deref(),
                temporary,
            )?;
            run.log(format!(
                "{} profile {id}{}",
                if temporary { "made a temporary" } else { "made" },
                if inline.is_some() { " with a proxy" } else { "" }
            ));
            // Switching profiles means the old browser is no longer this
            // worker's; let go of it rather than leaving a stale connection.
            if !bound.id.is_empty() && bound.id != id {
                cdp::detach(&bound.id);
            }
            // Remembered, not just bound: cleanup used to look at whatever was
            // FINALLY bound, so a graph that made a second temporary profile
            // left the first one behind for ever.
            if temporary {
                ctx.remember_temp(&id);
            }
            *bound = Bound { id: id.clone(), temporary, mobile: bound_is_mobile(&id) };
            if let Some(into) = param(p, "into") {
                vars.insert(check_var_name(into)?, id);
            }
            return Ok(Flow::Next);
        }
        "profile.use" => {
            let id = expand(param(p, "id").context("no profile id")?, vars);
            profile::load_raw(&id).with_context(|| format!("no profile {id}"))?;
            if !bound.id.is_empty() && bound.id != id {
                cdp::detach(&bound.id);
            }
            *bound = Bound { mobile: bound_is_mobile(&id), id, temporary: false };
            return Ok(Flow::Next);
        }
        "profile.keep" => {
            // A temporary profile the operator decided to hold on to.
            if bound.id.is_empty() {
                return Err(anyhow!("no profile to keep"));
            }
            let mut stored = profile::load_raw(&bound.id)?;
            stored.meta.temporary = false;
            if let Some(folder) = param(p, "folder") {
                stored.meta.folder = expand(folder, vars);
            }
            profile::save_raw(&mut stored)?;
            bound.temporary = false;
            run.log(format!("kept profile {}", bound.id));
            return Ok(Flow::Next);
        }
        "profile.delete" => {
            let id = param(p, "id").map(|s| expand(s, vars)).unwrap_or_else(|| bound.id.clone());
            if id.is_empty() {
                return Err(anyhow!("no profile to delete"));
            }
            let _ = process::Tracker::shared().kill(&id).await;
            cdp::detach(&id);
            profile::delete(&id)?;
            run.log(format!("deleted profile {id}"));
            if bound.id == id {
                *bound = Bound::default();
            }
            return Ok(Flow::Next);
        }
        "proxy.set" => {
            if bound.id.is_empty() {
                return Err(anyhow!("no profile to give a proxy to"));
            }
            let line = expand(param(p, "proxy").context("no proxy")?, vars);
            let entry = proxy::parse_single(&line)
                .ok_or_else(|| anyhow!("cannot read proxy \"{line}\""))?;
            let mut stored = profile::load_raw(&bound.id)?;
            // On the profile only. The operator's proxy library is theirs.
            stored.meta.inline_proxy = Some(entry);
            stored.meta.proxy_id = None;
            profile::save_raw(&mut stored)?;
            run.log("attached a proxy to the profile");
            return Ok(Flow::Next);
        }
        "proxy.generate" => {
            if bound.id.is_empty() {
                return Err(anyhow!("no profile to give a proxy to"));
            }
            let order = param(p, "order").map(|s| expand(s, vars));
            let country = param(p, "country").map(|s| expand(s, vars).to_uppercase());
            let line = crate::psapi::pick_proxy(order.as_deref(), country.as_deref()).await?;
            let entry = proxy::parse_single(&line)
                .ok_or_else(|| anyhow!("ProxyShard returned something unreadable"))?;
            let mut stored = profile::load_raw(&bound.id)?;
            stored.meta.inline_proxy = Some(entry);
            stored.meta.proxy_id = None;
            profile::save_raw(&mut stored)?;
            run.log("took a proxy from ProxyShard and put it on the profile");
            if let Some(into) = param(p, "into") {
                vars.insert(check_var_name(into)?, line);
            }
            return Ok(Flow::Next);
        }
        "proxy.residential" => {
            if bound.id.is_empty() {
                return Err(anyhow!("no profile to give a proxy to"));
            }
            let ex = |k: &str| {
                param(p, k)
                    .map(|s| expand(s, vars))
                    .filter(|s| !s.trim().is_empty())
            };
            // A selection field may hold several comma-separated choices (or
            // "any"): pick one at random for this run, so a fleet spreads across
            // them. Empty or "any" means "let the network decide".
            let pick = |k: &str| -> Option<String> {
                let raw = param(p, k).map(|s| expand(s, vars)).unwrap_or_default();
                let opts: Vec<String> = raw
                    .split(',')
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty() && !s.eq_ignore_ascii_case("any"))
                    .collect();
                if opts.is_empty() {
                    return None;
                }
                let idx = (uuid::Uuid::new_v4().as_bytes()[0] as usize) % opts.len();
                Some(opts[idx].clone())
            };
            let tier = ex("plan").unwrap_or_else(|| "standart".to_string());
            let country = pick("country");
            let region = pick("region");
            let city = pick("city");
            let os = pick("os");
            let isp = pick("isp");
            let relay = ex("relay");
            // Sticky by default: a profile that changes exit address mid-run
            // looks like a hijacked session to anything watching. "rotating" is
            // taken too — it is what the generator card calls dynamic.
            let sticky = ex("session")
                .map(|v| !(v.eq_ignore_ascii_case("dynamic") || v.eq_ignore_ascii_case("rotating")))
                .unwrap_or(true);
            let session_mode = ex("session_mode");
            let http = ex("protocol").map(|v| v.eq_ignore_ascii_case("http")).unwrap_or(false);
            // Any refusal here — wrong plan for an OS, no traffic left — comes
            // back as a step failure, which stops the run by default and names
            // the reason in the log.
            let entry = crate::psapi::residential_proxy(crate::psapi::ResiRequest {
                tier: &tier,
                country: country.as_deref(),
                region: region.as_deref(),
                city: city.as_deref(),
                os: os.as_deref(),
                isp: isp.as_deref(),
                sticky,
                session_mode: session_mode.as_deref(),
                http,
                relay: relay.as_deref(),
            })
            .await?;
            let line = format!(
                "{}:{}:{}:{}",
                entry.host, entry.port, entry.username, entry.password
            );
            let where_ = entry.country.clone();
            let host = entry.host.clone();
            let mut stored = profile::load_raw(&bound.id)?;
            stored.meta.inline_proxy = Some(entry);
            stored.meta.proxy_id = None;
            profile::save_raw(&mut stored)?;
            run.log(format!(
                "residential proxy on the profile — {tier}{} via {host}",
                if where_.is_empty() { String::new() } else { format!(" in {where_}") }
            ));
            if let Some(into) = param(p, "into") {
                vars.insert(check_var_name(into)?, line);
            }
            return Ok(Flow::Next);
        }
        "profile.read" => {
            if bound.id.is_empty() {
                return Err(anyhow!("no profile to read from"));
            }
            let path = expand(param(p, "path").context("no field")?, vars);
            let stored = profile::load_raw(&bound.id)?;
            let whole = serde_json::to_value(&stored)?;
            // A dotted path into the profile as it is stored, so everything it
            // carries is reachable: navigator.user_agent, screen.width,
            // timezone.id, client_hints.platform_version, _meta.folder.
            let mut cur = &whole;
            for part in path.split('.') {
                cur = cur
                    .get(part)
                    .ok_or_else(|| anyhow!("this profile has no \"{path}\""))?;
            }
            let value = match cur {
                Value::String(s) => s.clone(),
                Value::Null => String::new(),
                other => other.to_string(),
            };
            if let Some(into) = param(p, "into") {
                vars.insert(check_var_name(into)?, value);
            }
            return Ok(Flow::Next);
        }
        "var.autofill" => {
            if bound.id.is_empty() {
                return Err(anyhow!("no profile to read a person from"));
            }
            let field = expand(param(p, "field").context("no field")?, vars);
            let bus = crate::bus().await.map_err(|e| anyhow!(e))?;
            let offer = bus
                .helper_fields(&bound.id)
                // The generated person is offered by the fill helper, which
                // only speaks up once a page shows it a form. Saying so beats
                // handing back an empty string that looks like a real answer.
                .ok_or_else(|| anyhow!("the fill helper has nothing to offer yet — open a page with a form first"))?;
            let value = offer
                .get("fields")
                .and_then(|f| f.as_array())
                .and_then(|list| {
                    list.iter().find(|f| {
                        f.get("kind").and_then(|k| k.as_str()) == Some(field.as_str())
                    })
                })
                .and_then(|f| f.get("value"))
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
                .ok_or_else(|| anyhow!("this person has no \"{field}\""))?;
            if let Some(into) = param(p, "into") {
                vars.insert(check_var_name(into)?, value);
            }
            return Ok(Flow::Next);
        }
        // "setVar" is what this step was called before the var.* group; projects
        // saved then still carry it, and a module may still emit it.
        "var.set" | "setVar" => {
            let name = check_var_name(param(p, "name").context("no name")?)?;
            vars.insert(name, expand(param(p, "value").unwrap_or(""), vars));
            return Ok(Flow::Next);
        }
        "var.math" => {
            let op = param(p, "op").unwrap_or("+");
            let a = as_number(vars, param(p, "a").unwrap_or("0"))?;
            let b = as_number(vars, param(p, "b").unwrap_or("0"))?;
            let out = tidy(arithmetic(op, a, b)?);
            let into = check_var_name(param(p, "into").context("no name to save into")?)?;
            run.log(format!("{into} = {} {op} {} = {out}", tidy(a), tidy(b)));
            vars.insert(into, out);
            return Ok(Flow::Next);
        }
        "http.request" => {
            let url = expand(param(p, "url").context("no address")?, vars);
            let method = param(p, "method").unwrap_or("GET").to_string();
            let into = check_var_name(param(p, "into").context("no name to save into")?)?;
            // "Name: value" per line, the way a request is written down.
            let headers: Vec<(String, String)> = expand(param(p, "headers").unwrap_or(""), vars)
                .lines()
                .filter_map(|l| l.split_once(':'))
                .map(|(k, v)| (k.trim().to_string(), v.trim().to_string()))
                .filter(|(k, _)| !k.is_empty())
                .collect();
            let body = param(p, "body").map(|b| expand(b, vars)).filter(|b| !b.is_empty());
            let fingerprint = param(p, "fingerprint").map(|f| expand(f, vars));
            let session = param(p, "session").map(|x| expand(x, vars));

            // Out through the profile's own proxy unless told otherwise: a
            // call that leaves on the host address beside a browser that did
            // not is two visitors, and the site can see both.
            let via = param(p, "via").unwrap_or("profile");
            let proxy = match via {
                "host" => None,
                "profile" if !bound.id.is_empty() => {
                    run.space.still()?;
                    let stored = profile::load_raw(&bound.id)?;
                    let bound_proxy = stored
                        .meta
                        .proxy_id
                        .as_deref()
                        .and_then(|id| proxy::get(id).ok().flatten())
                        .or(stored.meta.inline_proxy);
                    match bound_proxy {
                        Some(pr) => Some(pr.to_proxy_server_arg()),
                        None => None,
                    }
                }
                "profile" => None,
                other => Some(expand(other, vars)),
            };

            let out = crate::requests::send(crate::requests::Request {
                method,
                url: url.clone(),
                headers,
                body,
                fingerprint,
                session,
                proxy,
                timeout_s: param_f64(p, "timeout", vars).unwrap_or(30.0),
            })
            .await?;

            let status = out.get("status").and_then(|v| v.as_u64()).unwrap_or(0);
            run.log(format!("{status} from {url}"));
            // Three names off one: the body under the name asked for, and the
            // status and headers beside it, because a step that answered only
            // with the body would need a second request to check it worked.
            vars.insert(
                format!("{into}_status"),
                status.to_string(),
            );
            vars.insert(
                format!("{into}_headers"),
                out.get("headers").map(|h| h.to_string()).unwrap_or_default(),
            );
            vars.insert(
                into,
                out.get("body")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
            );
            return Ok(Flow::Next);
        }
        "http.endSession" => {
            let name = expand(param(p, "session").context("no session to end")?, vars);
            crate::requests::drop_session(&name);
            run.log(format!("session {name} forgotten"));
            return Ok(Flow::Next);
        }
        "var.random" => {
            let into = check_var_name(param(p, "into").context("no name to save into")?)?;
            let list = expand(param(p, "list").unwrap_or(""), vars);
            let picked = if !list.trim().is_empty() {
                // A list wins over the range: an operator who filled both meant
                // the list, which is the more specific of the two.
                let items: Vec<&str> = list
                    .split(&[',', '\n'][..])
                    .map(|s| s.trim())
                    .filter(|s| !s.is_empty())
                    .collect();
                if items.is_empty() {
                    return Err(anyhow!("the list is empty"));
                }
                let i = (uuid::Uuid::new_v4().as_bytes()[0] as usize) % items.len();
                items[i].to_string()
            } else {
                let lo = as_number(vars, param(p, "min").unwrap_or("0"))?;
                let hi = as_number(vars, param(p, "max").unwrap_or("100"))?;
                let (lo, hi) = if lo <= hi { (lo, hi) } else { (hi, lo) };
                // 16 bits of the uuid is plenty for a wait or an index and
                // keeps this free of another dependency.
                let b = uuid::Uuid::new_v4();
                let raw = u16::from_le_bytes([b.as_bytes()[0], b.as_bytes()[1]]) as f64 / 65535.0;
                tidy(lo + (hi - lo) * raw)
            };
            run.log(format!("{into} = {picked}"));
            vars.insert(into, picked);
            return Ok(Flow::Next);
        }
        "file.readLine" => {
            let path = expand(param(p, "path").context("no file")?, vars);
            let into = check_var_name(param(p, "into").context("no name to save into")?)?;
            let text = std::fs::read_to_string(&path).with_context(|| format!("read {path}"))?;
            let mut lines = text.lines();
            let line = loop {
                match lines.next() {
                    Some(l) if l.trim().is_empty() => continue,
                    Some(l) => break l.trim().to_string(),
                    None => return Err(anyhow!("{path} has no lines left")),
                }
            };
            // Taking a line means taking it: a list of accounts read without
            // removal hands the same one to every thread, which is how a run
            // ends up logging into a single account forty times over. The file
            // is rewritten whole because these lists are small and a partial
            // rewrite that dies halfway loses the lot.
            if param(p, "take").map(|v| v != "keep").unwrap_or(true) {
                let mut rest: Vec<&str> = Vec::new();
                let mut dropped = false;
                for l in text.lines() {
                    if !dropped && l.trim() == line {
                        dropped = true;
                        continue;
                    }
                    rest.push(l);
                }
                std::fs::write(&path, rest.join("\n"))
                    .with_context(|| format!("write {path}"))?;
            }
            vars.insert(into, line);
            return Ok(Flow::Next);
        }
        "file.append" => {
            let path = expand(param(p, "path").context("no file")?, vars);
            let line = expand(param(p, "line").unwrap_or(""), vars);
            use std::io::Write;
            if let Some(dir) = std::path::Path::new(&path).parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            let mut f = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&path)
                .with_context(|| format!("open {path}"))?;
            writeln!(f, "{line}")?;
            return Ok(Flow::Next);
        }
        "db.open" => {
            let name = expand(param(p, "name").unwrap_or("default"), vars);
            let driver = expand(param(p, "driver").unwrap_or("sqlite"), vars);
            // "path" for SQLite, "target" (connection string) for the rest —
            // accept either field so the palette can label it per driver.
            let target = param(p, "target")
                .or_else(|| param(p, "path"))
                .map(|s| expand(s, vars))
                .unwrap_or_default();
            let database = param(p, "database").map(|s| expand(s, vars));
            tokio::task::spawn_blocking(move || crate::db::open(&name, &driver, &target, database.as_deref()))
                .await
                .map_err(|e| anyhow!("db task failed: {e}"))??;
            run.log("opened a database");
            return Ok(Flow::Next);
        }
        "db.exec" => {
            let name = expand(param(p, "name").unwrap_or("default"), vars);
            let sql = expand(param(p, "sql").context("no sql")?, vars);
            let out = tokio::task::spawn_blocking(move || crate::db::exec(&name, &sql))
                .await
                .map_err(|e| anyhow!("db task failed: {e}"))??;
            if let Some(into) = param(p, "into") {
                let changes = out.get("changes").map(|v| v.to_string()).unwrap_or_default();
                vars.insert(check_var_name(into)?, changes);
            }
            return Ok(Flow::Next);
        }
        "db.query" => {
            let name = expand(param(p, "name").unwrap_or("default"), vars);
            let sql = expand(param(p, "sql").context("no sql")?, vars);
            let (cols, rows) = tokio::task::spawn_blocking(move || crate::db::query(&name, &sql))
                .await
                .map_err(|e| anyhow!("db task failed: {e}"))??;
            let n = rows.len();
            let mode = param(p, "mode").map(|s| expand(s, vars)).unwrap_or_else(|| "rows".into());
            let value = match mode.as_str() {
                "count" => n.to_string(),
                "value" => rows
                    .first()
                    .and_then(|r| r.as_object())
                    .and_then(|o| cols.first().and_then(|c| o.get(c)))
                    .map(crate::db::scalar_to_string)
                    .unwrap_or_default(),
                "row" => rows.first().map(|r| r.to_string()).unwrap_or_else(|| "null".into()),
                _ => serde_json::Value::Array(rows).to_string(),
            };
            if let Some(into) = param(p, "into") {
                vars.insert(check_var_name(into)?, value);
            }
            run.log(format!("query returned {n} row(s)"));
            return Ok(Flow::Next);
        }
        "db.close" => {
            let name = expand(param(p, "name").unwrap_or("default"), vars);
            crate::db::close(&name);
            return Ok(Flow::Next);
        }
        "if.value" => {
            // Compares two values (variables, text, numbers). True takes "when
            // it works"; false takes "when it fails" (the else branch).
            let a = expand(param(p, "a").unwrap_or(""), vars);
            let op = param(p, "op").map(|s| expand(s, vars)).unwrap_or_else(|| "=".into());
            let b = expand(param(p, "b").unwrap_or(""), vars);
            let holds = cond_holds(&a, &op, &b);
            run.log(format!(
                "if {a:?} {op} {b:?} → {}",
                if holds { "yes" } else { "no (else)" }
            ));
            return Ok(if holds { Flow::Next } else { Flow::Else });
        }
        // A marker, not a step: it names a place in a project that something
        // else can call into. Running past it does nothing, so a project runs
        // standalone exactly as it did before anyone called it.
        "flow.entry" => return Ok(Flow::Next),
        // Runs a saved project as a piece of this one.
        //
        // Above the browser check on purpose: the first thing a called flow
        // usually does is make its own profile, and a sub-flow that could only
        // be called once a browser existed would be useless for the one job
        // everybody wants it for.
        "flow.call" => {
            let wanted_id = expand(param(p, "project").unwrap_or(""), vars);
            let wanted_name = expand(param(p, "projectName").unwrap_or(""), vars);
            let callee = resolve_flow(run, &wanted_id, &wanted_name)?;
            ctx.may_call(&Frame::Flow(callee.id.clone()))?;

            // Enter before anything else: depth, loops and the shared action
            // budget are all decided here, and `leave` has to run on every path
            // out, including an error.
            let saved = ctx.enter(Frame::Flow(callee.id.clone()))?;
            let result = call_flow(bound, vars, run, ctx, &callee, p).await;
            ctx.leave(saved);
            return result;
        }
        _ => {}
    }

    // Everything below drives a page, so there must be a browser by now. A
    // profile block has to be reached before this one — put it above this step
    // in the stack (the run follows the stack top-to-bottom).
    if bound.id.is_empty() {
        return Err(anyhow!(
            "this step needs a profile — put a profile block above it"
        ));
    }
    ensure_browser(bound, run).await?;
    let profile = bound.id.as_str();

    match block.kind.as_str() {
        "goto" => {
            let url = expand(param(p, "url").context("no address")?, vars);
            let before = current_url(profile).await;
            let wait = cdp::watch(profile);
            cdp::page_call(profile, "Page.navigate", json!({ "url": url })).await?;
            await_load(profile, wait, &before, Duration::from_secs(30)).await;
        }
        "proxy.swap" => {
            // Hot-swap the profile's proxy through the core, no restart. Empty
            // proxy = direct. Full URL scheme://user:pass@host:port.
            let proxy = param(p, "proxy").map(|s| expand(s, vars)).unwrap_or_default();
            let args = if proxy.trim().is_empty() {
                json!({})
            } else {
                json!({ "proxy": proxy })
            };
            cdp::page_call(profile, "Traffic.setProxy", args).await?;
            run.log("swapped the proxy");
            return Ok(Flow::Next);
        }
        "script.run" => {
            // Inject the operator's JS through the core's Script domain (isolated
            // world by default), not a page-visible CDP evaluate. Source comes
            // inline or from a file.
            let file = param(p, "file").map(|s| expand(s, vars)).unwrap_or_default();
            let source = if !file.trim().is_empty() {
                std::fs::read_to_string(file.trim())
                    .with_context(|| format!("reading script {}", file.trim()))?
            } else {
                expand(param(p, "source").unwrap_or(""), vars)
            };
            if source.trim().is_empty() {
                return Err(anyhow!("script.run has no source and no file"));
            }
            let mut args = json!({ "source": source });
            if param(p, "world").map(|s| expand(s, vars)).as_deref() == Some("main") {
                args["world"] = json!("main");
            }
            let out = cdp::page_call(profile, "Script.run", args).await?;
            let result = out
                .get("result")
                .and_then(|v| v.as_str())
                .unwrap_or("null")
                .to_string();
            if let Some(into) = param(p, "into") {
                vars.insert(check_var_name(into)?, result);
            }
            run.log("ran a script");
        }
        "traffic.observe" => {
            // Start (or stop) capturing requests so the Traffic tab can show
            // them. On by default; pass "off" to stop.
            let on = param(p, "enabled").map(|s| expand(s, vars)).as_deref() != Some("off");
            cdp::page_call(profile, "Traffic.observe", json!({ "enabled": on })).await?;
            run.log(if on { "watching requests" } else { "stopped watching requests" });
            return Ok(Flow::Next);
        }
        "traffic.block" | "traffic.redirect" | "traffic.setHeaders"
        | "traffic.editResponse" | "traffic.fulfill" => {
            // Mount one interception rule live, at this point in the flow. It
            // takes effect for requests made after it — put it before the step
            // that triggers them. Adds to whatever the project already mounts.
            let rule = build_traffic_rule(&block.kind, p, vars)?;
            let rules_json = serde_json::to_string(&vec![rule]).unwrap_or_else(|_| "[]".into());
            cdp::page_call(profile, "Traffic.mount", json!({ "rules": rules_json })).await?;
            run.log("mounted an interception rule");
            return Ok(Flow::Next);
        }
        "reload" => {
            let wait = cdp::watch(profile);
            cdp::page_call(profile, "Page.reload", json!({})).await?;
            if let Some(w) = wait {
                w.until("Page.loadEventFired", Duration::from_secs(30)).await;
            }
        }
        "back" | "forward" => {
            let hist = cdp::page_call(profile, "Page.getNavigationHistory", json!({})).await?;
            let idx = hist.get("currentIndex").and_then(|v| v.as_i64()).unwrap_or(0);
            let entries = hist
                .get("entries")
                .and_then(|e| e.as_array())
                .cloned()
                .unwrap_or_default();
            let want = if block.kind == "back" { idx - 1 } else { idx + 1 };
            if want < 0 || want as usize >= entries.len() {
                run.log(format!("{}: nowhere to go", block.kind));
                return Ok(Flow::Next);
            }
            let id = entries[want as usize].get("id").cloned().unwrap_or(Value::Null);
            cdp::page_call(profile, "Page.navigateToHistoryEntry", json!({ "entryId": id })).await?;
        }
        "waitLoad" => {
            let secs = param_f64(p, "timeout", vars).unwrap_or(30.0);
            if let Some(w) = cdp::watch(profile) {
                w.until("Page.loadEventFired", Duration::from_secs_f64(secs)).await;
            }
        }
        // Gestures with no desktop twin. Refused on a desktop profile rather
        // than approximated: the core will not deliver a touch to a window
        // that reports no touchscreen, and a silent no-op would look like the
        // step worked.
        "touch.tap" | "touch.longPress" | "touch.swipe" | "touch.drag" | "touch.pinch" => {
            if !bound.mobile {
                return Err(anyhow!(
                    "\"{}\" is a finger gesture and this profile has no touchscreen — \
                     bind a phone profile, or use the Pointer steps",
                    block.kind
                ));
            }
            let (x, y) = match param(p, "selector") {
                Some(sel) if !sel.trim().is_empty() => {
                    center_waiting(profile, &expand(sel, vars), wait_for(p, vars)).await?
                }
                _ => match (param_f64(p, "x", vars), param_f64(p, "y", vars)) {
                    (Some(x), Some(y)) => (x, y),
                    _ => viewport_center(profile).await,
                },
            };
            touch_front(profile).await;
            if block.kind == "touch.drag" {
                let to = expand(param(p, "to").context("no element to drag onto")?, vars);
                let (tx, ty) = center_waiting(profile, &to, wait_for(p, vars)).await?;
                let mut args = json!({ "fromX": x, "fromY": y, "toX": tx, "toY": ty });
                if let Some(hold) = param_f64(p, "holdMs", vars) {
                    args["holdMs"] = json!(hold);
                }
                cdp::page_call(profile, "Motion.touchDrag", args).await?;
                return Ok(Flow::Next);
            }
            match block.kind.as_str() {
                "touch.tap" => {
                    let taps = param_f64(p, "tapCount", vars).unwrap_or(1.0).clamp(1.0, 3.0) as i64;
                    cdp::page_call(profile, "Motion.touchTap",
                                   json!({ "x": x, "y": y, "tapCount": taps })).await?;
                }
                "touch.longPress" => {
                    cdp::page_call(profile, "Motion.touchLongPress",
                                   json!({ "x": x, "y": y })).await?;
                }
                "touch.swipe" => {
                    let dx = param_f64(p, "dx", vars).unwrap_or(0.0);
                    let dy = param_f64(p, "dy", vars).unwrap_or(0.0);
                    if dx == 0.0 && dy == 0.0 {
                        return Err(anyhow!("a swipe of nothing goes nowhere — set how far"));
                    }
                    cdp::page_call(profile, "Motion.touchSwipe",
                                   json!({ "fromX": x, "fromY": y,
                                           "toX": x + dx, "toY": y + dy })).await?;
                }
                _ => {
                    let scale = param_f64(p, "scale", vars).unwrap_or(2.0);
                    if scale <= 0.0 {
                        return Err(anyhow!("a pinch scales by a positive factor"));
                    }
                    cdp::page_call(profile, "Motion.pinch",
                                   json!({ "x": x, "y": y, "scale": scale })).await?;
                }
            }
        }
        // Turning the handset. Physical and refused on a desktop profile:
        // the two screen numbers are traded only for a phone, so elsewhere the
        // angle would move while screen.width stood still.
        "screen.rotate" => {
            if !bound.mobile {
                return Err(anyhow!(
                    "only a phone profile can be turned — this one claims a desktop screen"
                ));
            }
            let angle = param_f64(p, "angle", vars).unwrap_or(90.0) as i64;
            if !matches!(angle, 0 | 90 | 180 | 270) {
                return Err(anyhow!(
                    "a screen turns to 0, 90, 180 or 270 degrees, not {angle}"
                ));
            }
            let mut args = json!({ "angle": angle });
            if let Some(ms) = param_f64(p, "turnMs", vars) {
                args["turnMs"] = json!(ms);
            }
            let r = cdp::page_call(profile, "Motion.setOrientation", args).await?;
            // Optional, unlike readText's: the turn is the point, and the name
            // is only for a project that branches on portrait vs landscape.
            if let Some(into) = param(p, "into").filter(|s| !s.trim().is_empty()) {
                let t = r
                    .get("type")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string();
                vars.insert(check_var_name(into)?, t);
            }
        }
        "click" | "doubleClick" | "rightClick" | "hover" => {
            let (x, y) = match param(p, "selector") {
                Some(sel) => center_waiting(profile, &expand(sel, vars), wait_for(p, vars)).await?,
                None => (
                    param_f64(p, "x", vars).context("no target")?,
                    param_f64(p, "y", vars).context("no target")?,
                ),
            };
            // One palette, two bodies: the operator says WHAT, the runner picks
            // WHAT WITH from the bound profile. The core refuses a pointer on a
            // handset outright, so on such a profile every one of these has to
            // reach for a finger.
            if bound.mobile {
                match block.kind.as_str() {
                    // A phone has no cursor and nothing to hover with. Refused
                    // loudly rather than skipped: a project whose menu opens on
                    // hover must stop, not pretend it worked.
                    "hover" => {
                        return Err(anyhow!(
                            "this profile is a phone and has no cursor — replace \"hover\"                              with a tap or a long press"
                        ))
                    }
                    // The phone's context menu IS a long press.
                    "rightClick" => {
                        touch_front(profile).await;
                        cdp::page_call(profile, "Motion.touchLongPress",
                                       json!({ "x": x, "y": y })).await?;
                    }
                    other => {
                        touch_front(profile).await;
                        let taps = if other == "doubleClick" { 2 } else { 1 };
                        cdp::page_call(profile, "Motion.touchTap",
                                       json!({ "x": x, "y": y, "tapCount": taps })).await?;
                    }
                }
            } else if block.kind == "hover" {
                cdp::page_call(profile, "Motion.createPointer", json!({ "x": x, "y": y })).await?;
                cdp::page_call(profile, "Motion.glideTo", json!({ "x": x, "y": y })).await?;
            } else {
                let b = match block.kind.as_str() {
                    "doubleClick" => "double",
                    "rightClick" => "right",
                    _ => "left",
                };
                click_at(profile, x, y, b).await?;
            }
        }
        "drag" | "swipe" => {
            let from = expand(param(p, "selector").unwrap_or(""), vars);
            // The starting point: an element if one is named, otherwise the
            // middle of the viewport, which is what a swipe on a page means.
            let (x0, y0) = if from.trim().is_empty() {
                viewport_center(profile).await
            } else {
                center_waiting(profile, &from, wait_for(p, vars)).await?
            };
            let (x1, y1) = if block.kind == "drag" {
                let to = expand(param(p, "to").context("no element to drag onto")?, vars);
                center_waiting(profile, &to, wait_for(p, vars)).await?
            } else {
                let dx = param_f64(p, "dx", vars).unwrap_or(0.0);
                let dy = param_f64(p, "dy", vars).unwrap_or(0.0);
                if dx == 0.0 && dy == 0.0 {
                    return Err(anyhow!("a swipe of nothing goes nowhere — set how far"));
                }
                (x0 + dx, y0 + dy)
            };
            if bound.mobile {
                touch_front(profile).await;
                // A drag is a HOLD and then a move; a swipe is not. Sending one
                // for the other scrolls a sortable list instead of picking a
                // card up, so they are different commands here too.
                if block.kind == "drag" {
                    let mut args = json!({ "fromX": x0, "fromY": y0, "toX": x1, "toY": y1 });
                    if let Some(hold) = param_f64(p, "holdMs", vars) {
                        args["holdMs"] = json!(hold);
                    }
                    cdp::page_call(profile, "Motion.touchDrag", args).await?;
                } else {
                    cdp::page_call(profile, "Motion.touchSwipe",
                                   json!({ "fromX": x0, "fromY": y0, "toX": x1, "toY": y1 })).await?;
                }
                return Ok(Flow::Next);
            }
            let button = param(p, "button").unwrap_or("left");
            cdp::page_call(profile, "Motion.createPointer", json!({ "x": x0, "y": y0 })).await?;
            // Glide to the start first: the press has to land on the thing
            // being dragged, and createPointer only says where the hand is.
            cdp::page_call(profile, "Motion.glideTo", json!({ "x": x0, "y": y0 })).await?;
            cdp::page_call(
                profile,
                "Motion.dragTo",
                json!({ "x": x1, "y": y1, "button": button }),
            )
            .await?;
        }
        "scroll" => {
            let dy = param_f64(p, "deltaY", vars).unwrap_or(0.0);
            let dx = param_f64(p, "deltaX", vars).unwrap_or(0.0);
            if dx == 0.0 && dy == 0.0 {
                return Ok(Flow::Next);
            }
            // A wheel scrolls whatever is under the pointer, so where the
            // pointer is decides WHICH scroller moves. Over an element when one
            // is named, otherwise the middle of the actual viewport — a fixed
            // 400,400 landed outside a small window and inside a sidebar in a
            // wide one.
            let (px, py) = match param(p, "selector") {
                Some(sel) => center_waiting(profile, &expand(sel, vars), wait_for(p, vars)).await?,
                None => viewport_center(profile).await,
            };
            if bound.mobile {
                // A finger scrolls by dragging the content, so it travels
                // AGAINST the scroll — and it cannot travel further than the
                // glass. Long scrolls are cut into strokes, the way a hand
                // swipes several times.
                touch_front(profile).await;
                let (vw, vh) = viewport_size(profile).await;
                let max_x = (vw * 0.6).max(40.0);
                let max_y = (vh * 0.6).max(40.0);
                let mut left_x = dx;
                let mut left_y = dy;
                // A ceiling on the strokes, so a runaway parameter cannot pin
                // the worker to one step for ever.
                for _ in 0..40 {
                    if left_x.abs() < 1.0 && left_y.abs() < 1.0 {
                        break;
                    }
                    let step_x = left_x.clamp(-max_x, max_x);
                    let step_y = left_y.clamp(-max_y, max_y);
                    cdp::page_call(
                        profile,
                        "Motion.touchSwipe",
                        json!({ "fromX": px, "fromY": py,
                                "toX": px - step_x, "toY": py - step_y,
                                // A replayed scroll must land the distance it
                                // was given; a flick adds a fling on top.
                                "flick": false }),
                    )
                    .await?;
                    left_x -= step_x;
                    left_y -= step_y;
                    tokio::time::sleep(Duration::from_millis(120)).await;
                }
                return Ok(Flow::Next);
            }
            cdp::page_call(profile, "Motion.createPointer", json!({ "x": px, "y": py })).await?;
            let mut args = json!({ "deltaY": dy });
            if dx != 0.0 {
                args["deltaX"] = json!(dx);
            }
            cdp::page_call(profile, "Motion.wheel", args).await?;
        }
        "type" => {
            if let Some(sel) = param(p, "selector") {
                let (x, y) = center_waiting(profile, &expand(sel, vars), wait_for(p, vars)).await?;
                if bound.mobile {
                    touch_front(profile).await;
                    cdp::page_call(profile, "Motion.touchTap", json!({ "x": x, "y": y })).await?;
                } else {
                    click_at(profile, x, y, "left").await?;
                }
            }
            let text = expand(param(p, "text").unwrap_or(""), vars);
            if !text.is_empty() {
                cdp::page_call(profile, "Motion.enterText", json!({ "text": text })).await?;
            }
        }
        "press" => {
            let key = expand(param(p, "key").context("no key")?, vars);
            let mods: Vec<String> = param(p, "modifiers")
                .map(|m| {
                    m.split(|c: char| c == ',' || c == '+')
                        .map(|s| s.trim().to_string())
                        .filter(|s| !s.is_empty())
                        .collect()
                })
                .unwrap_or_default();
            if bound.mobile && !mods.is_empty() {
                // A soft keyboard has no Control, Alt or Command. A chord sent
                // to one is a key combination the device cannot produce.
                return Err(anyhow!(
                    "a phone's keyboard has no modifier keys — \"{}\" cannot be pressed with {}",
                    key,
                    mods.join("+")
                ));
            }
            let mut args = json!({ "key": key });
            if !mods.is_empty() {
                args["modifiers"] = json!(mods);
            }
            cdp::page_call(profile, "Motion.pressKey", args).await?;
        }
        "clear" => {
            if let Some(sel) = param(p, "selector") {
                let (x, y) = center_waiting(profile, &expand(sel, vars), wait_for(p, vars)).await?;
                if bound.mobile {
                    touch_front(profile).await;
                    cdp::page_call(profile, "Motion.touchTap", json!({ "x": x, "y": y })).await?;
                } else {
                    click_at(profile, x, y, "left").await?;
                }
            }
            if bound.mobile {
                // Select-all is Accel+A, and a phone has neither key. A finger
                // clears a field by holding Backspace, so that is what this
                // does — bounded, because a field of unknown length must not
                // become an unbounded loop.
                let len = param_f64(p, "count", vars).unwrap_or(120.0).clamp(1.0, 200.0) as usize;
                for _ in 0..len {
                    cdp::page_call(profile, "Motion.pressKey",
                                   json!({ "key": "Backspace" })).await?;
                }
            } else {
                cdp::page_call(
                    profile,
                    "Motion.pressKey",
                    json!({ "key": "a", "modifiers": ["Accel"] }),
                )
                .await?;
                cdp::page_call(profile, "Motion.pressKey", json!({ "key": "Backspace" })).await?;
            }
        }
        "wait" => {
            let secs = param_f64(p, "seconds", vars).unwrap_or(1.0).clamp(0.0, 3600.0);
            tokio::time::sleep(Duration::from_secs_f64(secs)).await;
        }
        "waitFor" => {
            let sel = expand(param(p, "selector").context("no element")?, vars);
            let secs = param_f64(p, "timeout", vars).unwrap_or(15.0);
            let deadline = Instant::now() + Duration::from_secs_f64(secs);
            loop {
                if exists(profile, &sel).await {
                    break;
                }
                if Instant::now() >= deadline {
                    return Err(anyhow!("{sel} never showed up"));
                }
                tokio::time::sleep(Duration::from_millis(300)).await;
            }
        }
        "ifExists" => {
            let sel = expand(param(p, "selector").context("no element")?, vars);
            if !exists(profile, &sel).await {
                run.log(format!("{sel} is not there — skipping the rest of this pass"));
                return Ok(Flow::EndPass);
            }
        }
        "if.exists" => {
            // Branching check: element there → "when it works"; missing → the
            // else branch ("when it fails").
            let sel = expand(param(p, "selector").context("no element")?, vars);
            let there = exists(profile, &sel).await;
            run.log(format!("if {sel} exists → {}", if there { "yes" } else { "no (else)" }));
            return Ok(if there { Flow::Next } else { Flow::Else });
        }
        "if.text" => {
            let sel = expand(param(p, "selector").context("no element")?, vars);
            let op = param(p, "op").map(|s| expand(s, vars)).unwrap_or_else(|| "contains".into());
            let want = expand(param(p, "text").unwrap_or(""), vars);
            let txt = element_text(profile, &sel).await.unwrap_or_default();
            let holds = cond_holds(&txt, &op, &want);
            run.log(format!("if {sel} text {op} {want:?} → {}", if holds { "yes" } else { "no (else)" }));
            return Ok(if holds { Flow::Next } else { Flow::Else });
        }
        "if.url" => {
            let op = param(p, "op").map(|s| expand(s, vars)).unwrap_or_else(|| "contains".into());
            let want = expand(param(p, "text").unwrap_or(""), vars);
            let url = current_url(profile).await;
            let holds = cond_holds(&url, &op, &want);
            run.log(format!("if address {op} {want:?} → {}", if holds { "yes" } else { "no (else)" }));
            return Ok(if holds { Flow::Next } else { Flow::Else });
        }
        "stop" => return Ok(Flow::EndPass),
        "log" => run.log(expand(param(p, "text").unwrap_or(""), vars)),
        "waitGone" => {
            let sel = expand(param(p, "selector").context("no element")?, vars);
            let deadline = Instant::now() + Duration::from_secs_f64(wait_for(p, vars).max(0.1));
            loop {
                if !exists(profile, &sel).await {
                    break;
                }
                if Instant::now() >= deadline {
                    return Err(anyhow!("{sel} is still there"));
                }
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
        }
        "waitText" => {
            let sel = expand(param(p, "selector").context("no element")?, vars);
            let want = expand(param(p, "text").context("no text to wait for")?, vars);
            let deadline = Instant::now() + Duration::from_secs_f64(wait_for(p, vars).max(0.1));
            loop {
                if let Ok(text) = element_text(profile, &sel).await {
                    if text.to_lowercase().contains(&want.to_lowercase()) {
                        break;
                    }
                }
                if Instant::now() >= deadline {
                    return Err(anyhow!("{sel} never said \"{want}\""));
                }
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
        }
        "waitUrl" => {
            let want = expand(param(p, "url").context("no address to wait for")?, vars);
            let deadline = Instant::now() + Duration::from_secs_f64(wait_for(p, vars).max(0.1));
            loop {
                if current_url(profile).await.contains(&want) {
                    break;
                }
                if Instant::now() >= deadline {
                    return Err(anyhow!("the address never became \"{want}\""));
                }
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
        }
        "count" => {
            let sel = expand(param(p, "selector").context("no element")?, vars);
            let into = check_var_name(param(p, "into").context("no name to save into")?)?;
            let n = cdp::query_piercing(profile, &sel).await.map(|v| v.len()).unwrap_or(0);
            run.log(format!("{into} = {n}"));
            vars.insert(into, n.to_string());
        }
        "readAttribute" => {
            let sel = expand(param(p, "selector").context("no element")?, vars);
            let name = expand(param(p, "name").context("no attribute")?, vars);
            let into = check_var_name(param(p, "into").context("no name to save into")?)?;
            center_waiting(profile, &sel, wait_for(p, vars)).await?;
            let node = *cdp::query_piercing(profile, &sel)
                .await?
                .first()
                .ok_or_else(|| anyhow!("no element matches {sel}"))?;
            let got = cdp::page_call(profile, "DOM.getAttributes", json!({ "nodeId": node })).await?;
            // Attributes arrive as one flat list: name, value, name, value.
            let flat: Vec<String> = got
                .get("attributes")
                .and_then(|a| a.as_array())
                .map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect())
                .unwrap_or_default();
            let value = flat
                .chunks(2)
                .find(|c| c.first().map(|n| n.eq_ignore_ascii_case(&name)).unwrap_or(false))
                .and_then(|c| c.get(1).cloned())
                .ok_or_else(|| anyhow!("{sel} has no attribute \"{name}\""))?;
            vars.insert(into, value);
        }
        "screenshot" => {
            let shot = cdp::page_call(profile, "Page.captureScreenshot", json!({})).await?;
            let b64 = shot
                .get("data")
                .and_then(|v| v.as_str())
                .context("the browser returned no picture")?;
            use base64::Engine as _;
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(b64)
                .map_err(|e| anyhow!("unreadable picture: {e}"))?;
            let path = match param(p, "path").map(|s| expand(s, vars)).filter(|s| !s.trim().is_empty()) {
                Some(p) => std::path::PathBuf::from(p),
                // Beside the run logs, so evidence and log sit together.
                None => crate::store::config_root()?
                    .join("automation-runs")
                    .join(format!("{}-{}.png", profile, now())),
            };
            if let Some(dir) = path.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            std::fs::write(&path, bytes).with_context(|| format!("write {}", path.display()))?;
            run.log(format!("picture saved to {}", path.display()));
        }
        "readText" => {
            let sel = expand(param(p, "selector").context("no element")?, vars);
            // An empty selector used to resolve to the document itself, which
            // read the whole page under the name of one element.
            if sel.trim().is_empty() {
                return Err(anyhow!("no element to read"));
            }
            // Same wait as a click: the text is usually the thing that arrives
            // last.
            center_waiting(profile, &sel, wait_for(p, vars)).await?;
            let node = *cdp::query_piercing(profile, &sel)
                .await?
                .first()
                .ok_or_else(|| anyhow!("no element matches {sel}"))?;
            // The whole subtree, shadow roots included. The old version took
            // the first direct text child and nothing else, so <div>Hello
            // <b>World</b></div> came back as "Hello" and <div><span>Hello
            // </span></div> came back empty.
            let described = cdp::page_call(
                profile,
                "DOM.describeNode",
                json!({ "nodeId": node, "depth": -1, "pierce": true }),
            )
            .await?;
            let mut text = String::new();
            if let Some(node) = described.get("node") {
                gather_text(node, &mut text);
            }
            let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
            // Required, not optional: without a name the step reads the page
            // and throws the answer away, which looks exactly like a step that
            // did not run.
            let into = param(p, "into")
                .filter(|s| !s.trim().is_empty())
                .context("no name to save the text into")?;
            vars.insert(check_var_name(into)?, text);
        }
        // A block a module contributed. The module never touches the page: it
        // is given the step's parameters and the run's variables, and answers
        // with primitive actions the runner then performs itself. So a
        // third-party module lives inside the same guarantee as everything
        // else — its clicks are still Motion clicks.
        other if other.starts_with("module:") => {
            let rest = other.trim_start_matches("module:");
            let (module_id, block_name) = rest.split_once(':').unwrap_or((rest, ""));
            return Box::pin(drive_module(
                bound, vars, run, ctx, module_id, block_name, p,
            ))
            .await;
        }
        other => run.log(format!("step \"{other}\" is not implemented yet — skipped")),
    }
    Ok(Flow::Next)
}


/// Runs one block a module contributed, start to finish.
///
/// Extracted from `run_block`'s `module:` arm so it can be entered from inside
/// itself: a module asking for `module:other:block` lands here again, one frame
/// deeper. That is sound because the outer module is parked in `rrx.recv()`
/// while this runs — it physically cannot ask for a second action — so the
/// borrows the nested frame needs are free.
///
/// This wrapper owns the frame. `leave` has to happen on every path out,
/// including the several that are a bare `?`, so the inner function's result is
/// held rather than returned straight through.
async fn drive_module(
    bound: &mut Bound,
    vars: &mut HashMap<String, String>,
    run: &Run,
    ctx: &mut CallCtx,
    module_id: &str,
    block_name: &str,
    params: &Value,
) -> Result<Flow> {
    // A module id reaches this from inside another module's request, so it is
    // not trusted text: it becomes a path. Same rule install() applies.
    if module_id.is_empty()
        || !module_id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err(anyhow!(
            "\"{module_id}\" is not a module name — letters, digits, - and _ only"
        ));
    }
    ctx.may_call(&Frame::Module(module_id.to_string()))?;
    let saved = ctx.enter(Frame::Module(module_id.to_string()))?;
    let result =
        Box::pin(drive_module_inner(bound, vars, run, ctx, &module_id.to_string(), block_name, params))
            .await;
    ctx.leave(saved);
    result
}

async fn drive_module_inner(
    bound: &mut Bound,
    vars: &mut HashMap<String, String>,
    run: &Run,
    ctx: &mut CallCtx,
    module_id: &str,
    block_name: &str,
    params: &Value,
) -> Result<Flow> {
    let input = json!({
        "block": block_name,
        "params": params,
        "vars": vars,
    });
    let module_id = module_id.to_string();
    let out = if wasm::is_v2(&module_id) {
        // A v2 module DRIVES: it runs on a blocking thread and asks for
        // one action at a time, and this loop performs each with the
        // runner's own state and answers. That is the whole difference
        // from v1 — the module can see what its action did and decide
        // what to do next, instead of handing over a finished plan.
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        // The operator's Stop, so a module in a long loop actually
        // leaves; `abort` is this call's own, for its action budget.
        let stop = run.stop.clone();
        let abort = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let snapshot = vars.clone();
        let id = module_id.clone();
        let flag = stop.clone();
        let abort_flag = abort.clone();
        let mut task = tokio::task::spawn_blocking(move || {
            wasm::run_step(&id, &input, snapshot, tx, flag, abort_flag)
        });
        let done = loop {
            tokio::select! {
                Some(req) = rx.recv() => {
                    ctx.mark_from_module(req.vars.keys().cloned().collect::<Vec<_>>());
                    for (k, v) in req.vars {
                        vars.insert(k, v);
                    }
                    let mut reply = wasm::ActionReply {
                        vars: vars.clone(),
                        ..Default::default()
                    };
                    // Charged to the whole CHAIN, not to this frame: a module
                    // that calls a module would otherwise multiply its budget
                    // by nesting.
                    if let Err(e) = ctx.charge() {
                        reply.error = e.to_string();
                        abort.store(true, std::sync::atomic::Ordering::Relaxed);
                    } else {
                        let kind = req.action.get("kind").and_then(|k| k.as_str())
                            .unwrap_or("").to_string();
                        if kind.is_empty() {
                            reply.error = "an action without a kind".into();
                        } else {
                            let inner = automation::Block {
                                id: String::new(),
                                kind,
                                label: String::new(),
                                params: req.action,
                                enabled: true,
                                secrets: Vec::new(),
                                x: 0.0,
                                y: 0.0,
                                on_done: automation::Branch::Next,
                                on_fail: automation::Branch::Stop,
                            };
                            match Box::pin(run_block(bound, &inner, vars, run, ctx)).await {
                                Ok(Flow::Else) => {
                                    reply.ok = true;
                                    reply.flow = "else".into();
                                }
                                Ok(_) => {
                                    reply.ok = true;
                                    reply.flow = "next".into();
                                }
                                Err(e) => reply.error = e.to_string(),
                            }
                        }
                        reply.vars = vars.clone();
                    }
                    // A module that stopped waiting is not an error:
                    // it may simply have given up on the answer.
                    let _ = req.reply.send(reply);
                }
                finished = &mut task => break finished,
            }
        };
        done.map_err(|e| anyhow!("module panicked: {e}"))??
    } else {
        tokio::task::spawn_blocking(move || wasm::run_block(&module_id, &input))
            .await
            .map_err(|e| anyhow!("module panicked: {e}"))??
    };

    // The module's own lines, from its own call. They used to come
    // from a process-global sink, which mixed them between workers.
    for line in out.logs {
        run.log(line);
    }
    if !out.error.is_empty() {
        return Err(anyhow!(out.error));
    }
    ctx.mark_from_module(out.vars.keys().cloned().collect::<Vec<_>>());
    for (k, v) in out.vars {
        vars.insert(k, v);
    }
    // Bounded: a module that returned a thousand clicks is a bug, and
    // running them would be indistinguishable from an attack.
    if out.actions.len() > 200 {
        return Err(anyhow!("the module asked for {} actions", out.actions.len()));
    }
    for action in out.actions {
        let kind = action
            .get("kind")
            .and_then(|k| k.as_str())
            .unwrap_or("")
            .to_string();
        if kind.starts_with("module:") {
            return Err(anyhow!("a module may not call another module"));
        }
        let inner = automation::Block {
            id: String::new(),
            kind,
            label: String::new(),
            params: action,
            enabled: true,
            secrets: Vec::new(),
            x: 0.0,
            y: 0.0,
            on_done: automation::Branch::Next,
            on_fail: automation::Branch::Stop,
        };
        Box::pin(run_block(bound, &inner, vars, run, ctx)).await?;
    }

    Ok(Flow::Next)
}

async fn run_worker(
    run: Arc<Run>,
    idx: usize,
    seed_profile: Option<String>,
    project: automation::Project,
    deadline: Option<Instant>,
) {
    let mut steps: Vec<automation::Block> =
        project.blocks.iter().filter(|b| b.enabled).cloned().collect();
    // The run follows the visual stack top-to-bottom (by column, then height),
    // not the order the blocks happen to sit in the list. A card dragged above
    // another therefore runs before it, which is what the canvas shows.
    // "next" means the card below; explicit gotos still jump anywhere.
    steps.sort_by(|a, b| {
        let ca = (a.x / 40.0).round() as i64;
        let cb = (b.x / 40.0).round() as i64;
        ca.cmp(&cb)
            .then(a.y.partial_cmp(&b.y).unwrap_or(std::cmp::Ordering::Equal))
    });

    run.worker(idx, |w| {
        w.status = "running".into();
        w.steps_total = steps.len() as u32;
    });

    // A profile the graph makes, or one the operator named. Either way it is
    // the worker's, and a temporary one is cleaned up when the worker is done.
    let seed_id = seed_profile.unwrap_or_default();
    let mut bound = Bound {
        mobile: !seed_id.is_empty() && bound_is_mobile(&seed_id),
        id: seed_id,
        temporary: false,
    };
    let mut vars: HashMap<String, String> = HashMap::new();
    vars.insert("thread".into(), (idx + 1).to_string());
    let mut ctx = CallCtx::new();

    let endless = project.run.loops == 0;
    let mut pass: u32 = 0;

    // Where the graph begins. An explicit start block, or the first one.
    let entry = steps
        .iter()
        .position(|b| b.id == project.run.start)
        .unwrap_or(0);

    loop {
        if run.stop.load(Ordering::Relaxed) {
            run.worker(idx, |w| w.status = "stopped".into());
            break;
        }
        if !endless && pass >= project.run.loops {
            run.worker(idx, |w| w.status = "done".into());
            break;
        }
        if let Some(d) = deadline {
            if Instant::now() >= d {
                run.worker(idx, |w| w.status = "done".into());
                break;
            }
        }
        pass += 1;
        vars.insert("pass".into(), pass.to_string());
        run.worker(idx, |w| {
            w.pass = pass;
            w.step = 0;
        });

        // Branch-driven, not a for-loop over the list: each step says where to
        // go next, and "where" can be backwards or across the canvas.
        let mut at: usize = entry;
        let mut retries: HashMap<usize, u32> = HashMap::new();
        let mut hops: u32 = 0;
        let mut end_run = false;

        while at < steps.len() {
            if run.stop.load(Ordering::Relaxed) {
                break;
            }
            // A graph that loops through itself forever is a mistake, not a
            // feature; this bounds one pass without bounding a real loop.
            hops += 1;
            if hops > 100_000 {
                run.log("too many jumps in one pass — stopping");
                end_run = true;
                break;
            }

            let block = &steps[at];
            run.worker(idx, |w| {
                w.step = at as u32 + 1;
                w.note = block.label.clone();
            });

            let outcome = run_block(&mut bound, block, &mut vars, &run, &mut ctx).await;
            let branch = match &outcome {
                Ok(Flow::Next) => block.on_done.clone(),
                Ok(Flow::EndPass) => automation::Branch::EndPass,
                // A false condition is not a failure — it just takes the "when
                // it fails" branch (the else path).
                Ok(Flow::Else) => block.on_fail.clone(),
                Err(e) => {
                    run.log(format!("step {} — {e}", at + 1));
                    block.on_fail.clone()
                }
            };

            match branch {
                automation::Branch::Next => {
                    retries.remove(&at);
                    at += 1;
                }
                automation::Branch::Stop => {
                    if outcome.is_err() {
                        run.worker(idx, |w| {
                            w.status = "failed".into();
                            w.note = format!("step {}", at + 1);
                        });
                        end_run = true;
                        break;
                    }
                    at = steps.len();
                }
                automation::Branch::EndPass => break,
                automation::Branch::Goto(target) => {
                    retries.remove(&at);
                    match steps.iter().position(|b| b.id == target) {
                        Some(next) => at = next,
                        // The block it pointed at is gone; carrying on is safer
                        // than stopping a long run over a stale connection.
                        None => at += 1,
                    }
                }
                automation::Branch::Retry(times) => {
                    let used = retries.entry(at).or_insert(0);
                    if *used < times {
                        *used += 1;
                        run.log(format!("retry {} of {} on step {}", *used, times, at + 1));
                        tokio::time::sleep(Duration::from_millis(600)).await;
                    } else {
                        retries.remove(&at);
                        at += 1;
                    }
                }
            }
        }

        if end_run {
            break;
        }
    }

    if !bound.id.is_empty() {
        cdp::detach(&bound.id);
        let _ = process::Tracker::shared().kill(&bound.id).await;
    }
    // Every temporary profile this worker made, not only the one still bound.
    // A temporary profile exists for one run; leaving them behind fills the
    // profile list with single-use entries nobody asked for.
    for id in std::mem::take(&mut ctx.temps) {
        if id != bound.id {
            cdp::detach(&id);
            let _ = process::Tracker::shared().kill(&id).await;
        }
        let _ = profile::forget_temporary(&id);
        run.log(format!("cleaned up temporary profile {id}"));
    }
}

pub async fn start(project_id: &str) -> Result<()> {
    if migrate::in_progress() {
        return Err(anyhow!("profiles are being moved — try again when that finishes"));
    }
    if status(project_id).map(|s| s.running).unwrap_or(false) {
        return Err(anyhow!("that project is already running"));
    }
    // Taken before the list, so the projects are the space's the run keeps to.
    let space = crate::space::Ticket::now();
    let projects = automation::list()?;
    space.still()?;
    let project = projects
        .iter()
        .find(|p| p.id == project_id)
        .cloned()
        .context("no such project")?;
    if project.blocks.iter().all(|b| !b.enabled) {
        return Err(anyhow!("the project has no steps to run"));
    }

    // A thread is one run of the graph. Which profile it drives is the graph's
    // own business now: a project usually starts by making one.
    let threads = project.run.threads.clamp(1, 64) as usize;

    // Every thread starts from nothing: a browser appears when a profile block
    // says so and not before. `run.profiles` used to seed threads with profiles
    // named up front, which is why a run could open a browser the project never
    // asked for — a project saved before profiles became blocks still carries
    // the field, nothing in the UI can clear it, and the operator has no way to
    // see it. It is now what its own doc comment always claimed: ignored.
    if !project.run.profiles.is_empty() {
        eprintln!(
            "[launcher] automation: project {} still lists {} profile(s) from an older save — ignored",
            project.id,
            project.run.profiles.len()
        );
    }
    let seeds: Vec<Option<String>> = (0..threads).map(|_| None).collect();

    let names = crate::space::profile::list_all()
        .unwrap_or_default()
        .into_iter()
        .map(|p| (p.id, p.name))
        .collect::<HashMap<_, _>>();

    let workers: Vec<WorkerState> = seeds
        .iter()
        .enumerate()
        .map(|(i, seed)| WorkerState {
            profile_id: seed.clone().unwrap_or_default(),
            profile_name: match seed {
                Some(id) => names.get(id).cloned().unwrap_or_else(|| id.clone()),
                None => format!("thread {}", i + 1),
            },
            pass: 0,
            step: 0,
            steps_total: 0,
            status: "queued".into(),
            note: String::new(),
        })
        .collect();

    let run = Arc::new(Run {
        state: Mutex::new(RunState {
            project_id: project.id.clone(),
            project_name: project.name.clone(),
            started_at: now(),
            running: true,
            workers,
            log: Vec::new(),
        }),
        stop: Arc::new(AtomicBool::new(false)),
        rules: project.rules.clone(),
        projects,
        space,
    });
    runs()
        .lock()
        .map_err(|_| anyhow!("runs lock poisoned"))?
        .insert(project.id.clone(), run.clone());

    // Trimmed when a run starts rather than on a timer: it is the only moment
    // a new file is about to appear, and a launcher left closed for a month
    // should not spend its first second deleting things.
    prune_logs(50);
    run.log(format!("started \"{}\"", project.name));

    let deadline = if project.run.loops == 0 && project.run.hours > 0.0 {
        Some(Instant::now() + Duration::from_secs_f64(project.run.hours * 3600.0))
    } else {
        None
    };

    tokio::spawn(async move {
        let sem = Arc::new(tokio::sync::Semaphore::new(threads));
        let mut handles = Vec::new();
        for (idx, seed) in seeds.into_iter().enumerate() {
            let permit = sem.clone().acquire_owned().await;
            if run.stop.load(Ordering::Relaxed) {
                break;
            }
            let run = run.clone();
            let project = project.clone();
            handles.push(tokio::spawn(async move {
                let _permit = permit;
                run_worker(run, idx, seed, project, deadline).await;
            }));
        }
        for h in handles {
            let _ = h.await;
        }
        if let Ok(mut s) = run.state.lock() {
            s.running = false;
        }
        // Request sessions outlive a step on purpose, but not the machine.
        // Cleared when the LAST run ends rather than this one, because a
        // session is named by the operator and two runs may share it — and a
        // jar left behind is a logged-in visitor arriving out of nowhere next
        // time.
        let others_going = runs()
            .lock()
            .map(|g| {
                g.values().any(|r| {
                    r.state.lock().map(|s| s.running).unwrap_or(false)
                })
            })
            .unwrap_or(false);
        if !others_going {
            crate::requests::drop_all_sessions();
            crate::db::drop_all();
        }
        run.log("finished");
    });

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn block(kind: &str, params: serde_json::Value) -> automation::Block {
        automation::Block {
            id: format!("{kind}-1"),
            kind: kind.into(),
            label: String::new(),
            params,
            enabled: true,
            secrets: Vec::new(),
            x: 0.0,
            y: 0.0,
            on_done: automation::Branch::Next,
            on_fail: automation::Branch::Stop,
        }
    }

    fn project(id: &str, name: &str, blocks: Vec<automation::Block>) -> automation::Project {
        automation::Project {
            id: id.into(),
            name: name.into(),
            notes: String::new(),
            blocks,
            run: automation::RunSettings::default(),
            rules: Vec::new(),
            created_at: 0,
            updated_at: 0,
            revision: None,
        }
    }

    /// A run logs to the config dir, so a test that makes one takes a test_root first.
    fn a_run(projects: Vec<automation::Project>) -> Run {
        Run {
            state: Mutex::new(RunState {
                project_id: "caller".into(),
                project_name: "caller".into(),
                started_at: 0,
                running: true,
                workers: Vec::new(),
                log: Vec::new(),
            }),
            stop: Arc::new(AtomicBool::new(false)),
            rules: Vec::new(),
            projects,
            space: crate::space::Ticket::now(),
        }
    }

    #[test]
    fn a_chain_is_bounded_and_a_loop_is_named() {
        let mut ctx = CallCtx::new();
        let mut saved = Vec::new();
        for i in 0..MAX_CALL_DEPTH {
            saved.push(ctx.enter(Frame::Module(format!("m{i}"))).expect("within depth"));
        }
        let too_deep = ctx.enter(Frame::Module("m9".into())).unwrap_err().to_string();
        assert!(too_deep.contains("deep"), "{too_deep}");

        // A repeat is reported as the loop it is, not as depth.
        let mut ctx = CallCtx::new();
        let _ = ctx.enter(Frame::Module("a".into())).unwrap();
        let _ = ctx.enter(Frame::Flow("f".into())).unwrap();
        let loop_err = ctx.enter(Frame::Module("a".into())).unwrap_err().to_string();
        assert!(loop_err.contains("circle"), "{loop_err}");
        assert!(loop_err.contains("\"a\""), "{loop_err}");
    }

    #[test]
    fn leaving_a_frame_puts_the_stack_back() {
        let mut ctx = CallCtx::new();
        let outer = ctx.enter(Frame::Module("a".into())).unwrap();
        {
            let inner = ctx.enter(Frame::Module("b".into())).unwrap();
            assert_eq!(ctx.depth, 2);
            ctx.leave(inner);
        }
        assert_eq!(ctx.depth, 1);
        // b is gone, so calling it again is not a loop.
        let again = ctx.enter(Frame::Module("b".into())).unwrap();
        ctx.leave(again);
        ctx.leave(outer);
        assert_eq!(ctx.depth, 0);
        assert!(ctx.stack.is_empty());
    }

    #[test]
    fn the_action_budget_is_per_chain_and_resets_with_it() {
        let mut ctx = CallCtx::new();
        let first = ctx.enter(Frame::Module("a".into())).unwrap();
        for _ in 0..MAX_CHAIN_ACTIONS {
            ctx.charge().expect("within budget");
        }
        assert!(ctx.charge().is_err(), "the chain must run out");
        ctx.leave(first);
        // The next placed block is a new chain and gets its own budget, exactly
        // as a module block does today.
        let second = ctx.enter(Frame::Module("a".into())).unwrap();
        assert!(ctx.charge().is_ok());
        ctx.leave(second);
    }

    #[test]
    fn a_flow_resolves_by_id_then_by_a_unique_name() {
        let run = a_run(vec![
            project("id-1", "Warm up", Vec::new()),
            project("id-2", "Twin", Vec::new()),
            project("id-3", "Twin", Vec::new()),
        ]);
        assert_eq!(resolve_flow(&run, "id-1", "").unwrap().id, "id-1");
        // An id that travelled falls back to the name.
        assert_eq!(resolve_flow(&run, "gone", "Warm up").unwrap().id, "id-1");
        // Two of a name is refused rather than guessed.
        let ambiguous = resolve_flow(&run, "", "Twin").unwrap_err().to_string();
        assert!(ambiguous.contains("more than one"), "{ambiguous}");
        assert!(resolve_flow(&run, "", "Nothing").is_err());
    }

    /// The whole sub-flow path, with blocks that touch no page: a called flow
    /// runs, sets a variable, and hands back only what the caller asked for.
    #[tokio::test]
    async fn a_flow_runs_and_returns_what_was_asked_for() {
        let _root = crate::store::test_root("runner");
        let callee = project(
            "flow-1",
            "Sets a token",
            vec![
                block("flow.entry", json!({ "name": "start" })),
                block("var.set", json!({ "name": "token", "value": "abc-{{seed}}" })),
                block("var.set", json!({ "name": "private", "value": "not yours" })),
            ],
        );
        // Lay them out down one column, the order a project runs in.
        let mut callee = callee;
        for (i, b) in callee.blocks.iter_mut().enumerate() {
            b.y = i as f64 * 60.0;
        }
        let run = a_run(vec![callee]);
        let mut ctx = CallCtx::new();
        let mut bound = Bound::default();
        let mut vars: HashMap<String, String> = HashMap::new();
        vars.insert("outer".into(), "kept".into());

        let call = block(
            "flow.call",
            json!({ "project": "flow-1", "in": "seed=42", "out": "token" }),
        );
        let flow = run_block(&mut bound, &call, &mut vars, &run, &mut ctx)
            .await
            .expect("the flow runs");
        assert!(matches!(flow, Flow::Next));
        assert_eq!(vars.get("token").map(String::as_str), Some("abc-42"));
        // Named in "in"/"out" means isolated: the caller keeps its own map and
        // the flow's other variables do not leak into it.
        assert_eq!(vars.get("outer").map(String::as_str), Some("kept"));
        assert!(vars.get("private").is_none());
        assert!(vars.get("seed").is_none());
        // The frame was left.
        assert_eq!(ctx.depth, 0);
    }

    #[tokio::test]
    async fn a_flow_that_never_sets_an_asked_for_variable_is_an_error() {
        let _root = crate::store::test_root("runner");
        // var.set, not log: log sits below the runner's browser check, so a
        // flow made of it would fail for the wrong reason.
        let mut callee = project(
            "flow-2",
            "Sets nothing",
            vec![block("var.set", json!({ "name": "other", "value": "hello" }))],
        );
        callee.blocks[0].y = 0.0;
        let run = a_run(vec![callee]);
        let mut ctx = CallCtx::new();
        let mut bound = Bound::default();
        let mut vars = HashMap::new();
        let call = block("flow.call", json!({ "project": "flow-2", "out": "token" }));
        let err = run_block(&mut bound, &call, &mut vars, &run, &mut ctx)
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("without setting"), "{err}");
        assert_eq!(ctx.depth, 0, "the frame must be left even on the error path");
    }

    #[tokio::test]
    async fn a_flow_calling_itself_is_refused_as_a_loop() {
        let _root = crate::store::test_root("runner");
        let mut callee = project(
            "flow-3",
            "Recursive",
            vec![block("flow.call", json!({ "project": "flow-3" }))],
        );
        callee.blocks[0].y = 0.0;
        // Its own failure edge is Stop, so the loop refusal comes back out.
        let run = a_run(vec![callee]);
        let mut ctx = CallCtx::new();
        let mut bound = Bound::default();
        let mut vars = HashMap::new();
        let call = block("flow.call", json!({ "project": "flow-3" }));
        let err = run_block(&mut bound, &call, &mut vars, &run, &mut ctx)
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("circle"), "{err}");
        assert_eq!(ctx.depth, 0);
    }

    #[tokio::test]
    async fn an_entry_that_is_gone_is_an_error_not_a_run_from_the_top() {
        let _root = crate::store::test_root("runner");
        let mut callee = project(
            "flow-4",
            "Has one entry",
            vec![
                block("var.set", json!({ "name": "ran", "value": "yes" })),
                block("flow.entry", json!({ "name": "later" })),
            ],
        );
        callee.blocks[0].y = 0.0;
        callee.blocks[1].y = 60.0;
        let run = a_run(vec![callee]);
        let mut ctx = CallCtx::new();
        let mut bound = Bound::default();
        let mut vars = HashMap::new();
        let call = block("flow.call", json!({ "project": "flow-4", "entry": "missing" }));
        let err = run_block(&mut bound, &call, &mut vars, &run, &mut ctx)
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("no entry called"), "{err}");
        assert!(vars.get("ran").is_none(), "nothing may have run");
    }

    #[tokio::test]
    async fn a_named_entry_starts_below_it() {
        let _root = crate::store::test_root("runner");
        let mut callee = project(
            "flow-5",
            "Two halves",
            vec![
                block("var.set", json!({ "name": "first", "value": "ran" })),
                block("flow.entry", json!({ "name": "second" })),
                block("var.set", json!({ "name": "second_ran", "value": "yes" })),
            ],
        );
        for (i, b) in callee.blocks.iter_mut().enumerate() {
            b.y = i as f64 * 60.0;
        }
        let run = a_run(vec![callee]);
        let mut ctx = CallCtx::new();
        let mut bound = Bound::default();
        let mut vars = HashMap::new();
        let call = block("flow.call", json!({ "project": "flow-5", "entry": "second" }));
        run_block(&mut bound, &call, &mut vars, &run, &mut ctx).await.unwrap();
        assert_eq!(vars.get("second_ran").map(String::as_str), Some("yes"));
        assert!(vars.get("first").is_none(), "the half above the entry must not run");
    }

    /// The guard fires through the real path, not only in its own tests: a
    /// module frame anywhere in the chain is enough.
    #[tokio::test]
    async fn a_module_cannot_navigate_off_the_web() {
        let _root = crate::store::test_root("runner");
        let run = a_run(Vec::new());
        let mut ctx = CallCtx::new();
        let mut bound = Bound::default();
        let mut vars = HashMap::new();
        let go = block("goto", json!({ "url": "file:///etc/passwd" }));

        // The operator placed it themselves: their block, their call.
        let their_own = run_block(&mut bound, &go, &mut vars, &run, &mut ctx).await;
        assert!(
            their_own.unwrap_err().to_string().contains("needs a profile"),
            "an operator's goto must get as far as the browser check"
        );

        // The same block, asked for by a module.
        let frame = ctx.enter(Frame::Module("scraper".into())).unwrap();
        let refused = run_block(&mut bound, &go, &mut vars, &run, &mut ctx)
            .await
            .unwrap_err()
            .to_string();
        ctx.leave(frame);
        assert!(refused.contains("http and https"), "{refused}");
    }

    /// A module may not call a flow it was not granted, and nothing about the
    /// call's own parameters changes that.
    #[tokio::test]
    async fn an_ungranted_flow_is_refused() {
        let _root = crate::store::test_root("runner");
        let run = a_run(vec![project("flow-7", "Someone else's", Vec::new())]);
        let mut ctx = CallCtx::new();
        let mut bound = Bound::default();
        let mut vars = HashMap::new();
        let frame = ctx.enter(Frame::Module("scraper".into())).unwrap();
        let call = block("flow.call", json!({ "project": "flow-7" }));
        let refused = run_block(&mut bound, &call, &mut vars, &run, &mut ctx)
            .await
            .unwrap_err()
            .to_string();
        ctx.leave(frame);
        assert!(refused.contains("permission"), "{refused}");
        assert!(refused.contains("scraper"), "{refused}");
    }

    /// And once inside a flow the module WAS granted, the guard still applies:
    /// the module chose the flow and what went into it, so a flow doing
    /// `goto {{url}}` is the module's reach with one step in between.
    #[tokio::test]
    async fn the_guard_reaches_through_a_flow() {
        let _root = crate::store::test_root("runner");
        let run = a_run(Vec::new());
        let mut ctx = CallCtx::new();
        let mut bound = Bound::default();
        let mut vars = HashMap::new();
        // The two frames a granted call would have pushed.
        let outer = ctx.enter(Frame::Module("scraper".into())).unwrap();
        let inner = ctx.enter(Frame::Flow("flow-7".into())).unwrap();
        let go = block("goto", json!({ "url": "file:///etc/passwd" }));
        let refused = run_block(&mut bound, &go, &mut vars, &run, &mut ctx)
            .await
            .unwrap_err()
            .to_string();
        ctx.leave(inner);
        ctx.leave(outer);
        assert!(refused.contains("http and https"), "{refused}");
    }

    /// The laundering path: a module writes a variable and waits for a block
    /// the OPERATOR wrote to run it.
    #[tokio::test]
    async fn a_modules_text_cannot_become_code_through_a_variable() {
        let _root = crate::store::test_root("runner");
        let run = a_run(Vec::new());
        let mut ctx = CallCtx::new();
        let mut bound = Bound::default();
        let mut vars = HashMap::new();

        // The operator's own variable is fine, whatever is in it.
        vars.insert("payload".into(), "fetch('/x')".into());
        let sink = block("script.run", json!({ "source": "{{payload}}" }));
        let theirs = run_block(&mut bound, &sink, &mut vars, &run, &mut ctx).await;
        assert!(
            theirs.unwrap_err().to_string().contains("needs a profile"),
            "the operator's own variable must get as far as the browser check"
        );

        // The same name, written by a module.
        let frame = ctx.enter(Frame::Module("formatter".into())).unwrap();
        ctx.mark_from_module(vec!["payload".to_string()]);
        ctx.leave(frame);
        let refused = run_block(&mut bound, &sink, &mut vars, &run, &mut ctx)
            .await
            .unwrap_err()
            .to_string();
        assert!(refused.contains("a module wrote that"), "{refused}");

        // A different variable in the same sink is untouched.
        vars.insert("clean".into(), "1".into());
        let other = block("script.run", json!({ "source": "{{clean}}" }));
        let ok_again = run_block(&mut bound, &other, &mut vars, &run, &mut ctx).await;
        assert!(ok_again.unwrap_err().to_string().contains("needs a profile"));

        // And a block that is not a sink does not care.
        let harmless = block("type", json!({ "text": "{{payload}}" }));
        let typed = run_block(&mut bound, &harmless, &mut vars, &run, &mut ctx).await;
        assert!(typed.unwrap_err().to_string().contains("needs a profile"));
    }

    #[tokio::test]
    async fn without_in_or_out_a_flow_shares_the_run_s_variables() {
        let _root = crate::store::test_root("runner");
        let mut callee = project(
            "flow-6",
            "Shares",
            vec![block("var.set", json!({ "name": "added", "value": "{{outer}}!" }))],
        );
        callee.blocks[0].y = 0.0;
        let run = a_run(vec![callee]);
        let mut ctx = CallCtx::new();
        let mut bound = Bound::default();
        let mut vars = HashMap::new();
        vars.insert("outer".into(), "hi".into());
        let call = block("flow.call", json!({ "project": "flow-6" }));
        run_block(&mut bound, &call, &mut vars, &run, &mut ctx).await.unwrap();
        assert_eq!(vars.get("added").map(String::as_str), Some("hi!"));
    }
}
