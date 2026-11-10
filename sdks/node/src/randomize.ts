// Per-launch randomisation of hardware_concurrency / device_memory /
// platform_version. Mirrors `randomize_hardware` and
// `randomize_platform_version` in `src-tauri/src/lib.rs`.
//
// Mac profiles use the curated MAC_HW_CONFIGS table. Windows / Linux
// profiles use the launcher's host-bracketed logic: real x86 logical-core
// counts filtered to [host_cores-4, host_cores+2], device_memory floored
// by core count and ceilinged by hostRamBucketGb().
import { hostLogicalCores, hostRamBucketGb } from "./host.js";

// Chrome writes major.minor.patch, never two parts. Current releases as of
// 2026-11 (support.apple.com/100100); Sonoma ends at 14.8.9.
export const MACOS_PLATFORM_VERSIONS: readonly string[] = [
  "14.8.8", "14.8.9",
  "15.7.9", "15.8.1", "15.8.2", "15.8.2",
  "26.6.1", "26.6.2", "26.7.1", "26.7.2", "26.7.2", "26.7.2",
  "27.0.1", "27.2.0", "27.2.0", "27.2.0",
];

// Chrome reports the UniversalApiContract: 10 on Win10 2004+, 15 on Win11
// 22H2/23H2, 19 on 24H2/25H2. 13 and 16-18 exist only on Insider builds.
export const WINDOWS_PLATFORM_VERSIONS: readonly string[] = [
  "10.0.0", "10.0.0", "10.0.0", "10.0.0", "10.0.0",
  "15.0.0",
  "19.0.0", "19.0.0", "19.0.0", "19.0.0", "19.0.0", "19.0.0", "19.0.0",
];

// Chrome on Linux reports an empty version, and the engine since build 4 says
// so itself. A value is still written so an older engine does not pass on the
// host's own.
export const LINUX_PLATFORM_VERSIONS: readonly string[] = [
  "5.15.0", "6.1.0", "6.5.0",
  "6.6.0", "6.8.0", "6.10.0", "6.11.0", "6.12.0",
  "6.14.0", "6.15.0", "6.16.0",
];

/** What a version that cannot stand becomes on a Mac, and on an M4 claiming 14.x. */
const MACOS_SETTLED = "26.7.2";
const MACOS_SETTLED_M4 = "15.8.2";

type HwPair = readonly [cores: number, gib: number];

export const MAC_HW_CONFIGS: Readonly<Record<string, readonly HwPair[]>> = {
  "mac-m1-air13":     [[8, 8], [8, 16]],
  "mac-m1-mbp13":     [[8, 8], [8, 16]],
  "mac-m1-imac24":    [[8, 8], [8, 16]],
  "mac-m1-pro-mbp14": [[8, 16], [10, 16], [10, 32]],
  "mac-m1-pro-mbp16": [[8, 16], [10, 16], [10, 32]],
  "mac-m1-max-mbp14": [[10, 32]],
  "mac-m1-max-mbp16": [[10, 32]],
  "mac-m2-air13":     [[8, 8], [8, 16]],
  "mac-m2-air15":     [[8, 8], [8, 16]],
  "mac-m2-mbp13":     [[8, 8], [8, 16]],
  "mac-m2-pro-mbp14": [[10, 16], [12, 16], [12, 32]],
  "mac-m2-pro-mbp16": [[10, 16], [12, 16], [12, 32]],
  "mac-m2-max-mbp14": [[12, 32]],
  "mac-m2-max-mbp16": [[12, 32]],
  "mac-m3-air13":     [[8, 8], [8, 16]],
  "mac-m3-air15":     [[8, 8], [8, 16]],
  "mac-m3-mbp14":     [[8, 8], [8, 16]],
  "mac-m3-imac24":    [[8, 8], [8, 16]],
  "mac-m3-pro-mbp14": [[11, 16], [12, 16], [12, 32]],
  "mac-m3-pro-mbp16": [[11, 16], [12, 16], [12, 32]],
  "mac-m3-max-mbp14": [[14, 32], [16, 32]],
  "mac-m3-max-mbp16": [[14, 32], [16, 32]],
  "mac-m4-air13":     [[10, 16], [10, 32]],
  "mac-m4-air15":     [[10, 16], [10, 32]],
  "mac-m4-mbp14":     [[10, 16], [10, 32]],
  "mac-m4-imac24":    [[10, 16], [10, 32]],
  "mac-m4-pro-mbp14": [[12, 16], [14, 16], [14, 32]],
  "mac-m4-pro-mbp16": [[12, 16], [14, 16], [14, 32]],
  "mac-m4-max-mbp14": [[14, 32], [16, 32]],
  "mac-m4-max-mbp16": [[14, 32], [16, 32]],
  "mac-m5-mbp14":     [[10, 16], [10, 32]],
};

/** Real x86 logical-core counts (SMT + Intel hybrid). Same array as lib.rs. */
export const X86_CORES: readonly number[] = [4, 6, 8, 12, 16, 20, 24, 28, 32];

function pickRandom<T>(pool: readonly T[]): T {
  return pool[Math.floor(Math.random() * pool.length)];
}

function platformOf(cfg: Record<string, unknown>): string {
  const nav = cfg["navigator"] as Record<string, unknown> | undefined;
  return (nav?.["platform"] as string | undefined) ?? "";
}

/** The first macOS major the claimed Apple chip shipped with: M4 came with 15, M5 with 26. */
function macFirstMajor(cfg: Record<string, unknown>): number {
  const webgl = cfg["webgl"] as Record<string, unknown> | undefined;
  const renderer = webgl?.["renderer"];
  if (typeof renderer !== "string") return 0;
  const at = renderer.indexOf("Apple M");
  const digits = at < 0 ? null : /^\d+/.exec(renderer.slice(at + 7));
  const gen = digits ? Number(digits[0]) : 0;
  return gen >= 5 ? 26 : gen === 4 ? 15 : 0;
}

function majorOf(version: string): number | null {
  const head = version.split(".")[0];
  return /^\d+$/.test(head) ? Number(head) : null;
}

/**
 * Mutates in-place: pick fresh `navigator.platform_version` (+ mirror to client_hints).
 * A Mac gets only versions its claimed chip can run.
 */
export function randomizePlatformVersion(cfg: Record<string, unknown>): void {
  const plat = platformOf(cfg);
  const first = plat === "macOS" ? macFirstMajor(cfg) : 0;
  const pool =
    plat === "macOS"   ? MACOS_PLATFORM_VERSIONS.filter((v) => (majorOf(v) ?? 0) >= first) :
    plat === "Windows" ? WINDOWS_PLATFORM_VERSIONS :
    plat === "Linux"   ? LINUX_PLATFORM_VERSIONS : null;
  if (!pool || pool.length === 0) return;
  const v = pickRandom(pool);
  const nav = (cfg["navigator"] ??= {}) as Record<string, unknown>;
  nav["platform_version"] = v;
  const ch = cfg["client_hints"] as Record<string, unknown> | undefined;
  if (ch && typeof ch === "object") ch["platform_version"] = v;
}

function settledWindows(version: string): string {
  if (["10.0.0", "14.0.0", "15.0.0", "19.0.0"].includes(version)) return version;
  // The engine counts an unreadable version as Windows 11, and picks its fonts by that.
  const major = majorOf(version);
  return major !== null && major < 13 ? "10.0.0" : "19.0.0";
}

function settledMacos(version: string, firstMajor: number): string {
  const parts = version.split(".");
  if (!parts.every((p) => /^\d+$/.test(p))) return MACOS_SETTLED;
  const nums = parts.map(Number);
  if (nums.length < 1 || nums.length > 3 || nums[0] < 12) return MACOS_SETTLED;
  if (nums[0] < firstMajor) return firstMajor >= 26 ? MACOS_SETTLED : MACOS_SETTLED_M4;
  while (nums.length < 3) nums.push(0);
  return nums.join(".");
}

/**
 * Mutates in-place: a Windows or macOS `platform_version` Chrome could not
 * report (Windows 13/16/17, a two-part macOS version, an M4 on 14.x) becomes
 * the nearest one it can, in navigator and client_hints alike. Deterministic,
 * so launch applies it every time. Linux is left to the engine. Returns
 * whether anything changed. SDK equivalent of the launcher's function of the
 * same name.
 */
export function settlePlatformVersion(cfg: Record<string, unknown>): boolean {
  const obj = (k: string) => {
    const v = cfg[k];
    return v && typeof v === "object" && !Array.isArray(v) ? (v as Record<string, unknown>) : undefined;
  };
  const nav = obj("navigator");
  const ch = obj("client_hints");
  // The engine reads client_hints over navigator.
  const current = [ch?.["platform_version"], nav?.["platform_version"]]
    .find((v): v is string => typeof v === "string" && v !== "") ?? "";
  const plat = nav?.["platform"];
  let want: string;
  if (plat === "Windows") want = settledWindows(current);
  else if (plat === "macOS") want = settledMacos(current, macFirstMajor(cfg));
  else return false;
  let changed = false;
  for (const o of [nav, ch]) {
    if (o && o["platform_version"] !== want) {
      o["platform_version"] = want;
      changed = true;
    }
  }
  return changed;
}

/**
 * Mutates in-place: pick fresh (hardware_concurrency, device_memory).
 *
 * macOS: curated MAC_HW_CONFIGS table by profile id.
 * Windows / Linux: bracket the host CPU count within [C-4, C+2] from
 * X86_CORES; floor device_memory by core count (>=12 → 16, else 8) and
 * cap by `hostRamBucketGb()`.
 */
export function randomizeHardware(cfg: Record<string, unknown>, profileId?: string): void {
  const plat = platformOf(cfg);
  let cores: number;
  let mem: number;

  if (plat === "macOS" && profileId && profileId in MAC_HW_CONFIGS) {
    [cores, mem] = pickRandom(MAC_HW_CONFIGS[profileId]);
  } else if (plat === "Windows" || plat === "Linux") {
    const c = hostLogicalCores();
    const lo = Math.max(0, c - 4);
    const hi = c + 2;
    const candidates = X86_CORES.filter((n) => n >= lo && n <= hi);
    if (candidates.length > 0) {
      cores = pickRandom(candidates);
    } else {
      cores = X86_CORES.reduce((best, n) =>
        Math.abs(n - c) < Math.abs(best - c) ? n : best, X86_CORES[0]);
    }
    const real = hostRamBucketGb();
    const floor = cores >= 12 ? 16 : 8;
    const memCand = [8, 16, 32].filter((m) => m >= floor && m <= real);
    mem = memCand.length > 0 ? pickRandom(memCand) : real;
  } else {
    return;
  }

  const nav = (cfg["navigator"] ??= {}) as Record<string, unknown>;
  nav["hardware_concurrency"] = cores;
  nav["device_memory"] = mem;
}
