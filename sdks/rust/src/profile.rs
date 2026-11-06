//! Profile = a fingerprint JSON + a per-launch working dir. Wraps the
//! bundled fingerprint library and lets callers override fields before
//! launch.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{anyhow, Context, Result};
use serde_json::{Map, Value};

use crate::runtime::Runtime;

#[derive(Clone, Debug)]
pub struct Profile {
    pub id: String,
    pub config: Value,
}

impl Profile {
    /// Wrap a config object. `id` defaults to `config.name` or `"anonymous"`.
    pub fn new(config: Value, id: Option<String>) -> Self {
        let id = id
            .or_else(|| {
                config
                    .get("name")
                    .and_then(|v| v.as_str())
                    .map(String::from)
            })
            .unwrap_or_else(|| "anonymous".to_string());
        Self { config, id }
    }

    pub fn from_file(path: &Path) -> Result<Self> {
        let text = fs::read_to_string(path).with_context(|| format!("read {path:?}"))?;
        let cfg: Value = serde_json::from_str(&text).with_context(|| format!("parse {path:?}"))?;
        let id = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("anonymous")
            .to_string();
        Ok(Self::new(cfg, Some(id)))
    }

    /// Shallow merge: object values merge one level deep, scalars replace.
    pub fn with_override(&self, overrides: Value) -> Profile {
        let mut out = self.config.clone();
        if let (Some(out_obj), Some(ov)) = (out.as_object_mut(), overrides.as_object()) {
            for (k, v) in ov {
                match (out_obj.get_mut(k), v) {
                    (Some(Value::Object(dst)), Value::Object(src)) => {
                        for (sk, sv) in src {
                            dst.insert(sk.clone(), sv.clone());
                        }
                    }
                    _ => {
                        out_obj.insert(k.clone(), v.clone());
                    }
                }
            }
        }
        let id = overrides
            .get("name")
            .and_then(|v| v.as_str())
            .map(String::from)
            .unwrap_or_else(|| self.id.clone());
        Profile::new(out, Some(id))
    }

    /// Enable exactly the named noise vectors (with soft defaults) and disable
    /// the rest. Declarative — re-calling replaces the selection, so a dropped
    /// vector is turned off. Seeds are derived per-profile at launch. Panics on
    /// an unknown vector name.
    ///
    /// Vectors: `canvas`, `webgl`, `audio`, `client_rects`, `sensors`, `fonts`.
    ///
    /// ```ignore
    /// p.set_noise(&["canvas", "audio"]); // only these two on
    /// p.set_noise(&["canvas"]);          // audio now off again
    /// ```
    pub fn set_noise(&mut self, vectors: &[&str]) -> &mut Self {
        const NOISE_VECTORS: [&str; 6] =
            ["canvas", "webgl", "audio", "client_rects", "sensors", "fonts"];
        for &v in vectors {
            assert!(NOISE_VECTORS.contains(&v), "unknown noise vector: {v}");
        }
        let obj = self
            .config
            .as_object_mut()
            .expect("profile config must be a JSON object");
        let noise = obj
            .entry("noise")
            .or_insert_with(|| Value::Object(Map::new()))
            .as_object_mut()
            .expect("noise must be a JSON object");
        for v in NOISE_VECTORS {
            let on = vectors.contains(&v);
            let block = noise
                .entry(v)
                .or_insert_with(|| Value::Object(Map::new()))
                .as_object_mut()
                .expect("noise vector must be a JSON object");
            block.insert("enabled".into(), Value::Bool(on));
            block.entry("seed").or_insert_with(|| Value::from(0));
            if on {
                match v {
                    "webgl" => {
                        block.entry("intensity").or_insert_with(|| Value::from(0.0005));
                    }
                    "client_rects" => {
                        block.entry("max_offset").or_insert_with(|| Value::from(1));
                    }
                    _ => {}
                }
            }
        }
        self
    }

    pub fn platform(&self) -> String {
        self.config
            .get("navigator")
            .and_then(|n| n.get("platform"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string()
    }

    /// Whether the profile claims a device whose primary input is a finger.
    pub fn claims_mobile(&self) -> bool {
        let s = |path: [&str; 2]| {
            self.config
                .get(path[0])
                .and_then(|v| v.get(path[1]))
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_ascii_lowercase()
        };
        if self
            .config
            .get("client_hints")
            .and_then(|c| c.get("mobile"))
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
        {
            return true;
        }
        s(["client_hints", "platform"]).starts_with("android")
            || s(["navigator", "user_agent"]).contains("android")
    }

    /// Whether the profile claims a DESKTOP Linux machine. Android is excluded:
    /// its platform string is Linux too, and a phone does have WebGPU.
    pub fn claims_linux_desktop(&self) -> bool {
        if self.claims_mobile() {
            return false;
        }
        let platform = self
            .config
            .get("navigator")
            .and_then(|n| n.get("platform"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        platform.starts_with("linux") || platform.starts_with("x11")
    }

    pub fn has_webgpu(&self) -> bool {
        self.config
            .get("webgpu")
            .and_then(|w| w.get("limits"))
            .and_then(|l| l.as_object())
            .map(|m: &Map<String, Value>| !m.is_empty())
            .unwrap_or(false)
    }
}

/// The bundled fingerprint library (JSON files under the cache dir).
pub struct FingerprintLibrary {
    runtime: Arc<Runtime>,
}

impl FingerprintLibrary {
    pub fn new(runtime: Arc<Runtime>) -> Self {
        Self { runtime }
    }

    pub fn ids(&self) -> Vec<String> {
        let mut ids: Vec<String> = fs::read_dir(self.runtime.fingerprints_dir())
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|e| {
                let p = e.path();
                if p.extension().and_then(|s| s.to_str()) == Some("json") {
                    p.file_stem().and_then(|s| s.to_str()).map(String::from)
                } else {
                    None
                }
            })
            .collect();
        ids.sort();
        ids
    }

    /// Ids whose `navigator.platform` contains `platform` (case-insensitive).
    pub fn filter(&self, platform: Option<&str>) -> Vec<String> {
        let needle = platform.map(|p| p.to_lowercase());
        self.ids()
            .into_iter()
            .filter(|id| match &needle {
                None => true,
                Some(n) => self
                    .load(id)
                    .map(|p| p.platform().to_lowercase().contains(n))
                    .unwrap_or(false),
            })
            .collect()
    }

    pub fn load(&self, fingerprint_id: &str) -> Result<Profile> {
        let path = self
            .runtime
            .fingerprints_dir()
            .join(format!("{fingerprint_id}.json"));
        if !path.exists() {
            let sample = self.ids().into_iter().take(10).collect::<Vec<_>>().join(", ");
            return Err(anyhow!(
                "Fingerprint '{fingerprint_id}' not found. Available: {sample}…"
            ));
        }
        Profile::from_file(&path)
    }
}

/// Normalise a profile config's spoofed Chrome version to `chromium_version`
/// (e.g. "152.0.7977.65") so it always matches the running engine — bumps
/// `navigator.user_agent` (Chrome/<major>.0.0.0) and the version fields in
/// `client_hints`: brand_version / brand_full_version / chrome_build /
/// chrome_patch (derived from the version) plus, when supplied, grease_brand /
/// grease_version / grease_full_version (GREASE rotates per release, so it can't
/// be derived — it comes from the manifest). Leaves platform_version,
/// architecture, etc. intact. SDK equivalent of the launcher's post-update
/// profile migration.
///
/// `tls` carries the manifest's TLS block. Only the keys it actually contains
/// are overwritten, so a profile's own `shuffle_extensions` (or any field the
/// manifest does not ship) survives, and a profile with no `tls` block is left
/// alone — absent means "use the engine default", not "stale". Without this the
/// profile keeps the previous release's signature_algorithms while the engine
/// advertises the new ones, and the JA4 hash no longer matches the Chrome
/// version the profile claims: 152 added the ML-DSA schemes 0x0904-0x0906, so a
/// 149-era profile is eleven entries short and reads as a mismatch.
///
/// `android_version` is the Chrome for Android build of the same major, passed
/// for phone profiles: Android ships its own build numbers, and a phone
/// reporting a desktop one claims a build no phone has.
pub fn apply_engine_version(
    config: &mut Value,
    chromium_version: &str,
    grease_brand: Option<&str>,
    grease_version: Option<&str>,
    tls: Option<&Value>,
    android_version: Option<&str>,
) {
    let parts: Vec<&str> = chromium_version.split('.').collect();
    if parts.len() != 4 {
        return;
    }
    let major = parts[0];
    let chromium_version = android_version
        .filter(|v| v.split('.').count() == 4 && v.split('.').next() == Some(major))
        .unwrap_or(chromium_version);
    let parts: Vec<&str> = chromium_version.split('.').collect();
    let build = parts[2].parse::<i64>().ok();
    let patch = parts[3].parse::<i64>().ok();

    if let Some(ua) = config
        .pointer("/navigator/user_agent")
        .and_then(|v| v.as_str())
        .map(String::from)
    {
        if let Some(idx) = ua.find("Chrome/") {
            let rest = &ua[idx + 7..];
            let end = rest.find(' ').unwrap_or(rest.len());
            let new_ua = format!("{}Chrome/{}.0.0.0{}", &ua[..idx], major, &rest[end..]);
            if let Some(slot) = config.pointer_mut("/navigator/user_agent") {
                *slot = Value::String(new_ua);
            }
        }
    }
    if let Some(ch) = config.get_mut("client_hints").and_then(|v| v.as_object_mut()) {
        ch.insert("brand_version".into(), serde_json::json!(major));
        ch.insert("brand_full_version".into(), serde_json::json!(chromium_version));
        if let Some(b) = build {
            ch.insert("chrome_build".into(), serde_json::json!(b));
        }
        if let Some(p) = patch {
            ch.insert("chrome_patch".into(), serde_json::json!(p));
        }
        if let Some(gb) = grease_brand {
            ch.insert("grease_brand".into(), serde_json::json!(gb));
        }
        if let Some(gv) = grease_version {
            ch.insert("grease_version".into(), serde_json::json!(gv));
            ch.insert("grease_full_version".into(), serde_json::json!(format!("{gv}.0.0.0")));
        }
    }

    if let (Some(want), Some(cfg_tls)) = (
        tls.and_then(|t| t.as_object()),
        config.get_mut("tls").and_then(|v| v.as_object_mut()),
    ) {
        for (k, v) in want {
            cfg_tls.insert(k.clone(), v.clone());
        }
    }
}

/// The brand Chrome lists in `userAgentData.brands` and `Sec-CH-UA`, Android included.
const CHROME_BRAND: &str = "Google Chrome";

/// Names the brand on a profile that names none, so the brand list is Chrome's and
/// not only "Chromium" and GREASE; a brand the profile names stays. Launch calls it
/// for phone profiles, whose library templates lacked the field. SDK equivalent of
/// the launcher's profile migration.
pub(crate) fn apply_chrome_brand(config: &mut Value) {
    let Some(ch) = config.get_mut("client_hints").and_then(|v| v.as_object_mut()) else {
        return;
    };
    if ch.get("brand").and_then(|v| v.as_str()).is_some_and(|b| !b.is_empty()) {
        return;
    }
    ch.insert("brand".into(), Value::String(CHROME_BRAND.into()));
}

/// Copies into a saved profile what its library template gained after it was frozen:
/// fonts, fonts_local and webgpu.features where the profile has none, and the adapter's
/// subgroup sizes for the same claimed GPU (the template's win). The template is found by
/// `_meta.gpu_preset_id`, else by `name`; a profile with no template is left as it is.
/// SDK equivalent of the launcher's `adopt_value`.
pub(crate) fn adopt_library_fields(config: &mut Value, fingerprints_dir: &Path) {
    let key = [config.pointer("/_meta/gpu_preset_id"), config.get("name")]
        .into_iter()
        .flatten()
        .filter_map(|v| v.as_str())
        .find(|k| !k.is_empty())
        .map(str::to_owned);
    let Some(key) = key.filter(|k| k.chars().all(|c| c.is_ascii_alphanumeric() || "._-".contains(c))) else {
        return;
    };
    let Some(src) = fs::read_to_string(fingerprints_dir.join(format!("{key}.json")))
        .ok()
        .and_then(|t| serde_json::from_str::<Value>(&t).ok())
    else {
        return;
    };
    for k in ["fonts", "fonts_local"] {
        if config.get(k).is_none() {
            if let Some(v) = src.get(k) {
                config[k] = v.clone();
            }
        }
    }
    let same_gpu = ["/webgpu/vendor", "/webgpu/architecture"]
        .iter()
        .all(|k| config.pointer(k).is_some() && config.pointer(k) == src.pointer(k));
    let Some(w) = config.get_mut("webgpu").and_then(|v| v.as_object_mut()) else {
        return;
    };
    if !w.contains_key("features") {
        if let Some(f) = src.pointer("/webgpu/features") {
            w.insert("features".into(), f.clone());
        }
    }
    // The template's subgroup sizes win for the same claimed GPU, over a stored null or a
    // pre-157 capture (D3D12 max was 128 then); a null in the template changes nothing.
    for k in ["subgroupMinSize", "subgroupMaxSize"] {
        let v = src.pointer(&format!("/webgpu/{k}")).filter(|v| v.is_u64());
        if let (true, Some(v)) = (same_gpu, v) {
            w.insert(k.into(), v.clone());
        }
    }
}

/// WebGPU features Chrome 156 reports on Apple M-series that older templates lack.
const APPLE_WEBGPU_ADDED: &[&str] = &["texture-compression-unaligned"];
/// Chrome 157 adds these from M2 on (Dawn: Apple8 with Mac2, or Apple9); M1 has none.
const APPLE_M2_WEBGPU_ADDED: &[&str] = &["atomic-vec2u-min-max"];

/// Adds `APPLE_WEBGPU_ADDED` to an Apple M-series profile's `webgpu.features`, in
/// sorted place; touches nothing else. An absent or empty list means "the host's
/// own" and stays as it is. SDK equivalent of the launcher's profile migration of
/// the same name.
pub(crate) fn apply_apple_webgpu_features(config: &mut Value) {
    let generation: Option<u32> = config
        .pointer("/webgl/renderer")
        .and_then(|v| v.as_str())
        .and_then(|r| r.find("Apple M").map(|i| &r[i + 7..]))
        .and_then(|rest| rest.split(|c: char| !c.is_ascii_digit()).next()?.parse().ok());
    let apple_adapter = config.pointer("/webgpu/vendor").and_then(|v| v.as_str()) == Some("apple");
    let Some(generation) = generation.filter(|_| apple_adapter) else {
        return;
    };
    let Some(list) = config.pointer_mut("/webgpu/features").and_then(|v| v.as_array_mut()) else {
        return;
    };
    if list.is_empty() {
        return;
    }
    let m2: &[&str] = if generation >= 2 { APPLE_M2_WEBGPU_ADDED } else { &[] };
    for &feat in APPLE_WEBGPU_ADDED.iter().chain(m2) {
        if list.iter().any(|v| v.as_str() == Some(feat)) {
            continue;
        }
        let at = list
            .iter()
            .position(|v| v.as_str().is_some_and(|s| s > feat))
            .unwrap_or(list.len());
        list.insert(at, Value::String(feat.to_string()));
    }
}

/// Per-profile state dir (cookies / IndexedDB / cache), preserved across
/// launches. Defaults to `<profiles_root>/<id>/`.
pub fn user_data_dir(runtime: &Runtime, profile_id: &str, base: Option<&Path>) -> Result<PathBuf> {
    let root = base.map(PathBuf::from).unwrap_or_else(|| runtime.profiles_root());
    let d = root.join(profile_id);
    fs::create_dir_all(&d)?;
    Ok(d)
}

#[cfg(test)]
mod android_version_tests {
    use super::apply_engine_version;
    use serde_json::{json, Value};

    fn phone() -> Value {
        json!({ "navigator": { "user_agent": "Mozilla/5.0 (Linux; Android 10; K) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/156.0.0.0 Mobile Safari/537.36" },
                "client_hints": { "mobile": true, "brand_full_version": "156.0.8078.48" } })
    }

    #[test]
    fn a_phone_gets_the_android_build() {
        let mut cfg = phone();
        apply_engine_version(&mut cfg, "157.0.8103.39", None, None, None, Some("157.0.8103.38"));
        let ch = &cfg["client_hints"];
        assert_eq!((ch["brand_version"].as_str(), ch["brand_full_version"].as_str(), ch["chrome_patch"].as_i64()),
                   (Some("157"), Some("157.0.8103.38"), Some(38)));
        assert!(cfg["navigator"]["user_agent"].as_str().unwrap().contains("Chrome/157.0.0.0 Mobile"));
    }

    #[test]
    fn no_android_build_or_another_major_keeps_the_engines() {
        for android in [None, Some("156.0.8078.81"), Some("157.0.8103"), Some("")] {
            let mut cfg = phone();
            apply_engine_version(&mut cfg, "157.0.8103.39", None, None, None, android);
            assert_eq!(cfg["client_hints"]["brand_full_version"], "157.0.8103.39", "{android:?}");
        }
    }
}

#[cfg(test)]
mod apple_webgpu_tests {
    use super::apply_apple_webgpu_features;
    use serde_json::{json, Value};

    const M1: &str = "ANGLE (Apple, ANGLE Metal Renderer: Apple M1, Unspecified Version)";

    fn mac(renderer: &str, vendor: &str, features: Value) -> Value {
        json!({ "webgl": { "renderer": renderer }, "webgpu": { "vendor": vendor, "features": features } })
    }

    #[test]
    fn an_apple_m_list_gains_the_feature_in_sorted_place_once() {
        let mut cfg = mac(M1, "apple", json!(["shader-f16", "texture-compression-etc2", "texture-formats-tier1"]));
        apply_apple_webgpu_features(&mut cfg);
        apply_apple_webgpu_features(&mut cfg);
        assert_eq!(
            cfg["webgpu"]["features"],
            json!(["shader-f16", "texture-compression-etc2", "texture-compression-unaligned", "texture-formats-tier1"])
        );
    }

    #[test]
    fn m2_and_later_also_gain_the_157_feature_and_m1_does_not() {
        let m3 = "ANGLE (Apple, ANGLE Metal Renderer: Apple M3 Max, Unspecified Version)";
        let mut cfg = mac(m3, "apple", json!(["shader-f16", "texture-compression-unaligned"]));
        apply_apple_webgpu_features(&mut cfg);
        assert_eq!(cfg["webgpu"]["features"], json!(["atomic-vec2u-min-max", "shader-f16", "texture-compression-unaligned"]));
        let mut m1 = mac(M1, "apple", json!(["shader-f16", "texture-compression-unaligned"]));
        apply_apple_webgpu_features(&mut m1);
        assert_eq!(m1["webgpu"]["features"], json!(["shader-f16", "texture-compression-unaligned"]));
    }

    #[test]
    fn other_gpus_and_an_empty_list_are_left_alone() {
        for mut cfg in [
            mac("ANGLE (Intel, Intel(R) Iris(TM) Plus Graphics)", "intel", json!(["shader-f16"])),
            mac(M1, "intel", json!(["shader-f16"])),
            mac(M1, "apple", json!([])),
        ] {
            let before = cfg.clone();
            apply_apple_webgpu_features(&mut cfg);
            assert_eq!(cfg, before);
        }
    }
}

#[cfg(test)]
mod adopt_tests {
    use super::adopt_library_fields;
    use serde_json::json;

    fn lib() -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("shardx-lib-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("win-rtx3060.json"), json!({ "name": "win-rtx3060", "fonts_local": ["Arial"], "webgpu": {
            "vendor": "nvidia", "architecture": "ampere", "features": ["shader-f16"], "subgroupMinSize": 32, "subgroupMaxSize": 32 } }).to_string()).unwrap();
        std::fs::write(d.join("android-x.json"), json!({ "name": "android-x", "webgpu": {
            "vendor": "arm", "architecture": "valhall", "features": ["shader-f16"], "subgroupMinSize": null, "subgroupMaxSize": null } }).to_string()).unwrap();
        d
    }

    #[test]
    fn the_same_gpu_gains_the_sizes_and_missing_fields_once() {
        let d = lib();
        let mut cfg = json!({ "name": "win-rtx3060", "webgpu": { "vendor": "nvidia", "architecture": "ampere" } });
        adopt_library_fields(&mut cfg, &d);
        adopt_library_fields(&mut cfg, &d);
        assert_eq!(cfg["webgpu"], json!({ "vendor": "nvidia", "architecture": "ampere", "features": ["shader-f16"], "subgroupMinSize": 32, "subgroupMaxSize": 32 }));
        assert_eq!(cfg["fonts_local"], json!(["Arial"]));
    }

    #[test]
    fn a_pre_157_capture_and_a_stored_null_give_way_to_the_template() {
        let d = lib();
        let mut stale = json!({ "name": "win-rtx3060", "webgpu": { "vendor": "nvidia", "architecture": "ampere", "features": [],
            "subgroupMinSize": 32, "subgroupMaxSize": 128 } });
        adopt_library_fields(&mut stale, &d);
        assert_eq!(stale["webgpu"]["subgroupMaxSize"], json!(32));
        let mut stored = json!({ "name": "win-rtx3060", "webgpu": { "vendor": "nvidia", "architecture": "ampere", "features": [],
            "subgroupMinSize": null, "subgroupMaxSize": null } });
        adopt_library_fields(&mut stored, &d);
        assert_eq!((stored["webgpu"]["subgroupMinSize"].clone(), stored["webgpu"]["subgroupMaxSize"].clone()), (json!(32), json!(32)));
    }

    #[test]
    fn another_gpu_a_null_or_no_template_change_no_sizes() {
        let d = lib();
        let mut other = json!({ "name": "win-rtx3060", "webgpu": { "vendor": "amd", "architecture": "rdna-2", "features": [] } });
        adopt_library_fields(&mut other, &d);
        assert!(other["webgpu"].get("subgroupMinSize").is_none());
        let mut nulls = json!({ "name": "android-x", "webgpu": { "vendor": "arm", "architecture": "valhall", "features": [] } });
        adopt_library_fields(&mut nulls, &d);
        assert!(nulls["webgpu"].get("subgroupMinSize").is_none());
        let mut own = json!({ "name": "my capture", "webgpu": { "vendor": "nvidia", "architecture": "ampere" } });
        adopt_library_fields(&mut own, &d);
        assert_eq!(own["webgpu"], json!({ "vendor": "nvidia", "architecture": "ampere" }));
    }
}

#[cfg(test)]
mod chrome_brand_tests {
    use super::apply_chrome_brand;
    use serde_json::json;

    #[test]
    fn a_profile_without_a_brand_gets_chromes_and_a_named_one_stays() {
        let mut bare = json!({ "client_hints": { "mobile": true, "brand_version": "156" } });
        apply_chrome_brand(&mut bare);
        assert_eq!(bare["client_hints"]["brand"], "Google Chrome");
        let mut empty = json!({ "client_hints": { "brand": "" } });
        apply_chrome_brand(&mut empty);
        assert_eq!(empty["client_hints"]["brand"], "Google Chrome");
        let mut named = json!({ "client_hints": { "brand": "Microsoft Edge" } });
        apply_chrome_brand(&mut named);
        assert_eq!(named["client_hints"]["brand"], "Microsoft Edge");
    }
}
