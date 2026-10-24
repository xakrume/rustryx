import { render, screen, waitFor } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { useState } from 'react'
import { describe, expect, it, vi } from 'vitest'
import { expectNoA11yViolations } from '@/test/utils'
import { Tree } from './tree'
import type { TreeNodeData } from './tree-model'

const folder = (id: string): TreeNodeData => ({
  id,
  label: id.replaceAll('/', '') || 'root',
  hasChildren: true,
})
const file = (id: string): TreeNodeData => ({
  id,
  label: id.split('/').pop() ?? id,
  hasChildren: false,
})
const more = (id: string): TreeNodeData => ({
  id,
  label: 'Load more',
  hasChildren: false,
  more: true,
})

describe('tree: load more and reveal', () => {
  it('replaces the "load more" row with the next page, which may end in another one', async () => {
    const user = userEvent.setup()
    const loadMore = vi
      .fn<(node: TreeNodeData) => Promise<TreeNodeData[]>>()
      .mockResolvedValueOnce([file('/a/3'), more('/a/more2')])
      .mockResolvedValueOnce([file('/a/4')])
    render(
      <Tree
        label="Files"
        roots={[folder('/a/')]}
        loadChildren={() => Promise.resolve([file('/a/1'), file('/a/2'), more('/a/more1')])}
        loadMore={loadMore}
      />,
    )
    await user.click(screen.getByRole('treeitem', { name: /a$/ }))
    await screen.findByRole('treeitem', { name: '2' })
    await user.click(screen.getByRole('treeitem', { name: 'Load more' }))
    await screen.findByRole('treeitem', { name: '3' })
    expect(loadMore.mock.calls[0]?.[0].id).toBe('/a/more1')
    await user.click(screen.getByRole('treeitem', { name: 'Load more' }))
    await screen.findByRole('treeitem', { name: '4' })
    expect(screen.queryByRole('treeitem', { name: 'Load more' })).not.toBeInTheDocument()
    expect(screen.getAllByRole('treeitem').map((i) => i.getAttribute('aria-posinset'))).toEqual([
      '1',
      '1',
      '2',
      '3',
      '4',
    ])
  })

  it('does not report the "load more" row as a selection, and works with Enter', async () => {
    const user = userEvent.setup()
    const onSelect = vi.fn()
    render(
      <Tree
        label="Files"
        roots={[folder('/a/')]}
        onSelect={onSelect}
        loadChildren={() => Promise.resolve([file('/a/1'), more('/a/more1')])}
        loadMore={() => Promise.resolve([file('/a/2')])}
      />,
    )
    await user.click(screen.getByRole('treeitem', { name: /a$/ }))
    const row = await screen.findByRole('treeitem', { name: 'Load more' })
    onSelect.mockClear()
    row.focus()
    await user.keyboard('{Enter}')
    await screen.findByRole('treeitem', { name: '2' })
    expect(onSelect).not.toHaveBeenCalled()
    expect(screen.queryByRole('treeitem', { selected: true })).not.toBeInTheDocument()
  })

  it('shows an error on the row when the next page fails and tries again on activation', async () => {
    const user = userEvent.setup()
    const loadMore = vi
      .fn<(node: TreeNodeData) => Promise<TreeNodeData[]>>()
      .mockRejectedValueOnce(new Error('down'))
      .mockResolvedValueOnce([file('/a/2')])
    render(
      <Tree
        label="Files"
        roots={[folder('/a/')]}
        loadChildren={() => Promise.resolve([file('/a/1'), more('/a/more1')])}
        loadMore={loadMore}
      />,
    )
    await user.click(screen.getByRole('treeitem', { name: /a$/ }))
    await user.click(await screen.findByRole('treeitem', { name: 'Load more' }))
    expect(await screen.findByText('Could not load more')).toBeInTheDocument()
    await user.click(screen.getByRole('treeitem', { name: /Load more/ }))
    await screen.findByRole('treeitem', { name: '2' })
  })

  it('opens the folders on the way to a selection, one level at a time, once', async () => {
    const loadChildren = vi.fn((node: TreeNodeData): Promise<TreeNodeData[]> => {
      if (node.id === '/') return Promise.resolve([folder('/x/')])
      if (node.id === '/x/') return Promise.resolve([folder('/x/y/'), file('/x/z')])
      return Promise.resolve([file('/x/y/leaf')])
    })
    function Harness() {
      const [reveal] = useState(['/', '/x/', '/x/y/'])
      return (
        <Tree
          label="Files"
          roots={[folder('/')]}
          loadChildren={loadChildren}
          revealIds={reveal}
          selectedId="/x/y/leaf"
        />
      )
    }
    const { container } = render(<Harness />)
    const leaf = await screen.findByRole('treeitem', { name: 'leaf' })
    expect(leaf).toHaveAttribute('aria-selected', 'true')
    expect(loadChildren.mock.calls.map((c) => c[0].id)).toEqual(['/', '/x/', '/x/y/'])
    await expectNoA11yViolations(container)
    // The user collapses one: the reveal does not open it again.
    await userEvent.setup().click(screen.getByRole('treeitem', { name: 'x' }))
    await waitFor(() =>
      expect(screen.queryByRole('treeitem', { name: 'leaf' })).not.toBeInTheDocument(),
    )
  })

  it('restarts a load that an unmount cancelled (React StrictMode remounts effects)', async () => {
    const loadChildren = vi.fn(() => Promise.resolve([file('/a')]))
    const { StrictMode } = await import('react')
    render(
      <StrictMode>
        <Tree label="Files" roots={[folder('/')]} loadChildren={loadChildren} revealIds={['/']} />
      </StrictMode>,
    )
    expect(await screen.findByRole('treeitem', { name: 'a' })).toBeInTheDocument()
  })
})
