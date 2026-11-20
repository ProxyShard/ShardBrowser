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
        config_claims_mobile(&self.config)
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
        let fingerprint_id = retired_successor(fingerprint_id).unwrap_or(fingerprint_id);
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
/// `android_version` is the build this profile's OS is shipped, when it is not
/// `chromium_version`: the Chrome for Android build of the same major for a
/// phone (a phone reporting a desktop build claims one no phone has), or the
/// manifest's `desktop_chromium_versions` entry for a desktop profile (158
/// went out as .37 for macOS and Windows and .36 for Linux).
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

/// Whether a config claims a phone: the engine's three signals, in its order.
pub(crate) fn config_claims_mobile(config: &Value) -> bool {
    let s = |path: [&str; 2]| {
        config
            .get(path[0])
            .and_then(|v| v.get(path[1]))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_ascii_lowercase()
    };
    if config.pointer("/client_hints/mobile").and_then(|v| v.as_bool()).unwrap_or(false) {
        return true;
    }
    s(["client_hints", "platform"]).starts_with("android") || s(["navigator", "user_agent"]).contains("android")
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
    let successor = key.as_deref().and_then(retired_successor);
    let src = successor
        .map(str::to_owned)
        .or(key)
        .filter(|k| k.chars().all(|c| c.is_ascii_alphanumeric() || "._-".contains(c)))
        .and_then(|k| fs::read_to_string(fingerprints_dir.join(format!("{k}.json"))).ok())
        .and_then(|t| serde_json::from_str::<Value>(&t).ok());
    // A phone's fixes need no template; the version takes the template's when there is one.
    adopt_phone_angle_webgl(config);
    adopt_phone_platform_version(config, src.as_ref(), fingerprints_dir);
    adopt_desktop_min_cores(config);
    let Some(src) = src else { return };
    for k in ["fonts", "fonts_local"] {
        if config.get(k).is_none() {
            if let Some(v) = src.get(k) {
                config[k] = v.clone();
            }
        }
    }
    adopt_laptop_battery(config, &src);
    adopt_air15_screen(config, &src);
    adopt_linux_gl_webgl(config, &src, successor);
    adopt_webgl_extensions(config, &src);
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

/// Plugged in and full: what the library pinned on every desktop template until 18.11, and the
/// one state a machine without a battery reports.
fn is_desktop_battery(b: Option<&Value>) -> bool {
    let Some(b) = b.and_then(|v| v.as_object()) else { return false };
    let num = |k: &str| b.get(k).and_then(|v| v.as_f64());
    b.get("charging").and_then(|v| v.as_bool()) == Some(true)
        && num("level") == Some(1.0)
        && num("charging_time") == Some(0.0)
}

/// A laptop profile still carrying that battery takes the template's `{"laptop": true}`; the
/// engine then picks its own state from the profile's seed.
fn adopt_laptop_battery(config: &mut Value, src: &Value) {
    if src.pointer("/battery/laptop").and_then(|v| v.as_bool()) == Some(true) && is_desktop_battery(config.get("battery")) {
        config["battery"] = serde_json::json!({ "laptop": true });
    }
}

/// MacBook Air 15" templates claimed 1680x1050; they now claim 1710x1112. A profile with the old
/// size moves, its window with it.
fn adopt_air15_screen(config: &mut Value, src: &Value) {
    let size = |v: &Value| (v.pointer("/screen/width").and_then(|x| x.as_i64()), v.pointer("/screen/height").and_then(|x| x.as_i64()));
    if size(config) != (Some(1680), Some(1050)) || size(src) != (Some(1710), Some(1112)) {
        return;
    }
    let get = |p: &str| config.pointer(p).and_then(|x| x.as_i64());
    let (Some(avail_h), Some(outer_h), Some(inner_h)) =
        (get("/screen/avail_height"), get("/window/outer_height"), get("/window/inner_height"))
    else {
        return;
    };
    let new_avail = 1112 - (1050 - avail_h);
    let new_outer = new_avail - (avail_h - outer_h);
    for (p, v) in [
        ("/screen/width", 1710),
        ("/screen/height", 1112),
        ("/screen/avail_width", 1710),
        ("/screen/avail_height", new_avail),
        ("/window/outer_width", 1710),
        ("/window/inner_width", 1710),
        ("/window/outer_height", new_outer),
        ("/window/inner_height", new_outer - (outer_h - inner_h)),
    ] {
        if let Some(slot) = config.pointer_mut(p) {
            *slot = v.into();
        }
    }
}

/// Library ids dropped for another GPU: the Cedar capture (a PrimeOS tablet's bare driver
/// string) became an RX 6800 on 20.11. A retired id loads its successor, and its file goes.
pub(crate) const RETIRED_TEMPLATES: &[(&str, &str)] = &[("linux-unknown-amdcedardr", "linux-navi21")];

fn retired_successor(id: &str) -> Option<&'static str> {
    RETIRED_TEMPLATES.iter().find(|(old, _)| *old == id).map(|(_, new)| *new)
}

/// Linux templates claimed Vulkan until 20.11, which Chrome on Linux never reports (ANGLE draws
/// through GLX there): such a profile, or one from a retired template, takes the template's whole
/// WebGL block. Notes still holding the old renderer take the template's; a retired template's
/// tablet platform and touch screen take the new one's. An empty list or table stays.
fn adopt_linux_gl_webgl(config: &mut Value, src: &Value, successor: Option<&str>) {
    let renderer = |v: &Value| v.pointer("/webgl/renderer").and_then(|x| x.as_str()).unwrap_or("").to_owned();
    let (have, want) = (renderer(config), renderer(src));
    let desktop_linux = config.pointer("/navigator/user_agent").and_then(|v| v.as_str()).is_some_and(|ua| ua.contains("X11; Linux"));
    let glx = want.starts_with("ANGLE (") && want.contains(", OpenGL ") && !want.contains("OpenGL ES");
    if !desktop_linux || !glx || have == want || !(successor.is_some() || have.contains(", Vulkan ")) {
        return;
    }
    let Some(from) = src.get("webgl").and_then(|v| v.as_object()).cloned() else { return };
    let Some(to) = config.get_mut("webgl").and_then(|v| v.as_object_mut()) else { return };
    for (k, v) in from {
        let host_own = to
            .get(&k)
            .is_some_and(|x| x.as_array().is_some_and(|a| a.is_empty()) || x.as_object().is_some_and(|o| o.is_empty()));
        if !host_own {
            to.insert(k, v);
        }
    }
    if config.get("notes").and_then(|v| v.as_str()) == Some(have.as_str()) {
        if let Some(n) = src.get("notes") {
            config["notes"] = n.clone();
        }
    }
    let Some(new) = successor else { return };
    for k in ["platform_value", "max_touch_points"] {
        if let (Some(v), Some(nav)) = (src.pointer(&format!("/navigator/{k}")).cloned(), config.get_mut("navigator").and_then(|v| v.as_object_mut())) {
            nav.insert(k.into(), v);
        }
    }
    if let Some(m) = config.get_mut("_meta").and_then(|v| v.as_object_mut()) {
        if m.contains_key("gpu_preset_id") {
            m.insert("gpu_preset_id".into(), new.into());
        }
    }
}

/// WebGL extensions and getParameter answers the template gained for the same GPU (Chrome 158
/// added WEBGL_shader_pixel_local_storage and its two limits): added, never removed or changed.
/// An empty list or table means "the host's own" and stays.
fn adopt_webgl_extensions(config: &mut Value, src: &Value) {
    if config.pointer("/webgl/renderer").is_none() || config.pointer("/webgl/renderer") != src.pointer("/webgl/renderer") {
        return;
    }
    for k in ["extensions", "extensions_v2"] {
        let p = format!("/webgl/{k}");
        let Some(want) = src.pointer(&p).and_then(|v| v.as_array()).cloned() else { continue };
        let Some(have) = config.pointer_mut(&p).and_then(|v| v.as_array_mut()) else { continue };
        if have.is_empty() {
            continue;
        }
        for e in want {
            if e.is_string() && !have.contains(&e) {
                have.push(e);
            }
        }
    }
    let want = src.pointer("/webgl/parameters").and_then(|v| v.as_object()).cloned();
    if let (Some(want), Some(have)) = (want, config.pointer_mut("/webgl/parameters").and_then(|v| v.as_object_mut())) {
        if !have.is_empty() {
            for (k, v) in want {
                have.entry(k).or_insert(v);
            }
        }
    }
}

/// A desktop profile claiming 1-3 logical cores claims 4: a page can time workers on the real
/// machine, and anti.bot flags a claim under 8 whose workers scale past twice it.
fn adopt_desktop_min_cores(config: &mut Value) {
    if config_claims_mobile(config) {
        return;
    }
    if let Some(1..=3) = config.pointer("/navigator/hardware_concurrency").and_then(|v| v.as_u64()) {
        config["navigator"]["hardware_concurrency"] = 4.into();
    }
}

/// GPU makers whose bare driver strings the Android templates carried before 19.11.
const PHONE_GL_MAKERS: &[&str] = &["Qualcomm", "ARM"];

/// A phone's bare WebGL vendor/renderer in the ANGLE form Chrome on Android reports (#131):
/// "Qualcomm" / "Adreno (TM) 619" become "Google Inc. (Qualcomm)" / "ANGLE (Qualcomm, Adreno (TM) 619, OpenGL ES 3.2)".
fn adopt_phone_angle_webgl(config: &mut Value) {
    if !config_claims_mobile(config) {
        return;
    }
    let Some(gl) = config.get_mut("webgl").and_then(|v| v.as_object_mut()) else { return };
    let vendor = gl.get("vendor").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let Some(maker) = PHONE_GL_MAKERS.iter().find(|m| vendor == **m || vendor == format!("Google Inc. ({m})")) else {
        return;
    };
    gl.insert("vendor".into(), format!("Google Inc. ({maker})").into());
    let renderer = gl.get("renderer").and_then(|v| v.as_str()).unwrap_or("").to_string();
    if !renderer.is_empty() && !renderer.starts_with("ANGLE (") {
        gl.insert("renderer".into(), format!("ANGLE ({maker}, {renderer}, OpenGL ES 3.2)").into());
    }
}

fn platform_version_of(v: &Value, block: &str) -> Option<String> {
    v.get(block)?.get("platform_version")?.as_str().filter(|s| !s.is_empty()).map(String::from)
}

/// A phone's platform_version that is no Android release (a desktop's, carried over by a template
/// pick before #131) becomes its template's, else that of a template of the same model; else it
/// stays. Never the user agent's: Chrome 158 on Android always says "Android 10" there.
fn adopt_phone_platform_version(config: &mut Value, src: Option<&Value>, fingerprints_dir: &Path) {
    use crate::randomize::is_android_release;
    if !config_claims_mobile(config) {
        return;
    }
    // The engine reads client_hints over navigator.
    let current = platform_version_of(config, "client_hints").or_else(|| platform_version_of(config, "navigator"));
    if !current.is_some_and(|v| !is_android_release(&v)) {
        return;
    }
    let release = |t: &Value| platform_version_of(t, "client_hints").filter(|v| is_android_release(v));
    let Some(want) = src.and_then(release).or_else(|| newest_of_model(config, fingerprints_dir, release)) else {
        return;
    };
    for block in ["client_hints", "navigator"] {
        if platform_version_of(config, block).is_some_and(|v| !is_android_release(&v)) {
            config[block]["platform_version"] = want.clone().into();
        }
    }
}

/// The newest release a library template of the profile's model carries: two can differ.
fn newest_of_model(config: &Value, fingerprints_dir: &Path, release: impl Fn(&Value) -> Option<String>) -> Option<String> {
    let model = config.pointer("/client_hints/model").and_then(|v| v.as_str()).filter(|m| !m.is_empty())?;
    fs::read_dir(fingerprints_dir)
        .ok()?
        .flatten()
        .filter(|e| e.path().extension().and_then(|x| x.to_str()) == Some("json"))
        .filter_map(|e| serde_json::from_str::<Value>(&fs::read_to_string(e.path()).ok()?).ok())
        .filter(|t| t.pointer("/client_hints/model").and_then(|v| v.as_str()) == Some(model))
        .filter_map(|t| release(&t))
        .max_by_key(|v| v.split('.').next().and_then(|m| m.parse::<u32>().ok()))
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

    #[test]
    fn a_desktop_claim_under_four_cores_is_raised_a_phone_keeps_its_own() {
        // No template needed: an empty folder.
        let d = std::env::temp_dir().join(format!("shardx-lib-cores-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        for n in [1, 2, 3] {
            let mut desk = json!({ "navigator": { "platform": "Windows", "hardware_concurrency": n } });
            adopt_library_fields(&mut desk, &d);
            assert_eq!(desk["navigator"]["hardware_concurrency"], 4);
        }
        for n in [4, 6, 0] {
            let mut desk = json!({ "navigator": { "platform": "Win32", "hardware_concurrency": n } });
            adopt_library_fields(&mut desk, &d);
            assert_eq!(desk["navigator"]["hardware_concurrency"], n);
        }
        let mut phone = json!({ "navigator": { "platform": "Linux armv81", "hardware_concurrency": 2,
            "user_agent": "Mozilla/5.0 (Linux; Android 10; K)" }, "client_hints": { "mobile": true } });
        adopt_library_fields(&mut phone, &d);
        assert_eq!(phone["navigator"]["hardware_concurrency"], 2);
    }

    fn lib() -> std::path::PathBuf {
        // A folder per call: tests run in parallel and one rewriting a shared one broke another's read.
        static N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let d = std::env::temp_dir().join(format!("shardx-lib-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("win-rtx3060.json"), json!({ "name": "win-rtx3060", "fonts_local": ["Arial"], "webgpu": {
            "vendor": "nvidia", "architecture": "ampere", "features": ["shader-f16"], "subgroupMinSize": 32, "subgroupMaxSize": 32 } }).to_string()).unwrap();
        std::fs::write(d.join("android-x.json"), json!({ "name": "android-x", "webgpu": {
            "vendor": "arm", "architecture": "valhall", "features": ["shader-f16"], "subgroupMinSize": null, "subgroupMaxSize": null } }).to_string()).unwrap();
        d
    }

    #[test]
    fn a_linux_profile_moves_from_vulkan_to_glx_and_cedar_to_the_rx6800() {
        let ua = "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/158.0.0.0 Safari/537.36";
        let glx = "ANGLE (Intel, Mesa Intel(R) UHD Graphics 620 (KBL GT2), OpenGL 4.6)";
        let vk = "ANGLE (Intel, Vulkan 1.3.246 (Intel(R) UHD Graphics 620 (KBL GT2) (0x00005917)), Intel open-source Mesa driver)";
        let navi = "ANGLE (AMD, AMD Radeon RX 6800 (radeonsi navi21 LLVM 20.1.2), OpenGL 4.6)";
        let d = lib();
        std::fs::write(d.join("linux-uhd620.json"), json!({ "notes": glx, "navigator": { "platform_value": "Linux x86_64", "max_touch_points": 0 },
            "webgl": { "renderer": glx, "extensions": ["EXT_new"], "parameters": { "0x0D33": 16384 } } }).to_string()).unwrap();
        std::fs::write(d.join("linux-navi21.json"), json!({ "notes": navi, "navigator": { "platform_value": "Linux x86_64", "max_touch_points": 0 },
            "webgl": { "renderer": navi, "extensions": ["EXT_amd"] } }).to_string()).unwrap();
        let old = |id: &str, r: &str, notes: &str| json!({ "name": "Profile 7", "notes": notes, "_meta": { "gpu_preset_id": id },
            "navigator": { "user_agent": ua, "platform_value": "Linux armv81", "max_touch_points": 5 },
            "webgl": { "renderer": r, "extensions": ["EXT_old"], "parameters": { "0x0D33": 8192 }, "own_key": 1 } });

        let mut p = old("linux-uhd620", vk, vk);
        adopt_library_fields(&mut p, &d);
        let once = p.clone();
        adopt_library_fields(&mut p, &d);
        assert_eq!(p, once);
        assert_eq!(p["webgl"], json!({ "renderer": glx, "extensions": ["EXT_new"], "parameters": { "0x0D33": 16384 }, "own_key": 1 }));
        assert_eq!(p["notes"], glx);
        assert_eq!(p["navigator"]["platform_value"], "Linux armv81");
        let mut kept = old("linux-uhd620", vk, "work account");
        adopt_library_fields(&mut kept, &d);
        assert_eq!(kept["notes"], "work account");

        let mut cedar = old("linux-unknown-amdcedardr", "AMD CEDAR (DRM 2.49.0 / 4.9.194-primeos+, LLVM 7.0.0)", "x");
        adopt_library_fields(&mut cedar, &d);
        assert_eq!(cedar["webgl"]["renderer"], navi);
        assert_eq!(cedar["_meta"]["gpu_preset_id"], "linux-navi21");
        assert_eq!(cedar["navigator"]["platform_value"], "Linux x86_64");
        assert_eq!(cedar["navigator"]["max_touch_points"], 0);

        // An operator's own GLX capture, a host-own list, and a non-Linux profile stay.
        let es = "ANGLE (Intel, Mesa Intel(R) UHD Graphics 620 (KBL GT2), OpenGL ES 3.2)";
        let mut own = old("linux-uhd620", es, "x");
        adopt_library_fields(&mut own, &d);
        assert_eq!(own["webgl"]["renderer"], es);
        let mut host = old("linux-uhd620", vk, "x");
        host["webgl"]["extensions"] = json!([]);
        adopt_library_fields(&mut host, &d);
        assert_eq!((host["webgl"]["extensions"].clone(), host["webgl"]["renderer"].clone()), (json!([]), json!(glx)));
        let mut win = old("linux-uhd620", vk, "x");
        win["navigator"]["user_agent"] = json!("Mozilla/5.0 (Windows NT 10.0; Win64; x64)");
        adopt_library_fields(&mut win, &d);
        assert_eq!(win["webgl"]["renderer"], vk);
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

    #[test]
    fn an_air15_takes_its_battery_screen_and_extensions_once() {
        let d = lib();
        let r = "ANGLE (Apple, ANGLE Metal Renderer: Apple M3, Unspecified Version)";
        std::fs::write(d.join("mac-m3-air15.json"), json!({
            "screen": { "width": 1710, "height": 1112, "avail_width": 1710, "avail_height": 1075 },
            "battery": { "laptop": true },
            "webgl": { "renderer": r, "extensions_v2": ["EXT_a", "WEBGL_shader_pixel_local_storage"],
                       "parameters": { "0x0D33": 16384, "0x96E0": 8, "0x96E1": 8 } } }).to_string()).unwrap();
        let mut cfg = json!({ "_meta": { "gpu_preset_id": "mac-m3-air15" },
            "screen": { "width": 1680, "height": 1050, "avail_width": 1680, "avail_height": 1013 },
            "window": { "outer_width": 1680, "outer_height": 1012, "inner_width": 1680, "inner_height": 925 },
            "battery": { "charging": true, "level": 1, "charging_time": 0, "discharging_time": "Infinity" },
            "webgl": { "renderer": r, "extensions_v2": ["EXT_a"], "parameters": { "0x0D33": 8192 } } });
        adopt_library_fields(&mut cfg, &d);
        adopt_library_fields(&mut cfg, &d);
        assert_eq!(cfg["battery"], json!({ "laptop": true }));
        assert_eq!((cfg["screen"]["width"].clone(), cfg["screen"]["avail_height"].clone()), (json!(1710), json!(1075)));
        assert_eq!((cfg["window"]["outer_height"].clone(), cfg["window"]["inner_height"].clone()), (json!(1074), json!(987)));
        assert_eq!(cfg["webgl"]["extensions_v2"], json!(["EXT_a", "WEBGL_shader_pixel_local_storage"]));
        // New limits come in; a value the profile already has stays.
        assert_eq!(cfg["webgl"]["parameters"], json!({ "0x0D33": 8192, "0x96E0": 8, "0x96E1": 8 }));
        let mut host = json!({ "_meta": { "gpu_preset_id": "mac-m3-air15" }, "webgl": { "renderer": r, "parameters": {} } });
        adopt_library_fields(&mut host, &d);
        assert_eq!(host["webgl"]["parameters"], json!({}), "an empty table means the host's values");
        let mut own = json!({ "_meta": { "gpu_preset_id": "mac-m3-air15" }, "battery": { "charging": false, "level": 0.4 } });
        adopt_library_fields(&mut own, &d);
        assert_eq!(own["battery"], json!({ "charging": false, "level": 0.4 }));
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

#[cfg(test)]
mod phone_tests {
    use super::adopt_library_fields;
    use crate::screen::{apply_screen_strategy, ScreenStrategy};
    use serde_json::{json, Value};

    const PHONE_UA: &str = "Mozilla/5.0 (Linux; Android 10; K) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/158.0.0.0 Mobile Safari/537.36";

    fn lib() -> std::path::PathBuf {
        // A folder per call, as above.
        static N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let d = std::env::temp_dir().join(format!("shardx-phone-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        for (name, model, v) in [("android-s24", "SM-S928B", "16.0.0"), ("android-a35", "SM-A356E", "16.0.0"), ("android-a35-2", "SM-A356E", "15.0.0")] {
            std::fs::write(d.join(format!("{name}.json")), json!({ "client_hints": { "model": model, "platform_version": v } }).to_string()).unwrap();
        }
        d
    }

    fn phone(key: &str, model: &str, version: &str, nav: Option<&str>) -> Value {
        let mut p = json!({ "_meta": { "gpu_preset_id": key }, "navigator": { "user_agent": PHONE_UA },
            "client_hints": { "mobile": true, "model": model, "platform_version": version } });
        if let Some(n) = nav {
            p["navigator"]["platform_version"] = json!(n);
        }
        p
    }

    /// #131: Chrome on Android has no WebGL backend but ANGLE.
    #[test]
    fn a_phones_bare_webgl_takes_the_angle_form_once() {
        let d = lib();
        let mut q = json!({ "navigator": { "user_agent": PHONE_UA }, "webgl": { "vendor_masked": "WebKit", "vendor": "Qualcomm", "renderer": "Adreno (TM) 619" } });
        adopt_library_fields(&mut q, &d);
        adopt_library_fields(&mut q, &d);
        assert_eq!(q["webgl"], json!({ "vendor_masked": "WebKit", "vendor": "Google Inc. (Qualcomm)", "renderer": "ANGLE (Qualcomm, Adreno (TM) 619, OpenGL ES 3.2)" }));
        let mut half = json!({ "client_hints": { "mobile": true }, "webgl": { "vendor": "Google Inc. (ARM)", "renderer": "Mali-G68" } });
        adopt_library_fields(&mut half, &d);
        assert_eq!(half["webgl"]["renderer"], "ANGLE (ARM, Mali-G68, OpenGL ES 3.2)");
        for mut cfg in [json!({ "navigator": { "user_agent": PHONE_UA }, "webgl": { "vendor": "Imagination Technologies", "renderer": "PowerVR Rogue GE8320" } }),
                        json!({ "navigator": { "user_agent": "Mozilla/5.0 (X11; Linux x86_64)" }, "webgl": { "vendor": "ARM", "renderer": "Mali-G57" } })] {
            let before = cfg.clone();
            adopt_library_fields(&mut cfg, &d);
            assert_eq!(cfg, before);
        }
    }

    /// #131: a template pick carried the previous OS's version onto a phone.
    #[test]
    fn a_desktop_version_becomes_the_templates_else_the_models() {
        let d = lib();
        let mut mac = phone("android-s24", "SM-S928B", "15.7.1", Some("15.7.1"));
        adopt_library_fields(&mut mac, &d);
        assert_eq!((mac["client_hints"]["platform_version"].clone(), mac["navigator"]["platform_version"].clone()), (json!("16.0.0"), json!("16.0.0")));
        let mut gone = phone("android-gone", "SM-A356E", "26.1.0", None);
        adopt_library_fields(&mut gone, &d);
        assert_eq!(gone["client_hints"]["platform_version"], "16.0.0", "the newest of the model's");
        assert!(gone["navigator"].get("platform_version").is_none());
    }

    #[test]
    fn no_template_and_no_model_leave_it_never_the_user_agents() {
        let d = lib();
        for mut cfg in [phone("android-gone", "SM-X000", "26.1.0", None), phone("android-s24", "SM-S928B", "13.0.0", Some("6.8.0")),
                        phone("android-s24", "SM-S928B", "", None),
                        json!({ "_meta": { "gpu_preset_id": "android-s24" }, "navigator": { "platform": "macOS" }, "client_hints": { "platform_version": "15.7.1" } })] {
            let before = cfg.clone();
            adopt_library_fields(&mut cfg, &d);
            assert_eq!(cfg, before);
        }
    }

    #[test]
    fn a_phone_keeps_its_own_screen_in_every_mode() {
        let mut cfg = json!({ "navigator": { "user_agent": PHONE_UA, "platform": "Linux" },
            "screen": { "width": 412, "height": 892, "avail_width": 412, "avail_height": 892 } });
        let before = cfg.clone();
        apply_screen_strategy(&mut cfg, ScreenStrategy::UseHost);
        apply_screen_strategy(&mut cfg, ScreenStrategy::CapToHost);
        assert_eq!(cfg, before);
    }
}
