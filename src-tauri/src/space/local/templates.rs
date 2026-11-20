//! Profile templates (#145) in `templates.json` beside the settings: what a new
//! profile starts with. A fingerprint is never kept, only where a fresh one comes from.

use crate::store;
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::collections::HashSet;
use std::fs;
use std::io::Write;
use std::path::Path;
use std::sync::Mutex;

const VERSION: u32 = 1;
/// Longest template name, as the interface cuts it.
pub const NAME_MAX: usize = 64;

/// Where each profile made from a template gets a fingerprint of its own.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Source {
    /// The library fingerprint each one is drawn from; None: any of `os`.
    #[serde(default)]
    pub library: Option<String>,
    /// "macOS", "Windows", "Linux" or "Android": the pick when there is no
    /// library fingerprint, or this computer does not have it.
    #[serde(default)]
    pub os: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct ProfileTemplate {
    /// A blank id gets a new one.
    #[serde(default)]
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub source: Source,
    /// What a new profile takes, as a stored profile holds it: only [`settings_of`]'s keys.
    #[serde(default)]
    pub settings: Map<String, Value>,
    #[serde(default)]
    pub folder: String,
    /// Tag ids of the space the template is in.
    #[serde(default)]
    pub tags: Vec<String>,
    /// "New profile" starts from it; one per space.
    #[serde(default)]
    pub is_default: bool,
    /// Unix seconds of the last save.
    #[serde(default)]
    pub updated_at: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct TemplateState {
    #[serde(default)]
    pub templates: Vec<ProfileTemplate>,
}

#[derive(Serialize, Deserialize)]
struct Stored {
    version: u32,
    #[serde(flatten)]
    state: TemplateState,
}

/// The parts of a stored profile a template carries: never its name, notes,
/// cookies or anything of its fingerprint.
const TOP: &[&str] = &["timezone", "icu_locale", "ui_language", "webrtc", "geolocation", "blocked_ports", "media_devices", "noise"];
const NESTED: &[(&str, &[&str])] = &[
    ("navigator", &["language", "accept_language", "languages", "do_not_track"]),
    ("screen", &["refresh_rate"]),
    ("window", &["keep_focus", "maximized"]),
    ("_meta", &["proxy_id", "color", "extensions", "android_media"]),
];

/// A template's part of a stored profile (`_meta` inside); the noise seeds stay each profile's own.
pub fn settings_of(config: &Map<String, Value>) -> Map<String, Value> {
    let mut out = Map::new();
    for k in TOP {
        if let Some(v) = config.get(*k) {
            out.insert(k.to_string(), v.clone());
        }
    }
    for (block, keys) in NESTED {
        let Some(src) = config.get(*block).and_then(Value::as_object) else { continue };
        let picked: Map<String, Value> =
            keys.iter().filter_map(|k| src.get(*k).map(|v| (k.to_string(), v.clone()))).collect();
        if !picked.is_empty() {
            out.insert(block.to_string(), Value::Object(picked));
        }
    }
    if let Some(noise) = out.get_mut("noise").and_then(Value::as_object_mut) {
        for block in noise.values_mut().filter_map(Value::as_object_mut) {
            block.insert("seed".into(), Value::from(0u32));
        }
    }
    out
}

/// The template's settings over a new profile's config; a block's other keys stay.
pub fn apply(config: &mut Map<String, Value>, settings: &Map<String, Value>) {
    for (k, v) in settings_of(settings) {
        let nested = NESTED.iter().any(|(b, _)| *b == k);
        match (config.get_mut(&k), v) {
            (Some(Value::Object(dst)), Value::Object(src)) if nested => dst.extend(src),
            (_, v) => {
                config.insert(k, v);
            }
        }
    }
}

/// The OS control's name for a fingerprint's platform (constants.ts `osIdFor`).
pub fn os_of(platform: &str) -> String {
    let p = platform.trim().to_ascii_lowercase();
    let os = if p.starts_with("android") || p.starts_with("linux arm") {
        "Android"
    } else if p.starts_with("linux") {
        "Linux"
    } else if p.starts_with("win") {
        "Windows"
    } else if p.starts_with("mac") || p == "darwin" || p == "osx" {
        "macOS"
    } else {
        ""
    };
    os.to_string()
}

/// Where a profile's fingerprint came from: its library fingerprint, and its OS.
pub fn source_of(config: &Map<String, Value>) -> Source {
    let text = |block: &str, key: &str| config.get(block).and_then(|b| b.get(key)).and_then(Value::as_str).unwrap_or("");
    let library = Some(text("_meta", "gpu_preset_id").trim().to_string()).filter(|s| !s.is_empty());
    let ua = text("navigator", "user_agent").to_ascii_lowercase();
    // A phone's navigator.platform says "Linux armv8l"; its user agent says Android.
    let os = if ua.contains("android") { "Android".to_string() } else { os_of(text("navigator", "platform")) };
    Source { library, os }
}

/// Trimmed, inner runs of whitespace as one space, at most `NAME_MAX` characters.
pub fn clean_name(name: &str) -> String {
    name.split_whitespace().collect::<Vec<_>>().join(" ").chars().take(NAME_MAX).collect()
}

fn now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// A template as it is kept: a name, its settings cut to what a template
/// carries, an OS the control knows, its tags once each, a new id when blank.
pub fn cleaned(t: ProfileTemplate) -> Result<ProfileTemplate> {
    let name = clean_name(&t.name);
    if name.is_empty() {
        bail!("a template needs a name");
    }
    let mut seen = HashSet::new();
    let tags = t.tags.into_iter().map(|x| x.trim().to_string()).filter(|x| !x.is_empty() && seen.insert(x.clone())).collect();
    let library = t.source.library.map(|l| l.trim().to_string()).filter(|l| !l.is_empty());
    let os = match os_of(&t.source.os) {
        os if os.is_empty() => t.source.os.trim().to_string(),
        os => os,
    };
    let id = match t.id.trim() {
        "" => format!("pt-{}", &uuid::Uuid::new_v4().simple().to_string()[..8]),
        id => id.to_string(),
    };
    Ok(ProfileTemplate {
        id,
        name,
        source: Source { library, os },
        settings: settings_of(&t.settings),
        folder: t.folder.trim().to_string(),
        tags,
        is_default: t.is_default,
        updated_at: now(),
    })
}

fn same(a: &str, b: &str) -> bool {
    a.to_lowercase() == b.to_lowercase()
}

impl TemplateState {
    /// Another template than `id` with this name, as names are told apart here.
    pub(in crate::space) fn named(&self, name: &str, id: &str) -> Option<&ProfileTemplate> {
        let name = clean_name(name);
        self.templates.iter().find(|t| t.id != id && same(&t.name, &name))
    }

    /// Adds the template or changes the one with its id; a default takes the mark from the rest.
    pub(in crate::space) fn upsert(&mut self, t: ProfileTemplate) -> Result<ProfileTemplate> {
        let t = cleaned(t)?;
        if self.named(&t.name, &t.id).is_some() {
            bail!("a template named {} already exists", t.name);
        }
        if t.is_default {
            for other in self.templates.iter_mut().filter(|o| o.id != t.id) {
                other.is_default = false;
            }
        }
        match self.templates.iter_mut().find(|o| o.id == t.id) {
            Some(slot) => *slot = t.clone(),
            None => self.templates.push(t.clone()),
        }
        Ok(t)
    }

    fn remove(&mut self, id: &str) {
        self.templates.retain(|t| t.id != id);
    }
}

/// Serialises writers: two windows, or a window and the API, never interleave.
static LOCK: Mutex<()> = Mutex::new(());

/// The saved templates, or None while nothing was ever saved.
fn read(path: &Path) -> Result<Option<TemplateState>> {
    let raw = match fs::read(path) {
        Ok(raw) => raw,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e).with_context(|| format!("read {}", path.display())),
    };
    let stored: Stored =
        serde_json::from_slice(&raw).with_context(|| format!("{} is not readable as profile templates", path.display()))?;
    if stored.version > VERSION {
        bail!("{} was written by a newer launcher", path.display());
    }
    Ok(Some(stored.state))
}

fn write(path: &Path, state: &TemplateState) -> Result<()> {
    let body = serde_json::to_vec_pretty(&Stored { version: VERSION, state: state.clone() })?;
    let tmp = path.with_extension("json.tmp");
    {
        let mut f = fs::File::create(&tmp).with_context(|| format!("write {}", tmp.display()))?;
        f.write_all(&body)?;
        f.sync_all()?;
    }
    fs::rename(&tmp, path).with_context(|| format!("replace {}", path.display()))
}

/// A change that changes nothing writes nothing.
fn change_at<T>(path: &Path, f: impl FnOnce(&mut TemplateState) -> Result<T>) -> Result<T> {
    let before = read(path)?;
    let mut state = before.clone().unwrap_or_default();
    let out = f(&mut state)?;
    if before.unwrap_or_default() != state {
        write(path, &state)?;
    }
    Ok(out)
}

fn change<T>(f: impl FnOnce(&mut TemplateState) -> Result<T>) -> Result<T> {
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    change_at(&store::templates_path()?, f)
}

pub(in crate::space) fn list() -> Result<Vec<ProfileTemplate>> {
    Ok(read(&store::templates_path()?)?.unwrap_or_default().templates)
}

pub(in crate::space) fn save(t: ProfileTemplate) -> Result<ProfileTemplate> {
    change(|s| s.upsert(t))
}

pub(in crate::space) fn delete(id: &str) -> Result<()> {
    change(|s| {
        s.remove(id);
        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::path::PathBuf;

    struct Dir(PathBuf);
    impl Drop for Dir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    fn dir() -> Dir {
        let d = std::env::temp_dir().join(format!("shardx-templates-{}", uuid::Uuid::new_v4().simple()));
        fs::create_dir_all(&d).unwrap();
        Dir(d)
    }
    fn obj(v: Value) -> Map<String, Value> {
        v.as_object().cloned().unwrap()
    }
    fn tpl(id: &str, name: &str) -> ProfileTemplate {
        ProfileTemplate { id: id.into(), name: name.into(), ..Default::default() }
    }

    /// A profile as the form writes it: identity, fingerprint and settings together.
    fn profile() -> Map<String, Value> {
        obj(json!({
            "_meta": { "id": "p1", "rev": 4, "proxy_id": "x1", "gpu_preset_id": "win-rtx", "color": "#ff0000",
                       "extensions": ["e1"], "folder": "Ads", "screen_custom": true, "created_at": "@1" },
            "name": "Shop 1", "notes": "secret notes",
            "timezone": "auto", "icu_locale": null, "webrtc": "tcp_only",
            "navigator": { "platform": "Win32", "user_agent": "Mozilla/5.0 (Windows NT 10.0)", "language": "de-DE",
                           "accept_language": "de-DE,de;q=0.9", "languages": ["de-DE", "de"], "hardware_concurrency": 12,
                           "device_memory": 16, "do_not_track": "1" },
            "screen": { "width": 1920, "height": 1080, "refresh_rate": 144 },
            "window": { "outer_width": 1920, "keep_focus": true },
            "webgl": { "renderer": "RTX" },
            "noise": { "canvas": { "enabled": true, "seed": 77 }, "webgl": { "enabled": true, "seed": 9, "intensity": 0.0005 } },
            "blocked_ports": [3389], "geolocation": { "mode": "auto" },
            "media_devices": { "audio_input_count": 2 },
        }))
    }

    #[test]
    fn a_template_keeps_the_settings_and_nothing_of_the_identity_or_fingerprint() {
        let s = settings_of(&profile());
        assert_eq!(
            Value::Object(s.clone()),
            json!({
                "timezone": "auto", "icu_locale": null, "webrtc": "tcp_only",
                "geolocation": { "mode": "auto" }, "blocked_ports": [3389], "media_devices": { "audio_input_count": 2 },
                "noise": { "canvas": { "enabled": true, "seed": 0 }, "webgl": { "enabled": true, "seed": 0, "intensity": 0.0005 } },
                "navigator": { "language": "de-DE", "accept_language": "de-DE,de;q=0.9", "languages": ["de-DE", "de"], "do_not_track": "1" },
                "screen": { "refresh_rate": 144 },
                "window": { "keep_focus": true },
                "_meta": { "proxy_id": "x1", "color": "#ff0000", "extensions": ["e1"] },
            })
        );
        assert_eq!(source_of(&profile()), Source { library: Some("win-rtx".into()), os: "Windows".into() });
        // Cut again, it stays as it is.
        assert_eq!(settings_of(&s), s);
    }

    #[test]
    fn applied_over_a_fresh_fingerprint_the_settings_win_and_the_fingerprint_stays() {
        let mut fresh = obj(json!({
            "_meta": { "id": "", "gpu_preset_id": "win-gtx" },
            "navigator": { "platform": "Win32", "language": "auto", "hardware_concurrency": 8 },
            "screen": { "width": 2560, "height": 1440, "refresh_rate": 60 },
            "noise": { "canvas": { "enabled": false, "seed": 0 } },
            "webgl": { "renderer": "GTX" },
        }));
        apply(&mut fresh, &settings_of(&profile()));
        assert_eq!(fresh["_meta"], json!({ "id": "", "gpu_preset_id": "win-gtx", "proxy_id": "x1", "color": "#ff0000", "extensions": ["e1"] }));
        assert_eq!(fresh["navigator"]["hardware_concurrency"], 8);
        assert_eq!(fresh["navigator"]["language"], "de-DE");
        assert_eq!(fresh["screen"], json!({ "width": 2560, "height": 1440, "refresh_rate": 144 }));
        assert_eq!(fresh["webgl"]["renderer"], "GTX");
        assert_eq!(fresh["noise"]["canvas"], json!({ "enabled": true, "seed": 0 }));
        assert!(fresh.get("name").is_none() && fresh.get("notes").is_none());
        // Settings handed in whole are cut first: a name or a fingerprint never lands.
        let mut again = obj(json!({ "_meta": { "id": "" } }));
        apply(&mut again, &profile());
        assert!(again.get("name").is_none() && again.get("webgl").is_none());
        assert!(again["_meta"].get("gpu_preset_id").is_none());
    }

    #[test]
    fn a_phone_and_an_imported_platform_read_as_the_os_control_names_them() {
        let phone = obj(json!({ "navigator": { "platform": "Linux armv8l", "user_agent": "Mozilla/5.0 (Linux; Android 14)" } }));
        assert_eq!(source_of(&phone), Source { library: None, os: "Android".into() });
        assert_eq!(os_of("MacIntel"), "macOS");
        assert_eq!(os_of("Linux x86_64"), "Linux");
        assert_eq!(os_of("Linux armv81"), "Android");
        assert_eq!(os_of("Plan 9"), "");
    }

    #[test]
    fn templates_round_trip_through_the_file_with_one_default_and_unique_names() {
        let d = dir();
        let path = d.0.join("templates.json");
        assert_eq!(read(&path).unwrap(), None);
        let made = change_at(&path, |s| {
            let a = s.upsert(ProfileTemplate { is_default: true, settings: profile(), ..tpl("", "  Ads   base ") })?;
            s.upsert(ProfileTemplate { is_default: true, ..tpl("pt-b", "Shops") })?;
            Ok(a)
        })
        .unwrap();
        assert!(made.id.starts_with("pt-") && made.name == "Ads base");
        assert!(made.settings.get("name").is_none(), "the settings were cut");
        let back = read(&path).unwrap().unwrap();
        assert_eq!(back.templates.iter().map(|t| (t.name.as_str(), t.is_default)).collect::<Vec<_>>(), [("Ads base", false), ("Shops", true)]);
        let raw: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(raw["version"], 1);

        let taken = change_at(&path, |s| s.upsert(tpl("pt-c", "ads BASE")).map(|_| ()));
        assert!(taken.unwrap_err().to_string().contains("already exists"));
        assert!(change_at(&path, |s| s.upsert(tpl("pt-c", "   ")).map(|_| ())).is_err());
        // Renaming one to its own name in another case is fine.
        change_at(&path, |s| s.upsert(tpl(&made.id, "ADS base")).map(|_| ())).unwrap();
        change_at(&path, |s| {
            s.remove("pt-b");
            Ok(())
        })
        .unwrap();
        assert_eq!(read(&path).unwrap().unwrap().templates.iter().map(|t| t.name.as_str()).collect::<Vec<_>>(), ["ADS base"]);
    }

    #[test]
    fn a_change_that_changes_nothing_makes_no_file_and_a_broken_one_stays() {
        let d = dir();
        let path = d.0.join("templates.json");
        change_at(&path, |s| {
            s.remove("pt-none");
            Ok(())
        })
        .unwrap();
        assert!(!path.exists());
        fs::write(&path, "{not json").unwrap();
        assert!(change_at(&path, |s| s.upsert(tpl("", "A")).map(|_| ())).is_err());
        assert_eq!(fs::read_to_string(&path).unwrap(), "{not json");
    }
}
