// One handler per Tauri command the frontend invokes. Handlers mutate the
// mock state in place and return what the Rust command would return; a throw
// rejects the invoke the way a Rust `Err` does (the app renders `String(e)`).
import type { MockFolders, MockState, MockTags, StoredProfile } from "./fixtures";
import {
  seed, metaOf, PS_ME, PS_PRODUCTS, PS_AVAILABLE, PS_ACTIVE, PS_TRAFFIC, psUsage,
  PS_COUNTRIES, PS_REGIONS, PS_CITIES, PS_ISPS,
} from "./fixtures";
import { newId, sleep } from "./store";
import { joinTeamServer, teamHandlers } from "./teamServer";
import { teamCreateRefusal, teamStartRefusal } from "./teamRights";
import { syncHandlers, syncLease, syncTouched } from "./syncServer";
import { alreadyInLibrary, extProfileClosed, extUpdateHandlers, importSame } from "./extUpdates";
import { teamNew, teamSave } from "./teamProjects";
import { openSessions, resendUnsent, sessionEnded } from "./teamSessions";
import { intoTeamTrash, teamTrashHandlers } from "./teamTrash";
import { launchCheck, lockHandlers, refuseLocked, saveStub, stubOf } from "./locks";
import { LOCK_REFUSALS } from "../../shared/api/lockRefusals";
import { AUTOMATION_REFUSALS } from "../../shared/api/automationRefusals";
import { NOT_IN_TEAM, profileFromFingerprint, settle, spaceStateOf, teamInUse, teamUnreachable, told } from "./spaces";
import { SYNC_REFUSALS } from "../../entities/sync/model/types";
import { TEAM_REFUSALS } from "../../shared/api/teamRefusals";
import type { ProxyEntry, ProxyTestSnapshot } from "../../entities/proxy/model/types";
import type { FingerprintEntry } from "../../entities/fingerprint/model/types";
import type { Project, RunState, ModuleInfo } from "../../entities/automation/model/types";

export type Ctx = {
  state: MockState;
  save: () => void;
  emit: (event: string, payload?: unknown) => void;
};

type Args = Record<string, any>;
export type Handler = (args: Args, ctx: Ctx) => unknown;

/** Launcher-side timestamps: "@<unix seconds>", the format profile.rs writes. */
const nowStamp = () => `@${Math.floor(Date.now() / 1000)}`;

// The library's vocabulary, as the backend's host_platform answers.
function hostPlatform(): string {
  const ua = navigator.userAgent;
  if (/Windows/i.test(ua)) return "Windows";
  if (/Macintosh|Mac OS X/i.test(ua)) return "macOS";
  return "Linux";
}

function profile(state: MockState, id: string): StoredProfile {
  const p = state.profiles[id];
  if (!p) throw `profile ${id} not found`;
  return p;
}

function changed(ctx: Ctx, topic = "profiles") {
  ctx.save();
  // As the backend (lib.rs notify_store_changed): the topic is the payload.
  ctx.emit("store-changed", topic);
}

/** What `space_state` answers (spaces.ts). */
const spaceState = spaceStateOf;

/** A write only the personal space takes (space/mod.rs `here`). */
function personalOnly(state: MockState) {
  if (teamInUse(state)) throw NOT_IN_TEAM;
}

/** The server of the team in use, when one is. */
const teamServerInUse = (state: MockState) => {
  const team = teamInUse(state);
  return team ? state.teamServers?.[team] : undefined;
};

/** As tags.rs: the saved tags, made on the first write. */
const tagsOf = (state: MockState): MockTags => (state.tags ??= { tags: [], by_profile: {} });
/** As folders.rs: the saved folders, made on the first write. */
const foldersOf = (state: MockState): MockFolders => (state.folders ??= { registry: [], meta: {} });
const cleanTag = (name: unknown) => String(name ?? "").split(/\s+/).filter(Boolean).join(" ").slice(0, 32);

/** As tags.rs `assign`: each listed profile gets exactly these known tags, once each. */
function assignTags(s: MockTags, byProfile: Record<string, string[]>) {
  const known = new Set(s.tags.map((t) => t.id));
  for (const [pid, ids] of Object.entries(byProfile)) {
    const kept = [...new Set(ids.filter((id) => known.has(id)))];
    if (kept.length) s.by_profile[pid] = kept; else delete s.by_profile[pid];
  }
}

/** As folders.rs `rename`: a kept entry keeps its place, the look follows; a folder never kept stays unkept. */
function renameFolder(s: MockFolders, old: string, next: string) {
  const to = next.trim();
  if (!to) { s.registry = s.registry.filter((f) => f !== old); delete s.meta[old]; return; }
  s.registry = [...new Set(s.registry.map((f) => (f === old ? to : f)))];
  if (Object.prototype.hasOwnProperty.call(s.meta, old)) { s.meta[to] = s.meta[old]; delete s.meta[old]; }
}

function stopProcess(state: MockState, id: string): boolean {
  const run = state.running[id];
  if (!run) return false;
  const p = state.profiles[id];
  if (p) p._meta.total_runtime_ms += Date.now() - run.started;
  delete state.running[id];
  return true;
}

/** As launch.rs refuses a profile whose bound proxy is gone: the reason, or null. */
function proxyGone(state: MockState, id: string): string | null {
  const bound = profile(state, id)._meta.proxy_id;
  if (!bound || state.proxies.some((p) => p.id === bound)) return null;
  return `profile ${id} is bound to a proxy that no longer exists (${bound}) — bind another proxy, or none, then start it again`;
}

/** Profiles with a start under way (launch.rs STARTING): a second start is refused BUSY. */
const starting = new Set<string>();

function startProcess(state: MockState, id: string, cdp = false): number {
  const p = profile(state, id);
  if (state.running[id]) return state.running[id].pid;
  const pid = state.nextPid++;
  state.running[id] = { pid, started: Date.now(), ...(cdp ? { cdp } : {}) };
  p._meta.last_launched_at = nowStamp();
  if (p._meta.launches !== undefined) p._meta.launches += 1;
  return pid;
}

function cloneProfile(src: StoredProfile, name: string): StoredProfile {
  const copy = JSON.parse(JSON.stringify(src)) as StoredProfile;
  copy._meta = { ...copy._meta, id: newId(), rev: 1, created_at: nowStamp(), last_launched_at: null, total_runtime_ms: 0, pinned: false };
  copy.name = name;
  return copy;
}

function testSnapshot(entry: ProxyEntry, previous?: ProxyTestSnapshot): ProxyTestSnapshot {
  const dead = !entry.host || entry.name.toLowerCase().includes("expired");
  const isShard = entry.kind === "socks5" && entry.host.includes("proxyshard");
  const base = 30 + Math.round(Math.random() * 120);
  return {
    // The backend stamps "@<unix seconds>", as it does everywhere.
    first_seen: previous?.first_seen ?? nowStamp(),
    last_seen: nowStamp(),
    ip: dead ? "" : previous?.ip || `${45 + (base % 100)}.${base}.${(base * 3) % 255}.${(base * 7) % 250 + 1}`,
    country_code: dead ? "" : entry.country || "US",
    country: dead ? "" : previous?.country || entry.country || "United States",
    region: dead ? "" : previous?.region || "",
    city: dead ? "" : previous?.city || "",
    isp: dead ? "" : previous?.isp || "Unknown ISP",
    timezone: dead ? "" : previous?.timezone || "UTC",
    latitude: dead ? 0 : previous?.latitude ?? 0,
    longitude: dead ? 0 : previous?.longitude ?? 0,
    tcp_ms: dead ? null : base,
    udp_ms: dead ? null : isShard ? base + 14 : null,
    udp_error: dead ? null : isShard ? null : "SOCKS5 UDP ASSOCIATE refused",
    geo_error: dead ? "connect timed out after 8000 ms" : null,
    provider: "ip-api.com",
  };
}

/** `host:port`, `host:port:user:pass`, `user:pass@host:port`, `socks5://user:pass@host:port`. */
function parseProxyLines(text: string, kind: ProxyEntry["kind"]): ProxyEntry[] {
  const out: ProxyEntry[] = [];
  for (const raw of text.split(/\r?\n/)) {
    const line = raw.trim();
    if (!line || line.startsWith("#")) continue;
    let k = kind;
    let rest = line;
    const scheme = /^(socks5|https?):\/\//i.exec(rest);
    if (scheme) { k = scheme[1].toLowerCase() as ProxyEntry["kind"]; rest = rest.slice(scheme[0].length); }
    let host = "", port = 0, username = "", password = "";
    if (rest.includes("@")) {
      const [auth, hp] = rest.split("@");
      [username, password = ""] = auth.split(":");
      [host, port] = [hp.split(":")[0], Number(hp.split(":")[1])];
    } else {
      const parts = rest.split(":");
      host = parts[0]; port = Number(parts[1]); username = parts[2] ?? ""; password = parts[3] ?? "";
    }
    if (!host || !Number.isFinite(port)) continue;
    // An id from the parse, as the backend's (proxy.rs `parse_one`): the save keeps it.
    out.push({ id: `px-${newId().slice(0, 6)}`, name: `${host}:${port}`, kind: k, host, port, username, password, country: "", notes: "" });
  }
  return out;
}

/// As the backend's `bulk_save` (proxy.rs `add_new`): each entry keeps its
/// id, and one identical to a stored proxy or to an earlier entry (same
/// kind, host, port, user and password) is skipped. Another password is
/// another proxy. Returns how many were added.
function addProxies(entries: ProxyEntry[], ctx: Ctx): number {
  let n = 0;
  for (const e of entries) {
    const same = ctx.state.proxies.some((p) => p.kind === e.kind && p.host === e.host && p.port === e.port && p.username === e.username && p.password === e.password);
    if (same) continue;
    handlers.proxy_save({ entry: { ...e, id: e.id && !ctx.state.proxies.some((p) => p.id === e.id) ? e.id : "" } }, ctx);
    n++;
  }
  return n;
}

/** What each module was allowed to call, kept for the session. */
const moduleGrants: Record<string, { modules: string[]; flows: string[] }> = {};
const runs: Record<string, RunState> = {};
const runTimers: Record<string, ReturnType<typeof setInterval>> = {};

/** Steps that write a profile on this machine: a team's run refuses them (runner.rs, space/mod.rs PERSONAL_ONLY). */
const WRITES_PROFILE = new Set(["profile.keep", "proxy.set", "proxy.generate", "proxy.residential"]);

function startRun(state: MockState, project: Project, ctx: Ctx): RunState {
  const ids = state.order.slice(0, Math.max(1, project.run.threads));
  // A team's profile starts for a run only with Start and Cookies on it (doors.rs RUN).
  // A protected one never runs in automation (#127).
  const refused = new Map(ids.map((id) => [id, teamStartRefusal(state, id, true) ?? (state.profiles[id]?._meta.lock ? LOCK_REFUSALS.notAvailable : null)] as const));
  const at = () => new Date().toLocaleTimeString();
  const run: RunState = {
    project_id: project.id,
    project_name: project.name,
    started_at: Math.floor(Date.now() / 1000),
    running: true,
    workers: ids.map((id) => ({
      profile_id: id, profile_name: state.profiles[id]?.name ?? id, pass: 1, step: 0,
      steps_total: project.blocks.length,
      ...(refused.get(id) ? { status: "failed" as const, note: refused.get(id)! } : { status: "starting" as const, note: "opening the browser" }),
    })),
    log: [`${at()} run started, ${ids.length} worker(s)`, ...ids.filter((id) => refused.get(id)).map((id) => `${at()} ${state.profiles[id]?.name ?? id}: ${refused.get(id)}`)],
  };
  runs[project.id] = run;
  for (const id of ids) if (!refused.get(id)) startProcess(state, id, true);
  runTimers[project.id] = setInterval(() => {
    let live = false;
    for (const w of run.workers) {
      if (w.status === "done" || w.status === "failed" || w.status === "stopped") continue;
      live = true;
      w.status = "running";
      w.step += 1;
      w.note = project.blocks[w.step - 1]?.label ?? "";
      if (teamInUse(state) && WRITES_PROFILE.has(project.blocks[w.step - 1]?.kind ?? "")) {
        w.status = "failed";
        w.note = TEAM_REFUSALS.personalOnly;
        run.log.push(`${new Date().toLocaleTimeString()} ${w.profile_name}: ${w.note}`);
        stopProcess(state, w.profile_id);
        continue;
      }
      run.log.push(`${new Date().toLocaleTimeString()} ${w.profile_name}: step ${w.step}/${w.steps_total} ${w.note}`);
      if (w.step >= w.steps_total) {
        if (project.run.loops === 0 || w.pass < project.run.loops) { w.pass += 1; w.step = 0; }
        else { w.status = "done"; w.note = "finished"; stopProcess(state, w.profile_id); }
      }
    }
    if (!live) { run.running = false; clearInterval(runTimers[project.id]); delete runTimers[project.id]; }
    ctx.save();
  }, 2500);
  return run;
}

/** A run's worker drives this profile now (cdp.rs `run_drives`). */
const runDrives = (id: string) =>
  Object.values(runs).some((r) => r.running && r.workers.some((w) => w.profile_id === id && (w.status === "starting" || w.status === "running")));

function stopRun(state: MockState, id: string): void {
  const run = runs[id];
  if (!run) return;
  run.running = false;
  for (const w of run.workers) { if (w.status !== "done") { w.status = "stopped"; w.note = "stopped by operator"; } stopProcess(state, w.profile_id); }
  run.log.push(`${new Date().toLocaleTimeString()} run stopped`);
  if (runTimers[id]) { clearInterval(runTimers[id]); delete runTimers[id]; }
}

/** Set `window.__shardxMockEngine` to see the engine screen: "offline" fails
 *  with a network error; "missing" downloads the engine and the fingerprint
 *  library, as on a fresh install; "update" has an engine and offers a newer
 *  one. Read on every call, so a test can switch it between tries.
 *  `window.__shardxMockEngineStepMs` slows each progress step (150 ms). */
const engineMode = () => (window as { __shardxMockEngine?: string }).__shardxMockEngine;
const engineOffline = () => engineMode() === "offline";
let engineDownloaded = false;
const engineInstalled = () => engineDownloaded || (engineMode() !== "offline" && engineMode() !== "missing");
const engineStepMs = () => Number((window as { __shardxMockEngineStepMs?: number }).__shardxMockEngineStepMs ?? 150);

/** An install that stops short (#122), as runtime.rs `engine_failed` reports it.
 *  `__shardxMockEngineFail`: each install stops at that stage (read on every call);
 *  `__shardxMockEngineBusy`: the browsers a busy stop counts (else the running profiles);
 *  `__shardxMockEngineError`: the stop an earlier start recorded (install-error.json);
 *  `__shardxMockEngineStaged`: an earlier try left the update unpacked;
 *  `__shardxMockEngineRemote`: the etag the update offers;
 *  `__shardxMockEngineManifestLost`: the status after a stop could not read the manifest (it drops the stop);
 *  `__shardxMockEngineManifestDown`: an install cannot read the manifest at all, so tries nothing;
 *  `__shardxMockEngineBusyKind`: "starting" (a profile starts) or "untracked" (one left from before). */
type EngineStage = "download" | "space" | "extract" | "verify" | "busy" | "swap";
type EngineError = { stage: EngineStage; message: string; running_browsers: number; starting: number; engine_kept: boolean; target: string | null; at_ms: number };
type EngineKnobs = {
  __shardxMockEngineFail?: EngineStage; __shardxMockEngineBusy?: number; __shardxMockEngineError?: EngineError; __shardxMockEngineStaged?: boolean;
  __shardxMockEngineRemote?: string; __shardxMockEngineManifestLost?: boolean; __shardxMockEngineManifestDown?: boolean;
  __shardxMockEngineBusyKind?: "starting" | "untracked";
};
const engineKnobs = () => window as EngineKnobs;
let engineError: EngineError | null | undefined;
let engineStaged: boolean | undefined;
const lastEngineError = () => (engineError === undefined ? (engineError = engineKnobs().__shardxMockEngineError ?? null) : engineError);
const engineIsStaged = () => (engineStaged === undefined ? (engineStaged = !!engineKnobs().__shardxMockEngineStaged) : engineStaged);
const remoteEtag = () => (engineMode() === "update" ? engineKnobs().__shardxMockEngineRemote ?? "a91c3e07" : "e2f22d25");
/** swap.rs `busy`, by the browsers it found. */
const busyMessage = (n: number) => {
  const kind = engineKnobs().__shardxMockEngineBusyKind;
  if (kind === "starting") return "The engine update is ready, but a profile is starting. Wait for it, close the profiles that are running, and try again.";
  const said = n === 1 ? "The engine update is ready, but 1 profile is still running on the current engine. Close it and try again."
    : n > 1 ? `The engine update is ready, but ${n} profiles are still running on the current engine. Close them and try again.`
      : "The engine update is ready, but 1 browser process is still running from the engine folder. Close it (or restart the computer) and try again.";
  return kind === "untracked" && n > 0
    ? `${said} If ShardBrowser shows none running, one is left from an earlier session: end it in Task Manager (Activity Monitor on a Mac) or restart the computer.`
    : said;
};
const ENGINE_STOPS: Record<Exclude<EngineStage, "busy">, string> = {
  space: "Not enough free disk space to update the ShardBrowser engine: it needs about 1460 MB, and 312 MB is free on the disk with /Users/you/Library/Application Support/com.proxyshard.shardx/runtime. Free up space and try again; the engine you have was not touched.",
  download: "Downloading the engine failed (error decoding response body). The engine you have was not touched.",
  extract: "Unpacking the engine update failed (invalid zip header). The engine you have was not touched.",
  verify: "The unpacked engine update is missing ShardX.app/Contents/MacOS/ShardX; an antivirus may have removed it. Allow the engine folder and try again. The engine you have was not touched.",
  swap: "The engine folder is in use and could not be replaced (Access is denied. (os error 5)). Close every browser window that ShardBrowser opened, and any program that has that folder open, then try again. The update is kept.",
};

const DATA_ROOT = "/Users/you/Library/Application Support/com.proxyshard.shardx";

/** Whether two stored profiles have the same content, `_meta` aside (key order too). */
function sameConfig(a: StoredProfile, b: StoredProfile): boolean {
  const norm = (v: unknown): unknown =>
    Array.isArray(v) ? v.map(norm)
      : v && typeof v === "object"
        ? Object.fromEntries(Object.keys(v).sort().map((k) => [k, norm((v as Record<string, unknown>)[k])]))
        : v;
  const body = (p: StoredProfile) => { const { _meta: _, ...rest } = p; return JSON.stringify(norm(rest)); };
  return body(a) === body(b);
}

/** The price of an order in cents, as user-api calculates it. The promo code
 *  "INVALID" is one it does not know (the way the app shows such an error). */
function psPrice(product: string, quantity: number, promoCode: unknown) {
  const promo = typeof promoCode === "string" ? promoCode.trim() : "";
  if (/^invalid$/i.test(promo)) throw "400 Bad Request: promo code not found";
  const unit = /premium/i.test(product) ? 1400 : /unlimited/i.test(product) ? 9900 : /standart/i.test(product) ? 700 : /isp/i.test(product) ? 390 : /mobile/i.test(product) ? 4500 : 180;
  const original = unit * Math.max(1, quantity);
  const discount = promo ? 15 : quantity >= 10 ? 10 : 0;
  const final = Math.round(original * (1 - discount / 100));
  return { original_price: original, final_price: final, discount_percent: discount, addons_price: 0, total_with_addons: final };
}

/** What Delete all data would move, as app_reset_plan answers. The stamp
 *  is UTC, so a test's frozen clock gives the same path everywhere. */
function resetPlan(state: MockState) {
  const counts = {
    profiles: Object.keys(state.profiles).length, proxies: state.proxies.length,
    fingerprints: state.fingerprints.filter((f) => !f.builtin).length, extensions: state.extensions.length,
    bookmarks: state.bookmarks.length, projects: state.automation.length,
  };
  const stamp = new Date().toISOString().slice(0, 19).replace("T", "-").replace(/:/g, "");
  return {
    id: `${stamp}.${Object.values(counts).join("-")}`,
    backups: [`${DATA_ROOT.replace(/\/[^/]+$/, "")}/shardx-launcher-backup-${stamp}`],
    counts,
    total_bytes: counts.profiles * 48 * 1024 * 1024 + counts.extensions * 1_200_000,
    entries: [`${DATA_ROOT}/profiles`, `${DATA_ROOT}/settings.json`],
  };
}

/** The state as a reset leaves it: the user's data gone to the backups, the
 *  bundled fingerprints and the defaults left. */
function emptyAfterReset(s: MockState, backups: string[]) {
  const fresh = seed();
  Object.assign(s, {
    profiles: {}, order: [], proxies: [], snapshots: {}, extensions: [], bookmarks: [], trash: [],
    trashed: {}, trashedAt: {}, fingerprints: s.fingerprints.filter((f) => f.builtin),
    settings: fresh.settings, api: fresh.api, psKey: "", psOrders: [], automation: [], modules: [], running: {},
    backup: backups[0],
  });
  delete s.psBalance;
  // tags.json and folders.json go to the backup with the settings.
  delete s.tags;
  delete s.folders;
}

export const handlers: Record<string, Handler> = {
  // ---- runtime, host, launcher ---------------------------------------------
  runtime_status: () => {
    const update_available = engineMode() === "update" && !engineDownloaded;
    return {
      installed: engineInstalled(), binary_path: `${DATA_ROOT}/engine/ShardX.app`, installed_browser_etag: engineDownloaded ? remoteEtag() : "e2f22d25", remote_browser_etag: remoteEtag(),
      update_available, spec: { browser: { key: "ShardX-Mac-arm64.zip", label: "macOS (Apple Silicon)" } },
      fingerprints_installed: engineInstalled(), needs_newer_launcher: false, min_launcher_version: "2.0.1", launcher_version: "2.0.3",
      install_error: update_available || !engineInstalled() ? lastEngineError() : null,
      update_staged: update_available && engineIsStaged(),
    };
  },
  runtime_install: async (_a, ctx) => {
    const step = (phase: "download" | "extract", received: number, percent: number) =>
      ctx.emit("runtime:progress", { label: "ShardX-Mac-arm64.zip", phase, received, total: 2e7, percent });
    // With an engine to keep the window opens on it; with none it is an error.
    const stop = (stage: EngineStage, message: string, browsers = 0, starting = 0) => {
      const kept = engineInstalled();
      engineError = { stage, message, running_browsers: browsers, starting, engine_kept: kept, target: remoteEtag(), at_ms: Date.now() };
      ctx.emit("runtime:failed", { ...engineError, update_staged: engineIsStaged() });
      if (!kept) throw message;
      const status = handlers.runtime_status({}, ctx) as Record<string, unknown>;
      // The manifest unread: no update offered, so the stop is filtered out (runtime.rs).
      return engineKnobs().__shardxMockEngineManifestLost
        ? { ...status, remote_browser_etag: null, update_available: false, install_error: null, update_staged: false }
        : status;
    };
    // The manifest unread: nothing is tried, yet runtime:done comes (runtime.rs).
    if (engineKnobs().__shardxMockEngineManifestDown) {
      ctx.emit("runtime:done");
      return { ...(handlers.runtime_status({}, ctx) as Record<string, unknown>), remote_browser_etag: null, update_available: false, install_error: null, update_staged: false };
    }
    if (engineOffline()) {
      step("download", 0, 0);
      return stop("download", "Downloading the engine failed (error sending request for url (https://cdn.shardx.io/ShardX-Mac-arm64.zip)). The engine you have was not touched.");
    }
    const fail = engineKnobs().__shardxMockEngineFail;
    if (engineIsStaged()) step("extract", 0, 100);
    else {
      if (fail === "space") return stop("space", ENGINE_STOPS.space);
      for (let i = 1; i <= 4; i++) {
        await sleep(engineStepMs());
        step(i < 4 ? "download" : "extract", i * 5e6, i * 25);
        if (fail === "download" && i === 2) return stop("download", ENGINE_STOPS.download);
      }
      if (fail === "extract" || fail === "verify") return stop(fail, ENGINE_STOPS[fail]);
      engineStaged = true;
    }
    if (fail === "busy") {
      // Refused only while something runs (or starts) from the engine, as swap.rs is.
      const knob = engineKnobs().__shardxMockEngineBusy;
      const n = knob ?? Object.keys(ctx.state.running).length;
      const starting = engineKnobs().__shardxMockEngineBusyKind === "starting";
      if (knob !== undefined || n > 0 || starting) return stop("busy", busyMessage(n), starting ? 0 : n, starting ? 1 : 0);
    }
    if (fail === "swap") return stop("swap", ENGINE_STOPS.swap);
    engineDownloaded = true;
    engineStaged = false;
    engineError = null;
    ctx.emit("runtime:done");
    return handlers.runtime_status({}, ctx);
  },
  launcher_update_check: () => ({ current: "2.0.3", latest: "2.0.3", update_available: false, release_url: "https://github.com/ProxyShard/ShardBrowser/releases/latest" }),
  host_platform: () => hostPlatform(),
  // `window.__shardxMockHostScreen = [w, h, dpr]` (or null: no monitor) stands in for the real one.
  host_screen: () => {
    const w = window as { __shardxMockHostScreen?: [number, number, number] | null };
    return w.__shardxMockHostScreen !== undefined ? w.__shardxMockHostScreen : [window.screen.width, window.screen.height, window.devicePixelRatio];
  },
  read_text_file: () => "",
  // The window's answer to a quit request (B48); recorded in __shardxMock.calls.
  app_quit_asking: () => null,
  app_quit_confirmed: () => null,
  app_quit_cancelled: () => null,
  clipboard_write: ({ text }) => {
    // Recorded for tests: headless browsers refuse the real clipboard.
    (window as { __shardxMockClipboard?: string }).__shardxMockClipboard = String(text ?? "");
    void navigator.clipboard?.writeText(String(text ?? "")).catch(() => {});
    return null;
  },
  clipboard_read: async () => {
    // What a test (or an earlier clipboard_write) put there: headless browsers refuse the real clipboard.
    const held = (window as { __shardxMockClipboard?: string }).__shardxMockClipboard;
    if (held !== undefined) return held;
    try { return await navigator.clipboard.readText(); } catch { return ""; }
  },

  // ---- settings, api, data root ---------------------------------------------
  settings_get: (_a, { state }) => state.settings,
  settings_save: ({ value }, ctx) => {
    // Recorded for tests: what the settings page sent.
    ((window as { __shardxMockSettingsSaves?: unknown[] }).__shardxMockSettingsSaves ??= []).push(value);
    // As the backend (lib.rs:1804-1808): an incoming signing key wins; a
    // missing or empty one keeps the key on disk.
    const keep = ctx.state.settings.api_secret;
    ctx.state.settings = { ...ctx.state.settings, ...value, api_secret: value.api_secret || keep };
    ctx.state.api = { ...ctx.state.api, enabled: !!value.api_enabled, port: value.api_port ?? ctx.state.api.port, base_url: `http://127.0.0.1:${value.api_port ?? ctx.state.api.port}` };
    ctx.save();
    return null;
  },
  // A test sets `__shardxMockSettingsLoadError` to show the load-error card.
  settings_load_error: () => (window as { __shardxMockSettingsLoadError?: string }).__shardxMockSettingsLoadError ?? null,
  api_info: (_a, { state }) => state.api,
  api_regenerate_token: (_a, ctx) => {
    // As the backend: a new signing key on disk, and a token signed with it.
    ctx.state.settings = { ...ctx.state.settings, api_secret: `mock-signing-key-${newId()}${newId()}` };
    ctx.state.api = { ...ctx.state.api, token: `shx_${newId()}${newId()}${newId()}${newId()}` };
    ctx.save();
    return ctx.state.api;
  },
  mcp_download: ({ dir }) => `${dir}/shardx-mcp`,
  data_root_get: () => ({ path: DATA_ROOT, custom: false, migrating: false }),
  // Settings > Storage > Delete all data (reset.rs). The plan counts what
  // would move; its id changes with any count, as the backend's digest does.
  // The reset moves everything aside (here: empties the state and remembers
  // the backups), keeping the engine and the bundled fingerprints. The app
  // then waits for app_restart; here the page reloads, onto an empty install.
  app_reset_plan: (_a, { state }) => resetPlan(state),
  app_reset: async ({ planId }, ctx) => {
    await sleep(300);
    const shown = resetPlan(ctx.state);
    if (planId !== shown.id) throw "Your data changed. Check again.";
    for (const id of Object.keys(runs)) stopRun(ctx.state, id);
    emptyAfterReset(ctx.state, shown.backups);
    ctx.save();
    return { backups: shown.backups };
  },
  app_reset_backup: (_a, { state }) => state.backup ?? null,
  app_reset_keep_storage: () => null,
  // A reset that did not finish: a test sets `state.resetPending` and reloads.
  app_reset_pending: (_a, { state }) => state.resetPending ?? null,
  app_reset_undo: async (_a, ctx) => { await sleep(200); delete ctx.state.resetPending; ctx.save(); return null; },
  // The journal renamed aside: the app unlocks, nothing else changes.
  app_reset_set_aside: (_a, ctx) => { delete ctx.state.resetPending; ctx.save(); return null; },
  app_reset_finish: async (_a, ctx) => {
    await sleep(200);
    const backups = ctx.state.resetPending?.backups ?? [];
    emptyAfterReset(ctx.state, backups);
    delete ctx.state.resetPending;
    ctx.save();
    return { backups };
  },
  // The window's keys were just cleared, this mock's own state among them:
  // written again, so the reload finds the emptied install.
  app_restart: (_a, ctx) => { ctx.save(); return null; },
  data_root_migrate: async ({ path }, ctx) => {
    const phases = ["scan", "copy", "copy", "copy", "verify", "cleanup", "done"] as const;
    let i = 0;
    for (const phase of phases) { await sleep(300); i++; ctx.emit("data-migration", { phase, done: i * 120, total: 840, percent: Math.round((i / phases.length) * 100), current: `${path}/profiles/${i}` }); }
    return 840;
  },

  // ---- profiles ---------------------------------------------------------------
  profile_list: (_a, { state }) => state.order.map((id) => metaOf(state.profiles[id])),
  // A protected profile's answer is a stub: its name and the open part of `_meta` (#127).
  profile_get: ({ id }, { state }) => { const p = profile(state, id); return p._meta.lock ? stubOf(p) : p; },
  profile_save: ({ payload, password }, ctx) => {
    // `window.__shardxMockSaveFails = true` makes saves fail as a full disk would.
    if ((window as { __shardxMockSaveFails?: boolean }).__shardxMockSaveFails) throw "No space left on device (os error 28)";
    const { state } = ctx;
    const incoming = payload as StoredProfile;
    const id: string = incoming?._meta?.id || newId();
    const prev = state.profiles[id];
    // As the backend (lib.rs `enrich_new_config`, `randomize_hardware`): a
    // profile the UI creates gets new cores and memory, whatever it sent.
    const nav = (incoming as { navigator?: { hardware_concurrency?: number; device_memory?: number } }).navigator;
    if (!incoming?._meta?.id && nav) {
      nav.hardware_concurrency = nav.hardware_concurrency === 4 ? 8 : 4;
      nav.device_memory = nav.device_memory === 8 ? 16 : 8;
    }
    // As the backend: a save that carries the rev it was opened at is checked
    // against the stored one (0 for a profile that is gone).
    const sent = incoming?._meta?.id ? incoming._meta.rev : undefined;
    const onDisk = prev?._meta.rev ?? 0;
    if (typeof sent === "number" && sent !== onDisk) {
      throw `This profile changed after you opened it (rev ${onDisk}, you have ${sent}) — probably through the API or another window. Reopen it and apply your changes to the current version.`;
    }
    // A protected one: a rename or recolor by stub, or the whole of it with its password.
    if (prev) {
      const row = saveStub(prev, incoming as Record<string, any>, password);
      if (row) { ctx.save(); return row; }
    }
    const meta = {
      ...(prev?._meta ?? { pinned: false, folder: "", total_runtime_ms: 0, created_at: nowStamp(), last_launched_at: null }),
      ...incoming._meta,
      id,
      // Only a change to the profile's own content moves the counter.
      rev: !prev ? 1 : sameConfig(prev, incoming) ? prev._meta.rev : prev._meta.rev + 1,
      last_launched_at: prev?._meta.last_launched_at ?? null,
      created_at: prev?._meta.created_at ?? nowStamp(),
      total_runtime_ms: prev?._meta.total_runtime_ms ?? 0,
      pinned: prev?._meta.pinned ?? false,
      // A new one takes the settings' own folder (write.rs create_profile); a save never moves one.
      folder: prev?._meta.folder ?? incoming?._meta?.folder ?? "",
    };
    state.profiles[id] = { ...incoming, _meta: meta };
    if (!prev) state.order.push(id);
    // The backend signals "store-changed" only for API writes; the UI reloads
    // after its own saves as it needs to.
    ctx.save();
    if (prev) syncTouched(state, [id], { [id]: prev });
    return metaOf(state.profiles[id]);
  },
  profile_delete: ({ id }, ctx) => {
    const { state } = ctx;
    const p = profile(state, id);
    stopProcess(state, id);
    if (teamInUse(state)) {
      // Into the team's trash on its server: its folder cannot go while it waits there.
      intoTeamTrash(ctx, "profile", id, p.name, p, { folder: p._meta.folder, revision: p._meta.rev });
      delete state.profiles[id];
      state.order = state.order.filter((x) => x !== id);
      ctx.save();
      return null;
    }
    (state.trashed ??= {})[id] = structuredClone(p);
    // As the backend (trash.rs RETENTION_DAYS): seven days.
    state.trash.unshift({ id, name: p.name, folder: p._meta.folder, deleted_at: Math.floor(Date.now() / 1000), expires_at: Math.floor(Date.now() / 1000) + 7 * 86400, size_bytes: 40_000_000 + Math.round(Math.random() * 150_000_000) });
    delete state.profiles[id];
    (state.trashedAt ??= {})[id] = state.order.indexOf(id);
    state.order = state.order.filter((x) => x !== id);
    // As the backend: store-changed is for API writes only.
    ctx.save();
    return null;
  },
  profile_clone: ({ id }, ctx) => {
    refuseLocked(ctx.state, id);
    const src = profile(ctx.state, id);
    const copy = cloneProfile(src, `${src.name} (copy)`);
    ctx.state.profiles[copy._meta.id] = copy;
    ctx.state.order.splice(ctx.state.order.indexOf(id) + 1, 0, copy._meta.id);
    // As the backend: store-changed is for API writes only.
    ctx.save();
    return metaOf(copy);
  },
  profile_set_pin: ({ id, pinned }, ctx) => { profile(ctx.state, id)._meta.pinned = !!pinned; ctx.save(); return null; },
  profile_set_folder: ({ id, folder }, ctx) => { profile(ctx.state, id)._meta.folder = folder ?? ""; ctx.save(); return null; },
  profile_bind_proxy: ({ profileId, proxyId }, ctx) => { refuseLocked(ctx.state, profileId); profile(ctx.state, profileId)._meta.proxy_id = proxyId ?? null; ctx.save(); return null; },
  profile_import: ({ payloads }, ctx) => {
    let n = 0;
    for (const p of payloads as StoredProfile[]) {
      // A sealed file is not taken in (#127).
      if (p?._meta?.lock) { ctx.save(); throw LOCK_REFUSALS.notAvailable; }
      // As lib.rs in a team: each is written in turn, and the first refused stops the rest.
      const refused = teamCreateRefusal(ctx.state, String(p?._meta?.folder ?? ""));
      if (refused) { ctx.save(); throw refused; }
      const id = newId();
      ctx.state.profiles[id] = { ...p, _meta: { ...(p._meta ?? {}), id, rev: 1, created_at: nowStamp(), last_launched_at: null, total_runtime_ms: 0, pinned: false, folder: p._meta?.folder ?? "", proxy_id: p._meta?.proxy_id ?? null, gpu_preset_id: p._meta?.gpu_preset_id ?? "", extensions: p._meta?.extensions ?? [] } };
      ctx.state.order.push(id); n++;
    }
    // As the backend: store-changed is for API writes only.
    ctx.save();
    return n;
  },
  profile_create_from_template: ({ templateId }, ctx) => {
    const fp = ctx.state.fingerprints.find((f) => f.id === templateId);
    if (!fp) throw `template ${templateId} not in library`;
    const n = ctx.state.order.length + 1;
    const p = profileFromFingerprint(fp, `Profile#${n}`);
    ctx.state.profiles[p._meta.id] = p;
    ctx.state.order.push(p._meta.id);
    // As the backend: store-changed is for API writes only.
    ctx.save();
    return metaOf(p);
  },
  process_list: (_a, { state }) => Object.entries(state.running).filter(([id]) => state.profiles[id]).map(([profile_id, r]) => ({ profile_id, pid: r.pid, uptime_ms: Date.now() - r.started })),
  // A synced profile's session goes up as its browser closes.
  process_kill: ({ profileId }, ctx) => {
    const id = String(profileId);
    // Only a team or synced one has a session to send (open.rs open_now); read while it runs.
    const sends = openSessions(ctx.state).some((s) => s.profile_id === id);
    const ok = stopProcess(ctx.state, profileId); ctx.save();
    // A team profile's session goes back to its team as its browser closes;
    // `window.__shardxMockSessionSaveMs` keeps it on its way that long (listed, a start refused).
    const saveMs = (window as { __shardxMockSessionSaveMs?: number }).__shardxMockSessionSaveMs;
    if (ok) syncTouched(ctx.state, [id]);
    if (ok && saveMs && sends) {
      (ctx.state.saving ??= {})[id] = true;
      setTimeout(() => { sessionEnded(ctx.state, id, "saved", ctx.emit); ctx.save(); }, saveMs);
    } else if (ok) sessionEnded(ctx.state, id, "saved", ctx.emit);
    // An extension update that waited for this browser goes in.
    extProfileClosed(ctx, String(profileId));
    return ok;
  },
  // The quit dialog's list: team and synced profiles open here (lib.rs team_sessions).
  team_sessions: (_a, { state }) => openSessions(state),
  launch: async ({ profileId, password }, ctx) => {
    // A team's start takes its lease from the server first (open.rs open_team).
    const offline = teamUnreachable(ctx.state);
    if (offline) throw offline;
    // A start under way (launch.rs Starting), or a closed team profile whose session is still sent (open.rs claim).
    if (starting.has(String(profileId)) || ctx.state.saving?.[String(profileId)]) throw TEAM_REFUSALS.busy;
    const resendMs = (window as { __shardxMockSessionSaveMs?: number }).__shardxMockSessionSaveMs ?? 1500;
    if (resendUnsent(ctx.state, String(profileId), ctx.emit, ctx.save, resendMs)) { ctx.save(); throw TEAM_REFUSALS.busy; }
    // A protected one opens with its password first (#127).
    await launchCheck(ctx.state, profileId, password);
    const gone = proxyGone(ctx.state, String(profileId));
    if (gone) throw gone;
    // As the backend: another device holds a synced profile's lease.
    const lease = syncLease(ctx.state, String(profileId));
    if (lease) throw SYNC_REFUSALS.leased(lease);
    // What the engine starts with is read now, as the backend does: recorded for tests.
    const w = window as { __shardxMockLaunches?: { id: string; proxy_id: string | null }[]; __shardxMockStartPayloads?: { id: string; extensions: string[]; bookmarks: string[] }[] };
    const meta = profile(ctx.state, String(profileId))._meta;
    (w.__shardxMockLaunches ??= []).push({ id: String(profileId), proxy_id: meta.proxy_id });
    // Its extensions and the team's bookmarks come with the start (items.rs, ?profile=), whatever the member's lists show.
    (w.__shardxMockStartPayloads ??= []).push({ id: String(profileId), extensions: [...meta.extensions], bookmarks: ctx.state.bookmarks.map((b) => b.id) });
    // `launch_cancel` (below, B49) ends the launch with a "cancelled" error, at any moment of it.
    const cancels = (window as { __shardxMockLaunchCancelled?: string[] }).__shardxMockLaunchCancelled ??= [];
    const cancelled = () => {
      const at = cancels.indexOf(String(profileId));
      if (at < 0) return false;
      cancels.splice(at, 1);
      return true;
    };
    if (cancelled()) throw "Launch cancelled";
    starting.add(String(profileId));
    try {
      for (let waited = 0; waited < 900; waited += 50) {
        await sleep(50);
        if (cancelled()) throw "Launch cancelled";
      }
      // `window.__shardxMockLaunchFails = [ids]`, set before load, makes those launches fail.
      if (((window as { __shardxMockLaunchFails?: string[] }).__shardxMockLaunchFails ?? []).includes(String(profileId))) {
        throw "Engine could not start: port in use";
      }
      const pid = startProcess(ctx.state, profileId); ctx.save(); return pid;
    } finally {
      starting.delete(String(profileId));
    }
  },
  // Backend B49. `window.__shardxMockLaunchCancel` picks how the backend answers: unset, it has no
  // such command (Tauri rejects an unknown one, as the real backend does today); "now", it ends the
  // pending launch; "late", it answers only once the launch has gone through (too late to end it).
  launch_cancel: async ({ profileId }, ctx) => {
    const mode = (window as { __shardxMockLaunchCancel?: string }).__shardxMockLaunchCancel;
    if (!mode) throw "Command launch_cancel not found";
    if (mode === "late") {
      while (!ctx.state.running[String(profileId)]) await sleep(20);
      return null;
    }
    ((window as { __shardxMockLaunchCancelled?: string[] }).__shardxMockLaunchCancelled ??= []).push(String(profileId));
    return null;
  },
  sync_launch: async ({ profileIds, group }, ctx) => {
    await sleep(700);
    // As lib.rs: a profile that cannot start is left out; only all of them failing is an error.
    const failed: string[] = [];
    for (const id of profileIds) {
      const gone = proxyGone(ctx.state, String(id));
      const lease = syncLease(ctx.state, String(id));
      // A team's profile starts only with Start on it (launch_profile_synced, open.rs passes).
      const right = teamUnreachable(ctx.state) ?? teamStartRefusal(ctx.state, String(id), false)
        ?? (ctx.state.profiles[String(id)]?._meta.lock ? LOCK_REFUSALS.notAvailable : null);
      if (right) failed.push(`${id}: ${right}`);
      else if (gone) failed.push(`${id}: ${gone}`);
      else if (lease) failed.push(`${id}: ${SYNC_REFUSALS.leased(lease)}`);
      else startProcess(ctx.state, id);
    }
    ctx.save();
    if (failed.length && failed.length === profileIds.length) throw `nothing launched — ${failed.join("; ")}`;
    return group || `group-${newId().slice(0, 4)}`;
  },
  sync_status: ({ group }, { state }) => ({ group, members: Object.keys(state.running).map((profile, i) => ({ profile, excluded: false, driving: i === 0 })), paused: false }),
  sync_set_paused: () => null,
  sync_arrange: () => null,
  sync_stop: (_a, ctx) => { for (const id of Object.keys(ctx.state.running)) stopProcess(ctx.state, id); ctx.save(); return null; },
  sync_set_excluded: () => null,
  sync_close_panel: () => null,
  helper_profiles: () => [],
  helper_fields: () => null,
  helper_fill: () => 0,
  helper_show: () => null,
  helper_close: () => null,
  helper_dismiss: () => null,
  folder_rename: ({ old, new: next }, ctx) => {
    let n = 0;
    for (const id of ctx.state.order) {
      const p = ctx.state.profiles[id];
      if (p._meta.folder === old) { p._meta.folder = String(next); n++; }
    }
    // As space::profile::rename_folder: the kept entry and the look follow.
    // A folder never kept makes no file.
    if (ctx.state.folders) renameFolder(ctx.state.folders, String(old), String(next));
    ctx.save();
    return n;
  },
  folder_delete: ({ folder, deleteProfiles }, ctx) => {
    const ids = ctx.state.order.filter((id) => ctx.state.profiles[id]._meta.folder === folder);
    for (const id of ids) { if (deleteProfiles) handlers.profile_delete({ id }, ctx); else ctx.state.profiles[id]._meta.folder = ""; }
    const server = teamServerInUse(ctx.state);
    if (server) {
      // As write.rs delete_folder: the server keeps it while profiles deleted from it wait in
      // the team's trash; only a delete that took its profiles along lets that pass.
      if ((server.trash ?? []).some((p) => p.folder === folder)) {
        ctx.save();
        if (!(deleteProfiles && ids.length)) throw TEAM_REFUSALS.folderInTrash;
        return ids.length;
      }
      server.folders = server.folders.filter((x) => x.name !== folder);
    }
    // As folders.rs: a folder never kept makes no file.
    const f = ctx.state.folders;
    if (f) { f.registry = f.registry.filter((x) => x !== folder); delete f.meta[String(folder)]; }
    // As the backend: store-changed is for API writes only.
    ctx.save();
    return ids.length;
  },
  // ---- Tags and folders (tags.rs, folders.rs) ------------------------------------
  tag_list: (_a, { state }) => state.tags ?? { tags: [], by_profile: {} },
  tag_save: ({ tag, at }, ctx) => {
    const s = tagsOf(ctx.state);
    const name = cleanTag(tag?.name);
    if (!name) throw "a tag needs a name";
    if (!String(tag?.color ?? "").trim()) throw "a tag needs a color";
    if (s.tags.some((t) => t.id !== tag.id && t.name.toLowerCase() === name.toLowerCase())) throw `a tag named ${name} already exists`;
    const next = { id: String(tag.id || `t-${newId()}`), name, color: String(tag.color).trim() };
    const i = s.tags.findIndex((t) => t.id === next.id);
    if (i >= 0) s.tags[i] = next;
    else s.tags.splice(Math.min(typeof at === "number" ? at : s.tags.length, s.tags.length), 0, next);
    ctx.save();
    return next;
  },
  tag_delete: ({ id }, ctx) => {
    // As tags.rs: a change that changes nothing makes no file.
    if (!ctx.state.tags) return null;
    const s = ctx.state.tags;
    const gone = s.tags.find((t) => t.id === id);
    if (gone && teamInUse(ctx.state)) intoTeamTrash(ctx, "tag", gone.id, gone.name, gone);
    s.tags = s.tags.filter((t) => t.id !== id);
    for (const [pid, ids] of Object.entries(s.by_profile)) {
      const kept = ids.filter((x) => x !== id);
      if (kept.length) s.by_profile[pid] = kept; else delete s.by_profile[pid];
    }
    ctx.save();
    return null;
  },
  tag_assign: ({ byProfile }, ctx) => { if (ctx.state.tags) { assignTags(ctx.state.tags, byProfile ?? {}); ctx.save(); } return null; },
  // Taken only while nothing was ever saved: a second import changes nothing.
  tag_import: ({ state: given }, ctx) => {
    personalOnly(ctx.state);
    if (ctx.state.tags) return false;
    const seen = new Set<string>();
    const tags = ((given?.tags ?? []) as MockTags["tags"]).filter((t) => t.id && t.name && t.color && !seen.has(t.id) && !!seen.add(t.id));
    const s: MockTags = { tags, by_profile: {} };
    assignTags(s, given?.by_profile ?? {});
    ctx.state.tags = s;
    ctx.save();
    return true;
  },
  folder_list: (_a, { state }) => {
    const server = teamServerInUse(state);
    if (!server) return state.folders ?? { registry: [], meta: {} };
    return {
      registry: server.folders.map((f) => f.name),
      meta: Object.fromEntries(server.folders.map((f) => [f.name, { color: f.color || null, icon: "folder" }])),
    };
  },
  folder_add: ({ name }, ctx) => {
    if (!String(name ?? "").trim()) throw "a folder needs a name";
    const server = teamServerInUse(ctx.state);
    if (server && !server.folders.some((x) => x.name === name)) server.folders.push({ name, color: "" });
    const f = foldersOf(ctx.state);
    if (!f.registry.includes(name)) f.registry.push(name);
    ctx.save();
    return null;
  },
  folder_meta_set: ({ name, meta }, ctx) => {
    if (!String(name ?? "").trim()) throw "a folder needs a name";
    foldersOf(ctx.state).meta[name] = { color: meta?.color ?? null, icon: meta?.icon ?? "folder" };
    const server = teamServerInUse(ctx.state);
    if (server) {
      const f = server.folders.find((x) => x.name === name);
      if (f) f.color = meta?.color ?? ""; else server.folders.push({ name, color: meta?.color ?? "" });
    }
    ctx.save();
    return null;
  },
  folder_import: ({ state: given }, ctx) => {
    personalOnly(ctx.state);
    if (ctx.state.folders) return false;
    const registry = [...new Set(((given?.registry ?? []) as string[]).filter((f) => typeof f === "string" && f.trim()))];
    const meta = Object.fromEntries(Object.entries((given?.meta ?? {}) as MockFolders["meta"]).filter(([n]) => n.trim()));
    ctx.state.folders = { registry, meta };
    ctx.save();
    return true;
  },
  cookies_export_to_file: ({ profileId }, { state }) => { refuseLocked(state, profileId); return 42; },
  cookies_import: ({ profileId, cookies }, { state }) => { refuseLocked(state, profileId); return Array.isArray(cookies) ? cookies.length : 0; },
  enrich_picks_for_preset: ({ presetId }, { state }) => {
    const nav = state.fingerprints.find((f) => f.id === presetId)?.payload?.navigator ?? {};
    return { hardware_concurrency: nav.hardware_concurrency, device_memory: nav.device_memory, platform_version: nav.platform_version };
  },

  // ---- proxies ------------------------------------------------------------------
  proxy_list: (_a, { state }) => state.proxies,
  proxy_save: ({ entry }, ctx) => {
    const e = { ...(entry as ProxyEntry) };
    if (!e.id) { e.id = `px-${newId().slice(0, 6)}`; ctx.state.proxies.push(e); }
    else { const i = ctx.state.proxies.findIndex((p) => p.id === e.id); if (i >= 0) ctx.state.proxies[i] = e; else ctx.state.proxies.push(e); }
    // As the backend: "store-changed" is for API writes only.
    ctx.save();
    return e;
  },
  proxy_delete: ({ id }, ctx) => {
    const gone = ctx.state.proxies.find((p) => p.id === id);
    if (gone && teamInUse(ctx.state)) intoTeamTrash(ctx, "proxy", gone.id, gone.name || `${gone.host}:${gone.port}`, gone, { proxy_kind: gone.kind, country: gone.country });
    ctx.state.proxies = ctx.state.proxies.filter((p) => p.id !== id);
    delete ctx.state.snapshots[id];
    // As the backend (proxy.rs:116-126): profiles keep the id of a proxy that
    // is gone. The UI must show that and refuse to start them (A4, backend B1).
    ctx.save();
    return null;
  },
  proxy_full_test: async ({ entry }, ctx) => {
    // `window.__shardxMockTestMs = ms` sets how long a test takes (tests of many results).
    const testMs = (window as { __shardxMockTestMs?: number }).__shardxMockTestMs;
    await sleep(testMs ?? 600 + Math.random() * 900);
    const e = entry as ProxyEntry;
    // What each test ran against, for tests.
    const w = window as { __shardxMockTests?: { id: string; host: string }[] };
    (w.__shardxMockTests ??= []).push({ id: e.id, host: e.host });
    const hist = ctx.state.snapshots[e.id] ?? [];
    const snap = testSnapshot(e, hist[hist.length - 1]);
    if (e.id) { ctx.state.snapshots[e.id] = [...hist, snap].slice(-20); ctx.save(); }
    // As the backend: a test fills in an empty country on the stored proxy.
    const stored = ctx.state.proxies.find((p) => p.id === e.id);
    if (stored && snap.country_code && (!stored.country || stored.country === "—")) { stored.country = snap.country_code; ctx.save(); }
    if (snap.geo_error) throw snap.geo_error;
    return snap;
  },
  proxy_last_test: ({ id }, { state }) => { const h = state.snapshots[id]; return h?.length ? h[h.length - 1] : null; },
  proxy_history: ({ id }, { state }) => state.snapshots[id] ?? [],
  proxy_bulk_parse: ({ text, kind }) => parseProxyLines(String(text ?? ""), kind),
  proxy_bulk_save: ({ entries }, ctx) => addProxies(entries as ProxyEntry[], ctx),
  proxy_bulk_import: ({ text, kind }, ctx) => addProxies(parseProxyLines(String(text ?? ""), kind), ctx),

  // ---- fingerprints -----------------------------------------------------------
  // The library ships with the engine: empty until the first-run download.
  fingerprint_list: (_a, { state }) => (engineInstalled() ? state.fingerprints : []),
  fingerprint_delete: ({ id }, ctx) => { ctx.state.fingerprints = ctx.state.fingerprints.filter((f) => f.id !== id); ctx.save(); return null; },
  fingerprint_import: ({ jsonText, idHint }, ctx) => {
    let payload: any;
    try { payload = JSON.parse(String(jsonText)); } catch { throw "not valid JSON"; }
    const ua = String(payload?.navigator?.user_agent ?? "");
    const platform = String(payload?.navigator?.platform ?? (/Windows/.test(ua) ? "Win32" : /Mac/.test(ua) ? "MacIntel" : "Linux x86_64"));
    const entry: FingerprintEntry = {
      id: idHint || `fp-${newId().slice(0, 6)}`,
      label: payload?.name || idHint || `Imported ${new Date().toLocaleDateString()}`,
      platform,
      chrome: (/Chrome\/([\d.]+)/.exec(ua)?.[1]) ?? "152.0.0.0",
      gpu: String(payload?.webgl?.renderer ?? "unknown GPU"),
      tag_color: "#64748b",
      builtin: false,
      payload,
    };
    ctx.state.fingerprints.push(entry);
    ctx.save();
    return entry;
  },
  fingerprint_dir: () => `${DATA_ROOT}/fingerprints`,
  gpu_caps: () => ({ renderer: "ANGLE (Apple, Apple M3 Pro, OpenGL 4.1)", vendor: "Google Inc. (Apple)", webgl1: ["ANGLE_instanced_arrays", "EXT_color_buffer_half_float", "OES_texture_float"], webgl2: ["EXT_color_buffer_float", "OES_texture_float_linear"], engine_version: "152.0.7977.65" }),
  gpu_caps_compat: (_a, { state }) => {
    const out: Record<string, unknown> = {};
    for (const f of state.fingerprints) {
      const incompatible = f.id === "win11-arc-a770" || f.id === "linux-nvidia-1660";
      out[f.id] = { compatible: !incompatible, missing_webgl1: incompatible ? ["WEBGL_compressed_texture_s3tc_srgb"] : [], missing_webgl2: incompatible ? ["EXT_texture_compression_bptc"] : [], missing_webgpu: incompatible ? ["texture-compression-bc"] : [], profile_renderer: f.gpu };
    }
    return out;
  },

  // ---- extensions, bookmarks, trash -------------------------------------------
  extension_list: (_a, { state }) => state.extensions,
  // As lib.rs: each file in turn, and each one refused is named with why (extensions.rs import_each).
  extension_import: ({ paths, separate }, ctx) => {
    const added: MockState["extensions"] = [];
    const refused: { path: string; error: string }[] = [];
    for (const path of paths as string[]) {
      // The same extension again (extUpdates.ts knob): refused unless a separate copy.
      const same = separate ? undefined : importSame(ctx.state);
      if (same) { refused.push({ path, error: alreadyInLibrary(ctx.state, same) }); continue; }
      // A file named "broken…" is no package.
      if (/broken/i.test(path)) { refused.push({ path, error: "not a zip/crx archive: invalid Zip archive: Could not find EOCD" }); continue; }
      added.push({ id: `ext-${newId().slice(0, 6)}`, name: path.split("/").pop()?.replace(/\.(crx|zip)$/, "") || "Extension", version: "1.0.0", description: "Imported from disk.", icon: "", path, size_bytes: 500_000 + Math.round(Math.random() * 5e6), added_at: `@${Math.floor(Date.now() / 1000)}` });
    }
    if (added.length) { ctx.state.extensions.push(...added); changed(ctx, "extensions"); }
    return { added, refused };
  },
  extension_import_url: ({ url, separate }, ctx) => {
    const store = /[a-p]{32}/.exec(String(url))?.[0];
    // One the library holds already (extensions.rs already_in_library).
    const there = store ? ctx.state.extensions.find((x) => x.source?.kind === "webstore" && x.source.id === store) : undefined;
    if (there && !separate) throw alreadyInLibrary(ctx.state, there);
    const id = there ? newId().slice(0, 6) : store?.slice(0, 8) ?? newId();
    const e = { id: `ext-${id}`, name: there?.name ?? `Web Store ${id.slice(0, 4)}`, version: there?.version ?? "3.2.1", description: `Downloaded from ${url}`, icon: "", path: `${DATA_ROOT}/extensions/${id}`, size_bytes: 1_200_000, added_at: `@${Math.floor(Date.now() / 1000)}`, ...(store ? { source: { kind: "webstore" as const, id: store } } : {}) };
    ctx.state.extensions.push(e); changed(ctx, "extensions"); return e;
  },
  extension_delete: ({ id }, ctx) => { const gone = ctx.state.extensions.find((e) => e.id === id); if (gone && teamInUse(ctx.state)) intoTeamTrash(ctx, "extension", gone.id, gone.name, gone); ctx.state.extensions = ctx.state.extensions.filter((e) => e.id !== id); delete ctx.state.extUpdates?.[id]; for (const p of Object.values(ctx.state.profiles)) p._meta.extensions = p._meta.extensions.filter((x) => x !== id); changed(ctx, "extensions"); return null; },
  ...extUpdateHandlers,
  bookmark_list: (_a, { state }) => state.bookmarks,
  // As bookmarks.rs cleaned(): trimmed, the address made whole (a bookmarklet as written), an untitled one named by its address.
  bookmark_save: ({ entry }, ctx) => { const b = { ...entry, title: String(entry.title ?? "").trim(), url: String(entry.url ?? "").trim(), folder: String(entry.folder ?? "").trim() }; if (!b.url) throw "a bookmark needs a URL"; if (!b.url.includes("://") && !/^javascript:/i.test(b.url)) b.url = `https://${b.url}`; if (!b.root || b.root === "bookmark_bar") delete b.root; if (!b.title) b.title = b.url; if (!b.id) { b.id = `bm-${newId().slice(0, 6)}`; ctx.state.bookmarks.push(b); } else { const i = ctx.state.bookmarks.findIndex((x) => x.id === b.id); if (i >= 0) ctx.state.bookmarks[i] = b; else ctx.state.bookmarks.push(b); } ctx.save(); return b; },
  bookmark_delete: ({ id }, ctx) => { const gone = ctx.state.bookmarks.find((b) => b.id === id); if (gone && teamInUse(ctx.state)) intoTeamTrash(ctx, "bookmark", gone.id, gone.title || gone.url, gone); ctx.state.bookmarks = ctx.state.bookmarks.filter((b) => b.id !== id); ctx.save(); return null; },
  bookmark_delete_many: ({ ids }, ctx) => { const gone = new Set((ids as string[]) ?? []); if (teamInUse(ctx.state)) for (const b of ctx.state.bookmarks) if (gone.has(b.id)) intoTeamTrash(ctx, "bookmark", b.id, b.title || b.url, b); ctx.state.bookmarks = ctx.state.bookmarks.filter((b) => !gone.has(b.id)); ctx.save(); return null; },
  // A team keeps no trash here (space/mod.rs trash::list).
  trash_list: (_a, { state }) => (teamInUse(state) ? [] : state.trash),
  trash_restore: ({ id }, ctx) => {
    personalOnly(ctx.state);
    const t = ctx.state.trash.find((x) => x.id === id);
    if (!t) throw `nothing in the trash with id ${id}`;
    // As the backend unpacks the archive: the profile as it went in. Seeded
    // entries have no archive here and get rebuilt from a fingerprint.
    let p = ctx.state.trashed?.[id];
    if (!p) {
      p = profileFromFingerprint(ctx.state.fingerprints[0], t.name, t.folder);
      p._meta.id = id;
    }
    ctx.state.profiles[id] = p;
    // Back where it stood, as the backend lists it (by creation date).
    const at = ctx.state.trashedAt?.[id];
    if (at === undefined || at < 0) ctx.state.order.push(id);
    else ctx.state.order.splice(Math.min(at, ctx.state.order.length), 0, id);
    delete ctx.state.trashedAt?.[id];
    delete ctx.state.trashed?.[id];
    ctx.state.trash = ctx.state.trash.filter((x) => x.id !== id);
    // As the backend: store-changed is for API writes only.
    ctx.save();
    return metaOf(p);
  },
  trash_purge: ({ id }, ctx) => { personalOnly(ctx.state); ctx.state.trash = ctx.state.trash.filter((x) => x.id !== id); delete ctx.state.trashed?.[id]; delete ctx.state.trashedAt?.[id]; ctx.save(); return null; },
  // A team keeps no trash here: emptying it empties nothing (space/mod.rs trash::empty).
  trash_empty: (_a, ctx) => { if (teamInUse(ctx.state)) return 0; const n = ctx.state.trash.length; ctx.state.trash = []; ctx.state.trashed = {}; ctx.state.trashedAt = {}; ctx.save(); return n; },

  // ---- Spaces and teams (team.rs) ---------------------------------------------
  space_state: (_a, { state }) => spaceState(state),
  space_set: ({ space }, ctx) => {
    const sp = space as NonNullable<MockState["space"]>;
    const joined = sp.kind === "team" ? (ctx.state.teams ?? []).find((t) => t.id === sp.team_id) : undefined;
    if (sp.kind === "team" && !joined) throw "not a member of that team";
    // As team.rs set_space: a team that withdrew this launcher stays shut, and
    // one whose token was replaced waits for the new invite.
    if (joined?.revoked) throw `${joined.name} withdrew this launcher's access: join it again`;
    if (joined?.needs_invite) throw `${joined.name}: the server does not know this token any more: paste the new invite from the team's admin`;
    ctx.state.space = sp;
    settle(ctx.state, ctx.emit);
    ctx.save();
    told(ctx.state, ctx.emit);
    return spaceState(ctx.state);
  },
  // A member token is "stm_…", an admin token a JWT ("eyJ…"); anything else
  // is what a server answers to a wrong token.
  team_join: async ({ server, token }, ctx) => {
    await sleep(300);
    const url = String(server ?? "").trim();
    const tok = String(token ?? "").trim();
    // As team.rs server_base and join_refused word it.
    let u: URL | null = null;
    try { u = new URL(url); } catch { /* not an address */ }
    if (!u || !/^https?:$/.test(u.protocol) || !u.host) throw "not a server address: give it as http://host:port";
    const host = u.host;
    const admin = tok.startsWith("eyJ");
    if (!admin && !tok.startsWith("stm_")) throw "the server does not know this token";
    const id = `team-${host}`;
    const teams = (ctx.state.teams ?? []).filter((t) => t.id !== id);
    const name = ctx.state.teamServers?.[id]?.name ?? host.split(".")[0].replace(/^./, (c) => c.toUpperCase()) + " team";
    // The server answers who the token is: the admin, or one of its members.
    joinTeamServer(ctx.state, id, name, tok, admin);
    ctx.state.teams = [...teams, { id, name, server: url, admin }];
    // Joined to work there: the launcher moves into it at once.
    ctx.state.space = { kind: "team", team_id: id };
    settle(ctx.state, ctx.emit);
    ctx.save();
    told(ctx.state, ctx.emit);
    return spaceState(ctx.state);
  },
  team_leave: ({ teamId }, ctx) => {
    // As lib.rs: its profiles open here go back to it and close first (close_team_and_wait).
    for (const s of openSessions(ctx.state)) {
      if (s.mine || s.team_id !== teamId) continue;
      stopProcess(ctx.state, s.profile_id);
      sessionEnded(ctx.state, s.profile_id, "saved", ctx.emit);
    }
    ctx.state.teams = (ctx.state.teams ?? []).filter((t) => t.id !== teamId);
    if (ctx.state.space?.kind === "team" && ctx.state.space.team_id === teamId) {
      ctx.state.space = { kind: "personal" };
      settle(ctx.state, ctx.emit);
      told(ctx.state, ctx.emit);
    } else {
      // As lib.rs team_leave: the windows hear of it either way.
      ctx.emit("space-changed", spaceStateOf(ctx.state).space);
    }
    // What was read of it is forgotten with it.
    if (ctx.state.spaceData) delete ctx.state.spaceData[String(teamId)];
    ctx.save();
    return spaceState(ctx.state);
  },

  // ---- Team admin and a member's own access (teamServer.ts) -------------------
  ...teamHandlers,
  ...teamTrashHandlers,

  // ---- Personal sync (syncServer.ts) -------------------------------------------
  ...syncHandlers,

  // ---- A profile's own password (locks.ts) ------------------------------------
  ...lockHandlers,

  // ---- ProxyShard account -----------------------------------------------------
  ps_get_key: (_a, { state }) => state.psKey,
  // No trial in the mock: the offer is the server's call, and inventing one
  // here would put a modal on every screenshot of the Proxies page.
  // What the site API answers about a trial. A test sets
  // `window.__shardxMockTrialOffer` before the page asks; unset, no offer is
  // going, which is what a developer should see.
  ps_trial_offer: async () => (window as { __shardxMockTrialOffer?: unknown }).__shardxMockTrialOffer ?? { show: false },
  ps_set_key: ({ key }, ctx) => { ctx.state.psKey = String(key ?? ""); ctx.save(); return null; },
  ps_me: async (_a, { state }) => { await sleep(400); if (!state.psKey) throw "401 Unauthorized: API key missing"; if (!state.psKey.startsWith("psk_")) throw "401 Unauthorized: key rejected by user-api.proxyshard.com"; return { ...PS_ME, wallet_balance: state.psBalance ?? PS_ME.wallet_balance, active_orders: state.psOrders.filter((o) => o.expires_at && Date.parse(o.expires_at) > Date.now()).length }; },
  ps_orders: ({ status = "all", offset = 0, limit = 20 }, { state }) => {
    const active = (o: { expires_at: string | null }) => !!o.expires_at && Date.parse(o.expires_at) > Date.now();
    const all = state.psOrders.filter((o) => status === "all" ? true : status === "active" ? active(o) : status === "cancelled" ? !active(o) : false);
    const page = all.slice(offset, offset + limit);
    return { orders: page, next: offset + limit < all.length ? `/orders?offset=${offset + limit}` : null, total: all.length };
  },
  ps_order: ({ id }, { state }) => { const o = state.psOrders.find((x) => x.order_id === id); if (!o) throw `order ${id} not found`; return { order: { ...o, proxies_count: (PS_ACTIVE[id] ?? []).length, slots: { avail: 5, used: (PS_ACTIVE[id] ?? []).length } } }; },
  ps_renew: async ({ id }, ctx) => { await sleep(500); const o = ctx.state.psOrders.find((x) => x.order_id === id); if (!o) throw `order ${id} not found`; const d = new Date(Math.max(Date.now(), o.expires_at ? Date.parse(o.expires_at) : Date.now())); d.setMonth(d.getMonth() + 1); o.expires_at = d.toISOString(); ctx.save(); return null; },
  ps_products: () => ({ products: PS_PRODUCTS }),
  ps_calculate: ({ product, quantity = 1, promoCode }) => psPrice(String(product), quantity, promoCode),
  ps_purchase: async ({ body }, ctx) => {
    await sleep(900);
    const id = 42000 + ctx.state.psOrders.length;
    const d = new Date(); d.setMonth(d.getMonth() + 1);
    // As the account: the order is charged to the wallet.
    const price = psPrice(String(body?.product ?? "Datacenter USA"), Number(body?.quantity ?? 1), body?.promo_code);
    ctx.state.psBalance = (ctx.state.psBalance ?? PS_ME.wallet_balance) - price.final_price;
    ctx.state.psOrders.unshift({ order_id: id, product_name: String(body?.product ?? "Datacenter USA"), cycle_name: String(body?.cycle ?? "monthly"), expires_at: d.toISOString(), auto_renewal: false, tag: null });
    ctx.save();
    return { ok: true, order_id: id };
  },
  ps_available_count: () => PS_AVAILABLE,
  ps_active: ({ orderId }, { state }) => ({ data: PS_ACTIVE[orderId] ?? [], order_tag: state.psOrders.find((o) => o.order_id === orderId)?.tag ?? "none" }),
  ps_signature_set: ({ orderId, items }) => { for (const it of items as { ip: string; signature: string }[]) { const row = (PS_ACTIVE[orderId] ?? []).find((r) => r.ip === it.ip); if (row) row.signature = it.signature || null; } return null; },
  ps_set_tag: ({ id, tag }, ctx) => { const o = ctx.state.psOrders.find((x) => x.order_id === id); if (o) o.tag = tag; ctx.save(); return null; },
  ps_add_bandwidth: async () => { await sleep(500); return null; },
  // `window.__shardxMockTraffic = { premium: { data, data_remain, data_spent } }`,
  // set before load, replaces a tier's traffic (a plan running low).
  ps_profile_traffic: ({ proxyType }) =>
    (window as { __shardxMockTraffic?: Record<string, unknown> }).__shardxMockTraffic?.[String(proxyType)]
    ?? PS_TRAFFIC[String(proxyType)] ?? PS_TRAFFIC.premium,
  // Backend B40. `window.__shardxMockUsage = null`, set before load, is an
  // account with no history (every card shows the No data ghost).
  ps_usage: async ({ range }) => {
    await sleep(250);
    const o = (window as { __shardxMockUsage?: unknown }).__shardxMockUsage;
    if (o === null) throw "ps_usage: no history";
    return o ?? psUsage(String(range));
  },
  ps_countries: () => ({ results: PS_COUNTRIES }),
  ps_regions: ({ countryCode }) => ({ results: PS_REGIONS[String(countryCode).toUpperCase()] ?? [] }),
  ps_cities: ({ regionCode }) => ({ results: PS_CITIES[String(regionCode)] ?? [] }),
  ps_resi_isps: () => ({ results: PS_ISPS }),

  // ---- automation ---------------------------------------------------------------
  automation_available: () => true,
  automation_list: (_a, { state }) => state.automation,
  automation_create: ({ name }, ctx) => {
    const p: Project = { id: `proj-${newId().slice(0, 6)}`, name: String(name || "Untitled"), notes: "", blocks: [], run: { threads: 1, loops: 1, hours: 0, profiles: [], start: "" }, rules: [], created_at: Math.floor(Date.now() / 1000), updated_at: Math.floor(Date.now() / 1000) };
    ctx.state.automation.push(teamNew(ctx.state, p)); ctx.save(); return p;
  },
  automation_save: ({ project }, ctx) => { const team = teamSave(ctx.state, project as Project); if (team) { ctx.save(); return team; } const p = { ...(project as Project), updated_at: Math.floor(Date.now() / 1000) }; const i = ctx.state.automation.findIndex((x) => x.id === p.id); if (i >= 0) ctx.state.automation[i] = p; else ctx.state.automation.push(p); ctx.save(); return p; },
  automation_delete: ({ id }, ctx) => { stopRun(ctx.state, id); const gone = ctx.state.automation.find((p) => p.id === id); if (gone && teamInUse(ctx.state)) intoTeamTrash(ctx, "automation", gone.id, gone.name, gone); ctx.state.automation = ctx.state.automation.filter((p) => p.id !== id); ctx.save(); return null; },
  automation_duplicate: ({ id }, ctx) => { const src = ctx.state.automation.find((p) => p.id === id); if (!src) throw `project ${id} not found`; const copy: Project = JSON.parse(JSON.stringify(src)); copy.id = `proj-${newId().slice(0, 6)}`; copy.name = `${src.name} copy`; copy.created_at = copy.updated_at = Math.floor(Date.now() / 1000); ctx.state.automation.push(teamNew(ctx.state, copy)); ctx.save(); return copy; },
  automation_attach: () => null,
  automation_detach: () => null,
  automation_attached: () => false,
  automation_screencast: () => null,
  automation_call: ({ method }) => { if (method === "Traffic.list") return { rulesJson: "[]" }; if (method === "Traffic.mount") return { ruleSetId: `rs-${newId().slice(0, 4)}` }; if (method === "Script.run") return { result: "null" }; return {}; },
  automation_launch: async ({ profileId, restart }, ctx) => {
    const offline = teamUnreachable(ctx.state);
    if (offline) throw offline;
    refuseLocked(ctx.state, profileId);
    // As lib.rs: running with automation is attached to, unless a run drives it; without it, restarted only when asked.
    const id = String(profileId);
    const on = ctx.state.running[id];
    if (on?.cdp) { if (runDrives(id)) throw AUTOMATION_REFUSALS.drivenByRun; return on.pid; }
    if (on && !restart) throw AUTOMATION_REFUSALS.openWithout;
    const gone = proxyGone(ctx.state, id);
    if (gone) throw gone;
    const lease = syncLease(ctx.state, id);
    if (lease) throw SYNC_REFUSALS.leased(lease);
    if (on) stopProcess(ctx.state, id);
    await sleep(700); const pid = startProcess(ctx.state, id, true); ctx.save(); return pid;
  },
  automation_pick: ({ x, y }) => ({ selector: "button.primary", tag: "button", label: "Continue", x, y, tried: ["#submit (not found)", "text=Continue (2 matches)"] }),
  automation_run: ({ projectId }, ctx) => { const p = ctx.state.automation.find((x) => x.id === projectId); if (!p) throw `project ${projectId} not found`; if (p.blocks.length === 0) throw "the project has no steps"; startRun(ctx.state, p, ctx); ctx.save(); return null; },
  automation_run_stop: ({ projectId }, ctx) => { stopRun(ctx.state, projectId); ctx.save(); return null; },
  automation_run_status: ({ projectId }) => runs[projectId] ?? null,
  automation_fleet: () => Object.values(runs),
  automation_fleet_window: () => null,
  automation_tls_fingerprints: () => ["chrome_152", "chrome_131", "chrome_120", "firefox_128", "safari_17"],
  automation_modules: (_a, { state }) => state.modules,
  automation_module_permissions: ({ id }) => ({ asks: { modules: [], flows: (window as { __shardxMockAsks?: { flows: string[] } }).__shardxMockAsks?.flows ?? ["*"], vars: ["rows", "sheet"], reason: `${id} needs to read the run variables it appends` }, granted: { decided_for: id, call_modules: moduleGrants[id]?.modules ?? [], call_flows: moduleGrants[id]?.flows ?? [] } }),
  automation_module_grant: ({ id, modules, flows }) => { moduleGrants[id] = { modules: modules as string[], flows: flows as string[] }; return null; },
  automation_module_install: ({ path }, ctx) => { const m: ModuleInfo = { id: `mod-${newId().slice(0, 4)}`, name: String(path).split("/").pop()?.replace(/\.wasm$/, "") || "module", path: String(path), error: "", blocks: [{ kind: "custom.step", label: "Custom step", about: "From an installed module.", params: [{ name: "input", label: "Input", kind: "text" }] }] }; ctx.state.modules.push(m); ctx.save(); return m; },
  automation_module_remove: ({ id }, ctx) => { ctx.state.modules = ctx.state.modules.filter((m) => m.id !== id); ctx.save(); return null; },
  automation_modules_dir: () => `${DATA_ROOT}/modules`,
  automation_export: ({ projectId }, { state }) => { const p = state.automation.find((x) => x.id === projectId); if (!p) throw `project ${projectId} not found`; return { format: 1, exported_at: Math.floor(Date.now() / 1000), project: p, modules: [], needs: p.blocks.filter((b) => b.secrets.length).map((b) => ({ block_id: b.id, label: b.label, params: b.secrets })) }; },
  automation_import: ({ bundle }, ctx) => { const p: Project = JSON.parse(JSON.stringify(bundle.project)); p.id = `proj-${newId().slice(0, 6)}`; ctx.state.automation.push(teamNew(ctx.state, p)); ctx.save(); return p; },
  automation_display: () => ({ server: hostPlatform() === "macOS" ? "quartz" : "x11", limited: false, note: "", browser_placement: true, panels: true }),

  // ---- Tauri plugins ------------------------------------------------------------
  "plugin:app|version": () => "2.0.3",
  "plugin:app|name": () => "ShardBrowser Launcher",
  "plugin:app|tauri_version": () => "2.5.0",
  "plugin:dialog|open": ({ options }) => {
    const o = options ?? {};
    // `window.__shardxMockOpenFiles = [paths]`: what a multi-file pick answers.
    const picked = (window as { __shardxMockOpenFiles?: string[] }).__shardxMockOpenFiles;
    if (picked && o.multiple && !o.directory) return picked;
    if (o.directory) return o.multiple ? ["/Users/you/Desktop/ShardX-data"] : "/Users/you/Desktop/ShardX-data";
    const file = o.filters?.[0]?.extensions?.[0] ? `export.${o.filters[0].extensions[0]}` : "profile.json";
    return o.multiple ? [`/Users/you/Downloads/${file}`] : `/Users/you/Downloads/${file}`;
  },
  "plugin:dialog|save": ({ options }) => `/Users/you/Downloads/${options?.defaultPath ?? "export.json"}`,
  "plugin:dialog|message": () => null,
  "plugin:opener|open_url": ({ url }) => { window.open(String(url), "_blank", "noopener"); return null; },
  "plugin:opener|open_path": ({ path }) => { console.info("[mock] open path", path); return null; },
  "plugin:opener|reveal_item_in_dir": ({ paths }) => { console.info("[mock] reveal", paths); return null; },
};
