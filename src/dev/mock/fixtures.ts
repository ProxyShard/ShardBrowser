// Seed data for the in-browser Tauri IPC mock. Dev only, never bundled into a
// release: the shim is injected by vite.config.ts when VITE_MOCK=1.
//
// Shapes mirror what the Rust side returns. Profiles are stored the way
// profile.rs writes them (a FingerprintConfig with a `_meta` block), because
// the editor reads them back through `fromStored`.
import type { MockTeamServer } from "./teamServer";
import type { MockSync } from "./syncServer";
import type { SpaceData } from "./spaces";
import type { ProfileSync } from "../../entities/sync/model/types";
import type { ProfileMeta } from "../../entities/profile/model/types";
import type { ProxyEntry, ProxyTestSnapshot } from "../../entities/proxy/model/types";
import type { FingerprintEntry } from "../../entities/fingerprint/model/types";
import type { ExtensionEntry, ExtensionUpdate } from "../../entities/extension/model/types";
import type { Bookmark } from "../../entities/bookmark/model/types";
import type { TrashEntry } from "../../entities/trash/model/types";
import type { Settings, ApiInfo } from "../../entities/settings/model/types";
import type { ModuleInfo, Project } from "../../entities/automation/model/types";
import type { PsOrder, PsProduct, PsActiveProxy } from "../../entities/proxyshard/model/types";

/** Bump when the seed changes shape so stale localStorage state is dropped. */
export const SEED_VERSION = 7;

export const DEFAULT_BLOCKED_PORTS = [3389, 5900, 5901, 5800, 7070, 6568, 5938, 1080, 8080, 3128, 3030];

/** A profile as profile.rs keeps it on disk. `_meta` carries the launcher's
 *  own fields; the rest is the FingerprintConfig the engine consumes. */
export type StoredProfile = {
  _meta: {
    id: string;
    rev: number;
    proxy_id: string | null;
    last_launched_at: string | null;
    created_at: string | null;
    pinned: boolean;
    folder: string;
    total_runtime_ms: number;
    /** Backend B41: starts and proxy traffic in all; left out for a profile never run. */
    launches?: number;
    traffic_bytes?: number;
    color?: string;
    gpu_preset_id: string;
    extensions: string[];
    android_media?: boolean;
  };
  name: string;
  notes: string;
  timezone: string;
  icu_locale: string | null;
  /// Browser menus in the profile's language (#81); absent = this computer's.
  ui_language?: "profile";
  webrtc: string;
  navigator: Record<string, unknown>;
  client_hints?: Record<string, unknown>;
  screen: Record<string, number>;
  media_devices: { audio_input_count: number; audio_output_count: number; video_input_count: number };
  geolocation: Record<string, unknown>;
  noise: Record<string, { enabled: boolean; seed: number; intensity?: number; max_offset?: number }>;
  blocked_ports: number[];
};

export type MockTags = { tags: { id: string; name: string; color: string }[]; by_profile: Record<string, string[]> };
export type MockFolders = { registry: string[]; meta: Record<string, { color: string | null; icon: string }> };

export type MockState = {
  version: number;
  profiles: Record<string, StoredProfile>;
  /** Insertion order, which is what profile_list returns. */
  order: string[];
  proxies: ProxyEntry[];
  snapshots: Record<string, ProxyTestSnapshot[]>;
  fingerprints: FingerprintEntry[];
  extensions: ExtensionEntry[];
  /** Web Store update records by entry id (extUpdates.ts); made on first read. */
  extUpdates?: Record<string, ExtensionUpdate>;
  bookmarks: Bookmark[];
  trash: TrashEntry[];
  /** The profiles behind the Trash entries moved there in this session, so a
   *  restore gives back exactly what went in. Seeded entries have none. */
  trashed?: Record<string, StoredProfile>;
  /** Where each trashed profile stood in `order`, so a restore puts it back
   *  (as the backend lists by creation date). */
  trashedAt?: Record<string, number>;
  /** As settings.json holds it: the API signing key too (backend B3). */
  settings: Settings & { api_secret: string };
  api: ApiInfo;
  psKey: string;
  psOrders: PsOrder[];
  /** As tags.rs keeps them; absent until the first write or import. */
  tags?: MockTags;
  /** As folders.rs keeps them; absent until the first write or import. */
  folders?: MockFolders;
  /** The space the launcher works in and the teams joined (team.rs). */
  space?: { kind: "personal" } | { kind: "team"; team_id: string };
  /** `needs_invite`: the admin replaced the token (live.rs token_replaced); a new join clears it. */
  teams?: { id: string; name: string; server: string; admin: boolean; revoked?: boolean; needs_invite?: boolean }[];
  /** Each joined team's server: its members, rules, folders and profiles (teamServer.ts). */
  teamServers?: Record<string, MockTeamServer>;
  /** Whose lists the fields above hold: "personal" (or absent) or a team id (spaces.ts). */
  dataOf?: string;
  /** The lists of the spaces not in use, by "personal" or team id. */
  spaceData?: Record<string, SpaceData>;
  /** Personal sync and what its servers keep (syncServer.ts). */
  sync?: MockSync;
  /** The wallet in cents once a purchase has charged it; absent: PS_ME's. */
  psBalance?: number;
  automation: Project[];
  modules: ModuleInfo[];
  /** profile id -> pid + wall-clock start. */
  running: Record<string, { pid: number; started: number }>;
  /** Team profiles closed here whose session is still being sent (open.rs resends it): listed, a start refused. */
  saving?: Record<string, true>;
  /** Team profiles whose session is kept here unsent (Outcome::Unsent): the next start sends it first. */
  unsent?: Record<string, true>;
  /** The folder Delete all data moved the old data into (app_reset_backup). */
  backup?: string;
  /** A Delete all data that did not finish, as app_reset_pending answers. */
  resetPending?: { journal: string; backups: string[]; moved: number; total: number; missing: string[]; error: string | null };
  nextPid: number;
};

const iso = (daysAgo: number, hour = 10) => {
  const d = new Date();
  d.setDate(d.getDate() - daysAgo);
  d.setHours(hour, 12, 0, 0);
  return d.toISOString();
};
const unix = (daysAgo: number) => Math.floor(Date.now() / 1000) - daysAgo * 86400;
/** profile.rs writes launcher timestamps as "@<unix seconds>" (see fmtTs). */
const stamp = (daysAgo: number, hour = 10) => `@${Math.floor(Date.parse(iso(daysAgo, hour)) / 1000)}`;

// ---- Fingerprints -----------------------------------------------------------

type FpSeed = {
  id: string;
  label: string;
  platform: string;
  chrome: string;
  gpu: string;
  tag_color: string;
  ua: string;
  cores: number;
  memory: number;
  screen: [number, number, number, number];
  platform_version: string;
};

const FP: FpSeed[] = [
  { id: "mac-m3-pro", label: "macOS 15, Apple M3 Pro", platform: "macOS", chrome: "152.0.7977.65", gpu: "Apple M3 Pro", tag_color: "#8b5cf6", ua: "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/152.0.0.0 Safari/537.36", cores: 12, memory: 8, screen: [1728, 1117, 1728, 1079], platform_version: "15.4.1" },
  { id: "mac-m1", label: "macOS 14, Apple M1", platform: "macOS", chrome: "152.0.7977.65", gpu: "Apple M1", tag_color: "#8b5cf6", ua: "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/152.0.0.0 Safari/537.36", cores: 8, memory: 8, screen: [1440, 900, 1440, 862], platform_version: "14.6.1" },
  { id: "mac-intel-iris", label: "macOS 13, Intel Iris Plus 655", platform: "macOS", chrome: "152.0.7977.65", gpu: "Intel Iris Plus Graphics 655", tag_color: "#8b5cf6", ua: "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/152.0.0.0 Safari/537.36", cores: 8, memory: 8, screen: [2560, 1600, 2560, 1575], platform_version: "13.7.2" },
  { id: "win11-rtx4070", label: "Windows 11, NVIDIA RTX 4070", platform: "Windows", chrome: "152.0.7977.65", gpu: "NVIDIA GeForce RTX 4070", tag_color: "#3b82f6", ua: "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/152.0.0.0 Safari/537.36", cores: 16, memory: 8, screen: [2560, 1440, 2560, 1392], platform_version: "15.0.0" },
  { id: "win11-rtx3060", label: "Windows 11, NVIDIA RTX 3060", platform: "Windows", chrome: "152.0.7977.65", gpu: "NVIDIA GeForce RTX 3060", tag_color: "#3b82f6", ua: "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/152.0.0.0 Safari/537.36", cores: 12, memory: 8, screen: [1920, 1080, 1920, 1032], platform_version: "15.0.0" },
  { id: "win10-uhd630", label: "Windows 10, Intel UHD 630", platform: "Windows", chrome: "152.0.7977.65", gpu: "Intel(R) UHD Graphics 630", tag_color: "#3b82f6", ua: "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/152.0.0.0 Safari/537.36", cores: 8, memory: 8, screen: [1920, 1080, 1920, 1040], platform_version: "10.0.0" },
  { id: "win10-rx6600", label: "Windows 10, AMD RX 6600", platform: "Windows", chrome: "152.0.7977.65", gpu: "AMD Radeon RX 6600", tag_color: "#3b82f6", ua: "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/152.0.0.0 Safari/537.36", cores: 6, memory: 8, screen: [1920, 1080, 1920, 1040], platform_version: "10.0.0" },
  { id: "win11-arc-a770", label: "Windows 11, Intel Arc A770", platform: "Windows", chrome: "152.0.7977.65", gpu: "Intel(R) Arc(TM) A770 Graphics", tag_color: "#3b82f6", ua: "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/152.0.0.0 Safari/537.36", cores: 20, memory: 8, screen: [3440, 1440, 3440, 1392], platform_version: "15.0.0" },
  { id: "linux-mesa-intel", label: "Ubuntu 24.04, Mesa Intel Xe", platform: "Linux", chrome: "152.0.7977.65", gpu: "Mesa Intel(R) Xe Graphics (TGL GT2)", tag_color: "#f59e0b", ua: "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/152.0.0.0 Safari/537.36", cores: 8, memory: 8, screen: [1920, 1080, 1920, 1053], platform_version: "6.8.0" },
  { id: "linux-nvidia-1660", label: "Fedora 40, NVIDIA GTX 1660", platform: "Linux", chrome: "152.0.7977.65", gpu: "NVIDIA GeForce GTX 1660/PCIe/SSE2", tag_color: "#f59e0b", ua: "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/152.0.0.0 Safari/537.36", cores: 6, memory: 8, screen: [2560, 1440, 2560, 1413], platform_version: "6.9.0" },
  { id: "android-pixel-8", label: "Android 15, Pixel 8, Mali-G715", platform: "Linux armv81", chrome: "152.0.7977.65", gpu: "Mali-G715-Immortalis MC10", tag_color: "#10b981", ua: "Mozilla/5.0 (Linux; Android 15; Pixel 8) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/152.0.0.0 Mobile Safari/537.36", cores: 9, memory: 8, screen: [412, 915, 412, 915], platform_version: "15.0.0" },
  { id: "android-s24", label: "Android 14, Galaxy S24, Adreno 750", platform: "Linux armv81", chrome: "152.0.7977.65", gpu: "Adreno (TM) 750", tag_color: "#10b981", ua: "Mozilla/5.0 (Linux; Android 14; SM-S921B) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/152.0.0.0 Mobile Safari/537.36", cores: 8, memory: 8, screen: [360, 780, 360, 780], platform_version: "14.0.0" },
];

function fingerprint(s: FpSeed): FingerprintEntry {
  const [width, height, avail_width, avail_height] = s.screen;
  return {
    id: s.id,
    label: s.label,
    platform: s.platform,
    chrome: s.chrome,
    gpu: s.gpu,
    tag_color: s.tag_color,
    builtin: true,
    payload: {
      navigator: {
        user_agent: s.ua,
        platform: s.platform,
        hardware_concurrency: s.cores,
        device_memory: s.memory,
        language: "en-US",
        platform_version: s.platform_version,
      },
      client_hints: { platform_version: s.platform_version },
      screen: { width, height, avail_width, avail_height, color_depth: 24, device_pixel_ratio: 2, refresh_rate: 60 },
      webgl: { vendor: "Google Inc.", renderer: `ANGLE (${s.gpu})` },
    },
  };
}

// ---- Profiles ---------------------------------------------------------------

type ProfileSeed = {
  id: string;
  name: string;
  notes: string;
  folder: string;
  fp: string;
  proxy: string | null;
  pinned?: boolean;
  color?: string;
  createdDaysAgo: number;
  launchedDaysAgo: number | null;
  /** A last run at an exact ms offset from now, for boundary tests `stamp`'s
   *  day-and-hour grain cannot hit (e2e/profile-status.spec.ts). Wins over
   *  `launchedDaysAgo` when set. */
  launchedMsAgo?: number;
  runtimeHours: number;
  /** Starts and proxy traffic so far (backend B41). */
  launches?: number;
  trafficGB?: number;
  extensions?: string[];
  running?: boolean;
  /** A timezone set by hand; "auto" (from the proxy) when left out. */
  timezone?: string;
};

const PROFILES: ProfileSeed[] = [
  { id: "a78a0eea", name: "Amazon US seller", notes: "Main storefront account. Do not rotate proxy.", folder: "Shops", fp: "win11-rtx4070", proxy: "px-us-1", pinned: true, color: "#f59e0b", createdDaysAgo: 41, launchedDaysAgo: 0, runtimeHours: 212, launches: 186, trafficGB: 4.8, extensions: ["ublock"], running: true },
  { id: "9828bf28", name: "eBay DE buyer", notes: "", folder: "Shops", fp: "win10-uhd630", proxy: "px-de-1", createdDaysAgo: 38, launchedDaysAgo: 0, runtimeHours: 96, launches: 88, trafficGB: 2.1, running: true },
  { id: "aed41005", name: "Etsy craft shop", notes: "Payout Tuesday", folder: "Shops", fp: "mac-m1", proxy: "px-us-2", createdDaysAgo: 30, launchedDaysAgo: 2, runtimeHours: 40, launches: 41, trafficGB: 1.2 },
  { id: "c31d92f0", name: "FB Ads acc-01", notes: "Warm-up week 2", folder: "Ads", fp: "win11-rtx3060", proxy: "px-us-1", pinned: true, color: "#3b82f6", createdDaysAgo: 12, launchedDaysAgo: 1, runtimeHours: 18, launches: 26, trafficGB: 0.9, extensions: ["ublock", "cookie-editor"] },
  { id: "c31d92f1", name: "FB Ads acc-02", notes: "", folder: "Ads", fp: "win11-rtx3060", proxy: "px-jp-1", createdDaysAgo: 12, launchedDaysAgo: 3, runtimeHours: 11, launches: 14, trafficGB: 0.4, timezone: "America/New_York" },
  { id: "d40e7b21", name: "TikTok creator", notes: "Mobile fingerprint, sticky session", folder: "Ads", fp: "android-pixel-8", proxy: "px-mobile-1", color: "#10b981", createdDaysAgo: 9, launchedDaysAgo: 5, runtimeHours: 7, launches: 9, trafficGB: 0.6 },
  { id: "e51f8c32", name: "Scraper pricing", notes: "Runs from the automation project every 6h", folder: "", fp: "linux-mesa-intel", proxy: "px-us-2", createdDaysAgo: 20, launchedDaysAgo: 0, runtimeHours: 301, launches: 420, trafficGB: 7.3, running: false },
  { id: "f62a9d43", name: "Sandbox", notes: "Throwaway. Direct connection.", folder: "", fp: "mac-m3-pro", proxy: null, createdDaysAgo: 3, launchedDaysAgo: null, runtimeHours: 0 },
];

function stored(s: ProfileSeed, fps: FingerprintEntry[]): StoredProfile {
  const fp = fps.find((f) => f.id === s.fp) ?? fps[0];
  const nav = { ...(fp.payload.navigator as Record<string, unknown>) };
  return {
    _meta: {
      id: s.id,
      rev: 1,
      proxy_id: s.proxy,
      last_launched_at:
        s.launchedMsAgo !== undefined ? `@${Math.floor((Date.now() - s.launchedMsAgo) / 1000)}`
        : s.launchedDaysAgo === null ? null
        : stamp(s.launchedDaysAgo, 9),
      created_at: stamp(s.createdDaysAgo),
      pinned: !!s.pinned,
      folder: s.folder,
      total_runtime_ms: Math.round(s.runtimeHours * 3600 * 1000),
      ...(s.launches !== undefined ? { launches: s.launches } : {}),
      ...(s.trafficGB !== undefined ? { traffic_bytes: Math.round(s.trafficGB * 1024 ** 3) } : {}),
      ...(s.color ? { color: s.color } : {}),
      gpu_preset_id: fp.id,
      extensions: s.extensions ?? [],
      ...(fp.platform.startsWith("Linux arm") ? { android_media: true } : {}),
    },
    name: s.name,
    notes: s.notes,
    timezone: s.timezone ?? "auto",
    icu_locale: null,
    webrtc: "auto",
    // As the backend makes a profile from a template (lib.rs): the language
    // follows the proxy until someone sets one.
    navigator: { ...nav, language: "auto", do_not_track: null },
    client_hints: { ...(fp.payload.client_hints as Record<string, unknown>) },
    screen: { ...(fp.payload.screen as Record<string, number>) },
    media_devices: { audio_input_count: 1, audio_output_count: 1, video_input_count: 1 },
    geolocation: { mode: "auto" },
    noise: {
      canvas: { enabled: true, seed: 0 },
      webgl: { enabled: true, seed: 0, intensity: 0.3 },
      audio: { enabled: true, seed: 0 },
      client_rects: { enabled: false, seed: 0, max_offset: 0 },
      sensors: { enabled: false, seed: 0 },
      fonts: { enabled: false, seed: 0 },
    },
    blocked_ports: DEFAULT_BLOCKED_PORTS.slice(),
  };
}

/** A profile's sync as the list reads it; syncServer.ts fills it in. */
export const syncLink: { of: (id: string) => ProfileSync | undefined } = { of: () => undefined };

/** ProfileMeta the list shows, derived from the stored file. */
export function metaOf(p: StoredProfile): ProfileMeta {
  const sync = syncLink.of(p._meta.id);
  const ua = String((p.navigator as { user_agent?: string } | undefined)?.user_agent ?? "");
  return {
    id: p._meta.id,
    name: p.name,
    notes: p.notes,
    proxy_id: p._meta.proxy_id,
    last_launched_at: p._meta.last_launched_at,
    created_at: p._meta.created_at,
    pinned: p._meta.pinned,
    folder: p._meta.folder,
    total_runtime_ms: p._meta.total_runtime_ms,
    ...(p._meta.launches !== undefined ? { launches: p._meta.launches } : {}),
    ...(p._meta.traffic_bytes !== undefined ? { traffic_bytes: p._meta.traffic_bytes } : {}),
    color: p._meta.color ?? null,
    extensions: p._meta.extensions,
    mobile: /android/i.test(ua),
    ...(sync ? { sync } : {}),
  };
}

// ---- Proxies ----------------------------------------------------------------

// Hosts are .example names and documentation-range addresses (RFC 5737), and
// every secret is plainly fake (SEC-16).
const PROXIES: ProxyEntry[] = [
  { id: "px-us-1", name: "ProxyShard resi US", kind: "socks5", host: "relay-eu.proxyshard.example", port: 1080, username: "plan-premium_country-us_session-a1f9", password: "mock-pass-1", country: "US", notes: "Sticky session, premium tier" },
  { id: "px-us-2", name: "DC New York", kind: "socks5", host: "203.0.113.18", port: 1080, username: "shard", password: "mock-pass-2", country: "US", notes: "" },
  { id: "px-de-1", name: "ISP Frankfurt", kind: "http", host: "198.51.100.7", port: 8080, username: "shard", password: "mock-pass-3", country: "DE", notes: "Unlimited ISP" },
  { id: "px-jp-1", name: "DC Tokyo", kind: "socks5", host: "203.0.113.90", port: 1080, username: "shard", password: "mock-pass-4", country: "JP", notes: "" },
  { id: "px-mobile-1", name: "Mobile UK EE", kind: "socks5", host: "relay-ru.proxyshard.example", port: 1080, username: "plan-mobile_country-gb_session-77c0", password: "mock-pass-5", country: "GB", notes: "Rotates every 10 min" },
  { id: "px-dead", name: "Old datacenter (expired)", kind: "https", host: "192.0.2.66", port: 443, username: "", password: "", country: "NL", notes: "Order lapsed 2 weeks ago" },
  // Never tested (e2e/status-dots.spec.ts): no entry in the snapshot loop
  // below, so `proxyStatus` sees no history at all — the one seed proxy
  // whose Test result is genuinely "Not tested", not just "failed".
  { id: "px-new", name: "New DC Warsaw", kind: "socks5", host: "192.0.2.44", port: 1080, username: "shard", password: "mock-pass-7", country: "PL", notes: "Not tested yet" },
];

function snapshot(p: ProxyEntry, minutesAgo: number, ok = true): ProxyTestSnapshot {
  const geo: Record<string, [string, string, string, string, string, number, number]> = {
    US: ["United States", "New York", "New York", "Verizon Fios", "America/New_York", 40.7128, -74.006],
    DE: ["Germany", "Hesse", "Frankfurt am Main", "Deutsche Telekom AG", "Europe/Berlin", 50.1109, 8.6821],
    JP: ["Japan", "Tokyo", "Tokyo", "NTT Communications", "Asia/Tokyo", 35.6762, 139.6503],
    GB: ["United Kingdom", "England", "Manchester", "EE Limited", "Europe/London", 53.4808, -2.2426],
    NL: ["Netherlands", "North Holland", "Amsterdam", "WorldStream", "Europe/Amsterdam", 52.3676, 4.9041],
  };
  const g = geo[p.country] ?? geo.US;
  const t = `@${Math.floor((Date.now() - minutesAgo * 60_000) / 1000)}`;
  const base = (p.id.charCodeAt(3) * 7) % 90;
  return {
    first_seen: new Date(Date.now() - (minutesAgo + 3 * 24 * 60) * 60_000).toISOString(),
    last_seen: t,
    ip: ok ? `${20 + base}.${100 + base}.${base}.${(base * 3) % 250 + 1}` : "",
    country_code: ok ? p.country : "",
    country: ok ? g[0] : "",
    region: ok ? g[1] : "",
    city: ok ? g[2] : "",
    isp: ok ? g[3] : "",
    timezone: ok ? g[4] : "",
    latitude: ok ? g[5] : 0,
    longitude: ok ? g[6] : 0,
    tcp_ms: ok ? 38 + base * 2 : null,
    udp_ms: ok && p.kind === "socks5" && p.host.includes("proxyshard") ? 52 + base * 2 : null,
    udp_error: ok && !(p.kind === "socks5" && p.host.includes("proxyshard")) ? "SOCKS5 UDP ASSOCIATE refused" : null,
    geo_error: ok ? null : "connect timed out after 8000 ms",
    provider: "ip-api.com",
  };
}

// ---- Library ----------------------------------------------------------------

const EXTENSIONS: ExtensionEntry[] = [
  { id: "ublock", name: "uBlock Origin Lite", version: "2025.1.14.1103", description: "An efficient content blocker. Easy on CPU and memory.", icon: "", path: "/Users/you/Library/Application Support/ShardX/extensions/ublock", size_bytes: 3_412_990, added_at: `@${unix(30)}`, source: { kind: "webstore", id: "ddkjiahejlhfcafbddmgiahcphecmpfh" } },
  { id: "cookie-editor", name: "Cookie-Editor", version: "1.13.0", description: "Simple yet powerful Cookie Editor. Create, edit and delete cookies without leaving your tab.", icon: "", path: "/Users/you/Library/Application Support/ShardX/extensions/cookie-editor", size_bytes: 612_400, added_at: `@${unix(14)}`, source: { kind: "webstore", id: "hlkenndednhfkekhgcdicdfddnkalmdm" } },
  { id: "metamask", name: "MetaMask", version: "12.9.3", description: "An Ethereum Wallet in your Browser.", icon: "", path: "/Users/you/Library/Application Support/ShardX/extensions/metamask", size_bytes: 41_320_112, added_at: `@${unix(6)}` },
];

const BOOKMARKS: Bookmark[] = [
  { id: "bm-1", title: "Seller Central", url: "https://sellercentral.amazon.com", folder: "Shops" },
  { id: "bm-2", title: "eBay Seller Hub", url: "https://www.ebay.de/sh/ovw", folder: "Shops" },
  { id: "bm-3", title: "Meta Business Suite", url: "https://business.facebook.com", folder: "Ads" },
  { id: "bm-4", title: "browserleaks.com", url: "https://browserleaks.com/ip", folder: "" },
  { id: "bm-5", title: "pixelscan.net", url: "https://pixelscan.net", folder: "" },
];

// As the backend: seven days in the Trash (trash.rs RETENTION_DAYS).
const TRASH: TrashEntry[] = [
  { id: "t-1", name: "Amazon UK old", folder: "Shops", deleted_at: unix(4), expires_at: unix(-3), size_bytes: 188_500_000 },
  { id: "t-2", name: "Test profile 3", folder: "", deleted_at: unix(6), expires_at: unix(-1), size_bytes: 12_100_000 },
];

// ---- ProxyShard -------------------------------------------------------------

/** wallet_balance is in cents, as user-api returns it (fmtMoney divides). */
export const PS_ME = { email: "you@example.com", active_orders: 3, wallet_balance: 12840 };

const PS_ORDERS: PsOrder[] = [
  { order_id: 41822, product_name: "Residential Premium", cycle_name: "monthly", expires_at: iso(-19), auto_renewal: true, tag: "main" },
  { order_id: 41390, product_name: "Datacenter USA", cycle_name: "monthly", expires_at: iso(-8), auto_renewal: false, tag: "dc-ny" },
  { order_id: 40977, product_name: "ISP Germany", cycle_name: "monthly", expires_at: iso(-2), auto_renewal: true, tag: null },
  { order_id: 39105, product_name: "Datacenter Netherlands", cycle_name: "monthly", expires_at: iso(14), auto_renewal: false, tag: "old" },
];

export const PS_PRODUCTS: PsProduct[] = [
  { name: "Residential Premium", description: "Real ISP residential IPs, sticky or rotating", location: null, cycles: ["monthly"] },
  { name: "Residential Standart", description: "Shared residential pool, pay per GB", location: null, cycles: ["monthly"] },
  { name: "Residential Unlimited", description: "Unmetered residential bandwidth", location: null, cycles: ["monthly"] },
  { name: "Datacenter USA", location: "US", cycles: ["monthly", "quarterly"] },
  { name: "Datacenter Germany", location: "DE", cycles: ["monthly", "quarterly"] },
  { name: "Datacenter Netherlands", location: "NL", cycles: ["monthly", "quarterly"] },
  { name: "Datacenter Japan", location: "JP", cycles: ["monthly"] },
  { name: "ISP Germany", location: "DE", cycles: ["monthly"] },
  { name: "ISP USA", location: "US", cycles: ["monthly"] },
  { name: "Mobile UK", location: "GB", cycles: ["monthly"] },
];

export const PS_AVAILABLE = [
  { country: "US", product: "datacenter", amount: 412 },
  { country: "DE", product: "datacenter", amount: 96 },
  { country: "NL", product: "datacenter", amount: 220 },
  { country: "JP", product: "datacenter", amount: 18 },
  { country: "DE", product: "isp", amount: 31 },
  { country: "US", product: "isp", amount: 74 },
];

export const PS_ACTIVE: Record<number, PsActiveProxy[]> = {
  41390: [
    { ip: "203.0.113.18", username: "shard", password: "mock-pass-2", http_port: 8080, socks_port: 1080, until: iso(-8), status: "active", signature: "win11" },
    { ip: "203.0.113.19", username: "shard", password: "mock-pass-2", http_port: 8080, socks_port: 1080, until: iso(-8), status: "active", signature: null },
    { ip: "203.0.113.20", username: "shard", password: "mock-pass-2", http_port: 8080, socks_port: 1080, until: iso(-8), status: "active", signature: "macos" },
  ],
  40977: [
    { ip: "198.51.100.7", username: "shard", password: "mock-pass-3", http_port: 8080, socks_port: 1080, until: iso(-2), status: "active", signature: "win10" },
  ],
  39105: [
    { ip: "192.0.2.66", username: "shard", password: "", http_port: 8080, socks_port: 1080, until: iso(14), status: "expired", signature: null },
  ],
};

const GB = 1024 ** 3;
/** In bytes, as fmtBytes reads them, until the backend confirms the unit (backend B17). */
export const PS_TRAFFIC: Record<string, { data: number; data_remain: number; data_spent: number; proxy_password: string }> = {
  premium: { data: 50 * GB, data_remain: 31.6 * GB, data_spent: 18.4 * GB, proxy_password: "mock-pass-1" },
  standart: { data: 20 * GB, data_remain: 13.2 * GB, data_spent: 6.8 * GB, proxy_password: "mock-pass-6" },
  unlimited: { data: 0, data_remain: 0, data_spent: 0, proxy_password: "" },
};

/** Usage history for `ps_usage` (backend B40), made up but steady: the same
 *  numbers for the same clock, so screenshots hold. The premium plan and the
 *  Datacenter USA order have history; ISP Germany has none, so its card
 *  shows the No data ghost, as an order the backend has no numbers for. */
const USAGE_RANGES: Record<string, { step: number; n: number }> = {
  "24h": { step: 3_600_000, n: 24 },
  "7d": { step: 6 * 3_600_000, n: 28 },
  "30d": { step: 86_400_000, n: 30 },
};
/** A steady wobble around 1 for bucket i: two waves and a little noise. */
const wobble = (i: number, seed: number) => {
  const noise = Math.abs(Math.sin(i * 12.9898 + seed * 78.233) * 43758.5453) % 1;
  return Math.max(0.15, 1 + 0.35 * Math.sin(i * 0.9 + seed) + 0.25 * Math.sin(i * 0.37 + seed * 2) + 0.5 * (noise - 0.5));
};
export function psUsage(range: string) {
  const { step, n } = USAGE_RANGES[range] ?? USAGE_RANGES["7d"];
  const from = Math.floor(Date.now() / step) * step - (n - 1) * step;
  const perDay = step / 86_400_000;
  const series = (dailyBase: number, seed: number, prevShare: number, round = false) => {
    const points = Array.from({ length: n }, (_, i) => {
      const v = dailyBase * perDay * wobble(i, seed);
      return round ? Math.round(v) : v;
    });
    const total = points.reduce((a, b) => a + b, 0);
    return { points, previous: round ? Math.round(total * prevShare) : total * prevShare };
  };
  const requests = series(7000, 3, 1.03, true);
  const failed = series(26, 7, 1.18, true);
  return {
    from,
    step,
    traffic: { premium: series(1.67 * GB, 1, 0.85) },
    requests,
    failed,
  };
}

export const PS_COUNTRIES = [
  { code: "US", name: "United States" }, { code: "GB", name: "United Kingdom" }, { code: "DE", name: "Germany" },
  { code: "FR", name: "France" }, { code: "NL", name: "Netherlands" }, { code: "JP", name: "Japan" },
  { code: "BR", name: "Brazil" }, { code: "IN", name: "India" }, { code: "CA", name: "Canada" }, { code: "AU", name: "Australia" },
];
export const PS_REGIONS: Record<string, { code: string; name: string }[]> = {
  US: [{ code: "ny", name: "New York" }, { code: "ca", name: "California" }, { code: "tx", name: "Texas" }, { code: "fl", name: "Florida" }],
  GB: [{ code: "eng", name: "England" }, { code: "sct", name: "Scotland" }],
  DE: [{ code: "he", name: "Hesse" }, { code: "by", name: "Bavaria" }, { code: "be", name: "Berlin" }],
};
export const PS_CITIES: Record<string, { code: string; name: string }[]> = {
  ny: [{ code: "newyork", name: "New York" }, { code: "buffalo", name: "Buffalo" }],
  ca: [{ code: "losangeles", name: "Los Angeles" }, { code: "sanfrancisco", name: "San Francisco" }],
  he: [{ code: "frankfurt", name: "Frankfurt am Main" }],
  eng: [{ code: "london", name: "London" }, { code: "manchester", name: "Manchester" }],
};
export const PS_ISPS = [
  { code: "verizon", name: "Verizon Fios" }, { code: "comcast", name: "Comcast Cable" }, { code: "att", name: "AT&T Internet" },
];

// ---- Automation -------------------------------------------------------------

const AUTOMATION: Project[] = [
  {
    id: "proj-pricing",
    name: "Pricing scrape 6h",
    notes: "Opens the competitor list, reads prices, posts to the sheet.",
    blocks: [
      { id: "b1", kind: "profile.use", label: "Use profile", params: { id: "e51f8c32" }, enabled: true, secrets: [], x: 80, y: 120, on_done: "next", on_fail: "stop" },
      { id: "b2", kind: "page.goto", label: "Open page", params: { url: "https://example-competitor.com/catalog" }, enabled: true, secrets: [], x: 340, y: 120, on_done: "next", on_fail: { retry: 2 } },
      { id: "b3", kind: "page.wait", label: "Wait for table", params: { selector: "table.prices", timeout: 15000 }, enabled: true, secrets: [], x: 600, y: 120, on_done: "next", on_fail: "stop" },
      { id: "b4", kind: "data.extract", label: "Read prices", params: { selector: "table.prices tr", into: "rows" }, enabled: true, secrets: [], x: 860, y: 120, on_done: "next", on_fail: "stop" },
      { id: "b5", kind: "http.post", label: "Post to sheet", params: { url: "https://hooks.example.com/sheet", body: "{{rows}}", token: "" }, enabled: true, secrets: ["token"], x: 1120, y: 120, on_done: "endpass", on_fail: { retry: 3 } },
    ],
    run: { threads: 1, loops: 0, hours: 6, profiles: [], start: "b1" },
    rules: [],
    created_at: unix(20),
    updated_at: unix(1),
  },
  {
    id: "proj-warmup",
    name: "Ads account warm-up",
    notes: "Human-paced browsing across the Ads folder, 20 minutes per pass.",
    blocks: [
      { id: "w1", kind: "profile.keep", label: "Each profile in folder", params: { folder: "Ads" }, enabled: true, secrets: [], x: 80, y: 120, on_done: "next", on_fail: "stop" },
      { id: "w2", kind: "page.goto", label: "Open feed", params: { url: "https://www.facebook.com" }, enabled: true, secrets: [], x: 340, y: 120, on_done: "next", on_fail: "stop" },
      { id: "w3", kind: "human.scroll", label: "Scroll like a person", params: { minutes: 4 }, enabled: true, secrets: [], x: 600, y: 120, on_done: "next", on_fail: "next" },
      { id: "w4", kind: "human.click", label: "Open a post", params: { selector: "[role=article] a" }, enabled: false, secrets: [], x: 860, y: 120, on_done: "next", on_fail: "next" },
      { id: "w5", kind: "flow.wait", label: "Idle", params: { seconds: 90 }, enabled: true, secrets: [], x: 1120, y: 120, on_done: "endpass", on_fail: "endpass" },
    ],
    run: { threads: 2, loops: 3, hours: 0, profiles: [], start: "" },
    rules: [],
    created_at: unix(9),
    updated_at: unix(2),
  },
];

const MODULES: ModuleInfo[] = [
  {
    id: "sheets",
    name: "Google Sheets",
    path: "/Users/you/Library/Application Support/ShardX/modules/sheets.wasm",
    error: "",
    blocks: [
      { kind: "sheets.append", label: "Append row", about: "Appends a row to a sheet using a service account.", group: "data", params: [
        { name: "sheet", label: "Sheet id", kind: "text" },
        { name: "row", label: "Row (JSON array)", kind: "text" },
        { name: "key", label: "Service key", kind: "text", secret: true },
      ] },
    ],
  },
];

// ---- Assemble ---------------------------------------------------------------

/** `window.__shardxMockProfiles = N`, set before load, adds N plain profiles
 *  ("Profile 0001" …) to try the list at scale. */
function extraProfiles(): ProfileSeed[] {
  const n = Number((window as { __shardxMockProfiles?: number }).__shardxMockProfiles ?? 0);
  return Array.from({ length: n }, (_, k) => {
    const i = k + 1;
    return {
      id: `bulk-${i}`, name: `Profile ${String(i).padStart(4, "0")}`, notes: "", folder: "",
      fp: FP[i % FP.length].id, proxy: null, createdDaysAgo: i % 90,
      launchedDaysAgo: i % 7 === 0 ? null : i % 30, runtimeHours: i % 40,
    };
  });
}

/** `window.__shardxMockFolders = N`, set before load: N more profiles
 *  ("Folder profile 01" …), each in a Folder of its own ("Folder 01" …), to
 *  try the sidebar at scale. `window.__shardxMockPinned = M` pins the first M
 *  of them. */
function extraFolderProfiles(): ProfileSeed[] {
  const w = window as { __shardxMockFolders?: number; __shardxMockPinned?: number };
  const n = Number(w.__shardxMockFolders ?? 0);
  const pinned = Number(w.__shardxMockPinned ?? 0);
  return Array.from({ length: n }, (_, k) => {
    const two = String(k + 1).padStart(2, "0");
    return {
      id: `folder-${two}`, name: `Folder profile ${two}`, notes: "", folder: `Folder ${two}`,
      fp: FP[(k + 1) % FP.length].id, proxy: null, pinned: k < pinned,
      createdDaysAgo: k + 1, launchedDaysAgo: null, runtimeHours: 0,
    };
  });
}

/** `window.__shardxMockStatusProbes = true`, set before load: adds profiles
 *  at exact ms offsets from now, one per bucket of the Status cell's
 *  relative-time formatter (e2e/profile-status.spec.ts) — `stamp`'s
 *  day-and-hour grain cannot hit "59 minutes ago" or "23 hours ago". Off by
 *  default, so every other test's profile list is untouched. */
function statusProbes(): ProfileSeed[] {
  if (!(window as { __shardxMockStatusProbes?: boolean }).__shardxMockStatusProbes) return [];
  const DAY = 24 * 3600_000;
  const seeds: [id: string, name: string, msAgo: number][] = [
    ["probe-just-now", "Status probe just now", 59_000],
    ["probe-minutes", "Status probe minutes", 59 * 60_000],
    ["probe-hours", "Status probe hours", 23 * 3600_000],
    ["probe-yesterday", "Status probe yesterday", 25 * 3600_000],
    ["probe-six-days", "Status probe six days", 6 * DAY],
    ["probe-same-year", "Status probe same year", 20 * DAY],
    ["probe-other-year", "Status probe other year", 370 * DAY],
  ];
  return seeds.map(([id, name, launchedMsAgo]) => ({
    id, name, notes: "", folder: "", fp: FP[0].id, proxy: null,
    createdDaysAgo: 1, launchedDaysAgo: null, launchedMsAgo, runtimeHours: 0,
  }));
}

/** `window.__shardxMockProxies = N`, set before load, adds N plain proxies
 *  ("Proxy 0001" …) to try the list at scale. */
function extraProxies(): ProxyEntry[] {
  const n = Number((window as { __shardxMockProxies?: number }).__shardxMockProxies ?? 0);
  return Array.from({ length: n }, (_, k) => {
    const i = k + 1;
    return {
      id: `px-bulk-${i}`, name: `Proxy ${String(i).padStart(4, "0")}`, kind: "socks5" as const,
      host: `10.${Math.floor(i / 250)}.${i % 250}.1`, port: 1080, username: "", password: "", country: "", notes: "",
    };
  });
}

/** `window.__shardxMockTrash = N`, set before load, adds N plain Trash
 *  entries ("Trashed 0001" …) to try the page at scale. */
function extraTrash(): TrashEntry[] {
  const n = Number((window as { __shardxMockTrash?: number }).__shardxMockTrash ?? 0);
  return Array.from({ length: n }, (_, k) => ({
    id: `trash-bulk-${k + 1}`, name: `Trashed ${String(k + 1).padStart(4, "0")}`, folder: "",
    deleted_at: unix(1), expires_at: unix(-6), size_bytes: 20_000_000,
  }));
}

/** `window.__shardxMockEmpty = true`, set before load: a fresh install, with
 *  no profiles, proxies, trash, bookmarks or extensions (the library stays). */
const emptyInstall = () => !!(window as { __shardxMockEmpty?: boolean }).__shardxMockEmpty;

export function seed(): MockState {
  const library = FP.map(fingerprint);
  // `window.__shardxMockNoFingerprints = true`, set before load: an empty
  // fingerprint library (the empty state of the Fingerprints page). Profiles
  // still seed from the whole set, so they keep their stored fingerprints.
  const noLibrary = !!(window as { __shardxMockNoFingerprints?: boolean }).__shardxMockNoFingerprints;
  const fingerprints = noLibrary ? [] : library;
  const empty = emptyInstall();
  const profiles: Record<string, StoredProfile> = {};
  const order: string[] = [];
  const running: MockState["running"] = {};
  let pid = 48210;
  for (const s of empty ? [] : [...PROFILES, ...extraProfiles(), ...extraFolderProfiles(), ...statusProbes()]) {
    profiles[s.id] = stored(s, library);
    order.push(s.id);
    if (s.running) running[s.id] = { pid: pid++, started: Date.now() - (s.id === "a78a0eea" ? 2 * 3600_000 + 14 * 60_000 : 27 * 60_000) };
  }
  const snapshots: Record<string, ProxyTestSnapshot[]> = {};
  for (const p of empty ? [] : PROXIES) {
    if (p.id === "px-dead") snapshots[p.id] = [snapshot(p, 14 * 24 * 60, true), snapshot(p, 35, false)];
    else if (p.id === "px-new") continue;
    else snapshots[p.id] = [snapshot(p, 3 * 24 * 60), snapshot(p, 26 * 60), snapshot(p, 12)];
  }
  return {
    version: SEED_VERSION,
    profiles,
    order,
    proxies: empty ? [] : [...PROXIES.map((p) => ({ ...p })), ...extraProxies()],
    snapshots,
    fingerprints,
    extensions: empty ? [] : EXTENSIONS.map((e) => ({ ...e })),
    bookmarks: empty ? [] : BOOKMARKS.map((b) => ({ ...b })),
    trash: empty ? [] : [...TRASH.map((t) => ({ ...t })), ...extraTrash()],
    settings: {
      browser_path: null,
      theme: "dark",
      geo_checker: "ip-api.com",
      screen_resolution_mode: "fingerprint",
      helper_enabled: true,
      helper_triggers: [],
      camera_enabled: true,
      minimize_to_tray: false,
      extra_args: "",
      ext_update_check: true,
      api_enabled: true,
      api_port: 40325,
      api_secret: "mock-signing-key-0000000000000000",
    },
    api: { enabled: true, port: 40325, base_url: "http://127.0.0.1:40325", token: "shx_mock_00000000000000000000000000" },
    psKey: "psk_test_mock_000000000000000000000000",
    psOrders: PS_ORDERS.map((o) => ({ ...o })),
    automation: AUTOMATION,
    modules: MODULES,
    running,
    nextPid: pid,
  };
}
