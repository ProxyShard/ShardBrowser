import { expect, test, type Page } from "@playwright/test";
import { openApp, pressCreate } from "./support/app";
import { openSearch } from "./support/search";

const palette = (page: Page) => page.getByRole("dialog", { name: "Command palette" });
const search = (page: Page) => palette(page).getByRole("combobox", { name: "Search commands" });
const options = (page: Page) => palette(page).getByRole("option");
const row = (page: Page, name: string) => page.locator("[role='row'][data-profile-id]", { hasText: name });

test("Mod+K opens the palette on its search, and Esc gives focus back", async ({ page }) => {
  await openApp(page);
  await row(page, "Sandbox").locator("[data-cell='status']").click();
  await page.keyboard.press("ControlOrMeta+k");
  await expect(search(page)).toBeFocused();
  await page.keyboard.press("Escape");
  await expect(palette(page)).toHaveCount(0);
  await expect(row(page, "Sandbox")).toBeFocused();
});

test("Mod+K works from the filter field, and closes an open palette", async ({ page }) => {
  await openApp(page);
  await (await openSearch(page)).click();
  await page.keyboard.press("ControlOrMeta+k");
  await expect(search(page)).toBeFocused();
  await page.keyboard.press("ControlOrMeta+k");
  await expect(palette(page)).toHaveCount(0);
});

test("Mod+K does nothing while another dialog is open", async ({ page }) => {
  await openApp(page);
  await page.locator("aside").getByRole("button", { name: "New folder" }).click();
  const name = page.getByRole("dialog").getByRole("textbox", { name: "Name" });
  await expect(name).toBeFocused();
  await page.keyboard.press("ControlOrMeta+k");
  // A render later, the Folder dialog still has the keyboard.
  await page.keyboard.type("x");
  await expect(name).toHaveValue("x");
  await expect(palette(page)).toHaveCount(0);
});

test("the sidebar's Search opens the palette", async ({ page }) => {
  await openApp(page);
  await page.locator("aside nav").getByRole("button", { name: "Search" }).click();
  await expect(search(page)).toBeFocused();
});

test("arrows move the active option and wrap; Enter runs it", async ({ page }) => {
  await openApp(page);
  await page.keyboard.press("ControlOrMeta+k");
  const first = options(page).first();
  const last = options(page).last();
  await expect(first).toHaveAttribute("aria-selected", "true");
  await page.keyboard.press("ArrowUp");
  await expect(last).toHaveAttribute("aria-selected", "true");
  await expect(search(page)).toHaveAttribute("aria-activedescendant", (await last.getAttribute("id"))!);
  await page.keyboard.press("ArrowDown");
  await expect(first).toHaveAttribute("aria-selected", "true");
  await search(page).fill("fingerprints");
  await expect(options(page).first()).toHaveAttribute("aria-selected", "true");
  await page.keyboard.press("Enter");
  await expect(palette(page)).toHaveCount(0);
  await expect(page.getByRole("heading", { level: 1 })).toHaveText("Fingerprints");
});

test("a click runs an option; no match says so", async ({ page }) => {
  await openApp(page);
  await page.keyboard.press("ControlOrMeta+k");
  await search(page).fill("zzzz");
  await expect(options(page)).toHaveCount(0);
  await expect(palette(page)).toContainText("No results");
  await search(page).fill("trash");
  await palette(page).getByRole("option", { name: /^Trash/ }).click();
  await expect(page.getByRole("heading", { level: 1 })).toHaveText("Trash");
});

test("an Enter that confirms an input method's text runs nothing", async ({ page }) => {
  await openApp(page);
  await page.keyboard.press("ControlOrMeta+k");
  await search(page).fill("trash");
  await search(page).dispatchEvent("keydown", { key: "Enter", keyCode: 229 });
  // A render later, the palette still has the keyboard and nothing ran.
  await page.keyboard.type("x");
  await expect(search(page)).toHaveValue("trashx");
  await expect(page.getByRole("heading", { level: 1 })).toHaveText("All profiles");
});

test("in Chinese, pages are found by Chinese and by English words", async ({ page }) => {
  await page.addInitScript(() => localStorage.setItem("shardx.lang", "zh"));
  await openApp(page);
  await page.keyboard.press("ControlOrMeta+k");
  const zhSearch = page.getByRole("dialog", { name: "命令面板" }).getByRole("combobox");
  await zhSearch.fill("代理");
  await expect(page.getByRole("option", { name: /^代理/ })).toBeVisible();
  await zhSearch.fill("proxies");
  await expect(page.getByRole("option", { name: /^代理/ })).toBeVisible();
});

async function run(page: Page, query: string, option: RegExp) {
  await page.keyboard.press("ControlOrMeta+k");
  await search(page).fill(query);
  await palette(page).getByRole("option", { name: option }).click();
}

// The pages' old names still find them after F5 renamed them to their titles.
test("the old name Projects still finds Automation", async ({ page }) => {
  await openApp(page);
  await run(page, "projects", /^Automation/);
  await expect(page.getByRole("heading", { level: 1 })).toHaveText("Automation");
});

test("the old names Account and ProxyShard still find ShardX", async ({ page }) => {
  await openApp(page);
  await run(page, "account", /^ShardX/);
  await expect(page.getByRole("heading", { level: 1 })).toHaveText("ShardX");
  await run(page, "proxyshard", /^ShardX/);
  await expect(page.getByRole("heading", { level: 1 })).toHaveText("ShardX");
});

test("Enter starts an idle profile; a running one is shown in the list", async ({ page }) => {
  await openApp(page);
  await page.keyboard.press("ControlOrMeta+k");
  await search(page).fill("sandbox");
  await expect(options(page).first()).toContainText("Start");
  await page.keyboard.press("Enter");
  await expect(row(page, "Sandbox").locator("[data-cell='status'] [data-status-label]")).toHaveText("Running");
  await page.locator("aside nav").getByText("Shops", { exact: true }).click();
  await page.keyboard.press("ControlOrMeta+k");
  await search(page).fill("sandbox");
  await expect(options(page).first()).toContainText("Show in list");
  await page.keyboard.press("Enter");
  await expect(page.getByRole("heading", { level: 1 })).toHaveText("All profiles");
  await expect(row(page, "Sandbox")).toBeFocused();
});

test("Mod+Enter opens an idle profile in Peek without starting it", async ({ page }) => {
  await openApp(page, { section: "proxies" });
  await page.keyboard.press("ControlOrMeta+k");
  await search(page).fill("etsy");
  await page.keyboard.press("ControlOrMeta+Enter");
  await expect(row(page, "Etsy craft shop")).toBeFocused();
  await expect(page.locator("aside[data-peek]")).toBeVisible();
  // Not started: still its last-run text, never "Running".
  await expect(row(page, "Etsy craft shop").locator("[data-cell='status']")).not.toHaveText("Running");
});

test("Folders and Views open from the palette", async ({ page }) => {
  await openApp(page);
  await run(page, "shops", /^Shops/);
  await expect(page.getByRole("heading", { level: 1 })).toHaveText("Shops");
  await run(page, "recent", /^Recently used/);
  await expect(page.getByRole("heading", { level: 1 })).toHaveText("Recently used");
});

test("New profile from another page opens the form, name ready to type; Create profile opens it in Peek", async ({ page }) => {
  await openApp(page, { section: "proxies" });
  await run(page, "new profile", /^New profile/);
  await expect(page.getByRole("dialog", { name: "New profile", exact: true }).getByRole("textbox", { name: "Name" })).toBeFocused();
  await expect(page.getByRole("heading", { level: 1 })).toHaveText("All profiles");
  await pressCreate(page);
  await expect(page.locator("aside[data-peek]")).toBeVisible();
  await expect(page.locator("[role='row'][data-profile-id]")).toHaveCount(9);
});

test("Import, New folder and the theme run from the palette", async ({ page }) => {
  await openApp(page);
  await page.evaluate(() => {
    (window as unknown as { __shardxMockClipboard: string }).__shardxMockClipboard = JSON.stringify([{ name: "Imported one" }]);
  });
  await run(page, "import", /^Import profiles/);
  await expect(row(page, "Imported one")).toHaveCount(1);
  await run(page, "light", /^Use light theme/);
  await expect(page.locator("html")).toHaveAttribute("data-theme", "light");
  await run(page, "folder", /^New folder/);
  await expect(page.getByRole("dialog", { name: /folder/i })).toBeVisible();
});

// Owner feedback 5r: the palette's "New folder" row uses the same
// folder-add glyph as the sidebar and never the bare + (action:new-profile
// keeps that one, for New profile).
test("the palette's New folder row shows the folder-add glyph", async ({ page }) => {
  await openApp(page);
  await page.keyboard.press("ControlOrMeta+k");
  await search(page).fill("new folder");
  const option = palette(page).getByRole("option", { name: /^New folder/ });
  await expect(option.locator("svg")).toHaveAttribute("data-icon", "folder-add");
});

test("Settings sections open on their tab, in English and in Chinese", async ({ page }) => {
  await openApp(page);
  await run(page, "api", /^API & MCP/);
  await expect(page.getByRole("tab", { name: "API & MCP" })).toHaveAttribute("aria-selected", "true");
  await page.evaluate(() => localStorage.setItem("shardx.lang", "zh"));
  await page.reload();
  await expect(page.locator("aside [data-nav='search']")).toBeVisible();
  await page.keyboard.press("ControlOrMeta+k");
  const zh = page.getByRole("dialog", { name: "命令面板" }).getByRole("combobox");
  await zh.fill("storage");
  await page.getByRole("option", { name: /^存储/ }).click();
  await expect(page.getByRole("tab", { name: "存储" })).toHaveAttribute("aria-selected", "true");
});

test("a thousand profiles: at most 12 show, and the active one stays in view", async ({ page }) => {
  await page.addInitScript(() => { (window as unknown as { __shardxMockProfiles: number }).__shardxMockProfiles = 1000; });
  await openApp(page);
  await page.keyboard.press("ControlOrMeta+k");
  await search(page).fill("profile 0");
  const group = palette(page).getByRole("group", { name: "Profiles" });
  await expect(group.getByRole("option")).toHaveCount(12);
  for (let i = 0; i < 11; i++) await page.keyboard.press("ArrowDown");
  await expect(group.getByRole("option").last()).toBeInViewport();
});

test("odd queries: brackets, stars and extra spaces", async ({ page }) => {
  await openApp(page);
  await page.keyboard.press("ControlOrMeta+k");
  await search(page).fill("(*[");
  await expect(palette(page)).toContainText("No results");
  await search(page).fill("  fb   ads  ");
  await expect(palette(page).getByRole("option", { name: /^FB Ads acc-01/ })).toBeVisible();
});

type Mock = { state: { running: Record<string, { pid: number; started: number }>; order: string[] }; save: () => void };

test("the highlight stays on its command when the list changes under it", async ({ page }) => {
  await openApp(page);
  const ebay = await row(page, "eBay DE buyer").getAttribute("data-profile-id");
  await page.keyboard.press("ControlOrMeta+k");
  const newProfile = palette(page).getByRole("option", { name: /^New profile/ });
  for (let i = 0; i < 10 && (await newProfile.getAttribute("aria-selected")) !== "true"; i++) await page.keyboard.press("ArrowDown");
  await expect(newProfile).toHaveAttribute("aria-selected", "true");
  // eBay's browser closes on its own: it leaves the running profiles above.
  await page.evaluate((id) => { const m = (window as unknown as { __shardxMock: Mock }).__shardxMock; delete m.state.running[id!]; m.save(); }, ebay);
  await expect(palette(page).getByRole("option", { name: /^eBay DE buyer/ })).toHaveCount(0);
  await expect(newProfile).toHaveAttribute("aria-selected", "true");
  await page.keyboard.press("Enter");
  await pressCreate(page);
  await expect(page.locator("[role='row'][data-profile-id]")).toHaveCount(9);
});

test("with nothing typed: pinned and running profiles, pinned first, at most 12", async ({ page }) => {
  await page.addInitScript(() => { (window as unknown as { __shardxMockProfiles: number }).__shardxMockProfiles = 40; });
  await openApp(page);
  await page.evaluate(() => {
    const m = (window as unknown as { __shardxMock: Mock }).__shardxMock;
    m.state.order.filter((id) => id.startsWith("bulk-")).slice(0, 30).forEach((id, i) => { m.state.running[id] = { pid: 1000 + i, started: Date.now() }; });
    m.save();
  });
  // The poll has seen all 32 (Task 7a: the Running View's count).
  await expect(page.locator("aside [data-view='builtin:running'] [data-count]")).toHaveText("32");
  await page.keyboard.press("ControlOrMeta+k");
  const profiles = palette(page).getByRole("group", { name: "Profiles" }).getByRole("option");
  await expect(profiles).toHaveCount(12);
  const names = await profiles.allInnerTexts();
  expect(names[0]).toMatch(/^Amazon US seller/);
  expect(names[1]).toMatch(/^FB Ads acc-01/);
});

test("with nothing typed, idle profiles that aren't pinned stay out", async ({ page }) => {
  await openApp(page);
  await page.keyboard.press("ControlOrMeta+k");
  const profiles = palette(page).getByRole("group", { name: "Profiles" }).getByRole("option");
  await expect(profiles).toHaveCount(3);
  await expect(palette(page).getByRole("option", { name: /^Sandbox/ })).toHaveCount(0);
});

test("a label match beats a Folder match, in the group order and inside a group", async ({ page }) => {
  await openApp(page);
  await page.keyboard.press("ControlOrMeta+k");
  // "shop", not "sh": "Show keyboard shortcuts" starts with "sh" too, and an
  // action label that starts with the query rightly ties with the Folder.
  await search(page).fill("shop");
  await expect(palette(page).getByRole("group").first()).toHaveAttribute("aria-label", "Folders");
  const profiles = await palette(page).getByRole("group", { name: "Profiles" }).getByRole("option").allInnerTexts();
  expect(profiles[0]).toMatch(/^Etsy craft shop/);
  expect(profiles.findIndex((n) => n.startsWith("Etsy"))).toBeLessThan(profiles.findIndex((n) => n.startsWith("Amazon")));
});

test("the pointer moves the highlight", async ({ page }) => {
  await openApp(page);
  await page.keyboard.press("ControlOrMeta+k");
  const trash = palette(page).getByRole("option", { name: /^Trash/ });
  await trash.hover();
  await expect(trash).toHaveAttribute("aria-selected", "true");
});

test.describe("with animations on", () => {
  test.use({ reducedMotion: "no-preference" });
  test("Mod+K right after Esc opens a fresh palette while the old one fades out", async ({ page }) => {
    await openApp(page);
    await page.keyboard.press("ControlOrMeta+k");
    await search(page).fill("zz");
    await page.keyboard.press("Escape");
    await page.keyboard.press("ControlOrMeta+k");
    await expect(search(page)).toBeFocused();
    await expect(search(page)).toHaveValue("");
  });

  // Pair 11 carry: a menu still fading out after Esc no longer has the
  // keyboard, so Mod+K is not swallowed by it.
  test("Mod+K right after Esc on a menu opens the palette while the menu fades out", async ({ page }) => {
    await openApp(page);
    await row(page, "Sandbox").click({ button: "right" });
    await expect(page.getByRole("menu")).toBeVisible();
    await page.keyboard.press("Escape");
    await page.keyboard.press("ControlOrMeta+k");
    await expect(search(page)).toBeFocused();
  });
});

test("results print the key that does the same: Go to Proxies G then P, New profile C, the first Folder ⌘1", async ({ page }) => {
  await openApp(page);
  await page.keyboard.press("ControlOrMeta+k");
  await search(page).fill("proxies");
  await expect(palette(page).getByRole("option", { name: /^Proxies/ }).locator("kbd")).toHaveText(["G", "P"]);
  await search(page).fill("new profile");
  await expect(palette(page).getByRole("option", { name: /^New profile/ }).locator("kbd")).toHaveText(["C"]);
  await expect(palette(page).getByRole("option", { name: /^New profile/ })).toHaveAttribute("aria-keyshortcuts", "C");
  await search(page).fill("ads");
  await expect(palette(page).getByRole("option", { name: /^Ads/ }).locator("kbd")).toHaveText(["⌘1"]);
});

test("a key shows only where it works: off Profiles, New profile prints no C; an app key still prints", async ({ page }) => {
  await openApp(page, { section: "proxies" });
  await page.keyboard.press("ControlOrMeta+k");
  await search(page).fill("new profile");
  const newProfile = palette(page).getByRole("option", { name: /^New profile/ });
  await expect(newProfile).toBeVisible();
  await expect(newProfile.locator("kbd")).toHaveCount(0);
  await expect(newProfile).not.toHaveAttribute("aria-keyshortcuts", /.*/);
  await search(page).fill("fingerprints");
  await expect(palette(page).getByRole("option", { name: /^Fingerprints/ }).locator("kbd")).toHaveText(["G", "F"]);
});

test("palette words are verbs, as in the menus and the sheet", async ({ page }) => {
  await openApp(page);
  await page.keyboard.press("ControlOrMeta+k");
  await search(page).fill("theme");
  await expect(palette(page).getByRole("option", { name: /^Use dark theme/ })).toBeVisible();
  await search(page).fill("shortcuts");
  await expect(palette(page).getByRole("option", { name: /^Show keyboard shortcuts/ })).toBeVisible();
});

test("two-word queries, initials and plurals find what the user means (EMIL-16)", async ({ page }) => {
  await openApp(page);
  await page.keyboard.press("ControlOrMeta+k");
  const firstProfile = () => palette(page).getByRole("group", { name: "Profiles" }).getByRole("option").first();
  await search(page).fill("fb 02");
  await expect(firstProfile()).toHaveAccessibleName(/^FB Ads acc-02/);
  await search(page).fill("amazon seller");
  await expect(firstProfile()).toHaveAccessibleName(/^Amazon US seller/);
  await search(page).fill("ebay buyer");
  await expect(firstProfile()).toHaveAccessibleName(/^eBay DE buyer/);
  await search(page).fill("fbads");
  await expect(firstProfile()).toHaveAccessibleName(/^FB Ads acc/);
  await search(page).fill("proxy");
  await expect(palette(page).getByRole("option", { name: /^Proxies/ })).toBeVisible();
  await search(page).fill("payout"); // Etsy craft shop's notes
  await expect(firstProfile()).toHaveAccessibleName(/^Etsy craft shop/);
});

test("a profile shows its Folder and proxy country as a subtitle", async ({ page }) => {
  await openApp(page);
  await page.keyboard.press("ControlOrMeta+k");
  await search(page).fill("fb ads");
  await expect(palette(page).getByRole("option", { name: /^FB Ads acc-02/ }).locator("[data-subtitle]")).toHaveText("Ads, JP");
});

test("the count is announced once typing pauses; no results says so", async ({ page }) => {
  await openApp(page);
  await page.keyboard.press("ControlOrMeta+k");
  // The app's polite region (C24); the list header has a live region of its own.
  const polite = page.locator("[data-live='polite']");
  await search(page).fill("fb ads");
  await expect(polite).toContainText(/2 results/);
  await search(page).fill("zzzz");
  await expect(polite).toContainText(/No results/);
});

test("a click on a heading keeps the keyboard in the search (PC-104)", async ({ page }) => {
  await openApp(page);
  await page.keyboard.press("ControlOrMeta+k");
  await palette(page).getByText("Go to", { exact: true }).click();
  await expect(search(page)).toBeFocused();
  await page.keyboard.press("ArrowDown");
  await expect(search(page)).toHaveAttribute("aria-activedescendant", /.+/);
});

test("the footer shows Mod+Enter only when the highlighted result has it (PC-107)", async ({ page }) => {
  await openApp(page);
  await page.keyboard.press("ControlOrMeta+k");
  const footer = palette(page).locator("[data-palette-footer]");
  await search(page).fill("trash");
  await expect(footer).not.toContainText("⌘Enter");
  await search(page).fill("sandbox");
  await expect(footer).toContainText("⌘Enter");
  // The sheet's words for the same key (one name everywhere).
  await expect(footer).toContainText("Open in Peek");
});

const pick = (page: Page, name: string) => row(page, name).locator("[data-cell='checkbox'] input").check({ force: true });

test("with two rows selected, the first group acts on them (O7, IFACE-10)", async ({ page }) => {
  await openApp(page);
  await pick(page, "eBay DE buyer"); // running
  await pick(page, "Sandbox"); // idle
  await page.keyboard.press("ControlOrMeta+k");
  const first = palette(page).getByRole("group").first();
  await expect(first).toHaveAttribute("aria-label", "2 selected");
  for (const name of ["Start", "Stop", "Move to folder", "Edit tags", "Assign proxies", "Move to Trash"]) {
    // "Start" is a prefix of "Start in sync" (G2), which the group also lists.
    await expect(first.getByRole("option", { name: new RegExp(`^${name.replace(/[.*+?^${}()|[\]\\]/g, "\\$&")}(?! in sync)`) })).toBeVisible();
  }
  await expect(first.getByRole("option", { name: /^Move to Trash/ }).locator("kbd")).toHaveText(["⌫"]);
  await first.getByRole("option", { name: "Start", exact: true }).click();
  await expect(row(page, "Sandbox").locator("[data-cell='status'] [data-status-label]")).toHaveText("Running");
  // The run keeps the selection exactly as it was (owner rule).
  await expect(page.locator("[role='row'][data-profile-id][aria-selected='true']")).toHaveCount(2);
});

test("with no selection, the first group is the active row's", async ({ page }) => {
  await openApp(page);
  await row(page, "Etsy craft shop").focus();
  await page.keyboard.press("ControlOrMeta+k");
  const first = palette(page).getByRole("group").first();
  await expect(first).toHaveAttribute("aria-label", "Etsy craft shop");
  await expect(first.getByRole("option", { name: /^Change proxy/ })).toBeVisible();
});

// Pair 44 ruling: the first group follows the selection, else the row the
// keyboard chose, else Peek's; never a row the pointer only rests on.
test("a row the pointer rests on never sets the first group; Peek's row does", async ({ page }) => {
  await openApp(page);
  await row(page, "Etsy craft shop").hover();
  await page.keyboard.press("ControlOrMeta+k");
  await expect(palette(page).getByRole("group", { name: "Etsy craft shop" })).toHaveCount(0);
  await page.keyboard.press("Escape");
  await row(page, "Sandbox").focus();
  await page.keyboard.press("Enter");
  await expect(page.locator("aside[data-peek]")).toHaveAttribute("aria-label", /Sandbox/);
  await row(page, "FB Ads acc-02").hover();
  await page.keyboard.press("ControlOrMeta+k");
  await expect(palette(page).getByRole("group").first()).toHaveAttribute("aria-label", "Sandbox");
});

test("Mod+Enter on a profile opens it in Peek (O7)", async ({ page }) => {
  await openApp(page, { section: "proxies" });
  await page.keyboard.press("ControlOrMeta+k");
  await search(page).fill("etsy");
  await page.keyboard.press("ControlOrMeta+Enter");
  await expect(page.locator("aside[data-peek]")).toHaveAttribute("aria-label", /Etsy craft shop/);
  await expect(row(page, "Etsy craft shop").locator("[data-cell='status']")).not.toHaveText("Running");
});

test("page and app commands: Stop all running, Add proxies and Test on Proxies, Empty Trash on Trash", async ({ page }) => {
  await openApp(page);
  await page.keyboard.press("ControlOrMeta+k");
  await search(page).fill("stop all");
  await palette(page).getByRole("option", { name: /^Stop all running profiles/ }).click();
  await expect(page.locator("aside [data-nav='profiles'] [data-running]")).toHaveCount(0);
  await openApp(page, { section: "proxies" });
  await page.locator("[role='row'][data-proxy-id='px-us-2']").focus();
  await page.keyboard.press("ControlOrMeta+k");
  await expect(palette(page).getByRole("group").first().getByRole("option", { name: /^Test/ })).toBeVisible();
  // The header's words since Phase F Task 3; "new proxy" still finds it.
  await search(page).fill("new proxy");
  await expect(palette(page).getByRole("option", { name: /^Add proxies/ })).toBeVisible();
  await openApp(page, { section: "trash" });
  await page.keyboard.press("ControlOrMeta+k");
  await search(page).fill("empty");
  await expect(palette(page).getByRole("option", { name: /^Empty Trash/ })).toBeVisible();
});


// E11: a result's actions in place of the results (ruling 11, IFACE-10, A11Y-13).
test("Tab on a profile lists its actions; Enter runs one; Esc goes back with the query", async ({ page }) => {
  await openApp(page);
  await page.keyboard.press("ControlOrMeta+k");
  await search(page).fill("sandbox");
  await page.keyboard.press("Tab");
  await expect(search(page)).toHaveAttribute("placeholder", "Actions for Sandbox");
  await expect(search(page)).toHaveValue("");
  // Said once in the palette's live region, going in and coming back.
  await expect(palette(page).getByRole("status")).toHaveText("Actions for Sandbox");
  const names = await options(page).allInnerTexts();
  expect(names[0]).toMatch(/^Start/);
  expect(names[1]).toMatch(/^Open in Peek/);
  expect(names.some((n) => /^Copy ID/.test(n))).toBe(true);
  await page.keyboard.press("Escape");
  await expect(search(page)).toHaveValue("sandbox");
  await expect(palette(page)).toBeVisible();
  await expect(palette(page).getByRole("status")).toHaveText("Results");
  await page.keyboard.press("ArrowRight"); // the caret is at the end: the same list
  await expect(search(page)).toHaveAttribute("placeholder", "Actions for Sandbox");
  await search(page).fill("rename");
  await page.keyboard.press("Enter");
  await expect(palette(page)).toHaveCount(0);
  // The keyboard lands in the row's name editor and stays there. (The name
  // is the editor's value now, not the row's text, so the row is found by it.)
  const editor = page.locator("[role='row'][data-profile-id]").getByRole("textbox", { name: "Name" });
  await expect(editor).toHaveValue("Sandbox");
  await expect(editor).toBeFocused();
});

test("ArrowRight in the middle of the text moves the caret, not into the actions", async ({ page }) => {
  await openApp(page);
  await page.keyboard.press("ControlOrMeta+k");
  await search(page).fill("sandbox");
  await page.keyboard.press("ArrowLeft");
  await page.keyboard.press("ArrowRight");
  await expect(search(page)).toHaveAttribute("placeholder", /Search/);
});

test("a Folder's actions edit or delete it from the keyboard (A11Y-13)", async ({ page }) => {
  await openApp(page);
  await page.keyboard.press("ControlOrMeta+k");
  await search(page).fill("shops");
  await palette(page).getByRole("option", { name: /^Shops/ }).hover();
  await page.keyboard.press("Tab");
  await expect(options(page)).toHaveText([/^Open/, /^Edit folder/, /^Delete folder/]);
  await page.keyboard.press("ArrowDown");
  await page.keyboard.press("Enter");
  await expect(page.getByRole("dialog").getByRole("textbox", { name: "Name" })).toHaveValue("Shops");
});

test("the sidebar's Folder menu is the registry's too", async ({ page }) => {
  await openApp(page);
  await page.locator("aside [data-folder='Shops']").click({ button: "right" });
  await expect(page.getByRole("menu", { name: "Shops" }).getByRole("menuitem")).toHaveText([/^Edit folder/, /^Delete folder/]);
});

// E12: recents that fade (EMIL-16, O7) and one Change theme (CRAFT-07).
test("the palette learns: what was run comes first as Recent, and fades with time", async ({ page }) => {
  await openApp(page);
  await run(page, "sandbox", /^Sandbox/); // starts it
  await run(page, "trash", /^Trash/);
  await page.keyboard.press("ControlOrMeta+k");
  const recent = palette(page).getByRole("group", { name: "Recent" });
  await expect(recent.getByRole("option")).toHaveText([/^Trash/, /^Profiles\s+\/\s+Sandbox/]);
  // A use a month old weighs less than one today.
  await page.evaluate(() => {
    const day = 86_400_000;
    localStorage.setItem("shardx.palette.recent", JSON.stringify({
      "cmd:nav.proxies": { n: 5, last: Date.now() - 30 * day },
      "cmd:nav.settings": { n: 1, last: Date.now() },
    }));
  });
  await page.keyboard.press("Escape");
  await page.keyboard.press("ControlOrMeta+k");
  await expect(palette(page).getByRole("group", { name: "Recent" }).getByRole("option").first()).toHaveAccessibleName(/^Settings/);
});

test("a broken recent list is no list", async ({ page }) => {
  await page.addInitScript(() => localStorage.setItem("shardx.palette.recent", "{not json"));
  await openApp(page);
  await page.keyboard.press("ControlOrMeta+k");
  await expect(palette(page).getByRole("group", { name: "Recent" })).toHaveCount(0);
  await expect(options(page).first()).toBeVisible();
});

test("one Change theme row opens the three themes, the current one marked", async ({ page }) => {
  await openApp(page);
  await page.keyboard.press("ControlOrMeta+k");
  await expect(palette(page).getByRole("option", { name: /^Use light theme/ })).toHaveCount(0);
  await palette(page).getByRole("option", { name: /^Change theme/ }).click();
  await expect(options(page)).toHaveText([/^Use light theme/, /^Use dark theme.*Current/, /^Use system theme/]);
  await palette(page).getByRole("option", { name: /^Use light theme/ }).click();
  await expect(page.locator("html")).toHaveAttribute("data-theme", "light");
  await page.keyboard.press("ControlOrMeta+k");
  // Recent remembers the row that asked, never a theme row (Pair 45 review, I1).
  await expect(palette(page).getByRole("group", { name: "Recent" }).getByRole("option")).toHaveText([/^Change theme/]);
  // A search still finds a theme by name.
  await search(page).fill("dark");
  await expect(palette(page).getByRole("option", { name: /^Use dark theme/ })).toBeVisible();
});

// Air and hierarchy, section 5: a Recent row says where it lives. The area is
// soft and the name strong, a history icon first and a chevron at the row end;
// a narrow row gives the area up first, so the name stays readable.
test("Recent rows show their area, soft, before a strong name", async ({ page }) => {
  await openApp(page);
  await run(page, "sandbox", /^Sandbox/);
  await page.keyboard.press("ControlOrMeta+k");
  const first = palette(page).getByRole("group", { name: "Recent" }).getByRole("option").first();
  await expect(first.locator("[data-recent-area]")).toHaveText(/^Profiles\s*\/\s*$/);
  await expect(first.locator("[data-recent-name]")).toHaveText("Sandbox");
  const colors = await first.evaluate((el) => ({
    area: getComputedStyle(el.querySelector("[data-recent-area]")!).color,
    name: getComputedStyle(el.querySelector("[data-recent-name]")!).color,
  }));
  expect(colors.area).not.toBe(colors.name);
  // History icon first, chevron last, both in a 14 px slot.
  const icons = first.locator("svg");
  await expect(icons).toHaveCount(2);
  for (const i of [0, 1]) expect((await icons.nth(i).boundingBox())!.width).toBe(14);
  // Truncation takes the area side first.
  await first.evaluate((el) => { (el.querySelector("[data-recent-path]") as HTMLElement).style.maxWidth = "60px"; });
  expect(await first.locator("[data-recent-name]").evaluate((el) => el.scrollWidth <= el.clientWidth)).toBe(true);
});

test("Recent rows keep their keyboard: arrows move, Enter runs", async ({ page }) => {
  await openApp(page);
  await run(page, "trash", /^Trash/);
  await run(page, "sandbox", /^Sandbox/);
  await page.keyboard.press("ControlOrMeta+k");
  await page.keyboard.press("ArrowDown");
  await expect(palette(page).getByRole("group", { name: "Recent" }).getByRole("option").nth(1)).toHaveAttribute("aria-selected", "true");
  await page.keyboard.press("Enter");
  await expect(page.getByRole("heading", { level: 1 })).toHaveText("Trash");
});

// Air and hierarchy follow-up: a page reads as its name; proxies and projects
// are found by a search (at most five each) and recorded as "{Area} / {name}".
test("a recent page reads as its name, with no area", async ({ page }) => {
  await openApp(page);
  await run(page, "trash", /^Trash/);
  await page.keyboard.press("ControlOrMeta+k");
  const first = palette(page).getByRole("group", { name: "Recent" }).getByRole("option").first();
  await expect(first).toHaveText(/^Trash$/);
  await expect(first.locator("[data-recent-area]")).toHaveCount(0);
  await expect(first.locator("svg")).toHaveCount(2);
});

test("a proxy is found by a search, opens its Peek, and Recent says Proxies / name", async ({ page }) => {
  await openApp(page);
  await page.keyboard.press("ControlOrMeta+k");
  await search(page).fill("frankfurt");
  await palette(page).getByRole("option", { name: /ISP Frankfurt/ }).click();
  await expect(page.getByRole("heading", { level: 1 })).toHaveText("Proxies");
  await expect(page.locator("aside[data-peek]")).toContainText("ISP Frankfurt");
  await page.keyboard.press("ControlOrMeta+k");
  await expect(palette(page).getByRole("group", { name: "Recent" }).getByRole("option").first()).toHaveText(/^Proxies\s+\/\s+ISP Frankfurt/);
});

test("a project is found by a search and opens in the editor; each new group holds five at most", async ({ page }) => {
  await openApp(page);
  await page.keyboard.press("ControlOrMeta+k");
  await search(page).fill("pricing scrape");
  await palette(page).getByRole("option", { name: /Pricing scrape 6h/ }).click();
  await expect(page.getByRole("heading", { level: 1 })).not.toHaveText("Proxies");
  await expect(page.locator("main")).toContainText("Pricing scrape 6h");
  await page.keyboard.press("ControlOrMeta+k");
  await expect(palette(page).getByRole("group", { name: "Recent" }).getByRole("option").first()).toHaveText(/^Automation\s+\/\s+Pricing scrape 6h/);
  await search(page).fill("1");
  expect(await palette(page).getByRole("group", { name: "Proxies" }).getByRole("option").count()).toBeLessThanOrEqual(5);
});
