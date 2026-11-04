//! A minimal CDP client: one live session per profile.
//!
//! Not a general library. It speaks exactly what the automation studio needs —
//! send a command and await its reply, and forward the events we subscribed to
//! into the UI as Tauri events.
//!
//! Compiled out with the `automation` feature; the whole file is one cfg.

#![cfg(feature = "automation")]

use anyhow::{anyhow, Context, Result};
use futures_util::{SinkExt, StreamExt};
use serde::Serialize;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use tokio::sync::{broadcast, mpsc, oneshot};
use tokio_tungstenite::tungstenite::Message;

/// A frame of the live view, on its way to the canvas.
#[derive(Debug, Clone, Serialize)]
pub struct Frame {
    pub profile_id: String,
    /// base64 JPEG, exactly as the browser sent it.
    pub data: String,
    /// CSS pixels of the page the frame covers, so a click on the canvas can
    /// be mapped back to a page point.
    pub width: f64,
    pub height: f64,
    pub offset_top: f64,
    pub page_scale: f64,
    pub scroll_x: f64,
    pub scroll_y: f64,
}

type Pending = Arc<Mutex<HashMap<u64, oneshot::Sender<Result<Value, String>>>>>;

struct Session {
    out: mpsc::UnboundedSender<Message>,
    pending: Pending,
    next_id: AtomicU64,
    /// Target session for the page, once attached.
    page_session: Mutex<Option<String>>,
    /// Every event, by method name, for callers that need to wait for one.
    events: broadcast::Sender<String>,
    /// A run drives this browser: the studio does not attach to it as well.
    by_run: AtomicBool,
}

fn sessions() -> &'static Mutex<HashMap<String, Arc<Session>>> {
    static S: OnceLock<Mutex<HashMap<String, Arc<Session>>>> = OnceLock::new();
    S.get_or_init(|| Mutex::new(HashMap::new()))
}

fn get(profile_id: &str) -> Option<Arc<Session>> {
    sessions().lock().ok()?.get(profile_id).cloned()
}

pub fn is_attached(profile_id: &str) -> bool {
    get(profile_id).is_some()
}

/// Marks the session as a run's, until it is detached or its browser closes.
pub fn mark_run(profile_id: &str) {
    if let Some(s) = get(profile_id) {
        s.by_run.store(true, Ordering::Relaxed);
    }
}

/// A run drives this profile's browser now.
pub fn run_drives(profile_id: &str) -> bool {
    get(profile_id).is_some_and(|s| s.by_run.load(Ordering::Relaxed))
}

impl Session {
    /// Sends a command and waits for its reply. `session` routes it to a page
    /// target; None talks to the browser itself.
    async fn call(&self, method: &str, params: Value, session: Option<&str>) -> Result<Value> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        self.pending
            .lock()
            .map_err(|_| anyhow!("cdp lock poisoned"))?
            .insert(id, tx);

        let mut msg = json!({ "id": id, "method": method, "params": params });
        if let Some(s) = session {
            msg["sessionId"] = json!(s);
        }
        self.out
            .send(Message::Text(msg.to_string()))
            .map_err(|_| anyhow!("cdp connection closed"))?;

        match tokio::time::timeout(std::time::Duration::from_secs(120), rx).await {
            Ok(Ok(Ok(v))) => Ok(v),
            Ok(Ok(Err(e))) => Err(anyhow!(e)),
            Ok(Err(_)) => Err(anyhow!("cdp connection closed")),
            Err(_) => Err(anyhow!("{method} timed out")),
        }
    }

    fn page(&self) -> Option<String> {
        self.page_session.lock().ok()?.clone()
    }
}

/// A run's attach to an already-running profile's first page; the session is marked as the run's.
/// `ws_url` comes from the launcher's own DevToolsActivePort read.
pub async fn attach<F>(profile_id: String, ws_url: String, on_frame: F) -> Result<()>
where
    F: Fn(Frame) + Send + Sync + 'static,
{
    attach_with(profile_id.clone(), ws_url, on_frame, |_, _| {}, |_, _| {}).await?;
    mark_run(&profile_id);
    Ok(())
}

pub async fn attach_with<F, N, E>(
    profile_id: String,
    ws_url: String,
    on_frame: F,
    on_nav: N,
    on_event: E,
) -> Result<()>
where
    F: Fn(Frame) + Send + Sync + 'static,
    N: Fn(String, String) + Send + Sync + 'static,
    // Any CDP event not handled specially above (method, params). The studio
    // uses it to surface Traffic.requestPaused for the request interceptor.
    E: Fn(String, Value) + Send + Sync + 'static,
{
    if is_attached(&profile_id) {
        return Ok(());
    }

    let (stream, _) = tokio_tungstenite::connect_async(&ws_url)
        .await
        .with_context(|| format!("connect {ws_url}"))?;
    let (mut sink, mut source) = stream.split();

    let (out_tx, mut out_rx) = mpsc::unbounded_channel::<Message>();
    let pending: Pending = Arc::new(Mutex::new(HashMap::new()));

    let (events, _) = broadcast::channel::<String>(64);
    let session = Arc::new(Session {
        out: out_tx.clone(),
        pending: pending.clone(),
        next_id: AtomicU64::new(1),
        page_session: Mutex::new(None),
        events,
        by_run: AtomicBool::new(false),
    });

    tokio::spawn(async move {
        while let Some(m) = out_rx.recv().await {
            if sink.send(m).await.is_err() {
                break;
            }
        }
    });

    // Reader: replies go to whoever is waiting, events are dispatched.
    {
        let pending = pending.clone();
        let session = session.clone();
        let profile_id = profile_id.clone();
        let out_tx = out_tx.clone();
        let on_event = on_event;
        tokio::spawn(async move {
            while let Some(Ok(msg)) = source.next().await {
                let text = match msg {
                    Message::Text(t) => t,
                    Message::Close(_) => break,
                    _ => continue,
                };
                let Ok(v) = serde_json::from_str::<Value>(&text) else { continue };

                if let Some(id) = v.get("id").and_then(|x| x.as_u64()) {
                    let slot = pending.lock().ok().and_then(|mut p| p.remove(&id));
                    if let Some(tx) = slot {
                        let reply = match v.get("error") {
                            Some(e) => Err(e
                                .get("message")
                                .and_then(|m| m.as_str())
                                .unwrap_or("cdp error")
                                .to_string()),
                            None => Ok(v.get("result").cloned().unwrap_or(Value::Null)),
                        };
                        let _ = tx.send(reply);
                    }
                    continue;
                }

                let method = v.get("method").and_then(|m| m.as_str()).unwrap_or("");
                // The whole event, not just its name: a caller waiting on a
                // navigation needs the URL that arrived with it.
                let _ = session.events.send(text.clone());

                match Some(method) {
                    Some("Page.screencastFrame") => {
                        let p = v.get("params").cloned().unwrap_or(Value::Null);
                        let sid = v.get("sessionId").and_then(|s| s.as_str());
                        // Ack FIRST and unconditionally. The browser allows
                        // two frames in flight and then stops sending; a
                        // stream that stalls after three frames is always a
                        // missing ack.
                        if let Some(ack) = p.get("sessionId") {
                            let mut m = json!({
                                "id": 0,
                                "method": "Page.screencastFrameAck",
                                "params": { "sessionId": ack },
                            });
                            if let Some(s) = sid {
                                m["sessionId"] = json!(s);
                            }
                            let _ = out_tx.send(Message::Text(m.to_string()));
                        }
                        let md = p.get("metadata").cloned().unwrap_or(Value::Null);
                        let num = |k: &str| md.get(k).and_then(|x| x.as_f64()).unwrap_or(0.0);
                        on_frame(Frame {
                            profile_id: profile_id.clone(),
                            data: p
                                .get("data")
                                .and_then(|d| d.as_str())
                                .unwrap_or_default()
                                .to_string(),
                            width: num("deviceWidth"),
                            height: num("deviceHeight"),
                            offset_top: num("offsetTop"),
                            page_scale: {
                                let s = num("pageScaleFactor");
                                if s > 0.0 { s } else { 1.0 }
                            },
                            scroll_x: num("scrollOffsetX"),
                            scroll_y: num("scrollOffsetY"),
                        });
                    }
                    Some("Page.frameNavigated") => {
                        // Only the top frame: an iframe navigating is not
                        // something the operator did.
                        let frame = v
                            .get("params")
                            .and_then(|p| p.get("frame"))
                            .cloned()
                            .unwrap_or(Value::Null);
                        if frame.get("parentId").is_none() {
                            if let Some(url) = frame.get("url").and_then(|u| u.as_str()) {
                                on_nav(profile_id.clone(), url.to_string());
                            }
                        }
                    }
                    Some("Target.detachedFromTarget") => {
                        if let Ok(mut g) = session.page_session.lock() {
                            *g = None;
                        }
                    }
                    _ => {
                        on_event(
                            method.to_string(),
                            v.get("params").cloned().unwrap_or(Value::Null),
                        );
                    }
                }
            }
            // The socket is gone; drop the session so a later attach reconnects.
            if let Ok(mut g) = sessions().lock() {
                g.remove(&profile_id);
            }
        });
    }

    // Attach to the first page target. flatten:true makes the page reachable
    // over this same socket with a sessionId, so one connection serves both.
    let targets = session.call("Target.getTargets", json!({}), None).await?;
    let page_target = targets
        .get("targetInfos")
        .and_then(|t| t.as_array())
        .and_then(|arr| {
            arr.iter()
                .find(|t| t.get("type").and_then(|x| x.as_str()) == Some("page"))
        })
        .and_then(|t| t.get("targetId").and_then(|x| x.as_str()))
        .ok_or_else(|| anyhow!("the profile has no page open"))?
        .to_string();

    let attached = session
        .call(
            "Target.attachToTarget",
            json!({ "targetId": page_target, "flatten": true }),
            None,
        )
        .await?;
    let page_session = attached
        .get("sessionId")
        .and_then(|s| s.as_str())
        .ok_or_else(|| anyhow!("no sessionId from attachToTarget"))?
        .to_string();

    *session.page_session.lock().unwrap() = Some(page_session.clone());
    session
        .call("Page.enable", json!({}), Some(&page_session))
        .await?;
    session
        .call("DOM.enable", json!({}), Some(&page_session))
        .await?;

    sessions()
        .lock()
        .map_err(|_| anyhow!("cdp lock poisoned"))?
        .insert(profile_id, session);
    Ok(())
}

pub fn detach(profile_id: &str) {
    if let Ok(mut g) = sessions().lock() {
        g.remove(profile_id);
    }
}

/// Sends a page-scoped command. Every automation action goes through here.
pub async fn page_call(profile_id: &str, method: &str, params: Value) -> Result<Value> {
    let s = get(profile_id).ok_or_else(|| anyhow!("not attached"))?;
    let page = s.page().ok_or_else(|| anyhow!("no page attached"))?;
    s.call(method, params, Some(&page)).await
}

pub async fn start_screencast(profile_id: &str, max_width: u32, max_height: u32) -> Result<()> {
    page_call(
        profile_id,
        "Page.startScreencast",
        json!({
            "format": "jpeg",
            "quality": 60,
            "maxWidth": max_width,
            "maxHeight": max_height,
            "everyNthFrame": 1,
        }),
    )
    .await
    .map(|_| ())
}

pub async fn stop_screencast(profile_id: &str) -> Result<()> {
    page_call(profile_id, "Page.stopScreencast", json!({}))
        .await
        .map(|_| ())
}

/// What a right-click during recording resolved to.
#[derive(Debug, Clone, Serialize)]
pub struct Picked {
    /// A CSS selector that matches this element and nothing else, when one
    /// could be found. Empty means fall back to the point.
    pub selector: String,
    pub tag: String,
    /// Something readable for the step's label: the visible text, the
    /// placeholder, the name — whatever the element carries.
    pub label: String,
    pub x: f64,
    pub y: f64,
    /// Every candidate that was tried and how many nodes it matched. Shown in
    /// the UI when nothing unique was found, because "no selector" on its own
    /// is not something anyone can act on.
    pub tried: Vec<String>,
}

fn attr<'a>(attrs: &'a [Value], name: &str) -> Option<&'a str> {
    // The DOM agent sends attributes as a flat [name, value, name, value…].
    attrs
        .chunks(2)
        .find(|c| c.first().and_then(|k| k.as_str()) == Some(name))
        .and_then(|c| c.get(1))
        .and_then(|v| v.as_str())
        .filter(|v| !v.is_empty())
}

fn css_escape(v: &str) -> String {
    v.replace('\\', "\\\\").replace('"', "\\\"")
}

/// Marks a step into a shadow root. A CSS selector cannot cross that boundary,
/// so a piercing selector is a sequence of ordinary ones and this is the join.
pub const PIERCE: &str = " >>> ";

/// Paths built only from tag names and positions, for the many real elements
/// that carry no id, no name and no label — most buttons on most sites, and
/// every control in the browser's own pages.
///
/// Computed from ONE document dump rather than a walk of round trips: the DOM
/// agent only fills in `parentId` for nodes it has already pushed, so asking
/// about a node found by hit-test gives no ancestors at all.
///
/// Shadow roots are walked as well. Chrome's own pages — the new tab among
/// them — put everything inside them, and a path that stops at the boundary
/// finds nothing at all.
fn path_within(root: &Value, target: i64) -> Vec<String> {
    fn find(node: &Value, target: i64, trail: &mut Vec<String>) -> bool {
        // Position among siblings of the same tag, which is what nth-of-type
        // counts — not among all children.
        let empty = Vec::new();
        let kids = node
            .get("children")
            .and_then(|c| c.as_array())
            .unwrap_or(&empty);
        let mut seen: std::collections::HashMap<String, usize> =
            std::collections::HashMap::new();
        for kid in kids {
            let Some(tag) = kid.get("localName").and_then(|l| l.as_str()) else {
                continue;
            };
            if tag.is_empty() {
                continue;
            }
            let n = seen.entry(tag.to_string()).or_insert(0);
            *n += 1;
            let step = format!("{tag}:nth-of-type({n})");
            trail.push(step);
            if kid.get("nodeId").and_then(|i| i.as_i64()) == Some(target) || find(kid, target, trail)
            {
                return true;
            }
            trail.pop();
        }

        // Into the shadow roots this node hosts.
        let roots = node
            .get("shadowRoots")
            .and_then(|c| c.as_array())
            .unwrap_or(&empty);
        for sr in roots {
            trail.push(PIERCE.trim().to_string());
            if sr.get("nodeId").and_then(|i| i.as_i64()) == Some(target) || find(sr, target, trail) {
                return true;
            }
            trail.pop();
        }
        false
    }

    let mut trail = Vec::new();
    if !find(root, target, &mut trail) {
        return Vec::new();
    }

    // Join, turning the boundary markers back into the piercing separator.
    let join = |parts: &[String]| -> String {
        let mut out = String::new();
        for part in parts {
            if part == ">>>" {
                out.push_str(PIERCE);
            } else {
                if !out.is_empty() && !out.ends_with(PIERCE) {
                    out.push_str(" > ");
                }
                out.push_str(part);
            }
        }
        out
    };

    // Shortest first, and the FULL path last.
    //
    // A bare tail is not a shorter version of the path — it is a descendant
    // pattern matched anywhere, so on a real page it hits dozens of subtrees
    // and the uniqueness check throws it away. Every one of these is still
    // verified before use, so offering the short ones costs nothing and wins a
    // sturdier selector when one of them genuinely is unique. A tail may not
    // begin part-way across a shadow boundary, so those are skipped.
    let mut out = Vec::new();
    for take in [2usize, 3, 4] {
        if trail.len() > take {
            let tail = &trail[trail.len() - take..];
            if tail.iter().any(|p| p == ">>>") {
                continue;
            }
            // Anchored at the last boundary, or it would search the wrong tree.
            if let Some(cut) = trail.iter().rposition(|p| p == ">>>") {
                if cut > trail.len() - take {
                    continue;
                }
                let mut parts = trail[..=cut].to_vec();
                parts.extend_from_slice(tail);
                out.push(join(&parts));
            } else {
                out.push(join(tail));
            }
        }
    }
    out.push(join(&trail));
    out.dedup();
    out
}

/// Everything up to and including the last shadow crossing on the way to this
/// node, or empty when it lives in the ordinary document.
fn shadow_prefix(root: &Value, target: i64) -> String {
    let full = match path_within(root, target).pop() {
        Some(p) => p,
        None => return String::new(),
    };
    match full.rfind(PIERCE.trim()) {
        Some(i) => format!("{}{PIERCE}", full[..i].trim_end()),
        None => String::new(),
    }
}

/// Runs a selector that may cross shadow boundaries, one hop at a time:
/// `host > path >>> inner > path`. Each hop is an ordinary querySelectorAll
/// under the previous hop's node, which is the only way across — CSS itself
/// cannot express it.
pub async fn query_piercing(profile_id: &str, selector: &str) -> Result<Vec<i64>> {
    let doc = page_call(profile_id, "DOM.getDocument", json!({ "depth": 0 })).await?;
    let root = doc
        .get("root")
        .and_then(|r| r.get("nodeId"))
        .and_then(|v| v.as_i64())
        .context("no document root")?;
    query_piercing_in(profile_id, root, selector).await
}

/// The same, but searching under a root the caller already has.
///
/// This distinction is load-bearing. DOM.getDocument REASSIGNS node ids: any id
/// obtained before it is meaningless afterwards. A caller comparing the result
/// against a node it found earlier must therefore search in that same id space,
/// or every match comes back as "unique, but a different node" — which is
/// exactly what happened when this fetched its own root.
pub async fn query_piercing_in(
    profile_id: &str,
    root_id: i64,
    selector: &str,
) -> Result<Vec<i64>> {
    let mut scope = vec![root_id];

    let hops: Vec<&str> = selector.split(">>>").map(|h| h.trim()).collect();
    for (i, hop) in hops.iter().enumerate() {
        if hop.is_empty() {
            continue;
        }
        let mut next = Vec::new();
        for node in &scope {
            let found = page_call(
                profile_id,
                "DOM.querySelectorAll",
                json!({ "nodeId": node, "selector": hop }),
            )
            .await;
            let Ok(found) = found else { continue };
            if let Some(ids) = found.get("nodeIds").and_then(|v| v.as_array()) {
                next.extend(ids.iter().filter_map(|v| v.as_i64()));
            }
        }
        // Between hops, step through each match's shadow root.
        if i + 1 < hops.len() {
            let mut hosts = Vec::new();
            for node in &next {
                let described =
                    page_call(profile_id, "DOM.describeNode", json!({ "nodeId": node })).await;
                let Ok(described) = described else { continue };
                if let Some(roots) = described
                    .get("node")
                    .and_then(|n| n.get("shadowRoots"))
                    .and_then(|r| r.as_array())
                {
                    hosts.extend(roots.iter().filter_map(|r| r.get("nodeId")?.as_i64()));
                }
            }
            next = hosts;
        }
        scope = next;
        if scope.is_empty() {
            break;
        }
    }
    Ok(scope)
}

/// Identifies the element under a viewport point WITHOUT running any script in/// Identifies the element under a viewport point WITHOUT running any script in
/// the page: the DOM domain answers all of this, and nothing here ever enters
/// a JavaScript execution context. `DOM.resolveNode` would — it mints a
/// Runtime.RemoteObject — so it is deliberately not used.
pub async fn pick_element(profile_id: &str, x: f64, y: f64) -> Result<Picked> {
    // Deep, because the structural fallback needs the tree and the hit-test
    // only reports a nodeId once the document has been requested.
    let root = page_call(
        profile_id,
        "DOM.getDocument",
        json!({ "depth": -1, "pierce": true }),
    )
    .await?;
    let root_node = root.get("root").cloned().unwrap_or(Value::Null);
    let root_id = root_node
        .get("nodeId")
        .and_then(|v| v.as_i64())
        .context("no document root")?;

    // The caller works in VIEWPORT coordinates, which is what Motion wants.
    // getNodeForLocation does not: its parameter is a DOCUMENT point, and the
    // DOM agent subtracts the layout viewport's scroll offset from it before
    // hit-testing (inspector_dom_agent.cc names the local `document_point` and
    // passes it through LocalFrameView::DocumentToFrame). Handing it a
    // viewport point therefore probes `y - scrollY`: correct only at the very
    // top of a page, and above the viewport entirely once the operator has
    // scrolled a screenful — which is when they actually click anything.
    let metrics = page_call(profile_id, "Page.getLayoutMetrics", json!({})).await?;
    let scroll = metrics
        .get("cssLayoutViewport")
        .cloned()
        .unwrap_or(Value::Null);
    let scroll_x = scroll.get("pageX").and_then(|v| v.as_f64()).unwrap_or(0.0);
    let scroll_y = scroll.get("pageY").and_then(|v| v.as_f64()).unwrap_or(0.0);

    let hit = page_call(
        profile_id,
        "DOM.getNodeForLocation",
        json!({
            "x": (x + scroll_x) as i64,
            "y": (y + scroll_y) as i64,
            "includeUserAgentShadowDOM": false,
        }),
    )
    .await?;
    let backend_id = hit
        .get("backendNodeId")
        .and_then(|v| v.as_i64())
        .context("nothing under that point")?;

    let described = page_call(
        profile_id,
        "DOM.describeNode",
        json!({ "backendNodeId": backend_id, "depth": 1 }),
    )
    .await?;
    let node = described.get("node").cloned().unwrap_or(Value::Null);
    let node_id = hit.get("nodeId").and_then(|v| v.as_i64());

    let tag = node
        .get("localName")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let empty: Vec<Value> = Vec::new();
    let attrs = node
        .get("attributes")
        .and_then(|a| a.as_array())
        .unwrap_or(&empty);

    // Ordered by how well each survives, and LANGUAGE-INDEPENDENT ones come
    // first — that ordering is the whole point.
    //
    // A label reads well in a recording ("Режим ИИ") and is the worst possible
    // thing to match on: the same button is "AI Mode" the moment the page is
    // served in another language, or when the site rewords it. Anything that
    // carries visible text is therefore a last resort, tried only after the
    // structural path, which cannot be translated at all.
    // An attribute selector still cannot cross a shadow boundary, so anything
    // built from this node's own attributes has to be anchored inside the tree
    // the node actually lives in. Without this every candidate for an element
    // in the browser's own pages came back with nought matches.
    let prefix = node_id
        .map(|id| shadow_prefix(&root_node, id))
        .unwrap_or_default();

    let mut candidates: Vec<String> = Vec::new();
    if let Some(v) = attr(attrs, "id") {
        candidates.push(format!("{prefix}#{v}"));
    }
    for name in ["data-testid", "data-test", "data-qa", "data-id", "name", "jsname"] {
        if let Some(v) = attr(attrs, name) {
            candidates.push(format!("{prefix}[{name}=\"{}\"]", css_escape(v)));
        }
    }
    if let Some(v) = attr(attrs, "type") {
        if tag == "input" || tag == "button" {
            candidates.push(format!("{prefix}{tag}[type=\"{}\"]", css_escape(v)));
        }
    }

    // A structural path, for the many real elements that carry no id, no name
    // and no label — most buttons on most sites. Walks up the tree adding
    // `tag:nth-of-type(k)` until the path is unique. Less durable than an id,
    // but far better than a screen position, which stops being right the
    // moment the window is a different size.
    if let Some(id) = node_id {
        candidates.extend(path_within(&root_node, id));
    }

    // Only now the ones that carry words. Better than a screen position, worse
    // than everything above, and useless the day the page changes language.
    for name in ["aria-label", "placeholder", "title", "alt"] {
        if let Some(v) = attr(attrs, name) {
            candidates.push(format!("{prefix}{tag}[{name}=\"{}\"]", css_escape(v)));
        }
    }

    // Keep the first candidate that matches exactly one node, and that node
    // ours. A selector that matches three buttons would click the wrong one on
    // the next run, which is worse than falling back to the point.
    let mut selector = String::new();
    let mut tried: Vec<String> = Vec::new();
    if node_id.is_none() {
        tried.push(
            "the page gave no node id — DOM.getDocument did not register the document".into(),
        );
    }
    for cand in candidates {
        let ids = match query_piercing_in(profile_id, root_id, &cand).await {
            Ok(v) => v,
            Err(e) => {
                tried.push(format!("{cand} — rejected by the page ({e})"));
                continue;
            }
        };
        if ids.len() != 1 {
            tried.push(format!("{cand} — {} matches", ids.len()));
            continue;
        }
        if let Some(mine) = node_id {
            // With the document requested we can confirm it is the same node.
            if ids[0] != mine {
                tried.push(format!("{cand} — unique, but a different node"));
                continue;
            }
        }
        selector = cand;
        break;
    }

    // A label for the step, from whatever the element carries. The child text
    // node comes back with depth:1, so no script is needed for it either.
    let text = node
        .get("children")
        .and_then(|c| c.as_array())
        .and_then(|c| {
            c.iter()
                .find(|n| n.get("nodeType").and_then(|t| t.as_i64()) == Some(3))
        })
        .and_then(|n| n.get("nodeValue"))
        .and_then(|v| v.as_str())
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .map(|s| s.chars().take(40).collect::<String>());

    let label = text
        .or_else(|| attr(attrs, "aria-label").map(str::to_string))
        .or_else(|| attr(attrs, "placeholder").map(str::to_string))
        .or_else(|| attr(attrs, "name").map(str::to_string))
        .unwrap_or_default();

    Ok(Picked { selector, tag, label, x, y, tried })
}

/// Waits for one occurrence of a CDP event, or gives up.
///
/// Subscribes BEFORE the caller does whatever triggers it — a subscription
/// taken afterwards misses events that arrive in between, which for a fast
/// navigation is most of them.
pub struct EventWait {
    rx: broadcast::Receiver<String>,
}

pub fn watch(profile_id: &str) -> Option<EventWait> {
    let s = get(profile_id)?;
    Some(EventWait { rx: s.events.subscribe() })
}

impl EventWait {
    pub async fn until(mut self, method: &str, timeout: std::time::Duration) -> bool {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            let left = deadline.saturating_duration_since(tokio::time::Instant::now());
            if left.is_zero() {
                return false;
            }
            match tokio::time::timeout(left, self.rx.recv()).await {
                Ok(Ok(raw)) => {
                    let m = serde_json::from_str::<Value>(&raw)
                        .ok()
                        .and_then(|v| v.get("method").and_then(|x| x.as_str()).map(str::to_string));
                    if m.as_deref() == Some(method) {
                        return true;
                    }
                    continue;
                }
                // Lagged, or the socket closed: the event we want may have
                // been among what was dropped, so stop rather than hang.
                Ok(Err(_)) => return false,
                Err(_) => return false,
            }
        }
    }
}
