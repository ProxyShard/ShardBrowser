import { invoke } from "../../../shared/api/invoke";
import type { ProfileMeta } from "./types";

export const profileList = () => invoke<ProfileMeta[]>("profile_list");
export const profileGet = (id: string) => invoke<any>("profile_get", { id });
/** `password` saves a protected profile whole: its fingerprint and proxy are sealed again. */
export const profileSave = (payload: any, password?: string) =>
  invoke<ProfileMeta>("profile_save", password === undefined ? { payload } : { payload, password });
export const profileDelete = (id: string) => invoke("profile_delete", { id });
export const profileClone = (id: string) => invoke<ProfileMeta>("profile_clone", { id });
export const profileSetPin = (id: string, pinned: boolean) => invoke("profile_set_pin", { id, pinned });
export const profileSetFolder = (id: string, folder: string) => invoke("profile_set_folder", { id, folder });
export const profileBindProxy = (profileId: string, proxyId: string | null) => invoke("profile_bind_proxy", { profileId, proxyId });
export const profileImport = (payloads: any[]) => invoke<number>("profile_import", { payloads });
/** Seals the profile with its own password (#127); answers its row, now `protected`. */
export const profileProtect = (id: string, password: string) => invoke<ProfileMeta>("profile_protect", { id, password });
/** Opens the seal for good; answers its row. */
export const profileUnprotect = (id: string, password: string) => invoke<ProfileMeta>("profile_unprotect", { id, password });
export const profileChangePassword = (id: string, oldPassword: string, newPassword: string) =>
  invoke<null>("profile_change_password", { id, oldPassword, newPassword });
/** The whole stored profile of a protected one, as `profile_get` gives an open one (`_meta.lock` stays). */
export const profileUnlock = (id: string, password: string) => invoke<any>("profile_unlock", { id, password });
/** The debugging endpoint of a profile started with CDP; absent otherwise. */
export type CdpInfo = { port: number; http_url: string; web_socket_debugger_url: string };
export const processList = () =>
  invoke<{ profile_id: string; pid: number; uptime_ms: number; cdp?: CdpInfo }[]>("process_list");
export const processKill = (profileId: string) => invoke<boolean>("process_kill", { profileId });
/** A protected profile starts only with its password. */
export const launch = (profileId: string, password?: string) =>
  invoke<number>("launch", password === undefined ? { profileId } : { profileId, password });
/** Ends a launch in progress (backend B49). The backend has no such command yet: it rejects, and the caller falls back. */
export const launchCancel = (profileId: string) => invoke<void>("launch_cancel", { profileId });
/** Brings a running profile's browser window to the front, with the keyboard. */
export const profileFocusWindow = (profileId: string) => invoke<void>("profile_focus_window", { profileId });

/** A group of profiles that mirror each other's input. Returns the group name. */
export const syncLaunch = (profileIds: string[], group?: string) =>
  invoke<string>("sync_launch", { profileIds, group });

export type SyncMember = { profile: string; excluded: boolean; driving: boolean };
export type SyncStatus = { group: string; members: SyncMember[]; paused: boolean };
export type SyncLayout = "row" | "grid" | "cascade";

export const syncStatus = (group: string) => invoke<SyncStatus>("sync_status", { group });
export const syncSetPaused = (group: string, paused: boolean) =>
  invoke<void>("sync_set_paused", { group, paused });
export const syncArrange = (group: string, layout: SyncLayout) =>
  invoke<void>("sync_arrange", { group, layout });
export const syncStop = (group: string) => invoke<void>("sync_stop", { group });
export const syncSetExcluded = (group: string, profile: string, excluded: boolean) =>
  invoke<void>("sync_set_excluded", { group, profile, excluded });
export const syncClosePanel = () => invoke<void>("sync_close_panel");

export type HelperField = { kind: string; select: boolean; x: number; y: number };
export type HelperReport = { fields: HelperField[] } | null;

export const helperProfiles = () => invoke<string[]>("helper_profiles");
export const helperFields = (profile: string) => invoke<HelperReport>("helper_fields", { profile });
/** Returns how many windows were told to fill — the whole group, when in one. */
export const helperFill = (profile: string) => invoke<number>("helper_fill", { profile });
export const helperShow = (profile: string) => invoke<void>("helper_show", { profile });
export const helperClose = () => invoke<void>("helper_close");
/** The operator closed the panel — a refusal about this page only. */
export const helperDismiss = (profile: string) => invoke<void>("helper_dismiss", { profile });
/** The kept entry and the look follow the name. */
export const folderRename = (oldName: string, newName: string) => invoke<number>("folder_rename", { old: oldName, new: newName });
/** The kept entry and the look go with the folder. */
export const folderDelete = (folder: string, deleteProfiles: boolean) => invoke<number>("folder_delete", { folder, deleteProfiles });

/** A folder's color (a palette id) and icon, as the backend keeps them (folders.rs). */
export type StoredFolderMeta = { color: string | null; icon: string };
/** Folders kept while empty, and each folder's look by name. */
export type FolderState = { registry: string[]; meta: Record<string, StoredFolderMeta> };
export const folderList = () => invoke<FolderState>("folder_list");
/** Keeps the folder while no profile is in it. */
export const folderAdd = (name: string) => invoke<null>("folder_add", { name });
export const folderMetaSet = (name: string, meta: StoredFolderMeta) => invoke<null>("folder_meta_set", { name, meta });
/** Hands over the copy an older build kept in this window; false when the backend had folders already. */
export const folderImport = (state: FolderState) => invoke<boolean>("folder_import", { state });
export const cookiesExportToFile = (profileId: string, path: string) => invoke<number>("cookies_export_to_file", { profileId, path });
export const cookiesImport = (profileId: string, cookies: any[]) => invoke<number>("cookies_import", { profileId, cookies });
export const enrichPicksForPreset = (presetId: string) => invoke<{ hardware_concurrency?: number; device_memory?: number; platform_version?: string }>("enrich_picks_for_preset", { presetId });
export const hostPlatform = () => invoke<string>("host_platform");
