import { create } from "zustand";
import { t } from "../../../shared/i18n";
import { toast } from "../../../shared/model/toast";
import { HOST_OS } from "../../../shared/lib/utils";
import { profileGet, useProfile } from "../../../entities/profile";
import { useTags } from "../../../entities/tag";
import { cleanTemplateName, osOfPlatform, templateNameTaken, templateSettingsOf, useTemplates } from "../../../entities/template";
import { settingsOfForm, sourceOfForm, type SourceKind } from "./template";
import type { ProfileFormDraft } from "./useProfileForm";

/// "Save as template" (#145): a profile's settings, from its form or as saved,
/// kept under a name. The fingerprint is not: only where it came from.

/** What a template is made of, read before the dialog opens. */
export type TemplateBase = {
  settings: Record<string, any>;
  folder: string;
  tagIds: string[];
  /** Tag names a form has not made yet: made as the template is saved. */
  newTags: string[];
  /** The profile's library fingerprint, when it has one this computer knows. */
  library: string | null;
  os: string;
  /** The name it starts with: the profile's. */
  name: string;
};

type SaveTemplateState = {
  base: TemplateBase | null;
  saving: boolean;
};

export const useSaveTemplate = create<SaveTemplateState>(() => ({ base: null, saving: false }));

export const closeSaveTemplate = () => useSaveTemplate.setState({ base: null, saving: false });

/** The dialog over a profile form, on what the form shows now (saved or not). */
export function saveFormAsTemplate(draft: ProfileFormDraft): void {
  const lib = useProfile.getState().fingerprints.find((f) => f.id === draft.form.gpu_preset_id) ?? null;
  const source = sourceOfForm(draft.form, lib, "library", HOST_OS);
  useSaveTemplate.setState({
    base: {
      settings: settingsOfForm(draft.form, lib), folder: draft.folder, tagIds: [...draft.tagIds], newTags: [...draft.newTags],
      library: lib ? lib.id : null, os: source.os, name: draft.form.name.trim(),
    },
    saving: false,
  });
}

/** The dialog on a profile as it is saved. */
export async function saveProfileAsTemplate(id: string): Promise<void> {
  const row = useProfile.getState().profiles.find((p) => p.id === id);
  try {
    const stored = await profileGet(id);
    const library = stored?._meta?.gpu_preset_id || null;
    const lib = library ? useProfile.getState().fingerprints.find((f) => f.id === library) : undefined;
    const ua = String(stored?.navigator?.user_agent ?? "");
    const os = osOfPlatform(lib?.platform ?? "") || (/android/i.test(ua) ? "Android" : osOfPlatform(stored?.navigator?.platform ?? "")) || HOST_OS;
    useSaveTemplate.setState({
      base: {
        settings: templateSettingsOf(stored), folder: row?.folder ?? stored?._meta?.folder ?? "",
        tagIds: [...(useTags.getState().byProfile[id] ?? row?.tags ?? [])], newTags: [],
        library: lib ? lib.id : null, os, name: row?.name ?? String(stored?.name ?? ""),
      },
      saving: false,
    });
  } catch (e) {
    toast.fail({ title: t("templates.readFailed", { name: row?.name ?? "" }), detail: e, subject: row?.name });
  }
}

/** Why `name` will not do as a new template's; null when it will. */
export function templateNameProblem(name: string): string | null {
  const n = cleanTemplateName(name);
  if (!n) return t("templates.nameRequired");
  return templateNameTaken(useTemplates.getState().templates, n) ? t("templates.nameTaken", { name: n }) : null;
}

/** Saves the dialog's template; false (and the dialog stays) when it did not land. */
export async function submitSaveTemplate(o: { name: string; kind: SourceKind; isDefault: boolean }): Promise<boolean> {
  const { base, saving } = useSaveTemplate.getState();
  if (!base || saving || templateNameProblem(o.name)) return false;
  useSaveTemplate.setState({ saving: true });
  const tags = useTags.getState();
  const made = base.newTags.map((n) => tags.create(n)?.id).filter((x): x is string => !!x);
  const name = cleanTemplateName(o.name);
  try {
    const saved = await useTemplates.getState().save({
      id: "", name, settings: base.settings, folder: base.folder, tags: [...new Set([...base.tagIds, ...made])],
      source: { library: o.kind === "library" ? base.library : null, os: base.os },
      is_default: o.isDefault, updated_at: 0,
    });
    toast.saved(t("templates.created", { name: saved.name }));
    closeSaveTemplate();
    return true;
  } catch (e) {
    useSaveTemplate.setState({ saving: false });
    toast.fail({ title: t("templates.saveFailed", { name }), detail: e, subject: name });
    return false;
  }
}
