import { expect, test, type Page } from "@playwright/test";
import { appModule, expectScrollsInCard, foldOverview, openApp, reloadApp, row, titles } from "./support/app";

// The list header's column menus (owner feature 2026-09-26): a right-click
// on a title opens that column's menu at the pointer, the columns button
// opens the checklist, Display shows the same list, and the header keeps
// its keys.
const header = (page: Page) => page.locator("[role='row'][aria-rowindex='1']");
const title = (page: Page, name: string) => header(page).locator("[data-col]", { hasText: name });
const cellsOf = (page: Page, name: string) =>
  row(page, name).locator("[data-cell]").evaluateAll((els) => els.map((e) => e.getAttribute("data-cell")));
const names = (page: Page) => page.locator("[role='row'][data-profile-id] [data-cell='name']").allTextContents();
const said = (page: Page) => page.locator("[data-column-status]");
const stored = (page: Page) => page.evaluate(() => JSON.parse(localStorage.getItem("shardx.profiles.layout") ?? "null"));
type Launch = { id: string };
const launched = (page: Page) =>
  page.evaluate(() => ((window as unknown as { __shardxMockLaunches?: Launch[] }).__shardxMockLaunches ?? []).map((l) => l.id));
const ALL = ["Name", "Status", "Folder", "Tags", "Proxy", "Last run", "Created", "Run time", "Device", "Extensions", "Notes"];
const TAGS = { version: 1, tags: [{ id: "t-warm", name: "Warm-up", color: "orange" }], byProfile: { aed41005: ["t-warm"] } };

// A tag exists, so the Tags column is there whatever later rules decide (audit Phase F Task 20).
test.beforeEach(async ({ page }) => {
  await page.addInitScript((v) => { if (localStorage.getItem("shardx.tags") === null) localStorage.setItem("shardx.tags", JSON.stringify(v)); }, TAGS);
});

async function rightClick(page: Page, name: string) {
  const b = (await title(page, name).boundingBox())!;
  await page.mouse.click(b.x + 8, b.y + b.height / 2, { button: "right" });
  return b;
}

test("right-clicking a title opens its column's menu at the pointer: a checklist that stays open while columns come and go", async ({ page }) => {
  await openApp(page);
  const b = await rightClick(page, "Tags");
  const menu = page.getByRole("menu", { name: "Tags column" });
  await expect(menu).toBeVisible();
  expect(Math.abs((await menu.boundingBox())!.x - (b.x + 8))).toBeLessThanOrEqual(2);
  const boxes = menu.getByRole("menuitemcheckbox");
  await expect(boxes).toHaveCount(ALL.length);
  for (const [i, n] of ALL.entries()) await expect(boxes.nth(i)).toHaveAccessibleName(n);
  await expect(menu.getByRole("menuitemcheckbox", { name: "Name" })).toBeDisabled();
  await expect(menu.getByRole("menuitemcheckbox", { name: "Name" })).toHaveAccessibleDescription("Always shown");
  // C10: a disabled checklist item keeps pointer-events, so the hit-test
  // lands on it and the base rule's `cursor: not-allowed` reaches the user
  // (not only the computed style: `elementFromPoint` at its center must be
  // the item itself, or `pointer-events: none` could still sit under a
  // `not-allowed` cursor that never reaches anything, fix round 1 item 9).
  const name = menu.getByRole("menuitemcheckbox", { name: "Name" });
  expect(await name.evaluate((el) => getComputedStyle(el).cursor)).toBe("not-allowed");
  expect(await name.evaluate((el) => {
    const r = el.getBoundingClientRect();
    const hit = document.elementFromPoint(r.x + r.width / 2, r.y + r.height / 2);
    return hit !== null && (hit === el || el.contains(hit));
  })).toBe(true);
  await menu.getByRole("menuitemcheckbox", { name: "Notes" }).click();
  await expect(menu.getByRole("menuitemcheckbox", { name: "Notes" })).toHaveAttribute("aria-checked", "false");
  await expect(menu).toBeVisible();
  expect(await titles(page)).toEqual(["Name", "Status", "Folder", "Tags", "Proxy"]);
});

test("the columns button names itself, opens the checklist alone, says when it is open; Display shows the same list", async ({ page }) => {
  await openApp(page);
  const button = header(page).getByRole("button", { name: "Columns" });
  await expect(button).toHaveAttribute("aria-haspopup", "menu");
  await button.click();
  const menu = page.getByRole("menu", { name: "Columns" });
  await expect(menu).toBeVisible();
  await expect(button).toHaveAttribute("aria-expanded", "true");
  await expect(menu.getByRole("menuitem", { name: "Move left" })).toHaveCount(0);
  for (const [i, n] of ALL.entries()) await expect(menu.getByRole("menuitemcheckbox").nth(i)).toHaveAccessibleName(n);
  await page.keyboard.press("Escape");
  await expect(button).toBeFocused();
  await page.getByRole("button", { name: "Display" }).click();
  const display = page.getByRole("dialog", { name: "Display" });
  for (const [i, n] of ALL.entries()) await expect(display.getByRole("checkbox").nth(i)).toHaveAccessibleName(n);
});

test("the Columns button toggles: a second click closes its own checklist, focus stays on the button (fix round 1 item 6)", async ({ page }) => {
  await openApp(page);
  const button = header(page).getByRole("button", { name: "Columns" });
  await button.click();
  await expect(page.getByRole("menu", { name: "Columns" })).toBeVisible();
  await button.click();
  await expect(page.getByRole("menu")).toHaveCount(0);
  await expect(button).toBeFocused();
});

test("a right-click on the select-all box or the actions column opens the checklist alone, never a row's menu", async ({ page }) => {
  await openApp(page);
  await header(page).getByRole("columnheader", { name: "Actions" }).click({ button: "right", position: { x: 4, y: 8 } });
  await expect(page.getByRole("menu", { name: "Columns" })).toBeVisible();
  await expect(page.getByRole("menuitem", { name: "Move to Trash" })).toHaveCount(0);
  await page.keyboard.press("Escape");
  // The select-all box shows in selection mode only (owner, 2026-10-01).
  await row(page, "Sandbox").locator("[data-cell='checkbox'] input").check({ force: true });
  await header(page).getByRole("columnheader").first().click({ button: "right", position: { x: 2, y: 2 } });
  await expect(page.getByRole("menu", { name: "Columns" })).toBeVisible();
});

test("Move left and Move right move a column among those on screen, say so, and the rows follow; it is kept", async ({ page }) => {
  await openApp(page);
  await rightClick(page, "Tags");
  await page.getByRole("menu", { name: "Tags column" }).getByRole("menuitem", { name: "Move right" }).click();
  expect(await titles(page)).toEqual(["Name", "Status", "Folder", "Proxy", "Tags", "Notes"]);
  await expect(said(page)).toHaveText("Tags moved to position 5 of 6");
  expect(await cellsOf(page, "Etsy craft shop")).toEqual(["checkbox", "name", "status", "folder", "proxy", "tags", "notes", "actions"]);
  await reloadApp(page);
  expect(await titles(page)).toEqual(["Name", "Status", "Folder", "Proxy", "Tags", "Notes"]);
  // The menu's own keys: opened by the pointer, the first arrow reaches Move
  // left, the first item (Pair 30 ruling: nothing is highlighted before).
  await rightClick(page, "Tags");
  await expect(page.getByRole("menu", { name: "Tags column" })).toBeFocused();
  await page.keyboard.press("ArrowDown");
  await expect(page.getByRole("menu", { name: "Tags column" }).getByRole("menuitem", { name: "Move left" })).toBeFocused();
  await page.keyboard.press("Enter");
  expect(await titles(page)).toEqual(["Name", "Status", "Folder", "Tags", "Proxy", "Notes"]);
});

test("Name can't move, and nothing moves before it", async ({ page }) => {
  await openApp(page);
  await rightClick(page, "Name");
  const name = page.getByRole("menu", { name: "Name column" });
  await expect(name).toBeVisible();
  await expect(name.getByRole("menuitem", { name: "Move left" })).toHaveCount(0);
  await expect(name.getByRole("menuitem", { name: "Move right" })).toHaveCount(0);
  await page.keyboard.press("Escape");
  await rightClick(page, "Status");
  await expect(page.getByRole("menu", { name: "Status column" }).getByRole("menuitem", { name: "Move left" })).toBeDisabled();
});

test("a column's order sets the list's; a hidden sort column keeps the order, and its mark comes back with it", async ({ page }) => {
  await page.setViewportSize({ width: 1600, height: 900 });
  await openApp(page);
  await rightClick(page, "Name");
  await page.getByRole("menuitemradio", { name: "Name, Z to A" }).click();
  expect((await names(page)).slice(0, 3)).toEqual(["FB Ads acc-01", "Amazon US seller", "TikTok creator"]);
  await expect(title(page, "Name")).toHaveAttribute("aria-sort", "descending");
  await header(page).getByRole("button", { name: "Columns" }).click();
  await page.getByRole("menuitemcheckbox", { name: "Created" }).click();
  await page.keyboard.press("Escape");
  await rightClick(page, "Created");
  await page.getByRole("menuitemradio", { name: "Oldest first" }).click();
  const oldest = ["Amazon US seller", "FB Ads acc-01", "eBay DE buyer", "Etsy craft shop", "Scraper pricing", "FB Ads acc-02", "TikTok creator", "Sandbox"];
  expect(await names(page)).toEqual(oldest);
  await expect(title(page, "Created")).toHaveAttribute("aria-sort", "ascending");
  await expect(title(page, "Name")).not.toHaveAttribute("aria-sort", /.+/);
  await rightClick(page, "Created");
  await page.getByRole("menuitemcheckbox", { name: "Created" }).click();
  await page.keyboard.press("Escape");
  expect(await names(page)).toEqual(oldest);
  await expect(header(page).locator("[aria-sort]")).toHaveCount(0);
  await page.getByRole("button", { name: "Display" }).click();
  await expect(page.getByRole("dialog", { name: "Display" }).getByRole("combobox", { name: "Order" })).toContainText("Oldest first");
  await page.keyboard.press("Escape");
  await header(page).getByRole("button", { name: "Columns" }).click();
  await page.getByRole("menuitemcheckbox", { name: "Created" }).click();
  await expect(title(page, "Created")).toHaveAttribute("aria-sort", "ascending");
});

test("keys pressed on a header control never act on a row (Review Focus 4)", async ({ page }) => {
  await openApp(page);
  // Etsy craft shop is the active row, the one the list keys act on.
  await row(page, "Etsy craft shop").locator("[data-cell='status']").click();
  await header(page).getByRole("button", { name: "Columns" }).focus();
  for (const key of ["Backspace", "Delete", "s", "x", "e", "t"]) await page.keyboard.press(key);
  await page.waitForTimeout(300);
  await expect(row(page, "Etsy craft shop")).toHaveCount(1);
  await expect(page.locator("[role='row'][aria-selected='true']")).toHaveCount(0);
  // No rename in the list; the click opened Peek, whose title is a Name field (Pair 40).
  await expect(page.locator("[role='row'][data-profile-id]").getByRole("textbox", { name: "Name" })).toHaveCount(0);
  await expect(page.getByRole("dialog", { name: "Tags" })).toHaveCount(0);
  expect(await launched(page)).not.toContain("aed41005");
});

test("ticking Select all keeps the list's keys: S, X, Delete and the arrows still act on the row, like a row checkbox (C6, fix round 1 item 9)", async ({ page }) => {
  await openApp(page);
  // Select all shows on the list's hover or in selection mode (owner,
  // 2026-10-01): a pick first. Sandbox, so X below still ticks the active row, Etsy.
  await row(page, "Sandbox").locator("[data-cell='checkbox'] input").check({ force: true });
  await row(page, "Etsy craft shop").locator("[data-cell='status']").click();
  const selectAll = header(page).getByRole("checkbox", { name: "Select all" });
  await selectAll.focus();
  await page.keyboard.press("x");
  await expect(row(page, "Etsy craft shop")).toHaveAttribute("aria-selected", "true");
  await selectAll.focus();
  await page.keyboard.press("s");
  await expect(row(page, "Etsy craft shop").getByRole("button", { name: "Stop" })).toBeVisible();
  await selectAll.focus();
  await page.keyboard.press("ArrowDown");
  await expect(row(page, "Etsy craft shop")).not.toHaveAttribute("data-active");
  await selectAll.focus();
  // The selection (Etsy, ticked above), not wherever the arrow just left
  // the active row: S stops it, Delete moves it to the Trash.
  await page.keyboard.press("s");
  await selectAll.focus();
  await page.keyboard.press("Delete");
  await expect(row(page, "Etsy craft shop")).toHaveCount(0);
});

test("the Run time order adds a running profile's live session, the same value its cell shows (Batch A review, Minor 3; fix round 1 item 11)", async ({ page }) => {
  await openApp(page);
  await appModule(page, "/src/entities/profile/lib/listing.ts", "__listing");
  const order = await page.evaluate(() => {
    const w = window as unknown as {
      __listing: { sortProfiles: (list: unknown[], sort: string, running: Record<string, number>, now: number) => { id: string }[] };
    };
    const now = Date.now();
    const list = [
      { id: "a", pinned: false, total_runtime_ms: 100_000 },
      { id: "b", pinned: false, total_runtime_ms: 90_000 },
    ];
    // "b" is running, started 20 s ago: its live total (90_000 + 20_000) now
    // passes "a"'s stored 100_000, exactly as the Run time cell would show it.
    const running = { b: now - 20_000 };
    return w.__listing.sortProfiles(list, "runtime-desc", running, now).map((p) => p.id);
  });
  expect(order).toEqual(["b", "a"]);
});

test("Reset width and Reset columns put widths, places and visibility back", async ({ page }) => {
  await page.addInitScript(() => {
    if (localStorage.getItem("shardx.profiles.layout") === null) {
      localStorage.setItem("shardx.profiles.layout", JSON.stringify({ v: 2, order: ["name", "notes", "status", "folder", "tags", "proxy"], hidden: ["folder"], widths: { tags: 150, notes: 120 } }));
    }
  });
  await openApp(page);
  await rightClick(page, "Tags");
  await page.getByRole("menuitem", { name: "Reset width" }).click();
  expect((await stored(page)).widths).toEqual({ notes: 120 });
  await header(page).getByRole("button", { name: "Columns" }).click();
  await page.getByRole("menuitem", { name: "Reset columns" }).click();
  expect(await titles(page)).toEqual(["Name", "Status", "Folder", "Tags", "Proxy", "Notes"]);
  expect((await stored(page)).widths).toEqual({});
});

test("a column turned on in a list too narrow for it joins the list, which scrolls inside its card with Name and the row actions pinned (Review Focus 5)", async ({ page }) => {
  await page.setViewportSize({ width: 1024, height: 680 });
  // Sandbox in view: the open overview (merge point 6) takes the rows' room.
  await foldOverview(page);
  await openApp(page);
  await header(page).getByRole("button", { name: "Columns" }).click();
  for (const n of ["Last run", "Created", "Run time", "Device", "Extensions"]) {
    await page.getByRole("menuitemcheckbox", { name: n }).click();
  }
  const extensions = page.getByRole("menuitemcheckbox", { name: "Extensions" });
  await expect(extensions).toHaveAttribute("aria-checked", "true");
  // Checked, with nothing said against it: it is on the list.
  await expect(extensions).not.toHaveAttribute("aria-describedby");
  await page.keyboard.press("Escape");
  expect(await titles(page)).toEqual(ALL);
  await expectScrollsInCard(page, page.getByRole("treegrid", { name: "Profiles" }), row(page, "Sandbox"), "Pin");
});
