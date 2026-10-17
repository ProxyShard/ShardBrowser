export type NoiseMode = "real" | "auto";
export type WebRtcMode = "auto" | "tcp_only" | "block";
export type GeoMode = "auto" | "manual";

export type ProfileMeta = {
  id: string;
  name: string;
  notes: string;
  proxy_id: string | null;
  last_launched_at: string | null;
  created_at: string | null;
  pinned: boolean;
  folder: string;
  /// Cumulative engine uptime in ms across every launch.  Increased when
  /// the engine exits — for the currently-running session add `running[id]`
  /// (Date.now() - sessionStartTs) on top.
  total_runtime_ms: number;
  /// Icon accent, "#rrggbb"; null = derived from the name, as the browser does.
  color: string | null;
  /// Extension ids from the library, loaded at launch.
  extensions: string[];
  /// Phone/tablet fingerprint, decided in Rust by the core's own rule.
  mobile: boolean;
};

export type ProfileForm = {
  id: string;
  /** The file's version when the editor opened it; a save carries it back. */
  rev: number;
  name: string;
  notes: string;
  proxy_id: string | null;
  /// "" = derive from the name, which is what the browser does on its own.
  color: string;
  /// Normalized square PNG, base64 without a data-URL prefix; empty = standard icon.
  icon_png: string;
  /// Extension ids from the library.
  extensions: string[];
  /// Answer media questions the Android way. Phone profiles only.
  android_media: boolean;
  /// How often the claimed display refreshes, in Hz. A page reads it by
  /// timing requestAnimationFrame; the browser can only slow frames down, so
  /// a claim above this machine's own panel quietly runs at the panel's rate.
  refresh_rate: number;
  /// Claimed screen, `0` = whatever the fingerprint template says.
  screen_w: number;
  screen_h: number;
  /// Cookies file picked in the editor, imported once the profile exists.
  /// Editor state, not part of the fingerprint — `toStored` does not write it.
  cookies_file: string;

  gpu_preset_id: string;
  user_agent: string;
  hardware_concurrency: number;
  device_memory: number;
  /// Sec-CH-UA-Platform-Version override; empty = use donor preset's value.
  platform_version: string;

  timezone: string;
  language: string;

  webrtc: WebRtcMode;
  do_not_track: boolean;

  noise_canvas: NoiseMode;
  noise_webgl: NoiseMode;
  noise_audio: NoiseMode;
  noise_client_rects: NoiseMode;
  noise_sensors: NoiseMode;
  /// Fonts: "real" passes host fonts through; "auto" hides a ~3% per-profile subset.
  noise_fonts: NoiseMode;
  /// TCP ports the browser refuses to connect to (RDP/VNC/TeamViewer/Squid).
  blocked_ports: number[];

  geo_mode: GeoMode;
  geo_lat: number;
  geo_lng: number;
  geo_accuracy: number;

  media_audio_in: number;
  media_audio_out: number;
  media_video_in: number;
};
