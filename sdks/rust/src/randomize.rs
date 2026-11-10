//! Per-launch randomisation of hardware_concurrency / device_memory /
//! platform_version. Mirrors `randomize_hardware` / `randomize_platform_version`
//! in the launcher.

use rand::seq::SliceRandom;
use serde_json::{json, Value};

use crate::host::{host_logical_cores, host_ram_bucket_gb};

/// Chrome writes major.minor.patch, never two parts. Current releases as of 2026-11
/// (support.apple.com/100100); Sonoma ends at 14.8.9.
pub const MACOS_PLATFORM_VERSIONS: &[&str] = &[
    "14.8.8", "14.8.9", "15.7.9", "15.8.1", "15.8.2", "15.8.2", "26.6.1", "26.6.2", "26.7.1",
    "26.7.2", "26.7.2", "26.7.2", "27.0.1", "27.2.0", "27.2.0", "27.2.0",
];

/// Chrome reports the UniversalApiContract: 10 on Win10 2004+, 15 on Win11 22H2/23H2,
/// 19 on 24H2/25H2. 13 and 16-18 exist only on Insider builds.
pub const WINDOWS_PLATFORM_VERSIONS: &[&str] = &[
    "10.0.0", "10.0.0", "10.0.0", "10.0.0", "10.0.0", "15.0.0", "19.0.0", "19.0.0", "19.0.0",
    "19.0.0", "19.0.0", "19.0.0", "19.0.0",
];

/// Chrome on Linux reports an empty version, and the engine since build 4 says so itself.
/// A value is still written so an older engine does not pass on the host's own.
pub const LINUX_PLATFORM_VERSIONS: &[&str] = &[
    "5.15.0", "6.1.0", "6.5.0", "6.6.0", "6.8.0", "6.10.0", "6.11.0", "6.12.0", "6.14.0",
    "6.15.0", "6.16.0",
];

/// What a version that cannot stand becomes on a Mac, and on an M4 claiming 14.x.
const MACOS_SETTLED: &str = "26.7.2";
const MACOS_SETTLED_M4: &str = "15.8.2";

/// Real x86 logical-core counts (SMT + Intel hybrid). Same array as the launcher.
pub const X86_CORES: &[u64] = &[4, 6, 8, 12, 16, 20, 24, 28, 32];

/// Curated `(hardware_concurrency, device_memory_gib)` pairs per Mac profile id.
pub fn mac_hw_configs(profile_id: &str) -> Option<&'static [(u64, u64)]> {
    Some(match profile_id {
        "mac-m1-air13" | "mac-m1-mbp13" | "mac-m1-imac24" => &[(8, 8), (8, 16)],
        "mac-m1-pro-mbp14" | "mac-m1-pro-mbp16" => &[(8, 16), (10, 16), (10, 32)],
        "mac-m1-max-mbp14" | "mac-m1-max-mbp16" => &[(10, 32)],
        "mac-m2-air13" | "mac-m2-air15" | "mac-m2-mbp13" => &[(8, 8), (8, 16)],
        "mac-m2-pro-mbp14" | "mac-m2-pro-mbp16" => &[(10, 16), (12, 16), (12, 32)],
        "mac-m2-max-mbp14" | "mac-m2-max-mbp16" => &[(12, 32)],
        "mac-m3-air13" | "mac-m3-air15" | "mac-m3-mbp14" | "mac-m3-imac24" => &[(8, 8), (8, 16)],
        "mac-m3-pro-mbp14" | "mac-m3-pro-mbp16" => &[(11, 16), (12, 16), (12, 32)],
        "mac-m3-max-mbp14" | "mac-m3-max-mbp16" => &[(14, 32), (16, 32)],
        "mac-m4-air13" | "mac-m4-air15" | "mac-m4-mbp14" | "mac-m4-imac24" => &[(10, 16), (10, 32)],
        "mac-m4-pro-mbp14" | "mac-m4-pro-mbp16" => &[(12, 16), (14, 16), (14, 32)],
        "mac-m4-max-mbp14" | "mac-m4-max-mbp16" => &[(14, 32), (16, 32)],
        "mac-m5-mbp14" => &[(10, 16), (10, 32)],
        _ => return None,
    })
}

fn platform_of(cfg: &Value) -> String {
    cfg.get("navigator")
        .and_then(|n| n.get("platform"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string()
}

fn ensure_obj<'a>(cfg: &'a mut Value, key: &str) -> &'a mut serde_json::Map<String, Value> {
    if !cfg.get(key).map(|v| v.is_object()).unwrap_or(false) {
        cfg[key] = json!({});
    }
    cfg[key].as_object_mut().unwrap()
}

/// The first macOS major the claimed Apple chip shipped with: M4 came with 15, M5 with 26.
fn mac_first_major(cfg: &Value) -> u32 {
    let generation: Option<u32> = cfg
        .pointer("/webgl/renderer")
        .and_then(|v| v.as_str())
        .and_then(|r| r.find("Apple M").map(|i| &r[i + 7..]))
        .and_then(|rest| rest.split(|c: char| !c.is_ascii_digit()).next()?.parse().ok());
    match generation {
        Some(g) if g >= 5 => 26,
        Some(4) => 15,
        _ => 0,
    }
}

fn major_of(version: &str) -> Option<u32> {
    version.split('.').next()?.parse().ok()
}

/// Pick a fresh `navigator.platform_version` (+ mirror to client_hints). A Mac gets only
/// versions its claimed chip can run.
pub fn randomize_platform_version(cfg: &mut Value) {
    let plat = platform_of(cfg);
    let pool: Vec<&str> = match plat.as_str() {
        "macOS" => {
            let first = mac_first_major(cfg);
            MACOS_PLATFORM_VERSIONS.iter().copied().filter(|v| major_of(v).unwrap_or(0) >= first).collect()
        }
        "Windows" => WINDOWS_PLATFORM_VERSIONS.to_vec(),
        "Linux" => LINUX_PLATFORM_VERSIONS.to_vec(),
        _ => return,
    };
    let mut rng = rand::thread_rng();
    let Some(v) = pool.choose(&mut rng) else { return };
    let v = v.to_string();
    ensure_obj(cfg, "navigator").insert("platform_version".into(), json!(v));
    if let Some(ch) = cfg.get_mut("client_hints").and_then(|c| c.as_object_mut()) {
        ch.insert("platform_version".into(), json!(v));
    }
}

fn settled_windows(version: &str) -> &str {
    if matches!(version, "10.0.0" | "14.0.0" | "15.0.0" | "19.0.0") {
        return version;
    }
    // The engine counts an unreadable version as Windows 11, and picks its fonts by that.
    match major_of(version) {
        Some(m) if m < 13 => "10.0.0",
        _ => "19.0.0",
    }
}

fn settled_macos(version: &str, first_major: u32) -> String {
    let parts: Option<Vec<u32>> = version
        .split('.')
        .map(|p| p.bytes().all(|b| b.is_ascii_digit()).then(|| p.parse().ok()).flatten())
        .collect();
    let Some(mut parts) = parts.filter(|p| (1..=3).contains(&p.len()) && p[0] >= 12) else {
        return MACOS_SETTLED.into();
    };
    if parts[0] < first_major {
        return if first_major >= 26 { MACOS_SETTLED } else { MACOS_SETTLED_M4 }.into();
    }
    parts.resize(3, 0);
    format!("{}.{}.{}", parts[0], parts[1], parts[2])
}

/// A Windows or macOS `platform_version` Chrome could not report (Windows 13/16/17, a
/// two-part macOS version, an M4 on 14.x) becomes the nearest one it can, in navigator and
/// client_hints alike. Deterministic, so launch applies it every time; Linux is left to the
/// engine. Whether anything changed. SDK equivalent of the launcher's function of the same name.
pub fn settle_platform_version(cfg: &mut Value) -> bool {
    // The engine reads client_hints over navigator.
    let current = ["/client_hints/platform_version", "/navigator/platform_version"]
        .iter()
        .filter_map(|p| cfg.pointer(p)?.as_str())
        .find(|v| !v.is_empty())
        .unwrap_or("")
        .to_string();
    let want = match platform_of(cfg).as_str() {
        "Windows" => settled_windows(&current).to_string(),
        "macOS" => settled_macos(&current, mac_first_major(cfg)),
        _ => return false,
    };
    let mut changed = false;
    for key in ["navigator", "client_hints"] {
        let Some(obj) = cfg.get_mut(key).and_then(|v| v.as_object_mut()) else { continue };
        if obj.get("platform_version").and_then(|v| v.as_str()) != Some(want.as_str()) {
            obj.insert("platform_version".into(), json!(want));
            changed = true;
        }
    }
    changed
}

/// Pick fresh `(hardware_concurrency, device_memory)`.
///
/// macOS uses the curated [`mac_hw_configs`] table by profile id; Windows /
/// Linux bracket the host CPU count within `[C-4, C+2]` from [`X86_CORES`],
/// floor device_memory by core count (>=12 → 16, else 8), cap by
/// [`host_ram_bucket_gb`].
pub fn randomize_hardware(cfg: &mut Value, profile_id: Option<&str>) {
    let plat = platform_of(cfg);
    let mut rng = rand::thread_rng();

    let (cores, mem): (u64, u64) = if plat == "macOS" {
        match profile_id.and_then(mac_hw_configs) {
            Some(pairs) => *pairs.choose(&mut rng).unwrap(),
            None => return,
        }
    } else if plat == "Windows" || plat == "Linux" {
        let c = host_logical_cores() as i64;
        let lo = (c - 4).max(0);
        let hi = c + 2;
        let candidates: Vec<u64> = X86_CORES
            .iter()
            .copied()
            .filter(|&n| (n as i64) >= lo && (n as i64) <= hi)
            .collect();
        let cores = if let Some(&v) = candidates.choose(&mut rng) {
            v
        } else {
            *X86_CORES
                .iter()
                .min_by_key(|&&n| (n as i64 - c).abs())
                .unwrap()
        };
        let real = host_ram_bucket_gb();
        let floor = if cores >= 12 { 16 } else { 8 };
        let mem_cand: Vec<u64> = [8u64, 16, 32]
            .into_iter()
            .filter(|&m| m >= floor && m <= real)
            .collect();
        let mem = mem_cand.choose(&mut rng).copied().unwrap_or(real);
        (cores, mem)
    } else {
        return;
    };

    let nav = ensure_obj(cfg, "navigator");
    nav.insert("hardware_concurrency".into(), json!(cores));
    nav.insert("device_memory".into(), json!(mem));
}

#[cfg(test)]
mod platform_version_tests {
    use super::*;

    const M1: &str = "ANGLE (Apple, ANGLE Metal Renderer: Apple M1, Unspecified Version)";
    const M4: &str = "ANGLE (Apple, ANGLE Metal Renderer: Apple M4 Pro, Unspecified Version)";
    const M5: &str = "ANGLE (Apple, ANGLE Metal Renderer: Apple M5, Unspecified Version)";

    fn profile(platform: &str, version: &str, renderer: &str) -> Value {
        json!({ "navigator": { "platform": platform, "platform_version": version },
                "client_hints": { "platform_version": version },
                "webgl": { "renderer": renderer } })
    }

    fn pv(c: &Value) -> (String, String) {
        let get = |p: &str| c.pointer(p).and_then(|v| v.as_str()).unwrap_or("").to_string();
        (get("/navigator/platform_version"), get("/client_hints/platform_version"))
    }

    #[test]
    fn windows_keeps_what_chrome_reports_and_moves_the_rest() {
        for (from, to) in [
            ("10.0.0", "10.0.0"), ("14.0.0", "14.0.0"), ("15.0.0", "15.0.0"), ("19.0.0", "19.0.0"),
            ("13.0.0", "19.0.0"), ("16.0.0", "19.0.0"), ("17.0.0", "19.0.0"), ("20.0.0", "19.0.0"),
            ("5.0.0", "10.0.0"), ("12.0.0", "10.0.0"), ("", "19.0.0"), ("x", "19.0.0"),
        ] {
            let mut c = profile("Windows", from, "");
            assert_eq!(settle_platform_version(&mut c), from != to, "{from}");
            assert_eq!(pv(&c), (to.into(), to.into()), "{from}");
        }
    }

    #[test]
    fn a_mac_gets_three_parts_and_a_version_its_chip_runs() {
        for (from, gpu, to) in [
            ("15.6.1", M1, "15.6.1"), ("15.4", M1, "15.4.0"), ("26", M1, "26.0.0"),
            ("14.7", M1, "14.7.0"), ("14.7", M4, "15.8.2"), ("15.6.1", M4, "15.6.1"),
            ("15.6.1", M5, "26.7.2"), ("26.1", M5, "26.1.0"), ("10.15.7", M1, "26.7.2"),
            ("", M1, "26.7.2"), ("15.6.1.2", M1, "26.7.2"), ("15.+4", M1, "26.7.2"),
        ] {
            let mut c = profile("macOS", from, gpu);
            settle_platform_version(&mut c);
            assert_eq!(pv(&c), (to.into(), to.into()), "{from} on {gpu}");
            assert!(!settle_platform_version(&mut c), "once: {from}");
        }
    }

    #[test]
    fn client_hints_win_and_navigator_is_brought_along() {
        let mut c = profile("Windows", "15.0.0", "");
        c["navigator"]["platform_version"] = json!("17.0.0");
        assert!(settle_platform_version(&mut c));
        assert_eq!(pv(&c), ("15.0.0".into(), "15.0.0".into()));
    }

    #[test]
    fn linux_and_phones_are_left_alone() {
        assert!(!settle_platform_version(&mut profile("Linux", "6.8.0", "")));
        assert!(!settle_platform_version(&mut profile("Android", "16.0.0", "")));
    }

    #[test]
    fn every_pool_value_is_already_settled_and_an_m5_draws_26_or_later() {
        for v in WINDOWS_PLATFORM_VERSIONS {
            assert!(!settle_platform_version(&mut profile("Windows", v, "")), "{v}");
        }
        for v in MACOS_PLATFORM_VERSIONS {
            assert!(!settle_platform_version(&mut profile("macOS", v, M1)), "{v}");
        }
        for _ in 0..200 {
            let mut c = profile("macOS", "", M5);
            randomize_platform_version(&mut c);
            assert!(major_of(&pv(&c).0).unwrap() >= 26, "{:?}", pv(&c));
            assert!(!settle_platform_version(&mut c));
        }
    }
}
