// Spaces as team.rs and space/ keep them: the lists in use sit in the state's own fields, the others
// in `spaceData`; the edition and each team's reach live in memory, so a reload is a restart.
import type { MockState, StoredProfile } from "./fixtures";
import type { Ctx } from "./handlers";
import type { FingerprintEntry } from "../../entities/fingerprint/model/types";
import type { SpaceState, TeamStatus } from "../../entities/space/model/types";
import { EPOCH_WRITES, SPACE_REFUSALS } from "../../shared/api/epochWrites";
import { TEAM_REFUSALS } from "../../shared/api/teamRefusals";
import { TEAM_WARNINGS } from "../../shared/api/backendWarnings";
import { openSessions, sessionEnded } from "./teamSessions";

/** A write a team's space does not take (space/mod.rs NOT_IN_TEAM). */
export const NOT_IN_TEAM = TEAM_REFUSALS.notInTeam;

/** The lists each space has of its own. */
const OWN = ["profiles", "order", "proxies", "snapshots", "extensions", "bookmarks", "automation", "tags", "folders"] as const;
export type SpaceData = Pick<MockState, (typeof OWN)[number]>;

/** Every list a switch tells the pages to read again (cache.rs TOPICS), in its order. */
const TOPICS = ["profiles", "proxies", "folders", "bookmarks", "extensions", "automation", "tags"];

type Space = NonNullable<MockState["space"]>;
const PERSONAL: Space = { kind: "personal" };
const nowSecs = () => Math.floor(Date.now() / 1000);
const keyOf = (sp: Space) => (sp.kind === "team" ? sp.team_id : "personal");
/** A launcher date ("@<unix seconds>" or ISO) as the server's unix seconds; null when there is none. */
const secsOf = (stamp: string | null): number | null => {
  const ms = !stamp ? NaN : stamp.startsWith("@") ? Number(stamp.slice(1)) * 1000 : Date.parse(stamp);
  return Number.isFinite(ms) && ms > 0 ? Math.floor(ms / 1000) : null;
};

/** Up by one each time the space in use changes (team.rs SPACE_EPOCH). */
let epoch = 1;
/** Each team's reach, as its last read of the lists found it. */
const reach: Record<string, TeamStatus> = {};

type Win = { __shardxMockTeamStatus?: Partial<TeamStatus> };

/** The space in use, as space_state answers it: personal when its team is gone or shut. */
export function spaceInUse(state: MockState): Space {
  const sp = state.space ?? PERSONAL;
  if (sp.kind !== "team") return PERSONAL;
  const t = (state.teams ?? []).find((x) => x.id === sp.team_id);
  return t && !t.revoked && !t.needs_invite ? sp : PERSONAL;
}

/** The team whose lists are in the state's own fields, or null. */
export function teamInUse(state: MockState): string | null {
  const sp = spaceInUse(state);
  return sp.kind === "team" ? sp.team_id : null;
}

/** A team's reach; on first asking, what entering it found. `__shardxMockTeamStatus`, set
 *  before, is that answer as given (no `synced_at`: nothing was ever read). */
function statusOf(team: string): TeamStatus {
  if (!reach[team]) {
    const asked = (window as Win).__shardxMockTeamStatus;
    reach[team] = asked?.state ? { ...asked } as TeamStatus : { state: "ready", synced_at: nowSecs() };
  }
  return reach[team];
}

/** A profile made from a library fingerprint, in `folder`. */
export function profileFromFingerprint(fp: FingerprintEntry | undefined, name: string, folder = ""): StoredProfile {
  const nav = { ...(fp?.payload?.navigator ?? {}) } as Record<string, unknown>;
  const stamp = `@${nowSecs()}`;
  return {
    _meta: {
      id: Math.random().toString(16).slice(2, 10), rev: 1, proxy_id: null, last_launched_at: null, created_at: stamp,
      pinned: false, folder, total_runtime_ms: 0, gpu_preset_id: fp?.id ?? "", extensions: [],
      ...(fp?.platform.startsWith("Linux arm") ? { android_media: true } : {}),
    },
    name,
    notes: "",
    timezone: "auto",
    icu_locale: null,
    webrtc: "auto",
    navigator: { ...nav, do_not_track: null },
    client_hints: { ...(fp?.payload?.client_hints ?? {}) },
    screen: { ...(fp?.payload?.screen ?? {}) },
    media_devices: { audio_input_count: 1, audio_output_count: 1, video_input_count: 1 },
    geolocation: { mode: "auto" },
    noise: {
      canvas: { enabled: true, seed: 0 }, webgl: { enabled: true, seed: 0, intensity: 0.3 },
      audio: { enabled: true, seed: 0 }, client_rects: { enabled: false, seed: 0, max_offset: 0 },
      sensors: { enabled: false, seed: 0 }, fonts: { enabled: false, seed: 0 },
    },
    blocked_ports: [3389, 5900, 5901, 5800, 7070, 6568, 5938, 1080, 8080, 3128, 3030],
  };
}

/** A team's lists as its server first hands them over: its profiles, nothing else yet. */
function teamSeed(state: MockState, team: string): SpaceData {
  const rows = state.teamServers?.[team]?.profiles ?? [];
  const fps = state.fingerprints;
  const profiles: Record<string, StoredProfile> = {};
  rows.forEach((r, i) => {
    const p = profileFromFingerprint(fps[i % Math.max(1, fps.length)], r.name, r.folder);
    p._meta.id = r.id;
    // The list's date is the server's row (remote.rs meta); a state saved before it had none.
    p._meta.created_at = typeof r.created_at === "number" ? `@${r.created_at}` : null;
    profiles[r.id] = p;
  });
  return {
    profiles, order: rows.map((r) => r.id), proxies: [], snapshots: {}, extensions: [], bookmarks: [], automation: [],
    tags: { tags: [], by_profile: {} }, folders: undefined,
  };
}

const emptyData = (): SpaceData => ({
  profiles: {}, order: [], proxies: [], snapshots: {}, extensions: [], bookmarks: [], automation: [], tags: undefined, folders: undefined,
});
const takeOwn = (state: MockState): SpaceData => Object.fromEntries(OWN.map((k) => [k, state[k]])) as SpaceData;

/** Puts the lists of the space in use in place; false when they already were.
 *  A change of space is a new edition, and entering a team reads its lists. */
export function settle(state: MockState, emit?: Ctx["emit"]): boolean {
  const want = keyOf(spaceInUse(state));
  const have = state.dataOf ?? "personal";
  if (want === have) return false;
  const kept = (state.spaceData ??= {});
  kept[have] = takeOwn(state);
  // As cache.rs keep(): entering a team reads its lists, and the status that moved is told.
  const was = reach[want];
  delete reach[want];
  const now = want === "personal" ? null : statusOf(want);
  // A server that never answered gave nothing to show: its cache is empty.
  const never = !!now && now.state !== "ready" && now.synced_at === undefined;
  if (never) unread.add(want);
  const next = kept[want] ?? (want === "personal" || never ? emptyData() : teamSeed(state, want));
  delete kept[want];
  Object.assign(state, next);
  if (!next.folders) delete state.folders;
  if (!next.tags) delete state.tags;
  state.dataOf = want;
  epoch++;
  if (now && emit && (was?.state !== now.state || was?.error !== now.error)) emit("team-status", { ...now });
  return true;
}

/** Teams entered while their server never answered: their lists come with its first answer. */
const unread = new Set<string>();

/** Why a team's start cannot be judged now (open.rs `passes` reads the server first), or null. */
export function teamUnreachable(state: MockState): string | null {
  const team = teamInUse(state);
  const r = team ? statusOf(team) : null;
  return r?.state === "offline" ? `the team server did not answer: ${r.error ?? "no answer"}` : null;
}

/** Why `team`'s server cannot answer a read now (space/admin.rs reads it), or null. */
export function teamReadRefusal(team: string): string | null {
  const r = reach[team];
  return r?.state === "offline" ? `the team server did not answer: ${r.error ?? "no answer"}` : null;
}

/** The person's own profiles, wherever they wait while a team is in use. */
export function personal(state: MockState): Pick<MockState, "profiles" | "order"> {
  const have = state.dataOf ?? "personal";
  return have === "personal" ? state : state.spaceData?.personal ?? state;
}

/** Each of a team's profiles, in use or waiting. */
export function eachTeamProfile(state: MockState, team: string, fn: (p: StoredProfile) => void): void {
  const lists = (state.dataOf ?? "personal") === team ? state : state.spaceData?.[team];
  for (const p of Object.values(lists?.profiles ?? {})) fn(p);
}

/** The team server's own list follows the team's profiles as they are written here. */
export function mirror(state: MockState): void {
  const team = state.dataOf;
  const server = team && team !== "personal" ? state.teamServers?.[team] : undefined;
  // Nothing was read of it yet: the empty lists here are not the server's.
  if (!server || unread.has(team!)) return;
  server.profiles = state.order.filter((id) => state.profiles[id]).map((id) => {
    const p = state.profiles[id];
    return { id, name: p.name, folder: p._meta.folder, created_at: secsOf(p._meta.created_at) };
  });
}

/** The refusal a write gets before anything is written (space::window_ticket, then the team's reach). */
export function refusal(state: MockState, cmd: string, args: Record<string, unknown>): string | null {
  if (!EPOCH_WRITES.has(cmd)) return null;
  const asked = args.epoch;
  if (typeof asked === "number" && asked !== epoch) return SPACE_REFUSALS.spaceChanged;
  const team = teamInUse(state);
  if (!team) return null;
  if (typeof asked !== "number") return SPACE_REFUSALS.noEpoch;
  const r = statusOf(team);
  if (r.state === "offline") return `the team server did not answer: ${r.error ?? "no answer"}`;
  return null;
}

/** What space_state answers. */
export function spaceStateOf(state: MockState): SpaceState {
  settle(state);
  const space = spaceInUse(state);
  const teams = (state.teams ?? []).map((t) => {
    const me = state.teamServers?.[t.id]?.me;
    return { member_id: t.admin ? "admin" : me ?? "", perm_key: "mock-perm-key", ...t };
  });
  return {
    space, teams, device_name: "Mock-MacBook", epoch,
    ...(space.kind === "team" ? { team_status: { ...statusOf(space.team_id) } } : {}),
  };
}

/** As settle() and lib.rs end a switch: every list is read again, then the space is told. */
export function told(state: MockState, emit: Ctx["emit"]): void {
  for (const topic of TOPICS) emit("store-changed", topic);
  emit("space-changed", spaceInUse(state));
}

/** Test knob, as live.rs moves the launcher: `tell: false` keeps the window from hearing of it. */
export function moveSpace(state: MockState, space: Space, emit: Ctx["emit"], tell = true): void {
  state.space = space;
  settle(state, tell ? emit : undefined);
  if (tell) told(state, emit);
}

/** Test knob: the team in use's reach as a read of its lists finds it, told as cache.rs does. */
export function setTeamStatus(state: MockState, next: Partial<TeamStatus>, emit: Ctx["emit"]): void {
  const team = teamInUse(state);
  if (!team) return;
  const was = statusOf(team);
  const ready = next.state === "ready";
  const now: TeamStatus = {
    state: next.state ?? was.state,
    ...(ready ? {} : next.error ? { error: next.error } : {}),
    ...(ready ? { synced_at: nowSecs() } : was.synced_at !== undefined ? { synced_at: was.synced_at } : {}),
    ...(next.synced_at !== undefined ? { synced_at: next.synced_at } : {}),
  };
  reach[team] = now;
  if (was.state !== now.state || was.error !== now.error) emit("team-status", { ...now });
  // Its first answer: the lists it never gave come now, and every page reads them.
  if (ready && unread.delete(team)) {
    Object.assign(state, teamSeed(state, team));
    for (const topic of TOPICS) emit("store-changed", topic);
  }
}

/** Test knob, as live.rs `revoke` and `replaced`: the team in use's profiles open here close
 *  (unsaved, or kept for the new invite), the launcher leaves it, and the warning says why. */
export function withdraw(state: MockState, kind: "revoked" | "replaced", emit: Ctx["emit"]): void {
  const team = teamInUse(state);
  const link = (state.teams ?? []).find((t) => t.id === team);
  if (!team || !link) return;
  for (const s of openSessions(state)) {
    if (s.mine || s.team_id !== team) continue;
    delete state.running[s.profile_id];
    sessionEnded(state, s.profile_id, kind === "revoked" ? "discarded" : "kept", emit);
  }
  if (kind === "revoked") link.revoked = true;
  else link.needs_invite = true;
  settle(state, emit);
  emit("space-changed", spaceInUse(state));
  emit("launcher-warning", kind === "revoked" ? TEAM_WARNINGS.withdrew(link.name) : TEAM_WARNINGS.replaced(link.name));
}

/** The edition now, for tests. */
export const spaceEpoch = () => epoch;
