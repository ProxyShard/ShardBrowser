import { t } from "../../../shared/i18n";
import { bindRefusal, inFolder, onProxy, rightName, teamRefusal } from "../../space";
import type { OpenOn, ProfileMeta, ProfileRight } from "../model/types";
import { isSaving } from "./saving";
import { isProtected } from "./lock";

/// What a team profile's own rights let this person do, and why not. A personal
/// row carries none: everything is given. Shown, not trusted: the backend judges
/// every door again.

type Row = Pick<ProfileMeta, "rights">;

/** Whether the row's rights allow `right`; always in the personal space. */
export const may = (p: Row, right: ProfileRight): boolean => !p.rights || p.rights[right];

/** Why the row's rights do not allow `right`, or null. `many`: said of several profiles. */
export function profileRefusal(p: Row, right: ProfileRight, many = false): string | null {
  if (may(p, right)) return null;
  const name = rightName(right);
  return many ? t("rights.needsProfiles", { right: name }) : t("rights.needsProfile", { right: name });
}

/** A team profile another device has open now, or null. */
export const openElsewhere = (p: Pick<ProfileMeta, "open_on">): OpenOn | null =>
  p.open_on && !p.open_on.this_device ? p.open_on : null;

/** "Open on <device> by <member>", or what of it the lease names. */
export function openOnWords(on: OpenOn): string {
  const device = on.device_name || t("rights.anotherDevice");
  return on.member_name ? t("rights.openOnBy", { device, member: on.member_name }) : t("rights.openOn", { device });
}

/** Why it cannot start here: no Start, its session still being sent from here, or open on another device. */
export function startRefusal(p: Pick<ProfileMeta, "id" | "rights" | "open_on">, many = false): string | null {
  const right = profileRefusal(p, "start", many);
  if (right) return right;
  // As open.rs refuses it (BUSY) until the session is in.
  if (isSaving(p.id)) return t("errors.teamBusy");
  const held = openElsewhere(p);
  return held ? t("rights.openElsewhere", { where: openOnWords(held) }) : null;
}

/** Why it cannot move to `folder`: Edit on it, and Create where it goes. */
export function moveRefusal(p: Pick<ProfileMeta, "rights" | "folder">, folder: string, many = false): string | null {
  return profileRefusal(p, "edit", many) ?? (p.folder === folder ? null : teamRefusal("create", inFolder(folder)));
}

/** Why it cannot be cloned: the copy is a new profile in its folder, on its proxy,
 *  which a team binds only for a member who sees and may use it (doors.rs, api.rs). */
export const cloneRefusal = (p: Pick<ProfileMeta, "folder" | "proxy_id" | "origin">): string | null =>
  teamRefusal("create", inFolder(p.folder))
  ?? (p.origin === "team" && p.proxy_id ? teamRefusal("see", onProxy(p.proxy_id)) ?? bindRefusal(p.proxy_id) : null);

/** Whether this member is shown the profile's settings: the server sends them only
 *  with Start, Edit, Cookies or Take out on it (cache.rs `reads_settings`). */
export const readsSettings = (p: Row): boolean =>
  !p.rights || p.rights.start || p.rights.edit || p.rights.cookies || p.rights.take_out;

/** Why the profile's settings are not shown, or null. */
export const settingsRefusal = (p: Row): string | null => (readsSettings(p) ? null : t("rights.settingsHidden"));

/** A team profile: its cookies are not offered here yet (perm.rs `cookies`). */
export const isTeamRow = (p: Pick<ProfileMeta, "origin">): boolean => p.origin === "team";

/** The ids of `ids` the check lets through, and the first refusal of the rest. */
export function letThrough(rows: readonly ProfileMeta[], ids: readonly string[], why: (p: ProfileMeta) => string | null): { ok: string[]; refused: string[]; reason: string | null } {
  const byId = new Map(rows.map((p) => [p.id, p]));
  const ok: string[] = [];
  const refused: string[] = [];
  let reason: string | null = null;
  for (const id of ids) {
    const p = byId.get(id);
    const r = p ? why(p) : null;
    if (r) { refused.push(id); reason ??= r; } else ok.push(id);
  }
  return { ok, refused, reason };
}

/** Why a protected profile does not take it (clone, cookies, a synced or automation start,
 *  sync, a proxy outside its editor), in the words errors.ts gives the backend's refusal. */
export const lockedRefusal = (p: Pick<ProfileMeta, "protected">): string | null =>
  isProtected(p) ? t("errors.lockedNotAvailable") : null;
