import { expect, test, type Locator, type Page } from "@playwright/test";
import { appModule, openApp, reloadApp } from "./support/app";
import { openSearch } from "./support/search";

// Columns size themselves to their content until a person resizes them
// (owner, 2026-09-30): "if we have in name more small names and one big,
// adjust to smaller width, so it looks better". Nine rows in ten fit; the
// rest truncate and show whole in a tooltip.
type List = "Profiles" | "Proxies";
const grid = (page: Page, list: List) => page.getByRole("treegrid", { name: list });
const header = (page: Page, list: List) => grid(page, list).locator("[role='row'][aria-rowindex='1']");
const colOf = (page: Page, list: List, id: string) => header(page, list).locator(`[data-col='${id}']`);
const edge = (page: Page, list: List, id: string) => header(page, list).locator(`[data-col-edge='${id}']`);
const width = (loc: Locator) => loc.evaluate((el) => el.getBoundingClientRect().width);
const stored = (page: Page) => page.evaluate(() => JSON.parse(localStorage.getItem("shardx.profiles.layout") ?? "null"));

/** Each row's text in a column: its full width (px) and whether it shows whole. */
const texts = (page: Page, list: List, id: string, text: string) =>
  grid(page, list).locator(`[role='row'][aria-rowindex]:not([aria-rowindex='1']) [data-cell='${id}']`).evaluateAll((cells, text) =>
    cells.map((cell) => {
      const el = (text ? cell.querySelector<HTMLElement>(text) : cell as HTMLElement)!;
      // A pinned name's pin sits after it (F17): its 14 px and the 6 px gap.
      const pin = cell.querySelector("[data-pin]");
      return { text: el.textContent ?? "", full: el.scrollWidth, whole: el.scrollWidth <= el.clientWidth, lead: el.getBoundingClientRect().left - cell.getBoundingClientRect().left, after: pin ? pin.getBoundingClientRect().width + 6 : 0 };
    }), text);

const LONG = "Sandbox for trying out a very long profile name that keeps going and going";
// Three names wider than Name's 116 px minimum, so the width is theirs.
const WIDER = {
  "9828bf28": "eBay DE buyer, the second account",
  aed41005: "Etsy craft shop for the summer sale",
  c31d92f1: "FB Ads acc-02 for the retargeting",
};

async function renameProfiles(page: Page, names: Record<string, string>) {
  await appModule(page, "/src/entities/profile/index.ts", "__profiles");
  await page.evaluate(async (names) => {
    const m = (window as unknown as { __profiles: typeof import("../src/entities/profile") }).__profiles;
    for (const [id, name] of Object.entries(names)) await m.useProfile.getState().renameProfile(id, name);
  }, names);
  // The mock saves its state a beat later: a reload must find the new names.
  await expect.poll(() => page.evaluate((names) => {
    const s = JSON.parse(localStorage.getItem("shardx-mock-state") ?? "null") as { profiles?: Record<string, { name: string }> } | null;
    return Object.entries(names).every(([id, name]) => s?.profiles?.[id]?.name === name);
  }, names)).toBe(true);
}

/** A column's own track in the header's grid, px: its fit (`min`, the width
 *  it stretches from) and what it may grow to (`max`: the width all its rows
 *  fit in, or its stretched width, an `fr` share of the row). */
async function trackOf(page: Page, list: List, id: string): Promise<{ min: number; max: number }> {
  const template = await header(page, list).evaluate((el) => (el as HTMLElement).style.gridTemplateColumns);
  const m = new RegExp(`--col-${id},\\s*(?:minmax\\(([\\d.]+)px,\\s*([\\d.]+)(?:px|fr)\\)|([\\d.]+)px)`).exec(template);
  expect(m, template).not.toBeNull();
  return m![3] ? { min: +m![3], max: +m![3] } : { min: +m![1], max: +m![2] };
}
const nameTrack = (page: Page) => trackOf(page, "Profiles", "name");

/** Name fits every name but the one long one: its fit is theirs (the
 *  nine-in-ten rule over 8 rows leaves out only the widest), within a px or
 *  two, and the long one truncates. Since F17 (merge point 3) the Proxy
 *  column needs less, so the list has room left over at 1280, and that room
 *  goes first to a cut column (controller ruling, 2026-09-30): Name may
 *  show more of the long one, never less than its fit. Returns Name's width. */
async function expectNameFitsAllButLong(page: Page): Promise<number> {
  const rows = await texts(page, "Profiles", "name", ".truncate");
  const others = rows.filter((r) => r.text !== LONG);
  const need = Math.max(...others.map((r) => r.full + r.lead + r.after));
  const fit = (await nameTrack(page)).min;
  expect(fit).toBeGreaterThanOrEqual(need);
  expect(fit).toBeLessThanOrEqual(need + 3);
  const w = await width(colOf(page, "Profiles", "name"));
  expect(w).toBeGreaterThanOrEqual(fit - 0.5);
  for (const r of others) expect(r, r.text).toMatchObject({ whole: true });
  expect(rows.find((r) => r.text === LONG)).toMatchObject({ whole: false });
  return w;
}

test("Profiles: a long name among short ones leaves Name at the short ones' width; it truncates and shows whole in its tooltip; the search never moves it", async ({ page }) => {
  await openApp(page);
  await renameProfiles(page, { ...WIDER, f62a9d43: LONG });
  // Recomputed a beat after the data changes (150 ms), from every row.
  await expect.poll(() => width(colOf(page, "Profiles", "name"))).toBeGreaterThan(160);
  const w = await expectNameFitsAllButLong(page);
  const longRow = grid(page, "Profiles").locator("[role='row'][data-profile-id='f62a9d43']");
  const longFull = (await texts(page, "Profiles", "name", ".truncate")).find((r) => r.text === LONG)!.full;
  expect(w).toBeLessThan(longFull - 100);

  // A name that shows whole says nothing more on hover; the cut one says it all.
  await grid(page, "Profiles").locator("[role='row'][data-profile-id='9828bf28'] [data-cell='name'] .truncate").hover();
  await page.waitForTimeout(600);
  await expect(page.getByRole("tooltip")).toHaveCount(0);
  await longRow.locator("[data-cell='name'] .truncate").hover();
  await expect(page.getByRole("tooltip")).toHaveText(LONG);

  // Typing in the search never resizes a column: the list it sizes to leaves the search out.
  await (await openSearch(page)).fill("Sandbox for");
  await expect(grid(page, "Profiles").locator("[role='row'][data-profile-id]")).toHaveCount(1);
  await page.waitForTimeout(400);
  expect(await width(colOf(page, "Profiles", "name"))).toBe(w);
});

test("Profiles: a Name width dragged by hand stays after a reload; a double click on its edge gives it back to its content, after a reload too", async ({ page }) => {
  test.slow(); // renames every profile and reloads twice: about 10 s
  await openApp(page);
  await renameProfiles(page, { ...WIDER, f62a9d43: LONG });
  await expect.poll(() => width(colOf(page, "Profiles", "name"))).toBeGreaterThan(160);
  const auto = await width(colOf(page, "Profiles", "name"));

  const e = (await edge(page, "Profiles", "name").boundingBox())!;
  await page.mouse.move(e.x + e.width / 2, e.y + e.height / 2);
  await page.mouse.down();
  await page.mouse.move(e.x + e.width / 2 + 60, e.y + e.height / 2, { steps: 6 });
  await page.mouse.up();
  const drawn = await width(colOf(page, "Profiles", "name"));
  expect(Math.abs(drawn - (auto + 60))).toBeLessThanOrEqual(1);
  // Kept as the width Name stretches from with the row (owner, 2026-10-23).
  const mine = (await stored(page)).widths.name as number;
  expect((await nameTrack(page)).min).toBe(mine);
  await reloadApp(page);
  expect(Math.abs((await width(colOf(page, "Profiles", "name"))) - drawn)).toBeLessThanOrEqual(0.5);
  // Rows added or renamed never move a width a person set.
  await renameProfiles(page, { "9828bf28": "eBay" });
  await page.waitForTimeout(400);
  expect(Math.abs((await width(colOf(page, "Profiles", "name"))) - drawn)).toBeLessThanOrEqual(0.5);
  await renameProfiles(page, { "9828bf28": WIDER["9828bf28"] });

  await edge(page, "Profiles", "name").dblclick();
  expect((await stored(page)).widths).toEqual({});
  await expect.poll(() => width(colOf(page, "Profiles", "name"))).toBe(auto);
  await reloadApp(page);
  expect(await width(colOf(page, "Profiles", "name"))).toBe(auto);
  await expectNameFitsAllButLong(page);
});

test("Proxies: Name and Host:Port size to their content the same way; a long name truncates and shows whole in its tooltip", async ({ page }) => {
  await openApp(page, { section: "proxies" });
  const long = "Warsaw datacenter pool for the long running scrapers, second batch";
  await appModule(page, "/src/entities/proxy/index.ts", "__proxies");
  await page.evaluate(async (name) => {
    const m = (window as unknown as { __proxies: typeof import("../src/entities/proxy") }).__proxies;
    const entry = m.useProxy.getState().proxies.find((p) => p.id === "px-new")!;
    await m.useProxy.getState().saveProxy({ ...entry, name });
  }, long);
  await expect(grid(page, "Proxies").locator("[role='row'][data-proxy-id='px-new'] [data-cell='name']")).toHaveText(long);

  // Name: every name but the long one shows whole, at their width (the
  // width it stretches from with the row).
  await expect.poll(async () => (await texts(page, "Proxies", "name", ".truncate")).filter((r) => !r.whole).map((r) => r.text)).toEqual([long]);
  const names = await texts(page, "Proxies", "name", ".truncate");
  const need = Math.max(...names.filter((r) => r.text !== long).map((r) => r.full));
  const w = (await trackOf(page, "Proxies", "name")).min;
  expect(w).toBeGreaterThanOrEqual(need);
  expect(w).toBeLessThanOrEqual(need + 3);
  expect(await width(colOf(page, "Proxies", "name"))).toBeGreaterThanOrEqual(w - 0.5);
  await grid(page, "Proxies").locator("[role='row'][data-proxy-id='px-new'] [data-cell='name'] .truncate").hover();
  await expect(page.getByRole("tooltip")).toHaveText(long);

  // Host:Port: sized to its addresses (wider than its old 150 px default), each whole.
  const addresses = await texts(page, "Proxies", "address", "");
  for (const r of addresses) expect(r, r.text).toMatchObject({ whole: true });
  const a = (await trackOf(page, "Proxies", "address")).min;
  expect(a).toBeGreaterThan(150);
  expect(a).toBeLessThanOrEqual(Math.max(...addresses.map((r) => r.full)) + 3);
});

// Controller ruling (2026-09-30): the room left over first lets a column cut
// at its content width show whole (each up to its maximum). "Frankfurt am
// Main" was cut ("Frankfurt …") beside empty space. The rest stretches every
// column (owner, 2026-10-23): no gap is left at the row's end.
test("Proxies: the room left over lets a cut Country and Test result show whole, then stretches the columns: no gap at the row end", async ({ page }) => {
  await openApp(page, { section: "proxies" });
  const row = grid(page, "Proxies").locator("[role='row'][data-proxy-id='px-de-1']");
  await expect(row.locator("[data-cell='country']")).toContainText("Frankfurt am Main");
  await expect.poll(async () => (await texts(page, "Proxies", "country", ".truncate")).filter((r) => !r.whole).map((r) => r.text)).toEqual([]);
  // Each result is a pill (Live layer) and, for a reached proxy, the IP
  // beside it: both whole.
  const results = await texts(page, "Proxies", "test", ".truncate");
  for (const r of results) expect(r, r.text).toMatchObject({ whole: true });
  const cells = grid(page, "Proxies").locator("[role='row'][aria-rowindex]:not([aria-rowindex='1']) [data-cell='test']");
  const ips = await cells.evaluateAll((els) => els.flatMap((cell) => {
    const ip = cell.querySelector<HTMLElement>(".mono");
    return ip ? [{ text: ip.textContent ?? "", whole: ip.scrollWidth <= ip.clientWidth }] : [];
  }));
  expect(ips.length).toBeGreaterThan(0);
  for (const r of ips) expect(r, r.text).toMatchObject({ whole: true });
  // The last column stretches from the width of its results (pill, gap and
  // IP), no wider; the actions cell after it is the Columns button's 24 px.
  const reach = await cells.evaluateAll((els) => els.map((cell) => {
    const range = document.createRange();
    range.selectNodeContents(cell);
    return range.getBoundingClientRect().right - cell.getBoundingClientRect().left;
  }));
  expect((await trackOf(page, "Proxies", "test")).min).toBeLessThanOrEqual(Math.max(...reach) + 3);
  expect(await width(colOf(page, "Proxies", "test"))).toBeGreaterThanOrEqual((await trackOf(page, "Proxies", "test")).min - 0.5);
  expect(await width(header(page, "Proxies").locator(".list-pin-end"))).toBe(24);
});

// Controller ruling (merge point 3): at the smallest window the Test
// result's IP read "OK 56 ms 2…". Short of room, the IP moves whole into the
// pill's tooltip before Country or Host:Port are cut.
test("Proxies, short of room: the Test result's IP moves into the pill's tooltip before Country or Host:Port are cut; at the smallest window no row shows it cut", async ({ page }) => {
  await openApp(page, { section: "proxies" });
  const row = grid(page, "Proxies").locator("[role='row'][data-proxy-id='px-de-1']");
  const ips = grid(page, "Proxies").locator("[data-cell='test'] .mono");
  const ip = (await row.locator("[data-cell='test'] .mono").textContent())!;
  expect(ip).toMatch(/^\d+\.\d+\.\d+\.\d+$/);

  // A little short: only the IP goes, and Country and Host:Port show whole.
  // The page panel's ground and 16 px gutter (borderless, owner 2026-10-01)
  // take 6 px more than the bare page's 21 px gutter did, and the list's tile
  // (borderless step 2, 4 px of inset each side) another 6: the list of 1180
  // at 1192, its select column in its own slot again (owner, 2026-10-07).
  await page.setViewportSize({ width: 1192, height: 800 });
  await expect(ips).toHaveCount(0);
  await expect.poll(async () => (await texts(page, "Proxies", "country", ".truncate")).filter((r) => !r.whole).map((r) => r.text)).toEqual([]);
  for (const r of await texts(page, "Proxies", "address", "")) expect(r, r.text).toMatchObject({ whole: true });
  // At the pill's start: the row's hover actions, shown under the pointer,
  // may lie over its end now that the type roles track -0.01em (owner,
  // 2026-10-01) and the columns sit a little tighter.
  await row.locator("[data-pill='proxy-test']").hover({ position: { x: 3, y: 4 } });
  await expect(page.getByRole("tooltip")).toContainText(`IP ${ip}`);

  // The smallest window: the columns give way too; still no IP shows, cut or whole.
  await page.mouse.move(0, 0);
  await page.setViewportSize({ width: 1024, height: 680 });
  await expect.poll(async () => (await texts(page, "Proxies", "country", ".truncate")).some((r) => !r.whole)).toBe(true);
  await expect(ips).toHaveCount(0);
});

// Pair 24 fix round, I3: a width a person gave the last column stays. The
// row still fills (owner, 2026-10-23): the column before it takes the rest.
test("Profiles: a Notes width set by hand stays; the column before it takes the room left over", async ({ page }) => {
  await page.setViewportSize({ width: 1600, height: 800 });
  await page.addInitScript(() => localStorage.setItem("shardx.profiles.layout", JSON.stringify({ v: 2, order: ["name", "status", "folder", "tags", "proxy", "lastRun", "created", "runTime", "device", "extensions", "notes"], hidden: ["lastRun", "created", "runTime", "device", "extensions"], widths: { notes: 200 } })));
  await openApp(page);
  await expect(colOf(page, "Profiles", "notes")).toBeVisible();
  expect(await width(colOf(page, "Profiles", "notes"))).toBe(200);
  // No gap after Notes: the row actions' cell starts one gap after it.
  const notes = (await colOf(page, "Profiles", "notes").boundingBox())!;
  const end = (await header(page, "Profiles").locator(".list-pin-end").boundingBox())!;
  expect(Math.abs(end.x - (notes.x + notes.width) - 10)).toBeLessThanOrEqual(1);
  expect(await width(colOf(page, "Profiles", "proxy"))).toBeGreaterThan((await trackOf(page, "Profiles", "proxy")).min);
  expect((await stored(page)).widths).toEqual({ notes: 200 });
});

// Autofit review C1: a column most rows leave empty sized to the empty
// cells (p90 of 0 px) and cut the one row that has something. Only the
// cells that show something are measured.
test("Profiles: one profile's two tags show whole though no other profile has any", async ({ page }) => {
  await page.addInitScript(() => localStorage.setItem("shardx.tags", JSON.stringify({
    version: 1,
    tags: [{ id: "t-warm", name: "Warm-up", color: "orange" }, { id: "t-ads", name: "Ads", color: "blue" }],
    byProfile: { f62a9d43: ["t-warm", "t-ads"] },
  })));
  await openApp(page);
  const chips = grid(page, "Profiles").locator("[role='row'][data-profile-id='f62a9d43'] [data-cell='tags'] [data-tag] .truncate");
  await expect(chips).toHaveText(["Warm-up", "Ads"]);
  await expect.poll(() => chips.evaluateAll((els) => els.map((el) => el.scrollWidth <= el.clientWidth))).toEqual([true, true]);
});

// Autofit review I1: every column that can cut a value shows it whole on
// hover while it is cut: the Folder chip, the Tag chips (all of the row's
// tags), the Profiles Proxy address and the Proxies Country city. At the
// smallest window the lists have no room to spare, so the rare long one is cut.
test("at the smallest window, a cut Folder, Tag, Proxy address or Country shows whole on hover", async ({ page }) => {
  await page.setViewportSize({ width: 1024, height: 680 });
  const HOST = "residential-eu-west-long-hostname-eu1.shardx.example";
  const FOLDER = "Scrapers for the pricing team";
  await page.addInitScript(() => localStorage.setItem("shardx.tags", JSON.stringify({
    version: 1,
    tags: [{ id: "t-ads", name: "Ads", color: "blue" }, { id: "t-long", name: "Summer retargeting campaign for the second store", color: "orange" }, { id: "t-vip", name: "VIP", color: "green" }],
    byProfile: { f62a9d43: ["t-ads"], aed41005: ["t-long", "t-vip"] },
  })));
  await openApp(page);
  type Mock = { __shardxMock: { state: { proxies: { id: string; host: string }[]; profiles: Record<string, { _meta: { folder: string } }> }; save: () => void } };
  await page.evaluate(({ HOST, FOLDER }) => {
    const mock = (window as unknown as Mock).__shardxMock;
    mock.state.proxies.find((p) => p.id === "px-mobile-1")!.host = HOST;
    mock.state.profiles.e51f8c32._meta.folder = FOLDER;
    mock.save();
  }, { HOST, FOLDER });
  await reloadApp(page);
  const rowOf = (id: string) => grid(page, "Profiles").locator(`[role='row'][data-profile-id='${id}']`);
  const tipFor = async (el: Locator, text: string, on: Locator = el) => {
    await page.mouse.move(0, 0);
    await expect(page.getByRole("tooltip")).toHaveCount(0);
    await expect.poll(() => el.evaluate((e) => e.scrollWidth > e.clientWidth), { message: `${text} is cut` }).toBe(true);
    await on.hover({ position: { x: 8, y: 4 } });
    await expect(page.getByRole("tooltip")).toHaveText(text);
  };
  await tipFor(rowOf("e51f8c32").locator("[data-cell='folder'] [data-folder-chip] .truncate"), FOLDER);
  await tipFor(rowOf("aed41005").locator("[data-cell='tags'] [data-tag='t-long'] .truncate"), "Summer retargeting campaign for the second store, VIP");
  // Here every column but Name has given way to its minimum (controller
  // rulings, 2026-09-30, Pair 31): Proxy's 80 px can cut the proxy's name.
  // F17 (Lane A): the cell names the proxy, and its tooltip always carries
  // the whole address (with the name and the type), cut or not.
  const proxyName = rowOf("d40e7b21").locator("[data-cell='proxy'] [data-proxy-name]");
  await page.mouse.move(0, 0);
  await expect(page.getByRole("tooltip")).toHaveCount(0);
  await proxyName.hover({ position: { x: 8, y: 4 } });
  await expect(page.getByRole("tooltip")).toContainText(`${HOST}:1080`);

  await openApp(page, { section: "proxies" });
  await tipFor(grid(page, "Proxies").locator("[role='row'][data-proxy-id='px-de-1'] [data-cell='country'] .truncate"), "Frankfurt am Main");
});

// Autofit review I2: the list used to re-measure beside Peek 150 ms after a
// keystroke, while the person was still typing. Manual save: a name typed in
// the Edit proxy dialog reaches the list only on Save, so nothing moves while
// it is typed; it is measured once, when it is saved.
test("Proxies: a name typed in the Edit proxy dialog moves no column while it is typed; the list sizes to it once it is saved", async ({ page }) => {
  await openApp(page, { section: "proxies" });
  const template = () => header(page, "Proxies").evaluate((el) => (el as HTMLElement).style.gridTemplateColumns);
  await grid(page, "Proxies").locator("[role='row'][data-proxy-id='px-new'] [data-cell='name']").click();
  await page.locator("aside[data-peek='proxy'] [data-peek-header]").getByRole("button", { name: "Edit", exact: true }).click();
  const dialog = page.getByRole("dialog", { name: /^Edit / });
  const field = dialog.getByRole("textbox", { name: "Name", exact: true });
  await field.click();
  await field.press("End");
  // Read beside Peek: the narrower list has its columns give way (controller
  // ruling, 2026-09-30), which is not the typing's doing.
  const before = await template();
  // What Name's rows measure: beside Peek the list scrolls, so a longer name
  // that stays cut no longer moves the template (merge point 3: the room
  // left over grows only a column it makes whole); the measure tells.
  await appModule(page, "/src/entities/proxy/index.ts", "__proxies");
  const measured = () => page.evaluate(() =>
    (window as unknown as { __proxies: typeof import("../src/entities/proxy") }).__proxies.useProxyColumns.getState().fullWidths.name);
  const was = await measured();
  await field.pressSequentially(" and a much longer name for the second pool");
  // More than the list's 150 ms of rest: nothing was saved, so nothing moved.
  await page.waitForTimeout(500);
  await expect(grid(page, "Proxies").locator("[role='row'][data-proxy-id='px-new'] [data-cell='name']")).not.toContainText("second pool");
  expect(await template()).toBe(before);
  expect(await measured()).toBe(was);
  await dialog.getByRole("button", { name: "Save", exact: true }).click();
  await expect(dialog).toHaveCount(0);
  await expect(grid(page, "Proxies").locator("[role='row'][data-proxy-id='px-new'] [data-cell='name']")).toContainText("second pool");
  await expect.poll(measured).not.toBe(was);
});
