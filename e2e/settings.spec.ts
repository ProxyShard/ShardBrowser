import { expect, test, type Page } from "@playwright/test";
import { openApp } from "./support/app";

const tab = (page: Page, name: string) =>
  page.getByRole("tablist", { name: "Settings sections" }).getByRole("tab", { name });

test("Settings has twelve sections", async ({ page }) => {
  await openApp(page, { section: "settings" });
  const tabs = page.getByRole("tablist", { name: "Settings sections" }).getByRole("tab");
  await expect(tabs).toHaveText(["General", "Appearance", "Profile defaults", "Templates", "Tags", "Views", "API & MCP", "Helper", "Storage", "Sync", "Other", "About"]);
  await expect(tab(page, "General")).toHaveAttribute("aria-selected", "true");
});

test("Appearance: choosing Dark switches the theme at once", async ({ page }) => {
  await openApp(page, { section: "settings", theme: "light" });
  await tab(page, "Appearance").click();
  await page.locator("[data-setting='theme']").getByRole("radio", { name: "Dark", exact: true }).click();
  await expect(page.locator("html")).toHaveAttribute("data-theme", "dark");
});

test("Appearance: Reduce motion applies at once and survives a reload", async ({ page }) => {
  await openApp(page, { section: "settings" });
  await tab(page, "Appearance").click();
  await page.getByRole("switch", { name: "Reduce motion" }).check();
  await expect(page.locator("html")).toHaveAttribute("data-reduce-motion", "true");
  // Travel and scale go (the tokens); fades and spinners stay (ANIM-05).
  expect(await page.evaluate(() => getComputedStyle(document.documentElement).getPropertyValue("--motion-shift").trim())).toBe("0px");
  // It shows at once and is stored on Save (manual save).
  await page.locator("[data-save-bar]").getByRole("button", { name: "Save", exact: true }).click();
  await page.reload();
  await expect(page.locator("html")).toHaveAttribute("data-reduce-motion", "true");
});

test("General: the tray switch is saved by the Save bar, not at once (manual save)", async ({ page }) => {
  await openApp(page, { section: "settings" });
  const saved = () => page.evaluate(() => (window as unknown as { __shardxMock: { state: { settings: { minimize_to_tray: boolean } } } }).__shardxMock.state.settings.minimize_to_tray);
  await expect(page.locator("[data-save-bar]")).toHaveCount(0);
  await page.getByRole("switch", { name: /tray/i }).click();
  const bar = page.locator("[data-save-bar]");
  await expect(bar).toContainText("Unsaved changes");
  expect(await saved()).toBe(false);
  await bar.getByRole("button", { name: "Save", exact: true }).click();
  await expect.poll(saved).toBe(true);
});

test("the sidebar's API status opens Settings on API & MCP", async ({ page }) => {
  await openApp(page);
  await page.getByRole("button", { name: /^API on/ }).click();
  await page.getByRole("button", { name: "API & MCP settings" }).click();
  await expect(tab(page, "API & MCP")).toHaveAttribute("aria-selected", "true");
  // The address sits in the API details card, beside its label (owner, "more air").
  await expect(page.getByRole("tabpanel").locator("[data-detail='base-url']")).toContainText(/Base URL\s*http:\/\/127\.0\.0\.1:40325/);
});

test("About shows the version and opens What's new", async ({ page }) => {
  await openApp(page, { section: "settings" });
  await tab(page, "About").click();
  await expect(page.locator("main").getByText(/2\.0\.3/).first()).toBeVisible();
  await page.locator("main").getByRole("button", { name: "What’s new" }).click();
  await expect(page.getByRole("dialog", { name: /What’s new/ })).toBeVisible();
});
