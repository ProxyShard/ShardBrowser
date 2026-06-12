import { useEffect, useLayoutEffect, useMemo, useRef, useState, type ReactNode } from "react";
import { createPortal } from "react-dom";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { open, save as saveDialog } from "@tauri-apps/plugin-dialog";
import { openPath, openUrl } from "@tauri-apps/plugin-opener";
import "./App.css";

// Host OS of the launcher window (never spoofed) — drives default OS tab + titlebar.
function detectHostOs(): "macOS" | "Windows" | "Linux" {
  const ua = navigator.userAgent;
  if (/Windows/i.test(ua)) return "Windows";
  if (/Macintosh|Mac OS X/i.test(ua)) return "macOS";
  if (/Linux|X11|CrOS/i.test(ua)) return "Linux";
  return "macOS";
}
const HOST_OS = detectHostOs();

// OS clipboard via Tauri plugin (webview navigator.clipboard throws).
const clip = {
  write: (text: string) => invoke("clipboard_write", { text }),
  read: () => invoke<string>("clipboard_read"),
};

const readTextFile = (path: string) => invoke<string>("read_text_file", { path });

// Single UTM tag appended to every outbound proxyshard.com link.
const UTM_QS = "utm_source=shardx&utm_medium=referral&utm_campaign=shardx-launcher";
const withUtm = (url: string) => url + (url.includes("?") ? "&" : "?") + UTM_QS;
// ProxyShard dashboard (account, billing, API key).
const DASHBOARD_URL = withUtm("https://dashboard.proxyshard.com/");
// Docs URL behind the proxy UDP/No-UDP pill.
const UDP_DOCS_URL = withUtm("https://docs.proxyshard.com/eng/our-products/about-udp");

// ---- toasts (global queue, auto-expiry; push via toast.ok / toast.err) ----

type ToastItem = { id: number; kind: "ok" | "err" | "info"; text: string };
let toastSeq = 0;
const toastSubs = new Set<(items: ToastItem[]) => void>();
let toastList: ToastItem[] = [];
const pushToast = (kind: ToastItem["kind"], text: string) => {
  const id = ++toastSeq;
  toastList = [...toastList, { id, kind, text }];
  toastSubs.forEach((cb) => cb(toastList));
  setTimeout(() => {
    toastList = toastList.filter((t) => t.id !== id);
    toastSubs.forEach((cb) => cb(toastList));
  }, 5500);
};
const toast = {
  ok: (t: string) => pushToast("ok", t),
  err: (t: string) => pushToast("err", t),
  info: (t: string) => pushToast("info", t),
};

function ToastHost() {
  const [items, setItems] = useState<ToastItem[]>(toastList);
  useEffect(() => {
    toastSubs.add(setItems);
    return () => { toastSubs.delete(setItems); };
  }, []);
  if (items.length === 0) return null;
  return (
    <div className="toast-host">
      {items.map((t) => (
        <div key={t.id} className={`toast toast-${t.kind}`}>
          <span className="toast-icon">{t.kind === "ok" ? "✓" : t.kind === "err" ? "✕" : "ℹ"}</span>
          <span>{t.text}</span>
        </div>
      ))}
    </div>
  );
}

// ---- confirm modal (replaces unreliable native confirm) ----

type ConfirmButton = { label: string; value: any; danger?: boolean; primary?: boolean };
type ConfirmReq = {
  title?: string;
  message: string;
  buttons: ConfirmButton[];
  resolve: (v: any) => void;
};
let confirmSub: ((req: ConfirmReq | null) => void) | null = null;

function confirmModal(opts: {
  title?: string;
  message: string;
  buttons?: ConfirmButton[];
  danger?: boolean;
}): Promise<any> {
  return new Promise((resolve) => {
    const buttons =
      opts.buttons ?? [
        { label: "Cancel", value: false },
        { label: opts.danger ? "Delete" : "OK", value: true, danger: opts.danger, primary: !opts.danger },
      ];
    confirmSub?.({ title: opts.title, message: opts.message, buttons, resolve });
  });
}

function ConfirmHost() {
  const [req, setReq] = useState<ConfirmReq | null>(null);
  useEffect(() => {
    confirmSub = setReq;
    return () => { if (confirmSub === setReq) confirmSub = null; };
  }, []);
  if (!req) return null;
  const done = (v: any) => { req.resolve(v); setReq(null); };
  return (
    <div className="dialog-bg" onClick={() => done(null)}>
      <div className="dialog dialog-confirm" onClick={(e) => e.stopPropagation()}>
        <header className="dialog-head">
          <h2>{req.title ?? "Confirm"}</h2>
          <button className="icon-btn" onClick={() => done(null)}>✕</button>
        </header>
        <div className="dialog-body">
          <p className="confirm-msg">{req.message}</p>
        </div>
        <div className="confirm-actions">
          {req.buttons.map((b, i) => (
            <button
              key={i}
              className={`btn-sm ${b.primary ? "btn-primary" : "btn-ghost"} ${b.danger ? "danger" : ""}`}
              onClick={() => done(b.value)}
            >
              {b.label}
            </button>
          ))}
        </div>
      </div>
    </div>
  );
}

// ---- first-run "star us" prompt ----

const GH_REPO_URL = "https://github.com/ProxyShard/ShardBrowser";

function GithubMark({ size = 18 }: { size?: number }) {
  return (
    <svg width={size} height={size} viewBox="0 0 16 16" fill="currentColor" aria-hidden>
      <path d="M8 0C3.58 0 0 3.58 0 8c0 3.54 2.29 6.53 5.47 7.59.4.07.55-.17.55-.38 0-.19-.01-.82-.01-1.49-2.01.37-2.53-.49-2.69-.94-.09-.23-.48-.94-.82-1.13-.28-.15-.68-.52-.01-.53.63-.01 1.08.58 1.23.82.72 1.21 1.87.87 2.33.66.07-.52.28-.87.51-1.07-1.78-.2-3.64-.89-3.64-3.95 0-.87.31-1.59.82-2.15-.08-.2-.36-1.02.08-2.12 0 0 .67-.21 2.2.82.64-.18 1.32-.27 2-.27.68 0 1.36.09 2 .27 1.53-1.04 2.2-.82 2.2-.82.44 1.1.16 1.92.08 2.12.51.56.82 1.27.82 2.15 0 3.07-1.87 3.75-3.65 3.95.29.25.54.73.54 1.48 0 1.07-.01 1.93-.01 2.2 0 .21.15.46.55.38A8.01 8.01 0 0 0 16 8c0-4.42-3.58-8-8-8z"/>
    </svg>
  );
}

/// One-time GitHub-star prompt shown after the app first loads. Dismissal is
/// remembered in localStorage so it never nags again.
function StarModal() {
  const [show, setShow] = useState(false);
  useEffect(() => {
    if (localStorage.getItem("shardx-star-prompt") === "done") return;
    // Let the UI settle before surfacing the prompt.
    const t = setTimeout(() => setShow(true), 700);
    return () => clearTimeout(t);
  }, []);
  const close = () => {
    localStorage.setItem("shardx-star-prompt", "done");
    setShow(false);
  };
  const star = () => {
    openUrl(GH_REPO_URL).catch(() => {});
    close();
  };
  if (!show) return null;
  return (
    <div className="dialog-bg" onClick={close}>
      <div className="dialog star-dialog" onClick={(e) => e.stopPropagation()}>
        <button className="icon-btn star-close" onClick={close} aria-label="Close">✕</button>
        <div className="star-body">
          <div className="star-badge"><GithubMark size={26} /><span className="star-spark">★</span></div>
          <h2>Enjoying ShardX?</h2>
          <p className="star-text">
            ShardX is provided and supported <strong>completely free</strong>. If it's
            useful to you, dropping a <strong>star on GitHub</strong> is the easiest way to
            support us — and it helps other people find the project.
          </p>
          <div className="star-actions">
            <button className="btn-ghost" onClick={close}>Maybe later</button>
            <button className="btn-primary star-btn" onClick={star}><GithubMark /> Star on GitHub</button>
          </div>
        </div>
      </div>
    </div>
  );
}

// ---- context menu ----

type ContextItem = { label: string; onClick: () => void; danger?: boolean; sep?: boolean };
function useContextMenu() {
  const [menu, setMenu] = useState<{ x: number; y: number; items: ContextItem[] } | null>(null);
  const close = () => setMenu(null);
  useEffect(() => {
    if (!menu) return;
    const dismiss = () => close();
    window.addEventListener("click", dismiss);
    window.addEventListener("scroll", dismiss, true);
    return () => {
      window.removeEventListener("click", dismiss);
      window.removeEventListener("scroll", dismiss, true);
    };
  }, [menu]);
  const open = (e: React.MouseEvent, items: ContextItem[]) => {
    e.preventDefault();
    setMenu({ x: e.clientX, y: e.clientY, items });
  };
  // Clamp menu into viewport post-layout.
  const ref = useRef<HTMLDivElement>(null);
  useLayoutEffect(() => {
    const el = ref.current;
    if (!menu || !el) return;
    const { width, height } = el.getBoundingClientRect();
    const pad = 8;
    let left = menu.x;
    let top = menu.y;
    if (left + width > window.innerWidth - pad) {
      left = Math.max(pad, window.innerWidth - width - pad);
    }
    if (top + height > window.innerHeight - pad) {
      top = Math.max(pad, window.innerHeight - height - pad);
    }
    el.style.left = `${left}px`;
    el.style.top = `${top}px`;
  }, [menu]);
  const node = menu ? (
    <div ref={ref} className="ctx-menu" style={{ left: menu.x, top: menu.y }} onClick={(e) => e.stopPropagation()}>
      {menu.items.map((it, i) =>
        it.sep ? (
          <div key={i} className="ctx-sep" />
        ) : (
          <button
            key={i}
            className={`ctx-item ${it.danger ? "ctx-danger" : ""}`}
            onClick={() => { it.onClick(); close(); }}
          >
            {it.label}
          </button>
        ),
      )}
    </div>
  ) : null;
  return { open, node };
}

// ---- backend types ----

type ProfileMeta = {
  id: string;
  name: string;
  notes: string;
  proxy_id: string | null;
  last_launched_at: string | null;
  created_at: string | null;
  pinned: boolean;
  folder: string;
  /// Cumulative engine uptime in ms across every launch.  Increased when
  /// the engine exits — for the currently-running session add `running[id]`
  /// (Date.now() - sessionStartTs) on top.
  total_runtime_ms: number;
};
type ProxyEntry = {
  id: string;
  name: string;
  kind: "socks5" | "http" | "https";
  host: string;
  port: number;
  username: string;
  password: string;
  country: string;
  notes: string;
};
type Settings = {
  browser_path: string | null;
  theme: string;
  geo_checker?: string | null;
  screen_resolution_mode?: string | null;
  api_enabled?: boolean;
  api_port?: number;
  api_secret?: string;
};
type ApiInfo = {
  enabled: boolean;
  port: number;
  base_url: string;
  token: string;
};
type Section = "browsers" | "proxies" | "proxyshard" | "fingerprints" | "settings";

/// Library fingerprint backing the editor GPU select; payload supplies the coherent base.
type FingerprintEntry = {
  id: string;
  label: string;
  platform: string;
  chrome: string;
  gpu: string;
  tag_color: string;
  builtin: boolean;
  payload: any;
};

// ---- profile form ----

type NoiseMode = "real" | "auto";
type WebRtcMode = "auto" | "tcp_only" | "block";
type GeoMode = "auto" | "manual";

type ProfileForm = {
  id: string;
  name: string;
  notes: string;
  proxy_id: string | null;

  gpu_preset_id: string;
  user_agent: string;
  hardware_concurrency: number;
  device_memory: number;
  /// Sec-CH-UA-Platform-Version override; empty = use donor preset's value.
  platform_version: string;

  timezone: string;
  language: string;

  webrtc: WebRtcMode;
  do_not_track: boolean;

  noise_canvas: NoiseMode;
  noise_webgl: NoiseMode;
  noise_audio: NoiseMode;
  noise_client_rects: NoiseMode;
  noise_sensors: NoiseMode;
  /// Fonts: "real" passes host fonts through; "auto" hides a ~3% per-profile subset.
  noise_fonts: NoiseMode;
  /// TCP ports the browser refuses to connect to (RDP/VNC/TeamViewer/Squid).
  blocked_ports: number[];

  geo_mode: GeoMode;
  geo_lat: number;
  geo_lng: number;
  geo_accuracy: number;

  media_audio_in: number;
  media_audio_out: number;
  media_video_in: number;
};

// Constrained options: stay within values Chrome actually reports.
const MEMORY_OPTIONS = [4, 8, 16, 32];
const CPU_OPTIONS = [2, 4, 6, 8, 10, 12, 14, 16, 20, 24];
const MEDIA_COUNT_OPTIONS = [0, 1, 2, 3];

/// Common remote-control/proxy ports to block from outgoing browser connects.
const DEFAULT_BLOCKED_PORTS = [
  3389, // RDP
  5900, // VNC
  5901, // VNC
  5800, // VNC HTTP
  7070, // RealVNC / RealAudio
  6568, // AnyDesk
  5938, // TeamViewer
  1080, // SOCKS
  8080, // HTTP proxy
  3128, // Squid
  3030, // misc
];

/// "auto" sentinel; the Rust launch resolver replaces with concrete TZ.
const AUTO_TZ = "auto";
const AUTO_LANG = "auto";

const TIMEZONES = [
  AUTO_TZ,
  "America/Chicago", "America/Denver", "America/Los_Angeles", "America/New_York",
  "America/Sao_Paulo", "America/Toronto",
  "Asia/Bangkok", "Asia/Dubai", "Asia/Hong_Kong", "Asia/Jakarta", "Asia/Kolkata",
  "Asia/Seoul", "Asia/Shanghai", "Asia/Singapore", "Asia/Tokyo",
  "Australia/Sydney",
  "Europe/Amsterdam", "Europe/Athens", "Europe/Berlin", "Europe/Bucharest",
  "Europe/Helsinki", "Europe/Istanbul", "Europe/Kyiv", "Europe/Lisbon",
  "Europe/London", "Europe/Madrid", "Europe/Moscow", "Europe/Paris",
  "Europe/Prague", "Europe/Rome", "Europe/Stockholm", "Europe/Warsaw",
  "Europe/Vienna", "Europe/Zurich",
  "Pacific/Auckland", "UTC",
];

const LOCALES: { code: string; label: string }[] = [
  { code: AUTO_LANG, label: "Auto (from proxy geo)" },
  { code: "en-US", label: "English (US)" },
  { code: "en-GB", label: "English (UK)" },
  { code: "en-CA", label: "English (Canada)" },
  { code: "en-AU", label: "English (Australia)" },
  { code: "de-DE", label: "Deutsch (Deutschland)" },
  { code: "es-ES", label: "Español (España)" },
  { code: "es-MX", label: "Español (México)" },
  { code: "fr-FR", label: "Français (France)" },
  { code: "it-IT", label: "Italiano" },
  { code: "nl-NL", label: "Nederlands" },
  { code: "pl-PL", label: "Polski" },
  { code: "pt-BR", label: "Português (Brasil)" },
  { code: "pt-PT", label: "Português (Portugal)" },
  { code: "ro-RO", label: "Română" },
  { code: "ru-RU", label: "Русский" },
  { code: "uk-UA", label: "Українська" },
  { code: "tr-TR", label: "Türkçe" },
  { code: "el-GR", label: "Ελληνικά" },
  { code: "cs-CZ", label: "Čeština" },
  { code: "sv-SE", label: "Svenska" },
  { code: "fi-FI", label: "Suomi" },
  { code: "no-NO", label: "Norsk" },
  { code: "da-DK", label: "Dansk" },
  { code: "hu-HU", label: "Magyar" },
  { code: "zh-CN", label: "中文 (简体)" },
  { code: "zh-TW", label: "中文 (繁體)" },
  { code: "ja-JP", label: "日本語" },
  { code: "ko-KR", label: "한국어" },
  { code: "ar-SA", label: "العربية" },
  { code: "he-IL", label: "עברית" },
  { code: "id-ID", label: "Bahasa Indonesia" },
  { code: "vi-VN", label: "Tiếng Việt" },
  { code: "th-TH", label: "ไทย" },
  { code: "hi-IN", label: "हिन्दी" },
];

/// Build accept-language chain (primary → base → English fallback).
function deriveAcceptLanguage(loc: string): string {
  if (!loc) return "en-US,en;q=0.9";
  const base = loc.split("-")[0];
  if (loc === "en-US") return "en-US,en;q=0.9";
  return `${loc},${base};q=0.9,en-US;q=0.8,en;q=0.7`;
}

function deriveLanguagesArray(loc: string): string[] {
  if (!loc) return ["en-US", "en"];
  const base = loc.split("-")[0];
  if (loc === "en-US") return ["en-US", "en"];
  return [loc, base, "en-US", "en"];
}

const defaultForm = (): ProfileForm => ({
  id: "",
  name: "",
  notes: "",
  proxy_id: null,

  // Empty until snapped to gpusForOs[0] by useEffect.
  gpu_preset_id: "",
  user_agent:
    "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/148.0.0.0 Safari/537.36",
  hardware_concurrency: 8,
  device_memory: 16,
  // Empty = inherit donor; setGpu refreshes via enrich_picks_for_preset.
  platform_version: "",

  timezone: AUTO_TZ,
  language: AUTO_LANG,

  webrtc: "auto",
  do_not_track: false,

  noise_canvas: "real",
  noise_webgl: "real",
  noise_audio: "real",
  noise_client_rects: "real",
  noise_sensors: "real",
  noise_fonts: "real",
  blocked_ports: DEFAULT_BLOCKED_PORTS.slice(),

  geo_mode: "auto",
  geo_lat: 52.2297,
  geo_lng: 21.0122,
  geo_accuracy: 50,

  media_audio_in: 1,
  media_audio_out: 1,
  media_video_in: 1,
});

function fromStored(stored: any): ProfileForm {
  const f = defaultForm();
  if (!stored) return f;
  f.id = stored?._meta?.id ?? "";
  f.proxy_id = stored?._meta?.proxy_id ?? null;
  f.name = stored?.name ?? "";
  f.notes = stored?.notes ?? "";
  // Empty for legacy profiles; snapped by useEffect.
  f.gpu_preset_id = stored?._meta?.gpu_preset_id ?? "";
  f.user_agent = stored?.navigator?.user_agent ?? f.user_agent;
  f.hardware_concurrency = stored?.navigator?.hardware_concurrency ?? 8;
  f.device_memory = stored?.navigator?.device_memory ?? 16;
  f.timezone = stored?.timezone ?? AUTO_TZ;
  f.language = stored?.navigator?.language ?? AUTO_LANG;
  f.webrtc = (stored?.webrtc === "replace" ? "tcp_only" : stored?.webrtc) ?? "auto";
  f.do_not_track = !!stored?.navigator?.do_not_track;

  const noise = stored?.noise ?? {};
  const noiseMode = (n: any): NoiseMode => (n?.enabled ? "auto" : "real");
  f.noise_canvas = noiseMode(noise.canvas);
  f.noise_webgl = noiseMode(noise.webgl);
  f.noise_audio = noiseMode(noise.audio);
  f.noise_client_rects = noiseMode(noise.client_rects);
  f.noise_sensors = noiseMode(noise.sensors);
  // Fonts default OFF (real); mirrors C++ default.
  f.noise_fonts = noiseMode(noise.fonts);
  f.blocked_ports = Array.isArray(stored?.blocked_ports)
    ? stored.blocked_ports.filter((n: any) => typeof n === "number")
    : DEFAULT_BLOCKED_PORTS.slice();

  const geo = stored?.geolocation ?? {};
  f.geo_mode = geo.mode === "manual" ? "manual" : "auto";
  f.geo_lat = typeof geo.latitude === "number" ? geo.latitude : f.geo_lat;
  f.geo_lng = typeof geo.longitude === "number" ? geo.longitude : f.geo_lng;
  f.geo_accuracy = typeof geo.accuracy === "number" ? geo.accuracy : f.geo_accuracy;

  const md = stored?.media_devices ?? {};
  f.media_audio_in = md.audio_input_count ?? 1;
  f.media_audio_out = md.audio_output_count ?? 1;
  f.media_video_in = md.video_input_count ?? 1;

  return f;
}

/// Deterministic per-profile per-slot 32-bit hash (stable noise seeds).
function noiseSeed(profileId: string, slot: string): number {
  const s = `${profileId || "new"}::${slot}`;
  let h = 2166136261;
  for (let i = 0; i < s.length; i++) {
    h ^= s.charCodeAt(i);
    h = Math.imul(h, 16777619);
  }
  return h >>> 0;
}

/// Build on-disk FingerprintConfig from library payload + user-edited fields.
function toStored(f: ProfileForm, lib: FingerprintEntry | null): any {
  const base: any = lib && lib.payload ? JSON.parse(JSON.stringify(lib.payload)) : {};

  base._meta = {
    id: f.id,
    proxy_id: f.proxy_id,
    last_launched_at: null,
    gpu_preset_id: f.gpu_preset_id,
  };
  base.name = f.name || "untitled";
  base.notes = f.notes;
  // "auto" sentinel: resolver replaces at launch; persists across edits.
  base.timezone = f.timezone;
  base.icu_locale = f.language === AUTO_LANG ? null : f.language;
  base.webrtc = f.webrtc;

  base.navigator = {
    ...(base.navigator || {}),
    language: f.language,
    accept_language: f.language === AUTO_LANG ? null : deriveAcceptLanguage(f.language),
    languages: f.language === AUTO_LANG ? null : deriveLanguagesArray(f.language),
    user_agent: f.user_agent,
    hardware_concurrency: f.hardware_concurrency,
    device_memory: f.device_memory,
    // Empty → inherit donor; set → write to both navigator + client_hints.
    ...(f.platform_version ? { platform_version: f.platform_version } : {}),
    do_not_track: f.do_not_track ? "1" : null,
  };
  if (f.platform_version) {
    base.client_hints = {
      ...(base.client_hints || {}),
      platform_version: f.platform_version,
    };
  }

  base.media_devices = {
    audio_input_count: f.media_audio_in,
    audio_output_count: f.media_audio_out,
    video_input_count: f.media_video_in,
  };

  base.geolocation =
    f.geo_mode === "manual"
      ? { mode: "manual", latitude: f.geo_lat, longitude: f.geo_lng, accuracy: f.geo_accuracy }
      : { mode: "auto" };

  base.noise = {
    canvas:       { enabled: f.noise_canvas === "auto",       seed: noiseSeed(f.id, "canvas") },
    webgl:        { enabled: f.noise_webgl === "auto",        seed: noiseSeed(f.id, "webgl"), intensity: f.noise_webgl === "auto" ? 0.0005 : 0 },
    audio:        { enabled: f.noise_audio === "auto",        seed: noiseSeed(f.id, "audio") },
    client_rects: { enabled: f.noise_client_rects === "auto", seed: noiseSeed(f.id, "client_rects"), max_offset: f.noise_client_rects === "auto" ? 1 : 0 },
    sensors:      { enabled: f.noise_sensors === "auto",      seed: noiseSeed(f.id, "sensors") },
    fonts:        { enabled: f.noise_fonts === "auto",        seed: noiseSeed(f.id, "fonts") },
  };
  base.blocked_ports = [...f.blocked_ports].sort((a, b) => a - b);

  return base;
}

// ---- app shell ----

type Theme = "dark" | "light";

export default function App() {
  const [section, setSection] = useState<Section>("browsers");
  const [theme, setTheme] = useState<Theme>(
    () => (localStorage.getItem("shardx-theme") as Theme) || "dark",
  );
  useEffect(() => {
    document.documentElement.setAttribute("data-theme", theme);
    localStorage.setItem("shardx-theme", theme);
  }, [theme]);
  return (
    <>
      {/* Custom title bar; drag-region outside .app stays clickable above modals. */}
      <div
        className={`titlebar ${HOST_OS === "macOS" ? "titlebar-mac" : "titlebar-custom"}`}
        data-tauri-drag-region
      >
        <span className="titlebar-title">ShardX Launcher</span>
        {/* Custom min/max/close on Win/Linux (macOS uses native traffic lights). */}
        {HOST_OS !== "macOS" && (
          <div className="titlebar-controls">
            <button
              className="tb-btn"
              aria-label="Minimize"
              onClick={() => getCurrentWindow().minimize()}
            >
              <svg width="10" height="10" viewBox="0 0 10 10" aria-hidden="true">
                <line x1="1" y1="5" x2="9" y2="5" stroke="currentColor" strokeWidth="1" />
              </svg>
            </button>
            <button
              className="tb-btn"
              aria-label="Maximize"
              onClick={() => getCurrentWindow().toggleMaximize()}
            >
              <svg width="10" height="10" viewBox="0 0 10 10" aria-hidden="true">
                <rect x="1.5" y="1.5" width="7" height="7" fill="none" stroke="currentColor" strokeWidth="1" />
              </svg>
            </button>
            <button
              className="tb-btn tb-close"
              aria-label="Close"
              onClick={() => getCurrentWindow().close()}
            >
              <svg width="10" height="10" viewBox="0 0 10 10" aria-hidden="true">
                <line x1="1" y1="1" x2="9" y2="9" stroke="currentColor" strokeWidth="1" />
                <line x1="9" y1="1" x2="1" y2="9" stroke="currentColor" strokeWidth="1" />
              </svg>
            </button>
          </div>
        )}
      </div>
      <FirstRunGate>
        <div className="app">
          <Sidebar
            section={section}
            onSelect={setSection}
            theme={theme}
            onToggleTheme={() => setTheme((t) => (t === "dark" ? "light" : "dark"))}
          />
          <main className="main">
            {section === "browsers" && <BrowsersView />}
            {section === "proxies" && <ProxiesView />}
            {section === "proxyshard" && <ProxyShardView />}
            {section === "fingerprints" && <FingerprintsView />}
            {section === "settings" && <SettingsView />}
          </main>
          <ToastHost />
          <ConfirmHost />
          <StarModal />
        </div>
      </FirstRunGate>
    </>
  );
}

function Sidebar({
  section, onSelect, theme, onToggleTheme,
}: {
  section: Section;
  onSelect: (s: Section) => void;
  theme: "dark" | "light";
  onToggleTheme: () => void;
}) {
  const sections: { label: string; items: { id: Section; label: string; svg: ReactNode }[] }[] = [
    {
      label: "Workspace",
      items: [
        { id: "browsers", label: "Browsers", svg: <IconShard /> },
        { id: "proxies", label: "Proxies", svg: <IconWire /> },
        { id: "proxyshard", label: "ProxyShard", svg: <IconCart /> },
      ],
    },
    {
      label: "Library",
      items: [{ id: "fingerprints", label: "Fingerprints", svg: <IconHex /> }],
    },
    {
      label: "System",
      items: [{ id: "settings", label: "Settings", svg: <IconCog /> }],
    },
  ];

  // Automation/MCP quick widget (fills the sidebar's lower space).
  const [autoUrl, setAutoUrl] = useState("");
  const [mcpBusy, setMcpBusy] = useState(false);
  useEffect(() => {
    invoke<{ base_url: string; enabled: boolean }>("api_info")
      .then((i) => setAutoUrl(i.enabled ? i.base_url : ""))
      .catch(() => {});
  }, []);
  const downloadMcp = async () => {
    const dir = await open({ directory: true, title: "Where to download the MCP server" });
    if (typeof dir !== "string") return;
    setMcpBusy(true);
    try {
      const p = await invoke<string>("mcp_download", { dir });
      toast.ok(`MCP downloaded to ${p}`);
    } catch (e) { toast.err("MCP download failed: " + String(e)); }
    finally { setMcpBusy(false); }
  };

  return (
    <aside className="sidebar">
      <div className="brand">
        <ShardLogo />
        <span>ShardX</span>
      </div>
      <nav>
        {sections.map((sec) => (
          <div key={sec.label} className="nav-group">
            <div className="nav-group-label">{sec.label}</div>
            {sec.items.map((it) => (
              <button
                key={it.id}
                className={`nav-item ${section === it.id ? "active" : ""}`}
                onClick={() => onSelect(it.id)}
              >
                <span className="nav-icon">{it.svg}</span>
                <span>{it.label}</span>
                {section === it.id && <span className="nav-active-dot" />}
              </button>
            ))}
          </div>
        ))}
      </nav>
      <div className="sidebar-foot">
        <div className="side-auto">
          <div className="side-auto-head">Automation API</div>
          {autoUrl ? (
            <button
              className="side-auto-url"
              title="Copy API base URL"
              onClick={() => { clip.write(autoUrl); toast.ok("Copied API URL"); }}
            >
              <span className="mono">{autoUrl.replace(/^https?:\/\//, "")}</span>
              <Icon.Clone />
            </button>
          ) : (
            <div className="side-auto-off">API off — enable in Settings</div>
          )}
          <button className="side-auto-btn" onClick={downloadMcp} disabled={mcpBusy}>
            <Icon.Download /> {mcpBusy ? "Downloading…" : "Download MCP"}
          </button>
          <button
            className="side-auto-btn"
            onClick={() => {
              openUrl(withUtm("https://docs.proxyshard.com/eng/shardx-launcher-api/binding-and-lifecycle?fallback=true")).catch(() => {});
            }}
            title="Open the full Automation API reference on docs.proxyshard.com"
          >
            <Icon.Info /> Documentation
          </button>
        </div>
        <button
          className="theme-toggle"
          onClick={onToggleTheme}
          title={theme === "dark" ? "Switch to light theme" : "Switch to dark theme"}
        >
          <span className={`theme-seg ${theme === "light" ? "active" : ""}`}>
            <IconSun /> Light
          </span>
          <span className={`theme-seg ${theme === "dark" ? "active" : ""}`}>
            <IconMoon /> Dark
          </span>
        </button>
        <VersionPill />
      </div>
    </aside>
  );
}

// ---- logos / icons ----

function ShardLogo() {
  return (
    <svg width="22" height="22" viewBox="0 0 22 22" className="shard-logo">
      <defs>
        <linearGradient id="g1" x1="0" x2="1" y1="0" y2="1">
          <stop offset="0%" stopColor="#a78bfa" />
          <stop offset="100%" stopColor="#7c3aed" />
        </linearGradient>
      </defs>
      <path d="M11 1L21 11L11 21L1 11Z" fill="url(#g1)" />
      <path d="M11 6L16 11L11 16L6 11Z" fill="#0c0e13" />
    </svg>
  );
}
const IconShard = () => (
  <svg width="14" height="14" viewBox="0 0 14 14"><path d="M7 1L13 7L7 13L1 7Z" fill="currentColor" /></svg>
);
const IconWire = () => (
  <svg width="14" height="14" viewBox="0 0 14 14" fill="none">
    <path d="M2 4H10M4 10H12M3 4L1 6L3 8M11 6L13 8L11 10" stroke="currentColor" strokeWidth="1.4" strokeLinecap="round" strokeLinejoin="round"/>
  </svg>
);
const IconHex = () => (
  <svg width="14" height="14" viewBox="0 0 14 14" fill="none">
    <path d="M7 1L12 4V10L7 13L2 10V4Z" stroke="currentColor" strokeWidth="1.4" strokeLinejoin="round"/>
  </svg>
);
const IconCart = () => (
  <svg width="14" height="14" viewBox="0 0 14 14" fill="none">
    <path d="M1 1.5h1.6l1.2 7.2c.05.3.3.5.6.5h6.1c.3 0 .55-.2.6-.5l.9-4.7H3.3"
          stroke="currentColor" strokeWidth="1.3" strokeLinecap="round" strokeLinejoin="round"/>
    <circle cx="5" cy="12" r="1" fill="currentColor"/>
    <circle cx="10.5" cy="12" r="1" fill="currentColor"/>
  </svg>
);
const IconCog = () => (
  <svg width="14" height="14" viewBox="0 0 14 14" fill="none">
    <circle cx="7" cy="7" r="2" stroke="currentColor" strokeWidth="1.4"/>
    <path d="M7 1V3M7 11V13M1 7H3M11 7H13M2.5 2.5L4 4M10 10L11.5 11.5M2.5 11.5L4 10M10 4L11.5 2.5" stroke="currentColor" strokeWidth="1.4" strokeLinecap="round"/>
  </svg>
);
const IconCopy = () => (
  <svg width="13" height="13" viewBox="0 0 14 14" fill="none">
    <rect x="4.4" y="4.4" width="7.2" height="7.2" rx="1.4" stroke="currentColor" strokeWidth="1.3"/>
    <path d="M9.4 2.4H3.1c-.66 0-1.2.54-1.2 1.2v6.3" stroke="currentColor" strokeWidth="1.3" strokeLinecap="round"/>
  </svg>
);
/// Read-only value with inline copy glyph.
function CopyField({ value, secret }: { value: string; secret?: boolean }) {
  return (
    <div className="copy-field">
      <input readOnly type={secret ? "password" : "text"} value={value} />
      <button
        type="button"
        className="copy-icon"
        title="Copy"
        onClick={async () => { try { await clip.write(value); toast.ok("Copied"); } catch (e) { toast.err(String(e)); } }}
      >
        <IconCopy />
      </button>
    </div>
  );
}
const IconSun = () => (
  <svg width="13" height="13" viewBox="0 0 14 14" fill="none">
    <circle cx="7" cy="7" r="2.6" stroke="currentColor" strokeWidth="1.3"/>
    <path d="M7 .8V2M7 12v1.2M.8 7H2M12 7h1.2M2.6 2.6l.85.85M10.55 10.55l.85.85M2.6 11.4l.85-.85M10.55 3.45l.85-.85"
          stroke="currentColor" strokeWidth="1.3" strokeLinecap="round"/>
  </svg>
);
const IconMoon = () => (
  <svg width="13" height="13" viewBox="0 0 14 14" fill="none">
    <path d="M12 8.2A5 5 0 1 1 5.8 2 4 4 0 0 0 12 8.2z"
          stroke="currentColor" strokeWidth="1.3" strokeLinejoin="round"/>
  </svg>
);

/// Inline-SVG icon set; stroke-based at 14x14, inherits color, `size` override.
type IconProps = { size?: number; className?: string };
const Icon = {
  Pin: ({ size = 13, className }: IconProps) => (
    // Upright thumbtack (Lucide "pin").
    <svg width={size} height={size} viewBox="0 0 24 24" fill="none" className={className}>
      <path d="M12 17v5" stroke="currentColor" strokeWidth="2" strokeLinecap="round"/>
      <path d="M9 10.76a2 2 0 0 1-1.11 1.79l-1.78.9A2 2 0 0 0 5 15.24V16a1 1 0 0 0 1 1h12a1 1 0 0 0 1-1v-.76a2 2 0 0 0-1.11-1.79l-1.78-.9A2 2 0 0 1 15 10.76V7a1 1 0 0 1 1-1 2 2 0 0 0 0-4H8a2 2 0 0 0 0 4 1 1 0 0 1 1 1z"
            stroke="currentColor" strokeWidth="1.7" strokeLinejoin="round"/>
    </svg>
  ),
  Edit: ({ size = 13, className }: IconProps) => (
    <svg width={size} height={size} viewBox="0 0 14 14" fill="none" className={className}>
      <path d="M10 1.5l2.5 2.5M9 2.5l2.5 2.5M2.5 9l6.5-6.5 2.5 2.5L5 11.5l-3 0.5z"
            stroke="currentColor" strokeWidth="1.3" strokeLinejoin="round" strokeLinecap="round"/>
    </svg>
  ),
  Clone: ({ size = 13, className }: IconProps) => (
    // "Duplicate" — two offset rounded rectangles.
    <svg width={size} height={size} viewBox="0 0 14 14" fill="none" className={className}>
      <rect x="4.5" y="4.5" width="8" height="8" rx="1.5" stroke="currentColor" strokeWidth="1.3"/>
      <path d="M2 9.5V2.5C2 1.95 2.45 1.5 3 1.5h7" stroke="currentColor" strokeWidth="1.3" strokeLinecap="round"/>
    </svg>
  ),
  More: ({ size = 13, className }: IconProps) => (
    // Vertical kebab — opens the same menu as right-click.
    <svg width={size} height={size} viewBox="0 0 14 14" fill="currentColor" className={className}>
      <circle cx="7" cy="2.5" r="1.3" />
      <circle cx="7" cy="7" r="1.3" />
      <circle cx="7" cy="11.5" r="1.3" />
    </svg>
  ),
  Trash: ({ size = 13, className }: IconProps) => (
    <svg width={size} height={size} viewBox="0 0 14 14" fill="none" className={className}>
      <path d="M2 3.5h10M5 3.5V2.2C5 1.8 5.3 1.5 5.7 1.5h2.6c0.4 0 0.7 0.3 0.7 0.7v1.3M3.3 3.5l0.7 8.4c0 0.4 0.4 0.6 0.7 0.6h4.6c0.4 0 0.7-0.2 0.7-0.6L10.7 3.5"
            stroke="currentColor" strokeWidth="1.3" strokeLinecap="round" strokeLinejoin="round"/>
      <path d="M6 6v4M8 6v4" stroke="currentColor" strokeWidth="1.3" strokeLinecap="round"/>
    </svg>
  ),
  Refresh: ({ size = 13, className }: IconProps) => (
    <svg width={size} height={size} viewBox="0 0 14 14" fill="none" className={className}>
      <path d="M12 7a5 5 0 1 1-1.5-3.5M12 1.5v3h-3" stroke="currentColor" strokeWidth="1.4" strokeLinecap="round" strokeLinejoin="round"/>
    </svg>
  ),
  Info: ({ size = 13, className }: IconProps) => (
    <svg width={size} height={size} viewBox="0 0 14 14" fill="none" className={className}>
      <circle cx="7" cy="7" r="5.5" stroke="currentColor" strokeWidth="1.3"/>
      <path d="M7 6.3v3.7" stroke="currentColor" strokeWidth="1.3" strokeLinecap="round"/>
      <circle cx="7" cy="4.4" r="0.75" fill="currentColor"/>
    </svg>
  ),
  Folder: ({ size = 13, className }: IconProps) => (
    <svg width={size} height={size} viewBox="0 0 14 14" fill="none" className={className}>
      <path d="M1.5 4.5V3c0-0.5 0.4-1 1-1h3l1.5 1.5h5c0.5 0 1 0.5 1 1V11c0 0.5-0.5 1-1 1H2.5c-0.6 0-1-0.5-1-1V4.5z"
            stroke="currentColor" strokeWidth="1.3" strokeLinejoin="round"/>
    </svg>
  ),
  Upload: ({ size = 13, className }: IconProps) => (
    // Up-arrow into a tray — used for "Export".
    <svg width={size} height={size} viewBox="0 0 14 14" fill="none" className={className}>
      <path d="M7 1.5v7M4 4.5l3-3 3 3M2.5 9.5V12c0 0.3 0.2 0.5 0.5 0.5h8c0.3 0 0.5-0.2 0.5-0.5V9.5"
            stroke="currentColor" strokeWidth="1.3" strokeLinecap="round" strokeLinejoin="round"/>
    </svg>
  ),
  Download: ({ size = 13, className }: IconProps) => (
    // Down-arrow into a tray — used for "Import".
    <svg width={size} height={size} viewBox="0 0 14 14" fill="none" className={className}>
      <path d="M7 8.5v-7M4 5.5l3 3 3-3M2.5 9.5V12c0 0.3 0.2 0.5 0.5 0.5h8c0.3 0 0.5-0.2 0.5-0.5V9.5"
            stroke="currentColor" strokeWidth="1.3" strokeLinecap="round" strokeLinejoin="round"/>
    </svg>
  ),
  Globe: ({ size = 14, className }: IconProps) => (
    <svg width={size} height={size} viewBox="0 0 14 14" fill="none" className={className}>
      <circle cx="7" cy="7" r="5.5" stroke="currentColor" strokeWidth="1.3"/>
      <path d="M1.5 7h11M7 1.5c1.8 2 1.8 9 0 11M7 1.5c-1.8 2-1.8 9 0 11" stroke="currentColor" strokeWidth="1.2"/>
    </svg>
  ),
  Clock: ({ size = 14, className }: IconProps) => (
    <svg width={size} height={size} viewBox="0 0 14 14" fill="none" className={className}>
      <circle cx="7" cy="7" r="5.5" stroke="currentColor" strokeWidth="1.3"/>
      <path d="M7 3.5V7l2.5 1.5" stroke="currentColor" strokeWidth="1.3" strokeLinecap="round"/>
    </svg>
  ),
  Building: ({ size = 14, className }: IconProps) => (
    <svg width={size} height={size} viewBox="0 0 14 14" fill="none" className={className}>
      <path d="M3 12.5V2.5h6v10M9 6.5h2.5V12.5M5 4.5h2M5 6.5h2M5 8.5h2M5 10.5h2"
            stroke="currentColor" strokeWidth="1.3" strokeLinejoin="round" strokeLinecap="round"/>
    </svg>
  ),
  Pin2: ({ size = 11, className }: IconProps) => (
    // Filled inline pin marker (11px).
    <svg width={size} height={size} viewBox="0 0 24 24" fill="none" className={className}>
      <path d="M12 17v5" stroke="currentColor" strokeWidth="2" strokeLinecap="round"/>
      <path d="M9 10.76a2 2 0 0 1-1.11 1.79l-1.78.9A2 2 0 0 0 5 15.24V16a1 1 0 0 0 1 1h12a1 1 0 0 0 1-1v-.76a2 2 0 0 0-1.11-1.79l-1.78-.9A2 2 0 0 1 15 10.76V7a1 1 0 0 1 1-1 2 2 0 0 0 0-4H8a2 2 0 0 0 0 4 1 1 0 0 1 1 1z"
            fill="currentColor" stroke="currentColor" strokeWidth="1.2" strokeLinejoin="round"/>
    </svg>
  ),
  Stop: ({ size = 13, className }: IconProps) => (
    <svg width={size} height={size} viewBox="0 0 14 14" className={className}>
      <rect x="3" y="3" width="8" height="8" rx="1" fill="currentColor"/>
    </svg>
  ),
  Play: ({ size = 13, className }: IconProps) => (
    <svg width={size} height={size} viewBox="0 0 14 14" className={className}>
      <path d="M4 2.5l8 4.5-8 4.5z" fill="currentColor"/>
    </svg>
  ),
};

// ---- Browsers view ----

function BrowsersView() {
  const [profiles, setProfiles] = useState<ProfileMeta[]>([]);
  const [proxies, setProxies] = useState<ProxyEntry[]>([]);
  const [search, setSearch] = useState("");
  const [folder, setFolder] = useState("all");
  const [expanded, setExpanded] = useState<string | null>(null);
  const [draft, setDraft] = useState<ProfileForm | null>(null);
  // Value = epoch ms at which the engine was first observed running. Used
  // both as a truthy flag (any number = running) and as the anchor for the
  // ticking uptime display in the Status column.
  const [running, setRunning] = useState<Record<string, number>>({});
  // Re-render trigger so the uptime label ticks every second without
  // re-fetching the process list (which polls every 2s).
  const [, setUptimeTick] = useState(0);
  useEffect(() => {
    if (Object.keys(running).length === 0) return;
    const h = setInterval(() => setUptimeTick((t) => t + 1), 1000);
    return () => clearInterval(h);
  }, [running]);
  const [selected, setSelected] = useState<Set<string>>(new Set());
  const [templatePickerOpen, setTemplatePickerOpen] = useState(false);
  const [fingerprints, setFingerprints] = useState<FingerprintEntry[]>([]);
  const [quickEdit, setQuickEdit] = useState<{ kind: "proxy" | "notes"; profile: ProfileMeta } | null>(null);
  // Empty folders persist in localStorage until a profile lands in them.
  const [folderRegistry, setFolderRegistry] = useState<string[]>(() => {
    try { return JSON.parse(localStorage.getItem("shardx-folders") || "[]"); }
    catch { return []; }
  });
  const [folderModal, setFolderModal] = useState<{ profileId: string | null } | null>(null);
  // Folder name currently highlighted as a drag-and-drop target ("__all__"
  // for the All tab).  Cleared in dragleave/drop.
  const [dropTarget, setDropTarget] = useState<string | null>(null);
  const rememberFolder = (f: string) =>
    setFolderRegistry((r) => {
      const next = r.includes(f) ? r : [...r, f];
      localStorage.setItem("shardx-folders", JSON.stringify(next));
      return next;
    });
  const ctx = useContextMenu();

  const reload = async () => {
    try {
      setProfiles(await invoke<ProfileMeta[]>("profile_list"));
      setProxies(await invoke<ProxyEntry[]>("proxy_list"));
    } catch (e) {
      toast.err(String(e));
    }
  };
  useEffect(() => { reload(); }, []);
  useEffect(() => {
    invoke<FingerprintEntry[]>("fingerprint_list").then(setFingerprints).catch((e) => toast.err(String(e)));
  }, []);

  // Scroll the expanded editor into view after expand animation.
  useEffect(() => {
    if (!expanded || expanded === "__new__") return;
    const t = setTimeout(() => {
      const el = document.querySelector<HTMLElement>(".row-wrap.row-expanded .inline-editor");
      el?.scrollIntoView({ behavior: "smooth", block: "center" });
    }, 60);
    return () => clearTimeout(t);
  }, [expanded]);

  // 2s poll for real child status; not optimistic UI state.  Uptime is
  // anchored to the moment the engine actually started (now - uptime_ms),
  // preserved across polls so the displayed clock doesn't jitter.  When a
  // profile transitions running → not-running, the backend has just bumped
  // its persisted `total_runtime_ms` — re-fetch profile_list so the Time
  // column reflects the new total (otherwise it shows whatever was on
  // disk before this session started, looking like a "reset").
  useEffect(() => {
    let cancelled = false;
    const tick = async () => {
      try {
        const list = await invoke<{ profile_id: string; pid: number; uptime_ms: number }[]>("process_list");
        if (cancelled) return;
        const now = Date.now();
        setRunning((prev) => {
          const next: Record<string, number> = {};
          for (const r of list) {
            next[r.profile_id] = prev[r.profile_id] ?? (now - r.uptime_ms);
          }
          // Detect any profile that was running on the previous tick but
          // dropped off this one — those need a profile_list refresh so
          // the freshly-accumulated total_runtime_ms appears in the UI.
          const justExited = Object.keys(prev).some((id) => !(id in next));
          if (justExited) {
            // Defer to next tick so React commits `next` before reload()
            // races against it.
            setTimeout(() => { if (!cancelled) reload(); }, 0);
          }
          return next;
        });
      } catch {}
    };
    tick();
    const handle = setInterval(tick, 2000);
    return () => { cancelled = true; clearInterval(handle); };
  }, []);

  const proxyMap = useMemo(() => Object.fromEntries(proxies.map((p) => [p.id, p])), [proxies]);

  // Folder tabs derived from profile assignments; "all" always first.
  const folders = useMemo(() => {
    const set = new Set<string>(folderRegistry);
    for (const p of profiles) if (p.folder) set.add(p.folder);
    return [...set].sort((a, b) => a.localeCompare(b));
  }, [profiles, folderRegistry]);

  const visible = useMemo(
    () =>
      profiles.filter(
        (p) =>
          (folder === "all" || p.folder === folder) &&
          p.name.toLowerCase().includes(search.toLowerCase()),
      ),
    [profiles, search, folder],
  );

  // Native non-passive wheel handler turns vertical scroll into horizontal tab scroll.
  const folderTabsRef = useRef<HTMLDivElement>(null);
  useEffect(() => {
    const el = folderTabsRef.current;
    if (!el) return;
    const onWheel = (e: WheelEvent) => {
      if (el.scrollWidth <= el.clientWidth || e.deltaY === 0) return;
      e.preventDefault();
      el.scrollLeft += e.deltaY;
    };
    el.addEventListener("wheel", onWheel, { passive: false });
    return () => el.removeEventListener("wheel", onWheel);
  }, []);

  // Pagination of the (filtered) profile list.
  const PAGE_SIZE = 20;
  const [page, setPage] = useState(1);
  const pageCount = Math.max(1, Math.ceil(visible.length / PAGE_SIZE));
  // Reset to page 1 when the filter changes; clamp if the list shrank.
  useEffect(() => { setPage(1); }, [folder, search]);
  useEffect(() => { if (page > pageCount) setPage(pageCount); }, [pageCount, page]);
  const paged = useMemo(
    () => visible.slice((page - 1) * PAGE_SIZE, page * PAGE_SIZE),
    [visible, page],
  );

  // Fall back to "all" when the active folder tab becomes empty.
  useEffect(() => {
    if (folder !== "all" && !folders.includes(folder)) setFolder("all");
  }, [folders, folder]);

  const runningCount = Object.values(running).filter(Boolean).length;

  // Block the Start button until `invoke("launch")` returns (success or
  // failure).  The launch includes pre-flight steps that can take real time
  // — UDP probe, geo lookup, Widevine pre-warm — and surfacing the busy
  // state for the whole window is what the user sees as "did it work?".
  // On failure we unlock immediately and toast the error.
  const [startBusy, setStartBusy] = useState<Set<string>>(new Set());
  const startStop = async (p: ProfileMeta) => {
    if (running[p.id]) {
      try {
        await invoke<boolean>("process_kill", { profileId: p.id });
      } catch (e) {
        toast.err(String(e));
      }
      return;
    }
    if (startBusy.has(p.id)) return;
    setStartBusy((s) => new Set([...s, p.id]));
    try {
      await invoke<number>("launch", { profileId: p.id });
      // Don't optimistically flip `running` here; the 2s poll above picks
      // up the new child immediately and anchors the uptime clock.
    } catch (e) {
      toast.err(String(e));
    } finally {
      setStartBusy((s) => {
        const n = new Set(s);
        n.delete(p.id);
        return n;
      });
    }
  };

  const remove = async (id: string) => {
    if ((await confirmModal({ title: "Delete profile", message: "Delete this profile? Its user-data dir is wiped too.", danger: true })) !== true) return;
    await invoke("profile_delete", { id });
    reload();
  };

  const cloneProfile = async (id: string) => {
    try {
      await invoke<ProfileMeta>("profile_clone", { id });
      reload();
    } catch (e) { toast.err(String(e)); }
  };

  const exportCookies = async (p: ProfileMeta) => {
    try {
      const path = await saveDialog({
        defaultPath: `${(p.name || p.id).replace(/[^\w.-]+/g, "_")}-cookies.json`,
        filters: [{ name: "JSON", extensions: ["json"] }],
      });
      if (typeof path !== "string") return; // cancelled
      const n = await invoke<number>("cookies_export_to_file", { profileId: p.id, path });
      toast.ok(`Exported ${n} cookie${n === 1 ? "" : "s"}`);
      // Open the containing folder so the user sees exactly where it went.
      const dir = path.replace(/[/\\][^/\\]*$/, "");
      try { await openPath(dir); } catch {}
    } catch (e) { toast.err(String(e)); }
  };

  const importCookies = async (p: ProfileMeta) => {
    if (running[p.id]) { toast.err("Stop the profile before importing cookies"); return; }
    try {
      const path = await open({
        multiple: false, directory: false, title: "Select cookies JSON",
        filters: [{ name: "JSON", extensions: ["json"] }],
      });
      if (typeof path !== "string") return;
      const text = await invoke<string>("read_text_file", { path });
      const cookies = JSON.parse(text);
      if (!Array.isArray(cookies)) { toast.err("Expected a JSON array of cookies"); return; }
      const n = await invoke<number>("cookies_import", { profileId: p.id, cookies });
      toast.ok(`Imported ${n} cookie${n === 1 ? "" : "s"}`);
    } catch (e) { toast.err(String(e)); }
  };

  // Per-profile action menu shared by right-click and ⋮ button.
  const profileMenu = (p: ProfileMeta) => [
    { label: running[p.id] ? "Stop" : "Launch", onClick: () => startStop(p) },
    { label: "Edit", onClick: () => expand(p.id) },
    { label: "Clone", onClick: () => cloneProfile(p.id) },
    { label: p.pinned ? "Unpin" : "Pin to top", onClick: () => togglePin(p) },
    { sep: true, label: "", onClick: () => {} },
    { label: "Move to folder…", onClick: () => setFolderModal({ profileId: p.id }) },
    ...(p.folder
      ? [{ label: "Remove from folder", onClick: () => setProfileFolder(p.id, "") }]
      : []),
    { sep: true, label: "", onClick: () => {} },
    { label: "Export cookies", onClick: () => exportCookies(p) },
    { label: "Import cookies", onClick: () => importCookies(p) },
    { sep: true, label: "", onClick: () => {} },
    { label: "Delete", onClick: () => remove(p.id), danger: true },
  ];

  const togglePin = async (p: ProfileMeta) => {
    try {
      await invoke("profile_set_pin", { id: p.id, pinned: !p.pinned });
      reload();
    } catch (e) { toast.err(String(e)); }
  };

  const setProfileFolder = async (id: string, f: string) => {
    try {
      await invoke("profile_set_folder", { id, folder: f });
      if (f) rememberFolder(f);
      reload();
    } catch (e) { toast.err(String(e)); }
  };

  const deleteFolder = async (f: string) => {
    const count = profiles.filter((p) => p.folder === f).length;
    // Three outcomes: delete profiles, unfile, cancel.
    const choice = await confirmModal({
      title: `Delete folder “${f}”`,
      message:
        count > 0
          ? `This folder has ${count} profile${count === 1 ? "" : "s"}. ` +
            `Delete them too, or keep them (they move to “All”)?`
          : `Delete the empty folder “${f}”?`,
      buttons:
        count > 0
          ? [
              { label: "Cancel", value: "cancel" },
              { label: "Keep profiles", value: "keep" },
              { label: "Delete profiles", value: "delete", danger: true },
            ]
          : [
              { label: "Cancel", value: "cancel" },
              { label: "Delete", value: "keep", danger: true },
            ],
    });
    if (choice == null || choice === "cancel") return;
    const alsoDelete = choice === "delete";
    try {
      const n = await invoke<number>("folder_delete", { folder: f, deleteProfiles: alsoDelete });
      if (folder === f) setFolder("all");
      reload();
      toast.ok(
        alsoDelete
          ? `Deleted folder “${f}” + ${n} profile${n === 1 ? "" : "s"}`
          : `Removed folder “${f}” (${n} profile${n === 1 ? "" : "s"} kept)`,
      );
    } catch (e) { toast.err(String(e)); }
  };

  const bulkLaunch = async () => {
    for (const id of selected) {
      if (running[id]) continue;
      try { await invoke<number>("launch", { profileId: id }); } catch {}
    }
    setSelected(new Set());
  };

  const bulkStop = async () => {
    for (const id of selected) {
      try { await invoke<boolean>("process_kill", { profileId: id }); } catch {}
    }
    setSelected(new Set());
  };

  const bulkDelete = async () => {
    const ids = [...selected];
    if (ids.length === 0) return;
    if ((await confirmModal({ title: "Delete profiles", message: `Delete ${ids.length} profile${ids.length === 1 ? "" : "s"}? This wipes their user-data dirs too.`, danger: true })) !== true) return;
    for (const id of ids) {
      try { await invoke("profile_delete", { id }); } catch (e) { toast.err(String(e)); }
    }
    setSelected(new Set());
    reload();
    toast.ok(`Deleted ${ids.length}`);
  };

  /// Dump selected profile FingerprintConfigs as a JSON array to clipboard.
  const bulkExport = async () => {
    const ids = [...selected];
    if (ids.length === 0) return;
    try {
      const payloads = await Promise.all(ids.map((id) => invoke<any>("profile_get", { id })));
      await clip.write(JSON.stringify(payloads, null, 2));
      toast.ok(`Copied ${payloads.length} to clipboard`);
    } catch (e) { toast.err(String(e)); }
  };

  // Paste profile JSON from clipboard → fresh profiles.
  const bulkImport = async () => {
    try {
      const text = await clip.read();
      if (!text.trim()) { toast.err("Clipboard is empty"); return; }
      const data = JSON.parse(text);
      const arr = Array.isArray(data) ? data : [data];
      const n = await invoke<number>("profile_import", { payloads: arr });
      reload();
      toast.ok(`Imported ${n} profile${n === 1 ? "" : "s"}`);
    } catch (e) { toast.err("Import failed: " + String(e)); }
  };

  const expand = async (id: string) => {
    if (expanded === id) { setExpanded(null); setDraft(null); return; }
    const stored = await invoke<any>("profile_get", { id });
    setDraft(fromStored(stored));
    setExpanded(id);
  };

  const newProfile = async () => {
    setDraft(defaultForm());
    setExpanded("__new__");
  };

  const saveDraft = async () => {
    if (!draft) return;
    try {
      const fp = fingerprints.find((g) => g.id === draft.gpu_preset_id) ?? null;
      const saved = await invoke<ProfileMeta>("profile_save", { payload: toStored(draft, fp) });
      await invoke("profile_bind_proxy", { profileId: saved.id, proxyId: draft.proxy_id });
      // A profile created while a folder tab is active should land in that
      // folder (otherwise it pops into "All" and the user has to drag it
      // back themselves).  `__new__` test scopes this to creations only —
      // edits preserve whatever folder the profile already had.
      if (!draft.id && folder && folder !== "all") {
        try { await invoke("profile_set_folder", { id: saved.id, folder }); }
        catch (e) { console.warn("auto-assign folder failed:", e); }
      }
      setExpanded(null);
      setDraft(null);
      reload();
      toast.ok(draft.id ? "Profile saved" : `Created "${saved.name}"`);
    } catch (e) { toast.err(String(e)); }
  };

  const toggleSel = (id: string) => {
    setSelected((s) => {
      const n = new Set(s);
      if (n.has(id)) n.delete(id); else n.add(id);
      return n;
    });
  };

  return (
    <section className="page">
      <Topbar crumbs={["Workspace", "Browsers"]} search={search} onSearch={setSearch} />

      <div className="metric-strip">
        <Metric label="Profiles" value={String(profiles.length)} accent />
        <Metric label="Running" value={String(runningCount)} pulse={runningCount > 0} />
        <Metric label="Proxies" value={String(proxies.length)} />
        <Metric label="Fingerprints" value={String(fingerprints.length)} />
      </div>

      <div className="page-title">
        <div className="title-with-tabs">
          <h1>Browsers</h1>
          <div className="folder-tabs" ref={folderTabsRef}>
            <button
              className={`folder-tab ${folder === "all" ? "active" : ""} ${dropTarget === "__all__" ? "folder-tab-drop" : ""}`}
              onClick={() => setFolder("all")}
              // Unconditional preventDefault on dragover is the *only* way
              // HTML5 marks the element as a valid drop target — any extra
              // logic inside the handler is fine, but the preventDefault
              // itself must fire on every event or `drop` never lands.
              onDragOver={(e) => {
                e.preventDefault();
                e.dataTransfer.dropEffect = "move";
                if (dropTarget !== "__all__") setDropTarget("__all__");
              }}
              onDragLeave={(e) => {
                // Ignore enter-into-child events: relatedTarget will be a
                // descendant of the button, in which case the drag is still
                // over us — clearing the highlight here would steal the drop.
                if (!e.currentTarget.contains(e.relatedTarget as Node)) {
                  setDropTarget(null);
                }
              }}
              onDrop={(e) => {
                e.preventDefault();
                setDropTarget(null);
                const id = e.dataTransfer.getData("application/x-shardx-profile")
                        || e.dataTransfer.getData("text/plain");
                if (id) setProfileFolder(id, "");           // "" = unassign folder
              }}
            >
              All<span className="tab-count">{profiles.length}</span>
            </button>
            {folders.map((f) => (
              <button
                key={f}
                className={`folder-tab ${folder === f ? "active" : ""} ${dropTarget === f ? "folder-tab-drop" : ""}`}
                onClick={() => setFolder(f)}
                title="Right-click for folder actions · drop profiles to move them"
                onContextMenu={(e) =>
                  ctx.open(e, [
                    { label: "Delete folder…", onClick: () => deleteFolder(f), danger: true },
                  ])
                }
                onDragOver={(e) => {
                  e.preventDefault();
                  e.dataTransfer.dropEffect = "move";
                  if (dropTarget !== f) setDropTarget(f);
                }}
                onDragLeave={(e) => {
                  if (!e.currentTarget.contains(e.relatedTarget as Node)) {
                    setDropTarget(null);
                  }
                }}
                onDrop={(e) => {
                  e.preventDefault();
                  setDropTarget(null);
                  const id = e.dataTransfer.getData("application/x-shardx-profile")
                          || e.dataTransfer.getData("text/plain");
                  if (id) setProfileFolder(id, f);
                }}
              >
                {f}
                <span className="tab-count">
                  {profiles.filter((p) => p.folder === f).length}
                </span>
              </button>
            ))}
            <button
              className="folder-tab folder-tab-add"
              title="Create a new folder"
              onClick={() => setFolderModal({ profileId: null })}
            >
              +
            </button>
          </div>
        </div>
        <div className="page-actions">
          {selected.size > 0 && (
            <div className="bulk-bar">
              <span>{selected.size} selected</span>
              <button className="btn-ghost btn-sm" onClick={bulkLaunch}><Icon.Play /> Launch</button>
              <button className="btn-ghost btn-sm" onClick={bulkStop}><Icon.Stop /> Stop</button>
              <button className="btn-ghost btn-sm" onClick={bulkExport}><Icon.Upload /> Export</button>
              <button className="btn-ghost btn-sm" onClick={bulkDelete}><Icon.Trash /> Delete</button>
              <button className="btn-ghost btn-sm" onClick={() => setSelected(new Set())}>Clear</button>
            </div>
          )}
          <button className="btn-ghost" onClick={bulkImport} title="Create profiles from exported JSON in the clipboard"><Icon.Download /> Import</button>
          <button className="btn-ghost" onClick={() => setTemplatePickerOpen(true)}><ShardMini /> From template</button>
          <button className="btn-primary" onClick={newProfile}>+ New profile</button>
        </div>
      </div>
      {templatePickerOpen && (
        <TemplatePicker
          fingerprints={fingerprints}
          onPick={async (tplId) => {
            try {
              const meta = await invoke<ProfileMeta>("profile_create_from_template", { templateId: tplId });
              setTemplatePickerOpen(false);
              reload();
              toast.ok(`Profile "${meta.name}" created`);
              // Auto-open the new profile in editor
              setTimeout(async () => {
                const stored = await invoke<any>("profile_get", { id: meta.id });
                setDraft(fromStored(stored));
                setExpanded(meta.id);
              }, 50);
            } catch (e) { toast.err(String(e)); }
          }}
          onClose={() => setTemplatePickerOpen(false)}
        />
      )}
      {folderModal && (() => {
        const moving = folderModal.profileId
          ? profiles.find((p) => p.id === folderModal!.profileId) ?? null
          : null;
        // "move" mode: pick from other folders; "create" mode: just the input.
        const pickable = moving ? folders.filter((f) => f !== moving.folder) : [];
        const assign = (f: string) => {
          if (folderModal!.profileId) setProfileFolder(folderModal!.profileId, f);
          else rememberFolder(f);
          setFolder(f);
          setFolderModal(null);
        };
        return (
          <FolderModal
            mode={folderModal.profileId ? "move" : "create"}
            existing={pickable}
            onPick={assign}
            onCreate={(name) => { const f = name.trim(); if (f) assign(f); }}
            onClose={() => setFolderModal(null)}
          />
        );
      })()}

      <div className="rows">
        <div className="rows-head t-cols">
          <div></div>
          <div>
            <input
              type="checkbox"
              title="Select all on this page"
              // Header checkbox toggles only visible page rows; other pages preserved.
              checked={paged.length > 0 && paged.every((p) => selected.has(p.id))}
              ref={(el) => {
                if (!el) return;
                const anySel = paged.some((p) => selected.has(p.id));
                const allSel = paged.length > 0 && paged.every((p) => selected.has(p.id));
                el.indeterminate = anySel && !allSel;
              }}
              onChange={(e) => {
                setSelected((prev) => {
                  const next = new Set(prev);
                  if (e.target.checked) {
                    for (const p of paged) next.add(p.id);
                  } else {
                    for (const p of paged) next.delete(p.id);
                  }
                  return next;
                });
              }}
            />
          </div>
          <div>Name</div><div>Status</div><div>Proxy</div><div>Notes</div><div className="head-time">Time</div><div className="head-lastrun">Last run</div><div></div>
        </div>
        {expanded === "__new__" && draft && (
          <div className="row-wrap row-expanded row-new">
            <InlineEditor
              draft={draft}
              setDraft={setDraft}
              proxies={proxies}
              fingerprints={fingerprints}
              onSave={saveDraft}
              onCancel={() => { setExpanded(null); setDraft(null); }}
            />
          </div>
        )}
        {paged.map((p) => {
          const px = p.proxy_id ? proxyMap[p.proxy_id] : null;
          const isRunning = !!running[p.id];
          const isExpanded = expanded === p.id;
          const isSel = selected.has(p.id);
          return (
            <div
              key={p.id}
              className={`row-wrap ${isRunning ? "row-running" : ""} ${isExpanded ? "row-expanded" : ""} ${p.pinned ? "row-pinned" : ""}`}
              onContextMenu={(e) => ctx.open(e, profileMenu(p))}
              draggable={!isExpanded}
              onDragStart={(e) => {
                e.dataTransfer.effectAllowed = "move";
                // Set BOTH a custom MIME (so non-folder drop zones can ignore
                // it) and text/plain (because Firefox refuses to start a
                // drag at all without text/plain, and some Chromium variants
                // hide custom MIME values from `dataTransfer.types` during
                // dragover for cross-origin reasons).
                e.dataTransfer.setData("application/x-shardx-profile", p.id);
                e.dataTransfer.setData("text/plain", p.id);
                // Replace the default full-row ghost (it obscures the folder
                // tabs and stops the drop event firing on them) with a tiny
                // chip that floats next to the cursor.
                const chip = document.createElement("div");
                chip.className = "drag-chip";
                chip.textContent = p.name || p.id.slice(0, 8);
                document.body.appendChild(chip);
                e.dataTransfer.setDragImage(chip, 12, 12);
                // The ghost is rasterised synchronously from the live DOM,
                // so we can safely remove it on the next tick.
                setTimeout(() => chip.remove(), 0);
              }}
            >
              <div className="row t-cols">
                <div className="cell-strip">
                  <span className={`shard ${isRunning ? "shard-on" : "shard-off"}`} />
                </div>
                <div>
                  <input type="checkbox" checked={isSel} onChange={() => toggleSel(p.id)} />
                </div>
                <div className="cell-name" onClick={() => expand(p.id)}>
                  <div className="name-main">
                    {p.pinned && <span className="pin-mark" title="Pinned"><Icon.Pin2 /></span>}
                    {p.name}
                  </div>
                  <div className="name-sub">{p.id.slice(0, 8)}</div>
                </div>
                <div>
                  <span className={`pill-status ${isRunning ? "ps-on" : "ps-off"}`}>
                    <i className="dot" />
                    {isRunning ? "Running" : "Idle"}
                  </span>
                </div>
                <div className="cell-click" onClick={() => setQuickEdit({ kind: "proxy", profile: p })} title="Change proxy">
                  {px ? (
                    <div className="proxy-cell">
                      <span className={`badge badge-${px.kind}`}>{px.kind}</span>
                      <span className="proxy-loc">
                        {px.country && (
                          <>
                            <CountryFlag cc={px.country} />
                            <span className="flag">{px.country}</span>
                          </>
                        )}
                        <span className="mono small">{px.host}:{px.port}</span>
                      </span>
                    </div>
                  ) : <span className="muted small">— direct —</span>}
                </div>
                <div
                  className="cell-notes cell-click"
                  title={p.notes || "Click to edit notes"}
                  onClick={() => setQuickEdit({ kind: "notes", profile: p })}
                >
                  {p.notes || <span className="muted">—</span>}
                </div>
                <div className="cell-time">
                  <span className={`small ${isRunning ? "" : "muted"}`}>
                    {(() => {
                      const live = isRunning ? Date.now() - running[p.id] : 0;
                      const total = p.total_runtime_ms + live;
                      return total > 0 ? fmtUptime(total) : "—";
                    })()}
                  </span>
                </div>
                <div className="cell-lastrun"><span className="muted small">{p.last_launched_at ? fmtTs(p.last_launched_at) : "never"}</span></div>
                <div className="row-actions">
                  <button
                    className={`btn-launch ${isRunning ? "btn-launch-stop" : ""}`}
                    onClick={() => startStop(p)}
                    disabled={!isRunning && startBusy.has(p.id)}
                    title={!isRunning && startBusy.has(p.id) ? "Starting (UDP probe + geo + spawn)…" : undefined}
                  >
                    {isRunning ? (
                      <><span className="btn-launch-ico"><Icon.Stop size={10} /></span><span>Stop</span></>
                    ) : startBusy.has(p.id) ? (
                      <><span className="btn-launch-ico spin"><Icon.Play size={10} /></span><span>Starting…</span></>
                    ) : (
                      <><span className="btn-launch-ico"><Icon.Play size={10} /></span><span>Start</span></>
                    )}
                  </button>
                  <button
                    className={`icon-btn ${p.pinned ? "icon-btn-on" : ""}`}
                    onClick={() => togglePin(p)}
                    title={p.pinned ? "Unpin" : "Pin to top"}
                  >
                    <Icon.Pin />
                  </button>
                  <button className="icon-btn" onClick={() => expand(p.id)} title="Edit"><Icon.Edit /></button>
                  <button className="icon-btn" onClick={() => cloneProfile(p.id)} title="Clone"><Icon.Clone /></button>
                  <button className="icon-btn danger" onClick={() => remove(p.id)} title="Delete"><Icon.Trash /></button>
                  <button
                    className="icon-btn"
                    onClick={(e) => { e.stopPropagation(); ctx.open(e, profileMenu(p)); }}
                    title="More actions"
                  ><Icon.More /></button>
                </div>
              </div>
              {isExpanded && draft && (
                <InlineEditor
                  draft={draft}
                  setDraft={setDraft}
                  proxies={proxies}
                  fingerprints={fingerprints}

                  onSave={saveDraft}
                  onCancel={() => { setExpanded(null); setDraft(null); }}
                />
              )}
            </div>
          );
        })}
        {visible.length === 0 && !expanded && (
          <div className="empty-rich">
            <div className="empty-shard"><ShardLogo /></div>
            <h3>No profiles yet</h3>
            <p>Pick a fingerprint template to start from a curated real-Chrome snapshot, or build one from scratch.</p>
            <div className="empty-cta">
              <button className="btn-ghost" onClick={() => setTemplatePickerOpen(true)}><ShardMini /> From template</button>
              <button className="btn-primary" onClick={newProfile}>+ New profile</button>
            </div>
          </div>
        )}
      </div>
      {pageCount > 1 && (
        <div className="pager">
          <button className="btn-ghost btn-sm" disabled={page <= 1}
            onClick={() => setPage((p) => Math.max(1, p - 1))}>‹ Prev</button>
          <span className="pager-info">Page {page} of {pageCount} · {visible.length} profiles</span>
          <button className="btn-ghost btn-sm" disabled={page >= pageCount}
            onClick={() => setPage((p) => Math.min(pageCount, p + 1))}>Next ›</button>
        </div>
      )}
      {ctx.node}
      {quickEdit && (
        <QuickEditDialog
          kind={quickEdit.kind}
          profile={quickEdit.profile}
          proxies={proxies}
          onClose={() => setQuickEdit(null)}
          onSaved={() => { setQuickEdit(null); reload(); }}
        />
      )}
    </section>
  );
}

function QuickEditDialog({
  kind, profile, proxies, onClose, onSaved,
}: {
  kind: "proxy" | "notes";
  profile: ProfileMeta;
  proxies: ProxyEntry[];
  onClose: () => void;
  onSaved: () => void;
}) {
  const [proxyId, setProxyId] = useState<string | null>(profile.proxy_id);
  const [notes, setNotes] = useState(profile.notes);

  const saveProxy = async () => {
    try {
      await invoke("profile_bind_proxy", { profileId: profile.id, proxyId });
      toast.ok("Proxy updated");
      onSaved();
    } catch (e) { toast.err(String(e)); }
  };

  const saveNotes = async () => {
    try {
      // Round-trip the whole profile JSON so the user's other fields stay intact.
      const stored = await invoke<any>("profile_get", { id: profile.id });
      stored.notes = notes;
      await invoke<ProfileMeta>("profile_save", { payload: stored });
      toast.ok("Notes saved");
      onSaved();
    } catch (e) { toast.err(String(e)); }
  };

  return (
    <div className="dialog-bg" onClick={onClose}>
      <div className="dialog" onClick={(e) => e.stopPropagation()}>
        <header className="dialog-head">
          <h2><ShardMini /> {kind === "proxy" ? "Bind proxy" : "Edit notes"} — {profile.name}</h2>
          <button className="icon-btn" onClick={onClose}>✕</button>
        </header>
        <div className="dialog-body">
          {kind === "proxy" ? (
            <label>
              <span className="lbl">Proxy</span>
              <select value={proxyId ?? ""} onChange={(e) => setProxyId(e.target.value || null)}>
                <option value="">— direct connection —</option>
                {proxies.map((px) => (
                  <option key={px.id} value={px.id}>
                    {px.name || `${px.host}:${px.port}`} · {px.country || px.kind}
                  </option>
                ))}
              </select>
            </label>
          ) : (
            <label>
              <span className="lbl">Notes</span>
              <textarea rows={6} value={notes} onChange={(e) => setNotes(e.target.value)} autoFocus />
            </label>
          )}
        </div>
        <footer className="dialog-foot">
          <button className="btn-ghost" onClick={onClose}>Cancel</button>
          <button className="btn-primary" onClick={kind === "proxy" ? saveProxy : saveNotes}>
            <ShardMini /> Save
          </button>
        </footer>
      </div>
    </div>
  );
}

// ---- inline editor ----

type OsPlatform = "macOS" | "Windows" | "Linux";
const OS_OPTIONS: { id: OsPlatform; label: string }[] = [
  { id: "macOS",   label: "macOS"   },
  { id: "Windows", label: "Windows" },
  { id: "Linux",   label: "Linux"   },
];

function InlineEditor({
  draft, setDraft, proxies, fingerprints, onSave, onCancel,
}: {
  draft: ProfileForm;
  setDraft: (f: ProfileForm) => void;
  proxies: ProxyEntry[];
  fingerprints: FingerprintEntry[];
  onSave: () => void;
  onCancel: () => void;
}) {
  const f = draft;
  const u = <K extends keyof ProfileForm>(k: K, v: ProfileForm[K]) => setDraft({ ...f, [k]: v });

  // OS filter init from bound fingerprint's platform; new profile uses host OS.
  const currentFp = fingerprints.find((x) => x.id === f.gpu_preset_id);
  const [osFilter, setOsFilter] = useState<OsPlatform>(
    (currentFp?.platform as OsPlatform) ?? HOST_OS
  );
  const gpusForOs = useMemo(
    () => fingerprints.filter((fp) => fp.platform === osFilter),
    [fingerprints, osFilter],
  );

  /// Pick GPU = full fingerprint snap; toStored carries lib.payload at save.
  const setGpu = async (id: string) => {
    const fp = fingerprints.find((x) => x.id === id);
    if (!fp) return;
    const nav = fp.payload?.navigator ?? {};
    // Ask Rust for the same hw + platform_version triplet save uses.
    let picks: { hardware_concurrency?: number; device_memory?: number; platform_version?: string } = {};
    try {
      picks = await invoke<{ hardware_concurrency?: number; device_memory?: number; platform_version?: string }>(
        "enrich_picks_for_preset",
        { presetId: id }
      );
    } catch {
      // Fall back to preset's nav defaults if Rust enrich fails.
    }
    setDraft({
      ...f,
      gpu_preset_id: id,
      hardware_concurrency: picks.hardware_concurrency ?? nav.hardware_concurrency ?? f.hardware_concurrency,
      device_memory: picks.device_memory ?? nav.device_memory ?? f.device_memory,
      platform_version: picks.platform_version ?? f.platform_version,
      user_agent: nav.user_agent ?? f.user_agent,
    });
  };

  // Snap unknown / empty gpu_preset_id to a random GPU of the active OS.
  useEffect(() => {
    if (fingerprints.length === 0) return;
    const exists = fingerprints.some((g) => g.id === f.gpu_preset_id);
    if (!exists) {
      const pool = gpusForOs.length > 0 ? gpusForOs : fingerprints;
      const pick = pool[Math.floor(Math.random() * pool.length)];
      if (pick) setGpu(pick.id);
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [fingerprints, osFilter, f.gpu_preset_id]);

  const pickOs = (os: OsPlatform) => {
    setOsFilter(os);
    // Switch GPU to first of new OS if current doesn't match.
    if (currentFp && currentFp.platform !== os) {
      const first = fingerprints.find((g) => g.platform === os);
      if (first) setGpu(first.id);
    }
  };

  return (
    <div className="inline-editor">
      <div className="ie-stripe" />
      <div className="ie-grid">
        {/* ----- col 1: identity + hardware ----- */}
        <div className="ie-section">
          <div className="ie-section-title">Identity</div>
          <Field label="Profile name" value={f.name} onChange={(v) => u("name", v)} placeholder="e.g. shop-pl-1" />

          <label>
            <span className="lbl">Operating system</span>
            <div className="seg">
              {OS_OPTIONS.map((o) => (
                <button
                  key={o.id}
                  type="button"
                  className={`seg-btn ${osFilter === o.id ? "active" : ""}`}
                  onClick={() => pickOs(o.id)}
                >
                  {o.label}
                </button>
              ))}
            </div>
          </label>

          <label>
            <span className="lbl">GPU / device (from Fingerprint Library)</span>
            <CSSelect
              value={f.gpu_preset_id}
              onChange={(v) => setGpu(v)}
              placeholder={`— no ${osFilter} fingerprints in library —`}
              options={gpusForOs.map((g) => ({ value: g.id, label: g.label }))}
            />
          </label>

          <Field label="User-Agent" value={f.user_agent} onChange={(v) => u("user_agent", v)} mono />

          <div className="form-row">
            <SelectField
              label="CPU cores"
              value={f.hardware_concurrency}
              onChange={(v) => u("hardware_concurrency", v)}
              options={CPU_OPTIONS}
            />
            <SelectField
              label="Memory (GB)"
              value={f.device_memory}
              onChange={(v) => u("device_memory", v)}
              options={MEMORY_OPTIONS}
            />
          </div>

          <label>
            <span className="lbl">Proxy</span>
            <CSSelect
              value={f.proxy_id ?? ""}
              onChange={(v) => u("proxy_id", v ? v : null)}
              options={[
                { value: "", label: "— direct connection —" },
                ...proxies.map((px) => ({
                  value: px.id,
                  label: `${px.name || `${px.host}:${px.port}`} · ${px.country || px.kind}`,
                })),
              ]}
            />
          </label>
        </div>

        {/* ----- col 2: locale + noise ----- */}
        <div className="ie-section">
          <div className="ie-section-title">Locale</div>
          <div className="form-row">
            <label>
              <span className="lbl">Timezone</span>
              <CSSelect
                value={f.timezone}
                onChange={(v) => u("timezone", v)}
                options={TIMEZONES.map((tz) => ({
                  value: tz,
                  label: tz === AUTO_TZ ? "Auto (from proxy geo)" : tz,
                }))}
              />
            </label>
            <label>
              <span className="lbl">Language</span>
              <CSSelect
                value={f.language}
                onChange={(v) => u("language", v)}
                options={LOCALES.map((l) => ({ value: l.code, label: l.label }))}
              />
            </label>
          </div>

          <div className="ie-section-title" style={{ marginTop: 6 }}>Noise</div>
          <div className="noise-grid noise-grid-3">
            <Pair label="Canvas"        value={f.noise_canvas}        on={(v) => u("noise_canvas", v)} />
            <Pair label="WebGL"         value={f.noise_webgl}         on={(v) => u("noise_webgl", v)} />
            <Pair label="Audio"         value={f.noise_audio}         on={(v) => u("noise_audio", v)} />
            <Pair label="Client rects"  value={f.noise_client_rects}  on={(v) => u("noise_client_rects", v)} />
            <Pair label="Sensors"       value={f.noise_sensors}       on={(v) => u("noise_sensors", v)} />
            <Pair label="Fonts"         value={f.noise_fonts}         on={(v) => u("noise_fonts", v)} onText="Noise" />
          </div>

          <PortList
            label="Ports to block"
            value={f.blocked_ports}
            onChange={(v) => u("blocked_ports", v)}
          />
        </div>

        {/* ----- col 3: privacy + media + notes ----- */}
        <div className="ie-section">
          <div className="ie-section-title">Privacy</div>
          <div className="form-row">
            <label>
              <span className="lbl">WebRTC</span>
              <CSSelect
                value={f.webrtc}
                onChange={(v) => u("webrtc", v as WebRtcMode)}
                options={[
                  { value: "auto", label: "Auto" },
                  { value: "tcp_only", label: "TCP only" },
                  { value: "block", label: "Block" },
                ]}
              />
            </label>
            <label>
              <span className="lbl">Do Not Track</span>
              <CSSelect
                value={f.do_not_track ? "1" : "0"}
                onChange={(v) => u("do_not_track", v === "1")}
                options={[
                  { value: "0", label: "Off" },
                  { value: "1", label: "On (send DNT: 1)" },
                ]}
              />
            </label>
          </div>

          <label>
            <span className="lbl">Geolocation</span>
            <div className="seg seg-2">
              {(["auto", "manual"] as GeoMode[]).map((m) => (
                <button key={m} className={`seg-btn ${f.geo_mode === m ? "active" : ""}`} onClick={() => u("geo_mode", m)}>
                  {m === "auto" ? "Auto (from proxy)" : "Manual coords"}
                </button>
              ))}
            </div>
          </label>
          {f.geo_mode === "manual" && (
            <div className="form-row form-row-3">
              <NumField label="Latitude" value={f.geo_lat} onChange={(v) => u("geo_lat", v)} step={0.0001} />
              <NumField label="Longitude" value={f.geo_lng} onChange={(v) => u("geo_lng", v)} step={0.0001} />
              <NumField label="Accuracy m" value={f.geo_accuracy} onChange={(v) => u("geo_accuracy", v)} />
            </div>
          )}

          <div className="ie-section-title" style={{ marginTop: 10 }}>Media devices</div>
          <div className="form-row form-row-3">
            <SelectField label="Mic in" value={f.media_audio_in} onChange={(v) => u("media_audio_in", v)} options={MEDIA_COUNT_OPTIONS} />
            <SelectField label="Speakers" value={f.media_audio_out} onChange={(v) => u("media_audio_out", v)} options={MEDIA_COUNT_OPTIONS} />
            <SelectField label="Webcam" value={f.media_video_in} onChange={(v) => u("media_video_in", v)} options={MEDIA_COUNT_OPTIONS} />
          </div>

          <label>
            <span className="lbl">Notes</span>
            <textarea rows={2} value={f.notes} onChange={(e) => u("notes", e.target.value)} placeholder="Free-form notes…" />
          </label>
        </div>
      </div>
      <div className="ie-foot">
        <button className="btn-ghost" onClick={onCancel}>Cancel</button>
        <button className="btn-primary" onClick={onSave}>
          <ShardMini /> {f.id ? "Save changes" : "Create profile"}
        </button>
      </div>
    </div>
  );
}

function ShardMini() {
  return <svg width="12" height="12" viewBox="0 0 12 12"><path d="M6 1L11 6L6 11L1 6Z" fill="currentColor" /></svg>;
}

// ---- shared inputs ----

type FieldProps = {
  label: string;
  value: string;
  onChange: (v: string) => void;
  type?: string;
  placeholder?: string;
  mono?: boolean;
};
function Field({ label, value, onChange, type = "text", placeholder, mono }: FieldProps) {
  return (
    <label>
      <span className="lbl">{label}</span>
      <input className={mono ? "mono" : ""} type={type} value={value} placeholder={placeholder} onChange={(e) => onChange(e.target.value)} />
    </label>
  );
}

function NumField({ label, value, onChange, step }: { label: string; value: number; onChange: (v: number) => void; step?: number }) {
  return (
    <label>
      <span className="lbl">{label}</span>
      <input type="number" step={step ?? 1} value={value} onChange={(e) => onChange(parseFloat(e.target.value) || 0)} />
    </label>
  );
}

function Pair({
  label, value, on, blockLabel, onText,
}: {
  label: string;
  value: NoiseMode;
  on: (v: NoiseMode) => void;
  /// Allow/Block labels instead of Real/Auto (used by Ports).
  blockLabel?: boolean;
  /// Custom "on" label (default "Auto noise"; Fonts passes "Noise").
  onText?: string;
}) {
  const opts: NoiseMode[] = ["real", "auto"];
  const labelFor = (o: NoiseMode) =>
    blockLabel
      ? (o === "real" ? "Allow" : "Block")
      : (o === "real" ? "Real" : (onText ?? "Auto noise"));
  return (
    <label>
      <span className="lbl">{label}</span>
      <div className="tri tri-2">
        {opts.map((o) => (
          <button key={o} className={`tri-btn ${value === o ? "active" : ""}`} onClick={() => on(o)}>
            {labelFor(o)}
          </button>
        ))}
      </div>
    </label>
  );
}

function PortList({
  label, value, onChange,
}: {
  label: string;
  value: number[];
  onChange: (v: number[]) => void;
}) {
  const [text, setText] = useState("");
  const commit = () => {
    // Accept "3389", "3389, 5900", "3389 5900"; drops non-numeric tokens.
    const toks = text.split(/[\s,]+/).filter(Boolean);
    if (toks.length === 0) return;
    const next = new Set(value);
    for (const t of toks) {
      const n = parseInt(t, 10);
      if (Number.isFinite(n) && n >= 1 && n <= 65535) next.add(n);
    }
    onChange([...next].sort((a, b) => a - b));
    setText("");
  };
  const remove = (p: number) => onChange(value.filter((x) => x !== p));
  return (
    <label className="port-list-wrap">
      <span className="lbl">{label}</span>
      <div className="port-list">
        {value.map((p) => (
          <span key={p} className="port-chip">
            <span>{p}</span>
            <button type="button" className="port-chip-x" onClick={() => remove(p)} title="Remove">✕</button>
          </span>
        ))}
        <input
          type="text"
          inputMode="numeric"
          className="port-input"
          value={text}
          onChange={(e) => setText(e.target.value)}
          onKeyDown={(e) => { if (e.key === "Enter" || e.key === "," || e.key === " ") { e.preventDefault(); commit(); } }}
          onBlur={commit}
          placeholder={value.length === 0 ? "e.g. 3389, 5900, 8080" : "add port…"}
        />
      </div>
    </label>
  );
}

type CSOption<T> = { value: T; label: string };

/// Themed dropdown replacing the native select inside the profile editor.
function CSSelect<T extends string | number>({
  value, options, onChange, placeholder,
}: {
  value: T;
  options: CSOption<T>[];
  onChange: (v: T) => void;
  placeholder?: string;
}) {
  const [open, setOpen] = useState(false);
  const triggerRef = useRef<HTMLButtonElement>(null);
  const menuRef = useRef<HTMLDivElement>(null);
  // Body portal so ancestor overflow:hidden can't clip the menu.
  const [anchor, setAnchor] = useState<{ left: number; top: number; width: number; up: boolean } | null>(null);

  const place = () => {
    const el = triggerRef.current;
    if (!el) return;
    const r = el.getBoundingClientRect();
    const menuH = Math.min(280, options.length * 38 + 10);
    // Flip up if no room below.
    const up = r.bottom + menuH + 8 > window.innerHeight && r.top - menuH - 8 > 0;
    setAnchor({
      left: r.left,
      top: up ? r.top - menuH - 4 : r.bottom + 4,
      width: r.width,
      up,
    });
  };

  const toggle = () => {
    if (!open) place();
    setOpen((v) => !v);
  };

  useEffect(() => {
    if (!open) return;
    const onDoc = (e: MouseEvent) => {
      const t = e.target as Node;
      if (triggerRef.current?.contains(t) || menuRef.current?.contains(t)) return;
      setOpen(false);
    };
    const onKey = (e: KeyboardEvent) => { if (e.key === "Escape") setOpen(false); };
    // Re-anchor on page scroll/resize; ignore scrolls inside the menu.
    const onScroll = (e: Event) => {
      if (menuRef.current && menuRef.current.contains(e.target as Node)) return;
      place();
    };
    document.addEventListener("mousedown", onDoc);
    document.addEventListener("keydown", onKey);
    window.addEventListener("resize", place);
    window.addEventListener("scroll", onScroll, true);
    return () => {
      document.removeEventListener("mousedown", onDoc);
      document.removeEventListener("keydown", onKey);
      window.removeEventListener("resize", place);
      window.removeEventListener("scroll", onScroll, true);
    };
  }, [open]);

  const current = options.find((o) => o.value === value);
  return (
    <div className={`cs ${open ? "cs-open" : ""}`}>
      <button ref={triggerRef} type="button" className="cs-trigger" onClick={toggle}>
        <span className="cs-value">{current?.label ?? placeholder ?? ""}</span>
        <span className="cs-caret" aria-hidden>▾</span>
      </button>
      {open && anchor && createPortal(
        <div
          ref={menuRef}
          className="cs-menu"
          role="listbox"
          style={{ left: anchor.left, top: anchor.top, width: anchor.width }}
        >
          {options.map((o) => (
            <div
              key={String(o.value)}
              role="option"
              aria-selected={o.value === value}
              className={`cs-opt ${o.value === value ? "active" : ""}`}
              onClick={() => { onChange(o.value); setOpen(false); }}
            >
              {o.label}
            </div>
          ))}
        </div>,
        document.body,
      )}
    </div>
  );
}

function SelectField<T extends string | number>({
  label, value, onChange, options, format,
}: {
  label: string;
  value: T;
  onChange: (v: T) => void;
  options: readonly T[];
  format?: (v: T) => string;
}) {
  const opts: CSOption<T>[] = options.map((o) => ({
    value: o,
    label: format ? format(o) : String(o),
  }));
  return (
    <label>
      <span className="lbl">{label}</span>
      <CSSelect value={value} options={opts} onChange={onChange} />
    </label>
  );
}

// ---- topbar + metrics ----

function Topbar({ crumbs, search, onSearch }: { crumbs: string[]; search: string; onSearch: (v: string) => void }) {
  return (
    <div className="topbar">
      <div className="crumbs">
        {crumbs.map((c, i) => (
          <span key={i}>
            {i > 0 && <span className="sep">›</span>}
            <span className={i === crumbs.length - 1 ? "crumb-now" : ""}>{c}</span>
          </span>
        ))}
      </div>
      <div className="search">
        <span className="search-icon">⌕</span>
        <input placeholder="Search…   ⌘K" value={search} onChange={(e) => onSearch(e.target.value)} />
      </div>
    </div>
  );
}

function Metric({ label, value, accent, pulse }: { label: string; value: string; accent?: boolean; pulse?: boolean }) {
  return (
    <div className={`metric ${accent ? "metric-accent" : ""}`}>
      <div className="m-k">{label}</div>
      <div className={`m-v ${pulse ? "m-v-pulse" : ""}`}>{value}</div>
    </div>
  );
}

// ---- Proxies ----

type ProxyTestSnapshot = {
  first_seen: string;
  last_seen: string;
  ip: string;
  country_code: string;
  country: string;
  region: string;
  city: string;
  isp: string;
  timezone: string;
  latitude: number;
  longitude: number;
  tcp_ms: number | null;
  udp_ms: number | null;
  udp_error: string | null;
  provider: string;
};

function ProxiesView() {
  const [proxies, setProxies] = useState<ProxyEntry[]>([]);
  const [editing, setEditing] = useState<ProxyEntry | null>(null);
  const [bulkOpen, setBulkOpen] = useState(false);
  const [snapshots, setSnapshots] = useState<Record<string, ProxyTestSnapshot>>({});
  const [busy, setBusy] = useState<Record<string, boolean>>({});
  const [infoFor, setInfoFor] = useState<{ proxy: ProxyEntry; anchor: { x: number; y: number } } | null>(null);
  const [proxySel, setProxySel] = useState<Set<string>>(new Set());
  const [renaming, setRenaming] = useState<{ id: string; draft: string } | null>(null);
  const [profiles, setProfiles] = useState<ProfileMeta[]>([]);
  const [search, setSearch] = useState("");
  const ctx = useContextMenu();

  // Search filter: matches name / host / port / country tag / notes / username
  // *and* the exit IP from the latest snapshot (so the user can find a proxy
  // by "last seen exiting at X.X.X.X").  Whitespace-trimmed, case-insensitive.
  const filteredProxies = useMemo(() => {
    const q = search.trim().toLowerCase();
    if (!q) return proxies;
    return proxies.filter((p) => {
      const ip = (snapshots[p.id]?.ip ?? "").toLowerCase();
      const city = (snapshots[p.id]?.city ?? "").toLowerCase();
      const isp = (snapshots[p.id]?.isp ?? "").toLowerCase();
      return (
        p.name.toLowerCase().includes(q) ||
        p.host.toLowerCase().includes(q) ||
        String(p.port).includes(q) ||
        p.country.toLowerCase().includes(q) ||
        p.notes.toLowerCase().includes(q) ||
        p.username.toLowerCase().includes(q) ||
        ip.includes(q) ||
        city.includes(q) ||
        isp.includes(q)
      );
    });
  }, [proxies, snapshots, search]);

  // Pagination over the filtered list.
  const PROXY_PAGE_SIZE = 20;
  const [proxyPage, setProxyPage] = useState(1);
  const proxyPageCount = Math.max(1, Math.ceil(filteredProxies.length / PROXY_PAGE_SIZE));
  useEffect(() => {
    if (proxyPage > proxyPageCount) setProxyPage(proxyPageCount);
  }, [proxyPageCount, proxyPage]);
  // Reset to page 1 when the search narrows the list to fewer pages.
  useEffect(() => { setProxyPage(1); }, [search]);
  const pagedProxies = useMemo(
    () => filteredProxies.slice((proxyPage - 1) * PROXY_PAGE_SIZE, proxyPage * PROXY_PAGE_SIZE),
    [filteredProxies, proxyPage],
  );

  const commitRename = async () => {
    if (!renaming) return;
    const entry = proxies.find((p) => p.id === renaming.id);
    if (!entry) { setRenaming(null); return; }
    const newName = renaming.draft.trim();
    if (newName === entry.name) { setRenaming(null); return; }
    try {
      await invoke("proxy_save", { entry: { ...entry, name: newName } });
      setRenaming(null);
      reload();
    } catch (e) { toast.err(String(e)); }
  };

  const reload = async () => {
    try {
      setProxies(await invoke<ProxyEntry[]>("proxy_list"));
      // Profile list powers the per-proxy bound-count column.
      setProfiles(await invoke<ProfileMeta[]>("profile_list"));
    } catch (e) { toast.err(String(e)); }
  };
  useEffect(() => { reload(); }, []);

  // proxy_id → bound-profile count (O(n) tally; n is small).
  const profileCountByProxy = useMemo(() => {
    const out: Record<string, number> = {};
    for (const p of profiles) {
      if (p.proxy_id) out[p.proxy_id] = (out[p.proxy_id] ?? 0) + 1;
    }
    return out;
  }, [profiles]);

  // Fetch latest snapshot per proxy so rows survive a launcher restart.
  useEffect(() => {
    if (proxies.length === 0) return;
    let cancelled = false;
    (async () => {
      const entries = await Promise.all(
        proxies.map(async (p) => {
          try {
            const snap = await invoke<ProxyTestSnapshot | null>("proxy_last_test", { id: p.id });
            return [p.id, snap] as const;
          } catch {
            return [p.id, null] as const;
          }
        }),
      );
      if (cancelled) return;
      const next: Record<string, ProxyTestSnapshot> = {};
      for (const [id, snap] of entries) if (snap) next[id] = snap;
      setSnapshots(next);
    })();
    return () => { cancelled = true; };
  }, [proxies]);

  const fullTest = async (p: ProxyEntry) => {
    setBusy((s) => ({ ...s, [p.id]: true }));
    try {
      const snap = await invoke<ProxyTestSnapshot>("proxy_full_test", { entry: p });
      setSnapshots((s) => ({ ...s, [p.id]: snap }));
      // Refresh: backend may have just populated the country tag.
      reload();
    } catch (e) {
      toast.err(`${p.name || p.host}: ${e}`);
    } finally {
      setBusy((s) => ({ ...s, [p.id]: false }));
    }
  };

  const remove = async (id: string) => {
    if ((await confirmModal({ title: "Delete proxy", message: "Delete this proxy?", danger: true })) !== true) return;
    try { await invoke("proxy_delete", { id }); reload(); toast.ok("Proxy deleted"); }
    catch (e) { toast.err(String(e)); }
  };

  // Capped-parallel bulk TCP/UDP/geo to avoid socket fan-out.
  const bulkTest = async () => {
    const ids = [...proxySel];
    if (ids.length === 0) return;
    toast.info(`Testing ${ids.length} prox${ids.length === 1 ? "y" : "ies"}…`);
    const targets = proxies.filter((p) => proxySel.has(p.id));
    const CONCURRENCY = 5;
    let i = 0;
    await Promise.all(
      Array.from({ length: Math.min(CONCURRENCY, targets.length) }, async () => {
        while (i < targets.length) {
          const p = targets[i++];
          if (!p) break;
          await fullTest(p);
        }
      }),
    );
    toast.ok("Bulk test done");
  };

  const bulkDelete = async () => {
    const ids = [...proxySel];
    if (ids.length === 0) return;
    if ((await confirmModal({ title: "Delete proxies", message: `Delete ${ids.length} prox${ids.length === 1 ? "y" : "ies"}?`, danger: true })) !== true) return;
    for (const id of ids) {
      try { await invoke("proxy_delete", { id }); } catch (e) { toast.err(String(e)); }
    }
    setProxySel(new Set());
    reload();
    toast.ok(`Deleted ${ids.length}`);
  };

  // Export in bulk-import format so round-trip preserves country tag.
  const bulkExport = () => {
    const targets = proxies.filter((p) => proxySel.has(p.id));
    if (targets.length === 0) return;
    const lines = targets.map((p) => {
      const auth = p.username || p.password ? `${p.username}:${p.password}@` : "";
      const base = `${p.kind}://${auth}${p.host}:${p.port}`;
      const tag = p.country ? `  # country=${p.country}` : "";
      return base + tag;
    });
    const text = lines.join("\n");
    clip.write(text).then(
      () => toast.ok(`Copied ${targets.length} to clipboard`),
      (e) => toast.err("Copy failed: " + String(e)),
    );
  };

  // Import from clipboard (one per line, bulkExport format).
  const bulkImportClipboard = async () => {
    try {
      const text = await clip.read();
      if (!text.trim()) { toast.err("Clipboard is empty"); return; }
      const n = await invoke<number>("proxy_bulk_import", { text, kind: "socks5" });
      reload();
      toast.ok(`Imported ${n} prox${n === 1 ? "y" : "ies"}`);
    } catch (e) { toast.err("Import failed: " + String(e)); }
  };

  return (
    <section className="page">
      <Topbar crumbs={["Workspace", "Proxies"]} search={search} onSearch={setSearch} />
      <div className="page-title">
        <h1>Proxies</h1>
        <div className="page-actions">
          {proxySel.size > 0 && (
            <div className="bulk-bar">
              <span>{proxySel.size} selected</span>
              <button className="btn-ghost btn-sm" onClick={bulkTest}><Icon.Refresh /> Test</button>
              <button className="btn-ghost btn-sm" onClick={bulkExport}><Icon.Upload /> Export</button>
              <button className="btn-ghost btn-sm" onClick={bulkDelete}><Icon.Trash /> Delete</button>
              <button className="btn-ghost btn-sm" onClick={() => setProxySel(new Set())}>Clear</button>
            </div>
          )}
          {/* Promo: routes to ProxyShard's UDP / p0f-spoofed residential
              pool — the proxies that actually make ShardX's QUIC +
              WebRTC stack work end-to-end.  Sits next to Import / New
              proxy so it's discoverable without opening any dialog. */}
          <button
            className="proxy-buy-cta"
            onClick={() => { openUrl(withUtm("https://proxyshard.com")).catch(() => {}); }}
            title="Open proxyshard.com — residential SOCKS5 with UDP_ASSOCIATE + p0f-spoofed exit"
          >
            <ShardMini /> Buy proxies <span className="muted">— UDP + p0f</span>
          </button>
          <button className="btn-ghost" onClick={bulkImportClipboard} title="Import proxies from the clipboard"><Icon.Download /> Import</button>
          <button className="btn-primary" onClick={() => setBulkOpen(true)}>+ New proxy</button>
        </div>
      </div>
      <div className="rows">
        <div className="rows-head p-cols">
          <div>
            <input
              type="checkbox"
              title="Select all on this page"
              // Page-only header toggle (matches profile table behaviour).
              checked={pagedProxies.length > 0 && pagedProxies.every((p) => proxySel.has(p.id))}
              ref={(el) => {
                if (!el) return;
                const any = pagedProxies.some((p) => proxySel.has(p.id));
                const all = pagedProxies.length > 0 && pagedProxies.every((p) => proxySel.has(p.id));
                el.indeterminate = any && !all;
              }}
              onChange={(e) => {
                setProxySel((prev) => {
                  const next = new Set(prev);
                  if (e.target.checked) {
                    for (const p of pagedProxies) next.add(p.id);
                  } else {
                    for (const p of pagedProxies) next.delete(p.id);
                  }
                  return next;
                });
              }}
            />
          </div>
          <div>Name</div><div>Type</div><div>Host:Port</div><div>Country</div><div>Profiles</div><div>Test result</div><div></div>
        </div>
        {pagedProxies.map((p) => {
          const r = snapshots[p.id];
          const isBusy = !!busy[p.id];
          const cc = r?.country_code || p.country || "";
          const isSel = proxySel.has(p.id);
          return (
            <div
              key={p.id}
              className="row-wrap"
              onContextMenu={(e) =>
                ctx.open(e, [
                  { label: "Test (TCP/UDP/geo)", onClick: () => fullTest(p) },
                  { label: "View details", onClick: () => setInfoFor({ proxy: p, anchor: { x: e.clientX, y: e.clientY } }) },
                  { label: "Edit", onClick: () => setEditing(p) },
                  { sep: true, label: "", onClick: () => {} },
                  { label: "Delete", onClick: () => remove(p.id), danger: true },
                ])
              }
            >
              <div className="row p-cols">
                <div>
                  <input
                    type="checkbox"
                    checked={isSel}
                    onChange={() => {
                      setProxySel((s) => {
                        const n = new Set(s);
                        if (n.has(p.id)) n.delete(p.id); else n.add(p.id);
                        return n;
                      });
                    }}
                  />
                </div>
                <div className="cell-name">
                  {renaming?.id === p.id ? (
                    <input
                      autoFocus
                      className="inline-rename"
                      value={renaming.draft}
                      onChange={(e) => setRenaming({ id: p.id, draft: e.target.value })}
                      onBlur={commitRename}
                      onKeyDown={(e) => {
                        if (e.key === "Enter") commitRename();
                        else if (e.key === "Escape") setRenaming(null);
                      }}
                    />
                  ) : (
                    <span
                      className="cell-click"
                      onClick={() => setRenaming({ id: p.id, draft: p.name })}
                      title="Click to rename"
                    >
                      {p.name || "—"}
                    </span>
                  )}
                </div>
                <div><span className={`badge badge-${p.kind}`}>{p.kind}</span></div>
                <div className="cell-hostport">
                  <span className="mono small cell-click" onClick={() => setEditing(p)} title="Edit proxy">
                    {p.host}:{p.port}
                  </span>
                </div>
                <div>
                  {cc ? (
                    <span className="proxy-country">
                      <CountryFlag cc={cc} />
                      <span className="flag">{cc}</span>
                      {r?.city && <span className="muted small">{r.city}</span>}
                    </span>
                  ) : <span className="muted small">—</span>}
                </div>
                <div>
                  <span className="profile-count" title={`${profileCountByProxy[p.id] ?? 0} profile(s) bound to this proxy`}>
                    {profileCountByProxy[p.id] ?? 0}
                  </span>
                </div>
                <div className="proxy-test-cell">
                  {!r && !isBusy && <span className="muted small">not tested</span>}
                  {isBusy && <span className="muted small">testing…</span>}
                  {r && !isBusy && (
                    <div className="proxy-test">
                      <span
                        className={`status-pill ${r.tcp_ms != null ? "status-active" : "status-failed"}`}
                        title={r.tcp_ms != null ? `TCP ${r.tcp_ms} ms` : "TCP failed"}
                      >
                        {r.tcp_ms != null ? "Active" : "Failed"}
                      </span>
                      {/* UDP pill: clickable to docs explaining what the
                          presence/absence of UDP means for QUIC + WebRTC.
                          Shown for any proxy type — HTTP proxies never
                          have UDP, but the badge still tells the user why
                          QUIC will be force-disabled at launch. */}
                      {r.udp_ms != null && p.kind === "socks5" && (
                        <button
                          type="button"
                          className="status-pill status-udp status-link"
                          title={`UDP relay works (${r.udp_ms} ms) — QUIC enabled at launch. Click for docs.`}
                          onClick={() => { openUrl(UDP_DOCS_URL).catch(() => {}); }}
                        >
                          UDP
                        </button>
                      )}
                      {r.udp_ms == null && (
                        <button
                          type="button"
                          className="status-pill status-no-udp status-link"
                          title="No UDP support — QUIC/HTTP-3 disabled at launch. Click for docs."
                          onClick={() => { openUrl(UDP_DOCS_URL).catch(() => {}); }}
                        >
                          UDP
                        </button>
                      )}
                      {r.tcp_ms != null && r.ip && (
                        <span className="test-ip mono small" title={r.isp}>{r.ip}</span>
                      )}
                    </div>
                  )}
                </div>
                <div className="row-actions">
                  <button
                    className="icon-btn"
                    onClick={(e) => setInfoFor({ proxy: p, anchor: { x: e.clientX, y: e.clientY } })}
                    title="Details + history"
                  ><Icon.Info /></button>
                  <button className="icon-btn" onClick={() => fullTest(p)} disabled={isBusy} title="Test TCP + UDP + geo"><Icon.Refresh /></button>
                  <button className="icon-btn" onClick={() => setEditing(p)} title="Edit"><Icon.Edit /></button>
                  <button className="icon-btn danger" onClick={() => remove(p.id)} title="Delete"><Icon.Trash /></button>
                </div>
              </div>
            </div>
          );
        })}
        {proxies.length === 0 && (
          <div className="empty-rich">
            <div className="empty-shard"><IconWire /></div>
            <h3>No proxies yet</h3>
            <p>Add a SOCKS5/HTTP(S) endpoint so profiles can route through it.</p>
            <div className="empty-cta">
              <button className="btn-primary" onClick={() => setBulkOpen(true)}>+ New proxy</button>
            </div>
          </div>
        )}
      </div>
      {proxyPageCount > 1 && (
        <div className="pager">
          <button className="btn-ghost btn-sm" disabled={proxyPage <= 1}
            onClick={() => setProxyPage((p) => Math.max(1, p - 1))}>‹ Prev</button>
          <span className="pager-info">Page {proxyPage} of {proxyPageCount} · {proxies.length} proxies</span>
          <button className="btn-ghost btn-sm" disabled={proxyPage >= proxyPageCount}
            onClick={() => setProxyPage((p) => Math.min(proxyPageCount, p + 1))}>Next ›</button>
        </div>
      )}
      {editing && <ProxyEditor initial={editing} onClose={() => { setEditing(null); reload(); }} />}
      {bulkOpen && <ProxyBulkImporter onClose={() => { setBulkOpen(false); reload(); }} />}
      {infoFor && (
        <ProxyInfoPopover
          proxy={infoFor.proxy}
          anchor={infoFor.anchor}
          latest={snapshots[infoFor.proxy.id]}
          onClose={() => setInfoFor(null)}
        />
      )}
      {ctx.node}
    </section>
  );
}

/// Proxy detail popover: latest IP/geo + UDP + IP-change history.
function ProxyInfoPopover({
  proxy, anchor, latest, onClose,
}: {
  proxy: ProxyEntry;
  anchor: { x: number; y: number };
  latest?: ProxyTestSnapshot;
  onClose: () => void;
}) {
  const [history, setHistory] = useState<ProxyTestSnapshot[]>([]);
  useEffect(() => {
    invoke<ProxyTestSnapshot[]>("proxy_history", { id: proxy.id })
      .then((h) => setHistory([...h].reverse()))
      .catch((e) => toast.err(String(e)));
  }, [proxy.id]);
  useEffect(() => {
    const onDoc = (e: MouseEvent) => {
      const t = e.target as HTMLElement;
      if (!t.closest(".proxy-popover")) onClose();
    };
    window.addEventListener("mousedown", onDoc);
    return () => window.removeEventListener("mousedown", onDoc);
  }, [onClose]);

  // Clamp inside viewport to avoid clipping at the right edge.
  const left = Math.min(anchor.x, window.innerWidth - 360);
  const top = Math.min(anchor.y + 8, window.innerHeight - 320);

  return (
    <div className="proxy-popover" style={{ left, top }} onClick={(e) => e.stopPropagation()}>
      <div className="popover-section">
        {latest?.ip ? (
          <>
            <div className="pop-row">
              <span className="pop-ico"><Icon.Globe /></span>
              <span className="mono">{latest.ip}</span>
            </div>
            <div className="pop-row">
              <span className="pop-ico">{latest.country_code ? <CountryFlag cc={latest.country_code} height={14} /> : <Icon.Globe />}</span>
              <span>{[latest.region, latest.city].filter(Boolean).join(", ") || latest.country || "—"}</span>
            </div>
            {latest.timezone && (
              <div className="pop-row">
                <span className="pop-ico"><Icon.Clock /></span>
                <span>{latest.timezone}</span>
              </div>
            )}
            {latest.isp && (
              <div className="pop-row">
                <span className="pop-ico"><Icon.Building /></span>
                <span className="muted small">{latest.isp}</span>
              </div>
            )}
            <div className="pop-row pop-row-split">
              <span className={`pop-pill ${latest.tcp_ms != null ? "ok" : "err"}`}>
                TCP {latest.tcp_ms != null ? `${latest.tcp_ms} ms` : "✗"}
              </span>
              {proxy.kind === "socks5" && (
                <span
                  className={`pop-pill ${latest.udp_ms != null ? "ok" : "err"}`}
                  title={latest.udp_error ?? undefined}
                >
                  UDP {latest.udp_ms != null ? `${latest.udp_ms} ms` : "✗"}
                </span>
              )}
            </div>
          </>
        ) : (
          <div className="muted small">Not tested yet — click ↻ on the row.</div>
        )}
      </div>
      <div className="popover-divider">IP HISTORY</div>
      <div className="popover-history">
        {history.length === 0 && <div className="muted small" style={{ padding: "10px 0" }}>No history yet</div>}
        {history.map((s, i) => (
          <div key={`${s.ip}-${s.first_seen}-${i}`} className="history-item">
            <div className="hi-head">
              <span className="mono">{s.ip || "—"}</span>
              {s.country_code && (
                <>
                  <CountryFlag cc={s.country_code} />
                  <span className="flag">{s.country_code}</span>
                </>
              )}
              {s.city && <span className="muted small">{s.city}</span>}
            </div>
            <div className="hi-meta muted small">
              {fmtTs(s.first_seen)}
              {s.first_seen !== s.last_seen && <> → {fmtTs(s.last_seen)}</>}
              {s.udp_ms != null && <> · UDP ✓</>}
              {s.udp_error && <> · UDP ✗</>}
            </div>
          </div>
        ))}
      </div>
    </div>
  );
}

/// Flat rectangular country flag (flag-icons sprite); empty input renders nothing.
function CountryFlag({ cc, height = 17 }: { cc: string; height?: number }) {
  if (!cc || cc.length !== 2 || !/^[a-zA-Z]{2}$/.test(cc)) return null;
  const code = cc.toLowerCase();
  // `fi fi-XX`; omit `fis` to keep 4:3 rectangle.
  return (
    <span
      className={`fi fi-${code} flag-sq`}
      style={{ height, width: Math.round(height * 4 / 3) }}
      aria-hidden
    />
  );
}

/// "@1700000000" → "May 26, 14:30" (UTC for cross-timezone consistency).
function fmtTs(stamp: string): string {
  if (!stamp.startsWith("@")) return stamp;
  const n = parseInt(stamp.slice(1), 10);
  if (!Number.isFinite(n)) return stamp;
  const d = new Date(n * 1000);
  return d.toLocaleString(undefined, { month: "short", day: "numeric", hour: "2-digit", minute: "2-digit" });
}

/// Format ms uptime as "1h 23m" / "12m 30s" / "45s".
function fmtUptime(ms: number): string {
  const s = Math.max(0, Math.floor(ms / 1000));
  const h = Math.floor(s / 3600);
  const m = Math.floor((s % 3600) / 60);
  const sec = s % 60;
  if (h > 0) return `${h}h ${m}m`;
  if (m > 0) return `${m}m ${sec.toString().padStart(2, "0")}s`;
  return `${sec}s`;
}

type BulkRowState = {
  entry: ProxyEntry;
  selected: boolean;
  status: "idle" | "testing" | "ok" | "fail";
  tcp_ms?: number | null;
  udp_ms?: number | null;
  country?: string;
  error?: string;
};

/// Two-step bulk import: paste → parse → (optional Test-all + dedup) → Save selected.
function ProxyBulkImporter({ onClose }: { onClose: () => void }) {
  const [text, setText] = useState("");
  const [kind, setKind] = useState<ProxyEntry["kind"]>("socks5");
  const [rows, setRows] = useState<BulkRowState[]>([]);
  const [busy, setBusy] = useState(false);

  const parse = async () => {
    if (!text.trim()) { toast.err("Nothing to parse"); return; }
    try {
      const parsed = await invoke<ProxyEntry[]>("proxy_bulk_parse", { text, kind });
      if (parsed.length === 0) { toast.err("No valid proxy lines found"); return; }
      setRows(parsed.map((e) => ({ entry: e, selected: true, status: "idle" })));
    } catch (e) { toast.err(String(e)); }
  };

  const testOne = async (idx: number) => {
    setRows((rs) => rs.map((r, i) => i === idx ? { ...r, status: "testing" } : r));
    const entry = rows[idx]?.entry;
    if (!entry) return;
    try {
      const snap = await invoke<ProxyTestSnapshot>("proxy_full_test", { entry });
      setRows((rs) => rs.map((r, i) =>
        i === idx
          ? {
              ...r,
              status: snap.tcp_ms != null ? "ok" : "fail",
              tcp_ms: snap.tcp_ms,
              udp_ms: snap.udp_ms,
              country: snap.country_code || r.country,
              entry: { ...r.entry, country: snap.country_code || r.entry.country },
            }
          : r,
      ));
    } catch (e) {
      setRows((rs) => rs.map((r, i) => i === idx ? { ...r, status: "fail", error: String(e) } : r));
    }
  };

  const testAll = async () => {
    setBusy(true);
    const CONCURRENCY = 5;
    const queue = rows.map((_, i) => i);
    let cursor = 0;
    await Promise.all(
      Array.from({ length: Math.min(CONCURRENCY, queue.length) }, async () => {
        while (cursor < queue.length) {
          const i = queue[cursor++];
          if (i == null) break;
          await testOne(i);
        }
      }),
    );
    setBusy(false);
  };

  const saveSelected = async () => {
    const entries = rows.filter((r) => r.selected).map((r) => r.entry);
    if (entries.length === 0) { toast.err("Nothing selected"); return; }
    try {
      const n = await invoke<number>("proxy_bulk_save", { entries });
      toast.ok(`Imported ${n} prox${n === 1 ? "y" : "ies"}`);
      onClose();
    } catch (e) { toast.err(String(e)); }
  };

  const allSel = rows.length > 0 && rows.every((r) => r.selected);
  const selCount = rows.filter((r) => r.selected).length;

  return (
    <div className="dialog-bg" onClick={onClose}>
      <div className="dialog dialog-wide" onClick={(e) => e.stopPropagation()}>
        <header className="dialog-head">
          <h2><ShardMini /> Bulk import proxies</h2>
          <button className="icon-btn" onClick={onClose}>✕</button>
        </header>
        <div className="dialog-body">
          {rows.length === 0 ? (
            <>
              <label>
                <span className="lbl">Default type (used when a line has no scheme)</span>
                <select value={kind} onChange={(e) => setKind(e.target.value as ProxyEntry["kind"])}>
                  <option value="socks5">SOCKS5</option>
                  <option value="http">HTTP</option>
                  <option value="https">HTTPS</option>
                </select>
              </label>
              <label>
                <span className="lbl">Paste one proxy per line</span>
                <textarea
                  rows={12}
                  className="mono"
                  value={text}
                  onChange={(e) => setText(e.target.value)}
                  placeholder={`socks5://user:pass@host:1080
user:pass@host:1080
host:1080:user:pass     # country=PL
host:8080               # no auth
# lines starting with # are ignored`}
                />
              </label>
              <p className="muted small">
                Duplicates (same host:port:user) are skipped on save.
              </p>
            </>
          ) : (
            <>
              <div className="bulk-preview-head">
                <label className="bulk-preview-checkall">
                  <input
                    type="checkbox"
                    checked={allSel}
                    onChange={(e) =>
                      setRows((rs) => rs.map((r) => ({ ...r, selected: e.target.checked })))
                    }
                  />
                  <span>{selCount} of {rows.length} selected</span>
                </label>
                <div style={{ marginLeft: "auto", display: "flex", gap: 6 }}>
                  <button className="btn-ghost btn-sm" onClick={() => setRows([])}>← Back</button>
                  <button className="btn-ghost btn-sm" onClick={testAll} disabled={busy}>
                    {busy ? "Testing…" : <><Icon.Refresh /> Test all</>}
                  </button>
                  <button
                    className="btn-ghost btn-sm"
                    onClick={() =>
                      setRows((rs) =>
                        rs.map((r) => ({ ...r, selected: r.status === "ok" }))
                      )
                    }
                    title="Tick only proxies whose latest test succeeded"
                  >
                    ✓ Keep working only
                  </button>
                </div>
              </div>
              <div className="bulk-preview-list">
                {rows.map((r, i) => (
                  <div key={`${r.entry.host}:${r.entry.port}:${i}`} className={`bulk-row bulk-row-${r.status}`}>
                    <input
                      type="checkbox"
                      checked={r.selected}
                      onChange={() =>
                        setRows((rs) => rs.map((x, j) => j === i ? { ...x, selected: !x.selected } : x))
                      }
                    />
                    <span className={`badge badge-${r.entry.kind}`}>{r.entry.kind}</span>
                    <span className="mono small bulk-host" title={`${r.entry.host}:${r.entry.port}${r.entry.username ? " @" + r.entry.username : ""}`}>
                      {r.entry.host}:{r.entry.port}
                      {r.entry.username && <span className="muted"> · {r.entry.username}</span>}
                    </span>
                    <div className="bulk-status">
                      {r.status === "idle" && <span className="muted small">not tested</span>}
                      {r.status === "testing" && <span className="muted small">testing…</span>}
                      {r.status === "ok" && (
                        <>
                          <span className="status-pill status-active" title={`TCP ${r.tcp_ms} ms`}>Active</span>
                          {r.entry.kind === "socks5" && r.udp_ms != null && (
                            <span className="status-pill status-udp" title={`UDP relay works (${r.udp_ms} ms)`}>UDP</span>
                          )}
                          {r.country && (
                            <>
                              <CountryFlag cc={r.country} />
                              <span className="flag">{r.country}</span>
                            </>
                          )}
                        </>
                      )}
                      {r.status === "fail" && (
                        <span className="status-pill status-failed" title={r.error}>Failed</span>
                      )}
                    </div>
                    <button className="btn-sm btn-ghost icon-only" onClick={() => testOne(i)} disabled={r.status === "testing"} title="Test this row"><Icon.Refresh /></button>
                  </div>
                ))}
              </div>
            </>
          )}
        </div>
        <footer className="dialog-foot">
          <button className="btn-ghost" onClick={onClose}>Cancel</button>
          {rows.length === 0 ? (
            <button className="btn-primary" onClick={parse}><ShardMini /> Parse →</button>
          ) : (
            <button className="btn-primary" onClick={saveSelected}>
              <ShardMini /> Import {selCount}
            </button>
          )}
        </footer>
      </div>
    </div>
  );
}

function ProxyEditor({ initial, onClose }: { initial: ProxyEntry; onClose: () => void }) {
  const [p, setP] = useState<ProxyEntry>(initial);
  // DC/ISP proxies imported from a ProxyShard order carry the order id in
  // notes — enables editing their p0f OS signature here.
  const orderId = useMemo(() => {
    const m = initial.notes.match(/ProxyShard order (\d+)/);
    return m ? Number(m[1]) : null;
  }, [initial.notes]);
  const [sig, setSig] = useState("");
  const [curSig, setCurSig] = useState("");
  // Pull the IP's currently-set p0f signature from the order's active list.
  useEffect(() => {
    if (!orderId) return;
    invoke<any>("ps_active", { orderId })
      .then((r) => {
        const found = (r.data ?? []).find((d: any) => d.ip === initial.host);
        const s = found?.signature ?? "";
        setSig(s);
        setCurSig(s);
      })
      .catch(() => {});
  }, [orderId]);
  const save = async () => {
    try {
      await invoke("proxy_save", { entry: p });
      // Apply the p0f signature only when it changed to a non-empty value.
      if (orderId && sig && sig !== curSig) {
        try {
          await invoke("ps_signature_set", { orderId, items: [{ ip: p.host, signature: sig }] });
          toast.ok(`p0f set to ${sig}`);
        } catch (e) { toast.err("p0f: " + String(e)); }
      }
      toast.ok(initial.id ? "Proxy saved" : "Proxy added");
      onClose();
    } catch (e) { toast.err(String(e)); }
  };
  return (
    <div className="dialog-bg" onClick={onClose}>
      <div className="dialog" onClick={(e) => e.stopPropagation()}>
        <header className="dialog-head">
          <h2><ShardMini /> {initial.id ? "Edit proxy" : "New proxy"}</h2>
          <button className="icon-btn" onClick={onClose}>✕</button>
        </header>
        <div className="dialog-body">
          <Field label="Name" value={p.name} onChange={(v: string) => setP({ ...p, name: v })} />
          <div className="form-row">
            <label>
              <span className="lbl">Type</span>
              <select value={p.kind} onChange={(e) => setP({ ...p, kind: e.target.value as ProxyEntry["kind"] })}>
                <option value="socks5">SOCKS5</option><option value="http">HTTP</option><option value="https">HTTPS</option>
              </select>
            </label>
            <Field label="Country" value={p.country} onChange={(v: string) => setP({ ...p, country: v })} />
          </div>
          <div className="form-row">
            <Field label="Host" value={p.host} onChange={(v: string) => setP({ ...p, host: v })} />
            <NumField label="Port" value={p.port} onChange={(v) => setP({ ...p, port: v as any })} />
          </div>
          <div className="form-row">
            <Field label="Username" value={p.username} onChange={(v: string) => setP({ ...p, username: v })} />
            <Field label="Password" value={p.password} onChange={(v: string) => setP({ ...p, password: v })} type="password" />
          </div>
          {orderId && (
            <div className="form-row">
              <label>
                <span className="lbl">
                  p0f signature · order #{orderId}
                  <span className="muted"> · current: {curSig || "none"}</span>
                </span>
                <CSSelect value={sig} onChange={setSig} options={PS_SIGNATURES} placeholder="Don't change" />
              </label>
              <div />
            </div>
          )}
        </div>
        <footer className="dialog-foot">
          <button className="btn-ghost" onClick={onClose}>Cancel</button>
          <button className="btn-primary" onClick={save}><ShardMini /> Save</button>
        </footer>
      </div>
    </div>
  );
}

function FingerprintsView() {
  const [items, setItems] = useState<FingerprintEntry[]>([]);
  const [importerOpen, setImporterOpen] = useState(false);

  const reload = () =>
    invoke<FingerprintEntry[]>("fingerprint_list").then(setItems).catch((e) => toast.err(String(e)));
  useEffect(() => { reload(); }, []);

  const use = async (id: string) => {
    try {
      const meta = await invoke<ProfileMeta>("profile_create_from_template", { templateId: id });
      toast.ok(`Created "${meta.name}" — open Browsers to edit`);
    } catch (e) {
      toast.err(String(e));
    }
  };

  const remove = async (id: string) => {
    if ((await confirmModal({ title: "Remove fingerprint", message: "Remove this fingerprint from the library?", danger: true })) !== true) return;
    try {
      await invoke("fingerprint_delete", { id });
      toast.ok("Removed");
      reload();
    } catch (e) { toast.err(String(e)); }
  };

  const importJsonFile = async () => {
    const path = await open({
      multiple: false,
      directory: false,
      title: "Pick a FingerprintConfig JSON",
      filters: [{ name: "JSON", extensions: ["json"] }],
    });
    if (typeof path !== "string") return;
    try {
      const txt = await readTextFile(path);
      const e = await invoke<FingerprintEntry>("fingerprint_import", { jsonText: txt, idHint: null });
      toast.ok(`Imported "${e.label}"`);
      reload();
    } catch (e) { toast.err(String(e)); }
  };

  return (
    <section className="page">
      <Topbar crumbs={["Library", "Fingerprints"]} search="" onSearch={() => {}} />
      <div className="page-title">
        <h1>Fingerprint Library</h1>
        <div className="page-actions">
          <button
            className="btn-ghost"
            onClick={async () => {
              try {
                const path = await invoke<string>("fingerprint_dir");
                // Reveal folder via tauri-plugin-opener.
                await openPath(path);
              } catch (e) { toast.err(String(e)); }
            }}
            title="Reveal the on-disk library folder; drop JSONs here to add them"
          >
            <Icon.Folder /> Library folder
          </button>
          <button className="btn-ghost" onClick={importJsonFile}><Icon.Folder /> Import from file</button>
          <button className="btn-primary" onClick={() => setImporterOpen(true)}>+ Paste JSON</button>
        </div>
      </div>
      <p className="muted small" style={{ marginBottom: 14 }}>
        These FingerprintConfig snapshots populate the <strong>GPU</strong> select in the profile editor.
        Import your own from any working ShardX profile JSON to expand the list.
      </p>
      {items.length === 0 ? (
        <div className="empty">Library is empty — click "Import from file" or "Paste JSON".</div>
      ) : (
        <LibraryGroups items={items} onUse={use} onRemove={remove} />
      )}
      {importerOpen && (
        <FingerprintImporter onClose={() => { setImporterOpen(false); reload(); }} />
      )}
    </section>
  );
}

/// Library entries grouped by OS (macOS → Windows → Linux → other).
function LibraryGroups({
  items, onUse, onRemove,
}: {
  items: FingerprintEntry[];
  onUse: (id: string) => void;
  onRemove: (id: string) => void;
}) {
  const groups = useMemo(() => {
    const order = ["macOS", "Windows", "Linux"];
    const buckets = new Map<string, FingerprintEntry[]>();
    for (const it of items) {
      const k = it.platform || "Other";
      if (!buckets.has(k)) buckets.set(k, []);
      buckets.get(k)!.push(it);
    }
    return [
      ...order.filter((k) => buckets.has(k)).map((k) => [k, buckets.get(k)!] as const),
      ...[...buckets.keys()].filter((k) => !order.includes(k)).map((k) => [k, buckets.get(k)!] as const),
    ];
  }, [items]);

  return (
    <div className="lib-groups">
      {groups.map(([platform, list]) => (
        <div key={platform} className="lib-group">
          <div className="lib-group-head">
            <span className={`lib-group-dot lib-dot-${platform.toLowerCase()}`} />
            <h3>{platform}</h3>
            <span className="lib-group-count">{list.length}</span>
          </div>
          <div className="lib-grid">
            {list.map((t) => (
              <div
                key={t.id}
                className="lib-card"
                style={{ ['--accent' as any]: t.tag_color }}
              >
                <div className="lib-card-head">
                  <span className="lib-label">{t.label}</span>
                  {t.chrome && <span className="lib-chrome">Chrome {t.chrome}</span>}
                </div>
                <div className="lib-gpu mono" title={t.gpu}>{t.gpu || "—"}</div>
                <div className="lib-card-foot">
                  <button className="btn-sm btn-ghost" onClick={() => onUse(t.id)}>Use →</button>
                  {t.builtin
                    ? <span className="lib-tag">built-in</span>
                    : <button className="btn-sm btn-ghost danger" onClick={() => onRemove(t.id)} title="Remove">✕</button>}
                </div>
              </div>
            ))}
          </div>
        </div>
      ))}
    </div>
  );
}

function FingerprintImporter({ onClose }: { onClose: () => void }) {
  const [text, setText] = useState("");
  const [name, setName] = useState("");
  const save = async () => {
    try {
      const e = await invoke<FingerprintEntry>("fingerprint_import", { jsonText: text, idHint: name || null });
      toast.ok(`Imported "${e.label}"`);
      onClose();
    } catch (e) { toast.err(String(e)); }
  };
  return (
    <div className="dialog-bg" onClick={onClose}>
      <div className="dialog dialog-wide" onClick={(e) => e.stopPropagation()}>
        <header className="dialog-head">
          <h2><ShardMini /> Paste FingerprintConfig JSON</h2>
          <button className="icon-btn" onClick={onClose}>✕</button>
        </header>
        <div className="dialog-body">
          <Field label="Name (optional, becomes the file id)" value={name} onChange={setName} placeholder="e.g. mac-m4-pro-real" />
          <label>
            <span className="lbl">Paste the full JSON</span>
            <textarea rows={14} className="mono" value={text} onChange={(e) => setText(e.target.value)} placeholder='{ "name": "...", "navigator": { ... }, "webgl": { ... }, ... }' />
          </label>
        </div>
        <footer className="dialog-foot">
          <button className="btn-ghost" onClick={onClose}>Cancel</button>
          <button className="btn-primary" onClick={save}><ShardMini /> Import</button>
        </footer>
      </div>
    </div>
  );
}

/// Folder picker/creator modal (replaces native prompt). mode: "create" | "move".
function FolderModal({
  mode, existing, onPick, onCreate, onClose,
}: {
  mode: "create" | "move";
  existing: string[];
  onPick: (folder: string) => void;
  onCreate: (name: string) => void;
  onClose: () => void;
}) {
  const [name, setName] = useState("");
  const ref = useRef<HTMLInputElement>(null);
  useEffect(() => { ref.current?.focus(); }, []);
  const trimmed = name.trim();
  const dup = existing.includes(trimmed);
  const create = () => { if (trimmed && !dup) onCreate(trimmed); };
  const showList = mode === "move" && existing.length > 0;
  return (
    <div className="dialog-bg" onClick={onClose}>
      <div className="dialog" onClick={(e) => e.stopPropagation()}>
        <header className="dialog-head">
          <h2><ShardMini /> {mode === "move" ? "Move to folder" : "New folder"}</h2>
          <button className="icon-btn" onClick={onClose}>✕</button>
        </header>
        <div className="dialog-body">
          {showList && (
            <>
              <span className="lbl">Existing folders</span>
              <div className="folder-pick-list">
                {existing.map((f) => (
                  <button key={f} className="folder-pick" onClick={() => onPick(f)}>
                    <Icon.Folder /> {f}
                  </button>
                ))}
              </div>
              <div className="folder-pick-sep"><span>or create new</span></div>
            </>
          )}
          <label>
            <span className="lbl">{showList ? "New folder name" : "Folder name"}</span>
            <input
              ref={ref}
              value={name}
              placeholder="e.g. Shops, Socials, QA…"
              onChange={(e) => setName(e.target.value)}
              onKeyDown={(e) => {
                if (e.key === "Enter") create();
                if (e.key === "Escape") onClose();
              }}
            />
          </label>
          {dup && <div className="muted small" style={{ color: "var(--err)" }}>Folder “{trimmed}” already exists.</div>}
          <div style={{ display: "flex", justifyContent: "flex-end", gap: 10, marginTop: 4 }}>
            <button className="btn-ghost" onClick={onClose}>Cancel</button>
            <button className="btn-primary" disabled={!trimmed || dup} onClick={create}>
              {showList ? "Create & move" : "Create"}
            </button>
          </div>
        </div>
      </div>
    </div>
  );
}

function TemplatePicker({
  fingerprints,
  onPick,
  onClose,
}: {
  /** When passed in, skip the fingerprint_list IO and the visible mount
   *  flash that used to happen while the 170-entry list streamed back. */
  fingerprints?: FingerprintEntry[];
  onPick: (id: string) => void;
  onClose: () => void;
}) {
  const [lib, setLib] = useState<FingerprintEntry[]>(fingerprints ?? []);
  const [host, setHost] = useState<string>("");
  useEffect(() => {
    if (!fingerprints) {
      invoke<FingerprintEntry[]>("fingerprint_list").then(setLib).catch(() => {});
    }
    invoke<string>("host_platform").then(setHost).catch(() => {});
  }, [fingerprints]);
  // Only host-matching fingerprints (UA/fonts/WebGL renderer are host-coupled).
  const tpls = host ? lib.filter((e) => e.platform === host) : [];
  return (
    <div className="dialog-bg" onClick={onClose}>
      <div className="dialog dialog-wide" onClick={(e) => e.stopPropagation()}>
        <header className="dialog-head">
          <h2><ShardMini /> Pick a {host || ""} fingerprint</h2>
          <button className="icon-btn" onClick={onClose}>✕</button>
        </header>
        <div className="dialog-body">
          {tpls.length === 0 ? (
            <div className="empty">
              No {host} fingerprints in the library yet. Add some on the
              Fingerprints page (or drop JSONs into the library folder).
            </div>
          ) : (
            <div className="tpl-grid">
              {tpls.map((t) => (
                <button key={t.id} className="tpl-card" onClick={() => onPick(t.id)} style={{ ['--accent' as any]: t.tag_color }}>
                  <div className="tpl-accent" />
                  <div className="tpl-head">
                    <span className="tpl-platform">{t.platform}</span>
                    <span className="tpl-chrome">Chrome {t.chrome}</span>
                  </div>
                  <div className="tpl-label">{t.label}</div>
                  <div className="tpl-gpu">{t.gpu}</div>
                </button>
              ))}
            </div>
          )}
        </div>
      </div>
    </div>
  );
}

/// First-run gate: fullscreen overlay until runtime is on disk.
type RtSpec = {
  browser: { key: string; label: string };
  widevine: { key: string; label: string } | null;
};
type RtStatus = {
  installed: boolean;
  binary_path: string | null;
  installed_browser_etag: string | null;
  remote_browser_etag: string | null;
  update_available: boolean;
  spec: RtSpec | null;
  fingerprints_installed: boolean;
};
type RtProgress = {
  label: string;
  phase: "download" | "extract";
  received: number;
  total: number;
  percent: number;
};

function FirstRunGate({ children }: { children: ReactNode }) {
  // null = querying backend; true = reveal; false = show overlay.
  const [installed, setInstalled] = useState<boolean | null>(null);
  const [prog, setProg] = useState<RtProgress | null>(null);
  const [err, setErr] = useState<string | null>(null);
  // Single in-flight install at a time.
  const installing = useRef(false);

  const fmt = (b: number) =>
    b < 1024 * 1024 ? `${(b / 1024).toFixed(0)} KB` : `${(b / (1024 * 1024)).toFixed(1)} MB`;

  useEffect(() => {
    let cancelled = false;
    let unProg: (() => void) | undefined;
    let unDone: (() => void) | undefined;

    (async () => {
      // Subscribe BEFORE invoking so we don't miss the first event.
      unProg = await listen<RtProgress>("runtime:progress", (e) => {
        if (!cancelled) setProg(e.payload);
      });
      unDone = await listen("runtime:done", () => {
        if (!cancelled) { setProg(null); setInstalled(true); }
      });

      let status: RtStatus;
      try {
        status = await invoke<RtStatus>("runtime_status");
      } catch (e: any) {
        if (!cancelled) setErr(String(e));
        return;
      }
      if (cancelled) return;

      // Unsupported platform: let the user in; launch will error if attempted.
      if (!status.spec) {
        setInstalled(true);
        return;
      }
      // Reveal only when the engine + fingerprints are installed AND up to
      // date. An available engine update (chromium version bump) falls through
      // to the install path below, which re-downloads the changed archives.
      if (status.installed && status.fingerprints_installed && !status.update_available) {
        setInstalled(true);
        return;
      }

      setInstalled(false);
      if (installing.current) return;
      installing.current = true;
      try {
        await invoke<RtStatus>("runtime_install", { force: false });
        if (!cancelled) setInstalled(true);
      } catch (e: any) {
        if (!cancelled) setErr(typeof e === "string" ? e : (e?.message ?? String(e)));
      } finally {
        installing.current = false;
      }
    })();

    return () => {
      cancelled = true;
      unProg?.();
      unDone?.();
    };
  }, []);

  if (installed === null) {
    return null;
  }
  if (installed) {
    return <>{children}</>;
  }

  return (
    <div
      style={{
        position: "fixed",
        inset: 0,
        zIndex: 1000,
        display: "flex",
        alignItems: "center",
        justifyContent: "center",
        background: "var(--bg, #0b0b0e)",
        color: "var(--fg, #e6e6e6)",
      }}
    >
      <div style={{ width: 460, padding: "32px 36px", textAlign: "center" }}>
        <div style={{ fontSize: 20, fontWeight: 600, marginBottom: 8 }}>
          Setting up ShardX browser
        </div>
        <div className="muted small" style={{ marginBottom: 24 }}>
          First-run download from our CDN. Done once per install
          (~{prog?.total ? fmt(prog.total) : "150 MB"}).
        </div>

        {prog && (
          <>
            <div className="muted small" style={{ marginBottom: 6, textAlign: "left" }}>
              {prog.label} —{" "}
              {prog.phase === "download"
                ? `${fmt(prog.received)} / ${fmt(prog.total)}  (${prog.percent}%)`
                : "extracting…"}
            </div>
            <div
              style={{
                height: 8,
                background: "var(--bg-muted, #1f1f24)",
                borderRadius: 4,
                overflow: "hidden",
              }}
            >
              <div
                style={{
                  width: `${prog.percent}%`,
                  height: "100%",
                  background: "var(--accent, #4ade80)",
                  transition: "width 0.1s linear",
                }}
              />
            </div>
          </>
        )}
        {!prog && !err && (
          <div className="muted small">Contacting CDN…</div>
        )}
        {err && (
          <div style={{ color: "var(--err, #fb7185)", marginTop: 12, fontSize: 13 }}>
            {err}
          </div>
        )}
      </div>
    </div>
  );
}

/// Sidebar version pill; tints amber when a newer GitHub Release exists.
type RtUpdate = {
  current: string;
  latest: string | null;
  update_available: boolean;
  release_url: string | null;
};

function VersionPill() {
  const [info, setInfo] = useState<RtUpdate | null>(null);
  useEffect(() => {
    invoke<RtUpdate>("launcher_update_check").then(setInfo).catch(() => {});
  }, []);
  const open = () => {
    if (info?.release_url) openUrl(info.release_url).catch(() => {});
  };
  const clickable = !!info?.release_url;
  return (
    <button
      type="button"
      className={`version-pill ${info?.update_available ? "update-available" : ""}`}
      onClick={open}
      disabled={!clickable}
      title={
        info?.update_available
          ? `New release ${info.latest} is available — click to open the Releases page.`
          : info
          ? `Running ${info.current}${info.latest ? `, GitHub: ${info.latest}` : ""}`
          : "Checking for updates…"
      }
    >
      <ShardMini />
      <div className="version-pill-text">
        <div className="version-pill-current">
          ShardX Launcher v{info?.current ?? "…"}
        </div>
        <div className="version-pill-sub">
          {info === null
            ? "checking for updates…"
            : info.update_available
            ? `Update available → ${info.latest}`
            : info.latest
            ? "up to date"
            : "offline"}
        </div>
      </div>
    </button>
  );
}

// ---- ProxyShard billing integration ----
//
// Talks to https://user-api.proxyshard.com via the ps_* Tauri commands
// (Bearer key stored locally in psapi.json).  Sub-user management is
// intentionally omitted — residential traffic is shown/topped-up at the
// account-owner level only.

type PsMe = { email: string; active_orders: number; wallet_balance: number };
type PsOrder = {
  order_id: number;
  product_name: string;
  cycle_name: string;
  expires_at: string | null;
  auto_renewal: boolean | null;
  tag: string | null;
};
type PsProduct = { name: string; description?: string | null; location?: string | null; cycles: string[] };
type PsCalc = { original_price: number; final_price: number; discount_percent: number; addons_price?: number; total_with_addons?: number };

const fmtCents = (c: number) => `$${(Number(c || 0) / 100).toFixed(2)}`;
const fmtGB = (bytes: number) => {
  const gb = Number(bytes || 0) / 1024 ** 3;
  return `${gb >= 100 ? gb.toFixed(0) : gb.toFixed(2)} GB`;
};
const isDcIsp = (name: string) => /datacenter|isp/i.test(name);

// Residential relay gateway hosts for generated proxy strings (port depends
// on protocol — see PS_PORT).
const PS_RELAYS = [
  "relay-eu.proxyshard.com",
  "relay-ru.proxyshard.net",
  "relay-ua.proxyshard.com",
];
// Relay ports differ by protocol: HTTP 8080, SOCKS5 1080.
const PS_PORT = { http: 8080, socks5: 1080 } as const;
type ResiType = "standart" | "premium" | "unmetered";
// Username plan token per residential tier.
const PS_PLAN: Record<ResiType, string> = { standart: "limited", premium: "premium", unmetered: "unlimited" };
// proxy_type query param accepted by /proxies/{profile,countries,regions,cities}.
const PS_PROXY_TYPE: Record<ResiType, string> = { standart: "standart", premium: "premium", unmetered: "unlimited" };
// p0f OS-fingerprint signatures (signature/set endpoint enum).
const PS_SIGNATURES: { value: string; label: string }[] = [
  { value: "", label: "Don't set" },
  { value: "ios", label: "iOS" },
  { value: "macos", label: "macOS" },
  { value: "android", label: "Android" },
  { value: "linux", label: "Linux" },
  { value: "win10", label: "Windows 10" },
  { value: "win11", label: "Windows 11" },
];
// 12-char lowercase alnum sticky-session id (matches ProxyShard's `sid` form).
const randSid = () => {
  const a = "abcdefghijklmnopqrstuvwxyz0123456789";
  let s = "";
  for (let i = 0; i < 12; i++) s += a[Math.floor(Math.random() * a.length)];
  return s;
};

function ProxyShardView() {
  // null = still loading the saved key from disk.
  const [key, setKey] = useState<string | null>(null);
  const [draft, setDraft] = useState("");
  const [showKey, setShowKey] = useState(false);
  const [me, setMe] = useState<PsMe | null>(null);
  const [status, setStatus] = useState<"idle" | "checking" | "ok" | "err">("idle");
  const [err, setErr] = useState("");

  const connect = async () => {
    setStatus("checking");
    setErr("");
    try {
      const m = await invoke<PsMe>("ps_me");
      setMe(m);
      setStatus("ok");
    } catch (e) {
      setMe(null);
      setStatus("err");
      setErr(String(e));
    }
  };

  useEffect(() => {
    invoke<string>("ps_get_key")
      .then((k) => {
        setKey(k);
        setDraft(k);
        if (k) connect();
        else setStatus("idle");
      })
      .catch(() => setKey(""));
  }, []);

  const saveKey = async () => {
    const next = draft.trim();
    try {
      await invoke("ps_set_key", { key: next });
      setKey(next);
      toast.ok("API key saved");
      if (next) connect();
      else { setMe(null); setStatus("idle"); }
    } catch (e) { toast.err(String(e)); }
  };

  const connected = status === "ok";

  return (
    <section className="page ps-page">
      <Topbar crumbs={["Workspace", "ProxyShard"]} search="" onSearch={() => {}} />

      <div className="metric-strip">
        <Metric label="Account" value={connected ? "Connected" : "—"} accent={connected} pulse={connected} />
        <Metric label="Balance" value={me ? fmtCents(me.wallet_balance) : "—"} />
        <Metric label="Active orders" value={me ? String(me.active_orders) : "—"} />
      </div>

      <div className="page-title">
        <h1>ProxyShard</h1>
        <div className="page-actions">
          <button
            className="proxy-buy-cta"
            onClick={() => { openUrl(DASHBOARD_URL).catch(() => {}); }}
            title="Open the ProxyShard dashboard in your browser"
          >
            <ShardMini /> Open dashboard
          </button>
        </div>
      </div>

      {/* API key — kept first so it's always on view. */}
      <div className="card" style={{ marginBottom: 14 }}>
        <h3>API key</h3>
        <p className="muted small">
          Paste your ProxyShard <strong>API key</strong> (from the{" "}
          <a href="#" onClick={(e) => { e.preventDefault(); openUrl(DASHBOARD_URL).catch(() => {}); }}>dashboard</a>).
          It's stored locally and sent as <code>Authorization: Bearer …</code> to user-api.proxyshard.com.
        </p>
        <div className="ps-key-row">
          <div className="copy-field ps-key-input">
            <input
              type={showKey ? "text" : "password"}
              placeholder="paste API key…"
              value={draft}
              onChange={(e) => setDraft(e.target.value)}
              onKeyDown={(e) => { if (e.key === "Enter") saveKey(); }}
            />
            <button
              type="button"
              className="copy-icon"
              title={showKey ? "Hide" : "Show"}
              onClick={() => setShowKey((v) => !v)}
            >
              {showKey ? "Hide" : "Show"}
            </button>
          </div>
          <button className="btn-primary" onClick={saveKey} disabled={draft.trim() === (key ?? "")}>Save</button>
          <button className="btn-ghost" onClick={connect} disabled={!key || status === "checking"}>
            {status === "checking" ? "Checking…" : "Test"}
          </button>
        </div>
        <div className="ps-key-status">
          {status === "checking" && <span className="muted small">Validating…</span>}
          {connected && me && <span className="status-pill status-active">Connected · {me.email}</span>}
          {status === "err" && <span className="status-pill status-failed" title={err}>Not connected — {err}</span>}
          {status === "idle" && !key && <span className="muted small">No key set yet.</span>}
        </div>
      </div>

      {connected ? (
        <>
          <PsResidentialCard />
          <PsOrdersCard onChanged={connect} />
          <PsBuyCard onPurchased={connect} />
        </>
      ) : (
        <div className="card">
          <p className="muted small">Add a valid API key above to view traffic, manage orders, and buy proxies.</p>
        </div>
      )}
    </section>
  );
}

/// Residential card: tier toggle (Standard / Premium / Unmetered), metered
/// traffic for Standard/Premium, in-place top-up, and the relay proxy
/// generator. Unmetered is a flat plan, so it skips the GB meter.
function PsResidentialCard() {
  const [type, setType] = useState<ResiType>("standart");
  const [data, setData] = useState<{ data: number; data_remain: number; data_spent: number } | null>(null);
  const [loading, setLoading] = useState(false);
  const [err, setErr] = useState("");
  const [orders, setOrders] = useState<PsOrder[]>([]);
  const [topup, setTopup] = useState<PsOrder | null>(null);
  const [genOpen, setGenOpen] = useState(false);
  const [renewing, setRenewing] = useState(false);

  const loadTraffic = async (t: ResiType) => {
    setData(null);
    setErr("");
    if (t === "unmetered") return; // flat plan — no GB meter
    setLoading(true);
    try {
      const r = await invoke<any>("ps_profile_traffic", { proxyType: t });
      setData({ data: r.data ?? 0, data_remain: r.data_remain ?? 0, data_spent: r.data_spent ?? 0 });
    } catch (e) { setErr(String(e)); }
    finally { setLoading(false); }
  };
  useEffect(() => { loadTraffic(type); }, [type]);
  // Orders back the top-up / renew targets (need an order id).
  const loadOrders = () =>
    invoke<any>("ps_orders", { status: "all", limit: 100 })
      .then((r) => setOrders(r.orders ?? []))
      .catch(() => {});
  useEffect(() => { loadOrders(); }, []);

  const re = { standart: /standart\s+residential/i, premium: /premium\s+residential/i, unmetered: /unmetered\s+residential/i }[type];
  const order = orders.find((o) => re.test(o.product_name)) ?? null;

  const renew = async () => {
    if (!order) return;
    setRenewing(true);
    try {
      await invoke("ps_renew", { id: order.order_id });
      toast.ok(`Renewed order #${order.order_id}`);
      loadOrders();
    } catch (e) { toast.err(String(e)); }
    finally { setRenewing(false); }
  };
  const pct = data && data.data > 0 ? Math.min(100, Math.round((data.data_spent / data.data) * 100)) : 0;

  return (
    <div className="card" style={{ marginBottom: 14 }}>
      <div className="ps-card-head">
        <h3>Residential</h3>
        <div className="ps-seg-toggle">
          {(["standart", "premium", "unmetered"] as ResiType[]).map((t) => (
            <button key={t} className={`ps-seg ${type === t ? "active" : ""}`} onClick={() => setType(t)}>
              {t === "standart" ? "Standard" : t === "premium" ? "Premium" : "Unmetered"}
            </button>
          ))}
        </div>
      </div>

      {type !== "unmetered" ? (
        <>
          {loading && <p className="muted small">Loading…</p>}
          {err && !loading && <p className="muted small">{err}</p>}
          {data && !loading && (
            <>
              <div className="ps-traffic-stats">
                <div><span className="ps-stat-val">{fmtGB(data.data_remain)}</span><span className="ps-stat-lbl">Remaining</span></div>
                <div><span className="ps-stat-val">{fmtGB(data.data_spent)}</span><span className="ps-stat-lbl">Used</span></div>
                <div><span className="ps-stat-val">{fmtGB(data.data)}</span><span className="ps-stat-lbl">Total</span></div>
              </div>
              <div className="ps-bar"><div className="ps-bar-fill" style={{ width: `${pct}%` }} /></div>
              <p className="muted small">{pct}% used.</p>
            </>
          )}
        </>
      ) : (
        <p className="muted small">
          Unlimited plan{order?.expires_at ? ` · expires ${order.expires_at.slice(0, 10)}` : ""}.
        </p>
      )}

      <div className="ps-buy-foot">
        {type !== "unmetered" ? (
          <button
            className="btn-ghost"
            disabled={!order}
            title={order ? undefined : "No residential order found for this tier"}
            onClick={() => order && setTopup(order)}
          >
            + Add traffic
          </button>
        ) : (
          <button
            className="btn-ghost"
            disabled={!order || renewing}
            title={order ? undefined : "No unmetered order found"}
            onClick={renew}
          >
            {renewing ? "Renewing…" : "Renew"}
          </button>
        )}
        <button className="btn-primary" onClick={() => setGenOpen(true)}><ShardMini /> Generate proxies</button>
      </div>

      {topup && <PsTopupModal order={topup} onClose={() => setTopup(null)} onDone={() => { setTopup(null); loadTraffic(type); }} />}
      {genOpen && <PsResiGenerator type={type} onClose={() => setGenOpen(false)} />}
    </div>
  );
}

type PsLoc = { code: string; name: string };

/// Build relay residential proxy strings (plan-country-region-city[-sid]) and
/// save them locally. Password comes from /proxies/profile.
function PsResiGenerator({ type, onClose }: { type: ResiType; onClose: () => void }) {
  const plan = PS_PLAN[type];
  const pt = PS_PROXY_TYPE[type];
  const [password, setPassword] = useState("");
  const [pwErr, setPwErr] = useState("");
  const [relay, setRelay] = useState(PS_RELAYS[0]);
  const [proto, setProto] = useState<"http" | "socks5">("socks5");
  const [session, setSession] = useState<"rotating" | "sticky">("sticky");
  const [count, setCount] = useState(1);
  const [prefix, setPrefix] = useState(`${type} resi`);

  const [countries, setCountries] = useState<PsLoc[]>([]);
  const [country, setCountry] = useState("");
  const [regions, setRegions] = useState<PsLoc[]>([]);
  const [region, setRegion] = useState("");
  const [cities, setCities] = useState<PsLoc[]>([]);
  const [city, setCity] = useState("");
  const [saving, setSaving] = useState(false);

  useEffect(() => {
    invoke<any>("ps_profile_traffic", { proxyType: pt })
      .then((r) => {
        const p = r.proxy_password ?? r.password ?? "";
        setPassword(p);
        if (!p) setPwErr("The API didn't return a residential password for this plan.");
      })
      .catch((e) => setPwErr(String(e)));
    invoke<any>("ps_countries", { proxyType: pt })
      .then((r) => setCountries(r.results ?? []))
      .catch((e) => toast.err(String(e)));
  }, [pt]);

  // Region depends on country; city depends on region.
  useEffect(() => {
    setRegion(""); setRegions([]); setCity(""); setCities([]);
    if (!country) return;
    invoke<any>("ps_regions", { proxyType: pt, countryCode: country })
      .then((r) => setRegions(r.results ?? [])).catch(() => {});
  }, [country]); // eslint-disable-line react-hooks/exhaustive-deps
  useEffect(() => {
    setCity(""); setCities([]);
    if (!country || !region) return;
    invoke<any>("ps_cities", { proxyType: pt, countryCode: country, regionCode: region })
      .then((r) => setCities(r.results ?? [])).catch(() => {});
  }, [region]); // eslint-disable-line react-hooks/exhaustive-deps

  const buildUser = (sid: string | null) => {
    const parts = [`plan-${plan}`];
    if (country) parts.push(`country-${country.toLowerCase()}`);
    if (region) parts.push(`region-${region}`);
    if (city) parts.push(`city-${city}`);
    if (sid) parts.push(`sid-${sid}`);
    return parts.join("-");
  };
  const sampleUser = buildUser(session === "sticky" ? "‹sid›" : null);

  const generate = async () => {
    if (!password) { toast.err("No residential password available from the API"); return; }
    const port = PS_PORT[proto];
    const n = Math.max(1, Math.round(count));
    const entries = Array.from({ length: n }, (_, i) => ({
      id: "",
      name: `${prefix.trim() || "resi"}${country ? " " + country.toUpperCase() : ""}${n > 1 ? ` #${i + 1}` : ""}`,
      kind: proto,
      host: relay,
      port,
      username: buildUser(session === "sticky" ? randSid() : null),
      password,
      country: country ? country.toUpperCase() : "",
      notes: `ProxyShard residential (${plan})`,
    }));
    setSaving(true);
    try {
      const added = await invoke<number>("proxy_bulk_save", { entries });
      toast.ok(added > 0 ? `Generated ${added} prox${added === 1 ? "y" : "ies"}` : "No new proxies (duplicates)");
      onClose();
    } catch (e) { toast.err(String(e)); }
    finally { setSaving(false); }
  };

  return (
    <div className="dialog-bg" onClick={onClose}>
      <div className="dialog dialog-wide" onClick={(e) => e.stopPropagation()}>
        <header className="dialog-head">
          <h2><ShardMini /> Generate residential proxies — {plan}</h2>
          <button className="icon-btn" onClick={onClose}>✕</button>
        </header>
        <div className="dialog-body">
          <div className="form-row">
            <label>
              <span className="lbl">Relay</span>
              <CSSelect value={relay} onChange={setRelay} options={PS_RELAYS.map((r) => ({ value: r, label: r }))} />
            </label>
            <label>
              <span className="lbl">Protocol</span>
              <div className="ps-seg-toggle">
                <button className={`ps-seg ${proto === "http" ? "active" : ""}`} onClick={() => setProto("http")}>HTTP</button>
                <button className={`ps-seg ${proto === "socks5" ? "active" : ""}`} onClick={() => setProto("socks5")}>SOCKS5</button>
              </div>
            </label>
          </div>
          <div className="form-row">
            <label>
              <span className="lbl">Country</span>
              <CSSelect
                value={country}
                onChange={setCountry}
                placeholder="Any"
                options={[{ value: "", label: "Any" }, ...countries.map((c) => ({ value: c.code, label: `${c.name} (${c.code})` }))]}
              />
            </label>
            <label>
              <span className="lbl">Session</span>
              <div className="ps-seg-toggle">
                <button className={`ps-seg ${session === "rotating" ? "active" : ""}`} onClick={() => setSession("rotating")}>Rotating</button>
                <button className={`ps-seg ${session === "sticky" ? "active" : ""}`} onClick={() => setSession("sticky")}>Sticky</button>
              </div>
            </label>
          </div>
          <div className="form-row">
            <label>
              <span className="lbl">Region</span>
              <CSSelect
                value={region}
                onChange={setRegion}
                placeholder={country ? "Any" : "Pick country first"}
                options={[{ value: "", label: "Any" }, ...regions.map((r) => ({ value: r.code, label: r.name }))]}
              />
            </label>
            <label>
              <span className="lbl">City</span>
              <CSSelect
                value={city}
                onChange={setCity}
                placeholder={region ? "Any" : "Pick region first"}
                options={[{ value: "", label: "Any" }, ...cities.map((c) => ({ value: c.code, label: c.name }))]}
              />
            </label>
          </div>
          <div className="form-row">
            <Field label="Name prefix" value={prefix} onChange={setPrefix} />
            <NumField label={session === "sticky" ? "Count (random sid each)" : "Count"} value={count} onChange={(v) => setCount(Math.max(1, Math.round(v)))} />
          </div>
          <div className="ps-gen-preview mono small">
            {relay}:{PS_PORT[proto]}:{sampleUser}:{password ? "••••" : "?"}
          </div>
          {pwErr && <p className="muted small">{pwErr}</p>}
        </div>
        <footer className="dialog-foot">
          <button className="btn-ghost" onClick={onClose}>Cancel</button>
          <button className="btn-primary" onClick={generate} disabled={saving || !password}>
            <Icon.Download /> {saving ? "Generating…" : `Generate ${Math.max(1, Math.round(count))}`}
          </button>
        </footer>
      </div>
    </div>
  );
}

/// Orders list: add DC/ISP proxies to the local list, top up residential
/// traffic, edit the tag, or renew an on-hold order. Mobile proxies are
/// hidden (they aren't manageable from here), and the list is paginated.
const PS_ORDERS_PAGE = 10;
function PsOrdersCard({ onChanged }: { onChanged: () => void }) {
  const [status, setStatus] = useState("active");
  const [orders, setOrders] = useState<PsOrder[]>([]);
  const [loading, setLoading] = useState(false);
  const [busy, setBusy] = useState<Record<number, boolean>>({});
  const [offset, setOffset] = useState(0);
  const [hasNext, setHasNext] = useState(false);
  const [importing, setImporting] = useState<PsOrder | null>(null);
  const [tagging, setTagging] = useState<PsOrder | null>(null);

  const load = async (off = offset) => {
    setLoading(true);
    try {
      const r = await invoke<any>("ps_orders", { status, offset: off, limit: PS_ORDERS_PAGE });
      setOrders(r.orders ?? []);
      // `next` is a page URI when more results exist (nullable).
      setHasNext(!!r.next);
    } catch (e) { toast.err(String(e)); }
    finally { setLoading(false); }
  };
  // Reset to the first page whenever the status filter changes.
  useEffect(() => { setOffset(0); load(0); }, [status]); // eslint-disable-line react-hooks/exhaustive-deps

  const setB = (id: number, v: boolean) => setBusy((s) => ({ ...s, [id]: v }));

  const renew = async (o: PsOrder) => {
    setB(o.order_id, true);
    try {
      await invoke("ps_renew", { id: o.order_id });
      toast.ok(`Renewed order #${o.order_id}`);
      load();
      onChanged();
    } catch (e) { toast.err(String(e)); }
    finally { setB(o.order_id, false); }
  };

  const go = (next: boolean) => {
    const off = Math.max(0, offset + (next ? PS_ORDERS_PAGE : -PS_ORDERS_PAGE));
    setOffset(off);
    load(off);
  };

  // Orders here are Datacenter/ISP only — residential is managed in the
  // Residential card, mobile isn't manageable from the launcher.
  const visible = orders.filter((o) => isDcIsp(o.product_name));

  return (
    <div className="card" style={{ marginBottom: 14 }}>
      <div className="ps-card-head">
        <h3>Orders</h3>
        <div className="row-inline" style={{ gap: 8 }}>
          <div style={{ width: 130 }}>
            <CSSelect
              value={status}
              onChange={setStatus}
              options={[
                { value: "active", label: "Active" },
                { value: "on-hold", label: "On hold" },
                { value: "cancelled", label: "Cancelled" },
                { value: "all", label: "All" },
              ]}
            />
          </div>
          <button className="icon-btn" onClick={() => load()} title="Refresh"><Icon.Refresh /></button>
        </div>
      </div>
      {loading && <p className="muted small">Loading…</p>}
      {!loading && visible.length === 0 && <p className="muted small">No orders for this filter.</p>}
      {!loading && visible.length > 0 && (
        <div className="rows ps-orders">
          {visible.map((o) => (
            <div key={o.order_id} className="row ps-order-row">
              <div className="ps-order-main">
                <span className="ps-order-name">{o.product_name}</span>
                <span className="muted small">
                  #{o.order_id} · {o.cycle_name}
                  {o.tag && o.tag !== "none" ? ` · ${o.tag}` : ""}
                  {o.expires_at ? ` · until ${o.expires_at.slice(0, 10)}` : ""}
                </span>
              </div>
              <div className="row-actions ps-order-actions">
                <button className="btn-ghost btn-sm" onClick={() => setImporting(o)} title="Pick which proxies to add to your list">
                  <Icon.Download /> Add to proxies
                </button>
                <button className="icon-btn" onClick={() => setTagging(o)} title="Edit tag"><Icon.Edit /></button>
                {status === "on-hold" && (
                  <button className="btn-ghost btn-sm" disabled={busy[o.order_id]} onClick={() => renew(o)}>Renew</button>
                )}
              </div>
            </div>
          ))}
        </div>
      )}
      {!loading && (offset > 0 || hasNext) && (
        <div className="pager">
          <button className="btn-ghost btn-sm" disabled={offset <= 0} onClick={() => go(false)}>‹ Prev</button>
          <span className="pager-info">Page {Math.floor(offset / PS_ORDERS_PAGE) + 1}</span>
          <button className="btn-ghost btn-sm" disabled={!hasNext} onClick={() => go(true)}>Next ›</button>
        </div>
      )}
      {importing && (
        <PsImportModal order={importing} onClose={() => setImporting(null)} />
      )}
      {tagging && (
        <PsTagModal order={tagging} onClose={() => setTagging(null)} onDone={() => { setTagging(null); load(); }} />
      )}
    </div>
  );
}

/// Active-proxy picker: fetch an order's proxies, choose SOCKS5/HTTP and which
/// IPs to import into the local proxy list (via proxy_bulk_save, which dedups).
type PsActiveProxy = { ip: string; username: string; password: string; http_port: number; socks_port: number; until: string; status: string; signature?: string | null };
function PsImportModal({ order, onClose }: { order: PsOrder; onClose: () => void }) {
  const [items, setItems] = useState<PsActiveProxy[] | null>(null);
  const [err, setErr] = useState("");
  const [kind, setKind] = useState<"socks5" | "http">("socks5");
  const [sel, setSel] = useState<Set<string>>(new Set());
  const [saving, setSaving] = useState(false);
  const [tag, setTag] = useState("");
  // Per-IP p0f signature ("" = leave unchanged).
  const [sigByIp, setSigByIp] = useState<Record<string, string>>({});
  // p0f slot accounting from the order detail (available vs already used).
  const [slots, setSlots] = useState<{ avail: number; used: number } | null>(null);

  useEffect(() => {
    invoke<any>("ps_active", { orderId: order.order_id })
      .then((r) => {
        const data: PsActiveProxy[] = r.data ?? [];
        setItems(data);
        setSel(new Set(data.map((d) => d.ip))); // select all by default
        // Prefill each row with its currently-set signature.
        setSigByIp(Object.fromEntries(data.map((d) => [d.ip, d.signature ?? ""])));
        setTag((r.order_tag && r.order_tag !== "none" ? r.order_tag : "") || `order ${order.order_id}`);
      })
      .catch((e) => setErr(String(e)));
    invoke<any>("ps_order", { id: order.order_id })
      .then((r) => {
        const o = r.order ?? {};
        setSlots({ avail: o.p0f_slots_available ?? 0, used: o.p0f_slots_used ?? 0 });
      })
      .catch(() => {});
  }, [order.order_id]);

  const toggle = (ip: string) =>
    setSel((s) => { const n = new Set(s); n.has(ip) ? n.delete(ip) : n.add(ip); return n; });

  const allChecked = !!items && items.length > 0 && items.every((d) => sel.has(d.ip));
  const toggleAll = () =>
    setSel(allChecked ? new Set() : new Set((items ?? []).map((d) => d.ip)));

  // p0f can be assigned only while free slots remain.
  const canSetP0f = !!slots && slots.avail > slots.used;
  const free = slots ? Math.max(0, slots.avail - slots.used) : 0;
  const setSig = (ip: string, v: string) => setSigByIp((m) => ({ ...m, [ip]: v }));

  const save = async () => {
    if (!items) return;
    const chosen = items.filter((d) => sel.has(d.ip));
    if (chosen.length === 0) { toast.err("Select at least one proxy"); return; }
    const label = tag.trim() || `order ${order.order_id}`;
    const entries = chosen
      .map((d) => {
        const port = kind === "http" ? d.http_port : d.socks_port;
        if (!d.ip || !port) return null;
        return {
          id: "",
          name: `${label} · ${d.ip}`,
          kind,
          host: d.ip,
          port,
          username: d.username ?? "",
          password: d.password ?? "",
          country: "",
          notes: `ProxyShard order ${order.order_id}`,
        };
      })
      .filter(Boolean);
    setSaving(true);
    try {
      const n = await invoke<number>("proxy_bulk_save", { entries });
      toast.ok(n > 0 ? `Added ${n} prox${n === 1 ? "y" : "ies"}` : "No new proxies (already in your list)");
      // Apply only the selected proxies whose signature actually changed
      // (a non-empty value differing from the one already set).
      const sigItems = chosen
        .filter((d) => { const v = sigByIp[d.ip] ?? ""; return v !== "" && v !== (d.signature ?? ""); })
        .map((d) => ({ ip: d.ip, signature: sigByIp[d.ip] }));
      if (sigItems.length > 0) {
        try {
          await invoke("ps_signature_set", { orderId: order.order_id, items: sigItems });
          toast.ok(`Set p0f on ${sigItems.length} IP${sigItems.length === 1 ? "" : "s"}`);
        } catch (e) { toast.err("Signature: " + String(e)); }
      }
      onClose();
    } catch (e) { toast.err(String(e)); }
    finally { setSaving(false); }
  };

  return (
    <div className="dialog-bg" onClick={onClose}>
      <div className="dialog dialog-wide" onClick={(e) => e.stopPropagation()}>
        <header className="dialog-head">
          <h2><ShardMini /> Add proxies — {order.product_name} #{order.order_id}</h2>
          <button className="icon-btn" onClick={onClose}>✕</button>
        </header>
        <div className="dialog-body">
          <div className="ps-import-top">
            <div style={{ flex: 1 }}>
              <Field label="Name prefix" value={tag} onChange={setTag} />
            </div>
            <div className="ps-seg-toggle">
              <button className={`ps-seg ${kind === "socks5" ? "active" : ""}`} onClick={() => setKind("socks5")}>SOCKS5</button>
              <button className={`ps-seg ${kind === "http" ? "active" : ""}`} onClick={() => setKind("http")}>HTTP</button>
            </div>
          </div>
          {slots && (
            <p className="muted small" style={{ marginBottom: 6 }}>
              p0f slots: {slots.used}/{slots.avail} used
              {canSetP0f ? ` · ${free} free — set a signature per proxy below` : " · no free slots (buy more to assign p0f)"}
            </p>
          )}
          {!items && !err && <p className="muted small">Loading proxies…</p>}
          {err && <p className="muted small">{err}</p>}
          {items && items.length === 0 && <p className="muted small">This order has no active proxies.</p>}
          {items && items.length > 0 && (
            <div className="rows ps-import-list">
              <div className="row ps-import-head">
                <input type="checkbox" checked={allChecked} onChange={toggleAll} title="Select all" />
                <span className="muted small">{sel.size} of {items.length} selected</span>
                <span className="muted small" style={{ textAlign: "right" }}>{canSetP0f ? "p0f" : ""}</span>
              </div>
              {items.map((d) => {
                const port = kind === "http" ? d.http_port : d.socks_port;
                return (
                  <div key={d.ip} className="row ps-import-row">
                    <input type="checkbox" checked={sel.has(d.ip)} onChange={() => toggle(d.ip)} />
                    <span className="mono small cell-click" onClick={() => toggle(d.ip)}>{d.ip}:{port}</span>
                    <span className="muted small">{d.username}</span>
                    <span className={`status-pill ${d.status === "active" ? "status-active" : ""}`}>{d.status}</span>
                    {(canSetP0f || d.signature) ? (
                      // Editable when free slots exist, or this IP is already
                      // signed (re-assigning an OS doesn't consume a slot).
                      <div onClick={(e) => e.stopPropagation()}>
                        <CSSelect value={sigByIp[d.ip] ?? ""} onChange={(v) => setSig(d.ip, v)} options={PS_SIGNATURES} placeholder="p0f" />
                      </div>
                    ) : (
                      <span className="muted small" style={{ textAlign: "right" }}>—</span>
                    )}
                  </div>
                );
              })}
            </div>
          )}
        </div>
        <footer className="dialog-foot">
          <button className="btn-ghost" onClick={onClose}>Cancel</button>
          <button className="btn-primary" onClick={save} disabled={saving || !items || sel.size === 0}>
            <Icon.Download /> {saving ? "Adding…" : `Add ${sel.size}`}
          </button>
        </footer>
      </div>
    </div>
  );
}

function PsTagModal({ order, onClose, onDone }: { order: PsOrder; onClose: () => void; onDone: () => void }) {
  const [tag, setTag] = useState(order.tag && order.tag !== "none" ? order.tag : "");
  const [busy, setBusy] = useState(false);
  const submit = async () => {
    setBusy(true);
    try {
      await invoke("ps_set_tag", { id: order.order_id, tag: tag.trim() || "none" });
      toast.ok(`Tag updated for #${order.order_id}`);
      onDone();
    } catch (e) { toast.err(String(e)); }
    finally { setBusy(false); }
  };
  return (
    <div className="dialog-bg" onClick={onClose}>
      <div className="dialog" onClick={(e) => e.stopPropagation()}>
        <header className="dialog-head">
          <h2><Icon.Edit /> Edit tag</h2>
          <button className="icon-btn" onClick={onClose}>✕</button>
        </header>
        <div className="dialog-body">
          <p className="muted small">{order.product_name} · order #{order.order_id}</p>
          <Field label="Tag" value={tag} onChange={setTag} placeholder="leave empty to clear" />
        </div>
        <footer className="dialog-foot">
          <button className="btn-ghost" onClick={onClose}>Cancel</button>
          <button className="btn-primary" onClick={submit} disabled={busy}><ShardMini /> {busy ? "Saving…" : "Save"}</button>
        </footer>
      </div>
    </div>
  );
}

function PsTopupModal({ order, onClose, onDone }: { order: PsOrder; onClose: () => void; onDone: () => void }) {
  const [amount, setAmount] = useState(5);
  const [promo, setPromo] = useState("");
  const [busy, setBusy] = useState(false);
  const submit = async () => {
    if (amount < 1) { toast.err("Amount must be at least 1 GB"); return; }
    setBusy(true);
    try {
      await invoke("ps_add_bandwidth", { id: order.order_id, amount, promoCode: promo.trim() || null });
      toast.ok(`Added ${amount} GB to order #${order.order_id}`);
      onDone();
    } catch (e) { toast.err(String(e)); }
    finally { setBusy(false); }
  };
  return (
    <div className="dialog-bg" onClick={onClose}>
      <div className="dialog" onClick={(e) => e.stopPropagation()}>
        <header className="dialog-head">
          <h2><ShardMini /> Add traffic</h2>
          <button className="icon-btn" onClick={onClose}>✕</button>
        </header>
        <div className="dialog-body">
          <p className="muted small">{order.product_name} · order #{order.order_id}</p>
          <NumField label="Amount (GB)" value={amount} onChange={(v) => setAmount(Math.max(1, Math.round(v)))} />
          <Field label="Promo code (optional)" value={promo} onChange={setPromo} />
        </div>
        <footer className="dialog-foot">
          <button className="btn-ghost" onClick={onClose}>Cancel</button>
          <button className="btn-primary" onClick={submit} disabled={busy}>
            <ShardMini /> {busy ? "Buying…" : `Buy ${amount} GB`}
          </button>
        </footer>
      </div>
    </div>
  );
}

type PsBuyOption = { name: string; cycles: string[]; locations: string[] };

/// available-count product code for a base product name.
const availCode = (name: string) => (/datacenter/i.test(name) ? "dc" : /isp/i.test(name) ? "isp" : "");

/// Buy a new order. DC/ISP can be bought repeatedly (quantity + country);
/// residential products can only be owned once, so any already-owned tier is
/// hidden here (top it up from the Residential card instead).
function PsBuyCard({ onPurchased }: { onPurchased: () => void }) {
  const [options, setOptions] = useState<PsBuyOption[]>([]);
  const [avail, setAvail] = useState<Record<string, number>>({});
  const [productName, setProductName] = useState("");
  const [cycle, setCycle] = useState("");
  const [country, setCountry] = useState("");
  const [quantity, setQuantity] = useState(1);
  const [promo, setPromo] = useState("");
  const [autoRenew, setAutoRenew] = useState(false);
  const [buyP0f, setBuyP0f] = useState(false);
  const [calc, setCalc] = useState<PsCalc | null>(null);
  const [calcing, setCalcing] = useState(false);
  const [buying, setBuying] = useState(false);
  const [ready, setReady] = useState(false);

  useEffect(() => {
    (async () => {
      try {
        const [prodRes, orderRes] = await Promise.all([
          invoke<any>("ps_products"),
          invoke<any>("ps_orders", { status: "all", limit: 100 }),
        ]);
        // Already-owned product names (any status) — used to hide
        // single-purchase residential tiers from the buy list.
        const owned = new Set<string>((orderRes.orders ?? []).map((o: PsOrder) => o.product_name));
        // Collapse the per-location product rows into one option per name,
        // keeping the set of locations (DC/ISP country picker). Drop mobile,
        // and drop residential tiers already owned.
        const byName = new Map<string, PsBuyOption>();
        for (const p of (prodRes.products ?? []) as PsProduct[]) {
          if (/mobile/i.test(p.name)) continue;
          if (!isDcIsp(p.name) && owned.has(p.name)) continue;
          const o = byName.get(p.name) ?? { name: p.name, cycles: p.cycles ?? [], locations: [] };
          if (p.location && !o.locations.includes(p.location)) o.locations.push(p.location);
          byName.set(p.name, o);
        }
        const list = [...byName.values()];
        setOptions(list);
        if (list[0]) {
          setProductName(list[0].name);
          setCycle(list[0].cycles?.[0] ?? "");
          setCountry(isDcIsp(list[0].name) ? (list[0].locations[0] ?? "") : "");
        }
      } catch (e) { toast.err(String(e)); }
      // available-count is best-effort (badge only).
      try {
        const arr = await invoke<any>("ps_available_count");
        const m: Record<string, number> = {};
        for (const a of (arr ?? []) as { country: string; product: string; amount: number }[]) {
          m[`${String(a.product).toLowerCase()}|${String(a.country).toUpperCase()}`] = a.amount;
        }
        setAvail(m);
      } catch { /* ignore */ }
      setReady(true);
    })();
  }, []);

  const product = useMemo(() => options.find((p) => p.name === productName) ?? null, [options, productName]);
  const needLocation = isDcIsp(productName);

  // Reset dependent fields + stale price when the product changes.
  useEffect(() => {
    setCycle(product?.cycles?.[0] ?? "");
    setCountry(product && isDcIsp(product.name) ? (product.locations[0] ?? "") : "");
    setCalc(null);
  }, [productName]); // eslint-disable-line react-hooks/exhaustive-deps

  const availForCountry = needLocation && country ? avail[`${availCode(productName)}|${country.toUpperCase()}`] : undefined;

  const buildBody = () => {
    const body: any = { product: productName };
    if (cycle) body.cycle = cycle;
    if (needLocation && country) body.location = country;
    if (quantity) body.quantity = quantity;
    if (promo.trim()) body.promo_code = promo.trim();
    if (autoRenew) body.auto_renewal = true;
    const addons = p0fAddons();
    if (addons) body.addons = addons;
    return body;
  };

  // p0f slots — one per proxy, only meaningful for DC/ISP. null when off.
  const p0fAddons = () => (needLocation && buyP0f ? [{ addon_key: "p0f_slots", qty: quantity }] : null);

  const fetchCalc = async (): Promise<PsCalc> => {
    const addons = p0fAddons();
    const r = await invoke<any>("ps_calculate", {
      product: productName,
      location: needLocation ? country || null : null,
      cycle: cycle || null,
      quantity,
      promoCode: promo.trim() || null,
      addonsJson: addons ? JSON.stringify(addons) : null,
    });
    return {
      original_price: r.original_price ?? 0,
      final_price: r.final_price ?? 0,
      discount_percent: r.discount_percent ?? 0,
      addons_price: r.addons_price ?? 0,
      total_with_addons: r.total_with_addons,
    };
  };

  const calculate = async () => {
    setCalcing(true);
    setCalc(null);
    try { setCalc(await fetchCalc()); }
    catch (e) { toast.err(String(e)); }
    finally { setCalcing(false); }
  };

  const buy = async () => {
    if (needLocation && !country) { toast.err("Pick a location for Datacenter/ISP proxies"); return; }
    // Auto-calculate when the user hasn't pressed Calculate, so the confirm
    // shows the real total (incl. add-ons) instead of a placeholder.
    let c = calc;
    if (!c) {
      try { c = await fetchCalc(); setCalc(c); } catch { /* show placeholder below */ }
    }
    const price = c ? fmtCents(c.total_with_addons ?? c.final_price) : "this order";
    const ok = await confirmModal({
      title: "Confirm purchase",
      message: `Buy ${quantity} × ${productName}${cycle ? ` (${cycle})` : ""} for ${price}? Your wallet will be charged.`,
      buttons: [
        { label: "Cancel", value: false },
        { label: "Buy", value: true, primary: true },
      ],
    });
    if (ok !== true) return;
    setBuying(true);
    try {
      const r = await invoke<any>("ps_purchase", { body: buildBody() });
      toast.ok(r.message ? `${r.message}${r.order_id ? ` (#${r.order_id})` : ""}` : "Order placed");
      setCalc(null);
      onPurchased();
    } catch (e) { toast.err(String(e)); }
    finally { setBuying(false); }
  };

  return (
    <div className="card" style={{ marginBottom: 14 }}>
      <h3>Buy proxies</h3>
      {!ready ? (
        <p className="muted small">Loading products…</p>
      ) : options.length === 0 ? (
        <p className="muted small">Nothing available to buy right now.</p>
      ) : (
        <>
          <div className="form-row">
            <label>
              <span className="lbl">Product</span>
              <CSSelect
                value={productName}
                onChange={setProductName}
                options={options.map((p) => ({ value: p.name, label: p.name }))}
              />
            </label>
            <label>
              <span className="lbl">Billing cycle</span>
              <CSSelect
                value={cycle}
                onChange={setCycle}
                placeholder="—"
                options={(product?.cycles?.length ? product.cycles : []).map((c) => ({ value: c, label: c }))}
              />
            </label>
          </div>
          <div className="form-row">
            {needLocation ? (
              <label>
                <span className="lbl">
                  Location{availForCountry != null && <span className="muted"> · {availForCountry} available</span>}
                </span>
                <CSSelect
                  value={country}
                  onChange={setCountry}
                  placeholder="Pick a country"
                  options={(product?.locations ?? []).map((l) => ({ value: l, label: l }))}
                />
              </label>
            ) : (
              <div />
            )}
            <NumField label="Quantity" value={quantity} onChange={(v) => { setQuantity(Math.max(1, Math.round(v))); setCalc(null); }} />
          </div>
          <div className="form-row">
            <Field label="Promo code (optional)" value={promo} onChange={setPromo} />
            <label className="row-inline" style={{ alignItems: "center", marginTop: 22 }}>
              <input type="checkbox" checked={autoRenew} onChange={(e) => setAutoRenew(e.target.checked)} />
              <span className="lbl">Auto-renew</span>
            </label>
          </div>
          {needLocation && (
            <label className="row-inline" style={{ marginBottom: 4 }}>
              <input type="checkbox" checked={buyP0f} onChange={(e) => { setBuyP0f(e.target.checked); setCalc(null); }} />
              <span className="lbl">Add p0f signature slots for all {quantity} prox{quantity === 1 ? "y" : "ies"}</span>
            </label>
          )}
          <div className="ps-buy-foot">
            <button className="btn-ghost" onClick={calculate} disabled={calcing || !productName}>
              {calcing ? "Calculating…" : "Calculate price"}
            </button>
            {calc && (
              <span className="ps-price">
                {calc.discount_percent > 0 && <span className="ps-price-orig">{fmtCents(calc.original_price)}</span>}
                <span className="ps-price-final">{fmtCents(calc.total_with_addons ?? calc.final_price)}</span>
                {calc.discount_percent > 0 && <span className="status-pill status-active">-{calc.discount_percent}%</span>}
                {!!calc.addons_price && calc.addons_price > 0 && (
                  <span className="muted small">incl. {fmtCents(calc.addons_price)} p0f</span>
                )}
              </span>
            )}
            <button className="btn-primary" onClick={buy} disabled={buying || !productName}>
              <ShardMini /> {buying ? "Buying…" : "Buy"}
            </button>
          </div>
        </>
      )}
    </div>
  );
}

function SettingsView() {
  const [s, setS] = useState<Settings>({
    browser_path: null,
    theme: "dark",
    geo_checker: "ip-api.com",
    screen_resolution_mode: "fingerprint",
    api_enabled: true,
    api_port: 40325,
  });
  const [api, setApi] = useState<ApiInfo | null>(null);
  const refreshApi = () => invoke<ApiInfo>("api_info").then(setApi).catch(() => {});
  useEffect(() => { invoke<Settings>("settings_get").then(setS); refreshApi(); }, []);
  const regenToken = async () => {
    try { setApi(await invoke<ApiInfo>("api_regenerate_token")); toast.ok("Token regenerated"); }
    catch (e) { toast.err(String(e)); }
  };

  const [mcpBusy, setMcpBusy] = useState(false);
  // Download MCP server source; user manages install + client setup.
  const downloadMcp = async () => {
    const dir = await open({ directory: true, title: "Where to download the MCP server" });
    if (typeof dir !== "string") return;
    setMcpBusy(true);
    try {
      const path = await invoke<string>("mcp_download", { dir });
      toast.ok(`MCP downloaded to ${path}`);
    } catch (e) { toast.err("MCP download failed: " + String(e)); }
    finally { setMcpBusy(false); }
  };
  const save = async () => {
    try { await invoke("settings_save", { value: s }); toast.ok("Settings saved"); }
    catch (e) { toast.err(String(e)); }
  };
  return (
    <section className="page settings-page">
      <Topbar crumbs={["System", "Settings"]} search="" onSearch={() => {}} />
      <div className="page-title"><h1>Settings</h1></div>


      <div className="card" style={{ marginBottom: 14 }}>
        <h3>Proxy geo checker</h3>
        <p className="muted small">Which free public IP-geo service to hit when you press the proxy <strong>Test</strong> button. All three are no-key, rate-limited.</p>
        <label>
          <span className="lbl">Provider</span>
          <select value={s.geo_checker ?? "ip-api.com"} onChange={(e) => setS({ ...s, geo_checker: e.target.value })}>
            <option value="ip-api.com">ip-api.com (45 req/min, HTTP)</option>
            <option value="ipapi.co">ipapi.co (1k/day, HTTPS)</option>
            <option value="ipwho.is">ipwho.is (10k/month, HTTPS)</option>
          </select>
        </label>
      </div>

      <div className="card" style={{ marginBottom: 14 }}>
        <h3>Screen resolution</h3>
        <p className="muted small">
          <strong>From fingerprint</strong> reports the screen carried in the bound profile (recommended for anti-detect coherence).
          <strong> Real</strong> lets ShardX expose the host monitor's actual size.
        </p>
        <label>
          <span className="lbl">Mode</span>
          <select
            value={s.screen_resolution_mode ?? "fingerprint"}
            onChange={(e) => setS({ ...s, screen_resolution_mode: e.target.value })}
          >
            <option value="fingerprint">From fingerprint</option>
            <option value="real">Real (host monitor)</option>
          </select>
        </label>
      </div>

      <div className="card" style={{ marginBottom: 14 }}>
        <h3>Automation API</h3>
        <p className="muted small">
          Local HTTP API (axum) for scripting — create/launch/close profiles
          and get a CDP WebSocket URL. Binds <strong>127.0.0.1</strong> only,
          JWT Bearer auth. Changes to enable/port apply after restarting the app.{" "}
          <a
            href="#"
            onClick={(e) => {
              e.preventDefault();
              openUrl(withUtm("https://docs.proxyshard.com/eng/shardx-launcher-api/binding-and-lifecycle?fallback=true")).catch(() => {});
            }}
          >
            Full API reference →
          </a>
        </p>
        <label className="row-inline">
          <input
            type="checkbox"
            checked={s.api_enabled ?? true}
            onChange={(e) => setS({ ...s, api_enabled: e.target.checked })}
          />
          <span className="lbl">Enable API server</span>
        </label>
        <label>
          <span className="lbl">Port</span>
          <input
            type="number"
            value={s.api_port ?? 40325}
            onChange={(e) => setS({ ...s, api_port: Number(e.target.value) || 40325 })}
          />
        </label>
        {api && (
          <>
            <label>
              <span className="lbl">Base URL</span>
              <CopyField value={api.base_url} />
            </label>
            <label>
              <span className="lbl">Bearer token</span>
              <CopyField value={api.token} secret />
            </label>
            <div className="row-inline" style={{ marginTop: 10, gap: 10 }}>
              <button className="btn-ghost" onClick={regenToken}>Regenerate token</button>
              <span className="muted small">Invalidates the current token immediately.</span>
            </div>
            <p className="muted small" style={{ marginTop: 8 }}>
              Send it as <code>Authorization: Bearer &lt;token&gt;</code>.
            </p>
          </>
        )}
      </div>

      <div className="card" style={{ marginBottom: 14 }}>
        <h3>MCP server</h3>
        <p className="muted small">
          Download the <strong>MCP</strong> server source (lets an AI client drive
          profiles and a CDP browser) into a folder you choose. The app does not run
          it — install its deps and register it with your MCP client per the included
          README. Requires Node.js.
        </p>
        <button className="btn-ghost" onClick={downloadMcp} disabled={mcpBusy}>
          <Icon.Download /> {mcpBusy ? "Downloading…" : "Download MCP server"}
        </button>
      </div>

      <div className="card-actions">
        <button className="btn-primary" onClick={async () => { await save(); refreshApi(); }}><ShardMini /> Save settings</button>
      </div>
    </section>
  );
}
