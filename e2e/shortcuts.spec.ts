import { expect, test, type Page } from "@playwright/test";
import { appModule, openApp } from "./support/app";
import { openSearch } from "./support/search";

const sheet = (page: Page) => page.getByRole("dialog", { name: "Keyboard shortcuts" });
const row = (page: Page, name: string) => page.locator("[role='row'][data-profile-id]", { hasText: name });
const heading = (page: Page) => page.getByRole("heading", { level: 1 });

test("? opens the keyboard shortcuts, and Esc gives focus back", async ({ page }) => {
  await openApp(page);
  await row(page, "Sandbox").locator("[data-cell='status']").click();
  await page.keyboard.type("?");
  await expect(sheet(page)).toBeVisible();
  await expect(sheet(page)).toContainText("Command palette");
  await expect(sheet(page)).toContainText("Move to Trash");
  await expect(sheet(page)).toContainText("⌘K");
  await page.keyboard.press("Escape");
  await expect(sheet(page)).toHaveCount(0);
  await expect(row(page, "Sandbox")).toBeFocused();
});

test("? typed into the filter is a question mark", async ({ page }) => {
  await openApp(page);
  const filter = await openSearch(page);
  await page.keyboard.type("?");
  await expect(filter).toHaveValue("?");
  await expect(sheet(page)).toHaveCount(0);
});

test("? does nothing while the palette is open", async ({ page }) => {
  await openApp(page);
  await page.keyboard.press("ControlOrMeta+k");
  await expect(page.getByRole("combobox", { name: "Search commands" })).toBeFocused();
  await page.keyboard.type("?");
  // The palette's search took it as text: the sheet stayed shut.
  await expect(page.getByRole("combobox", { name: "Search commands" })).toHaveValue("?");
  await expect(sheet(page)).toHaveCount(0);
});

test("the palette opens the keyboard shortcuts", async ({ page }) => {
  await openApp(page);
  await page.keyboard.press("ControlOrMeta+k");
  await page.getByRole("combobox", { name: "Search commands" }).fill("shortcuts");
  await page.getByRole("option", { name: /^Show keyboard shortcuts/ }).click();
  await expect(sheet(page)).toBeVisible();
});

test("in Chinese the labels are translated and the key caps stay", async ({ page }) => {
  await page.addInitScript(() => localStorage.setItem("shardx.lang", "zh"));
  await openApp(page);
  await page.keyboard.type("?");
  const zh = page.getByRole("dialog", { name: "键盘快捷键" });
  await expect(zh).toContainText("命令面板");
  await expect(zh).toContainText("Enter");
});

test("G then P, F or S goes to Proxies, Fingerprints or Settings", async ({ page }) => {
  await openApp(page);
  await page.keyboard.press("g");
  await page.keyboard.press("p");
  await expect(heading(page)).toHaveText("Proxies");
  await page.keyboard.press("g");
  await page.keyboard.press("f");
  await expect(heading(page)).toHaveText("Fingerprints");
  await page.keyboard.press("g");
  await page.keyboard.press("s");
  await expect(heading(page)).toHaveText("Settings");
});

test("G's second key never reaches the list: no start, no proxy editor", async ({ page }) => {
  await openApp(page);
  await row(page, "Sandbox").locator("[data-cell='status']").click();
  await page.keyboard.press("g");
  await page.keyboard.press("p");
  await expect(heading(page)).toHaveText("Proxies");
  // Back on the list, no proxy editor waits on Sandbox's row.
  await page.locator("aside").getByText("All profiles", { exact: true }).click();
  await expect(page.getByRole("dialog", { name: "Proxy" })).toHaveCount(0);
  await row(page, "Sandbox").locator("[data-cell='status']").click();
  await page.keyboard.press("g");
  await page.keyboard.press("s");
  await expect(heading(page)).toHaveText("Settings");
  await page.locator("aside").getByText("All profiles", { exact: true }).click();
  await expect(row(page, "Sandbox").locator("[data-cell='status']")).toHaveText("Never run");
});

test("a G left alone expires; the next S does its own job", async ({ page }) => {
  await openApp(page);
  await row(page, "Sandbox").locator("[data-cell='status']").click();
  await page.keyboard.press("g");
  await page.waitForTimeout(1700);
  await page.keyboard.press("s");
  await expect(row(page, "Sandbox").locator("[data-cell='status'] [data-status-label]")).toHaveText("Running");
  await expect(heading(page)).toHaveText("All profiles");
});

test("G and another key: the G is dropped and the key does its job", async ({ page }) => {
  await openApp(page);
  await row(page, "Sandbox").locator("[data-cell='status']").click();
  await page.keyboard.press("g");
  await page.keyboard.press("x");
  await expect(row(page, "Sandbox")).toHaveAttribute("aria-selected", "true");
  await page.keyboard.press("s");
  await expect(heading(page)).toHaveText("All profiles");
});

test("g and p typed into a field are just text", async ({ page }) => {
  await openApp(page);
  const filter = await openSearch(page);
  await page.keyboard.type("gp");
  await expect(filter).toHaveValue("gp");
  await expect(heading(page)).toHaveText("All profiles");
});

test("a second G starts over: G G S goes to Settings and starts nothing", async ({ page }) => {
  await openApp(page);
  await row(page, "Sandbox").locator("[data-cell='status']").click();
  await page.keyboard.press("g");
  await page.keyboard.press("g");
  await page.keyboard.press("s");
  await expect(heading(page)).toHaveText("Settings");
  await page.locator("aside").getByText("All profiles", { exact: true }).click();
  await expect(row(page, "Sandbox").locator("[data-cell='status']")).toHaveText("Never run");
});

test("a held G, and G then ?, still work", async ({ page }) => {
  await openApp(page);
  await page.keyboard.down("g");
  await page.keyboard.down("g"); // auto-repeat
  await page.keyboard.up("g");
  await page.keyboard.press("s");
  await expect(heading(page)).toHaveText("Settings");
  await page.keyboard.press("g");
  await page.keyboard.type("?");
  await expect(sheet(page)).toBeVisible();
});

test("Shift+G then Shift+S, as the sheet prints them, goes to Settings", async ({ page }) => {
  await openApp(page);
  await page.keyboard.press("Shift+G");
  await page.keyboard.press("Shift+S");
  await expect(heading(page)).toHaveText("Settings");
});

test("with a menu open, ? and G S do nothing", async ({ page }) => {
  await openApp(page);
  await row(page, "Sandbox").click({ button: "right" });
  await expect(page.getByRole("menu")).toBeVisible();
  await page.keyboard.type("?");
  await page.keyboard.press("g");
  await page.keyboard.press("s");
  await expect(sheet(page)).toHaveCount(0);
  await expect(heading(page)).toHaveText("All profiles");
});

test("the sheet shows alternatives with a slash, and says when Undo works", async ({ page }) => {
  await openApp(page);
  await page.keyboard.type("?");
  // "Open in Peek" names a row in each list and the palette's Mod+Enter: this is the Profiles one.
  await expect(sheet(page).locator("[data-sheet-row][data-commands='profile.openPeek']")).toContainText(/Enter\s*\/\s*Space/);
  await expect(sheet(page).locator("[data-sheet-row][data-commands='app.undo']")).toContainText("Undo");
  await expect(sheet(page)).toContainText("Move to folder");
});

test("keys typed into the Automation live browser view go to that browser", async ({ page }) => {
  await openApp(page, { section: "automation" });
  await page.getByRole("button", { name: "New project", exact: true }).click();
  await expect(page.getByRole("textbox", { name: "Project name" })).toBeFocused();
  await page.keyboard.type("Probe");
  // Creating it opens it in the editor, its name in the title field.
  await expect(page.getByRole("textbox", { name: "Project name" })).toHaveValue("Probe");
  await page.getByRole("button", { name: "Open browser" }).click();
  await page.getByRole("combobox", { name: "Choose a profile…" }).click();
  await page.getByRole("option", { name: "Amazon US seller" }).click();
  await page.getByRole("button", { name: "Start", exact: true }).click();
  // The mock has it open without automation, so the launcher offers a restart.
  await page.getByRole("dialog", { name: "“Amazon US seller” is open without automation" })
    .getByRole("button", { name: "Restart" }).click();
  const canvas = page.locator("canvas[tabindex='0']");
  await canvas.focus();
  await page.evaluate(() => {
    const w = window as unknown as { __sent: string[] };
    w.__sent = [];
    document.querySelector("canvas[tabindex='0']")!.addEventListener("keydown", (e) => w.__sent.push((e as KeyboardEvent).key));
  });
  await page.keyboard.type("gs?");
  await expect.poll(() => page.evaluate(() => (window as unknown as { __sent: string[] }).__sent)).toEqual(["g", "s", "?"]);
  await expect(page.locator("main")).toHaveAttribute("data-section", "automation");
  await expect(sheet(page)).toHaveCount(0);
});

type SheetRow = { ids: string[]; label: string; keys: { caps: string[] }[] };
type SheetSection = { area: string; title: string; rows: SheetRow[] };
const sections = (page: Page, section: string) =>
  page.evaluate((s) => (window as unknown as { __cmds: { sheetSections: (s: string) => SheetSection[] } }).__cmds.sheetSections(s), section);

test("every key the registry knows is on the sheet, next to its command's name", async ({ page }) => {
  await openApp(page);
  await appModule(page, "/src/features/commands/index.ts", "__cmds");
  const want = await sections(page, "profiles");
  await page.keyboard.type("?");
  await expect(sheet(page)).toBeVisible();
  for (const s of want) {
    await expect(sheet(page).getByRole("heading", { name: s.title })).toBeVisible();
    for (const r of s.rows) {
      const el = sheet(page).locator(`[data-sheet-row][data-commands='${r.ids.join(" ")}']`);
      await expect(el).toContainText(r.label);
      for (const k of r.keys) for (const cap of k.caps) await expect(el.locator("kbd", { hasText: cap }).first()).toBeVisible();
    }
  }
  // A pair prints as one row with both names (ruling 5).
  await expect(sheet(page).locator("[data-sheet-row][data-commands='profile.start profile.stop']")).toContainText("Start / Stop");
});

test("a page's own Save key is on that page's sheet only", async ({ page }) => {
  await openApp(page);
  await appModule(page, "/src/features/commands/index.ts", "__cmds");
  const ids = async (section: string) => (await sections(page, section)).flatMap((s) => s.rows.flatMap((r) => r.ids));
  expect(await ids("profiles")).not.toEqual(expect.arrayContaining(["settings.save"]));
  expect(await ids("profiles")).not.toEqual(expect.arrayContaining(["automation.save"]));
  expect(await ids("settings")).toEqual(expect.arrayContaining(["settings.save"]));
  expect(await ids("settings")).not.toEqual(expect.arrayContaining(["automation.save"]));
  expect(await ids("automation")).toEqual(expect.arrayContaining(["automation.save"]));
});

test("on Proxies the sheet starts with the Proxies list: T tests there (PC-112)", async ({ page }) => {
  await openApp(page, { section: "proxies" });
  await page.keyboard.type("?");
  const first = sheet(page).locator("section").first();
  await expect(first.getByRole("heading")).toHaveText("Proxies list");
  await expect(first.locator("[data-sheet-row][data-commands='proxy.test']")).toContainText("Test");
  await expect(first.locator("[data-sheet-row][data-commands='proxy.test'] kbd")).toHaveText("T");
});

type Launch = { id: string };
const launches = (page: Page) =>
  page.evaluate(() => ((window as unknown as { __shardxMockLaunches?: Launch[] }).__shardxMockLaunches ?? []).map((l) => l.id));
/** Sends one key event to the focused element, as a layout the test runner does not have would. */
const sendKey = (page: Page, init: KeyboardEventInit) =>
  page.evaluate((init) => (document.activeElement ?? document.body).dispatchEvent(new KeyboardEvent("keydown", { bubbles: true, cancelable: true, ...init })), init);

test("a held S starts the row once", async ({ page }) => {
  await openApp(page);
  await row(page, "Sandbox").focus();
  await page.keyboard.down("s");
  await page.keyboard.down("s"); // auto-repeat
  await page.keyboard.down("s");
  await page.keyboard.up("s");
  await expect(row(page, "Sandbox").locator("[data-cell='status'] [data-status-label]")).toHaveText("Running");
  expect((await launches(page)).filter((id) => id === "f62a9d43")).toHaveLength(1);
});

test("a Cyrillic layout's S key starts the row (the key's place, A11Y-02)", async ({ page }) => {
  await openApp(page);
  await row(page, "Sandbox").focus();
  await sendKey(page, { key: "ы", code: "KeyS" });
  await expect(row(page, "Sandbox").locator("[data-cell='status'] [data-status-label]")).toHaveText("Running");
});

test("Cmd+1 on AZERTY (the 1 key types &) opens the first Folder", async ({ page }) => {
  await openApp(page);
  await sendKey(page, { key: "&", code: "Digit1", metaKey: true, ctrlKey: false });
  await expect(heading(page)).toHaveText("Ads");
});

test("a held Cmd+K opens the palette once", async ({ page }) => {
  await openApp(page);
  await page.keyboard.down("ControlOrMeta");
  await page.keyboard.down("k");
  await page.keyboard.down("k"); // auto-repeat
  await page.keyboard.up("k");
  await page.keyboard.up("ControlOrMeta");
  await expect(page.getByRole("combobox", { name: "Search commands" })).toBeFocused();
});

test("/ puts the keyboard in the filter from a row, and is a slash in a field", async ({ page }) => {
  await openApp(page);
  await row(page, "Sandbox").focus();
  await page.keyboard.press("/");
  const filter = page.getByRole("searchbox", { name: "Search…" });
  await expect(filter).toBeFocused();
  await page.keyboard.press("/");
  await expect(filter).toHaveValue("/");
});

test("the Proxies list keys come from the same registry: T tests, X selects", async ({ page }) => {
  await openApp(page, { section: "proxies" });
  const r = page.locator("[role='row'][data-proxy-id='px-us-2']");
  await r.focus();
  await page.keyboard.press("x");
  await expect(r).toHaveAttribute("aria-selected", "true");
  await page.keyboard.press("t");
  await expect(r.locator("[data-cell='test']")).toContainText(/Testing|ms|Failed/);
});

// E22b (IFACE-11): C is each page's New, from the registry; the sheet lists it.
test("C is each page's New: Add proxies on Proxies, a bookmark on Bookmarks; nothing on Fingerprints", async ({ page }) => {
  await openApp(page, { section: "proxies" });
  await page.keyboard.press("c");
  await expect(page.getByRole("dialog", { name: /prox/i })).toBeVisible();
  await openApp(page, { section: "bookmarks" });
  await page.keyboard.press("c");
  await expect(page.getByRole("dialog", { name: "New bookmark" })).toBeVisible();
  await openApp(page, { section: "fingerprints" });
  await page.keyboard.press("c");
  await expect(page.getByRole("dialog")).toHaveCount(0);
  await page.keyboard.type("?");
  await expect(sheet(page).locator("[data-sheet-row][data-commands='proxy.new']")).toHaveCount(1);
  // The header buttons say so in their tooltips, and declare the key.
  await page.keyboard.press("Escape");
  await openApp(page, { section: "proxies" });
  await expect(page.locator("main").getByRole("button", { name: "Add proxies" })).toHaveAttribute("aria-keyshortcuts", "C");
  await openApp(page, { section: "bookmarks" });
  await expect(page.locator("main header").getByRole("button", { name: "New bookmark" })).toHaveAttribute("aria-keyshortcuts", "C");
});
