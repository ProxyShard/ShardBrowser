//! `platform_version`: the OS version a profile reports in Sec-CH-UA-Platform-Version and
//! `userAgentData.platformVersion`. Only values Chrome itself can report (#147).

use serde_json::{Map, Value};

// Chrome writes major.minor.patch, never two parts. Current releases as of 2026-11
// (support.apple.com/100100); Sonoma ends at 14.8.9.
pub(crate) const MACOS_PLATFORM_VERSIONS: &[&str] = &[
    "14.8.8", "14.8.9",
    "15.7.9", "15.8.1", "15.8.2", "15.8.2",
    "26.6.1", "26.6.2", "26.7.1", "26.7.2", "26.7.2", "26.7.2",
    "27.0.1", "27.2.0", "27.2.0", "27.2.0",
];

// Chrome reports the UniversalApiContract: 10 on Win10 2004+, 15 on Win11 22H2/23H2,
// 19 on 24H2/25H2. 13 and 16-18 exist only on Insider builds.
pub(crate) const WINDOWS_PLATFORM_VERSIONS: &[&str] = &[
    "10.0.0", "10.0.0", "10.0.0", "10.0.0", "10.0.0",
    "15.0.0",
    "19.0.0", "19.0.0", "19.0.0", "19.0.0", "19.0.0", "19.0.0", "19.0.0",
];

// Chrome on Linux reports an empty version, and the engine since build 4 says so itself.
// A value is still written so an older engine does not pass on the host's own.
pub(crate) const LINUX_PLATFORM_VERSIONS: &[&str] = &[
    "5.15.0", "6.1.0", "6.5.0",
    "6.6.0", "6.8.0", "6.10.0", "6.11.0", "6.12.0",
    "6.14.0", "6.15.0", "6.16.0",
];

/// What a version that cannot stand becomes on a Mac, and on an M4 claiming 14.x.
const MACOS_SETTLED: &str = "26.7.2";
const MACOS_SETTLED_M4: &str = "15.8.2";

fn platform_of(payload: &Map<String, Value>) -> &str {
    payload
        .get("navigator")
        .and_then(|n| n.get("platform"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
}

/// The first macOS major the claimed Apple chip shipped with: M4 came with 15, M5 with 26.
fn mac_first_major(payload: &Map<String, Value>) -> u32 {
    let generation: Option<u32> = payload
        .get("webgl")
        .and_then(|w| w.get("renderer"))
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

fn write(payload: &mut Map<String, Value>, version: &str) -> bool {
    let mut changed = false;
    for key in ["navigator", "client_hints"] {
        let Some(obj) = payload.get_mut(key).and_then(|v| v.as_object_mut()) else { continue };
        if obj.get("platform_version").and_then(|v| v.as_str()) != Some(version) {
            obj.insert("platform_version".into(), Value::String(version.into()));
            changed = true;
        }
    }
    changed
}

/// Write a random platform_version into navigator + client_hints; unknown platforms left alone.
pub(crate) fn randomize_platform_version(payload: &mut Map<String, Value>) {
    let pool: Vec<&str> = match platform_of(payload) {
        "macOS" => {
            let first = mac_first_major(payload);
            MACOS_PLATFORM_VERSIONS.iter().copied().filter(|v| major_of(v).unwrap_or(0) >= first).collect()
        }
        "Windows" => WINDOWS_PLATFORM_VERSIONS.to_vec(),
        "Linux" => LINUX_PLATFORM_VERSIONS.to_vec(),
        _ => return,
    };
    if pool.is_empty() {
        return;
    }
    let pick = pool[(uuid::Uuid::new_v4().as_bytes()[0] as usize) % pool.len()];
    write(payload, pick);
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
    let parts: Option<Vec<u32>> = version.split('.').map(|p| p.parse().ok()).collect();
    let Some(mut parts) = parts.filter(|p| (1..=3).contains(&p.len()) && p[0] >= 12) else {
        return MACOS_SETTLED.into();
    };
    if parts[0] < first_major {
        return if first_major >= 26 { MACOS_SETTLED } else { MACOS_SETTLED_M4 }.into();
    }
    parts.resize(3, 0);
    format!("{}.{}.{}", parts[0], parts[1], parts[2])
}

/// A Windows or macOS platform_version Chrome could not report, replaced by the nearest one it
/// can, in navigator and client_hints alike. Deterministic, so it is safe on every start.
pub(crate) fn settle_platform_version(payload: &mut Map<String, Value>) -> bool {
    // The engine reads client_hints over navigator.
    let current = ["client_hints", "navigator"]
        .iter()
        .filter_map(|k| payload.get(*k)?.get("platform_version")?.as_str())
        .find(|v| !v.is_empty())
        .unwrap_or("")
        .to_string();
    let settled = match platform_of(payload) {
        "Windows" => settled_windows(&current).to_string(),
        "macOS" => settled_macos(&current, mac_first_major(payload)),
        _ => return false,
    };
    write(payload, &settled)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn profile(platform: &str, version: &str, renderer: &str) -> Map<String, Value> {
        json!({
            "navigator": { "platform": platform, "platform_version": version },
            "client_hints": { "platform_version": version },
            "webgl": { "renderer": renderer },
        })
        .as_object()
        .cloned()
        .unwrap()
    }

    fn pv(p: &Map<String, Value>) -> (String, String) {
        let get = |k: &str| p[k]["platform_version"].as_str().unwrap_or("").to_string();
        (get("navigator"), get("client_hints"))
    }

    #[test]
    fn windows_keeps_what_chrome_reports_and_moves_the_rest() {
        for (from, to) in [
            ("10.0.0", "10.0.0"), ("14.0.0", "14.0.0"), ("15.0.0", "15.0.0"), ("19.0.0", "19.0.0"),
            ("13.0.0", "19.0.0"), ("16.0.0", "19.0.0"), ("17.0.0", "19.0.0"), ("20.0.0", "19.0.0"),
            ("5.0.0", "10.0.0"), ("12.0.0", "10.0.0"), ("", "19.0.0"), ("x", "19.0.0"),
        ] {
            let mut p = profile("Windows", from, "ANGLE (NVIDIA)");
            assert_eq!(settle_platform_version(&mut p), from != to, "{from}");
            assert_eq!(pv(&p), (to.into(), to.into()), "{from}");
        }
    }

    #[test]
    fn mac_gets_three_parts_and_a_version_its_chip_runs() {
        let m1 = "ANGLE (Apple, ANGLE Metal Renderer: Apple M1, Unspecified Version)";
        let m4 = "ANGLE (Apple, ANGLE Metal Renderer: Apple M4 Pro, Unspecified Version)";
        let m5 = "ANGLE (Apple, ANGLE Metal Renderer: Apple M5, Unspecified Version)";
        for (from, gpu, to) in [
            ("15.6.1", m1, "15.6.1"), ("15.4", m1, "15.4.0"), ("26", m1, "26.0.0"),
            ("14.7", m1, "14.7.0"), ("14.7", m4, "15.8.2"), ("15.6.1", m4, "15.6.1"),
            ("15.6.1", m5, "26.7.2"), ("26.1", m5, "26.1.0"), ("10.15.7", m1, "26.7.2"),
            ("", m1, "26.7.2"), ("15.6.1.2", m1, "26.7.2"),
        ] {
            let mut p = profile("macOS", from, gpu);
            settle_platform_version(&mut p);
            assert_eq!(pv(&p), (to.into(), to.into()), "{from} on {gpu}");
            assert!(!settle_platform_version(&mut p), "once: {from}");
        }
    }

    #[test]
    fn client_hints_win_and_navigator_is_brought_along() {
        let mut p = profile("Windows", "15.0.0", "");
        p["navigator"]["platform_version"] = json!("17.0.0");
        assert!(settle_platform_version(&mut p));
        assert_eq!(pv(&p), ("15.0.0".into(), "15.0.0".into()));
    }

    #[test]
    fn linux_and_phones_are_left_alone() {
        let mut linux = profile("Linux", "6.8.0", "");
        let mut phone = profile("Android", "16.0.0", "");
        assert!(!settle_platform_version(&mut linux));
        assert!(!settle_platform_version(&mut phone));
        assert_eq!(pv(&linux).0, "6.8.0");
    }

    #[test]
    fn every_pool_value_is_already_settled() {
        let m5 = "ANGLE (Apple, ANGLE Metal Renderer: Apple M5, Unspecified Version)";
        for v in WINDOWS_PLATFORM_VERSIONS {
            assert!(!settle_platform_version(&mut profile("Windows", v, "")), "{v}");
        }
        for v in MACOS_PLATFORM_VERSIONS {
            assert!(!settle_platform_version(&mut profile("macOS", v, "Apple M1")), "{v}");
        }
        for _ in 0..200 {
            let mut p = profile("macOS", "", m5);
            randomize_platform_version(&mut p);
            assert!(major_of(&pv(&p).0).unwrap() >= 26, "{:?}", pv(&p));
            assert!(!settle_platform_version(&mut p));
        }
    }
}
