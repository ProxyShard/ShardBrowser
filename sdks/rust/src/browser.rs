//! Browser launch + lifecycle. Spawns the ShardX engine with the same
//! spoofing flags the desktop launcher uses, plus pre-launch auto-resolve,
//! screen strategy, and UDP probe.

use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;

use crate::auto_resolve::{has_auto_fields, resolve_auto_fields};
use crate::geo::{geo_check_via, GeoInfo};
use crate::profile::{user_data_dir, Profile};
use crate::proxy::{parse_proxy, probe_udp, proxy_to_arg, ParsedProxy, ProxyScheme};
use crate::runtime::Runtime;
use crate::screen::{apply_screen_strategy, default_screen_mode_for, ScreenStrategy};

/// Deterministic non-zero 32-bit FNV-1a of `<id>::<slot>`.
fn noise_seed(id: &str, slot: &str) -> u32 {
    let mut h: u32 = 2166136261;
    for b in format!("{id}::{slot}").bytes() {
        h = (h ^ b as u32).wrapping_mul(16777619);
    }
    if h == 0 {
        1
    } else {
        h
    }
}

/// Add the default noise block when absent, then fill any seed-0 vector with a
/// stable per-profile value — without it every profile shares seed 0 and gets
/// an identical canvas/audio/WebGL fingerprint.
fn apply_noise_seeds(config: &mut serde_json::Value, id: &str) {
    let Some(obj) = config.as_object_mut() else {
        return;
    };
    obj.entry("noise").or_insert_with(|| {
        serde_json::json!({
            "canvas":       { "enabled": false, "seed": 0 },
            "webgl":        { "enabled": false, "seed": 0, "intensity": 0 },
            "audio":        { "enabled": false, "seed": 0 },
            "client_rects": { "enabled": false, "seed": 0, "max_offset": 0 },
            "sensors":      { "enabled": false, "seed": 0 },
            "fonts":        { "enabled": false, "seed": 0 }
        })
    });
    if let Some(noise) = obj.get_mut("noise").and_then(|n| n.as_object_mut()) {
        for (slot, block) in noise.iter_mut() {
            if let Some(b) = block.as_object_mut() {
                let needs = b
                    .get("seed")
                    .and_then(|v| v.as_u64())
                    .map(|n| n == 0)
                    .unwrap_or(true);
                if needs {
                    b.insert("seed".into(), serde_json::Value::from(noise_seed(id, slot)));
                }
            }
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum WebRtcMode {
    #[default]
    Auto,
    Block,
    TcpOnly,
}

#[derive(Default)]
pub struct LaunchOptions {
    pub proxy: Option<String>,
    pub cdp: bool,
    pub headless: bool,
    pub extra_args: Vec<String>,
    pub env: Vec<(String, String)>,
    pub webrtc: Option<WebRtcMode>,
    pub webrtc_public_ip: Option<String>,
    /// Override the UDP-probe auto-decision for QUIC.
    pub quic: Option<bool>,
    /// Defaults to `CapToHost` on macOS, `UseHost` on Win/Linux.
    pub screen_mode: Option<ScreenStrategy>,
    pub probe_timeout_ms: Option<u64>,
    /// Custom user-data-dir root. Defaults to `<profiles_root>/<id>/`.
    pub user_data_dir: Option<PathBuf>,
    /// When picking a random profile, filter by `navigator.platform` substring.
    pub platform: Option<String>,
    /// Re-pick hardware_concurrency / device_memory / platform_version.
    pub randomize: bool,
}

/// A running engine process + the decisions made at launch.
pub struct BrowserSession {
    pub pid: u32,
    pub user_data_dir: PathBuf,
    pub cdp_url: Option<String>,
    pub proxy_udp_ms: Option<u128>,
    pub quic_enabled: bool,
    pub webrtc_mode: WebRtcMode,
    pub geo: Option<GeoInfo>,
    child: Option<Child>,
    stopped: bool,
}

impl BrowserSession {
    /// Terminate the engine process. Idempotent.
    pub async fn stop(&mut self) -> Result<()> {
        if self.stopped {
            return Ok(());
        }
        self.stopped = true;
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            // Reap so we don't leave a zombie.
            tokio::task::spawn_blocking(move || {
                let _ = child.wait();
            })
            .await
            .ok();
        }
        Ok(())
    }
}

impl Drop for BrowserSession {
    fn drop(&mut self) {
        if !self.stopped {
            if let Some(mut child) = self.child.take() {
                let _ = child.kill();
            }
        }
    }
}

pub struct Browser {
    runtime: Arc<Runtime>,
}

impl Browser {
    pub fn new(runtime: Arc<Runtime>) -> Self {
        Self { runtime }
    }

    pub async fn launch(&self, mut profile: Profile, opts: LaunchOptions) -> Result<BrowserSession> {
        self.runtime.install(false).await?;

        let parsed: Option<ParsedProxy> = match &opts.proxy {
            Some(u) => Some(parse_proxy(u)?),
            None => None,
        };

        // ---- pre-launch: auto-resolve, screen strategy, UDP probe ----
        let mut geo: Option<GeoInfo> = None;
        if has_auto_fields(&profile.config) {
            geo = resolve_auto_fields(&mut profile.config, parsed.as_ref()).await;
        }

        let mode = opts
            .screen_mode
            .unwrap_or_else(|| default_screen_mode_for(&profile.platform()));
        apply_screen_strategy(&mut profile.config, mode);

        let mut proxy_udp_ms: Option<u128> = None;
        if let Some(p) = &parsed {
            if p.scheme == ProxyScheme::Socks5 {
                proxy_udp_ms = probe_udp(p, opts.probe_timeout_ms.unwrap_or(6000)).await.ok();
            }
        }
        let udp_ok = proxy_udp_ms.is_some();
        let quic_enabled = opts.quic.unwrap_or(parsed.is_some() && udp_ok);
        let mut webrtc_mode = opts.webrtc.unwrap_or(WebRtcMode::Auto);
        if webrtc_mode == WebRtcMode::Auto && parsed.is_some() && !udp_ok {
            webrtc_mode = WebRtcMode::TcpOnly;
        }

        // ---- profile + udd ----
        let udd = user_data_dir(&self.runtime, &profile.id, opts.user_data_dir.as_deref())?;
        eprintln!("[shardx] profile '{}' → {}", profile.id, udd.display());
        // Keep the spoofed Chrome version coherent with the installed engine,
        // regardless of where the profile config came from (library / file / value).
        let (grease_brand, grease_version) = self.runtime.grease();
        crate::profile::apply_engine_version(
            &mut profile.config,
            &self.runtime.chromium_version(),
            grease_brand.as_deref(),
            grease_version.as_deref(),
        );
        apply_noise_seeds(&mut profile.config, &profile.id);
        let fp_file = udd.join("fingerprint.json");
        std::fs::write(&fp_file, serde_json::to_string(&profile.config)?)?;

        let mut argv: Vec<String> = vec![
            format!("--fingerprint-profile={}", fp_file.display()),
            format!("--user-data-dir={}", udd.display()),
            "--no-first-run".into(),
        ];
        if !profile.has_webgpu() {
            argv.push("--disable-features=WebGPU".into());
        }
        if !opts.headless && !opts.cdp {
            argv.push("--restore-last-session".into());
            argv.push("--hide-crash-restore-bubble".into());
        }
        if mode == ScreenStrategy::UseHost {
            argv.push("--shardx-real-screen".into());
        }
        if let Some(p) = &parsed {
            argv.push(format!("--proxy-server={}", proxy_to_arg(p)));
            argv.push(if quic_enabled { "--enable-quic" } else { "--disable-quic" }.into());
        }
        match webrtc_mode {
            WebRtcMode::Block => {
                argv.push("--force-webrtc-ip-handling-policy=disable_non_proxied_udp".into());
                argv.push("--shardx-webrtc-policy=block".into());
            }
            WebRtcMode::TcpOnly => {
                argv.push("--force-webrtc-ip-handling-policy=disable_non_proxied_udp".into());
                argv.push("--shardx-webrtc-policy=tcp_only".into());
                // Engine spoofs the public side of ICE candidates with this IP.
                let mut ip = opts
                    .webrtc_public_ip
                    .clone()
                    .or_else(|| geo.as_ref().map(|g| g.ip.clone()).filter(|s| !s.is_empty()));
                if ip.is_none() {
                    if let Some(p) = &parsed {
                        if let Ok(g) = geo_check_via(Some(p), "ip-api.com").await {
                            if !g.ip.is_empty() {
                                ip = Some(g.ip);
                            }
                        }
                    }
                }
                if let Some(ip) = ip {
                    argv.push(format!("--shardx-webrtc-public-ip={ip}"));
                }
            }
            WebRtcMode::Auto => {}
        }
        if opts.cdp {
            let marker = udd.join("DevToolsActivePort");
            let _ = std::fs::remove_file(&marker);
            argv.push("--remote-debugging-port=0".into());
            argv.push("--remote-allow-origins=*".into());
        }
        if opts.headless {
            argv.push("--headless=new".into());
        }
        argv.extend(opts.extra_args.iter().cloned());

        let mut cmd = Command::new(self.runtime.binary_path());
        cmd.args(&argv)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        for (k, v) in &opts.env {
            cmd.env(k, v);
        }
        let child = cmd.spawn()?;
        let pid = child.id();

        let cdp_url = if opts.cdp {
            read_cdp_endpoint(&udd, 15_000).await
        } else {
            None
        };

        Ok(BrowserSession {
            pid,
            user_data_dir: udd,
            cdp_url,
            proxy_udp_ms,
            quic_enabled,
            webrtc_mode,
            geo,
            child: Some(child),
            stopped: false,
        })
    }
}

async fn read_cdp_endpoint(udd: &std::path::Path, timeout_ms: u64) -> Option<String> {
    let marker = udd.join("DevToolsActivePort");
    let deadline = tokio::time::Instant::now() + Duration::from_millis(timeout_ms);
    let client = reqwest::Client::new();
    while tokio::time::Instant::now() < deadline {
        if let Ok(text) = std::fs::read_to_string(&marker) {
            if let Some(first) = text.lines().next() {
                if let Ok(port) = first.trim().parse::<u16>() {
                    if let Ok(resp) = client
                        .get(format!("http://127.0.0.1:{port}/json/version"))
                        .send()
                        .await
                    {
                        if resp.status().is_success() {
                            if let Ok(v) = resp.json::<serde_json::Value>().await {
                                if let Some(ws) =
                                    v.get("webSocketDebuggerUrl").and_then(|x| x.as_str())
                                {
                                    return Some(ws.to_string());
                                }
                            }
                        }
                    }
                }
            }
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    None
}
