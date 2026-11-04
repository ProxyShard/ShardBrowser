import { readFileSync } from "node:fs";
import { expect, test } from "@playwright/test";
import { openApp } from "./support/app";

// The newest release in the log, so the check doesn't go stale with each one.
const latest = (JSON.parse(readFileSync(new URL("../src/widgets/WhatsNewDialog/patchlog.json", import.meta.url), "utf8")) as { releases: { version: string }[] }).releases[0].version;

for (const theme of ["dark", "light"] as const) {
  test(`What's new opens by itself on the first run after an update (${theme})`, async ({ page }) => {
    await page.addInitScript(() => localStorage.setItem("shardx-patchlog-seen-version", "0.0.1"));
    await openApp(page, { theme });
    const dialog = page.getByRole("dialog", { name: /What’s new/ });
    await expect(dialog).toBeVisible();
    await expect(dialog.getByText(`v${latest}`).first()).toBeVisible();
  });

  test(`What's new, ${theme}`, { tag: "@visual" }, async ({ page }) => {
    await page.addInitScript(() => localStorage.setItem("shardx-patchlog-seen-version", "0.0.1"));
    await openApp(page, { theme });
    await expect(page.getByRole("dialog", { name: /What’s new/ })).toBeVisible();
    await expect(page).toHaveScreenshot(`whats-new-${theme}.png`);
  });
}

test("the version item in the sidebar opens What's new", async ({ page }) => {
  await openApp(page);
  await page.getByRole("button", { name: /ShardBrowser v2\.0\.3/ }).click();
  await expect(page.getByRole("dialog", { name: /What’s new/ })).toBeVisible();
});

test("there is no Patch log page any more", async ({ page }) => {
  await openApp(page, { section: "patchlog" });
  await expect(page.locator("main[data-section='profiles']")).toBeVisible();
  await expect(page.locator("[data-nav='patchlog']")).toHaveCount(0);
});
