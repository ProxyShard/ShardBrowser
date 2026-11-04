import type { ProxyEntry } from "../../proxy";
import { stampMs } from "../../../shared/lib/utils";
import type { ProfileMeta } from "../model/types";

/** The Connection filter: "bound" = has a proxy, "direct" = none,
 *  "shared" = on a proxy another profile the list keeps uses too (what the
 *  Shared proxy card counts), `proxy:<id>` = on that one proxy (`onProxy`). */
export type ProxyFilter = "all" | "bound" | "direct" | "shared" | `proxy:${string}`;

/** Narrows the list beyond the folder tab and the search box.
 *
 *  A caller that points the user at a set of profiles by what they share
 *  (the Live layer's action cards) sets one field through
 *  `useProfileUi.setFilters`, and the list shows it as a removable chip:
 *  `{ proxy: onProxy(id) }` lists the profiles on one proxy,
 *  `{ lastRun: "stale" }` those not run in 30 days (or never),
 *  `{ lastRun: "unused" }` exactly what the Unused card counts (`isUnused`). */
export type ProfileFilters = {
  status: "all" | "running" | "idle";
  /** Country code of the bound proxy, or "" for any. */
  country: string;
  proxy: ProxyFilter;
  /** A tag id, or "" for any. */
  tag: string;
  /** When the profile last ran: in the last day, week or month; "stale" = not
   *  in 30 days (or never); "unused" = unused for a month (`isUnused`: a new
   *  profile that has not run yet is not); "never" = never. */
  lastRun: "any" | "day" | "week" | "month" | "stale" | "unused" | "never";
};

export const emptyFilters = (): ProfileFilters => ({ status: "all", country: "", proxy: "all", tag: "", lastRun: "any" });

const ON_PROXY = "proxy:";
/** The Connection filter's value for the profiles on one proxy. */
export const onProxy = (proxyId: string): ProxyFilter => `${ON_PROXY}${proxyId}`;
/** The proxy a Connection filter value names, or null for any, bound or direct. */
export const proxyIdOf = (v: ProxyFilter): string | null => (v.startsWith(ON_PROXY) ? v.slice(ON_PROXY.length) : null);
/** Whether a stored value is a Connection filter value. */
export const isProxyFilter = (v: unknown): v is ProxyFilter =>
  v === "all" || v === "bound" || v === "direct" || v === "shared" || (typeof v === "string" && v.startsWith(ON_PROXY) && v.length > ON_PROXY.length);

/** How many of the filter bar's fields narrow the list: the count on the
 *  Filters button and the chips row (Task 19). */
export function countActiveFilters(f: ProfileFilters): number {
  return (f.status !== "all" ? 1 : 0) + (f.country ? 1 : 0) + (f.proxy !== "all" ? 1 : 0) + (f.tag ? 1 : 0) + (f.lastRun !== "any" ? 1 : 0);
}

const DAY_MS = 24 * 60 * 60 * 1000;
const UNUSED_MS = 30 * DAY_MS;

/** Unused for a month: it last ran more than 30 days ago, or it never ran and
 *  was made more than 30 days ago (spec 5.3). The Unused action card counts
 *  with it and its Review filter (`lastRun: "unused"`) lists with it, so the
 *  count and the list always agree. */
export function isUnused(p: { last_launched_at: string | null; created_at: string | null }, now: number): boolean {
  const at = stampMs(p.last_launched_at);
  if (at !== null) return now - at > UNUSED_MS;
  const made = stampMs(p.created_at);
  return made !== null && now - made > UNUSED_MS;
}

/** Whether a last-run stamp passes the Last run filter's value (the Filters
 *  menu counts each value with it). "unused" also reads when the profile
 *  was made (`created`). */
export function lastRunMatches(stamp: string | null | undefined, want: ProfileFilters["lastRun"], now: number, created: string | null = null) {
  if (want === "any") return true;
  if (want === "unused") return isUnused({ last_launched_at: stamp ?? null, created_at: created }, now);
  const at = stampMs(stamp);
  if (want === "never") return at === null;
  if (want === "stale") return at === null || now - at > UNUSED_MS;
  if (at === null) return false;
  return now - at <= (want === "day" ? 1 : want === "week" ? 7 : 30) * DAY_MS;
}

/** Row order. "added" is the order the store returns them in — how it always was. */
export type ProfileSort =
  | "added"
  | "name-asc"
  | "name-desc"
  | "created-desc"
  | "created-asc"
  | "launched-desc"
  | "runtime-desc"
  | "status";

const SORT_KEY = "shardx.profiles.sort";

export const loadSort = (): ProfileSort => {
  try {
    const v = localStorage.getItem(SORT_KEY);
    return (v as ProfileSort) || "added";
  } catch {
    return "added";
  }
};

export const persistSort = (v: ProfileSort) => {
  try { localStorage.setItem(SORT_KEY, v); } catch { /* private mode */ }
};

/** One collator for every name sort: a collator built per comparison made a
 *  2,000-name sort 12 times slower (PERF-05). */
const NAME_COLLATOR = new Intl.Collator(undefined, { numeric: true });
/** Each name's place in the collator's order among the names last ranked,
 *  names the collator calls equal sharing one. Every change to the list
 *  (each row an Undo brings back, one by one) sorts it again; while its
 *  names are all known, two numbers are compared instead of two names, in
 *  the same order. A name not seen yet ranks the list's names afresh. */
let ranked: Map<string, number> | null = null;
function nameRanks(list: readonly ProfileMeta[]): Map<string, number> {
  if (ranked && list.every((p) => ranked!.has(p.name))) return ranked;
  const names = [...new Set(list.map((p) => p.name))].sort(NAME_COLLATOR.compare);
  const rank = new Map<string, number>();
  let at = 0;
  names.forEach((name, i) => {
    if (i > 0 && NAME_COLLATOR.compare(names[i - 1], name) !== 0) at++;
    rank.set(name, at);
  });
  ranked = rank;
  return rank;
}
/** Folder names keep their plain order, as before. */
const FOLDER_COLLATOR = new Intl.Collator();

/** Missing dates sort last, whichever direction is asked for. */
function byDate(a: string | null | undefined, b: string | null | undefined, desc: boolean) {
  const av = stampMs(a);
  const bv = stampMs(b);
  if (av === null && bv === null) return 0;
  if (av === null) return 1;
  if (bv === null) return -1;
  return desc ? bv - av : av - bv;
}

export function sortProfiles(
  list: ProfileMeta[],
  sort: ProfileSort,
  /** Running (and starting — the store marks a launch running at once)
   *  profile ids, for `sort === "status"` and `"runtime-desc"`: each value
   *  is the run's start time, ms. */
  running: Record<string, number> = {},
  /** For `"runtime-desc"`: added to a running profile's stored total, the
   *  same running session the Run time cell adds (Batch A review, Minor 3). */
  now: number = Date.now(),
): ProfileMeta[] {
  // Pinning is the operator saying "keep this one where I can see it", and an
  // ordering that scatters pinned rows among two hundred others makes the pin
  // button look broken. Every order sorts within the pinned rows and within
  // the rest, and keeps the two groups apart.
  const pinned = list.filter((p) => p.pinned);
  const rest = list.filter((p) => !p.pinned);
  // Ranked over the whole list, so both groups read the same ranks.
  const rank = sort === "name-asc" || sort === "name-desc" ? nameRanks(list) : null;
  return [...sortGroup(pinned, sort, running, now, rank), ...sortGroup(rest, sort, running, now, rank)];
}

function sortGroup(list: ProfileMeta[], sort: ProfileSort, running: Record<string, number>, now: number, rank: Map<string, number> | null): ProfileMeta[] {
  // "added" is the order the store returns them in.
  if (sort === "added") return list;
  const out = [...list];
  switch (sort) {
    case "name-asc":
      out.sort((a, b) => rank!.get(a.name)! - rank!.get(b.name)!);
      break;
    case "name-desc":
      out.sort((a, b) => rank!.get(b.name)! - rank!.get(a.name)!);
      break;
    case "created-desc":
      out.sort((a, b) => byDate(a.created_at, b.created_at, true));
      break;
    case "created-asc":
      out.sort((a, b) => byDate(a.created_at, b.created_at, false));
      break;
    case "launched-desc":
      out.sort((a, b) => byDate(a.last_launched_at, b.last_launched_at, true));
      break;
    case "runtime-desc": {
      // The same value the Run time cell shows: the stored total, plus the
      // running session so far for a profile that is running now.
      const total = (p: ProfileMeta) => {
        const since = running[p.id];
        const live = since !== undefined && Number.isFinite(since) ? Math.max(0, now - since) : 0;
        return p.total_runtime_ms + live;
      };
      out.sort((a, b) => total(b) - total(a));
      break;
    }
    case "status":
      // Running (and starting) first, then most recently run, never-run last.
      out.sort((a, b) => {
        const ar = !!running[a.id];
        const br = !!running[b.id];
        if (ar !== br) return ar ? -1 : 1;
        return byDate(a.last_launched_at, b.last_launched_at, true);
      });
      break;
  }
  return out;
}

/** Every Folder: those profiles are in, and the empty ones the registry keeps. Sorted. */
export function folderNamesOf(profiles: ProfileMeta[], registry: string[]): string[] {
  const set = new Set<string>(registry);
  for (const p of profiles) if (p.folder) set.add(p.folder);
  return [...set].sort(FOLDER_COLLATOR.compare);
}

/** A profile bound to a proxy that no longer exists: deleted here, through
 *  the API or in another window. It must never pass for a chosen direct
 *  connection, and the backend would start it on this computer's own IP
 *  (backend B1). A team's row is never so: its proxy may be one this member
 *  cannot see, which comes with the session (`proxyHidden`). */
export function proxyMissing(p: { proxy_id: string | null; origin?: ProfileMeta["origin"] }, proxies: readonly { id: string }[]): boolean {
  return !!p.proxy_id && p.origin !== "team" && !proxies.some((x) => x.id === p.proxy_id);
}

/** A team profile bound to a proxy not on this member's list: the server sends it
 *  with the session on Start (team-server api.rs); one gone the start refuses. */
export function proxyHidden(p: { proxy_id: string | null; origin?: ProfileMeta["origin"] }, proxies: readonly { id: string }[]): boolean {
  return !!p.proxy_id && p.origin === "team" && !proxies.some((x) => x.id === p.proxy_id);
}

/** The proxies two or more of `list` are on (a proxy that is gone is none):
 *  sites can link the profiles behind one IP. */
export function sharedProxyIds(list: readonly ProfileMeta[], proxies: ReadonlyMap<string, unknown>): Set<string> {
  const seen = new Set<string>();
  const shared = new Set<string>();
  for (const p of list) {
    if (!p.proxy_id || !proxies.has(p.proxy_id)) continue;
    if (seen.has(p.proxy_id)) shared.add(p.proxy_id);
    else seen.add(p.proxy_id);
  }
  return shared;
}

/** A profile's search text: its name, notes and tags' names, and its proxy's
 *  name, address and last seen IP, lowercased once per change of its inputs,
 *  so a keystroke is one `includes` per row (PERF-04, IFACE-13). */
export function searchTextOf(
  p: ProfileMeta,
  tagNames: string[],
  proxy?: { name: string; host: string; port: number } | null,
  ip?: string,
): string {
  return [p.name, p.notes ?? "", ...tagNames, proxy?.name ?? "", proxy ? `${proxy.host}:${proxy.port}` : "", ip ?? ""].join("\n").toLowerCase();
}

/// Whether a profile passes the folder tab, the search and the filter bar
/// (the order is separate). Every list of profiles uses it, so they agree.
export function makeProfileFilter(
  proxies: ProxyEntry[],
  search: string,
  folder: string,
  filters: ProfileFilters,
  running: Record<string, number>,
  /** A profile's tags: the search matches their names, the Tag filter their ids. */
  tagsOf: (id: string) => { id: string; name: string }[],
  /** For the Last run filter. */
  now: number,
  /** `searchTextOf` by profile id; without it the search reads the fields. */
  searchText?: ReadonlyMap<string, string>,
  /** Every profile: "shared" counts who else is on a profile's proxy. */
  all: readonly ProfileMeta[] = [],
): (p: ProfileMeta) => boolean {
  const q = search.trim().toLowerCase();
  const byId = new Map(proxies.map((p) => [p.id, p]));
  const onId = proxyIdOf(filters.proxy);
  // On a shared proxy: shared among the profiles the Folder and the other
  // filters keep, the search aside (a search does not change who shares a
  // proxy), as the Shared proxy card counts in its scope.
  const shared = filters.proxy === "shared"
    ? sharedProxyIds(all.filter(makeProfileFilter(proxies, "", folder, { ...filters, proxy: "all" }, running, tagsOf, now)), byId)
    : null;
  return (p) => {
    if (folder !== "all" && p.folder !== folder) return false;
    if (q) {
      const text = searchText?.get(p.id);
      const bound = p.proxy_id ? byId.get(p.proxy_id) : undefined;
      const found = text !== undefined
        ? text.includes(q)
        : p.name.toLowerCase().includes(q) || (p.notes ?? "").toLowerCase().includes(q)
          || tagsOf(p.id).some((t) => t.name.toLowerCase().includes(q))
          || (!!bound && `${bound.name}\n${bound.host}:${bound.port}`.toLowerCase().includes(q));
      // An ID from an API log: its start, from 4 characters (shorter would match noise).
      const byIdPrefix = q.length >= 4 && p.id.toLowerCase().startsWith(q);
      if (!found && !byIdPrefix) return false;
    }
    if (filters.tag && !tagsOf(p.id).some((t) => t.id === filters.tag)) return false;
    if (!lastRunMatches(p.last_launched_at, filters.lastRun, now, p.created_at)) return false;
    // A proxy that is gone counts as none: the "No proxy" View and filter show the
    // profiles that would run on this computer's own IP (IFACE-01).
    const bound = !!p.proxy_id && byId.has(p.proxy_id);
    if (filters.proxy === "bound" && !bound) return false;
    // A protected row's proxy is unknown while it is locked: not one with none.
    if (filters.proxy === "direct" && (bound || p.protected)) return false;
    if (onId !== null && !(bound && p.proxy_id === onId)) return false;
    if (shared && !(bound && shared.has(p.proxy_id!))) return false;
    if (filters.country) {
      const cc = p.proxy_id ? byId.get(p.proxy_id)?.country ?? "" : "";
      if (cc.toUpperCase() !== filters.country.toUpperCase()) return false;
    }
    if (filters.status !== "all") {
      const isRunning = !!running[p.id];
      if ((filters.status === "running") !== isRunning) return false;
    }
    return true;
  };
}

/// One list of profiles, filtered and sorted: the View counts and "would the
/// list hide this one" use it. The Profiles list itself reads the cached
/// `computeVisible` (visible.ts), which applies the same filter and order.
export function applyProfileFilters(
  profiles: ProfileMeta[],
  proxies: ProxyEntry[],
  search: string,
  folder: string,
  filters: ProfileFilters,
  running: Record<string, number> = {},
  sort: ProfileSort = "added",
  /** A profile's tags: the search matches their names, the Tag filter their ids. */
  tagsOf: (id: string) => { id: string; name: string }[] = () => [],
  /** For the Last run filter. */
  now: number = Date.now(),
): ProfileMeta[] {
  // Sorted here rather than in the table, so the rows on screen and the rows a
  // shift-click range covers stay the same list.
  return sortProfiles(profiles.filter(makeProfileFilter(proxies, search, folder, filters, running, tagsOf, now, undefined, profiles)), sort, running, now);
}
