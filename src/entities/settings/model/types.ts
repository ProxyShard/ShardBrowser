export type Settings = {
  browser_path: string | null;
  theme: string;
  geo_checker?: string | null;
  screen_resolution_mode?: string | null;
  api_enabled?: boolean;
  api_port?: number;
  /** Shard Helper: offer to fill forms a generated identity fits. */
  helper_enabled?: boolean;
  /** Which field kinds it reacts to. Empty means all of them. */
  helper_triggers?: string[];
  /** Profile's camera is ShardBrowser's rather than the machine's. */
  camera_enabled?: boolean;
  /** The profile's name in the address bar and the window title. */
  show_profile_name?: boolean;
  /** A password can be set on a profile (#127); off by default. Protected ones stay so when it goes off. */
  profile_passwords?: boolean;
  /** Hide to the tray on close instead of quitting. */
  minimize_to_tray?: boolean;
  /** Appended to every launch, one per line. Applied last, so a repeat wins. */
  extra_args?: string;
  /** Ask the Web Store about linked extensions every 5 h (on when absent). */
  ext_update_check?: boolean;
};

/** Where profiles, user-data, extensions and the trash live. */
export type DataRootInfo = {
  path: string;
  /** False while the data still sits in the config dir. */
  custom: boolean;
  migrating: boolean;
};

/** Progress of a data-root move, as `data-migration` events carry it. */
export type MigrationProgress = {
  phase: "scan" | "copy" | "verify" | "cleanup" | "done";
  done: number;
  total: number;
  percent: number;
  current: string;
};

/** The kinds the engine publishes. `label` is a translation key, not text —
 *  the settings page renders it through t(). */
export const HELPER_KINDS: { value: string; label: string }[] = [
  { value: "first_name",  label: "helperKinds.firstName" },
  { value: "last_name",   label: "helperKinds.lastName" },
  { value: "full_name",   label: "helperKinds.fullName" },
  { value: "email",       label: "helperKinds.email" },
  { value: "username",    label: "helperKinds.username" },
  { value: "phone",       label: "helperKinds.phone" },
  { value: "country",     label: "helperKinds.country" },
  { value: "city",        label: "helperKinds.city" },
  { value: "postal_code", label: "helperKinds.postalCode" },
  { value: "street",      label: "helperKinds.street" },
  { value: "birth_date",  label: "helperKinds.birthDate" },
  { value: "birth_day",   label: "helperKinds.birthDay" },
  { value: "birth_month", label: "helperKinds.birthMonth" },
  { value: "birth_year",  label: "helperKinds.birthYear" },
  { value: "gender",      label: "helperKinds.gender" },
];

export type ApiInfo = {
  enabled: boolean;
  port: number;
  base_url: string;
  token: string;
};
