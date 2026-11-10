import { expect, test, type Page } from "@playwright/test";
import { openApp, reloadApp } from "./support/app";

// The trial offer, at every start of the app on whatever page it opens, while
// no ProxyShard key is connected. Everything in the dialog comes from
// the site API's answer — the amount, the link, and whether there is an offer
// at all — so the test states the answer and reads the dialog built from it.
const OFFER = { show: true, url: "https://dashboard.shardx.example/trial", trial_gb: 0.1 };

type Mock = { state: { psKey: string }; save: () => void };

const dialog = (page: Page) => page.getByRole("dialog");
const proxies = (page: Page) => page.locator("aside [data-nav='proxies']");
const profiles = (page: Page) => page.locator("aside [data-nav='profiles']");

/** Takes the ProxyShard key away and starts the app again on it. The mock
 *  writes its state 200 ms after `save()`, so the reload waits for the write
 *  to land: without that, a busy machine reloads on the old state, the app
 *  still has a key, and a test about an operator with none passes or fails
 *  for the wrong reason. */
async function forgetKey(page: Page) {
  await page.evaluate(() => {
    const m = (window as unknown as { __shardxMock: Mock }).__shardxMock;
    m.state.psKey = "";
    m.save();
  });
  await expect
    .poll(() => page.evaluate(() => JSON.parse(localStorage.getItem("shardx-mock-state") ?? "{}").psKey))
    .toBe("");
  await reloadApp(page);
}

/** An operator with no ProxyShard account, and an offer waiting for them. */
async function noKey(page: Page) {
  await page.addInitScript((o) => {
    (window as { __shardxMockTrialOffer?: unknown }).__shardxMockTrialOffer = o;
  }, OFFER);
  await openApp(page, { section: "profiles" });
  await forgetKey(page);
}

test("the offer names the traffic the API answered with, and the keyboard is on the confirm", async ({ page }) => {
  await noKey(page);
  // On the page the app opened on: no visit to Proxies needed.
  await expect(page.getByRole("heading", { name: "All profiles" })).toBeVisible();
  // 0.1 GB from the API is spoken of as 100 MB (nothing in the app decides the amount).
  await expect(dialog(page).getByRole("heading", { name: /100 MB/ })).toBeVisible();
  // A dialog with no field puts the keyboard on its confirm, which is also
  // the button Enter presses.
  await expect(dialog(page).getByRole("button", { name: "Get the trial" })).toBeFocused();
});

test("asked once a run: moving between pages does not bring it back", async ({ page }) => {
  await noKey(page);
  await expect(dialog(page)).toBeVisible();
  await dialog(page).getByRole("button", { name: "Not now" }).click();
  await expect(dialog(page)).toHaveCount(0);
  await proxies(page).click();
  await expect(page.getByRole("heading", { name: "Proxies" })).toBeVisible();
  await profiles(page).click();
  await expect(page.getByRole("heading", { name: "All profiles" })).toBeVisible();
  await expect(dialog(page)).toHaveCount(0);
});

test("every start of the app asks again, on whatever page it opens", async ({ page }) => {
  await noKey(page);
  await dialog(page).getByRole("button", { name: "Not now" }).click();
  await expect(dialog(page)).toHaveCount(0);
  await proxies(page).click();
  await expect(page.getByRole("heading", { name: "Proxies" })).toBeVisible();
  // A reload is a new run of the launcher: it reopens on the page it was
  // left on, and the question comes back until a key is saved.
  await reloadApp(page);
  await expect(page.getByRole("heading", { name: "Proxies" })).toBeVisible();
  await expect(dialog(page)).toBeVisible();
});

test("it waits for What's new and comes after it", async ({ page }) => {
  // An update opens What's new at the start.
  await page.addInitScript(() => localStorage.setItem("shardx-patchlog-seen-version", "0.0.1"));
  await noKey(page);
  const news = page.getByRole("dialog", { name: /What’s new/ });
  await expect(news).toBeVisible();
  await expect(page.getByRole("dialog", { name: /100 MB/ })).toHaveCount(0);
  await page.keyboard.press("Escape");
  await expect(news).toHaveCount(0);
  await expect(page.getByRole("dialog", { name: /100 MB/ })).toBeVisible();
});

test("an operator with a key is never asked", async ({ page }) => {
  await page.addInitScript((o) => {
    (window as { __shardxMockTrialOffer?: unknown }).__shardxMockTrialOffer = o;
  }, OFFER);
  // The seeded account has a key, so the offer is not for them.
  await openApp(page, { section: "profiles" });
  await expect(page.getByRole("heading", { name: "All profiles" })).toBeVisible();
  await proxies(page).click();
  await expect(page.getByRole("heading", { name: "Proxies" })).toBeVisible();
  await expect(dialog(page)).toHaveCount(0);
});

test("no offer going: nothing is shown and nothing is said", async ({ page }) => {
  await openApp(page, { section: "profiles" });
  await forgetKey(page);
  await expect(page.getByRole("heading", { name: "All profiles" })).toBeVisible();
  await proxies(page).click();
  await expect(page.getByRole("heading", { name: "Proxies" })).toBeVisible();
  await expect(dialog(page)).toHaveCount(0);
});
