import { fromStored, toStored, type ProfileForm } from "../../../entities/profile";
import type { FingerprintEntry } from "../../../entities/fingerprint";
import { osOfPlatform, templateSettingsOf, type TemplateSource } from "../../../entities/template";

/// A profile template (#145) and the profile form: what of a form a template
/// keeps, and the form fields its settings set over a fresh fingerprint.

/** Where a template's profiles get their fingerprints: this library one, or any of its OS. */
export type SourceKind = "library" | "os";

const NOISE = ["canvas", "webgl", "audio", "client_rects", "sensors", "fonts"] as const;

/** The form fields a template's settings set (templates.rs `settings_of`'s keys, only
 *  those it has); the rest come with the new profile's own fingerprint. */
export function formOfTemplate(settings: Record<string, any> | null | undefined): Partial<ProfileForm> {
  const s = settings ?? {};
  const f = fromStored(s);
  const out: Partial<ProfileForm> = {};
  const nav = s.navigator ?? {};
  const meta = s._meta ?? {};
  const win = s.window ?? {};
  const noise = s.noise ?? {};
  if ("timezone" in s) out.timezone = f.timezone;
  if ("language" in nav) out.language = f.language;
  if ("ui_language" in s) out.ui_language = f.ui_language;
  if ("webrtc" in s) out.webrtc = f.webrtc;
  if ("do_not_track" in nav) out.do_not_track = f.do_not_track;
  if ("blocked_ports" in s) out.blocked_ports = f.blocked_ports;
  if ("geolocation" in s) Object.assign(out, { geo_mode: f.geo_mode, geo_lat: f.geo_lat, geo_lng: f.geo_lng, geo_accuracy: f.geo_accuracy });
  if ("media_devices" in s) Object.assign(out, { media_audio_in: f.media_audio_in, media_audio_out: f.media_audio_out, media_video_in: f.media_video_in });
  for (const v of NOISE) if (v in noise) out[`noise_${v}`] = f[`noise_${v}`];
  if ("refresh_rate" in (s.screen ?? {})) out.refresh_rate = f.refresh_rate;
  if ("keep_focus" in win) out.keep_focus = f.keep_focus;
  if ("maximized" in win) out.maximized = f.maximized;
  if ("proxy_id" in meta) out.proxy_id = f.proxy_id;
  if ("color" in meta) out.color = f.color;
  if ("extensions" in meta) out.extensions = f.extensions;
  if ("android_media" in meta) out.android_media = f.android_media;
  return out;
}

/** A form's settings as a template keeps them: as the profile would be saved, cut. */
export function settingsOfForm(form: ProfileForm, lib: FingerprintEntry | null): Record<string, any> {
  return templateSettingsOf(toStored(form, lib));
}

/** Where a form's fingerprint came from, as a template remembers it. */
export function sourceOfForm(form: ProfileForm, lib: FingerprintEntry | null, kind: SourceKind, hostOs: string): TemplateSource {
  const os = osOfPlatform(lib?.platform ?? "") || (/android/i.test(form.user_agent) ? "Android" : hostOs);
  return { library: kind === "library" && form.gpu_preset_id ? form.gpu_preset_id : null, os };
}
