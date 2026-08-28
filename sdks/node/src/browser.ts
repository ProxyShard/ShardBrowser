// Browser launch + lifecycle. Spawns the ShardX engine with the same
// spoofing flags the desktop launcher uses, plus pre-launch:
//
//   • resolveAutoFields    — fill timezone/language/geolocation from a
//     live geo lookup through the bound proxy.
//   • applyScreenStrategy — cap to host monitor (macOS) or replace with
//     the host monitor (Win/Linux), matching the launcher's
//     `clamp_screen_to_real_display` / `--shardx-real-screen` switch.
//   • probeUdp             — decide QUIC + WebRTC policy from a live
//     SOCKS5 UDP_ASSOCIATE probe.
import { spawn, type ChildProcess } from "node:child_process";
import { existsSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { join } from "node:path";

import { hasAutoFields, resolveAutoFields } from "./autoResolve.js";
import { geoCheckVia, type GeoInfo } from "./geo.js";
import { Profile, userDataDir, applyEngineVersion } from "./profile.js";
import { parseProxy, probeUdp, proxyToArg, type ParsedProxy } from "./proxy.js";
import type { Runtime } from "./runtime.js";
import { applyScreenStrategy, defaultScreenModeFor, type ScreenStrategy } from "./screen.js";

export type WebRtcMode   = "auto" | "block" | "tcp_only";
/** Legacy alias retained for back-compat; prefer `ScreenStrategy`. */
export type ScreenMode   = ScreenStrategy;

const noiseDefault = (): Record<string, Record<string, number | boolean>> => ({
  canvas:       { enabled: false, seed: 0 },
  webgl:        { enabled: false, seed: 0, intensity: 0 },
  audio:        { enabled: false, seed: 0 },
  client_rects: { enabled: false, seed: 0, max_offset: 0 },
  sensors:      { enabled: false, seed: 0 },
  fonts:        { enabled: false, seed: 0 },
});

/** Deterministic non-zero 32-bit FNV-1a of `<id>::<slot>`. */
function noiseSeed(id: string, slot: string): number {
  let h = 2166136261;
  const s = `${id}::${slot}`;
  for (let i = 0; i < s.length; i++) h = Math.imul(h ^ s.charCodeAt(i), 16777619);
  h >>>= 0;
  return h === 0 ? 1 : h;
}

/** Add the default noise block when absent, then fill any seed-0 vector with a
 *  stable per-profile value — without it every profile shares seed 0 and gets
 *  an identical canvas/audio/WebGL fingerprint. */
function applyNoiseSeeds(config: Record<string, unknown>, id: string): void {
  let noise = config["noise"] as Record<string, Record<string, unknown>> | undefined;
  if (!noise || typeof noise !== "object") {
    noise = noiseDefault();
    config["noise"] = noise;
  }
  for (const slot of Object.keys(noise)) {
    const block = noise[slot];
    if (block && typeof block === "object" && !block["seed"]) {
      block["seed"] = noiseSeed(id, slot);
    }
  }
}

export interface LaunchOptions {
  proxy?: string;
  cdp?: boolean;
  headless?: boolean;
  extraArgs?: string[];
  env?: Record<string, string>;
  webrtc?: WebRtcMode;
  webrtcPublicIp?: string;
  /** Override the UDP-probe auto-decision. */
  quic?: boolean;
  /** Defaults to "cap_to_host" on macOS, "use_host" on Win/Linux. */
  screenMode?: ScreenStrategy;
  probeTimeoutMs?: number;
  /** Custom user-data-dir root. Defaults to ./shardx-profiles/<id>/. */
  userDataDir?: string;
}

export class BrowserSession {
  private _stopped = false;

  constructor(
    readonly pid: number,
    readonly userDataDir: string,
    readonly cdpUrl: string | null,
    readonly process: ChildProcess,
    readonly proxyUdpMs: number | null = null,
    readonly quicEnabled: boolean = false,
    readonly webrtcMode: WebRtcMode = "auto",
    readonly geo: GeoInfo | null = null,
  ) {}

  async stop(timeoutMs = 5000): Promise<void> {
    if (this._stopped) return;
    this._stopped = true;
    if (!this.process.pid) return;
    try { this.process.kill("SIGTERM"); } catch { /* already gone */ }
    const exited = await new Promise<boolean>((resolve) => {
      const t = setTimeout(() => resolve(false), timeoutMs);
      this.process.once("exit", () => { clearTimeout(t); resolve(true); });
    });
    if (!exited) {
      try { this.process.kill("SIGKILL"); } catch { /* ignore */ }
    }
  }
}

export class Browser {
  constructor(private readonly runtime: Runtime) {}

  async launch(profile: Profile, opts: LaunchOptions = {}): Promise<BrowserSession> {
    // Auto-install on first use (high-level ShardX.launch already does
    // this; the call is here too so low-level Browser.launch users
    // don't have to remember).
    await this.runtime.install();

    const parsed: ParsedProxy | null = opts.proxy ? parseProxy(opts.proxy) : null;

    // ---- pre-launch: auto-resolve, screen strategy, UDP probe ------
    let geo: GeoInfo | null = null;
    if (hasAutoFields(profile.config)) {
      geo = await resolveAutoFields(profile.config, parsed);
    }

    const mode: ScreenStrategy = opts.screenMode ?? defaultScreenModeFor(profile.platform);
    applyScreenStrategy(profile.config, mode);

    let proxyUdpMs: number | null = null;
    if (parsed && parsed.scheme === "socks5") {
      proxyUdpMs = await probeUdp(parsed, opts.probeTimeoutMs ?? 6000);
    }
    const udpOk = proxyUdpMs !== null;
    const quicEnabled = opts.quic ?? (parsed !== null && udpOk);
    let webrtcMode: WebRtcMode = opts.webrtc ?? "auto";
    if (webrtcMode === "auto" && parsed !== null && !udpOk) webrtcMode = "tcp_only";

    // ---- profile + udd ----------------------------------------------
    const udd = userDataDir(this.runtime, profile.id, opts.userDataDir);
    console.log(`[shardx] profile '${profile.id}' → ${udd}`);
    // Keep the spoofed Chrome version coherent with the installed engine,
    // regardless of where the profile config came from (library / file / dict).
    applyEngineVersion(profile.config, this.runtime.chromiumVersion, this.runtime.greaseBrand, this.runtime.greaseVersion);
    applyNoiseSeeds(profile.config, profile.id);
    const fpFile = join(udd, "fingerprint.json");
    writeFileSync(fpFile, JSON.stringify(profile.config));

    const argv: string[] = [
      `--fingerprint-profile=${fpFile}`,
      `--user-data-dir=${udd}`,
      "--no-first-run",
    ];
    if (!profile.hasWebGPU) argv.push("--disable-features=WebGPU");
    if (!opts.headless && !opts.cdp) {
      argv.push("--restore-last-session", "--hide-crash-restore-bubble");
    }
    // Engine-side real-screen switch only fires on use_host (where the SDK
    // already rewrote screen.* — keep them in sync with the launcher).
    if (mode === "use_host") argv.push("--shardx-real-screen");
    if (parsed) {
      argv.push(`--proxy-server=${proxyToArg(parsed)}`);
      argv.push(quicEnabled ? "--enable-quic" : "--disable-quic");
    }
    if (webrtcMode === "block") {
      argv.push(
        "--force-webrtc-ip-handling-policy=disable_non_proxied_udp",
        "--shardx-webrtc-policy=block",
      );
    } else if (webrtcMode === "tcp_only") {
      argv.push(
        "--force-webrtc-ip-handling-policy=disable_non_proxied_udp",
        "--shardx-webrtc-policy=tcp_only",
      );
      // Engine spoofs the public side of ICE candidates with this IP.
      // Match the launcher: ALWAYS resolve when proxy is bound — relying
      // on `geo` from auto-resolve only works when the profile has auto
      // sentinels, otherwise the engine falls back to the host IP.
      let ip = opts.webrtcPublicIp ?? geo?.ip;
      if (!ip && parsed) {
        try { ip = (await geoCheckVia(parsed)).ip || undefined; } catch { /* leave undefined */ }
      }
      if (ip) argv.push(`--shardx-webrtc-public-ip=${ip}`);
    }
    const cdpMarker = join(udd, "DevToolsActivePort");
    if (opts.cdp) {
      if (existsSync(cdpMarker)) rmSync(cdpMarker, { force: true });
      argv.push("--remote-debugging-port=0", "--remote-allow-origins=*");
    }
    if (opts.headless) argv.push("--headless=new");
    if (opts.extraArgs) argv.push(...opts.extraArgs);

    const child = spawn(this.runtime.binaryPath, argv, {
      env: { ...process.env, ...(opts.env ?? {}) },
      stdio: "ignore",
      detached: process.platform !== "win32",
    });

    const cdpUrl = opts.cdp ? await readCdpEndpoint(udd, 15_000) : null;
    return new BrowserSession(
      child.pid!, udd, cdpUrl, child, proxyUdpMs, quicEnabled, webrtcMode, geo,
    );
  }
}

async function readCdpEndpoint(udd: string, timeoutMs: number): Promise<string | null> {
  const marker = join(udd, "DevToolsActivePort");
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    if (existsSync(marker)) {
      try {
        const firstLine = readFileSync(marker, "utf8").split("\n")[0].trim();
        const port = parseInt(firstLine, 10);
        if (!Number.isNaN(port)) {
          const r = await fetch(`http://127.0.0.1:${port}/json/version`);
          if (r.ok) {
            const data = await r.json() as { webSocketDebuggerUrl?: string };
            if (data.webSocketDebuggerUrl) return data.webSocketDebuggerUrl;
          }
        }
      } catch { /* keep polling */ }
    }
    await new Promise((r) => setTimeout(r, 100));
  }
  return null;
}
