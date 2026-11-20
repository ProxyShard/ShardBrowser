import { create } from "zustand";
import { onSpaceMoved } from "../../../shared/api/invoke";
import { isPendingDelete } from "../../../shared/lib/deferDelete";
import { toast } from "../../../shared/model/toast";
import { t } from "../../../shared/i18n";
import type { ProfileTemplate } from "../model/types";
import { templateList, templateSave } from "../model/api";

/// The space's profile templates (#145). The backend keeps them
/// (templates.rs); a save waits for its answer, which the list then shows.

/** Longest template name, as the backend cuts it. */
export const TEMPLATE_NAME_MAX = 64;

export const cleanTemplateName = (name: string) => name.trim().replace(/\s+/g, " ").slice(0, TEMPLATE_NAME_MAX);
/** Whether another template than `exceptId` has this name (any case). */
export const templateNameTaken = (list: readonly ProfileTemplate[], name: string, exceptId?: string) => {
  const n = cleanTemplateName(name).toLocaleLowerCase();
  return list.some((x) => x.id !== exceptId && x.name.toLocaleLowerCase() === n);
};

const byName = (a: ProfileTemplate, b: ProfileTemplate) => a.name.localeCompare(b.name, undefined, { sensitivity: "base" }) || a.id.localeCompare(b.id);
/** A delete waiting behind its Undo stays out of the list. */
export const templateDeleteKey = (id: string) => `template:${id}`;

type TemplateStore = {
  templates: ProfileTemplate[];
  loaded: boolean;
  /** Reads them once; never rejects (a failure is shown). */
  load: () => Promise<void>;
  /** Reads them again: a change made elsewhere, another space. */
  reload: () => Promise<void>;
  /** Saves it and shows the answer; rejects with the backend's words. */
  save: (tpl: ProfileTemplate) => Promise<ProfileTemplate>;
  /** Out of the list now (a delete behind its Undo), or back in it. */
  hide: (id: string) => void;
  show: (tpl: ProfileTemplate) => void;
};

let loading: Promise<void> | null = null;

export const useTemplates = create<TemplateStore>((set, get) => {
  const read = async () => {
    const list = await templateList();
    set({ templates: list.filter((x) => !isPendingDelete(templateDeleteKey(x.id))).sort(byName), loaded: true });
  };
  return {
    templates: [],
    loaded: false,
    load: () => loading ??= read().catch((e) => { toast.fail({ title: t("templates.loadFailed"), detail: e }); }),
    reload: async () => {
      await get().load();
      try { await read(); } catch (e) { toast.fail({ title: t("templates.loadFailed"), detail: e }); }
    },
    save: async (tpl) => {
      const saved = await templateSave(tpl);
      // One default per space: the backend took the mark from the rest.
      const rest = get().templates.filter((x) => x.id !== tpl.id && x.id !== saved.id)
        .map((x) => (saved.is_default && x.is_default ? { ...x, is_default: false } : x));
      set({ templates: [...rest, saved].sort(byName) });
      return saved;
    },
    hide: (id) => set({ templates: get().templates.filter((x) => x.id !== id) }),
    show: (tpl) => set({ templates: [...get().templates.filter((x) => x.id !== tpl.id), tpl].sort(byName) }),
  };
});

// Another space: none of the old templates show while its own are read.
onSpaceMoved(() => {
  if (!loading) return;
  useTemplates.setState({ templates: [], loaded: false });
  void useTemplates.getState().reload();
});

/** The space's default template, if one is marked. */
export const defaultTemplate = (list: readonly ProfileTemplate[] = useTemplates.getState().templates) =>
  list.find((x) => x.is_default) ?? null;
