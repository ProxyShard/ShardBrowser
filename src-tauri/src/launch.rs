use crate::{
    bookmarks, extensions,
    process::{self, Tracker},
    profile, proxy, settings, store,
};
use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use std::process::Stdio;

/// Launch result: OS pid plus CDP endpoint when remote-debugging is on.
pub struct LaunchOutcome {
    pub pid: u32,
    pub cdp: Option<process::CdpInfo>,
    /// Why `cdp` is empty, so a client knows to poll rather than guess.
    pub cdp_error: Option<String>,
}

/// Resolve the ShardX executable from settings, runtime cache, or dev guess.
pub fn resolve_binary() -> Result<PathBuf> {
    if let Some(p) = settings::load()?.browser_path {
        let pb = PathBuf::from(p);
        if pb.exists() {
            return Ok(pb);
        }
    }
    if let Ok(pb) = crate::runtime::binary_path() {
        if pb.exists() {
            return Ok(pb);
        }
    }
    #[cfg(target_os = "macos")]
    let guess = "/Users/kritos/Documents/GitHub/ShardXBrowser/build/src/out/Release_GN_arm64/ShardX.app/Contents/MacOS/ShardX";
    #[cfg(target_os = "windows")]
    let guess = "C:\\Program Files\\ShardX\\ShardX.exe";
    #[cfg(target_os = "linux")]
    let guess = "/opt/shardx/shardx";
    let pb = PathBuf::from(guess);
    if pb.exists() {
        return Ok(pb);
    }
    anyhow::bail!("ShardX browser not installed yet — open Settings to download, or configure Browser path manually")
}

pub async fn launch_profile(
    profile_id: &str,
    enable_cdp: bool,
    headless: bool,
) -> Result<LaunchOutcome> {
    launch_profile_synced(profile_id, enable_cdp, headless, None, 0, "").await
}

/// As `launch_profile`, but joins the browser to a synchronisation group:
/// every profile launched under the same `sync_group` mirrors input.
pub async fn launch_profile_synced(
    profile_id: &str,
    enable_cdp: bool,
    headless: bool,
    sync_group: Option<&str>,
    bus_port: u16,
    bus_token: &str,
) -> Result<LaunchOutcome> {
    // One browser per profile: two children sharing a user-data dir corrupt each other's
    // state, and the second displaces the first in the tracker, leaving it unstoppable.
    if Tracker::shared().is_running(profile_id) {
        anyhow::bail!("profile {profile_id} is already running");
    }
    let bin = resolve_binary()?;
    let stored = profile::load_raw(profile_id)?;
    let udd = profile::user_data_dir(profile_id)?;

    // Stored proxy by id, else ephemeral inline (quick profiles, not in store).
    let bound_proxy: Option<proxy::ProxyEntry> = stored
        .meta
        .proxy_id
        .as_deref()
        .and_then(|pid| proxy::get(pid).ok().flatten())
        .or_else(|| stored.meta.inline_proxy.clone());

    // Live UDP probe; QUIC/WebRTC gating uses current capability not stale cache.
    let proxy_udp_ok = if let Some(p) = bound_proxy.as_ref() {
        if matches!(p.kind, proxy::ProxyKind::Socks5) {
            match proxy::probe_udp(p).await {
                Ok(ms) => {
                    eprintln!("[launcher] UDP relay OK ({ms} ms) for proxy {}", p.host);
                    true
                }
                Err(e) => {
                    // A failed probe does not mean the proxy has no relay. Some
                    // VPNs pass UDP for one application and drop it for another,
                    // and the launcher can be on the losing side while the
                    // browser it starts is not — measured on a Mac where the
                    // browser got STUN replies and a plain binary got none.
                    // Treating that as "no UDP" took QUIC and proxied WebRTC
                    // away from a browser that could have used both.
                    if !proxy::can_send_udp_directly().await {
                        crate::notify_warning(
                            "This computer would not let the launcher send UDP, so \
                             whether the proxy relays it could not be checked. The \
                             browser is started with UDP left on — if it is a VPN \
                             doing this, the browser may well be allowed where the \
                             launcher is not.",
                        );
                        eprintln!(
                            "[launcher] UDP probe failed for proxy {} ({e}), and this \
                             process cannot send UDP at all — leaving UDP enabled",
                            p.host
                        );
                        true
                    } else {
                        let cached = proxy::latest_test(&p.id).and_then(|s| s.udp_ms).is_some();
                        eprintln!(
                            "[launcher] UDP probe failed for proxy {} ({e}) while this \
                             process CAN send UDP, so the proxy has no relay; cached={cached}",
                            p.host
                        );
                        cached
                    }
                }
            }
        } else {
            false
        }
    } else {
        false
    };

    // Strip `_meta` wrapper and resolve "auto" sentinels before serialising.
    let mut raw = stored.config.clone();
    raw.remove("_meta");
    resolve_auto_fields(&mut raw, bound_proxy.as_ref()).await;
    // A profile made on another machine carries that machine's screen; on Win/Linux
    // the window has to fit this monitor. "real" mode skips — the core drops it anyway.
    if settings::load()?.screen_resolution_mode.as_deref() != Some("real") {
        if let Some(w) = crate::main_window() {
            crate::clamp_screen_to_real_display(&w, &mut raw);
        }
    }
    let json = serde_json::to_string(&raw).context("serialize profile")?;

    // Pass fingerprint by file path — inline JSON overflows Windows' 32767-char CreateProcess limit.
    let fp_file = udd.join("fingerprint.json");
    std::fs::write(&fp_file, &json).context("write fingerprint.json")?;

    // Keep whatever CDM an engine has already fetched for itself, then hand it
    // to this profile. The first profile to open a DRM page downloads one; every
    // profile after that starts with it in place.
    if let Err(e) = harvest_widevine(&udd) {
        eprintln!("[launcher] widevine harvest skipped: {e}");
    }
    if let Err(e) = install_widevine(&udd) {
        eprintln!("[launcher] widevine pre-warm skipped: {e}");
    }

    let mut cmd = tokio::process::Command::new(&bin);
    cmd.arg(format!("--fingerprint-profile={}", fp_file.display()));
    cmd.arg(format!("--user-data-dir={}", udd.display()));

    // Per-profile window icon. A failure here is cosmetic, never fatal.
    let color = stored.meta.color.clone().filter(|c| !c.trim().is_empty());
    match crate::runtime::runtime_dir().and_then(|dir| {
        // Display name lives in the config, not in _meta; id as fallback.
        let name = stored
            .config
            .get("name")
            .and_then(|v| v.as_str())
            .filter(|s| !s.trim().is_empty())
            .unwrap_or(profile_id);
        crate::profile_icon::ensure_icon(&dir, name, color.as_deref(), stored.meta.icon_png.as_deref())
    }) {
        Ok(path) => {
            cmd.arg(format!("--shardx-profile-icon={}", path.display()));
        }
        Err(e) => {
            eprintln!("[launcher] profile icon unavailable: {e:#}");
        }
    }
    // Same accent behind the profile-name pill in the omnibox, so the icon and
    // the window never disagree about a profile's colour. Left off for "auto":
    // the pill then keeps the toolbar colour, which is the browser's own
    // default and follows the theme.
    if let Some(c) = color.as_deref() {
        cmd.arg(format!("--shardx-profile-pill-color={c}"));
    }
    cmd.arg("--no-first-run");

    // Extensions from the library. Chromium loads only what
    // --disable-extensions-except allows, so the two lists have to match.
    let ext_paths: Vec<String> = stored
        .meta
        .extensions
        .iter()
        .filter_map(|id| extensions::load_path(id))
        .map(|p| p.display().to_string())
        .collect();
    if !ext_paths.is_empty() {
        let joined = ext_paths.join(",");
        cmd.arg(format!("--disable-extensions-except={joined}"));
        cmd.arg(format!("--load-extension={joined}"));
    }

    // Folder bookmarks, written before the browser reads the file.
    match bookmarks::apply_to_profile(&udd, &stored.meta.folder) {
        Ok(n) if n > 0 => eprintln!("[launcher] {n} folder bookmark(s) applied"),
        Ok(_) => {}
        Err(e) => eprintln!("[launcher] bookmarks skipped: {e}"),
    }

    // Disable WebGPU when profile omits `webgpu` (matches real Linux Chrome).
    let webgpu_present = raw
        .get("webgpu")
        .map(|v| !v.is_null())
        .unwrap_or(false);
    if !webgpu_present {
        cmd.arg("--disable-features=WebGPU");
    }

    // Interactive launches: suppress the crash bubble. Restoring the session is
    // the browser's own "On startup" setting — forcing it here overrode it.
    if !headless && !enable_cdp {
        cmd.arg("--hide-crash-restore-bubble");
    }

    if let Some(p) = bound_proxy.as_ref() {
        cmd.arg(format!("--proxy-server={}", p.to_proxy_server_arg()));

        // QUIC: enable only when proxy UDP relay verified; rely on Alt-Svc upgrade path.
        if proxy_udp_ok {
            cmd.arg("--enable-quic");
            eprintln!(
                "[launcher] QUIC enabled (Alt-Svc upgrade path): proxy {} UDP relay verified",
                p.host
            );
        } else {
            cmd.arg("--disable-quic");
            eprintln!("[launcher] QUIC disabled: proxy {} has no working UDP relay", p.host);
        }
    }

    // WebRTC IP policy: block / tcp_only / auto (auto = relay if UDP, else tcp_only).
    let webrtc_mode = raw
        .get("webrtc")
        .and_then(|v| v.as_str())
        .unwrap_or("auto");
    let latest = bound_proxy
        .as_ref()
        .and_then(|p| proxy::latest_test(&p.id));
    // Live geo for ICE-candidate spoofing, cached snapshot as fallback.
    let proxy_public_ip: Option<String> = if let Some(p) = bound_proxy.as_ref() {
        match proxy::geo_check(p, None).await {
            Ok(g) if !g.ip.is_empty() => Some(g.ip),
            _ => latest.as_ref().map(|s| s.ip.clone()).filter(|ip| !ip.is_empty()),
        }
    } else {
        None
    };
    match webrtc_mode {
        "block" => {
            cmd.arg("--force-webrtc-ip-handling-policy=disable_non_proxied_udp");
            cmd.arg("--shardx-webrtc-policy=block");
            eprintln!("[launcher] WebRTC blocked (servers stripped, relay-only, UDP off)");
        }
        "tcp_only" => {
            cmd.arg("--force-webrtc-ip-handling-policy=disable_non_proxied_udp");
            cmd.arg("--shardx-webrtc-policy=tcp_only");
            if let Some(ip) = proxy_public_ip.as_deref() {
                cmd.arg(format!("--shardx-webrtc-public-ip={ip}"));
            }
            eprintln!("[launcher] WebRTC: TCP-only (servers stripped, mDNS host only, UDP off)");
        }
        _ => {
            if bound_proxy.is_none() {
                // No proxy bound — let WebRTC use the host network natively
                // (real IP shows in ICE candidates, which is what the user wants
                // when they explicitly didn't bind a proxy).
                eprintln!("[launcher] WebRTC auto -> native (no proxy bound)");
            } else if !proxy_udp_ok {
                cmd.arg("--force-webrtc-ip-handling-policy=disable_non_proxied_udp");
                cmd.arg("--shardx-webrtc-policy=tcp_only");
                if let Some(ip) = proxy_public_ip.as_deref() {
                    cmd.arg(format!("--shardx-webrtc-public-ip={ip}"));
                }
                eprintln!("[launcher] WebRTC auto -> TCP-only (no proxied UDP available)");
            } else {
                eprintln!("[launcher] WebRTC auto -> through proxy UDP relay");
            }
        }
    }

    // Screen resolution mode: presence-only switch to use host monitor.
    let s = settings::load()?;
    if s.screen_resolution_mode.as_deref() == Some("real") {
        cmd.arg("--shardx-real-screen");
    }

    // The bus is the process's link to the launcher, not the group's — the page
    // helper reports over it on launches that belong to no group at all.
    if bus_port != 0 {
        cmd.arg(format!("--shardx-bus=127.0.0.1:{bus_port}"));
        cmd.arg(format!("--shardx-bus-token={bus_token}"));
        cmd.arg(format!("--shardx-sync-profile={profile_id}"));
    }
    if let Some(group) = sync_group {
        cmd.arg(format!("--shardx-sync-group={group}"));
    }
    if s.helper_enabled {
        // In a group too: the fill travels as a command, not as input, so each
        // window fills with its own generated person.
        cmd.arg("--shardx-helper");
    }
    if s.camera_enabled {
        // No value — the picture is chosen in the running browser. Without the
        // switch the machine's own camera answers.
        cmd.arg("--shardx-camera");
    }

    // CDP: port=0 makes Chrome pick free port and write DevToolsActivePort.
    if enable_cdp {
        let _ = std::fs::remove_file(udd.join("DevToolsActivePort"));
        cmd.arg("--remote-debugging-port=0");
        cmd.arg("--remote-allow-origins=*");
        // Gates the Motion domain, which every automated click and keystroke goes
        // through. Free when unused: it is absent from Schema.getDomains and /json/protocol.
        cmd.arg("--shardx-automation");
    }

    if headless {
        cmd.arg("--headless=new");
    }

    // Pin X11 (XWayland under Wayland): a Wayland client may not place its own windows,
    // so SetBounds moves nothing. Not without DISPLAY — forcing x11 then opens no window.
    #[cfg(target_os = "linux")]
    {
        let chosen = settings::parse_extra_args(&s.extra_args)
            .iter()
            .any(|a| a.starts_with("--ozone-platform"));
        if !chosen && std::env::var_os("DISPLAY").is_some() {
            cmd.arg("--ozone-platform=x11");
        }
    }

    // Answer media questions the Android way. Gated on the profile claiming a phone
    // as well as on the setting: the flag does nothing in the engine on a desktop one.
    if stored.meta.android_media && profile::claims_mobile(&stored.config) {
        cmd.arg("--shardx-android-media");
    }

    // Operator's own switches, last so they win a repeat.
    for a in settings::parse_extra_args(&s.extra_args) {
        cmd.arg(a);
    }

    cmd.stdout(Stdio::null()).stderr(Stdio::null());
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        // 0x08000000 = CREATE_NO_WINDOW — suppress the brief console flash
        // when a Tauri GUI app spawns the engine binary.
        cmd.creation_flags(0x08000000);
    }
    let child = cmd.spawn().context("spawn ShardX")?;
    let pid = Tracker::shared().track(profile_id.to_string(), child, stored.meta.temporary);

    profile::touch_launched(profile_id, None)?;

    let mut cdp_error = None;
    let cdp = if enable_cdp {
        match read_devtools_endpoint(&udd).await {
            Some(c) => {
                eprintln!("[launcher] CDP ready for {profile_id}: {}", c.web_socket_debugger_url);
                Tracker::shared().set_cdp(profile_id, c.clone());
                Some(c)
            }
            None => {
                let msg = format!(
                    "the browser did not report a debugging port within {}s; \
                     read DevToolsActivePort in the profile's user-data dir, \
                     or ask this endpoint again",
                    CDP_WAIT.as_secs()
                );
                eprintln!("[launcher] CDP: {msg}");
                cdp_error = Some(msg);
                None
            }
        }
    } else {
        None
    };

    Ok(LaunchOutcome { pid, cdp, cdp_error })
}

/// How long a launch waits for the browser to publish its debugging port. Six
/// seconds was not enough for a cold start on Windows with a large profile.
const CDP_WAIT: std::time::Duration = std::time::Duration::from_secs(30);

/// Poll `<udd>/DevToolsActivePort`; line 1 = port, line 2 = ws path.
async fn read_devtools_endpoint(udd: &Path) -> Option<process::CdpInfo> {
    let file = udd.join("DevToolsActivePort");
    let deadline = std::time::Instant::now() + CDP_WAIT;
    while std::time::Instant::now() < deadline {
        if let Ok(txt) = std::fs::read_to_string(&file) {
            let mut lines = txt.lines();
            if let (Some(port_s), Some(path)) = (lines.next(), lines.next()) {
                if let Ok(port) = port_s.trim().parse::<u16>() {
                    return Some(process::CdpInfo {
                        port,
                        http_url: format!("http://127.0.0.1:{port}"),
                        web_socket_debugger_url: format!(
                            "ws://127.0.0.1:{port}{}",
                            path.trim()
                        ),
                    });
                }
            }
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    None
}

/// Resolve "auto" sentinels in profile JSON; with proxy: live → cached → country tag → host warn.
async fn resolve_auto_fields(
    cfg: &mut serde_json::Map<String, serde_json::Value>,
    proxy_opt: Option<&proxy::ProxyEntry>,
) {
    let want_tz_auto = cfg.get("timezone").and_then(|v| v.as_str()) == Some("auto");
    let want_lang_auto = cfg
        .get("navigator")
        .and_then(|n| n.get("language"))
        .and_then(|v| v.as_str())
        == Some("auto");
    let want_geo_auto = matches!(
        cfg.get("geolocation").and_then(|g| g.get("mode")).and_then(|v| v.as_str()),
        Some("auto")
    );

    if !(want_tz_auto || want_lang_auto || want_geo_auto) {
        return;
    }

    eprintln!(
        "[launcher] resolving auto fields (tz={} lang={} geo={} proxy={})",
        want_tz_auto,
        want_lang_auto,
        want_geo_auto,
        proxy_opt.map(|p| format!("{}:{}", p.host, p.port)).unwrap_or_else(|| "(direct)".into()),
    );

    // ---- geo source ----
    let mut source = "";
    let geo: Option<proxy::GeoInfo> = match proxy_opt {
        Some(p) => {
            match proxy::geo_check_via(Some(p), None).await {
                Ok(g) => { source = "proxy-live"; Some(g) }
                Err(e) => {
                    eprintln!("[launcher] proxy geo failed: {e} — falling back to cached snapshot");
                    if let Some(snap) = proxy::latest_test(&p.id) {
                        if !snap.country_code.is_empty() || !snap.timezone.is_empty() {
                            source = "cached-snapshot";
                            Some(proxy::GeoInfo {
                                ip: snap.ip,
                                country: snap.country,
                                country_code: snap.country_code,
                                region: snap.region,
                                city: snap.city,
                                isp: snap.isp,
                                timezone: snap.timezone,
                                latitude: snap.latitude,
                                longitude: snap.longitude,
                                provider: snap.provider,
                            })
                        } else { None }
                    } else { None }
                    .or_else(|| {
                        if !p.country.is_empty() {
                            source = "country-tag";
                            Some(proxy::GeoInfo {
                                ip: String::new(),
                                country: String::new(),
                                country_code: p.country.clone(),
                                region: String::new(),
                                city: String::new(),
                                isp: String::new(),
                                timezone: String::new(),
                                latitude: 0.0,
                                longitude: 0.0,
                                provider: String::new(),
                            })
                        } else { None }
                    })
                }
            }
        }
        None => {
            match proxy::geo_check_via(None, None).await {
                Ok(g) => { source = "direct-live"; Some(g) }
                Err(e) => {
                    eprintln!("[launcher] direct geo failed: {e} — falling back to host TZ/locale");
                    None
                }
            }
        }
    };

    let host_warn = || {
        if proxy_opt.is_some() {
            crate::notify_warning(
                "Could not read the proxy's location. Rather than hand a page \
                 this computer's timezone, the profile starts on UTC — which \
                 few people really keep. Test the proxy again, or set the \
                 timezone on the profile yourself.",
            );
        }
    };

    // ---- concrete tz/locale/lat/lng ----
    let (resolved_tz, resolved_locale, resolved_lat, resolved_lng) = match geo {
        Some(ref g) => {
            let tz = if !g.timezone.is_empty() {
                g.timezone.clone()
            } else {
                proxy::country_to_timezone(&g.country_code).to_string()
            };
            let locale = proxy::country_to_locale(&g.country_code).to_string();
            let lat = if g.latitude != 0.0 { Some(g.latitude) } else { None };
            let lng = if g.longitude != 0.0 { Some(g.longitude) } else { None };
            (tz, locale, lat, lng)
        }
        None => {
            host_warn();
            if proxy_opt.is_some() {
                // A profile behind a proxy must never answer with this
                // computer's clock. Before, the Windows branch had no way to
                // read the host zone and landed on UTC by accident; now that
                // it can read it, handing it over would be a real leak of
                // where the operator is. UTC is wrong too, but it is not
                // anybody's address.
                ("UTC".into(), "en-US".into(), None, None)
            } else {
                (
                    host_timezone().unwrap_or_else(|| "UTC".into()),
                    host_locale().unwrap_or_else(|| "en-US".into()),
                    None,
                    None,
                )
            }
        }
    };

    eprintln!(
        "[launcher] resolved tz={resolved_tz} locale={resolved_locale} (source={source})"
    );

    if want_tz_auto {
        cfg.insert("timezone".into(), serde_json::Value::String(resolved_tz.clone()));
    }

    if want_lang_auto {
        let base = resolved_locale.split('-').next().unwrap_or(&resolved_locale).to_string();
        let accept = if resolved_locale == "en-US" {
            "en-US,en;q=0.9".to_string()
        } else {
            format!("{resolved_locale},{base};q=0.9,en-US;q=0.8,en;q=0.7")
        };
        let languages = if resolved_locale == "en-US" {
            vec![
                serde_json::Value::String("en-US".into()),
                serde_json::Value::String("en".into()),
            ]
        } else {
            vec![
                serde_json::Value::String(resolved_locale.clone()),
                serde_json::Value::String(base),
                serde_json::Value::String("en-US".into()),
                serde_json::Value::String("en".into()),
            ]
        };
        if let Some(nav) = cfg.get_mut("navigator").and_then(|v| v.as_object_mut()) {
            nav.insert("language".into(), serde_json::Value::String(resolved_locale.clone()));
            nav.insert("accept_language".into(), serde_json::Value::String(accept));
            nav.insert("languages".into(), serde_json::Value::Array(languages));
        }
        // Always overwrite icu_locale so it matches resolved navigator.language.
        cfg.insert("icu_locale".into(), serde_json::Value::String(resolved_locale));
    }

    if want_geo_auto {
        if let (Some(lat), Some(lng)) = (resolved_lat, resolved_lng) {
            cfg.insert(
                "geolocation".into(),
                serde_json::json!({
                    "mode": "manual",
                    "latitude": lat,
                    "longitude": lng,
                    "accuracy": 50.0,
                }),
            );
        } else {
            cfg.remove("geolocation");
        }
    }
}

/// Version string as numbers, so "4.10.2891.0" sorts above "4.9.9999.0".
fn cdm_version(v: &str) -> Vec<u64> {
    v.split('.').map(|p| p.parse().unwrap_or(0)).collect()
}

fn cdm_version_of(dir: &Path) -> Option<String> {
    let text = std::fs::read_to_string(dir.join("manifest.json")).ok()?;
    let manifest: serde_json::Value = serde_json::from_str(&text).ok()?;
    manifest.get("version")?.as_str().map(String::from)
}

/// Take the CDM the engine downloaded for itself into the cache, so the next
/// profile does not have to download its own. The engine's component updater is
/// the only thing that writes `<udd>/WidevineCdm/<version>/`, and it keeps that
/// copy current — which is why the CDM is not shipped from the CDN at all: its
/// version would have to be chased there forever.
fn harvest_widevine(udd: &Path) -> Result<()> {
    let root = udd.join("WidevineCdm");
    if !root.exists() {
        return Ok(());
    }
    let mut newest: Option<(Vec<u64>, String, PathBuf)> = None;
    for entry in std::fs::read_dir(&root)?.flatten() {
        let p = entry.path();
        if !p.is_dir() {
            continue;
        }
        let Some(version) = cdm_version_of(&p) else { continue };
        let parsed = cdm_version(&version);
        if newest.as_ref().is_none_or(|(best, _, _)| parsed > *best) {
            newest = Some((parsed, version, p));
        }
    }
    let Some((found, version, src)) = newest else { return Ok(()) };

    let cache = store::widevine_cache_dir()?;
    if let Some(have) = cdm_version_of(&cache) {
        if cdm_version(&have) >= found {
            return Ok(());
        }
    }
    // Into a sibling first: a half-copied cache is worse than none, because
    // install_widevine reads it without knowing it is unfinished.
    let staging = cache.with_extension("incoming");
    let _ = std::fs::remove_dir_all(&staging);
    copy_dir_recursive(&src, &staging)?;
    let _ = std::fs::remove_dir_all(&cache);
    std::fs::rename(&staging, &cache)?;
    eprintln!("[launcher] widevine cached from a profile: {version}");
    Ok(())
}

/// Copy cached Widevine CDM into `<udd>/WidevineCdm/<version>/` (versioned layout
/// required by Chromium's DefaultComponentInstaller). No-op if cache absent.
fn install_widevine(udd: &Path) -> Result<()> {
    let src = store::widevine_cache_dir()?;
    if !src.exists() {
        anyhow::bail!("cache dir absent ({})", src.display());
    }
    let manifest_path = src.join("manifest.json");
    if !manifest_path.exists() {
        anyhow::bail!("cache missing manifest.json — re-seed from a real Chrome");
    }
    let manifest_text = std::fs::read_to_string(&manifest_path)?;
    let manifest: serde_json::Value = serde_json::from_str(&manifest_text)
        .context("parse widevine manifest.json")?;
    let version = manifest
        .get("version")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("widevine manifest missing `version`"))?;

    let widevine_root = udd.join("WidevineCdm");
    let versioned = widevine_root.join(version);
    if versioned.exists() {
        return Ok(());
    }
    // Clean up any stale flat layout from older launcher versions.
    let flat_manifest = widevine_root.join("manifest.json");
    if flat_manifest.exists() {
        for stray in ["manifest.json", "LICENSE", "_platform_specific"] {
            let p = widevine_root.join(stray);
            if p.is_dir() {
                let _ = std::fs::remove_dir_all(&p);
            } else if p.exists() {
                let _ = std::fs::remove_file(&p);
            }
        }
    }
    copy_dir_recursive(&src, &versioned).with_context(|| {
        format!("copy {} → {}", src.display(), versioned.display())
    })?;
    // Chromium reads this single-line marker on startup.
    std::fs::write(
        widevine_root.join("latest-component-updated-version"),
        version,
    )?;
    eprintln!("[launcher] widevine pre-warmed: {}", versioned.display());
    Ok(())
}

fn copy_dir_recursive(src: &Path, dst: &Path) -> Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let from = entry.path();
        let to = dst.join(entry.file_name());
        let ty = entry.file_type()?;
        if ty.is_dir() {
            copy_dir_recursive(&from, &to)?;
        } else if ty.is_symlink() {
            // Resolve symlinks so dst tree stays portable across hosts.
            let target = std::fs::read_link(&from)?;
            let resolved = if target.is_absolute() { target } else { from.parent().unwrap().join(target) };
            if resolved.is_dir() {
                copy_dir_recursive(&resolved, &to)?;
            } else {
                std::fs::copy(&resolved, &to)?;
            }
        } else {
            std::fs::copy(&from, &to)?;
        }
    }
    Ok(())
}

/// Read host TZ from /etc/localtime symlink, fall back to $TZ.
fn host_timezone() -> Option<String> {
    if let Ok(target) = std::fs::read_link("/etc/localtime") {
        let path = target.to_string_lossy().into_owned();
        for prefix in ["/usr/share/zoneinfo/", "/var/db/timezone/zoneinfo/"] {
            if let Some(tz) = path.strip_prefix(prefix) {
                return Some(tz.to_string());
            }
        }
    }
    if let Ok(tz) = std::env::var("TZ") {
        if !tz.is_empty() {
            return Some(tz);
        }
    }
    // Windows has neither of the above, and answering "UTC" there put every
    // profile on a clock almost nobody really keeps.
    iana_time_zone::get_timezone().ok().filter(|s| !s.is_empty())
}

/// Extract BCP-47 locale from $LANG/$LC_ALL ("en_US.UTF-8" → "en-US").
fn host_locale() -> Option<String> {
    for var in ["LANG", "LC_ALL", "LC_MESSAGES"] {
        if let Ok(v) = std::env::var(var) {
            let stripped = v.split('.').next().unwrap_or("").replace('_', "-");
            if stripped.contains('-') {
                return Some(stripped);
            }
        }
    }
    None
}
