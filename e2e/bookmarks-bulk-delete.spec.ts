import { expect, test, type Page } from "@playwright/test";
import { openApp } from "./support/app";

// Users who imported hundreds of bookmarks deleted them one by one (owner,
// 2026-11-04): Select all in the header, the bar's Delete, one confirm and one
// backend call. A row still opens its editor on a click.
type Bm = { id: string; title: string; url: string; folder: string };
type Mock = { __shardxMock: { state: { bookmarks: Bm[] }; calls: Record<string, number>; emit(event: string, payload: unknown): void } };

const grid = (page: Page) => page.getByRole("treegrid", { name: "Bookmarks" });
const rows = (page: Page) => grid(page).locator("[role='row'][data-row-id]");
const selectAll = (page: Page) => grid(page).locator("[data-list-header]").getByRole("checkbox", { name: "Select all" });
/** Select all shows on the list's hover (owner, 2026-11-04). */
async function pickAll(page: Page) {
  await grid(page).locator("[data-list-header] [data-col='name']").hover();
  await selectAll(page).check();
}
const bar = (page: Page) => page.getByRole("toolbar", { name: /selected/ });
const calls = (page: Page, name: string) => page.evaluate((n) => (window as unknown as Mock).__shardxMock.calls[n] ?? 0, name);
const undoToast = (page: Page) => page.locator("[data-toast]").filter({ has: page.getByRole("button", { name: "Undo" }) });
const stored = (page: Page) => page.evaluate(() => (window as unknown as Mock).__shardxMock.state.bookmarks.map((b) => b.id));

/** Many more than a screen of rows, as an import leaves. */
async function seed(page: Page, n: number) {
  await page.evaluate((n) => {
    const m = (window as unknown as Mock).__shardxMock;
    for (let i = 0; i < n; i++) m.state.bookmarks.push({ id: `imp-${i}`, title: `Imported ${i}`, url: `https://site-${i}.example`, folder: "" });
    m.emit("store-changed", "bookmarks");
  }, n);
}

test("Select all shows on the list's hover and picks every bookmark the list shows; Delete asks once and makes one call", async ({ page }) => {
  await openApp(page, { section: "bookmarks", theme: "light" });
  await seed(page, 120);
  // The header row and every bookmark, drawn or not (a virtual list).
  await expect(grid(page)).toHaveAttribute("aria-rowcount", "126");
  await expect(selectAll(page)).toBeHidden();
  await pickAll(page);
  await expect(bar(page)).toContainText("125 selected");
  await bar(page).getByRole("button", { name: "Delete" }).click();
  const ask = page.getByRole("alertdialog", { name: "Delete 125 bookmarks?" });
  await expect(ask).toBeVisible();
  await ask.getByRole("button", { name: "Delete 125 bookmarks" }).click();
  await expect(rows(page)).toHaveCount(0);
  await expect(bar(page)).toHaveCount(0);
  await expect(undoToast(page)).toContainText("Deleted 125 bookmarks");
  // Behind its Undo toast: the call goes when the toast does (or the window).
  expect(await calls(page, "bookmark_delete_many")).toBe(0);
  await page.evaluate(() => window.dispatchEvent(new Event("pagehide")));
  await expect.poll(() => stored(page)).toEqual([]);
  expect(await calls(page, "bookmark_delete_many")).toBe(1);
  expect(await calls(page, "bookmark_delete")).toBe(0);
});

test("Undo brings every one back, picked again", async ({ page }) => {
  await openApp(page, { section: "bookmarks", theme: "light" });
  await expect(rows(page)).toHaveCount(5);
  await pickAll(page);
  await bar(page).getByRole("button", { name: "Delete" }).click();
  await page.getByRole("alertdialog").getByRole("button", { name: "Delete 5 bookmarks" }).click();
  await expect(rows(page)).toHaveCount(0);
  await undoToast(page).getByRole("button", { name: "Undo" }).click();
  await expect(rows(page)).toHaveCount(5);
  await expect(bar(page)).toContainText("5 selected");
  expect(await calls(page, "bookmark_delete_many")).toBe(0);
});

test("Select all respects the Folder tab: only the rows shown go", async ({ page }) => {
  await openApp(page, { section: "bookmarks", theme: "light" });
  await page.getByRole("radio", { name: "Shops" }).click();
  await expect(rows(page)).toHaveCount(2);
  await pickAll(page);
  await expect(bar(page)).toContainText("2 selected");
  // The Delete key on a row acts on the picks, as the bar's button does.
  await rows(page).first().focus();
  await page.keyboard.press("Delete");
  await page.getByRole("alertdialog").getByRole("button", { name: "Delete 2 bookmarks" }).click();
  await expect(rows(page)).toHaveCount(0);
  await page.getByRole("radio", { name: "All" }).click();
  await expect(rows(page)).toHaveCount(3);
});

test("a plain click still opens the editor; Cmd/Ctrl+click and the box select, with no editor", async ({ page }) => {
  await openApp(page, { section: "bookmarks", theme: "light" });
  const first = grid(page).locator("[role='row'][data-row-id='bm-1']");
  await first.locator("[data-cell='name']").click();
  await expect(page.getByRole("dialog", { name: "Edit bookmark" })).toBeVisible();
  await page.keyboard.press("Escape");
  await expect(page.getByRole("dialog", { name: "Edit bookmark" })).toHaveCount(0);
  await first.locator("[data-cell='name']").click({ modifiers: ["ControlOrMeta"] });
  await expect(first).toHaveAttribute("aria-selected", "true");
  const second = grid(page).locator("[role='row'][data-row-id='bm-2']");
  await second.hover();
  await second.getByRole("checkbox", { name: /^Select / }).check();
  await expect(bar(page)).toContainText("2 selected");
  await expect(page.getByRole("dialog", { name: "Edit bookmark" })).toHaveCount(0);
  // Unticked from the keyboard, Select all keeps it.
  await selectAll(page).focus();
  await page.keyboard.press("Space");
  await expect(bar(page)).toContainText("5 selected");
  await page.keyboard.press("Space");
  await expect(bar(page)).toHaveCount(0);
  await expect(selectAll(page)).toBeFocused();
});
