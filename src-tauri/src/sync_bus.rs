//! Loopback broker for window sync: one TCP connection per browser, one JSON
//! line per input event, copied to the group's other members. Read only for
//! its "t", which marks an action: nothing else a member sends is passed on.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;

/// One connected browser.
struct Member {
    id: u64,
    /// The profile this window runs; the panel names and addresses members by it.
    profile: String,
    /// Held out without leaving: neither sends nor receives while excluded.
    excluded: bool,
    /// The box this window may occupy: the layout's only input besides the screen.
    win: WindowBox,
    /// Its greeting said it knows `{"quit":true}`; an older engine would ignore the line.
    quits: bool,
    tx: mpsc::UnboundedSender<String>,
}

#[derive(Default)]
struct State {
    /// group name -> members
    groups: HashMap<String, Vec<Member>>,
    /// group name -> suspended
    paused: HashMap<String, bool>,
    /// group name -> id of the member that published most recently
    driving: HashMap<String, u64>,
    /// group name -> layout last asked for and the area it used. Remembered so a
    /// window that comes up after the layout was chosen is placed too.
    arranged: HashMap<String, (Layout, (i32, i32, i32, i32))>,
    /// profile -> what the page helper last found there.
    helper: HashMap<String, serde_json::Value>,
    /// profile -> the field set the panel was dismissed for. Closing the panel
    /// answers about THIS page; a different field set is a different question.
    helper_dismissed: HashMap<String, String>,
    next_id: u64,
}

pub struct Bus {
    pub port: u16,
    pub token: String,
    state: Arc<Mutex<State>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemberStatus {
    pub profile: String,
    pub excluded: bool,
    /// True for the window that published most recently — the one driving.
    pub driving: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GroupStatus {
    pub group: String,
    pub members: Vec<MemberStatus>,
    pub paused: bool,
}

#[derive(Deserialize)]
struct Hello {
    hello: String,
    #[serde(default)]
    token: String,
    #[serde(default)]
    profile: String,
    /// Absent from an older engine: an all-zero box means no preference, and
    /// that member is laid out the way it always was.
    #[serde(default)]
    win: WindowBox,
    /// Absent from an older engine, which is then never sent `{"quit":true}`.
    #[serde(default)]
    quit: bool,
}

/// The size a member's window may be. Reported by the browser: only it knows
/// the screen the profile claims and whether that profile is a handset.
#[derive(Debug, Clone, Copy, Default, Deserialize)]
pub struct WindowBox {
    /// The size the window came up at, in DIP.
    #[serde(default)]
    pub w: i32,
    #[serde(default)]
    pub h: i32,
    /// The screen the profile claims — the OS refuses anything larger.
    #[serde(default)]
    pub max_w: i32,
    #[serde(default)]
    pub max_h: i32,
    /// A handset: its page area IS its screen, so it cannot be resized at all.
    #[serde(default)]
    pub fixed: bool,
}

impl WindowBox {
    /// Size for a slot `cap` wide and tall: never past the claimed screen (the
    /// OS refuses that, which left gaps in a phone fleet), and a handset is fixed.
    fn size_for(&self, cap_w: i32, cap_h: i32) -> (i32, i32) {
        if self.w <= 0 || self.h <= 0 {
            return (cap_w.max(1), cap_h.max(1));
        }
        if self.fixed {
            return (self.w, self.h);
        }
        let ceil_w = if self.max_w > 0 { self.max_w } else { self.w };
        let ceil_h = if self.max_h > 0 { self.max_h } else { self.h };
        (cap_w.clamp(1, ceil_w), cap_h.clamp(1, ceil_h))
    }
}

/// How to lay the group's windows out on screen.
#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Layout {
    /// Side by side in one row. Best for two or three.
    Row,
    /// As square a grid as the count allows.
    Grid,
    /// Overlapping, offset down-right; every title bar stays clickable.
    Cascade,
}

/// A mirrored action: a JSON object naming its type in "t". A control never does, which is how
/// the engine tells them apart.
fn is_action(line: &str) -> bool {
    serde_json::from_str::<serde_json::Map<String, serde_json::Value>>(line).is_ok_and(|m| m.contains_key("t"))
}

impl Bus {
    /// Binds an ephemeral loopback port and accepts. Loopback only — this
    /// carries the operator's keystrokes.
    pub async fn start(token: String) -> Result<Arc<Bus>> {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .context("bind sync bus")?;
        let port = listener.local_addr()?.port();
        let bus = Arc::new(Bus {
            port,
            token,
            state: Arc::new(Mutex::new(State::default())),
        });
        let accept_bus = bus.clone();
        tokio::spawn(async move {
            loop {
                match listener.accept().await {
                    Ok((stream, _)) => {
                        let b = accept_bus.clone();
                        tokio::spawn(async move { b.serve(stream).await });
                    }
                    Err(e) => {
                        eprintln!("[bus] accept failed: {e}");
                        return;
                    }
                }
            }
        });
        Ok(bus)
    }

    async fn serve(self: Arc<Self>, stream: TcpStream) {
        // Nagle would hold a keystroke back waiting for company.
        let _ = stream.set_nodelay(true);
        let (read_half, mut write_half) = stream.into_split();
        let mut lines = BufReader::new(read_half).lines();

        // Greeting first: loopback is not authorisation, any local process can
        // connect, so the token is what proves this launcher started it.
        let Ok(Some(first)) = lines.next_line().await else {
            return;
        };
        let Ok(hello) = serde_json::from_str::<Hello>(&first) else {
            return;
        };
        if hello.token != self.token {
            eprintln!("[bus] rejected a connection with a bad token");
            return;
        }
        let group = hello.hello;

        let (tx, mut rx) = mpsc::unbounded_channel::<String>();
        // This window's own line back, for answers meant for it alone.
        let own = tx.clone();
        let id = {
            let mut st = self.state.lock().unwrap();
            st.next_id += 1;
            let id = st.next_id;
            st.groups.entry(group.clone()).or_default().push(Member {
                id,
                profile: hello.profile.clone(),
                excluded: false,
                win: hello.win,
                quits: hello.quit,
                tx,
            });
            // A member joining a suspended group must not start acting.
            if *st.paused.get(&group).unwrap_or(&false) {
                let members = st.groups.get(&group).unwrap();
                if let Some(m) = members.iter().find(|m| m.id == id) {
                    let _ = m.tx.send("{\"suspended\":true}\n".to_string());
                }
            }
            id
        };

        // Re-place every member, not just the newcomer: slots depend on the count.
        let replay = self
            .state
            .lock()
            .unwrap()
            .arranged
            .get(&group)
            .copied();
        if let Some((layout, area)) = replay {
            self.arrange(&group, layout, area);
        }

        let writer = tokio::spawn(async move {
            while let Some(line) = rx.recv().await {
                if write_half.write_all(line.as_bytes()).await.is_err() {
                    return;
                }
            }
        });

        while let Ok(Some(line)) = lines.next_line().await {
            if line.is_empty() {
                continue;
            }
            // "Change IP" in the browser (#162): done here, answered to that window alone.
            if line.contains("\"change_ip\"") {
                if serde_json::from_str::<serde_json::Value>(&line)
                    .is_ok_and(|v| v.get("change_ip").and_then(|c| c.as_bool()) == Some(true))
                {
                    let profile = hello.profile.clone();
                    let own = own.clone();
                    tokio::spawn(async move {
                        let answer = crate::launch::change_ip_from_browser(&profile).await;
                        let _ = own.send(format!("{answer}\n"));
                    });
                    continue;
                }
            }
            // A helper report is for the launcher, not for the group: it
            // describes one page in one window.
            if line.contains("\"helper\"") {
                if let Ok(v) = serde_json::from_str::<serde_json::Value>(&line) {
                    if let Some(h) = v.get("helper") {
                        let mut st = self.state.lock().unwrap();
                        st.helper.insert(hello.profile.clone(), h.clone());
                    }
                    continue;
                }
            }
            // Only actions go to the group. Anything else would reach every other member as an
            // instruction from the panel, and every lone browser shares the "default" group.
            if is_action(&line) {
                self.fan_out(&group, id, &line);
            }
        }

        // EOF: the browser closed or died. No heartbeats, no stale entries.
        {
            let mut st = self.state.lock().unwrap();
            if let Some(members) = st.groups.get_mut(&group) {
                members.retain(|m| m.id != id);
                if members.is_empty() {
                    st.groups.remove(&group);
                }
            }
            // The window is gone, so its offer goes with it; unless the browser is still here on
            // a newer connection, as after it greeted again for a new group.
            if !st.groups.values().flatten().any(|m| m.profile == hello.profile) {
                st.helper.remove(&hello.profile);
            }
        }
        writer.abort();
    }

    fn fan_out(&self, group: &str, from: u64, line: &str) {
        let mut st = self.state.lock().unwrap();
        if *st.paused.get(group).unwrap_or(&false) {
            return;
        }
        st.driving.insert(group.to_string(), from);
        let Some(members) = st.groups.get(group) else {
            return;
        };
        // An excluded window neither sends nor receives.
        if members.iter().any(|m| m.id == from && m.excluded) {
            return;
        }
        let framed = format!("{line}\n");
        for m in members {
            // Never back to the sender — that is the echo to avoid.
            if m.id != from && !m.excluded {
                let _ = m.tx.send(framed.clone());
            }
        }
    }

    /// Lays the group's windows out inside `area` (the usable screen). The
    /// launcher assigns slots; each window then moves itself — moving them from
    /// here would need an Accessibility grant on macOS.
    pub fn arrange(&self, group: &str, layout: Layout, area: (i32, i32, i32, i32)) {
        let mut st = self.state.lock().unwrap();
        st.arranged.insert(group.to_string(), (layout, area));
        let Some(members) = st.groups.get(group) else {
            return;
        };
        let n = members.len();
        if n == 0 {
            return;
        }
        let (ax, ay, aw, ah) = area;

        // Level the heights first: desktop profiles claim different screens, so
        // untouched they make a ragged row. Handsets cannot resize, so no vote.
        let flexible: Vec<&Member> = members.iter().filter(|m| !m.win.fixed).collect();
        let level_h = flexible
            .iter()
            .map(|m| {
                let ceil = if m.win.max_h > 0 { m.win.max_h } else { m.win.h };
                if ceil > 0 { ceil } else { ah }
            })
            .min()
            .unwrap_or(ah)
            .min(ah);

        let cols = match layout {
            Layout::Row => n,
            Layout::Grid => (n as f64).sqrt().ceil() as usize,
            Layout::Cascade => n,
        };
        let rows = match layout {
            Layout::Cascade => 1,
            _ => n.div_ceil(cols.max(1)),
        };

        // Sizes first: a row cannot be packed before its widths are known.
        let sizes: Vec<(i32, i32)> = members
            .iter()
            .map(|m| {
                let cap_w = match layout {
                    Layout::Cascade => aw * 3 / 4,
                    _ => aw / cols.max(1) as i32,
                };
                let cap_h = match layout {
                    Layout::Cascade => ah * 3 / 4,
                    _ => (ah / rows.max(1) as i32).min(level_h),
                };
                m.win.size_for(cap_w, cap_h)
            })
            .collect();

        // Packed left to right at their own size, wrapping when the next will
        // not fit; scaling one down would ask for a size the device cannot make.
        let mut placed: Vec<(i32, i32, i32, i32)> = Vec::with_capacity(n);
        match layout {
            Layout::Cascade => {
                let step = 32.min(ah / (n as i32 + 1).max(1));
                for (i, (w, h)) in sizes.iter().enumerate() {
                    placed.push((ax + step * i as i32, ay + step * i as i32, *w, *h));
                }
            }
            _ => {
                let mut x = ax;
                let mut y = ay;
                let mut row_h = 0;
                for (w, h) in &sizes {
                    if x > ax && x + w > ax + aw {
                        x = ax;
                        y += row_h;
                        row_h = 0;
                    }
                    placed.push((x, y, *w, *h));
                    x += w;
                    row_h = row_h.max(*h);
                }
                // A single row wider than the screen: overlap evenly so every
                // title bar stays reachable instead of the tail hanging off.
                let total: i32 = sizes.iter().map(|(w, _)| *w).sum();
                if rows == 1 && total > aw && n > 1 {
                    let step = (aw - sizes[n - 1].0).max(0) / (n as i32 - 1);
                    for (i, slot) in placed.iter_mut().enumerate() {
                        slot.0 = ax + step * i as i32;
                        slot.1 = ay;
                    }
                }
            }
        }

        for (m, (x, y, w, h)) in members.iter().zip(placed) {
            let line = format!(
                "{{\"bounds\":{{\"x\":{x},\"y\":{y},\"w\":{w},\"h\":{h}}},\"activate\":true}}\n"
            );
            let _ = m.tx.send(line);
        }
    }

    /// Asks every window to close — a browser asked to close writes out its
    /// session and cookies; a killed process does not.
    pub fn stop(&self, group: &str) {
        let st = self.state.lock().unwrap();
        if let Some(members) = st.groups.get(group) {
            for m in members {
                let _ = m.tx.send("{\"close\":true}\n".to_string());
            }
        }
    }

    /// What the page helper last found in `profile`, if anything.
    pub fn helper_fields(&self, profile: &str) -> Option<serde_json::Value> {
        self.state.lock().unwrap().helper.get(profile).cloned()
    }

    /// The kinds on offer, comparable between reports. Kinds, not positions —
    /// a page that reflows on scroll is still the same offer.
    fn helper_key(fields: &serde_json::Value) -> String {
        let Some(list) = fields.get("fields").and_then(|f| f.as_array()) else {
            return String::new();
        };
        let mut kinds: Vec<&str> = list
            .iter()
            .filter_map(|f| f.get("kind").and_then(|k| k.as_str()))
            .collect();
        kinds.sort_unstable();
        kinds.join(",")
    }

    /// The operator closed the panel. Remember what it was offering.
    pub fn helper_dismiss(&self, profile: &str) {
        let mut st = self.state.lock().unwrap();
        let key = st.helper.get(profile).map(Self::helper_key).unwrap_or_default();
        st.helper_dismissed.insert(profile.to_string(), key);
    }

    /// Every profile with something fillable on screen. `triggers` narrows it to
    /// the kinds wanted (empty = all); filtered here so the setting is live.
    pub fn helper_profiles(&self, triggers: &[String]) -> Vec<String> {
        let st = self.state.lock().unwrap();
        st.helper
            .iter()
            .filter(|(profile, v)| {
                // Dismissed and still the same offer. A different step asks for
                // different kinds and so comes back on its own.
                if st.helper_dismissed.get(*profile) == Some(&Self::helper_key(*v)) {
                    return false;
                }
                let Some(fields) = v.get("fields").and_then(|f| f.as_array()) else {
                    return false;
                };
                fields.iter().any(|f| {
                    let kind = f.get("kind").and_then(|k| k.as_str()).unwrap_or("");
                    triggers.is_empty() || triggers.iter().any(|t| t == kind)
                })
            })
            .map(|(k, _)| k.clone())
            .collect()
    }

    /// Tells every window in a group to fill: a command each, so each fills with
    /// its own person. Returns how many were told.
    pub fn fill_group(&self, group: &str) -> usize {
        let st = self.state.lock().unwrap();
        let Some(members) = st.groups.get(group) else {
            return 0;
        };
        let mut told = 0;
        for m in members {
            if !m.excluded && m.tx.send("{\"fill\":true}\n".to_string()).is_ok() {
                told += 1;
            }
        }
        told
    }

    /// Which group a profile belongs to, if any.
    pub fn group_of(&self, profile: &str) -> Option<String> {
        let st = self.state.lock().unwrap();
        for (group, members) in &st.groups {
            if members.iter().any(|m| m.profile == profile) {
                return Some(group.clone());
            }
        }
        None
    }

    /// Asks one profile's browser to quit as its Quit menu does, cookies written out.
    /// False when that browser is not on the bus (an engine without the quit joins it only
    /// with the helper or a group).
    pub fn close(&self, profile: &str) -> bool {
        let st = self.state.lock().unwrap();
        let mut sent = false;
        for m in st.groups.values().flatten().filter(|m| m.profile == profile) {
            sent |= m.tx.send("{\"close\":true}\n".to_string()).is_ok();
        }
        sent
    }

    /// Whether one profile's browser is on the bus and said it knows `{"quit":true}`.
    pub fn quits(&self, profile: &str) -> bool {
        let st = self.state.lock().unwrap();
        st.groups.values().flatten().any(|m| m.profile == profile && m.quits)
    }

    /// Stop's ask: quits one profile's browser as `close` does, but past a "Leave site?"
    /// prompt. Only to an engine that said it knows it; false when none on the bus did.
    pub fn quit(&self, profile: &str) -> bool {
        let st = self.state.lock().unwrap();
        let mut sent = false;
        for m in st.groups.values().flatten().filter(|m| m.profile == profile && m.quits) {
            sent |= m.tx.send("{\"quit\":true}\n".to_string()).is_ok();
        }
        sent
    }

    /// Tells one profile's window to fill what its helper found.
    pub fn fill(&self, profile: &str) {
        let st = self.state.lock().unwrap();
        for members in st.groups.values() {
            for m in members {
                if m.profile == profile {
                    let _ = m.tx.send("{\"fill\":true}\n".to_string());
                }
            }
        }
    }

    /// Brings one profile's window to the front, with the keyboard.
    ///
    /// Asked of the browser rather than done from here: on macOS moving or
    /// raising another application's window needs an Accessibility grant,
    /// and the window belongs to the browser anyway. Returns whether anything
    /// was listening, so the caller can say so when nothing was.
    pub fn focus(&self, profile: &str) -> bool {
        let st = self.state.lock().unwrap();
        let mut sent = false;
        for members in st.groups.values() {
            for m in members {
                if m.profile == profile {
                    if m.tx.send("{\"focus\":true}\n".to_string()).is_ok() {
                        sent = true;
                    }
                }
            }
        }
        sent
    }

    pub fn set_excluded(&self, group: &str, profile: &str, excluded: bool) {
        let mut st = self.state.lock().unwrap();
        if let Some(members) = st.groups.get_mut(group) {
            for m in members.iter_mut() {
                if m.profile == profile {
                    m.excluded = excluded;
                    // Tell the window too, so it stops performing.
                    let _ = m.tx.send(format!("{{\"suspended\":{excluded}}}\n"));
                }
            }
        }
    }

    pub fn status(&self, group: &str) -> GroupStatus {
        let st = self.state.lock().unwrap();
        let driving = st.driving.get(group).copied().unwrap_or(0);
        GroupStatus {
            group: group.to_string(),
            members: st
                .groups
                .get(group)
                .map(|ms| {
                    ms.iter()
                        .map(|m| MemberStatus {
                            profile: m.profile.clone(),
                            excluded: m.excluded,
                            driving: m.id == driving,
                        })
                        .collect()
                })
                .unwrap_or_default(),
            paused: *st.paused.get(group).unwrap_or(&false),
        }
    }

    /// Suspends or resumes a whole group at once.
    pub fn set_paused(&self, group: &str, paused: bool) {
        let st = self.state.lock().unwrap();
        let mut st = st;
        st.paused.insert(group.to_string(), paused);
        if let Some(members) = st.groups.get(group) {
            let line = format!("{{\"suspended\":{paused}}}\n");
            for m in members {
                let _ = m.tx.send(line.clone());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_an_object_naming_its_type_is_an_action() {
        assert!(is_action(r#"{"t":"mouse","n":1}"#));
        for line in [r#"{"quit":true}"#, r#"{"close":true,"x":"\"t\":1"}"#, r#"["t"]"#, "t", ""] {
            assert!(!is_action(line), "{line}");
        }
    }
}
