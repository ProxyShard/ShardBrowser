import { expect, test, type Page } from "@playwright/test";
import { openApp } from "./support/app";

// F9: the craft target state for ProxyShard.
type Mock = { __shardxMock: { state: { psKey: string }; save: () => void } };
const fail = (page: Page, cmds: Record<string, number>) => page.addInitScript((c) => Object.assign(window, { __shardxMockFail: c }), cmds);

// The owner's "more air, clearer hierarchy" (2026-10-01) replaces FEEL-41's
// one header line: the facts are stat tiles at the top of the page.
test("the account facts are stat tiles under the header", async ({ page }) => {
  await openApp(page, { section: "proxyshard" });
  await expect(page.locator("[data-ps-tiles]")).toContainText(/Balance\s*\$128\.40/);
  await expect(page.locator("[data-ps-tiles]")).toContainText(/Active orders\s*3/);
  await expect(page.locator("main header [data-ps-facts]")).toHaveCount(0);
  await expect(page.locator("main header").getByRole("button", { name: "Open dashboard" })).toBeVisible();
});

test("while connected the key lives in the header menu (craft target)", async ({ page }) => {
  await openApp(page, { section: "proxyshard" });
  await expect(page.locator("[data-ps-key-setup]")).toHaveCount(0);
  await page.locator("main header").getByRole("button", { name: "More actions" }).click();
  await page.getByRole("menuitem", { name: "Change API key" }).click();
  const dialog = page.getByRole("dialog", { name: "Change API key" });
  await expect(dialog.getByLabel("API key")).toBeVisible();
  await page.keyboard.press("Escape");
  await expect(dialog).toHaveCount(0);
});

test("residential traffic is a stat in the first tile, with a bar flush on its foot", async ({ page }) => {
  await page.setViewportSize({ width: 1280, height: 800 });
  await openApp(page, { section: "proxyshard" });
  const tile = page.locator("[data-ps-tile='residential']");
  await expect(tile.locator("[data-ps-traffic]")).toContainText(/6\.80?\sGB\s*of 20(\.00)?\sGB used/);
  await expect(tile.getByText("Remaining", { exact: true })).toHaveCount(0);
  // One row: Residential first at half width, Balance and Active orders equal, heights within 8 px.
  const boxes = await Promise.all(["residential", "balance", "orders"].map((id) => page.locator(`[data-ps-tile='${id}']`).boundingBox()));
  const [b3, b1, b2] = boxes.map((b) => b!);
  expect(Math.abs(b1.y - b2.y)).toBeLessThan(1);
  expect(Math.abs(b1.y - b3.y)).toBeLessThan(1);
  expect(b3.x).toBeLessThan(b1.x);
  expect(b1.x).toBeLessThan(b2.x);
  const rowWidth = b2.x + b2.width - b3.x;
  expect(Math.abs(b3.width - (rowWidth - 12) / 2)).toBeLessThanOrEqual(12);
  expect(Math.abs(b1.width - b2.width)).toBeLessThan(1);
  expect(Math.max(b1.height, b2.height, b3.height) - Math.min(b1.height, b2.height, b3.height)).toBeLessThanOrEqual(8);
  // The bar is flush with the tile's bottom edge, inside its border.
  const bar = (await tile.locator("[data-ps-bar]").boundingBox())!;
  expect(Math.abs(bar.y + bar.height - (b3.y + b3.height - 1))).toBeLessThanOrEqual(1);
  expect(bar.height).toBe(4);
  // The old wide card is gone: one Residential card, and it is the tile.
  await expect(page.locator("[data-ps-card='residential']")).toHaveCount(1);
  await expect(page.locator("[data-ps-tiles] [data-ps-card='residential']")).toHaveCount(1);
});

test("the plan select switches the plan and keeps it", async ({ page }) => {
  await openApp(page, { section: "proxyshard" });
  const tile = page.locator("[data-ps-tile='residential']");
  const plan = tile.getByRole("button", { name: "Residential plan" });
  await expect(plan).toHaveText("Standard");
  await plan.click();
  await page.getByRole("menuitemradio", { name: "Premium" }).click();
  await expect(plan).toHaveText("Premium");
  await expect(tile.locator("[data-ps-traffic]")).toContainText(/18\.40?\sGB\s*of 50(\.00)?\sGB used/);
  await page.reload();
  await expect(page.locator("[data-ps-tile='residential']").getByRole("button", { name: "Residential plan" })).toHaveText("Premium");
});

test("Buy traffic and Generate proxies open their dialogs from the tile", async ({ page }) => {
  await page.setViewportSize({ width: 1280, height: 800 });
  await openApp(page, { section: "proxyshard" });
  const tile = page.locator("[data-ps-tile='residential']");
  // The mock has a "Residential Premium" order and no Standard one.
  await expect(tile.getByRole("button", { name: "Buy traffic" })).toBeDisabled();
  await tile.getByRole("button", { name: "Residential plan" }).click();
  await page.getByRole("menuitemradio", { name: "Premium" }).click();
  await tile.getByRole("button", { name: "Buy traffic" }).click();
  await expect(page.getByRole("dialog")).toBeVisible();
  await page.keyboard.press("Escape");
  await expect(page.getByRole("dialog")).toHaveCount(0);
  await tile.getByRole("button", { name: "Generate proxies" }).click();
  await expect(page.getByRole("dialog")).toBeVisible();
});

test("narrow, the Residential tile takes the full row above the two small tiles", async ({ page }) => {
  await page.setViewportSize({ width: 900, height: 800 });
  await openApp(page, { section: "proxyshard" });
  await page.setViewportSize({ width: 640, height: 800 });
  const b1 = (await page.locator("[data-ps-tile='balance']").boundingBox())!;
  const b2 = (await page.locator("[data-ps-tile='orders']").boundingBox())!;
  const b3 = (await page.locator("[data-ps-tile='residential']").boundingBox())!;
  expect(Math.abs(b1.y - b2.y)).toBeLessThan(1);
  expect(b1.y).toBeGreaterThan(b3.y + b3.height);
  expect(b3.width).toBeGreaterThan(b1.width + b2.width);
});

test("the page spans the window: tiles, Usage, Orders and the header's actions share the body's right edge (owner, 2026-10-23)", async ({ page }) => {
  await page.setViewportSize({ width: 1280, height: 800 });
  await openApp(page, { section: "proxyshard" });
  const edges = () => page.evaluate(() => {
    const body = document.querySelector<HTMLElement>("[data-page-body]")!;
    const end = body.getBoundingClientRect().right - parseFloat(getComputedStyle(body).paddingRight) * (parseFloat(getComputedStyle(document.documentElement).zoom) || 1);
    const right = (sel: string) => document.querySelector(sel)!.getBoundingClientRect().right;
    const actions = [...document.querySelectorAll("main header button")].map((b) => b.getBoundingClientRect().right);
    return [right("[data-ps-tiles]"), right("[data-ps-usage]"), right("[data-ps-card='orders']"), right("[data-ps-card='orders'] [role='treegrid']"), Math.max(...actions)]
      .map((r) => Math.round(Math.abs(r - end)));
  });
  expect(await edges()).toEqual([0, 0, 0, 0, 0]);
  // 1280 as before the cap went: the body's 992 px.
  expect(Math.round((await page.locator("[data-ps-tiles]").boundingBox())!.width)).toBe(992);
  for (const [width, height] of [[1920, 1080], [2560, 1440]]) {
    await page.setViewportSize({ width, height });
    // A 2560 screen gets 125% by Auto (owner, 2026-10-23).
    await expect.poll(() => page.evaluate(() => getComputedStyle(document.documentElement).zoom)).toBe(width > 2200 ? "1.25" : "1");
    await expect.poll(edges).toEqual([0, 0, 0, 0, 0]);
    expect((await page.locator("[data-ps-tiles]").boundingBox())!.width).toBeGreaterThan(1120);
  }
});

test("in Traffic used the number, the unit and the trend share one baseline (owner)", async ({ page }) => {
  await openApp(page, { section: "proxyshard" });
  const stat = page.locator("[data-usage-card='traffic']").locator("[data-stat]");
  await expect(stat.locator("[data-trend]")).toBeVisible();
  // A zero-size inline probe at the end of each text sits on its baseline.
  const bases = await stat.evaluate((row) => {
    const texts = [row.querySelector("[data-stat-value]"), row.querySelector("[data-stat-unit]"), row.querySelector("[data-trend] > span[aria-hidden]:not(.inline-grid)")] as HTMLElement[];
    return texts.map((el) => {
      const probe = document.createElement("span");
      probe.style.cssText = "display:inline-block;width:0;height:0";
      el.appendChild(probe);
      const y = probe.getBoundingClientRect().bottom;
      probe.remove();
      return y;
    });
  });
  expect(bases.length).toBe(3);
  expect(Math.max(...bases) - Math.min(...bases)).toBeLessThanOrEqual(1);
});

test("without a key, the key card is the page (craft target)", async ({ page }) => {
  await openApp(page, { section: "proxyshard" });
  await page.evaluate(() => { const m = (window as unknown as Mock).__shardxMock; m.state.psKey = ""; m.save(); });
  await page.reload();
  const setup = page.locator("[data-ps-key-setup]");
  await expect(setup.getByRole("heading", { name: "Connect your ProxyShard account" })).toBeVisible();
  await expect(page.locator("[data-ps-card]")).toHaveCount(0);
  await expect(page.locator("main header [data-ps-facts]")).toHaveCount(0);
});

test("a failed check says it once, with one way to try again (PC-127)", async ({ page }) => {
  await fail(page, { ps_me: 1 });
  await openApp(page, { section: "proxyshard" });
  const setup = page.locator("[data-ps-key-setup]");
  await expect(setup.getByText("Couldn’t reach ProxyShard")).toBeVisible();
  await expect(setup.getByText("Mock failure: ps_me")).toHaveCount(1);
  await expect(setup.getByRole("button", { name: "Test" })).toHaveCount(0);
  await setup.getByRole("button", { name: "Check again" }).click();
  await expect(page.getByRole("heading", { name: "Orders" })).toBeVisible();
});

test("a rejected key is called a rejected key (PC-127)", async ({ page }) => {
  await openApp(page, { section: "proxyshard" });
  await page.evaluate(() => { const m = (window as unknown as Mock).__shardxMock; m.state.psKey = "bad-key"; m.save(); });
  await page.reload();
  await expect(page.locator("[data-ps-key-setup]").getByText("This key didn’t work")).toBeVisible();
});

test("generated proxies are named after the translated plan (CRAFT-22)", async ({ page }) => {
  await openApp(page, { section: "proxyshard" });
  await page.locator("[data-ps-tile='residential']").getByRole("button", { name: "Generate proxies" }).click();
  const dialog = page.getByRole("dialog");
  await expect(dialog.getByLabel("Name prefix")).toHaveValue("Standard residential");
  await expect(dialog).not.toContainText("standart");
  await expect(dialog.getByText("Generated proxies are added to your proxy list.")).toBeVisible();
});

test("the Add to proxies dialog names each row checkbox by its address", async ({ page }) => {
  await openApp(page, { section: "proxyshard" });
  const grid = page.locator("[data-ps-card='orders']").getByRole("treegrid", { name: "Orders" });
  const row = grid.getByRole("row").filter({ hasText: "Datacenter USA" });
  await row.hover();
  await row.getByRole("button", { name: "Add to proxies" }).click();
  const dialog = page.getByRole("dialog");
  // "Select <host>:<port>": the name a screen reader reads (SOCKS5 port by default).
  const box = dialog.getByRole("checkbox", { name: /^Select \d+\.\d+\.\d+\.\d+:\d+$/ });
  await expect(box.first()).toBeVisible();
  await expect(box.first()).toHaveAccessibleName(/^Select \d{1,3}(\.\d{1,3}){3}:1080$/);
});

test("proxies added from an order are in Proxies at once, no restart needed (owner, 2026-11-06)", async ({ page }) => {
  // Proxies read first, so a list that stays as it was would show here.
  await openApp(page, { section: "proxies" });
  const rows = page.locator("[role='row'][data-proxy-id]");
  await expect(rows.first()).toBeVisible();
  const before = await rows.count();
  await page.locator("aside [data-nav='proxyshard']").click();
  const grid = page.locator("[data-ps-card='orders']").getByRole("treegrid", { name: "Orders" });
  const row = grid.getByRole("row").filter({ hasText: "Datacenter USA" });
  await row.hover();
  await row.getByRole("button", { name: "Add to proxies" }).click();
  const dialog = page.getByRole("dialog");
  await expect(dialog.getByRole("checkbox", { name: /^Select \d/ }).first()).toBeVisible();
  await dialog.getByRole("button", { name: /^Add \d+$/ }).click();
  await expect(dialog).toBeHidden();
  // Ones already in the list are skipped: the toast says how many came in.
  const added = Number((await page.getByText(/^Added \d+ prox/).first().textContent())!.match(/\d+/)![0]);
  expect(added).toBeGreaterThan(0);
  await page.locator("aside [data-nav='proxies']").click();
  await expect(rows).toHaveCount(before + added);
});

test("generated proxies are in Proxies at once, no restart needed (owner, 2026-11-06)", async ({ page }) => {
  await openApp(page, { section: "proxies" });
  await expect(page.locator("[role='row'][data-proxy-id]").first()).toBeVisible();
  await page.locator("aside [data-nav='proxyshard']").click();
  await page.locator("[data-ps-tile='residential']").getByRole("button", { name: "Generate proxies" }).click();
  const dialog = page.getByRole("dialog");
  await dialog.getByLabel("Name prefix").fill("Fresh resi");
  await dialog.getByRole("button", { name: /^Generate \d+$/ }).click();
  await expect(page.getByText(/Added \d+ prox/).first()).toBeVisible();
  await dialog.getByRole("button", { name: "Cancel" }).click();
  await page.locator("aside [data-nav='proxies']").click();
  await expect(page.getByText(/^Fresh resi/).first()).toBeVisible();
});

test("orders are a table with a header row; actions show on the row (craft target)", async ({ page }) => {
  await openApp(page, { section: "proxyshard" });
  const grid = page.locator("[data-ps-card='orders']").getByRole("treegrid", { name: "Orders" });
  await expect(grid.getByRole("columnheader")).toHaveText(["Product", "Order", "Until", "Tag", "Actions"]);
  const row = grid.getByRole("row").filter({ hasText: "Datacenter USA" });
  await expect(row).toContainText("#41390");
  await row.hover();
  await expect(row.getByRole("button", { name: "Add to proxies" })).toBeVisible();
});

test("pagination hides under an orders error (PC-126)", async ({ page }) => {
  await page.addInitScript(() => Object.assign(window, { __shardxMockFail: { ps_orders: 4 } }));
  await openApp(page, { section: "proxyshard" });
  const orders = page.locator("[data-ps-card='orders']");
  await expect(orders.getByText("Couldn’t load orders")).toBeVisible();
  await expect(orders.getByRole("button", { name: /Next/ })).toHaveCount(0);
});

test("Buy opens a dialog; the price appears by itself without moving Pay (LAYOUT-18)", async ({ page }) => {
  // The first price is held back, so Pay is measured before it (as "Buy") and after (as "Pay $…").
  await page.addInitScript(() => Object.assign(window, { __shardxMockSlow: { ps_calculate: 600 } }));
  await openApp(page, { section: "proxyshard" });
  await page.getByRole("button", { name: "Buy proxies" }).click();
  const dialog = page.getByRole("dialog", { name: "Buy proxies" });
  const pay = dialog.getByRole("button", { name: /^Pay|^Buy$/ });
  await expect(pay).toBeVisible();
  await expect(pay).toBeDisabled();
  const before = (await pay.boundingBox())!;
  await expect(dialog.locator("[data-ps-buy-price]")).toContainText("$");
  await expect(dialog.getByRole("button", { name: /^Pay / })).toBeEnabled();
  const after = (await dialog.getByRole("button", { name: /^Pay / }).boundingBox())!;
  expect(Math.abs(after.x + after.width - (before.x + before.width))).toBeLessThanOrEqual(1);
  expect(Math.abs(after.y - before.y)).toBeLessThanOrEqual(1);
});

test("the purchase confirm names the price and the balance (WRITE-21)", async ({ page }) => {
  await openApp(page, { section: "proxyshard" });
  await page.getByRole("button", { name: "Buy proxies" }).click();
  const dialog = page.getByRole("dialog", { name: "Buy proxies" });
  await expect(dialog.getByRole("button", { name: /^Pay / })).toBeEnabled();
  await dialog.getByRole("button", { name: /^Pay / }).click();
  const ask = page.getByRole("alertdialog");
  await expect(ask).toContainText(/Buy 1 × .+\?/);
  await expect(ask).toContainText("is charged to your ProxyShard balance.");
  await expect(ask.getByRole("button", { name: /^Pay \$/ })).toBeVisible();
  await ask.getByRole("button", { name: "Cancel" }).click();
});

test("Residential fields leave no hole in the form (LAYOUT-18)", async ({ page }) => {
  await openApp(page, { section: "proxyshard" });
  await page.getByRole("button", { name: "Buy proxies" }).click();
  const dialog = page.getByRole("dialog", { name: "Buy proxies" });
  await dialog.getByRole("combobox", { name: "Product" }).click();
  await page.getByRole("option", { name: /Residential/ }).first().click();
  const quantity = (await dialog.getByRole("textbox", { name: "Quantity" }).boundingBox())!;
  const cycle = (await dialog.getByRole("combobox", { name: "Billing cycle" }).boundingBox())!;
  // Quantity flows into the first column's next cell, under Product. (The
  // text box sits 8 px inside its field's frame; the Product trigger is the frame.)
  const product = (await dialog.getByRole("combobox", { name: "Product" }).boundingBox())!;
  expect(quantity.x).toBeLessThan(cycle.x);
  expect(Math.abs(quantity.x - product.x)).toBeLessThanOrEqual(9);
  expect(quantity.y).toBeGreaterThan(cycle.y);
});
