import { fireEvent, render, screen } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { useState } from 'react'
import { describe, expect, it } from 'vitest'
import { TooltipProvider } from '@/components/ui/tooltip'
import { expectNoA11yViolations } from '@/test/utils'
import { MemberOrderList, moveItem, type MemberInfo } from './member-order-list'

const INFO: MemberInfo[] = [
  { name: 'releases', repo_type: 'hosted', online: true },
  { name: 'snapshots', repo_type: 'hosted', online: true },
  { name: 'central', repo_type: 'proxy', online: false },
  { name: 'extra', repo_type: 'hosted', online: true },
]
const KNOWN = new Map(INFO.map((i) => [i.name, i]))

function Harness({ initial = ['releases', 'snapshots', 'central'] }: { initial?: string[] }) {
  const [members, setMembers] = useState(initial)
  return (
    <TooltipProvider>
      <MemberOrderList members={members} onChange={setMembers} candidates={INFO} known={KNOWN} />
      <p data-testid="order">{members.join(',')}</p>
    </TooltipProvider>
  )
}

const order = () => screen.getByTestId('order').textContent
const names = () =>
  screen.getAllByRole('listitem').map((li) => li.querySelector('.font-mono')?.textContent)

describe('moveItem', () => {
  it('moves an item without changing the input', () => {
    const list = ['a', 'b', 'c']
    expect(moveItem(list, 0, 2)).toEqual(['b', 'c', 'a'])
    expect(moveItem(list, 2, 0)).toEqual(['c', 'a', 'b'])
    expect(list).toEqual(['a', 'b', 'c'])
  })
  it('ignores moves that do not make sense', () => {
    expect(moveItem(['a', 'b'], 1, 1)).toEqual(['a', 'b'])
    expect(moveItem(['a', 'b'], -1, 0)).toEqual(['a', 'b'])
    expect(moveItem(['a', 'b'], 0, 5)).toEqual(['a', 'b'])
  })
})

describe('member order list', () => {
  it('shows the members in resolution order with their type and status', async () => {
    const { container } = render(<Harness />)
    expect(names()).toEqual(['releases', 'snapshots', 'central'])
    expect(screen.getByText('Proxy')).toBeInTheDocument()
    expect(screen.getByText('Offline')).toBeInTheDocument()
    expect(screen.getByText('releases > snapshots > central')).toBeInTheDocument()
    await expectNoA11yViolations(container)
  })

  it('moves a member with the up and down buttons and keeps the end buttons disabled', async () => {
    const user = userEvent.setup()
    render(<Harness />)
    expect(screen.getByRole('button', { name: 'Move releases up' })).toBeDisabled()
    expect(screen.getByRole('button', { name: 'Move central down' })).toBeDisabled()
    await user.click(screen.getByRole('button', { name: 'Move central up' }))
    expect(order()).toBe('releases,central,snapshots')
    await user.click(screen.getByRole('button', { name: 'Move releases down' }))
    expect(order()).toBe('central,releases,snapshots')
  })

  it('reorders with the keyboard: pick up, arrows, drop, and announces each step', async () => {
    const user = userEvent.setup()
    render(<Harness />)
    const handle = screen.getByRole('button', { name: 'Reorder releases' })
    handle.focus()
    await user.keyboard(' ')
    expect(handle).toHaveAttribute('aria-pressed', 'true')
    expect(screen.getByRole('status')).toHaveTextContent('releases picked up at position 1 of 3')
    await user.keyboard('{ArrowDown}{ArrowDown}')
    expect(order()).toBe('snapshots,central,releases')
    expect(screen.getByRole('button', { name: 'Reorder releases' })).toHaveFocus()
    expect(screen.getByRole('status')).toHaveTextContent('releases moved to position 3 of 3')
    await user.keyboard('{ArrowDown}')
    expect(order()).toBe('snapshots,central,releases')
    await user.keyboard('{Enter}')
    expect(screen.getByRole('status')).toHaveTextContent('releases dropped at position 3 of 3')
    expect(screen.getByRole('button', { name: 'Reorder releases' })).toHaveAttribute(
      'aria-pressed',
      'false',
    )
    // Arrow keys on a row that is not picked up do nothing.
    await user.keyboard('{ArrowUp}')
    expect(order()).toBe('snapshots,central,releases')
  })

  it('puts the row back when Escape cancels a move', async () => {
    const user = userEvent.setup()
    render(<Harness />)
    screen.getByRole('button', { name: 'Reorder snapshots' }).focus()
    await user.keyboard(' {ArrowUp}')
    expect(order()).toBe('snapshots,releases,central')
    await user.keyboard('{Escape}')
    expect(order()).toBe('releases,snapshots,central')
    expect(screen.getByRole('status')).toHaveTextContent('Move of snapshots cancelled')
  })

  it('reorders by dragging a row onto another', () => {
    render(<Harness />)
    const rows = screen.getAllByRole('listitem')
    fireEvent.dragStart(rows[0]!, { dataTransfer: { setData: () => undefined, effectAllowed: '' } })
    fireEvent.dragOver(rows[2]!)
    fireEvent.drop(rows[2]!)
    expect(order()).toBe('snapshots,central,releases')
    fireEvent.dragEnd(rows[2]!)
  })

  it('adds a member from the list of candidates and removes one', async () => {
    const user = userEvent.setup()
    render(<Harness />)
    const select = screen.getByLabelText('Add a member')
    expect(screen.queryByRole('option', { name: 'releases' })).not.toBeInTheDocument()
    await user.selectOptions(select, 'extra')
    await user.click(screen.getByRole('button', { name: 'Add' }))
    expect(order()).toBe('releases,snapshots,central,extra')
    await user.click(screen.getByRole('button', { name: 'Remove snapshots from the group' }))
    expect(order()).toBe('releases,central,extra')
  })

  it('says so when there is no member and no candidate is left', () => {
    render(
      <TooltipProvider>
        <MemberOrderList
          members={[]}
          onChange={() => undefined}
          candidates={[]}
          known={new Map()}
        />
      </TooltipProvider>,
    )
    expect(screen.getByText(/No members yet/)).toBeInTheDocument()
    expect(screen.getByRole('button', { name: 'Add' })).toBeDisabled()
    expect(
      screen.getByRole('option', { name: 'No other repository of this format' }),
    ).toBeInTheDocument()
  })
})
