import type { ProfileMeta } from "../profile/model/types";
import type { ProxyEntry, ProxyTestSnapshot } from "../proxy/model/types";
import { planTierOf, type PlanUsage } from "../proxyshard/lib/planTier";
import { proxyStatus } from "../proxy/lib/status";
import { localeCountry, zoneCountry } from "../../shared/lib/geo";
import { stampMs } from "../../shared/lib/utils";

/// What needs the user on the Profiles page: one pure function over the data
/// the page has, so the problem cards, their counts and their tests read the
/// same answer. The owner's redesign (2026-10-01) keeps only what can cost an
/// account or money, or broke work: a card shows only while it is true.

/** The signals in risk order, the order the cards take: an account at risk,
 *  then money, then broken work. */
export const SIGNAL_KINDS = ["locationLeak", "proxyBalance", "failedToday"] as const;
export type SignalKind = (typeof SIGNAL_KINDS)[number];

/** A profile's hand-set timezone and language ("auto" follows the proxy). */
export type ProfileLocale = { timezone: string; language: string };

type Named = { id: string; name: string };

export type LeakDetail = {
  /** On no proxy (or one that is gone): they start on this computer's own IP. */
  direct: Named[];
  /** A hand-set timezone or language that places the browser outside the
   *  country the proxy's last test saw. `tz`, `lang`: the offending country
   *  and language code (null when that one matches or is Auto). */
  mismatch: (Named & { tz: string | null; lang: string | null; ip: string })[];
};

export type BalanceDetail = {
  plan: PlanUsage;
  /** Under 15 % of its traffic left. */
  low: boolean;
  /** Ends within 3 days (or ended this week). */
  ends: boolean;
  /** Whole days until the traffic runs out at the rate so far; null when no
   *  rate can be told (unmetered, nothing spent, under a day in). */
  daysLeft: number | null;
};

export type FailedDetail = {
  /** Starts that failed today in this session, the latest first. */
  starts: (Named & { at: number; reason: string; proxyId: string | null })[];
  /** Proxies a scope profile uses whose last test failed, the most used first. */
  proxies: (Named & { lastSeen: number | null; error: string; usedBy: number })[];
};

type Base = {
  /** The profiles the card names: its click shows exactly these in the list. */
  ids: string[];
  /** What a hide is about: a hidden card comes back when this set gains one. */
  subjects: string[];
  count: number;
};
export type Signal =
  | (Base & { kind: "locationLeak"; detail: LeakDetail })
  | (Base & { kind: "proxyBalance"; detail: BalanceDetail })
  | (Base & { kind: "failedToday"; detail: FailedDetail });

export type SignalInput = {
  profiles: ProfileMeta[];
  /** The profiles the current Folder or View shows (the search aside). */
  scopeIds: Set<string>;
  /** Running profile ids, each with its run's start, ms. */
  running: Record<string, number>;
  proxies: ProxyEntry[];
  /** Each proxy's last test. */
  snapshots: Record<string, ProxyTestSnapshot>;
  /** The timezone and language of the profiles read so far. */
  locales: Record<string, ProfileLocale>;
  /** When a start last failed this session, and why (the store's). */
  startFailed: Record<string, number>;
  failReasons: Record<string, string>;
  /** The account's residential plans (none without a working key). */
  plans: PlanUsage[];
  now: number;
};

const DAY = 86_400_000;
/** Low: under this share of a metered plan's traffic left. */
export const LOW_SHARE = 0.15;
/** Ends soon: within this long. */
export const ENDS_SOON_MS = 3 * DAY;
const nameOf = (p: Named): Named => ({ id: p.id, name: p.name });
const proxyName = (x: ProxyEntry) => x.name || `${x.host}:${x.port}`;
const sameDay = (a: number, b: number) => new Date(a).toDateString() === new Date(b).toDateString();

/** The country a proxy's last good test saw; null when it never passed one. */
export function testedCountry(snap: ProxyTestSnapshot | undefined): string | null {
  return snap && proxyStatus(snap) === "ok" && snap.country_code ? snap.country_code.toUpperCase() : null;
}

/** Only the signals that are true, in risk order. */
export function computeSignals(input: SignalInput): Signal[] {
  const { profiles, scopeIds, running, proxies, snapshots, locales, startFailed, failReasons, plans, now } = input;
  const scope = profiles.filter((p) => scopeIds.has(p.id));
  const byId = new Map(proxies.map((x) => [x.id, x]));
  const boundTo = (p: ProfileMeta) => (p.proxy_id && byId.has(p.proxy_id) ? p.proxy_id : null);
  const out: Signal[] = [];

  // Location leak. (a) No proxy, or one that is gone: the list's Direct rule.
  // (b) A timezone or language set by hand that places the browser outside
  // the country its proxy's last good test saw; a profile whose proxy never
  // passed a test, or whose settings are not read yet, is not judged.
  // A protected row's proxy is unknown while it is locked: never judged.
  const direct = scope.filter((p) => boundTo(p) === null && !p.protected).map(nameOf);
  const mismatch: LeakDetail["mismatch"] = [];
  for (const p of scope) {
    const px = boundTo(p);
    const ip = px ? testedCountry(snapshots[px]) : null;
    const locale = locales[p.id];
    if (!ip || !locale) continue;
    const tz = zoneCountry(locale.timezone);
    const lc = localeCountry(locale.language);
    const tzOff = tz !== null && tz !== ip ? tz : null;
    const langOff = lc !== null && lc !== ip ? locale.language : null;
    if (tzOff || langOff) mismatch.push({ ...nameOf(p), tz: tzOff, lang: langOff, ip });
  }
  if (direct.length + mismatch.length > 0) {
    const ids = [...direct.map((p) => p.id), ...mismatch.map((p) => p.id)];
    out.push({
      kind: "locationLeak", ids, count: ids.length,
      subjects: [...direct.map((p) => `direct:${p.id}`), ...mismatch.map((m) => `${m.id}:${m.tz ?? ""}:${m.lang ?? ""}@${m.ip}`)],
      detail: { direct, mismatch },
    });
  }

  // Proxy balance: a residential plan under 15 % of its traffic, or ending
  // within 3 days. The most urgent names the card; the scope's profiles on
  // its proxies are what it shows.
  const urgent = plans.flatMap((plan) => {
    const low = plan.total !== null && plan.remain !== null && plan.remain / plan.total < LOW_SHARE;
    const ends = plan.endsAt !== null && plan.endsAt - now <= ENDS_SOON_MS && now - plan.endsAt <= 7 * DAY;
    if (!low && !ends) return [];
    let daysLeft: number | null = null;
    if (plan.remain !== null && plan.spent !== null && plan.spent > 0 && plan.startedAt !== null && now - plan.startedAt >= DAY) {
      daysLeft = Math.max(0, Math.floor(plan.remain / (plan.spent / ((now - plan.startedAt) / DAY))));
    }
    const endsIn = plan.endsAt !== null ? Math.max(0, (plan.endsAt - now) / DAY) : Infinity;
    return [{ plan, low, ends, daysLeft, soon: Math.min(daysLeft ?? Infinity, endsIn) }];
  }).sort((a, b) => a.soon - b.soon);
  if (urgent.length > 0) {
    const { plan, low, ends, daysLeft } = urgent[0];
    const ids = scope.filter((p) => { const px = boundTo(p); return px !== null && planTierOf(byId.get(px)!) === plan.tier; }).map((p) => p.id);
    out.push({
      kind: "proxyBalance", ids, count: ids.length,
      subjects: [`${plan.tier}:${low ? "low" : ""}:${ends ? "ends" : ""}`],
      detail: { plan, low, ends, daysLeft },
    });
  }

  // Failed today: a start that failed today in this session and was not
  // followed by a run (the list's own "Failed to start" rule), and the
  // profiles on a proxy whose last test failed. A start clears it.
  const starts: FailedDetail["starts"] = [];
  for (const p of scope) {
    const at = startFailed[p.id];
    if (at === undefined || running[p.id] !== undefined || !sameDay(at, now)) continue;
    const ran = stampMs(p.last_launched_at);
    if (ran !== null && ran >= at) continue;
    starts.push({ ...nameOf(p), at, reason: failReasons[p.id] ?? "", proxyId: boundTo(p) });
  }
  starts.sort((a, b) => b.at - a.at);
  const on = new Map<string, string[]>();
  for (const p of scope) {
    const px = boundTo(p);
    if (px && proxyStatus(snapshots[px]) === "failed") on.set(px, [...(on.get(px) ?? []), p.id]);
  }
  const failing: FailedDetail["proxies"] = proxies.filter((x) => on.has(x.id)).map((x) => ({
    id: x.id, name: proxyName(x), lastSeen: stampMs(snapshots[x.id]?.last_seen), error: snapshots[x.id]?.geo_error ?? "", usedBy: on.get(x.id)!.length,
  })).sort((a, b) => b.usedBy - a.usedBy);
  const failedIds = [...new Set([...starts.map((s) => s.id), ...failing.flatMap((x) => on.get(x.id)!)])];
  if (failedIds.length > 0) {
    out.push({
      kind: "failedToday", ids: failedIds, count: failedIds.length,
      subjects: [
        ...starts.map((s) => `start:${s.id}@${s.at}`),
        ...failing.flatMap((x) => on.get(x.id)!.map((id) => `${x.id}@${snapshots[x.id]?.last_seen ?? ""}:${id}`)),
      ],
      detail: { starts, proxies: failing },
    });
  }

  return out;
}
