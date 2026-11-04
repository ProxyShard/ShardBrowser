// Personal sync as the backend runs it against a team server (entities/sync/model/types.ts). Test toggles:
// __shardxMockSyncOffline and __shardxMockSyncError at any time; __shardxMockSync = "connected" before load.
import type { MockState, StoredProfile } from "./fixtures";
import { metaOf, syncLink } from "./fixtures";
import type { Ctx, Handler } from "./handlers";
import { personal } from "./spaces";
import { LOCK_REFUSALS } from "../../shared/api/lockRefusals";
import { DEFAULT_KINDS, SYNC_REFUSALS, type ProfileSync, type SyncConnection, type SyncKinds, type SyncState } from "../../entities/sync/model/types";

/** This launcher, as the other devices see it (space_state's device name). */
const DEVICE = "Mock-MacBook";
/** The other device in the seeded space, which holds the pulled profile's lease. */
const OTHER_DEVICE = "Work-PC";
/** The profile that device put on the server: pulled on the first connect, open there. */
const REMOTE_PROFILE = { id: "sync-office-main", name: "Office main" };
/** When that device last checked in: two hours before the connect. */
const OTHER_SEEN_AGO = 2 * 3600;
const SYNC_MS = 1000;

export type MockSync = {
  connection: SyncConnection | null;
  /** The token this launcher holds for its space, for the invite it gives its other computers. */
  token?: string;
  kinds: SyncKinds;
  last_sync_at?: number;
  pending: number;
  /** Profiles picked to sync; each keeps its pick while kinds.profiles is "all". */
  picked: string[];
  /** Profile id to the other device that holds its lease. */
  leases: Record<string, string>;
  /** Profile id to when the device holding it last checked in (unix seconds). */
  seen?: Record<string, number>;
  /** The server's version of a held profile changed here while offline: the original a conflict puts back. */
  base?: Record<string, StoredProfile>;
  /** Conflict copies a sync made. */
  conflicts: string[];
  /** Synced profiles changed while offline. */
  dirty: string[];
  /** What each server keeps of this person's space, by address; `remote`: the other device's profile is there. */
  held: Record<string, { picked: string[]; kinds: SyncKinds; remote: boolean; device_name?: string }>;
};

type Win = { __shardxMockSyncOffline?: boolean; __shardxMockSyncError?: string; __shardxMockSync?: string };
const win = window as unknown as Win;

let current: () => MockState = () => { throw new Error("sync mock not bound"); };
let emit: Ctx["emit"] = () => {};
let save: () => void = () => {};
let syncing = false;
let timer: ReturnType<typeof setTimeout> | undefined;
let offline = !!win.__shardxMockSyncOffline;
let failing: string | undefined = win.__shardxMockSyncError;
/** The last failure's words: shown until a sync gets through. */
let stuck: string | undefined = failing;
/** Up by one on every change, as the backend's own counter. */
let rev = 1;

const nowSecs = () => Math.floor(Date.now() / 1000);
const fresh = (): MockSync => ({ connection: null, kinds: { ...DEFAULT_KINDS }, pending: 0, picked: [], leases: {}, conflicts: [], dirty: [], held: {} });
const syncOf = (s: MockState): MockSync => (s.sync ??= fresh());
const clone = <T,>(v: T): T => JSON.parse(JSON.stringify(v)) as T;

/** A profile's sync as ProfileMeta carries it: absent while not connected, and on a team space's rows. */
function profileSync(s: MockState, id: string): ProfileSync | undefined {
  const m = s.sync;
  if (!m?.connection || s.space?.kind === "team") return undefined;
  const on = m.kinds.profiles === "all" || m.picked.includes(id);
  if (!on) return { on: false };
  const lease = m.leases[id];
  const seen = m.seen?.[id];
  return { on, ...(lease ? { open_on: lease, ...(seen !== undefined ? { seen_at: seen } : {}) } : {}), ...(m.conflicts.includes(id) ? { conflict: true } : {}) };
}

export function syncState(s: MockState): SyncState {
  const m = s.sync;
  if (!m?.connection) return { rev, connection: null, kinds: m?.kinds ?? { ...DEFAULT_KINDS }, status: { state: "idle", pending: 0 } };
  const error = failing ?? stuck;
  const state = syncing ? "syncing" : error ? "error" : offline ? "offline" : "idle";
  return {
    rev,
    connection: m.connection,
    kinds: m.kinds,
    status: { state, pending: m.pending, ...(m.last_sync_at !== undefined ? { last_sync_at: m.last_sync_at } : {}), ...(state === "error" ? { error } : {}) },
  };
}

const announce = (profiles = false) => {
  rev++;
  save();
  emit("sync-changed", syncState(current()));
  if (profiles) emit("store-changed", "profiles");
};

/// A held profile changed both here (offline) and on the device holding it: the
/// edit made here goes to a "(conflict copy)", the original back to the server's version.
function conflictCopy(state: MockState, id: string): boolean {
  // Sync is the person's own space, whichever space is in use.
  const s = personal(state);
  const local = s.profiles[id];
  if (!local) return false;
  const m = syncOf(state);
  const copy = clone(local);
  const cid = `${id}-conflict`;
  copy._meta = { ...copy._meta, id: cid, rev: 1, pinned: false };
  copy.name = `${local.name} (conflict copy)`;
  s.profiles[cid] = copy;
  if (!s.order.includes(cid)) s.order.splice(s.order.indexOf(id) + 1, 0, cid);
  const server = m.base?.[id];
  if (server) s.profiles[id] = { ...clone(server), _meta: { ...clone(server._meta), rev: local._meta.rev + 1 } };
  if (m.base) delete m.base[id];
  if (!m.picked.includes(cid)) m.picked.push(cid);
  if (!m.conflicts.includes(cid)) m.conflicts.push(cid);
  return true;
}

/** Runs a sync: "syncing" now, the end a moment later. Nothing while offline or failing. */
function runSync() {
  const s = current();
  if (!s.sync?.connection || offline || failing) return;
  syncing = true;
  announce();
  if (timer) clearTimeout(timer);
  timer = setTimeout(() => {
    timer = undefined;
    syncing = false;
    const m = current().sync;
    if (!m?.connection) return;
    if (offline || failing) { announce(); return; }
    let copies = false;
    for (const id of m.dirty) if (m.leases[id]) copies = conflictCopy(current(), id) || copies;
    m.dirty = [];
    m.pending = 0;
    m.last_sync_at = nowSecs();
    stuck = undefined;
    announce(copies);
  }, SYNC_MS);
}

/// A change here to synced profiles (an edit, a session closed): queued while
/// offline, else synced. `before`: each profile as it was, the server's version.
export function syncTouched(s: MockState, ids: string[], before: Record<string, StoredProfile> = {}): void {
  const m = s.sync;
  if (!m?.connection) return;
  const synced = ids.filter((id) => profileSync(s, id)?.on);
  if (synced.length === 0) return;
  if (offline || failing) {
    m.pending += synced.length;
    for (const id of synced) {
      if (!m.dirty.includes(id)) m.dirty.push(id);
      // The first change to a held profile keeps the server's version, for the conflict.
      if (m.leases[id] && before[id] && !m.base?.[id]) (m.base ??= {})[id] = clone(before[id]);
    }
    announce();
  } else runSync();
}

/** The device that holds a synced profile's lease, or null. */
export const syncLease = (s: MockState, id: string): string | null => profileSync(s, id)?.open_on ?? null;

/** What the other device put on the server: one profile, open there. */
function pullRemote(state: MockState) {
  const m = syncOf(state);
  const s = personal(state);
  const { id, name } = REMOTE_PROFILE;
  if (!s.profiles[id]) {
    const base = s.order.map((x) => s.profiles[x]).find(Boolean);
    if (!base) return;
    const p = clone(base);
    // Its assigned proxy comes with it, as a synced profile's always does.
    p._meta = { ...p._meta, id, rev: 1, pinned: false, folder: "", last_launched_at: `@${nowSecs() - 3600}`, total_runtime_ms: 5_400_000, launches: 12, traffic_bytes: 380_000_000, extensions: [] };
    delete p._meta.color;
    p.name = name;
    p.notes = "";
    s.profiles[id] = p;
    s.order.unshift(id);
  }
  if (!m.picked.includes(id)) m.picked.push(id);
  m.leases[id] = OTHER_DEVICE;
  (m.seen ??= {})[id] = nowSecs() - OTHER_SEEN_AGO;
}

function connectTo(s: MockState, conn: SyncConnection, token: string) {
  const m = syncOf(s);
  const kept = m.held[conn.server] ?? { picked: [], kinds: { ...DEFAULT_KINDS }, remote: true };
  m.connection = { ...conn, device_name: kept.device_name ?? conn.device_name };
  m.token = token;
  m.kinds = { ...kept.kinds };
  m.picked = kept.picked.filter((id) => personal(s).profiles[id]);
  m.leases = {};
  m.seen = {};
  m.base = {};
  m.conflicts = [];
  m.dirty = [];
  m.pending = 0;
  if (kept.remote) pullRemote(s);
}

/** Wires the mock to the app's state and events (install.ts); starts connected when a test asks. */
export function bindSync(getState: () => MockState, emitFn: Ctx["emit"], saveFn: () => void) {
  current = getState;
  emit = emitFn;
  save = saveFn;
  syncLink.of = (id) => profileSync(current(), id);
  const s = getState();
  if (win.__shardxMockSync === "connected" && !s.sync?.connection) {
    connectTo(s, { server: "http://sync.example.net:8787", teamId: "team-sync.example.net:8787", own: true, device_name: DEVICE }, "eyJhbGciOiJIUzI1NiJ9.mock.admin");
    syncOf(s).last_sync_at = nowSecs() - 120;
  }
  // A test flips these at any moment; the app hears it as the backend would say it.
  Object.defineProperty(window, "__shardxMockSyncOffline", {
    configurable: true,
    get: () => offline,
    set: (v: unknown) => {
      const was = offline;
      offline = !!v;
      if (was && !offline) runSync();
      announce();
    },
  });
  Object.defineProperty(window, "__shardxMockSyncError", {
    configurable: true,
    get: () => failing,
    // Cleared, the cause is gone; the status keeps saying it until Retry gets through.
    set: (v: unknown) => { failing = typeof v === "string" && v ? v : undefined; if (failing) stuck = failing; announce(); },
  });
}

const KIND_KEYS: (keyof SyncKinds)[] = ["profiles", "proxies", "folders_tags", "bookmarks", "extensions", "automation"];

function connected(s: MockState): MockSync {
  const m = syncOf(s);
  if (!m.connection) throw SYNC_REFUSALS.notConnected;
  return m;
}

const hostOf = (url: string) => { try { return new URL(url).host; } catch { return url; } };

export const syncHandlers: Record<string, Handler> = {
  sync_state: (_a, { state }) => syncState(state),
  // As team.rs words a refused join: a member token is "stm_…", an admin token "eyJ…".
  sync_connect: (args, ctx) => {
    const m = syncOf(ctx.state);
    if (m.connection) throw SYNC_REFUSALS.alreadyConnected;
    let conn: SyncConnection;
    let token: string;
    if (typeof args.teamId === "string") {
      const team = (ctx.state.teams ?? []).find((x) => x.id === args.teamId && !x.revoked);
      if (!team) throw SYNC_REFUSALS.notMember;
      conn = { server: team.server, teamId: team.id, own: team.admin, device_name: DEVICE };
      // A token for this person's own space there, which the server issues.
      token = team.admin ? "eyJhbGciOiJIUzI1NiJ9.mock.admin" : `stm_sync_${ctx.state.teamServers?.[team.id]?.me ?? "member"}`;
    } else {
      const url = String(args.server ?? "").trim();
      const tok = String(args.token ?? "").trim();
      let u: URL | null = null;
      try { u = new URL(url); } catch { /* not an address */ }
      if (!u || !/^https?:$/.test(u.protocol) || !u.host) throw "not a server address: give it as http://host:port";
      const own = tok.startsWith("eyJ");
      if (!own && !tok.startsWith("stm_")) throw "the server does not know this token";
      if (offline) throw SYNC_REFUSALS.noAnswer(u.host);
      conn = { server: url, teamId: `team-${u.host}`, own, device_name: DEVICE };
      token = tok;
    }
    connectTo(ctx.state, conn, token);
    runSync();
    announce(true);
    return syncState(ctx.state);
  },
  sync_disconnect: ({ removeFromServer }, ctx) => {
    const m = connected(ctx.state);
    const server = m.connection!.server;
    // Removing needs the server; keeping is this computer's own business.
    if (removeFromServer && offline) throw SYNC_REFUSALS.noAnswer(hostOf(server));
    m.held[server] = removeFromServer
      ? { picked: [], kinds: { ...DEFAULT_KINDS }, remote: false }
      : { picked: [...m.picked], kinds: { ...m.kinds }, remote: true, device_name: m.connection!.device_name };
    if (timer) clearTimeout(timer);
    timer = undefined;
    syncing = false;
    stuck = undefined;
    Object.assign(m, { connection: null, picked: [], leases: {}, seen: {}, base: {}, conflicts: [], dirty: [], pending: 0 });
    delete m.token;
    delete m.last_sync_at;
    announce(true);
    return syncState(ctx.state);
  },
  sync_set_kinds: ({ kinds }, ctx) => {
    const m = connected(ctx.state);
    const next = kinds as SyncKinds;
    if (!next || KIND_KEYS.some((k) => !(k in next))) throw "kinds: every kind is needed";
    const profilesChanged = next.profiles !== m.kinds.profiles;
    m.kinds = { ...next };
    if (offline || failing) { m.pending += 1; announce(profilesChanged); }
    else { runSync(); announce(profilesChanged); }
    return syncState(ctx.state);
  },
  sync_now: (_a, ctx) => {
    const m = connected(ctx.state);
    if (offline) throw SYNC_REFUSALS.noAnswer(hostOf(m.connection!.server));
    if (failing) throw failing;
    runSync();
    return syncState(ctx.state);
  },
  sync_invite: (_a, ctx) => {
    const m = connected(ctx.state);
    return { invite: `${m.connection!.server}#${m.token ?? ""}` };
  },
  sync_set_device_name: ({ name }, ctx) => {
    const m = connected(ctx.state);
    const next = String(name ?? "").trim();
    if (next.length < 1 || next.length > 64) throw SYNC_REFUSALS.deviceName;
    m.connection = { ...m.connection!, device_name: next };
    announce();
    return syncState(ctx.state);
  },
  profile_sync_set: ({ ids, on }, ctx) => {
    const m = connected(ctx.state);
    const list = (ids as string[]).map(String);
    for (const id of list) if (!ctx.state.profiles[id]) throw `profile ${id} not found`;
    // A protected one stays on this computer (#127).
    if (on && list.some((id) => ctx.state.profiles[id]._meta.lock)) throw LOCK_REFUSALS.notAvailable;
    // Another device holds it: stopping here would let both run it.
    if (!on) for (const id of list) if (m.leases[id]) throw SYNC_REFUSALS.leasedStop(m.leases[id]);
    for (const id of list) {
      if (on) { if (!m.picked.includes(id)) m.picked.push(id); continue; }
      m.picked = m.picked.filter((x) => x !== id);
      // While "all" is on only the pick changes: it still syncs.
      if (m.kinds.profiles === "all") continue;
      m.conflicts = m.conflicts.filter((x) => x !== id);
      m.dirty = m.dirty.filter((x) => x !== id);
    }
    if (offline || failing) { m.pending += list.length; announce(); } else runSync();
    save();
    return list.map((id) => metaOf(ctx.state.profiles[id]));
  },
  profile_sync_take: ({ id }, ctx) => {
    const m = connected(ctx.state);
    const pid = String(id);
    if (!ctx.state.profiles[pid]) throw `profile ${pid} not found`;
    if (offline) throw SYNC_REFUSALS.noAnswer(hostOf(m.connection!.server));
    // The other device closes it at its next sync; this launcher holds it now.
    delete m.leases[pid];
    if (m.seen) delete m.seen[pid];
    announce(true);
    return metaOf(ctx.state.profiles[pid]);
  },
};
