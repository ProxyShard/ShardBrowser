import { expect, test, type Page } from "@playwright/test";
import { appModule, openApp, reloadApp } from "./support/app";

// The command core (Task 1): the key grammar and the registry map, tested
// on the module the app itself loads (Phase D's appModule, dev project).
const MAC = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/140.0.0.0 Safari/537.36";
const WINDOWS = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/140.0.0.0 Safari/537.36";

type Chord = { mod: boolean; shift: boolean; alt: boolean; key: string };
type Core = {
  parseChord: (t: string) => Chord;
  keyCaps: (b: string) => string[];
  ariaKeys: (b: string[]) => string | undefined;
  matchesChord: (e: KeyboardEvent, c: Chord) => boolean;
  matchesSequenceStep: (e: KeyboardEvent, c: Chord) => boolean;
  defineCommands: (list: unknown[]) => void;
  command: (id: string) => { id: string };
  commandsOf: (area: string, surface?: string) => { id: string }[];
};
type W = { __core: Core };

const load = async (page: Page) => {
  await openApp(page);
  await appModule(page, "/src/shared/commands/index.ts", "__core");
};
/** Builds a key event in the page and asks whether it is `binding`. */
const hits = (page: Page, init: KeyboardEventInit, binding: string) =>
  page.evaluate(([init, binding]) => {
    const k = (window as unknown as W).__core;
    return k.matchesChord(new KeyboardEvent("keydown", init), k.parseChord(binding));
  }, [init, binding] as const);
const caps = (page: Page, binding: string) => page.evaluate((b) => (window as unknown as W).__core.keyCaps(b), binding);
const aria = (page: Page, bindings: string[]) => page.evaluate((b) => (window as unknown as W).__core.ariaKeys(b), bindings);

test.describe("keys on macOS", () => {
  test.use({ userAgent: MAC });

  test("a letter is the Latin letter on the key, else the key's place", async ({ page }) => {
    await load(page);
    expect(await hits(page, { key: "s", code: "KeyS" }, "S")).toBe(true);
    // Caps Lock types "S": still S.
    expect(await hits(page, { key: "S", code: "KeyS" }, "S")).toBe(true);
    // A Cyrillic layout: the key in S's place types "ы" (A11Y-02).
    expect(await hits(page, { key: "ы", code: "KeyS" }, "S")).toBe(true);
    // AZERTY: the key printed A sits where US keys have Q; it is A, not Q.
    expect(await hits(page, { key: "a", code: "KeyQ" }, "A")).toBe(true);
    expect(await hits(page, { key: "a", code: "KeyQ" }, "Q")).toBe(false);
    // Shift+S and Ctrl+S are not S.
    expect(await hits(page, { key: "S", code: "KeyS", shiftKey: true }, "S")).toBe(false);
    expect(await hits(page, { key: "s", code: "KeyS", ctrlKey: true }, "S")).toBe(false);
  });

  test("Mod is Cmd; digits by place; punctuation by the character typed", async ({ page }) => {
    await load(page);
    expect(await hits(page, { key: "k", code: "KeyK", metaKey: true }, "Mod+K")).toBe(true);
    expect(await hits(page, { key: "k", code: "KeyK", ctrlKey: true }, "Mod+K")).toBe(false);
    // AZERTY: Cmd and the 1 key types "&" (PC-044).
    expect(await hits(page, { key: "&", code: "Digit1", metaKey: true }, "Mod+1")).toBe(true);
    // "?" is Shift+/ on US keys; the character decides.
    expect(await hits(page, { key: "?", code: "Slash", shiftKey: true }, "?")).toBe(true);
    expect(await hits(page, { key: "/", code: "Slash" }, "?")).toBe(false);
    expect(await hits(page, { key: " ", code: "Space" }, "Space")).toBe(true);
    expect(await hits(page, { key: "Backspace", code: "Backspace" }, "Backspace")).toBe(true);
    expect(await hits(page, { key: "ArrowDown", code: "ArrowDown", shiftKey: true }, "Shift+ArrowDown")).toBe(true);
    expect(await hits(page, { key: "ArrowDown", code: "ArrowDown" }, "Shift+ArrowDown")).toBe(false);
  });

  test("a sequence's letters ignore Shift, as the sheet prints them", async ({ page }) => {
    await load(page);
    const step = (init: KeyboardEventInit, letter: string) => page.evaluate(([init, letter]) => {
      const k = (window as unknown as W).__core;
      return k.matchesSequenceStep(new KeyboardEvent("keydown", init), k.parseChord(letter));
    }, [init, letter] as const);
    expect(await step({ key: "G", code: "KeyG", shiftKey: true }, "G")).toBe(true);
    expect(await step({ key: "g", code: "KeyG", metaKey: true }, "G")).toBe(false);
  });

  test("key caps and aria-keyshortcuts", async ({ page }) => {
    await load(page);
    expect(await caps(page, "Mod+K")).toEqual(["⌘K"]);
    expect(await caps(page, "Mod+Enter")).toEqual(["⌘Enter"]);
    expect(await caps(page, "Delete")).toEqual(["⌫"]);
    expect(await caps(page, "G P")).toEqual(["G", "P"]);
    expect(await caps(page, "Escape")).toEqual(["Esc"]);
    expect(await caps(page, "Shift+ArrowDown")).toEqual(["⇧↓"]);
    expect(await aria(page, ["Delete", "Backspace"])).toBe("Delete Backspace");
    expect(await aria(page, ["Enter", "Space"])).toBe("Enter Space");
    expect(await aria(page, ["Mod+K"])).toBe("Meta+K");
    expect(await aria(page, ["G P"])).toBeUndefined();
  });
});

test.describe("keys on Windows", () => {
  test.use({ userAgent: WINDOWS });

  test("Mod is Ctrl, and the caps spell the modifier", async ({ page }) => {
    await load(page);
    expect(await hits(page, { key: "k", code: "KeyK", ctrlKey: true }, "Mod+K")).toBe(true);
    expect(await hits(page, { key: "k", code: "KeyK", metaKey: true }, "Mod+K")).toBe(false);
    expect(await caps(page, "Mod+K")).toEqual(["Ctrl+K"]);
    expect(await caps(page, "Delete")).toEqual(["Del"]);
    expect(await caps(page, "Shift+F6")).toEqual(["Shift+F6"]);
    expect(await aria(page, ["Mod+K"])).toBe("Control+K");
  });
});

test("the registry keeps one entry per id and refuses an unknown one", async ({ page }) => {
  await load(page);
  const result = await page.evaluate(() => {
    const k = (window as unknown as W).__core;
    const base = { area: "app", scope: "global", group: "help", label: () => "x", surfaces: ["sheet"] };
    k.defineCommands([{ ...base, id: "test.one" }]);
    k.defineCommands([{ ...base, id: "test.one", label: () => "y" }]);
    let threw = false;
    try { k.command("test.missing"); } catch { threw = true; }
    return { count: k.commandsOf("app", "sheet").filter((c) => c.id === "test.one").length, threw };
  });
  expect(result).toEqual({ count: 1, threw: true });
});

// E2 (Task 2): the commands, subjects and adapters, read through the module
// the app itself loads. Nothing renders from them yet (Tasks 3 to 7).
type Row = { id: string; label: string; cap: string | null; aria: string | null };
type Cmd = { id: string; area: string; keys?: string[]; keyOwner?: string; label: (n: number) => string };
type Cmds = {
  describe: (surface: string, area: string, ids: string[]) => Row[];
  labelOf: (id: string, n?: number) => string;
  allCommands: () => Cmd[];
  SETTINGS_TAB_IDS: string[];
  runCommand: (c: Cmd, s: { ids: string[] }, ctx: { from: string }) => Promise<void>;
  profileSelection: () => { ids: string[] };
  proxySelection: () => { ids: string[] };
};
type CW = { __cmds: Cmds };
const loadCommands = async (page: Page, section = "profiles") => {
  await openApp(page, { section });
  await appModule(page, "/src/features/commands/index.ts", "__cmds");
};
const describeIn = (page: Page, surface: string, area: string, ids: string[]) =>
  page.evaluate(([s, a, i]) => (window as unknown as CW).__cmds.describe(s, a, i), [surface, area, ids] as const);

test("a row's menu: one idle profile gets Start, Open in Peek, Change proxy and the keys it has", async ({ page }) => {
  await loadCommands(page);
  const rows = await describeIn(page, "rowMenu", "profiles", ["f62a9d43"]); // Sandbox: idle, no Folder
  const labels = rows.map((r) => r.label);
  expect(labels).toEqual([
    "Start", "Open in Peek", "Edit", "Rename", "Clone", "Pin",
    "Move to folder", "Edit tags", "Change proxy",
    "Copy ID", "Export cookies", "Import cookies",
    "Move to Trash",
  ]);
  const byLabel = Object.fromEntries(rows.map((r) => [r.label, r]));
  expect(byLabel["Start"].aria).toBe("S");
  expect(byLabel["Open in Peek"].aria).toBe("Enter Space");
  expect(byLabel["Rename"].aria).toBe("E");
  expect(byLabel["Move to Trash"].aria).toBe("Delete Backspace");
  expect(byLabel["Clone"].aria).toBeNull();
});

test("a running, pinned profile in a Folder gets Stop, Unpin and Remove from folder", async ({ page }) => {
  await loadCommands(page);
  // Amazon US seller is pinned and in Shops; start it first.
  await page.evaluate(() => {
    const m = (window as unknown as { __shardxMock: { state: { running: Record<string, unknown> }; save: () => void } }).__shardxMock;
    m.state.running["a78a0eea"] = { pid: 4242, started: Date.now() };
    m.save();
  });
  await reloadApp(page);
  await appModule(page, "/src/features/commands/index.ts", "__cmds");
  await expect.poll(async () => (await describeIn(page, "rowMenu", "profiles", ["a78a0eea"])).map((r) => r.label)).toContain("Stop");
  const labels = (await describeIn(page, "rowMenu", "profiles", ["a78a0eea"])).map((r) => r.label);
  expect(labels).not.toContain("Start");
  expect(labels).toContain("Unpin");
  expect(labels).toContain("Remove from folder");
});

test("three rows: one-row commands leave the menu; Change proxy becomes Assign proxies", async ({ page }) => {
  await loadCommands(page);
  const labels = (await describeIn(page, "rowMenu", "profiles", ["aed41005", "c31d92f1", "f62a9d43"])).map((r) => r.label);
  expect(labels).toEqual(["Start", "Move to folder", "Remove from folder", "Edit tags", "Assign proxies", "Move to Trash"]);
});

test("the bulk bars' commands, in order", async ({ page }) => {
  await loadCommands(page);
  expect((await describeIn(page, "bulkBar", "profiles", ["aed41005", "c31d92f1"])).map((r) => r.id))
    .toEqual(["profile.start", "profile.stop", "profile.moveToFolder", "profile.tags", "profile.proxy", "profile.trash"]);
  expect((await describeIn(page, "bulkBar", "proxies", ["px-us-2", "px-jp-1"])).map((r) => r.id))
    .toEqual(["proxy.test", "proxy.assign", "proxy.export", "proxy.delete"]);
});

test("every command has an English and a Chinese label, and no key is taken twice in one area", async ({ page }) => {
  await loadCommands(page);
  const check = () => page.evaluate(() => {
    const all = (window as unknown as CW).__cmds.allCommands();
    const labels = all.map((c) => [c.id, c.label(1)] as const);
    // Pairs share a key on purpose (ruling 5). Automation's and Settings'
    // save are one key, Mod+S, on two pages: each page's own editor listens,
    // and only one page is ever open (manual save).
    const PAIRS = [["profile.start", "profile.stop"], ["profile.clearSelection", "profile.closePeek"], ["proxy.clearSelection", "proxy.closePeek"], ["automation.save", "settings.save"]];
    const paired = (a: string, b: string) => PAIRS.some((p) => p.includes(a) && p.includes(b));
    const clashes: string[] = [];
    for (const a of all) for (const b of all) {
      if (a.id >= b.id || a.area !== b.area) continue;
      const shared = (a.keys ?? []).filter((k) => (b.keys ?? []).includes(k));
      if (shared.length && !paired(a.id, b.id)) clashes.push(`${a.id} ${b.id} ${shared.join(",")}`);
    }
    return { labels, clashes };
  });
  const en = await check();
  expect(en.clashes).toEqual([]);
  for (const [id, label] of en.labels) expect(label, id).not.toMatch(/^(commands|sidebar|settings)\./);
  await page.evaluate(() => localStorage.setItem("shardx.lang", "zh"));
  await reloadApp(page);
  await appModule(page, "/src/features/commands/index.ts", "__cmds");
  const zh = await check();
  const enById = Object.fromEntries(en.labels);
  // Every label is translated (the few words the same in both languages,
  // like "ID", and the ProxyShard brand aside).
  const same = zh.labels.filter(([id, label]) => label === enById[id] && !/ID|JSON|Cookie|ProxyShard|ShardX/.test(label));
  expect(same).toEqual([]);
});

test("Settings commands come from one typed record: every tab has one", async ({ page }) => {
  await loadCommands(page);
  const ids = await page.evaluate(() => (window as unknown as CW).__cmds.SETTINGS_TAB_IDS);
  expect(ids.sort()).toEqual(["about", "api", "appearance", "general", "helper", "other", "profiles", "storage", "sync", "tags", "views"]);
});

test("a command run on the selection keeps the selection exactly as it was (owner rule, ORDER R26 a)", async ({ page }) => {
  await loadCommands(page);
  await appModule(page, "/src/entities/profile/index.ts", "__profile");
  type P = { __profile: { useProfileUi: { getState: () => { toggleSelect: (id: string) => void; selected: Set<string> } } } };
  const picked = ["aed41005", "c31d92f1", "f62a9d43"];
  const after = await page.evaluate(async (picked) => {
    const w = window as unknown as CW & P;
    const ui = w.__profile.useProfileUi;
    for (const id of picked) ui.getState().toggleSelect(id);
    const cmds = w.__cmds;
    const start = cmds.allCommands().find((c) => c.id === "profile.start")!;
    await cmds.runCommand(start, cmds.profileSelection(), { from: "bulkBar" });
    return [...ui.getState().selected].sort();
  }, picked);
  // Not cleared (all started) and not narrowed to what failed.
  expect(after).toEqual([...picked].sort());
});

test("a proxy command run on the shown selection never drops a selected proxy a filter hides", async ({ page }) => {
  await loadCommands(page, "proxies");
  await appModule(page, "/src/entities/proxy/index.ts", "__proxy");
  type X = { __proxy: { useProxy: { getState: () => { proxies: { id: string; host: string }[]; proxySel: Set<string>; toggleSelect: (id: string) => void; setSearch: (q: string) => void } } } };
  await expect.poll(() => page.evaluate(() => (window as unknown as X).__proxy.useProxy.getState().proxies.length)).toBeGreaterThan(1);
  const result = await page.evaluate(async () => {
    const w = window as unknown as CW & X;
    const store = w.__proxy.useProxy;
    const [a, b] = store.getState().proxies;
    store.getState().toggleSelect(a.id);
    store.getState().toggleSelect(b.id);
    // A filter that shows only the first.
    store.getState().setSearch(a.host);
    const cmds = w.__cmds;
    const shown = cmds.proxySelection().ids;
    const exportCmd = cmds.allCommands().find((c) => c.id === "proxy.export")!;
    await cmds.runCommand(exportCmd, cmds.proxySelection(), { from: "bulkBar" });
    return { shown, after: [...store.getState().proxySel].sort(), both: [a.id, b.id].sort() };
  });
  expect(result.shown).toHaveLength(1);
  expect(result.after).toEqual(result.both);
});

// Pair 37 review, fix round 1.
type ProfileUi = {
  folder: string; selected: Set<string>; activeId: string | null;
  editing: { id: string; field: string } | null; assign: { ids: string[] } | null;
  folderPicker: { profileIds: string[] } | null; tagPicker: unknown;
  setFolder: (f: string) => void; setActive: (id: string | null) => void; toggleSelect: (id: string) => void;
};
type WithProfile = CW & { __profile: { useProfileUi: { getState: () => ProfileUi } } };
type MenuRow = { label: string; disabled?: boolean; hint?: string };
type WithMenus = CW & { __cmds: { menuItemsFor: (surface: string, area: string, s: { ids: string[] }) => MenuRow[] } };
const loadProfileUi = (page: Page) => appModule(page, "/src/entities/profile/index.ts", "__profile");
const runIn = (page: Page, id: string, ids: string[], from: string) =>
  page.evaluate(async ([id, ids, from]) => {
    const cmds = (window as unknown as CW).__cmds;
    await cmds.runCommand(cmds.allCommands().find((c) => c.id === id)!, { ids }, { from });
  }, [id, ids, from] as const);

test("with nothing to act on, Move to folder, Change proxy and Edit tags open nothing and say why (I1)", async ({ page }) => {
  await loadCommands(page);
  await loadProfileUi(page);
  await page.evaluate(() => (window as unknown as WithProfile).__profile.useProfileUi.getState().setActive(null));
  for (const id of ["profile.moveToFolder", "profile.proxy", "profile.tags"]) await runIn(page, id, [], "keys");
  const state = await page.evaluate(() => {
    const s = (window as unknown as WithProfile).__profile.useProfileUi.getState();
    return { folderPicker: s.folderPicker, assign: s.assign, tagPicker: s.tagPicker ?? null, editing: s.editing };
  });
  expect(state).toEqual({ folderPicker: null, assign: null, tagPicker: null, editing: null });
  await expect(page.getByRole("dialog")).toHaveCount(0);
  // The palette lists them, disabled, with the reason.
  const items = await page.evaluate(() => (window as unknown as WithMenus).__cmds.menuItemsFor("palette", "profiles", { ids: [] })
    .filter((i) => i.label === "Move to folder" || i.label === "Edit tags")
    .map((i) => ({ label: i.label, disabled: i.disabled, hint: i.hint })));
  expect(items).toEqual([
    { label: "Move to folder", disabled: true, hint: "Select a profile first" },
    { label: "Edit tags", disabled: true, hint: "Select a profile first" },
  ]);
});

test("a palette Rename inside a Folder stays in that Folder (I2)", async ({ page }) => {
  await loadCommands(page);
  await loadProfileUi(page);
  await page.evaluate(() => (window as unknown as WithProfile).__profile.useProfileUi.getState().setFolder("Shops"));
  await runIn(page, "profile.rename", ["a78a0eea"], "palette"); // Amazon US seller, in Shops
  const s = await page.evaluate(() => {
    const ui = (window as unknown as WithProfile).__profile.useProfileUi.getState();
    return { folder: ui.folder, editing: ui.editing?.id ?? null };
  });
  expect(s).toEqual({ folder: "Shops", editing: "a78a0eea" });
});

test("the bar's Change proxy for one profile opens Assign, never the cell picker (I3)", async ({ page }) => {
  await loadCommands(page);
  await loadProfileUi(page);
  await runIn(page, "profile.proxy", ["f62a9d43"], "bulkBar");
  const s = await page.evaluate(() => {
    const ui = (window as unknown as WithProfile).__profile.useProfileUi.getState();
    return { assign: ui.assign, editing: ui.editing };
  });
  expect(s).toEqual({ assign: { ids: ["f62a9d43"] }, editing: null });
});

test("New folder and Show in list keep the owner's glyphs (I4)", async ({ page }) => {
  await loadCommands(page);
  await appModule(page, "/src/shared/icons/index.tsx", "__icons");
  await appModule(page, "/src/shared/ui/rowActionIcons.tsx", "__rowIcons");
  const same = await page.evaluate(() => {
    const w = window as unknown as CW & { __icons: Record<string, unknown>; __rowIcons: { rowActionIcons: Record<string, unknown> } };
    const icon = (id: string) => (w.__cmds.allCommands().find((c) => c.id === id) as unknown as { icon?: unknown }).icon;
    return [icon("app.newFolder") === w.__icons.FolderAddIconApp, icon("profile.showInList") === w.__rowIcons.rowActionIcons.showInList];
  });
  expect(same).toEqual([true, true]);
});

test("a one-proxy Test from the bar reports as the bar always did (M1)", async ({ page }) => {
  await loadCommands(page, "proxies");
  await appModule(page, "/src/entities/proxy/index.ts", "__proxy");
  type X = { __proxy: { useProxy: { getState: () => { proxies: { id: string }[] } } } };
  await expect.poll(() => page.evaluate(() => (window as unknown as X).__proxy.useProxy.getState().proxies.length)).toBeGreaterThan(0);
  const id = await page.evaluate(() => (window as unknown as X).__proxy.useProxy.getState().proxies[0].id);
  await runIn(page, "proxy.test", [id], "bulkBar");
  await expect(page.locator("[data-toast]").filter({ hasText: /tested/i })).toBeVisible();
});

test("a cancelled Regenerate is not a failure (M2)", async ({ page }) => {
  await loadCommands(page);
  await page.evaluate(() => {
    const w = window as unknown as CW & { __run: Promise<unknown> };
    const c = w.__cmds.allCommands().find((x) => x.id === "profile.regenerate") as unknown as { run: (s: { ids: string[] }, ctx: { from: string }) => Promise<unknown> };
    w.__run = c.run({ ids: ["f62a9d43"] }, { from: "palette" });
  });
  await page.getByRole("dialog").getByRole("button", { name: "Cancel" }).click();
  const failed = await page.evaluate(() => (window as unknown as { __run: Promise<unknown> }).__run);
  expect(failed ?? []).toEqual([]);
});

test("the palette's Export and Assign act on their proxy and leave the selection alone (M3)", async ({ page }) => {
  await loadCommands(page, "proxies");
  await appModule(page, "/src/entities/proxy/index.ts", "__proxy");
  type X = { __proxy: { useProxy: { getState: () => { proxies: { id: string }[]; proxySel: Set<string> } } } };
  await expect.poll(() => page.evaluate(() => (window as unknown as X).__proxy.useProxy.getState().proxies.length)).toBeGreaterThan(0);
  const id = await page.evaluate(() => (window as unknown as X).__proxy.useProxy.getState().proxies[0].id);
  const selection = () => page.evaluate(() => [...(window as unknown as X).__proxy.useProxy.getState().proxySel]);
  await runIn(page, "proxy.export", [id], "palette");
  await expect(page.locator("[data-toast]").filter({ hasText: /Copied 1/ })).toBeVisible();
  expect(await selection()).toEqual([]);
  await runIn(page, "proxy.assign", [id], "palette");
  await expect(page.getByRole("dialog")).toBeVisible();
  expect(await selection()).toEqual([]);
});

test("a Move to Trash that fails keeps the failed profile selected, from the selection or one row of it (M4)", async ({ page }) => {
  // Only the first `profile_delete` call fails each time.
  await page.addInitScript(() => { (window as unknown as { __shardxMockFail: Record<string, number> }).__shardxMockFail = { profile_delete: 1 }; });
  await loadCommands(page);
  await loadProfileUi(page);
  const picked = ["aed41005", "c31d92f1", "f62a9d43"];
  const afterBar = await page.evaluate(async (picked) => {
    const w = window as unknown as WithProfile;
    const ui = w.__profile.useProfileUi;
    for (const id of picked) ui.getState().toggleSelect(id);
    const cmds = w.__cmds;
    await cmds.runCommand(cmds.allCommands().find((c) => c.id === "profile.trash")!, cmds.profileSelection(), { from: "bulkBar" });
    return [...ui.getState().selected];
  }, picked);
  expect(afterBar).toEqual([picked[0]]);
  // Again from Peek's menu, on one row of a larger selection.
  await page.evaluate(() => { (window as unknown as { __shardxMockFail: Record<string, number> }).__shardxMockFail = { profile_delete: 1 }; });
  const afterPeek = await page.evaluate(async () => {
    const w = window as unknown as WithProfile;
    const ui = w.__profile.useProfileUi;
    ui.getState().toggleSelect("a78a0eea");
    const cmds = w.__cmds;
    await cmds.runCommand(cmds.allCommands().find((c) => c.id === "profile.trash")!, { ids: ["a78a0eea"] }, { from: "peekMenu" });
    return [...ui.getState().selected].sort();
  });
  expect(afterPeek).toEqual(["a78a0eea", "aed41005"].sort());
});
