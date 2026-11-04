import { expect, test, type Page } from "@playwright/test";
import { resolve } from "node:path";
import { openApp } from "./support/app";
import { pickOption } from "./support/select";

// Import from Chrome: Chrome's own `Bookmarks` file (no extension) comes in
// with its roots and folders, in Chrome's order, into every profile or one Folder.
// The sample holds 11 bookmarks in nested folders: a bookmarklet (kept), an
// empty address (skipped), GitHub twice in Work (one repeat) and once in Other
// bookmarks (its own), and pixelscan.net on the bar, where the seed has it.
type Bm = { id: string; title: string; url: string; folder: string; root?: string; path?: string[] };
type Mock = { __shardxMock: { state: { bookmarks: Bm[] }; calls: Record<string, number> } };

// Read from the repo root, where Playwright runs.
const SAMPLE = resolve("e2e/fixtures/chrome/Bookmarks");
/** Title, address, Chrome's root ("" is the bar) and folders. */
const NEW_IN_ORDER: [string, string, string, string[]][] = [
  ["Gmail", "https://mail.google.com/mail/u/0/", "", []],
  ["GitHub", "https://github.com/", "", ["Work"]],
  ["MDN Web Docs", "https://developer.mozilla.org/en-US/", "", ["Work", "Docs"]],
  ["Show page title", "javascript:alert(document.title)", "", ["Work"]],
  ["Hacker News", "https://news.ycombinator.com/", "", []],
  ["GitHub again", "https://github.com/", "other", []],
  ["Amazon", "https://www.amazon.com/", "other", ["Shopping"]],
  ["pixelscan", "https://pixelscan.net/", "other", []],
];

const bookmarks = (page: Page) => page.evaluate(() => (window as unknown as Mock).__shardxMock.state.bookmarks);
const dialog = (page: Page) => page.getByRole("dialog", { name: "Import from Chrome" });

/** Import from Chrome in the header, answering the file picker with `file`. */
async function startImport(page: Page, file: string | { name: string; mimeType: string; buffer: Buffer }, from = page.locator("main header")) {
  const chooser = page.waitForEvent("filechooser");
  await from.getByRole("button", { name: "Import from Chrome" }).click();
  await (await chooser).setFiles(file);
  await expect(dialog(page)).toHaveAttribute("data-state", "open");
}

test("imports Chrome's bookmarks with their folders, in Chrome's order", async ({ page }) => {
  await openApp(page, { section: "bookmarks" });
  const before = await bookmarks(page);
  await startImport(page, SAMPLE);
  const d = dialog(page);
  await expect(d.locator("[data-import-summary]")).toHaveText("Found 11 bookmarks in “Bookmarks”. 1 is already here, in the same folder.");
  await expect(d.locator("[data-import-skipped]")).toHaveText("1 skipped: an empty address or a link like mailto: that is neither a page nor a bookmarklet.");
  // GitHub's second copy in Work is a repeat; the one in Other bookmarks is a bookmark of its own.
  await expect(d.locator("[data-import-repeats]")).toHaveText("1 is a repeat within one folder: it comes in once.");
  await expect(d.locator("[data-import-folders]")).toHaveText("Chrome’s folders are kept: in profiles, each bookmark shows up where it was in Chrome.");
  // Every profile unless another is picked: the same choices as the editor's.
  await expect(d.getByRole("combobox", { name: "Shows in" })).toContainText("Every profile");
  await pickOption(d, "Shows in", "Ads");
  await d.getByRole("button", { name: "Import 8 bookmarks" }).click();
  await expect(d).toHaveCount(0);
  await expect(page.locator("[data-toast]").getByText("Imported 8 bookmarks", { exact: true })).toBeVisible();

  const after = await bookmarks(page);
  expect(after.slice(0, before.length)).toEqual(before);
  // No Chrome folder made it in as a bookmark; the bookmarklet did, as written.
  expect(after.slice(before.length).map((b) => [b.title, b.url, b.root ?? "", b.path ?? [], b.folder]))
    .toEqual(NEW_IN_ORDER.map(([title, url, root, path]) => [title, url, root, path, "Ads"]));
  // The list shows them, on All, with where they sit in the browser.
  for (const [title] of NEW_IN_ORDER) await expect(page.locator("main [data-cell='name']").getByText(title, { exact: true }).first()).toBeVisible();
  await expect(page.locator("main [data-cell='path']").getByText("Bookmarks bar / Work / Docs", { exact: true })).toBeVisible();
  await expect(page.locator("main [data-cell='path']").getByText("Other bookmarks / Shopping", { exact: true })).toBeVisible();
});

test("importing the same file twice adds nothing the second time", async ({ page }) => {
  await openApp(page, { section: "bookmarks" });
  await startImport(page, SAMPLE);
  await dialog(page).getByRole("button", { name: "Import 8 bookmarks" }).click();
  await expect(dialog(page)).toHaveCount(0);
  const once = await bookmarks(page);
  // Once in each place Chrome has it.
  expect(once.filter((b) => b.folder === "" && b.url === "https://github.com/").map((b) => [b.root ?? "", b.path ?? []])).toEqual([["", ["Work"]], ["other", []]]);

  await startImport(page, SAMPLE);
  const d = dialog(page);
  await expect(d.locator("[data-import-summary]")).toHaveText("Found 11 bookmarks in “Bookmarks”. 10 are already here, in the same folder.");
  await expect(d.getByRole("button", { name: "Import", exact: true })).toBeDisabled();
  await expect(d.getByText("Nothing new to import.", { exact: true })).toBeVisible();
  await d.getByRole("button", { name: "Cancel" }).click();
  await expect(d).toHaveCount(0);
  expect(await bookmarks(page)).toEqual(once);
});

test("a file that is not Chrome's says so in the dialog, and another can be picked", async ({ page }) => {
  await openApp(page, { section: "bookmarks" });
  // The launcher's own bookmarks file: JSON, but not Chrome's.
  const own = { name: "bookmarks.json", mimeType: "application/json", buffer: Buffer.from(JSON.stringify({ bookmarks: [{ id: "x", title: "X", url: "https://x.example", folder: "" }] })) };
  await startImport(page, own);
  const d = dialog(page);
  await expect(d.getByRole("alert")).toHaveText("“bookmarks.json” isn’t a Chrome bookmarks file.");
  await expect(d).toContainText("chrome://version");
  const chooser = page.waitForEvent("filechooser");
  await d.getByRole("button", { name: "Choose another file" }).click();
  await (await chooser).setFiles(SAMPLE);
  await expect(d.locator("[data-import-summary]")).toHaveText("Found 11 bookmarks in “Bookmarks”. 1 is already here, in the same folder.");
  await expect(d.getByRole("button", { name: "Import 8 bookmarks" })).toBeVisible();
  expect(await page.evaluate(() => (window as unknown as Mock).__shardxMock.calls.bookmark_save ?? 0)).toBe(0);
});

test("a save the team refuses stops the import and says why, in place", async ({ page }) => {
  await openApp(page, { section: "bookmarks" });
  const before = await bookmarks(page);
  await startImport(page, SAMPLE);
  const d = dialog(page);
  // A member without Create in a team: the backend's own words.
  await page.evaluate(() => Object.assign(window, { __shardxMockFailWith: { bookmark_save: "not allowed in this team: needs create on the team" } }));
  await d.getByRole("button", { name: "Import 8 bookmarks" }).click();
  await expect(d.getByRole("alert")).toHaveText("Nothing was imported. Your rights in this team don’t let you add bookmarks: ask the team’s admin.");
  await expect(d).toHaveAttribute("data-state", "open");
  expect(await bookmarks(page)).toEqual(before);
  // Once allowed, the same dialog imports them.
  await d.getByRole("button", { name: "Import 8 bookmarks" }).click();
  await expect(d).toHaveCount(0);
  expect((await bookmarks(page)).length).toBe(before.length + 8);
});

test("the empty page offers the import too", async ({ page }) => {
  await page.addInitScript(() => Object.assign(window, { __shardxMockEmpty: true }));
  await openApp(page, { section: "bookmarks" });
  const empty = page.locator("main [data-empty-state]");
  await startImport(page, SAMPLE, empty);
  // Nothing is here yet: the repeat in the file is said as one.
  await expect(dialog(page).locator("[data-import-summary]")).toHaveText("Found 11 bookmarks in “Bookmarks”.");
  await expect(dialog(page).locator("[data-import-repeats]")).toHaveText("1 is a repeat within one folder: it comes in once.");
  await dialog(page).getByRole("button", { name: "Import 9 bookmarks" }).click();
  await expect(dialog(page)).toHaveCount(0);
  expect((await bookmarks(page)).map((b) => b.title)).toEqual(["Gmail", "GitHub", "MDN Web Docs", "Show page title", "Hacker News", "pixelscan", "GitHub again", "Amazon", "pixelscan"]);
});

test("a press outside the dialog stops the import, which stays open to go on", async ({ page }) => {
  await openApp(page, { section: "bookmarks" });
  const before = await bookmarks(page);
  await startImport(page, SAMPLE);
  const d = dialog(page);
  await page.evaluate(() => Object.assign(window, { __shardxMockSlow: { bookmark_save: 400 } }));
  await d.getByRole("button", { name: "Import 8 bookmarks" }).click();
  await expect(d.locator("[data-import-progress]")).toContainText("Imported 1 of 8");
  // A press on the backdrop, as Esc and Stop: the import stops, the dialog stays.
  await page.mouse.click(8, 400);
  await expect(d.getByRole("status")).toHaveText(/^Stopped after [12] of 8\.$/);
  await expect(d).toHaveAttribute("data-state", "open");
  const done = (await bookmarks(page)).length - before.length;
  expect(await d.getByRole("status").textContent()).toBe(`Stopped after ${done} of 8.`);
  await expect(page.locator("[data-toast]")).toHaveCount(0);
  // The rest, from where it stopped.
  await page.evaluate(() => Object.assign(window, { __shardxMockSlow: {} }));
  await d.getByRole("button", { name: `Import ${8 - done} bookmarks` }).click();
  await expect(d).toHaveCount(0);
  await expect(page.locator("[data-toast]").getByText(`Imported ${8 - done} bookmarks`, { exact: true })).toBeVisible();
  expect((await bookmarks(page)).slice(before.length).map((b) => b.title)).toEqual(NEW_IN_ORDER.map(([title]) => title));
});
