// The team server behind the team admin commands, as team-server/src/api.rs answers.
// One per joined team, in MockState.teamServers; a throw is the refusal as the Rust command words it.
import type { MockState } from "./fixtures";
import type { Ctx, Handler } from "./handlers";
import { newId } from "./store";
import { eachTeamProfile, teamReadRefusal } from "./spaces";
import type { Grant, TeamAction } from "../../entities/team/model/types";
import type { MockTrashRow } from "./teamTrash";
import { decide, mergeGrants, sortGrants } from "../../entities/team/lib/decide";

/** `last_seen_at`: null until their invite is used; `epoch` moves with their rights and switch. */
export type MockTeamMember = {
  id: string; name: string; active: boolean; created_at: number; epoch: number; last_seen_at: number | null; grants: Grant[]; token: string;
};
export type MockTeamServer = {
  name: string;
  /** The member this launcher joined as; null for the admin token. */
  me: string | null;
  members: MockTeamMember[];
  folders: { name: string; color: string }[];
  /** `created_at`: unix seconds, null for a row an older server kept (store.rs). */
  profiles: { id: string; name: string; folder: string; created_at: number | null }[];
  /** What was deleted in the team, 30 days (teamTrash.ts); a profile's folder cannot go while it waits. */
  trash?: MockTrashRow[];
  /** Profiles another device has open now, by id: its lease (unix seconds). */
  leases?: Record<string, { device_name: string; member_name: string; since: number }>;
};

const ACTIONS: readonly TeamAction[] = ["see", "start", "edit", "create", "delete", "cookies", "take_out"];
const MAX_LABEL = 200;
const MAX_TEAM_NAME = 64;
const day = (iso: string) => Math.floor(Date.parse(iso) / 1000);
const nowSecs = () => Math.floor(Date.now() / 1000);
const allow = (scope: Grant["scope"], action: TeamAction): Grant => ({ scope, action, allow: true });
const refuse = (scope: Grant["scope"], action: TeamAction): Grant => ({ scope, action, allow: false });

/** A team that has been running a while: three members, three folders, eight profiles. */
export function seedTeamServer(name: string): MockTeamServer {
  // Seen relative to the join, so the tooltip reads the same in every run.
  const ago = (secs: number) => nowSecs() - secs;
  const days = (n: number) => ago(n * 86_400);
  const team = { kind: "team" } as const;
  const shops = { kind: "folder", name: "Shops" } as const;
  const ads = { kind: "folder", name: "Ads" } as const;
  return {
    name,
    me: null,
    members: [
      {
        id: "m-dana", name: "Dana Kim", active: true, created_at: day("2026-08-03T09:00:00Z"), epoch: 3, last_seen_at: ago(5 * 60), token: "stm_mock_dana",
        grants: [allow(team, "see"), allow(team, "start"), allow(shops, "edit"), allow(shops, "cookies"), refuse({ kind: "profile", id: "tp-ads-2" }, "start")],
      },
      {
        id: "m-lee", name: "Lee Park", active: true, created_at: day("2026-08-12T09:00:00Z"), epoch: 2, last_seen_at: ago(2 * 86_400), token: "stm_mock_lee",
        grants: [allow(team, "see"), allow(ads, "start"), allow(ads, "edit"), allow(ads, "create")],
      },
      {
        id: "m-sam", name: "Sam Ortiz", active: false, created_at: day("2026-07-20T09:00:00Z"), epoch: 2, last_seen_at: day("2026-07-28T09:00:00Z"),
        token: "stm_mock_sam", grants: [],
      },
    ],
    folders: [{ name: "Ads", color: "blue" }, { name: "Shops", color: "green" }, { name: "Warm-up", color: "orange" }],
    profiles: [
      { id: "tp-ads-1", name: "FB Ads 01", folder: "Ads", created_at: days(22) },
      { id: "tp-ads-2", name: "FB Ads 02", folder: "Ads", created_at: days(18) },
      { id: "tp-ads-3", name: "Google Ads 01", folder: "Ads", created_at: days(4) },
      { id: "tp-shop-1", name: "Etsy main", folder: "Shops", created_at: days(27) },
      { id: "tp-shop-2", name: "Etsy backup", folder: "Shops", created_at: days(25) },
      { id: "tp-shop-3", name: "Amazon seller", folder: "Shops", created_at: days(11) },
      { id: "tp-misc-1", name: "Shared sandbox", folder: "", created_at: days(29) },
      { id: "tp-misc-2", name: "Reddit 01", folder: "", created_at: days(1) },
    ],
  };
}

/** Joining: the team's server, made on the first join; a member token says who this launcher is.
 *  A server first met through a member's invite knows that token (its admin made it elsewhere). */
export function joinTeamServer(state: MockState, teamId: string, name: string, token: string, admin: boolean): void {
  const servers = (state.teamServers ??= {});
  const fresh = !servers[teamId];
  const server = (servers[teamId] ??= seedTeamServer(name));
  if (admin) { server.me = null; return; }
  let m = server.members.find((x) => x.token === token);
  if (!m && fresh) {
    m = { id: `m-${newId()}`, name: "Alex Moore", active: true, created_at: nowSecs(), epoch: 1, last_seen_at: null, grants: [], token };
    server.members.push(m);
  }
  // As team.rs join_refused words what /me answers.
  if (!m) throw "the server does not know this token";
  if (!m.active) throw "this member is switched off on the server";
  // Their invite is used: the admin's list shows them active from now.
  m.last_seen_at = nowSecs();
  server.me = m.id;
}

function link(state: MockState, teamId: unknown) {
  const l = (state.teams ?? []).find((t) => t.id === teamId);
  if (!l) throw "not a member of that team";
  return l;
}

function serverOf(state: MockState, teamId: string): MockTeamServer {
  const l = link(state, teamId);
  return ((state.teamServers ??= {})[teamId] ??= seedTeamServer(l.name));
}

/** The server for an admin route: anyone else is refused, as `require_admin` does. */
function adminServer(state: MockState, teamId: unknown): MockTeamServer {
  if (!link(state, teamId).admin) throw "not allowed";
  return serverOf(state, String(teamId));
}

// A state saved before these fields reads as the server's defaults.
const memberOut = (m: MockTeamMember) => ({
  id: m.id, name: m.name, active: m.active, created_at: m.created_at, epoch: m.epoch ?? 1, last_seen_at: m.last_seen_at ?? null,
});
const folderOut = (s: MockTeamServer, f: { name: string; color: string }) => ({ ...f, profiles: s.profiles.filter((p) => p.folder === f.name).length });

function member(s: MockTeamServer, id: unknown): MockTeamMember {
  const m = s.members.find((x) => x.id === id);
  if (!m) throw "no such thing";
  return m;
}

function folder(s: MockTeamServer, name: unknown) {
  const f = s.folders.find((x) => x.name === name);
  if (!f) throw "no such thing";
  return f;
}

/** A folder's name as the server takes it: trimmed, not blank, not too long. */
function folderName(raw: unknown): string {
  const name = String(raw ?? "").trim();
  if ([...name].length > MAX_LABEL) throw "a name or label is too long";
  if (!name) throw "a folder needs a name";
  return name;
}

function label(raw: unknown): string {
  const s = String(raw ?? "");
  if ([...s].length > MAX_LABEL) throw "a name or label is too long";
  return s;
}

/** A grant list as serde reads it: an unknown action or scope is refused whole. */
function grantsArg(raw: unknown): Grant[] {
  if (!Array.isArray(raw)) throw "the request is not what this route takes";
  return raw.map((g) => {
    const action = g?.action as TeamAction;
    if (!ACTIONS.includes(action)) throw "the request is not what this route takes";
    const sc = g?.scope;
    const scope: Grant["scope"] = sc?.kind === "team" ? { kind: "team" }
      : sc?.kind === "folder" && typeof sc.name === "string" ? { kind: "folder", name: sc.name }
      : sc?.kind === "profile" && typeof sc.id === "string" ? { kind: "profile", id: sc.id }
      : (() => { throw "the request is not what this route takes"; })();
    return { scope, action, allow: g?.allow === true };
  });
}

const inviteOf = (state: MockState, teamId: string, token: string) => `${link(state, teamId).server}#${token}`;
const copyRules = (gs: Grant[]) => gs.map((g) => ({ ...g, scope: { ...g.scope } }));
/** A rule list as the store keeps it: one per cell, a refusal over an allowance, in `sort_rules` order. */
const stored = (gs: Grant[]) => sortGrants(mergeGrants(gs), ACTIONS);

/** The rules a member's permission carries: only about what they may See (store.rs `in_view`). */
function inView(s: MockTeamServer, grants: Grant[]): Grant[] {
  const sees = (profileId: string, folder: string) => decide(grants, "see", { profileId, folder }).kind === "allowed";
  return grants.filter((g) => {
    if (g.scope.kind === "team") return true;
    if (g.scope.kind === "folder") {
      const name = g.scope.name;
      return decide(grants, "see", { folder: name }).kind === "allowed" || s.profiles.some((p) => p.folder === name && sees(p.id, name));
    }
    const id = g.scope.id;
    const p = s.profiles.find((x) => x.id === id);
    return !!p && sees(p.id, p.folder);
  });
}
const newToken = () => `stm_${newId()}${newId()}`;

function saved(ctx: Ctx) {
  ctx.save();
}

export const teamHandlers: Record<string, Handler> = {
  team_admin_overview: ({ teamId }, { state }) => {
    const s = adminServer(state, teamId);
    return {
      team: { id: String(teamId), name: s.name },
      members: s.members.map(memberOut),
      folders: s.folders.map((f) => folderOut(s, f)),
      profiles: s.profiles.map((p) => ({ ...p })),
      grants: Object.fromEntries(s.members.map((m) => [m.id, copyRules(m.grants)])),
    };
  },
  team_admin_member_create: ({ teamId, name }, ctx) => {
    const s = adminServer(ctx.state, teamId);
    const n = String(name ?? "").trim();
    if (!n) throw "a member needs a name";
    const m: MockTeamMember = { id: `m-${newId()}`, name: n, active: true, created_at: nowSecs(), epoch: 1, last_seen_at: null, grants: [], token: newToken() };
    s.members.push(m);
    saved(ctx);
    return { member: memberOut(m), invite: inviteOf(ctx.state, String(teamId), m.token) };
  },
  team_admin_member_set_active: ({ teamId, memberId, active }, ctx) => {
    const m = member(adminServer(ctx.state, teamId), memberId);
    m.active = active === true;
    m.epoch = (m.epoch ?? 1) + 1;
    saved(ctx);
    return memberOut(m);
  },
  team_admin_member_delete: ({ teamId, memberId }, ctx) => {
    const s = adminServer(ctx.state, teamId);
    member(s, memberId);
    // What they made stays in the team: it was never theirs.
    s.members = s.members.filter((m) => m.id !== memberId);
    saved(ctx);
    return null;
  },
  team_admin_member_reissue: ({ teamId, memberId }, ctx) => {
    const m = member(adminServer(ctx.state, teamId), memberId);
    m.token = newToken();
    m.last_seen_at = null;
    saved(ctx);
    return { member: memberOut(m), invite: inviteOf(ctx.state, String(teamId), m.token) };
  },
  // GET /admin/members/:id/grants.
  team_admin_grants: ({ teamId, memberId }, { state }) => copyRules(stored(member(adminServer(state, teamId), memberId).grants)),
  // PUT /admin/members/:id/grants, then the list read back.
  team_admin_set_grants: ({ teamId, memberId, grants }, ctx) => {
    const m = member(adminServer(ctx.state, teamId), memberId);
    m.grants = stored(grantsArg(grants));
    m.epoch = (m.epoch ?? 1) + 1;
    saved(ctx);
    return copyRules(m.grants);
  },
  team_admin_rename: ({ teamId, name }, ctx) => {
    const s = adminServer(ctx.state, teamId);
    const n = String(name ?? "").trim();
    if (!n || [...n].length > MAX_TEAM_NAME) throw "a team name is 1 to 64 characters";
    s.name = n;
    // The launcher's own link takes the name too: the space menu shows it.
    link(ctx.state, teamId).name = n;
    saved(ctx);
    ctx.emit("space-changed", ctx.state.space);
    return { id: String(teamId), name: n };
  },
  team_admin_folder_create: ({ teamId, name, color }, ctx) => {
    const s = adminServer(ctx.state, teamId);
    const n = folderName(name);
    const c = label(color ?? "");
    if (s.folders.some((f) => f.name === n)) throw "a folder of that name is already there";
    const f = { name: n, color: c };
    s.folders.push(f);
    saved(ctx);
    return folderOut(s, f);
  },
  team_admin_folder_rename: ({ teamId, name, next }, ctx) => {
    const s = adminServer(ctx.state, teamId);
    const f = folder(s, name);
    const to = folderName(next);
    if (to !== f.name) {
      // A name its profiles or the trash still carry is taken too.
      if (s.folders.some((x) => x.name === to) || s.profiles.some((p) => p.folder === to) || (s.trash ?? []).some((p) => p.folder === to)) {
        throw "a folder of that name is already there";
      }
      const was = f.name;
      // The rules that named it, and its profiles, move with the name; rules already on the new name merge in.
      for (const p of s.profiles) if (p.folder === was) p.folder = to;
      // The team's profiles as this launcher holds them move too.
      eachTeamProfile(ctx.state, String(teamId), (p) => { if (p._meta.folder === was) p._meta.folder = to; });
      for (const m of s.members) {
        for (const g of m.grants) if (g.scope.kind === "folder" && g.scope.name === was) g.scope = { kind: "folder", name: to };
        m.grants = stored(m.grants);
      }
      f.name = to;
      saved(ctx);
    }
    return folderOut(s, f);
  },
  team_admin_folder_color: ({ teamId, name, color }, ctx) => {
    const s = adminServer(ctx.state, teamId);
    const f = folder(s, name);
    f.color = label(color ?? "");
    saved(ctx);
    return folderOut(s, f);
  },
  team_admin_folder_delete: ({ teamId, name }, ctx) => {
    const s = adminServer(ctx.state, teamId);
    const f = folder(s, name);
    // Where its profiles should go is the person's call, not the server's.
    if (s.profiles.some((p) => p.folder === f.name)) throw "the folder is not empty";
    if ((s.trash ?? []).some((p) => p.folder === f.name)) throw "the folder's deleted profiles wait in the trash";
    s.folders = s.folders.filter((x) => x !== f);
    for (const m of s.members) m.grants = m.grants.filter((g) => !(g.scope.kind === "folder" && g.scope.name === f.name));
    saved(ctx);
    return null;
  },
  team_my_access: ({ teamId }, { state }) => {
    const l = link(state, teamId);
    // Read from the server each time (space/admin.rs my_access): none while it does not answer.
    const offline = teamReadRefusal(String(teamId));
    if (offline) throw offline;
    const s = serverOf(state, String(teamId));
    // The admin token holds every right on the whole team.
    if (l.admin) return ACTIONS.map((a) => allow({ kind: "team" }, a));
    const me = s.members.find((m) => m.id === s.me);
    if (!me) throw "the server does not know this token";
    if (!me.active) throw "this member is switched off";
    return copyRules(stored(inView(s, me.grants)));
  },
};
