import { readFileSync } from "node:fs";
import { expect, test, type Page } from "@playwright/test";
import { appModule, openApp, seed } from "./support/app";

// Onboarding Task 3: the full tour. Its 18 steps walk every page and end on
// "You're all set." These specs use the real registry (`fullTour`) on the
// real pages; the engine itself is tour-engine.spec.ts.

type Entry = { id: string; section: string; target: string | null; picture?: string };
type TourApi = {
  fullTour: Entry[];
  useTour: { getState: () => { start: (steps: Entry[], kind: "full") => void; ended: { reason: string; kind: string; visited: string[] } | null } };
};
const stepIds = (page: Page) => page.evaluate(() => (window as unknown as { __tour: TourApi }).__tour.fullTour.map((e) => e.id));
const endedOf = (page: Page) => page.evaluate(() => (window as unknown as { __tour: TourApi }).__tour.useTour.getState().ended);

/** What a step must show: its element (a cut-out), its picture (a centred card), or either. */
type Shows = "target" | "picture" | "either";

/** With the seeded profiles (8): the list, Start and Filters are there; the panel is closed and no
 *  project is open, so those two show their pictures. The seed has a Location leak, so Problems to fix
 *  is drawn (its cut-out); on a clean list it is not, and the step shows its picture instead. */
const SEEDED: Record<string, Shows> = {
  sidebar: "target", search: "target", list: "target", newProfile: "target", start: "target",
  panel: "picture", listTools: "target", viewsFolders: "target", problems: "target",
  proxies: "target", fingerprints: "target", extensions: "target", bookmarks: "target",
  automation: "target", editor: "picture", proxyshard: "either", settings: "target", help: "target",
};

/** With no profiles (a real first install: the Get started checklist is the whole Profiles body):
 *  the list, Start, the panel, Filters and Display and Problems to fix are all drawn. */
const EMPTY: Record<string, Shows> = { ...SEEDED, list: "picture", start: "picture", listTools: "picture", problems: "picture" };

/** Steps whose text is true without their element, so a centred card with no picture is right:
 *  an account with no ProxyShard key has no Buy traffic button. */
const TEXT_ONLY = ["proxyshard"];

/** Step 9, as `walk` would hand it to `expectShows`. */
const PROBLEMS: Entry = { id: "problems", section: "profiles", target: "problems", picture: "problemCard" };

const emptyInstall = (page: Page) => page.addInitScript(() => { (window as unknown as { __shardxMockEmpty: boolean }).__shardxMockEmpty = true; });

const card = (page: Page) => page.locator("[data-tour-card]");
const scrim = (page: Page) => page.locator("[data-tour-scrim]");
const onboarding = (page: Page) => page.evaluate(() => JSON.parse(localStorage.getItem("shardx.onboarding") ?? "null"));

/** A first install as the tour sees it: the welcome answered and the tips off (so neither
 *  covers these specs), the tour itself never taken, no page seen. */
const neverToured = (page: Page) => seed(page, "shardx.onboarding", {
  version: 2, state: "unknown", proxySkipped: false, welcomeDone: true, tour: "never", pagesSeen: [], tipsOff: true,
});

/** Starts the full tour, or its steps from `from` up to (not including) `to`. */
async function startTour(page: Page, from = 0, to?: number) {
  await appModule(page, "/src/features/tour/index.ts", "__tour");
  await page.evaluate(([a, b]) => {
    const tour = (window as unknown as { __tour: TourApi }).__tour;
    tour.useTour.getState().start(tour.fullTour.slice(a, b ?? undefined), "full");
  }, [from, to] as const);
}

/** From now on, notes the cut-outs drawn while the card is a beat (`data-beat`), by the page they open. */
async function watchBeats(page: Page) {
  await page.evaluate(() => {
    const w = window as unknown as { __beats: Record<string, string[]> };
    w.__beats = {};
    const note = () => {
      const c = document.querySelector("[data-tour-card]");
      const beat = c?.getAttribute("data-beat");
      const cut = document.querySelector("[data-tour-scrim]")?.getAttribute("data-cutout");
      if (beat && cut && cut !== "none") (w.__beats[beat] ??= []).push(cut);
    };
    new MutationObserver(note).observe(document.body, { subtree: true, childList: true, attributes: true, attributeFilter: ["data-cutout", "data-beat"] });
  });
}

/** The cut-out sits 8 px round the first element with `data-tour="key"` that has a box (`page-title`: the page's heading). */
async function expectHoleAround(page: Page, key: string) {
  const want = () => page.evaluate((k) => {
    const found = k === "page-title" ? document.querySelectorAll("main [data-page-title]") : document.querySelectorAll(`[data-tour="${k}"]`);
    const el = [...found].find((e) => { const r = e.getBoundingClientRect(); return r.width > 0 || r.height > 0; });
    if (!el) return null;
    const html = document.documentElement;
    const z = html.getBoundingClientRect().width / html.offsetWidth || 1;
    const r = el.getBoundingClientRect();
    return [r.left / z - 8, r.top / z - 8, r.width / z + 16, r.height / z + 16];
  }, key);
  await expect.poll(async () => {
    const got = (await scrim(page).getAttribute("data-cutout"))?.split(" ").map(Number) ?? [];
    const exp = await want();
    return !!exp && got.length === 5 && exp.every((v, i) => Math.abs(v - got[i]) <= 1);
  }, { message: `the cut-out sits around ${key}` }).toBe(true);
}

/** Visits every step in order: waits for its card, calls `check`, then Next (Finish on the last). */
async function walk(page: Page, check: (entry: Entry, i: number) => Promise<void>) {
  const entries = await page.evaluate(() => (window as unknown as { __tour: TourApi }).__tour.fullTour.map(({ id, section, target, picture }) => ({ id, section, target, picture })));
  expect(entries).toHaveLength(18);
  for (const [i, entry] of entries.entries()) {
    await expect(card(page)).toHaveAttribute("data-step", entry.id);
    await expect(card(page)).not.toHaveAttribute("data-entering");
    // "3 of 18", "3 / 18" in Chinese, "3 из 18" in Russian.
    await expect(card(page)).toContainText(new RegExp(`(?<!\\d)${i + 1} (?:of|/|из) 18(?!\\d)`));
    await expect(page.locator(`main[data-section='${entry.section}']`)).toBeVisible();
    await check(entry, i);
    // Next, or Finish on the last: the card's last button, in any language.
    await card(page).getByRole("button").last().click();
  }
}

/** The page names the cards show, which are the sidebar's labels. */
const PAGE_NAME: Record<string, string> = {
  profiles: "All profiles", proxies: "Proxies", fingerprints: "Fingerprints", extensions: "Extensions",
  bookmarks: "Bookmarks", automation: "Automation", proxyshard: "ShardX", settings: "Settings", trash: "Trash",
};

/** A quiet line above the step's title: the page's icon (14 px) and name. */
async function expectPageName(page: Page, entry: Entry) {
  const line = card(page).locator("[data-tour-page]");
  await expect(line, `step ${entry.id} names its page`).toHaveText(PAGE_NAME[entry.section]);
  const icon = line.locator("[data-tour-page-icon]");
  await expect(icon).toHaveCount(1);
  const box = (await icon.boundingBox())!;
  expect(Math.round(box.width) + "x" + Math.round(box.height)).toBe("14x14");
  // Above the title, in muted text with no capitals and no tracking.
  const title = (await card(page).locator("h2").boundingBox())!;
  expect((await line.boundingBox())!.y).toBeLessThan(title.y);
  const style = await line.evaluate((el) => { const c = getComputedStyle(el); return { transform: c.textTransform, spacing: c.letterSpacing }; });
  expect(style.transform).toBe("none");
  expect(parseFloat(style.spacing) || 0).toBeLessThanOrEqual(0);
}

/** Every step shows its element or its picture, as `shows` says (the step is on screen and landed). */
async function expectShows(page: Page, entry: Entry, shows: Record<string, Shows>) {
  await expectPageName(page, entry);
  const want = shows[entry.id];
  const hole = await scrim(page).getAttribute("data-cutout");
  const picture = card(page).locator("[data-tour-picture]");
  if (hole === "none") {
    expect(want, `step ${entry.id} lost its element`).not.toBe("target");
    if (TEXT_ONLY.includes(entry.id)) return;
    expect(entry.picture, `step ${entry.id} has neither its element nor a picture`).toBeTruthy();
    await expect(picture).toHaveAttribute("data-tour-picture", entry.picture!);
    await expect(picture).toBeVisible();
  } else {
    // A step with its element is a cut-out round it and no picture.
    expect(want, `step ${entry.id} shows a cut-out where its picture belongs`).not.toBe("picture");
    await expectHoleAround(page, entry.target!);
    await expect(picture).toHaveCount(0);
  }
}

test("the full tour walks all 18 steps to the end card on the 8 seeded profiles", async ({ page }) => {
  test.setTimeout(120_000);
  await neverToured(page);
  await openApp(page);
  await startTour(page);
  expect(Object.keys(SEEDED)).toEqual(await stepIds(page));
  await watchBeats(page);
  await walk(page, (entry) => expectShows(page, entry, SEEDED));
  // Every change of page opened on its item in the sidebar before the step's action.
  const beats = await page.evaluate(() => (window as unknown as { __beats: Record<string, string[]> }).__beats);
  for (const section of ["proxies", "fingerprints", "extensions", "bookmarks", "automation", "proxyshard", "settings", "trash"]) {
    const nav = (await page.locator(`aside [data-nav='${section}']`).boundingBox())!;
    expect(beats[section]?.some((cut) => {
      const [x, y, w, h] = cut.split(" ").map(Number);
      return Math.abs(x - (nav.x - 8)) <= 1 && Math.abs(y - (nav.y - 8)) <= 1 && Math.abs(w - (nav.width + 16)) <= 1 && Math.abs(h - (nav.height + 16)) <= 1;
    }), `the page change to ${section} opened on its sidebar item`).toBe(true);
  }
  // The tour starts on Profiles, which it is on already, so that page has no beat.
  expect(Object.keys(beats).sort()).toEqual(["automation", "bookmarks", "extensions", "fingerprints", "proxies", "proxyshard", "settings", "trash"]);
  // The end card, and the record: a finished tour is "done" and every page it showed is seen.
  const end = page.getByRole("dialog", { name: "You’re all set." });
  await expect(end).toBeVisible();
  await expect(end).toContainText("replay this tour any time from Settings");
  await expect(page.locator("[data-tour-layer]")).toHaveCount(0);
  // Profiles exist here, so Done is the primary button and the other is a plain New profile.
  await expect(end.getByRole("button", { name: "Done" })).toHaveAttribute("data-primary", "");
  await expect(end.getByRole("button", { name: "New profile" })).not.toHaveAttribute("data-primary");
  await expect.poll(async () => (await onboarding(page)).tour).toBe("done");
  expect((await onboarding(page)).pagesSeen).toEqual(["profiles", "proxies", "fingerprints", "extensions", "bookmarks", "automation", "proxyshard", "settings", "trash"]);
  // Done goes to Profiles, where the list (or the Get started checklist) is, not the last page toured.
  await end.getByRole("button", { name: "Done" }).click();
  await expect(end).toHaveCount(0);
  await expect(page.locator("main[data-section='profiles']")).toBeVisible();
});

test("step 9 points at Problems to fix while there is something to fix, and shows its picture when there is not", async ({ page }) => {
  await neverToured(page);
  await openApp(page);
  const needs = page.locator("[data-overview-section='needs']");
  await startTour(page, 8, 9);
  await expect(card(page)).toHaveAttribute("data-step", "problems");
  await expect(card(page)).not.toHaveAttribute("data-entering");
  await expectShows(page, PROBLEMS, SEEDED);
  await page.keyboard.press("Escape");
  await expect(page.locator("[data-tour-layer]")).toHaveCount(0);
  // Hide the seed's only card: nothing to fix. The section is shut (inert, hidden, in the DOM
  // all the same), and the step must not frame it: it shows the picture of a card.
  const leak = page.locator("[data-problem-card='locationLeak']");
  await leak.hover();
  await leak.getByRole("button", { name: "Hide" }).click();
  await expect(needs).toBeHidden();
  await startTour(page, 8, 9);
  await expect(card(page)).toHaveAttribute("data-step", "problems");
  await expect(card(page)).not.toHaveAttribute("data-entering");
  await expectShows(page, PROBLEMS, { ...SEEDED, problems: "picture" });
  await expect(scrim(page)).toHaveAttribute("data-cutout", "none");
});

test("the full tour walks all 18 steps with no profiles: each shows its element or its picture", async ({ page }) => {
  test.setTimeout(120_000);
  // A real first install: no profiles, proxies, bookmarks or extensions; the Get started checklist
  // is the Profiles body, so the list, Filters and Display and Problems to fix are not there.
  await emptyInstall(page);
  await neverToured(page);
  await openApp(page);
  await startTour(page);
  expect(Object.keys(EMPTY)).toEqual(await stepIds(page));
  await walk(page, (entry) => expectShows(page, entry, EMPTY));
  // With no profiles the end card's primary button is the first profile.
  const end = page.getByRole("dialog", { name: "You’re all set." });
  await expect(end).toBeVisible();
  await expect(end.getByRole("button", { name: "Create your first profile" })).toHaveAttribute("data-primary", "");
  await end.getByRole("button", { name: "Done" }).click();
  await expect(end).toHaveCount(0);
});

test("Create your first profile opens the create form", async ({ page }) => {
  // No profiles: a first install in the data too.
  await emptyInstall(page);
  await openApp(page);
  await startTour(page, 17);
  // The card has landed: a press before then is ignored, as the engine holds Next while it looks.
  await expect(card(page)).toHaveAttribute("data-step", "help");
  await expect(card(page)).not.toHaveAttribute("data-entering");
  await card(page).getByRole("button", { name: "Finish" }).click();
  const end = page.getByRole("dialog", { name: "You’re all set." });
  await expect(end).toBeVisible();
  // With no profiles it is the primary button, and Done the plain one.
  const create = end.getByRole("button", { name: "Create your first profile" });
  await expect(create).toHaveAttribute("data-primary", "");
  await expect(end.getByRole("button", { name: "Done" })).not.toHaveAttribute("data-primary");
  await create.click();
  await expect(page.locator("main[data-section='profiles']")).toBeVisible();
  await expect(page.getByRole("dialog", { name: "New profile", exact: true })).toBeVisible();
  await expect(end).toHaveCount(0);
});

test("Skip tour marks the tour skipped and the page it showed as seen", async ({ page }) => {
  await neverToured(page);
  await openApp(page);
  await startTour(page);
  await expect(card(page)).toContainText("1 of 18");
  await card(page).getByRole("button", { name: "Skip tour" }).click();
  await expect(page.locator("[data-tour-layer]")).toHaveCount(0);
  await expect.poll(async () => (await onboarding(page)).tour).toBe("skipped");
  expect((await onboarding(page)).pagesSeen).toEqual(["profiles"]);
  // Skipping is no ending to celebrate.
  await page.waitForTimeout(400);
  await expect(page.getByRole("dialog", { name: "You’re all set." })).toHaveCount(0);
});

test("a page with unsaved changes asks; Cancel ends the tour there", async ({ page }) => {
  await openApp(page, { section: "settings" });
  const bar = page.locator("[data-save-bar]");
  await page.getByRole("switch", { name: /tray/i }).click();
  await expect(bar).toBeVisible();
  // From step 16 (ProxyShard): the tour has to leave Settings, so the guard asks first.
  await startTour(page, 15);
  const guard = page.getByRole("alertdialog", { name: "Save changes to Settings?" });
  await expect(guard).toBeVisible();
  await guard.getByRole("button", { name: "Cancel" }).click();
  await expect(page.locator("[data-tour-layer]")).toHaveCount(0);
  await expect(page.locator("main[data-section='settings']")).toBeVisible();
  await expect(bar).toBeVisible();
  expect(await endedOf(page)).toMatchObject({ reason: "cancelled", kind: "full" });
  await expect.poll(async () => (await onboarding(page)).tour).toBe("skipped");
  // The first step never came up, so no page counts as seen.
  expect((await onboarding(page)).pagesSeen).toEqual([]);
  expect((await endedOf(page))?.visited).toEqual([]);

  // Settings is still unsaved, so its step points at the Save bar ...
  await startTour(page, 16, 17);
  await expect(card(page)).toHaveAttribute("data-step", "settings");
  await expectHoleAround(page, "settings-save");
  expect(await page.locator("[data-tour='settings-save']").evaluate((el) => el.hasAttribute("data-save-bar"))).toBe(true);
  await page.keyboard.press("Escape");
  await expect(page.locator("[data-tour-layer]")).toHaveCount(0);
  // ... and with nothing unsaved, at the page title, in the same words.
  await bar.getByRole("button", { name: "Don’t save" }).click();
  await expect(bar).toHaveCount(0);
  await startTour(page, 16, 17);
  await expect(card(page)).toHaveAttribute("data-step", "settings");
  await expectHoleAround(page, "settings-save");
  expect(await page.locator("[data-tour='settings-save']").evaluate((el) => el.hasAttribute("data-page-title"))).toBe(true);
  await expect(card(page)).toContainText("Save");
});

test("the last step frames the Trash title, not the (i)", async ({ page }) => {
  await neverToured(page);
  await openApp(page);
  await startTour(page, 17);
  await expect(card(page)).toHaveAttribute("data-step", "help");
  await expect(card(page)).not.toHaveAttribute("data-entering");
  await expect.poll(async () => {
    const got = (await scrim(page).getAttribute("data-cutout"))?.split(" ").map(Number) ?? [];
    const title = await page.locator("main[data-section='trash'] [data-page-title]").evaluate((el) => { const r = el.getBoundingClientRect(); return [r.left - 8, r.top - 8, r.width + 16, r.height + 16]; });
    return got.length === 5 && title.every((v, i) => Math.abs(v - got[i]) <= 1);
  }, { message: "the cut-out sits around the page title" }).toBe(true);
});

test("no step of the tour or of a page tip points at the (i), and the (i) carries no tour key", async ({ page }) => {
  test.slow(); // nine pages in turn
  await neverToured(page);
  await openApp(page);
  await appModule(page, "/src/features/tour/index.ts", "__tour");
  const targets = await page.evaluate(() => {
    const t = (window as unknown as { __tour: { fullTour: Entry[]; pageTips: Record<string, Entry[]> } }).__tour;
    return [...t.fullTour, ...Object.values(t.pageTips).flat()].map((e) => e.target);
  });
  expect(targets).not.toContain("help");
  for (const section of ["profiles", "proxies", "fingerprints", "extensions", "bookmarks", "automation", "proxyshard", "trash", "settings"]) {
    await page.locator(`aside [data-nav='${section}']`).click();
    await expect(page.locator(`main[data-section='${section}']`)).toBeVisible();
    const info = page.locator("main header").getByRole("button", { name: "About this page" });
    await expect(info, section).toBeVisible();
    // Nothing a step could look the (i) up by, and no step key sits on the button or inside it.
    expect(await info.evaluate((el) => el.hasAttribute("data-tour") || el.querySelector("[data-tour]") !== null), `${section}: the (i) is no tour target`).toBe(false);
  }
});

// Chinese is the densest script and Russian the longest words: both at the smallest window.
for (const lang of ["zh", "ru"] as const) {
  test(`in ${lang === "zh" ? "Chinese" : "Russian"} at 1024×680 no card text overflows`, async ({ page }) => {
    test.setTimeout(120_000);
    // No profiles: most steps draw their pictures, whose buttons carry translated words too.
    await emptyInstall(page);
    await page.setViewportSize({ width: 1024, height: 680 });
    await page.addInitScript((l) => localStorage.setItem("shardx.lang", l), lang);
    await openApp(page);
    await startTour(page);
    await walk(page, async (entry) => {
      const fit = await card(page).evaluate((el) => {
        // The arrow sits outside the card's edge on purpose: measure without it.
        const arrow = el.querySelector<HTMLElement>("[data-tour-arrow]");
        if (arrow) arrow.style.display = "none";
        const r = el.getBoundingClientRect();
        const result = {
          card: el.scrollWidth <= el.clientWidth && el.scrollHeight <= el.clientHeight,
          // Every text fits in its own box. A button is measured by its label: its hit area
          // (a ::before 5 px out) would count as overflow.
          texts: [...el.querySelectorAll("h2, p, button")].filter((t) => {
            if (t instanceof HTMLButtonElement) {
              const b = t.getBoundingClientRect();
              return [...t.children].some((c) => { const r = c.getBoundingClientRect(); return r.left < b.left - 1 || r.right > b.right + 1 || r.top < b.top - 1 || r.bottom > b.bottom + 1; });
            }
            return !(t.scrollWidth <= t.clientWidth && t.scrollHeight <= t.clientHeight);
          }).map((t) => t.textContent),
          inside: r.left >= 0 && r.top >= 0 && r.right <= window.innerWidth && r.bottom <= window.innerHeight,
        };
        if (arrow) arrow.style.display = "";
        return result;
      });
      expect(fit, `step ${entry.id}`).toEqual({ card: true, texts: [], inside: true });
    });
    // The title and the end card speak the language too.
    const words = JSON.parse(readFileSync(`src/shared/i18n/locales/${lang}.json`, "utf8")) as Record<string, string>;
    await expect(page.getByRole("dialog", { name: words["tour.end.title"] })).toBeVisible();
  });
}
