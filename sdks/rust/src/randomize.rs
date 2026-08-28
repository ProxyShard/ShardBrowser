//! Per-launch randomisation of hardware_concurrency / device_memory /
//! platform_version. Mirrors `randomize_hardware` / `randomize_platform_version`
//! in the launcher.

use rand::seq::SliceRandom;
use serde_json::{json, Value};

use crate::host::{host_logical_cores, host_ram_bucket_gb};

pub const MACOS_PLATFORM_VERSIONS: &[&str] = &[
    "14.6.1", "14.7", "14.7.1", "14.7.2", "15.4", "15.4.1", "15.5", "15.6", "15.6.1", "15.7",
    "26.0", "26.0.1", "26.1",
];

pub const WINDOWS_PLATFORM_VERSIONS: &[&str] = &[
    "10.0.0", "13.0.0", "14.0.0", "14.0.0", "14.0.0", "15.0.0", "15.0.0", "15.0.0", "15.0.0",
    "16.0.0", "16.0.0", "16.0.0", "17.0.0",
];

pub const LINUX_PLATFORM_VERSIONS: &[&str] = &[
    "5.15.0", "6.1.0", "6.5.0", "6.6.0", "6.8.0", "6.10.0", "6.11.0", "6.12.0", "6.14.0",
    "6.15.0", "6.16.0",
];

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

/// Pick a fresh `navigator.platform_version` (+ mirror to client_hints).
pub fn randomize_platform_version(cfg: &mut Value) {
    let plat = platform_of(cfg);
    let pool: &[&str] = match plat.as_str() {
        "macOS" => MACOS_PLATFORM_VERSIONS,
        "Windows" => WINDOWS_PLATFORM_VERSIONS,
        "Linux" => LINUX_PLATFORM_VERSIONS,
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
