import type { ProfileForm, NoiseMode } from "./types";
import type { FingerprintEntry } from "../../fingerprint/model/types";
import { AUTO_TZ, AUTO_LANG, DEFAULT_BLOCKED_PORTS, WEBGL_NOISE_INTENSITY, CLIENT_RECTS_MAX_OFFSET } from "../../../shared/constants";
import { deriveAcceptLanguage, deriveLanguagesArray } from "../../../shared/lib/utils";

export const defaultForm = (): ProfileForm => ({
  id: "",
  rev: 0,
  name: "",
  notes: "",
  proxy_id: null,
  color: "",
  icon_png: "",
  extensions: [],
  android_media: false,
  refresh_rate: 60,
  screen_w: 0,
  screen_h: 0,
  cookies_file: "",

  // Empty until snapped to gpusForOs[0] by useEffect.
  gpu_preset_id: "",
  user_agent:
    "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/152.0.0.0 Safari/537.36",
  hardware_concurrency: 8,
  device_memory: 16,
  // Empty = inherit donor; setGpu refreshes via enrich_picks_for_preset.
  platform_version: "",

  timezone: AUTO_TZ,
  language: AUTO_LANG,

  webrtc: "auto",
  do_not_track: false,

  noise_canvas: "real",
  noise_webgl: "real",
  noise_audio: "real",
  noise_client_rects: "real",
  noise_sensors: "real",
  noise_fonts: "real",
  blocked_ports: DEFAULT_BLOCKED_PORTS.slice(),

  geo_mode: "auto",
  geo_lat: 52.2297,
  geo_lng: 21.0122,
  geo_accuracy: 50,

  media_audio_in: 1,
  media_audio_out: 1,
  media_video_in: 1,
});

export function fromStored(stored: any): ProfileForm {
  const f = defaultForm();
  if (!stored) return f;
  f.id = stored?._meta?.id ?? "";
  f.rev = stored?._meta?.rev ?? 0;
  f.proxy_id = stored?._meta?.proxy_id ?? null;
  f.name = stored?.name ?? "";
  f.notes = stored?.notes ?? "";
  f.color = stored?._meta?.color ?? "";
  f.icon_png = stored?._meta?.icon_png ?? "";
  f.extensions = Array.isArray(stored?._meta?.extensions) ? stored._meta.extensions : [];
  f.android_media = stored?._meta?.android_media === true;
  // Empty for legacy profiles; snapped by useEffect.
  f.gpu_preset_id = stored?._meta?.gpu_preset_id ?? "";
  f.user_agent = stored?.navigator?.user_agent ?? f.user_agent;
  f.hardware_concurrency = stored?.navigator?.hardware_concurrency ?? 8;
  f.refresh_rate = stored?.screen?.refresh_rate ?? 60;
  f.screen_w = stored?.screen?.width ?? 0;
  f.screen_h = stored?.screen?.height ?? 0;
  f.device_memory = stored?.navigator?.device_memory ?? 16;
  f.timezone = stored?.timezone ?? AUTO_TZ;
  f.language = stored?.navigator?.language ?? AUTO_LANG;
  f.webrtc = (stored?.webrtc === "replace" ? "tcp_only" : stored?.webrtc) ?? "auto";
  f.do_not_track = !!stored?.navigator?.do_not_track;

  const noise = stored?.noise ?? {};
  const noiseMode = (n: any): NoiseMode => (n?.enabled ? "auto" : "real");
  f.noise_canvas = noiseMode(noise.canvas);
  f.noise_webgl = noiseMode(noise.webgl);
  f.noise_audio = noiseMode(noise.audio);
  f.noise_client_rects = noiseMode(noise.client_rects);
  f.noise_sensors = noiseMode(noise.sensors);
  // Fonts default OFF (real); mirrors C++ default.
  f.noise_fonts = noiseMode(noise.fonts);
  f.blocked_ports = Array.isArray(stored?.blocked_ports)
    ? stored.blocked_ports.filter((n: any) => typeof n === "number")
    : DEFAULT_BLOCKED_PORTS.slice();

  const geo = stored?.geolocation ?? {};
  f.geo_mode = geo.mode === "manual" ? "manual" : "auto";
  f.geo_lat = typeof geo.latitude === "number" ? geo.latitude : f.geo_lat;
  f.geo_lng = typeof geo.longitude === "number" ? geo.longitude : f.geo_lng;
  f.geo_accuracy = typeof geo.accuracy === "number" ? geo.accuracy : f.geo_accuracy;

  const md = stored?.media_devices ?? {};
  f.media_audio_in = md.audio_input_count ?? 1;
  f.media_audio_out = md.audio_output_count ?? 1;
  f.media_video_in = md.video_input_count ?? 1;

  return f;
}

/// Build on-disk FingerprintConfig from library payload + user-edited fields.
export function toStored(f: ProfileForm, lib: FingerprintEntry | null): any {
  const base: any = lib && lib.payload ? JSON.parse(JSON.stringify(lib.payload)) : {};

  base._meta = {
    id: f.id,
    rev: f.rev,
    proxy_id: f.proxy_id,
    last_launched_at: null,
    gpu_preset_id: f.gpu_preset_id,
    // Absent, not empty: that is what "derive it" means on disk.
    ...(f.color ? { color: f.color } : {}),
    // Explicit null distinguishes Reset from older clients omitting this field.
    icon_png: f.icon_png || null,
    extensions: f.extensions,
    ...(f.android_media ? { android_media: true } : {}),
  };
  base.name = f.name || "untitled";
  base.notes = f.notes;
  // "auto" sentinel: resolver replaces at launch; persists across edits.
  base.timezone = f.timezone;
  base.icu_locale = f.language === AUTO_LANG ? null : f.language;
  base.webrtc = f.webrtc;

  base.navigator = {
    ...(base.navigator || {}),
    language: f.language,
    accept_language: f.language === AUTO_LANG ? null : deriveAcceptLanguage(f.language),
    languages: f.language === AUTO_LANG ? null : deriveLanguagesArray(f.language),
    user_agent: f.user_agent,
    hardware_concurrency: f.hardware_concurrency,
    device_memory: f.device_memory,
    // Empty → inherit donor; set → write to both navigator + client_hints.
    ...(f.platform_version ? { platform_version: f.platform_version } : {}),
    do_not_track: f.do_not_track ? "1" : null,
  };
  if (f.platform_version) {
    base.client_hints = {
      ...(base.client_hints || {}),
      platform_version: f.platform_version,
    };
  }

  // The rest of the screen block comes from the fingerprint template; only the
  // refresh rate is the operator's to set, because it is the one part of it a
  // page reads by timing rather than by asking.
  base.screen = {
    ...(base.screen || {}),
    refresh_rate: f.refresh_rate,
  };

  // A chosen resolution replaces the template's. avail_* keeps the template's
  // own insets — the menubar or taskbar strip — instead of being invented,
  // because a screen whose avail equals its full height is a screen with no
  // system chrome, which no ordinary desktop has.
  if (f.screen_w > 0 && f.screen_h > 0) {
    const tpl = (base.screen || {}) as Record<string, number>;
    const insetW = Math.max((tpl.width ?? f.screen_w) - (tpl.avail_width ?? tpl.width ?? f.screen_w), 0);
    const insetH = Math.max((tpl.height ?? f.screen_h) - (tpl.avail_height ?? tpl.height ?? f.screen_h), 0);
    base.screen = {
      ...base.screen,
      width: f.screen_w,
      height: f.screen_h,
      avail_width: Math.max(f.screen_w - insetW, 1),
      avail_height: Math.max(f.screen_h - insetH, 1),
    };
  }

  base.media_devices = {
    audio_input_count: f.media_audio_in,
    audio_output_count: f.media_audio_out,
    video_input_count: f.media_video_in,
  };

  base.geolocation =
    f.geo_mode === "manual"
      ? { mode: "manual", latitude: f.geo_lat, longitude: f.geo_lng, accuracy: f.geo_accuracy }
      : { mode: "auto" };

  // seed: 0 is the "derive automatically" sentinel — the launcher fills each
  // vector with a stable per-profile seed once the real profile id exists
  // (see fill_noise_seeds in profile.rs).  Computing seeds here is impossible
  // for new profiles (no id yet) and previously collapsed every new profile
  // onto one shared seed, giving them all an identical fingerprint.
  base.noise = {
    canvas:       { enabled: f.noise_canvas === "auto",       seed: 0 },
    webgl:        { enabled: f.noise_webgl === "auto",        seed: 0, intensity: f.noise_webgl === "auto" ? WEBGL_NOISE_INTENSITY : 0 },
    audio:        { enabled: f.noise_audio === "auto",        seed: 0 },
    client_rects: { enabled: f.noise_client_rects === "auto", seed: 0, max_offset: f.noise_client_rects === "auto" ? CLIENT_RECTS_MAX_OFFSET : 0 },
    sensors:      { enabled: f.noise_sensors === "auto",      seed: 0 },
    fonts:        { enabled: f.noise_fonts === "auto",        seed: 0 },
  };
  base.blocked_ports = [...f.blocked_ports].sort((a, b) => a - b);

  return base;
}

/** Whether a form describes a phone. By the user agent: the OS control offers no
 *  phone, and navigator.platform on Chrome for Android says "Linux armv8l". */
export function claimsMobile(f: { user_agent?: string }): boolean {
  return (f.user_agent || "").toLowerCase().includes("android");
}
