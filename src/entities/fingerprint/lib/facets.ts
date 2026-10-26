import { osIdFor } from "../../../shared/constants";
import type { FingerprintEntry } from "../model/types";

export type DeviceType = "desktop" | "phone";
export type GpuVendor = "NVIDIA" | "AMD" | "Intel" | "Apple" | "Qualcomm" | "Arm" | "Other";

/** The OS a fingerprint belongs under, by the OS control's names: a handset's
 *  "Linux armv81" platform is Android. */
export function fingerprintOs(e: FingerprintEntry): string {
  return osIdFor(e.platform || "");
}

/** A phone when its user agent says Mobile or its OS is Android. */
export function deviceTypeOf(e: FingerprintEntry): DeviceType {
  const ua = String(e.payload?.navigator?.user_agent ?? "");
  return /\bMobile\b/.test(ua) || fingerprintOs(e) === "Android" ? "phone" : "desktop";
}

/** Who made the GPU, from the renderer string. */
export function gpuVendorOf(e: FingerprintEntry): GpuVendor {
  const g = (e.gpu || "").toLowerCase();
  if (/nvidia|geforce|quadro/.test(g)) return "NVIDIA";
  if (/\bamd\b|radeon/.test(g)) return "AMD";
  if (/apple/.test(g)) return "Apple";
  if (/adreno|qualcomm/.test(g)) return "Qualcomm";
  if (/mali|\barm\b/.test(g)) return "Arm";
  if (/intel/.test(g)) return "Intel";
  return "Other";
}

export type FingerprintFilters = { os: string; device: "all" | DeviceType; vendor: "all" | GpuVendor };
export const noFingerprintFilters = (): FingerprintFilters => ({ os: "all", device: "all", vendor: "all" });

/** The library entries a search and the filters keep. */
export function filterFingerprints(items: FingerprintEntry[], search: string, f: FingerprintFilters): FingerprintEntry[] {
  const q = search.trim().toLowerCase();
  return items.filter((e) =>
    (f.os === "all" || fingerprintOs(e) === f.os)
    && (f.device === "all" || deviceTypeOf(e) === f.device)
    && (f.vendor === "all" || gpuVendorOf(e) === f.vendor)
    && (!q || [e.label, e.gpu, e.platform, e.chrome].some((s) => (s || "").toLowerCase().includes(q))));
}
