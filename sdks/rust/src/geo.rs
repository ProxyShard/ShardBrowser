//! Live geo lookup for proxies — mirrors `geo_check_via` in the launcher.
//! Supports ip-api.com / ipapi.co / ipwho.is. SOCKS5 routes via `socks5h`
//! (DNS through the proxy); HTTP/HTTPS via the matching scheme.

use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::proxy::{ParsedProxy, ProxyScheme};

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct GeoInfo {
    pub ip: String,
    pub country: String,
    /// ISO-3166 alpha-2.
    pub country_code: String,
    pub region: String,
    pub city: String,
    pub isp: String,
    /// IANA timezone.
    pub timezone: String,
    pub latitude: f64,
    pub longitude: f64,
    pub provider: String,
}

fn url_for(provider: &str) -> &'static str {
    match provider {
        "ipapi.co" => "https://ipapi.co/json/",
        "ipwho.is" => "https://ipwho.is/",
        _ => "http://ip-api.com/json/?fields=status,message,query,country,countryCode,regionName,city,isp,timezone,lat,lon",
    }
}

/// Probe the geo `proxy` exits at, or direct geo when `proxy` is `None`.
pub async fn geo_check_via(proxy: Option<&ParsedProxy>, provider: &str) -> Result<GeoInfo> {
    let provider = if provider.is_empty() { "ip-api.com" } else { provider };
    let url = url_for(provider);

    let mut builder = reqwest::Client::builder().timeout(std::time::Duration::from_secs(8));
    if let Some(p) = proxy {
        let scheme = match p.scheme {
            ProxyScheme::Socks5 => "socks5h", // DNS via proxy
            ProxyScheme::Http => "http",
            ProxyScheme::Https => "https",
        };
        let proxy_url = if p.username.is_empty() && p.password.is_empty() {
            format!("{scheme}://{}:{}", p.host, p.port)
        } else {
            let enc = |s: &str| url::form_urlencoded::byte_serialize(s.as_bytes()).collect::<String>();
            format!(
                "{scheme}://{}:{}@{}:{}",
                enc(&p.username),
                enc(&p.password),
                p.host,
                p.port
            )
        };
        builder = builder.proxy(reqwest::Proxy::all(&proxy_url).context("bad proxy URL")?);
    } else {
        builder = builder.no_proxy();
    }

    let body: Value = builder.build()?.get(url).send().await?.json().await?;

    let s = |k: &str| body.get(k).and_then(|v| v.as_str()).unwrap_or("").to_string();
    let f = |k: &str| body.get(k).and_then(|v| v.as_f64()).unwrap_or(0.0);

    Ok(match provider {
        "ip-api.com" => {
            if s("status") == "fail" {
                return Err(anyhow!("ip-api.com: {}", s("message")));
            }
            GeoInfo {
                ip: s("query"),
                country: s("country"),
                country_code: s("countryCode"),
                region: s("regionName"),
                city: s("city"),
                isp: s("isp"),
                timezone: s("timezone"),
                latitude: f("lat"),
                longitude: f("lon"),
                provider: provider.into(),
            }
        }
        "ipapi.co" => GeoInfo {
            ip: s("ip"),
            country: s("country_name"),
            country_code: s("country_code"),
            region: s("region"),
            city: s("city"),
            isp: s("org"),
            timezone: s("timezone"),
            latitude: f("latitude"),
            longitude: f("longitude"),
            provider: provider.into(),
        },
        "ipwho.is" => GeoInfo {
            ip: s("ip"),
            country: s("country"),
            country_code: s("country_code"),
            region: s("region"),
            city: s("city"),
            isp: body
                .get("connection")
                .and_then(|c| c.get("isp"))
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            timezone: body
                .get("timezone")
                .and_then(|t| t.get("id"))
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            latitude: f("latitude"),
            longitude: f("longitude"),
            provider: provider.into(),
        },
        _ => GeoInfo {
            ip: s("query"),
            country: s("country"),
            country_code: s("countryCode"),
            provider: provider.into(),
            ..Default::default()
        },
    })
}
