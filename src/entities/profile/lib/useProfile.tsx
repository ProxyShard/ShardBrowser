import { create } from "zustand";
import { save as saveDialog } from "@tauri-apps/plugin-dialog";
import { revealItemInDir } from "@tauri-apps/plugin-opener";
import { toast } from "../../../shared/model/toast";
import { announce } from "../../../shared/model/announce";
import { reportLoadError } from "../../../shared/model/loadError";
import { confirmModal } from "../../../shared/lib/confirm";
import { clip, copySecret } from "../../../shared/lib/clipboard";
import { clipboardTooBig, FileTooBig, MB, pickTextFile } from "../../../shared/lib/pickFile";
import { storeBus } from "../../../shared/lib/storeBus";
import { onSpaceMoved, spaceEpoch, withEpoch } from "../../../shared/api/invoke";
import { TEAM_REFUSALS } from "../../../shared/api/teamRefusals";
import { explainError, rawError } from "../../../shared/lib/errors";
import { markArrived as markArrivedIn } from "../../../shared/lib/arrival";
import { eachReporting, type EachResult } from "../../../shared/lib/eachReporting";
import { t } from "../../../shared/i18n";
import { pendingProxyIds, proxyList, type ProxyEntry } from "../../proxy";
import { fingerprintList, type FingerprintEntry } from "../../fingerprint";
import { trashRestore } from "../../trash/model/api";
import type { ProfileMeta, ProfileForm } from "../model/types";
import {
  profileList, profileGet, profileSave, profileDelete, profileClone,
  profileSetPin, profileSetFolder, profileBindProxy, profileImport,
  processList, processKill, launch, launchCancel, syncLaunch, profileUnlock,
  folderAdd, folderDelete, folderRename, cookiesExportToFile, cookiesImport, enrichPicksForPreset,
} from "../model/api";
import { fromStored, toStored } from "../model/form";
import { proxyMissing } from "./listing";
import { forgetChanged, forgetForm, peekForm, rememberForm } from "./formCache";
import {
  addPlaceholder, applyEdits, beginEdit, dropPlaceholder, endEdit, isArriving, isPlaceholder, keepRowKey, localView,
  moveEdits, newPlaceholderId, resolveId, revertOf, sameJson, sameMeta, shareRows, trackCreation, type Edit,
} from "./optimistic";
import { useDirectStart } from "../../../shared/model/directStart";
import { HOST_OS } from "../../../shared/lib/utils";
import { matchesOs, osIdFor } from "../../../shared/constants";
import { useGpuCompat, verdictsLoaded as loadGpuVerdicts } from "../../../shared/model/gpuCompat";
import { loadFolders, lookOf, onRegistry, sendFolders, useFolderMeta } from "./folderMeta";
import type { AssignPair } from "./assignment";
import { useViews } from "./views";
import { setTagAssignGuard, useTags } from "../../tag";
import { adminRefusal, bindRefusal, inFolder, inTeam, teamRefusal } from "../../space";
import { cloneRefusal, isTeamRow, letThrough, lockedRefusal, moveRefusal, profileRefusal, startRefusal } from "./rights";
import { isLockedStub, isProtected, isWrongPassword } from "./lock";
import { askPassword } from "../../../shared/model/passwordAsk";
import { LOCK_REFUSALS } from "../../../shared/api/lockRefusals";
import { loadProfilePasswords } from "../../settings";

/** How long a start or stop may run ahead of the poll before the poll wins. */
const PENDING_MS = 20_000;
/** How long a row says Cancelled after a start was cancelled. */
const CANCELLED_MS = 3000;
/** A Cancel this soon after the start is ignored: Start turns into Cancel in
 *  the same place, so a double-click or a held Enter must not undo the start
 *  it just made. */
const CANCEL_GUARD_MS = 400;
/** When each profile being started began (`performance.now()`, which a test's
 *  frozen clock leaves alone), for that guard. */
const startedAt = new Map<string, number>();
/** The `launch` call of each profile being started, as a promise that settles
 *  once `startStop` has finished with its result: what a cancel waits for. */
const launching = new Map<string, Promise<void>>();
/** The timer that ends each row's Cancelled word, so a second cancel starts its own 3 s. */
const cancelledTimers = new Map<string, ReturnType<typeof setTimeout>>();

type Set_ = (partial: Partial<ProfileStore>) => void;
type Get_ = () => ProfileStore;

/** `id` added to (or taken from) a Set, as a new Set; the same one when nothing changes. */
function withSet(from: Set<string>, id: string, on: boolean): Set<string> {
  if (from.has(id) === on) return from;
  const next = new Set(from);
  if (on) next.add(id); else next.delete(id);
  return next;
}

function markStarted(set: Set_, get: Get_, id: string) {
  set({
    running: { ...get().running, [id]: Date.now() },
    pending: { ...get().pending, [id]: { kind: "start", at: Date.now() } },
    // A new start ends the Cancelled word of the last one.
    cancelled: withSet(get().cancelled, id, false),
    // A new start settles the last failed one (a start from elsewhere, one
    // the store never saw, moves `last_launched_at` past it instead).
    startFailed: without(get().startFailed, [id]),
  });
}

/** `map` without `ids`: the same object when none of them is in it. */
function without<T>(map: Record<string, T>, ids: readonly string[]): Record<string, T> {
  let next = map;
  for (const id of ids) {
    if (!(id in next)) continue;
    if (next === map) next = { ...map };
    delete next[id];
  }
  return next;
}

/// Rows that just arrived (a create, an import, a clone, a restore) are
/// marked for their one slide-in (Live layer, spec 6.2), and unmarked once it
/// has played (shared/lib/arrival, timed from the tokens), so a row the list
/// mounts again later never replays it.
function markArrived(set: Set_, get: Get_, ids: readonly string[]) {
  markArrivedIn(() => get().arrived, (arrived) => set({ arrived }), ids);
}

function markStopped(set: Set_, get: Get_, id: string) {
  const running = { ...get().running };
  delete running[id];
  set({ running, pending: { ...get().pending, [id]: { kind: "stop", at: Date.now() } } });
}

function unmark(set: Set_, get: Get_, id: string) {
  const pending = { ...get().pending };
  delete pending[id];
  set({ pending });
}

export type SaveResult = "ok" | "conflict" | "failed";

/** A part of a create that runs once the profile exists. */
export type CreateStep = "fingerprint" | "folder" | "proxy";
/** What a create did: nothing (`meta` null, with why), or the profile, with
 *  the first part that did not land and why, if one did not. */
export type CreateResult =
  | { meta: null; error: unknown }
  | { meta: ProfileMeta; failed: { step: CreateStep; error: unknown } | null };

/** Same ids with the same start times: nothing for the poll to store. */
function sameRunning(a: Record<string, number>, b: Record<string, number>): boolean {
  const keys = Object.keys(a);
  if (keys.length !== Object.keys(b).length) return false;
  return keys.every((k) => b[k] === a[k]);
}

// The list puts the keyboard back on a profile that Undo restored.
let onRestored: ((ids: string[]) => void) | null = null;
export function setOnRestored(fn: ((ids: string[]) => void) | null) { onRestored = fn; }

// A refused Start offers Assign proxies. The dialog's state lives in
// useProfileUi, which imports this module, so it registers here.
let onAssign: ((ids: string[]) => void) | null = null;
export function setOnAssign(fn: ((ids: string[]) => void) | null) { onAssign = fn; }

// The list (and the View it shows) follows a renamed Folder before the old
// name leaves the registry: once it is gone, the list would fall back to All
// Profiles.
let onFolderRenamed: ((from: string, to: string) => void) | null = null;
export function setOnFolderRenamed(fn: ((from: string, to: string) => void) | null) { onFolderRenamed = fn; }

// A proxy deleted behind an Undo toast is gone for this store until the toast
// goes: the profiles on it read "Proxy missing" meanwhile (ruling 18).
const livingProxies = (list: ProxyEntry[]) => list.filter((p) => !pendingProxyIds.has(p.id));

// Profiles on their way to the Trash: hidden from the list at once (the
// backend archives each one and can take a while), and skipped by a second
// trigger (a double click) while the move runs.
const leaving = new Set<string>();

// Rows moved to the Trash in this session and where they stood: Undo shows
// them again at once, as they were (LQ-03). The newest TRASHED_KEPT only
// (ruling F14): an Undo only ever reaches the latest moves.
const trashedRows = new Map<string, { meta: ProfileMeta; at: number }>();
const TRASHED_KEPT = 500;
function rememberTrashed(id: string, row: { meta: ProfileMeta; at: number }) {
  trashedRows.delete(id);
  trashedRows.set(id, row);
  for (const old of trashedRows.keys()) {
    if (trashedRows.size <= TRASHED_KEPT) break;
    trashedRows.delete(old);
  }
}

/** A backend error as words, unwrapped the one way (`rawError`, which the
 *  toast's detail line starts from too): what a failed start keeps. */
const reason = (e: unknown) => rawError(e) ?? String(e);
/** The team server keeps the folder: profiles deleted from it wait in the team's trash. */
const folderInTrash = (e: unknown) => reason(e).includes(TEAM_REFUSALS.folderInTrash);

// The store's bus listeners are registered once, by the first `init`.
let wired = false;

/// Profile data and every call to the backend. List and editor state (search,
/// Folder, selection, the open editor) live in `useProfileUi`.
export type ProfileStore = {
  status: "idle" | "loading" | "ready" | "error";
  error: string | null;

  profiles: ProfileMeta[];
  proxies: ProxyEntry[];
  /** Proxy id to its last seen IP, while the Proxies store has its tests (filled by src/app/wireStores.ts). */
  proxyIps: Record<string, string>;
  fingerprints: FingerprintEntry[];

  /// Value = epoch ms at which the engine was first observed running. Used both
  /// as a truthy flag (any number = running) and as the anchor for the ticking
  /// uptime display.
  running: Record<string, number>;
  /// Profiles whose `launch()` call is in flight (pre-flight probes can be
  /// slow). They already show as running; Stop waits for the call to return.
  startBusy: Set<string>;
  /// Starts the person cancelled whose `launch` call has not returned yet:
  /// their button says Cancelling and does nothing.
  cancelling: Set<string>;
  /// Starts the person cancelled a moment ago: the row says Cancelled for
  /// 3 s, then its status before.
  cancelled: Set<string>;
  /// Starts and stops the poll has not caught up with yet, with when they began.
  pending: Record<string, { kind: "start" | "stop"; at: number }>;
  /// When a start of a profile last failed in this session (ms epoch), for
  /// its "Failed to start" status (`profileStatus`'s `failedAt`). Gone once
  /// the profile starts again or leaves the list; never persisted.
  startFailed: Record<string, number>;
  /// Why each of those starts failed (the backend's words), for the Failed
  /// today card. Read only where `startFailed` has the profile.
  failReasons: Record<string, string>;
  /// Rows that just arrived (a create, an import, a clone, a restore), with
  /// when (`performance.now()`): each row slides in once while it is here
  /// (Live layer, spec 6.2). Each goes once it has played (`arrivalMs`).
  arrived: Record<string, number>;
  /// Folders kept even while no profile is in them (the backend's
  /// folders.rs; the folders of the profiles add to it).
  folderRegistry: string[];
  /** Group currently being synchronised, or null. */
  syncGroup: string | null;

  init: () => Promise<void>;
  reload: () => Promise<void>;
  startProcessPolling: () => () => void;

  rememberFolder: (f: string) => void;
  /** Takes a deleted Folder out of the registry here: `folder_delete` did in the backend. */
  forgetFolder: (f: string) => void;

  /** A profile as an editor form; Peek's cache keeps it (PERF-14). A protected
   *  one's is refused: it opens only with its password (`unlockForm`). */
  loadForm: (id: string) => Promise<ProfileForm>;
  /** A protected profile as an editor form, opened with its password; never cached. */
  unlockForm: (id: string, password: string) => Promise<ProfileForm>;
  /** Loads the forms of these profiles into Peek's cache, quietly. */
  prefetchForms: (ids: string[]) => void;
  /** Saves the form of an existing profile, quietly: "conflict" when the
   *  profile changed on disk since the form's rev, "failed" for any other
   *  error, which `opts.onFailed` gets. Nothing is shown here. A protected
   *  profile is saved with the `password` its editor was opened with. */
  saveProfile: (draft: ProfileForm, opts?: { onFailed?: (error: unknown) => void; password?: string }) => Promise<SaveResult>;
  /** Creates the profile the profile form describes (Create profile): the
   *  backend creates it from the form, the form's values go over what the
   *  backend rolls for a new profile, then it goes in its Folder, on its
   *  proxy and gets its tags (`newTags` are made now); its row is in the list
   *  on return. Once the backend has it, it is returned whatever fails after:
   *  a retry must never make it twice. Nothing is shown here; the form says it. */
  createProfile: (draft: { form: ProfileForm; folder: string; tagIds: string[]; newTags: string[] }) => Promise<CreateResult>;

  /** Renames a profile; false when it failed (the error is already shown). */
  renameProfile: (id: string, name: string) => Promise<boolean>;
  /** Replaces a profile's notes; false when it failed. */
  setNotes: (id: string, notes: string) => Promise<boolean>;
  /** Binds a proxy, or none for a direct connection; false when it failed. */
  bindProxy: (id: string, proxyId: string | null) => Promise<boolean>;
  /** Rows the backend answered for a change made elsewhere (sync); pending edits stay on top. */
  applyRows: (rows: ProfileMeta[]) => void;

  /** Starts or stops a profile. Resolves to the reason it failed or was
   *  refused (what its toast says), or null: it went ahead, or the person
   *  chose not to. A profile whose proxy is gone is refused, with Assign a
   *  proxy offered. Starting one with no proxy while proxies exist asks
   *  first (O9a), unless `directChecked` says the question already ran for
   *  this start. `quiet` reports nothing: a bulk caller reports the batch
   *  once (IFACE-06). A protected profile's password is asked first, at
   *  every start; one the person did not give goes into `skipped`. */
  startStop: (p: ProfileMeta, opts?: { directChecked?: boolean; quiet?: boolean; skipped?: Set<string> }) => Promise<string | null>;
  /** Cancels a start that is still launching. The backend's `launch_cancel`
   *  (B49) ends the launch; without it (the command is missing, or fails) the
   *  launch is left to return, and whatever it started is stopped then. Either
   *  way no failure is reported, and the row says Cancelled for 3 s. A profile
   *  that is not launching is left alone. `now`: no double-click guard (a
   *  password dialog's Cancel, which is not where Start was). */
  cancelStart: (id: string, opts?: { now?: boolean }) => Promise<void>;
  /** Moves one profile to the Trash, with an Undo toast. */
  remove: (id: string) => Promise<void>;
  /** Copies a profile: the copy shows at once, and its toast offers Open. */
  cloneProfile: (id: string) => Promise<void>;
  togglePin: (p: ProfileMeta) => Promise<void>;
  exportCookies: (p: ProfileMeta) => Promise<void>;
  importCookies: (p: ProfileMeta) => Promise<void>;

  setProfileFolder: (id: string, f: string) => Promise<void>;
  deleteFolder: (f: string) => Promise<void>;
  /** Renames a Folder, its profiles and its look; refuses a name in use.
   *  False when it did not happen (the reason is already shown). */
  renameFolder: (from: string, to: string) => Promise<boolean>;

  /** Starts the profiles one by one; returns the ids that did not start. */
  launchMany: (ids: string[]) => Promise<string[]>;
  /** Launches the profiles as one synchronised group; false when it failed. */
  launchSynced: (ids: string[]) => Promise<boolean>;
  /** Stops the running ones; returns the ids that did not stop. */
  stopMany: (ids: string[]) => Promise<string[]>;
  /** Moves the profiles to the Trash at once and shows one Undo toast for
   *  them; the ids that actually moved (empty when none did), so a caller can
   *  drop just those from the selection. */
  deleteMany: (ids: string[]) => Promise<string[]>;
  /** Brings profiles back from the Trash (Undo): at once, where they were. */
  restoreMany: (ids: string[]) => Promise<void>;
  /** Copies the profiles' JSON to the clipboard. */
  exportMany: (ids: string[]) => Promise<void>;
  /** Moves the profiles into a Folder ("" = out of any), at once; says so
   *  with Undo when rows leave the list (not when `quiet`). False when any failed. */
  moveMany: (ids: string[], folder: string, opts?: { quiet?: boolean }) => Promise<boolean>;
  /** Puts each profile back in the Folder it had (a move's Undo). */
  restoreFolders: (folders: Map<string, string>) => Promise<void>;
  /** One proxy (or No proxy) for the given profiles; a spread goes through
   *  `planAssignment` and `assignPairs`. False when any bind failed. */
  assignProxies: (ids: string[], proxyIds: (string | null)[]) => Promise<boolean>;
  /** Binds each pair of an assignment plan, every row at once, and reports
   *  by toast; the selection is left as it is (R26 a). False when any bind
   *  failed (the error is already shown). */
  assignPairs: (pairs: AssignPair[]) => Promise<boolean>;
  /** Asks first, then gives each idle profile another library fingerprint of
   *  its OS. Returns how many changed. */
  regenerateFingerprints: (ids: string[]) => Promise<number>;
  /** Creates profiles from JSON on the clipboard. */
  importFromClipboard: () => Promise<void>;
};

/** Edits a stored profile field by round-tripping the whole profile JSON, so
 *  every other field stays as it is. Returns the row as the backend saved it. */
async function patchStored(id: string, patch: (stored: { name: string; notes: string }) => void): Promise<ProfileMeta> {
  const stored = await profileGet(id);
  patch(stored);
  return profileSave(stored);
}

const formSaved = new Set<(form: ProfileForm) => void>();
/** Calls `fn` with each form a save stored (Peek's, a card's fix): what the
 *  list row does not carry, such as the timezone, is read from it. */
export function onFormSaved(fn: (form: ProfileForm) => void): () => void {
  formSaved.add(fn);
  return () => formSaved.delete(fn);
}

/** Forms being read ahead for the rows beside Peek's. */
const prefetching = new Set<string>();

/** Lays pending edits and placeholder rows over the list as it stands. */
function showLocal(set: Set_, get: Get_) {
  set({ profiles: shareRows(get().profiles, localView(get().profiles, leaving), sameMeta) });
}

/** Replaces rows with what the backend returned for them; pending edits stay on top. */
function putRows(set: Set_, get: Get_, rows: ProfileMeta[]) {
  if (rows.length === 0) return;
  const byId = new Map(rows.map((r) => [r.id, r]));
  set({ profiles: shareRows(get().profiles, applyEdits(get().profiles.map((p) => byId.get(p.id) ?? p)), sameMeta) });
}

/** A failed edit: its fields go back to what the row had before it, unless a
 *  newer edit of the same field is still on its way (`revertOf`). */
function putBack(set: Set_, get: Get_, edit: Edit) {
  const row = get().profiles.find((p) => p.id === edit.id);
  const revert = revertOf(edit, row);
  if (row && Object.keys(revert).length > 0) set({ profiles: get().profiles.map((p) => (p === row ? { ...p, ...revert } : p)) });
}

const isMeta = (v: unknown): v is ProfileMeta =>
  !!v && typeof v === "object" && "id" in v && "name" in v && "folder" in v;

/// One row changed at once (PERF-13): the edit shows now; the call runs with
/// the row's backend id (a row still being created waits for it); a failure
/// puts back what this edit set, unless a later edit changed it, and says
/// what failed. `call` may return the row as the backend now has it. True
/// when it went through.
async function patchRow(
  set: Set_, get: Get_, id: string, patch: Partial<ProfileMeta>,
  call: (realId: string) => Promise<unknown>,
  failed: (name: string) => string,
  need?: (p: ProfileMeta) => string | null,
): Promise<boolean> {
  const before = get().profiles.find((p) => p.id === id);
  if (!before) return false;
  // A team's rights refuse it: nothing shows, so nothing goes back (no flash).
  const refused = need?.(before);
  if (refused) { toast.fail({ title: failed(before.name), detail: refused }); return false; }
  const edit = beginEdit(id, patch, before);
  showLocal(set, get);
  try {
    const real = await resolveId(id);
    // Its create failed: the row is already gone.
    if (!real) { endEdit(edit); return false; }
    const answer = await call(real);
    endEdit(edit);
    if (isMeta(answer)) putRows(set, get, [answer]);
    return true;
  } catch (e) {
    endEdit(edit);
    putBack(set, get, edit);
    toast.fail({ title: failed(before.name), detail: e });
    return false;
  }
}

/// Several rows changed at once, each by its own call (a move, an assign):
/// every edit shows now (`shown` runs right after); the calls run one by one
/// past failures, and a row whose call failed gets back what it had. The
/// caller words the outcome.
async function patchRows(
  set: Set_, get: Get_, ids: string[],
  // A Partial<ProfileMeta>, spelled so check:i18n does not take "=> Partial" for JSX text.
  patchOf: (p: ProfileMeta, k: number) => Edit["patch"],
  call: (realId: string, k: number) => Promise<unknown>,
  shown?: () => void,
): Promise<EachResult> {
  const before = new Map(get().profiles.map((p) => [p.id, p]));
  const edits = new Map<string, Edit>();
  ids.forEach((id, k) => {
    const p = before.get(id);
    if (p) edits.set(id, beginEdit(id, patchOf(p, k), p));
  });
  showLocal(set, get);
  shown?.();
  // A row whose create failed is gone: neither done nor failed.
  const gone = new Set<string>();
  const result = await eachReporting(ids, async (id, k) => {
    const real = await resolveId(edits.get(id)?.id ?? id);
    if (real) await call(real, k);
    else gone.add(id);
  });
  if (gone.size > 0) result.done = result.done.filter((id) => !gone.has(id));
  const failed = new Set(result.failed);
  for (const [id, edit] of edits) {
    endEdit(edit);
    if (failed.has(id)) putBack(set, get, edit);
  }
  return result;
}

/// The backend ids of `ids`: a row the backend does not have yet (being
/// created, or put back by Undo) waits for it (`resolveId`, the one path every
/// store action takes); a row whose create or restore failed is left out (its
/// failure is already shown).
async function realIds(ids: string[]): Promise<string[]> {
  if (!ids.some(isArriving)) return ids;
  return (await Promise.all(ids.map(resolveId))).filter((id): id is string => id !== null);
}

/// The rows a team's rights let through, the others named in one toast before
/// anything shows: a write they refuse is never shown, then taken back.
function allowed(get: Get_, ids: string[], why: (p: ProfileMeta, many: boolean) => string | null, oneTitle: (name: string) => string): string[] {
  const many = ids.length > 1;
  const { ok, refused, reason } = letThrough(get().profiles, ids, (p) => why(p, many));
  if (refused.length > 0) {
    const name = get().profiles.find((p) => p.id === refused[0])?.name ?? "";
    toast.fail({ title: refused.length === 1 ? oneTitle(name) : t("rights.skipped", { n: refused.length }), detail: reason });
  }
  return ok;
}

/** Rows the backend just returned (a create, a restore), put in the list
 *  without reading it again. */
function insertRows(set: Set_, get: Get_, rows: ProfileMeta[], at: number) {
  const have = new Set(get().profiles.map((p) => p.id));
  // A row already on its way to the Trash again stays out.
  const add = rows.filter((r) => !have.has(r.id) && !leaving.has(r.id));
  if (add.length === 0) return;
  const next = [...get().profiles];
  next.splice(Math.min(at, next.length), 0, ...add);
  set({ profiles: applyEdits(next) });
}

/** Rows back from the Trash, as the backend returned them, in one update:
 *  each replaces its placeholder (or, not shown yet, goes in where a new
 *  profile goes, in order, and slides in); `lost`, whose restore failed,
 *  leave the list. */
function putRestored(set: Set_, get: Get_, back: ProfileMeta[], lost: string[]) {
  for (const m of back) dropPlaceholder(m.id);
  for (const id of lost) dropPlaceholder(id);
  if (back.length === 0 && lost.length === 0) return;
  const gone = new Set(lost);
  let list = gone.size > 0 ? get().profiles.filter((p) => !gone.has(p.id)) : get().profiles;
  const have = new Set(list.map((p) => p.id));
  const shown = new Map(back.filter((m) => have.has(m.id)).map((m) => [m.id, m]));
  // A row already on its way to the Trash again stays out.
  const fresh = back.filter((m) => !have.has(m.id) && !leaving.has(m.id));
  if (shown.size > 0) list = list.map((p) => shown.get(p.id) ?? p);
  if (fresh.length > 0) {
    const at = list.filter((p) => p.pinned).length;
    list = [...list];
    list.splice(Math.min(at, list.length), 0, ...fresh);
  }
  set({ profiles: shareRows(get().profiles, applyEdits(list), sameMeta) });
  if (fresh.length > 0) markArrived(set, get, fresh.map((m) => m.id));
}

/** Where the backend lists a new profile: newest first, after the pinned rows. */
const newRowIndex = (get: Get_) => get().profiles.filter((p) => p.pinned).length;

/** Moves profiles to the Trash the way Move to Trash does: hidden at once, a
 *  running one stopped first (the Trash archives its folder), one by one past
 *  failures (shown). No toast: the caller says what happened. */
async function moveToTrash(set: Set_, get: Get_, ids: string[]): Promise<{ moved: string[]; names: Map<string, string> }> {
  const team = inTeam();
  const names = new Map(get().profiles.map((p) => [p.id, p.name]));
  const todo = ids.filter((id) => !leaving.has(id));
  if (todo.length === 0) return { moved: [], names };
  const going = new Set(todo);
  get().profiles.forEach((p, at) => { if (going.has(p.id)) rememberTrashed(p.id, { meta: p, at }); });
  for (const id of todo) leaving.add(id);
  set({ profiles: get().profiles.filter((p) => !leaving.has(p.id)), startFailed: without(get().startFailed, todo) });
  const moved: string[] = [];
  // A row still being created leaves under its real id (`settle` moves it in `leaving`).
  const resolved: string[] = [];
  try {
    const { failed, firstError } = await eachReporting(todo, async (id) => {
      const real = await resolveId(id);
      // Its create failed: nothing to move.
      if (!real) { trashedRows.delete(id); return; }
      resolved.push(real);
      if (real !== id) {
        names.set(real, names.get(id) ?? "");
        const r = trashedRows.get(id);
        trashedRows.delete(id);
        if (r) rememberTrashed(real, { meta: { ...r.meta, id: real }, at: r.at });
      }
      // The Trash archives the browser's folder and then wipes it: a running
      // browser must not be writing to it.
      if (get().running[real]) {
        markStopped(set, get, real);
        await processKill(real);
      }
      await profileDelete(real);
      moved.push(real);
    });
    if (failed.length > 0) {
      toast.fail({
        title: todo.length === 1
          ? (team ? t("useProfile.teamDeleteFailed", { name: names.get(failed[0]) ?? "" }) : t("useProfile.trashFailed", { name: names.get(failed[0]) ?? "" }))
          : (team ? t("useProfile.teamDeleteSomeFailed", { failed: failed.length, total: todo.length }) : t("useProfile.trashSomeFailed", { failed: failed.length, total: todo.length })),
        detail: firstError,
      });
    }
  } finally {
    for (const id of todo) leaving.delete(id);
    for (const id of resolved) leaving.delete(id);
  }
  // The Trash's copy no longer holds everything it has.
  if (moved.length > 0) storeBus.emit("trash", "profiles");
  await get().reload();
  return { moved, names };
}

/** O9a: before profiles without a proxy start while proxies exist, ask once.
 *  One profile: Cancel, Assign a proxy…, Start anyway. Several: Cancel, Skip
 *  those, Start all anyway. "start" when there is nothing to ask. Exported
 *  (fix round 2) so LiveView's Start and Project Run can reuse this exact
 *  question — call it with `useProfile.getState` — instead of a third copy.
 *  `allowSkip: false` (fix round 3, Project Run only: a run can't start a
 *  subset of its profiles) drops Skip even with several profiles, offering
 *  Cancel / Assign a proxy… / Start anyway instead. Every other caller keeps
 *  the default (`true`). */
export async function confirmDirect(
  get: Get_,
  targets: ProfileMeta[],
  opts?: { allowSkip?: boolean },
): Promise<"start" | "skip" | "assign" | "cancel"> {
  // A protected row's proxy is unknown while it is locked: never asked about.
  const direct = targets.filter((p) => !p.proxy_id && !isProtected(p));
  if (direct.length === 0 || get().proxies.length === 0 || !useDirectStart.getState().ask) return "start";
  const allowSkip = opts?.allowSkip ?? true;
  const one = targets.length === 1;
  const offerAssign = one || !allowSkip;
  const choice = await confirmModal({
    title: one
      ? t("useProfile.directTitle", { name: targets[0].name })
      : direct.length === 1 ? t("useProfile.directTitleManyOne") : t("useProfile.directTitleMany", { n: direct.length }),
    message: one ? t("useProfile.directMessage") : t("useProfile.directMessageMany"),
    buttons: offerAssign
      ? [
          { label: t("confirm.cancel"), value: "cancel" },
          { label: t("useProfile.directAssign"), value: "assign" },
          { label: one ? t("useProfile.directStart") : t("useProfile.directStartAll"), value: "start", primary: true },
        ]
      : [
          { label: t("confirm.cancel"), value: "cancel" },
          { label: t("useProfile.directSkip"), value: "skip" },
          { label: t("useProfile.directStartAll"), value: "start", primary: true },
        ],
  });
  return choice === "start" || choice === "skip" || choice === "assign" ? choice : "cancel";
}

/// What the store asks of the list's UI state. `useProfileUi` imports this
/// module, so it registers these rather than being imported (as
/// `setOnFolderRenamed` and `setOnAssign` do).
export type UiBridge = {
  /** Rows moved to another Folder (or out of one): say where, with Undo, when any left the list. */
  moved?: (m: { ids: string[]; names: Map<string, string>; from: Map<string, string>; to: string; undo: () => void }) => void;
  /** A row being created got its real id: everything that names it follows (Task 6). */
  idChanged?: (from: string, to: string) => void;
  /** A row was just created: bring it into view, or say that the list hides it (Task 6). */
  created?: (id: string) => void;
  /** Profiles that just arrived (an import): select them and show the first (Task 6). */
  arrived?: (ids: string[]) => void;
  /** Open a profile in Peek with its name ready to type (Task 7). */
  open?: (id: string) => void;
  /** Select exactly these profiles: a bulk Start's "Select failed" (C25). */
  select?: (ids: string[]) => void | boolean;
  /** Take a profile another computer holds, after asking, and start it (personal sync). */
  takeLease?: (p: ProfileMeta) => void;
};
const uiBridge: UiBridge = {};
export function setUiBridge(part: UiBridge) { Object.assign(uiBridge, part); }

/** The other computer that holds a synced profile, or undefined. */
const leaseOf = (p: ProfileMeta): string | undefined => (p.sync?.on ? p.sync.open_on : undefined);

/** Says, in one toast, which of these another computer holds; answers their ids. */
function sayLeased(list: ProfileMeta[]): string[] {
  const held = list.filter((p) => leaseOf(p));
  if (held.length === 1) {
    const p = held[0];
    toast.fail({
      title: t("useProfile.startFailed", { name: p.name }), detail: t("errors.openElsewhere", { device: leaseOf(p)! }), subject: p.name,
      action: uiBridge.takeLease ? { label: t("profileSync.takeAction"), run: () => uiBridge.takeLease?.(p) } : undefined,
    });
  } else if (held.length > 1) {
    toast.err(t("profileSync.leasedMany", { n: held.length }), { detail: held.map((p) => `${p.name}: ${t("status.openOn", { device: leaseOf(p)! })}`).join("\n") });
  }
  return held.map((p) => p.id);
}

/// A placeholder's calls are done: its row becomes the backend's row, under
/// its real id, and everything that named it follows (Peek, the active row,
/// the selection, a pending edit, a move to the Trash).
function settle(set: Set_, get: Get_, tempId: string, meta: ProfileMeta) {
  dropPlaceholder(tempId);
  keepRowKey(tempId, meta.id);
  moveEdits(tempId, meta.id);
  if (leaving.has(tempId)) { leaving.delete(tempId); leaving.add(meta.id); }
  // The UI state first, then the row: Peek never sees its profile missing.
  uiBridge.idChanged?.(tempId, meta.id);
  // A reload that landed meanwhile may list the real row already: one row
  // stays, where the placeholder stood. A slide-in under way goes on.
  const rows = get().profiles.filter((p) => p.id !== meta.id).map((p) => (p.id === tempId ? meta : p));
  const arrived = get().arrived;
  set({
    profiles: shareRows(get().profiles, applyEdits(rows), sameMeta),
    ...(tempId in arrived ? { arrived: { ...without(arrived, [tempId]), [meta.id]: arrived[tempId] } } : {}),
  });
}

export const useProfile = create<ProfileStore>((set, get) => ({
  status: "idle",
  error: null,

  profiles: new Array<ProfileMeta>(),
  proxies: new Array<ProxyEntry>(),
  proxyIps: {},
  fingerprints: new Array<FingerprintEntry>(),

  running: {},
  startBusy: new Set<string>(),
  cancelling: new Set<string>(),
  cancelled: new Set<string>(),
  pending: {},
  startFailed: {},
  failReasons: {},
  arrived: {},
  folderRegistry: [],
  syncGroup: null,

  init: async () => {
    if (get().status === "loading" || get().status === "ready") return;
    set({ status: "loading" });
    // Whether a password may be set, for the rows' menus.
    loadProfilePasswords();
    // Listeners first, and once: a library that lands while the list loads
    // (the first engine download) must not be missed; a retried load must
    // not register them twice.
    if (!wired) {
      wired = true;
      // A proxy added on the Proxies page has to reach the editor's select, and
      // a proxy bound there — by the distribute dialog — has to reach the table.
      // A proxy saved from a profile's select has already been read back here.
      storeBus.on("proxies", (from) => {
        if (from === "profiles") return;
        void proxyList().then((proxies) => set({ proxies: shareRows(get().proxies, livingProxies(proxies), sameJson) })).catch(() => {});
      });
      // Its own changes are already on screen: only another store's reloads (PERF-13).
      storeBus.on("profiles", (from) => { if (from !== "profiles") void get().reload(); });
      // Peek tells a profile's template from one no longer in the library.
      storeBus.on("fingerprints", () => {
        void fingerprintList().then((fingerprints) => set({ fingerprints })).catch(() => {});
      });
    }
    try {
      // Tags and folders arrive with the list; their own failure is shown, never thrown.
      const [profiles, proxies, fingerprints] = await Promise.all([
        profileList(), proxyList(), fingerprintList(), useTags.getState().load(), loadFolders(),
      ]);
      set({ profiles, proxies: livingProxies(proxies), fingerprints, status: "ready" });
    } catch (e) {
      set({ status: "error", error: String(e) });
      reportLoadError("profiles", t("useProfile.loadFailed"), e);
    }
  },

  reload: async () => {
    try {
      const [profiles, proxies] = await Promise.all([profileList(), proxyList()]);
      // Pending edits, rows on their way in and rows on their way to the
      // Trash stay as the user sees them; a row that did not change keeps its
      // object, so its memoized row does not render again (PERF-13).
      const nextProfiles = shareRows(get().profiles, localView(profiles, leaving), sameMeta);
      const nextProxies = shareRows(get().proxies, livingProxies(proxies), sameJson);
      if (nextProfiles !== get().profiles || nextProxies !== get().proxies) set({ profiles: nextProfiles, proxies: nextProxies });
    } catch (e) { toast.fail({ title: t("useProfile.refreshFailed"), detail: e }); }
  },

  // 2s poll for real child status. Uptime is anchored to the moment the engine
  // actually started (now - uptime_ms), preserved across polls so the displayed
  // clock doesn't jitter. A start or stop the UI already shows (`pending`) wins
  // over the poll until the poll agrees, or for PENDING_MS at most. When a
  // profile goes from running to stopped, the backend has just bumped its
  // persisted total_runtime_ms — re-fetch so totals are current.
  startProcessPolling: () => {
    let cancelled = false;
    const tick = async () => {
      try {
        const list = await processList();
        if (cancelled) return;
        const now = Date.now();
        const prev = get().running;
        const prevPending = get().pending;
        const seen = new Set(list.map((r) => r.profile_id));
        const next: Record<string, number> = {};
        for (const r of list) {
          next[r.profile_id] = prev[r.profile_id] ?? (now - r.uptime_ms);
        }
        // Copied only when an entry goes, so an unchanged poll keeps the object.
        let pending = prevPending;
        // A stop the poll now confirms is an exit too: totals and "last run" moved.
        let stopConfirmed = false;
        for (const [id, op] of Object.entries(prevPending)) {
          const agreed = op.kind === "start" ? seen.has(id) : !seen.has(id);
          if (agreed && op.kind === "stop") stopConfirmed = true;
          if (agreed || now - op.at > PENDING_MS) {
            if (pending === prevPending) pending = { ...prevPending };
            delete pending[id];
            continue;
          }
          if (op.kind === "start") next[id] = prev[id] ?? op.at;
          else delete next[id];
        }
        const justExited = Object.keys(prev).some((id) => !(id in next) && !(id in prevPending));
        // A profile seen running has started, whoever started it (the API,
        // another window, a launch whose own call failed after the spawn):
        // a failed start of it is settled.
        const startFailed = without(get().startFailed, [...seen]);
        // Nothing changed: store nothing, so nothing that reads the maps
        // re-renders or re-filters every 2 s (PERF-01).
        const running = sameRunning(prev, next) ? prev : next;
        if (running !== prev || pending !== prevPending || startFailed !== get().startFailed) set({ running, pending, startFailed });
        if (justExited || stopConfirmed) get().reload();
      } catch {}
    };
    tick();
    const handle = setInterval(tick, 2000);
    return () => { cancelled = true; clearInterval(handle); };
  },

  rememberFolder: (f) => {
    if (get().folderRegistry.includes(f)) return;
    set({ folderRegistry: [...get().folderRegistry, f] });
    sendFolders(() => folderAdd(f));
  },
  forgetFolder: (f) => {
    set({ folderRegistry: get().folderRegistry.filter((x) => x !== f) });
  },

  loadForm: async (id) => {
    const stored = await profileGet(id);
    // A protected profile's stub has no fingerprint: no form is made of it.
    if (isLockedStub(stored)) throw LOCK_REFUSALS.needsPassword;
    const form = fromStored(stored);
    rememberForm(form);
    return form;
  },
  unlockForm: async (id, password) => fromStored(await profileUnlock(id, password)),

  // A read still on its way is not asked for again (Pair 18 review, M11).
  // Peek's own read on arrival still goes: it must follow any save of that
  // profile, which a read started earlier may not.
  prefetchForms: (ids) => {
    const locked = new Set(get().profiles.filter(isProtected).map((p) => p.id));
    for (const id of ids) {
      // A protected one opens only with its password: never read behind the person's back.
      if (isPlaceholder(id) || locked.has(id) || peekForm(id) || prefetching.has(id)) continue;
      prefetching.add(id);
      void get().loadForm(id).catch(() => {}).finally(() => prefetching.delete(id));
    }
  },

  saveProfile: async (draft, opts) => {
    const { fingerprints } = get();
    const before = get().profiles.find((p) => p.id === draft.id);
    const refused = before ? profileRefusal(before, "edit") : null;
    if (refused) { opts?.onFailed?.(refused); return "failed"; }
    const locked = isProtected(before);
    if (locked && opts?.password === undefined) { opts?.onFailed?.(LOCK_REFUSALS.needsPassword); return "failed"; }
    // The list shows Peek's name, notes, proxy and color as the save starts (PERF-13);
    // a protected row only what it shows while locked.
    const edit = before
      ? beginEdit(draft.id, locked ? { name: draft.name, color: draft.color || null } : { name: draft.name, notes: draft.notes, proxy_id: draft.proxy_id, color: draft.color || null }, before)
      : null;
    if (edit) showLocal(set, get);
    try {
      const fp = fingerprints.find((g) => g.id === draft.gpu_preset_id) ?? null;
      // Built on the profile as stored, so what the form doesn't show survives.
      const prev = locked ? await profileUnlock(draft.id, opts!.password!) : await profileGet(draft.id);
      const saved = await profileSave(toStored(draft, fp, prev), locked ? opts!.password : undefined);
      if (edit) endEdit(edit);
      // Only this profile changed: its row takes the saved one, no list read.
      putRows(set, get, [saved]);
      // A protected one's settings stay in its editor: nothing else reads them.
      if (!locked) for (const fn of formSaved) fn(draft);
      return "ok";
    } catch (e) {
      if (edit) { endEdit(edit); putBack(set, get, edit); }
      const msg = String(e);
      if (/changed after you opened it/.test(msg)) return "conflict";
      // No toast: the caller shows the failure where the edit was made (Peek's
      // line under its header, the form's line by Save; WRITE-15, F1).
      opts?.onFailed?.(e);
      return "failed";
    }
  },

  renameProfile: (id, name) =>
    patchRow(set, get, id, { name }, (real) => patchStored(real, (s) => { s.name = name; }),
      (was) => t("useProfile.renameFailed", { name: was }), (p) => profileRefusal(p, "edit")),
  setNotes: (id, notes) =>
    patchRow(set, get, id, { notes }, (real) => patchStored(real, (s) => { s.notes = notes; }),
      (was) => t("useProfile.notesFailed", { name: was }), (p) => profileRefusal(p, "edit") ?? lockedRefusal(p)),
  bindProxy: (id, proxyId) =>
    patchRow(set, get, id, { proxy_id: proxyId }, (real) => profileBindProxy(real, proxyId),
      (was) => t("useProfile.bindFailed", { name: was }),
      (p) => profileRefusal(p, "edit") ?? lockedRefusal(p) ?? (proxyId && isTeamRow(p) && proxyId !== p.proxy_id ? bindRefusal(proxyId) : null)),
  applyRows: (rows) => putRows(set, get, rows),

  // Optimistic: the row shows the new state at once and the poll confirms it.
  // A failed call puts the old state back and says why.
  startStop: async (given, opts) => {
    // A profile still being created, or still coming back from the Trash,
    // starts once the backend has it.
    let p = given;
    if (isArriving(p.id)) {
      const real = await resolveId(p.id);
      const found = real ? get().profiles.find((x) => x.id === real) : undefined;
      if (!found) return null;
      p = found;
    }
    if (get().running[p.id]) {
      if (get().startBusy.has(p.id)) return null; // the launch is still starting it
      const since = get().running[p.id];
      markStopped(set, get, p.id);
      try {
        await processKill(p.id);
        // The row's status word changes without focus moving: say it
        // (A11Y-22). Many at once say it in their own toast.
        if (!opts?.quiet) announce(t("useProfile.stoppedAnnounce", { name: p.name }));
        return null;
      } catch (e) {
        unmark(set, get, p.id);
        set({ running: { ...get().running, [p.id]: since } });
        // Names the profile; the backend's words are the detail (WRITE-01).
        if (!opts?.quiet) toast.fail({ title: t("useProfile.stopFailed", { name: p.name }), detail: e, subject: p.name });
        return reason(e);
      }
    }
    if (get().startBusy.has(p.id)) return null;
    const current = get().profiles.find((x) => x.id === p.id) ?? p;
    // Its proxy is gone: the backend would start it on this computer's own IP
    // (IFACE-01, backend B1). Refuse, and offer the fix.
    if (proxyMissing(current, get().proxies)) {
      const refused = t("useProfile.proxyMissingStart", { name: current.name });
      if (!opts?.quiet) {
        toast.err(refused, {
          action: { label: t("useProfile.assignProxy"), run: () => onAssign?.([current.id]) },
        });
      }
      return refused;
    }
    // Synced and open on another device: one device at a time holds it.
    const lease = leaseOf(current);
    if (lease) {
      const why = t("errors.openElsewhere", { device: lease });
      if (!opts?.quiet) {
        toast.fail({
          title: t("useProfile.startFailed", { name: current.name }), detail: why, subject: current.name,
          action: uiBridge.takeLease ? { label: t("profileSync.takeAction"), run: () => uiBridge.takeLease?.(current) } : undefined,
        });
      }
      return why;
    }
    // A team's: no Start, or another device has it open.
    const refused = startRefusal(current);
    if (refused) {
      if (!opts?.quiet) toast.fail({ title: t("useProfile.startFailed", { name: current.name }), detail: refused, subject: current.name });
      return refused;
    }
    // Cancel and Assign a proxy are choices, not failures: nothing to report.
    if (!opts?.directChecked) {
      const choice = await confirmDirect(get, [p]);
      if (choice === "assign") { onAssign?.([p.id]); return null; }
      if (choice !== "start") return null;
    }
    // The launch itself, with a protected profile's password. A wrong one is
    // thrown for the dialog to say, with nothing marked failed.
    const go = async (password?: string): Promise<string | null> => {
      markStarted(set, get, p.id);
      startedAt.set(p.id, performance.now());
      set({ startBusy: withSet(get().startBusy, p.id, true) });
      // What a cancel waits for: the launch has returned and this has settled it.
      let settled!: () => void;
      launching.set(p.id, new Promise<void>((done) => { settled = done; }));
      try {
        await launch(p.id, password);
        // A launch longer than the poll's patience (`PENDING_MS`) was dropped
        // from `running` meanwhile; it is running now, and a cancel stops it.
        if (get().running[p.id] === undefined) markStarted(set, get, p.id);
        // Cancelled while it launched: it went through anyway, and `cancelStart`
        // stops it; not a start to announce.
        if (!opts?.quiet && !get().cancelling.has(p.id)) announce(t("useProfile.startedAnnounce", { name: current.name }));
        return null;
      } catch (e) {
        if (get().cancelling.has(p.id)) {
          // The person cancelled it, and the launch ended (the backend's
          // "cancelled" error, or a failure on its way): a choice, not a
          // failure. Nothing runs, and nothing says it failed.
          set({ pending: without(get().pending, [p.id]), running: without(get().running, [p.id]) });
          return null;
        }
        if (password !== undefined && isWrongPassword(e)) {
          set({ pending: without(get().pending, [p.id]), running: without(get().running, [p.id]) });
          throw e;
        }
        // One write: a watcher never sees the start confirmed (pending gone,
        // running still marked) on its way to failed (the star ask counts it).
        set({ pending: without(get().pending, [p.id]), running: without(get().running, [p.id]), startFailed: { ...get().startFailed, [p.id]: Date.now() }, failReasons: { ...get().failReasons, [p.id]: reason(e) } });
        // Names the profile and closes on its own (8 s); the row keeps Failed
        // and its why line. The backend's words are the detail (WRITE-01, PC-037).
        if (!opts?.quiet) toast.fail({ title: t("useProfile.startFailed", { name: current.name }), detail: e, subject: current.name });
        return reason(e);
      } finally {
        set({ startBusy: withSet(get().startBusy, p.id, false) });
        startedAt.delete(p.id);
        launching.delete(p.id);
        settled();
      }
    };
    if (!isProtected(current)) return go();
    // Asked at every start (#127); the launch runs from the dialog, which stays
    // for another try after a wrong one. Cancel is a choice: nothing to report.
    const answer = await askPassword({
      kind: "ask",
      title: t("profileLock.startTitle", { name: current.name }),
      message: t("profileLock.startMessage"),
      confirmLabel: t("commands.start"),
      attempt: ({ password }) => go(password),
      onBusyCancel: () => { void get().cancelStart(p.id, { now: true }); },
    });
    if (!answer) opts?.skipped?.add(p.id);
    return answer ? answer.value : null;
  },

  cancelStart: async (id, opts) => {
    // Not launching (or already cancelled): nothing to cancel.
    if (!get().startBusy.has(id) || get().cancelling.has(id)) return;
    // The click that started it may be the one pressing this (a double-click).
    if (!opts?.now && performance.now() - (startedAt.get(id) ?? -Infinity) < CANCEL_GUARD_MS) return;
    const returned = launching.get(id);
    set({ cancelling: withSet(get().cancelling, id, true) });
    const name = get().profiles.find((p) => p.id === id)?.name ?? "";
    try {
      // B49: the backend ends the launch itself, and the pending `launch`
      // call returns a "cancelled" error. Without the command (Tauri rejects
      // an unknown one) or when it fails, the launch is left to return.
      try { await launchCancel(id); } catch { /* no launch_cancel yet, or it failed */ }
      await returned;
      // The launch went through anyway (no cancel, or too late): stop it.
      if (get().running[id] !== undefined) await get().stopMany([id]);
    } finally {
      set({ cancelling: withSet(get().cancelling, id, false) });
    }
    // Stopped, as asked: the row says so for a moment. A stop that failed
    // (already toasted) leaves the profile running, which the row must not hide.
    if (get().running[id] !== undefined) return;
    set({ cancelled: withSet(get().cancelled, id, true) });
    announce(t("useProfile.cancelledAnnounce", { name }));
    clearTimeout(cancelledTimers.get(id));
    cancelledTimers.set(id, setTimeout(() => {
      cancelledTimers.delete(id);
      set({ cancelled: withSet(get().cancelled, id, false) });
    }, CANCELLED_MS));
  },

  remove: async (id) => { await get().deleteMany([id]); },

  cloneProfile: async (id) => {
    const src = get().profiles.find((p) => p.id === id);
    if (!src) return;
    const refused = cloneRefusal(src) ?? lockedRefusal(src);
    if (refused) { toast.fail({ title: t("useProfile.cloneFailed", { name: src.name }), detail: refused }); return; }
    // The copy shows at once where the backend lists new profiles, named as
    // the backend names it (`clone_profile`); its call runs behind it (LQ-03).
    const row: ProfileMeta = {
      ...src, id: newPlaceholderId(), name: `${src.name} (copy)`, pinned: false,
      last_launched_at: null, created_at: new Date().toISOString(), total_runtime_ms: 0,
    };
    addPlaceholder(row, newRowIndex(get));
    showLocal(set, get);
    markArrived(set, get, [row.id]);
    const done = (async (): Promise<string | null> => {
      try {
        const from = await resolveId(id);
        if (!from) throw t("useProfile.createFailed");
        const meta = await profileClone(from);
        settle(set, get, row.id, meta);
        // IFACE-14: the copy may land far from its source; the toast opens it.
        toast.ok(t("useProfile.cloned", { name: src.name }), {
          action: { label: t("toast.open"), run: () => uiBridge.open?.(meta.id) },
        });
        return meta.id;
      } catch (e) {
        dropPlaceholder(row.id);
        set({ profiles: get().profiles.filter((p) => p.id !== row.id) });
        toast.fail({ title: t("useProfile.cloneFailed", { name: src.name }), detail: e });
        return null;
      }
    })();
    trackCreation(row.id, done);
    await done;
  },

  togglePin: async (p) => {
    const pinned = !p.pinned;
    await patchRow(set, get, p.id, { pinned }, (real) => profileSetPin(real, pinned),
      (was) => (pinned ? t("useProfile.pinFailed", { name: was }) : t("useProfile.unpinFailed", { name: was })),
      (row) => (isTeamRow(row) ? t("errors.notInTeam") : null));
  },

  exportCookies: async (given) => {
    const [id] = await realIds([given.id]);
    if (!id) return;
    const p = get().profiles.find((x) => x.id === id) ?? given;
    const refused = isTeamRow(p) ? t("errors.teamNotYet") : lockedRefusal(p);
    if (refused) { toast.fail({ title: t("useProfile.cookiesExportFailed", { name: p.name }), detail: refused, subject: p.name }); return; }
    try {
      const path = await saveDialog({
        defaultPath: `${(p.name || p.id).replace(/[^\w.-]+/g, "_")}-cookies.json`,
        filters: [{ name: "JSON", extensions: ["json"] }],
      });
      if (typeof path !== "string") return; // cancelled
      const n = await cookiesExportToFile(id, path);
      toast.ok(n === 1
        ? t("useProfile.cookieExportedOne")
        : t("useProfile.cookiesExportedMany", { n }));
      // Show the exported file itself in its folder. Reveal never runs a file,
      // where open would (SEC-04).
      try { await revealItemInDir(path); } catch {}
    } catch (e) { toast.fail({ title: t("useProfile.cookiesExportFailed", { name: p.name }), detail: e, subject: p.name }); }
  },

  importCookies: async (given) => {
    const [id] = await realIds([given.id]);
    if (!id) return;
    const refused = isTeamRow(given) ? t("errors.teamNotYet") : lockedRefusal(given);
    if (refused) { toast.fail({ title: t("useProfile.cookiesImportFailed", { name: given.name }), detail: refused, subject: given.name }); return; }
    if (get().running[id]) { toast.err(t("useProfile.stopBeforeCookieImport")); return; }
    try {
      const picked = await pickTextFile({ accept: ".json,application/json", maxBytes: 20 * MB });
      if (!picked) return;
      const cookies = JSON.parse(picked.text);
      if (!Array.isArray(cookies)) { toast.err(t("useProfile.cookiesNotArray")); return; }
      const n = await cookiesImport(id, cookies);
      toast.ok(n === 1
        ? t("useProfile.cookieImportedOne")
        : t("useProfile.cookiesImportedMany", { n }));
    } catch (e) {
      const name = get().profiles.find((x) => x.id === id)?.name ?? given.name;
      toast.fail({ title: t("useProfile.cookiesImportFailed", { name }), detail: e instanceof FileTooBig ? e.message : e, subject: name });
    }
  },

  setProfileFolder: async (id, f) => {
    // Dropping a profile onto the folder it already lives in is a no-op — tell
    // the user instead of silently doing nothing.
    const p = get().profiles.find((x) => x.id === id);
    if (p && p.folder === f) {
      const who = p.name || id.slice(0, 8);
      toast.info(f
        ? t("useProfile.alreadyInFolder", { who, f })
        : t("useProfile.notInAnyFolder", { who }));
      return;
    }
    await get().moveMany([id], f);
  },

  // The current Folder falls back to All Profiles through the sidebar, which
  // resets it whenever the Folder it names disappears.
  deleteFolder: async (f) => {
    const adminOnly = adminRefusal();
    if (adminOnly) { toast.fail({ title: t("useProfile.deleteFolderFailed", { f }), detail: adminOnly }); return; }
    // A team's deleted profiles go to its server's Trash, not this computer's.
    const team = inTeam();
    const inFolder = get().profiles.filter((p) => p.folder === f).map((p) => p.id);
    const count = inFolder.length;
    const choice = await confirmModal({
      title: t("useProfile.deleteFolderTitle", { f }),
      message:
        count === 0 ? t("useProfile.deleteEmptyFolder")
        : team ? (count === 1 ? t("useProfile.deleteTeamFolderWithOneProfile") : t("useProfile.deleteTeamFolderWithProfiles", { count }))
        : count === 1 ? t("useProfile.deleteFolderWithOneProfile")
        : t("useProfile.deleteFolderWithProfiles", { count }),
      buttons:
        count > 0
          ? [
              { label: t("useProfile.deleteFolderCancel"), value: "cancel" },
              { label: t("useProfile.deleteFolderKeepProfiles"), value: "keep" },
              { label: team ? t("useProfile.deleteFolderDeleteTeamProfiles") : t("useProfile.deleteFolderDeleteProfiles"), value: "trash", danger: true },
            ]
          : [
              { label: t("useProfile.deleteEmptyFolderCancel"), value: "cancel" },
              { label: t("useProfile.deleteEmptyFolderConfirm"), value: "keep", danger: true },
            ],
    });
    if (choice === "keep") {
      try {
        const n = await folderDelete(f, false);
        // The Folder lives in two places: profile tags (cleared by
        // folder_delete) and the registry of empty Folders.
        get().forgetFolder(f);
        useFolderMeta.getState().dropMeta(f);
        get().reload();
        toast.ok(
          n === 0 ? t("useProfile.folderDeleted", { f })
          : n === 1 ? t("useProfile.folderRemovedOneProfileKept", { f })
          : t("useProfile.folderRemovedProfilesKept", { f, n }),
        );
      } catch (e) {
        toast.fail({ title: t("useProfile.deleteFolderFailed", { f }), detail: e });
        // A team's profiles left it before the server kept the folder (write.rs delete_folder).
        if (team) void get().reload();
      }
      return;
    }
    if (choice !== "trash") return;
    if (team) {
      const gone = await moveToTrash(set, get, allowed(get, inFolder, (p, many) => profileRefusal(p, "delete", many), (name) => t("useProfile.teamDeleteFailed", { name })));
      if (gone.moved.length < inFolder.length) return;
      try {
        await folderDelete(f, false);
        get().forgetFolder(f);
        useFolderMeta.getState().dropMeta(f);
        toast.ok(t("useProfile.folderDeletedWithTeamProfiles", { f, n: gone.moved.length }));
      } catch (e) {
        // Its deleted profiles wait in the team's trash for 30 days: the folder stays, empty, till then.
        if (folderInTrash(e)) toast.ok(t("useProfile.teamProfilesDeletedFolderStays", { f, n: gone.moved.length }));
        else toast.fail({ title: t("useProfile.deleteFolderFailed", { f }), detail: e });
      }
      await get().reload();
      return;
    }
    // Its profiles go the way Move to Trash sends them: stopped first, into
    // the Trash, one Undo that also brings the Folder back. Never
    // folder_delete(f, true), which trashes running profiles and can delete
    // for good (IFACE-07, backend B2).
    const look = lookOf(f);
    const { moved, names } = await moveToTrash(set, get, inFolder);
    if (moved.length < inFolder.length) {
      // Some are still in it (the failure is shown): the Folder stays.
      if (moved.length > 0) {
        const title = moved.length === 1
          ? t("useProfile.movedToTrashOne", { name: names.get(moved[0]) ?? "" })
          : t("useProfile.movedToTrash", { n: moved.length });
        toast.undo(title, () => { void get().restoreMany(moved); });
      }
      return;
    }
    try {
      await folderDelete(f, false);
    } catch (e) {
      // Every profile reached the Trash, but the folder itself didn't go:
      // keep it (now empty, still registered) instead of claiming it's
      // gone, and reload so the sidebar's count matches the backend. No
      // profile left carries its folder tag, so the registry is what keeps
      // it visible now (the empty-folder mechanism `rememberFolder` already
      // exists for). The profiles' archived folder tag already points at f,
      // so their own Undo still lands them back in the kept folder.
      get().rememberFolder(f);
      toast.fail({ title: t("useProfile.folderKeptAfterTrashFailure", { f }), detail: e });
      await get().reload();
      toast.undo(
        moved.length === 1
          ? t("useProfile.movedToTrashOne", { name: names.get(moved[0]) ?? "" })
          : t("useProfile.movedToTrash", { n: moved.length }),
        () => { void get().restoreMany(moved); },
      );
      return;
    }
    get().forgetFolder(f);
    useFolderMeta.getState().dropMeta(f);
    await get().reload();
    toast.undo(
      moved.length === 1
        ? t("useProfile.folderDeletedWithOneProfile", { f })
        : t("useProfile.folderDeletedWithProfiles", { f, n: moved.length }),
      () => {
        get().rememberFolder(f);
        if (look) useFolderMeta.getState().setMeta(f, look);
        void get().restoreMany(moved);
      },
    );
  },

  renameFolder: async (from, to) => {
    const next = to.trim();
    if (!next || next === from) return false;
    const adminOnly = adminRefusal();
    if (adminOnly) { toast.fail({ title: t("useProfile.renameFolderFailed", { f: from }), detail: adminOnly }); return false; }
    const taken = get().folderRegistry.includes(next) || get().profiles.some((p) => p.folder === next);
    if (taken) { toast.err(t("useProfile.folderNameTaken", { f: next })); return false; }
    try {
      await folderRename(from, next);
      // The registry of empty Folders and the look follow the name (the
      // backend moved both); a Folder renamed here is kept even when empty.
      const registry = get().folderRegistry.map((x) => (x === from ? next : x));
      if (!registry.includes(next)) {
        registry.push(next);
        sendFolders(() => folderAdd(next));
      }
      // Views, and the list showing it, follow the name first.
      useViews.getState().renameFolder(from, next);
      onFolderRenamed?.(from, next);
      set({ folderRegistry: registry });
      useFolderMeta.getState().renameMeta(from, next);
      get().reload();
      return true;
    } catch (e) {
      toast.fail({ title: t("useProfile.renameFolderFailed", { f: from }), detail: e });
      return false;
    }
  },

  createProfile: async ({ form, folder, tagIds, newTags }) => {
    const fp = get().fingerprints.find((g) => g.id === form.gpu_preset_id) ?? null;
    const team = inTeam();
    // A team judges a new profile by its folder: Create there, Cookies for its extensions,
    // a proxy it may use; the steps after the first write need Edit on it (doors.rs).
    const refused = teamRefusal("create", inFolder(folder))
      ?? (form.extensions.length > 0 && teamRefusal("cookies", inFolder(folder)) ? t("rights.newExtensionsNeedCookies") : null)
      ?? (team && form.proxy_id ? bindRefusal(form.proxy_id) : null)
      ?? (tagIds.length + newTags.length > 0 && teamRefusal("edit", inFolder(folder)) ? t("rights.newTagsNeedEdit") : null);
    if (refused) return { meta: null, error: refused };
    // Without Edit on it, the one write that makes it is all there is.
    const mayEdit = !team || !teamRefusal("edit", inFolder(folder));
    // Every write of it goes to the space it was asked in (invoke.ts).
    const asked = spaceEpoch();
    let saved: ProfileMeta;
    try {
      // An empty id: the backend creates the profile (lib.rs `profile_save`).
      const payload = toStored({ ...form, id: "", rev: 0 }, fp);
      // A team makes it in its folder and on its proxy at once (write.rs create_profile).
      if (folder && team) payload._meta.folder = folder;
      if (team) payload._meta.proxy_id = form.proxy_id || null;
      saved = await withEpoch(asked, () => profileSave(payload));
    } catch (error) {
      return { meta: null, error };
    }
    // It exists from here: each part after it is tried on its own, and the
    // first that fails is reported with the profile.
    let failed: { step: CreateStep; error: unknown } | null = null;
    const step = async (name: CreateStep, run: () => Promise<unknown>) => {
      try { await withEpoch(asked, run); return true; } catch (error) { failed ??= { step: name, error }; return false; }
    };
    // The backend rolls new cores, memory and platform version for a profile
    // the UI creates (lib.rs `enrich_new_config`): the form's own values go
    // back over them, on the profile as stored, so it is what the form showed.
    if (mayEdit) await step("fingerprint", async () => {
      const stored = await profileGet(saved.id);
      // A new profile's form has screen 0 ("the template's"); the backend has
      // since fitted that screen to this display, and a 0 here would read as
      // "As in the fingerprint" and undo the fit.
      const screen = form.screen_w > 0 ? {} : { screen_w: stored?.screen?.width ?? 0, screen_h: stored?.screen?.height ?? 0 };
      const again = toStored({ ...form, ...screen, id: saved.id, rev: stored?._meta?.rev ?? 0 }, fp, stored);
      saved = await withEpoch(asked, () => profileSave(again));
    });
    const filed = !!folder && (saved.folder === folder || await step("folder", () => profileSetFolder(saved.id, folder)));
    const bound = !!form.proxy_id && ((team && saved.proxy_id === form.proxy_id) || await step("proxy", () => profileBindProxy(saved.id, form.proxy_id)));
    // Tags live in this app, not the backend: the new ones are made now that the profile is.
    const tags = useTags.getState();
    const made = newTags.map((name) => tags.create(name)?.id).filter((id): id is string => !!id);
    if (tagIds.length + made.length > 0) tags.setTags(saved.id, [...tagIds, ...made]);
    const meta = { ...saved, folder: filed ? folder : saved.folder, proxy_id: bound ? form.proxy_id : saved.proxy_id };
    // In the list at once, where the backend lists it; no reload (PERF-13).
    insertRows(set, get, [meta], newRowIndex(get));
    markArrived(set, get, [meta.id]);
    // PC-060, PC-095: the new row comes into view, or the list says it hides it.
    uiBridge.created?.(meta.id);
    return { meta, failed };
  },

  launchSynced: async (given) => {
    const ids = await realIds(given);
    const byId = new Map(get().profiles.map((p) => [p.id, p]));
    const proxies = get().proxies;
    // Fix round 1, item 1: a proxy-missing profile must never reach
    // sync_launch — it would run on this computer's own IP alongside the
    // rest of the fleet. Same notice and fix as launchMany, then the sync
    // goes ahead with whoever is left.
    const missing = ids.filter((id) => { const p = byId.get(id); return !!p && proxyMissing(p, proxies); });
    if (missing.length > 0) {
      toast.err(
        missing.length === 1
          ? t("useProfile.proxyMissingStart", { name: byId.get(missing[0])!.name })
          : t("useProfile.proxyMissingStartMany", { n: missing.length }),
        { action: { label: missing.length === 1 ? t("useProfile.assignProxy") : t("useProfile.assignProxies"), run: () => onAssign?.(missing) } },
      );
    }
    // Open on another computer: never in this fleet; nor a team's it may not start.
    const leased = sayLeased(ids.map((id) => byId.get(id)).filter((p): p is ProfileMeta => !!p && !missing.includes(p.id)));
    const left = ids.filter((id) => !missing.includes(id) && !leased.includes(id));
    // A protected one starts only on its own, with its password.
    const mayStart = new Set(allowed(get, left, (p, many) => startRefusal(p, many) ?? lockedRefusal(p), (name) => t("useProfile.startFailed", { name })));
    const held = [...leased, ...left.filter((id) => !mayStart.has(id))];
    // An id the list doesn't know can't be checked, so it doesn't go either.
    const startable = ids
      .map((id) => byId.get(id))
      .filter((p): p is ProfileMeta => !!p && !missing.includes(p.id) && !held.includes(p.id));
    // Task 11 fix round 1, item 1 (critical): a synced launch is still a
    // launch — a no-proxy profile joining the fleet on this computer's own
    // IP asks first too, same rule as launchMany. Cancel (or Assign, which
    // only fires with one profile left) drops the whole group: two fleets
    // starting apart, one held back by the question, is not "synchronised".
    const choice = await confirmDirect(get, startable);
    if (choice === "assign") { onAssign?.(startable.map((p) => p.id)); return false; }
    if (choice === "cancel") return false;
    const ok = (choice === "skip" ? startable.filter((p) => p.proxy_id || isProtected(p)) : startable).map((p) => p.id);
    if (ok.length < 2) return false; // nothing left to synchronise
    // Per launch, not fixed: two fleets must not share session files.
    const group = `fleet-${Date.now().toString(36)}`;
    try {
      const name = await syncLaunch(ok, group);
      set({ syncGroup: name });
      toast.ok(t("useProfile.synchronisingProfiles", { n: ok.length }));
      return true;
    } catch (e) {
      toast.fail({ title: t("useProfile.startInSyncFailed", { n: ok.length }), detail: e });
      return false;
    }
  },

  launchMany: async (ids) => {
    const byId = new Map(get().profiles.map((p) => [p.id, p]));
    const proxies = get().proxies;
    // One notice for every profile whose proxy is gone, not one per profile.
    const missing = ids.filter((id) => {
      const p = byId.get(id);
      return !!p && !get().running[id] && proxyMissing(p, proxies);
    });
    if (missing.length > 0) {
      toast.err(
        missing.length === 1
          ? t("useProfile.proxyMissingStart", { name: byId.get(missing[0])!.name })
          : t("useProfile.proxyMissingStartMany", { n: missing.length }),
        { action: { label: missing.length === 1 ? t("useProfile.assignProxy") : t("useProfile.assignProxies"), run: () => onAssign?.(missing) } },
      );
    }
    // Open on another computer: refused before anyone is asked about a proxy.
    const held = sayLeased(ids.map((id) => byId.get(id)).filter((p): p is ProfileMeta => !!p && !get().running[p.id] && !missing.includes(p.id)));
    // A team's it may not start, or open on another device: left out and said so, not failed.
    const known = ids.map((id) => byId.get(id)).filter((p): p is ProfileMeta => !!p && !get().running[p.id] && !missing.includes(p.id) && !held.includes(p.id));
    const skipped = known.filter((p) => startRefusal(p) !== null);
    if (skipped.length > 0) {
      toast.info(skipped.length === 1 ? t("useProfile.startSkippedOne", { name: skipped[0].name }) : t("rights.skipped", { n: skipped.length }),
        { detail: startRefusal(skipped[0], skipped.length > 1) ?? undefined });
      held.push(...skipped.map((p) => p.id));
    }
    const failed = [...missing, ...held];
    const startable = ids
      .map((id) => byId.get(id))
      .filter((p): p is ProfileMeta => !!p && !get().running[p.id] && !missing.includes(p.id) && !held.includes(p.id));
    const choice = await confirmDirect(get, startable);
    // Task 11 fix round 1, item 2: "assign" only fires with one profile left
    // to start (confirmDirect's single-target branch), and that one has no
    // proxy — same call as startStop's identical branch, or the button lies.
    if (choice === "assign") onAssign?.(startable.map((p) => p.id));
    if (choice === "cancel" || choice === "assign") return [...failed, ...startable.map((p) => p.id)];
    // "Start the others" leaves the ones with no proxy as they are.
    const todo = startable.filter((p) => choice !== "skip" || !!p.proxy_id || isProtected(p));
    for (const p of startable) if (!todo.includes(p)) failed.push(p.id);
    if (todo.length === 0) return failed;
    // One toast for the whole batch instead of one per profile (IFACE-06):
    // it counts up in place, then says how many started and which failed.
    const total = todo.length;
    const prog = total > 1 ? toast.progress({ title: t("useProfile.startingMany", { done: 0, total }), total }) : null;
    const errors: { p: ProfileMeta; message: string }[] = [];
    // Protected ones whose password the person did not give: left as they are.
    const unasked = new Set<string>();
    let done = 0;
    // One at a time, as before: each launch probes the network first, and each
    // protected one asks its password in turn.
    for (const p of todo) {
      const message = await get().startStop(p, { directChecked: true, quiet: prog !== null, skipped: unasked });
      if (message) { errors.push({ p, message }); failed.push(p.id); }
      else if (unasked.has(p.id)) failed.push(p.id);
      done++;
      prog?.update(done, t("useProfile.startingMany", { done, total }));
    }
    if (!prog) return failed;
    const started = total - errors.length - unasked.size;
    prog.done({
      title: errors.length
        ? t("useProfile.startedManyFailed", { n: started, total, failed: errors.length })
        : t("useProfile.startedMany", { n: started, total }),
      // One line per failure, named, in the app's words: the detail keeps its line breaks.
      detail: errors.map((f) => `${f.p.name}: ${explainError(f.message) ?? f.message}`).join("\n") || undefined,
      failed: errors.length,
      // The selection stays as it was (owner fix 1); this is the person's call.
      action: errors.length
        ? { label: t("useProfile.selectFailed"), run: () => uiBridge.select?.(errors.map((f) => f.p.id)) }
        : undefined,
    });
    return failed;
  },

  stopMany: async (ids) => {
    const byId = new Map(get().profiles.map((p) => [p.id, p]));
    const todo = ids.map((id) => byId.get(id)).filter((p): p is ProfileMeta => !!p && !!get().running[p.id]);
    // More than one: one toast for the failures, not one each (IFACE-06).
    const quiet = todo.length > 1;
    const results = await Promise.all(todo.map(async (p) => ({ p, message: await get().startStop(p, { quiet }) })));
    const failed = results.filter((r): r is { p: ProfileMeta; message: string } => r.message !== null);
    if (quiet && failed.length > 0) {
      toast.fail({
        title: t("useProfile.stopManyFailed", { n: failed.length, total: todo.length }),
        detail: failed.map((f) => `${f.p.name}: ${f.message}`).join("\n"),
      });
    }
    return failed.map((f) => f.p.id);
  },

  deleteMany: async (given) => {
    const ids = allowed(get, given, (p, many) => profileRefusal(p, "delete", many),
      (name) => (inTeam() ? t("useProfile.teamDeleteFailed", { name }) : t("useProfile.trashFailed", { name })));
    if (ids.length === 0) return [];
    if (inTeam()) {
      const one = ids.length === 1;
      const name = get().profiles.find((p) => p.id === ids[0])?.name ?? "";
      const yes = await confirmModal({
        title: one ? t("useProfile.teamDeleteTitleOne", { name }) : t("useProfile.teamDeleteTitle", { n: ids.length }),
        message: t("useProfile.teamDeleteMessage"),
        buttons: [
          { label: t("confirm.cancel"), value: false },
          { label: t("useProfile.teamDeleteConfirm"), value: true, danger: true },
        ],
      });
      if (yes !== true) return [];
      const { moved, names } = await moveToTrash(set, get, ids);
      if (moved.length > 0) {
        toast.ok(moved.length === 1 ? t("useProfile.teamDeletedOne", { name: names.get(moved[0]) ?? "" }) : t("useProfile.teamDeleted", { n: moved.length }));
      }
      return moved;
    }
    const { moved, names } = await moveToTrash(set, get, ids);
    if (moved.length === 0) return [];
    toast.undo(
      moved.length === 1
        ? t("useProfile.movedToTrashOne", { name: names.get(moved[0]) ?? "" })
        : t("useProfile.movedToTrash", { n: moved.length }),
      () => { void get().restoreMany(moved); },
    );
    return moved;
  },

  restoreMany: async (ids) => {
    // Back in the list at once, where they were: the rows they had a moment
    // ago (LQ-03). Until its own restore answers, anything done to a row
    // (Start, a rename, Peek) waits for it (`resolveId`). One that fails to
    // come back leaves again, with the reason.
    const remembered = new Map(ids.flatMap((id) => { const r = trashedRows.get(id); return r ? [[id, r] as const] : []; }));
    const answer = new Map<string, (real: string | null) => void>();
    for (const [id, r] of remembered) {
      addPlaceholder(r.meta, r.at);
      trackCreation(id, new Promise((done) => { answer.set(id, done); }));
    }
    showLocal(set, get);
    markArrived(set, get, [...remembered.keys()]);
    if (remembered.size > 0) onRestored?.([...remembered.keys()]);
    const name = remembered.get(ids[0])?.meta.name ?? "";
    const back: ProfileMeta[] = [];
    const lost: string[] = [];
    // One by one, in order. The rows the backend answered with go in once
    // they have all answered, in one update and so one render (speed lane
    // 2026-10-02: one update per row re-ran the list's work fifteen times
    // for an Undo of fifteen). The rows already show meanwhile, and an
    // action on one of them waits for its row (`resolveId`), answered after
    // that update so it never meets the placeholder.
    const { failed, firstError } = await eachReporting(ids, async (id) => {
      // A later move to the Trash remembers the row anew: that one stays.
      if (trashedRows.get(id) === remembered.get(id)) trashedRows.delete(id);
      try {
        back.push(await trashRestore(id));
      } catch (e) {
        lost.push(id);
        throw e;
      }
    });
    putRestored(set, get, back, lost);
    for (const m of back) answer.get(m.id)?.(m.id);
    for (const id of lost) answer.get(id)?.(null);
    // Out of the Trash again (an Undo): its copy is stale.
    storeBus.emit("trash", "profiles");
    if (failed.length > 0) {
      if (ids.length === 1) toast.fail({ title: t("useProfile.restoreFailed", { name }), detail: firstError });
      else toast.fail({ title: t("useProfile.restoreSomeFailed", { failed: failed.length, total: ids.length }), detail: firstError });
    }
    // Rows that could not show at once get the keyboard once they are back.
    if (remembered.size === 0 && back.length > 0) onRestored?.(back.map((m) => m.id));
  },

  // Dump selected profile FingerprintConfigs as a JSON array to clipboard.
  exportMany: async (given) => {
    const ids = await realIds(allowed(get, given, (p, many) => profileRefusal(p, "take_out", many) ?? lockedRefusal(p), () => t("useProfile.copyJsonFailed")));
    if (ids.length === 0) return;
    try {
      const payloads = await Promise.all(ids.map((id) => profileGet(id)));
      // A profile export can carry a temporary profile's proxy password (SEC-11).
      await copySecret(JSON.stringify(payloads, null, 2));
      const n = payloads.length;
      toast.ok(n === 1 ? t("useProfile.copiedJsonOne") : t("useProfile.copiedJsonMany", { n }), { detail: t("clipboard.clearsSoon") });
    } catch (e) { toast.fail({ title: t("useProfile.copyJsonFailed"), detail: e }); }
  },

  // Each profile on its own: one that fails (gone, locked) doesn't stop the
  // rest, and goes back to the Folder it had.
  moveMany: async (given, folder, opts) => {
    const ids = allowed(get, given, (p, many) => moveRefusal(p, folder, many), (name) => t("useProfile.moveFailed", { name }));
    if (ids.length === 0) return given.length === 0;
    const asked = new Set(ids);
    const was = new Map(get().profiles.filter((p) => asked.has(p.id)).map((p) => [p.id, p.folder]));
    const todo = ids.filter((id) => was.has(id) && was.get(id) !== folder);
    if (todo.length === 0) return true;
    if (folder) get().rememberFolder(folder);
    const names = new Map(get().profiles.map((p) => [p.id, p.name]));
    const { done, failed, firstError } = await patchRows(set, get, todo, () => ({ folder }), (real) => profileSetFolder(real, folder), () => {
      if (opts?.quiet) return;
      // IFACE-14: rows that left the list say where they went, with Undo.
      uiBridge.moved?.({
        ids: todo, names, from: was, to: folder,
        undo: () => { void get().restoreFolders(new Map(todo.map((id) => [id, was.get(id)!]))); },
      });
    });
    // Counted as the backend saw them: a row whose create failed is in neither.
    const total = done.length + failed.length;
    if (failed.length === 1 && total === 1) toast.fail({ title: t("useProfile.moveFailed", { name: names.get(failed[0]) ?? "" }), detail: firstError });
    else if (failed.length > 0) toast.fail({ title: t("useProfile.moveSomeFailed", { failed: failed.length, total }), detail: firstError });
    return failed.length === 0;
  },

  restoreFolders: async (folders) => {
    const byFolder = new Map<string, string[]>();
    for (const [id, f] of folders) byFolder.set(f, [...(byFolder.get(f) ?? []), id]);
    // Every Folder's moves start together, so every row is back at once (the
    // list's Undo puts them back in the selection right after).
    await Promise.all([...byFolder].map(([f, ids]) => get().moveMany(ids, f, { quiet: true })));
  },

  assignProxies: (ids, proxyIds) => get().assignPairs(ids.map((profileId) => ({ profileId, proxyId: proxyIds[0] ?? null }))),

  assignPairs: async (asked) => {
    const to = new Map(asked.map((p) => [p.profileId, p.proxyId]));
    const bindable = (p: ProfileMeta) => { const x = to.get(p.id); return x && isTeamRow(p) && x !== p.proxy_id ? bindRefusal(x) : null; };
    // A protected one's proxy changes in its editor, with its password.
    const ok = new Set(allowed(get, asked.map((p) => p.profileId), (p, many) => profileRefusal(p, "edit", many) ?? lockedRefusal(p) ?? bindable(p), (name) => t("useProfile.bindFailed", { name })));
    const pairs = asked.filter((p) => ok.has(p.profileId));
    if (pairs.length === 0) return false;
    const ids = pairs.map((p) => p.profileId);
    // Each row shows its proxy at once (PERF-13); a failed bind puts the old one back.
    const { done, failed, firstError } = await patchRows(
      set, get, ids,
      (_p, k) => ({ proxy_id: pairs[k].proxyId }),
      (real, k) => profileBindProxy(real, pairs[k].proxyId),
    );
    if (failed.length > 0) {
      const total = done.length + failed.length;
      const name = get().profiles.find((x) => x.id === failed[0])?.name ?? "";
      toast.fail({
        title: total === 1 ? t("useProfile.bindFailed", { name }) : t("useProfile.assignSomeFailed", { failed: failed.length, total }),
        detail: firstError,
      });
    }
    if (done.length > 0) {
      toast.ok(done.length === 1
        ? t("useProfile.proxyAssignedOne")
        : t("useProfile.proxiesAssigned", { n: done.length }));
    }
    return failed.length === 0;
  },

  regenerateFingerprints: async (given) => {
    // A protected one's fingerprint is sealed: it is never read here.
    const ids = allowed(get, given, (p, many) => profileRefusal(p, "edit", many) ?? lockedRefusal(p), (name) => t("useProfile.regenerateFailed", { name }));
    if (ids.length === 0) return 0;
    const one = ids.length === 1;
    const oneName = one ? (get().profiles.find((p) => p.id === ids[0])?.name ?? "") : "";
    const yes = await confirmModal({
      title: one
        ? t("useProfile.regenerateTitleOne", { name: oneName })
        : t("useProfile.regenerateTitle", { n: ids.length }),
      message: t("useProfile.regenerateMessage"),
      buttons: [
        { label: t("confirm.cancel"), value: false },
        { label: one ? t("useProfile.regenerateConfirmOne") : t("useProfile.regenerateConfirm"), value: true, primary: true },
      ],
    });
    if (yes !== true) return 0;
    // The verdicts first, for up to a second: a check that hangs must not hold this up.
    await loadGpuVerdicts();
    const todo = await realIds(ids);
    const { fingerprints, running } = get();
    const compat = useGpuCompat.getState().byId;
    let changed = 0;
    let skipped = 0;
    let failed = 0;
    let firstError: unknown;
    for (const id of todo) {
      // A running engine keeps the fingerprint it launched with; changing it
      // under it would only take effect on the next start anyway.
      if (running[id]) { skipped++; continue; }
      try {
        const form = await get().loadForm(id);
        const current = fingerprints.find((f) => f.id === form.gpu_preset_id);
        const os = current ? osIdFor(current.platform) : HOST_OS;
        // Same pick rules as the editor: this OS, backed by this machine's GPU.
        const sameOs = fingerprints.filter((f) => matchesOs(f.platform, os));
        const base = sameOs.length > 0 ? sameOs : fingerprints;
        const fitting = base.filter((f) => compat[f.id]?.compatible !== false);
        let pool = fitting.length > 0 ? fitting : base;
        if (pool.length > 1) pool = pool.filter((f) => f.id !== form.gpu_preset_id);
        const fp = pool[Math.floor(Math.random() * pool.length)];
        if (!fp) continue;
        const nav = fp.payload?.navigator ?? {};
        let picks: { hardware_concurrency?: number; device_memory?: number; platform_version?: string } = {};
        try { picks = await enrichPicksForPreset(fp.id); } catch { /* the preset's own values */ }
        await profileSave(toStored({
          ...form,
          gpu_preset_id: fp.id,
          hardware_concurrency: picks.hardware_concurrency ?? nav.hardware_concurrency ?? form.hardware_concurrency,
          device_memory: picks.device_memory ?? nav.device_memory ?? form.device_memory,
          platform_version: picks.platform_version ?? form.platform_version,
          user_agent: nav.user_agent ?? form.user_agent,
        }, fp));
        // The fingerprint is not in the row, so the row does not change: Peek's cached form goes by hand.
        forgetForm(id);
        changed++;
      } catch (e) { failed++; firstError ??= e; }
    }
    get().reload();
    if (failed > 0) {
      const name = get().profiles.find((x) => x.id === todo[0])?.name ?? "";
      toast.fail({
        title: todo.length === 1 ? t("useProfile.regenerateFailed", { name }) : t("useProfile.regenerateSomeFailed", { failed, total: ids.length }),
        detail: firstError,
      });
    }
    if (changed > 0) {
      toast.ok(changed === 1
        ? t("useProfile.regeneratedOne")
        : t("useProfile.regeneratedMany", { n: changed }));
    }
    if (skipped > 0) toast.info(t("useProfile.regenerateSkippedRunning", { n: skipped }));
    return changed;
  },

  // Paste profile JSON from clipboard → fresh profiles.
  importFromClipboard: async () => {
    try {
      const text = await clip.read();
      if (!text.trim()) { toast.err(t("useProfile.clipboardEmpty")); return; }
      const tooBig = clipboardTooBig(text);
      if (tooBig) { toast.err(tooBig); return; }
      const data = JSON.parse(text);
      const arr = Array.isArray(data) ? data : [data];
      // A team makes each one in the folder its JSON names, one by one (lib.rs profile_import):
      // Create is asked for every such folder before any is written.
      const refused = inTeam() ? arr.map((x) => teamRefusal("create", inFolder(String(x?._meta?.folder ?? "")))).find((r) => r) : null;
      if (refused) { toast.fail({ title: t("useProfile.importFailed"), detail: refused }); return; }
      const n = await profileImport(arr);
      // IFACE-14: what arrived is selected, and the first is brought into view.
      const had = new Set(get().profiles.map((p) => p.id));
      await get().reload();
      const arrived = get().profiles.filter((p) => !had.has(p.id)).map((p) => p.id);
      markArrived(set, get, arrived);
      if (arrived.length > 0) uiBridge.arrived?.(arrived);
      toast.ok(n === 1
        ? t("useProfile.profileImportedOne")
        : t("useProfile.profilesImportedMany", { n }));
    } catch (e) {
      toast.fail({ title: t("useProfile.importFailed"), detail: e });
      // A team's import stops at the first it refuses: the ones before it are there.
      if (inTeam()) void get().reload();
    }
  },
}));

// A row that changed (an edit, a reload with news, a save from Peek) may have
// left its cached form behind: Peek reads it again next time (PERF-14).
useProfile.subscribe((s, prev) => {
  if (s.profiles !== prev.profiles) forgetChanged(prev.profiles, s.profiles);
});

// Which profiles carry a tag is each profile's own: a team asks Edit on it (doors.rs tag_assign).
setTagAssignGuard((ids) => {
  const { refused, reason } = letThrough(useProfile.getState().profiles, ids, (p) => profileRefusal(p, "edit", ids.length > 1));
  return { refused, reason };
});

// Each read of the backend's folders brings the ones kept while empty.
onRegistry((folderRegistry) => useProfile.setState({ folderRegistry }));

// Another space: its list is read afresh, and nothing of the old one shows meanwhile.
onSpaceMoved(() => {
  const was = useProfile.getState().status;
  useProfile.setState({
    status: "idle", error: null, profiles: [], proxies: [], folderRegistry: [],
    running: {}, pending: {}, arrived: {}, startFailed: {}, failReasons: {},
  });
  if (was !== "idle") void useProfile.getState().init();
});
