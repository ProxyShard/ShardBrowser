import { expect, test, type Page } from "@playwright/test";
import { appModule, openApp, reloadApp, waitForIdle } from "./support/app";

// Clarity fixes, Task 14 (corrections items 12, 13, 14): the buy button says
// what it does, the balance follows a purchase, the price follows the form,
// and a residential plan is Lifetime.

type Mock = { __shardxMock: { calls: Record<string, number>; log: { cmd: string; args: Record<string, unknown> }[] } };
/** The arguments of every price request so far, oldest first. */
const priceRequests = (page: Page) =>
  page.evaluate(() => (window as unknown as Mock).__shardxMock.log.filter((c) => c.cmd === "ps_calculate").map((c) => c.args));
const slowPrice = (page: Page, ms: number) => page.evaluate((n) => Object.assign(window, { __shardxMockSlow: { ps_calculate: n } }), ms);

/** The Buy proxies dialog with its first price in (Residential Standart, $7.00). */
async function openBuy(page: Page) {
  await openApp(page, { section: "proxyshard" });
  await page.getByRole("button", { name: "Buy proxies" }).click();
  const dialog = page.getByRole("dialog", { name: "Buy proxies" });
  const price = dialog.locator("[data-ps-buy-price]");
  const pay = dialog.getByRole("button", { name: /^Pay |^Buy$/ });
  await expect(price).toContainText("$7.00");
  await expect(pay).toBeEnabled();
  return { dialog, price, pay, quantity: dialog.getByRole("textbox", { name: "Quantity" }) };
}

test("Buy traffic is a labelled button", async ({ page }) => {
  await page.setViewportSize({ width: 1280, height: 800 });
  await openApp(page, { section: "proxyshard" });
  const tile = page.locator("[data-ps-tile='residential']");
  const buy = tile.getByRole("button", { name: "Buy traffic" });
  // A button with its word, not a bare "+"; the mock has no Standard order, so it waits.
  await expect(buy).toBeVisible();
  await expect(buy).toHaveText("Buy traffic");
  await expect(buy).toBeDisabled();
  await expect(tile.getByRole("button", { name: "Add traffic" })).toHaveCount(0);
  // Beside Generate proxies, which stays the one primary button.
  const generate = (await tile.getByRole("button", { name: "Generate proxies" }).boundingBox())!;
  const box = (await buy.boundingBox())!;
  expect(box.x + box.width).toBeLessThanOrEqual(generate.x);
  expect(Math.abs(box.height - generate.height)).toBeLessThanOrEqual(1);
  // Premium has an order: the button works and opens the top-up.
  await tile.getByRole("button", { name: "Residential plan" }).click();
  await page.getByRole("menuitemradio", { name: "Premium" }).click();
  await expect(buy).toBeEnabled();
  await buy.click();
  await expect(page.getByRole("dialog", { name: "Buy traffic" })).toBeVisible();
});

test("after a purchase the balance updates without reload", async ({ page }) => {
  const { dialog, pay } = await openBuy(page);
  const tiles = page.locator("[data-ps-tiles]");
  const tile = page.locator("[data-ps-tile='residential']");
  await expect(tiles).toContainText(/Balance\s*\$128\.40/);
  await expect(tiles).toContainText(/Active orders\s*3/);
  await expect(tile.getByRole("button", { name: "Buy traffic" })).toBeDisabled();
  await pay.click();
  await page.getByRole("alertdialog").getByRole("button", { name: "Pay $7.00" }).click();
  await expect(dialog).toHaveCount(0);
  // The wallet, the order count and the Residential tile's order all read again by themselves.
  await expect(tiles).toContainText(/Balance\s*\$121\.40/);
  await expect(tiles).toContainText(/Active orders\s*4/);
  await expect(tile.getByRole("button", { name: "Buy traffic" })).toBeEnabled();
});

test("changing quantity updates the price without Calculate", async ({ page }) => {
  const { dialog, price, quantity } = await openBuy(page);
  await expect(dialog.getByRole("button", { name: "Calculate price" })).toHaveCount(0);
  await quantity.fill("3");
  await expect(price).toContainText("$21.00");
  await expect(price).not.toContainText("Updating");
  // Every other field asks again too.
  let asked = (await priceRequests(page)).length;
  await dialog.getByRole("checkbox", { name: "Auto-renew" }).check();
  await expect.poll(async () => (await priceRequests(page)).length).toBe(asked + 1);
  // A burst of keys is one request, sent 300 ms after the last.
  asked = (await priceRequests(page)).length;
  await quantity.fill("");
  await quantity.pressSequentially("20", { delay: 40 });
  await expect(price).toContainText("$126.00");
  await expect(price).toContainText("-10%");
  expect(await priceRequests(page)).toHaveLength(asked + 1);
  expect((await priceRequests(page)).at(-1)).toMatchObject({ quantity: 20 });
});

test("an older price answer never replaces a newer one", async ({ page }) => {
  const { price, pay, quantity } = await openBuy(page);
  const base = (await priceRequests(page)).length;
  // The answer to 2 is slow, the one to 5 is fast: 2 lands last and must be dropped.
  await slowPrice(page, 2500);
  await quantity.fill("2");
  await expect.poll(async () => (await priceRequests(page)).length).toBe(base + 1);
  await slowPrice(page, 0);
  await quantity.fill("5");
  await expect(price).toContainText("$35.00");
  await expect(pay).toHaveText("Pay $35.00");
  await expect(pay).toBeEnabled();
  // The slow answer ($14.00) has landed by the time the mock is idle.
  await waitForIdle(page);
  await expect(price).toContainText("$35.00");
  await expect(price).not.toContainText("$14.00");
  await expect(pay).toHaveText("Pay $35.00");
  expect((await priceRequests(page)).at(-1)).toMatchObject({ quantity: 5 });
});

test("Pay is disabled while the price updates", async ({ page }) => {
  const { price, pay, quantity } = await openBuy(page);
  await slowPrice(page, 1200);
  await quantity.fill("4");
  // The old price stays, with the word; Pay waits, and so does Enter.
  await expect(price).toContainText("Updating");
  await expect(price).toContainText("$7.00");
  await expect(pay).toBeDisabled();
  await quantity.press("Enter");
  await expect(page.getByRole("alertdialog")).toHaveCount(0);
  await expect(price).toContainText("$28.00");
  await expect(price).not.toContainText("Updating");
  await expect(pay).toBeEnabled();
  await expect(pay).toHaveText("Pay $28.00");
});

test("a field changed by the same Enter that submits never confirms the old price", async ({ page }) => {
  const { price, pay, quantity } = await openBuy(page);
  await quantity.fill("3");
  await expect(price).toContainText("$21.00");
  await expect(pay).toBeEnabled();
  await slowPrice(page, 800);
  // "0" is out of range, so the form still holds 3 until Enter brings the field to 1
  // and submits in the same task: the old price ($21.00) must not be quoted for 1.
  await quantity.fill("0");
  await quantity.press("Enter");
  await expect(price).toContainText("$7.00");
  await expect(page.getByRole("alertdialog")).toHaveCount(0);
  await pay.click();
  const ask = page.getByRole("alertdialog");
  await expect(ask).toContainText(/Buy 1 × .+\?/);
  await expect(ask).toContainText("$7.00");
  await expect(ask).not.toContainText("$21.00");
});

test("a purchase that fails may have charged: the account is read again, Pay waits and the dialog says to check", async ({ page }) => {
  const { dialog, pay } = await openBuy(page);
  const meReads = () => page.evaluate(() => (window as unknown as Mock).__shardxMock.calls.ps_me ?? 0);
  const before = await meReads();
  await page.evaluate(() => Object.assign(window, { __shardxMockFail: { ps_purchase: 1 }, __shardxMockSlow: { ps_me: 1200 } }));
  await pay.click();
  await page.getByRole("alertdialog").getByRole("button", { name: "Pay $7.00" }).click();
  await expect(page.locator("[data-toast]").filter({ hasText: "Couldn’t place the order" })).toBeVisible();
  await expect(dialog.getByRole("alert")).toContainText("Check your balance before you try again");
  // Pay waits until the balance has been read again, then works.
  const submit = dialog.locator("button[type='submit']");
  await expect(submit).toBeDisabled();
  await expect.poll(meReads).toBe(before + 1);
  await expect(submit).toBeEnabled();
  await expect(dialog).toBeVisible();
});

test("a promo code the shop rejects is marked with the shop's reason, the price shows without it, and Pay waits", async ({ page }) => {
  const { dialog, price, pay } = await openBuy(page);
  const promo = dialog.getByRole("textbox", { name: /Promo code/ });
  await promo.fill("INVALID");
  // The mock refuses "INVALID" with "400 Bad Request: promo code not found".
  await expect(dialog.getByText("That promo code didn’t work. Promo code not found.")).toBeVisible();
  // The form was asked again without the code, and that price is on screen.
  await expect(price).toContainText("$7.00");
  await expect(price).not.toContainText("Updating");
  await expect(price).not.toContainText("Couldn’t calculate the price");
  expect((await priceRequests(page)).slice(-2)).toMatchObject([{ promoCode: "INVALID" }, { promoCode: null }]);
  await expect(pay).toBeDisabled();
  await promo.fill("SAVE15");
  await expect(price).toContainText("-15%");
  await expect(dialog.getByText("That promo code didn’t work")).toHaveCount(0);
  await expect(pay).toBeEnabled();
});

test("a promo code the shop refuses in its own words (no promo word in them) is still the promo's", async ({ page }) => {
  const { dialog, price, pay } = await openBuy(page);
  await page.evaluate(() => Object.assign(window, { __shardxMockFailWith: { ps_calculate: "Code expired" } }));
  await dialog.getByRole("textbox", { name: /Promo code/ }).fill("SAVE15");
  await expect(dialog.getByText("That promo code didn’t work. Code expired.")).toBeVisible();
  await expect(price).toContainText("$7.00");
  await expect(pay).toBeDisabled();
});

test("a price that fails to calculate says why, and Try again asks the same form", async ({ page }) => {
  const { dialog, price, pay, quantity } = await openBuy(page);
  // Nothing about a promo in the shop's words, and no promo code in the form.
  await page.evaluate(() => Object.assign(window, { __shardxMockFailWith: { ps_calculate: "Out of stock in this location" } }));
  await quantity.fill("2");
  await expect(price).toContainText("Couldn’t calculate the price. Out of stock in this location.");
  await expect(price).not.toContainText("Updating");
  await expect(pay).toBeDisabled();
  await expect(dialog.getByText("That promo code didn’t work")).toHaveCount(0);
  await price.getByRole("button", { name: "Try again" }).click();
  await expect(price).toContainText("$14.00");
  await expect(pay).toBeEnabled();
  // With a promo code too: if the form fails without the code as well, it is the form, not the code.
  await page.evaluate(() => Object.assign(window, { __shardxMockFail: { ps_calculate: 2 } }));
  await dialog.getByRole("textbox", { name: /Promo code/ }).fill("SAVE15");
  await expect(price).toContainText("Couldn’t calculate the price. Mock failure: ps_calculate.");
  await expect(dialog.getByText("That promo code didn’t work")).toHaveCount(0);
  expect((await priceRequests(page)).slice(-2)).toMatchObject([{ promoCode: "SAVE15" }, { promoCode: null }]);
  await price.getByRole("button", { name: "Try again" }).click();
  await expect(price).toContainText("-15%");
  await expect(pay).toBeEnabled();
});

test("a quantity the field does not accept shows no price and holds Pay", async ({ page }) => {
  const { dialog, price, pay, quantity } = await openBuy(page);
  await quantity.fill("3");
  await expect(price).toContainText("$21.00");
  await quantity.fill("abc");
  // The price of 3 is not the price of what the field shows.
  await expect(price).not.toContainText("$21.00");
  await expect(pay).toBeDisabled();
  await quantity.press("Enter");
  await expect(dialog.getByText("Enter a number.")).toBeVisible();
  await expect(page.getByRole("alertdialog")).toHaveCount(0);
  await expect(pay).toBeDisabled();
  await quantity.fill("2");
  await expect(price).toContainText("$14.00");
  await expect(pay).toBeEnabled();
});

test("the price slot never says Updating when the shop failed to load or has nothing to sell", async ({ page }) => {
  await openApp(page, { section: "proxyshard" });
  await page.evaluate(() => Object.assign(window, { __shardxMockFailWith: { ps_products: "Shop is down" } }));
  await page.getByRole("button", { name: "Buy proxies" }).click();
  const dialog = page.getByRole("dialog", { name: "Buy proxies" });
  await expect(dialog.getByText("Shop is down")).toBeVisible();
  const price = dialog.locator("[data-ps-buy-price]");
  await expect(price).not.toContainText("Updating");
  await expect(price).not.toHaveAttribute("aria-busy", "true");
  await expect(dialog.locator("button[type='submit']")).toBeDisabled();
  // Retry loads the shop and prices the form.
  await dialog.getByRole("button", { name: "Retry" }).click();
  await expect(price).toContainText("$7.00");
});

test("a plan that ends in 2125 prints no end date on the Problems card of Profiles", async ({ page }) => {
  await page.addInitScript((gb) => {
    Object.assign(window, { __shardxMockTraffic: { premium: { data: 10 * gb, data_remain: 1.2 * gb, data_spent: 8.8 * gb, proxy_password: "" } } });
  }, 1024 ** 3);
  await openApp(page);
  await page.evaluate(() => {
    const m = (window as unknown as { __shardxMock: { state: { psOrders: { product_name: string; expires_at: string }[] }; save: () => void } }).__shardxMock;
    for (const o of m.state.psOrders) if (o.product_name === "Residential Premium") o.expires_at = "2125-12-17T00:00:00.000Z";
    m.save();
  });
  await reloadApp(page);
  const balance = page.locator("[data-overview] [data-problem-card='proxyBalance']");
  await expect(balance.getByText("1.2 GB left")).toBeVisible();
  await expect(balance.locator("[data-card-usage]")).toHaveText(/^8\.8 of 10 GB used$/);
  await expect(balance).not.toContainText("2125");
});

test("residential plan shows Lifetime", async ({ page }) => {
  await openApp(page, { section: "proxyshard" });
  const card = page.locator("[data-usage-card='ends']");
  await expect(card).toContainText("Lifetime");
  // No day count, no end date, no renew line (the mock's Premium order ends in 19 days).
  await expect(card).not.toContainText(/\bdays?\b|Ends|Ended|Started|renew/i);
  await expect(card.getByRole("progressbar")).toHaveCount(0);
});

test("a plan is lifetime when it is residential traffic or ends more than 50 years away", async ({ page }) => {
  await openApp(page, { section: "proxyshard" });
  await appModule(page, "/src/entities/proxyshard/lib/planTier.ts", "__plan");
  const answers = await page.evaluate(() => {
    const m = (window as unknown as { __plan: { isLifetime: (p: object, now?: number) => boolean } }).__plan;
    const now = Date.UTC(2026, 8, 1);
    const day = 86_400_000;
    const plan = (tier: string, endsAt: number | null) => ({ tier, orderId: 1, productName: "x", total: null, remain: null, spent: null, endsAt, startedAt: null });
    return [
      m.isLifetime(plan("premium", now + 19 * day), now),
      m.isLifetime(plan("standart", now + 36_232 * day), now),
      m.isLifetime(plan("unmetered", now + 30 * day), now),
      m.isLifetime(plan("unmetered", now + 49 * 365 * day), now),
      m.isLifetime(plan("unmetered", now + 51 * 365 * day), now),
      m.isLifetime(plan("unmetered", null), now),
    ];
  });
  expect(answers).toEqual([true, true, false, false, true, false]);
});

test("opening ProxyShard by mouse shows no focus ring on the title", async ({ page }) => {
  await openApp(page, { section: "profiles" });
  // A click that opens the page and leaves nothing focused (a button takes no
  // focus on a click on macOS): the page hands the title the keyboard, with no ring.
  await page.locator("aside [data-nav='proxyshard']").dispatchEvent("click", { detail: 1 });
  const title = page.locator("main h1[data-page-title]");
  await expect(title).toHaveText("ShardX");
  await expect(title).toBeFocused();
  expect(await title.evaluate((el) => getComputedStyle(el).outlineStyle)).toBe("none");
});

test("opening ProxyShard from the keyboard keeps the title's ring", async ({ page }) => {
  await openApp(page, { section: "profiles" });
  await page.locator("aside [data-nav='proxyshard']").focus();
  await page.keyboard.press("Enter");
  const title = page.locator("main h1[data-page-title]");
  await expect(title).toHaveText("ShardX");
  await expect(title).toBeFocused();
  expect(await title.evaluate((el) => getComputedStyle(el).outlineStyle)).toBe("solid");
});
