import { expect, test, type Page } from "@playwright/test";
import { appModule, openApp } from "./support/app";
import { parseColor } from "./support/color";
import { openSearch } from "./support/search";

// Clarity fixes, item 6: a bookmark's address is read decoded (Name, Address
// and the row menu), a long one never stretches the menu, an unfinished
// address is not saved, and Delete in the menu is quiet until it is aimed at.
type Bm = { id: string; title: string; url: string; folder: string };
type Mock = { __shardxMock: { state: { bookmarks: Bm[] }; calls: Record<string, number>; emit(event: string, payload: unknown): void } };

const CYRILLIC_URL = encodeURI("https://chromewebstore.google.com/detail/бесплатный-vpn/akcocjjpkmlniicdeemdceeajlmoabhg");
const READ = "chromewebstore.google.com/detail/бесплатный-vpn/akcocjjpkmlniicdeemdceeajlmoabhg";

/** The backend gives an untitled bookmark its own address as its title: so do these. */
async function add(page: Page, url: string, id: string, title = url) {
  await page.evaluate(([id, title, url]) => {
    const m = (window as unknown as Mock).__shardxMock;
    m.state.bookmarks.push({ id, title, url, folder: "" });
    m.emit("store-changed", "bookmarks");
  }, [id, title, url]);
  await expect(row(page, id)).toBeVisible();
}

const row = (page: Page, id: string) => page.locator(`[role='row'][data-row-id='${id}']`);
const rowMenu = async (page: Page, id: string) => {
  await row(page, id).click({ button: "right" });
  const menu = page.getByRole("menu");
  await expect(menu).toBeVisible();
  return menu;
};

test("Cyrillic address shows decoded in Name, Address and menu", async ({ page }) => {
  expect(CYRILLIC_URL).toContain("%D0%B1");
  await openApp(page, { section: "bookmarks" });
  await add(page, CYRILLIC_URL, "cyr");
  const r = row(page, "cyr");
  await expect(r.locator("[data-cell='name']")).toHaveText(READ);
  await expect(r.locator("[data-cell='address']")).toHaveText(READ);
  const menu = await rowMenu(page, "cyr");
  // Named by its heading, which is the decoded address; the heading's own text reads whole too.
  await expect(menu).toHaveAttribute("aria-label", READ);
  await expect(menu.locator("[role='presentation']").first()).toHaveText(READ);
  await page.keyboard.press("Escape");
  // The whole address, scheme and all, on hover where it is cut: read too.
  // (At 1280 the stretched Address column shows it whole: owner, 2026-10-23.)
  await page.setViewportSize({ width: 1024, height: 680 });
  await expect.poll(() => r.locator("[data-cell='address'] span.truncate").evaluate((el) => el.scrollWidth > el.clientWidth)).toBe(true);
  // The right-click left the pointer over the address; its tooltip opens on a fresh entry.
  await page.mouse.move(0, 0);
  await r.locator("[data-cell='address'] span.truncate").hover();
  await expect(page.getByRole("tooltip")).toHaveText(`https://${READ}`);
  // A search in the words as they read.
  await (await openSearch(page)).fill("бесплатный");
  await expect(row(page, "cyr")).toBeVisible();
  await expect(page.locator("[role='row'][data-row-id]")).toHaveCount(1);
});

test("broken encoding shows raw and nothing throws", async ({ page }) => {
  const errors: string[] = [];
  page.on("pageerror", (e) => errors.push(e.message));
  await openApp(page, { section: "bookmarks" });
  // A stray % and a half-written escape: not valid percent-encoding.
  await add(page, "https://example.com/100%/off%zz", "broken");
  await add(page, "https://example.com/%E0%A4%A", "half");
  await expect(row(page, "broken").locator("[data-cell='name']")).toHaveText("example.com/100%/off%zz");
  await expect(row(page, "broken").locator("[data-cell='address']")).toHaveText("example.com/100%/off%zz");
  await expect(row(page, "half").locator("[data-cell='address']")).toHaveText("example.com/%E0%A4%A");
  const menu = await rowMenu(page, "broken");
  await expect(menu).toHaveAttribute("aria-label", "example.com/100%/off%zz");
  await page.keyboard.press("Escape");
  // The rest of the list is still there.
  await expect(row(page, "bm-1")).toBeVisible();
  expect(errors).toEqual([]);
});

test("menu is at most 320 px wide", async ({ page }) => {
  await openApp(page, { section: "bookmarks" });
  const long = `https://www.example.com/${"a-very-long-path-segment/".repeat(12)}final-page-name`;
  await add(page, long, "long");
  const menu = await rowMenu(page, "long");
  const m = (await menu.boundingBox())!;
  expect(m.width).toBeLessThanOrEqual(320.5);
  const viewport = page.viewportSize()!;
  expect(m.x + m.width).toBeLessThanOrEqual(viewport.width);
  // Cut in the middle: the start gives way and the address's end stays in view.
  const head = menu.locator("[role='presentation']").first();
  await expect(head).toContainText("final-page-name");
  const tail = (await head.locator("span").last().boundingBox())!;
  expect(tail.x + tail.width).toBeLessThanOrEqual(m.x + m.width + 0.5);
  expect(tail.x).toBeGreaterThanOrEqual(m.x);
  // The heading reads whole to a screen reader.
  await expect(menu).toHaveAttribute("aria-label", long.replace(/^https:\/\//, ""));
  // A short one keeps its small menu.
  await page.keyboard.press("Escape");
  const short = (await (await rowMenu(page, "bm-1")).boundingBox())!;
  expect(short.width).toBeLessThan(320);
});

test("\"https://goog\" fails with \"Enter a full address, like https://example.com\"", async ({ page }) => {
  await openApp(page, { section: "bookmarks" });
  const count = () => page.evaluate(() => (window as unknown as Mock).__shardxMock.state.bookmarks.length);
  const before = await count();
  await page.getByRole("button", { name: "New bookmark" }).first().click();
  const dialog = page.getByRole("dialog", { name: "New bookmark" });
  await expect(dialog).toHaveAttribute("data-state", "open");
  const url = dialog.getByRole("textbox", { name: "URL" });
  const said = dialog.getByText("Enter a full address, like https://example.com", { exact: true });
  // Nothing is said before Save.
  await url.fill("https://goog");
  await expect(said).toHaveCount(0);
  await dialog.getByRole("button", { name: "Save", exact: true }).click();
  await expect(said).toBeVisible();
  await expect(url).toHaveAttribute("aria-invalid", "true");
  await expect(url).toBeFocused();
  await expect(dialog).toBeVisible();
  expect(await count()).toBe(before);
  expect(await page.evaluate(() => (window as unknown as Mock).__shardxMock.calls.bookmark_save ?? 0)).toBe(0);
  // Still unfinished: still said. Whole: gone, and Save saves.
  await url.fill("https://goog");
  await expect(said).toBeVisible();
  await url.fill("https://example.com");
  await expect(said).toHaveCount(0);
  await dialog.getByRole("button", { name: "Save", exact: true }).click();
  await expect(dialog).toHaveCount(0);
  expect(await count()).toBe(before + 1);
  // No scheme and a path is a full address too (the backend adds https://).
  await page.getByRole("button", { name: "New bookmark" }).first().click();
  await url.fill("facebook.com/ads/manager");
  await dialog.getByRole("button", { name: "Save", exact: true }).click();
  await expect(dialog).toHaveCount(0);
  expect(await count()).toBe(before + 2);
});

test("an empty address is said the same way", async ({ page }) => {
  await openApp(page, { section: "bookmarks" });
  await page.getByRole("button", { name: "New bookmark" }).first().click();
  const dialog = page.getByRole("dialog", { name: "New bookmark" });
  await expect(dialog).toHaveAttribute("data-state", "open");
  await dialog.getByRole("button", { name: "Save", exact: true }).click();
  await expect(dialog.getByText("Enter a full address, like https://example.com", { exact: true })).toBeVisible();
  await expect(dialog).toBeVisible();
});

for (const theme of ["dark", "light"] as const) {
  test(`Delete has no strong fill at rest (${theme})`, async ({ page }) => {
    await openApp(page, { section: "bookmarks", theme });
    const menu = await rowMenu(page, "bm-1");
    const del = menu.getByRole("menuitem", { name: "Delete" });
    const edit = menu.getByRole("menuitem", { name: "Edit" });
    const style = (loc: typeof del) => loc.evaluate((el) => { const cs = getComputedStyle(el); return { bg: cs.backgroundColor, color: cs.color }; });
    // At rest (opened with the pointer, aimed at nothing): red text, no fill.
    const rest = await style(del);
    expect(parseColor(rest.bg)[3]).toBe(0);
    const [r, g, b] = parseColor(rest.color);
    expect(r).toBeGreaterThan(g + 40);
    expect(r).toBeGreaterThan(b + 40);
    expect(rest.color).not.toBe((await style(edit)).color);
    // Aimed at (the pointer, the keys): the error tint at about 12%, a wash and not a block of colour.
    await del.hover();
    await expect.poll(async () => parseColor((await style(del)).bg)[3]).toBeGreaterThan(0.1);
    expect(parseColor((await style(del)).bg)[3]).toBeLessThanOrEqual(0.13);
    // Away again: no fill.
    await edit.hover();
    await expect.poll(async () => parseColor((await style(del)).bg)[3]).toBe(0);
  });
}

test("isFullUrl turns away only what was never finished", async ({ page }) => {
  await openApp(page, { section: "bookmarks" });
  await appModule(page, "/src/shared/lib/utils.ts", "__u");
  const check = (texts: string[]) => page.evaluate(
    (texts) => texts.map((t) => (window as unknown as { __u: { isFullUrl(t: string): boolean } }).__u.isFullUrl(t)),
    texts,
  );
  const accepted = [
    "https://example.com", "example.com", "facebook.com/ads/manager", "example.com:8080/x", "127.0.0.1:8080",
    "localhost", "localhost:3000", "http://localhost:3000", "https://пример.рф", "ftp://files.example.com",
    // The browser's own pages and local files: the backend keeps any scheme.
    "chrome://flags", "chrome://extensions", "file:///C:/x.html", "about://blank",
    // An intranet host, once it goes on past its name.
    "http://nas:5000", "http://nas:80", "http://router/", "http://router/admin", "https://nas.local", "example.com.",
    // IPv6, bracketed.
    "http://[::1]:3000", "[::1]:3000", "https://[2001:db8::1]/",
    // A bookmarklet, with its script.
    "javascript:alert(1)", "JavaScript: void(0)",
  ];
  const refused = [
    "", "   ", "goog", "https://goog", "http://nas", "goog:80", "goog/path",
    "chrome://", "file://", "https://", "javascript:", "javascript:   ", "http://goog:",
    // One number or a name with nothing after its dot is not a finished host.
    "12345", "https://12345", "goog.", "https://goog.",
  ];
  expect((await check(accepted)).map((ok, i) => [accepted[i], ok]).filter(([, ok]) => !ok), "refused").toEqual([]);
  expect((await check(refused)).map((ok, i) => [refused[i], ok]).filter(([, ok]) => ok), "accepted").toEqual([]);
});

test("an address the backend took before can keep its address while its title changes", async ({ page }) => {
  await openApp(page, { section: "bookmarks" });
  await add(page, "chrome://flags", "flags");
  const state = () => page.evaluate(() => (window as unknown as Mock).__shardxMock.state.bookmarks.find((b) => b.id === "flags")!);
  await row(page, "flags").locator("[data-cell='name']").click();
  const dialog = page.getByRole("dialog", { name: "Edit bookmark" });
  await expect(dialog).toHaveAttribute("data-state", "open");
  const url = dialog.getByRole("textbox", { name: "URL" });
  const said = dialog.getByText("Enter a full address, like https://example.com", { exact: true });
  await expect(url).toHaveValue("chrome://flags");
  // Touched into something unfinished, it is judged again.
  await url.fill("https://goog");
  await dialog.getByRole("button", { name: "Save", exact: true }).click();
  await expect(said).toBeVisible();
  expect((await state()).url).toBe("chrome://flags");
  // Put back as it was, it is not judged: only the title changes.
  await url.fill("chrome://flags");
  await expect(said).toHaveCount(0);
  await dialog.getByRole("textbox", { name: "Title" }).fill("Chrome flags");
  await dialog.getByRole("button", { name: "Save", exact: true }).click();
  await expect(dialog).toHaveCount(0);
  await expect(page.locator("[data-toast]").getByText("Chrome flags saved", { exact: true })).toBeVisible();
  expect(await state()).toMatchObject({ title: "Chrome flags", url: "chrome://flags" });
});

test("a new bookmark takes an intranet address and still refuses a bare word", async ({ page }) => {
  await openApp(page, { section: "bookmarks" });
  const count = () => page.evaluate(() => (window as unknown as Mock).__shardxMock.state.bookmarks.length);
  const before = await count();
  await page.getByRole("button", { name: "New bookmark" }).first().click();
  const dialog = page.getByRole("dialog", { name: "New bookmark" });
  await expect(dialog).toHaveAttribute("data-state", "open");
  const url = dialog.getByRole("textbox", { name: "URL" });
  await url.fill("goog");
  await dialog.getByRole("button", { name: "Save", exact: true }).click();
  await expect(dialog.getByText("Enter a full address, like https://example.com", { exact: true })).toBeVisible();
  expect(await count()).toBe(before);
  await url.fill("http://nas:5000");
  await dialog.getByRole("button", { name: "Save", exact: true }).click();
  await expect(dialog).toHaveCount(0);
  expect(await count()).toBe(before + 1);
});

test("a new bookmark takes a bookmarklet and keeps it as written", async ({ page }) => {
  await openApp(page, { section: "bookmarks" });
  await page.getByRole("button", { name: "New bookmark" }).first().click();
  const dialog = page.getByRole("dialog", { name: "New bookmark" });
  await expect(dialog).toHaveAttribute("data-state", "open");
  await dialog.getByRole("textbox", { name: "URL" }).fill("javascript:alert(document.title)");
  await dialog.getByRole("textbox", { name: "Title" }).fill("Show title");
  await dialog.getByRole("button", { name: "Save", exact: true }).click();
  await expect(dialog).toHaveCount(0);
  const saved = await page.evaluate(() => (window as unknown as Mock).__shardxMock.state.bookmarks.find((b) => b.title === "Show title"));
  expect(saved?.url).toBe("javascript:alert(document.title)");
});
