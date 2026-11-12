import { render, screen, waitFor } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { describe, expect, it, vi } from 'vitest'
import { expectNoA11yViolations } from '@/test/utils'
import { Tree, type TreeNodeData } from './tree'
import { findTypeAhead, visibleRows } from './tree-model'

const ROOTS: TreeNodeData[] = [
  { id: 'alpha', label: 'alpha', hasChildren: true },
  { id: 'apricot', label: 'apricot', hasChildren: false },
  { id: 'beta', label: 'beta', hasChildren: true },
  { id: 'gamma', label: 'gamma', hasChildren: false },
]

const CHILDREN: Record<string, TreeNodeData[]> = {
  alpha: [
    { id: 'alpha/one', label: 'one', hasChildren: false },
    { id: 'alpha/two', label: 'two', hasChildren: true },
  ],
  beta: [],
  'alpha/two': [{ id: 'alpha/two/deep', label: 'deep', hasChildren: false }],
}

function setup(
  loadChildren = vi.fn((node: TreeNodeData) => Promise.resolve(CHILDREN[node.id] ?? [])),
) {
  const onSelect = vi.fn()
  const view = render(
    <Tree
      label="Repository content"
      roots={ROOTS}
      loadChildren={loadChildren}
      onSelect={onSelect}
    />,
  )
  return { loadChildren, onSelect, ...view }
}

const item = (name: string) => screen.getByRole('treeitem', { name })

describe('Tree', () => {
  it('exposes the ARIA tree pattern with levels, positions and one tab stop', async () => {
    const { container } = setup()
    expect(screen.getByRole('tree', { name: 'Repository content' })).toBeInTheDocument()
    const alpha = item('alpha')
    expect(alpha).toHaveAttribute('aria-level', '1')
    expect(alpha).toHaveAttribute('aria-posinset', '1')
    expect(alpha).toHaveAttribute('aria-setsize', '4')
    expect(alpha).toHaveAttribute('aria-expanded', 'false')
    expect(item('apricot')).not.toHaveAttribute('aria-expanded')
    const stops = screen
      .getAllByRole('treeitem')
      .filter((el) => el.getAttribute('tabindex') === '0')
    expect(stops).toHaveLength(1)
    await expectNoA11yViolations(container)
  })

  it('moves with the arrow keys, Home and End', async () => {
    const user = userEvent.setup()
    setup()
    await user.tab()
    expect(item('alpha')).toHaveFocus()
    await user.keyboard('{ArrowDown}')
    expect(item('apricot')).toHaveFocus()
    await user.keyboard('{End}')
    expect(item('gamma')).toHaveFocus()
    await user.keyboard('{Home}')
    expect(item('alpha')).toHaveFocus()
    await user.keyboard('{ArrowUp}')
    expect(item('alpha')).toHaveFocus()
  })

  it('expands lazily with Right, loads children once, and walks into them', async () => {
    const user = userEvent.setup()
    const { loadChildren } = setup()
    await user.tab()
    await user.keyboard('{ArrowRight}')
    expect(await screen.findByRole('treeitem', { name: 'one' })).toBeInTheDocument()
    expect(item('alpha')).toHaveAttribute('aria-expanded', 'true')
    expect(screen.getByRole('treeitem', { name: 'one' })).toHaveAttribute('aria-level', '2')
    expect(loadChildren).toHaveBeenCalledTimes(1)

    await user.keyboard('{ArrowRight}')
    expect(item('one')).toHaveFocus()
    // Collapse and expand again: the children are kept, not fetched a second time.
    await user.keyboard('{ArrowLeft}')
    expect(item('alpha')).toHaveFocus()
    await user.keyboard('{ArrowLeft}')
    expect(item('alpha')).toHaveAttribute('aria-expanded', 'false')
    await user.keyboard('{ArrowRight}')
    expect(await screen.findByRole('treeitem', { name: 'two' })).toBeInTheDocument()
    expect(loadChildren).toHaveBeenCalledTimes(1)
  })

  it('shows a busy state and announces while a folder loads', async () => {
    const user = userEvent.setup()
    let resolve: (nodes: TreeNodeData[]) => void = () => {}
    const loadChildren = vi.fn(() => new Promise<TreeNodeData[]>((r) => (resolve = r)))
    setup(loadChildren)
    await user.tab()
    await user.keyboard('{ArrowRight}')
    expect(item('alpha')).toHaveAttribute('aria-busy', 'true')
    expect(screen.getByRole('status')).toHaveTextContent('Loading alpha')
    resolve(CHILDREN.alpha ?? [])
    await waitFor(() => expect(item('alpha')).not.toHaveAttribute('aria-busy'))
  })

  it('jumps by type-ahead and cycles through the same first letter', async () => {
    const user = userEvent.setup()
    setup()
    await user.tab()
    await user.keyboard('g')
    expect(item('gamma')).toHaveFocus()
    await user.keyboard('a')
    // "ga" still matches gamma; a pause resets the buffer, and "a" alone cycles alpha, apricot.
    expect(item('gamma')).toHaveFocus()
    await new Promise((r) => setTimeout(r, 650))
    await user.keyboard('a')
    expect(item('alpha')).toHaveFocus()
    await new Promise((r) => setTimeout(r, 650))
    await user.keyboard('a')
    expect(item('apricot')).toHaveFocus()
  })

  it('selects on Enter and on click, and folders also toggle', async () => {
    const user = userEvent.setup()
    const { onSelect } = setup()
    await user.tab()
    await user.keyboard('{Enter}')
    expect(onSelect).toHaveBeenCalledWith(expect.objectContaining({ id: 'alpha' }))
    expect(await screen.findByRole('treeitem', { name: 'one' })).toBeInTheDocument()
    await user.click(item('apricot'))
    expect(onSelect).toHaveBeenLastCalledWith(expect.objectContaining({ id: 'apricot' }))
  })

  it('shows a failure on the row and retries by expanding again', async () => {
    const user = userEvent.setup()
    const loadChildren = vi
      .fn<(node: TreeNodeData) => Promise<TreeNodeData[]>>()
      .mockRejectedValueOnce(new Error('network'))
      .mockResolvedValueOnce(CHILDREN.alpha ?? [])
    setup(loadChildren)
    await user.tab()
    await user.keyboard('{ArrowRight}')
    // Shown on the row and announced through the live region.
    expect((await screen.findAllByText('Could not load alpha')).length).toBe(2)
    // Collapse and expand again to retry.
    await user.keyboard('{ArrowLeft}')
    await user.keyboard('{ArrowRight}')
    expect(await screen.findByRole('treeitem', { name: 'one' })).toBeInTheDocument()
    expect(loadChildren).toHaveBeenCalledTimes(2)
  })

  it('marks an empty folder after loading', async () => {
    const user = userEvent.setup()
    setup()
    await user.click(item('beta'))
    expect(await screen.findByText('Empty')).toBeInTheDocument()
  })

  it('must_not_contain_an_interactive_element_inside_a_row', async () => {
    const user = userEvent.setup()
    setup()
    await user.click(item('alpha'))
    await screen.findByRole('treeitem', { name: 'one' })
    for (const row of screen.getAllByRole('treeitem')) {
      expect(row.querySelector('button, a, input, select, textarea')).toBeNull()
    }
  })
})

describe('tree model', () => {
  it('flattens only expanded and loaded folders, with levels and positions', () => {
    const rows = visibleRows(
      ROOTS,
      new Set(['alpha', 'alpha/two']),
      new Map([
        ['alpha', { status: 'loaded' as const, items: CHILDREN.alpha ?? [] }],
        ['alpha/two', { status: 'loaded' as const, items: CHILDREN['alpha/two'] ?? [] }],
      ]),
    )
    expect(rows.map((r) => `${r.level}:${r.node.label}`)).toEqual([
      '1:alpha',
      '2:one',
      '2:two',
      '3:deep',
      '1:apricot',
      '1:beta',
      '1:gamma',
    ])
    expect(rows[2]).toMatchObject({ posInSet: 2, setSize: 2, parentId: 'alpha' })
  })

  it('finds type-ahead matches, wrapping around', () => {
    const rows = visibleRows(ROOTS, new Set(), new Map())
    expect(findTypeAhead(rows, 0, 'b')).toBe(2)
    expect(findTypeAhead(rows, 3, 'a')).toBe(0)
    expect(findTypeAhead(rows, 0, 'zzz')).toBe(-1)
    expect(findTypeAhead(rows, 0, '')).toBe(-1)
  })
})
