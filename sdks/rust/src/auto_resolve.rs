//! Resolve `"auto"` sentinels in a profile config — port of
//! `resolve_auto_fields` in the launcher. Reads live geo (through the bound
//! proxy when present, direct otherwise), falls back to host TZ/locale, then
//! writes concrete timezone / navigator.language / accept_language /
//! languages / icu_locale / geolocation values.

use serde_json::{json, Value};

use crate::geo::{geo_check_via, GeoInfo};
use crate::proxy::ParsedProxy;

fn country_to_locale(cc: &str) -> &'static str {
    match cc.to_ascii_uppercase().as_str() {
        "US" => "en-US", "GB" | "UK" => "en-GB", "CA" => "en-CA", "AU" => "en-AU",
        "NZ" => "en-NZ", "IE" => "en-IE", "ZA" => "en-ZA", "IN" => "en-IN",
        "DE" => "de-DE", "AT" => "de-AT", "CH" => "de-CH",
        "FR" => "fr-FR", "BE" => "fr-BE",
        "ES" => "es-ES", "MX" => "es-MX", "AR" => "es-AR", "CO" => "es-CO", "CL" => "es-CL",
        "IT" => "it-IT", "NL" => "nl-NL", "PL" => "pl-PL",
        "BR" => "pt-BR", "PT" => "pt-PT",
        "RO" => "ro-RO", "RU" => "ru-RU", "BY" => "be-BY", "UA" => "uk-UA",
        "TR" => "tr-TR", "GR" => "el-GR",
        "CZ" => "cs-CZ", "SK" => "sk-SK", "HU" => "hu-HU",
        "SE" => "sv-SE", "FI" => "fi-FI", "NO" => "nb-NO", "DK" => "da-DK",
        "BG" => "bg-BG", "HR" => "hr-HR", "SI" => "sl-SI", "RS" => "sr-RS",
        "IL" => "he-IL",
        "SA" | "AE" | "EG" => "ar-SA",
        "ID" => "id-ID", "MY" => "ms-MY", "PH" => "fil-PH", "VN" => "vi-VN", "TH" => "th-TH",
        "CN" => "zh-CN", "HK" => "zh-HK", "TW" => "zh-TW",
        "JP" => "ja-JP", "KR" => "ko-KR",
        _ => "en-US",
    }
}

fn country_to_timezone(cc: &str) -> &'static str {
    match cc.to_ascii_uppercase().as_str() {
        "US" => "America/New_York", "CA" => "America/Toronto",
        "GB" | "UK" => "Europe/London",
        "DE" => "Europe/Berlin", "FR" => "Europe/Paris", "ES" => "Europe/Madrid",
        "IT" => "Europe/Rome", "NL" => "Europe/Amsterdam", "PL" => "Europe/Warsaw",
        "PT" => "Europe/Lisbon", "RO" => "Europe/Bucharest", "RU" => "Europe/Moscow",
        "UA" => "Europe/Kyiv", "TR" => "Europe/Istanbul", "GR" => "Europe/Athens",
        "CZ" => "Europe/Prague", "HU" => "Europe/Budapest",
        "SE" => "Europe/Stockholm", "FI" => "Europe/Helsinki",
        "NO" => "Europe/Oslo", "DK" => "Europe/Copenhagen",
        "CH" => "Europe/Zurich", "AT" => "Europe/Vienna",
        "BR" => "America/Sao_Paulo", "AR" => "America/Argentina/Buenos_Aires",
        "MX" => "America/Mexico_City",
        "AU" => "Australia/Sydney", "NZ" => "Pacific/Auckland",
        "IN" => "Asia/Kolkata", "ID" => "Asia/Jakarta", "MY" => "Asia/Kuala_Lumpur",
        "SG" => "Asia/Singapore", "TH" => "Asia/Bangkok", "VN" => "Asia/Ho_Chi_Minh",
        "CN" => "Asia/Shanghai", "HK" => "Asia/Hong_Kong", "TW" => "Asia/Taipei",
        "JP" => "Asia/Tokyo", "KR" => "Asia/Seoul",
        "IL" => "Asia/Jerusalem", "SA" => "Asia/Riyadh", "AE" => "Asia/Dubai",
        _ => "UTC",
    }
}

/// True when the config still carries any unresolved `"auto"` sentinel.
pub fn has_auto_fields(cfg: &Value) -> bool {
    if cfg.get("timezone").and_then(|v| v.as_str()) == Some("auto") {
        return true;
    }
    if cfg
        .get("navigator")
        .and_then(|n| n.get("language"))
        .and_then(|v| v.as_str())
        == Some("auto")
    {
        return true;
    }
    if cfg
        .get("geolocation")
        .and_then(|g| g.get("mode"))
        .and_then(|v| v.as_str())
        == Some("auto")
    {
        return true;
    }
    false
}

fn host_timezone() -> Option<String> {
    if let Ok(tz) = std::env::var("TZ") {
        let tz = tz.trim().to_string();
        if tz.contains('/') {
            return Some(tz);
        }
    }
    if let Ok(target) = std::fs::read_link("/etc/localtime") {
        let target = target.to_string_lossy();
        for prefix in ["/usr/share/zoneinfo/", "/var/db/timezone/zoneinfo/"] {
            if let Some(i) = target.find(prefix) {
                return Some(target[i + prefix.len()..].to_string());
            }
        }
    }
    None
}

fn host_locale() -> String {
    for key in ["LANG", "LC_ALL", "LC_MESSAGES"] {
        if let Ok(v) = std::env::var(key) {
            let stripped = v.split('.').next().unwrap_or("").replace('_', "-");
            if stripped.contains('-') {
                return stripped;
            }
        }
    }
    "en-US".to_string()
}

/// Apply the launcher's "auto" resolution. Returns the `GeoInfo` that fed it,
/// or `None` when both proxy + direct probes failed and the host fallback was
/// used (or there was nothing to resolve).
pub async fn resolve_auto_fields(cfg: &mut Value, proxy: Option<&ParsedProxy>) -> Option<GeoInfo> {
    let want_tz = cfg.get("timezone").and_then(|v| v.as_str()) == Some("auto");
    let want_lang = cfg
        .get("navigator")
        .and_then(|n| n.get("language"))
        .and_then(|v| v.as_str())
        == Some("auto");
    let want_geo = cfg
        .get("geolocation")
        .and_then(|g| g.get("mode"))
        .and_then(|v| v.as_str())
        == Some("auto");
    if !want_tz && !want_lang && !want_geo {
        return None;
    }

    let mut geo: Option<GeoInfo> = None;
    if let Some(p) = proxy {
        geo = geo_check_via(Some(p), "ip-api.com").await.ok();
    }
    if geo.is_none() {
        geo = geo_check_via(None, "ip-api.com").await.ok();
    }

    let (resolved_tz, resolved_locale, lat, lng) = match &geo {
        Some(g) => {
            let tz = if g.timezone.is_empty() {
                country_to_timezone(&g.country_code).to_string()
            } else {
                g.timezone.clone()
            };
            let loc = country_to_locale(&g.country_code).to_string();
            let lat = if g.latitude != 0.0 { Some(g.latitude) } else { None };
            let lng = if g.longitude != 0.0 { Some(g.longitude) } else { None };
            (tz, loc, lat, lng)
        }
        None => (
            host_timezone().unwrap_or_else(|| "UTC".to_string()),
            host_locale(),
            None,
            None,
        ),
    };

    if want_tz {
        cfg["timezone"] = json!(resolved_tz);
    }

    if want_lang {
        let base = resolved_locale.split('-').next().unwrap_or("en").to_string();
        let accept = if resolved_locale == "en-US" {
            "en-US,en;q=0.9".to_string()
        } else {
            format!("{resolved_locale},{base};q=0.9,en-US;q=0.8,en;q=0.7")
        };
        let languages: Vec<String> = if resolved_locale == "en-US" {
            vec!["en-US".into(), "en".into()]
        } else {
            vec![resolved_locale.clone(), base, "en-US".into(), "en".into()]
        };
        if !cfg.get("navigator").map(|n| n.is_object()).unwrap_or(false) {
            cfg["navigator"] = json!({});
        }
        let nav = cfg["navigator"].as_object_mut().unwrap();
        nav.insert("language".into(), json!(resolved_locale));
        nav.insert("accept_language".into(), json!(accept));
        nav.insert("languages".into(), json!(languages));
        cfg["icu_locale"] = json!(resolved_locale);
    }

    if want_geo {
        match (lat, lng) {
            (Some(la), Some(lo)) => {
                cfg["geolocation"] = json!({
                    "mode": "manual",
                    "latitude": la,
                    "longitude": lo,
                    "accuracy": 50.0,
                });
            }
            _ => {
                if let Some(obj) = cfg.as_object_mut() {
                    obj.remove("geolocation");
                }
            }
        }
    }

    geo
}
