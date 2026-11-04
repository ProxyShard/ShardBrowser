import { expect, test, type Page } from "@playwright/test";
import { openApp, pressCreate } from "./support/app";

// React runs a development effect twice; a production build runs it once.
const EFFECT_RUNS = process.env.E2E_BUILT === "1" ? 1 : 2;

const fail = (page: Page, cmds: Record<string, number>) => page.addInitScript((c) => { (window as unknown as { __shardxMockFail: Record<string, number> }).__shardxMockFail = c; }, cmds);
type Mock = { state: { psKey: string; psOrders: { product_name: string }[] }; save: () => void };

test("ProxyShard: no key says what a key gives", async ({ page }) => {
  await openApp(page, { section: "proxyshard" });
  await page.evaluate(() => { const m = (window as unknown as { __shardxMock: Mock }).__shardxMock; m.state.psKey = ""; m.save(); });
  await page.reload();
  // The key card is the page, and says what the key is for (F32).
  await expect(page.locator("[data-ps-key-setup]").getByRole("heading", { name: "Connect your ProxyShard account" })).toBeVisible();
});

test("ProxyShard: a key that fails says why, and Check again fixes it", async ({ page }) => {
  await fail(page, { ps_me: 1 });
  await openApp(page, { section: "proxyshard" });
  // The injected failure is not a rejection: ProxyShard was out of reach (PC-127).
  await expect(page.getByText("Couldn’t reach ProxyShard")).toBeVisible();
  await expect(page.locator("[data-ps-key-state]").getByText("Mock failure: ps_me")).toBeVisible();
  await page.getByRole("button", { name: "Check again" }).click();
  await expect(page.getByText("Couldn’t reach ProxyShard")).toHaveCount(0);
  await expect(page.getByRole("heading", { name: "Orders" })).toBeVisible();
});

test("ProxyShard: orders that fail to load offer Retry; a status with none offers Show all", async ({ page }) => {
  // Two cards read ps_orders on the page (the shop reads them when its
  // dialog opens), each EFFECT_RUNS times: all of those calls fail.
  await fail(page, { ps_orders: 2 * EFFECT_RUNS });
  await openApp(page, { section: "proxyshard" });
  const orders = page.locator("[data-ps-card='orders']");
  await expect(orders.getByText("Couldn’t load orders")).toBeVisible();
  await orders.getByRole("button", { name: "Retry" }).click();
  await expect(orders.getByText("Couldn’t load orders")).toHaveCount(0);
  await orders.getByRole("combobox", { name: "Status" }).click();
  await page.getByRole("option", { name: "On hold", exact: true }).click();
  await expect(page.getByText("No on hold datacenter or ISP orders")).toBeVisible();
  await page.getByRole("button", { name: "Show all" }).click();
  await expect(page.getByText("No on hold datacenter or ISP orders")).toHaveCount(0);
});

test("ProxyShard: the shop that fails to load offers Retry", async ({ page }) => {
  await fail(page, { ps_products: 1 });
  await openApp(page, { section: "proxyshard" });
  // The shop is a dialog now (F33): it loads when it opens.
  await page.getByRole("button", { name: "Buy proxies" }).click();
  const dialog = page.getByRole("dialog", { name: "Buy proxies" });
  await expect(dialog.getByText("Couldn’t load the shop")).toBeVisible();
  await dialog.getByRole("button", { name: "Retry" }).click();
  await expect(dialog.getByText("Couldn’t load the shop")).toHaveCount(0);
});

test("Profile Peek: a form that fails to load says so, Retry loads it, and the next profile loads", async ({ page }) => {
  // Armed once the page is idle: Profiles reads the timezones of the
  // profiles on tested proxies (the Location leak card) when it opens.
  await openApp(page);
  await page.evaluate(() => { (window as unknown as { __shardxMockFail: Record<string, number> }).__shardxMockFail = { profile_get: 1 }; });
  await page.locator("[role='row'][data-profile-id]", { hasText: "Sandbox" }).locator("[data-cell='name']").click();
  const peek = page.locator("aside[data-peek]");
  await expect(peek.getByText("Couldn’t load this profile")).toBeVisible();
  await peek.getByRole("button", { name: "Retry" }).click();
  // Peek only shows: the form it read is its Fingerprint rows.
  const os = peek.locator("[data-peek-section='fingerprint'] [data-property='os']");
  await expect(os).toBeVisible();
  await expect(peek.getByText("Couldn’t load this profile")).toHaveCount(0);
  await page.locator("[role='row'][data-profile-id]", { hasText: "Etsy craft shop" }).locator("[data-cell='name']").click();
  await expect(peek).toHaveAttribute("aria-label", "Peek: Etsy craft shop");
  await expect(os).toBeVisible();
});

test("Settings: a file that can't be read says so, and shows nothing until Retry reads it", async ({ page }) => {
  // Two reads at start, in either order: the profile list's (is the password switch on?) and this page's.
  await fail(page, { settings_get: 2 });
  await openApp(page, { section: "settings" });
  await expect(page.getByText("Couldn’t load settings")).toBeVisible();
  await expect(page.getByRole("switch", { name: /tray/i })).toHaveCount(0);
  await page.getByRole("button", { name: "Retry" }).click();
  await expect(page.getByText("Couldn’t load settings")).toHaveCount(0);
  // The settings show once read; there is no Save to hold back (O6).
  await expect(page.getByRole("switch", { name: /tray/i })).toBeVisible();
});

test("Settings > API & MCP: API info that can't be read offers Retry", async ({ page }) => {
  // The sidebar's API status reads api_info too: fail every call until Retry.
  await fail(page, { api_info: 99 });
  await openApp(page, { section: "settings" });
  await page.getByRole("tab", { name: "API & MCP" }).click();
  await expect(page.getByText("Couldn’t load the API details")).toBeVisible();
  await page.evaluate(() => { (window as unknown as { __shardxMockFail: Record<string, number> }).__shardxMockFail.api_info = 0; });
  await page.getByRole("button", { name: "Retry" }).click();
  await expect(page.getByText("Couldn’t load the API details")).toHaveCount(0);
  // The base URL is back (it sits in a read-only field under this label).
  await expect(page.getByText("Base URL", { exact: true })).toBeVisible();
});

test("Assign proxies with no proxies points to the Proxies page", async ({ page }) => {
  await page.addInitScript(() => { (window as unknown as { __shardxMockEmpty: boolean }).__shardxMockEmpty = true; });
  await openApp(page);
  await page.getByRole("button", { name: "Skip" }).click();
  await page.getByRole("region", { name: "Get started" }).getByRole("button", { name: "New profile" }).click();
  await pressCreate(page);
  await page.locator("aside[data-peek]").getByRole("button", { name: "Close Peek" }).click();
  await page.locator("[role='row'][data-profile-id]").first().locator("[data-cell='checkbox'] input").check({ force: true });
  await page.getByRole("toolbar", { name: /selected/ }).getByRole("button", { name: /Change proxy/ }).click();
  await expect(page.getByRole("dialog")).toContainText("No proxies yet");
  await page.getByRole("dialog").getByRole("button", { name: "Go to proxies" }).click();
  await expect(page.getByRole("heading", { level: 1 })).toHaveText("Proxies");
});

test("in Chinese", async ({ page }) => {
  await fail(page, { ps_me: 1 });
  await page.addInitScript(() => localStorage.setItem("shardx.lang", "zh"));
  await openApp(page, { section: "proxyshard" });
  await expect(page.getByText("无法连接 ProxyShard")).toBeVisible();
});

test("ProxyShard: a residential-only account isn't told it has no orders", async ({ page }) => {
  await openApp(page, { section: "proxyshard" });
  await page.evaluate(() => { const m = (window as unknown as { __shardxMock: Mock }).__shardxMock; m.state.psOrders = m.state.psOrders.filter((o) => /Residential/.test(o.product_name)); m.save(); });
  await page.reload();
  const orders = page.locator("[data-ps-card='orders']");
  await expect(orders.getByText("No active datacenter or ISP orders")).toBeVisible();
  await orders.getByRole("button", { name: "Show all" }).click();
  await expect(orders.getByText("No datacenter or ISP orders yet")).toBeVisible();
});

test("ProxyShard: a slow answer for the old status never lands under the new one", async ({ page }) => {
  await openApp(page, { section: "proxyshard" });
  const orders = page.locator("[data-ps-card='orders']");
  // A row's actions show on hover (F33, the list recipe).
  await orders.locator("[data-row-id]").first().hover();
  await expect(orders.getByRole("button", { name: /Renew|Add to proxies/ }).first()).toBeVisible();
  const setSlow = (ms: number) => page.evaluate((v) => { (window as unknown as { __shardxMockSlow: Record<string, number> }).__shardxMockSlow = { ps_orders: v }; }, ms);
  await setSlow(1500);
  await orders.getByRole("button", { name: "Refresh" }).click();
  await setSlow(0);
  await orders.getByRole("combobox", { name: "Status" }).click();
  await page.getByRole("option", { name: "On hold", exact: true }).click();
  await expect(orders.getByText("No on hold datacenter or ISP orders")).toBeVisible();
  await page.waitForTimeout(1800);
  await expect(orders.getByText("No on hold datacenter or ISP orders")).toBeVisible();
  expect(await orders.getByRole("button", { name: /Add to proxies/ }).count()).toBe(0);
});

test("ProxyShard: switching the status straight from an orders error loads again", async ({ page }) => {
  await fail(page, { ps_orders: 2 * EFFECT_RUNS });
  await openApp(page, { section: "proxyshard" });
  const orders = page.locator("[data-ps-card='orders']");
  await expect(orders.getByText("Couldn’t load orders")).toBeVisible();
  await orders.getByRole("combobox", { name: "Status" }).click();
  await page.getByRole("option", { name: "All", exact: true }).click();
  await expect(orders.getByText("Couldn’t load orders")).toHaveCount(0);
  await orders.locator("[data-row-id]").first().hover();
  await expect(orders.getByRole("button", { name: /Add to proxies/ }).first()).toBeVisible();
});

test("ProxyShard: saving a new key after a failure clears the failure", async ({ page }) => {
  await fail(page, { ps_me: 1 });
  await openApp(page, { section: "proxyshard" });
  await expect(page.getByText("Couldn’t reach ProxyShard")).toBeVisible();
  const key = page.getByLabel(/API key/).first();
  await key.fill("psk_live_new_key_0000000000000000");
  await key.press("Enter");
  await expect(page.getByText("Couldn’t reach ProxyShard")).toHaveCount(0);
  await expect(page.getByRole("heading", { name: "Orders" })).toBeVisible();
});
