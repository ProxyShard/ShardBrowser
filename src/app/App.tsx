import { lazy, Suspense, useEffect, useLayoutEffect, useRef, useState, type ReactNode } from "react";
import { cn } from "@proxyshard/shardx-ui-kit";
// Loading the definitions registers them: every surface finds its commands
// from the first render. The app's keys come from them too.
import { useAppKeys } from "../features/commands";
import { Sidebar } from "../widgets/Sidebar/Sidebar";
import { FirstRunGate } from "../widgets/FirstRunGate/FirstRunGate";
import { EngineUpdateBanner } from "../widgets/FirstRunGate/EngineUpdateBanner";
import { ToastHost } from "../widgets/ToastHost/ToastHost";
import { LiveAnnouncer } from "../widgets/LiveAnnouncer/LiveAnnouncer";
import { ConfirmHost } from "../widgets/ConfirmHost/ConfirmHost";
import { PasswordHost } from "../widgets/PasswordHost/PasswordHost";
import { UnsavedGuard } from "../widgets/UnsavedGuard/UnsavedGuard";
import { StarModal } from "../widgets/StarModal/StarModal";
import { HelperWatcher } from "../widgets/HelperWatcher";
import { WhatsNewGate } from "../widgets/WhatsNewGate";
import { PsTrialModal } from "../features/proxyshard";
import { ProfilesPage } from "../pages/profiles";
import { ShellWindowControls } from "../widgets/WindowControls/WindowControls";
import { AssignProxiesHost, FolderDialog } from "../features/manage-profiles";
import { ViewDialog } from "../features/manage-views";
import { ProfileFormDialog } from "../features/profile-form";
import { CommandPalette } from "../features/command-palette";
import { ShortcutSheet } from "../features/shortcuts";
import { TourEndCard, TourLayer, WelcomeDialog, usePageTips } from "../features/tour";
import { useNav } from "../shared/model/navigation";
import { useWhatsNew } from "../shared/model/whatsNew";
import { useT } from "../shared/i18n";
import type { Section } from "../shared/types";
import { useThemeBridge } from "../shared/model/theme";
import { reloadFolders, useProfile } from "../entities/profile";
import { useTags } from "../entities/tag";
import { useInTeam, wireAccess } from "../entities/space";
import { useSync } from "../entities/sync";
import { useExtensionUpdates, watchExtensionUpdates } from "../entities/extension";
import { watchConflictCopies } from "../features/profile-sync";
import { confirmOpenSessions, watchSaving, watchTeamSessions } from "../features/team-sessions";
import { useTeamTrash, useTeamTrashShown, wireTeamTrash } from "../features/team-trash";
import { trashList } from "../entities/trash";
import { useStoreChanged } from "../shared/hooks/useStoreChanged";
import { useQuitGuard } from "../shared/hooks/useQuitGuard";
import { useLauncherWarning } from "../shared/hooks/useLauncherWarning";
import { trackSection } from "../shared/lib/analytics";
import { focusIsLost, focusPageStart, landOnPage } from "../shared/lib/focus";
import { shellPadClass, titleBarOnMouseDown } from "../shared/lib/dragWindow";
import { storeBus } from "../shared/lib/storeBus";
import { PageBody, PageHeader } from "../widgets/PageHeader";
import { TeamStatusBanner } from "../widgets/TeamStatusBanner";
import { LoadingCards, LoadingRows } from "../shared/ui/ListStates";
import { whenIdle } from "../shared/lib/idle";
import "./wireStores";

// The startup chunk is the shell and the Profiles page; every other page,
// and What's new with its release notes (80 KB of JSON), is a chunk of its
// own, loaded when first asked for (speed lane 2026-10-02: the launcher's
// startup script was 1,052 KB with every page in it). Nothing waits on them
// in practice: the pointer or the keyboard on a sidebar item fetches that
// page's code ahead (`useWarmPages`), and once the first rows are on screen
// and the app sits idle, every page is fetched in turn (`usePreloadPages`).
// A page asked for before its code is in shows its own header over its
// loading look in its place (`SectionLoading`), which the loading look
// itself holds back for its first moments, so a quick load shows the header
// only.
const loadProxies = () => import("../pages/proxies");
const loadProxyShard = () => import("../pages/proxyshard");
const loadFingerprints = () => import("../pages/fingerprints");
const loadExtensions = () => import("../pages/extensions");
const loadBookmarks = () => import("../pages/bookmarks");
const loadTrash = () => import("../pages/trash");
const loadSettings = () => import("../pages/settings");
const loadWhatsNew = () => import("../widgets/WhatsNewDialog");
const loadAutomation = () => import("../pages/automation");
const loadTeam = () => import("../pages/team");
const ProxiesPage = lazy(() => loadProxies().then((m) => ({ default: m.ProxiesPage })));
const ProxyShardPage = lazy(() => loadProxyShard().then((m) => ({ default: m.ProxyShardPage })));
const FingerprintsPage = lazy(() => loadFingerprints().then((m) => ({ default: m.FingerprintsPage })));
const ExtensionsPage = lazy(() => loadExtensions().then((m) => ({ default: m.ExtensionsPage })));
const BookmarksPage = lazy(() => loadBookmarks().then((m) => ({ default: m.BookmarksPage })));
const TrashPage = lazy(() => loadTrash().then((m) => ({ default: m.TrashPage })));
const SettingsPage = lazy(() => loadSettings().then((m) => ({ default: m.SettingsPage })));
const WhatsNewDialog = lazy(() => loadWhatsNew().then((m) => ({ default: m.WhatsNewDialog })));
const AutomationPage = lazy(() => loadAutomation().then((m) => ({ default: m.AutomationPage })));
const TeamPage = lazy(() => loadTeam().then((m) => ({ default: m.TeamPage })));
const WARM: Partial<Record<string, () => Promise<unknown>>> = {
  proxies: loadProxies,
  proxyshard: loadProxyShard,
  fingerprints: loadFingerprints,
  extensions: loadExtensions,
  bookmarks: loadBookmarks,
  trash: loadTrash,
  settings: loadSettings,
  "whats-new": loadWhatsNew,
  automation: loadAutomation,
  // The space switcher's menu opens the Team page.
  space: loadTeam,
};
/** The order the idle preload fetches them in: the pages most sessions open first. */
const PRELOAD = [loadProxies, loadFingerprints, loadExtensions, loadBookmarks, loadProxyShard, loadTrash, loadSettings, loadAutomation];

/** What each lazy page shows while its code loads: its own header (so its
 *  title and heading are there from the first frame) over its own loading
 *  look, cards or rows, at its own width. */
const LOADING: Partial<Record<Section, { title: (t: ReturnType<typeof useT>) => string; look: "rows" | "cards" | "form"; width?: "form" }>> = {
  proxies: { title: (t) => t("proxies.title"), look: "rows" },
  proxyshard: { title: () => "ProxyShard", look: "rows" },
  fingerprints: { title: (t) => t("fingerprints.title"), look: "cards" },
  extensions: { title: (t) => t("extensions.title"), look: "cards" },
  bookmarks: { title: (t) => t("bookmarks.heading"), look: "rows" },
  trash: { title: (t) => t("trash.title"), look: "rows" },
  settings: { title: (t) => t("settings.title"), look: "form" },
  automation: { title: (t) => t("automation.title"), look: "cards" },
  team: { title: (t) => t("team.pageTitle"), look: "rows" },
};

function SectionLoading({ section }: { section: Section }) {
  const t = useT();
  const page = LOADING[section];
  if (!page) return null;
  // `data-page-loading`: the page is not its own yet (tests wait it out). A
  // box that is not one (`contents`): the header and body sit in the page's
  // column as the page's own do.
  return (
    <div data-page-loading className="contents">
      <PageHeader title={page.title(t)} width={page.width} />
      <PageBody width={page.width}>
        {page.look === "cards" ? <LoadingCards /> : page.look === "form" ? <LoadingRows rows={4} rowHeight={56} /> : <LoadingRows />}
      </PageBody>
    </div>
  );
}

/** A lazy page in its place: its loading look until its code is in, then the
 *  page, with the keyboard handed on if the loading header held it. */
function LazyPage({ section, children }: { section: Section; children: ReactNode }) {
  return (
    <Suspense fallback={<SectionLoading section={section} />}>
      {children}
      <KeepKeyboard section={section} />
    </Suspense>
  );
}

/** Mounts with the page that replaced a fallback: a title that held the
 *  keyboard left with it, so the keyboard goes to the page's own title. */
function KeepKeyboard({ section }: { section: Section }) {
  useLayoutEffect(() => { if (focusIsLost()) focusPageStart(section); }, [section]);
  return null;
}

/** What's new, mounted once first opened and kept from then on, so it closes with its animation. */
function WhatsNewHost() {
  const open = useWhatsNew((s) => s.open);
  const [wanted, setWanted] = useState(open);
  if (open && !wanted) setWanted(true);
  return wanted ? <Suspense fallback={null}><WhatsNewDialog /></Suspense> : null;
}

/** Every lazy page's code, fetched one at a time while the app sits idle,
 *  once the Profiles list has its rows and a moment has passed: so a page
 *  opened later never waits, while the first rows and the first moves in
 *  them (a scroll, Peek) have the CPU to themselves. */
function usePreloadPages() {
  const ready = useProfile((s) => s.status === "ready" || s.status === "error");
  useEffect(() => {
    if (!ready) return;
    // A chunk's code runs in one piece when it arrives: never while the
    // person types, clicks or scrolls (a key's next frame came late once
    // in the perf budget's filter typing), only after a quiet moment.
    let lastInput = 0;
    const onInput = () => { lastInput = performance.now(); };
    const opts = { capture: true, passive: true } as const;
    for (const type of INPUT_EVENTS) window.addEventListener(type, onInput, opts);
    let timer: ReturnType<typeof setTimeout> | undefined;
    let cancel = () => {};
    let i = 0;
    const next = () => {
      if (i >= PRELOAD.length) return;
      const quietFor = performance.now() - lastInput;
      if (quietFor < PRELOAD_QUIET_MS) { timer = setTimeout(next, PRELOAD_QUIET_MS - quietFor); return; }
      cancel = whenIdle(() => {
        if (performance.now() - lastInput < PRELOAD_QUIET_MS) { next(); return; }
        void PRELOAD[i++]().catch(() => {}).then(next);
      }, 2_000);
    };
    timer = setTimeout(next, PRELOAD_AFTER_MS);
    return () => {
      clearTimeout(timer);
      cancel();
      for (const type of INPUT_EVENTS) window.removeEventListener(type, onInput, opts);
    };
  }, [ready]);
}
/** How long after the first rows the idle preload waits before it starts. */
const PRELOAD_AFTER_MS = 2_000;
/** How long no key, press or scroll must have come before a chunk loads. */
const PRELOAD_QUIET_MS = 1_000;
const INPUT_EVENTS = ["keydown", "pointerdown", "wheel", "scroll"] as const;

/** Fetches a lazy part's code when the pointer or the focus reaches its sidebar item (`data-nav`). */
function useWarmPages() {
  useEffect(() => {
    const warm = (e: Event) => {
      const id = e.target instanceof Element ? e.target.closest("[data-nav]")?.getAttribute("data-nav") : null;
      if (id) void WARM[id]?.();
    };
    document.addEventListener("pointerover", warm, { passive: true });
    document.addEventListener("focusin", warm);
    return () => {
      document.removeEventListener("pointerover", warm);
      document.removeEventListener("focusin", warm);
    };
  }, []);
}

export function App() {
  const section = useNav((s) => s.section);

  useEffect(() => { void trackSection(section); }, [section]);
  useLauncherWarning();
  // The profile list loads while the engine check runs, not after it (PERF-18):
  // it reads files only. The fingerprint library is read again after a first
  // download (the gate raises "fingerprints").
  const initProfiles = useProfile((s) => s.init);
  useEffect(() => { void initProfiles().then(forgetDeletedProfileTags); }, [initProfiles]);

  return (
    <>
      {/* Before the engine gate and the shell: the live regions exist from the first frame. */}
      <LiveAnnouncer />
      <HelperWatcher />
      <WhatsNewGate />
      <FirstRunGate>
        <Shell />
      </FirstRunGate>
    </>
  );
}

/// Sidebar and page. Mounts only once the engine and its fingerprint library
/// are installed; the profile list has been loading since `App` mounted.
function Shell() {
  const section = useNav((s) => s.section);
  const focusSeq = useNav((s) => s.focusSeq);
  // A page opened from the keyboard (a key, the palette, the sidebar by
  // keyboard) gets the keyboard once it has rendered; any other page change
  // gives it the keyboard only when the control that changed the page left
  // with the old page (A11Y-04, ruling 18). A pointer click on a sidebar
  // item keeps the keyboard on the item.
  const seen = useRef({ section, focusSeq });
  useEffect(() => {
    const prev = seen.current;
    seen.current = { section, focusSeq };
    const force = focusSeq !== prev.focusSeq;
    if (!force && section === prev.section) return;
    return landOnPage(section, force);
  }, [section, focusSeq]);

  // The sidebar shows Folder and running counts on every page, so process
  // status and reloads live here rather than on the Profiles page.
  const reloadProfiles = useProfile((s) => s.reload);
  const startProcessPolling = useProfile((s) => s.startProcessPolling);
  // Real child-process status: anchors the uptime clock, refreshes totals.
  useEffect(() => startProcessPolling(), [startProcessPolling]);
  // Personal sync, for the profile menus and the palette; it follows "sync-changed".
  useEffect(() => { void useSync.getState().load().catch(() => {}); }, []);
  // A sync's conflict copy says so once, with a way to it.
  useEffect(() => watchConflictCopies(), []);
  // What became of a team profile's session as it closed: saved, kept, or not kept.
  useEffect(() => watchTeamSessions(), []);
  // A closed one whose session is still being sent says Saving…, and Start waits.
  useEffect(() => watchSaving(), []);
  // Profiles created or moved through the automation API or MCP show up live.
  // Only profile and proxy changes reload this list (PERF-22): the other
  // topics (extensions, bookmarks, trash, automation) have nothing it shows.
  useStoreChanged(reloadProfiles, ["profiles", "proxies"]);
  // A profile trashed or restored there changes the Trash too: the palette
  // reads it again as it next opens (Pair 45 review, I2).
  useStoreChanged(() => storeBus.emit("trash", "ui"), ["trash", "profiles"]);
  // Tags and folders changed elsewhere (a team's member, the Local API) are read again.
  useStoreChanged(() => void useTags.getState().reload(), ["tags"]);
  useStoreChanged(() => void reloadFolders(), ["folders"]);
  // Web Store updates of extensions: the sidebar's count, Peek and the page read them.
  useEffect(() => watchExtensionUpdates(), []);
  useStoreChanged(() => void useExtensionUpdates.getState().load(), ["extension-updates", "extensions"]);
  // A profile that closes shortens what an update waits on; the backend says
  // nothing then. Read again once its browser has had time to exit.
  useEffect(() => {
    const timers: ReturnType<typeof setTimeout>[] = [];
    const off = useProfile.subscribe((s, was) => {
      const closed = Object.keys(was.running).some((id) => !s.running[id]);
      const waits = Object.values(useExtensionUpdates.getState().byId).some((u) => u.state === "waiting");
      if (closed && waits) for (const ms of [1500, 5000]) timers.push(setTimeout(() => void useExtensionUpdates.getState().load(), ms));
    });
    return () => { off(); timers.forEach(clearTimeout); };
  }, []);
  // What this person may do in the team in use, read as the space and its permission move.
  useEffect(() => wireAccess(), []);
  // A team's Trash is its server's: read as the space moves and as the team tells of a change.
  useEffect(() => wireTeamTrash(), []);
  // Entering a team whose Trash this person sees nothing of leaves its page.
  const team = useInTeam();
  const teamTrash = useTeamTrashShown();
  const trashRead = useTeamTrash((s) => s.status !== "idle" && s.status !== "loading");
  useEffect(() => {
    if (team && section === "trash" && !teamTrash && trashRead) void useNav.getState().setSection("profiles");
  }, [team, section, teamTrash, trashRead]);
  useAppKeys();
  // Quit (tray, Cmd+Q) asks about unsaved changes, then names the team and synced profiles it closes.
  useQuitGuard(confirmOpenSessions);
  useThemeBridge();
  useWarmPages();
  usePreloadPages();
  // The first visit to a page plays that page's short tip (onboarding spec 6).
  usePageTips();

  // The sidebar and the page on the gray ground, 8 px apart and 8 px from
  // the window's edges (on macOS a 38 px title strip on top holds the
  // traffic lights: shellPadClass), with no line between them (borderless, owner
  // 2026-10-01). In light the page is the ground itself and its content
  // white tiles on it (light flip: `--bg-page`, `page-ground`); in dark it
  // is a panel. The ground between and around them is the window's frame:
  // a press there drags the window, as the title bars do (only on the
  // ground itself, never on what a panel holds).
  return (
    <div
      data-shell
      className={cn("relative grid h-full grid-cols-[auto_minmax(0,1fr)] gap-panel overflow-hidden bg-bg-canvas", shellPadClass())}
      onMouseDown={(e) => { if (e.target === e.currentTarget) titleBarOnMouseDown(e); }}
    >
      <Sidebar />
      <main data-section={section} className="page-ground flex min-w-0 flex-col overflow-hidden rounded-panel bg-bg-page">
        <TeamStatusBanner />
        <EngineUpdateBanner />
        {section === "profiles" && <ProfilesPage />}
        {section === "proxies" && <LazyPage section="proxies"><ProxiesPage /></LazyPage>}
        {section === "proxyshard" && <LazyPage section="proxyshard"><ProxyShardPage /></LazyPage>}
        {section === "fingerprints" && <LazyPage section="fingerprints"><FingerprintsPage /></LazyPage>}
        {section === "extensions" && <LazyPage section="extensions"><ExtensionsPage /></LazyPage>}
        {section === "bookmarks" && <LazyPage section="bookmarks"><BookmarksPage /></LazyPage>}
        {section === "automation" && <LazyPage section="automation"><AutomationPage /></LazyPage>}
        {section === "trash" && <LazyPage section="trash"><TrashPage /></LazyPage>}
        {section === "settings" && <LazyPage section="settings"><SettingsPage /></LazyPage>}
        {section === "team" && <LazyPage section="team"><TeamPage /></LazyPage>}
      </main>
      <ToastHost />
      <ConfirmHost />
      <UnsavedGuard />
      <StarModal />
      <WhatsNewHost />
      <FolderDialog />
      <AssignProxiesHost />
      <ViewDialog />
      <ProfileFormDialog />
      {/* After the profile form: a protected profile's password is asked over it. */}
      <PasswordHost />
      <CommandPalette />
      <ShortcutSheet />
      <WelcomeDialog />
      <PsTrialModal />
      <TourLayer />
      <TourEndCard />
      <ShellWindowControls />
    </div>
  );
}

/// Tags of profiles deleted for good without the app seeing it (the Trash
/// expiry sweep at start, another client) are dropped once the list has
/// loaded: ids neither in the list nor in the Trash will never come back.
async function forgetDeletedProfileTags() {
  if (useProfile.getState().status !== "ready") return;
  try {
    const trashed = await trashList();
    const known = new Set([...useProfile.getState().profiles.map((p) => p.id), ...trashed.map((e) => e.id)]);
    const gone = Object.keys(useTags.getState().byProfile).filter((id) => !known.has(id));
    if (gone.length) useTags.getState().forgetProfiles(gone);
  } catch { /* kept until the next start */ }
}
