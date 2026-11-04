import { expect, test, type Locator, type Page } from "@playwright/test";
import { openApp, type Theme } from "./support/app";
import { openSearch } from "./support/search";

// Borderless, step 1 (owner, 2026-10-01: "reduce amount of borders so it
// looks more like our dashboard platform"; on the mock, "Looks really nice
// borderless"): the shell is two white panels on the gray ground, and the
// kit's secondary buttons, fields at rest, menus and dialogs draw no edge of
// their own. Focus still draws its ring, and a list's rows sit on a tile
// with dividers inset 12 px.

/** The shadow layers that draw a ring: an edge, not a shadow. A layer with
 *  no offset and no blur and a spread of 1 px or more, in a color that
 *  shows (Tailwind's unused slots are transparent zero layers). */
const ringsOf = (shadow: string): string[] => {
  if (shadow === "none") return [];
  return shadow.split(/,(?![^(]*\))/).filter((layer) => {
    const color = layer.match(/rgba?\([^)]*\)/)?.[0] ?? "";
    const alpha = /rgba\(/.test(color) ? Number(color.match(/[\d.]+/g)![3]) : 1;
    const [x, y, blur, spread] = (layer.replace(color, "").match(/-?[\d.]+px/g) ?? []).map(parseFloat);
    return alpha > 0 && x === 0 && y === 0 && (blur ?? 0) === 0 && (spread ?? 0) >= 1;
  });
};

/** Every side's border width, and the rings among its shadows. */
const edgeOf = (el: Locator) => el.evaluate((node) => {
  const cs = getComputedStyle(node);
  return {
    borders: [cs.borderTopWidth, cs.borderRightWidth, cs.borderBottomWidth, cs.borderLeftWidth],
    shadow: cs.boxShadow,
  };
});

async function expectNoEdge(el: Locator, what: string) {
  const { borders, shadow } = await edgeOf(el);
  expect(borders, `${what}: border widths`).toEqual(["0px", "0px", "0px", "0px"]);
  expect(ringsOf(shadow), `${what}: a ring among its shadows (${shadow})`).toEqual([]);
}

const box = async (el: Locator) => (await el.boundingBox())!;

/** A background as the browser computes it, for a CSS value. */
const bgFor = (page: Page, css: string) => page.evaluate((value) => {
  const probe = document.createElement("div");
  probe.style.backgroundColor = value;
  document.body.append(probe);
  const v = getComputedStyle(probe).backgroundColor;
  probe.remove();
  return v;
}, css);
const bgOf = (el: Locator) => el.evaluate((node) => getComputedStyle(node).backgroundColor);

for (const theme of ["light", "dark"] as Theme[]) {
  // Light flip (owner, 2026-10-01): in light the page is the gray ground
  // itself, its content white on it; in dark it is a panel, as in step 1.
  test(`the sidebar is a panel and the page sits 8 px from it and from the window's edges, 12 px corners (${theme})`, async ({ page }) => {
    await openApp(page, { theme });
    const shell = page.locator("[data-shell]");
    const sidebar = page.locator("aside[data-region='sidebar']");
    const main = page.locator("main");
    const ground = await bgFor(page, "var(--bg-canvas)");
    const white = await bgFor(page, "var(--bg-white-0)");
    for (const [panel, name] of [[sidebar, "sidebar"], [main, "page"]] as const) {
      const cs = await panel.evaluate((el) => {
        const s = getComputedStyle(el);
        return { radius: s.borderTopLeftRadius, bg: s.backgroundColor, right: s.borderRightWidth, top: s.borderTopWidth };
      });
      expect(cs.radius, `${name} radius`).toBe("12px");
      expect([cs.right, cs.top], `${name}: no border`).toEqual(["0px", "0px"]);
      const want = name === "page" && theme === "light" ? ground : white;
      expect(cs.bg, `${name}: ${want === ground ? "the ground" : "a white panel"}`).toBe(want);
    }
    const viewport = page.viewportSize()!;
    const s = await box(sidebar);
    const m = await box(main);
    expect(s.x).toBe(8);
    expect(s.y).toBe(8);
    expect(m.y).toBe(8);
    expect(m.x - (s.x + s.width)).toBe(8);
    expect(viewport.width - (m.x + m.width)).toBe(8);
    expect(viewport.height - (m.y + m.height)).toBe(8);
    expect(viewport.height - (s.y + s.height)).toBe(8);
    // The ground between and around them is the canvas.
    expect(await shell.evaluate((el) => getComputedStyle(el).backgroundColor))
      .toBe(await page.evaluate(() => getComputedStyle(document.body).backgroundColor));
  });

  test(`a secondary button, a field at rest, a menu and a dialog draw no border and no ring (${theme})`, async ({ page }) => {
    await openApp(page, { theme });
    const header = page.locator("main header");
    await expectNoEdge(header.getByRole("button", { name: "Display", exact: true }), "secondary button");
    // The field's frame is the searchbox's parent (the kit Input).
    const field = await openSearch(page);
    await field.fill("a");
    await field.blur();
    await expectNoEdge(field.locator("xpath=.."), "field at rest");

    await page.locator("[role='row'][data-profile-id]").first().click({ button: "right" });
    const menu = page.getByRole("menu");
    await expect(menu).toBeVisible();
    await expectNoEdge(menu, "menu");
    expect((await edgeOf(menu)).shadow, "the menu floats on its shadow").not.toBe("none");
    expect(await menu.evaluate((el) => getComputedStyle(el).borderTopLeftRadius)).toBe("12px");
    await page.keyboard.press("Escape");
    await expect(menu).toBeHidden();

    await page.locator("aside").getByRole("button", { name: "New folder" }).click();
    const dialog = page.getByRole("dialog", { name: /New folder/ });
    await expect(dialog).toBeVisible();
    await expectNoEdge(dialog, "dialog");
    expect(await dialog.evaluate((el) => getComputedStyle(el).borderTopLeftRadius)).toBe("12px");
    // Its footer has no rule over it either.
    const footer = dialog.getByRole("button", { name: "Cancel" }).locator("xpath=..");
    expect(await footer.evaluate((el) => getComputedStyle(el).borderTopWidth)).toBe("0px");
  });
}

test("in dark a floating layer carries only its shadow: no lit top edge, never a ring", async ({ page }) => {
  await openApp(page, { theme: "dark" });
  await page.locator("[role='row'][data-profile-id]").first().click({ button: "right" });
  const shadow = (await edgeOf(page.getByRole("menu"))).shadow;
  // The lit top edge is gone (owner, 2026-10-02: "remove it all").
  expect(shadow).not.toMatch(/rgba\(255, 255, 255, 0\.\d+\) 0px 1px 0px 0px inset/);
  expect(shadow).toMatch(/0px 4px 10px -2px/);
  expect(ringsOf(shadow)).toEqual([]);
});

test("focus still shows a ring: a field takes the focus outline on its frame", async ({ page }) => {
  await openApp(page, { theme: "light" });
  const field = await openSearch(page);
  const frame = field.locator("xpath=..");
  await expect.poll(() => frame.evaluate((el) => {
    const cs = getComputedStyle(el);
    return [cs.outlineStyle, cs.outlineWidth];
  })).toEqual(["solid", "2px"]);
});

// The kit DataList on its test page (F23): a tile with a 4 px inner inset,
// and each row's divider inset 12 px from both of the row's ends, on the
// row's top edge: hidden on the first row (under the header), on the
// hovered row and on the row after it (merge point 11: a sibling rule in
// place of `:has(+ row)`).
const grid = (page: Page) => page.getByRole("treegrid", { name: "Things" });
const dataRows = (page: Page) => grid(page).locator("[role='row'][data-row-id]");
const after = (el: Locator) => el.evaluate((node) => {
  const cs = getComputedStyle(node, "::after");
  return { left: cs.left, right: cs.right, height: cs.height, opacity: cs.opacity };
});

test("a DataList sits on a tile, and a row's divider is inset 12 px and hides beside a hovered row", async ({ page }) => {
  await page.goto("/e2e/kit/datalist.html");
  const list = grid(page).locator("xpath=ancestor::*[contains(@class,'list-tile')][1]");
  const tile = await list.evaluate((el) => {
    const cs = getComputedStyle(el);
    return {
      radius: cs.borderTopLeftRadius,
      borders: [cs.borderLeftWidth, cs.borderRightWidth, cs.borderBottomWidth, cs.borderTopWidth],
      inset: [cs.paddingLeft, cs.paddingRight, cs.paddingBottom, cs.paddingTop],
      shadow: cs.boxShadow,
    };
  });
  expect(tile.radius).toBe("10px");
  // No border, as no card has one (owner): the 4 px inset is the tile's
  // padding round the scroll box. In light (this page) the white tile
  // carries the cards' softest shadow, never a ring.
  expect(tile.borders).toEqual(["0px", "0px", "0px", "0px"]);
  // The header spans the tile edge to edge (owner, 2026-10-02): the tile
  // keeps only its 4 px bottom inset, and its rows carry the side inset.
  expect(tile.inset).toEqual(["0px", "0px", "4px", "0px"]);
  expect(tile.shadow).not.toBe("none");
  expect(ringsOf(tile.shadow)).toEqual([]);

  const [first, second, third, fourth] = await dataRows(page).all();
  await page.mouse.move(1, 1);
  expect(await after(second)).toEqual({ left: "12px", right: "12px", height: "1px", opacity: "1" });
  expect(await second.evaluate((node) => getComputedStyle(node, "::after").top)).toBe("0px");
  // The first row's line would run under the header, which has none.
  expect((await after(first)).opacity).toBe("0");
  await third.hover();
  // The hovered row's line and the next row's, the two edges of its fill,
  // hide; the line above the row before it stays.
  await expect.poll(async () => (await after(third)).opacity).toBe("0");
  expect((await after(fourth)).opacity).toBe("0");
  expect((await after(second)).opacity).toBe("1");
});

// Light flip (owner, 2026-10-01: "we need to make that we have a bit darker
// or greyer bg and white on top of it, not reverse"): in light, content is
// white on the gray ground. A list tile is white, its header on its white;
// a card is white; a control on the ground rests white and on a tile at
// the weak fill.
test("light: a list is a white tile on the gray ground, its header on its white, and controls step the other way", async ({ page }) => {
  await openApp(page, { section: "trash", theme: "light" });
  const ground = await bgFor(page, "var(--bg-canvas)");
  const white = await bgFor(page, "#ffffff");
  expect(await bgOf(page.locator("main"))).toBe(ground);
  const list = page.locator("main .list-tile").first();
  expect(await bgOf(list)).toBe(white);
  const head = list.locator("[data-list-header]");
  // No band of its own (owner: "remove grey bg under tables headers rows"):
  // the tile's one opaque color, which its rows and pinned cells paint too
  // (owner, 2026-10-07 item 20: a translucent header and the opaque pinned
  // cells composited apart on a display with its own color profile).
  expect(await bgOf(head)).toBe(await bgFor(page, "#ffffff"));
  // On the ground a secondary button and a field rest white.
  await openApp(page, { theme: "light" });
  const header = page.locator("main header");
  expect(await bgOf(header.getByRole("button", { name: "Display", exact: true }))).toBe(white);
  // The search rests as a button (vf-f, merge point 12); opened, its field is white too.
  expect(await bgOf((await openSearch(page)).locator("xpath=.."))).toBe(white);
  await page.keyboard.press("Escape");
  // In a white surface (the New folder dialog) a field rests at the weak fill.
  await page.locator("aside").getByRole("button", { name: "New folder" }).click();
  const field = page.getByRole("dialog", { name: /New folder/ }).getByRole("textbox").first().locator("xpath=..");
  expect(await bgOf(field)).toBe(await bgFor(page, "var(--bg-weak-50)"));
});

test("light: a card is a white tile on the gray ground, with the softest shadow and no ring", async ({ page }) => {
  await openApp(page, { section: "settings", theme: "light" });
  await page.getByRole("tab", { name: "About" }).click();
  const card = page.locator("[data-details='about']");
  expect(await bgOf(card)).toBe(await bgFor(page, "#ffffff"));
  const { borders, shadow } = await edgeOf(card);
  expect(borders).toEqual(["0px", "0px", "0px", "0px"]);
  expect(shadow).not.toBe("none");
  expect(ringsOf(shadow)).toEqual([]);
});

// Dark keeps step 1: the page panel, a list tile and its header band compute
// to the backgrounds they had before the light flip.
// Dark keeps step 1: the page panel, the list tile and its header, which sits
// inside the tile in the tile's own color (owner: "top row dont have bg"
// meant inside the tile, not a band).
test("dark: the page panel, a list tile and its header keep their step-1 backgrounds", async ({ page }) => {
  await openApp(page, { section: "trash", theme: "dark" });
  expect(await bgOf(page.locator("main"))).toBe(await bgFor(page, "#191919"));
  const list = page.locator("main .list-tile").first();
  // The weak fill's opaque twin: a tile that holds a table paints the one
  // color its header, rows and pinned cells paint (owner, 2026-10-07, item 20).
  expect(await bgOf(list)).toBe(await bgFor(page, "#222222"));
  // No shadow shows: none, or only transparent layers (the tile token's dark "none" is `0 0 #0000`).
  const shadow = (await edgeOf(list)).shadow;
  expect(shadow === "none" || (shadow.match(/rgba?\([^)]*\)/g) ?? []).every((c) => /,\s*0\)$/.test(c)), shadow).toBe(true);
  // The header: the tile's one opaque color, as in light (item 20).
  expect(await bgOf(list.locator("[data-list-header]"))).toBe(await bgFor(page, "#222222"));
  expect((await edgeOf(list)).borders).toEqual(["0px", "0px", "0px", "0px"]);
});

// ---- The standard list header (owner, 2026-10-01) --------------------------
// "all top rows need to be standartized, like on main tables with profiles":
// every table's header row is the kit's `list-head` (ListHeader), with the
// Profiles header's metrics, on the list's own fill ("remove grey bg under
// tables headers rows"); a table has no border, as no card has one;
// one divider parts the header from the first row (owner: "add divider under
// header row"); dividers sit between rows only, and none under the last row
// when it ends the tile.

const TABLES = [
  { section: "profiles", name: "Profiles" },
  { section: "bookmarks", name: "Bookmarks" },
  { section: "proxyshard", name: "Orders" },
] as const;

/** A table's header row as it reads: its height, first label's type, the
 *  label's offset from the list's left edge (its tile), and its background. */
async function headerFacts(page: Page) {
  return page.locator("main [data-list-header]").first().evaluate((head) => {
    const label = head.querySelector<HTMLElement>("[data-col-title]")!;
    const edge = (head.closest(".list-tile") ?? head.closest("[role='treegrid']"))!;
    const ls = getComputedStyle(label);
    const hs = getComputedStyle(head);
    return {
      height: head.getBoundingClientRect().height,
      size: ls.fontSize,
      weight: ls.fontWeight,
      color: ls.color,
      offset: Math.round(label.getBoundingClientRect().left - edge.getBoundingClientRect().left),
      radius: hs.borderTopLeftRadius,
    };
  });
}

for (const theme of ["light", "dark"] as Theme[]) {
  test(`every table's header row is the standard header: Profiles' height, label type and corner (${theme})`, async ({ page }) => {
    const facts: Record<string, Awaited<ReturnType<typeof headerFacts>>> = {};
    for (const t of TABLES) {
      await openApp(page, { section: t.section, theme });
      await expect(page.locator("main [data-list-header]").first()).toBeVisible();
      facts[t.name] = await headerFacts(page);
    }
    const profiles = facts.Profiles;
    for (const t of TABLES) {
      const f = facts[t.name];
      expect({ height: f.height, size: f.size, weight: f.weight, color: f.color, radius: f.radius }, t.name)
        .toEqual({ height: profiles.height, size: profiles.size, weight: profiles.weight, color: profiles.color, radius: profiles.radius });
    }
    expect(profiles).toMatchObject({ height: 32, size: "12px", weight: "500", radius: "0px" });
    // The label's offset from the list's edge: all three tables are tiles
    // (Profiles since vf-f, merge point 12), so the first label sits 16 px in.
    // Profiles selects, and its select box has its own slot before Name (owner,
    // 2026-10-07): its first label sits 38 px further in, the lists that select
    // nothing keep the 16. Bookmarks selects too since bulk delete (2.1.3).
    expect(facts.Orders.offset).toBe(16);
    expect(facts.Profiles.offset).toBe(facts.Orders.offset + 38);
    expect(facts.Bookmarks.offset).toBe(facts.Profiles.offset);
  });

  test(`the header reads as the rows' own fill; one divider, a row divider's twin, parts it from the first row; no border round the table (${theme})`, async ({ page }) => {
    await openApp(page, { section: "bookmarks", theme });
    const tile = page.locator("main .list-tile").first();
    const head = tile.locator("[data-list-header]");
    const first = tile.locator("[role='row'][data-row-id]").first();
    await expect(first).toBeVisible();
    await page.mouse.move(1, 1);
    // Laid over the tile, the header is the color the rows sit on: no band.
    const painted = await tile.evaluate((el) => {
      const paint = (...layers: string[]) => {
        const c = document.createElement("canvas");
        c.width = c.height = 1;
        const x = c.getContext("2d")!;
        x.fillStyle = getComputedStyle(document.querySelector("main")!).backgroundColor;
        x.fillRect(0, 0, 1, 1);
        for (const l of layers) { x.fillStyle = l; x.fillRect(0, 0, 1, 1); }
        return [...x.getImageData(0, 0, 1, 1).data].join(",");
      };
      const tileBg = getComputedStyle(el).backgroundColor;
      const head = el.querySelector("[data-list-header]")!;
      const row = el.querySelector("[role='row'][data-row-id]")!;
      return { head: paint(tileBg, getComputedStyle(head).backgroundColor), row: paint(tileBg, getComputedStyle(row).backgroundColor) };
    });
    expect(painted.head).toBe(painted.row);
    // No border under the header; its divider is the row divider's own line
    // (color, image, inset, height), and the first row hides its own top-edge
    // line: exactly one line between the header and the first row.
    const line = (el: Locator) => el.evaluate((node) => {
      const cs = getComputedStyle(node, "::after");
      return { left: cs.left, right: cs.right, height: cs.height, top: cs.top, bottom: cs.bottom, color: cs.backgroundColor, image: cs.backgroundImage, opacity: cs.opacity };
    });
    expect(await head.evaluate((el) => getComputedStyle(el).borderBottomWidth)).toBe("0px");
    const headLine = await line(head);
    const second = tile.locator("[role='row'][data-row-id]").nth(1);
    const rowLine = await line(second);
    expect({ left: headLine.left, right: headLine.right, height: headLine.height, color: headLine.color, image: headLine.image, opacity: headLine.opacity })
      .toEqual({ left: rowLine.left, right: rowLine.right, height: rowLine.height, color: rowLine.color, image: rowLine.image, opacity: "1" });
    expect(headLine).toMatchObject({ left: "12px", right: "12px", height: "1px", bottom: "0px" });
    expect(rowLine.top).toBe("0px");
    expect((await line(first)).opacity).toBe("0");
    expect(await first.evaluate((el) => getComputedStyle(el).borderTopWidth)).toBe("0px");
    // The table: border 0 and no ring, in both themes.
    const { borders, shadow } = await edgeOf(tile);
    expect(borders).toEqual(["0px", "0px", "0px", "0px"]);
    expect(ringsOf(shadow)).toEqual([]);
  });
}

// The last row's divider (owner, 2026-10-01): dividers go between rows only.
// A row's line is on its top edge (merge point 11's perf rule), so no line
// is ever drawn under the last row: not when it ends the tile, nor when the
// tile is taller than its rows.
const lastRow = (page: Page) => dataRows(page).last();
const noLineUnder = async (row: Locator) => {
  const cs = await row.evaluate((el) => { const a = getComputedStyle(el, "::after"); return { top: a.top, bottom: a.bottom, border: getComputedStyle(el).borderBottomWidth }; });
  expect(cs.top).toBe("0px");
  expect(cs.border).toBe("0px");
};
test("a list's last row draws no divider under it, whether it ends the tile or the tile is taller than its rows", async ({ page }) => {
  // The tile taller than its rows.
  await page.goto("/e2e/kit/datalist.html");
  await expect(lastRow(page)).toBeVisible();
  await page.mouse.move(1, 1);
  await noLineUnder(lastRow(page));
  // A middle row keeps its line, on its top edge.
  expect((await after(dataRows(page).nth(3))).opacity).toBe("1");
  // Short of room: scrolled to its end, the last row sits on the tile's inset with no line under it.
  await page.setViewportSize({ width: 1280, height: 320 });
  const box = grid(page).locator("xpath=..");
  await box.evaluate((el) => { el.scrollTop = el.scrollHeight; });
  await expect(lastRow(page)).toBeVisible();
  await noLineUnder(lastRow(page));
});

test("Profiles: scrolled to its end, the last row draws no divider under it", async ({ page }) => {
  await page.addInitScript(() => Object.assign(window, { __shardxMockProfiles: 60 }));
  await openApp(page, { theme: "light" });
  const box = page.getByRole("treegrid");
  await box.evaluate((el) => { el.scrollTop = el.scrollHeight; });
  await page.mouse.move(1, 1);
  const last = page.locator("[role='row'][data-profile-id]").last();
  await expect(last).toBeVisible();
  await noLineUnder(last);
});
