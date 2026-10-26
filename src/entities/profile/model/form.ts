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
  extensions: [],
  android_media: false,
  refresh_rate: 60,
  screen_w: 0,
  screen_h: 0,
  screen_custom: false,
  screen_host: false,
  screen_dpr: 0,
  keep_focus: false,
  cookies_file: "",

  // Empty until snapped to gpusForOs[0] by useEffect.
  gpu_preset_id: "",
  real_gpu: false,
  user_agent:
    "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/152.0.0.0 Safari/537.36",
  hardware_concurrency: 8,
  device_memory: 16,
  // Empty = inherit donor; setGpu refreshes via enrich_picks_for_preset.
  platform_version: "",

  timezone: AUTO_TZ,
  language: AUTO_LANG,
  ui_language: "system",

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
  f.extensions = Array.isArray(stored?._meta?.extensions) ? stored._meta.extensions : [];
  f.android_media = stored?._meta?.android_media === true;
  // Empty for legacy profiles; snapped by useEffect.
  f.gpu_preset_id = stored?._meta?.gpu_preset_id ?? "";
  f.real_gpu = stored?._meta?.real_gpu === true;
  f.user_agent = stored?.navigator?.user_agent ?? f.user_agent;
  f.hardware_concurrency = stored?.navigator?.hardware_concurrency ?? 8;
  f.refresh_rate = stored?.screen?.refresh_rate ?? 60;
  f.screen_w = stored?.screen?.width ?? 0;
  f.screen_h = stored?.screen?.height ?? 0;
  // Every profile has a screen, so its size alone says nothing about who chose
  // it. Only the marker does, and only it may travel back to disk.
  f.screen_custom = stored?._meta?.screen_custom === true;
  f.keep_focus = stored?.window?.keep_focus === true;
  f.device_memory = stored?.navigator?.device_memory ?? 16;
  f.timezone = stored?.timezone ?? AUTO_TZ;
  f.language = stored?.navigator?.language ?? AUTO_LANG;
  f.ui_language = stored?.ui_language === "profile" ? "profile" : "system";
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
    ? [...new Set<number>(stored.blocked_ports.filter((n: any) => typeof n === "number"))]
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

/// Build on-disk FingerprintConfig from the user-edited fields on top of a
/// base: the profile as stored (`prev`), when its template stays the same, so
/// the parts the form doesn't show (set by the API, or kept from a template no
/// longer in the library) survive an edit; else the library payload.
export function toStored(f: ProfileForm, lib: FingerprintEntry | null, prev?: any): any {
  const keep = prev && (prev._meta?.gpu_preset_id ?? "") === f.gpu_preset_id;
  const source = keep ? prev : lib?.payload;
  const base: any = source ? JSON.parse(JSON.stringify(source)) : {};
  delete base._meta;
  // Noise seeds the stored profile already has stay; 0 asks the backend for one.
  const seedOf = (k: string): number => (keep ? prev.noise?.[k]?.seed : 0) ?? 0;

  base._meta = {
    id: f.id,
    rev: f.rev,
    proxy_id: f.proxy_id,
    last_launched_at: null,
    gpu_preset_id: f.gpu_preset_id,
    ...(f.real_gpu ? { real_gpu: true } : {}),
    // Absent, not empty: that is what "derive it" means on disk.
    ...(f.color ? { color: f.color } : {}),
    extensions: f.extensions,
    ...(f.android_media ? { android_media: true } : {}),
    // The operator picked a resolution rather than taking the template's. The
    // launcher reads this to leave the global screen setting out of the launch:
    // that setting exists for macOS profiles, and applying it on top of a
    // deliberate choice would silently overrule it.
    ...(f.screen_custom && f.screen_w > 0 && f.screen_h > 0 ? { screen_custom: true } : {}),
  };
  // "This computer's graphics card": the profile simply carries no graphics
  // block, and the browser then reports what is really here. Nothing else
  // about the profile changes — the template it was built on stays named, so
  // turning this off puts that card back.
  if (f.real_gpu) {
    delete base.webgl;
    delete base.webgpu;
  }
  base.name = f.name || "untitled";
  base.notes = f.notes;
  // "auto" sentinel: resolver replaces at launch; persists across edits.
  base.timezone = f.timezone;
  base.icu_locale = f.language === AUTO_LANG ? null : f.language;
  // Absent = this computer's language, as before; only the opt-in is written.
  if (f.ui_language === "profile") base.ui_language = "profile";
  else delete base.ui_language;
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
  // system chrome, which no ordinary desktop has. This computer's screen (a
  // new profile's default) is written the same way, without the mark.
  if ((f.screen_custom || f.screen_host) && f.screen_w > 0 && f.screen_h > 0) {
    const tpl = (base.screen || {}) as Record<string, number>;
    const insetW = Math.max((tpl.width ?? f.screen_w) - (tpl.avail_width ?? tpl.width ?? f.screen_w), 0);
    const insetH = Math.max((tpl.height ?? f.screen_h) - (tpl.avail_height ?? tpl.height ?? f.screen_h), 0);
    const availW = Math.max(f.screen_w - insetW, 1);
    const availH = Math.max(f.screen_h - insetH, 1);
    base.screen = {
      ...base.screen,
      width: f.screen_w,
      height: f.screen_h,
      avail_width: availW,
      avail_height: availH,
      // This computer's size goes with its scale (launch.rs `claim_host_screen`).
      ...(f.screen_host && f.screen_dpr > 0 ? { device_pixel_ratio: f.screen_dpr } : {}),
    };
    // The window has to move with it. Without a window block the engine has no
    // --window-size to pass, and Chromium then sizes the window from the REAL
    // monitor's work area — which is how a profile claiming 1920x1080 ends up
    // in a window that does not fit inside its own claimed screen. Only for a
    // picked resolution: a window the template or the API captured stays.
    //
    // Shape follows the library's own desktop entries: the window fills the
    // work area, one pixel short, and the viewport is 87 px shorter than that
    // — the browser's own chrome.
    const outerH = Math.max(availH - 1, 1);
    base.window = {
      ...(base.window || {}),
      outer_width: availW,
      outer_height: outerH,
      inner_width: availW,
      inner_height: Math.max(outerH - 87, 1),
    };
  } else if (!(f.screen_w > 0) && lib?.payload?.screen) {
    // "As in the fingerprint": the template's own screen and window come back.
    // Building on the stored profile would otherwise keep the old pick.
    base.screen = { ...JSON.parse(JSON.stringify(lib.payload.screen)), refresh_rate: f.refresh_rate };
    if (lib.payload.window) base.window = JSON.parse(JSON.stringify(lib.payload.window));
    else delete base.window;
  }

  // "Always focused": one flag inside the window block, written only while it
  // is on, so a profile that never asked for it keeps its window exactly as it
  // was. Last of the window work on purpose — the branches above rebuild the
  // block and would drop the flag.
  if (f.keep_focus) {
    const win = { ...(base.window || {}) };
    win.keep_focus = true;
    // A block with no size is one the launcher replaces at start when the
    // profile claims a screen (fill_window_from_screen) — and the flag would go
    // with it. So give it the very block the launcher would derive: the window
    // fills the work area one pixel short, the viewport 87 px shorter again.
    const scr = base.screen || {};
    if (!(win.outer_width > 0 && win.outer_height > 0) && scr.width > 0 && scr.height > 0) {
      const availW = Math.max(scr.avail_width ?? scr.width, 1);
      const availH = Math.max(scr.avail_height ?? scr.height, 1);
      const outerH = Math.max(availH - 1, 1);
      win.outer_width = availW;
      win.outer_height = outerH;
      win.inner_width = availW;
      win.inner_height = Math.max(outerH - 87, 1);
    }
    base.window = win;
  } else if (base.window) {
    delete base.window.keep_focus;
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
    canvas:       { enabled: f.noise_canvas === "auto",       seed: seedOf("canvas") },
    webgl:        { enabled: f.noise_webgl === "auto",        seed: seedOf("webgl"), intensity: f.noise_webgl === "auto" ? WEBGL_NOISE_INTENSITY : 0 },
    audio:        { enabled: f.noise_audio === "auto",        seed: seedOf("audio") },
    client_rects: { enabled: f.noise_client_rects === "auto", seed: seedOf("client_rects"), max_offset: f.noise_client_rects === "auto" ? CLIENT_RECTS_MAX_OFFSET : 0 },
    sensors:      { enabled: f.noise_sensors === "auto",      seed: seedOf("sensors") },
    fonts:        { enabled: f.noise_fonts === "auto",        seed: seedOf("fonts") },
  };
  base.blocked_ports = [...f.blocked_ports].sort((a, b) => a - b);

  return base;
}

/** Whether a form describes a phone. As profile.rs `claims_mobile`: the user
 *  agent, a `client_hints.mobile` flag, or a Client Hints platform that says
 *  Android. The user agent alone missed an imported handset whose UA was
 *  rewritten but whose hints still name the phone. */
export function claimsMobile(f: { user_agent?: string; client_hints?: { mobile?: boolean; platform?: string } }): boolean {
  if (f.client_hints?.mobile === true) return true;
  if (/^android/i.test(f.client_hints?.platform ?? "")) return true;
  return (f.user_agent || "").toLowerCase().includes("android");
}
