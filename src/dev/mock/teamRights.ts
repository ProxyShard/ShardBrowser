// A team's rights as the launcher's doors judge them (space/doors.rs, perm.rs), by the shared rule
// (team-model `decide`, here its port in entities/team): the narrowest grant wins, a refusal wins
// at equal width, nothing matching is no. Each refusal is worded as space/error.rs words it.
import type { MockState, StoredProfile } from "./fixtures";
import type { Ctx } from "./handlers";
import { NOT_IN_TEAM, teamInUse } from "./spaces";
import { TEAM_REFUSALS } from "../../shared/api/teamRefusals";
import { decide, type Target } from "../../entities/team/lib/decide";
import type { Grant, TeamAction } from "../../entities/team/model/types";
import type { OpenOn, ProfileMeta, ProfileRights } from "../../entities/profile/model/types";

const { teamNotYet: TEAM_NOT_YET, forbidden, missing, leased } = TEAM_REFUSALS;
/** This launcher's own device, as space_state names it. */
const DEVICE = "Mock-MacBook";
const nowSecs = () => Math.floor(Date.now() / 1000);



/** The team in use's server, when one is. */
function server(state: MockState) {
  const team = teamInUse(state);
  return team ? state.teamServers?.[team] : undefined;
}

/** The rules this launcher holds for the team in use: "all" for its admin token, null in the personal space. */
export function myGrants(state: MockState): Grant[] | "all" | null {
  const team = teamInUse(state);
  if (!team) return null;
  if ((state.teams ?? []).find((t) => t.id === team)?.admin) return "all";
  const s = state.teamServers?.[team];
  return s?.members.find((m) => m.id === s.me)?.grants ?? [];
}

/** Whether the team in use allows `action` on `target`; always in the personal space. */
export function may(state: MockState, action: TeamAction, target: Target): boolean {
  const g = myGrants(state);
  return g === null || g === "all" || decide(g, action, target).kind === "allowed";
}

const isAdmin = (state: MockState) => myGrants(state) !== null && myGrants(state) === "all";
const ofProfile = (p: StoredProfile): Target => ({ profileId: p._meta.id, folder: p._meta.folder });
/** A proxy is in no folder, as the server judges it. */
const ofProxy = (id: string): Target => ({ profileId: id });

/** One team profile's rights (perm.rs `rights`). */
export function rightsOf(state: MockState, p: StoredProfile): ProfileRights {
  const t = ofProfile(p);
  const m = (a: TeamAction) => may(state, a, t);
  return { see: m("see"), start: m("start"), edit: m("edit"), delete: m("delete"), cookies: m("cookies"), take_out: m("take_out") };
}

/** Who has a team profile open: this launcher while it runs here, or another device's lease. */
export function openOnOf(state: MockState, id: string): OpenOn | undefined {
  const s = server(state);
  if (!s) return undefined;
  const me = s.members.find((m) => m.id === s.me)?.name ?? "Admin";
  if (state.running[id]) return { device_name: DEVICE, member_name: me, this_device: true, since: Math.floor(state.running[id].started / 1000), until: nowSecs() + 90 };
  const l = s.leases?.[id];
  return l ? { device_name: l.device_name, member_name: l.member_name, this_device: false, since: l.since, until: nowSecs() + 90 } : undefined;
}

/** A row as a team's list shows it (remote.rs `meta`): its rights, its lease, its tags; no pin. */
export function teamRow(state: MockState, meta: ProfileMeta): ProfileMeta {
  const p = state.profiles[meta.id];
  if (!p || !teamInUse(state)) return meta;
  const on = openOnOf(state, meta.id);
  return {
    ...meta, pinned: false, origin: "team", rights: rightsOf(state, p),
    ...(on ? { open_on: on } : {}), tags: state.tags?.by_profile[meta.id] ?? [],
  };
}

/** Whether a row is listed for this member at all (See). */
const sees = (state: MockState, p: StoredProfile) => may(state, "see", ofProfile(p));

/** Why a new team profile cannot be made in `folder`; null in the personal space. */
export function teamCreateRefusal(state: MockState, folder: string): string | null {
  return !teamInUse(state) || may(state, "create", { folder }) ? null : forbidden("create", "folder");
}

/** Why a team profile does not start from here (open.rs `passes`): Start on it, and
 *  Cookies too when automation drives it; null in the personal space. */
export function teamStartRefusal(state: MockState, id: string, driven: boolean): string | null {
  if (!teamInUse(state)) return null;
  const p = state.profiles[id];
  if (!p || !sees(state, p)) return missing("profile", id);
  if (!may(state, "start", ofProfile(p))) return forbidden("start", "profile");
  return driven && !may(state, "cookies", ofProfile(p)) ? forbidden("cookies", "profile") : null;
}

/** What a team's lists show this member: only what they may See, each row decorated. */
export function teamView(state: MockState, cmd: string, out: unknown): unknown {
  if (!teamInUse(state)) return out;
  const g = myGrants(state);
  const row = (m: ProfileMeta) => teamRow(state, m);
  switch (cmd) {
    case "profile_list":
      return (out as ProfileMeta[]).filter((m) => { const p = state.profiles[m.id]; return !p || sees(state, p); }).map(row);
    case "profile_save": case "profile_clone": case "profile_create_from_template": case "trash_restore":
      return out && typeof out === "object" && "id" in out ? row(out as ProfileMeta) : out;
    case "proxy_list":
      return (out as { id: string }[]).filter((x) => may(state, "see", ofProxy(x.id)));
    case "profile_get": {
      // The server hands its settings only to who may use it (cache.rs `reads_settings`):
      // with See alone the row is all there is (remote.rs `stored(row, None)`).
      const p = out as StoredProfile;
      const r = p?._meta ? rightsOf(state, p) : null;
      if (!r || r.start || r.edit || r.cookies || r.take_out) return out;
      return { _meta: { ...p._meta, pinned: false, total_runtime_ms: 0, extensions: [], gpu_preset_id: "" }, name: p.name, notes: p.notes };
    }
    case "bookmark_list": case "extension_list": case "automation_list":
      return g === "all" || may(state, "see", {}) ? out : [];
    // Without See on the team its tags are none (the server lists no items).
    case "tag_list":
      return g === "all" || may(state, "see", {}) ? out : { tags: [], by_profile: {} };
    case "folder_list": {
      // As the server's GET /folders: a folder they may See, or one holding a profile they may.
      if (g === "all") return out;
      const f = out as { registry: string[]; meta: Record<string, unknown> };
      const shown = (name: string) => may(state, "see", { folder: name })
        || Object.values(state.profiles).some((p) => p._meta.folder === name && sees(state, p));
      const registry = f.registry.filter(shown);
      return { registry, meta: Object.fromEntries(Object.entries(f.meta).filter(([n]) => registry.includes(n))) };
    }
    default:
      return out;
  }
}

/** The refusal a door gives this request in a team before anything is written, or null. */
export function teamDoor(state: MockState, cmd: string, args: Record<string, unknown>): string | null {
  if (!teamInUse(state)) return null;
  const profile = (id: unknown): StoredProfile | string => {
    const p = state.profiles[String(id)];
    return p && sees(state, p) ? p : missing("profile", String(id));
  };
  const onProfile = (id: unknown, ...need: TeamAction[]): string | null => {
    const p = profile(id);
    if (typeof p === "string") return p;
    const lacks = need.find((a) => !may(state, a, ofProfile(p)));
    return lacks ? forbidden(lacks, "profile") : null;
  };
  const inFolder = (folder: string, action: TeamAction = "create") => (may(state, action, { folder }) ? null : forbidden(action, "folder"));
  const onTeam = (action: TeamAction) => (may(state, action, {}) ? null : forbidden(action, "team"));
  const onProxy = (id: string, action: TeamAction) =>
    !may(state, "see", ofProxy(id)) ? missing("proxy", id) : may(state, action, ofProxy(id)) ? null : forbidden(action, "proxy");
  const admin = () => (isAdmin(state) ? null : forbidden("admin", "team"));
  /** A proxy bound anew: See on it (doors.rs BindsProxy), then the server's use of it (api.rs bad_proxy). */
  const binds = (proxyId: unknown, was?: string | null) => {
    const id = String(proxyId ?? "").trim();
    if (!id || id === (was ?? "")) return null;
    if (!may(state, "see", ofProxy(id))) return forbidden("see", "proxy");
    return (["start", "edit", "take_out"] as const).some((a) => may(state, a, ofProxy(id))) ? null : TEAM_REFUSALS.badProxy;
  };
  /** Another device holds it (the server's lease): the start is refused with its name. */
  const lease = (id: unknown) => {
    const l = server(state)?.leases?.[String(id)];
    return l && !state.running[String(id)] ? leased(l.device_name) : null;
  };

  switch (cmd) {
    case "profile_get":
      return typeof profile(args.id) === "string" ? (profile(args.id) as string) : null;
    case "profile_save": {
      const incoming = args.payload as StoredProfile | undefined;
      const id = incoming?._meta?.id;
      if (!id) {
        // A new one: Create in its folder, a proxy it may bind, Cookies for its extensions (write.rs create_profile).
        const folder = String(incoming?._meta?.folder ?? "");
        const made: Target = { profileId: "new", folder };
        return inFolder(folder) ?? binds(incoming?._meta?.proxy_id)
          ?? ((incoming?._meta?.extensions ?? []).length && !may(state, "cookies", made) ? forbidden("cookies", "profile") : null);
      }
      const p = profile(id);
      if (typeof p === "string") return p;
      const edit = onProfile(id, "edit") ?? binds(incoming?._meta?.proxy_id, p._meta.proxy_id);
      if (edit) return edit;
      // A change of its extensions reads its cookies too (doors.rs ChangesExtensions).
      const was = [...p._meta.extensions].sort().join(",");
      const now = [...(incoming?._meta?.extensions ?? [])].sort().join(",");
      return was !== now && !may(state, "cookies", ofProfile(p)) ? forbidden("cookies", "profile") : null;
    }
    case "profile_clone": {
      const p = profile(args.id);
      return typeof p === "string" ? p : inFolder(p._meta.folder) ?? binds(p._meta.proxy_id);
    }
    case "profile_bind_proxy": {
      const p = profile(args.profileId);
      return typeof p === "string" ? p : onProfile(args.profileId, "edit") ?? binds(args.proxyId, p._meta.proxy_id);
    }
    case "profile_set_folder": {
      const p = profile(args.id);
      if (typeof p === "string") return p;
      return onProfile(args.id, "edit") ?? (p._meta.folder === String(args.folder ?? "") ? null : inFolder(String(args.folder ?? "")));
    }
    case "profile_delete":
      return onProfile(args.id, "delete");
    case "profile_set_pin": case "folder_import": case "tag_import":
      return NOT_IN_TEAM;
    case "profile_create_from_template":
      return inFolder("");
    // Judged one by one as each is written (the handler): the ones before a refusal stay.
    case "profile_import":
      return null;
    case "folder_add": case "folder_meta_set": case "folder_rename":
      return admin();
    case "folder_delete": {
      const a = admin();
      if (a || !args.deleteProfiles) return a;
      const ids = state.order.filter((id) => state.profiles[id]?._meta.folder === args.folder);
      return ids.map((id) => onProfile(id, "delete")).find((r) => r) ?? null;
    }
    case "launch":
      return onProfile(args.profileId, "start") ?? lease(args.profileId);
    case "sync_launch":
      return null;
    case "automation_launch":
      return onProfile(args.profileId, "start", "cookies") ?? lease(args.profileId);
    // The CDP pass kept with a running profile, judged at each use (space/mod.rs cdp_pass).
    case "automation_attach": case "automation_screencast": case "automation_call": case "automation_pick": {
      const p = state.profiles[String(args.profileId)];
      return p && !may(state, "cookies", ofProfile(p)) ? forbidden("cookies", "profile") : null;
    }
    case "cookies_export": case "cookies_export_to_file": case "cookies_import":
      return TEAM_NOT_YET;
    case "tag_save": {
      const id = (args.tag as { id?: string } | undefined)?.id;
      return onTeam(id && state.tags?.tags.some((t) => t.id === id) ? "edit" : "create");
    }
    case "tag_delete": case "bookmark_delete": case "bookmark_delete_many": case "extension_delete": case "automation_delete":
      return onTeam("delete");
    case "tag_assign":
      return Object.keys((args.byProfile ?? {}) as Record<string, unknown>).map((id) => onProfile(id, "edit")).find((r) => r) ?? null;
    case "bookmark_save": {
      const id = (args.entry as { id?: string } | undefined)?.id;
      return onTeam(id && state.bookmarks.some((b) => b.id === id) ? "edit" : "create");
    }
    case "extension_import": case "extension_import_url":
    case "automation_create": case "automation_duplicate": case "automation_import":
    case "proxy_bulk_import": case "proxy_bulk_save": case "ps_import_order":
      return onTeam("create");
    case "automation_save":
      return onTeam("edit");
    // Updates of a team's extensions come later (space/mod.rs extensions::update).
    case "extension_update": case "extension_link_source":
      return onTeam("edit") ?? TEAM_REFUSALS.teamExtensionsNotYet;
    case "automation_export":
      return onTeam("take_out");
    case "automation_run":
      return onTeam("see");
    case "proxy_save": {
      const id = (args.entry as { id?: string } | undefined)?.id;
      return id && state.proxies.some((x) => x.id === id) ? onProxy(id, "edit") : onTeam("create");
    }
    case "proxy_delete":
      return onProxy(String(args.id), "delete");
    default:
      return null;
  }
}

/** Test knob: the member this launcher joined as gets these rules; the team tells it, as live.rs does. */
export function setMyGrants(state: MockState, grants: Grant[], emit: Ctx["emit"]): void {
  const s = server(state);
  const me = s?.members.find((m) => m.id === s.me);
  if (!me) return;
  me.grants = grants.map((g) => ({ ...g, scope: { ...g.scope } }));
  me.epoch = (me.epoch ?? 1) + 1;
  // A new permission edition: every row's rights are news (cache.rs).
  emit("store-changed", "profiles");
}

/** Test knob: another device opens (or closes) a team profile; the team's live channel says so. */
export function setLease(state: MockState, id: string, on: { device_name: string; member_name: string } | null, emit: Ctx["emit"]): void {
  const s = server(state);
  if (!s) return;
  const leases = (s.leases ??= {});
  if (on) leases[id] = { ...on, since: nowSecs() - 12 * 60 };
  else delete leases[id];
  emit("store-changed", "profiles");
}
