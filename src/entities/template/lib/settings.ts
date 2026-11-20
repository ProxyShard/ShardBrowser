/// What of a stored profile a template carries (templates.rs `settings_of`):
/// never its name, notes, cookies or anything of its fingerprint. No imports,
/// so the mock backend and node tests read it as it is.

const TOP = ["timezone", "icu_locale", "ui_language", "webrtc", "geolocation", "blocked_ports", "media_devices", "noise"] as const;
const NESTED: Record<string, readonly string[]> = {
  navigator: ["language", "accept_language", "languages", "do_not_track"],
  screen: ["refresh_rate"],
  window: ["keep_focus", "maximized"],
  _meta: ["proxy_id", "color", "extensions", "android_media"],
};

const clone = <T>(v: T): T => (v === undefined ? v : JSON.parse(JSON.stringify(v)));

/** A template's part of a stored profile; each noise seed is left to the new profile. */
export function templateSettingsOf(stored: any): Record<string, any> {
  const out: Record<string, any> = {};
  if (!stored || typeof stored !== "object") return out;
  for (const k of TOP) if (k in stored) out[k] = clone(stored[k]);
  for (const [block, keys] of Object.entries(NESTED)) {
    const src = stored[block];
    if (!src || typeof src !== "object") continue;
    const picked = Object.fromEntries(keys.filter((k) => k in src).map((k) => [k, clone(src[k])]));
    if (Object.keys(picked).length > 0) out[block] = picked;
  }
  if (out.noise && typeof out.noise === "object") {
    for (const v of Object.values(out.noise)) if (v && typeof v === "object") (v as Record<string, unknown>).seed = 0;
  }
  return out;
}

/** The OS control's name for a fingerprint's platform (templates.rs `os_of`). */
export function osOfPlatform(platform: string): string {
  const p = (platform || "").trim().toLowerCase();
  if (p.startsWith("android") || p.startsWith("linux arm")) return "Android";
  if (p.startsWith("linux")) return "Linux";
  if (p.startsWith("win")) return "Windows";
  if (p.startsWith("mac") || p === "darwin" || p === "osx") return "macOS";
  return "";
}
