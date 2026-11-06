// Profile = a fingerprint JSON + a per-launch working dir. Wraps the
// bundled fingerprint library and lets callers override fields before
// launch.
import { existsSync, mkdirSync, readdirSync, readFileSync } from "node:fs";
import { join } from "node:path";

import type { Runtime } from "./runtime.js";

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
    const ch = this.config["client_hints"] as Record<string, unknown> | undefined;
    if (ch?.["mobile"] === true) return true;
    const chPlatform = String(ch?.["platform"] ?? "").toLowerCase();
    if (chPlatform.startsWith("android")) return true;
    const nav = this.config["navigator"] as Record<string, unknown> | undefined;
    return String(nav?.["user_agent"] ?? "").toLowerCase().includes("android");
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
 * `androidVersion` is the Chrome for Android build of the same major, passed
 * for phone profiles: Android ships its own build numbers, and a phone
 * reporting a desktop one claims a build no phone has.
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
  if (!key || !/^[A-Za-z0-9._-]+$/.test(key)) return;
  const path = join(fingerprintsDir, `${key}.json`);
  if (!existsSync(path)) return;
  let src: Record<string, unknown>;
  try { src = JSON.parse(readFileSync(path, "utf8")); } catch { return; }
  for (const k of ["fonts", "fonts_local"]) {
    if (config[k] === undefined && src[k] !== undefined) config[k] = src[k];
  }
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
