import { create } from "zustand";
import { listen } from "@tauri-apps/api/event";
import { t } from "../../../shared/i18n";
import { toast } from "../../../shared/model/toast";
import { confirmModal } from "../../../shared/model/confirm";
import { activeTeam, useAccess, useSpace } from "../../../entities/space";
import { useTemplates } from "../../../entities/template";
import { decide, teamCall, trashError, useTeamAdmin, type Target, type TeamTrashKind, type TeamTrashRow } from "../../../entities/team";
import { reloadFolders, useProfile, useProfileUi } from "../../../entities/profile";
import { useBookmarks } from "../../../entities/bookmark";
import { useExtensions } from "../../../entities/extension";
import { useAutomation } from "../../../entities/automation";
import { useTags } from "../../../entities/tag";
import { storeBus } from "../../../shared/lib/storeBus";
import { rawError } from "../../../shared/lib/errors";
import { TEAM_REFUSALS } from "../../../shared/api/teamRefusals";

type Status = "idle" | "loading" | "ready" | "error";

/// The team's trash on its server (30 days), as this launcher's token may see it:
/// the admin sees all of it, a member what they may See where it was deleted from.
export const useTeamTrash = create<{
  teamId: string | null;
  status: Status;
  error: string | null;
  rows: TeamTrashRow[];
  /** A member saw rows of this team's trash: its Trash item stays from then on. */
  seen: boolean;
  search: string;
  setSearch: (q: string) => void;
}>((set) => ({
  teamId: null,
  status: "idle",
  error: null,
  rows: [],
  seen: false,
  search: "",
  setSearch: (search) => set({ search }),
}));

const team = () => activeTeam(useSpace.getState().state);

/** Whether the Trash item shows in a team: always for its admin, for a member once they see rows. */
export const useTeamTrashShown = (): boolean => {
  const admin = useSpace((s) => !!activeTeam(s.state)?.admin);
  const seen = useTeamTrash((s) => s.seen);
  return admin || seen;
};

/** Rows a restore or a purge is taking out right now: a read meanwhile leaves them out. */
const leaving = new Set<string>();
const keyOf = (r: Pick<TeamTrashRow, "kind" | "id">) => `${r.kind}:${r.id}`;
let seq = 0;

/** Reads the trash of the team in use; outside a team it empties. */
export async function loadTeamTrash(): Promise<void> {
  const now = team();
  const mine = ++seq;
  if (!now) { useTeamTrash.setState({ teamId: null, status: "idle", error: null, rows: [], seen: false }); return; }
  const fresh = useTeamTrash.getState().teamId !== now.id;
  if (fresh) useTeamTrash.setState({ teamId: now.id, status: "loading", error: null, rows: [], seen: false, search: "" });
  try {
    const read = await teamCall("team_trash_list", { teamId: now.id });
    if (mine !== seq) return;
    const rows = read.filter((r) => !leaving.has(keyOf(r)));
    useTeamTrash.setState((s) => ({ rows, status: "ready", error: null, seen: s.seen || rows.length > 0 }));
  } catch (e) {
    if (mine === seq) useTeamTrash.setState({ status: "error", error: String(e) });
  }
}

/** Why this person may not restore or purge the row: See and Delete where it was. Shown, not trusted. */
export function trashActRefusal(row: TeamTrashRow): string | null {
  const now = team();
  if (!now || now.admin) return null;
  const read = useAccess.getState().read;
  // Not read yet: the server judges.
  if (!read?.grants || read.teamId !== now.id) return null;
  const target: Target = row.kind === "profile" ? { profileId: row.id, folder: row.folder } : row.kind === "proxy" ? { profileId: row.id } : {};
  const ok = (["see", "delete"] as const).every((a) => decide(read.grants!, a, target).kind === "allowed");
  return ok ? null : t("team.trashNotAllowed", { right: t("team.actDelete") });
}

/** Who deleted it, in words: the admin, you, a member by name where the admin's list has them. */
export function deletedByName(row: TeamTrashRow): string {
  if (row.deleted_by === "admin") return t("team.trashByAdmin");
  const now = team();
  if (now && !now.admin && now.member_id === row.deleted_by) return t("team.trashByYou");
  const admin = useTeamAdmin.getState();
  const named = admin.teamId === now?.id ? admin.overview?.members.find((m) => m.id === row.deleted_by)?.name : undefined;
  return named ?? t("team.trashByMember");
}

const take = (row: TeamTrashRow) => {
  leaving.add(keyOf(row));
  const at = useTeamTrash.getState().rows.findIndex((r) => keyOf(r) === keyOf(row));
  useTeamTrash.setState((s) => ({ rows: s.rows.filter((r) => keyOf(r) !== keyOf(row)) }));
  return (e: unknown) => {
    // The server has no such row (purged meanwhile, by the sweep or elsewhere): it stays out, and the list is read again.
    if (rawError(e)?.trim() === TEAM_REFUSALS.trashGone) { void loadTeamTrash(); return; }
    // Failed: back in its place, unless a read put it back already.
    if (useTeamTrash.getState().rows.some((r) => keyOf(r) === keyOf(row))) return;
    const rows = [...useTeamTrash.getState().rows];
    rows.splice(Math.max(0, at), 0, row);
    useTeamTrash.setState({ rows });
  };
};

/** The list a restored item went back to, read again where it was read before: its page may not be open to hear of it. */
const RELOAD: Record<TeamTrashKind, () => unknown> = {
  profile: () => Promise.all([useProfile.getState().reload(), reloadFolders()]),
  proxy: () => storeBus.emit("proxies", "ui"),
  bookmark: () => useBookmarks.getState().status === "ready" && useBookmarks.getState().reload(),
  extension: () => useExtensions.getState().status === "ready" && useExtensions.getState().reload(),
  automation: () => useAutomation.getState().status === "ready" && useAutomation.getState().reload(),
  tag: () => useTags.getState().reload(),
  template: () => useTemplates.getState().reload(),
};

/** Back where it was; a profile with its cookies and session. */
export async function restoreTeamTrash(row: TeamTrashRow): Promise<void> {
  const now = team();
  if (!now) return;
  const putBack = take(row);
  try {
    const back = await teamCall("team_trash_restore", { teamId: now.id, id: row.id, kind: row.kind });
    leaving.delete(keyOf(row));
    void RELOAD[row.kind]();
    const show = back.kind === "profile"
      ? { label: t("toast.showInList"), run: () => { void useProfile.getState().reload().then(() => useProfileUi.getState().showInList(back.id)); } }
      : undefined;
    toast.ok(t("team.trashRestored", { name: back.name }), show ? { action: show } : undefined);
  } catch (e) {
    leaving.delete(keyOf(row));
    putBack(e);
    toast.fail({ title: t("useTrash.restoreFailed", { name: row.name }), detail: trashError(e), subject: row.name });
  }
}

/** Gone for good, after a question; a profile's session with it. */
export async function purgeTeamTrash(row: TeamTrashRow): Promise<void> {
  const now = team();
  if (!now) return;
  const ok = await confirmModal({
    title: t("useTrash.purgeTitle", { name: row.name }),
    message: t("team.trashPurgeConfirmBody"),
    confirmLabel: t("useTrash.purgeConfirm"),
    danger: true,
  });
  if (ok !== true) return;
  const putBack = take(row);
  try {
    await teamCall("team_trash_purge", { teamId: now.id, id: row.id, kind: row.kind });
    leaving.delete(keyOf(row));
    toast.ok(t("team.trashPurged", { name: row.name }));
  } catch (e) {
    leaving.delete(keyOf(row));
    putBack(e);
    toast.fail({ title: t("useTrash.purgeFailed", { name: row.name }), detail: trashError(e), subject: row.name });
  }
}

let wired = false;
/** Reads the team's trash as the space moves, as the server tells of a change, and when it answers again. */
export function wireTeamTrash(): void {
  if (wired) return;
  wired = true;
  const key = () => team()?.id ?? "";
  let last = key();
  void loadTeamTrash();
  useSpace.subscribe((s, prev) => {
    const k = key();
    if (k !== last) { last = k; void loadTeamTrash(); return; }
    if (k && s.state?.team_status?.state === "ready" && prev.state?.team_status?.state !== "ready") void loadTeamTrash();
  });
  void listen<string>("store-changed", (e) => { if (e.payload === "team-trash") void loadTeamTrash(); });
}
