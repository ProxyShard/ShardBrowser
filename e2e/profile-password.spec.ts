import { expect, test, type Page } from "@playwright/test";
import { openApp, row } from "./support/app";

// Password-protected profiles (#127): a password per profile, asked at every
// start and to open its editor; what it seals shows nowhere else.
type Logged = { cmd: string; args: Record<string, unknown> };
type Mock = {
  __shardxMock: {
    log: Logged[];
    calls: Record<string, number>;
    state: { running: Record<string, unknown>; settings: Record<string, unknown>; profiles: Record<string, { _meta: { lock?: unknown } }> };
  };
};
const VAULT = "lock-vault";
// The mock's seeded password (src/dev/mock/fixtures.ts MOCK_LOCKED_PASSWORD).
const PASSWORD = "vault-pass-1";
const SANDBOX = "f62a9d43";

const locked = (page: Page) => page.addInitScript(() => { (window as unknown as { __shardxMockLocked: boolean }).__shardxMockLocked = true; });
const passwordsOn = (page: Page) => page.addInitScript(() => { (window as unknown as { __shardxMockProfilePasswords: boolean }).__shardxMockProfilePasswords = true; });
const calls = (page: Page, cmd: string) => page.evaluate((c) => (window as unknown as Mock).__shardxMock.log.filter((l) => l.cmd === c).map((l) => l.args), cmd);
const running = (page: Page, id: string) => page.evaluate((i) => i in (window as unknown as Mock).__shardxMock.state.running, id);
const isLocked = (page: Page, id: string) => page.evaluate((i) => !!(window as unknown as Mock).__shardxMock.state.profiles[i]?._meta.lock, id);
const ask = (page: Page, title: string) => page.getByRole("dialog", { name: title, exact: true });
const alert = (page: Page, title: string) => page.getByRole("alertdialog", { name: title, exact: true });

test("a protected row shows its lock and hides what the password seals", async ({ page }) => {
  await locked(page);
  await openApp(page);
  const r = row(page, "Bank vault");
  await expect(r.locator("[data-protected]")).toHaveText("Protected by a password");
  await expect(r.locator("[data-cell='proxy']")).toContainText("Locked");
  // Peek never reads its fingerprint: it says where it is.
  await r.locator("[data-cell='name']").click();
  const peek = page.locator("aside[data-peek]");
  await expect(peek.locator("[data-fingerprint-locked]")).toBeVisible();
  await expect(peek.getByText("Asked at every start")).toBeVisible();
  expect(await calls(page, "profile_get")).not.toContainEqual({ id: VAULT });
  // The parts it does not have while locked are offered, but cannot run.
  await r.click({ button: "right" });
  await expect(page.getByRole("menuitem", { name: /^Clone/ })).toBeDisabled();
  await expect(page.getByRole("menuitem", { name: /^Export cookies/ })).toBeDisabled();
  await expect(page.getByRole("menuitem", { name: /^Change proxy/ })).toBeDisabled();
  await expect(page.getByRole("menuitem", { name: /^Change password/ })).toBeEnabled();
  await expect(page.getByRole("menuitem", { name: /^Remove password/ })).toBeEnabled();
  // Off in Settings, so an open profile is offered no password.
  await page.keyboard.press("Escape");
  await row(page, "Sandbox").click({ button: "right" });
  await expect(page.getByRole("menuitem", { name: /^Set password/ })).toHaveCount(0);
});

test("Start asks the password; a wrong one is said in the dialog, the right one starts it", async ({ page }) => {
  await locked(page);
  await openApp(page);
  const r = row(page, "Bank vault");
  await r.hover();
  await r.getByRole("button", { name: "Start" }).click();
  const d = ask(page, "Start “Bank vault”");
  const field = d.getByLabel("Password", { exact: true });
  await expect(field).toBeFocused();
  await field.fill("not-the-password");
  await d.getByRole("button", { name: "Start", exact: true }).click();
  await expect(d.getByText("Wrong password.")).toBeVisible();
  // Not failed: the row is idle, and no toast says it failed.
  await expect(page.locator("[data-toast]").getByText(/could not start/i)).toHaveCount(0);
  expect(await running(page, VAULT)).toBe(false);
  await field.fill(PASSWORD);
  await d.getByRole("button", { name: "Start", exact: true }).click();
  await expect(d).toHaveCount(0);
  await expect.poll(() => running(page, VAULT)).toBe(true);
  expect((await calls(page, "launch")).at(-1)).toEqual({ profileId: VAULT, password: PASSWORD });
});

test("Cancel in the password dialog starts nothing and says nothing", async ({ page }) => {
  await locked(page);
  await openApp(page);
  const r = row(page, "Bank vault");
  await r.hover();
  await r.getByRole("button", { name: "Start" }).click();
  const d = ask(page, "Start “Bank vault”");
  await d.getByRole("button", { name: "Cancel" }).click();
  await expect(d).toHaveCount(0);
  expect(await calls(page, "launch")).toEqual([]);
  await expect(page.locator("[data-toast]")).toHaveCount(0);
});

test("a bulk Start asks each protected profile in turn; a cancel skips that one", async ({ page }) => {
  await locked(page);
  await openApp(page);
  for (const name of ["Bank vault", "Sandbox"]) await row(page, name).locator("[data-cell='checkbox'] input").check({ force: true });
  await page.getByRole("toolbar", { name: /selected/ }).getByRole("button", { name: "Start" }).click();
  const d = ask(page, "Start “Bank vault”");
  await d.getByRole("button", { name: "Cancel" }).click();
  await expect(page.locator("[data-toast]").filter({ hasText: "Started 1 of 2 profiles" })).toBeVisible({ timeout: 10000 });
  expect(await running(page, SANDBOX)).toBe(true);
  expect(await running(page, VAULT)).toBe(false);
  // Never asked whether to start without a proxy: its proxy is unknown while locked.
  await expect(page.getByRole("alertdialog")).toHaveCount(0);
});

test("the editor opens with the password and saves with it; a rename needs none", async ({ page }) => {
  await locked(page);
  await openApp(page);
  await row(page, "Bank vault").click({ button: "right" });
  await page.getByRole("menuitem", { name: "Edit", exact: true }).click();
  const d = ask(page, "Open “Bank vault”");
  await d.getByLabel("Password", { exact: true }).fill("wrong-one-here");
  await d.getByRole("button", { name: "Open", exact: true }).click();
  await expect(d.getByText("Wrong password.")).toBeVisible();
  await d.getByLabel("Password", { exact: true }).fill(PASSWORD);
  await d.getByRole("button", { name: "Open", exact: true }).click();
  const form = page.getByRole("dialog", { name: "Edit Bank vault", exact: true });
  // The sealed notes, read with the password; no cookies file for a sealed profile.
  await expect(form.getByRole("textbox", { name: "Notes" })).toHaveValue("Online banking only");
  await expect(form.getByText("Cookies", { exact: true })).toHaveCount(0);
  await form.getByRole("textbox", { name: "Notes" }).fill("Banking, weekdays");
  await form.getByRole("button", { name: "Save", exact: true }).click();
  await expect(form).toHaveCount(0);
  const saves = await calls(page, "profile_save");
  expect(saves.at(-1)?.password).toBe(PASSWORD);
  // The row still hides them.
  await expect(row(page, "Bank vault").locator("[data-cell='notes']")).toContainText("Locked");

  // E renames through the stub: no password asked.
  await row(page, "Bank vault").locator("[data-cell='status']").click();
  await page.keyboard.press("e");
  // While renaming, the name is an input value, not row text: the row by id.
  const name = page.locator(`[data-profile-id='${VAULT}']`).getByRole("textbox", { name: "Name" });
  await name.fill("Bank vault 2");
  await name.press("Enter");
  await expect(row(page, "Bank vault 2")).toBeVisible();
  await expect.poll(async () => (await calls(page, "profile_save")).length).toBe(saves.length + 1);
  const rename = (await calls(page, "profile_save")).at(-1)!;
  expect(rename.payload).toMatchObject({ name: "Bank vault 2" });
  expect(rename).not.toHaveProperty("password");
});

test("with the setting on, a new profile can get a password, set as the last step", async ({ page }) => {
  await passwordsOn(page);
  await openApp(page);
  await page.getByRole("button", { name: "New profile" }).first().click();
  const d = page.getByRole("dialog", { name: "New profile", exact: true });
  await d.getByRole("textbox", { name: "Name" }).fill("Locked one");
  const password = d.getByRole("textbox", { name: "Password", exact: true });
  const repeat = d.getByRole("textbox", { name: "Repeat the password", exact: true });
  await password.fill("short");
  await repeat.fill("short");
  await expect(d.getByText("The password needs at least 8 characters.").first()).toBeVisible();
  await expect(d.getByRole("button", { name: "Create profile" })).toBeDisabled();
  await password.fill("long enough 1");
  await repeat.fill("long enough 2");
  await expect(d.getByText("The passwords don’t match.").first()).toBeVisible();
  await repeat.fill("long enough 1");
  await expect(d.locator("[data-no-recovery]")).toBeVisible();
  await d.getByRole("button", { name: "Create profile" }).click();
  await expect(d).toHaveCount(0);
  await expect(row(page, "Locked one").locator("[data-protected]")).toBeAttached();
  const log = await page.evaluate(() => (window as unknown as Mock).__shardxMock.log.map((l) => l.cmd));
  // Made as ever, then sealed: profile_protect comes after every other write of it.
  const protect = log.lastIndexOf("profile_protect");
  expect(protect).toBeGreaterThan(log.lastIndexOf("profile_save"));
});

test("Set, Change and Remove password from the row menu", async ({ page }) => {
  await passwordsOn(page);
  await openApp(page);
  await row(page, "Sandbox").click({ button: "right" });
  await page.getByRole("menuitem", { name: "Set password" }).click();
  const set = ask(page, "Set a password for “Sandbox”");
  await expect(set.locator("[data-no-recovery]")).toBeVisible();
  await set.getByLabel("New password").fill("sandbox-pass");
  await set.getByLabel("Repeat the password").fill("sandbox-pass");
  await set.getByRole("button", { name: "Set password" }).click();
  await expect(set).toHaveCount(0);
  await expect.poll(() => isLocked(page, SANDBOX)).toBe(true);
  await expect(row(page, "Sandbox").locator("[data-protected]")).toBeAttached();

  await row(page, "Sandbox").click({ button: "right" });
  await page.getByRole("menuitem", { name: "Change password" }).click();
  const change = ask(page, "Change the password of “Sandbox”");
  await change.getByLabel("Current password").fill("not-it-at-all");
  await change.getByLabel("New password").fill("sandbox-pass-2");
  await change.getByLabel("Repeat the password").fill("sandbox-pass-2");
  await change.getByRole("button", { name: "Change password" }).click();
  await expect(change.getByText("Wrong password.")).toBeVisible();
  await change.getByLabel("Current password").fill("sandbox-pass");
  await change.getByRole("button", { name: "Change password" }).click();
  await expect(change).toHaveCount(0);

  await row(page, "Sandbox").click({ button: "right" });
  await page.getByRole("menuitem", { name: "Remove password" }).click();
  const remove = alert(page, "Remove the password from “Sandbox”");
  await remove.getByLabel("Current password").fill("sandbox-pass-2");
  await remove.getByRole("button", { name: "Remove password" }).click();
  await expect(remove).toHaveCount(0);
  await expect.poll(() => isLocked(page, SANDBOX)).toBe(false);
  await expect(row(page, "Sandbox").locator("[data-protected]")).toHaveCount(0);
});

test("the Settings switch offers passwords, with what it means", async ({ page }) => {
  await openApp(page, { section: "settings" });
  await page.getByRole("tab", { name: "Profile defaults" }).click();
  const sw = page.getByRole("switch", { name: "Protect profiles with a password" });
  await expect(sw).not.toBeChecked();
  await expect(page.getByText("Asked at every start. A forgotten password can’t be recovered.")).toBeVisible();
  await sw.click();
  await page.locator("[data-save-bar]").getByRole("button", { name: "Save", exact: true }).click();
  await expect.poll(() => page.evaluate(() => (window as unknown as Mock).__shardxMock.state.settings.profile_passwords)).toBe(true);
  await page.locator("aside [data-nav='profiles']").click();
  await row(page, "Sandbox").click({ button: "right" });
  await expect(page.getByRole("menuitem", { name: "Set password" })).toBeVisible();
});
