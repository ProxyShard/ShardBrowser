import { expect, test } from "@playwright/test";
import { appModule, openApp } from "./support/app";

// Audit Phase G2: whole sentences with inline tags (t.rich).

type Rich = {
  parseRich(s: string): { tag?: string; text: string }[];
  renderRich(s: string, tags: Record<string, (c: string) => unknown>, vars?: Record<string, string>): { props: { children: unknown } }[];
};

test("t.rich: tags wrap their text, variables stay text, unknown tags are plain", async ({ page }) => {
  await openApp(page);
  await appModule(page, "/src/shared/i18n/rich.ts", "__r");
  const out = await page.evaluate(() => {
    const r = (window as unknown as { __r: Rich }).__r;
    return {
      parts: r.parseRich("Uses <b>From fingerprint</b> sizes, <code>x</code>."),
      unclosed: r.parseRich("a <b>b"),
      rendered: r.renderRich("Delete <b>{name}</b>?", { b: (c) => `[${c}]` }, { name: "Tom’s <b>shop</b>" }).map((e) => e.props.children),
      unknownTag: r.renderRich("a <i>b</i>", {}).map((e) => e.props.children),
    };
  });
  expect(out.parts).toEqual([{ text: "Uses " }, { tag: "b", text: "From fingerprint" }, { text: " sizes, " }, { tag: "code", text: "x" }, { text: "." }]);
  expect(out.unclosed).toEqual([{ text: "a <b>b" }]);
  expect(out.rendered).toEqual(["Delete ", "[Tom’s <b>shop</b>]", "?"]);
  expect(out.unknownTag).toEqual(["a ", "b"]);
});

test("a sentence with a bold word renders as one sentence", async ({ page }) => {
  // The GitHub-star ask, due at the next confirmed start (as star-ask.spec seeds it).
  await page.addInitScript(() => {
    localStorage.setItem("shardx-star-prompt", "counting");
    localStorage.setItem("shardx.starAsk", JSON.stringify({ v: 1, starts: 9, days: ["2020-01-01"], state: "counting" }));
  });
  await openApp(page);
  const row = page.locator("[role='row'][data-profile-id]", { hasText: "Etsy craft shop" });
  await row.hover();
  await row.getByRole("button", { name: "Start" }).click();
  const dialog = page.getByRole("dialog", { name: /Enjoying ShardBrowser/ });
  await expect(dialog.locator("strong", { hasText: "completely free" })).toBeVisible();
  await expect(dialog).toContainText("ShardBrowser is provided and supported completely free. If it’s useful to you");
});

type Fmt = {
  fmtCount(n: number): string; fmtMoney(c: number): string; fmtBytes(b: number): string;
  fmtBytesProgress(d: number, t: number): string; fmtDuration(ms: number): string;
};
const NB = " ";

async function formats(page: import("@playwright/test").Page, lang: "en" | "zh") {
  if (lang === "zh") await page.addInitScript(() => localStorage.setItem("shardx.lang", "zh"));
  await openApp(page);
  await appModule(page, "/src/shared/lib/format.ts", "__f");
  return page.evaluate(() => {
    const f = (window as unknown as { __f: Fmt }).__f;
    return {
      counts: [0, 1, 1283, 2000].map((n) => f.fmtCount(n)),
      money: [0, 1250, 123450].map((c) => f.fmtMoney(c)),
      bytes: [0, 1023, 1024, 20027801, 1099511627776].map((b) => f.fmtBytes(b)),
      progress: f.fmtBytesProgress(5033165, 20027801),
      durations: [45_000, 14 * 60_000, 2 * 3_600_000 + 14 * 60_000, 212 * 3_600_000].map((ms) => f.fmtDuration(ms)),
    };
  });
}

test("numbers, money, sizes and durations in English", async ({ page }) => {
  expect(await formats(page, "en")).toEqual({
    counts: ["0", "1", "1,283", "2,000"],
    money: ["$0.00", "$12.50", "$1,234.50"],
    bytes: [`0${NB}B`, `1,023${NB}B`, `1${NB}KB`, `19.1${NB}MB`, `1${NB}TB`],
    progress: `4.8 of 19.1${NB}MB`,
    durations: [`45${NB}s`, `14${NB}min`, `2${NB}h 14${NB}min`, `8${NB}d 20${NB}h`],
  });
});

test("numbers, money, sizes and durations in Chinese", async ({ page }) => {
  expect(await formats(page, "zh")).toEqual({
    counts: ["0", "1", "1,283", "2,000"],
    money: ["$0.00", "$12.50", "$1,234.50"],
    bytes: [`0${NB}B`, `1,023${NB}B`, `1${NB}KB`, `19.1${NB}MB`, `1${NB}TB`],
    progress: `4.8 / 19.1${NB}MB`,
    durations: [`45${NB}秒`, `14${NB}分钟`, `2${NB}小时 14${NB}分钟`, `8${NB}天 20${NB}小时`],
  });
});

test("counts inside copy are grouped; other numbers are left alone", async ({ page }) => {
  await openApp(page);
  await appModule(page, "/src/shared/i18n/index.ts", "__i");
  const out = await page.evaluate(() => {
    const i = (window as unknown as { __i: { translate(l: string, k: string, v?: Record<string, number>): string } }).__i;
    // An unknown key renders as itself, so the key is the template here.
    return [
      i.translate("en", "Port {port}, order #{id}: {n} of {total}", { port: 9222, id: 12345, n: 1283, total: 2000 }),
      i.translate("zh", "{n} 个", { n: 1283 }),
    ];
  });
  expect(out).toEqual(["Port 9222, order #12345: 1,283 of 2,000", "1,283 个"]);
});

test("a Russian count picks one of three word forms", async ({ page }) => {
  await openApp(page);
  await appModule(page, "/src/shared/i18n/index.ts", "__i");
  const out = await page.evaluate(() => {
    type I = {
      translate(l: string, k: string, v?: Record<string, string | number>): string;
      inLanguage<R>(l: string, fn: () => R): R;
      t: { rich(k: string, tags: Record<string, (c: string) => unknown>, v?: Record<string, string | number>): { props: { children: unknown } }[] };
    };
    const i = (window as unknown as { __i: I }).__i;
    // An unknown key renders as itself, so the key is the template here.
    const files = "{n} {n|файл|файла|файлов}";
    return {
      counts: [0, 1, 2, 4, 5, 11, 14, 21, 22, 25, 101, 111, 1283].map((n) => i.translate("ru", files, { n })),
      // Already formatted, or a fraction (Russian says "2,5 файла").
      strings: [i.translate("ru", files, { n: "21" }), i.translate("ru", files, { n: "1 000" }), i.translate("ru", files, { n: 2.5 })],
      // No count given: the form a list reads in, never the raw selector.
      missing: i.translate("ru", "{n|файл|файла|файлов}"),
      // Bold around a count: the form is picked before the sentence is parsed.
      rich: i.inLanguage("ru", () => i.t.rich("<b>{n}</b> {n|профиль|профиля|профилей}", { b: (c) => `[${c}]` }, { n: 3 })).map((e) => e.props.children),
      english: i.translate("en", "{n} files", { n: 21 }),
    };
  });
  expect(out.counts).toEqual([
    "0 файлов", "1 файл", "2 файла", "4 файла", "5 файлов", "11 файлов", "14 файлов",
    "21 файл", "22 файла", "25 файлов", "101 файл", "111 файлов", `1${NB}283 файла`,
  ]);
  expect(out.strings).toEqual(["21 файл", "1 000 файлов", "2,5 файла"]);
  expect(out.missing).toBe("файлов");
  expect(out.rich).toEqual(["[3]", " профиля"]);
  expect(out.english).toBe("21 files");
});
