import { expect, test, type Locator, type Page } from "@playwright/test";
import { openApp, row } from "./support/app";
import { openSearch } from "./support/search";

// Selection mode (owner, 2026-10-01). At rest a list shows no checkboxes,
// in its rows or its header, and its select column keeps its own slot
// before Name (owner, 2026-10-07, item 9: a box laid over the name hid its
// start); hovering a row fades in that row's box in its slot, hovering the
// list fades in Select all (owner, 2026-11-04), and nothing moves. The
// first pick (a box, Cmd/Ctrl+click or Shift+click on a row, X,
// a long press on touch) shows every box with Select all and brings up the
// bulk bar; Esc, the bar's X or an empty selection put it all back. Name
// never moves. Profiles here; Proxies and the Trash (the kit DataList) below.

const grid = (page: Page) => page.getByRole("treegrid", { name: "Profiles" });
const header = (page: Page) => grid(page).locator("[data-list-header]");
const bar = (page: Page) => page.getByRole("toolbar", { name: /selected/ });
const peek = (page: Page) => page.locator("aside[data-peek]");
const polite = (page: Page) => page.locator("[data-live='polite']");
/** The boxes a person (and a screen reader) can see: hidden ones are out of the role tree. */
const boxes = (scope: Locator) => scope.getByRole("checkbox");
const width = async (l: Locator) => Math.round((await l.boundingBox())!.width);
const left = async (l: Locator) => Math.round((await l.boundingBox())!.x);
/** Where every row's Name and every column title start: what must not move on hover. */
const layoutOf = (page: Page) => grid(page).evaluate((el) =>
  [...el.querySelectorAll("[data-cell='name'], [data-list-header] [data-col]")].map((c) => Math.round(c.getBoundingClientRect().x)));

test("at rest no box shows, in the rows or the header, and the select column keeps its slot", async ({ page }) => {
  await openApp(page, { theme: "light" });
  await expect(grid(page)).not.toHaveClass(/has-selection/);
  await expect(boxes(grid(page))).toHaveCount(0);
  expect(await width(header(page).locator(".list-pin-check"))).toBe(38);
  const r = row(page, "Sandbox");
  expect(await width(r.locator("[data-cell='checkbox']"))).toBe(38);
  // Name starts past the box's slot: the cell padding, the 28 px column and its 10 px gap.
  expect(await left(r.locator("[data-cell='name']")) - (await left(r))).toBe(50);
  await expect(bar(page)).toHaveCount(0);
});

test("hovering the list, its header included, fades in Select all alone, and nothing moves", async ({ page }) => {
  await openApp(page, { theme: "light" });
  const before = await layoutOf(page);
  await header(page).locator("[data-col='status']").hover();
  await expect(header(page).getByRole("checkbox", { name: "Select all" })).toBeVisible();
  await expect(boxes(grid(page))).toHaveCount(1);
  expect(await layoutOf(page)).toEqual(before);
  await page.mouse.move(1, 1);
  await expect(boxes(grid(page))).toHaveCount(0);
});

test("hovering a row fades in its box alone, in its own slot before the name, and nothing moves", async ({ page }) => {
  await openApp(page, { theme: "light" });
  const before = await layoutOf(page);
  const r = row(page, "Sandbox");
  await r.locator("[data-cell='status']").hover();
  const box = r.getByRole("checkbox", { name: "Select Sandbox" });
  await expect(box).toBeVisible();
  // Its own, and Select all on the list's hover.
  await expect(boxes(grid(page))).toHaveCount(2);
  expect(await layoutOf(page)).toEqual(before);
  // At the cell padding, clear of the name, on top.
  expect(await left(box) - (await left(r))).toBe(12);
  expect((await box.boundingBox())!.x + (await box.boundingBox())!.width).toBeLessThan(await left(r.locator("[data-cell='name']")));
  expect(await box.evaluate((el) => {
    const b = el.getBoundingClientRect();
    return document.elementFromPoint(b.x + b.width / 2, b.y + b.height / 2) === el;
  })).toBe(true);
  // Nothing to cover: no fill or fade of its own.
  const overlay = await r.locator(".list-select-box").evaluate((el) => {
    const cs = getComputedStyle(el);
    return { fill: cs.backgroundImage !== "none" || cs.backgroundColor !== "rgba(0, 0, 0, 0)", mask: cs.maskImage || cs.webkitMaskImage };
  });
  expect(overlay.fill).toBe(false);
  expect(overlay.mask).toBe("none");
});

test("a box's click enters selection mode: every box and Select all show, the bar comes up, and Name stays put", async ({ page }) => {
  await openApp(page, { theme: "light" });
  const r = row(page, "Sandbox");
  const other = row(page, "Etsy craft shop");
  const nameAt = await left(r.locator("[data-cell='name']"));
  const otherAt = await left(other.locator("[data-cell='name']"));
  await r.locator("[data-cell='status']").hover();
  await r.getByRole("checkbox", { name: "Select Sandbox" }).click();
  await expect(r).toHaveAttribute("aria-selected", "true");
  await expect(grid(page)).toHaveClass(/has-selection/);
  await expect(bar(page)).toContainText("1 selected");
  await expect(polite(page)).toHaveText("1 selected");
  await expect(header(page).getByRole("checkbox", { name: "Select all" })).toBeVisible();
  const rows = await grid(page).locator("[role='row'][data-profile-id]").count();
  await expect(boxes(grid(page))).toHaveCount(rows + 1);
  expect(await width(header(page).locator(".list-pin-check"))).toBe(38);
  expect(await left(r.locator("[data-cell='name']"))).toBe(nameAt);
  expect(await left(other.locator("[data-cell='name']"))).toBe(otherAt);
  await expect(peek(page)).toHaveCount(0);
  // In the mode a plain click still opens Peek, and the selection stays.
  await other.locator("[data-cell='status']").click();
  await expect(peek(page)).toBeVisible();
  await expect(r).toHaveAttribute("aria-selected", "true");
  await expect(other).toHaveAttribute("aria-selected", "false");
});

test("Esc leaves selection mode: the boxes, Select all and the bar go, the column keeps its slot, and the row keeps the keyboard", async ({ page }) => {
  await openApp(page, { theme: "light" });
  const r = row(page, "Sandbox");
  const nameAt = await left(r.locator("[data-cell='name']"));
  const before = await layoutOf(page);
  await r.locator("[data-cell='status']").hover();
  await r.getByRole("checkbox", { name: "Select Sandbox" }).click();
  await expect(grid(page)).toHaveClass(/has-selection/);
  await r.focus();
  await page.mouse.move(1, 1);
  await page.keyboard.press("Escape");
  await expect(grid(page)).not.toHaveClass(/has-selection/);
  await expect(bar(page)).toHaveCount(0);
  await expect(boxes(grid(page))).toHaveCount(0);
  await expect(polite(page)).toHaveText("Selection cleared");
  expect(await left(r.locator("[data-cell='name']"))).toBe(nameAt);
  expect(await layoutOf(page)).toEqual(before);
  expect(await width(header(page).locator(".list-pin-check"))).toBe(38);
  await expect(r).toBeFocused();
});

test("unticking the last box leaves the mode with the keyboard on its row, never on a hidden box", async ({ page }) => {
  await openApp(page, { theme: "light" });
  const r = row(page, "Sandbox");
  await r.locator("[data-cell='status']").hover();
  const box = r.getByRole("checkbox", { name: "Select Sandbox" });
  await box.focus();
  await page.keyboard.press("Space");
  await expect(grid(page)).toHaveClass(/has-selection/);
  await page.keyboard.press("Space");
  await expect(grid(page)).not.toHaveClass(/has-selection/);
  await expect(r).toBeFocused();
  // Select all, ticked and unticked from the keyboard with the pointer away:
  // focused, it stays shown and keeps the keyboard.
  const all = header(page).getByRole("checkbox", { name: "Select all" });
  await all.focus();
  await page.mouse.move(1, 1);
  await page.keyboard.press("Space");
  await expect(all).toBeChecked();
  await expect(grid(page)).toHaveClass(/has-selection/);
  await page.keyboard.press("Space");
  await expect(grid(page)).not.toHaveClass(/has-selection/);
  await expect(all).toBeFocused();
  await expect(all).toBeVisible();
});

test("Cmd/Ctrl+click and Shift+click on a row enter selection mode and open no Peek", async ({ page }) => {
  await openApp(page, { theme: "light" });
  await row(page, "Sandbox").locator("[data-cell='status']").click({ modifiers: ["ControlOrMeta"] });
  await expect(row(page, "Sandbox")).toHaveAttribute("aria-selected", "true");
  await expect(grid(page)).toHaveClass(/has-selection/);
  await expect(peek(page)).toHaveCount(0);
  await page.keyboard.press("Escape");
  await expect(grid(page)).not.toHaveClass(/has-selection/);
  await row(page, "Etsy craft shop").locator("[data-cell='status']").click({ modifiers: ["Shift"] });
  await expect(row(page, "Etsy craft shop")).toHaveAttribute("aria-selected", "true");
  await expect(grid(page)).toHaveClass(/has-selection/);
  await expect(peek(page)).toHaveCount(0);
});

test("X on the focused row enters selection mode", async ({ page }) => {
  await openApp(page, { theme: "light" });
  await page.keyboard.press("ArrowDown");
  const active = grid(page).locator("[role='row'][data-active]");
  await expect(active).toBeFocused();
  await page.keyboard.press("x");
  await expect(active).toHaveAttribute("aria-selected", "true");
  await expect(grid(page)).toHaveClass(/has-selection/);
  await expect(bar(page)).toContainText("1 selected");
});

test.describe("on touch", () => {
  test.use({ hasTouch: true });

  test("holding a row selects it and enters selection mode; the hold opens no Peek and no menu, a tap still opens Peek", async ({ page }) => {
    await openApp(page, { theme: "light" });
    const r = row(page, "Sandbox");
    const b = (await r.locator("[data-cell='status']").boundingBox())!;
    const cdp = await page.context().newCDPSession(page);
    await cdp.send("Input.dispatchTouchEvent", { type: "touchStart", touchPoints: [{ x: b.x + b.width / 2, y: b.y + b.height / 2 }] });
    await expect(r).toHaveAttribute("aria-selected", "true", { timeout: 3000 });
    await cdp.send("Input.dispatchTouchEvent", { type: "touchEnd", touchPoints: [] });
    await expect(grid(page)).toHaveClass(/has-selection/);
    await page.waitForTimeout(300);
    await expect(peek(page)).toHaveCount(0);
    await expect(page.getByRole("menu")).toHaveCount(0);
    await expect(r).toHaveAttribute("aria-selected", "true");
    const t = (await row(page, "Etsy craft shop").locator("[data-cell='status']").boundingBox())!;
    await page.touchscreen.tap(t.x + t.width / 2, t.y + t.height / 2);
    await expect(peek(page)).toBeVisible();
    await expect(row(page, "Etsy craft shop")).toHaveAttribute("aria-selected", "false");
  });
});

test("at 1024 the select column's slot leaves no sideways scroll, at rest or in selection mode", async ({ page }) => {
  await page.setViewportSize({ width: 1024, height: 680 });
  await openApp(page, { theme: "light" });
  const over = () => grid(page).evaluate((el) => el.scrollWidth - el.clientWidth);
  expect(await over()).toBeLessThanOrEqual(0);
  expect(await width(header(page).locator(".list-pin-check"))).toBe(38);
  await row(page, "Sandbox").locator("[data-cell='checkbox'] input").check({ force: true });
  await expect(grid(page)).toHaveClass(/has-selection/);
  await expect.poll(over).toBeLessThanOrEqual(0);
  expect(await width(header(page).locator(".list-pin-check"))).toBe(38);
  await page.keyboard.press("Escape");
  await expect(grid(page)).not.toHaveClass(/has-selection/);
  await expect.poll(over).toBeLessThanOrEqual(0);
});

test("Proxies: no box at rest, in the column's own slot; the first pick shows every box and Select all", async ({ page }) => {
  await openApp(page, { section: "proxies", theme: "light" });
  const list = page.getByRole("treegrid", { name: "Proxies" });
  await expect(boxes(list)).toHaveCount(0);
  expect(await width(list.locator("[data-list-header] .list-pin-check"))).toBe(38);
  const r = list.locator("[role='row'][data-proxy-id]").first();
  await r.locator("[data-cell='type']").click({ modifiers: ["ControlOrMeta"] });
  await expect(list).toHaveClass(/has-selection/);
  await expect(list.locator("[data-list-header]").getByRole("checkbox", { name: "Select all" })).toBeVisible();
  await expect(boxes(list)).toHaveCount((await list.locator("[role='row'][data-proxy-id]").count()) + 1);
  expect(await width(list.locator("[data-list-header] .list-pin-check"))).toBe(38);
  await page.keyboard.press("Escape");
  await expect(list).not.toHaveClass(/has-selection/);
});

test("the Trash (kit DataList): no box at rest; a row's click enters the mode, Esc leaves it, both said in the list's live region", async ({ page }) => {
  await openApp(page, { section: "trash", theme: "light" });
  const box = page.locator("main .list-select");
  const rows = box.locator("[role='row'][data-row-id]");
  await expect(rows.first()).toBeVisible();
  await expect(boxes(box)).toHaveCount(0);
  expect(await width(box.locator("[data-list-header] .list-pin-check"))).toBe(38);
  await rows.first().locator("[data-cell='name']").click();
  await expect(rows.first()).toHaveAttribute("aria-selected", "true");
  await expect(box).toHaveClass(/has-selection/);
  await expect(box.getByRole("status")).toHaveText("1 selected");
  await expect(box.locator("[data-list-header]").getByRole("checkbox", { name: "Select all" })).toBeVisible();
  expect(await width(box.locator("[data-list-header] .list-pin-check"))).toBe(38);
  await page.mouse.move(1, 1);
  await page.keyboard.press("Escape");
  await expect(box).not.toHaveClass(/has-selection/);
  await expect(box.getByRole("status")).toHaveText("Selection cleared");
  await expect(boxes(box)).toHaveCount(0);
});

test("the Trash: a search that hides every pick drops them, so the mode and the bar leave together", async ({ page }) => {
  await openApp(page, { section: "trash", theme: "light" });
  const box = page.locator("main .list-select");
  const rows = box.locator("[role='row'][data-row-id]");
  await expect(rows.first()).toBeVisible();
  await rows.first().locator("[data-cell='name']").click();
  await expect(box).toHaveClass(/has-selection/);
  await (await openSearch(page)).fill("zzz-no-such-entry");
  await expect(box).not.toHaveClass(/has-selection/);
  await expect(bar(page)).toHaveCount(0);
});
