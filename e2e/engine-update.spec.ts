import { expect, test, type Page } from "@playwright/test";
import { appModule, openApp, waitForIdle } from "./support/app";

// An engine update that stops short keeps the engine there is (#122): the app
// opens on it with a banner that says why, and Try again finishes it; a busy
// one finishes once the running profiles close. Only a first install, with no
// engine to open, keeps the full-screen setup.
type Stage = "download" | "space" | "extract" | "verify" | "busy" | "swap";
type Mock = { __shardxMock: { calls: Record<string, number>; emit(e: string, p: unknown): void } };
const knobs = (page: Page, k: Record<string, unknown>) => page.addInitScript((k) => { Object.assign(window, k); }, k);
const setKnobs = (page: Page, k: Record<string, unknown>) => page.evaluate((k) => { Object.assign(window, k); }, k);
const installs = (page: Page) => page.evaluate(() => (window as unknown as Mock).__shardxMock.calls.runtime_install ?? 0);
const banner = (page: Page) => page.locator("[data-engine-update]");
/** The banner's words (its Copy details button has a status of its own). */
const said = (page: Page) => banner(page).locator("p[role='status']");
const profilesPage = (page: Page) => page.locator("main[data-section='profiles']");
const toast = (page: Page, text: string) => page.locator("[data-toast]").filter({ hasText: text });

/** The etag the mock's update offers. */
const REMOTE = "a91c3e07";
const UPDATE = { __shardxMockEngine: "update", __shardxMockEngineStepMs: 20 };
const TITLE = "The browser engine update didn’t finish.";
const KEPT = "You’re still on the previous version; profiles work as before.";
const STAGED = "The update is downloaded.";
const SWAP = "The browser engine’s folder is in use. Close the running profiles and any program that has that folder open, then try again.";
/** The stop an earlier start recorded (install-error.json). */
const recorded = (stage: Stage, target = REMOTE, running_browsers = 0) => ({
  stage, message: `stopped at ${stage}`, running_browsers, starting: 0, engine_kept: true, target, at_ms: 0,
});
const open = async (page: Page) => {
  await openApp(page, { idle: false });
  await expect(profilesPage(page)).toBeVisible({ timeout: 10_000 });
};
/** The mock's seed runs two profiles: Amazon US seller and eBay DE buyer. */
const stopButton = (page: Page) => banner(page).getByRole("button", { name: "Stop 2 profiles and update…" });

// The fact first, then what to do; the space numbers come from the backend's message.
const STOPS: { stage: Stage; busy?: number; says: string }[] = [
  { stage: "download", says: "Couldn’t download it. Check your internet connection." },
  { stage: "space", says: "Not enough disk space: it needs about 1.4 GB, and 312 MB is free." },
  { stage: "extract", says: "Couldn’t unpack it. An antivirus may be in the way." },
  { stage: "verify", says: "Couldn’t unpack it. An antivirus may be in the way." },
  { stage: "busy", busy: 2, says: `${STAGED} It finishes once the 2 running profiles close.` },
  { stage: "busy", busy: 1, says: `${STAGED} It finishes once the running profile closes.` },
  { stage: "busy", busy: 0, says: `${STAGED} Programs from the browser engine’s folder are still running, though no profile is. End them in the system’s task manager or restart this computer, then try again.` },
  { stage: "swap", says: `${STAGED} ${SWAP}` },
];

for (const { stage, busy, says } of STOPS) {
  test(`an update stopped at ${stage}${busy === undefined ? "" : ` with ${busy} browsers`} opens the app with a banner that says why`, async ({ page }) => {
    await knobs(page, { ...UPDATE, __shardxMockEngineFail: stage, __shardxMockEngineBusy: busy });
    await open(page);
    await expect(banner(page)).toHaveAttribute("data-engine-update", stage);
    await expect(said(page)).toHaveText(`${TITLE} ${says} ${KEPT}`);
    await expect(page.getByRole("heading", { name: "Couldn’t download the browser engine" })).toHaveCount(0);
  });
}

// Windows and Linux draw the caption buttons over the banner's row (#142).
test.describe("on Windows", () => {
  test.use({ userAgent: "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/140.0 Safari/537.36" });

  test("the banner ends before the caption buttons, Try again clear of them", async ({ page }) => {
    await knobs(page, { ...UPDATE, __shardxMockEngineFail: "download" });
    await open(page);
    const captions = (await page.locator("[data-captions]").boundingBox())!;
    const box = (await banner(page).boundingBox())!;
    expect(box.x + box.width).toBeLessThanOrEqual(captions.x);
    await banner(page).getByRole("button", { name: "Try again" }).click();
  });
});

test("a busy stop counts the profiles in Russian with the right word form", async ({ page }) => {
  await page.addInitScript(() => localStorage.setItem("shardx.lang", "ru"));
  await knobs(page, { ...UPDATE, __shardxMockEngineFail: "busy", __shardxMockEngineBusy: 5 });
  await open(page);
  await expect(banner(page)).toContainText("Обновление движка браузера не завершилось. Обновление уже скачано. Оно завершится после закрытия 5 запущенных профилей.");
});

for (const stage of ["space", "verify"] as const) {
  test(`a ${stage} stop for the update still offered is not tried again at start`, async ({ page }) => {
    await knobs(page, { ...UPDATE, __shardxMockEngineError: recorded(stage, REMOTE, 2) });
    await open(page);
    await expect(banner(page)).toHaveAttribute("data-engine-update", stage);
    await waitForIdle(page);
    expect(await installs(page)).toBe(0);
  });
}

test("a busy stop at start is tried again at once, and says the profiles running now", async ({ page }) => {
  // Recorded with 5 browsers last time; the seed runs 2 now.
  await knobs(page, { ...UPDATE, __shardxMockEngineFail: "busy", __shardxMockEngineStaged: true, __shardxMockEngineError: recorded("busy", REMOTE, 5) });
  await open(page);
  await expect(said(page)).toHaveText(`${TITLE} ${STAGED} It finishes once the 2 running profiles close. ${KEPT}`);
  expect(await installs(page)).toBe(1);
  // Tried without being asked: no failure toast.
  await expect(page.locator("[data-toast]")).toHaveCount(0);
});

test("a stop for an older update is tried again at start, and the banner goes once it lands", async ({ page }) => {
  await knobs(page, { ...UPDATE, __shardxMockEngineError: recorded("busy", "e2f22d25", 2) });
  await open(page);
  await waitForIdle(page);
  expect(await installs(page)).toBe(1);
  await expect(banner(page)).toHaveCount(0);
});

test("a download stop at start opens the app at once, the download again in the banner", async ({ page }) => {
  await knobs(page, { ...UPDATE, __shardxMockEngineStepMs: 600, __shardxMockEngineError: recorded("download") });
  await open(page);
  // No full-screen update: the app is in, and the banner shows the download.
  await expect(page.getByRole("heading", { name: "Updating the browser engine" })).toHaveCount(0);
  await expect(said(page)).toHaveText(/^Updating the browser engine\. \d+\.\d of 19\.1\sMB downloaded\.$/);
  await expect(banner(page)).toHaveCount(0, { timeout: 10_000 });
  await expect(toast(page, "Browser engine updated")).toBeVisible();
  expect(await installs(page)).toBe(1);
});

test("Try again downloads the update with its progress in the banner", async ({ page }) => {
  await knobs(page, { ...UPDATE, __shardxMockEngineStepMs: 600, __shardxMockEngineError: recorded("space") });
  await open(page);
  await expect(banner(page)).toContainText("Not enough disk space.");
  // The kit hides a busy button's label under its spinner: found as the banner's last button.
  const retry = banner(page).getByRole("button").last();
  await expect(retry).toHaveText("Try again");
  await retry.click();
  await expect(retry).toHaveAttribute("aria-busy", "true");
  await expect(said(page)).toHaveText(/^Updating the browser engine\. \d+\.\d of 19\.1\sMB downloaded\.$/);
  await expect(banner(page)).toHaveCount(0, { timeout: 10_000 });
  await expect(toast(page, "Browser engine updated")).toBeVisible();
});

test("busy: Stop 2 profiles and update asks with their names, and the update finishes once they close", async ({ page }) => {
  await knobs(page, { ...UPDATE, __shardxMockEngineFail: "busy" });
  await open(page);
  await expect(banner(page)).toContainText(`${STAGED} It finishes once the 2 running profiles close.`);
  // Try again would meet the same profiles: stopping them is the way on.
  await expect(banner(page).getByRole("button", { name: "Try again" })).toHaveCount(0);
  await stopButton(page).click();
  const ask = page.getByRole("dialog", { name: "Stop profiles to finish the browser engine update?" });
  await expect(ask.locator("[data-confirm-items] li")).toHaveText(["Amazon US seller", "eBay DE buyer"]);
  await ask.getByRole("button", { name: "Stop and update" }).click();
  await expect(banner(page)).toHaveCount(0, { timeout: 10_000 });
  await expect(toast(page, "Browser engine updated")).toBeVisible();
  expect(await installs(page)).toBe(2);
});

test("busy: a profile left from before, or one starting, is said in words", async ({ page }) => {
  await knobs(page, { ...UPDATE, __shardxMockEngineFail: "busy", __shardxMockEngineBusy: 1, __shardxMockEngineBusyKind: "untracked" });
  await open(page);
  await expect(banner(page)).toContainText("It finishes once the running profile closes. If no profile shows as running, one was left running when ShardBrowser last closed: end it in the system’s task manager or restart this computer.");
  await setKnobs(page, { __shardxMockEngineBusyKind: "starting" });
  // Read from `starting`, not from the message's words.
  const failed = (starting: number, message: string) => page.evaluate((e) => (window as unknown as Mock).__shardxMock.emit("runtime:failed", e), {
    stage: "busy", message, running_browsers: 0, starting, engine_kept: true, target: "a91c3e07", at_ms: 0, update_staged: true,
  });
  await failed(1, "stopped at busy");
  await expect(banner(page)).toContainText("A profile is starting. Once it has started, close the running profiles and the update finishes.");
  await failed(0, "The engine update is ready, but a profile is starting. Wait for it, close the profiles that are running, and try again.");
  await expect(banner(page)).toContainText("Programs from the browser engine’s folder are still running");
  await expect(banner(page)).not.toContainText("A profile is starting");
});

test("whether the update is downloaded comes with the stop itself", async ({ page }) => {
  await knobs(page, { ...UPDATE, __shardxMockEngineFail: "busy", __shardxMockEngineBusy: 2 });
  await open(page);
  await expect(said(page)).toContainText(STAGED);
  await waitForIdle(page);
  // A busy stop whose update is not unpacked here (runtime:failed's update_staged) says no such thing.
  await page.evaluate((e) => (window as unknown as Mock).__shardxMock.emit("runtime:failed", e), {
    ...recorded("busy", REMOTE, 2), update_staged: false,
  });
  await expect(said(page)).toHaveText(`${TITLE} It finishes once the 2 running profiles close. ${KEPT}`);
});

test("Try again that meets the same stop says so in a toast; a new stop shows in the banner", async ({ page }) => {
  await knobs(page, { ...UPDATE, __shardxMockEngineFail: "swap" });
  await open(page);
  await expect(banner(page)).toContainText(SWAP);
  await banner(page).getByRole("button", { name: "Try again" }).click();
  await expect(toast(page, "Couldn’t update the browser engine")).toContainText(SWAP);
  await setKnobs(page, { __shardxMockEngineFail: "busy", __shardxMockEngineBusy: 1 });
  await banner(page).getByRole("button", { name: "Try again" }).click();
  await expect(banner(page)).toHaveAttribute("data-engine-update", "busy");
  await expect(banner(page)).toContainText("It finishes once the running profile closes.");
  expect(await installs(page)).toBe(3);
});

test("a stop whose status lost it (the manifest unread) still shows, and Try again claims no update", async ({ page }) => {
  await knobs(page, { ...UPDATE, __shardxMockEngineFail: "download", __shardxMockEngineManifestLost: true });
  await open(page);
  await expect(banner(page)).toHaveAttribute("data-engine-update", "download");
  await banner(page).getByRole("button", { name: "Try again" }).click();
  await expect(toast(page, "Couldn’t update the browser engine")).toContainText("Couldn’t download it. Check your internet connection.");
  await expect(toast(page, "Browser engine updated")).toHaveCount(0);
  await expect(banner(page)).toHaveAttribute("data-engine-update", "download");
});

test("Try again with the manifest out of reach: nothing tried, said so, and no update claimed", async ({ page }) => {
  await knobs(page, { ...UPDATE, __shardxMockEngineError: recorded("space") });
  await open(page);
  await expect(banner(page)).toHaveAttribute("data-engine-update", "space");
  // runtime.rs still sends runtime:done when it could not read the manifest.
  await setKnobs(page, { __shardxMockEngineManifestDown: true });
  await banner(page).getByRole("button", { name: "Try again" }).click();
  await expect(toast(page, "Couldn’t update the browser engine")).toContainText("Couldn’t download it. Check your internet connection.");
  await expect(toast(page, "Browser engine updated")).toHaveCount(0);
  await expect(banner(page)).toHaveAttribute("data-engine-update", "space");
});

test("Copy details puts the backend's own words on the clipboard", async ({ page }) => {
  await knobs(page, { ...UPDATE, __shardxMockEngineFail: "verify" });
  await open(page);
  await banner(page).getByRole("button", { name: "Copy details" }).click();
  await expect.poll(() => page.evaluate(() => (window as { __shardxMockClipboard?: string }).__shardxMockClipboard))
    .toMatch(/^The unpacked engine update is missing ShardX\.app\/Contents\/MacOS\/ShardX/);
});

test("a language change rebuilds the app without starting the update again", async ({ page }) => {
  await knobs(page, { ...UPDATE, __shardxMockEngineFail: "download" });
  await open(page);
  await expect(banner(page)).toBeVisible();
  await appModule(page, "/src/shared/i18n/index.ts", "__i18n");
  await page.evaluate(() => (window as unknown as { __i18n: { useLang: { getState(): { setLang(v: string): void } } } }).__i18n.useLang.getState().setLang("ru"));
  await expect(banner(page)).toContainText("Обновление движка браузера не завершилось. Не удалось скачать.");
  await waitForIdle(page);
  expect(await installs(page)).toBe(1);
});

test("a stop told while the app is open shows the banner", async ({ page }) => {
  await openApp(page);
  await expect(banner(page)).toHaveCount(0);
  await page.evaluate((e) => (window as unknown as Mock).__shardxMock.emit("runtime:failed", e), { ...recorded("swap"), update_staged: true });
  await expect(banner(page)).toContainText(`${TITLE} ${STAGED} ${SWAP} ${KEPT}`);
});

test("a first install that stops keeps the setup screen, its title by where it stopped", async ({ page }) => {
  await knobs(page, { __shardxMockEngine: "missing", __shardxMockEngineStepMs: 20, __shardxMockEngineFail: "space" });
  await openApp(page, { idle: false });
  await expect(page.getByRole("heading", { name: "Not enough disk space for the browser engine" })).toBeVisible();
  await expect(page.getByText("Not enough disk space: it needs about 1.4 GB, and 312 MB is free.", { exact: true })).toBeVisible();
  // The backend's words speak of an update and of an engine there is: kept for Copy details only.
  await expect(page.getByText(/Not enough free disk space to update/)).toHaveCount(0);
  await expect(page.getByRole("button", { name: "Copy details" })).toBeVisible();
  await expect(banner(page)).toHaveCount(0);
  await setKnobs(page, { __shardxMockEngineFail: "verify" });
  await page.getByRole("button", { name: "Try again" }).click();
  await expect(page.getByRole("heading", { name: "Couldn’t unpack the browser engine" })).toBeVisible();
  await expect(page.getByText("Couldn’t unpack it. An antivirus may be in the way.", { exact: true })).toBeVisible();
  await setKnobs(page, { __shardxMockEngineFail: null });
  await page.getByRole("button", { name: "Try again" }).click();
  await expect(profilesPage(page)).toBeVisible({ timeout: 10_000 });
  await expect(banner(page)).toHaveCount(0);
});

test("a first install refused for the launcher's version says so in words", async ({ page }) => {
  await openApp(page);
  await appModule(page, "/src/shared/lib/errors.ts", "__e");
  const said = (m: string) => page.evaluate((m) => (window as unknown as { __e: { explainError(e: unknown): string } }).__e.explainError(m), m);
  expect(await said("This engine build needs ShardBrowser Launcher 2.2.0 or newer — you are on 2.0.3. Update the launcher first."))
    .toBe("This browser engine needs ShardBrowser 2.2.0 or newer, and this is 2.0.3. Update ShardBrowser first.");
  expect(await said("Host platform has no published ShardBrowser engine archive")).toBe("There’s no browser engine for this computer’s system yet.");
});

test("a start refused while the engine is swapped says so in words", async ({ page }) => {
  await openApp(page);
  await appModule(page, "/src/shared/lib/errors.ts", "__e");
  const UPDATING = "The ShardBrowser engine is being updated. Try again in a moment.";
  expect(await page.evaluate((m) => (window as unknown as { __e: { explainError(e: unknown): string } }).__e.explainError(m), UPDATING))
    .toBe("The browser engine is being updated. Try again in a moment.");
  await setKnobs(page, { __shardxMockFailWith: { launch: UPDATING } });
  const r = page.locator("[role='row'][data-profile-id]", { hasText: "Sandbox" });
  await r.hover();
  await r.getByRole("button", { name: "Start" }).click();
  await expect(toast(page, "Couldn’t start “Sandbox”")).toContainText("The browser engine is being updated. Try again in a moment.");
});
