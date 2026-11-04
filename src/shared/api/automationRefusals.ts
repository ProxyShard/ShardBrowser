/// The automation studio's start refusals in the backend's own words (lib.rs, space/mod.rs):
/// errors.ts reads them back. Kept free of imports, so node tests read it as it is.

export const AUTOMATION_REFUSALS = {
  /** Open without automation; `automation_launch` with `restart` closes it and starts it again (OPEN_WITHOUT_AUTOMATION). */
  openWithout: "this profile is open without automation: it has to start again to be driven",
  /** A restart's browser that would not close (DID_NOT_CLOSE). */
  didNotClose: "the profile's browser did not close: close its window, then start it here again",
  /** Started, but no debugging port came (NO_DEBUG_PORT). */
  noDebugPort: "the browser started, but it did not report its debugging port in time",
  /** Running with automation, but a run's session drives it (DRIVEN_BY_RUN). */
  drivenByRun: "a project run is driving this profile: wait for it to finish, or stop it",
  /** A door that drives the browser, on a profile not running with automation (space::cdp_pass). */
  notAutomated: "that profile is not running with automation on",
} as const;

/** The studio's start was refused because the profile is already open without automation. */
export const isOpenWithoutAutomation = (e: unknown): boolean =>
  String(e instanceof Error ? e.message : e).includes(AUTOMATION_REFUSALS.openWithout);
