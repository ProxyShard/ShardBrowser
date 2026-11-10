import { expect, test, type Page } from "@playwright/test";
import { appModule, openApp } from "./support/app";

// The signal module behind the problem cards on Profiles (the owner's
// overview redesign, 2026-10-01): what can cost an account or money, or broke
// work, as one pure function. Run as the app loads it from the dev server, on
// the pills test page (no mock backend needed).

type Mod = typeof import("../src/entities/signals/signals");
type Input = Parameters<Mod["computeSignals"]>[0];
type Signal = ReturnType<Mod["computeSignals"]>[number];
type Win = { __signals: Mod };
/** The input as the test writes it: the scope as a list (all profiles when left out). */
type Given = Partial<Omit<Input, "scopeIds">> & { scope?: string[] };

const NOW = Date.parse("2026-09-01T12:00:00Z");
const DAY = 86_400_000;
const GB = 1024 ** 3;
/** A launcher stamp, as the backend writes it ("@<unix seconds>"). */
const stamp = (ms: number) => `@${Math.floor(ms / 1000)}`;

const profile = (id: string, over: Record<string, unknown> = {}) => ({
  id, name: `Name ${id}`, notes: "", proxy_id: null as string | null, last_launched_at: stamp(NOW - DAY), created_at: stamp(NOW - 40 * DAY),
  pinned: false, folder: "", total_runtime_ms: 0, color: null, extensions: [], mobile: false, ...over,
});
const proxy = (id: string, over: Record<string, unknown> = {}) => ({ id, name: `Proxy ${id}`, kind: "socks5" as const, host: `${id}.example`, port: 1080, username: "", password: "", country: "", notes: "", ...over });
/** A last test: passed in `cc` (a country code), or failed. */
const tested = (cc: string | null, at = NOW - 12 * 60_000) => ({
  first_seen: stamp(at), last_seen: stamp(at), ip: cc ? "203.0.113.1" : "", country_code: cc ?? "", country: "", region: "", city: "", isp: "",
  timezone: "", latitude: 0, longitude: 0, tcp_ms: cc ? 40 : null, udp_ms: null, udp_error: null, geo_error: cc ? null : "connect timed out after 8000 ms", provider: "",
});
const plan = (over: Record<string, unknown> = {}) => ({
  tier: "premium", orderId: 41822, productName: "Residential Premium", total: 10 * GB, remain: 5 * GB, spent: 5 * GB,
  endsAt: NOW + 20 * DAY, startedAt: NOW - 10 * DAY, ...over,
});

async function load(page: Page) {
  await page.goto("/e2e/kit/pills.html");
  await appModule(page, "/src/entities/signals/signals.ts", "__signals");
}

function signals(page: Page, given: Given): Promise<Signal[]> {
  return page.evaluate(({ given, now }) => {
    const { scope, ...rest } = given;
    const profiles = rest.profiles ?? [];
    const input = {
      profiles, running: {}, proxies: [], snapshots: {}, locales: {}, startFailed: {}, failReasons: {}, plans: [], now, ...rest,
      scopeIds: new Set(scope ?? profiles.map((p) => p.id)),
    };
    return (window as unknown as Win).__signals.computeSignals(input as Input);
  }, { given, now: NOW });
}
const find = (list: Signal[], kind: string) => list.find((s) => s.kind === kind);

test("empty inputs yield no signals", async ({ page }) => {
  await load(page);
  expect(await signals(page, {})).toEqual([]);
});

test("Location leak (a): no proxy, or one that is gone, starts on the real IP; a profile on a proxy does not", async ({ page }) => {
  await load(page);
  const profiles = [profile("1"), profile("2", { proxy_id: "gone" }), profile("3", { proxy_id: "a" })];
  const out = find(await signals(page, { profiles, proxies: [proxy("a")] }), "locationLeak");
  expect(out?.ids).toEqual(["1", "2"]);
  expect(out?.detail).toEqual({ direct: [{ id: "1", name: "Name 1" }, { id: "2", name: "Name 2" }], mismatch: [] });
  expect(await signals(page, { profiles: [profile("3", { proxy_id: "a" })], proxies: [proxy("a")] })).toEqual([]);
});

test("Location leak (b): a hand-set timezone or language outside the proxy's tested country; Auto, UTC and untested proxies are not judged", async ({ page }) => {
  await load(page);
  const px = ["de", "de2", "none", "bad"].map((id) => proxy(id));
  const snapshots = { de: tested("DE"), de2: tested("de"), bad: tested(null) };
  const profiles = [
    profile("tz", { proxy_id: "de" }), profile("lang", { proxy_id: "de" }), profile("ok", { proxy_id: "de" }), profile("auto", { proxy_id: "de2" }),
    profile("utc", { proxy_id: "de" }), profile("untested", { proxy_id: "none" }), profile("failed", { proxy_id: "bad" }), profile("unread", { proxy_id: "de" }),
  ];
  const locales = {
    tz: { timezone: "America/New_York", language: "auto" },
    lang: { timezone: "Europe/Berlin", language: "en-US" },
    ok: { timezone: "Europe/Berlin", language: "de-DE" },
    auto: { timezone: "auto", language: "auto" },
    utc: { timezone: "UTC", language: "auto" },
    untested: { timezone: "America/New_York", language: "en-US" },
    failed: { timezone: "America/New_York", language: "en-US" },
  };
  const out = find(await signals(page, { profiles, proxies: px, snapshots, locales }), "locationLeak");
  expect(out?.detail.mismatch).toEqual([
    { id: "tz", name: "Name tz", tz: "US", lang: null, ip: "DE" },
    { id: "lang", name: "Name lang", tz: null, lang: "en-US", ip: "DE" },
  ]);
  expect(out?.count).toBe(2);
});

test("Location leak: one card holds both, the profiles on no proxy first", async ({ page }) => {
  await load(page);
  const profiles = [profile("m", { proxy_id: "de" }), profile("d")];
  const out = find(await signals(page, {
    profiles, proxies: [proxy("de")], snapshots: { de: tested("DE") }, locales: { m: { timezone: "Asia/Tokyo", language: "auto" } },
  }), "locationLeak");
  expect(out?.ids).toEqual(["d", "m"]);
  expect(out?.subjects).toEqual(["direct:d", "m:JP:@DE"]);
});

test("Proxy balance: under 15 % left, with how long the traffic lasts at the rate so far, or ending within 3 days", async ({ page }) => {
  await load(page);
  // 1.2 of 10 GB left after 10 days at 0.88 GB a day: 1 day.
  const low = find(await signals(page, { plans: [plan({ remain: 1.2 * GB, spent: 8.8 * GB })] }), "proxyBalance");
  expect(low?.detail).toMatchObject({ low: true, ends: false, daysLeft: 1 });
  const ending = find(await signals(page, { plans: [plan({ endsAt: NOW + 2 * DAY })] }), "proxyBalance");
  expect(ending?.detail).toMatchObject({ low: false, ends: true });
  // 15 % left exactly, and 4 days to go: nothing to say.
  expect(await signals(page, { plans: [plan({ remain: 1.5 * GB, spent: 8.5 * GB, endsAt: NOW + 4 * DAY })] })).toEqual([]);
  // Unmetered: only its end counts, and no rate.
  const flat = find(await signals(page, { plans: [plan({ tier: "unmetered", total: null, remain: null, spent: null, endsAt: NOW + DAY })] }), "proxyBalance");
  expect(flat?.detail).toMatchObject({ low: false, ends: true, daysLeft: null });
});

test("Proxy balance names the profiles on that plan's proxies, and the most urgent plan", async ({ page }) => {
  await load(page);
  const proxies = [
    proxy("resi", { host: "relay-eu.shardx.com", username: "plan-premium-country-us" }),
    // Saved before the relays moved to shardx.com: still the same plan.
    proxy("std", { host: "relay-eu.proxyshard.com", username: "plan-limited-country-de" }),
    proxy("dc", { host: "203.0.113.9" }),
  ];
  const profiles = [profile("1", { proxy_id: "resi" }), profile("2", { proxy_id: "dc" }), profile("3", { proxy_id: "std" }), profile("4", { proxy_id: "resi" })];
  const out = find(await signals(page, {
    profiles, proxies,
    plans: [plan({ endsAt: NOW + 2 * DAY }), plan({ tier: "standart", remain: 0.2 * GB, spent: 9.8 * GB })],
  }), "proxyBalance");
  // Standard runs out in a day, before Premium ends in two.
  expect(out?.detail.plan.tier).toBe("standart");
  expect(out?.ids).toEqual(["3"]);
});

test("Failed today: a start that failed today and was not followed by a run, and the profiles on a proxy that failed its test", async ({ page }) => {
  await load(page);
  const profiles = [
    profile("late", { proxy_id: "a" }), profile("early"), profile("yesterday"), profile("ranSince", { last_launched_at: stamp(NOW - 60_000) }),
    profile("running"), profile("onBad", { proxy_id: "bad" }),
  ];
  const out = find(await signals(page, {
    profiles, proxies: [proxy("a"), proxy("bad")], snapshots: { a: tested("US"), bad: tested(null) },
    startFailed: { late: NOW - 60 * 60_000, early: NOW - 3 * 3_600_000, yesterday: NOW - DAY, ranSince: NOW - 5 * 60_000, running: NOW - 60_000 },
    failReasons: { late: "SOCKS5 connection refused" },
    running: { running: NOW - 30_000 },
  }), "failedToday");
  expect(out?.detail.starts.map((s) => s.id)).toEqual(["late", "early"]);
  expect(out?.detail.starts[0]).toMatchObject({ reason: "SOCKS5 connection refused", proxyId: "a" });
  expect(out?.detail.proxies).toEqual([{ id: "bad", name: "Proxy bad", lastSeen: NOW - 12 * 60_000, error: "connect timed out after 8000 ms", usedBy: 1 }]);
  expect(out?.ids).toEqual(["late", "early", "onBad"]);
});

test("every true signal, in risk order: an account, then money, then broken work; the scope counts, the plans do not", async ({ page }) => {
  await load(page);
  const profiles = [profile("in"), profile("out")];
  const list = await signals(page, {
    profiles, scope: ["in"], plans: [plan({ endsAt: NOW + DAY })], startFailed: { in: NOW - 60_000, out: NOW - 60_000 },
  });
  expect(list.map((s) => s.kind)).toEqual(["locationLeak", "proxyBalance", "failedToday"]);
  expect(find(list, "locationLeak")?.ids).toEqual(["in"]);
  expect(find(list, "failedToday")?.ids).toEqual(["in"]);
});

type Hidden = typeof import("../src/entities/signals/hidden");

test("a hidden card stays hidden for the same or fewer subjects, comes back for a new one, and only in its own scope", async ({ page }) => {
  await page.goto("/e2e/kit/pills.html");
  await appModule(page, "/src/entities/signals/hidden.ts", "__hidden");
  const out = await page.evaluate(() => {
    const m = (window as unknown as { __hidden: Hidden }).__hidden;
    const card = (kind: "locationLeak" | "failedToday", subjects: string[]) => ({ kind, subjects });
    m.hideSignal(card("locationLeak", ["direct:a", "direct:b"]), "folder:Personal");
    // A hide in another scope joins nothing and replaces nothing.
    m.hideSignal(card("locationLeak", ["direct:x"]), "folder:Tests");
    const inPersonal = {
      same: m.isHidden(card("locationLeak", ["direct:b", "direct:a"]), "folder:Personal"),
      fewer: m.isHidden(card("locationLeak", ["direct:a"]), "folder:Personal"),
      joined: m.isHidden(card("locationLeak", ["direct:a", "direct:b", "direct:c"]), "folder:Personal"),
      otherKind: m.isHidden(card("failedToday", ["direct:a"]), "folder:Personal"),
    };
    const otherScope = m.isHidden(card("locationLeak", ["direct:a"]), "folder:all");
    // A proxy that was fixed and failed again is a new subject.
    m.hideSignal(card("failedToday", ["px@@100:a"]), "folder:all");
    const failedAgain = m.isHidden(card("failedToday", ["px@@200:a"]), "folder:all");
    return { inPersonal, otherScope, failedAgain, stored: JSON.parse(localStorage.getItem("shardx.signals.hidden") ?? "null") };
  });
  expect(out).toEqual({
    inPersonal: { same: true, fewer: true, joined: false, otherKind: false },
    otherScope: false,
    failedAgain: false,
    stored: {
      locationLeak: { "folder:Personal": ["direct:a", "direct:b"], "folder:Tests": ["direct:x"] },
      failedToday: { "folder:all": ["px@@100:a"] },
    },
  });
});

// A hide goes when its card is no longer true in its scope (with its reads
// in), and with a scope that is gone; a hide of a card the redesign dropped
// hides nothing.
test("a hide goes when its card is no longer true in its scope, and with a scope that is gone; an old card's hide is dropped", async ({ page }) => {
  await page.goto("/e2e/kit/pills.html");
  await page.evaluate(() => localStorage.setItem("shardx.signals.hidden", JSON.stringify({ noProxy: { "folder:all": ["a"] } })));
  await appModule(page, "/src/entities/signals/hidden.ts", "__hidden");
  const out = await page.evaluate(() => {
    const m = (window as unknown as { __hidden: Hidden }).__hidden;
    type Kinds = Parameters<Hidden["pruneHidden"]>[1];
    const kinds = (...k: string[]) => new Set(k) as unknown as Kinds;
    m.hideSignal({ kind: "locationLeak", subjects: ["direct:a"] }, "folder:all");
    m.hideSignal({ kind: "failedToday", subjects: ["px@@100:a"] }, "folder:all");
    m.hideSignal({ kind: "locationLeak", subjects: ["direct:x"] }, "folder:Gone");
    m.hideSignal({ kind: "proxyBalance", subjects: ["premium:low:"] }, "view:v1");
    const exists = (scope: string) => scope === "folder:all" || scope === "view:v1";
    // The leak cleared on All profiles; the tests are not in yet, so Failed today stays.
    m.pruneHidden("folder:all", kinds("locationLeak"), kinds(), exists);
    const first = JSON.parse(localStorage.getItem("shardx.signals.hidden") ?? "null");
    // With the tests in and the proxy passing, its hide goes too.
    m.pruneHidden("folder:all", kinds("locationLeak", "failedToday"), kinds(), exists);
    const then = JSON.parse(localStorage.getItem("shardx.signals.hidden") ?? "null");
    return { first, then, back: m.isHidden({ kind: "locationLeak", subjects: ["direct:a"] }, "folder:all") };
  });
  expect(out).toEqual({
    first: { failedToday: { "folder:all": ["px@@100:a"] }, proxyBalance: { "view:v1": ["premium:low:"] } },
    then: { proxyBalance: { "view:v1": ["premium:low:"] } },
    back: false,
  });
});

test("a broken stored value hides nothing", async ({ page }) => {
  await page.goto("/e2e/kit/pills.html");
  await page.evaluate(() => localStorage.setItem("shardx.signals.hidden", JSON.stringify({ locationLeak: ["a"], failedToday: { "folder:all": [1] }, bogus: { "folder:all": ["x"] } })));
  await appModule(page, "/src/entities/signals/hidden.ts", "__hidden");
  const out = await page.evaluate(() => {
    const m = (window as unknown as { __hidden: Hidden }).__hidden;
    return [m.isHidden({ kind: "locationLeak", subjects: ["a"] }, "folder:all"), m.isHidden({ kind: "failedToday", subjects: ["1"] }, "folder:all")];
  });
  expect(out).toEqual([false, false]);
});

// Review I2: the timezone and language reads behind the Location leak card
// share one queue: two batches asked at once never have more than four
// `profile_get` in flight between them, and every read is still made.
test("the locale reads keep one queue of four across calls", async ({ page }) => {
  await openApp(page);
  await appModule(page, "/src/entities/signals/locales.ts", "__locales");
  const seen = await page.evaluate(async () => {
    type Invoke = (cmd: string, ...rest: unknown[]) => Promise<unknown>;
    const w = window as unknown as { __TAURI_INTERNALS__: { invoke: Invoke }; __locales: { readLocales: (ids: string[]) => Promise<void> } };
    const inner = w.__TAURI_INTERNALS__.invoke;
    let now = 0, most = 0, total = 0;
    w.__TAURI_INTERNALS__.invoke = async (cmd, ...rest) => {
      if (cmd !== "profile_get") return inner(cmd, ...rest);
      now++; total++; most = Math.max(most, now);
      try { return await inner(cmd, ...rest); } finally { now--; }
    };
    const batch = (tag: string) => Array.from({ length: 6 }, (_, i) => `queue-${tag}-${i}`);
    await Promise.all([w.__locales.readLocales(batch("a")), w.__locales.readLocales(batch("b"))]);
    w.__TAURI_INTERNALS__.invoke = inner;
    return { most, total };
  });
  expect(seen).toEqual({ most: 4, total: 12 });
});
