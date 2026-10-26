/// "auto" sentinel; the Rust launch resolver replaces with concrete TZ.
export const AUTO_TZ = "auto";
export const AUTO_LANG = "auto";

export const TIMEZONES = [
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

export const LOCALES: { code: string; label: string }[] = [
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

// Constrained options: stay within values Chrome actually reports. 2 is real
// on an entry-level handset (android-anyreal-note56-x); leaving it out left
// that profile's picker blank.
export const MEMORY_OPTIONS = [2, 4, 8, 16, 32];
// Counts the bundled templates name (a 9-core Pixel, a 28- or 40-core
// workstation); a number missing here left the picker blank on that profile.
export const CPU_OPTIONS = [2, 4, 6, 8, 9, 10, 12, 14, 16, 20, 22, 24, 28, 32, 40];
export const MEDIA_COUNT_OPTIONS = [0, 1, 2, 3];
/// Refresh rates real panels ship with. A page reads this by timing
/// requestAnimationFrame, so a profile that names none reports 60 — what most
/// machines have — rather than whatever this computer's screen does.
export const REFRESH_RATE_OPTIONS = [60, 75, 90, 100, 120, 144, 165, 240];

/// Desktop resolutions a profile may claim, largest first. Offered on Windows
/// and Linux only, and only down to what this machine can actually show: a
/// window cannot be bigger than the monitor it opens on, so a larger claim is
/// contradicted the moment the page reads window.outerWidth.
export const SCREEN_RESOLUTIONS: readonly (readonly [number, number])[] = [
  [3840, 2160], [3440, 1440], [2560, 1600], [2560, 1440], [2560, 1080],
  [1920, 1200], [1920, 1080], [1680, 1050], [1600, 900], [1536, 864],
  [1440, 900], [1366, 768], [1280, 1024], [1280, 800], [1280, 720],
];

/// Common remote-control/proxy ports to block from outgoing browser connects.
export const DEFAULT_BLOCKED_PORTS = [
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

// "Maximally soft" anti-fingerprint defaults: the smallest perturbation that
// still shifts the fingerprint hash without visibly degrading rendering.
// max_offset can't drop below 1 (0 = off), so client_rects is already at its
// gentlest floor.
export const WEBGL_NOISE_INTENSITY = 0.0005;
export const CLIENT_RECTS_MAX_OFFSET = 1;

export const OS_OPTIONS: { id: string; label: string }[] = [
  { id: "macOS",   label: "macOS"   },
  { id: "Windows", label: "Windows" },
  { id: "Linux",   label: "Linux"   },
  { id: "Android", label: "Android" },
];

// Which selector entry a fingerprint belongs under. A handset's
// navigator.platform is "Linux armv81", so matching Linux by prefix would file
// every phone under it.
export function osIdFor(platform: string): string {
  return OS_OPTIONS.find((o) => matchesOs(platform, o.id))?.id ?? platform;
}

export function matchesOs(platform: string, os: string): boolean {
  const p = (platform || "").toLowerCase();
  const isAndroid = p.startsWith("android") || p.startsWith("linux arm");
  if (os === "Android") return isAndroid;
  if (os === "Linux") return p.startsWith("linux") && !isAndroid;
  // An imported fingerprint keeps its navigator.platform: "Win32", "MacIntel".
  if (os === "Windows") return p.startsWith("win");
  if (os === "macOS") return p.startsWith("mac");
  return p === os.toLowerCase();
}

export { ACCENT_HEX, LABEL_COLORS, labelColor, type LabelColorId } from "./labelColors";
