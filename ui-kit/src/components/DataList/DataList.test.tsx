import { act, createEvent, fireEvent, render, screen, waitFor, within } from '@testing-library/react'
import { useRef, useState } from 'react'
import { describe, expect, it, vi } from 'vitest'
import DataList, { type DataListColumn, type DataListLayout } from '.'

type Row = { id: string; name: string }
const ROWS: Row[] = [{ id: 'a', name: 'Alpha' }, { id: 'b', name: 'Beta' }, { id: 'c', name: 'Gamma' }]
const COLUMNS: DataListColumn<Row>[] = [{ id: 'name', header: 'Name', width: 'minmax(0,1fr)', cell: (r) => r.name }]
const LABELS = { selectAll: 'Select all', select: (n: string) => `Select ${n}`, more: 'More actions', actions: 'Actions' }

function Harness({ onOpen = () => {}, onDelete = () => {}, items = ROWS, status = 'ready' as const, openOnClick = false }: {
  onOpen?: (r: Row) => void; onDelete?: (ids: string[]) => void; items?: Row[]; status?: 'loading' | 'error' | 'ready'; openOnClick?: boolean
}) {
  const [selected, setSelected] = useState<Set<string>>(new Set())
  return (
    <div style={{ height: 400, display: 'flex' }}>
      <DataList<Row>
        aria-label="Things"
        items={items}
        getId={(r) => r.id}
        getName={(r) => r.name}
        labels={LABELS}
        status={status}
        loading={<p>Loading</p>}
        empty={<p>Nothing here</p>}
        columns={COLUMNS}
        selected={selected}
        onSelectedChange={setSelected}
        onOpen={onOpen}
        openOnClick={openOnClick}
        onDelete={onDelete}
        menuItems={(r) => [{ label: `Delete ${r.name}`, onSelect: () => onDelete([r.id]), danger: true }]}
      />
      <output data-testid="sel">{[...selected].join(',')}</output>
    </div>
  )
}

const rowOf = (name: string) => screen.getByRole('row', { name: new RegExp(name) })

describe('DataList (table)', () => {
  it('is a treegrid of level-1 rows under a header row', () => {
    render(<Harness />)
    const grid = screen.getByRole('treegrid', { name: 'Things' })
    expect(within(grid).getAllByRole('columnheader').map((h) => h.textContent)).toContain('Name')
    expect(rowOf('Alpha')).toHaveAttribute('aria-level', '1')
  })

  it('moves one Tab stop with the arrows, Home and End', () => {
    render(<Harness />)
    const first = rowOf('Alpha')
    expect(first).toHaveAttribute('tabindex', '0')
    act(() => first.focus())
    fireEvent.keyDown(first, { key: 'ArrowDown' })
    expect(rowOf('Beta')).toHaveAttribute('data-active', 'true')
    fireEvent.keyDown(rowOf('Beta'), { key: 'End' })
    expect(rowOf('Gamma')).toHaveAttribute('data-active', 'true')
    expect(rowOf('Gamma')).toHaveAttribute('tabindex', '0')
    expect(rowOf('Alpha')).toHaveAttribute('tabindex', '-1')
  })

  // Review I1: a stale active id never leaves the list without a Tab stop.
  it('an active id no longer listed leaves exactly one Tab stop, on a drawn row', () => {
    render(
      <DataList<Row> aria-label="Things" items={ROWS} getId={(r) => r.id} getName={(r) => r.name} labels={LABELS} status="ready" columns={COLUMNS} activeId="gone" onActiveChange={() => {}} />,
    )
    const stops = screen.getAllByRole('row').filter((r) => r.getAttribute('tabindex') === '0')
    expect(stops).toEqual([rowOf('Alpha')])
  })

  it('selects with X, extends with Shift+Arrow, all with Mod+A, and Esc clears', () => {
    render(<Harness />)
    fireEvent.keyDown(rowOf('Alpha'), { key: 'x' })
    expect(screen.getByTestId('sel').textContent).toBe('a')
    fireEvent.keyDown(rowOf('Alpha'), { key: 'ArrowDown', shiftKey: true })
    expect(screen.getByTestId('sel').textContent).toBe('a,b')
    fireEvent.keyDown(rowOf('Beta'), { key: 'a', ctrlKey: true })
    expect(screen.getByTestId('sel').textContent).toBe('a,b,c')
    fireEvent.keyDown(rowOf('Beta'), { key: 'Escape' })
    expect(screen.getByTestId('sel').textContent).toBe('')
  })

  // Review M1, M2: a Shift run shrinks on a step back; Shift+click on a box extends it.
  it('a Shift run shrinks when it steps back, and a Shift+click on a box extends it', () => {
    render(<Harness />)
    fireEvent.keyDown(rowOf('Alpha'), { key: 'x' })
    fireEvent.keyDown(rowOf('Alpha'), { key: 'ArrowDown', shiftKey: true })
    fireEvent.keyDown(rowOf('Beta'), { key: 'ArrowDown', shiftKey: true })
    expect(screen.getByTestId('sel').textContent).toBe('a,b,c')
    fireEvent.keyDown(rowOf('Gamma'), { key: 'ArrowUp', shiftKey: true })
    expect(screen.getByTestId('sel').textContent).toBe('a,b')
    fireEvent.keyDown(rowOf('Beta'), { key: 'Escape' })
    fireEvent.click(within(rowOf('Alpha')).getByRole('checkbox'))
    fireEvent.click(within(rowOf('Gamma')).getByRole('checkbox'), { shiftKey: true })
    expect(screen.getByTestId('sel').textContent).toBe('a,b,c')
  })

  // Review I3: a pick the list does not show is never dropped by select-all
  // and never deleted.
  it('select-all and its box act on the shown rows only; Delete never takes a hidden pick', () => {
    const onDelete = vi.fn()
    function Hidden() {
      const [selected, setSelected] = useState<Set<string>>(new Set(['hidden']))
      return (
        <>
          <DataList<Row>
            aria-label="Things" items={ROWS} getId={(r) => r.id} getName={(r) => r.name} labels={LABELS} status="ready" columns={COLUMNS}
            selected={selected} onSelectedChange={setSelected} onDelete={onDelete}
          />
          <output data-testid="sel">{[...selected].join(',')}</output>
        </>
      )
    }
    render(<Hidden />)
    act(() => rowOf('Beta').focus())
    fireEvent.keyDown(rowOf('Beta'), { key: 'Delete' })
    expect(onDelete).toHaveBeenLastCalledWith(['b'])
    fireEvent.keyDown(rowOf('Beta'), { key: 'a', ctrlKey: true })
    expect(screen.getByTestId('sel').textContent).toBe('hidden,a,b,c')
    fireEvent.keyDown(rowOf('Beta'), { key: 'Delete' })
    expect(onDelete).toHaveBeenLastCalledWith(['a', 'b', 'c'])
    fireEvent.click(screen.getByRole('checkbox', { name: 'Select all' }))
    expect(screen.getByTestId('sel').textContent).toBe('hidden')
  })

  it('opens with Enter and deletes the selection with Delete', () => {
    const onOpen = vi.fn()
    const onDelete = vi.fn()
    render(<Harness onOpen={onOpen} onDelete={onDelete} />)
    // Keys act on the active row: focusing a row makes it the active one.
    act(() => rowOf('Beta').focus())
    fireEvent.keyDown(rowOf('Beta'), { key: 'Enter' })
    expect(onOpen).toHaveBeenCalledWith(ROWS[1])
    fireEvent.keyDown(rowOf('Beta'), { key: 'x' })
    fireEvent.keyDown(rowOf('Beta'), { key: 'Delete' })
    expect(onDelete).toHaveBeenCalledWith(['b'])
  })

  // Review I2: a delete never drops focus to the page.
  it('after a delete, focus and the active row go to the next row, then the one before, then the list', async () => {
    function Removing() {
      const [items, setItems] = useState(ROWS)
      return (
        <DataList<Row>
          aria-label="Things" items={items} getId={(r) => r.id} getName={(r) => r.name} labels={LABELS} status="ready" columns={COLUMNS}
          onDelete={(gone) => setItems((xs) => xs.filter((x) => !gone.includes(x.id)))}
        />
      )
    }
    render(<Removing />)
    act(() => rowOf('Beta').focus())
    fireEvent.keyDown(rowOf('Beta'), { key: 'Delete' })
    await waitFor(() => expect(rowOf('Gamma')).toHaveFocus())
    expect(rowOf('Gamma')).toHaveAttribute('data-active', 'true')
    fireEvent.keyDown(rowOf('Gamma'), { key: 'Delete' })
    await waitFor(() => expect(rowOf('Alpha')).toHaveFocus())
    fireEvent.keyDown(rowOf('Alpha'), { key: 'Delete' })
    await waitFor(() => expect(document.activeElement).not.toBe(document.body))
    expect(screen.queryByRole('row')).toBeNull()
  })

  // Review N1: the row menu's Delete, which leaves focus in the menu, ends on the next row too.
  it('after a delete from the row menu, focus and the active row go to the next row', async () => {
    function MenuRemoving() {
      const [items, setItems] = useState(ROWS)
      return (
        <DataList<Row>
          aria-label="Things" items={items} getId={(r) => r.id} getName={(r) => r.name} labels={LABELS} status="ready" columns={COLUMNS}
          menuItems={(r) => [{ label: `Delete ${r.name}`, danger: true, onSelect: () => setItems((xs) => xs.filter((x) => x.id !== r.id)) }]}
        />
      )
    }
    render(<MenuRemoving />)
    act(() => rowOf('Beta').focus())
    fireEvent.contextMenu(rowOf('Beta'))
    fireEvent.click(await screen.findByRole('menuitem', { name: 'Delete Beta' }))
    await waitFor(() => expect(rowOf('Gamma')).toHaveFocus())
    expect(rowOf('Gamma')).toHaveAttribute('data-active', 'true')
  })

  // Review N2: a row a filter hides never moves focus, even with focus on the page.
  it('a row hidden by a filter moves no focus; only the Tab stop falls back', async () => {
    const { rerender } = render(
      <DataList<Row> aria-label="Things" items={ROWS} getId={(r) => r.id} getName={(r) => r.name} labels={LABELS} status="ready" columns={COLUMNS} />,
    )
    act(() => rowOf('Beta').focus())
    act(() => (document.activeElement as HTMLElement).blur())
    rerender(
      <DataList<Row> aria-label="Things" items={[ROWS[0], ROWS[2]]} getId={(r) => r.id} getName={(r) => r.name} labels={LABELS} status="ready" columns={COLUMNS} />,
    )
    await new Promise((r) => requestAnimationFrame(r))
    expect(document.activeElement).toBe(document.body)
    expect(screen.getAllByRole('row').filter((r) => r.getAttribute('tabindex') === '0')).toEqual([rowOf('Alpha')])
  })

  it('a right-click opens the row menu', async () => {
    render(<Harness />)
    fireEvent.contextMenu(rowOf('Gamma'))
    expect(await screen.findByRole('menuitem', { name: 'Delete Gamma' })).toBeInTheDocument()
  })

  it('an empty list has no header and shows its empty slot', () => {
    render(<Harness items={[]} />)
    expect(screen.queryByRole('treegrid')).toBeNull()
    expect(screen.queryByRole('columnheader')).toBeNull()
    expect(screen.getByText('Nothing here')).toBeInTheDocument()
  })

  it('loading keeps the header; the loading slot sits beside the rows, not in the grid', () => {
    render(<Harness status="loading" />)
    const grid = screen.getByRole('treegrid')
    expect(grid).toHaveAttribute('aria-busy', 'true')
    expect(within(grid).queryByText('Loading')).toBeNull()
    expect(screen.getByText('Loading')).toBeInTheDocument()
  })

  // Owner, 2026-09-30: a click anywhere on a row that holds a checkbox toggles it, once.
  it('a click on a checkbox row toggles it once, on its body or on its box', () => {
    const onOpen = vi.fn()
    render(<Harness onOpen={onOpen} />)
    fireEvent.click(within(rowOf('Beta')).getByText('Beta'))
    expect(screen.getByTestId('sel').textContent).toBe('b')
    fireEvent.click(within(rowOf('Beta')).getByRole('checkbox', { name: 'Select Beta' }))
    expect(screen.getByTestId('sel').textContent).toBe('')
    expect(onOpen).not.toHaveBeenCalled()
  })

  it('a list that opens on a click opens with a plain click; Cmd/Ctrl+click and its box toggle', () => {
    const onOpen = vi.fn()
    render(<Harness onOpen={onOpen} openOnClick />)
    fireEvent.click(within(rowOf('Beta')).getByText('Beta'))
    expect(onOpen).toHaveBeenCalledWith(ROWS[1])
    expect(screen.getByTestId('sel').textContent).toBe('')
    fireEvent.click(within(rowOf('Beta')).getByText('Beta'), { ctrlKey: true, metaKey: true })
    expect(screen.getByTestId('sel').textContent).toBe('b')
    fireEvent.click(within(rowOf('Gamma')).getByRole('checkbox', { name: 'Select Gamma' }))
    expect(screen.getByTestId('sel').textContent).toBe('b,c')
    expect(onOpen).toHaveBeenCalledTimes(1)
  })

  it('unticking Select all leaves the keyboard on it', () => {
    render(<Harness />)
    const all = screen.getByRole('checkbox', { name: 'Select all' })
    act(() => all.focus())
    fireEvent.click(all)
    expect(screen.getByTestId('sel').textContent).toBe('a,b,c')
    fireEvent.click(all)
    expect(screen.getByTestId('sel').textContent).toBe('')
    expect(all).toHaveFocus()
  })

  it('a saved layout draws its own header, its order and its template', () => {
    const header = vi.fn((lead: React.ReactNode) => (
      <div role="row" aria-rowindex={1} data-testid="saved-header"><div role="columnheader">{lead}</div></div>
    ))
    function Saved() {
      const scrollRef = useRef<HTMLDivElement>(null)
      const gridRef = useRef<HTMLDivElement>(null)
      const layout: DataListLayout = {
        template: '28px 90px 120px 40px', order: ['size', 'name'], minWidth: 320, header, headerHeight: 32, scrollRef, gridRef,
        scroll: { className: 'saved-scroll', 'data-pin-first': true }, pin: { lead: 'pin-lead', first: 'pin-first', end: 'pin-end' },
      }
      const [selected, setSelected] = useState<Set<string>>(new Set())
      return (
        <DataList<Row>
          aria-label="Saved"
          items={ROWS}
          getId={(r) => r.id}
          getName={(r) => r.name}
          labels={LABELS}
          status="ready"
          selected={selected}
          onSelectedChange={setSelected}
          columns={[...COLUMNS, { id: 'size', header: 'Size', width: '1fr', cell: (r) => `${r.name.length} letters` }]}
          layout={layout}
        />
      )
    }
    render(<Saved />)
    expect(screen.getByTestId('saved-header')).toBeInTheDocument()
    expect(within(screen.getByTestId('saved-header')).getByRole('checkbox', { name: 'Select all' })).toBeInTheDocument()
    const row = rowOf('Alpha')
    expect(row.style.gridTemplateColumns).toBe('28px 90px 120px 40px')
    expect([...row.querySelectorAll('[data-cell]')].map((c) => c.getAttribute('data-cell'))).toEqual(['checkbox', 'size', 'name', 'actions'])
    expect(row.querySelector('[data-cell="checkbox"]')).toHaveClass('pin-lead')
    expect(row.querySelector('[data-cell="size"]')).toHaveClass('pin-first')
    expect(row.querySelector('[data-cell="actions"]')).toHaveClass('pin-end')
    const scroller = screen.getByRole('treegrid').parentElement!
    expect(scroller).toHaveClass('saved-scroll')
    expect(scroller).toHaveAttribute('data-pin-first', 'true')
    expect(screen.getByRole('treegrid')).toHaveStyle({ minWidth: '320px' })
  })

  it('a row the caller says arrived slides in once; the mark goes when it has played', () => {
    const { rerender } = render(
      <DataList<Row> aria-label="Things" items={ROWS.slice(0, 2)} getId={(r) => r.id} getName={(r) => r.name} labels={LABELS} status="ready" columns={COLUMNS} />,
    )
    const arrived = new Set(['c'])
    rerender(
      <DataList<Row> aria-label="Things" items={ROWS} getId={(r) => r.id} getName={(r) => r.name} labels={LABELS} status="ready" columns={COLUMNS} arrived={arrived} />,
    )
    expect(rowOf('Gamma')).toHaveClass('slide-in')
    expect(rowOf('Alpha')).not.toHaveClass('slide-in')
    // jsdom has no AnimationEvent (React may listen for the prefixed name
    // there): both, each with the animation's name put on by hand.
    for (const type of ['animationend', 'webkitAnimationEnd']) {
      const ended = createEvent(type, rowOf('Gamma'), {}, { EventType: 'Event', defaultInit: { bubbles: true } })
      Object.defineProperty(ended, 'animationName', { value: 'slide-in-tint' })
      fireEvent(rowOf('Gamma'), ended)
    }
    expect(rowOf('Gamma')).not.toHaveClass('slide-in')
    rerender(
      <DataList<Row> aria-label="Things" items={ROWS} getId={(r) => r.id} getName={(r) => r.name} labels={LABELS} status="ready" columns={COLUMNS} arrived={new Set(arrived)} />,
    )
    expect(rowOf('Gamma')).not.toHaveClass('slide-in')
  })
})

describe('DataList (grid)', () => {
  it('moves along the cards with the arrows and marks the active one', () => {
    render(
      <DataList<Row>
        variant="grid"
        aria-label="Cards"
        items={ROWS}
        getId={(r) => r.id}
        getName={(r) => r.name}
        labels={LABELS}
        status="ready"
        renderCard={(r) => <span>{r.name}</span>}
      />,
    )
    const list = screen.getByRole('list', { name: 'Cards' })
    const cards = within(list).getAllByRole('listitem')
    fireEvent.keyDown(cards[0], { key: 'ArrowRight' })
    expect(cards[1]).toHaveAttribute('data-active', 'true')
    // The active card is not the current one: only the card in Peek is.
    expect(cards[1]).not.toHaveAttribute('aria-current')
  })

  it('marks the card shown in Peek aria-current, whichever card is active', () => {
    render(
      <DataList<Row>
        variant="grid"
        aria-label="Cards"
        items={ROWS}
        getId={(r) => r.id}
        getName={(r) => r.name}
        labels={LABELS}
        status="ready"
        currentId="c"
        renderCard={(r) => <span>{r.name}</span>}
      />,
    )
    const cards = within(screen.getByRole('list', { name: 'Cards' })).getAllByRole('listitem')
    fireEvent.keyDown(cards[0], { key: 'ArrowRight' })
    expect(cards[2]).toHaveAttribute('aria-current', 'true')
    expect(cards.filter((c) => c.hasAttribute('aria-current'))).toHaveLength(1)
  })

  it('an arriving card slides in on an inner layer, never on the lifting item', () => {
    const grid = (arrived?: ReadonlySet<string>) => (
      <DataList<Row>
        variant="grid"
        aria-label="Cards"
        items={ROWS}
        getId={(r) => r.id}
        getName={(r) => r.name}
        labels={LABELS}
        status="ready"
        onOpen={() => {}}
        arrived={arrived}
        renderCard={(r) => <span>{r.name}</span>}
      />
    )
    const { rerender } = render(grid())
    rerender(grid(new Set(['c'])))
    const card = within(screen.getByRole('list', { name: 'Cards' })).getAllByRole('listitem')[2]
    expect(card).toHaveClass('tile', 'hover:bg-bg-active')
    expect(card).not.toHaveClass('hover-lift', 'slide-in')
    expect(card.querySelector('.slide-in')).not.toBeNull()
  })

  // Owner, 2026-09-30: a selected card shows one 2 px ring, the focus ring's
  // own, never a fill of its own. The item is a tile that steps on hover,
  // with no lift (borderless, 2026-10-01). Rings are never cut off.
  it('a selected card shows the one ring and no selected fill; the scroller leaves room for it', () => {
    render(
      <DataList<Row>
        variant="grid"
        aria-label="Cards"
        items={ROWS}
        getId={(r) => r.id}
        getName={(r) => r.name}
        labels={LABELS}
        status="ready"
        selected={new Set(['b'])}
        onSelectedChange={() => {}}
        renderCard={(r) => <span>{r.name}</span>}
      />,
    )
    const cards = within(screen.getByRole('list', { name: 'Cards' })).getAllByRole('listitem')
    expect(cards[1]).toHaveClass('selected-ring', 'focus-ring', 'tile')
    expect(cards[1]).not.toHaveClass('hover-lift')
    expect(cards[1].className).not.toMatch(/\bbg-(selected|primary)/)
    expect(cards[0]).not.toHaveClass('selected-ring')
    const room = 'calc(var(--focus-width)*2)'
    expect(screen.getByRole('list', { name: 'Cards' }).closest('.overflow-y-auto')).toHaveClass(`p-[${room}]`, `-m-[${room}]`)
    // Review M4 (round 2): X on a focused card says its new state politely.
    act(() => cards[0].focus())
    fireEvent.keyDown(cards[0], { key: 'x' })
    expect(screen.getByRole('status')).toHaveTextContent(/^Selected$/)
    act(() => cards[1].focus())
    fireEvent.keyDown(cards[1], { key: 'x' })
    expect(screen.getByRole('status')).toHaveTextContent(/^Not selected$/)
    // Review M4: a list item has no selected state, so the word says it.
    expect(within(cards[1]).getByText('Selected')).toHaveClass('sr-only')
    expect(within(cards[0]).queryByText('Selected')).toBeNull()
  })
})
