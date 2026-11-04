import { invoke } from "@tauri-apps/api/core";
import type { Settings, ApiInfo, DataRootInfo } from "./types";
import { useProfilePasswords } from "./profilePasswords";

/** The settings file, minus the API signing key. The form must never hold
 *  it: a Save would write an old key back after Regenerate token, and a
 *  leaked token would work again after a restart (SEC-01, backend B3). */
export const settingsGet = async (): Promise<Settings> => {
  const raw = await invoke<Settings & { api_secret?: string }>("settings_get");
  delete raw.api_secret;
  return raw;
};
/** Set once a save from this window is on disk: a read after that may show
 *  the change, not what runs. A failed save changes nothing, so it does not
 *  count (Pair 34 ruling, M3); one still on its way makes a read unsure. */
let sentSave = false;
let saving = 0;
export const settingsSave = (value: Settings) => {
  saving += 1;
  return invoke("settings_save", { value })
    .then((r) => { sentSave = true; useProfilePasswords.setState({ on: value.profile_passwords === true }); return r; })
    .finally(() => { saving -= 1; });
};
/** Why settings.json could not be read, or null when it reads fine. */
export const settingsLoadError = () => invoke<string | null>("settings_load_error");
/** The API server as this window first read it. Saving does not restart the
 *  server, and `api_info` reports the saved settings, so this is what runs
 *  until the app restarts (O6: a changed switch or port says so). Only a read
 *  asked and answered before any save counts: one after a change (a failed
 *  first read, then Retry) or racing one would take the change for the
 *  server and hide the note (Pair 34 review). */
let serverAtStart: Pick<ApiInfo, "enabled" | "port"> | null = null;
export const apiServerAtStart = () => serverAtStart;
export const apiInfo = () => {
  const clean = () => !sentSave && saving === 0;
  const beforeAnySave = clean();
  return invoke<ApiInfo>("api_info").then((a) => {
    if (beforeAnySave && clean()) serverAtStart ??= { enabled: a.enabled, port: a.port };
    return a;
  });
};
export const apiRegenerateToken = () => invoke<ApiInfo>("api_regenerate_token");
export const mcpDownload = (dir: string) => invoke<string>("mcp_download", { dir });

export const dataRootGet = () => invoke<DataRootInfo>("data_root_get");
/** Moves the data; progress arrives as `data-migration` events. */
export const dataRootMigrate = (path: string) => invoke<number>("data_root_migrate", { path });
/** What Delete all data would move out of ShardBrowser, counted (reset.rs). Reads only. */
export type ResetPlan = {
  /** Handed back to `appReset`: the reset refuses when the disk no longer matches it. */
  id: string;
  /** Where the old data will be kept: one folder, or two when the data folder is on another disk. */
  backups: string[];
  counts: { profiles: number; proxies: number; fingerprints: number; extensions: number; bookmarks: number; projects: number };
  total_bytes: number;
  entries: string[];
};
export const appResetPlan = () => invoke<ResetPlan>("app_reset_plan");
/** Settings > Storage > Delete all data, for the plan shown: stops every
 *  profile and run and moves the user's data into backup folders (nothing
 *  is deleted; the browser engine and its fingerprint library stay). Answers
 *  with the backups; the app then waits for `appRestart`. */
export const appReset = (planId: string) => invoke<{ backups: string[] }>("app_reset", { planId });
/** The window's own storage, written into a backup the reset just made
 *  (`webview-storage.json`): it is cleared for the fresh start next. */
export const appResetKeepStorage = (backup: string, items: Record<string, string>) =>
  invoke<void>("app_reset_keep_storage", { backup, items });
/** A Delete all data that did not finish (a crash, a forced quit), or null. */
export type ResetPending = {
  /** The journal file (`reset-in-progress.json`), always named. */
  journal: string;
  backups: string[];
  moved: number;
  total: number;
  /** Folders out of reach (a disk not connected): Undo and Finish refuse until they are back. */
  missing: string[];
  /** The journal can't be read or trusted: only setting it aside is offered. */
  error: string | null;
};
export const appResetPending = () => invoke<ResetPending | null>("app_reset_pending");
/** Puts back everything the unfinished reset had moved. */
export const appResetUndo = () => invoke<void>("app_reset_undo");
/** Makes the moves the unfinished reset had left. */
export const appResetFinish = () => invoke<{ backups: string[] }>("app_reset_finish");
/** Renames the unfinished reset's journal aside (never deletes it) and unlocks the app. */
export const appResetSetAside = () => invoke<void>("app_reset_set_aside");
/** The newest backup Delete all data left, or null. */
export const appResetBackup = () => invoke<string | null>("app_reset_backup");
/** Starts the app again (after Delete all data). The browser preview's mock answers instead. */
export const appRestart = () => invoke<void>("app_restart");
