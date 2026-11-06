import { expect, test, type Page } from "@playwright/test";
import { openApp, waitForIdle } from "./support/app";

// "Report the window as maximized" (#140): a flag in the profile's window
// block, off by default, and no switch at all on a phone profile.
type Win = { outer_width?: number; outer_height?: number; maximized?: boolean; keep_focus?: boolean };
type Mock = { __shardxMock: { state: { order: string[]; profiles: Record<string, { window?: Win }> } } };

const dialog = (page: Page) => page.getByRole("dialog", { name: "New profile", exact: true });
const maximized = (page: Page) => dialog(page).getByRole("switch", { name: "Report the window as maximized" });

async function openNew(page: Page, os: string) {
  await openApp(page);
  await page.getByRole("button", { name: "New profile" }).first().click();
  await dialog(page).getByRole("tab", { name: "Fingerprint", exact: true }).click();
  await dialog(page).getByRole("radiogroup", { name: "Operating system" }).getByRole("radio", { name: os, exact: true }).click();
}

async function created(page: Page): Promise<Win | undefined> {
  await dialog(page).getByRole("button", { name: "Create profile" }).click();
  await expect(dialog(page)).toHaveCount(0);
  await waitForIdle(page);
  return page.evaluate(() => {
    const s = (window as unknown as Mock).__shardxMock.state;
    return s.profiles[s.order[s.order.length - 1]].window;
  });
}

test("the maximized switch is off by default and writes nothing", async ({ page }) => {
  await openNew(page, "Windows");
  await expect(maximized(page)).not.toBeChecked();
  expect((await created(page))?.maximized).toBeUndefined();
});

test("turned on, the window block carries the flag and a size", async ({ page }) => {
  await openNew(page, "Windows");
  await maximized(page).click();
  const win = await created(page);
  expect(win?.maximized).toBe(true);
  expect(win?.keep_focus).toBeUndefined();
  expect(win?.outer_width).toBeGreaterThan(0);
  expect(win?.outer_height).toBeGreaterThan(0);
});

test("a phone profile has no maximized switch", async ({ page }) => {
  await openNew(page, "Android");
  await expect(dialog(page).getByRole("switch", { name: "Always report the window as focused" })).toBeVisible();
  await expect(maximized(page)).toHaveCount(0);
});
