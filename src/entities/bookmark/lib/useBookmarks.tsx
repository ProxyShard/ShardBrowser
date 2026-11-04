import { create } from "zustand";
import { toast } from "../../../shared/model/toast";
import { reportLoadError } from "../../../shared/model/loadError";
import { deferDelete, isPendingDelete } from "../../../shared/lib/deferDelete";
import { confirmModal } from "../../../shared/lib/confirm";
import { onSpaceMoved } from "../../../shared/api/invoke";
import { t } from "../../../shared/i18n";
import { bookmarkDelete, bookmarkDeleteMany, bookmarkList, bookmarkSave } from "../model/api";
import { teamRefusal } from "../../space";
import type { Bookmark } from "../model/types";
import { bookmarkName } from "./name";

/** Bookmarks of a many-at-once delete whose Undo toast has not gone yet. */
const pendingMany = new Set<string>();
// A bookmark deleted behind its Undo toast stays out of every read until the toast goes.
const living = (items: Bookmark[]) => items.filter((x) => !isPendingDelete(`bookmark:${x.id}`) && !pendingMany.has(x.id));

export const emptyBookmark = (folder = ""): Bookmark => ({
  id: "",
  title: "",
  url: "",
  folder,
});

// The page's tabs: All, Every profile, one per Folder. A Folder's tab has a
// prefix, so a Folder named "all" is still a tab of its own.
export const BOOKMARK_TAB_ALL = "all";
export const BOOKMARK_TAB_ANY = "any";
export const bookmarkFolderTab = (name: string) => `folder:${name}`;
/** The Folder a tab shows, or null on All and Every profile. */
export const bookmarkTabFolder = (tab: string) => (tab.startsWith("folder:") ? tab.slice(7) : null);

export type BookmarkStore = {
  status: "idle" | "loading" | "ready" | "error";
  /** Why the first load failed. */
  error: string | null;
  items: Bookmark[];
  /** Open editor, or null. */
  editing: Bookmark | null;
  search: string;
  /** The page's tab (BOOKMARK_TAB_ALL shows everything), not a bookmark's folder. */
  folder: string;
  /** The page's picks (ids). */
  selected: Set<string>;

  init: () => Promise<void>;
  reload: () => Promise<void>;
  setEditing: (b: Bookmark | null) => void;
  setSearch: (q: string) => void;
  setFolder: (f: string) => void;
  setSelected: (ids: Set<string>) => void;
  save: (b: Bookmark) => Promise<void>;
  remove: (b: Bookmark) => Promise<void>;
  /** One goes as `remove`; several ask first, then go in one backend call. */
  removeMany: (list: Bookmark[]) => Promise<void>;
};

export const useBookmarks = create<BookmarkStore>((set, get) => ({
  status: "idle",
  error: null,
  items: [],
  editing: null,
  search: "",
  folder: BOOKMARK_TAB_ALL,
  selected: new Set<string>(),

  init: async () => {
    if (get().status === "loading" || get().status === "ready") return;
    set({ status: "loading", error: null });
    try {
      set({ items: living(await bookmarkList()), status: "ready" });
    } catch (e) {
      set({ status: "error", error: String(e) });
      reportLoadError("bookmarks", t("useBookmarks.loadFailed"), e);
    }
  },
  reload: async () => {
    try { set({ items: living(await bookmarkList()) }); }
    catch (e) { toast.fail({ title: t("useBookmarks.refreshFailed"), detail: e }); }
  },
  setEditing: (editing) => set({ editing }),
  setSearch: (search) => set({ search }),
  setFolder: (folder) => set({ folder }),
  setSelected: (selected) => set({ selected }),

  save: async (b) => {
    const name = bookmarkName(b);
    // A team's: a new one needs Create, a change Edit, in the team.
    const refused = teamRefusal(b.id ? "edit" : "create");
    if (refused) { toast.fail({ title: t("useBookmarks.saveFailed", { name }), detail: refused, subject: name }); return; }
    try {
      await bookmarkSave(b);
      set({ editing: null });
      await get().reload();
      toast.saved(t("useBookmarks.saved", { name }));
    } catch (e) {
      toast.fail({ title: t("useBookmarks.saveFailed", { name }), detail: e, subject: name });
    }
  },

  // Behind an Undo toast, nothing asked (ruling 18).
  remove: async (b) => {
    const name = bookmarkName(b);
    // Refused by a team's rules: it never leaves the list, so it never comes back.
    const refused = teamRefusal("delete");
    if (refused) { toast.fail({ title: t("useBookmarks.deleteFailed", { name }), detail: refused }); return; }
    deferDelete({
      key: `bookmark:${b.id}`,
      title: t("useBookmarks.deleted", { name }),
      failTitle: t("useBookmarks.deleteFailed", { name }),
      hide: () => set({ items: get().items.filter((x) => x.id !== b.id) }),
      restore: () => void get().reload(),
      commit: async () => { await bookmarkDelete(b.id); await get().reload(); },
    });
  },

  // Behind one Undo toast, as one is; the deleted leave the selection, and
  // Undo or a failure puts them back in it (as Proxies do).
  removeMany: async (list) => {
    if (list.length <= 1) { if (list[0]) await get().remove(list[0]); return; }
    const n = list.length;
    const refused = teamRefusal("delete");
    if (refused) { toast.fail({ title: t("useBookmarks.deleteManyFailed", { n }), detail: refused }); return; }
    const ok = await confirmModal({
      title: t("useBookmarks.deleteManyTitle", { n }),
      message: t("useBookmarks.deleteManyMessage"),
      confirmLabel: t("useBookmarks.deleteManyConfirm", { n }),
      danger: true,
    });
    if (ok !== true) return;
    const ids = new Set(list.map((b) => b.id));
    let wasSelected: string[] = [];
    let failed: Bookmark[] = list;
    deferDelete({
      key: `bookmarks:${[...ids].sort().join(",")}`,
      title: t("useBookmarks.deletedMany", { n }),
      failTitle: () => failed.length === 1
        ? t("useBookmarks.deleteFailed", { name: bookmarkName(failed[0]) })
        : t("useBookmarks.deleteManyFailed", { n: failed.length }),
      hide: () => {
        for (const id of ids) pendingMany.add(id);
        const sel = get().selected;
        wasSelected = [...ids].filter((id) => sel.has(id));
        set({ items: get().items.filter((x) => !ids.has(x.id)), selected: new Set([...sel].filter((id) => !ids.has(id))) });
      },
      restore: () => {
        for (const id of ids) pendingMany.delete(id);
        void get().reload().then(() => {
          const here = new Set(get().items.map((x) => x.id));
          const back = wasSelected.filter((id) => here.has(id));
          if (back.length > 0) set({ selected: new Set([...get().selected, ...back]) });
        });
      },
      commit: async () => {
        try {
          await bookmarkDeleteMany([...ids]);
        } catch (e) {
          // A team takes them one by one: what is still listed is what failed.
          const left = (await bookmarkList().catch(() => null))?.filter((x) => ids.has(x.id));
          if (left && left.length > 0) failed = left;
          for (const id of ids) pendingMany.delete(id);
          throw e;
        }
        for (const id of ids) pendingMany.delete(id);
        await get().reload();
      },
    });
  },
}));

// Another space: its bookmarks are read afresh, nothing of the old ones shows or stays open.
onSpaceMoved(() => {
  const was = useBookmarks.getState().status;
  useBookmarks.setState({ status: "idle", error: null, items: [], editing: null, folder: BOOKMARK_TAB_ALL, selected: new Set<string>() });
  if (was !== "idle") void useBookmarks.getState().init();
});
