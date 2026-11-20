import { useEffect, useLayoutEffect, useRef, useState, type ComponentType } from "react";
import { Card, TabPanel, Tabs } from "@proxyshard/shardx-ui-kit";
import { PageBody, PageHeader } from "../../widgets/PageHeader";
import { useNav, type SettingsTab } from "../../shared/model/navigation";
import type { ApiInfo, Settings } from "../../entities/settings";
import { settingsLoadError, apiInfo } from "../../entities/settings";
import { LoadError, LoadingRows } from "../../shared/ui/ListStates";
import { useT } from "../../shared/i18n";
import { useUnsaved } from "../../shared/model/unsaved";
import { isMod, overlayHasKeyboard } from "../../shared/lib/keyboard";
import { commitFocusedField } from "../../shared/lib/commitField";
import { useSettingsDraft } from "./useSettingsDraft";
import { SaveBar } from "./SaveBar";
import { GeneralSection } from "./sections/General";
import { AppearanceSection } from "./sections/Appearance";
import { ProfileDefaultsSection } from "./sections/ProfileDefaults";
import { TagsSection } from "./sections/Tags";
import { TemplatesSection } from "./sections/Templates";
import { ViewsSection } from "./sections/Views";
import { ApiSection } from "./sections/Api";
import { HelperSection } from "./sections/Helper";
import { StorageSection } from "./sections/Storage";
import { SyncSection } from "./sections/Sync";
import { OtherSection } from "./sections/Other";
import { AboutSection } from "./sections/About";
import type { SettingsProps } from "./SettingRow";
import { showMeAround } from "../../features/tour";
import { useSync } from "../../entities/sync";

type SectionProps = SettingsProps & {
  /** What is on disk, for what speaks of saved settings (the restart note). */
  saved: Settings;
  api: ApiInfo | null;
  setApi: (api: ApiInfo) => void;
  /** Why the API details couldn't be read, and a way to read them again. */
  apiError: string | null;
  retryApi: () => void;
};

const SECTIONS: Record<SettingsTab, ComponentType<SectionProps>> = {
  general: GeneralSection,
  appearance: AppearanceSection,
  profiles: ProfileDefaultsSection,
  templates: TemplatesSection,
  tags: TagsSection,
  views: ViewsSection,
  api: ApiSection,
  helper: HelperSection,
  storage: StorageSection,
  sync: SyncSection,
  other: OtherSection,
  about: AboutSection,
};

// The open section's name, over its blocks (TYPE-10: the page's h1, then this
// h2, then the blocks' h3). The same keys as the tabs.
const TAB_LABEL: Record<SettingsTab, string> = {
  general: "settings.tabGeneral",
  appearance: "settings.tabAppearance",
  profiles: "settings.tabProfiles",
  templates: "settings.tabTemplates",
  tags: "settings.tabTags",
  views: "settings.tabViews",
  api: "settings.tabApi",
  helper: "settings.tabHelper",
  storage: "settings.tabStorage",
  sync: "settings.tabSync",
  other: "settings.tabOther",
  about: "settings.tabAbout",
};

export function SettingsPage() {
  const t = useT();
  const syncMissing = useSync((st) => st.missing);
  const tab = useNav((st) => (syncMissing && st.settingsTab === "sync" ? "general" : st.settingsTab));
  const openSettings = useNav((st) => st.openSettings);
  // The page's draft: nothing is written until Save; nothing shows until the
  // settings are read (LAYOUT-21).
  const draft = useSettingsDraft();
  // Leaving Settings with changes asks first (Save, Don't save, Cancel).
  useUnsaved(draft.dirty ? { id: "settings", label: t("settings.title"), save: draft.save, discard: draft.discard } : null);
  // Cmd/Ctrl+S saves. An app command cannot run in a field or a dialog, so
  // the page listens for it itself; with nothing to save, nothing happens.
  const latest = useRef(draft);
  useLayoutEffect(() => { latest.current = draft; });
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (!isMod(e) || e.altKey || e.shiftKey || e.key.toLowerCase() !== "s") return;
      // Never the browser's own Save page.
      e.preventDefault();
      if (e.repeat || overlayHasKeyboard()) return;
      // The field that has the keyboard commits first: the save reads what the screen shows.
      commitFocusedField();
      const d = latest.current;
      if (!d.dirty || d.held || d.state === "saving") return;
      void d.save();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, []);
  const [api, setApi] = useState<ApiInfo | null>(null);
  const [apiError, setApiError] = useState<string | null>(null);
  const refreshApi = () => apiInfo().then((a) => { setApi(a); setApiError(null); }).catch((e) => setApiError(String(e)));
  const [loadError, setLoadError] = useState<string | null>(null);
  useEffect(() => {
    settingsLoadError().then(setLoadError).catch(() => {});
    refreshApi();
    // eslint-disable-next-line react-hooks/exhaustive-deps -- once, on open
  }, []);
  // A saved API switch or port shows in the API details at once.
  useEffect(() => {
    if (draft.saves > 0) refreshApi();
    // eslint-disable-next-line react-hooks/exhaustive-deps -- on each save
  }, [draft.saves]);

  const Section = SECTIONS[tab];

  return (
    <>
      {/* The tour points at the Save bar while it shows (SaveBar), else at the title. */}
      <PageHeader
        title={t("settings.title")}
        titleTour={draft.dirty ? undefined : "settings-save"}
        help={{ title: t("pageHelp.settings.title"), body: t("pageHelp.settings.body"), onTour: () => showMeAround("settings") }}
      />
      {/* Each tab keeps its own place (WIG-06). */}
      <PageBody scrollKey={`settings:${tab}`}>
        {draft.getError && (
          <Card className="mb-3.5">
            <LoadError what={t("otherStates.settings")} error={draft.getError} onRetry={draft.retry} />
          </Card>
        )}
        {loadError && (
          // The one card look (UI-14), a tile in the error tint (no ring, no
          // stripe, borderless owner 2026-10-01); its bold title is the word.
          // `!`: the kit's cn does not merge, so the tile's own fill would
          // tie with this one on stylesheet order. The detail takes the sub
          // text, since the soft one falls under 4.5:1 on the tint.
          <Card padding="md" className="mb-3.5 bg-error-tint! text-body text-text-strong-950">
            <strong>{t("settings.loadErrorTitle")}</strong> {t.rich("settings.loadErrorBody", { code: (s) => <code>{s}</code> })}
            <div className="mt-1 text-secondary text-text-sub-600">{loadError}</div>
          </Card>
        )}

        {/* One content column beside the tabs, to the window's edge (owner, 2026-10-23); its help keeps a readable line. */}
        <div className="grid grid-cols-[180px_minmax(0,1fr)] items-start gap-6">
          <Tabs
            idBase="settings"
            aria-label={t("settings.sectionsLabel")}
            orientation="vertical"
            className="sticky top-0"
            value={tab}
            onChange={(v) => openSettings(v as SettingsTab)}
            items={[
              { value: "general", label: t("settings.tabGeneral") },
              { value: "appearance", label: t("settings.tabAppearance") },
              { value: "profiles", label: t("settings.tabProfiles") },
              { value: "templates", label: t("settings.tabTemplates") },
              { value: "tags", label: t("settings.tabTags") },
              { value: "views", label: t("settings.tabViews") },
              { value: "api", label: t("settings.tabApi") },
              { value: "helper", label: t("settings.tabHelper") },
              { value: "storage", label: t("settings.tabStorage") },
              // A backend without sync commands (a build before them) has no Sync tab.
              ...(syncMissing ? [] : [{ value: "sync", label: t("settings.tabSync") }]),
              { value: "other", label: t("settings.tabOther") },
              { value: "about", label: t("settings.tabAbout") },
            ]}
          />
          {/* The open section, named by its tab (A11Y-08; F8 keeps it). */}
          <TabPanel tabsId="settings" value={tab} className="flex flex-col gap-3.5">
            <h2 className="m-0 text-panel-title text-text-strong-950">{t(TAB_LABEL[tab])}</h2>
            {draft.s && draft.saved
              // Keyed by the discards, so a field that holds text of its own starts over.
              ? <Section key={draft.epoch} s={draft.s} saved={draft.saved} setS={draft.set} p={draft.p} setP={draft.setP} api={api} setApi={setApi} apiError={apiError} retryApi={refreshApi} />
              : !draft.getError && <LoadingRows rows={4} rowHeight={56} />}
          </TabPanel>
        </div>
      </PageBody>
      <SaveBar dirty={draft.dirty} saving={draft.state === "saving"} error={draft.error} held={draft.held} onSave={() => { commitFocusedField(); void draft.save(); }} onDiscard={draft.discard} />
    </>
  );
}
