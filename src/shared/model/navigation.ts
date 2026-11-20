import { create } from "zustand";
import { SECTIONS, type Section } from "../types";
import { confirmLeave, hasUnsaved } from "./unsaved";

/** The parts of the Settings page. */
export type SettingsTab = "general" | "appearance" | "profiles" | "templates" | "tags" | "views" | "api" | "helper" | "storage" | "sync" | "other" | "about";

/** `focus`: the page was asked for from the keyboard; it gets the keyboard once it renders. */
export type NavOptions = { focus?: boolean };

/// App-level navigation state (active sidebar section).
type NavState = {
  section: Section;
  /** Resolves false when the person stayed (the unsaved-changes guard's
   *  Cancel, or a save that failed): a caller with more to do after the page
   *  change awaits it and stops. With nothing to ask, the page has changed
   *  before it returns. */
  setSection: (section: Section, opts?: NavOptions) => Promise<boolean>;
  settingsTab: SettingsTab;
  /** Opens Settings, on the given tab if one is named. Resolves as `setSection`. */
  openSettings: (tab?: SettingsTab, opts?: NavOptions) => Promise<boolean>;
  /** Bumped when a page change should put the keyboard on the new page: a
   *  key, the palette, a keyboard press in the sidebar (A11Y-04, E5). The
   *  Shell reads it. */
  focusSeq: number;
};

// The section lives in `?section=` too, so a reload or a test opens the same
// page. Unknown values fall back to Profiles.
function sectionFromUrl(): Section {
  const value = new URLSearchParams(window.location.search).get("section");
  return (SECTIONS as readonly string[]).includes(value ?? "") ? (value as Section) : "profiles";
}

// The Settings tab lives in `?tab=` beside it (WIG-06): a reload opens the
// same tab. The record makes tsc check this list against the type.
const SETTINGS_TABS: Record<SettingsTab, true> = {
  general: true, appearance: true, profiles: true, templates: true, tags: true, views: true, api: true, helper: true, storage: true, sync: true, other: true, about: true,
};

function tabFromUrl(): SettingsTab {
  const value = new URLSearchParams(window.location.search).get("tab") ?? "";
  return (Object.keys(SETTINGS_TABS) as string[]).includes(value) ? (value as SettingsTab) : "general";
}

function writeToUrl(section: Section, tab?: SettingsTab) {
  const url = new URL(window.location.href);
  url.searchParams.set("section", section);
  if (section === "settings" && tab) url.searchParams.set("tab", tab);
  else url.searchParams.delete("tab");
  history.replaceState(history.state, "", url.href);
}

/** Runs `go` once leaving the page may drop nothing: at once with nothing
 *  unsaved (every caller's order stays as it was), else after the guard's
 *  answer, and not at all on Cancel or a failed save. Staying on the page
 *  (the same section, another Settings tab) is no exit. True when `go` ran. */
function leave(stays: boolean, go: () => void): Promise<boolean> {
  if (stays || !hasUnsaved()) {
    go();
    return Promise.resolve(true);
  }
  return confirmLeave().then((ok) => {
    if (ok) go();
    return ok;
  });
}

export const useNav = create<NavState>((set, get) => ({
  section: sectionFromUrl(),
  setSection: (section, opts) => leave(section === get().section, () => {
    writeToUrl(section, section === "settings" ? get().settingsTab : undefined);
    set((s) => ({ section, focusSeq: opts?.focus ? s.focusSeq + 1 : s.focusSeq }));
  }),
  settingsTab: tabFromUrl(),
  openSettings: (tab, opts) => leave(get().section === "settings", () => {
    writeToUrl("settings", tab ?? get().settingsTab);
    set((s) => ({ section: "settings", settingsTab: tab ?? s.settingsTab, focusSeq: opts?.focus ? s.focusSeq + 1 : s.focusSeq }));
  }),
  focusSeq: 0,
}));
