// ProxyShard billing/user API client (https://user-api.proxyshard.com).
//
// Every /user/api/ path takes `Authorization: Bearer <API_KEY>`.  The key
// lives in its own `psapi.json` (see store::psapi_path) so the Settings page,
// which round-trips the whole Settings struct, can't accidentally wipe it.
//
// `call()` is a thin generic wrapper: it injects the bearer key + base URL,
// sends JSON, and unwraps the API's `{ success:false, message }` error shape
// into an anyhow error so the UI shows the server's own wording.

use crate::space::proxy::{self, ProxyEntry, ProxyKind};
use crate::store;
use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::fs;

const BASE: &str = "https://user-api.proxyshard.com";

/// The PUBLIC site API, which takes no key at all. Only the trial offer comes
/// from here; everything else in this file is the billing API above.
const SITE_BASE: &str = "https://site-api.proxyshard.com";

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PsConfig {
    #[serde(default)]
    pub api_key: String,
}

pub fn load() -> Result<PsConfig> {
    let path = store::psapi_path()?;
    if !path.exists() {
        return Ok(PsConfig::default());
    }
    let body = fs::read_to_string(&path)?;
    Ok(serde_json::from_str(&body).unwrap_or_default())
}

fn save(c: &PsConfig) -> Result<()> {
    fs::write(store::psapi_path()?, serde_json::to_string_pretty(c)?)?;
    Ok(())
}

pub fn get_key() -> Result<String> {
    Ok(load()?.api_key)
}

pub fn set_key(key: String) -> Result<()> {
    let mut c = load()?;
    c.api_key = key.trim().to_string();
    save(&c)
}

/// Authenticated JSON request against the billing API.
/// `method` is one of GET / POST / PATCH / DELETE.
pub async fn call(
    method: &str,
    path: &str,
    query: &[(String, String)],
    body: Option<Value>,
) -> Result<Value> {
    let key = get_key()?;
    if key.is_empty() {
        return Err(anyhow!("ProxyShard API key not set"));
    }
    let url = format!("{BASE}{path}");
    let cli = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(20))
        .build()?;
    let mut req = match method {
        "GET" => cli.get(&url),
        "POST" => cli.post(&url),
        "PATCH" => cli.patch(&url),
        "DELETE" => cli.delete(&url),
        other => return Err(anyhow!("unsupported method {other}")),
    };
    req = req.bearer_auth(&key);
    if !query.is_empty() {
        req = req.query(query);
    }
    if let Some(b) = body {
        req = req.json(&b);
    } else if matches!(method, "POST" | "PATCH") {
        // Body-less POST/PATCH: send an empty JSON object so reqwest emits a
        // real Content-Length (+ Content-Type). A zero-length body can be sent
        // with no Content-Length at all, which the server (actix) rejects with
        // "no Content-Length specified … invalid Header provided".
        req = req.json(&serde_json::json!({}));
    }

    let resp = req.send().await.context("request to ProxyShard failed")?;
    let status = resp.status();
    let text = resp.text().await.unwrap_or_default();
    let value: Value = serde_json::from_str(&text).unwrap_or(Value::Null);

    if !status.is_success() {
        if status.as_u16() == 401 {
            return Err(anyhow!("Unauthorized — check your API key"));
        }
        let msg = value
            .get("message")
            .and_then(|m| m.as_str())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| if text.is_empty() { status.as_str() } else { &text });
        return Err(anyhow!("{msg}"));
    }
    Ok(value)
}

/// The trial offer, asked of the public site API — the same call the website's
/// own popup makes.
///
/// NOTHING about the offer is decided here. Whether there is one at all, how
/// much traffic it is for and the personal link to claim it all come back from
/// the server, which weighs country, address type and budget before answering.
/// That is the point: a campaign can be changed, paused or ended without
/// shipping a new launcher, and no amount, link or identifier sits in this
/// source.
///
/// Returns the server's object as-is. `show: false` — or any failure, which is
/// what an offline launcher gets — means there is nothing to show; the caller
/// treats both the same way, because an offer nobody can reach is not an offer.
pub async fn trial_offer() -> Result<Value> {
    let cli = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()?;
    // An empty body still has to be a JSON object with a real Content-Length;
    // see the note in call() for what a zero-length POST does to actix.
    let resp = cli
        .post(format!("{SITE_BASE}/api/trial/offer"))
        .json(&serde_json::json!({}))
        .send()
        .await
        .context("request to ProxyShard failed")?;
    if !resp.status().is_success() {
        return Err(anyhow!("trial offer: {}", resp.status()));
    }
    let text = resp.text().await.unwrap_or_default();
    Ok(serde_json::from_str(&text).unwrap_or(Value::Null))
}

/// Fetch the active proxies for a Datacenter/ISP order and persist them into
/// the proxy list of the space `t` was asked in, as `kind` ("socks5" | "http").
/// Returns the number of new proxies actually added (identical ones are skipped).
pub async fn import_order_proxies(t: &crate::space::Ticket, order_id: i64, kind: String) -> Result<usize> {
    let q = vec![("order_id".to_string(), order_id.to_string())];
    let resp = call("GET", "/user/api/proxies/active", &q, None).await?;

    let data = resp
        .get("data")
        .and_then(|d| d.as_array())
        .cloned()
        .unwrap_or_default();
    let tag = resp
        .get("order_tag")
        .and_then(|t| t.as_str())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string());
    let label = tag.unwrap_or_else(|| format!("order {order_id}"));

    let use_http = kind == "http";
    let s = |v: &Value, k: &str| v.get(k).and_then(|x| x.as_str()).unwrap_or("").to_string();
    let port_of = |v: &Value, k: &str| v.get(k).and_then(|x| x.as_u64()).unwrap_or(0) as u16;

    let mut entries = Vec::new();
    for it in data {
        let ip = s(&it, "ip");
        if ip.is_empty() {
            continue;
        }
        let (proxy_kind, port) = if use_http {
            (ProxyKind::Http, port_of(&it, "http_port"))
        } else {
            (ProxyKind::Socks5, port_of(&it, "socks_port"))
        };
        if port == 0 {
            continue;
        }
        entries.push(ProxyEntry {
            id: String::new(),
            name: format!("{label} ({ip})"),
            kind: proxy_kind,
            host: ip,
            port,
            username: s(&it, "username"),
            password: s(&it, "password"),
            country: String::new(),
            notes: format!("ProxyShard order {order_id}"),
        });
    }

    proxy::bulk_save(crate::space::Door("ps_import_order"), t, entries).await
}

/// One proxy line for a project's `proxy.generate` block. Deliberately not via
/// `import_order_proxies`: this must not land in the operator's proxy library.
#[cfg_attr(not(feature = "automation"), allow(dead_code))]
pub async fn pick_proxy(order: Option<&str>, country: Option<&str>) -> anyhow::Result<String> {
    let mut q: Vec<(String, String)> = Vec::new();
    if let Some(o) = order.filter(|o| !o.is_empty()) {
        q.push(("order_id".into(), o.to_string()));
    }
    let resp = call("GET", "/user/api/proxies/active", &q, None).await?;
    let data = resp
        .get("data")
        .and_then(|d| d.as_array())
        .cloned()
        .unwrap_or_default();

    let s = |v: &Value, k: &str| v.get(k).and_then(|x| x.as_str()).unwrap_or("").to_string();
    let candidates: Vec<&Value> = data
        .iter()
        .filter(|it| !s(it, "ip").is_empty())
        .filter(|it| match country {
            Some(cc) if !cc.is_empty() => s(it, "country").eq_ignore_ascii_case(cc),
            _ => true,
        })
        .collect();
    if candidates.is_empty() {
        return Err(anyhow!(match country {
            Some(cc) if !cc.is_empty() => format!("no active proxy in {cc}"),
            _ => "no active proxies on that order".to_string(),
        }));
    }
    // Pick at random: always the first would put a whole fleet on one address.
    let idx = (uuid::Uuid::new_v4().as_bytes()[0] as usize) % candidates.len();
    let it = candidates[idx];
    let port = it
        .get("socks_port")
        .or_else(|| it.get("port"))
        .and_then(|x| x.as_u64())
        .unwrap_or(0);
    let user = s(it, "login");
    let pass = s(it, "password");
    let ip = s(it, "ip");
    Ok(if user.is_empty() {
        format!("{ip}:{port}")
    } else {
        format!("{ip}:{port}:{user}:{pass}")
    })
}

// ---- Residential sessions ----
// Built only by the automation runner, hence the feature gate on each item.

/// Tier to (`plan` for the session login, `proxy_type` for the account endpoints).
/// Crossing the pair silently hands out a session on the wrong plan.
#[cfg(feature = "automation")]
fn resi_tokens(tier: &str) -> Result<(&'static str, &'static str)> {
    match tier.trim().to_lowercase().as_str() {
        "standart" | "standard" => Ok(("limited", "standart")),
        "premium" => Ok(("premium", "premium")),
        "unmetered" | "unlimited" => Ok(("unlimited", "unlimited")),
        other => Err(anyhow!("unknown residential plan \"{other}\"")),
    }
}

/// Relay gateways and their ports, the same ones the generator card offers.
#[cfg(feature = "automation")]
const RESI_RELAYS: [&str; 3] = [
    "relay-eu.proxyshard.com",
    "relay-ru.proxyshard.net",
    "relay-ua.proxyshard.com",
];

/// What a project asked for.
#[cfg(feature = "automation")]
pub struct ResiRequest<'a> {
    pub tier: &'a str,
    pub country: Option<&'a str>,
    pub region: Option<&'a str>,
    pub city: Option<&'a str>,
    /// macos | windows | android | linux | ios. Premium only.
    pub os: Option<&'a str>,
    /// ISP code (carrier) to pin exits to. Premium only; the code comes from
    /// resi_isps() for the chosen country/region/city.
    pub isp: Option<&'a str>,
    /// Sticky pins one exit address for the session; dynamic takes a new one.
    pub sticky: bool,
    /// "static" holds the session until it is dropped; anything else, including
    /// nothing, turns over seconds after idle. Independent of sticky/dynamic.
    pub session_mode: Option<&'a str>,
    pub http: bool,
    /// Empty for the first relay.
    pub relay: Option<&'a str>,
}

#[cfg(feature = "automation")]
/// Lists the ISPs for a country/region/city as `{count, results:[{code,name,...}]}`.
/// Premium only: the provider keeps no ISP directory for the other tiers.
pub async fn resi_isps(tier: &str, country: &str, region: &str, city: &str) -> Result<Value> {
    let (_plan, proxy_type) = resi_tokens(tier)?;
    let q = vec![
        ("proxy_type".to_string(), proxy_type.to_string()),
        ("country_code".to_string(), country.trim().to_lowercase()),
        ("region_code".to_string(), region.trim().to_lowercase()),
        ("city_code".to_string(), city.trim().to_lowercase()),
    ];
    call("GET", "/user/api/proxies/isps", &q, None)
        .await
        .context("asking ProxyShard for the ISP list")
}

/// Builds one residential session and hands back a proxy ready to bind. Fails
/// fast when the plan has no password or no traffic left, not page by page.
#[cfg(feature = "automation")]
pub async fn residential_proxy(req: ResiRequest<'_>) -> Result<ProxyEntry> {
    let (plan, proxy_type) = resi_tokens(req.tier)?;

    // OS and ISP are premium-only: a cheaper plan would come back without them
    // and say nothing, so name the mistake here.
    let os = req.os.map(|s| s.trim().to_lowercase()).filter(|s| !s.is_empty() && s != "unset");
    let isp = req.isp.map(|s| s.trim().to_string()).filter(|s| !s.is_empty() && s != "unset");
    if isp.is_some() && plan != "premium" {
        return Err(anyhow!(
            "an ISP filter can only be pinned on the Premium residential plan"
        ));
    }
    if let Some(os) = os.as_deref() {
        if plan != "premium" {
            return Err(anyhow!(
                "the OS \"{os}\" can only be pinned on the Premium residential plan"
            ));
        }
        const KNOWN: [&str; 5] = ["macos", "windows", "android", "linux", "ios"];
        if !KNOWN.contains(&os) {
            return Err(anyhow!("unknown OS \"{os}\" — one of macos, windows, android, linux, ios"));
        }
    }

    let profile = call(
        "GET",
        "/user/api/proxies/profile",
        &[("proxy_type".into(), proxy_type.to_string())],
        None,
    )
    .await
    .context("asking ProxyShard for the residential profile")?;

    let password = profile
        .get("proxy_password")
        .or_else(|| profile.get("password"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    if password.is_empty() {
        return Err(anyhow!(
            "no residential password on this account for the {proxy_type} plan"
        ));
    }

    // Unmetered is a flat plan and reports no meter, so there is nothing to
    // check; the other two stop here rather than burn a pass on dead sessions.
    if proxy_type != "unlimited" {
        let left = profile.get("data_remain").and_then(|v| v.as_f64()).unwrap_or(0.0);
        if left <= 0.0 {
            return Err(anyhow!(
                "no residential traffic left on the {proxy_type} plan — top it up and run again"
            ));
        }
    }

    let mut login = format!("plan-{plan}");
    if let Some(c) = req.country.map(str::trim).filter(|s| !s.is_empty()) {
        login.push_str(&format!("-country-{}", c.to_lowercase()));
    }
    if let Some(r) = req.region.map(str::trim).filter(|s| !s.is_empty()) {
        login.push_str(&format!("-region-{r}"));
    }
    if let Some(c) = req.city.map(str::trim).filter(|s| !s.is_empty()) {
        login.push_str(&format!("-city-{c}"));
    }
    if let Some(isp) = isp.as_deref() {
        login.push_str(&format!("-isp-{isp}"));
    }
    if req.sticky {
        // One address for the whole session. Random per call, so two profiles
        // in one run never share an exit.
        let sid = uuid::Uuid::new_v4().simple().to_string()[..8].to_string();
        login.push_str(&format!("-sid-{sid}"));
    }
    if let Some(os) = os.as_deref() {
        login.push_str(&format!("-os-{os}"));
    }
    // The token is the five-second mode; static is its absence. Same rule as the
    // generator card, so both paths build the same session.
    let static_mode = req
        .session_mode
        .map(|m| m.trim().eq_ignore_ascii_case("static"))
        .unwrap_or(false);
    if !static_mode {
        login.push_str("-session_mode-2");
    }

    let host = req
        .relay
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(RESI_RELAYS[0])
        .to_string();
    let (kind, port) = if req.http {
        (ProxyKind::Http, 8080u16)
    } else {
        (ProxyKind::Socks5, 1080u16)
    };

    Ok(ProxyEntry {
        id: String::new(),
        name: format!("resi {plan}"),
        kind,
        host,
        port,
        username: login,
        password,
        country: req
            .country
            .map(|c| c.trim().to_uppercase())
            .unwrap_or_default(),
        notes: format!("ProxyShard residential ({plan})"),
    })
}

// ---- Usage history (`ps_usage`) ----
//
// What the user API publishes: residential traffic and successful requests,
// per tier, from `/user/api/proxies/analytics/overall` (a passthrough of the
// provider's own analytics, bytes per bucket). Datacenter and ISP orders have
// no traffic limit and so no usage to report. Failed requests are not
// published, so that series is null and its card says No data.

/// One bucket as the provider sends it: its start (unix s), bytes, requests.
type Row = (i64, u64, u64);

/// `range` → (bucket length in s, bucket count).
fn usage_buckets(range: &str) -> Result<(i64, usize)> {
    Ok(match range {
        "24h" => (3600, 24),
        "7d" => (6 * 3600, 28),
        "30d" => (86400, 30),
        other => return Err(anyhow!("unknown range {other}")),
    })
}

/// Days since 1970-01-01 for a civil date (Howard Hinnant's algorithm).
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

/// A provider timestamp as unix seconds: ISO 8601 with `Z`, an offset or
/// none (then UTC, which is the timezone asked for), or a number of seconds
/// or milliseconds.
pub(crate) fn parse_ts(v: &Value) -> Option<i64> {
    if let Some(n) = v.as_i64() {
        return Some(if n > 100_000_000_000 { n / 1000 } else { n });
    }
    let s = v.as_str()?.trim();
    let b = s.as_bytes();
    if b.len() < 19 || b[4] != b'-' || b[7] != b'-' || (b[10] != b'T' && b[10] != b' ') {
        return None;
    }
    let num = |a: usize, z: usize| s.get(a..z)?.parse::<i64>().ok();
    let secs = days_from_civil(num(0, 4)?, num(5, 7)?, num(8, 10)?) * 86400
        + num(11, 13)? * 3600
        + num(14, 16)? * 60
        + num(17, 19)?;
    // Skip fractional seconds, then read the offset, if any.
    let mut rest = &s[19..];
    if let Some(r) = rest.strip_prefix('.') {
        rest = r.trim_start_matches(|c: char| c.is_ascii_digit());
    }
    let offset = match rest.as_bytes().first() {
        None | Some(b'Z') | Some(b'z') => 0,
        Some(&sign @ (b'+' | b'-')) => {
            let digits: String = rest[1..].chars().filter(|c| c.is_ascii_digit()).collect();
            let h: i64 = digits.get(0..2)?.parse().ok()?;
            let m: i64 = digits.get(2..4).and_then(|x| x.parse().ok()).unwrap_or(0);
            let off = h * 3600 + m * 60;
            if sign == b'+' { off } else { -off }
        }
        _ => return None,
    };
    Some(secs - offset)
}

/// Every bucket the provider has for one tier in `[start, end)`, all pages.
async fn usage_rows(tier: &str, start: i64, end: i64) -> Result<Vec<Row>> {
    const LIMIT: usize = 1000;
    let mut out = Vec::new();
    for page in 0..20 {
        let q = vec![
            ("proxy_type".to_string(), tier.to_string()),
            ("start".to_string(), start.to_string()),
            ("end".to_string(), end.to_string()),
            ("timezone".to_string(), "UTC".to_string()),
            ("include_sub_users".to_string(), "true".to_string()),
            ("limit".to_string(), LIMIT.to_string()),
            ("offset".to_string(), (page * LIMIT).to_string()),
        ];
        let v = call("GET", "/user/api/proxies/analytics/overall", &q, None).await?;
        let results = v.get("results").and_then(|r| r.as_array()).cloned().unwrap_or_default();
        for r in &results {
            let Some(ts) = r.get("timestamp").and_then(parse_ts) else { continue };
            let data = r.get("data").and_then(|x| x.as_u64()).unwrap_or(0);
            let reqs = r.get("requests").and_then(|x| x.as_u64()).unwrap_or(0);
            out.push((ts, data, reqs));
        }
        let more = v.get("next").map(|n| !n.is_null()).unwrap_or(false);
        if !more || results.is_empty() {
            break;
        }
    }
    out.sort_by_key(|r| r.0);
    Ok(out)
}

/// The provider picks its own bucket length (hourly on a short window,
/// daily on a long one). When that is coarser than the buckets wanted, the
/// window is read again a day at a time, which comes back hourly.
async fn usage_rows_fine(tier: &str, start: i64, end: i64, step: i64) -> Result<Vec<Row>> {
    let rows = usage_rows(tier, start, end).await?;
    let coarsest = rows.windows(2).map(|w| w[1].0 - w[0].0).filter(|d| *d > 0).min().unwrap_or(0);
    if coarsest <= step || step >= 86400 {
        return Ok(rows);
    }
    let mut out = Vec::new();
    let mut day = start;
    while day < end {
        let next = (day + 86400).min(end);
        out.extend(usage_rows(tier, day, next).await?);
        day = next;
    }
    out.sort_by_key(|r| r.0);
    Ok(out)
}

/// A tier the account has no active order of: the API answers in plain text.
fn no_such_tier(e: &anyhow::Error) -> bool {
    let m = e.to_string().to_ascii_lowercase();
    m.contains("active order") || m.contains("resident not set")
}

/// One residential tier over the range and the same span before it: bytes
/// and requests per bucket, plus the two totals before. None when the
/// account has no order of that tier.
async fn usage_tier(tier: &str, from: i64, now: i64, step: i64, n: usize) -> Result<Option<(Vec<u64>, u64, Vec<u64>, u64)>> {
    let span = step * n as i64;
    let rows = match usage_rows_fine(tier, from, now, step).await {
        Ok(r) => r,
        Err(e) if no_such_tier(&e) => return Ok(None),
        Err(e) => return Err(e),
    };
    let mut data = vec![0u64; n];
    let mut reqs = vec![0u64; n];
    for (ts, d, r) in rows {
        if ts < from {
            continue;
        }
        let i = ((ts - from) / step) as usize;
        if i < n {
            data[i] += d;
            reqs[i] += r;
        }
    }
    let before = usage_rows(tier, from - span, from).await.unwrap_or_default();
    let prev_data = before.iter().map(|r| r.1).sum();
    let prev_reqs = before.iter().map(|r| r.2).sum();
    Ok(Some((data, prev_data, reqs, prev_reqs)))
}

/// The ProxyShard page's Usage section: `{ from, step, traffic, requests,
/// failed }`, `from`/`step` in ms. Tiers keyed as the launcher
/// names them (`unlimited` is the Unmetered plan).
pub async fn usage(range: &str) -> Result<Value> {
    let (step, n) = usage_buckets(range)?;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    // The last bucket is the one now falls in, so it fills up as it goes.
    let end = (now / step + 1) * step;
    let from = end - step * n as i64;
    const TIERS: [(&str, &str); 3] = [("standart", "standart"), ("premium", "premium"), ("unlimited", "unmetered")];
    let answers = futures_util::future::join_all(
        TIERS.iter().map(|(api, _)| usage_tier(api, from, now, step, n)),
    )
    .await;

    let mut traffic = serde_json::Map::new();
    let mut reqs = vec![0u64; n];
    let mut reqs_before = 0u64;
    let mut any = false;
    for ((_, key), answer) in TIERS.iter().zip(answers) {
        let Some((data, data_before, r, r_before)) = answer? else { continue };
        any = true;
        traffic.insert((*key).into(), serde_json::json!({ "points": data, "previous": data_before }));
        for (sum, x) in reqs.iter_mut().zip(r) {
            *sum += x;
        }
        reqs_before += r_before;
    }
    Ok(serde_json::json!({
        "from": from * 1000,
        "step": step * 1000,
        "traffic": traffic,
        // The user API does not publish failures.
        "requests": if any { serde_json::json!({ "points": reqs, "previous": reqs_before }) } else { Value::Null },
        "failed": Value::Null,
    }))
}

#[cfg(test)]
mod usage_tests {
    use super::*;

    #[test]
    fn provider_timestamps_parse_to_unix_seconds() {
        let at = |s: &str| parse_ts(&Value::String(s.into()));
        assert_eq!(at("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(at("2026-10-21T10:00:00Z"), Some(1_792_576_800));
        assert_eq!(at("2026-10-21T10:00:00.123456Z"), Some(1_792_576_800));
        assert_eq!(at("2026-10-21T13:00:00+03:00"), Some(1_792_576_800));
        assert_eq!(at("2026-10-21 10:00:00"), Some(1_792_576_800));
        assert_eq!(parse_ts(&serde_json::json!(1_792_576_800)), Some(1_792_576_800));
        assert_eq!(parse_ts(&serde_json::json!(1_792_576_800_000i64)), Some(1_792_576_800));
        assert_eq!(at("yesterday"), None);
    }

    #[test]
    fn ranges_have_the_buckets_the_ui_draws() {
        assert_eq!(usage_buckets("24h").unwrap(), (3600, 24));
        assert_eq!(usage_buckets("7d").unwrap(), (21600, 28));
        assert_eq!(usage_buckets("30d").unwrap(), (86400, 30));
        assert!(usage_buckets("1y").is_err());
    }

    #[test]
    fn a_missing_tier_is_told_apart_from_a_failure() {
        assert!(no_such_tier(&anyhow!("User don't have active order with this proxy type")));
        assert!(!no_such_tier(&anyhow!("Upstream error: 502")));
    }
}
