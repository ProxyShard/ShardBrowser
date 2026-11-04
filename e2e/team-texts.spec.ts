import { expect, test, type Page } from "@playwright/test";
import { appModule, openApp } from "./support/app";
import { ADMIN_INVITE, joinTeam, menu, space, toast } from "./support/team";
import { TEAM_WARNINGS } from "../src/shared/api/backendWarnings";
import { TEAM_REFUSALS } from "../src/shared/api/teamRefusals";
import { SYNC_STOPS } from "../src/entities/sync/model/types";

// The backend's new words of team steps 5 to 10 reach a person translated, never raw
// (team phase C): its launcher warnings, a refused switch of space, a stopped sync.

type M = { __shardxMock: { emit: (e: string, p?: unknown) => void } };
type T = typeof import("../src/shared/model/toast");

const warn = (page: Page, text: string) => page.evaluate((t) => (window as unknown as M).__shardxMock.emit("launcher-warning", t), text);
const clearToasts = (page: Page) => page.evaluate(() => (window as unknown as { __toasts: T }).__toasts.useToastStore.setState({ items: [] }));

/** A warning as the backend words it, and the title and line the toast says instead. */
const WARNINGS: [string, string, string][] = [
  [TEAM_WARNINGS.quitting, "Saving the open team, synced and password-protected profiles before quitting…", ""],
  [TEAM_WARNINGS.withdrew("Ads team"), "“Ads team” withdrew your access", "You can only remove the team from the space menu now."],
  [TEAM_WARNINGS.withdrew("A team"), "A team withdrew your access", "Its profiles open here were closed."],
  [TEAM_WARNINGS.replaced("Ads team"), "Your invite to “Ads team” was replaced", "paste it in “Join a team”"],
  [TEAM_WARNINGS.permission("the team server did not answer: timed out"), "The team’s permission couldn’t be checked", "The team server isn’t answering"],
  [TEAM_WARNINGS.anotherTeam("Ads team", "http://ads.example.net:8787"), "“Ads team” answers as a different team now", "The server at http://ads.example.net:8787 may have been started without its data."],
  [TEAM_WARNINGS.closesSoon("FB Ads 01", 4), "The team server isn’t answering", "“FB Ads 01” closes in 4 min unless it answers."],
  [TEAM_WARNINGS.tooBig("FB Ads 01", 3), "Part of “FB Ads 01” wasn’t saved", "stored data from 3 of its sites was left out"],
  [TEAM_WARNINGS.localInvite("127.0.0.1:8787"), "This invite works only on this computer", "It starts with 127.0.0.1:8787."],
  [TEAM_WARNINGS.projectConflict("Warm-up"), "“Warm-up” was changed on another computer too", "the changes made here are in “Warm-up (conflict copy)”"],
  [TEAM_WARNINGS.proxyChanged("US 1"), "Proxy “US 1” was changed on another computer too", "That computer’s version is kept."],
  [TEAM_WARNINGS.proxySetDifferently("px-us-1"), "A proxy was set differently on another computer", "That computer’s version is kept."],
  [TEAM_WARNINGS.itemSetDifferently("bookmark"), "A bookmark was set differently on another computer", "That computer’s version is kept."],
  [TEAM_WARNINGS.itemChanged("Docs"), "“Docs” was changed on another computer too", "That computer’s version is kept."],
  [TEAM_WARNINGS.resending("FB Ads 01", "the team server did not answer: timed out"), "“FB Ads 01” isn’t saved to the team yet", "sent again until the server takes it"],
  [TEAM_WARNINGS.unsent("FB Ads 01", "the team server did not take it in time"), "“FB Ads 01” is saved on this computer", "It goes to the team when ShardBrowser starts next."],
  [TEAM_WARNINGS.clockAhead(7, "FB Ads 01"), "This computer’s clock is 7 min ahead", "Set the clock right, or “FB Ads 01” may close"],
  [TEAM_WARNINGS.extensionsMissing("FB Ads 01", 2), "“FB Ads 01” started without 2 of its extensions", "The team server didn’t send them to this computer."],
];

test("a warning about team and synced profiles is said in plain words", async ({ page }) => {
  await openApp(page);
  await appModule(page, "/src/shared/model/toast.ts", "__toasts");
  for (const [raw, title, line] of WARNINGS) {
    await warn(page, raw);
    const shown = toast(page, title);
    await expect(shown, raw).toBeVisible();
    if (line) await expect(shown, raw).toContainText(line);
    await expect(page.locator("[data-toast]", { hasText: "A profile started with a warning" }), raw).toHaveCount(0);
    await clearToasts(page);
  }
  // A synced profile's conflict copy is said by the list, with a way to it: not here again.
  await warn(page, TEAM_WARNINGS.profileConflict("Shop 1"));
  await page.waitForTimeout(300);
  await expect(page.locator("[data-toast]")).toHaveCount(0);
});

test("a switch of space refused while a profile starts says so in plain words", async ({ page }) => {
  await openApp(page);
  await joinTeam(page, ADMIN_INVITE);
  await expect(space(page)).toHaveText("Ads team");
  await page.evaluate((w) => { (window as unknown as { __shardxMockFailWith: Record<string, string> }).__shardxMockFailWith = { space_set: w }; }, TEAM_REFUSALS.switchWhileStarting);
  await space(page).click();
  await menu(page).getByRole("menuitemradio", { name: "Personal" }).click();
  await expect(toast(page, "Couldn’t switch to “Personal”")).toContainText("A profile is starting. Switch spaces once it has started.");
  await expect(space(page)).toHaveText("Ads team");
});

test("a sync stopped by what this computer could not read says so in plain words", async ({ page }) => {
  await page.addInitScript(() => { (window as unknown as { __shardxMockSync?: string }).__shardxMockSync = "connected"; });
  await openApp(page, { section: "settings" });
  await page.getByRole("tablist", { name: "Settings sections" }).getByRole("tab", { name: "Sync", exact: true }).click();
  const status = page.locator("[data-setting='sync-status']");
  const stop = (words: string) => page.evaluate((w) => { (window as unknown as { __shardxMockSyncError?: string }).__shardxMockSyncError = w; }, words);
  await stop(SYNC_STOPS.vanished);
  await expect(status.locator("[data-sync-status]")).toHaveText("Sync stopped");
  await expect(status).toContainText("None of the synced profiles is on this computer any more, so nothing was deleted from the server.");
  await stop(SYNC_STOPS.unread("the profiles", "Permission denied (os error 13)"));
  await expect(status).toContainText("Some data on this computer couldn’t be read, so nothing was sent or deleted.");
  await expect(status).not.toContainText("os error 13");
});
