import { useEffect, useId, useRef, useState, type ReactNode } from "react";
import { cn, IconButton, layoutRect, layoutToPointer, Menu, Portal, type MenuItem } from "@proxyshard/shardx-ui-kit";
import { ColumnsIcon, MoveLeftIcon, MoveRightIcon, RefreshIcon, SortAscIcon, SortDescIcon, UndoIcon } from "../../icons";
import { useT } from "../../i18n";
import {
  columnChoices, isDefaultSet, moveBy, setShown, visibleOf,
  type ColumnSet, type ColumnsApi, type TableLayout,
} from "../../lib/tableColumns";
import { useColumnDrag } from "./useColumnDrag";
import { useColumnResize } from "./useColumnResize";
import { LIST_HEADER_HEIGHT } from "../listMetrics";

/** The header row's height, px (`height` below, by default): a list's rows
 *  start under it (listMetrics). */
export { LIST_HEADER_HEIGHT };

/** How a list is ordered by its columns: the sort marks, and each column menu's orders. */
export type ColumnSort<Id extends string> = {
  /** `aria-sort` for the column the list is ordered by, else undefined. */
  of: (id: Id) => "ascending" | "descending" | "other" | undefined;
  /** The orders a column offers in its menu (radio items). */
  items: (id: Id) => MenuItem[];
};

type Opened<Id extends string> = {
  anchor: { x: number; y: number } | React.RefObject<HTMLElement | null>;
  /** The column the menu is for; null for the checklist alone. */
  column: Id | null;
  fromButton: boolean;
  /** Every opening is a new menu (audit Phase A Task 4's rule). */
  session: number;
};

/// The header row the Profiles and Proxies lists share (owner feature
/// 2026-09-26): the data columns' titles in the list's order, and their
/// menus. A right-click on a title opens that column's menu at the pointer
/// (Move left, Move right, Reset width, its orders, then the checklist); a
/// right-click anywhere else on the header, or the columns button over the
/// actions column, opens the checklist alone. The checklist is the list a
/// Display menu shows too (`columnChoices`). A resize edge and the columns
/// button keep their own keys: the list keys skip `[data-list-header]
/// :is([data-col], .list-pin-end)`; the select-all box is not one of these,
/// so it keeps the list's keys, like a row checkbox (C6). A list wider than
/// its card scrolls sideways under this row as its rows do: the select-all
/// box, Name and the columns button stay pinned (`list-pin-*` in app.css).
export function ListHeader<Id extends string>({ api, layout, listRef, wrapRef, lead, actionsLabel, excludedNote, sort, height = LIST_HEADER_HEIGHT }: {
  api: ColumnsApi<Id>;
  layout: TableLayout<Id>;
  /** The list's scroll box: a live resize sets its column variables. */
  listRef: React.RefObject<HTMLElement | null>;
  /** The box that holds the list's min-width: it grows with a column dragged wider. */
  wrapRef: React.RefObject<HTMLElement | null>;
  /** The select-all box; none for a list that selects nothing (its layout has no lead track). */
  lead?: ReactNode;
  /** Names the actions column's header cell. */
  actionsLabel: string;
  /** Why a chosen column the list leaves out here is not shown (Folder inside a Folder). */
  excludedNote?: string | ((id: Id) => string | undefined);
  sort?: ColumnSort<Id>;
  /** Px. A list whose header was shorter before Phase B keeps that (the
   *  Trash's 27, owner: nothing grows); the 24 px Columns button fits. */
  height?: number;
}) {
  const t = useT();
  const uid = useId();
  const byId = new Map(api.defs.map((d) => [d.id, d]));
  const titleOf = (id: Id) => byId.get(id)!.title(t);
  const buttonRef = useRef<HTMLButtonElement>(null);
  const [menu, setMenu] = useState<Opened<Id> | null>(null);
  // Apart from `menu`, so a closing menu keeps its items through its fade.
  const [menuOpen, setMenuOpen] = useState(false);
  const [said, setSaid] = useState("");

  const openMenu = (anchor: Opened<Id>["anchor"], column: Id | null, fromButton = false) => {
    setMenu((m) => ({ anchor, column, fromButton, session: (m?.session ?? 0) + 1 }));
    setMenuOpen(true);
  };
  // "Tags moved to position 5 of 6": what a keyboard move (and a drag) says.
  const announceMove = (next: ColumnSet<Id>, id: Id) => {
    const onScreen = visibleOf(next).filter((x) => layout.shown.includes(x));
    setSaid(t("tableColumns.moved", { column: titleOf(id), n: onScreen.indexOf(id) + 1, total: onScreen.length }));
  };
  const move = (id: Id, step: -1 | 1) => {
    const next = moveBy(api.defs, api.set, id, step, layout.shown);
    if (!next) return;
    api.setSet(next);
    announceMove(next, id);
  };

  const rowRef = useRef<HTMLDivElement>(null);
  // The header is a material (Live layer, spec 7): `data-scrolled` while
  // rows pass under it brings its scroll-edge fade in. Set from the grid's
  // own scroll, at most once a frame, on the element (a scroll never
  // renders the list), and read once as it mounts over a scrolled grid.
  // A passive effect: the grid is this row's ancestor, and its ref is set
  // only after this component's layout effects have run.
  useEffect(() => {
    const list = listRef.current;
    const row = rowRef.current;
    if (!list || !row) return;
    let frame = 0;
    const mark = () => {
      frame = 0;
      row.toggleAttribute("data-scrolled", list.scrollTop > 0);
    };
    const onScroll = () => {
      if (!frame) frame = requestAnimationFrame(mark);
    };
    mark();
    list.addEventListener("scroll", onScroll, { passive: true });
    return () => {
      list.removeEventListener("scroll", onScroll);
      cancelAnimationFrame(frame);
    };
  }, [listRef]);
  const startResize = useColumnResize({ api, layout, listRef, wrapRef });
  const drag = useColumnDrag({ api, rowRef, onMoved: announceMove });
  // The width on screen: what the keys step and the edge reports.
  const widthOf = (id: Id) => Math.round(layout.drawn[id] ?? byId.get(id)!.need);
  // A stretched row's last edge is the row's end: only the keys move it (from its left).
  const fixedEnd = (id: Id) => layout.stretched && id === layout.shown[layout.shown.length - 1];
  // Hiding the column a menu belongs to removes its anchor with it (its
  // edge unmounts): close the menu and land focus on the column that now
  // stands where it was — the next one on screen, else the previous, else
  // the Columns button (fix round 1 item 3). The Columns button's own
  // checklist never needs this: its anchor never goes away.
  const closeOwnMenu = (id: Id) => {
    const shown = layout.shown;
    const at = shown.indexOf(id);
    const neighbor = shown[at + 1] ?? shown[at - 1] ?? null;
    setMenuOpen(false);
    // The neighbor's edge (or the Columns button) exists only after this
    // render commits the column's removal.
    requestAnimationFrame(() => {
      const el = neighbor
        ? rowRef.current?.querySelector<HTMLElement>(`[data-col-edge="${CSS.escape(String(neighbor))}"]`)
        : buttonRef.current;
      el?.focus();
    });
  };

  const onEdgeKey = (e: React.KeyboardEvent<HTMLElement>, id: Id) => {
    const def = byId.get(id)!;
    // The keyboard's way to a column's menu: the context-menu keys on its
    // edge. A held key acts once, not once per repeat (C16): each repeat
    // would re-mount the menu under a new `session` key and flicker it.
    if ((e.shiftKey && e.key === "F10") || e.key === "ContextMenu") {
      e.preventDefault();
      e.stopPropagation();
      if (e.repeat) return;
      // A point, not the edge itself (fix round 1 item 3): the edge can
      // unmount from under an element anchor (hiding its own column is the
      // checklist's main job), which would re-place an open menu at the
      // window's corner and drop focus to `body` on Esc. A point anchor
      // (as a right-click's) has no element to lose. Read in layout px and
      // handed over as a pointer position, as a point anchor takes it.
      const r = layoutRect(e.currentTarget);
      openMenu(layoutToPointer({ x: r.left, y: r.bottom }), id);
      return;
    }
    const step = e.shiftKey ? 40 : 8;
    const now = layout.drawn[id] ?? def.need;
    const to = e.key === "ArrowLeft" ? now - step
      : e.key === "ArrowRight" ? now + step
      : e.key === "Home" ? def.min
      : e.key === "End" ? Infinity
      : null;
    if (to === null) return;
    e.preventDefault();
    e.stopPropagation();
    // The drag's own rule: the next column gives the px.
    const { widths } = layout.resize(id, to);
    for (const c of layout.shown) if (widths[c] !== api.widths[c]) api.setWidth(c, widths[c] ?? null);
  };

  // Built on every render, so an open checklist shows each toggle at once.
  const itemsFor = (column: Id | null): MenuItem[] => {
    const out: MenuItem[] = [];
    if (column !== null) {
      if (byId.get(column)!.movable) {
        out.push(
          { label: t("tableColumns.moveLeft"), icon: <MoveLeftIcon className="size-3.5" />, disabled: !moveBy(api.defs, api.set, column, -1, layout.shown), onSelect: () => move(column, -1) },
          { label: t("tableColumns.moveRight"), icon: <MoveRightIcon className="size-3.5" />, disabled: !moveBy(api.defs, api.set, column, 1, layout.shown), onSelect: () => move(column, 1) },
        );
      }
      if (api.widths[column] !== undefined) {
        out.push({ label: t("tableColumns.resetWidth"), icon: <RefreshIcon className="size-3.5" />, onSelect: () => api.setWidth(column, null) });
      }
      const orders = sort?.items(column) ?? [];
      if (orders.length > 0) out.push(...(out.length > 0 ? [{ type: "separator" } as const] : []), ...orders);
      if (out.length > 0) out.push({ type: "separator" });
    }
    out.push({ type: "label", label: t("tableColumns.columns") });
    for (const c of columnChoices(api.defs, api.set, layout, t, excludedNote)) {
      out.push({
        type: "checkbox", label: c.title, checked: c.checked, disabled: c.disabled, hint: c.note,
        onSelect: () => {
          api.setSet(setShown(api.defs, api.set, c.id, !c.checked));
          // Unchecking the column this very menu is for takes its anchor
          // with it (fix round 1 item 3).
          if (column !== null && column === c.id && c.checked) closeOwnMenu(c.id);
        },
      });
    }
    out.push(
      { type: "separator" },
      {
        label: t("tableColumns.reset"),
        icon: <UndoIcon className="size-3.5" />,
        disabled: api.pristine ?? (isDefaultSet(api.defs, api.set) && Object.keys(api.widths).length === 0),
        onSelect: api.reset,
      },
    );
    return out;
  };

  return (
    <>
      <div
        ref={rowRef}
        role="row"
        aria-rowindex={1}
        data-list-header
        onContextMenu={(e) => {
          // The app's menu, never the webview's (the window guard sees defaultPrevented).
          e.preventDefault();
          const cell = e.target instanceof Element ? e.target.closest<HTMLElement>("[data-col]") : null;
          const column = (cell?.dataset.col as Id | undefined) ?? null;
          // From the keyboard (Shift+F10, the Menu key) there is no pointer:
          // open at a point read from the element's rect (fix round 1 item
          // 3), not the element itself — hiding the column it named would
          // otherwise unmount the anchor mid-fade and jump the menu to the
          // window's corner.
          const fromKeys = e.clientX === 0 && e.clientY === 0 && e.target instanceof HTMLElement;
          if (fromKeys) {
            const r = layoutRect(cell ?? (e.currentTarget as HTMLElement));
            openMenu(layoutToPointer({ x: r.left, y: r.bottom }), column);
          } else {
            openMenu({ x: e.clientX, y: e.clientY }, column);
          }
        }}
        // The standard header (the kit's `list-head`, owner 2026-10-01: one
        // header for every table): on the list's own fill, the material rows
        // show blurred through, with the list's first divider under it; a
        // soft edge fades in only while rows pass under it (`data-scrolled`,
        // above). The pinned
        // cells let it through (app.css). A selecting list's row starts
        // short of the cell padding by its select column's gap (the kit's
        // `list-select-row`): its box sits 12 px in, where a list that
        // selects nothing has its first label (`px-cell`).
        className={cn(
          "list-head sticky top-0 z-(--layer-raised) grid items-center gap-[10px]",
          lead !== undefined ? "list-select-row" : "px-cell",
        )}
        // One height for the list's header and Peek's, so their rules line up
        // (LAYOUT-03); a DataList may pass its own.
        style={{ height, gridTemplateColumns: layout.template }}
      >
        {/* Pinned at the card's left while the list scrolls sideways. Its
            box shows on the list's hover and in selection mode (`list-select-box`). */}
        {lead !== undefined && (
          <div role="columnheader" className="list-pin-check flex items-center self-stretch">
            <span className="list-select-box">{lead}</span>
          </div>
        )}
        {layout.shown.map((id) => {
          const dir = sort?.of(id);
          return (
            <div
              key={id}
              role="columnheader"
              data-col={id}
              aria-sort={dir}
              // Named by its title alone: the edge inside it has its own name.
              aria-labelledby={`${uid}-${id}`}
              onPointerDown={byId.get(id)!.movable ? (e) => drag.start(e, id, titleOf(id)) : undefined}
              className={cn(
                "relative flex h-full min-w-0 items-center gap-1",
                // Name can't move: it stays pinned while the list scrolls sideways.
                byId.get(id)!.movable ? "cursor-grab" : "list-pin-name",
                drag.dragging === id && "opacity-50",
              )}
            >
              <span id={`${uid}-${id}`} data-col-title className="min-w-0 truncate select-none">{titleOf(id)}</span>
              {/* An "other" order (Status's "running first") draws no arrow:
                  neither up nor down describes it (fix round 1 item 8);
                  `aria-sort="other"` on the cell already says so to AT. */}
              {(dir === "ascending" || dir === "descending") && (
                <span aria-hidden="true" className="inline-flex shrink-0">
                  {dir === "ascending" ? <SortAscIcon className="size-3.5" /> : <SortDescIcon className="size-3.5" />}
                </span>
              )}
              {/* The column's edge: drag it, or Tab to it and use the arrows
                  (Shift for bigger steps, Home and End for the ends); a
                  double click (or Reset width) gives the column back to its
                  content's width (owner, 2026-09-30); Shift+F10 opens the
                  column's menu. */}
              <div
                role="separator"
                aria-orientation="vertical"
                aria-label={t("tableColumns.resize", { column: titleOf(id) })}
                aria-valuenow={widthOf(id)}
                aria-valuemin={byId.get(id)!.min}
                aria-valuemax={Math.max(byId.get(id)!.max, widthOf(id))}
                tabIndex={0}
                data-col-edge={id}
                onPointerDown={(e) => startResize(e, id)}
                onDoubleClick={() => api.setWidth(id, null)}
                onKeyDown={(e) => onEdgeKey(e, id)}
                className={cn(
                  "group/edge absolute inset-y-0 -right-[5px] z-[1] flex w-[10px] justify-center focus-ring-inset",
                  fixedEnd(id) ? "pointer-events-none" : "cursor-col-resize",
                )}
              >
                {/* A neutral hairline on hover or focus: the edge, not a stripe. CanvasText in contrast themes. */}
                <span aria-hidden="true" className="my-1.5 w-px bg-stroke-sub-300 opacity-0 group-hover/edge:opacity-100 group-focus-visible/edge:opacity-100 forced-colors:bg-[CanvasText]" />
              </div>
            </div>
          );
        })}
        {/* Pinned at the card's right edge, over the rows' pinned actions. */}
        {/* Named by its own text, not an aria-label on the cell (A11Y-08). */}
        <div role="columnheader" className="list-pin-end flex h-full items-center justify-end">
          <span className="sr-only">{actionsLabel}</span>
          <IconButton
            ref={buttonRef}
            variant="neutral"
            mode="ghost"
            size="3xsmall"
            icon={<ColumnsIcon className="size-3.5" />}
            label={t("tableColumns.columns")}
            aria-haspopup="menu"
            aria-expanded={menuOpen && !!menu?.fromButton}
            onClick={() => {
              // Toggles (fix round 1 item 6): a second press while its own
              // checklist is open closes it instead of re-mounting a new
              // one; focus stays on the button either way.
              if (menuOpen && menu?.fromButton) { setMenuOpen(false); return; }
              openMenu(buttonRef, null, true);
            }}
          />
        </div>
        {drag.node}
      </div>
      {menu && (
        <Menu
          key={menu.session}
          open={menuOpen}
          onOpenChange={setMenuOpen}
          anchor={menu.anchor}
          align={menu.fromButton ? "end" : "start"}
          items={itemsFor(menu.column)}
          heading={menu.column !== null ? titleOf(menu.column) : undefined}
          aria-label={menu.column !== null ? t("tableColumns.columnMenu", { column: titleOf(menu.column) }) : t("tableColumns.columns")}
        />
      )}
      {/* Moves are said out loud. Outside the grid, which holds rows only. */}
      <Portal>
        <div role="status" aria-live="polite" data-column-status className="sr-only">{said}</div>
      </Portal>
    </>
  );
}
