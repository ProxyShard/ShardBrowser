// ProxyShard billing/user API client (https://user-api.proxyshard.com).
//
// Every /user/api/ path takes `Authorization: Bearer <API_KEY>`.  The key
// lives in its own `psapi.json` (see store::psapi_path) so the Settings page,
// which round-trips the whole Settings struct, can't accidentally wipe it.
//
// `call()` is a thin generic wrapper: it injects the bearer key + base URL,
// sends JSON, and unwraps the API's `{ success:false, message }` error shape
// into an anyhow error so the UI shows the server's own wording.

use crate::proxy::{self, ProxyEntry, ProxyKind};
use crate::store;
use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::fs;

const BASE: &str = "https://user-api.proxyshard.com";

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

/// Fetch the active proxies for a Datacenter/ISP order and persist them into
/// the local proxy list as `kind` ("socks5" | "http"). Returns the number of
/// new proxies actually added (existing host:port:user pairs are skipped).
pub async fn import_order_proxies(order_id: i64, kind: String) -> Result<usize> {
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
            name: format!("{label} · {ip}"),
            kind: proxy_kind,
            host: ip,
            port,
            username: s(&it, "username"),
            password: s(&it, "password"),
            country: String::new(),
            notes: format!("ProxyShard order {order_id}"),
        });
    }

    proxy::bulk_save(entries)
}
