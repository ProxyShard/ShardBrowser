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
 */
export function applyEngineVersion(
  config: Record<string, unknown>,
  chromiumVersion: string,
  greaseBrand?: string,
  greaseVersion?: string,
  tls?: Record<string, unknown>,
): void {
  const parts = chromiumVersion.split(".");
  if (parts.length !== 4) return;
  const major = parts[0];
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

/** The voices an Android handset offers. Chrome on Android enumerates the Google
 *  TTS engine's voices, and the names are the engine's own: the same "Google
 *  <language>" roster Chrome ships on Linux, every one of them network-backed. */
export const ANDROID_SPEECH_VOICES: readonly (readonly [string, string])[] = [
  ["Google US English", "en-US"],
  ["Google UK English Female", "en-GB"],
  ["Google UK English Male", "en-GB"],
  ["Google Deutsch", "de-DE"],
  ["Google español", "es-ES"],
  ["Google español de Estados Unidos", "es-US"],
  ["Google français", "fr-FR"],
  ["Google हिन्दी", "hi-IN"],
  ["Google Bahasa Indonesia", "id-ID"],
  ["Google italiano", "it-IT"],
  ["Google 日本語", "ja-JP"],
  ["Google 한국의", "ko-KR"],
  ["Google Nederlands", "nl-NL"],
  ["Google polski", "pl-PL"],
  ["Google português do Brasil", "pt-BR"],
  ["Google русский", "ru-RU"],
  ["Google 普通话（中国大陆）", "zh-CN"],
  ["Google 粤語（香港）", "zh-HK"],
  ["Google 國語（臺灣）", "zh-TW"],
];

/**
 * Give a profile that claims a phone the handset's voices when it names none.
 *
 * Without a block the engine enumerates the voices of the machine it runs on: an
 * Android profile on a Mac offered the Mac's 63 Chrome voices, all local, which
 * is the list no handset has. An empty list is not the fix either — the engine
 * reads it as no claim and falls back to the host — so the roster has to be
 * there. A phone that declares its own voices is left alone. Mutates `config`.
 */
export function applyPhoneVoices(config: Record<string, unknown>): void {
  const nav = (config["navigator"] ?? {}) as Record<string, unknown>;
  const ch = (config["client_hints"] ?? {}) as Record<string, unknown>;
  const phone =
    ch["mobile"] === true ||
    String(ch["platform"] ?? "").toLowerCase().startsWith("android") ||
    String(nav["user_agent"] ?? "").toLowerCase().includes("android");
  if (!phone) return;
  const speech = config["speech"] as Record<string, unknown> | undefined;
  const declared = speech?.["voices"];
  if (Array.isArray(declared) && declared.length > 0) return;
  // Chrome's rule: the first voice in the list is the default one.
  config["speech"] = {
    voices: ANDROID_SPEECH_VOICES.map(([name, lang], i) => ({
      name, lang, local_service: false, is_default: i === 0,
    })),
  };
}

/** WebGPU features Chrome 156 reports on Apple M-series that older templates lack. */
const APPLE_WEBGPU_ADDED = ["texture-compression-unaligned"];

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
  const at = renderer.indexOf("Apple M");
  if (at < 0 || !/[0-9]/.test(renderer.charAt(at + 7))) return;
  const list = webgpu["features"];
  if (!Array.isArray(list) || list.length === 0) return;
  for (const feat of APPLE_WEBGPU_ADDED) {
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
