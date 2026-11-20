// Screen strategies — three modes that match what the launcher does in
// `clamp_screen_to_real_display` (`src-tauri/src/lib.rs`):
//
//   "profile"     — keep whatever the fingerprint claims.
//   "cap_to_host" — macOS default. Scale screen/window down if host is
//                   smaller than the FP claim; no-op otherwise.
//   "use_host"    — Win/Linux default. Overwrite screen/window with the
//                   host display, subtract a taskbar inset for avail_h.
//
// All modes leave DPR / color_depth / orientation / etc. untouched.
import { platform as osPlatform } from "node:os";
import { hostScreenSize } from "./host.js";
import { configClaimsMobile } from "./profile.js";

export type ScreenStrategy = "profile" | "cap_to_host" | "use_host";

export function applyScreenStrategy(cfg: Record<string, unknown>, mode: ScreenStrategy | string): void {
  // A phone keeps the handset's screen in every mode, as in the launcher (#131).
  if (mode === "profile" || configClaimsMobile(cfg)) return;
  const host = hostScreenSize();
  if (!host) return;
  const [hw, hh] = host;
  if (mode === "cap_to_host") capToHost(cfg, hw, hh);
  else if (mode === "use_host") useHost(cfg, hw, hh);
}

function asInt(v: unknown): number {
  const n = typeof v === "number" ? v : Number(v);
  return Number.isFinite(n) ? Math.floor(n) : 0;
}

function capToHost(cfg: Record<string, unknown>, hw: number, hh: number): void {
  const scr = cfg["screen"] as Record<string, unknown> | undefined;
  if (!scr || typeof scr !== "object") return;
  const fpW = asInt(scr["width"]);
  const fpH = asInt(scr["height"]);
  if (fpW <= 0 || fpH <= 0) return;
  if (hw >= fpW && hh >= fpH) return;

  const ratio = Math.min(hw / fpW, hh / fpH);
  const newW = Math.max(1, Math.round(fpW * ratio));
  const newH = Math.max(1, Math.round(fpH * ratio));
  const fpAw = asInt(scr["avail_width"])  || fpW;
  const fpAh = asInt(scr["avail_height"]) || fpH;
  const newAw = Math.max(1, Math.round(fpAw * ratio));
  const newAh = Math.max(1, Math.round(fpAh * ratio));

  scr["width"]        = newW;
  scr["height"]       = newH;
  scr["avail_width"]  = newAw;
  scr["avail_height"] = newAh;

  const win = cfg["window"] as Record<string, unknown> | undefined;
  if (win && typeof win === "object") {
    for (const k of ["outer_width", "inner_width", "outer_height", "inner_height"] as const) {
      const v = asInt(win[k]);
      if (v > 0) win[k] = Math.max(1, Math.round(v * ratio));
    }
  }
}

function useHost(cfg: Record<string, unknown>, hw: number, hh: number): void {
  const taskbar = osPlatform() === "win32" ? 40 : 0;
  const availW = hw;
  const availH = Math.max(1, hh - taskbar);

  const scr = (cfg["screen"] ??= {}) as Record<string, unknown>;
  scr["width"]        = hw;
  scr["height"]       = hh;
  scr["avail_width"]  = availW;
  scr["avail_height"] = availH;

  const win = (cfg["window"] ??= {}) as Record<string, unknown>;
  win["outer_width"]  = availW;
  win["outer_height"] = Math.max(1, availH - 1);
  win["inner_width"]  = availW;
  win["inner_height"] = Math.max(1, availH - 88);
}

/** Map `navigator.platform` → the launcher's default screen mode. A phone is "profile"
 *  whatever its platform says: an imported one can say plain "Linux". */
export function defaultScreenModeFor(platform: string, mobile = false): ScreenStrategy {
  if (mobile) return "profile";
  if (platform === "macOS") return "cap_to_host";
  if (platform === "Windows" || platform === "Linux") return "use_host";
  return "profile";
}
