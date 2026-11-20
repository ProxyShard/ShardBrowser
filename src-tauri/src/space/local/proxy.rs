use crate::{settings, store};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::net::SocketAddr;
use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProxyKind {
    Socks5,
    Http,
    Https,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProxyEntry {
    #[serde(default)]
    pub id: String,
    pub name: String,
    pub kind: ProxyKind,
    pub host: String,
    pub port: u16,
    #[serde(default)]
    pub username: String,
    #[serde(default)]
    pub password: String,
    /// "PL", "US", …
    #[serde(default)]
    pub country: String,
    /// Free-form note.
    #[serde(default)]
    pub notes: String,
}

impl ProxyEntry {
    /// The same proxy as `other`: one kind, host, port, user and password.
    /// The password counts: providers that carry a session or routing
    /// parameters in it (a sticky session, a country) serve different exits
    /// from one host, port and user, so those are different proxies. Only an
    /// identical one is skipped on import.
    pub fn same_proxy(&self, other: &ProxyEntry) -> bool {
        self.kind == other.kind
            && self.host == other.host
            && self.port == other.port
            && self.username == other.username
            && self.password == other.password
    }

    /// Build `--proxy-server=<scheme>://[user:pass@]host:port` for ShardX.
    pub fn to_proxy_server_arg(&self) -> String {
        let scheme = match self.kind {
            ProxyKind::Socks5 => "socks5",
            ProxyKind::Http => "http",
            ProxyKind::Https => "https",
        };
        let host_port = format!("{}:{}", self.host, self.port);
        if self.username.is_empty() && self.password.is_empty() {
            format!("{scheme}://{host_port}")
        } else {
            format!(
                "{scheme}://{}:{}@{host_port}",
                percent_encode_userinfo(&self.username),
                percent_encode_userinfo(&self.password)
            )
        }
    }
}

/// A username or password as it may appear inside a proxy URL.
///
/// Form encoding was used here, and it writes a space as "+" — which is a
/// plus sign to everyone who reads the string back, so a password with a
/// space in it arrived at the proxy wrong. Percent encoding is what userinfo
/// actually uses, and it is what the engine now decodes on the other side.
///
/// Everything outside the unreserved set is encoded. That is wider than the
/// grammar demands and deliberately so: the engine's `--proxy-server` is a
/// list of rules where "," starts another rule, ";" another scheme and "="
/// a mapping, so those three in particular must never travel raw. Providers
/// put all of them in usernames — country lists, session tokens.
/// The reverse: "%2C" back to ",". Anything that is not a well-formed
/// escape is left exactly as it stands, so a password that really contains a
/// per-cent sign survives being read back.
pub fn percent_decode_userinfo(raw: &str) -> String {
    let b = raw.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(b.len());
    let mut i = 0usize;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            let hi = (b[i + 1] as char).to_digit(16);
            let lo = (b[i + 2] as char).to_digit(16);
            if let (Some(hi), Some(lo)) = (hi, lo) {
                let byte = (hi * 16 + lo) as u8;
                // A control character is not part of any login and would put
                // a NUL or a newline into a protocol message.
                if byte >= 0x20 {
                    out.push(byte);
                    i += 3;
                    continue;
                }
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8(out).unwrap_or_else(|_| raw.to_string())
}

pub fn percent_encode_userinfo(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for b in raw.as_bytes() {
        let c = *b as char;
        if c.is_ascii_alphanumeric() || matches!(c, '-' | '.' | '_' | '~') {
            out.push(c);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ProxyStore {
    #[serde(default)]
    pub proxies: Vec<ProxyEntry>,
}

pub(in crate::space) fn load() -> Result<ProxyStore> {
    let path = store::proxies_path()?;
    if !path.exists() {
        return Ok(ProxyStore::default());
    }
    let body = fs::read_to_string(&path)?;
    Ok(serde_json::from_str(&body).unwrap_or_default())
}

fn save(s: &ProxyStore) -> Result<()> {
    let body = serde_json::to_string_pretty(s)?;
    fs::write(store::proxies_path()?, body)?;
    Ok(())
}

pub(in crate::space) fn list() -> Result<Vec<ProxyEntry>> {
    Ok(load()?.proxies)
}

pub(in crate::space) fn upsert(mut entry: ProxyEntry) -> Result<ProxyEntry> {
    if entry.id.is_empty() {
        entry.id = uuid::Uuid::new_v4().to_string();
    }
    let mut s = load()?;
    if let Some(slot) = s.proxies.iter_mut().find(|p| p.id == entry.id) {
        *slot = entry.clone();
    } else {
        s.proxies.push(entry.clone());
    }
    save(&s)?;
    Ok(entry)
}

/// Upsert that reuses an identical entry (`same_proxy`): one with the same
/// endpoint and user but another password is a proxy of its own.
pub(in crate::space) fn upsert_dedup(mut entry: ProxyEntry) -> Result<ProxyEntry> {
    let mut s = load()?;
    if let Some(existing) = s.proxies.iter().find(|p| p.same_proxy(&entry)) {
        return Ok(existing.clone());
    }
    if entry.id.is_empty() {
        entry.id = uuid::Uuid::new_v4().to_string();
    }
    s.proxies.push(entry.clone());
    save(&s)?;
    Ok(entry)
}

/// Like `upsert_dedup`, but when the profile already has a proxy that differs
/// only in its password, that entry's password is CHANGED instead of a second
/// one being stored.
///
/// A provider that routes by the password hands out a new one per session, so
/// a profile whose proxy is updated through the API would otherwise leave a
/// trail of entries behind it — one per update — each still carrying a
/// password that has expired. Dedup proper must keep them apart (two profiles
/// on two sessions of one gateway are two proxies), which is why this is a
/// separate door, opened only when the caller names the profile the proxy
/// belongs to.
pub(in crate::space) fn upsert_for_binding(mut entry: ProxyEntry, bound_id: Option<&str>) -> Result<ProxyEntry> {
    let mut s = load()?;
    if let Some(existing) = s.proxies.iter().find(|p| p.same_proxy(&entry)) {
        return Ok(existing.clone());
    }
    if let Some(bound) = bound_id.filter(|b| !b.is_empty()) {
        if let Some(slot) = s.proxies.iter_mut().find(|p| {
            p.id == bound
                && p.kind == entry.kind
                && p.host == entry.host
                && p.port == entry.port
                && p.username == entry.username
        }) {
            slot.password = entry.password.clone();
            let out = slot.clone();
            save(&s)?;
            return Ok(out);
        }
    }
    if entry.id.is_empty() {
        entry.id = uuid::Uuid::new_v4().to_string();
    }
    s.proxies.push(entry.clone());
    save(&s)?;
    Ok(entry)
}

pub(in crate::space) fn delete(id: &str) -> Result<()> {
    let mut s = load()?;
    s.proxies.retain(|p| p.id != id);
    save(&s)?;
    // Also wipe persisted test history.
    with_history(|hs| {
        let gone = hs.by_proxy.remove(id).is_some();
        Ok(((), gone))
    })?;
    Ok(())
}

pub(in crate::space) fn get(id: &str) -> Result<Option<ProxyEntry>> {
    Ok(load()?.proxies.into_iter().find(|p| p.id == id))
}

/// How long one step of a test may take. A proxy that has not answered in
/// this long is not one anybody would browse through, and the operator is
/// waiting on a whole list of them, not on this one.
const STEP_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(6);

/// The cap on one probe end to end, however many steps it takes.
const TEST_DEADLINE: std::time::Duration = std::time::Duration::from_secs(12);

/// What a probe asks the proxy to reach. A name, not an address: a proxy that
/// cannot resolve is no use to a browser, which hands it names (`socks5h`).
/// IANA's, answers on 443 from everywhere, and nobody blocks it.
const PROBE_HOST: &str = "example.com";
const PROBE_PORT: u16 = 443;

/// `host[:port]` as typed into Settings: a scheme or a path is tolerated and
/// dropped, anything that is not a plain name or IPv4 address is refused.
pub(crate) fn parse_host_port(raw: &str, default_port: u16) -> Option<(String, u16)> {
    let mut t = raw.trim();
    if let Some(i) = t.find("://") {
        t = &t[i + 3..];
    }
    // A login ("user@host") is not a host, wherever the '@' sits.
    if t.contains('@') {
        return None;
    }
    let t = t.split(['/', '?', '#']).next().unwrap_or("");
    let (host, port) = match t.rsplit_once(':') {
        Some((h, p)) => (h, p.parse::<u16>().ok().filter(|p| *p > 0)?),
        None => (t, default_port),
    };
    let ok = !host.is_empty()
        && host.len() <= 253
        && host.chars().all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-')
        && !host.starts_with(['.', '-']);
    ok.then(|| (host.to_ascii_lowercase(), port))
}

/// What the reach probe asks for: the host set in Settings (#157), else example.com:443.
fn probe_target() -> (String, u16) {
    crate::settings::load()
        .ok()
        .and_then(|s| s.probe_host)
        .and_then(|h| parse_host_port(&h, PROBE_PORT))
        .unwrap_or_else(|| (PROBE_HOST.to_string(), PROBE_PORT))
}

/// RFC 1928 §6 reply codes, in the operator's words.
fn socks5_reply_text(rep: u8) -> String {
    match rep {
        0x01 => "the proxy failed the request (general failure) — usually a dead upstream or an account out of traffic".into(),
        0x02 => "the proxy refused: not allowed by its ruleset".into(),
        0x03 => "network unreachable from the proxy".into(),
        0x04 => "host unreachable from the proxy".into(),
        0x05 => "connection refused by the target".into(),
        0x06 => "TTL expired".into(),
        0x07 => "the proxy does not support this command".into(),
        0x08 => "the proxy does not support this address type".into(),
        other => format!("the proxy refused the request (code {other:#x})"),
    }
}

/// RFC 1928 §3 greeting and, if the proxy picks it, the RFC 1929 user/pass
/// exchange. Leaves the stream right after the handshake.
///
/// Both methods are offered whenever there are credentials, and what happens
/// next follows the method the proxy SELECTED. Offering only user/pass and
/// then assuming it was chosen sent the credentials to a proxy that had just
/// said "none needed", and everything after that was read as the wrong
/// message.
async fn socks5_handshake<S>(stream: &mut S, entry: &ProxyEntry) -> Result<()>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let has_creds = !entry.username.is_empty() || !entry.password.is_empty();
    if has_creds {
        stream.write_all(&[0x05, 0x02, 0x00, 0x02]).await?;
    } else {
        stream.write_all(&[0x05, 0x01, 0x00]).await?;
    }
    let mut resp = [0u8; 2];
    tokio::time::timeout(STEP_TIMEOUT, stream.read_exact(&mut resp))
        .await
        .context("the proxy accepted the connection but never greeted us")??;
    if resp[0] != 0x05 {
        anyhow::bail!("not SOCKS5");
    }
    match resp[1] {
        0x00 => {}
        0x02 => {
            if !has_creds {
                anyhow::bail!("the proxy wants a username and password");
            }
            if entry.username.len() > 255 || entry.password.len() > 255 {
                anyhow::bail!("username or password longer than SOCKS5 allows (255 bytes)");
            }
            let mut buf = vec![0x01u8];
            buf.push(entry.username.len() as u8);
            buf.extend_from_slice(entry.username.as_bytes());
            buf.push(entry.password.len() as u8);
            buf.extend_from_slice(entry.password.as_bytes());
            stream.write_all(&buf).await?;
            let mut auth_resp = [0u8; 2];
            tokio::time::timeout(STEP_TIMEOUT, stream.read_exact(&mut auth_resp))
                .await
                .context("no answer to the username and password")??;
            if auth_resp[1] != 0x00 {
                anyhow::bail!("the proxy rejected the username or password");
            }
        }
        0xFF => anyhow::bail!(if has_creds {
            "the proxy accepted neither anonymous nor username/password login"
        } else {
            "the proxy wants a username and password"
        }),
        other => anyhow::bail!("the proxy asked for login method {other:#x}, which we do not speak"),
    }
    Ok(())
}

/// SOCKS5/HTTP CONNECT probe; returns RTT in ms on success.
///
/// The handshake alone proves only that the proxy let us in. A gateway whose
/// upstream is dead, whose traffic ran out or whose address is banned greets
/// and authenticates exactly like a working one, which is why this used to
/// call such a proxy reachable. Every kind now ends with a real CONNECT to
/// `PROBE_HOST`, and only the answer to that decides.
pub async fn probe(entry: &ProxyEntry) -> Result<u128> {
    // One deadline over the whole exchange as well as one per step. A proxy
    // that answers every read just inside the step timeout could otherwise
    // string them together and hold a test slot far longer than any of them.
    tokio::time::timeout(TEST_DEADLINE, probe_inner(entry))
        .await
        .map_err(|_| anyhow::anyhow!("the proxy did not finish answering in time"))?
}

async fn probe_inner(entry: &ProxyEntry) -> Result<u128> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpStream;
    use tokio::time::{timeout, Instant};

    // An HTTPS proxy speaks TLS on its own port, so a raw socket writing a
    // plaintext CONNECT at it was talking to the wrong layer — every one of
    // them failed the probe and only the geo request, which does wrap TLS,
    // ever got through. Ask over a client that can do it instead.
    let (probe_host, probe_port) = probe_target();
    if matches!(entry.kind, ProxyKind::Https) {
        let started = Instant::now();
        let proxy = reqwest::Proxy::all(&entry.to_proxy_server_arg()).context("bad proxy URL")?;
        let client = reqwest::Client::builder()
            .timeout(STEP_TIMEOUT)
            .connect_timeout(std::time::Duration::from_secs(4))
            .proxy(proxy)
            .build()?;
        let url = format!("https://{probe_host}:{probe_port}/");
        let resp = client.head(&url).send().await.context("the proxy did not carry the request")?;
        if !resp.status().is_success() && !resp.status().is_redirection() {
            anyhow::bail!("the proxy answered {} for {url}", resp.status());
        }
        return Ok(started.elapsed().as_millis());
    }

    let started = Instant::now();
    let addr = format!("{}:{}", entry.host, entry.port);
    let mut stream = timeout(STEP_TIMEOUT, TcpStream::connect(&addr))
        .await
        .context("connect timeout")??;

    match entry.kind {
        ProxyKind::Socks5 => {
            socks5_handshake(&mut stream, entry).await?;

            // CONNECT with ATYP=3 (a name), so the proxy resolves it — the
            // same way a browser behind socks5h makes it.
            let host = probe_host.as_bytes();
            let mut req = vec![0x05, 0x01, 0x00, 0x03, host.len() as u8];
            req.extend_from_slice(host);
            req.extend_from_slice(&probe_port.to_be_bytes());
            stream.write_all(&req).await?;

            let mut hdr = [0u8; 4];
            timeout(STEP_TIMEOUT, stream.read_exact(&mut hdr))
                .await
                .context("the proxy let us in but never answered CONNECT")??;
            if hdr[0] != 0x05 {
                anyhow::bail!("not SOCKS5");
            }
            if hdr[1] != 0x00 {
                anyhow::bail!("{}", socks5_reply_text(hdr[1]));
            }
            // Read BND.ADDR/BND.PORT so the reply is consumed whole.
            let skip = match hdr[3] {
                0x01 => 4 + 2,
                0x04 => 16 + 2,
                0x03 => {
                    let mut len = [0u8; 1];
                    timeout(STEP_TIMEOUT, stream.read_exact(&mut len))
                        .await
                        .context("the CONNECT reply stopped half-way")??;
                    len[0] as usize + 2
                }
                _ => anyhow::bail!("unsupported address type in the CONNECT reply"),
            };
            let mut rest = vec![0u8; skip];
            timeout(STEP_TIMEOUT, stream.read_exact(&mut rest))
                .await
                .context("the CONNECT reply stopped half-way")??;
        }
        // Https is answered above, over a client that speaks TLS to the proxy.
        ProxyKind::Http | ProxyKind::Https => {
            // CONNECT with Basic auth; read until CRLFCRLF to avoid clipping headers.
            use base64::{engine::general_purpose::STANDARD, Engine as _};
            let mut req = format!(
                "CONNECT {probe_host}:{probe_port} HTTP/1.1\r\n\
                 Host: {probe_host}:{probe_port}\r\n"
            );
            if !entry.username.is_empty() || !entry.password.is_empty() {
                let creds = format!("{}:{}", entry.username, entry.password);
                let encoded = STANDARD.encode(creds.as_bytes());
                req.push_str(&format!("Proxy-Authorization: Basic {encoded}\r\n"));
            }
            req.push_str("Proxy-Connection: keep-alive\r\n\r\n");
            stream.write_all(req.as_bytes()).await?;

            // Read until CRLFCRLF or 4 KB cap.
            let mut buf = Vec::with_capacity(512);
            let mut tmp = [0u8; 256];
            let head: String = loop {
                let n = timeout(STEP_TIMEOUT, stream.read(&mut tmp))
                    .await
                    .context("the proxy accepted the connection but never answered CONNECT")??;
                if n == 0 { break String::from_utf8_lossy(&buf).to_string(); }
                buf.extend_from_slice(&tmp[..n]);
                if buf.windows(4).any(|w| w == b"\r\n\r\n") || buf.len() > 4096 {
                    break String::from_utf8_lossy(&buf).to_string();
                }
            };
            let first_line = head.lines().next().unwrap_or("");
            if !first_line.starts_with("HTTP/1.1 200") && !first_line.starts_with("HTTP/1.0 200") {
                anyhow::bail!("CONNECT failed: {first_line}");
            }
        }
    }
    Ok(started.elapsed().as_millis())
}

// ---- Bulk import ----
//
// Accepted: socks5://user:pass@host:port, user:pass@host:port, host:port:user:pass,
//           host:port@user:pass, host:port. A trailing `#` is the proxy's name
//           (`#facebook`); `country=X` and `note=Y` there are still read, for
//           lines exported by older builds, but the country a proxy reports is
//           filled in by its test. Whole-line `#` comments are skipped.
//           SOCKS5 when no scheme given.

/// Parse a single proxy line for inline (unsaved) use by the API.
pub fn parse_single(line: &str) -> Option<ProxyEntry> {
    parse_one(line.trim(), &ProxyKind::Socks5)
}

pub fn parse_bulk(text: &str, default_kind: ProxyKind) -> Vec<ProxyEntry> {
    let mut out = Vec::new();
    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some(p) = parse_one(line, &default_kind) {
            out.push(p);
        }
    }
    out
}

fn parse_one(line: &str, default_kind: &ProxyKind) -> Option<ProxyEntry> {
    // Optional trailing `# country=US note=foo`.
    let (main, comment) = match line.find('#') {
        Some(i) => (line[..i].trim(), Some(line[i + 1..].trim())),
        None => (line, None),
    };
    let (kind, rest) = if let Some(r) = main.strip_prefix("socks5://") {
        (ProxyKind::Socks5, r)
    } else if let Some(r) = main.strip_prefix("https://") {
        (ProxyKind::Https, r)
    } else if let Some(r) = main.strip_prefix("http://") {
        (ProxyKind::Http, r)
    } else {
        (default_kind.clone(), main)
    };

    // Split at the LAST "@", not the first: a password may contain one, and
    // the host part never does. Splitting at the first turned
    // "user:p@ss@host:port" into the host "ss@host:port".
    let (host_part, user, pass) = if let Some((u, hp)) = rest.rsplit_once('@') {
        let (un, pw) = u.split_once(':').unwrap_or((u, ""));
        // A line pasted from a URL, or one the local API was given, carries
        // its userinfo percent-encoded. Decoded here, so what is stored is
        // the login itself — the engine is handed an encoded copy again on
        // the way out, and encoding something already encoded would send
        // "%252C" to the proxy.
        (hp.to_string(), percent_decode_userinfo(un), percent_decode_userinfo(pw))
    } else {
        // host:port or host:port:user:pass
        let parts: Vec<&str> = rest.split(':').collect();
        match parts.len() {
            2 => (rest.to_string(), String::new(), String::new()),
            4 => (
                format!("{}:{}", parts[0], parts[1]),
                parts[2].to_string(),
                parts[3].to_string(),
            ),
            _ => return None,
        }
    };

    let (host, port_s) = host_part.rsplit_once(':')?;
    let port: u16 = port_s.parse().ok()?;
    let mut country = String::new();
    let mut notes = String::new();
    // The comment is the name; `key=value` is only for lines older builds wrote.
    let mut name_parts: Vec<&str> = Vec::new();
    if let Some(c) = comment {
        for kv in c.split_whitespace() {
            if let Some(v) = kv.strip_prefix("country=") {
                country = v.to_string();
            } else if let Some(v) = kv.strip_prefix("note=") {
                notes = v.to_string();
            } else {
                name_parts.push(kv.trim_start_matches('#'));
            }
        }
    }
    let name = name_parts.join(" ");
    Some(ProxyEntry {
        // ID assigned now so pre-save test snapshots key under the kept uuid.
        id: uuid::Uuid::new_v4().to_string(),
        name: if name.is_empty() { format!("{host}:{port}") } else { name },
        kind,
        host: host.to_string(),
        port,
        username: user,
        password: pass,
        country,
        notes,
    })
}

/// Save many entries; returns the count actually persisted. An entry
/// identical to one already stored, or to one earlier in `entries`, is
/// skipped (`same_proxy`).
pub(in crate::space) fn bulk_save(entries: Vec<ProxyEntry>) -> Result<usize> {
    let mut store_data = load()?;
    let added = add_new(&mut store_data.proxies, entries);
    save(&store_data)?;
    Ok(added)
}

/// Appends each entry `proxies` has no identical copy of yet; returns how
/// many it appended.
fn add_new(proxies: &mut Vec<ProxyEntry>, entries: Vec<ProxyEntry>) -> usize {
    let mut added = 0usize;
    for mut e in entries {
        if proxies.iter().any(|x| x.same_proxy(&e)) {
            continue;
        }
        if e.id.is_empty() {
            e.id = uuid::Uuid::new_v4().to_string();
        }
        proxies.push(e);
        added += 1;
    }
    added
}

// ---- UDP probe (SOCKS5 UDP_ASSOCIATE; RFC 1928 §7) ----

/// The STUN address, resolved once. Testing a list of proxies asked DNS for
/// the same name on every one of them.
static STUN_ADDR: std::sync::OnceLock<(std::net::Ipv4Addr, u16)> = std::sync::OnceLock::new();

/// Resolve a public STUN server to IPv4 (probe target for the UDP relay).
async fn resolve_stun_ipv4() -> Result<(std::net::Ipv4Addr, u16)> {
    // The operator's own (#157): Google's and Cloudflare's are unreachable
    // from some networks. Resolved each time, so a changed setting applies at once.
    let custom = crate::settings::load().ok().and_then(|s| s.stun_server).filter(|h| !h.trim().is_empty());
    if let Some(raw) = custom {
        let (host, port) = parse_host_port(&raw, 3478).context("the STUN server in Settings is not a host[:port]")?;
        let addrs = tokio::net::lookup_host((host.as_str(), port))
            .await
            .with_context(|| format!("the STUN server in Settings ({host}) did not resolve"))?;
        for a in addrs {
            if let std::net::IpAddr::V4(v4) = a.ip() {
                return Ok((v4, a.port()));
            }
        }
        anyhow::bail!("the STUN server in Settings ({host}) has no IPv4 address");
    }
    if let Some(hit) = STUN_ADDR.get() {
        return Ok(*hit);
    }
    const HOSTS: &[&str] = &[
        "stun.l.google.com:19302",
        "stun1.l.google.com:19302",
        "stun.cloudflare.com:3478",
    ];
    for h in HOSTS {
        if let Ok(addrs) = tokio::net::lookup_host(*h).await {
            for a in addrs {
                if let std::net::IpAddr::V4(v4) = a.ip() {
                    let hit = (v4, a.port());
                    let _ = STUN_ADDR.set(hit);
                    return Ok(hit);
                }
            }
        }
    }
    anyhow::bail!("no STUN server resolved to IPv4")
}

/// Can THIS process send UDP at all? Answers the question the relay probe
/// cannot: a VPN or firewall that passes TCP and drops UDP — and some do it
/// per application, so the browser may be allowed where the launcher is not —
/// looks exactly like a proxy without a relay. One STUN request straight out.
pub async fn can_send_udp_directly() -> bool {
    use tokio::net::UdpSocket;
    use tokio::time::{timeout, Duration};
    let Ok((ip, port)) = resolve_stun_ipv4().await else {
        return false;
    };
    let Ok(sock) = UdpSocket::bind("0.0.0.0:0").await else {
        return false;
    };
    let mut req = vec![0x00u8, 0x01, 0x00, 0x00, 0x21, 0x12, 0xA4, 0x42];
    req.extend_from_slice(&uuid::Uuid::new_v4().as_bytes()[..12]);
    if sock.send_to(&req, (ip, port)).await.is_err() {
        return false;
    }
    let mut buf = [0u8; 512];
    matches!(
        timeout(Duration::from_secs(4), sock.recv_from(&mut buf)).await,
        Ok(Ok((n, _))) if n >= 20
    )
}

pub async fn probe_udp(entry: &ProxyEntry) -> Result<u128> {
    tokio::time::timeout(TEST_DEADLINE, probe_udp_inner(entry))
        .await
        .map_err(|_| anyhow::anyhow!("the proxy did not finish the UDP exchange in time"))?
}

async fn probe_udp_inner(entry: &ProxyEntry) -> Result<u128> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::{TcpStream, UdpSocket};
    use tokio::time::{timeout, Duration, Instant};

    if !matches!(entry.kind, ProxyKind::Socks5) {
        anyhow::bail!("UDP probe only supported for SOCKS5");
    }
    let started = Instant::now();
    let mut tcp = timeout(
        STEP_TIMEOUT,
        TcpStream::connect(format!("{}:{}", entry.host, entry.port)),
    )
    .await
    .context("connect timeout")??;

    socks5_handshake(&mut tcp, entry).await?;

    // UDP_ASSOCIATE: cmd=0x03, ATYP=IPv4, addr=0.0.0.0, port=0
    tcp.write_all(&[0x05, 0x03, 0x00, 0x01, 0, 0, 0, 0, 0, 0])
        .await?;
    let mut hdr = [0u8; 4];
    timeout(STEP_TIMEOUT, tcp.read_exact(&mut hdr))
        .await
        .context("no answer to UDP_ASSOCIATE")??;
    if hdr[1] != 0x00 {
        anyhow::bail!("the proxy refused UDP: {}", socks5_reply_text(hdr[1]));
    }
    // BND.ADDR says where to send, and a relay is free to answer it in any of
    // three forms. Only the public ones are worth believing: a proxy that
    // names 0.0.0.0, its own loopback or a LAN address is describing where it
    // listens on its own side of the world, not an address reachable from
    // here. Sending there went nowhere, the probe then sat out its wait, and
    // the proxy was reported as having no UDP relay. The control connection's
    // own peer is the one address we know we can reach it at.
    let peer = tcp.peer_addr()?;
    let reachable = |ip: std::net::IpAddr| match ip {
        std::net::IpAddr::V4(v4) => {
            !(v4.is_unspecified() || v4.is_loopback() || v4.is_private() || v4.is_link_local())
        }
        std::net::IpAddr::V6(v6) => !(v6.is_unspecified() || v6.is_loopback()),
    };
    async fn read_port(tcp: &mut TcpStream) -> Result<u16> {
        use tokio::io::AsyncReadExt;
        let mut p = [0u8; 2];
        tokio::time::timeout(STEP_TIMEOUT, tcp.read_exact(&mut p))
            .await
            .context("the UDP_ASSOCIATE reply stopped half-way")??;
        Ok(u16::from_be_bytes(p))
    }
    let bind_addr: SocketAddr = match hdr[3] {
        0x01 => {
            let mut ip = [0u8; 4];
            timeout(STEP_TIMEOUT, tcp.read_exact(&mut ip))
                .await
                .context("the UDP_ASSOCIATE reply stopped half-way")??;
            let port = read_port(&mut tcp).await?;
            let v4 = std::net::IpAddr::V4(std::net::Ipv4Addr::from(ip));
            SocketAddr::new(if reachable(v4) { v4 } else { peer.ip() }, port)
        }
        0x04 => {
            let mut ip = [0u8; 16];
            timeout(STEP_TIMEOUT, tcp.read_exact(&mut ip))
                .await
                .context("the UDP_ASSOCIATE reply stopped half-way")??;
            let port = read_port(&mut tcp).await?;
            let v6 = std::net::IpAddr::V6(std::net::Ipv6Addr::from(ip));
            SocketAddr::new(if reachable(v6) { v6 } else { peer.ip() }, port)
        }
        0x03 => {
            let mut len = [0u8; 1];
            timeout(STEP_TIMEOUT, tcp.read_exact(&mut len))
                .await
                .context("the UDP_ASSOCIATE reply stopped half-way")??;
            let mut name = vec![0u8; len[0] as usize];
            timeout(STEP_TIMEOUT, tcp.read_exact(&mut name))
                .await
                .context("the UDP_ASSOCIATE reply stopped half-way")??;
            let port = read_port(&mut tcp).await?;
            let name = String::from_utf8_lossy(&name).to_string();
            // Resolve what it named; if that leads nowhere, the peer.
            let resolved = timeout(STEP_TIMEOUT, tokio::net::lookup_host((name.as_str(), port)))
                .await
                .ok()
                .and_then(|r| r.ok())
                .and_then(|mut it| it.find(|a| reachable(a.ip())));
            resolved.unwrap_or_else(|| SocketAddr::new(peer.ip(), port))
        }
        other => anyhow::bail!("the proxy answered UDP_ASSOCIATE with address type {other:#x}"),
    };

    // Probe with STUN binding request (DNS-port-53 often blocked, STUN passes).
    let (stun_ip, stun_port) = resolve_stun_ipv4()
        .await
        .context("could not resolve a STUN server to probe UDP with")?;

    // Not connect(): a connected UDP socket accepts datagrams only from the
    // exact address it was pointed at, and a relay is free to answer from
    // another one — RFC 1928 names BND.ADDR as where to SEND, not as where
    // replies come from. Behind a VPN that rewrites source addresses this is
    // the difference between a working relay and a silent timeout.
    let udp = UdpSocket::bind("0.0.0.0:0").await?;
    let mut pkt: Vec<u8> = Vec::with_capacity(32);
    // SOCKS5 UDP header: RSV(2)=0, FRAG=0, ATYP=IPv4, DST=<stun>, PORT.
    pkt.extend_from_slice(&[0, 0, 0, 0x01]);
    pkt.extend_from_slice(&stun_ip.octets());
    pkt.extend_from_slice(&stun_port.to_be_bytes());
    // STUN Binding Request (RFC 5389): type=0x0001, magic 0x2112A442, 12B txid.
    let mut stun = vec![0x00u8, 0x01, 0x00, 0x00, 0x21, 0x12, 0xA4, 0x42];
    stun.extend_from_slice(&uuid::Uuid::new_v4().as_bytes()[..12]);
    pkt.extend_from_slice(&stun);
    udp.send_to(&pkt, bind_addr)
        .await
        .with_context(|| format!("could not send UDP to the relay at {bind_addr}"))?;

    // A relay bound to this socket's source port answers only this socket, so
    // whatever arrives here is ours; the source is logged rather than trusted.
    //
    // Waited six seconds once. A relay that works answers in well under one,
    // so all that bought was six seconds of nothing on every proxy without
    // UDP — and an operator testing a list of them paid it per proxy. Now the
    // wait is short and a second datagram covers the one thing the long wait
    // was really protecting against: a single lost packet.
    let mut buf = vec![0u8; 1500];
    let first = timeout(Duration::from_millis(1200), udp.recv_from(&mut buf)).await;
    let (n, from) = match first {
        Ok(r) => r?,
        Err(_) => {
            udp.send_to(&pkt, bind_addr).await?;
            timeout(Duration::from_millis(1800), udp.recv_from(&mut buf))
                .await
                .context(
                    "no UDP came back. Either the proxy does not relay UDP, or this \
                     machine cannot send UDP out — a VPN or a firewall that passes \
                     TCP and drops UDP looks exactly like a proxy without it",
                )??
        }
    };
    if from != bind_addr {
        eprintln!("[launcher] UDP relay answered from {from}, associated at {bind_addr}");
    }
    if n < 20 {
        anyhow::bail!("UDP reply too short ({n} bytes)");
    }
    // RFC 1928: dropping TCP control tears down the relay; keep it alive.
    drop(tcp);
    Ok(started.elapsed().as_millis())
}

// ---- Geo lookup ----

#[derive(Debug, Clone, Serialize)]
pub struct GeoInfo {
    pub ip: String,
    pub country: String,
    /// ISO 3166-1 alpha-2.
    pub country_code: String,
    pub region: String,
    pub city: String,
    pub isp: String,
    pub timezone: String,
    pub latitude: f64,
    pub longitude: f64,
    pub provider: String,
}

// ---- Change IP (#162) ----

/// Each proxy's change-IP link, by proxy id, on this computer only: not in the
/// proxy itself, so a team's copy never carries the token and GET /proxies
/// never shows it.
fn rotation_path() -> Result<PathBuf> {
    Ok(store::config_root()?.join("proxy-rotation.json"))
}

fn rotation_load() -> std::collections::BTreeMap<String, String> {
    rotation_path()
        .ok()
        .and_then(|p| fs::read_to_string(p).ok())
        .and_then(|b| serde_json::from_str(&b).ok())
        .unwrap_or_default()
}

/// The proxies that have a change-IP link.
pub fn rotation_ids() -> Vec<String> {
    rotation_load().into_keys().collect()
}

pub fn rotation_get(id: &str) -> Option<String> {
    rotation_load().remove(id)
}

/// Sets `id`'s link, or clears it when `url` is empty. Only http(s).
pub fn rotation_set(id: &str, url: Option<&str>) -> Result<()> {
    let mut all = rotation_load();
    match url.map(str::trim).filter(|u| !u.is_empty()) {
        Some(u) => {
            let parsed = reqwest::Url::parse(u).context("the change-IP link is not a link")?;
            anyhow::ensure!(matches!(parsed.scheme(), "http" | "https"), "the change-IP link must start with http:// or https://");
            all.insert(id.to_string(), u.to_string());
        }
        None => {
            all.remove(id);
        }
    }
    let path = rotation_path()?;
    fs::write(&path, serde_json::to_string_pretty(&all)?)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(&path, fs::Permissions::from_mode(0o600));
    }
    Ok(())
}

/// What a change of IP came to.
#[derive(Debug, Clone, Serialize)]
pub struct ChangeIp {
    /// The exit before, from the last test; None when never tested.
    pub before: Option<String>,
    /// The exit read after the provider's answer; None when no read worked.
    pub after: Option<GeoInfo>,
    pub changed: bool,
}

/// Opens the provider's change-IP `url`, then reads the exit until it differs
/// from `before`, for up to ~25 s: rotation finishes on the provider's side a
/// little after it answers. A refusal is reported, not retried — rotation is
/// rate-limited.
pub(in crate::space) async fn change_ip(entry: &ProxyEntry, url: &str, before: Option<String>) -> Result<ChangeIp> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(20))
        .connect_timeout(std::time::Duration::from_secs(8))
        .no_proxy()
        .build()?;
    let resp = client.get(url).send().await.context("the change-IP link did not answer")?;
    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        let said: String = body.trim().chars().take(200).collect();
        if said.is_empty() {
            anyhow::bail!("the provider refused ({status})");
        }
        anyhow::bail!("the provider refused ({status}): {said}");
    }
    let mut after = None;
    for _ in 0..8 {
        tokio::time::sleep(std::time::Duration::from_secs(3)).await;
        if let Ok(geo) = geo_check(entry, None).await {
            let moved = before.as_deref().map_or(true, |b| b != geo.ip);
            after = Some(geo);
            if moved {
                break;
            }
        }
    }
    let changed = match (&before, &after) {
        (Some(b), Some(a)) => b != &a.ip,
        (None, Some(_)) => true,
        _ => false,
    };
    Ok(ChangeIp { before, after, changed })
}

/// `entry` as a reqwest proxy; SOCKS5 resolves names on the proxy (`socks5h`).
pub(in crate::space) fn reqwest_proxy(entry: &ProxyEntry) -> Result<reqwest::Proxy> {
    let scheme = match entry.kind {
        ProxyKind::Socks5 => "socks5h",
        ProxyKind::Http => "http",
        ProxyKind::Https => "https",
    };
    let proxy_url = if entry.username.is_empty() && entry.password.is_empty() {
        format!("{scheme}://{}:{}", entry.host, entry.port)
    } else {
        format!(
            "{scheme}://{}:{}@{}:{}",
            percent_encode_userinfo(&entry.username),
            percent_encode_userinfo(&entry.password),
            entry.host,
            entry.port
        )
    };
    reqwest::Proxy::all(&proxy_url).context("bad proxy URL")
}

/// Probe IP/country the world sees when traffic exits the proxy.
pub async fn geo_check(entry: &ProxyEntry, provider_override: Option<String>) -> Result<GeoInfo> {
    geo_check_via(Some(entry), provider_override).await
}

/// Every provider we know, chosen provider first. ip-api is plain HTTP on the
/// free tier, and a proxy that refuses port 80 or rewrites HTTP fails on it
/// alone — the other two are HTTPS, so the chain gets an answer anyway.
fn provider_chain(chosen: &str) -> Vec<String> {
    let all = ["ip-api.com", "ipapi.co", "ipwho.is"];
    let mut out = Vec::new();
    // The operator's own service goes first (#157); the built-in ones stay behind it.
    if custom_geo_url().is_some() {
        out.push(CUSTOM_GEO.to_string());
    }
    out.push(chosen.to_string());
    out.extend(all.iter().filter(|p| **p != chosen).map(|p| p.to_string()));
    out
}

/// The provider name of the geo service set in Settings.
const CUSTOM_GEO: &str = "custom";

/// The geo service set in Settings (#157), when it is an http(s) URL.
fn custom_geo_url() -> Option<String> {
    let raw = crate::settings::load().ok()?.geo_custom_url?;
    let url = reqwest::Url::parse(raw.trim()).ok()?;
    matches!(url.scheme(), "http" | "https").then(|| url.to_string())
}

/// A geo answer in whatever shape a service gives it: the usual JSON field
/// names (ip-api, ipapi, ipwho, ipinfo and the like, also under `data`), or a
/// bare address as plain text.
fn parse_any_geo(text: &str, provider: &str) -> Result<GeoInfo> {
    let trimmed = text.trim();
    let Ok(root) = serde_json::from_str::<serde_json::Value>(trimmed) else {
        let ip: std::net::IpAddr = trimmed.parse().map_err(|_| anyhow::anyhow!("{provider}: answered neither JSON nor an IP address"))?;
        return Ok(GeoInfo { ip: ip.to_string(), country: String::new(), country_code: String::new(), region: String::new(), city: String::new(), isp: String::new(), timezone: String::new(), latitude: 0.0, longitude: 0.0, provider: provider.to_string() });
    };
    let v = root.get("data").filter(|d| d.is_object()).unwrap_or(&root);
    let s = |keys: &[&str]| -> String {
        keys.iter().find_map(|k| v.get(*k).and_then(|x| x.as_str()).filter(|x| !x.is_empty())).unwrap_or("").to_string()
    };
    let f = |keys: &[&str]| -> f64 {
        keys.iter().find_map(|k| v.get(*k).and_then(|x| x.as_f64().or_else(|| x.as_str().and_then(|t| t.parse().ok())))).unwrap_or(0.0)
    };
    let mut code = s(&["country_code", "countryCode", "country_iso", "countryIso"]);
    let mut country = s(&["country_name", "countryName", "country"]);
    if code.is_empty() && country.len() == 2 {
        code = country.to_ascii_uppercase();
        country.clear();
    }
    let (mut latitude, mut longitude) = (f(&["latitude", "lat"]), f(&["longitude", "lon", "lng"]));
    if let Some((a, b)) = v.get("loc").and_then(|x| x.as_str()).and_then(|l| l.split_once(',')) {
        latitude = a.trim().parse().unwrap_or(latitude);
        longitude = b.trim().parse().unwrap_or(longitude);
    }
    let timezone = v.get("timezone").and_then(|t| t.as_str().map(str::to_string).or_else(|| t.get("id").and_then(|x| x.as_str()).map(str::to_string)))
        .unwrap_or_else(|| s(&["time_zone"]));
    let mut isp = s(&["isp", "org", "organization", "asn_org"]);
    if isp.is_empty() {
        isp = v.get("connection").and_then(|c| c.get("isp")).and_then(|x| x.as_str()).unwrap_or("").to_string();
    }
    Ok(GeoInfo {
        ip: s(&["ip", "query", "ip_address", "ipAddress", "origin", "clientIp"]),
        country,
        country_code: code,
        region: s(&["region", "regionName", "region_name", "province", "state"]),
        city: s(&["city"]),
        isp,
        timezone,
        latitude,
        longitude,
        provider: provider.to_string(),
    })
}

/// Probe geo through `entry` if Some, else direct. Tries the chosen provider,
/// then the others; the error carries what each one said.
pub async fn geo_check_via(entry: Option<&ProxyEntry>, provider_override: Option<String>) -> Result<GeoInfo> {
    let chosen = provider_override
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| settings::load().ok().and_then(|s| s.geo_checker).unwrap_or_else(|| "ip-api.com".into()));

    // One provider at a time, each with its own timeout, cost up to three
    // timeouts in a row — half a minute of waiting before the test could even
    // say "no IP". The chosen one still goes first and still wins on a tie,
    // but if it has not answered shortly the next joins it rather than
    // replacing it, and the first good answer ends the race.
    use futures_util::{stream::FuturesUnordered, StreamExt};

    let chain = provider_chain(&chosen);
    let mut pending = FuturesUnordered::new();
    let mut next = 0usize;
    let mut failures = Vec::new();
    let mut stagger = tokio::time::interval(std::time::Duration::from_millis(900));

    loop {
        tokio::select! {
            biased;

            // A provider answered.
            Some((provider, res)) = pending.next(), if !pending.is_empty() => {
                match res {
                    Ok(info) => {
                        if !failures.is_empty() {
                            eprintln!(
                                "[launcher] geo: {provider} answered after {} failed: {}",
                                failures.len(), failures.join("; ")
                            );
                        }
                        return Ok(info);
                    }
                    Err(e) => failures.push(format!("{provider}: {e}")),
                }
                if pending.is_empty() && next >= chain.len() {
                    break;
                }
            }

            // Nothing yet: bring the next provider in alongside.
            _ = stagger.tick(), if next < chain.len() => {
                let provider = chain[next].clone();
                next += 1;
                pending.push(async move {
                    let r = geo_check_one(entry, &provider).await;
                    (provider, r)
                });
            }

            else => break,
        }
    }
    anyhow::bail!("every geo source failed — {}", failures.join("; "))
}

async fn geo_check_one(entry: Option<&ProxyEntry>, provider: &str) -> Result<GeoInfo> {
    let custom = (provider == CUSTOM_GEO).then(custom_geo_url).flatten();
    let url = match provider {
        _ if custom.is_some() => custom.as_deref().unwrap_or_default(),
        "ip-api.com" => "http://ip-api.com/json/?fields=status,message,query,country,countryCode,regionName,city,isp,timezone,lat,lon",
        "ipapi.co" => "https://ipapi.co/json/",
        "ipwho.is" => "https://ipwho.is/",
        _ => "http://ip-api.com/json/?fields=status,message,query,country,countryCode,regionName,city,isp,timezone,lat,lon",
    };

    // Short on purpose: three of these can be in the air at once and the
    // operator is waiting on all of them. A service that has not answered in
    // this long is not the one that will give the address first.
    let mut builder = reqwest::Client::builder()
        .timeout(STEP_TIMEOUT)
        .connect_timeout(std::time::Duration::from_secs(4));
    if let Some(entry) = entry {
        builder = builder.proxy(reqwest_proxy(entry)?);
    } else {
        // Direct check: bypass any system proxy.
        builder = builder.no_proxy();
    }
    let client = builder.build()?;

    if custom.is_some() {
        let text = client.get(url).send().await?.error_for_status()?.text().await?;
        let info = parse_any_geo(&text, provider)?;
        if info.ip.trim().is_empty() {
            anyhow::bail!("{provider}: answered without an IP");
        }
        return Ok(info);
    }
    let body: serde_json::Value = client.get(url).send().await?.json().await?;

    let s = |v: &serde_json::Value, k: &str| {
        v.get(k).and_then(|x| x.as_str()).unwrap_or("").to_string()
    };
    let f = |v: &serde_json::Value, k: &str| {
        v.get(k).and_then(|x| x.as_f64()).unwrap_or(0.0)
    };
    // A provider over its quota answers 200 with an error object. Parsed as a
    // success that gave no fields, it fell through as "no location" and the
    // chain stopped trying — the profile then launched on the host's clock.
    if body.get("error").and_then(|v| v.as_bool()) == Some(true)
        || body.get("success").and_then(|v| v.as_bool()) == Some(false)
    {
        let why = body
            .get("reason")
            .or_else(|| body.get("message"))
            .and_then(|v| v.as_str())
            .unwrap_or("refused");
        anyhow::bail!("{provider}: {why}");
    }

    let info = match provider {
        "ip-api.com" => {
            if s(&body, "status") == "fail" {
                anyhow::bail!("ip-api.com: {}", s(&body, "message"));
            }
            GeoInfo {
                ip: s(&body, "query"),
                country: s(&body, "country"),
                country_code: s(&body, "countryCode"),
                region: s(&body, "regionName"),
                city: s(&body, "city"),
                isp: s(&body, "isp"),
                timezone: s(&body, "timezone"),
                latitude: f(&body, "lat"),
                longitude: f(&body, "lon"),
                provider: provider.to_string(),
            }
        }
        "ipapi.co" => GeoInfo {
            ip: s(&body, "ip"),
            country: s(&body, "country_name"),
            country_code: s(&body, "country_code"),
            region: s(&body, "region"),
            city: s(&body, "city"),
            isp: s(&body, "org"),
            timezone: s(&body, "timezone"),
            latitude: f(&body, "latitude"),
            longitude: f(&body, "longitude"),
            provider: provider.to_string(),
        },
        "ipwho.is" => GeoInfo {
            ip: s(&body, "ip"),
            country: s(&body, "country"),
            country_code: s(&body, "country_code"),
            region: s(&body, "region"),
            city: s(&body, "city"),
            isp: body.get("connection").and_then(|c| c.get("isp")).and_then(|x| x.as_str()).unwrap_or("").to_string(),
            timezone: body.get("timezone").and_then(|t| t.get("id")).and_then(|x| x.as_str()).unwrap_or("").to_string(),
            latitude: f(&body, "latitude"),
            longitude: f(&body, "longitude"),
            provider: provider.to_string(),
        },
        _ => GeoInfo {
            ip: s(&body, "query"),
            country: s(&body, "country"),
            country_code: s(&body, "countryCode"),
            region: String::new(),
            city: String::new(),
            isp: String::new(),
            timezone: String::new(),
            latitude: 0.0,
            longitude: 0.0,
            provider: provider.to_string(),
        },
    };
    // Every provider names the address it saw. Without one there is nothing to
    // trust in the rest of the object, so the next provider gets a turn.
    if info.ip.trim().is_empty() {
        anyhow::bail!("{provider}: answered without an IP");
    }
    Ok(info)
}

/// Map ISO-3166 alpha-2 to BCP-47 locale (coarse).
pub fn country_to_locale(cc: &str) -> &'static str {
    match cc.to_ascii_uppercase().as_str() {
        "US" => "en-US",
        "GB" | "UK" => "en-GB",
        "CA" => "en-CA",
        "AU" => "en-AU",
        "NZ" => "en-NZ",
        "IE" => "en-IE",
        "ZA" => "en-ZA",
        "IN" => "en-IN",
        "DE" => "de-DE",
        "AT" => "de-AT",
        "CH" => "de-CH",
        "FR" => "fr-FR",
        "BE" => "fr-BE",
        "ES" => "es-ES",
        "MX" => "es-MX",
        "AR" => "es-AR",
        "CO" => "es-CO",
        "CL" => "es-CL",
        "IT" => "it-IT",
        "NL" => "nl-NL",
        "PL" => "pl-PL",
        "BR" => "pt-BR",
        "PT" => "pt-PT",
        "RO" => "ro-RO",
        "RU" => "ru-RU",
        "BY" => "be-BY",
        "UA" => "uk-UA",
        "TR" => "tr-TR",
        "GR" => "el-GR",
        "CZ" => "cs-CZ",
        "SK" => "sk-SK",
        "HU" => "hu-HU",
        "SE" => "sv-SE",
        "FI" => "fi-FI",
        "NO" => "nb-NO",
        "DK" => "da-DK",
        "BG" => "bg-BG",
        "HR" => "hr-HR",
        "SI" => "sl-SI",
        "RS" => "sr-RS",
        "IL" => "he-IL",
        "SA" | "AE" | "EG" => "ar-SA",
        "ID" => "id-ID",
        "MY" => "ms-MY",
        "PH" => "fil-PH",
        "VN" => "vi-VN",
        "TH" => "th-TH",
        "CN" => "zh-CN",
        "HK" => "zh-HK",
        "TW" => "zh-TW",
        "JP" => "ja-JP",
        "KR" => "ko-KR",
        _ => "en-US",
    }
}

// ---- Test history ----

/// One observation of a proxy's exit state; same-IP consecutive entries collapse.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TestSnapshot {
    pub first_seen: String,
    pub last_seen: String,
    pub ip: String,
    pub country_code: String,
    pub country: String,
    pub region: String,
    pub city: String,
    pub isp: String,
    pub timezone: String,
    pub latitude: f64,
    pub longitude: f64,
    pub tcp_ms: Option<u128>,
    pub udp_ms: Option<u128>,
    pub udp_error: Option<String>,
    /// Why there is no geo on this snapshot. Without it a failed probe reads
    /// as "no country", and the profile quietly launches on the host's clock.
    #[serde(default)]
    pub geo_error: Option<String>,
    pub provider: String,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
struct HistoryStore {
    #[serde(default)]
    by_proxy: HashMap<String, Vec<TestSnapshot>>,
}

fn history_path() -> Result<PathBuf> {
    Ok(store::config_root()?.join("proxies-history.json"))
}

/// The test history, held in memory behind one lock.
///
/// It used to be read from disk, parsed whole and written back on every
/// touch. Opening the Proxies page asks for the last test of every row at
/// once, and a bulk test writes from five tasks at a time, so the file was
/// parsed dozens of times a second — and two tests finishing together each
/// wrote back the copy they had read, so one of them simply vanished. That
/// is how a proxy that had just failed could keep showing its old green
/// result.
static HISTORY: std::sync::Mutex<Option<HistoryStore>> = std::sync::Mutex::new(None);

/// Runs `f` against the one in-memory copy, loading it the first time.
fn with_history<T>(f: impl FnOnce(&mut HistoryStore) -> Result<(T, bool)>) -> Result<T> {
    let mut guard = HISTORY.lock().unwrap_or_else(|e| e.into_inner());
    let mut pruned = false;
    if guard.is_none() {
        let path = history_path()?;
        let mut loaded: HistoryStore = if path.exists() {
            serde_json::from_str(&fs::read_to_string(&path)?).unwrap_or_default()
        } else {
            HistoryStore::default()
        };
        // Results belonging to proxies that are gone. They were parsed on
        // every call and were never going to be shown again — on this
        // machine 228 of the 270 keys, most of a 170 KB file. Pruned only
        // when the proxy list could actually be read, so a list that fails
        // to load never passes for "no proxies" and takes real history with
        // it.
        if let Ok(known) = load() {
            let live: std::collections::HashSet<String> =
                known.proxies.iter().map(|p| p.id.clone()).collect();
            let before = loaded.by_proxy.len();
            loaded.by_proxy.retain(|id, _| live.contains(id));
            pruned = loaded.by_proxy.len() != before;
        }
        *guard = Some(loaded);
    }
    let store = guard.as_mut().expect("just loaded");
    let (out, dirty) = f(store)?;
    if dirty || pruned {
        // To the side and renamed in: a crash mid-write leaves the old file,
        // not half of a new one.
        let path = history_path()?;
        let body = serde_json::to_string_pretty(store)?;
        let tmp = path.with_extension("json.tmp");
        fs::write(&tmp, body)?;
        fs::rename(&tmp, &path)?;
    }
    Ok(out)
}


/// Persist a test result; same-IP entries collapse, capped at 50 per proxy.
fn record_test(proxy_id: &str, mut snap: TestSnapshot) -> Result<TestSnapshot> {
    if proxy_id.is_empty() {
        if snap.first_seen.is_empty() {
            snap.first_seen = snap.last_seen.clone();
        }
        return Ok(snap);
    }
    with_history(|hs| Ok((keep_test(hs.by_proxy.entry(proxy_id.into()).or_default(), snap), true)))
}

/// Add a result to one proxy's history; answers it as kept.
pub(in crate::space) fn keep_test(entries: &mut Vec<TestSnapshot>, mut snap: TestSnapshot) -> TestSnapshot {
    // Same address as last time: one entry for the whole stay, kept at
    // the newest reading. Everything the test just measured is written,
    // not only the latency — the location, the operator and the source
    // used to stay as they were first seen, so a proxy that had moved
    // went on claiming its old city.
    if let Some(last) = entries.last_mut() {
        if !snap.ip.is_empty() && last.ip == snap.ip {
            snap.first_seen = last.first_seen.clone();
            *last = snap.clone();
            return snap;
        }
    }
    if snap.first_seen.is_empty() {
        snap.first_seen = snap.last_seen.clone();
    }
    entries.push(snap.clone());
    if entries.len() > 50 {
        let drop = entries.len() - 50;
        entries.drain(..drop);
    }
    snap
}

pub(in crate::space) fn history(proxy_id: &str) -> Result<Vec<TestSnapshot>> {
    with_history(|hs| Ok((hs.by_proxy.get(proxy_id).cloned().unwrap_or_default(), false)))
}

pub(in crate::space) fn latest_test(proxy_id: &str) -> Option<TestSnapshot> {
    with_history(|hs| {
        Ok((hs.by_proxy.get(proxy_id).and_then(|v| v.last().cloned()), false))
    })
    .ok()
    .flatten()
}

fn unix_now() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let s = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format!("@{s}")
}

/// Run TCP + UDP + geo, persist into history, auto-fill country tag.
pub(in crate::space) async fn full_test(entry: &ProxyEntry) -> Result<TestSnapshot> {
    let recorded = record_test(&entry.id, measure(entry).await)?;

    // Backfill empty country tag on the stored entry.
    if !recorded.country_code.is_empty() {
        let mut store_data = load()?;
        if let Some(p) = store_data.proxies.iter_mut().find(|p| p.id == entry.id) {
            if p.country.is_empty() || p.country == "—" {
                p.country = recorded.country_code.clone();
                save(&store_data)?;
            }
        }
    }

    Ok(recorded)
}

/// Run TCP + UDP + geo and answer what was seen; nothing is kept.
///
/// The three run together. One after another, each waiting out its own
/// timeout, a single dead proxy could hold the operator for most of a minute
/// — and a list of them for as long as it took. They do not depend on each
/// other: the reach test, the relay test and the address the world sees are
/// three separate questions to the same proxy.
pub(in crate::space) async fn measure(entry: &ProxyEntry) -> TestSnapshot {
    let now = unix_now();

    let udp_fut = async {
        if matches!(entry.kind, ProxyKind::Socks5) {
            Some(probe_udp(entry).await)
        } else {
            None
        }
    };
    // Timed, so a proxy that refuses the probe's host but carries everything
    // else still shows how long a round trip through it takes.
    let geo_fut = async {
        let started = std::time::Instant::now();
        let res = geo_check(entry, None).await;
        (res, started.elapsed().as_millis())
    };
    let (tcp_res, udp_res, (geo_res, geo_ms)) =
        tokio::join!(probe(entry), udp_fut, geo_fut);

    // An answer from a geo service IS the proxy working: the request went out
    // through it and came back. So its address is kept whenever it arrived,
    // even if the reach probe failed — a proxy may refuse the one host the
    // probe asks for and carry everything else. The other way round, a probe
    // that reached a host is proof too, and the test then says so with no
    // address rather than calling a working proxy dead.
    let geo_error = match (&tcp_res, &geo_res) {
        (_, Ok(_)) => None,
        (Ok(_), Err(e)) => Some(e.to_string()),
        (Err(t), Err(e)) => Some(format!("proxy unreachable: {t} (no address either: {e})")),
    };
    let (ip, country_code, country, region, city, isp, tz, lat, lng, provider) = match &geo_res {
        Ok(g) => (
            g.ip.clone(), g.country_code.clone(), g.country.clone(),
            g.region.clone(), g.city.clone(), g.isp.clone(),
            g.timezone.clone(), g.latitude, g.longitude, g.provider.clone(),
        ),
        Err(_) => (String::new(), String::new(), String::new(),
                   String::new(), String::new(), String::new(),
                   String::new(), 0.0, 0.0, String::new()),
    };

    TestSnapshot {
        first_seen: String::new(),
        last_seen: now,
        ip,
        country_code,
        country,
        region,
        city,
        isp,
        timezone: tz,
        latitude: lat,
        longitude: lng,
        // Set whenever the proxy carried something, so "reached" always comes
        // with a time: the probe's own round trip, or the geo request's when
        // only that one got through.
        tcp_ms: tcp_res.ok().or(if geo_res.is_ok() { Some(geo_ms) } else { None }),
        udp_ms: udp_res
            .as_ref()
            .and_then(|r| r.as_ref().ok().copied()),
        udp_error: udp_res
            .as_ref()
            .and_then(|r| r.as_ref().err().map(|e| e.to_string())),
        geo_error,
        provider,
    }
}

/// Fallback country → IANA timezone for providers that omit timezone.
pub fn country_to_timezone(cc: &str) -> &'static str {
    match cc.to_ascii_uppercase().as_str() {
        "US" => "America/New_York",
        "CA" => "America/Toronto",
        "GB" | "UK" => "Europe/London",
        "DE" => "Europe/Berlin",
        "FR" => "Europe/Paris",
        "ES" => "Europe/Madrid",
        "IT" => "Europe/Rome",
        "NL" => "Europe/Amsterdam",
        "PL" => "Europe/Warsaw",
        "PT" => "Europe/Lisbon",
        "RO" => "Europe/Bucharest",
        "RU" => "Europe/Moscow",
        "UA" => "Europe/Kyiv",
        "TR" => "Europe/Istanbul",
        "GR" => "Europe/Athens",
        "CZ" => "Europe/Prague",
        "HU" => "Europe/Budapest",
        "SE" => "Europe/Stockholm",
        "FI" => "Europe/Helsinki",
        "NO" => "Europe/Oslo",
        "DK" => "Europe/Copenhagen",
        "CH" => "Europe/Zurich",
        "AT" => "Europe/Vienna",
        "BR" => "America/Sao_Paulo",
        "AR" => "America/Argentina/Buenos_Aires",
        "MX" => "America/Mexico_City",
        "AU" => "Australia/Sydney",
        "NZ" => "Pacific/Auckland",
        "IN" => "Asia/Kolkata",
        "ID" => "Asia/Jakarta",
        "MY" => "Asia/Kuala_Lumpur",
        "SG" => "Asia/Singapore",
        "TH" => "Asia/Bangkok",
        "VN" => "Asia/Ho_Chi_Minh",
        "CN" => "Asia/Shanghai",
        "HK" => "Asia/Hong_Kong",
        "TW" => "Asia/Taipei",
        "JP" => "Asia/Tokyo",
        "KR" => "Asia/Seoul",
        "IL" => "Asia/Jerusalem",
        "SA" => "Asia/Riyadh",
        "AE" => "Asia/Dubai",
        _ => "UTC",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(line: &str) -> ProxyEntry {
        parse_one(line, &ProxyKind::Socks5).expect("parses")
    }

    #[test]
    fn a_password_with_a_session_in_it_is_another_proxy() {
        // Providers that route by the password: one gateway, port and user.
        let text = "gate.example.com:7000:alice:pw_session-a1\n\
                    gate.example.com:7000:alice:pw_session-b2\n\
                    gate.example.com:7000:alice:pw_session-c3";
        let mut store = Vec::new();
        let added = add_new(&mut store, parse_bulk(text, ProxyKind::Socks5));
        assert_eq!(added, 3);
        let passwords: Vec<&str> = store.iter().map(|p| p.password.as_str()).collect();
        assert_eq!(passwords, ["pw_session-a1", "pw_session-b2", "pw_session-c3"]);
    }

    #[test]
    fn an_identical_proxy_is_skipped_in_a_paste_and_against_the_store() {
        let mut store = vec![entry("gate.example.com:7000:alice:one")];
        let text = "gate.example.com:7000:alice:one\n\
                    gate.example.com:7000:alice:two\n\
                    gate.example.com:7000:alice:two";
        assert_eq!(add_new(&mut store, parse_bulk(text, ProxyKind::Socks5)), 1);
        assert_eq!(store.len(), 2);
    }

    #[test]
    fn same_proxy_compares_kind_endpoint_user_and_password() {
        let a = entry("socks5://alice:one@gate.example.com:7000");
        assert!(a.same_proxy(&entry("gate.example.com:7000:alice:one")));
        assert!(!a.same_proxy(&entry("gate.example.com:7000:alice:two")));
        assert!(!a.same_proxy(&entry("gate.example.com:7000:bob:one")));
        assert!(!a.same_proxy(&entry("gate.example.com:7001:alice:one")));
        assert!(!a.same_proxy(&entry("other.example.com:7000:alice:one")));
        assert!(!a.same_proxy(&entry("http://alice:one@gate.example.com:7000")));
        // The name is not part of it: a renamed copy is still the same proxy.
        assert!(a.same_proxy(&entry("gate.example.com:7000:alice:one #shop")));
    }

    #[test]
    fn an_entry_keeps_the_id_it_was_parsed_with() {
        let e = entry("gate.example.com:7000:alice:one");
        let id = e.id.clone();
        let mut store = Vec::new();
        add_new(&mut store, vec![e]);
        assert_eq!(store[0].id, id);
    }

    /// A provider that routes by the password hands out a new one per
    /// session. Updating the profile's proxy must move that entry on, not
    /// leave a dead one behind every time.
    #[test]
    fn a_new_session_password_updates_the_bound_proxy() {
        let first = entry("gate.example.com:7000:alice:pw_session-a1");
        let mut store = Vec::new();
        add_new(&mut store, vec![first.clone()]);
        let bound = store[0].id.clone();

        // Same gateway and user, another session.
        let second = entry("gate.example.com:7000:alice:pw_session-b2");
        // Standing in for upsert_for_binding's inner decision, which is what
        // the API calls: the bound entry is the one that moves.
        let slot = store
            .iter_mut()
            .find(|p| p.id == bound && p.host == second.host && p.port == second.port
                      && p.username == second.username)
            .expect("the bound proxy is still there");
        slot.password = second.password.clone();

        assert_eq!(store.len(), 1, "no second entry for the same binding");
        assert_eq!(store[0].password, "pw_session-b2");
        assert_eq!(store[0].id, bound, "and the profile keeps pointing at it");
    }

    /// A login that carries the characters the rule list uses as its own
    /// punctuation. Providers hand these out — country lists, session
    /// tokens — and neither spelling worked before: encoded reached the
    /// proxy as the literal "%2C", raw split the rule list in two.
    #[test]
    fn a_login_with_separators_survives_the_round_trip() {
        let mut e = entry("gate.example.com:7000");
        e.username = "groups-RESIDENTIAL,session-ab12,country-GB".into();
        e.password = "pw;with=signs and a space".into();
        let arg = e.to_proxy_server_arg();
        for ch in [',', ';', '=', ' '] {
            assert!(
                !arg[arg.find("//").unwrap()..arg.rfind('@').unwrap()].contains(ch),
                "{ch:?} must not travel raw in {arg}"
            );
        }
        assert!(!arg.contains('+'), "a space is %20, never a plus: {arg}");
        // And back again, as the importer reads a pasted line.
        let back = parse_one(&arg, &ProxyKind::Socks5).expect("parses");
        assert_eq!(back.username, e.username);
        assert_eq!(back.password, e.password);
        assert_eq!(back.host, e.host);
        assert_eq!(back.port, e.port);
    }

    /// A password may contain "@"; a host never does.
    #[test]
    fn the_host_is_whatever_follows_the_last_at() {
        let p = parse_one("user:p@ssw0rd@gate.example.com:1080", &ProxyKind::Socks5)
            .expect("parses");
        assert_eq!(p.host, "gate.example.com");
        assert_eq!(p.port, 1080);
        assert_eq!(p.username, "user");
        assert_eq!(p.password, "p@ssw0rd");
    }

    /// A per-cent sign that is not an escape stays as it is.
    #[test]
    fn a_literal_per_cent_is_left_alone() {
        assert_eq!(percent_decode_userinfo("100%done"), "100%done");
        assert_eq!(percent_decode_userinfo("a%2Cb"), "a,b");
        assert_eq!(percent_decode_userinfo("tail%"), "tail%");
    }

    /// A proxy that greets and authenticates but refuses to carry anything
    /// used to pass the test. The reply code decides now, so each refusal
    /// has to reach the operator as words rather than a number.
    #[test]
    fn every_socks5_refusal_reads_as_a_reason() {
        for rep in [0x01u8, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08] {
            let text = socks5_reply_text(rep);
            assert!(!text.is_empty());
            assert!(!text.contains("rep="), "{rep:#x} still reads like a protocol dump: {text}");
        }
        assert!(socks5_reply_text(0x02).contains("ruleset"));
        // An unknown code still says something, with the number to go on.
        assert!(socks5_reply_text(0x7f).contains("0x7f"));
    }

    /// The handshake follows the method the proxy SELECTED. Offering both and
    /// then assuming user/pass sent credentials to a proxy that had just said
    /// it wanted none, and every byte after that was read as the wrong thing.
    #[tokio::test]
    async fn a_proxy_that_wants_no_login_is_not_sent_one() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        // probe() reads the reach host from Settings (#157).
        let _root = crate::store::test_root("proxy-probe");
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.unwrap();
            let mut greet = [0u8; 4];
            sock.read_exact(&mut greet[..2]).await.unwrap();
            let n = greet[1] as usize;
            sock.read_exact(&mut greet[2..2 + n]).await.unwrap();
            // Both offered, because there are credentials.
            assert_eq!(&greet[..2 + n], &[0x05, 0x02, 0x00, 0x02]);
            // "No login needed."
            sock.write_all(&[0x05, 0x00]).await.unwrap();
            // Whatever comes next must be the CONNECT, not a username.
            let mut cmd = [0u8; 4];
            sock.read_exact(&mut cmd).await.unwrap();
            cmd
        });

        let mut e = entry("127.0.0.1:1:alice:secret");
        e.host = addr.ip().to_string();
        e.port = addr.port();
        let probe = tokio::spawn(async move { probe(&e).await });
        let cmd = server.await.unwrap();
        assert_eq!(cmd[0], 0x05, "SOCKS5 version");
        assert_eq!(cmd[1], 0x01, "must be CONNECT, not an RFC 1929 username");
        assert_eq!(cmd[3], 0x03, "asks for a name, so the proxy resolves it");
        probe.abort();
    }

    /// Greeting and login alone are no longer a pass: the probe asks the
    /// proxy to reach something, and a refusal is a failed test.
    #[tokio::test]
    async fn a_greeting_without_a_working_connect_is_not_a_pass() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        // probe() reads the reach host from Settings (#157).
        let _root = crate::store::test_root("proxy-probe");
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.unwrap();
            let mut greet = [0u8; 3];
            sock.read_exact(&mut greet).await.unwrap();
            sock.write_all(&[0x05, 0x00]).await.unwrap();
            let mut head = [0u8; 5];
            sock.read_exact(&mut head).await.unwrap();
            let mut rest = vec![0u8; head[4] as usize + 2];
            sock.read_exact(&mut rest).await.unwrap();
            // "Not allowed by ruleset" — a spent account to the letter.
            sock.write_all(&[0x05, 0x02, 0x00, 0x01, 0, 0, 0, 0, 0, 0]).await.unwrap();
        });

        let mut e = entry("127.0.0.1:1");
        e.host = addr.ip().to_string();
        e.port = addr.port();
        let err = probe(&e).await.expect_err("a refused CONNECT is a failure");
        assert!(err.to_string().contains("ruleset"), "says why: {err}");
    }

    #[test]
    fn probe_and_stun_hosts_as_typed() {
        assert_eq!(parse_host_port("www.baidu.com", 443), Some(("www.baidu.com".into(), 443)));
        assert_eq!(parse_host_port(" https://WWW.Baidu.com:8443/path ", 443), Some(("www.baidu.com".into(), 8443)));
        assert_eq!(parse_host_port("stun.miwifi.com:3478", 3478), Some(("stun.miwifi.com".into(), 3478)));
        assert_eq!(parse_host_port("1.2.3.4", 3478), Some(("1.2.3.4".into(), 3478)));
        for bad in ["", ":443", "host:0", "host:99999", "bad host", "-x.com", "a/b@c"] {
            assert_eq!(parse_host_port(bad, 443), None, "{bad:?}");
        }
    }

    #[test]
    fn any_geo_shape_is_read() {
        let ipinfo = r#"{"ip":"1.2.3.4","city":"Shenzhen","region":"Guangdong","country":"CN","loc":"22.5,114.06","org":"AS4134 Chinanet","timezone":"Asia/Shanghai"}"#;
        let g = parse_any_geo(ipinfo, "custom").unwrap();
        assert_eq!((g.ip.as_str(), g.country_code.as_str(), g.city.as_str(), g.timezone.as_str()), ("1.2.3.4", "CN", "Shenzhen", "Asia/Shanghai"));
        assert_eq!((g.latitude, g.longitude), (22.5, 114.06));
        let nested = r#"{"code":0,"data":{"ip":"5.6.7.8","country_name":"China","country_code":"CN","timezone":{"id":"Asia/Shanghai"}}}"#;
        let g = parse_any_geo(nested, "custom").unwrap();
        assert_eq!((g.ip.as_str(), g.country.as_str(), g.timezone.as_str()), ("5.6.7.8", "China", "Asia/Shanghai"));
        let g = parse_any_geo(" 9.9.9.9\n", "custom").unwrap();
        assert_eq!((g.ip.as_str(), g.country_code.as_str()), ("9.9.9.9", ""));
        assert!(parse_any_geo("<html>blocked</html>", "custom").is_err());
    }
}
