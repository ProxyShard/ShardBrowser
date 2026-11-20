import { expect, test, type Page } from "@playwright/test";
import { openApp } from "./support/app";

// #161: a newer launcher asks by itself, a little after the start, and Update
// downloads it and installs it (the mock's Apply leaves a mark).

const offer = (installable: boolean) => ({
  current: "2.0.3",
  version: "2.1.7",
  installable,
  release_url: "https://github.com/ProxyShard/ShardBrowser/releases/tag/v2.1.7",
});

async function withOffer(page: Page, installable: boolean) {
  await page.addInitScript((o) => {
    (window as { __shardxMockLauncherOffer?: unknown }).__shardxMockLauncherOffer = o;
  }, offer(installable));
  await openApp(page);
}

const prompt = (page: Page) => page.getByRole("dialog", { name: "ShardBrowser Launcher 2.1.7 is available" })
  .or(page.getByRole("alertdialog", { name: "ShardBrowser Launcher 2.1.7 is available" }));

// The first check waits a few seconds after the start.
test.describe.configure({ timeout: 45_000 });

test("a newer launcher asks by itself, names the profiles it closes, and Update installs it", async ({ page }) => {
  await withOffer(page, true);
  await expect(prompt(page)).toBeVisible({ timeout: 20_000 });
  // The seed has two profiles running: they are named, and stopped before the install.
  await expect(prompt(page)).toContainText("These profiles close first, and the launcher restarts on the new version.");
  await expect(prompt(page)).toContainText("Amazon US seller");
  await prompt(page).getByRole("button", { name: "Update" }).click();
  await expect(page.getByRole("region", { name: "Notifications" }).getByText("Launcher update 2.1.7 downloaded")).toBeVisible();
  await expect.poll(() => page.evaluate(() => (window as { __shardxMockLauncherApplied?: boolean }).__shardxMockLauncherApplied === true)).toBe(true);
});

test("Not now closes the question and installs nothing", async ({ page }) => {
  await withOffer(page, true);
  await expect(prompt(page)).toBeVisible({ timeout: 20_000 });
  await prompt(page).getByRole("button", { name: "Not now" }).click();
  await expect(prompt(page)).toHaveCount(0);
  expect(await page.evaluate(() => (window as { __shardxMockLauncherApplied?: boolean }).__shardxMockLauncherApplied)).toBeUndefined();
});

test("a copy that cannot update itself offers the download instead", async ({ page }) => {
  await withOffer(page, false);
  await expect(prompt(page)).toBeVisible({ timeout: 20_000 });
  await expect(prompt(page)).toContainText("This copy can’t update itself.");
  await expect(prompt(page).getByRole("button", { name: "Download" })).toBeVisible();
  await expect(prompt(page).getByRole("button", { name: "Update" })).toHaveCount(0);
});
