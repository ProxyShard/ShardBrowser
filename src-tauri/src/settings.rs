use crate::store;
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::fs;
use std::sync::Mutex;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Settings {
    /// Absolute path to the ShardX executable.
    pub browser_path: Option<String>,
    /// Theme: "dark" (default) or "light".
    #[serde(default = "default_theme")]
    pub theme: String,
    /// Geo-IP checker provider used by the proxy "Test" button.
    /// One of "ip-api.com" | "ipapi.co" | "ipwho.is".
    #[serde(default)]
    pub geo_checker: Option<String>,
    /// "fingerprint" (use the screen from the bound fingerprint) or
    /// "real" (let ShardX use the host's real screen).
    #[serde(default)]
    pub screen_resolution_mode: Option<String>,
    /// Offer to fill fields a generated identity fits. Never applies to a
    /// synchronised launch — input is already mirrored there.
    #[serde(default = "default_true")]
    pub helper_enabled: bool,
    /// Profile's camera is ShardX's rather than the machine's. On by default:
    /// the host's real camera contradicts the fingerprint and links profiles.
    #[serde(default = "default_true")]
    pub camera_enabled: bool,
    /// The profile's name in the address bar and the window title (#126).
    #[serde(default = "default_true")]
    pub show_profile_name: bool,
    /// Offer password protection for profiles (#127). Off by default.
    #[serde(default)]
    pub profile_passwords: bool,
    /// Field kinds the helper reacts to, as the engine names them. Empty = all.
    #[serde(default)]
    pub helper_triggers: Vec<String>,
    /// Hide the launcher to the system tray on close instead of quitting.
    #[serde(default = "default_minimize_to_tray")]
    pub minimize_to_tray: bool,
    /// Appended to every launch, one per line. Applied last, so a repeat wins.
    #[serde(default)]
    pub extra_args: String,
    /// Where profiles, user-data, extensions and the trash live. None = the
    /// config dir. Changed through `data_root_migrate`, never by hand.
    #[serde(default)]
    pub data_root: Option<String>,
    /// Every 5 h ask the Web Store about the library's linked extensions,
    /// straight from this computer's IP, not through a profile's proxy.
    #[serde(default = "default_true")]
    pub ext_update_check: bool,

    // ---- Local automation HTTP API (axum + JWT bearer) ----
    /// Whether the local API server listens on 127.0.0.1:`api_port`.
    #[serde(default = "default_api_enabled")]
    pub api_enabled: bool,
    /// Port the API binds on 127.0.0.1.
    #[serde(default = "default_api_port")]
    pub api_port: u16,
    /// HS256 signing key for API JWTs.  Auto-generated on first run
    /// (see `ensure_secret`); rotating it invalidates issued tokens.
    #[serde(default)]
    pub api_secret: String,
}

fn default_true() -> bool {
    true
}

fn default_theme() -> String {
    "dark".into()
}

fn default_minimize_to_tray() -> bool {
    true
}

fn default_api_enabled() -> bool {
    true
}

fn default_api_port() -> u16 {
    40325
}

/// Why the settings file could not be read, if it could not. The file is kept
/// as it is until the user saves over it, so nothing is lost meanwhile.
static LOAD_ERROR: Mutex<Option<String>> = Mutex::new(None);

/// The settings a fresh install starts with — the serde defaults, spelled out.
fn defaults() -> Settings {
    Settings {
        browser_path: None,
        theme: default_theme(),
        geo_checker: Some("ip-api.com".into()),
        screen_resolution_mode: Some("fingerprint".into()),
        helper_enabled: default_true(),
        camera_enabled: default_true(),
        show_profile_name: default_true(),
        profile_passwords: false,
        helper_triggers: Vec::new(),
        minimize_to_tray: default_minimize_to_tray(),
        extra_args: String::new(),
        data_root: None,
        ext_update_check: default_true(),
        api_enabled: default_api_enabled(),
        api_port: default_api_port(),
        api_secret: String::new(),
    }
}

/// Read the file as text whatever PowerShell wrote it as. `Set-Content
/// -Encoding UTF8` puts a byte-order mark in front of UTF-8, and a plain `>`
/// redirect writes UTF-16 — both of which used to be a parse failure, and a
/// parse failure used to reset every setting.
fn decode_text(bytes: &[u8]) -> String {
    if bytes.starts_with(&[0xFF, 0xFE]) || bytes.starts_with(&[0xFE, 0xFF]) {
        let big_endian = bytes[0] == 0xFE;
        let units: Vec<u16> = bytes[2..]
            .chunks_exact(2)
            .map(|c| {
                if big_endian {
                    u16::from_be_bytes([c[0], c[1]])
                } else {
                    u16::from_le_bytes([c[0], c[1]])
                }
            })
            .collect();
        return String::from_utf16_lossy(&units);
    }
    let body = String::from_utf8_lossy(bytes).into_owned();
    body.strip_prefix('\u{feff}').map(str::to_owned).unwrap_or(body)
}

/// The message for the UI banner, or None while the file reads fine.
pub fn load_error() -> Option<String> {
    LOAD_ERROR.lock().ok().and_then(|g| g.clone())
}

pub fn load() -> Result<Settings> {
    let path = store::settings_path()?;
    if !path.exists() {
        return Ok(defaults());
    }
    let bytes = fs::read(&path)?;
    let body = decode_text(&bytes);
    match serde_json::from_str::<Settings>(&body) {
        Ok(s) => {
            if let Ok(mut g) = LOAD_ERROR.lock() {
                *g = None;
            }
            Ok(s)
        }
        Err(e) => {
            // Defaults for this run only: the file stays as it is, and `save`
            // keeps a copy of it before writing over it.
            let msg = format!("{} could not be read ({e}); running on defaults", path.display());
            eprintln!("[launcher] settings: {msg}");
            if let Ok(mut g) = LOAD_ERROR.lock() {
                *g = Some(msg);
            }
            Ok(defaults())
        }
    }
}

/// Load settings, generating + persisting the API JWT secret if it's
/// still empty.  Call once at startup before the server reads it.
/// The secret minted for a run whose settings file could not be read. Held
/// here because `ensure_secret` is called more than once — at startup for the
/// server and again whenever the UI asks for a token — and minting a fresh one
/// each time would hand out tokens the running server rejects.
static RUN_SECRET: Mutex<Option<String>> = Mutex::new(None);

pub fn ensure_secret() -> Result<Settings> {
    let mut s = load()?;
    if !s.api_secret.is_empty() {
        return Ok(s);
    }
    // An unreadable file may well hold a working secret, and writing a new one
    // over it would sign out every API client over a stray byte. Mint one for
    // this run only, and reuse it for the rest of the run.
    if load_error().is_some() {
        let mut g = RUN_SECRET
            .lock()
            .map_err(|_| anyhow::anyhow!("run-secret lock poisoned"))?;
        if g.is_none() {
            *g = Some(new_secret());
        }
        s.api_secret = g.clone().unwrap_or_default();
        return Ok(s);
    }
    s.api_secret = new_secret();
    save(&s)?;
    Ok(s)
}

fn new_secret() -> String {
    format!(
        "{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    )
}

/// Split into switches on whitespace; quoted runs survive.
pub fn parse_extra_args(raw: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quote: Option<char> = None;
    for ch in raw.chars() {
        match (quote, ch) {
            (Some(q), c) if c == q => quote = None,
            (Some(_), c) => cur.push(c),
            (None, c @ ('"' | '\'')) => quote = Some(c),
            (None, c) if c.is_whitespace() => {
                if !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                }
            }
            (None, c) => cur.push(c),
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

pub fn save(s: &Settings) -> Result<()> {
    let path = store::settings_path()?;
    // Keep the unreadable file next to the new one — it is the only copy of
    // whatever the user had configured.
    if load_error().is_some() && path.exists() {
        let backup = path.with_extension("json.bad");
        if let Err(e) = fs::rename(&path, &backup) {
            eprintln!("[launcher] settings: could not keep a copy of the old file: {e}");
        } else {
            eprintln!("[launcher] settings: kept the unreadable file as {}", backup.display());
        }
    }
    let body = serde_json::to_string_pretty(s)?;
    fs::write(&path, body)?;
    if let Ok(mut g) = LOAD_ERROR.lock() {
        *g = None;
    }
    Ok(())
}
