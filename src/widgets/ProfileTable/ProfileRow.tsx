import { memo, useMemo, useRef, useState, type KeyboardEvent as ReactKeyboardEvent, type MouseEvent as ReactMouseEvent, type ReactNode } from "react";
import { useShallow } from "zustand/react/shallow";
import { flushSync } from "react-dom";
import { focusCellButton, focusProfileRow } from "../../shared/lib/focusRow";
import { Checkbox, clientScale, cn, pageZoom, rectToLayout, swallowFollowUpClick, Tooltip, useLongPress, type ContextMenuApi } from "@proxyshard/shardx-ui-kit";
import { PasswordIcon, PinIconApp, SyncConflictIcon, SyncedIcon } from "../../shared/icons";
import { ProxyFlag } from "../../shared/ui/ProxyFlag";
import { StatusPill } from "../../shared/ui/StatusPill";
import { CellEditButton } from "../../shared/ui/CellEditButton";
import { useLang, useT, type Lang } from "../../shared/i18n";
import { HOST_OS, stampMs } from "../../shared/lib/utils";
import { fmtDuration } from "../../shared/lib/format";
import { slideDelayOf } from "../../shared/lib/arrival";
import { useReducedMotion } from "../../shared/hooks/useReducedMotion";
import {
  formatExactRun, formatRelativeRun,
  isProtected, may, profileFocusWindow,
  profileStatus, useFolderLook, useProfile, useProfileUi, useSaving, PROFILE_EDITABLE, type ProfileColumnId, type ProfileMeta,
} from "../../entities/profile";
import type { ProxyEntry } from "../../entities/proxy";
import { FolderCellMenu, InlineText, ProfileRowActions, ProxyCellPopover } from "../../features/manage-profiles";
import { openProfileForm } from "../../features/profile-form";
import { useProfileTags } from "../../entities/tag";
import { useDialogLife } from "../../shared/lib/useDialogLife";
import { toast } from "../../shared/model/toast";
import { fitTagChips, TAGS_RESERVE, textWidth } from "../../shared/lib/measureText";
import { TagChip } from "../../shared/ui/TagChip";
import { FolderChip } from "../../shared/ui/FolderChip";
import { menuItemsFor, profileRowSubject } from "../../features/commands";
import { pointerMoved } from "../../shared/lib/pointer";
import { rowFill } from "../../shared/ui/rowFill";
import { writeProfileDrag } from "../../shared/lib/profileDrag";

/** The stores' actions, read when used: they never change, so a row
 *  subscribes to none of them (speed lane 2026-10-02: a scroll mounts two
 *  rows a frame, and each subscription is a hook to build and take down). */
const ui = () => useProfileUi.getState();
const store = () => useProfile.getState();

/// One 36 px line of the Profiles list. Positioned by the list (`top`); the
/// open inline editor is drawn by the list, under the row.
/// Memoized: a row renders again only when its own props or store values change (PERF-02).
export const ProfileRow = memo(function ProfileRow({ profile, proxy, top, columns, order, tagsWidth, now, rowIndex, active, inPeek, tabbable, onMenu }: {
  profile: ProfileMeta;
  proxy: ProxyEntry | null;
  /** Offset from the top of the rows box, in px. */
  top: number;
  /** CSS grid-template-columns shared with the header. */
  columns: string;
  /** The data columns on screen, in the header's order (owner feature 2026-09-26). */
  order: readonly ProfileColumnId[];
  /** The Tags column's width, measured once by the list (EMIL-15); null while hidden. */
  tagsWidth: number | null;
  /** The list's shared clock, for the Status cell's relative time. */
  now: number;
  /** 1-based position among all rows, for `aria-rowindex`. */
  rowIndex: number;
  /** The row the keyboard acts on. */
  active: boolean;
  /** The profile Peek shows. */
  inPeek: boolean;
  /** The one row Tab lands on. */
  tabbable: boolean;
  /** Opens the row menu: at the pointer on a right-click, under ⋮ on a
   *  click. While it is open the row carries `data-menu-open`, which keeps
   *  its actions in view (RowHoverActions) without rendering the list (PERF-11). */
  onMenu: ContextMenuApi["open"];
}) {
  const t = useT();
  // Brings this profile's browser window forward. The browser raises itself:
  // on macOS raising another application's window needs an Accessibility
  // grant the operator never gave.
  const showWindow = async () => {
    try {
      await profileFocusWindow(p.id);
    } catch (e) {
      toast.fail({ title: t("profileList.showWindowFailed", { name: p.name }), detail: e, subject: p.name });
    }
  };
  const lang = useLang((s) => s.lang);
  const p = profile;
  // The row's own store values, one subscription per store (a row mounts
  // two a frame on a scroll; each subscription is a hook to build and take
  // down): compared item by item, so the row renders only when one changes.
  const [runningSince, isStarting, cancelled, stopping, failedAt, arrivedAt] = useProfile(useShallow((s) => [
    s.running[p.id], s.startBusy.has(p.id), s.cancelled.has(p.id), s.pending[p.id]?.kind === "stop", s.startFailed[p.id], s.arrived[p.id],
  ] as const));
  // `proxy` is the bound proxy from the list's map: none for a bound id means it was deleted,
  // or, on a team's row, that this member may not see it (it comes with the session).
  const hidden = !!p.proxy_id && !proxy && p.origin === "team";
  const missing = !!p.proxy_id && !proxy && !hidden;
  const proxyNamed = !!proxy?.name && proxy.name !== `${proxy.host}:${proxy.port}`;
  const saving = useSaving((s) => s.ids.has(p.id));
  const status = profileStatus(p, { starting: isStarting, cancelled, stopping, runningSince, failedAt, saving, now, lang, proxyMissing: missing });

  // Live layer (spec 6.1, 6.2). A start from this window lands (Starting,
  // then Running): the row flashes once and the Running dot's halo swells
  // once. Row state, read from the tone's last change, so a row the list
  // mounts again (scrolled back into view) never replays it; any other
  // tone ends it at once, so a quick Stop leaves no stale flash. Each part
  // comes off when it has played (onAnimationEnd); under Reduce motion the
  // swell never starts (the flash is a color fade, and stays). Reduce motion
  // switched on mid-swell removes the animation with no end event: the
  // swell comes off then, so it never replays when Reduce motion goes off
  // again (Tasks 5-6 review, M1).
  const reduced = useReducedMotion();
  const [lastTone, setLastTone] = useState(status.tone);
  const [started, setStarted] = useState<{ flash: boolean; swell: boolean } | null>(null);
  if (status.tone !== lastTone) {
    setLastTone(status.tone);
    setStarted(lastTone === "starting" && status.tone === "running" ? { flash: true, swell: !reduced } : null);
  } else if (reduced && started?.swell) {
    setStarted(started.flash ? { ...started, swell: false } : null);
  }
  // A row that just arrived (a create, an import, a clone, a restore) slides
  // in once. One the list mounts partway through (scrolled into view) picks
  // it up where it is, never from the start.
  const slideDelay = useMemo(() => slideDelayOf(arrivedAt), [arrivedAt]);
  const slides = arrivedAt !== undefined;
  // `editingKeyed`: `P` (a row key) started the edit, not a click on
  // Proxy's own edit button or its cell; its close goes back to the row
  // (amended focus-return ruling, merge-columns-fix1.md), never to a button
  // the key never touched.
  const [isSel, editing, editingKeyed] = useProfileUi(useShallow((s) => [
    s.selected.has(p.id),
    s.editing?.id === p.id ? s.editing.field : null,
    s.editing?.id === p.id ? s.editing.keyed : undefined,
  ] as const));

  // P's picker stays mounted through its exit (FEEL-01): hooks only, no DOM
  // while closed, so the memoized row stays cheap.
  const proxyLife = useDialogLife(editing === "proxy");
  // Fix round 1 (M1): `FolderChip` is pure now, so the row reads the
  // Folder's look and passes its color in.
  const folderLook = useFolderLook(p.folder);
  // On touch, with no hover, holding the row selects it, and so enters
  // selection mode (owner, 2026-10-01). Its click and the platform's menu
  // are the hold's own: no Peek, no row menu.
  const longPress = useLongPress(() => {
    ui().setActive(p.id);
    if (!useProfileUi.getState().selected.has(p.id)) ui().toggleSelect(p.id);
  });
  const hold = longPress.bind(p.id);

  // Shift-presses are handled in mousedown only: a click on the checkbox's
  // <label> reaches the row twice, and applying the range twice would undo it.
  const shiftPress = useRef(false);
  // Cmd-press on macOS, Ctrl-press elsewhere: the press adds or removes the
  // row (WIG-08), so its click must not open Peek or redo it.
  const modPress = useRef(false);
  const proxyCell = useRef<HTMLDivElement>(null);
  const folderCell = useRef<HTMLDivElement>(null);
  const tagsCell = useRef<HTMLDivElement>(null);
  const notesCell = useRef<HTMLDivElement>(null);
  const [folderMenu, setFolderMenu] = useState(false);
  const tagCount = useProfileTags(p.id).length;
  // An inline edit hands the keyboard back to its row when it ends, unless the
  // user already put it somewhere else (clicked the filter, another row).
  // Fix round 1 (I4): with a cell ref, back to that cell's own edit button
  // (once it re-renders), not just the row; Name has no button (Ruling 11),
  // so it keeps the plain row-level fallback.
  const endEdit = (cellRef?: React.RefObject<HTMLElement | null>) => {
    ui().stopEdit();
    if (cellRef) focusCellButton(cellRef, p.id);
    else focusProfileRow(p.id, { ifLost: true });
  };
  // One fill per row state (E13, O5): the target (the row the keys act on,
  // which the pointer picks by moving onto it) is always drawn; the Peek
  // row, when it is not the target, one step quieter; a selected target
  // keeps its selected tint with the active fill laid over it. No hover
  // fill. The hover actions and the cells' edit buttons take the same tone,
  // so their fades never show a seam (fix round 1, I1).
  const look = rowFill({ selected: isSel, active, inPeek });
  const tone = look.tone;
  // What only a hover or a focus shows is built once the row is engaged
  // (the pointer comes onto it, focus goes into it) or while it is the
  // target: the hover actions' buttons, and the fade and fill behind each
  // cell's edit button (the edit buttons themselves stay, named and
  // reachable). Until then all of it is hidden, and a scroll would build it
  // for every row it brings in. Built in the pointer's own event, so no
  // frame shows the strip or an edit button without them.
  const [engaged, setEngaged] = useState(false);
  const built = engaged || active;
  const engage = () => { if (!engaged) flushSync(() => setEngaged(true)); };

  // The registry's menu for this row (UI-29, CRAFT-10): the whole selection
  // when the row is part of it, else this row alone, and its heading says
  // which (ruling 4, the Finder and Linear rule). Built when it opens.
  const openMenu = (e: React.MouseEvent) => {
    const subject = profileRowSubject(p.id);
    const heading = subject.ids.length > 1 ? t("commands.nProfiles", { n: subject.ids.length }) : p.name;
    onMenu(e, menuItemsFor("rowMenu", "profiles", subject), { heading });
  };

  // A team profile without Edit is read-only here: no cell edits it.
  const canEdit = may(p, "edit");
  // Protected by a password (#127): its proxy and notes are sealed, and change only in its editor.
  const locked = isProtected(p);
  // Each editable cell's edit button (owner feature 2026-09-26): it opens
  // that field's own editor at the cell. "Add" when the cell is empty.
  const editOf = (id: ProfileColumnId, lead = false): ReactNode => {
    if (!canEdit) return null;
    const tabIndex = active ? 0 : -1;
    switch (id) {
      case "folder":
        return (
          <CellEditButton
            lead={lead}
            tone={tone}
            decorated={built}
            tabIndex={tabIndex}
            label={p.folder ? t("cellEdit.editFolder", { name: p.name }) : t("cellEdit.addFolder", { name: p.name })}
            onEdit={() => { ui().setActive(p.id); setFolderMenu(true); }}
          />
        );
      case "tags":
        return (
          <CellEditButton
            lead={lead}
            tone={tone}
            decorated={built}
            tabIndex={tabIndex}
            label={tagCount > 0 ? t("cellEdit.editTags", { name: p.name }) : t("cellEdit.addTags", { name: p.name })}
            onEdit={() => { ui().setActive(p.id); ui().setTagPicker({ profileIds: [p.id], from: "list" }); }}
          />
        );
      case "proxy":
        if (locked) return null;
        return <CellEditButton lead={lead} tone={tone} decorated={built} tabIndex={tabIndex} label={t("cellEdit.editProxy", { name: p.name })} onEdit={() => ui().startEdit(p.id, "proxy")} />;
      case "notes":
        if (locked) return null;
        return (
          <CellEditButton
            lead={lead}
            tone={tone}
            decorated={built}
            tabIndex={tabIndex}
            label={p.notes ? t("cellEdit.editNotes", { name: p.name }) : t("cellEdit.addNotes", { name: p.name })}
            onEdit={() => ui().startEdit(p.id, "notes")}
          />
        );
      default:
        return null;
    }
  };
  // What a double-click on a cell edits, as its edit button and its key do
  // (EMIL-12; PC-041: a double-click on Name renames). The first click names
  // the cell: the Peek it opens may move the columns before the second.
  const doubleClickEdit = (id: string | undefined): (() => void) | null => {
    if (editing) return null;
    // Read-only: the name still opens the form, to read; any other cell does nothing more.
    if (!canEdit && id !== "name") return () => {};
    switch (id) {
      // The name opens the profile form: every field, saved by hand (rename is E).
      case "name": return () => openProfileForm({ mode: "edit", id: p.id });
      case "folder": return () => { ui().setActive(p.id); setFolderMenu(true); };
      case "tags": return () => ui().setTagPicker({ profileIds: [p.id], from: "list" });
      case "proxy": case "notes": return locked ? () => {} : () => ui().startEdit(p.id, id);
      // Any other cell: its click opened Peek, and the second press does
      // nothing more (it never lands on what slid under the pointer).
      default: return () => {};
    }
  };
  // The row actions float over the last column: its button rides with them.
  const last = order[order.length - 1];
  const own = (id: ProfileColumnId) => (id === last ? null : editOf(id));
  // Tracked by position, because the row actions take the pointer there. A
  // DOM attribute, not state: a pointer move never renders the row.
  const trackLast = (e: React.PointerEvent<HTMLDivElement>) => {
    const rowEl = e.currentTarget;
    const was = rowEl.dataset.overLast !== undefined;
    const lastCell = last && PROFILE_EDITABLE.has(last) ? rowEl.querySelector<HTMLElement>(`[data-cell="${last}"]`) : null;
    if (!lastCell) { if (was) delete rowEl.dataset.overLast; return; }
    // Once shown, the button's own left edge counts too: moving onto it never hides it.
    const lead = was ? rowEl.querySelector<HTMLElement>("[data-row-lead]") : null;
    // Layout px on both sides: the pointer by the page zoom, rects by their
    // own scale, read once per move (each read measures <html>).
    const k = clientScale();
    const left = Math.min(rectToLayout(lastCell.getBoundingClientRect(), k).left, lead ? rectToLayout(lead.getBoundingClientRect(), k).left : Infinity);
    const now = e.clientX / pageZoom() >= left;
    // Fix round 1 (M7): a hovered row is not re-invalidated on every move.
    if (now === was) return;
    if (now) rowEl.dataset.overLast = "";
    else delete rowEl.dataset.overLast;
  };

  // One cell per data column, drawn in the header's order (owner feature
  // 2026-09-26). Each case is the cell as it was; only its place moved.
  const cell = (id: ProfileColumnId): ReactNode => {
    switch (id) {
      case "name":
        return (
          <div
            key="name"
            role="gridcell"
            data-cell="name"
            // Pinned at the card's left while the list scrolls sideways. The
            // name's type is the cell's, so its inline rename field (which
            // takes the row's type, FEEL-29) keeps it too.
            className="list-pin-name flex min-w-0 items-center gap-1.5 self-stretch text-ui"
          >
            {editing === "name" ? (
              <InlineText
                value={p.name}
                label={t("profileRow.nameLabel")}
                subject={p.name}
                required
                onCommit={async (name) => {
                  endEdit();
                  // Shown by the press that saved it; a failed save says so itself and puts the old name back.
                  const ok = await store().renameProfile(p.id, name);
                  if (ok) toast.saved(t("cellEdit.saved", { name }));
                  return ok;
                }}
                onCancel={endEdit}
              />
            ) : (
              <>
                {/* Name is as wide as its names need (owner, 2026-09-30): a
                    rare long one is cut, and shows whole on hover. */}
                <Tooltip content={p.name} whenTruncated warm={false} delay={700}>
                  <span data-name className="truncate">{p.name}</span>
                </Tooltip>
                {locked && (
                  // Muted after the name, as the pin: said by word too, for a screen reader.
                  <Tooltip content={t("profileRow.protected")} warm={false} delay={700}>
                    <span data-protected className="inline-flex shrink-0 text-icon-soft-400">
                      <PasswordIcon aria-hidden="true" className="size-3.5" />
                      <span className="sr-only">{t("profileRow.protected")}</span>
                    </span>
                  </Tooltip>
                )}
                {p.pinned && (
                  // After the name, muted: every name starts at the same x,
                  // pinned or not, and a rename never moves it (CRAFT-15).
                  <Tooltip content={t("profileRow.pinnedTitle")} warm={false} delay={700}>
                    <span data-pin className="inline-flex shrink-0 text-icon-soft-400"><PinIconApp aria-hidden="true" className="size-3.5" /></span>
                  </Tooltip>
                )}
                {p.sync?.on && (
                  // Synced across the person's devices: said by word too, for a screen reader.
                  // A conflict copy has its own glyph and warns not to run it beside the original.
                  <Tooltip content={p.sync.conflict ? t("profileRow.syncedConflict") : t("profileRow.synced")} warm={false} delay={700}>
                    <span data-synced={p.sync.conflict ? "conflict" : "on"} className={cn("inline-flex shrink-0", p.sync.conflict ? "text-warning-base" : "text-icon-soft-400")}>
                      {p.sync.conflict ? <SyncConflictIcon aria-hidden="true" className="size-3.5" /> : <SyncedIcon aria-hidden="true" className="size-3.5" />}
                      <span className="sr-only">{p.sync.conflict ? t("profileRow.syncedConflict") : t("profileRow.synced")}</span>
                    </span>
                  </Tooltip>
                )}
              </>
            )}
          </div>
        );
      case "status":
        return (
          <div key="status" role="gridcell" data-cell="status" className="flex min-w-0 items-center">
            {/* Cold and patient, as every list cell's tooltip: it informs,
                and never follows the pointer down the list (EMIL-14). */}
            <Tooltip content={status.detail} warm={false} delay={700}>
              {/* The kit Tooltip only describes the cell while it is open
                  (hover, or a keyboard focus this pill never gets), so the
                  exact detail is also a visually-hidden sibling of the word:
                  read by a screen reader without "opening" anything, and part
                  of the gridcell's ordinary accessible name. */}
              <span className="flex min-w-0 max-w-full">
                {/* Running means there is a window somewhere, and finding it
                    among a dozen others is the operator's problem until this
                    word is clickable. Only while running: every other status
                    here is a statement, not an action. A span with a role
                    rather than a button, because the row is a grid cell and
                    the kit's button would bring its own box into it. */}
                <span
                  {...(status.tone === "running"
                    ? {
                        "data-focus-window": true,
                        role: "button",
                        tabIndex: 0,
                        "aria-label": t("profileList.showWindow", { name: p.name }),
                        className: "flex min-w-0 max-w-full cursor-pointer",
                        onClick: (e: ReactMouseEvent) => { e.stopPropagation(); void showWindow(); },
                        onKeyDown: (e: ReactKeyboardEvent) => {
                          if (e.key === "Enter" || e.key === " ") {
                            e.preventDefault();
                            e.stopPropagation();
                            void showWindow();
                          }
                        },
                      }
                    : { className: "flex min-w-0 max-w-full" })}
                >
                  <StatusPill status={status} swell={!!started?.swell} />
                </span>
                {/* No leading punctuation: the accessible-name algorithm already
                    joins this with the visible word with a space. */}
                {status.detail && <span className="sr-only">{status.detail}</span>}
              </span>
            </Tooltip>
          </div>
        );
      case "folder":
        return (
          <div key="folder" ref={folderCell} role="gridcell" data-cell="folder" className="group/cell @container relative flex min-w-0 items-center gap-1 self-stretch">
            <span className="relative flex min-w-0 items-center">
              {p.folder && <span className="flex min-w-0"><FolderChip name={p.folder} color={folderLook.color} wholeOnHover /></span>}
              {own("folder")}
            </span>
          </div>
        );
      case "tags":
        return (
          <div
            key="tags"
            ref={tagsCell}
            role="gridcell"
            data-cell="tags"
            className="group/cell @container relative flex min-w-0 items-center gap-1 self-stretch"
          >
            <span className="relative flex min-w-0 items-center">
              <TagChips profileId={p.id} width={tagsWidth} />
              {own("tags")}
            </span>
          </div>
        );
      case "proxy":
        // "Change proxy" moved into the edit button's own name.
        return (
          <Tooltip key="proxy" content={locked ? t("profileRow.lockedHint") : missing ? t("profileRow.proxyMissingHint") : hidden ? t("profileRow.proxyHiddenHint") : undefined}>
            <div
              ref={proxyCell}
              role="gridcell"
              data-cell="proxy"
              data-proxy-missing={missing || undefined}
              className="group/cell @container relative flex min-w-0 items-center gap-1.5 self-stretch"
            >
              {/* The edit button sits right after what the cell shows (CellEditButton). */}
              <span className="relative flex min-w-0 items-center">
                {proxy ? (
                  // The proxy's name, as Proxies and Peek show it (CRAFT-13); an
                  // unnamed one (or one named by its address, as a pasted line
                  // is) shows its host, cut before the port, never after
                  // (TYPE-07). The whole address and the type are in the tooltip.
                  <Tooltip
                    warm={false}
                    delay={700}
                    content={
                      <>
                        {proxyNamed && <>{proxy.name}<br /></>}
                        <span className="mono">{proxy.host}:{proxy.port}</span>
                        <br />
                        {proxy.kind.toUpperCase()}
                      </>
                    }
                  >
                    <span className="flex min-w-0 items-center gap-1.5">
                      <ProxyFlag cc={proxy.country} />
                      {proxyNamed ? (
                        <span data-proxy-name className="truncate text-secondary text-text-sub-600">{proxy.name}</span>
                      ) : (
                        <span data-proxy-address className="flex min-w-0 text-mono text-text-sub-600">
                          <span className="truncate">{proxy.host}</span>
                          <span data-port className="shrink-0">:{proxy.port}</span>
                        </span>
                      )}
                    </span>
                  </Tooltip>
                ) : missing ? (
                  // Plain text, no dot here: the Status column already carries the
                  // warning dot for this state (controller ruling, Task 8 — a dot
                  // in two cells would duplicate it).
                  <span className="text-secondary text-text-strong-950">{t("profileRow.proxyMissing")}</span>
                ) : hidden ? (
                  <span data-proxy-hidden className="truncate text-secondary text-text-sub-600">{t("profileRow.proxyHidden")}</span>
                ) : locked ? (
                  <span data-proxy-locked className="truncate text-secondary text-text-soft-400">{t("profileRow.locked")}</span>
                ) : (
                  <span className="text-secondary text-text-soft-400">{t("profileRow.direct")}</span>
                )}
                {own("proxy")}
              </span>
            </div>
          </Tooltip>
        );
      case "lastRun": {
        const at = stampMs(p.last_launched_at);
        return (
          <div key="lastRun" role="gridcell" data-cell="lastRun" className="min-w-0 truncate whitespace-nowrap text-secondary text-text-sub-600">
            {at === null ? <span className="text-text-soft-400">{t("peek.never")}</span> : <When at={at} now={now} lang={lang} />}
          </div>
        );
      }
      case "created": {
        const at = stampMs(p.created_at);
        return (
          <div key="created" role="gridcell" data-cell="created" className="min-w-0 truncate whitespace-nowrap text-secondary text-text-sub-600">
            {at === null ? <span className="text-text-soft-400">—</span> : <When at={at} now={now} lang={lang} />}
          </div>
        );
      }
      case "runTime": {
        // The stored total, plus the session still running (the list's
        // clock ticks once a minute, so this stays true while it runs).
        const live = runningSince !== undefined && Number.isFinite(runningSince) ? Math.max(0, now - runningSince) : 0;
        const ms = p.total_runtime_ms + live;
        const total = ms > 0 ? fmtDuration(ms, lang) : null;
        return (
          <div key="runTime" role="gridcell" data-cell="runTime" className="min-w-0 truncate whitespace-nowrap text-secondary tabular-nums text-text-sub-600">
            {/* Sized to its content like the other columns: a cut total shows whole on hover. */}
            {total ? (
              <Tooltip content={total} whenTruncated warm={false}>
                <span className="block truncate">{total}</span>
              </Tooltip>
            ) : <span className="text-text-soft-400">—</span>}
          </div>
        );
      }
      case "device":
        return (
          <div key="device" role="gridcell" data-cell="device" className="min-w-0 truncate text-secondary text-text-sub-600">
            {locked ? <span className="text-text-soft-400">—</span> : p.mobile ? t("profileTable.devicePhone") : t("profileTable.deviceDesktop")}
          </div>
        );
      case "extensions":
        return (
          <div
            key="extensions"
            role="gridcell"
            data-cell="extensions"
            className={cn("text-secondary tabular-nums", p.extensions.length > 0 ? "text-text-sub-600" : "text-text-soft-400")}
          >
            {locked ? "—" : p.extensions.length}
          </div>
        );
      case "notes":
        return editing === "notes" ? (
          <div key="notes" ref={notesCell} role="gridcell" data-cell="notes" className="relative min-w-0">
            <InlineText
              value={p.notes ?? ""}
              label={t("profileRow.notesLabel")}
              subject={p.name}
              onCommit={async (notes) => {
                endEdit(notesCell);
                const ok = await store().setNotes(p.id, notes);
                if (ok) toast.saved(t("cellEdit.saved", { name: p.name }));
                return ok;
              }}
              onCancel={() => endEdit(notesCell)}
            />
          </div>
        ) : (
          <div
            key="notes"
            ref={notesCell}
            role="gridcell"
            data-cell="notes"
            className="group/cell @container relative flex min-w-0 items-center gap-1 self-stretch text-secondary text-text-sub-600"
          >
            <span className="relative flex min-w-0 items-center">
              {/* A cut note shows whole on hover, only when it is cut, and
                  only the note (EMIL-14): the edit button names the edit. */}
              {locked ? (
                // Sealed with the profile: said, not edited here (its editor changes it).
                <Tooltip content={t("profileRow.lockedHint")} warm={false} delay={700}>
                  <span data-notes-locked className="min-w-0 truncate text-text-soft-400">{t("profileRow.locked")}</span>
                </Tooltip>
              ) : (
                <Tooltip content={p.notes || undefined} whenTruncated warm={false} delay={700}>
                  <span className="min-w-0 truncate">{p.notes || <span className="text-text-soft-400">—</span>}</span>
                </Tooltip>
              )}
              {own("notes")}
            </span>
          </div>
        );
    }
  };

  return (
    <div
      role="row"
      aria-level={1}
      aria-rowindex={rowIndex}
      aria-selected={isSel}
      data-profile-id={p.id}
      data-active={active || undefined}
      data-peek-open={inPeek || undefined}
      // A click anywhere on the row opens it in Peek (O5): the pointer says
      // so, except on the row already in Peek or one being edited, where a
      // click does nothing (Pair 40 review, M2).
      data-clickable={(!inPeek && !editing) || undefined}
      // An inline editor (a rename) holds the row: no hover box beside it.
      data-editing={editing ? "" : undefined}
      tabIndex={tabbable ? 0 : -1}
      onFocus={(e) => {
        if (!engaged) setEngaged(true);
        if (e.target === e.currentTarget && !active) ui().setActive(p.id);
      }}
      onPointerEnter={engage}
      onMouseEnter={engage}
      // A pinned row draws like any other (owner, 2026-10-01). The
      // slide-in's tint is a layer over the whole row (the
      // kit's ::before, lifted over the pinned cells); the flash is a layer
      // of its own (the first child), so a row that arrives and starts
      // within its second shows both (L3 review, N4).
      className={cn(
        "row-line group absolute inset-x-0 grid h-[36px] items-center gap-[10px] list-select-row text-secondary text-text-strong-950 focus-ring-inset aria-selected:forced-selected",
        look.row,
        slides && "slide-in",
      )}
      style={{ top, gridTemplateColumns: columns, animationDelay: slides && slideDelay > 0 ? `-${slideDelay}ms` : undefined }}
      onAnimationEnd={(e) => {
        const done = e.animationName === "row-flash" ? "flash" : e.animationName === "halo-swell" ? "swell" : null;
        if (done && started?.[done]) {
          const next = { ...started, [done]: false };
          setStarted(next.flash || next.swell ? next : null);
        }
      }}
      onContextMenu={(e) => {
        if (longPress.claimMenu(e, p.id)) return;
        // A right-click makes this the row the keys act on, with the keyboard
        // on it, and leaves the selection as it is; the menu's own keys act
        // on this row (Pair 30 review, Important 2). From the keys (no
        // pointer), focus is already in the row: it stays where it is.
        ui().setActive(p.id);
        if (e.clientX !== 0 || e.clientY !== 0) e.currentTarget.focus({ preventScroll: true });
        openMenu(e);
      }}
      onPointerDown={hold.onPointerDown}
      onPointerUp={hold.onPointerUp}
      onPointerCancel={hold.onPointerCancel}
      onPointerMove={(e) => {
        engage();
        hold.onPointerMove(e);
        trackLast(e);
        // The row under a moving pointer is the one the keys act on (O5,
        // EMIL-03). A list scrolling under a still pointer changes nothing
        // (ruling 7); it never moves Peek (ruling 6).
        if (!pointerMoved(e) || e.pointerType === "touch" || active) return;
        // The keyboard follows only when it was on a row: never out of a
        // field, an inline editor or Peek.
        const focused = document.activeElement;
        if (focused instanceof HTMLElement && focused !== e.currentTarget && focused.matches("[role='row'][data-profile-id]")) {
          e.currentTarget.focus({ preventScroll: true });
        }
        // After the focus, whose handler marks the row as the keyboard's:
        // the pointer made it active, so the palette's first group does not
        // follow it (Pair 44 ruling: an idle mouse changes nothing there).
        ui().setActive(p.id, { pointer: true });
      }}
      onPointerLeave={(e) => { delete e.currentTarget.dataset.overLast; }}
      // A key's click (Space on the checkbox) follows a keydown: never a
      // modified press, whatever the last mouse press was (Pair 40 review, I2).
      onKeyDownCapture={() => { shiftPress.current = false; modPress.current = false; }}
      // Buttons keep their own meaning, and so does a press in a layer the
      // row opened (a menu, a picker: React hands their events up here).
      onMouseDown={(e) => {
        const target = e.target as HTMLElement;
        // Every press starts clean, one in such a layer too (I2).
        shiftPress.current = false;
        modPress.current = false;
        if (!e.currentTarget.contains(target)) return;
        // A press chooses the row, even one the pointer already made active (E13).
        if (e.button === 0) ui().setActive(p.id);
        const onControl = !!target.closest("button, a");
        shiftPress.current = e.button === 0 && e.shiftKey && !onControl;
        // Cmd-click on macOS, Ctrl-click elsewhere adds or removes the row
        // (WIG-08). Ctrl-click on macOS is the secondary click: the menu's.
        modPress.current = e.button === 0 && !e.shiftKey && (HOST_OS === "macOS" ? e.metaKey : e.ctrlKey) && !onControl;
        // Stops the text selection a shift-drag would otherwise begin, and the
        // label activation that would reach the checkbox.
        if (shiftPress.current) { e.preventDefault(); ui().selectRangeTo(p.id); return; }
        if (modPress.current) { e.preventDefault(); ui().toggleSelect(p.id); return; }
        // A second or third press never selects a word of the row.
        if (e.detail > 1 && !onControl && !target.closest("input, textarea, label")) e.preventDefault();
      }}
      onClick={(e) => {
        // A long press selected the row; its click opens nothing.
        if (longPress.took(p.id)) { e.preventDefault(); return; }
        // The press already did a range or a toggle; its click must not redo it.
        if (shiftPress.current || modPress.current) { e.preventDefault(); return; }
        // On macOS a Ctrl-click is the secondary click: the menu's (I3).
        if (HOST_OS === "macOS" && e.ctrlKey) return;
        const target = e.target as HTMLElement;
        // Controls keep their meaning; an open editor keeps its row; a click
        // in a layer the row opened is that layer's.
        if (editing || !e.currentTarget.contains(target)) return;
        if (target.closest("button, a, input, label, [role='button'], [data-cell='checkbox']")) return;
        // A click anywhere on the row opens it in Peek and never closes it
        // (O5); an edit starts from a key, a double-click or a cell's edit
        // button, never a plain click (EMIL-12).
        if (!inPeek) ui().openPeek(p.id);
        // The Peek that opens narrows the list, so the second press of a
        // double-click may land on another cell, a hover button or Peek
        // itself: the kit guard takes it before anything under it sees it,
        // and it edits the cell this click named. Only after an unmodified
        // primary click (Pair 40 review, I1, I3, M1).
        if (e.detail === 1 && e.button === 0 && !e.shiftKey && !e.metaKey && !e.ctrlKey && !e.altKey) {
          const edit = doubleClickEdit(target.closest<HTMLElement>("[data-cell]")?.dataset.cell);
          if (edit) swallowFollowUpClick(e, { onFollow: edit });
        }
      }}
      draggable={!editing}
      onDragStart={(e) => {
        // A selected row carries the whole selection, the one the bar counts
        // (IFACE-05); any other row, itself. The chip says which.
        const { selected } = useProfileUi.getState();
        const ids = isSel && selected.size > 1 ? [...selected] : [p.id];
        writeProfileDrag(e.dataTransfer, ids, ids.length > 1 ? t("commands.nProfiles", { n: ids.length }) : p.name || p.id.slice(0, 8));
      }}
    >
      {/* A started row's flash: out of the grid's flow, over the row (app.css). */}
      {started?.flash && <span aria-hidden="true" className="row-flash pointer-events-none absolute inset-0" />}
      <div role="gridcell" data-cell="checkbox" className="list-pin-check flex items-center self-stretch">
        {/* Pinned at the card's left while the list scrolls sideways, in
            its own slot before Name in every state (owner, 2026-10-07): at
            rest the box shows only on hover, never over the name; in
            selection mode every box shows (`list-select-box`, the kit's
            tokens.css: the list's class, never this row's state). */}
        <span className="list-select-box">
          <Checkbox
            aria-label={t("profileRow.select", { name: p.name })}
            // Row controls join the Tab order only on the active row (roving tabindex).
            tabIndex={active ? 0 : -1}
            checked={isSel}
            onChange={() => { if (!shiftPress.current && !modPress.current) ui().toggleSelect(p.id); }}
          />
        </span>
      </div>
      {order.map(cell)}
      <div role="gridcell" data-cell="actions" className="list-pin-end self-stretch">
        <ProfileRowActions
          profile={p}
          tabIndex={active ? 0 : -1}
          editing={editing !== null}
          tone={tone}
          built={built}
          // The first row's Start is the one the tour points at.
          tourStart={rowIndex === 2}
          lead={built && last && PROFILE_EDITABLE.has(last) ? editOf(last, true) : undefined}
          onMore={(e) => { e.stopPropagation(); openMenu(e); }}
        />
      </div>
      {/* Anchored at the Proxy cell (it can't be hidden), and never mounted
          without one (fix round 1, Important 3). */}
      {proxyLife.mounted && proxyCell.current && (
        <ProxyCellPopover key={proxyLife.session} open={editing === "proxy"} profile={p} anchorRef={proxyCell} onClose={() => endEdit(!editingKeyed ? proxyCell : undefined)} />
      )}
      {folderMenu && <FolderCellMenu profile={p} anchorRef={folderCell} onClose={() => { setFolderMenu(false); focusCellButton(folderCell, p.id); }} />}
    </div>
  );
});

/** The chips' font, as the page draws them (the row's sans stack at the
 *  secondary role's 12 px), read once. */
let chipFont: string | null = null;
const chipFontOf = () => (chipFont ??= `400 12px ${getComputedStyle(document.body).fontFamily}`);

/** As many of a profile's tags as fit whole, then "+N" (EMIL-15); the rest
 *  are named in the tooltip and to screen readers (PC-089). `width` is the
 *  Tags column's, measured once by the list; null while it is unknown. */
function TagChips({ profileId, width }: { profileId: string; width: number | null }) {
  const tags = useProfileTags(profileId);
  if (tags.length === 0) return null;
  const fit = width === null ? 2 : fitTagChips(tags.map((x) => x.name), width - TAGS_RESERVE, (s) => textWidth(s, chipFontOf()));
  const shown = tags.slice(0, fit);
  const rest = tags.slice(fit);
  const restNames = rest.map((x) => x.name).join(", ");
  // A chip still cut by its column's width (one tag wider than the column)
  // names every tag of the row (autofit review I1).
  const all = tags.map((x) => x.name).join(", ");
  return (
    <span className="flex min-w-0 items-center gap-1">
      {shown.map((tag) => <TagChip key={tag.id} tag={tag} wholeOnHover={all} />)}
      {rest.length > 0 && (
        <Tooltip content={restNames} warm={false} delay={700}>
          <span data-more-tags className="shrink-0 text-secondary text-text-soft-400">
            <span aria-hidden="true">+{rest.length}</span>
            <span className="sr-only">{restNames}</span>
          </span>
        </Tooltip>
      )}
    </span>
  );
}

/** A time as the rest of the list says it ("3 days ago", "Aug 2"), with the
 *  exact date in the tooltip and, for a screen reader, in the cell's own
 *  name: the Status cell's pattern. */
function When({ at, now, lang }: { at: number; now: number; lang: Lang }) {
  const exact = formatExactRun(at, lang);
  return (
    <Tooltip content={exact} warm={false} delay={700}>
      <span>
        {formatRelativeRun(at, now, lang)}
        <span className="sr-only">{exact}</span>
      </span>
    </Tooltip>
  );
}

