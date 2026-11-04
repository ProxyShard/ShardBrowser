import { expect, test, type Page } from "@playwright/test";
import { openApp, pressCreate } from "./support/app";

// "More air, clearer hierarchy" (owner-approved mock, 2026-10-01), Lane E's
// part: the ProxyShard Usage section, its stat tiles, the details cards in
// Settings, and Peek's stat strip and row rhythm.

const usage = (page: Page) => page.locator("[data-ps-usage]");
const card = (page: Page, id: string) => page.locator(`[data-usage-card='${id}']`);
/** Usage with its charts drawn (each measures its box first). */
async function openUsage(page: Page, opts: { noHistory?: boolean } = {}) {
  // The history read failing, as it does until the backend has ps_usage (B40).
  if (opts.noHistory) await page.addInitScript(() => Object.assign(window, { __shardxMockFail: { ps_usage: 99 } }));
  await openApp(page, { section: "proxyshard" });
  await expect(usage(page)).toBeVisible();
  if (!opts.noHistory) await expect(card(page, "traffic").locator("svg[role='img']")).toBeVisible();
}

/** How far each Usage card's bottom lies below the last of its content. */
const emptyStretch = (page: Page) => page.locator("[data-usage-card]").evaluateAll((cards) => cards.map((c) => {
  const bottom = Math.max(...[...c.children].map((ch) => ch.getBoundingClientRect().bottom));
  return { id: c.getAttribute("data-usage-card"), gap: c.getBoundingClientRect().bottom - bottom };
}));

test("no Usage card stretches into empty space (owner fix)", async ({ page }) => {
  await openUsage(page);
  // Row one at 1280: the chart over two columns, the stacked pair in the third, one height.
  const chart = (await card(page, "traffic").boundingBox())!;
  const left = (await card(page, "left").boundingBox())!;
  const ends = (await card(page, "ends").boundingBox())!;
  expect(left.x).toBeGreaterThan(chart.x + chart.width);
  expect(Math.abs(ends.x - left.x)).toBeLessThan(1);
  expect(Math.abs(ends.y + ends.height - (chart.y + chart.height))).toBeLessThan(1);
  for (const { id, gap } of await emptyStretch(page)) expect(gap, `${id} ends ${gap}px under its content`).toBeLessThanOrEqual(24);
  // Narrow: two columns, and still no stretch.
  await page.setViewportSize({ width: 860, height: 800 });
  await expect.poll(async () => {
    const c = (await card(page, "traffic").boundingBox())!;
    return (await card(page, "left").boundingBox())!.y - (c.y + c.height);
  }).toBeGreaterThan(0);
  for (const { id, gap } of await emptyStretch(page)) expect(gap, `${id} ends ${gap}px under its content`).toBeLessThanOrEqual(24);
});

test("Usage cards: no empty band inside a card, and sparklines run edge to edge (owner)", async ({ page }) => {
  await openUsage(page);
  for (const width of [1024, 1280, 1600]) {
    await page.setViewportSize({ width, height: 800 });
    await expect(card(page, "traffic").locator("svg[role='img']")).toBeVisible();
    // Each card's children sit within 24 px of each other and of the card's foot.
    const gaps = await page.locator("[data-usage-card]").evaluateAll((cards) => cards.map((c) => {
      const kids = [...c.children].map((k) => k.getBoundingClientRect()).sort((a, b) => a.top - b.top);
      let widest = 0;
      for (let i = 1; i < kids.length; i++) widest = Math.max(widest, kids[i].top - kids[i - 1].bottom);
      return { id: c.getAttribute("data-usage-card"), widest, foot: c.getBoundingClientRect().bottom - Math.max(...kids.map((k) => k.bottom)) };
    }));
    for (const g of gaps) {
      expect(g.widest, `${g.id} at ${width}`).toBeLessThanOrEqual(24);
      expect(g.foot, `${g.id} foot at ${width}`).toBeLessThanOrEqual(24);
    }
    // A sparkline fills its card's inner box: sides and bottom, to the pixel,
    // once it has measured the new width (it follows a ResizeObserver).
    const edges = () => page.locator("[data-usage-rows] [data-usage-card]").evaluateAll((cards) => cards.flatMap((c) => {
      const svg = c.querySelector("[data-sparkline] svg");
      if (!svg) return [];
      const cs = getComputedStyle(c);
      const b = c.getBoundingClientRect();
      const r = svg.getBoundingClientRect();
      return [{ l: r.left - (b.left + parseFloat(cs.borderLeftWidth)), r: (b.right - parseFloat(cs.borderRightWidth)) - r.right, b: (b.bottom - parseFloat(cs.borderBottomWidth)) - r.bottom }];
    }));
    expect((await edges()).length).toBeGreaterThan(0);
    await expect.poll(async () => Math.max(...(await edges()).flatMap((e) => [Math.abs(e.l), Math.abs(e.r), Math.abs(e.b)])), `sparkline edges at ${width}`).toBeLessThanOrEqual(1);
  }
});

test("row two cards are at least 220 px wide, and none sits alone in a row", async ({ page }) => {
  await openUsage(page);
  for (const width of [1024, 1280]) {
    await page.setViewportSize({ width, height: 800 });
    await expect(card(page, "traffic").locator("svg[role='img']")).toBeVisible();
    const rows = await page.locator("[data-usage-rows] [data-usage-card]").evaluateAll((cards) => {
      const byTop = new Map<number, number[]>();
      for (const c of cards) { const r = c.getBoundingClientRect(); byTop.set(Math.round(r.top), [...(byTop.get(Math.round(r.top)) ?? []), r.width]); }
      return { total: cards.length, rows: [...byTop.values()] };
    });
    for (const row of rows.rows) {
      for (const w of row) expect(w, `at ${width}`).toBeGreaterThanOrEqual(220);
      if (rows.total > 1) expect(row.length, `at ${width}`).toBeGreaterThan(1);
    }
  }
});

/** The lower cards (Requests on) grouped by their row: [count in row, every card spans the whole row]. */
const lowerRows = (page: Page) => page.locator("[data-usage-rows]").evaluate((rows) => {
  const cards = [...rows.querySelectorAll<HTMLElement>("[data-usage-card]")].map((c) => c.getBoundingClientRect());
  const width = rows.getBoundingClientRect().width;
  const byTop = new Map<number, DOMRect[]>();
  for (const r of cards) byTop.set(Math.round(r.top), [...(byTop.get(Math.round(r.top)) ?? []), r]);
  return [...byTop.values()].map((row) => ({ n: row.length, full: row.every((r) => r.width > width - 2) }));
});

test("no card sits alone in a row: the lower cards share their rows evenly", async ({ page }) => {
  test.slow(); // two loads and four window sizes: over 30 s on the dev server
  await openUsage(page);
  // Requests and Failed requests: one row of two, wide or narrow.
  expect(await lowerRows(page)).toEqual([{ n: 2, full: false }]);
  await page.setViewportSize({ width: 860, height: 800 });
  await expect.poll(() => lowerRows(page)).toEqual([{ n: 2, full: false }]);

  // Two more residential plans (Datacenter and ISP orders have no card):
  // one row of four; in two columns, two rows of two.
  await page.setViewportSize({ width: 1280, height: 800 });
  const addOrder = (name: string, id: number) => page.evaluate(({ name, id }) => {
    const m = (window as unknown as { __shardxMock: { state: { psOrders: unknown[] }; save: () => void } }).__shardxMock;
    m.state.psOrders.push({ order_id: id, product_name: name, cycle_name: "monthly", expires_at: "2026-11-25T10:00:00Z", auto_renewal: false, tag: null });
    m.save();
  }, { name, id });
  await addOrder("Residential Standart", 40001);
  await addOrder("Residential Unlimited", 40002);
  await page.reload();
  await expect(card(page, "traffic").locator("svg[role='img']")).toBeVisible();
  await expect.poll(() => lowerRows(page)).toEqual([{ n: 4, full: false }]);
  for (const { id, gap } of await emptyStretch(page)) expect(gap, `${id} ends ${gap}px under its content`).toBeLessThanOrEqual(24);
  await page.setViewportSize({ width: 860, height: 800 });
  await expect.poll(() => lowerRows(page)).toEqual([{ n: 2, full: false }, { n: 2, full: false }]);
});

test("the chart's value axis covers the data, a label on every gridline", async ({ page }) => {
  await openUsage(page);
  const chart = card(page, "traffic").locator("svg[role='img']");
  const { grid, labels, lineTop } = await chart.evaluate((svg) => {
    const ys = [...svg.querySelectorAll("[data-grid]")].map((l) => Number(l.getAttribute("y1")));
    const d = svg.querySelector("[data-line]")!.getAttribute("d")!;
    const pathYs = [...d.matchAll(/[ML]([\d.]+),([\d.]+)/g)].map((m) => Number(m[2]));
    return { grid: ys, labels: svg.querySelectorAll("[data-grid-label]").length, lineTop: Math.min(...pathYs) };
  });
  expect(grid.length).toBeGreaterThanOrEqual(2);
  expect(labels).toBe(grid.length);
  // The top gridline sits at or above the line's highest point.
  expect(Math.min(...grid)).toBeLessThanOrEqual(lineTop + 0.5);
  await expect(chart.locator("[data-grid-label]").last()).toHaveText("800 MB");
});

/** Every text a chart draws, and whether its box lies inside its card. */
const chartTextsInCards = (page: Page) => page.locator("[data-usage-card]").evaluateAll((cards) => cards.flatMap((c) => {
  const box = c.getBoundingClientRect();
  return [...c.querySelectorAll("svg text")].map((el) => {
    const r = el.getBoundingClientRect();
    return { card: c.getAttribute("data-usage-card"), text: el.textContent, inside: r.left >= box.left - 0.5 && r.right <= box.right + 0.5 && r.top >= box.top - 0.5 && r.bottom <= box.bottom + 0.5 };
  });
}));

test("every chart label stays inside its card, at 100 % and at Interface size 125 %", async ({ page }) => {
  await openUsage(page);
  const at100 = await chartTextsInCards(page);
  expect(at100.length).toBeGreaterThan(5);
  expect(at100.filter((t) => !t.inside)).toEqual([]);
  // The owner's live window: a zoomed page reports zoomed rects, and the
  // chart drew itself that much too wide, past its card.
  await page.addInitScript(() => localStorage.setItem("shardx.interfaceSize", "125"));
  await page.setViewportSize({ width: 1600, height: 1000 });
  await openUsage(page);
  const at125 = await chartTextsInCards(page);
  expect(at125.length).toBeGreaterThan(5);
  expect(at125.filter((t) => !t.inside)).toEqual([]);
});

test("the day labels sit under their data points, the first full day too", async ({ page }) => {
  await openUsage(page);
  const labels = await card(page, "traffic").locator("svg[role='img']").evaluate((svg) => {
    const d = svg.querySelector("[data-line]")!.getAttribute("d")!;
    const xs = [...d.matchAll(/[ML]([\d.]+),([\d.]+)/g)].map((m) => Number(m[1]));
    const w = Number(svg.getAttribute("width"));
    return [...svg.querySelectorAll("text:not([data-grid-label])")].map((el) => {
      const b = (el as SVGTextElement).getBBox();
      const mid = b.x + b.width / 2;
      return { text: el.textContent, atEdge: b.x < 1 || b.x + b.width > w - 1, off: Math.min(...xs.map((x) => Math.abs(x - mid))) };
    });
  });
  // The mock's 7 days run from Aug 25 18:00: a label for every day from Aug 26.
  expect(labels.map((l) => l.text)).toEqual(["Aug 26", "Aug 27", "Aug 28", "Aug 29", "Aug 30", "Aug 31", "Sep 1"]);
  for (const l of labels) if (!l.atEdge) expect(l.off, `${l.text} is ${l.off.toFixed(1)} px off its point`).toBeLessThan(1);
});

test("The plan card says Lifetime with no bar of its own, and the stacked pair sizes to its content from the column top", async ({ page }) => {
  await openUsage(page);
  await expect(card(page, "ends").getByRole("progressbar")).toHaveCount(0);
  // Residential traffic has no end: no day count, date or renew line (owner, 2026-10-07).
  await expect(card(page, "ends")).toContainText("Lifetime");
  await expect(card(page, "ends")).not.toContainText(/\bdays?\b|Ends|renew/i);
  const left = (await card(page, "left").boundingBox())!;
  const ends = (await card(page, "ends").boundingBox())!;
  // Not stretched to even halves (owner): the plan card, with no bar, is the shorter.
  expect(ends.height).toBeLessThan(left.height);
});

test("without history a card shows the No data ghost, never a line", async ({ page }) => {
  await openUsage(page);
  // Datacenter and ISP orders have no traffic limit: no usage card at all.
  await expect(page.locator("[data-usage-card]", { hasText: "ISP Germany" })).toHaveCount(0);
  await expect(page.locator("[data-usage-card]", { hasText: "Datacenter USA" })).toHaveCount(0);
  await expect(card(page, "requests").locator("[data-no-data]")).toHaveCount(0);

  // The history read failing (B40 not there yet): every chart is the ghost,
  // and no card shows a number the app does not know.
  await openUsage(page, { noHistory: true });
  await expect(card(page, "traffic").locator("[data-no-data]")).toBeVisible();
  await expect(usage(page).locator("svg[role='img']")).toHaveCount(0);
  // Traffic used: what the plans report used this period.
  await expect(card(page, "traffic").locator("[data-stat]")).toHaveText(/^18\.4\s*GB$/);
  await expect(card(page, "traffic")).toContainText("Used this plan period");
  // Requests and Failed requests: no number row, one line saying when.
  for (const id of ["requests", "failed"]) {
    await expect(card(page, id).locator("[data-stat]")).toHaveCount(0);
    await expect(card(page, id).locator("[data-not-reported]")).toHaveText("Shows up when ProxyShard reports it");
    await expect(card(page, id).locator("[data-no-data]")).toBeVisible();
  }
  expect(await usage(page).innerText()).not.toContain("—");
  for (const { id, gap } of await emptyStretch(page)) expect(gap, `${id} ends ${gap}px under its content`).toBeLessThanOrEqual(24);
});

test("Usage: key numbers are the stat role, trends need both periods, the range persists", async ({ page }) => {
  await openUsage(page);
  const value = card(page, "traffic").locator("[data-stat-value]");
  expect(await value.evaluate((el) => { const cs = getComputedStyle(el); return [cs.fontSize, cs.lineHeight, cs.fontWeight]; })).toEqual(["26px", "32px", "600"]);
  // Requests up is good: the mock's drop is red; failed requests down is good: green.
  // Fewer requests is neither good nor bad: soft. Only Failed requests colors its trend.
  await expect(card(page, "requests").locator("[data-trend='down']")).toHaveClass(/text-text-soft-400/);
  await expect(card(page, "failed").locator("[data-trend='down']")).toHaveClass(/text-success-text/);
  await expect(card(page, "traffic").locator("[data-trend]")).toHaveClass(/text-text-soft-400/);
  await expect(card(page, "left")).toContainText("Left on Residential Premium");
  await expect(card(page, "ends")).toContainText("Lifetime");

  const range = usage(page).getByRole("combobox", { name: "Range" });
  await range.click();
  await page.getByRole("option", { name: "Last 30 days" }).click();
  await expect(range).toContainText("Last 30 days");
  await page.reload();
  await expect(usage(page).getByRole("combobox", { name: "Range" })).toContainText("Last 30 days");
});

test("the account facts are stat tiles; the header keeps only the checking word", async ({ page }) => {
  await openApp(page, { section: "proxyshard" });
  await expect(page.locator("[data-ps-tile='balance']")).toContainText(/Balance\s*\$128\.40/);
  await expect(page.locator("[data-ps-tile='orders']")).toContainText(/Active orders\s*3/);
  const number = page.locator("[data-ps-tile='balance'] [data-stat-value]");
  expect(await number.evaluate((el) => getComputedStyle(el).fontSize)).toBe("20px");
  await expect(page.locator("main header [data-ps-facts]")).toHaveCount(0);
});

test("Settings: About and the API are details cards with copy buttons", async ({ page }) => {
  await openApp(page, { section: "settings" });
  const tab = (name: string) => page.getByRole("tablist", { name: "Settings sections" }).getByRole("tab", { name });
  await tab("API & MCP").click();
  const api = page.locator("[data-details='api']");
  await expect(api.getByRole("heading", { name: "Connection" })).toBeVisible();
  await expect(api.locator("[data-detail='base-url']")).toContainText("http://127.0.0.1:40325");
  await expect(api.locator("[data-detail='status']")).toContainText("Running");
  // Compact rows on the tile (borderless, owner 2026-10-01): at least 34 px, no line between them.
  for (const row of await api.locator("[data-detail]").all()) {
    expect((await row.boundingBox())!.height).toBeGreaterThanOrEqual(34);
    expect(await row.evaluate((el) => getComputedStyle(el).borderTopWidth)).toBe("0px");
  }
  await api.locator("[data-detail='base-url']").getByRole("button", { name: "Copy Base URL" }).click();
  await expect(page.locator("[data-toast]").filter({ hasText: "Copied" })).toBeVisible();
  expect(await page.evaluate(() => (window as unknown as { __shardxMockClipboard?: string }).__shardxMockClipboard)).toBe("http://127.0.0.1:40325");

  await tab("About").click();
  const about = page.locator("[data-details='about']");
  await expect(about.locator("[data-detail='version']")).toContainText("2.0.3");
  await expect(about.locator("[data-detail='version']").getByRole("button", { name: "Copy Version" })).toBeVisible();
});

const peek = (page: Page) => page.locator("aside[data-peek='profile']");
async function openPeek(page: Page, name: string) {
  await page.locator("[role='row'][data-profile-id]", { hasText: name }).locator("[data-cell='name']").click();
  await expect(peek(page)).toBeVisible();
}

test("Peek's stat strip shows only the stats that have data", async ({ page }) => {
  await openApp(page);
  await openPeek(page, "Amazon US seller");
  const stats = peek(page).locator("[data-peek-stats] [data-peek-stat]");
  await expect(stats).toHaveCount(3);
  await expect(peek(page).locator("[data-peek-stat='total-time']")).toContainText(/Total time\s*212\s*h/i);
  await expect(peek(page).locator("[data-peek-stat='launches']")).toContainText(/Starts\s*186/i);
  await expect(peek(page).locator("[data-peek-stat='traffic']")).toContainText(/Traffic\s*4\.8\s*GB/i);
  const value = peek(page).locator("[data-peek-stat='launches'] [data-stat-value]");
  expect(await value.evaluate((el) => getComputedStyle(el).fontSize)).toBe("20px");
  // Sandbox never ran: 0 starts and 0 B, known without the backend's counts (B41).
  await openPeek(page, "Sandbox");
  await expect(stats).toHaveCount(3);
  await expect(peek(page).locator("[data-peek-stat='launches']")).toContainText(/Starts\s*0/i);
  await expect(peek(page).locator("[data-peek-stat='traffic']")).toContainText(/Traffic\s*0\s*B/i);
});

test("a profile just created shows its whole stat strip in Peek, not Total time alone (owner, 2026-10-02)", async ({ page }) => {
  await openApp(page);
  await page.getByRole("button", { name: "New profile" }).first().click();
  await pressCreate(page);
  // The created profile's own form, under its real id, once the backend has it.
  await expect(peek(page).locator("[data-peek-section='activity']")).toBeVisible();
  const stats = peek(page).locator("[data-peek-stats] [data-peek-stat]");
  await expect(stats).toHaveCount(3);
  await expect(peek(page).locator("[data-peek-stat='total-time']")).toContainText(/Total time\s*0\s*min/i);
  await expect(peek(page).locator("[data-peek-stat='launches']")).toContainText(/Starts\s*0/i);
  await expect(peek(page).locator("[data-peek-stat='traffic']")).toContainText(/Traffic\s*0\s*B/i);
});

test("Peek's label and value rows: soft 12 px labels, strong 13 px values, 14 px apart", async ({ page }) => {
  await openApp(page);
  await openPeek(page, "Amazon US seller");
  const rows = peek(page).locator("[data-peek-section='profile'] [data-property]");
  const a = (await rows.nth(0).boundingBox())!;
  const b = (await rows.nth(1).boundingBox())!;
  // 32 px rows, 2 px apart: their 20 px lines of text sit 14 px apart.
  expect(Math.round(b.y - (a.y + a.height))).toBe(2);
  const label = rows.nth(0).locator("[data-property-label]");
  expect(await label.evaluate((el) => getComputedStyle(el).fontSize)).toBe("12px");
});

for (const theme of ["light", "dark"] as const) {
  test(`ProxyShard Usage section, ${theme}`, { tag: "@visual" }, async ({ page }) => {
    await page.setViewportSize({ width: 1280, height: 1400 });
    await openApp(page, { section: "proxyshard", theme });
    await expect(card(page, "traffic").locator("svg[role='img']")).toBeVisible();
    await expect(usage(page)).toHaveScreenshot(`ps-usage-${theme}.png`);
  });
}
