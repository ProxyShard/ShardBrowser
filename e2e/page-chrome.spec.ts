import { expect, test, type Page } from "@playwright/test";
import { appModule, openApp, SECTIONS } from "./support/app";
import { WINDOWS_UA, isTruncated, withLang } from "./support/geometry";
import { openSearch } from "./support/search";

// F4, the page chrome recipe. Tasks 3, 4 and 5 append to this file.
const header = (page: Page) => page.locator("main header").first();
const title = (page: Page) => header(page).locator("h1");

test.describe("on Windows", () => {
  test.use({ userAgent: WINDOWS_UA });

  test("the title is the last thing in the header to give way (LAYOUT-05)", async ({ page }) => {
    await openApp(page);
    await page.locator("aside [data-view='builtin:running']").click();
    // A changed View adds Modified, Reset and Save as view to the header.
    await appModule(page, "/src/entities/profile/lib/useProfileUi.tsx", "__ui");
    await page.evaluate(() => (window as unknown as { __ui: { useProfileUi: { getState: () => { setSort: (s: string) => void } } } }).__ui.useProfileUi.getState().setSort("name-asc"));
    await expect(header(page).getByText("Modified")).toBeVisible();
    await expect(title(page)).toHaveText("Running");
    expect(await isTruncated(title(page))).toBe(false);
  });

  test("every page title is whole at 1024 x 680 in Chinese", async ({ page }) => {
  test.slow(); // opens every page in turn: 9 loads
    await page.setViewportSize({ width: 1024, height: 680 });
    await withLang(page, "zh");
    for (const section of SECTIONS) {
      await openApp(page, { section });
      expect(await isTruncated(title(page)), section).toBe(false);
    }
  });
});

test("the filter is a search button at rest, narrow or not; / unfolds it", async ({ page }) => {
  await page.setViewportSize({ width: 800, height: 700 });
  await openApp(page);
  const field = header(page).getByRole("searchbox");
  // Exact: the Filters menu button sits beside it.
  const button = header(page).getByRole("button", { name: "Search…", exact: true });
  await expect(field).toBeHidden();
  await expect(button).toBeVisible();
  await page.locator("body").press("/");
  await expect(field).toBeFocused();
  await field.fill("fb");
  await expect(page.locator("[role='row'][data-profile-id]")).toHaveCount(2);
  await page.locator("main").click({ position: { x: 5, y: 300 } });
  // It keeps what was typed, so it stays open.
  await expect(field).toBeVisible();
});

test("the header draws its rule only while the page under it is scrolled (LQ-09)", async ({ page }) => {
  await page.setViewportSize({ width: 1280, height: 500 });
  await openApp(page, { section: "settings" });
  await page.getByRole("tab", { name: "Profile defaults" }).click();
  await expect(header(page)).not.toHaveAttribute("data-scrolled", /.*/);
  await page.locator("main [data-page-body]").evaluate((el) => { el.scrollTop = 200; el.dispatchEvent(new Event("scroll")); });
  await expect(header(page)).toHaveAttribute("data-scrolled", "true");
});

test("a list that scrolls itself never draws the header rule", async ({ page }) => {
  await page.addInitScript(() => Object.assign(window, { __shardxMockProfiles: 200 }));
  await openApp(page);
  await page.locator("main [aria-label='Profiles']").first().evaluate((el) => { el.scrollTop = 900; el.dispatchEvent(new Event("scroll")); });
  await expect(header(page)).not.toHaveAttribute("data-scrolled", /.*/);
  // The rule is the header's ::after, laid over its bottom edge (Lane D, scroll-edge with no layout).
  expect(await header(page).first().evaluate((el) => getComputedStyle(el, "::after").backgroundColor)).toBe("rgba(0, 0, 0, 0)");
});

test("a search says how many of how many (CRAFT-18)", async ({ page }) => {
  await openApp(page);
  await (await openSearch(page)).fill("fb");
  await expect(header(page).locator("[data-count]")).toHaveText("2 of 8");
  await (await openSearch(page)).fill("");
  await expect(header(page).locator("[data-count]")).toHaveText("8");
  await openApp(page, { section: "proxies" });
  await (await openSearch(page)).fill("tokyo");
  await expect(header(page).locator("[data-count]")).toHaveText("1 of 7");
});

for (const section of SECTIONS) {
  test(`${section}: headings never skip a level (TYPE-10)`, async ({ page }) => {
    await openApp(page, { section });
    const levels = await page.locator("main").evaluate((m) =>
      [...m.querySelectorAll("h1, h2, h3, h4, h5, h6")].map((h) => Number(h.tagName[1])));
    expect(levels[0]).toBe(1);
    for (let i = 1; i < levels.length; i++) {
      expect(levels[i], `heading ${i} on ${section}: ${levels.join(",")}`).toBeLessThanOrEqual(Math.max(...levels.slice(0, i)) + 1);
    }
  });
}

// F4 Task 3: one action recipe on every header.
const menuItem = (page: Page, name: string) => page.getByRole("menuitem", { name });
async function openMore(page: Page) {
  await header(page).getByRole("button", { name: "More actions" }).click();
  await expect(page.getByRole("menu")).toBeVisible();
}

test("Profiles: the … menu holds import and New folder (EMIL-20)", async ({ page }) => {
  await openApp(page);
  await openMore(page);
  await expect(menuItem(page, "Import profiles from clipboard")).toBeVisible();
  await menuItem(page, "New folder").click();
  await expect(page.getByRole("dialog", { name: /New folder/ })).toBeVisible();
});

test("Proxies: one primary, Buy in the … menu (CRAFT-25)", async ({ page }) => {
  await openApp(page, { section: "proxies" });
  await expect(header(page).getByRole("button", { name: /Buy/ })).toHaveCount(0);
  await expect(header(page).getByRole("button", { name: "Add proxies" })).toBeVisible();
  await openMore(page);
  await expect(menuItem(page, "Import proxies from clipboard")).toBeVisible();
  await expect(menuItem(page, "Buy proxies")).toBeVisible();
});

// Pair 3 review, Minor 6: Buy proxies swallowed an opener failure; the
// menu and the empty list now share one handler that says so.
test("Buy proxies says so when the browser cannot open", async ({ page }) => {
  await page.addInitScript(() => Object.assign(window, { __shardxMockFail: { "plugin:opener|open_url": 1 } }));
  await openApp(page, { section: "proxies" });
  await openMore(page);
  await menuItem(page, "Buy proxies").click();
  await expect(page.locator("[data-toast]").filter({ hasText: "Could not open shardx.com" })).toBeVisible();
});

// One line goes through the preview too, so the proxy can be tested before
// it is added; once added, it opens in Peek (CRAFT-26).
test("Add proxies with one line previews it with its test, then opens it in Peek once added", async ({ page }) => {
  await openApp(page, { section: "proxies" });
  await header(page).getByRole("button", { name: "Add proxies" }).click();
  const dialog = page.getByRole("dialog", { name: "Add proxies" });
  await dialog.getByRole("textbox").fill("socks5://198.51.100.99:1080");
  await expect(dialog.getByRole("button", { name: "Add proxy", exact: true })).toHaveCount(0);
  await dialog.getByRole("button", { name: "Preview" }).click();
  await expect(dialog.getByText("1 of 1 selected")).toBeVisible();
  await expect(dialog.getByRole("button", { name: "Test all" })).toBeVisible();
  await expect(dialog.getByRole("button", { name: "Test this row" })).toBeVisible();
  await dialog.getByRole("button", { name: "Test this row" }).click();
  // The row's own result: Active (its TCP time in the tooltip) or Failed.
  await expect(dialog.getByText(/^(Active|Failed)$/)).toBeVisible();
  await dialog.getByRole("button", { name: "Add 1 proxy" }).click();
  await expect(dialog).toHaveCount(0);
  await expect(page.locator("aside[data-peek='proxy']")).toHaveAttribute("aria-label", "Peek: 198.51.100.99:1080");
});

// Pair 2 review, Minor 1: Add proxy had no busy guard (a second press
// saved the proxy twice) and closed with no word when the proxy it saved
// was not in the list read back.
test("Add proxy saves once, however fast it is pressed again", async ({ page }) => {
  await openApp(page, { section: "proxies" });
  await header(page).getByRole("button", { name: "Add proxies" }).click();
  const dialog = page.getByRole("dialog", { name: "Add proxies" });
  await dialog.getByRole("textbox").fill("socks5://198.51.100.96:1080");
  await dialog.getByRole("button", { name: "Preview" }).click();
  await dialog.getByRole("button", { name: "Add 1 proxy" }).dblclick();
  await expect(page.locator("aside[data-peek='proxy']")).toHaveAttribute("aria-label", "Peek: 198.51.100.96:1080");
  expect(await page.evaluate(() => (window as unknown as { __shardxMock: { calls: Record<string, number> } }).__shardxMock.calls.proxy_bulk_save)).toBe(1);
});

test("Add proxy says it was added when the list read back does not show it", async ({ page }) => {
  await openApp(page, { section: "proxies" });
  await header(page).getByRole("button", { name: "Add proxies" }).click();
  const dialog = page.getByRole("dialog", { name: "Add proxies" });
  await dialog.getByRole("textbox").fill("socks5://198.51.100.95:1080");
  await dialog.getByRole("button", { name: "Preview" }).click();
  await page.evaluate(() => Object.assign(window, { __shardxMockFail: { proxy_list: 1 } }));
  await dialog.getByRole("button", { name: "Add 1 proxy" }).click();
  await expect(page.locator("[data-toast]").filter({ hasText: "Imported 1 proxy" })).toBeVisible();
  await expect(dialog).toHaveCount(0);
});

test("Add proxies with several lines previews them first (WRITE-18)", async ({ page }) => {
  await openApp(page, { section: "proxies" });
  await header(page).getByRole("button", { name: "Add proxies" }).click();
  const dialog = page.getByRole("dialog", { name: "Add proxies" });
  await dialog.getByRole("textbox").fill("socks5://198.51.100.98:1080\nsocks5://198.51.100.97:1080");
  await dialog.getByRole("button", { name: "Preview" }).click();
  await expect(dialog.getByRole("button", { name: "Add 2 proxies" })).toBeVisible();
});

test("Fingerprints: imports in the … menu, no file format as the primary (CRAFT-29)", async ({ page }) => {
  await openApp(page, { section: "fingerprints" });
  await expect(header(page).getByRole("button", { name: "Paste JSON" })).toHaveCount(0);
  await openMore(page);
  for (const name of ["Import from file", "Paste JSON", "Show library folder"]) await expect(menuItem(page, name)).toBeVisible();
});

test("Extensions: one Add extension button lists the three sources (CRAFT-29)", async ({ page }) => {
  await openApp(page, { section: "extensions" });
  await header(page).getByRole("button", { name: "Add extension" }).click();
  for (const name of ["From a link", ".crx or .zip file", "Unpacked folder"]) await expect(menuItem(page, name)).toBeVisible();
  await menuItem(page, "From a link").click();
  await expect(page.getByRole("dialog")).toBeVisible();
});

test("Trash: Empty Trash sits in the … menu", async ({ page }) => {
  await openApp(page, { section: "trash" });
  await expect(header(page).getByRole("button", { name: /Empty Trash/i })).toHaveCount(0);
  await openMore(page);
  // The palette's words for the same command (E10: one name per command).
  await expect(menuItem(page, "Empty Trash")).toBeVisible();
});

// Merge point 5 review (Lanes C and E): a one-column page caps its body at
// 760 px; its header's controls end at that column's right edge too.
test("a one-column page's header ends where its column ends (ProxyShard)", async ({ page }) => {
  await openApp(page, { section: "proxyshard" });
  const last = header(page).locator("button").last();
  await expect(last).toBeVisible();
  const end = await last.evaluate((el) => el.getBoundingClientRect().right);
  const column = await page.locator("main [data-page-body] > div").first().evaluate((el) => el.getBoundingClientRect().right);
  expect(Math.abs(end - column)).toBeLessThanOrEqual(1);
});

// Owner, 2026-10-01: every page's content starts on the sidebar's line. The
// first thing the page body paints (a card's or list's edge, a fill, text)
// has its top at the top of the sidebar's first row (Search), with the
// sidebar open and as the rail. A block led by text may start its text or
// its row on the line (Profiles' CONTINUE heading, merge point 6: its row on
// the line, its text where the Search row's own label sits), so text lies
// between the line and the Search label's inset under it.
const searchTops = (page: Page) => page.locator("aside [data-nav='search']").evaluate((row) => {
  const label = [...row.querySelectorAll<HTMLElement>("*")]
    .find((el) => [...el.childNodes].some((n) => n.nodeType === Node.TEXT_NODE && (n.textContent ?? "").trim() !== ""));
  return { row: row.getBoundingClientRect().top, label: label ? label.getBoundingClientRect().top : null };
});
const firstPaint = (page: Page) => page.locator("main [data-page-body]").evaluate((body) => {
  let top = Infinity;
  let text = false;
  for (const el of body.querySelectorAll<HTMLElement>("*")) {
    const r = el.getBoundingClientRect();
    if (r.width === 0 || r.height === 0 || r.top >= top) continue;
    if (!el.checkVisibility({ opacityProperty: true, visibilityProperty: true })) continue;
    const s = getComputedStyle(el);
    const edge = parseFloat(s.borderTopWidth) > 0 || s.backgroundColor !== "rgba(0, 0, 0, 0)";
    const words = [...el.childNodes].some((n) => n.nodeType === Node.TEXT_NODE && (n.textContent ?? "").trim() !== "");
    if (edge || words) { top = r.top; text = !edge; }
  }
  return { top, text };
});

test("every page's content starts on the sidebar's line, right under the header", async ({ page }) => {
  test.slow(); // opens every page in turn: 9 loads
  // The Search label's inset in its row, read with the sidebar open (the
  // rail shows no label).
  let inset = 0;
  const offLine = async () => {
    const s = await searchTops(page);
    if (s.label !== null) inset = s.label - s.row;
    const f = await firstPaint(page);
    const off = f.top - s.row;
    return f.text && off > 0 ? Math.max(0, off - inset) : Math.abs(off);
  };
  for (const section of SECTIONS) {
    await openApp(page, { section });
    await expect(page.locator(`main[data-section='${section}']`)).toBeVisible();
    expect(await offLine(), section).toBeLessThanOrEqual(1);
  }
  // The rail: the same line.
  await page.addInitScript(() => localStorage.setItem("shardx.sidebar", JSON.stringify({ v: 1, collapsed: {}, foldersOpen: false, rail: true })));
  await openApp(page);
  expect((await page.locator("aside").first().boundingBox())!.width).toBeLessThan(100);
  expect(await offLine(), "profiles, rail").toBeLessThanOrEqual(1);
});

// Task 4: intros behind the info button; empty pages offer their first
// action; empty lists hide their controls.
test("page intros live behind the info button, not above the content (LAYOUT-19)", async ({ page }) => {
  for (const [section, title, words] of [
    ["fingerprints", "About the fingerprint library", /GPU/], ["trash", "About Trash", /7 days/],
    ["extensions", "About extensions", /for each profile in its details panel/], ["bookmarks", "About bookmarks", /every profile in that folder/],
  ] as const) {
    await openApp(page, { section });
    await expect(page.locator("main [data-page-body] > p")).toHaveCount(0);
    // The intro is a popover the (i) opens (clarity fixes, item 7), not a tooltip.
    await header(page).locator("[data-title-block]").getByRole("button", { name: "About this page" }).click();
    await expect(page.getByRole("dialog", { name: title })).toContainText(words);
  }
});

test.describe("on a fresh install", () => {
  test.beforeEach(async ({ page }) => { await page.addInitScript(() => Object.assign(window, { __shardxMockEmpty: true })); });

  test("an empty list shows no filter, count or view controls (LAYOUT-20)", async ({ page }) => {
    await openApp(page);
    await expect(header(page).getByRole("searchbox")).toHaveCount(0);
    await expect(header(page).locator("[data-count]")).toHaveCount(0);
    await expect(header(page).getByRole("button", { name: /^Filters/ })).toHaveCount(0);
    await expect(header(page).getByRole("button", { name: "Display" })).toHaveCount(0);
    await openApp(page, { section: "proxies" });
    await expect(header(page).getByRole("searchbox")).toHaveCount(0);
  });

  test("each empty page offers its first action in the middle (CRAFT-06, CRAFT-25)", async ({ page }) => {
    await openApp(page, { section: "extensions" });
    // Clarity fixes, item 17: a zero state is the kit's one empty page: an
    // icon, the title, one hint and the page's main action as a filled
    // primary button (30 px, the header's size), inside the list's card.
    const oneTile = async (name: string) => {
      const empty = page.locator("main [data-empty-state]");
      await expect(empty.locator("[data-empty-action]")).toHaveCount(1);
      const tile = empty.getByRole("button", { name });
      await expect(tile).toBeVisible();
      await expect(tile).toHaveClass(/bg-primary-base/);
      const box = await tile.boundingBox();
      expect(box!.height).toBe(30);
      await expect(empty.locator("p")).toHaveCount(1);
    };
    await openApp(page, { section: "extensions" });
    await oneTile("Add an extension");
    await openApp(page, { section: "bookmarks" });
    await oneTile("Add a bookmark");
    await openApp(page, { section: "proxies" });
    await oneTile("Add proxies");
    // The hint's quiet text button opens the same Buy flow as the header menu.
    await expect(page.locator("main [data-empty-state] p").getByRole("button", { name: "or buy from ProxyShard" })).toBeVisible();
  });

  test("the Automation tile creates and opens a project", async ({ page }) => {
    type A = { __auto: { useAutomation: { setState: (s: object) => void; getState: () => { openId: string | null; projects: { name: string }[] } } } };
    await openApp(page, { section: "automation" });
    await appModule(page, "/src/entities/automation/index.ts", "__auto");
    await page.evaluate(() => (window as unknown as A).__auto.useAutomation.setState({ projects: [] }));
    await page.locator("main [data-empty-state]").getByRole("button", { name: "New project" }).click();
    await expect.poll(() => page.evaluate(() => (window as unknown as A).__auto.useAutomation.getState().openId)).not.toBeNull();
    await expect.poll(() => page.evaluate(() => (window as unknown as A).__auto.useAutomation.getState().projects.map((p) => p.name))).toContain("New project");
  });

  test("the empty page's button is a real button with a visible focus ring", async ({ page }) => {
    await openApp(page, { section: "bookmarks" });
    const tile = page.locator("main [data-empty-state]").getByRole("button", { name: "Add a bookmark" });
    await tile.focus();
    await page.keyboard.press("Shift+Tab");
    await page.keyboard.press("Tab");
    await expect(tile).toBeFocused();
    await expect(tile).toHaveCSS("outline-style", "solid");
    expect(await tile.evaluate((el) => parseFloat(getComputedStyle(el).outlineWidth))).toBeGreaterThanOrEqual(2);
    await page.keyboard.press("Enter");
    await expect(page.getByRole("dialog", { name: "New bookmark" })).toBeVisible();
  });
});

// Pair 3 review, Minor 3: a list emptied under a search or filters kept
// saying "No profiles match" with Clear search while the header hid the
// field (and the Filters menu) that narrowed it. As Proxies: an empty list
// hides them only with no search and no filters on.
test("Profiles emptied under a search or filters keeps the field and Filters (LAYOUT-20)", async ({ page }) => {
  type P = { __profile: { useProfile: { setState: (s: object) => void }; useProfileUi: { getState: () => { setFilters: (f: object) => void } } } };
  await openApp(page);
  await appModule(page, "/src/entities/profile/index.ts", "__profile");
  const field = await openSearch(page);
  await field.fill("zzzz");
  await page.evaluate(() => (window as unknown as P).__profile.useProfile.setState({ profiles: [] }));
  await expect(page.getByText("No profiles match")).toBeVisible();
  await expect(field).toHaveValue("zzzz");
  await field.fill("");
  await expect(field).toHaveCount(0);
  await page.evaluate(() => (window as unknown as P).__profile.useProfileUi.getState().setFilters({ status: "running" }));
  await expect(page.getByText("No profiles match these filters")).toBeVisible();
  await expect(header(page).getByRole("button", { name: /^Filters/ })).toBeVisible();
  await expect(field).toBeVisible();
});

// Pair 3 review, Minor 9: the Folder chips hide once no bookmark is left,
// so the chip that was selected no longer shows, and the empty state's New
// bookmark still went into that Folder. The last bookmark gone, the page is
// back on All.
test("Bookmarks: deleting the last bookmark in a Folder goes back to All", async ({ page }) => {
  await openApp(page, { section: "bookmarks" });
  const main = page.locator("main");
  // F26: Delete is in the row's menu, not a red button on every row.
  const rowOf = (title: string) => main.locator("[role='row'][data-row-id]").filter({ has: page.locator("[data-cell='name']").getByText(title, { exact: true }) });
  const remove = async (title: string) => {
    await rowOf(title).hover();
    await rowOf(title).getByRole("button", { name: "More actions" }).click();
    // Behind an Undo toast, nothing asked (ruling 18): the row goes at once.
    await page.getByRole("menuitem", { name: "Delete" }).click();
    await expect(rowOf(title)).toHaveCount(0);
  };
  for (const title of ["Meta Business Suite", "browserleaks.com", "pixelscan.net"]) await remove(title);
  await main.getByRole("radio", { name: "Shops", exact: true }).click();
  await expect(main.getByRole("radio", { name: "Shops", exact: true })).toHaveAttribute("aria-checked", "true");
  for (const title of ["Seller Central", "eBay Seller Hub"]) await remove(title);
  await main.locator("[data-empty-state]").getByRole("button", { name: "Add a bookmark" }).click();
  await expect(page.getByRole("dialog", { name: "New bookmark" }).getByText("Every profile", { exact: true })).toBeVisible();
});

test("an empty fingerprint library says what to do, with the two ways (WRITE-14)", async ({ page }) => {
  await page.addInitScript(() => Object.assign(window, { __shardxMockNoFingerprints: true }));
  await openApp(page, { section: "fingerprints" });
  const empty = page.locator("main [data-empty-state]");
  await expect(empty).toContainText("No fingerprints yet");
  await expect(empty.getByRole("button")).toHaveCount(1);
  await expect(empty.getByRole("button", { name: "Import a fingerprint" })).toBeVisible();
});

// Task 5: titles equal their nav labels; the pages hold at 960 x 640.
test("each page's title is its sidebar label (CRAFT-29)", async ({ page }) => {
  test.slow(); // opens every page in turn: 9 loads
  for (const section of SECTIONS) {
    await openApp(page, { section });
    const nav = page.locator("aside [aria-current='page']").first();
    await expect(title(page)).toHaveText((await nav.innerText()).split("\n")[0].trim());
  }
});

// On Windows, where the caption buttons take the header's top-right corner
// (the tightest header F5 makes), whatever the host running the test is.
test.describe("on Windows", () => {
  test.use({ userAgent: WINDOWS_UA });

  test("every page holds at 960 x 640: nothing scrolls sideways, titles are whole, actions clear the captions", async ({ page }) => {
  test.slow(); // opens every page in turn: 9 loads
    await page.setViewportSize({ width: 960, height: 640 });
    for (const section of SECTIONS) {
      await openApp(page, { section });
      expect(await page.evaluate(() => document.documentElement.scrollWidth), section).toBeLessThanOrEqual(960);
      expect(await isTruncated(title(page)), section).toBe(false);
      const minimize = (await page.locator("[data-captions]").getByRole("button", { name: "Minimize" }).boundingBox())!;
      const actions = header(page).locator("> div.ml-auto");
      if (await actions.count()) {
        const box = (await actions.boundingBox())!;
        expect(box.x + box.width, section).toBeLessThanOrEqual(minimize.x - 8);
      }
    }
  });
});
