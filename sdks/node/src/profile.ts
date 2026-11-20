// Profile = a fingerprint JSON + a per-launch working dir. Wraps the
// bundled fingerprint library and lets callers override fields before
// launch.
import { existsSync, mkdirSync, readdirSync, readFileSync } from "node:fs";
import { join } from "node:path";

import { isAndroidRelease } from "./randomize.js";
import { RETIRED_TEMPLATES, type Runtime } from "./runtime.js";

const NOISE_VECTORS = ["canvas", "webgl", "audio", "client_rects", "sensors", "fonts"] as const;
export type NoiseVector = (typeof NOISE_VECTORS)[number];
// vector -> [soft knob, value applied when the vector is enabled]
const NOISE_KNOB: Partial<Record<NoiseVector, [string, number]>> = {
  webgl: ["intensity", 0.0005],
  client_rects: ["max_offset", 1],
};

export class Profile {
  readonly id: string;
  config: Record<string, unknown>;

  constructor(config: Record<string, unknown>, id?: string) {
    this.config = JSON.parse(JSON.stringify(config));   // deep clone
    this.id = id ?? (config["name"] as string | undefined) ?? "anonymous";
  }

  static fromFile(path: string): Profile {
    const cfg = JSON.parse(readFileSync(path, "utf8"));
    const id = path.split(/[\\/]/).pop()!.replace(/\.json$/, "");
    return new Profile(cfg, id);
  }

  /** Shallow merge: object values are merged one level deep, scalars replaced. */
  withOverride(overrides: Record<string, unknown>): Profile {
    const out: Record<string, unknown> = JSON.parse(JSON.stringify(this.config));
    for (const [k, v] of Object.entries(overrides)) {
      if (v && typeof v === "object" && !Array.isArray(v)
          && out[k] && typeof out[k] === "object" && !Array.isArray(out[k])) {
        out[k] = { ...(out[k] as object), ...(v as object) };
      } else {
        out[k] = v;
      }
    }
    return new Profile(out, (overrides["name"] as string | undefined) ?? this.id);
  }

  /** Enable exactly the named noise vectors (soft defaults) and disable the
   *  rest. Declarative — re-calling replaces the selection, so a dropped
   *  vector is turned off. Seeds are derived per-profile at launch.
   *
   *  @example p.setNoise("canvas", "audio")  // only these two on
   *  @example p.setNoise("canvas")           // audio now off again
   */
  setNoise(...vectors: NoiseVector[]): this {
    for (const v of vectors) {
      if (!NOISE_VECTORS.includes(v)) {
        throw new Error(`unknown noise vector: ${v} (valid: ${NOISE_VECTORS.join(", ")})`);
      }
    }
    const on = new Set<string>(vectors);
    let noise = this.config["noise"] as Record<string, Record<string, unknown>> | undefined;
    if (!noise || typeof noise !== "object") {
      noise = {};
      this.config["noise"] = noise;
    }
    for (const v of NOISE_VECTORS) {
      let block = noise[v];
      if (!block || typeof block !== "object") {
        block = {};
        noise[v] = block;
      }
      block["enabled"] = on.has(v);
      if (block["seed"] === undefined) block["seed"] = 0;
      const knob = NOISE_KNOB[v];
      if (on.has(v) && knob && block[knob[0]] === undefined) block[knob[0]] = knob[1];
    }
    return this;
  }

  get platform(): string {
    const nav = this.config["navigator"] as Record<string, unknown> | undefined;
    return (nav?.["platform"] as string | undefined) ?? "";
  }

  /** A device whose primary input is a finger. */
  get claimsMobile(): boolean {
    return configClaimsMobile(this.config);
  }

  /** Desktop Linux. Android is excluded: its platform is Linux too, and a phone has WebGPU. */
  get claimsLinuxDesktop(): boolean {
    if (this.claimsMobile) return false;
    const p = this.platform.toLowerCase();
    return p.startsWith("linux") || p.startsWith("x11");
  }

  get hasWebGPU(): boolean {
    const wgp = this.config["webgpu"] as Record<string, unknown> | null | undefined;
    if (!wgp) return false;
    const limits = wgp["limits"];
    return !!(limits && typeof limits === "object" && Object.keys(limits as object).length > 0);
  }
}

/** Whether a config claims a phone: the engine's three signals, in its order. */
export function configClaimsMobile(config: Record<string, unknown>): boolean {
  const ch = config["client_hints"] as Record<string, unknown> | undefined;
  if (ch?.["mobile"] === true) return true;
  const chPlatform = String(ch?.["platform"] ?? "").toLowerCase();
  if (chPlatform.startsWith("android")) return true;
  const nav = config["navigator"] as Record<string, unknown> | undefined;
  return String(nav?.["user_agent"] ?? "").toLowerCase().includes("android");
}

export class FingerprintLibrary {
  constructor(private readonly runtime: Runtime) {}

  ids(): string[] {
    return readdirSync(this.runtime.fingerprintsDir)
      .filter((f) => f.endsWith(".json"))
      .map((f) => f.replace(/\.json$/, ""))
      .sort();
  }

  *filter(opts: { platform?: string } = {}): Generator<string> {
    for (const id of this.ids()) {
      if (opts.platform) {
        try {
          const p = this.load(id);
          if (!p.platform.toLowerCase().includes(opts.platform.toLowerCase())) continue;
        } catch { continue; }
      }
      yield id;
    }
  }

  load(fingerprintId: string): Profile {
    fingerprintId = RETIRED_TEMPLATES.get(fingerprintId) ?? fingerprintId;
    const path = join(this.runtime.fingerprintsDir, `${fingerprintId}.json`);
    if (!existsSync(path)) {
      const sample = this.ids().slice(0, 10).join(", ");
      throw new Error(`Fingerprint '${fingerprintId}' not found. Available: ${sample}…`);
    }
    return Profile.fromFile(path);
  }
}

/**
 * Normalise a profile config's spoofed Chrome version to `chromiumVersion`
 * (e.g. "152.0.7977.65") so it always matches the running engine — bumps
 * `navigator.user_agent` (Chrome/<major>.0.0.0) and the version fields in
 * `client_hints`: brand_version / brand_full_version / chrome_build /
 * chrome_patch (derived from the version) plus, when supplied, grease_brand /
 * grease_version / grease_full_version (GREASE rotates per release, so it can't
 * be derived — it comes from the manifest). Leaves platform_version,
 * architecture, etc. intact. Mutates `config` in place. SDK equivalent of the
 * launcher's post-update profile migration.
 *
 * `tls` carries the manifest's TLS block. Only the keys it actually contains
 * are overwritten, so a profile's own `shuffle_extensions` (or any field the
 * manifest does not ship) survives, and a profile with no `tls` block is left
 * alone — absent means "use the engine default", not "stale". Without this the
 * profile keeps the previous release's signature_algorithms while the engine
 * advertises the new ones, and the JA4 hash no longer matches the Chrome
 * version the profile claims: 152 added the ML-DSA schemes 0x0904-0x0906, so a
 * 149-era profile is eleven entries short and reads as a mismatch.
 *
 * `androidVersion` is the build this profile's OS is shipped, when it is not
 * `chromiumVersion`: the Chrome for Android build of the same major for a phone
 * (a phone reporting a desktop build claims one no phone has), or the
 * manifest's `desktop_chromium_versions` entry for a desktop profile (158 went
 * out as .37 for macOS and Windows and .36 for Linux).
 */
export function applyEngineVersion(
  config: Record<string, unknown>,
  chromiumVersion: string,
  greaseBrand?: string,
  greaseVersion?: string,
  tls?: Record<string, unknown>,
  androidVersion?: string,
): void {
  let parts = chromiumVersion.split(".");
  if (parts.length !== 4) return;
  const major = parts[0];
  if (androidVersion && androidVersion.split(".").length === 4 && androidVersion.split(".")[0] === major) {
    chromiumVersion = androidVersion;
    parts = chromiumVersion.split(".");
  }
  const build = parseInt(parts[2], 10);
  const patch = parseInt(parts[3], 10);

  const nav = config["navigator"] as Record<string, unknown> | undefined;
  const ua = nav?.["user_agent"];
  if (nav && typeof ua === "string") {
    const idx = ua.indexOf("Chrome/");
    if (idx >= 0) {
      const rest = ua.slice(idx + 7);
      const end = rest.indexOf(" ");
      const tail = end >= 0 ? rest.slice(end) : "";
      nav["user_agent"] = `${ua.slice(0, idx)}Chrome/${major}.0.0.0${tail}`;
    }
  }

  const ch = config["client_hints"] as Record<string, unknown> | undefined;
  if (ch && typeof ch === "object") {
    ch["brand_version"] = major;
    ch["brand_full_version"] = chromiumVersion;
    if (Number.isFinite(build)) ch["chrome_build"] = build;
    if (Number.isFinite(patch)) ch["chrome_patch"] = patch;
    if (greaseBrand) ch["grease_brand"] = greaseBrand;
    if (greaseVersion) {
      ch["grease_version"] = greaseVersion;
      ch["grease_full_version"] = `${greaseVersion}.0.0.0`;
    }
  }

  const cfgTls = config["tls"] as Record<string, unknown> | undefined;
  if (tls && cfgTls && typeof cfgTls === "object") {
    for (const [k, v] of Object.entries(tls)) cfgTls[k] = v;
  }
}

/** The brand Chrome lists in `userAgentData.brands` and `Sec-CH-UA`, Android included. */
const CHROME_BRAND = "Google Chrome";

/**
 * Name the brand on a profile that names none, so the brand list is Chrome's and
 * not only "Chromium" and GREASE; a brand the profile names stays. Launch calls
 * it for phone profiles, whose library templates lacked the field. Mutates
 * `config` in place. SDK equivalent of the launcher's profile migration.
 */
export function applyChromeBrand(config: Record<string, unknown>): void {
  const ch = config["client_hints"] as Record<string, unknown> | undefined;
  if (!ch || typeof ch !== "object") return;
  if (typeof ch["brand"] === "string" && ch["brand"] !== "") return;
  ch["brand"] = CHROME_BRAND;
}

/**
 * Copy into a saved profile what its library template gained after it was
 * frozen: fonts, fonts_local and webgpu.features where the profile has none,
 * and the adapter's subgroup sizes for the same claimed GPU (the template's win). The template is
 * found by `_meta.gpu_preset_id`, else by `name`; a profile with no template
 * is left as it is. Mutates `config` in place. SDK equivalent of the
 * launcher's `adopt_value`.
 */
export function adoptLibraryFields(config: Record<string, unknown>, fingerprintsDir: string): void {
  const meta = config["_meta"] as Record<string, unknown> | undefined;
  const key = [meta?.["gpu_preset_id"], config["name"]].find((k) => typeof k === "string" && k !== "") as string | undefined;
  const successor = key !== undefined ? RETIRED_TEMPLATES.get(key) : undefined;
  const pick = successor ?? key;
  const path = pick && /^[A-Za-z0-9._-]+$/.test(pick) ? join(fingerprintsDir, `${pick}.json`) : null;
  let src: Record<string, unknown> | null = null;
  if (path && existsSync(path)) {
    try { src = JSON.parse(readFileSync(path, "utf8")); } catch { /* no template */ }
  }
  // A phone's fixes need no template; the version takes the template's when there is one.
  adoptPhoneAngleWebgl(config);
  adoptPhonePlatformVersion(config, src, fingerprintsDir);
  adoptDesktopMinCores(config);
  if (!src) return;
  for (const k of ["fonts", "fonts_local"]) {
    if (config[k] === undefined && src[k] !== undefined) config[k] = src[k];
  }
  adoptLaptopBattery(config, src);
  adoptAir15Screen(config, src);
  adoptLinuxGlWebgl(config, src, successor);
  adoptWebglExtensions(config, src);
  const w = config["webgpu"] as Record<string, unknown> | undefined;
  const sw = src["webgpu"] as Record<string, unknown> | undefined;
  if (!w || typeof w !== "object" || !sw || typeof sw !== "object") return;
  if (w["features"] === undefined && sw["features"] !== undefined) w["features"] = sw["features"];
  // The template's subgroup sizes win for the same claimed GPU, over a stored null or a
  // pre-157 capture (D3D12 max was 128 then); a null in the template changes nothing.
  const sameGpu = ["vendor", "architecture"].every((k) => w[k] !== undefined && w[k] === sw[k]);
  for (const k of ["subgroupMinSize", "subgroupMaxSize"]) {
    if (sameGpu && Number.isInteger(sw[k])) w[k] = sw[k];
  }
}

type Obj = Record<string, unknown>;
const isObj = (v: unknown): v is Obj => typeof v === "object" && v !== null && !Array.isArray(v);

/** Plugged in and full: what the library pinned on every desktop template until 18.11, and
 * the one state a machine without a battery reports. */
function isDesktopBattery(b: unknown): boolean {
  return isObj(b) && b["charging"] === true && b["level"] === 1 && b["charging_time"] === 0;
}

/** A laptop profile still carrying that battery takes the template's {"laptop": true}; the
 * engine then picks its own state from the profile's seed. */
function adoptLaptopBattery(config: Obj, src: Obj): void {
  const sb = src["battery"];
  if (isObj(sb) && sb["laptop"] === true && isDesktopBattery(config["battery"])) config["battery"] = { laptop: true };
}

/** MacBook Air 15" templates claimed 1680x1050; they now claim 1710x1112. A profile with the
 * old size moves, its window with it. */
function adoptAir15Screen(config: Obj, src: Obj): void {
  const s = config["screen"], w = config["window"], ss = src["screen"];
  if (!isObj(s) || !isObj(w) || !isObj(ss)) return;
  if (s["width"] !== 1680 || s["height"] !== 1050 || ss["width"] !== 1710 || ss["height"] !== 1112) return;
  const [availH, outerH, innerH] = [s["avail_height"], w["outer_height"], w["inner_height"]];
  if (![availH, outerH, innerH].every(Number.isInteger)) return;
  const newAvail = 1112 - (1050 - (availH as number));
  const newOuter = newAvail - ((availH as number) - (outerH as number));
  Object.assign(s, { width: 1710, height: 1112, avail_width: 1710, avail_height: newAvail });
  Object.assign(w, { outer_width: 1710, inner_width: 1710, outer_height: newOuter,
    inner_height: newOuter - ((outerH as number) - (innerH as number)) });
}

/** Linux templates claimed Vulkan until 20.11, which Chrome on Linux never reports (ANGLE draws
 * through GLX there): such a profile, or one from a retired template, takes the template's whole
 * WebGL block. Notes still holding the old renderer take the template's; a retired template's
 * tablet platform and touch screen take the new one's. An empty list or table stays. */
function adoptLinuxGlWebgl(config: Obj, src: Obj, successor: string | undefined): void {
  const g = config["webgl"], sg = src["webgl"], nav = config["navigator"];
  if (!isObj(g) || !isObj(sg) || !isObj(nav)) return;
  const have = typeof g["renderer"] === "string" ? g["renderer"] : "";
  const want = typeof sg["renderer"] === "string" ? sg["renderer"] : "";
  const ua = typeof nav["user_agent"] === "string" ? nav["user_agent"] : "";
  const glx = want.startsWith("ANGLE (") && want.includes(", OpenGL ") && !want.includes("OpenGL ES");
  if (!ua.includes("X11; Linux") || !glx || have === want || !(successor !== undefined || have.includes(", Vulkan "))) return;
  for (const [k, v] of Object.entries(sg)) {
    const cur = g[k];
    const hostOwn = (Array.isArray(cur) && cur.length === 0) || (isObj(cur) && Object.keys(cur).length === 0);
    if (!hostOwn) g[k] = v;
  }
  if (config["notes"] === have && src["notes"] !== undefined) config["notes"] = src["notes"];
  if (successor === undefined) return;
  const snav = src["navigator"];
  if (isObj(snav)) for (const k of ["platform_value", "max_touch_points"]) if (snav[k] !== undefined) nav[k] = snav[k];
  const meta = config["_meta"];
  if (isObj(meta) && "gpu_preset_id" in meta) meta["gpu_preset_id"] = successor;
}

/** WebGL extensions and getParameter answers the template gained for the same GPU (Chrome 158
 * added WEBGL_shader_pixel_local_storage and its two limits): added, never removed or changed.
 * An empty list or table means "the host's own" and stays. */
function adoptWebglExtensions(config: Obj, src: Obj): void {
  const g = config["webgl"], sg = src["webgl"];
  if (!isObj(g) || !isObj(sg) || !g["renderer"] || g["renderer"] !== sg["renderer"]) return;
  for (const k of ["extensions", "extensions_v2"]) {
    const have = g[k], want = sg[k];
    if (!Array.isArray(have) || have.length === 0 || !Array.isArray(want)) continue;
    for (const e of want) if (typeof e === "string" && !have.includes(e)) have.push(e);
  }
  const have = g["parameters"], want = sg["parameters"];
  if (isObj(have) && Object.keys(have).length > 0 && isObj(want)) {
    for (const [k, v] of Object.entries(want)) if (!(k in have)) have[k] = v;
  }
}

/** A desktop profile claiming 1-3 logical cores claims 4: a page can time workers on the real
 * machine, and anti.bot flags a claim under 8 whose workers scale past twice it. */
function adoptDesktopMinCores(config: Obj): void {
  const nav = config["navigator"];
  if (configClaimsMobile(config) || !isObj(nav)) return;
  const n = nav["hardware_concurrency"];
  if (Number.isInteger(n) && (n as number) >= 1 && (n as number) <= 3) nav["hardware_concurrency"] = 4;
}

/** GPU makers whose bare driver strings the Android templates carried before 19.11. */
const PHONE_GL_MAKERS = ["Qualcomm", "ARM"];

/** A phone's bare WebGL vendor/renderer in the ANGLE form Chrome on Android reports (#131):
 * "Qualcomm" / "Adreno (TM) 619" become "Google Inc. (Qualcomm)" / "ANGLE (Qualcomm, Adreno (TM) 619, OpenGL ES 3.2)". */
function adoptPhoneAngleWebgl(config: Obj): void {
  const g = config["webgl"];
  if (!configClaimsMobile(config) || !isObj(g)) return;
  const maker = PHONE_GL_MAKERS.find((m) => g["vendor"] === m || g["vendor"] === `Google Inc. (${m})`);
  if (!maker) return;
  g["vendor"] = `Google Inc. (${maker})`;
  const r = g["renderer"];
  if (typeof r === "string" && r !== "" && !r.startsWith("ANGLE (")) g["renderer"] = `ANGLE (${maker}, ${r}, OpenGL ES 3.2)`;
}

/** A phone's platform_version that is no Android release (a desktop's, carried over by a template
 * pick before #131) becomes its template's, else that of a template of the same model; else it
 * stays. Never the user agent's: Chrome 158 on Android always says "Android 10" there. */
function adoptPhonePlatformVersion(config: Obj, src: Obj | null, fingerprintsDir: string): void {
  if (!configClaimsMobile(config)) return;
  const read = (o: Obj | null, block: string): string => {
    const b = o?.[block];
    const v = isObj(b) ? b["platform_version"] : undefined;
    return typeof v === "string" ? v : "";
  };
  // The engine reads client_hints over navigator.
  const current = read(config, "client_hints") || read(config, "navigator");
  if (current === "" || isAndroidRelease(current)) return;
  const release = (t: Obj | null) => (isAndroidRelease(read(t, "client_hints")) ? read(t, "client_hints") : "");
  const want = release(src) || newestOfModel(config, fingerprintsDir, release);
  if (!want) return;
  for (const b of ["client_hints", "navigator"]) {
    const v = read(config, b);
    if (v !== "" && !isAndroidRelease(v)) (config[b] as Obj)["platform_version"] = want;
  }
}

/** The newest release a library template of the profile's model carries: two can differ. */
function newestOfModel(config: Obj, fingerprintsDir: string, release: (t: Obj | null) => string): string {
  const ch = config["client_hints"];
  const model = isObj(ch) && typeof ch["model"] === "string" ? ch["model"] : "";
  if (!model || !existsSync(fingerprintsDir)) return "";
  let best = "";
  for (const f of readdirSync(fingerprintsDir).filter((n) => n.endsWith(".json"))) {
    let t: Obj;
    try { t = JSON.parse(readFileSync(join(fingerprintsDir, f), "utf8")); } catch { continue; }
    const tch = t["client_hints"];
    if (!isObj(tch) || tch["model"] !== model) continue;
    const v = release(t);
    if (v && parseInt(v, 10) > parseInt(best || "0", 10)) best = v;
  }
  return best;
}

/** WebGPU features Chrome 156 reports on Apple M-series that older templates lack. */
const APPLE_WEBGPU_ADDED = ["texture-compression-unaligned"];
/** Chrome 157 adds these from M2 on (Dawn: Apple8 with Mac2, or Apple9); M1 has none. */
const APPLE_M2_WEBGPU_ADDED = ["atomic-vec2u-min-max"];

/**
 * Add `APPLE_WEBGPU_ADDED` to an Apple M-series profile's `webgpu.features`, in
 * sorted place; touches nothing else. An absent or empty list means "the host's
 * own" and stays as it is. Mutates `config` in place. SDK equivalent of the
 * launcher's profile migration of the same name.
 */
export function applyAppleWebGpuFeatures(config: Record<string, unknown>): void {
  const renderer = (config["webgl"] as Record<string, unknown> | undefined)?.["renderer"];
  const webgpu = config["webgpu"] as Record<string, unknown> | undefined;
  if (typeof renderer !== "string" || webgpu?.["vendor"] !== "apple") return;
  const gen = /Apple M(\d+)/.exec(renderer);
  if (!gen) return;
  const list = webgpu["features"];
  if (!Array.isArray(list) || list.length === 0) return;
  const m2 = Number(gen[1]) >= 2 ? APPLE_M2_WEBGPU_ADDED : [];
  for (const feat of [...APPLE_WEBGPU_ADDED, ...m2]) {
    if (list.includes(feat)) continue;
    const i = list.findIndex((v) => typeof v === "string" && v > feat);
    list.splice(i < 0 ? list.length : i, 0, feat);
  }
}

/** Per-profile state (cookies / IndexedDB / cache) — preserved across
 *  launches. Defaults to `./shardx-profiles/<id>/` next to the running
 *  script. Override per launch with `userDataDir` or per SDK with
 *  `new ShardX({ profilesDir })`. */
export function userDataDir(runtime: Runtime, profileId: string, base?: string): string {
  const root = base ?? runtime.profilesRoot;
  const d = join(root, profileId);
  mkdirSync(d, { recursive: true });
  return d;
}
