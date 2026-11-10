import { expect, test, type Page } from "@playwright/test";
import { appModule, openApp } from "./support/app";

// Audit Phase G3: no raw backend string reaches a toast as its title
// (overview section 2). Tasks 8 to 10 add a test per area.
export const toasts = (page: Page) => page.locator("[data-toast]");
export const failNext = (page: Page, cmd: string, n = 1) =>
  page.evaluate(([c, k]) => { (window as unknown as { __shardxMockFail: Record<string, number> }).__shardxMockFail = { [c as string]: k as number }; }, [cmd, n]);

type Errs = { explainError(e: unknown): string | undefined };
const explainAll = (page: Page) =>
  page.evaluate(() => {
    const { explainError } = (window as unknown as { __e: Errs }).__e;
    return [
      explainError(new Error("profile 7f3a2c1e-0000 is already running")),
      explainError("error sending request for url (https://user-api.shardx.com/me): dns error"),
      explainError("expected value at line 1 column 1"),
      explainError({ code: "proxy_missing", message: "proxy p-1 not found" }),
      explainError({ code: "something_new", message: "the raw words" }),
      explainError("Mock failure: launch"),
      explainError("Error: boom"),
      explainError({}),
      explainError(undefined),
      explainError(""),
    ];
  });

test("explainError: known failures in words, the rest as the backend wrote it", async ({ page }) => {
  await openApp(page);
  await appModule(page, "/src/shared/lib/errors.ts", "__e");
  expect(await explainAll(page)).toEqual([
    "It’s already running. Use its open window, or stop it first.",
    "Couldn’t connect. Check your internet connection.",
    "The text isn’t valid JSON.",
    "Its proxy was deleted. Assign another proxy first.",
    "the raw words",
    "Mock failure: launch",
    "boom",
    undefined,
    undefined,
    undefined,
  ]);
});

test("explainError speaks Chinese in the Chinese UI", async ({ page }) => {
  await page.addInitScript(() => localStorage.setItem("shardx.lang", "zh"));
  await openApp(page);
  await appModule(page, "/src/shared/lib/errors.ts", "__e");
  const out = await explainAll(page);
  expect(out[0]).toBe("它已经在运行。请使用已打开的窗口，或先停止它。");
  expect(out[5]).toBe("Mock failure: launch");
});

test("a known launcher warning is a translated warning that closes after 8 s", async ({ page }) => {
  await openApp(page);
  await page.evaluate(() => (window as unknown as { __shardxMock: { emit(e: string, p: unknown): void } }).__shardxMock.emit(
    "launcher-warning",
    "Could not read the proxy's location. Rather than hand a page this computer's timezone, the profile starts on UTC — which few people really keep. Test the proxy again, or set the timezone on the profile yourself.",
  ));
  const toast = toasts(page).filter({ hasText: "Started on UTC: the proxy’s location is unknown" });
  await expect(toast).toBeVisible();
  await expect(toast).toContainText("Test the proxy again, or set the profile’s time zone yourself.");
  // Screen readers hear the word the icon shows (G7a review, I1); the visible title stays the message.
  await expect(page.locator("[data-live='polite']")).toHaveText("Warning: Started on UTC: the proxy’s location is unknown. Test the proxy again, or set the profile’s time zone yourself.");
  await expect(toast).not.toContainText("Warning");
  await page.clock.runFor(6000);
  await expect(toast).toBeVisible();
  await page.clock.runFor(3000);
  await expect(toast).toHaveCount(0);
});

test("an unknown launcher warning keeps the backend's words under a translated title", async ({ page }) => {
  await openApp(page);
  await page.evaluate(() => (window as unknown as { __shardxMock: { emit(e: string, p: unknown): void } }).__shardxMock.emit("launcher-warning", "Something new happened."));
  await expect(toasts(page).filter({ hasText: "A profile started with a warning" })).toContainText("Something new happened.");
});

// G8: every Profiles failure has a translated title naming the action and
// the profile; the backend's words are the detail under it.
type ProfileStore = {
  getState(): {
    profiles: { id: string; name: string }[];
    reload(): Promise<void>;
    exportCookies(p: { id: string; name: string }): Promise<void>;
  };
};
const profiles = async (page: Page) => {
  await appModule(page, "/src/entities/profile/index.ts", "__p");
  return {
    reload: () => page.evaluate(() => (window as unknown as { __p: { useProfile: ProfileStore } }).__p.useProfile.getState().reload()),
    exportCookies: (name: string) => page.evaluate((n) => {
      const s = (window as unknown as { __p: { useProfile: ProfileStore } }).__p.useProfile.getState();
      return s.exportCookies(s.profiles.find((p) => p.name === n)!);
    }, name),
  };
};

test("Profiles: a first load that fails is shown in place, not in a toast", async ({ page }) => {
  await page.addInitScript(() => { (window as unknown as { __shardxMockFail: Record<string, number> }).__shardxMockFail = { profile_list: 1 }; });
  await openApp(page);
  await expect(page.getByRole("button", { name: /Retry|Try again/ })).toBeVisible();
  await expect(toasts(page)).toHaveCount(0);
});

test("Profiles: a background refresh that fails says so", async ({ page }) => {
  await openApp(page);
  const p = await profiles(page);
  await failNext(page, "profile_list");
  await p.reload();
  const toast = toasts(page).filter({ hasText: "Couldn’t refresh the profile list" });
  await expect(toast).toBeVisible();
  await expect(toast).toContainText("Mock failure: profile_list");
});

test("Profiles: a failed cookie export names the profile", async ({ page }) => {
  await openApp(page);
  const p = await profiles(page);
  await failNext(page, "cookies_export_to_file");
  await p.exportCookies("Sandbox");
  await expect(toasts(page).filter({ hasText: "Couldn’t export cookies from “Sandbox”" })).toContainText("Mock failure: cookies_export_to_file");
});

test("Profiles: copying the ID can fail, and says so", async ({ page }) => {
  await openApp(page);
  await failNext(page, "clipboard_write");
  await page.locator("[role='row'][data-profile-id]", { hasText: "Sandbox" }).click({ button: "right" });
  await page.getByRole("menuitem", { name: /Copy (profile )?ID/ }).click();
  await expect(toasts(page).filter({ hasText: "Couldn’t copy the profile ID" })).toBeVisible();
});

test("Profiles: an empty clipboard says what to copy first", async ({ page }) => {
  await openApp(page);
  await page.evaluate(() => { (window as unknown as { __shardxMockClipboard: string }).__shardxMockClipboard = ""; });
  await page.keyboard.press("ControlOrMeta+k");
  // The E1 registry's name for it: "Import profiles from clipboard".
  await page.getByRole("combobox").fill("import profiles from clipboard");
  await page.keyboard.press("Enter");
  await expect(toasts(page).filter({ hasText: "Nothing to import. Copy exported profile JSON first, then try again." })).toBeVisible();
});

test("Bookmarks: a first load that fails is shown in place, not in a toast", async ({ page }) => {
  await page.addInitScript(() => { (window as unknown as { __shardxMockFail: Record<string, number> }).__shardxMockFail = { bookmark_list: 1 }; });
  await openApp(page, { section: "bookmarks" });
  await expect(page.getByRole("button", { name: /Retry|Try again/ })).toBeVisible();
  await expect(toasts(page)).toHaveCount(0);
});

test("Bookmarks: a failed save names the bookmark", async ({ page }) => {
  await openApp(page, { section: "bookmarks" });
  await page.getByRole("button", { name: /New bookmark|Add bookmark/ }).first().click();
  const dialog = page.getByRole("dialog");
  await dialog.getByRole("textbox", { name: "Title" }).fill("Docs");
  await dialog.getByRole("textbox", { name: "URL" }).fill("https://example.com/docs");
  await failNext(page, "bookmark_save");
  await page.keyboard.press("Enter");
  await expect(toasts(page).filter({ hasText: "Couldn’t save bookmark “Docs”" })).toContainText("Mock failure: bookmark_save");
});

test("Proxies: a failed rename names the proxy", async ({ page }) => {
  await openApp(page, { section: "proxies" });
  await appModule(page, "/src/entities/proxy/index.ts", "__px");
  await failNext(page, "proxy_save");
  await page.evaluate(() => {
    type S = { getState(): { proxies: { id: string; name: string }[]; renameProxy(id: string, n: string): Promise<void> } };
    const s = (window as unknown as { __px: { useProxy: S } }).__px.useProxy.getState();
    return s.renameProxy(s.proxies.find((p) => p.name === "DC Tokyo")!.id, "Tokyo 2");
  });
  await expect(toasts(page).filter({ hasText: "Couldn’t rename proxy “DC Tokyo”" })).toBeVisible();
});

test("Trash: restoring something already gone says so in words (PC-071)", async ({ page }) => {
  await openApp(page, { section: "trash" });
  await appModule(page, "/src/entities/trash/index.ts", "__tr");
  await page.evaluate(() => { (window as unknown as { __shardxMockFailWith: Record<string, string> }).__shardxMockFailWith = { trash_restore: "No such file or directory (os error 2)" }; });
  const name = await page.evaluate(() => {
    type S = { getState(): { items: { id: string; name: string }[]; restore(e: unknown): Promise<void> } };
    const s = (window as unknown as { __tr: { useTrash: S } }).__tr.useTrash.getState();
    void s.restore(s.items[0]);
    return s.items[0].name;
  });
  const toast = toasts(page).filter({ hasText: `Couldn’t restore “${name}”` });
  await expect(toast).toContainText("It no longer exists.");
  await expect(toast).not.toContainText("os error");
});

test("Automation: a failed create names the project", async ({ page }) => {
  await openApp(page, { section: "automation" });
  await appModule(page, "/src/entities/automation/index.ts", "__au");
  await failNext(page, "automation_create");
  await page.evaluate(() => {
    type S = { getState(): { create(name: string): Promise<void> } };
    return (window as unknown as { __au: { useAutomation: S } }).__au.useAutomation.getState().create("Warm-up");
  });
  await expect(toasts(page).filter({ hasText: "Couldn’t create project “Warm-up”" })).toContainText("Mock failure: automation_create");
});
