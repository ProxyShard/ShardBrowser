import { useCallback, useLayoutEffect, useRef, useState, type ReactNode } from 'react'
import { cn } from '@/lib/cn'
import { useVirtualList } from '@/lib/virtual'
import { useInputModality, useLongPress } from '@/lib/list'
import Checkbox from '@/components/Checkbox'
import IconButton from '@/components/IconButton'
import type { MenuItem } from '@/components/Menu'
import { useContextMenu } from '@/components/Menu/useContextMenu'
import { MoreVerticalIcon } from '@/lib/icons'

export type DataListColumn<T> = { id: string; header: ReactNode; width: string; cell: (item: T) => ReactNode; className?: string }
export type DataListLabels = {
  selectAll: string
  select: (name: string) => string
  more: string
  actions: string
  /** Said with a selected card (a list item has no selected state of its own). "Selected" by default. */
  selected?: string
  /** Said when a focused card's pick is taken off. "Not selected" by default. */
  notSelected?: string
  /** Said when a table enters selection mode: "3 selected" by default. */
  selectedCount?: (n: number) => string
  /** Said when a table leaves it. "Selection cleared" by default. */
  selectionCleared?: string
}

/// A table whose columns a person sizes, moves, hides and sorts, and whose
/// choices are saved (owner, 2026-09-30): the app keeps that layout (its
/// `useListLayout`, on the one column model Profiles and Proxies share) and
/// hands the list what to draw. The list stays model-free: it draws the
/// columns in `order` on `template`, puts `header` where its own header row
/// would be, and gives the parts that stay pinned while it scrolls sideways
/// their classes.
export type DataListLayout = {
  /** `grid-template-columns` for the header and every row: the select box's
   *  track (only when the list selects: `var(--list-select)`, its own slot
   *  in every state), the columns in `order`, then the actions track (always:
   *  the header keeps its columns button there). */
  template: string
  /** The columns to draw (their `id`s), in order. */
  order: readonly string[]
  /** The list's least width, px: a narrower card scrolls it sideways. */
  minWidth: number
  /** The header row, in place of the list's own; `lead` is the select-all box. */
  header: (lead: ReactNode) => ReactNode
  /** The header row's height, px (the rows' offsets start under it). */
  headerHeight: number
  /** The scroll box and the box that holds `minWidth`: a live resize writes
   *  its widths on them, and the layout reads the list's width from them. */
  scrollRef: React.RefObject<HTMLDivElement | null>
  gridRef: React.RefObject<HTMLDivElement | null>
  /** More for the scroll box: its class, style and data attributes (the
   *  pinned parts' offsets, whether the first column pins). */
  scroll?: React.HTMLAttributes<HTMLDivElement> & { [data: `data-${string}`]: string | boolean | undefined }
  /** Classes for the cells that stay pinned while the list scrolls sideways. */
  pin?: { lead?: string; first?: string; end?: string }
}

type Common<T> = {
  'aria-label': string
  items: readonly T[]
  getId: (item: T) => string
  getName: (item: T) => string
  labels: DataListLabels
  status: 'loading' | 'error' | 'ready'
  loading?: ReactNode
  error?: ReactNode
  empty?: ReactNode
  selected?: ReadonlySet<string>
  onSelectedChange?: (next: Set<string>) => void
  activeId?: string | null
  onActiveChange?: (id: string | null) => void
  onOpen?: (item: T) => void
  /** A selecting table whose rows still open on a plain click, as Profiles':
   *  the box, Cmd/Ctrl+click and Shift+click select. */
  openOnClick?: boolean
  onDelete?: (ids: string[]) => void
  rowActions?: (item: T) => ReactNode
  menuItems?: (item: T) => MenuItem[]
  /** Where a row menu's heading (the item's name) is cut when it is wider
   *  than the menu: `middle` keeps its end visible (an address). */
  menuHeadingTruncate?: 'end' | 'middle'
  bottomInset?: number
  /** Ids that just arrived (a create, a restore, an import), from the
   *  caller's store: each slides in once (Live layer, `slide-in`). */
  arrived?: ReadonlySet<string>
  className?: string
}
type TableProps<T> = {
  variant?: 'table'
  columns: DataListColumn<T>[]
  rowHeight?: number
  headerHeight?: number
  /** The saved, person-made layout (`DataListLayout`); without it the columns keep their `width`s. */
  layout?: DataListLayout
}
type GridProps<T> = {
  variant: 'grid'
  renderCard: (item: T, state: { active: boolean; selected: boolean }) => ReactNode
  minCardWidth?: number
  groupOf?: (item: T) => string
  groupHeader?: (group: string, count: number) => ReactNode
  /** The card shown elsewhere (in Peek): it, not the active card, is
   *  `aria-current` (the active one carries `data-active`). */
  currentId?: string | null
  /** The grid's scroll box, for a caller that keeps its place (scroll memory). */
  scrollRef?: React.RefObject<HTMLDivElement | null>
}
export type DataListProps<T> = Common<T> & (TableProps<T> | GridProps<T>)

/** A row's divider, variant b (borderless, owner 2026-10-01): the kit's
 *  `row-line` (tokens.css), the one divider every list uses. */
const ROW_SEPARATOR = 'row-line'
/** A row's or card's own buttons, links and fields keep their clicks. */
const CONTROLS = 'button, a, input, label, [role="menuitem"]'
const EMPTY: ReadonlySet<string> = new Set()
/** Layers that hold focus for a moment on the list's behalf: its row menu, a confirm. */
const OVERLAYS = '[role="menu"], [role="dialog"], [role="alertdialog"]'
const isMod = (e: React.KeyboardEvent | React.MouseEvent) => (navigator.platform.startsWith('Mac') ? e.metaKey : e.ctrlKey)
// A row's own buttons and fields keep their keys; the list acts on keys
// pressed on a row or a card itself.
const ownsKey = (e: React.KeyboardEvent) => (e.target as HTMLElement).hasAttribute('data-row-id')

/// Rows and cards the caller says just arrived slide in once (Live layer,
/// `slide-in`): the list, not a heuristic, knows a create from a filter
/// cleared. Each id plays once for the list's life, and only once it is in
/// the list. Its mark goes when the animation ends, or when the row is not
/// drawn (out of the window): a one-shot class plays again whenever its
/// element mounts again, so it is never left on.
function useArrivals(arrived: ReadonlySet<string> | undefined, ids: readonly string[]) {
  const played = useRef(new Set<string>())
  const [playing, setPlaying] = useState<ReadonlySet<string>>(EMPTY)
  const listed = ids.join('\n')
  useLayoutEffect(() => {
    if (!arrived || arrived.size === 0) return
    const inList = new Set(listed.split('\n'))
    const fresh = [...arrived].filter((id) => inList.has(id) && !played.current.has(id))
    if (fresh.length === 0) return
    for (const id of fresh) played.current.add(id)
    setPlaying((prev) => new Set([...prev, ...fresh]))
  }, [arrived, listed])
  const settle = useCallback((keep: (id: string) => boolean) => {
    setPlaying((prev) => ([...prev].every(keep) ? prev : new Set([...prev].filter(keep))))
  }, [])
  return { arriving: playing, settle }
}

/// One list for every list page (F5, IFACE-11): the same header, rows,
/// selection, keys, menus and states everywhere. A table (treegrid, ruling 14:
/// rows are what users act on) or a grid of cards. Hand-rolled (ADR 0002).
export default function DataList<T>(props: DataListProps<T>) {
  const {
    items, getId, getName, labels, status, selected, onSelectedChange, onOpen, openOnClick = false, onDelete, rowActions, menuItems,
    menuHeadingTruncate, bottomInset = 0, className,
  } = props
  const menu = useContextMenu()
  // The target row is drawn while the keys drive, or while the pointer is on
  // it (`row-target`, tokens.css): never a hover look left behind.
  useInputModality()
  const table = props.variant !== 'grid' ? (props as TableProps<T>) : null
  const grid = table ? null : (props as GridProps<T>)
  const layout = table?.layout

  // A grid draws its cards group by group: keys, ranges and positions follow
  // that order, whatever order `items` came in.
  const groups = new Map<string, T[]>()
  if (grid) {
    for (const item of items) {
      const g = grid.groupOf?.(item) ?? ''
      const list = groups.get(g)
      if (list) list.push(item)
      else groups.set(g, [item])
    }
  }
  const ordered: readonly T[] = grid ? [...groups.values()].flat() : items
  const ids = ordered.map(getId)

  const [ownActive, setOwnActive] = useState<string | null>(null)
  const activeId = props.activeId !== undefined ? props.activeId : ownActive
  const setActive = (id: string | null) => {
    if (props.onActiveChange) props.onActiveChange(id)
    else setOwnActive(id)
  }
  // A Shift run (review M1): the picks when it started, plus every row from
  // its anchor to where it reaches now, so a step back shrinks it. A toggle,
  // a plain move or a click starts one; with none (focus came by Tab), or an
  // anchor no longer listed, it starts at the active row.
  const run = useRef<{ anchor: string; base: ReadonlySet<string> } | null>(null)
  const [said, setSaid] = useState({ text: '', n: 0 })
  const ownScroll = useRef<HTMLDivElement>(null)
  const scrollRef = layout?.scrollRef ?? grid?.scrollRef ?? ownScroll
  const activeIndex = activeId ? ids.indexOf(activeId) : -1
  const selectable = !!onSelectedChange
  const sel = selected ?? EMPTY
  const hasActions = !!rowActions || !!menuItems
  const ready = status === 'ready'
  const { arriving, settle } = useArrivals(props.arrived, ids)

  // Selection mode (owner, 2026-10-01): a table that selects keeps its select
  // column in every state (owner, 2026-10-07) and shows a row's box on its
  // hover only; with anything selected, `has-selection` on the list shows
  // every box (the kit's tokens.css).
  // Entering and leaving are said in the list's live region; leaving, a
  // focused box that hides hands the keyboard to its row (Select all: to the
  // list's Tab stop).
  const selecting = selectable && sel.size > 0
  const wasSelecting = useRef(selecting)
  useLayoutEffect(() => {
    if (!table || wasSelecting.current === selecting) return
    wasSelecting.current = selecting
    const text = selecting
      ? labels.selectedCount?.(sel.size) ?? `${sel.size} selected`
      : labels.selectionCleared ?? 'Selection cleared'
    setSaid((p) => ({ text, n: p.n + 1 }))
    if (selecting) return
    const box = scrollRef.current
    const at = document.activeElement
    if (!box || !(at instanceof HTMLElement) || !box.contains(at) || !at.closest('.list-select-box')) return
    // Select all, focused, stays shown (`:focus-within`): it keeps the keyboard.
    if (at.closest('[data-list-header]')) return
    ;(at.closest<HTMLElement>('[data-row-id]') ?? box.querySelector<HTMLElement>('[data-row-id][tabindex="0"]'))?.focus({ preventScroll: true })
  })

  const focusRow = (id: string) => requestAnimationFrame(() => {
    const rows = scrollRef.current?.querySelectorAll<HTMLElement>('[data-row-id]') ?? []
    ;[...rows].find((el) => el.dataset.rowId === id)?.focus({ preventScroll: !!table })
  })
  // On touch, with no hover, a long press of a row selects it and so enters
  // selection mode (owner, 2026-10-01). Its click and the platform's menu
  // are its own.
  const longPress = useLongPress((id) => {
    setActive(id)
    if (!sel.has(id)) toggle(id)
  })
  const select = (next: Set<string>) => onSelectedChange?.(next)
  // Select all and its box add or remove the rows the list shows, and only
  // those: picks a search or a filter hides stay picked (review I3, as
  // Profiles' selectProfiles), since only Esc, X or deletion clears them.
  const selectShown = (on: boolean) => {
    const next = new Set(sel)
    for (const id of ids) if (on) next.add(id); else next.delete(id)
    select(next)
  }
  const toggle = (id: string) => {
    const next = new Set(sel)
    if (next.has(id)) next.delete(id)
    else next.add(id)
    run.current = { anchor: id, base: next }
    select(next)
    // A card says its new state out loud when X or a click toggles it while
    // it has focus (review M4): a list item has no selected state to announce.
    const at = document.activeElement
    if (grid && at instanceof HTMLElement && at.dataset.rowId === id) {
      setSaid((p) => ({ text: next.has(id) ? labels.selected ?? 'Selected' : labels.notSelected ?? 'Not selected', n: p.n + 1 }))
    }
  }
  const range = (to: string) => {
    let r = run.current
    if (!r || !ids.includes(r.anchor)) {
      r = { anchor: activeId && ids.includes(activeId) ? activeId : to, base: sel }
      run.current = r
    }
    const from = ids.indexOf(r.anchor)
    const at = ids.indexOf(to)
    const [a, b] = from <= at ? [from, at] : [at, from]
    // The run's base less what was unpicked since (Esc, a bulk action).
    select(new Set([...[...r.base].filter((id) => sel.has(id)), ...ids.slice(a, b + 1)]))
  }

  // ---- table geometry ----
  const rowHeight = table?.rowHeight ?? 36
  const headerHeight = layout?.headerHeight ?? table?.headerHeight ?? 32 // the standard header (`list-head`, `--list-head-height`)
  const { range: win, totalHeight, offsetOf, scrollToIndex } = useVirtualList({
    count: table ? ordered.length : 0, rowHeight, scrollRef, headerHeight, bottomInset,
  })
  // The rows drawn: the window, plus the active row (it keeps the keyboard
  // focus while scrolled away). A grid draws every card.
  const rendered: number[] = []
  if (table) {
    for (let i = win.start; i < win.end; i++) rendered.push(i)
    if (activeIndex >= 0 && !rendered.includes(activeIndex)) { rendered.push(activeIndex); rendered.sort((a, b) => a - b) }
  }
  // The list's one Tab stop is always a drawn row (review I1): the active
  // row (always drawn), else, with none or one no longer listed, the first
  // row in view. Rows start under the sticky header, so row i is whole in
  // view once i * rowHeight >= scrollTop.
  let stop = activeIndex
  if (table && stop < 0 && rendered.length > 0) {
    const inView = Math.ceil((scrollRef.current?.scrollTop ?? 0) / rowHeight)
    stop = rendered.includes(inView) ? inView : rendered[0]
  }
  if (!table && stop < 0) stop = 0
  const drawnKey = table ? rendered.map((i) => ids[i]).join('\n') : ids.join('\n')
  useLayoutEffect(() => {
    if (arriving.size === 0) return
    const drawn = new Set(drawnKey.split('\n'))
    settle((id) => drawn.has(id))
  }, [arriving, drawnKey, settle])

  // A delete that starts from the list itself (the Delete key, or a pick in
  // its row menu) owns the focus that follows (reviews I2, N1, N2). The
  // mark lasts while the person's input stays in the list and its menus and
  // dialogs (a confirm): a press or a key anywhere else ends it, so a filter
  // or chip that hides the active row never moves focus (in WebKit a clicked
  // chip leaves focus on the page); only the Tab stop falls back (I1).
  const intent = useRef<string | null>(null)
  const endIntent = useRef<(() => void) | null>(null)
  const markIntent = (id: string) => {
    endIntent.current?.()
    intent.current = id
    const off = (e: Event) => {
      const t = e.target instanceof Element ? e.target : null
      if (t && (scrollRef.current?.contains(t) || t.closest(OVERLAYS))) return
      end()
    }
    const end = () => {
      intent.current = null
      endIntent.current = null
      document.removeEventListener('pointerdown', off, true)
      document.removeEventListener('keydown', off, true)
    }
    document.addEventListener('pointerdown', off, true)
    document.addEventListener('keydown', off, true)
    endIntent.current = end
  }
  useLayoutEffect(() => () => endIntent.current?.(), [])

  // When that row leaves the list, focus goes to the row that takes its
  // place, else the one before it, else the empty state's first control,
  // else the list itself, and the active row follows (a stale id is
  // cleared). It lands once a menu or a confirm holding focus has closed,
  // and only if focus fell to the page: somewhere else, the person moved on.
  const lastIndex = useRef(-1)
  const wasListed = useRef(false)
  const landing = useRef(0)
  useLayoutEffect(() => {
    const listed = activeIndex >= 0
    const was = wasListed.current
    wasListed.current = listed
    if (listed) { lastIndex.current = activeIndex; return }
    if (!was || !activeId || intent.current !== activeId) return
    endIntent.current?.()
    const next = ids[Math.min(lastIndex.current, ids.length - 1)] ?? null
    setActive(next)
    const ticket = ++landing.current
    const land = () => {
      const box = scrollRef.current
      if (ticket !== landing.current || !box?.isConnected) return
      const at = document.activeElement
      const free = !at || at === document.body || !at.isConnected
      const overlay = free ? null : at.closest(OVERLAYS)
      if (overlay && !overlay.contains(box)) { requestAnimationFrame(land); return }
      if (!free) return
      if (next) {
        if (table) scrollToIndex(ids.indexOf(next))
        ;[...box.querySelectorAll<HTMLElement>('[data-row-id]')].find((el) => el.dataset.rowId === next)?.focus({ preventScroll: !!table })
      } else {
        const control = box.querySelector<HTMLElement>('button:not([disabled]), a[href], input:not([disabled]), [tabindex="0"]')
        ;(control ?? box).focus()
      }
    }
    requestAnimationFrame(land)
  })

  // ---- grid geometry: arrows move by what is above or below on screen ----
  const moveVertical = (from: number, dir: 1 | -1): number => {
    const cards = [...(scrollRef.current?.querySelectorAll<HTMLElement>('[data-row-id]') ?? [])]
    const cur = cards[from]?.getBoundingClientRect()
    if (!cur) return from
    const cx = cur.left + cur.width / 2
    let best = from
    let bestDy = Infinity
    let bestDx = Infinity
    cards.forEach((el, j) => {
      const r = el.getBoundingClientRect()
      const dy = dir > 0 ? r.top - cur.bottom : cur.top - r.bottom
      if (dy < -1) return
      const dx = Math.abs(r.left + r.width / 2 - cx)
      if (dy < bestDy - 1 || (Math.abs(dy - bestDy) <= 1 && dx < bestDx)) { best = j; bestDy = dy; bestDx = dx }
    })
    return best
  }

  const pageRows = () =>
    Math.max(1, Math.floor(((scrollRef.current?.clientHeight ?? 0) - headerHeight - bottomInset) / rowHeight) - 1)
  const pageVertical = (from: number, dir: 1 | -1): number => {
    const box = scrollRef.current
    const cards = box?.querySelectorAll<HTMLElement>('[data-row-id]')
    const top = cards?.[from]?.getBoundingClientRect().top
    if (!box || !cards || top === undefined) return from
    let at = from
    for (;;) {
      const next = moveVertical(at, dir)
      if (next === at) return at
      at = next
      const r = cards[at].getBoundingClientRect()
      if (Math.abs(r.top - top) + r.height >= box.clientHeight) return at
    }
  }

  const move = (to: number, extend: boolean) => {
    const i = Math.max(0, Math.min(ordered.length - 1, to))
    const id = ids[i]
    if (!id) return
    setActive(id)
    if (extend && selectable) range(id)
    else run.current = { anchor: id, base: sel }
    if (table) scrollToIndex(i)
    focusRow(id)
  }

  const onKeyDown = (e: React.KeyboardEvent) => {
    if (!ownsKey(e) || e.altKey || ordered.length === 0) return
    const at = activeIndex >= 0 ? activeIndex : 0
    const item = ordered[at]
    const k = e.key
    const handled = () => { e.preventDefault(); e.stopPropagation() }
    if (k === 'ArrowDown') { handled(); move(grid ? moveVertical(at, 1) : at + 1, e.shiftKey) }
    else if (k === 'ArrowUp') { handled(); move(grid ? moveVertical(at, -1) : at - 1, e.shiftKey) }
    else if (grid && k === 'ArrowRight') { handled(); move(at + 1, e.shiftKey) }
    else if (grid && k === 'ArrowLeft') { handled(); move(at - 1, e.shiftKey) }
    else if (k === 'Home') { handled(); move(0, e.shiftKey) }
    else if (k === 'End') { handled(); move(ordered.length - 1, e.shiftKey) }
    // By what the list shows (review M3): a table by its rows in view, less
    // one kept for context; a grid by a screen of cards straight down or up.
    else if (k === 'PageDown') { handled(); move(grid ? pageVertical(at, 1) : at + pageRows(), e.shiftKey) }
    else if (k === 'PageUp') { handled(); move(grid ? pageVertical(at, -1) : at - pageRows(), e.shiftKey) }
    else if (selectable && k.toLowerCase() === 'x' && !isMod(e) && !e.shiftKey) { handled(); toggle(getId(item)) }
    else if (selectable && k.toLowerCase() === 'a' && isMod(e)) { handled(); selectShown(true) }
    else if ((k === 'Enter' || k === ' ') && onOpen) { handled(); onOpen(item) }
    // A checkbox row with nothing to open: Space toggles it, as its box would.
    else if (k === ' ' && selectable) { handled(); toggle(getId(item)) }
    // Delete acts on the picked rows the list shows, else on the focused
    // one; never on a pick the list hides (review I3).
    else if ((k === 'Delete' || k === 'Backspace') && onDelete) {
      handled()
      const picked = ids.filter((id) => sel.has(id))
      const gone = picked.length > 0 ? picked : [getId(item)]
      if (activeId && gone.includes(activeId)) markIntent(activeId)
      onDelete(gone)
    }
    else if (k === 'Escape' && sel.size > 0) { handled(); select(new Set()) }
  }

  // The row menu's picks start from the list: one that removes the row
  // hands focus on as the Delete key does (review N1).
  const rowMenu = (item: T): MenuItem[] => (menuItems?.(item) ?? []).map((m) => {
    if (!('onSelect' in m)) return m
    return { ...m, onSelect: () => { markIntent(getId(item)); m.onSelect() } }
  })

  const holds = !!table && selectable
  const pointer = (item: T) => ({
    ...(holds ? longPress.bind(getId(item)) : {}),
    onMouseDown: (e: React.MouseEvent) => {
      if (e.button !== 0 || (e.target as HTMLElement).closest(CONTROLS)) return
      setActive(getId(item))
      if (e.shiftKey && selectable) { e.preventDefault(); range(getId(item)) }
    },
    // A click anywhere on a row that holds a checkbox toggles it, once
    // (owner, 2026-09-30); its box and its own controls keep their clicks.
    // A row without one opens, and so does one of a list that opens on a
    // click (`openOnClick`), where Cmd/Ctrl+click toggles.
    onClick: (e: React.MouseEvent) => {
      if (holds && longPress.took(getId(item))) return
      if ((e.target as HTMLElement).closest(CONTROLS) || e.shiftKey) return
      if (selectable && (!openOnClick || !onOpen || isMod(e))) { toggle(getId(item)); return }
      run.current = { anchor: getId(item), base: sel }
      onOpen?.(item)
    },
    onContextMenu: (e: React.MouseEvent) => {
      if (holds && longPress.claimMenu(e, getId(item))) return
      if (!menuItems) return
      setActive(getId(item))
      menu.openAtPointer(e, rowMenu(item), { heading: getName(item), headingTruncate: menuHeadingTruncate })
    },
    onAnimationEnd: (e: React.AnimationEvent) => {
      // The tint is slide-in's longer half: the mark goes once it is over.
      if (e.target === e.currentTarget && e.animationName === 'slide-in-tint') {
        const id = getId(item)
        settle((x) => x !== id)
      }
    },
  })

  // Shown on hover, on focus, while its menu is open, and on the active
  // card. A table row's: on focus in it and on the active row only while the
  // keys drive (`data-input`), so a pointer that leaves takes them away, and
  // a click's focus does not keep them (owner bug, 2026-10-01).
  const actionsOf = (item: T, active: boolean) => hasActions && (
    <span
      className={cn(
        'flex items-center gap-[4px]',
        table
          ? cn(
            'invisible group-hover:visible [:root:not([data-input=pointer])_[role=row]:focus-within_&]:visible group-data-[menu-open]:visible group-has-[[data-menu-open]]:visible',
            active && '[:root:not([data-input=pointer])_&]:visible',
          )
          : !active && 'invisible group-hover:visible group-focus-within:visible group-data-[menu-open]:visible group-has-[[data-menu-open]]:visible',
      )}
    >
      {rowActions?.(item)}
      {menuItems && (
        <IconButton
          size="3xsmall"
          label={labels.more}
          aria-haspopup="menu"
          aria-expanded={false}
          tabIndex={active ? 0 : -1}
          icon={<MoreVerticalIcon className="size-3.5" />}
          onClick={(e) => {
            e.stopPropagation()
            setActive(getId(item))
            menu.openFrom(e.currentTarget, rowMenu(item), { heading: getName(item), headingTruncate: menuHeadingTruncate })
          }}
        />
      )}
    </span>
  )

  const stateBox = (node: ReactNode) => <div className="flex h-full w-full flex-1 items-center justify-center">{node}</div>
  const showList = status === 'loading' || (ready && ordered.length > 0)
  const empty = ready && ordered.length === 0

  if (grid) {
    const acts = selectable || !!onOpen
    let index = -1
    return (
      <div
        ref={scrollRef}
        // Room for a card's ring at the scroller's edges (its width and its
        // offset), given back by a matching negative margin: never cut off
        // (owner). A card has no lift now (borderless, 2026-10-01).
        className={cn(
          '-m-[calc(var(--focus-width)*2)] flex min-h-0 flex-1 flex-col overflow-y-auto p-[calc(var(--focus-width)*2)]',
          empty && 'focus-visible:outline-hidden',
          className,
        )}
        // Where focus lands when the last card goes and the empty state has no control.
        tabIndex={empty ? -1 : undefined}
        onKeyDown={onKeyDown}
      >
        {ready && ordered.length > 0 && [...groups].map(([group, list]) => (
          <section key={group || 'all'} className="flex flex-col gap-[10px] pb-[20px] last:pb-0">
            {grid.groupHeader?.(group, list.length)}
            <div
              role="list"
              aria-label={group ? `${props['aria-label']}: ${group}` : props['aria-label']}
              className="grid gap-[10px]"
              style={{ gridTemplateColumns: `repeat(auto-fill, minmax(${grid.minCardWidth ?? 240}px, 1fr))` }}
            >
              {list.map((item) => {
                index++
                const id = getId(item)
                const isActive = activeId === id
                const tabbable = index === stop
                const isSel = sel.has(id)
                const { onAnimationEnd, ...press } = pointer(item)
                return (
                  <div
                    key={id}
                    role="listitem"
                    data-row-id={id}
                    data-active={isActive || undefined}
                    data-selected={isSel || undefined}
                    aria-current={grid.currentId === id || undefined}
                    tabIndex={tabbable ? 0 : -1}
                    // The item is the tile (the weak fill, no border, no shadow, no
                    // lift) and, when it acts, steps to the active fill on hover and
                    // while pressed; the card inside is only its content. A selection
                    // is one ring, the focus ring's own (owner): never a fill, never two.
                    className={cn(
                      // min-w-0 and a column that may shrink: a card never
                      // grows past its cell, whatever its text (it wraps or is cut).
                      'group focus-ring relative grid min-w-0 grid-cols-[minmax(0,1fr)] tile',
                      acts && 'cursor-pointer transition-hover hover:bg-bg-active active:bg-bg-active forced-colors:hover:outline',
                      isSel && 'selected-ring',
                    )}
                    onFocus={(e) => { if (e.target === e.currentTarget && activeId !== id) setActive(id) }}
                    {...press}
                  >
                    {/* An arriving card plays slide-in on this inner layer,
                        never on the tile itself (its ring and fill stay put). */}
                    <div className={cn('grid min-w-0 rounded-[inherit]', arriving.has(id) && 'slide-in')} onAnimationEnd={onAnimationEnd}>
                      {grid.renderCard(item, { active: isActive, selected: isSel })}
                    </div>
                    {/* A list item has no selected state: the word says it (review M4). */}
                    {isSel && <span className="sr-only">{labels.selected ?? 'Selected'}</span>}
                    {hasActions && <span className="absolute right-[8px] top-[8px]">{actionsOf(item, isActive)}</span>}
                  </div>
                )
              })}
            </div>
          </section>
        ))}
        {/* The first load: the list is there already and says it is busy
            (A11Y-22); the placeholder cards inside it say only "Loading". */}
        {status === 'loading' && <div role="list" aria-label={props['aria-label']} aria-busy="true">{props.loading}</div>}
        {/* Polite, always in the page; a new node per change, so the same word twice is said twice. */}
        <div role="status" className="sr-only"><span key={said.n}>{said.text}</span></div>
        {status === 'error' && stateBox(props.error)}
        {/* An empty grid sits in the same tile a table does (`list-tile`), so
            every empty page is one surface (clarity fixes, item 17). */}
        {ready && ordered.length === 0 && (
          <div data-list-tile className="flex min-h-0 min-w-0 flex-1 flex-col list-tile">{stateBox(props.empty)}</div>
        )}
        {menu.node}
      </div>
    )
  }

  // With a saved layout, its order and template; else the columns as given.
  const byId = new Map(table!.columns.map((c) => [c.id, c]))
  const columns = layout ? layout.order.flatMap((id) => byId.get(id) ?? []) : table!.columns
  const template = layout
    ? layout.template
    : [selectable ? 'var(--list-select)' : null, ...columns.map((c) => c.width), hasActions ? 'auto' : null].filter(Boolean).join(' ')
  const endCell = hasActions || !!layout
  const allSelected = ordered.length > 0 && ids.every((id) => sel.has(id))
  const lead = selectable ? (
    <Checkbox
      aria-label={labels.selectAll}
      checked={allSelected}
      indeterminate={!allSelected && ids.some((id) => sel.has(id))}
      onChange={(e) => selectShown(e.target.checked)}
    />
  ) : undefined
  const { className: scrollClass, style: scrollStyle, ...scrollRest }: NonNullable<DataListLayout['scroll']> = layout?.scroll ?? {}
  const pin = layout?.pin

  return (
    // The tile round the list (`list-tile`, borderless, owner 2026-10-01):
    // no border, its 4 px padding the inner inset round the scroll box, so
    // rows clip inside it. `className` sizes the tile in its host.
    <div data-list-tile className={cn('flex min-h-0 min-w-0 flex-1 flex-col list-tile', className)}>
    <div
      {...scrollRest}
      ref={scrollRef}
      className={cn(
        // `isolate`: the pinned cells' and separators' layers stay inside the list.
        'relative isolate flex min-h-0 min-w-0 flex-1 flex-col overflow-auto',
        empty && 'focus-visible:outline-hidden',
        selectable && 'list-select',
        selecting && 'has-selection',
        scrollClass,
      )}
      // Where focus lands when the last row goes and the empty state has no control.
      tabIndex={empty ? -1 : undefined}
      style={{ scrollPaddingBottom: bottomInset, ...scrollStyle }}
      onKeyDown={onKeyDown}
    >
      {showList && (
        <div
          ref={layout?.gridRef}
          role="treegrid"
          aria-label={props['aria-label']}
          aria-rowcount={ordered.length + 1}
          aria-multiselectable={selectable || undefined}
          aria-busy={status === 'loading' || undefined}
          className="shrink-0"
          style={layout ? { minWidth: layout.minWidth } : undefined}
        >
          {layout ? layout.header(lead) : (
            <div
              role="row"
              aria-rowindex={1}
              // The standard header (`list-head`): on the tile's own fill,
              // with the list's first divider under it (owner, 2026-10-01).
              data-list-header
              className={cn(
                'list-head sticky top-0 z-10 grid items-center gap-[10px]',
                selectable ? 'list-select-row' : 'px-cell',
              )}
              style={{ height: headerHeight, gridTemplateColumns: template }}
            >
              {lead && <div role="columnheader" className="relative self-stretch"><span className="list-select-box">{lead}</span></div>}
              {columns.map((c) => <div key={c.id} role="columnheader" className="min-w-0 truncate">{c.header}</div>)}
              {hasActions && <div role="columnheader"><span className="sr-only">{labels.actions}</span></div>}
            </div>
          )}
          {ready && (
            <div className="relative" style={{ height: totalHeight + bottomInset }}>
              {rendered.map((i) => {
                const item = ordered[i]
                const id = ids[i]
                const active = i === activeIndex
                const isSel = sel.has(id)
                const tabbable = i === stop
                return (
                  <div
                    key={id}
                    role="row"
                    aria-rowindex={i + 2}
                    aria-level={1}
                    aria-selected={selectable ? isSel : undefined}
                    data-row-id={id}
                    data-active={active || undefined}
                    tabIndex={tabbable ? 0 : -1}
                    className={cn(
                      'group focus-ring-inset absolute inset-x-0 grid items-center gap-[10px] text-ui text-text-strong-950',
                      selectable ? 'list-select-row' : 'px-cell',
                      ROW_SEPARATOR,
                      // The target is drawn only while it is the target in sight
                      // (`row-target`). On the tile, a hovered row is one step
                      // darker; the target and a checked row lie over the panel
                      // (`row-fill-*`, tokens.css `list-tile`): one opaque token
                      // the row and its pinned cells both paint.
                      isSel ? 'row-fill-selected' : active ? 'row-target:row-fill-active' : 'hover:row-fill-active',
                      arriving.has(id) && 'slide-in',
                    )}
                    style={{ top: offsetOf(i), height: rowHeight, gridTemplateColumns: template }}
                    onFocus={(e) => { if (e.target === e.currentTarget && !active) setActive(id) }}
                    {...pointer(item)}
                  >
                    {selectable && (
                      <div role="gridcell" data-cell="checkbox" className={cn('relative flex items-center self-stretch', pin?.lead)}>
                        {/* In its own slot before Name: at rest only on
                            hover, every box in selection mode (tokens.css). */}
                        <span className="list-select-box">
                          <Checkbox
                            aria-label={labels.select(getName(item))}
                            tabIndex={active ? 0 : -1}
                            checked={isSel}
                            // Shift+click on the box extends the run, as on the row (review M2).
                            onChange={(e) => {
                              if ((e.nativeEvent as MouseEvent).shiftKey) { setActive(id); range(id) }
                              else toggle(id)
                            }}
                          />
                        </span>
                      </div>
                    )}
                    {columns.map((c, n) => (
                      <div
                        key={c.id}
                        role="gridcell"
                        data-cell={c.id}
                        className={cn('min-w-0', n === 0 && pin?.first && cn('flex items-center self-stretch', pin.first), c.className)}
                      >
                        {n === 0 && pin?.first ? <div className="min-w-0 flex-1">{c.cell(item)}</div> : c.cell(item)}
                      </div>
                    ))}
                    {endCell && (
                      <div role="gridcell" data-cell="actions" className={cn('flex items-center justify-end self-stretch', pin?.end)}>
                        {actionsOf(item, active)}
                      </div>
                    )}
                  </div>
                )
              })}
            </div>
          )}
        </div>
      )}
      {status === 'loading' && <div aria-hidden="true">{props.loading}</div>}
      {/* Polite, always in the page, outside the grid: selection mode's
          words. A new node per change, so the same words twice are said twice. */}
      {selectable && <div role="status" className="sr-only"><span key={said.n}>{said.text}</span></div>}
      {status === 'error' && stateBox(props.error)}
      {ready && ordered.length === 0 && stateBox(props.empty)}
      {menu.node}
    </div>
    </div>
  )
}
