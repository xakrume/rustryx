import { useQuery, useQueryClient } from '@tanstack/react-query'
import { screen, waitFor } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { beforeEach, describe, expect, it, vi } from 'vitest'
import { ApiError } from '@/api/errors'
import type { Versioned } from '@/api/request'
import { ConflictDialog } from '@/components/conflict-dialog'
import { renderWithProviders } from '@/test/utils'
import { useUpdateResource } from './use-update-resource'

interface Role {
  description: string
}

const KEY = ['role', 'r1']
const stale = () =>
  new ApiError({ code: 'precondition_failed', message: 'Changed', status: 412, requestId: 'req-1' })
const inUse = () =>
  new ApiError({
    code: 'resource_in_use',
    message: 'Still assigned',
    status: 409,
    details: { dependents: [{ name: 'npm-proxy', kind: 'repository' }] },
  })

/** What the "server" currently holds; the query reads it, so a refetch after an update is truthful. */
let server: Versioned<Role> = { data: { description: 'old' }, etag: '"v1"' }

function Harness(props: {
  update: (vars: string, etag: string | null) => Promise<Versioned<Role>>
  fetchCurrent: () => Promise<Versioned<Role>>
  copyChanges?: (vars: string) => string
}) {
  const client = useQueryClient()
  const read = useQuery({
    queryKey: KEY,
    queryFn: () => Promise.resolve(server),
  })
  const resource = useUpdateResource<Role, string>({
    queryKey: KEY,
    resourceName: 'role r1',
    update: props.update,
    fetchCurrent: props.fetchCurrent,
    copyChanges: props.copyChanges,
  })
  return (
    <>
      <p>description: {read.data?.data.description ?? '-'}</p>
      <p>etag: {read.data?.etag ?? '-'}</p>
      <button onClick={() => resource.mutate('mine')}>Save</button>
      <button onClick={() => void client.invalidateQueries({ queryKey: KEY })}>Refetch</button>
      <ConflictDialog {...resource.conflict} />
    </>
  )
}

describe('useUpdateResource', () => {
  beforeEach(() => {
    server = { data: { description: 'old' }, etag: '"v1"' }
  })

  it('sends the ETag that was read in If-Match and stores the new version', async () => {
    const update = vi.fn((_vars: string, _etag: string | null) => {
      server = { data: { description: 'new' }, etag: '"v2"' }
      return Promise.resolve(server)
    })
    const user = userEvent.setup()
    renderWithProviders(<Harness update={update} fetchCurrent={vi.fn()} />)
    await screen.findByText('etag: "v1"')

    await user.click(screen.getByRole('button', { name: 'Save' }))
    expect(update).toHaveBeenCalledWith('mine', '"v1"')
    await screen.findByText('description: new')
    expect(screen.getByText('etag: "v2"')).toBeInTheDocument()
  })

  it('opens the three way dialog on 412 and overwrites against the fresh ETag', async () => {
    const update = vi
      .fn<(vars: string, etag: string | null) => Promise<Versioned<Role>>>()
      .mockRejectedValueOnce(stale())
      .mockResolvedValueOnce({ data: { description: 'mine' }, etag: '"v4"' })
    const fetchCurrent = vi.fn(() =>
      Promise.resolve<Versioned<Role>>({ data: { description: 'theirs' }, etag: '"v3"' }),
    )
    const user = userEvent.setup()
    renderWithProviders(<Harness update={update} fetchCurrent={fetchCurrent} />)
    await screen.findByText('etag: "v1"')

    await user.click(screen.getByRole('button', { name: 'Save' }))
    const dialog = await screen.findByRole('alertdialog', { name: /changed by someone else/i })
    expect(dialog).toHaveTextContent('role r1')

    await user.click(screen.getByRole('button', { name: /overwrite with my changes/i }))
    await waitFor(() => expect(update).toHaveBeenCalledTimes(2))
    // The retry carries the version fetched after the conflict, not the stale one.
    expect(update.mock.calls[1]).toEqual(['mine', '"v3"'])
    await waitFor(() => expect(screen.queryByRole('alertdialog')).not.toBeInTheDocument())
  })

  it('reloads on request and offers a copy of the edit only when it can serialize one', async () => {
    const update = vi.fn().mockRejectedValue(stale())
    const user = userEvent.setup()
    const { unmount } = renderWithProviders(<Harness update={update} fetchCurrent={vi.fn()} />)
    await screen.findByText('etag: "v1"')
    await user.click(screen.getByRole('button', { name: 'Save' }))
    await screen.findByRole('alertdialog')
    expect(screen.queryByRole('button', { name: /copy my changes/i })).not.toBeInTheDocument()
    await user.click(screen.getByRole('button', { name: /reload the latest version/i }))
    await waitFor(() => expect(screen.queryByRole('alertdialog')).not.toBeInTheDocument())
    unmount()

    renderWithProviders(
      <Harness update={update} fetchCurrent={vi.fn()} copyChanges={(v) => JSON.stringify({ v })} />,
    )
    await screen.findByText('etag: "v1"')
    await user.click(screen.getByRole('button', { name: 'Save' }))
    expect(await screen.findByRole('button', { name: /copy my changes/i })).toBeInTheDocument()
  })

  it('explains a 409 with the blocking objects and offers no overwrite', async () => {
    const update = vi.fn().mockRejectedValue(inUse())
    const user = userEvent.setup()
    renderWithProviders(<Harness update={update} fetchCurrent={vi.fn()} />)
    await screen.findByText('etag: "v1"')
    await user.click(screen.getByRole('button', { name: 'Save' }))
    const dialog = await screen.findByRole('alertdialog', {
      name: /conflicts with the current state/i,
    })
    expect(dialog).toHaveTextContent('npm-proxy')
    expect(screen.queryByRole('button', { name: /overwrite/i })).not.toBeInTheDocument()
  })
})
