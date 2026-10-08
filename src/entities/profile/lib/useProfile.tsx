import { create } from "zustand";
import { open, save as saveDialog } from "@tauri-apps/plugin-dialog";
import { openPath } from "@tauri-apps/plugin-opener";
import { toast } from "../../../shared/lib/toast";
import { confirmModal } from "../../../shared/lib/confirm";
import { clip } from "../../../shared/lib/clipboard";
import { storeBus } from "../../../shared/lib/storeBus";
import { t } from "../../../shared/i18n";
import { proxyList, type ProxyEntry } from "../../proxy";
import { fingerprintList, type FingerprintEntry } from "../../fingerprint";
import type { ProfileMeta, ProfileForm } from "../model/types";
import {
  profileList, profileGet, profileSave, profileDelete, profileClone,
  profileSetPin, profileSetFolder, profileBindProxy, profileImport,
  profileCreateFromTemplate, processList, processKill, launch, syncLaunch,
  folderDelete, cookiesExportToFile, cookiesImportFromFile,
} from "../model/api";
import { defaultForm, fromStored, toStored } from "../model/form";

const FOLDERS_KEY = "shardx-folders";

const loadFolderRegistry = (): string[] => {
  try { return JSON.parse(localStorage.getItem(FOLDERS_KEY) || "[]"); }
  catch { return []; }
};

export type QuickEditTarget = { kind: "proxy" | "notes"; profile: ProfileMeta };
export type FolderModalTarget = { profileId: string | null };

/** Narrows the list beyond the folder tab and the search box. */
export type ProfileFilters = {
  status: "all" | "running" | "idle";
  /** Country code of the bound proxy, or "" for any. */
  country: string;
  /** "bound" = has a proxy, "direct" = none. */
  proxy: "all" | "bound" | "direct";
};

export const emptyFilters = (): ProfileFilters => ({ status: "all", country: "", proxy: "all" });

/** Row order. "added" is the order the store returns them in — how it always was. */
export type ProfileSort =
  | "added"
  | "name-asc"
  | "name-desc"
  | "created-desc"
  | "created-asc"
  | "launched-desc"
  | "runtime-desc";

const SORT_KEY = "shardx.profiles.sort";

export const loadSort = (): ProfileSort => {
  try {
    const v = localStorage.getItem(SORT_KEY);
    return (v as ProfileSort) || "added";
  } catch {
    return "added";
  }
};

const saveSort = (v: ProfileSort) => {
  try { localStorage.setItem(SORT_KEY, v); } catch { /* private mode */ }
};

/** Missing dates sort last, whichever direction is asked for. */
function byDate(a: string | null | undefined, b: string | null | undefined, desc: boolean) {
  const av = a ? Date.parse(a) : NaN;
  const bv = b ? Date.parse(b) : NaN;
  const aBad = Number.isNaN(av);
  const bBad = Number.isNaN(bv);
  if (aBad && bBad) return 0;
  if (aBad) return 1;
  if (bBad) return -1;
  return desc ? bv - av : av - bv;
}

export function sortProfiles(list: ProfileMeta[], sort: ProfileSort): ProfileMeta[] {
  if (sort === "added") return list;
  // Pinning is the operator saying "keep this one where I can see it", and an
  // ordering that scatters pinned rows among two hundred others makes the pin
  // button look broken. Every order sorts within the pinned rows and within
  // the rest, and keeps the two groups apart.
  const pinned = list.filter((p) => p.pinned);
  const rest = list.filter((p) => !p.pinned);
  return [...sortGroup(pinned, sort), ...sortGroup(rest, sort)];
}

function sortGroup(list: ProfileMeta[], sort: ProfileSort): ProfileMeta[] {
  const out = [...list];
  switch (sort) {
    case "name-asc":
      out.sort((a, b) => a.name.localeCompare(b.name, undefined, { numeric: true }));
      break;
    case "name-desc":
      out.sort((a, b) => b.name.localeCompare(a.name, undefined, { numeric: true }));
      break;
    case "created-desc":
      out.sort((a, b) => byDate(a.created_at, b.created_at, true));
      break;
    case "created-asc":
      out.sort((a, b) => byDate(a.created_at, b.created_at, false));
      break;
    case "launched-desc":
      out.sort((a, b) => byDate(a.last_launched_at, b.last_launched_at, true));
      break;
    case "runtime-desc":
      out.sort((a, b) => (b.total_runtime_ms ?? 0) - (a.total_runtime_ms ?? 0));
      break;
  }
  return out;
}

export type ProfileStore = {
  status: "idle" | "loading" | "ready" | "error";
  error: string | null;

  profiles: ProfileMeta[];
  proxies: ProxyEntry[];
  fingerprints: FingerprintEntry[];

  /// Value = epoch ms at which the engine was first observed running. Used both
  /// as a truthy flag (any number = running) and as the anchor for the ticking
  /// uptime display in the Status column.
  running: Record<string, number>;
  /// Profiles whose `launch()` call is in-flight (pre-flight probes can be slow).
  startBusy: Set<string>;
  selected: Set<string>;

  // UI state lives in the store so feature buttons stay prop-free.
  search: string;
  folder: string;
  expanded: string | null;
  draft: ProfileForm | null;
  /// Empty folders persist here until a profile lands in them.
  folderRegistry: string[];
  folderModal: FolderModalTarget | null;
  /// Folder name currently highlighted as a drag-and-drop target ("__all__"
  /// for the All tab). Cleared in dragleave/drop.
  dropTarget: string | null;
  templatePickerOpen: boolean;
  quickEdit: QuickEditTarget | null;
  filters: ProfileFilters;
  sort: ProfileSort;
  /// Row the last plain click landed on; a shift-click selects the run from it.
  anchorId: string | null;

  init: () => Promise<void>;
  reload: () => Promise<void>;
  startProcessPolling: () => () => void;

  setSearch: (q: string) => void;
  setFolder: (f: string) => void;
  setDraft: (draft: ProfileForm | null) => void;
  setDropTarget: (target: string | null) => void;
  setTemplatePickerOpen: (open: boolean) => void;
  setQuickEdit: (target: QuickEditTarget | null) => void;
  setFolderModal: (target: FolderModalTarget | null) => void;
  setFilters: (f: Partial<ProfileFilters>) => void;
  setSort: (v: ProfileSort) => void;
  clearFilters: () => void;

  rememberFolder: (f: string) => void;
  forgetFolder: (f: string) => void;

  selectProfiles: (isChecked: boolean, profiles: ProfileMeta[]) => void;
  toggleSelect: (id: string) => void;
  /** Shift-click: selects every row between the anchor and `id`. */
  selectRangeTo: (id: string) => void;
  clearSelected: () => void;

  expand: (id: string) => Promise<void>;
  newProfile: () => void;
  cancelEdit: () => void;
  saveDraft: () => Promise<void>;

  startStop: (p: ProfileMeta) => Promise<void>;
  remove: (id: string) => Promise<void>;
  cloneProfile: (id: string) => Promise<void>;
  togglePin: (p: ProfileMeta) => Promise<void>;
  exportCookies: (p: ProfileMeta) => Promise<void>;
  importCookies: (p: ProfileMeta) => Promise<void>;

  setProfileFolder: (id: string, f: string) => Promise<void>;
  deleteFolder: (f: string) => Promise<void>;
  createFromTemplate: (tplId: string) => Promise<void>;

  bulkLaunch: () => Promise<void>;
  /** Launches the selection as one synchronised group. */
  bulkLaunchSynced: () => Promise<void>;
  /** Group currently being synchronised, or null. */
  syncGroup: string | null;
  bulkStop: () => Promise<void>;
  bulkDelete: () => Promise<void>;
  bulkExport: () => Promise<void>;
  bulkImport: () => Promise<void>;
};

export const useProfile = create<ProfileStore>((set, get) => ({
  status: "idle",
  error: null,

  profiles: new Array<ProfileMeta>(),
  proxies: new Array<ProxyEntry>(),
  fingerprints: new Array<FingerprintEntry>(),

  running: {},
  startBusy: new Set<string>(),
  selected: new Set<string>(),

  search: "",
  folder: "all",
  expanded: null,
  draft: null,
  folderRegistry: loadFolderRegistry(),
  folderModal: null,
  dropTarget: null,
  templatePickerOpen: false,
  quickEdit: null,
  filters: emptyFilters(),
  sort: loadSort(),
  anchorId: null,

  init: async () => {
    if (get().status === "loading" || get().status === "ready") return;
    set({ status: "loading" });
    try {
      const [profiles, proxies, fingerprints] = await Promise.all([
        profileList(), proxyList(), fingerprintList(),
      ]);
      set({ profiles, proxies, fingerprints, status: "ready" });
      // A proxy added on the Proxies page has to reach the editor's select, and
      // a proxy bound there — by the distribute dialog — has to reach the table.
      storeBus.on("proxies", () => { void get().reload(); });
      storeBus.on("profiles", () => { void get().reload(); });
    } catch (e) {
      set({ status: "error", error: (e as Error).message });
      toast.err(String(e));
    }
  },

  reload: async () => {
    try {
      const [profiles, proxies] = await Promise.all([profileList(), proxyList()]);
      set({ profiles, proxies });
    } catch (e) { toast.err(String(e)); }
  },

  // 2s poll for real child status; not optimistic UI state. Uptime is anchored
  // to the moment the engine actually started (now - uptime_ms), preserved
  // across polls so the displayed clock doesn't jitter. When a profile
  // transitions running → not-running, the backend has just bumped its persisted
  // total_runtime_ms — re-fetch so the Time column reflects the new total.
  startProcessPolling: () => {
    let cancelled = false;
    const tick = async () => {
      try {
        const list = await processList();
        if (cancelled) return;
        const now = Date.now();
        const prev = get().running;
        const next: Record<string, number> = {};
        for (const r of list) {
          next[r.profile_id] = prev[r.profile_id] ?? (now - r.uptime_ms);
        }
        const justExited = Object.keys(prev).some((id) => !(id in next));
        set({ running: next });
        if (justExited) get().reload();
      } catch {}
    };
    tick();
    const handle = setInterval(tick, 2000);
    return () => { cancelled = true; clearInterval(handle); };
  },

  setSearch: (search) => set({ search }),
  setFolder: (folder) => set({ folder }),
  setDraft: (draft) => set({ draft }),
  setDropTarget: (dropTarget) => set({ dropTarget }),
  setTemplatePickerOpen: (templatePickerOpen) => set({ templatePickerOpen }),
  setQuickEdit: (quickEdit) => set({ quickEdit }),
  setFolderModal: (folderModal) => set({ folderModal }),
  setFilters: (f) => set({ filters: { ...get().filters, ...f } }),
  setSort: (v) => { saveSort(v); set({ sort: v }); },
  clearFilters: () => set({ filters: emptyFilters() }),

  rememberFolder: (f) => {
    const next = get().folderRegistry.includes(f)
      ? get().folderRegistry
      : [...get().folderRegistry, f];
    localStorage.setItem(FOLDERS_KEY, JSON.stringify(next));
    set({ folderRegistry: next });
  },
  forgetFolder: (f) => {
    const next = get().folderRegistry.filter((x) => x !== f);
    localStorage.setItem(FOLDERS_KEY, JSON.stringify(next));
    set({ folderRegistry: next });
  },

  selectProfiles: (isChecked, profiles) => {
    const next = new Set(get().selected);
    if (isChecked) {
      for (const p of profiles) next.add(p.id);
    } else {
      for (const p of profiles) next.delete(p.id);
    }
    set({ selected: next });
  },
  toggleSelect: (id) => {
    const next = new Set(get().selected);
    if (next.has(id)) next.delete(id); else next.add(id);
    set({ selected: next, anchorId: id });
  },
  // Ordered as the table paints it. The clicked row decides the direction: a
  // ticked one clears the run, an unticked one selects it.
  selectRangeTo: (id) => {
    const order = visibleIds(get());
    const to = order.indexOf(id);
    if (to < 0) return;
    const anchor = get().anchorId;
    // Only a still-ticked anchor has a run to extend; spanning back over a
    // cleared one would put its tick straight back.
    const from = anchor && get().selected.has(anchor) ? order.indexOf(anchor) : -1;
    if (from < 0) { get().toggleSelect(id); return; }
    const [lo, hi] = from <= to ? [from, to] : [to, from];
    const removing = get().selected.has(id);
    const next = new Set(get().selected);
    for (let i = lo; i <= hi; i++) {
      if (removing) next.delete(order[i]); else next.add(order[i]);
    }
    set({ selected: next });
  },
  clearSelected: () => set({ selected: new Set<string>(), anchorId: null }),

  expand: async (id) => {
    if (get().expanded === id) { set({ expanded: null, draft: null }); return; }
    const stored = await profileGet(id);
    set({ draft: fromStored(stored), expanded: id });
  },
  newProfile: () => set({ draft: defaultForm(), expanded: "__new__" }),
  cancelEdit: () => set({ expanded: null, draft: null }),

  saveDraft: async () => {
    const { draft, fingerprints, folder } = get();
    if (!draft) return;
    try {
      const fp = fingerprints.find((g) => g.id === draft.gpu_preset_id) ?? null;
      const saved = await profileSave(toStored(draft, fp));
      await profileBindProxy(saved.id, draft.proxy_id);
      // A profile created while a folder tab is active should land in that
      // folder (otherwise it pops into "All" and the user has to drag it back).
      // `!draft.id` scopes this to creations only — edits keep their folder.
      if (!draft.id && folder && folder !== "all") {
        try { await profileSetFolder(saved.id, folder); }
        catch (e) { console.warn("auto-assign folder failed:", e); }
      }
      // Cookies picked in the editor: the profile has to exist first, so the
      // import happens here rather than on the form.
      if (draft.cookies_file) {
        try {
          // Chromium keeps its cookie database open and writes it back on
          // exit, so anything put there under a running browser is either
          // lost or corrupts the file.
          if (get().running[saved.id]) {
            throw new Error(t("useProfile.cookiesNeedStop"));
          }
          const n = await cookiesImportFromFile(saved.id, draft.cookies_file);
          toast.ok(n === 1
            ? t("useProfile.draftCookieImportedOne")
            : t("useProfile.draftCookiesImportedMany", { n }));
        } catch (e) {
          toast.err(t("useProfile.draftCookiesLoadFailed", { e: String(e) }));
        }
      }
      set({ expanded: null, draft: null });
      get().reload();
      storeBus.emit("profiles");
      toast.ok(draft.id
        ? t("useProfile.profileSaved")
        : t("useProfile.profileCreated", { name: saved.name }));
    } catch (e) { toast.err(String(e)); }
  },

  // Block the Start button until launch() returns. The launch includes
  // pre-flight steps that can take real time (UDP probe, geo, Widevine
  // pre-warm); surfacing the busy state is what the user reads as "did it work?".
  startStop: async (p) => {
    if (get().running[p.id]) {
      try { await processKill(p.id); }
      catch (e) { toast.err(String(e)); }
      return;
    }
    if (get().startBusy.has(p.id)) return;
    set({ startBusy: new Set([...get().startBusy, p.id]) });
    try {
      await launch(p.id);
      // Don't optimistically flip `running`; the 2s poll picks up the new child.
    } catch (e) {
      toast.err(String(e));
    } finally {
      const n = new Set(get().startBusy);
      n.delete(p.id);
      set({ startBusy: n });
    }
  },

  remove: async (id) => {
    if ((await confirmModal({
      title: t("useProfile.deleteProfileTitle"),
      message: t("useProfile.deleteProfileMessage"),
      danger: true,
    })) !== true) return;
    try {
      await profileDelete(id);
      get().reload();
      storeBus.emit("profiles");
    } catch (e) { toast.err(String(e)); }
  },

  cloneProfile: async (id) => {
    try { await profileClone(id); get().reload(); }
    catch (e) { toast.err(String(e)); }
  },

  togglePin: async (p) => {
    try { await profileSetPin(p.id, !p.pinned); get().reload(); }
    catch (e) { toast.err(String(e)); }
  },

  exportCookies: async (p) => {
    try {
      const path = await saveDialog({
        defaultPath: `${(p.name || p.id).replace(/[^\w.-]+/g, "_")}-cookies.txt`,
        filters: [
          { name: "Netscape cookies.txt", extensions: ["txt"] },
          { name: "JSON", extensions: ["json"] },
        ],
      });
      if (typeof path !== "string") return; // cancelled
      const n = await cookiesExportToFile(p.id, path);
      toast.ok(n === 1
        ? t("useProfile.cookieExportedOne")
        : t("useProfile.cookiesExportedMany", { n }));
      // Open the containing folder so the user sees exactly where it went.
      const dir = path.replace(/[/\\][^/\\]*$/, "");
      try { await openPath(dir); } catch {}
    } catch (e) { toast.err(String(e)); }
  },

  importCookies: async (p) => {
    if (get().running[p.id]) { toast.err(t("useProfile.stopBeforeCookieImport")); return; }
    try {
      const path = await open({
        multiple: false, directory: false, title: t("useProfile.selectCookiesDialogTitle"),
        filters: [{ name: "Cookies", extensions: ["txt", "json"] }],
      });
      if (typeof path !== "string") return;
      const n = await cookiesImportFromFile(p.id, path);
      toast.ok(n === 1
        ? t("useProfile.cookieImportedOne")
        : t("useProfile.cookiesImportedMany", { n }));
    } catch (e) { toast.err(String(e)); }
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
    try {
      await profileSetFolder(id, f);
      if (f) get().rememberFolder(f);
      get().reload();
      storeBus.emit("profiles");
    } catch (e) { toast.err(String(e)); }
  },

  deleteFolder: async (f) => {
    const count = get().profiles.filter((p) => p.folder === f).length;
    // Three outcomes: delete profiles, unfile, cancel.
    const choice = await confirmModal({
      title: t("useProfile.deleteFolderTitle", { f }),
      message:
        count > 0
          ? count === 1
            ? t("useProfile.deleteFolderWithOneProfile")
            : t("useProfile.deleteFolderWithProfiles", { count })
          : t("useProfile.deleteEmptyFolder", { f }),
      buttons:
        count > 0
          ? [
              { label: t("useProfile.deleteFolderCancel"), value: "cancel" },
              { label: t("useProfile.deleteFolderKeepProfiles"), value: "keep" },
              { label: t("useProfile.deleteFolderDeleteProfiles"), value: "delete", danger: true },
            ]
          : [
              { label: t("useProfile.deleteEmptyFolderCancel"), value: "cancel" },
              { label: t("useProfile.deleteEmptyFolderConfirm"), value: "keep", danger: true },
            ],
    });
    if (choice == null || choice === "cancel") return;
    const alsoDelete = choice === "delete";
    try {
      const n = await folderDelete(f, alsoDelete);
      // The folder lives in two places: profile tags (cleared by folder_delete)
      // and the localStorage registry of empty folders. Drop it from the
      // registry too, otherwise the tab lingers after every profile is gone.
      get().forgetFolder(f);
      if (get().folder === f) set({ folder: "all" });
      get().reload();
      toast.ok(
        alsoDelete
          ? n === 1
            ? t("useProfile.folderDeletedWithOneProfile", { f })
            : t("useProfile.folderDeletedWithProfiles", { f, n })
          : n === 1
            ? t("useProfile.folderRemovedOneProfileKept", { f })
            : t("useProfile.folderRemovedProfilesKept", { f, n }),
      );
    } catch (e) { toast.err(String(e)); }
  },

  createFromTemplate: async (tplId) => {
    try {
      const meta = await profileCreateFromTemplate(tplId);
      set({ templatePickerOpen: false });
      get().reload();
      toast.ok(t("useProfile.templateProfileCreated", { name: meta.name }));
      // Auto-open the new profile in the editor.
      const stored = await profileGet(meta.id);
      set({ draft: fromStored(stored), expanded: meta.id });
    } catch (e) { toast.err(String(e)); }
  },

  syncGroup: null,

  bulkLaunchSynced: async () => {
    const ids = [...get().selected];
    if (ids.length < 2) return;
    // Per launch, not fixed: two fleets must not share session files.
    const group = `fleet-${Date.now().toString(36)}`;
    try {
      const name = await syncLaunch(ids, group);
      set({ syncGroup: name });
      get().clearSelected();
      toast.ok(t("useProfile.synchronisingProfiles", { n: ids.length }));
    } catch (e) {
      toast.err(String(e));
    }
  },

  bulkLaunch: async () => {
    const { selected, running } = get();
    for (const id of selected) {
      if (running[id]) continue;
      try { await launch(id); } catch {}
    }
    get().clearSelected();
  },

  bulkStop: async () => {
    for (const id of get().selected) {
      try { await processKill(id); } catch {}
    }
    get().clearSelected();
  },

  bulkDelete: async () => {
    const ids = [...get().selected];
    if (ids.length === 0) return;
    if ((await confirmModal({
      title: t("useProfile.deleteProfilesTitle"),
      message: ids.length === 1
        ? t("useProfile.deleteProfilesMessageOne")
        : t("useProfile.deleteProfilesMessageMany", { n: ids.length }),
      danger: true,
    })) !== true) return;
    for (const id of ids) {
      try { await profileDelete(id); } catch (e) { toast.err(String(e)); }
    }
    get().clearSelected();
    get().reload();
    storeBus.emit("profiles");
    toast.ok(t("useProfile.movedToTrash", { n: ids.length }));
  },

  // Dump selected profile FingerprintConfigs as a JSON array to clipboard.
  bulkExport: async () => {
    const ids = [...get().selected];
    if (ids.length === 0) return;
    try {
      const payloads = await Promise.all(ids.map((id) => profileGet(id)));
      await clip.write(JSON.stringify(payloads, null, 2));
      toast.ok(t("useProfile.copiedToClipboard", { n: payloads.length }));
    } catch (e) { toast.err(String(e)); }
  },

  // Paste profile JSON from clipboard → fresh profiles.
  bulkImport: async () => {
    try {
      const text = await clip.read();
      if (!text.trim()) { toast.err(t("useProfile.clipboardEmpty")); return; }
      const data = JSON.parse(text);
      const arr = Array.isArray(data) ? data : [data];
      const n = await profileImport(arr);
      get().reload();
      toast.ok(n === 1
        ? t("useProfile.profileImportedOne")
        : t("useProfile.profilesImportedMany", { n }));
    } catch (e) { toast.err(t("useProfile.importFailed", { e: String(e) })); }
  },
}));

/// The ids in the order the table paints them — a range covers what is visible.
function visibleIds(s: ProfileStore): string[] {
  return applyProfileFilters(
    s.profiles, s.proxies, s.search, s.folder, s.filters, s.running, s.sort,
  ).map((p) => p.id);
}

/// Shared with `useVisibleProfiles` so the rows on screen and the rows a range
/// covers cannot drift apart.
export function applyProfileFilters(
  profiles: ProfileMeta[],
  proxies: ProxyEntry[],
  search: string,
  folder: string,
  filters: ProfileFilters,
  running: Record<string, number> = {},
  sort: ProfileSort = "added",
): ProfileMeta[] {
  const q = search.trim().toLowerCase();
  const byId = new Map(proxies.map((p) => [p.id, p]));
  const kept = profiles.filter((p) => {
    if (folder !== "all" && p.folder !== folder) return false;
    if (q && !p.name.toLowerCase().includes(q) && !p.notes.toLowerCase().includes(q)) return false;
    if (filters.proxy === "bound" && !p.proxy_id) return false;
    if (filters.proxy === "direct" && p.proxy_id) return false;
    if (filters.country) {
      const cc = p.proxy_id ? byId.get(p.proxy_id)?.country ?? "" : "";
      if (cc.toUpperCase() !== filters.country.toUpperCase()) return false;
    }
    if (filters.status !== "all") {
      const isRunning = !!running[p.id];
      if ((filters.status === "running") !== isRunning) return false;
    }
    return true;
  });
  // Sorted here rather than in the table, so the rows on screen and the rows a
  // shift-click range covers stay the same list.
  return sortProfiles(kept, sort);
}
