import { expect, test, type Page } from "@playwright/test";
import { SECTIONS, openApp } from "./support/app";

// Clarity fixes, item 7: the (i) beside a page's title opens a popover that
// says what the page is for and what you do there. It used to hold the text
// as a tooltip only, and a click dismissed it.

const info = (page: Page) => page.locator("main header").getByRole("button", { name: "About this page" });

/** Each page's popover heading and a phrase from its body. */
const HELP: Record<(typeof SECTIONS)[number], { title: string; body: RegExp }> = {
  profiles: { title: "About profiles", body: /one browser identity/ },
  proxies: { title: "About proxies", body: /address a profile connects through/ },
  proxyshard: { title: "About ShardX", body: /proxy service/ },
  automation: { title: "About automation", body: /runs a list of steps/ },
  fingerprints: { title: "About the fingerprint library", body: /hardware identity/ },
  extensions: { title: "About extensions", body: /turn it on for each profile in its details panel/ },
  bookmarks: { title: "About bookmarks", body: /shows up in every profile in that folder/ },
  trash: { title: "About Trash", body: /stay here for 7 days/ },
  settings: { title: "About Settings", body: /how ShardBrowser looks and behaves/ },
};

test("clicking (i) opens the page help on every page", async ({ page }) => {
  test.slow(); // opens every page in turn: 9 loads
  for (const section of SECTIONS) {
    await openApp(page, { section });
    await expect(info(page), section).toHaveAttribute("aria-expanded", "false");
    await info(page).click();
    const help = page.getByRole("dialog", { name: HELP[section].title });
    // A click opens it and it stays: the tooltip no longer swallows the press.
    await expect(help, section).toBeVisible();
    await expect(help.getByRole("heading", { name: HELP[section].title }), section).toBeVisible();
    await expect(help, section).toContainText(HELP[section].body);
    await expect(info(page), section).toHaveAttribute("aria-expanded", "true");
    // Two or three sentences, on every page.
    const body = help.locator("p");
    await expect(body, section).toHaveCount(1);
    const sentences = ((await body.textContent()) ?? "").split(/(?<=[.!?])\s+/).filter(Boolean);
    expect(sentences.length, section).toBeGreaterThanOrEqual(2);
    expect(sentences.length, section).toBeLessThanOrEqual(3);
  }
});

test("Esc closes the help and focus returns to the button", async ({ page }) => {
  await openApp(page, { section: "bookmarks" });
  await info(page).click();
  const help = page.getByRole("dialog", { name: "About bookmarks" });
  await expect(help).toBeVisible();
  await page.keyboard.press("Escape");
  await expect(help).toHaveCount(0);
  await expect(info(page)).toBeFocused();
  await expect(info(page)).toHaveAttribute("aria-expanded", "false");
});

test("Enter and Space open the help from the keyboard", async ({ page }) => {
  await openApp(page, { section: "trash" });
  const help = page.getByRole("dialog", { name: "About Trash" });
  await info(page).focus();
  await page.keyboard.press("Enter");
  await expect(help).toBeVisible();
  await page.keyboard.press("Escape");
  await expect(help).toHaveCount(0);
  await expect(info(page)).toBeFocused();
  await page.keyboard.press("Space");
  await expect(help).toBeVisible();
});

test("a press outside closes the help", async ({ page }) => {
  await openApp(page, { section: "extensions" });
  await info(page).click();
  const help = page.getByRole("dialog", { name: "About extensions" });
  await expect(help).toBeVisible();
  await page.locator("main [data-page-body]").click({ position: { x: 600, y: 300 } });
  await expect(help).toHaveCount(0);
});

test("every page's help ends with Show me around, its one button", async ({ page }) => {
  test.slow(); // three pages in turn
  for (const section of ["profiles", "fingerprints", "settings"] as const) {
    await openApp(page, { section });
    await info(page).click();
    const help = page.getByRole("dialog", { name: HELP[section].title });
    await expect(help, section).toBeVisible();
    await expect(help.getByRole("button"), section).toHaveCount(1);
    await expect(help.getByRole("button", { name: "Show me around" }), section).toBeVisible();
  }
});
