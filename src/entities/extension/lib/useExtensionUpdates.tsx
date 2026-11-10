import { create } from "zustand";
import { listen } from "@tauri-apps/api/event";
import { openUrl } from "@tauri-apps/plugin-opener";
import { toast } from "../../../shared/model/toast";
import { onSpaceMoved } from "../../../shared/api/invoke";
import { useNav } from "../../../shared/model/navigation";
import { t } from "../../../shared/i18n";
import {
  extensionLinkSource, extensionUpdate, extensionUpdateCancel, extensionUpdateSkip, extensionUpdates, extensionUpdatesCheck,
} from "../model/api";
import { canUpdate, storeIdOf, storePage } from "../model/updates";
import { extensionName } from "../model/name";
import { useExtensions } from "./useExtensions";
import type { ExtensionEntry, ExtensionUpdate, LinkResult, UpdateNews, UpdateProgressEvent, UpdateResult } from "../model/types";

/** updates.rs CANCELLED: a cancel the person asked for says nothing. */
const CANCELLED = "the update was cancelled";
const says = (e: unknown, words: string) => String(e instanceof Error ? e.message : e).includes(words);
const index = (list: ExtensionUpdate[]) => Object.fromEntries(list.map((u) => [u.id, u]));

export type LinkAsk = Extract<LinkResult, { result: "confirm" }>;

export type ExtensionUpdatesStore = {
  /** By library entry id. Empty in a team space (v1). */
  byId: Record<string, ExtensionUpdate>;
  checking: boolean;
  updatingAll: boolean;
  /** Entries an Update or a link is running for. */
  working: Record<string, true>;
  /** Entries a link is running for: its download is not an update (updates.rs link). */
  linking: Record<string, true>;
  /** An update that asks first: the dialog's question. */
  ask: { entry: ExtensionEntry; update: ExtensionUpdate } | null;
  /** The extension whose Link to Web Store dialog is open. */
  linkFor: ExtensionEntry | null;

  load: () => Promise<void>;
  check: () => Promise<void>;
  update: (e: ExtensionEntry, confirmed?: boolean) => Promise<void>;
  updateAll: () => Promise<void>;
  cancel: (e: ExtensionEntry) => Promise<void>;
  skip: (e: ExtensionEntry) => Promise<void>;
  link: (e: ExtensionEntry, link: string, confirmed?: boolean) => Promise<LinkResult | null>;
  setAsk: (ask: ExtensionUpdatesStore["ask"]) => void;
  setLinkFor: (e: ExtensionEntry | null) => void;
  openInStore: (e: ExtensionEntry) => void;
};

/** What one Update came to, in a toast; `confirm` opens the dialog instead. */
function tell(r: UpdateResult, e: ExtensionEntry, ask: (u: ExtensionUpdate) => void) {
  const name = extensionName(e);
  switch (r.result) {
    case "applied": toast.ok(t("useExtensions.updated", { name, version: r.to })); break;
    case "waiting": toast.info(r.waiting_on.length > 0
      ? t("useExtensions.updateWaiting", { name })
      : t("useExtensions.updateWaitingLauncher", { name })); break;
    case "confirm": ask(r.update); break;
    case "needs_engine": toast.warn({ title: t("useExtensions.updateFailed", { name }), detail: t("extensions.needsEngine", { version: r.version }) }); break;
    case "not_served": toast.info(t("useExtensions.notServedNow")); break;
    case "current": toast.ok(t("useExtensions.upToDateOne", { name })); break;
  }
}

export const useExtensionUpdates = create<ExtensionUpdatesStore>((set, get) => {
  const mark = (id: string, on: boolean, linking = false) => set((s) => {
    const working = { ...s.working };
    const links = { ...s.linking };
    if (on) { working[id] = true; if (linking) links[id] = true; }
    else { delete working[id]; delete links[id]; }
    return { working, linking: links };
  });
  return {
    byId: {},
    checking: false,
    updatingAll: false,
    working: {},
    linking: {},
    ask: null,
    linkFor: null,

    load: async () => {
      // No records: the cards show no update state, nothing else breaks.
      try { set({ byId: index(await extensionUpdates()) }); } catch { /* kept as it was */ }
    },

    check: async () => {
      if (get().checking) return;
      set({ checking: true });
      try {
        const list = await extensionUpdatesCheck();
        set({ byId: index(list) });
        const n = list.filter(canUpdate).length;
        toast.ok(n === 0 ? t("useExtensions.upToDate")
          : n === 1 ? t("useExtensions.updatesFoundOne", { n }) : t("useExtensions.updatesFoundMany", { n }));
      } catch (e) {
        toast.fail({ title: t("useExtensions.checkFailed"), detail: e });
      } finally {
        set({ checking: false });
      }
    },

    update: async (e, confirmed = false) => {
      if (get().working[e.id]) return;
      mark(e.id, true);
      try {
        const r = await extensionUpdate(e.id, confirmed);
        tell(r, e, (update) => set({ ask: { entry: e, update } }));
        if (r.result === "applied") void useExtensions.getState().reload();
      } catch (err) {
        if (!says(err, CANCELLED)) toast.fail({ title: t("useExtensions.updateFailed", { name: extensionName(e) }), detail: err });
      } finally {
        mark(e.id, false);
        void get().load();
      }
    },

    // One at a time, one toast at the end; what asks first is named, not asked (§3.7).
    updateAll: async () => {
      const targets = useExtensions.getState().items.filter((e) => canUpdate(get().byId[e.id]));
      if (targets.length === 0 || get().updatingAll) return;
      set({ updatingAll: true });
      const total = targets.length;
      const progress = toast.progress({ title: t("useExtensions.updatingAll", { n: total }), total });
      let applied = 0;
      const waiting: string[] = [];
      const asks: string[] = [];
      const failed: string[] = [];
      for (const [i, e] of targets.entries()) {
        const name = extensionName(e);
        mark(e.id, true);
        try {
          const r = await extensionUpdate(e.id);
          if (r.result === "applied") applied++;
          else if (r.result === "waiting") waiting.push(name);
          else if (r.result === "confirm") asks.push(name);
          else if (r.result === "needs_engine") failed.push(name);
        } catch (err) {
          if (!says(err, CANCELLED)) failed.push(name);
        } finally {
          mark(e.id, false);
        }
        progress.update(i + 1);
      }
      const detail = [
        waiting.length ? t("useExtensions.waitingInUpdateAll", { names: waiting.join(", ") }) : "",
        asks.length ? t("useExtensions.skippedInUpdateAll", { names: asks.join(", ") }) : "",
        failed.length ? t("useExtensions.failedInUpdateAll", { names: failed.join(", ") }) : "",
      ].filter(Boolean).join("\n");
      progress.done({
        title: applied === 0 ? t("useExtensions.noneUpdated")
          : applied === 1 ? t("useExtensions.updatedAllOne", { n: applied }) : t("useExtensions.updatedAllMany", { n: applied }),
        detail: detail || undefined,
        failed: failed.length,
        // Nothing went in: no success mark.
        tone: applied === 0 ? "info" : "ok",
      });
      set({ updatingAll: false });
      if (applied > 0) void useExtensions.getState().reload();
      void get().load();
    },

    cancel: async (e) => {
      try { await extensionUpdateCancel(e.id); } catch (err) {
        toast.fail({ title: t("useExtensions.cancelFailed", { name: extensionName(e) }), detail: err });
      }
      void get().load();
    },

    skip: async (e) => {
      const version = get().byId[e.id]?.latest;
      if (!version) return;
      try {
        await extensionUpdateSkip(e.id, version);
        toast.info(t("useExtensions.skipped", { name: extensionName(e), version }));
      } catch (err) {
        toast.fail({ title: t("useExtensions.skipFailed", { name: extensionName(e) }), detail: err });
      }
      void get().load();
    },

    link: async (e, link, confirmed = false) => {
      const name = extensionName(e);
      mark(e.id, true, true);
      try {
        const r = await extensionLinkSource(e.id, link, confirmed);
        if (r.result === "linked") {
          toast.ok(t("useExtensions.linked", { name }));
          if (r.update) { const u = r.update; set((s) => ({ byId: { ...s.byId, [e.id]: u } })); }
          void useExtensions.getState().reload();
        }
        return r;
      } catch (err) {
        if (!says(err, CANCELLED)) toast.fail({ title: t("useExtensions.linkFailed", { name }), detail: err });
        return null;
      } finally {
        mark(e.id, false);
        void get().load();
      }
    },

    setAsk: (ask) => set({ ask }),
    setLinkFor: (linkFor) => set({ linkFor }),
    openInStore: (e) => {
      const id = storeIdOf(e);
      if (id) void openUrl(storePage(id)).catch(() => {});
    },
  };
});

/** A background check's news: one toast for new versions, one per new Web Store flag. */
function tellNews(news: UpdateNews) {
  const fresh = news.fresh ?? [];
  const n = fresh.length;
  if (n > 0) {
    toast.info(n === 1 ? t("useExtensions.updatesFoundOne", { n }) : t("useExtensions.updatesFoundMany", { n }), {
      action: { label: t("toast.open"), run: () => void useNav.getState().setSection("extensions") },
    });
  }
  for (const f of news.flagged ?? []) {
    toast.warn({
      title: t("useExtensions.flaggedNow", { name: f.name }),
      detail: t("extensions.flaggedHelp"),
      action: { label: t("toast.open"), run: () => void useNav.getState().setSection("extensions") },
    });
  }
  void useExtensionUpdates.getState().load();
}

/// The updates on every page (the sidebar's count, Peek): read once, then
/// kept by the backend's events. Returns the way to stop listening.
export function watchExtensionUpdates(): () => void {
  void useExtensionUpdates.getState().load();
  const offs = [
    listen<UpdateProgressEvent>("extension-update:progress", ({ payload: p }) => {
      useExtensionUpdates.setState((s) => {
        const u = s.byId[p.id];
        // A link downloads to compare files: the card says it links, not updates.
        if (!u || s.linking[p.id]) return s;
        const state = p.phase === "apply" ? "applying" : "downloading";
        return { byId: { ...s.byId, [p.id]: { ...u, state, progress: { phase: p.phase, received: p.received, total: p.total } } } };
      });
    }),
    listen<UpdateNews>("extension-updates:news", ({ payload }) => tellNews(payload)),
    // A check or an update over the local API: the cards read it again.
    listen<string>("store-changed", ({ payload }) => {
      if (payload === "extension-updates" || payload === "extensions") void useExtensionUpdates.getState().load();
    }),
  ];
  // Another space: its own records, none of the old ones.
  const offSpace = onSpaceMoved(() => {
    useExtensionUpdates.setState({ byId: {}, ask: null, linkFor: null });
    void useExtensionUpdates.getState().load();
  });
  return () => {
    offSpace();
    for (const off of offs) void off.then((fn) => fn());
  };
}
