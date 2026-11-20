import { openUrl } from "@tauri-apps/plugin-opener";
import { t } from "../../shared/i18n";
import { siteUrl } from "../../shared/lib/utils";
import {
  ComputerIcon, FolderAddIconApp, GlobeIcon, HelpCircleIcon, KeyboardIcon, MoonIcon, NavAutomationIcon, NavBookmarksIcon, NavBrowsersIcon,
  NavExtensionsIcon, NavFingerprintsIcon, NavPatchLogIcon, NavSettingsIcon, NavShopIcon, NavTrashIcon,
  RouteIcon, SpaceTeamIcon, StopIcon, SunIcon, AddIcon, SyncedIcon,
} from "../../shared/icons";
import type { Section } from "../../shared/types";
import { useNav, type SettingsTab } from "../../shared/model/navigation";
import { usePalette } from "../../shared/model/palette";
import { useShortcutSheet } from "../../shared/model/shortcutSheet";
import { useWhatsNew } from "../../shared/model/whatsNew";
import type { Theme } from "@proxyshard/shardx-ui-kit";
import { setThemeFromCommand } from "../../shared/model/theme";
import { dropPref } from "../../pages/settings/useSettingsDraft";
import { focusPageFilter, hasPageFilter } from "../../shared/lib/pageFilter";
import { cycleRegion } from "../../shared/lib/regions";
import { defineCommands, type Command } from "../../shared/commands";
import { leaveList, useProfile, useProfileUi } from "../../entities/profile";
import { leaveEditor, useAutomation } from "../../entities/automation";
import { toast } from "../../shared/model/toast";
// The lib file, not the tour's index, so the registry does not load the tour's screens.
import { takeTheTour } from "../tour/lib/entryPoints";
// The stores alone, likewise: the team dialogs load with the sidebar and the Team page.
import { activeTeam, adminRefusal, useSpace } from "../../entities/space";
import { useCreateTeam } from "../create-team/model/store";
import { useJoinTeam } from "../join-team/model/store";
import { useTeamAsk } from "../team-admin/model/store";
import { useTeamTrash } from "../team-trash/model/store";
import { syncConnected, useSync } from "../../entities/sync";
import { syncNow } from "../sync-setup/model/actions";

// A theme picked here is stored at once and shows at once; a theme still
// waiting unsaved in Settings gives way to it.
const chooseTheme = (theme: Theme) => { setThemeFromCommand(theme); dropPref("theme"); };

// A page asked for from the keys or the palette gets the keyboard (A11Y-04).
// Go to Automation, pressed on its own page, leaves the open project for the
// list, as the sidebar's item does (the unsaved-changes question comes first).
const go = (section: Section) => () => {
  const nav = useNav.getState();
  if (section === "automation" && nav.section === "automation") {
    void leaveEditor().then((ok) => { if (ok) void nav.setSection(section, { focus: true }); });
    return;
  }
  void nav.setSection(section, { focus: true });
};
// English words the palette also finds these by, in every language.
const THEME_WORDS = () => ["theme", "appearance", "mode"];
const APP = { area: "app", scope: "global" } as const;
// The palette's own keys, one group so the sheet keeps this order.
const PALETTE = { area: "palette", scope: "global", group: "help", keyOwner: "self", surfaces: ["sheet"] } as const;

/// The Settings tabs, one label each. A typed record: a new tab that has no
/// command here fails tsc (PC-110).
export const SETTINGS_TABS: Record<SettingsTab, () => string> = {
  general: () => t("settings.tabGeneral"),
  appearance: () => t("settings.tabAppearance"),
  profiles: () => t("settings.tabProfiles"),
  templates: () => t("settings.tabTemplates"),
  tags: () => t("settings.tabTags"),
  views: () => t("settings.tabViews"),
  api: () => t("settings.tabApi"),
  helper: () => t("settings.tabHelper"),
  storage: () => t("settings.tabStorage"),
  sync: () => t("settings.tabSync"),
  other: () => t("settings.tabOther"),
  about: () => t("settings.tabAbout"),
};
export const SETTINGS_TAB_IDS = Object.keys(SETTINGS_TABS) as SettingsTab[];

/// The palette's own keys, by role: the sheet lists them, and the palette's
/// footer prints their caps (E7), so neither spells a key by hand.
export const PALETTE_KEYS = {
  run: { ...PALETTE, id: "palette.run", label: () => t("commands.paletteRun"), keys: ["Enter"] },
  openPeek: { ...PALETTE, id: "palette.openPeek", label: () => t("commands.openPeek"), keys: ["Mod+Enter"] },
  // A result's actions in place of the results (ruling 11); ArrowRight only
  // with the caret at the end of the search.
  actions: { ...PALETTE, id: "palette.actions", label: () => t("commands.paletteActions"), keys: ["Tab", "ArrowRight"], keysJoin: "or" },
  move: { ...PALETTE, id: "palette.move", label: () => t("commands.paletteMove"), keys: ["ArrowUp", "ArrowDown"] },
  close: { ...PALETTE, id: "palette.close", label: () => t("commands.paletteClose"), keys: ["Escape"] },
} satisfies Record<string, Command>;

export const APP_COMMANDS: readonly Command[] = [
  {
    ...APP, id: "app.palette", group: "help",
    label: () => t("commands.openPalette"), keys: ["Mod+K"], keyWhere: "app", inFields: true,
    surfaces: ["sheet"],
    run: () => { const p = usePalette.getState(); p.setOpen(!p.open); },
  },
  {
    ...APP, id: "app.shortcuts", group: "help", icon: KeyboardIcon,
    focusAfter: true,
    label: () => t("commands.showShortcuts"), keys: ["?"], keyWhere: "app", singleKey: true,
    keywords: () => ["keys", "help", "?"],
    surfaces: ["palette", "sheet"],
    run: () => useShortcutSheet.getState().setOpen(true),
  },
  {
    ...APP, id: "app.filter", scope: "page", group: "view",
    label: () => t("commands.filter"), keys: ["/"], keyWhere: "page", singleKey: true,
    surfaces: ["sheet"],
    // On a page whose header has a filter (the page header registers it).
    when: () => hasPageFilter(),
    run: () => { focusPageFilter(); },
  },
  // The sidebar, the page header, the content and Peek (A11Y-14). From a
  // text field too, as in every desktop app; a dialog or menu keeps them.
  { ...APP, id: "app.nextRegion", group: "help", label: () => t("commands.nextRegion"), keys: ["F6"], keyWhere: "app", inFields: true, surfaces: ["sheet"], run: () => cycleRegion(1) },
  { ...APP, id: "app.prevRegion", group: "help", label: () => t("commands.prevRegion"), keys: ["Shift+F6"], keyWhere: "app", inFields: true, surfaces: ["sheet"], run: () => cycleRegion(-1) },
  {
    ...APP, id: "app.undo", group: "help",
    label: () => t("commands.undo"), keys: ["Mod+Z"], keyOwner: "self",
    surfaces: ["sheet"],
  },
  {
    // The Automation editor's own key (an app command cannot run in a field, so the editor listens), listed for the sheet.
    ...APP, id: "automation.save", page: "automation", group: "help",
    label: () => t("commands.saveProject"), keys: ["Mod+S"], keyOwner: "self",
    surfaces: ["sheet"],
  },
  {
    // The Settings page's own key (an app command cannot run in a field or a dialog, so the page listens), listed for the sheet.
    ...APP, id: "settings.save", page: "settings", group: "help",
    label: () => t("commands.saveSettings"), keys: ["Mod+S"], keyOwner: "self",
    surfaces: ["sheet"],
  },
  {
    // The one New folder glyph, everywhere (owner feedback 5r).
    ...APP, id: "app.newFolder", group: "create", icon: FolderAddIconApp,
    focusAfter: true,
    label: () => t("commands.newFolder"),
    surfaces: ["palette", "pageMenu"],
    // A team's folders are its admin's (doors.rs folder_add).
    disabled: () => adminRefusal(),
    run: () => useProfileUi.getState().setFolderDialog({ mode: "create" }),
  },
  {
    ...APP, id: "app.whatsNew", group: "help", icon: NavPatchLogIcon,
    focusAfter: true,
    label: () => t("commands.whatsNew"),
    surfaces: ["palette"],
    run: () => useWhatsNew.getState().setOpen(true),
  },
  {
    // The full tour from step 1, from any page (it goes to Profiles first).
    // A guard dialog may open first (unsaved changes), then the tour's card:
    // both take the keyboard, so the command runs after the palette has closed (PC-108).
    ...APP, id: "app.tour", group: "help", icon: HelpCircleIcon,
    focusAfter: true,
    label: () => t("commands.takeTour"),
    keywords: () => ["tutorial", "welcome", "onboarding", "replay"],
    surfaces: ["palette"],
    run: () => { void takeTheTour(); },
  },
  {
    // The product site, in the system browser.
    ...APP, id: "app.website", group: "help", icon: GlobeIcon,
    label: () => t("commands.openWebsite"),
    keywords: () => ["website", "site", "web", "shardbrowser"],
    surfaces: ["palette"],
    run: () => { void openUrl(siteUrl("palette")).catch(() => {}); },
  },
  {
    // Every running browser at once, from any page (IFACE-10). Listed only
    // while one runs; the store's own toast names any that failed.
    ...APP, id: "app.stopAll", group: "run", icon: StopIcon,
    label: () => t("commands.stopAll"),
    surfaces: ["palette"],
    when: () => Object.keys(useProfile.getState().running).length > 0,
    run: async () => { await useProfile.getState().stopMany(Object.keys(useProfile.getState().running)); },
  },
  {
    // The editor's Tidy up: only while a project is open on the Automation page.
    ...APP, id: "app.tidyCanvas", scope: "page", page: "automation", group: "view", icon: NavAutomationIcon,
    label: () => t("commands.tidyCanvas"), keywords: () => ["tidy", "layout", "arrange", "align"],
    surfaces: ["palette"],
    when: () => useNav.getState().section === "automation" && useAutomation.getState().openId !== null,
    run: () => {
      const id = useAutomation.getState().openId;
      if (!id) return;
      void useAutomation.getState().tidy(id).then(() => {
        toast.undo(t("projectEditor.tidiedUp"), () => { void useAutomation.getState().undo(); });
      });
    },
  },
  {
    ...APP, id: "app.theme", group: "view", icon: SunIcon,
    label: () => t("commands.changeTheme"), keywords: THEME_WORDS,
    surfaces: ["palette"],
    // No run: the palette opens its three choices in place (CRAFT-07).
  },
  { ...APP, id: "app.themeLight", group: "view", icon: SunIcon, label: () => t("commands.themeLight"), keywords: THEME_WORDS, surfaces: ["palette"], run: () => chooseTheme("light") },
  { ...APP, id: "app.themeDark", group: "view", icon: MoonIcon, label: () => t("commands.themeDark"), keywords: THEME_WORDS, surfaces: ["palette"], run: () => chooseTheme("dark") },
  { ...APP, id: "app.themeSystem", group: "view", icon: ComputerIcon, label: () => t("commands.themeSystem"), keywords: THEME_WORDS, surfaces: ["palette"], run: () => chooseTheme("system") },
  // Go to: the nav labels (6.2: page names are the nav labels).
  {
    ...APP, id: "nav.profiles", group: "navigate", icon: NavBrowsersIcon,
    label: () => t("sidebar.allProfiles"), keys: ["Mod+0"], keyWhere: "app", inFields: true,
    surfaces: ["palette", "sheet"],
    run: () => { void leaveList(() => { useProfileUi.getState().setFolder("all"); void useNav.getState().setSection("profiles", { focus: true }); }); },
  },
  { ...APP, id: "nav.proxies", group: "navigate", icon: RouteIcon, label: () => t("sidebar.navProxies"), keys: ["G P"], keyWhere: "app", singleKey: true, surfaces: ["palette", "sheet"], run: go("proxies") },
  { ...APP, id: "nav.fingerprints", group: "navigate", icon: NavFingerprintsIcon, label: () => t("sidebar.navFingerprints"), keys: ["G F"], keyWhere: "app", singleKey: true, surfaces: ["palette", "sheet"], run: go("fingerprints") },
  { ...APP, id: "nav.extensions", group: "navigate", icon: NavExtensionsIcon, label: () => t("sidebar.navExtensions"), surfaces: ["palette"], run: go("extensions") },
  { ...APP, id: "nav.bookmarks", group: "navigate", icon: NavBookmarksIcon, label: () => t("sidebar.navBookmarks"), surfaces: ["palette"], run: go("bookmarks") },
  // The pages' names before F5 made titles equal the nav labels still find them.
  { ...APP, id: "nav.automation", group: "navigate", icon: NavAutomationIcon, label: () => t("sidebar.navProjects"), keywords: () => ["projects"], surfaces: ["palette"], run: go("automation") },
  { ...APP, id: "nav.proxyshard", group: "navigate", icon: NavShopIcon, label: () => t("sidebar.navAccountShop"), keywords: () => ["account", "shop", "proxyshard"], surfaces: ["palette"], run: go("proxyshard") },
  // In a team: for its admin, and for a member once they see rows of its Trash.
  { ...APP, id: "nav.trash", group: "navigate", icon: NavTrashIcon, label: () => t("sidebar.navTrash"), surfaces: ["palette"], when: () => { const team = activeTeam(useSpace.getState().state); return !team || team.admin || useTeamTrash.getState().seen; }, run: go("trash") },
  // The Team page has no row of its own: the space menu and these reach it.
  {
    ...APP, id: "nav.team", group: "navigate", icon: SpaceTeamIcon, label: () => t("team.pageTitle"),
    keywords: () => ["team", "members", "rights", "access", "admin"], surfaces: ["palette"], run: go("team"),
  },
  {
    ...APP, id: "team.create", group: "create", icon: SpaceTeamIcon, focusAfter: true,
    label: () => t("commands.createTeam"), keywords: () => ["team", "server"], surfaces: ["palette"],
    run: () => useCreateTeam.getState().setOpen(true),
  },
  {
    ...APP, id: "team.join", group: "create", icon: SpaceTeamIcon, focusAfter: true,
    label: () => t("commands.joinTeam"), keywords: () => ["team", "invite"], surfaces: ["palette"],
    run: () => useJoinTeam.getState().setOpen(true),
  },
  {
    // Only for the team's admin, in that team's space.
    ...APP, id: "team.addMember", group: "create", icon: AddIcon, focusAfter: true,
    label: () => t("commands.addTeamMember"), keywords: () => ["team", "invite", "member"], surfaces: ["palette"],
    when: () => !!activeTeam(useSpace.getState().state)?.admin,
    run: () => { useTeamAsk.getState().askAddMember(true); void useNav.getState().setSection("team"); },
  },
  {
    // Personal sync: a sync of everything now, while it is connected.
    ...APP, id: "sync.now", group: "run", icon: SyncedIcon,
    label: () => t("commands.syncNow"), keywords: () => ["sync", "devices", "cloud"], surfaces: ["palette"],
    when: () => syncConnected(),
    run: () => syncNow(),
  },
  { ...APP, id: "nav.settings", group: "navigate", icon: NavSettingsIcon, label: () => t("sidebar.navSettings"), keys: ["G S"], keyWhere: "app", singleKey: true, surfaces: ["palette", "sheet"], run: go("settings") },
  {
    ...APP, id: "nav.folders", group: "navigate",
    // The app key hook reads the digit (useAppKeys); listed for the sheet.
    label: () => t("commands.openFolders"), keys: ["Mod+1", "Mod+9"], keysJoin: "to", keyOwner: "self",
    surfaces: ["sheet"],
  },
  ...SETTINGS_TAB_IDS.map((tab): Command => ({
    ...APP, id: `settings.${tab}`, group: "settings", icon: NavSettingsIcon,
    label: () => SETTINGS_TABS[tab](),
    surfaces: ["palette"],
    // No Sync tab on a backend without sync commands.
    ...(tab === "sync" ? { when: () => !useSync.getState().missing } : {}),
    run: () => { void useNav.getState().openSettings(tab, { focus: true }); },
  })),
  // The palette's own keys, for the sheet and the palette's footer.
  PALETTE_KEYS.run, PALETTE_KEYS.openPeek, PALETTE_KEYS.actions, PALETTE_KEYS.move, PALETTE_KEYS.close,
];

defineCommands(APP_COMMANDS);
