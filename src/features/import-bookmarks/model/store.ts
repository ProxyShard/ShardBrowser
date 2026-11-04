import { create } from "zustand";
import { t } from "../../../shared/i18n";
import { toast } from "../../../shared/model/toast";
import { FileTooBig, MB, pickTextFile } from "../../../shared/lib/pickFile";
import { BOOKMARK_TAB_ALL, bookmarkSave, useBookmarks } from "../../../entities/bookmark";
import { teamError } from "../../../entities/team";
import { newMarks, readChromeBookmarks, type ChromeFile } from "../lib/chromeBookmarks";

/** Chrome's file for a few thousand bookmarks is a few MB. */
const MAX_BYTES = 50 * MB;

export type ImportProgress = { done: number; total: number };
/** How far the last import got before a save was refused, and why, in words. */
export type ImportFailure = ImportProgress & { reason: string };

type ImportState = {
  open: boolean;
  fileName: string;
  /** The file's bookmarks; null when it is not Chrome's file or could not be read. */
  file: ChromeFile | null;
  /** Why the file cannot be used, in words. */
  problem: string | null;
  /** The profile Folder they show in; "" is every profile. */
  folder: string;
  /** Set while the bookmarks are being saved. */
  progress: ImportProgress | null;
  stopping: boolean;
  failure: ImportFailure | null;
  /** How far an import they stopped got; the dialog stays, to go on or close. */
  stopped: ImportProgress | null;
  /** Asks for the file and opens the dialog on it; false when the pick was cancelled. */
  pick: () => Promise<boolean>;
  setFolder: (folder: string) => void;
  run: () => Promise<void>;
  /** Closes, or stops a running import once the bookmark in flight is saved
   *  (Esc and a press on the backdrop too: it stops, it never closes). */
  close: () => void;
};

/** A save refused in a team without Create says so; anything else as the app words errors. */
const reasonOf = (e: unknown): string => (/\bneeds create\b/i.test(String(e)) ? t("importBookmarks.noCreateRight") : teamError(e));

export const useImportBookmarks = create<ImportState>((set, get) => ({
  open: false,
  fileName: "",
  file: null,
  problem: null,
  folder: "",
  progress: null,
  stopping: false,
  failure: null,
  stopped: null,

  pick: async () => {
    if (get().progress) return false;
    // Chrome's file has no extension: any file may be picked.
    let picked: { name: string; text: string } | null;
    try {
      picked = await pickTextFile({ accept: "", maxBytes: MAX_BYTES });
    } catch (e) {
      set({ open: true, fileName: "", file: null, problem: e instanceof FileTooBig ? e.message : t("importBookmarks.readFailed"), failure: null, stopped: null });
      return true;
    }
    if (!picked) return false;
    const file = readChromeBookmarks(picked.text);
    set({
      open: true,
      fileName: picked.name,
      file,
      problem: file ? null : t("importBookmarks.notChrome", { name: picked.name }),
      // A new import starts on every profile; another file in the same dialog keeps the choice.
      folder: get().open ? get().folder : "",
      failure: null,
      stopped: null,
    });
    // The list it is compared with, when the page has not loaded it yet.
    void useBookmarks.getState().init();
    return true;
  },

  setFolder: (folder) => set({ folder }),

  run: async () => {
    const { file, folder } = get();
    if (!file || get().progress) return;
    set({ progress: { done: 0, total: 0 }, stopping: false, failure: null, stopped: null });
    // Against the list as it is now, so a second import adds nothing.
    const bm = useBookmarks.getState();
    await (bm.status === "ready" ? bm.reload() : bm.init());
    if (useBookmarks.getState().status !== "ready") {
      set({ progress: null, failure: { done: 0, total: 0, reason: t("useBookmarks.loadFailed") } });
      return;
    }
    const { fresh } = newMarks(file.marks, useBookmarks.getState().items);
    const total = fresh.length;
    let done = 0;
    let failure: ImportFailure | null = null;
    set({ progress: { done, total } });
    // One at a time, in Chrome's order; the first refusal stops the rest.
    for (const m of fresh) {
      if (get().stopping) break;
      try {
        await bookmarkSave({ id: "", title: m.title, url: m.url, folder, root: m.root, path: m.path });
      } catch (e) {
        failure = { done, total, reason: reasonOf(e) };
        break;
      }
      done++;
      set({ progress: { done, total } });
    }
    await useBookmarks.getState().reload();
    if (failure) {
      set({ progress: null, stopping: false, failure });
      return;
    }
    if (done < total) {
      // Stopped: said in the dialog, with the rest one click away.
      set({ progress: null, stopping: false, stopped: { done, total } });
      return;
    }
    set({ open: false, progress: null, stopping: false });
    if (done === 0) return;
    // On All, so a Folder tab does not hide what just came in.
    useBookmarks.getState().setFolder(BOOKMARK_TAB_ALL);
    toast.ok(done === 1 ? t("importBookmarks.importedOne") : t("importBookmarks.importedMany", { n: done }));
  },

  close: () => {
    if (get().progress) set({ stopping: true });
    else set({ open: false });
  },
}));
