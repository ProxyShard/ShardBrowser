import type { ProfileSync } from "../../sync/model/types";

export type NoiseMode = "real" | "auto";
export type WebRtcMode = "auto" | "tcp_only" | "block";
export type GeoMode = "auto" | "manual";
export type UiLanguage = "system" | "profile";

export type ProfileMeta = {
  id: string;
  name: string;
  /** Null while a password locks it (`protected`). */
  notes: string | null;
  /** Null for no proxy, and while a password locks it: unknown then. */
  proxy_id: string | null;
  last_launched_at: string | null;
  created_at: string | null;
  pinned: boolean;
  folder: string;
  /// Cumulative engine uptime in ms across every launch.  Increased when
  /// the engine exits — for the currently-running session add `running[id]`
  /// (Date.now() - sessionStartTs) on top.
  total_runtime_ms: number;
  /// How many times it was started, and the bytes its proxy carried for it,
  /// in all (backend B41). Absent until the backend sends them: Peek's stat
  /// strip shows only the stats it has.
  launches?: number | null;
  traffic_bytes?: number | null;
  /// Icon accent, "#rrggbb"; null = derived from the name, as the browser does.
  color: string | null;
  /// Extension ids from the library, loaded at launch.
  extensions: string[];
  /// Phone/tablet fingerprint, decided in Rust by the core's own rule.
  mobile: boolean;
  /// Present while personal sync is connected (entities/sync).
  sync?: ProfileSync;
  /** Which space the row is from (local/profile.rs `Origin`). */
  origin?: "personal" | "team";
  /** A team row's rights by the permission held; absent in the personal space, where all are given. */
  rights?: ProfileRights;
  /** A team row open on some device now. */
  open_on?: OpenOn;
  /** A team row's tag ids, from its card; personal rows take theirs from tag_list. */
  tags?: string[];
  /** Sealed by its own password (#127): notes and proxy are null, `mobile` false and
   *  `extensions` empty until it is unlocked. Absent from builds before it. */
  protected?: boolean;
};

/** What this member may do with one team profile (perm.rs `Rights`). */
export type ProfileRights = { see: boolean; start: boolean; edit: boolean; delete: boolean; cookies: boolean; take_out: boolean };
export type ProfileRight = keyof ProfileRights;

/** Who has a team profile open, and where (local/profile.rs `OpenOn`); `since`/`until` in unix seconds. */
export type OpenOn = { device_name: string; member_name: string; this_device: boolean; since: number; until: number };

export type ProfileForm = {
  id: string;
  /** The file's version when the editor opened it; a save carries it back. */
  rev: number;
  name: string;
  notes: string;
  proxy_id: string | null;
  /// "" = derive from the name, which is what the browser does on its own.
  color: string;
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
  /// The operator picked the resolution instead of taking the template's.
  /// Saved to `_meta`; the launcher keeps the global screen setting out of a
  /// launch that has one.
  screen_custom: boolean;
  /// A new Windows/Linux profile's default: this computer's screen, in
  /// screen_w/h. Form state only; saved as that size, never as a mark.
  screen_host: boolean;
  /// This computer's pixel ratio, saved with `screen_host`; 0 = the template's.
  screen_dpr: number;
  /// Report the window as focused even while another one is in front, so
  /// `document.hasFocus()` stays true. Stored as `window.keep_focus`; off by
  /// default, because a window that never loses focus is unusual in itself.
  keep_focus: boolean;
  /// Present the window as maximized within the profile's screen (#140).
  /// Stored as `window.maximized`; off by default.
  maximized: boolean;
  /// Cookies file picked in the editor, imported once the profile exists.
  /// Editor state, not part of the fingerprint — `toStored` does not write it.
  cookies_file: string;

  gpu_preset_id: string;
  /** Report this computer's own graphics card instead of the template's.
   *  Offered only while the profile's OS is this computer's. */
  real_gpu: boolean;
  user_agent: string;
  hardware_concurrency: number;
  device_memory: number;
  /// Sec-CH-UA-Platform-Version override; empty = use donor preset's value.
  platform_version: string;

  timezone: string;
  language: string;
  /// The browser's own menus: this computer's language, or the profile's.
  /// Stored as top-level `ui_language: "profile"` only when chosen.
  ui_language: UiLanguage;

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
