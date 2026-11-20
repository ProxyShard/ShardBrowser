// The team's trash on its server, as team-server keeps it and space/admin.rs reaches it: a team's
// deletes land here for 30 days; list, restore and purge are judged by the server, row by row.
import type { MockState, StoredProfile } from "./fixtures";
import type { Ctx, Handler } from "./handlers";
import { profileFromFingerprint, teamInUse, teamReadRefusal } from "./spaces";
import { myGrants } from "./teamRights";
import { decide, type Target } from "../../entities/team/lib/decide";
import type { TeamAction, TeamTrashKind, TeamTrashRow } from "../../entities/team/model/types";
import type { ProxyEntry } from "../../entities/proxy/model/types";
import { TEAM_REFUSALS } from "../../shared/api/teamRefusals";

/** A row with what comes back on a restore. */
export type MockTrashRow = Partial<TeamTrashRow> & { id: string; folder?: string; data?: unknown };

/** team-server's TRASH_TTL. */
const TTL = 30 * 86_400;
const nowSecs = () => Math.floor(Date.now() / 1000);
const { trashGone: GONE, trashNotAllowed: NOT_ALLOWED, trashTagTaken } = TEAM_REFUSALS;

/** A row as the server answers it: an older one in the state (only an id and a folder) reads as a profile. */
function out(r: MockTrashRow): TeamTrashRow {
  const deleted = r.deleted_at ?? nowSecs();
  return {
    id: r.id, kind: r.kind ?? "profile", name: r.name ?? r.id, folder: r.folder ?? "", proxy_kind: r.proxy_kind ?? "", country: r.country ?? "",
    revision: r.revision ?? 1, updated_at: r.updated_at ?? deleted, updated_by: r.updated_by ?? "admin",
    deleted_at: deleted, deleted_by: r.deleted_by ?? "admin", purge_at: r.purge_at ?? deleted + TTL,
  };
}

const serverOf = (state: MockState, teamId: unknown) => {
  if (!(state.teams ?? []).some((t) => t.id === teamId)) throw "not a member of that team";
  const offline = teamReadRefusal(String(teamId));
  if (offline) throw offline;
  const s = state.teamServers?.[String(teamId)];
  if (!s) throw "not a member of that team";
  return s;
};

/** Where the server judges a row: a profile's folder, the proxy, else the team. */
const placeOf = (r: TeamTrashRow): Target =>
  r.kind === "profile" ? { profileId: r.id, folder: r.folder } : r.kind === "proxy" ? { profileId: r.id } : {};

/** The admin token holds every right; a member's rules decide. */
function mayAt(state: MockState, teamId: string, action: TeamAction, r: TeamTrashRow): boolean {
  if ((state.teams ?? []).find((t) => t.id === teamId)?.admin) return true;
  const s = state.teamServers?.[teamId];
  const grants = s?.members.find((m) => m.id === s.me)?.grants ?? [];
  return decide(grants, action, placeOf(r)).kind === "allowed";
}

/** Who this launcher is to the team in use: a member id, or "admin". */
function me(state: MockState): string {
  const team = teamInUse(state);
  const s = team ? state.teamServers?.[team] : undefined;
  return myGrants(state) === "all" || !s?.me ? "admin" : s.me;
}

/** A delete in the team in use goes to its trash, the item kept for a restore; the windows hear of it. */
export function intoTeamTrash(ctx: Ctx, kind: TeamTrashKind, id: string, name: string, data: unknown, extra: Partial<TeamTrashRow> = {}): void {
  const team = teamInUse(ctx.state);
  const s = team ? ctx.state.teamServers?.[team] : undefined;
  if (!s) return;
  const at = nowSecs();
  const who = me(ctx.state);
  (s.trash ??= []).unshift({
    id, kind, name, folder: "", proxy_kind: "", country: "", revision: 1, updated_at: at, updated_by: who,
    deleted_at: at, deleted_by: who, purge_at: at + TTL, ...extra, data: structuredClone(data),
  });
  ctx.emit("store-changed", "team-trash");
}

/** Test knob: another device deletes something in the team; `row` names it as the server keeps it. */
export function addTeamTrash(state: MockState, row: MockTrashRow, emit: Ctx["emit"]): void {
  const team = teamInUse(state);
  const s = team ? state.teamServers?.[team] : undefined;
  if (!s) return;
  (s.trash ??= []).unshift({ ...out(row), data: row.data });
  emit("store-changed", "team-trash");
}

/** The row as this token sees it, or the server's "no such thing". */
function seen(state: MockState, teamId: string, id: unknown, kind: unknown) {
  const s = serverOf(state, teamId);
  const at = (s.trash ?? []).findIndex((r) => r.id === id && out(r).kind === (kind || out(r).kind));
  const row = at >= 0 ? out(s.trash![at]) : null;
  if (!row || !mayAt(state, teamId, "see", row)) throw GONE;
  return { s, at, row, data: s.trash![at].data };
}

/** The row this token may act on: Delete where it was too. */
function mayAct(state: MockState, teamId: string, got: ReturnType<typeof seen>) {
  if (!mayAt(state, teamId, "delete", got.row)) throw NOT_ALLOWED;
  return got;
}

/** What a row another device deleted comes back as: the server keeps it whole, the knob names it only. */
function made(state: MockState, r: TeamTrashRow): unknown {
  const at = nowSecs();
  switch (r.kind) {
    case "profile": return profileFromFingerprint(state.fingerprints[0], r.name, r.folder);
    case "proxy": return { id: r.id, name: r.name, kind: r.proxy_kind || "socks5", host: "restored.example.net", port: 1080, username: "", password: "", country: r.country, notes: "" };
    case "bookmark": return { id: r.id, title: r.name, url: "https://example.com/", folder: "" };
    case "extension": return { id: r.id, name: r.name, version: "1.0.0", description: "", icon: "", path: `/x/${r.id}`, size_bytes: 1, added_at: `@${at}` };
    case "automation": return { id: r.id, name: r.name, notes: "", blocks: [], run: { threads: 1, loops: 1, hours: 0, profiles: [], start: "" }, rules: [], created_at: at, updated_at: at };
    case "tag": return { id: r.id, name: r.name, color: "gray" };
    case "template": return { id: r.id, name: r.name, source: { library: null, os: "Windows" }, settings: {}, folder: "", tags: [], is_default: false, updated_at: at };
  }
}

/** Puts a restored item back in the team's lists, wherever they wait now. */
function putBack(state: MockState, teamId: string, row: TeamTrashRow, given: unknown): string[] {
  const lists = (state.dataOf ?? "personal") === teamId ? state : state.spaceData?.[teamId];
  if (!lists) return [];
  const data = given ?? made(state, row);
  switch (row.kind) {
    case "profile": {
      const p = data as StoredProfile;
      p._meta = { ...p._meta, id: row.id, rev: row.revision + 1 };
      lists.profiles[row.id] = p;
      if (!lists.order.includes(row.id)) lists.order.push(row.id);
      // Its folder comes back with it when it is gone.
      const s = state.teamServers?.[teamId];
      if (s && row.folder && !s.folders.some((f) => f.name === row.folder)) s.folders.push({ name: row.folder, color: "" });
      return ["profiles", "folders"];
    }
    case "proxy":
      lists.proxies.push(data as ProxyEntry);
      return ["proxies"];
    case "bookmark":
      lists.bookmarks.push(data as MockState["bookmarks"][number]);
      return ["bookmarks"];
    case "extension":
      lists.extensions.push(data as MockState["extensions"][number]);
      return ["extensions"];
    case "automation":
      lists.automation.push(data as MockState["automation"][number]);
      return ["automation"];
    case "tag":
      (lists.tags ??= { tags: [], by_profile: {} }).tags.push(data as { id: string; name: string; color: string });
      return ["tags"];
    case "template":
      (lists.templates ??= []).push(data as NonNullable<MockState["templates"]>[number]);
      return ["templates"];
  }
}

export const teamTrashHandlers: Record<string, Handler> = {
  // GET /trash: what this token may See where each was deleted from, newest first.
  team_trash_list: ({ teamId }, { state }) => {
    const s = serverOf(state, teamId);
    return (s.trash ?? []).map(out)
      .filter((r) => mayAt(state, String(teamId), "see", r))
      .sort((a, b) => b.deleted_at - a.deleted_at || a.id.localeCompare(b.id));
  },
  team_trash_restore: ({ teamId, id, kind }, ctx) => {
    const got = seen(ctx.state, String(teamId), id, kind);
    const { row } = got;
    // The launcher's own check (admin.rs tag_name_free), on a row it sees, before the server judges Delete.
    if (row.kind === "tag") {
      const lists = (ctx.state.dataOf ?? "personal") === teamId ? ctx.state : ctx.state.spaceData?.[String(teamId)];
      const live = lists?.tags?.tags.find((x) => x.id !== row.id && x.name.toLowerCase() === row.name.toLowerCase());
      if (live) throw trashTagTaken(live.name);
    }
    if (row.kind === "template") {
      const lists = (ctx.state.dataOf ?? "personal") === teamId ? ctx.state : ctx.state.spaceData?.[String(teamId)];
      const live = lists?.templates?.find((x) => x.id !== row.id && x.name.toLowerCase() === row.name.toLowerCase());
      if (live) throw `a template named ${live.name} already exists`;
    }
    const { s, at, data } = mayAct(ctx.state, String(teamId), got);
    s.trash!.splice(at, 1);
    const topics = putBack(ctx.state, String(teamId), row, data);
    ctx.save();
    for (const topic of topics) ctx.emit("store-changed", topic);
    ctx.emit("store-changed", "team-trash");
    return { kind: row.kind, id: row.id, name: row.name, revision: row.revision + 1 };
  },
  team_trash_purge: ({ teamId, id, kind }, ctx) => {
    const { s, at } = mayAct(ctx.state, String(teamId), seen(ctx.state, String(teamId), id, kind));
    s.trash!.splice(at, 1);
    ctx.save();
    ctx.emit("store-changed", "team-trash");
    return null;
  },
};
