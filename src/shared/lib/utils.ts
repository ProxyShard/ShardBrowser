import { appLocale } from "../i18n";

// Host OS of the launcher window (never spoofed) — drives default OS tab + titlebar.
export function detectHostOs(): "macOS" | "Windows" | "Linux" {
  const ua = navigator.userAgent;
  if (/Windows/i.test(ua)) return "Windows";
  if (/Macintosh|Mac OS X/i.test(ua)) return "macOS";
  if (/Linux|X11|CrOS/i.test(ua)) return "Linux";
  return "macOS";
}

export const HOST_OS = detectHostOs();

/// A launcher date as milliseconds: the backend writes "@<unix seconds>";
/// older or imported data may hold an ISO string. Null when there is none.
export function stampMs(stamp: string | null | undefined): number | null {
  if (!stamp || stamp === "@") return null;
  const ms = stamp.startsWith("@") ? Number(stamp.slice(1)) * 1000 : Date.parse(stamp);
  return Number.isFinite(ms) ? ms : null;
}

const TS_FORMATS = new Map<string, Intl.DateTimeFormat>();

/// A launcher date ("@<unix seconds>" or ISO) as "Sep 25, 6:48 PM", in the UI
/// language and the computer's time zone. One formatter per language: this
/// runs on every row render (PERF-12). Anything unreadable passes through.
export function fmtTs(stamp: string): string {
  const ms = stampMs(stamp);
  if (ms === null) return stamp;
  const locale = appLocale();
  let format = TS_FORMATS.get(locale);
  if (!format) {
    format = new Intl.DateTimeFormat(locale, { month: "short", day: "numeric", hour: "numeric", minute: "2-digit" });
    TS_FORMATS.set(locale, format);
  }
  return format.format(ms);
}

const RELATIVE_FORMATS = new Map<string, Intl.RelativeTimeFormat>();

/// A launcher date as how long ago it was ("Yesterday", "5 hours ago"), in the
/// UI language; a month or more back, the day itself. `now` is the minute
/// clock's time, so a list's rows move on together. The full date is for a
/// tooltip (`fmtTs`). `capital` false: as it reads inside a sentence.
export function fmtRelative(stamp: string, now: number, capital = true): string {
  const ms = stampMs(stamp);
  if (ms === null) return "";
  const secs = Math.round((now - ms) / 1000);
  if (secs >= 30 * 86_400 || secs < -60) return fmtDay(stamp);
  const locale = appLocale();
  let format = RELATIVE_FORMATS.get(locale);
  if (!format) {
    format = new Intl.RelativeTimeFormat(locale, { numeric: "auto" });
    RELATIVE_FORMATS.set(locale, format);
  }
  const text = secs < 60 ? format.format(0, "second")
    : secs < 3600 ? format.format(-Math.floor(secs / 60), "minute")
    : secs < 86_400 ? format.format(-Math.floor(secs / 3600), "hour")
    : format.format(-Math.floor(secs / 86_400), "day");
  // Sentence case: a cell starts with a capital.
  return capital ? text.charAt(0).toLocaleUpperCase(locale) + text.slice(1) : text;
}

const DAY_FORMATS = new Map<string, Intl.DateTimeFormat>();

/// A launcher date as a day ("Aug 15"; "Aug 15, 2025" in another year), in
/// the UI language. For quiet lines where the time of day is noise.
export function fmtDay(stamp: string): string {
  const ms = stampMs(stamp);
  if (ms === null) return "";
  const locale = appLocale();
  const sameYear = new Date(ms).getFullYear() === new Date().getFullYear();
  const key = `${locale}|${sameYear}`;
  let format = DAY_FORMATS.get(key);
  if (!format) {
    format = new Intl.DateTimeFormat(locale, sameYear ? { month: "short", day: "numeric" } : { month: "short", day: "numeric", year: "numeric" });
    DAY_FORMATS.set(key, format);
  }
  return format.format(ms);
}

/// An address without its "http://" or "https://": a list shows the part
/// that tells one site from another (CRAFT-27); the whole one is on hover.
export const stripScheme = (url: string) => url.replace(/^https?:\/\//i, "");

/// An address as a person reads it: without "http://" or "https://" (kept
/// with `keepScheme`), and with its percent-encoding undone, so a Cyrillic
/// path reads as Cyrillic. `decodeURI` leaves what only an encoding can carry
/// (%2F, %3F, %23) alone; text that is not valid encoding (a stray "%")
/// comes back as typed, never as an error.
export function readableUrl(url: string, keepScheme = false): string {
  const bare = stripScheme(url);
  let readable = bare;
  try { readable = decodeURI(bare); } catch { /* not valid encoding: the raw text */ }
  return keepScheme ? url.slice(0, url.length - bare.length) + readable : readable;
}

/// Whether the text is an address a bookmark can hold. The backend takes
/// anything but an empty one and adds "https://" when there is no "://", so
/// this only turns away what was never finished:
/// - scheme-less text and http(s) need a host with a dot (example.com),
///   localhost, or a bracketed IPv6 address ("goog" and "https://goog" are an
///   unfinished host);
/// - a single-label host with a scheme (http://nas:5000, http://router/) is
///   an intranet address once it goes on past the host: a port, a path, a
///   query or a fragment;
/// - any other scheme (chrome://flags, file:///C:/x.html) only needs
///   something after "://";
/// - a bookmarklet (javascript:…) needs its script, and is kept as written.
export function isFullUrl(text: string): boolean {
  const t = text.trim();
  if (!t) return false;
  if (/^javascript:/i.test(t)) return t.slice(11).trim() !== "";
  const at = t.indexOf("://");
  const scheme = at >= 0;
  let url: URL;
  try { url = new URL(scheme ? t : `https://${t}`); } catch { return false; }
  // What follows "://" (the whole text when no scheme was typed).
  const rest = scheme ? t.slice(at + 3) : t;
  if (scheme && !/^https?:$/.test(url.protocol)) return rest.trim() !== "";
  // The URL parser reads "12345" as the address 0.0.48.57 and "goog." as a
  // host with a dot in it: neither is a finished name, so the dot is judged
  // on what was typed.
  const host = url.hostname.replace(/\.$/, "");
  const typed = rest.split(/[/?#]/)[0].replace(/^.*@/, "").replace(/:\d*$/, "");
  if ((host.includes(".") && !/^\d+$/.test(typed)) || host === "localhost" || host.startsWith("[")) return true;
  return scheme && /:\d|[/?#]/.test(rest);
}

/// Whole days from now until a unix-second instant; 0 once it has passed.
export function daysUntil(unixSecs: number): number {
  return Math.max(0, Math.ceil((unixSecs * 1000 - Date.now()) / 86_400_000));
}

// Single UTM tag appended to every outbound proxyshard.com link.
export const UTM_QS = "utm_source=shardx&utm_medium=referral&utm_campaign=shardx-launcher";
export const withUtm = (url: string) => url + (url.includes("?") ? "&" : "?") + UTM_QS;

// ProxyShard dashboard (account, billing, API key).
export const DASHBOARD_URL = withUtm("https://dashboard.proxyshard.com/");

export const GH_REPO_URL = "https://github.com/ProxyShard/ShardBrowser";

// The product site; utm_content names the place in the launcher the click came from.
export const SITE_HOST = "shardbrowser.com";
export const siteUrl = (placement: string) =>
  `https://${SITE_HOST}/?utm_source=launcher&utm_medium=app&utm_campaign=shardbrowser&utm_content=${placement}`;

/// Build accept-language chain (primary → base → English fallback).
export function deriveAcceptLanguage(loc: string): string {
  if (!loc) return "en-US,en;q=0.9";
  const base = loc.split("-")[0];
  if (loc === "en-US") return "en-US,en;q=0.9";
  return `${loc},${base};q=0.9,en-US;q=0.8,en;q=0.7`;
}

export function deriveLanguagesArray(loc: string): string[] {
  if (!loc) return ["en-US", "en"];
  const base = loc.split("-")[0];
  if (loc === "en-US") return ["en-US", "en"];
  return [loc, base, "en-US", "en"];
}

// 12-char lowercase alnum sticky-session id (matches ProxyShard's `sid` form).
export const randSid = () => {
  const a = "abcdefghijklmnopqrstuvwxyz0123456789";
  let s = "";
  for (let i = 0; i < 12; i++) s += a[Math.floor(Math.random() * a.length)];
  return s;
};

export const isDcIsp = (name: string) => /datacenter|isp/i.test(name);

/// available-count product code for a base product name.
export const availCode = (name: string) => (/datacenter/i.test(name) ? "dc" : /isp/i.test(name) ? "isp" : "");

/// A random id. `crypto.randomUUID` needs a secure context, which a
/// custom-scheme window is not guaranteed to be.
export function randomId(): string {
  try {
    if (typeof crypto?.randomUUID === "function") return crypto.randomUUID();
  } catch { /* not a secure context */ }
  return `${Date.now().toString(36)}-${Math.random().toString(36).slice(2, 12)}`;
}

/// Own properties only: a key from outside ("__proto__", "constructor")
/// must never reach Object.prototype (SEC-14).
export const hasOwn = (obj: object, key: PropertyKey): boolean => Object.prototype.hasOwnProperty.call(obj, key);
