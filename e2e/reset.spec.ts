import { expect, test, type Page } from "@playwright/test";
import { openApp, waitForIdle } from "./support/app";

// Settings > Storage > Delete all data (owner, 2026-10-08): a fresh start
// behind two warnings, with nothing deleted. The mock's app_reset_plan counts
// what would move; app_reset empties its state the way reset.rs moves the
// data into a backup folder, and the page reloads on Restart.

type Mock = {
  __shardxMock: {
    calls: Record<string, number>;
    state: {
      profiles: Record<string, unknown>; proxies: unknown[]; bookmarks: unknown[]; automation: unknown[];
      extensions: unknown[]; psKey: string; fingerprints: { builtin: boolean }[];
      tags?: unknown; folders?: unknown;
    };
  };
};

const BACKUP = "/Users/you/Library/Application Support/shardx-launcher-backup-2026-09-01-120000";

const storage = async (page: Page) => {
  await openApp(page, { section: "settings" });
  await page.getByRole("tablist", { name: "Settings sections" }).getByRole("tab", { name: "Storage" }).click();
};
const trigger = (page: Page) => page.locator("[data-setting='delete-all-data']").getByRole("button", { name: "Delete all data" });
const first = (page: Page) => page.getByRole("alertdialog", { name: "Delete all data?" });
const second = (page: Page) => page.getByRole("alertdialog", { name: "Type DELETE to confirm" });
const resets = (page: Page) => page.evaluate(() => (window as unknown as Mock).__shardxMock.calls.app_reset ?? 0);
const counts = (page: Page) => page.evaluate(() => {
  const s = (window as unknown as Mock).__shardxMock.state;
  return {
    profiles: Object.keys(s.profiles).length, proxies: s.proxies.length, extensions: s.extensions.length,
    bookmarks: s.bookmarks.length, projects: s.automation.length,
  };
});

test("Delete all data counts what goes, asks twice, keeps a backup and starts over", async ({ page }) => {
  await storage(page);
  await page.evaluate(() => { localStorage.setItem("shardx.e2eMarker", "1"); localStorage.setItem("elsewhere.keep", "1"); });
  // tags.json and folders.json, which go to the backup with the settings.
  await page.evaluate(() => Object.assign((window as unknown as Mock).__shardxMock.state, {
    tags: { tags: [{ id: "t-vip", name: "VIP", color: "violet" }], by_profile: {} },
    folders: { registry: ["Empty1"], meta: { Empty1: { color: "green", icon: "cart" } } },
  }));
  const row = page.locator("[data-setting='delete-all-data']");
  await expect(row).toContainText("Moves every profile, proxy and setting out of ShardBrowser into a backup folder. The browser engine stays installed.");
  // No reset yet: no backup to show.
  await expect(page.locator("[data-setting='reset-backup']")).toHaveCount(0);
  const n = await counts(page);

  await trigger(page).click();
  const warn = first(page);
  await expect(warn).toBeVisible();
  await expect(warn.getByRole("button", { name: "Cancel" })).toBeFocused();
  // The counts come from the plan the reset will be held to.
  for (const item of [
    `${n.profiles} profiles, with their cookies and browser data`, `${n.proxies} proxies`, `${n.extensions} extensions`,
    `${n.bookmarks} bookmarks`, `${n.projects} automation projects`, "Settings and your ShardX API key",
  ]) {
    await expect(warn.getByRole("listitem").filter({ hasText: item })).toHaveCount(1);
  }
  await expect(warn).toContainText("Nothing is deleted.");
  await expect(warn.locator("[data-delete-all-backup]")).toHaveText(`Your old data is kept in ${BACKUP}. Delete that folder when you no longer need it.`);
  await expect(warn).toContainText("Open profiles and automation runs will be closed.");
  await expect(warn.locator("[data-delete-all-unsaved]")).toHaveCount(0);
  await warn.getByRole("button", { name: "Continue" }).click();

  const type = second(page);
  await expect(type).toBeVisible();
  await expect(first(page)).toHaveCount(0);
  await expect(type.getByRole("button", { name: "Cancel" })).toBeFocused();
  const confirm = type.getByRole("button", { name: "Delete all data" });
  const field = type.getByRole("textbox", { name: "Type DELETE" });
  await expect(confirm).toBeDisabled();
  // Enter does nothing until the word is typed, exactly.
  await field.fill("delete");
  await field.press("Enter");
  await expect(confirm).toBeDisabled();
  await expect(type).toBeVisible();
  expect(await resets(page)).toBe(0);
  await field.fill("DELETE");
  await expect(confirm).toBeEnabled();

  // Held long enough to see the window's state while the data moves.
  await page.evaluate(() => Object.assign(window, { __shardxMockSlow: { app_reset: 1500 } }));
  await confirm.click();
  const whole = page.locator("[data-deleting-all]");
  await expect(whole).toContainText("Moving your data to the backup folder…");
  // The end says where the old data is, and waits for Restart.
  await expect(whole).toHaveAttribute("data-deleting-all", "done", { timeout: 10_000 });
  await expect(whole.locator("[data-delete-all-backup]")).toHaveText(`Your old data is kept in ${BACKUP}. Delete that folder when you no longer need it.`);
  const restart = whole.getByRole("button", { name: "Restart ShardBrowser" });
  await expect(restart).toBeFocused();

  const reloaded = page.waitForEvent("load", { timeout: 15_000 });
  await restart.click();
  await reloaded;
  await expect.poll(() => page.evaluate(() => "__shardxMock" in window), { timeout: 15_000 }).toBe(true);
  await waitForIdle(page);

  const after = await page.evaluate(() => {
    const s = (window as unknown as Mock).__shardxMock.state;
    return {
      profiles: Object.keys(s.profiles).length, proxies: s.proxies.length, bookmarks: s.bookmarks.length,
      projects: s.automation.length, psKey: s.psKey, imported: s.fingerprints.filter((f) => !f.builtin).length,
      library: s.fingerprints.length, marker: localStorage.getItem("shardx.e2eMarker"), other: localStorage.getItem("elsewhere.keep"),
      tags: s.tags ?? null, folders: s.folders ?? null,
    };
  });
  expect(after).toMatchObject({ profiles: 0, proxies: 0, bookmarks: 0, projects: 0, psKey: "", imported: 0, marker: null, other: "1", tags: null, folders: null });
  expect(after.library).toBeGreaterThan(0);

  // Settings > Storage now has a quiet way to the backup.
  await page.getByRole("tablist", { name: "Settings sections" }).getByRole("tab", { name: "Storage" }).click();
  const backupRow = page.locator("[data-setting='reset-backup']");
  await expect(backupRow).toContainText(BACKUP);
  await expect(backupRow.getByRole("button", { name: "Show backup folder" })).toBeVisible();
});

test("Cancel at either warning moves nothing and gives the keyboard back", async ({ page }) => {
  await storage(page);
  const before = await counts(page);
  await trigger(page).click();
  await first(page).getByRole("button", { name: "Cancel" }).click();
  await expect(first(page)).toHaveCount(0);
  await expect(trigger(page)).toBeFocused();

  await trigger(page).click();
  await first(page).getByRole("button", { name: "Continue" }).click();
  await second(page).getByRole("textbox", { name: "Type DELETE" }).fill("DELETE");
  await page.keyboard.press("Escape");
  await expect(second(page)).toHaveCount(0);
  await expect(trigger(page)).toBeFocused();

  // It starts over: the word is asked for again.
  await trigger(page).click();
  await first(page).getByRole("button", { name: "Continue" }).click();
  await expect(second(page).getByRole("textbox", { name: "Type DELETE" })).toHaveValue("");
  await second(page).getByRole("button", { name: "Cancel" }).click();

  expect(await resets(page)).toBe(0);
  expect(await counts(page)).toEqual(before);
});

test("the first warning names unsaved changes", async ({ page }) => {
  await openApp(page, { section: "settings" });
  await page.getByRole("switch", { name: /tray/i }).click();
  await expect(page.locator("[data-save-bar]")).toContainText("Unsaved changes");
  await page.getByRole("tablist", { name: "Settings sections" }).getByRole("tab", { name: "Storage" }).click();
  await trigger(page).click();
  await expect(first(page).locator("[data-delete-all-unsaved]")).toHaveText("Unsaved changes will be lost too.");
});

test("data that changed after the counts were shown is counted again, not moved", async ({ page }) => {
  await storage(page);
  await trigger(page).click();
  await first(page).getByRole("button", { name: "Continue" }).click();
  await second(page).getByRole("textbox", { name: "Type DELETE" }).fill("DELETE");
  // A proxy arrives (through the local API, say) after the plan was read.
  const proxies = await page.evaluate(() => {
    const s = (window as unknown as Mock).__shardxMock.state;
    s.proxies.push({ ...(s.proxies[0] as object), id: "px-late" });
    return s.proxies.length;
  });
  await second(page).getByRole("button", { name: "Delete all data" }).click();

  const warn = first(page);
  await expect(warn.locator("[data-delete-all-changed]")).toHaveText("Your data changed. Check again.");
  await expect(warn.getByRole("listitem").filter({ hasText: `${proxies} proxies` })).toHaveCount(1);
  await expect(page.locator("[data-deleting-all]")).toHaveCount(0);
  expect(await page.evaluate(() => (window as unknown as Mock).__shardxMock.state.proxies.length)).toBe(proxies);
});

test("a refused reset says why and leaves the app as it was", async ({ page }) => {
  await storage(page);
  await page.evaluate(() => {
    localStorage.setItem("shardx.e2eMarker", "1");
    Object.assign(window, { __shardxMockFailWith: { app_reset: "Nothing was moved: /Users is a system folder." } });
  });
  await trigger(page).click();
  await first(page).getByRole("button", { name: "Continue" }).click();
  await second(page).getByRole("textbox", { name: "Type DELETE" }).fill("DELETE");
  await second(page).getByRole("button", { name: "Delete all data" }).click();

  const toast = page.locator("[data-toast]").filter({ hasText: "Nothing was moved" });
  await expect(toast).toContainText("/Users is a system folder.");
  await expect(page.locator("[data-deleting-all]")).toHaveCount(0);
  await expect(trigger(page)).toBeFocused();
  // This window's own keys came back, and the data is all there.
  expect(await page.evaluate(() => localStorage.getItem("shardx.e2eMarker"))).toBe("1");
  expect((await counts(page)).profiles).toBeGreaterThan(0);
});

test("a reset that could not put everything back says where the data is, in a dialog that stays", async ({ page }) => {
  await storage(page);
  await page.evaluate((backup) => {
    Object.assign(window, { __shardxMockFailWith: { app_reset: `Some data is still in ${backup}: the disk went away. It could not all be put back (proxies.json).` } });
  }, BACKUP);
  await trigger(page).click();
  await first(page).getByRole("button", { name: "Continue" }).click();
  await second(page).getByRole("textbox", { name: "Type DELETE" }).fill("DELETE");
  await second(page).getByRole("button", { name: "Delete all data" }).click();

  const stuck = page.getByRole("alertdialog", { name: "Some data couldn’t be put back" });
  await expect(stuck).toBeVisible();
  await expect(stuck).toContainText(`Your data is partly in ${BACKUP}. Move the items listed in its RECOVERY.txt back where they came from.`);
  await expect(stuck).toContainText("the disk went away");
  await expect(stuck.getByRole("button", { name: "Show folder" })).toBeVisible();
  // Not a toast that goes away, and no restart.
  await expect(page.locator("[data-toast]").filter({ hasText: "put back" })).toHaveCount(0);
  await expect(page.locator("[data-deleting-all]")).toHaveCount(0);
  await stuck.getByRole("button", { name: "Close" }).click();
  await expect(stuck).toHaveCount(0);
});

/** Opens the app on a reset that did not finish (the mock's journal). */
async function unfinished(page: Page) {
  await openApp(page, { section: "settings" });
  await page.evaluate((backup) => {
    const m = (window as unknown as { __shardxMock: { state: Record<string, unknown>; save: () => void } }).__shardxMock;
    m.state.resetPending = {
      journal: "/Users/you/Library/Application Support/com.proxyshard.shardx/reset-in-progress.json",
      backups: [backup, "/Volumes/T7/shardx-launcher-backup-2026-09-01-120000"], moved: 7, total: 22, missing: [], error: null,
    };
    m.save();
  }, BACKUP);
  await page.reload();
  const screen = page.locator("[data-reset-pending]");
  await expect(screen.getByRole("heading", { name: "A reset didn’t finish" })).toBeVisible({ timeout: 15_000 });
  // Nothing of the app loaded behind it.
  await expect(page.locator("main [data-page-title]")).toHaveCount(0);
  return screen;
}

test("a reset that did not finish opens on its own screen, and Undo puts everything back", async ({ page }) => {
  const screen = await unfinished(page);
  await expect(screen.locator("[data-reset-pending-where]")).toHaveText(
    `Delete all data stopped partway. Some of your data is in ${BACKUP} and /Volumes/T7/shardx-launcher-backup-2026-09-01-120000.`,
  );
  await expect(screen).toContainText("7 of 22 items were moved.");
  const reloaded = page.waitForEvent("load", { timeout: 15_000 });
  await screen.getByRole("button", { name: "Undo the reset" }).click();
  await reloaded;
  await expect(page.locator("main [data-page-title]")).toBeAttached({ timeout: 15_000 });
  await waitForIdle(page);
  expect((await counts(page)).profiles).toBeGreaterThan(0);
  await expect(page.locator("[data-reset-pending]")).toHaveCount(0);
});

test("Finish the reset completes it and starts over", async ({ page }) => {
  const screen = await unfinished(page);
  const reloaded = page.waitForEvent("load", { timeout: 15_000 });
  await screen.getByRole("button", { name: "Finish the reset" }).click();
  await reloaded;
  await expect(page.locator("main [data-page-title]")).toBeAttached({ timeout: 15_000 });
  await waitForIdle(page);
  expect(await counts(page)).toEqual({ profiles: 0, proxies: 0, extensions: 0, bookmarks: 0, projects: 0 });
  await expect(page.locator("[data-reset-pending]")).toHaveCount(0);
});

const NOTES = "/Users/you/Library/Application Support/com.proxyshard.shardx/reset-in-progress.json";

/** Opens the app on notes it can't read (a corrupt or untrusted journal). */
async function corrupt(page: Page) {
  await openApp(page, { section: "settings" });
  await page.evaluate((notes) => {
    const m = (window as unknown as { __shardxMock: { state: Record<string, unknown>; save: () => void } }).__shardxMock;
    m.state.resetPending = {
      journal: notes, backups: [], moved: 0, total: 0, missing: [],
      error: `ShardBrowser can’t read its notes on the reset (${notes}): expected value at line 1 column 3`,
    };
    m.save();
  }, NOTES);
  await page.reload();
  const screen = page.locator("[data-reset-pending]");
  await expect(screen.getByRole("heading", { name: "A reset didn’t finish" })).toBeVisible({ timeout: 15_000 });
  return screen;
}

test("notes that can't be read still name their file and offer a way out", async ({ page }) => {
  const screen = await corrupt(page);
  await expect(screen).toContainText("ShardBrowser can’t read its notes on this reset.");
  await expect(screen.locator("[data-reset-pending-notes]")).toHaveText(`Its notes are in ${NOTES}.`);
  await expect(screen).toContainText("When you’ve put back what you need (RECOVERY.txt lists it), choose “Set these notes aside”.");
  await expect(screen.getByRole("button", { name: "Show folder" })).toBeVisible();
  // Nothing to undo or finish from notes that can't be trusted.
  await expect(screen.getByRole("button", { name: "Undo the reset" })).toHaveCount(0);
  await expect(screen.getByRole("button", { name: "Finish the reset" })).toHaveCount(0);

  // Asked first; Cancel changes nothing.
  await screen.getByRole("button", { name: "Set these notes aside" }).click();
  const ask = page.getByRole("alertdialog", { name: "Set these notes aside?" });
  await expect(ask.getByRole("button", { name: "Cancel" })).toBeFocused();
  await ask.getByRole("button", { name: "Cancel" }).click();
  await expect(ask).toHaveCount(0);
  expect(await page.evaluate(() => (window as unknown as { __shardxMock: { calls: Record<string, number> } }).__shardxMock.calls.app_reset_set_aside ?? 0)).toBe(0);

  // Set aside: the app starts as usual, with the data as it was.
  await screen.getByRole("button", { name: "Set these notes aside" }).click();
  const reloaded = page.waitForEvent("load", { timeout: 15_000 });
  await page.getByRole("alertdialog", { name: "Set these notes aside?" }).getByRole("button", { name: "Set notes aside" }).click();
  await reloaded;
  await expect(page.locator("main [data-page-title]")).toBeAttached({ timeout: 15_000 });
  await expect(page.locator("[data-reset-pending]")).toHaveCount(0);
  expect((await counts(page)).profiles).toBeGreaterThan(0);
});

test("a reset waiting on a disk says which one to connect", async ({ page }) => {
  await openApp(page, { section: "settings" });
  await page.evaluate((notes) => {
    const m = (window as unknown as { __shardxMock: { state: Record<string, unknown>; save: () => void } }).__shardxMock;
    m.state.resetPending = {
      journal: notes, backups: ["/Users/you/Library/Application Support/shardx-launcher-backup-2026-09-01-120000", "/Volumes/T7/shardx-launcher-backup-2026-09-01-120000"],
      moved: 7, total: 22, missing: ["/Volumes/T7/shardx-launcher-backup-2026-09-01-120000"], error: null,
    };
    m.save();
  }, NOTES);
  await page.reload();
  const screen = page.locator("[data-reset-pending]");
  await expect(screen.locator("[data-reset-pending-missing]")).toHaveText("Connect the disk with /Volumes/T7/shardx-launcher-backup-2026-09-01-120000, then try again.", { timeout: 15_000 });
  await expect(screen.getByRole("button", { name: "Undo the reset" })).toBeVisible();
  await expect(screen.getByRole("button", { name: "Set these notes aside" })).toBeVisible();
});
