import { t } from "../../shared/i18n";
import {
  AddIcon, ChangePasswordIcon, ChevronDownIcon, ChevronRightIcon, CloseIcon, CopyIcon, DeleteIcon, DownloadIcon, EditIcon, FolderIcon, FolderMinusIconApp, IdCardIcon,
  PasswordIcon, PeekIcon, PinIconApp, PlayIcon, RegenerateIcon, RemovePasswordIcon, RenameIcon, RouteIcon, StopIcon, SyncIcon, SyncedIcon, SyncOffIcon, TagIcon,
  UnpinIconApp, UploadIcon,
} from "../../shared/icons";
import { syncConnected, useSync } from "../../entities/sync";
import { openHereInstead, setProfilesSync } from "../profile-sync";
import { processList, profileFocusWindow } from "../../entities/profile";
import { clip } from "../../shared/lib/clipboard";
import { toast } from "../../shared/model/toast";
import { useNav } from "../../shared/model/navigation";
import { defineCommands, type Command, type RunFrom, type Subject } from "../../shared/commands";
import {
  cloneRefusal, findView, folderNamesOf, isProtected, isTeamRow, lockedRefusal, moveRefusal, profileRefusal, resolveId, selectVisible, setContinueByHand, startRefusal, syncBlockReason,
  useOverviewFolds, useProfile, useProfileUi, viewFolds, type ProfileMeta, type ProfileRight,
} from "../../entities/profile";
import { createAnywhereRefusal, inTeam } from "../../entities/space";
import { rowActionIcons } from "../../shared/ui/rowActionIcons";
import { openProfileForm } from "../profile-form/model/useProfileForm";
import { changePassword, removePassword, setPassword } from "../profile-lock";
import { passwordsOn } from "../../entities/settings";

const data = () => useProfile.getState();
const ui = () => useProfileUi.getState();
/// The list by id, built once per list (the store replaces the array on every
/// change): each row's hover commands ask `when` as the row renders, and a
/// 2,000-entry map per ask was a third of a scrolled frame's script at 4x.
let indexed: { list: ProfileMeta[]; byId: Map<string, ProfileMeta> } | null = null;
const metas = (s: Subject): ProfileMeta[] => {
  const list = data().profiles;
  if (indexed?.list !== list) indexed = { list, byId: new Map(list.map((p) => [p.id, p])) };
  const { byId } = indexed;
  return s.ids.flatMap((id) => {
    const p = byId.get(id);
    return p ? [p] : [];
  });
};
const isRunning = (id: string) => data().running[id] !== undefined;
/// Its launch is still going: it already counts as running, but Stop waits for
/// the launch to return; Cancel is what ends it.
const isStarting = (id: string) => data().startBusy.has(id);
const isLive = (id: string) => isRunning(id) && !isStarting(id);
const fromPalette = (from: RunFrom) => from === "palette" || from === "paletteActions";
/// From the palette the row may be hidden or on another page. A row the list
/// shows is only scrolled into view, so the Folder or View the person is in
/// stays; any other is shown in All Profiles first (Pair 37 review, I2).
/// `then` runs once the row is there: at once outside the palette, and never
/// when the person stayed on their page (unsaved changes, Cancel).
const atRow = (id: string, from: RunFrom, then: () => void): void | Promise<void> => {
  if (!fromPalette(from)) return then();
  const shown = useNav.getState().section === "profiles" && selectVisible(ui()).indexById.has(id);
  if (shown) {
    ui().reveal(id);
    return then();
  }
  return ui().showInList(id).then((ok) => { if (ok) then(); });
};

const LIST = { area: "profiles", page: "profiles" } as const;

/// Why none of `list` may be acted on, or null when one may: a team's rights
/// on each row, or another device holding it. Those that may are acted on;
/// the store says which it left out (useProfile's guards).
function noneMay(list: ProfileMeta[], why: (p: ProfileMeta, many: boolean) => string | null): string | null {
  const many = list.length > 1;
  let first: string | null = null;
  for (const p of list) {
    const r = why(p, many);
    if (r === null) return null;
    first ??= r;
  }
  return first;
}
const lacks = (right: ProfileRight) => (s: Subject) => noneMay(metas(s), (p, many) => profileRefusal(p, right, many));
/** A new profile needs Create in some folder of the team. */
const createRefusal = () => createAnywhereRefusal(folderNamesOf(data().profiles, data().folderRegistry));
/** Only rows of the personal space: a team's cookies and pins are not offered yet (perm.rs, space/mod.rs). */
const personalRows = (s: Subject) => metas(s).every((p) => !isTeamRow(p));
/** Why none of them may: each is protected by a password (#127). */
const allLocked = (s: Subject) => noneMay(metas(s), lockedRefusal);
/** One row, protected; a password's own commands. */
const oneLocked = (s: Subject) => s.ids.length === 1 && metas(s).some(isProtected);
/** A password's commands wait for the browser to close (the backend's "close the profile first"). */
const stopFirst = (s: Subject) => (s.ids.some(isRunning) ? t("profileRow.stopFirst") : null);

/// Every run follows the registry's contract (`Command.run`): the ids a run
/// answers with are the ones that failed, which its own toast already names,
/// and the selection stays exactly as it was (owner rule: bulk actions keep
/// the selection; ORDER R26 a). The one exception is D5's ruling F13: rows
/// that move to the Trash leave the list, and so the selection.
export const PROFILE_COMMANDS: readonly Command[] = [
  {
    ...LIST, id: "profile.start", scope: "selection", group: "run", icon: PlayIcon,
    label: () => t("commands.start"), keys: ["S"],
    surfaces: ["rowMenu", "pinnedMenu", "bulkBar", "palette", "paletteActions", "sheet"],
    when: (s) => s.ids.some((id) => !isRunning(id)),
    // The bulk bar keeps Start whatever runs (it ignores \`when\`): with every
    // selected profile running, it says why it does nothing (EMIL-05).
    disabled: (s) => {
      if (s.ids.length > 0 && s.ids.every(isRunning)) return t("commands.alreadyRunning");
      // Every one left is open on another computer (personal sync).
      const idle = metas(s).filter((p) => !isRunning(p.id));
      const held = idle.find((p) => p.sync?.on && p.sync.open_on);
      if (held && idle.every((p) => p.sync?.on && p.sync.open_on)) return t("errors.openElsewhere", { device: held.sync!.open_on! });
      // A team's: no Start on any of them, or each open on another device.
      return noneMay(idle, startRefusal);
    },
    run: async (s) => {
      const idle = metas(s).filter((p) => !isRunning(p.id));
      // One: startStop resolves to the reason it failed, or null (C25).
      if (s.ids.length === 1 && idle.length === 1) return (await data().startStop(idle[0])) !== null ? [idle[0].id] : [];
      return data().launchMany(idle.map((p) => p.id));
    },
  },
  {
    ...LIST, id: "profile.stop", scope: "selection", group: "run", icon: StopIcon,
    label: () => t("commands.stop"), keys: ["S"],
    surfaces: ["rowMenu", "pinnedMenu", "bulkBar", "palette", "paletteActions", "sheet"],
    // A profile still starting has Cancel instead.
    when: (s) => s.ids.some(isLive),
    // And Stop, with none of them running.
    disabled: (s) => (s.ids.length > 0 && !s.ids.some(isRunning) ? t("commands.notRunning") : null),
    run: async (s) => {
      const live = metas(s).filter((p) => isLive(p.id));
      if (s.ids.length === 1 && live.length === 1) return (await data().startStop(live[0])) !== null ? [live[0].id] : [];
      return data().stopMany(live.map((p) => p.id));
    },
  },
  {
    // The debugging address of a profile started with it, for pointing a
    // script or an inspector at this browser by hand. Only when there is one:
    // a profile the operator started from the list has no CDP port, and the
    // command then has nothing to give.
    ...LIST, id: "profile.copyCdp", scope: "selection", group: "run", icon: CopyIcon,
    label: () => t("commands.copyCdp"),
    surfaces: ["rowMenu", "paletteActions"],
    when: (s) => s.ids.length === 1 && s.ids.some(isLive),
    // Without Cookies the team keeps the debugging address closed (process_list leaves it out).
    disabled: (s) => (metas(s).some((p) => profileRefusal(p, "cookies")) ? t("rights.cdpNeedsCookies") : null),
    run: async (s) => {
      const one = metas(s).find((p) => isLive(p.id));
      if (!one) return;
      const running = await processList();
      const cdp = running.find((r) => r.profile_id === one.id)?.cdp;
      if (!cdp) { toast.err(t("commands.noCdp", { name: one.name })); return; }
      // The WebSocket address: what Playwright and Puppeteer connect to.
      // Opening the inspector on a live profile is a different matter — a
      // page with a debugger trap notices an open DevTools.
      await clip.write(cdp.web_socket_debugger_url);
      toast.ok(t("commands.copiedCdp"));
    },
  },
  {
    // Finding one browser window among a dozen is the operator's problem
    // otherwise. Only for a profile that is actually up: the browser raises
    // itself, so there is nothing to ask when there is no browser.
    ...LIST, id: "profile.showWindow", scope: "selection", group: "run", icon: PeekIcon,
    label: () => t("commands.showWindow"),
    surfaces: ["rowMenu", "pinnedMenu", "paletteActions"],
    when: (s) => s.ids.length === 1 && s.ids.some(isLive),
    run: async (s) => {
      const live = metas(s).filter((p) => isLive(p.id));
      const one = live[0];
      if (!one) return;
      try {
        await profileFocusWindow(one.id);
      } catch (e) {
        toast.fail({ title: t("profileList.showWindowFailed", { name: one.name }), detail: e, subject: one.name });
      }
    },
  },
  {
    // While a profile starts (its launch can take a while on a slow proxy):
    // the way out. No key: a second S must not undo the first.
    ...LIST, id: "profile.cancelStart", scope: "selection", group: "run", icon: CloseIcon,
    label: () => t("commands.cancelStart"),
    surfaces: ["rowMenu", "pinnedMenu", "paletteActions"],
    when: (s) => s.ids.some(isStarting),
    // Pressed already: the launch is being ended.
    disabled: (s) => (s.ids.filter(isStarting).every((id) => data().cancelling.has(id)) ? t("commands.alreadyCancelling") : null),
    run: async (s) => {
      await Promise.all(s.ids.filter(isStarting).map((id) => data().cancelStart(id)));
    },
  },
  {
    // A profile another computer holds (personal sync): take it here and start it,
    // for when that computer is off or lost.
    ...LIST, id: "profile.takeLease", scope: "active", group: "run", icon: PlayIcon,
    label: () => t("commands.openHereInstead"),
    surfaces: ["rowMenu", "pinnedMenu", "paletteActions"],
    when: (s) => metas(s).some((p) => !isRunning(p.id) && p.sync?.on && !!p.sync.open_on),
    run: async (s) => { const p = metas(s)[0]; if (p && !(await openHereInstead(p))) return [p.id]; },
  },
  {
    ...LIST, id: "profile.openPeek", scope: "active", group: "open", icon: PeekIcon,
    focusAfter: true,
    label: () => t("commands.openPeek"), keys: ["Enter", "Space"], keysJoin: "or",
    surfaces: ["rowMenu", "hover", "paletteActions", "sheet"],
    run: (s, { from }) => {
      const id = s.ids[0];
      // Enter and Space open and close (6.2); a menu or the palette opens.
      if (from === "keys") { ui().togglePeek(id); return; }
      return atRow(id, from, () => ui().openPeek(id));
    },
  },
  {
    // The eye, as the Pinned menu has it (owner feedback 5m).
    ...LIST, id: "profile.showInList", scope: "active", group: "open", icon: rowActionIcons.showInList,
    focusAfter: true,
    label: () => t("commands.showInList"),
    surfaces: ["pinnedMenu", "paletteActions"],
    run: async (s) => { await ui().showInList(s.ids[0]); },
  },
  {
    // The profile form (manual save): every field of the profile, saved by hand.
    ...LIST, id: "profile.edit", scope: "active", group: "open", icon: EditIcon,
    focusAfter: true,
    label: () => t("commands.edit"),
    // Without Edit the form opens to read: the palette says so.
    paletteLabel: (s) => {
      const p = metas(s)[0];
      return p && profileRefusal(p, "edit") ? t("commands.viewNamed", { name: p.name }) : t("commands.editNamed", { name: p?.name ?? "" });
    },
    // Without Edit the row's menu says View, as Peek does.
    menuLabel: (s) => {
      const p = metas(s)[0];
      return p && profileRefusal(p, "edit") ? t("peek.view") : t("commands.edit");
    },
    surfaces: ["rowMenu", "palette", "paletteActions"],
    run: (s, { from }) => atRow(s.ids[0], from, () => openProfileForm({ mode: "edit", id: s.ids[0] })),
  },
  {
    // The form's own key (a dialog has the keyboard, so the form listens for it), listed for the sheet.
    ...LIST, id: "profile.save", scope: "page", group: "open",
    label: () => t("commands.saveProfile"), keys: ["Mod+S"], keyOwner: "self",
    surfaces: ["sheet"],
  },
  {
    ...LIST, id: "profile.rename", scope: "active", group: "open", icon: RenameIcon,
    focusAfter: true,
    label: () => t("commands.rename"), keys: ["E"],
    surfaces: ["rowMenu", "paletteActions", "sheet"],
    disabled: lacks("edit"),
    // `keyed`: its close gives the keyboard back to the row (merge-columns-fix1).
    run: (s, { from }) => atRow(s.ids[0], from, () => ui().startEdit(s.ids[0], "name", { keyed: from === "keys" })),
  },
  {
    ...LIST, id: "profile.clone", scope: "active", group: "open", icon: CopyIcon,
    label: () => t("commands.clone"),
    surfaces: ["rowMenu", "hover", "paletteActions", "peekMenu"],
    // The copy is a new profile in the same folder; a protected one is not copied.
    disabled: (s) => noneMay(metas(s), (p) => cloneRefusal(p) ?? lockedRefusal(p)),
    run: (s) => data().cloneProfile(s.ids[0]),
  },
  {
    ...LIST, id: "profile.pin", scope: "active", group: "open", icon: PinIconApp,
    label: () => t("commands.pin"),
    surfaces: ["rowMenu", "hover", "paletteActions", "peekMenu"],
    // A pin is this computer's own mark, and a team's profiles keep none yet (space/mod.rs set_pin).
    when: (s) => personalRows(s) && metas(s).some((p) => !p.pinned),
    run: async (s) => { const p = metas(s)[0]; if (p) await data().togglePin(p); },
  },
  {
    // Its own glyph in menus, as the row menu had (Pair 5e fix round 1, finding 10).
    ...LIST, id: "profile.unpin", scope: "active", group: "open", icon: UnpinIconApp,
    label: () => t("commands.unpin"),
    surfaces: ["rowMenu", "hover", "pinnedMenu", "paletteActions", "peekMenu"],
    when: (s) => personalRows(s) && metas(s).some((p) => p.pinned),
    run: async (s) => { const p = metas(s)[0]; if (p) await data().togglePin(p); },
  },
  {
    ...LIST, id: "profile.moveToFolder", scope: "selection", group: "organize", icon: FolderIcon,
    focusAfter: true,
    label: () => t("commands.moveToFolder"), keys: ["M"],
    surfaces: ["rowMenu", "bulkBar", "palette", "paletteActions", "peekMenu", "sheet"],
    // Where they may go is the picker's to say: each folder needs Create there.
    disabled: lacks("edit"),
    run: (s, { from }) => {
      // The bar's button opens its picker and closes it again, as Tags does.
      if (from === "bulkBar" && ui().folderPicker?.from === "bar") { ui().setFolderPicker(null); return; }
      const open = () => ui().setFolderPicker({ profileIds: s.ids, from: from === "bulkBar" ? "bar" : "list" });
      // The picker opens under the row's Folder cell (EMIL-09).
      if (s.ids.length === 1 && from !== "bulkBar") return atRow(s.ids[0], from, () => { ui().setActive(s.ids[0]); open(); });
      open();
    },
  },
  {
    ...LIST, id: "profile.removeFromFolder", scope: "selection", group: "organize", icon: FolderMinusIconApp,
    label: () => t("commands.removeFromFolder"),
    surfaces: ["rowMenu", "palette", "paletteActions", "peekMenu"],
    when: (s) => metas(s).some((p) => p.folder !== ""),
    disabled: (s) => noneMay(metas(s).filter((p) => p.folder !== ""), (p, many) => moveRefusal(p, "", many)),
    run: async (s) => {
      const ids = metas(s).filter((p) => p.folder !== "").map((p) => p.id);
      await data().moveMany(ids, "");
      // Those still in a Folder did not move.
      return metas({ ids }).filter((p) => p.folder !== "").map((p) => p.id);
    },
  },
  {
    ...LIST, id: "profile.tags", scope: "selection", group: "organize", icon: TagIcon,
    focusAfter: true,
    label: () => t("commands.editTags"), keys: ["T"],
    surfaces: ["rowMenu", "bulkBar", "palette", "paletteActions", "peekMenu", "sheet"],
    // Which tags a profile carries is the profile's: Edit on it (doors.rs tag_assign).
    disabled: lacks("edit"),
    run: (s, { from }) => {
      // The bar's button opens its picker and closes it again.
      if (from === "bulkBar" && ui().tagPicker?.from === "bar") { ui().setTagPicker(null); return; }
      const open = () => ui().setTagPicker({ profileIds: s.ids, from: from === "bulkBar" ? "bar" : "list", keyed: from === "keys" || undefined });
      // From the bar the active row stays where it is, as the bar always had it.
      if (s.ids.length === 1 && from !== "bulkBar") return atRow(s.ids[0], from, () => { ui().setActive(s.ids[0]); open(); });
      open();
    },
  },
  {
    ...LIST, id: "profile.proxy", scope: "selection", group: "organize", icon: RouteIcon,
    focusAfter: true,
    label: (n) => (n > 1 ? t("commands.assignProxies") : t("commands.changeProxy")), keys: ["P"],
    surfaces: ["rowMenu", "bulkBar", "palette", "paletteActions", "peekMenu", "sheet"],
    // A protected one's proxy changes in its editor, with its password.
    disabled: (s) => lacks("edit")(s) ?? allLocked(s),
    run: (s, { from }) => {
      // One row the list shows (its menu, its keys, or the palette after it
      // brings the row into view): that row's proxy cell picker. Otherwise,
      // and from the bar for any count, the Assign proxies dialog (IFACE-05;
      // Pair 37 review, I3), which leaves the selection as it is (R26 a).
      const inline = s.ids.length === 1 && (from === "rowMenu" || from === "keys" || fromPalette(from));
      if (inline) return atRow(s.ids[0], from, () => ui().startEdit(s.ids[0], "proxy", { keyed: from === "keys" }));
      ui().openAssign(s.ids);
    },
  },
  {
    ...LIST, id: "profile.regenerate", scope: "selection", group: "organize", icon: RegenerateIcon,
    focusAfter: true,
    label: () => t("commands.regenerate"),
    surfaces: ["bulkMore", "palette", "paletteActions"],
    disabled: (s) => lacks("edit")(s) ?? allLocked(s),
    // It asks first; a cancel is not a failure, and the store's own toast
    // names what it skipped or could not change (Pair 37 review, M2).
    run: async (s) => { await data().regenerateFingerprints(s.ids); },
  },
  {
    // Personal sync, on the rows it can carry (they have `sync`: the personal
    // space, while connected). Every profile syncs while All profiles is on.
    ...LIST, id: "profile.sync", scope: "selection", group: "organize", icon: SyncedIcon,
    label: (n) => (n > 1 ? t("commands.syncProfiles") : t("commands.syncProfile")),
    surfaces: ["rowMenu", "bulkMore", "palette", "paletteActions", "peekMenu"],
    when: (s) => syncConnected() && metas(s).some((p) => p.sync && !p.sync.on),
    // A protected one stays on this computer (the backend refuses it).
    disabled: (s) => noneMay(metas(s).filter((p) => p.sync && !p.sync.on), lockedRefusal),
    run: (s) => setProfilesSync(metas(s).filter((p) => p.sync && !p.sync.on && !isProtected(p)).map((p) => p.id), true),
  },
  {
    ...LIST, id: "profile.stopSync", scope: "selection", group: "organize", icon: SyncOffIcon,
    label: () => t("commands.stopSyncing"),
    surfaces: ["rowMenu", "bulkMore", "palette", "paletteActions", "peekMenu"],
    when: (s) => syncConnected() && metas(s).some((p) => p.sync?.on),
    disabled: (s) => {
      // On for all of them: it cannot be turned off one by one.
      if (useSync.getState().state?.kinds.profiles === "all") return t("commands.syncAllOn");
      // Held by another computer: stopping here would let both run it.
      const on = metas(s).filter((p) => p.sync?.on);
      const held = on.find((p) => p.sync?.open_on);
      if (held && on.every((p) => p.sync?.open_on)) return t("commands.syncStopLeased", { device: held.sync!.open_on! });
      return null;
    },
    run: (s) => setProfilesSync(metas(s).filter((p) => p.sync?.on).map((p) => p.id), false),
  },
  {
    // A profile's own password (#127): offered while Settings allows it, on this computer's own profiles.
    ...LIST, id: "profile.setPassword", scope: "active", group: "organize", icon: PasswordIcon,
    label: () => t("commands.setPassword"),
    surfaces: ["rowMenu", "paletteActions", "peekMenu"],
    when: (s) => passwordsOn() && s.ids.length === 1 && personalRows(s) && metas(s).some((p) => !isProtected(p)),
    // Sync carries a profile open between computers: the backend asks for it off first.
    disabled: (s) => stopFirst(s) ?? (metas(s).some((p) => p.sync?.on) ? t("errors.passwordSyncOn") : null),
    run: async (s) => { const p = metas(s)[0]; if (p) await setPassword(p); },
  },
  {
    // Whatever Settings says: a profile that has one keeps these.
    ...LIST, id: "profile.changePassword", scope: "active", group: "organize", icon: ChangePasswordIcon,
    label: () => t("commands.changePassword"),
    surfaces: ["rowMenu", "paletteActions", "peekMenu"],
    when: oneLocked,
    disabled: stopFirst,
    run: async (s) => { const p = metas(s)[0]; if (p) await changePassword(p); },
  },
  {
    ...LIST, id: "profile.removePassword", scope: "active", group: "organize", icon: RemovePasswordIcon,
    label: () => t("commands.removePassword"),
    surfaces: ["rowMenu", "paletteActions", "peekMenu"],
    when: oneLocked,
    disabled: stopFirst,
    run: async (s) => { const p = metas(s)[0]; if (p) await removePassword(p); },
  },
  {
    ...LIST, id: "profile.launchSynced", scope: "selection", group: "run", icon: SyncIcon,
    label: () => t("commands.launchSynced"),
    surfaces: ["bulkMore", "palette"],
    when: (s) => s.ids.length >= 2,
    disabled: (s) => syncBlockReason(s.ids) || metas(s).map((p) => startRefusal(p, true)).find((r) => r !== null)
      // A protected one starts only on its own: fewer than two left is no group.
      || (metas(s).filter((p) => !isProtected(p)).length < 2 ? allLocked({ ids: metas(s).filter(isProtected).map((p) => p.id) }) : null),
    // A cancel is not a failure; the store's own toast names one (M2).
    run: async (s) => { await data().launchSynced(s.ids); },
  },
  {
    ...LIST, id: "profile.export", scope: "selection", group: "copy", icon: CopyIcon,
    label: () => t("commands.exportJson"),
    surfaces: ["bulkMore", "palette"],
    // A copy of its settings leaves the team: Take out. A protected one's are sealed.
    disabled: (s) => lacks("take_out")(s) ?? allLocked(s),
    run: (s) => data().exportMany(s.ids),
  },
  {
    ...LIST, id: "profile.copyId", scope: "active", group: "copy", icon: IdCardIcon,
    label: () => t("commands.copyId"),
    surfaces: ["rowMenu", "paletteActions", "peekMenu"],
    // A row still being created copies its real id once it has one, never a pending one.
    run: (s) => resolveId(s.ids[0])
      .then((id) => (id ? clip.write(id).then(() => { toast.ok(t("profileRow.idCopied")); }) : undefined))
      .catch((e) => { toast.fail({ title: t("profileRow.copyIdFailed"), detail: e }); }),
  },
  {
    ...LIST, id: "profile.exportCookies", scope: "active", group: "copy", icon: UploadIcon,
    label: () => t("commands.exportCookies"),
    surfaces: ["rowMenu", "paletteActions", "peekMenu"],
    when: personalRows,
    disabled: allLocked,
    run: async (s) => { const p = metas(s)[0]; if (p) await data().exportCookies(p); },
  },
  {
    ...LIST, id: "profile.importCookies", scope: "active", group: "copy", icon: DownloadIcon,
    label: () => t("commands.importCookies"),
    surfaces: ["rowMenu", "paletteActions", "peekMenu"],
    when: personalRows,
    // A running browser holds its cookie store: the item says so before the
    // click (Pair 18 review, M6), rather than a toast after it.
    disabled: (s) => allLocked(s) ?? stopFirst(s),
    run: async (s) => { const p = metas(s)[0]; if (p) await data().importCookies(p); },
  },
  {
    ...LIST, id: "profile.trash", scope: "selection", group: "danger", icon: DeleteIcon, danger: true,
    // A team's go to its server's Trash for 30 days, after a question.
    label: () => (inTeam() ? t("commands.delete") : t("commands.moveToTrash")), keys: ["Delete", "Backspace"], keysJoin: "or",
    surfaces: ["rowMenu", "hover", "bulkBar", "palette", "paletteActions", "peekMenu", "sheet"],
    disabled: lacks("delete"),
    run: async (s) => {
      // The selected ones, read before the optimistic removal prunes them.
      const { selected } = ui();
      const wasSelected = s.ids.filter((id) => selected.has(id));
      const moved = await data().deleteMany(s.ids);
      // Only what reached the Trash leaves the selection; a selected one that
      // failed (already toasted) is put back, still in the list (owner fix 1),
      // whether the subject was the selection or one row of it (M4).
      if (wasSelected.length > 0) ui().settleDeleted(wasSelected, moved);
      const gone = new Set(moved);
      return s.ids.filter((id) => !gone.has(id));
    },
  },
  // The list's own keys, listed for the sheet; the list handles them (keyOwner).
  {
    ...LIST, id: "profile.select", scope: "active", group: "select",
    label: () => t("commands.select"), keys: ["X"],
    surfaces: ["sheet"],
    run: (s) => ui().toggleSelect(s.ids[0]),
  },
  {
    ...LIST, id: "profile.selectAll", scope: "page", group: "select",
    label: () => t("commands.selectAll"), keys: ["Mod+A"],
    surfaces: ["sheet"],
    run: () => ui().selectAll(),
  },
  {
    ...LIST, id: "profile.closePeek", scope: "page", group: "select",
    label: () => t("commands.closePeek"), keys: ["Escape"], keyOwner: "self",
    surfaces: ["sheet"],
  },
  {
    ...LIST, id: "profile.clearSelection", scope: "page", group: "select",
    label: () => t("commands.clearSelection"), keys: ["Escape"], keyOwner: "self",
    surfaces: ["bulkBar", "sheet"],
    run: () => ui().clearSelected(),
  },
  {
    ...LIST, id: "profile.moveThrough", scope: "page", group: "move",
    label: () => t("commands.moveThroughList"), keys: ["ArrowUp", "ArrowDown"], keyOwner: "self",
    surfaces: ["sheet"],
  },
  {
    ...LIST, id: "profile.firstLast", scope: "page", group: "move",
    label: () => t("commands.firstLast"), keys: ["Home", "End"], keysJoin: "or", keyOwner: "self",
    surfaces: ["sheet"],
  },
  {
    ...LIST, id: "profile.selectRange", scope: "page", group: "select",
    label: () => t("commands.selectRange"), keys: ["Shift+ArrowUp", "Shift+ArrowDown"], keyOwner: "self",
    surfaces: ["sheet"],
  },
  {
    // A pointer chord, listed for the sheet (WIG-08): the row handles it.
    // It prints "⌘Click" on macOS and "Ctrl+Click" elsewhere.
    ...LIST, id: "profile.addToSelection", scope: "page", group: "select",
    label: () => t("commands.addToSelection"), keys: ["Mod+Click"], keyOwner: "self",
    surfaces: ["sheet"],
  },
  {
    ...LIST, id: "profile.new", scope: "page", group: "create", icon: AddIcon,
    focusAfter: true,
    label: () => t("commands.newProfile"), keys: ["C"], keyWhere: "page", singleKey: true,
    surfaces: ["palette", "sheet"],
    disabled: createRefusal,
    run: async () => {
      if (await useNav.getState().setSection("profiles")) openProfileForm({ mode: "create" });
    },
  },
  {
    ...LIST, id: "profile.import", scope: "page", group: "create", icon: DownloadIcon,
    label: () => t("commands.importProfiles"),
    surfaces: ["pageMenu", "palette"],
    disabled: createRefusal,
    run: async () => { if (await useNav.getState().setSection("profiles")) await data().importFromClipboard(); },
  },
  // Edit view: a custom View's own filters are locked on the list; this opens
  // them for change, with Save view and Cancel beside the title. Listed only
  // while a custom View is open and not already being edited.
  {
    ...LIST, id: "profile.editView", scope: "page", group: "view", icon: EditIcon,
    label: () => t("commands.editView"),
    surfaces: ["palette"],
    when: () => {
      const view = findView(ui().viewId);
      return !!view && !view.builtin && ui().viewEdit !== view.id && useNav.getState().section === "profiles";
    },
    run: () => { const id = ui().viewId; if (id) ui().editView(id); },
  },
  // The sections on top of the list fold as the sidebar's groups do (owner,
  // 2026-10-01); the palette lists only the one that applies. No key: a
  // page key would be one more single key to learn for a rare act. The icon
  // is the heading's chevron as the act leaves it (right folded, down open),
  // so the label lines up with the palette's other rows.
  ...(["continue", "needs"] as const).flatMap((section): Command[] => {
    const fold = async (folded: boolean) => {
      if (!(await useNav.getState().setSection("profiles"))) return;
      if (section === "continue") setContinueByHand(folded); else useOverviewFolds.getState().setFolded(section, folded);
    };
    // What the view shows: Continue folded in a short window counts as folded.
    const shown = () => viewFolds(useOverviewFolds.getState())[section];
    // The sections are on All profiles only: nowhere else is there one to fold.
    const onAll = () => ui().folder === "all" && ui().viewId === null;
    return [
      {
        ...LIST, id: `profile.fold.${section}`, scope: "page", group: "view", icon: ChevronRightIcon,
        label: () => (section === "continue" ? t("commands.foldContinue") : t("commands.foldNeedsYou")),
        surfaces: ["palette"],
        when: () => onAll() && !shown(),
        run: () => fold(true),
      },
      {
        ...LIST, id: `profile.unfold.${section}`, scope: "page", group: "view", icon: ChevronDownIcon,
        label: () => (section === "continue" ? t("commands.unfoldContinue") : t("commands.unfoldNeedsYou")),
        surfaces: ["palette"],
        when: () => onAll() && shown(),
        run: () => fold(false),
      },
    ];
  }),
];

defineCommands(PROFILE_COMMANDS);
